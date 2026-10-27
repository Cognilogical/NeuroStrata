use crate::domain;
use crate::rdf::ontology::OntologyIndex;
use serde_json::Value;

/// Canonicalize the `component` and `target_system` fields of a topology
/// object using the ontology's alias map, falling back to
/// `domain::canonical_entity_slug`.
pub fn canonicalize_topology(topology: &mut Value, index: &OntologyIndex) {
    if let Some(obj) = topology.as_object_mut() {
        for key in ["component", "target_system"] {
            if let Some(Value::String(raw)) = obj.get(key).cloned() {
                let canonical = index
                    .canonical_slug(&raw)
                    .unwrap_or_else(|| domain::canonical_entity_slug(&raw));
                obj.insert(key.to_string(), Value::String(canonical));
            }
        }
    }
}

/// Canonicalize a raw entity alias to a slug, using the ontology when possible.
pub fn canonicalize_known_ids_alias(raw: &str, index: &OntologyIndex) -> String {
    index
        .canonical_slug(raw)
        .unwrap_or_else(|| domain::canonical_entity_slug(raw))
}