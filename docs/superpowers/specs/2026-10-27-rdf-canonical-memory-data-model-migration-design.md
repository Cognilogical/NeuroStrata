# RDF Canonical Memory Data Model Migration Design

> Status: draft — pending Argon review.
> Task: `neurostrata-z9e7` (RDF canonical memory data model migration, OxiRS §1.1).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`.

## 1. Problem

NeuroStrata's local store is LadybugDB and its mesh wire format is a JSON record (`synaptic-mesh-record/v1`). The RDF semantic layer projects LadybugDB rows into RDF on demand, but the mesh and export paths do not yet have a canonical RDF representation. This makes cross-instance exchange, canonicalization, diffing, and provenance annotation harder than necessary and leaves JSON as the only wire format.

## 2. Goal

Adopt RDF quads as the canonical representation for shared memory exchange and deterministic export, while keeping LadybugDB as the sole store:

- local storage remains LadybugDB (no second database),
- mesh records carry a canonical RDF payload alongside the existing JSON payload during migration,
- canonicalization is deterministic and offline,
- import validates and applies the canonical RDF payload,
- Canonical N-Quads snapshots are used for exchange; delta patching is handled by the separate streaming-delta spec.

## 3. Constraints

- LadybugDB remains the sole persistent store.
- No LLM in canonicalization, import, export, or mesh exchange.
- Deterministic output: sorted quads, stable IRIs, fixed serialization.
- Backward compatible: v1 JSON records remain readable throughout migration.
- Privacy: private rows, mesh-conflicting rows, and unshared memory types never enter canonical RDF exchange.
- Warn-first: malformed canonical payloads are reported and skipped, not fatal.

## 3.1 Library evaluation

Prefer existing crates over in-house parsing, canonicalization, or patch generation:

- `oxrdf` with features `rdf-12` (and `rdfc-10` only if blank-node canonicalization is later required) for RDF terms and quads.
- `oxttl` for N-Quads parsing and serialization.
- Evaluate `oxirs-ttl` in the separate streaming-delta spec (`neurostrata-grv3`), not in this migration spec.
- Do not embed `oxigraph` as a second store; use it only as an in-memory helper if a later benchmark justifies it.

Do not implement RDF canonicalization in-house when these crates cover the functionality.

## 4. Scope

In scope:

- Canonical quad mapping from a `Memory` row and its edges.
- Dual-write of JSON + RDF in mesh records.
- Import validation and application of canonical RDF payloads.
- Deterministic N-Quads export for a namespace or memory.
- Migration phases and compatibility gates.

Out of scope:

- Replacing LadybugDB with an RDF store.
- Full SPARQL query engine (separate spec).
- Reasoning over canonical quads (separate specs).
- Mesh federation protocol changes beyond record format.

## 5. Approach

### 5.1 Canonical quad mapping

A `Memory` row maps to quads under `urn:neurostrata:` using the RDF semantic layer projection. Two projection profiles are required:

- **Local export profile:** full `MemoryPayload` for local audit and export.
- **Mesh wire profile:** only the curated mesh envelope: `id`, `namespace`, `memory_type`, `lifecycle`, content or procedure definition, curated metadata (`eligibility`, `source`, `tags`, `concept_iri`), relationships, `update_id`, `origin_mesh_epoch`, and `trust`.

The mesh wire profile must not include raw `metadata`, file paths, `user_id`, `agent_name`, location fields, or procedure runtime counters.

Canonical mesh quads use:

- subject: strictly `urn:neurostrata:memory:<id>`,
- `metadata.concept_iri`, when present, as the object of `ns:conceptIri` (not as the subject),
- edges as typed predicates (`ns:governs`, `ns:contains`, `ns:relatesTo`, `ns:extractedFrom`, `ns:dependsOn`),
- named graph: `urn:neurostrata:graph:<namespace>:domain:gen:0`.

Because projected subjects, predicates, objects, and graphs are ground IRIs/literals, canonicalization is deterministic lexicographic sorting of N-Quads lines followed by SHA-256. Blank-node canonicalization is not required for this profile.

### 5.2 Mesh record format

Mesh records remain JSON envelopes for compatibility. In Phase 1 and Phase 2, keep `schema_version: "synaptic-mesh-record/v1"` and add an optional `rdf` object:

```json
"rdf": {
  "schema": "neurostrata-rdf-record/v1",
  "format": "n-quads",
  "canonical_hash": "sha256:...",
  "quads": "..."
}
```

- `quads` contains canonical N-Quads for the record's memory, metadata, and relationships.
- `canonical_hash` is computed from the N-Quads text.
- `rdf` is `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- v1 records without `rdf` remain valid.
- `schema_version` becomes `synaptic-mesh-record/v2` only in Phase 3, when legacy JSON fields may be omitted.

### 5.3 Dual-write phase

Phase 1 (dual-write):

- Publishers emit both the existing JSON fields and the canonical `rdf` payload.
- Importers validate the JSON record first, then validate the canonical payload when present.
- In Phase 1, if JSON is valid but `rdf` validation fails or mismatches, emit a warning/diagnostic and apply the valid JSON payload. Do not reject the record solely because its shadow RDF payload failed.
- If JSON and canonical payload disagree, report the mismatch as a diagnostic. Canonical payload is authoritative only in Phase 2.

Phase 2 (RDF-authoritative import):

- Importers apply rows from the canonical RDF payload, reconstructing `MemoryPayload`, metadata, lifecycle, and relationships.
- JSON fields remain for compatibility and fallback.
- Importers reject invalid `rdf` payloads.

Phase 3 (RDF-only records):

- Publishers may omit legacy JSON fields once all joined instances support v2 records.
- Importers must still accept v1 records for a documented compatibility window.

