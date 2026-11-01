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

fn default_timeout() -> u64 { 5 }
fn default_similarity_threshold() -> f32 { 0.85 }
fn default_confidence_threshold() -> f32 { 0.8 }
fn default_laya_bundle_path() -> String { "artifacts/laya-bundle".to_string() }
fn default_max_candidates() -> usize { 5 }
fn default_cb_failure_threshold() -> u32 { 3 }
fn default_cb_backoff_initial() -> u64 { 30 }
fn default_cb_backoff_max() -> u64 { 3600 }

/// Pure resolution of the store ceilings, so the precedence order can be tested
/// without mutating the process environment.
///
/// Precedence: environment, then the config file, then the built-in default.
fn parse_limit(name: &str, raw: Option<&str>) -> Result<Option<u64>> {
    let Some(raw) = raw else { return Ok(None) };
    let value = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{name} must be a positive bounded byte count, got {raw:?}"))?;
    validate_limit(name, value)?;
    Ok(Some(value))
}

fn validate_limit(name: &str, value: u64) -> Result<()> {
    if value == 0 || value == u32::MAX as u64 {
        anyhow::bail!(
            "{name}={value} is an engine sentinel, not a bounded byte limit; use a positive byte count other than 0 or {}",
            u32::MAX
        );
    }
    Ok(())
}

fn resolve_store_limits(
    configured_pool: Option<u64>,
    configured_max_db: Option<u64>,
    env: impl Fn(&str) -> Result<Option<u64>>,
) -> Result<crate::store::StoreLimits> {
    let pool = env("NEUROSTRATA_BUFFER_POOL_BYTES")?.or(configured_pool);
    let max_db = env("NEUROSTRATA_MAX_DB_BYTES")?.or(configured_max_db);
    if let Some(value) = pool {
        validate_limit("store.buffer_pool_bytes", value)?;
    }
    if let Some(value) = max_db {
        validate_limit("store.max_db_bytes", value)?;
    }
    let limits = crate::store::StoreLimits {
        buffer_pool_bytes: pool.unwrap_or(crate::store::StoreLimits::DEFAULT_BUFFER_POOL_BYTES),
        max_db_bytes: max_db.unwrap_or(crate::store::StoreLimits::DEFAULT_MAX_DB_BYTES),
    };
    if limits.buffer_pool_bytes > limits.max_db_bytes {
        anyhow::bail!(
            "NEUROSTRATA_BUFFER_POOL_BYTES ({}) cannot exceed NEUROSTRATA_MAX_DB_BYTES ({})",
            limits.buffer_pool_bytes,
            limits.max_db_bytes
        );
    }
    Ok(limits)
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

/// A named statement of capacity intent (Spec Feature 1): `low` or
/// `standard`. A profile fills only the configuration slots the operator
/// left unset -- the embedder default in `crate::embed` and the store
/// limits below -- and never overrides an explicit value.
///
/// The values are *starting values, not derived constants*: they are a
/// coherent small-machine / workstation posture chosen by hand, and the
/// two-model validation matrix is the instrument that can later derive
/// them from measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityProfile {
    Low,
    Standard,
}

impl CapacityProfile {
    /// Parse a profile name, case-insensitively (consistent with
    /// `NEUROSTRATA_MODEL` matching). Anything else is a hard error at the
    /// surfaces that consume the profile.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "standard" => Ok(Self::Standard),
            _ => anyhow::bail!(
                "NEUROSTRATA_PROFILE must be \"low\" or \"standard\", got {raw:?}"
            ),
        }
    }

    /// Resolve an optional raw value: unset (or empty, matching the
    /// `NEUROSTRATA_MODEL` convention) means no profile.
    pub fn from_value(raw: Option<&str>) -> Result<Option<Self>> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(None),
            Some(value) => Ok(Some(Self::parse(value)?)),
        }
    }

    /// Read `NEUROSTRATA_PROFILE` from the process environment. Only
    /// profile-consuming callers (embedder construction, store opening,
    /// startup resolution) may call this: an invalid value must never fail
    /// a command that neither embeds nor opens a store (Spec AC6), so
    /// `Config::store_limits()` deliberately never does.
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var("NEUROSTRATA_PROFILE") {
            Ok(value) => Self::from_value(Some(&value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => anyhow::bail!("could not read NEUROSTRATA_PROFILE: {error}"),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
        }
    }

    /// The profile's model intent, resolved directly -- never looked up in
    /// the acceptable-embedders list, so `low` can name BGESmallENV15Q
    /// even when that model is not a list entry.
    pub fn model_name(&self) -> &'static str {
        match self {
            Self::Low => "BGESmallENV15Q",
            Self::Standard => "NomicEmbedTextV15",
        }
    }

    pub fn embedder_dimensions(&self) -> usize {
        match self {
            Self::Low => 384,
            Self::Standard => 768,
        }
    }

    pub fn buffer_pool_bytes(&self) -> u64 {
        match self {
            Self::Low => 64 * 1024 * 1024,
            Self::Standard => crate::store::StoreLimits::DEFAULT_BUFFER_POOL_BYTES,
        }
    }

    pub fn max_db_bytes(&self) -> u64 {
        match self {
            Self::Low => 2 * 1024 * 1024 * 1024,
            Self::Standard => crate::store::StoreLimits::DEFAULT_MAX_DB_BYTES,
        }
    }
}

