# WASM Browser/Edge Engine Design

> Status: **APPROVED** (2026-10-28).
> Task: `neurostrata-6hjs` (WASM browser/edge engine, OxiRS §1.20).
> Companion: `docs/superpowers/specs/2026-10-27-rdf-canonical-memory-data-model-migration-design.md`, `docs/superpowers/specs/2026-10-28-tdb2-storage-hygiene-design.md`.

## 1. Problem

There is no WASM target. NeuroStrata depends on LadybugDB (C++ via `lbug`), `fastembed`/`ort`, `openssl`, `cxx`, `tree-sitter`, and an OS-bound Tokio runtime. These cannot compile to `wasm32-unknown-unknown`. Browser/edge deployments need a read-only subset that loads a pre-built snapshot and performs lexical search, vector search, and bounded graph traversal without any native dependency.

## 2. Goal

Add a **separate, read-only WASM crate** (`crates/neurostrata-wasm`) that:

- compiles to `wasm32-unknown-unknown`,
- loads a versioned JSON snapshot into memory,
- performs lexical search, vector search, and bounded graph traversal,
- exposes a JS API via `wasm-bindgen`,
- shares pure-Rust algorithms with the native crate (`src/fts`, `src/graph_read/related.rs`) without reimplementing them,
- never depends on LadybugDB, `fastembed`, `ort`, `openssl`, `cxx`, `tree-sitter`, `tokio`, `reqwest`, `axum`, `uuid`, or `chrono`.

## 3. Constraints

- The WASM crate is a separate workspace member; the root native binary build is unchanged.
- No write operations (no upsert/delete/mesh sync).
- No embedding inference in WASM; vectors are provided in the snapshot and query vectors are supplied by the JS caller.
- No `SystemTime::now()` / `chrono::Utc::now()`; `now_unix: i64` is supplied by the caller at engine construction.
- Deterministic: identical snapshot + query + `now_unix` → identical output.
- Bundle size budget: ≤ 2 MiB gzipped (warn if exceeded).
- Snapshot format is strict JSON with an explicit schema; no RDF/N-Quads parsing in WASM.
- Invalid rows are skipped with warnings (warn-first), never silently dropped and never abort a whole load unless the JSON itself is syntactically invalid.

## 4. Scope

In scope:

- New crate `crates/neurostrata-wasm` with `[lib] crate-type = ["cdylib", "rlib"]`.
- `SnapshotFile` / `SnapshotRecord` / `SnapshotEdge` schema and loader.
- `WasmEngine` JS API: `new(now_unix)`, `load_snapshot`, `search`, `get_memory`, `list`, `neighbors`, `namespaces`, `vector_search`.
- Lexical search via the approved `LexicalIndex` API from `src/fts/mod.rs`.
- Vector search (cosine similarity, pure Rust).
- Graph traversal via `src/graph_read/related.rs`.
- `wasm-pack` build + `wasm-opt -Oz` pipeline.

Out of scope:

- Write operations.
- LadybugDB in WASM.
- Embedding inference.
- Mesh sync.
- RDF/N-Quads ingestion.
- Multi-namespace federated search.

## 5. Approach

### 5.1 Workspace and crate layout

Root `Cargo.toml` gains:

```toml
[workspace]
members = [".", "crates/neurostrata-wasm"]
```

`crates/neurostrata-wasm/Cargo.toml`:

```toml
[package]
name = "neurostrata-wasm"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
wasm-bindgen = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- The WASM crate has **no** native dependencies and no `[build-dependencies]`.
- It includes the pure modules by path:

```rust
#[path = "../../../src/fts/mod.rs"]
mod fts;
#[path = "../../../src/graph_read/related.rs"]
mod related;
```

- `src/fts/mod.rs` and `src/graph_read/related.rs` are `std`/`serde` only (verified: no `crate::` imports in `related.rs`; `fts` uses `std` only). The full-text spec's `LexicalIndex` is the API used here.
- The root crate keeps its existing binary target; no `#[cfg(target_arch = "wasm32")]` scattered through native modules.
- `serde-wasm-bindgen` is not used; all JS-facing methods take primitives/`&str`/`&[f32]` and return JSON `String`.

