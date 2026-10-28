use super::types::Tier;

const NS: &str = "urn:neurostrata:ns#";

pub fn ns_predicate(name: &str) -> String {
    format!("{}{}", NS, name)
}

pub fn memory_iri(id: &str, _concept_iri: Option<&str>) -> String {
    format!("urn:neurostrata:memory:{}", id)
}

pub fn graph_iri(namespace: &str, tier: Tier, generation: u64) -> String {
    format!(
        "urn:neurostrata:graph:{}:{}:gen:{}",
        namespace,
        tier.as_str(),
        generation
    )
}

pub fn assertion_iri(namespace: &str, id: &str, version: u64) -> String {
    format!("urn:neurostrata:assertion:{}:{}:{}", namespace, id, version)
}

/// Map a LadybugDB relation type to its RDF predicate.
pub fn relation_predicate(rel_type: &str) -> String {
    let camel = match rel_type {
        "RELATES_TO" => "relatesTo",
        "CONTAINS" => "contains",
        "GOVERNS" => "governs",
        "EXTRACTED_FROM" => "extractedFrom",
        "DEPENDS_ON" => "dependsOn",
        other => other,
    };
    ns_predicate(camel)
}

/// Common memory field predicates.
pub fn field_predicate(field: &str) -> String {
    ns_predicate(field)
}