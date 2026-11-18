# 2026-11-17 — `v1.19.0`: LadybugDB Ecosystem Survey & `ontobug` RDF/OWL/SHACL/ShEx Integration Specification

> **Task**: `neurostrata-wy2r` (`in_progress`, claimed by `bosun`)
> **Branch**: `feat/doc-gate-and-workflow-connectors`
> **Stashes**: All 4 existing git stashes (`stash@{0}`..`stash@{3}`) preserved untouched.

---

## 1. Executive Summary & Ecosystem-First Mandate

### 1.1 Ecosystem-First Architectural Rule (`dc27f9f9-8039-4896-a370-3c9f38224765`)
Never build functionality in-house when a compatible-licensed (`MIT`, `Apache-2.0`, `BSD`) upstream library or crate exists in the ecosystem. Before designing or coding any storage, graph, ontology, RDF/SHACL/SPARQL, retrieval, or protocol feature, survey the upstream ecosystem (`https://ladybugdb.com/`, `org:LadybugDB`, `org:Ladybug-Memory`, `oxigraph`, `rudof`, crates.io), verify SPDX license compatibility, and integrate or adapt the upstream crate rather than writing bespoke parsers or validators.

### 1.2 `v1.19.0` Release Scope
Branch `feat/doc-gate-and-workflow-connectors` consolidates:
1. **`v1.18.1` Mainline Merge** (`dd1affef8b685bda92616736ed8ef479cc039224`).
2. **Bidirectional Doc-Sync Gate & Scoped Verification** (`src/task/mod.rs`, `src/daemon.rs`, `src/main.rs`).
3. **Zero-Config Workflow Connectors** (`github`, `linear`, `jira` in `src/connectors/`).
4. **Complete `ladybugdb.com` Ecosystem Audit** (`org:LadybugDB` [43 repos] + `org:Ladybug-Memory` [16 repos]) recorded in the SKOS domain taxonomy (`concept-w3c-rdf-star-bitemporal-ontology-v2`, `concept-bitemporal-lpg-knowledge-graph-v4`, `concept-leiden-community-and-scip-code-graph-v3`) and project memory (`98d82a06-90fe-410e-a445-dbc85e80a28d`).
5. **Option 1 `LadybugDB/ontobug` + `rudof` Integration**:
   - Workspace crate `crates/ontobug` derived from upstream `LadybugDB/ontobug` (`MIT OR Apache-2.0`), aligned to NeuroStrata's `lbug = "0.22.3"`, `oxrdf = "0.3.4"`, `oxrdfio = "0.2.6"`, `oxigraph = "0.5.11"`, and `rudof` (`rudof_rdf = "0.3.21"`, `rudof_iri = "0.3.21"`, `shacl = "0.3.21"`, `shex_ast = "0.3.21"`, `shex_validation = "0.3.21"`), with `default-features = false` (no `reqwest` blocking client overhead).
   - Shared-database constructor `LadybugSchemaStore::from_database(Arc<lbug::Database>, path)` synchronized with `LadybugStore::write_lock` (`src/store/ladybug.rs:1518`).
   - Replacement of the brittle ~310-line hand-rolled Turtle triple-order state machine in `src/rdf/ontology.rs` (`OntologyIndex::load_shacl_shapes`) with `ontobug` schema parsing, SHA-256 drift detection, LadybugDB property-graph projection (`SchemaEntry`, `OntologyClass`, `OntologyProperty`, `ShaclShape`, `ShaclProperty`, `Namespace`), and `rudof` W3C SHACL-Core + ShEx validation.

---

## 2. Complete LadybugDB Ecosystem Survey (`ladybugdb.com`)

Audited surfaces: `https://ladybugdb.com/`, `org:LadybugDB` (43 public repositories), `org:Ladybug-Memory` (16 public repositories), and `LadybugDB/ladybug-docs`.

