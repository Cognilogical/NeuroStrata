use crate::traits::{Embedder, MemoryPayload, VectorStore};
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[allow(dead_code)]
    pub params: Option<Value>,
}

#[derive(Serialize)]
pub struct JsonRpcResponse<T> {
    jsonrpc: String,
    id: Option<Value>,
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

impl<T> JsonRpcResponse<T> {
    pub fn success(id: Option<Value>, result: T) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }
}

#[allow(dead_code)]
impl JsonRpcResponse<Value> {
    pub fn error(id: Option<Value>, error: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub async fn process_mcp_request(
    request: JsonRpcRequest,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> Value {
    let id = request.id.clone();
    match request.method.as_str() {
        "initialize" => {
            let result = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "neurostrata-mcp",
                    "version": "1.0.0"
                },
                "capabilities": {
                    "tools": {}
                }
            });
            serde_json::to_value(JsonRpcResponse::success(id.clone(), result)).unwrap_or_else(|e| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Internal serialization error: {}", e)
                    }
                })
            })
        }
        "notifications/initialized" => {
            serde_json::json!({})
        }
        "tools/list" => {
            let result = serde_json::json!({
                "tools": [
                    {
                        "name": "neurostrata_add_memory",
                        "description": "Store an architectural rule, project pattern, or task insight.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "The text of the memory to save." },
                                "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata') or 'global'. Do not use folder paths." },
                                "project_root": { "type": "string", "description": "The absolute path to the project root directory where the agent is currently working." },
                                "memory_type": { "type": "string", "description": "Type of memory: 'rule', 'preference', 'bootstrap', 'persona', or 'context'. Defaults to 'context'." },
                                "create_new_namespace": { "type": "boolean", "description": "Set to true ONLY if you are absolutely certain this is a brand new project namespace that doesn't exist yet." },
                                "user_id": { "type": "string", "description": "The user making the request." },
                                "agent_name": { "type": "string", "description": "The name of the agent storing the memory." },
                                "locations": { 
                                    "type": "array", 
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "path": { "type": "string", "description": "File path (e.g. docs/architecture.md)" },
                                            "lines": { "type": "string", "description": "Line numbers (e.g. 42-49)" },
                                            "symbol": { "type": "string", "description": "Code symbol (e.g. startSync())" }
                                        }
                                    },
                                    "description": "An array of file paths, line numbers, and symbols this memory governs. Memories MUST reference the specific documents they belong to."
                                },
                                "domain": { "type": "string", "description": "Optional category or domain this rule belongs to (e.g., 'frontend', 'database', 'devops', 'api')." },
                                "related_to": { "type": "array", "items": { "type": "string" }, "description": "Optional list of memory IDs this rule connects to, forming a knowledge graph edge." },
                                "metadata": { "type": "object", "description": "Optional dictionary with Bi-Directional Anchors" }
                            },
                            "required": ["content", "namespace", "project_root"]
                        }
                    },
                    {
                        "name": "neurostrata_get_snapshot",
                        "description": "Get a pre-computed cognitive snapshot of the most important active architectural rules for a project. Use this immediately upon starting a new task to ground yourself in the project's core architecture before searching.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata') or 'global'." }
                            },
                            "required": ["namespace"]
                        }
                    },
                    {
                        "name": "neurostrata_ingest_directory",
                        "description": "Batch ingest and parse the Abstract Syntax Tree (AST) of the current project directory to build the Software Graph.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "dir_path": { "type": "string", "description": "Absolute path to the project directory to ingest. Usually the current working directory." },
                                "namespace": { "type": "string", "description": "The project namespace." }
                            },
                            "required": ["dir_path", "namespace"]
                        }
                    },
                    {
                        "name": "neurostrata_list_namespaces",
                        "description": "List all existing project namespaces in the database. Use this to prevent hallucinating namespace names.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    },
                    {
                        "name": "neurostrata_search_memory",
                        "description": "Search the project's long-term memory for architectural rules.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string", "description": "What to search for." },
                                "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata') or 'global'. Do not use folder paths." }
                            },
                            "required": ["query", "namespace"]
                        }
                    }
                ]
            });
            serde_json::to_value(JsonRpcResponse::success(id.clone(), result)).unwrap_or_else(|e| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Internal serialization error: {}", e)
                    }
                })
            })
        }
        "tools/call" => {
            let mut result_text = "Tool execution failed".to_string();
            if let Some(params) = &request.params {
                if let Some(name) = params.get("name").and_then(|n| n.as_str()) {
                    let arguments = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));

                    match name {
                        "neurostrata_list_namespaces" => {
                            result_text = handle_list_namespaces(store.clone()).await;
                        }
                        "neurostrata_add_memory" => {
                            result_text = handle_add_memory(arguments, emb.clone(), store.clone()).await;
                        }
                        "neurostrata_get_snapshot" => {
                            result_text = handle_get_snapshot(arguments, store.clone()).await;
                        }
                        "neurostrata_ingest_directory" => {
                            result_text = handle_ingest_directory(arguments, emb.clone(), store.clone()).await;
                        }
                        "neurostrata_move_memory" => {
                            result_text = handle_move_memory(arguments, store.clone()).await;
                        }
                        "neurostrata_search_memory" => {
                            result_text = handle_search_memory(arguments, emb.clone(), store.clone()).await;
                        }
                        _ => {
                            result_text = format!("Unknown tool: {}", name);
                        }
                    }
                }
            }

            let result = serde_json::json!({
                "content": [
                    { "type": "text", "text": result_text }
                ]
            });
            serde_json::to_value(JsonRpcResponse::success(id.clone(), result)).unwrap_or_else(|e| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Internal serialization error: {}", e)
                    }
                })
            })
        }
        _ => serde_json::json!({})
    }
}

