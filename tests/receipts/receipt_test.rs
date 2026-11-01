// The module is compiled wholesale from its real path, so prod-only methods
// are quiesced here (same pattern as tests/context/model_test.rs).
#![allow(dead_code)]

#[path = "../../src/receipts/store.rs"]
mod store;

use store::{Receipt, ReceiptStore};

#[test]
fn receipt_keyed_idempotent() {
    let mut s = ReceiptStore::new();
    let r = Receipt {
        session_id: "s".into(),
        task_id: "t".into(),
        event_id: "e".into(),
        outcome: "ok".into(),
        exposed_ids: vec![],
        ..Default::default()
    };
    assert!(s.record(r.clone()));
    assert!(!s.record(r));
}

#[test]
fn get_receipt() {
    let mut s = ReceiptStore::new();
    let r = Receipt {
        session_id: "s".into(),
        task_id: "t".into(),
        event_id: "e".into(),
        outcome: "ok".into(),
        exposed_ids: vec![],
        ..Default::default()
    };
    s.record(r);
    assert!(s.get("s", "t", "e").is_some());
}

#[test]
fn receipt_worktree_currency_check() {
    let r = Receipt {
        session_id: "s".into(),
        task_id: "t".into(),
        event_id: "e".into(),
        outcome: "ok".into(),
        exposed_ids: vec![],
        worktree: "/tmp/w".into(),
        head_sha: "sha1".into(),
        dirty_tree_hash: "hash1".into(),
    };
    assert!(r.is_current("sha1", "hash1"));
    // Same HEAD but a changed dirty-tree hash means the tree moved since the
    // receipt was captured.
    assert!(!r.is_current("sha1", "hash2"));
}
