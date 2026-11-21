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
//! loss of one side behind residual bytes on the other. Each side gets its own
//! verdict and the pair verdict is a commutative fold:
//!
//! ```text
//! CommittedLost  >  Unknown  >  ForeignWal  >  Deferred  >  Replayed  >  NothingCommitted  >  NoWal
//! ```
//!
//! A checkpoint claim is attributed **per side** and typed: `Observed` means the
//! scan saw a complete `CHECKPOINT` record, `InferredByDeletion` means the
//! engine removed a frozen WAL whose committed work it applied (the
//! applied-record count is *emulated* from the recovery facts, never claimed by
//! the engine). A completed `CHECKPOINT` record in the active WAL says nothing
//! about a frozen WAL that vanished.
//!
//! # Evidence is checksum-verified
//!
//! The scanner walks frames (`WALRecord::serializeWithLength`,
//! `storage/wal/wal_record.cpp:21-35`) and, for a checksums-on WAL, verifies the
//! DuckDB checksum the pinned engine writes (`common/checksum.cpp`, ported
//! verbatim below) over:
//!
//! * the 17-byte header payload, against the 8-byte trailer at offset 17;
//! * every frame's `u64 length ‖ payload`, against the 8-byte trailer that
//!   `ChecksumWriter::onObjectEnd` appends (`checksum_writer.cpp:52-61`), because
//!   `WAL::addNewWALRecordNoLock` wraps `serializeWithLength` in
//!   `onObjectBegin`/`onObjectEnd` (`wal.cpp:431-439`).
//!
//! Any mismatch is [`WalTail::Corrupt`] → [`CommitEvidence::Unknown`] → refuse.
//! This is what makes a single flipped length or type byte impossible to hide a
//! commit behind (the previous structural-only walk could be walked past a real
//! `COMMIT`; that is why this revision verifies).
//!
//! **Checksums-off WALs have no trailer.** Their walk cannot prove the absence
//! of a commit, so `pre > 0, post == 0, no commit found` is
//! [`CommitEvidence::Unverifiable`] → [`UnknownReason::UnverifiableWal`]: refuse
//! and preserve. That over-refuses a genuinely empty checksums-off WAL, and that
//! is the accepted operator policy: a WAL written without checksums cannot be
//! distinguished from one whose commit record was destroyed, and the runbook
//! path (inspect the preserved copy) is cheaper than silent loss.
//!
//! The scan is capped at [`MAX_SCAN_BYTES`]. A larger file is not walked: it
//! becomes [`SideVerdict::Deferred`] (defer to the verified open), never a
//! refusal and never an implicit "nothing committed".
//!
//! # Why "had a commit?" is the right question
//!
//! `WALReplayer::dryReplay` advances `offsetDeserialized` only on `COMMIT_RECORD`
//! and `CHECKPOINT_RECORD` (`wal_replayer.cpp:956-970`). When no such record
//! deserializes, the engine deletes the frozen WAL (`wal_replayer.cpp:674-679`)
//! or truncates the active one to 0 (`:807`). "Zero records replayed" therefore
//! means "no *committed* record was readable", not "the WAL held nothing".
//!
//! # Record layout
//!
//! Header: [`crate::store::wal_header`] (17 bytes, or 25 with a checksum).
//!
//! Frame payload: `[u8 type][type-specific fields][u64 ownerNameLen][ownerName]`.
//! Only three types have a fixed prefix before the trailer:
//!
//! | type | prefix | source |
//! |---|---|---|
//! | `COMMIT_RECORD = 2` | `type` only (9) | `records/commit_record.cpp:15-19` |
//! | `BEGIN_TRANSACTION_RECORD = 1` | `type` only (9) | `records/begin_transaction_record.cpp:15-19` |
//! | `CHECKPOINT_RECORD = 254` | `u64 bundleFormatVersion` (17) | `records/checkpoint_record.cpp:15-19` |
//!
//! `LOAD_EXTENSION_RECORD` (`records/load_extension_record.cpp:15-19`) and
//! `CREATE_INDEX_RECORD` (`records/create_index_record.cpp:21-37`) are variable,
//! as is every other type. `writeDebuggingInfo` is compiled out unless
//! `DESER_DEBUG` is set (`common/serializer/serializer.cpp:15-21`). All integers
//! are host-endian.
//!
//! This module is pure: it never touches the filesystem. Callers measure bytes
//! and scan bytes, then ask [`classify_pair`].

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
/// `Corrupt` (refuse) instead of a "torn" frame (which could be accepted).
pub const MAX_PLAUSIBLE_RECORD_BYTES: u64 = 1 << 30;

/// Largest WAL the scanner will walk.
///
/// Headroom over the engine's own auto-checkpoint threshold: `autoCheckpoint`
/// defaults to true with `checkpointThreshold = 16777216` (16 MiB,
/// `include/main/database.h:81`; `lbug/src/database.rs:60-63`), so a healthy
/// crash can leave a WAL at or above 16 MiB. 64 MiB keeps a fully verifiable
/// 16–64 MiB WAL classifiable; anything larger is *deferred*, never refused.
/// The caller reads at most `cap + 1` bytes.
pub const MAX_SCAN_BYTES: u64 = 64 << 20;

/// The `WALRecordType` values the pinned engine can write
/// (`wal_record_base.h:18-40`). `0` (`INVALID_RECORD`) is deliberately absent.
const KNOWN_RECORD_TYPES: [u8; 17] = [
    1, 2, 13, 14, 15, 16, 17, 18, 19, 30, 31, 32, 33, 34, 35, 100, 254,
];

/// Bytes from the start of the payload through `ownerNameLen` for types whose
/// payload prefix is fixed. `None` means the prefix is variable (opaque).
const fn fixed_prefix_len(record_type: u8) -> Option<u64> {
    match record_type {
        BEGIN_TRANSACTION_RECORD_TYPE | COMMIT_RECORD_TYPE => Some(MIN_RECORD_LEN),
        CHECKPOINT_RECORD_TYPE => Some(17),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// DuckDB checksum, ported from lbug 0.22.3 `common/checksum.cpp`
// ---------------------------------------------------------------------------
//
// The engine's `ChecksumWriter`/`ChecksumReader` call `common::checksum(buffer,
// size)` over the bytes between `onObjectBegin` and `onObjectEnd`. The function
// is DuckDB's (`duckdb v1.3-ossivalis`, MIT), and this port is literal: 8-byte
// blocks hashed with the `finalizer` multiply, then the 0-7 byte remainder with
// the MurmurHash-style tail.

/// `checksum(uint64_t)`: the per-block finalizer.
const fn checksum_block(x: u64) -> u64 {
    x.wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

const REMAINDER_M: u64 = 0xc6a4_a793_5bd1_e995;
const REMAINDER_SEED: u64 = 0xe17a_1465;
const REMAINDER_R: u32 = 47;

/// `checksumRemainder` for 0-7 trailing bytes.
fn checksum_remainder(data: &[u8]) -> u64 {
    let len = data.len();
    let mut h = REMAINDER_SEED ^ (len as u64).wrapping_mul(REMAINDER_M);
    let blocks = len / 8;
    for i in 0..blocks {
        let mut k = u64::from_ne_bytes(data[i * 8..i * 8 + 8].try_into().unwrap());
        k = k.wrapping_mul(REMAINDER_M);
        k ^= k >> REMAINDER_R;
        k = k.wrapping_mul(REMAINDER_M);
        h ^= k;
        h = h.wrapping_mul(REMAINDER_M);
    }
    let tail = &data[blocks * 8..];
    // The C++ switch falls through from `len & 7` to case 1, XORing each byte at
    // its little-endian position, then multiplies once for case 1.
    for (i, byte) in tail.iter().enumerate() {
        h ^= (*byte as u64) << (8 * i);
    }
    if !tail.is_empty() {
        h = h.wrapping_mul(REMAINDER_M);
    }
    h ^= h >> REMAINDER_R;
    h = h.wrapping_mul(REMAINDER_M);
    h ^= h >> REMAINDER_R;
    h
}

/// `common::checksum(uint8_t*, size_t)` from the pinned engine, byte for byte.
///
/// `common/checksum.cpp:73-87`: start at 5381, XOR the finalizer of every
/// 8-byte block, then XOR the remainder hash when the size is not a multiple of
/// eight. Endianness is native, matching the `reinterpret_cast` the engine uses.
pub fn wal_checksum(buffer: &[u8]) -> u64 {
    let mut result: u64 = 5381;
    let blocks = buffer.len() / 8;
    for i in 0..blocks {
        let value = u64::from_ne_bytes(buffer[i * 8..i * 8 + 8].try_into().unwrap());
        result ^= checksum_block(value);
    }
    if buffer.len() % 8 > 0 {
        result ^= checksum_remainder(&buffer[blocks * 8..]);
    }
    result
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

/// Byte sizes of both WAL sides. `0` covers "absent" and "present but empty"
/// on purpose: no arm may key on existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalPairBytes {
    pub active: u64,
    pub frozen: u64,
}

impl WalPairBytes {
    /// Both sides empty. Test fixture value.
    #[allow(dead_code)]
    pub const ZERO: Self = Self {
        active: 0,
        frozen: 0,
    };
}

/// One of the two WAL files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalSide {
    Active,
    Frozen,
}

/// The sides a **successful** lenient open consumed.
///
/// The inner set is private and there is deliberately **no** `union`, `insert`
/// or `SideSet` accessor: the only way to build a value is [`consumed_sides`],
/// which never covers the active side. [`WalPairObservation::consumed`] is the
/// only way to attach one, so a call site cannot widen the set with
/// `.union(SideSet::ACTIVE)` without a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsumedSides(SideSet);

impl ConsumedSides {
    /// Nothing was consumed. Used when classifying a pre-open snapshot.
    pub const fn none() -> Self {
        Self(SideSet::EMPTY)
    }

    /// Whether the consumption covers one side. Read-only: there is no set
    /// algebra here, on purpose.
    pub const fn covers(self, side: WalSide) -> bool {
        match side {
            WalSide::Active => self.0.contains(SideSet::ACTIVE),
            WalSide::Frozen => self.0.contains(SideSet::FROZEN),
        }
    }

    /// Nothing was consumed. Read-only predicate used by the table tests.
    #[allow(dead_code)]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

/// Which sides a **successful** lenient open consumed, from the wiring's facts.
///
/// `WALReplayer::replayFrozenWAL` removes `<db>.wal.checkpoint` after either
/// replaying it to completion (`completeInterruptedCheckpoint`, which makes the
/// records durable) or reading nothing (`offsetDeserialized == 0`, which the
/// pre-replay scan reports as `Unknown`). Only the frozen side can be consumed
/// this way.
///
/// The active side is **never** consumed: `post == 0` on the active WAL means
/// the engine truncated it to zero without replaying (`replayActiveWAL`
/// truncates to `offsetDeserialized`, and only a commit moves that), so a
/// proven commit there is a loss, not a completed checkpoint. Returning it here
/// would turn that loss into a `Replayed` verdict.
pub const fn consumed_sides(
    pre: &WalPairBytes,
    post: &WalPairBytes,
    lenient_ok: bool,
) -> ConsumedSides {
    if !lenient_ok {
        return ConsumedSides::none();
    }
    if pre.frozen > 0 && post.frozen == 0 {
        ConsumedSides(SideSet::FROZEN)
    } else {
        ConsumedSides::none()
    }
}

/// What one side's pre-replay bytes prove about committed records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitEvidence {
    /// A complete `COMMIT_RECORD` frame (checksum-verified when the WAL has
    /// checksums).
    Some,
    /// A checksum-verified walk reached a sound end and held no commit.
    None,
    /// The WAL carries no checksums, so the walk cannot prove anything.
    Unverifiable,
    /// The bytes could not be attributed. Never treat this as `None`.
    Unknown,
}

/// What the engine reported about the replay it performed (`#228`).
///
/// The engine's `WALReplayInfo` carries `offsetDeserialized` and
/// `isLastRecordCheckpoint` — not an applied-record count — and the lbug
/// binding exposes neither (`neurostrata-5lam`). Until it does, the positive
/// evidence a *deleted* WAL can carry is an observed `CHECKPOINT` record or an
/// emulated applied-record count; this type models the missing signal so that
/// exposing it later is a wiring change, not a redesign.
///
/// [`emulate_applied_records`] derives the value the recovery funnel uses
/// today from what the failed open already proved about the WAL. The
/// `Unavailable` default stays the honest answer for every caller that has no
/// such fact: absence of a signal is not evidence of a replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedRecords {
    /// The engine confirmed that it applied at least one record.
    AtLeastOne,
    /// The engine confirmed that it applied no record.
    None,
    /// No engine signal is available.
    Unavailable,
}

/// The evidence that a WAL held committed or applied work (`#228`).
///
/// The recovery funnel emulates the engine's applied-record count, so the
/// evidence it emulates from must be evidence of **work**: a committed
/// transaction or an applied checkpoint. A frame that proves nothing — a lone
/// `BEGIN`, an incomplete frame — is not evidence, and the count it would
/// imply is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedEvidence {
    /// The walk **proved** a `COMMIT_RECORD`, or the WAL ended in a valid
    /// observed `CHECKPOINT` record: there was committed work the engine could
    /// apply, and the bytes were authenticated.
    CommittedWork,
    /// The walk **saw** a `COMMIT_RECORD` in a sound walk but could not
    /// authenticate it: a checksums-off WAL, whose frames carry no trailer to
    /// verify.
    ///
    /// The engine's replay reads such a WAL under the WAL header's own
    /// setting — no verification — and applies the commit it finds, so the
    /// frame is committed work even though the bytes cannot be proved intact.
    /// This is the checksum-config-mismatch case (`#191`): the records are
    /// intact and only the checksum *setting* was wrong, so the lenient
    /// replay drains the WAL and the store reopens.
    UnverifiedCommittedWork,
    /// Nothing proves committed or applied work: no commit frame was seen in a
    /// sound walk and no valid checkpoint was observed.
    NoCommittedWork,
}

