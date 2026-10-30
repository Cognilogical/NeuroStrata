# OWL/RDFS Reasoning over Memory Graph Design

> Status: **APPROVED** (2026-10-28).
> Task: `neurostrata-2hxl` (OWL/RDFS reasoning over memory graph, OxiRS §1.2).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md` (§4, §3.4), `docs/superpowers/specs/2026-10-27-rdf-canonical-direction-design.md` (§2 Decision: JSON primary + RDF canonical transition), `docs/superpowers/specs/2026-10-27-closure-budget-zstd-anomaly-design.md` (`yjah`, QueryBudget and transitive closure primitives).

## 1. Problem

NeuroStrata currently uses hand-coded bounded traversal for graph relationships such as `GOVERNS` and domain expansion. The RDF semantic layer already defines an `OntologyIndex` and hybrid reasoning, but the reasoning rules are not yet specified as a deterministic, library-backed component. The system needs RDFS and restricted OWL 2 RL reasoning over the projected memory graph without materializing inferred edges or introducing a tableau reasoner.

## 2. Goal

Add **deterministic, query-time RDFS/OWL 2 RL reasoning** over the RDF projection of LadybugDB:

- expand domain filters via `subClassOf*`,
- expand relation filters via `subPropertyOf`,
- canonicalize aliases and `KnownIds` via `altLabel` and `exactMatch`,
- compute transient inferred paths for search and `related`,
- keep all inferred results transient and attached to `SearchEvidence`,
- never materialize inferred edges in LadybugDB.

## 3. Constraints

- LadybugDB remains the sole store.
- Profile is SKOS + OWL 2 RL (restricted). OWL 2 DL tableau reasoning is prohibited.
- No LLM anywhere in this path.
- Deterministic output: fixed input order, stable rule application, reproducible results.
- No materialized inferred edges; inferred results are computed in Rust and attached transiently.
- Warn-first: if reasoning fails or times out, fall back to baseline traversal.
- The global `GOVERNS ∘ CONTAINS → GOVERNS` property chain remains rejected; transitive `GOVERNS` is bounded and evidence-backed.

## 3.1 Library evaluation

Prefer existing crates over an in-house rule engine:

- In-tree bounded BFS/DFS (via `QueryBudget` from `yjah` spec) for `subClassOf*`, `subPropertyOf*`, and bounded transitive `GOVERNS`. This is primary because it satisfies determinism and the crate-free `related.rs` / `context/` test-compilation contract (see `yjah` AC-3).
- In-tree bounded BFS/DFS (via `QueryBudget` from `yjah` spec) for `subClassOf*`, `subPropertyOf*`, and bounded transitive `GOVERNS`. This is primary.
- `oxrdf` for RDF terms, triples, and quads shared with the projection layer.
- `serde_json` for deterministic JSON-LD TBox loading.
- `petgraph` is optional backup only if BFS/DFS cannot satisfy reachability/cycle guarantees.
- `reasonable` and `sophia_reasoner` are evaluation-only; not added to Cargo.toml until a benchmark proves they meet determinism and the restricted profile.

Do not add `horned-owl` (LGPL) or any OWL 2 DL reasoner. Reject `oxirs-core`/`oxirs-shacl`/`scirs2` (pulls `ctor` pre-main rustls hooks; per `yjah` and federation specs).

### 3.2 Alignment with RDF canonical direction (neurostrata-9fnf)

- Direction confirmed: JSON remains primary for wire/mesh; RDF canonical layer (`oxrdf`/`oxttl`) is the long-term target via dual-write then JSON retirement.
- This spec builds against the RDF projection layer (`oxrdf::Dataset`), which `9fnf` confirms as canonical. TBox stored in `src/schemas/memory-ontology.v1.jsonld`.

## 4. Scope

In scope:

- RDFS `subClassOf`, `subPropertyOf`, `domain`, and `range` entailment.
- OWL 2 RL rules needed by NeuroStrata: `equivalentClass`, `equivalentProperty`, `inverseOf`, `symmetricProperty`, `transitiveProperty`, `sameAs` (restricted), and `hasValue`/`someValuesFrom` only where already represented by facets.
- SKOS `altLabel`, `prefLabel`, `broader`/`narrower`, and `exactMatch` expansion.
- Bounded transitive `GOVERNS` traversal with path evidence (BFS/DFS closure from `yjah` spec, `QueryBudget` applied per `related` and `search_with_domain`).
- `OntologyIndex` compiled closure and `ArcSwap` lifecycle.
- Domain, alias, and relation filter expansion in `search_with_domain`, `related`, and `memories_for_file`.
- Write-time facets: `enforced-rule`, `failed-attempt`, `architecture-decision` are derived from metadata predicates `rule_kind`, `attempt_status`, `decision_type`; inferred facets are marked `inferred: true` for bi-temporal retraction.

Out of scope:

- OWL 2 DL tableau reasoning.
- Blank-node generation.
- Materialized inferred edges.
- Cross-namespace reasoning beyond `concept_iri` alignment.
- LLM-based ontology inference.

## 5. Approach

### 5.0 Projection alignment

`src/rdf/projection.rs` must emit `rdf:type` and domain values as IRIs (for example `urn:neurostrata:class:...` and `urn:neurostrata:domain:...`) for ontology reasoning, not only `xsd:string` literals. String literals remain allowed for display fields. The TBox is stored in `src/schemas/memory-ontology.v1.jsonld`.

### 5.1 Ontology index

`OntologyIndex` (already in `src/rdf/ontology.rs`) is extended to hold:

- `subClassOf`: `BTreeMap<Iri, BTreeSet<Iri>>`,
- `subPropertyOf`: `BTreeMap<Iri, BTreeSet<Iri>>`,
- `equivalentClass`: `BTreeMap<Iri, BTreeSet<Iri>>`,
- `equivalentProperty`: `BTreeMap<Iri, BTreeSet<Iri>>`,
- `inverseOf`: `BTreeMap<Iri, BTreeSet<Iri>>`,
- `transitiveProperties`: `BTreeSet<Iri>`,
- `symmetricProperties`: `BTreeSet<Iri>`,
- `altLabels`: `BTreeMap<Iri, BTreeSet<String>>`,
- `exactMatch`: `BTreeMap<Iri, BTreeSet<Iri>>`,
- `broader`/`narrower`: `BTreeMap<Iri, BTreeSet<Iri>>`.

The index is compiled from:

- `src/schemas/memory-ontology.v1.jsonld`,
- per-namespace `domain_registry` ontology extensions,
- promoted mesh vocabulary.

It is held in a per-namespace map, `ArcSwap<BTreeMap<String, Arc<OntologyIndex>>>`, and rebuilt only on registry mutation, `relink_edges`, or mesh import. `LadybugStore` must not refresh or overwrite a global ontology slot during `search_with_domain`.

### 5.2 Rule engine

Primary closure is in-tree BFS/DFS (matching `yjah` spec §1.19), applied via `QueryBudget` for bounded traversal. `petgraph` is an optional backup. The compiled closure is stored in `OntologyIndex`, precomputed at index build time from the TBox + ABox using `serde_json` JSON-LD loading and `oxrdf` terms.

- project the relevant TBox and ABox quads into an `oxrdf` dataset at index build time,
- run the restricted rule set once per ontology/registry change,
- store the resulting closures in `OntologyIndex`,
- discard any inferred triples not used by search, `related`, or domain expansion.

`reasonable` and `sophia_reasoner` are not added to `Cargo.toml` unless a benchmark proves they meet determinism and the restricted profile; otherwise the in-tree closure is used.

### 5.3 Query-time closure

At read time:

1. Load the current namespace's `OntologyIndex` from the per-namespace map.
2. Expand the query domain via `subClassOf*` closure.
3. Expand query identifiers via `altLabel`, `prefLabel`, `exactMatch`, and `KnownIds`.
4. Expand relation filters via `subPropertyOf*`.
5. For `search_with_domain`, attach inferred paths to `SearchEvidence` using `EvidenceKind::GovernsNhop { hops }`, serialized as `governs_<hops>hop`; `Ord` is derived on `(hops, ...)`.
6. `related` and `memories_for_file` keep their existing crate-local evidence types (`EvidencePath`, `GraphMatchKind`) and do not import `SearchEvidence`; map them explicitly rather than conflating types.
7. If any step fails, fall back to the baseline traversal without expansion.

`governs_max_hops` applies to `search_inner` traversal. `related::MAX_HOPS` remains 2 unless a separate change explicitly raises it. No inferred edges are written to LadybugDB.

### 5.4 SubPropertyOf and fixed relation tables

LadybugDB has five fixed relation tables: `GOVERNS`, `CONTAINS`, `RELATES_TO`, `EXTRACTED_FROM`, and `DEPENDS_ON`. `subPropertyOf` expansion maps fine-grained RDF/metadata predicates up to those five canonical table names before Cypher traversal. It does not create new relation tables.

### 5.5 Bounded transitive GOVERNS

The rejected global property chain is not implemented. Instead:

- `govern_out_q` and `govern_in_q` are extended to bounded N hops (default 2, configurable 1..=5),
- each hop records its path in `SearchEvidence`,
- cycle detection prevents repeated nodes,
- a hard cap limits the number of paths per result.

This is a compiled traversal, not an OWL property chain.

### 5.6 Write-time canonicalization

At `upsert`, `relink_edges`, and `publish_generation`:

- canonicalize `fact.topology` entities and `KnownIds` aliases to slugs,
- infer class facets (`enforced-rule`, `failed-attempt`, `architecture-decision`) from metadata,
- validate closed-world shapes,
- write canonicalized metadata back to LadybugDB.

Inferred facets are stored as metadata, not as graph edges.

### 5.7 Determinism and caps

- Sort all quads before reasoning.
- Fix the rule order and input order.
- Cap reasoning time and output size per request.
- If a cap is exceeded, return partial, evidence-backed results and a warning.

## 6. Interfaces

- `OntologyIndex` methods:
  - `expand_domain(&str) -> Vec<String>`,
  - `canonical_slug(&str) -> Option<String>`,
  - `expand_relation(&str) -> Vec<String>`,
  - `subclasses_of(iri) -> BTreeSet<Iri>`,
  - `superclasses_of(iri) -> BTreeSet<Iri>`,
  - `subproperties_of(iri) -> BTreeSet<Iri>`,
  - `equivalents_of(iri) -> BTreeSet<Iri>`,
  - `inverse_of(iri) -> BTreeSet<Iri>`,
  - `is_transitive(iri) -> bool`,
  - `aliases_for(symbol) -> BTreeSet<String>`.
  - `closure_subclasses(root, budget) -> ClosureResult<String>` (in-tree BFS, per `yjah` QueryBudget; returns class IRIs).
  - `closure_governs(root, edges, budget) -> ClosureResult<String>` (bounded N hops; returns memory IRIs).
- `search_with_domain`, `related`, and `memories_for_file` consume these methods.
- Configuration:
  - `reasoning_enabled: bool` (default true),
  - `governs_max_hops: u8` (default 2, range 1..=5; aligns with `yjah` spec `related.rs` default),
  - `reasoning_timeout_ms: u64` (default 50).
  - `closure_budget: QueryBudget` (max_ms 50 / max_nodes 256, defaults from `src/rdf/reasoning.rs`).

## 7. Edge cases

- Missing ontology extension: use built-in TBox only.
- Unknown class or property: no expansion.
- Reasoner failure: fall back to baseline traversal and emit a warning.
- Cycle in class hierarchy: deduplicate and terminate.
- Timeout: return partial results with evidence and a warning.
- Empty graph: no inferences.

## 8. Testing

- Unit: closure of a small SKOS/OWL fixture.
- Unit: deterministic output across repeated runs.
- Integration: domain filter expansion returns expected memories.
- Integration: `search_with_domain` includes bounded transitive `GOVERNS` paths; `related` stays within its 2-hop cap.
- Unit: `related::MAX_HOPS` remains 2 unless a separate change explicitly raises it.
- Performance: reasoning and traversal stay within configured caps.
- Fallback: disabled reasoning matches baseline traversal.

## 9. Acceptance

RDFS/OWL 2 RL reasoning is available as a deterministic, query-time layer over the RDF projection: domain, alias, property, and bounded transitive expansions work; inferred paths are transient and evidence-backed; failures fall back safely; LadybugDB remains the sole store and no LLM is used.

## 10. Files touched (expected)

- `src/rdf/ontology.rs`: index fields and closure methods.
- `src/schemas/memory-ontology.v1.jsonld` (new): built-in TBox.
- `src/rdf/reasoning.rs` (new): reasoner adapter and deterministic wrappers.
- `src/store/ladybug.rs`: bounded `GOVERNS` traversal and filter expansion hooks.
- `src/server.rs`: configuration and tool exposure if needed.
- `Cargo.toml`: `oxrdf`, `petgraph` (optional backup); NO `oxirs-core`/`scirs2`/`horned-owl`/`reasonable`/`sophia_reasoner`/`oxttl` (unless later benchmark justifies).

## 11. Argon review resolutions (2026-10-28)

1. **Materialization scope:** This spec remains query-time only; tasks `2xe9`/`duzn` requiring upsert materialization belong to a separate task and must not be conflated with this spec.
2. **Library choice:** Primary implementation uses in-tree BFS/DFS for subClassOf*, subPropertyOf*, and bounded GOVERNS. `reasonable`/`sophia_reasoner` are evaluation-only; if they fail determinism or profile, fall back to in-tree closure. No tableau reasoner.
3. **JSON-LD parser:** TBox loader uses deterministic `serde_json` JSON-LD parsing for `src/schemas/memory-ontology.v1.jsonld`; `oxttl` is not required for JSON-LD.
4. **OntologyIndex directionality:** `sub_classes` maps child -> parents; `children` maps parent -> children. `closure_subclasses(root)` returns class IRIs, not MemoryIds.
5. **QueryBudget:** Reuse existing `src/rdf/reasoning.rs::QueryBudget { max_ms, max_nodes }` and `yjah` budgets; wall-clock deadline may only truncate, never alter result sets; hard caps determine truncation.
6. **EvidenceKind wire format:** `GovernsNhop { hops: u8 }` serializes as `governs_<hops>hop` for backward compatibility; `Ord` derived on `(hops, ...)`.
7. **Write-time facets:** `enforced-rule`, `failed-attempt`, `architecture-decision` are derived from metadata predicates `rule_kind`, `attempt_status`, `decision_type`; inferred facets are marked `inferred: true` for bi-temporal retraction.
