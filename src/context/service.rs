//! The shared context read-planning service.
//!
//! `ContextService` is the single entry point for building a [`ContextPlan`]
//! and rendering it. It consumes a `VectorStore`/`GraphRead` in production;
//! here it keeps an honest record of whether those capabilities are present
//! so an unsupported backend is reported as `Unsupported` rather than as an
//! empty `Supported` plan.

use super::model::{ContextPlan, ContextRequest, Degradation};
use std::marker::PhantomData;

#[derive(Debug, Clone, Default)]
pub struct ContextService {
    has_vector_store: bool,
    has_graph_read: bool,
    _phantom: PhantomData<()>,
}

impl ContextService {
    /// Construct from two optional capabilities. The booleans are derived from
    /// `Option::is_some`, so the service never claims a capability it was not
    /// given.
    pub fn new_with<V, G>(vector_store: Option<V>, graph_read: Option<G>) -> Self {
        ContextService {
            has_vector_store: vector_store.is_some(),
            has_graph_read: graph_read.is_some(),
            _phantom: PhantomData,
        }
    }

    /// A service plan is only full when BOTH capabilities are present: ranking
    /// needs the store, freshness needs the graph read. A missing half still
    /// yields a plan, but flagged `Unsupported`.
    pub fn plan<R>(&self, request: R) -> ContextPlan
    where
        R: AsRef<ContextRequest>,
    {
        let req = request.as_ref();
        let degradation = if self.has_graph_read && self.has_vector_store {
            Degradation::Supported
        } else {
            Degradation::Unsupported
        };
        let mut plan = ContextPlan::new(req.event_id.clone(), degradation);
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

    pub fn plan_with_items<R>(&self, request: R, candidates: Vec<super::model::ContextItem>, budget: super::plan::Budget, max_items: usize) -> ContextPlan
    where
        R: AsRef<ContextRequest>,
    {
        let mut plan = self.plan(request);
        if self.has_graph_read {
            let total = candidates.len();
            let mut deduped = super::plan::rank_dedup_budget(candidates, budget);
            if deduped.len() > max_items {
                deduped.truncate(max_items);
            }
            plan.truncation = total > deduped.len();
            plan.items = deduped;
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::{ContextPlan, ContextRequest, Degradation, EgressPolicy, SourceIdentity};

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

    #[test]
    fn plan_without_graph_read_is_unsupported() {
        let service = ContextService::default();
        let plan = service.plan(request());
        assert!(plan.items.is_empty());
        assert_eq!(plan.degradation, Degradation::Unsupported);
    }

    #[test]
    fn plan_with_graph_read_is_supported() {
        let service = ContextService::new_with(Some(1u8), Some(2u8));
        let plan = service.plan(&request());
        assert_eq!(plan.degradation, Degradation::Supported);
    }

    #[test]
    fn render_works() {
        let plan = ContextPlan::new("evt", Degradation::Unsupported);
        let text = ContextService::render(&plan);
        assert!(text.contains("context-plan"));
    }

    #[test]
    fn plan_with_items_budgets_and_populates_when_supported() {
        use super::super::model::{ContextItem};
        let service = ContextService::new_with(Some(1u8), Some(2u8));
        let req = request();
        let items = vec![
            ContextItem { id: "i1".into(), memory_type: "m".into(), content: "c1".into(), freshness: None },
        ];
        let plan = service.plan_with_items(&req, items, super::super::plan::Budget::from(1024usize), 10);
        assert_eq!(plan.items.len(), 1);
        assert!(!plan.truncation);
        assert_eq!(plan.degradation, Degradation::Supported);
    }
}
