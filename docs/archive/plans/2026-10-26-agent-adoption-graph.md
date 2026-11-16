# Agent Adoption + Real Graph Usage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make NeuroStrata agent adoption automatic (no user reminders) and make the knowledge graph actually traversable, with honest degraded-state reporting, across supported hosts.

**Architecture:** Introduce `src/context/` as a read-planning boundary (model/source/freshness/plan/service/render). Storage owns generations/stubs/receipts; adapters own host-side injection. Add typed graph-read tools (`memories_for_file`, `related`), source/generation-scoped ingestion, 4-dimension freshness, typed decision envelope (vocabulary v8), embedding-space identity, durable receipts, and a trace-graded NeuroPlasticity loop.

**Tech Stack:** Rust (tokio, serde, sha2, fastembed, ladybug), MCP server, OpenCode/OpenClaw plugins (TypeScript), Podman NeuroPlasticity harness.

**Spec:** `docs/superpowers/specs/2026-10-26-agent-adoption-graph-design.md`

## Global Constraints

- No background LLM agents in any path.
- No autonomous knowledge/task/procedure mutations from triggers; bounded operational receipts only.
- No arbitrary Cypher over MCP; typed tools only.
- `lbug 0.20.4` single-writer: hold write lock inside `spawn_blocking`; debounce incremental re-ingest.
- Privacy/egress violations are hard failures, never weighted.
- Exact file/graph retrieval must survive embedding outage/migration.
- All degraded states must be reported, never masked.

## Review Focus

- Concurrent worktrees must not conflate source identity or governance.
- Host compaction/restart must rehydrate dedup state, not silently suppress recall.
- A file hash match must not be treated as semantic verification of a decision.
- `RELATES_TO` must never be treated as governance authority.
- Privacy denial must be distinct from "not found."

---

## File Structure

**New files:**
- `src/context/mod.rs`, `src/context/model.rs`, `src/context/source.rs`, `src/context/freshness.rs`, `src/context/plan.rs`, `src/context/service.rs`, `src/context/render.rs`
- `src/graph_read/mod.rs`, `src/graph_read/memories_for_file.rs`, `src/graph_read/related.rs`
- `src/receipts/mod.rs`, `src/receipts/store.rs`
- `tests/context/*`, `tests/graph_read/*`

**Modified files:**
- `src/traits.rs` (GraphRead trait beside VectorStore)
- `src/server.rs` (handlers, format_memory, snapshot, search, task_claim)
- `src/store/ladybug.rs` (source/generation-scoped IDs, staged publication, stub nodes, embedding identity)
- `src/parser/ingest.rs` (hash capture, stable symbol identity, staged publication)
- `src/buffer.rs` (read-only domain recall, remove auto append from triggers)
- `src/tools/procedure.rs` (state machine + idempotent receipts)
- `src/schemas/memory-vocabulary.v3.json` (v8 decision envelope)
- `src/embed.rs` (egress policy, descriptor provenance)
- `plugins/opencode/index.ts`, `plugins/openclaw/index.ts` (auth, tools/call, triggers)
- `src/recall_metrics.rs` (durable receipts)
- `plasticity.json` + harness (trace grading)

---

## Task 1: Phase 0 — Adapter/auth/SDK contracts & DTOs

**Files:**
- Modify: `plugins/opencode/index.ts`, `plugins/openclaw/index.ts`
- Create: `src/context/model.rs` (ContextRequest, ContextPlan, ContextItem, Freshness, Coverage, Degradation enums)
- Test: `tests/context/model_test.rs`

**Interfaces:**
- Produces: `ContextRequest { session_id, context_epoch, event_id, trigger, source: SourceIdentity, policy: EgressPolicy }`
- Produces: `ContextPlan { schema_version, event_id, index_generation, applied_scope, items: Vec<ContextItem>, truncation, degradation }`

