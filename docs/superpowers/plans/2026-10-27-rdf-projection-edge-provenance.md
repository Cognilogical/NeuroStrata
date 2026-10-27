# Implementation Plan: RDF Projection + Edge Provenance (Phase 1)

**Spec:** `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`

**Goal:** Add edge provenance columns to the five relation tables and a
read-only RDF projection module that converts LadybugDB rows into quads. No
OWL/SHACL/reasoning yet. No storage migration beyond `ALTER TABLE ... ADD`.

**Architecture:** LadybugDB stays the sole store. Edge provenance columns are
added in our schema/init code and migrated idempotently for existing DBs.
Upsert and relink write provenance alongside edges. A new `src/rdf/` module
projects `Memory` rows and edges into `Quad` values; it has no DB dependency
and is pure Rust, so it can be unit-tested without LadybugDB.

**Tech stack:** Rust, lbug 0.20.4, serde_json, chrono (already deps). No new
crates in Phase 1.

---

## File structure

Create:
- `src/rdf/mod.rs` — module root, re-exports.
- `src/rdf/types.rs` — `Quad`, `QuadObject`, `EdgeProvenance`, `Tier`.
- `src/rdf/namespace.rs` — URN/IRI helpers (`memory_iri`, `graph_iri`, `assertion_iri`).
- `src/rdf/projection.rs` — `memory_to_quads`, `edge_to_quad`, `rows_to_quads`.
- `src/rdf/projection_tests.rs` (or `#[cfg(test)]` in projection.rs) — unit tests.

Modify:
- `src/main.rs` — add `mod rdf;`.
- `src/store/ladybug.rs` — schema init + migration + upsert edge MERGE + relink `create_missing_edges`.
- `src/traits.rs` — (optional) `EdgeProvenance` re-export only; no trait change in Phase 1.

Tests:
- `tests/rdf/projection_test.rs` (new integration test file, if integration style used)
- Existing ladybug tests must still pass.

---

## Task 1: Edge provenance columns — schema + migration

**Files:** `src/store/ladybug.rs` (init, ~1780-1810).

**Step 1: Update CREATE REL TABLE statements to include provenance columns.**

Change:
```rust
let create_rel_table = "CREATE REL TABLE RELATES_TO (FROM Memory TO Memory)";
```
to:
```rust
let create_rel_table = "CREATE REL TABLE RELATES_TO (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING)";
```
Same for CONTAINS, GOVERNS, EXTRACTED_FROM, DEPENDS_ON.

**Step 2: Add idempotent migration for existing DBs.**

After the CREATE statements (still inside the init closure, before `Ok(())`), add:
```rust
const EDGE_PROVENANCE_COLUMNS: [&str; 5] = ["source", "valid_from", "valid_to", "generation", "extractor"];
const EDGE_TABLES: [&str; 5] = ["RELATES_TO", "CONTAINS", "GOVERNS", "EXTRACTED_FROM", "DEPENDS_ON"];
for table in EDGE_TABLES {
    for col in EDGE_PROVENANCE_COLUMNS {
        let ty = match col {
            "source" | "extractor" => "STRING",
            _ => "INT64",
        };
        let sql = format!("ALTER TABLE {} ADD {} {}", table, col, ty);
        if let Err(e) = conn.query(&sql) {
            let msg = e.to_string();
            if !msg.contains("already has property") && !msg.contains("already exists") {
                // Do not fail init on a migration error for one column; log and continue.
                eprintln!("[neurostrata:schema] edge provenance migration failed for {}.{}: {}", table, col, msg);
            }
        }
    }
}
```

Note: `ALTER TABLE ... ADD col TYPE` is the supported syntax (verified against
lbug 0.20.4). Existing edges read null for all five columns until backfilled.

**Step 3: Tests.**

- Fresh DB: init creates tables with the five columns (query `MATCH (a)-[r:GOVERNS]->(b) RETURN r.source` does not error even with no rows).
- Existing DB: create tables without columns, close, reopen with new code, columns appear.

Add a unit-ish test in `src/store/ladybug.rs` `#[cfg(test)]` or integration test. Prefer integration test under `tests/` if the harness supports async LadybugStore.

**Commit:** `feat(store): edge provenance columns on relation tables`

---

## Task 2: Write provenance on upsert edge MERGE

**Files:** `src/store/ladybug.rs` (upsert edge loop, ~2023).

