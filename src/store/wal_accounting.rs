#![allow(dead_code)] // consumed by tests only until Phase B wires it (#207)

//! One zero-record accounting model for every WAL recovery arm (`#207`).
//!
//! The recovery funnel in [`crate::store::ladybug::LadybugStore::with_system_config`]
//! has four arms that can end in a `CHECKPOINT`, a rename, or a reopen: the
//! plain verified open, the checksum-config mismatch reopen, the lenient
//! (unverified) replay, and the unreadable-WAL rename. Before `#207` only the
//! lenient replay arm had a zero-record guard, and that guard keyed on the
//! *active* `.wal` alone, so:
//!
//! * a frozen `<db>.wal.checkpoint` with no active `.wal` replayed zero records,
//!   was accepted, and the canonical frozen file was deleted by the engine;
//! * the checksum-mismatch arm checkpointed an empty replay and reported success;
//! * a WAL whose only `COMMIT` record was torn was refused as `DATA LOSS` even
//!   though nothing committed could be lost.
//!
//! # The model is **per side**
//!
//! The two files are accounted separately, never summed. Summing hides a total
//! loss of one side behind residual bytes on the other: with
//! `pre = {active: 300, frozen: 3103}`, `post = {0, 0}` and a commit in the
//! frozen WAL, a total-based model sees "bytes went away" while a
//! `post.total() > 0` shortcut would report a replay. Each side therefore gets
//! its own verdict, and the pair verdict is a commutative fold of the two:
//!
//! ```text
//! CommittedLost  >  Unknown  >  Replayed  >  NothingCommitted  >  NoWal
//! ```
//!
//! `checkpoint_terminated` is attributed **per side** too: a completed
//! `CHECKPOINT` record in the active WAL says nothing about a frozen WAL that
//! vanished.
//!
//! This module is pure: it never touches the filesystem. Callers measure bytes
//! and scan bytes ([`scan_wal`]), then ask [`classify_side`] / [`classify_pair`].
//!
//! # Where the byte layout comes from
//!
//! The pinned engine is `lbug 0.22.3`
//! (`~/.cargo/registry/src/index.crates.io-*/lbug-0.22.3/lbug-src`). Nothing in
//! the constants below is invented; each is cited to the source this crate is
//! compiled against.
//!
//! Header (`WAL::writeHeader`, `storage/wal/wal.cpp:394-402`): `uuid
//! databaseID` (16 bytes) plus `bool enableChecksums` (1 byte) — see
//! [`crate::store::wal_header`], which also owns the 17-vs-25 size rule.
//!
//! Records (`WALRecord::serializeWithLength`, `storage/wal/wal_record.cpp:21-35`
//! and `WAL::addNewWALRecordNoLock`, `wal.cpp:431-439`): a record object is
//!
//! ```text
//! [u64 recordLength][recordLength bytes payload][u64 checksum?]
//! ```
//!
//! and the payload always ends with the owner-name trailer the engine appends:
//!
//! ```text
//! [u8 type][type-specific fields][u64 ownerNameLen][ownerName]
//! ```
//!
//! The `type` byte is the first payload byte. The type-specific prefix is what
//! makes a frame checkable:
//!
//! | type | prefix before `ownerNameLen` | source |
//! |---|---|---|
//! | `COMMIT_RECORD = 2` | none (`type` only) | `records/commit_record.cpp:15-19` |
//! | `BEGIN_TRANSACTION_RECORD = 1` | none | `records/begin_transaction_record.cpp:15-19` |
//! | `CHECKPOINT_RECORD = 254` | `u64 bundleFormatVersion` (8) | `records/checkpoint_record.cpp:15-19` |
//! | `LOAD_EXTENSION_RECORD = 100` | a `std::string` path | `records/load_extension_record.cpp:15-19` |
//! | `CREATE_INDEX_RECORD = 15` | a polymorphic catalog entry + index info + tree bytes | `records/create_index_record.cpp:21-37` |
//! | everything else | type-specific, not modelled here | `records/*_record.cpp` |
//!
//! So a frame is **fixed-prefix** (trailer position known) only for the three
//! types in the first three rows; the rest are **variable** and their payload is
//! opaque to this scanner. `writeDebuggingInfo` is compiled out unless
//! `DESER_DEBUG` is set (`common/serializer/serializer.cpp:15-21`). All integers
//! are written through `reinterpret_cast<const uint8_t*>` and read the same way,
//! i.e. **host** endianness.
//!
//! # The scanner is a heuristic, and says so
//!
//! It walks frames and validates what can be validated without the engine's
//! checksum algorithm: the declared length is at least the 9-byte minimum and at
//! most [`MAX_PLAUSIBLE_RECORD_BYTES`], a frame's type byte is a known
//! `WALRecordType`, and a fixed-prefix frame's owner-name length equals
//! `recordLength - prefix`. Any violation stops the walk pessimistically.
//!
//! It is **not** a verifier. A corrupted length inside a variable-payload frame
//! can still move the walk to a plausible-looking boundary; the defences above
//! make that land on `Corrupt`/`TornTailUnvalidated` for every single-byte
//! length flip this crate tests, but a targeted corruption that happens to
//! produce a minimal torn fixed-prefix frame remains indistinguishable without
//! the checksum. Until `WALReplayInfo` is exposed by the binding or the engine
//! grows a non-mutating dry run, this is a heuristic whose failures are designed
//! to land on [`CommitEvidence::Unknown`] (refuse), never on
//! [`CommitEvidence::None`] (accept).
//!
//! # Why "had a commit?" is the right question
//!
//! `WALReplayer::dryReplay` advances `offsetDeserialized` only on `COMMIT_RECORD`
//! and `CHECKPOINT_RECORD` (`storage/wal/wal_replayer.cpp:956-970`). When no such
//! record deserializes, the engine deletes the frozen WAL
//! (`wal_replayer.cpp:674-679`) or truncates the active one to 0 (`:807`). "Zero
//! records replayed" therefore means "no *committed* record was readable", not
//! "the WAL held nothing": a torn `COMMIT`, a delete-only transaction, and a
//! wholesale-corrupted file are all zero-record replays, but only one of them
//! lost committed data.
//!
//! Positive evidence (a complete `COMMIT_RECORD` frame) separates them. It is
//! conservative in the direction that matters: a missed commit can only turn a
//! lossless case into [`CommitEvidence::Unknown`] (refuse), never a lossy case
//! into [`CommitEvidence::None`] (accept).

