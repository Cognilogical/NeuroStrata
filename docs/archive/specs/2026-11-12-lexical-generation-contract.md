# Lexical active-generation visibility — contract (#149)

Status: Phase 1 / WP1.3. Issue: Cognilogical/NeuroStrata#149. Follow-ups:
#127 F1 (validity parity), #127 F5 (`rebuild_scoped` scope), #127 F2
(unicode/starvation fixtures), PAR-02 B4 (owner parity).

## Problem

Both lexical routes index and hydrate every `Memory` row:

- `LadybugStore::ensure_lexical_loaded` and `rebuild_tantivy_lexical` scan
  `MATCH (m:Memory)` with no generation predicate.
- Hydration (`search_lexical`, `search_tantivy_lexical`) uses `get`, which
  does not know about the active pointer.
- `publish_generation` flips the pointer but never invalidates the in-memory
  partition or the Tantivy documents.

Consequences after any re-ingest: duplicate hits for unchanged files, hits for
deleted files, and pre-fix secret-bearing rows still searchable — while the
vector path correctly hides them.

Generation is already encoded in an ingest row's id
(`<namespace>::<generation>::<path>`, `parser::generation::split_scoped_id`).
What the lexical layer lacks is (a) the generation on its own documents, (b)
the row's source so it can be compared against that source's pointer, and (c)
invalidation when the pointer moves.

## Contracts

### C1 — documents carry generation and source

`fts::LexicalDoc` gains `generation: u64` and `source_key: String`.

- `generation` is the middle segment of a generation-scoped id, else `0`
  (unscoped legacy rows: rules, facts, tasks, pre-generation ingest).
- `source_key` is `metadata.source_identity.worktree_id`, the same stable key
  `parser::ingest::source_key_for` records in the `IngestGeneration` pointer,
  else the empty string.
- The Tantivy schema gains `generation` (`INDEXED | FAST | STORED`); every
  write path (`upsert_memory`, `rebuild_scoped`) stamps it.
- `LexicalDoc::new` defaults both fields, so constructors outside the store
  keep compiling.

### C2 — visibility is per source, with a namespace fallback

A row is visible when one of the following holds:

1. `generation == 0` (not generation-scoped), or
2. `source_key` is non-empty and equals the active-generation map entry for
   that source (`IngestGeneration.source_key` → `active_generation`), or
3. `source_key` is empty and the generation is in the namespace's active set
   (the rule the vector `list`/`export_graph` paths already use).

The source-specific rule is required because two worktrees of one repository
share a namespace: generation `5` active for source A does not make source B's
generation `5` current. The namespace set remains the fallback for rows written
before the source stamp (and for non-ingest writers).

C2 is enforced in all four places:

1. load (`ensure_lexical_loaded`),
2. rebuild (`rebuild_tantivy_lexical`),
3. the Tantivy query (zero-score `Must` clause: `generation == 0 OR
   generation IN active(namespace)`), so stale rows cannot consume TopDocs
   slots,
4. hydration on both lexical routes, so a stale index entry can never surface
   a row even if 1–3 missed it.

### C3 — publication invalidates the lexical view

`publish_generation` and `abort_generation` invalidate the namespace after the
pointer write:

- clear the in-memory partition and remove the namespace from `lexical_loaded`;
- if a Tantivy index is open, delete the namespace's documents and rebuild
  them from the just-published active rows.

A failed refresh is a warning, not a failed publication: the graph flip already
committed, and hydration (C2.4) keeps stale rows unreadable until the next
refresh. The next `ensure_lexical_loaded`/`rebuild_tantivy_lexical` retries.

### C4 — owner parity

`lexical_doc` sets `user_id = rebac::extract_resource_owner(payload.user_id,
metadata)`. Both lexical routes therefore use the same owner the vector route
uses: a payload whose `user_id` is empty but whose metadata names an owner is
owned by that principal in all three routes.

### C5 — validity parity at the `as_of` boundary

Presence, not sign: an absent `valid_to` means "no expiry"; a present value
retires once it is at or before the visible moment (an explicit `0` is in the
past). That is the store's existing `is_retired_at` rule. For `as_of = None`,
both engines hide a row exactly when it has a `valid_to` and `valid_to <= now`.
For `as_of = Some(ts)`, both hide when `valid_from > ts` or when the row has a
`valid_to` and `valid_to <= ts`. Tantivy therefore stores a `has_valid_to`
flag: `valid_to` alone cannot distinguish absent from explicit `0`. The query
clause and the per-hit/`doc_visible` rules must agree, so a hidden row cannot
starve a visible one.

### C6 — search quality (#127 F1/F2)

- Literal code tokens (`foo<T>`, `a(b)`, `x+y`, `path/to/file`, `C++`) and
  unicode tokens (`café::naïve`) return the seeded row.
- Archived and retired rows that match a query do not occupy the page: a
  visible row behind them is still returned.
- No test may pass through a disjunction that is true because the query is
  already scoped (`hits.is_empty() || all(...)`); the seeded row is asserted
  present.

### C7 — scoped rebuild scope (#127 F5)

`rebuild_scoped(rows, Some(ns))` never inserts a row whose `ns != ns`. A
foreign row is skipped with a warning; it is not indexed under the scoped
namespace. A scoped rebuild still replaces only its own namespace.

## Tests

Inline, binary-only crate:

```
cargo test --locked --bin neurostrata-mcp <name> -- --test-threads=1
```

| Test | Contract |
|---|---|
| `lexical_search_returns_only_active_generation` | C1, C2, C3: gen 1 then gen 2; exactly the current id on both routes, no duplicates |
| `deleted_path_absent_from_lexical` | C2, C3: a gen-1 file absent from gen 2 is not returned |
| `secret_skipped_absent_from_lexical` | C2, C3: a dropped secret-bearing row is not returned |
| `lexical_owner_parity` | C4: empty payload owner + metadata owner alice agrees on both routes |
| `validity_boundary_parity` | C5: `None`/`Some` boundaries agree across engines |
| `rebuild_scoped_rejects_foreign_namespace` | C7 |
| `e1_code_token_breadth` | C6 |
| `e3_seeded_unicode_returns` | C6 |
| `archived_and_validity_starvation` | C5, C6 |

## Acceptance

- One current hit per expected id; deleted and secret-skipped rows absent on
  both lexical routes.
- The two lexical routes and the vector route agree on owner and validity.
- `rebuild_scoped` enforces its namespace.
- Mutation: removing the generation filter makes the stale/deleted tests fail;
  removing the scope check makes the F5 test fail.
