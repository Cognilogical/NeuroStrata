use sha2::{Digest, Sha256};

pub const NS_PREFIX: &str = "urn:neurostrata:ns#";

/// Canonical mesh wire N-Quads builder (spec 770f: §5.2, §5.3 Phase 1).
/// Produces deterministic sorted N-Quads + sha256 digest.
pub fn build_mesh_canonical_nquads(
    id: &str,
    namespace: &str,
    memory_type: &str,
    lifecycle_state: &str,
    created_at: &str,
    created_by: &str,
    superseded_by: Option<&str>,
    archived_at: Option<&str>,
    tombstoned_at: Option<&str>,
    content: &str,
    eligibility: Option<&str>,
    source: Option<&str>,
    tags: &[String],
    concept_iri: Option<&str>,
    relationships: &[(String, String)],
    update_id: &str,
    origin_mesh_epoch: u32,
    trust: &str,
    trigger: Option<&str>,
    initial_fires: Option<i64>,
    valid_to: Option<&str>,
    rehearsal_script: Option<&str>,
) -> (String, String) {
    let graph = format!("urn:neurostrata:graph:{}:domain:gen:0", namespace);
    let str_dt = "http://www.w3.org/2001/XMLSchema#string";
    let dt_dt = "http://www.w3.org/2001/XMLSchema#dateTime";
    let int_dt = "http://www.w3.org/2001/XMLSchema#integer";
    let ns = |name: &str| format!("{NS_PREFIX}{name}");
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");

    let mut lines: Vec<String> = Vec::new();
    let mut push = |line: String| { lines.push(line); };

    push(format!("<urn:neurostrata:memory:{}> <{}> <urn:neurostrata:memory:{}> <{}> .", id, ns("id"), id, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("namespace"), esc(namespace), str_dt, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("memoryType"), esc(memory_type), str_dt, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("lifecycleState"), esc(lifecycle_state), str_dt, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("createdAt"), esc(created_at), dt_dt, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("createdBy"), esc(created_by), str_dt, graph));
    if let Some(s) = superseded_by {
        push(format!("<urn:neurostrata:memory:{}> <{}> <urn:neurostrata:memory:{}> <{}> .", id, ns("supersededBy"), s, graph));
    }
    if let Some(a) = archived_at {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("archivedAt"), esc(a), dt_dt, graph));
    }
    if let Some(t) = tombstoned_at {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("tombstonedAt"), esc(t), dt_dt, graph));
    }
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("content"), esc(content), str_dt, graph));
    if let Some(tr) = trigger {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("trigger"), esc(tr), str_dt, graph));
    }
    if let Some(f) = initial_fires {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("initialFires"), esc(&f.to_string()), int_dt, graph));
    }
    if let Some(vt) = valid_to {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("validTo"), esc(vt), dt_dt, graph));
    }
    if let Some(rs) = rehearsal_script {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("rehearsalScript"), esc(rs), str_dt, graph));
    }
    if let Some(el) = eligibility {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("eligibility"), esc(el), str_dt, graph));
    }
    if let Some(src) = source {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("source"), esc(src), str_dt, graph));
    }
    for tag in tags.iter() {
        push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("tag"), esc(tag), str_dt, graph));
    }
    if let Some(ci) = concept_iri {
        push(format!("<urn:neurostrata:memory:{}> <{}> <{}> <{}> .", id, ns("conceptIri"), ci, graph));
    }
    for (kind, target_uuid) in relationships.iter() {
        let pred_name = if kind == "supersedes" { "supersedes" } else if kind == "references" { "references" } else { continue };
        push(format!("<urn:neurostrata:memory:{}> <{}> <urn:neurostrata:memory:{}> <{}> .", id, ns(pred_name), target_uuid, graph));
    }
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("updateId"), esc(update_id), str_dt, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("originMeshEpoch"), esc(&origin_mesh_epoch.to_string()), int_dt, graph));
    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("trust"), esc(trust), str_dt, graph));

    let mut set = std::collections::BTreeSet::new();
    for l in lines { set.insert(l); }
    let mut sorted: Vec<String> = set.into_iter().collect();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let nquads_text = if sorted.is_empty() { String::new() } else { sorted.join("\n") + "\n" };
    let mut h = Sha256::new();
    h.update(nquads_text.as_bytes());
    let digest = format!("sha256:{}", hex::encode(h.finalize()));
    (nquads_text, digest)
}

