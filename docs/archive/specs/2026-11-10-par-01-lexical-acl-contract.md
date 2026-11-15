# PAR-01: canonical in-house lexical ACL contract

Baseline: original/main `7b8c2b985f83d74143f0d4ba45bf389571f3f166`.
Reference: fork origin/develop `3d693fd96c5fcad07d3f740584203e05975b53ce`,
ACL fix located by `1612b59`. Scope: `LadybugStore::lexical_doc` only.

## Acceptance contract (specified before test/production edits)

Synthetic memories use an isolated temporary LadybugDB and four fake vector
dimensions; no embeddings, daemon, global database, or user services are needed.
Search exercises the real store's in-house lexical route and hydrated results.

| Criterion | Expected behavior | Test |
| --- | --- | --- |
| A1 object ACL | `{principal:"alice",role:"viewer"}` grants alice read; owner/admin retain read; bob and anonymous receive no hit. Allowed retrieval is asserted before denial. | `par01_lexical_object_acl_denies_unauthorized` |
| A2 roles and strings | Object viewer/editor/owner roles all permit the named reader. Missing/unknown object roles retain canonical viewer fallback. String ACLs retain exact principal matching, with no substring match. | `par01_lexical_acl_compatibility_matrix` |
| A3 mixed ACL | String/object entries both grant named readers; malformed companions are ignored without discarding valid grants. Unlisted/anonymous readers are denied. | `par01_lexical_acl_compatibility_matrix` |
| A4 malformed nonempty array | Arrays containing only invalid entries (numbers, null, missing/nonstring principal) use canonical fail-closed semantics: no ordinary reader; owner/admin still read. | `par01_lexical_acl_compatibility_matrix` |
| A5 wildcard | String/object `*` grants any nonempty principal; anonymous stays denied. | `par01_lexical_acl_compatibility_matrix` |
| A6 backward compatibility | Missing/empty ACL permits nonempty readers; anonymous stays denied. Canonical legacy null/nonarray metadata behavior is unchanged. | `par01_lexical_acl_compatibility_matrix` |
| A7 index lifecycle | A1–A6 hold after incremental upsert, explicit rebuild, and cold load from the same temporary DB. | Both PAR-01 tests (matrix covers all lifecycle stages) |
| A8 retrieval compatibility | Matching remains positive for allowed users; symbol ranking, namespace isolation, domain and row-class filters, deterministic ordering, and lexical mutations remain intact. | Existing `lexical_search_ranks_symbols_filters_namespaces_and_rebuilds`, `lexical_mutations_keep_a_loaded_index_in_step`, and `lexical_test` suite |
| A9 stable surfaces | APIs, schemas, dependencies, Tantivy hydration, vector search, graph traversal, and parser semantics remain unchanged. | Minimal production diff review; existing ReBAC tests; matrix also checks the existing Tantivy hydration route |

## Intentional behavior change and preserved fork behavior

Object-only ACLs and wholly malformed nonempty ACL arrays must no longer become
unrestricted in-house lexical documents. Mixed object/string ACLs must retain
object principals instead of losing their valid grants. Use the existing
`rebac::extract_visible_to` contract, including its invalid-array sentinel;
this work does not redesign validation or reserve new principal names.

The pinned fork already implements this canonical extraction. Preserve its
object/string/mixed visibility, wildcard, owner/admin and anonymous rules,
fail-closed malformed-array handling, validity/archive/domain/row-class filters,
and lexical indexing/ranking. Port only the extraction change: do not replace
the module or alter the innovation branch, its capacity profiles, query quoting,
staged ingestion/carry behavior, or other fork features. Where practical, run the
same tests against extracted pinned fork source outside its checkout; distinguish
source evidence from executed results.

TDD order: this tracked spec → regression tests with unchanged production code →
record assertion RED (build failures do not count) → minimal canonical-parser fix
→ execute GREEN plus focused regressions. Record exact commands and limitations
in the external PAR-01 implementation evidence directory.
