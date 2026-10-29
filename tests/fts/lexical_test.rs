//! Contract tests for the in-house multi-field lexical engine.
//!
//! The crate ships a binary only, so the module is compiled from its real
//! source path -- the same pattern `tests/graph_read/related_test.rs` uses.
//! The module must stay `std`-only so the WASM crate can reuse it by path.

#[path = "../../src/fts/mod.rs"]
mod fts;

use fts::{Field, LexicalDoc, LexicalIndex, SearchOptions};

fn doc(namespace: &str, id: &str, content: &str) -> LexicalDoc {
    LexicalDoc::new(id, namespace, "fact", content)
}

fn search(index: &LexicalIndex, namespace: &str, query: &str) -> Vec<fts::LexicalHit> {
    index
        .search(namespace, query, &SearchOptions::default())
        .hits
}

#[test]
fn tokenize_whitespace_lowercases_and_strips_edge_punctuation() {
    assert_eq!(
        fts::tokenize_whitespace("Hello, (WORLD)!"),
        vec!["hello".to_string(), "world".to_string()]
    );
}

#[test]
fn tokenize_code_splits_paths_separators_and_camel_case() {
    assert_eq!(
        fts::tokenize_code("src/fts/Mod.rs::searchIndex_v2"),
        vec![
            "src".to_string(),
            "fts".to_string(),
            "mod".to_string(),
            "rs".to_string(),
            "search".to_string(),
            "index".to_string(),
            "v2".to_string(),
        ]
    );
}

#[test]
fn empty_query_returns_no_hits() {
    let mut index = LexicalIndex::new();
    index.add(doc("ns", "a", "hello world"));
    assert!(search(&index, "ns", "   ").is_empty());
}

#[test]
fn symbol_matches_outrank_content_matches() {
    let mut index = LexicalIndex::new();
    let mut symbol_doc = doc("ns", "symbol", "aaa bbb");
    symbol_doc.symbol = "target".to_string();
    let content_doc = doc("ns", "content", "target aaa bbb");
    index.add(symbol_doc);
    index.add(content_doc);

    let hits = search(&index, "ns", "target");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "symbol");
}

#[test]
fn phrase_is_a_hard_adjacency_filter() {
    let mut index = LexicalIndex::new();
    index.add(doc("ns", "adjacent", "alpha beta gamma"));
    index.add(doc("ns", "reversed", "beta alpha gamma"));

    let hits = search(&index, "ns", "\"alpha beta\"");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "adjacent");
}

#[test]
fn fuzzy_matching_is_bounded_and_decays_below_exact() {
    let mut index = LexicalIndex::new();
    index.add(doc("ns", "exact", "color"));
    index.add(doc("ns", "fuzzy", "colour"));

    let hits = search(&index, "ns", "color~1");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "exact");
    assert_eq!(hits[1].id, "fuzzy");
    assert!(hits[0].score <= hits[1].score);
}

#[test]
fn namespace_isolation_is_absolute() {
    let mut index = LexicalIndex::new();
    index.add(doc("one", "a", "shared word"));
    index.add(doc("two", "b", "shared word"));

    let hits = search(&index, "one", "shared");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a");
}

#[test]
fn row_class_filters_hide_tasks_archived_retired_registry_and_conflicts() {
    let mut index = LexicalIndex::new();
    index.add(LexicalDoc::new("task", "ns", "task", "needle"));
    let mut archived = doc("ns", "archived", "needle");
    archived.archived = true;
    index.add(archived);
    let mut retired = doc("ns", "retired", "needle");
    retired.valid_to = Some(10);
    index.add(retired);
    let mut registry = doc("ns", "registry", "needle");
    registry.registry = true;
    index.add(registry);
    let mut conflict = doc("ns", "conflict", "needle");
    conflict.conflicting = true;
    index.add(conflict);
    let mut structural = doc("ns", "structural", "needle");
    structural.structural = true;
    index.add(structural);
    index.add(doc("ns", "live", "needle"));

    let opts = SearchOptions {
        now: 20,
        ..SearchOptions::default()
    };
    let hits = index.search("ns", "needle", &opts).hits;
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(hits.len(), 1, "visible rows: {ids:?}");
    assert_eq!(hits[0].id, "live");

    let opts = SearchOptions {
        now: 20,
        include_tasks: true,
        include_archived: true,
        ..SearchOptions::default()
    };
    let all = index.search("ns", "needle", &opts).hits;
    assert_eq!(all.len(), 3, "task, archived and live; retired/registry/conflict/structural stay out");
}