/// Rebuild the canonical mesh wire N-Quads for a MeshRecord (spec §5.2f
/// shadow reconstruction). Extracts all fields from the record's own
/// metadata + lifecycle + relationships so the reconstructed payload is
/// independent of any caller-side escaping or ordering.
fn rebuild_canonical_nquads(record: &crate::synaptic_mesh::record::MeshRecord) -> String {
    use crate::synaptic_mesh::record::LifecycleState;
    let meta = record.metadata.as_object().cloned().unwrap_or_default();
    // Content: match worker.rs pattern — string directly, else JSON serialize
    let content_str = match &record.content {
        serde_json::Value::String(s) => s.clone(),
        _ => serde_json::to_string(&record.content).unwrap_or_default(),
    };
    // Procedure fields: for "procedure" type, extract from content object (worker.rs pattern)
    let (trigger, initial_fires, valid_to, rehearsal_script) = if record.memory_type == "procedure" {
        if let Some(def) = record.content.as_object() {
            let trig = def.get("trigger").and_then(|v| v.as_str()).map(String::from);
            let fires = def.get("remaining_fires_initial").and_then(|v| v.as_i64());
            let vt = def.get("valid_to").and_then(|v| v.as_str()).map(String::from);
            let rs = def.get("rehearsal_script").and_then(|v| v.as_str()).map(String::from);
            (trig, fires, vt, rs)
        } else { (None, None, None, None) }
    } else { (None, None, None, None) };
    let tags: Vec<String> = meta.get("tags").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut rels: Vec<(String, String)> = Vec::new();
    if let Some(refs) = meta.get("references").and_then(|v| v.as_array()) {
        for r in refs { if let Some(s) = r.as_str() { rels.push(("references".to_string(), s.to_string())); } }
    }
    for r in &record.relationships {
        rels.push((r.kind.clone(), r.target.to_string()));
    }
    let created_at = record.lifecycle.created_at.to_rfc3339();
    let archived_at = record.lifecycle.archived_at.map(|d| d.to_rfc3339());
    let tombstoned_at = record.lifecycle.tombstoned_at.map(|d| d.to_rfc3339());
    let superseded_by = record.lifecycle.superseded_by.map(|u| u.to_string());
    let eligibility = meta.get("eligibility").and_then(|v| v.as_str());
    let source = meta.get("source").and_then(|v| v.as_str());
    let concept_iri = meta.get("concept_iri").and_then(|v| v.as_str());
    let update_id = record.update_id.to_string();
    let trust = record.trust.to_string();
    let (canonical, _digest) = build_mesh_canonical_nquads(
        &record.id.to_string(), &record.namespace, &record.memory_type,
        &record.lifecycle.state.to_string(), &created_at, &record.lifecycle.created_by,
        superseded_by.as_deref(), archived_at.as_deref(), tombstoned_at.as_deref(),
        &content_str, eligibility, source, &tags, concept_iri, &rels,
        &update_id, record.origin_mesh_epoch, &trust,
        trigger.as_deref(), initial_fires, valid_to.as_deref(), rehearsal_script.as_deref(),
    );
    canonical
}

/// Parse an N-Quads text into a sorted set of canonical quad lines.
/// Returns (canonical lines, parsed error messages).
fn canonical_quads(text: &str) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let mut set = std::collections::BTreeSet::new();
    let mut errors: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        for q in oxttl::NQuadsParser::new().for_slice(trimmed.as_bytes()) {
            match q {
                Ok(qr) => { set.insert(qr.to_string()); }
                Err(e) => { errors.push(format!("line {}: oxttl parse error: {}", i + 1, e)); }
            }
        }
    }
    (set, errors)
}

/// Canonical mesh wire predicate allowlist (spec §5.2a). Any predicate
/// not in this set is refused at import time (default-deny).
pub const MESH_WIRE_PREDICATES: &[&str] = &[
    "urn:neurostrata:ns#id",
    "urn:neurostrata:ns#namespace",
    "urn:neurostrata:ns#memoryType",
    "urn:neurostrata:ns#lifecycleState",
    "urn:neurostrata:ns#createdAt",
    "urn:neurostrata:ns#createdBy",
    "urn:neurostrata:ns#supersededBy",
    "urn:neurostrata:ns#archivedAt",
    "urn:neurostrata:ns#tombstonedAt",
    "urn:neurostrata:ns#content",
    "urn:neurostrata:ns#trigger",
    "urn:neurostrata:ns#initialFires",
    "urn:neurostrata:ns#validTo",
    "urn:neurostrata:ns#rehearsalScript",
    "urn:neurostrata:ns#eligibility",
    "urn:neurostrata:ns#source",
    "urn:neurostrata:ns#tag",
    "urn:neurostrata:ns#conceptIri",
    "urn:neurostrata:ns#updateId",
    "urn:neurostrata:ns#originMeshEpoch",
    "urn:neurostrata:ns#trust",
    "urn:neurostrata:ns#supersedes",
    "urn:neurostrata:ns#references",
];

