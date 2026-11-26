//! The durable event outbox (Wave 3, step 1): the write-ahead file that makes
//! audit and mesh pulses survive the bounded bus.
//!
//! The bus is lossy by design: a bounded queue with drop-oldest, and a 250 ms
//! per-subscriber budget after which a slow subscriber is abandoned. For two
//! consumers that loss is silent data loss -- the audit log cannot reconstruct
//! a guard call that left no row, and the mesh outbox can only reconcile what
//! it is told about. So those two classes are *durability-qualified*: `emit`
//! appends the pulse here, synchronously (`write` + `sync_data`), before the
//! queue sees it, and the entry is acknowledged only after the subscribers
//! return.
//!
//! ## Contract: at-least-once, never exactly-once
//!
//! A class offset advances only after its delivery succeeded. A crash between
//! the subscriber's effect and the offset write therefore redelivers the entry
//! on the next start, and consumers absorb the duplicate: the mesh outbox keys
//! an entry on the delivery's `seq` (`OutboxEntry::bus_seq`), and the audit log
//! is append-only, where a duplicate row is a faithful record of a redelivery.
//! No text in this module claims exactly-once delivery.
//!
//! ## Layout
//!
//! - `event-outbox.jsonl` -- one versioned JSON object per line:
//!   `{v, seq, class, at, pulse}`. `seq` is a single monotonic sequence across
//!   both classes, so each class offset is a high-water mark in one order.
//!   `at` is RFC 3339 (UTC, millisecond precision). `pulse` is the tagged
//!   serde form of [`ThalamicPulse`] (`event.rs`).
//! - `event-outbox.offsets.json` -- `{v, base_seq, audit, mesh}`. `audit` and
//!   `mesh` are the last sequence each class acknowledged; `base_seq` is the
//!   greatest sequence ever assigned. Rewritten tmp + `sync_all` + rename on
//!   every advance.
//!
//! ## Bounds and repair
//!
//! At open a torn trailing fragment (bytes after the last newline) is
//! truncated before any append can glue onto it. A complete but corrupt line
//! is skipped and counted; it never fails the open. Compaction runs at open
//! when the file exceeds the configured cap (8 MiB by default) or when more
//! than half the entries are past both class offsets, keeping only entries
//! their own class has not acknowledged. Compaction preserves `base_seq`, so
//! the sequence never restarts at 1 and can never collide with an offset that
//! outlived the entries.

use super::ThalamicPulse;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The append-only entries file, inside the outbox directory.
pub const OUTBOX_FILE_NAME: &str = "event-outbox.jsonl";

/// The per-class offsets file, sibling of [`OUTBOX_FILE_NAME`].
pub const OFFSETS_FILE_NAME: &str = "event-outbox.offsets.json";

/// The format version stamped on every entry and on the offsets file.
pub const FORMAT_VERSION: u32 = 1;

/// The default size bound: past this many bytes, startup compaction rewrites
/// the entries file keeping only unacknowledged entries.
pub const MAX_OUTBOX_BYTES: u64 = 8 * 1024 * 1024;

/// A durability class: the unit of qualification, delivery, and offsets.
///
/// Qualification follows the subscription registry, never a hardcoded list of
/// event kinds: a pulse qualifies for a class iff that class's named
/// subscriber is registered **and** interested in the pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventClass {
    /// The audit trail: `guard_event_log`'s record of guard calls.
    Audit,
    /// Mesh sync state: `mesh_outbox`'s capture of shareable mutations.
    Mesh,
}

impl EventClass {
    /// Both classes, in a stable order for replay and worker startup.
    pub fn all() -> [EventClass; 2] {
        [EventClass::Audit, EventClass::Mesh]
    }

    /// The offsets-file key and log label for this class.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventClass::Audit => "audit",
            EventClass::Mesh => "mesh",
        }
    }

    /// The class a registered subscriber belongs to, by its stable name.
    /// `None` is every other subscriber: never durability-qualified.
    pub fn class_of_subscriber(name: &str) -> Option<EventClass> {
        match name {
            "guard_event_log" => Some(EventClass::Audit),
            "mesh_outbox" => Some(EventClass::Mesh),
            _ => None,
        }
    }

    /// Whether this class's subscriber acts on `pulse`. This mirrors the named
    /// subscribers' own matching: `GuardEventLog` handles only
    /// `GuardedActionFired`; `MeshOutbox` handles the three storage mutations
    /// and returns early on a guard verdict.
    pub fn interested_in(&self, pulse: &ThalamicPulse) -> bool {
        match self {
            EventClass::Audit => matches!(pulse, ThalamicPulse::GuardedActionFired { .. }),
            EventClass::Mesh => matches!(
                pulse,
                ThalamicPulse::Created { .. }
                    | ThalamicPulse::Superseded { .. }
                    | ThalamicPulse::Archived { .. }
            ),
        }
    }
}