- [ ] **Step 1: Write failing test** for `ContextRequest` serialization round-trip and `Degradation` enum variants.
- [ ] **Step 2: Run test** → expect FAIL.
- [ ] **Step 3: Implement** `src/context/model.rs` with serde derives; include `SourceIdentity`, `Freshness { source_currency, semantic_validation, index_coverage, decision_lifecycle }`, `Coverage { complete, partial, unavailable }`.
- [ ] **Step 4: Run test** → PASS.
- [ ] **Step 5: Fix adapters**: real `tools/call` dispatch, unwrap `result.content[].text`, endpoint `:34343`, bearer token loading, remove phantom `neurostrata_detect_task_intents`, capture `ctx` inside `initialize`, add degraded-state reporting.
- [ ] **Step 6: Commit** `feat: phase 0 adapter/auth contracts and context DTOs`

---

## Task 2: Shared context service (read-planning boundary)

**Files:**
- Create: `src/context/source.rs`, `src/context/freshness.rs`, `src/context/plan.rs`, `src/context/service.rs`, `src/context/render.rs`, `src/context/mod.rs`
- Modify: `src/server.rs` (wire service into AppState)
- Test: `tests/context/service_test.rs`

**Interfaces:**
- Consumes: `VectorStore`, `GraphRead`
- Produces: `ContextService::plan(request) -> ContextPlan`, `ContextService::render(plan) -> String`

- [ ] **Step 1: Write failing test** — `plan()` returns empty plan with `degradation = Unsupported` when no `GraphRead` provided.
- [ ] **Step 2: Run test** → FAIL.
- [ ] **Step 3: Implement** pure functions: `source::resolve(path, project_root) -> SourceIdentity`, `freshness::compute(...) -> Freshness`, `plan::rank_dedup_budget(items, budget) -> Vec<ContextItem>` with cumulative per-call/turn/session budgets.
- [ ] **Step 4: Implement** `ContextService` consuming `GraphRead` + `VectorStore`, returning typed plan; `render` produces compact text with truncation markers.
- [ ] **Step 5: Run tests** → PASS.
- [ ] **Step 6: Commit** `feat: shared context read-planning service`

---

## Task 3: Graph-read capability + `memories_for_file`

**Files:**
- Create: `src/graph_read/mod.rs`, `src/graph_read/memories_for_file.rs`
- Modify: `src/traits.rs` (add `GraphRead` trait), `src/store/ladybug.rs` (implement), `src/server.rs` (handler + tool registration)
- Test: `tests/graph_read/memories_for_file_test.rs`

**Interfaces:**
- `GraphRead::memories_for_file(ns, source, path, symbol, include_ancestors, types, limit) -> Vec<MemoryHit>`
- `neurostrata_memories_for_file` MCP tool (inputs per spec §4.1)

- [ ] **Step 1: Write failing test** — file with governing `rule` returns it; unindexed file returns parent-directory rules; unsupported backend returns `unsupported` (not empty).
- [ ] **Step 2: Run test** → FAIL.
- [ ] **Step 3: Implement** `GraphRead` trait; `LadybugStore` implementation: normalize path, build candidate IDs (exact + ancestors), query `GOVERNS`/`RELATES_TO` edges, metadata fallback, 2-hop symbol governance, ranking per spec.
- [ ] **Step 4: Auto-materialize stub file nodes** in `upsert` when `GOVERNS` target missing.
- [ ] **Step 5: Register MCP tool** in `src/server.rs` tool list + handler.
- [ ] **Step 6: Run tests** → PASS.
- [ ] **Step 7: Commit** `feat: memories_for_file graph read tool`

---

## Task 4: Bounded `related` traversal

**Files:**
- Create: `src/graph_read/related.rs`
- Modify: `src/store/ladybug.rs`, `src/server.rs`
- Test: `tests/graph_read/related_test.rs`

**Interfaces:**
- `GraphRead::related(ns, seed, relations, direction, max_hops, types, limit, cursor) -> RelatedResult { nodes, edges, evidence, total, truncated, next_cursor }`

