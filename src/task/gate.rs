//! The shared gate engine. One implementation serves the MCP
//! `neurostrata_task_validate` tool, the CLI `task gate`, and the daemon's
//! `POST /tasks/gate` route: every caller gets the same verdict, and all three
//! stay metadata-only (a `list` scan -- no embedder, no judgment, no graph
//! query), so the gate runs inside a `git push`.
//!
//! Design: docs/design-task-subsystem.md sections 4 and 5.3.
//!
//! The gate judges live work. A row whose `metadata.valid_to` has passed is
//! retired history: it is not an active rule and not work in flight, so it
//! raises no violation -- except the extraction lesson it still owes, which
//! travels to the row that superseded it.

use crate::task::machine::Status;
use crate::traits::{MemoryPayload, SearchResult, VectorStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

/// A blocking rule break: exactly the shape POST /tasks/gate returns.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub id: String,
    pub kind: &'static str,
    pub detail: String,
}

impl Violation {
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "kind": self.kind, "detail": self.detail })
    }
}

/// P0 rot threshold: an open priority-0 task older than this blocks.
pub const P0_ROT_AFTER_SECS: i64 = 24 * 60 * 60;
/// Staleness threshold for an in_progress task: advisory, never blocking.
pub const STALE_AFTER_SECS: i64 = 60 * 60;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub open: usize,
    pub in_progress: usize,
    pub blocked: usize,
    pub done: usize,
    pub total: usize,
    /// Task rows the gate did not judge because they are retired history.
    /// Counted separately so the report never looks self-contradictory:
    /// `in_progress` may exceed the number of `in_progress` violations.
    pub retired: usize,
}

