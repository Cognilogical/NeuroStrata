# NeuroStrata RDF Semantic Layer (Approach 1: RDF Projection)

Date: 2026-10-27. Design only. No implementation authorized.

Status: draft for user review.

## 1. Purpose & scope

NeuroStrata is a general-purpose agent memory system. Its current storage is
LadybugDB (embedded property graph + vectors, Cypher). Its current data model is
JSON payloads in `Memory` rows with a hand-maintained vocabulary
(`src/schemas/memory-vocabulary.v3.json`, internal `vocabulary_version` 9) and
five typed relation tables with no edge properties.

This spec defines an **RDF semantic layer** built on top of LadybugDB, without
modifying LadybugDB's source. The goal is to give NeuroStrata RDF/OWL/SHACL
semantics (RDF 1.2 / RDF-star, OWL 2 RL reasoning, SHACL Core validation, named
graphs, domain/alias expansion, entity canonicalization) while preserving:

- Single store (LadybugDB).
- Pre-filtered vector search (row-class/tombstone/namespace/mesh-conflict
  filters before candidate budget).
- Bi-temporal memory (`valid_from`/`valid_to`).
- 3-tier namespace stratification + SynapticMesh privacy rules.
- The pre-push task gate.

**Approach chosen:** Approach 1 (RDF Projection Layer) — recommended by both
Argon and K3 in `docs/designs/2026-10-27-argon-k3-oxirs-verdict.md`. LadybugDB
stays the sole storage engine; a Rust semantic layer projects rows into RDF
quads on demand and computes reasoning/validation in Rust.

**Out of scope for this spec (named follow-ons):** SPARQL endpoint, community
detection (UI), KGE structural channel, entity/mention linking, Tantivy lexical,
federation, streaming delta, DID/signed records, ReBAC, WASM, TDB2 migration,
GraphQL, RAG chat. Interfaces for these are *defined* here so they compose
later; implementation is separate.

**Prerequisite (P0, separate):** resolve or formally retire the Laya stub
(`src/judgment/laya_onnx.rs:55,109`) per `AGENTS.md` §0. SynapticMesh v1
hygiene (oversized record cap, `origin_mesh_epoch` quarantine) also P0.

## 2. Architecture overview

```
┌─────────────────────────────────────────────────────────┐
│  MCP tools / daemon HTTP (search, related, add_memory, …) │
├─────────────────────────────────────────────────────────┤
│  Rust semantic layer (NEW: src/rdf/, src/ontology/)      │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐     │
│  │ RDF projection│ │ OWL 2 RL     │ │ SHACL Core   │     │
│  │ (rows→quads)  │ │ (compiled    │ │ (validate    │     │
│  │               │ │  closure,    │ │  warn-first)  │     │
│  │               │ │  query-time) │ │              │     │
│  └──────────────┘ └──────────────┘ └──────────────┘     │
│  ┌──────────────┐ ┌──────────────┐                      │
│  │ Ontology index│ │ Domain/alias  │                     │
│  │ (ArcSwap)     │ │ expansion     │                     │
│  └──────────────┘ └──────────────┘                      │
├─────────────────────────────────────────────────────────┤
│  LadybugDB (unchanged source, extended schema in our code)│
│  Memory rows + 5 relation tables (+ edge provenance cols)│
│  Vectors (HNSW), Cypher, WAL/checkpoint                  │
└─────────────────────────────────────────────────────────┘
```

Key invariants:
- LadybugDB is the sole persistent store. No second database.
- All RDF/OWL/SHACL computation is deterministic and offline (no LLM, no
  network). Remote LLM is an optional client concern, never in core.
- Reasoning is **hybrid (Option C + light Option A)**: write-time
  canonicalization + facets + SHACL validation; query-time compiled closure for
  transitive `subClassOf`/alias expansion. **No materialized inferred edges**
  in LadybugDB, so no retraction/DRed problem on supersede/archive.