- [ ] **Step 1: Write failing tests** — depth-2 traversal returns edges; `RELATES_TO` edges marked supporting (not governance); cursor pagination rechecks retirement/privacy; cyclic graph terminates within budget.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** traversal with relation/direction allowlists, depth ≤2, node/edge/output budgets, query fingerprint + generation in cursor.
- [ ] **Step 4: Register MCP tool** `neurostrata_related`.
- [ ] **Step 5: Run tests** → PASS.
- [ ] **Step 6: Commit** `feat: bounded related traversal tool`

---

## Task 5: Source-scoped staged ingestion

**Files:**
- Modify: `src/parser/ingest.rs`, `src/store/ladybug.rs`
- Test: `tests/ingest/staged_publication_test.rs`

- [ ] **Step 1: Write failing test** — two worktrees ingest same namespace; queries by source identity return distinct generations; failed staging leaves previous generation active.
- [ ] **Step 2: Run test** → FAIL.
- [ ] **Step 3: Implement** source/generation-scoped physical IDs; staged build + atomic active-generation pointer flip; hash capture (`file_sha256`, `span_sha256`) on file/symbol nodes; stable symbol identity (body hash, not start line).
- [ ] **Step 4: Update** `memories_for_file` to filter by active generation.
- [ ] **Step 5: Run tests** → PASS.
- [ ] **Step 6: Commit** `feat: source-scoped staged ingestion`

---

## Task 6: Freshness computation (4 dimensions)

**Files:**
- Modify: `src/context/freshness.rs`, `src/server.rs` (`format_memory`, search, snapshot, get, file-recall, related)
- Test: `tests/context/freshness_test.rs`

- [ ] **Step 1: Write failing tests** — `changed` hash ⇒ `source currency = changed`; decision lifecycle independent of anchor drift; `exclude_stale` filters per definition; `not_applicable` distinct from `unknown`.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** lazy freshness computation in `src/context/freshness.rs` (pure comparisons + bounded `AnchorIo`); render all 4 dimensions on Line 2 of `format_memory` on every retrieval path.
- [ ] **Step 4: Update** search ranking: drifted `fact` forfeits `source_gain` + usage boost; `rule` surfaces with `[DRIFTED — verify or supersede]` banner.
- [ ] **Step 5: Snapshot** top-5 = 4 by `access_count` DESC + 1 reserved recent rule/decision slot (14-day window).
- [ ] **Step 6: Run tests** → PASS.
- [ ] **Step 7: Commit** `feat: 4-dimension freshness rendering`

---

## Task 7: Typed decision envelope (vocabulary v8)

**Files:**
- Modify: `src/schemas/memory-vocabulary.v3.json` (bump to v8, add `metadata.decision`)
- Modify: `src/server.rs` (`handle_add_memory`, `handle_supersede_memory`)
- Test: `tests/vocabulary/v8_decision_test.rs`

- [ ] **Step 1: Write failing tests** — v8 validates `metadata.decision` shape; conciseness budget warns >600, rejects >1200; advisory when `fact` lacks structured shape; legacy rows readable.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** vocabulary v8; conciseness check; `reverify` flag on supersede (anchor refresh vs. semantic verification).
- [ ] **Step 4: Run tests** → PASS.
- [ ] **Step 5: Commit** `feat: vocabulary v8 typed decision envelope`

---

## Task 8: Embedding-space identity (storage-wide)

**Files:**
- Modify: `src/embed.rs`, `src/store/ladybug.rs`, `src/traits.rs`
- Test: `tests/embed/identity_test.rs`

- [ ] **Step 1: Write failing tests** — incompatible descriptor refused on query/write/dedup; exact file/graph retrieval works during embedding outage.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** descriptor provenance `{ provider, model, revision, dims, normalization, preprocessing, distance_metric }` on namespace metadata; enforcement on every vector path; legacy migration rules.
- [ ] **Step 4: Run tests** → PASS.
- [ ] **Step 5: Commit** `feat: embedding-space identity enforcement`

---

## Task 9: Durable receipts + procedure state machine

