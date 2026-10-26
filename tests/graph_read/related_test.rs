//! Pins the bounded `related` traversal contract without a library target.
//!
//! The crate ships a binary only (see `Cargo.toml`), so this test compiles the
//! crate-free `graph_read::related` module from its real path -- the same
//! pattern `tests/graph_read/memories_for_file_test.rs` uses. The store owns
//! fetching and row-class filtering; this module owns the traversal bounds,
//! cycle termination, edge-role assignment and cursor pagination, so those are
//! what is pinned here.

#[path = "../../src/graph_read/related.rs"]
mod related;

use related::{
    clamp_limit, clamp_max_hops, edge_role, graph_read_related, Direction, EdgeRole, EdgeSpec,
    NodeSpec, MAX_EDGES, MAX_NODES,
};

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

fn edge(source: &str, relation: &str, target: &str) -> EdgeSpec {
    EdgeSpec {
        source: source.to_string(),
        relation: relation.to_string(),
        target: target.to_string(),
    }
}

fn ids(result: &related::RelatedResult) -> Vec<&str> {
    result.nodes.iter().map(|n| n.id.as_str()).collect()
}

#[test]
fn depth_two_traversal_returns_nodes_and_edges() {
    let nodes = vec![
        node("A", "rule", true),
        node("B", "fact", true),
        node("C", "fact", true),
    ];
    let edges = vec![edge("A", "RELATES_TO", "B"), edge("B", "RELATES_TO", "C")];

    let result = graph_read_related("A", &nodes, &edges, &[], Direction::Both, 2, &[], 25, 0, None);

    // Depth-2 means the traversal reaches C through B.
    assert_eq!(ids(&result), vec!["A", "B", "C"]);
    assert_eq!(result.total, 3);
    // Both hops are returned as edges, the second one discovered at depth 1.
    assert_eq!(result.edges.len(), 2);
    assert!(result
        .edges
        .iter()
        .any(|e| e.depth == 1 && e.source == "B" && e.target == "C"));
}

#[test]
fn depth_one_stops_at_the_first_hop() {
    let nodes = vec![
        node("A", "rule", true),
        node("B", "fact", true),
        node("C", "fact", true),
    ];
    let edges = vec![edge("A", "RELATES_TO", "B"), edge("B", "RELATES_TO", "C")];

    let result = graph_read_related("A", &nodes, &edges, &[], Direction::Both, 1, &[], 25, 0, None);
    assert_eq!(ids(&result), vec!["A", "B"]);
    assert_eq!(result.total, 2);
}

#[test]
fn relates_to_is_supporting_never_governance() {
    // One seed with one edge of each relation, so every role is reachable.
    let nodes = vec![
        node("seed", "rule", true),
        node("governed", "fact", true),
        node("related", "fact", true),
        node("child", "file", true),
        node("extracted", "task", true),
    ];
    let edges = vec![
        edge("seed", "GOVERNS", "governed"),
        edge("seed", "RELATES_TO", "related"),
        edge("seed", "CONTAINS", "child"),
        edge("seed", "EXTRACTED_FROM", "extracted"),
    ];

    let result =
        graph_read_related("seed", &nodes, &edges, &[], Direction::Both, 1, &[], 25, 0, None);

    let role_of = |source: &str, target: &str| {
        result
            .edges
            .iter()
            .find(|e| e.source == source && e.target == target)
            .map(|e| e.role)
            .expect("edge present")
    };
    assert_eq!(role_of("seed", "governed"), EdgeRole::Governance);
    assert_eq!(role_of("seed", "related"), EdgeRole::Supporting);
    assert_eq!(role_of("seed", "child"), EdgeRole::Structure);
    assert_eq!(role_of("seed", "extracted"), EdgeRole::Extraction);
    assert_eq!(edge_role("RELATES_TO"), EdgeRole::Supporting);
    assert_ne!(EdgeRole::Supporting, EdgeRole::Governance);
}

#[test]
fn cursor_paginates_deterministically() {
    let nodes = vec![
        node("A", "rule", true),
        node("B", "fact", true),
        node("C", "fact", true),
        node("D", "fact", true),
        node("E", "fact", true),
    ];
    let edges = vec![
        edge("A", "RELATES_TO", "B"),
        edge("A", "RELATES_TO", "C"),
        edge("A", "RELATES_TO", "D"),
        edge("A", "RELATES_TO", "E"),
    ];

    let page1 = graph_read_related("A", &nodes, &edges, &[], Direction::Both, 1, &[], 2, 0, None);
    assert_eq!(ids(&page1), vec!["A", "B"]);
    assert_eq!(page1.total, 5);
    assert!(page1.truncated, "more pages remain, so truncated must be set");
    let cursor = page1.next_cursor.clone().expect("a next_cursor when pages remain");
    assert_eq!(cursor.offset, 2);

    let page2 =
        graph_read_related("A", &nodes, &edges, &[], Direction::Both, 1, &[], 2, 0, Some(&cursor));
    assert_eq!(ids(&page2), vec!["C", "D"]);

    let cursor = page2.next_cursor.clone().expect("a next_cursor when pages remain");
    let page3 =
        graph_read_related("A", &nodes, &edges, &[], Direction::Both, 1, &[], 2, 0, Some(&cursor));
    assert_eq!(ids(&page3), vec!["E"]);
    assert!(page3.next_cursor.is_none(), "final page carries no cursor");
}