/// Which layer supplied a resolved store limit, for the startup summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreLimitSource {
    Environment,
    ConfigFile,
    Profile,
    BuiltInDefault,
}

impl StoreLimitSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::ConfigFile => "config file",
            Self::Profile => "profile",
            Self::BuiltInDefault => "built-in default",
        }
    }
}

/// Per-limit provenance returned alongside resolved store limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimitSources {
    pub pool: StoreLimitSource,
    pub max_db: StoreLimitSource,
}

/// Store-limit resolution with the profile layer included: environment,
/// then the config file, then the profile (filling only `None` slots),
/// then the built-in defaults. Provenance is decided by which slot
/// supplied the value, never by comparing values -- an explicit value
/// equal to the profile's still came from the explicit layer.
fn resolve_store_limits_with_profile(
    configured_pool: Option<u64>,
    configured_max_db: Option<u64>,
    profile: Option<CapacityProfile>,
    env: impl Fn(&str) -> Result<Option<u64>>,
) -> Result<(crate::store::StoreLimits, StoreLimitSources)> {
    let env_pool = env("NEUROSTRATA_BUFFER_POOL_BYTES")?;
    let env_max_db = env("NEUROSTRATA_MAX_DB_BYTES")?;
    let limits = resolve_store_limits(
        configured_pool.or_else(|| profile.map(|p| p.buffer_pool_bytes())),
        configured_max_db.or_else(|| profile.map(|p| p.max_db_bytes())),
        |name| {
            Ok(match name {
                "NEUROSTRATA_BUFFER_POOL_BYTES" => env_pool,
                "NEUROSTRATA_MAX_DB_BYTES" => env_max_db,
                _ => None,
            })
        },
    )?;
    let source = |env_value: Option<u64>, configured: Option<u64>| {
        if env_value.is_some() {
            StoreLimitSource::Environment
        } else if configured.is_some() {
            StoreLimitSource::ConfigFile
        } else if profile.is_some() {
            StoreLimitSource::Profile
        } else {
            StoreLimitSource::BuiltInDefault
        }
    };
    Ok((
        limits,
        StoreLimitSources {
            pool: source(env_pool, configured_pool),
            max_db: source(env_max_db, configured_max_db),
        },
    ))
}