**Files:**
- Create: `src/receipts/mod.rs`, `src/receipts/store.rs`
- Modify: `src/tools/procedure.rs`, `src/recall_metrics.rs`
- Test: `tests/receipts/receipt_test.rs`, `tests/procedure/state_machine_test.rs`

- [ ] **Step 1: Write failing tests** — receipt keyed by session/task/event; idempotent replay; procedure due/offered/acknowledged/performed; crash between counter update and episodic write recovers without double-count.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** event-keyed receipts with bounded retention; procedure state machine with CAS/transactional counter update and idempotent recovery.
- [ ] **Step 4: Run tests** → PASS.
- [ ] **Step 5: Commit** `feat: durable receipts and procedure state machine`

---

## Task 10: Host-triggered recall (shadow mode)

**Files:**
- Modify: `plugins/opencode/index.ts`, `plugins/openclaw/index.ts`
- Test: `tests/plugins/adapter_contract_test.ts` (or Rust integration)

- [ ] **Step 1: Write failing tests** — session-start preflight injects bounded block; file-touch prefetch calls `memories_for_file` with ≤512 tokens; topic-transition uses read-only recall; dedup by content revision + context_epoch; no `access_count` increment on injection.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** triggers in shadow mode (log only, no injection) first; then enable injection behind config flag.
- [ ] **Step 4: Run tests** → PASS.
- [ ] **Step 5: Commit** `feat: host-triggered recall (shadow mode)`

---

## Task 11: Privacy/egress & cross-scope authorization

**Files:**
- Modify: `src/embed.rs`, `src/store/ladybug.rs`, `src/context/service.rs`
- Test: `tests/privacy/egress_test.rs`

- [ ] **Step 1: Write failing tests** — same-namespace default for qualified targets; cross-namespace requires authorization; egress policy enforced before render and before host injection; denial distinct from not-found; private intermediate neighbors filtered.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** authorization inputs, policy enforcement, denial statuses.
- [ ] **Step 4: Run tests** → PASS.
- [ ] **Step 5: Commit** `feat: privacy/egress and cross-scope authorization`

---

## Task 12: Upgraded `search_memory` + compact renderer

**Files:**
- Modify: `src/server.rs` (search handler, format_memory, snapshot rendering)
- Test: `tests/search/upgraded_search_test.rs`

- [ ] **Step 1: Write failing tests** — `memory_types`, `fact_shape`, `path_prefix`, `exclude_stale` filters work; `code_ast` excluded by default; compact renderer bounded; mandatory-rule overflow behavior defined.
- [ ] **Step 2: Run tests** → FAIL.
- [ ] **Step 3: Implement** filters in `search_inner`; compact renderer with truncation markers.
- [ ] **Step 4: Run tests** → PASS.
- [ ] **Step 5: Commit** `feat: upgraded search and compact renderer`

---

## Task 13: NeuroPlasticity trace-graded loop

**Files:**
- Modify: `plasticity.json`, harness scripts, `src/learning/evaluator.rs`
- Create: NP-1..NP-4 scenario fixtures + controls
- Test: harness runs

- [ ] **Step 1: Infra gate** — pin base image digest; required mounts fail loudly; provider availability reported separately.
- [ ] **Step 2: Inline NP scenarios** (NP-1 hidden file governance, NP-2 2-hop, NP-3 stale vs live, NP-4 topic transition) + controls.
- [ ] **Step 3: Trace grading** — grade typed traces (task transitions, applied scope, pre-action delivery order, artifact/outcome); DB-file/commit-message proxies removed.
- [ ] **Step 4: Arm comparison** — no-memory / repaired-adapter / exact-file / graph-enabled; primary metric correct outcomes; privacy/missed guards hard failures.
- [ ] **Step 5: Commit** `feat: trace-graded NeuroPlasticity loop`

---

## Task 14: Final integration & whole-branch review

- [ ] **Step 1: Run full test suite** (`cargo test`, plugin tests).
- [ ] **Step 2: Run NeuroPlasticity harness** (infra gate first).
- [ ] **Step 3: Whole-branch review** by fresh reviewer.
- [ ] **Step 4: Commit** `chore: integration complete`