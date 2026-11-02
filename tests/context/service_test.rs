//! Pins the shared context service contract without a library target.

#![allow(dead_code)]

#[path = "../../src/context/model.rs"]
mod model;
#[path = "../../src/context/source.rs"]
mod source;
#[path = "../../src/context/freshness.rs"]
mod freshness;
#[path = "../../src/context/plan.rs"]
mod plan;
#[path = "../../src/context/render.rs"]
mod render;
#[path = "../../src/context/service.rs"]
mod service;

use model::{ContextPlan, ContextRequest, Degradation, EgressPolicy, SourceIdentity};
use service::ContextService;

#[test]
fn plan_is_supported() {
    let service = ContextService::new();
    let request = ContextRequest {
        session_id: "s".into(),
        context_epoch: 1,
        event_id: "evt".into(),
        trigger: "file_touch".into(),
        source: SourceIdentity::default(),
        policy: EgressPolicy::default(),
    };
    let plan = service.plan(&request);
    assert!(plan.items.is_empty());
    assert!(!plan.truncation);
    assert_eq!(plan.degradation, Degradation::Supported);
}

#[test]
fn plan_with_items_dedups_budgets_and_truncates() {
    let service = ContextService::new();
    let request = ContextRequest {
        session_id: "s".into(),
        context_epoch: 1,
        event_id: "evt".into(),
        trigger: "file_touch".into(),
        source: SourceIdentity::default(),
        policy: EgressPolicy::default(),
    };
    let mk = |id: &str, content: &str| model::ContextItem {
        id: id.into(),
        memory_type: "rule".into(),
        content: content.into(),
        freshness: None,
    };
    let candidates = vec![
        mk("i1", "content-1"),
        mk("i1", "dup-id"),
        mk("i2", "content-1"), // dup content for i1
        mk("i3", "content-3"),
        mk("i4", "content-4"),
    ];
    // Dedup by id/content leaves 3 items; max_items=2 truncates further.
    let plan = service.plan_with_items(&request, candidates, plan::Budget::from(10_000usize), 2);
    assert_eq!(plan.items.len(), 2);
    assert_eq!(plan.items[0].id, "i1");
    assert_eq!(plan.items[1].id, "i3");
    assert!(plan.truncation);
    assert_eq!(plan.degradation, Degradation::Supported);

    // A tight budget starves expensive items instead of half-delivering them.
    let candidates = vec![mk("big", &"x".repeat(1000)), mk("small", "tiny")];
    let plan = service.plan_with_items(&request, candidates, plan::Budget::from(30usize), 10);
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].id, "small");
    assert!(plan.truncation); // the out-of-budget item counts as dropped
}

#[test]
fn source_resolve_sets_relative_path() {
    let id = source::resolve("src/lib.rs", std::env::temp_dir().to_string_lossy().as_ref());
    assert_eq!(id.relative_path, "src/lib.rs");
}

#[test]
fn freshness_changed_hash() {
    let f = freshness::compute(&freshness::FreshnessInputs {
        has_governed_path: true,
        stored_file_sha256: Some("a".into()),
        current_file_sha256: Some("b".into()),
        ..Default::default()
    });
    assert_eq!(f.source_currency, model::SourceCurrency::Changed);
}

#[test]
fn plan_dedups_and_budgets() {
    use model::ContextItem;
    let items = vec![
        ContextItem { id: "a".into(), memory_type: "rule".into(), content: "hello".into(), freshness: None },
        ContextItem { id: "a".into(), memory_type: "rule".into(), content: "hello2".into(), freshness: None },
    ];
    let out = plan::rank_dedup_budget(items, 1000usize);
    assert_eq!(out.len(), 1);
}

#[test]
fn render_marks_truncation() {
    let mut plan = ContextPlan::new("evt", Degradation::Partial);
    plan.truncation = true;
    let text = render::render(&plan);
    assert!(text.contains("truncated"));
}