### 5.2 Snapshot schema

```rust
#[derive(Deserialize)]
pub struct SnapshotFile {
    pub version: u32,                 // must be 1
    pub records: Vec<SnapshotRecord>,
    #[serde(default)]
    pub edges: Vec<SnapshotEdge>,
}

#[derive(Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub namespace: String,
    pub payload: SnapshotPayload,
    pub vector: Option<Vec<f32>>,
    pub embedding_id: Option<String>, // e.g. "fastembed:all-MiniLM-L6-v2"
}

#[derive(Deserialize)]
pub struct SnapshotPayload {
    pub content: String,
    pub user_id: String,
    pub memory_type: String,
    pub agent_name: String,
    pub location: String,
    pub location_lines: String,
    pub metadata: serde_json::Value,
}

#[derive(Deserialize)]
pub struct SnapshotEdge {
    pub source: String,   // resolved record id
    pub relation: String, // GOVERNS | CONTAINS | RELATES_TO | EXTRACTED_FROM | DEPENDS_ON
    pub target: String,   // resolved record id
}
```

- Edges are **pre-resolved** by the native snapshot builder. WASM never resolves metadata targets such as `"src/lib.rs"` to generated/scoped ids; it requires the exporter to emit the same ids the records carry.
- `version != 1` → `{ "error": "unsupported snapshot version" }`, engine unchanged.
- Duplicate `(namespace, id)` → last record wins, warning `DuplicateId`.
- Missing required field → JSON parse error; engine unchanged.
- Snapshot builder is native-only and emits this JSON from a read-only export; it is not part of the WASM crate.

### 5.3 In-memory store

```rust
pub struct WasmStore {
    pub namespaces: BTreeMap<String, NamespaceData>,
    pub now_unix: i64,
}
pub struct NamespaceData {
    pub records: BTreeMap<String, SnapshotRecord>,
    pub edges: Vec<SnapshotEdge>,
    pub lexical: fts::LexicalIndex,
    pub vector_dims: Option<usize>,
}
```

- `WasmStore::from_snapshot(file: SnapshotFile, now_unix: i64) -> (Self, LoadReport)`.
- Builds one `LexicalIndex` per namespace from `payload.content` and `payload.location`.
- Vector dimension consistency: the first record with `Some(vector)` sets `vector_dims`; any later record whose vector length differs is skipped with `DimensionMismatch`.
- `embedding_id` consistency: the first vector record sets `embedding_id`; records with a different id are skipped with `EmbeddingMismatch`.
- No trait abstraction layer. `WasmStore` is concrete; `ListFilter` is a serde struct, not a competing `MemoryStore`/`StorageBackend` trait.

### 5.4 Row-class filtering

All read paths (`get_memory`, `list`, `search`, `vector_search`, `neighbors`) apply the same filter:

```rust
pub struct ReadOptions {
    pub include_tasks: bool,      // default false
    pub include_archived: bool,   // default false
    pub include_structural: bool, // default false
}
pub fn is_visible(rec: &SnapshotRecord, now_unix: i64, opts: &ReadOptions) -> bool
```

Exclude when any of:

- `metadata.valid_to` is a positive integer `<= now_unix` (retired),
- `metadata.archived == true` and `!include_archived`,
- `synaptic_mesh::conflict::is_conflicting_metadata(&metadata)` (ported predicate; no native dependency),
- `registry::is_registry_row(&metadata)` (ported predicate),
- `payload.memory_type == "task"` and `!include_tasks`,
- `payload.memory_type` in `["directory", "file", "markdown", "freshness_flag", "guard_event"]` and `!include_structural`.

The two predicate functions are small, pure JSON checks copied into the WASM crate with a comment naming their native source. They must not drift; a native test asserts both implementations agree on fixtures.

### 5.5 Lexical search

- Uses `fts::LexicalIndex` from the approved full-text spec (`src/fts/mod.rs`): multi-field `content` + `location`, namespace-partitioned index.
- `WasmEngine::search(namespace, query, limit, include_tasks, include_archived)`:
  - returns `Vec<SearchHit { id, score: f32 }>`,
  - sorts by `score` descending, then `id` ascending (deterministic),
  - truncates to `limit` (clamped 1..=100).