/// The identity a durable delivery carries into the subscriber's context:
/// which class's acknowledgment the delivery earns, and at which sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableDelivery {
    /// The class this delivery is acknowledged by.
    pub class: EventClass,
    /// The entry's position in the single cross-class sequence.
    pub seq: u64,
}

/// One durable entry: the outbox line format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// The format version this line was written with.
    #[serde(default = "default_format_version")]
    pub v: u32,
    /// Position in the single cross-class sequence, assigned at append.
    pub seq: u64,
    /// The class this entry is delivered to (and acknowledged by).
    pub class: EventClass,
    /// When the entry was appended, as RFC 3339 (UTC, milliseconds).
    pub at: String,
    /// The pulse itself, in its tagged serde wire form (`event.rs`).
    pub pulse: ThalamicPulse,
}

fn default_format_version() -> u32 {
    FORMAT_VERSION
}

/// Why a durable append or offset operation failed.
///
/// Returned by `ThalamicBus::emit_checked`, where a mesh-state publisher fails
/// its operation on a failed append -- so this is a type callers match on, not
/// a log string.
#[derive(Debug)]
pub enum EventOutboxError {
    /// The outbox is poisoned: an append's partial write could not be rolled
    /// back, so the file's tail can no longer be trusted for a new line. The
    /// failure is terminal until an operator inspects the file; refusing the
    /// append is the only honest answer, because appending would glue new
    /// bytes onto a fragment the next open would truncate.
    Poisoned,
    /// The emit was refused at the bus boundary: the caller's
    /// `RecursionToken` is held at depth >= 1.
    RecursionRefused,
    /// A filesystem operation failed.
    Io(std::io::Error),
    /// An entry could not be encoded or decoded.
    Serde(serde_json::Error),
}

impl fmt::Display for EventOutboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventOutboxError::Poisoned => write!(
                f,
                "event outbox is poisoned: a partial append could not be rolled back"
            ),
            EventOutboxError::RecursionRefused => write!(
                f,
                "emit refused: the caller holds a recursion token at depth >= 1"
            ),
            EventOutboxError::Io(err) => write!(f, "event outbox io error: {err}"),
            EventOutboxError::Serde(err) => {
                write!(f, "event outbox serialization error: {err}")
            }
        }
    }
}

impl std::error::Error for EventOutboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EventOutboxError::Poisoned | EventOutboxError::RecursionRefused => None,
            EventOutboxError::Io(err) => Some(err),
            EventOutboxError::Serde(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for EventOutboxError {
    fn from(err: std::io::Error) -> Self {
        EventOutboxError::Io(err)
    }
}

impl From<serde_json::Error> for EventOutboxError {
    fn from(err: serde_json::Error) -> Self {
        EventOutboxError::Serde(err)
    }
}

/// The offsets file's shape: one high-water mark per class, plus the recovered
/// sequence base.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Offsets {
    #[serde(default = "default_format_version")]
    v: u32,
    /// The greatest sequence ever assigned. Preserved across compaction so the
    /// sequence never restarts at 1.
    #[serde(default)]
    base_seq: u64,
    #[serde(default)]
    audit: u64,
    #[serde(default)]
    mesh: u64,
}

impl Default for Offsets {
    fn default() -> Self {
        Self {
            v: FORMAT_VERSION,
            base_seq: 0,
            audit: 0,
            mesh: 0,
        }
    }
}

impl Offsets {
    fn get(&self, class: EventClass) -> u64 {
        match class {
            EventClass::Audit => self.audit,
            EventClass::Mesh => self.mesh,
        }
    }

    fn set(&mut self, class: EventClass, seq: u64) {
        match class {
            EventClass::Audit => self.audit = seq,
            EventClass::Mesh => self.mesh = seq,
        }
    }

    /// The highest sequence any recorded state remembers.
    fn high_water(&self) -> u64 {
        self.base_seq.max(self.audit).max(self.mesh)
    }
}

