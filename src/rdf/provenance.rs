use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use serde::{Deserialize, Serialize};

pub const ALLOWED_ORIGINS: [&str; 4] = ["user", "agent", "system", "mesh"];
pub const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
pub const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
pub const PROV_WAS_INVALIDATED_BY: &str = "http://www.w3.org/ns/prov#wasInvalidatedBy";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeProvenance {
    pub source: Option<String>,
    pub extractor: String,
    pub origin: String,
    pub confidence: f64,
    pub valid_from: i64,
    pub valid_to: i64,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceWarning {
    pub target: String,
    pub field: String,
    pub message: String,
}

pub fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => {
                out.push(ch);
            }
            _ => {
                for b in ch.encode_utf8(&mut [0; 4]).as_bytes() {
                    out.push('%');
                    out.push_str(&format!("{:02X}", b));
                }
            }
        }
    }
    out
}

pub fn memory_assertion_iri(
    namespace: &str,
    memory_id: &str,
    predicate_slug: &str,
    object_discriminator: usize,
    assertion_version: i64,
) -> String {
    format!(
        "urn:neurostrata:assertion:{}:memory:{}:{}:{}:{}",
        percent_encode_segment(namespace),
        percent_encode_segment(memory_id),
        percent_encode_segment(predicate_slug),
        object_discriminator,
        assertion_version
    )
}

pub fn edge_assertion_iri(
    namespace: &str,
    relation: &str,
    from_id: &str,
    to_id: &str,
    generation: i64,
    valid_from: i64,
    ordinal: Option<usize>,
) -> String {
    let base = format!(
        "urn:neurostrata:assertion:{}:edge:{}:{}:{}:g{}:v{}",
        percent_encode_segment(namespace),
        percent_encode_segment(relation),
        percent_encode_segment(from_id),
        percent_encode_segment(to_id),
        generation,
        valid_from
    );
    match ordinal {
        Some(ord) => format!("{}:{}", base, ord),
        None => base,
    }
}

pub fn extract_node_provenance(metadata: &serde_json::Value, default_generation: i64) -> NodeProvenance {
    let prov_obj = metadata.get("provenance").and_then(|v| v.as_object());
    let source = if let Some(p) = prov_obj {
        p.get("source").and_then(|v| v.as_str()).map(String::from)
    } else {
        None
    };
    let source_fallback = if source.is_none() {
        metadata.get("source").and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(String::from(s))
            } else if let Some(obj) = v.as_object() {
                if obj.is_empty() {
                    // Nothing readable at all: leave `source` unset rather
                    // than inventing a blank-but-present source string.
                    None
                } else {
                    let kind = obj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    let r = obj.get("ref").and_then(|v| v.as_str()).unwrap_or("");
                    let cap = obj.get("captured_at").and_then(|v| v.as_str()).unwrap_or("");
                    Some(if kind.is_empty() && r.is_empty() && cap.is_empty() {
                        // Structured source without the usual {kind, ref,
                        // captured_at} shape: serialize the object instead of
                        // emitting the meaningless "::" triple-empty form.
                        serde_json::to_string(obj).unwrap_or_default()
                    } else {
                        format!("{}:{}:{}", kind, r, cap)
                    })
                }
            } else {
                Some(v.to_string())
            }
        })
    } else {
        None
    };
    let source_final = source.or(source_fallback);
    let extractor = prov_obj
        .and_then(|p| p.get("extractor"))
        .and_then(|v| v.as_str())
        .unwrap_or("manual")
        .to_string();
    let origin = prov_obj
        .and_then(|p| p.get("origin"))
        .and_then(|v| v.as_str())
        .unwrap_or("system")
        .to_string();
    let confidence = prov_obj
        .and_then(|p| p.get("confidence"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let valid_from = metadata
        .get("valid_from")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let valid_to = metadata
        .get("valid_to")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let generation = prov_obj
        .and_then(|p| p.get("generation"))
        .and_then(|v| v.as_i64())
        .or_else(|| metadata.get("generation").and_then(|v| v.as_i64()))
        .unwrap_or(default_generation);
    NodeProvenance {
        source: source_final,
        extractor,
        origin,
        confidence,
        valid_from,
        valid_to,
        generation,
    }
}

pub fn validate_node_provenance(target: &str, prov: &NodeProvenance) -> Vec<ProvenanceWarning> {
    let mut warnings = Vec::new();
    if !prov.confidence.is_finite() || prov.confidence < 0.0 || prov.confidence > 1.0 {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "confidence".to_string(),
            message: format!("confidence {} not in [0,1]", prov.confidence),
        });
    }
    if !ALLOWED_ORIGINS.contains(&prov.origin.as_str()) {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "origin".to_string(),
            message: format!("origin '{}' not allowed", prov.origin),
        });
    }
    if prov.valid_to != 0 && prov.valid_to < prov.valid_from {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "valid_to".to_string(),
            message: "valid_to < valid_from".to_string(),
        });
    }
    if prov.source.as_ref().map_or(false, |s| s.trim().is_empty()) {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "source".to_string(),
            message: "source is empty".to_string(),
        });
    }
    if prov.extractor.trim().is_empty() {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "extractor".to_string(),
            message: "extractor is empty".to_string(),
        });
    }
    warnings
}

