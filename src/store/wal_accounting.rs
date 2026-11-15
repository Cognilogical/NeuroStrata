//! One zero-record accounting model for every WAL recovery arm (`#207`).
//!
//! The recovery funnel in [`crate::store::ladybug::LadybugStore::with_system_config`]
//! has four arms that can end in a `CHECKPOINT`, a rename, or a reopen: the
//! checksum-config mismatch reopen, the lenient (unverified) replay, the
//! unreadable-WAL rename, and the plain verified open. Before `#207` only the
//! lenient replay arm had a zero-record guard, and that guard keyed on the
//! *active* `.wal` alone, so:
//!
//! * a frozen `<db>.wal.checkpoint` with no active `.wal` replayed zero records,
//!   was accepted, and the canonical frozen file was deleted by the engine;
//! * the checksum-mismatch arm checkpointed an empty replay and reported success;
//! * a WAL whose only `COMMIT` record was torn was refused as `DATA LOSS` even
//!   though nothing committed could be lost.
//!
//! This module is the single decision table those arms must share. It is **pure**:
//! it never touches the filesystem. Callers measure bytes and (optionally) scan
//! bytes, then ask [`classify`] what happened.
//!
//! # Where the byte layout comes from
//!
//! The pinned engine is `lbug 0.22.3`
//! (`~/.cargo/registry/src/index.crates.io-*/lbug-0.22.3/lbug-src`). Nothing in
//! the constants below is invented; each is cited to the source this crate is
//! compiled against.
//!
//! Header (`WAL::writeHeader`, `storage/wal/wal.cpp:394-403`): the serializer
//! writes `uuid databaseID` (16 raw bytes — `common::uuid` is an `int128_t`,
//! `storage/file_db_id_utils.cpp:18-20`) followed by `bool enableChecksums`
//! (1 byte, `Serializer::serializeValue<T>` writes `sizeof(T)` bytes). Byte 16
//! is therefore the checksum flag, which is what the existing header reader in
//! `ladybug.rs` relies on.
//!
//! When `enableChecksums` is on, the whole WAL is wrapped in `ChecksumWriter`
//! (`wal.cpp:414-429`): every `onObjectBegin`/`onObjectEnd` pair becomes
//! `payload || u64 checksum` (`storage/wal/checksum_writer.cpp:52-61`). The
//! header is such an object, so a checksummed header occupies 17 + 8 = 25 bytes
//! — the "25-byte header" `ladybug.rs` and the `#191` runbook refer to.
//!
//! Records (`WALRecord::serializeWithLength`, `storage/wal/wal_record.cpp:21-35`
//! and `WAL::addNewWALRecordNoLock`, `wal.cpp:431-439`): a record object is
//!
//! ```text
//! [u64 recordLength][recordLength bytes: u8 type || u64 ownerNameLen || ownerName][u64 checksum?]
//! ```
//!
//! `type` is `WALRecordType` (`include/storage/wal/record/wal_record_base.h:18-40`):
//! `COMMIT_RECORD = 2`, `CHECKPOINT_RECORD = 254`. `writeDebuggingInfo` is
//! compiled out unless `DESER_DEBUG` is set (`common/serializer/serializer.cpp:15-21`),
//! so the on-disk payload is exactly the type byte followed by the owner-name
//! trailer. All integers are written with `reinterpret_cast<const uint8_t*>` and
//! read with the same cast, i.e. **host** endianness.
//!
//! The scan below walks those frames and counts `COMMIT_RECORD` frames. It does
//! **not** verify the checksum: the pinned checksum is DuckDB's block hash
//! (`common/checksum.cpp`), and reimplementing it in Phase A would introduce a
//! second, unverified copy of an engine internal. Instead the scan is
//! conservative — any frame it cannot attribute ends as [`WalTail::Corrupt`] and
//! the evidence becomes [`CommitEvidence::Unknown`], which the classifier
//! refuses rather than silently accepts.
//!
//! # Why "had a commit?" is the right question
//!
//! `WALReplayer::dryReplay` advances `offsetDeserialized` only on `COMMIT_RECORD`
//! and `CHECKPOINT_RECORD` (`storage/wal/wal_replayer.cpp:944-971`). When no such
//! record deserializes, `offsetDeserialized` stays 0, and the engine either
//! deletes the frozen WAL (`wal_replayer.cpp:674-679`) or truncates the active
//! one to 0 (`wal_replayer.cpp:807`). "Zero records replayed" therefore means
//! "no *committed* record was readable", not "the WAL held nothing": a torn
//! `COMMIT`, a delete-only transaction, and a wholesale-corrupted file are all
//! zero-record replays but only one of them lost committed data.
//!
//! Positive evidence (a complete `COMMIT_RECORD` frame) is what separates them.
//! It is conservative in the direction that matters: a missed commit can only
//! turn a lossless case into [`WalAccounting::Unknown`] (refuse), never a lossy
//! case into [`WalAccounting::NothingCommitted`] (accept).

