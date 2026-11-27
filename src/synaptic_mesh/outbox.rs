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

/// The signed MeshFrame log file inside the outbox directory for cursor catch-up.
pub const FRAME_WAL_FILE_NAME: &str = "frames.wal";

/// The sidecar remembering `bus_seq`s whose entries were committed (drained)
/// before the event bus advanced its durable offset. A replay after a crash
/// must still be recognised as a duplicate even though the entry is no longer
/// pending in the WAL.
pub const COMMITTED_SEQ_FILE_NAME: &str = "outbox.committed.json";

/// How many committed `bus_seq`s the sidecar remembers. The set is a safety
/// window, not history: a replay older than the window is re-applied, and the
/// mesh worker's record-level idempotency (`stable_op_id`) still keeps it a
/// no-op.
const MAX_COMMITTED_SEQS: usize = 4096;

/// Op tags. The WAL is internal, so the op is a string field rather than a
/// tagged enum: it survives a format reader that predates a new op, and the
/// worker matches these constants (an unknown op is skipped, not a crash).
pub const OP_CREATED: &str = "created";
pub const OP_SUPERSEDED: &str = "superseded";
pub const OP_ARCHIVED: &str = "archived";

/// Whether `memory_type` is a type the mesh may carry. The predicate lives in
/// the crate-free `shareable` module so the privacy egress test exercises this
/// exact production function rather than a drifting copy of the allowlist.
pub use super::shareable::is_shareable_memory_type;

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
    /// The ThalamicBus event-outbox sequence that produced this entry, when
    /// the pulse arrived over the durable delivery path. `MeshOutbox` skips an
    /// entry whose sequence is already pending, so a redelivery after a crash
    /// between the capture and the bus offset write does not append a
    /// duplicate. `#[serde(default)]` reads a WAL line written before this
    /// field existed as `None` (no dedup basis, applied idempotently as
    /// before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_seq: Option<u64>,
    /// Generation-scoped, per-effect identity of the durable delivery. A
    /// `Superseded` pulse emits two entries with the same `bus_seq`; the
    /// retirement and replacement legs carry different keys so a crash between
    /// their appends cannot suppress either on replay. `serde(default)` keeps
    /// pre-hardening WAL lines readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_key: Option<String>,
}

/// Serialises `append` and `mark_committed` so a compaction can never lose an
/// entry an append landed mid-flight (see module docs' "Concurrency").
static OUTBOX_LOCK: Mutex<()> = Mutex::new(());

/// Owner-only directory creation and file modes on Unix. Audit and mesh WALs
/// carry memory content and must not be readable by other local users.
fn create_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("outbox dir {} could not be created: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("outbox dir {} could not be tightened: {e}", dir.display()))?;
    }
    Ok(())
}

fn open_private_append(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("outbox WAL {} could not be opened: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("outbox WAL {} could not be tightened: {e}", path.display()))?;
    }
    Ok(file)
}

fn create_private_file(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("outbox tmp {} could not be created: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("outbox tmp {} could not be tightened: {e}", path.display()))?;
    }
    Ok(file)
}

