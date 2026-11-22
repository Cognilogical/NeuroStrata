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
4. `LadybugStore::upsert` canonicalises unqualified edge targets through
   `KnownIds::resolve` and then materialises edges from
   `edge_specs(payload.metadata)`.

### Target resolution

`KnownIds` indexes every ingested symbol by its logical path
(`logical_path_of`: namespace and generation stripped). An unqualified
declaration that equals such a path resolves to the qualified symbol id. The
edge write repeats the same inference in `resolve_target_on_write`, which also
tries the active generation and path suffixes. Vault note ids are unqualified
`@id` strings and may legitimately equal a symbol's logical path (a note
`src/lib.rs` and an ingested `src/lib.rs`), so both steps can bind the symbol
instead of the note. The importer therefore stamps `exact_edge_targets: true`;
both steps opt out and bind the declared id verbatim.

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
- `import_vault_with_limits` calls it for every planned head **after** that
  head's `upsert` commits, and again after the deferred-edge replay upsert. A
  failed head upsert therefore leaves the prior edge set intact; the old head
  and its edges survive together, and a later unchanged reimport is a no-op
  rather than a skipped repair. Reconciliation is delete-only, so running it
  after the write reaches the same declared set.

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

## 3b. #363 — Vault edges bind the exact note, and retire atomically

### Invariants

1. A WikiLink written by the vault importer binds the note id named in the
   vault, exactly. A stored `RELATES_TO` edge or `metadata.related_to` entry
   for a vault-origin head never names an ingested symbol that happens to
   share the note's logical path. Export renders exactly the parsed link set,
   adds no second WikiLink, and the next import fingerprint is stable.
2. Edge reconciliation commits only with a successful head replacement. If the
   head `upsert` fails, the previous edge set is untouched and the old head
   remains current; an unchanged reimport of the original note is a no-op, not
   a skipped repair.

### Mechanism

- The importer stamps `metadata.exact_edge_targets = true` on every planned
  head. Ordinary memories never carry the flag, so their write-path resolution
  is unchanged.
- `LadybugStore::upsert` skips the `KnownIds::resolve` rewrite when the flag is
  present. Edge materialisation checks it before the qualified-ID branch and
  looks the target up verbatim inside the importer's namespace
  (`exact_target_in_namespace_exists`), removing an own-namespace prefix first
  (§3c), instead of through `resolve_target_on_write`'s generation and suffix
  inference. A target not yet written defers to the existing replay pass, which
  retries the same exact lookup.
- `reconcile_declared_edges` runs after the head `upsert` returns `Ok`, and
  after the deferred replay `upsert` returns `Ok`; a failed upsert performs no
  reconciliation. The history row cleanup on failure is unchanged.

Both halves matter: skipping only the `KnownIds` rewrite still leaves the edge
free to resolve to the same-path symbol during materialisation, after which the
post-write reconciliation deletes that edge because it does not match the
declared id.

### Evals (fail on baseline)

- `vault_targets_survive_symbol_path_collisions`
  - Seed a same-namespace, published symbol `<ns>::<gen>::shared-target` (user
    `auto-ingestor`, type `symbol`), then import vault notes `shared-target`
    and `linker` linking `shared-target`.
  - Assert head `related_to == ["shared-target"]`, the edge ends at the note id
    and not the symbol id, export renders exactly one `[[shared-target]]` and
    no symbol link, and the reimport imports `0` notes.
- `failed_reimport_keeps_existing_edges_with_the_old_head`
  - Import `linker` → `stale-target`, `kept-target`; edit it to keep only
    `kept-target`; inject a head-upsert failure for `linker` with a delegating
    test store.
  - Assert the import fails, the old head still declares both links, and the
    `stale-target` edge remains.

### Residual risk

- `exact_edge_targets` is a metadata flag, so a direct store caller can set it
  to opt a row out of path inference. It grants no authority; it only narrows
  how that row's own declaration is bound.
- If the head `upsert` succeeds but the follow-up reconciliation fails, the
  import reports an error and stale edges remain until the next changed
  import of that note. A byte-identical reimport stays a no-op (same class as
  the migration note in §3a), so convergence requires touching the note or
  running reconciliation explicitly.
- Reconciliation is a separate statement from the head write; the two are not
  one transaction. The ordering makes a failed write safe, but a process crash
  between them leaves either old edges plus new head (retried on the next
  changed import) or the intended set; it cannot leave deleted edges with the
  old head.

### 3c. Exact-target ordering for qualified spellings

#### Invariant

For a head carrying `exact_edge_targets`, target resolution happens before the
generic qualified-ID branch. No exact-mode target may reach
`resolve_target_on_write`'s generation or suffix inference, including a target
that contains `::`. A same-namespace spelling (`<ns>::z`) names the vault note
stored as the bare id `z`; a target absent at write time defers to the replay
pass instead of resolving through a symbol that shares the logical path. The
declared spelling stays in metadata for export, while edge binding, stub
detection and reconciliation share the verbatim-first candidate order (§3e).

