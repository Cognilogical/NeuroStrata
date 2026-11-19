# `v1.19.0`: `LadybugDB/ontobug` + `rudof` RDF/OWL/SHACL/ShEx Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` to implement this plan task-by-task using free subagents (`worker.fallback` / `craftsman` / `checker-free` / `reviewer`).

**Goal:** Integrate `LadybugDB/ontobug` (`MIT OR Apache-2.0`) and `rudof` (`0.3.21` W3C SHACL-Core & ShEx validation) as workspace crate `crates/ontobug` aligned to `lbug = "0.22.3"` and `LadybugStore` shared `Arc<lbug::Database>`, replace the hand-rolled SHACL `.ttl` parser in `src/rdf/ontology.rs`, normalize all 8 embedded SHACL shape files in `src/schemas/shacl/*.ttl`, wire W3C SHACL & ShEx validation into `src/rdf/shacl.rs` and `src/rdf/ontology.rs`, and bump release version to `1.19.0`.

**Architecture:**
- `crates/ontobug` vendors upstream `LadybugDB/ontobug` (`0.1.0`, `MIT OR Apache-2.0`) with `lbug = "0.22.3"`, `default = []`, `LadybugSchemaStore::from_database(db: Arc<Database>, path: impl AsRef<Path>)` + `Catalog::from_database(db: Arc<Database>, path: impl AsRef<Path>)`, and a pure in-memory `extract_shacl_shapes(ttl: &str) -> Result<Vec<ExtractedShaclShape>>` helper that resolves RDF `rdf:first`/`rdf:rest` lists and blank-node property shapes regardless of triple emission order.
- `src/schemas/shacl/*.ttl` (8 embedded shape files) are normalized for strict W3C SHACL-Core (`rudof` `ShaclParser`) compliance (`sh:nodeKind sh:BlankNodeOrIRI` instead of invalid `sh:List`, required `sh:path` on `ns:AttemptSubPropertyShape`, unified `@prefix ns: <https://neurostrata.org/vocab#>`).
- `src/rdf/ontology.rs` delegates `OntologyIndex::load_shacl_shapes` to `ontobug::extract_shacl_shapes` and exposes `validate_rdf_turtle` / `validate_rdf_shex` backed by `ontobug::validate::shacl_turtle` / `ontobug::validate::shex` backed by `rudof` `0.3.21`.
- `src/rdf/shacl.rs` validates all 8 embedded `.ttl` shape files through `ontobug::validate::shacl_schema` + `ontobug::extract_shacl_shapes` and provides W3C SHACL/ShEx validation helpers translating `ontobug::Finding` / `ShexResult` into `ShaclViolation`.
- `src/store/ladybug.rs` exposes `ontobug_catalog()` sharing `self.db.clone()` under `self.write_lock` for schema registration and property-graph projection.

---