/// Bytes of `WALHeader` written by `WAL::writeHeader` (`wal.cpp:394-403`):
/// 16-byte database id plus the 1-byte `enableChecksums` flag.
pub const WAL_HEADER_BYTES: u64 = 17;

/// A header plus its `u64` checksum (`ChecksumWriter::onObjectEnd`). The issue's
/// `len >= 25` rule: only at this length are both the flag byte and a complete
/// checksummed header guaranteed present, so a 0-byte active `.wal` can no
/// longer hide a full frozen `.wal.checkpoint`.
pub const WAL_HEADER_CHECKSUMMED_BYTES: u64 = 25;

/// `WALRecordType::COMMIT_RECORD` (`wal_record_base.h:22`).
pub const COMMIT_RECORD_TYPE: u8 = 2;

/// `WALRecordType::CHECKPOINT_RECORD` (`wal_record_base.h:39`).
pub const CHECKPOINT_RECORD_TYPE: u8 = 254;

/// Byte offset of the `enableChecksums` flag inside the header.
pub const CHECKSUM_FLAG_OFFSET: usize = 16;

/// The `WALRecordType` values the pinned engine can write (`wal_record_base.h:18-40`).
/// A frame whose type byte is outside this set is not a record this code can
/// attribute, so the scan stops pessimistically. `0` (`INVALID_RECORD`) is
/// deliberately absent: the engine reserves it to detect reads from an empty
/// buffer (`wal_record.cpp:56-62`).
const KNOWN_RECORD_TYPES: [u8; 17] = [
    1, 2, 13, 14, 15, 16, 17, 18, 19, 30, 31, 32, 33, 34, 35, 100, 254,
];

/// One side of the WAL pair, in bytes. `0` covers both "absent" and
/// "present but empty" **on purpose**: the `#207` bug came from an
/// existence-based shortcut, and a recovery arm may never key on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalBytes {
    pub active: u64,
    pub frozen: u64,
}

impl WalBytes {
    pub const ZERO: Self = Self {
        active: 0,
        frozen: 0,
    };

    pub const fn total(self) -> u64 {
        self.active.saturating_add(self.frozen)
    }

    pub const fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// What the pre-replay bytes prove about committed records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitEvidence {
    /// At least one complete `COMMIT_RECORD` frame existed before replay.
    Some,
    /// The WAL was scanned to a sound stopping point and held no commit.
    None,
    /// The bytes could not be attributed. Never treat this as `None`.
    Unknown,
}

/// How a WAL file's frame walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalTail {
    /// The file ended exactly on a frame boundary.
    Clean,
    /// The final frame is a prefix of a well-formed frame (a crash mid-append).
    TornTail,
    /// A frame boundary could not be trusted; the rest of the file is opaque.
    Corrupt,
}

/// Whether a usable header was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalHeader {
    /// No bytes at all: there is nothing to replay.
    Absent,
    /// Fewer than [`WAL_HEADER_CHECKSUMMED_BYTES`]: the flag cannot be trusted.
    Unusable,
    /// Byte 16 was readable.
    Readable { checksums: bool },
}

/// The result of walking one WAL file's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalScan {
    pub header: WalHeader,
    /// Complete frames walked before the stopping point.
    pub frames_parsed: u32,
    /// Complete `COMMIT_RECORD` frames.
    pub commits: u32,
    /// The last complete frame's `WALRecordType`, if any.
    pub last_complete_type: Option<u8>,
    pub tail: WalTail,
}

