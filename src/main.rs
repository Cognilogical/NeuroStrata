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

    // CLI commands
    let command = &args[1];

    if command == "daemon" {
        println!("NeuroStrata MCP Server initializing in DAEMON-ONLY mode...");
        let config = Config::from_default_path()?;
        let embedder = Arc::new(FastEmbedder::new()?);
        let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
            config.db_path.to_str().unwrap().to_string(),
            embedder.dimensions(),
        )?);
        vector_store.init("global").await?;
        daemon::start_daemon(embedder, vector_store).await?;
        return Ok(());
    }

    // Only instantiate the database for known CLI commands
    match command.as_str() {
        "namespaces" | "list" | "ingest" | "export-graph" | "delete" | "add" | "edit" => {
            if daemon_running {
                eprintln!("CRITICAL ERROR: The NeuroStrata daemon is currently running (likely via OpenCode) and holds the database lock.");
                eprintln!("You cannot run database-modifying CLI commands while the daemon is active.");
                eprintln!("Please shut down OpenCode, or kill the daemon process to run this command.");
                std::process::exit(1);
            }
            
            let config = Config::from_default_path()?;
            let embedder = Arc::new(FastEmbedder::new()?);
            let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
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
                }
                "list" => {
                    if args.len() < 3 {
                        eprintln!("Usage: neurostrata-mcp list <namespace>");
                        return Ok(());
                    }
                    let namespace = &args[2];
                    let results: Vec<SearchResult> = vector_store.list(namespace, None).await?;
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
                        r#"{ "languages": {} }"#.to_string()
                    };

                    if let Ok(schema) = crate::parser::schema::ParserSchema::load(&schema_str) {
                        println!("Ingesting AST from {:?} into namespace '{}'", dir_path, namespace);
                        crate::parser::ingest::ingest_directory(dir_path, &schema, embedder, vector_store, namespace).await?;
                        println!("Ingestion complete.");
                    }
                }
                "export-graph" => {
                    let out_path = args.get(2).map(|s| s.as_str()).unwrap_or(".NeuroStrata/graph/graph.json");
                    println!("Exporting Memory Graph to {}", out_path);
                    if let Some(parent) = std::path::Path::new(out_path).parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    vector_store.init("global").await?;
                    let graph_data = vector_store.export_graph().await?;
                    std::fs::write(out_path, serde_json::to_string_pretty(&graph_data)?)?;
                    println!("Graph exported successfully.");
                }
                "delete" => {
                    if args.len() < 4 {
                        eprintln!("Usage: neurostrata-mcp delete <namespace> <id>");
                        return Ok(());
                    }
                    vector_store.delete(&args[2], &args[3]).await?;
                    println!("Memory deleted successfully.");
                }
                "add" => {
                    if args.len() < 5 {
                        eprintln!("Usage: neurostrata-mcp add <namespace> <type> <content> [location]");
                        return Ok(());
                    }
                    let content = &args[4];
                    let vector = embedder.embed(content).await?;
                    let payload = crate::traits::MemoryPayload {
                        content: content.clone(),
                        memory_type: args[3].clone(),
                        location: args.get(5).cloned().unwrap_or_default(),
                        user_id: "system".to_string(),
                        agent_name: Some("NeuroStrata".to_string()),
                        location_lines: "".to_string(),
                        metadata: serde_json::json!({}),
                    };
                    let id = uuid::Uuid::new_v4().to_string();
                    vector_store.upsert(&args[2], &id, vector, payload).await?;
                    println!("Memory added successfully with ID: {}", id);
                }
                "edit" => {
                    if args.len() < 6 {
                        eprintln!("Usage: neurostrata-mcp edit <namespace> <id> <new_namespace> <content> <location>");
                        return Ok(());
                    }
                    if let Some((_, mut payload)) = vector_store.get(&args[2], &args[3]).await? {
                        vector_store.delete(&args[2], &args[3]).await?;
                        payload.content = args[5].clone();
                        payload.location = args[6].clone();
                        let vector = embedder.embed(&args[5]).await?;
                        vector_store.upsert(&args[4], &args[3], vector, payload).await?;
                        println!("Successfully edited memory {}", &args[3]);
                    }
                }
                _ => unreachable!(),
            }
        }
        _ => {
            // Unrecognized commands are assumed to be external plugins/runners
            let mut cmd = std::process::Command::new(command);
            cmd.args(&args[2..]);
            
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let err = cmd.exec();
                eprintln!("Failed to execute external command '{}': {}", command, err);
                std::process::exit(1);
            }
            
            #[cfg(not(unix))]
            {
                match cmd.status() {
                    Ok(status) => {
                        std::process::exit(status.code().unwrap_or(1));
                    }
                    Err(e) => {
                        eprintln!("Failed to execute external command '{}': {}", command, e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    Ok(())
}
