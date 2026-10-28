# Inter-Namespace Edge Traversal — Jump Between Domains Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-mhsp` (Inter-namespace edge traversal: jump between domains, P3).

## 1. Problem

`related` / `search_with_domain` restricted to single namespace. No mechanism to traverse edges across namespaces / domains; recall limited within namespace.

## 2. Goal

Inter-namespace edge traversal (hop between domains via shared edges); bounded; transient, not materialized.

## 3. Constraints

- Deterministic: same traversal rules regardless of namespace.
- No LLM.
- Library hygiene: `oxrdf` for RDF projection across namespaces; `yjah` QueryBudget for bounds; no `oxirs-core`/`scirs2`/`horned-owl`.
- Warn-first: cross-namespace hop failure → stop traversal + warning.
- Per-namespace isolation: target namespace config.

## 4. Scope

- Cross-namespace GOVERNS/CONTAINS traversal.
- QueryBudget bounds enforced per traversal.
- Integration with `KnownIds` / `pjoa` concept IRIs.

## 5. Library evaluation

- `oxrdf`: per-namespace RDF dataset (matches `9fnf`, `k8tc` federation).
- `yjah` QueryBudget: bounded hops (matches `2hxl`).
- `petgraph = "0.8.3"`: backup for cross-namespace DAG.
- No new core libraries.

## 6. Approach

- `ArcSwap<BTreeMap<String, OntologyIndex>>` per namespace + structural cross-namespace index.
- On traversal: check namespace config for cross-namespace allowance.
- BFS/DFS with QueryBudget; stop at namespace boundary if disallowed.
- Report cross-namespace results with namespace labels.

## 7. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: cross-namespace edge flags.
- `src/graph_read/traversal.rs`: cross-namespace traversal.
- `Cargo.toml`: no new deps.