- The compiled ontology index is held in `ArcSwap<OntologyIndex>` (read lock-free,
  swapped on registry mutation). Follows the OWL re-evaluation's Option C.

## 3. Data model

### 3.1 RDF projection (rows → quads)

A `Memory` row projects to a set of RDF triples/quads under the
`urn:neurostrata:` namespace:

- Subject: `urn:neurostrata:memory:<id>` (or the memory's own IRI if
  `metadata.concept_iri` is set, for cross-namespace alignment).
- Predicates: one per vocabulary field (`content`, `user_id`, `memory_type`,
  `agent_name`, `location`, `location_lines`, `metadata.*`).
- Object: typed literal or IRI, depending on field.

Edges project as triples:
- `GOVERNS` → `ns:governs`
- `CONTAINS` → `ns:contains`
- `RELATES_TO` → `ns:relatesTo`
- `EXTRACTED_FROM` → `ns:extractedFrom`
- `DEPENDS_ON` → `ns:dependsOn`

**Named graphs** map NeuroStrata's stratification to RDF 1.2 named graphs:

```
urn:neurostrata:graph:<namespace>:<tier>:gen:<generation>
```

where `<tier>` ∈ {global, domain, task} and `<generation>` is the active
ingest generation (0 = none published). This replaces the current
string-splitting of scoped IDs (`src/store/ladybug.rs:1480-1500`) with an O(1)
graph-URI pointer flip and cleanly isolates private vs. mesh records.

Mesh-shared records additionally carry `metadata.concept_iri` stamped at export
time, so receivers map to their local slug without leaking private domain slugs
(`src/registry.rs:50-57`).

### 3.2 Edge provenance (schema change in our code, not upstream)

LadybugDB relation tables currently have zero property columns
(`src/store/ladybug.rs:1790-1804`). We extend them in our own schema (no
LadybugDB source change — LadybugDB supports edge properties):

```
CREATE REL TABLE GOVERNS (
  FROM Memory TO Memory,
  source STRING,
  valid_from INT64,
  valid_to INT64,
  generation INT64,
  extractor STRING
)
```

Same for CONTAINS, RELATES_TO, EXTRACTED_FROM, DEPENDS_ON.

Rationale: provenance and bi-temporal bounds travel with the edge, not the node.
This is the "edge evidence" gap from Cognee §1.3 and the OxiRS explainability
module. `SearchEvidence.path` (`src/traits.rs:90-106`) already carries transient
paths/explanations; edge properties populate them.

**Migration:** `relink_edges` (`src/traits.rs:339`) and staged generations
(`begin_generation`/`publish_generation`) must write edge properties. Existing
edges without properties (from pre-migration data) read as null and are
backfilled by a one-time `relink_edges` pass.

### 3.3 Assertion identity (K3 addition)

RDF-star syntax (`<< :rule1 ns:governs :fileA >>`) is a serialization convenience,
not an assertion model. Each memory's assertion gets a stable IRI:

```
urn:neurostrata:assertion:<namespace>:<id>:<version>
```

where `<version>` increments on supersede. This gives distinct assertion events
(PROV-O-aligned: `prov:wasGeneratedBy`, `prov:wasDerivedFrom`,
`prov:invalidatedAtTime`) and supports valid-time vs. transaction-time
semantics. RDF-star quoted triples are used as an optional *syntax* for
provenance, not as the identity model.

### 3.4 Vocabulary ontology (two-tier, per OWL re-evaluation §9)

- **TBox core:** compiled into the binary as a versioned JSON-LD context
  (`src/schemas/memory-ontology.v1.jsonld`, pinned alongside vocabulary version).
  Contains SKOS labels, `subClassOf` hierarchy, `subPropertyOf` mappings,
  `disjointWith`, `equivalentClass` (restricted).
- **ABox / instance data:** LadybugDB rows, projected to quads at read time.
- **Per-namespace extension:** `domain_registry` row (`src/registry.rs`) extended
  to carry `ontology_version`, `subClassOf`, `altLabels`, `exactMatch`. Private
  by default (`eligibility: private`); never travels mesh without promotion.

