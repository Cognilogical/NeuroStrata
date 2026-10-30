# RDF-star Provenance for Memory Statements Design

> Status: **APPROVED** (2026-10-28).
> Task: `neurostrata-4gb2` (RDF-star provenance, OxiRS §1.18).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md` (§3.2 edge provenance, §3.3 assertion identity).

## 1. Problem

NeuroStrata stores provenance as transient `SearchEvidence` and as ad hoc metadata. LadybugDB edges currently have no property columns, so `source`, `valid_from`, `valid_to`, `generation`, and `extractor` cannot travel with a relation. RDF export (planned in the RDF semantic layer spec) therefore cannot answer who asserted a triple, when, and from what source without re-deriving it from node metadata. RDF-star gives a standard quoted-triple syntax for annotating statements while keeping LadybugDB as the sole store.

## 2. Goal

Add RDF-star provenance annotations to the RDF projection of memory rows and edges:

- every memory statement and every graph edge gets a stable assertion IRI,
- provenance metadata (source, extractor, generation, validity, confidence, origin) is stored on LadybugDB edges/nodes and projected as quoted-triple annotations,
- deterministic RDF-star export and provenance lookup are available without a second store,
- full SPARQL-star querying remains a separate spec but consumes this projection.

## 3. Constraints

- LadybugDB remains the sole store. Provenance is stored as edge/node properties and projected to RDF-star at read/export time.
- No LLM anywhere in this path.
- Deterministic output: sorted triples, stable assertion IRIs, fixed field order.
- Privacy: private rows and mesh-conflicting rows never export quoted triples to mesh; provenance fields are filtered by existing mesh rules.
- Warn-first: malformed provenance is reported by validation, not fatal to search or export.
- Backward compatible: pre-migration edges without properties read as unknown provenance, not as errors.

## 3.1 Library evaluation

Prefer existing crates over in-house parsing/serialization:

- `oxrdf` with feature `rdf-12` for RDF 1.2 terms, triples, quads, and triple terms.
- `oxttl` with feature `rdf-12` for Turtle-star, TriG-star, N-Triples-star, and N-Quads-star parsing and serialization (both crates require `rdf-12` for triple-term output).
- `sha2` for deterministic statement digests if an assertion IRI needs a content-derived suffix.
- Evaluate `oxirs-star` only in the SPARQL spec; it is a heavier dependency and is not required for projection/export.

Do not implement a custom RDF-star parser or serializer when `oxttl` and `oxrdf` already cover the format.

## 4. Scope

In scope:

- Assertion identity IRI per memory statement and per graph edge.
- Edge property schema for all 5 relation tables (`source`, `valid_from`, `valid_to`, `generation`, `extractor`, `confidence`, `origin`).
- Node-level provenance fields in `Memory.metadata` (`provenance.source`, `provenance.extractor`, `provenance.origin`, `provenance.confidence`).
- RDF-star projection and deterministic export (`neurostrata_export_rdf_star`).
- Provenance lookup (`neurostrata_provenance_for` for a memory or edge).
- Refresh of existing edges and rows during `relink_edges`.
- SHACL-style validation of provenance shapes (warn-first).

Out of scope:

- Full SPARQL-star query engine (separate spec).
- Reasoning over provenance (OWL/SHACL separate specs).
- Storing inferred provenance as facts.
- Cross-instance provenance federation (mesh stays git-batch).

## 5. Approach

### 5.1 Assertion identity

Each projected statement receives a stable assertion IRI. The assertion IRI is the subject of the provenance block; the quoted triple is linked to it with `rdf:reifies` (RDF 1.2 reifier syntax). This avoids collapsing multiple assertions of the same triple.

```
urn:neurostrata:assertion:<namespace>:memory:<memory_id>:<predicate_slug>:<object_discriminator>:<assertion_version>
urn:neurostrata:assertion:<namespace>:edge:<relation>:<from_id>:<to_id>:g<generation>:v<valid_from>[:<ordinal>]
```

- `<namespace>` and `<memory_id>` are percent-encoded (`%3A` for `:`, `%2F` for `/`).
- For a memory node statement, `<predicate_slug>` is the projected predicate. `<object_discriminator>` is `0` for single-valued predicates; for multi-valued predicates (`ns:domain`, `ns:facet`, `ns:conceptIri`, `ns:tag`), it is the zero-based index in the sorted object list (stable across runs). `<assertion_version>` is the active generation (or `valid_from` when generation is `0`).
- The memory id changes on supersede, so the new assertion IRI is distinct; `metadata.supersedes`/`metadata.superseded_by` are projected as `prov:wasDerivedFrom`/`prov:wasInvalidatedBy`.
- For an edge statement, `<relation>` is one of the 5 relation types. `<from_id>` and `<to_id>` are percent-encoded. `<ordinal>` is appended only when multiple edges of the same type connect the same node pair; it is the deterministic index in edge order sorted by `(generation ASC, valid_from ASC, extractor ASC, source ASC)` (not by id, since edges have no id column).
- `<assertion_version>` records audit/version information; it is not the sole uniqueness key.
- Assertion IRIs are projection identifiers, not LadybugDB primary keys.
- Re-upsert of `mention-detector` edges changes `valid_from` (new `edge_now`), producing a new assertion IRI; the prior assertion IRI remains queryable via `neurostrata_provenance_for({ target: "assertion", iri })` (see §5.5).

### 5.2 Edge property schema

Extend the 5 relation tables (`GOVERNS`, `CONTAINS`, `RELATES_TO`, `EXTRACTED_FROM`, `DEPENDS_ON`) with an 8th column `origin` (keeping the existing 7 `EDGE_PROVENANCE_COLUMNS`: `source`, `valid_from`, `valid_to`, `generation`, `extractor`, `confidence`, `evidence_id`):

- `source STRING` — source key or origin namespace.
- `valid_from INT64` — statement validity start (0 means unspecified).
- `valid_to INT64` — statement validity end (0 means current).
- `generation INT64` — ingest generation that created the edge.
- `extractor STRING` — extractor name or `manual`.
- `confidence DOUBLE` — 0.0..=1.0, default 1.0.
- `evidence_id STRING` — existing column (unchanged).
- `origin STRING` — `user`, `agent`, `system`, or `mesh` (new, 8th column via `ALTER TABLE ... ADD origin STRING`).

`EDGE_PROVENANCE_COLUMNS` in `src/store/ladybug.rs:2028-2056` and `EdgeProvenance` in `src/rdf/types.rs` are extended to 8. `project_namespace_rdf` (`ladybug.rs:10515`) is updated to `RETURN` all 8 columns. `upsert` and `create_missing_edges` write all 8; `valid_from` is set only on edge creation (`ON CREATE`), not overwritten on re-upsert. `origin` defaults to `system` and `confidence` defaults to `1.0`. `evidence_id` remains as-is (existing semantics).

### 5.3 Node provenance

`Memory.metadata.provenance` (optional) contains:

- `source`: source key or origin namespace.
- `extractor`: extractor name or `manual`.
- `origin`: `user`, `agent`, `system`, or `mesh`.
- `confidence`: 0.0..=1.0.

If absent, projection uses `metadata.source` (when present) and defaults `origin = system`, `confidence = 1.0`, `extractor = manual`. Precedence: `metadata.provenance.*` overrides legacy `metadata.source`; legacy `metadata.source` may be a string or `{kind, ref, captured_at}` object and is canonicalized to `provenance.source` during projection without mutating the row.

### 5.4 RDF-star projection

The assertion IRI is the subject of the provenance block. The quoted triple is attached with `rdf:reifies` (RDF 1.2 reifier syntax). The base triple is emitted separately so ordinary RDF consumers can read the graph.

```turtle
<urn:neurostrata:assertion:...> a ns:Assertion ;
  rdf:reifies <<( s p o )>> ;
  ns:source "..." ;
  ns:extractor "..." ;
  ns:origin "..." ;
  ns:confidence 1.0 ;
  ns:validFrom 0 ;
  ns:validTo 0 ;
  ns:generation 0 .
