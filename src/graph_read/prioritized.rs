//! Deterministic weighted ordering for `related` neighbours.
//!
//! The base traversal discovers neighbours breadth-first, which is
//! deterministic but uniform: a `GOVERNS` hop and a `RELATES_TO` hop at the
//! same depth tie. This module assigns the canonical per-relation weights from
//! the prioritized-edge-traversal spec and offers a `BinaryHeap` ordering so a
//! caller can rank a page by path authority without changing the traversal
//! itself.
//!
//! Weights are fixed constants (no LLM, no learning), and the ordering is
//! total: weight descending, then hop distance ascending, then id ascending.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Canonical edge priority (spec §4): `GOVERNS` is the strongest authority,
/// `DEPENDS_ON`/unknown relations are the weakest. `SUPERSEDES` sits between
/// governance and provenance so a supersession path outranks extraction.
pub fn edge_priority_weight(rel: &str) -> u32 {
    match rel {
        "GOVERNS" => 5,
        "SUPERSEDES" => 4,
        "EXTRACTED_FROM" => 3,
        "RELATES_TO" | "CONTAINS" => 2,
        "DEPENDS_ON" => 1,
        _ => 1,
    }
}

/// Weight of a whole path.
pub fn path_weight(relations: &[&str]) -> u32 {
    relations
        .iter()
        .map(|rel| edge_priority_weight(rel))
        .sum()
}

/// One ranked neighbour: the reached id, its hop distance from the seed, the
/// accumulated weight of the path that reached it, and the relation chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedItem {
    pub id: String,
    pub hop: usize,
    pub weight_sum: u32,
    #[serde(default)]
    pub path: Vec<String>,
}

impl RelatedItem {
    /// Build an item from the ordered relation chain that reached `id`.
    pub fn with_path(id: impl Into<String>, relations: &[&str]) -> Self {
        Self {
            id: id.into(),
            hop: relations.len(),
            weight_sum: path_weight(relations),
            path: relations.iter().map(|rel| (*rel).to_string()).collect(),
        }
    }

    /// Build an item from owned relation names (the store returns `String`s).
    pub fn with_relations(id: impl Into<String>, relations: &[String]) -> Self {
        let refs: Vec<&str> = relations.iter().map(String::as_str).collect();
        Self::with_path(id, &refs)
    }
}

impl Ord for RelatedItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Max-heap: strongest weight first, then the shortest path, then the
        // smallest id so equal-weight neighbours still order deterministically.
        self.weight_sum
            .cmp(&other.weight_sum)
            .then_with(|| other.hop.cmp(&self.hop))
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for RelatedItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Order neighbours by the canonical weight, dropping duplicate ids (the
/// strongest occurrence wins). The returned order is the production ranking.
pub fn prioritize_neighbors(items: Vec<RelatedItem>) -> Vec<RelatedItem> {
    let mut heap: BinaryHeap<RelatedItem> = BinaryHeap::with_capacity(items.len());
    for item in items {
        heap.push(item);
    }

    let mut ordered: Vec<RelatedItem> = Vec::with_capacity(heap.len());
    while let Some(item) = heap.pop() {
        if ordered.iter().any(|seen| seen.id == item.id) {
            continue;
        }
        ordered.push(item);
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_weights_match_the_spec() {
        assert_eq!(edge_priority_weight("GOVERNS"), 5);
        assert_eq!(edge_priority_weight("SUPERSEDES"), 4);
        assert_eq!(edge_priority_weight("EXTRACTED_FROM"), 3);
        assert_eq!(edge_priority_weight("RELATES_TO"), 2);
        assert_eq!(edge_priority_weight("CONTAINS"), 2);
        assert_eq!(edge_priority_weight("DEPENDS_ON"), 1);
        assert_eq!(edge_priority_weight("UNKNOWN"), 1);
    }

    #[test]
    fn path_weight_accumulates() {
        assert_eq!(path_weight(&["GOVERNS", "CONTAINS"]), 7);
        assert_eq!(path_weight(&[]), 0);
        assert_eq!(path_weight(&["UNKNOWN", "DEPENDS_ON"]), 2);
    }

    #[test]
    fn ordering_is_weight_then_hop_then_id() {
        let ordered = prioritize_neighbors(vec![
            RelatedItem::with_path("weak", &["DEPENDS_ON"]),
            RelatedItem::with_path("governed", &["GOVERNS"]),
            RelatedItem::with_path("near", &["CONTAINS"]),
            RelatedItem::with_path("far", &["CONTAINS", "DEPENDS_ON"]),
            RelatedItem::with_path("alpha", &["CONTAINS"]),
        ]);
        let ids: Vec<&str> = ordered.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["governed", "far", "alpha", "near", "weak"]);
    }

    #[test]
    fn duplicate_ids_keep_the_strongest_path() {
        let ordered = prioritize_neighbors(vec![
            RelatedItem::with_path("dup", &["DEPENDS_ON"]),
            RelatedItem::with_path("dup", &["GOVERNS"]),
        ]);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].weight_sum, 5);
        assert_eq!(ordered[0].path, vec!["GOVERNS"]);
    }

    #[test]
    fn owned_relations_and_str_relations_agree() {
        let owned = vec!["GOVERNS".to_string(), "CONTAINS".to_string()];
        assert_eq!(
            RelatedItem::with_relations("x", &owned),
            RelatedItem::with_path("x", &["GOVERNS", "CONTAINS"])
        );
    }
}
