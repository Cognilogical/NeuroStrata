# Phase 2: Ontology Index, SHACL Core, Domain Expansion, Canonicalization

**Spec:** `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`

**Goal:** Add a compiled, deterministic ontology index (ArcSwap) with
subClassOf*/altLabel/subPropertyOf closure; expand domain filters through it;
add SHACL Core warn-first validation at write time; add entity canonicalization
for fact topology and KnownIds aliases.

**Architecture:** LadybugDB remains the sole store. A new `src/rdf/ontology.rs`
holds a pure-Rust `OntologyIndex` built from a namespace's registry row (v2
entries with parent/aliases/concept_iri). `LadybugStore` holds an
`Arc<ArcSwap<OntologyIndex>>` refreshed on demand. `search_with_domain`
expands input slugs through the index before filtering. SHACL validation runs
in `upsert` alongside existing shape warnings (warn-first). Canonicalization
normalizes topology entities and aliases via the index's altLabel map.

**Tech stack:** Rust, serde_json, arc-swap (new dep), chrono (existing).

**New dependency:** `arc-swap = "1"` (small, Apache-2.0/MIT).

---

## Task 1: OntologyIndex + closure (pure Rust)

**Files:** `src/rdf/ontology.rs` (new), `src/rdf/mod.rs` (export).

**Types:**
```rust
pub struct DomainEntry {
    pub slug: String,
    pub parent: Option<String>,
    pub aliases: Vec<String>,
    pub concept_iri: Option<String>,
}

pub struct OntologyIndex {
    pub domains: Vec<DomainEntry>,
    // children adjacency: parent -> Vec<child slug>
    children: HashMap<String, Vec<String>>,
    // alias (lowercased) -> canonical slug
    alias_map: HashMap<String, String>,
    // subPropertyOf: relation -> Vec<super-relation>
    sub_properties: HashMap<String, Vec<String>>,
}

impl OntologyIndex {
    pub fn empty() -> Self;
    pub fn from_registry_metadata(metadata: &Value) -> Self;
    // descendants of slug (including slug itself)
    pub fn expand_domain(&self, slug: &str) -> Vec<String>;
    // canonical slug for an alias or slug (None if unknown)
    pub fn canonical_slug(&self, raw: &str) -> Option<String>;
    // expand a relation to itself + super-properties
    pub fn expand_relation(&self, rel: &str) -> Vec<String>;
}
```

`from_registry_metadata` reads `registry_domain_entries` (v1 strings map to
{slug, parent: null, aliases: [], concept_iri: null}). Builds children adjacency
and alias map (alias lowercased -> slug). Cycle guard: if a cycle is detected,
drop the offending parent link (log warning).

`expand_domain`: BFS/DFS over children; returns sorted unique including self.
`canonical_slug`: normalize via `domain::normalize_domain_slug`, then if it's a
known slug return it; else look up in alias_map (lowercased raw).
`expand_relation`: returns [rel] + sub_properties[rel] (no transitive for now).

**Tests:** empty index expands a slug to itself; parent/child expansion; alias
resolution; cycle rejection.

**Commit:** `feat(rdf): ontology index with domain/alias/relation closure`

---

## Task 2: ArcSwap handle on LadybugStore

**Files:** `src/store/ladybug.rs`.

Add field to `LadybugStore`:
```rust
pub ontology: Arc<ArcSwap<crate::rdf::OntologyIndex>>,
```
Initialize in constructor (all three construction sites: `new`, `for_testing`,
and any other) to `Arc::new(ArcSwap::from_pointee(OntologyIndex::empty()))`.

Add method:
```rust
pub fn refresh_ontology(&self, namespace: &str) {
    // read registry row; build index; swap
}
```
Reads `store.get(namespace, registry_row_id(namespace))`, builds index from
metadata, swaps. Fail-soft: on error, keep current index.

**Tests:** construct store, refresh with no registry row → empty index; refresh
after writing a v2 registry row → index populated.

**Commit:** `feat(store): ArcSwap ontology handle on LadybugStore`

---

## Task 3: Domain expansion in search_with_domain

**Files:** `src/store/ladybug.rs`.

In `search_with_domain`, before computing `domain_keys`:
```rust
let mut expanded: HashSet<String> = HashSet::new();
for d in domains {
    if let Some(slug) = self.ontology.load().canonical_slug(d) {
        for e in self.ontology.load().expand_domain(&slug) {
            expanded.insert(e);
        }
    } else if let Some(s) = crate::domain::normalize_domain_slug(d) {
        expanded.insert(s);
    }
}
```
Use `expanded` for `domain_keys`. (Canonicalize aliases before expansion.)

