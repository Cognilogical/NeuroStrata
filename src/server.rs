use crate::traits::{Embedder, MemoryPayload, VectorStore};
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[allow(dead_code)]
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse<T> {
    jsonrpc: String,
    id: Option<Value>,
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

impl<T> JsonRpcResponse<T> {
    fn success(id: Option<Value>, result: T) -> Self {
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
    fn error(id: Option<Value>, error: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub async fn start_mcp_server(
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = stdout;

    while let Some(line) = reader.next_line().await? {
        if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(&line) {
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
                    let resp = JsonRpcResponse::success(id, result);
                    writer
                        .write_all(serde_json::to_string(&resp).unwrap().as_bytes())
                        .await?;
                    writer.write_all(b"\n").await?;
                }
                "notifications/initialized" => {
                    // Do nothing
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
                                        "memory_type": { "type": "string", "description": "Type of memory: 'rule', 'preference', 'bootstrap', 'persona', or 'context'. Defaults to 'context'." },
                                        "create_new_namespace": { "type": "boolean", "description": "Set to true ONLY if you are absolutely certain this is a brand new project namespace that doesn't exist yet." },
                                        "user_id": { "type": "string", "description": "The user making the request." },
                                        "agent_name": { "type": "string", "description": "The name of the agent storing the memory." },
                                        "location": { "type": "string", "description": "The file path, URL, or general location this memory refers to." },
                                        "location_lines": { "type": "string", "description": "The line numbers (e.g. '42-49') this memory refers to." },
                                        "domain": { "type": "string", "description": "Optional category or domain this rule belongs to (e.g., 'frontend', 'database', 'devops', 'api')." },
                                        "related_to": { "type": "array", "items": { "type": "string" }, "description": "Optional list of memory IDs this rule connects to, forming a knowledge graph edge." },
                                        "metadata": { "type": "object", "description": "Optional dictionary with Bi-Directional Anchors" }
                                    },
                                    "required": ["content", "namespace"]

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
                                "name": "neurostrata_generate_canvas",
                                "description": "Generate an Obsidian Canvas (.canvas) file visually mapping the architectural rules and their relationships for a project. Opens directly in NeuroVault.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata')." }
                                    },
                                    "required": ["namespace"]
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
                    let resp = JsonRpcResponse::success(id, result);
                    writer
                        .write_all(serde_json::to_string(&resp).unwrap().as_bytes())
                        .await?;
                    writer.write_all(b"\n").await?;
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
                                            return Ok(CallToolResult {
                                                content: vec![ToolContent::Text {
                                                    text: "ERROR [SECURITY]: Memory rejected due to sensitive information (e.g., API keys, passwords, or tokens). Please redact the secrets from your request and try storing the memory again.".to_string(),
                                                }],
                                                is_error: Some(true),
                                            });
                                        }

                                        let namespace = arguments
                                            .get("namespace")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("global");
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
                                        let location = arguments
                                            .get("location")
                                            .and_then(|l| l.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let location_lines = arguments
                                            .get("location_lines")
                                            .and_then(|l| l.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let mut metadata = arguments
                                            .get("metadata")
                                            .cloned()
                                            .unwrap_or(serde_json::json!({}));

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
                                "neurostrata_generate_canvas" => {
                                    if let Some(namespace) =
                                        arguments.get("namespace").and_then(|n| n.as_str())
                                    {
                                        if let Ok(mut all_memories) =
                                            store.list(namespace, None).await
                                        {
                                            all_memories.retain(|r| {
                                                r.payload.metadata.get("valid_to").is_none()
                                                    || r.payload.metadata["valid_to"].is_null()
                                            });

                                            let mut nodes = Vec::new();
                        let mut edges = Vec::new();

                        let doc_width = 400;
                        let doc_height = 400;
                        let mem_width = 300;
                        let mem_height = 150;
                        let x_gap = 500;
                        let y_gap = 50;

                        let mut domain_map: std::collections::HashMap<String, Vec<&crate::traits::SearchResult>> = std::collections::HashMap::new();
                        let mut orphaned_memories = Vec::new();

                        for m in &all_memories {
                            let mut found_doc = false;
                            
                            // Check refs first
                            if let Some(refs) = m.payload.metadata.get("refs").and_then(|r| r.as_array()) {
                                for r in refs {
                                    if let Some(file) = r.get("file").and_then(|f| f.as_str()) {
                                        domain_map.entry(file.to_string()).or_default().push(m);
                                        found_doc = true;
                                    }
                                }
                            }
                            
                            // Check location if no refs found
                            if !found_doc && !m.payload.location.is_empty() {
                                domain_map.entry(m.payload.location.clone()).or_default().push(m);
                                found_doc = true;
                            }

                            if !found_doc {
                                orphaned_memories.push(m);
                            }
                        }

                        let start_x = -1000;
                        let start_y = -1000;
                        let mut max_structured_y = start_y + doc_height;

                        // Create sets for fast lookup of related_to
                        let mut existing_ids = std::collections::HashSet::new();
                        for m in &all_memories {
                            existing_ids.insert(m.id.clone());
                        }

                        // Build File Nodes & Grouped Memories
                        let mut col_index = 0;
                        for (doc_path, memories) in &domain_map {
                            let x = start_x + (col_index * (doc_width + x_gap));
                            let y = start_y;
                            let doc_node_id = format!("doc-{}", col_index);

                            nodes.push(serde_json::json!({
                                "id": doc_node_id,
                                "type": "file",
                                "file": doc_path,
                                "x": x,
                                "y": y,
                                "width": doc_width,
                                "height": doc_height,
                                "color": "4"
                            }));

                            for (row_index, m) in memories.iter().enumerate() {
                                let mem_node_id = format!("mem-{}", m.id);
                                let mem_x = x + doc_width + 100;
                                let mem_y = y + (row_index as i32 * (mem_height + y_gap));

                                let domain = m.payload.metadata.get("domain").and_then(|d| d.as_str()).unwrap_or("general");
                                let r_type = &m.payload.memory_type;
                                let text = format!("### {}
**Domain:** {}

{}", r_type.to_uppercase(), domain, m.payload.content);

                                nodes.push(serde_json::json!({
                                    "id": mem_node_id,
                                    "type": "text",
                                    "text": text,
                                    "x": mem_x,
                                    "y": mem_y,
                                    "width": mem_width,
                                    "height": mem_height,
                                    "color": "3"
                                }));

                                edges.push(serde_json::json!({
                                    "id": format!("edge-doc-{}", m.id),
                                    "fromNode": mem_node_id,
                                    "fromSide": "left",
                                    "toNode": doc_node_id,
                                    "toSide": "right",
                                    "color": "5"
                                }));
                                
                                // Related_to edges
                                if let Some(related) = m.payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                                    for target_val in related {
                                        if let Some(target_id) = target_val.as_str() {
                                            if existing_ids.contains(target_id) {
                                                edges.push(serde_json::json!({
                                                    "id": format!("edge-rel-{}-{}", m.id, target_id),
                                                    "fromNode": mem_node_id,
                                                    "fromSide": "right",
                                                    "toNode": format!("mem-{}", target_id),
                                                    "toSide": "left",
                                                    "color": "4"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }

                            let column_max_y = y + (memories.len() as i32 * (mem_height + y_gap));
                            if column_max_y > max_structured_y {
                                max_structured_y = column_max_y;
                            }
                            col_index += 1;
                        }

                        // Orphaned Memories
                        if !orphaned_memories.is_empty() {
                            let orph_start_y = max_structured_y + 400;
                            let cols = 4;
                            let rows = (orphaned_memories.len() as f32 / cols as f32).ceil() as i32;
                            
                            let orph_group_width = (cols * mem_width) + ((cols - 1) * 50) + 100;
                            let orph_group_height = (rows * mem_height) + ((rows - 1) * 50) + 100;

                            nodes.push(serde_json::json!({
                                "id": "group-orphans",
                                "type": "group",
                                "x": start_x - 50,
                                "y": orph_start_y - 50,
                                "width": orph_group_width,
                                "height": orph_group_height,
                                "label": "Orphaned Memories (No document links)",
                                "color": "1"
                            }));

                            for (i, m) in orphaned_memories.iter().enumerate() {
                                let col = (i as i32) % cols;
                                let row = (i as i32) / cols;
                                
                                let domain = m.payload.metadata.get("domain").and_then(|d| d.as_str()).unwrap_or("general");
                                let r_type = &m.payload.memory_type;
                                let text = format!("### {}
**Domain:** {}

{}", r_type.to_uppercase(), domain, m.payload.content);
                                let mem_node_id = format!("mem-{}", m.id);

                                nodes.push(serde_json::json!({
                                    "id": mem_node_id,
                                    "type": "text",
                                    "text": text,
                                    "x": start_x + (col * (mem_width + 50)),
                                    "y": orph_start_y + (row * (mem_height + 50)),
                                    "width": mem_width,
                                    "height": mem_height,
                                    "color": "1"
                                }));
                                
                                // Related_to edges
                                if let Some(related) = m.payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                                    for target_val in related {
                                        if let Some(target_id) = target_val.as_str() {
                                            if existing_ids.contains(target_id) {
                                                edges.push(serde_json::json!({
                                                    "id": format!("edge-rel-{}-{}", m.id, target_id),
                                                    "fromNode": mem_node_id,
                                                    "fromSide": "right",
                                                    "toNode": format!("mem-{}", target_id),
                                                    "toSide": "left",
                                                    "color": "4"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }

                                            let canvas_json = serde_json::json!({
                                                "nodes": nodes,
                                                "edges": edges
                                            });

                                            if let Some(home) = dirs::home_dir() {
                                                let vault_dir = home
                                                    .join("Documents")
                                                    .join("NeuroVault")
                                                    .join(namespace);
                                                if !vault_dir.exists() {
                                                    let _ = std::fs::create_dir_all(&vault_dir);
                                                }
                                                let canvas_path =
                                                    vault_dir.join("CognitiveMap.canvas");
                                                if let Ok(canvas_str) =
                                                    serde_json::to_string_pretty(&canvas_json)
                                                {
                                                    if std::fs::write(&canvas_path, canvas_str)
                                                        .is_ok()
                                                    {
                                                        result_text = format!("Successfully generated Obsidian Canvas. View it at: {}", canvas_path.display());
                                                    } else {
                                                        result_text =
                                                            "Failed to write .canvas file."
                                                                .to_string();
                                                    }
                                                } else {
                                                    result_text =
                                                        "Failed to serialize canvas JSON."
                                                            .to_string();
                                                }
                                            } else {
                                                result_text = "Could not determine home directory."
                                                    .to_string();
                                            }
                                        } else {
                                            result_text = "Failed to list memories or namespace does not exist.".to_string();
                                        }
                                    } else {
                                        result_text = "Missing 'namespace' parameter.".to_string();
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
                                        let namespace = arguments
                                            .get("namespace")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("global");
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
                                                                format!(
                                                                    "[{}] {}",
                                                                    r.id, r.payload.content
                                                                )
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
                    let resp = JsonRpcResponse::success(id, result);
                    writer
                        .write_all(serde_json::to_string(&resp).unwrap().as_bytes())
                        .await?;
                    writer.write_all(b"\n").await?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}