**Step 1: Build provenance values.**

Before the edge loop, compute:
```rust
let now = chrono::Utc::now().timestamp();
let source = escape_ladybug_string(&namespace);
// extractor: "upsert" for now; future: caller-supplied.
let extractor = "upsert";
let generation = 0i64; // upsert is not generation-scoped; ingest uses relink.
```

**Step 2: Change edge MERGE to set provenance.**

Change:
```rust
let edge_query = format!(
    "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[:{}]->(b)",
    from, to, edge.rel_type
);
```
to:
```rust
let edge_query = format!(
    "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[r:{}]->(b) ON CREATE SET r.source = '{}', r.valid_from = {}, r.valid_to = 0, r.generation = {}, r.extractor = '{}' ON MATCH SET r.source = '{}', r.valid_from = {}, r.extractor = '{}'",
    from, to, edge.rel_type,
    source, now, generation, extractor,
    source, now, extractor
);
```

Keep `valid_to = 0` meaning "not expired" (consistent with existing 0 = none).

**Step 3: Tests.**

- Upsert a memory with `metadata.governs = ["target"]` where target exists; query edge and assert `source`, `valid_from > 0`, `extractor = 'upsert'`.
- Re-upsert same edge; `valid_from` updates (ON MATCH SET).

**Commit:** `feat(store): write provenance on upsert edges`

---

## Task 3: Write provenance on relink

**Files:** `src/store/ladybug.rs` (`create_missing_edges`, ~1710).

**Step 1: Thread provenance into `create_missing_edges`.**

Change signature:
```rust
fn create_missing_edges(
    conn: &Connection,
    rel_type: &str,
    pairs: &[(String, String)],
    namespace: &str,
    generation: u64,
) -> Result<usize>
```

Inside, the CREATE statement becomes:
```rust
"CREATE (x)-[:{rel} {source: '{ns}', valid_from: {now}, valid_to: 0, generation: {gen}, extractor: 'relink'}]->(y)"
```
where `{now}` is `chrono::Utc::now().timestamp()` captured at the start of
`create_missing_edges`.

**Step 2: Update caller in `relink_edges`.**

Pass `namespace` and `active_gen`:
```rust
created += create_missing_edges(conn, rel_type, pairs, &namespace_owned, active_gen)?;
```

**Step 3: Tests.**

- Run `relink_edges` on a namespace with declared edges; query edge and assert
  `extractor = 'relink'`, `generation = active_gen`.

**Commit:** `feat(store): provenance on relinked edges`

---

## Task 4: RDF module skeleton + types

**Files:** create `src/rdf/mod.rs`, `src/rdf/types.rs`, `src/rdf/namespace.rs`.

