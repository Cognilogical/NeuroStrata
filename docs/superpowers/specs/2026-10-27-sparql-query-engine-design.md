# SPARQL 1.1/1.2 Query Engine Design

> Status: **APPROVED** (2026-10-28).
> Task: `neurostrata-damg` (SPARQL 1.1/1.2 query engine over memory graph, OxiRS §1.4).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-27-owl-rdfs-reasoning-design.md`.

## 1. Problem

NeuroStrata's retrieval paths are purpose-built Rust traversals over LadybugDB. They are fast and evidence-backed, but external clients and advanced users cannot ask ad hoc graph questions. A SPARQL surface would let clients query the projected RDF graph without replacing the existing ranked retrieval, neural-gain scoring, or lifecycle filtering.

## 2. Goal

Add a **read-only, local-first SPARQL 1.1/1.2 query tool** over the RDF projection of LadybugDB:

- parse and evaluate SPARQL against a transient projected dataset,
- enforce namespace and row-visibility filters before evaluation,
- register NeuroStrata-specific functions such as `ns:salience`,
- budget query time, solution count, and memory use,
- return deterministic JSON/CSV/TSV results,
- never mutate LadybugDB.

## 3. Constraints

- LadybugDB remains the sole store.
- SPARQL is augmentation, not a replacement for `search_memory`, `related`, or `memories_for_file`.
- Read-only: no SPARQL UPDATE, no `SERVICE`, no federation in the default mode.
- No LLM in parsing, planning, or evaluation.
- Namespace-scoped: queries may only see rows from the requested namespace unless explicitly authorized later.
- Visibility filters (`retired`, `archived`, `mesh-conflicting`, `registry`) apply before SPARQL evaluation; `include_archived` and `include_retired` match existing MCP tools (`export_graph`, `task_list`). Mesh-conflicting and registry rows are never served even under `include_archived: true`.
- Deterministic output: stable serialization and bounded results.
- Warn-first: malformed or unsupported queries return structured errors, not panics.

## 3.1 Library evaluation

Prefer the Oxigraph stack over an in-house SPARQL engine:

- `spargebra` for parsing SPARQL 1.1/1.2 queries and updates.
- `sparopt` for algebra optimization.
- `spareval` for evaluating prepared queries over an `oxrdf` dataset, including custom functions and cancellation.
- `sparesults` for serializing SPARQL result sets.
- `oxirs-arq` is not used as a dependency or lift-and-shift candidate; decoupling it from `oxirs-core` would require rewriting its AST and term bindings onto `oxrdf`.
- Evaluate the Oxigraph stack against the required SPARQL subset, budgets, and custom functions before implementation.
- Avoid direct `oxirs-core` dependencies due to ctor pre-main rustls hooks and dependency bloat.
- Do not implement a SPARQL parser or evaluator in-house unless no candidate passes.

## 4. Scope

In scope:

- SPARQL 1.1 Query (SELECT, CONSTRUCT, ASK, DESCRIBE) and read-only SPARQL 1.2 features needed by clients.
- Transient in-memory dataset built from the namespace projection.
- Namespace and visibility filtering.
- Custom functions: `ns:salience`, `ns:validAt`, and `ns:isRetired`.
- Query budgets and result caps.
- Structured errors and result serialization.

Out of scope:

- SPARQL UPDATE.
- `SERVICE` and federation (separate spec).
- Replacing vector/lexical/graph retrieval.
- Persistent SPARQL endpoints or HTTP server mode in this spec.
- GeoSPARQL unless separately approved.

## 5. Approach

### 5.1 Query lifecycle

1. Parse the query with `spargebra`.
2. Reject UPDATE and `SERVICE` unless explicitly enabled by configuration.
3. Optimize the algebra with `sparopt`.
4. Build a transient `oxrdf::Dataset` from the namespace projection.
5. Apply visibility and namespace filters during projection, not after evaluation.
6. Prepare and execute with `spareval`.
7. Convert results to the requested format.
8. Drop the transient dataset.

### 5.2 Projection

Use union-default-graph semantics: every projected quad is added both to its named tier graph and to the default graph, or configure `spareval` to treat the default graph as the union of all named graphs. Otherwise ad hoc `SELECT` queries without `GRAPH` return no results.

The projection must include:

- memory nodes and metadata predicates,
- relation edges as typed predicates, with endpoint IRIs kept as `urn:neurostrata:memory:<id>` (primary subject IRIs) so joins work; `ns:conceptIri` is emitted as a separate predicate for cross-concept alignment, but edge endpoints are NOT rewritten to `concept_iri`,
- ontology classes and properties needed by OWL/RDFS reasoning,
- named graphs for namespaces and tiers,
- `ns:validFrom`, `ns:validTo`, `ns:archived`, `ns:accessCount`, and `rdf:type` predicates for every memory node.

Projection excludes retired, archived, mesh-conflicting, and registry rows. `include_retired` and `include_archived` flags (matching `export_graph` and `task_list`) control retired/archived inclusion. Mesh-conflicting and registry rows are never served. Local `neurostrata_sparql_query` includes `eligibility: "private"` (or unset) rows in the target namespace by default, matching `search_memory`/`list`/`export_graph`; any future remote/federated entrypoint excludes non-shareable rows.

An edge `(from, rel, to)` is projected **if and only if** both `from` and `to` are present in the post-filter set of visible node IDs for the query (excluding superseded generations, mesh-conflicting rows, registry rows, and—unless enabled by flags—retired and archived rows). No dangling edges to hidden/conflicting nodes are emitted.

Named graph IRI derivation (per row): `urn:neurostrata:graph:<namespace>:<tier>:gen:<generation>`, where tier is `Global` if `namespace == "global"`, `Task` if `memory_type == "task"`, else `Domain`; generation is extracted via `split_scoped_id(&id)` or `0` for unscoped rows. `project_sparql_dataset(namespace, include_retired, include_archived)` derives tier/generation per row (the single `(tier, generation)` parameter on `project_namespace_rdf` is not used for SPARQL projection).

### 5.3 Custom functions

Register NeuroStrata functions with the evaluator. They read only projected metadata via an `Arc<HashMap<NamedNode, RowMeta>>` captured at preparation time; no LadybugDB access during evaluation.

- `ns:salience(?memory)` returns `xsd:double`:
  - `access_count` clamped to `>= 0` before `ln(1 + n)`,
  - `source_gain` = `0.05` (`SOURCE_RANK_GAIN`) when `crate::traits::has_source(&metadata)` is true else `0.0`,
  - `feedback_gain` = `0.05 * feedback_score.clamp(-1.0, 1.0)` is **included**,
  - formula: `0.05 * ln(1 + access_count_clamped) + source_gain + feedback_gain`.
  - If `?memory` is not a known memory IRI, return `None` (SPARQL evaluation error), never panic.
- `ns:validAt(?memory, ?timestamp)` returns `xsd:boolean`. Accepted timestamp literal types: `xsd:integer` (Unix seconds) and `xsd:dateTime`. The RDF datatypes emitted for `ns:validFrom`/`ns:validTo` in `src/rdf/projection.rs` are `xsd:integer` Unix seconds. Half-open interval check: `valid_from <= t && (valid_to.is_none() || valid_to == 0 || t < valid_to)`, matching `is_retired_at` in `ladybug.rs`. If `?memory` unknown, return `None`.
- `ns:isRetired(?memory)` returns `xsd:boolean` using the same `is_retired_at` logic with a single query-start timestamp `now` captured once per query (used for both visibility filtering and `ns:isRetired`). If `?memory` unknown, return `None`.

### 5.4 Budgets

Configuration:

- `sparql_enabled: bool` (default true)
- `sparql_timeout_ms: u64` (default 250)
- `sparql_max_solutions: usize` (default 1000)
- `sparql_max_dataset_quads: usize` (default 250000)
- `sparql_max_algebra_nodes: usize` (default 1000)
- `sparql_service_enabled: bool` (default false)

If a budget is exceeded, return the §5.5 envelope with `"timed_out": true` and the solutions collected up to cancellation (`solution_count: 0` if cancelled before the first solution); `truncated` remains false unless the solution cap was hit. Memory is enforced through structural caps (`sparql_max_dataset_quads`, `sparql_max_algebra_nodes`) and streaming `Iterator::take(sparql_max_solutions)`, not through a global allocator. There is no separate "mode" parameter; this contract is uniform.

### 5.5 Result formats

Support JSON, CSV, and TSV output. Results are sorted according to SPARQL result ordering; if no ORDER BY is present, apply a deterministic tie-break by comparing bound RDF term values across the lexicographically ordered projected variables for `SELECT`, and by `(subject, predicate, object)` for `CONSTRUCT`/`DESCRIBE`. Quad insertion into `oxrdf::Dataset` must be deterministic (sorted by `(s,p,o,g)`). `sparql_max_solutions` interacts with sorting as follows: when no ORDER BY is present, stream and truncate after the deterministic tie-break ordering has been established over the full result set (i.e., sort first, then take); when ORDER BY is present, apply ORDER BY then take.

Query-form to format mapping:
- `SELECT`: serialized via `sparesults` (JSON/CSV/TSV bindings).
- `ASK`: JSON returns `{ "boolean": bool }`; CSV/TSV return a single-column `result` header with `true`/`false`.
- `CONSTRUCT`/`DESCRIBE`: produce an RDF graph; for JSON/CSV/TSV, project triples into synthetic bindings `(?subject, ?predicate, ?object)` (one row per triple, sorted by `(s,p,o)`). `ntriples`/`turtle` may be added later as separate formats but are not in this spec.

The MCP return envelope is:

```json
{
  "query_form": "select" | "ask" | "construct" | "describe",
  "format": "json" | "csv" | "tsv",
  "results": "<serialized>",
  "solution_count": 0,
  "truncated": false,
  "timed_out": false
}
```

`results` is the serialized output for the chosen format. `truncated` is set when the solution cap was hit; `timed_out` when the timeout was hit.

### 5.6 Security

- No filesystem or network access from SPARQL.
- No custom functions beyond the registered NeuroStrata set.
- Query depth and algebra size limits prevent pathological queries.
- `SERVICE` disabled by default.

## 6. Interfaces

- Tool:
  - `neurostrata_sparql_query(query, format?, namespace?, include_retired?, include_archived?)`
- Store/projection:
  - `project_sparql_dataset(namespace, include_retired, include_archived) -> oxrdf::Dataset`
  - `list_namespace_edges(namespace) -> Vec<(MemoryId, Relation, MemoryId)>` for edge validation
- Configuration as listed in §5.4.

Pre-registered prefix table (available without explicit PREFIX):

| Prefix | IRI |
|---|---|
| `ns:` | `urn:neurostrata:ns#` |
| `mem:` | `urn:neurostrata:memory:` |
| `class:` | `urn:neurostrata:class:` |
| `dom:` | `urn:neurostrata:domain:` |
| `rdf:` | `http://www.w3.org/1999/02/22-rdf-syntax-ns#` |
| `rdfs:` | `http://www.w3.org/2000/01/rdf-schema#` |
| `owl:` | `http://www.w3.org/2002/07/owl#` |
| `skos:` | `http://www.w3.org/2004/02/skos/core#` |
| `xsd:` | `http://www.w3.org/2001/XMLSchema#` |

