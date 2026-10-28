# Memory Version Tracking — Per-Memory Change Log Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-ixi3` (Memory version tracking: per-memory change log, P3).

## 1. Problem

No per-memory version/change tracking; `staged_publication` tracks file-level; need memory-level audit trail.

## 2. Goal

Per-memory change log (create/update/delete events) with deterministic timestamps; queryable via `doctor`.

## 3. Constraints

- Deterministic: events ordered by monotonic sequence, not wall clock.
- No LLM.
- Library hygiene: `oxrdf` for RDF event representation; `sha2` for event hash; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: existing memories get empty log.
- Warn-first: log write failures reported.

## 4. Scope

- Create event: initial upsert.
- Update event: mutation of fields.
- Delete event: removal (soft delete for audit).
- Log query: `doctor` surface.

## 5. Approach

- Event record: `{memory_id, event_type, hash, sequence, timestamp}`.
- Sequence: monotonic per namespace.
- Hash: `sha2(memory_content + sequence)`.
- Log persisted in `LadybugStore` or separate table.

## 6. Library evaluation

- `oxrdf` for RDF event representation (optional, for `z9e7` migration).
- `sha2` for event hashing (already present).
- No `oxirs-core`/`scirs2`.

## 7. Per-namespace isolation
- Event sequence per namespace; events keyed by namespace + memory_id.
- References `neurostrata-9fnf` (RDF canonical direction), `yjah` (QueryBudget / bounded traversal), `2hxl` (OWL reasoning), `pjoa` (concept IRI), `4gb2` (provenance), `z9e7` (RDF canonical migration).

## 8. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: event table, log query.
- `src/doctor.rs`: change log surface.
- `src/telemetry/enrichment.rs`: event recording.
- `Cargo.toml`: `oxrdf`, `sha2` (already present).