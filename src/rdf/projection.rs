use crate::traits::MemoryPayload;
use super::namespace::{field_predicate, graph_iri, memory_iri, ns_predicate, relation_predicate};
use super::types::{EdgeProvenance, Quad, QuadObject, Tier};

/// Project a single Memory row into RDF quads.
pub fn memory_to_quads(
    id: &str,
    payload: &MemoryPayload,
    namespace: &str,
    tier: Tier,
    generation: u64,
) -> Vec<Quad> {
    let subject = memory_iri(id, payload.metadata.get("concept_iri").and_then(|v| v.as_str()));
    let graph = Some(graph_iri(namespace, tier, generation));

    let mut quads = Vec::new();

    // Core fields.
    quads.push(Quad {
        subject: subject.clone(),
        predicate: field_predicate("content"),
        object: QuadObject::Literal {
            value: payload.content.clone(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
            language: None,
        },
        graph: graph.clone(),
    });
    quads.push(Quad {
        subject: subject.clone(),
        predicate: field_predicate("user_id"),
        object: QuadObject::Literal {
            value: payload.user_id.clone(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
            language: None,
        },
        graph: graph.clone(),
    });
    quads.push(Quad {
        subject: subject.clone(),
        predicate: field_predicate("memory_type"),
        object: QuadObject::Literal {
            value: payload.memory_type.clone(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
            language: None,
        },
        graph: graph.clone(),
    });
    if let Some(agent) = &payload.agent_name {
        quads.push(Quad {
            subject: subject.clone(),
            predicate: field_predicate("agent_name"),
            object: QuadObject::Literal {
                value: agent.clone(),
                datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
                language: None,
            },
            graph: graph.clone(),
        });
    }
    quads.push(Quad {
        subject: subject.clone(),
        predicate: field_predicate("location"),
        object: QuadObject::Literal {
            value: payload.location.clone(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
            language: None,
        },
        graph: graph.clone(),
    });
    quads.push(Quad {
        subject: subject.clone(),
        predicate: field_predicate("location_lines"),
        object: QuadObject::Literal {
            value: payload.location_lines.clone(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
            language: None,
        },
        graph: graph.clone(),
    });

    // Metadata as a JSON literal (lossless round-trip).
    quads.push(Quad {
        subject: subject.clone(),
        predicate: field_predicate("metadata"),
        object: QuadObject::Literal {
            value: payload.metadata.to_string(),
            datatype: Some("https://www.w3.org/ns/ldp#JSON".into()),
            language: None,
        },
        graph: graph.clone(),
    });

    // Selected metadata sub-fields as first-class predicates.
    if let Some(domain) = payload.metadata.get("domain") {
        match domain {
            serde_json::Value::String(s) => {
                quads.push(Quad {
                    subject: subject.clone(),
                    predicate: ns_predicate("domain"),
                    object: QuadObject::Literal {
                        value: s.clone(),
                        datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
                        language: None,
                    },
                    graph: graph.clone(),
                });
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        quads.push(Quad {
                            subject: subject.clone(),
                            predicate: ns_predicate("domain"),
                            object: QuadObject::Literal {
                                value: s.to_string(),
                                datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
                                language: None,
                            },
                            graph: graph.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(facets) = payload.metadata.get("facets").and_then(|v| v.as_array()) {
        for f in facets {
            if let Some(s) = f.as_str() {
                quads.push(Quad {
                    subject: subject.clone(),
                    predicate: ns_predicate("facet"),
                    object: QuadObject::Literal {
                        value: s.to_string(),
                        datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
                        language: None,
                    },
                    graph: graph.clone(),
                });
            }
        }
    }
    if let Some(source) = payload.metadata.get("source") {
        quads.push(Quad {
            subject: subject.clone(),
            predicate: ns_predicate("source"),
            object: QuadObject::Literal {
                value: source.to_string(),
                datatype: Some("https://www.w3.org/ns/ldp#JSON".into()),
                language: None,
            },
            graph: graph.clone(),
        });
    }
    if let Some(topology) = payload.metadata.get("topology") {
        if let Some(obj) = topology.as_object() {
            for (k, v) in obj {
                quads.push(Quad {
                    subject: subject.clone(),
                    predicate: ns_predicate(&format!("topology_{}", k)),
                    object: QuadObject::Literal {
                        value: v.to_string(),
                        datatype: Some("https://www.w3.org/ns/ldp#JSON".into()),
                        language: None,
                    },
                    graph: graph.clone(),
                });
            }
        }
    }

    quads
}

/// Project one LadybugDB edge into an RDF quad.
pub fn edge_to_quad(
    from: &str,
    rel_type: &str,
    to: &str,
    _provenance: Option<EdgeProvenance>,
    graph: Option<String>,
) -> Quad {
    Quad {
        subject: format!("urn:neurostrata:memory:{}", from),
        predicate: relation_predicate(rel_type),
        object: QuadObject::Iri(format!("urn:neurostrata:memory:{}", to)),
        graph,
    }
}

/// Combine node + edge quads for a namespace snapshot.
pub fn rows_to_quads(
    rows: &[(String, MemoryPayload)],
    namespace: &str,
    tier: Tier,
    generation: u64,
    edges: &[(String, String, String, Option<EdgeProvenance>)],
) -> Vec<Quad> {
    let mut out = Vec::new();
    for (id, payload) in rows {
        out.extend(memory_to_quads(id, payload, namespace, tier, generation));
    }
    let graph = Some(graph_iri(namespace, tier, generation));
    for (from, rel, to, prov) in edges {
        out.push(edge_to_quad(from, rel, to, prov.clone(), graph.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    fn payload() -> MemoryPayload {
        MemoryPayload {
            content: "hello".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: Some("agent".into()),
            location: "src/lib.rs".into(),
            location_lines: "1-2".into(),
            metadata: json!({"domain": ["auth", "tls"], "facets": ["enforced-rule"], "source": "owner"}),
        }
    }

    #[test]
    fn memory_projects_core_fields() {
        let qs = memory_to_quads("m1", &payload(), "ns", Tier::Domain, 0);
        let preds: Vec<&str> = qs.iter().map(|q| q.predicate.as_str()).collect();
        assert!(preds.contains(&"urn:neurostrata:ns#content"));
        assert!(preds.contains(&"urn:neurostrata:ns#memory_type"));
        assert!(preds.contains(&"urn:neurostrata:ns#domain"));
        assert!(preds.contains(&"urn:neurostrata:ns#facet"));
        assert!(preds.contains(&"urn:neurostrata:ns#source"));
        assert!(qs.iter().all(|q| q.graph.is_some()));
    }

    #[test]
    fn concept_iri_overrides_subject() {
        let mut p = payload();
        p.metadata = json!({"concept_iri": "urn:example:foo"});
        let qs = memory_to_quads("m1", &p, "ns", Tier::Domain, 0);
        assert!(qs.iter().all(|q| q.subject == "urn:example:foo"));
    }

    #[test]
    fn edge_projects_relation() {
        let q = edge_to_quad("a", "GOVERNS", "b", None, Some("g".into()));
        assert_eq!(q.predicate, "urn:neurostrata:ns#governs");
        assert!(matches!(q.object, QuadObject::Iri(s) if s == "urn:neurostrata:memory:b"));
    }

    #[test]
    fn rows_to_quads_combines() {
        let rows = vec![("m1".to_string(), payload())];
        let edges = vec![("m1".to_string(), "GOVERNS".to_string(), "m2".to_string(), None)];
        let qs = rows_to_quads(&rows, "ns", Tier::Domain, 0, &edges);
        assert!(qs.iter().any(|q| q.predicate.ends_with("#governs")));
    }
}