- Empty query → empty array, no error.
- Unknown namespace → empty array.
- No substring matching: the `LexicalIndex` API from the full-text spec is token-based.

### 5.6 Vector search

```rust
pub fn vector_search(
    namespace: &str,
    query_vec: &[f32],
    embedding_id: &str,
    limit: usize,
    include_tasks: bool,
    include_archived: bool,
) -> Result<Vec<SearchHit>, String>
```

- If namespace has no vectors → empty.
- If `query_vec.len() != namespace.vector_dims` → error `{ "error": "vector dimension mismatch: expected N, got M" }`.
- If `embedding_id` differs from namespace `embedding_id` → error `{ "error": "embedding mismatch" }`.
- Cosine similarity:

```
cosine(a,b) = dot(a,b) / (norm(a) * norm(b))
```

Zero-norm → `0.0`.
- Score is `f32`; sort by score descending, then `id` ascending.
- `limit` clamped 1..=100.

### 5.7 Graph traversal

- Reuses `src/graph_read/related.rs` (`graph_read_related`, `DEFAULT_RELATIONS`, `Direction`, `parse_direction`, `clamp_max_hops`, `clamp_limit`, `MAX_NODES`).
- `WasmEngine::neighbors(namespace, id, relation, direction, max_hops, limit)`:
  - `relation`: `""` for all allowed relations, or one of `GOVERNS`, `CONTAINS`, `RELATES_TO`, `EXTRACTED_FROM`, `DEPENDS_ON` (case-sensitive uppercase; unknown → error).
  - `direction`: `"outgoing" | "incoming" | "both"` via `parse_direction`.
  - `max_hops` clamped 1..=2; `limit` clamped 1..=25; hard `MAX_NODES = 20` from `related.rs`.
  - Builds `NodeSpec` from visible records and `EdgeSpec` from snapshot edges, marks invisible nodes `visible: false`.
  - Returns `RelatedResult` JSON (nodes, edges, evidence, total, truncated, next_cursor omitted because WASM has no generation/pagination).
- Edge direction semantics are those already encoded in `related.rs` (`RELATES_TO` undirected; `CONTAINS` parent→child; others self→target).
- Cycles terminate via the existing BFS visited set.

### 5.8 Snapshot loading report

`load_snapshot(json: &str) -> String` always returns JSON:

```json
{ "ok": true, "loaded": 42, "skipped": 2, "warnings": [{"id":"mem-x","reason":"DimensionMismatch"}] }
```

or

```json
{ "ok": false, "error": "invalid snapshot JSON: ..." }
```

- On any top-level parse/version error, the engine retains its previous state.
- Per-row SHACL-equivalent violations (missing `content`, invalid `memory_type`, invalid metadata types) are skipped with warnings; they do not abort the load. This matches the native warn-first convention for pre-existing rows.
- `load_snapshot` never throws a JS exception.

### 5.9 JS API

```rust
#[wasm_bindgen]
pub struct WasmEngine { store: WasmStore }

#[wasm_bindgen]
impl WasmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new(now_unix: f64) -> Self;
    pub fn load_snapshot(&mut self, json: &str) -> String;
    pub fn search(&self, namespace: &str, query: &str, limit: usize, include_tasks: bool, include_archived: bool) -> String;
    pub fn get_memory(&self, namespace: &str, id: &str, include_tasks: bool, include_archived: bool) -> String; // JSON or "null"
    pub fn list(&self, namespace: &str, filter_json: &str) -> String; // JSON array
    pub fn neighbors(&self, namespace: &str, id: &str, relation: &str, direction: &str, max_hops: usize, limit: usize) -> String;
    pub fn namespaces(&self) -> String; // JSON array
    pub fn vector_search(&self, namespace: &str, query_vec: &[f32], embedding_id: &str, limit: usize, include_tasks: bool, include_archived: bool) -> String;
}
```

