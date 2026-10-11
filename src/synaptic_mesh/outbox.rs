//! The SynapticMesh durable outbox (slice 3).
//!
//! See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`
//! §"Durable mutation → mesh handoff" — the Thalamic Bus tees a memory
//! mutation into a durable outbox, NOT straight into git, and the mesh-worker
//! drains the outbox.
//!
//! ## Design decision: (b) write-ahead JSONL (documented, per task)
//!
//! The alternative (a — outbox rows in the store via a new `VectorStore`
//! method) would reach the atomic-with-the-mutation guarantee the spec's
//! idealised flow sketches, but it forces a trait method onto `VectorStore`
//! and therefore onto *every* implementer (the daemon's `LadybugStore` plus
//! every test double), and it pollutes the store schema with mesh bookkeeping
//! that slice 4's import path would then have to filter out. That blast radius
//! is exactly what a publish-path slice must not own.
//!
//! So the outbox is a write-ahead JSONL file in a daemon-owned scratch dir
//! that is a *sibling* of the mesh checkout — deliberately OUTSIDE the git
//! repo, so `MeshStorage::commit_all`'s `git add -A` can never stage it.
//! Layout:
//!
//! ```text
//! ~/.NeuroStrata/synaptic-mesh/<mesh_id>/          # the git repo (MeshStorage)
//! ~/.NeuroStrata/synaptic-mesh/<mesh_id>.outbox/   # this module's WAL dir
//!     outbox.wal                                    # append-only JSONL
//! ```
//!
//! ## The recoverable write-ahead protocol (WAL-before + origin reconciliation)
//!
//! There is **no cross-write transaction** between a LadybugDB mutation and
//! the outbox: the bus tees the pulse *after* the store commit, and the
//! subscriber (which runs on the dispatcher task) appends the outbox entry
//! afterwards. The order of durable events is therefore:
//!
//!   1. store commit  (LadybugDB — the source of truth)
//!   2. WAL append    (the subscriber, capturing the full payload — "WAL-before")
//!   3. mesh file     (the worker, atomic tmp-write + rename)
//!   4. git commit    (one per drain)
//!   5. WAL compaction (mark committed)
//!
//! "WAL-before" means the outbox entry is **self-contained**: the subscriber
//! reads the memory back from the store and snapshots its content + metadata
//! into the entry, so the worker never has to re-read the store (which it
//! cannot — `drain_once` is a pure, synchronous function). The entry survives
//! a daemon crash between (1) and (2)-completion *whenever the append reached
//! `sync_all`*, and the worker replays it on restart.
//!
//! "Origin reconciliation" means the worker applies each entry **idempotently
//! against the mesh checkout's current file**: the record's `update_id` is the
//! entry's `stable_op_id`, so re-applying the same entry produces byte-identical
//! output and `git commit` sees no change (see `worker::drain_once`'s
//! `AlreadyApplied` arm). A crash between (3) and (4)/(5) is therefore a no-op
//! on retry, never a duplicate commit.
//!
//! ### The one residual gap (documented, not hidden)
//!
//! The accepted loss window is wider than "a crash between the store commit
//! (1) and the WAL append (2)". It has two natural portions, one per
//! component on the tee path:
//!
//! * The bus itself is lossy by design: the queue is bounded and
//!   drop-oldest (`bus.rs`, `Queue::push` trims the front when full) and each
//!   subscriber runs under a `250ms` timeout (`SUBSCRIBER_TIMEOUT`) after
//!   which it is abandoned. So a pulse can vanish **before the WAL append
//!   (2) ever runs even in a perfectly healthy daemon** — a busy bus
//!   overflowing the queue, or a slow subscriber eating its timeout — with
//!   nothing crashing at all.
//! * A crash *between* (1) and (2) — before the append's `sync_all` lands —
//!   leaves the same state: the memory durable in LadybugDB but absent from
//!   the outbox.
//!
//! Both are the price of teeing the bus *after* the store mutation instead of
//! inside it, and both are covered the same way, by reconstruction rather
//! than by this code: the next mutation of a record re-emits it as a
//! **full-row re-snapshot** (the entry carries the whole payload), and slice
//! 4's **pull/replay** path is the standing full store→mesh reconciliation
//! (spec §"Cursor and full-tree reconciliation"). What is accepted is
//! therefore at-least-once *between peers* and only eventually consistent
//! locally: the memory is never lost, but its mesh projection may lag until
//! one of those two cover events arrives. This is an accepted property of
//! the design, not a guarantee the publish path can code its way out of.
//!
//! ### Concurrency
//!
//! The subscriber (dispatcher task) and the worker (drain loop task) are
//! different tasks opening their own [`Outbox`] over the same directory.
//! `append` and `mark_committed` are serialised by a process-global lock so a
//! compaction can never drop an entry that an append landed mid-compaction;
//! `pending` needs no lock (it reads a file that is only ever atomically
//! renamed or single-line appended).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The WAL file inside the outbox directory.
pub const OUTBOX_FILE_NAME: &str = "outbox.wal";

