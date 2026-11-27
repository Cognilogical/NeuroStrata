use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Provider selection mode for deduplication.
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DedupProviderMode {
    Auto,
    Heuristic,
    LayaLocal,
    Jev,
    None,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct DeduplicationConfig {
    #[serde(default)]
    pub provider: DedupProviderMode,
    pub judgment_api_key_env: Option<String>,
    #[serde(default)]
    pub judgment_api_key: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f32,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
    #[serde(default = "default_laya_bundle_path")]
    pub laya_bundle_path: String,
    #[serde(default = "default_cb_failure_threshold")]
    pub cb_failure_threshold: u32,
    #[serde(default = "default_cb_backoff_initial")]
    pub cb_backoff_initial_seconds: u64,
    #[serde(default = "default_cb_backoff_max")]
    pub cb_backoff_max_seconds: u64,
}

fn default_timeout() -> u64 {
    5
}
fn default_similarity_threshold() -> f32 {
    0.85
}
fn default_confidence_threshold() -> f32 {
    0.8
}
fn default_laya_bundle_path() -> String {
    "artifacts/laya-bundle".to_string()
}
fn default_max_candidates() -> usize {
    5
}
fn default_cb_failure_threshold() -> u32 {
    3
}
fn default_cb_backoff_initial() -> u64 {
    30
}
fn default_cb_backoff_max() -> u64 {
    3600
}

impl Default for DedupProviderMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl Default for DeduplicationConfig {
    fn default() -> Self {
        Self {
            provider: DedupProviderMode::Auto,
            judgment_api_key_env: None,
            judgment_api_key: None,
            timeout_seconds: default_timeout(),
            similarity_threshold: default_similarity_threshold(),
            confidence_threshold: default_confidence_threshold(),
            max_candidates: default_max_candidates(),
            laya_bundle_path: default_laya_bundle_path(),
            cb_failure_threshold: default_cb_failure_threshold(),
            cb_backoff_initial_seconds: default_cb_backoff_initial(),
            cb_backoff_max_seconds: default_cb_backoff_max(),
        }
    }
}

/// Byte-denominated store settings (#414). Both values are in bytes so the
/// buffer pool and the size cap are commensurable. The older megabyte knob is
/// deprecated and migrated on load, never used as a second live setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct StoreConfig {
    /// Cap on the engine's buffer pool. `None` uses the 384 MiB default and
    /// `Some(0)` hands sizing back to the engine. Values below the 128 MiB
    /// floor are raised to it in `store::ladybug`.
    pub buffer_pool_bytes: Option<u64>,
    /// Hard cap on the store's on-disk bytes (database plus write-ahead log).
    /// `None` or `Some(0)` means no cap.
    pub max_db_bytes: Option<u64>,
}

/// #414 owner ruling (2026-11-27): no megabyte knob is configuration. A legacy
/// value is recognized only to fail with the exact byte replacement.
fn legacy_pool_error(legacy_key: &str, replacement_key: &str, value: &str, mb: u64) -> String {
    format!(
        "{legacy_key}={value} is no longer accepted; replace it with \
         {replacement_key}={} bytes",
        mb.saturating_mul(1024 * 1024)
    )
}

/// Error text for the legacy environment variable, including the exact byte
/// replacement when the value is numeric.
pub(crate) fn legacy_pool_env_error(value: &str) -> String {
    let value = value.trim();
    match value.parse::<u64>() {
        Ok(mb) => legacy_pool_error(
            "NEUROSTRATA_BUFFER_POOL_MB",
            "NEUROSTRATA_BUFFER_POOL_BYTES",
            value,
            mb,
        ),
        Err(_) => format!(
            "NEUROSTRATA_BUFFER_POOL_MB={value:?} is no longer accepted; replace it with \
             NEUROSTRATA_BUFFER_POOL_BYTES=<bytes>; the megabyte variable must not be set"
        ),
    }
}

/// The real environment check. `None` (unset or empty) is the only accepted
/// state; any legacy value fails closed.
pub(crate) fn reject_legacy_pool_env_at(value: Option<&str>) -> Result<()> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => anyhow::bail!("{}", legacy_pool_env_error(value)),
        None => Ok(()),
    }
}

