//! AC2 — guard_validate preflight state machine (real behavioral tests).
//!
//! The FSM is a standalone pure module (`src/guard/fsm.rs`), so the tests
//! compile it directly from source — no whole-binary-crate import, no
//! filesystem-text greps.

#![allow(dead_code, unused_imports)]

#[path = "../../src/guard/fsm.rs"]
mod fsm;

use fsm::evaluate_fsm_guard;

#[test]
fn read_only_actions_always_permitted() {
    // No task claimed, Breath threshold exceeded: reads are still free.
    let (ok, msg) = evaluate_fsm_guard("s1", None, "w1", "read", false, 99);
    assert!(ok);
    assert!(msg.is_none());

    // A claimed task but mutating=false is likewise permitted.
    let (ok2, msg2) = evaluate_fsm_guard("s1", Some("t1"), "w1", "read", false, 0);
    assert!(ok2);
    assert!(msg2.is_none());

    // grep/glob/search are recognized read-only action types even with
    // is_mutating true (caller mislabeled): still permitted.
    for action in ["grep", "glob", "search"] {
        let (okx, _) = evaluate_fsm_guard("s1", None, "w1", action, true, 99);
        assert!(okx, "read-only action {} must be permitted", action);
    }
}

#[test]
fn mutating_action_without_task_claim_blocked_with_transition() {
    let (ok, msg) = evaluate_fsm_guard("s1", None, "w1", "bash rm", true, 0);
    assert!(!ok);
    let msg = msg.expect("blocked mutation must carry a message");
    assert!(msg.contains("Unpreflighted"), "got: {}", msg);
    assert!(
        msg.contains("claim -> ContextLoaded -> TaskClaimed"),
        "legal transition must be named, got: {msg}"
    );
}

#[test]
fn mutating_action_with_claim_below_breath_threshold_permitted() {
    let (ok, msg) = evaluate_fsm_guard("s1", Some("t1"), "w1", "bash rm", true, 4);
    assert!(ok);
    assert!(msg.is_none());
}

#[test]
fn mutating_action_beyond_breath_threshold_blocked_with_transition() {
    let (ok, msg) = evaluate_fsm_guard("s1", Some("t1"), "w1", "bash rm", true, 5);
    assert!(!ok);
    let msg = msg.expect("breath-required must block with a message");
    assert!(msg.contains("BreathRequired"), "got: {}", msg);
    assert!(
        msg.contains("task_update(note) -> Mutating"),
        "legal transition must be named, got: {msg}"
    );
}
