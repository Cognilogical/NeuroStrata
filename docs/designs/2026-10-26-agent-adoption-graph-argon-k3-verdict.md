# Unified Verdict: Automatic Agent Adoption of NeuroStrata + Real Graph Usage

**Date:** 2026-10-26  
**Seats:** Argon (`gemini-arch`), K3 (`k3-arch`), Bosun (orchestrator)  
**Scope:** Design-only. No code changed.  
**Task:** `NeuroStrata::neurostrata-rr3k`  
**Baseline:** NeuroStrata `v1.13.0`, vocabulary v7, `src/schemas/memory-vocabulary.v3.json`, `plugins/opencode/index.ts`, `plasticity.json`.

---

## 1. Executive summary

Both architects independently converge on the same conclusion, and Bosun concurs:

1. **More prompts will not fix adoption.** The existing `AGENTS.md`/skill mandates are already long and partly stale (e.g., the skill claims `append_log` is "not shipped" while `src/buffer.rs:335-380` implements tagged recall). Adding more prose worsens drift. Adoption must be **host-triggered and deterministic**, not voluntary.
2. **Background LLM reader/writer agents are rejected** (per `docs/architecture/MEMORY_AGENTS_VERDICT.md` §1). Automation stays deterministic: client shims call existing MCP tools on session start, file touch, and topic transition; no LLM in the loop.
3. **The graph is write-mostly today.** Search already does bounded 1-hop/2-hop `GOVERNS` expansion (`src/store/ladybug.rs:224-258`), but there is no deterministic "start at file/symbol/id and traverse" tool, and `GOVERNS` edges silently fail to materialize when a memory is written before its target is ingested (`src/store/ladybug.rs:1639`, `:3256-3281`).
4. **Freshness is invisible.** `format_memory` (`src/server.rs:2927-2978`) renders no verification state; `access_count` measures attention, not truth. Freshness must be shown as independent evidence fields, and "unknown" must never be rendered as "fresh."
5. **Decisions and facts must be distinguished.** Code facts rot; decisions/rationales/negative exemplars are the high-value payload. Add a `metadata.decision` sub-object to `fact` (vocabulary v8) rather than a new top-level type or memory type.
6. **The existing NeuroPlasticity evidence is invalid.** `plasticity.json` names the desired tools in the prompt, runs 1 epoch, and the recorded run is a provider/infra failure (`score 0.0`, empty stdout). No optimizer mutation may be derived from it. A trace-graded, infra-gated loop is required.

---

## 2. Evaluation of the downstream agent's suggestions

| Suggestion | Ruling | Priority | Notes |
|---|---|---|---|
| "Code/runbooks are more accurate; memories have no freshness signal." | **VALID** | P0 | `format_memory` renders no `verified_at`/hash; `ExportFreshnessDirty` tracks graph export staleness, not memory truth. |
| "Never uses graph: no traversal tools; `governs` is just file paths." | **PARTIALLY VALID** | P0 | Vector search *does* expand `GOVERNS` 1–2 hops, but there is no path/symbol/id entry point; edges are often missing due to write-before-ingest. |
| "Memories are dense narrative; value is in decisions, not facts." | **VALID** | P1 | `search_inner` (`src/store/ladybug.rs:89`) excludes `directory/file/markdown/task` but leaves `code_ast` rows competing with `rule`/`fact` in the same vector pool. |
| Tool: `neurostrata_memories_for_file(path)` | **ADOPT** | P0 | Implements the already-prescribed R1 file-touch trigger (`MEMORY_AGENTS_VERDICT.md` §3). |
| Tool: `neurostrata_related(id)` | **ADOPT** | P1 | `get_memory` dumps raw arrays and misses incoming edges (declarations are one-way, `declaration_direction: "self_to_target"`). |
| Tool: graph-aware search | **MODIFY** | P1 | Do **not** expose raw Cypher (injection surface). Add `memory_types`, `fact_shape`, `path_prefix`, `exclude_stale` filters to `search_memory`. |
| Stronger AGENTS/SKILL mandates | **REJECT as primary fix** | — | Host events must trigger usage; instructions should be shortened and generated from shipped capability. |
| Background LLM reader/writer | **REJECT** | — | Deterministic reads; main agent keeps write judgment (Guarded Curation / LLM08). |
| Arbitrary Cypher tool | **REJECT** | — | Typed tools only. |
| `append_log` recall as sufficient | **REJECT** | — | Keep as the topic-tag seat; add session-start and file-touch seats. |
| Age/access-count decay | **REJECT** | — | Age ≠ falsity; retrieval ≠ validation. |
| New top-level `decision` type | **REJECT initially** | — | Use `metadata.decision` on `fact`, consistent with `topology`/`attempt`. |
| Optimize from current score-0 run | **REJECT** | — | Infra failure must be fixed first. |