pub(crate) fn reject_legacy_pool_env() -> Result<()> {
    let raw = std::env::var("NEUROSTRATA_BUFFER_POOL_MB").ok();
    reject_legacy_pool_env_at(raw.as_deref())
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Config {
    #[serde(default)]
    pub db_path: PathBuf,

    #[serde(default)]
    pub deduplication: DeduplicationConfig,

    #[serde(default)]
    pub project_contexts: HashMap<String, PathBuf>,

    /// Directories outside the served project root that daemon callers may
    /// name — backup destinations, beads files, ingest trees (Task 2 / J6).
    /// Empty by default: a caller-supplied path must resolve inside the
    /// project root. This list is the ONLY escape hatch, and every path
    /// refusal the daemon issues names this key.
    #[serde(default)]
    pub allowed_external_dirs: Vec<PathBuf>,

    /// Maximum Levenshtein distance for fuzzy lexical clauses, clamped 0..=2.
    #[serde(default = "default_lexical_fuzzy_max_distance")]
    pub lexical_fuzzy_max_distance: usize,

    /// Hybrid scoring weights (vector, graph, symbol). Must sum to 1.0.
    /// Defaults (0.5, 0.3, 0.2) per spec §6.2.
    #[serde(default = "default_hybrid_weights")]
    pub hybrid_weights: (f32, f32, f32),

    #[serde(default)]
    pub storage_hygiene: crate::storage::snapshot::StorageHygieneConfig,

    #[serde(default)]
    pub store: StoreConfig,

    #[serde(default)]
    pub rag_chat: crate::rag_chat::RagChatConfig,

    #[serde(default)]
    pub multimodal: crate::multimodal::MultimodalConfig,

    #[serde(default)]
    pub federation_allowed_hosts: Vec<String>,

    #[serde(default)]
    pub mesh_signing_key_path: Option<PathBuf>,

    #[serde(default = "default_metrics_ring_capacity")]
    pub metrics_ring_capacity: usize,

    #[serde(default = "default_metrics_ewma_alpha")]
    pub metrics_ewma_alpha: f64,

    #[serde(default = "default_metrics_flush_interval_secs")]
    pub metrics_flush_interval_secs: u64,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_metrics_ring_capacity() -> usize {
    1024
}

fn default_metrics_ewma_alpha() -> f64 {
    0.2
}

fn default_metrics_flush_interval_secs() -> u64 {
    60
}

fn default_hybrid_weights() -> (f32, f32, f32) {
    (0.5, 0.3, 0.2)
}

fn default_lexical_fuzzy_max_distance() -> usize {
    2
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let home_dir = dirs::home_dir().context("Could not find home directory")?;
        Ok(home_dir
            .join(".config")
            .join("neurostrata")
            .join("config.json"))
    }

    pub fn from_default_path() -> Result<Self> {
        let home_dir = dirs::home_dir().context("Could not find home directory")?;
        let config_path = Self::default_path()?;

        if !config_path.exists() {
            let default_config = Config {
                db_path: home_dir
                    .join(".config")
                    .join("NeuroStrata")
                    .join("data")
                    .join("db")
                    .join("ladybug.db"),
                deduplication: DeduplicationConfig::default(),
                project_contexts: HashMap::new(),
                allowed_external_dirs: Vec::new(),
                lexical_fuzzy_max_distance: default_lexical_fuzzy_max_distance(),
                hybrid_weights: default_hybrid_weights(),
                storage_hygiene: crate::storage::snapshot::StorageHygieneConfig::default(),
                store: StoreConfig::default(),
                rag_chat: crate::rag_chat::RagChatConfig::default(),
                multimodal: crate::multimodal::MultimodalConfig::default(),
                federation_allowed_hosts: Vec::new(),
                mesh_signing_key_path: None,
                metrics_ring_capacity: default_metrics_ring_capacity(),
                metrics_ewma_alpha: default_metrics_ewma_alpha(),
                metrics_flush_interval_secs: default_metrics_flush_interval_secs(),
                extra: HashMap::new(),
            };

            if let Some(parent) = config_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Some(db_parent) = default_config.db_path.parent() {
                fs::create_dir_all(db_parent)?;
            }

            default_config.save_to(&config_path)?;

            return Ok(default_config);
        }

        Self::load_from(&config_path)
    }

    /// Load from the default path WITHOUT creating a default config file
    /// (unlike `from_default_path`, which materializes one when missing).
    pub fn load() -> Result<Self> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Could not read config at {}", path.display()))?;
        reject_legacy_pool_env()?;
        let mut config: Config = serde_json::from_str(&content)?;

        if let Some(mb) = config.storage_hygiene.legacy_buffer_pool_mb {
            anyhow::bail!(
                "{}",
                legacy_pool_error(
                    "storage_hygiene.buffer_pool_mb",
                    "store.buffer_pool_bytes",
                    &mb.to_string(),
                    mb,
                )
            );
        }

        if config.db_path.is_dir() {
            config.db_path = config.db_path.join("ladybug.db");
        }
        config.lexical_fuzzy_max_distance = config.lexical_fuzzy_max_distance.min(2);

        Ok(config)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("Could not create config directory {}", parent.display())
            })?;
        }

        let file_name = path
            .file_name()
            .context("Config path must name a file")?
            .to_string_lossy();
        let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
        let existing_permissions = match fs::metadata(path) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Could not inspect config permissions at {}", path.display())
                })
            }
        };
        let result = (|| -> Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp_path).with_context(|| {
                format!("Could not create temporary config {}", temp_path.display())
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .context("Could not restrict temporary config permissions")?;
            }
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
            if let Some(permissions) = existing_permissions {
                file.set_permissions(permissions).with_context(|| {
                    format!(
                        "Could not preserve config permissions at {}",
                        path.display()
                    )
                })?;
            }
            file.sync_all()?;
            fs::rename(&temp_path, path)
                .with_context(|| format!("Could not replace config at {}", path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    pub fn bind_project_context(&mut self, source_id: &str, marker_path: &Path) -> Result<()> {
        if source_id.trim().is_empty() {
            anyhow::bail!("Project context source ID must not be empty");
        }
        let metadata = fs::metadata(marker_path).with_context(|| {
            format!(
                "Project context marker does not exist: {}",
                marker_path.display()
            )
        })?;
        if !metadata.is_dir() {
            anyhow::bail!(
                "Project context marker is not a directory: {}",
                marker_path.display()
            );
        }
        if marker_path.file_name().and_then(|name| name.to_str()) != Some(".NeuroStrata") {
            anyhow::bail!(
                "Project context path must end in .NeuroStrata: {}",
                marker_path.display()
            );
        }

        let canonical_path = fs::canonicalize(marker_path)
            .with_context(|| format!("Could not canonicalize {}", marker_path.display()))?;
        if canonical_path.file_name().and_then(|name| name.to_str()) != Some(".NeuroStrata") {
            anyhow::bail!(
                "Canonical project context path must end in .NeuroStrata: {}",
                marker_path.display()
            );
        }
        self.project_contexts
            .insert(source_id.to_string(), canonical_path);
        Ok(())
    }

    pub fn unbind_project_context(&mut self, source_id: &str) -> Result<()> {
        if self.project_contexts.remove(source_id).is_none() {
            anyhow::bail!("Unknown project context source ID: {source_id}");
        }
        Ok(())
    }
}

/// How long a `config.json.lock` may sit before it is taken as left behind by
/// a process that died holding it.
const CONFIG_LOCK_STALE: Duration = Duration::from_secs(60);

/// How long [`ConfigLock::acquire`] waits for a live lock.
const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(30);

/// Poll interval while waiting for a contended lock.
const CONFIG_LOCK_POLL: Duration = Duration::from_millis(25);

/// Poll interval while retrying a transient lock on a freshly created file.
const LOCK_RETRY_POLL: Duration = Duration::from_millis(5);

/// An exclusive lock on the user config: the file `<config>.lock` beside it.
/// The file carries an OS advisory lock held for the guard's whole life, plus
/// a nonce. Mtime alone cannot tell a paused holder from a dead one, and a
/// `Drop` that unlinks whatever sits at the path can delete a successor's
/// lock; both are prevented here. A read-check-write of the config holds the
/// guard so two writers cannot overwrite each other in between.
pub struct ConfigLock {
    path: PathBuf,
    /// Keeps the advisory lock until the guard is dropped.
    file: Option<std::fs::File>,
    nonce: String,
}

impl ConfigLock {
    pub fn acquire(config_path: &Path) -> Result<Self> {
        let name = config_path
            .file_name()
            .context("Config path must name a file")?
            .to_string_lossy()
            .into_owned();
        let path = config_path.with_file_name(format!("{name}.lock"));
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| {
                format!("Could not create config directory {}", parent.display())
            })?;
        }
        let nonce = format!("{}:{}", std::process::id(), uuid::Uuid::new_v4());
        let deadline = Instant::now() + CONFIG_LOCK_WAIT;
        loop {
            // Read access lets Drop check the nonce through this handle on
            // Windows, where the lock refuses reads through any other.
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return finish_created_lock(file, path, nonce, deadline),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    // A live holder owns the advisory lock even when its mtime
                    // is old (a paused process). Only a stale, unheld lock is
                    // breakable, and the break re-validates atomically so a
                    // writer cannot be unlinked out from under it.
                    if try_break_stale_lock(&path) {
                        continue;
                    }
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "the user config is locked by {} (another NeuroStrata command is writing it)",
                            path.display()
                        );
                    }
                    std::thread::sleep(CONFIG_LOCK_POLL);
                }
                // Windows answers "access denied" while another process's
                // delete or rename of the lock file is still pending: the
                // lock is changing hands, so wait like for a held lock.
                Err(e)
                    if cfg!(windows)
                        && e.kind() == ErrorKind::PermissionDenied
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(CONFIG_LOCK_POLL);
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("Could not create {}", path.display()))
                }
            }
        }
    }
}

