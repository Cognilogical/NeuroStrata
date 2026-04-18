use serde::Deserialize;
use std::path::PathBuf;
use anyhow::Result;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub api_key: String,
    pub model_path: PathBuf,
    
    #[serde(default = "default_backend")]
    pub backend: String,
    
    #[serde(default = "default_qdrant_url")]
    pub qdrant_url: String,
}

fn default_backend() -> String {
    "lancedb".to_string()
}

fn default_qdrant_url() -> String {
    "http://localhost:6334".to_string()
}

impl Config {
    pub fn from_default_path() -> Result<Self> {
        // Return dummy config for now, will implement dirs::config_dir
        Ok(Self {
            api_key: "default".to_string(),
            model_path: PathBuf::from("~/.local/share/neurostrata/db"),
            backend: default_backend(),
            qdrant_url: default_qdrant_url(),
        })
    }
}