#[test]
fn as_of_uses_validity_window() {
    let mut index = LexicalIndex::new();
    let mut historical = doc("ns", "historical", "needle");
    historical.valid_from = Some(1);
    historical.valid_to = Some(10);
    index.add(historical);

    let now = SearchOptions {
        now: 20,
        ..SearchOptions::default()
    };
    assert!(index.search("ns", "needle", &now).hits.is_empty());

    let as_of = SearchOptions {
        now: 20,
        as_of: Some(5),
        ..SearchOptions::default()
    };
    assert_eq!(index.search("ns", "needle", &as_of).hits.len(), 1);
}

#[test]
fn acl_restricts_visible_to_principals() {
    let mut index = LexicalIndex::new();
    let mut restricted = doc("ns", "restricted", "needle");
    restricted.user_id = "owner".to_string();
    restricted.visible_to = vec!["alice".to_string()];
    index.add(restricted);

    let bob = SearchOptions {
        principal: "bob".to_string(),
        ..SearchOptions::default()
    };
    assert!(index.search("ns", "needle", &bob).hits.is_empty());

    let alice = SearchOptions {
        principal: "alice".to_string(),
        ..SearchOptions::default()
    };
    assert_eq!(index.search("ns", "needle", &alice).hits.len(), 1);
}

#[test]
fn results_are_distance_sorted_and_deterministic_on_ties() {
    let mut index = LexicalIndex::new();
    index.add(doc("ns", "b", "same"));
    index.add(doc("ns", "a", "same"));
    index.add(doc("ns", "c", "same same same"));

    let hits = search(&index, "ns", "same");
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|h| (0.0..=1.0).contains(&h.score)));
    assert_eq!(hits[0].score, 0.0);
    assert_eq!(hits[1].id, "a");
    assert_eq!(hits[2].id, "b");
}

#[test]
fn add_replaces_an_existing_id_and_remove_deletes_it() {
    let mut index = LexicalIndex::new();
    index.add(doc("ns", "a", "oldterm"));
    index.add(doc("ns", "a", "newterm"));

    assert_eq!(index.len(), 1);
    assert!(search(&index, "ns", "oldterm").is_empty());
    assert_eq!(search(&index, "ns", "newterm").len(), 1);

    index.remove("ns", "a");
    assert!(index.is_empty());
}

#[test]
fn domain_filter_falls_back_and_reports_the_skip() {
    let mut index = LexicalIndex::new();
    let mut matching = doc("ns", "matching", "needle");
    matching.domains = vec!["auth".to_string()];
    let mut other = doc("ns", "other", "needle");
    other.domains = vec!["data".to_string()];
    index.add(matching);
    index.add(other);

    let filtered = SearchOptions {
        domains: vec!["auth".to_string()],
        ..SearchOptions::default()
    };
    let outcome = index.search("ns", "needle", &filtered);
    assert!(outcome.domain_filter_applied);
    assert_eq!(outcome.hits.len(), 1);
    assert_eq!(outcome.hits[0].id, "matching");

    let missing = SearchOptions {
        domains: vec!["no-such-domain".to_string()],
        ..SearchOptions::default()
    };
    let outcome = index.search("ns", "needle", &missing);
    assert!(!outcome.domain_filter_applied);
    assert_eq!(outcome.hits.len(), 2);
}

#[test]
fn field_weights_are_the_documented_defaults() {
    let index = LexicalIndex::new();
    assert_eq!(index.namespaces.len(), 0);
    assert_eq!(fts::DEFAULT_FIELD_WEIGHTS, [1.0, 2.0, 1.5]);
    assert_eq!(Field::Content, Field::Content);
}
