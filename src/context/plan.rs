//! Deterministic ranking, deduplication and budget application for a context plan.

use super::model::ContextItem;
use std::borrow::Borrow;
use std::collections::HashSet;

/// Cumulative budgets applied while building a plan. All three must be
/// respected: the plan stops as soon as any one of them is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub per_call: usize,
    pub per_turn: usize,
    pub per_session: usize,
}

impl Default for Budget {
    fn default() -> Self {
        // Per-trigger ceiling from spec §5.3.
        Budget {
            per_call: 512,
            per_turn: 1500,
            per_session: 1500,
        }
    }
}

impl From<usize> for Budget {
    fn from(value: usize) -> Self {
        Budget {
            per_call: value,
            per_turn: value,
            per_session: value,
        }
    }
}

impl From<(usize, usize, usize)> for Budget {
    fn from((per_call, per_turn, per_session): (usize, usize, usize)) -> Self {
        Budget {
            per_call,
            per_turn,
            per_session,
        }
    }
}

/// Estimate the token cost of one item (chars/4 + a small per-item overhead).
pub fn estimate_tokens(item: &ContextItem) -> usize {
    item.content.len() / 4 + 8
}

/// Deduplicate `items` by id and content, then keep as many as fit in `budget`,
/// respecting the cumulative per-call/turn/session ceilings. The input order is
/// preserved (callers rank first).
pub fn rank_dedup_budget<I, T>(items: I, budget: impl Into<Budget>) -> Vec<ContextItem>
where
    I: IntoIterator<Item = T>,
    T: Borrow<ContextItem>,
{
    let budget = budget.into();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_content: HashSet<String> = HashSet::new();
    let mut out: Vec<ContextItem> = Vec::new();
    let mut call_tokens = 0usize;
    let mut turn_tokens = 0usize;
    let mut session_tokens = 0usize;

    for borrowed in items {
        let item = borrowed.borrow().clone();
        if !seen_ids.insert(item.id.clone()) {
            continue;
        }
        if !seen_content.insert(item.content.clone()) {
            continue;
        }
        let cost = estimate_tokens(&item);
        if call_tokens + cost > budget.per_call
            || turn_tokens + cost > budget.per_turn
            || session_tokens + cost > budget.per_session
        {
            // Budget exhausted for this item; skip rather than truncate content,
            // so a plan never silently delivers a half-memory.
            continue;
        }
        call_tokens += cost;
        turn_tokens += cost;
        session_tokens += cost;
        out.push(item);
    }
    out
}

/// Alias for the same operation.
pub fn rank_and_dedup<I, T>(items: I, budget: impl Into<Budget>) -> Vec<ContextItem>
where
    I: IntoIterator<Item = T>,
    T: Borrow<ContextItem>,
{
    rank_dedup_budget(items, budget)
}

/// Alias kept for callers that name it after the budget application.
pub fn apply_budget<I, T>(items: I, budget: impl Into<Budget>) -> Vec<ContextItem>
where
    I: IntoIterator<Item = T>,
    T: Borrow<ContextItem>,
{
    rank_dedup_budget(items, budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::ContextItem;

    fn item(id: &str, content: &str) -> ContextItem {
        ContextItem {
            id: id.into(),
            memory_type: "rule".into(),
            content: content.into(),
            freshness: None,
        }
    }

    #[test]
    fn dedups_by_id() {
        let items = vec![item("a", "hello"), item("a", "hello2")];
        let out = rank_dedup_budget(items, 10_000usize);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content, "hello");
    }

    #[test]
    fn dedups_by_content() {
        let items = vec![item("a", "same"), item("b", "same")];
        let out = rank_dedup_budget(items, 10_000usize);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn budget_truncates() {
        let items = vec![item("a", &"x".repeat(1000)), item("b", "short")];
        let out = rank_dedup_budget(items, 10usize);
        assert!(out.len() <= 1);
    }
}
