# Task 2 Report — Active-Generation Isolation & Normative Reserve in Hybrid Search & SPARQL

## Status: DONE

## Scope

### Task 2.1 — `src/store/ladybug.rs` (`search_inner`)
- Added helpers `active_generations_by_source_of`, `is_active_scoped_row`, `bare_id_of` and constants `NORMATIVE_MEMORY_TYPES` (`rule, fact, decision, procedure`), `NORMATIVE_RELEVANCE_FLOOR` (0.20), `normative_slot_limit(limit)` (`min(3, limit.div_ceil(2))`). Helpers made `pub(crate)` for cross-crate use from `sparql.rs`.
- `search_inner`: `by_source`/`active_set`/`max_code_gen` computed once at top via `active_generations_by_source_of`; active-generation filter applied in the primary loop before RRF/top-k (unscoped normative rows always pass); dedup by bare_id keeps the highest generation per logical id (unscoped rows not deduped).
- Normative slot reservation inside the hybrid mode block only (after `results = hybrid_results...take(limit)`), preserving non-hybrid behavior: when `limit >= 3`, reserve up to `normative_slot_limit(limit)` slots for normative memories with score <= 0.20, then fill the rest from non-normative results.
- Backward compat preserved: existing `search` tests use bare/unscoped ids (`rule`, `mem-1`, ...) and still pass.
- TDD test added: `search_excludes_inactive_ingest_generations_and_preserves_normative_memories` (gen-2 `code_ast` row present, superseded gen-1 excluded, unscoped normative rule/fact preserved, exactly one `src/a.rs` after dedup).

### Task 2.2 — `src/rdf/sparql.rs` (`project_sparql_dataset_with_meta_async` + `execute_sparql_query_with_config`)
- `project_sparql_dataset_with_meta_async`: active-generation filter + dedup by bare_id (keep highest generation, input order preserved); removed the double insert (every quad was previously inserted into both its named graph and the DefaultGraph, doubling `dataset.len()`); quads now live once in their named graph.
- `execute_sparql_query_with_config`: `prepared.dataset_mut().set_default_graph_as_union()` after `prepare`, so a default-graph query sees the union of all named graphs.
- Also fixed pre-existing uncompiled code left in the function: `unwrap_or_else` closure used an illegal tuple-pattern with `HashMap::new()`/`HashSet::new()` (→ `|_err|`), the `with_conn` closure captured `namespace: &str` in a `'static` requirement (→ clone into `ns_owned`), and `crate::store::ladybug::SearchResult` is not visible from `sparql.rs` (→ `crate::traits::SearchResult`).
- Unit test added: `test_project_sparql_dataset_filters_inactive_generations_and_dedupes` (gen-1 superseded row absent, gen-2 active row present, unscoped normative rule present, zero default-graph quads, `dataset.len()` == sum of per-memory quads → no duplication).

## RED → GREEN TDD Evidence

### RED (failing before implementation)
- `search_inner` never consulted ingest generations: superseded rows consumed candidate slots, the same bare_id could appear across generations, and no normative slots were reserved. The new test `search_excludes_inactive_ingest_generations_and_preserves_normative_memories` could not pass (no filter, no dedup, no reservation).
- `project_sparql_dataset_with_meta_async` inserted each quad twice (named graph + DefaultGraph) and performed no generation filtering or dedup. The new test `test_project_sparql_dataset_filters_inactive_generations_and_dedupes` failed on the old code (double insert, no inactive-gen filter). The function also did not compile (`unwrap_or_else` tuple-pattern, non-`'static` closure capture, invisible `SearchResult` type).

### GREEN (after implementation)
- `search_excludes_inactive_ingest_generations_and_preserves_normative_memories` ✓ — gen-2 row present, gen-1 absent, normative rule/fact present, exactly one `src/a.rs`.
- `test_project_sparql_dataset_filters_inactive_generations_and_dedupes` ✓ — inactive gen filtered, one bare_id version kept, no DefaultGraph quads, no duplicated quads.
- `test_execute_sparql_query_live_projection` ✓ — default-graph union query returns projected triples (validates `set_default_graph_as_union()` wiring).

## Full Suite Results

| Suite | Tests | Pass | Fail |
|---|---|---|---|
| `store::ladybug::tests` | 142 | 142 | 0 |
| `rdf::sparql::tests` | 11 | 11 | 0 |

## Files Changed
- `src/store/ladybug.rs` — Task 2.1: helpers + `search_inner` filter/dedup/normative-reservation + TDD test.
- `src/rdf/sparql.rs` — Task 2.2: clean dedup block, removed double DefaultGraph insert, `set_default_graph_as_union()` wiring, fixed pre-existing compile errors, unit test.
- `.superpowers/sdd/2026-11-07-autonomous-memory-control-plane/task-2-report.md` (this report)

## Task State
- `neurostrata-a1si` (namespace `NeuroStrata`): left `in_progress` (not completed).
- `kenton-ui0c` (namespace `kenton`): left `in_progress` (not completed).
- Commit pending on `opencode/v116-autonomous-memory-control-plane` (not pushed).