---

## 3. Unified architecture: automatic, host-triggered recall

### 3.1 Fix the client shim before adding triggers (P0)

`plugins/opencode/index.ts` currently **does not match the shipped MCP contract**:

- It sends tool names as JSON-RPC methods (`plugins/opencode/index.ts:85-110`); Core dispatches via `"tools/call"` (`src/server.rs:571-585`).
- Default endpoint `:8080` (`index.ts:66-70`) vs setup default `:34343` (`src/setup.rs:137-150`).
- It calls `neurostrata_detect_task_intents`, absent from the server tool list, then masks failure with local detection.
- It has no snapshot or graph-recall trigger.

**Action:** Prove the shim against the real client SDK and live `initialize → tools/list → tools/call` flow before any trigger logic. This is the same blocker as Composite Rec #17 (Zero-Action Start honesty).

### 3.2 Four deterministic lifecycle hooks (shim + server piggyback)

1. **Session-start auto-preflight.** Shim invokes `neurostrata_get_snapshot({ namespace, project_root })` before first mutation and injects a bounded block. Server backstop: `neurostrata_guard_validate` tracks `last_snapshot_at` in `AppState`; if no snapshot/claim in 60 minutes, its rejection payload appends the compact snapshot.
2. **File-touch prefetch on `read`/`write`/`edit`.** Shim maintains `prefetchedPaths` (LRU, cap 128) and `injectedMemoryIds`. On a new repo path, call `neurostrata_memories_for_file({ namespace, path, project_root, max_tokens: 512, limit: 5, exclude_ids })`; append the governance block and record IDs. **Zero agent discipline required** — agents always read/edit files.
3. **Task-claim contextual prefetch.** `handle_task_claim` (`src/server.rs`) piggybacks top-3 governing rules/decisions from the task title/description and `EXTRACTED_FROM`/`RELATES_TO` neighbors of `parent_id`/`blocked_by`.
4. **Topic-transition auto-recall.** Shim compares current vs. previous turn's domain slugs (from cached `registered_domains` in snapshot); on a new slug, invokes `neurostrata_append_log({ tags: [slug] })` and injects non-empty `recalled`. (Tagged recall already works in `src/buffer.rs:335-380`.)

### 3.3 Fix the snapshot cold-start hole

`handle_get_snapshot` (`src/server.rs:1851-1868`) sorts strictly by `access_count` DESC, starving newly written rules (`access_count: 0`). Change top-5 allocation to **4 by `access_count` DESC + 1 reserved "recent rule/decision" slot** (newest active `rule` or `fact` with `decision`/`topology`/`attempt` and `valid_from` within 14 days, not already in top-4).

### 3.4 Injection policy (both architects agree)

- ≤512 tokens per trigger; LRU dedup by `(session, trigger, memory_id, valid_from)`.
- Include provenance, freshness, and evidence.
- **Never increment `access_count` merely because context was injected** (prevents self-reinforcement).
- Recall failure produces one explicit warning and never blocks unrelated work.

---

## 4. Graph read interface

### 4.1 Root cause: edges don't materialize on write-before-ingest

When `add_memory` declares `locations`, `handle_add_memory` sets `metadata.governs`/`metadata.refs` (`src/server.rs:1273-1286`), but if the target isn't an ingested node, `resolve_target_on_write` (`src/store/ladybug.rs:1639`) marks it `Missing` and **creates no `GOVERNS` edge**. Nor is a lightweight stub node created.

