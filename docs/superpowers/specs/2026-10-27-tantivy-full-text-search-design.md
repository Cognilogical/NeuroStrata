# Tantivy Full-Text Search Design

> Status: revised per Argon review (approve with minor fixes applied) — pending user review.
> Task: `neurostrata-n20x` (Tantivy full-text search, OxiRS §1.9).
> Companion: `docs/design-oxirs-concepts.md` §1.9, `docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md` (F1 exact names).

## 1. Problem

NeuroStrata's retrieval is embedding + bounded graph expansion. An exact-name pre-pass exists (`src/store/ladybug.rs` exact-match block, `derivation: ["exact-name"]`) but it is equality-only: it misses partial/typo/phrase queries and provides no ranked lexical score. The docs claim lexical recall is "NOT IMPLEMENTED" (stale; the pre-pass ships). Tantivy would upgrade the pre-pass to ranked lexical recall (multi-field, fuzzy, phrase) without changing the primary store.

## 2. Goal

Add an **optional, feature-gated Tantivy index** over memory content + symbol names + location, blended into retrieval deterministically (single ranking rule, no RRF), or offered as a separate `lexical` search mode. LadybugDB remains the sole source of truth; the Tantivy index is a derived, rebuildable artifact.

## 3. Constraints (non-negotiable)

- LadybugDB is the sole store. Tantivy is a derived index, never a source of truth; it must be rebuildable from LadybugDB at any time (`neurostrata_rebuild_lexical_index`).
- No LLM anywhere in this path.
- Deterministic ranking: a single, documented scoring rule. No fusion of multiple rankers (no RRF). Blending with vector score uses a documented linear combination with a fixed weight, or the lexical track is offered as a separate mode.
- Feature-gated: `--features tantivy` (default off) so the default build has no Tantivy dependency and no index files.
- Warn-first on index corruption: a corrupt index is rebuilt, never fatal.
- Score polarity: `SearchResult.score` is a **distance** (lower is better; exact matches `0.0`, graph expansions `1.0`, sorted `ASC` at `src/store/ladybug.rs:741`). Lexical BM25 (higher-is-better) MUST be converted to a distance before combination. All blending uses distances, sorted `ASC`.

## 4. Scope

In scope:
- Tantivy schema: fields `id` (stored, string), `namespace` (stored, string, indexed), `content` (text), `symbol` (text), `location` (text), `memory_type` (string, indexed), `valid_from` (i64, indexed), `valid_to` (i64, indexed).
- Index writer behind a mutex; single writer per process.
- Index lifecycle: create on first use; rebuild from LadybugDB on demand or when `index_meta` version mismatches.
- Search integration: a `lexical` mode in `neurostrata_search_memory` (new `mode` enum value) returning results with `derivation: ["tantivy-lexical"]`; and an optional blend into the default mode behind a config flag.
- A benchmark comparing the existing exact pre-pass vs Tantivy on the repo's own query set before enabling blend by default (gate: Tantivy must beat exact pre-pass on recall@10 by ≥5% on the benchmark to be enabled in default mode; otherwise lexical stays opt-in).
- Index stored under `<db_path_parent>/lexical/` (machine-wide, rebuildable, gitignored).

Out of scope:
- Replacing the embedding/vector track.
- Cross-namespace federated lexical search (separate spec).
- Real-time incremental sync to other instances (mesh stays git-batch).

## 5. Approach

### 5.1 Feature gate

Cargo feature `tantivy` adds dependency `tantivy = "0.22"` (or latest compatible). Without the feature:
- `mode` parameter is still accepted in the `neurostrata_search_memory` schema (for forward compatibility).
- Passing `mode: "lexical"` returns a deterministic error string: `"ERROR [LEXICAL]: Tantivy lexical search is not enabled in this build (--features tantivy)."`.
- `neurostrata_rebuild_lexical_index` is registered as a stub returning the same disabled message.
- Index code compiles to no-ops.

### 5.2 Index writer

`LexicalIndex` struct wrapping `tantivy::Index`, `IndexWriter`, and `IndexReader`. Writer is `Mutex<IndexWriter>`; reader is `Arc<IndexReader>` rebuilt (reload) after commits. Commits happen on:

