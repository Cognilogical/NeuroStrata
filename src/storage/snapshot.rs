use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SNAPSHOT_FORMAT: &str = "neurostrata-snapshot";

/// The snapshot format this build writes (#209, J7).
///
/// Version 2 records `wal_kind` (mandatory) and may carry a second WAL member
/// (`ladybug.store.wal.checkpoint`) for a mixed pair. Older builds pin
/// `version == 1`, so they refuse a v2 snapshot outright instead of verifying
/// it and then silently dropping the frozen side on restore.
pub const SNAPSHOT_VERSION: u32 = 2;

/// The only format version written before #209, kept readable forever. A v1
/// manifest has no `wal_kind`: its WAL member is the active `<db>.wal`.
pub const SNAPSHOT_VERSION_LEGACY: u32 = 1;

/// Whether this build can read `version` (legacy through current).
pub fn snapshot_version_supported(version: u32) -> bool {
    (SNAPSHOT_VERSION_LEGACY..=SNAPSHOT_VERSION).contains(&version)
}

/// Manifest provenance (#209).
///
/// `coordinated` means the export ran while this process held the store (the
/// online `export_database` path, checkpoint included), so the database and
/// its companions were captured under one lock. `unverified` means the files
/// were copied straight off disk (`backup --no-open`, or its fallback when
/// strict WAL verification refuses the open): the bytes are checksummed, but
/// nothing proves the database, WAL, and shadow came from the same instant.
pub const PROVENANCE_COORDINATED: &str = "coordinated";
pub const PROVENANCE_UNVERIFIED: &str = "unverified";

/// The WAL role a snapshot captured (#209): the active `<db>.wal`, the frozen
/// `<db>.wal.checkpoint` a crashed checkpoint left behind, both sides of a
/// mixed pair, or no WAL at all.
pub const WAL_KIND_ACTIVE: &str = "active";
pub const WAL_KIND_FROZEN: &str = "frozen";
pub const WAL_KIND_BOTH: &str = "both";
pub const WAL_KIND_NONE: &str = "none";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageHygieneConfig {
    pub checkpoint_interval_secs: u64,
    pub wal_warn_bytes: u64,
    pub snapshot_retention_count: usize,
    /// Cap on the database engine's buffer pool, in MB. The pool caches pages
    /// of the database file; left to itself the engine sizes it to most of the
    /// machine's memory, so the daemon's resident memory climbs toward the size
    /// of the whole store. 0 hands the sizing back to the engine. Values below
    /// the floor (see `store::ladybug`) are raised to it, because a pool too
    /// small for one query makes that query fail.
    pub buffer_pool_mb: u64,
}

