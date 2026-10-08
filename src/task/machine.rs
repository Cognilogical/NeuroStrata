//! The task state machine: four states, eight legal edges, hand-rolled.
//!
//! A task's state is runtime JSON in a database, mutated across process
//! invocations, and its guards are facts only the handler can observe (an
//! extraction edge, a reason argument) -- so the machine is a table, not a
//! compile-time FSM. See docs/design-task-subsystem.md section 2.

use crate::traits::MemoryPayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Blocked,
    Done,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::InProgress => "in_progress",
            Status::Blocked => "blocked",
            Status::Done => "done",
        }
    }

    /// Parses a stored status string; unknown values read as no status.
    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "open" => Some(Status::Open),
            "in_progress" => Some(Status::InProgress),
            "blocked" => Some(Status::Blocked),
            "done" => Some(Status::Done),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Nothing to check: the handler's own side effects (claim sets
    /// assignee+session, unclaim clears them) carry the semantics.
    None,
    /// A note/reason argument must be non-empty.
    RequireReason,
    /// At least one inbound EXTRACTED_FROM edge, or a `memory` argument that
    /// writes one. Reachable only through task_complete.
    ExtractionRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    pub guard: Guard,
}

const fn t(from: Status, to: Status, guard: Guard) -> Transition {
    Transition { from, to, guard }
}

/// The whole machine: 4 states, 8 legal edges, one terminal state.
/// `done` has exactly two entrances and both are ExtractionRequired.
pub const TABLE: &[Transition] = &[
    t(Status::Open, Status::InProgress, Guard::None),             // claim
    t(Status::Open, Status::Blocked, Guard::RequireReason),
    t(Status::Open, Status::Done, Guard::ExtractionRequired),     // task_complete only
    t(Status::InProgress, Status::Open, Guard::None),             // unclaim
    t(Status::InProgress, Status::Blocked, Guard::RequireReason),
    t(Status::InProgress, Status::Done, Guard::ExtractionRequired), // task_complete only
    t(Status::Blocked, Status::Open, Guard::RequireReason),       // resolution note
    t(Status::Done, Status::Open, Guard::RequireReason),          // reopen; edges retained
];

/// One task record: the id (which doubles as the record id) plus the payload
/// whose `metadata.task` holds the state.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub payload: MemoryPayload,
}

impl Task {
    /// The stored status. A row with no readable status reads as open rather
    /// than erroring, so a hand-edited task stays claimable.
    pub fn status(&self) -> Status {
        self.payload
            .metadata
            .get("task")
            .and_then(|t| t.get("status"))
            .and_then(|s| s.as_str())
            .and_then(Status::parse)
            .unwrap_or(Status::Open)
    }

    pub fn task_field(&self, key: &str) -> Option<&serde_json::Value> {
        self.payload.metadata.get("task")?.get(key)
    }
}

/// What only the handler knows: the reason argument, whether an inbound
/// EXTRACTED_FROM edge exists, and who is asking (recorded in history).
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub reason: String,
    pub extraction_edge_exists: bool,
    pub actor: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// Not in the table. Carries the legal set from the current state so the
    /// error message teaches the machine.
    Illegal { from: Status, to: Status, legal: Vec<Status> },
    /// A RequireReason edge with an empty note.
    MissingReason { from: Status, to: Status },
    /// An ExtractionRequired edge with no extraction edge in sight.
    ExtractionRequired { id: String },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionError::Illegal { from, to, legal } => {
                let legal: Vec<&str> = legal.iter().map(|s| s.as_str()).collect();
                write!(
                    f,
                    "Illegal transition {} -> {}. From {} the legal transitions are: {}.",
                    from.as_str(),
                    to.as_str(),
                    from.as_str(),
                    legal.join(", ")
                )
            }
            TransitionError::MissingReason { from, to } => write!(
                f,
                "The {} -> {} transition requires a non-empty reason: pass `note`.",
                from.as_str(),
                to.as_str()
            ),
            TransitionError::ExtractionRequired { id } => write!(
                f,
                "task {} cannot reach done without an extracted memory (no inbound EXTRACTED_FROM edge). Complete it through neurostrata_task_complete with `memory` or `link_memory_id` (Lock 2).",
                id
            ),
        }
    }
}

impl std::error::Error for TransitionError {}

