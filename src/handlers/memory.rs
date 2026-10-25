//! Memory handlers extracted from server.rs (R28 structural split).
//!
//! These handlers are reachable from `tools/call` via the MCP tool surface,
//! the daemon's HTTP routes, and the CLI. Living here keeps one implementation
//! so the surfaces cannot drift from each other.

use crate::traits::VectorStore;
use std::sync::Arc;
use serde_json::Value;
use crate::server::format_memory;

pub async fn handle_get_memory(arguments: Value, store: Arc<dyn VectorStore>) -> String {
    let id = match arguments.get("id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => {
            return "Missing 'id' parameter. Ids are returned by neurostrata_search_memory."
                .to_string()
        }
    };
    let namespace = match arguments.get("namespace").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the namespace the memory lives in.".to_string(),
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();
    if namespace.contains('/') || namespace.contains('\\') {
        return "ERROR [NAMESPACE]: The namespace cannot be a file path. It must be the exact project name (e.g., 'NeuroStrata'). Do not use slashes.".to_string();
    }

    match store.get(namespace, id).await {
        Ok(Some((_, payload))) => {
            let store_clone = store.clone();
            let ns_clone = namespace.to_string();
            let id_clone = id.to_string();
            tokio::spawn(async move {
                let _ = store_clone.increment_access_count(&ns_clone, &id_clone).await;
            });
            let _ = crate::recall_metrics::record_promotion(namespace, id);
            format_memory(id, &payload)
        }
        Ok(None) => format!(
            "No memory with id '{}' in namespace '{}'. Ids come from neurostrata_search_memory; check the namespace with neurostrata_list_namespaces.",
            id, namespace
        ),
        Err(e) => format!("Failed to read memory '{}': {}", id, e),
    }
}

pub async fn handle_list_namespaces(store: Arc<dyn VectorStore>) -> String {
    match store.list_namespaces().await {
        Ok(namespaces) => {
            if namespaces.is_empty() {
                "No namespaces found. Ingest a directory to create one.".to_string()
            } else {
                let mut out = String::new();
                for ns in namespaces {
                    out.push_str(&ns);
                    out.push('\n');
                }
                out.trim_end().to_string()
            }
        }
        Err(e) => format!("Failed to list namespaces: {}", e),
    }
}