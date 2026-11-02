use super::types::Tier;

const NS: &str = "urn:neurostrata:ns#";

pub fn ns_predicate(name: &str) -> String {
    format!("{}{}", NS, name)
}

pub fn memory_iri(id: &str, concept_iri: Option<&str>) -> String {
    if let Some(c) = concept_iri.map(str::trim).filter(|s| !s.is_empty()) {
        if c.starts_with("urn:") || c.starts_with("http://") || c.starts_with("https://") {
            return c.to_string();
        }
    }
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