impl Default for StorageHygieneConfig {
    fn default() -> Self {
        Self {
            checkpoint_interval_secs: 2,
            wal_warn_bytes: 64 * 1024 * 1024,
            snapshot_retention_count: 5,
            // Measured on a copy of a 739 MB store, three passes over every
            // namespace: 384 MB queries ran as fast as the engine default
            // (within run-to-run noise) and cut peak memory by about 15%;
            // 256 MB was 15 to 30% slower and 192 MB about 50% slower.
            buffer_pool_mb: 384,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStatus {
    pub store_path: String,
    pub store_bytes: u64,
    pub wal_bytes: u64,
    pub wal_warn_threshold_exceeded: bool,
    pub snapshot_count: usize,
    pub retained_limit: usize,
    // Spec §5.5 (2026-10-28 storage-hygiene design).
    pub dirty: bool,
    pub checkpoint_interval_secs: u64,
    pub last_checkpoint_at: Option<String>,
    pub last_checkpoint_duration_ms: Option<u64>,
    pub checkpoint_failures: u32,
    pub snapshot_export_dir: String,
    /// Whether **this process's live handle** was opened through a WAL
    /// recovery fallback (checksum-mismatch reopen, unverified-WAL replay,
    /// or WAL-discard reopen) rather than the normal verified open. A
    /// recovered open's view of the store is unauthenticated; operators
    /// should treat it as degraded provenance. Defaults to `false`.
    ///
    /// Handle-scoped by design (`#212`): it is `false` whenever no live
    /// handle in this process recorded a recovery, even when the store-state
    /// sidecar holds one. Use [`Self::last_recovery_at`] for "did this store
    /// ever recover?"; the sidecar's history must not make a verified daemon
    /// look degraded.
    #[serde(default)]
    pub opened_via_recovery: bool,
    /// Which recovery arm opened the store (`checksum_config_mismatch`,
    /// `unverified_wal_replay`, or `discarded_unreadable_wal`) when opened via
    /// recovery (#194). Live layer first: this process's kind when it has
    /// one, otherwise the persisted `last_recovery.kind` — the answer a
    /// path-only `storage status` can give. See also
    /// [`Self::last_recovery_at`], which is always history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_provenance: Option<String>,
    /// RFC 3339 UTC timestamp of the store's last recorded recovery open
    /// (`#212`), from the store-state sidecar. History, not live state: it
    /// survives a verified reopen, so it answers "did this store ever
    /// recover?" for a live daemon and a path-only query alike. `None` when
    /// no recovery is recorded (or the store has no sidecar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recovery_at: Option<String>,
    /// Number of preserved `.wal.unclean.*` / `.wal.corrupted.*` recovery
    /// artifacts sitting beside the store (#195).
    #[serde(default)]
    pub wal_recovery_artifact_count: usize,
    /// Total bytes across all preserved `.wal.unclean.*` / `.wal.corrupted.*`
    /// recovery artifacts sitting beside the store (#195).
    #[serde(default)]
    pub wal_recovery_artifact_bytes: u64,
    /// Age in whole seconds of the newest active/frozen WAL file beside the
    /// store (`<store>.wal`, `<store>.wal.checkpoint`; #212). `null` when
    /// neither exists. Measured over regular files only: a symlink at either
    /// name is not a WAL this store wrote and contributes no age, while
    /// `wal_bytes` (unchanged by #212) follows the names as the engine does
    /// and therefore can count a symlink's target. Read the pair as "how
    /// long has this store been dirty", remembering that the two fields do
    /// not necessarily describe the same file set.
    ///
    /// Serialized even when absent, as `"wal_age_secs": null`: the runbook
    /// promises the key, and a consumer polling a healthy (WAL-free) store
    /// must see the field it would see on a dirty one.
    #[serde(default)]
    pub wal_age_secs: Option<u64>,
    /// Whether this process stopped checkpointing because one ran out of
    /// buffer pool (#211). While set, the engine's `auto_checkpoint` and
    /// `force_checkpoint_on_close` are off on the shared database config, so
    /// writes stay in the write-ahead log until a later checkpoint succeeds or
    /// the process is restarted with a larger pool. Defaults to `false` so
    /// older clients deserialize the status unchanged.
    #[serde(default)]
    pub checkpoints_stopped: bool,
    /// Checkpoints skipped because `checkpoints_stopped` was set (#211). A
    /// skip is not an attempt: it is counted here and never in
    /// `checkpoint_failures`, which only moves when a CHECKPOINT ran and
    /// failed. Monotonic for the life of the process.
    #[serde(default)]
    pub checkpoint_skips: u64,
}

static RECOVERY_PROVENANCE_BY_PATH: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, String>>,
> = std::sync::OnceLock::new();

/// Record (or clear when `None`) the recovery provenance for `store_path` (#194).
pub fn record_recovery_provenance(store_path: &Path, provenance: Option<&str>) {
    let map = RECOVERY_PROVENANCE_BY_PATH
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    match provenance {
        Some(kind) => {
            guard.insert(store_path.to_path_buf(), kind.to_string());
        }
        None => {
            guard.remove(store_path);
        }
    }
}

/// The RFC 3339 UTC timestamp the durable provenance records (`#212`).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Record a recovery open for `store_path` (#194, #212).
///
/// Two layers, deliberately: the process-local map is the live fallback
/// (it answers for a store with no sidecar, and for the process that just
/// opened it), and the store-state sidecar holds the durable record a
/// fresh, path-only `storage status` reports. A `None` provenance clears
/// only the live layer: the sidecar's `last_recovery` records history, and
/// a later verified open does not rewrite what happened.
pub fn record_recovery_open(store_path: &Path, provenance: Option<&str>, wal_bytes: u64) {
    record_recovery_provenance(store_path, provenance);
    let Some(kind) = provenance else {
        return;
    };
    match crate::model_choice::record_store_state_recovery(
        store_path,
        kind,
        wal_bytes,
        &now_rfc3339(),
    ) {
        // The store has no recorded sidecar (fresh or pre-sidecar): the
        // in-process map is its only provenance layer, and inventing a
        // sidecar here would claim an identity this process cannot attest.
        Ok(true) => {}
        Ok(false) => tracing::debug!(
            "recovery provenance for {} stays process-local: no recorded store-state sidecar to extend",
            store_path.display()
        ),
        Err(e) => tracing::warn!("store-state recovery provenance write failed: {e}"),
    }
}

/// Look up any recorded recovery provenance for `store_path` (#194).
pub fn recorded_recovery_provenance(store_path: &Path) -> Option<String> {
    let map = RECOVERY_PROVENANCE_BY_PATH
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.get(store_path).cloned()
}

/// Whether `candidate_name` is a WAL recovery artifact (`.wal.unclean.*`,
/// `.wal.checkpoint.unclean.*`, or `.wal.corrupted.*`) belonging to
/// `store_file_name` (#195).
pub fn is_wal_recovery_artifact_name(store_file_name: &str, candidate_name: &str) -> bool {
    if store_file_name.is_empty() {
        return false;
    }
    let Some(rest) = candidate_name.strip_prefix(store_file_name) else {
        return false;
    };
    rest.starts_with(".wal.unclean.")
        || rest.starts_with(".wal.checkpoint.unclean.")
        || rest.starts_with(".wal.corrupted.")
        || rest.starts_with(".wal.checkpoint.corrupted.")
}

/// List regular WAL recovery artifact files (`path`, `bytes`, `mtime`) beside
/// `store_path`, refusing symlinks and directories (#193, #195).
pub fn list_wal_recovery_artifacts(
    store_path: &Path,
) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let Some(store_name) = store_path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let parent = store_path.parent().unwrap_or_else(|| Path::new("."));
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !is_wal_recovery_artifact_name(store_name, name_str) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        out.push((path, meta.len(), mtime));
    }
    out.sort_by(|a, b| {
        b.2.cmp(&a.2).then_with(|| {
            b.0.file_name()
                .unwrap_or_default()
                .cmp(a.0.file_name().unwrap_or_default())
        })
    });
    out
}

/// Prune WAL recovery artifacts beside `store_path` beyond `keep` (clamped to
/// at least 1), returning the removed paths (#195).
pub fn prune_wal_recovery_artifacts(store_path: &Path, keep: usize) -> Vec<PathBuf> {
    prune_wal_recovery_artifacts_except(store_path, keep, &[])
}

/// Prune WAL recovery artifacts beyond `keep` **without ever removing a path in
/// `keep_paths`**, returning the removed paths (#207).
///
/// The retained set is the newest `keep` artifacts (as
/// [`prune_wal_recovery_artifacts`] does) **plus** every protected path, so a
/// caller that has just recorded copies of the current WAL pair cannot have one
/// of them deleted by a retention budget of 1. Protected paths that already fall
/// inside the newest `keep` are not double-counted; ones outside it simply
/// survive in addition.
pub fn prune_wal_recovery_artifacts_except(
    store_path: &Path,
    keep: usize,
    keep_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let keep = keep.max(1);
    let protected: std::collections::HashSet<&Path> =
        keep_paths.iter().map(|p| p.as_path()).collect();
    let mut removed = Vec::new();
    for (index, (path, _, _)) in list_wal_recovery_artifacts(store_path).iter().enumerate() {
        if index < keep || protected.contains(path.as_path()) {
            continue;
        }
        if fs::remove_file(path).is_ok() {
            removed.push(path.clone());
        }
    }
    removed
}

/// Duration of the most recent checkpoint, in milliseconds. Cross-task record
/// because checkpoints are driven by whichever task lands the write.
static LAST_CHECKPOINT_DURATION_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Unix-seconds timestamp of the most recent checkpoint recorded in this
/// process (0 = none recorded yet). The directory mtime is only a proxy;
/// this records the real event the moment it happens.
static LAST_CHECKPOINT_AT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Count of failed checkpoints since process start (monotonic, never reset).
static CHECKPOINT_FAILURES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The checkpoint latch as `compute_storage_status` can see it (#211): whether
/// the store stopped checkpointing, and how many checkpoints that stopped.
///
/// The latch itself lives in `store::ladybug` as a process-local atomic on the
/// store handle. This is the mirror the status read consults, because
/// `compute_storage_status` is handed a path and nothing else. Keyed by path so
/// two stores opened in one process -- the test suite, a backup export -- never
/// report each other's latch.
#[derive(Debug, Clone, Copy, Default)]
struct CheckpointLatchStatus {
    stopped: bool,
    skips: u64,
}

static CHECKPOINT_LATCH_BY_PATH: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, CheckpointLatchStatus>>,
> = std::sync::OnceLock::new();

type CheckpointLatchMap =
    std::sync::Mutex<std::collections::HashMap<PathBuf, CheckpointLatchStatus>>;

fn checkpoint_latch_map() -> &'static CheckpointLatchMap {
    CHECKPOINT_LATCH_BY_PATH.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Record that this process stopped (or resumed) checkpointing `store_path`
/// after a checkpoint ran out of buffer pool (#211). Resuming does not reset
/// the skip count: it is a running total for the process, like
/// `checkpoint_failures`.
pub fn record_checkpoints_stopped(store_path: &Path, stopped: bool) {
    let map = checkpoint_latch_map();
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(store_path.to_path_buf()).or_default().stopped = stopped;
}

/// Count one checkpoint that the latch skipped (#211). Separate from
/// `record_checkpoint_outcome` on purpose: a skip issues no CHECKPOINT, so
/// counting it as a failure would misreport the store as repeatedly trying.
pub fn record_checkpoint_skip(store_path: &Path) {
    let map = checkpoint_latch_map();
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    let entry = guard.entry(store_path.to_path_buf()).or_default();
    entry.skips = entry.skips.saturating_add(1);
}

fn recorded_checkpoint_latch(store_path: &Path) -> CheckpointLatchStatus {
    let map = checkpoint_latch_map();
    let guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.get(store_path).copied().unwrap_or_default()
}

/// Record a finished checkpoint. Called from the store's checkpoint path so
/// `compute_storage_status` reports real hygiene data instead of constants.
///
/// The outcome is also persisted to the store-state sidecar (`#212`), so a
/// fresh process — where these atomics read 0 — still reports the last real
/// checkpoint and its failure count. A sidecar write failure is logged, not
/// propagated: a checkpoint that succeeded must not be reported as failed
/// because its bookkeeping could not be written.
pub fn record_checkpoint_outcome(store_path: &Path, duration_ms: u64, ok: bool) {
    use std::sync::atomic::Ordering;
    LAST_CHECKPOINT_DURATION_MS.store(duration_ms, Ordering::Relaxed);
    LAST_CHECKPOINT_AT_SECS.store(
        chrono::Utc::now().timestamp().max(0) as u64,
        Ordering::Relaxed,
    );
    if !ok {
        CHECKPOINT_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    if let Err(e) = crate::model_choice::record_store_state_checkpoint(
        store_path,
        &now_rfc3339(),
        duration_ms,
        ok,
    ) {
        tracing::warn!("store-state checkpoint outcome write failed: {e}");
    }
}

pub fn compute_storage_status(store_path: &Path, config: &StorageHygieneConfig) -> StorageStatus {
    let store_bytes = fs::metadata(store_path).map(|m| m.len()).unwrap_or(0);
    let mut wal_os = store_path.as_os_str().to_os_string();
    wal_os.push(".wal");
    let wal_path = PathBuf::from(wal_os);
    let mut ckpt_wal_os = store_path.as_os_str().to_os_string();
    ckpt_wal_os.push(".wal.checkpoint");
    let ckpt_wal_path = PathBuf::from(ckpt_wal_os);
    let wal_bytes = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
        + fs::metadata(&ckpt_wal_path).map(|m| m.len()).unwrap_or(0);
    let wal_age_secs = wal_age_secs(&wal_path, &ckpt_wal_path);
    let wal_artifacts = list_wal_recovery_artifacts(store_path);
    let wal_recovery_artifact_count = wal_artifacts.len();
    let wal_recovery_artifact_bytes: u64 = wal_artifacts.iter().map(|(_, len, _)| *len).sum();
    let checkpoint_latch = recorded_checkpoint_latch(store_path);
    // Durable provenance (#212): the store-state sidecar survives the
    // process that recovered, so a fresh, path-only status reports the last
    // recovery and the recorded checkpoint health. The in-process map stays
    // authoritative for this process (it is the live event); the sidecar is
    // the fallback when the map has nothing.
    let persisted = match crate::model_choice::read_store_state(store_path) {
        crate::model_choice::StoreStateRead::Recorded(state) => Some(state),
        // Absence is legitimate (fresh or pre-sidecar stores); an
        // unparseable sidecar is an open-time refusal elsewhere and must not
        // turn a status query into one.
        crate::model_choice::StoreStateRead::Absent
        | crate::model_choice::StoreStateRead::Unparseable(_) => None,
    };
    let live_recovery_provenance = recorded_recovery_provenance(store_path);
    let recovery_provenance = live_recovery_provenance.clone().or_else(|| {
        persisted
            .as_ref()
            .and_then(|state| state.last_recovery.as_ref())
            .map(|recovery| recovery.kind.clone())
    });
    let last_recovery_at = persisted
        .as_ref()
        .and_then(|state| state.last_recovery.as_ref())
        .map(|recovery| recovery.at.clone());
    let snapshot_dir = store_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let snapshot_read = snapshot_dir.read_dir();
    let snapshot_count = match snapshot_read {
        Ok(entries) => entries
            .filter_map(|res| match res {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!("snapshot directory entry read failed: {e}");
                    None
                }
            })
            .filter(|e| {
                e.path().is_dir()
                    && e.file_name()
                        .to_str()
                        .map(|s| s.starts_with("snapshot-"))
                        .unwrap_or(false)
            })
            .count(),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("snapshot read_dir failed: {e}");
            }
            0
        }
    };
    // Prefer the real checkpoint timestamp recorded in this process; then the
    // durable record a previous process left in the sidecar (#212); then the
    // store directory's mtime, which is only a proxy (and which remains the
    // answer for a store that has never recorded a checkpoint here).
    let dir_mtime = || {
        fs::metadata(&snapshot_dir)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                let dt = chrono::DateTime::<chrono::Utc>::from(std::time::UNIX_EPOCH + d);
                dt.to_rfc3339()
            })
    };
    let recorded_checkpoint_ts = LAST_CHECKPOINT_AT_SECS.load(std::sync::atomic::Ordering::Relaxed);
    let in_process_checkpoint_at = if recorded_checkpoint_ts > 0 {
        chrono::DateTime::from_timestamp(recorded_checkpoint_ts as i64, 0).map(|dt| dt.to_rfc3339())
    } else {
        None
    };
    let last_checkpoint_at = in_process_checkpoint_at
        .or_else(|| {
            persisted
                .as_ref()
                .and_then(|state| state.last_checkpoint_at.clone())
        })
        .or_else(dir_mtime);
    let checkpoint_failures = CHECKPOINT_FAILURES
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(
            persisted
                .as_ref()
                .map(|state| state.checkpoint_failures)
                .unwrap_or(0),
        );
    StorageStatus {
        store_path: store_path.display().to_string(),
        store_bytes,
        wal_bytes,
        wal_warn_threshold_exceeded: wal_bytes > config.wal_warn_bytes,
        snapshot_count,
        retained_limit: config.snapshot_retention_count.max(1),
        dirty: wal_bytes > 0,
        checkpoint_interval_secs: config.checkpoint_interval_secs,
        last_checkpoint_at,
        // Only a checkpoint that actually happened in this process claims a
        // duration from the atomics: before the first checkpoint they read 0,
        // and reporting `Some(0)` would be a fabricated metric. The persisted
        // record answers for a previous process.
        last_checkpoint_duration_ms: if recorded_checkpoint_ts > 0 {
            Some(LAST_CHECKPOINT_DURATION_MS.load(std::sync::atomic::Ordering::Relaxed))
        } else {
            persisted
                .as_ref()
                .and_then(|state| state.last_checkpoint_duration_ms)
        },
        checkpoint_failures,
        snapshot_export_dir: snapshot_dir.display().to_string(),
        // Handle-scoped (`#212`): only a live recovery in this process sets
        // it. The persisted history is `last_recovery_at` + the path-only
        // `recovery_provenance` fallback, so a verified daemon never looks
        // degraded because the store recovered in an earlier process.
        opened_via_recovery: live_recovery_provenance.is_some(),
        recovery_provenance,
        last_recovery_at,
        wal_recovery_artifact_count,
        wal_recovery_artifact_bytes,
        wal_age_secs,
        checkpoints_stopped: checkpoint_latch.stopped,
        checkpoint_skips: checkpoint_latch.skips,
    }
}