s p o .
```

The object of `rdf:reifies` is a triple term `<<( s p o )>>` (RDF 1.2 / `oxttl` with `rdf-12`), not reified-triple sugar `<< s p o ~ :r >>`. Implementers must test against `oxttl` output, not hand-written strings.

For an edge `s ns:relatesTo o`, the same annotation block is emitted using the edge's properties.

Serialization uses `oxrdf` (feature `rdf-12`) for RDF 1.2 terms and `oxttl` (feature `rdf-12`) for Turtle-star, TriG-star, N-Triples-star, or N-Quads-star output. Do not hand-format RDF-star syntax.

### 5.5 Export and lookup

- `neurostrata_export_rdf_star(namespace?, format?, flatten_graphs?)` returns deterministic output. Supported formats: `turtle-star`, `n-triples-star`, `trig-star`, and `n-quads-star`. Default is `trig-star` when named graphs are included; use `turtle-star` with `flatten_graphs = true` for a flat graph. Sort by assertion IRI, then subject, predicate, object.
- `neurostrata_provenance_for(request)` accepts structured fields rather than a string DSL:
  - `{ target: "memory", id, namespace? }`,
  - `{ target: "edge", from, to, relation, namespace? }`,
  - `{ target: "assertion", iri, namespace? }`.
- Both tools are read-only and never mutate LadybugDB.

### 5.6 Migration and refresh

- `relink_edges` and staged generation publish write edge properties.
- Existing edges without properties read as unknown provenance (`source = null`, `extractor = null`, `confidence = 1.0`, `origin = system`).
- No mutating backfill tool is provided. Backfilling current generation values onto historical edges would fabricate provenance.
- Active edges are refreshed naturally during `relink_edges`; read-time projection supplies defaults for missing properties.
- Companion spec `neurostrata-rdf-semantic-layer-design.md` §3.2 must be updated to remove the "one-time relink_edges backfill" claim and §3.3 to use the 4/5-segment assertion IRI grammar from §5.1 of this spec.

### 5.7 Validation

Warn-first SHACL-style checks:

- `confidence` in [0.0, 1.0].
- `origin` in the allowed set.
- `valid_to == 0` or `valid_to >= valid_from`.
- `source` and `extractor` are non-empty when present.
- Assertion IRI matches the namespace/id/version grammar.

Invalid provenance produces a warning in export/lookup output and a `guard_event`-style diagnostic, but does not block search, export, or mesh.

### 5.8a Privacy filtering of `ns:metadata`

- During `neurostrata_export_rdf_star`, the raw `ns:metadata` literal quad is NOT emitted (it may contain `absolute_path`, `edge_sources.file`, private fields). Only allowlisted provenance predicates are projected (`ns:source`, `ns:extractor`, `ns:origin`, `ns:confidence`, `ns:validFrom`, `ns:validTo`, `ns:generation`). `metadata` is projected per-predicate (as in SHACL spec §5.1a), never as an opaque JSON literal. Quoted-triple annotations never include private domain slugs or local filesystem paths; use `concept_iri` for cross-namespace alignment.

### 5.8 Privacy and mesh

- Export reuses the filter predicates from `export_graph` (`src/store/ladybug.rs`): retired, archived, mesh-conflicting, and registry rows are excluded.
- Mesh export excludes private provenance fields unless `eligibility: shareable` is explicitly set on the memory or edge.
- Quoted-triple annotations never include private domain slugs or local filesystem paths; use `concept_iri` for cross-namespace alignment.

## 6. Interfaces

- Extends the RDF semantic layer (no separate feature gate in this spec; implemented with the RDF projection).
- Tools:
  - `neurostrata_export_rdf_star`
  - `neurostrata_provenance_for`
- Store changes:
  - edge property columns on the 5 relation tables (`source`, `valid_from`, `valid_to`, `generation`, `extractor`, `confidence`, `origin`),
  - optional `metadata.provenance`,
  - `relink_edges`, `upsert`, and generation publish paths write properties.

## 7. Edge cases

- Pre-migration data: unknown provenance, no export failure.
- Missing edge property: default as above.
- Superseded memory: old assertion IRI remains queryable through `neurostrata_provenance_for`; new assertion gets a new IRI and links via `prov:wasDerivedFrom`.
- Archived/retired row: excluded from export; direct provenance lookup is allowed for audit.
- Mesh-conflicting row: excluded from mesh export.
- Invalid provenance: warning only.
- Empty namespace: empty export.

## 8. Testing

- Unit: assertion IRI grammar and deterministic sort.
- Unit: quoted-triple projection for a memory and an edge.
- Integration: export contains provenance annotations; lookup returns expected fields.
- Refresh: active edges receive properties during `relink_edges`; read-time defaults handle pre-migration rows.
- Validation: invalid confidence/origin/validity produces warnings, not failures.
- Privacy: private rows excluded from mesh-oriented export.

## 9. Acceptance

RDF-star provenance is available as a projection over LadybugDB: edges and memory statements carry assertion IRIs and provenance properties; export and lookup return deterministic RDF 1.2 reifier annotations through `oxrdf`/`oxttl`; active-edge refresh and read-time defaults handle pre-migration rows; invalid provenance is warn-first. LadybugDB remains the sole store and no LLM is used.

## 10. Files touched (expected)

- `src/rdf/provenance.rs` (new): assertion IRI, projection, export, lookup. Consolidates and replaces `src/rdf/rdf_star.rs` (in-house quoted-triple serializer, prohibited), `src/rdf/assertion.rs` (UUIDv7 assertion IRI, contradicts deterministic grammar), and `src/rdf/namespace.rs::assertion_iri` (3-segment form) — all three are retired/removed.
- `src/store/ladybug.rs`: edge property schema (8 columns), relink/generation writes, read-time defaults, `project_namespace_rdf` returns all 8 columns.
- `src/server.rs`: tool registration and handlers.
- `src/schemas`: provenance shape additions (if shapes are stored).
- `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`: align §3.2 (backfill wording) and §3.3 (assertion IRI grammar) with this spec.