/// Validate a mesh record's canonical RDF payload (spec §5.2f shadow
/// reconstruction). Returns `Ok(())` when the payload is well-formed and
/// semantically equivalent to the JSON envelope; `Err(violations)` otherwise.
///
/// Phase 1 semantics: the caller applies the valid JSON payload regardless
/// and emits the violations as diagnostics; the canonical payload is
/// authoritative only in Phase 2.
pub fn validate_canonical_rdf(
    record: &crate::synaptic_mesh::record::MeshRecord,
    rdf: &crate::synaptic_mesh::record::RdfBlock,
) -> Result<(), Vec<String>> {
    use crate::synaptic_mesh::import::{MAX_RDF_QUADS_BYTES, MAX_RDF_QUADS_COUNT};
    let mut violations: Vec<String> = Vec::new();
    if rdf.schema != "neurostrata-rdf-record/v1" {
        violations.push(format!(
            "rdf.schema is {} (expected neurostrata-rdf-record/v1)",
            rdf.schema
        ));
    }
    if rdf.format != "n-quads" {
        violations.push(format!(
            "rdf.format is {} (expected n-quads)",
            rdf.format
        ));
    }
    if rdf.quads.is_empty() {
        violations.push("rdf.quads is empty".to_string());
        return Err(violations);
    }
    // Spec §5.5: bounds checks
    if rdf.quads.len() > MAX_RDF_QUADS_BYTES {
        violations.push(format!(
            "rdf.quads is {} bytes, exceeds 512 KiB max",
            rdf.quads.len()
        ));
    }
    // Count lines (quads) - each line ends with \n
    let quad_count = rdf.quads.lines().filter(|l| !l.trim().is_empty()).count();
    if quad_count > MAX_RDF_QUADS_COUNT {
        violations.push(format!(
            "rdf contains {} quads, exceeds {} max",
            quad_count, MAX_RDF_QUADS_COUNT
        ));
    }
    // Recompute the canonical hash from the quads text and compare.
    let mut h = Sha256::new();
    h.update(rdf.quads.as_bytes());
    let recomputed = format!("sha256:{}", hex::encode(h.finalize()));
    if recomputed != rdf.canonical_hash {
        violations.push(format!(
            "canonical_hash {} does not match recomputed {}",
            rdf.canonical_hash, recomputed
        ));
    }
    // Validate named graph IRI (spec §5.2e)
    let expected_graph = format!("<urn:neurostrata:graph:{}:domain:gen:0>", record.namespace);
    // Parse each line
    for (i, line) in rdf.quads.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let line_no_dot = line.strip_suffix('.').unwrap_or(line);
        // Simplified parse: split by whitespace
        let parts: Vec<&str> = line_no_dot.split_whitespace().collect();
        if parts.len() < 4 {
            violations.push(format!(
                "line {}: malformed N-Quads (expected 4 terms, got {})",
                i + 1, parts.len()
            ));
            continue;
        }
        let subj = parts[0];
        let pred = parts[1];
        let graph = parts.last().unwrap();

        // Validate subject
        let expected_subj = format!("<urn:neurostrata:memory:{}>", record.id);
        if subj != expected_subj {
            violations.push(format!(
                "line {}: subject {} does not match record id {}",
                i + 1, subj, record.id
            ));
        }
        // Validate named graph
        let expected_graph_bracketed = format!("<urn:neurostrata:graph:{}:domain:gen:0>", record.namespace);
        if *graph != expected_graph_bracketed {
            violations.push(format!(
                "line {}: graph {} does not match expected {}",
                i + 1, graph, expected_graph
            ));
        }
        // Strip angle brackets from predicate for allowlist check
        let pred_unbracketed = if pred.starts_with('<') && pred.ends_with('>') {
            &pred[1..pred.len()-1]
        } else {
            pred
        };
        if !MESH_WIRE_PREDICATES.contains(&pred_unbracketed) {
            violations.push(format!(
                "line {}: predicate {} is not in the mesh wire allowlist",
                i + 1, pred
            ));
        }
    }
    // Spec §5.2f shadow reconstruction: compare canonical N-Quads
    // from MeshRecord against payload quads (single quad-set compare,
    // not per-line — eliminates O(N²) parse and duplicate errors).
    let rebuilt_text = rebuild_canonical_nquads(record);
    let (rebuilt_set, rebuilt_errors) = canonical_quads(&rebuilt_text);
    let (payload_set, payload_errors) = canonical_quads(&rdf.quads);
    for e in rebuilt_errors.into_iter().chain(payload_errors) {
        violations.push(e);
    }
    if rebuilt_set != payload_set {
        violations.push("§5.2f shadow reconstruction: rebuilt MeshRecord N-Quads (canonical_quads) do not match payload N-Quads".to_string());
        for q in rebuilt_set.iter() {
            if !payload_set.contains(q) {
                violations.push(format!("reconstructed-only quad: {}", q));
            }
        }
        for q in payload_set.iter() {
            if !rebuilt_set.contains(q) {
                violations.push(format!("payload-only quad: {}", q));
            }
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_mesh_canonical_nquads_is_deterministic() {
        let tags: Vec<String> = vec!["a".to_string()];
        let (a, da) = build_mesh_canonical_nquads(
            "11111111-1111-1111-1111-111111111111", "ns", "rule", "active",
            "2026-10-29T00:00:00Z", "node-author", None, None, None,
            "content", Some("shareable"), Some("owner"), tags.as_slice(),
            Some("https://neurostrata/iri/concept/test"),
            &[("supersedes".to_string(), "22222222-2222-2222-2222-222222222222".to_string())],
            "upd-1", 1, "trusted",
            None, None, None, None,
        );
        let (b, db) = build_mesh_canonical_nquads(
            "11111111-1111-1111-1111-111111111111", "ns", "rule", "active",
            "2026-10-29T00:00:00Z", "node-author", None, None, None,
            "content", Some("shareable"), Some("owner"), tags.as_slice(),
            Some("https://neurostrata/iri/concept/test"),
            &[("supersedes".to_string(), "22222222-2222-2222-2222-222222222222".to_string())],
            "upd-1", 1, "trusted",
            None, None, None, None,
        );
        assert_eq!(a, b, "N-Quads must be byte-identical");
        assert_eq!(da, db, "SHA-256 must match");
    }

    #[test]
    fn validate_canonical_rdf_on_built_record() {
        let tags = vec!["t1".to_string(), "t2".to_string()];
        let uuid_str = "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11";
        let update_id_val = "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a14";
        let (nquads, digest) = build_mesh_canonical_nquads(
            uuid_str, "test.ns", "memory", "active",
            "2026-10-30T12:00:00+00:00", "author-1",
            Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a12"),
            Some("2026-10-30T13:00:00+00:00"), None,
            "test content body", Some("shareable"), Some("owner"), &tags,
            Some("https://example.com/concept/test"),
            &[("supersedes".to_string(), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a13".to_string())],
            &update_id_val, 3, "trusted",  // origin_mesh_epoch=3 matches MeshRecord
            None, None, None, None,
        );
        use crate::synaptic_mesh::record::{LifecycleState, MeshRecord, RdfBlock};
        use uuid::Uuid;
        use chrono::{DateTime, Utc};
        let record = MeshRecord {
            schema_version: "v1".to_string(),
            id: Uuid::parse_str(uuid_str).unwrap(),
            namespace: "test.ns".to_string(),
            memory_type: "memory".to_string(),
            lifecycle: crate::synaptic_mesh::record::Lifecycle {
                state: LifecycleState::Active,
                created_at: DateTime::parse_from_rfc3339("2026-10-30T12:00:00+00:00").unwrap().with_timezone(&Utc),
                created_by: "author-1".to_string(),
                superseded_by: Some(Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a12").unwrap()),
                archived_at: Some(DateTime::parse_from_rfc3339("2026-10-30T13:00:00+00:00").unwrap().with_timezone(&Utc)),
                tombstoned_at: None,
            },
            content: serde_json::json!("test content body"),
            metadata: serde_json::from_str(r#"{"tags":["t1","t2"],"eligibility":"shareable","source":"owner","concept_iri":"https://example.com/concept/test","supersedes":["a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a13"],"update_id":"a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a14"}"#).unwrap(),
            rdf: Some(RdfBlock { schema: "neurostrata-rdf-record/v1".to_string(), format: "n-quads".to_string(), canonical_hash: digest.clone(), quads: nquads.clone() }),
            relationships: vec![crate::synaptic_mesh::record::Relationship { kind: "supersedes".to_string(), target: Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a13").unwrap() }],
            update_id: Uuid::parse_str(update_id_val).unwrap(),
            origin_mesh_epoch: 3,
            trust: crate::synaptic_mesh::manifest::Trust::Trusted,
        };
        assert!(validate_canonical_rdf(&record, record.rdf.as_ref().unwrap()).is_ok(), "shadow reconstruction (§5.2f) must pass for built record");
    }
}
