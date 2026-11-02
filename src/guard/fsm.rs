//! AC2 — the task preflight state machine, as a standalone pure module.
//!
//! Kept free of any crate or filesystem dependencies so integration tests can
//! compile it directly (`#[path = "..."] mod fsm;`) without reaching the whole
//! binary crate.

/// The AC2 preflight decision for one mutating action, judged in order:
/// 1. non-mutating (or a known read-only action type) is always permitted;
/// 2. mutating without a claimed task is blocked ("Unpreflighted"), with the
///    next legal transition named;
/// 3. mutating with a claimed task but an empty session or worktree is a
///    broken preflight, blocked ("Unpreflighted") before the Breath check so
///    routing mistakes surface first;
/// 4. mutating with too many mutations since the last task_update note is
///    blocked ("BreathRequired", >= 5), with the next legal transition named.
pub fn evaluate_fsm_guard(
    session_id: &str,
    active_task_id: Option<&str>,
    worktree: &str,
    action_type: &str,
    is_mutating: bool,
    mutations_since_note: usize,
) -> (bool, Option<String>) {
    let action_lower = action_type.to_lowercase();
    if !is_mutating
        || matches!(
            action_lower.as_str(),
            "read" | "read_file" | "get" | "list" | "search" | "query" | "view" | "cat"
                | "inspect" | "grep" | "glob"
        )
    {
        return (true, None);
    }
    // Mutating without a task claim: block with the transition message.
    if active_task_id.is_none() {
        return (
            false,
            Some(format!(
                "Unpreflighted: claim task (session={}, worktree={}) before mutating ({}); next legal transition: claim -> ContextLoaded -> TaskClaimed",
                session_id, worktree, action_type
            )),
        );
    }
    // A claimed task without a session or a worktree is a broken preflight,
    // not a live claim: mutating work must say where and by whom it runs
    // (the MCP caller passes session_id + cwd here). Judged BEFORE the Breath
    // check so a broken/unroutable claim is reported as such even when the
    // Breath counter is also hot (AJ-17).
    if active_task_id.is_some() && (session_id.trim().is_empty() || worktree.trim().is_empty()) {
        return (
            false,
            Some(format!(
                "Unpreflighted: mutating action ({}) carries an empty session or worktree (session='{}', worktree='{}'); next legal transition: claim -> ContextLoaded -> TaskClaimed",
                action_type, session_id, worktree
            )),
        );
    }
    // The Breath prompt: a mutating run must surface a status note regularly.
    if mutations_since_note >= 5 {
        return (
            false,
            Some(format!(
                "BreathRequired: {} mutations since last task_update note (session={}, task={}, worktree={}); next legal transition: task_update(note) -> Mutating",
                mutations_since_note,
                session_id,
                active_task_id.unwrap_or(""),
                worktree
            )),
        );
    }
    (true, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unclaimed_mutating_action_blocked() {
        let (ok, msg) = evaluate_fsm_guard("s1", None, "w1", "bash write", true, 0);
        assert!(!ok);
        assert!(msg.unwrap().contains("Unpreflighted"));
    }

    #[test]
    fn claimed_action_below_breath_threshold_permitted() {
        let (ok, msg) = evaluate_fsm_guard("s1", Some("t1"), "w1", "bash write", true, 4);
        assert!(ok);
        assert!(msg.is_none());
    }

    #[test]
    fn breath_required_at_five_mutations() {
        let (ok, msg) = evaluate_fsm_guard("s1", Some("t1"), "w1", "bash write", true, 5);
        assert!(!ok);
        assert!(msg.unwrap().contains("BreathRequired"));
    }

    #[test]
    fn reads_always_permitted() {
        for at in [None, Some("t1")] {
            for m in [0usize, 99] {
                let (ok, msg) = evaluate_fsm_guard("s1", at, "w1", "read", false, m);
                assert!(ok);
                assert!(msg.is_none());
            }
        }
    }
}
