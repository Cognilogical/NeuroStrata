# Entity / Mention Linking Design

> Status: revised per Argon review (approve with changes) — pending user review.
> Task: `neurostrata-q1mt` (Entity / mention linking, OxiRS §1.8).
> Companion: `docs/design-oxirs-concepts.md` §1.8, `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md` §13.

## 1. Problem

Ingested symbols exist as memory rows, but a memory whose `content` mentions a symbol creates no graph edge. Retrieval therefore cannot walk from a rule/fact to the symbols it talks about, and the symbol's neighborhood cannot surface the memories that reference it. The only edges today are declared (`governs`, `related_to`, `contained_by`, `extracted_from`, `depends_on`); prose references are invisible.

## 2. Goal

Add a **deterministic, LLM-free mention detector** that, for each memory, finds the symbols/file paths it references and creates edges from the memory to those symbols. Edges are surfaced in `SearchEvidence` so a search can explain why a symbol-linked memory was returned. No new store, no second database, no model.

## 3. Scope

In scope:
- A mention detector over `MemoryPayload.content` and `location` against the active symbol index of the namespace.
- Edge creation `memory -> symbol` using the existing `RELATES_TO` table with `extractor = "mention-detector"` and `source = memory.id`.
- Integration into the write path (upsert) and `relink_edges`, so edges stay consistent with content.
- Surfacing mention edges in `SearchEvidence` (derivation `"mention-link"`).
- A cap on mentions per memory to bound work.
- Warn-first, never reject: a failed detection logs a warning and writes the memory without edges.
- SHACL validation of the new `mentions` field.

Out of scope (named follow-ons):
- Type classification of mentions (OxiRS `entity_classifier`): no type tagging in v1; all mentions are plain links.
- Cross-namespace mention linking: only symbols in the same namespace are candidates.
- Mention detection over metadata fields other than `content`/`location`.
- Fuzzy/semantic mention matching: exact token match only.

## 4. Approach

### 4.1 Symbol index

Reuse `KnownIds` (`src/store/ladybug.rs`). Build it from the **active generation's** symbol node ids for the namespace. Candidate structural memory types: `code_ast` (ingested AST symbols), `markdown` (doc sections), `directory`, `file`. Exclude auto-ingestor bookkeeping rows. Only generation-scoped ids are candidates, so stale generations never produce mentions.

During ingest, the ingestor itself must NOT run mention detection on its own generated rows (code-to-code mentions are noise and waste cycles). Detection runs only on rows written by agents/users (memory_type not in the structural set, or agent_name != "auto-ingestor" with non-structural type).

### 4.2 Candidate strings

Ingested AST ids look like `ns::7::src/foo.rs#functions:bar@<sha256>` and markdown section ids like `ns::7::docs/x.md#heading`. `logical_path_of(id)` returns `src/foo.rs#functions:bar@<sha256>` (with the hash), not `src/foo.rs::bar`.

For each symbol id, derive the mentionable strings:
- **Full logical path** (without generation scope): `src/foo.rs#functions:bar` and `src/foo.rs` (file path portion before `#`).
- **Qualified tail**: `src/foo.rs::bar` is NOT produced (that format does not exist); instead produce `src/foo.rs` and the symbol name `bar` when unambiguous.
- **Short alias**: the final path component (`foo.rs`, `bar`) is added **only** when no two symbols in the namespace share it (unambiguous). Ambiguous short aliases are dropped.
- **Minimum length guard**: short aliases shorter than 3 characters, or matching a small stopword set (`id`, `x`, `i`, `t`, `n`, `ok`, `err`, `new`, `get`, `set`), are dropped to avoid noise.

### 4.3 Matching (token-based, not regex alternation)

Do NOT compile a giant regex alternation (pathological backtracking + boundary bugs). Instead:
1. Tokenize `content`/`location` into lexical spans. A span is a maximal run of non-whitespace characters, then split further on path/identifier separators only where needed. Concretely: split content on whitespace first; each token is matched **exactly** against a `HashMap<String, String>` mapping candidate string → symbol id. This is O(tokens) and avoids `\b` boundary bugs on paths like `src/foo.rs` appearing inside `path/to/src/foo.rs`.
2. For each token, try the full token, then progressively strip surrounding punctuation (backticks, quotes, parentheses, trailing commas/periods) and re-lookup. A token matches only if, after stripping, it equals a candidate exactly.
3. A candidate that is a short alias matches a token only if the token's **neighboring characters** (the char before/after the stripped token in the original content) are not `/`, `.`, `:`, or `_` (i.e., not part of a larger path/identifier), to prevent `path/to/foo.rs` from resolving to `foo.rs`.
4. Collect resolved symbol ids, deduplicate, drop self (memory id == symbol id), cap at 32 (truncate + warn "mention cap reached").