### Task 1: Create Workspace Crate `crates/ontobug` Aligned to `lbug = "0.22.3"` & Shared `Arc<Database>`

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/ontobug/Cargo.toml`
- Create: `crates/ontobug/LICENSE-MIT`
- Create: `crates/ontobug/LICENSE-APACHE`
- Create: `crates/ontobug/src/lib.rs`
- Create: `crates/ontobug/src/error.rs`
- Create: `crates/ontobug/src/registry.rs`
- Create: `crates/ontobug/src/ladybug.rs`
- Create: `crates/ontobug/src/catalog.rs`
- Create: `crates/ontobug/src/validate.rs`

**Steps:**
1. Add `"crates/ontobug"` to `[workspace].members` and `ontobug = { path = "crates/ontobug" }` to `[dependencies]` in root `Cargo.toml`. Also bump `version = "1.19.0"` in root `Cargo.toml`.
2. Create `crates/ontobug/Cargo.toml` with `lbug = "0.22.3"`, `oxigraph = { version = "0.5.11", default-features = false }`, `oxrdf = "0.3.4"`, `oxrdfio = "0.2.6"`, `oxsdatatypes = "0.2.3"`, `sha2 = "0.10"`, `thiserror = "2"`, `rudof_rdf = "0.3.21"`, `rudof_iri = "0.3.21"`, `shacl = "0.3.21"`, `shex_ast = "0.3.21"`, `shex_validation = "0.3.21"`, `serde_json = "1"`, `default = []`.
3. Add `LadybugSchemaStore::from_database(db: Arc<Database>, path: impl AsRef<Path>) -> Result<Self>`, `LadybugSchemaStore::from_database_uninitialized(db: Arc<Database>, path: impl AsRef<Path>) -> Self`, `pub fn ensure_table(&self) -> Result<()>`, and `Catalog::from_database(db: Arc<Database>, path: impl AsRef<Path>) -> Result<Self>`.
4. In `crates/ontobug/src/validate.rs`, add `ExtractedShaclShape` and `ExtractedShaclProperty` plus `pub fn extract_shacl_shapes(ttl: &str) -> Result<Vec<ExtractedShaclShape>>` which parses Turtle via `oxrdfio::RdfParser`, resolves `rdf:first`/`rdf:rest` list chains in any triple order, extracts `sh:NodeShape` targetClass, property constraints (`sh:path`, `sh:minCount`, `sh:maxCount`, `sh:datatype`, `sh:nodeKind`, `sh:in`, `sh:pattern`, `sh:minInclusive`, `sh:maxInclusive`, `sh:class`, `sh:message`), and validates the SHACL graph syntax.
5. Run `cargo test -p ontobug` to verify `crates/ontobug` compiles and passes all unit tests.

---

### Task 2: Normalize Embedded `.ttl` Shapes & Wire `ontobug` into `src/rdf/shacl.rs`, `src/rdf/ontology.rs`, and `src/store/ladybug.rs`

**Files:**
- Modify: `src/schemas/shacl/memory-shapes.v1.ttl`
- Modify: `src/schemas/shacl/fact.ttl`
- Modify: `src/schemas/shacl/attempt-shapes.v1.ttl`
- Modify: `src/schemas/shacl/multimodal-shapes.v1.ttl`
- Modify: `src/rdf/shacl.rs`
- Modify: `src/rdf/ontology.rs`
- Modify: `src/store/ladybug.rs`

**Steps:**
1. Fix W3C SHACL-Core compliance in the 4 embedded `.ttl` files:
   - `memory-shapes.v1.ttl` & `fact.ttl`: replace `sh:nodeKind sh:List` with `sh:nodeKind sh:BlankNodeOrIRI`.
   - `attempt-shapes.v1.ttl`: add `sh:path ns:attemptSubProperty ;` to `ns:AttemptSubPropertyShape`.
   - `multimodal-shapes.v1.ttl`: unify `@prefix ns: <https://neurostrata.org/vocab#> .`.
2. Update `src/rdf/shacl.rs`:
   - `parse_embedded_shapes()` compiles each embedded `.ttl` file via `ontobug::validate::shacl_schema(content, &ontobug::validate::RDFFormat::Turtle)` and `ontobug::extract_shacl_shapes(content)`.
   - Add `validate_rdf_with_ontobug(data_ntriples_or_ttl: &str, shapes_ttl: &str) -> anyhow::Result<Vec<ShaclViolation>>` and `validate_shex_with_ontobug(data_ntriples_or_ttl: &str, shexc: &str, base: &str, shape_map: &str) -> anyhow::Result<Vec<ShaclViolation>>`.
3. Update `src/rdf/ontology.rs`:
   - Replace the ~310-line manual state machine in `OntologyIndex::load_shacl_shapes(&mut self, ttl: &str)` with `ontobug::extract_shacl_shapes(ttl)?`, mapping `ExtractedShaclShape` into `ShaclShapeDef` (`shape_iri`, `target_class`, `property_constraints`, `min_counts`, `max_counts`, `datatypes`, `node_kinds`, `in_values`, `messages`).
   - Add `OntologyIndex::validate_rdf_graph(&self, data_ttl: &str) -> anyhow::Result<Vec<ShaclViolation>>` using `ontobug`.
4. Update `src/store/ladybug.rs`:
   - Add `pub async fn ontobug_catalog(&self) -> anyhow::Result<ontobug::Catalog>` on `LadybugStore` that acquires `self.write_lock.lock().await` and returns `ontobug::Catalog::from_database(self.db.clone(), &self.local_path)`.
5. Run focused tests:
   - `cargo test --bin neurostrata-mcp rdf::shacl::`
   - `cargo test --bin neurostrata-mcp rdf::ontology::`

---

### Task 3: Update Version & Documentation (`v1.19.0`) and Run Independent Verification (`checker-free` + `reviewer`)

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `docs/` (if needed for Doc-Sync Gate)

**Steps:**
1. Update `CHANGELOG.md` with the `[1.19.0]` release entry covering the `LadybugDB/ontobug` + `rudof` RDF/OWL/SHACL/ShEx integration, the LadybugDB ecosystem audit, the Bidirectional Doc-Sync Gate, and Zero-Config Workflow Connectors.
2. Dispatch `checker-free` and `reviewer` to verify all claims, run focused gates (`cargo test -p ontobug`, `cargo test --bin neurostrata-mcp rdf::shacl::`, `cargo test --bin neurostrata-mcp rdf::ontology::`, `neurostrata-mcp task gate NeuroStrata --strict`), complete task `neurostrata-wy2r`, push `feat/doc-gate-and-workflow-connectors`, and open the `v1.19.0` PR.
