use crate::registry;
use crate::store::ladybug::LadybugStore;
use serde_json::Value;
use std::collections::BTreeSet;

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

/// Read and parse a vocabulary file, keeping the failure kind distinct so
/// callers can report `unreadable` (I/O) separately from `malformed` (JSON).
fn load_vocabulary_classified(path: &str) -> Result<Value, (&'static str, String)> {
    let content =
        std::fs::read_to_string(path).map_err(|e| ("unreadable", e.to_string()))?;
    serde_json::from_str(&content)
        .map_err(|e| ("malformed", format!("invalid JSON: {e}")))
}

/// Read and parse a vocabulary file. Returns parsed JSON on success.
pub fn load_vocabulary(path: &str) -> Result<Value, String> {
    load_vocabulary_classified(path).map_err(|(_, msg)| msg)
}

/// Extract concept_iri values from a vocabulary for the given domain slugs.
/// Supports several shapes (array of entries, object with domains/entries/concepts,
/// or a top-level slug->iri map).
pub fn concept_iris_from_vocabulary(vocab: &Value, domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // Collect candidate entries as (slug, concept_iri).
    let mut entries: Vec<(String, String, Vec<String>)> = Vec::new();

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
                        entries.push((k.clone(), iri, aliases_of(v)));
                    }
                }
            }
        } else {
            // top-level slug -> iri/object
            for (k, v) in obj {
                if let Some(iri) = value_to_iri(v) {
                    entries.push((k.clone(), iri, aliases_of(v)));
                }
            }
        }
    }

    for slug in domains {
        let norm = crate::domain::normalize_domain_slug(slug).unwrap_or_else(|| slug.to_lowercase());
        for (entry_slug, iri, aliases) in &entries {
            let entry_norm = crate::domain::normalize_domain_slug(entry_slug).unwrap_or_else(|| entry_slug.to_lowercase());
            let alias_match = aliases.iter().any(|a| {
                crate::domain::normalize_domain_slug(a).unwrap_or_else(|| a.to_lowercase()) == norm
            });
            if (entry_norm == norm || alias_match) && !iri.trim().is_empty() && seen.insert(iri.clone()) {
                out.push(iri.clone());
            }
        }
    }
    out
}