/// The outbox's mutable state. `entries` is an in-memory mirror of the file
/// (same order, same contents), so a reader never races a parse.
struct Inner {
    /// The next sequence to assign: one past the greatest sequence ever
    /// recorded -- an entry's, an offset's, or the persisted base.
    next_seq: u64,
    base_seq: u64,
    entries: Vec<OutboxEntry>,
    offsets: Offsets,
    /// Set when a failed append's rollback also failed: the file tail can no
    /// longer be trusted for a new line.
    poisoned: bool,
    /// Complete but corrupt lines skipped at open.
    corrupt_skipped: u64,
}

/// The durable outbox over one directory. `Send + Sync` by construction (one
/// `Mutex` over the state); the bus shares it between `emit`'s append path and
/// the dispatcher's acknowledgment path.
pub struct EventOutbox {
    dir: PathBuf,
    max_bytes: u64,
    inner: Mutex<Inner>,
}

/// The file operations the append write step needs, behind a trait so a test
/// can drive the step with a file whose write fails after a partial write --
/// the mid-line failure a disk filling produces. Production always passes the
/// real [`File`]; the trait is a seam for the write step only.
trait AppendTarget: Write {
    /// The file's current length: the point a failed append rolls back to.
    fn file_len(&self) -> std::io::Result<u64>;
    /// Flush the file's data to storage (`File::sync_data`).
    fn sync(&self) -> std::io::Result<()>;
    /// Truncate the file to `len` (`File::set_len`): the rollback itself.
    fn truncate_to(&self, len: u64) -> std::io::Result<()>;
}

impl AppendTarget for File {
    fn file_len(&self) -> std::io::Result<u64> {
        self.metadata().map(|metadata| metadata.len())
    }

    fn sync(&self) -> std::io::Result<()> {
        self.sync_data()
    }

    fn truncate_to(&self, len: u64) -> std::io::Result<()> {
        self.set_len(len)
    }
}

/// Why one durable line write failed, and whether the file was left
/// byte-identical to its pre-append state.
#[derive(Debug)]
struct AppendFailure {
    error: EventOutboxError,
    rolled_back: bool,
}

/// Write one entry line and sync it, atomically from the caller's point of
/// view: the file's length is recorded before the write, and on any write or
/// sync error the file is truncated back to that length. Without the rollback
/// the torn bytes of a mid-line failure stay in the file and the next
/// successful append glues onto them; the glued line is then truncated as torn
/// at the next open, taking the later, successfully-receipted entry with it.
/// A failed append therefore changes nothing. Callers hold the outbox lock
/// across the whole call, so no other append or compaction can interleave.
fn write_line_durably(target: &mut impl AppendTarget, line: &[u8]) -> Result<(), AppendFailure> {
    let prior_len = target.file_len().map_err(|error| AppendFailure {
        error: error.into(),
        rolled_back: true,
    })?;
    let outcome = target.write_all(line).and_then(|()| target.sync());
    if let Err(error) = outcome {
        let rolled_back = target.truncate_to(prior_len).is_ok();
        if rolled_back {
            let _ = target.sync();
        }
        return Err(AppendFailure {
            error: error.into(),
            rolled_back,
        });
    }
    Ok(())
}

/// The clean prefix length of an entries file: everything before a trailing
/// fragment that has no newline. A fragment was never durably receipted (the
/// append writes the line and newline before `sync_data`), so it is truncated
/// rather than read.
fn repair_torn_tail(path: &Path, bytes: &[u8]) -> Result<usize, EventOutboxError> {
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return Ok(bytes.len());
    }
    let clean_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(clean_len as u64)?;
    file.sync_data()?;
    tracing::warn!(
        "event outbox: truncated {} torn trailing byte(s) from {} at open",
        bytes.len() - clean_len,
        path.display()
    );
    Ok(clean_len)
}

/// Parse the clean prefix into entries, skipping and counting complete lines
/// that are not valid UTF-8, not JSON, not an `OutboxEntry`, or not the current
/// format version. One bad line never fails the open.
fn parse_entries(bytes: &[u8], path: &Path) -> (Vec<OutboxEntry>, u64) {
    let mut entries = Vec::new();
    let mut corrupt = 0u64;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let parsed = std::str::from_utf8(line)
            .map_err(|err| err.to_string())
            .and_then(|text| {
                serde_json::from_str::<OutboxEntry>(text).map_err(|err| err.to_string())
            });
        match parsed {
            Ok(entry) if entry.v == FORMAT_VERSION => entries.push(entry),
            Ok(entry) => {
                tracing::warn!(
                    "event outbox: skipping an entry with unsupported version {} in {}",
                    entry.v,
                    path.display()
                );
                corrupt += 1;
            }
            Err(err) => {
                tracing::warn!(
                    "event outbox: skipping a corrupt line in {}: {err}",
                    path.display()
                );
                corrupt += 1;
            }
        }
    }
    entries.sort_by_key(|entry| entry.seq);
    (entries, corrupt)
}

