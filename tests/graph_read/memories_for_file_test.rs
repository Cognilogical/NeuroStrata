//! Pins the `memories_for_file` ranking contract without a library target.
//!
//! The crate ships a binary only (see `Cargo.toml`), so this test compiles the
//! crate-free `graph_read::memories_for_file` module from its real path -- the
//! same pattern `tests/ingest/staged_publication_test.rs` uses for the id/hash
//! helpers.

#[path = "../../src/graph_read/memories_for_file.rs"]
mod memories_for_file;

use memories_for_file::{
    freshness_priority, memory_type_priority, rank_hits, GraphMatchKind, MemoryHit,
    DEFAULT_LIMIT, DEFAULT_MEMORY_TYPES, MAX_LIMIT,
};
use serde_json::{json, Value};

fn hit(
    id: &str,
    kind: GraphMatchKind,
    memory_type: &str,
    access_count: i64,
    metadata: Value,
) -> MemoryHit {
    MemoryHit {
        id: id.to_string(),
        memory_type: memory_type.to_string(),
        content: format!("content of {}", id),
        match_kind: kind,
        access_count,
        explanation: String::new(),
        metadata,
    }
}

#[test]
fn match_kind_priority_orders_best_first() {
    assert!(
        GraphMatchKind::ExactSymbol.priority() < GraphMatchKind::ExactFile.priority()
    );
    assert!(GraphMatchKind::ExactFile.priority() < GraphMatchKind::SymbolTwoHop.priority());
    assert!(GraphMatchKind::SymbolTwoHop.priority() < GraphMatchKind::AncestorDirectory.priority());
    assert!(
        GraphMatchKind::AncestorDirectory.priority() < GraphMatchKind::MetadataFallback.priority()
    );
}

#[test]
fn memory_type_priority_ranks_enforced_rules_first() {
    let enforced = json!({ "enforcement": "ENFORCED" });
    let partial = json!({ "enforcement": "PARTIAL" });
    let shaped_fact = json!({ "topology": {} });
    let plain_fact = json!({});
    assert!(
        memory_type_priority("rule", &enforced) < memory_type_priority("rule", &partial)
    );
    assert!(
        memory_type_priority("rule", &partial) < memory_type_priority("fact", &shaped_fact)
    );
    assert!(
        memory_type_priority("fact", &shaped_fact)
            < memory_type_priority("procedure", &json!({}))
    );
    assert!(
        memory_type_priority("procedure", &json!({}))
            < memory_type_priority("fact", &plain_fact)
    );
}

#[test]
fn freshness_priority_ranks_verified_first() {
    assert!(
        freshness_priority(&json!({ "verification": { "status": "verified" } }))
            < freshness_priority(&json!({}))
    );
    assert!(
        freshness_priority(&json!({}))
            < freshness_priority(&json!({ "verification": { "status": "contradicted" } }))
    );
}

#[test]
fn rank_hits_orders_by_kind_then_type_then_access_count() {
    let mut hits = vec![
        hit(
            "ancestor-rule",
            GraphMatchKind::AncestorDirectory,
            "rule",
            100,
            json!({ "enforcement": "ENFORCED" }),
        ),
        hit("exact-fact", GraphMatchKind::ExactFile, "fact", 100, json!({})),
        hit(
            "exact-rule",
            GraphMatchKind::ExactFile,
            "rule",
            0,
            json!({ "enforcement": "ENFORCED" }),
        ),
    ];
    rank_hits(&mut hits);
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["exact-rule", "exact-fact", "ancestor-rule"]);
}

#[test]
fn rank_hits_breaks_ties_by_access_count_descending() {
    let mut hits = vec![
        hit(
            "a",
            GraphMatchKind::ExactFile,
            "rule",
            1,
            json!({ "enforcement": "ENFORCED" }),
        ),
        hit(
            "b",
            GraphMatchKind::ExactFile,
            "rule",
            99,
            json!({ "enforcement": "ENFORCED" }),
        ),
    ];
    rank_hits(&mut hits);
    assert_eq!(hits[0].id, "b");
    assert_eq!(hits[1].id, "a");
}

#[test]
fn constants_match_the_spec_defaults() {
    assert_eq!(DEFAULT_LIMIT, 5);
    assert_eq!(MAX_LIMIT, 20);
    assert_eq!(DEFAULT_MEMORY_TYPES, ["rule", "fact", "procedure"]);
}
