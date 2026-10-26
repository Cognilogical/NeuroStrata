//! Event-keyed receipt storage with idempotent replay.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub session_id: String,
    pub task_id: String,
    pub event_id: String,
    pub outcome: String,
    pub exposed_ids: Vec<String>,
}

impl Receipt {
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.session_id, self.task_id, self.event_id)
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
        let key = format!("{}:{}:{}", session_id, task_id, event_id);
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
        };
        assert!(store.record(r.clone()));
        assert!(!store.record(r));
        assert_eq!(store.len(), 1);
    }
}
