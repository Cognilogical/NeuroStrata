use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct DeduplicationConfig {
    /// API key for judgment model (TypeSafe Jev or compatible)
    /// Presence of key enables deduplication, absence disables it
    #[serde(default)]
    pub judgment_api_key: Option<String>,
    
    /// Request timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    
    /// Similarity threshold for candidate selection (0.0-1.0)
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f32,
    
    /// Confidence threshold to consider something a duplicate (0.0-1.0)
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    
    /// Maximum number of candidates to check
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
    
    /// Circuit breaker: failures before opening
    #[serde(default = "default_cb_failure_threshold")]
    pub cb_failure_threshold: u32,
    
    /// Circuit breaker: initial backoff in seconds
    #[serde(default = "default_cb_backoff_initial")]
    pub cb_backoff_initial_seconds: u64,
    
    /// Circuit breaker: maximum backoff in seconds
    #[serde(default = "default_cb_backoff_max")]
    pub cb_backoff_max_seconds: u64,
}

fn default_timeout() -> u64 { 5 }
fn default_similarity_threshold() -> f32 { 0.85 }
fn default_confidence_threshold() -> f32 { 0.8 }
fn default_max_candidates() -> usize { 5 }
fn default_cb_failure_threshold() -> u32 { 3 }
fn default_cb_backoff_initial() -> u64 { 30 }
fn default_cb_backoff_max() -> u64 { 3600 }

impl Default for DeduplicationConfig {
    fn default() -> Self {
        Self {
            judgment_api_key: None,
            timeout_seconds: default_timeout(),
            similarity_threshold: default_similarity_threshold(),
            confidence_threshold: default_confidence_threshold(),
            max_candidates: default_max_candidates(),
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

        // If config doesn't exist, create default
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
        
        // If they have a legacy config where db_path points to an existing directory (from LanceDB),
        // we need to append a filename so LadybugDB doesn't crash.
        if config.db_path.is_dir() {
            config.db_path = config.db_path.join("ladybug.db");
        }
        
        Ok(config)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("Could not create config directory {}", parent.display()))?;
        }

        let file_name = path
            .file_name()
            .context("Config path must name a file")?
            .to_string_lossy();
        let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .with_context(|| format!("Could not create temporary config {}", temp_path.display()))?;
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
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
            format!("Project context marker does not exist: {}", marker_path.display())
        })?;
        if !metadata.is_dir() {
            anyhow::bail!("Project context marker is not a directory: {}", marker_path.display());
        }
        if marker_path.file_name().and_then(|name| name.to_str()) != Some(".NeuroStrata") {
            anyhow::bail!("Project context path must end in .NeuroStrata: {}", marker_path.display());
        }

        let canonical_path = fs::canonicalize(marker_path)
            .with_context(|| format!("Could not canonicalize {}", marker_path.display()))?;
        if canonical_path.file_name().and_then(|name| name.to_str()) != Some(".NeuroStrata") {
            anyhow::bail!(
                "Canonical project context path must end in .NeuroStrata: {}",
                canonical_path.display()
            );
        }
        self.project_contexts.insert(source_id.to_string(), canonical_path);
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

    #[test]
    fn bind_project_context_requires_nonempty_id_and_existing_marker() {
        let root = temp_dir("validation");
        let marker = root.join(".NeuroStrata");
        fs::create_dir(&marker).unwrap();
        let regular_file = root.join("file");
        fs::write(&regular_file, "not a directory").unwrap();
        let mut config = Config::load_from(&write_config(&root)).unwrap();

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
        assert_eq!(config.deduplication.judgment_api_key.as_deref(), Some("keep-me"));
        assert_eq!(config.deduplication.similarity_threshold, 0.42);
    }

    #[test]
    fn invalid_bind_or_unbind_leaves_config_bytes_unchanged() {
        let root = temp_dir("unchanged");
        let config_path = write_config(&root);
        let original = fs::read(&config_path).unwrap();
        let marker = root.join(".NeuroStrata");
        fs::create_dir(&marker).unwrap();

        let mut config = Config::load_from(&config_path).unwrap();
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

    fn write_config(root: &Path) -> PathBuf {
        write_config_with(root, r#"{"db_path":"test.db"}"#)
    }

    fn write_config_with(root: &Path, contents: &str) -> PathBuf {
        let path = root.join("config.json");
        fs::write(&path, contents).unwrap();
        path
    }
}
