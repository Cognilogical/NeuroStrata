//! Shared request handler for `neurostrata_taxonomy` (MCP), `POST /taxonomy` (Daemon),
//! and `neurostrata-mcp taxonomy` (CLI).

use crate::events::{
    MemorySubscriber, RecursionToken, SubscriberContext, ThalamicBus, ThalamicPulse,
};
use crate::rdf::action::ActionSecurityContext;
use crate::rdf::taxonomy::{execute_taxonomy, TaxonomyRequest};
use crate::store::ladybug::LadybugStore;
use crate::traits::{Embedder, VectorStore};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

/// `/taxonomy` hands us caller-controlled JSON. Bound its shape before serde
/// maps it onto `TaxonomyRequest` so a deep or huge request cannot drive
/// unbounded allocation in deserialization (CodeQL rust/unsafe-deserialization).
const MAX_TAXONOMY_ARG_BYTES: usize = 1024 * 1024;
const MAX_TAXONOMY_FIELDS: usize = 128;
const MAX_TAXONOMY_ITEMS: usize = 4096;
const MAX_TAXONOMY_STRING_CHARS: usize = 64 * 1024;
const MAX_TAXONOMY_NESTING_DEPTH: usize = 16;
const MAX_TAXONOMY_KEY_CHARS: usize = 256;

