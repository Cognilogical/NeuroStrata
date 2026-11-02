//! Bounded attribute propagation for graph enrichment.
//!
//! Seeds carry sparse attributes (domain labels, source tags, extractor ids).
//! This module propagates a seed's domain across `GOVERNS` / `CONTAINS` hops,
//! decaying confidence per hop and stopping at an explicit horizon. The result
//! is transient: it is attached to the query response and never written back,
//! so it cannot contaminate later reads.
//!
//! The walk is deterministic (neighbours sorted by target then relation, seeds
//! sorted by node then domain) and bounded by `max_hops` / `min_confidence`.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph_read::related::EdgeSpec;

/// Bounds for one propagation pass. `max_hops` is clamped to `1..=5` at
/// evaluation time, per the spec.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PropagationConfig {
    pub max_hops: usize,
    pub min_confidence: f32,
    pub decay_per_hop: f32,
}

impl Default for PropagationConfig {
    fn default() -> Self {
        Self {
            max_hops: 3,
            min_confidence: 0.1,
            decay_per_hop: 0.5,
        }
    }
}

impl PropagationConfig {
    pub fn clamped(&self) -> Self {
        Self {
            max_hops: self.max_hops.clamp(1, 5),
            min_confidence: self.min_confidence.clamp(0.0, 1.0),
            decay_per_hop: self.decay_per_hop.clamp(0.0, 1.0),
        }
    }
}

/// One propagated domain label plus the evidence chain that carried it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropagatedTag {
    pub node_id: String,
    pub domain: String,
    pub confidence: f32,
    pub propagation_path: Vec<String>,
}

/// Relations an attribute may travel along (spec §4: domain labels travel
/// through governance and containment, never through adjacency alone).
fn is_propagation_relation(relation: &str) -> bool {
    relation == "GOVERNS" || relation == "CONTAINS"
}

/// Propagate every `(seed_id, domain)` pair outward from the seeds.
///
/// Returns seed tags too (confidence `1.0`, one-element path), followed by the
/// reached neighbours, sorted by `(node_id, domain)` so the response is
/// stable across runs.
pub fn propagate_domains(
    seeds: &[(String, String)],
    edges: &[EdgeSpec],
    config: &PropagationConfig,
) -> Vec<PropagatedTag> {
    let config = config.clamped();

    // Deterministic adjacency for the relations that may carry attributes.
    let mut adjacency: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for edge in edges {
        if !is_propagation_relation(&edge.relation) {
            continue;
        }
        adjacency
            .entry(edge.source.as_str())
            .or_default()
            .push((edge.target.as_str(), edge.relation.as_str()));
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort();
        neighbors.dedup();
    }

    let mut ordered_seeds: Vec<(&str, &str)> = seeds
        .iter()
        .map(|(node, domain)| (node.as_str(), domain.as_str()))
        .filter(|(node, domain)| !node.is_empty() && !domain.is_empty())
        .collect();
    ordered_seeds.sort();
    ordered_seeds.dedup();

    let mut visited: HashSet<(String, String)> = HashSet::new();
    let mut queue: VecDeque<(String, String, f32, Vec<String>)> = VecDeque::new();
    let mut tags: Vec<PropagatedTag> = Vec::new();

    for (node, domain) in ordered_seeds {
        let key = (node.to_string(), domain.to_string());
        if !visited.insert(key.clone()) {
            continue;
        }
        tags.push(PropagatedTag {
            node_id: key.0.clone(),
            domain: key.1.clone(),
            confidence: 1.0,
            propagation_path: vec![key.0.clone()],
        });
        queue.push_back((key.0, key.1, 1.0, queue_path(node)));
    }

    while let Some((node, domain, confidence, path)) = queue.pop_front() {
        // `path` includes the starting seed, so its length-1 is the hop count.
        if path.len().saturating_sub(1) >= config.max_hops {
            continue;
        }
        let Some(neighbors) = adjacency.get(node.as_str()) else {
            continue;
        };

        let next_confidence = confidence * config.decay_per_hop;
        if next_confidence < config.min_confidence {
            continue;
        }

        for (target, _relation) in neighbors {
            let key = ((*target).to_string(), domain.clone());
            if !visited.insert(key) {
                continue;
            }
            let mut next_path = path.clone();
            next_path.push((*target).to_string());
            tags.push(PropagatedTag {
                node_id: (*target).to_string(),
                domain: domain.clone(),
                confidence: next_confidence,
                propagation_path: next_path.clone(),
            });
            queue.push_back((
                (*target).to_string(),
                domain.clone(),
                next_confidence,
                next_path,
            ));
        }
    }

    tags.sort_by(|a, b| {
        a.node_id
            .cmp(&b.node_id)
            .then_with(|| a.domain.cmp(&b.domain))
    });
    tags
}