/// Finish claiming a lock file we created with `create_new`: write our nonce,
/// acquire the advisory lock (retrying a transient free-probe), and reclaim the
/// path on every error so a failed acquisition never strands a lock file.
fn finish_created_lock(
    mut file: std::fs::File,
    path: PathBuf,
    nonce: String,
    deadline: Instant,
) -> Result<ConfigLock> {
    if let Err(e) = writeln!(file, "{nonce}") {
        // We created this path exclusively; no successor can own it yet.
        let _ = fs::remove_file(&path);
        return Err(e).with_context(|| format!("Could not write {}", path.display()));
    }
    if let Err(e) = lock_created_file(&mut file, deadline) {
        // Reclaim only if the path still carries our nonce.
        if lock_nonce_is(&path, &nonce) {
            let _ = fs::remove_file(&path);
        }
        return Err(e).with_context(|| format!("Could not lock {}", path.display()));
    }
    Ok(ConfigLock {
        path,
        file: Some(file),
        nonce,
    })
}

/// Acquire the advisory lock on a file we just created. A concurrent waiter's
/// free-probe can briefly hold that lock; retry the transient contention
/// instead of failing a valid config write.
fn lock_created_file(file: &mut std::fs::File, deadline: Instant) -> std::io::Result<()> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(e) => {
                let retryable = match &e {
                    std::fs::TryLockError::WouldBlock => true,
                    std::fs::TryLockError::Error(io) => {
                        io.kind() == ErrorKind::WouldBlock
                            || (cfg!(windows) && io.kind() == ErrorKind::PermissionDenied)
                    }
                };
                if retryable && Instant::now() < deadline {
                    std::thread::sleep(LOCK_RETRY_POLL);
                    continue;
                }
                return Err(match e {
                    std::fs::TryLockError::WouldBlock => {
                        std::io::Error::new(ErrorKind::WouldBlock, "lock is held")
                    }
                    std::fs::TryLockError::Error(io) => io,
                });
            }
        }
    }
}

/// Whether the lock file at `path` still carries `nonce` (our identity).
fn lock_nonce_is(path: &Path, nonce: &str) -> bool {
    fs::read_to_string(path)
        .map(|text| text.trim() == nonce)
        .unwrap_or(false)
}