/// Best-effort parent-directory sync so a rename survives a power loss.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// The committed-sequence sidecar's shape (see [`COMMITTED_SEQ_FILE_NAME`]).
#[derive(Debug, Default, Serialize, Deserialize)]
struct CommittedSeqs {
    #[serde(default)]
    seqs: Vec<u64>,
    /// Generation-scoped per-leg keys (see [`OutboxEntry::bus_key`]). Older
    /// sidecars carry only `seqs` and keep working through the legacy check.
    #[serde(default)]
    keys: Vec<String>,
}

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
        create_private_dir(dir)?;
        let wal = dir.join(OUTBOX_FILE_NAME);
        let _probe = open_private_append(&wal)?;
        sync_dir(dir);
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
        // Idempotency across a commit: the worker may have drained this
        // sequence (and recorded it in the committed sidecar) before the bus
        // wrote its offset. Re-appending would duplicate the mesh record.
        if let Some(key) = &entry.bus_key {
            if self.is_bus_key_committed(key)? {
                tracing::debug!(
                    key,
                    "mesh outbox: skipping an append for an already-committed delivery leg"
                );
                return Ok(());
            }
        } else if let Some(seq) = entry.bus_seq {
            if self.is_bus_seq_committed(seq)? {
                tracing::debug!(
                    seq,
                    "mesh outbox: skipping an append for an already-committed bus sequence"
                );
                return Ok(());
            }
        }
        let mut line = serde_json::to_string(entry)
            .map_err(|e| format!("outbox entry could not be serialised: {e}"))?;
        line.push('\n');
        let wal = self.dir.join(OUTBOX_FILE_NAME);
        // A previous failed append can have left a fragment without its final
        // newline. `pending` ignores it; appending after it would glue two
        // records into one corrupt line. Truncate back to the last complete
        // newline while holding the process-global lock.
        let bytes = std::fs::read(&wal)
            .map_err(|e| format!("outbox WAL {} could not be read: {e}", wal.display()))?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            let clean_len = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(0);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&wal)
                .map_err(|e| format!("outbox WAL {} could not be repaired: {e}", wal.display()))?;
            file.set_len(clean_len as u64)
                .map_err(|e| format!("outbox WAL truncate failed: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("outbox WAL repair sync failed: {e}"))?;
            tracing::warn!(
                "mesh outbox: truncated {} trailing byte(s) from an incomplete append",
                bytes.len() - clean_len
            );
        }
        let mut file = open_private_append(&wal)?;
        let prior_len = file
            .metadata()
            .map_err(|e| format!("outbox WAL metadata failed: {e}"))?
            .len();
        if let Err(error) = file
            .write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
        {
            // Roll back the partial line so a retry starts at a clean boundary
            // instead of relying on this truncate-on-next-append repair.
            let _ = file.set_len(prior_len);
            let _ = file.sync_all();
            return Err(format!("outbox WAL append failed: {error}"));
        }
        Ok(())
    }

    /// The committed `bus_seq` window, read tolerantly: a missing or corrupt
    /// sidecar reads as empty, which falls back to `pending` dedup and the
    /// worker's record-level idempotency rather than failing the write.
    fn read_committed(&self) -> CommittedSeqs {
        let path = self.dir.join(COMMITTED_SEQ_FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CommittedSeqs::default(),
            Err(e) => {
                tracing::warn!(
                    "outbox committed-seq file {} unreadable ({e}); treating the window as empty",
                    path.display()
                );
                return CommittedSeqs::default();
            }
        };
        match serde_json::from_str::<CommittedSeqs>(&text) {
            Ok(committed) => committed,
            Err(e) => {
                tracing::warn!(
                    "outbox committed-seq file {} unparseable ({e}); treating the window as empty",
                    path.display()
                );
                CommittedSeqs::default()
            }
        }
    }

    fn read_committed_seqs(&self) -> Vec<u64> {
        self.read_committed().seqs
    }

    /// Whether this generation-scoped per-leg key was already drained. A
    /// replay of one leg cannot suppress its sibling: keys are distinct.
    pub fn is_bus_key_committed(&self, key: &str) -> Result<bool, String> {
        Ok(self.read_committed().keys.iter().any(|seen| seen == key))
    }

    /// Whether `seq` was committed (drained) by the worker. The subscriber
    /// checks this in addition to [`Outbox::pending`]: an entry committed
    /// before the bus advanced its offset is no longer pending, but a replay
    /// must still not append it again.
    pub fn is_bus_seq_committed(&self, seq: u64) -> Result<bool, String> {
        Ok(self.read_committed_seqs().contains(&seq))
    }

    /// Record committed `bus_seq`s durably. The caller holds [`OUTBOX_LOCK`].
    /// The window is sorted, deduplicated, and capped to the most recent
    /// [`MAX_COMMITTED_SEQS`].
    fn record_committed_locked(&self, seqs: &[u64], keys: &[String]) -> Result<(), String> {
        if seqs.is_empty() && keys.is_empty() {
            return Ok(());
        }
        let mut committed = self.read_committed();
        committed.seqs.extend_from_slice(seqs);
        committed.seqs.sort_unstable();
        committed.seqs.dedup();
        committed.keys.extend_from_slice(keys);
        committed.keys.sort();
        committed.keys.dedup();
        if committed.seqs.len() > MAX_COMMITTED_SEQS {
            let excess = committed.seqs.len() - MAX_COMMITTED_SEQS;
            committed.seqs.drain(0..excess);
        }
        if committed.keys.len() > MAX_COMMITTED_SEQS {
            let excess = committed.keys.len() - MAX_COMMITTED_SEQS;
            committed.keys.drain(0..excess);
        }
        let payload = serde_json::to_string(&committed)
            .map_err(|e| format!("outbox committed-seq encoding failed: {e}"))?;
        let tmp = self.dir.join(format!("{COMMITTED_SEQ_FILE_NAME}.tmp"));
        {
            let mut file = create_private_file(&tmp)?;
            file.write_all(payload.as_bytes())
                .map_err(|e| format!("outbox committed-seq write failed: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("outbox committed-seq sync failed: {e}"))?;
        }
        std::fs::rename(&tmp, self.dir.join(COMMITTED_SEQ_FILE_NAME))
            .map_err(|e| format!("outbox committed-seq rename failed: {e}"))?;
        sync_dir(&self.dir);
        Ok(())
    }

    /// Read the pending entries in stable (append) order. A trailing fragment
    /// — a line cut off before its newline by a crash mid-append — is dropped
    /// rather than read as a corrupt complete line, because it was never
    /// durably committed to the WAL. A corrupt complete line is likewise
    /// never fatal: it is logged (warn) and skipped, so a single bad line
    /// cannot wedge `drain_once`/`mark_committed`; the diagnostics stay on
    /// disk untouched.
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
            // Fail-soft: one corrupt line must not permanently wedge the
            // drain loop (mark_committed re-reads pending(), so an Err here
            // would block every retry). Diagnostics are preserved, never
            // deleted, matching the status surface's contract — the line
            // stays on disk and is skipped.
            let entry: OutboxEntry = match serde_json::from_str(line) {
                Ok(entry) => entry,
                Err(e) => {
                    tracing::warn!(
                        "skipping corrupt outbox WAL line in {}: {e}",
                        self.dir.display()
                    );
                    continue;
                }
            };
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Mark entries committed by rewriting the WAL without them. Atomic
    /// (tmp-write + rename) and stable-ordered. Called by the worker *after*
    /// the git commit succeeds; a crash before this line leaves the entries
    /// pending and the worker's idempotent apply makes the retry a no-op.
    ///
    /// The removed entries' `bus_seq`s are recorded in the committed sidecar
    /// *before* the WAL rewrite: a crash in between leaves them pending (the
    /// subscriber's pending dedup still covers that), while a marker without
    /// removal is harmless. This is what keeps a bus replay from re-appending
    /// an entry the worker already committed but the event bus had not yet
    /// acknowledged.
    pub fn mark_committed(&self, ids: &[Uuid]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let _guard = OUTBOX_LOCK
            .lock()
            .map_err(|_| "outbox lock poisoned".to_string())?;
        let pending = self.pending()?;
        let committed: Vec<&OutboxEntry> = pending
            .iter()
            .filter(|e| ids.contains(&e.stable_op_id))
            .collect();
        let committed_seqs: Vec<u64> = committed.iter().filter_map(|e| e.bus_seq).collect();
        let committed_keys: Vec<String> = committed
            .iter()
            .filter_map(|e| e.bus_key.clone())
            .collect();
        self.record_committed_locked(&committed_seqs, &committed_keys)?;
        let remaining: Vec<OutboxEntry> = pending
            .into_iter()
            .filter(|e| !ids.contains(&e.stable_op_id))
            .collect();
        let wal = self.dir.join(OUTBOX_FILE_NAME);
        let tmp = self.dir.join(format!("{OUTBOX_FILE_NAME}.tmp"));
        {
            let mut file = create_private_file(&tmp)?;
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
        sync_dir(&self.dir);
        Ok(())
    }

    /// Append a signed [`MeshFrame`](super::frame::MeshFrame) to `frames.wal`
    /// with `sync_all`, returning `frame.seq`.
    pub fn append_frame(&self, frame: &super::frame::MeshFrame) -> anyhow::Result<u64> {
        let _guard = OUTBOX_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("outbox lock poisoned"))?;
        let mut line = serde_json::to_string(frame)?;
        line.push('\n');
        let wal = self.dir.join(FRAME_WAL_FILE_NAME);
        let mut file = open_private_append(&wal).map_err(anyhow::Error::msg)?;
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        Ok(frame.seq)
    }

    fn read_all_frames(&self) -> anyhow::Result<Vec<super::frame::MeshFrame>> {
        let wal = self.dir.join(FRAME_WAL_FILE_NAME);
        let bytes = match std::fs::read(&wal) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut segments: Vec<&str> = text.split('\n').collect();
        segments.pop();
        let mut frames = Vec::with_capacity(segments.len());
        for line in segments {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(frame) = serde_json::from_str::<super::frame::MeshFrame>(line) {
                frames.push(frame);
            }
        }
        Ok(frames)
    }

    /// Replay frames originating from `origin_node_id` with `seq` in
    /// `from_seq_inclusive..=to_seq_inclusive`, sorted by `seq` ASC and deduplicated.
    pub fn replay_frames_range(
        &self,
        origin_node_id: &str,
        from_seq_inclusive: u64,
        to_seq_inclusive: u64,
    ) -> anyhow::Result<Vec<super::frame::MeshFrame>> {
        let mut by_seq = std::collections::BTreeMap::new();
        for f in self.read_all_frames()? {
            if f.origin_node_id == origin_node_id
                && f.seq >= from_seq_inclusive
                && f.seq <= to_seq_inclusive
            {
                by_seq.entry(f.seq).or_insert(f);
            }
        }
        Ok(by_seq.into_values().collect())
    }

    /// Return the highest `seq` recorded for `origin_node_id` in `frames.wal` (`0` if none).
    pub fn max_seq_for(&self, origin_node_id: &str) -> anyhow::Result<u64> {
        let max = self
            .read_all_frames()?
            .into_iter()
            .filter(|f| f.origin_node_id == origin_node_id)
            .map(|f| f.seq)
            .max()
            .unwrap_or(0);
        Ok(max)
    }

    /// Return the highest recorded `seq` per `origin_node_id` across `frames.wal`.
    pub fn all_peer_cursors(&self) -> anyhow::Result<std::collections::BTreeMap<String, u64>> {
        let mut cursors = std::collections::BTreeMap::new();
        for f in self.read_all_frames()? {
            let entry = cursors.entry(f.origin_node_id).or_insert(0);
            if f.seq > *entry {
                *entry = f.seq;
            }
        }
        Ok(cursors)
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
            Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(op: &str, id: &str) -> OutboxEntry {
        OutboxEntry {
            stable_op_id: Uuid::now_v7(),
            op: op.to_string(),
            id: id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            content: "text".to_string(),
            metadata: serde_json::json!({}),
            superseded_by: None,
            ts: 1_700_000_000,
            epoch: 1,
            bus_seq: None,
            bus_key: None,
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
    fn a_corrupt_complete_line_is_skipped_not_fatal() {
        let dir = scratch("corrupt-line");
        let outbox = Outbox::open(&dir).unwrap();
        let a = entry(OP_CREATED, "mem-a");
        outbox.append(&a).unwrap();

        // A complete line (trailing newline — a durable append in git terms of
        // the WAL format) whose payload is not valid JSON. Diagnostics are the
        // WAL itself: they stay on disk, never deleted, and are skipped so a
        // single bad line cannot wedge the drain loop.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(OUTBOX_FILE_NAME))
            .unwrap()
            .write_all(b"this is not an outbox entry\n")
            .unwrap();
        let c = entry(OP_ARCHIVED, "mem-c");
        outbox.append(&c).unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(
            pending,
            vec![a.clone(), c.clone()],
            "valid entries around the corrupt line survive, nothing wedges"
        );
        // mark_committed keeps working; the corrupt line is never rewritten in.
        outbox.mark_committed(&[c.stable_op_id]).unwrap();
        assert_eq!(
            outbox.pending().unwrap(),
            vec![a],
            "commit still removes only the named ids"
        );
    }

    #[test]
    fn shareable_types_are_only_rule_fact_procedure() {
        for t in ["rule", "fact", "procedure", "action_type"] {
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
            "action_receipt",
        ] {
            assert!(!is_shareable_memory_type(t), "{t} is NOT shareable");
        }
    }

    #[test]
    fn frame_wal_appends_tracks_cursors_and_replays_range() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(&tmp.path().join("mesh.outbox")).unwrap();
        let (pkey, did, _) = crate::did::generate_keypair().unwrap();

        for seq in 1..=5u64 {
            let mut f = crate::synaptic_mesh::frame::MeshFrame {
                mesh_id: "m1".into(),
                epoch: 1,
                origin_node_id: did.clone(),
                seq,
                lamport_ts: seq * 10,
                timestamp_ms: 1_700_000_000_000 + (seq as i64),
                namespace: "ns".into(),
                visible_to: vec![],
                kind: crate::synaptic_mesh::frame::MeshPayloadKind::SyncHello(
                    crate::synaptic_mesh::frame::SyncHello {
                        node_id: did.clone(),
                        mesh_id: "m1".into(),
                        epoch: 1,
                        peer_cursors: std::collections::BTreeMap::new(),
                        endpoint: None,
                    },
                ),
                signature: String::new(),
            };
            f.sign_frame(&pkey).unwrap();
            outbox.append_frame(&f).unwrap();
        }

        assert_eq!(outbox.max_seq_for(&did).unwrap(), 5);
        let slice = outbox.replay_frames_range(&did, 2, 4).unwrap();
        let seqs: Vec<u64> = slice.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![2, 3, 4]);
        assert_eq!(outbox.all_peer_cursors().unwrap().get(&did), Some(&5));
    }
}