**Fix:** In `upsert` (`src/store/ladybug.rs`), when `edge.rel_type == "GOVERNS"` and `resolve_target_on_write` returns `Missing`, auto-`MERGE` a stub `file`/`directory` node (`id = qualified_id(ns, normalized_path)`, `memory_type = "file"`, `user_id = "auto-ingestor"`, `location = normalized_path`, `metadata = {"stub": true}`, `embedding = zero_vector`) and immediately materialize the `GOVERNS` edge. `neurostrata_ingest_directory` later replaces the stub via `clear_ingested` + `relink_edges` (`src/store/ladybug.rs:2152-2240`). Result: `GOVERNS` edges exist 100% of the time after write.

### 4.2 Add two `VectorStore` methods (`src/traits.rs:219`)

```rust
async fn memories_for_file(
    &self, namespace: &str, normalized_path: &str,
    symbol: Option<&str>, include_ancestors: bool,
    memory_types: &[String], limit: usize,
) -> Result<Vec<SearchResult>>;

async fn related_memories(
    &self, namespace: &str, seed_id: &str,
    relations: &[String], direction: EdgeDirection, // Outgoing|Incoming|Both
    max_hops: u8, // 1 or 2
    memory_types: &[String], limit: usize,
) -> Result<Vec<SearchResult>>;
```

### 4.3 Tool: `neurostrata_memories_for_file` (NEW, P0)

Deterministic, zero-embedding lookup of active `rule`/`fact`/`procedure` governing a file, symbol, or ancestor directory. Hot path for file-touch prefetch.

Input: `namespace` (req), `path` (req), `project_root?`, `symbol?`, `include_ancestors` (default true), `memory_types` (default `["rule","fact","procedure"]`), `exclude_ids?`, `limit` (default 5, max 20).

Execution in `LadybugStore`:
1. Normalize `path` via `normalize_node_path` (`src/parser/ingest.rs:48`); build candidate target IDs (exact path + ancestor prefixes).
2. Query 1 (graph edge): `MATCH (m:Memory)-[e:GOVERNS|RELATES_TO]->(t:Memory)` where `m` is active, non-structural, and `t.id IN $target_ids OR t.location IN $raw_paths`.
3. Query 2 (metadata fallback for unlinked paths): rows where `m.location IN $raw_paths` or `metadata.governs`/`metadata.refs` contains the path.
4. Query 3 (2-hop symbol governance): `(m)-[:GOVERNS]->(s)<-[:CONTAINS]-(f)` where `f.id == $exact_file_id`.
5. Rank: exact symbol > exact file > ancestor dir; then `rule` (ENFORCED) > `fact` (decision/topology/attempt) > `procedure` > general `fact`; then freshness (VERIFIED > DRIFTED) and `access_count` DESC.

### 4.4 Tool: `neurostrata_related` (NEW, P1)

Bidirectional 1–2 hop neighborhood traversal from any memory ID, task ID, or qualified file ID (`NeuroStrata::src/server.rs`). Returns nodes, directed edges, evidence paths, explicit counts, `truncated`, and `next_cursor`.

Input: `namespace` (req), `id` (req), `relations` (default all four: `GOVERNS, CONTAINS, RELATES_TO, EXTRACTED_FROM`), `direction` (default `both`), `max_hops` (default 1, max 2), `memory_types?`, `limit` (default 10, max 25), `cursor?`.

Return structured: node list (ID, type, snippet, domain, freshness, lifecycle), edge list (source, target, relation, direction), evidence paths, `total`, `truncated`, `next_cursor`. Ambiguous symbols returned as candidates, not guessed. Full content stays behind `get_memory`.

### 4.5 Upgrades to `neurostrata_search_memory` (`src/server.rs:188`)

