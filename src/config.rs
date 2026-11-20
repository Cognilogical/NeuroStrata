use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
        let mut config: Config = serde_json::from_str(&content)?;

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
                canonical_path.display()
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
const CONFIG_LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long [`ConfigLock::acquire`] waits for a live lock.
const CONFIG_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// An exclusive lock on the user config: the file `<config>.lock` beside it,
/// created with `create_new` and removed on drop; one left older than
/// [`CONFIG_LOCK_STALE`] by a dead process is broken (see
/// `break_stale_lock`). A read-check-write of the
/// config holds it so two writers cannot overwrite each other in between.
pub struct ConfigLock {
    path: PathBuf,
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
            fs::create_dir_all(parent)
                .with_context(|| format!("Could not create config directory {}", parent.display()))?;
        }
        let started = std::time::Instant::now();
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(ConfigLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&path) {
                        break_stale_lock(&path);
                        continue;
                    }
                    if started.elapsed() > CONFIG_LOCK_WAIT {
                        anyhow::bail!(
                            "the user config is locked by {} (another NeuroStrata command is writing it)",
                            path.display()
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                // Windows answers "access denied" while another process's
                // delete or rename of the lock file is still pending: the
                // lock is changing hands, so wait like for a held lock.
                Err(e)
                    if cfg!(windows)
                        && e.kind() == std::io::ErrorKind::PermissionDenied
                        && started.elapsed() <= CONFIG_LOCK_WAIT =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("Could not create {}", path.display()))
                }
            }
        }
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > CONFIG_LOCK_STALE)
}

/// Remove the stale lock at `path`, serialized by a short-lived second lock
/// (`<lock>.break`) and re-checked under it: two waiters that both saw the
/// same stale file cannot both remove "it", because the second one would
/// find the first one's successor fresh and leave it alone. A `.break` file
/// itself older than the stale limit was left by a process that died while
/// breaking, and is removed.
fn break_stale_lock(path: &Path) {
    let mut breaker = path.as_os_str().to_owned();
    breaker.push(".break");
    let breaker = PathBuf::from(breaker);
    match fs::OpenOptions::new().write(true).create_new(true).open(&breaker) {
        Ok(file) => {
            drop(file);
            if is_stale(path) {
                let _ = fs::remove_file(path);
            }
            let _ = fs::remove_file(&breaker);
        }
        Err(_) => {
            if is_stale(&breaker) {
                let _ = fs::remove_file(&breaker);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
        assert!(!config_path.with_file_name("config.json.lock").exists(), "lock released");
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
                config.bind_project_context("example-project", &explicit).unwrap();
                config.save_to(&config_path).unwrap();
            })
        };
        locked_rx.recv().unwrap();
        let outcome = bind_project_context_if_free_at(&config_path, "example-project", &other).unwrap();
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
            fs::File::options().write(true).open(&lock).unwrap().set_modified(old).unwrap();

            let inside = std::sync::Arc::new(AtomicUsize::new(0));
            let most = std::sync::Arc::new(AtomicUsize::new(0));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
            let waiters: Vec<_> = (0..4)
                .map(|_| {
                    let (config_path, inside, most, barrier) =
                        (config_path.clone(), inside.clone(), most.clone(), barrier.clone());
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
        fs::File::options().write(true).open(&lock).unwrap().set_modified(old).unwrap();
        let held = ConfigLock::acquire(&config_path).unwrap();
        assert!(lock.exists());
        drop(held);
        assert!(!lock.exists());
    }
}
