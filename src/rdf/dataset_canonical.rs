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

    push(format!("<urn:neurostrata:memory:{}> <{}> \"{}\"^^<{}> <{}> .", id, ns("id"), esc(id), str_dt, graph));
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
        // Spec §5.2f shadow reconstruction: compare parsed object term
        // against MeshRecord fields using oxttl N-Quads parsing.
        for quad_res in oxttl::NQuadsParser::new().for_slice(line.trim().as_bytes()) {
            match quad_res {
                Ok(quad_ref) => {
                    let pred_iri = quad_ref.predicate.as_str();
                    let obj_term = quad_ref.object.as_ref();
                    let obj_str = obj_term.to_string();
                    // Map predicate -> expected object serialized form
                    let expected_obj: Option<String> = match pred_iri {
                        "urn:neurostrata:ns#id" => Some(format!("<urn:neurostrata:memory:{}>", record.id)),
                        "urn:neurostrata:ns#namespace" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", record.namespace.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r"))),
                        "urn:neurostrata:ns#memoryType" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", record.memory_type)),
                        "urn:neurostrata:ns#lifecycleState" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", record.lifecycle.state)),
                        "urn:neurostrata:ns#createdAt" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>", record.lifecycle.created_at.to_rfc3339())),
                        "urn:neurostrata:ns#createdBy" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", record.lifecycle.created_by)),
                        "urn:neurostrata:ns#supersededBy" => {
                            if let Some(u) = record.lifecycle.superseded_by {
                                Some(format!("<urn:neurostrata:memory:{}>", u))
                            } else { Some("_ABSENT_".to_string()) }
                        }
                        "urn:neurostrata:ns#archivedAt" => {
                            if let Some(dt) = record.lifecycle.archived_at {
                                Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>", dt.to_rfc3339()))
                            } else { Some("_ABSENT_".to_string()) }
                        }
                        "urn:neurostrata:ns#tombstonedAt" => {
                            if let Some(dt) = record.lifecycle.tombstoned_at {
                                Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>", dt.to_rfc3339()))
                            } else { Some("_ABSENT_".to_string()) }
                        }
                        "urn:neurostrata:ns#content" => {
                            let s = record.content.as_str().unwrap_or("").to_string();
                            Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r")))
                        }
                        "urn:neurostrata:ns#updateId" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", record.update_id)),
                        "urn:neurostrata:ns#originMeshEpoch" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>", record.origin_mesh_epoch)),
                        "urn:neurostrata:ns#trust" => Some(format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#string>", record.trust)),
                        _ => None,
                    };
                    if let Some(exp) = expected_obj {
                        if obj_str != exp {
                            violations.push(format!("line {}: predicate {} object {} does not match MeshRecord (expected {})", i + 1, pred_iri, obj_str, exp));
                        }
                    }
                }
                Err(_) => {
                    violations.push(format!("line {}: oxttl parse error for N-Quads object", i + 1));
                }
            }
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
}