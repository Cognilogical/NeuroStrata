//! Bounded 1–2 hop traversal primitives for the `related` graph-read tool.
//!
//! Like `graph_read::memories_for_file`, this module is deliberately free of
//! `crate::` dependencies (serde + std only) so
//! `tests/graph_read/related_test.rs` can compile it from its real path and
//! pin the traversal contract without a library target -- the same pattern
//! `src/parser/generation.rs` uses.
//!
//! The store (`LadybugStore`) is responsible for fetching the bounded subgraph
//! and applying the row-class filters (retired / archived / mesh-conflicting /
//! private); this module owns the parts that must not drift between the store
//! and the contract tests: relation/direction allowlists, depth/limit/budget
//! clamping, cycle termination, edge-role assignment, cursor fingerprinting and
//! offset pagination.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// The four relations the traversal may follow (vocabulary v7).
pub const DEFAULT_RELATIONS: [&str; 4] = ["GOVERNS", "CONTAINS", "RELATES_TO", "EXTRACTED_FROM"];

/// The default and maximum hop counts (spec §4.2: default 1, max 2).
pub const DEFAULT_MAX_HOPS: usize = 1;
pub const MAX_HOPS: usize = 2;

/// The default and maximum node counts returned per page (spec §4.2: default
/// 10, max 25).
pub const DEFAULT_LIMIT: usize = 10;
pub const MAX_LIMIT: usize = 25;

/// The hard budgets the traversal may never exceed in one result (spec §5.3).
pub const MAX_NODES: usize = 20;
pub const MAX_EDGES: usize = 40;

/// How many characters of a node's content a `TraversalNode.snippet` keeps.
pub const SNIPPET_CHARS: usize = 200;

/// Which direction the traversal follows relative to the frontier node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow stored edges away from the frontier (`source == frontier`).
    Outgoing,
    /// Follow stored edges toward the frontier (`target == frontier`).
    Incoming,
    /// Follow both orientations (the default).
    Both,
}

/// The semantic weight of an edge. `RELATES_TO` is supporting evidence only --
/// never governance authority (spec §4.2 / Review Focus).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRole {
    /// Incoming `GOVERNS`: the only authority.
    Governance,
    /// `CONTAINS`: structural containment.
    Structure,
    /// `RELATES_TO`: supporting adjacency, not authority.
    Supporting,
    /// `EXTRACTED_FROM`: provenance (memory ← task), evidence not authority.
    Extraction,
}

/// The role a relation plays in the traversal.
pub fn edge_role(relation: &str) -> EdgeRole {
    match relation {
        "GOVERNS" => EdgeRole::Governance,
        "CONTAINS" => EdgeRole::Structure,
        "EXTRACTED_FROM" => EdgeRole::Extraction,
        _ => EdgeRole::Supporting,
    }
}

/// `RELATES_TO` is the only undirected relation (vocabulary v7): an undirected
/// edge is followed from either endpoint regardless of the requested direction.
pub fn is_undirected(relation: &str) -> bool {
    relation == "RELATES_TO"
}

/// True only for a relation in the traversal allowlist. Anything else is never
/// followed (and never interpolated into a query by the store).
pub fn is_allowed_relation(relation: &str) -> bool {
    DEFAULT_RELATIONS.contains(&relation)
}

/// Parse a caller-supplied direction, defaulting to `Both`.
pub fn parse_direction(raw: Option<&str>) -> Direction {
    match raw {
        Some("outgoing") => Direction::Outgoing,
        Some("incoming") => Direction::Incoming,
        _ => Direction::Both,
    }
}

/// Clamp a hop count into `[1, MAX_HOPS]`.
pub fn clamp_max_hops(hops: usize) -> usize {
    hops.clamp(1, MAX_HOPS)
}

/// Clamp a page limit into `[1, MAX_LIMIT]`; `0` (unset) reads as the default.
pub fn clamp_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit.min(MAX_LIMIT)
    }
}