use crate::store::wal_header::{read_header, HeaderRead};

/// `WALRecordType::BEGIN_TRANSACTION_RECORD` (`wal_record_base.h:21`).
pub const BEGIN_TRANSACTION_RECORD_TYPE: u8 = 1;

/// `WALRecordType::COMMIT_RECORD` (`wal_record_base.h:22`).
pub const COMMIT_RECORD_TYPE: u8 = 2;

/// `WALRecordType::CHECKPOINT_RECORD` (`wal_record_base.h:39`).
pub const CHECKPOINT_RECORD_TYPE: u8 = 254;

/// Smallest possible record: `u8 type` + `u64 ownerNameLen`.
pub const MIN_RECORD_LEN: u64 = 9;

/// Upper bound for a single record's declared length. No record this crate has
/// ever observed comes close; the bound exists so a scrambled `0xFFFF…` length is
/// `Corrupt` (refuse) instead of a "torn" frame (which could be accepted). A
/// legitimate record larger than this would be refused, never accepted silently.
pub const MAX_PLAUSIBLE_RECORD_BYTES: u64 = 1 << 30;

/// The `WALRecordType` values the pinned engine can write
/// (`wal_record_base.h:18-40`). A frame whose type byte is outside this set is
/// not a record this code can attribute, so the scan stops pessimistically.
/// `0` (`INVALID_RECORD`) is deliberately absent: the engine reserves it to
/// detect reads from an empty buffer (`wal_record.cpp:56-62`).
const KNOWN_RECORD_TYPES: [u8; 17] = [
    1, 2, 13, 14, 15, 16, 17, 18, 19, 30, 31, 32, 33, 34, 35, 100, 254,
];

/// Bytes from the start of the payload through `ownerNameLen` for types whose
/// payload prefix is fixed. `None` means the prefix is variable (opaque).
///
/// `CHECKPOINT_RECORD` carries `bundleFormatVersion` before the trailer
/// (`checkpoint_record.cpp:15-19`); `BEGIN`/`COMMIT` carry nothing.
const fn fixed_prefix_len(record_type: u8) -> Option<u64> {
    match record_type {
        BEGIN_TRANSACTION_RECORD_TYPE | COMMIT_RECORD_TYPE => Some(MIN_RECORD_LEN),
        CHECKPOINT_RECORD_TYPE => Some(17),
        _ => None,
    }
}

/// Which of the two WAL files a verdict is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SideSet(u8);

impl SideSet {
    pub const EMPTY: Self = Self(0);
    pub const ACTIVE: Self = Self(1);
    pub const FROZEN: Self = Self(2);
    pub const BOTH: Self = Self(3);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `"active"`, `"frozen"`, `"active+frozen"` or `"-"`.
    pub const fn label(self) -> &'static str {
        match self.0 {
            0 => "-",
            1 => "active",
            2 => "frozen",
            3 => "active+frozen",
            _ => "?",
        }
    }
}

/// What one side's pre-replay bytes prove about committed records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitEvidence {
    /// At least one complete `COMMIT_RECORD` frame existed before replay.
    Some,
    /// The WAL was walked to a sound stopping point and held no commit.
    None,
    /// The bytes could not be attributed. Never treat this as `None`.
    Unknown,
}

/// How a WAL file's frame walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalTail {
    /// The file ended exactly on a frame boundary.
    Clean,
    /// The final frame is a torn write whose type and trailer validate: a real
    /// record that never finished.
    TornTail,
    /// The final frame is torn but could not be validated (variable payload, or
    /// too few bytes to check the trailer). It could be a real partial write or
    /// a corrupted length that landed here.
    TornTailUnvalidated,
    /// A frame boundary cannot be trusted at all.
    Corrupt,
}

/// The result of walking one WAL file's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalScan {
    pub header: HeaderRead,
    /// Complete frames walked before the stopping point.
    pub frames_parsed: u32,
    /// Frames whose owner-name trailer was checked against the declared length.
    pub validated_frames: u32,
    /// Complete `COMMIT_RECORD` frames.
    pub commits: u32,
    /// The last complete frame's `WALRecordType`, if any.
    pub last_complete_type: Option<u8>,
    pub tail: WalTail,
}

impl WalScan {
    /// A file with no bytes. Distinct from a too-short header: there is nothing
    /// to replay and nothing was lost.
    pub const EMPTY: Self = Self {
        header: HeaderRead::Absent,
        frames_parsed: 0,
        validated_frames: 0,
        commits: 0,
        last_complete_type: None,
        tail: WalTail::Clean,
    };

    /// Whether this side ended with a complete `CHECKPOINT_RECORD` and no later
    /// record. `dryReplay` records exactly this as `isLastRecordCheckpoint`, and
    /// the engine only removes the WAL after applying that checkpoint
    /// (`wal_replayer.cpp:946-964`, `:674-679`). A torn tail *after* the
    /// checkpoint keeps the flag: the engine's switch never runs for the record
    /// that threw.
    pub const fn ended_in_checkpoint(self) -> bool {
        matches!(self.last_complete_type, Some(CHECKPOINT_RECORD_TYPE))
            && !matches!(self.tail, WalTail::Corrupt)
    }

    /// What these bytes prove about committed records.
    ///
    /// `None` requires *positive* evidence that the walk is trustworthy: a
    /// readable header and a stopping point that is either an exact frame
    /// boundary or a validated torn fixed-prefix frame. Everything else is
    /// `Unknown`.
    pub const fn commit_evidence(self) -> CommitEvidence {
        match self.header {
            HeaderRead::Absent => CommitEvidence::None,
            HeaderRead::TooShort { .. } => CommitEvidence::Unknown,
            HeaderRead::Readable { .. } => {
                if self.commits > 0 {
                    CommitEvidence::Some
                } else {
                    match self.tail {
                        WalTail::Clean | WalTail::TornTail => CommitEvidence::None,
                        WalTail::TornTailUnvalidated | WalTail::Corrupt => CommitEvidence::Unknown,
                    }
                }
            }
        }
    }
}

