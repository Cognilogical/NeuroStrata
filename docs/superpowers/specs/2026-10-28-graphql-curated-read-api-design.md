# GraphQL Curated Read API Design

> Status: revised per Argon review — pending re-review.
> Task: `neurostrata-ect5` (GraphQL curated read API, OxiRS §1.13).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`.

## 1. Problem

`src/graphql/mod.rs` is a stub: `execute(query)` echoes the query with `"data": null`; `schema_from_vocabulary` auto-generates a flat `{ name, type: "String" }` list from arbitrary vocab slices. This violates the Prime Directive (no stubs) and contradicts the "curated, versioned, no auto-generated unstable schema" goal. OxiRS §1.13 requires a curated read API; this spec defines it.

## 2. Goal

Add a **curated, versioned, read-only GraphQL API** over the memory graph:

- Fixed SDL (`v1`), hand-authored, not generated from vocabulary.
- Pagination (cursor-based, Relay-style) with hard caps.
- Namespace scoping, privacy, and tombstone filtering.
- Read-only (mutations rejected).
- Standard GraphQL response shape (`{ "data": ..., "errors": [...] }`).

## 3. Constraints

- LadybugDB remains the sole store.
- No auto-generated schema from vocabulary; schema is a versioned constant.
- Read-only: any mutation operation returns a GraphQL error.
- No LLM.
- Schema version tied to `memory-vocabulary` version (currently v3 / vocabulary_version 10); schema SDL lives at `src/schemas/graphql/schema.v1.graphql` (embedded via `include_str!`).

## 4. Scope

In scope:
- Curated SDL `v1` with `Memory`, `Edge`, `MemoryConnection`, `PageInfo`.
- `execute(store, namespace, principal, query, variables) -> GraphQLResponse`.
- Pagination (`first`/`after`, max 100).
- Filtering (`memory_type`, `include_archived`, `include_retired`).
- Namespace isolation and privacy (ReBAC).
- Transport: daemon HTTP (`POST /graphql`) and MCP tool `neurostrata_graphql_query`.

Out of scope:
- Subscriptions.
- Mutations (rejected).
- Auto-schema generation.

## 5. Approach

### 5.1 Library
Add `graphql-parser = "0.4"` to `Cargo.toml` for AST parsing + validation. Execution is in-house over `LadybugStore` (no async runtime in core). `schema_from_vocabulary` is deleted from `src/graphql/mod.rs`; the schema is the curated SDL constant. No `async-graphql`.

### 5.2 Schema SDL (`src/schemas/graphql/schema.v1.graphql`)
```graphql
type Memory {
  id: ID!
  namespace: String!
  memoryType: String!
  content: String
  userId: String
  agentName: String
  location: String
  locationLines: [Int!]
  validFrom: Int
  validTo: Int
  feedbackScore: Float
  metadata: JSON
  mentions: [String!]
  edges(first: Int = 20, after: String): EdgeConnection!
}

type Edge {
  targetId: ID!
  relation: String!
  extractor: String
}

# Edge resolution does NOT traverse to target Memory; only targetId is returned.
# Nested fan-out is forbidden; depth cap = 1 (no nested Memory under Edge).
# max_visited_nodes = 1000 per query; timeout via graphql_timeout_ms (default 250).

type EdgeConnection { edges: [Edge!]! pageInfo: PageInfo! }
type MemoryConnection { edges: [MemoryEdge!]! pageInfo: PageInfo! }
type MemoryEdge { cursor: String! node: Memory! }
type PageInfo { hasNextPage: Boolean! endCursor: String }

type Query {
  memory(id: ID!, namespace: String!): Memory
  memories(
    namespace: String!
    first: Int = 20
    after: String
    memoryType: String
    includeArchived: Boolean = false
    includeRetired: Boolean = false
  ): MemoryConnection!
  search(
    text: String!
    namespace: String!
    first: Int = 20
    after: String
    memoryType: String
    includeArchived: Boolean = false
    includeRetired: Boolean = false
  ): MemoryConnection!
}

scalar JSON
```

### 5.3 Execute signature
```rust
pub fn execute(
    store: &LadybugStore,
    namespace: &str,
    principal: &str,   // resolved from auth context, never client input (ReBAC §5.3)
    query: &str,
    variables: Option<&Value>,
) -> GraphQLResponse
```
Returns `{ "data": ..., "errors": [...] }`. Parse errors, validation errors, and execution errors populate `errors`; partial data may still be returned.

Principal resolution mirrors ReBAC §5.3: on `POST /graphql`, MCP `neurostrata_graphql_query`, and CLI, `principal` is derived from daemon/session auth; an explicit client-supplied `principal` is honored only for `admin`.

`Memory.metadata` is returned with internal/sensitive keys stripped (`mesh`, `validation`, `visible_to` unless caller is allowed). Registry/mesh-conflict rows are hard-excluded even when queried by `id` (returns `null`).

### 5.4 Pagination & ordering
- `first` clamped to `[1, 100]`; default 20.
- Deterministic sort: `memories` → `(valid_from DESC, id ASC)`; `search` → `(score ASC, id ASC)` (score is the existing search distance).
- Cursor = base64 of the last returned `MemoryId` (the sort key tuple, not just id) so `after` resumes deterministically. Invalid/tampered `after` → GraphQL error `{ "message": "invalid cursor" }` (no panic, no silent reset).
- `hasNextPage` computed from total count vs. returned.

### 5.5 Filtering & privacy
- `namespace` required and enforced.
- Excludes retired/archived/superseded/mesh-conflicting/registry rows by default; `includeArchived`/`includeRetired` override.
- ReBAC: `principal` filter applied (restricted rows hidden unless allowed).
- `STRUCTURAL_MEMORY_TYPES` + `community_summary` excluded from default `memories` query unless `memoryType` explicitly requests them.

### 5.6 Mutations
Any operation whose root field is not `Query` → GraphQL error `"mutations are not supported"`.

### 5.7 Transport
- Daemon: `POST /graphql` with body `{ "query": ..., "variables": ... }`; returns GraphQL response JSON.
- MCP tool: `neurostrata_graphql_query(query, variables?, namespace?)`.
- CLI: `neurostrata-mcp graphql query --file <path>`.

## 6. Interfaces

- `src/graphql/mod.rs`: `execute`, `GraphQLResponse`, `GraphQLValue`.
- `src/schemas/graphql/schema.v1.graphql`: embedded SDL.
- Config: `graphql_enabled: bool` (default true).

## 7. Edge cases

- Empty query: parse error.
- Unknown field: validation error.
- `first > 100`: clamped with warning in `errors`.
- Unknown namespace: empty result, no error.
- Mutation attempt: error, no data.

## 8. Testing

- Unit: schema SDL parses.
- Unit: pagination clamping.
- Integration: `memories` returns paginated, filtered results.
- Integration: mutation rejected.
- Integration: namespace isolation and privacy.

## 9. Acceptance

Curated v1 GraphQL read API available; fixed SDL; pagination capped; filtering and privacy enforced; mutations rejected; standard response shape; no auto-generated schema; no LLM.

## 10. Files touched (expected)

- `src/graphql/mod.rs`: full implementation.
- `src/schemas/graphql/schema.v1.graphql`: new SDL.
- `src/server.rs`: MCP tool.
- `src/daemon.rs`: HTTP endpoint.
- `Cargo.toml`: `graphql-parser` (or approved crate).