/// Age in whole seconds of the newest regular (non-symlink) WAL file of the
/// pair (#212), or `None` when neither exists.
fn wal_age_secs(active: &Path, frozen: &Path) -> Option<u64> {
    let newest = [active, frozen]
        .iter()
        .filter_map(|path| {
            let meta = fs::symlink_metadata(path).ok()?;
            if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                return None;
            }
            meta.modified().ok()
        })
        .max()?;
    // A WAL stamped in the future (clock skew, a copy that carried its
    // timestamps across) has no age yet. Report zero rather than letting
    // the subtraction wrap into a huge number (#241).
    let age = match std::time::SystemTime::now().duration_since(newest) {
        Ok(elapsed) => elapsed.as_secs(),
        Err(_) => 0,
    };
    Some(age)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotChecksums {
    pub ladybug_store_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wal_sha256: Option<String>,
    pub manifest_sha256: String,
    /// The store-state sidecar's checksum: present only
    /// when the snapshot carries `ladybug.store.store-state.json`, so
    /// snapshots written before sidecar carriage (and their checksum
    /// files) deserialize and verify exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_state_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wal_checkpoint_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotVerifyReport {
    /// Fully valid: the checksums match **and** the snapshot was taken under
    /// the store lock. Uncoordinated (`--no-open`) snapshots never set this.
    pub valid: bool,
    /// The copied bytes match `checksums.json` (#209).
    #[serde(default)]
    pub integrity_ok: bool,
    /// The manifest records that a daemon held the store for the export.
    #[serde(default)]
    pub coordinated: bool,
    /// `coordinated` or `unverified`, as recorded in the manifest.
    #[serde(default)]
    pub provenance: String,
    /// The WAL role the snapshot captured (`active`, `frozen`, or `none`).
    #[serde(default)]
    pub wal_kind: Option<String>,
    pub dir: PathBuf,
    pub checksums: Option<SnapshotChecksums>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum SnapshotError {
    Io(std::io::Error),
    Json(serde_json::Error),
    MissingFile(String),
    InvalidManifest(String),
    /// A snapshot member exists but is not the regular file it must be
    /// (#241): a directory in place of the store image, or a symlink.
    NotARegularFile(String),
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "io error: {}", e),
            SnapshotError::Json(e) => write!(f, "json error: {}", e),
            SnapshotError::MissingFile(s) => write!(f, "missing file: {}", s),
            SnapshotError::InvalidManifest(s) => write!(f, "invalid manifest: {}", s),
            SnapshotError::NotARegularFile(s) => write!(f, "not a regular file: {}", s),
            SnapshotError::ChecksumMismatch {
                file,
                expected,
                actual,
            } => write!(
                f,
                "checksum mismatch for {}: expected {}, got {}",
                file, expected, actual
            ),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotError::Io(e) => Some(e),
            SnapshotError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

impl From<serde_json::Error> for SnapshotError {
    fn from(e: serde_json::Error) -> Self {
        SnapshotError::Json(e)
    }
}

pub fn sha256_file(path: &Path) -> Result<String, SnapshotError> {
    let mut hasher = Sha256::new();
    if path.is_dir() {
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(path)?
            .filter_map(|res| match res {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!("snapshot directory entry read failed: {e}");
                    None
                }
            })
            .filter(|e| e.path().is_file() || e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for entry in entries {
            let p = entry.path();
            let mut file = File::open(&p)?;
            let mut buf = [0u8; 65536];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }
    } else {
        let mut file = File::open(path)?;
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    let digest = hasher.finalize();
    Ok(digest
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>())
}

pub fn write_checksums(dir: &Path) -> Result<SnapshotChecksums, SnapshotError> {
    let ladybug = dir.join("ladybug.store");
    let wal = dir.join("ladybug.store.wal");
    let manifest = dir.join("manifest.json");

    if !ladybug.exists() && !ladybug.is_dir() {
        return Err(SnapshotError::MissingFile("ladybug.store".into()));
    }
    if !manifest.exists() {
        return Err(SnapshotError::MissingFile("manifest.json".into()));
    }

    let ladybug_sha = sha256_file(&ladybug)?;
    let wal_sha = if wal.exists() {
        Some(sha256_file(&wal)?)
    } else {
        None
    };
    let manifest_sha = sha256_file(&manifest)?;
    let sidecar = dir.join("ladybug.store.store-state.json");
    let store_state_sha = if sidecar.exists() {
        Some(sha256_file(&sidecar)?)
    } else {
        None
    };
    // The checkpoint shadow and its pinned marker are part of a crashed
    // checkpoint's recovery state (#209): a tampered or truncated shadow must
    // fail verification, not restore as a torn checkpoint.
    let shadow = dir.join("ladybug.store.shadow");
    let shadow_sha = if shadow.exists() {
        Some(sha256_file(&shadow)?)
    } else {
        None
    };
    let pinned = dir.join("ladybug.store.pinned.tmp");
    let pinned_sha = if pinned.exists() {
        Some(sha256_file(&pinned)?)
    } else {
        None
    };
    // A mixed pair keeps its frozen side under its own member (#209): the
    // `ladybug.store.wal` member alone cannot hold both byte streams.
    let checkpoint_wal = dir.join("ladybug.store.wal.checkpoint");
    let wal_checkpoint_sha = if checkpoint_wal.exists() {
        Some(sha256_file(&checkpoint_wal)?)
    } else {
        None
    };

    let checksums = SnapshotChecksums {
        ladybug_store_sha256: ladybug_sha,
        wal_sha256: wal_sha,
        manifest_sha256: manifest_sha,
        store_state_sha256: store_state_sha,
        shadow_sha256: shadow_sha,
        pinned_sha256: pinned_sha,
        wal_checkpoint_sha256: wal_checkpoint_sha,
    };

    let checksums_path = dir.join("checksums.json");
    let mut file = File::create(&checksums_path)?;
    let json = serde_json::to_string_pretty(&checksums)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;

    // zstd-compressed manifest copy for space-efficient transport.
    if manifest.exists() {
        let raw = fs::read(&manifest)?;
        let compressed = crate::compression::zstd::compress_snapshot(&raw)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut zst = File::create(dir.join("manifest.json.zst"))?;
        zst.write_all(&compressed)?;
    }

    Ok(checksums)
}

pub fn verify_snapshot(dir: &Path) -> Result<SnapshotVerifyReport, SnapshotError> {
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(SnapshotError::MissingFile("manifest.json".into()));
    }
    let manifest_str = fs::read_to_string(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str)?;
    let format = manifest
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let version = manifest
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    if format != SNAPSHOT_FORMAT || !snapshot_version_supported(version) {
        return Err(SnapshotError::InvalidManifest(format!(
            "unsupported snapshot: expected format={} version={} (legacy version {} is also readable), got format={:?} version={:?}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION, SNAPSHOT_VERSION_LEGACY, format, version
        )));
    }
    let legacy = version == SNAPSHOT_VERSION_LEGACY;

    // Provenance (#209). Version 1 carried neither flag; it verifies exactly
    // as before, so an old backup does not turn invalid merely because the
    // field was added.
    //
    // Version 2 (#241) is the opposite: it *records* how the snapshot was
    // taken, so the field is mandatory. A version 2 manifest that does not
    // say whether the copy was coordinated cannot be judged, and defaulting
    // it to "coordinated" would hand a snapshot the verify-but-cannot-
    // restore verdict a backup must never carry.
    let coordinated = match manifest.get("coordinated") {
        None if legacy => true,
        None => {
            return Err(SnapshotError::InvalidManifest(format!(
                "manifest field \"coordinated\" is required in a version {} snapshot: it records whether the copy was coordinated, and a snapshot that does not say cannot be judged",
                version
            )));
        }
        Some(value) => match value.as_bool() {
            Some(flag) => flag,
            None => {
                return Err(SnapshotError::InvalidManifest(format!(
                    "manifest field \"coordinated\" must be a boolean in a version {} snapshot, got {}",
                    version,
                    value
                )));
            }
        },
    };
    let provenance = manifest
        .get("provenance")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if coordinated {
                PROVENANCE_COORDINATED.to_string()
            } else {
                PROVENANCE_UNVERIFIED.to_string()
            }
        });
    let recorded_wal_kind = manifest.get("wal_kind").and_then(|v| v.as_str());
    // Version 1 cannot express a mixed pair: a v1 reader (any build before
    // #209) verifies such a manifest and then restores only the active side,
    // so claiming `both` in v1 is a downgrade hazard, not a legacy shape.
    if legacy && recorded_wal_kind == Some(WAL_KIND_BOTH) {
        return Err(SnapshotError::InvalidManifest(format!(
            "version {} snapshot claims wal_kind \"{}\", which only format version {} can express; a v1 reader would verify it and drop the frozen side",
            SNAPSHOT_VERSION_LEGACY, WAL_KIND_BOTH, SNAPSHOT_VERSION
        )));
    }
    // Version 2 makes the role mandatory: the reader must know whether the
    // WAL member is active or frozen, and whether a second member exists.
    if !legacy && recorded_wal_kind.is_none() {
        return Err(SnapshotError::InvalidManifest(format!(
            "version {} snapshot records no wal_kind; version {} manifests must record {}, {}, {} or {}",
            version, SNAPSHOT_VERSION, WAL_KIND_ACTIVE, WAL_KIND_FROZEN, WAL_KIND_BOTH, WAL_KIND_NONE
        )));
    }
    let wal_kind = match recorded_wal_kind {
        Some(kind @ (WAL_KIND_ACTIVE | WAL_KIND_FROZEN | WAL_KIND_BOTH | WAL_KIND_NONE)) => {
            Some(kind.to_string())
        }
        Some(other) => {
            return Err(SnapshotError::InvalidManifest(format!(
                "unrecognized wal_kind {:?}: expected {}, {}, {} or {}",
                other, WAL_KIND_ACTIVE, WAL_KIND_FROZEN, WAL_KIND_BOTH, WAL_KIND_NONE
            )))
        }
        None => None,
    };

    let checksums_path = dir.join("checksums.json");
    if !checksums_path.exists() {
        return Err(SnapshotError::MissingFile("checksums.json".into()));
    }
    let checksums_str = fs::read_to_string(&checksums_path)?;
    let checksums: SnapshotChecksums = serde_json::from_str(&checksums_str)?;

    let ladybug = dir.join("ladybug.store");
    let manifest_path = dir.join("manifest.json");
    let wal = dir.join("ladybug.store.wal");

    // The store member is the database image: one regular file (#241). A
    // directory in its place used to be hashed like a directory (a digest
    // over its sorted entries), so a snapshot carrying no database at all
    // could match its own checksums and verify as intact. A symlink is
    // refused for the same reason the WAL sides are: the member is a
    // pointer, not the bytes.
    match fs::symlink_metadata(&ladybug) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(meta) => {
            let found = if meta.file_type().is_symlink() {
                "a symlink"
            } else if meta.file_type().is_dir() {
                "a directory"
            } else {
                "not a regular file"
            };
            return Err(SnapshotError::NotARegularFile(format!(
                "the snapshot member ladybug.store is {found}; it must be the regular file holding the database image",
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SnapshotError::MissingFile("ladybug.store".into()));
        }
        Err(e) => return Err(SnapshotError::Io(e)),
    }

    let computed_ladybug = sha256_file(&ladybug)?;
    if computed_ladybug != checksums.ladybug_store_sha256 {
        return Err(SnapshotError::ChecksumMismatch {
            file: "ladybug.store".into(),
            expected: checksums.ladybug_store_sha256.clone(),
            actual: computed_ladybug,
        });
    }

    let computed_manifest = sha256_file(&manifest_path)?;
    if computed_manifest != checksums.manifest_sha256 {
        return Err(SnapshotError::ChecksumMismatch {
            file: "manifest.json".into(),
            expected: checksums.manifest_sha256.clone(),
            actual: computed_manifest,
        });
    }

    let declares_both = wal_kind.as_deref() == Some(WAL_KIND_BOTH);
    let declares_single = matches!(
        wal_kind.as_deref(),
        Some(WAL_KIND_ACTIVE) | Some(WAL_KIND_FROZEN)
    );
    let declares_none = wal_kind.as_deref() == Some(WAL_KIND_NONE);
    // #209 (Codex P2): `none` means the snapshot owns no WAL. A WAL member (or
    // a checksum naming one) under that manifest is an inconsistency, not a
    // stray file to ignore: restore would silently leave it out.
    if declares_none && (wal.exists() || checksums.wal_sha256.is_some()) {
        return Err(SnapshotError::InvalidManifest(format!(
            "wal_kind \"{}\" but the snapshot carries ladybug.store.wal (or its checksum); a snapshot with no WAL must not ship one",
            WAL_KIND_NONE
        )));
    }
    let wal_mentioned =
        checksums.wal_sha256.is_some() || wal.exists() || declares_single || declares_both;
    if wal_mentioned {
        if !wal.exists() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.wal".into(),
                expected: checksums.wal_sha256.clone().unwrap_or_default(),
                actual: "<missing>".into(),
            });
        }
        let computed_wal = sha256_file(&wal)?;
        if Some(&computed_wal) != checksums.wal_sha256.as_ref() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.wal".into(),
                expected: checksums.wal_sha256.clone().unwrap_or_default(),
                actual: computed_wal,
            });
        }
    }

    // The frozen side of a mixed pair is a member of its own (#209): a
    // `wal_kind: both` manifest must produce both streams, and a one-sided
    // manifest must not carry the frozen member at all. Otherwise a restore
    // could place a pair the manifest never promised.
    let checkpoint_wal = dir.join("ladybug.store.wal.checkpoint");
    if checkpoint_wal.exists() && !declares_both {
        return Err(SnapshotError::InvalidManifest(format!(
            "the snapshot carries ladybug.store.wal.checkpoint but its wal_kind is {:?}; a mixed pair is recorded as wal_kind \"{}\"",
            wal_kind, WAL_KIND_BOTH
        )));
    }
    let checkpoint_mentioned =
        checksums.wal_checkpoint_sha256.is_some() || checkpoint_wal.exists() || declares_both;
    if checkpoint_mentioned {
        if !checkpoint_wal.exists() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.wal.checkpoint".into(),
                expected: checksums.wal_checkpoint_sha256.clone().unwrap_or_default(),
                actual: "<missing>".into(),
            });
        }
        let computed_checkpoint = sha256_file(&checkpoint_wal)?;
        if Some(&computed_checkpoint) != checksums.wal_checkpoint_sha256.as_ref() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.wal.checkpoint".into(),
                expected: checksums.wal_checkpoint_sha256.clone().unwrap_or_default(),
                actual: computed_checkpoint,
            });
        }
    }

    // The store-state sidecar, under the same rule as the WAL: a
    // snapshot that carries one (or whose checksums name one) must
    // produce it intact; a snapshot with neither verifies as before.
    let sidecar = dir.join("ladybug.store.store-state.json");
    let sidecar_mentioned = checksums.store_state_sha256.is_some() || sidecar.exists();
    if sidecar_mentioned {
        if !sidecar.exists() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.store-state.json".into(),
                expected: checksums.store_state_sha256.clone().unwrap_or_default(),
                actual: "<missing>".into(),
            });
        }
        let computed_sidecar = sha256_file(&sidecar)?;
        if Some(&computed_sidecar) != checksums.store_state_sha256.as_ref() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.store-state.json".into(),
                expected: checksums.store_state_sha256.clone().unwrap_or_default(),
                actual: computed_sidecar,
            });
        }
    }

    // The checkpoint shadow and pinned marker, under the same rule as the
    // sidecar: a snapshot that carries one (or whose checksums name one) must
    // produce it intact (#209).
    for (name, path, recorded) in [
        (
            "ladybug.store.shadow",
            dir.join("ladybug.store.shadow"),
            checksums.shadow_sha256.as_ref(),
        ),
        (
            "ladybug.store.pinned.tmp",
            dir.join("ladybug.store.pinned.tmp"),
            checksums.pinned_sha256.as_ref(),
        ),
    ] {
        let mentioned = recorded.is_some() || path.exists();
        if !mentioned {
            continue;
        }
        if !path.exists() {
            return Err(SnapshotError::ChecksumMismatch {
                file: name.to_string(),
                expected: recorded.cloned().unwrap_or_default(),
                actual: "<missing>".into(),
            });
        }
        let computed = sha256_file(&path)?;
        if Some(&computed) != recorded {
            return Err(SnapshotError::ChecksumMismatch {
                file: name.to_string(),
                expected: recorded.cloned().unwrap_or_default(),
                actual: computed,
            });
        }
    }

    // If a zstd-compressed manifest copy is present, verify it decompresses
    // within the bounded snapshot size cap AND that its content matches the
    // plain `manifest.json` bytes: a compressed copy drifting from the manifest
    // is an inconsistent snapshot, not a valid one.
    let zst_path = dir.join("manifest.json.zst");
    if zst_path.exists() {
        let raw = fs::read(&zst_path)?;
        let decompressed = crate::compression::zstd::decompress_snapshot(
            &raw,
            crate::compression::zstd::DEFAULT_SNAPSHOT_MAX_DECOMPRESSED_BYTES,
        )
        .map_err(|e| SnapshotError::InvalidManifest(format!("manifest.json.zst: {}", e)))?;
        let expected_raw = fs::read(&manifest_path)?;
        if decompressed != expected_raw {
            return Err(SnapshotError::InvalidManifest(
                "manifest.json.zst content mismatch with manifest.json".to_string(),
            ));
        }
    }

    Ok(SnapshotVerifyReport {
        // The bytes match their checksums (every check above returned Err
        // otherwise), but only a coordinated snapshot earns the `valid`
        // verdict: an uncoordinated pair cannot prove its members came from
        // one instant (#209).
        valid: coordinated,
        integrity_ok: true,
        coordinated,
        provenance,
        wal_kind,
        dir: dir.to_path_buf(),
        checksums: Some(checksums),
        error: None,
    })
}

