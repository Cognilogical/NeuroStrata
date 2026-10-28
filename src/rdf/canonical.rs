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
/// Render a memory as a deterministic N-Quads string (canonical RDF projection).
/// This is a read-only projection; it does not change the primary store.
pub fn memory_to_nquads(id: &str, payload: &crate::traits::MemoryPayload) -> String {
    let subj = format!("<urn:neurostrata:memory:{}>", id);
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("{} <urn:neurostrata:content> \"{}\" .", subj, escape_nq(&payload.content)));
    lines.push(format!("{} <urn:neurostrata:memory_type> \"{}\" .", subj, escape_nq(&payload.memory_type)));
    lines.push(format!("{} <urn:neurostrata:user_id> \"{}\" .", subj, escape_nq(&payload.user_id)));
    if !payload.location.is_empty() {
        lines.push(format!("{} <urn:neurostrata:location> \"{}\" .", subj, escape_nq(&payload.location)));
    }
    if let Some(obj) = payload.metadata.as_object() {
        for (k, v) in obj {
            let val = match v {
                serde_json::Value::String(s) => format!("\"{}\"", escape_nq(s)),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => "null".to_string(),
                other => format!("\"{}\"", escape_nq(&other.to_string())),
            };
            lines.push(format!("{} <urn:neurostrata:meta:{}> {} .", subj, escape_nq(k), val));
        }
    }
    lines.sort();
    if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" }
}

/// SHA-256 digest of the canonical N-Quads projection.
pub fn nquads_digest(nquads: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(nquads.as_bytes());
    format!("{:x}", h.finalize())
}

fn escape_nq(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    #[test]
    fn memory_to_nquads_is_deterministic() {
        let payload = MemoryPayload {
            content: "hello".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "src/lib.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: json!({"domain":"auth"}),
        };
        let a = memory_to_nquads("m1", &payload);
        let b = memory_to_nquads("m1", &payload);
        assert_eq!(a, b);
        assert!(a.contains("urn:neurostrata:memory:m1"));
    }
}
