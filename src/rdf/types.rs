/// RDF projection types for NeuroStrata's semantic layer.
///
/// Phase 1 keeps these as plain owned values with no dependency on an RDF
/// crate. Later phases may replace them with `oxrdf` types behind a thin
/// adapter, but the projection logic should not depend on that choice.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuadObject {
    Iri(String),
    Literal {
        value: String,
        datatype: Option<String>,
        language: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quad {
    pub subject: String,
    pub predicate: String,
    pub object: QuadObject,
    pub graph: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeProvenance {
    pub source: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub generation: Option<i64>,
    pub extractor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Global,
    Domain,
    Task,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Global => "global",
            Tier::Domain => "domain",
            Tier::Task => "task",
        }
    }
}