mod traits;

use traits::{Embedder, VectorStore, MemoryPayload, SearchResult};
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

    // Initialize VectorStore
    let vector_store = Arc::new(EmbeddedQdrantStore::new(config.model_path)?);
    println!("Vector store initialized.");

    Ok(())
}