impl WalScan {
    /// A file with no bytes. Distinct from [`WalHeader::Unusable`]: there is
    /// nothing to replay and nothing was lost.
    pub const EMPTY: Self = Self {
        header: WalHeader::Absent,
        frames_parsed: 0,
        commits: 0,
        last_complete_type: None,
        tail: WalTail::Clean,
    };

    /// Whether the WAL ended with a complete `CHECKPOINT_RECORD` and no later
    /// record. `dryReplay` records exactly this as `isLastRecordCheckpoint`, and
    /// the engine only removes the WAL after applying that checkpoint
    /// (`wal_replayer.cpp:946-964`, `:674-679`). A torn tail *after* the
    /// checkpoint keeps the flag: the engine's switch never runs for the record
    /// that threw.
    pub const fn ended_in_checkpoint(self) -> bool {
        matches!(self.last_complete_type, Some(CHECKPOINT_RECORD_TYPE))
            && !matches!(self.tail, WalTail::Corrupt)
    }

    /// What these bytes prove about committed records. See the module docs for
    /// why `frames_parsed == 0` on a torn/corrupt file is `Unknown`.
    pub const fn commit_evidence(self) -> CommitEvidence {
        match self.header {
            WalHeader::Absent => CommitEvidence::None,
            WalHeader::Unusable => CommitEvidence::Unknown,
            WalHeader::Readable { .. } => {
                if self.commits > 0 {
                    CommitEvidence::Some
                } else {
                    match self.tail {
                        // Walked every frame: nothing committed was written.
                        WalTail::Clean => CommitEvidence::None,
                        // A torn final frame can only hide records *after* the
                        // last complete one. Once at least one complete frame
                        // parsed, nothing committed preceded the tear.
                        WalTail::TornTail if self.frames_parsed > 0 => CommitEvidence::None,
                        // No complete frame parsed at all, or a mid-file frame
                        // could not be trusted: the bytes cannot be attributed.
                        WalTail::TornTail | WalTail::Corrupt => CommitEvidence::Unknown,
                    }
                }
            }
        }
    }

    /// Conservative combination for the pair: a commit anywhere wins, an
    /// unreadable file only downgrades when nothing else proved a commit.
    pub const fn merge(self, other: Self) -> Self {
        if self.commits > 0 && other.commits == 0 {
            return self;
        }
        if other.commits > 0 {
            return other;
        }
        // Neither has a commit; keep the pessimistic tail/header, but preserve
        // the checkpoint signal from whichever file walked furthest.
        if matches!(self.header, WalHeader::Readable { .. })
            && matches!(other.header, WalHeader::Readable { .. })
        {
            if self.ended_in_checkpoint() && !other.ended_in_checkpoint() {
                return self;
            }
            if other.ended_in_checkpoint() && !self.ended_in_checkpoint() {
                return other;
            }
        }
        if matches!(self.commit_evidence(), CommitEvidence::Unknown) {
            return self;
        }
        other
    }
}

/// Reads the header flag from raw WAL bytes. Pure: the caller does the I/O.
pub fn wal_checksum_flag(bytes: &[u8]) -> Option<bool> {
    if (bytes.len() as u64) < WAL_HEADER_CHECKSUMMED_BYTES {
        return None;
    }
    bytes.get(CHECKSUM_FLAG_OFFSET).map(|flag| *flag != 0)
}