#### Mechanism

- `LadybugStore::upsert` checks `exact_edge_targets` first. The declared target
  is looked up verbatim with `exact_target_in_namespace_exists`; only when that
  is absent is the importer's own namespace prefix stripped and the alias
  retried (§3e). Missing defers, and the earlier qualified branch keeps its
  inference for ordinary memories only.
- `unresolved_wiki_targets` applies the same candidate order within the vault,
  while the reconciliation keep-set resolves each spelling against both the
  vault-known ids and the stored ids (§3e), so a qualified spelling neither
  mints a bogus alias stub nor deletes the edge it just bound.

#### Eval (failed on `c14f5de0`)

- `qualified_wikilink_binds_exact_note_before_symbol_resolution`
  - Seed a published same-namespace symbol `<ns>::<gen>::z`, then import vault
    notes `z` and `linker` linking `<ns>::z`; `linker` sorts before `z`.
  - Assert `related_to == ["<ns>::z"]`, the `RELATES_TO` edge ends at `z` and
    not the symbol, export carries exactly one `[[<ns>::z]]` and no symbol
    link, and the reimport imports `0` notes with an unchanged fingerprint.
  - On `c14f5de0` the qualified branch inferred the symbol before the exact
    branch; reconciliation then deleted the non-matching edge
    (`the edge must target the exact vault note z: []`).

### 3d. Exact-target replay during ingestion

#### Invariant

`relink_edges` replays every live memory's declarations at the end of an
ingest (`src/parser/ingest.rs:2532`). For a row carrying
`exact_edge_targets`, replay binds each target by the same verbatim-first
rule as the write path (§3e): the declared id is checked before an
own-namespace alias. A target that is not stored in the namespace stays
deferred; replay never falls back to `KnownIds` path, suffix, or symbol
inference. Ordinary memory relink resolution is unchanged. Therefore an
imported edge `a → z` survives an ingest of a same-path symbol `z`: the
stored edge set, export links, and reimport fingerprint do not change.

#### Mechanism

- The declaration loop reads `exact_edge_targets` per memory, not globally.
- Flagged rows resolve against the namespace id set already loaded by the
  same `list` call that supplies the declarations, skipping an absent target
  so the declaration remains for a later replay. The existing bare-vs-
  qualified form (`<ns>::z` names the stored note `z`) is applied first.
- Unflagged rows keep the existing `KnownIds::active(...).resolve(...)` path
  and its legacy absolute-path suffix rule.

#### Eval (failed on `80a678fd`)

- `relink_preserves_exact_vault_targets_across_ingestion`
  - Import vault notes `a` (linking `z`) and `z`; assert one `RELATES_TO`
    edge `a → z`.
  - Stage and publish a same-namespace generation containing symbol
    `<ns>::<gen>::z`, then call `relink_edges(ns, "code")`.
  - Assert the `RELATES_TO` target set from `a` is still exactly `["z"]`, no
    edge ends at the symbol id, export renders one `[[z]]` and no symbol
    link, and an unchanged reimport imports `0` notes with the same
    fingerprint.
  - On `80a678fd` replay added `a → <ns>::<gen>::z` (and reported it as a
    legacy suffix rewrite), so the target set was `[symbol, "z"]`.

#### Residual risk

- Replay still runs mention detection for non-ingestor rows; a vault note
  whose prose names a symbol can gain a `RELATES_TO` mention edge from that
  pass. This is separate from declaration replay and out of scope here; the
  exact-declared edge set is unaffected.
- The existence check uses the namespace rows loaded at replay start, so a
  target written concurrently after that read defers to the next ingest.
  Deferral is safe: the declaration is stored in metadata and retried.

### 3e. Verbatim-first exact targets

#### Invariant

For a head carrying `exact_edge_targets`, the declared target is tried
verbatim in the importer's namespace before any own-namespace prefix is
interpreted as an alias. An exported note may legitimately have an `@id`
that starts with the namespace and carries a generation segment
(`<ns>::7::path`); it is that note, not the alias `7::path`. The same rule
governs all four exact-mode paths:

1. **Stub discovery** (`unresolved_wiki_targets` + the import stub loop):
   a declared target is unresolved only when neither the vault nor the store
   holds any candidate spelling. Minting uses the spelling the vault
   declared, so a qualified id never silently becomes a different stub.
2. **Write materialisation** (`LadybugStore::upsert`): the verbatim id is
   looked up in the importer's namespace first; the stripped alias is tried
   only if it is absent; otherwise the edge defers.
