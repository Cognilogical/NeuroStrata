//! Shared request handler for `neurostrata_taxonomy` (MCP), `POST /taxonomy` (Daemon),
//! and `neurostrata-mcp taxonomy` (CLI).

use crate::events::{RecursionToken, ThalamicBus, ThalamicPulse};
use crate::rdf::action::ActionSecurityContext;
use crate::rdf::taxonomy::{execute_taxonomy, TaxonomyRequest};
use crate::store::ladybug::LadybugStore;
use crate::traits::{Embedder, VectorStore};
use serde_json::{json, Value};
use std::sync::Arc;

/// Dispatch a `neurostrata_taxonomy` request with default local/MCP `"admin"` principal.
pub async fn handle_taxonomy(
    arguments: &Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    handle_taxonomy_with_auth_and_bus(arguments, "admin", emb, store, None).await
}

/// Dispatch a `neurostrata_taxonomy` request with default `"admin"` principal and optional `ThalamicBus`.
pub async fn handle_taxonomy_with_bus(
    arguments: &Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    bus: Option<Arc<ThalamicBus>>,
) -> String {
    handle_taxonomy_with_auth_and_bus(arguments, "admin", emb, store, bus).await
}

/// Dispatch a `neurostrata_taxonomy` request with an explicit authenticated principal.
pub async fn handle_taxonomy_with_auth(
    arguments: &Value,
    authenticated_principal: &str,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    handle_taxonomy_with_auth_and_bus(arguments, authenticated_principal, emb, store, None).await
}

/// Dispatch a `neurostrata_taxonomy` request with an explicit authenticated principal and optional `ThalamicBus`.
pub async fn handle_taxonomy_with_auth_and_bus(
    arguments: &Value,
    authenticated_principal: &str,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    bus: Option<Arc<ThalamicBus>>,
) -> String {
    let Some(ns_raw) = arguments
        .get("namespace")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    else {
        return json!({"ok": false, "error": "namespace required"}).to_string();
    };

    let namespace = crate::server::resolve_namespace(&store, ns_raw).await;
    let Some(lb_store) =
        crate::traits::VectorStore::as_any(store.as_ref()).downcast_ref::<LadybugStore>()
    else {
        return json!({
            "ok": false,
            "error": "subject-matter taxonomy requires LadybugStore backend"
        })
        .to_string();
    };

    let mut req: TaxonomyRequest = match serde_json::from_value(arguments.clone()) {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "ok": false,
                "error": format!("invalid taxonomy request payload: {e}")
            })
            .to_string();
        }
    };
    req.namespace = namespace.clone();
    if req.operation.trim().is_empty() {
        return json!({
            "ok": false,
            "error": "operation required ('set_thesis', 'upsert_concept', 'tree', 'briefing', 'compare', 'gaps', or 'advise')"
        })
        .to_string();
    }

    let requested_principal = req.principal.clone().or_else(|| {
        arguments
            .get("principal")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });
    let resolved_principal =
        crate::rebac::resolve_principal(authenticated_principal, requested_principal.as_deref());
    let security = ActionSecurityContext {
        authenticated_principal: authenticated_principal.to_string(),
        requested_principal: Some(resolved_principal),
    };

    let hint = req
        .query
        .clone()
        .or_else(|| req.topic.clone())
        .or_else(|| req.slug.clone())
        .or_else(|| req.domain.clone())
        .unwrap_or_else(|| req.operation.clone());

    match execute_taxonomy(lb_store, emb.as_ref(), req, &security).await {
        Ok(val) => {
            if let Some(ref b) = bus {
                let op = val.get("operation").and_then(|v| v.as_str()).unwrap_or("");
                if op == "set_thesis" {
                    if let Some(new_id) = val.get("thesis_id").and_then(|v| v.as_str()) {
                        b.emit(
                            ThalamicPulse::Created {
                                id: new_id.to_string(),
                                namespace: namespace.clone(),
                                kind: "thesis".to_string(),
                            },
                            &RecursionToken::root(),
                        );
                        if let Some(old_id) = val.get("superseded_id").and_then(|v| v.as_str()) {
                            b.emit(
                                ThalamicPulse::Superseded {
                                    old_id: old_id.to_string(),
                                    new_id: new_id.to_string(),
                                    namespace: namespace.clone(),
                                },
                                &RecursionToken::root(),
                            );
                        }
                    }
                } else if op == "upsert_concept" {
                    if let Some(new_id) = val.get("concept_id").and_then(|v| v.as_str()) {
                        b.emit(
                            ThalamicPulse::Created {
                                id: new_id.to_string(),
                                namespace: namespace.clone(),
                                kind: "concept".to_string(),
                            },
                            &RecursionToken::root(),
                        );
                        if let Some(old_id) = val.get("superseded_id").and_then(|v| v.as_str()) {
                            b.emit(
                                ThalamicPulse::Superseded {
                                    old_id: old_id.to_string(),
                                    new_id: new_id.to_string(),
                                    namespace: namespace.clone(),
                                },
                                &RecursionToken::root(),
                            );
                        }
                    }
                }
            }
            crate::task::record_graph_query(&store, &namespace, "neurostrata_taxonomy", &hint)
                .await;
            serde_json::to_string_pretty(&val)
                .unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()}).to_string())
        }
        Err(err) => json!({
            "ok": false,
            "error": err,
        })
        .to_string(),
    }
}