/// Applies one transition. Pure given `ctx` apart from the wall clock: it
/// returns a new `Task` with the status set, `updated_at` stamped, and the
/// history entry appended. The handler fills `ctx` from the DB, then upserts.
pub fn apply(task: &Task, to: Status, ctx: &Ctx) -> Result<Task, TransitionError> {
    let from = task.status();
    let legal: Vec<Status> = TABLE
        .iter()
        .filter(|e| e.from == from)
        .map(|e| e.to)
        .collect();
    let edge = TABLE
        .iter()
        .find(|e| e.from == from && e.to == to)
        .ok_or(TransitionError::Illegal { from, to, legal })?;

    match edge.guard {
        Guard::None => {}
        Guard::RequireReason if ctx.reason.trim().is_empty() => {
            return Err(TransitionError::MissingReason { from, to });
        }
        Guard::ExtractionRequired if !ctx.extraction_edge_exists => {
            return Err(TransitionError::ExtractionRequired { id: task.id.clone() });
        }
        Guard::RequireReason | Guard::ExtractionRequired => {}
    }

    let mut payload = task.payload.clone();
    if !payload.metadata.is_object() {
        payload.metadata = serde_json::json!({});
    }
    let meta = payload
        .metadata
        .as_object_mut()
        .expect("metadata was just coerced to an object");

    let mut task_obj = match meta.get("task") {
        Some(serde_json::Value::Object(o)) => o.clone(),
        _ => serde_json::Map::new(),
    };

    let at = timestamp();
    task_obj.insert("status".into(), serde_json::json!(to.as_str()));
    task_obj.insert("updated_at".into(), serde_json::json!(at));

    let mut history = match task_obj.get("history").and_then(|h| h.as_array()) {
        Some(entries) => entries.clone(),
        None => Vec::new(),
    };
    history.push(serde_json::json!({
        "from": from.as_str(),
        "to": to.as_str(),
        "at": at,
        "by": ctx.actor.as_str(),
        "session": ctx.session.as_str(),
        "note": ctx.reason.as_str(),
    }));
    task_obj.insert("history".into(), serde_json::Value::Array(history));
    meta.insert("task".into(), serde_json::Value::Object(task_obj));

    Ok(Task { id: task.id.clone(), payload })
}

/// Appends a history entry without moving the state, and stamps `updated_at`:
/// the Breath prompt's `task_update(note=...)` lands here, so Tier-3 task
/// state is the record itself rather than a separate log.
pub fn annotate(task: &Task, ctx: &Ctx) -> Task {
    let from = task.status();
    let mut payload = task.payload.clone();
    if !payload.metadata.is_object() {
        payload.metadata = serde_json::json!({});
    }
    let meta = payload
        .metadata
        .as_object_mut()
        .expect("metadata was just coerced to an object");

    let mut task_obj = match meta.get("task") {
        Some(serde_json::Value::Object(o)) => o.clone(),
        _ => serde_json::Map::new(),
    };
    let at = timestamp();
    task_obj.insert("updated_at".into(), serde_json::json!(at));
    let mut history = match task_obj.get("history").and_then(|h| h.as_array()) {
        Some(entries) => entries.clone(),
        None => Vec::new(),
    };
    history.push(serde_json::json!({
        "from": from.as_str(),
        "to": from.as_str(),
        "at": at,
        "by": ctx.actor.as_str(),
        "session": ctx.session.as_str(),
        "note": ctx.reason.as_str(),
    }));
    task_obj.insert("history".into(), serde_json::Value::Array(history));
    meta.insert("task".into(), serde_json::Value::Object(task_obj));

    Task { id: task.id.clone(), payload }
}

