#![allow(dead_code)]

#[path = "../../src/context/model.rs"]
mod model;
#[path = "../../src/context/freshness.rs"]
mod freshness;

use freshness::FreshnessInputs;
use model::{Coverage, DecisionLifecycle, SemanticValidation, SourceCurrency};

#[test]
fn changed_hash_means_changed() {
    let f = freshness::compute(&FreshnessInputs {
        has_governed_path: true,
        stored_file_sha256: Some("a".into()),
        current_file_sha256: Some("b".into()),
        ..Default::default()
    });
    assert_eq!(f.source_currency, SourceCurrency::Changed);
}

#[test]
fn decision_lifecycle_independent() {
    let f = freshness::compute(&FreshnessInputs {
        has_governed_path: true,
        stored_file_sha256: Some("a".into()),
        current_file_sha256: Some("b".into()),
        decision_status: Some("accepted".into()),
        ..Default::default()
    });
    assert_eq!(f.source_currency, SourceCurrency::Changed);
    assert_eq!(f.decision_lifecycle, DecisionLifecycle::Accepted);
}

#[test]
fn not_applicable_distinct() {
    let na = freshness::compute(&FreshnessInputs { has_governed_path: false, ..Default::default() });
    let un = freshness::compute(&FreshnessInputs { has_governed_path: true, ..Default::default() });
    assert_ne!(na.source_currency, un.source_currency);
    assert_eq!(na.source_currency, SourceCurrency::NotApplicable);
    assert_eq!(un.source_currency, SourceCurrency::Unknown);
}

#[test]
fn exclude_stale_definition() {
    use model::Freshness;
    let f = Freshness {
        source_currency: SourceCurrency::Changed,
        semantic_validation: SemanticValidation::Verified,
        index_coverage: Coverage::Complete,
        decision_lifecycle: DecisionLifecycle::Accepted,
    };
    assert!(!freshness::is_stale(&f));
    let f2 = Freshness {
        source_currency: SourceCurrency::Changed,
        semantic_validation: SemanticValidation::Unreviewed,
        ..f
    };
    assert!(freshness::is_stale(&f2));
}

#[test]
fn span_hash_authoritative() {
    let f = freshness::compute(&FreshnessInputs {
        has_governed_path: true,
        stored_file_sha256: Some("a".into()),
        current_file_sha256: Some("b".into()),
        stored_span_sha256: Some("s".into()),
        current_span_sha256: Some("s".into()),
        ..Default::default()
    });
    assert_eq!(f.source_currency, SourceCurrency::Unchanged);
}
