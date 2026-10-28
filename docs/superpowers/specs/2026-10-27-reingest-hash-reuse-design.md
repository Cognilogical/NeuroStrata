# Re-ingest Only What Changed — File-Level Content Hash Reuse Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-emrz` (Re-ingest only what changed, P2, ranked #2, M-L effort).

## 1. Problem

Ingestion re-parses every file on every run. Unchanged files waste CPU and produce duplicate embeddings. Current ingest carries forward unchanged nodes by node hash only; file-level reuse and version-gated re-parse are not implemented.

## 2. Goal

Add file-level content hash reuse: unchanged files reuse existing nodes and embeddings; re-parse only when file content, parser version, or schema version changes.

## 3. Constraints

- Deterministic: hash is content-based, not timestamp-based.
- No LLM in hash/reuse path.
- Library hygiene: use `sha2` for hashing; no `oxirs-core`.
- Backward compatible: pre-migration files without hashes are re-parsed.
- Warn-first: hash mismatch or version change reported, not fatal.

## 4. Scope

- File-level content hash computation and storage.
- Reuse of unchanged nodes and embeddings.
- Version-gated re-parse (parser/schema version).
- Integration with `staged_publication` and `relink_edges`.

## 5. Approach

- `sha2 = "0.10"` for file content hashing.
- Store hash in `src/ingest/staged_publication.rs` per file.
- On ingest, compare stored hash to current hash; if match, skip re-parse.
- Version gate: bump parser/schema version triggers full re-parse.

## 6. Library evaluation (explicit)
- `sha2` (hash reuse); NO `oxirs-core`/`scirs2`/`horned-owl`; `oxrdf` optional for RDF projection (`9fnf`/`2hxl`).

## 7. Per-namespace isolation / references / constraints
- Per-namespace isolation: hash-reuse scoped per namespace (namespace key in hash lookup).
- References `neurostrata-9fnf` (canonical direction / RDF layer), `yjah` (QueryBudget / transient inference / warn-first), `2hxl` (BFS closure), `4gb2` (provenance), `52dq` (GraphRAG), `mhsp` (inter-namespace).
- Warn-first: hash mismatch / version mismatch → warning + re-parse.
- Deterministic: content-based hash, not timestamp-based.

## 8. Interfaces / Files Touched (expected)

- `src/ingest/staged_publication.rs`: hash computation and reuse logic.
- `src/store/ladybug.rs`: hash lookup and node reuse.
- `Cargo.toml`: `sha2 = "0.10"` (already used in `2hxl` and `4gb2` specs).