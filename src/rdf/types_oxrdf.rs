/// RDF projection types for NeuroStrata's semantic layer using oxrdf.
/// 
/// Phase 2: Replacing hand-written Quad/QuadObject/Tier types with oxrdf types
/// as specified in task neurostrata-c2jd (OxiRS adoption: migrate hand-written RDF to oxrdf/oxttl).

use oxrdf::{Literal, NamedNode, NamedOrBlankNode, GraphName, Quad};
use oxrdf::Dataset;

/// A NeuroStrata RDF Quad built on top of oxrdf::Quad.
/// This provides oxrdf-backed RDF quads that can be serialized with oxttl
/// and queried with the spargebra SPARQL engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OxRdfQuad {
    pub inner: Quad,
}

impl OxRdfQuad {
    /// Create a new oxrdf-backed quad.
    pub fn new(
        subject: impl Into<NamedOrBlankNode>,
        predicate: impl Into<NamedNode>,
        object: impl Into<oxrdf::Term>,
        graph_name: impl Into<GraphName>,
    ) -> Self {
        Self {
            inner: Quad::new(subject, predicate, object, graph_name),
        }
    }

    /// Get the underlying oxrdf Quad reference.
    pub fn as_quad(&self) -> &Quad {
        &self.inner
    }

    /// Convert to N-Quads string representation.
    pub fn to_nquads(&self) -> String {
        self.inner.to_string()
    }

    /// Create a quad from strings for use in projection logic.
    pub fn from_strings(
        subject: &str,
        predicate: &str,
        object_type: &str,
        object_value: &str,
        graph: Option<&str>,
    ) -> Result<Self, oxrdf::IriParseError> {
        let subject_nn = NamedNode::new_unchecked(subject.to_string());
        let predicate_nn = NamedNode::new_unchecked(predicate.to_string());
        let object = match object_type {
            "iri" => oxrdf::Term::NamedNode(NamedNode::new_unchecked(object_value.to_string())),
            _ => {
                let datatype = NamedNode::new("http://www.w3.org/2001/XMLSchema#string")?;
                oxrdf::Term::Literal(Literal::new_typed_literal(object_value.to_string(), datatype))
            }
        };
        let graph_name = match graph {
            Some(g) => GraphName::NamedNode(NamedNode::new_unchecked(g.to_string())),
            None => GraphName::DefaultGraph,
        };
        Ok(Self::new(subject_nn, predicate_nn, object, graph_name))
    }
}

impl From<OxRdfQuad> for Quad {
    fn from(q: OxRdfQuad) -> Self {
        q.inner
    }
}

/// An oxrdf-backed dataset for storing NeuroStrata RDF quads.
/// Wraps oxrdf::Dataset which provides quad storage and pattern matching.
#[derive(Debug, Default, Clone)]
pub struct OxRdfDataset {
    pub inner: Dataset,
}

impl OxRdfDataset {
    /// Create a new empty dataset.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a quad into the dataset.
    pub fn insert(&mut self, quad: OxRdfQuad) -> bool {
        self.inner.insert(&quad.inner)
    }

    /// Check if the dataset contains a quad.
    pub fn contains(&self, quad: &OxRdfQuad) -> bool {
        self.inner.contains(&quad.inner)
    }

