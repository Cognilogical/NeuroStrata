# Design: `neurostrata_locate` — Single-Call Code Finder, Blast-Radius & Governance Locator

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-mtqx`, Feature Task `neurostrata-r3as` (Spec Task `neurostrata-sttv`).
**Approach:** Compound In-Daemon Graph + Hybrid RRF Locator inspired by Sourcegraph's `Code Finder` MCP subagent, returning ranked `file:line_range` spans hydrated with AST call-graph summary, active `GOVERNS` rules, and negative `attempt` hazards in a single MCP round-trip.

## 1. Problem

Sourcegraph's 2026 MCP benchmarks showed that a dedicated single-call locator (`code_finder`) is **$2.19\times$ faster** and uses a fraction of the host context window compared to an agent manually chaining `search` $\rightarrow$ `read_file` $\rightarrow$ `find_references`.

In NeuroStrata today, an agent obeying the Graph-First Lookup Reflex (`AGENTS.md` §3d) must execute 3–6 sequential MCP tool calls to orient on a task:
1. `neurostrata_search_memory({ query, include_code_ast: true })` to find candidate rules and AST symbols.
2. `neurostrata_memories_for_file({ path })` on each candidate file to fetch ancestor `GOVERNS` rules and `ast_summary`.
3. `neurostrata_related({ id })` to follow 1–2 hop `DEPENDS_ON` (`calls`/`imports`) or `EXTRACTED_FROM` edges.
4. `neurostrata_search_memory({ query, facets: ["failed-attempt"] })` to check for negative exemplars (`metadata.attempt.outcome == "failed"`).

Each round-trip adds LLM inference latency and dumps verbose intermediate payloads into the parent agent's context window.

## 2. Goals

1. **One-Shot Orientation (`neurostrata_locate`):** Provide a single deterministic, zero-LLM-in-the-loop MCP tool (`neurostrata_locate`), daemon endpoint (`POST /locate`), and CLI command (`neurostrata-mcp locate`) that takes a natural-language or symbol query and returns a compact, token-budgeted localization bundle.
2. **Compound Pipeline in `LadybugStore`:**
   - **Step 1 (Hybrid RRF Seed Retrieval):** Run RRF hybrid search (Task `neurostrata-5xjo`) across `symbol`, `code_ast`, `file`, `doc_section`, `rule`, `fact`, and `procedure` rows.
   - **Step 2 (AST Call/Import Neighborhood Expansion):** Expand top symbol/file seeds 1–2 hops across `DEPENDS_ON` (`calls`, `imports`, `implements`) to compute caller/callee counts, blast radius, and co-occurring structural targets.
   - **Step 3 (Governance & Negative-Attempt Hydration):** For every top-ranked file/symbol location, deterministically gather:
     - Active `GOVERNS` memories on the symbol, exact file, and ancestor directories (`memories_for_file` logic in `src/graph_read/`).
     - Active failed-attempt memories (`metadata.attempt.outcome == "failed"`) linked to those files/symbols or matching the query.
   - **Step 4 (One-Line Rationale Synthesis & Budgeted Packing):** Synthesize a deterministic 1-line `why` explanation per location (e.g., `"Exact symbol match 'check_caller_path' + 4 callers in src/daemon.rs; governed by rule neurostrata-p1ex"`) and pack within `max_bytes` (default `6144` bytes / ~1500 tokens).
3. **Automatic Graph-First Ledger Accounting:** A single `neurostrata_locate` call increments `task.graph_queries_count` and appends all inspected paths to `.NeuroStrata/session_queries.json`, satisfying the `pre-commit` Graph-First verification predicate for all returned files at once.

## 3. Non-Goals

- Invoking an external LLM inside the daemon to summarize files (must remain 100% local, deterministic, and sub-50ms).
- Returning full file contents (returns exact `path`, `lines`, `symbol`, `signature_preview`, `why`, and governing rule summaries so the host agent can `read` only the exact line range needed).

## 4. Architecture

### 4.1 Request & Response Contract (`src/graph_read/locate.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateRequest {
    pub namespace: String,
    pub query: String,
    pub path_prefix: Option<String>,
    pub symbols_only: Option<bool>,
    pub max_locations: Option<usize>, // default 8, max 20
    pub max_bytes: Option<usize>,     // default 6144 (~1500 tokens)
    pub project_root: Option<String>,
    pub principal: Option<String>,    // default "admin" on local surface
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocatedSpan {
    pub path: String,
    pub lines: Option<String>,        // e.g. "881-961"
    pub symbol: Option<String>,       // e.g. "ingest_directory"
    pub symbol_kind: Option<String>,  // e.g. "functions"
    pub scip_symbol: Option<String>,  // populated when Tier 2 SCIP is ingested
    pub signature: Option<String>,    // 1-line signature preview
    pub score: f32,                   // combined RRF + graph centrality score
    pub why: String,                  // deterministic 1-line explanation
    pub callers_count: usize,
    pub callees_count: usize,
    pub blast_radius_count: usize,
    pub governing_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateRuleSummary {
    pub id: String,
    pub memory_type: String,          // "rule" | "fact" | "procedure"
    pub summary: String,              // bounded to 280 chars
    pub governs_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateFailedAttempt {
    pub id: String,
    pub approach: String,
    pub failure_mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateResponse {
    pub namespace: String,
    pub query: String,
    pub locations: Vec<LocatedSpan>,
    pub governing_rules: Vec<LocateRuleSummary>,
    pub failed_attempts: Vec<LocateFailedAttempt>,
    pub conceptual_bridges: Vec<String>,
    pub truncated_by_budget: bool,
}
```

### 4.2 Ranking & Coalescing Algorithm

1. **Candidate Collection:**
   - Retrieve top `4 * max_locations` hits via Hybrid RRF (`search_inner` with `SearchMode::Hybrid`, `include_code_ast: true`, `include_tasks: false`).
   - Partition hits into:
     - **Structural Spans** (`symbol`, `code_ast`, `file`, `doc_section`).
     - **Normative Anchors** (`rule`, `fact`, `procedure`) that carry `location` or `metadata.governs` targets.
2. **Anchor Projection:**
   - Normative hits that govern a specific file/symbol project a relevance boost onto that target file/symbol span (`+0.25 * rrf_gain(rule)`), ensuring files governed by query-relevant rules surface alongside direct AST matches.
3. **Span Deduplication & Line-Range Coalescing:**
   - Group candidate spans by `(path, symbol)`. If both a `file` node and a specific `symbol` node inside that file match, prefer the specific `symbol` span (`lines: "start-end"`).
4. **AST & Governance Hydration:**
   - Batch-execute the `memories_for_file` + 1-hop `DEPENDS_ON` lookup across the top `max_locations` distinct file paths in a single `with_conn` closure (avoiding $N$ separate lock acquisitions).
   - Extract any memory with `metadata.attempt.outcome == "failed"` into `failed_attempts`.
5. **Deterministic `why` Builder:**
   - Format `why` from the active signals:
     - Match source: `["exact symbol", "BM25 lexical", "semantic vector", "1-hop caller of <X>", "governed by matched rule <id>"]`.
     - Topology summary: `"callers=N, callees=M, blast_radius=B, rules=R"`.

### 4.3 Graph-First Ledger Integration (`src/server.rs` & Harness Plugins)

- In `src/server.rs`, invoke `crate::task::record_graph_query(&store, &namespace, "neurostrata_locate", Some(path), project_root)` (`src/server.rs:2033`) for each returned `locations[].path` (incrementing `task.graph_queries_count` and recording all returned paths into `.NeuroStrata/session_queries.json`), and add `"neurostrata_locate"` to the graph-query tool list in `plugins/core/index.ts`.
- **Library Reuse & Formal Alignment (Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`):** `neurostrata_locate` reuses `TantivyLexicalIndex` (`tantivy`, MIT) and the formally verified `HybridSlots` (`allocate_hybrid_slots` in `src/formal_kernel.rs` / `proofs/NeuroStrata/HybridSlots.lean`) candidate ranking pipeline from `neurostrata-5xjo` rather than duplicating tokenization or ranking logic.

## 5. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| $N+1$ database queries when hydrating `max_locations` files | Execute all ancestor `GOVERNS` and 1-hop `DEPENDS_ON` lookups inside a single batched `with_conn` Cypher pass keyed by the candidate `path` set. |
| Empty AST graph on un-ingested repos | If no structural rows exist in the namespace, `neurostrata_locate` still returns locations derived from normative memories' `locations[]` / `metadata.governs` anchors and sets an advisory note `"ast_ingested": false`. |
| Context blowup on heavily-governed core files (e.g., `src/store/ladybug.rs`) | Deduplicate `governing_rules` into a top-level lookup table (`LocateResponse.governing_rules`) referenced by ID in `LocatedSpan.governing_rule_ids`, and enforce `max_bytes`. |

## 6. Checkable Acceptance Criteria

1. Calling `neurostrata_locate({ namespace: "NeuroStrata", query: "check_caller_path" })` returns `src/daemon.rs` with the exact line range of `check_caller_path`, its caller/callee counts, and the governing rule `neurostrata-p1ex` in a single tool call.
2. When a file has an associated failed-attempt memory (`metadata.attempt.outcome == "failed"`), `LocateResponse.failed_attempts` includes that hazard automatically.
3. Calling `neurostrata_locate` increments `graph_queries_count` on the active `in_progress` task and writes every returned `locations[].path` into `.NeuroStrata/session_queries.json`.
4. Total serialized response size never exceeds `max_bytes` when `max_bytes >= 1024`, setting `truncated_by_budget: true` when trimmed.

## 7. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-r3as` to produce the step-by-step TDD implementation plan.