/// A stable FNV-1a 64-bit fingerprint over the query identity. A cursor only
/// continues a page when this fingerprint AND the generation still match, so a
/// caller can never resume a stale traversal after the query or the data moved.
pub fn query_fingerprint(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        // A separator byte so ["a","bc"] and ["ab","c"] fingerprint differently.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Truncate content to `max_chars` characters, appending an ellipsis only when
/// characters were actually dropped.
pub fn snippet_of(content: &str, max_chars: usize) -> String {
    let mut chars = content.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// One node the traversal surfaced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalNode {
    pub id: String,
    pub memory_type: String,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
}

/// One directed edge the traversal crossed, carrying its semantic role and the
/// depth of the frontier node it left.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalEdge {
    pub source: String,
    pub relation: String,
    pub target: String,
    pub role: EdgeRole,
    pub depth: usize,
}

/// The chain of edges that led the traversal to one node. `kind` is `"direct"`
/// for the seed and `"path"` otherwise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePath {
    pub kind: String,
    pub path: Vec<TraversalEdge>,
    pub explanation: String,
}

/// A pagination cursor: the query fingerprint plus the generation the query ran
/// against, and how many nodes earlier pages already returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    pub fingerprint: String,
    pub generation: u64,
    pub offset: usize,
}

/// The bounded-traversal result (spec §4.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedResult {
    pub nodes: Vec<TraversalNode>,
    pub edges: Vec<TraversalEdge>,
    #[serde(default)]
    pub evidence: Vec<EvidencePath>,
    /// Nodes discovered across all pages (before `limit` is applied).
    pub total: usize,
    /// True when a budget (`MAX_NODES`, `MAX_EDGES`, or the page `limit`) was
    /// exhausted, so the caller must not mistake the page for the whole graph.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
}

/// One in-memory node the store hands to the traversal. `visible` is false for
/// rows the store already filtered out (retired / archived / mesh-conflicting /
/// private): those are never returned and never traversed through.
#[derive(Debug, Clone)]
pub struct NodeSpec {
    pub id: String,
    pub memory_type: String,
    pub content: String,
    pub domain: Option<String>,
    pub freshness: Option<String>,
    pub lifecycle: Option<String>,
    pub visible: bool,
}

/// One in-memory edge the store hands to the traversal.
#[derive(Debug, Clone)]
pub struct EdgeSpec {
    pub source: String,
    pub relation: String,
    pub target: String,
}

