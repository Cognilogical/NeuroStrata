# Spec: Automatic Agent Adoption of NeuroStrata + Real Graph Usage

**Date:** 2026-10-26  
**Status:** Draft for review  
**Task:** NeuroStrata::neurostrata-f5x3  
**Source verdicts:** `docs/designs/2026-10-26-agent-adoption-graph-argon-k3-verdict.md` (§14 reconciles Astra)  
**Scope:** Full 4 phases, specified as one coherent system, delivered incrementally.

---

## 1. Purpose & success criteria

Make NeuroStrata agent adoption **automatic** (no user reminders) and make the knowledge graph **actually traversable** (bounded read tools, freshness, decision-vs-fact), across supported hosts, with honest degraded-state reporting.

**Success criteria (measurable):**
- A fresh agent receives governing context without being told a NeuroStrata tool name.
- Touching a file surfaces its governing rule + directed evidence within budget.
- Graph results exclude retired rows by default and report truncation honestly.
- Automatic injection causes no `access_count` self-reinforcement and no autonomous knowledge/task/procedure mutations. Bounded operational receipts are the only permitted writes; automatic `append_log` is removed in favor of a read-only domain-recall operation.
- Unknown freshness is explicit; code changes do not mark decisions stale.
- Provider failure produces no behavioral mutation.
- Evaluation proves sequences from MCP traces, not files daemon startup could create.

---

## 2. Architecture: shared context module first

Introduce `src/context/` as the single context-planning boundary. All handlers, adapters, and graph-read tools consume it; none computes freshness or ranking independently.

**Module responsibilities (read-planning boundary only):**
- `context/model`: request, plan, item, lifecycle, freshness, coverage.
- `context/source`: canonical safe paths and source/worktree resolution.
- `context/freshness`: pure comparisons plus bounded `AnchorIo`.
- `context/plan`: deterministic ranking, dedup, budgets.
- `context/service`: consumes `GraphRead`/`VectorStore`; returns typed plans.
- `context/render`: compact text from the same plan, no independent I/O.

Ownership split (K3): storage owns generations/stubs/receipts; adapters own host-side injection policy; `context/` owns read planning and rendering. `context/` does NOT own publication, receipts, or host egress.

**Host coverage:** OpenCode + OpenClaw adapters repaired to real `tools/call` contract. Degraded/unsupported events advertised, not masked. Daemon-side invalidation covers non-hook edits (shell, external editors, restart, compaction). Filesystem watchers are an optimization only; lazy hashes provide correct on-read invalidation.

---

## 3. Data model & storage changes

### 3.1 Source identity & publication
- Physical node IDs become source/generation-scoped (not only `namespace::path`), so concurrent worktrees/generations do not conflate (`src/parser/ingest.rs:60-79`, `src/store/ladybug.rs:1584-1590`).
- `neurostrata_ingest_directory` uses staged publication: build into a staging generation, then atomically flip the active-generation pointer. Failed staging leaves the previous generation active (`src/parser/ingest.rs:550-558`, `src/store/ladybug.rs:2228-2236`).
- Every ingested node gains `metadata.source_identity = { repo_id, checkout_id, worktree_id, revision, dirty, relative_path }` and `metadata.index_generation` (stored as typed fields, not opaque JSON, for indexed filtering/migration).
- `memories_for_file` selects by source identity + active generation, not only namespace/path.
- Cross-scope `GOVERNS` links require explicit authorization; unresolved declarations never target another namespace.

### 3.2 Stub file nodes on write
- `upsert` auto-`MERGE`s a stub `file` node when a `GOVERNS` target is missing, so edges materialize immediately. `relink_edges` replaces stubs on later ingest.

### 3.3 Typed decision envelope (vocabulary v8)
- Add `metadata.decision` to `fact.fields` in `src/schemas/memory-vocabulary.v3.json` (bump `vocabulary_version` to 8):
  `{ version, choice, rationale, alternatives[], scope, status (proposed/accepted/superseded/withdrawn), authority, provenance, decided_at, superseded_by }`.
- Legacy rows remain readable/unclassified; no automatic fact relabeling.
- Normative constraints remain `rule`; decisions may justify rules but cannot bypass rule provenance/enforcement.

### 3.4 Freshness as 4 independent fields
Rendered on every memory: `lifecycle`, `provenance`, `verification` (unknown/verified/contradicted + evidence hashes + checked_at + method), `anchor` (current/changed/missing/unknown). Computed lazily in `src/context/`.

