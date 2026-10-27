# SPARQL 1.1/1.2 Query Engine Design

> Status: draft — pending Argon review.
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
- Visibility filters (retired, archived, mesh-conflicting, registry) apply before SPARQL evaluation.
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
- relation edges as typed predicates, with endpoint IRIs resolved through each endpoint row's `concept_iri` when present,
- ontology classes and properties needed by OWL/RDFS reasoning,
- named graphs for namespaces and tiers.
- `ns:validFrom`, `ns:validTo`, `ns:archived`, `ns:accessCount`, and `rdf:type` predicates for every memory node.

Projection excludes retired, archived, mesh-conflicting, and registry rows unless the caller has audit authorization. Private rows are excluded from mesh and remote use; local audit mode may include them explicitly.

### 5.3 Custom functions

Register NeuroStrata functions with the evaluator:

- `ns:salience(?memory)` returns the memory's standalone neural gain: `0.05 * ln(1 + access_count) + source_gain`, as `xsd:double`. It does not use query-time vector distance.
- `ns:validAt(?memory, ?timestamp)` returns whether the memory is valid at the timestamp.
- `ns:isRetired(?memory)` returns whether the row is retired.

These functions must read only the projected metadata and must not query LadybugDB during evaluation. At evaluator preparation time, capture an `Arc<HashMap<NamedNode, RowMeta>>` in each closure so term arguments can be resolved to row metadata without store access.

### 5.4 Budgets

Configuration:

- `sparql_enabled: bool` (default true)
- `sparql_timeout_ms: u64` (default 250)
- `sparql_max_solutions: usize` (default 1000)
- `sparql_max_memory_mb: usize` (default 64)
- `sparql_service_enabled: bool` (default false)

If a budget is exceeded, return a structured partial result or error with the number of solutions produced so far.

### 5.5 Result formats

Support JSON, CSV, and TSV output. Results are sorted according to SPARQL result ordering; if no ORDER BY is present, apply a deterministic tie-break by subject, predicate, object, and graph.

### 5.6 Security

- No filesystem or network access from SPARQL.
- No custom functions beyond the registered NeuroStrata set.
- Query depth and algebra size limits prevent pathological queries.
- `SERVICE` disabled by default.

## 6. Interfaces

- Tool:
  - `neurostrata_sparql_query(query, format?, namespace?, audit?)`
- Store/projection:
  - `project_sparql_dataset(namespace, audit) -> oxrdf::Dataset`
- Configuration as listed in §5.4.

## 7. Edge cases

- Empty namespace: empty dataset, valid query, empty results.
- Unsupported feature: structured error with feature name.
- Timeout: partial results or timeout error depending on mode.
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
- `Cargo.toml`: `spargebra`, `sparopt`, `spareval`, `sparesults`.