/// Walks one WAL file's frames. `bytes` is the whole file.
///
/// The walk never resynchronises: the first frame it cannot attribute ends it.
/// That is the property that keeps a corrupted middle from masquerading as a
/// clean prefix.
pub fn scan_wal(bytes: &[u8]) -> WalScan {
    if bytes.is_empty() {
        return WalScan::EMPTY;
    }
    let checksums = match wal_checksum_flag(bytes) {
        Some(flag) => flag,
        None => {
            return WalScan {
                header: WalHeader::Unusable,
                frames_parsed: 0,
                commits: 0,
                last_complete_type: None,
                tail: WalTail::Corrupt,
            };
        }
    };
    let mut scan = WalScan {
        header: WalHeader::Readable { checksums },
        frames_parsed: 0,
        commits: 0,
        last_complete_type: None,
        tail: WalTail::Clean,
    };
    let checksum_bytes: u64 = if checksums { 8 } else { 0 };
    let mut cursor: usize = if checksums {
        WAL_HEADER_CHECKSUMMED_BYTES as usize
    } else {
        WAL_HEADER_BYTES as usize
    };

    loop {
        let remaining = bytes.len() - cursor;
        if remaining == 0 {
            scan.tail = WalTail::Clean;
            return scan;
        }
        if remaining < 8 {
            scan.tail = WalTail::TornTail;
            return scan;
        }
        let mut length_bytes = [0u8; 8];
        length_bytes.copy_from_slice(&bytes[cursor..cursor + 8]);
        // Host-endian: the engine writes the `u64` through a byte cast
        // (`Serializer::serializeValue`), and every target this crate builds for
        // is little-endian. `from_ne_bytes` states that dependency instead of
        // hiding it behind `from_le_bytes`.
        let record_length = u64::from_ne_bytes(length_bytes);
        let frame_len = match record_length
            .checked_add(8)
            .and_then(|n| n.checked_add(checksum_bytes))
        {
            Some(n) => n,
            None => {
                scan.tail = WalTail::Corrupt;
                return scan;
            }
        };
        // `1` is the minimum a payload can be (`u8 type`).
        if record_length == 0 {
            scan.tail = WalTail::Corrupt;
            return scan;
        }
        if frame_len > remaining as u64 {
            scan.tail = WalTail::TornTail;
            return scan;
        }
        let type_offset = cursor + 8;
        let record_type = bytes[type_offset];
        if !KNOWN_RECORD_TYPES.contains(&record_type) {
            scan.tail = WalTail::Corrupt;
            return scan;
        }
        scan.frames_parsed = scan.frames_parsed.saturating_add(1);
        if record_type == COMMIT_RECORD_TYPE {
            scan.commits = scan.commits.saturating_add(1);
        }
        scan.last_complete_type = Some(record_type);
        cursor += frame_len as usize;
    }
}

/// One WAL side as observed by the header pre-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderProbe {
    pub exists: bool,
    pub bytes: u64,
    /// `wal_checksum_flag`'s answer; `None` when the file is absent, empty, or
    /// shorter than [`WAL_HEADER_CHECKSUMMED_BYTES`].
    pub checksums: Option<bool>,
}

impl HeaderProbe {
    pub const ABSENT: Self = Self {
        exists: false,
        bytes: 0,
        checksums: None,
    };

    /// A probe of a file the caller already read. `bytes` is its length.
    pub const fn present(bytes: u64, checksums: Option<bool>) -> Self {
        Self {
            exists: true,
            bytes,
            checksums,
        }
    }

    /// Usable means both the flag and a complete checksummed header are there.
    pub const fn usable(self) -> bool {
        self.exists && self.bytes >= WAL_HEADER_CHECKSUMMED_BYTES && self.checksums.is_some()
    }
}

/// Which side the checksum configuration should be read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderSource {
    /// Neither WAL exists.
    None,
    /// Exactly one side is usable, or both agree.
    Single { checksums: bool, frozen_only: bool },
    /// Refuse: the pair cannot be served by one `enable_checksums` setting.
    Conflict(HeaderConflict),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderConflict {
    /// At least one WAL exists but no side carries a usable 25-byte header.
    /// Before `#207` this silently defaulted to checksums-on, which mis-parses a
    /// checksums-off WAL.
    NoUsableHeader,
    /// Both sides are readable and disagree. The engine replays the frozen WAL
    /// first, so any single `enable_checksums` value mis-parses one side.
    ChecksumsDisagree,
}

