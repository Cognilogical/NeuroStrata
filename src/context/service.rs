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
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Alias for `new_with`.
    pub fn from_capabilities<V, G>(vector_store: Option<V>, graph_read: Option<G>) -> Self {
        Self::new_with(vector_store, graph_read)
    }

    /// A service with no capabilities: every plan is `Unsupported`.
    pub fn unsupported() -> Self {
        Self::default()
    }

    pub fn without_graph_read() -> Self {
        Self::default()
    }

    pub fn without_capabilities() -> Self {
        Self::default()
    }

    pub fn new_unsupported() -> Self {
        Self::default()
    }

    pub fn has_graph_read(&self) -> bool {
        self.has_graph_read
    }

    pub fn has_vector_store(&self) -> bool {
        self.has_vector_store
    }

    pub fn is_supported(&self) -> bool {
        self.has_graph_read
    }

    pub fn plan<R>(&self, request: R) -> ContextPlan
    where
        R: AsRef<ContextRequest>,
    {
        let req = request.as_ref();
        let degradation = if self.has_graph_read {
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
    pub fn render(plan: impl AsRef<ContextPlan>) -> String {
        let plan = plan.as_ref();
        let mut out = String::new();
        out.push_str(&format!(
            "context-plan {} (generation {}, scope {}, degradation: {})\n",
            plan.event_id, plan.index_generation, plan.applied_scope, plan.degradation
        ));
        if plan.items.is_empty() {
            out.push_str("(no items)\n");
        } else {
            for item in &plan.items {
                out.push_str("- ");
                out.push_str(&item.memory_type);
                out.push_str(": ");
                out.push_str(&item.content);
                out.push('\n');
            }
        }
        if plan.truncation {
            out.push_str("… (truncated)\n");
        }
        out
    }

    pub fn render_plan(plan: impl AsRef<ContextPlan>) -> String {
        Self::render(plan)
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
}