impl EventOutbox {
    /// Open (creating if needed) the outbox in `dir`: load the offsets, repair
    /// a torn tail, load the entries, recover the sequence base, and run
    /// startup compaction against the default [`MAX_OUTBOX_BYTES`] cap.
    pub fn open(dir: &Path) -> Result<Self, EventOutboxError> {
        Self::open_with_max_bytes(dir, MAX_OUTBOX_BYTES)
    }

    /// [`EventOutbox::open`] with an explicit compaction cap. Production uses
    /// the default; tests use a small cap to exercise the size rule without
    /// writing eight megabytes.
    pub fn open_with_max_bytes(dir: &Path, max_bytes: u64) -> Result<Self, EventOutboxError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(OUTBOX_FILE_NAME);
        if !path.exists() {
            // Create it now so no reader ever races a missing file.
            File::create(&path)?;
        }
        let mut offsets = read_offsets(dir);
        let raw = std::fs::read(&path)?;
        let clean_len = repair_torn_tail(&path, &raw)?;
        let (entries, corrupt_skipped) = parse_entries(&raw[..clean_len], &path);
        let max_entry = entries.iter().map(|entry| entry.seq).max().unwrap_or(0);
        let base_seq = offsets.high_water().max(max_entry);
        offsets.base_seq = base_seq;
        let outbox = Self {
            dir: dir.to_path_buf(),
            max_bytes,
            inner: Mutex::new(Inner {
                next_seq: base_seq + 1,
                base_seq,
                entries,
                offsets,
                poisoned: false,
                corrupt_skipped,
            }),
        };
        let skipped = outbox.corrupt_entries_skipped();
        if skipped > 0 {
            tracing::warn!(
                "event outbox: skipped {skipped} corrupt line(s) in {}; the surviving entries are intact",
                path.display()
            );
        }
        // Startup compaction. A failure here must not take durability down
        // with it: appends still work against the uncompacted file, and the
        // next start tries again -- so it is logged, not fatal.
        if let Err(err) = outbox.compact_if_needed() {
            tracing::error!("event outbox: startup compaction failed: {err}");
        }
        Ok(outbox)
    }

    fn entries_path(&self) -> PathBuf {
        self.dir.join(OUTBOX_FILE_NAME)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>, EventOutboxError> {
        self.inner.lock().map_err(|_| EventOutboxError::Poisoned)
    }

    /// Durably append `pulse` for `class`: encode the entry, write the line,
    /// `sync_data`, and only then record it in the mirror and return its
    /// sequence. A failed append changes nothing -- no mirror entry, no
    /// sequence consumed, and no bytes (the write step rolls the file back).
    pub fn append(
        &self,
        class: EventClass,
        pulse: &ThalamicPulse,
    ) -> Result<u64, EventOutboxError> {
        let mut inner = self.lock()?;
        if inner.poisoned {
            return Err(EventOutboxError::Poisoned);
        }
        let seq = inner.next_seq;
        let entry = OutboxEntry {
            v: FORMAT_VERSION,
            seq,
            class,
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            pulse: pulse.clone(),
        };
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.entries_path())?;
        if let Err(failure) = write_line_durably(&mut file, &line) {
            if !failure.rolled_back {
                inner.poisoned = true;
                tracing::error!(
                    class = class.as_str(),
                    "event outbox: a failed append could not be rolled back; outbox poisoned: {}",
                    failure.error
                );
            }
            return Err(failure.error);
        }
        drop(file);
        inner.entries.push(entry);
        inner.next_seq = seq + 1;
        inner.base_seq = inner.base_seq.max(seq);
        inner.offsets.base_seq = inner.offsets.base_seq.max(inner.base_seq);
        Ok(seq)
    }

    /// This class's unacknowledged entries, in sequence order.
    pub fn pending(&self, class: EventClass) -> Vec<OutboxEntry> {
        let inner = match self.lock() {
            Ok(inner) => inner,
            Err(_) => return Vec::new(),
        };
        let offset = inner.offsets.get(class);
        inner
            .entries
            .iter()
            .filter(|entry| entry.class == class && entry.seq > offset)
            .cloned()
            .collect()
    }

    /// The last sequence this class acknowledged.
    #[cfg(test)]
    pub fn offset(&self, class: EventClass) -> u64 {
        match self.lock() {
            Ok(inner) => inner.offsets.get(class),
            Err(_) => 0,
        }
    }

    /// The greatest sequence ever assigned, entry or offset.
    #[cfg(test)]
    pub fn base_seq(&self) -> u64 {
        match self.lock() {
            Ok(inner) => inner.base_seq.max(inner.offsets.base_seq),
            Err(_) => 0,
        }
    }

    /// Complete but corrupt lines skipped by the last [`EventOutbox::open`].
    pub fn corrupt_entries_skipped(&self) -> u64 {
        match self.lock() {
            Ok(inner) => inner.corrupt_skipped,
            Err(_) => 0,
        }
    }

    /// Acknowledge `seq` for `class`: advance the offset in memory and rewrite
    /// the offsets file (tmp + `sync_all` + rename) before returning, so an
    /// acknowledged entry is never redelivered by a later process mistaking
    /// volatile state for durable. Offsets never move backwards.
    pub fn advance(&self, class: EventClass, seq: u64) -> Result<(), EventOutboxError> {
        let mut inner = self.lock()?;
        if seq <= inner.offsets.get(class) {
            return Ok(());
        }
        let mut offsets = inner.offsets;
        offsets.set(class, seq);
        offsets.base_seq = offsets.base_seq.max(inner.base_seq).max(seq);
        write_offsets(&self.dir, offsets)?;
        inner.base_seq = inner.base_seq.max(offsets.base_seq);
        inner.offsets = offsets;
        Ok(())
    }

    /// Compaction, when a bound says it is due: the file exceeds the
    /// configured cap, or more than half the entries are past both class
    /// offsets (acknowledged by everyone who will ever read them). Returns
    /// whether it compacted.
    pub fn compact_if_needed(&self) -> Result<bool, EventOutboxError> {
        let mut inner = self.lock()?;
        let size = std::fs::metadata(self.entries_path())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let total = inner.entries.len();
        let floor = inner.offsets.audit.min(inner.offsets.mesh);
        let dead = inner
            .entries
            .iter()
            .filter(|entry| entry.seq <= floor)
            .count();
        if size > self.max_bytes || (total > 0 && dead * 2 > total) {
            self.compact_locked(&mut inner)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn compact_locked(&self, inner: &mut Inner) -> Result<(), EventOutboxError> {
        let kept: Vec<OutboxEntry> = inner
            .entries
            .iter()
            .filter(|entry| entry.seq > inner.offsets.get(entry.class))
            .cloned()
            .collect();
        let tmp_path = self.dir.join(format!("{OUTBOX_FILE_NAME}.tmp"));
        {
            let mut file = File::create(&tmp_path)?;
            for entry in &kept {
                let mut line = serde_json::to_vec(entry)?;
                line.push(b'\n');
                file.write_all(&line)?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, self.entries_path())?;
        sync_dir(&self.dir);
        inner.entries = kept;
        // Persist the recovered base alongside the rewrite. Without this a
        // crash between compaction and the next advance would leave the base
        // only in memory, which is exactly the state compaction exists to
        // survive.
        let mut offsets = inner.offsets;
        offsets.base_seq = offsets.base_seq.max(inner.base_seq);
        if offsets.base_seq != inner.offsets.base_seq {
            write_offsets(&self.dir, offsets)?;
            inner.offsets = offsets;
        }
        Ok(())
    }
}

/// The offsets, read tolerantly: a missing file is a fresh outbox; a file that
/// does not parse (or has another format version) is reported and read as
/// all-zero -- redelivery is the safe failure direction under at-least-once.
fn read_offsets(dir: &Path) -> Offsets {
    let path = dir.join(OFFSETS_FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Offsets::default(),
        Err(err) => {
            tracing::warn!(
                "event outbox: offsets file {} unreadable ({err}); treating every entry as unacknowledged",
                path.display()
            );
            return Offsets::default();
        }
    };
    match serde_json::from_str::<Offsets>(&text) {
        Ok(offsets) if offsets.v == FORMAT_VERSION => offsets,
        Ok(offsets) => {
            tracing::warn!(
                "event outbox: offsets file {} has unsupported version {}; treating every entry as unacknowledged",
                path.display(),
                offsets.v
            );
            Offsets::default()
        }
        Err(err) => {
            tracing::warn!(
                "event outbox: offsets file {} unparseable ({err}); treating every entry as unacknowledged",
                path.display()
            );
            Offsets::default()
        }
    }
}

/// Rewrite the offsets file durably: write a sibling tmp file, `sync_all` it,
/// then rename over the real name, so a reader sees the old offsets or the new
/// ones, never a torn half.
fn write_offsets(dir: &Path, offsets: Offsets) -> Result<(), EventOutboxError> {
    let tmp_path = dir.join(format!("{OFFSETS_FILE_NAME}.tmp"));
    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(serde_json::to_string(&offsets)?.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp_path, dir.join(OFFSETS_FILE_NAME))?;
    sync_dir(dir);
    Ok(())
}

/// Best-effort directory sync so a rename is durable. Not every platform
/// allows syncing a directory handle; the rename itself is already atomic.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ThalamicPulse;
    use std::io::Write;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-event-outbox-test-{}-{}-{}",
            std::process::id(),
            tag,
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn created(id: &str) -> ThalamicPulse {
        ThalamicPulse::Created {
            id: id.to_string(),
            namespace: "n".to_string(),
            kind: "fact".to_string(),
        }
    }

    fn guarded(trace_id: &str) -> ThalamicPulse {
        ThalamicPulse::GuardedActionFired {
            trace_id: trace_id.to_string(),
            action_type: "shell".to_string(),
            payload_hash: 7,
            verdict: "deny".to_string(),
            rule_ids_triggered: vec!["r1".to_string()],
            namespace: "n".to_string(),
        }
    }

    fn ids(entries: &[OutboxEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match &entry.pulse {
                ThalamicPulse::Created { id, .. } => id.clone(),
                ThalamicPulse::Archived { id, .. } => id.clone(),
                ThalamicPulse::GuardedActionFired { trace_id, .. } => trace_id.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_torn_tail_is_repaired_at_open() {
        let dir = scratch("torn-tail");
        let path = dir.join(OUTBOX_FILE_NAME);
        let outbox = EventOutbox::open(&dir).expect("the outbox opens");
        outbox
            .append(EventClass::Mesh, &created("kept"))
            .expect("append kept");
        outbox
            .append(EventClass::Mesh, &created("torn"))
            .expect("append torn");
        drop(outbox);

        // A crash mid-append leaves a partial second line: no trailing newline.
        let len = std::fs::metadata(&path).expect("file metadata").len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for truncation")
            .set_len(len - 7)
            .expect("truncate");

        let outbox = EventOutbox::open(&dir).expect("the torn outbox reopens");
        let pending = outbox.pending(EventClass::Mesh);
        assert_eq!(ids(&pending), vec!["kept".to_string()]);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&pending[0].at).is_ok(),
            "every durable entry carries an RFC 3339 timestamp: {}",
            pending[0].at
        );

        // The unreceipted torn entry never consumed a sequence: the next
        // append reuses it and lands as its own parseable line.
        let seq = outbox
            .append(EventClass::Mesh, &created("after-repair"))
            .expect("append after repair");
        assert_eq!(seq, 2);
        let bytes = std::fs::read(&path).expect("read repaired file");
        assert!(bytes.ends_with(b"\n"), "the repaired file ends cleanly");
        let text = String::from_utf8(bytes).expect("the repaired file is UTF-8");
        assert_eq!(text.lines().count(), 2);
        for line in text.lines() {
            serde_json::from_str::<OutboxEntry>(line).expect("every line is a parseable entry");
        }
    }

    #[test]
    fn an_invalid_utf8_line_is_skipped_at_open() {
        let dir = scratch("invalid-utf8");
        let path = dir.join(OUTBOX_FILE_NAME);
        let outbox = EventOutbox::open(&dir).expect("the outbox opens");
        outbox
            .append(EventClass::Mesh, &created("before"))
            .expect("append before");
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open for corruption");
            file.write_all(&[0xff, 0xfe, b'\n']).expect("write bytes");
            file.write_all(b"{\"not\":\"an entry\"}\n")
                .expect("write a well-formed but wrong line");
        }
        outbox
            .append(EventClass::Mesh, &created("after"))
            .expect("append after");
        drop(outbox);

        let outbox = EventOutbox::open(&dir).expect("invalid UTF-8 does not fail open");
        assert_eq!(
            ids(&outbox.pending(EventClass::Mesh)),
            vec!["before".to_string(), "after".to_string()]
        );
        assert_eq!(
            outbox.corrupt_entries_skipped(),
            2,
            "the invalid UTF-8 line and the non-entry JSON line are both counted"
        );
    }

    /// A real file whose writes stop after a fixed byte budget and then fail,
    /// as a disk filling mid-write behaves: a prefix of the caller's bytes
    /// genuinely lands in the real file before the error surfaces.
    struct FailAfterFile {
        file: std::fs::File,
        budget: usize,
        len_at_failure: Option<u64>,
        rollback_fails: bool,
    }

    impl Write for FailAfterFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.budget == 0 {
                self.len_at_failure = Some(self.file.metadata()?.len());
                return Err(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "simulated mid-write failure (disk full)",
                ));
            }
            let accepted = buf.len().min(self.budget);
            let written = self.file.write(&buf[..accepted])?;
            self.budget -= written;
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }

    impl AppendTarget for FailAfterFile {
        fn file_len(&self) -> std::io::Result<u64> {
            self.file.metadata().map(|metadata| metadata.len())
        }

        fn sync(&self) -> std::io::Result<()> {
            self.file.sync_data()
        }

        fn truncate_to(&self, len: u64) -> std::io::Result<()> {
            if self.rollback_fails {
                return Err(std::io::Error::other("simulated rollback failure"));
            }
            self.file.set_len(len)
        }
    }

    #[test]
    fn a_failed_append_rolls_back_the_partial_write() {
        let dir = scratch("append-rollback");
        let path = dir.join(OUTBOX_FILE_NAME);
        let outbox = EventOutbox::open(&dir).expect("the outbox opens");
        outbox
            .append(EventClass::Mesh, &created("kept"))
            .expect("append kept");
        drop(outbox);
        let prior = std::fs::read(&path).expect("read prior bytes");

        let entry = OutboxEntry {
            v: FORMAT_VERSION,
            seq: 2,
            class: EventClass::Mesh,
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            pulse: created("victim"),
        };
        let mut line = serde_json::to_vec(&entry).expect("serialize entry");
        line.push(b'\n');
        let mut failing = FailAfterFile {
            file: std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open failing target"),
            budget: 10,
            len_at_failure: None,
            rollback_fails: false,
        };
        let failure = write_line_durably(&mut failing, &line).expect_err("the write fails");
        assert!(
            matches!(failure.error, EventOutboxError::Io(_)),
            "the write failure surfaces as an io error: {failure:?}"
        );
        assert!(
            failure.rolled_back,
            "the failed append reports that the rollback succeeded"
        );
        assert_eq!(
            failing.len_at_failure,
            Some(prior.len() as u64 + 10),
            "the partial bytes genuinely landed before the failure"
        );
        drop(failing);
        assert_eq!(
            std::fs::read(&path).expect("read after rollback"),
            prior,
            "the rollback leaves the file byte-identical to its pre-append state"
        );

        // A subsequent successful append lands as its own parseable entry --
        // it does not glue onto the failed append's debris.
        let outbox = EventOutbox::open(&dir).expect("the outbox reopens");
        let seq = outbox
            .append(EventClass::Mesh, &created("after-rollback"))
            .expect("append after rollback");
        assert_eq!(seq, 2, "the failed append consumed no sequence");
        let text = std::fs::read_to_string(&path).expect("read after reappend");
        assert_eq!(text.lines().count(), 2);
        for line in text.lines() {
            serde_json::from_str::<OutboxEntry>(line).expect("every line parses");
        }
    }

    #[test]
    fn a_failed_rollback_poisons_the_outbox() {
        let target = scratch("poison").join("file.jsonl");
        std::fs::write(&target, b"{}\n").expect("seed file");
        let mut failing = FailAfterFile {
            file: std::fs::OpenOptions::new()
                .append(true)
                .open(&target)
                .expect("open failing target"),
            budget: 4,
            len_at_failure: None,
            rollback_fails: true,
        };
        let failure = write_line_durably(&mut failing, b"partial line\n").expect_err("write fails");
        assert!(
            !failure.rolled_back,
            "a failed rollback is reported, not hidden"
        );
    }

    #[test]
    fn compaction_runs_when_the_file_exceeds_the_configured_cap() {
        let dir = scratch("compaction-cap");
        let path = dir.join(OUTBOX_FILE_NAME);
        let outbox = EventOutbox::open(&dir).expect("the outbox opens");
        let first = outbox
            .append(EventClass::Mesh, &created("acknowledged"))
            .expect("append acknowledged");
        outbox
            .append(EventClass::Mesh, &created("pending"))
            .expect("append pending");
        outbox
            .advance(EventClass::Mesh, first)
            .expect("advance mesh");
        drop(outbox);
        let before = std::fs::metadata(&path).expect("metadata").len();
        assert!(before > 1);

        // A one-byte cap forces the size rule at the next open.
        let outbox = EventOutbox::open_with_max_bytes(&dir, 1).expect("reopen with a tiny cap");
        let after = std::fs::metadata(&path).expect("metadata").len();
        assert!(
            after < before,
            "compaction must shrink the file: before {before}, after {after}"
        );
        assert_eq!(
            ids(&outbox.pending(EventClass::Mesh)),
            vec!["pending".to_string()],
            "compaction keeps only unacknowledged entries"
        );
        assert_eq!(outbox.offset(EventClass::Mesh), first);
    }

    #[test]
    fn compaction_runs_when_over_half_the_entries_are_past_both_offsets() {
        let dir = scratch("compaction-half");
        let outbox = EventOutbox::open(&dir).expect("the outbox opens");
        let audit_one = outbox
            .append(EventClass::Audit, &guarded("a1"))
            .expect("append audit 1");
        outbox
            .append(EventClass::Mesh, &created("m2"))
            .expect("append mesh 2");
        let audit_three = outbox
            .append(EventClass::Audit, &guarded("a3"))
            .expect("append audit 3");
        let mesh_four = outbox
            .append(EventClass::Mesh, &created("m4"))
            .expect("append mesh 4");
        let audit_five = outbox
            .append(EventClass::Audit, &guarded("a5"))
            .expect("append audit 5");
        assert_eq!((audit_one, audit_three, audit_five), (1, 3, 5));
        outbox
            .advance(EventClass::Audit, audit_three)
            .expect("advance audit");
        outbox
            .advance(EventClass::Mesh, mesh_four)
            .expect("advance mesh");
        assert_eq!(outbox.base_seq(), 5);

        // 4 of 5 entries are past both offsets: reopening compacts.
        outbox.compact_if_needed().expect("compaction");
        assert_eq!(
            ids(&outbox.pending(EventClass::Audit)),
            vec!["a5".to_string()]
        );
        assert!(outbox.pending(EventClass::Mesh).is_empty());
        assert_eq!(
            outbox.base_seq(),
            5,
            "compaction preserves the sequence base"
        );

        let next = outbox
            .append(EventClass::Audit, &guarded("a6"))
            .expect("append after compaction");
        assert_eq!(next, 6);
    }

    #[test]
    fn the_sequence_base_never_restarts_at_one() {
        let dir = scratch("sequence-base");
        // Seed the offsets file with a base far beyond any surviving entry,
        // exactly the state compaction leaves behind: every entry gone, the
        // high-water mark still recorded.
        std::fs::write(
            dir.join(OFFSETS_FILE_NAME),
            r#"{"v":1,"base_seq":42,"audit":42,"mesh":42}"#,
        )
        .expect("seed offsets");
        let outbox = EventOutbox::open(&dir).expect("the outbox opens");
        assert!(outbox.pending(EventClass::Mesh).is_empty());
        assert_eq!(outbox.offset(EventClass::Mesh), 42);

        let seq = outbox
            .append(EventClass::Mesh, &created("m43"))
            .expect("append after compaction");
        assert_eq!(seq, 43, "the sequence continues past the recorded base");
        assert_eq!(outbox.base_seq(), 43);
        outbox.advance(EventClass::Mesh, seq).expect("advance");
        drop(outbox);

        let offsets = std::fs::read_to_string(dir.join(OFFSETS_FILE_NAME)).expect("offsets file");
        let offsets: serde_json::Value = serde_json::from_str(&offsets).expect("offsets json");
        assert_eq!(offsets["v"], 1);
        assert_eq!(offsets["base_seq"], 43);
        assert_eq!(offsets["mesh"], 43);

        // A reopen compacts the acknowledged entry away and still continues
        // the sequence, never restarting at 1.
        let outbox = EventOutbox::open(&dir).expect("reopen");
        assert!(outbox.pending(EventClass::Mesh).is_empty());
        let next = outbox
            .append(EventClass::Mesh, &created("m44"))
            .expect("append after reopen");
        assert_eq!(next, 44);
    }
}