    /// Number of quads in the dataset.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the dataset is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Convert dataset to N-Quads string.
    pub fn to_nquads(&self) -> String {
        self.inner
            .iter()
            .map(|q| q.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Bridge between oxrdf types and NeuroStrata's domain types.
/// This allows existing code using hand-written Quad/QuadObject types
/// to interoperate with oxrdf::Quad-based code.
pub struct OxRdfBridge;

impl OxRdfBridge {
    /// Convert a NeuroStrata hand-written Quad (from types.rs) to an OxRdfQuad.
    pub fn neurostrata_quad_to_oxrdf(
        quad: &crate::rdf::types::Quad,
    ) -> Result<OxRdfQuad, oxrdf::IriParseError> {
        let subject = NamedNode::new_unchecked(quad.subject.clone());
        let predicate = NamedNode::new_unchecked(quad.predicate.clone());
        let object = match &quad.object {
            crate::rdf::types::QuadObject::Iri(iri) => {
                oxrdf::Term::NamedNode(NamedNode::new_unchecked(iri.clone()))
            }
            crate::rdf::types::QuadObject::Literal { value, datatype, language } => {
                if let Some(dt) = datatype {
                    let datatype_nn = NamedNode::new(dt.clone())?;
                    match language {
                        Some(lang) => {
                            match Literal::new_language_tagged_literal(value.clone(), lang) {
                                Ok(lit) => oxrdf::Term::Literal(lit),
                                Err(_) => Literal::new_simple_literal(value.clone()).into(),
                            }
                        }
                        None => Literal::new_typed_literal(value.clone(), datatype_nn).into(),
                    }
                } else {
                    Literal::new_simple_literal(value.clone()).into()
                }
            }
        };
        let graph_name = match &quad.graph {
            Some(g) => GraphName::NamedNode(NamedNode::new_unchecked(g.clone())),
            None => GraphName::DefaultGraph,
        };
        Ok(OxRdfQuad::new(subject, predicate, object, graph_name))
    }

    /// Convert an OxRdfQuad to NeuroStrata's hand-written Quad.
    pub fn oxrdf_quad_to_neurostrata(quad: &OxRdfQuad) -> crate::rdf::types::Quad {
        use crate::rdf::types::{Quad as NSQuad, QuadObject, Tier};

        let subject = match &quad.inner.subject {
            NamedOrBlankNode::NamedNode(nn) => nn.to_string(),
            NamedOrBlankNode::BlankNode(bn) => format!("_:{}", bn),
        };
        let predicate = quad.inner.predicate.to_string();
        let object = match &quad.inner.object {
            oxrdf::Term::NamedNode(nn) => QuadObject::Iri(nn.to_string()),
            oxrdf::Term::Literal(lit) => QuadObject::Literal {
                value: lit.value().to_string(),
                datatype: Some(lit.datatype().to_string()),
                language: lit.language().map(|lang| lang.to_string()),
            },
            oxrdf::Term::BlankNode(bn) => QuadObject::Iri(format!("_:{}", bn)),
            oxrdf::Term::Triple(_) => {
                QuadObject::Iri("urn:neurostrata:triple".to_string())
            },
        };
        let graph = match &quad.inner.graph_name {
            GraphName::NamedNode(gn) => Some(gn.to_string()),
            GraphName::BlankNode(bg) => Some(format!("_:{}", bg)),
            GraphName::DefaultGraph => None,
        };
        NSQuad { subject, predicate, object, graph }
    }

    /// Convert NeuroStrata's Tier + namespace + generation to an oxrdf GraphName.
    pub fn tier_to_graph_name(
        tier: &crate::rdf::types::Tier,
        namespace: &str,
        generation: u64,
    ) -> GraphName {
        let iri = format!("urn:neurostrata:graph:{}:{}:gen:{}", namespace, tier.as_str(), generation);
        GraphName::NamedNode(NamedNode::new_unchecked(iri))
    }

    /// Convert NeuroStrata's graph IRI string to an oxrdf GraphName.
    pub fn graph_iri_to_graph_name(graph_iri: &str) -> GraphName {
        GraphName::NamedNode(NamedNode::new_unchecked(graph_iri.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oxrdf_quad_round_trip() {
        let ns_quad = crate::rdf::types::Quad {
            subject: "urn:neurostrata:memory:m1".to_string(),
            predicate: "urn:neurostrata:ns#content".to_string(),
            object: crate::rdf::types::QuadObject::Literal {
                value: "hello".to_string(),
                datatype: Some("http://www.w3.org/2001/XMLSchema#string".to_string()),
                language: None,
            },
            graph: Some("urn:neurostrata:graph:ns:domain:gen:0".to_string()),
        };

        let ox_quad = OxRdfBridge::neurostrata_quad_to_oxrdf(&ns_quad).unwrap();
        let back = OxRdfBridge::oxrdf_quad_to_neurostrata(&ox_quad);

        assert_eq!(back.subject, ns_quad.subject);
        assert_eq!(back.predicate, ns_quad.predicate);
        assert_eq!(back.graph, ns_quad.graph);
    }

    #[test]
    fn oxrdf_dataset_insert_and_len() {
        let mut ds = OxRdfDataset::new();
        let graph = OxRdfBridge::tier_to_graph_name(
            &crate::rdf::types::Tier::Domain,
            "ns",
            0,
        );
        let q = OxRdfQuad::from_strings(
            "urn:neurostrata:memory:m1",
            "urn:neurostrata:ns#content",
            "literal",
            "hello",
            Some(&graph.to_string()),
        ).unwrap();
        assert!(ds.insert(q));
        assert_eq!(ds.len(), 1);
        assert!(!ds.is_empty());
    }

    #[test]
    fn tier_to_graph_name_produces_valid_graph() {
        let graph_name = OxRdfBridge::tier_to_graph_name(
            &crate::rdf::types::Tier::Domain,
            "test_ns",
            1,
        );
        let s = graph_name.to_string();
        assert!(s.contains("urn:neurostrata:graph:test_ns:domain:gen:1"));
    }
}