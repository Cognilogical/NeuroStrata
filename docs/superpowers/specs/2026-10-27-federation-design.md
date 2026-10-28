# Federation Design

> Status: draft — pending Argon review.
> Task: `neurostrata-k8tc` (Federation (SERVICE) across daemon instances, OxiRS §1.11).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-27-sparql-query-engine-design.md`.

## 1. Problem

NeuroStrata daemons each own a local LadybugDB + projected RDF graph. There is no mechanism to query or exchange memories across instances. The OxiRS §1.11 spec requires federation / cross-instance queries via `SERVICE` clauses, but implementing a full persistent federation layer risks the `oxirs-core` / `oxirs-arq` bloat identified in prior adjudication (ctor pre-main rustls hooks, `scirs2` dependency bloat). The approach must be read-only, transient, and compatible with the single-store / no-LLM constraint.

## 2. Goal

Add **read-only SPARQL federation** across NeuroStrata daemon instances using `SERVICE` clauses, scoped to existing P1/P2 capabilities:

- `SERVICE <uri> { ... }` queries against other running NeuroStrata daemons,
- queries never materialize inferred edges in LadybugDB; all inference remains transient and evidence-backed,
- namespace-filtered: a daemon only serves namespaces it serves; cross-namespace queries are denied unless authorization is explicitly configured,
- visibility filters (`retired=0` and `archived=0`) apply on the remote side and are honored by the query planner,
- deterministic output: stable result ordering, bounded solution counts,
- never embed an LLM in the query path.

## 3. Constraints

- LadybugDB remains the sole store; federation is entirely transient / read-only.
- No materialized inferred edges; all `SERVICE` results are transient.
- Namespace isolation: a daemon only serves namespaces it is configured to expose; cross-daemon namespace access is denied unless the remote is explicitly configured to serve them.
- Row exclusion: `eligibility != "shareable"` (private), `structural`, and `task` rows are never forwarded or served remotely; audit flags (`include_retired`, `include_archived`) are denied on remote `/sparql`.
- Warn-first: malformed or unsupported queries return structured errors, not panics.
- SPARQL `SERVICE` support; `UPDATE` / `DELETE` are rejected.
- Zero LLM usage in federation path.
- Library evaluation: prefer existing OxiRS federation crates over in-house HTTP+SPARQL client code; avoid direct `oxirs-core` dependency if possible.

## 3.1 Library evaluation

- `oxirs-federate` is rejected: it hard-depends on `oxirs-core`, `oxirs-arq`, `oxirs-vec`, and `scirs2-core` (which pulls `ctor = "1.0"` pre-main rustls hooks), and its planner operates over `oxirs-arq` ASTs rather than `spargebra`, so lift-and-shift would require rewriting the crate.
- Use `spareval::ServiceHandler` (or `DefaultServiceHandler`) backed by `reqwest` and `sparesults`: `spareval` already evaluates `GraphPattern::Service` natively inside `SimpleEvaluator` and exposes a handler trait for remote service resolution.
- `reqwest` for HTTP POST of SPARQL queries to remote endpoints.
- `sparesults` for parsing W3C `application/sparql-results+json` responses.
- `spargebra` for formatting the inner query expression passed to the handler.
- Do not implement SERVICE federation in-house unless `spareval::ServiceHandler` cannot meet the requirements.

## 3.2 Projection

- `project_sparql_dataset(namespace, include_retired, include_archived) -> oxrdf::Dataset` — local projection.
- `project_federated_sparql_dataset(namespace) -> oxrdf::Dataset` — remote-side projection that excludes private (`eligibility != "shareable"`), `structural`, `task`, `retired`, `archived`, mesh-conflicting, and registry rows. Filtering happens at the remote daemon during projection, never post-hoc on the caller.

## 4. Scope

In scope:

- SERVICE clause parsing and forwarding over HTTP(S) to remote NeuroStrata daemons.
- Transient dataset building from remote projection; results are not persisted.
- Namespace filtering: only namespaces explicitly served by the remote daemon are queryable.
- Visibility enforcement: `retired=0` and `archived=0` required; mesh-conflicting and registry always excluded; `eligibility: "shareable"` required for mesh-shared rows.
- Deterministic result serialization (JSON/CSV/TSV).
- Custom NeuroStrata functions (`ns:salience`, etc.) are local-only and are rejected on the remote; remote endpoints only register `ns:salience` if explicitly enabled.
- Executes the query with `spareval` over the transient dataset.
- Returns W3C `application/sparql-results+json` (via `sparesults::QueryResultsSerializer`), NOT the MCP envelope — the caller unwraps the MCP envelope locally and feeds `results` to `sparesults` for parsing.

## 5. Approach

### 5.1 SERVICE forwarding

1. Parse the SPARQL query with `spargebra`, detecting `GraphPattern::Service` (SERVICE) clauses.
2. For each SERVICE pattern:
   a. Resolve the endpoint URI against `sparql_service_peers` (mapping to `(host:port, remote_namespace, bearer_token_env)`).
   b. Format the inner query expression from the `spargebra` AST and pass it to a registered `spareval::ServiceHandler` backed by `reqwest`.
   c. Execute the inner query locally via `spareval::QueryEvaluator::prepare(...).execute(...)` with the handler; `spareval` calls the handler to fetch remote results and merge them.
   d. Apply namespace and visibility filters on the remote results before merging; remote `include_retired` and `include_archived` flags are denied. Only rows with `retired=0` and `archived=0` are forwarded; `eligibility: "shareable"` is required for mesh-shared rows.
3. Merge all SERVICE results with local results, applying the same budgets and deterministic tie-breaks. If any SERVICE call exceeds its budget, return partial results with `truncated: true` and a structured warning.

If a SERVICE pattern references an unconfigured peer or unauthorized namespace, return a structured `QueryEvaluationError::Denied` (not an empty result set).

### 5.1a Sync/async execution bridge

`spareval::ServiceHandler::handle` is synchronous (`Fn(...) -> Result<...>`). Execute it inside `tokio::task::spawn_blocking(...)` (or `Handle::current().block_in_place(...)`) so `spareval` evaluation does not deadlock the async `axum` worker thread.

### 5.2 Remote endpoint

Each NeuroStrata daemon exposes a `GET /sparql` (or `POST /sparql`) handler that:

- Parses the query with `spargebra` + `sparopt`.
- Projects the requested namespace using `project_federated_sparql_dataset(namespace)` — which excludes private (`eligibility != "shareable"`), `structural`, `task`, `retired`, `archived`, mesh-conflicting, and registry rows. Filtering happens at the remote daemon during projection, never post-hoc on the caller.
- Applies namespace and visibility filters.
- Does NOT register `ns:salience` or other custom NeuroStrata functions — custom functions are local-only and are rejected on the remote.
- Executes the query with `spareval` over the transient dataset.
- Returns W3C `application/sparql-results+json` (via `sparesults::QueryResultsSerializer`), NOT the MCP envelope — the caller unwraps the MCP envelope locally and feeds `results` to `sparesults` for parsing.

### 5.3 Query budgets and capping

- `sparql_service_enabled: bool` (default false) — must be explicitly enabled per daemon.
- `sparql_timeout_ms: u64` (default 250)
- `sparql_max_solutions: usize` (default 1000) — applies per-SERVICE call.
- `sparql_max_dataset_quads: usize` (default 250000) — applied during projection on both sides.
- If a budget is exceeded, return partial results with `truncated: true` and a structured warning.

### 5.4 Security

- No `SERVICE` by default; must be opt-in via `sparql_service_enabled: true`.
- Each daemon only serves namespaces it explicitly configures; cross-daemon namespace access is denied unless the remote is explicitly configured to serve them.
- `SERVICE` over untrusted hosts is sandboxed; query complexity limits prevent pathological requests.
- Each daemon requires `Authorization: Bearer <daemon.token>` and validates `Host` header for non-loopback peers (`src/daemon.rs:202-233`); remote `/sparql` is blocked for loopback-only hosts without a peer-token allowlist.
- No network access beyond the configured endpoint URL.

## 6. Interfaces

- Tool:
  - `neurostrata_sparql_query(query, format?, namespace?, include_retired?, include_archived?)`
- Configuration:
  - `sparql_service_enabled: bool` (default false)
  - `sparql_service_peers: Vec<(String, String, String)>` — list of `(endpoint_url, remote_namespace, bearer_token_env)` triplets this daemon trusts for federation.
  - `sparql_timeout_ms: u64` (default 250)
  - `sparql_max_solutions: usize` (default 1000)
  - `sparql_max_dataset_quads: usize` (default 250000)
- Store/projection:
  - `project_sparql_dataset(namespace, include_retired, include_archived) -> oxrdf::Dataset`
  - `project_federated_sparql_dataset(namespace) -> oxrdf::Dataset`
- Custom NeuroStrata functions (`ns:salience`, etc.) are local-only and are rejected on the remote; remote endpoints only register `ns:salience` if explicitly enabled.

## 7. Edge cases

- No enabled peers: `SERVICE` returns a structured `Denied` error.
- Peer is unreachable: structured error with connection details; does not affect local query results.
- Peer serves different namespace: structured `Denied` error; local results unaffected.
- Peer query times out: partial results or timeout error, depending on configuration.
- Query references unknown prefix: parse error, same as local.
- Nested SERVICE: rejected; only top-level SERVICE clauses are supported.

## 8. Testing

- Unit: parse SERVICE patterns from representative SPARQL queries.
- Integration: `SERVICE` query against a running daemon returns projected memories.
- Integration: namespace filtering works; only served namespaces are queryable.
- Integration: visibility filters apply on remote side.
- Performance: queries stay within configured budgets.
- Security: `SERVICE` disabled by default; enabling it only exposes configured peers.

## 9. Acceptance

Read-only SPARQL federation is available via `SERVICE` clauses: queries forward to configured remote daemons, namespace and visibility filters apply; results are deterministic and bounded; LadybugDB remains the sole store and no LLM is used.

## 10. Files touched (expected)

- `src/rdf/federation.rs` (new): SERVICE forwarding (`spareval::ServiceHandler` adapter), remote client (`reqwest`), result merging.
- `src/daemon.rs`: `GET /sparql` handler, `sparql_service_enabled` config, auth middleware (`bearer_token_env`).
- `src/rdf/projection.rs`: add `project_federated_sparql_dataset`; ensure projection covers all federation queryable predicates.
- `Cargo.toml`: `reqwest`, `sparesults`, `spargebra` (parser), `spareval` (evaluator with ServiceHandler), `oxrdf` (dataset). No `oxirs-federate`; no `oxirs-core`.