| Upstream Repository | SPDX License | Language & Key Dependencies | Capability vs. NeuroStrata Stack | `v1.19.0` Architectural Decision |
|---|---|---|---|---|
| **`LadybugDB/ladybug`** (`lbug`) | `MIT` | C++20 / Rust (`lbug = "0.22.3"`, `cxx = "=1.0.205"`) | Core embedded columnar Labeled Property Graph storage engine (`src/store/ladybug.rs`). | **Retain** (`lbug = "0.22.3"` already in `Cargo.toml:102`). |
| **`LadybugDB/ontobug`** | `MIT OR Apache-2.0` | Rust (`lbug`, `oxrdf`, `oxttl`, `rudof` `0.3.21` [`shacl_validation`, `shex_validation`, `rudof_rdf`], `sha2`) | RDF/OWL/RDFS/SHACL/ShEx schema catalog, `LadybugSchemaStore` Cypher projection (6 node tables, 9 rel tables), SHA-256 drift detection, transitive `owl:imports` resolution, W3C SHACL & ShEx validation. | **Adopt (Option 1 in `v1.19.0`)** as workspace member `crates/ontobug` aligned to `lbug = "0.22.3"`, `oxrdf = "0.3"`, `oxttl = "0.2"`, and `LadybugStore` shared `Arc<Database>`. |
| **`Ladybug-Memory/icebug`** & **`icebug-rust`** | `MIT` | C++ NetworKit fork + Rust (`icebug = "13.5.0"`, `arrow = "56"`, `libnetworkit`, `libomp`) | Zero-copy Arrow CSR graph analytics providing 220+ algorithms (`Leiden`, `Louvain`, `PageRank`, `Betweenness`, `DegreeCentrality`, `ConnectedComponents`). | **Document in SKOS Taxonomy; keep `leiden-rs` default**. `icebug` requires `arrow = "56"` and external C++ `libnetworkit`/`libomp` shared libraries, whereas NeuroStrata's single-binary sovereignty axis (`10000 bps`) uses pure-Rust `leiden-rs` (`src/community.rs`). |
| **`LadybugDB/extensions`** (`vector`, `fts`, `algo`, `llm`, `json`, `httpfs`, `duckdb`, `postgres`, `sqlite`, `iceberg`, `delta`, `unity_catalog`, `neo4j`) | `MIT` | C++ dynamically loaded `.lbug_extension` plugins | Native disk HNSW vector index, Okapi BM25 FTS, GDS graph algorithms, and external DB `ATTACH`. | **Retain Rust-native `Tantivy` + `fastembed` + bitemporal Cypher**. `LOAD <ext>` is recorded in the LadybugDB WAL and replayed on every DB open (creating C++ ABI/symbol-skew failure risk across binary upgrades, as documented in `LadybugDB/bugscope`), and native extension indexes do not pre-filter by NeuroStrata's bitemporal (`valid_from..valid_to`, `tx_from..tx_to`) + ReBAC (`access_policy`) predicates. |
| **`LadybugDB/ladybug-adbc-rs`** | `MIT` | Rust (`lbug = "0.22.1"`, Arrow Flight SQL / ADBC) | Arrow Database Connectivity (ADBC) & Flight SQL server over `lbug`. | **Catalog in SKOS Taxonomy** (`concept-bitemporal-lpg-knowledge-graph-v4`) as MIT Rust protocol adapter. |
| **`LadybugDB/bolt4rs`** | `MIT` | Rust (Neo4j Bolt v5.x wire protocol) | Rust Bolt protocol client/server adapter. | **Catalog in SKOS Taxonomy** (`concept-bitemporal-lpg-knowledge-graph-v4`) as MIT Rust protocol adapter. |
| **`LadybugDB/explorer`** | `MIT` | TypeScript / Node web UI | Standalone web graph explorer for LadybugDB. | **Retain NeuroStrata UI** (NeuroStrata already ships its own Tauri + WebGPU/Three.js bitemporal graph UI). |
| **`LadybugDB/bugscope`** & **`bugscope-tauri`** | **None** (no LICENSE file) | Rust GPUI / Tauri React (`private: true`) | Desktop visualizers for `.lbdb` files. | **Exclude** — fails NeuroStrata's SPDX license verification gate (`MIT` / `Apache-2.0` / `BSD` required). |
| **`LadybugDB/ladybug-fs`**, **`Ladybug-Memory/hermes-memory-plugin`**, **`LadybugDB/pg_ladybug`** | `MIT` / `PostgreSQL` | Python FUSE / Python Hermes / C Postgres extension | Language/platform bindings for Python FUSE, Python Hermes, and PostgreSQL FDW. | **Skip** — NeuroStrata is a pure-Rust single binary (Python is last-resort per rule `bf710e0e-a4b5-41cc-a8d7-e25fb14b12f8`) and already ships a zero-dependency TypeScript/Node Hermes plugin in `plugins/`. |