Multi-process access: Tantivy's `IndexWriter` acquires an exclusive OS-level lock file (`<index_dir>/.tantivy-writer.lock`). Only the daemon process opens `IndexWriter`. CLI/stdio/MCP-proxy processes open a read-only `IndexReader` only; if they need to trigger a rebuild, they route through daemon HTTP (`POST /lexical/rebuild`). If a non-daemon process attempts to open `IndexWriter` and hits lock contention, it falls back to read-only reader mode and logs a warning (no crash).
- upsert of a memory (add/replace document by id),
- supersede (valid_to set),
- archive,
- relocate (move),
- delete,
- explicit `neurostrata_rebuild_lexical_index`.

Commit is batched with a 200ms debounce to avoid per-write fsync storms; `rebuild` forces immediate commit. Because LadybugDB writes are serialized by `write_lock` and tests/agents require read-after-write, `upsert`/`delete`/`relocate` flush synchronously (commit immediately) when called; the 200ms debounce applies only to bulk rebuild/indexer background tasks. `search` additionally calls `reader.reload()` if a dirty flag is set, ensuring no stale reads.

Mutation hooks in `LadybugStore` (behind `#[cfg(feature = "tantivy")]`): `upsert` inspects the incoming `payload` (checking `STRUCTURAL_MEMORY_TYPES`, `is_registry_row`, `is_archived`, `is_mesh_conflicting`, `is_retired_at`, `is_retired_at(now)`) to decide whether to upsert or delete the document in Tantivy; `delete`, `relocate` (move id across namespaces), `clear_ingested`, `handle_supersede_memory` (via `upsert` with `valid_to` set), and `archive_memory` (via `upsert` with `archived: true`) all route through the same upsert/delete decision. No separate `supersede`/`archive` methods exist in `LadybugStore` (both call `upsert` with updated metadata).

### 5.3 Indexing fields & tokenization

For each memory:
- Excluded entirely: structural types (`STRUCTURAL_MEMORY_TYPES`), registry rows (`is_registry_row`), mesh-conflicting rows (`is_mesh_conflicting`), deleted rows.
- Indexed with filtering at query time: archived rows (`is_archived`) and task rows (`memory_type == "task"`) ARE indexed (with `memory_type` and `valid_to`/`valid_from` fields) so `include_archived`/`include_tasks` can be honored; they are filtered at query time based on those flags.
- Superseded rows (`valid_to > 0`) are KEPT with updated `valid_to` so historical `as_of` queries resolve.
- `id` = composite key `"{namespace}::{memory_id}"` (namespace + id) to prevent cross-namespace collisions and to scope delete/replace by `(namespace, id)`. `namespace` is also a stored/indexed field for filtering.
- `namespace` = namespace
- `content` = content (Tantivy default tokenizer; acceptable for prose)
- `symbol` = `metadata.symbol` if present else empty (code-aware tokenizer: split on `/`, `.`, `:`, `_`, camelCase boundaries, lowercase)
- `location` = location (same code-aware tokenizer)
- `memory_type`, `valid_from`, `valid_to` as above.

Code-aware tokenization is required because the default tokenizer splits on `_` and strips punctuation, degrading symbol/path queries.

### 5.4 Search

`mode = "lexical"`:
- Skips `emb.embed(&query)` entirely (zero-embedding, deterministic).
- Runs a Tantivy multi-field query over `content:symbol:location` with boosts content=1.0, symbol=2.0, location=1.5 (code-aware tokenization applied to query terms for symbol/location fields).
- Does NOT seed Step 2 graph expansion: lexical primaries are returned directly (no 1-hop/2-hop expansion) to keep the mode pure and predictable. (Default mode continues to expand.)
- Applies LadybugDB read-time filters (`is_retired_at`, domain slugs, `as_of`) by over-fetching `top_k * FETCH_MULTIPLIER` from Tantivy and verifying each candidate against LadybugDB; dropped candidates are replaced from the next Tantivy page.
- Domain filter fallback mirrors `search_inner`: if the domain filter matches 0 rows among candidates, fall back to unfiltered results and report `domain filter: skipped — no row matched the domain; the results are unfiltered (lexical)` in the mode header.
- Maps each surviving hit's `id` back to a `SearchResult` via LadybugDB lookup (payload/evidence come from the store, not the index).
- `score` = `1.0 - (bm25 / max_bm25_in_result_set)` (clamped to [0,1]); if result set empty, score 0.0 (distance convention: lower better). `derivation: ["tantivy-lexical"]`. Lexical distance is raw normalized BM25 (no `capped_gain`/usage/provenance/feedback boost).
- Sort `ASC` by score.
- Blend (if enabled): `vector_distance` in `0.7 * vector_distance + 0.3 * lexical_distance` uses the boosted distance (with `capped_gain`); `lexical_distance` uses raw normalized BM25 distance. Vector-only candidates substitute worst-case lexical distance 1.0; lexical-only substitute worst-case vector distance 1.0.