/// Walks one WAL file's frames. `bytes` is the whole file.
///
/// The walk never resynchronises: the first frame it cannot attribute ends it.
/// That is the property that keeps a corrupted middle from masquerading as a
/// clean prefix.
pub fn scan_wal(bytes: &[u8]) -> WalScan {
    let header = read_header(bytes);
    let (checksums, header_bytes) = match header {
        HeaderRead::Readable {
            checksums,
            header_bytes,
        } => (checksums, header_bytes),
        HeaderRead::Absent | HeaderRead::TooShort { .. } => {
            return WalScan {
                header,
                frames_parsed: 0,
                validated_frames: 0,
                commits: 0,
                last_complete_type: None,
                tail: WalTail::Clean,
            };
        }
    };
    let mut scan = WalScan {
        header,
        frames_parsed: 0,
        validated_frames: 0,
        commits: 0,
        last_complete_type: None,
        tail: WalTail::Clean,
    };
    let checksum_bytes: u64 = if checksums { 8 } else { 0 };
    let mut cursor: usize = header_bytes as usize;

    loop {
        let remaining = bytes.len() - cursor;
        if remaining == 0 {
            scan.tail = WalTail::Clean;
            return scan;
        }
        if remaining < 8 {
            // Not even a whole length prefix is on disk; the type cannot be
            // peeked, so this tail cannot be validated.
            scan.tail = WalTail::TornTailUnvalidated;
            return scan;
        }
        let record_length = read_u64(bytes, cursor);
        if record_length < MIN_RECORD_LEN || record_length > MAX_PLAUSIBLE_RECORD_BYTES {
            scan.tail = WalTail::Corrupt;
            return scan;
        }
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
        if frame_len > remaining as u64 {
            scan.tail = torn_tail(bytes, cursor, remaining, record_length);
            return scan;
        }
        let record_type = bytes[cursor + 8];
        if !KNOWN_RECORD_TYPES.contains(&record_type) {
            scan.tail = WalTail::Corrupt;
            return scan;
        }
        if let Some(fixed) = fixed_prefix_len(record_type) {
            if record_length < fixed {
                scan.tail = WalTail::Corrupt;
                return scan;
            }
            // `ownerNameLen` sits at `payload_start + fixed - 8`, i.e. `cursor + fixed`.
            let owner_len = read_u64(bytes, cursor + fixed as usize);
            if owner_len != record_length - fixed {
                scan.tail = WalTail::Corrupt;
                return scan;
            }
            scan.validated_frames = scan.validated_frames.saturating_add(1);
        }
        scan.frames_parsed = scan.frames_parsed.saturating_add(1);
        if record_type == COMMIT_RECORD_TYPE {
            scan.commits = scan.commits.saturating_add(1);
        }
        scan.last_complete_type = Some(record_type);
        cursor += frame_len as usize;
    }
}

/// Classifies a final frame whose declared length runs past the end of the file.
///
/// The declared length and the type byte are the first bytes the writer emits,
/// so they are checkable when they are present. A fixed-prefix type is accepted
/// as a genuine torn write only when its trailer validates, or — when the tear
/// leaves too few bytes to read the trailer — when the declared length is
/// exactly that type's minimal payload, i.e. a record with no owner name. A
/// variable payload cannot be validated at all, so it is never trusted.
fn torn_tail(bytes: &[u8], cursor: usize, remaining: usize, record_length: u64) -> WalTail {
    if remaining < 9 {
        return WalTail::TornTailUnvalidated;
    }
    let record_type = bytes[cursor + 8];
    if !KNOWN_RECORD_TYPES.contains(&record_type) {
        return WalTail::Corrupt;
    }
    match fixed_prefix_len(record_type) {
        Some(fixed) if record_length >= fixed => {
            if (remaining as u64) >= 8 + fixed {
                let owner_len = read_u64(bytes, cursor + fixed as usize);
                if owner_len == record_length - fixed {
                    WalTail::TornTail
                } else {
                    WalTail::Corrupt
                }
            } else if record_length == fixed {
                // Only the length and type are on disk; both agree with a
                // minimal record of this type.
                WalTail::TornTail
            } else {
                WalTail::TornTailUnvalidated
            }
        }
        Some(_) => WalTail::Corrupt,
        None => WalTail::TornTailUnvalidated,
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[offset..offset + 8]);
    // Host-endian: the engine writes the `u64` through a byte cast
    // (`Serializer::serializeValue`), and every target this crate builds for is
    // little-endian. `from_ne_bytes` states that dependency instead of hiding it.
    u64::from_ne_bytes(buf)
}

/// Why a side's accounting could not reach a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// No commit could be proven and none could be excluded.
    CommitEvidenceUnreadable,
}

/// What one WAL side did across the recovery open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideVerdict {
    /// Nothing before, nothing after.
    NoWal,
    /// Nothing before, bytes after: the open created a WAL header (a 25-byte
    /// checksums-on file). Report, never refuse.
    FreshWal { bytes: u64 },
    /// Bytes before, nothing after, no committed record among them. Lossless.
    NothingCommitted { uncommitted_bytes: u64 },
    /// A committed record existed and no bytes survive on this side.
    CommittedLost { pre_bytes: u64 },
    /// Bytes survive, or the engine consumed a completed checkpoint.
    Replayed {
        pre_bytes: u64,
        residual_bytes: u64,
        truncated_bytes: u64,
        checkpoint_terminated: bool,
    },
    /// Refuse or report; never silently accept.
    Unknown { reason: UnknownReason },
}

/// Per-side inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideObservation {
    /// Bytes on this side before the recovery open (`0` when absent or empty —
    /// deliberately indistinguishable).
    pub pre: u64,
    /// Bytes on this side after the open and before any `CHECKPOINT`.
    pub post: u64,
    /// The pre-replay scan, required when `pre > 0`.
    pub scan: Option<WalScan>,
}

impl SideObservation {
    pub const fn empty() -> Self {
        Self {
            pre: 0,
            post: 0,
            scan: None,
        }
    }
}

/// Both sides, measured the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalPairObservation {
    pub active: SideObservation,
    pub frozen: SideObservation,
}

impl WalPairObservation {
    pub const fn new(active: SideObservation, frozen: SideObservation) -> Self {
        Self { active, frozen }
    }
}

