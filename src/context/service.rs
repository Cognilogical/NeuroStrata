//! The shared context read-planning service.
//!
//! `ContextService` is the single entry point for building a [`ContextPlan`]
//! and rendering it. It is stateless: the caller wires whatever backends a
//! production plan needs before invoking it, so there are no capability flags
//! to toggle — a plan is always built the same way.

use super::model::{ContextPlan, ContextRequest, Degradation};

#[derive(Debug, Clone, Default)]
pub struct ContextService;

impl ContextService {
    pub fn new() -> Self {
        Self
    }

    /// An empty plan degrades to [`Degradation::Partial`]: the model
    /// guarantees an empty plan is never implicitly `Supported` (model.rs),
    /// because "nothing was planned" is a degraded outcome the caller must
    /// be able to observe.
    pub fn plan<R>(&self, request: R) -> ContextPlan
    where
        R: AsRef<ContextRequest>,
    {
        let req = request.as_ref();
        let mut plan = ContextPlan::new(req.event_id.clone(), Degradation::Partial);
        plan.applied_scope = String::new();
        plan.items = Vec::new();
        plan.truncation = false;
        plan
    }

    /// Render `plan` as compact text. Associated function (call as
    /// `ContextService::render(&plan)`).
    /// Render `plan` as compact text. Test-only — production renders via the
    /// free `render` re-exported from `context`.
    #[cfg(test)]
    pub fn render(plan: impl AsRef<ContextPlan>) -> String {
        super::render::render(plan.as_ref())
    }

    pub fn plan_with_items<R>(
        &self,
        request: R,
        candidates: Vec<super::model::ContextItem>,
        budget: super::plan::Budget,
        max_items: usize,
    ) -> ContextPlan
    where
        R: AsRef<ContextRequest>,
    {
        let mut plan = self.plan(request);
        let total = candidates.len();
        let mut deduped = super::plan::rank_dedup_budget(candidates, budget);
        if deduped.len() > max_items {
            deduped.truncate(max_items);
        }
        plan.truncation = total > deduped.len();
        plan.items = deduped;
        // Empty selection with candidates available is a degraded plan; a
        // non-empty selection is fully supported.
        plan.degradation = if !plan.items.is_empty() {
            Degradation::Supported
        } else {
            Degradation::Partial
        };
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::{ContextItem, ContextRequest, Degradation, EgressPolicy, SourceIdentity};
    use super::super::plan::Budget;

    fn request() -> ContextRequest {
        ContextRequest {
            session_id: "s".into(),
            context_epoch: 1,
            event_id: "evt".into(),
            trigger: "file_touch".into(),
            source: SourceIdentity::default(),
            policy: EgressPolicy::default(),
        }
    }

    fn item(id: &str, content: &str) -> ContextItem {
        ContextItem {
            id: id.into(),
            memory_type: "rule".into(),
            content: content.into(),
            freshness: None,
        }
    }

    #[test]
    fn plan_degrades_partial_when_empty() {
        let service = ContextService::new();
        let plan = service.plan(&request());
        assert_eq!(plan.degradation, Degradation::Partial);
        assert!(plan.items.is_empty());
        assert!(!plan.truncation);
    }

    #[test]
    fn plan_with_items_rank_dedups_by_id_and_content() {
        let service = ContextService::new();
        let candidates = vec![
            item("i1", "c1"),
            item("i1", "c1-dup-id"),
            item("i2", "c1"),
            item("i3", "c3"),
        ];
        let plan = service.plan_with_items(&request(), candidates, Budget::from(10_000usize), 10);
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0].id, "i1");
        assert_eq!(plan.items[1].id, "i3");
        // Drops by dedup/budget also count toward `truncation` (total input >
        // output), matching the long-standing contract.
        assert!(plan.truncation);
        assert_eq!(plan.degradation, Degradation::Supported);
    }

    #[test]
    fn plan_with_items_budgets_out_expensive_items() {
        let service = ContextService::new();
        let candidates = vec![item("a", &"x".repeat(1000)), item("b", "tiny")];
        // item "a" costs 1000/4+8 = 258 tokens > the 30-token budget; "b" fits.
        let plan = service.plan_with_items(&request(), candidates, Budget::from(30usize), 10);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].id, "b");
    }

    #[test]
    fn plan_with_items_truncates_to_max_items() {
        let service = ContextService::new();
        let candidates: Vec<ContextItem> = (0..5)
            .map(|i| item(&format!("i{}", i), &format!("content-{}", i)))
            .collect();
        let plan = service.plan_with_items(&request(), candidates, Budget::from(10_000usize), 3);
        assert_eq!(plan.items.len(), 3);
        assert!(plan.truncation);
    }

    #[test]
    fn render_works() {
        let plan = ContextPlan::new("evt", Degradation::Supported);
        let text = ContextService::render(&plan);
        assert!(text.contains("context-plan"));
    }
}