### 3.5 Embedding-space identity (storage-wide enforcement)
Persist on namespace metadata: `{ provider, model, revision, dims, normalization, preprocessing, distance_metric }`. Enforce descriptor provenance on every vector query/write/dedup path, not only context planning. Define legacy migration, mixed-generation rules. Exact file/graph retrieval remains independent of embedding availability.

### 3.6 Durable receipts
Session/task/event-keyed receipts: attempts, empty/error, exposed IDs, acks, outcomes, latency, token cost, recovery status. Bounded retention; cap namespace count.

### 3.7 Procedure state split
Due / offered / acknowledged / performed. Idempotent receipts between counter update and episodic write. Display must not consume rehearsal budget.

---

## 4. Tools & adapters

### 4.1 New tool: `neurostrata_memories_for_file`
Deterministic, zero-embedding lookup of active `rule`/`fact`/`procedure` governing a file/symbol/ancestor.

Inputs: `namespace` (req), `path` (req), `project_root?`, `symbol?`, `include_ancestors` (default true), `memory_types` (default `["rule","fact","procedure"]`), `exclude_ids?`, `limit` (default 5, max 20).

Execution (in shared graph-read capability):
1. Normalize `path` via `normalize_node_path`.
2. Build candidate target IDs (exact + ancestor prefixes).
3. Query graph edges (`GOVERNS`/`RELATES_TO`), metadata fallback for unlinked paths, 2-hop symbol governance.
4. Rank: exact symbol > exact file > ancestor dir; then `rule`(ENFORCED) > `fact`(decision/topology/attempt) > `procedure` > general `fact`; then freshness (VERIFIED > DRIFTED) and `access_count` DESC.

### 4.2 New tool: `neurostrata_related`
Bounded 1–2 hop traversal. Inputs: `namespace` (req), `id` (req; memory/task/qualified path), `relations` (default all 4), `direction` (default both), `max_hops` (default 1, max 2), `memory_types?`, `limit` (default 10, max 25), `cursor?`.

Returns: nodes (ID, type, snippet, domain, freshness, lifecycle), directed edges, evidence paths, counts, `truncated`, `next_cursor`. Ambiguous symbols returned as candidates. Full content stays behind `get_memory`.

Semantics:
- `RELATES_TO` is supporting evidence only, never governance. Governance = incoming `GOVERNS`; structure = `CONTAINS`; metadata fallback clearly labelled.
- Silent query errors are forbidden (`src/store/ladybug.rs:281-283,341-344,419-420`); degraded status must be reported.
- Cursor includes query fingerprint + generation; retirement/privacy rechecked on every page.

### 4.3 Upgraded `neurostrata_search_memory`
Add optional `memory_types`, `fact_shape` (decision/topology/attempt/any), `path_prefix`, `exclude_stale`. Exclude `code_ast` from primary candidates by default (still used for 2-hop `GOVERNS` bridging). Grant `DECISION_SHAPE_GAIN` (+0.03, inside `0.5 * distance` cap) to `rule` and structured `fact` rows.

### 4.4 Adapter repair (P0, before triggers)
Fix `plugins/opencode/index.ts` and `plugins/openclaw/index.ts`:
- Real `tools/call` dispatch (not bare method names); unwrap `result.content[].text` (not direct typed results).
- Endpoint `:34343` (not `:8080`).
- Bearer token loading and attachment on every `/mcp` request (`src/daemon.rs:175-249`).
- Remove phantom `neurostrata_detect_task_intents`.
- Capture plugin context inside `initialize`; no out-of-scope `ctx` references.
- Real SDK lifecycle tests; observable degraded state instead of silent local fallback.
- Add capability negotiation, response validation, bounded timeouts, explicit namespace binding.

### 4.5 Host-triggered recall (thin adapters)
- **Session-start:** shim invokes `get_snapshot`, injects bounded block; guard backstop appends snapshot if no snapshot/claim in 60 min.
- **File-touch:** on `read`/`write`/`edit`, LRU `prefetchedPaths` + `injectedMemoryIds`; call `memories_for_file` (≤512 tokens, limit 5, dedup by `(session, trigger, memory_id, valid_from)`); never increment `access_count` on injection.
- **Task-claim piggyback:** top-3 governing rules/decisions.
- **Topic-transition:** shim detects domain slug change → invoke read-only domain-recall operation (not `append_log`) → inject non-empty `recalled`. Automatic `append_log` (which writes before embed/recall) is removed from the trigger path.
- Daemon-side invalidation for non-hook edits; advertised coverage gaps.