---

## 3. Deep-Dive Architecture: Option 1 `LadybugDB/ontobug` + `rudof` Integration

### 3.1 Workspace Crate `crates/ontobug` & Dependency Alignment
Upstream `LadybugDB/ontobug` (`0.1.0`, `MIT OR Apache-2.0`) cannot be pulled as an unmodified git dependency for three technical reasons verified against source:
1. **Native C++ Static Library Link Conflict (`links = "lbug"`)**: Upstream `ontobug` specifies `lbug = "0.21"`, whereas NeuroStrata uses `lbug = "0.22.3"` (`Cargo.toml:102`). Cargo forbids two versions of a crate declaring `links = "lbug"` in the same build graph, and `lbug::Database` types across `0.21` and `0.22.3` would be incompatible types in Rust.
2. **Embedded Database Lock Contention**: Upstream `LadybugSchemaStore::open(db_path)` calls `Database::new(path, SystemConfig::default())`. Because NeuroStrata's `LadybugStore` (`src/store/ladybug.rs:1501`) already holds the exclusive `.lbdb` process/file lock via `db: Arc<Database>`, calling `LadybugSchemaStore::open` on the same path would fail with a lock error.
3. **Oxigraph & Validation Crate Alignment & Default `reqwest` Feature**: Upstream `ontobug` defaults to `features = ["remote"]` (which pulls `reqwest` `0.12` with `blocking` + `rustls-tls`) and uses `oxrdf = "0.2"` / `oxttl = "0.1"` (`oxrdf::Subject`), whereas NeuroStrata uses `oxrdf = "0.3.4"` / `oxrdfio = "0.2.6"` (`oxrdf::NamedOrBlankNode`) and `reqwest = "0.13"`.

**Solution**:
Vendor `LadybugDB/ontobug` into `crates/ontobug` (preserving its `MIT OR Apache-2.0` license header and copyright attribution) as a workspace member in `Cargo.toml`:
```toml
[workspace]
members = ["crates/neurostrata-wasm", "crates/ontobug"]
exclude = ["src-tauri"]
```
In `crates/ontobug/Cargo.toml`:
- `lbug = "0.22.3"`
- `oxrdf = "0.3.4"`
- `oxrdfio = "0.2.6"`
- `oxigraph = { version = "0.5.11", default-features = false }`
- `rudof_rdf = "0.3.21"`
- `rudof_iri = "0.3.21"`
- `shacl = "0.3.21"`
- `shex_ast = "0.3.21"`
- `shex_validation = "0.3.21"`
- `default = []` (no `reqwest` dependency unless `remote` is explicitly enabled).

