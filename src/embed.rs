use crate::traits::Embedder;
use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Serialize, Deserialize, Clone)]
pub struct AcceptableEmbedder {
    pub model_name: String,
    pub dimensions: usize,
}

fn get_acceptable_embedders() -> Result<Vec<AcceptableEmbedder>> {
    let config_dir = match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(".config/neurostrata"),
        Err(_) => PathBuf::from(".neurostrata"),
    };

    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).context("Failed to create config directory")?;
    }

    let config_path = config_dir.join("embedders.json");

    if config_path.exists() {
        let content = fs::read_to_string(&config_path).context("Failed to read embedders.json")?;
        if let Ok(models) = serde_json::from_str::<Vec<AcceptableEmbedder>>(&content) {
            if !models.is_empty() {
                return Ok(models);
            }
        }
    }

    // Default list if file doesn't exist or is empty
    let default_models = vec![
        AcceptableEmbedder {
            model_name: "NomicEmbedTextV15".to_string(),
            dimensions: 768,
        },
        AcceptableEmbedder {
            model_name: "BGEBaseENV15".to_string(),
            dimensions: 768,
        },
    ];

    // Write default to file so user can edit it
    if let Ok(json_content) = serde_json::to_string_pretty(&default_models) {
        let _ = fs::write(&config_path, json_content);
    }

    Ok(default_models)
}

fn find_existing_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let home_path = PathBuf::from(home);
    
    // Adopt the Neuro cache pattern: share models across all Neuro* tools
    let primary_neuro_cache = home_path.join(".cache/neuro/models/fastembed");
    
    // Other common fastembed caches we should check to avoid re-downloading
    let cache_dirs = vec![
        primary_neuro_cache.clone(),
        home_path.join(".cache/fastembed"),
        home_path.join(".cache/huggingface/hub"),
    ];

    for dir in cache_dirs {
        if dir.exists() && dir.read_dir().map(|mut i| i.next().is_some()).unwrap_or(false) {
            return dir; // Return the first valid existing cache directory
        }
    }

    primary_neuro_cache
}

pub struct FastEmbedder {
    model: TextEmbedding,
    dimensions: usize,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        let acceptable_models = get_acceptable_embedders()?;
        let target_model = &acceptable_models[0];
        
        let model_enum = EmbeddingModel::from_str(&target_model.model_name)
            .unwrap_or(EmbeddingModel::NomicEmbedTextV15);

        let cache_dir = find_existing_cache_dir();
        
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir).context("Failed to create shared model cache directory")?;
        }

        eprintln!("Initializing FastEmbedder with model: {} using cache: {:?}", target_model.model_name, cache_dir);

        let model = TextEmbedding::try_new(
            InitOptions::new(model_enum)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(true),
        )?;
        
        Ok(Self { 
            model,
            dimensions: target_model.dimensions,
        })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut embeddings = self.model.embed(vec![text], None)?;
        Ok(embeddings.pop().unwrap_or_default())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
