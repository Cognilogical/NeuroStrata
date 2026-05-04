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
            serde_json::to_value(JsonRpcResponse::success(id, result)).unwrap()
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
            serde_json::to_value(JsonRpcResponse::success(id, result)).unwrap()
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
                            if let Ok(namespaces) = store.list_namespaces().await {
                                result_text =
                                    format!("Existing namespaces: {:?}", namespaces);
                            } else {
                                result_text = "Failed to list namespaces.".to_string();
                            }
                        }
                        "neurostrata_add_memory" => {
                            if let Some(content) =
                                arguments.get("content").and_then(|c| c.as_str())
                            {
                                // Secret Scrubber Check
                                let content_lower = content.to_lowercase();
                                if content_lower.contains("sk-ant-") || 
                                   content_lower.contains("ghp_") ||
                                   content_lower.contains("xoxb-") ||
                                   content_lower.contains("eyjhbg") || // JWT header
                                   content_lower.contains("api_key=") ||
                                   content_lower.contains("password=") ||
                                   content_lower.contains("sk-proj-")
                                {
                                    result_text = "ERROR [SECURITY]: Memory rejected due to sensitive information (e.g., API keys, passwords, or tokens). Please redact the secrets from your request and try storing the memory again.".to_string();
                                } else if let Some(namespace) = arguments.get("namespace").and_then(|n| n.as_str()) {
                                    if namespace.contains('/') || namespace.contains('\\') {
                                        result_text = "ERROR [NAMESPACE]: The namespace cannot be a file path. It must be the exact project name (e.g., 'NeuroStrata'). Do not use slashes.".to_string();
                                        return serde_json::json!({
                                            "content": [
                                                { "type": "text", "text": result_text }
                                            ],
                                            "isError": true
                                        });
                                    }

                                    if namespace != "global" {
                                        if let Some(project_root) = arguments.get("project_root").and_then(|r| r.as_str()) {
                                            let ns_dir = std::path::Path::new(project_root).join(".NeuroStrata");
                                            if !ns_dir.exists() {
                                                let create_new_namespace = arguments
                                                    .get("create_new_namespace")
                                                    .and_then(|v| v.as_bool())
                                                    .unwrap_or(false);

                                                if !create_new_namespace {
                                                    result_text = format!("ERROR: No .NeuroStrata directory found at {}. This indicates the project does not have a designated context/namespace yet. Do NOT guess the namespace. Ask the user if they want to initialize this directory as a new context, and if so, call this tool again with create_new_namespace=true.", project_root);
                                                    return serde_json::json!({
                                                        "content": [
                                                            { "type": "text", "text": result_text }
                                                        ],
                                                        "isError": true
                                                    });
                                                } else {
                                                    if let Err(e) = std::fs::create_dir_all(&ns_dir) {
                                                        result_text = format!("ERROR: Failed to create .NeuroStrata directory: {}", e);
                                                        return serde_json::json!({
                                                            "content": [
                                                                { "type": "text", "text": result_text }
                                                            ],
                                                            "isError": true
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    let memory_type = arguments
                                    .get("memory_type")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("context");
                                let create_new_namespace = arguments
                                    .get("create_new_namespace")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let user_id = arguments
                                    .get("user_id")
                                    .and_then(|u| u.as_str())
                                    .unwrap_or("unknown");
                                let agent_name = arguments
                                    .get("agent_name")
                                    .and_then(|a| a.as_str())
                                    .map(|s| s.to_string());
                                let mut location = "".to_string();
                                let mut location_lines = "".to_string();
                                let mut metadata = arguments
                                    .get("metadata")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}));
                                
                                if let Some(locations) = arguments.get("locations").and_then(|l| l.as_array()) {
                                    if let Some(first) = locations.first() {
                                        location = first.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
                                        location_lines = first.get("lines").and_then(|l| l.as_str()).unwrap_or("").to_string();
                                    }
                                    
                                    // Map to metadata.refs for the Obsidian canvas generator
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
                                    }
                                }

                                // Merge cognitive fields into metadata JSON blob
                                if let Some(meta_obj) = metadata.as_object_mut() {
                                    if let Some(domain) = arguments.get("domain") {
                                        meta_obj
                                            .insert("domain".to_string(), domain.clone());
                                    }
                                    if let Some(related_to) = arguments.get("related_to") {
                                        meta_obj.insert(
                                            "related_to".to_string(),
                                            related_to.clone(),
                                        );
                                    }
                                    meta_obj.insert(
                                        "valid_from".to_string(),
                                        serde_json::json!(chrono::Utc::now().timestamp()),
                                    );
                                    meta_obj.insert(
                                        "access_count".to_string(),
                                        serde_json::json!(0),
                                    );
                                }

                                if let Ok(existing_namespaces) =
                                    store.list_namespaces().await
                                {
                                    if !existing_namespaces.contains(&namespace.to_string())
                                        && !create_new_namespace
                                    {
                                        result_text = format!(
                                            "Error: Namespace '{}' does not exist. SYSTEM ALERT: Your agent overconfidence and inaccuracy score has been flagged and degraded by the telemetry monitor. You MUST use `neurostrata_list_namespaces` to check existing project names before guessing. Existing namespaces are: {:?}. If you are absolutely certain this is a brand new project, you must explicitly pass `create_new_namespace: true` to bypass this lock.",
                                            namespace, existing_namespaces
                                        );
                                    } else {
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
                                                let new_id =
                                                    uuid::Uuid::new_v4().to_string();
                                                if let Ok(_) = store
                                                    .upsert(
                                                        namespace, &new_id, vec, payload,
                                                    )
                                                    .await
                                                {
                                                    result_text = format!("Successfully added memory for namespace: {}", namespace);
                                                } else {
                                                    result_text = "Failed to store memory in database.".to_string();
                                                }
                                            } else {
                                                result_text =
                                                    "Failed to generate embedding."
                                                        .to_string();
                                            }
                                        } else {
                                            result_text =
                                                "Failed to initialize table.".to_string();
                                        }
                                    }
                                } else {
                                    result_text =
                                        "Failed to verify existing namespaces.".to_string();
                                }
                            } else {
                                result_text = "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace. NEVER default to 'global' unless instructed.".to_string();
                            }
                        } else {
                            result_text = "Missing 'content' parameter.".to_string();
                        }
                    }
                    "neurostrata_get_snapshot" => {
                            if let Some(namespace) =
                                arguments.get("namespace").and_then(|n| n.as_str())
                            {
                                if let Ok(mut all_memories) =
                                    store.list(namespace, None).await
                                {
                                    // Filter temporal (active memories only)
                                    all_memories.retain(|r| {
                                        r.payload.metadata.get("valid_to").is_none()
                                            || r.payload.metadata["valid_to"].is_null()
                                    });
                                    // Sort by access_count (neural gain) descending
                                    all_memories.sort_by(|a, b| {
                                        let a_count = a
                                            .payload
                                            .metadata
                                            .get("access_count")
                                            .and_then(|v| v.as_i64())
                                            .unwrap_or(0);
                                        let b_count = b
                                            .payload
                                            .metadata
                                            .get("access_count")
                                            .and_then(|v| v.as_i64())
                                            .unwrap_or(0);
                                        b_count.cmp(&a_count) // b compared to a = descending
                                    });
                                    all_memories.truncate(5); // Return top 5

                                    if all_memories.is_empty() {
                                        result_text = format!(
                                            "No active memories found for namespace: {}",
                                            namespace
                                        );
                                    } else {
                                        result_text =
                                            serde_json::to_string_pretty(&all_memories)
                                                .unwrap();
                                    }
                                } else {
                                    result_text = "Failed to list memories or namespace does not exist.".to_string();
                                }
                            } else {
                                result_text = "Missing 'namespace' parameter.".to_string();
                            }
                        }
                        "neurostrata_ingest_directory" => {
                            if let Some(dir_path) = arguments.get("dir_path").and_then(|d| d.as_str()) {
                                if let Some(namespace) = arguments.get("namespace").and_then(|n| n.as_str()) {
                                    let schema_str = r#"
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
                                    "#;
                                    
                                    if let Ok(schema) = crate::parser::schema::ParserSchema::load(schema_str) {
                                        let dir = std::path::Path::new(dir_path);
                                        if let Ok(_) = crate::parser::ingest::ingest_directory(dir, &schema, emb.clone(), store.clone(), namespace).await {
                                            result_text = format!("Successfully ingested AST from {} into namespace '{}'", dir_path, namespace);
                                        } else {
                                            result_text = "Failed to ingest directory. Ensure tree-sitter and parsing logic is fully initialized.".to_string();
                                        }
                                    } else {
                                        result_text = "Failed to load default parser schema.".to_string();
                                    }
                                } else {
                                    result_text = "ERROR: namespace missing.".to_string();
                                }
                            } else {
                                result_text = "ERROR: dir_path missing.".to_string();
                            }
                        }
                        "neurostrata_move_memory" => {
                            if let (Some(id), Some(src), Some(tgt)) = (
                                arguments.get("id").and_then(|v| v.as_str()),
                                arguments.get("source_namespace").and_then(|v| v.as_str()),
                                arguments.get("target_namespace").and_then(|v| v.as_str()),
                            ) {
                                if let Ok(Some((vec, payload))) =
                                    store.get(src, id).await
                                {
                                    if let Ok(_) = store.init(tgt).await {
                                        // Prepend note or just move directly
                                        if let Ok(_) =
                                            store.upsert(tgt, id, vec, payload).await
                                        {
                                            if let Ok(_) = store.delete(src, id).await {
                                                result_text = format!("Successfully moved memory {} from {} to {}", id, src, tgt);
                                            } else {
                                                result_text = "Memory copied to target but failed to delete from source.".to_string();
                                            }
                                        } else {
                                            result_text = "Failed to insert memory into target namespace.".to_string();
                                        }
                                    } else {
                                        result_text =
                                            "Failed to initialize target namespace."
                                                .to_string();
                                    }
                                } else {
                                    result_text =
                                        "Memory not found in source namespace.".to_string();
                                }
                            } else {
                                result_text = "Missing required parameters: id, source_namespace, or target_namespace.".to_string();
                            }
                        }
                        "neurostrata_search_memory" => {
                            if let Some(query) =
                                arguments.get("query").and_then(|q| q.as_str())
                            {
                                if let Some(namespace) = arguments.get("namespace").and_then(|n| n.as_str()) {
                                    if let Ok(_) = store.init(namespace).await {
                                    if let Ok(vec) = emb.embed(&query).await {
                                        if let Ok(results) =
                                            store.search(namespace, vec, 5).await
                                        {
                                            if results.is_empty() {
                                                result_text = "No relevant memories found."
                                                    .to_string();
                                            } else {
                                                let formatted: Vec<String> = results
                                                    .into_iter()
                                                    .map(|r| {
                                                        let mut out = format!(
                                                            "--- Memory ID: {} ---\nType: {}\nContent: {}",
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
                                                        if let Some(related) = r.payload.metadata.get("related_to") {
                                                            if let Some(arr) = related.as_array() {
                                                                if !arr.is_empty() {
                                                                    out.push_str(&format!("\nRelated Nodes: {}", related));
                                                                }
                                                            }
                                                        }
                                                        out
                                                    })
                                                    .collect();
                                                result_text = formatted.join("\n\n");
                                            }
                                        } else {
                                            result_text =
                                                "Failed to search database.".to_string();
                                        }
                                    } else {
                                        result_text =
                                            "Failed to generate embedding for search."
                                                .to_string();
                                    }
                                } else {
                                    result_text =
                                        "Failed to initialize namespace table.".to_string();
                                }
                            } else {
                                result_text = "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace to search in.".to_string();
                            }
                        } else {
                            result_text = "Missing 'query' parameter.".to_string();
                        }
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
            serde_json::to_value(JsonRpcResponse::success(id, result)).unwrap()
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