/// Memory ceilings for the embedded graph store.
///
/// Unset means "use the built-in ceiling", which is bounded on purpose: leaving
/// the engine's own defaults in place lets the buffer pool grow to 80% of
/// physical memory.
#[derive(Debug, Default, Deserialize, serde::Serialize, Clone)]
pub struct StoreConfig {
    #[serde(default)]
    pub buffer_pool_bytes: Option<u64>,
    #[serde(default)]
    pub max_db_bytes: Option<u64>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Config {
    #[serde(default)]
    pub db_path: PathBuf,

    #[serde(default)]
    pub deduplication: DeduplicationConfig,

    #[serde(default)]
    pub project_contexts: HashMap<String, PathBuf>,

    #[serde(default)]
    pub store: StoreConfig,
    /// Directories outside the served project root that daemon callers may
    /// name — backup destinations, beads files, ingest trees (Task 2 / J6).
    /// Empty by default: a caller-supplied path must resolve inside the
    /// project root. This list is the ONLY escape hatch, and every path
    /// refusal the daemon issues names this key.
    #[serde(default)]
    pub allowed_external_dirs: Vec<PathBuf>,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Config {
    /// Resolve the store's memory ceilings: environment overrides win over the
    /// config file, and the file wins over the built-in defaults.
    ///
    /// The environment exists so an operator can bound the pool without editing
    /// JSON, and so a one-off run can be retried with a smaller ceiling without
    /// changing the installed configuration.
    pub fn store_limits(&self) -> Result<crate::store::StoreLimits> {
        Ok(self.store_limits_resolved(None)?.0)
    }

    /// Store limits with a caller-supplied capacity profile applied to the
    /// slots the config file left unset. The profile arrives as a value,
    /// parsed once by the consuming caller: this method (like
    /// `store_limits()`) never reads `NEUROSTRATA_PROFILE` itself, because
    /// its callers include snapshot creation and the gate report, which
    /// must not fail on an invalid profile value (Spec AC6).
    pub fn store_limits_with_profile(
        &self,
        profile: Option<CapacityProfile>,
    ) -> Result<crate::store::StoreLimits> {
        Ok(self.store_limits_resolved(profile)?.0)
    }

    /// As `store_limits_with_profile`, but also reports which layer
    /// supplied each limit, for the startup summary line.
    pub fn store_limits_resolved(
        &self,
        profile: Option<CapacityProfile>,
    ) -> Result<(crate::store::StoreLimits, StoreLimitSources)> {
        resolve_store_limits_with_profile(
            self.store.buffer_pool_bytes,
            self.store.max_db_bytes,
            profile,
            |name| match std::env::var(name) {
                Ok(value) => parse_limit(name, Some(&value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => anyhow::bail!("could not read {name}: {error}"),
            },
        )
    }

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
                store: StoreConfig::default(),
                allowed_external_dirs: Vec::new(),
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

    pub fn load_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Could not read config at {}", path.display()))?;
        let mut config: Config = serde_json::from_str(&content)?;

        if config.db_path.is_dir() {
            config.db_path = config.db_path.join("ladybug.db");
        }

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

    /// A store section is optional, and omitting it must not mean "let the
    /// engine decide" -- it must mean the bounded default.
    #[test]
    fn a_config_without_a_store_section_resolves_to_the_bounded_default() {
        let config: Config = serde_json::from_str(r#"{"db_path":"db"}"#).unwrap();
        assert!(config.store.buffer_pool_bytes.is_none());

        let limits = resolve_store_limits(None, None, |_| Ok(None)).unwrap();
        assert_eq!(
            limits.buffer_pool_bytes,
            crate::store::StoreLimits::DEFAULT_BUFFER_POOL_BYTES
        );
        assert_eq!(
            limits.max_db_bytes,
            crate::store::StoreLimits::DEFAULT_MAX_DB_BYTES
        );
    }

    #[test]
    fn store_limit_precedence_is_environment_then_config_then_default() {
        let from_config = resolve_store_limits(Some(100), Some(200), |_| Ok(None)).unwrap();
        assert_eq!(from_config.buffer_pool_bytes, 100);
        assert_eq!(from_config.max_db_bytes, 200);

        let from_environment = resolve_store_limits(Some(100), Some(200), |name| {
            Ok(match name {
                "NEUROSTRATA_BUFFER_POOL_BYTES" => Some(7),
                "NEUROSTRATA_MAX_DB_BYTES" => Some(9),
                _ => None,
            })
        })
        .unwrap();
        assert_eq!(
            from_environment.buffer_pool_bytes, 7,
            "the environment must win over the config file"
        );
        assert_eq!(from_environment.max_db_bytes, 9);

        let partial = resolve_store_limits(None, Some(200), |name| {
            Ok(match name {
                "NEUROSTRATA_BUFFER_POOL_BYTES" => Some(7),
                _ => None,
            })
        })
        .unwrap();
        assert_eq!(partial.buffer_pool_bytes, 7);
        assert_eq!(
            partial.max_db_bytes, 200,
            "an unset override must fall through to the config, not to zero"
        );
    }

    // Task 2 (capacity profiles): the profile layer sits between the config
    // file and the built-in defaults, fills only unset slots, and is parsed
    // by the consuming caller -- never inside `store_limits()` itself.

    #[test]
    fn profile_parse_is_case_insensitive_and_rejects_unknown_values() {
        assert_eq!(
            CapacityProfile::parse("low").unwrap(),
            CapacityProfile::Low
        );
        assert_eq!(
            CapacityProfile::parse(" LoW ").unwrap(),
            CapacityProfile::Low
        );
        assert_eq!(
            CapacityProfile::parse("STANDARD").unwrap(),
            CapacityProfile::Standard
        );
        assert!(CapacityProfile::parse("huge").is_err());
        assert!(CapacityProfile::parse("").is_err());
        assert_eq!(CapacityProfile::from_value(None).unwrap(), None);
        assert_eq!(CapacityProfile::from_value(Some("")).unwrap(), None);
        assert_eq!(
            CapacityProfile::from_value(Some("low")).unwrap(),
            Some(CapacityProfile::Low)
        );
        assert!(CapacityProfile::from_value(Some("bogus")).is_err());
    }

    #[test]
    fn profile_values_are_the_spec_starting_values() {
        // Starting values, not derived constants (Spec Feature 1).
        let low = CapacityProfile::Low;
        assert_eq!(low.model_name(), "BGESmallENV15Q");
        assert_eq!(low.embedder_dimensions(), 384);
        assert_eq!(low.buffer_pool_bytes(), 64 * 1024 * 1024);
        assert_eq!(low.max_db_bytes(), 2 * 1024 * 1024 * 1024);

        let standard = CapacityProfile::Standard;
        assert_eq!(standard.model_name(), "NomicEmbedTextV15");
        assert_eq!(standard.embedder_dimensions(), 768);
        assert_eq!(
            standard.buffer_pool_bytes(),
            crate::store::StoreLimits::DEFAULT_BUFFER_POOL_BYTES
        );
        assert_eq!(
            standard.max_db_bytes(),
            crate::store::StoreLimits::DEFAULT_MAX_DB_BYTES
        );
    }

    #[test]
    fn standard_profile_is_a_noop() {
        // Spec AC3: profile=standard with nothing explicit resolves to
        // today's defaults exactly.
        let (limits, _sources) = resolve_store_limits_with_profile(
            None,
            None,
            Some(CapacityProfile::Standard),
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(limits, crate::store::StoreLimits::default());
    }

    #[test]
    fn no_profile_no_change_in_store_resolution() {
        // Spec AC4 (store side): no profile resolves exactly as before, and
        // unset slots report the built-in layer as their source.
        let (limits, sources) =
            resolve_store_limits_with_profile(None, None, None, |_| Ok(None)).unwrap();
        assert_eq!(limits, crate::store::StoreLimits::default());
        assert_eq!(sources.pool, StoreLimitSource::BuiltInDefault);
        assert_eq!(sources.max_db, StoreLimitSource::BuiltInDefault);
    }

    #[test]
    fn store_matrix_env_then_config_then_profile_then_builtin() {
        // Spec AC2: per-limit precedence, profile fills only unset slots.
        let low = Some(CapacityProfile::Low);

        // Nothing explicit: the profile supplies both limits.
        let (limits, sources) =
            resolve_store_limits_with_profile(None, None, low, |_| Ok(None)).unwrap();
        assert_eq!(limits.buffer_pool_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_db_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(sources.pool, StoreLimitSource::Profile);
        assert_eq!(sources.max_db, StoreLimitSource::Profile);

        // Config values win over the profile, per limit independently.
        let (limits, sources) =
            resolve_store_limits_with_profile(Some(100), None, low, |_| Ok(None)).unwrap();
        assert_eq!(limits.buffer_pool_bytes, 100);
        assert_eq!(sources.pool, StoreLimitSource::ConfigFile);
        assert_eq!(limits.max_db_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(sources.max_db, StoreLimitSource::Profile);

        // Environment wins over config and profile, per limit independently.
        let (limits, sources) =
            resolve_store_limits_with_profile(Some(100), Some(200), low, |name| {
                Ok(match name {
                    "NEUROSTRATA_BUFFER_POOL_BYTES" => Some(7),
                    _ => None,
                })
            })
            .unwrap();
        assert_eq!(limits.buffer_pool_bytes, 7);
        assert_eq!(sources.pool, StoreLimitSource::Environment);
        assert_eq!(limits.max_db_bytes, 200);
        assert_eq!(sources.max_db, StoreLimitSource::ConfigFile);
    }

    #[test]
    fn invalid_store_limit_environment_values_are_rejected() {
        for raw in ["0", "", "abc", "-1", "18446744073709551616", "4294967295"] {
            let outcome = parse_limit("NEUROSTRATA_BUFFER_POOL_BYTES", Some(raw));
            assert!(outcome.is_err(), "accepted invalid value {raw:?}");
        }
        assert_eq!(parse_limit("X", Some("4096")).unwrap(), Some(4096));
    }

    #[test]
    fn zero_and_engine_sentinel_config_limits_are_rejected() {
        for value in [0, u32::MAX as u64] {
            assert!(resolve_store_limits(Some(value), None, |_| Ok(None)).is_err());
            assert!(resolve_store_limits(None, Some(value), |_| Ok(None)).is_err());
        }
        assert!(resolve_store_limits(Some(20), Some(10), |_| Ok(None)).is_err());
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
        // Any nonempty source ID is valid; use a path with the wrong marker
        // name to exercise a genuinely invalid bind.
        assert!(config.bind_project_context("bad", &root).is_err());
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
            store: StoreConfig::default(),
            allowed_external_dirs: Vec::new(),
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
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
}
