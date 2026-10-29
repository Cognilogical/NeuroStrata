//! RDF projection: MemoryPayload → RDF quads (oxrdf-backed).
//! 
//! Phase 2 migration: uses oxrdf::Quad and oxrdf::Term instead of hand-written
//! Quad/QuadObject. The OxRdfBridge provides conversion to/from legacy types.

use crate::traits::MemoryPayload;
use super::namespace::{field_predicate, graph_iri, ns_predicate, relation_predicate};
use super::types::{EdgeProvenance, Tier};
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Literal, GraphName, Quad};

/// XSD string datatype IRI
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// JSON-LD JSON datatype IRI
const LDP_JSON: &str = "https://www.w3.org/ns/ldp#JSON";

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
                json_quad!(ns_predicate(&format!("topology_{}", k)), v.to_string(), ldp_json.clone());
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
    let graph_iri = format!("urn:neurostrata:graph:{}:{}:gen:{}",
        namespace, tier.as_str(), generation);
    let graph = GraphName::NamedNode(
        NamedNode::new_unchecked(graph_iri)
    );
    let ox_quads: Vec<Quad> = edges.iter().map(|(from, rel, to, prov)| {
        edge_to_quad(from, rel, to, prov.clone(), Some(graph.clone()))
    }).collect();
    out.extend(quads_to_legacy(&ox_quads));
    out
}

/// Convert a slice of oxrdf Quads to NeuroStrata legacy Quad type.
pub fn quads_to_legacy(quads: &[Quad]) -> Vec<super::types::Quad> {
    use super::types::{Quad as LegacyQuad, QuadObject};
    
    quads.iter().map(|q| {
        let subject = match &q.subject {
            NamedOrBlankNode::NamedNode(nn) => nn.to_string(),
            NamedOrBlankNode::BlankNode(bn) => format!("_:{}", bn),
        };
        let predicate = q.predicate.to_string();
        let object = match &q.object {
            Term::NamedNode(nn) => QuadObject::Iri(nn.to_string()),
            Term::Literal(lit) => QuadObject::Literal {
                value: lit.value().to_string(),
                datatype: Some(lit.datatype().to_string()),
                language: lit.language().map(|lang| lang.to_string()),
            },
            Term::BlankNode(bn) => QuadObject::Iri(format!("_:{}", bn)),
            Term::Triple(_) => QuadObject::Iri("urn:neurostrata:triple".to_string()),
        };
        let graph = match &q.graph_name {
            GraphName::NamedNode(gn) => Some(gn.to_string()),
            GraphName::BlankNode(bg) => Some(format!("_:{}", bg)),
            GraphName::DefaultGraph => None,
        };
        
        LegacyQuad { subject, predicate, object, graph }
    }).collect()
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
                NamedOrBlankNode::NamedNode(nn) => nn.to_string() == subj,
                _ => false,
            }
        }));
        
        // Should have conceptIri as predicate with IRI object
        assert!(qs.iter().any(|q| {
            q.predicate.to_string() == "urn:neurostrata:ns#conceptIri" &&
            matches!(&q.object, Term::NamedNode(nn) if nn.to_string() == "urn:example:foo")
        }));
        
        // Two memories sharing one concept keep distinct subjects.
        let qs2 = memory_to_quads("m2", &p, "ns", Tier::Domain, 0);
        assert!(qs2.iter().all(|q| {
            match &q.subject {
                NamedOrBlankNode::NamedNode(nn) => nn.to_string() == format!("urn:neurostrata:memory:m2"),
                _ => false,
            }
        }));
    }

    #[test]
    fn edge_projects_relation() {
        let q = edge_to_quad("a", "GOVERNS", "b", None, Some(GraphName::DefaultGraph));
        assert_eq!(q.predicate, NamedNode::new_unchecked("urn:neurostrata:ns#governs"));
        assert!(matches!(&q.object, Term::NamedNode(nn) if nn.to_string() == "urn:neurostrata:memory:b"));
    }

    #[test]
    fn rows_to_oxrdf_quads_combines() {
        let rows = vec![("m1".to_string(), payload())];
        let edges = vec![("m1".to_string(), "GOVERNS".to_string(), "m2".to_string(), None)];
        let qs = rows_to_quads(&rows, "ns", Tier::Domain, 0, &edges);
        assert!(qs.iter().any(|q| q.predicate.to_string().ends_with("#governs")));
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