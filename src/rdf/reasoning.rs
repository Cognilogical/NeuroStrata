use crate::traits::{EvidenceEdge, EvidenceKind, MemoryPayload, SearchEvidence, SearchResult};
use crate::rdf::ontology::ClosureResult;
use crate::rdf::ontology::OntologyIndex;
use crate::rdf::ontology::QueryBudget;
use serde_json::json;
use std::collections::{BTreeSet, HashMap, VecDeque};

/// Configuration for deterministic reasoning.
#[derive(Debug, Clone, Copy)]
pub struct ReasoningConfig {
    pub governs_max_hops: u8,
    pub closure_budget: QueryBudget,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            governs_max_hops: 2,
            closure_budget: QueryBudget {
                max_ms: 50,
                max_nodes: 256,
            },
        }
    }
}

/// ReasoningAdapter — deterministic query-time RDFS/OWL 2 RL reasoning.
/// Bounded BFS via QueryBudget, no tableau reasoner.
pub struct ReasoningAdapter {
    pub config: ReasoningConfig,
    /// Payload lookup keyed by memory id, populated by the call site from the
    /// store so `execute` can hydrate inferred `SearchResult` rows with real
    /// `MemoryPayload`s instead of empty stubs.
    payloads: HashMap<String, MemoryPayload>,
}

impl ReasoningAdapter {
    pub fn new() -> Self {
        Self { config: ReasoningConfig::default(), payloads: HashMap::new() }
    }

    pub fn new_with(config: ReasoningConfig) -> Self {
        Self { config, payloads: HashMap::new() }
    }

    /// Builder: register a payload lookup map (id -> MemoryPayload).
    pub fn with_payloads(mut self, payloads: HashMap<String, MemoryPayload>) -> Self {
        self.payloads = payloads;
        self
    }

    /// Hydrate a payload for `id`, falling back to the empty stub.
    fn payload_for(&self, id: &str) -> MemoryPayload {
        self.payloads.get(id).cloned().unwrap_or_else(Self::empty_payload)
    }

    fn empty_payload() -> MemoryPayload {
        MemoryPayload {
            content: String::new(),
            user_id: String::new(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        }
    }

    /// Expand domain via subClassOf* closure (RDFS §5.3 query-time expansion).
    pub fn expand_domain(&self, ontology: &OntologyIndex, domain: &str) -> Vec<String> {
        if self.config.governs_max_hops == 0
            || self.config.closure_budget.max_nodes == 0
        {
            return vec![domain.to_string()];
        }
        ontology.expand_domain(domain)
    }

    /// Expand identifiers via exactMatch / altLabels equivalence closure.
    pub fn expand_identifiers(&self, ontology: &OntologyIndex, query: &str) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        out.insert(query.to_string());
        for e in ontology.equivalents_of(query) {
            out.insert(e);
        }
        for a in ontology.alt_labels_for(query) {
            out.insert(a);
        }
        for a in ontology.aliases_for(query) {
            out.insert(a);
        }
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort();
        v
    }

    /// Expand relation filters via subPropertyOf* closure.
    pub fn expand_relation(&self, ontology: &OntologyIndex, relation: &str) -> Vec<String> {
        ontology.expand_relation(relation)
    }

