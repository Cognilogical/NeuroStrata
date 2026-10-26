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
    };
    s.record(r);
    assert!(s.get("s", "t", "e").is_some());
}
