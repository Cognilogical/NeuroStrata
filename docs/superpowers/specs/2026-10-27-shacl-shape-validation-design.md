# SHACL Shape Validation Design

> Status: draft — pending Argon review.
> Task: `neurostrata-7zvo` (SHACL shape validation + AI constraint inference, OxiRS §1.3).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-27-owl-rdfs-reasoning-design.md`.

## 1. Problem

NeuroStrata has hand-written, soft shape checks for edge declarations, access topology, and failed attempts. These checks live in `src/store/ladybug.rs` and are not expressed as reusable shapes. There is no shared shape model for `Memory` rows, metadata, relationships, or bi-temporal fields, and no way to validate imported mesh records or exported RDF against the same rules. At the same time, automatically inferring constraints would institutionalize drift, so shape validation must be explicit, versioned, and warn-first.

## 2. Goal

Add **deterministic SHACL Core validation** as a shared layer over the RDF projection of LadybugDB:

- validate `Memory` rows and their edges at write time,
- validate imported SynapticMesh records and canonical RDF payloads,
- validate exported RDF when requested,
- emit structured warnings instead of rejecting writes,
- keep all shapes in versioned RDF/Turtle files.

## 3. Constraints

- LadybugDB remains the sole store.
- Validation is warn-first: malformed data is reported and still written.
- No LLM in core validation, search, dedup, guard, or routing.
- SHACL profile is Core only; SHACL-SPARQL is optional and must not require a SPARQL engine in the core path.
- Shapes are data, not code: versioned, reviewable, and loaded at runtime.
- AI-assisted constraint inference is never auto-applied; it produces proposals for human review.
- Deterministic output: stable shape order, stable result order, reproducible reports.

## 3.1 Library evaluation

Prefer existing crates over an in-house SHACL engine:

- `shacl` (rudof) for an established Rust SHACL validator with Native mode and optional SPARQL mode.
- `shacl-rust` (ensaremirerol) as an alternative if its `oxigraph` dependency and feature set fit the build better.
- `shacl-rs` (`shacl-core` only) if a backend-independent implementation is preferred; it implements `RdfGraph` without `oxigraph`.
- `oxirs-shacl` and its dependency set (`oxrdf`, `oxsdatatypes`, `spargebra`, etc.) as a lift-and-shift/vendor candidate if direct dependency hygiene permits; prefer vendoring selected Apache-2.0 modules over adding `oxirs-core`.
- Evaluate all candidates against the required constraint set and report bundle size before implementation.
- Avoid direct `oxirs-core` dependencies due to ctor pre-main rustls hooks, `scirs2` bloat, and dependency hygiene.
- Do not implement SHACL Core in-house unless no candidate passes the evaluation.

## 4. Scope

In scope:

- SHACL Core shapes for `Memory`, metadata, topology, attempt, edges, bi-temporal fields, and mesh record envelopes.
- Write-time validation in `upsert` and `relink_edges`.
- Import-time validation of canonical RDF payloads.
- Export-time validation when explicitly requested.
- Structured validation reports and warnings.
- Versioned shape files and shape loading.

Out of scope:

- AI-assisted constraint inference and auto-applying inferred constraints.
- SHACL-SPARQL in the default core path.
- Replacing hand-written closed-world checks until parity is demonstrated.
- Full SHACL-AF or advanced rule execution.

## 5. Approach

### 5.1 Shape model

Shape files are embedded with `include_str!` and may be overridden by a configured directory path. Runtime loading from a source-tree-relative path is not sufficient for the installed binary.

Shapes are stored as Turtle under `src/schemas/shacl/`:

- `memory-shapes.v1.ttl`
- `topology-shapes.v1.ttl`
- `attempt-shapes.v1.ttl`
- `edge-shapes.v1.ttl`
- `mesh-record-shapes.v1.ttl`

Each shape file declares:

- target classes for `Memory`, `Fact`, `Rule`, `Procedure`, `Task`, and mesh records,
- property constraints for required and optional fields,
- datatype and cardinality constraints,
- closed-world `sh:closed` only where explicitly desired,
- `sh:message` text suitable for user-facing warnings.

### 5.1a Projection alignment

`src/rdf/projection.rs` must emit the predicates and datatypes required by SHACL shapes:

- subject: strictly `urn:neurostrata:memory:<id>`; `metadata.concept_iri`, when present, is emitted as `ns:conceptIri`,
- `rdf:type` for `Memory`, `Fact`, `Rule`, `Procedure`, and `Task`,
- memory fields as typed predicates rather than opaque JSON literals (`content`, `user_id`, `memory_type`, `agent_name`, `location`, `location_lines`, `valid_from`, `valid_to`, `feedback_score`, `node_set`),
- metadata fields as first-class predicates where shapes need to inspect them (`metadata.topology.*`, `metadata.attempt.*`, `metadata.governs`, `metadata.contained_by`, `metadata.related_to`, `metadata.extracted_from`),
- edge declarations as parsed target strings or target stubs so edge shapes can validate syntax before resolution.

SHACL shapes must not attempt to inspect `ldp#JSON` literals.

### 5.2 Validation modes

Three modes are supported:

- `write`: validate a `MemoryPayload` before upsert and attach warnings to the write result.
- `import`: validate a canonical RDF payload or mesh record before application.
- `export`: validate an exported RDF dataset when the caller requests strict validation.