/// The engine's applied-record count, **emulated** from the recovery facts
/// (`#228`).
///
/// The lbug binding exposes neither `WALReplayInfo::offsetDeserialized` nor
/// `isLastRecordCheckpoint` (`neurostrata-5lam`), so the recovery funnel
/// emulates the count from what the failed open already proved about the WAL:
///
/// * `record_failure` — the open failed on a **record** the engine cannot
///   deserialize ("Corrupted wal file", "invalid WAL record type"). The
///   lenient replay reads the same bytes and stops at the same record, so the
///   engine applies nothing and removes the frozen WAL ("Nothing was
///   committed, so the frozen WAL holds nothing to keep",
///   `wal_replayer.cpp:674-679`). The applied-record count is zero.
/// * otherwise — the failure was at the **header** (the checksum-config
///   mismatch) or in a torn **tail**. The records themselves are intact, so
///   the engine replays them under the WAL header's own checksum setting and
///   only then retires the file. The applied-record count is at least one
///   **iff** the WAL held committed work to apply.
///
/// `evidence` is that committed-work evidence ([`AppliedEvidence`]): a commit
/// frame in a sound walk, or a valid observed `CHECKPOINT` record. It is
/// never "the WAL held some frames": a lone `BEGIN` frame commits nothing,
/// so the engine applies nothing and retires the file with
/// `offsetDeserialized == 0`.
///
/// The emulation never raises the replay claim on a record failure, so a
/// removed frozen WAL whose records are broken is never reported as a replay.
pub const fn emulate_applied_records(
    record_failure: bool,
    evidence: AppliedEvidence,
) -> AppliedRecords {
    if record_failure || matches!(evidence, AppliedEvidence::NoCommittedWork) {
        AppliedRecords::None
    } else {
        AppliedRecords::AtLeastOne
    }
}

/// The applied-record signal for a removed WAL, from the side's own scan
/// (`#228`).
///
/// A side whose bytes could not be scanned at all proves nothing, so the
/// signal is `None` — absence of evidence is not evidence of a replay.
pub fn emulated_applied_records(record_failure: bool, scan: Option<WalScan>) -> AppliedRecords {
    emulate_applied_records(
        record_failure,
        scan.map(|scan| scan.committed_work_evidence())
            .unwrap_or(AppliedEvidence::NoCommittedWork),
    )
}

/// How a WAL file's frame walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalTail {
    /// The file ended exactly on a frame boundary.
    Clean,
    /// The final frame is a torn write whose type and trailer validate.
    TornTail,
    /// The final frame is torn but could not be validated (variable payload, or
    /// too few bytes to check the trailer).
    TornTailUnvalidated,
    /// A frame boundary, checksum, or header cannot be trusted.
    Corrupt,
}

/// The result of walking one WAL file's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalScan {
    pub header: HeaderRead,
    /// The header's own checksum matched (only meaningful when checksums are on).
    pub header_verified: bool,
    /// The file exceeded the scan cap and was not walked.
    pub oversized: bool,
    /// Complete frames walked before the stopping point.
    pub frames_parsed: u32,
    /// Complete frames whose trailer checksum matched.
    pub verified_frames: u32,
    /// Complete `COMMIT_RECORD` frames.
    pub commits: u32,
    /// The last complete frame's `WALRecordType`, if any.
    pub last_complete_type: Option<u8>,
    pub tail: WalTail,
}

impl WalScan {
    /// A file with no bytes.
    ///
    /// Test fixture and diagnostic value; the funnel gets its scans from
    /// [`scan_wal_with_cap`].
    #[allow(dead_code)]
    pub const EMPTY: Self = Self {
        header: HeaderRead::Absent,
        header_verified: false,
        oversized: false,
        frames_parsed: 0,
        verified_frames: 0,
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
    /// `None` requires a **checksum-verified** walk: a checksums-off WAL is
    /// `Unverifiable`, never `None`.
    pub const fn commit_evidence(self) -> CommitEvidence {
        match self.header {
            HeaderRead::Absent => CommitEvidence::None,
            HeaderRead::TooShort { .. } => CommitEvidence::Unknown,
            HeaderRead::Readable { checksums, .. } => {
                if self.commits > 0 {
                    return CommitEvidence::Some;
                }
                // A walk that could not be trusted at all is `Unknown`, whatever
                // the checksum setting: `Corrupt` must outrank the
                // "unverifiable" policy below.
                if self.oversized || matches!(self.tail, WalTail::Corrupt) {
                    return CommitEvidence::Unknown;
                }
                if !checksums {
                    // No trailer anywhere in the file: the walk cannot exclude a
                    // destroyed commit record.
                    return CommitEvidence::Unverifiable;
                }
                if !self.header_verified {
                    return CommitEvidence::Unknown;
                }
                match self.tail {
                    WalTail::Clean | WalTail::TornTail => CommitEvidence::None,
                    WalTail::TornTailUnvalidated => CommitEvidence::Unknown,
                    WalTail::Corrupt => CommitEvidence::Unknown,
                }
            }
        }
    }

    /// Whether this walk **proved** a committed record (`#228`).
    ///
    /// `commit_evidence` answers "was a commit frame seen?"; this answers the
    /// stronger question the recovery accounting needs: "did a *verified* walk
    /// prove that a commit existed?". It requires checksums in the file, a
    /// verified header, and every complete frame's trailer verified — so a
    /// checksums-off WAL never proves anything, and a frame whose trailer does
    /// not verify cannot be counted as proof either.
    pub const fn proved_commit(self) -> bool {
        self.commits > 0
            && self.header_verified
            && self.verified_frames == self.frames_parsed
            && !matches!(self.tail, WalTail::Corrupt)
            && matches!(
                self.header,
                HeaderRead::Readable {
                    checksums: true,
                    ..
                }
            )
    }

    /// Whether these bytes prove that the WAL held **work the engine could
    /// apply** (`#228`).
    ///
    /// This is the only evidence the applied-record emulation accepts:
    ///
    /// * a commit frame a sound walk **proved** ([`Self::proved_commit`]) — a
    ///   committed transaction the engine could apply; or
    /// * a valid **observed** `CHECKPOINT` record ([`Self::ended_in_checkpoint`])
    ///   — the engine applies that checkpoint and only then retires the file;
    /// * a commit frame a sound walk **saw** but could not authenticate — a
    ///   checksums-off WAL, whose frames carry no trailer to verify. The
    ///   engine's replay reads such a WAL under the WAL header's own setting
    ///   and applies the commit it finds, so the frame is committed work
    ///   (`#191`).
    ///
    /// It is deliberately *not* "the WAL held some frames": a lone `BEGIN`
    /// frame commits nothing, so the engine applies nothing and retires the
    /// file with `offsetDeserialized == 0`
    /// (`wal_replayer.cpp:604-700`). A frame walk that cannot be trusted
    /// proves nothing either: a walk that found a corrupt frame, a corrupt
    /// header, or a torn frame it could not validate has no committed work to
    /// offer, whatever frames it walked.
    pub const fn committed_work_evidence(self) -> AppliedEvidence {
        if self.proved_commit() || self.ended_in_checkpoint() {
            AppliedEvidence::CommittedWork
        } else if self.commits > 0 && self.sound_walk() {
            AppliedEvidence::UnverifiedCommittedWork
        } else {
            AppliedEvidence::NoCommittedWork
        }
    }

    /// Whether the walk itself can be trusted: no corrupt frame, no corrupt
    /// header, and nothing oversized.
    ///
    /// A checksums-off WAL has no trailer to authenticate its frames, so its
    /// walk is as sound as such a WAL can be: the frame boundaries and the
    /// record types were read, and nothing was found wrong with them.
    const fn sound_walk(self) -> bool {
        !self.oversized && !matches!(self.tail, WalTail::Corrupt)
    }
}

/// Walks one WAL file's frames with the default [`MAX_SCAN_BYTES`] cap.
///
/// Convenience for tests and tooling; the funnel reads a capped prefix and calls
/// [`scan_wal_with_cap`] so an oversized file is never read whole.
#[allow(dead_code)]
pub fn scan_wal(bytes: &[u8]) -> WalScan {
    scan_wal_with_cap(bytes, MAX_SCAN_BYTES)
}

/// Walks one WAL file's frames. `bytes` is the whole file (or the first
/// `cap + 1` bytes of it).
pub fn scan_wal_with_cap(bytes: &[u8], cap: u64) -> WalScan {
    let header = read_header(bytes);
    let (checksums, header_bytes) = match header {
        HeaderRead::Readable {
            checksums,
            header_bytes,
        } => (checksums, header_bytes),
        HeaderRead::Absent | HeaderRead::TooShort { .. } => {
            return WalScan {
                header,
                header_verified: false,
                oversized: false,
                frames_parsed: 0,
                verified_frames: 0,
                commits: 0,
                last_complete_type: None,
                tail: WalTail::Clean,
            };
        }
    };
    let mut scan = WalScan {
        header,
        header_verified: false,
        oversized: (bytes.len() as u64) > cap,
        frames_parsed: 0,
        verified_frames: 0,
        commits: 0,
        last_complete_type: None,
        tail: WalTail::Clean,
    };
    if scan.oversized {
        scan.tail = WalTail::Corrupt;
        return scan;
    }
    if checksums {
        // `WAL::writeHeader` is an object on the `ChecksumWriter`, so the header
        // payload is followed by `u64 checksum` at offset 17.
        if read_u64(bytes, 17) != wal_checksum(&bytes[..17]) {
            scan.tail = WalTail::Corrupt;
            return scan;
        }
        scan.header_verified = true;
    }

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
        let payload_end = cursor + 8 + record_length as usize;
        if checksums {
            // `ChecksumWriter::onObjectEnd`: the trailer covers `u64 length ‖ payload`.
            if read_u64(bytes, payload_end) != wal_checksum(&bytes[cursor..payload_end]) {
                scan.tail = WalTail::Corrupt;
                return scan;
            }
            scan.verified_frames = scan.verified_frames.saturating_add(1);
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
    u64::from_ne_bytes(buf)
}

/// Why a side's accounting could not reach a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// The bytes could not be attributed (bad framing, checksum, or header).
    CommitEvidenceUnreadable,
    /// The WAL carries no checksums, so "nothing committed" is not provable.
    UnverifiableWal,
}

impl UnknownReason {
    /// Which reason a pair-level `Unknown` reports when both sides are unknown.
    /// Order-independent so the fold stays commutative.
    const fn severity(self) -> u8 {
        match self {
            UnknownReason::UnverifiableWal => 1,
            UnknownReason::CommitEvidenceUnreadable => 2,
        }
    }
}

/// What one WAL side did across the recovery open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideVerdict {
    /// Nothing before, nothing after.
    NoWal,
    /// Nothing before, bytes after: the open created a WAL header. Report.
    FreshWal { bytes: u64 },
    /// Bytes before, nothing after, no committed record among them. Lossless.
    NothingCommitted { uncommitted_bytes: u64 },
    /// A committed record existed and no bytes survive on this side.
    CommittedLost { pre_bytes: u64 },
    /// The side belongs to another database (the engine's `verifyDatabaseID`
    /// refused it). Arm D's contract: rename aside and report, do not refuse.
    ForeignWal { pre_bytes: u64 },
    /// The side is too large to classify before the open. Defer to the verified
    /// open; accounting is only needed if that open fails.
    Deferred {
        pre_bytes: u64,
        reason: DeferralReason,
    },
    /// Bytes survive, or the engine consumed a completed checkpoint.
    Replayed {
        pre_bytes: u64,
        residual_bytes: u64,
        truncated_bytes: u64,
        checkpoint: Option<CheckpointEvidence>,
    },
    /// Refuse or report; never silently accept.
    Unknown { reason: UnknownReason },
}