### 3.2 Shared `Arc<lbug::Database>` & Write-Turn Serialization
In `crates/ontobug/src/ladybug.rs`:
- Change `LadybugSchemaStore` to hold `db: Arc<lbug::Database>` instead of an owned `Database`.
- Provide:
  - `LadybugSchemaStore::from_database(db: Arc<lbug::Database>, path: impl AsRef<Path>) -> Result<Self>`: attaches to an already-open `Arc<lbug::Database>` and initializes the schema catalog DDL (`ensure_table()`).
  - `LadybugSchemaStore::from_database_uninitialized(db: Arc<lbug::Database>, path: impl AsRef<Path>) -> Self`: wraps `Arc<lbug::Database>` without running DDL so callers can invoke `ensure_table()` inside an existing write lock.
  - `LadybugSchemaStore::open(db_path: &Path) -> Result<Self>`: standalone constructor wrapping `Arc::new(Database::new(db_path, SystemConfig::default())?)` for standalone tests/tools.
  - `Catalog::from_database(db: Arc<lbug::Database>, path: impl AsRef<Path>) -> Result<Self>`: same shared-database attach for the catalog facade (see `crates/ontobug/src/catalog.rs`).
- **Write-Lock Invariant (`src/store/ladybug.rs:1518`)**:
  LadybugDB allows only one active write transaction at a time and refuses concurrent write transactions immediately. All `LadybugSchemaStore` mutations invoked from `LadybugStore` (`ensure_table`, `register`, `remove`, `load_triples`, `project_ontology`) MUST execute while holding `LadybugStore::write_lock`.

### 3.3 LadybugDB Property-Graph Schema Projection (`LadybugSchemaStore`)
`LadybugSchemaStore` maintains the following Cypher tables inside LadybugDB:
- **Catalog Node Table**:
  - `SchemaEntry(name STRING PRIMARY KEY, uri STRING, version STRING, format STRING, kind STRING, content STRING, sha256 STRING, loaded_at STRING, source STRING, triple_count INT64, classes_json STRING, properties_json STRING, shapes_json STRING, imports_json STRING)`
- **Ontology & SHACL Node Tables**:
  - `OntologyClass(uri STRING PRIMARY KEY, label STRING, comment STRING, schema_name STRING, is_deprecated BOOLEAN)`
  - `OntologyProperty(uri STRING PRIMARY KEY, label STRING, comment STRING, prop_type STRING, schema_name STRING, is_functional BOOLEAN)`
  - `ShaclShape(uri STRING PRIMARY KEY, target_class STRING, schema_name STRING, closed BOOLEAN, severity STRING)`
  - `ShaclProperty(id STRING PRIMARY KEY, shape_uri STRING, path STRING, datatype STRING, node_kind STRING, class_target STRING, min_count INT64, max_count INT64, pattern STRING, in_values STRING, message STRING, schema_name STRING)`
  - `Namespace(prefix STRING PRIMARY KEY, uri STRING, schema_name STRING)`
- **Ontology & SHACL Relationship Tables**:
  - `SubClassOf(FROM OntologyClass TO OntologyClass, schema_name STRING)`
  - `SubPropertyOf(FROM OntologyProperty TO OntologyProperty, schema_name STRING)`
  - `DomainOf(FROM OntologyProperty TO OntologyClass, schema_name STRING)`
  - `RangeOf(FROM OntologyProperty TO OntologyClass, schema_name STRING)`
  - `TargetsClass(FROM ShaclShape TO OntologyClass, schema_name STRING)`
  - `HasPropertyConstraint(FROM ShaclShape TO ShaclProperty, schema_name STRING)`
  - `RefersToShape(FROM ShaclProperty TO ShaclShape, schema_name STRING)`
  - `ImportsSchema(FROM SchemaEntry TO SchemaEntry)`
  - `InNamespace(FROM OntologyClass TO Namespace)`

Note: Compared to upstream `ontobug`, `ShaclProperty` in `crates/ontobug` is extended with `node_kind STRING`, `in_values STRING` (JSON array), and `message STRING` so that every constraint field needed by `OntologyIndex::shacl_shapes` (`ShaclShapeDef`) is preserved losslessly in both the in-memory `ExtractedShaclShape` and the LadybugDB `ShaclProperty` table.