/// Whether the lock file we hold open still carries `nonce`, read through our
/// own handle. While we hold the lock, no acquirer or breaker can replace the
/// path, so this is the file the path names.
#[cfg(not(unix))]
fn held_nonce_is(file: &std::fs::File, nonce: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    let mut text = String::new();
    file.seek(SeekFrom::Start(0)).is_ok()
        && file.read_to_string(&mut text).is_ok()
        && text.trim() == nonce
}

/// Whether nobody holds the advisory lock on an existing lock file. A file
/// that cannot be opened cannot be proven free, so it stays untouched.
#[cfg(test)]
fn lock_is_free(path: &Path) -> bool {
    match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => matches!(file.try_lock(), Ok(())),
        Err(_) => false,
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > CONFIG_LOCK_STALE)
}

/// True when `path` still names the same inode as the open `file` handle.
#[cfg(unix)]
fn same_file_at_path(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
        _ => false,
    }
}

/// Break the stale lock at `path`, if any.
///
/// Serialized by a short-lived sibling `.break` lock. Under that guard the
/// lock file is re-checked for staleness and acquired with the OS advisory
/// lock; the path is unlinked *while that advisory lock is held*, so a
/// writer cannot acquire the file between the check and the unlink. On Unix
/// the inode is re-validated against `path` before removal. A `.break` file older than
/// the stale limit was left by a process that died mid-break and is removed.
fn try_break_stale_lock(path: &Path) -> bool {
    let mut breaker = path.as_os_str().to_owned();
    breaker.push(".break");
    let breaker = PathBuf::from(breaker);
    let breaker_guard = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&breaker)
    {
        Ok(file) => file,
        Err(_) => {
            if is_stale(&breaker) {
                let _ = fs::remove_file(&breaker);
            }
            return false;
        }
    };

    let broke = (|| -> bool {
        if !is_stale(path) {
            return false;
        }
        let file = match fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(_) => return false,
        };
        // Proves the stale file is unheld; keep the lock through the unlink.
        if file.try_lock().is_err() {
            return false;
        }
        #[cfg(unix)]
        {
            if !same_file_at_path(&file, path) {
                return false;
            }
            let removed = fs::remove_file(path).is_ok();
            drop(file); // release the advisory lock on the now-unlinked inode
            removed
        }
        #[cfg(not(unix))]
        {
            // As on Unix, unlink while the lock is held so no writer can take
            // the path in between. Windows allows it: the file was opened with
            // delete sharing. Windows has no stable same-file check, so
            // re-check staleness: if the holder unlinked the file between our
            // open and our lock, a successor's fresh file now names the path.
            if !is_stale(path) {
                drop(file);
                return false;
            }
            let removed = fs::remove_file(path).is_ok();
            drop(file);
            removed
        }
    })();

    drop(breaker_guard);
    let _ = fs::remove_file(&breaker);
    broke
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        // Re-validate identity and unlink while the advisory lock is still
        // held, so a successor cannot acquire the file (or replace it) between
        // the check and the removal and get its own lock file unlinked.
        let Some(file) = self.file.take() else {
            return;
        };
        #[cfg(unix)]
        {
            if !lock_nonce_is(&self.path, &self.nonce) {
                // Somebody replaced the path; it is theirs, not ours to unlink.
                return;
            }
            let _ = fs::remove_file(&self.path);
            drop(file); // release the advisory lock on the now-unlinked inode
        }
        #[cfg(not(unix))]
        {
            // The lock refuses reads through any other handle, so check the
            // nonce through ours; then unlink while still holding the lock
            // (the file was opened with delete sharing), as on Unix.
            if !held_nonce_is(&file, &self.nonce) {
                return;
            }
            let _ = fs::remove_file(&self.path);
            drop(file);
        }
    }
}

/// What [`bind_project_context_if_free`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum BindOutcome {
    Bound,
    AlreadyBoundHere,
    /// The id is bound to this other `.NeuroStrata` folder, which exists.
    LiveElsewhere(PathBuf),
}

/// Bind `source_id` to the `.NeuroStrata` folder `marker_dir` only when the id
/// is unbound or bound to a folder that no longer exists; a live binding is
/// never replaced (spec section 4). The check and the write hold
/// [`ConfigLock`].
pub fn bind_project_context_if_free(source_id: &str, marker_dir: &Path) -> Result<BindOutcome> {
    bind_project_context_if_free_at(&Config::default_path()?, source_id, marker_dir)
}