/// Why a side could not be classified before the open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferralReason {
    /// The file exceeds [`MAX_SCAN_BYTES`].
    ScanTooLarge,
}

/// How the "this side ended in a checkpoint" claim is supported.
///
/// The two are not interchangeable: `Observed` is a complete `CHECKPOINT` record
/// at the end of the WAL, while `InferredByDeletion` is the weaker fact that
/// the engine removed the frozen WAL after the WAL's records were replayed.
/// Operator text must not call the latter an observed checkpoint, and it must
/// not claim that a *successful replay* consumed the file: the engine also
/// removes a frozen WAL whose replay never applied anything (`#228`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointEvidence {
    /// The scan saw a complete `CHECKPOINT_RECORD` as the last record.
    Observed,
    /// The engine removed the frozen WAL after applying **at least one** of its
    /// records; no `CHECKPOINT` record was seen.
    ///
    /// The applied-record count is *emulated* from the recovery facts (`#228`),
    /// because the lbug binding exposes neither
    /// `WALReplayInfo::offsetDeserialized` nor `isLastRecordCheckpoint`
    /// (`neurostrata-5lam`). The emulation only claims an applied record when
    /// the WAL held **committed work** — a proved commit, a commit frame a
    /// sound walk saw but could not authenticate (a checksums-off WAL), or an
    /// observed `CHECKPOINT` — and never on a record-level failure. So this
    /// evidence says "the engine applied at least one record", not "the engine
    /// replayed the whole WAL".
    InferredByDeletion,
}

/// Per-side inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideObservation {
    /// Bytes on this side before the recovery open (`0` when absent or empty).
    pub pre: u64,
    /// Bytes of this side actually handed to the scanner (`min(pre, cap + 1)`).
    /// Reported so an operator can see that an oversized file was not read.
    pub bytes_scanned: u64,
    /// Bytes on this side after the open and **before** any `CHECKPOINT`.
    pub post: u64,
    /// The pre-replay scan, required when `pre > 0`.
    pub scan: Option<WalScan>,
    /// The engine rejected this side as belonging to another database.
    ///
    /// Supplied by the arm from the `verifyDatabaseID` failure
    /// (`storage/file_db_id_utils.cpp:4-17`); the pure module deliberately does
    /// not re-derive it from the main DB header.
    pub foreign: bool,
}

impl SideObservation {
    pub const fn empty() -> Self {
        Self {
            pre: 0,
            bytes_scanned: 0,
            post: 0,
            scan: None,
            foreign: false,
        }
    }
}

/// Both sides, measured the same way, plus the consumption fact.
///
/// `consumed` is private so the only way to set it is [`Self::consumed`], which
/// takes the sealed [`ConsumedSides`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalPairObservation {
    pub active: SideObservation,
    pub frozen: SideObservation,
    consumed: ConsumedSides,
}

impl WalPairObservation {
    pub const fn new(active: SideObservation, frozen: SideObservation) -> Self {
        Self {
            active,
            frozen,
            consumed: ConsumedSides::none(),
        }
    }

    /// Attaches which sides a successful lenient open consumed.
    pub const fn consumed(mut self, consumed: ConsumedSides) -> Self {
        self.consumed = consumed;
        self
    }
}

/// One side's verdict, with **no** consumption. The only way to mark a side
/// consumed is [`WalPairObservation::consumed`] plus [`classify_pair`]; this
/// entry exists for the per-side table tests and future single-side callers.
#[allow(dead_code)]
pub fn classify_side(observation: SideObservation) -> SideVerdict {
    classify_side_with(observation, false, AppliedRecords::Unavailable)
}

/// Maps the accounting's commit evidence onto the formal kernel's closed
/// input space (`#228`).
///
/// The kernel owns the decision, so the funnel must hand it the same four
/// cases the Lean parity rows enumerate.
fn kernel_commit_evidence(
    evidence: CommitEvidence,
) -> crate::formal_kernel::KernelWalCommitEvidence {
    use crate::formal_kernel::KernelWalCommitEvidence;
    match evidence {
        CommitEvidence::Some => KernelWalCommitEvidence::CommitSeen,
        CommitEvidence::None => KernelWalCommitEvidence::NoCommit,
        CommitEvidence::Unverifiable => KernelWalCommitEvidence::Unverifiable,
        CommitEvidence::Unknown => KernelWalCommitEvidence::Unreadable,
    }
}

/// Classifies one side.
///
/// `applied` is the engine's replay result (`#228`): it is the only signal
/// that can make a *removed* WAL a replay, and it is
/// [`AppliedRecords::Unavailable`] until the binding exposes the engine's
/// replay result (`neurostrata-5lam`).
fn classify_side_with(
    observation: SideObservation,
    consumed_by_engine: bool,
    applied: AppliedRecords,
) -> SideVerdict {
    let SideObservation {
        pre,
        bytes_scanned: _,
        post,
        scan,
        foreign,
    } = observation;
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
    // A scan of zero bytes cannot account for `pre > 0`: the caller must scan
    // the whole side. Without this, `Some(WalScan::EMPTY)` would claim
    // "nothing committed" for bytes that were never looked at.
    if matches!(scan.header, HeaderRead::Absent) {
        return SideVerdict::Unknown {
            reason: UnknownReason::CommitEvidenceUnreadable,
        };
    }
    if scan.oversized {
        return SideVerdict::Deferred {
            pre_bytes: pre,
            reason: DeferralReason::ScanTooLarge,
        };
    }
    let evidence = scan.commit_evidence();
    // A foreign attribution is only safe when the bytes are attributable. A
    // one-byte flip in the 16-byte database id of this store's own WAL trips
    // `verifyDatabaseID` too, and quarantining that would discard this store's
    // data with a "belongs to another database" message. Unattributable bytes
    // are refused as `Unknown` instead, with the preserved copy named.
    if foreign && !matches!(evidence, CommitEvidence::Unknown) {
        return SideVerdict::ForeignWal { pre_bytes: pre };
    }
    let checkpoint = if scan.ended_in_checkpoint() {
        Some(CheckpointEvidence::Observed)
    } else {
        None
    };
    // A side the engine removed is a replay only on **positive evidence**
    // (`#228`).
    //
    // The old rule trusted the *successful open*: a frozen WAL the engine
    // removed was reported as "consumed by a successful replay". That is
    // wrong, because the engine removes a frozen WAL whose replay stopped
    // before its first record — the semantic early failure of an unknown
    // table (`wal_replayer.cpp:674-679`: "Nothing was committed, so the
    // frozen WAL holds nothing to keep"). The open succeeds, the committed
    // records are gone, and nothing was applied.
    //
    // Positive evidence is either an observed `CHECKPOINT` record (the engine
    // applies that checkpoint and only then removes the file) or an
    // engine-confirmed applied-record count. With neither, the file is gone
    // and the open's success proves nothing: committed records the pre-delete
    // scan proved are **lost**, and bytes that cannot be attributed are
    // **unknown**.
    if consumed_by_engine && post == 0 {
        // `#228`: the removed-WAL decision lives in the formal kernel, so the
        // recovery funnel and the Lean parity rows share one truth table
        // instead of two that can drift apart.
        let decision = crate::formal_kernel::evaluate_removed_wal_verdict(
            kernel_commit_evidence(evidence),
            scan.proved_commit(),
            checkpoint.is_some(),
            matches!(applied, AppliedRecords::AtLeastOne),
        );
        return match decision {
            crate::formal_kernel::KernelWalVerdict::ReplayedObserved => SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: 0,
                truncated_bytes: pre,
                checkpoint: Some(CheckpointEvidence::Observed),
            },
            crate::formal_kernel::KernelWalVerdict::ReplayedInferred => SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: 0,
                truncated_bytes: pre,
                checkpoint: Some(CheckpointEvidence::InferredByDeletion),
            },
            crate::formal_kernel::KernelWalVerdict::CommittedLost => {
                SideVerdict::CommittedLost { pre_bytes: pre }
            }
            crate::formal_kernel::KernelWalVerdict::UnknownUnverifiable => SideVerdict::Unknown {
                reason: UnknownReason::UnverifiableWal,
            },
            crate::formal_kernel::KernelWalVerdict::UnknownUnreadable => SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            },
            crate::formal_kernel::KernelWalVerdict::NothingCommitted => {
                SideVerdict::NothingCommitted {
                    uncommitted_bytes: pre,
                }
            }
        };
    }
    match (evidence, post) {
        (CommitEvidence::Unknown, _) => SideVerdict::Unknown {
            reason: UnknownReason::CommitEvidenceUnreadable,
        },
        // A checksums-off WAL cannot prove "nothing committed" once its bytes
        // are gone: refuse and preserve.
        (CommitEvidence::Unverifiable, 0) => SideVerdict::Unknown {
            reason: UnknownReason::UnverifiableWal,
        },
        (CommitEvidence::None, 0) => SideVerdict::NothingCommitted {
            uncommitted_bytes: pre,
        },
        // `Some` with no observed checkpoint is a loss, never a replay.
        (CommitEvidence::Some, 0) if checkpoint.is_none() => {
            SideVerdict::CommittedLost { pre_bytes: pre }
        }
        _ => SideVerdict::Replayed {
            pre_bytes: pre,
            residual_bytes: post,
            truncated_bytes: pre.saturating_sub(post),
            checkpoint,
        },
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
    /// A side belongs to another database. Arm D's contract: quarantine it and
    /// reopen as `DiscardedUnreadableWal`.
    ForeignWal { pre_bytes: u64, sides: SideSet },
    /// At least one side is too large to classify. Run the verified open; do not
    /// checkpoint on the strength of this verdict.
    Deferred { pre_bytes: u64, sides: SideSet },
    /// Bytes survived, or the engine consumed a completed checkpoint. Proceed;
    /// report what changed. `pre_bytes == 0` with `residual_bytes > 0` is a fresh
    /// WAL header created by the open.
    Replayed {
        pre_bytes: u64,
        residual_bytes: u64,
        truncated_bytes: u64,
        checkpoint: Option<CheckpointEvidence>,
    },
    /// Refuse or report; never silently accept.
    Unknown {
        reason: UnknownReason,
        sides: SideSet,
    },
}

