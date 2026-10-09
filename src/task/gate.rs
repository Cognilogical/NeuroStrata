//! The shared gate engine. One implementation serves the MCP
//! `neurostrata_task_validate` tool, the CLI `task gate`, and the daemon's
//! `POST /tasks/gate` route: every caller gets the same verdict, and all three
//! stay metadata-only (a `list` scan -- no embedder, no judgment, no graph
//! query), so the gate runs inside a `git push`.
//!
//! Design: docs/design-task-subsystem.md sections 4 and 5.3.

use crate::task::machine::Status;
use crate::traits::{MemoryPayload, SearchResult, VectorStore};
use serde_json::{json, Value};
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
}

impl Counts {
    pub fn to_json(&self) -> Value {
        json!({
            "open": self.open,
            "in_progress": self.in_progress,
            "blocked": self.blocked,
            "done": self.done,
            "total": self.total,
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

/// Whether any row declares `metadata.extracted_from: ["<task id>"]` -- the
/// declaration the edge_specs write path materializes as an inbound
/// EXTRACTED_FROM edge.
///
/// Checked off metadata rather than a Cypher query on purpose: `list` is on
/// the `VectorStore` trait every caller already holds, so the gate stays a
/// metadata-only scan (section 4) and the trait surface stays frozen.
pub fn extraction_exists(memories: &[SearchResult], task_id: &str) -> bool {
    memories.iter().any(|m| {
        m.id != task_id
            && m.payload
                .metadata
                .get("extracted_from")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().any(|target| target.as_str() == Some(task_id)))
                .unwrap_or(false)
    })
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

        match task_status(row) {
            Status::Open => {
                report.counts.open += 1;
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
                // Imported beads history is exempt: demanding retroactive
                // extraction from 40 closed issues is friction, not
                // enforcement (section 7).
                let grandfathered = meta
                    .and_then(|t| t.get("grandfathered"))
                    .and_then(|g| g.as_bool())
                    .unwrap_or(false);
                if !grandfathered && !extraction_exists(memories, &row.id) {
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
    self_test_with(&[
        "done_without_extraction",
        "in_progress",
        "p0_open_over_24h",
    ])
}

/// Runs the engine against a store: one `list`, no embedder, milliseconds.
pub async fn run(store: &Arc<dyn VectorStore>, namespace: &str) -> Result<GateReport, String> {
    let memories = store
        .list(namespace, None)
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
        row(id, "task", &format!("work for {}", id), json!({ "task": task_meta }))
    }

    /// FEATURE-4: a rule is an assertion until it says where it came from.
    #[test]
    fn a_rule_without_provenance_fails_the_gate() {
        let rows = vec![
            row("rule-sourced", "rule", "Use podman", json!({ "source": "owner 2026-10-08" })),
            row("rule-unsourced", "rule", "Use docker-free images", json!({})),
            row("rule-blank", "rule", "Blank source", json!({ "source": "  " })),
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
        let kinds: Vec<&str> = report
            .violations
            .iter()
            .map(|v| v.kind)
            .collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == "rule_overclaims_enforcement").count(),
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
            task("ns-aaaa", json!({ "status": "open", "priority": 0, "created_at": iso(DAY - HOUR) })),
            task("ns-bbbb", json!({ "status": "blocked", "created_at": iso(DAY * 3) })),
            task("ns-cccc", json!({ "status": "done", "created_at": iso(DAY * 3) })),
            row("mem-1", "fact", "the lesson", json!({ "extracted_from": ["ns-cccc"] })),
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
            task("ns-aaaa", json!({ "status": "done", "created_at": iso(DAY) })),
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
            task("ns-aaaa", json!({ "status": "done", "created_at": iso(DAY) })),
            row("mem-1", "fact", "the lesson", json!({ "extracted_from": ["ns-aaaa"] })),
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
        let fresh_p0 = task("ns-p0f", json!({ "status": "open", "priority": 0, "created_at": iso(HOUR) }));
        let old_p1 = task("ns-p1o", json!({ "status": "open", "priority": 1, "created_at": iso(DAY * 2) }));

        let report = evaluate(&[fresh_p0.clone(), old_p1.clone()], NOW);
        assert!(report.ok(), "neither should block: {:?}", report.violations);

        let old_p0 = task("ns-p0o", json!({ "status": "open", "priority": 0, "created_at": iso(DAY + HOUR) }));
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
        assert!(report.ok(), "absence of data is not a violation: {:?}", report.violations);
    }

    #[test]
    fn a_stale_claim_is_advisory_and_lands_outside_violations() {
        let memories = vec![task(
            "ns-aaaa",
            json!({ "status": "in_progress", "assignee": "agent-a", "updated_at": iso(DAY) }),
        )];
        let report = evaluate(&memories, NOW);
        assert_eq!(report.violations.len(), 1, "in_progress itself still blocks");
        assert_eq!(report.violations[0].kind, "in_progress");
        assert_eq!(report.stale.len(), 1, "and the staleness is reported apart");
        assert_eq!(report.stale[0].kind, "stale_in_progress");
        assert!(
            !report.violations.iter().any(|v| v.kind == "stale_in_progress"),
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
        assert!(report
            .extra
            .contains(&"p0_open_over_24h".to_string()));
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
            task("ns-aaaa", json!({ "status": "in_progress", "assignee": "a" })),
            task("ns-bbbb", json!({ "status": "done" })),
            task("ns-cccc", json!({ "status": "open", "priority": 2 })),
        ];
        let report = evaluate(&memories, NOW);

        let gate = report.gate_json();
        assert_eq!(gate["ok"], json!(false));
        assert!(gate["violations"].as_array().unwrap().len() >= 2);
        assert!(gate.get("stale").is_none(), "the gate body carries violations only");

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
            &[row("mem-1", "rule", "always use podman", json!({ "source": "owner 2026-10-08" }))],
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
        assert!(extraction_exists(&memories, "ns-aaaa"));
        assert!(!extraction_exists(&memories, "ns-zzzz"));
        // A task never extracts from itself.
        assert!(!extraction_exists(&memories[..1], "ns-aaaa"));
    }
}