/// Op tags. The WAL is internal, so the op is a string field rather than a
/// tagged enum: it survives a format reader that predates a new op, and the
/// worker matches these constants (an unknown op is skipped, not a crash).
pub const OP_CREATED: &str = "created";
pub const OP_SUPERSEDED: &str = "superseded";
pub const OP_ARCHIVED: &str = "archived";

/// The memory types that may be shared over the mesh (spec §"Sharing
/// boundary": `rule`, `fact`, `procedure` definitions). Everything else —
/// tasks, structural/source-ingestion rows, embeddings, the bus's own
/// sentinel/audit rows — is strictly local.
const SHAREABLE_MEMORY_TYPES: &[&str] = &["rule", "fact", "procedure"];

/// Whether `memory_type` is a type the mesh may carry. Used by both the
/// subscriber (to decide what to capture) and the worker (to decide what to
/// export). The worker also re-checks it defensively, because a captured
/// entry's type is not the worker's only filter.
pub fn is_shareable_memory_type(memory_type: &str) -> bool {
    SHAREABLE_MEMORY_TYPES.contains(&memory_type)
}

/// One pending mesh operation, as it sits in the WAL. Self-contained on
/// purpose: the worker builds the mesh record from this alone and never reads
/// the store (see the module docs' "WAL-before" note).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Idempotency key. One fresh v4 per captured mutation; the worker uses it
    /// as the record's `update_id`, so re-applying the same entry produces
    /// byte-identical output.
    pub stable_op_id: Uuid,
    /// `created` | `superseded` | `archived` (see the `OP_*` constants).
    pub op: String,
    /// The memory id — also the mesh record id (memory ids are UUID v4).
    pub id: String,
    pub namespace: String,
    /// The memory's `memory_type` as captured at write time.
    pub memory_type: String,
    /// The memory's text content, snapshotted at write time.
    pub content: String,
    /// The memory's full metadata, snapshotted at write time. The worker reads
    /// `eligibility`, `mesh` (foreign-origin), `trust`, `source`, `tags`, and
    /// the procedure definition fields out of this.
    pub metadata: serde_json::Value,
    /// The replacement id, present only for a `superseded` op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Dispatcher timestamp (epoch seconds) stamped when the pulse was handled.
    /// The worker uses it as the `created_at`/`archived_at` anchor.
    pub ts: i64,
    /// The mesh manifest epoch stamped at capture time (when the subscriber
    /// appended this entry). The worker compares it against the manifest's
    /// CURRENT epoch at drain time: an entry born in an older epoch is
    /// quarantined (left pending for operator inspection) rather than replayed
    /// into the new epoch (spec §"Deletion & erasure"). `#[serde(default)]`
    /// reads a WAL line written before this field existed as `0` — an epoch
    /// lower than any real manifest epoch, so such a legacy entry is
    /// quarantined rather than auto-replayed at an unknown birth epoch.
    #[serde(default)]
    pub epoch: u32,
}

/// Serialises `append` and `mark_committed` so a compaction can never lose an
/// entry an append landed mid-flight (see module docs' "Concurrency").
static OUTBOX_LOCK: Mutex<()> = Mutex::new(());

