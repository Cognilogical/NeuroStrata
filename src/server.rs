use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use std::sync::Arc;
use crate::traits::{Embedder, VectorStore, MemoryPayload};

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

pub async fn start_mcp_server(emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> io::Result<()> {
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
                    writer.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await?;
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
                    writer.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                }
                "tools/call" => {
                    let mut result_text = "Tool execution failed".to_string();
                    if let Some(params) = &request.params {
                        if let Some(name) = params.get("name").and_then(|n| n.as_str()) {
                            let arguments = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                            
                            match name {
                                "neurostrata_list_namespaces" => {
                                    if let Ok(namespaces) = store.list_namespaces().await {
                                        result_text = format!("Existing namespaces: {:?}", namespaces);
                                    } else {
                                        result_text = "Failed to list namespaces.".to_string();
                                    }
                                }
                                "neurostrata_add_memory" => {
                                    if let Some(content) = arguments.get("content").and_then(|c| c.as_str()) {
                                        let namespace = arguments.get("namespace").and_then(|n| n.as_str()).unwrap_or("global");
                                        let memory_type = arguments.get("memory_type").and_then(|m| m.as_str()).unwrap_or("context");
                                        let create_new_namespace = arguments.get("create_new_namespace").and_then(|v| v.as_bool()).unwrap_or(false);
                                        let user_id = arguments.get("user_id").and_then(|u| u.as_str()).unwrap_or("unknown");
                                        let agent_name = arguments.get("agent_name").and_then(|a| a.as_str()).map(|s| s.to_string());
                                        let location = arguments.get("location").and_then(|l| l.as_str()).unwrap_or("").to_string();
                                        let location_lines = arguments.get("location_lines").and_then(|l| l.as_str()).unwrap_or("").to_string();
                                        let mut metadata = arguments.get("metadata").cloned().unwrap_or(serde_json::json!({}));
                                        
                                        // Merge cognitive fields into metadata JSON blob
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
                                                        let new_id = uuid::Uuid::new_v4().to_string();
                                                        if let Ok(_) = store.upsert(namespace, &new_id, vec, payload).await {
                                                            result_text = format!("Successfully added memory for namespace: {}", namespace);
                                                        } else {
                                                            result_text = "Failed to store memory in database.".to_string();
                                                        }
                                                    } else {
                                                        result_text = "Failed to generate embedding.".to_string();
                                                    }
                                                } else {
                                                    result_text = "Failed to initialize table.".to_string();
                                                }
                                            }
                                        } else {
                                            result_text = "Failed to verify existing namespaces.".to_string();
                                        }
                                    } else {
                                        result_text = "Missing 'content' parameter.".to_string();
                                    }
                                }
                                "neurostrata_get_snapshot" => {
                                    if let Some(namespace) = arguments.get("namespace").and_then(|n| n.as_str()) {
                                        if let Ok(mut all_memories) = store.list(namespace, None).await {
                                            // Filter temporal (active memories only)
                                            all_memories.retain(|r| {
                                                r.payload.metadata.get("valid_to").is_none() || r.payload.metadata["valid_to"].is_null()
                                            });
                                            // Sort by access_count (neural gain) descending
                                            all_memories.sort_by(|a, b| {
                                                let a_count = a.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
                                                let b_count = b.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
                                                b_count.cmp(&a_count) // b compared to a = descending
                                            });
                                            all_memories.truncate(5); // Return top 5
                                            
                                            if all_memories.is_empty() {
                                                result_text = format!("No active memories found for namespace: {}", namespace);
                                            } else {
                                                result_text = serde_json::to_string_pretty(&all_memories).unwrap();
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
                                        arguments.get("target_namespace").and_then(|v| v.as_str())
                                    ) {
                                        if let Ok(Some((vec, mut payload))) = store.get(src, id).await {
                                            if let Ok(_) = store.init(tgt).await {
                                                // Prepend note or just move directly
                                                if let Ok(_) = store.upsert(tgt, id, vec, payload).await {
                                                    if let Ok(_) = store.delete(src, id).await {
                                                        result_text = format!("Successfully moved memory {} from {} to {}", id, src, tgt);
                                                    } else {
                                                        result_text = "Memory copied to target but failed to delete from source.".to_string();
                                                    }
                                                } else {
                                                    result_text = "Failed to insert memory into target namespace.".to_string();
                                                }
                                            } else {
                                                result_text = "Failed to initialize target namespace.".to_string();
                                            }
                                        } else {
                                            result_text = "Memory not found in source namespace.".to_string();
                                        }
                                    } else {
                                        result_text = "Missing required parameters: id, source_namespace, or target_namespace.".to_string();
                                    }
                                }
                                "neurostrata_search_memory" => {
                                    if let Some(query) = arguments.get("query").and_then(|q| q.as_str()) {
                                        let namespace = arguments.get("namespace").and_then(|n| n.as_str()).unwrap_or("global");
                                        if let Ok(_) = store.init(namespace).await {
                                            if let Ok(vec) = emb.embed(&query).await {
                                                if let Ok(results) = store.search(namespace, vec, 5).await {
                                                if results.is_empty() {
                                                    result_text = "No relevant memories found.".to_string();
                                                } else {
                                                    let formatted: Vec<String> = results.into_iter()
                                                        .map(|r| format!("[{}] {}", r.id, r.payload.content))
                                                        .collect();
                                                    result_text = formatted.join("\n\n");
                                                }
                                            } else {
                                                result_text = "Failed to search database.".to_string();
                                            }
                                            } else {
                                                result_text = "Failed to generate embedding for search.".to_string();
                                            }
                                        } else {
                                            result_text = "Failed to initialize namespace table.".to_string();
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
                    writer.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                }
                _ => {}
            }
        }
    }
    
    Ok(())
}