### 3.4 Normalization of All 8 Embedded `.ttl` Shape Files (`src/schemas/shacl/`)
`src/rdf/shacl.rs:6-33` embeds **8** Turtle shape files:
1. `memory-shapes.v1.ttl`
2. `fact.ttl`
3. `rule.ttl`
4. `attempt-shapes.v1.ttl`
5. `edge-shapes.v1.ttl`
6. `topology-shapes.v1.ttl`
7. `mesh-record-shapes.v1.ttl`
8. `multimodal-shapes.v1.ttl`

**Audit Findings on Embedded `.ttl` Files for Strict W3C SHACL (`rudof` `ShaclParser`) Compliance**:
1. `memory-shapes.v1.ttl` (lines 48, 55, 62) and `fact.ttl` (lines 48, 55, 62) use `sh:nodeKind sh:List`. In W3C SHACL-Core §4.1.2, `sh:nodeKind` only permits `sh:BlankNode`, `sh:IRI`, `sh:Literal`, `sh:BlankNodeOrIRI`, `sh:BlankNodeOrLiteral`, or `sh:IRIOrLiteral`; `sh:List` is rejected by strict W3C SHACL compilers (`rudof`). **Fix**: Replace `sh:nodeKind sh:List` with `sh:nodeKind sh:BlankNodeOrIRI`.
2. `attempt-shapes.v1.ttl` (lines 12-13) declares `ns:AttemptSubPropertyShape a sh:PropertyShape ; sh:message "..." .` without a `sh:path`. In W3C SHACL-Core §2.3, every `sh:PropertyShape` must have exactly one `sh:path`. **Fix**: Add `sh:path ns:attemptSubProperty ;`.
3. `multimodal-shapes.v1.ttl` (line 1) declares `@prefix ns: <http://example.org/ns#> .` while the other 7 embedded shape files declare `@prefix ns: <https://neurostrata.org/vocab#> .`. **Fix**: Align `@prefix ns: <https://neurostrata.org/vocab#> .`.

### 3.5 Replacing the Hand-Rolled SHACL Parser in `src/rdf/ontology.rs` & Integrating `rudof` in `src/rdf/shacl.rs`
1. **`OntologyIndex::load_shacl_shapes(&mut self, ttl: &str)` (`src/rdf/ontology.rs:1193-1502`)**:
   - Replace the ~310-line manual `oxttl` triple-order state machine (`cur_shape`, `PendingShapeProp`, `deferred_in_lists`, `parse_shacl_in_values`, `resolve_sh_in_list`) with `ontobug::extract_shacl_shapes(ttl)?`, which builds an RDF graph (resolving blank-node `rdf:first`/`rdf:rest` lists regardless of triple emission order and validating syntax via `oxttl` + `rudof`) and returns structured `ontobug::ExtractedShaclShape` definitions mapped directly into `ShaclShapeDef`.
   - Store the raw validated Turtle source in `OntologyIndex::shapes_ttl: Option<String>` (set by `OntologyIndex::load_shacl_shapes` and consumed by `OntologyIndex::validate_rdf_graph`) so `OntologyIndex` can invoke full W3C SHACL RDF-graph validation via `ontobug::validate::shacl_turtle(data_ttl, shapes_ttl, &Mode::Native)` and ShEx validation via `ontobug::validate::shex(data_ttl, schema_shex, base, shape_map)`.