## 4. OWL 2 RL reasoning (hybrid C + light A)

**Profile: SKOS + OWL 2 RL (restricted) + closed-world shapes.** Never OWL 2 DL
(no tableau reasoner; no `horned-owl` LGPL dependency). `oxirs-rule` advertises
RDFS/OWL RL — use that, not OWL 2 DL.

**Light Option A (write-time):**
- At `upsert`, `relink_edges`, `publish_generation`: validate closed-world
  shapes (§5), canonicalize `fact.topology` entities and `KnownIds` aliases to
  slugs, infer class facets (`enforced-rule`, `failed-attempt`,
  `architecture-decision`) from metadata fields. Write results back to
  LadybugDB as metadata.
- Bi-temporal truth maintenance: tag inferred rows with supporting source IDs,
  or recompute per namespace during `relink_edges`. On supersede/archive, the
  supporting source is invalidated and facets are recomputed.

**Option C (query-time compiled closure):**
- Hold the compiled TBox + SKOS vocabulary in `ArcSwap<OntologyIndex>`.
- At read time (`search_with_domain`, `related`, `memories_for_file`):
  - Expand `domain` filters via `subClassOf*` closure.
  - Expand query identifiers via `altLabel` → canonical symbols/`KnownIds`.
  - Expand relation filters via `subPropertyOf`.
- Always retain the original query; on any failure, fall back to baseline
  (no expansion). Single store, deterministic, offline.
- **No materialized inferred edges** in LadybugDB. Inferences are computed in
  Rust and attached as transient `SearchEvidence` (existing seam). No retraction
  problem.

**Materialization rules (if any inferred edges are ever materialized):**
Named individuals only, no blank-node generation, fixed rule set, sorted
fixed-point output, derivation provenance, explicit closure and time caps.

**Transitive `GOVERNS`:** The OWL re-evaluation rejected a global
`GOVERNS ∘ CONTAINS → GOVERNS` property chain (§5 of that doc). Instead,
extend the Cypher traversal to 2-3 hops with explicit provenance in
`SearchEvidence`. The existing `govern_out_q` is one hop
(`src/store/ladybug.rs:251-259`); this spec extends it to bounded N hops
with the path recorded.

**Disjointness:** Only across soundly separated dimensions (structural vs.
normative; record role vs. artifact kind vs. lifecycle). `STRUCTURAL_MEMORY_TYPES`
(`src/store/ladybug.rs:1440-1446`) is a fixed set, so structural-vs-normative
disjointness is sound. But `memory_type` accepts arbitrary strings
(`src/schemas/memory-vocabulary.v3.json`), so role disjointness requires
separating dimensions first.

## 5. SHACL Core validation (warn-first)

Adopt `rudof_lib`/`shacl` (Apache-2.0) or closed-world Rust validators.
Shapes for `Memory` rows and the 5 relation tables:

- Required fields: `content`, `user_id`, `memory_type`, `location`,
  `location_lines`, `metadata`.
- Datatypes: `metadata.domain` is `string | string[]`; `metadata.node_set` is
  `string[]`; `metadata.facets` is `string[]`; `metadata.feedback_score` is
  number in [-1, 1].
- Edge endpoint classes: `GOVERNS`/`CONTAINS`/`RELATES_TO`/`EXTRACTED_FROM`/
  `DEPENDS_ON` all go `Memory → Memory`.
- Cardinality: `EXTRACTED_FROM` target is a task ID (single).

**Mode: warn-first** (match current behavior — log violations, don't reject
writes initially). Upgrade to reject after a measurement period. This matches
the OWL re-evaluation's "warn-first, not reject" for closed-world checks.

`oxirs-shacl-ai` (AI constraint inference) is **not adopted** — auto-inferred
constraints from noisy agent memories institutionalize drift.

