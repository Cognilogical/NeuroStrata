# Prioritized Edge Traversal — Weighted Graph Walk Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-pmwh` (Prioritized edge traversal: weighted graph walk, P3).

## 1. Problem

`related` uses uniform edge weights. Need deterministic weighted traversal (GOVERNS > CONTAINS > RELATES_TO > EXTRACTED_FROM > DEPENDS_ON) without LLM.

## 2. Goal

Deterministic weighted edge traversal; per-edge-type priority; bounded by `yjah` QueryBudget.

## 3. Constraints

- Deterministic: fixed weights per edge type; same query → same path ranking.
- No LLM.
- Library hygiene: `oxrdf` for graph projection; `yjah` QueryBudget for bounds; no `oxirs-core`/`scirs2`/`horned-owl`.
- Warn-first: budget exceeded → partial + warning.
- Per-namespace isolation.

## 4. Scope

- Edge-type weights: GOVERNS=5, CONTAINS=4, RELATES_TO=3, EXTRACTED_FROM=2, DEPENDS_ON=1.
- Priority queue BFS with weight accumulation.
- Integration with `yjah` ClosureResult.

## 5. Library evaluation

- In-tree priority queue BFS (Rust `BinaryHeap` + `yjah` QueryBudget).
- `oxrdf` for graph projection.
- `petgraph = "0.8.3"` backup for complex DAG with weights.
- No new core libraries.

## 6. Approach

- Priority queue BFS: expand highest-weight path first.
- Budget: max_hops, max_nodes, max_edges, deadline (per `yjah`).
- Return paths with `weight_sum` and `path_evidence`.

## 7. Interfaces / Files Touched (expected)

- `src/graph_read/prioritized.rs` (new): weighted BFS.
- `src/store/ladybug.rs`: edge weight config.
- `Cargo.toml`: no new deps.