- All methods return JSON strings; no method throws for domain errors (errors are `{ "error": "..." }`).
- `list` accepts `{"memory_type": "...", "domain": "...", "include_tasks": false, "include_archived": false}`; unknown fields rejected.
- `new` takes `now_unix` as `f64` because JS numbers are doubles; it is rounded/truncated to `i64` and must be finite.

### 5.10 Build pipeline

- Target: `wasm32-unknown-unknown`.
- Build: `wasm-pack build crates/neurostrata-wasm --target web --release`.
- Post-process: `wasm-opt -Oz -o pkg/neurostrata_wasm_bg.wasm pkg/neurostrata_wasm_bg.wasm` (from `binaryen`).
- Output `crates/neurostrata-wasm/pkg/` is gitignored.
- Measure gzipped size; fail CI if > 2 MiB, warn locally.
- Native tests remain in the root crate; WASM smoke tests use `wasm-pack test --headless --firefox crates/neurostrata-wasm` (CI only).

### 5.11 Determinism

- No clock reads; `now_unix` is an input.
- All maps are `BTreeMap`; all sorts are explicit `(score, id)`.
- Snapshot loading is single-threaded and deterministic.
- No random ids; ids come from the snapshot.

## 6. Interfaces

- New crate: `crates/neurostrata-wasm`.
- `crates/neurostrata-wasm/src/lib.rs`: `WasmEngine`, `WasmStore`, `SnapshotFile`, `SnapshotRecord`, `SnapshotPayload`, `SnapshotEdge`, `ReadOptions`, `LoadReport`, `SearchHit`.
- Reused pure modules: `src/fts/mod.rs` (`LexicalIndex`), `src/graph_read/related.rs` (`graph_read_related`).
- JS API: `new`, `load_snapshot`, `search`, `get_memory`, `list`, `neighbors`, `namespaces`, `vector_search`.
- Build: `wasm-pack build crates/neurostrata-wasm --target web --release`.
- Config: none (build-time crate; `now_unix` is runtime input).

## 7. Edge cases

- Empty snapshot: engine returns empty results, no error.
- Invalid JSON: `{ "ok": false, "error": ... }`, engine unchanged.
- Unknown namespace: empty results.
- Zero-norm vector: cosine returns `0.0`.
- Dimension/embedding mismatch: skipped at load, error at query.
- Deep traversal: clamped by `related.rs` (max 2 hops, 20 nodes).
- Invisible rows: never returned and never traversed through.
- Unicode: lexical snippets and ids are UTF-8 safe (no byte slicing).

## 8. Testing

- Unit: snapshot parse/version/duplicate/row-skip warnings.
- Unit: lexical search deterministic ranking and tie-break.
- Unit: cosine similarity (known vectors), zero-norm, dimension mismatch.
- Unit: `is_visible` filter parity with native fixtures.
- Unit: graph traversal bounded, directionality, cycle termination, invisible nodes excluded.
- Unit: JSON error shape for every public method.
- WASM smoke: `wasm-pack test --headless --firefox`.
- Size: gzipped bundle ≤ 2 MiB.

## 9. Acceptance

Separate WASM crate compiles to `wasm32-unknown-unknown`; loads versioned JSON snapshot; lexical + vector search + bounded traversal; JS API; no native dependencies; bundle ≤ 2 MiB gzipped; no LLM; deterministic with caller-supplied time.

## 10. Files touched (expected)

- `Cargo.toml` (root): add `[workspace] members = [".", "crates/neurostrata-wasm"]`; no native dependency changes.
- `crates/neurostrata-wasm/Cargo.toml` (new): only `wasm-bindgen`, `serde`, `serde_json`.
- `crates/neurostrata-wasm/src/lib.rs` (new): engine, snapshot schema, store, JS bindings.
- `crates/neurostrata-wasm/.gitignore` (new): `pkg/`.
- `src/fts/mod.rs`: reused by path; no WASM-specific changes.
- `src/graph_read/related.rs`: reused by path; no WASM-specific changes.
- Native snapshot builder (root): emits `SnapshotFile` JSON (new `neurostrata_export_wasm_snapshot` tool or CLI `export-wasm-snapshot`).