### 4.6 Narrow graph interface
Expose `memories_for_file`/`related` through a dedicated graph-read capability beside `VectorStore` (`src/traits.rs:219`); unsupported backends return `unsupported`, not empty (file nodes have zero vectors).

### 4.7 Vocabulary v8 write ergonomics
- Conciseness budget: soft warn >600 chars, reject >1200; advisory when `fact` lacks structured shape.
- `neurostrata_supersede_memory` gains `reverify: true` (or `neurostrata_verify_memory`) to re-stamp `verified_at`/hashes.

---

## 5. Freshness, retrieval & context budget

### 5.1 Freshness computation (lazy, `src/context/`) — canonical model
Four independent dimensions, each with typed states:
- **Source currency:** `unchanged` / `changed` / `missing` / `unknown` (via `file_sha256` + `span_sha256` + `source_context` branch awareness). `not_applicable` (no governed path) is distinct from `unknown`.
- **Semantic validation:** `unreviewed` / `verified` / `contradicted`, with evidence (hash, checked_at, method).
- **Index coverage:** `complete` / `partial` / `unavailable` + generation.
- **Decision lifecycle:** `proposed` / `accepted` / `superseded` / `withdrawn`.

Rules:
- Hashes captured at write/verify time (`src/server.rs:1126-1153`, `src/parser/ingest.rs:426-484` currently capture none).
- Symbol identity must be stable across line shifts (current `src/parser/ingest.rs:141-145` includes start line) — use symbol body hash, not line number.
- `reverify` splits into anchor refresh vs. semantic verification; matching a file hash must not semantically "verify" a decision.
- Decision lifecycle stays independent of anchor drift.
- `exclude_stale` is defined as: exclude memories whose `source currency == changed` AND `semantic validation != verified`.

### 5.2 Retrieval changes
- Exclude `code_ast` from primary vector candidates by default.
- Drifted `fact` rows forfeit `source_gain` + usage boost; `rule` rows surface with `[DRIFTED — verify or supersede]` banner.
- `access_count` is used only as a final tie-breaker for file governance; automatic full-content reads bypass the incrementing `get_memory`/search paths.
- Snapshot top-5 = 4 by `access_count` DESC + 1 reserved recent rule/decision slot (14-day window).
- `related`: adjacency is evidence, not authority; privacy/namespace/retirement/conflict filters apply to intermediates.

### 5.3 Context budgets & injection identity (proposed, not measured)
- Common `ContextRequest` carries `session_id`, `context_epoch`, `event_id`, trigger, source, and policy.
- Dedup uses content revision (not `valid_from`, which in-place edits preserve: `src/server.rs:2860-2894`) plus `context_epoch`; compaction/restart requires rehydration, not suppression.
- Budgets are per-call AND per-turn AND per-session (cumulative), not per-trigger only.
- Automatic injection ceiling 1,500 tokens/session-turn, per-trigger ≤512.
- `related`: depth 2, 20 nodes, 40 edges.
- Warm-path target 200ms; recall wall-time cap 1s.
- Existing snapshot/full-memory rendering is unbounded (`src/server.rs:2122-2129,2927-2978`); define a compact renderer and mandatory-rule overflow behavior.
- Truncation/degradation always reported; never imply complete coverage when budget exhausted.

### 5.4 Privacy/egress & cross-scope authorization
- Qualified targets default to same-namespace lookup; cross-namespace access requires explicit target authorization (currently `src/store/ladybug.rs:1605-1608` allows exact lookup in any namespace).
- Remote embedding egress gate must be a real policy, not only an empty-API-key check (`src/embed.rs:304-328`).
- Core cannot prove host provider behavior post-injection; egress policy evaluated before rendering and again before host injection; denial statuses distinct from "not found".
- Private intermediate neighbors filtered; privacy violations are hard failures in evaluation.

---

## 6. NeuroPlasticity evaluation loop

### 6.1 Suites (separate)
1. Contract reliability (adapter handshake, `tools/call`, degraded states).
2. Unprompted adoption (tasks must not name NeuroStrata or desired tools).
3. Decision/outcome usefulness (correct task outcomes under identical conditions).