**Tests:** existing domain tests still pass; new test: alias "authentication"
expands to "auth" + descendants.

**Commit:** `feat(store): domain/alias expansion in search_with_domain`

---

## Task 4: SHACL Core validator (warn-first)

**Files:** `src/rdf/shacl.rs` (new), `src/rdf/mod.rs` export.

```rust
pub struct ShaclViolation { pub field: String, pub why: String }

pub fn validate_memory(payload: &MemoryPayload) -> Vec<ShaclViolation>
```
Shapes (from spec §5):
- required: content, user_id, memory_type, location, location_lines, metadata
  (non-empty where applicable; memory_type non-empty).
- metadata.domain: string or string[].
- metadata.facets: string[].
- metadata.feedback_score: number in [-1,1] if present.
- metadata.topology: if present, object with the 5 string keys (reuse TOPOLOGY_KEYS).
- metadata.attempt: if present, object.
- Edge endpoint classes: not validated here (store-level).

Returns violations; never rejects.

Integrate in `upsert`: call `validate_memory`, convert each to `WriteShapeWarning`
(field, why, skipped="SHACL Core (warn-first)"), emit via existing
`emit_write_shape_warning`. This runs alongside `write_shape_warnings`.

**Tests:** valid payload → no violations; missing content → violation;
feedback_score out of range → violation.

**Commit:** `feat(rdf): SHACL Core warn-first validation`

---

## Task 5: Entity canonicalization

**Files:** `src/rdf/canonical.rs` (new), `src/rdf/mod.rs` export.

```rust
pub fn canonicalize_topology(
    topology: &mut serde_json::Value,
    index: &OntologyIndex,
);
pub fn canonicalize_known_ids_alias(raw: &str, index: &OntologyIndex) -> String;
```
- `canonicalize_topology`: for `component` and `target_system`, if the index
  has a canonical slug for the value, replace; else apply
  `canonical_entity_slug` (existing in domain.rs) as fallback.
- `canonicalize_known_ids_alias`: return canonical slug if known, else
  `canonical_entity_slug(raw)`.

Integrate in `upsert`: after computing payload, if metadata.topology is an
object, canonicalize component/target_system using the store's current ontology
index. Mutate a clone of payload before write.

Also infer facets: call `domain::compute_facets` and merge into metadata.facets
(if absent, set; if present, union). This is light Option A.

**Tests:** topology component "LadybugDB" with alias map → canonical; fallback
slug; facets inferred.

**Commit:** `feat(rdf): entity canonicalization + facet inference`

---

## Task 6: Registry v2 write support (optional, minimal)

**Files:** `src/registry.rs`.

Add:
```rust
pub fn registry_metadata_v2(entries: &[Value]) -> Value
```
producing `domain_registry.domains` as array of objects with slug/parent/
aliases/concept_iri, plus eligibility private.

Add `validate_domain_entries(entries: &[Value]) -> Result<(), String>` checking
slug normalization, no cycles (DAG), aliases non-empty.

Extend `registry_domain_entries` already supports v2; no change needed.

Do NOT change check_domain flow (still slug-only); v2 entries are written by
future `register_domain` tool. This task only adds helpers + tests.

**Tests:** v2 metadata round-trips through registry_domain_entries; cycle
detection rejects; slug normalization enforced.

**Commit:** `feat(registry): v2 metadata helpers + validation`

---

## Task 7: OntologyIndex from registry + refresh tests

**Files:** `src/store/ladybug.rs` tests.

Add integration test: write a v2 registry row (domains with parent/aliases),
call `refresh_ontology`, assert `expand_domain("auth")` includes "tls",
`canonical_slug("authentication")` == Some("auth").

**Commit:** included in Task 2 or separate test commit.

---

## Verification

```bash
cargo build
cargo test --bin neurostrata-mcp
```

All existing tests pass; new tests pass.

## Out of scope (later phases)

- SPARQL endpoint, community detection, KGE, mention linking, Tantivy.
- Materialized inferred edges, retraction (none needed — Option C).
- Full OWL 2 RL over arbitrary TBox (only domain/alias/subPropertyOf closure
  here; full TBox deferred to Phase 3 if needed).