impl Counts {
    pub fn to_json(&self) -> Value {
        json!({
            "open": self.open,
            "in_progress": self.in_progress,
            "blocked": self.blocked,
            "done": self.done,
            "total": self.total,
            "retired": self.retired,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct GateReport {
    /// Blocking: in_progress, done-without-extraction, P0 open > 24h.
    pub violations: Vec<Violation>,
    /// Advisory only: in_progress with no update in > 60 min.
    pub stale: Vec<Violation>,
    /// The done-without-extraction violations, also listed on their own so
    /// `task_validate` can render them apart.
    pub unextracted_done: Vec<Violation>,
    pub counts: Counts,
}

impl GateReport {
    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }

    /// The POST /tasks/gate body: `{ok, violations[]}`.
    pub fn gate_json(&self) -> Value {
        json!({
            "ok": self.ok(),
            "violations": self.violations.iter().map(Violation::to_json).collect::<Vec<_>>(),
        })
    }

    /// The neurostrata_task_validate body.
    pub fn validate_json(&self) -> Value {
        json!({
            "violations": self.violations.iter().map(Violation::to_json).collect::<Vec<_>>(),
            "stale": self.stale.iter().map(Violation::to_json).collect::<Vec<_>>(),
            "unextracted_done": self.unextracted_done.iter().map(Violation::to_json).collect::<Vec<_>>(),
            "counts": self.counts.to_json(),
        })
    }
}

pub fn is_task(result: &SearchResult) -> bool {
    result.payload.memory_type == "task"
}

/// Whether a row is retired history: its `metadata.valid_to` has passed.
///
/// A retired row records something that used to be true -- a superseded
/// memory, an expired rule -- and is not a live claim, so the gate does
/// not judge it. Malformed expiry metadata is not proof of retirement, so
/// such a row stays live and keeps failing closed.
///
/// This deliberately disagrees with the store's own retirement filter
/// (`store::ladybug::is_retired_at`), which treats an unparseable expiry as
/// expired and hides the row from search. The store hides; the gate
/// refuses to forgive. A row the gate still judges is at least still
/// visible in `task_list`.
pub(crate) fn is_retired(metadata: &Value, now: i64) -> bool {
    metadata
        .get("valid_to")
        .and_then(Value::as_i64)
        .is_some_and(|valid_to| valid_to <= now)
}

/// The chain bound: a namespace is written by many hands, so a loop in the
/// lineage is data, not a hang.
const MAX_CHAIN: usize = 32;

/// The rows a row replaced, as ids. `supersedes` is written as one id, but a
/// hand-edited row may carry a list, and a list is read as a list.
fn superseded_ids(metadata: &Value) -> Vec<String> {
    match metadata.get("supersedes") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// The first live row in a retired row's supersession chain: the row that
/// carries the work now.
///
/// The chain, not the next link: a row that was replaced twice is followed to
/// the row that replaced it twice. Bounded and cycle-safe, because a
/// namespace is written by many hands and a loop in the lineage is data, not a
/// hang.
pub(crate) fn live_successor<'a>(
    metadata: &Value,
    memories: &'a [SearchResult],
    now: i64,
) -> Option<&'a SearchResult> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = metadata.get("superseded_by").and_then(Value::as_str)?;
    for _ in 0..MAX_CHAIN {
        if !seen.insert(current.to_string()) {
            // A cycle carries nothing.
            return None;
        }
        let row = memories.iter().find(|m| m.id == current)?;
        if !is_retired(&row.payload.metadata, now) {
            return Some(row);
        }
        current = row
            .payload
            .metadata
            .get("superseded_by")
            .and_then(Value::as_str)?;
    }
    None
}

/// The ids one unit of work passed through: the row's own id, then the rows
/// it replaced, then theirs.
///
/// A lesson belongs to the work, not to one row id: the row that replaced a
/// finished task is the same work under a new id, so the lesson extracted
/// from the old id is its lesson too.
pub(crate) fn work_lineage(memories: &[SearchResult], id: &str) -> Vec<String> {
    let mut lineage: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = id.to_string();
    while lineage.len() < MAX_CHAIN {
        if !seen.insert(current.clone()) {
            break;
        }
        lineage.push(current.clone());
        let Some(row) = memories.iter().find(|m| m.id == current) else {
            break;
        };
        let Some(next) = superseded_ids(&row.payload.metadata).into_iter().next() else {
            break;
        };
        current = next;
    }
    lineage
}

/// Whether a live lesson discharges the given task: some row declares
/// `metadata.extracted_from: ["<task id>"]`, where the task id is any id in
/// the task's work lineage.
///
/// A retired lesson row is history: it discharges nothing, exactly as the
/// store hides it from search. The store hides; the gate refuses to forgive.
pub fn extraction_exists(memories: &[SearchResult], task_id: &str, now: i64) -> bool {
    let lineage = work_lineage(memories, task_id);
    memories.iter().any(|m| {
        m.id != task_id
            && !is_retired(&m.payload.metadata, now)
            && m.payload
                .metadata
                .get("extracted_from")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter().any(|target| {
                        target
                            .as_str()
                            .map(|t| lineage.iter().any(|id| id == t))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
    })
}

/// Whether a retired row's lesson is carried by a live successor: the row
/// that replaced it is itself a completed task whose own inbound extraction
/// edge exists.
///
/// The successor is the row at the end of the supersession chain, not merely
/// the next link: a work that was replaced twice is carried by the row that
/// replaced it twice.
///
/// Anything less leaves the obligation where it started. A successor that
/// is not a task, one that is still open (a reopened task owes nothing yet),
/// one whose own lesson is still missing, one whose lesson is retired, and
/// no successor at all -- all of them mean the retired row still owes its
/// lesson and still blocks.
fn obligation_carried(metadata: &Value, memories: &[SearchResult], now: i64) -> bool {
    let Some(successor) = live_successor(metadata, memories, now) else {
        return false;
    };
    is_task(successor)
        && task_status(successor) == Status::Done
        && extraction_exists(memories, &successor.id, now)
}

pub fn task_status(result: &SearchResult) -> Status {
    result
        .payload
        .metadata
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|s| s.as_str())
        .and_then(Status::parse)
        .unwrap_or(Status::Open)
}

/// The age of `metadata.task.<first parseable key>`, in seconds. Crate-visible
/// because the claim handler needs the same liveness window the gate's
/// staleness advisory uses.
pub(crate) fn age_secs(meta: Option<&Value>, keys: &[&str], now: i64) -> Option<i64> {
    let meta = meta?;
    for key in keys {
        if let Some(stamp) = meta
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
        {
            return Some(now - stamp);
        }
    }
    None
}

/// The whole gate: three blocking rules plus the advisory staleness list.
pub fn evaluate(memories: &[SearchResult], now: i64) -> GateReport {
    let mut report = GateReport::default();

    for row in memories {
        if !is_task(row) {
            continue;
        }
        report.counts.total += 1;
        let meta = row.payload.metadata.get("task");
        let content = row.payload.content.as_str();
        // A retired row is history, not work in flight: it is neither
        // unfinished work nor rot. It is still counted (the counts describe
        // the namespace's rows) but it raises no live-work violation.
        let retired = is_retired(&row.payload.metadata, now);
        if retired {
            report.counts.retired += 1;
        }

        match task_status(row) {
            Status::Open => {
                report.counts.open += 1;
                if retired {
                    continue;
                }
                // Missing priority reads as the default (2), never as P0:
                // absence of data is not a reason to block a push.
                let priority = meta
                    .and_then(|t| t.get("priority"))
                    .and_then(|p| p.as_i64())
                    .unwrap_or(2);
                let age = age_secs(meta, &["created_at", "updated_at"], now);
                if priority == 0 && age.map(|a| a > P0_ROT_AFTER_SECS).unwrap_or(false) {
                    report.violations.push(Violation {
                        id: row.id.clone(),
                        kind: "p0_open_over_24h",
                        detail: format!(
                            "P0 task '{}' has been open for more than 24h; claim it, block it with a reason, or finish it.",
                            content
                        ),
                    });
                }
            }
            Status::InProgress => {
                report.counts.in_progress += 1;
                if retired {
                    continue;
                }
                let assignee = meta
                    .and_then(|t| t.get("assignee"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("unknown");
                report.violations.push(Violation {
                    id: row.id.clone(),
                    kind: "in_progress",
                    detail: format!(
                        "task '{}' is claimed by {} and unfinished; finish it via neurostrata_task_complete or release it (task_update status open).",
                        content, assignee
                    ),
                });
                if age_secs(meta, &["updated_at", "created_at"], now)
                    .map(|a| a > STALE_AFTER_SECS)
                    .unwrap_or(false)
                {
                    report.stale.push(Violation {
                        id: row.id.clone(),
                        kind: "stale_in_progress",
                        detail: format!(
                            "task '{}' has had no update for over 60 minutes; the claim may be dead -- take it over or release it.",
                            content
                        ),
                    });
                }
            }
            Status::Blocked => report.counts.blocked += 1,
            Status::Done => {
                report.counts.done += 1;
                // A retired row hands its lesson to the row that replaced it.
                // With no successor carrying it, the lesson is still owed here.
                if retired && obligation_carried(&row.payload.metadata, memories, now) {
                    continue;
                }
                // Imported beads history is exempt: demanding retroactive
                // extraction from 40 closed issues is friction, not
                // enforcement (section 7).
                let grandfathered = meta
                    .and_then(|t| t.get("grandfathered"))
                    .and_then(|g| g.as_bool())
                    .unwrap_or(false);
                if !grandfathered && !extraction_exists(memories, &row.id, now) {
                    let violation = Violation {
                        id: row.id.clone(),
                        kind: "done_without_extraction",
                        detail: format!(
                            "task '{}' is done with no inbound EXTRACTED_FROM edge; write the lesson with metadata extracted_from: [\"{}\"].",
                            content, row.id
                        ),
                    };
                    report.unextracted_done.push(violation.clone());
                    report.violations.push(violation);
                }
            }
        }
    }

    // A6 rule honesty: "a rule that no machine checks is a note, not a rule."
    // A rule claiming ENFORCED must name a real wire from the closed registry.
    // FEATURE-4: every rule carries provenance -- a memory is an assertion
    // until it says where it came from.
    let known = crate::task::wiring::known_wire_ids();
    for row in memories {
        if row.payload.memory_type != "rule" {
            continue;
        }
        let rule = &row.payload.metadata;
        // Malformed expiry metadata is not proof that a rule is retired.
        // Superseded rules stay in the store as history, not active claims.
        if is_retired(rule, now) {
            continue;
        }
        let has_source = crate::traits::has_source(rule);
        if !has_source {
            report.violations.push(Violation {
                id: row.id.clone(),
                kind: "rule_without_source",
                detail: format!(
                    "rule '{}' carries no metadata.source; rules are load-bearing claims and must name their origin (owner quote, doc, commit) with a date.",
                    content_short(&row.payload.content)
                ),
            });
        }
        if let Some(enf) = rule.get("enforcement").and_then(|e| e.as_str()) {
            if !matches!(enf, "ENFORCED" | "PARTIAL" | "NOT_ENFORCED") {
                report.violations.push(Violation {
                    id: row.id.clone(),
                    kind: "invalid_enforcement_value",
                    detail: format!(
                        "rule '{}' carries an unknown enforcement value '{}'; use ENFORCED, PARTIAL, or NOT_ENFORCED.",
                        content_short(&row.payload.content), enf
                    ),
                });
                continue;
            }
        }
        if let Some(enf) = rule.get("enforcement").and_then(|e| e.as_str()) {
            match enf {
                "ENFORCED" => {}
                "PARTIAL" => {
                    let guard = rule.get("guard").and_then(|g| g.as_str()).unwrap_or("");
                    if guard.is_empty() || !known.contains(&guard) {
                        report.violations.push(Violation {
                            id: row.id.clone(),
                            kind: "partial_without_guard",
                            detail: format!(
                                "rule '{}' claims PARTIAL but names no real guard.",
                                content_short(&row.payload.content)
                            ),
                        });
                        continue;
                    }
                }
                "NOT_ENFORCED" => {
                    let guard = rule.get("guard").and_then(|g| g.as_str()).unwrap_or("");
                    if !guard.is_empty() {
                        report.violations.push(Violation {
                            id: row.id.clone(),
                            kind: "not_enforced_with_guard",
                            detail: format!(
                                "rule '{}' claims NOT_ENFORCED but names a guard.",
                                content_short(&row.payload.content)
                            ),
                        });
                        continue;
                    }
                }
                _ => {}
            }
        }
        if rule.get("enforcement").and_then(|e| e.as_str()) != Some("ENFORCED") {
            continue;
        }
        let guard = rule.get("guard").and_then(|g| g.as_str()).unwrap_or("");
        if guard.is_empty() || !known.contains(&guard) {
            report.violations.push(Violation {
                id: row.id.clone(),
                kind: "rule_overclaims_enforcement",
                detail: format!(
                    "rule '{}' claims ENFORCED but names no real guard; set enforcement honestly (PARTIAL/NOT_ENFORCED) or name an existing wire id in metadata.guard.",
                    content_short(&row.payload.content)
                ),
            });
        }
    }

    report
}

fn content_short(content: &str) -> String {
    content.chars().take(80).collect()
}

// ---------------------------------------------------------------------------
// Self-test (Q4.3 of docs/design-wiring-panel.md)
//
// The gate self-tests itself so it can prove it is still a gate. The runner
// does NOT ship project-side (Q4.1 of the wiring panel -- per-file mutation
// and byte-identical restore was corrupting the guinea pig), but the contract
// DOES ship as documented convention (Q4.2): every gate SHOULD accept
// `--self-test`. Core realises it against a synthetic fixture namespace
// (no embedder, no DB, no daemon): four planted rows, one `evaluate()` pass,
// diff the produced violation-kind set against an expected set.
// ---------------------------------------------------------------------------

/// Build a `SearchResult` row from inside the engine. Public visibility via
/// the engine path so the canonical fixture can be planted the same way the
/// unit tests plant individual rows.
fn self_test_row(id: &str, memory_type: &str, content: &str, metadata: Value) -> SearchResult {
    SearchResult {
        id: id.to_string(),
        score: 0.0,
        payload: MemoryPayload {
            content: content.to_string(),
            user_id: "self-test-fixture".to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        },
        evidence: None,
    }
}

fn self_test_task(id: &str, task_meta: Value) -> SearchResult {
    self_test_row(
        id,
        "task",
        &format!("self-test fixture task {}", id),
        json!({ "task": task_meta }),
    )
}

/// The verdict `task gate --self-test` prints. Both `expected` and `actual`
/// violation-kind sets plus the diff are kept, so a passing gate and a broken
/// gate are expressed in the same shape and a CI reporter can pin which rule
/// drifted.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfTestReport {
    pub passed: bool,
    pub expected_kinds: Vec<String>,
    pub actual_kinds: Vec<String>,
    pub missing: Vec<String>,
    pub extra: Vec<String>,
    pub total_violations: usize,
    pub counts: Counts,
}

impl SelfTestReport {
    pub fn to_json(&self) -> Value {
        json!({
            "passed": self.passed,
            "expected_kinds": self.expected_kinds,
            "actual_kinds": self.actual_kinds,
            "missing": self.missing,
            "extra": self.extra,
            "total_violations": self.total_violations,
            "counts": self.counts.to_json(),
        })
    }
}

/// A fixed clock for self-test so the same fixture always rotates the same
/// way. NOW is well past the P0 rot threshold so the planted rotting P0 task
/// is reliably 25h old, and the planted in_progress is fresh enough to avoid
/// the staleness advisory (which lives in `report.stale`, not
/// `report.violations`).
const SELF_TEST_NOW: i64 = 1_760_000_000;
const SELF_TEST_DAY: i64 = 86_400;
const SELF_TEST_HOUR: i64 = 3600;

/// The canonical self-test fixture: three violating tasks (unextracted done,
/// claimed in_progress, rotting P0) plus one clean control (a done WITH an
/// inbound `extracted_from` edge) so the engine proves it is not producing a
/// violation per row.
fn self_test_fixture() -> Vec<SearchResult> {
    fn iso(offset: i64) -> String {
        chrono::DateTime::from_timestamp(SELF_TEST_NOW - offset, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
    vec![
        // Violating: done task without an inbound EXTRACTED_FROM edge.
        self_test_task(
            "ns-self-done",
            json!({ "status": "done", "created_at": iso(SELF_TEST_DAY) }),
        ),
        // Violating: in_progress, claimed by agent-self, 60s old (under stale).
        self_test_task(
            "ns-self-claimed",
            json!({
                "status": "in_progress",
                "assignee": "agent-self",
                "updated_at": iso(60),
            }),
        ),
        // Violating: P0 open for 25h (rot threshold is 24h).
        self_test_task(
            "ns-self-rot",
            json!({
                "status": "open",
                "priority": 0,
                "created_at": iso(SELF_TEST_DAY + SELF_TEST_HOUR),
            }),
        ),
        // Clean control: done task WITH an inbound extracted_from edge.
        self_test_task(
            "ns-self-clean",
            json!({ "status": "done", "created_at": iso(SELF_TEST_DAY * 3) }),
        ),
        self_test_row(
            "mem-self-lesson",
            "fact",
            "self-test clean control's extracted lesson",
            json!({ "extracted_from": ["ns-self-clean"] }),
        ),
    ]
}

/// Run the gate engine against the canonical self-test fixture and diff the
/// produced violation-kind set against `expected_kinds`. Used by both the
/// CLI `--self-test` flag and the in-module unit tests.
///
/// `passed` is true iff every expected kind was produced AND no other kind
/// was produced. Same shape for PASS and FAIL so a CI reporter can pin which
/// rule drifted.
pub fn self_test_with(expected_kinds: &[&'static str]) -> SelfTestReport {
    let fixture = self_test_fixture();
    let report = evaluate(&fixture, SELF_TEST_NOW);
    let mut actual: Vec<String> = report
        .violations
        .iter()
        .map(|v| v.kind.to_string())
        .collect();
    actual.sort();
    actual.dedup();
    let expected: Vec<String> = expected_kinds.iter().map(|s| s.to_string()).collect();
    let missing: Vec<String> = expected_kinds
        .iter()
        .filter(|k| !actual.iter().any(|a| a == **k))
        .map(|s| s.to_string())
        .collect();
    let extra: Vec<String> = actual
        .iter()
        .filter(|a| !expected_kinds.iter().any(|e| *e == a.as_str()))
        .cloned()
        .collect();
    SelfTestReport {
        passed: missing.is_empty() && extra.is_empty(),
        expected_kinds: expected,
        actual_kinds: actual,
        missing,
        extra,
        total_violations: report.violations.len(),
        counts: report.counts,
    }
}

/// The canonical self-test invocation: the three blocking rule breaks the
/// wiring panel prescribes for the gate to recognize itself as a gate.
pub fn self_test() -> SelfTestReport {
    self_test_with(&["done_without_extraction", "in_progress", "p0_open_over_24h"])
}

/// Runs the engine against a store: one `list`, no embedder, milliseconds.
pub async fn run(store: &Arc<dyn VectorStore>, namespace: &str) -> Result<GateReport, String> {
    let memories = store
        .list(namespace, None, None)
        .await
        .map_err(|e| format!("could not list tasks in '{}': {}", namespace, e))?;
    Ok(evaluate(&memories, chrono::Utc::now().timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;

    fn row(id: &str, memory_type: &str, content: &str, metadata: Value) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score: 0.0,
            payload: MemoryPayload {
                content: content.to_string(),
                user_id: "kenton".to_string(),
                memory_type: memory_type.to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata,
            },
            evidence: None,
        }
    }

    fn task(id: &str, task_meta: Value) -> SearchResult {
        row(
            id,
            "task",
            &format!("work for {}", id),
            json!({ "task": task_meta }),
        )
    }

    /// FEATURE-4: a rule is an assertion until it says where it came from.
    #[test]
    fn a_rule_without_provenance_fails_the_gate() {
        let rows = vec![
            row(
                "rule-sourced",
                "rule",
                "Use podman",
                json!({ "source": "owner 2026-10-08" }),
            ),
            row(
                "rule-unsourced",
                "rule",
                "Use docker-free images",
                json!({}),
            ),
            row(
                "rule-blank",
                "rule",
                "Blank source",
                json!({ "source": "  " }),
            ),
        ];
        let report = evaluate(&rows, 0);
        let hits: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "rule_without_source")
            .map(|v| v.id.as_str())
            .collect();
        assert_eq!(hits, vec!["rule-unsourced", "rule-blank"]);
    }

    #[test]
    fn retired_rules_do_not_fail_active_rule_checks() {
        let rows = vec![
            row("active-unsourced", "rule", "Active rule", json!({})),
            row(
                "retired-unsourced",
                "rule",
                "Superseded rule",
                json!({ "valid_to": NOW - 1 }),
            ),
            row(
                "retired-at-boundary",
                "rule",
                "Rule expiring now",
                json!({ "valid_to": NOW }),
            ),
            row(
                "retired-overclaim",
                "rule",
                "Expired enforced rule",
                json!({
                    "valid_to": NOW - 1,
                    "source": "owner 2026-10-08",
                    "enforcement": "ENFORCED",
                    "guard": "no-such-wire"
                }),
            ),
            row(
                "future-unsourced",
                "rule",
                "Still-current rule",
                json!({ "valid_to": NOW + 1 }),
            ),
            row(
                "malformed-expiry-unsourced",
                "rule",
                "Unparseable expiry remains active",
                json!({ "valid_to": "invalid" }),
            ),
        ];

        let report = evaluate(&rows, NOW);
        let unsourced: Vec<&str> = report
            .violations
            .iter()
            .filter(|violation| violation.kind == "rule_without_source")
            .map(|violation| violation.id.as_str())
            .collect();

        assert_eq!(
            unsourced,
            vec![
                "active-unsourced",
                "future-unsourced",
                "malformed-expiry-unsourced"
            ]
        );
        assert!(!report.violations.iter().any(|violation| {
            violation.id == "retired-overclaim" && violation.kind == "rule_overclaims_enforcement"
        }));
        assert!(
            !report
                .violations
                .iter()
                .any(|violation| violation.id.starts_with("retired-")),
            "retired rules must not block the active-rule gate"
        );
    }

    /// A6 rule honesty: a rule that claims ENFORCED must name a real wire.
    #[test]
    fn a_rule_overclaiming_enforcement_fails_the_gate() {
        let rows = vec![
            row(
                "rule-1",
                "rule",
                "Always use podman",
                json!({ "enforcement": "ENFORCED", "guard": null, "source": "owner 2026-10-08" }),
            ),
            row(
                "rule-2",
                "rule",
                "Always use podman for real",
                json!({ "enforcement": "ENFORCED", "guard": "task-close-lock", "source": "owner 2026-10-08" }),
            ),
            row(
                "rule-3",
                "rule",
                "A note pretending to be a rule",
                json!({ "enforcement": "ENFORCED", "guard": "no-such-wire", "source": "owner 2026-10-08" }),
            ),
            row(
                "rule-4",
                "rule",
                "Honestly unenforced",
                json!({ "enforcement": "NOT_ENFORCED", "guard": null, "source": "owner 2026-10-08" }),
            ),
        ];
        let report = evaluate(&rows, 0);
        let kinds: Vec<&str> = report.violations.iter().map(|v| v.kind).collect();
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == "rule_overclaims_enforcement")
                .count(),
            2,
            "rule-1 (null guard) and rule-3 (dangling guard); rule-2 and rule-4 are honest"
        );
        assert!(report.violations.iter().any(|v| v.id == "rule-1"));
        assert!(report.violations.iter().any(|v| v.id == "rule-3"));
        assert!(!report.violations.iter().any(|v| v.id == "rule-2"));
        assert!(!report.violations.iter().any(|v| v.id == "rule-4"));
    }

    const NOW: i64 = 1_760_000_000;
    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;

    fn iso(offset: i64) -> String {
        chrono::DateTime::from_timestamp(NOW - offset, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// A retired task row is history, not live work: it blocks nothing
    /// except the extraction lesson it still owes.
    #[test]
    fn retired_task_rows_are_outside_the_live_gate() {
        let memories = vec![
            // Live work still blocks.
            task(
                "live-in-progress",
                json!({ "status": "in_progress", "assignee": "agent-a", "updated_at": iso(60) }),
            ),
            // A live claim that has gone quiet is stale, and only stale.
            task(
                "live-stale-in-progress",
                json!({ "status": "in_progress", "assignee": "agent-a", "updated_at": iso(DAY * 3) }),
            ),
            // Retired work does not.
            row(
                "retired-in-progress",
                "task",
                "work for retired-in-progress",
                json!({
                    "task": { "status": "in_progress", "assignee": "ghost", "updated_at": iso(60) },
                    "valid_to": NOW - 1
                }),
            ),
            row(
                "retired-stale-in-progress",
                "task",
                "work for retired-stale-in-progress",
                json!({
                    "task": { "status": "in_progress", "assignee": "ghost", "updated_at": iso(DAY * 3) },
                    "valid_to": NOW - 1
                }),
            ),
            row(
                "retired-p0-open",
                "task",
                "work for retired-p0-open",
                json!({
                    "task": { "status": "open", "priority": 0, "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1
                }),
            ),
            // Expiring exactly now is already history.
            row(
                "retired-at-boundary",
                "task",
                "work for retired-at-boundary",
                json!({
                    "task": { "status": "in_progress", "assignee": "ghost", "updated_at": iso(60) },
                    "valid_to": NOW
                }),
            ),
            // An unparseable expiry is not proof of retirement: it stays live.
            row(
                "malformed-expiry-in-progress",
                "task",
                "work for malformed-expiry-in-progress",
                json!({
                    "task": { "status": "in_progress", "assignee": "agent-a", "updated_at": iso(60) },
                    "valid_to": "invalid"
                }),
            ),
            // A future expiry is not a retirement yet: the row is still live.
            row(
                "future-expiry-in-progress",
                "task",
                "work for future-expiry-in-progress",
                json!({
                    "task": { "status": "in_progress", "assignee": "agent-a", "updated_at": iso(60) },
                    "valid_to": NOW + 1
                }),
            ),
            // The lesson moved with the work: the retired row hands its
            // obligation to a successor that is itself done and has a lesson.
            row(
                "retired-done-with-successor",
                "task",
                "work for retired-done-with-successor",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "live-successor"
                }),
            ),
            task(
                "live-successor",
                json!({ "status": "done", "created_at": iso(DAY * 3) }),
            ),
            row(
                "mem-lesson",
                "fact",
                "the lesson",
                json!({ "extracted_from": ["live-successor"] }),
            ),
            // A retired row with no successor still owes its lesson.
            row(
                "retired-done-no-successor",
                "task",
                "work for retired-done-no-successor",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1
                }),
            ),
            // A live done row without an extraction still blocks.
            task(
                "live-done-no-lesson",
                json!({ "status": "done", "created_at": iso(DAY) }),
            ),
            // A successor that is done but has no lesson of its own carries
            // nothing: it owes one too, and so does the row it replaced.
            task(
                "done-successor-no-lesson",
                json!({ "status": "done", "created_at": iso(DAY) }),
            ),
            row(
                "retired-done-successor-no-lesson",
                "task",
                "work for retired-done-successor-no-lesson",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "done-successor-no-lesson"
                }),
            ),
            // A successor that is still open carries nothing: a reopened
            // task owes no lesson yet, so the retired row still owes one.
            row(
                "retired-done-successor-open",
                "task",
                "work for retired-done-successor-open",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "open-successor"
                }),
            ),
            task(
                "open-successor",
                json!({ "status": "open", "priority": 2, "created_at": iso(DAY) }),
            ),
            // A successor that was reopened is live work again: it is not
            // finished work that carries a lesson, so it carries nothing.
            task(
                "reopened-successor",
                json!({ "status": "open", "priority": 2, "created_at": iso(DAY) }),
            ),
            row(
                "mem-lesson-reopened",
                "fact",
                "the earlier lesson",
                json!({ "extracted_from": ["reopened-successor"] }),
            ),
            row(
                "retired-done-successor-reopened",
                "task",
                "work for retired-done-successor-reopened",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "reopened-successor"
                }),
            ),
            // A successor that is itself retired carries nothing.
            row(
                "retired-done-successor-retired",
                "task",
                "work for retired-done-successor-retired",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "retired-successor"
                }),
            ),
            row(
                "retired-successor",
                "task",
                "work for retired-successor",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1
                }),
            ),
            row(
                "mem-lesson-retired",
                "fact",
                "the earlier lesson",
                json!({ "extracted_from": ["retired-successor"] }),
            ),
            // A successor that does not exist carries nothing.
            row(
                "retired-done-successor-missing",
                "task",
                "work for retired-done-successor-missing",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "no-such-row"
                }),
            ),
            // A successor that is not a task carries nothing.
            row(
                "retired-done-successor-nontask",
                "task",
                "work for retired-done-successor-nontask",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "mem-fact"
                }),
            ),
            row("mem-fact", "fact", "an unrelated fact", json!({})),
            // A successor that is not a task carries nothing, even when it
            // wears a task-shaped metadata block and a lesson of its own.
            row(
                "mem-fact-done",
                "fact",
                "a fact wearing a task's clothes",
                json!({ "task": { "status": "done" } }),
            ),
            row(
                "mem-lesson-fact",
                "fact",
                "the lesson",
                json!({ "extracted_from": ["mem-fact-done"] }),
            ),
            row(
                "retired-done-successor-factshape",
                "task",
                "work for retired-done-successor-factshape",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "mem-fact-done"
                }),
            ),
        ];

        let report = evaluate(&memories, NOW);

        let in_progress: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "in_progress")
            .map(|v| v.id.as_str())
            .collect();
        assert_eq!(
            in_progress,
            vec![
                "live-in-progress",
                "live-stale-in-progress",
                "malformed-expiry-in-progress",
                "future-expiry-in-progress"
            ]
        );

        let rot: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "p0_open_over_24h")
            .map(|v| v.id.as_str())
            .collect();
        assert!(
            rot.is_empty(),
            "a retired open row is not live rot: {:?}",
            rot
        );

        // The lesson moved with the work: the retired row whose successor
        // carries it is clean. Every retired row whose successor does not
        // carry it still owes one, and still blocks.
        let unextracted: Vec<&str> = report
            .unextracted_done
            .iter()
            .map(|v| v.id.as_str())
            .collect();
        assert_eq!(
            unextracted,
            vec![
                "retired-done-no-successor",
                "live-done-no-lesson",
                "done-successor-no-lesson",
                "retired-done-successor-no-lesson",
                "retired-done-successor-open",
                "retired-done-successor-reopened",
                "retired-done-successor-retired",
                "retired-done-successor-missing",
                "retired-done-successor-nontask",
                "retired-done-successor-factshape",
            ]
        );
        // The advisory list is not the gate: the blocking list must carry
        // them, or the push gate would stay green.
        let blocking: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "done_without_extraction")
            .map(|v| v.id.as_str())
            .collect();
        assert_eq!(blocking, unextracted);
        assert!(!report.ok(), "an unfulfilled lesson still blocks");

        // Only live work goes stale; retired work is not work in flight.
        let stale: Vec<&str> = report.stale.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(stale, vec!["live-stale-in-progress"]);

        // Counts describe the namespace's task rows, retired ones included
        // and counted apart; violations describe the live ones.
        assert_eq!(report.counts.total, 23);
        assert_eq!(report.counts.in_progress, 7);
        assert_eq!(report.counts.open, 3);
        assert_eq!(report.counts.done, 13);
        assert_eq!(report.counts.retired, 14);
    }

    /// A retired lesson row is history: it discharges nothing. A live task
    /// whose only lesson is retired still owes one.
    #[test]
    fn a_retired_lesson_row_discharges_nothing() {
        let memories = vec![
            task(
                "live-done",
                json!({ "status": "done", "created_at": iso(DAY) }),
            ),
            row(
                "retired-lesson",
                "fact",
                "the lesson for live-done",
                json!({ "extracted_from": ["live-done"], "valid_to": NOW - 1 }),
            ),
        ];
        let report = evaluate(&memories, NOW);
        let blocking: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "done_without_extraction")
            .map(|v| v.id.as_str())
            .collect();
        assert_eq!(
            blocking,
            vec!["live-done"],
            "a retired lesson is history, not a discharge"
        );
    }

    /// The successor's lesson has to be alive for the successor to carry the
    /// retired row's obligation: a retired lesson leaves both rows owing.
    #[test]
    fn a_retired_lesson_does_not_carry_a_retired_rows_obligation() {
        let memories = vec![
            row(
                "retired-done",
                "task",
                "retired work",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "live-done"
                }),
            ),
            task(
                "live-done",
                json!({ "status": "done", "created_at": iso(DAY) }),
            ),
            row(
                "retired-lesson",
                "fact",
                "the lesson",
                json!({ "extracted_from": ["live-done"], "valid_to": NOW - 1 }),
            ),
        ];
        let report = evaluate(&memories, NOW);
        let mut blocking: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "done_without_extraction")
            .map(|v| v.id.as_str())
            .collect();
        blocking.sort();
        assert_eq!(
            blocking,
            vec!["live-done", "retired-done"],
            "a retired lesson discharges neither the live task nor the retired row"
        );
    }

    /// The obligation follows the chain: a retired row whose successor is
    /// itself retired is discharged by the live row at the end of the chain,
    /// because that row is where the work landed.
    #[test]
    fn the_extraction_obligation_follows_the_supersession_chain() {
        let memories = vec![
            row(
                "chain-first",
                "task",
                "the work, first version",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "chain-second"
                }),
            ),
            row(
                "chain-second",
                "task",
                "the work, second version",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "chain-third"
                }),
            ),
            task(
                "chain-third",
                json!({ "status": "done", "created_at": iso(DAY * 3) }),
            ),
            row(
                "live-lesson",
                "fact",
                "the lesson",
                json!({ "extracted_from": ["chain-third"] }),
            ),
        ];
        let report = evaluate(&memories, NOW);
        let blocking: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "done_without_extraction")
            .map(|v| v.id.as_str())
            .collect();
        assert!(
            blocking.is_empty(),
            "the chain ends in a done row that carries the lesson: {:?}",
            blocking
        );
    }

    /// A lesson belongs to the work, not to one row id: the successor of a
    /// task that was already consolidated keeps that lesson.
    #[test]
    fn a_lesson_from_the_work_it_replaced_still_discharges_the_successor() {
        let memories = vec![
            row(
                "work-first",
                "task",
                "the work, first version",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "valid_to": NOW - 1,
                    "superseded_by": "work-second"
                }),
            ),
            row(
                "work-second",
                "task",
                "the work, second version",
                json!({
                    "task": { "status": "done", "created_at": iso(DAY * 3) },
                    "supersedes": "work-first"
                }),
            ),
            row(
                "the-lesson",
                "fact",
                "the lesson for the work",
                json!({ "extracted_from": ["work-first"] }),
            ),
        ];
        let report = evaluate(&memories, NOW);
        let blocking: Vec<&str> = report
            .violations
            .iter()
            .filter(|v| v.kind == "done_without_extraction")
            .map(|v| v.id.as_str())
            .collect();
        assert!(
            blocking.is_empty(),
            "the work was already consolidated: {:?}",
            blocking
        );
    }

    #[test]
    fn any_in_progress_task_blocks_the_gate() {
        let memories = vec![task(
            "ns-aaaa",
            json!({ "status": "in_progress", "assignee": "agent-a", "updated_at": iso(60) }),
        )];
        let report = evaluate(&memories, NOW);
        assert!(!report.ok());
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, "in_progress");
        assert_eq!(report.counts.in_progress, 1);
    }

    #[test]
    fn a_clean_namespace_passes() {
        let memories = vec![
            task(
                "ns-aaaa",
                json!({ "status": "open", "priority": 0, "created_at": iso(DAY - HOUR) }),
            ),
            task(
                "ns-bbbb",
                json!({ "status": "blocked", "created_at": iso(DAY * 3) }),
            ),
            task(
                "ns-cccc",
                json!({ "status": "done", "created_at": iso(DAY * 3) }),
            ),
            row(
                "mem-1",
                "fact",
                "the lesson",
                json!({ "extracted_from": ["ns-cccc"] }),
            ),
        ];
        let report = evaluate(&memories, NOW);
        assert!(report.ok(), "{:?}", report.violations);
        assert_eq!(report.counts.total, 3, "the memory is not a task");
        assert_eq!(report.counts.done, 1);
        assert_eq!(report.counts.blocked, 1);
        assert_eq!(report.counts.open, 1);
    }

    #[test]
    fn done_without_an_extraction_edge_blocks() {
        let memories = vec![
            task(
                "ns-aaaa",
                json!({ "status": "done", "created_at": iso(DAY) }),
            ),
            row("mem-1", "fact", "the lesson", json!({})),
        ];
        let report = evaluate(&memories, NOW);
        assert!(!report.ok());
        assert_eq!(report.violations[0].kind, "done_without_extraction");
        assert_eq!(report.unextracted_done.len(), 1);
        assert_eq!(report.unextracted_done[0].id, "ns-aaaa");
    }

    #[test]
    fn an_inbound_extraction_edge_clears_the_done_rule() {
        let memories = vec![
            task(
                "ns-aaaa",
                json!({ "status": "done", "created_at": iso(DAY) }),
            ),
            row(
                "mem-1",
                "fact",
                "the lesson",
                json!({ "extracted_from": ["ns-aaaa"] }),
            ),
        ];
        let report = evaluate(&memories, NOW);
        assert!(report.ok(), "{:?}", report.violations);
        assert!(report.unextracted_done.is_empty());
        assert_eq!(report.counts.done, 1);
    }

    #[test]
    fn a_grandfathered_done_task_needs_no_extraction() {
        let memories = vec![task(
            "ns-e49",
            json!({ "status": "done", "grandfathered": true, "bead_id": "NeuroStrata-e49" }),
        )];
        let report = evaluate(&memories, NOW);
        assert!(report.ok(), "{:?}", report.violations);
        assert_eq!(report.counts.done, 1);
    }

    #[test]
    fn a_p0_open_past_24h_blocks_but_fresh_p0_and_old_p1_do_not() {
        let fresh_p0 = task(
            "ns-p0f",
            json!({ "status": "open", "priority": 0, "created_at": iso(HOUR) }),
        );
        let old_p1 = task(
            "ns-p1o",
            json!({ "status": "open", "priority": 1, "created_at": iso(DAY * 2) }),
        );

        let report = evaluate(&[fresh_p0.clone(), old_p1.clone()], NOW);
        assert!(report.ok(), "neither should block: {:?}", report.violations);

        let old_p0 = task(
            "ns-p0o",
            json!({ "status": "open", "priority": 0, "created_at": iso(DAY + HOUR) }),
        );
        let report = evaluate(&[fresh_p0, old_p1, old_p0], NOW);
        assert!(!report.ok());
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, "p0_open_over_24h");
        assert_eq!(report.violations[0].id, "ns-p0o");
    }

    #[test]
    fn a_p0_open_past_24h_without_timestamps_does_not_block() {
        let no_clock = task("ns-p0x", json!({ "status": "open", "priority": 0 }));
        let report = evaluate(&[no_clock], NOW);
        assert!(
            report.ok(),
            "absence of data is not a violation: {:?}",
            report.violations
        );
    }

    #[test]
    fn a_stale_claim_is_advisory_and_lands_outside_violations() {
        let memories = vec![task(
            "ns-aaaa",
            json!({ "status": "in_progress", "assignee": "agent-a", "updated_at": iso(DAY) }),
        )];
        let report = evaluate(&memories, NOW);
        assert_eq!(
            report.violations.len(),
            1,
            "in_progress itself still blocks"
        );
        assert_eq!(report.violations[0].kind, "in_progress");
        assert_eq!(report.stale.len(), 1, "and the staleness is reported apart");
        assert_eq!(report.stale[0].kind, "stale_in_progress");
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.kind == "stale_in_progress"),
            "stale is never a blocking kind"
        );
    }

    #[test]
    fn a_fresh_claim_is_not_stale() {
        let memories = vec![task(
            "ns-aaaa",
            json!({ "status": "in_progress", "updated_at": iso(HOUR - 60) }),
        )];
        let report = evaluate(&memories, NOW);
        assert!(report.stale.is_empty());
    }

    // --- self-test (Q4.3 of docs/design-wiring-panel.md) -----------------

    #[test]
    fn self_test_passes_against_the_canonical_fixture() {
        // Three violating tasks + one clean control. Engine must surface
        // exactly the three blocking kinds and stop counting the clean one.
        let report = self_test();
        assert!(
            report.passed,
            "self_test must pass against the canonical fixture; missing={:?} extra={:?}",
            report.missing, report.extra
        );
        assert_eq!(report.total_violations, 3, "exactly 3 violations");
        assert_eq!(report.actual_kinds.len(), 3);
        assert!(report
            .actual_kinds
            .contains(&"done_without_extraction".to_string()));
        assert!(report.actual_kinds.contains(&"in_progress".to_string()));
        assert!(report
            .actual_kinds
            .contains(&"p0_open_over_24h".to_string()));
        // Clean control is counted but never violated.
        assert_eq!(report.counts.total, 4, "fixture plants 4 tasks");
        assert_eq!(report.counts.done, 2, "ns-self-done + ns-self-clean");
        assert_eq!(report.counts.in_progress, 1);
        assert_eq!(report.counts.open, 1);
    }

    #[test]
    fn self_test_fails_when_an_expected_kind_is_missing() {
        // Expect a kind the engine never produces. Missing is the failure mode.
        let report = self_test_with(&[
            "done_without_extraction",
            "in_progress",
            "p0_open_over_24h",
            "nonexistent_rule_kind",
        ]);
        assert!(!report.passed);
        assert!(report.extra.is_empty(), "engine produced no extras here");
        assert_eq!(report.missing, vec!["nonexistent_rule_kind".to_string()]);
    }

    #[test]
    fn self_test_fails_when_the_engine_emits_an_unexpected_kind() {
        // Expect one fewer kind than the engine produces. Extra is the failure mode.
        // Catches a future "the gate silently added a rule" -- "a gate that
        // passes its own test is not a gate."
        let report = self_test_with(&["done_without_extraction", "in_progress"]);
        assert!(!report.passed);
        assert!(report.missing.is_empty(), "every expected kind was found");
        assert!(report.extra.contains(&"p0_open_over_24h".to_string()));
    }

    #[test]
    fn self_test_to_json_round_trips_the_pass_fail_shape() {
        let pass = self_test();
        let pass_json = pass.to_json();
        assert_eq!(pass_json["passed"], json!(true));
        assert_eq!(pass_json["missing"], json!([]));
        assert_eq!(pass_json["extra"], json!([]));
        assert_eq!(pass_json["total_violations"], json!(3));

        let fail = self_test_with(&["does", "not", "match"]);
        let fail_json = fail.to_json();
        assert_eq!(fail_json["passed"], json!(false));
        assert!(!fail_json["missing"].as_array().unwrap().is_empty());
    }

    #[test]
    fn the_json_bodies_carry_the_shape_the_surfaces_declare() {
        let memories = vec![
            task(
                "ns-aaaa",
                json!({ "status": "in_progress", "assignee": "a" }),
            ),
            task("ns-bbbb", json!({ "status": "done" })),
            task("ns-cccc", json!({ "status": "open", "priority": 2 })),
        ];
        let report = evaluate(&memories, NOW);

        let gate = report.gate_json();
        assert_eq!(gate["ok"], json!(false));
        assert!(gate["violations"].as_array().unwrap().len() >= 2);
        assert!(
            gate.get("stale").is_none(),
            "the gate body carries violations only"
        );

        let validate = report.validate_json();
        assert!(validate.get("violations").is_some());
        assert!(validate.get("stale").is_some());
        assert!(validate.get("unextracted_done").is_some());
        assert_eq!(validate["counts"]["total"], json!(3));
        assert_eq!(validate["counts"]["done"], json!(1));
        assert_eq!(validate["counts"]["open"], json!(1));
        assert_eq!(validate["counts"]["in_progress"], json!(1));
        assert_eq!(validate["counts"]["blocked"], json!(0));
    }

    #[test]
    fn memories_are_never_counted_as_tasks() {
        // A provenance-bearing rule (rules carry source since FEATURE-4);
        // the point of this test is that it still is not a task.
        let report = evaluate(
            &[row(
                "mem-1",
                "rule",
                "always use podman",
                json!({ "source": "owner 2026-10-08" }),
            )],
            NOW,
        );
        assert!(report.ok());
        assert_eq!(report.counts.total, 0);
    }

    #[test]
    fn extraction_from_another_task_does_not_count_as_a_memory() {
        // The declaration still counts: the engine checks the metadata key
        // at the root, where the write path reads it, and nothing about the
        // declaring row's source type matters.
        let memories = vec![
            task("ns-aaaa", json!({ "status": "done" })),
            row(
                "ns-bbbb",
                "task",
                "other work",
                json!({
                    "task": { "status": "open" },
                    "extracted_from": ["ns-aaaa"],
                }),
            ),
        ];
        assert!(extraction_exists(&memories, "ns-aaaa", NOW));
        assert!(!extraction_exists(&memories, "ns-zzzz", NOW));
        // A task never extracts from itself.
        assert!(!extraction_exists(&memories[..1], "ns-aaaa", NOW));
    }
}