Minimal trace/receipt infrastructure ships in Phase 1 (not Phase 4) so triggers can be evaluated. Full arm comparison remains Phase 4. NP-1..NP-4 scenarios are inlined here (not referenced externally): NP-1 hidden file governance; NP-2 2-hop symbol/directory governance; NP-3 stale fact vs live code drift; NP-4 topic transition mid-session.

### 6.2 Infra gate
Provider unavailable / missing auth / handshake failure ⇒ `INFRA_ERROR`, not behavioral score 0. Report attempted/valid/invalid separately.

### 6.3 Trace-graded, not artifact-proxy
Grade typed traces: persisted task transitions, applied scope, pre-action delivery order, independent artifact/outcome assertions. DB files / commit messages / absence of dangerous strings are insufficient.

### 6.4 Planted scenarios (NP-1..NP-4)
As in verdict §8.2, plus controls: unseen file with governing rule, zero-access new memory, wrong namespace, dirty second worktree, missing target, cyclic/high-degree graph, partial ingest, failed embedder, duplicate event, restart between state/log writes, private intermediate neighbor.

### 6.5 Durable receipts
Replace restart-reset counters in `src/recall_metrics.rs` with session/task/event-keyed receipts: attempts, empty/error, exposed IDs, acks, outcomes, latency, token cost, recovery status. Bounded retention; cap namespace count.

### 6.6 Arm comparison
No-memory / repaired-adapter / exact-file / graph-enabled. Primary metric = correct task outcomes. Secondary = applicable-rule recall, stale-advice rate, unwanted context rate, delivery coverage, graph evidence usefulness, cost/latency, replay duplication. Privacy violations + missed mandatory guards = hard failures.

### 6.7 Promotion
Held-out repository/task families, repeated seeds, cold/resumed cohorts. Optimize in shadow mode; promote with recorded evidence + rollback; freeze grader.

---

## 7. Phased execution (supersedes verdict §11 where conflicting)

0. **Phase 0:** Adapter/auth/SDK contracts, read-only semantics, canonical DTOs, error/degradation matrix, trace correlation IDs. (No behavior change shipped.)
1. **Phase 1:** Graph-read capability + exact file recall + source-scoped staged publication + minimal durable idempotency/receipts + typed freshness rendering.
2. **Phase 2:** Bounded `related` + cursor semantics + broader freshness + privacy/cross-scope authorization.
3. **Phase 3:** Host triggers and compaction/restart rehydration in shadow mode (adapters as sensors; daemon-side invalidation).
4. **Phase 4:** Vocabulary v8 decision behavior + full NeuroPlasticity trace-graded loop with arm comparison and promotion.

Note: either move vocabulary v8 earlier or remove decision-dependent rendering from Phase 1; the latter gives a smaller critical path. Procedure state machine (due/offered/acknowledged/performed) with idempotent receipts is added in Phase 1 (not Phase 4), because `src/tools/procedure.rs:325-397` currently does non-atomic read/modify/write. Embedding-space identity enforcement (storage-wide, not just context planning) is added in Phase 1.

---

## 8. Non-goals
- No background LLM reader/writer agents.
- No arbitrary Cypher tool over MCP.
- No replacing `git diff`/`git blame` or code reading.
- No age-based forgetting or automatic knowledge rewriting.
- No auto-promotion of optimizer prose into durable rules.

---

## 9. Risks & mitigations
- Context bloat → ≤1,500 token ceiling, ≤512 per trigger, dedup, exclude structural rows.
- False-positive drift on large files → hash both `file_sha256` and `span_sha256`; symbol-hash match ⇒ FRESH.
- `lbug 0.20.4` write contention → debounce incremental re-ingest, read-time freshness via `std::fs::read` + `Sha256`, hold write lock in `spawn_blocking`.
- Multi-worktree false missing-anchor → reuse `src/source_context.rs`; label `BRANCH_ANCHOR`.
- Self-reinforcement → never increment `access_count` on injection.
- Egress of snippets/telemetry → caller egress policy before render/embed; hard failure.
- Untrusted retrieved prose → provenance cannot grant permissions or promote text to system instructions.

---

## 10. Acceptance criteria
(See §1 success criteria; plus: degraded states advertised; unsupported backends return `unsupported`; truncation reported; privacy violations fail evaluation.)