fn queue_path(node: &str) -> Vec<String> {
    vec![node.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(source: &str, relation: &str, target: &str) -> EdgeSpec {
        EdgeSpec {
            source: source.to_string(),
            relation: relation.to_string(),
            target: target.to_string(),
            valid_from: 0,
            valid_to: 0,
        }
    }

    fn seed(node: &str, domain: &str) -> (String, String) {
        (node.to_string(), domain.to_string())
    }

    #[test]
    fn propagates_through_governs_and_decays_per_hop() {
        let edges = vec![
            edge("rule", "GOVERNS", "file"),
            edge("file", "CONTAINS", "symbol"),
        ];
        let tags = propagate_domains(&[seed("rule", "auth")], &edges, &PropagationConfig::default());
        let file = tags.iter().find(|t| t.node_id == "file").unwrap();
        assert_eq!(file.domain, "auth");
        assert_eq!(file.confidence, 0.5);
        assert_eq!(file.propagation_path, vec!["rule", "file"]);

        let symbol = tags.iter().find(|t| t.node_id == "symbol").unwrap();
        assert_eq!(symbol.confidence, 0.25);
        assert_eq!(symbol.propagation_path, vec!["rule", "file", "symbol"]);
    }

    #[test]
    fn max_hops_bounds_the_walk() {
        let edges = vec![
            edge("a", "GOVERNS", "b"),
            edge("b", "GOVERNS", "c"),
            edge("c", "GOVERNS", "d"),
        ];
        let config = PropagationConfig {
            max_hops: 1,
            ..PropagationConfig::default()
        };
        let tags = propagate_domains(&[seed("a", "auth")], &edges, &config);
        let ids: Vec<&str> = tags.iter().map(|t| t.node_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn min_confidence_stops_low_value_hops() {
        let edges = vec![
            edge("a", "GOVERNS", "b"),
            edge("b", "GOVERNS", "c"),
        ];
        let config = PropagationConfig {
            max_hops: 5,
            min_confidence: 0.4,
            decay_per_hop: 0.5,
        };
        let tags = propagate_domains(&[seed("a", "auth")], &edges, &config);
        let ids: Vec<&str> = tags.iter().map(|t| t.node_id.as_str()).collect();
        // a=1.0, b=0.5, c=0.25 < 0.4
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn only_governance_and_containment_carry_attributes() {
        let edges = vec![
            edge("a", "RELATES_TO", "b"),
            edge("a", "DEPENDS_ON", "c"),
            edge("a", "EXTRACTED_FROM", "d"),
        ];
        let tags = propagate_domains(&[seed("a", "auth")], &edges, &PropagationConfig::default());
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].node_id, "a");
    }

    #[test]
    fn cycles_terminate_and_output_is_sorted() {
        let edges = vec![
            edge("a", "GOVERNS", "b"),
            edge("b", "GOVERNS", "a"),
            edge("a", "CONTAINS", "c"),
        ];
        let tags = propagate_domains(&[seed("a", "auth")], &edges, &PropagationConfig::default());
        let ids: Vec<&str> = tags.iter().map(|t| t.node_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn config_clamps_to_the_spec_range() {
        let clamped = PropagationConfig {
            max_hops: 99,
            min_confidence: -1.0,
            decay_per_hop: 9.0,
        }
        .clamped();
        assert_eq!(clamped.max_hops, 5);
        assert_eq!(clamped.min_confidence, 0.0);
        assert_eq!(clamped.decay_per_hop, 1.0);
    }
}
