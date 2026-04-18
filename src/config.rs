use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub api_key: String,
    pub model_path: PathBuf,
}

impl Config {
    pub fn from_default_path() -> anyhow::Result<Self> {
        let config_path = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Unable to locate config directory"))?
            .join("neurostrata")
            .join("config.json");

        let config_str = fs::read_to_string(&config_path)?;
        let config = serde_json::from_str(&config_str)?;

        Ok(config)
    }
}