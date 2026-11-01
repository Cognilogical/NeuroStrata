# Agent Workflow State Machine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce the two-layer agent workflow state machine (per-task FSM + graph target lifecycle) at host boundaries so NeuroStrata graph/memory systems receive automatic, receipt-bound use without prompt-only mandates.

**Architecture:** Host-enforced FSM via MCP server `guard_validate` state table and OpenCode plugin shim gate; event-driven Layer 2 via ThalamicBus post-ingest normalize + `task_complete` pre-check. Layer 1 scopes FSM per-task claim; verification↔mutation loops allowed; reads unrestricted.

**Tech Stack:** Rust (tokio/tokio-util), LadybugDB, MCP server SDK, OpenCode TypeScript plugin, systemd/launchd/NSSM.

**Spec:** `docs/superpowers/specs/2026-10-26-state-machine-agent-workflow-design.md`

## Global Constraints

- Receipts bind to `(task_id, worktree, HEAD_sha, dirty_tree_hash)` computed via git + working-tree hash delta. NOT bash-string parsing. Edits invalidate revision marks → stale receipts surfaced.
- Reads permitted freely at any task FSM state; only mutations and task completion gated at host boundaries.
- FSM scoped per-task claim, not global session; `Verifying → Mutating` loopback allowed.
- Target resolution recomputable; orphaned targets warn, not block; task-completion pre-check normalizes dirty targets first (AC4 redefined).
- Semantic writes remain explicit / extraction-gated; auto-normalize runs deterministic reconciliation only.
- p95 latency budget: <100 ms for `ContextService::plan` + `upsert` canonicalization (AC5 must be measured pre-merge).

## Review Focus

1. Receipt staleness: uncommitted edits must flip `HEAD_sha`/`dirty_tree_hash` and surface stale receipts — no bash regex parsing anywhere.
2. FSM deadlock: `Verifying → Mutating` loopback and `BreathRequired → Mutating` clear paths must hold under repeated cycles.
3. Over-blocking: false-block rate of mutating tools in `Unpreflighted`/`BreathRequired` must stay near zero (reads always allowed).

---

### Task 0: AC5 — Baseline p95 benchmark on current code