Default mode blend (only if benchmark gate passes and config `lexical_blend = true`): run lexical query in parallel with vector search; merge candidate sets; final distance = `0.7 * vector_distance + 0.3 * lexical_distance` for ids present in both, else `0.7 * vector_distance + 0.3` (vector-only, substituting worst-case lexical distance 1.0) or `0.7 + 0.3 * lexical_distance` (lexical-only, substituting worst-case vector distance 1.0). Documented single rule. Off by default until benchmark evidence.

### 5.5 Rebuild

`neurostrata_rebuild_lexical_index(namespace?)`: drops index, re-scans LadybugDB for all non-structural, non-archived, non-deleted, non-conflicting memories in scope (superseded rows with `valid_to > 0` included with their `valid_to`), re-indexes. Idempotent. Reports counts.

### 5.6 Staleness

Index stores `index_version` (crate version + schema version). On open, if mismatch, auto-rebuild in background and serve from LadybugDB exact pre-pass meanwhile (degraded but correct).

### 5.7 Evidence rendering

`render_evidence_suffix` (`src/server.rs`) currently skips `EvidenceKind::DirectMatch`. Update it to render a non-empty `derivation` tag on `DirectMatch` rows with non-empty `derivation`: emit `\nMatch [{derivation joined by ", "}]` (e.g. `\nMatch [tantivy-lexical]` or `\nMatch [exact-name]`). This makes `derivation: ["tantivy-lexical"]` visible in MCP output and also fixes `exact-name` visibility. Exact string format: `\nMatch [tag1, tag2]` with tags in insertion order.

## 6. Interfaces

- Cargo feature `tantivy`.
- New module `src/lexical.rs` (or `src/search/lexical.rs`) behind `#[cfg(feature = "tantivy")]`.
- `neurostrata_search_memory` gains `mode: "lexical"` and `as_of: Option<i64>` (enum extended; non-tantivy builds reject `mode="lexical"` with message). `as_of` is threaded through `VectorStore::search_with_domain` and `search_inner`.
- New tool `neurostrata_rebuild_lexical_index` (namespace optional).
- Config: `lexical_blend: bool` (default false).

## 7. Edge cases

- Feature disabled: no index files, no tool, clear message.
- Corrupt index: rebuild, log warning, serve exact pre-pass meanwhile.
- Empty namespace: empty index, no error.
- Concurrent writers: mutex serializes; readers see last committed snapshot.
- Missing index on first search: build lazily from LadybugDB.
- `as_of` in the past: over-fetch and filter by LadybugDB.

## 8. Testing

- Unit: index round-trips a memory; search returns it; delete removes it.
- Integration: `mode=lexical` returns symbol-mentioning memory; derivation cites `tantivy-lexical` in MCP output.
- Benchmark: harness in `tests/bench_lexical.rs` comparing exact pre-pass vs Tantivy on a fixed query set; gate documented.
- Feature-off build: `cargo build` (no features) succeeds and `mode=lexical` rejected.

## 9. Acceptance

With `--features tantivy`, `neurostrata_search_memory(mode="lexical")` returns ranked lexical results with `tantivy-lexical` derivation visible in output; index is rebuildable; default build (no feature) is unaffected. Blend enabled only after benchmark gate documented in a later task.

## 10. Files touched (expected)

- `Cargo.toml`: feature `tantivy`, optional dep.
- New: `src/lexical.rs`.
- `src/server.rs`: mode enum, tool registration, rebuild handler, `render_evidence_suffix` fix.
- `src/store/ladybug.rs`: upsert/supersede/archive/relocate/delete hooks to update index (behind cfg).
- `.gitignore`: `<db_path_parent>/lexical/` (machine-wide; not tracked anyway).