2. **`src/rdf/shacl.rs`**:
   - `parse_embedded_shapes()` validates all 8 `EMBEDDED_SHAPES` through `ontobug::extract_shacl_shapes` and `ontobug::validate::shacl_schema(content, &ontobug::validate::RDFFormat::Turtle)`, ensuring both Turtle syntax and W3C SHACL-Core schema validity at startup.
   - Add `validate_rdf_with_ontobug(data_ttl: &str, shapes_ttl: &str) -> anyhow::Result<Vec<ShaclViolation>>` and `validate_shex_with_ontobug(data_ttl: &str, schema_shex: &str, base: &str, shape_map: &str) -> anyhow::Result<Vec<ShaclViolation>>`, translating `ontobug::Report` / `ontobug::Finding` (`focus_node`, `result_path`, `message`, `source_shape`) into `ShaclViolation { field, why }`.
   - Keep `validate_memory(&MemoryPayload) -> Vec<ShaclViolation>` fast-path checks and exact `field`/`why` violation strings intact so all existing callers (`enforce`, `validate_strict_post_image`, `commit_action_batch`, `commit_scenario`, and all existing unit/integration tests) remain 100% backward compatible.

---

## 4. Data Flow, Fail-Closed Guarantees & Testing Plan

### 4.1 Data Flow
```text
Embedded Shapes (8 .ttl files in src/schemas/shacl/*.ttl)
  + Namespace Ontology Memories (memory_type = "ontology")
        │
        ▼
  crates/ontobug
  ├─ extract_shacl_shapes(ttl) ──► OntologyIndex.shacl_shapes (ShaclShapeDef)
  ├─ ontobug::validate::shacl_turtle / ontobug::validate::shex (rudof 0.3.21 W3C SHACL-Core & ShEx engine)
  └─ LadybugSchemaStore::from_database(Arc<lbug::Database>, path) [under write_lock]
       └─ Cypher projection: SchemaEntry, OntologyClass, OntologyProperty,
          ShaclShape, ShaclProperty, Namespace + 9 relationship tables
```

### 4.2 Fail-Closed Guarantees
- Any malformed embedded `.ttl` shape file causes `parse_embedded_shapes()` to report a `shacl.embedded_shapes` violation immediately.
- `commit_action_batch` (`src/rdf/action.rs`) and `commit_scenario` (`src/rdf/scenario.rs`) remain strictly fail-closed: any post-image `ShaclViolation` aborts before committing to LadybugDB.
- All `LadybugSchemaStore` writes on a shared `LadybugStore` acquire `write_lock` to prevent concurrent LadybugDB write-transaction collisions.

### 4.3 Focused Verification & Testing Plan
Per Permanent Owner Rule (`f689fec8-303f-4e58-92c8-51ac82595ccd`), subagents MUST NOT run full-workspace `cargo test` or use `sleep` polling. Run only targeted/focused tests:
1. `cargo test -p ontobug` — unit tests in `crates/ontobug` covering `Catalog`, `extract_shacl_shapes` (including out-of-order blank-node `sh:property` and `sh:in ( ... )` RDF lists), `rudof` `shacl_turtle` (conforming + violating graphs), `shex`, and `LadybugSchemaStore::from_database(Arc<Database>, path)` (registration, SHA-256 drift verification, ontology/SHACL Cypher node/rel projection).
2. `cargo test --bin neurostrata-mcp rdf::shacl::` — embedded shape compilation (all 8 `.ttl` files), rule/procedure/task/twin/action/taxonomy validation, and `ontobug` W3C SHACL & ShEx validation bridge tests.
3. `cargo test --bin neurostrata-mcp rdf::ontology::` — `load_shacl_shapes` via `ontobug`, `sh:in` list extraction, `shacl_validate`, and `validate_strict_post_image`.

### 4.4 Version Bump Plan (`v1.18.1` → `v1.19.0`)
- `Cargo.toml`: `version = "1.18.1"` → `version = "1.19.0"` (plus add `"crates/ontobug"` to `[workspace].members` and `ontobug = { path = "crates/ontobug" }` to `[dependencies]`).
- `crates/neurostrata-wasm/Cargo.toml`: align version if applicable.
- Check and align any version references in `src-tauri/` / `web-ui/` / `plugins/` if release-versioned.