pub fn validate_assertion_iri(iri: &str) -> bool {
    // Full shape: urn : neurostrata : assertion : ns : (memory|edge) : ...
    // Anchor by position so a namespace or ID named "memory" or "edge"
    // elsewhere in the IRI cannot confuse the validator.
    let parts: Vec<&str> = iri.split(':').collect();
    if parts.get(0) != Some(&"urn")
        || parts.get(1) != Some(&"neurostrata")
        || parts.get(2) != Some(&"assertion")
    {
        return false;
    }
    let tail = &parts[3..];
    let has_g = tail.iter().any(|p| p.starts_with('g') && p.len() > 1);
    let has_v = tail.iter().any(|p| p.starts_with('v') && p.len() > 1);
    if tail.get(1) == Some(&"memory") {
        // Expected: ns, "memory", id, slug, disc, version => 6 after prefix,
        // spec allows 7 (tolerance)
        return tail.len() >= 6;
    }
    if tail.get(1) == Some(&"edge") {
        // Expected: ns, "edge", rel, from, to, g..., v... => 6+ after prefix
        return tail.len() >= 6 && has_g && has_v;
    }
    false
}

pub fn project_memory_rdf_star(
    namespace: &str,
    id: &str,
    payload: &crate::traits::MemoryPayload,
    tier: crate::rdf::types::Tier,
    generation: u64,
) -> (Vec<oxrdf::Quad>, Vec<ProvenanceWarning>) {
    use crate::rdf::namespace::{field_predicate, graph_iri, memory_iri, ns_predicate};
    let s = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(memory_iri(id, None)));
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(graph_iri(
        namespace, tier, generation,
    )));
    let xsd_str = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#string".to_string());
    let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer".to_string());
    let xsd_dbl = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#double".to_string());
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string());
    let rdf_reifies = NamedNode::new_unchecked(RDF_REIFIES.to_string());

    let mut quads = Vec::new();
    let mut base = Vec::new();

    let push_str_quad = |base: &mut Vec<Quad>, pred: &str, val: String| {
        base.push(Quad {
            subject: s.clone(),
            predicate: NamedNode::new_unchecked(pred.to_string()),
            object: Term::Literal(Literal::new_typed_literal(val, xsd_str.clone())),
            graph_name: graph.clone(),
        });
    };

    push_str_quad(&mut base, &field_predicate("content"), payload.content.clone());
    push_str_quad(&mut base, &field_predicate("user_id"), payload.user_id.clone());
    push_str_quad(&mut base, &field_predicate("memory_type"), payload.memory_type.clone());
    if let Some(agent) = &payload.agent_name {
        push_str_quad(&mut base, &field_predicate("agent_name"), agent.clone());
    }

    // domain
    if let Some(domain) = payload.metadata.get("domain") {
        match domain {
            serde_json::Value::String(sval) => {
                push_str_quad(&mut base, &ns_predicate("domain"), sval.clone());
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    if let Some(sval) = v.as_str() {
                        push_str_quad(&mut base, &ns_predicate("domain"), sval.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    // conceptIri
    if let Some(concept) = payload.metadata.get("concept_iri") {
        let iris: Vec<String> = match concept {
            serde_json::Value::String(sval) => vec![sval.clone()],
            serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        for iri in iris {
            if !iri.trim().is_empty() {
                base.push(Quad {
                    subject: s.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("conceptIri")),
                    object: Term::NamedNode(NamedNode::new_unchecked(iri)),
                    graph_name: graph.clone(),
                });
            }
        }
    }
    // facet
    if let Some(facets) = payload.metadata.get("facets").and_then(|v| v.as_array()) {
        for f in facets {
            if let Some(sval) = f.as_str() {
                push_str_quad(&mut base, &ns_predicate("facet"), sval.to_string());
            }
        }
    }
    // supersedes / superseded_by provenance relations (not opaque JSON)
    if let Some(sup) = payload.metadata.get("supersedes") {
        if let Some(sval) = sup.as_str() {
            base.push(Quad {
                subject: s.clone(),
                predicate: NamedNode::new_unchecked(PROV_WAS_DERIVED_FROM.to_string()),
                object: Term::NamedNode(NamedNode::new_unchecked(sval.to_string())),
                graph_name: graph.clone(),
            });
        }
    }
    if let Some(sup_by) = payload.metadata.get("superseded_by") {
        if let Some(sval) = sup_by.as_str() {
            base.push(Quad {
                subject: s.clone(),
                predicate: NamedNode::new_unchecked(PROV_WAS_INVALIDATED_BY.to_string()),
                object: Term::NamedNode(NamedNode::new_unchecked(sval.to_string())),
                graph_name: graph.clone(),
            });
        }
    }

    // rdf:type Memory
    base.push(Quad {
        subject: s.clone(),
        predicate: rdf_type.clone(),
        object: Term::NamedNode(NamedNode::new_unchecked("urn:neurostrata:Memory".to_string())),
        graph_name: graph.clone(),
    });

    let prov = extract_node_provenance(&payload.metadata, generation as i64);
    let warnings = validate_node_provenance(&format!("urn:neurostrata:memory:{}", id), &prov);

    let assertion_iri = memory_assertion_iri(namespace, id, "memory", 0, prov.generation);
    let assertion_subj = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(assertion_iri.clone()));

    for b in &base {
        // emit base
        quads.push(b.clone());
        // assertion block for this base quad
        let triple_term = Term::Triple(Box::new(Triple::new(
            b.subject.clone(),
            b.predicate.clone(),
            b.object.clone(),
        )));
        // rdf:type Assertion
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: rdf_type.clone(),
            object: Term::NamedNode(NamedNode::new_unchecked("urn:neurostrata:ns#Assertion".to_string())),
            graph_name: b.graph_name.clone(),
        });
        // rdf:reifies << ( s p o ) >>
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: rdf_reifies.clone(),
            object: triple_term,
            graph_name: b.graph_name.clone(),
        });
        // provenance fields
        if let Some(src) = &prov.source {
            if !src.trim().is_empty() {
                quads.push(Quad {
                    subject: assertion_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("source")),
                    object: Term::Literal(Literal::new_typed_literal(src.clone(), xsd_str.clone())),
                    graph_name: b.graph_name.clone(),
                });
            }
        }
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("extractor")),
            object: Term::Literal(Literal::new_typed_literal(prov.extractor.clone(), xsd_str.clone())),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("origin")),
            object: Term::Literal(Literal::new_typed_literal(prov.origin.clone(), xsd_str.clone())),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("confidence")),
            object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.confidence), xsd_dbl.clone())),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("validFrom")),
            object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.valid_from), xsd_int.clone())),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("validTo")),
            object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.valid_to), xsd_int.clone())),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("generation")),
            object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.generation), xsd_int.clone())),
            graph_name: b.graph_name.clone(),
        });
    }
    (quads, warnings)
}