/// The `#207` header rule: pick by a *usable* 25-byte header, never by
/// existence. A 0-byte or truncated active `.wal` no longer hides the frozen
/// `.wal.checkpoint`, and a mixed pair is refused instead of defaulting to
/// checksums-on.
pub const fn select_header_source(active: HeaderProbe, frozen: HeaderProbe) -> HeaderSource {
    let (active_usable, frozen_usable) = (active.usable(), frozen.usable());
    match (active_usable, frozen_usable) {
        (true, true) => {
            let (a, f) = match (active.checksums, frozen.checksums) {
                (Some(a), Some(f)) => (a, f),
                // `usable()` guarantees both are `Some`.
                _ => return HeaderSource::Conflict(HeaderConflict::NoUsableHeader),
            };
            if a == f {
                HeaderSource::Single {
                    checksums: a,
                    frozen_only: false,
                }
            } else {
                HeaderSource::Conflict(HeaderConflict::ChecksumsDisagree)
            }
        }
        // Only the frozen side is usable: the case `#207` fixes.
        (false, true) => HeaderSource::Single {
            checksums: match frozen.checksums {
                Some(flag) => flag,
                None => true,
            },
            frozen_only: true,
        },
        (true, false) => HeaderSource::Single {
            checksums: match active.checksums {
                Some(flag) => flag,
                None => true,
            },
            frozen_only: false,
        },
        (false, false) => {
            if active.exists || frozen.exists {
                HeaderSource::Conflict(HeaderConflict::NoUsableHeader)
            } else {
                HeaderSource::None
            }
        }
    }
}

/// Why the accounting could not reach a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// Bytes exist after the open but no WAL existed before it: the WAL was not
    /// the thing that changed, so this accounting does not describe the open.
    PostBytesWithoutPreWal,
    /// Replay only truncates or removes; growth means the pair does not belong
    /// to one recovery.
    PostBytesExceedPreWal,
    /// No commit could be proven and none could be excluded.
    CommitEvidenceUnreadable,
}

/// The single verdict every recovery arm must act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalAccounting {
    /// No WAL before the open: nothing to account for.
    NoWal,
    /// Bytes were there, nothing had committed, and nothing survived. Lossless;
    /// open normally.
    NothingCommitted { discarded_bytes: u64 },
    /// A committed record existed and no WAL bytes survived. Refuse and keep
    /// both WALs.
    CommittedLost { pre_bytes: u64 },
    /// Bytes survived the replay, or the engine consumed a completed
    /// checkpoint. Proceed; report what was truncated.
    Replayed {
        residual_bytes: u64,
        truncated_bytes: u64,
        checkpoint_terminated: bool,
    },
    /// Refuse or report; never silently accept.
    Unknown { reason: UnknownReason },
}

/// The decision table. Total over its inputs.
///
/// `pre`/`post` are byte counts captured before the recovery open and after it
/// but **before** any `CHECKPOINT` (post-open mutation by the caller would
/// invalidate the model). `evidence` is the pre-replay scan of both WALs
/// combined with [`WalScan::merge`]. `checkpoint_terminated` is
/// [`WalScan::ended_in_checkpoint`] for the surviving pair.
pub const fn classify(
    pre: WalBytes,
    post: WalBytes,
    evidence: CommitEvidence,
    checkpoint_terminated: bool,
) -> WalAccounting {
    let pre_bytes = pre.total();
    let post_bytes = post.total();
    if pre.is_empty() {
        return if post_bytes == 0 {
            WalAccounting::NoWal
        } else {
            WalAccounting::Unknown {
                reason: UnknownReason::PostBytesWithoutPreWal,
            }
        };
    }
    if post_bytes > pre_bytes {
        return WalAccounting::Unknown {
            reason: UnknownReason::PostBytesExceedPreWal,
        };
    }
    if post_bytes == 0 {
        match evidence {
            CommitEvidence::Unknown => WalAccounting::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            },
            CommitEvidence::None => WalAccounting::NothingCommitted {
                discarded_bytes: pre_bytes,
            },
            CommitEvidence::Some if checkpoint_terminated => WalAccounting::Replayed {
                residual_bytes: 0,
                truncated_bytes: pre_bytes,
                checkpoint_terminated: true,
            },
            CommitEvidence::Some => WalAccounting::CommittedLost { pre_bytes },
        }
    } else {
        if let CommitEvidence::Unknown = evidence {
            return WalAccounting::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            };
        }
        WalAccounting::Replayed {
            residual_bytes: post_bytes,
            truncated_bytes: pre_bytes - post_bytes,
            checkpoint_terminated,
        }
    }
}

