mod config;
mod daemon;
mod embed;
mod parser;
mod server;
mod store;
mod traits;

use config::Config;
use embed::FastEmbedder;
use std::sync::Arc;
use crate::traits::SearchResult;
use store::LadybugStore;
use traits::{Embedder, VectorStore};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "neurostrata-mcp")]
#[command(about = "🧠 NeuroStrata — MCP Server & CLI Engine", version = "0.1.1")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start NeuroStrata in daemon-only mode
    Daemon,

    /// List all active namespaces
    Namespaces,

    /// List all memories in a given namespace
    List {
        /// The target namespace
        namespace: String,
    },

    /// Ingest a directory of code symbols into a namespace
    Ingest {
        /// The directory path to ingest
        dir: String,

        /// The target namespace
        namespace: String,

        /// Optional path to the parser schema JSON file
        schema_path: Option<String>,
    },

    /// Export the memory graph to a JSON file
    #[command(name = "export-graph")]
    ExportGraph {
        /// Output path for the JSON graph export
        out_path: Option<String>,
    },

    /// Delete a memory from a namespace by ID
    Delete {
        /// The target namespace
        namespace: String,

        /// The memory ID to delete
        id: String,
    },

    /// Add a new memory to a namespace
    Add {
        /// The target namespace
        namespace: String,

        /// The memory type (e.g. symbol, text)
        memory_type: String,

        /// The memory content string
        content: String,

        /// Optional physical location metadata
        location: Option<String>,
    },

    /// Edit an existing memory
    Edit {
        /// The target namespace
        namespace: String,

        /// The memory ID to edit
        id: String,

        /// The new namespace to move/save to
        new_namespace: String,

        /// The new content
        content: String,

        /// The new location
        location: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Check if the daemon is already running on port 34343
    let daemon_running = reqwest::Client::new()
        .get("http://127.0.0.1:34343/health")
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .is_ok();

    // If no arguments, start standard MCP stdio mode
    if args.len() == 1 {
        if daemon_running {
            eprintln!("Daemon is already running. Starting MCP proxy...");
            server::start_mcp_proxy().await?;
            return Ok(());
        } else {
            eprintln!("NeuroStrata MCP Server initializing...");
            
            // Spawn the daemon as a detached process
            let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("neurostrata-mcp"));
            std::process::Command::new(exe)
                .arg("daemon")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            
            eprintln!("Waiting for daemon to become ready (this may take a moment while models load)...");
            
            // Wait for daemon to become ready
            let client = reqwest::Client::new();
            for _ in 0..300 { // 30 seconds max
                if client.get("http://127.0.0.1:34343/health").send().await.is_ok() {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
            
            server::start_mcp_proxy().await?;
            return Ok(());
        }
    }

    // Unrecognized commands are assumed to be external plugins/runners
    // Check if the first argument matches any known command or help/version flags
    let command_str = args[1].as_str();
    let recognized = matches!(
        command_str,
        "daemon" | "namespaces" | "list" | "ingest" | "export-graph" | "delete" | "add" | "edit" | "-h" | "--help" | "-V" | "--version"
    );

    if !recognized {
        let mut cmd = std::process::Command::new(command_str);
        cmd.args(&args[2..]);
        
        match cmd.status() {
            Ok(status) => {
                std::process::exit(status.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("Failed to execute external command '{}': {}", command_str, e);
                std::process::exit(1);
            }
        }
    }

    // Now parse using clap
    let cli = Cli::parse();

    if let Some(command) = cli.command {
        match command {
            Commands::Daemon => {
                println!("NeuroStrata MCP Server initializing in DAEMON-ONLY mode...");
                let config = Config::from_default_path()?;
                let embedder = Arc::new(FastEmbedder::new()?);
                let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embedder.dimensions(),
                )?);
                vector_store.init("global").await?;
                daemon::start_daemon(embedder, vector_store).await?;
            }
            other => {
                if daemon_running {
                    eprintln!("CRITICAL ERROR: The NeuroStrata daemon is currently running (likely via OpenCode) and holds the database lock.");
                    eprintln!("You cannot run database-modifying CLI commands while the daemon is active.");
                    eprintln!("Please shut down OpenCode, or kill the daemon process to run this command.");
                    std::process::exit(1);
                }
                
                let config = Config::from_default_path()?;
                let embedder = Arc::new(FastEmbedder::new()?);
                let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embedder.dimensions(),
                )?);

                match other {
                    Commands::Namespaces => {
                        let namespaces = vector_store.list_namespaces().await?;
                        println!("Namespaces:");
                        for ns in namespaces {
                            println!("  - {}", ns);
                        }
                    }
                    Commands::List { namespace } => {
                        let results: Vec<SearchResult> = vector_store.list(&namespace, None).await?;
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
                            println!("Content: {}\n", res.payload.content);
                        }
                    }
                    Commands::Ingest { dir, namespace, schema_path } => {
                        let dir_path = std::path::Path::new(&dir);
                        let schema_str = if let Some(path) = schema_path {
                            std::fs::read_to_string(&path).unwrap_or_else(|e| {
                                eprintln!("Failed to read schema from {}: {}", path, e);
                                std::process::exit(1);
                            })
                        } else {
                            include_str!("schema.json").to_string()
                        };

                        if let Ok(schema) = crate::parser::schema::ParserSchema::load(&schema_str) {
                            println!("Ingesting AST from {:?} into namespace '{}'", dir_path, namespace);
                            crate::parser::ingest::ingest_directory(dir_path, &schema, embedder, vector_store, &namespace).await?;
                            println!("Ingestion complete.");
                        }
                    }
                    Commands::ExportGraph { out_path } => {
                        let default_path = ".NeuroStrata/graph/graph.json".to_string();
                        let target_path = out_path.as_ref().unwrap_or(&default_path);
                        println!("Exporting Memory Graph to {}", target_path);
                        if let Some(parent) = std::path::Path::new(target_path).parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        vector_store.init("global").await?;
                        let graph_data = vector_store.export_graph().await?;
                        std::fs::write(target_path, serde_json::to_string_pretty(&graph_data)?)?;
                        println!("Graph exported successfully.");
                    }
                    Commands::Delete { namespace, id } => {
                        vector_store.delete(&namespace, &id).await?;
                        println!("Memory deleted successfully.");
                    }
                    Commands::Add { namespace, memory_type, content, location } => {
                        let vector = embedder.embed(&content).await?;
                        let payload = crate::traits::MemoryPayload {
                            content: content.clone(),
                            memory_type: memory_type.clone(),
                            location: location.unwrap_or_default(),
                            user_id: "system".to_string(),
                            agent_name: Some("NeuroStrata".to_string()),
                            location_lines: "".to_string(),
                            metadata: serde_json::json!({}),
                        };
                        let id = uuid::Uuid::new_v4().to_string();
                        vector_store.upsert(&namespace, &id, vector, payload).await?;
                        println!("Memory added successfully with ID: {}", id);
                    }
                    Commands::Edit { namespace, id, new_namespace, content, location } => {
                        if let Some((_, mut payload)) = vector_store.get(&namespace, &id).await? {
                            vector_store.delete(&namespace, &id).await?;
                            payload.content = content.clone();
                            payload.location = location.clone();
                            let vector = embedder.embed(&content).await?;
                            vector_store.upsert(&new_namespace, &id, vector, payload).await?;
                            println!("Successfully edited memory {}", id);
                        }
                    }
                    Commands::Daemon => unreachable!(),
                }
            }
        }
    }

    Ok(())
}