**`src/rdf/types.rs`:**
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuadObject {
    Iri(String),
    Literal { value: String, datatype: Option<String>, language: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quad {
    pub subject: String,
    pub predicate: String,
    pub object: QuadObject,
    pub graph: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeProvenance {
    pub source: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub generation: Option<i64>,
    pub extractor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier { Global, Domain, Task }
```

**`src/rdf/namespace.rs`:**
```rust
pub fn memory_iri(id: &str, concept_iri: Option<&str>) -> String { ... }
pub fn graph_iri(namespace: &str, tier: Tier, generation: u64) -> String { ... }
pub fn assertion_iri(namespace: &str, id: &str, version: u64) -> String { ... }
```
- `memory_iri`: use `concept_iri` if present, else `urn:neurostrata:memory:<id>`.
- `graph_iri`: `urn:neurostrata:graph:<namespace>:<tier>:gen:<generation>`.
- `assertion_iri`: `urn:neurostrata:assertion:<namespace>:<id>:<version>`.

**`src/rdf/mod.rs`:**
```rust
pub mod namespace;
pub mod projection;
pub mod types;
pub use namespace::*;
pub use projection::*;
pub use types::*;
```

**`src/main.rs`:** add `mod rdf;`.

**Tests:** unit tests for IRI helpers (pure, no DB).

**Commit:** `feat(rdf): module skeleton, types, namespace helpers`

---

## Task 5: RDF projection (rows → quads)

**Files:** create `src/rdf/projection.rs`.

**API:**
```rust
pub fn memory_to_quads(
    id: &str,
    payload: &crate::traits::MemoryPayload,
    namespace: &str,
    tier: Tier,
    generation: u64,
) -> Vec<Quad>;

pub fn edge_to_quad(
    from: &str,
    rel_type: &str,
    to: &str,
    provenance: Option<EdgeProvenance>,
    graph: Option<String>,
) -> Quad;

pub fn rows_to_quads(
    rows: &[(String, crate::traits::MemoryPayload)],
    namespace: &str,
    tier: Tier,
    generation: u64,
    edges: &[(String, String, String, Option<EdgeProvenance>)], // (from, rel, to, prov)
) -> Vec<Quad>;
```

**Projection rules (from spec §3.1):**
- Subject = `memory_iri(id, payload.metadata.get("concept_iri"))`.
- One quad per vocabulary field present in payload: `content`, `user_id`, `memory_type`, `agent_name`, `location`, `location_lines`, `metadata` (as JSON literal) → predicate `ns:<field>` (use `http://neurostrata.org/ns#` prefix? Use `urn:neurostrata:ns#` to avoid external dependency).
- Metadata sub-fields: project `metadata.domain`, `metadata.facets`, `metadata.source`, `metadata.topology.*` as individual predicates when present.
- Edges: one quad per edge with predicate `ns:<relType>` (lowercased first letter? use camelCase: `governs`, `contains`, `relatesTo`, `extractedFrom`, `dependsOn`).

**Graph:** all quads get `graph = Some(graph_iri(namespace, tier, generation))`.

**Tests (unit, no DB):**
- A memory with content + user_id projects to at least 2 quads with correct subject/predicate/object.
- `concept_iri` overrides subject.
- Edge with provenance produces a quad (edge quad carries provenance? Phase 1: provenance is a separate concern; edge quad object is the target IRI; provenance not in quad yet — that's Phase 2/RDF-star).
- `rows_to_quads` combines node + edge quads.

**Commit:** `feat(rdf): projection from rows to quads`

---

## Task 6: LadybugStore integration — project a namespace

**Files:** `src/store/ladybug.rs`.

Add a method (not on trait yet — keep Phase 1 store-specific):
```rust
impl LadybugStore {
    pub async fn project_namespace_rdf(
        &self,
        namespace: &str,
        tier: crate::rdf::Tier,
        generation: u64,
    ) -> Result<Vec<crate::rdf::Quad>> { ... }
}
```
Implementation:
1. `let memories = self.list(namespace, None, None).await?;`
2. For each memory, call `memory_to_quads`.
3. Collect edges: query `MATCH (a:Memory)-[r]->(b:Memory) RETURN a.id, type(r), b.id, r.source, r.valid_from, r.valid_to, r.generation, r.extractor` and build `EdgeProvenance`.
4. Append `edge_to_quad` for each.
5. Return.

Note: `type(r)` in Cypher returns the relation type string. Verify lbug supports `type(r)` — test in the integration test; if not, query each of the 5 tables separately.

**Tests:** integration test (requires LadybugStore test harness). If the harness is heavy, defer to a manual smoke test and keep projection unit-tested. Prefer at least one integration test using the existing test setup pattern.

**Commit:** `feat(store): project namespace to RDF quads`

---

## Task 7: Backfill existing edges

**Files:** `src/store/ladybug.rs`.

Add a one-time helper (not auto-run; callable):
```rust
pub async fn backfill_edge_provenance(&self, namespace: &str) -> Result<usize>
```
Implementation: run `relink_edges(namespace, "")` (or a dedicated pass) so existing edges get provenance. Actually relink already re-creates edges with provenance now (Task 3). So backfill = `relink_edges`. Document this in the plan; no new code needed beyond ensuring relink writes provenance.

**Tests:** covered by Task 3.

**Commit:** none (documentation only).

---

## Verification

Run:
```bash
cargo test --package neurostrata 2>&1 | tail -30
cargo clippy --all-targets 2>&1 | tail -20
```
All existing tests must pass. New tests: projection unit tests + edge provenance integration tests.

## Rollback

- Schema migration is additive (ALTER ADD). Rolling back code leaves extra columns in DB, which are harmless (old code ignores them).
- RDF module is new; removing it reverts to pre-Phase-1 state.
- No data loss.

## Out of scope (next plans)

- OWL 2 RL reasoning, SHACL validation, domain/alias expansion, entity canonicalization, named-graph switching logic, SPARQL, community detection, KGE.
- `VectorStore` trait changes (Phase 2+).
- Mesh sharing of RDF quads.