impl WalAccounting {
    /// A stable machine-readable slug for logs and recovery telemetry.
    pub const fn slug(self) -> &'static str {
        match self {
            WalAccounting::NoWal => "no_wal",
            WalAccounting::NothingCommitted { .. } => "nothing_committed",
            WalAccounting::CommittedLost { .. } => "committed_lost",
            WalAccounting::Replayed { .. } => "replayed",
            WalAccounting::Unknown { .. } => "unknown",
        }
    }

    /// Whether the arm may proceed to `CHECKPOINT`/reopen. Only the two
    /// lossless/consumed verdicts may.
    pub const fn may_proceed(self) -> bool {
        matches!(
            self,
            WalAccounting::NoWal
                | WalAccounting::NothingCommitted { .. }
                | WalAccounting::Replayed { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAL header as `WAL::writeHeader` lays it out.
    fn header(checksums: bool) -> Vec<u8> {
        let mut out = vec![0x2au8; CHECKSUM_FLAG_OFFSET]; // 16-byte database id
        out.push(u8::from(checksums));
        if checksums {
            out.extend_from_slice(&[0u8; 8]); // `ChecksumWriter` trailer
        }
        out
    }

    /// One `serializeWithLength` frame. The checksum is structural only: the
    /// scanner never verifies it.
    fn frame(record_type: u8, owner: &str, checksums: bool) -> Vec<u8> {
        let mut payload = vec![record_type];
        payload.extend_from_slice(&(owner.len() as u64).to_ne_bytes());
        payload.extend_from_slice(owner.as_bytes());
        let mut out = (payload.len() as u64).to_ne_bytes().to_vec();
        out.extend_from_slice(&payload);
        if checksums {
            out.extend_from_slice(&[0u8; 8]);
        }
        out
    }

    fn wal(checksums: bool, types: &[u8]) -> Vec<u8> {
        let mut out = header(checksums);
        for t in types {
            out.extend_from_slice(&frame(*t, "", checksums));
        }
        out
    }

    fn pair(active: u64, frozen: u64) -> WalBytes {
        WalBytes { active, frozen }
    }

    // ---- scanner -----------------------------------------------------------

    #[test]
    fn scan_reads_a_checksummed_wal_frame_by_frame() {
        let bytes = wal(true, &[1, 30, 2]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.header, WalHeader::Readable { checksums: true });
        assert_eq!(scan.frames_parsed, 3);
        assert_eq!(scan.commits, 1);
        assert_eq!(scan.last_complete_type, Some(COMMIT_RECORD_TYPE));
        assert_eq!(scan.tail, WalTail::Clean);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_reads_a_plain_wal_frame_by_frame() {
        let bytes = wal(false, &[1, 31, 2, 2]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.header, WalHeader::Readable { checksums: false });
        assert_eq!(scan.frames_parsed, 4);
        assert_eq!(scan.commits, 2);
        assert_eq!(scan.tail, WalTail::Clean);
    }

    #[test]
    fn scan_of_a_header_only_wal_is_clean_and_commitless() {
        let scan = scan_wal(&header(true));
        assert_eq!(scan.frames_parsed, 0);
        assert_eq!(scan.tail, WalTail::Clean);
        assert_eq!(scan.commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_treats_an_empty_file_as_no_records() {
        let scan = scan_wal(&[]);
        assert_eq!(scan, WalScan::EMPTY);
        assert_eq!(scan.commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_refuses_a_truncated_header() {
        let scan = scan_wal(&[0u8; 24]);
        assert_eq!(scan.header, WalHeader::Unusable);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_sees_a_torn_commit_as_no_commit_after_a_sound_prefix() {
        let mut bytes = wal(true, &[1, 30]);
        let commit = frame(COMMIT_RECORD_TYPE, "", true);
        bytes.extend_from_slice(&commit[..commit.len() - 5]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.frames_parsed, 2);
        assert_eq!(scan.commits, 0);
        assert_eq!(scan.tail, WalTail::TornTail);
        assert_eq!(scan.commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_calls_an_unattributable_first_frame_unknown() {
        // The `#207` probe shape: intact header, 0xFF over every record byte.
        let mut bytes = header(true);
        bytes.extend(std::iter::repeat(0xFFu8).take(512));
        let scan = scan_wal(&bytes);
        assert_eq!(scan.frames_parsed, 0);
        assert!(
            matches!(scan.tail, WalTail::TornTail | WalTail::Corrupt),
            "an absurd length prefix is never a clean end: {:?}",
            scan.tail
        );
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_calls_an_unknown_record_type_corrupt() {
        let bytes = wal(true, &[1, 0xAA]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.frames_parsed, 1);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_keeps_a_proven_commit_when_the_tail_is_corrupt() {
        let bytes = wal(true, &[1, 2, 0xAA]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.commits, 1);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_recognises_a_trailing_checkpoint() {
        let scan = scan_wal(&wal(true, &[1, 2, CHECKPOINT_RECORD_TYPE]));
        assert!(scan.ended_in_checkpoint());
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_of_a_torn_record_after_a_checkpoint_still_ends_in_checkpoint() {
        let mut bytes = wal(true, &[1, 2, CHECKPOINT_RECORD_TYPE]);
        bytes.extend_from_slice(&[0u8; 3]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::TornTail);
        assert!(scan.ended_in_checkpoint());
    }

    #[test]
    fn merge_prefers_a_commit_over_an_unreadable_peer() {
        let some = scan_wal(&wal(true, &[1, 2]));
        let unknown = scan_wal(&[0xFFu8; 64]);
        assert_eq!(some.merge(unknown).commit_evidence(), CommitEvidence::Some);
        assert_eq!(unknown.merge(some).commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn merge_stays_pessimistic_when_neither_side_proves_anything() {
        let clean = scan_wal(&header(true));
        let unknown = scan_wal(&[0xFFu8; 64]);
        assert_eq!(
            clean.merge(unknown).commit_evidence(),
            CommitEvidence::Unknown
        );
        assert_eq!(
            unknown.merge(clean).commit_evidence(),
            CommitEvidence::Unknown
        );
    }

    // ---- header selection --------------------------------------------------

    #[test]
    fn header_source_uses_the_only_usable_side() {
        let source = select_header_source(
            // A 0-byte active `.wal` must not hide the frozen one (#207).
            HeaderProbe::present(0, None),
            HeaderProbe::present(3103, Some(true)),
        );
        assert_eq!(
            source,
            HeaderSource::Single {
                checksums: true,
                frozen_only: true
            }
        );
    }

    #[test]
    fn header_source_accepts_a_pair_that_agrees() {
        let source = select_header_source(
            HeaderProbe::present(1100, Some(false)),
            HeaderProbe::present(3103, Some(false)),
        );
        assert_eq!(
            source,
            HeaderSource::Single {
                checksums: false,
                frozen_only: false
            }
        );
    }

    #[test]
    fn header_source_refuses_a_mixed_pair() {
        let source = select_header_source(
            HeaderProbe::present(1100, Some(true)),
            HeaderProbe::present(3103, Some(false)),
        );
        assert_eq!(
            source,
            HeaderSource::Conflict(HeaderConflict::ChecksumsDisagree)
        );
    }

    #[test]
    fn header_source_refuses_a_short_active_with_no_frozen_peer() {
        let source = select_header_source(HeaderProbe::present(18, None), HeaderProbe::ABSENT);
        assert_eq!(
            source,
            HeaderSource::Conflict(HeaderConflict::NoUsableHeader)
        );
    }

    #[test]
    fn header_source_is_none_only_when_neither_wal_exists() {
        assert_eq!(
            select_header_source(HeaderProbe::ABSENT, HeaderProbe::ABSENT),
            HeaderSource::None
        );
    }

    // ---- the truth table ---------------------------------------------------

    #[test]
    fn no_wal_when_nothing_existed_before_or_after() {
        let verdict = classify(WalBytes::ZERO, WalBytes::ZERO, CommitEvidence::None, false);
        assert_eq!(verdict, WalAccounting::NoWal);
        assert!(verdict.may_proceed());
    }

    #[test]
    fn nothing_committed_when_the_discarded_bytes_held_no_commit() {
        // The torn-only-COMMIT row, and the delete-only row's tail case.
        let verdict = classify(pair(530, 0), WalBytes::ZERO, CommitEvidence::None, false);
        assert_eq!(
            verdict,
            WalAccounting::NothingCommitted {
                discarded_bytes: 530
            }
        );
        assert!(verdict.may_proceed());
    }

    #[test]
    fn committed_lost_when_a_commit_existed_and_no_bytes_survive() {
        let verdict = classify(pair(0, 3103), WalBytes::ZERO, CommitEvidence::Some, false);
        assert_eq!(verdict, WalAccounting::CommittedLost { pre_bytes: 3103 });
        assert!(!verdict.may_proceed());
    }

    #[test]
    fn committed_lost_also_for_the_active_side_of_the_pair() {
        let verdict = classify(pair(3103, 0), WalBytes::ZERO, CommitEvidence::Some, false);
        assert_eq!(verdict, WalAccounting::CommittedLost { pre_bytes: 3103 });
    }

    #[test]
    fn a_consumed_checkpoint_is_a_replay_not_a_loss() {
        let verdict = classify(pair(3103, 0), WalBytes::ZERO, CommitEvidence::Some, true);
        assert_eq!(
            verdict,
            WalAccounting::Replayed {
                residual_bytes: 0,
                truncated_bytes: 3103,
                checkpoint_terminated: true
            }
        );
        assert!(verdict.may_proceed());
    }

    #[test]
    fn replayed_when_bytes_survive_untouched() {
        let verdict = classify(pair(530, 0), pair(530, 0), CommitEvidence::Some, false);
        assert_eq!(
            verdict,
            WalAccounting::Replayed {
                residual_bytes: 530,
                truncated_bytes: 0,
                checkpoint_terminated: false
            }
        );
    }

    #[test]
    fn replayed_reports_the_torn_tail_it_dropped() {
        let verdict = classify(pair(530, 2048), pair(530, 0), CommitEvidence::Some, false);
        assert_eq!(
            verdict,
            WalAccounting::Replayed {
                residual_bytes: 530,
                truncated_bytes: 2048,
                checkpoint_terminated: false
            }
        );
    }

    #[test]
    fn unknown_when_the_bytes_cannot_be_attributed() {
        let verdict = classify(
            pair(0, 3103),
            WalBytes::ZERO,
            CommitEvidence::Unknown,
            false,
        );
        assert_eq!(
            verdict,
            WalAccounting::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
        );
        assert!(!verdict.may_proceed());
    }

    #[test]
    fn unknown_when_an_unreadable_pair_still_has_residual_bytes() {
        // Residual bytes must not launder an unattributable scan into `Replayed`.
        let verdict = classify(pair(900, 0), pair(400, 0), CommitEvidence::Unknown, false);
        assert_eq!(
            verdict,
            WalAccounting::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
        );
    }

    #[test]
    fn unknown_when_bytes_appear_without_a_pre_replay_wal() {
        let verdict = classify(WalBytes::ZERO, pair(120, 0), CommitEvidence::None, false);
        assert_eq!(
            verdict,
            WalAccounting::Unknown {
                reason: UnknownReason::PostBytesWithoutPreWal
            }
        );
    }

    #[test]
    fn unknown_when_the_post_state_grows() {
        let verdict = classify(pair(100, 0), pair(200, 0), CommitEvidence::Some, false);
        assert_eq!(
            verdict,
            WalAccounting::Unknown {
                reason: UnknownReason::PostBytesExceedPreWal
            }
        );
    }

    #[test]
    fn every_verdict_has_a_stable_slug() {
        assert_eq!(WalAccounting::NoWal.slug(), "no_wal");
        assert_eq!(
            WalAccounting::NothingCommitted { discarded_bytes: 1 }.slug(),
            "nothing_committed"
        );
        assert_eq!(
            WalAccounting::CommittedLost { pre_bytes: 1 }.slug(),
            "committed_lost"
        );
        assert_eq!(
            WalAccounting::Replayed {
                residual_bytes: 0,
                truncated_bytes: 0,
                checkpoint_terminated: false
            }
            .slug(),
            "replayed"
        );
        assert_eq!(
            WalAccounting::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
            .slug(),
            "unknown"
        );
    }
}