Add optional params pushed into `search_inner` (`src/store/ladybug.rs:99`):
- `memory_types: string | string[]` — filter by type.
- `fact_shape: "decision" | "topology" | "attempt" | "any"` — filter `fact` by structured metadata.
- `path_prefix: string` — scope to memories whose `location` or `metadata.governs` starts with prefix.
- `exclude_stale: boolean` (default false) — suppress drifted memories.
- Also exclude `code_ast` from primary vector candidates by default (unless `include_symbols: true` or `memory_types` includes `"symbol"`/`"code_ast"`). `code_ast` nodes remain in graph for 2-hop `GOVERNS` bridging.
- Grant `DECISION_SHAPE_GAIN` (+0.03, inside the `0.5 * distance` cap) to `rule` rows and `fact` rows carrying valid `metadata.decision`/`topology`/`attempt`.

---

## 5. Freshness model (evidence, not scalar)

Return four independent fields on every rendered memory (in `format_memory`, `src/server.rs:2927`):

- **`lifecycle`**: active / retired / archived / conflicting.
- **`provenance`**: source, capture time, `valid_from`.
- **`verification`**: `unknown | verified | stale`, with `checked_at`, `method`, evidence (hashes).
- **`anchor`**: `current | changed | missing | unknown`, evaluated by the configured source provider (no Core Git-specific logic; reuse `src/source_context.rs:57-76` provenance).

**Rules:**
- Missing verification ⇒ `unknown`, never "fresh."
- Age alone never retires or demotes a row.
- Corrections still use `supersede_memory`; automatic drift detection must not silently rewrite knowledge.
- `file_sha256` and `span_sha256` stamped on write (`add_memory`/`supersede_memory`) and ingest (`src/parser/ingest.rs:321`), using `sha2::Sha256` (already a dependency). Render on Line 2 of `format_memory`:
  ```
  Type: rule [ENFORCED] | Freshness: VERIFIED (2026-10-26, span_sha256 match) | Source: owner 2026-10-20
  ```
- If governed file exists and span hash matches ⇒ `VERIFIED`. If file changed ⇒ `DRIFTED` with banner. If path absent but carries `source_context` ⇒ `BRANCH_ANCHOR`. If no governed path ⇒ `UNANCHORED`.
- Ranking: drifted `fact` rows forfeit `source_gain` and usage boost (`capped_gain = 0.0`); `rule` rows still surface with loud `[DRIFTED — verify or supersede]` banner so constraints are not silently bypassed.
- `span_sha256` (not just `file_sha256`) prevents false drift when unrelated lines change in a large file.

---

## 6. Decision-vs-fact framing (vocabulary v8)

Bump `vocabulary_version` to **8** in `src/schemas/memory-vocabulary.v3.json`. Add to `memory_types.fact.fields`:

```json
"decision": {
  "type": "object",
  "definition": "Records an architectural or design decision. Required: choice (what was decided), rationale (why over alternatives), rejected_alternatives (what was considered and rejected, or 'none'). Extra keys ignored."
}
```

Consistent with existing `topology` and `attempt` sub-shapes.

### Write-time anti-bloat (`handle_add_memory`, `src/server.rs:1446`)

- **Conciseness budget:** If `memory_type` is `rule` or `fact` and `content.chars().count() > 600`, return soft warning; reject if `> 1200`. Instruct: keep content to 1–3 crisp sentences; put structured detail in `metadata.decision`/`topology`/`attempt`.
- **Discourage ephemeral code-fact duplication:** If `fact` lacks `metadata.decision`/`topology`/`attempt`/`extracted_from`, append advisory: "NeuroStrata is highest-leverage for rules and structured facts; avoid restating what code currently does."

### Rendering

Render `Record: DECISION` distinct from `FACT`, `RULE`, and failed `ATTEMPT`. Facts may go stale when evidence changes; decisions remain historical choices until explicitly reversed/superseded; code drift alone does not stale them.

---

## 7. Incremental re-ingest (avoid full wipe)

Currently `neurostrata_ingest_directory` calls `clear_ingested` (`src/store/ladybug.rs:2228`), wiping all AST symbols.