pub fn project_edge_rdf_star(
    namespace: &str,
    from: &str,
    relation: &str,
    to: &str,
    prov: &NodeProvenance,
    ordinal: Option<usize>,
    graph_name: oxrdf::GraphName,
) -> (Vec<oxrdf::Quad>, Vec<ProvenanceWarning>) {
    use crate::rdf::namespace::relation_predicate;
    let s = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        from
    )));
    let o_named = NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        to
    ));
    let o_term = Term::NamedNode(o_named.clone());
    let xsd_str = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#string".to_string());
    let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer".to_string());
    let xsd_dbl = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#double".to_string());
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string());
    let rdf_reifies = NamedNode::new_unchecked(RDF_REIFIES.to_string());
    let ns_predicate = |n: &str| NamedNode::new_unchecked(format!("urn:neurostrata:ns#{}", n));

    let base = Quad {
        subject: s.clone(),
        predicate: NamedNode::new_unchecked(relation_predicate(relation)),
        object: o_term.clone(),
        graph_name: graph_name.clone(),
    };

    let assertions = edge_assertion_iri(namespace, relation, from, to, prov.generation, prov.valid_from, ordinal);
    let assertion_subj = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(assertions));

    let mut quads = Vec::new();
    quads.push(base.clone());

    // assertion block
    let triple_term = Term::Triple(Box::new(Triple::new(s.clone(), base.predicate.clone(), o_term.clone())));
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: rdf_type.clone(),
        object: Term::NamedNode(NamedNode::new_unchecked("urn:neurostrata:ns#Assertion".to_string())),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: rdf_reifies.clone(),
        object: triple_term,
        graph_name: graph_name.clone(),
    });
    if let Some(src) = &prov.source {
        if !src.trim().is_empty() {
            quads.push(Quad {
                subject: assertion_subj.clone(),
                predicate: ns_predicate("source"),
                object: Term::Literal(Literal::new_typed_literal(src.clone(), xsd_str.clone())),
                graph_name: graph_name.clone(),
            });
        }
    }
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("extractor"),
        object: Term::Literal(Literal::new_typed_literal(prov.extractor.clone(), xsd_str.clone())),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("origin"),
        object: Term::Literal(Literal::new_typed_literal(prov.origin.clone(), xsd_str.clone())),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("confidence"),
        object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.confidence), xsd_dbl.clone())),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("validFrom"),
        object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.valid_from), xsd_int.clone())),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("validTo"),
        object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.valid_to), xsd_int.clone())),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("generation"),
        object: Term::Literal(Literal::new_typed_literal(format!("{}", prov.generation), xsd_int.clone())),
        graph_name: graph_name.clone(),
    });

    let warnings = validate_node_provenance(&format!("urn:neurostrata:edge:{}:{}:{}", namespace, relation, from), prov);
    (quads, warnings)
}

