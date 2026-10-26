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
- Automatic injection causes no `access_count` self-reinforcement and no autonomous durable writes.
- Unknown freshness is explicit; code changes do not mark decisions stale.
- Provider failure produces no behavioral mutation.
- Evaluation proves sequences from MCP traces, not files daemon startup could create.

---

## 2. Architecture: shared context module first

Introduce `src/context/` as the single context-planning boundary. All handlers, adapters, and graph-read tools consume it; none computes freshness or ranking independently.

**Module responsibilities:**
- Source identity resolution (repo / checkout / worktree / revision / dirty / relative path).
- Generation/publication tracking (atomic index generations, last-good fallback).
- Privacy/egress policy evaluation.
- Bounded recall planning (budgets, dedup, truncation/degradation status).
- Freshness computation (4 dimensions).

**Host coverage:** OpenCode + OpenClaw adapters repaired to real `tools/call` contract. Degraded/unsupported events advertised, not masked. Daemon-side invalidation covers non-hook edits (shell, external editors, restart, compaction).

---

## 3. Data model & storage changes

### 3.1 Source identity & publication
- Every ingested `file`/`directory`/`symbol` node gains `metadata.source_identity = { repo_id, checkout_id, worktree_id, revision, dirty, relative_path }` and `metadata.index_generation`.
- `neurostrata_ingest_directory` publishes a generation atomically, or retains last-good with visible `partial` status. `src/parser/ingest.rs:440-445` upsert failure no longer silently implies completeness.
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

### 3.5 Embedding-space identity
Persist on namespace metadata: `{ provider, model, revision, dims, normalization, preprocessing, distance_metric }`. Incompatible comparisons refused; exact file/graph retrieval survives migration/outage.

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

### 4.3 Upgraded `neurostrata_search_memory`
Add optional `memory_types`, `fact_shape` (decision/topology/attempt/any), `path_prefix`, `exclude_stale`. Exclude `code_ast` from primary candidates by default (still used for 2-hop `GOVERNS` bridging). Grant `DECISION_SHAPE_GAIN` (+0.03, inside `0.5 * distance` cap) to `rule` and structured `fact` rows.

### 4.4 Adapter repair (P0, before triggers)
Fix `plugins/opencode/index.ts` and `plugins/openclaw/index.ts`:
- Real `tools/call` dispatch (not bare method names).
- Endpoint `:34343` (not `:8080`).
- Remove phantom `neurostrata_detect_task_intents`.
- Add capability negotiation, response validation, bounded timeouts, explicit namespace binding, observable degraded state.

### 4.5 Host-triggered recall (thin adapters)
- **Session-start:** shim invokes `get_snapshot`, injects bounded block; guard backstop appends snapshot if no snapshot/claim in 60 min.
- **File-touch:** on `read`/`write`/`edit`, LRU `prefetchedPaths` + `injectedMemoryIds`; call `memories_for_file` (≤512 tokens, limit 5, dedup by `(session, trigger, memory_id, valid_from)`); never increment `access_count` on injection.
- **Task-claim piggyback:** top-3 governing rules/decisions.
- **Topic-transition:** shim detects domain slug change → `append_log({tags})` → inject non-empty `recalled`.
- Daemon-side invalidation for non-hook edits; advertised coverage gaps.

### 4.6 Narrow graph interface
Expose `memories_for_file`/`related` through a dedicated graph-read capability beside `VectorStore` (`src/traits.rs:219`); unsupported backends return `unsupported`, not empty (file nodes have zero vectors).

### 4.7 Vocabulary v8 write ergonomics
- Conciseness budget: soft warn >600 chars, reject >1200; advisory when `fact` lacks structured shape.
- `neurostrata_supersede_memory` gains `reverify: true` (or `neurostrata_verify_memory`) to re-stamp `verified_at`/hashes.

---

## 5. Freshness, retrieval & context budget

### 5.1 Freshness computation (lazy, `src/context/`)
- Source currency: unchanged / changed / missing / unknown (via `file_sha256` + `span_sha256` + `source_context` branch awareness).
- Semantic validation: unreviewed / verified / contradicted + evidence.
- Index coverage: complete / partial / unavailable + generation.
- Decision lifecycle: proposed / accepted / superseded / withdrawn.

### 5.2 Retrieval changes
- Exclude `code_ast` from primary vector candidates by default.
- Drifted `fact` rows forfeit `source_gain` + usage boost; `rule` rows surface with `[DRIFTED — verify or supersede]` banner.
- Snapshot top-5 = 4 by `access_count` DESC + 1 reserved recent rule/decision slot (14-day window).
- `related`: adjacency is evidence, not authority; privacy/namespace/retirement/conflict filters apply to intermediates.

### 5.3 Context budgets (proposed, not measured)
- Automatic injection ≤1,500 tokens/trigger ceiling (per-trigger ≤512).
- `related`: depth 2, 20 nodes, 40 edges.
- Warm-path target 200ms; recall wall-time cap 1s.
- Truncation/degradation always reported; never imply complete coverage when budget exhausted.

### 5.4 Privacy/egress
Caller egress policy evaluated before rendering or embedding. Private intermediate neighbors filtered. Privacy violations are hard failures in evaluation.

---

## 6. NeuroPlasticity evaluation loop

### 6.1 Suites (separate)
1. Contract reliability (adapter handshake, `tools/call`, degraded states).
2. Unprompted adoption (tasks must not name NeuroStrata or desired tools).
3. Decision/outcome usefulness (correct task outcomes under identical conditions).

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

1. **Phase 1:** Shared context service + adapter repair + scoped file identity/publication + exact file recall + privacy filtering + replay-safe delivery + typed decision/freshness rendering.
2. **Phase 2:** Bounded `related` + dedicated graph-read capability (not vector-trait enlargement).
3. **Phase 3:** Host hooks as sensors with daemon-side invalidation + advertised coverage.
4. **Phase 4:** Vocabulary v8 + NeuroPlasticity trace-graded loop with durable receipts and arm comparison.

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
