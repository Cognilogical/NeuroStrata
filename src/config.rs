use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

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
}

impl Config {
    pub fn from_default_path() -> Result<Self> {
        let home_dir = dirs::home_dir().context("Could not find home directory")?;
        let config_path = home_dir
            .join(".config")
            .join("neurostrata")
            .join("config.json");

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
            };

            if let Some(parent) = config_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Some(db_parent) = default_config.db_path.parent() {
                fs::create_dir_all(db_parent)?;
            }

            let json = serde_json::to_string_pretty(&default_config)?;
            fs::write(&config_path, json)?;

            return Ok(default_config);
        }

        let content = fs::read_to_string(&config_path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        
        // If they have a legacy config where db_path points to an existing directory (from LanceDB),
        // we need to append a filename so LadybugDB doesn't crash.
        if config.db_path.is_dir() {
            config.db_path = config.db_path.join("ladybug.db");
        }
        
        Ok(config)
    }
}