### 5.4 Outbox and deltas

- `OutboxEntry` does not gain an `rdf_patch` field in this spec.
- `worker::build_record` constructs the canonical `rdf` block deterministically from the `MeshRecord` fields at drain time.
- RDF Patch generation and application remain in the separate streaming-delta spec. This spec publishes full canonical N-Quads snapshots in mesh records.

### 5.5 Import validation

Before applying a record:

- validate path shape, namespace, memory type, and size limits as today,
- parse `rdf.quads` with `oxttl`,
- verify `canonical_hash`,
- verify that every canonical subject is `urn:neurostrata:memory:<id>` and every graph is `urn:neurostrata:graph:<namespace>:domain:gen:0`,
- reject runtime counter keys if present in RDF metadata,
- reject records whose canonical payload contains private or unshared predicates,
- enforce `MAX_RDF_QUADS_BYTES` (512 KiB) and a maximum of 1,000 quads per record.

Import applies the row only when both JSON and canonical validation pass (Phase 2+; in Phase 1 valid JSON still applies with a warning when shadow RDF validation fails).

### 5.6 Export

- `neurostrata_export_canonical_rdf(namespace?, format?)` exports deterministic N-Quads or TriG.
- Export reuses existing retirement, archive, conflict, and registry filters.
- Private rows are excluded unless explicitly requested in an audit mode.

### 5.7 Migration gates

- Gate A: dual-write enabled and canonical payload present on all new records.
- Gate B: importers successfully apply canonical payloads in production for a compatibility window.
- Gate C: v1-only records are absent from the joined mesh or explicitly archived.
- Gate D: publishers may omit legacy JSON fields after Gate C.

Each gate is tracked as a task with an owner and verification command.

### 5.8 Compatibility and rollback

- v1 records remain readable indefinitely as a fallback.
- In Phase 2+, if canonical import fails, the record is ignored and JSON is not applied. In Phase 1, valid JSON still applies with a diagnostic.
- If a publisher lacks canonical support, it emits v1 records and joined peers continue to apply them.
- Rollback is a configuration change: disable canonical import and return to JSON-only application.

## 6. Interfaces

- Mesh record schema: `synaptic-mesh-record/v1` with optional `rdf` during migration; `synaptic-mesh-record/v2` only when legacy JSON fields are omitted.
- Outbox entry: unchanged.
- Tools:
  - `neurostrata_export_canonical_rdf`
  - `neurostrata_validate_canonical_rdf`
- Store changes: none required; LadybugDB remains authoritative.
- RDF projection module: canonical dataset builder shared by export and mesh.

## 7. Edge cases

- Pre-migration v1 record: apply JSON only.
- Invalid canonical hash: ignore record in Phase 2+; in Phase 1 warn and apply valid JSON.
- Canonical payload larger than size limit: ignore record in Phase 2+; in Phase 1 warn and apply valid JSON.
- Missing graph or subject mismatch: ignore record in Phase 2+; in Phase 1 warn and apply valid JSON.
- Mixed v1/v2 peers: compatibility through dual-write.
- Private row: excluded from canonical export and mesh.

## 8. Testing

- Unit: canonical dataset construction and hash stability.
- Unit: canonical N-Quads round-trip and hash stability.
- Integration: dual-write record validates and imports.
- Integration: v1 record still imports.
- Migration: Gate A/B/C checks pass on a fixture mesh.
- Privacy: private rows excluded from canonical export.

## 9. Acceptance

Canonical RDF quads are available for memory exchange and export; mesh records can carry a validated canonical payload alongside JSON; importers can apply canonical payloads or safely fall back to v1; LadybugDB remains the sole store; no LLM is used.

## 10. Files touched (expected)

- `src/rdf/dataset_canonical.rs` (new): canonical dataset builder and hash. Existing `src/rdf/canonical.rs` remains entity-slug canonicalization.
- `src/synaptic_mesh/record.rs`: optional `rdf` field.
- `src/synaptic_mesh/worker.rs`: build canonical `rdf` payload in `build_record`.
- `src/synaptic_mesh/import.rs`: canonical validation and application.
- `src/server.rs`: export and validation tools.
- `Cargo.toml`: `oxrdf` (`rdf-12`), `oxttl`.

## 6. Argon review resolutions (2026-10-28)

1. **Mesh wire predicate & datatype table**: Add explicit mapping table for every MeshRecord field (lifecycle, update_id, origin_mesh_epoch, trust, procedure definition, curated metadata) to exact `urn:neurostrata:ns#...` predicates with XSD datatypes and round-trip rules.
2. **Relationship vocabulary**: Map MeshRecord.relationships (`supersedes`, `references`) to `ns:supersedes` / `ns:references` targeting `urn:neurostrata:memory:<uuid>`.
3. **Publish-time lifecycle ordering**: Recompute `rdf` block after preserving origin fields in `worker::apply_one` and `import::preserved_edit_record`.
4. **RdfBlock struct**: Replace fields in `src/synaptic_mesh/record.rs` to match `{schema, format, canonical_hash, quads}` and add serde defaults.
5. **Import validation**: Parse `rdf.quads`, reconstruct shadow MeshRecord, verify semantic equivalence before secret scanning and classify_merge; default-deny unknown predicates.
6. **Canonical serialization**: Specify oxrdf/oxttl formatting, sorted lines, trailing newline, sha256 lowercase hex.
7. **Named graph IRI**: Align `src/rdf/named_graphs.rs` with `urn:neurostrata:graph:<namespace>:domain:gen:0` or update spec to authoritative form.
8. **MCP tool schemas**: Specify exact JSON Schema for `neurostrata_export_canonical_rdf` and `neurostrata_validate_canonical_rdf`.
