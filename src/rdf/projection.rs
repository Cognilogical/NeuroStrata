//! RDF projection: MemoryPayload → RDF quads (oxrdf-backed).
//! 
//! Phase 2 migration: uses oxrdf::Quad and oxrdf::Term instead of hand-written
//! Quad/QuadObject. The OxRdfBridge provides conversion to/from legacy types.

use crate::traits::MemoryPayload;
use super::namespace::{field_predicate, ns_predicate, relation_predicate};
use super::types::{EdgeProvenance, Tier};
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Literal, GraphName, Quad};
use std::collections::BTreeMap;

/// XSD string datatype IRI
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// JSON-LD JSON datatype IRI
const LDP_JSON: &str = "https://www.w3.org/ns/ldp#JSON";

/// Sanitize a dynamic metadata key for use as an IRI suffix so
/// `NamedNode::new_unchecked` can never panic on spaces or `<`/`>`.
fn sanitize_iri_suffix(k: &str) -> String {
    k.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// Project a single Memory row into oxrdf RDF quads.
/// Returns a vector of oxrdf::Quad that can be inserted into a Dataset.
pub fn memory_to_quads(
    id: &str,
    payload: &MemoryPayload,
    namespace: &str,
    tier: Tier,
    generation: u64,
) -> Vec<Quad> {
    let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!("urn:neurostrata:memory:{}", id)));
    let graph_iri = format!("urn:neurostrata:graph:{}:{}:gen:{}",
        namespace, tier.as_str(), generation);
    let graph = GraphName::NamedNode(
        NamedNode::new_unchecked(graph_iri.clone())
    );

    let xsd_string = NamedNode::new_unchecked(XSD_STRING.to_string());
    let ldp_json = NamedNode::new_unchecked(LDP_JSON.to_string());

    let mut quads = Vec::new();

    // Helper to produce a typed literal quad
    macro_rules! lit_quad {
        ($pred:expr, $val:expr, $dt:expr) => {
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked($pred),
                object: Term::Literal(Literal::new_typed_literal($val, $dt)),
                graph_name: graph.clone(),
            });
        };
    }

    // Helper to produce a JSON-typed literal quad
    macro_rules! json_quad {
        ($pred:expr, $val:expr, $dt:expr) => {
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked($pred),
                object: Term::Literal(Literal::new_typed_literal($val, $dt)),
                graph_name: graph.clone(),
            });
        };
    }

    macro_rules! iri_quad {
        ($pred:expr, $iri:expr) => {
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked($pred),
                object: Term::NamedNode(NamedNode::new_unchecked($iri)),
                graph_name: graph.clone(),
            });
        };
    }

    // Core fields.
    lit_quad!(field_predicate("content"), payload.content.clone(), xsd_string.clone());
    lit_quad!(field_predicate("user_id"), payload.user_id.clone(), xsd_string.clone());
    lit_quad!(field_predicate("memory_type"), payload.memory_type.clone(), xsd_string.clone());

    if let Some(agent) = &payload.agent_name {
        lit_quad!(field_predicate("agent_name"), agent.clone(), xsd_string.clone());
    }

    lit_quad!(field_predicate("location"), payload.location.clone(), xsd_string.clone());
    lit_quad!(field_predicate("location_lines"), payload.location_lines.clone(), xsd_string.clone());

    // Metadata as a JSON literal (lossless round-trip).
    json_quad!(field_predicate("metadata"), payload.metadata.to_string(), ldp_json.clone());

    // Selected metadata sub-fields as first-class predicates.
    if let Some(domain) = payload.metadata.get("domain") {
        match domain {
            serde_json::Value::String(s) => {
                lit_quad!(ns_predicate("domain"), s.clone(), xsd_string.clone());
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        lit_quad!(ns_predicate("domain"), s.to_string(), xsd_string.clone());
                    }
                }
            }
            _ => {}
        }
    }
    // concept_iri is emitted as a separate predicate (not as the RDF subject),
    // so multiple memories sharing one concept keep distinct subjects.
    if let Some(concept) = payload.metadata.get("concept_iri") {
        let iris: Vec<String> = match concept {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
            _ => vec![],
        };
        for iri in iris {
            if iri.trim().is_empty() { continue; }
            iri_quad!(ns_predicate("conceptIri"), iri);
        }
    }
    if let Some(facets) = payload.metadata.get("facets").and_then(|v| v.as_array()) {
        for f in facets {
            if let Some(s) = f.as_str() {
                lit_quad!(ns_predicate("facet"), s.to_string(), xsd_string.clone());
            }
        }
    }
    if let Some(source) = payload.metadata.get("source") {
        json_quad!(ns_predicate("source"), source.to_string(), ldp_json.clone());
    }
    if let Some(topology) = payload.metadata.get("topology") {
        if let Some(obj) = topology.as_object() {
            for (k, v) in obj {
                json_quad!(ns_predicate(&format!("topology_{}", sanitize_iri_suffix(k))), v.to_string(), ldp_json.clone());
            }
        }
    }

    // ReBAC visible_to projection: `<mem_iri> ns:visibleTo "<principal>:<role>"`
    if let Some(vt) = payload.metadata.get("visible_to").and_then(|v| v.as_array()) {
        for entry in vt {
            if let (Some(p), Some(r)) = (
                entry.get("principal").and_then(|v| v.as_str()),
                entry.get("role").and_then(|v| v.as_str()),
            ) {
                lit_quad!(
                    ns_predicate("visibleTo"),
                    format!("{}:{}", p, r),
                    xsd_string.clone()
                );
            }
        }
    }

    // Digital twin RDF projection
    if payload.memory_type == "digital_twin" {
        if let Some(dt) = payload
            .metadata
            .get("digital_twin")
            .or_else(|| payload.metadata.get("twin"))
            .and_then(|v| v.as_object())
        {
            let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer");
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("TwinState"))),
                graph_name: graph.clone(),
            });
            if let Some(twin_id) = dt.get("twin_id").or_else(|| dt.get("id")).and_then(|v| v.as_str()) {
                lit_quad!(ns_predicate("twinId"), twin_id.to_string(), xsd_string.clone());
            }
            if let Some(obs) = dt
                .get("observed_at")
                .and_then(|v| v.as_i64())
                .or_else(|| payload.metadata.get("valid_from").and_then(|v| v.as_i64()))
            {
                lit_quad!(ns_predicate("observedAt"), obs.to_string(), xsd_int.clone());
            }
            if let Some(props) = dt.get("properties").or_else(|| dt.get("variables")).and_then(|v| v.as_object()) {
                for (k, v) in props {
                    json_quad!(
                        ns_predicate(&format!("twinProp_{}", sanitize_iri_suffix(k))),
                        v.to_string(),
                        ldp_json.clone()
                    );
                }
            }
        }
    }

    // Multimodal attachment RDF projection
    if let Some(atts) = payload.metadata.get("attachments").and_then(|v| v.as_array()) {
        let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer");
        for (idx, att) in atts.iter().enumerate() {
            let Some(obj) = att.as_object() else { continue };
            let raw_id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let sanitized_id: String = raw_id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
                .collect();
            let bnode_id = if sanitized_id.is_empty() {
                format!("att_{}_{}", id.replace('-', "_"), idx)
            } else {
                format!("att_{}_{}", id.replace('-', "_"), sanitized_id)
            };
            let bnode = oxrdf::BlankNode::new_unchecked(bnode_id);
            let bnode_subj = NamedOrBlankNode::BlankNode(bnode.clone());
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked(ns_predicate("hasAttachment")),
                object: Term::BlankNode(bnode),
                graph_name: graph.clone(),
            });
            quads.push(Quad {
                subject: bnode_subj.clone(),
                predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("Attachment"))),
                graph_name: graph.clone(),
            });
            if !raw_id.is_empty() {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentId")),
                    object: Term::Literal(Literal::new_typed_literal(raw_id.to_string(), xsd_string.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(mime) = obj.get("mime").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentMime")),
                    object: Term::Literal(Literal::new_typed_literal(mime.to_string(), xsd_string.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(sz) = obj.get("size_bytes").and_then(|v| v.as_u64()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentSizeBytes")),
                    object: Term::Literal(Literal::new_typed_literal(sz.to_string(), xsd_int.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(dig) = obj.get("digest").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentDigest")),
                    object: Term::Literal(Literal::new_typed_literal(dig.to_string(), xsd_string.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(uri) = obj.get("uri").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentUri")),
                    object: Term::Literal(Literal::new_typed_literal(uri.to_string(), xsd_string.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(w) = obj.get("width").and_then(|v| v.as_u64()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentWidth")),
                    object: Term::Literal(Literal::new_typed_literal(w.to_string(), xsd_int.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(h) = obj.get("height").and_then(|v| v.as_u64()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentHeight")),
                    object: Term::Literal(Literal::new_typed_literal(h.to_string(), xsd_int.clone())),
                    graph_name: graph.clone(),
                });
            }
            if let Some(alt) = obj.get("alt_text").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentAltText")),
                    object: Term::Literal(Literal::new_typed_literal(alt.to_string(), xsd_string.clone())),
                    graph_name: graph.clone(),
                });
            }
        }
    }

    // Add RDF type declaration to the graph name
    quads.push(Quad {
        subject: subject.clone(),
        predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        object: Term::NamedNode(NamedNode::new_unchecked(format!("urn:neurostrata:Memory"))),
        graph_name: graph.clone(),
    });

    quads
}

/// Project one LadybugDB edge into an oxrdf Quad.
///
/// Single-quad core; provenance quads are emitted by [`edge_to_quads`] because
/// a single-Quad return cannot carry the extra provenance rows.
pub fn edge_to_quad(
    from: &str,
    rel_type: &str,
    to: &str,
    _provenance: Option<EdgeProvenance>,
    graph: Option<GraphName>,
) -> Quad {
    let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!("urn:neurostrata:memory:{}", from)));
    let object = Term::NamedNode(NamedNode::new_unchecked(format!("urn:neurostrata:memory:{}", to)));
    let graph = graph.unwrap_or(GraphName::DefaultGraph);

    Quad {
        subject,
        predicate: NamedNode::new_unchecked(relation_predicate(rel_type)),
        object,
        graph_name: graph,
    }
}

/// Project one LadybugDB edge into oxrdf Quads, emitting provenance quads
/// (`ns:provenanceSource`, `ns:provenanceValidFrom/To`, `ns:provenanceGeneration`,
/// `ns:provenanceExtractor`) beside the relation quad when provenance is Some.
pub fn edge_to_quads(
    from: &str,
    rel_type: &str,
    to: &str,
    provenance: Option<EdgeProvenance>,
    graph: Option<GraphName>,
) -> Vec<Quad> {
    let mut quads = vec![edge_to_quad(from, rel_type, to, None, graph.clone())];
    if let Some(prov) = provenance {
        let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!("urn:neurostrata:memory:{}", from)));
        let graph = graph.unwrap_or(GraphName::DefaultGraph);
        let xsd_string = NamedNode::new_unchecked(XSD_STRING.to_string());
        let xsd_integer = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer".to_string());
        let mut provenance_quals: Vec<(&'static str, Term)> = Vec::new();
        if let Some(src) = prov.source {
            provenance_quals.push((
                "provenanceSource",
                Term::Literal(Literal::new_typed_literal(src, xsd_string.clone())),
            ));
        }
        if let Some(vf) = prov.valid_from {
            provenance_quals.push((
                "provenanceValidFrom",
                Term::Literal(Literal::new_typed_literal(vf.to_string(), xsd_integer.clone())),
            ));
        }
        if let Some(vt) = prov.valid_to {
            provenance_quals.push((
                "provenanceValidTo",
                Term::Literal(Literal::new_typed_literal(vt.to_string(), xsd_integer.clone())),
            ));
        }
        if let Some(gen) = prov.generation {
            provenance_quals.push((
                "provenanceGeneration",
                Term::Literal(Literal::new_typed_literal(gen.to_string(), xsd_integer.clone())),
            ));
        }
        if let Some(ex) = prov.extractor {
            provenance_quals.push((
                "provenanceExtractor",
                Term::Literal(Literal::new_typed_literal(ex, xsd_string.clone())),
            ));
        }
        for (name, term) in provenance_quals {
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked(ns_predicate(name)),
                object: term,
                graph_name: graph.clone(),
            });
        }
    }
    quads
}

/// Combine node + edge quads for a namespace snapshot.
/// Returns legacy `crate::rdf::types::Quad` for backward compatibility with LadybugStore API.
pub fn rows_to_quads(
    rows: &[(String, MemoryPayload)],
    namespace: &str,
    tier: Tier,
    generation: u64,
    edges: &[(String, String, String, Option<EdgeProvenance>)],
) -> Vec<super::types::Quad> {
    let mut out = Vec::new();
    for (id, payload) in rows {
        let qs = memory_to_quads(id, payload, namespace, tier, generation);
        out.extend(quads_to_legacy(&qs));
    }
    let graph = super::types_oxrdf::OxRdfBridge::tier_to_graph_name(&tier, namespace, generation);
    let graph = Some(graph);
    let ox_quads: Vec<Quad> = edges.iter().flat_map(|(from, rel, to, prov)| {
        edge_to_quads(from, rel, to, prov.clone(), graph.clone())
    }).collect();
    out.extend(quads_to_legacy(&ox_quads));
    out
}

/// Legacy projection path that derives each row's tier from its `memory_type`
/// via `Tier::from_memory_type`, projects rows group-by-group through
/// `rows_to_quads` (which converts via `quads_to_legacy`), and concatenates.
/// Edge quads are attached to the `default_tier` group's graph.
/// Unsupported memory types resolve to `Tier::Domain`; the rows here use
/// `default_tier` only to decide which tier group carries the edge graph.
pub fn project_rows_legacy(
    rows: &[(String, MemoryPayload)],
    edges: &[(String, String, String, Option<EdgeProvenance>)],
    namespace: &str,
    default_tier: Tier,
    generation: u64,
) -> Vec<super::types::Quad> {
    let mut by_tier: BTreeMap<Tier, Vec<(String, MemoryPayload)>> = BTreeMap::new();
    for (id, payload) in rows {
        by_tier
            .entry(Tier::from_memory_type(&payload.memory_type))
            .or_default()
            .push((id.clone(), payload.clone()));
    }

    let mut out = Vec::new();
    for (tier, tier_rows) in by_tier {
        let tier_edges: Vec<(String, String, String, Option<EdgeProvenance>)> =
            if tier == default_tier {
                edges.to_vec()
            } else {
                Vec::new()
            };
        out.extend(rows_to_quads(
            &tier_rows,
            namespace,
            tier,
            generation,
            &tier_edges,
        ));
    }
    // Bridge round-trip check: every legacy quad must convert back through
    // `OxRdfBridge::neurostrata_quad_to_oxrdf` without a parse error; a
    // failure means the legacy and oxrdf projections have drifted apart.
    // Logged (never failed) so the export still returns its full legacy set.
    for q in &out {
        if let Err(e) = super::types_oxrdf::OxRdfBridge::neurostrata_quad_to_oxrdf(q) {
            tracing::warn!(
                "project_rows_legacy: legacy quad failed oxrdf round-trip ({} {}): {}",
                q.subject,
                q.predicate,
                e
            );
        }
    }
    out
}

/// Convert a slice of oxrdf Quads to NeuroStrata legacy Quad type.
///
/// Every conversion routes through `OxRdfBridge::oxrdf_quad_to_neurostrata`
/// (single source of truth for the oxrdf→legacy mapping). Each OxRdfQuad is
/// also inserted into an `OxRdfDataset`: an insert that reports a duplicate
/// means the projection emitted the exact same quad twice, which is logged
/// but the legacy output order is untouched.
pub fn quads_to_legacy(quads: &[Quad]) -> Vec<super::types::Quad> {
    use super::types_oxrdf::{OxRdfBridge, OxRdfDataset, OxRdfQuad};

    let mut dataset = OxRdfDataset::new();
    let legacy: Vec<super::types::Quad> = quads
        .iter()
        .map(|q| {
            let wrap = OxRdfQuad::new(
                q.subject.clone(),
                q.predicate.clone(),
                q.object.clone(),
                q.graph_name.clone(),
            );
            let legacy = OxRdfBridge::oxrdf_quad_to_neurostrata(&wrap);
            if !dataset.insert(wrap) {
                tracing::warn!("duplicate RDF quad projected: {}", q);
            }
            legacy
        })
        .collect();
    if !dataset.is_empty() {
        tracing::debug!(
            "quads_to_legacy: {} unique of {} projected quads",
            dataset.len(),
            quads.len()
        );
    }
    legacy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;
    use oxrdf::Dataset;

    fn payload() -> MemoryPayload {
        MemoryPayload {
            content: "hello".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: Some("agent".to_string()),
            location: "src/lib.rs".to_string(),
            location_lines: "1-2".to_string(),
            metadata: json!({"domain": ["auth", "tls"], "facets": ["enforced-rule"], "source": "owner"}),
        }
    }

    #[test]
    fn memory_to_oxrdf_quads_core_fields() {
        let qs = memory_to_quads("m1", &payload(), "ns", Tier::Domain, 0);
        
        // Check that we got valid oxrdf quads
        let preds: Vec<String> = qs.iter().map(|q| q.predicate.as_str().to_string()).collect();
        assert!(preds.contains(&"urn:neurostrata:ns#content".to_string()));
        assert!(preds.contains(&"urn:neurostrata:ns#memory_type".to_string()));
        assert!(preds.contains(&"urn:neurostrata:ns#domain".to_string()));
        assert!(preds.contains(&"urn:neurostrata:ns#facet".to_string()));
        assert!(preds.contains(&"urn:neurostrata:ns#source".to_string()));
        // All quads should have the same graph
        assert!(qs.iter().all(|q| q.graph_name != GraphName::DefaultGraph));
    }

    #[test]
    fn memory_to_oxrdf_quads_insertable_into_dataset() {
        let qs = memory_to_quads("m1", &payload(), "ns", Tier::Domain, 0);
        let mut ds = Dataset::new();
        for q in &qs {
            ds.insert(q);
        }
        assert_eq!(ds.len(), qs.len());
        assert!(!ds.is_empty());
    }

    #[test]
    fn concept_iri_does_not_override_subject() {
        let mut p = payload();
        p.metadata = json!({"concept_iri": "urn:example:foo"});
        let qs = memory_to_quads("m1", &p, "ns", Tier::Domain, 0);
        
        // All subjects should be the memory subject
        let subj = format!("urn:neurostrata:memory:m1");
        assert!(qs.iter().all(|q| {
            match &q.subject {
                NamedOrBlankNode::NamedNode(nn) => nn.as_str() == subj,
                _ => false,
            }
        }));
        
        // Should have conceptIri as predicate with IRI object
        assert!(qs.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#conceptIri" &&
            matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:example:foo")
        }));
        
        // Two memories sharing one concept keep distinct subjects.
        let qs2 = memory_to_quads("m2", &p, "ns", Tier::Domain, 0);
        assert!(qs2.iter().all(|q| {
            match &q.subject {
                NamedOrBlankNode::NamedNode(nn) => nn.as_str() == format!("urn:neurostrata:memory:m2"),
                _ => false,
            }
        }));
    }

    #[test]
    fn edge_projects_relation() {
        let q = edge_to_quad("a", "GOVERNS", "b", None, Some(GraphName::DefaultGraph));
        assert_eq!(q.predicate, NamedNode::new_unchecked("urn:neurostrata:ns#governs"));
        assert!(matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:b"));
    }

    #[test]
    fn edge_provenance_quads_emitted_when_some() {
        let prov = EdgeProvenance {
            source: Some("owner".to_string()),
            valid_from: Some(1730000000),
            valid_to: None,
            generation: Some(1),
            extractor: Some("metadata".to_string()),
        };
        let none = edge_to_quads("a", "GOVERNS", "b", None, Some(GraphName::DefaultGraph));
        assert_eq!(none.len(), 1, "no provenance -> only the relation quad");

        let qs = edge_to_quads("a", "GOVERNS", "b", Some(prov), Some(GraphName::DefaultGraph));
        let preds: Vec<&str> = qs
            .iter()
            .skip(1)
            .map(|q| q.predicate.as_str().rsplit('#').next().unwrap())
            .collect();
        assert_eq!(
            preds,
            vec![
                "provenanceSource",
                "provenanceValidFrom",
                "provenanceGeneration",
                "provenanceExtractor"
            ]
        );
        assert!(qs.iter().any(|q| q.predicate.as_str().ends_with("#provenanceSource")
            && matches!(&q.object, Term::Literal(l) if l.value() == "owner")));
    }

    #[test]
    fn rows_to_oxrdf_quads_combines() {
        let rows = vec![("m1".to_string(), payload())];
        let edges = vec![("m1".to_string(), "GOVERNS".to_string(), "m2".to_string(), None)];
        let qs = rows_to_quads(&rows, "ns", Tier::Domain, 0, &edges);
        assert!(qs.iter().any(|q| q.predicate.as_str().ends_with("#governs")));
    }

    #[test]
    fn oxrdf_to_legacy_round_trip() {
        let qs = memory_to_quads("m1", &payload(), "ns", Tier::Domain, 0);
        let legacy = quads_to_legacy(&qs);
        
        assert_eq!(legacy.len(), qs.len());
        for (orig, conv) in qs.iter().zip(legacy.iter()) {
            let back = super::super::types_oxrdf::OxRdfBridge::neurostrata_quad_to_oxrdf(conv).unwrap();
            assert_eq!(orig, &back.inner);
        }
    }
}