3. **Replay** (`LadybugStore::relink_edges`): the same candidate order
   against the namespace's stored ids; no candidate means deferred, never a
   `KnownIds` inference.
4. **Reconciliation** (the importer's keep-set): the keep-set uses the id
   the write would bind, not a separately stripped spelling, so it cannot
   delete the verbatim edge it just created.

The candidate order is centralised in
`parser::generation::exact_target_spellings`: verbatim first, then the
own-namespace-stripped alias, never the reverse. Precedence selection is
centralised in `exact_target_match` (and its fallible form) so materialisation,
replay, and reconciliation cannot drift.

**Dual-set precedence.** The importer holds two id sources: the vault-known
note ids and the stored ids. Reconciliation must resolve against *both sets per
spelling*, never one set to exhaustion before the other. If the store holds
`<ns>::z` and a partial vault holds only the note `z`, the first candidate
`<ns>::z` exists in the store, so it wins before the vault-known alias `z` is
considered — the same id the upsert materialises. A vault-first short-circuit
would keep `z` and the delete-only reconciler would remove the freshly written
edge, with no retry and no repair on an unchanged reimport. Unresolved
declarations keep the declared spelling.

#### Evals (failed on `c209c4ad` and `6f132a49`)

- `qualified_note_id_binds_verbatim_before_alias_stripping`
  - Import vault notes `linker` → `<ns>::7::path` and the note whose `@id`
    is exactly `<ns>::7::path`.
  - Assert the import reports no error, the verbatim note exists and no
    `7::path` stub exists, the `RELATES_TO` edge ends at `<ns>::7::path` and
    never at `7::path`, export renders exactly one `[[<ns>::7::path]]`, and
    an unchanged reimport imports `0` notes with an unchanged fingerprint.
  - On `c209c4ad` normalization rewrote the link to `7::path` before the
    known-note check: the import minted a `7::path` stub (which then failed
    its Turtle subject validation) and the real note received no edge.
- `stored_verbatim_target_outranks_vault_alias_during_reconciliation`
  - Store a memory at `<ns>::z`; import a partial vault containing only the
    note `z` and a `linker` note linking `<ns>::z`.
  - Assert the import succeeds with no errors, the only `linker` `RELATES_TO`
    edge ends at `<ns>::z` (not the alias `z`), the vault note `z` is not a
    stub, export renders exactly one `[[<ns>::z]]` and no `[[z]]`, and an
    unchanged reimport imports `0` notes while leaving the edge intact.
  - On `6f132a49` the vault-only keep resolution chose `z`; the upsert still
    materialised `linker → <ns>::z`, and reconciliation deleted it
    (`reconciliation must keep the id materialization bound: []`). Import
    reported success, so no retry was scheduled and reimport could not repair
    the edge.

#### Residual risk

- Stub discovery still caches one store-backed resolution per distinct
  declaration for the import turn. Reconciliation no longer reuses that cache:
  it checks the store per candidate spelling so a kept verbatim id cannot be
  masked by an earlier alias-only answer. The added reads are bounded by
  distinct declared targets, not by edge count.
- If an intermediate import fails after binding a verbatim edge, a later
  alias-only spelling resolving to a different row can still retire that edge
  through reconciliation, as with any changed declaration. The keep-set
  follows the current declaration; it does not preserve old targets.
- An unresolved declared spelling is used for the stub. For a namespaced id
  that is not also a valid Turtle subject scheme (for example a namespace
  containing `_`), stub validation refuses it and reports the error instead
  of minting an alias; the declaration stays deferred.

### 3f. Pending preferred targets replay after the write phase

#### Invariant

A declaration's preferred binding is the first candidate spelling the vault
knows. If that id is not stored when the declaring head is written, the head
depends on the post-write replay even when the write bound a stored alias
candidate and therefore reported no deferred edge. After every head exists the
replay rebinds the preferred id, and reconciliation keeps it. A declaration
whose preferred id is already stored, or that has no vault-known candidate,
keeps the existing behaviour: it replays only when the write reports a
deferred edge.

#### Mechanism

- `exact_keep_targets` returns the keep ids together with a
  `pending_preferred` flag. When a candidate spelling is vault-known the store
  is queried once for that id; absent (or unreadable) sets the flag while the
  id stays the keep target.
- The write loop schedules the existing deferred replay when the upsert report
  is non-empty **or** `pending_preferred` is set. Replay recomputes the keep
  set — now preferring the stored verbatim id — re-upserts the same head, and
  reconciles against it.
- The flag is per declarer, so notes whose preferred target was already
  stored do not replay. Ordinary store callers and alias-only declarations
  are unchanged.

#### Eval (failed on `667c0cd9`)

- `forward_reference_rebinds_preferred_target_after_alias_fallback`
  - Store the bare note `z`; import a vault containing `a → ns::z` and the
    note whose literal id is `ns::z` (`a` sorts first).
  - Assert the only `a` `RELATES_TO` edge ends at `ns::z` (no `a → z`), export
    renders exactly one `[[ns::z]]` and no `[[z]]`, and an unchanged reimport
    imports `0` notes while leaving the edge intact.
  - On `667c0cd9` the first write bound `z` and reported no deferred edge, so
    no replay ran; reconciliation then deleted it
    (`the replay must rebind the preferred id once it exists: []`).

### 3g. Unchanged declarers rebind when a preferred target arrives

#### Invariant

A declarer whose fingerprint matches its stored head skips planning. If its
declared preferred exact target is imported for the first time in the same
turn, the skip would otherwise leave the edge on the alias fallback forever.
After every head is written, each unchanged declarer whose current preferred
keep set contains a newly imported id has its `RELATES_TO` edges rebound:
missing edges are materialised and stale ones reconciled away. The head row is
not rewritten, so content, type, identity, `valid_to`/`supersedes`, and
history rows are unchanged. A turn that imports no relevant id performs no
edge work, so repeated unchanged imports stay a stable no-op.

Rebinding still mutates a protected head's declared graph, so an unchanged
declarer is queued only when `writable_prior(principal, prior)` accepts its
stored payload. A foreign- or mesh-owned head is never rebound: its edges stay
exactly as stored and the turn remains a silent no-op for that head, matching
the unchanged path's prior behavior.

A rebind is also a `RELATES_TO`-only operation, so it may act only on a
`RELATES_TO` declaration. An unchanged head is queued only when
`metadata.vault_source` or `metadata.exact_edge_targets` marks it
vault-import-owned, and its declaration set comes from the head's
`related`/`related_to` arrays only. `VaultNote::from_payload` unions
`governs`/`depends_on` into the exported link set, so a store-owned rule's
round-trip looks unchanged; replaying that union as a rebind would add an
unintended `RELATES_TO` beside the rule's `GOVERNS` edge. Store-owned
rules and memories keep their original relationship types untouched.

#### Mechanism

- The planning loop runs the same `writable_prior` sovereignty gate used for a
  changed head before recording an unchanged declaration. A protected head is
  dropped at `continue` without an error or edge mutation.
- `vault_declared_targets(prior)` returns the sorted, deduplicated
  `related`/`related_to` set only for a vault-import-owned head; every other
  head is skipped, so `governs`/`depends_on` links are never rebound as
  `RELATES_TO`.
- The write loop collects the ids whose head upsert succeeded
  (`imported_ids`).
- After the deferred replay, an unchanged declarer recomputes
  `exact_keep_targets` against the now-complete store and vault sets. If any
  keep id is newly imported, `VectorStore::rebind_declared_edges` creates the
  missing outgoing edges, then runs the same `reconcile_declared_edges`
  retirement pass. The store method defaults to a no-op; only the graph-backed
  store overrides it, and no ordinary caller changes.

#### Eval (failed on `b0019a31`)

- `unchanged_declarer_rebinds_when_preferred_target_arrives`
  - Store the bare `z`; first import binds `a → ns::z` to that alias (partial
    vault: `a.md` plus `z.md`).
  - Second import adds the literal `ns::z` note while `a.md` is byte-identical.
  - Assert `a` keeps content, type, `canonical_sha256`, no `supersedes`, and
    zero `a.vault-history.` rows, while its only `RELATES_TO` edge ends at
    `ns::z`; export renders exactly one `[[ns::z]]` and no `[[z]]`; a third
    unchanged import imports `0` notes and leaves the edge unchanged.
  - On `b0019a31` the unchanged `a` never replanned, so the alias edge
    survived (`the unchanged declarer must rebind its preferred target:
    ["z"] vs ["ns::z"]`).
- `unchanged_foreign_declarer_does_not_rebind_protected_edges` and
  `unchanged_mesh_owned_declarer_does_not_rebind_protected_edges`
  (failed on `32d4ce41`)
  - Seed the bare `z`, an unchanged declarer `a` already bound to `z`, and a
    vault that adds the literal `ns::z`. One case makes `a` foreign; the
    other makes it mesh-authored by another peer while the local owner
    matches the importing principal.
  - Assert `a` keeps its head payload byte-for-byte, zero history rows, and
    its only `RELATES_TO` edge still ends at `z`; no `ns::z` edge appears.
  - On `32d4ce41` the unchanged queue bypassed `writable_prior`, so the
    import rebound the protected head (`["ns::z"] vs ["z"]`).
- `owned_unchanged_declarer_still_rebinds_preferred_target` is the control:
  a writable unchanged declarer still follows the newly imported preferred id.
- `unchanged_owned_rule_keeps_governs_without_relates_to_rebind` (failed on
  `3a6ad8a3`)
  - Seed an owned rule `rule-r` with `governs: [target-b]` (GOVERNS edge) and
    `target-b`; export, import byte-identical, then edit `target-b` and import
    again.
  - Assert `rule-r` keeps its content and `canonical_sha256`, its `GOVERNS`
    edge survives, and no `rule-r → target-b` `RELATES_TO` edge exists.
  - On `3a6ad8a3` the unchanged rule entered the declarer queue and the
    exported `governs` link was rebound as `RELATES_TO`, producing both edges.

#### Residual risk

- The gate reuses `writable_prior` and therefore inherits its policy: a row
  with no owner and default private eligibility is writable by any principal,
  matching the changed-head path. Protected unchanged heads are skipped
  silently rather than reported in `errors`, preserving the historical no-op
  contract; an operator who needs a signal must compare store state.
- The rebind is edge-only and idempotent, but a crash between edge creation
  and reconciliation can leave both bindings until the next import that
  imports a relevant id. A later unchanged import with no new ids will not
  repair it; a changed import of the declarer will.
- The trigger is membership of a keep id in this turn's imported ids. A
  preferred id that was already stored but whose edge was mis-bound by an
  older build is not re-detected without comparing materialised edges, which
  this fix intentionally does not do.
- The vault-ownership gate trusts `vault_source`/`exact_edge_targets` in the
  head metadata. A store-owned row that copied those markers would be treated
  as a vault declaration; import owns both keys on the heads it writes, and a
  row without them keeps its original relationship types even if its vault
  export rendered `governs`/`depends_on` as links.

### 3h. Provenance retires with the edge it cites

#### Invariant

`metadata.edge_sources` cites only edges that the row actually has. When
declaration reconciliation retires a stale declared edge, it also removes the
`upsert`-owned provenance entry for that relation and target — including when
the replacement declaration set is empty. Entries for other relations
(`GOVERNS`, `DEPENDS_ON`, …), entries owned by another extractor (mentions),
and rows that carry no provenance are untouched. The metadata update runs on
the same reconciliation path as the edge delete and writes the row's metadata
only, so it never creates a content version, `supersedes` stamp, or history
row.

#### Mechanism

- `LadybugStore::reconcile_declared_edges` reads `m.metadata` after deleting
  stale outgoing `extractor = 'upsert'` edges and retains a provenance entry
  only when it matches the reconciled `rel_type`, is `extractor = 'upsert'`
  with `orientation = 'self_to_target'`, and its `resolved_target` is in the
  keep set. An empty keep set therefore clears every entry the reconciler
  owns for that relation.
- The provenance write is skipped when nothing was removed; a read or write
  failure propagates as a reconciliation error (the caller reports it) rather
  than being swallowed.
- Nonempty declarations were already covered at the write path: `upsert`
  replaces `edge_sources` whenever it resolves at least one declaration. The
  gap was the empty declaration, where `upsert` writes nothing and import
  preserves the prior field.

#### Eval (failed on `3b4c647b`)

- `removed_wikilink_retires_edge_source_provenance`
  - Import `provenance-declarer → provenance-target`; assert a search with
    `include_references: true` cites the target (control).
  - Remove the declarer's final WikiLink, reimport; assert the `RELATES_TO`
    edge is gone, the stored metadata no longer contains a provenance entry
    for the target, and the same search no longer cites it.
  - On `3b4c647b` the edge was deleted but `edge_sources` still cited
    `provenance-target`, so both the metadata read and the search report kept
    the deleted relationship.
- `reconcile_retires_edge_sources_with_the_edge` (store-level control)
  - A declarer with `related_to` and `governs`; reconcile `RELATES_TO` with an
    empty keep set.
  - Assert the `RELATES_TO` provenance and edge are gone, the `GOVERNS`
    provenance and edge survive, and an ordinary row gains no provenance.

#### Residual risk

- A database that already performed a buggy removal before this patch keeps
  its stale entry until that note changes: a byte-identical reimport is a
  no-op and never reaches reconciliation (same class as the edge orphan in
  §3a). Touching the note or a changed import converges it.
- The edge delete and the metadata write are two statements. A failure between
  them leaves the edge gone with stale provenance and reports an error; the
  next changed import reconciles again. The reverse (provenance removed, edge
  kept) cannot happen because the provenance pass runs after the delete.

### 3i. Rebound declarations mint matching provenance

#### Invariant

Every live declared edge has exactly one `metadata.edge_sources` entry that
cites the declaration that produced it, including an edge created by an
unchanged-declarer rebind. When `a → z` is rebound to `a → ns::z`, the
replacement entry names `declared_target = ns::z`, `resolved_target = ns::z`,
and the same relation, declaration index and evidence id that `upsert` would
stamp. Reconciliation then retires the alias entry with the alias edge, so the
row never cites a target it is not bound to and never loses the citation for
the edge it is bound to. Rebinding one declaration never removes an entry for
another binding: a retained declaration keeps its citation, and multiple
rebinds in the same import each keep theirs, so `include_references` returns
one citation per live target. The provenance update is metadata-only: no
content version, `supersedes` stamp, or history row is created.

#### Mechanism

- `edge_provenance_entry` and `EdgeProvenanceSource` are the single builder for
  both writers. `upsert` reads the coordinates from the payload being written;
  `rebind_declared_edges` reads the same fields (`metadata`, `location`,
  `location_lines`, `content`) from the unchanged stored head, so an entry's
  shape and evidence id cannot drift between the two paths.
- `VectorStore::rebind_declared_edges` takes the importing note's `declared`
  list, positionally aligned with `keep`. For each created edge it builds the
  entry and replaces only an existing entry with the same
  `(rel_type, extractor, resolved_target)` — the binding identity. Every other
  entry survives, and one metadata write after the loop accumulates all new
  entries. The edge carries the same computed `evidence_id` as its entry.
- The subsequent reconciliation keep-set contains the preferred id, so it
  removes the alias edge and the alias entry and retains the replacement pair.

#### Eval (failed on `414f7981`)

- `rebound_declaration_keeps_edge_source_provenance`
  - Import `a → ns::z` and the alias note `z`; the first import binds `a → z`.
  - Add the literal `ns::z` note while `a.md` is byte-identical; reimport.
  - Assert the only `a` `RELATES_TO` edge ends at `ns::z`, the head keeps its
    content and `canonical_sha256`, has no `supersedes`, and has zero history
    rows; `edge_sources` contains a `RELATES_TO` entry whose `declared_target`
    and `resolved_target` are both `ns::z` and no `RELATES_TO` entry for `z`.
  - A `neurostrata_search_memory` lexical call with `include_references: true`
    cites `ns::z` and not `z`.
  - On `414f7981` the edge rebound correctly but `edge_sources` was `[]`:
    reconciliation had retired the alias entry and the rebind minted none.
- `rebound_declaration_preserves_retained_edge_provenance` (failed on
  `79ac5a66`)
  - Import an unchanged note with two declarations: `a → c` and `a → ns::z`,
    where only the bare `z` alias exists. The first import binds `c` and `z`.
  - Add the literal `ns::z` note while `a.md` is byte-identical; reimport.
  - Assert both `a` edges are live (`c`, `ns::z`), `edge_sources` holds exactly
    one `RELATES_TO` entry for each live target and none for `z`, and an
    `include_references` search cites both `c` and `ns::z`.
  - On `79ac5a66` the replacement `retain` kept only entries matching the new
    binding, so `c`'s citation was deleted even though its edge stayed live.
- `multiple_rebound_declarations_accumulate_edge_provenance` (failed on
  `79ac5a66`)
  - Same shape with `a → ns::y` and `a → ns::z` declared; both aliases exist,
    then both literal notes arrive in one import.
  - Assert both rebound edges are live, each has exactly one citation, the
    alias citations are gone, and `include_references` cites both targets.
  - On `79ac5a66` each iteration deleted the previous iteration's entry, so
    only the last rebind (`ns::z`) remained cited.

#### Residual risk

- `declared` and `keep` are positional. The only caller is the vault importer,
  which derives both from the same note, so a future caller must preserve that
  alignment; a mismatch only mislabels `declared_target` and the declaration
  index, never the binding or the citation's resolved target.
- A crash after the edge MERGE and before the metadata write leaves the edge
  uncited, not wrongly cited; the next changed import of the head re-stamps
  provenance through `upsert`. A byte-identical import does not retry, same
  class as §3a.
- Binding identity is `(rel_type, extractor, resolved_target)`; a declaration
  whose target rebinds to the same id is one binding, so duplicate
  declarations of that target collapse to one citation. `declaration_index`
  and `declared_target` are labels carried positionally from the unchanged
  head's declaration list, not part of the replace key.

### 3j. Replayed vault declarations belong to reconciliation

#### Invariant

A declaration belonging to a head carrying `exact_edge_targets` is
materialised by `relink_edges` at the end of ingestion with
`extractor = 'upsert'` — the same ownership as the import write path—not
`'relink'`. Its `evidence_id` and `metadata.edge_sources` citation name the
same binding with `extractor = 'upsert'`, so the row never cites a replayed
edge under a different owner. Removing the WikiLink and reimporting therefore
retires both the imported and replayed edges and their citations. Ordinary
non-flagged replay edges keep `extractor = 'relink'`, and mention-detector
edges keep `extractor = 'mention-detector'`; neither is reconciliation-owned
or touched by a vault reconcile.

#### Mechanism

- The replay collection loop splits flags: a flagged row's resolved pairs go
  into the vault batch together with an `edge_provenance_entry` built from
  the stored head's coordinates; ordinary rows stay on the existing
  `relink` batch. Flagged resolution itself is unchanged from §3d/§3e.
- `create_missing_edges` takes an `EdgeReplay` owner descriptor. The vault
  batch writes `extractor = 'upsert'` and the citation's `evidence_id` on
  the edge, then stamps only the citations for pairs it actually created,
  using the same `(rel_type, extractor, resolved_target)` replace key as
  §3i. A repeat ingest that finds every edge present performs no metadata
  write.
- Vault pairs are processed before ordinary pairs, so a pair declared by both
  a flagged row and an ordinary row is owned by reconciliation.
- `reconcile_declared_edges` already deletes `extractor = 'upsert'` edges and
  retires their provenance (§3a, §3h); it needs no new extractor because the
  replay no longer mints a competing owner.

#### Eval (failed on `bc83c12b`)

- `reimport_retires_replayed_vault_declaration_edges`
  - Import `a → ns::z` while only the bare alias `z` exists, so the first
    import binds `a → z`. Seed an ordinary declarer whose target is also
    absent (control).
  - Create the literal `ns::z` and the control target through the memory API,
    then run `relink_edges(ns, "code")`. Assert replay materialises
    `a → ns::z` as `upsert` with a matching `edge_sources` citation, and the
    control edge is an ordinary `relink` edge.
  - Remove `a`'s WikiLink, reimport. Assert no `a` `RELATES_TO` edge remains
    under any extractor, no `RELATES_TO` provenance remains, and the control
    `relink` edge survives.
  - On `bc83c12b` replay created `a → ns::z` as `relink`; reconciliation only
    deletes `upsert`, so the removed link left the edge (and no citation)
    live.

#### Residual risk

- An edge already created by a pre-fix replay keeps its `relink` owner; this
  patch does not rewrite existing edges. The replay-ownership change ships in
  the same unreleased stack as the replay path itself, so no deployed
  database predates it; a row that did hit the intermediate build converges
  when its declarer changes or the edge is replayed after removal.
- Citation stamping adds one metadata read per flagged declarer only when a
  replayed edge is newly created. A repeat ingest with no missing edges
  writes neither edges nor metadata.
- The edge and citation are still two statements inside one write turn; a
  failure between them leaves the edge uncited, the same crash class as §3i.

### 3k. Replay reconciles obsolete exact bindings

#### Invariant

Replay owns the whole binding set of an `exact_edge_targets` declarer, not
only the edges it is about to create. When the preferred verbatim target
appears and replay materialises `a → ns::z`, the alias edge `a → z` bound by
the first import — and its `metadata.edge_sources` citation — is retired in
the same replay turn. The declarer then has exactly one relationship for one
declaration in `list_edges` and RDF projection. Ordinary `relink` edges and
`mention-detector` edges are not reconciliation-owned and survive. The retire
is edge- and metadata-only: no content version, `supersedes` stamp, or history
row is created.

#### Mechanism

- The replay loop records a `(declarer id, rel_type)` keep set for every
  outgoing exact-mode declaration. The entry is created before target
  resolution, so a declaration whose target is absent keeps an empty keep set
  and still retires the obsolete binding it replaced.
- The reconciler body of `reconcile_declared_edges` is extracted into a
  connection-scoped helper. Both the trait method and `relink_edges` call it,
  so replay runs the delete and provenance retire inside its existing write
  turn instead of opening a nested write. Deletion is restricted exactly as in
  §3h: outgoing edges with `extractor = 'upsert'` whose target is not in keep,
  plus `upsert`/`self_to_target` citations for that relation and target.
- Replay reconciles after creating the vault batch and before replaying the
  ordinary batch. A pair declared by both a flagged row and an ordinary row
  therefore loses its obsolete `upsert` edge first and is re-created with
  `extractor = 'relink'` for the ordinary declaration in the same pass.

#### Eval (failed on `b325c8ae`)

- `replay_reconciles_obsolete_exact_bindings`
  - Import `a → ns::z` while only the bare alias `z` exists, so the first
    import binds `a → z`. Seed an ordinary declarer whose target is also
    absent (control).
  - Create the literal `ns::z` and the control target through the memory API,
    then run `relink_edges(ns, "code")`.
  - Assert the only `a` `RELATES_TO` edge ends at `ns::z`, `a → z` is gone,
    `edge_sources` cites `ns::z` and no longer cites `z`, and the control edge
    is still an ordinary `relink` edge.
  - On `b325c8ae` replay added the preferred edge but left the alias live:
    `["ns::z", "z"]`.

#### Residual risk

- The keep set is the *current* resolved declaration set, so a temporarily
  missing exact target retires its previous binding until a later replay
  re-creates it. This is the same authoritative-declaration semantics the
  import reconciler already applies; an ingest is the repair point.
- Reconciliation covers outgoing (`self_to_target`) declarations, matching
  the existing reconciler; an incoming exact-mode declaration is not
  reconciled by this pass.
- Edge delete and provenance removal are two statements but run in one write
  turn; a failure returns from replay before the transaction commits, so it
  cannot publish a half-reconciled graph.

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
  pending-preferred-target replay (§3f), unchanged-declarer edge rebind with
  its vault-ownership/`related`-only scope (§3g), retirement export filter, history-id
  import guard, `fingerprint_version` stamp, `exact_edge_targets` stamp,
  per-head `reconcile_declared_edges` call after a successful upsert, updated
  `import_reconciles_preserved_edge_fields_with_edited_links`
  assertion (that key now exists by design), and the #363 evals.
- `src/traits.rs` — `VectorStore::reconcile_declared_edges` default no-op,
  `VectorStore::rebind_declared_edges` default no-op, and the rebind's
  `declared`/`keep` binding contract (§3g, §3i).
- `src/store/ladybug.rs` — `extractor = 'upsert'` RELATES_TO reconciler,
  `rebind_declared_edges` materialiser that stamps replacement-edge provenance
  through the shared `edge_provenance_entry` builder and replaces only the
  rebinding entry, preserving unrelated and earlier-rebind citations (§3g,
  §3i),
  `exact_edge_targets` opt-out from `KnownIds` rewriting, from
  path-inferring edge materialisation, and from `relink_edges` declaration
  replay (§3d), both using the shared verbatim-first match (§3e),
  reconciliation-owned replay of flagged declarations with matching citations
  while ordinary replay keeps `relink` and mentions keep `mention-detector`
  (§3j), replay-time reconciliation of each flagged declarer's obsolete
  bindings and citations against its current exact keep set (§3k), and
  provenance reconciliation inside the edge reconciler (§3h).
- `src/server.rs` — the `include_references` evals for retired and rebound
  citations, including retained-declaration and multiple-rebind coverage
  (§3h, §3i); no behaviour change.
- `docs/features-api/mcp-and-http-api.md` — `include_references` citation
  contract now states that provenance retires with its edge, that a rebind
  cites its replacement edge, and that a rebind preserves citations for every
  other live target.
- `src/parser/generation.rs` — `exact_target_spellings`, `exact_target_match`,
  and `exact_target_match_fallible`: the single verbatim-first alias rule and
  precedence selector shared by the vault importer and the store (§3e),
  re-exported through `parser::ingest`.
- `src/parser/ingest.rs` — re-export only; no parser behaviour change.

## 7. Risk

- Added write per linked note only when its edges were deferred; no change to
  unlinked notes.
- One extra metadata key (`related_to`, `fingerprint_version`) per imported
  head; both import-owned.
- Export file set shrinks for namespaces with history: stale files remain on
  disk (additive export), documented above.
- One extra delete-only statement per planned note after its upsert; it
  performs no page writes when the edge set already matches. Caller-scoped, so
  non-vault upserts keep their current behaviour.
- Vault-origin upserts skip path-based target resolution and use a verbatim
  namespace lookup instead; ordinary upserts, the parser, and `KnownIds` are
  untouched.
- A declaration whose preferred vault id is not stored yet costs one extra
  store read at keep-set time and one re-upsert after the write phase; only
  that declarer replays, not the whole vault.
- An unchanged declarer costs one keep-set read after the write phase and, when
  a preferred target arrived, one metadata read, one edge-create plus its
  provenance entry, and one reconcile statement. The head row is never
  rewritten, so no version or history row appears; its `edge_sources` changes
  only by swapping the retired citation for the replacement (§3h, §3i).
- Reconciliation performs one metadata read per pass and writes metadata only
  when a provenance entry was removed. Ordinary rows and other relations are
  untouched.
- Replay of a flagged declaration costs one content-hash provenance build per
  declarer and, only when the edge is newly created, one metadata read/write
  for its citation; repeat ingests with no missing edges add no writes.
- Replay now reconciles each flagged declarer against its exact keep set once
  per `(declarer, rel_type)` per ingest: one delete query and one metadata
  read, with a metadata write only when a citation was removed. Repeat ingests
  whose bindings already match perform no mutation.
- The replay reconcile follows the current resolved declaration set, not the
  historical edge set: a preferred target that is temporarily absent retires
  its old binding until a later ingest rebinds it.
- The reconcile and the head write are two statements, not one transaction;
  the order makes a failed write safe at the cost of a crash window that can
  leave stale edges with the new head (repaired on the next changed import).