/// One side's verdict. Order of the checks is the safety order:
/// [`CommitEvidence::Some`] first, so a proven commit can never be accepted.
pub fn classify_side(observation: SideObservation) -> SideVerdict {
    let SideObservation { pre, post, scan } = observation;
    if pre == 0 {
        return if post == 0 {
            SideVerdict::NoWal
        } else {
            SideVerdict::FreshWal { bytes: post }
        };
    }
    let scan = match scan {
        Some(scan) => scan,
        None => {
            return SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            }
        }
    };
    let evidence = scan.commit_evidence();
    if evidence == CommitEvidence::Unknown {
        return SideVerdict::Unknown {
            reason: UnknownReason::CommitEvidenceUnreadable,
        };
    }
    let checkpoint_terminated = scan.ended_in_checkpoint();
    if post == 0 {
        match evidence {
            CommitEvidence::None => SideVerdict::NothingCommitted {
                uncommitted_bytes: pre,
            },
            // `Some` and not a consumed checkpoint: the bytes held a committed
            // record and are gone.
            _ if !checkpoint_terminated => SideVerdict::CommittedLost { pre_bytes: pre },
            _ => SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: 0,
                truncated_bytes: pre,
                checkpoint_terminated: true,
            },
        }
    } else {
        SideVerdict::Replayed {
            pre_bytes: pre,
            residual_bytes: post,
            truncated_bytes: pre.saturating_sub(post),
            checkpoint_terminated,
        }
    }
}

/// The accounting verdict for the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalAccounting {
    /// No WAL on either side before or after.
    NoWal,
    /// Nothing had committed and nothing survived. Lossless; open normally.
    NothingCommitted {
        uncommitted_bytes: u64,
        sides: SideSet,
    },
    /// A committed record existed on at least one side and no bytes survive
    /// there. Refuse and keep both WALs.
    CommittedLost { pre_bytes: u64, sides: SideSet },
    /// Bytes survived, or the engine consumed a completed checkpoint. Proceed;
    /// report what changed. `pre_bytes == 0` with `residual_bytes > 0` is a fresh
    /// WAL header created by the open, not a replay.
    Replayed {
        pre_bytes: u64,
        residual_bytes: u64,
        truncated_bytes: u64,
        checkpoint_terminated: bool,
    },
    /// Refuse or report; never silently accept.
    Unknown {
        reason: UnknownReason,
        sides: SideSet,
    },
}

/// Folds the two side verdicts into one accounting verdict.
///
/// The fold is a **join**, not a merge of scans: it is commutative up to the
/// side labels (`CommittedLost` on the active side and on the frozen side
/// produce the same structure with `sides` swapped), associative, and its
/// identity is `NoWal`. Severity order:
/// `CommittedLost > Unknown > Replayed > NothingCommitted > NoWal`.
pub fn fold_side_verdicts(active: SideVerdict, frozen: SideVerdict) -> WalAccounting {
    let sides = [(SideSet::ACTIVE, active), (SideSet::FROZEN, frozen)];

    let mut lost_sides = SideSet::EMPTY;
    let mut lost_bytes = 0u64;
    for (side, verdict) in sides {
        if let SideVerdict::CommittedLost { pre_bytes } = verdict {
            lost_sides = lost_sides.union(side);
            lost_bytes = lost_bytes.saturating_add(pre_bytes);
        }
    }
    if !lost_sides.is_empty() {
        return WalAccounting::CommittedLost {
            pre_bytes: lost_bytes,
            sides: lost_sides,
        };
    }

    let mut unknown_sides = SideSet::EMPTY;
    let mut unknown_reason = None;
    for (side, verdict) in sides {
        if let SideVerdict::Unknown { reason } = verdict {
            unknown_sides = unknown_sides.union(side);
            unknown_reason = Some(reason);
        }
    }
    if let Some(reason) = unknown_reason {
        return WalAccounting::Unknown {
            reason,
            sides: unknown_sides,
        };
    }

    let mut pre_bytes = 0u64;
    let mut residual_bytes = 0u64;
    let mut truncated_bytes = 0u64;
    let mut checkpoint_terminated = false;
    let mut moved = false;
    for (_, verdict) in sides {
        match verdict {
            SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: residual,
                truncated_bytes: truncated,
                checkpoint_terminated: ckpt,
            } => {
                moved = true;
                pre_bytes = pre_bytes.saturating_add(pre);
                residual_bytes = residual_bytes.saturating_add(residual);
                truncated_bytes = truncated_bytes.saturating_add(truncated);
                checkpoint_terminated |= ckpt;
            }
            SideVerdict::FreshWal { bytes } => {
                moved = true;
                residual_bytes = residual_bytes.saturating_add(bytes);
            }
            _ => {}
        }
    }
    if moved {
        return WalAccounting::Replayed {
            pre_bytes,
            residual_bytes,
            truncated_bytes,
            checkpoint_terminated,
        };
    }

    let mut uncommitted_bytes = 0u64;
    let mut nothing_sides = SideSet::EMPTY;
    for (side, verdict) in sides {
        if let SideVerdict::NothingCommitted {
            uncommitted_bytes: bytes,
        } = verdict
        {
            nothing_sides = nothing_sides.union(side);
            uncommitted_bytes = uncommitted_bytes.saturating_add(bytes);
        }
    }
    if !nothing_sides.is_empty() {
        return WalAccounting::NothingCommitted {
            uncommitted_bytes,
            sides: nothing_sides,
        };
    }
    WalAccounting::NoWal
}

/// Measures both sides and folds them.
pub fn classify_pair(pair: WalPairObservation) -> WalAccounting {
    fold_side_verdicts(classify_side(pair.active), classify_side(pair.frozen))
}

/// What a refusing arm must do with the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The recovery may `CHECKPOINT`/reopen.
    Proceed,
    /// Refuse before any `CHECKPOINT`, and keep both WALs. `restore` names the
    /// sides that lost **all** of their bytes and may therefore be restored from
    /// the preserved copy; a side with residual bytes must not be restored over
    /// the engine's truncation (that would re-offer records the engine already
    /// parsed, and it is not needed — its bytes are still on disk).
    Refuse { restore: SideSet },
}

