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

## 7. Interfaces / Files Touched (expected)

- `src/search/query_budget.rs` (new): per-query budget enforcement.
- `src/graph_read/hybrid.rs`: budget integration.
- `Cargo.toml`: no new deps.