fn bounded_taxonomy_value(value: &Value, depth: usize) -> Result<Value, String> {
    if depth > MAX_TAXONOMY_NESTING_DEPTH {
        return Err("taxonomy arguments are nested too deeply".to_string());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.clone()),
        Value::String(s) if s.chars().count() <= MAX_TAXONOMY_STRING_CHARS => {
            Ok(Value::String(s.clone()))
        }
        Value::String(_) => Err("taxonomy string field exceeds its length limit".to_string()),
        Value::Array(items) if items.len() <= MAX_TAXONOMY_ITEMS => Ok(Value::Array(
            items
                .iter()
                .map(|item| bounded_taxonomy_value(item, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Value::Array(_) => Err("taxonomy array field exceeds its length limit".to_string()),
        Value::Object(map) if map.len() <= MAX_TAXONOMY_FIELDS => {
            let mut bounded = serde_json::Map::with_capacity(map.len());
            for (key, val) in map {
                if key.chars().count() > MAX_TAXONOMY_KEY_CHARS {
                    return Err("taxonomy field name exceeds its length limit".to_string());
                }
                bounded.insert(key.clone(), bounded_taxonomy_value(val, depth + 1)?);
            }
            Ok(Value::Object(bounded))
        }
        Value::Object(_) => Err("taxonomy object has too many fields".to_string()),
    }
}

/// Validate before deserializing. Unknown keys are kept (the handler ignores
/// them with a debug note) so existing callers keep working.
fn validated_taxonomy_arguments(arguments: &Value) -> Result<Value, String> {
    if !arguments.is_object() {
        return Err("taxonomy arguments must be a JSON object".to_string());
    }
    let bounded = bounded_taxonomy_value(arguments, 0)?;
    let encoded = serde_json::to_string(&bounded).map_err(|e| e.to_string())?;
    if encoded.len() > MAX_TAXONOMY_ARG_BYTES {
        return Err("taxonomy arguments exceed the size limit".to_string());
    }
    Ok(bounded)
}

/// Synchronously tee a taxonomy mutation pulse into the project's durable
/// SynapticMesh outbox when running in CLI fallback mode (`bus == None`).
pub async fn tee_taxonomy_pulse_to_project_outbox(
    project_root: &Path,
    store: &dyn VectorStore,
    namespace: &str,
    pulse: &ThalamicPulse,
) {
    let Ok(Some((_mesh_id, mesh_dir, _remote))) =
        crate::synaptic_mesh::storage::MeshStorage::read_pointer(project_root)
    else {
        return;
    };
    let Ok(mesh) = crate::synaptic_mesh::storage::MeshStorage::open(&mesh_dir) else {
        return;
    };
    let outbox_dir = crate::synaptic_mesh::outbox::outbox_dir_for(&mesh_dir);
    let wake = Arc::new(tokio::sync::Notify::new());
    let Ok(sub) =
        crate::events::subscribers::MeshOutbox::new(outbox_dir, wake, mesh.manifest().epoch)
    else {
        return;
    };
    let ctx = SubscriberContext {
        store,
        namespace,
        ts: chrono::Utc::now().timestamp(),
    };
    let _ = sub.handle(pulse, &RecursionToken::root(), &ctx).await;
}

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

    let validated_arguments = match validated_taxonomy_arguments(arguments) {
        Ok(v) => v,
        Err(e) => {
            return json!({
                "ok": false,
                "error": format!("invalid taxonomy request payload: {e}")
            })
            .to_string();
        }
    };
    // CodeQL's `rust/unsafe-deserialization` recognizes a size guard on the
    // exact value passed to the sink. Serialize the already shape-bounded
    // arguments and guard those bytes immediately before deserializing them.
    let encoded = match serde_json::to_string(&validated_arguments) {
        Ok(s) => s,
        Err(e) => {
            return json!({
                "ok": false,
                "error": format!("invalid taxonomy request payload: {e}")
            })
            .to_string();
        }
    };
    if encoded.len() > MAX_TAXONOMY_ARG_BYTES {
        return json!({
            "ok": false,
            "error": "invalid taxonomy request payload: too large"
        })
        .to_string();
    }
    let mut req: TaxonomyRequest = match serde_json::from_str::<TaxonomyRequest>(&encoded) {
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
    let unknown_fields = crate::rdf::taxonomy::unknown_taxonomy_request_fields(arguments);
    if !unknown_fields.is_empty() {
        tracing::debug!(
            operation = %req.operation,
            namespace = %namespace,
            unknown_fields = ?unknown_fields,
            "taxonomy request contains unrecognised fields (ignored)"
        );
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
            let op = val.get("operation").and_then(|v| v.as_str()).unwrap_or("");
            let pulse_opt = if op == "set_thesis" {
                val.get("thesis_id").and_then(|v| v.as_str()).map(|new_id| {
                    if let Some(old_id) = val.get("superseded_id").and_then(|v| v.as_str()) {
                        ThalamicPulse::Superseded {
                            old_id: old_id.to_string(),
                            new_id: new_id.to_string(),
                            namespace: namespace.clone(),
                        }
                    } else {
                        ThalamicPulse::Created {
                            id: new_id.to_string(),
                            namespace: namespace.clone(),
                            kind: "thesis".to_string(),
                        }
                    }
                })
            } else if op == "upsert_concept" {
                val.get("concept_id")
                    .and_then(|v| v.as_str())
                    .map(|new_id| {
                        if let Some(old_id) = val.get("superseded_id").and_then(|v| v.as_str()) {
                            ThalamicPulse::Superseded {
                                old_id: old_id.to_string(),
                                new_id: new_id.to_string(),
                                namespace: namespace.clone(),
                            }
                        } else {
                            ThalamicPulse::Created {
                                id: new_id.to_string(),
                                namespace: namespace.clone(),
                                kind: "concept".to_string(),
                            }
                        }
                    })
            } else {
                None
            };
            if let Some(pulse) = pulse_opt {
                if let Some(ref b) = bus {
                    // Class M (Wave 3): a taxonomy mutation's mesh sync
                    // record goes through `emit_checked`, matching the other
                    // memory-write paths. A failed durable append fails the
                    // request instead of reporting a success the mesh never
                    // hears about.
                    if let Err(err) = b.emit_checked(pulse, &RecursionToken::root()).await {
                        return json!({
                            "ok": false,
                            "error": format!(
                                "taxonomy mutation recorded, but its durable event record failed: {err}"
                            )
                        })
                        .to_string();
                    }
                } else {
                    let project_root = crate::daemon::resolve_project_root();
                    tee_taxonomy_pulse_to_project_outbox(
                        &project_root,
                        store.as_ref(),
                        &namespace,
                        &pulse,
                    )
                    .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_arguments_are_bounded_before_deserialization() {
        let mut deep = json!(1);
        for _ in 0..(MAX_TAXONOMY_NESTING_DEPTH + 1) {
            deep = json!([deep]);
        }
        assert!(validated_taxonomy_arguments(&json!({
            "operation": "briefing",
            "query": deep,
        }))
        .is_err());

        assert!(validated_taxonomy_arguments(&json!({
            "operation": "briefing",
            "query": "x".repeat(MAX_TAXONOMY_STRING_CHARS + 1),
        }))
        .is_err());

        let too_many: Vec<String> = (0..MAX_TAXONOMY_ITEMS + 1).map(|i| i.to_string()).collect();
        assert!(validated_taxonomy_arguments(&json!({
            "operation": "gaps",
            "slugs": too_many,
        }))
        .is_err());

        assert!(validated_taxonomy_arguments(&json!("not an object")).is_err());
    }

    #[test]
    fn valid_taxonomy_arguments_survive_the_gate() {
        let payload = json!({
            "operation": "upsert_concept",
            "namespace": "example",
            "slug": "concept-a",
            "definition": "A concept",
            "aliases": ["alias"],
            "unrecognised": "ignored as before",
        });
        let bounded = validated_taxonomy_arguments(&payload).unwrap();
        let req: TaxonomyRequest = serde_json::from_value(bounded).unwrap();
        assert_eq!(req.operation, "upsert_concept");
        assert_eq!(req.aliases, Some(vec!["alias".to_string()]));
    }
}
