//! Named-graph mapping for 3-tier stratification + ingest generations (Argon addition).
pub fn graph_iri(namespace: &str, tier: &str, generation: Option<u64>, mesh_origin: Option<&str>) -> String {
    let gen = generation.map(|g| format!("gen:{}", g)).unwrap_or_else(|| "gen:live".to_string());
    let origin = mesh_origin.unwrap_or("local");
    format!("urn:neurostrata:graph:{}:{}:{}:{}", namespace, tier, gen, origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds_iri() {
        let g = graph_iri("ns", "private", Some(3), Some("mesh"));
        assert!(g.contains("ns"));
        assert!(g.contains("gen:3"));
    }
    #[test]
    fn defaults() {
        let g = graph_iri("ns", "shared", None, None);
        assert!(g.contains("gen:live"));
        assert!(g.contains("local"));
    }
}