/// Extract an `aliases` array from a vocabulary value when the value itself is
/// an entry object (e.g. a concept object with its own alias list).
fn aliases_of(v: &Value) -> Vec<String> {
    v.get("aliases")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

fn collect_entries_from_array(arr: &[Value], out: &mut Vec<(String, String, Vec<String>)>) {
    for item in arr {
        if let Some(obj) = item.as_object() {
            let at_id = obj.get("@id").and_then(|v| v.as_str());
            // RDF/JSON-LD @graph nodes carry the concept IRI directly in @id;
            // explicit concept fields win when present.
            let iri = obj
                .get("concept_iri")
                .or_else(|| obj.get("conceptIri"))
                .or_else(|| obj.get("concept"))
                .or_else(|| obj.get("iri"))
                .or_else(|| obj.get("uri"))
                .and_then(value_to_iri)
                .or_else(|| at_id.map(|s| s.to_string()).filter(|s| !s.trim().is_empty()));
            // The slug names the domain: label fields first, then the trailing
            // IRI segment of @id (`http://x/onto#auth` -> "auth").
            let slug = obj
                .get("slug")
                .or_else(|| obj.get("domain"))
                .or_else(|| obj.get("name"))
                .or_else(|| obj.get("rdfs:label"))
                .or_else(|| obj.get("skos:prefLabel"))
                .or_else(|| obj.get("label"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    at_id
                        .and_then(|s| s.rsplit(['/', '#']).next())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                });
            let aliases = aliases_of(item);
            if let (Some(s), Some(i)) = (slug, iri) {
                out.push((s, i, aliases));
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
        Ok(None) => Value::Null,
        Err(e) => {
            tracing::warn!("ontology_warnings store.get failed for '{namespace}:{rid}': {e}");
            Value::Null
        }
    };

    let cfg = match resolve_config(&meta) {
        Some(c) => c,
        None => {
            // missing: info only, not a warning record.
            return out;
        }
    };

    // annotate mode keeps messages unchanged; strict mode prefixes each
    // warning so downstream tooling can filter hard-gated findings.
    let warn_prefix = strict_warn_prefix(&cfg.mode);

    let vocab = match load_vocabulary_classified(&cfg.path) {
        Ok(vocab) => vocab,
        Err((reason, msg)) => {
            out.push(Value::json_obj(
                namespace,
                reason,
                Some(cfg.path.clone()),
                format!("{warn_prefix}{msg}"),
            ));
            return out;
        }
    };
    // out_of_vocabulary: report domains in registry with no concept_iri in vocab.
    let entries = crate::registry::registry_domain_entries(&meta);
    for e in &entries {
        if let Some(slug) = e.get("slug").and_then(|v| v.as_str()) {
            let iris = concept_iris_from_vocabulary(&vocab, &[slug.to_string()]);
            if iris.is_empty() {
                out.push(Value::json_obj(namespace, "out_of_vocabulary", Some(cfg.path.clone()),
                    format!("{warn_prefix}domain '{}' has no concept_iri in vocabulary", slug)));
            }
        }
    }
    out
}

/// `[strict] ` when the configured ontology mode is strict (case-insensitive),
/// empty otherwise (annotate mode output must stay unchanged).
fn strict_warn_prefix(mode: &str) -> &'static str {
    if mode.eq_ignore_ascii_case("strict") {
        "[strict] "
    } else {
        ""
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_f21_resolve_config_and_load_vocabulary() {
        // resolve_config from registry metadata
        let meta_annotate = serde_json::json!({"ontology_config":{"path":"/tmp/vocab.json","mode":"annotate"}});
        let cfg = resolve_config(&meta_annotate).unwrap();
        assert_eq!(cfg.path, "/tmp/vocab.json");
        assert_eq!(cfg.mode, "annotate");

        let meta_strict = serde_json::json!({"ontology_config":{"path":"/tmp/vocab.json","mode":"strict"}});
        let cfg_strict = resolve_config(&meta_strict).unwrap();
        assert_eq!(cfg_strict.mode, "strict");

        // default mode when missing
        let meta_default = serde_json::json!({"ontology_config":{"path":"/tmp/vocab.json"}});
        let cfg_def = resolve_config(&meta_default).unwrap();
        assert_eq!(cfg_def.mode, "annotate");

        // Write temporary vocabulary file
        let tmp = std::env::temp_dir().join("neurostrata_vocab_f21.json");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            let vocab = serde_json::json!([
                {"slug":"auth","concept_iri":"http://example/auth","aliases":["authentication","login"]},
                {"slug":"database","concept_iri":"http://example/db","aliases":[]}
            ]);
            f.write_all(vocab.to_string().as_bytes()).unwrap();
        }

        let loaded = load_vocabulary(tmp.to_str().unwrap()).unwrap();
        assert!(loaded.is_array());

        // concept_iris_from_vocabulary matches canonical slug and alias
        let iris_slugs = concept_iris_from_vocabulary(&loaded, &["auth".to_string()]);
        assert!(iris_slugs.contains(&"http://example/auth".to_string()));
        let iris_alias = concept_iris_from_vocabulary(&loaded, &["authentication".to_string()]);
        assert!(iris_alias.contains(&"http://example/auth".to_string()));

        // Missing vocabulary file returns Err
        let missing = load_vocabulary("/tmp/nonexistent_neurostrata_vocab_12345.json");
        assert!(missing.is_err());

        // Cleanup
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn strict_mode_prefixes_warning_messages_annotate_unchanged() {
        assert_eq!(strict_warn_prefix("annotate"), "");
        assert_eq!(strict_warn_prefix(""), "");
        assert_eq!(strict_warn_prefix("STRICT"), "[strict] ");
        assert_eq!(strict_warn_prefix("strict"), "[strict] ");

        // The prefix is prepended to the emitted message text, keeping the
        // annotate-mode message identical to the pre-strict surface.
        let annotate_msg = "domain 'auth' has no concept_iri in vocabulary";
        assert_eq!(format!("{}{annotate_msg}", strict_warn_prefix("annotate")), annotate_msg);
        assert_eq!(
            format!("{}{annotate_msg}", strict_warn_prefix("strict")),
            format!("[strict] {annotate_msg}")
        );
    }
}