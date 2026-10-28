# Dangling-Edge Report in Doctor and task_validate Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-xlya` (Dangling-edge report in doctor and task_validate, P2, ranked #13, S-M effort).

## 1. Problem

Unresolved GOVERNS / CONTAINS / EXTRACTED_FROM / RELATES_TO / DEPENDS_ON targets are silently skipped in `doctor` and `task_validate`. No dangling-edge report exists.

## 2. Goal

Report dangling edges (edges pointing to non-existent nodes) in `doctor` and `task_validate`; warn-first, not reject.

## 3. Constraints

- No LLM.
- Library hygiene: `std` + `oxrdf` for edge lookup; no `oxirs-core`.
- Warn-first: dangling edges reported; not fatal.
- Backward compatible: existing dangling edges remain; report only.

## 4. Scope

- Dangling-edge detection in `doctor`.
- Dangling-edge detection in `task_validate`.
- Integration with `relink_edges`.

## 5. Approach

- During `relink_edges`, collect all target node IDs referenced in edges.
- Compare against active node IDs; report differences.
- Report in `doctor` output and `task_validate` advisory.

## 6. Library evaluation (explicit)
- `oxrdf` for RDF projection / edge lookup; NO `oxirs-core`/`scirs2`/`horned-owl`. Manual edge validation preferred.

## 7. Per-namespace isolation / references / constraints
- Per-namespace isolation: dangling-edge report scoped per namespace (`relink_edges` per namespace).
- References `neurostrata-9fnf` (canonical IRI), `yjah` (QueryBudget / bounded closure), `2hxl` (BFS / transient inference), `4gb2` (provenance), `52dq` (GraphRAG), `mhsp` (inter-namespace), `pjoa` (concept IRI).
- Warn-first: dangling edges reported, not rejected (backward compatible).
- Deterministic: sorted dangling-edge list; stable reporting.

## 8. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: dangling edge check in `relink_edges`.
- `src/doctor.rs`: dangling report output.
- `tests/task_validate/`: dangling-edge advisory.
