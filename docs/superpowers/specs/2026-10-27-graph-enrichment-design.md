# Graph Enrichment — Attribute Propagation from Seed Nodes Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-grv3` (Graph enrichment: attribute propagation from seed nodes, P3).

## 1. Problem

Memory graph has sparse attributes on seed nodes; need bounded propagation (e.g., domain labels, source tags) to neighbors via GOVERNS/CONTAINS edges. Currently no attribute propagation layer.

## 2. Goal

Bounded attribute propagation from seed nodes to N hops via GOVERNS/CONTAINS; transient, query-time, no materialization.

## 3. Constraints

- Deterministic: fixed propagation rules, fixed hop limit (default 3, range 1..=5).
- No LLM.
- Library hygiene: `oxrdf` for graph projection; `yjah` QueryBudget for bounds; no `oxirs-core`/`scirs2`/`horned-owl`.
- Warn-first: propagation failures (cycle, budget exceeded) → partial + warning.
- Per-namespace isolation.

## 4. Scope

- Propagation rules: domain labels, source tags, extractor IDs.
- Bounded by `yjah` QueryBudget (max_hops, max_nodes, deadline).
- Transient: computed at query time, not stored.

## 5. Library evaluation

- In-tree BFS/DFS (primary, per `yjah` spec) with `QueryBudget`.
- `oxrdf` for graph projection (shared with `9fnf`/`2hxl`).
- `petgraph = "0.8.3"` backup for complex DAGs.
- No `oxirs-core`/`scirs2`/`horned-owl`.

## 6. Approach

- At `search_with_domain` / `related`, run bounded BFS from seeds.
- Propagate attributes per edge type (GOVERNS, CONTAINS).
- Return enriched results with `propagation_path` evidence.

## 7. Interfaces / Files Touched (expected)

- `src/graph_read/propagate.rs` (new): bounded attribute propagation.
- `src/store/ladybug.rs`: propagation hooks.
- `Cargo.toml`: `oxrdf`, `petgraph` (already present); no new deps.