/// The stronger of two checkpoint claims: an observed record beats a deletion
/// inference, which beats nothing.
const fn strongest_checkpoint(
    a: Option<CheckpointEvidence>,
    b: Option<CheckpointEvidence>,
) -> Option<CheckpointEvidence> {
    match (a, b) {
        (Some(CheckpointEvidence::Observed), _) | (_, Some(CheckpointEvidence::Observed)) => {
            Some(CheckpointEvidence::Observed)
        }
        (Some(CheckpointEvidence::InferredByDeletion), _)
        | (_, Some(CheckpointEvidence::InferredByDeletion)) => {
            Some(CheckpointEvidence::InferredByDeletion)
        }
        (None, None) => None,
    }
}

/// Folds the two side verdicts into one accounting verdict.
///
/// A commutative join: `CommittedLost > Unknown > ForeignWal > Deferred >
/// Replayed > NothingCommitted > NoWal`. `NoWal` is the identity.
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
    let mut unknown_reason: Option<UnknownReason> = None;
    for (side, verdict) in sides {
        if let SideVerdict::Unknown { reason } = verdict {
            unknown_sides = unknown_sides.union(side);
            // Keep the most severe reason; the comparison is order-independent
            // so swapping the sides cannot change the verdict.
            unknown_reason = Some(match unknown_reason {
                Some(current) if current.severity() >= reason.severity() => current,
                _ => reason,
            });
        }
    }
    if let Some(reason) = unknown_reason {
        return WalAccounting::Unknown {
            reason,
            sides: unknown_sides,
        };
    }

    let mut foreign_sides = SideSet::EMPTY;
    let mut foreign_bytes = 0u64;
    for (side, verdict) in sides {
        if let SideVerdict::ForeignWal { pre_bytes } = verdict {
            foreign_sides = foreign_sides.union(side);
            foreign_bytes = foreign_bytes.saturating_add(pre_bytes);
        }
    }
    if !foreign_sides.is_empty() {
        return WalAccounting::ForeignWal {
            pre_bytes: foreign_bytes,
            sides: foreign_sides,
        };
    }

    let mut deferred_sides = SideSet::EMPTY;
    let mut deferred_bytes = 0u64;
    for (side, verdict) in sides {
        if let SideVerdict::Deferred { pre_bytes, .. } = verdict {
            deferred_sides = deferred_sides.union(side);
            deferred_bytes = deferred_bytes.saturating_add(pre_bytes);
        }
    }
    if !deferred_sides.is_empty() {
        return WalAccounting::Deferred {
            pre_bytes: deferred_bytes,
            sides: deferred_sides,
        };
    }

    let mut pre_bytes = 0u64;
    let mut residual_bytes = 0u64;
    let mut truncated_bytes = 0u64;
    let mut checkpoint: Option<CheckpointEvidence> = None;
    let mut moved = false;
    for (_, verdict) in sides {
        match verdict {
            SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: residual,
                truncated_bytes: truncated,
                checkpoint: side_checkpoint,
            } => {
                moved = true;
                pre_bytes = pre_bytes.saturating_add(pre);
                residual_bytes = residual_bytes.saturating_add(residual);
                truncated_bytes = truncated_bytes.saturating_add(truncated);
                // An observed record outranks the deletion inference.
                checkpoint = strongest_checkpoint(checkpoint, side_checkpoint);
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
            checkpoint,
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

/// Measures both sides and folds them, applying the pair's consumption fact.
///
/// The applied-record signal defaults to [`AppliedRecords::Unavailable`], so a
/// removed WAL is a replay only on an observed `CHECKPOINT` record. The
/// recovery funnel passes the emulated count from [`emulate_applied_records`]
/// instead (`#228`).
pub fn classify_pair(pair: WalPairObservation) -> WalAccounting {
    classify_pair_with_applied(pair, AppliedRecords::Unavailable)
}

/// Measures both sides and folds them with the engine's replay result.
pub fn classify_pair_with_applied(
    pair: WalPairObservation,
    applied: AppliedRecords,
) -> WalAccounting {
    fold_side_verdicts(
        classify_side_with(pair.active, pair.consumed.covers(WalSide::Active), applied),
        classify_side_with(pair.frozen, pair.consumed.covers(WalSide::Frozen), applied),
    )
}

/// Measures both sides **after a failed open** (`#228`).
///
/// A side that lost every byte is a WAL the engine removed while the open was
/// failing, so its verdict comes from the removed-WAL truth table in the
/// formal kernel ([`crate::formal_kernel::evaluate_removed_wal_verdict`]) —
/// the same table the Lean parity rows enumerate. A side that kept its bytes
/// cannot be a loss: the failed open left them on disk.
///
/// `applied` is the applied-record signal. A failed open reports none, so the
/// caller passes [`AppliedRecords::Unavailable`]: the absence of a signal must
/// never be read as a replay.
pub fn classify_pair_after_failed_open(
    pair: WalPairObservation,
    applied: AppliedRecords,
) -> WalAccounting {
    let active_removed = pair.active.pre > 0 && pair.active.post == 0;
    let frozen_removed = pair.frozen.pre > 0 && pair.frozen.post == 0;
    fold_side_verdicts(
        classify_side_with(pair.active, active_removed, applied),
        classify_side_with(pair.frozen, frozen_removed, applied),
    )
}

/// What a refusing or quarantining arm must do with the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The recovery may `CHECKPOINT`/reopen.
    Proceed,
    /// Arm D: rename the named sides aside to `.corrupted.<ts>` and reopen with
    /// `OpenProvenance::DiscardedUnreadableWal`. No `CHECKPOINT`.
    Quarantine { sides: SideSet },
    /// Too large to classify before the open: run the verified open as today.
    /// Accounting is only needed if that open fails, and a failed open may not
    /// `CHECKPOINT` on an unaccounted WAL.
    DeferToVerifiedOpen { sides: SideSet },
    /// Refuse before any `CHECKPOINT`, and keep both WALs. `restore` names the
    /// sides that lost **all** of their bytes and may therefore be restored from
    /// the preserved copy; a side with residual bytes must not be restored over
    /// the engine's truncation.
    Refuse { restore: SideSet },
}

/// Maps a verdict plus the observations to what the arm may do.
pub fn disposition(verdict: WalAccounting, pair: WalPairObservation) -> Disposition {
    match verdict {
        WalAccounting::ForeignWal { sides, .. } => Disposition::Quarantine { sides },
        WalAccounting::Deferred { sides, .. } => Disposition::DeferToVerifiedOpen { sides },
        WalAccounting::CommittedLost { .. } | WalAccounting::Unknown { .. } => {
            let mut restore = SideSet::EMPTY;
            if pair.active.pre > 0 && pair.active.post == 0 {
                restore = restore.union(SideSet::ACTIVE);
            }
            if pair.frozen.pre > 0 && pair.frozen.post == 0 {
                restore = restore.union(SideSet::FROZEN);
            }
            Disposition::Refuse { restore }
        }
        WalAccounting::NoWal
        | WalAccounting::NothingCommitted { .. }
        | WalAccounting::Replayed { .. } => Disposition::Proceed,
    }
}

impl WalAccounting {
    /// A stable machine-readable slug for logs and recovery telemetry.
    pub const fn slug(self) -> &'static str {
        match self {
            WalAccounting::NoWal => "no_wal",
            WalAccounting::NothingCommitted { .. } => "nothing_committed",
            WalAccounting::CommittedLost { .. } => "committed_lost",
            WalAccounting::ForeignWal { .. } => "foreign_wal",
            WalAccounting::Deferred { .. } => "deferred",
            WalAccounting::Replayed { .. } => "replayed",
            WalAccounting::Unknown { .. } => "unknown",
        }
    }

    /// Operator text for how the checkpoint claim is supported. `None` when the
    /// verdict carries no checkpoint claim at all.
    pub const fn checkpoint_label(self) -> Option<&'static str> {
        match self {
            WalAccounting::Replayed {
                checkpoint: Some(CheckpointEvidence::Observed),
                ..
            } => Some("a completed CHECKPOINT record was observed"),
            WalAccounting::Replayed {
                checkpoint: Some(CheckpointEvidence::InferredByDeletion),
                ..
            } => Some(
                "the engine applied at least one record of the frozen WAL and then removed the file \
                 (the applied-record count is emulated from the recovery facts: the engine does \
                 not report it)",
            ),
            WalAccounting::Replayed {
                pre_bytes,
                checkpoint: None,
                ..
            } if pre_bytes > 0 => {
                Some("the write-ahead log was replayed and its surviving bytes are still on disk")
            }
            _ => None,
        }
    }

    /// Whether the arm may continue to its reopen. `ForeignWal` proceeds through
    /// arm D's quarantine rather than a plain `CHECKPOINT`; callers must use
    /// [`disposition`] to choose.
    ///
    /// The funnel switches on [`Disposition`], which also separates `ForeignWal`
    /// (quarantine) and `Deferred` (no checkpoint); this stays as the predicate
    /// the table tests and future arms assert against.
    #[allow(dead_code)]
    pub const fn may_proceed(self) -> bool {
        matches!(
            self,
            WalAccounting::NoWal
                | WalAccounting::NothingCommitted { .. }
                | WalAccounting::ForeignWal { .. }
                | WalAccounting::Replayed { .. }
        )
    }

    /// Which sides the verdict is about.
    pub const fn sides(self) -> SideSet {
        match self {
            WalAccounting::CommittedLost { sides, .. }
            | WalAccounting::ForeignWal { sides, .. }
            | WalAccounting::Deferred { sides, .. }
            | WalAccounting::Unknown { sides, .. }
            | WalAccounting::NothingCommitted { sides, .. } => sides,
            WalAccounting::NoWal | WalAccounting::Replayed { .. } => SideSet::BOTH,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fixtures ----------------------------------------------------------

    /// A WAL header as `WAL::writeHeader` lays it out, checksum included.
    fn header(checksums: bool) -> Vec<u8> {
        let mut out = vec![0x2au8; 16]; // 16-byte database id
        out.push(u8::from(checksums));
        if checksums {
            let crc = wal_checksum(&out);
            out.extend_from_slice(&crc.to_ne_bytes());
        }
        out
    }

    /// One `serializeWithLength` frame with a real checksum trailer, matching
    /// `WAL::addNewWALRecordNoLock`.
    fn record(record_type: u8, owner: &str, checksums: bool) -> Vec<u8> {
        let fixed = fixed_prefix_len(record_type).unwrap_or(MIN_RECORD_LEN);
        let mut payload = vec![record_type];
        payload.extend(std::iter::repeat(0u8).take((fixed - MIN_RECORD_LEN) as usize));
        payload.extend_from_slice(&(owner.len() as u64).to_ne_bytes());
        payload.extend_from_slice(owner.as_bytes());
        let mut out = (payload.len() as u64).to_ne_bytes().to_vec();
        out.extend_from_slice(&payload);
        if checksums {
            let crc = wal_checksum(&out);
            out.extend_from_slice(&crc.to_ne_bytes());
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

    /// `(length offset, type offset)` for every frame of a well-formed fixture.
    fn frame_offsets(bytes: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut cursor = match read_header(bytes) {
            HeaderRead::Readable { header_bytes, .. } => header_bytes as usize,
            _ => return out,
        };
        while cursor + 8 <= bytes.len() {
            let record_length = read_u64(bytes, cursor) as usize;
            if cursor + 8 + record_length > bytes.len() {
                break;
            }
            out.push((cursor, cursor + 8));
            let trailer = if matches!(
                read_header(bytes),
                HeaderRead::Readable {
                    checksums: true,
                    ..
                }
            ) {
                8
            } else {
                0
            };
            cursor += 8 + record_length + trailer;
        }
        out
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
        scan_wal(&truncate(&wal(true, &[1, 30, 2]), 2))
    }

    fn scan_torn_unvalidated() -> WalScan {
        let mut bytes = wal(true, &[1]);
        bytes.extend_from_slice(&record(30, &"x".repeat(40), true));
        scan_wal(&truncate(&bytes, 5))
    }

    fn scan_unattributable() -> WalScan {
        let mut bytes = header(true);
        bytes.extend(std::iter::repeat(0xFFu8).take(512));
        scan_wal(&bytes)
    }

    fn scan_oversized() -> WalScan {
        scan_wal_with_cap(&wal(true, &[1, 2]), 8)
    }

    fn scan_checksums_off_no_commit() -> WalScan {
        scan_wal(&wal(false, &[1]))
    }

    fn scan_checksums_off_commit() -> WalScan {
        scan_wal(&wal(false, &[1, 2]))
    }

    fn side(pre: u64, post: u64, scan: Option<WalScan>) -> SideObservation {
        SideObservation {
            pre,
            bytes_scanned: pre.min(MAX_SCAN_BYTES + 1),
            post,
            scan,
            foreign: false,
        }
    }

    /// A foreign WAL the scan can still attribute (readable committed records).
    fn foreign_side(pre: u64, post: u64) -> SideObservation {
        SideObservation {
            pre,
            bytes_scanned: pre.min(MAX_SCAN_BYTES + 1),
            post,
            scan: Some(scan_checksums_off_commit()),
            foreign: true,
        }
    }

    /// A foreign attribution whose bytes cannot be attributed: a flipped
    /// database id on an otherwise unreadable WAL looks exactly like this.
    fn unattributable_foreign_side(pre: u64, post: u64) -> SideObservation {
        SideObservation {
            pre,
            bytes_scanned: pre.min(MAX_SCAN_BYTES + 1),
            post,
            scan: Some(scan_unattributable()),
            foreign: true,
        }
    }

    /// The pair entry the wiring uses: a frozen side consumed by a successful
    /// open, built through the sealed [`ConsumedSides`].
    fn frozen_consumed_verdict(pre: u64, post: u64, scan: Option<WalScan>) -> WalAccounting {
        let pair = WalPairObservation::new(SideObservation::empty(), side(pre, post, scan))
            .consumed(consumed_sides(
                &WalPairBytes {
                    active: 0,
                    frozen: pre,
                },
                &WalPairBytes {
                    active: 0,
                    frozen: post,
                },
                true,
            ));
        classify_pair(pair)
    }

    /// `#228` IMPORTANT 1: after a **failed** open, a side that lost every
    /// byte is a WAL the engine removed, so its verdict comes from the
    /// removed-WAL truth table; a side that kept its bytes cannot be a loss.
    #[test]
    fn a_failed_open_classifies_a_removed_side_as_a_loss_and_a_surviving_side_as_not() {
        // The frozen side lost every byte and its walk proves a commit.
        let removed = classify_pair_after_failed_open(
            WalPairObservation::new(
                SideObservation::empty(),
                side(942, 0, Some(scan_with_commit())),
            ),
            AppliedRecords::Unavailable,
        );
        assert_eq!(
            removed,
            WalAccounting::CommittedLost {
                pre_bytes: 942,
                sides: SideSet::FROZEN
            }
        );
        assert!(!removed.may_proceed(), "a lost commit must never proceed");

        // The same bytes, still on disk: the failed open left them, so no loss
        // is proven.
        let surviving = classify_pair_after_failed_open(
            WalPairObservation::new(
                SideObservation::empty(),
                side(942, 942, Some(scan_with_commit())),
            ),
            AppliedRecords::Unavailable,
        );
        assert!(
            !matches!(surviving, WalAccounting::CommittedLost { .. }),
            "a surviving side is never a loss: {surviving:?}"
        );

        // A removed side that committed nothing lost nothing.
        let no_commit = classify_pair_after_failed_open(
            WalPairObservation::new(
                SideObservation::empty(),
                side(942, 0, Some(scan_wal(&wal(true, &[1])))),
            ),
            AppliedRecords::Unavailable,
        );
        assert!(
            matches!(no_commit, WalAccounting::NothingCommitted { .. }),
            "a lone BEGIN frame commits nothing, so nothing is lost: {no_commit:?}"
        );
    }

    // ---- checksum port -----------------------------------------------------

    #[test]
    fn checksum_of_an_empty_buffer_is_the_dbs_seed() {
        // `common/checksum.cpp:73-76`: `result = 5381`, no blocks, no remainder.
        assert_eq!(wal_checksum(&[]), 5381);
    }

    #[test]
    fn checksum_mixes_blocks_and_the_trailing_bytes() {
        // Eight zero bytes hash to the finalizer of 0, which is 0.
        assert_eq!(wal_checksum(&[0u8; 8]), 5381);
        assert_eq!(wal_checksum(&[0u8; 16]), 5381);
        // One non-zero byte goes through the remainder path only.
        assert_ne!(wal_checksum(&[1u8]), wal_checksum(&[2u8]));
        assert_ne!(wal_checksum(&[0u8; 9]), wal_checksum(&[0u8; 8]));
    }

    #[test]
    fn our_fixtures_are_checksum_valid_for_the_engine_framing() {
        // Guards the fixture builder: if it ever writes a trailer the scanner
        // cannot verify, every test below would silently pass through `Unknown`.
        let scan = scan_with_commit();
        assert!(scan.header_verified);
        assert_eq!(scan.frames_parsed, 2);
        assert_eq!(scan.verified_frames, 2);
        assert_eq!(scan.tail, WalTail::Clean);
    }

    // ---- scanner: framing, validation, checksums ---------------------------

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
        assert_eq!(scan.verified_frames, 3);
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
        assert_eq!(scan.verified_frames, 0, "no trailer to verify");
        assert_eq!(scan.commits, 2);
        assert_eq!(scan.tail, WalTail::Clean);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Some);
    }

    #[test]
    fn scan_accepts_a_seventeen_byte_checksums_off_wal() {
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
        // A checksums-off WAL cannot prove the absence of a commit.
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unverifiable);
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
    fn scan_refuses_a_header_whose_checksum_does_not_match() {
        let mut bytes = header(true);
        bytes[3] ^= 0x01;
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert!(!scan.header_verified);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_sees_a_torn_commit_as_no_commit_after_a_sound_prefix() {
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
    fn scan_rejects_a_frame_whose_checksum_does_not_match() {
        let mut bytes = wal(true, &[1, 2]);
        // Flip a bit in the BEGIN payload, inside the checksummed frame.
        let (_, type_offset) = frame_offsets(&bytes)[0];
        bytes[type_offset] ^= 0x10;
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_rejects_a_commit_whose_owner_trailer_does_not_match() {
        // Length 17 with ownerNameLen 0: `ownerLen != len - 9`, checksums off so
        // the trailer check is the only guard.
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
        let mut bytes = header(false);
        bytes.extend_from_slice(&9u64.to_ne_bytes());
        bytes.push(CHECKPOINT_RECORD_TYPE);
        bytes.extend_from_slice(&0u64.to_ne_bytes());
        let scan = scan_wal(&bytes);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn scan_calls_an_unknown_record_type_corrupt() {
        let scan = scan_wal(&wal(false, &[1, 0xAA]));
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

    #[test]
    fn scan_marks_a_wal_over_the_cap_as_oversized() {
        let bytes = wal(true, &[1, 2]);
        let scan = scan_wal_with_cap(&bytes, 32);
        assert!(scan.oversized);
        assert_eq!(scan.tail, WalTail::Corrupt);
        assert_eq!(scan.commit_evidence(), CommitEvidence::Unknown);
    }

    #[test]
    fn the_default_cap_has_headroom_over_the_engine_checkpoint_threshold() {
        // `include/main/database.h:81`: checkpointThreshold defaults to 16 MiB
        // with autoCheckpoint on, so a healthy crash routinely leaves a WAL at
        // or above 16 MiB. The cap must not sit below it.
        assert!(
            MAX_SCAN_BYTES >= 64 << 20,
            "cap {MAX_SCAN_BYTES} must leave headroom over the 16 MiB threshold"
        );
        assert!(MAX_SCAN_BYTES > 16 << 20);
    }

    #[test]
    fn a_seventeen_megabyte_wal_is_within_the_default_cap() {
        // Just over the engine's auto-checkpoint threshold: the walk is allowed
        // (it is refused as `Corrupt` only because the bytes are zeros, which is
        // irrelevant here - the assertion is that the cap did not fire).
        let bytes = vec![0u8; (17 << 20) as usize];
        let scan = scan_wal(&bytes);
        assert!(!scan.oversized, "17 MiB must be classifiable");
    }

    #[test]
    fn an_oversized_wal_defers_to_the_verified_open() {
        let scan = scan_oversized();
        assert!(scan.oversized);
        let pair = WalPairObservation::new(side(140, 140, Some(scan)), SideObservation::empty());
        let verdict = classify_pair(pair);
        assert_eq!(
            verdict,
            WalAccounting::Deferred {
                pre_bytes: 140,
                sides: SideSet::ACTIVE
            }
        );
        assert_eq!(
            disposition(verdict, pair),
            Disposition::DeferToVerifiedOpen {
                sides: SideSet::ACTIVE
            }
        );
        assert!(
            !verdict.may_proceed(),
            "a deferred WAL is not permission to CHECKPOINT"
        );
        assert_eq!(verdict.slug(), "deferred");
    }

    #[test]
    fn a_proven_loss_still_outranks_a_deferred_peer() {
        let pair = WalPairObservation::new(
            side(140, 140, Some(scan_oversized())),
            side(3103, 0, Some(scan_with_commit())),
        );
        assert!(matches!(
            classify_pair(pair),
            WalAccounting::CommittedLost { .. }
        ));
    }

    #[test]
    fn every_single_byte_mutation_of_a_length_or_type_byte_is_refused() {
        // B1: 255 masks over every length-prefix byte and every type byte of a
        // committed checksums-on fixture. A flip must never produce
        // `NothingCommitted` (the pre-checksum walk could be walked past the
        // real COMMIT; `0x73` on the first length byte was one such mask).
        let bytes = {
            let mut out = header(true);
            out.extend_from_slice(&record(BEGIN_TRANSACTION_RECORD_TYPE, "", true));
            out.extend_from_slice(&record(30, &"x".repeat(40), true));
            out.extend_from_slice(&record(COMMIT_RECORD_TYPE, "", true));
            out
        };
        assert_eq!(scan_wal(&bytes).commit_evidence(), CommitEvidence::Some);
        let offsets = frame_offsets(&bytes);
        assert_eq!(offsets.len(), 3);

        for (length_offset, type_offset) in offsets {
            for offset in (0..8).map(|i| length_offset + i).chain([type_offset]) {
                for mask in 1u8..=255 {
                    let mut mutated = bytes.clone();
                    mutated[offset] ^= mask;
                    let scan = scan_wal(&mutated);
                    assert_ne!(
                        scan.commit_evidence(),
                        CommitEvidence::None,
                        "mask 0x{mask:02x} at byte {offset} hid the commit: {:?}",
                        scan
                    );
                    assert_ne!(
                        scan.commit_evidence(),
                        CommitEvidence::Unverifiable,
                        "a checksums-on WAL must never become unverifiable"
                    );
                    let verdict = classify_side(side(140, 0, Some(scan)));
                    assert!(
                        !verdict_may_proceed(verdict),
                        "mask 0x{mask:02x} at byte {offset} produced {verdict:?}"
                    );
                    assert_ne!(
                        verdict,
                        SideVerdict::NothingCommitted {
                            uncommitted_bytes: 140
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn every_single_byte_mutation_of_a_checksums_off_wal_is_refused_or_kept_open() {
        // Without trailers the walk can be walked past a commit, so the model
        // must never *accept* on a checksums-off side with no surviving bytes:
        // the evidence is `Unverifiable`, never `None`.
        let bytes = {
            let mut out = header(false);
            out.extend_from_slice(&record(BEGIN_TRANSACTION_RECORD_TYPE, "", false));
            out.extend_from_slice(&record(30, &"x".repeat(40), false));
            out.extend_from_slice(&record(COMMIT_RECORD_TYPE, "", false));
            out
        };
        for (length_offset, type_offset) in frame_offsets(&bytes) {
            for offset in (0..8).map(|i| length_offset + i).chain([type_offset]) {
                for mask in 1u8..=255 {
                    let mut mutated = bytes.clone();
                    mutated[offset] ^= mask;
                    let scan = scan_wal(&mutated);
                    assert_ne!(
                        scan.commit_evidence(),
                        CommitEvidence::None,
                        "mask 0x{mask:02x} at byte {offset} claimed a verified empty \
                         checksums-off walk: {:?}",
                        scan
                    );
                }
            }
        }
    }

    fn verdict_may_proceed(verdict: SideVerdict) -> bool {
        !matches!(
            verdict,
            SideVerdict::CommittedLost { .. }
                | SideVerdict::Unknown { .. }
                | SideVerdict::Deferred { .. }
        )
    }

    // ---- per-side classification ------------------------------------------

    // ---- consumption (a successful open only ever consumes the frozen side)

    #[test]
    fn consumed_sides_marks_only_the_frozen_side() {
        let pre = WalPairBytes {
            active: 0,
            frozen: 942,
        };
        let post = WalPairBytes::ZERO;
        let consumed = consumed_sides(&pre, &post, true);
        assert!(consumed.covers(WalSide::Frozen));
        assert!(!consumed.covers(WalSide::Active));
        assert!(!consumed.is_empty());
    }

    #[test]
    fn consumed_sides_never_marks_the_active_side() {
        // The dangerous mutation: treating an active WAL truncated to zero as a
        // consumed checkpoint would accept a lost commit as a replay.
        let pre = WalPairBytes {
            active: 530,
            frozen: 0,
        };
        let post = WalPairBytes::ZERO;
        assert!(
            consumed_sides(&pre, &post, true).is_empty(),
            "the active side is never consumed"
        );

        let pre = WalPairBytes {
            active: 530,
            frozen: 942,
        };
        let post = WalPairBytes::ZERO;
        let consumed = consumed_sides(&pre, &post, true);
        assert!(
            consumed.covers(WalSide::Frozen) && !consumed.covers(WalSide::Active),
            "only the frozen side is consumed when both emptied"
        );
    }

    #[test]
    fn consumed_sides_requires_a_successful_open() {
        let pre = WalPairBytes {
            active: 0,
            frozen: 942,
        };
        let post = WalPairBytes::ZERO;
        assert!(consumed_sides(&pre, &post, false).is_empty());
    }

    #[test]
    fn consumed_sides_is_empty_for_an_empty_pair_or_a_residual_frozen_side() {
        assert!(consumed_sides(&WalPairBytes::ZERO, &WalPairBytes::ZERO, true).is_empty());
        let pre = WalPairBytes {
            active: 0,
            frozen: 942,
        };
        let post = WalPairBytes {
            active: 0,
            frozen: 400,
        };
        assert!(consumed_sides(&pre, &post, true).is_empty());
    }

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

    /// `#228`: a successful open is **not** evidence that the engine applied
    /// the frozen WAL. The engine removes a frozen WAL whose replay stopped
    /// before its first record (`wal_replayer.cpp:674-679`: "Nothing was
    /// committed, so the frozen WAL holds nothing to keep"), so a consumed
    /// frozen WAL whose scan proves a commit is a **loss**, not a replay.
    #[test]
    fn a_frozen_side_consumed_by_a_successful_open_is_not_a_replay() {
        let verdict = frozen_consumed_verdict(942, 0, Some(scan_with_commit()));
        assert_eq!(
            verdict,
            WalAccounting::CommittedLost {
                pre_bytes: 942,
                sides: SideSet::FROZEN
            }
        );
        assert!(!verdict.may_proceed(), "a lost commit must never proceed");
        assert_eq!(
            verdict.checkpoint_label(),
            None,
            "a loss carries no checkpoint claim"
        );
    }

    /// `#228` §6.1 — the truth table over the four inputs of the rule:
    /// `(consumed_by_engine, post_bytes, CommitEvidence, applied_records)`.
    ///
    /// A `Replayed` verdict for a *removed* frozen WAL requires positive
    /// evidence — an observed `CHECKPOINT` record or an engine-confirmed
    /// applied-record count. Without it the verdict is `CommittedLost` (a
    /// proven commit) or `Unknown` (bytes that cannot be attributed), never
    /// `Replayed`.
    #[test]
    fn the_removed_frozen_wal_truth_table() {
        let rows: [(&str, u64, WalScan, AppliedRecords, &str); 7] = [
            (
                "a removed WAL with a proven commit and no engine signal",
                0,
                scan_with_commit(),
                AppliedRecords::Unavailable,
                "committed_lost",
            ),
            (
                "a removed WAL with a proven commit and an engine-confirmed empty replay",
                0,
                scan_with_commit(),
                AppliedRecords::None,
                "committed_lost",
            ),
            (
                "a removed WAL with a proven commit and an applied-record count",
                0,
                scan_with_commit(),
                AppliedRecords::AtLeastOne,
                "replayed",
            ),
            (
                "a removed WAL with an observed CHECKPOINT record",
                0,
                scan_with_checkpoint(),
                AppliedRecords::Unavailable,
                "replayed",
            ),
            (
                "a removed WAL whose commit cannot be verified",
                0,
                scan_checksums_off_commit(),
                AppliedRecords::Unavailable,
                "unknown",
            ),
            (
                "a removed WAL that provably held no commit",
                0,
                scan_no_commit(),
                AppliedRecords::Unavailable,
                "nothing_committed",
            ),
            (
                "a surviving WAL with a proven commit",
                530,
                scan_with_commit(),
                AppliedRecords::Unavailable,
                "replayed",
            ),
        ];
        for (name, post, scan, applied, expected) in rows {
            let pair =
                WalPairObservation::new(SideObservation::empty(), side(942, post, Some(scan)))
                    .consumed(consumed_sides(
                        &WalPairBytes {
                            active: 0,
                            frozen: 942,
                        },
                        &WalPairBytes {
                            active: 0,
                            frozen: post,
                        },
                        true,
                    ));
            let verdict = classify_pair_with_applied(pair, applied);
            assert_eq!(verdict.slug(), expected, "{name}");
        }
    }

    /// `#228`: the lone-`BEGIN` truth row.
    ///
    /// A WAL whose only frame is a `BEGIN` commits nothing. The engine applies
    /// nothing and retires the file, so the removed WAL is *not* a replay: the
    /// truth row is `nothing_committed`, and the applied-record signal the
    /// funnel emulates for it is `None`.
    #[test]
    fn a_lone_begin_wal_is_never_a_replay() {
        let scan = scan_no_commit();
        let applied = emulated_applied_records(false, Some(scan));
        assert_eq!(
            applied,
            AppliedRecords::None,
            "a lone BEGIN frame emulates no applied record"
        );
        let pair = WalPairObservation::new(SideObservation::empty(), side(942, 0, Some(scan)))
            .consumed(consumed_sides(
                &WalPairBytes {
                    active: 0,
                    frozen: 942,
                },
                &WalPairBytes {
                    active: 0,
                    frozen: 0,
                },
                true,
            ));
        let verdict = classify_pair_with_applied(pair, applied);
        assert_eq!(
            verdict.slug(),
            "nothing_committed",
            "a lone BEGIN frame loses nothing and replays nothing"
        );
    }

    /// `#228`: the emulated applied-record count is what keeps a
    /// checksum-config mismatch from reading as a loss, and it must never read
    /// a record-level failure as a replay.
    #[test]
    fn the_emulated_applied_record_count_never_reads_a_record_failure_as_a_replay() {
        assert_eq!(
            emulate_applied_records(true, AppliedEvidence::CommittedWork),
            AppliedRecords::None,
            "a record-level failure replays nothing: the lenient replay stops at the same record"
        );
        assert_eq!(
            emulate_applied_records(false, AppliedEvidence::CommittedWork),
            AppliedRecords::AtLeastOne,
            "a header-level failure or a torn tail leaves the records intact"
        );
        assert_eq!(
            emulate_applied_records(false, AppliedEvidence::NoCommittedWork),
            AppliedRecords::None,
            "a WAL with no committed work applies nothing, whatever the failure was"
        );
        assert_eq!(
            emulate_applied_records(true, AppliedEvidence::NoCommittedWork),
            AppliedRecords::None,
            "a broken record never raises the replay claim"
        );
    }

    /// `#228`: the emulation is built on **committed work**, never on "the WAL
    /// held some frames".
    ///
    /// A lone `BEGIN` frame commits nothing, so the engine applies nothing and
    /// retires the file with `offsetDeserialized == 0`
    /// (`wal_replayer.cpp:604-700`). A frame walk that cannot be trusted
    /// proves nothing either.
    #[test]
    fn the_applied_record_emulation_needs_committed_work() {
        // The lone-BEGIN WAL: one `BEGIN` frame, no commit.
        let lone_begin = scan_no_commit();
        assert_eq!(
            lone_begin.frames_parsed, 1,
            "the fixture is a single BEGIN frame"
        );
        assert_eq!(
            lone_begin.commit_evidence(),
            CommitEvidence::None,
            "a lone BEGIN frame commits nothing"
        );
        assert_eq!(
            lone_begin.committed_work_evidence(),
            AppliedEvidence::NoCommittedWork,
            "a lone BEGIN frame is no evidence of committed work"
        );
        assert_eq!(
            emulated_applied_records(false, Some(lone_begin)),
            AppliedRecords::None,
            "a lone BEGIN frame must never emulate an applied record"
        );
        assert_eq!(
            emulated_applied_records(true, Some(lone_begin)),
            AppliedRecords::None,
            "a lone BEGIN frame applies nothing on a record failure either"
        );

        // A proven commit is committed work.
        assert_eq!(
            scan_with_commit().committed_work_evidence(),
            AppliedEvidence::CommittedWork,
            "a proven commit is committed work"
        );
        assert_eq!(
            emulated_applied_records(false, Some(scan_with_commit())),
            AppliedRecords::AtLeastOne,
            "a proven commit emulates an applied record"
        );

        // An observed CHECKPOINT record is committed work too: the engine
        // applies that checkpoint and only then retires the file.
        assert_eq!(
            scan_with_checkpoint().committed_work_evidence(),
            AppliedEvidence::CommittedWork,
            "an observed CHECKPOINT record is committed work"
        );

        // Bytes that could not be attributed prove nothing.
        assert_eq!(
            emulated_applied_records(false, Some(scan_unattributable())),
            AppliedRecords::None,
            "unattributable bytes are no evidence of committed work"
        );
        assert_eq!(
            emulated_applied_records(false, None),
            AppliedRecords::None,
            "a side that was never scanned is no evidence of committed work"
        );
    }

    /// `#228` IMPORTANT 2 with `#191`: a commit frame a sound walk **saw** but
    /// could not authenticate is committed work.
    ///
    /// A checksums-off WAL carries no trailer to verify, so its commit frame
    /// can never be *proved* — but the engine's replay reads such a WAL under
    /// the WAL header's own setting and applies the commit it finds. The
    /// checksum-config mismatch therefore drains the WAL and reopens the
    /// store (`#191`), and the emulation must say so.
    ///
    /// It is still committed evidence, not "the WAL held some frames": a lone
    /// `BEGIN` and a walk that found a corrupt frame both emulate nothing.
    #[test]
    fn an_unauthenticated_commit_frame_is_committed_work_for_the_emulation() {
        let scan = scan_checksums_off_commit();
        assert!(
            !scan.proved_commit(),
            "a checksums-off WAL proves nothing: it carries no trailer to verify"
        );
        assert_eq!(
            scan.committed_work_evidence(),
            AppliedEvidence::UnverifiedCommittedWork
        );
        assert_eq!(
            emulated_applied_records(false, Some(scan)),
            AppliedRecords::AtLeastOne,
            "the checksum-config mismatch drains the WAL and reopens the store (#191)"
        );
        assert_eq!(
            emulated_applied_records(true, Some(scan)),
            AppliedRecords::None,
            "a record-level failure still applies nothing"
        );

        // A checksums-off WAL with no commit applies nothing.
        assert_eq!(
            scan_checksums_off_no_commit().committed_work_evidence(),
            AppliedEvidence::NoCommittedWork
        );

        // A walk that found a corrupt frame proves nothing, whatever frames it
        // walked: the commit it saw cannot be trusted.
        let corrupt_tail = scan_wal(&wal(true, &[1, 2, 0xAA]));
        assert_eq!(corrupt_tail.commits, 1);
        assert_eq!(
            corrupt_tail.committed_work_evidence(),
            AppliedEvidence::NoCommittedWork
        );
    }

    /// `#228`: the operator text must never claim that a successful replay
    /// consumed the WAL. The only claim left for a removed WAL is the
    /// engine's own confirmation that it applied records.
    #[test]
    fn the_operator_text_never_claims_a_successful_replay() {
        let inferred = WalAccounting::Replayed {
            pre_bytes: 942,
            residual_bytes: 0,
            truncated_bytes: 942,
            checkpoint: Some(CheckpointEvidence::InferredByDeletion),
        };
        let label = inferred
            .checkpoint_label()
            .expect("an inferred deletion still carries a label");
        assert!(
            !label.contains("successful replay"),
            "the label must not claim a replay: {label}"
        );
        assert!(
            !label.contains("consumed by"),
            "the label must not claim the WAL was consumed by a replay: {label}"
        );

        let lost = WalAccounting::CommittedLost {
            pre_bytes: 942,
            sides: SideSet::FROZEN,
        };
        assert_eq!(lost.checkpoint_label(), None);
    }

    #[test]
    fn consumption_never_rescues_an_unattributable_scan() {
        // The `#207` probe: the engine read nothing and deleted the frozen WAL.
        // Consumption must not launder that into a replay.
        let verdict = frozen_consumed_verdict(3103, 0, Some(scan_unattributable()));
        assert!(matches!(verdict, WalAccounting::Unknown { .. }));
        // Nor a checksums-off WAL with no commit found: that is `Unverifiable`.
        let verdict = frozen_consumed_verdict(530, 0, Some(scan_checksums_off_no_commit()));
        assert_eq!(
            verdict,
            WalAccounting::Unknown {
                reason: UnknownReason::UnverifiableWal,
                sides: SideSet::FROZEN
            }
        );
    }

    #[test]
    fn consumption_does_not_apply_to_the_active_side() {
        // For the active WAL, `post == 0` means the engine truncated to zero
        // without replaying. `consumed_sides` is asked with an active-only
        // change and answers "nothing consumed", so the proven commit is a loss.
        let verdict = classify_pair(
            WalPairObservation::new(
                side(530, 0, Some(scan_with_commit())),
                SideObservation::empty(),
            )
            .consumed(consumed_sides(
                &WalPairBytes {
                    active: 530,
                    frozen: 0,
                },
                &WalPairBytes::ZERO,
                true,
            )),
        );
        assert_eq!(
            verdict,
            WalAccounting::CommittedLost {
                pre_bytes: 530,
                sides: SideSet::ACTIVE
            }
        );
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
                checkpoint: Some(CheckpointEvidence::Observed)
            }
        );
        let pair = WalPairObservation::new(
            SideObservation::empty(),
            side(3103, 0, Some(scan_with_checkpoint())),
        );
        assert_eq!(
            classify_pair(pair).checkpoint_label(),
            Some("a completed CHECKPOINT record was observed")
        );
    }

    #[test]
    fn a_checkpoint_observed_in_the_scan_outranks_a_deletion_inference() {
        // One side observed a CHECKPOINT record; the other was inferred by the
        // engine removing it. The pair reports the stronger claim.
        let observed = side(3103, 0, Some(scan_with_checkpoint()));
        let inferred = side(0, 0, None);
        let pair = WalPairObservation::new(observed, inferred);
        assert_eq!(
            classify_pair(pair).checkpoint_label(),
            Some("a completed CHECKPOINT record was observed")
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
                checkpoint: None
            }
        );
    }

    #[test]
    fn partial_active_truncation_is_a_replay_with_a_loss_report() {
        let verdict = classify_side(side(2530, 530, Some(scan_with_commit())));
        assert_eq!(
            verdict,
            SideVerdict::Replayed {
                pre_bytes: 2530,
                residual_bytes: 530,
                truncated_bytes: 2000,
                checkpoint: None
            }
        );
    }

    #[test]
    fn a_fresh_wal_is_reported_not_refused() {
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
                checkpoint: None
            }
        );
        assert!(verdict.may_proceed());
    }

    #[test]
    fn measuring_post_after_a_fresh_header_would_mask_a_loss() {
        // P2: the post snapshot must be taken immediately after `Database::new`.
        // If it is taken later, the fresh 25-byte header turns a proven loss
        // into a `Replayed` verdict.
        let lost = classify_side(side(3103, 0, Some(scan_with_commit())));
        let measured_late = classify_side(side(3103, 25, Some(scan_with_commit())));
        assert_eq!(lost, SideVerdict::CommittedLost { pre_bytes: 3103 });
        assert_eq!(
            measured_late,
            SideVerdict::Replayed {
                pre_bytes: 3103,
                residual_bytes: 25,
                truncated_bytes: 3078,
                checkpoint: None
            }
        );
        assert!(verdict_may_proceed(measured_late), "the hazard is real");
    }

    #[test]
    fn unknown_when_a_positive_pre_has_no_scan_header() {
        // A caller that scanned nothing cannot claim `pre` bytes held no commit.
        let verdict = classify_side(side(530, 0, Some(WalScan::EMPTY)));
        assert_eq!(
            verdict,
            SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
        );
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
    fn a_checksums_off_wal_with_no_surviving_bytes_is_unverifiable() {
        // B1.3 policy: no trailer means "nothing committed" is not provable.
        let verdict = classify_side(side(530, 0, Some(scan_checksums_off_no_commit())));
        assert_eq!(
            verdict,
            SideVerdict::Unknown {
                reason: UnknownReason::UnverifiableWal
            }
        );
    }

    #[test]
    fn a_checksums_off_wal_with_a_commit_is_still_a_proven_loss() {
        let verdict = classify_side(side(530, 0, Some(scan_checksums_off_commit())));
        assert_eq!(verdict, SideVerdict::CommittedLost { pre_bytes: 530 });
    }

    #[test]
    fn a_checksums_off_wal_with_residual_bytes_replays() {
        let verdict = classify_side(side(530, 400, Some(scan_checksums_off_no_commit())));
        assert!(matches!(verdict, SideVerdict::Replayed { .. }));
        assert!(verdict_may_proceed(verdict));
    }

    #[test]
    fn committed_lost_when_a_frozen_loss_is_masked_by_a_residual_active_wal() {
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

    // ---- B10: foreign WAL ----------------------------------------------

    #[test]
    fn a_foreign_wal_is_quarantined_not_refused() {
        // Arm D's contract: the donor WAL is renamed to `.corrupted.<ts>` and the
        // store reopens as `DiscardedUnreadableWal`.
        let pair = WalPairObservation::new(foreign_side(539, 0), SideObservation::empty());
        let verdict = classify_pair(pair);
        assert_eq!(
            verdict,
            WalAccounting::ForeignWal {
                pre_bytes: 539,
                sides: SideSet::ACTIVE
            }
        );
        assert!(verdict.may_proceed());
        assert_eq!(
            disposition(verdict, pair),
            Disposition::Quarantine {
                sides: SideSet::ACTIVE
            }
        );
    }

    #[test]
    fn a_foreign_pair_quarantines_both_sides() {
        let pair = WalPairObservation::new(foreign_side(539, 0), foreign_side(2048, 0));
        let verdict = classify_pair(pair);
        assert_eq!(
            verdict,
            WalAccounting::ForeignWal {
                pre_bytes: 2587,
                sides: SideSet::BOTH
            }
        );
        assert_eq!(
            disposition(verdict, pair),
            Disposition::Quarantine {
                sides: SideSet::BOTH
            }
        );
    }

    #[test]
    fn a_foreign_side_with_an_unattributable_scan_is_refused() {
        // `#207` C4: a one-byte flip in the database id of this store's own WAL
        // also trips verifyDatabaseID; quarantining unattributable bytes would
        // discard this store's data under a "another database" message.
        let verdict = classify_side(unattributable_foreign_side(539, 539));
        assert_eq!(
            verdict,
            SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable
            }
        );
        let pair = WalPairObservation::new(
            unattributable_foreign_side(539, 539),
            SideObservation::empty(),
        );
        assert!(matches!(classify_pair(pair), WalAccounting::Unknown { .. }));
    }

    #[test]
    fn a_foreign_side_with_no_scan_is_refused() {
        let verdict = classify_side(SideObservation {
            pre: 539,
            bytes_scanned: 0,
            post: 539,
            scan: None,
            foreign: true,
        });
        assert!(matches!(verdict, SideVerdict::Unknown { .. }));
    }

    #[test]
    fn a_proven_loss_outranks_a_foreign_peer() {
        let pair = WalPairObservation::new(
            foreign_side(539, 0),
            side(3103, 0, Some(scan_with_commit())),
        );
        assert_eq!(
            classify_pair(pair),
            WalAccounting::CommittedLost {
                pre_bytes: 3103,
                sides: SideSet::FROZEN
            }
        );
    }

    #[test]
    fn an_unknown_side_outranks_a_foreign_peer() {
        let pair = WalPairObservation::new(
            foreign_side(539, 0),
            side(900, 0, Some(scan_unattributable())),
        );
        assert!(matches!(classify_pair(pair), WalAccounting::Unknown { .. }));
    }

    #[test]
    fn a_foreign_wal_outranks_a_replayed_peer() {
        let pair = WalPairObservation::new(
            foreign_side(539, 0),
            side(530, 530, Some(scan_with_commit())),
        );
        assert!(matches!(
            classify_pair(pair),
            WalAccounting::ForeignWal { .. }
        ));
    }

    // ---- the corrected table, exhaustively --------------------------------

    #[derive(Clone, Copy, Debug)]
    enum Shape {
        NoScan,
        CleanNoCommit,
        CleanCommit,
        ConsumedCheckpoint,
        TornNoCommit,
        TornUnvalidated,
        Corrupt,
        ChecksumsOffNoCommit,
        ChecksumsOffCommit,
        Oversized,
        EmptyScan,
    }

    const SHAPES: [Shape; 11] = [
        Shape::NoScan,
        Shape::CleanNoCommit,
        Shape::CleanCommit,
        Shape::ConsumedCheckpoint,
        Shape::TornNoCommit,
        Shape::TornUnvalidated,
        Shape::Corrupt,
        Shape::ChecksumsOffNoCommit,
        Shape::ChecksumsOffCommit,
        Shape::Oversized,
        Shape::EmptyScan,
    ];

    fn scan_shape(shape: Shape) -> Option<WalScan> {
        match shape {
            Shape::NoScan => None,
            Shape::CleanNoCommit => Some(scan_no_commit()),
            Shape::CleanCommit => Some(scan_with_commit()),
            Shape::ConsumedCheckpoint => Some(scan_with_checkpoint()),
            Shape::TornNoCommit => Some(scan_torn_no_commit()),
            Shape::TornUnvalidated => Some(scan_torn_unvalidated()),
            Shape::Corrupt => Some(scan_unattributable()),
            Shape::ChecksumsOffNoCommit => Some(scan_checksums_off_no_commit()),
            Shape::ChecksumsOffCommit => Some(scan_checksums_off_commit()),
            Shape::Oversized => Some(scan_oversized()),
            Shape::EmptyScan => Some(WalScan::EMPTY),
        }
    }

    /// What each shape's scan must report; a scanner change that alters a shape
    /// fails the exhaustive table loudly.
    fn shape_evidence(shape: Shape) -> (Option<CommitEvidence>, bool) {
        match shape {
            Shape::NoScan => (None, false),
            Shape::CleanNoCommit | Shape::TornNoCommit => (Some(CommitEvidence::None), false),
            Shape::CleanCommit | Shape::ChecksumsOffCommit => (Some(CommitEvidence::Some), false),
            Shape::ConsumedCheckpoint => (Some(CommitEvidence::Some), true),
            Shape::ChecksumsOffNoCommit => (Some(CommitEvidence::Unverifiable), false),
            Shape::TornUnvalidated | Shape::Corrupt | Shape::Oversized => {
                (Some(CommitEvidence::Unknown), false)
            }
            Shape::EmptyScan => (Some(CommitEvidence::None), false),
        }
    }

    /// An independent restatement of the per-side rules, over primitives.
    fn reference_side(pre: u64, post: u64, shape: Shape, foreign: bool) -> SideVerdict {
        if pre == 0 {
            return if post == 0 {
                SideVerdict::NoWal
            } else {
                SideVerdict::FreshWal { bytes: post }
            };
        }
        if matches!(shape, Shape::EmptyScan) {
            return SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            };
        }
        if matches!(shape, Shape::Oversized) {
            return SideVerdict::Deferred {
                pre_bytes: pre,
                reason: DeferralReason::ScanTooLarge,
            };
        }
        let (evidence, observed_checkpoint) = shape_evidence(shape);
        let checkpoint = if observed_checkpoint {
            Some(CheckpointEvidence::Observed)
        } else {
            None
        };
        let evidence = match evidence {
            Some(e) => e,
            None => {
                return SideVerdict::Unknown {
                    reason: UnknownReason::CommitEvidenceUnreadable,
                }
            }
        };
        if foreign && !matches!(evidence, CommitEvidence::Unknown) {
            return SideVerdict::ForeignWal { pre_bytes: pre };
        }
        match (evidence, post == 0) {
            (CommitEvidence::None, false) | (CommitEvidence::Unverifiable, false) => {
                SideVerdict::Replayed {
                    pre_bytes: pre,
                    residual_bytes: post,
                    truncated_bytes: pre.saturating_sub(post),
                    checkpoint,
                }
            }
            (CommitEvidence::Some, false) => SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: post,
                truncated_bytes: pre.saturating_sub(post),
                checkpoint,
            },
            (CommitEvidence::None, true) => SideVerdict::NothingCommitted {
                uncommitted_bytes: pre,
            },
            (CommitEvidence::Unverifiable, true) => SideVerdict::Unknown {
                reason: UnknownReason::UnverifiableWal,
            },
            (CommitEvidence::Unknown, _) => SideVerdict::Unknown {
                reason: UnknownReason::CommitEvidenceUnreadable,
            },
            (CommitEvidence::Some, true) if checkpoint.is_none() => {
                SideVerdict::CommittedLost { pre_bytes: pre }
            }
            (CommitEvidence::Some, true) => SideVerdict::Replayed {
                pre_bytes: pre,
                residual_bytes: 0,
                truncated_bytes: pre,
                checkpoint,
            },
        }
    }

    #[test]
    fn every_per_side_combination_matches_the_reference_table() {
        let mut rows = 0usize;
        for shape in SHAPES {
            for foreign in [false, true] {
                for pre in [0u64, 9, 530, 3103] {
                    for post in [0u64, 25, 400, 530, 5000] {
                        let observed = classify_side(SideObservation {
                            pre,
                            bytes_scanned: pre.min(MAX_SCAN_BYTES + 1),
                            post,
                            scan: scan_shape(shape),
                            foreign,
                        });
                        assert_eq!(
                            observed,
                            reference_side(pre, post, shape, foreign),
                            "pre={pre} post={post} shape={shape:?} foreign={foreign}"
                        );
                        if pre > 0 && !matches!(shape, Shape::NoScan) {
                            let scan = scan_shape(shape).expect("pre>0 scans");
                            assert_eq!(
                                Some(scan.commit_evidence()),
                                shape_evidence(shape).0,
                                "shape {shape:?} must scan as its name says"
                            );
                            assert_eq!(
                                scan.ended_in_checkpoint(),
                                shape_evidence(shape).1,
                                "shape {shape:?} checkpoint flag"
                            );
                        }
                        rows += 1;
                    }
                }
            }
        }
        assert_eq!(rows, 11 * 2 * 4 * 5);
    }

    #[test]
    fn the_pair_fold_is_a_commutative_join() {
        for a in SHAPES {
            for b in SHAPES {
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
            WalAccounting::ForeignWal { pre_bytes, sides } => WalAccounting::ForeignWal {
                pre_bytes,
                sides: swap_sides(sides),
            },
            WalAccounting::Deferred { pre_bytes, sides } => WalAccounting::Deferred {
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
        for a in SHAPES {
            for b in SHAPES {
                let verdict = classify_pair(WalPairObservation::new(
                    side(530, 0, scan_shape(a)),
                    side(530, 0, scan_shape(b)),
                ));
                let side_proceeds = |shape: Shape| {
                    !matches!(
                        classify_side(side(530, 0, scan_shape(shape))),
                        SideVerdict::CommittedLost { .. }
                            | SideVerdict::Unknown { .. }
                            | SideVerdict::Deferred { .. }
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

    // ---- refusal protocol -------------------------------------------------

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

    // ---- telemetry --------------------------------------------------------

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
            WalAccounting::ForeignWal {
                pre_bytes: 1,
                sides: SideSet::ACTIVE
            }
            .slug(),
            "foreign_wal"
        );
        assert_eq!(
            WalAccounting::Replayed {
                pre_bytes: 0,
                residual_bytes: 0,
                truncated_bytes: 0,
                checkpoint: None
            }
            .slug(),
            "replayed"
        );
        assert_eq!(
            WalAccounting::Unknown {
                reason: UnknownReason::UnverifiableWal,
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
