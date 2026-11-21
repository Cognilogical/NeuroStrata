# 2026-11-21 — Vault Follow-ups: Edges, Retirement, Fingerprint (W-C)

> **Baseline**: `cognilogical/main` @ `a75b243a` (#341 merged)
> **Issues**: #343, #344, #345 (P2)
> **Branch**: `fix/vault-followups`
> **Workflow**: Gate 0 trace → spec → red evals → minimal fix → green evals →
> regression suites

---

## 1. Scope

Three independent defects in the vault sync surface (`src/vault_ld/mod.rs`,
`src/vault_ld/vault_sync.rs`). They are fixed in one stack because they share
the import/export pipeline and the same regression suite.

Non-goals: #349/#338, connector code, parser changes, vault format redesign,
and any opportunistic refactor.

---

## 2. Gate 0 trace

### Write path

1. `import_vault` parses each note and derives `canonical_sha256` from
   `VaultNote::fingerprint` (content, tier, related).
2. Unchanged check compares the parsed fingerprint against the prior head's
   `metadata.canonical_sha256` (or derives one from the prior payload).
3. Metadata is rebuilt from the prior head minus `IMPORT_OWNED_METADATA_KEYS`,
   then overlaid: `vault_source`, `stub`, `canonical_sha256`, `related`,
   `valid_from`, `supersedes`.
4. `LadybugStore::upsert` materialises edges from `edge_specs(payload.metadata)`.

### Edge parser

`parse_edge_specs` (`src/store/ladybug.rs`) maps exactly five keys:
`related_to` → `RELATES_TO`, `contained_by` → `CONTAINS`, `governs` →
`GOVERNS`, `extracted_from` → `EXTRACTED_FROM`, `depends_on` → `DEPENDS_ON`.
`related` is not a relation key. `#341` made `related` import-owned and kept
`related_to`/`relates_to`/`governs`/`depends_on` import-owned-and-removed, but
wrote no relation key, so no edge was ever materialised.

### Retirement semantics

`LadybugStore::is_retired_at(metadata, now)` is `valid_to` non-null and
`<= now`. `list_rows`/`is_active_scoped_row` are generation-scoped only and
never consult `valid_to`/`superseded_by`, so `store.list` returns retired rows.
The importer's `retire_metadata` stamps `{valid_to, superseded_by,
archived_by_vault_import}` on unscoped `<id>.vault-history.<uuid>` copies.

### Export path

`export_vault_with_limits` lists rows, filters only by ReBAC, converts each
with `VaultNote::from_payload` (which unions `related`, `related_to`,
`governs`, `depends_on`), and renders fixed frontmatter. No retirement state
reaches the vault.

### Gate 0 decision (#343)

Write the parser-recognised key `related_to` on the imported head while keeping
`related` for the vault round-trip and export. `parse_edge_specs` then
materialises `RELATES_TO` with no parser change, no new relation vocabulary,
and no blast radius outside the import head. `related` remains the vault-owned
render; both keys carry the same parsed link set, and `from_payload`
sorts/dedups the union, so export round-trips do not duplicate links.

Edge materialisation is target-order sensitive: `upsert` skips an edge whose
target does not exist yet. Import writes notes in id order, so a declarer may
precede its target. Phase 2 therefore retries the upsert of any head whose
`UpsertReport.deferred_edges` is non-empty **after** every head (and stub) is
written. One retry pass materialises forward references and cycles; a target
absent from both store and vault stays deferred and no error is invented.

---

## 3. #343 — Imported WikiLinks must become stored edges

### Invariant

A WikiLink that exists in the vault exists as a stored `RELATES_TO` edge
between the two persisted memories after import, independent of note id order.

### Evals (must fail on baseline)

- `import_materializes_wikilink_edges_and_exports_without_duplicates`
  - Import `note-a` linking `note-b`, with `note-b` present.
  - Assert `store.list_edges(ns)` contains `("note-a", "RELATES_TO",
    "note-b")` — not `metadata.related`.
  - Export and assert `note-a.md` carries exactly one `[[note-b]]` link, then
    reimport reports no changes (no version churn from the duplicate rendering).

---

## 3a. #346 — A removed WikiLink must retire its stored edge

### Invariant

After an import, the `RELATES_TO` edges that originate at an imported head
with `extractor = 'upsert'` equal the vault's parsed link set for that note.
A link removed from the vault leaves no edge — including the transition to an
empty link set, which retires every such edge. Kept links survive, and edges
owned by other provenance (`mention-detector`, hand-authored) or other
relations (`GOVERNS`, `DEPENDS_ON`, `CONTAINS`, `EXTRACTED_FROM`) are never
touched.

### Mechanism

`upsert` only MERGEs declared edges; its deletion pass covers only
mention-detector edges. Reconciliation is therefore a caller-owned step:

- `VectorStore::reconcile_declared_edges(namespace, id, rel_type, keep)` —
  default no-op for stores without a graph projection.
- `LadybugStore` deletes edges `a-[r:rel_type]->b` where `a.id = id`,
  `a.namespace = namespace`, `r.extractor = 'upsert'`, and `b.id` is not in
  `keep`. An empty `keep` drops the `NOT IN` clause, deleting all of them.
- `import_vault_with_limits` calls it for every planned head **before** that
  head's `upsert`, so a declared target that resolves to a different stored id
  cannot delete the edge the same write just created; the deferred-edge replay
  reconciles again before its retry upsert.

The pass is explicit rather than implicit in `upsert`: an absent `related_to`
key means "not declared by this write" for ordinary callers, and only the
vault knows that its parsed link set is authoritative and total.

### Eval (fails on baseline)

- `reimport_retires_stored_edges_for_removed_wikilinks`
  - Import `linker` → `stale-target`, `kept-target`; assert both edges exist
    (control) and seed an unrelated `GOVERNS` upsert edge.
  - Rewrite `linker` with only `kept-target`, reimport; assert the
    `stale-target` edge is gone, `kept-target` survives, `GOVERNS` survives,
    export renders exactly one `[[kept-target]]` and no `[[stale-target]]`.
  - Clear the last link, reimport; assert no `RELATES_TO` from `linker`
    remains and the unrelated `GOVERNS` edge still does.

### Migration and residual risk

No schema change. Reconciliation is delete-only and idempotent, so a head that
reimports after this patch converges in one pass. Residual: reimporting a
byte-identical note is a deliberate no-op, so an edge orphaned by a *pre-fix*
import of that same note survives until the note is edited (the no-op path
never reaches reconciliation). This is bounded by the fact that the orphan was
created by a pre-fix removal import; an operator can force convergence by
touching the note. `LadybugStore::upsert` behaviour and the parser are
untouched, so no other writer changes shape.

---

## 4. #344 — Retired rows must not be exported

### Invariant

An export contains only rows that are live at export time: no row whose
`valid_to <= now`, and no row carrying `superseded_by`. Live and future-dated
rows remain. Note count and canvas node set describe the same row set.

### Behavior

- Export filters after ReBAC and before conversion using
  `store::ladybug::is_retired_at(metadata, now)` plus a non-null
  `superseded_by` (an unscoped superseded row may carry only that stamp).
- Import ignores notes whose `@id` contains `.vault-history.`: history copies
  are store-internal projections manufactured by the importer, never
  vault-authored knowledge, and an old export may still contain them.

### Stale-vault risk (documented, not eliminated)

Export is additive: it never deletes files from the vault. A vault populated
by a pre-fix export still contains retired note files. Reimporting one is
normally a no-op because the stored fingerprint still matches; the history-id
guard makes the `.vault-history.` class inert even if such a file was edited.
Residual risk: an *edited* stale file for an ordinary superseded id can
re-head that id (`valid_to`/`superseded_by` are import-owned and stripped).
This is the same resurrection class the issue names; it requires a human edit
of a stale file and is recorded here rather than silently broadening import
authority in a P2 patch.

### Evals (must fail on baseline)

- `export_excludes_retired_rows_and_keeps_canvas_coherent`
  - Seed a live rule, a past-dated retired rule, a `superseded_by`-only
    retired rule, and a future-dated rule.
  - Assert exported files exist for live and future rows and not for retired
    rows; `notes_exported == 2`; `topology.canvas` has exactly 2 nodes.
- `import_ignores_stale_vault_history_files`
  - Write `<id>.vault-history.<uuid>.md` into the vault, import.
  - Assert no live memory is created at that history id.

---

## 5. #345 — Fingerprint must include `rdf_type`

### Invariant

A type-only edit (`@type` changes; content, tier, related unchanged) creates a
new head version. An unchanged reimport remains a no-op. Upgrading from the v1
fingerprint does not mass-version existing heads.

### Migration

`fingerprint()` becomes v2: content, tier, `rdf_type`, sorted/deduped related.
`fingerprint_legacy()` preserves the v1 input set for comparison only.
`FINGERPRINT_VERSION = 2` is stamped as `metadata.fingerprint_version` on every
imported head (and is import-owned).

Unchanged check:

- `fingerprint_version >= 2`: stored hash equals the v2 fingerprint (hashless
  rows derive it from the prior payload). No fallback: v2 identity is exact.
- Version absent / v1: compare the stored hash against the v1 fingerprint (or
  the derived v1 payload fingerprint when hashless) **and** require the prior
  `memory_type` to equal the note's declared type. A legacy head keeps its
  no-op round-trip; a type-only edit still versions because the type differs.
  The upgraded head is then stamped v2.

No bulk re-version is performed: identity is compared per note, and only a
note whose *real* type changed is versioned.

### Evals (must fail on baseline)

- `type_only_edit_creates_new_head_version`
  - Seed a v1 head (`canonical_sha256` = legacy fingerprint, no
    `fingerprint_version`); reimport with only `@type` changed.
  - Assert `notes_imported == 1`, head `memory_type` is the new type,
    `supersedes` exists, head metadata `fingerprint_version == 2`.
- `legacy_v1_head_reimport_is_not_mass_versioned`
  - Same v1 seed, unchanged vault file; assert `notes_imported == 0` and no
    `supersedes`.
- `unchanged_v2_reimport_is_a_noop`
  - Import once, reimport unchanged; assert `0` and no new history.

---

## 6. Files

- `src/vault_ld/mod.rs` — v2 fingerprint, legacy fingerprint, version const.
- `src/vault_ld/vault_sync.rs` — `related_to` head key, deferred-edge retry,
  retirement export filter, history-id import guard, `fingerprint_version`
  stamp, per-head `reconcile_declared_edges` call, updated
  `import_reconciles_preserved_edge_fields_with_edited_links`
  assertion (that key now exists by design).
- `src/traits.rs` — `VectorStore::reconcile_declared_edges` default no-op.
- `src/store/ladybug.rs` — `extractor = 'upsert'` RELATES_TO reconciler.

## 7. Risk

- Added write per linked note only when its edges were deferred; no change to
  unlinked notes.
- One extra metadata key (`related_to`, `fingerprint_version`) per imported
  head; both import-owned.
- Export file set shrinks for namespaces with history: stale files remain on
  disk (additive export), documented above.
- One extra delete-only statement per planned note before its upsert; it
  performs no page writes when the edge set already matches. Caller-scoped, so
  non-vault upserts keep their current behaviour.
- Parser and `upsert` edge materialization untouched.
