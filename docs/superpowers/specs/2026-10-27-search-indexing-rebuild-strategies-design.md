# Search Index — Rebuild Strategies and Incremental Updates Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-umk8` (Search indexing rebuild strategies and incremental updates, P3).

## 1. Problem

Full index rebuild on every search. Unchanged files cause pointless re-index (contradicts `emrz` file-level hash reuse); need index rebuild strategies (incremental / partial / full) + incremental update path.

## 2. Goal

Search index supports incremental updates (per-memory upsert/delete) with hash-gated skip; partial index rebuild for schema migrations; config-driven strategy selection.

## 3. Constraints

- Deterministic: same mutations → same index state.
- No LLM.
- Library hygiene: `oxrdf` projection; `sha2` for file hash; `fastembed` embeddings; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: `emrz` file-level reuse also gated to new code.
- Warn-first: index inconsistency, hash mismatch → warnings.

## 4. Scope

- Incremental upsert: add/update single memory in index.
- Incremental delete: mark deleted; clean-up sweep.
- Partial rebuild: changed-memory-only re-index (all files, but only re-ingested ones re-indexed).
- Full rebuild: all files, all indices.
- Strategy config: `emit_index_strategy` enum (incremental / partial / full).

## 5. Approach

- Incremental: at `staged_publication` commit, only changed memories update index.
- Hash gate: unchanged file → skip index update (shared with `emrz` spec).
- Strategy selection from config; default incremental.

## 6. Library evaluation

- `oxrdf`: RDF projection (`9fnf`, `2hxl`).
- `sha2`: file content hash (already present, shared with `emrz`).
- `fastembed`: vector embedding, already present.
- No new core libraries.

## 7. Per-namespace isolation
- Index rebuild strategy (`incremental`/`partial`/`full`) configured per namespace; no cross-namespace without `mhsp` rules.
- References `neurostrata-9fnf` (canonical IRI / RDF projection), `2hxl` (OWL reasoning), `yjah` (QueryBudget), `emrz` (hash reuse), `wtnn` (embeddings), `jwoa` (retrieval budget).

## 8. Interfaces / Files Touched (expected)

- `src/index/store.rs` (new): indexed data structure + incremental hooks.
- `src/store/ladybug.rs`: upsert/delete hooks feeding index.
- `src/ingest/staged_publication.rs`: commit-time index update.
- `Cargo.toml`: no new deps.