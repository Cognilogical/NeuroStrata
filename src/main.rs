mod traits;
mod config;
mod embed;
mod store;

use traits::{Embedder, VectorStore};
use config::Config;
use embed::FastEmbedder;
use store::{LanceDBStore, RemoteQdrantStore};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("NeuroStrata MCP Server initializing...");

    // Load configuration
    let config = Config::from_default_path()?;
    println!("Config loaded: {:?}", config);

    // Initialize Embedder
    let embedder = Arc::new(FastEmbedder::new()?);
    println!("Embedder initialized.");

    // Initialize VectorStore based on configuration backend
    let vector_store: Arc<dyn VectorStore> = match config.backend.as_str() {
        "qdrant" => {
            println!("Initializing Remote Qdrant Store at {}", config.qdrant_url);
            Arc::new(RemoteQdrantStore::new(&config.qdrant_url, embedder.dimensions())?)
        },
        "lancedb" | _ => {
            println!("Initializing Embedded LanceDB Store at {:?}", config.model_path);
            Arc::new(LanceDBStore::new(config.model_path, embedder.dimensions())?)
        }
    };
    
    vector_store.init().await?;
    println!("Vector store tables ensured.");

    Ok(())
}
