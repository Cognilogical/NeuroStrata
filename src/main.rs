mod config;
mod embed;
mod parser;
mod server;
mod store;
mod traits;

use config::Config;
use embed::FastEmbedder;
use std::sync::Arc;
use store::KuzuStore;
use traits::{Embedder, VectorStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // CLI overrides
    if args.len() > 1 {
        let command = &args[1];
        let config = Config::from_default_path()?;
        let embedder = Arc::new(FastEmbedder::new()?);
        let vector_store: Arc<dyn VectorStore> = Arc::new(KuzuStore::new(
            config.db_path.to_str().unwrap().to_string(),
            embedder.dimensions(),
        )?);

        match command.as_str() {
            "namespaces" => {
                let namespaces = vector_store.list_namespaces().await?;
                println!("Namespaces:");
                for ns in namespaces {
                    println!("  - {}", ns);
                }
                return Ok(());
            }
            "list" => {
                if args.len() < 3 {
                    eprintln!("Usage: neurostrata-mcp list <namespace>");
                    return Ok(());
                }
                let namespace = &args[2];
                let results = vector_store.list(namespace, None).await?;
                println!("Found {} memories in namespace '{}':\n", results.len(), namespace);
                for res in results {
                    let location_str = if res.payload.location.is_empty() {
                        "N/A".to_string()
                    } else {
                        res.payload.location.clone()
                    };
                    println!("--- ID: {} ---", res.id);
                    println!("Type: {}", res.payload.memory_type);
                    println!("Location: {}", location_str);
                    if !res.payload.location_lines.is_empty() {
                        println!("Lines: {}", res.payload.location_lines);
                    }
                    println!("Metadata: {:?}", res.payload.metadata);
                    println!("Content: {}\n", res.payload.content);
                }
                return Ok(());
            }
            "ingest" => {
                if args.len() < 4 {
                    eprintln!("Usage: neurostrata-mcp ingest <dir> <namespace> [schema_path]");
                    return Ok(());
                }
                let dir_path = std::path::Path::new(&args[2]);
                let namespace = &args[3];
                let schema_str = if let Some(schema_path) = args.get(4) {
                    std::fs::read_to_string(schema_path).unwrap_or_else(|e| {
                        eprintln!("Failed to read schema from {}: {}", schema_path, e);
                        std::process::exit(1);
                    })
                } else {
                    r#"
                    {
                        "languages": {
                            "rust": {
                                "extensions": ["rs"],
                                "queries": {
                                    "functions": "(function_item name: (identifier) @name) @func",
                                    "structs": "(struct_item name: (type_identifier) @name) @struct",
                                    "impls": "(impl_item type: (type_identifier) @name) @impl"
                                }
                            },
                            "javascript": {
                                "extensions": ["js", "jsx", "ts", "tsx"],
                                "queries": {
                                    "functions": "(function_declaration name: (identifier) @name) @func",
                                    "classes": "(class_declaration name: (identifier) @name) @class"
                                }
                            },
                            "python": {
                                "extensions": ["py"],
                                "queries": {
                                    "functions": "(function_definition name: (identifier) @name) @func",
                                    "classes": "(class_definition name: (identifier) @name) @class"
                                }
                            }
                        }
                    }
                    "#.to_string()
                };

                let schema = match crate::parser::schema::ParserSchema::load(&schema_str) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Failed to parse schema: {}", e);
                        return Ok(());
                    }
                };

                println!("Ingesting AST from {:?} into namespace '{}' using default/provided schema", dir_path, namespace);
                crate::parser::ingest::ingest_directory(dir_path, &schema, embedder, vector_store, namespace).await?;
                println!("Ingestion complete.");
                return Ok(());
            }
            "export-graph" => {
                let default_out_path = ".NeuroStrata/graph/graph.json";
                let out_path = args.get(2).map(|s| s.as_str()).unwrap_or(default_out_path);
                println!("Exporting Memory Graph to {}", out_path);
                
                // Ensure output directory exists
                if let Some(parent) = std::path::Path::new(out_path).parent() {
                    std::fs::create_dir_all(parent)?;
                }

                // Query the graph natively through the active store (Kuzu)
                let graph_data = vector_store.export_graph().await?;

                let json = serde_json::to_string_pretty(&graph_data)?;
                std::fs::write(out_path, json)?;
                println!("Graph exported successfully with {} nodes and {} edges.", 
                    graph_data["nodes"].as_array().map_or(0, |a: &Vec<serde_json::Value>| a.len()),
                    graph_data["links"].as_array().map_or(0, |a: &Vec<serde_json::Value>| a.len())
                );
                return Ok(());
            }
            _ => {
                // If it's something else, fall through or print usage? Let's just start the server.
                // Or maybe they passed a random arg. Let's start the server but we can ignore it.
            }
        }
    }

    println!("NeuroStrata MCP Server initializing...");

    // Load configuration
    let config = Config::from_default_path()?;
    println!("Config loaded: {:?}", config);

    // Initialize Embedder
    let embedder = Arc::new(FastEmbedder::new()?);
    println!("Embedder initialized.");

    // Initialize Embedded LanceDB VectorStore
    println!(
        "Initializing Embedded LanceDB Store at {:?}",
        config.db_path
    );
    let vector_store: Arc<dyn VectorStore> = Arc::new(KuzuStore::new(
        config.db_path.to_str().unwrap().to_string(),
        embedder.dimensions(),
    )?);

    vector_store.init("global").await?;
    println!("Vector store tables ensured.");

    // Boot actual MCP server loop
    println!("Starting in MCP JSON-RPC mode");
    server::start_mcp_server(embedder, vector_store).await?;

    Ok(())
}
