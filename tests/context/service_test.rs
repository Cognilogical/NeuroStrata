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
fn plan_without_graph_read_is_unsupported() {
    let service = ContextService::default();
    let request = ContextRequest {
        session_id: "s".into(),
        context_epoch: 1,
        event_id: "evt".into(),
        trigger: "file_touch".into(),
        source: SourceIdentity::default(),
        policy: EgressPolicy::default(),
    };
    let plan = service.plan(request);
    assert!(plan.items.is_empty());
    assert_eq!(plan.degradation, Degradation::Unsupported);
}

#[test]
fn plan_with_graph_read_is_supported() {
    let service = ContextService::new_with(Some(1u8), Some(2u8));
    let request = ContextRequest {
        session_id: "s".into(),
        context_epoch: 1,
        event_id: "evt".into(),
        trigger: "file_touch".into(),
        source: SourceIdentity::default(),
        policy: EgressPolicy::default(),
    };
    let plan = service.plan(&request);
    assert_eq!(plan.degradation, Degradation::Supported);
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