When enabled, the projection also includes an ontology graph (`urn:neurostrata:graph:<ns>:ontology`) containing the `OntologyIndex` triples (subClassOf, subPropertyOf, altLabel) for SPARQL-based domain reasoning.

## 7. Edge cases

- Empty namespace: empty dataset, valid query, empty results.
- Unsupported feature: structured error with feature name.
- Timeout: return the §5.5 envelope with `"timed_out": true` and solutions collected up to cancellation.
- Query references unknown prefix: parse error.
- Query asks for retired rows without audit mode: no results for those rows.
- Large result set: truncated at `sparql_max_solutions`.

## 8. Testing

- Unit: parse representative SPARQL 1.1/1.2 queries.
- Unit: custom functions return expected values.
- Integration: query returns projected memories and edges.
- Integration: retired rows are excluded by default.
- Performance: queries stay within configured budgets.
- Security: UPDATE and SERVICE are rejected by default.

## 9. Acceptance

A read-only SPARQL query tool is available over the projected memory graph: queries are parsed, optimized, and evaluated with budgets; visibility and namespace filters apply; custom NeuroStrata functions work; LadybugDB remains the sole store and no LLM is used.

## 10. Files touched (expected)

- `src/rdf/sparql.rs` (new): adapter, projection, evaluation, and result conversion.
- `src/server.rs`: SPARQL tool.
- `src/rdf/projection.rs`: ensure projection covers all queryable predicates.
- `neurostrata-damg` (SPARQL 1.1/1.2 query tool).
- `Cargo.toml`: `spargebra`, `sparopt`, `spareval`, `sparesults` with `rdf-12`/`sparql-12` feature flags enabled on `oxrdf`, `spargebra`, `spareval`, `sparesults` (unified version train).