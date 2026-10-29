# Digital Twin Domain Module Design

> Status: revised per Argon review — pending re-review.
> Task: `neurostrata-jk91` (digital twin domain module, OxiRS §1.14 domain track).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-28-rebac-design.md`.

## 1. Problem

`src/digital_twin/mod.rs` is a stub: `Twin` is an in-memory `BTreeMap<String, f64>` with no persistence, no versioning, no link to memory rows, and no retrieval integration. If NeuroStrata targets digital-twin domains, state must be durable, versioned, queryable, and must not make core retrieval depend on it.

## 2. Goal

Add an **optional digital-twin domain module** (feature `digital-twin`, default off) that:

- represents a twin as a first-class memory row (`memory_type = "digital_twin"`),
- stores state variables as versioned memory updates (no in-memory-only state),
- supports deterministic get/set/diff over state variables,
- exposes twin state via search/export/SPARQL without core retrieval dependency,
- is LLM-free and LadybugDB-backed.

When the feature is off, the module and its MCP tools are absent from the binary (no stubs).

## 3. Constraints

- Core memory retrieval must not depend on digital-twin module (feature-gated `digital-twin`, default off).
- LadybugDB remains sole store; twin state is memory rows, not a separate table.
- No LLM.
- Deterministic: state transitions are explicit, versioned, auditable.
- Privacy/ReBAC applies to twin rows like any memory.
- No `digital_twin_enabled` runtime config; feature gate is the switch.

## 4. Scope

In scope:

- Twin memory type and metadata schema (all twin fields nested under `metadata.twin`).
- State variable get/set/diff.
- Versioning via memory supersede (each state update creates a new memory row).
- Twin query helpers.
- RDF projection and SHACL validation in `src/rdf/shacl.rs`.
- MCP tools (feature-gated).
- Exclusion from all non-twin recall/dedup surfaces.

Out of scope:

- Simulation engines / physics solvers.
- Real-time streaming of twin state.
- Cross-instance twin federation (separate spec).

## 5. Approach

### 5.1 Twin memory type

A twin is a `Memory` row with:

- `memory_type = "digital_twin"`.
- `content` = human-readable twin description.
- `metadata.twin = { "id": "<twin_id>", "version": <u64>, "source": "<sensor|model|manual>", "schema_id": "<optional memory id of digital_twin_schema row>", "variables": { "<name>": <f64> }, "units": { "<name>": "<unit>" } }`.
- A twin definition row uses `memory_type = "digital_twin_schema"` with `metadata.twin_schema = { "variables": { "<name>": { "min": f64, "max": f64, "unit": "..." } } }`.
- Twin rows link to their schema via `metadata.twin.schema_id` (optional; if absent, any variable name allowed).

Twin rows and schema rows are excluded from default `search_inner`, `find_similar_for_dedup`, `find_exact_for_dedup`, `get_snapshot` top-5/domain strap, and `memories_for_file` unless explicitly requested.

### 5.2 State operations (`src/digital_twin/mod.rs`)

```rust
pub fn get_state(payload: &MemoryPayload) -> BTreeMap<String, f64>
pub fn set_state(payload: &mut MemoryPayload, vars: &BTreeMap<String, f64>, merge: bool) -> Result<(), TwinError>
pub fn diff(old: &MemoryPayload, new: &MemoryPayload) -> Vec<TwinDelta> // { name, old: Option<f64>, new: Option<f64> }
pub fn validate_state(vars: &BTreeMap<String, f64>, schema: Option<&TwinSchema>) -> Vec<TwinWarning>
```

- `set_state` with `merge = false` replaces the full variable map; `merge = true` merges into existing.
- `set_state` rejects `NaN`/`±Inf` with `TwinError::NonFinite`.
- State updates are persisted by creating a new memory row via the supersede protocol (`server.rs:2993-3162`): write replacement first (`valid_from = now`, `supersedes = old_id`), CAS re-read old row, retire old row (`valid_to = now`, `superseded_by = new_id`), emit `ThalamicPulse::Superseded`.
- `metadata.twin.version` increments monotonically per `twin_id` (version = 1 on initial creation).
- Embedding: twin rows use a zero vector (like `STRUCTURAL_MEMORY_TYPES`) so they never enter vector similarity; text search still works on `content`.

### 5.3 Twin schema (`metadata.twin_schema`)

Optional schema stored on a `digital_twin_schema` row:

```json
"twin_schema": {
  "variables": {
    "temperature": { "min": -50.0, "max": 200.0, "unit": "celsius" },
    "pressure": { "min": 0.0, "max": 10.0, "unit": "bar" }
  }
}
```

- If no schema, any `f64` variable name is allowed (warn-first).
- Schema violations produce `TwinWarning::OutOfBounds { name, value, min, max }` or `TwinWarning::UnknownVariable { name }`.

### 5.4 Query helpers

- `list_twins(namespace) -> Vec<TwinSummary>` (id, twin_id, twin_version, variable count, last updated).
- `get_twin_state(namespace, twin_id) -> Option<BTreeMap<String, f64>>` (latest version).
- `twin_history(namespace, twin_id, limit) -> Vec<(twin_version, ts, BTreeMap<String,f64>)>` (versioned history, sorted by version ASC).
- `twin_diff(namespace, twin_id, from_version, to_version) -> Vec<TwinDelta>`.

### 5.5 RDF projection

- `rdf:type` = `ns:DigitalTwin` for twin rows; `ns:DigitalTwinSchema` for schema rows.
- `ns:twinId`, `ns:twinVersion`, `ns:twinSource`, `ns:twinSchemaId`.
- Per-variable: `ns:twinVar_<sanitized_name>` (typed `xsd:double`) where `<sanitized_name>` is the variable name with non-identifier characters replaced by `_` (IRI-safe). Each row projects its own state (no "latest only" special-casing; `memory_to_quads` is pure per-row).
- `ns:supersedes` / `ns:supersededBy` for version chain.

### 5.6 SHACL validation (`src/rdf/shacl.rs`)

Extend `validate_memory`:

- `Memory` with `memory_type = "digital_twin"` requires `metadata.twin` object with `id` (string), `version` (integer, min 1), `variables` (object of finite numbers).
- `metadata.twin.units` values are strings.
- `metadata.twin_schema.variables` entries have `min`/`max` (numbers, min ≤ max) and `unit` (string).
- Finite number check: reject `NaN`/`±Inf`.

### 5.7 Search integration

- Twin rows excluded from default search (added to exclusion list alongside `STRUCTURAL_MEMORY_TYPES`: `digital_twin`, `digital_twin_schema`).
- `include_twins: bool` flag on `neurostrata_search_memory` (default false) includes them.
- Twin state variables are queryable via `neurostrata_twin_get_state` (not via text search).

### 5.8 MCP tools (feature-gated)

- `neurostrata_twin_list(namespace)`.
- `neurostrata_twin_get_state(namespace, twin_id)`.
- `neurostrata_twin_set_state(namespace, twin_id, variables, merge?, source?)`.
- `neurostrata_twin_history(namespace, twin_id, limit?)`.
- `neurostrata_twin_diff(namespace, twin_id, from_version, to_version)`.

## 6. Interfaces

- `src/digital_twin/mod.rs`: `TwinError`, `TwinWarning`, `TwinDelta`, `TwinSummary`, `TwinSchema`, `get_state`, `set_state`, `diff`, `validate_state`, `list_twins`, `get_twin_state`, `twin_history`, `twin_diff`.
- `LadybugStore`: twin rows use standard upsert; supersede protocol.
- `src/rdf/shacl.rs`: twin validation.
- `src/rdf/projection.rs`: twin predicates.
- Config: none (feature gate only).

## 7. Edge cases

- Feature off: module and tools absent (no stubs).
- Unknown twin_id: `get_twin_state` returns `None`.
- Concurrent updates: serialized by `write_lock`; version increments deterministically.
- Schema violation in strict mode: reject upsert; in non-strict: warn and apply.
- Twin rows excluded from default search unless `include_twins`.
- `NaN`/`±Inf` rejected.

## 8. Testing

- Unit: state get/set/diff (full replace and merge).
- Unit: diff with added/removed keys.
- Unit: schema validation bounds and finite check.
- Integration: versioned update creates supersede chain.
- Integration: twin history ordered by version.
- Integration: search excludes twins by default.
- Integration: SHACL validates twin shape.

## 9. Acceptance

Optional digital-twin module available behind feature flag; state versioned in LadybugDB via supersede protocol; deterministic get/set/diff; SHACL/RDF integration; no core dependency; no LLM; no stubs when disabled.

## 10. Files touched (expected)

- `src/digital_twin/mod.rs`: full implementation.
- `src/store/ladybug.rs`: `include_twins` filter in search/dedup/snapshot/memories_for_file.
- `src/rdf/shacl.rs`: twin validation.
- `src/rdf/projection.rs`: twin predicates.
- `src/server.rs`: MCP tools (feature-gated).
- `src/schemas/memory-vocabulary.v3.json`: add `digital_twin`, `digital_twin_schema` to memory_type enum.
- `Cargo.toml`: feature `digital-twin` (default off).