use crate::traits::EvidenceEdge;
use crate::traits::EvidenceKind;
use crate::traits::MemoryPayload;
use crate::traits::SearchEvidence;
use crate::traits::SearchResult;
use crate::rdf::ontology::ClosureResult;
use crate::rdf::ontology::OntologyIndex;
use crate::rdf::ontology::QueryBudget;
use serde_json::json;
use std::collections::BTreeSet;
use std::collections::VecDeque;

/// Configuration for deterministic reasoning.
#[derive(Debug, Clone, Copy)]
pub struct ReasoningConfig {
    pub reasoning_enabled: bool,
    pub governs_max_hops: u8,
    pub closure_budget: QueryBudget,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            reasoning_enabled: true,
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
}

impl ReasoningAdapter {
    pub fn new() -> Self {
        Self { config: ReasoningConfig::default() }
    }

    pub fn new_with(config: ReasoningConfig) -> Self {
        Self { config }
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
        if !self.config.reasoning_enabled {
            return vec![domain.to_string()];
        }
        ontology.expand_domain(domain)
    }

    /// Expand identifiers via exactMatch / altLabels equivalence closure.
    pub fn expand_identifiers(&self, ontology: &OntologyIndex, query: &str) -> Vec<String> {
        if !self.config.reasoning_enabled {
            return vec![query.to_string()];
        }
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
        if !self.config.reasoning_enabled {
            return vec![relation.to_string()];
        }
        ontology.expand_relation(relation)
    }

    /// Bounded transitive GOVERNS closure (default 2 hops, §5.5).
    pub fn closure_governs(&self, ontology: &OntologyIndex, root: &str, edges: &[String]) -> ClosureResult {
        let _ = ontology;
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
            if parts.len() == 2 && parts[0] == root {
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
                }
                frontier.push_back(target.clone());
            }
        }

        // BFS for further bounded transitive hops
        let mut current_hops = 1usize;
        while current_hops < max_hops && node_count < budget.max_nodes && start.elapsed().as_millis() as u64 <= budget.max_ms {
            let frontier_len = frontier.len();
            for _ in 0..frontier_len {
                let node = match frontier.pop_front() {
                    Some(n) => n,
                    None => break,
                };
                node_count += 1;
                for edge in edges {
                    let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
                    if parts.len() == 2 && parts[0] == node {
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
        if !self.config.reasoning_enabled {
            return vec![SearchResult {
                id: query.to_string(),
                score: 1.0,
                payload: Self::empty_payload(),
                evidence: None,
            }];
        }

        // Domain expansion via subClassOf*
        let expanded_domains = self.expand_domain(ontology, domain);
        let expanded_query = self.expand_identifiers(ontology, query);

        // Bounded GOVERNS closure over expanded domains
        let mut all_results: Vec<String> = Vec::new();
        for d in &expanded_domains {
            let closure = self.closure_governs(ontology, d, edges);
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
                    payload: Self::empty_payload(),
                    evidence: None,
                });
            }
        }
        for r in &all_results {
            if seen.insert(r.clone()) {
                out.push(SearchResult {
                    id: r.clone(),
                    score: 0.8,
                    payload: Self::empty_payload(),
                    evidence: None,
                });
            }
        }
        // If nothing expanded, fall back to baseline
        if out.is_empty() {
            out.push(SearchResult {
                id: query.to_string(),
                score: 1.0,
                payload: Self::empty_payload(),
                evidence: None,
            });
        }
        out
    }
}
