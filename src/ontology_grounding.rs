use crate::registry;
use crate::store::ladybug::LadybugStore;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct OntologyConfig {
    pub path: String,
    pub mode: String,
}

/// Resolve per-namespace ontology config: registry row metadata first, then env.
pub fn resolve_config(registry_metadata: &Value) -> Option<OntologyConfig> {
    if let Some(cfg) = registry_metadata.get("ontology_config") {
        if let Some(path) = cfg.get("path").and_then(|v| v.as_str()) {
            let mode = cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("annotate").to_string();
            return Some(OntologyConfig { path: path.to_string(), mode });
        }
    }
    let path = std::env::var("ONTOLOGY_FILE_PATH").ok()?;
    let mode = std::env::var("ONTOLOGY_MODE").unwrap_or_else(|_| "annotate".to_string());
    Some(OntologyConfig { path, mode })
}

/// Read and parse a vocabulary file. Returns parsed JSON on success.
pub fn load_vocabulary(path: &str) -> Result<Value, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

/// Extract concept_iri values from a vocabulary for the given domain slugs.
/// Supports several shapes (array of entries, object with domains/entries/concepts,
/// or a top-level slug->iri map).
pub fn concept_iris_from_vocabulary(vocab: &Value, domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // Collect candidate entries as (slug, concept_iri).
    let mut entries: Vec<(String, String)> = Vec::new();

    if let Some(arr) = vocab.as_array() {
        collect_entries_from_array(arr, &mut entries);
    } else if let Some(obj) = vocab.as_object() {
        if let Some(domains_arr) = obj.get("domains").and_then(|v| v.as_array()) {
            collect_entries_from_array(domains_arr, &mut entries);
        } else if let Some(entries_arr) = obj.get("entries").and_then(|v| v.as_array()) {
            collect_entries_from_array(entries_arr, &mut entries);
        } else if let Some(graph) = obj.get("@graph").and_then(|v| v.as_array()) {
            collect_entries_from_array(graph, &mut entries);
        } else if let Some(concepts) = obj.get("concepts") {
            if let Some(cobj) = concepts.as_object() {
                for (k, v) in cobj {
                    if let Some(iri) = value_to_iri(v) {
                        entries.push((k.clone(), iri));
                    }
                }
            }
        } else {
            // top-level slug -> iri/object
            for (k, v) in obj {
                if let Some(iri) = value_to_iri(v) {
                    entries.push((k.clone(), iri));
                }
            }
        }
    }

    for slug in domains {
        let norm = crate::domain::normalize_domain_slug(slug).unwrap_or_else(|| slug.to_lowercase());
        for (entry_slug, iri) in &entries {
            let entry_norm = crate::domain::normalize_domain_slug(entry_slug).unwrap_or_else(|| entry_slug.to_lowercase());
            if entry_norm == norm && !iri.trim().is_empty() && seen.insert(iri.clone()) {
                out.push(iri.clone());
            }
        }
    }
    out
}

fn collect_entries_from_array(arr: &[Value], out: &mut Vec<(String, String)>) {
    for item in arr {
        if let Some(obj) = item.as_object() {
            let slug = obj
                .get("slug")
                .or_else(|| obj.get("domain"))
                .or_else(|| obj.get("name"))
                .or_else(|| obj.get("@id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let iri = obj
                .get("concept_iri")
                .or_else(|| obj.get("conceptIri"))
                .or_else(|| obj.get("concept"))
                .or_else(|| obj.get("iri"))
                .or_else(|| obj.get("uri"))
                .and_then(value_to_iri);
            if let (Some(s), Some(i)) = (slug, iri) {
                out.push((s, i));
            }
        }
    }
}

fn value_to_iri(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o
            .get("concept_iri")
            .or_else(|| o.get("iri"))
            .or_else(|| o.get("uri"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Collect ontology warnings for a namespace (advisory, never blocks).
pub async fn ontology_warnings(store: &LadybugStore, namespace: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let rid = registry::registry_row_id(namespace);
    let meta = match crate::traits::VectorStore::get(store, namespace, &rid).await {
        Ok(Some((_, payload))) => payload.metadata,
        _ => Value::Null,
    };

    let cfg = match resolve_config(&meta) {
        Some(c) => c,
        None => {
            // missing: info only, not a warning record.
            return out;
        }
    };

    let path = Path::new(&cfg.path);
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            out.push(Value::json_obj(namespace, "unreadable", Some(cfg.path.clone()), e.to_string()));
            return out;
        }
    };
    // malformed check: must be JSON object
    if serde_json::from_str::<Value>(&content).is_err() {
        out.push(Value::json_obj(namespace, "malformed", Some(cfg.path.clone()), "invalid JSON".to_string()));
    }
    out
}

trait JsonObjExt {
    fn json_obj(namespace: &str, reason: &str, path: Option<String>, message: String) -> Value;
}

impl JsonObjExt for Value {
    fn json_obj(namespace: &str, reason: &str, path: Option<String>, message: String) -> Value {
        let mut obj = serde_json::json!({
            "namespace": namespace,
            "reason": reason,
            "message": message,
        });
        if let Some(p) = path {
            obj.as_object_mut().unwrap().insert("path".to_string(), serde_json::json!(p));
        }
        obj
    }
}