## 6. Bi-temporal memory & truth maintenance

- `valid_from`/`valid_to` Unix timestamps on `Memory` rows (existing).
- On `neurostrata_supersede_memory`: set `valid_to = now` on old row, create
  new row with `valid_from = now`. Search filters expired rows.
- On `neurostrata_archive_memory`: set `metadata.archived = true` (tombstone,
  strict literal `true` only).
- **Truth maintenance:** because no inferred edges are materialized (Option C),
  there is no retraction problem for inferences. Canonicalization/facets
  (light Option A) are recomputed per namespace during `relink_edges` on
  supersede/archive, tagged with supporting source IDs so stale results are
  detectable.
- `SearchEvidence` already carries transient paths/explanations
  (`src/traits.rs:66-104`); ontology-derived evidence fits this seam without
  mutating payloads.

## 7. Domain & alias expansion

Upgrade `domain_registry` (`src/registry.rs`) from flat `Vec<String>` to a DAG
with `parent` and `aliases`:

```json
{
  "domain_registry": {
    "version": 2,
    "domains": [
      { "slug": "auth", "parent": null, "aliases": ["authentication", "authn"] },
      { "slug": "tls", "parent": "auth", "aliases": ["ssl", "mtls"] }
    ]
  }
}
```

- `search_with_domain` expands `domain="auth"` → `{"auth", "tls", "mtls", ...}`
  via `subClassOf*` closure.
- Query terms expand via `altLabel` → canonical symbols/`KnownIds`.
- Relation filters expand via `subPropertyOf`.
- Cycle rejection (DAG check) and slug normalization via `normalize_domain_slug`
  (`src/domain.rs`) on registration.
- `concept_iri` stamping on mesh export for cross-namespace alignment (opt-in,
  private slugs stay private).

## 8. Entity canonicalization

Normalize `fact.topology.{component, target_system}` and `KnownIds` aliases to
one slug per concept, stored as plain strings. Stops `lbug` vs `LadybugDB` from
splitting recall. Uses `skos:exactMatch`/`closeMatch` (cautious) for
cross-vocabulary alignment without merging memory rows.

## 9. Data migration strategy

**Phase 1 — Dual-write (JSON + RDF projection):**
- Keep JSON vocabulary as source of truth.
- Add RDF projection layer (read-time, from JSON rows). No storage change.
- Add edge provenance columns to relation tables (schema migration in our code).
- Backfill existing edges via one-time `relink_edges` pass.

**Phase 2 — Shadow reads:**
- Run both JSON-path and RDF-path retrieval in parallel on a sample of queries.
- Compare results (Recall@5, MRR, latency). Log discrepancies.
- Gate: RDF path must match or beat JSON path on the 60-query labeled set
  (per OWL re-evaluation §10) before proceeding.

**Phase 3 — Cutover:**
- Switch default read path to RDF projection.
- JSON vocabulary becomes a compatibility view (still readable, no longer
  authoritative).
- Keep rollback: flip a config flag to revert to JSON path.

**Phase 4 — JSON retirement (future, separate decision):**
- Remove JSON vocabulary; RDF ontology becomes canonical.
- Requires owner approval; not part of this spec.

**Rollback:** At any phase, flip `rdf_projection_enabled = false` in config to
revert to JSON path. No data loss — LadybugDB rows are unchanged throughout
(phases 1-3 are read-path changes, not storage changes).

## 10. Interfaces

### New MCP tools (future, not this spec)
- `neurostrata_sparql_query` (P2, separate spec) — read-only, local-first,
  namespace-constrained, budgeted.
- `neurostrata_ontology_register_domain` (P1) — register domain with
  `parent`/`aliases`, DAG check, slug normalization.

### Changed tools
- `neurostrata_search_memory` — accepts `domain` (already soft filter);
  internally expands via `subClassOf*` closure when ontology is loaded.
- `neurostrata_add_memory` — canonicalizes `fact.topology` entities;
  validates SHACL shapes (warn-first).
