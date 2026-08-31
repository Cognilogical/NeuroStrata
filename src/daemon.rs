use crate::traits::{Embedder, VectorStore};
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// How often the daemon flushes to durable storage. Anything written between
/// checkpoints is lost if the process is killed, so this bounds the damage.
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone)]
struct AppState {
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    /// Fires once, when something asks the daemon to stop. Taken by whoever
    /// gets there first so a second /shutdown call is harmless.
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

#[derive(Deserialize)]
struct IngestReq {
    dir: String,
    namespace: String,
}

#[derive(Deserialize)]
struct BackupReq {
    dir: String,
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
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let state = AppState {
        embedder,
        vector_store: vector_store.clone(),
        shutdown: Arc::new(Mutex::new(Some(shutdown_tx))),
    };

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/graph", get(handle_get_graph))
        .route("/ingest", post(handle_ingest))
        .route("/delete", post(handle_delete))
        .route("/edit", post(handle_edit))
        .route("/mcp", post(handle_mcp))
        .route("/backup", post(handle_backup))
        .route("/shutdown", post(handle_shutdown))
        .with_state(state);

    // Bound the loss window for anything that kills us without warning.
    //
    // This is the ONLY place a checkpoint happens while the daemon is serving.
    // It used to run inside each write handler, which looked safer and was far
    // worse: a checkpoint waits for every active transaction to drain, and
    // under a steady stream of queries that window never opens, so the engine
    // blocked for its own timeout -- around two and a half minutes -- with the
    // caller still waiting on the response (bead neurostrata-3fi.6.4). Out here
    // a failure costs a retry instead of a request, and the write is already in
    // the log either way.
    let periodic_store = vector_store.clone();
    tokio::spawn(async move {
        let mut wait = CHECKPOINT_INTERVAL;
        let mut failures: u32 = 0;

        loop {
            tokio::time::sleep(wait).await;

            if !periodic_store.is_dirty() {
                wait = CHECKPOINT_INTERVAL;
                continue;
            }

            match periodic_store.checkpoint().await {
                Ok(()) => {
                    if failures > 0 {
                        eprintln!(
                            "Checkpoint succeeded after {} failed attempts; those writes are on disk now.",
                            failures
                        );
                    }
                    failures = 0;
                    wait = CHECKPOINT_INTERVAL;
                }
                Err(e) => {
                    failures += 1;
                    // Say it once, then only occasionally: a busy database can
                    // refuse the quiet moment for a while, and a warning per
                    // attempt would bury the log without adding anything.
                    if failures == 1 {
                        eprintln!("WARNING: checkpoint failed, so recent writes stay in the log until one succeeds. Retrying: {}", e);
                    } else if failures % 10 == 0 {
                        eprintln!("WARNING: {} checkpoints in a row have failed -- everything written since the last success would be lost to a hard kill: {}", failures, e);
                    }
                    wait = checkpoint_backoff(failures);
                }
            }
        }
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:34343").await?;
    eprintln!("NeuroStrata Daemon listening on 127.0.0.1:34343");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_rx))
        .await?;

    // The whole point of stopping gracefully: get everything on disk before exit.
    match vector_store.checkpoint().await {
        Ok(()) => eprintln!("Checkpoint complete. NeuroStrata Daemon stopped."),
        Err(e) => eprintln!("ERROR: final checkpoint failed, recent writes may be lost: {}", e),
    }
    Ok(())
}

async fn handle_get_graph(
    State(state): State<AppState>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let ns = query.namespace.unwrap_or_else(|| "global".to_string());
    
    // Using export_graph here temporarily or implement native LadybugDB querying here
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

    // export_graph emits "links"; reading "edges" here served an edgeless graph.
    if let Some(links) = data.get("links").and_then(|l| l.as_array()) {
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

/// Backup and restore run here rather than in the CLI so they work against a
/// live daemon: the database is single-writer, and the daemon holds that writer.
async fn handle_backup(
    State(state): State<AppState>,
    Json(req): Json<BackupReq>,
) -> Result<String, (axum::http::StatusCode, String)> {
    state
        .vector_store
        .export_database(&req.dir)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(format!("Backed up to {}", req.dir))
}

/// How long to wait after a checkpoint that could not get its quiet moment.
/// Short at first, because the window can open as soon as one query finishes,
/// and capped so a lull is never missed by much.
fn checkpoint_backoff(failures: u32) -> std::time::Duration {
    let secs = 1u64 << failures.min(5);
    std::time::Duration::from_secs(secs.min(30))
}

async fn handle_delete(
    State(state): State<AppState>,
    Json(req): Json<DeleteReq>,
) -> Result<&'static str, (axum::http::StatusCode, String)> {
    // Answer with what the engine said. A bare 500 cost an afternoon here: a
    // caller could not tell a write conflict from a missing id, and neither
    // could the log (bead neurostrata-3fi.6.5).
    state.vector_store.delete(&req.namespace, &req.id)
        .await
        .map_err(|e| {
            eprintln!("delete of {} in {} failed: {}", req.id, req.namespace, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
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

/// Resolves when the daemon should stop: an explicit POST /shutdown, Ctrl-C, or
/// the OS asking us to go away. On Windows a console close gives roughly five
/// seconds before the process is killed regardless, so the checkpoint that
/// follows has to be quick.
async fn shutdown_signal(rx: oneshot::Receiver<()>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("Received Ctrl-C, shutting down.");
    };

    #[cfg(windows)]
    let os_signal = async {
        let mut close = match tokio::signal::windows::ctrl_close() {
            Ok(s) => s,
            Err(_) => return std::future::pending::<()>().await,
        };
        let mut shutdown = match tokio::signal::windows::ctrl_shutdown() {
            Ok(s) => s,
            Err(_) => return std::future::pending::<()>().await,
        };
        tokio::select! {
            _ = close.recv() => eprintln!("Console is closing, shutting down."),
            _ = shutdown.recv() => eprintln!("System is shutting down."),
        }
    };

    #[cfg(unix)]
    let os_signal = async {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return std::future::pending::<()>().await,
        };
        term.recv().await;
        eprintln!("Received SIGTERM, shutting down.");
    };

    tokio::select! {
        _ = ctrl_c => {}
        _ = os_signal => {}
        _ = rx => eprintln!("Shutdown requested over HTTP."),
    }
}

async fn handle_shutdown(State(state): State<AppState>) -> &'static str {
    // A second caller finds None here; stopping twice is not an error.
    let sender = state.shutdown.lock().ok().and_then(|mut guard| guard.take());
    match sender {
        Some(tx) => {
            let _ = tx.send(());
            "Shutting down"
        }
        None => "Already shutting down",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_retry_comes_quickly() {
        // The quiet moment a checkpoint needs can open as soon as one query
        // finishes, so the first wait is seconds, not the full interval.
        assert!(checkpoint_backoff(1) < std::time::Duration::from_secs(5));
    }

    #[test]
    fn repeated_failures_back_off_but_never_give_up_for_long() {
        let waits: Vec<u64> = (1..=8).map(|n| checkpoint_backoff(n).as_secs()).collect();

        assert!(waits.windows(2).all(|w| w[1] >= w[0]), "{:?} should not shrink", waits);
        assert!(
            waits.iter().all(|w| *w <= 30),
            "a lull must never be missed by more than half a minute: {:?}",
            waits
        );
        assert_eq!(*waits.last().unwrap(), 30, "and it settles at the cap");
    }
}
