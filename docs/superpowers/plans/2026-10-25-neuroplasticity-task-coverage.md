# NeuroPlasticity Run: Task Coverage Strengthening Regression

**Cycle purpose:** Validate the task-coverage enforcement changes, capture intent-detection lessons, and feed a learning procedure back into the system.

## Phase 1: Regression Test Matrix

Run `cargo test task::tests` and validate these scenarios:

| # | Scenario | Expected Result |
|---|----------|----------------|
| 1 | `cargo check` succeeds | ✓ 133 pre-existing warnings, no new errors |
| 2 | All `task::tests` pass | 64 passed; intent-blocking, hook wiring |
| 3 | Gate with no git → exit 0 | Script skips gracefully |
| 4 | Gate with git, no .md changes → exit 0 | "no markdown files changed" |
| 5 | Gate with git, .md added containing untracked marker (`next move`, `todo:`, `follow-up`, etc.) but no `ns-` ref → exit 1 + error message |
| 6 | Gate with git, .md added with task ID ref (`ns-`) → exit 0 | Task ID citation blocks the gate |

**Run:** `bash scripts/task-coverage-gate.sh` from repo root. Verify each case.

## Phase 2: Intent Detection Audit

1. Purposefully add TODO/markers to docs without task IDs
2. Observe if `detect_task_intents()` catches them
3. If false negatives → extract lesson as procedure memory

**Example test:** Add `TODO: investigate SEO traffic patterns` to a doc, run task complete → should block or guide to follow_up_tasks

## Phase 3: NeuroPlasticity Capture

If gap found in Phase 2, encode as procedure:

```
trigger: "before-edit (markdown doc modification)"
remaining_fires: 3
valid_to: [timestamp 30 days from now]
performance_count: 0
last_performed_at: now
last_episodic_pointer: <memory-id-of-this-cycle>
content: "After task-intent detection missed SEOs without ns-xrefs, added regex: (next|todo|follow-up|remaining): case-insensitive; gate now inspects only git diff added lines (+ prefix)."
```

If no gap → encode null-finding: "intent detection is adequate; blocking occurs only via git-diff citation scanning"

## Phase 4: Close-out

1. Task `neurostrata_task_complete` with `metadata.extracted_from: ["<this-task-id>"]` 
2. `git push` (pre-push gate runs successfully on clean repo)
3. No remaining work items

---
**Saved:** docs/superpowers/plans/2026-10-25-neuroplasticity-task-coverage.md

Proceed? Y to run Phase 1 regression matrix, N to skip.