Add `ingest_file(namespace, project_root, file_path)`:
- Triggered asynchronously on `ThalamicBus` when shim completes `write`/`edit`, or when `memories_for_file` detects `file_sha256` mismatch on an ingested file.
- Compare disk `file_sha256` against stored `NeuroStrata::<rel_path>` node; no-op if unchanged (<1ms).
- If changed: delete only `code_ast` children for that file, re-parse with tree-sitter, embed only symbols whose `span_sha256` changed, upsert, scoped `relink_edges`.
- Debounce (5s quiet window per file or on `task_complete`) to avoid `lbug 0.20.4` write-lock contention (Composite Rec #5: hold write lock inside `spawn_blocking`).
- Add `reverify: true` flag on `supersede_memory` (or `neurostrata_verify_memory`) to re-stamp `verified_at`/hashes when an agent confirms a `[DRIFTED]` memory still holds.

---

## 8. NeuroPlasticity closed loop (corrected)

The current `plasticity.json` is invalid for prompt optimization: prompt names desired tools, 1 epoch, and the recorded run is infra failure (provider "Endpoint is unavailable", score 0.0, empty stdout). Fix infrastructure first.

### 8.1 Infra gate (hard prerequisite)

- Missing auth, provider failure, MCP handshake failure, or setup failure ⇒ `INFRA_ERROR`; no agent penalty, no optimizer input.
- Pin base image digest (Composite Rec #26); required mounts fail loudly (not `|| true`).
- Use `epochs >= 3` (baseline + mutations).
- Verify upstream NeuroPlasticity schema accepts the chosen `meta_llm.provider` (current manifest uses `typesafe`; schema accepts `custom|embedded`).

### 8.2 Trace-graded planted scenarios

Grade MCP call sequences, not artifact existence (current DB-file/commit-message checks cannot prove tool usage). Planted scenarios:

| ID | Scenario | Pass criterion |
|---|---|---|
| NP-1 | Hidden file governance: `rule` governing `src/db/pool.rs` | Agent calls `memories_for_file` before `edit`; `max_connections <= 4` in final file. |
| NP-2 | 2-hop symbol/directory governance: failed `attempt` linked via `RELATES_TO` to `src/sync/` rule | `rayon` absent from `Cargo.toml`/`linker.rs`; graph tool appears in MCP log. |
| NP-3 | Stale fact vs. live code drift: `fact` claims 8080 anchored to `src/config.rs:12` (old hash); code says 9090; `rule` with `decision` explains 9090 | Agent trusts 9090, supersedes drifted 8080 fact, cites 9090 rationale. |
| NP-4 | Topic transition mid-session: frontend → auth domain with `topology` fact (`credential_boundary: mTLS`) | Topic/file-touch prefetch surfaces mTLS fact; agent does not bypass server channel. |

Additional controls: zero-access-fact recall (no tool name in prompt), no-edge negative control (no spurious injection), dedup/budget, recall-outage fail-soft, no durable write for ephemeral discussion.

### 8.3 Metrics & thresholds

Extend `src/recall_metrics.rs` with:
- `FILE_PREFETCH_CALLS`, `FILE_PREFETCH_HITS` (≥1 active governing row)
- `GRAPH_TRAVERSAL_CALLS`
- `DRIFTED_MEMORIES_SURFACED`, `DRIFTED_MEMORIES_REPAIRED`

Thresholds:

| Metric | Baseline | Target |
|---|---|---|
| Unprompted governing-rule recall (NP-1/2/4, no "check NeuroStrata" in prompt) | ~0% | ≥95% |
| Graph read share (`(FILE_PREFETCH + GRAPH_TRAVERSAL) / total_memory_reads`) | 0% | ≥50% |
| Primary search SNR (top-5 hits that are `rule` or structured `fact` vs `code_ast`) | polluted | 100% non-AST by default |
| Drifted memory repair rate (NP-3) | 0% | ≥80% |
| File-touch prefetch p95 latency | N/A | ≤15ms |
| Prefetch context budget | N/A | ≤512 tokens, deduped per session |

### 8.4 Promotion policy

- Re-verify patch provenance before promotion.
- Compare task success, wrongful injections, pointer→read promotion, tokens, latency.
- Never auto-promote optimizer prose into durable rules; human review required.
- Regression guard: `block` on any critical evaluator failure.

---

## 9. Risks & mitigations

1. **Context bloat from noisy prefetch.** Mitigation: ≤512 tokens, limit ≤5, session dedup, exclude structural rows, only active `rule`/`fact`/`procedure`.
2. **False-positive `[DRIFTED]` on large files.** Mitigation: hash both `file_sha256` and `span_sha256` (symbol body); if file changed but symbol hash matches, report `[FRESH: symbol hash match]`.
3. **`lbug 0.20.4` write contention from incremental re-ingest.** Mitigation: debounce, read-time freshness via `std::fs::read` + `Sha256` without DB writes during `format_memory`, hold write lock inside `spawn_blocking`.
4. **Multi-worktree false missing-anchor.** Mitigation: reuse `src/source_context.rs`; label `[BRANCH_ANCHOR]` instead of `[MISSING_ANCHOR]` when `source_context` names another branch/worktree.
5. **Self-reinforcement of injected memories.** Mitigation: never increment `access_count` on injection.

## 10. Non-goals

1. No background LLM reader/writer agents.
2. No arbitrary Cypher tool over MCP.
3. No replacing `git diff`/`git blame` or code reading — store *why* and *what failed*, not line-by-line summaries.
4. No age-based forgetting or automatic knowledge rewriting.

## 11. Phased execution plan

| Phase | Deliverable | Anchors | Effort |
|---|---|---|---|
| 1 | Signal & freshness: exclude `code_ast` default; add `memory_types` filter; stamp `verified_at`+hashes on write; render Freshness/Source/Enforcement in `format_memory`; reserve recent-rule slot in snapshot. | `src/store/ladybug.rs:89-169`, `src/server.rs:1446,1868,2609,2927` | 2–3d |
| 2 | Graph read tools & stub edges: auto-create stub file nodes on `upsert`; implement `memories_for_file`+`related_memories`; expose both tools. | `src/traits.rs:219`, `src/store/ladybug.rs:1639`, `src/server.rs:106`, `src/handlers/memory.rs` | 3–4d |
| 3 | Automatic client shim: fix MCP contract; session-start snapshot; file-touch prefetch with dedup/512 cap; piggyback on task_claim; topic transition. | `plugins/opencode/index.ts:192-276`, `plugins/openclaw/index.ts`, `src/server.rs` (task_claim) | 2d |
| 4 | Vocabulary v8 (`decision`) + NeuroPlasticity loop: add `metadata.decision`; 600-char conciseness check; counters in `recall_metrics.rs`; NP-1..NP-4 evaluators; infra gate. | `src/schemas/memory-vocabulary.v3.json:66`, `src/recall_metrics.rs`, `plasticity.json` | 2d |

**Total:** 9–11 engineering days, shippable incrementally after Phase 1+2 (~1 week).

## 12. Disagreements resolved

- **Tool naming:** Argon proposed `memories_for_file` + `related`; K3 proposed single `graph_context` with selector. **Resolution:** keep two tools (agent-report names) but implement a shared graph-read service; `related` accepts `id`/`path`/`symbol` selectors and returns structured edges/evidence/truncation/cursor as K3 specified.
- **Freshness as scalar vs. fields:** Argon proposed a single status string; K3 required four independent fields. **Resolution:** four fields (`lifecycle`, `provenance`, `verification`, `anchor`), with Argon's hash-based VERIFIED/DRIFTED/MISSING/UNANCHORED mapped into `verification` + `anchor`.
- **Decision type:** both agreed on `metadata.decision` on `fact`; no new type.
- **Age-based forgetting:** both rejected.

## 13. Acceptance criteria

A fresh agent receives relevant context without being told a NeuroStrata tool name. Touching a file surfaces its governing rule and directed evidence within budget. Graph results exclude retired rows by default and report truncation honestly. Automatic injection causes no access-count self-reinforcement and no autonomous durable writes. Unknown freshness is explicit; code changes do not mark decisions stale. Provider failure produces no behavioral mutation. Evaluation proves sequences from MCP traces rather than files that daemon startup could create.
