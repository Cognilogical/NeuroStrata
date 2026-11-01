//! AC5 budget verification / retention gate (Tasks 8-14 consolidated pass).
//! p95 <100ms verified for plan + upsert; receipt retention + Lock 2 gate enforced.

pub fn budget_verified(p95_plan_ms: f64, p95_upsert_ms: f64) -> bool {
    p95_plan_ms < 100.0 && p95_upsert_ms < 100.0
}

pub fn retention_gate(receipt_exists: bool) -> bool {
    receipt_exists // retention requires receipt present
}

/// AC5 for one recorded task receipt: the receipt must still be retrievable
/// from the store, and when the receipt carries a worktree binding its stored
/// (head_sha, dirty_tree_hash) must still match the worktree's current state —
/// a tree that moved or changed since the receipt was captured is stale.
///
/// An unbound receipt (`worktree` empty — e.g. recorded where no project root
/// is resolvable) verifies on presence, which is all `retention_gate` can
/// honestly assert about it.
pub fn verify_task_receipt_retention(
    store: &crate::receipts::store::ReceiptStore,
    session_id: &str,
    task_id: &str,
    event_id: &str,
) -> bool {
    let Some(stored) = store.get(session_id, task_id, event_id) else {
        return retention_gate(false); // failed retention: receipt is gone
    };
    if stored.worktree.is_empty() {
        return true; // presence-only receipt: nothing further to verify
    }
    let (head_sha, dirty_tree_hash) =
        crate::task::receipt::compute_worktree_state(std::path::Path::new(&stored.worktree));
    retention_gate(stored.is_current(&head_sha, &dirty_tree_hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{MemoryPayload, VectorStore};
    use crate::store::ladybug::LadybugStore;
    use std::time::Instant;

    fn p95_ms(mut samples_ms: Vec<f64>) -> f64 {
        samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((samples_ms.len() as f64) * 0.95).ceil() as usize - 1;
        samples_ms[idx.min(samples_ms.len() - 1)]
    }

    fn request() -> crate::context::ContextRequest {
        crate::context::ContextRequest {
            session_id: "ac5".to_string(),
            context_epoch: 0,
            event_id: "ac5".to_string(),
            trigger: String::new(),
            source: crate::context::SourceIdentity::default(),
            policy: crate::context::EgressPolicy::default(),
        }
    }

    fn items(n: usize) -> Vec<crate::context::ContextItem> {
        (0..n)
            .map(|i| crate::context::ContextItem {
                id: format!("ns::item-{}", i),
                memory_type: "rule".to_string(),
                content: format!("rule {}", i),
                freshness: None,
            })
            .collect()
    }

    /// The real p95 both budgets must be under 100ms: actual plan calls over a
    /// populated candidate set, and actual AC3-canonicalizing upserts.
    #[tokio::test]
    async fn ac5_real_context_plan_and_upsert_p95_under_100ms() {
        // -- ContextService::plan_with_items over 10 real items, 100 runs --
        let svc = crate::context::ContextService::new_with(Some(1u8), Some(2u8));
        let budget = crate::context::Budget::default();
        let mut plan_samples = Vec::<f64>::new();
        for _ in 0..100 {
            let start = Instant::now();
            let plan = svc.plan_with_items(request(), items(10), budget, 10);
            plan_samples.push(start.elapsed().as_secs_f64() * 1000.0);
            assert!(!plan.items.is_empty());
        }
        let p95_plan_ms = p95_ms(plan_samples);

        // -- LadybugStore::upsert with related_to metadata (AC3 canonicalization)
        //    over a real store, 20 calls --
        let dir = std::env::temp_dir().join(format!(
            "ac5-gate-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ));
        let store = LadybugStore::for_testing(&dir, 4)
            .expect("test store");
        store.init("ac5-ns").await.expect("init");
        for i in 0..10 {
            let id = format!("Ac5::src/lib{}.rs", i);
            store
                .upsert("ac5-ns", &id, vec![0.1; 4], MemoryPayload {
                    content: format!("node {}", i),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "file".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                })
                .await
                .expect("seed node");
        }

        let mut upsert_samples = Vec::<f64>::new();
        for i in 0..20 {
            let declaring = format!("ac5-rule-{}", i);
            let payload = MemoryPayload {
                content: format!("rule {}", i),
                user_id: "worker".to_string(),
                memory_type: "rule".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::json!({
                    // Legacy bare-path declarations the resolver must
                    // canonicalize while planning the edge (AC3 in the write
                    // path — exactly what the budget must afford).
                    "related_to": ["src/lib.rs", "src/lib3.rs"]
                }),
            };

            let start = Instant::now();
            store
                .upsert("ac5-ns", &declaring, vec![0.9; 4], payload)
                .await
                .expect("upsert");
            upsert_samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let p95_upsert_ms = p95_ms(upsert_samples);

        std::fs::remove_dir_all(&dir).ok();

        assert!(
            budget_verified(p95_plan_ms, p95_upsert_ms),
            "AC5 budget exceeded: p95_plan_ms={} p95_upsert_ms={}",
            p95_plan_ms,
            p95_upsert_ms
        );
    }

    #[test]
    fn budget_verified_ok() {
        assert!(budget_verified(50.0, 50.0));
    }
    #[test]
    fn retention_gate_requires_receipt() {
        assert!(retention_gate(true));
        assert!(!retention_gate(false));
    }

    #[test]
    fn verify_task_receipt_retention_needs_recorded_receipt() {
        let mut store = crate::receipts::store::ReceiptStore::new();
        assert!(!verify_task_receipt_retention(&store, "s", "t", "e"));
        store.record(crate::receipts::store::Receipt {
            session_id: "s".into(),
            task_id: "t".into(),
            event_id: "e".into(),
            outcome: "completed".into(),
            exposed_ids: vec![],
            worktree: String::new(),
            head_sha: String::new(),
            dirty_tree_hash: String::new(),
        });
        assert!(verify_task_receipt_retention(&store, "s", "t", "e"));
        assert!(!verify_task_receipt_retention(&store, "s", "t", "other"));
    }

    #[test]
    fn verify_task_receipt_retention_checks_worktree_binding_is_current() {
        // A receipt bound to a worktree that does not exist cannot verify
        // against any observed state.
        let mut store = crate::receipts::store::ReceiptStore::new();
        store.record(crate::receipts::store::Receipt {
            session_id: "s".into(),
            task_id: "t".into(),
            event_id: "e".into(),
            outcome: "completed".into(),
            exposed_ids: vec![],
            worktree: "/tmp/opencode/definitely-not-a-worktree".into(),
            head_sha: "head".into(),
            dirty_tree_hash: "hash".into(),
        });
        // Unreadable worktree reports ("none", "clean"), so a receipt bound to
        // anything else is stale.
        assert!(!verify_task_receipt_retention(&store, "s", "t", "e"));

        // An empty binding ("none", "clean") matches an unreadable worktree.
        store.record(crate::receipts::store::Receipt {
            session_id: "s".into(),
            task_id: "t".into(),
            event_id: "e2".into(),
            outcome: "completed".into(),
            exposed_ids: vec![],
            worktree: "/tmp/opencode/definitely-not-a-worktree".into(),
            head_sha: "none".into(),
            dirty_tree_hash: "clean".into(),
        });
        assert!(verify_task_receipt_retention(&store, "s", "t", "e2"));
    }
}
