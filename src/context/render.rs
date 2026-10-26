//! Compact rendering of a [`ContextPlan`] into text. No I/O: the text is derived
//! solely from the plan, so the adapter and the server can never disagree.

use super::model::ContextPlan;

/// Render `plan` as compact text with truncation/degradation markers.
pub fn render(plan: &ContextPlan) -> String {
    render_plan(plan)
}

pub fn render_plan(plan: &ContextPlan) -> String {
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
            if let Some(f) = &item.freshness {
                out.push_str(&format!(
                    " [source={}, semantic={}, coverage={}, lifecycle={}]",
                    f.source_currency, f.semantic_validation, f.index_coverage, f.decision_lifecycle
                ));
            }
            out.push('\n');
        }
    }
    if plan.truncation {
        out.push_str("… (truncated)\n");
    }
    out
}

/// Alias kept for callers that name it after the plan.
pub fn render_context_plan(plan: &ContextPlan) -> String {
    render(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::{ContextItem, ContextPlan, Degradation};

    #[test]
    fn render_marks_truncation() {
        let mut plan = ContextPlan::new("evt", Degradation::Partial);
        plan.items.push(ContextItem {
            id: "m1".into(),
            memory_type: "rule".into(),
            content: "use podman".into(),
            freshness: None,
        });
        plan.truncation = true;
        let text = render(&plan);
        assert!(text.contains("use podman"));
        assert!(text.contains("truncated"));
    }
}