// Optional proxy helper to keep backwards compat with the stdio loop
pub async fn start_mcp_proxy() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = stdout;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();

    while let Some(line) = reader.next_line().await? {
        if let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) {
            match client.post("http://127.0.0.1:34343/mcp").json(&request).send().await {
                Ok(resp) => {
                    if let Ok(text) = resp.text().await {
                        writer.write_all(text.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                        writer.flush().await?;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to proxy MCP request to daemon: {}", e);
                }
            }
        }
    }
    Ok(())
}


async fn handle_list_namespaces(store: Arc<dyn VectorStore>) -> String {
    if let Ok(namespaces) = store.list_namespaces().await {
        format!("Existing namespaces: {:?}", namespaces)
    } else {
        "Failed to list namespaces.".to_string()
    }
}

// A memory is durable only once checkpointed -- WAL replay restores the catalog
// but not row insertions (bead neurostrata-kug) -- and this surface used to
// checkpoint after every write to close that window. It cost more than it
// bought: the engine waits for all transactions to drain before flushing, so
// under load the checkpoint blocked the writer for minutes and failed anyway
// (bead neurostrata-3fi.6.4). The store now marks itself dirty and the daemon's
// background task does the flushing, which is also what runs at shutdown.

async fn handle_add_memory(arguments: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String {
    let content = match arguments.get("content").and_then(|c| c.as_str()) {
        Some(c) => c,
        None => return "Missing 'content' parameter.".to_string(),
    };

    let secret_regex = regex::Regex::new(r"(?i)(sk-ant-|ghp_|xoxb-|eyjhbg|api_key\s*=|password\s*=|sk-proj-)").unwrap();
    if secret_regex.is_match(content) {
        return "ERROR [SECURITY]: Memory rejected due to sensitive information (e.g., API keys, passwords, or tokens). Please redact the secrets from your request and try storing the memory again.".to_string();
    }

    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace. NEVER default to 'global' unless instructed.".to_string(),
    };

    if namespace.contains('/') || namespace.contains('\\') {
        return "ERROR [NAMESPACE]: The namespace cannot be a file path. It must be the exact project name (e.g., 'NeuroStrata'). Do not use slashes.".to_string();
    }

    if namespace != "global" {
        if let Some(project_root) = arguments.get("project_root").and_then(|r| r.as_str()) {
            let ns_dir = std::path::Path::new(project_root).join(".NeuroStrata");
            if !tokio::fs::try_exists(&ns_dir).await.unwrap_or(false) {
                let create_new_namespace = arguments.get("create_new_namespace").and_then(|v| v.as_bool()).unwrap_or(false);
                if !create_new_namespace {
                    return format!("ERROR: No .NeuroStrata directory found at {}. This indicates the project does not have a designated context/namespace yet. Do NOT guess the namespace. Ask the user if they want to initialize this directory as a new context, and if so, call this tool again with create_new_namespace=true.", project_root);
                } else {
                    if let Err(e) = tokio::fs::create_dir_all(&ns_dir).await {
                        return format!("ERROR: Failed to create .NeuroStrata directory: {}", e);
                    }
                }
            }
        }
    }

    let memory_type = arguments.get("memory_type").and_then(|m| m.as_str()).unwrap_or("context");
    let create_new_namespace = arguments.get("create_new_namespace").and_then(|v| v.as_bool()).unwrap_or(false);
    let user_id = arguments.get("user_id").and_then(|u| u.as_str()).unwrap_or("unknown");
    if user_id == "auto-ingestor" {
        // Directory ingestion deletes every row owned by this user_id before it
        // rebuilds, so a memory stored under it would vanish on the next ingest.
        return "ERROR: 'auto-ingestor' is reserved for directory ingestion, and memories stored under it are deleted by the next ingest. Please use a different user_id.".to_string();
    }
    let agent_name = arguments.get("agent_name").and_then(|a| a.as_str()).map(|s| s.to_string());
    let mut location = "".to_string();
    let mut location_lines = "".to_string();
    let mut metadata = arguments.get("metadata").cloned().unwrap_or_else(|| serde_json::json!({}));
    
    if let Some(locations) = arguments.get("locations").and_then(|l| l.as_array()) {
        if let Some(first) = locations.first() {
            location = first.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
            location_lines = first.get("lines").and_then(|l| l.as_str()).unwrap_or("").to_string();
        }
        
        if let Some(obj) = metadata.as_object_mut() {
            let refs: Vec<serde_json::Value> = locations.iter().map(|loc| {
                let mut ref_obj = serde_json::Map::new();
                if let Some(path) = loc.get("path").and_then(|p| p.as_str()) {
                    ref_obj.insert("file".to_string(), serde_json::json!(path));
                }
                if let Some(lines) = loc.get("lines").and_then(|l| l.as_str()) {
                    ref_obj.insert("lines".to_string(), serde_json::json!(lines));
                }
                if let Some(sym) = loc.get("symbol").and_then(|s| s.as_str()) {
                    ref_obj.insert("symbol".to_string(), serde_json::json!(sym));
                }
                serde_json::Value::Object(ref_obj)
            }).collect();
            obj.insert("refs".to_string(), serde_json::Value::Array(refs));

            // A rule that names files governs them. This is the edge that makes an
            // architectural memory reachable from the code it constrains, rather
            // than only from a similar-sounding query.
            let governed: Vec<serde_json::Value> = locations
                .iter()
                .filter_map(|loc| loc.get("path").and_then(|p| p.as_str()))
                .filter(|p| !p.is_empty())
                // Same normalisation the ingester applies to its node ids, so a
                // path written by hand lands on the file node it names.
                .map(crate::parser::ingest::normalize_node_path)
                .fold(Vec::new(), |mut acc: Vec<String>, path| {
                    // Several locations often sit in one file; one edge is enough.
                    if !acc.contains(&path) {
                        acc.push(path);
                    }
                    acc
                })
                .into_iter()
                .map(|p| serde_json::json!(p))
                .collect();
            if !governed.is_empty() {
                obj.insert("governs".to_string(), serde_json::Value::Array(governed));
            }
        }
    }

    if let Some(meta_obj) = metadata.as_object_mut() {
        if let Some(domain) = arguments.get("domain") {
            meta_obj.insert("domain".to_string(), domain.clone());
        }
        if let Some(related_to) = arguments.get("related_to") {
            meta_obj.insert("related_to".to_string(), related_to.clone());
        }
        meta_obj.insert("valid_from".to_string(), serde_json::json!(chrono::Utc::now().timestamp()));
        meta_obj.insert("access_count".to_string(), serde_json::json!(0));
    }

    if let Ok(existing_namespaces) = store.list_namespaces().await {
        if !existing_namespaces.contains(&namespace.to_string()) && !create_new_namespace {
            return format!(
                "Error: Namespace '{}' does not exist. SYSTEM ALERT: Your agent overconfidence and inaccuracy score has been flagged and degraded by the telemetry monitor. You MUST use `neurostrata_list_namespaces` to check existing project names before guessing. Existing namespaces are: {:?}. If you are absolutely certain this is a brand new project, you must explicitly pass `create_new_namespace: true` to bypass this lock.",
                namespace, existing_namespaces
            );
        }
        
        let payload = MemoryPayload {
            content: content.to_string(),
            user_id: user_id.to_string(),
            memory_type: memory_type.to_string(),
            agent_name,
            location,
            location_lines,
            metadata,
        };

        if let Ok(_) = store.init(namespace).await {
            if let Ok(vec) = emb.embed(&content).await {
                let new_id = uuid::Uuid::new_v4().to_string();
                if let Ok(_) = store.upsert(namespace, &new_id, vec, payload).await {
                    return format!("Successfully added memory for namespace: {}", namespace);
                } else {
                    return "Failed to store memory in database.".to_string();
                }
            } else {
                return "Failed to generate embedding.".to_string();
            }
        } else {
            return "Failed to initialize table.".to_string();
        }
    } else {
        return "Failed to verify existing namespaces.".to_string();
    }
}

