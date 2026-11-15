# PAR-02: consistent visibility at retrieval merge and hydration

Baseline: `parity/lexical-acl-contract` @ `8f488fd5`, on original/main
`7b8c2b985f83d74143f0d4ba45bf389571f3f166`.
Reference: fork origin/develop `3d693fd96c5fcad07d3f740584203e05975b53ce`
(`LadybugStore::lexical_apply_doc`, `search_tantivy_lexical`, inferred merge in
`search_inner`). Scope: store retrieval merge, Tantivy hydration and lexical
upsert flag preservation. No API, schema or dependency change.

## Acceptance contract (specified before test/production edits)

Synthetic memories use an isolated temporary LadybugDB and four fake vector
dimensions; no embeddings, daemon, global database or user services are needed.
Each test exercises the real store method, not a reimplementation.

| Criterion | Expected behavior | Test |
| --- | --- | --- |
| B1 registry/conflict hydration | A stale Tantivy entry whose stored row is a registry row or mesh-conflicting row is not returned by `search_tantivy_lexical`, even when the index entry carries no exclusion metadata. A clean row still surfaces. | `par02_tantivy_hydration_excludes_registry_and_conflicting_rows` |
| B2 retirement hydration | A stored row retired under `is_retired_at` semantics (`valid_to: 0`, or unparseable non-null `valid_to`) never surfaces from Tantivy hydration. A clean row surfaces. The same rows stay excluded by the vector/domain control path. | `par02_tantivy_hydration_excludes_retired_rows` |
| B3 archive hydration | A stored archived row is excluded from Tantivy hydration by default and admitted when `include_archived=true`. | `par02_tantivy_hydration_respects_archive_escape_hatch` |
| B4 canonical owner/ACL hydration | Hydration derives visibility with `rebac::can_read_memory`: an empty payload `user_id` with metadata `user_id: alice` is alice's row; alice is admitted, bob is denied. Object-form `visible_to` entries are honoured. | `par02_tantivy_hydration_uses_metadata_owner_for_acl` |
| B5 inferred merge predicates | Rows surfaced only through the reasoning-inferred merge apply the same predicates as the vector passes: retired, archived, mesh-conflicting, registry, task (unless `include_tasks`), structural and out-of-domain rows do not re-enter. `include_tasks`/`include_archived` remain honoured, and a clean row still surfaces. | `par02_inferred_merge_excludes_rows_hidden_from_vector_passes` |
| B6 upsert flag preservation | `lexical_apply_doc` preserves the registry and mesh-conflict flags decided by `lexical_doc`, so `TantivyLexicalIndex::upsert_memory` excludes them at index time instead of indexing rows the rebuild path excludes. | `par02_tantivy_upsert_path_excludes_registry_and_conflicting_rows` |
| B7 preserved behavior | Valid rows keep documented fallback behavior: domain filter falls back to unfiltered candidates when it selects none, ordering/ranking unchanged, incremental upsert and explicit rebuild paths still index clean rows, and `include_tasks`/`include_archived` still admit their classes in hydration. | Existing `tantivy_view_converges_with_inhouse_index`, `search_stable_tie_order`, lexical suite; B5 escape-hatch assertions |

## Intentional behavior change and preserved fork behavior

Before this change, Tantivy hydration trusted the stale index entry for
archive/registry/conflict/retirement and used the raw payload owner for ACL, so
a stored row hidden by the vector passes could resurface through the secondary
lexical route. The inferred merge admitted any hydrated reasoning row, so a
query equal to a hidden memory id could return it. Both paths must apply the
same current-state predicates the vector passes apply.

`lexical_apply_doc` reconstructs a `MemoryPayload` from `LexicalDoc` before
indexing; it must carry the registry/conflict flags forward or `upsert_memory`
cannot exclude them.

Preserve documented behavior: metadata-derived owner fallback, `admin`
override, object/string ACL semantics, domain-filter fallback, task and archive
escape hatches, deterministic ordering and ranking. Do not port unrelated fork
features (mention cache scoping is PAR-15, literal code-token search is
PAR-10, namespace identity is PAR-15).

TDD order: this tracked spec → regression tests with unchanged production
code → record assertion RED (build failures do not count) → minimal predicate
port → execute GREEN plus focused regressions. Evidence logs go to the external
PAR-02 implementation evidence directory.
