//! Event-keyed receipt storage with idempotent replay.

use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Receipt {
    pub session_id: String,
    pub task_id: String,
    pub event_id: String,
    pub outcome: String,
    pub exposed_ids: Vec<String>,
    /// Worktree binding captured at receipt time (`task::receipt::capture`):
    /// where the work happened and what state the tree was in. An empty
    /// `worktree` means "not bound" — the receipt is verified on presence only.
    pub worktree: String,
    pub head_sha: String,
    pub dirty_tree_hash: String,
}

/// Key components are joined with `:`. A `:` or `\` inside a component is
/// escaped so distinct component tuples cannot collide on the same key
/// (AO-30): plain alphanumeric IDs still format as `"s:t:e"`.
fn key_component(component: &str) -> String {
    component.replace('\\', "\\\\").replace(':', "\\:")
}

impl Receipt {
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            key_component(&self.session_id),
            key_component(&self.task_id),
            key_component(&self.event_id)
        )
    }

    /// Same contract as `task::receipt::Receipt::is_current`: true when the
    /// stored worktree binding still matches the worktree's observed state.
    pub fn is_current(&self, head_sha: &str, dirty_tree_hash: &str) -> bool {
        self.head_sha == head_sha && self.dirty_tree_hash == dirty_tree_hash
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReceiptStore {
    inner: HashMap<String, Receipt>,
}

impl ReceiptStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a receipt. Returns true if newly inserted, false if a receipt
    /// with the same key already existed (idempotent replay).
    pub fn record(&mut self, receipt: Receipt) -> bool {
        let key = receipt.key();
        if self.inner.contains_key(&key) {
            return false;
        }
        self.inner.insert(key, receipt);
        true
    }

    pub fn get(&self, session_id: &str, task_id: &str, event_id: &str) -> Option<&Receipt> {
        let key = format!(
            "{}:{}:{}",
            key_component(session_id),
            key_component(task_id),
            key_component(event_id)
        );
        self.inner.get(&key)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent_replay() {
        let mut store = ReceiptStore::new();
        let r = Receipt {
            session_id: "s".into(),
            task_id: "t".into(),
            event_id: "e".into(),
            outcome: "ok".into(),
            exposed_ids: vec!["m1".into()],
            worktree: "/tmp/w".into(),
            head_sha: "sha".into(),
            dirty_tree_hash: "hash".into(),
        };
        assert!(store.record(r.clone()));
        assert!(!store.record(r));
        assert_eq!(store.len(), 1);
    }
}