async fn handle_get_snapshot(arguments: Value, store: Arc<dyn VectorStore>) -> String {
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "Missing 'namespace' parameter.".to_string(),
    };

    if let Ok(mut all_memories) = store.list(namespace, None).await {
        let now = chrono::Utc::now().timestamp();
        all_memories.retain(|r| {
            match r.payload.metadata.get("valid_to") {
                None => true,
                Some(v) => v.is_null() || (v.as_i64().unwrap_or(0) > now),
            }
        });
        all_memories.sort_by(|a, b| {
            let a_count = a.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let b_count = b.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            b_count.cmp(&a_count)
        });
        all_memories.truncate(5);

        if all_memories.is_empty() {
            format!("No active memories found for namespace: {}", namespace)
        } else {
            serde_json::to_string_pretty(&all_memories).unwrap()
        }
    } else {
        "Failed to list memories or namespace does not exist.".to_string()
    }
}

async fn handle_ingest_directory(arguments: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String {
    let dir_path = match arguments.get("dir_path").and_then(|d| d.as_str()) {
        Some(d) => d,
        None => return "ERROR: dir_path missing.".to_string(),
    };
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR: namespace missing.".to_string(),
    };

    let schema_str = include_str!("schema.json");
    
    if let Ok(schema) = crate::parser::schema::ParserSchema::load(schema_str) {
        let dir = std::path::Path::new(dir_path);
        if let Ok(_) = crate::parser::ingest::ingest_directory(dir, &schema, emb.clone(), store.clone(), namespace).await {
            // Once for the whole walk: ingestion upserts thousands of rows, and
            // checkpointing each one would dominate the run.
            format!("Successfully ingested AST from {} into namespace '{}'", dir_path, namespace)
        } else {
            "Failed to ingest directory. Ensure tree-sitter and parsing logic is fully initialized.".to_string()
        }
    } else {
        "Failed to load default parser schema.".to_string()
    }
}

