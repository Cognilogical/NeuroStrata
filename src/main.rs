mod mqtt_worker;
mod traits;
mod config;
mod embed;
mod store;
mod server;
mod mqtt;

use traits::{Embedder, VectorStore};
use config::Config;
use embed::FastEmbedder;
use store::LanceDBStore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("NeuroStrata MCP Server initializing...");

    // Load configuration
    let config = Config::from_default_path()?;
    println!("Config loaded: {:?}", config);

    // Start Embedded MQTT Broker (handles WebSockets and TCP)
    println!("Starting Embedded MQTT Broker on ports 1883 and 8080 (WS)...");
    mqtt::start_broker();
    
    // Give broker a split second to boot before client connects
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Initialize Embedder
    let embedder = Arc::new(FastEmbedder::new()?);
    println!("Embedder initialized.");

    // Initialize Embedded LanceDB VectorStore 
    println!("Initializing Embedded LanceDB Store at {:?}", config.db_path);
    let vector_store: Arc<dyn VectorStore> = Arc::new(LanceDBStore::new(
        config.db_path.to_str().unwrap().to_string(), 
        embedder.dimensions()
    )?);
    
    vector_store.init().await?;
    println!("Vector store tables ensured.");

    // Start internal MQTT worker to process requests from Obsidian
    println!("Starting internal MQTT worker...");
    mqtt_worker::start_worker(embedder.clone(), vector_store.clone()).await;

    // Boot actual MCP server loop
    println!("Starting in MCP JSON-RPC mode");
    server::start_mcp_server(embedder, vector_store).await?;
    
    Ok(())
}