- `neurostrata_ingest_directory` — canonicalizes symbol names; stamps
  `concept_iri` on mesh-exportable rows.

### Changed traits
- `VectorStore::search_with_domain` (`src/traits.rs:432`) — domain expansion
  happens inside, transparent to callers.
- `VectorStore::relink_edges` (`src/traits.rs:339`) — writes edge provenance
  columns; recomputes canonicalization/facets.

### New internal modules
- `src/rdf/projection.rs` — rows → quads.
- `src/rdf/ontology.rs` — `OntologyIndex`, `ArcSwap`, closure computation.
- `src/rdf/shacl.rs` — shape validation (warn-first).
- `src/rdf/canonical.rs` — entity canonicalization.

## 11. Testing strategy

- **Unit tests:** OWL 2 RL closure correctness (fixed-point, sorted output,
  derivation provenance), SHACL shape validation (valid/invalid rows), domain
  expansion (cycle rejection, alias resolution), canonicalization (slug
  normalization).
- **Integration tests:** dual-write parity (JSON path vs RDF path Recall@5 on
  60-query labeled set), edge provenance round-trip, bi-temporal filtering
  with supersede, named-graph isolation.
- **Regression:** existing 832+ cargo tests must pass; `search_with_domain`
  fallback behavior preserved (`src/store/ladybug.rs:147-211`).
- **Property-based:** OWL closure is idempotent (running twice = same result);
  SHACL validation is deterministic.
- **Performance:** p95 added latency ≤ 25 ms cached for ontology expansion;
  ≤ 10% end-to-end overhead. Derived storage < 5% of namespace.

## 12. Risks & governance

| Risk | Mitigation |
|---|---|
| OWL open-world vs. closed-world validation | SHACL/Core for validation (closed-world); OWL only for inference (open-world, but scoped to RL). `rdfs:domain`/`range` are inference-only, never validation. |
| `owl:sameAs` identity collapse | Restricted to reviewed individual identity only; never synonym substitution (`altLabel` handles those). |
| Free text opaque to reasoner | Ontology operates on metadata/AST symbols only, not `MemoryPayload.content`. |
| Bi-temporal phantom truths | No materialized inferred edges (Option C); canonicalization/facets recomputed on `relink_edges`. |
| Second-store temptation | Single-store rule enforced; no Oxigraph/TDB2 in this spec. |
| Governance cost | Two-tier ontology (core compiled, per-namespace extension); additive aliases are minor; changed equivalence/disjointness/transitivity requires major version + rematerialization + regression. |
| Licensing | Apache-2.0/MIT allowlist in `cargo-deny`; no `horned-owl` (LGPL); no `rustdl`. |
| Crate bloat | Use `oxrdf`/`oxttl`/`oxjsonld` (small, focused) + `reasonable` (RL engine) + `rudof_lib`/`shacl`. Do not depend on `oxirs-core` umbrella (ctor/rustls/scirs2). |

## 13. Out of scope (named follow-ons)

- SPARQL 1.1/1.2 endpoint (P2, separate spec).
- Community detection / hierarchical summaries (P3, UI-first).
- KGE structural channel (P2, offline experiment).
- Entity/mention linking (P2, additive).
- Tantivy lexical (P2, after benchmarking F1).
- Federation (P3).
- Streaming delta / RDF Patch (P3).
- DID / signed records (P3).
- ReBAC (P3).
- WASM (P3, deferred).
- TDB2 storage migration (P3, benchmark-gated).
- GraphQL auto-schema (killed from core).
- RAG chat (separate optional client).
- IoT/physics/digital twins (out of domain).

## 14. Decision requested

Approve Approach 1 (RDF projection layer) and this spec's scope, or request
changes. On approval, I'll invoke `writing-plans` to create the implementation
plan, starting with Phase 1 (dual-write RDF projection + edge provenance
columns) as the first sub-project.

---
*Design only. No implementation authorized by this document.*