/// UTC timestamp in the shape the record examples use: `2026-10-08T10:12:00Z`.
pub fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with(status: &str) -> Task {
        Task {
            id: "ns-x7q2".to_string(),
            payload: MemoryPayload {
                content: "Migrate beads issues".to_string(),
                user_id: "kenton".to_string(),
                memory_type: "task".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::json!({
                    "task": { "status": status, "priority": 1, "history": [] }
                }),
            },
        }
    }

    fn ctx() -> Ctx {
        Ctx {
            reason: String::new(),
            extraction_edge_exists: false,
            actor: "agent-a".to_string(),
            session: "ses_abc".to_string(),
        }
    }

    #[test]
    fn the_table_is_exactly_the_eight_legal_edges() {
        let expected = [
            (Status::Open, Status::InProgress, Guard::None),
            (Status::Open, Status::Blocked, Guard::RequireReason),
            (Status::Open, Status::Done, Guard::ExtractionRequired),
            (Status::InProgress, Status::Open, Guard::None),
            (Status::InProgress, Status::Blocked, Guard::RequireReason),
            (Status::InProgress, Status::Done, Guard::ExtractionRequired),
            (Status::Blocked, Status::Open, Guard::RequireReason),
            (Status::Done, Status::Open, Guard::RequireReason),
        ];
        assert_eq!(TABLE.len(), 8);
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(
                (TABLE[i].from, TABLE[i].to, TABLE[i].guard),
                *want,
                "edge {} of the table",
                i
            );
        }
        // No self-loops and no second entrance into done that bypasses the guard.
        assert_eq!(
            TABLE
                .iter()
                .filter(|e| e.to == Status::Done)
                .filter(|e| e.guard == Guard::ExtractionRequired)
                .count(),
            2
        );
    }

    #[test]
    fn an_illegal_transition_returns_the_legal_set_from_the_current_state() {
        let err = apply(&task_with("blocked"), Status::InProgress, &ctx())
            .expect_err("blocked -> in_progress is not in the table");
        match &err {
            TransitionError::Illegal { from, to, legal } => {
                assert_eq!(*from, Status::Blocked);
                assert_eq!(*to, Status::InProgress);
                assert_eq!(*legal, vec![Status::Open]);
            }
            other => panic!("expected Illegal, got {:?}", other),
        }
        let message = err.to_string();
        assert!(message.contains("blocked -> in_progress"), "{}", message);
        assert!(message.contains("legal transitions are: open"), "{}", message);
    }

    #[test]
    fn a_task_cannot_stay_in_its_own_state_via_apply() {
        let err = apply(&task_with("open"), Status::Open, &ctx())
            .expect_err("open -> open is not in the table");
        match err {
            TransitionError::Illegal { from, legal, .. } => {
                assert_eq!(from, Status::Open);
                assert_eq!(legal, vec![Status::InProgress, Status::Blocked, Status::Done]);
            }
            other => panic!("expected Illegal, got {:?}", other),
        }
    }

    #[test]
    fn blocking_without_a_reason_is_refused() {
        let err = apply(&task_with("open"), Status::Blocked, &ctx())
            .expect_err("RequireReason with an empty note");
        assert!(matches!(err, TransitionError::MissingReason { .. }), "{:?}", err);
        assert!(err.to_string().contains("pass `note`"), "{}", err);

        let mut with_note = ctx();
        with_note.reason = "waiting on upstream".to_string();
        let moved = apply(&task_with("open"), Status::Blocked, &with_note).expect("reason given");
        assert_eq!(moved.status(), Status::Blocked);
    }

    #[test]
    fn done_requires_an_extraction_edge() {
        let err = apply(&task_with("in_progress"), Status::Done, &ctx())
            .expect_err("ExtractionRequired with no edge");
        assert!(
            matches!(err, TransitionError::ExtractionRequired { ref id } if id == "ns-x7q2"),
            "{:?}",
            err
        );

        let mut with_edge = ctx();
        with_edge.extraction_edge_exists = true;
        let moved = apply(&task_with("in_progress"), Status::Done, &with_edge).expect("edge exists");
        assert_eq!(moved.status(), Status::Done);
    }

    #[test]
    fn a_claim_needs_no_reason() {
        let moved = apply(&task_with("open"), Status::InProgress, &ctx())
            .expect("Guard::None accepts an empty reason");
        assert_eq!(moved.status(), Status::InProgress);
    }

    #[test]
    fn reopening_done_is_guarded_and_keeps_the_history() {
        let done = task_with("done");
        err_reopen_requires_reason(&done);

        let mut c = ctx();
        c.reason = "regression reopened".to_string();
        let reopened = apply(&done, Status::Open, &c).expect("reason given");
        assert_eq!(reopened.status(), Status::Open);
        let history = reopened.task_field("history").and_then(|h| h.as_array()).unwrap();
        assert_eq!(history.len(), 1, "the transition is recorded");
        assert_eq!(history[0]["from"], "done");
        assert_eq!(history[0]["to"], "open");
        assert_eq!(history[0]["by"], "agent-a");
        assert_eq!(history[0]["note"], "regression reopened");
        assert!(history[0]["at"].as_str().unwrap().ends_with('Z'));
    }

    fn err_reopen_requires_reason(done: &Task) {
        let err = apply(done, Status::Open, &ctx()).expect_err("done -> open is RequireReason");
        assert!(matches!(err, TransitionError::MissingReason { .. }), "{:?}", err);
    }

    #[test]
    fn apply_stamps_updated_at_and_leaves_every_other_field_alone() {
        let mut task = task_with("open");
        task.payload.metadata["task"]["assignee"] = serde_json::json!("kenton");
        task.payload.metadata["task"]["priority"] = serde_json::json!(0);

        let moved = apply(&task, Status::InProgress, &ctx()).expect("legal");

        assert!(moved.task_field("updated_at").and_then(|v| v.as_str()).is_some());
        assert_eq!(moved.task_field("assignee"), Some(&serde_json::json!("kenton")));
        assert_eq!(moved.task_field("priority"), Some(&serde_json::json!(0)));
        // The original row is untouched; apply returns a new Task.
        assert_eq!(task.status(), Status::Open);
    }

    #[test]
    fn stored_status_strings_round_trip() {
        for status in [
            Status::Open,
            Status::InProgress,
            Status::Blocked,
            Status::Done,
        ] {
            assert_eq!(Status::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{}\"", status.as_str())
            );
        }
        assert_eq!(Status::parse("canceled"), None, "a fifth state buys nothing");
    }

    #[test]
    fn a_row_without_a_status_reads_as_open() {
        let task = Task {
            id: "ns-aaaa".to_string(),
            payload: MemoryPayload {
                content: "t".into(),
                user_id: "u".into(),
                memory_type: "task".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::json!({}),
            },
        };
        assert_eq!(task.status(), Status::Open);
    }
}