Default mode is `write`; `export` is opt-in.

### 5.3 Integration with LadybugDB

At `upsert` and `relink_edges`:

1. Build the RDF projection for the payload.
2. Run SHACL Core against the projected graph and loaded shapes.
3. Convert each violation to a `WriteShapeWarning`-compatible diagnostic.
4. Continue the write regardless of violations.
5. Append warnings to `UpsertReport.warnings`, extending `UpsertReport` with `warnings: Vec<WriteShapeWarning>`.
6. Store the validation result summary in `metadata.validation` only when `validation_record: true` is configured; by default do not mutate payload content. When enabled, strip `metadata.validation` from the canonical mesh wire profile so it does not alter canonical hashes or cross-instance exchange.

During transition, each field is validated by exactly one source: when SHACL is enabled for a field, the corresponding hand-written check is disabled for that field to avoid duplicate warnings. Topology and attempt shapes target only `Memory` rows whose `memory_type` is `fact`; on other types those metadata keys remain undeclared and accepted unchanged.

The existing hand-written checks remain as fast pre-checks until SHACL parity tests pass. They are then either retained as fallbacks or removed in a later task (neurostrata-rzda).

### 5.4 Import and export

- Import validation uses the same shapes as write validation.
- Invalid canonical payloads are handled according to the canonical migration spec: Phase 1 warns and applies valid JSON; Phase 2+ rejects invalid `rdf`.
- Export validation is available through `neurostrata_validate_rdf_export` and returns a structured report.
- `neurostrata_validate_canonical_rdf` validates the mesh wire profile specifically, including envelope fields, canonical hash, subject/graph pinning, and size limits.
- `neurostrata_validate_rdf_export` validates the local export profile.
- Edge shapes validate declaration syntax and cardinality at write/import time. Target-node class checks (for example `Memory -> Task` for `EXTRACTED_FROM`) run during `relink_edges` or import application, when target rows are available.

### 5.5 Reports

Validation reports use a stable JSON shape:

```json
{
  "conforms": false,
  "violations": [
    {
      "shape": "MemoryTopologyShape",
      "focus_node": "urn:neurostrata:memory:...",
      "path": "metadata.topology",
      "severity": "warning",
      "message": "..."
    }
  ]
}
```

- Severity is `warning` for write/import soft failures and `violation` for strict export checks.
- Results are sorted by shape id, focus node, and path.
- Reports never include private row content beyond identifiers already exposed by the source row.

### 5.6 AI constraint inference

AI-assisted inference is not part of the core daemon. If implemented later:

- it operates over anonymized or already-shared shape corpora,
- it produces candidate SHACL shapes as proposals,
- it requires human review before any shape file changes,
- it never modifies shapes automatically,
- it is tracked separately under `neurostrata-mhsp` or a dedicated task.

The core crate must not depend on an LLM client for validation.

### 5.7 Determinism and caps

- Load shapes once per namespace/configuration and cache them.
- Sort shapes and results before reporting.
- Cap validation time and result count per request.
- Because the selected Rust SHACL crates are synchronous, enforce the timeout by checking the budget between shapes and between focus nodes; return the partial report and a warning when the budget is exceeded.
- If a cap is exceeded, return partial results with a warning.

## 6. Interfaces

- Tools:
  - `neurostrata_validate_memory_shape`
  - `neurostrata_validate_rdf_export`
  - `neurostrata_validate_canonical_rdf` (wire-profile validator; registration shared with the canonical migration spec)
- Store hooks:
  - `validate_write_payload(payload) -> Vec<WriteShapeWarning>`
  - `validate_rdf_dataset(dataset, mode) -> ValidationReport`
- Configuration:
  - `shacl_enabled: bool` (default true)
  - `shacl_strict_export: bool` (default false)
  - `validation_record: bool` (default false)
  - `shacl_timeout_ms: u64` (default 50)
- Shape files under `src/schemas/shacl/`.

## 7. Edge cases

- Missing shape file: validation disabled with a warning.
- Unknown memory type: no shape match, no violation.
- Empty payload: validate required fields only.
- Private row: excluded from export validation unless audit mode is explicitly requested.
- Shape parse failure: report and disable validation for that file.
- Timeout: return partial results with a warning.

## 8. Testing

- Unit: each shape catches its intended malformed case.
- Unit: well-formed payloads produce no violations.
- Integration: `upsert` with malformed topology emits the same warning as today.
- Integration: import validation rejects invalid canonical payload in Phase 2+.
- Migration: hand-written checks and SHACL agree on a fixture corpus before cutover.
- Performance: validation stays within configured caps.

## 9. Acceptance

SHACL Core validation is available as a deterministic, warn-first layer over the RDF projection: write, import, and export paths can validate against versioned shapes; reports are structured and stable; existing soft checks remain compatible; no LLM is used in core validation.

## 10. Files touched (expected)

- `src/rdf/shacl.rs` (new): validator adapter and report conversion.
- `src/rdf/projection.rs`: typed predicates and `rdf:type` for SHACL targets.
- `src/store/ladybug.rs`: validation hooks in `upsert` and `relink_edges`.
- `src/schemas/shacl/*.ttl` (new): versioned shape files.
- `src/server.rs`: validation tools.
- `Cargo.toml`: selected SHACL crate dependency.