    /// Bounded transitive GOVERNS closure (default 2 hops, §5.5).
    pub fn closure_governs(&self, ontology: &OntologyIndex, root: &str, edges: &[String]) -> ClosureResult {
        // GOVERNS closure expands over subPropertyOf*: an edge carrying any
        // sub-relation of GOVERNS (e.g. a strict-governs variant) participates.
        let governed_rels: Vec<String> = self
            .expand_relation(ontology, "GOVERNS")
            .into_iter()
            .map(|r| r.to_lowercase())
            .collect();
        let mut result = ClosureResult::default();
        let max_hops = self.config.governs_max_hops as usize;
        let budget = self.config.closure_budget;
        let start = std::time::Instant::now();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut frontier: VecDeque<String> = VecDeque::new();
        let mut node_count = 0usize;

        // Hop 0: direct outgoing edges from root
        for edge in edges {
            let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
            if parts.len() < 2 || parts[0] != root {
                continue;
            }
            // 2-part edges carry no explicit relation; 3-part "src::tgt::rel" must be a
            // GOVERNS sub-relation (lowercase compare) to participate.
            let relates = parts.len() == 2
                || governed_rels.iter().any(|r| *r == parts[2].to_lowercase());
            if !relates {
                continue;
            }
            let target = &parts[1];
            if !visited.contains(target) {
                visited.insert(target.clone());
                result.results.push(target.clone());
                result.evidence.push(SearchEvidence {
                    kind: EvidenceKind::Governs2hop,
                    path: vec![EvidenceEdge {
                        source: root.to_string(),
                        relation: "governs".to_string(),
                        target: target.clone(),
                    }],
                    explanation: format!("GOVERNS {}::{}", root, target),
                    derivation: vec!["reasoning".to_string()],
                });
                // Only unvisited targets seed the frontier; re-pushing an
                // already-visited node wastes a BFS slot (AN-R3).
                frontier.push_back(target.clone());
            }
        }

        // BFS for further bounded transitive hops
        let mut current_hops = 1usize;
        while current_hops < max_hops
            && node_count < budget.max_nodes
            && (budget.max_ms == 0 || start.elapsed().as_millis() as u64 <= budget.max_ms)
        {
            let frontier_len = frontier.len();
            for _ in 0..frontier_len {
                let node = match frontier.pop_front() {
                    Some(n) => n,
                    None => break,
                };
                node_count += 1;
                for edge in edges {
                    let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
                    if parts.len() < 2 || parts[0] != node {
                        continue;
                    }
                    let relates = parts.len() == 2
                        || governed_rels.iter().any(|r| *r == parts[2].to_lowercase());
                    if !relates {
                        continue;
                    }
                    let target = &parts[1];
                    if !visited.contains(target) {
                        visited.insert(target.clone());
                        result.results.push(target.clone());
                        frontier.push_back(target.clone());
                        result.evidence.push(SearchEvidence {
                            kind: EvidenceKind::Governs2hop,
                            path: vec![EvidenceEdge {
                                source: node.clone(),
                                relation: "governs".to_string(),
                                target: target.clone(),
                            }],
                            explanation: format!("GOVERNS {}::{}", node, target),
                            derivation: vec!["reasoning".to_string()],
                        });
                    }
                }
            }
            current_hops += 1;
            if node_count >= budget.max_nodes {
                break;
            }
        }

        result
    }

    /// Execute reasoning-backed search expansion over domain, query, and edges.
    pub fn execute(
        &self,
        ontology: &OntologyIndex,
        domain: &str,
        query: &str,
        edges: &[String],
    ) -> Vec<SearchResult> {
        // Domain expansion via subClassOf*
        let expanded_domains = self.expand_domain(ontology, domain);
        let expanded_query = self.expand_identifiers(ontology, query);

        // Bounded GOVERNS closure over expanded domains; closure evidence is
        // attached to the SearchResult rows it discovered.
        let mut all_results: Vec<String> = Vec::new();
        let mut closure_evidence: HashMap<String, Vec<SearchEvidence>> = HashMap::new();
        for d in &expanded_domains {
            let closure = self.closure_governs(ontology, d, edges);
            for ev in closure.evidence {
                if let Some(target) = ev.path.first().map(|e| e.target.clone()) {
                    closure_evidence.entry(target).or_default().push(ev);
                }
            }
            for r in closure.results {
                all_results.push(r);
            }
        }

        // Build SearchResults
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<SearchResult> = Vec::new();
        for e in &expanded_query {
            if seen.insert(e.clone()) {
                out.push(SearchResult {
                    id: e.clone(),
                    score: 1.0,
                    payload: self.payload_for(e),
                    evidence: None,
                });
            }
        }
        for r in &all_results {
            if seen.insert(r.clone()) {
                out.push(SearchResult {
                    id: r.clone(),
                    score: 0.8,
                    payload: self.payload_for(r),
                    evidence: closure_evidence.get(r).cloned(),
                });
            }
        }
        // If nothing expanded, fall back to baseline
        if out.is_empty() {
            out.push(SearchResult {
                id: query.to_string(),
                score: 1.0,
                payload: self.payload_for(query),
                evidence: None,
            });
        }
        out
    }

