//! R12: Recall completeness + exact-identifier lane verification.
//!
//! The crate ships a binary target only (`src/main.rs`), so an integration
//! test cannot import the module tree. The recall-closure state (the join
//! between a recall and its later promotion — the completeness half of R12)
//! lives in `src/recall_metrics.rs`, a module deliberately free of `crate::`
//! dependencies, and is compiled here from its real path: the behavior under
//! test is the file the binary ships, not a transcription.
//!
//! The exact-identifier/raw-distance SQL lane itself drives the real
//! LadybugDB store, so it is exercised by store-level unit tests; this target
//! additionally pins the *shipped source* of that lane against semantic
//! drift (exact `=` matching, deterministic distance ordering, the R16
//! decimal-array defense) rather than merely checking that files exist.

#![allow(dead_code)]

#[path = "../src/recall_metrics.rs"]
mod recall_metrics;

use recall_metrics::{
    record_domain_query, record_promotion, record_recall, recall_snapshot,
    domain_filter_snapshot,
};

/// The recall→promotion join: a promotion of an id that a previous recall in
/// the same namespace returned counts, exactly once; a promotion of an id
/// that was never recalled does not.
#[test]
fn promotion_joins_a_previous_recall() {
    let ns = "r12-join-ns";
    record_recall(ns, &["11111111-1111-1111-1111-111111111111".to_string()]);

    assert!(
        record_promotion(ns, "11111111-1111-1111-1111-111111111111"),
        "promotion of a recently recalled id must join the recall"
    );
    assert!(
        !record_promotion(ns, "11111111-1111-1111-1111-111111111111"),
        "the same id cannot join the recall twice: remember-in-recall removes it"
    );
    assert!(
        !record_promotion(ns, "22222222-2222-2222-2222-222222222222"),
        "an id that was never recalled must not join"
    );
    assert!(
        !record_promotion("r12-other-ns", "33333333-3333-3333-3333-333333333333"),
        "the join is per-namespace"
    );
}

/// An empty recall response (the lane returned zero candidates) feeds nothing
/// into the join state.
#[test]
fn empty_recall_response_records_nothing_joinable() {
    record_recall("r12-empty-ns", &[]);
    assert!(
        !record_promotion("r12-empty-ns", "44444444-4444-4444-4444-444444444444"),
        "empty recall must not poison the join state"
    );
}

/// Counters move with recorded events: `recall_promotions` only advances on a
/// successful join, and the domain-filter counters distinguish a narrowed
/// query from a fallback — the two open questions R12 measures.
#[test]
fn counters_reflect_recorded_events() {
    let before = recall_snapshot();
    let before_promoted = before["recall_promotions"].as_u64().unwrap_or(0);

    record_recall("r12-counters-ns", &["55555555-5555-5555-5555-555555555555".to_string()]);
    assert!(record_promotion("r12-counters-ns", "55555555-5555-5555-5555-555555555555"));

    let after = recall_snapshot();
    assert_eq!(
        after["recall_promotions"].as_u64().unwrap_or(0),
        before_promoted + 1,
        "one successful join increments recall_promotions by exactly one"
    );

    let dom_before = domain_filter_snapshot();
    record_domain_query(true, false, true);
    let dom_after = domain_filter_snapshot();
    assert_eq!(
        dom_after["domain_queries_total"].as_u64().unwrap_or(0),
        dom_before["domain_queries_total"].as_u64().unwrap_or(0) + 1,
    );
    assert_eq!(
        dom_after["domain_narrowed_hits"].as_u64().unwrap_or(0),
        dom_before["domain_narrowed_hits"].as_u64().unwrap_or(0) + 1,
    );
    assert_eq!(
        dom_after["domain_fallback_vector"].as_u64().unwrap_or(0),
        dom_before["domain_fallback_vector"].as_u64().unwrap_or(0),
        "a narrowed query must not count as a fallback"
    );
}

/// The shipped source of the exact-identifier/raw-distance lane carries its
/// semantic contract: exact `=` content matching (not a LIKE/prefix match),
/// raw-distance ASC ordering with a deterministic tie-break on id, and the R16
/// integer-array decimalization that keeps the ARRAY_DISTANCE binder valid.
#[test]
fn raw_distance_lane_source_carries_the_semantic_contract() {
    let ladybug = std::fs::read_to_string("src/store/ladybug.rs").expect("ladybug.rs readable");
    let traits = std::fs::read_to_string("src/traits.rs").expect("traits.rs readable");

    // Exact-identifier lane: equality matching, not substring matching.
    assert!(
        ladybug.contains("AND m.content = '{}'"),
        "exact-content dedup must match content by equality"
    );
    assert!(
        !ladybug.contains("m.content LIKE"),
        "the dedup lane must never degrade to LIKE matching"
    );

    // Raw-distance lane: ordered by the raw distance itself, ties broken by
    // the deterministic id ordering, never by a boosted score.
    assert!(
        ladybug.contains("ORDER BY dist ASC, m.id ASC"),
        "raw distance candidates must be ordered by raw distance ASC with deterministic id tie-break"
    );
    assert!(
        ladybug.contains("array_distance(m.embedding, "),
        "raw distance must be measured by the engine's array_distance, not recomputed"
    );

    // R16: a vector written into SQL is always decimal (0 → "0.0"), so the
    // ARRAY_DISTANCE binder never sees a pure-integer array.
    assert!(
        ladybug.contains("\".0\"") || ladybug.contains("\"0.0\""),
        "R16 decimalization of integer vector components must ship"
    );

    // The trait surface the lane implements.
    for name in [
        "nearest_by_raw_distance",
        "find_active_exact_content",
        "RawDistanceQuery",
        "RawDistanceCandidate",
        "ExactContentCandidate",
    ] {
        assert!(
            traits.contains(name),
            "traits.rs must declare the contract name: {name}"
        );
    }
}
