use crate::traits::{Embedder, VectorStore};
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
}

#[derive(Deserialize)]
struct IngestReq {
    dir: String,
    namespace: String,
}

#[derive(Deserialize)]
struct DeleteReq {
    namespace: String,
    id: String,
}

#[derive(Deserialize)]
struct EditReq {
    old_namespace: String,
    id: String,
    new_namespace: String,
    content: String,
    location: String,
}

#[derive(Deserialize)]
struct GraphQuery {
    namespace: Option<String>,
}

pub async fn start_daemon(embedder: Arc<dyn Embedder>, vector_store: Arc<dyn VectorStore>) -> anyhow::Result<()> {
    let state = AppState {
        embedder,
        vector_store,
    };

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/graph", get(handle_get_graph))
        .route("/ingest", post(handle_ingest))
        .route("/delete", post(handle_delete))
        .route("/edit", post(handle_edit))
        .route("/mcp", post(handle_mcp))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:34343").await?;
    println!("NeuroStrata Daemon listening on 127.0.0.1:34343");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_get_graph(
    State(state): State<AppState>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let ns = query.namespace.unwrap_or_else(|| "global".to_string());
    
    // Using export_graph here temporarily or implement native Kuzu querying here
    // For now, let's just use export_graph (which gets everything) and filter by namespace
    // In a real refactor, we would add get_graph_by_namespace to VectorStore.
    // Wait! VectorStore has export_graph() returning the whole graph!
    let data = state.vector_store.export_graph().await.map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // We can just return it all and let the client filter, or we can filter it here.
    // The Tauri backend did: "MATCH (n:Memory) WHERE n.namespace = 'global' OR n.namespace = '{ns}'"
    // Let's filter it.
    let mut filtered_nodes = Vec::new();
    let mut filtered_links = Vec::new();
    let mut allowed_ids = std::collections::HashSet::new();

    if let Some(nodes) = data.get("nodes").and_then(|n| n.as_array()) {
        for node in nodes {
            if let Some(n_ns) = node.get("namespace").and_then(|ns| ns.as_str()) {
                if n_ns == "global" || n_ns == ns {
                    filtered_nodes.push(node.clone());
                    if let Some(id) = node.get("id").and_then(|i| i.as_str()) {
                        allowed_ids.insert(id.to_string());
                    }
                }
            }
        }
    }

    if let Some(links) = data.get("edges").and_then(|l| l.as_array()) {
        for link in links {
            let source = link.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let target = link.get("target").and_then(|s| s.as_str()).unwrap_or("");
            if allowed_ids.contains(source) && allowed_ids.contains(target) {
                filtered_links.push(link.clone());
            }
        }
    }

    Ok(Json(serde_json::json!({
        "nodes": filtered_nodes,
        "links": filtered_links
    })))
}

async fn handle_ingest(
    State(state): State<AppState>,
    Json(req): Json<IngestReq>,
) -> Result<&'static str, axum::http::StatusCode> {
    // Provide default schema if none passed, but we should use ParserSchema
    let schema_str = r#"
    {
        "languages": {
            "rust": {
                "extensions": ["rs"],
                "queries": {
                    "functions": "(function_item name: (identifier) @name) @func"
                }
            }
        }
    }
    "#;
    let schema = crate::parser::schema::ParserSchema::load(schema_str).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    
    let dir_path = std::path::Path::new(&req.dir);
    crate::parser::ingest::ingest_directory(dir_path, &schema, state.embedder.clone(), state.vector_store.clone(), &req.namespace)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok("OK")
}

async fn handle_delete(
    State(state): State<AppState>,
    Json(req): Json<DeleteReq>,
) -> Result<&'static str, axum::http::StatusCode> {
    state.vector_store.delete(&req.namespace, &req.id)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok("OK")
}

async fn handle_edit(
    State(state): State<AppState>,
    Json(req): Json<EditReq>,
) -> Result<&'static str, axum::http::StatusCode> {
    // Basic edit implementation
    let existing = state.vector_store.get(&req.old_namespace, &req.id)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        
    if let Some((vector, mut payload)) = existing {
        payload.content = req.content;
        payload.location = req.location;
        
        state.vector_store.delete(&req.old_namespace, &req.id).await.ok();
        state.vector_store.upsert(&req.new_namespace, &req.id, vector, payload).await.ok();
    }
    Ok("OK")
}

// Handle a single MCP JSON-RPC line
async fn handle_mcp(
    State(state): State<AppState>,
    Json(request): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    if let Ok(rpc_req) = serde_json::from_value::<crate::server::JsonRpcRequest>(request) {
        let response = crate::server::process_mcp_request(rpc_req, state.embedder.clone(), state.vector_store.clone()).await;
        Json(response)
    } else {
        Json(serde_json::json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "Invalid Request"}}))
    }
}