    /// Production search-side domain expansion (§5.3): subclass-closure BFS,
    /// class/superclass resolution, equivalence, and identifier expansion.
    pub fn expand_domains_for_search(
        &self,
        index: &OntologyIndex,
        seed_domains: &[String],
    ) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for d in seed_domains {
            if d.is_empty() || !out.insert(d.clone()) {
                continue;
            }
            for r in index.closure_subclasses(d, &self.config.closure_budget).results {
                out.insert(r);
            }
            for p in index.expand_class(d) {
                out.insert(p);
            }
            for p in index.subclasses_of(d) {
                out.insert(p);
            }
            for p in index.superclasses_of(d) {
                out.insert(p);
            }
            for p in index.super_classes(d) {
                out.insert(p);
            }
            if let Some(e) = index.equivalent_of(d) {
                out.insert(e);
            }
            for e in index.equivalents_of(d) {
                out.insert(e);
            }
            for a in index.aliases_for(d) {
                out.insert(a);
            }
            for a in index.alt_labels_for(d) {
                out.insert(a);
            }
        }
        out
    }

    /// Production search-side relation expansion (§5.4): subPropertyOf* chain,
    /// direct subproperties, and inverses.
    pub fn expand_relations_for_search(
        &self,
        index: &OntologyIndex,
        rel: &str,
    ) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        out.insert(rel.to_string());
        for r in index.expand_relation(rel) {
            out.insert(r);
        }
        for r in index.subproperties_of(rel) {
            out.insert(r);
        }
        for r in index.inverse_of(rel) {
            out.insert(r);
        }
        // Transitive properties keep the seed relation across chains.
        if index.is_transitive(rel) {
            out.insert(rel.to_string());
        }
        out
    }

    /// SHACL validation of a payload against the index's loaded shapes.
    /// Returns no violations when no shapes are loaded.
    pub fn validate_payload(
        &self,
        index: &OntologyIndex,
        payload: &crate::traits::MemoryPayload,
    ) -> Vec<crate::rdf::shacl::ShaclViolation> {
        if index.shacl_shapes_loaded() > 0 {
            index.shacl_validate(payload)
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_adapter_expands_and_executes_within_budget() {
        let idx = OntologyIndex::from_registry_metadata(&json!({
            "domain_registry": {
                "domains": [
                    {"slug": "auth", "aliases": ["authn"]},
                    {"slug": "tls", "parent": "auth", "aliases": ["ssl"]}
                ]
            }
        }));
        let adapter = ReasoningAdapter::new_with(ReasoningConfig::default());
        assert!(adapter.expand_domain(&idx, "auth").contains(&"tls".to_string()));
        assert!(adapter.expand_identifiers(&idx, "ssl").contains(&"tls".to_string()));
        assert!(!adapter.expand_relation(&idx, "GOVERNS").is_empty());
        let results = adapter.execute(&idx, "auth", "ssl", &["tls::src/tls.rs".to_string()]);
        assert!(results.iter().any(|r| r.id == "src/tls.rs"));
    }

    #[test]
    fn reasoning_adapter_hydrates_registered_payloads() {
        let idx = OntologyIndex::from_registry_metadata(&json!({
            "domain_registry": {
                "domains": [
                    {"slug": "auth", "aliases": ["authn"]},
                    {"slug": "tls", "parent": "auth", "aliases": ["ssl"]}
                ]
            }
        }));
        let mut payloads: HashMap<String, MemoryPayload> = HashMap::new();
        payloads.insert(
            "src/tls.rs".to_string(),
            MemoryPayload {
                content: "TLS terminates at src/tls.rs".to_string(),
                user_id: "agent:bosun".to_string(),
                memory_type: "rule".to_string(),
                agent_name: Some("bosun".to_string()),
                location: "src/tls.rs".to_string(),
                location_lines: "1-10".to_string(),
                metadata: json!({"domain": "tls"}),
            },
        );
        let adapter = ReasoningAdapter::new_with(ReasoningConfig::default())
            .with_payloads(payloads);

        let results = adapter.execute(&idx, "auth", "ssl", &["tls::src/tls.rs".to_string()]);
        let hit = results
            .iter()
            .find(|r| r.id == "src/tls.rs")
            .expect("governs closure result must be present");
        assert_eq!(hit.payload.content, "TLS terminates at src/tls.rs");
        assert_eq!(hit.payload.memory_type, "rule");
        assert_eq!(hit.payload.user_id, "agent:bosun");

        // Unregistered ids still fall back to the empty stub.
        let miss = results
            .iter()
            .find(|r| r.id != "src/tls.rs")
            .expect("expanded query result must be present");
        assert_eq!(miss.payload.content, "");
        assert_eq!(miss.payload.memory_type, "fact");
    }
}