### 4.4 Edge creation

For each resolved symbol id:
- If `(memory, symbol)` already exists as a declared `related_to` edge, skip (declared intent wins).
- Otherwise add to `metadata.mentions: [symbol_id, ...]`.

Materialization (in `upsert` and `relink_edges`): for the memory, delete existing `RELATES_TO` rows with `extractor = 'mention-detector'` and `a.id = memory.id`, then batch-create fresh rows with `extractor = 'mention-detector'`, `source = memory.id`, `valid_from = now`, `valid_to = 0`, `generation = active_gen`. Idempotent.

### 4.5 Search evidence

Update `neighbor_out_q` / `neighbor_in_q` in `search_inner` to also return `e.extractor`. When expanding over a `RELATES_TO` edge whose `extractor == 'mention-detector'`, push `"mention-link"` into `SearchEvidence.derivation` (instead of the current empty derivation for RELATES_TO). No change to ranking beyond existing graph expansion budget.

### 4.6 Caching KnownIds on the store

`LadybugStore::upsert` currently constructs `KnownIds::new(vec![])`. Add a cached `Arc<RwLock<Option<KnownIds>>>` (or rebuild per write batch) populated from the active generation's symbol ids on first use and invalidated on ingest/relink. Detection calls `store.known_ids(namespace)` instead of a full scan per memory. If the cache is empty/unavailable, detection is skipped with a warning (never blocks the write).

## 5. Data flow

1. Write path (`upsert`): after canonicalization and facet computation, call `detect_mentions(content, location, known_ids)` → `Vec<String>` of symbol ids. Write `metadata.mentions`. Materialize mention edges (delete old mention-detector rows for this memory, create new).
2. `relink_edges`: for each memory, re-run `detect_mentions` against the **new** active symbol index (so mentions are rediscovered after re-ingest), update `metadata.mentions`, and rematerialize mention edges. This fixes the stale-generation problem: physical ids in `mentions` are regenerated from the new index; old-generation ids never linger.
3. Search: existing expansion walks RELATES_TO including mention edges; evidence derivation switched on extractor.

## 6. Interfaces

- `pub fn detect_mentions(content: &str, location: &str, index: &KnownIds) -> Vec<String>` in `src/mention.rs` (new module), returning deduplicated, capped symbol ids.
- `metadata.mentions: Vec<String>` (new optional field, validated as array of strings by SHACL).
- `relink_edges` gains a pass that materializes mention edges (idempotent).
- No new MCP tool; behavior is automatic on write and relink.

## 7. Edge cases

- Empty content/location or no symbols: returns `[]`, no edges.
- Symbol id equals the memory's own id: skip (no self-loop).
- Ambiguous short alias: dropped at index build.
- Content longer than 1 MiB: skip detection, log warning.
- Cache miss: skip detection, log warning, write memory without mentions.
- Declared `related_to` already contains the symbol: skip mention edge for that symbol.

## 8. Testing

- Unit: `detect_mentions` returns expected ids for fixture content + symbol set; dedup; cap; no self-loop; ambiguous alias dropped; minimum length guard; punctuation stripping.
- Integration: upsert a memory mentioning a known symbol → `metadata.mentions` populated and a RELATES_TO edge with `extractor="mention-detector"` exists; declared `related_to` to same symbol does not create a duplicate mention edge.
- Search: a query returning the memory includes `"mention-link"` in evidence derivation.
- Relink idempotency: running relink twice yields the same edge set; re-ingest with new generation rediscovers mentions.
- Performance: 10k-symbol namespace, 1 memory with 100 mentions → under 50ms.

## 9. Acceptance

A memory whose content names an ingested symbol gains a `mentions` array and a `RELATES_TO` edge (`extractor="mention-detector"`, `source=memory.id`) to that symbol; `search_memory` evidence cites `"mention-link"` for paths traversed via that edge. Re-ingest rediscovers mentions against the new generation.

## 10. Files touched (expected)

- New: `src/mention.rs` (detector + tests).
- Module registration in `src/main.rs` (or `src/lib.rs` if present).
- `src/store/ladybug.rs`: `known_ids` cache; edge materialization in upsert/relink; `neighbor_*_q` select `e.extractor`; evidence derivation.
- `src/rdf/shacl.rs`: validate `mentions` as array of strings.
- `src/schemas/memory-vocabulary.v*.json`: add `mentions` field; bump vocabulary version.
- `src/synaptic_mesh`: strip `mentions` on export (physical symbol ids are local).

## 11. SynapticMesh handling

`metadata.mentions` contains physical, generation-scoped symbol ids that are local to an instance/worktree. On mesh export, strip `mentions` (and do not re-materialize mention edges on import). Imported memories may re-gain mentions on the receiver's next relink against its own symbol index.