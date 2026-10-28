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

## 6. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: dangling edge check in `relink_edges`.
- `src/doctor.rs`: dangling report output.
- `tests/task_validate/`: dangling-edge advisory.