pub(crate) fn bind_project_context_if_free_at(
    config_path: &Path,
    source_id: &str,
    marker_dir: &Path,
) -> Result<BindOutcome> {
    let _lock = ConfigLock::acquire(config_path)?;
    let mut config = Config::load_from(config_path)?;
    let wanted = fs::canonicalize(marker_dir)
        .with_context(|| format!("Could not canonicalize {}", marker_dir.display()))?;
    if let Some(bound) = config.project_contexts.get(source_id) {
        if fs::canonicalize(bound).is_ok_and(|b| b == wanted) {
            return Ok(BindOutcome::AlreadyBoundHere);
        }
        if bound.exists() {
            return Ok(BindOutcome::LiveElsewhere(bound.clone()));
        }
    }
    config.bind_project_context(source_id, marker_dir)?;
    config.save_to(config_path)?;
    Ok(BindOutcome::Bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ns-project-context-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Issue #414: the byte-denominated store settings parse from the
    /// `store` object, and a config that predates them gets `None` for both
    /// (resolution then applies the defaults).
    #[test]
    fn store_limits_parse_from_json() {
        let config: Config = serde_json::from_str(
            r#"{"db_path":"test.db","store":{"buffer_pool_bytes":536870912,"max_db_bytes":1073741824}}"#,
        )
        .unwrap();
        assert_eq!(config.store.buffer_pool_bytes, Some(512 * 1024 * 1024));
        assert_eq!(config.store.max_db_bytes, Some(1024 * 1024 * 1024));

        let bare: Config = serde_json::from_str(r#"{"db_path":"test.db"}"#).unwrap();
        assert_eq!(bare.store, StoreConfig::default());
        assert!(bare.store.buffer_pool_bytes.is_none());
        assert!(bare.store.max_db_bytes.is_none());
    }

    /// Owner ruling 2026-11-27 (#414): the megabyte key is not configuration.
    /// It is recognized only to fail with the exact byte replacement, whether
    /// or not a byte setting is also present.
    #[test]
    fn a_legacy_megabyte_pool_key_is_rejected_with_a_byte_migration_error() {
        let dir = temp_dir("legacy-pool");
        let path = dir.join("config.json");
        let legacy = r#"{"db_path":"test.db","store":{"buffer_pool_bytes":null},"storage_hygiene":{"buffer_pool_mb":768}}"#;
        fs::write(&path, legacy).unwrap();
        let err = Config::load_from(&path).unwrap_err().to_string();
        assert!(err.contains("storage_hygiene.buffer_pool_mb"), "{err}");
        assert!(err.contains("store.buffer_pool_bytes=805306368"), "{err}");
        assert!(err.contains("no longer accepted"), "{err}");

        let both = r#"{"db_path":"test.db","store":{"buffer_pool_bytes":536870912},"storage_hygiene":{"buffer_pool_mb":768}}"#;
        fs::write(&path, both).unwrap();
        let err = Config::load_from(&path).unwrap_err().to_string();
        assert!(err.contains("store.buffer_pool_bytes=805306368"), "{err}");
    }

    /// The byte-only setting loads and is the only pool key that round-trips.
    #[test]
    fn byte_pool_settings_load_without_any_legacy_field() {
        let dir = temp_dir("byte-pool");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"db_path":"test.db","store":{"buffer_pool_bytes":536870912}}"#,
        )
        .unwrap();
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.store.buffer_pool_bytes, Some(536870912));
        let serialized = serde_json::to_string(&config).unwrap();
        assert!(!serialized.contains("buffer_pool_mb"), "{serialized}");
    }

    #[test]
    fn config_without_project_contexts_deserializes_empty_registry() {
        let config: Config = serde_json::from_str(r#"{"db_path":"db"}"#).unwrap();
        assert!(config.project_contexts.is_empty());
    }

    /// Task 2 (J6): the caller-path allowlist defaults to empty — a config
    /// that never names `allowed_external_dirs` permits nothing outside the
    /// project root — and parses as a list of paths when present.
    #[test]
    fn allowed_external_dirs_defaults_empty_and_parses() {
        let config: Config = serde_json::from_str(r#"{"db_path":"db"}"#).unwrap();
        assert!(
            config.allowed_external_dirs.is_empty(),
            "unconfigured means no external directory is permitted"
        );
        let config: Config = serde_json::from_str(
            r#"{"db_path":"db","allowed_external_dirs":["/srv/backups","/mnt/beads"]}"#,
        )
        .unwrap();
        assert_eq!(
            config.allowed_external_dirs,
            vec![PathBuf::from("/srv/backups"), PathBuf::from("/mnt/beads")]
        );
    }

    #[test]
    fn bind_project_context_requires_nonempty_id_and_existing_marker() {
        let root = temp_dir("validation");
        let marker = root.join(".NeuroStrata");
        fs::create_dir(&marker).unwrap();
        let regular_file = root.join("file");
        fs::write(&regular_file, "not a directory").unwrap();
        let config_path = write_config(&root);
        let mut config = Config::load_from(&config_path).unwrap();

        assert!(config.bind_project_context("", &marker).is_err());
        assert!(config.bind_project_context("  \t", &marker).is_err());
        assert!(config
            .bind_project_context("missing", &root.join("absent/.NeuroStrata"))
            .is_err());
        assert!(config.bind_project_context("file", &regular_file).is_err());
        assert!(config.bind_project_context("wrong-name", &root).is_err());
    }

    #[test]
    fn bind_project_context_stores_canonical_marker_and_preserves_other_config() {
        let root = temp_dir("canonical");
        let marker = root.join("project/.NeuroStrata");
        fs::create_dir_all(&marker).unwrap();
        let config_path = write_config_with(
            &root,
            r#"{"db_path":"custom.db","deduplication":{"judgment_api_key":"keep-me","similarity_threshold":0.42}}"#,
        );
        let mut config = Config::load_from(&config_path).unwrap();

        config.bind_project_context("source", &marker).unwrap();

        assert_eq!(
            config.project_contexts.get("source"),
            Some(&fs::canonicalize(&marker).unwrap())
        );
        assert_eq!(config.db_path, PathBuf::from("custom.db"));
        assert_eq!(
            config.deduplication.judgment_api_key.as_deref(),
            Some("keep-me")
        );
        assert_eq!(config.deduplication.similarity_threshold, 0.42);
    }

    #[test]
    fn bind_and_save_preserves_unknown_top_level_config_fields() {
        let root = temp_dir("unknown-fields");
        let marker = root.join("project/.NeuroStrata");
        fs::create_dir_all(&marker).unwrap();
        let config_path = write_config_with(
            &root,
            r#"{"db_path":"custom.db","future_extension":{"enabled":true,"options":[1,"two"]}}"#,
        );
        let mut config = Config::load_from(&config_path).unwrap();

        config.bind_project_context("source", &marker).unwrap();
        config.save_to(&config_path).unwrap();

        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        assert_eq!(
            saved["future_extension"],
            serde_json::json!({"enabled": true, "options": [1, "two"]})
        );
    }

    #[test]
    fn invalid_bind_or_unbind_leaves_config_bytes_unchanged() {
        let root = temp_dir("unchanged");
        let config_path = write_config(&root);
        let original = fs::read(&config_path).unwrap();
        let marker = root.join(".NeuroStrata");
        fs::create_dir(&marker).unwrap();

        let mut config = Config::load_from(&config_path).unwrap();
        assert!(config.bind_project_context("", &marker).is_err());
        assert!(config.unbind_project_context("unknown").is_err());

        assert_eq!(fs::read(&config_path).unwrap(), original);
    }

    #[test]
    fn project_context_unbind_removes_only_requested_id() {
        let root = temp_dir("unbind");
        let first = root.join("first/.NeuroStrata");
        let second = root.join("second/.NeuroStrata");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let config_path = write_config(&root);
        let mut config = Config::load_from(&config_path).unwrap();
        config.bind_project_context("first", &first).unwrap();
        config.bind_project_context("second", &second).unwrap();

        config.unbind_project_context("first").unwrap();

        assert!(!config.project_contexts.contains_key("first"));
        assert_eq!(
            config.project_contexts.get("second"),
            Some(&fs::canonicalize(&second).unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_to_preserves_existing_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("permissions");
        let config_path = write_config(&root);
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = Config::load_from(&config_path).unwrap();

        config.save_to(&config_path).unwrap();

        let mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_creates_owner_only_config() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("new-permissions");
        let config_path = root.join("config.json");
        let config = Config {
            db_path: PathBuf::from("test.db"),
            deduplication: DeduplicationConfig::default(),
            project_contexts: HashMap::new(),
            allowed_external_dirs: Vec::new(),
            lexical_fuzzy_max_distance: default_lexical_fuzzy_max_distance(),
            hybrid_weights: default_hybrid_weights(),
            storage_hygiene: crate::storage::StorageHygieneConfig::default(),
            store: StoreConfig::default(),
            rag_chat: crate::rag_chat::RagChatConfig::default(),
            multimodal: crate::multimodal::MultimodalConfig::default(),
            federation_allowed_hosts: Vec::new(),
            mesh_signing_key_path: None,
            metrics_ring_capacity: default_metrics_ring_capacity(),
            metrics_ewma_alpha: default_metrics_ewma_alpha(),
            metrics_flush_interval_secs: default_metrics_flush_interval_secs(),
            extra: HashMap::new(),
        };

        config.save_to(&config_path).unwrap();

        let mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    fn write_config(root: &Path) -> PathBuf {
        write_config_with(root, r#"{"db_path":"test.db"}"#)
    }

    fn write_config_with(root: &Path, contents: &str) -> PathBuf {
        let path = root.join("config.json");
        fs::write(&path, contents).unwrap();
        path
    }

    // Task 5 Step 1: provider selection config tests. These tests FAIL until
    // Step 2 adds `provider` and `judgment_api_key_env` to DeduplicationConfig.
    // They are written first so the spec's sequence is honored.

    /// Old JSON with a literal `judgment_api_key` must deserialize safely for
    /// one compatibility release WITHOUT auto-selecting Jev.
    #[test]
    fn dedup_provider_config_legacy_literal_key_does_not_auto_select_jev() {
        let config: Config = serde_json::from_str(
            r#"{"db_path":"test.db","deduplication":{"judgment_api_key":"old-literal-key"}}"#,
        )
        .unwrap();
        assert_eq!(
            config.deduplication.judgment_api_key.as_deref(),
            Some("old-literal-key")
        );
        assert_ne!(
            config.deduplication.provider,
            DedupProviderMode::Jev,
            "legacy literal key must not auto-select Jev"
        );
    }

    /// Default config without a key must select `auto` (not Jev, not None).
    #[test]
    fn dedup_provider_config_default_auto_without_key() {
        let config: Config = serde_json::from_str(r#"{"db_path":"test.db"}"#).unwrap();
        assert_eq!(config.deduplication.provider, DedupProviderMode::Auto);
        assert!(config.deduplication.judgment_api_key.is_none());
        assert!(config.deduplication.judgment_api_key_env.is_none());
    }

    /// Explicit `jev` with an env var reference.
    #[test]
    fn dedup_provider_config_explicit_jev_with_env_key() {
        std::env::set_var("TYPESAFE_API_KEY", "env-key-123");
        let config: Config = serde_json::from_str(
            r#"{"db_path":"test.db","deduplication":{"provider":"jev","judgment_api_key_env":"TYPESAFE_API_KEY"}}"#,
        )
        .unwrap();
        assert_eq!(config.deduplication.provider, DedupProviderMode::Jev);
        assert_eq!(
            config.deduplication.judgment_api_key_env.as_deref(),
            Some("TYPESAFE_API_KEY")
        );
        std::env::remove_var("TYPESAFE_API_KEY");
    }

    /// Explicit `jev` without a key must NOT auto-failover.
    #[test]
    fn dedup_provider_config_jev_without_key_does_not_failover() {
        std::env::remove_var("TYPESAFE_API_KEY");
        let config: Config =
            serde_json::from_str(r#"{"db_path":"test.db","deduplication":{"provider":"jev"}}"#)
                .unwrap();
        assert_eq!(config.deduplication.provider, DedupProviderMode::Jev);
        assert!(config.deduplication.judgment_api_key.is_none());
        assert!(config.deduplication.judgment_api_key_env.is_none());
    }

    /// `none` disables dedup learning entirely.
    #[test]
    fn dedup_provider_config_none_disables_dedup_learning() {
        let config: Config =
            serde_json::from_str(r#"{"db_path":"test.db","deduplication":{"provider":"none"}}"#)
                .unwrap();
        assert_eq!(config.deduplication.provider, DedupProviderMode::None);
    }

    /// `similarity_threshold` is deprecated and must not be applied to raw
    /// distance.
    #[test]
    fn dedup_provider_config_similarity_threshold_deprecated() {
        let config: Config = serde_json::from_str(
            r#"{"db_path":"test.db","deduplication":{"similarity_threshold":0.95}}"#,
        )
        .unwrap();
        assert_eq!(config.deduplication.similarity_threshold, 0.95);
        assert_eq!(config.deduplication.provider, DedupProviderMode::Auto);
    }

    #[test]
    fn bind_if_free_never_overwrites_live_binding() {
        let root = temp_dir("bind-if-free-live");
        let first = root.join("first/.NeuroStrata");
        let second = root.join("second/.NeuroStrata");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let config_path = write_config(&root);

        assert_eq!(
            bind_project_context_if_free_at(&config_path, "example-project", &first).unwrap(),
            BindOutcome::Bound
        );
        assert_eq!(
            bind_project_context_if_free_at(&config_path, "example-project", &first).unwrap(),
            BindOutcome::AlreadyBoundHere
        );
        let live = fs::canonicalize(&first).unwrap();
        assert_eq!(
            bind_project_context_if_free_at(&config_path, "example-project", &second).unwrap(),
            BindOutcome::LiveElsewhere(live.clone())
        );
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.project_contexts.get("example-project"), Some(&live));
        assert!(
            !config_path.with_file_name("config.json.lock").exists(),
            "lock released"
        );
    }

    #[test]
    fn bind_if_free_rebinds_missing_folder() {
        let root = temp_dir("bind-if-free-missing");
        let gone = root.join("gone/.NeuroStrata");
        let here = root.join("here/.NeuroStrata");
        fs::create_dir_all(&gone).unwrap();
        fs::create_dir_all(&here).unwrap();
        let config_path = write_config(&root);
        bind_project_context_if_free_at(&config_path, "example-project", &gone).unwrap();
        fs::remove_dir_all(root.join("gone")).unwrap();

        assert_eq!(
            bind_project_context_if_free_at(&config_path, "example-project", &here).unwrap(),
            BindOutcome::Bound
        );
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.project_contexts.get("example-project"),
            Some(&fs::canonicalize(&here).unwrap())
        );
    }

    #[test]
    fn concurrent_explicit_bind_survives() {
        let root = temp_dir("bind-if-free-concurrent");
        let explicit = root.join("explicit/.NeuroStrata");
        let other = root.join("other/.NeuroStrata");
        fs::create_dir_all(&explicit).unwrap();
        fs::create_dir_all(&other).unwrap();
        let config_path = write_config(&root);

        // A holds the lock across its read-modify-write, as `project-context
        // bind` does; B starts while A holds it.
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let a = {
            let (config_path, explicit) = (config_path.clone(), explicit.clone());
            std::thread::spawn(move || {
                let _lock = ConfigLock::acquire(&config_path).unwrap();
                locked_tx.send(()).unwrap();
                let mut config = Config::load_from(&config_path).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                config
                    .bind_project_context("example-project", &explicit)
                    .unwrap();
                config.save_to(&config_path).unwrap();
            })
        };
        locked_rx.recv().unwrap();
        let outcome =
            bind_project_context_if_free_at(&config_path, "example-project", &other).unwrap();
        a.join().unwrap();

        let live = fs::canonicalize(&explicit).unwrap();
        assert_eq!(outcome, BindOutcome::LiveElsewhere(live.clone()));
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.project_contexts.get("example-project"), Some(&live));
    }

    #[test]
    fn waiters_on_a_stale_config_lock_hold_it_one_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for round in 0..5 {
            let root = temp_dir("config-lock-stale-race");
            let config_path = write_config(&root);
            let lock = config_path.with_file_name("config.json.lock");
            fs::write(&lock, "12345").unwrap();
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
            fs::File::options()
                .write(true)
                .open(&lock)
                .unwrap()
                .set_modified(old)
                .unwrap();

            let inside = std::sync::Arc::new(AtomicUsize::new(0));
            let most = std::sync::Arc::new(AtomicUsize::new(0));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
            let waiters: Vec<_> = (0..4)
                .map(|_| {
                    let (config_path, inside, most, barrier) = (
                        config_path.clone(),
                        inside.clone(),
                        most.clone(),
                        barrier.clone(),
                    );
                    std::thread::spawn(move || {
                        barrier.wait();
                        let _held = ConfigLock::acquire(&config_path).unwrap();
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        inside.fetch_sub(1, Ordering::SeqCst);
                    })
                })
                .collect();
            for w in waiters {
                w.join().unwrap();
            }
            assert_eq!(most.load(Ordering::SeqCst), 1, "round {round}");
            let left: Vec<_> = fs::read_dir(&root)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".lock"))
                .collect();
            assert!(left.is_empty(), "round {round}: {left:?}");
        }
    }

    #[test]
    fn stale_config_lock_is_replaced() {
        let root = temp_dir("config-lock-stale");
        let config_path = write_config(&root);
        let lock = config_path.with_file_name("config.json.lock");
        fs::write(&lock, "12345").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let held = ConfigLock::acquire(&config_path).unwrap();
        assert!(lock.exists());
        drop(held);
        assert!(!lock.exists());
    }

    #[test]
    fn a_held_config_lock_is_not_broken_by_an_old_mtime() {
        let root = temp_dir("config-lock-held-not-stale");
        let config_path = write_config(&root);
        let lock = config_path.with_file_name("config.json.lock");
        let held = ConfigLock::acquire(&config_path).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // Age alone must not make a live holder's lock breakable.
        assert!(!lock_is_free(&lock));
        drop(held);
        assert!(!lock.exists());
    }

    #[test]
    fn dropping_a_config_lock_does_not_remove_a_successors_file() {
        let root = temp_dir("config-lock-foreign-successor");
        let config_path = write_config(&root);
        let lock = config_path.with_file_name("config.json.lock");
        let held = ConfigLock::acquire(&config_path).unwrap();
        // A paused holder's path is overwritten by another writer. Its nonce
        // differs, so our Drop must leave it alone. Written through the held
        // handle: on Windows the held lock refuses writes through any other.
        {
            use std::io::{Seek, SeekFrom};
            let mut file = held.file.as_ref().unwrap();
            file.set_len(0).unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"9999:successor").unwrap();
        }
        drop(held);
        assert!(lock.exists(), "a foreign lock file must survive our drop");
        fs::remove_file(&lock).unwrap();
    }

    // PR #400 bot findings. Concurrency regressions: no sleeps, channels and
    // barriers only, so the races are exercised deterministically.

    /// [Codex P2 / 4670088580] A concurrent free-probe can briefly hold the
    /// advisory lock on a file we just created; `lock_created_file` must retry
    /// that transient `WouldBlock` instead of failing the acquisition.
    #[test]
    fn transient_holder_on_created_file_is_retried() {
        use std::sync::mpsc;

        let root = temp_dir("config-lock-retry");
        let path = root.join("created.lock");
        fs::write(&path, b"seed").unwrap();

        // Establish real contention: a separate fd holds the advisory lock.
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder_path = path.clone();
        let holder = std::thread::spawn(move || {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&holder_path)
                .unwrap();
            file.try_lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            // `file` drops here -> advisory lock released
        });
        locked_rx.recv().unwrap();

        let mut created = fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(
            created.try_lock().is_err(),
            "contention must be established before the retry is exercised"
        );

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let acquirer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            lock_created_file(&mut created, Instant::now() + Duration::from_secs(5)).unwrap();
            done_tx.send(()).unwrap();
        });

        started_rx.recv().unwrap();
        release_tx.send(()).unwrap();
        done_rx.recv().unwrap();
        holder.join().unwrap();
        acquirer.join().unwrap();
    }

    /// [Sourcery Medium / 4670091305] A failed post-create acquisition must
    /// reclaim the lock path it created, so later callers do not wait on a
    /// lock nobody holds.
    #[test]
    fn failed_lock_acquisition_reclaims_created_lock_path() {
        let root = temp_dir("config-lock-cleanup");
        let path = root.join("created.lock");
        fs::write(&path, b"").unwrap();

        // A different fd holds the advisory lock, so our handle cannot lock.
        let holder = fs::OpenOptions::new().write(true).open(&path).unwrap();
        holder.try_lock().unwrap();
        let created = fs::OpenOptions::new().write(true).open(&path).unwrap();

        let err = match finish_created_lock(
            created,
            path.clone(),
            format!("{}:nonce", std::process::id()),
            Instant::now(),
        ) {
            Ok(_) => panic!("expected the contended acquisition to fail"),
            Err(err) => err,
        };
        // On Windows the holder's lock also refuses our nonce write, so the
        // acquisition fails one step earlier; the path must be reclaimed
        // either way.
        let expected = if cfg!(windows) {
            "Could not write"
        } else {
            "Could not lock"
        };
        assert!(
            format!("{err:#}").contains(expected),
            "unexpected error: {err:#}"
        );
        assert!(
            !path.exists(),
            "a failed acquisition must reclaim the lock path it created"
        );
        drop(holder);
    }

    /// [Sourcery High / 4670091186 + 4670091174] Under heavy contention the
    /// break path and `Drop` must never double-hold or leave a lock file (or a
    /// `.break` sibling) behind.
    #[test]
    fn contended_acquire_and_drop_never_double_holds_or_leaves_locks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        for round in 0..8 {
            let root = temp_dir("config-lock-contention");
            let config_path = write_config(&root);
            let inside = Arc::new(AtomicUsize::new(0));
            let most = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(8));
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let (config_path, inside, most, barrier) = (
                        config_path.clone(),
                        inside.clone(),
                        most.clone(),
                        barrier.clone(),
                    );
                    std::thread::spawn(move || {
                        barrier.wait();
                        let _guard = ConfigLock::acquire(&config_path).unwrap();
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            assert_eq!(
                most.load(Ordering::SeqCst),
                1,
                "round {round}: mutual exclusion"
            );
            let leftovers: Vec<_> = fs::read_dir(&root)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".lock"))
                .collect();
            assert!(leftovers.is_empty(), "round {round}: {leftovers:?}");
        }
    }
}
