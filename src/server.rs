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
                                        "namespace": { "type": "string", "description": "The project ID or global namespace." },
                                        "user_id": { "type": "string", "description": "The user making the request." },
                                        "agent_name": { "type": "string", "description": "The name of the agent storing the memory." },
                                        "metadata": { "type": "object", "description": "Optional dictionary with Bi-Directional Anchors" }
                                    },
                                    "required": ["content", "namespace"]
                                    
                                }
                            },
                            {
                                "name": "neurostrata_search_memory",
                                "description": "Search the project's long-term memory for architectural rules.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "query": { "type": "string", "description": "What to search for." },
                                        "namespace": { "type": "string", "description": "The project ID or global namespace." }
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
                                "neurostrata_add_memory" => {
                                    if let Some(content) = arguments.get("content").and_then(|c| c.as_str()) {
                                        let namespace = arguments.get("namespace").and_then(|n| n.as_str()).unwrap_or("global");
                                        let user_id = arguments.get("user_id").and_then(|u| u.as_str()).unwrap_or("unknown");
                                        let agent_name = arguments.get("agent_name").and_then(|a| a.as_str()).map(|s| s.to_string());
                                        let metadata = arguments.get("metadata").cloned().unwrap_or(serde_json::json!({}));
                                        
                                        let payload = MemoryPayload {
                                            content: content.to_string(),
                                            user_id: user_id.to_string(),
                                            agent_name,
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
                                            }
                                        } else {
                                            result_text = "Failed to generate embedding.".to_string();
                                        }
                                    } else {
                                        result_text = "Missing 'content' parameter.".to_string();
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
