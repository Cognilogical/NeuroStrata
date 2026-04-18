use qdrant_client::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use crate::traits::VectorStore;

pub struct EmbeddedQdrantStore {
    client: QdrantClient,
    local_path: PathBuf,
}

impl EmbeddedQdrantStore {
    pub fn new(local_path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let local_path = local_path.into();
        if !local_path.exists() {
            fs::create_dir_all(&local_path)?;
        }
        
        let config = QdrantClientConfig::new(Some(local_path.join("qdrant_db".to_string_lossy().to_string())));
        let client = QdrantClient::new(Some(config))?;
        
}