async fn handle_move_memory(arguments: Value, store: Arc<dyn VectorStore>) -> String {
    let id = match arguments.get("id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => return "Missing required parameters: id, source_namespace, or target_namespace.".to_string(),
    };
    let src = match arguments.get("source_namespace").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Missing required parameters: id, source_namespace, or target_namespace.".to_string(),
    };
    let tgt = match arguments.get("target_namespace").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return "Missing required parameters: id, source_namespace, or target_namespace.".to_string(),
    };

    if let Ok(Some((vec, payload))) = store.get(src, id).await {
        if let Ok(_) = store.init(tgt).await {
            if let Ok(_) = store.upsert(tgt, id, vec, payload).await {
                if let Ok(_) = store.delete(src, id).await {
                    format!("Successfully moved memory {} from {} to {}", id, src, tgt)
                } else {
                    "Memory copied to target but failed to delete from source.".to_string()
                }
            } else {
                "Failed to insert memory into target namespace.".to_string()
            }
        } else {
            "Failed to initialize target namespace.".to_string()
        }
    } else {
        "Memory not found in source namespace.".to_string()
    }
}

async fn handle_search_memory(arguments: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String {
    let query = match arguments.get("query").and_then(|q| q.as_str()) {
        Some(q) => q,
        None => return "Missing 'query' parameter.".to_string(),
    };
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace to search in.".to_string(),
    };

    if let Ok(_) = store.init(namespace).await {
        if let Ok(vec) = emb.embed(&query).await {
            if let Ok(results) = store.search(namespace, vec, 5).await {
                if results.is_empty() {
                    "No relevant memories found.".to_string()
                } else {
                    for res in &results {
                        let store_clone = store.clone();
                        let ns_clone = namespace.to_string();
                        let id_clone = res.id.clone();
                        tokio::spawn(async move {
                            let _ = store_clone.increment_access_count(&ns_clone, &id_clone).await;
                        });
                    }

                    let formatted: Vec<String> = results.into_iter().map(|r| {
                        let mut out = format!(
                            "--- Memory ID: {} ---
Type: {}
Content: {}",
                            r.id, r.payload.memory_type, r.payload.content
                        );
                        if !r.payload.location.is_empty() {
                            out.push_str(&format!("\nFile Location: {}", r.payload.location));
                            if !r.payload.location_lines.is_empty() {
                                out.push_str(&format!(" (Lines: {})", r.payload.location_lines));
                            }
                        }
                        if let Some(locations) = r.payload.metadata.get("locations") {
                            if let Some(arr) = locations.as_array() {
                                if !arr.is_empty() {
                                    out.push_str(&format!("\nCode Graph Locations: {}", locations));
                                }
                            }
                        }
                        for (key, label) in [
                            ("related_to", "Related Nodes"),
                            ("contained_by", "Contained By"),
                            ("governs", "Governs"),
                        ] {
                            if let Some(value) = r.payload.metadata.get(key) {
                                if let Some(arr) = value.as_array() {
                                    if !arr.is_empty() {
                                        out.push_str(&format!("\n{}: {}", label, value));
                                    }
                                }
                            }
                        }
                        out
                    }).collect();
                    formatted.join("\n\n")
                }
            } else {
                "Failed to search database.".to_string()
            }
        } else {
            "Failed to generate embedding for search.".to_string()
        }
    } else {
        "Failed to initialize namespace table.".to_string()
    }
}
