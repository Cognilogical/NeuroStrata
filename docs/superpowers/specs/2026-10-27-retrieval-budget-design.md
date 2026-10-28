# Retrieval Budget — Per-Query Cap and Timeout Across Hybrid Retrieval Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-jwoa` (Retrieval budget: per-query cap and timeout across hybrid retrieval, P3).

## 1. Problem

Hybrid retrieval (`52dq`) has no query-level budget. Unbounded graph traversal + vector search can exceed deadlines; needs per-query cap and timeout.

## 2. Goal

Per-query retrieval budget (max_nodes/max_edges/max_hops/deadline) applied across graph + vector + keyword sub-pieces; deterministic timeout.

## 3. Constraints

- Deterministic: same query + same budget → same result set.
- No LLM.
- Library hygiene: reuse `yjah` `QueryBudget` type; `oxrdf` for graph; `fastembed` for vector; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: default budget aligns with `yjah` (max_hops 10 / max_nodes 5000 / max_edges 50000 / deadline 250ms).
- Warn-first: budget exceeded → partial results + `truncated: true`.

## 4. Scope

- Per-query `QueryBudget` applied to graph sub-piece.
- Vector sub-piece: cap at fixed max results (e.g., 200).
- Keyword sub-piece: cap at fixed max results (e.g., 200).
- Hybrid: combined score + budget enforcement.

## 5. Library evaluation

- `yjah` `QueryBudget` type (already in codebase): reuse.
- `oxrdf`: graph traversal with budget (matches `2hxl`, `9fnf`).
- `fastembed`: capped results.
- No new core libraries.

## 6. Approach

- At `search_with_domain` / `related`: construct `QueryBudget` from user/config.
- Graph sub-piece: BFS with budget (per `2hxl`).
- Vector sub-piece: `fastembed` top-K capped.
- Keyword sub-piece: deterministic symbol match capped.
- Hybrid: score and cap; if budget exceeded, truncate and set `truncated: true`.

## 7. Determinism guarantee (K3 review)

- **Budget truncation is a function of hard caps only.** `max_hops` / `max_nodes` / `max_edges` are deterministic; `deadline` is advisory and **never changes the result set** across identical inputs. If a deadline is hit, the traversal returns the same partial set it would have returned at the equivalent hard-cap hit.
- **Neighbor ordering is deterministic before budget cuts.** BFS/DFS expands neighbors in stable, sorted order (sorted by canonical IRI / MemoryId) before any budget-based truncation.
- **Vector sub-piece**: `fastembed` top-K capped at fixed K; deterministic ranking by cosine similarity; ties broken by MemoryId.
- **Keyword sub-piece**: deterministic symbol match, sorted by exactness rank then MemoryId.
- **Hybrid scoring**: fixed weights (`w_graph`, `w_vec`, `w_sym`) applied deterministically; no wall-clock dependence.
- **`governs_max_hops` default = 2** (aligns with `yjah` spec `related.rs` default; range `1..=5`).

## 7. Per-namespace isolation
- Retrieval budget applied per namespace (`ArcSwap` per namespace); cross-namespace only via `mhsp` inter-namespace traversal rules.
- References `neurostrata-9fnf` (RDF canonical direction), `yjah` (QueryBudget), `2hxl` (in-tree BFS), `52dq` (GraphRAG hybrid), `wtnn` (similarity), `mhsp` (inter-namespace traversal).

## 8. Interfaces / Files Touched (expected)

- `src/search/query_budget.rs` (new): per-query budget enforcement.
- `src/graph_read/hybrid.rs`: budget integration.
- `Cargo.toml`: no new deps.