pub fn serialize_rdf_star(quads: &[oxrdf::Quad], format: &str, flatten_graphs: bool) -> Result<String, String> {
    if !matches!(
        format,
        "turtle-star" | "trig-star" | "n-triples-star" | "n-quads-star"
    ) {
        return Err(format!("unsupported RDF-star format: {format}"));
    }

    let mut sorted: Vec<&oxrdf::Quad> = quads.iter().collect();
    sorted.sort_by(|a, b| {
        let a_g = a.graph_name.to_string();
        let b_g = b.graph_name.to_string();
        let a_s = a.subject.to_string();
        let b_s = b.subject.to_string();
        let a_p = a.predicate.to_string();
        let b_p = b.predicate.to_string();
        let a_o = a.object.to_string();
        let b_o = b.object.to_string();
        (a_g, a_s, a_p, a_o).cmp(&(b_g, b_s, b_p, b_o))
    });

    // Use TurtleSerializer for triple formats, TriG/NQuads for quad formats.
    let is_triple_format = format == "turtle-star"
        || (flatten_graphs && format != "trig-star" && format != "n-triples-star" && format != "n-quads-star");
    let is_trig = format == "trig-star" || (!flatten_graphs && format != "n-quads-star" && format != "n-triples-star" && format != "turtle-star");

    if format == "n-triples-star" || (flatten_graphs && is_triple_format && format != "turtle-star") {
        // NTriplesSerializer over triples
        let mut serializer = oxttl::NTriplesSerializer::new().for_writer(Vec::new());
        for q in sorted {
            let t = Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone());
            serializer.serialize_triple(t.as_ref()).map_err(|e| format!("serialize: {}", e))?;
        }
        let bytes = serializer.finish();
        return String::from_utf8(bytes).map_err(|_| "utf8".to_string());
    }

    if format == "turtle-star" || (flatten_graphs && is_triple_format) {
        let mut serializer = oxttl::TurtleSerializer::new().for_writer(Vec::new());
        for q in sorted {
            let t = Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone());
            serializer.serialize_triple(t.as_ref()).map_err(|e| format!("serialize: {}", e))?;
        }
        let bytes = serializer.finish().map_err(|e| format!("finish: {}", e))?;
        return String::from_utf8(bytes).map_err(|_| "utf8".to_string());
    }

    if format == "trig-star" || is_trig {
        let mut serializer = oxttl::TriGSerializer::new().for_writer(Vec::new());
        for q in sorted {
            serializer.serialize_quad(q.as_ref()).map_err(|e| format!("serialize: {}", e))?;
        }
        let bytes = serializer.finish().map_err(|e| format!("finish: {}", e))?;
        return String::from_utf8(bytes).map_err(|_| "utf8".to_string());
    }

    // Any remaining format must be `n-quads-star`: every other combination is
    // handled by the branches above (n-triples-star/turtle-star via
    // `is_triple_format` when flattening, every non-quad format via `is_trig`
    // when graphs are preserved).
    let mut serializer = oxttl::NQuadsSerializer::new().for_writer(Vec::new());
    for q in sorted {
        // `flatten_graphs` also applies to n-quads-star: strip the graph name
        // so every quad lands in the default graph.
        let graph_name = if flatten_graphs {
            GraphName::DefaultGraph
        } else {
            q.graph_name.clone()
        };
        let flattened = Quad {
            subject: q.subject.clone(),
            predicate: q.predicate.clone(),
            object: q.object.clone(),
            graph_name,
        };
        serializer.serialize_quad(flattened.as_ref()).map_err(|e| format!("serialize: {}", e))?;
    }
    let bytes = serializer.finish();
    String::from_utf8(bytes).map_err(|_| "utf8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;
    use oxrdf::{GraphName, NamedNode};

    fn sample_payload() -> MemoryPayload {
        MemoryPayload {
            content: "test content".to_string(),
            user_id: "u1".to_string(),
            memory_type: "fact".to_string(),
            agent_name: Some("agent".to_string()),
            location: "a.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: json!({"source":"manual","domain":"d","facets":["f"]}),
        }
    }

    #[test]
    fn memory_assertion_iri_format() {
        let iri = memory_assertion_iri("ns", "m1", "memory", 0, 3);
        assert!(iri.starts_with("urn:neurostrata:assertion:ns:memory:m1:memory:0:3"));
    }

    #[test]
    fn edge_assertion_iri_format() {
        let iri = edge_assertion_iri("ns", "GOVERNS", "a", "b", 2, 10, Some(1));
        assert!(iri.contains(":edge:GOVERNS:a:b:g2:v10:1"));
        let iri2 = edge_assertion_iri("ns", "RELATES_TO", "x", "y", 1, 5, None);
        assert!(iri2.contains(":edge:RELATES_TO:x:y:g1:v5") && !iri2.contains(":g1:v5:1"));
    }

    #[test]
    fn percent_encode_segment_escapes() {
        assert_eq!(percent_encode_segment("a/b"), "a%2Fb");
        assert_eq!(percent_encode_segment("hello world"), "hello%20world");
        assert_eq!(percent_encode_segment("%"), "%25");
        assert_eq!(percent_encode_segment(":"), "%3A");
    }

    #[test]
    fn validate_node_provenance_warn_first() {
        let bad = NodeProvenance {
            source: Some("".to_string()),
            extractor: "".to_string(),
            origin: "bad".to_string(),
            confidence: 1.5,
            valid_from: 10,
            valid_to: 5,
            generation: 1,
        };
        let w = validate_node_provenance("t", &bad);
        assert!(!w.is_empty());
    }

    #[test]
    fn validate_assertion_iri_memory_and_edge() {
        assert!(validate_assertion_iri("urn:neurostrata:assertion:ns:memory:m:sl:0:1"));
        assert!(validate_assertion_iri("urn:neurostrata:assertion:ns:edge:r:a:b:g1:v0"));
        assert!(!validate_assertion_iri("nope"));
    }

    #[test]
    fn project_memory_rdf_star_emits_base_and_assertion() {
        let (qs, _w) = project_memory_rdf_star("ns", "m1", &sample_payload(), crate::rdf::types::Tier::Domain, 1);
        assert!(!qs.is_empty());
        assert!(qs.iter().any(|q| q.predicate.as_str().contains("content")));
        assert!(qs.iter().any(|q| q.object.to_string().contains("Assertion") || q.predicate.as_str().contains("type") && q.object.to_string().contains("Assertion")));
    }

    #[test]
    fn project_edge_rdf_star_emits_base_and_assertion() {
        let prov = NodeProvenance {
            source: Some("src".to_string()),
            extractor: "e".to_string(),
            origin: "system".to_string(),
            confidence: 0.9,
            valid_from: 1,
            valid_to: 0,
            generation: 1,
        };
        let (qs, _w) = project_edge_rdf_star("ns", "a", "GOVERNS", "b", &prov, None, GraphName::DefaultGraph);
        assert!(!qs.is_empty());
    }

    #[test]
    fn serialize_rdf_star_formats() {
        let q = oxrdf::Quad {
            subject: NamedOrBlankNode::NamedNode(NamedNode::new_unchecked("urn:test:s".to_string())),
            predicate: NamedNode::new_unchecked("urn:test:p".to_string()),
            object: Term::Literal(Literal::new_simple_literal("hello")),
            graph_name: GraphName::DefaultGraph,
        };
        let res_t = serialize_rdf_star(&[q.clone()], "turtle-star", true);
        assert!(res_t.is_ok());
        let res_n = serialize_rdf_star(&[q.clone()], "n-triples-star", true);
        assert!(res_n.is_ok());
        let res_trig = serialize_rdf_star(&[q.clone()], "trig-star", false);
        assert!(res_trig.is_ok());
        let res_nq = serialize_rdf_star(&[q.clone()], "n-quads-star", false);
        assert!(res_nq.is_ok());
    }
}