/// Maps a verdict plus the observations to what the arm may do. Refusal is only
/// ever returned for `CommittedLost`/`Unknown`; restoration is only ever offered
/// for a side whose `post == 0`.
pub fn disposition(verdict: WalAccounting, pair: WalPairObservation) -> Disposition {
    if verdict.may_proceed() {
        return Disposition::Proceed;
    }
    let mut restore = SideSet::EMPTY;
    if pair.active.pre > 0 && pair.active.post == 0 {
        restore = restore.union(SideSet::ACTIVE);
    }
    if pair.frozen.pre > 0 && pair.frozen.post == 0 {
        restore = restore.union(SideSet::FROZEN);
    }
    Disposition::Refuse { restore }
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

    /// Whether the arm may proceed to `CHECKPOINT`/reopen.
    pub const fn may_proceed(self) -> bool {
        matches!(
            self,
            WalAccounting::NoWal
                | WalAccounting::NothingCommitted { .. }
                | WalAccounting::Replayed { .. }
        )
    }

    /// Which sides the verdict is about.
    pub const fn sides(self) -> SideSet {
        match self {
            WalAccounting::CommittedLost { sides, .. } | WalAccounting::Unknown { sides, .. } => {
                sides
            }
            WalAccounting::NothingCommitted { sides, .. } => sides,
            WalAccounting::NoWal | WalAccounting::Replayed { .. } => SideSet::BOTH,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fixtures ----------------------------------------------------------

    /// A WAL header as `WAL::writeHeader` lays it out.
    fn header(checksums: bool) -> Vec<u8> {
        let mut out = vec![0x2au8; 16]; // 16-byte database id
        out.push(u8::from(checksums));
        if checksums {
            out.extend_from_slice(&[0u8; 8]); // `ChecksumWriter` trailer
        }
        out
    }

    /// One `serializeWithLength` frame, valid by construction. The checksum is
    /// structural only: the scanner never verifies it.
    fn record(record_type: u8, owner: &str, checksums: bool) -> Vec<u8> {
        let fixed = fixed_prefix_len(record_type).unwrap_or(MIN_RECORD_LEN);
        let mut payload = vec![record_type];
        payload.extend(std::iter::repeat(0u8).take((fixed - MIN_RECORD_LEN) as usize));
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
            out.extend_from_slice(&record(*t, "", checksums));
        }
        out
    }

    fn truncate(bytes: &[u8], cut: usize) -> Vec<u8> {
        bytes[..bytes.len() - cut].to_vec()
    }

    fn scan_no_commit() -> WalScan {
        scan_wal(&wal(true, &[1]))
    }

    fn scan_with_commit() -> WalScan {
        scan_wal(&wal(true, &[1, 2]))
    }

    fn scan_with_checkpoint() -> WalScan {
        scan_wal(&wal(true, &[1, 2, CHECKPOINT_RECORD_TYPE]))
    }

    fn scan_torn_no_commit() -> WalScan {
        // BEGIN, a variable record, then a COMMIT whose trailer is cut off.
        scan_wal(&truncate(&wal(true, &[1, 30, 2]), 2))
    }

    fn scan_torn_unvalidated() -> WalScan {
        // A torn variable-payload frame: nothing about it is checkable.
        let mut bytes = wal(true, &[1]);
        bytes.extend_from_slice(&record(30, &"x".repeat(40), true));
        scan_wal(&truncate(&bytes, 5))
    }

    fn scan_unattributable() -> WalScan {
        let mut bytes = header(true);
        bytes.extend(std::iter::repeat(0xFFu8).take(512));
        scan_wal(&bytes)
    }

    fn side(pre: u64, post: u64, scan: Option<WalScan>) -> SideObservation {
        SideObservation { pre, post, scan }
    }

    // ---- scanner: framing and validation (B1, B7) --------------------------

    #[test]
    fn scan_reads_a_checksummed_wal_frame_by_frame() {
        let scan = scan_wal(&wal(true, &[1, 30, 2]));
        assert_eq!(
            scan.header,
            HeaderRead::Readable {
                checksums: true,
                header_bytes: 25
            }
        );
        assert_eq!(scan.frames_parsed, 3);
        assert_eq!(
            scan.validated_frames, 2,
            "BEGIN and COMMIT have fixed prefixes"
        );
        assert_eq!(scan.commits, 1);
        assert_eq!(scan.last_complete_type, Some(COMMIT_RECORD_TYPE));
        assert_eq!(scan.tail, WalTail::Clean);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_reads_a_plain_wal_frame_by_frame() {
        let scan = scan_wal(&wal(false, &[1, 31, 2, 2]));
        assert_eq!(
            scan.header,
            HeaderRead::Readable {
                checksums: false,
                header_bytes: 17
            }
        );
        assert_eq!(scan.frames_parsed, 4);
        assert_eq!(scan.commits, 2);
        assert_eq!(scan.tail, WalTail::Clean);
    }

    #[test]
    fn scan_accepts_a_seventeen_byte_checksums_off_wal() {
        // B7: a legitimate header-only checksums-off WAL is readable at 17 bytes.
        let scan = scan_wal(&header(false));
        assert_eq!(
            scan.header,
            HeaderRead::Readable {
                checksums: false,
                header_bytes: 17
            }
        );
        assert_eq!(scan.frames_parsed, 0);
        assert_eq!(scan.tail, WalTail::Clean);
        assert_eq!(scan.commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_treats_an_empty_file_as_no_records() {
        assert_eq!(scan_wal(&[]), WalScan::EMPTY);
        assert_eq!(scan_wal(&[]).commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_refuses_a_truncated_header() {
        let scan = scan_wal(&header(true)[..24]);
        assert_eq!(scan.header, HeaderRead::TooShort { bytes: 24 });
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_sees_a_torn_commit_as_no_commit_after_a_sound_prefix() {
        // The tear reaches into the COMMIT payload, so its trailer is unreadable;
        // the declared length is exactly the minimal COMMIT, which is the only
        // unreadable-trailer shape accepted as a genuine torn write.
        let scan = scan_wal(&truncate(&wal(true, &[1, 30, 2]), 9));
        assert_eq!(scan.frames_parsed, 2);
        assert_eq!(scan.commits, 0);
        assert_eq!(scan.tail, WalTail::TornTail);
        assert_eq!(scan.commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_sees_a_commit_with_only_its_checksum_torn() {
        let scan = scan_wal(&truncate(&wal(true, &[1, 30, 2]), 2));
        assert_eq!(scan.commits, 0);
        assert_eq!(scan.tail, WalTail::TornTail);
        assert_eq!(scan.commit_evidence(), CommitEvidence::None);
    }

    #[test]
    fn scan_calls_a_torn_variable_frame_unknown() {
        // B1: a variable payload cannot be validated, so a tear there is never
        // trusted enough to say "nothing committed".
        let scan = scan_torn_unvalidated();
        assert_eq!(scan.tail, WalTail::TornTailUnvalidated);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_calls_an_unattributable_first_frame_unknown() {
        let scan = scan_unattributable();
        assert_eq!(scan.frames_parsed, 0);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_rejects_a_record_shorter_than_the_minimum() {
        // B1: `recordLength < 9` is impossible for a real record.
        let mut bytes = header(false);
        bytes.extend_from_slice(&8u64.to_ne_bytes());
        bytes.extend_from_slice(&[COMMIT_RECORD_TYPE; 8]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_rejects_an_implausible_declared_length() {
        let mut bytes = header(false);
        bytes.extend_from_slice(&(MAX_PLAUSIBLE_RECORD_BYTES + 1).to_ne_bytes());
        bytes.extend_from_slice(&[COMMIT_RECORD_TYPE; 16]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_rejects_a_commit_whose_owner_trailer_does_not_match() {
        // declared length 17 but ownerNameLen 0: `ownerLen != len - 9`.
        let mut bytes = header(false);
        bytes.extend_from_slice(&17u64.to_ne_bytes());
        bytes.push(COMMIT_RECORD_TYPE);
        bytes.extend_from_slice(&0u64.to_ne_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_rejects_a_checkpoint_whose_trailer_does_not_match() {
        // CHECKPOINT's prefix is 17 bytes; a 9-byte one is not this engine's.
        let mut bytes = header(false);
        bytes.extend_from_slice(&9u64.to_ne_bytes());
        bytes.push(CHECKPOINT_RECORD_TYPE);
        bytes.extend_from_slice(&0u64.to_ne_bytes());
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn a_single_flipped_length_byte_never_hides_a_commit() {
        // B1 regression: the demonstrated attack flipped one byte of a length
        // prefix so the walk skipped the real COMMIT. Every single-byte mutation
        // of every length prefix in a committed WAL must leave the verdict at
        // `Some` or `Unknown` — never `None`.
        let mut bytes = header(false);
        bytes.extend_from_slice(&record(BEGIN_TRANSACTION_RECORD_TYPE, "", false));
        bytes.extend_from_slice(&record(30, &"x".repeat(40), false));
        bytes.extend_from_slice(&record(COMMIT_RECORD_TYPE, "", false));
        assert_eq!(scan_wal(&bytes).commit_evidence(), CommitEvidence::Some);

        // Length prefixes: BEGIN at 17, the variable frame at 34, COMMIT at 91.
        let prefixes = [17usize, 34, 91];
        for start in prefixes {
            for offset in 0..8 {
                for mutation in [0xFFu8, 0x01, 0x80] {
                    let mut mutated = bytes.clone();
                    mutated[start + offset] ^= mutation;
                    let scan = scan_wal(&mutated);
                    assert_ne!(
                        scan.commit_evidence(),
                        CommitEvidence::None,
                        "flip 0x{mutation:02x} of length byte {offset} at {start} \
                         hid the commit: {:?}",
                        scan
                    );
                }
            }
        }
    }

    #[test]
    fn scan_calls_an_unknown_record_type_corrupt() {
        let scan = scan_wal(&wal(true, &[1, 0xAA]));
        assert_eq!(scan.frames_parsed, 1);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_keeps_a_proven_commit_when_the_tail_is_corrupt() {
        let scan = scan_wal(&wal(true, &[1, 2, 0xAA]));
        assert_eq!(scan.commits, 1);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_recognises_a_trailing_checkpoint() {
        let scan = scan_with_checkpoint();
        assert!(scan.ended_in_checkpoint());
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_of_a_torn_record_after_a_checkpoint_still_ends_in_checkpoint() {
        let mut bytes = wal(true, &[1, 2, CHECKPOINT_RECORD_TYPE]);
        bytes.extend_from_slice(&[0u8; 3]);
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::TornTailUnvalidated);
        assert!(scan.ended_in_checkpoint());
    }

    // ---- per-side classification (B2, B3) ----------------------------------

    #[test]
    fn no_wal_when_nothing_existed_before_or_after() {
        let verdict = classify_pair(WalPairObservation::new(
            SideObservation::empty(),
            SideObservation::empty(),
        ));
        assert_eq!(verdict, WalAccounting::NoWal);
        assert!(verdict.may_proceed());
    }

    #[test]
    fn nothing_committed_when_the_uncommitted_bytes_held_no_commit() {
        let verdict = classify_side(side(530, 0, Some(scan_no_commit())));
        assert_eq!(
            verdict,
            SideVerdict::NothingCommitted {
                uncommitted_bytes: 530
            }
        );
    }

    #[test]
    fn committed_lost_when_a_commit_existed_and_no_bytes_survive() {
        let verdict = classify_side(side(3103, 0, Some(scan_with_commit())));
        assert_eq!(verdict, SideVerdict::CommittedLost { pre_bytes: 3103 });
    }

    #[test]
    fn a_consumed_checkpoint_is_a_replay_not_a_loss() {
        let verdict = classify_side(side(3103, 0, Some(scan_with_checkpoint())));
        assert_eq!(
            verdict,
            SideVerdict::Replayed {
                pre_bytes: 3103,
                residual_bytes: 0,
                truncated_bytes: 3103,
                checkpoint_terminated: true
            }
        );
    }

    #[test]
    fn replayed_when_bytes_survive_untouched() {
        let verdict = classify_side(side(530, 530, Some(scan_with_commit())));
        assert_eq!(
            verdict,
            SideVerdict::Replayed {
                pre_bytes: 530,
                residual_bytes: 530,
                truncated_bytes: 0,
                checkpoint_terminated: false
            }
        );
    }

    #[test]
    fn partial_active_truncation_is_a_replay_with_a_loss_report() {
        // B-non-blocking: the engine truncated 2 000 tail bytes but kept the
        // committed prefix; that is a replay with a report, not a refusal.
        let verdict = classify_side(side(2530, 530, Some(scan_with_commit())));
        assert_eq!(
            verdict,
            SideVerdict::Replayed {
                pre_bytes: 2530,
                residual_bytes: 530,
                truncated_bytes: 2000,
                checkpoint_terminated: false
            }
        );
    }

    #[test]
    fn a_fresh_wal_is_reported_not_refused() {
        // The open created a 25-byte checksums-on WAL header where none existed.
        let verdict = classify_pair(WalPairObservation::new(
            SideObservation::empty(),
            side(0, 25, None),
        ));
        assert_eq!(
            verdict,
            WalAccounting::Replayed {
                pre_bytes: 0,
                residual_bytes: 25,
                truncated_bytes: 0,
                checkpoint_terminated: false
            }
        );
        assert!(verdict.may_proceed());
    }

    #[test]
    fn unknown_when_the_bytes_cannot_be_attributed() {
        let verdict = classify_side(side(3103, 0, Some(scan_unattributable())));
        assert_eq!(
            verdict,
            SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
        );
    }

    #[test]
    fn unknown_when_an_unreadable_side_still_has_residual_bytes() {
        let verdict = classify_side(side(900, 400, Some(scan_unattributable())));
        assert_eq!(
            verdict,
            SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
        );
    }

    #[test]
    fn committed_lost_when_a_frozen_loss_is_masked_by_a_residual_active_wal() {
        // B2: `pre = {300, 3103}`, `post = {0, 0}` with the commit on the frozen
        // side. Summing the pair hides the loss behind the other side's bytes.
        let verdict = classify_pair(WalPairObservation::new(
            side(300, 300, Some(scan_no_commit())),
            side(3103, 0, Some(scan_with_commit())),
        ));
        assert_eq!(
            verdict,
            WalAccounting::CommittedLost {
                pre_bytes: 3103,
                sides: SideSet::FROZEN
            }
        );
        assert!(!verdict.may_proceed());
    }

    #[test]
    fn a_checkpoint_on_one_side_does_not_excuse_the_other() {
        // B3: the active side consumed a completed checkpoint; the frozen side
        // still lost a committed WAL with no checkpoint of its own.
        let verdict = classify_pair(WalPairObservation::new(
            side(5000, 0, Some(scan_with_checkpoint())),
            side(3103, 0, Some(scan_with_commit())),
        ));
        assert_eq!(
            verdict,
            WalAccounting::CommittedLost {
                pre_bytes: 3103,
                sides: SideSet::FROZEN
            }
        );
    }

    #[test]
    fn both_sides_lost_are_both_named() {
        let verdict = classify_pair(WalPairObservation::new(
            side(300, 0, Some(scan_with_commit())),
            side(3103, 0, Some(scan_with_commit())),
        ));
        assert_eq!(
            verdict,
            WalAccounting::CommittedLost {
                pre_bytes: 3403,
                sides: SideSet::BOTH
            }
        );
    }

    #[test]
    fn unknown_outranks_replayed_but_not_a_proven_loss() {
        let replayed = side(530, 530, Some(scan_with_commit()));
        let unknown = side(900, 0, Some(scan_unattributable()));
        assert!(matches!(
            classify_pair(WalPairObservation::new(replayed, unknown)),
            WalAccounting::Unknown { .. }
        ));
        let lost = side(300, 0, Some(scan_with_commit()));
        assert!(matches!(
            classify_pair(WalPairObservation::new(lost, unknown)),
            WalAccounting::CommittedLost { .. }
        ));
    }

    // ---- the corrected table, exhaustively ---------------------------------

    #[derive(Clone, Copy, Debug)]
    enum Shape {
        NoScan,
        CleanNoCommit,
        CleanCommit,
        ConsumedCheckpoint,
        TornNoCommit,
        TornUnvalidated,
        Corrupt,
    }

    fn scan_shape(shape: Shape) -> Option<WalScan> {
        match shape {
            Shape::NoScan => None,
            Shape::CleanNoCommit => Some(scan_no_commit()),
            Shape::CleanCommit => Some(scan_with_commit()),
            Shape::ConsumedCheckpoint => Some(scan_with_checkpoint()),
            Shape::TornNoCommit => Some(scan_torn_no_commit()),
            Shape::TornUnvalidated => Some(scan_torn_unvalidated()),
            Shape::Corrupt => Some(scan_unattributable()),
        }
    }

    /// An independent restatement of the per-side rules, over primitives.
    fn reference_side(pre: u64, post: u64, shape: Shape) -> SideVerdict {
        let (evidence, checkpoint) = match shape {
            Shape::NoScan | Shape::CleanNoCommit | Shape::TornNoCommit => {
                (CommitEvidence::None, false)
            }
            Shape::CleanCommit => (CommitEvidence::Some, false),
            Shape::ConsumedCheckpoint => (CommitEvidence::Some, true),
            Shape::TornUnvalidated | Shape::Corrupt => (CommitEvidence::Unknown, false),
        };
        if pre == 0 {
            return if post == 0 {
                SideVerdict::NoWal
            } else {
                SideVerdict::FreshWal { bytes: post }
            };
        }
        if matches!(shape, Shape::NoScan) {
            return SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            };
        }
        match evidence {
            CommitEvidence::Unknown => SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            },
            CommitEvidence::None if post == 0 => SideVerdict::NothingCommitted {
                uncommitted_bytes: pre,
            },
            CommitEvidence::Some if post == 0 && !checkpoint => {
                SideVerdict::CommittedLost { pre_bytes: pre }
            }
            evidence => SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: post,
                truncated_bytes: pre.saturating_sub(post),
                checkpoint_terminated: checkpoint && matches!(evidence, CommitEvidence::Some),
            },
        }
    }

    #[test]
    fn every_per_side_combination_matches_the_reference_table() {
        let shapes = [
            Shape::NoScan,
            Shape::CleanNoCommit,
            Shape::CleanCommit,
            Shape::ConsumedCheckpoint,
            Shape::TornNoCommit,
            Shape::TornUnvalidated,
            Shape::Corrupt,
        ];
        let mut rows = 0usize;
        for shape in shapes {
            for pre in [0u64, 9, 530, 3103] {
                for post in [0u64, 25, 400, 530, 5000] {
                    let observed = classify_side(side(pre, post, scan_shape(shape)));
                    let expected = reference_side(pre, post, shape);
                    assert_eq!(observed, expected, "pre={pre} post={post} shape={shape:?}");
                    assert_eq!(
                        observed_evidence(shape),
                        scan_shape(shape).map(|s| s.commit_evidence()),
                        "shape {shape:?} must scan as its name says"
                    );
                    rows += 1;
                }
            }
        }
        assert_eq!(rows, 7 * 4 * 5);
    }

    /// Guards the test fixture names against a scanner change that would make a
    /// "shape" mean something else.
    fn observed_evidence(shape: Shape) -> Option<CommitEvidence> {
        match shape {
            Shape::NoScan => None,
            Shape::CleanNoCommit | Shape::TornNoCommit => Some(CommitEvidence::None),
            Shape::CleanCommit | Shape::ConsumedCheckpoint => Some(CommitEvidence::Some),
            Shape::TornUnvalidated | Shape::Corrupt => Some(CommitEvidence::Unknown),
        }
    }

    #[test]
    fn the_pair_fold_is_a_commutative_join() {
        let shapes = [
            Shape::CleanNoCommit,
            Shape::CleanCommit,
            Shape::ConsumedCheckpoint,
            Shape::TornNoCommit,
            Shape::TornUnvalidated,
            Shape::Corrupt,
        ];
        for a in shapes {
            for b in shapes {
                let ab = classify_pair(WalPairObservation::new(
                    side(530, 0, scan_shape(a)),
                    side(3103, 0, scan_shape(b)),
                ));
                let ba = classify_pair(WalPairObservation::new(
                    side(3103, 0, scan_shape(b)),
                    side(530, 0, scan_shape(a)),
                ));
                assert_eq!(
                    swap_accounting_sides(ab),
                    ba,
                    "fold({a:?}, {b:?}) is not symmetric under a side swap"
                );
            }
        }
    }

    fn swap_accounting_sides(verdict: WalAccounting) -> WalAccounting {
        match verdict {
            WalAccounting::CommittedLost { pre_bytes, sides } => WalAccounting::CommittedLost {
                pre_bytes,
                sides: swap_sides(sides),
            },
            WalAccounting::Unknown { reason, sides } => WalAccounting::Unknown {
                reason,
                sides: swap_sides(sides),
            },
            WalAccounting::NothingCommitted {
                uncommitted_bytes,
                sides,
            } => WalAccounting::NothingCommitted {
                uncommitted_bytes,
                sides: swap_sides(sides),
            },
            other => other,
        }
    }

    fn swap_sides(sides: SideSet) -> SideSet {
        let mut out = SideSet::EMPTY;
        if sides.contains(SideSet::ACTIVE) {
            out = out.union(SideSet::FROZEN);
        }
        if sides.contains(SideSet::FROZEN) {
            out = out.union(SideSet::ACTIVE);
        }
        out
    }

    #[test]
    fn folding_never_proceeds_when_a_side_refuses() {
        let shapes = [
            Shape::CleanNoCommit,
            Shape::CleanCommit,
            Shape::TornUnvalidated,
            Shape::Corrupt,
        ];
        for a in shapes {
            for b in shapes {
                let verdict = classify_pair(WalPairObservation::new(
                    side(530, 0, scan_shape(a)),
                    side(530, 0, scan_shape(b)),
                ));
                let side_proceeds = |shape: Shape| {
                    !matches!(
                        classify_side(side(530, 0, scan_shape(shape))),
                        SideVerdict::CommittedLost { .. } | SideVerdict::Unknown { .. }
                    )
                };
                assert_eq!(
                    verdict.may_proceed(),
                    side_proceeds(a) && side_proceeds(b),
                    "fold({a:?}, {b:?}) = {verdict:?}"
                );
            }
        }
    }

    // ---- refusal protocol (B5) ---------------------------------------------

    #[test]
    fn refusal_restores_only_sides_that_lost_all_bytes() {
        let pair = WalPairObservation::new(
            side(530, 530, Some(scan_with_commit())),
            side(3103, 0, Some(scan_with_commit())),
        );
        let verdict = classify_pair(pair);
        assert!(matches!(verdict, WalAccounting::CommittedLost { .. }));
        assert_eq!(
            disposition(verdict, pair),
            Disposition::Refuse {
                restore: SideSet::FROZEN
            },
            "the active side still holds its residual bytes and must not be \
             restored over the engine's truncation"
        );
    }

    #[test]
    fn refusal_restores_both_sides_when_both_are_empty() {
        let pair = WalPairObservation::new(
            side(300, 0, Some(scan_with_commit())),
            side(3103, 0, Some(scan_with_commit())),
        );
        assert_eq!(
            disposition(classify_pair(pair), pair),
            Disposition::Refuse {
                restore: SideSet::BOTH
            }
        );
    }

    #[test]
    fn a_proceeding_verdict_never_restores() {
        for (pre, post, scan) in [
            (0u64, 0u64, None),
            (0, 25, None),
            (530, 0, Some(scan_no_commit())),
            (530, 530, Some(scan_with_commit())),
        ] {
            let pair = WalPairObservation::new(side(pre, post, scan), SideObservation::empty());
            let verdict = classify_pair(pair);
            assert!(verdict.may_proceed(), "{verdict:?}");
            assert_eq!(disposition(verdict, pair), Disposition::Proceed);
        }
    }

    // ---- telemetry ---------------------------------------------------------

    #[test]
    fn every_verdict_has_a_stable_slug() {
        assert_eq!(WalAccounting::NoWal.slug(), "no_wal");
        assert_eq!(
            WalAccounting::NothingCommitted {
                uncommitted_bytes: 1,
                sides: SideSet::ACTIVE
            }
            .slug(),
            "nothing_committed"
        );
        assert_eq!(
            WalAccounting::CommittedLost {
                pre_bytes: 1,
                sides: SideSet::FROZEN
            }
            .slug(),
            "committed_lost"
        );
        assert_eq!(
            WalAccounting::Replayed {
                pre_bytes: 0,
                residual_bytes: 0,
                truncated_bytes: 0,
                checkpoint_terminated: false
            }
            .slug(),
            "replayed"
        );
        assert_eq!(
            WalAccounting::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
                sides: SideSet::ACTIVE
            }
            .slug(),
            "unknown"
        );
    }

    #[test]
    fn side_sets_label_themselves() {
        assert_eq!(SideSet::EMPTY.label(), "-");
        assert_eq!(SideSet::ACTIVE.label(), "active");
        assert_eq!(SideSet::FROZEN.label(), "frozen");
        assert_eq!(SideSet::BOTH.label(), "active+frozen");
    }
}
