#[path = "../../src/search.rs"]
mod search;

use search::{SearchFilters, passes_filters};
use serde_json::json;

#[test]
fn excludes_code_ast_by_default() {
    let f = SearchFilters::default_filters();
    assert!(!search::type_allowed("code_ast", &f));
}

#[test]
fn fact_shape_filter() {
    let f = SearchFilters { fact_shape: Some("decision".into()), ..SearchFilters::default_filters() };
    let m = json!({"decision": {"choice":"x"}});
    assert!(search::fact_shape_matches(&m, "decision"));
    assert!(passes_filters("fact", &m, "src/lib.rs", &f));
    let m2 = json!({});
    assert!(!passes_filters("fact", &m2, "src/lib.rs", &f));
}

#[test]
fn path_prefix_filter() {
    let f = SearchFilters { path_prefix: Some("src/".into()), ..SearchFilters::default_filters() };
    assert!(passes_filters("rule", &json!({}), "src/lib.rs", &f));
    assert!(!passes_filters("rule", &json!({}), "tests/x.rs", &f));
}

#[test]
fn exclude_stale_filter() {
    let f = SearchFilters { exclude_stale: true, ..SearchFilters::default_filters() };
    let m = json!({"source_currency":"changed","verification":{"status":"unreviewed"}});
    assert!(!passes_filters("fact", &m, "src/lib.rs", &f));
    let m2 = json!({"source_currency":"changed","verification":{"status":"verified"}});
    assert!(passes_filters("fact", &m2, "src/lib.rs", &f));
}