/// The bounded BFS core. It takes an in-memory subgraph (already row-filtered
/// by the store), performs a cycle-safe traversal out to `max_hops`, assigns
/// each crossed edge its semantic `role`, and applies `limit`/cursor pagination
/// over the deterministic discovery order.
#[allow(clippy::too_many_arguments)]
pub fn graph_read_related(
    seed: &str,
    nodes: &[NodeSpec],
    edges: &[EdgeSpec],
    relations: &[String],
    direction: Direction,
    max_hops: usize,
    memory_types: &[String],
    limit: usize,
    generation: u64,
    cursor: Option<&Cursor>,
) -> RelatedResult {
    let max_hops = clamp_max_hops(max_hops);
    let limit = clamp_limit(limit);

    let empty = || RelatedResult {
        nodes: Vec::new(),
        edges: Vec::new(),
        evidence: Vec::new(),
        total: 0,
        truncated: false,
        next_cursor: None,
    };

    let node_map: HashMap<&str, &NodeSpec> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let Some(&seed_node) = node_map.get(seed) else {
        return empty();
    };
    if !seed_node.visible {
        return empty();
    }

    // Relation allowlist: empty means the four defaults.
    let allowed: HashSet<&str> = if relations.is_empty() {
        DEFAULT_RELATIONS.iter().copied().collect()
    } else {
        relations
            .iter()
            .map(String::as_str)
            .filter(|r| is_allowed_relation(r))
            .collect()
    };
    let types: HashSet<&str> = memory_types.iter().map(String::as_str).collect();

    // Adjacency, honouring direction and the undirected RELATES_TO.
    let mut adj: HashMap<&str, Vec<(&str, &EdgeSpec)>> = HashMap::new();
    for edge in edges {
        if !allowed.contains(edge.relation.as_str()) {
            continue;
        }
        let follow_out =
            is_undirected(&edge.relation) || matches!(direction, Direction::Outgoing | Direction::Both);
        let follow_in =
            is_undirected(&edge.relation) || matches!(direction, Direction::Incoming | Direction::Both);
        if follow_out {
            adj.entry(edge.source.as_str())
                .or_default()
                .push((edge.target.as_str(), edge));
        }
        if follow_in {
            adj.entry(edge.target.as_str())
                .or_default()
                .push((edge.source.as_str(), edge));
        }
    }

    let mut visited: HashSet<&str> = HashSet::new(); // enqueued (cycle termination)
    let mut emitted: HashSet<&str> = HashSet::new(); // returned in `nodes`
    let mut edge_seen: HashSet<(&str, &str, &str)> = HashSet::new();
    let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
    let mut parent: HashMap<&str, (&str, &EdgeSpec, usize)> = HashMap::new();

    let mut all_nodes: Vec<TraversalNode> = Vec::new();
    let mut all_edges: Vec<TraversalEdge> = Vec::new();
    let mut truncated = false;

    queue.push_back((seed, 0));
    visited.insert(seed);

    while let Some((id, depth)) = queue.pop_front() {
        let Some(&node) = node_map.get(id) else { continue };
        if !node.visible {
            continue;
        }

        // The seed is always emitted; a non-seed node is emitted only when its
        // type passes the filter. Filtered-but-visible nodes are still traversed
        // through (structural bridges), which is why `emitted` is separate from
        // `visited`.
        let passes_type =
            id == seed || types.is_empty() || types.contains(node.memory_type.as_str());
        if passes_type && !emitted.contains(id) {
            emitted.insert(id);
            if all_nodes.len() >= MAX_NODES {
                truncated = true;
            } else {
                all_nodes.push(TraversalNode {
                    id: node.id.clone(),
                    memory_type: node.memory_type.clone(),
                    snippet: snippet_of(&node.content, SNIPPET_CHARS),
                    domain: node.domain.clone(),
                    freshness: node.freshness.clone(),
                    lifecycle: node.lifecycle.clone(),
                });
            }
        }

        if depth >= max_hops {
            continue;
        }

        let mut neighbors: Vec<(&str, &EdgeSpec)> = adj.get(id).cloned().unwrap_or_default();
        // Deterministic order: neighbour id, then relation, then the stored
        // edge endpoints -- so the same graph always pages identically.
        neighbors.sort_by(|a, b| {
            (a.0, a.1.relation.as_str(), a.1.source.as_str(), a.1.target.as_str()).cmp(&(
                b.0,
                b.1.relation.as_str(),
                b.1.source.as_str(),
                b.1.target.as_str(),
            ))
        });

        for (neighbor, es) in neighbors {
            let Some(&nnode) = node_map.get(neighbor) else { continue };
            // Privacy/retirement hard stop: an invisible neighbour is neither
            // returned nor crossed.
            if !nnode.visible {
                continue;
            }
            let key = (es.source.as_str(), es.relation.as_str(), es.target.as_str());
            if !edge_seen.contains(&key) {
                edge_seen.insert(key);
                if all_edges.len() >= MAX_EDGES {
                    truncated = true;
                } else {
                    all_edges.push(TraversalEdge {
                        source: es.source.clone(),
                        relation: es.relation.clone(),
                        target: es.target.clone(),
                        role: edge_role(&es.relation),
                        depth,
                    });
                }
            }
            if !visited.contains(neighbor) {
                visited.insert(neighbor);
                parent.entry(neighbor).or_insert((id, es, depth));
                queue.push_back((neighbor, depth + 1));
            }
        }
    }

    // Evidence paths: the seed is direct; every other emitted node carries the
    // chain of edges that led to it.
    let mut evidence: Vec<EvidencePath> = Vec::new();
    for node in &all_nodes {
        if node.id == seed {
            continue;
        }
        let mut path: Vec<TraversalEdge> = Vec::new();
        let mut cur = node.id.as_str();
        let mut hops = 0usize;
        while let Some(&(prev, es, es_depth)) = parent.get(cur) {
            path.push(TraversalEdge {
                source: es.source.clone(),
                relation: es.relation.clone(),
                target: es.target.clone(),
                role: edge_role(&es.relation),
                depth: es_depth,
            });
            hops += 1;
            cur = prev;
            if cur == seed || hops > max_hops {
                break;
            }
        }
        path.reverse();
        evidence.push(EvidencePath {
            kind: "path".to_string(),
            path,
            explanation: format!("reached in {hops} hop(s)"),
        });
    }

    // Pagination over the deterministic discovery order.
    let total = all_nodes.len();
    let fingerprint = query_fingerprint(&[
        seed,
        &relations.join(","),
        &format!("{direction:?}"),
        &max_hops.to_string(),
        &memory_types.join(","),
    ]);
    let offset = match cursor {
        Some(c) if c.fingerprint == fingerprint && c.generation == generation => c.offset,
        _ => 0,
    };
    let start = offset.min(total);
    let end = (offset + limit).min(total);
    let page = all_nodes[start..end].to_vec();
    let more = end < total;

    RelatedResult {
        nodes: page,
        edges: all_edges,
        evidence,
        total,
        truncated: truncated || more,
        next_cursor: if more {
            Some(Cursor {
                fingerprint,
                generation,
                offset: end,
            })
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, memory_type: &str, visible: bool) -> NodeSpec {
        NodeSpec {
            id: id.to_string(),
            memory_type: memory_type.to_string(),
            content: format!("content of {id}"),
            domain: None,
            freshness: None,
            lifecycle: None,
            visible,
        }
    }

    #[test]
    fn relation_allowlist_rejects_unknown_relations() {
        assert!(is_allowed_relation("GOVERNS"));
        assert!(is_allowed_relation("RELATES_TO"));
        assert!(!is_allowed_relation("OWNS"));
        assert!(!is_allowed_relation(""));
    }

    #[test]
    fn relates_to_is_the_only_undirected_relation() {
        assert!(is_undirected("RELATES_TO"));
        assert!(!is_undirected("GOVERNS"));
        assert!(!is_undirected("CONTAINS"));
        assert!(!is_undirected("EXTRACTED_FROM"));
    }

    #[test]
    fn edge_role_marks_relates_to_supporting_not_governance() {
        assert_eq!(edge_role("GOVERNS"), EdgeRole::Governance);
        assert_eq!(edge_role("CONTAINS"), EdgeRole::Structure);
        assert_eq!(edge_role("RELATES_TO"), EdgeRole::Supporting);
        assert_eq!(edge_role("EXTRACTED_FROM"), EdgeRole::Extraction);
    }

    #[test]
    fn clamps_bound_hops_and_limit() {
        assert_eq!(clamp_max_hops(0), 1);
        assert_eq!(clamp_max_hops(9), 2);
        assert_eq!(clamp_limit(0), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(1000), MAX_LIMIT);
        assert_eq!(clamp_limit(7), 7);
    }

    #[test]
    fn fingerprint_is_stable_and_distinguishes_queries() {
        assert_eq!(query_fingerprint(&["a", "b"]), query_fingerprint(&["a", "b"]));
        assert_ne!(query_fingerprint(&["a", "b"]), query_fingerprint(&["a", "c"]));
        assert_ne!(query_fingerprint(&["a", "bc"]), query_fingerprint(&["ab", "c"]));
    }

    #[test]
    fn snippet_truncates_and_marks() {
        assert_eq!(snippet_of("hello", 10), "hello");
        assert_eq!(snippet_of("hello world", 5), "hello…");
    }
}