#[test]
fn stale_cursor_generation_restarts_from_the_first_page() {
    let nodes = vec![node("A", "rule", true), node("B", "fact", true), node("C", "fact", true)];
    let edges = vec![edge("A", "RELATES_TO", "B"), edge("A", "RELATES_TO", "C")];

    // A cursor from an older generation (or a different query) must not skip
    // nodes -- retirement/privacy are re-evaluated against the current graph.
    let stale = related::Cursor {
        fingerprint: "deadbeef".to_string(),
        generation: 1,
        offset: 10,
    };
    let result = graph_read_related(
        "A",
        &nodes,
        &edges,
        &[],
        Direction::Both,
        1,
        &[],
        25,
        0,
        Some(&stale),
    );
    assert_eq!(ids(&result), vec!["A", "B", "C"]);
}

#[test]
fn private_or_retired_nodes_are_not_returned_nor_crossed() {
    // C is invisible (retired/private): it must not appear, its incident edge
    // must not appear, and D behind it must not be reached through it.
    let nodes = vec![
        node("A", "rule", true),
        node("B", "fact", true),
        node("C", "fact", false),
        node("D", "fact", true),
    ];
    let edges = vec![
        edge("A", "RELATES_TO", "B"),
        edge("B", "RELATES_TO", "C"),
        edge("C", "RELATES_TO", "D"),
    ];

    let result = graph_read_related("A", &nodes, &edges, &[], Direction::Both, 2, &[], 25, 0, None);
    assert_eq!(ids(&result), vec!["A", "B"]);
    assert!(result
        .edges
        .iter()
        .all(|e| e.source != "C" && e.target != "C"));
    assert!(!result.nodes.iter().any(|n| n.id == "D"));
}

#[test]
fn cyclic_graph_terminates_within_budget() {
    // A triangle A -> B -> C -> A must terminate, not loop forever, and never
    // exceed the node budget.
    let nodes = vec![node("A", "rule", true), node("B", "fact", true), node("C", "fact", true)];
    let edges = vec![
        edge("A", "RELATES_TO", "B"),
        edge("B", "RELATES_TO", "C"),
        edge("C", "RELATES_TO", "A"),
    ];

    let result = graph_read_related("A", &nodes, &edges, &[], Direction::Both, 2, &[], 25, 0, None);
    assert_eq!(result.total, 3);
    assert!(result.total <= MAX_NODES);
    assert!(result.edges.len() <= MAX_EDGES);
}

#[test]
fn node_and_edge_budgets_are_enforced() {
    // 30 nodes in a fan from the seed: the traversal must cap at MAX_NODES and
    // flag truncation.
    let mut nodes = vec![node("A", "rule", true)];
    let mut edges = Vec::new();
    for i in 0..30 {
        let id = format!("N{i}");
        nodes.push(node(&id, "fact", true));
        edges.push(edge("A", "RELATES_TO", &id));
    }

    let result = graph_read_related("A", &nodes, &edges, &[], Direction::Both, 1, &[], 25, 0, None);
    assert_eq!(result.nodes.len(), MAX_NODES);
    assert!(result.truncated, "budget exhaustion must set truncated");
}

#[test]
fn clamps_and_direction_allowlist_hold() {
    assert_eq!(clamp_max_hops(0), 1);
    assert_eq!(clamp_max_hops(100), 2);
    assert_eq!(clamp_limit(0), 10);
    assert_eq!(clamp_limit(1000), 25);

    // Outgoing direction does not follow an incoming-only stored edge.
    let nodes = vec![node("A", "rule", true), node("B", "fact", true), node("C", "fact", true)];
    let edges = vec![edge("B", "GOVERNS", "A"), edge("A", "GOVERNS", "C")];
    let result =
        graph_read_related("A", &nodes, &edges, &[], Direction::Outgoing, 1, &[], 25, 0, None);
    // Only A -> C is outgoing from A; B -> A is incoming and must not be followed.
    assert_eq!(ids(&result), vec!["A", "C"]);
}