/// The outbox directory for a mesh checkout: `<mesh_dir>.outbox`, a sibling of
/// the repo so `git add -A` never stages it.
pub fn outbox_dir_for(mesh_dir: &Path) -> PathBuf {
    let mut os = mesh_dir.as_os_str().to_os_string();
    os.push(".outbox");
    PathBuf::from(os)
}

/// A handle to one mesh's durable outbox directory.
#[derive(Debug, Clone)]
pub struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    /// Open (creating if needed) the outbox directory and its WAL file.
    /// Creating the WAL here — at daemon startup, before any entry — is what
    /// makes the file's directory entry durable independently of the first
    /// append.
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("outbox dir {} could not be created: {e}", dir.display()))?;
        let wal = dir.join(OUTBOX_FILE_NAME);
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal)
            .map_err(|e| format!("outbox WAL {} could not be opened: {e}", wal.display()))?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Append one entry durably: a single JSONL line followed by a newline,
    /// then `sync_all` so the write is on disk before the worker may ever see
    /// it. The trailing newline is the commit marker — `pending` drops a
    /// fragment without one as an incomplete (non-durable) append.
    pub fn append(&self, entry: &OutboxEntry) -> Result<(), String> {
        let _guard = OUTBOX_LOCK
            .lock()
            .map_err(|_| "outbox lock poisoned".to_string())?;
        let mut line = serde_json::to_string(entry)
            .map_err(|e| format!("outbox entry could not be serialised: {e}"))?;
        line.push('\n');
        let wal = self.dir.join(OUTBOX_FILE_NAME);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal)
            .map_err(|e| format!("outbox WAL {} could not be opened: {e}", wal.display()))?;
        file.write_all(line.as_bytes())
            .map_err(|e| format!("outbox WAL append failed: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("outbox WAL sync failed: {e}"))?;
        Ok(())
    }

    /// Read the pending entries in stable (append) order. A trailing fragment
    /// — a line cut off before its newline by a crash mid-append — is dropped
    /// rather than read as a corrupt complete line, because it was never
    /// durably committed to the WAL.
    pub fn pending(&self) -> Result<Vec<OutboxEntry>, String> {
        let wal = self.dir.join(OUTBOX_FILE_NAME);
        let bytes = match std::fs::read(&wal) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "outbox WAL {} could not be read: {e}",
                    wal.display()
                ))
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut segments: Vec<&str> = text.split('\n').collect();
        // The last segment is either "" (the file ends in a newline — clean) or
        // a fragment from an interrupted append — dropped either way.
        segments.pop();
        let mut entries = Vec::with_capacity(segments.len());
        for line in segments {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: OutboxEntry = serde_json::from_str(line)
                .map_err(|e| format!("corrupt outbox WAL line ({}): {e}", self.dir.display()))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Mark entries committed by rewriting the WAL without them. Atomic
    /// (tmp-write + rename) and stable-ordered. Called by the worker *after*
    /// the git commit succeeds; a crash before this line leaves the entries
    /// pending and the worker's idempotent apply makes the retry a no-op.
    pub fn mark_committed(&self, ids: &[Uuid]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let _guard = OUTBOX_LOCK
            .lock()
            .map_err(|_| "outbox lock poisoned".to_string())?;
        let remaining: Vec<OutboxEntry> = self
            .pending()?
            .into_iter()
            .filter(|e| !ids.contains(&e.stable_op_id))
            .collect();
        let wal = self.dir.join(OUTBOX_FILE_NAME);
        let tmp = self.dir.join(format!("{OUTBOX_FILE_NAME}.tmp"));
        {
            let mut file = std::fs::File::create(&tmp)
                .map_err(|e| format!("outbox tmp {} could not be created: {e}", tmp.display()))?;
            for entry in &remaining {
                let mut line = serde_json::to_string(entry)
                    .map_err(|e| format!("outbox entry could not be serialised: {e}"))?;
                line.push('\n');
                file.write_all(line.as_bytes())
                    .map_err(|e| format!("outbox compaction write failed: {e}"))?;
            }
            file.sync_all()
                .map_err(|e| format!("outbox compaction sync failed: {e}"))?;
        }
        std::fs::rename(&tmp, &wal).map_err(|e| format!("outbox compaction rename failed: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-outbox-{}-{}-{}",
            std::process::id(),
            tag,
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(op: &str, id: &str) -> OutboxEntry {
        OutboxEntry {
            stable_op_id: Uuid::new_v4(),
            op: op.to_string(),
            id: id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            content: "text".to_string(),
            metadata: serde_json::json!({}),
            superseded_by: None,
            ts: 1_700_000_000,
            epoch: 1,
        }
    }

    #[test]
    fn the_outbox_dir_is_a_sibling_of_the_checkout() {
        let mesh_dir = PathBuf::from("/base/some-mesh-id");
        let outbox = outbox_dir_for(&mesh_dir);
        assert_eq!(outbox, PathBuf::from("/base/some-mesh-id.outbox"));
        assert!(
            !outbox.starts_with(&mesh_dir),
            "the outbox must live OUTSIDE the git repo"
        );
    }

    #[test]
    fn append_is_durable_and_pending_returns_stable_order() {
        let dir = scratch("append");
        let outbox = Outbox::open(&dir).unwrap();
        let a = entry(OP_CREATED, "mem-a");
        let b = entry(OP_ARCHIVED, "mem-b");
        outbox.append(&a).unwrap();
        outbox.append(&b).unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(
            pending,
            vec![a.clone(), b.clone()],
            "entries come back in append order"
        );
        assert_eq!(outbox.pending().unwrap().len(), 2);
    }

    #[test]
    fn a_fragment_without_a_newline_is_not_a_durable_entry() {
        let dir = scratch("fragment");
        let outbox = Outbox::open(&dir).unwrap();
        let a = entry(OP_CREATED, "mem-a");
        outbox.append(&a).unwrap();

        // Simulate a crash mid-append: a second line with no trailing newline.
        let mut line = serde_json::to_string(&entry(OP_ARCHIVED, "mem-b")).unwrap();
        line.push('x'); // any tail after a complete entry that lacks a newline
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(OUTBOX_FILE_NAME))
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(
            pending,
            vec![a],
            "the fragment is dropped, not read as corrupt"
        );
    }

    #[test]
    fn mark_committed_drops_only_the_named_ids() {
        let dir = scratch("commit");
        let outbox = Outbox::open(&dir).unwrap();
        let a = entry(OP_CREATED, "mem-a");
        let b = entry(OP_ARCHIVED, "mem-b");
        let c = entry(OP_CREATED, "mem-c");
        outbox.append(&a).unwrap();
        outbox.append(&b).unwrap();
        outbox.append(&c).unwrap();

        outbox
            .mark_committed(&[a.stable_op_id, c.stable_op_id])
            .unwrap();
        assert_eq!(
            outbox.pending().unwrap(),
            vec![b.clone()],
            "b survives, a and c are gone"
        );
    }

    #[test]
    fn mark_committed_is_idempotent_and_preserves_new_appends() {
        let dir = scratch("idem");
        let outbox = Outbox::open(&dir).unwrap();
        let a = entry(OP_CREATED, "mem-a");
        outbox.append(&a).unwrap();

        // Commit a, then append b — the compaction must keep b even though it
        // landed after the first pending() read a caller might have taken.
        outbox.mark_committed(&[a.stable_op_id]).unwrap();
        let b = entry(OP_ARCHIVED, "mem-b");
        outbox.append(&b).unwrap();
        outbox.mark_committed(&[a.stable_op_id]).unwrap(); // a is already gone: no-op

        assert_eq!(
            outbox.pending().unwrap(),
            vec![b],
            "a committed, b retained"
        );
    }

    #[test]
    fn shareable_types_are_only_rule_fact_procedure() {
        for t in ["rule", "fact", "procedure"] {
            assert!(is_shareable_memory_type(t), "{t} is shareable");
        }
        for t in [
            "task",
            "directory",
            "file",
            "markdown",
            "symbol",
            "guard_event",
            "freshness_flag",
            "context",
        ] {
            assert!(!is_shareable_memory_type(t), "{t} is NOT shareable");
        }
    }
}
