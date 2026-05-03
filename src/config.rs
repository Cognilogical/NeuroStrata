use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Config {
    #[serde(default)]
    pub db_path: PathBuf,
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