**Files:**
- Create: `tests/benchmark/context_plan_latency.rs` (or repo's existing benchmark harness)
- Modify: none (measure existing `ContextService::plan` stub + `upsert`)

**Interfaces:**
- Produces: baseline p95 latency numbers for context planning and upsert canonicalization on the NeuroStrata repo.

- [ ] **Step 1: Write benchmark harness measuring `ContextService::plan` p95 and `LadybugStore::upsert` canonicalization p95**
- [ ] **Step 2: Run benchmark, record baseline numbers**
- [ ] **Step 3: Commit benchmark harness with recorded baselines**

---

### Task 1: Layer 1 — MCP server session+task+worktree state table + guard_validate check (AC2)

**Files:**
- Modify: `src/guard/validator.rs` (extend `guard_validate`)
- Modify: `src/server.rs` (add state lookup by `(session_id, task_id, worktree)`)
- Test: `tests/guard/validator_test.rs`

**Interfaces:**
- Consumes: task FSM state from `src/task/machine.rs` (`TaskClaimed` semantics).
- Produces: `guard_validate` returns legal next transition on rejection; session+task+worktree key validated.

- [ ] **Step 1: Write failing test — `guard_validate` rejects mutating tool in `Unpreflighted` task state**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement state check keyed by `(session_id, active_task_id, worktree)` in `guard_validate`**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

### Task 2: Layer 1 — OpenCode plugin shim blocks mutating tools in `Unpreflighted`/`BreathRequired`; allows reads

**Files:**
- Modify: `plugins/opencode/index.ts`

**Interfaces:**
- Consumes: legal-transition feedback from `guard_validate`.
- Produces: plugin-side pre-tool-use gate that denies `write`/`edit`/mutating `bash` when task state demands; denies until `task_update` Breath note recorded at >=5 mutations.

- [ ] **Step 1: Write failing test (plugin gate unit test / manual hook trace) — mutating tool blocked in `Unpreflighted`, allowed for reads**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement plugin gate honoring `guard_validate` legal transitions**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

### Task 3: AC1 — Wire `ContextService::plan` to `graph_read::memories_for_file` + `rank_dedup_budget`

**Files:**
- Modify: `src/context/service.rs` (de-stub `plan`)
- Modify: `src/context/rank.rs` (dedup budget ranking, if present; else per spec)
- Test: `tests/context/service_test.rs`

**Interfaces:**
- Consumes: `graph_read::memories_for_file` (file-scoped read).
- Produces: `ContextService::plan(task, worktree, file) -> Vec<ContextItem>` honoring 512 tok/call, 1500 tok/turn dedup budget.

- [ ] **Step 1: Write failing test — `plan` returns ranked file-scoped memories within dedup budget**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement `plan` wiring**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

### Task 4: Layer 2 — ThalamicBus post-ingest normalize subscriber + `task_complete` pre-check

**Files:**
- Modify: `src/events/subscribers/` (new ThalamicPulse subscriber for `IngestDone`)
- Modify: `src/task/` (completion pre-check hook)

**Interfaces:**
- Consumes: `ThalamicBus` pulse events; `LadybugStore::upsert` canonicalization (Task 5).
- Produces: auto-run `KnownIds::resolve` + relink on ingest done; task completion pre-check normalizes dirty targets.

- [ ] **Step 1: Write failing test — ingest-done triggers reconcile; task_complete with dirty targets normalizes before gating**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement subscriber + pre-check**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

### Task 5: AC3 — `LadybugStore::upsert` canonicalizes resolvable targets at write time

**Files:**
- Modify: `src/store/ladybug.rs` (`upsert`, `KnownIds::resolve`)

**Interfaces:**
- Consumes: `KnownIds::resolve` for unqualified/legacy path strings.
- Produces: edges with resolvable targets materialized at write time; deferred ones remain `Declared`.

- [ ] **Step 1: Write failing test — upsert of unqualified target resolves via KnownIds suffix match**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement canonicalization in `upsert`**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

### Task 6: AC4 — Classify unmatched targets; orphan warnings not blocks

**Files:**
- Modify: `src/main.rs` (`doctor` output), normalize-metadata flow

**Interfaces:**
- Consumes: classification states `Materialized` / `Declared` / `Orphaned`.
- Produces: doctor reports 0 resolvable-unmatched targets; unrelated orphans warn, non-blocking.

- [ ] **Step 1: Write failing test — doctor output classifies orphans into Materialized/Declared/Orphaned with warn-not-block semantics**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement classification**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

### Task 7: Receipt binding to `HEAD_sha` + `dirty_tree_hash`

**Files:**
- Modify: `src/task/` (receipt struct), `src/guard/` or store layer (hash computation)

**Interfaces:**
- Consumes: git worktree state (HEAD sha + working-tree hash delta).
- Produces: receipts carry `(task_id, worktree, HEAD_sha, dirty_tree_hash)`; stale receipts surfaced on edit.

- [ ] **Step 1: Write failing test — receipt invalidated when worktree dirty/hash changes post-creation**
- [ ] **Step 2: Run test to verify it fails**
- [ ] **Step 3: Implement receipt hash binding**
- [ ] **Step 4: Run test to verify it passes**
- [ ] **Step 5: Commit**

---

## Self-Review Checklist (writing-plans)

1. **Spec coverage:** Each AC (1–5) maps to at least one task (AC5→T0, AC1→T3, AC2→T1, AC3→T5, AC4→T6; receipts→T7; Layer 2→T4). ✓
2. **Step scan:** Every step = one checkable action (write test → run fails → implement → run passes → commit). ✓
3. **Type consistency:** Task interfaces use consistent names (`ContextService::plan`, `LadybugStore::upsert`, `KnownIds::resolve`, `guard_validate`). ✓
4. **Review Focus:** Three uncovered failure classes (receipt staleness, deadlock, over-blocking) are listed and each has a task. ✓
5. **Proportion:** Plan has 8 small tasks, each one TDD cycle; spec is ~90 lines. ✓