pub fn select_expired_snapshots(root: &Path, keep: usize) -> Result<Vec<PathBuf>, SnapshotError> {
    let keep = keep.max(1);
    let mut valid: Vec<(PathBuf, String, String)> = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().unwrap_or_default();
        if !name.starts_with("snapshot-") {
            continue;
        }
        if path == root {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let manifest_str = match fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "unreadable snapshot manifest at {}: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        let manifest: serde_json::Value = match serde_json::from_str(&manifest_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "invalid JSON in snapshot manifest at {}: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        let format = manifest
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let version = manifest
            .get("version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if format != SNAPSHOT_FORMAT || !snapshot_version_supported(version) {
            continue;
        }
        let created_at = manifest
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if created_at.is_empty() {
            continue;
        }
        valid.push((path.clone(), created_at.to_string(), name));
    }

    valid.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));

    let expired = if valid.len() > keep {
        valid[keep..].iter().map(|(p, _, _)| p.clone()).collect()
    } else {
        Vec::new()
    };
    Ok(expired)
}

pub fn prune_expired_snapshots(root: &Path, keep: usize) -> Result<Vec<PathBuf>, SnapshotError> {
    let to_remove = select_expired_snapshots(root, keep)?;
    let mut removed = Vec::new();
    for path in &to_remove {
        fs::remove_dir_all(path)?;
        removed.push(path.clone());
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// #211: the buffer-pool latch is visible in status, its skip count is
    /// separate from the failure count, and a payload written before the fields
    /// existed still deserializes (they carry serde defaults).
    #[test]
    fn storage_status_reports_the_checkpoint_latch() {
        let dir = temp_dir("checkpoint-latch-status");
        let store = dir.join("s.lbug");
        fs::write(&store, b"db").unwrap();
        let cfg = StorageHygieneConfig::default();

        let clean = compute_storage_status(&store, &cfg);
        assert!(!clean.checkpoints_stopped);
        assert_eq!(clean.checkpoint_skips, 0);

        record_checkpoint_skip(&store);
        record_checkpoint_skip(&store);
        let failures_before = CHECKPOINT_FAILURES.load(std::sync::atomic::Ordering::Relaxed);
        record_checkpoints_stopped(&store, true);
        let latched = compute_storage_status(&store, &cfg);
        assert!(
            latched.checkpoints_stopped,
            "the latch must be visible in status (#211)"
        );
        assert_eq!(latched.checkpoint_skips, 2, "skips are counted (#211)");
        assert_eq!(
            latched.checkpoint_failures, failures_before,
            "a skip is not a failure (#211)"
        );

        record_checkpoints_stopped(&store, false);
        let resumed = compute_storage_status(&store, &cfg);
        assert!(!resumed.checkpoints_stopped);
        assert_eq!(
            resumed.checkpoint_skips, 2,
            "clearing the latch keeps the process's running skip count"
        );

        let mut value = serde_json::to_value(&resumed).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("checkpoints_stopped");
        object.remove("checkpoint_skips");
        let old: StorageStatus = serde_json::from_value(value).unwrap();
        assert!(
            !old.checkpoints_stopped,
            "an old payload defaults to no latch"
        );
        assert_eq!(
            old.checkpoint_skips, 0,
            "an old payload defaults to no skips"
        );
    }

    #[test]
    fn prune_wal_recovery_artifacts_except_never_removes_a_protected_path() {
        let dir = temp_dir("prune-except");
        let store = dir.join("p.lbug");
        fs::write(&store, b"db").unwrap();
        let newest = dir.join("p.lbug.wal.unclean.300");
        let middle = dir.join("p.lbug.wal.unclean.200");
        let oldest = dir.join("p.lbug.wal.unclean.100");
        for (path, payload) in [
            (&newest, b"newest"),
            (&middle, b"middle"),
            (&oldest, b"oldest"),
        ] {
            fs::write(path, payload).unwrap();
        }
        let now = std::time::SystemTime::now();
        let set = |path: &PathBuf, secs: u64| {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - std::time::Duration::from_secs(secs))
                .unwrap();
        };
        set(&newest, 1);
        set(&middle, 60);
        set(&oldest, 3600);

        // keep=1 would drop `middle` and `oldest`; the protected oldest survives.
        let removed = prune_wal_recovery_artifacts_except(&store, 1, &[oldest.clone()]);
        assert_eq!(
            removed,
            vec![middle.clone()],
            "only the unprotected excess goes"
        );
        assert!(newest.exists(), "the newest artifact is kept");
        assert!(
            oldest.exists(),
            "a protected path survives beyond the budget"
        );
        assert!(!middle.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// `#212`: the status reports the age of the newest active/frozen WAL, and
    /// nothing when the store has no WAL. A symlink is not a WAL this store
    /// wrote, so it never contributes a size or an age.
    #[test]
    fn wal_age_secs_tracks_the_newest_wal_and_is_absent_without_one() {
        let dir = temp_dir("wal-age");
        let store = dir.join("age.lbug");
        fs::write(&store, b"db").unwrap();
        let cfg = StorageHygieneConfig::default();
        assert_eq!(
            compute_storage_status(&store, &cfg).wal_age_secs,
            None,
            "a store with no WAL has no WAL age"
        );
        let json = serde_json::to_value(compute_storage_status(&store, &cfg)).unwrap();
        assert!(
            json.get("wal_age_secs").is_some(),
            "wal_age_secs is serialized even when absent (#212)"
        );
        assert_eq!(
            json["wal_age_secs"],
            serde_json::Value::Null,
            "absence is `null`, not an omitted key (#212)"
        );

        let active = dir.join("age.lbug.wal");
        let frozen = dir.join("age.lbug.wal.checkpoint");
        fs::write(&active, b"active").unwrap();
        fs::write(&frozen, b"frozen").unwrap();
        let now = std::time::SystemTime::now();
        let set_age = |path: &PathBuf, secs: u64| {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - std::time::Duration::from_secs(secs))
                .unwrap();
        };
        set_age(&active, 30);
        set_age(&frozen, 90);

        let age = compute_storage_status(&store, &cfg)
            .wal_age_secs
            .expect("an active WAL has an age");
        assert!(
            (29..60).contains(&age),
            "the age is the newest WAL's, not the older frozen side's: {age}"
        );

        // The frozen side alone still answers.
        fs::remove_file(&active).unwrap();
        let age = compute_storage_status(&store, &cfg)
            .wal_age_secs
            .expect("a frozen WAL has an age");
        assert!(
            (89..180).contains(&age),
            "a store whose only WAL is the frozen side still reports its age: {age}"
        );

        // Neither side: null. A symlink at `.wal` is not counted either.
        fs::remove_file(&frozen).unwrap();
        assert_eq!(
            compute_storage_status(&store, &cfg).wal_age_secs,
            None,
            "removing the pair returns the age to null"
        );
        #[cfg(unix)]
        {
            let victim = dir.join("victim.wal");
            fs::write(&victim, b"not-this-store's-wal").unwrap();
            std::os::unix::fs::symlink(&victim, &active).unwrap();
            assert_eq!(
                compute_storage_status(&store, &cfg).wal_age_secs,
                None,
                "a symlinked .wal is not a WAL this store wrote"
            );
        }
    }

    /// `#212`: a fresh process reading the status from the path alone (no
    /// in-process map entry) reports the persisted recovery and checkpoint
    /// health; the live in-process map still wins when it has an entry.
    #[test]
    fn storage_status_reads_the_persisted_recovery_and_checkpoint_health() {
        let dir = temp_dir("persisted-status");
        let store = dir.join("persisted.lbug");
        fs::write(&store, b"db").unwrap();
        crate::model_choice::write_store_state_identity(
            &store,
            &crate::model_choice::StoreState::new(true, test_descriptor()),
        )
        .expect("write the recorded sidecar");
        crate::model_choice::record_store_state_recovery(
            &store,
            "discarded_unreadable_wal",
            8192,
            "2026-11-15T00:00:00Z",
        )
        .expect("record the recovery");
        crate::model_choice::record_store_state_checkpoint(
            &store,
            "2026-11-15T00:00:01Z",
            7,
            false,
        )
        .expect("record the failed checkpoint");

        // The fresh-process shape: the path-only query has no live entry.
        record_recovery_provenance(&store, None);
        let status = compute_storage_status(&store, &StorageHygieneConfig::default());
        assert_eq!(
            status.recovery_provenance.as_deref(),
            Some("discarded_unreadable_wal"),
            "the persisted recovery kind is reported as the fallback"
        );
        assert_eq!(
            status.last_recovery_at.as_deref(),
            Some("2026-11-15T00:00:00Z"),
            "the persisted recovery timestamp is history and is reported"
        );
        assert!(
            !status.opened_via_recovery,
            "opened_via_recovery is handle-scoped: a path-only query never opened the store (#212)"
        );
        assert!(
            status.checkpoint_failures >= 1,
            "the persisted failure count reaches the status: {}",
            status.checkpoint_failures
        );
        assert!(
            status.last_checkpoint_at.is_some(),
            "a recorded checkpoint timestamp is reported"
        );
        assert!(
            status.last_checkpoint_duration_ms.is_some(),
            "a recorded checkpoint duration is reported"
        );

        // The live layer is the fallback's superior: it answers for a store
        // with no durable record, and it wins when both exist. It alone owns
        // `opened_via_recovery`.
        record_recovery_provenance(&store, Some("unverified_wal_replay"));
        let live = compute_storage_status(&store, &StorageHygieneConfig::default());
        assert_eq!(
            live.recovery_provenance.as_deref(),
            Some("unverified_wal_replay"),
            "the in-process map stays authoritative for the kind"
        );
        assert!(
            live.opened_via_recovery,
            "a recorded live recovery is the only thing that sets the handle-scoped flag"
        );
        assert_eq!(
            live.last_recovery_at.as_deref(),
            Some("2026-11-15T00:00:00Z"),
            "history is unchanged by the live layer"
        );
        record_recovery_provenance(&store, None);
    }

    fn test_descriptor() -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "fastembed".to_string(),
            model: "ModelA".to_string(),
            revision: "onnx".to_string(),
            dims: 4,
            normalization: "unknown".to_string(),
            preprocessing: "unknown".to_string(),
            distance_metric: "cosine".to_string(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join("neurostrata-snapshot-tests");
        let d = base.join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_snapshot_dir(parent: &Path, snap_name: &str, created_at: &str) -> PathBuf {
        make_snapshot_dir_with_version(parent, snap_name, created_at, SNAPSHOT_VERSION)
    }

    fn make_snapshot_dir_with_version(
        parent: &Path,
        snap_name: &str,
        created_at: &str,
        version: u32,
    ) -> PathBuf {
        let d = parent.join(snap_name);
        fs::create_dir_all(&d).unwrap();
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"{}\"}}",
            SNAPSHOT_FORMAT, version, created_at
        );
        manifest.write_all(content.as_bytes()).unwrap();
        d
    }

    fn write_store(dir: &Path) {
        let mut f = File::create(dir.join("ladybug.store")).unwrap();
        f.write_all(b"test store content").unwrap();
    }

    #[test]
    fn roundtrip_write_and_verify() {
        let d = temp_dir("roundtrip");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION
        );
        manifest.write_all(content.as_bytes()).unwrap();
        let checksums = write_checksums(&d).unwrap();
        assert!(!checksums.ladybug_store_sha256.is_empty());
        assert!(checksums.manifest_sha256.len() == 64);
        let report = verify_snapshot(&d).unwrap();
        assert!(report.valid);
        assert_eq!(report.dir, d);
        assert!(report.error.is_none());
    }

    #[test]
    fn checksums_cover_the_store_state_sidecar() {
        // The snapshot carries the store-state sidecar,
        // and the checksums cover it — a tampered sidecar must fail
        // verification like any other snapshot member. Snapshots
        // without a sidecar (older backups) carry no sidecar checksum
        // and verify exactly as before.
        let d = temp_dir("sidecar-checksum");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION
        );
        manifest.write_all(content.as_bytes()).unwrap();
        fs::write(d.join("ladybug.store.store-state.json"), b"{\"version\":1}").unwrap();
        let checksums = write_checksums(&d).unwrap();
        assert!(
            checksums.store_state_sha256.is_some(),
            "a present sidecar is checksummed"
        );
        let report = verify_snapshot(&d).unwrap();
        assert!(report.valid);
        fs::write(d.join("ladybug.store.store-state.json"), b"{\"version\":2}").unwrap();
        let result = verify_snapshot(&d);
        assert!(
            matches!(result, Err(SnapshotError::ChecksumMismatch { .. })),
            "a tampered sidecar fails verification: {result:?}"
        );

        let d2 = temp_dir("sidecar-absent");
        write_store(&d2);
        let mut manifest = File::create(d2.join("manifest.json")).unwrap();
        manifest.write_all(content.as_bytes()).unwrap();
        let checksums = write_checksums(&d2).unwrap();
        assert!(
            checksums.store_state_sha256.is_none(),
            "an absent sidecar carries no checksum"
        );
        assert!(verify_snapshot(&d2).unwrap().valid);
    }

    #[test]
    fn tampered_file_triggers_checksum_mismatch() {
        let d = temp_dir("tampered");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION
        );
        manifest.write_all(content.as_bytes()).unwrap();
        write_checksums(&d).unwrap();
        // Tamper store after checksums written
        let mut f = File::create(d.join("ladybug.store")).unwrap();
        f.write_all(b"tampered").unwrap();
        let result = verify_snapshot(&d);
        assert!(matches!(
            result,
            Err(SnapshotError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn invalid_manifest_triggers_invalid_manifest() {
        let d = temp_dir("invalid_manifest");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        manifest
            .write_all(b"{\"format\":\"bad\",\"version\":99}")
            .unwrap();
        let mut f = File::create(d.join("checksums.json")).unwrap();
        f.write_all(b"{}").unwrap();
        let result = verify_snapshot(&d);
        assert!(matches!(result, Err(SnapshotError::InvalidManifest(..))));
    }

    #[test]
    fn an_uncoordinated_manifest_never_verifies_as_fully_valid() {
        // #209: a `--no-open` export records `coordinated: false` /
        // `provenance: "unverified"`. Its bytes still have to match
        // `checksums.json`, but the snapshot was captured without the store
        // lock, so `verify-snapshot` must not call it fully `valid`.
        let d = temp_dir("uncoordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"active\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"wal-bytes").unwrap();
        write_checksums(&d).unwrap();

        let report = verify_snapshot(&d).expect("the copied bytes still verify");
        assert!(report.integrity_ok, "the checksums still match");
        assert!(
            !report.coordinated,
            "the manifest says it was uncoordinated"
        );
        assert!(
            !report.valid,
            "an uncoordinated snapshot must not report as fully valid (#209)"
        );
        assert_eq!(report.provenance, "unverified");
        assert_eq!(report.wal_kind.as_deref(), Some("active"));
    }

    #[test]
    fn a_coordinated_manifest_still_verifies_as_valid() {
        let d = temp_dir("coordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"provenance\":\"coordinated\",\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let report = verify_snapshot(&d).expect("a coordinated snapshot verifies");
        assert!(report.integrity_ok);
        assert!(report.coordinated);
        assert!(
            report.valid,
            "coordination is what buys the `valid` verdict"
        );
        assert_eq!(report.provenance, "coordinated");
    }

    #[test]
    fn the_checkpoint_shadow_and_pinned_marker_are_snapshot_members() {
        // #209: a crashed checkpoint leaves `<db>.shadow` and `<db>.pinned.tmp`
        // beside the WAL. They are part of the snapshot, so they are
        // checksummed like every other member and a tampered shadow fails
        // verification instead of restoring as a torn checkpoint.
        let d = temp_dir("shadow-members");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"frozen\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"frozen-wal").unwrap();
        fs::write(d.join("ladybug.store.shadow"), b"shadow-pages").unwrap();
        fs::write(d.join("ladybug.store.pinned.tmp"), b"pinned-marker").unwrap();

        let checksums = write_checksums(&d).unwrap();
        assert!(
            checksums.shadow_sha256.is_some(),
            "the checkpoint shadow is checksummed"
        );
        assert!(
            checksums.pinned_sha256.is_some(),
            "the pinned-checkpoint marker is checksummed"
        );
        verify_snapshot(&d).expect("an intact checkpoint set verifies");

        fs::write(d.join("ladybug.store.shadow"), b"tampered-pages").unwrap();
        let result = verify_snapshot(&d);
        assert!(
            matches!(result, Err(SnapshotError::ChecksumMismatch { .. })),
            "a tampered shadow fails verification: {result:?}"
        );
    }

    #[test]
    fn a_both_wal_manifest_verifies_only_with_both_sides() {
        // #209 (V5): a mixed WAL pair (`<db>.wal` + `<db>.wal.checkpoint`)
        // preflights as a usable single store, so the export must carry BOTH
        // byte streams. A `wal_kind: both` manifest is only verifiable when
        // both members are present and checksummed.
        let d = temp_dir("both-wal");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"both\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"active-bytes").unwrap();
        fs::write(d.join("ladybug.store.wal.checkpoint"), b"frozen-bytes").unwrap();

        let checksums = write_checksums(&d).unwrap();
        assert!(
            checksums.wal_checkpoint_sha256.is_some(),
            "the frozen side of a mixed pair is checksummed"
        );
        let report = verify_snapshot(&d).expect("both sides verify together");
        assert!(report.integrity_ok);
        assert_eq!(report.wal_kind.as_deref(), Some("both"));

        // Dropping either side leaves a snapshot that claims more than it
        // holds; that is a mismatch, never a quiet pass.
        fs::remove_file(d.join("ladybug.store.wal.checkpoint")).unwrap();
        let result = verify_snapshot(&d);
        assert!(
            matches!(result, Err(SnapshotError::ChecksumMismatch { .. })),
            "a both-kind manifest with the frozen side missing fails: {result:?}"
        );
    }

    #[test]
    fn a_version_1_legacy_snapshot_still_verifies() {
        // Pre-#209 snapshots are format version 1: no `wal_kind`, and their
        // WAL member is the active `.wal`. Current code keeps reading them.
        let d = temp_dir("v1-legacy");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\"}}",
                SNAPSHOT_FORMAT, 1
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"legacy-wal").unwrap();
        write_checksums(&d).unwrap();

        let report = verify_snapshot(&d).expect("a version 1 snapshot verifies");
        assert!(report.integrity_ok);
        assert!(
            report.valid,
            "a version 1 snapshot keeps the legacy default: it is treated as coordinated (#241)"
        );
        assert!(
            report.coordinated,
            "the legacy default is the coordinated verdict (#241)"
        );
        assert_eq!(
            report.wal_kind, None,
            "a version 1 manifest records no WAL role; restore treats it as active"
        );
    }

    #[test]
    fn a_version_1_manifest_claiming_both_is_invalid() {
        // #209 (J7): `both` is only expressible in version 2. A version 1
        // manifest that claims it describes a pair a v1 reader would truncate,
        // so it is refused rather than half-read.
        let d = temp_dir("v1-both");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":1,\"created_at\":\"2026-01-01T00:00:00Z\",\"wal_kind\":\"both\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"active-bytes").unwrap();
        fs::write(d.join("ladybug.store.wal.checkpoint"), b"frozen-bytes").unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("both") && message.contains("version 1"),
                "the refusal names the version/kind mismatch: {message}"
            ),
            other => panic!("a version 1 manifest claiming both must be rejected: {other:?}"),
        }
    }

    #[test]
    fn a_version_2_manifest_without_wal_kind_is_invalid() {
        let d = temp_dir("v2-no-kind");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("wal_kind"),
                "the refusal names the missing field: {message}"
            ),
            other => panic!("a version 2 manifest without wal_kind must be rejected: {other:?}"),
        }
    }

    #[test]
    fn a_wal_stamped_in_the_future_reports_an_age_of_zero() {
        // #241: a WAL whose mtime is in the future has no age yet. The
        // subtraction used to wrap and report a huge age, which reads as
        // "this WAL has not been written in years".
        let d = temp_dir("wal-future-age");
        let wal = d.join("ladybug.store.wal");
        fs::write(&wal, b"wal-bytes").unwrap();
        // Setting a time needs write access on Windows.
        let file = File::options().write(true).open(&wal).unwrap();
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        file.set_times(fs::FileTimes::new().set_modified(future))
            .unwrap();
        drop(file);

        assert_eq!(
            wal_age_secs(&wal, &d.join("ladybug.store.wal.checkpoint")),
            Some(0),
            "a WAL stamped in the future reports an age of zero, not a wrapped-around number"
        );
    }

    #[test]
    fn a_directory_in_place_of_the_store_is_refused() {
        // #241: `ladybug.store` is the database image -- one file. A
        // directory in its place used to be hashed like a directory (a
        // digest over its sorted entries), so a snapshot with no database
        // in it at all could carry matching checksums and verify as
        // intact. The member must be a regular file, and the refusal has
        // to name it.
        let d = temp_dir("dir-store");
        fs::create_dir_all(d.join("ladybug.store")).unwrap();
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let err = verify_snapshot(&d)
            .expect_err("a directory in place of ladybug.store must be refused (#241)");
        let message = err.to_string();
        assert!(
            message.contains("ladybug.store"),
            "the refusal names the member: {message}"
        );
        assert!(
            message.contains("not a regular file"),
            "the refusal names what it found: {message}"
        );
    }

    #[test]
    fn a_version_2_manifest_without_coordinated_is_invalid() {
        // #241: a version 2 manifest records how the snapshot was taken. A
        // manifest that does not say whether the copy was coordinated cannot
        // be judged, and guessing "coordinated" hands a snapshot the
        // verify-but-cannot-restore verdict a backup must never carry.
        let d = temp_dir("v2-no-coordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("coordinated"),
                "the refusal names the missing field: {message}"
            ),
            other => panic!("a version 2 manifest without coordinated must be rejected: {other:?}"),
        }
    }

    #[test]
    fn a_version_2_manifest_with_a_non_boolean_coordinated_is_invalid() {
        // #241: "coordinated" is a boolean field. A string or a number there
        // is not a snapshot that says "not coordinated" — it is a snapshot
        // that says nothing, so it is refused rather than read.
        let d = temp_dir("v2-bad-coordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":\"true\",\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("coordinated") && message.contains("boolean"),
                "the refusal names the field and its type: {message}"
            ),
            other => panic!(
                "a version 2 manifest with a non-boolean coordinated must be rejected: {other:?}"
            ),
        }
    }

    #[test]
    fn a_version_1_manifest_with_a_non_boolean_coordinated_is_invalid() {
        // #241: version 1 predates the field, so a missing one keeps the
        // legacy default. A present-but-unreadable one is refused rather
        // than defaulted: "coordinated": "no" is not a coordinated snapshot.
        let d = temp_dir("v1-bad-coordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":1,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":1}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("coordinated") && message.contains("boolean"),
                "the refusal names the field and its type: {message}"
            ),
            other => panic!(
                "a version 1 manifest with a non-boolean coordinated must be rejected: {other:?}"
            ),
        }
    }

    #[test]
    fn a_version_2_manifest_that_is_not_coordinated_is_intact_but_not_valid() {
        // #241: an uncoordinated copy is honest about what it is. Its bytes
        // can still match their checksums, but it never earns the `valid`
        // verdict, because it cannot prove its members came from one instant.
        let d = temp_dir("v2-uncoordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let report =
            verify_snapshot(&d).expect("an uncoordinated snapshot still verifies its bytes");
        assert!(report.integrity_ok, "the bytes match their checksums");
        assert!(
            !report.valid,
            "an uncoordinated snapshot is not fully valid"
        );
        assert!(
            !report.coordinated,
            "the verdict reports it as uncoordinated"
        );
        assert_eq!(
            report.provenance, PROVENANCE_UNVERIFIED,
            "an uncoordinated snapshot records the unverified provenance"
        );
    }

    #[test]
    fn a_version_2_manifest_that_is_coordinated_is_valid() {
        // The control for the two refusals above: an explicit `true` is a
        // coordinated snapshot and earns the full verdict.
        let d = temp_dir("v2-coordinated");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let report = verify_snapshot(&d).expect("a coordinated snapshot verifies");
        assert!(report.integrity_ok, "the bytes match their checksums");
        assert!(report.valid, "a coordinated snapshot is fully valid");
        assert!(report.coordinated, "the verdict reports it as coordinated");
        assert_eq!(
            report.provenance, PROVENANCE_COORDINATED,
            "a coordinated snapshot records the coordinated provenance"
        );
    }

    #[test]
    fn a_version_2_manifest_with_an_unknown_wal_kind_is_invalid() {
        let d = temp_dir("v2-unknown-kind");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"sometimes\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("wal_kind"),
                "the refusal names the field: {message}"
            ),
            other => panic!("an unknown wal_kind must be rejected: {other:?}"),
        }
    }

    #[test]
    fn an_unsupported_snapshot_version_is_refused() {
        // The same rule an old reader applies to version 2: a version this
        // build does not know is refused, never half-read.
        let d = temp_dir("v3");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":3,\"created_at\":\"2026-01-01T00:00:00Z\",\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT
            ),
        )
        .unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        assert!(
            matches!(result, Err(SnapshotError::InvalidManifest(ref m)) if m.contains("version")),
            "an unknown future version is refused with a version message: {result:?}"
        );
    }

    #[test]
    fn a_none_manifest_with_a_wal_member_is_refused() {
        // #209 (Codex P2): `wal_kind: none` promises no WAL member. A WAL (or
        // a checksum naming one) under that manifest is inconsistent, and a
        // restore would silently leave it out.
        let d = temp_dir("none-with-wal");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"unexpected-wal").unwrap();
        write_checksums(&d).unwrap();

        match verify_snapshot(&d) {
            Err(SnapshotError::InvalidManifest(message)) => {
                assert!(
                    message.contains("none") && message.contains("ladybug.store.wal"),
                    "the refusal names the inconsistency: {message}"
                );
            }
            other => panic!("a none manifest carrying a WAL must be refused: {other:?}"),
        }

        // Same rule when only the checksum names a WAL: the member the
        // manifest says cannot exist is still named.
        fs::remove_file(d.join("ladybug.store.wal")).unwrap();
        let checksums_path = d.join("checksums.json");
        let mut checksums: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&checksums_path).unwrap()).unwrap();
        checksums["wal_sha256"] =
            serde_json::json!("0000000000000000000000000000000000000000000000000000000000000000");
        fs::write(
            &checksums_path,
            serde_json::to_string_pretty(&checksums).unwrap(),
        )
        .unwrap();
        match verify_snapshot(&d) {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("none") && message.contains("ladybug.store.wal"),
                "the checksum-only case names the inconsistency: {message}"
            ),
            other => {
                panic!("a none manifest whose checksums name a WAL must be refused: {other:?}")
            }
        }
    }

    #[test]
    fn a_one_sided_manifest_with_a_frozen_member_is_refused() {
        // #209 (J7): a `wal_kind: active` manifest that also carries
        // `ladybug.store.wal.checkpoint` describes a pair it does not own. The
        // verifier refuses rather than certifying a set restore would treat as
        // active-only.
        let d = temp_dir("active-with-frozen-member");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"active\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal"), b"active-bytes").unwrap();
        fs::write(d.join("ladybug.store.wal.checkpoint"), b"leftover-frozen").unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        match result {
            Err(SnapshotError::InvalidManifest(message)) => assert!(
                message.contains("ladybug.store.wal.checkpoint"),
                "the refusal names the unexpected member: {message}"
            ),
            other => panic!("a one-sided manifest with a frozen member must be refused: {other:?}"),
        }
    }

    #[test]
    fn a_both_manifest_missing_the_active_side_is_refused() {
        // A `both` manifest must produce the active member too; half a pair is
        // not verifiable.
        let d = temp_dir("both-missing-active");
        write_store(&d);
        fs::write(
            d.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"both\"}}",
                SNAPSHOT_FORMAT, SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        fs::write(d.join("ladybug.store.wal.checkpoint"), b"frozen-bytes").unwrap();
        write_checksums(&d).unwrap();

        let result = verify_snapshot(&d);
        assert!(
            matches!(result, Err(SnapshotError::ChecksumMismatch { .. })),
            "a both manifest without the active member fails: {result:?}"
        );
    }

    #[test]
    fn select_expired_counts_version_1_and_version_2_snapshots() {
        // Readable v1 snapshots are still snapshots: retention must count
        // them, not leak them because their version is not the current one.
        let root = temp_dir("expired_versions");
        make_snapshot_dir_with_version(&root, "snapshot-v1", "2025-01-01T00:00:00Z", 1);
        make_snapshot_dir_with_version(&root, "snapshot-v2", "2026-01-01T00:00:00Z", 2);

        let expired = select_expired_snapshots(&root, 1).unwrap();
        assert_eq!(expired.len(), 1, "both versions are retention candidates");
        assert!(expired[0].to_string_lossy().contains("v1"));
    }

    #[test]
    fn select_expired_keeps_newest_n_and_ignores_invalid() {
        let root = temp_dir("expired_select");
        // Create 3 valid snapshots
        make_snapshot_dir(&root, "snapshot-old1", "2025-01-01T00:00:00Z");
        make_snapshot_dir(&root, "snapshot-new1", "2026-01-01T00:00:00Z");
        make_snapshot_dir(&root, "snapshot-new2", "2026-06-01T00:00:00Z");
        // Invalid directory without manifest
        fs::create_dir_all(root.join("snapshot-bad")).unwrap();
        // Directory with bad manifest
        let bad = root.join("snapshot-bad-manifest");
        fs::create_dir_all(&bad).unwrap();
        let mut mf = File::create(bad.join("manifest.json")).unwrap();
        mf.write_all(b"{\"format\":\"bad\"}").unwrap();

        let expired = select_expired_snapshots(&root, 2).unwrap();
        assert_eq!(expired.len(), 1);
        assert!(expired[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("old1"));
    }

    #[test]
    fn select_expired_clamps_keep_to_one() {
        let root = temp_dir("clamp");
        make_snapshot_dir(&root, "snapshot-a", "2026-01-01T00:00:00Z");
        let expired = select_expired_snapshots(&root, 0).unwrap();
        assert!(expired.is_empty());
    }

    #[test]
    fn prune_expired_removes_oldest() {
        let root = temp_dir("prune");
        make_snapshot_dir(&root, "snapshot-old", "2025-01-01T00:00:00Z");
        make_snapshot_dir(&root, "snapshot-new", "2026-01-01T00:00:00Z");
        let removed = prune_expired_snapshots(&root, 1).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!root.join("snapshot-old").exists());
        assert!(root.join("snapshot-new").exists());
    }
}
