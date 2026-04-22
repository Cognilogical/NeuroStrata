mod config;
mod embed;
mod parser;
mod server;
mod store;
mod traits;

use config::Config;
use embed::FastEmbedder;
use std::sync::Arc;
use store::LanceDBStore;
use traits::{Embedder, VectorStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // CLI overrides
    if args.len() > 1 {
        let command = &args[1];
        let config = Config::from_default_path()?;
        let embedder = Arc::new(FastEmbedder::new()?);
        let vector_store: Arc<dyn VectorStore> = Arc::new(LanceDBStore::new(
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

                let mut nodes = Vec::new();
                let mut links = Vec::new();

                // Regex to find standard Markdown links and Obsidian WikiLinks
                let re_md = regex::Regex::new(r"\]\((.*?\.md)\)").unwrap();
                let re_wiki = regex::Regex::new(r"\[\[(.*?)\]\]").unwrap();

                // Build the physical Software Graph (AST + Files)
                let skipped_dirs = [
                    "node_modules", "target", "vendor", ".venv", "venv", "env", ".env",
                    "dist", "build", "out", ".dolt", ".git", ".next", ".nuxt", "__pycache__"
                ];

                for entry in walkdir::WalkDir::new(".").into_iter().filter_map(|e| e.ok()) {
                    let path = entry.path().to_string_lossy().to_string();
                    let name = entry.file_name().to_string_lossy().to_string();
                    
                    // Skip 3rd party libraries and build artifacts
                    let mut should_skip = false;
                    for skip_dir in &skipped_dirs {
                        let skip_pattern = format!("/{}/", skip_dir);
                        let skip_start = format!("{}/", skip_dir);
                        let skip_exact = format!("./{}", skip_dir);
                        if path.contains(&skip_pattern) || path.starts_with(&skip_start) || path == skip_exact {
                            should_skip = true;
                            break;
                        }
                    }
                    if should_skip {
                        continue;
                    }

                    let mut is_file = false;
                    let mut mem_type = "directory";
                    if entry.file_type().is_file() {
                        is_file = true;
                        if path.ends_with(".md") { mem_type = "markdown"; }
                        else if path.ends_with(".rs") || path.ends_with(".ts") || path.ends_with(".tsx") { mem_type = "code_ast"; }
                        else { continue; } // Only index relevant files
                    }
                    
                    let abs_path = std::env::current_dir().unwrap().join(&path).canonicalize().unwrap_or_default().to_string_lossy().to_string();

                    // If markdown, parse links
                    if mem_type == "markdown" {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            for cap in re_md.captures_iter(&content) {
                                let mut target = cap[1].to_string();
                                // Only link if it looks like a local relative file path
                                if !target.starts_with("http") && !target.contains("://") {
                                    // Clean up basic relative path dots
                                    if target.starts_with("./") {
                                        target = target[2..].to_string();
                                    }
                                    links.push(serde_json::json!({
                                        "source": path.clone(),
                                        "target": format!("./{}", target),
                                        "type": "links_to"
                                    }));
                                }
                            }
                            for cap in re_wiki.captures_iter(&content) {
                                let mut target = cap[1].to_string();
                                
                                // Strip off anchor tags (e.g., #Header)
                                if let Some(idx) = target.find('#') {
                                    target = target[..idx].to_string();
                                }
                                
                                // Optional: append .md if wiki link doesn't have it
                                if !target.ends_with(".md") {
                                    target.push_str(".md");
                                }
                                
                                // Simple loose linking strategy for now
                                links.push(serde_json::json!({
                                    "source": path.clone(),
                                    "target": format!("./{}", target),
                                    "type": "links_to"
                                }));
                            }
                        }
                    }

                    nodes.push(serde_json::json!({
                        "id": path.clone(),
                        "name": name,
                        "content": format!("Path: {}", path),
                        "memory_type": mem_type,
                        "namespace": "filesystem",
                        "agent_name": "system",
                        "location": path.clone(),
                        "absolute_path": abs_path,
                        "location_lines": "",
                        "degree": if is_file { 1 } else { 3 },
                        "metadata": {}
                    }));

                    if let Some(parent) = entry.path().parent() {
                        let parent_path = parent.to_string_lossy().to_string();
                        if parent_path != "." && parent_path != "" {
                            links.push(serde_json::json!({
                                "source": path,
                                "target": parent_path,
                                "type": "contains"
                            }));
                        }
                    }
                }

                let namespaces = vector_store.list_namespaces().await?;
                
                for ns in namespaces {
                    let memories = vector_store.list(&ns, None).await?;
                    for res in memories {
                        let payload = res.payload;
                        let mut degree = 0;
                        
                        // Extract related_to links
                        if let Some(related) = payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                            degree += related.len();
                            for link in related {
                                if let Some(target_id) = link.as_str() {
                                    links.push(serde_json::json!({
                                        "source": res.id.clone(),
                                        "target": target_id.to_string(),
                                        "type": "related_to"
                                    }));
                                }
                            }
                        }
                        
                        // Extract locations as links (to link semantic memories to code_ast memories)
                        if let Some(locations) = payload.metadata.get("locations").and_then(|l| l.as_array()) {
                            degree += locations.len();
                            for loc in locations {
                                if let Some(path) = loc.get("path").and_then(|p| p.as_str()) {
                                    // create a synthetic edge to the path string (or we could resolve it later)
                                    links.push(serde_json::json!({
                                        "source": res.id.clone(),
                                        "target": format!("path:{}", path),
                                        "type": "references_file"
                                    }));
                                }
                            }
                        }

                        // Determine a display name (short snippet)
                        let mut name = payload.content.chars().take(60).collect::<String>();
                        if payload.content.len() > 60 {
                            name.push_str("...");
                        }

                        let mut abs_path = "".to_string();
                        if !payload.location.is_empty() {
                            let p = std::path::Path::new(&payload.location);
                            if p.is_absolute() {
                                abs_path = payload.location.clone();
                            } else {
                                if let Ok(cwd) = std::env::current_dir() {
                                    abs_path = cwd.join(p).canonicalize().unwrap_or_default().to_string_lossy().to_string();
                                }
                            }
                        }

                        nodes.push(serde_json::json!({
                            "id": res.id,
                            "name": name,
                            "content": payload.content,
                            "memory_type": payload.memory_type,
                            "namespace": ns,
                            "agent_name": payload.agent_name.unwrap_or_else(|| "unknown".to_string()),
                            "location": payload.location,
                            "absolute_path": abs_path,
                            "location_lines": payload.location_lines,
                            "degree": degree,
                            "metadata": payload.metadata
                        }));
                    }
                }
                
                let graph = serde_json::json!({
                    "nodes": nodes,
                    "links": links
                });
                
                std::fs::write(out_path, serde_json::to_string_pretty(&graph)?)?;
                println!("Export complete.");
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
    let vector_store: Arc<dyn VectorStore> = Arc::new(LanceDBStore::new(
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
