//! RDF projection: MemoryPayload → RDF quads (oxrdf-backed).
//!
//! Phase 2 migration: uses oxrdf::Quad and oxrdf::Term instead of hand-written
//! Quad/QuadObject. The OxRdfBridge provides conversion to/from legacy types.

use super::namespace::{field_predicate, ns_predicate, relation_predicate};
use super::types::{EdgeProvenance, Tier};
use crate::traits::MemoryPayload;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use std::collections::BTreeMap;

/// XSD string datatype IRI
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// JSON-LD JSON datatype IRI
const LDP_JSON: &str = "https://www.w3.org/ns/ldp#JSON";

/// Sanitize a dynamic metadata key for use as an IRI suffix so
/// `NamedNode::new_unchecked` can never panic on spaces or `<`/`>`.
fn sanitize_iri_suffix(k: &str) -> String {
    k.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
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
    let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        id
    )));
    let graph_iri = format!(
        "urn:neurostrata:graph:{}:{}:gen:{}",
        namespace,
        tier.as_str(),
        generation
    );
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(graph_iri.clone()));

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
    lit_quad!(
        field_predicate("content"),
        payload.content.clone(),
        xsd_string.clone()
    );
    lit_quad!(
        field_predicate("user_id"),
        payload.user_id.clone(),
        xsd_string.clone()
    );
    lit_quad!(
        field_predicate("memory_type"),
        payload.memory_type.clone(),
        xsd_string.clone()
    );

    if let Some(agent) = &payload.agent_name {
        lit_quad!(
            field_predicate("agent_name"),
            agent.clone(),
            xsd_string.clone()
        );
    }

    lit_quad!(
        field_predicate("location"),
        payload.location.clone(),
        xsd_string.clone()
    );
    lit_quad!(
        field_predicate("location_lines"),
        payload.location_lines.clone(),
        xsd_string.clone()
    );

    // Metadata as a JSON literal (lossless round-trip).
    json_quad!(
        field_predicate("metadata"),
        payload.metadata.to_string(),
        ldp_json.clone()
    );

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
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => vec![],
        };
        for iri in iris {
            if iri.trim().is_empty() {
                continue;
            }
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
    if payload.memory_type == "action_receipt" {
        lit_quad!(
            ns_predicate("eligibility"),
            "private".to_string(),
            xsd_string.clone()
        );
    } else if let Some(elig) = payload.metadata.get("eligibility").and_then(|v| v.as_str()) {
        lit_quad!(
            ns_predicate("eligibility"),
            elig.to_string(),
            xsd_string.clone()
        );
    }
    if let Some(topology) = payload.metadata.get("topology") {
        if let Some(obj) = topology.as_object() {
            for (k, v) in obj {
                json_quad!(
                    ns_predicate(&format!("topology_{}", sanitize_iri_suffix(k))),
                    v.to_string(),
                    ldp_json.clone()
                );
            }
        }
    }

    // ReBAC visible_to projection: `<mem_iri> ns:visibleTo "<principal>:<role>"`
    if let Some(vt) = payload
        .metadata
        .get("visible_to")
        .and_then(|v| v.as_array())
    {
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

    // Digital twin schema RDF projection (interfaces -> owl:Class + rdfs:subClassOf)
    if payload.memory_type == "digital_twin_schema" || payload.metadata.get("twin_schema").is_some()
    {
        if let Some(schema) = crate::digital_twin::TwinSchema::from_metadata(&payload.metadata) {
            if schema.kind == "interface" {
                let slug = schema
                    .interface_slug
                    .or(schema.schema_id)
                    .unwrap_or_else(|| id.to_string());
                let iface_iri = format!("urn:neurostrata:interface:{}", sanitize_iri_suffix(&slug));
                let iface_subj = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(iface_iri));
                quads.push(Quad {
                    subject: iface_subj.clone(),
                    predicate: NamedNode::new_unchecked(
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                    ),
                    object: Term::NamedNode(NamedNode::new_unchecked(
                        "http://www.w3.org/2002/07/owl#Class",
                    )),
                    graph_name: graph.clone(),
                });
                for parent in &schema.implements_interfaces {
                    let parent_iri =
                        format!("urn:neurostrata:interface:{}", sanitize_iri_suffix(parent));
                    quads.push(Quad {
                        subject: iface_subj.clone(),
                        predicate: NamedNode::new_unchecked(
                            "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                        ),
                        object: Term::NamedNode(NamedNode::new_unchecked(parent_iri)),
                        graph_name: graph.clone(),
                    });
                }
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
            let xsd_bool = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#boolean");
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked(
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                ),
                object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("TwinState"))),
                graph_name: graph.clone(),
            });
            if let Some(twin_id) = dt
                .get("twin_id")
                .or_else(|| dt.get("id"))
                .and_then(|v| v.as_str())
            {
                lit_quad!(
                    ns_predicate("twinId"),
                    twin_id.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(obs) = dt
                .get("observed_at")
                .and_then(|v| v.as_i64())
                .or_else(|| payload.metadata.get("valid_from").and_then(|v| v.as_i64()))
            {
                lit_quad!(ns_predicate("observedAt"), obs.to_string(), xsd_int.clone());
            }
            if let Some(props) = dt
                .get("properties")
                .or_else(|| dt.get("variables"))
                .and_then(|v| v.as_object())
            {
                for (k, v) in props {
                    let sfx = sanitize_iri_suffix(k);
                    json_quad!(
                        ns_predicate(&format!("twinProp_{sfx}")),
                        v.to_string(),
                        ldp_json.clone()
                    );
                    if let Some(f) = v.as_f64().filter(|n| n.is_finite()) {
                        quads.push(Quad {
                            subject: subject.clone(),
                            predicate: NamedNode::new_unchecked(ns_predicate(&format!(
                                "twinVar_{sfx}"
                            ))),
                            object: Term::Literal(Literal::from(f)),
                            graph_name: graph.clone(),
                        });
                    }
                }
            }
            if let Some(attrs) = dt.get("attributes").and_then(|v| v.as_object()) {
                for (k, v) in attrs {
                    let pred = ns_predicate(&format!("twinAttr_{}", sanitize_iri_suffix(k)));
                    if let Some(b) = v.as_bool() {
                        lit_quad!(pred, b.to_string(), xsd_bool.clone());
                    } else if let Some(i) = v.as_i64() {
                        lit_quad!(pred, i.to_string(), xsd_int.clone());
                    } else if let Some(s) = v.as_str() {
                        lit_quad!(pred, s.to_string(), xsd_string.clone());
                    }
                }
            }
            if let Some(derived) = dt.get("derived_variables").and_then(|v| v.as_object()) {
                for (k, v) in derived {
                    json_quad!(
                        ns_predicate(&format!("twinDerived_{}", sanitize_iri_suffix(k))),
                        v.to_string(),
                        ldp_json.clone()
                    );
                }
            }
            if let Some(ifaces) = dt.get("implements_interfaces").and_then(|v| v.as_array()) {
                for iface in ifaces.iter().filter_map(|v| v.as_str()) {
                    let iface_iri =
                        format!("urn:neurostrata:interface:{}", sanitize_iri_suffix(iface));
                    quads.push(Quad {
                        subject: subject.clone(),
                        predicate: NamedNode::new_unchecked(
                            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                        ),
                        object: Term::NamedNode(NamedNode::new_unchecked(iface_iri)),
                        graph_name: graph.clone(),
                    });
                }
            }
        }
    }

    // Kinetic ActionType RDF projection
    if payload.memory_type == "action_type" {
        quads.push(Quad {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("ActionType"))),
            graph_name: graph.clone(),
        });
        if let Some(at) = payload
            .metadata
            .get("action_type")
            .and_then(|v| v.as_object())
        {
            if let Some(slug) = at.get("slug").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("actionSlug"),
                    slug.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(title) = at.get("title").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("actionTitle"),
                    title.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(desc) = at.get("description").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("actionDescription"),
                    desc.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(role) = at.get("required_role").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("requiredRole"),
                    role.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(params) = at.get("parameters").and_then(|v| v.as_object()) {
                for param_name in params.keys() {
                    lit_quad!(
                        ns_predicate("actionParam"),
                        param_name.clone(),
                        xsd_string.clone()
                    );
                }
            }
        }
    }

    // Kinetic ActionReceipt RDF projection (receipt params/body values are never projected as public predicates)
    if payload.memory_type == "action_receipt" {
        let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer");
        quads.push(Quad {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("ActionReceipt"))),
            graph_name: graph.clone(),
        });
        if let Some(ar) = payload
            .metadata
            .get("action_receipt")
            .and_then(|v| v.as_object())
        {
            if let Some(slug) = ar.get("action_slug").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("actionSlug"),
                    slug.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(principal) = ar.get("principal").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("principal"),
                    principal.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(exec_at) = ar.get("executed_at").and_then(|v| v.as_i64()) {
                lit_quad!(
                    ns_predicate("executedAt"),
                    exec_at.to_string(),
                    xsd_int.clone()
                );
            }
            if let Some(rev_at) = ar.get("reverted_at").and_then(|v| v.as_i64()) {
                lit_quad!(
                    ns_predicate("revertedAt"),
                    rev_at.to_string(),
                    xsd_int.clone()
                );
            }
            if let Some(rev_by) = ar.get("reverted_by_receipt_id").and_then(|v| v.as_str()) {
                if !rev_by.trim().is_empty() {
                    iri_quad!(
                        ns_predicate("revertedByReceipt"),
                        super::namespace::memory_iri(rev_by, None)
                    );
                }
            }
            if let Some(reverts) = ar.get("reverts_receipt_id").and_then(|v| v.as_str()) {
                if !reverts.trim().is_empty() {
                    iri_quad!(
                        ns_predicate("revertsReceipt"),
                        super::namespace::memory_iri(reverts, None)
                    );
                }
            }
            if let Some(forced_by) = ar.get("forced_by").and_then(|v| v.as_str()) {
                if !forced_by.trim().is_empty() {
                    lit_quad!(
                        ns_predicate("forcedBy"),
                        forced_by.to_string(),
                        xsd_string.clone()
                    );
                }
            }
            if let Some(forced_at) = ar.get("forced_at").and_then(|v| v.as_i64()) {
                lit_quad!(
                    ns_predicate("forcedAt"),
                    forced_at.to_string(),
                    xsd_int.clone()
                );
            }
            if let Some(pairs) = ar.get("superseded_pairs").and_then(|v| v.as_array()) {
                for pair in pairs {
                    if let Some(before_id) = pair.get("before_id").and_then(|v| v.as_str()) {
                        if !before_id.trim().is_empty() {
                            iri_quad!(
                                ns_predicate("supersededMemory"),
                                super::namespace::memory_iri(before_id, None)
                            );
                        }
                    }
                    if let Some(after_id) = pair.get("after_id").and_then(|v| v.as_str()) {
                        if !after_id.trim().is_empty() {
                            iri_quad!(
                                ns_predicate("replacementMemory"),
                                super::namespace::memory_iri(after_id, None)
                            );
                        }
                    }
                }
            }
            if let Some(created) = ar.get("created_ids").and_then(|v| v.as_array()) {
                for cid in created.iter().filter_map(|v| v.as_str()) {
                    if !cid.trim().is_empty() {
                        iri_quad!(
                            ns_predicate("createdMemory"),
                            super::namespace::memory_iri(cid, None)
                        );
                    }
                }
            }
            if let Some(twins) = ar.get("twin_deltas").and_then(|v| v.as_array()) {
                for td in twins {
                    if let Some(tid) = td.get("twin_id").and_then(|v| v.as_str()) {
                        if !tid.trim().is_empty() {
                            lit_quad!(
                                ns_predicate("mutatedTwin"),
                                tid.to_string(),
                                xsd_string.clone()
                            );
                        }
                    }
                }
            }
        }
    }

    // Thesis RDF projection
    if payload.memory_type == "thesis" {
        let thesis_obj = payload.metadata.get("thesis").and_then(|v| v.as_object());
        let get_field = |key: &str| -> Option<&serde_json::Value> {
            payload
                .metadata
                .get(key)
                .or_else(|| thesis_obj.and_then(|o| o.get(key)))
        };
        if let Some(s) = get_field("project_purpose").and_then(|v| v.as_str()) {
            lit_quad!(
                ns_predicate("projectPurpose"),
                s.to_string(),
                xsd_string.clone()
            );
        }
        if let Some(s) = get_field("domain_summary").and_then(|v| v.as_str()) {
            lit_quad!(
                ns_predicate("domainSummary"),
                s.to_string(),
                xsd_string.clone()
            );
        }
        if let Some(to) = get_field("target_outcomes").and_then(|v| v.as_array()) {
            for (idx, outcome) in to.iter().enumerate() {
                if let Some(s) = outcome.as_str() {
                    lit_quad!(
                        ns_predicate(&format!("targetOutcome_{idx}")),
                        s.to_string(),
                        xsd_string.clone()
                    );
                }
            }
        }
        if let Some(at) = get_field("architectural_tenets").and_then(|v| v.as_array()) {
            for (idx, tenet) in at.iter().enumerate() {
                if let Some(s) = tenet.as_str() {
                    lit_quad!(
                        ns_predicate(&format!("architecturalTenet_{idx}")),
                        s.to_string(),
                        xsd_string.clone()
                    );
                }
            }
        }
        if let Some(cc) = get_field("canonical_concepts").and_then(|v| v.as_array()) {
            for concept in cc {
                if let Some(s) = concept.as_str() {
                    iri_quad!(ns_predicate("canonicalConcept"), s.to_string());
                }
            }
        }
        if let Some(er) = get_field("external_references").and_then(|v| v.as_array()) {
            for ref_obj in er {
                if let Some(obj) = ref_obj.as_object() {
                    if let Some(doi) = obj.get("doi").and_then(|v| v.as_str()) {
                        lit_quad!(
                            ns_predicate("externalReferenceDoi"),
                            doi.to_string(),
                            xsd_string.clone()
                        );
                    }
                    if let Some(title) = obj.get("title").and_then(|v| v.as_str()) {
                        lit_quad!(
                            ns_predicate("externalReferenceTitle"),
                            title.to_string(),
                            xsd_string.clone()
                        );
                    }
                }
            }
        }
        quads.push(Quad {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("Thesis"))),
            graph_name: graph.clone(),
        });
        quads.push(Quad {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("ProjectThesis"))),
            graph_name: graph.clone(),
        });
    }

    // Concept RDF projection (W3C SKOS):
    //
    // Active visible `concept` rows project W3C SKOS triples into the
    // namespace RDF graph.
    if payload.memory_type == "concept" {
        let skos = |name: &str| -> String { format!("http://www.w3.org/2004/02/skos/core#{name}") };
        let concept_node_for_slug =
            |slug_str: &str| -> NamedNode { canonical_concept_node(namespace, slug_str, None) };
        if let Some(c) = payload.metadata.get("concept").and_then(|v| v.as_object()) {
            let slug = c
                .get("slug")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();
            let concept_nn = canonical_concept_node(
                namespace,
                &slug,
                c.get("concept_iri").and_then(|v| v.as_str()),
            );
            let concept_subject = NamedOrBlankNode::NamedNode(concept_nn.clone());
            macro_rules! push_concept_quad {
                ($subj:expr, $pred:expr, $obj:expr) => {
                    quads.push(Quad {
                        subject: ($subj).clone(),
                        predicate: NamedNode::new_unchecked(($pred).to_string()),
                        object: $obj,
                        graph_name: graph.clone(),
                    });
                };
            }

            // Emit rdf:type skos:Concept on both the concept IRI subject and memory subject
            push_concept_quad!(
                &concept_subject,
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                Term::NamedNode(NamedNode::new_unchecked(skos("Concept")))
            );
            push_concept_quad!(
                &subject,
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                Term::NamedNode(NamedNode::new_unchecked(skos("Concept")))
            );

            if let Some(s) = c.get("slug").and_then(|v| v.as_str()) {
                iri_quad!(ns_predicate("conceptSlug"), s.to_string());
            }
            if let Some(pref_label) = c.get("pref_label").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("prefLabel"),
                    pref_label.to_string(),
                    xsd_string.clone()
                );
                let lit = Term::Literal(Literal::new_typed_literal(
                    pref_label.to_string(),
                    xsd_string.clone(),
                ));
                push_concept_quad!(&concept_subject, &skos("prefLabel"), lit.clone());
                push_concept_quad!(&subject, &skos("prefLabel"), lit);
            }
            if let Some(alt_labels) = c.get("alt_labels").and_then(|v| v.as_array()) {
                for (idx, alt) in alt_labels.iter().enumerate() {
                    if let Some(s) = alt.as_str() {
                        lit_quad!(
                            ns_predicate(&format!("altLabel_{idx}")),
                            s.to_string(),
                            xsd_string.clone()
                        );
                        let lit = Term::Literal(Literal::new_typed_literal(
                            s.to_string(),
                            xsd_string.clone(),
                        ));
                        push_concept_quad!(&concept_subject, &skos("altLabel"), lit.clone());
                        push_concept_quad!(&subject, &skos("altLabel"), lit);
                    }
                }
            }
            let def_str = c
                .get("definition")
                .and_then(|v| v.as_str())
                .unwrap_or(payload.content.as_str());
            if !def_str.is_empty() {
                lit_quad!(
                    ns_predicate("definition"),
                    def_str.to_string(),
                    xsd_string.clone()
                );
                let lit = Term::Literal(Literal::new_typed_literal(
                    def_str.to_string(),
                    xsd_string.clone(),
                ));
                push_concept_quad!(&concept_subject, &skos("definition"), lit.clone());
                push_concept_quad!(&subject, &skos("definition"), lit);
            }
            if let Some(domain) = c.get("domain").and_then(|v| v.as_str()) {
                lit_quad!(
                    ns_predicate("domain"),
                    domain.to_string(),
                    xsd_string.clone()
                );
            }
            if let Some(ext_iri) = c.get("concept_iri").and_then(|v| v.as_str()) {
                if !ext_iri.trim().is_empty() {
                    iri_quad!(ns_predicate("conceptIri"), ext_iri.to_string());
                    let ext_nn = Term::NamedNode(NamedNode::new_unchecked(ext_iri.to_string()));
                    push_concept_quad!(&concept_subject, &skos("exactMatch"), ext_nn.clone());
                    push_concept_quad!(&subject, &skos("exactMatch"), ext_nn);
                }
            }

            let broader_vec: Vec<String> = c
                .get("broader")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            for (idx, b_slug) in broader_vec.iter().enumerate() {
                iri_quad!(
                    ns_predicate(&format!("broaderConcept_{idx}")),
                    b_slug.clone()
                );
                let parent_nn = concept_node_for_slug(b_slug);
                let parent_subj = NamedOrBlankNode::NamedNode(parent_nn.clone());
                push_concept_quad!(
                    &concept_subject,
                    &skos("broader"),
                    Term::NamedNode(parent_nn.clone())
                );
                push_concept_quad!(&subject, &skos("broader"), Term::NamedNode(parent_nn));
                // Derived inverse skos:narrower on the parent concept node
                push_concept_quad!(
                    &parent_subj,
                    &skos("narrower"),
                    Term::NamedNode(concept_nn.clone())
                );
            }

            let related_vec: Vec<String> = c
                .get("related")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            for (idx, r_slug) in related_vec.iter().enumerate() {
                iri_quad!(
                    ns_predicate(&format!("relatedConcept_{idx}")),
                    r_slug.clone()
                );
                let rel_nn = concept_node_for_slug(r_slug);
                let rel_subj = NamedOrBlankNode::NamedNode(rel_nn.clone());
                push_concept_quad!(
                    &concept_subject,
                    &skos("related"),
                    Term::NamedNode(rel_nn.clone())
                );
                push_concept_quad!(&subject, &skos("related"), Term::NamedNode(rel_nn));
                push_concept_quad!(
                    &rel_subj,
                    &skos("related"),
                    Term::NamedNode(concept_nn.clone())
                );
            }

            let contrasts_vec: Vec<String> = c
                .get("contrasts_with")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            for (idx, cw_slug) in contrasts_vec.iter().enumerate() {
                iri_quad!(
                    ns_predicate(&format!("contrastsWith_{idx}")),
                    cw_slug.clone()
                );
                let cw_nn = concept_node_for_slug(cw_slug);
                let cw_subj = NamedOrBlankNode::NamedNode(cw_nn.clone());
                let pred_cw = ns_predicate("contrastsWith");
                push_concept_quad!(&concept_subject, &pred_cw, Term::NamedNode(cw_nn.clone()));
                push_concept_quad!(&subject, &pred_cw, Term::NamedNode(cw_nn));
                push_concept_quad!(&cw_subj, &pred_cw, Term::NamedNode(concept_nn.clone()));
            }

            let realization_status = c
                .get("realization_status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let rs_lit = Term::Literal(Literal::new_typed_literal(
                realization_status.to_string(),
                xsd_string.clone(),
            ));
            push_concept_quad!(
                &concept_subject,
                &ns_predicate("realizationStatus"),
                rs_lit.clone()
            );
            push_concept_quad!(&subject, &ns_predicate("realizationStatus"), rs_lit);

            // Dynamic read-time epistemic_status projection
            let non_empty_arr = |val: Option<&serde_json::Value>| -> bool {
                val.and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            };
            let non_empty_str = |val: Option<&serde_json::Value>| -> bool {
                val.and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            };
            let has_definition = !def_str.trim().is_empty();
            let has_taxonomy_link = non_empty_str(c.get("domain"))
                || !broader_vec.is_empty()
                || non_empty_arr(c.get("narrower"))
                || !related_vec.is_empty();
            let has_synthesis = non_empty_arr(c.get("tradeoffs"))
                && (!contrasts_vec.is_empty()
                    || non_empty_arr(c.get("references"))
                    || non_empty_arr(c.get("comparable_implementations")));
            let has_empirical_grounding = has_synthesis
                && (non_empty_arr(c.get("evidence_refs"))
                    || non_empty_arr(payload.metadata.get("governs")));
            let ep_status = crate::formal_kernel::classify_epistemic_status(
                has_definition,
                has_taxonomy_link,
                has_synthesis,
                has_empirical_grounding,
            );
            let ep_lit = Term::Literal(Literal::new_typed_literal(
                ep_status.as_str().to_string(),
                xsd_string.clone(),
            ));
            push_concept_quad!(
                &concept_subject,
                &ns_predicate("epistemicStatus"),
                ep_lit.clone()
            );
            push_concept_quad!(&subject, &ns_predicate("epistemicStatus"), ep_lit);
        }
        quads.push(Quad {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("Concept"))),
            graph_name: graph.clone(),
        });
    }

    // Multimodal attachment RDF projection
    if let Some(atts) = payload
        .metadata
        .get("attachments")
        .and_then(|v| v.as_array())
    {
        let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer");
        for (idx, att) in atts.iter().enumerate() {
            let Some(obj) = att.as_object() else { continue };
            let raw_id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let sanitized_id: String = raw_id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
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
                predicate: NamedNode::new_unchecked(
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                ),
                object: Term::NamedNode(NamedNode::new_unchecked(ns_predicate("Attachment"))),
                graph_name: graph.clone(),
            });
            if !raw_id.is_empty() {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentId")),
                    object: Term::Literal(Literal::new_typed_literal(
                        raw_id.to_string(),
                        xsd_string.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(mime) = obj.get("mime").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentMime")),
                    object: Term::Literal(Literal::new_typed_literal(
                        mime.to_string(),
                        xsd_string.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(sz) = obj.get("size_bytes").and_then(|v| v.as_u64()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentSizeBytes")),
                    object: Term::Literal(Literal::new_typed_literal(
                        sz.to_string(),
                        xsd_int.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(dig) = obj.get("digest").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentDigest")),
                    object: Term::Literal(Literal::new_typed_literal(
                        dig.to_string(),
                        xsd_string.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(uri) = obj.get("uri").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentUri")),
                    object: Term::Literal(Literal::new_typed_literal(
                        uri.to_string(),
                        xsd_string.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(w) = obj.get("width").and_then(|v| v.as_u64()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentWidth")),
                    object: Term::Literal(Literal::new_typed_literal(
                        w.to_string(),
                        xsd_int.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(h) = obj.get("height").and_then(|v| v.as_u64()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentHeight")),
                    object: Term::Literal(Literal::new_typed_literal(
                        h.to_string(),
                        xsd_int.clone(),
                    )),
                    graph_name: graph.clone(),
                });
            }
            if let Some(alt) = obj.get("alt_text").and_then(|v| v.as_str()) {
                quads.push(Quad {
                    subject: bnode_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("attachmentAltText")),
                    object: Term::Literal(Literal::new_typed_literal(
                        alt.to_string(),
                        xsd_string.clone(),
                    )),
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
    let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        from
    )));
    let object = Term::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        to
    )));
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
        let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
            "urn:neurostrata:memory:{}",
            from
        )));
        let graph = graph.unwrap_or(GraphName::DefaultGraph);
        let xsd_string = NamedNode::new_unchecked(XSD_STRING.to_string());
        let xsd_integer =
            NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer".to_string());
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
                Term::Literal(Literal::new_typed_literal(
                    vf.to_string(),
                    xsd_integer.clone(),
                )),
            ));
        }
        if let Some(vt) = prov.valid_to {
            provenance_quals.push((
                "provenanceValidTo",
                Term::Literal(Literal::new_typed_literal(
                    vt.to_string(),
                    xsd_integer.clone(),
                )),
            ));
        }
        if let Some(gen) = prov.generation {
            provenance_quals.push((
                "provenanceGeneration",
                Term::Literal(Literal::new_typed_literal(
                    gen.to_string(),
                    xsd_integer.clone(),
                )),
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

/// Emit schema-resolved `rdf:type urn:neurostrata:interface:<slug>` and
/// `ns:twinDerived_<name>` quads for a `digital_twin` row given the namespace's
/// active schema map, rows, and edges.
pub fn twin_schema_and_reducer_quads_for_row(
    id: &str,
    payload: &MemoryPayload,
    namespace: &str,
    tier: Tier,
    generation: u64,
    schemas: &BTreeMap<String, crate::digital_twin::TwinSchema>,
    rows: &[(String, MemoryPayload)],
    edges: &[(String, String, String, Option<EdgeProvenance>)],
) -> Vec<Quad> {
    let mut quads = Vec::new();
    if payload.memory_type != "digital_twin" {
        return quads;
    }
    let Some(dt) = payload
        .metadata
        .get("digital_twin")
        .or_else(|| payload.metadata.get("twin"))
        .and_then(|v| v.as_object())
    else {
        return quads;
    };
    let Some(schema_id) = dt.get("schema_id").and_then(|v| v.as_str()) else {
        return quads;
    };
    let Some(raw_schema) = schemas.get(schema_id) else {
        return quads;
    };
    let eff = crate::digital_twin::resolve_effective_twin_schema(raw_schema, schemas);
    let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        id
    )));
    let graph_iri = format!(
        "urn:neurostrata:graph:{}:{}:gen:{}",
        namespace,
        tier.as_str(),
        generation
    );
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(graph_iri));
    let ldp_json = NamedNode::new_unchecked(LDP_JSON.to_string());

    for iface in &eff.resolved_interfaces {
        let iface_iri = format!("urn:neurostrata:interface:{}", sanitize_iri_suffix(iface));
        quads.push(Quad {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            object: Term::NamedNode(NamedNode::new_unchecked(iface_iri)),
            graph_name: graph.clone(),
        });
    }

    if !eff.reducers.is_empty() {
        let twin_id = dt
            .get("id")
            .or_else(|| dt.get("twin_id"))
            .and_then(|v| v.as_str())
            .unwrap_or(id);
        let derived = crate::digital_twin::compute_twin_reducers(
            id, twin_id, payload, &eff, rows, edges, schemas, None,
        );
        for (k, v) in derived {
            quads.push(Quad {
                subject: subject.clone(),
                predicate: NamedNode::new_unchecked(ns_predicate(&format!(
                    "twinDerived_{}",
                    sanitize_iri_suffix(&k)
                ))),
                object: Term::Literal(Literal::new_typed_literal(v.to_string(), ldp_json.clone())),
                graph_name: graph.clone(),
            });
        }
    }

    quads
}

/// Construct the canonical SKOS concept IRI (`urn:neurostrata:concept:{namespace}:{slug}`)
/// per Spec §4.2, honoring an explicit valid `custom_iri` when present.
pub fn canonical_concept_node(namespace: &str, slug: &str, custom_iri: Option<&str>) -> NamedNode {
    if let Some(iri) = custom_iri.map(str::trim).filter(|s| !s.is_empty()) {
        if NamedNode::new(iri).is_ok() {
            return NamedNode::new_unchecked(iri.to_string());
        }
    }
    if slug.contains(':') && NamedNode::new(slug).is_ok() {
        return NamedNode::new_unchecked(slug.to_string());
    }
    NamedNode::new_unchecked(format!(
        "urn:neurostrata:concept:{}:{}",
        sanitize_iri_suffix(namespace),
        sanitize_iri_suffix(slug)
    ))
}

/// Reconcile a `concept` row's projected RDF quads (`ns:epistemicStatus`, derived `skos:narrower`,
/// and symmetrized `skos:related` / `ns:contrastsWith`) against the multi-row
/// [`crate::rdf::taxonomy::PrincipalTaxonomyView`] so SPARQL and dataset exports agree 100%
/// with `neurostrata_taxonomy` operations.
pub fn reconcile_concept_quads_with_view(
    id: &str,
    payload: &MemoryPayload,
    namespace: &str,
    tier: Tier,
    generation: u64,
    view: &crate::rdf::taxonomy::PrincipalTaxonomyView,
    quads: &mut Vec<Quad>,
) {
    if payload.memory_type != "concept" {
        return;
    }
    let Some(c) = payload.metadata.get("concept").and_then(|v| v.as_object()) else {
        return;
    };
    let slug = c.get("slug").and_then(|v| v.as_str()).unwrap_or(id);
    let Some(hydrated) = view.concepts.get(slug) else {
        return;
    };
    if hydrated.id != id {
        return;
    }

    let graph_iri = format!(
        "urn:neurostrata:graph:{}:{}:gen:{}",
        namespace,
        tier.as_str(),
        generation
    );
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(graph_iri));
    let subject = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{id}"
    )));
    let concept_subject = NamedOrBlankNode::NamedNode(canonical_concept_node(
        namespace,
        slug,
        c.get("concept_iri")
            .and_then(|v| v.as_str())
            .or(hydrated.concept_iri.as_deref()),
    ));

    let ep_pred = NamedNode::new_unchecked(ns_predicate("epistemicStatus"));
    quads.retain(|q| q.predicate != ep_pred);

    let ep_lit = Term::Literal(Literal::new_typed_literal(
        &hydrated.epistemic_status,
        NamedNode::new_unchecked(XSD_STRING),
    ));
    let q1 = Quad::new(
        concept_subject.clone(),
        ep_pred.clone(),
        ep_lit.clone(),
        graph.clone(),
    );
    if !quads.contains(&q1) {
        quads.push(q1);
    }
    let q2 = Quad::new(subject.clone(), ep_pred, ep_lit, graph.clone());
    if !quads.contains(&q2) {
        quads.push(q2);
    }

    let skos_narrower = NamedNode::new_unchecked("http://www.w3.org/2004/02/skos/core#narrower");
    let skos_related = NamedNode::new_unchecked("http://www.w3.org/2004/02/skos/core#related");
    let ns_contrasts = NamedNode::new_unchecked(ns_predicate("contrastsWith"));

    let concept_node_for_slug = |target_slug: &str| -> NamedNode {
        canonical_concept_node(
            namespace,
            target_slug,
            view.concepts
                .get(target_slug)
                .and_then(|tc| tc.concept_iri.as_deref()),
        )
    };

    for n_slug in &hydrated.narrower {
        let child_nn = concept_node_for_slug(n_slug);
        let q_c = Quad::new(
            concept_subject.clone(),
            skos_narrower.clone(),
            Term::NamedNode(child_nn.clone()),
            graph.clone(),
        );
        if !quads.contains(&q_c) {
            quads.push(q_c);
        }
        let q_s = Quad::new(
            subject.clone(),
            skos_narrower.clone(),
            Term::NamedNode(child_nn),
            graph.clone(),
        );
        if !quads.contains(&q_s) {
            quads.push(q_s);
        }
    }

    for r_slug in &hydrated.related_concepts {
        let rel_nn = concept_node_for_slug(r_slug);
        let q_c = Quad::new(
            concept_subject.clone(),
            skos_related.clone(),
            Term::NamedNode(rel_nn.clone()),
            graph.clone(),
        );
        if !quads.contains(&q_c) {
            quads.push(q_c);
        }
        let q_s = Quad::new(
            subject.clone(),
            skos_related.clone(),
            Term::NamedNode(rel_nn),
            graph.clone(),
        );
        if !quads.contains(&q_s) {
            quads.push(q_s);
        }
    }

    for cw_slug in &hydrated.contrasts_with {
        let cw_nn = concept_node_for_slug(cw_slug);
        let q_c = Quad::new(
            concept_subject.clone(),
            ns_contrasts.clone(),
            Term::NamedNode(cw_nn.clone()),
            graph.clone(),
        );
        if !quads.contains(&q_c) {
            quads.push(q_c);
        }
        let q_s = Quad::new(
            subject.clone(),
            ns_contrasts.clone(),
            Term::NamedNode(cw_nn),
            graph.clone(),
        );
        if !quads.contains(&q_s) {
            quads.push(q_s);
        }
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
    let schemas = crate::digital_twin::collect_twin_schemas_from_rows(rows);
    let has_concepts = rows.iter().any(|(_, p)| p.memory_type == "concept");
    let tax_view = if has_concepts {
        Some(crate::rdf::taxonomy::build_principal_taxonomy_view(
            rows, edges, "admin",
        ))
    } else {
        None
    };
    for (id, payload) in rows {
        let mut qs = memory_to_quads(id, payload, namespace, tier, generation);
        if !schemas.is_empty() && payload.memory_type == "digital_twin" {
            let extra = twin_schema_and_reducer_quads_for_row(
                id, payload, namespace, tier, generation, &schemas, rows, edges,
            );
            for eq in extra {
                if !qs.contains(&eq) {
                    qs.push(eq);
                }
            }
        }
        if let Some(ref view) = tax_view {
            if payload.memory_type == "concept" {
                reconcile_concept_quads_with_view(
                    id, payload, namespace, tier, generation, view, &mut qs,
                );
            }
        }
        out.extend(quads_to_legacy(&qs));
    }
    let graph = super::types_oxrdf::OxRdfBridge::tier_to_graph_name(&tier, namespace, generation);
    let graph = Some(graph);
    let ox_quads: Vec<Quad> = edges
        .iter()
        .flat_map(|(from, rel, to, prov)| edge_to_quads(from, rel, to, prov.clone(), graph.clone()))
        .collect();
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
    use oxrdf::Dataset;
    use serde_json::json;

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
        let preds: Vec<String> = qs
            .iter()
            .map(|q| q.predicate.as_str().to_string())
            .collect();
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
            q.predicate.as_str() == "urn:neurostrata:ns#conceptIri"
                && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:example:foo")
        }));

        // Two memories sharing one concept keep distinct subjects.
        let qs2 = memory_to_quads("m2", &p, "ns", Tier::Domain, 0);
        assert!(qs2.iter().all(|q| {
            match &q.subject {
                NamedOrBlankNode::NamedNode(nn) => {
                    nn.as_str() == format!("urn:neurostrata:memory:m2")
                }
                _ => false,
            }
        }));
    }

    #[test]
    fn edge_projects_relation() {
        let q = edge_to_quad("a", "GOVERNS", "b", None, Some(GraphName::DefaultGraph));
        assert_eq!(
            q.predicate,
            NamedNode::new_unchecked("urn:neurostrata:ns#governs")
        );
        assert!(
            matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:b")
        );
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

        let qs = edge_to_quads(
            "a",
            "GOVERNS",
            "b",
            Some(prov),
            Some(GraphName::DefaultGraph),
        );
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
        assert!(qs
            .iter()
            .any(|q| q.predicate.as_str().ends_with("#provenanceSource")
                && matches!(&q.object, Term::Literal(l) if l.value() == "owner")));
    }

    #[test]
    fn rows_to_oxrdf_quads_combines() {
        let rows = vec![("m1".to_string(), payload())];
        let edges = vec![(
            "m1".to_string(),
            "GOVERNS".to_string(),
            "m2".to_string(),
            None,
        )];
        let qs = rows_to_quads(&rows, "ns", Tier::Domain, 0, &edges);
        assert!(qs
            .iter()
            .any(|q| q.predicate.as_str().ends_with("#governs")));
    }

    #[test]
    fn oxrdf_to_legacy_round_trip() {
        let qs = memory_to_quads("m1", &payload(), "ns", Tier::Domain, 0);
        let legacy = quads_to_legacy(&qs);

        assert_eq!(legacy.len(), qs.len());
        for (orig, conv) in qs.iter().zip(legacy.iter()) {
            let back =
                super::super::types_oxrdf::OxRdfBridge::neurostrata_quad_to_oxrdf(conv).unwrap();
            assert_eq!(orig, &back.inner);
        }
    }

    #[test]
    fn action_types_and_receipts_project_expected_rdf_classes_and_revert_links() {
        let action_def_payload = MemoryPayload {
            content: "Action: recalibrate-cooling-loop".to_string(),
            user_id: "alice".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "eligibility": "shareable",
                "action_type": {
                    "slug": "recalibrate-cooling-loop",
                    "title": "Recalibrate Cooling Loop",
                    "description": "Adjusts cooling loop target temperature",
                    "required_role": "editor",
                    "parameters": {
                        "twin_id": { "param_type": "twin_id", "required": true },
                        "target_temp": { "param_type": "f64", "required": true }
                    },
                    "submission_criteria": [],
                    "effects": []
                }
            }),
        };
        let def_quads = memory_to_quads("act-def-1", &action_def_payload, "ns", Tier::Domain, 0);
        assert!(
            def_quads.iter().any(|q| {
                q.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:ns#ActionType")
            }),
            "action_type must project rdf:type ns:ActionType"
        );
        assert!(def_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#actionSlug"
                && matches!(&q.object, Term::Literal(l) if l.value() == "recalibrate-cooling-loop")
        }));
        assert!(def_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#requiredRole"
                && matches!(&q.object, Term::Literal(l) if l.value() == "editor")
        }));
        assert!(def_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#actionParam"
                && matches!(&q.object, Term::Literal(l) if l.value() == "twin_id")
        }));

        let receipt_payload = MemoryPayload {
            content: "Action receipt r1".to_string(),
            user_id: "alice".to_string(),
            memory_type: "action_receipt".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "eligibility": "private",
                "action_receipt": {
                    "receipt_id": "act-receipt-1",
                    "action_slug": "recalibrate-cooling-loop",
                    "principal": "alice",
                    "executed_at": 1794675600,
                    "params": { "twin_id": "loop-a", "target_temp": 42.5 },
                    "superseded_pairs": [{ "before_id": "mem-old", "after_id": "mem-new" }],
                    "created_ids": ["mem-created-1"],
                    "twin_deltas": [{
                        "twin_id": "loop-a",
                        "before_row_id": "twin-v1",
                        "after_row_id": "twin-v2",
                        "before_version": 1,
                        "after_version": 2,
                        "name": "temperature",
                        "old": 65.0,
                        "new": 42.5
                    }],
                    "edge_deltas": [],
                    "row_deltas": [],
                    "task_deltas": [],
                    "reverted_at": 1794675700,
                    "reverted_by_receipt_id": "act-revert-99",
                    "reverts_receipt_id": "act-receipt-0",
                    "forced_by": "admin",
                    "forced_at": 1794675700
                }
            }),
        };
        let rec_quads = memory_to_quads("act-receipt-1", &receipt_payload, "ns", Tier::Domain, 0);
        assert!(
            rec_quads.iter().any(|q| {
                q.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:ns#ActionReceipt")
            }),
            "action_receipt must project rdf:type ns:ActionReceipt"
        );
        assert!(rec_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#revertedByReceipt"
                && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:act-revert-99")
        }));
        assert!(rec_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#revertsReceipt"
                && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:act-receipt-0")
        }));
        assert!(rec_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#supersededMemory"
                && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:mem-old")
        }));
        assert!(rec_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#replacementMemory"
                && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:mem-new")
        }));
        assert!(rec_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#createdMemory"
                && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:memory:mem-created-1")
        }));
        assert!(rec_quads.iter().any(|q| {
            q.predicate.as_str() == "urn:neurostrata:ns#mutatedTwin"
                && matches!(&q.object, Term::Literal(l) if l.value() == "loop-a")
        }));
    }

    #[test]
    fn thesis_projects_rdf_type_and_key_fields() {
        let thesis_payload = MemoryPayload {
            content: "Project north star: distributed knowledge graph".to_string(),
            user_id: "alice".to_string(),
            memory_type: "thesis".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "project_purpose": "Enable decentralized semantic reasoning",
                "domain_summary": "Knowledge graphs for AI reasoning",
                "target_outcomes": ["latency < 100ms", "throughput > 10K QPS"],
                "architectural_tenets": ["modular", "extensible", "observability"],
                "canonical_concepts": ["concept-knowledge-graph", "concept-reasoning"],
                "external_references": [
                    { "doi": "10.1234/abc", "title": "Foundations of Knowledge Graphs" },
                    { "doi": "10.5678/def", "title": "Semantic Web Standards" }
                ]
            }),
        };
        let qs = memory_to_quads("thesis-1", &thesis_payload, "ns", Tier::Domain, 0);

        // Should project Thesis rdf:type
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:ns#Thesis")
            }),
            "thesis must project rdf:type ns:Thesis"
        );
        // Should project projectPurpose
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#projectPurpose"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "Enable decentralized semantic reasoning")
            }),
            "thesis must project projectPurpose"
        );
        // Should project domainSummary
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#domainSummary"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "Knowledge graphs for AI reasoning")
            }),
            "thesis must project domainSummary"
        );
        // Should project targetOutcome_0
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#targetOutcome_0"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "latency < 100ms")
            }),
            "thesis must project targetOutcome_0"
        );
        // Should project architecturalTenet_0
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#architecturalTenet_0"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "modular")
            }),
            "thesis must project architecturalTenet_0"
        );
        // Should project canonicalConcept
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#canonicalConcept"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "concept-knowledge-graph")
            }),
            "thesis must project canonicalConcept"
        );
        // Should project externalReferenceDoi
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#externalReferenceDoi"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "10.1234/abc")
            }),
            "thesis must project externalReferenceDoi"
        );
        // Should project externalReferenceTitle
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#externalReferenceTitle"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "Foundations of Knowledge Graphs")
            }),
            "thesis must project externalReferenceTitle"
        );
    }

    #[test]
    fn concept_projects_rdf_type_and_skos_fields() {
        let concept_payload = MemoryPayload {
            content: "SKOS concept for knowledge graph reasoning".to_string(),
            user_id: "alice".to_string(),
            memory_type: "concept".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "concept": {
                    "slug": "concept-knowledge-graph",
                    "pref_label": "Knowledge Graph Concept",
                    "alt_labels": ["KG Concept", "Graph Concept"],
                    "definition": "A concept for knowledge graph reasoning",
                    "domain": "knowledge-graphs",
                    "concept_iri": "urn:example:kg-concept"
                }
            }),
        };
        let qs = memory_to_quads("concept-1", &concept_payload, "ns", Tier::Domain, 0);

        // Should project Concept rdf:type
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:neurostrata:ns#Concept")
            }),
            "concept must project rdf:type ns:Concept"
        );
        // Should project conceptSlug
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#conceptSlug"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "concept-knowledge-graph")
            }),
            "concept must project conceptSlug"
        );
        // Should project prefLabel
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#prefLabel"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "Knowledge Graph Concept")
            }),
            "concept must project prefLabel"
        );
        // Should project altLabel_0
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#altLabel_0"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "KG Concept")
            }),
            "concept must project altLabel_0"
        );
        // Should project altLabel_1
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#altLabel_1"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "Graph Concept")
            }),
            "concept must project altLabel_1"
        );
        // Should project definition
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#definition"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "A concept for knowledge graph reasoning")
            }),
            "concept must project definition"
        );
        // Should project domain
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#domain"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "knowledge-graphs")
            }),
            "concept must project domain"
        );
        // Should project conceptIri
        assert!(
            qs.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#conceptIri"
                    && matches!(&q.object, Term::NamedNode(nn) if nn.as_str() == "urn:example:kg-concept")
            }),
            "concept must project conceptIri"
        );
    }

    #[tokio::test]
    async fn structural_actions_stay_out_of_default_vector_and_lexical_recall() {
        use crate::traits::VectorStore;

        let tmp =
            std::env::temp_dir().join(format!("ns-struct-action-recall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-recall").await.unwrap();

        let shared_vec = vec![0.9, 0.1, 0.0, 0.0];
        store
            .upsert(
                "ns-recall",
                "normative-fact-1",
                shared_vec.clone(),
                MemoryPayload {
                    content: "cryogenicmanifold77 normative cooling rule".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "domain": "ops" }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-recall",
                "struct-action-type-1",
                shared_vec.clone(),
                MemoryPayload {
                    content: "cryogenicmanifold77 action definition".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "action_type": {
                            "slug": "cryo-op",
                            "title": "Cryo Op",
                            "description": "cryogenicmanifold77",
                            "required_role": "editor",
                            "parameters": {},
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "noop",
                                    "metadata_template": {}
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-recall",
                "struct-action-receipt-1",
                shared_vec.clone(),
                MemoryPayload {
                    content: "cryogenicmanifold77 action receipt".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_receipt".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "eligibility": "private",
                        "action_receipt": {
                            "receipt_id": "struct-action-receipt-1",
                            "action_slug": "cryo-op",
                            "principal": "alice",
                            "executed_at": 1794675600,
                            "params": {},
                            "superseded_pairs": [],
                            "created_ids": [],
                            "twin_deltas": [],
                            "edge_deltas": [],
                            "row_deltas": [],
                            "task_deltas": [],
                            "reverted_at": null,
                            "reverted_by_receipt_id": null
                        }
                    }),
                },
            )
            .await
            .unwrap();

        // 1. Default vector recall excludes action_type and action_receipt
        let vec_hits = store
            .search("ns-recall", shared_vec, 10, false, false)
            .await
            .unwrap();
        let vec_ids: Vec<&str> = vec_hits.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(vec_ids, vec!["normative-fact-1"]);

        // 2. Default lexical recall excludes action_type and action_receipt
        let lex_hits = store
            .search_lexical(
                "ns-recall",
                "cryogenicmanifold77",
                10,
                false,
                false,
                &[],
                "alice",
                0,
            )
            .await
            .unwrap();
        let lex_ids: Vec<&str> = lex_hits.results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(lex_ids, vec!["normative-fact-1"]);

        // 3. Direct SPARQL over the projected dataset still finds both ActionType and ActionReceipt
        let (ask_type_str, _, _, _) = crate::rdf::sparql::execute_sparql_query(
            "ASK { <urn:neurostrata:memory:struct-action-type-1> a <urn:neurostrata:ns#ActionType> }",
            "ns-recall",
            sparesults::QueryResultsFormat::Json,
            true,
            false,
            &store,
        )
        .await
        .unwrap();
        let ask_type: serde_json::Value = serde_json::from_str(&ask_type_str).unwrap();
        assert_eq!(ask_type["boolean"], json!(true));
        let (ask_rec_str, _, _, _) = crate::rdf::sparql::execute_sparql_query(
            "ASK { <urn:neurostrata:memory:struct-action-receipt-1> a <urn:neurostrata:ns#ActionReceipt> }",
            "ns-recall",
            sparesults::QueryResultsFormat::Json,
            true,
            false,
            &store,
        )
        .await
        .unwrap();
        let ask_rec: serde_json::Value = serde_json::from_str(&ask_rec_str).unwrap();
        assert_eq!(ask_rec["boolean"], json!(true));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn shareable_action_type_round_trips_but_receipt_is_rejected_everywhere() {
        use crate::synaptic_mesh::import::{validate_record, Candidate};
        use crate::synaptic_mesh::outbox::{
            is_shareable_memory_type, outbox_dir_for, Outbox, OutboxEntry, OP_CREATED,
        };
        use crate::synaptic_mesh::storage::MeshStorage;
        use crate::synaptic_mesh::worker::drain_once;
        use uuid::Uuid;

        assert!(
            is_shareable_memory_type("action_type"),
            "action_type must be in SHAREABLE_MEMORY_TYPES when opted into eligibility: shareable"
        );
        assert!(
            !is_shareable_memory_type("action_receipt"),
            "action_receipt must never be shareable"
        );

        let base = std::env::temp_dir().join(format!(
            "ns-mesh-action-share-{}-{}",
            std::process::id(),
            Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mesh = MeshStorage::init(&base, "node-a", "team", &["ns".to_string()], None).unwrap();
        let outbox = Outbox::open(&outbox_dir_for(mesh.root())).unwrap();

        let action_id = Uuid::now_v7();
        let receipt_id = Uuid::now_v7();

        outbox
            .append(&OutboxEntry {
                stable_op_id: Uuid::now_v7(),
                op: OP_CREATED.to_string(),
                id: action_id.to_string(),
                namespace: "ns".to_string(),
                memory_type: "action_type".to_string(),
                content: "Action: shared-op".to_string(),
                metadata: json!({
                    "eligibility": "shareable",
                    "action_type": {
                        "slug": "shared-op",
                        "title": "Shared Op",
                        "description": "Shareable action definition",
                        "required_role": "editor",
                        "parameters": {},
                        "submission_criteria": [],
                        "effects": []
                    }
                }),
                superseded_by: None,
                ts: 1794675600,
                epoch: 1,
                bus_seq: None,
                bus_key: None,
            })
            .unwrap();

        // Even if an action_receipt were somehow appended to outbox with forged eligibility: shareable,
        // worker::drain_once must skip it and never write a mesh file.
        outbox
            .append(&OutboxEntry {
                stable_op_id: Uuid::now_v7(),
                op: OP_CREATED.to_string(),
                id: receipt_id.to_string(),
                namespace: "ns".to_string(),
                memory_type: "action_receipt".to_string(),
                content: "Receipt: secret".to_string(),
                metadata: json!({
                    "eligibility": "shareable",
                    "action_receipt": {
                        "receipt_id": receipt_id.to_string(),
                        "action_slug": "shared-op",
                        "principal": "alice",
                        "executed_at": 1794675600
                    }
                }),
                superseded_by: None,
                ts: 1794675600,
                epoch: 1,
                bus_seq: None,
                bus_key: None,
            })
            .unwrap();

        let report = drain_once(&outbox, &mesh, "node-a").unwrap();
        assert_eq!(report.applied, 1, "only action_type may be drained");
        assert_eq!(report.skipped, 1, "action_receipt must be skipped");

        let action_path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{action_id}.json"));
        let receipt_path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{receipt_id}.json"));
        assert!(action_path.exists(), "shareable action_type record written");
        assert!(
            !receipt_path.exists(),
            "action_receipt must never be written"
        );

        // Validate import accepts action_type and preserves metadata.action_type
        let raw_action = std::fs::read_to_string(&action_path).unwrap();
        let (parsed_action, _) = validate_record(
            &Candidate {
                namespace: "ns".to_string(),
                file_stem: action_id.to_string(),
            },
            &["ns".to_string()],
            &raw_action,
        )
        .expect("import must accept shareable action_type");
        assert_eq!(parsed_action.memory_type, "action_type");
        assert_eq!(
            parsed_action.metadata["action_type"]["slug"],
            json!("shared-op")
        );

        // Validate import rejects a forged action_receipt JSON record
        let forged_receipt_json = raw_action.replace(
            "\"memory_type\": \"action_type\"",
            "\"memory_type\": \"action_receipt\"",
        );
        let forged_res = validate_record(
            &Candidate {
                namespace: "ns".to_string(),
                file_stem: action_id.to_string(),
            },
            &["ns".to_string()],
            &forged_receipt_json,
        );
        assert!(
            forged_res.is_err(),
            "mesh import must reject action_receipt memory_type"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn forged_receipt_eligibility_is_private_and_log_fields_are_not_projected_publicly() {
        let forged_receipt = MemoryPayload {
            content: "Forged receipt".to_string(),
            user_id: "alice".to_string(),
            memory_type: "action_receipt".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "eligibility": "shareable",
                "action_receipt": {
                    "receipt_id": "rec-forged-1",
                    "action_slug": "secret-op",
                    "principal": "alice",
                    "executed_at": 1794675600,
                    "params": {
                        "api_secret": "ULTRA_SENSITIVE_PARAM_999"
                    },
                    "superseded_pairs": [],
                    "created_ids": [],
                    "twin_deltas": [],
                    "edge_deltas": [],
                    "row_deltas": [
                        {
                            "kind": "upsert_memory",
                            "before_id": null,
                            "before_payload": null,
                            "after_id": "m-secret",
                            "after_payload": {
                                "content": "ULTRA_SENSITIVE_BODY_888",
                                "user_id": "alice",
                                "memory_type": "fact",
                                "agent_name": null,
                                "location": "",
                                "location_lines": "",
                                "metadata": {}
                            },
                            "created": true
                        }
                    ],
                    "task_deltas": [],
                    "reverted_at": null,
                    "reverted_by_receipt_id": null
                }
            }),
        };

        // 1. SHACL validation rejects forged eligibility != "private"
        let violations = crate::rdf::shacl::validate_memory(&forged_receipt);
        assert!(
            violations.iter().any(|v| v.field == "metadata.eligibility"),
            "SHACL must reject forged eligibility on action_receipt: {violations:?}"
        );

        // 2. RDF projection forces ns:eligibility to "private" and never projects secret param/body values as public ns:* predicates
        let quads = memory_to_quads("rec-forged-1", &forged_receipt, "ns", Tier::Domain, 0);
        assert!(
            quads.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#eligibility"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "private")
            }),
            "projected ns:eligibility on action_receipt must be forced to 'private'"
        );
        assert!(
            !quads.iter().any(|q| {
                q.predicate.as_str() == "urn:neurostrata:ns#eligibility"
                    && matches!(&q.object, Term::Literal(l) if l.value() == "shareable")
            }),
            "forged shareable eligibility must never be projected"
        );
        for q in &quads {
            if q.predicate.as_str() != "urn:neurostrata:ns#metadata" {
                let obj_str = q.object.to_string();
                assert!(
                    !obj_str.contains("ULTRA_SENSITIVE_PARAM_999")
                        && !obj_str.contains("ULTRA_SENSITIVE_BODY_888"),
                    "public RDF predicates must not project raw param or delta body values: {q}"
                );
            }
        }
    }
}
