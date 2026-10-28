# Incremental Index — Add/Remove/Update Without Full Rebuild Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-iwrn` (Incremental index: add/remove/update without full rebuild, P3).

## 1. Problem

Current index rebuilds on every change. Need incremental updates (add/remove/update single memories) without full rebuild; uses `emrz` file-level hash reuse.

## 2. Goal

Incremental index updates for LadybugDB; single-node add/remove/update in O(log N); triggers on `staged_publication` commit.

## 3. Constraints

- Deterministic: same mutations → same index state.
- No LLM.
- Library hygiene: `oxrdf` for RDF projection; `sha2` for hashing; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: full rebuild still works.
- Warn-first: index inconsistency warnings, not fatal.

## 4. Scope

- Add: insert single memory + edges.
- Remove: mark memory + edges deleted; cleanup on next full rebuild.
- Update: remove old + add new atomically.

## 5. Approach

- Use `emrz` hash reuse: unchanged files skip re-index.
- Per-memory delta: `upsert`/`delete` in `LadybugStore`.
- RDF projection delta: update `oxrdf` dataset incrementally.
- Trigger: `staged_publication` commit (existing hook).

## 6. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: incremental upsert/delete.
- `src/rdf/projection.rs`: delta updates.
- `src/ingest/staged_publication.rs`: commit-time index update.
- `Cargo.toml`: no new deps (`oxrdf`, `sha2` present).