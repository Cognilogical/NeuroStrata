# Design: Polymorphic Digital Twin Interfaces, Typed Link Constraints & Declarative Property Reducers

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-1mus`, Feature Task `neurostrata-brs1` (Spec Task `neurostrata-oh0c`).
**Companion Specs:** `2026-10-28-digital-twin-design.md` (`jk91`), `2026-10-27-neurostrata-rdf-semantic-layer-design.md` (`9fnf` / `2hxl` / `pjoa` / `u5o1`), `2026-10-27-rdf-star-provenance-design.md` (`4gb2`), and OxiRS §1.19 `QueryBudget` (`yjah`).

## 1. Problem

A direct comparison between Palantir Foundry's **Semantic Layer (Object Types, Interfaces, Link Types, Derived Property Reducers)** and NeuroStrata's Digital Twin module (`src/digital_twin/mod.rs`, `docs/superpowers/specs/2026-10-28-digital-twin-design.md`) reveals four expressive limitations in our current implementation:

1. **No Polymorphic Interfaces (`src/digital_twin/mod.rs:73-97`):**
   - In `src/digital_twin/mod.rs:73`, `TwinSchema` is strictly a flat `BTreeMap<String, TwinVarBounds>` bound to a single `schema_id`.
   - Palantir's **Interfaces** allow heterogeneous object types (e.g., `CoolingPump`, `BackupGenerator`, `ChillerUnit`) to implement shared interfaces (e.g., `ThermalAsset`, `MaintainableEquipment`) that enforce required shared properties/variables and allow polymorphic queries (`SELECT ?asset WHERE { ?asset a ns:Interface_ThermalAsset }`) and polymorphic `Action Types`.
2. **Numeric-Only `f64` State (`src/digital_twin/mod.rs:105-119`):**
   - `get_state` and `set_state` only accept `BTreeMap<String, f64>`. Real-world operational twins also carry discrete typed **attributes** (`status: "online" | "degraded" | "offline"`, `firmware_version: "v2.4.1"`, `failover_enabled: true`).
3. **No Typed Link Constraints Between Twins, Rules & Code (`src/digital_twin/mod.rs:121+`):**
   - Twin schemas cannot declare cardinality-constrained link types (e.g., a `CoolingLoop` twin must link via `CONTAINS` to `1..=8` `CoolingPump` twins and be governed via `GOVERNS` by $\ge 1$ safety `rule`).
4. **No Declarative Property Reducers (Derived Roll-Up Properties):**
   - In Palantir's Ontology, a parent object type can declare a **Property Reducer** (e.g., `total_power_kw = sum(CONTAINS -> ChildAsset.power_kw)`, `max_child_temp = max(CONTAINS -> ThermalAsset.temperature)`). In NeuroStrata today, twin variables are strictly static stored `f64` numbers with no roll-up reducer evaluation.

## 2. Goals

1. **Polymorphic Twin Interfaces & Schema Inheritance (`src/digital_twin/mod.rs`):**
   - Extend `metadata.twin_schema` (`memory_type = "digital_twin_schema"`) with:
     - `kind: "interface" | "object_schema"` (default `"object_schema"` for 100% backward compatibility with existing `jk91` schemas).
     - `interface_slug: Option<String>` (canonical kebab-case slug per `u5o1`).
     - `implements_interfaces: Vec<String>` (list of interface schema IDs or slugs implemented by this `object_schema` or extended by this sub-interface, cycle-guarded via DFS just like `OntologyIndex` domain parents at `src/rdf/ontology.rs:119-135`).
   - Effective `TwinSchema` merges required `variables`, `attributes`, and `link_constraints` from all transitive `implements_interfaces`.
2. **Typed Discrete `attributes` Alongside Numeric `variables`:**
   - Support optional `metadata.twin.attributes: BTreeMap<String, TwinAttrValue>` (`String`, `Bool`, `I64`) validated against `metadata.twin_schema.attributes: BTreeMap<String, TwinAttrSpec>` (`attr_type: "string" | "enum" | "bool" | "i64"`, `in_values: Option<Vec<String>>`, `required: bool`).
3. **Typed `link_constraints` Validation:**
   - Allow `TwinSchema` to declare `link_constraints: Vec<TwinLinkConstraint>` (`relation: "CONTAINS" | "DEPENDS_ON" | "RELATES_TO" | "GOVERNS"`, `direction: "outgoing" | "incoming"`, `target_interface_or_schema: Option<String>`, `target_memory_type: Option<String>`, `min_count: u32`, `max_count: Option<u32>`).
   - Report link cardinality mismatches as warn-first `TwinWarning::LinkCardinalityViolation` (`src/digital_twin/mod.rs:37`) and `ShaclViolation` (`src/rdf/shacl.rs:44`).
4. **Declarative `PropertyReducer` Evaluation (Bounded by `QueryBudget`):**
   - Allow `TwinSchema` to declare `reducers: BTreeMap<String, PropertyReducerSpec>`:
     ```rust
     #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
     pub struct PropertyReducerSpec {
         pub agg: ReducerAgg,              // Sum | Avg | Min | Max | Count
         pub relation: String,             // "CONTAINS" | "DEPENDS_ON" | "RELATES_TO"
         pub direction: String,            // "outgoing" | "incoming"
         pub target_interface_or_schema: Option<String>,
         pub source_variable: Option<String>, // required for Sum/Avg/Min/Max
         pub unit: Option<String>,
     }
     ```
   - Compute derived variables deterministically at read time (`get_twin_state_with_reducers`) and project them into RDF quads (`ns:twinDerived_<name>`) within `QueryBudget` (`max_nodes: 40`, `max_ms: 50`, matching `yjah` defaults).

## 3. Non-Goals

- Breaking any existing `digital_twin` or `digital_twin_schema` tests or JSON payloads from `neurostrata-jk91` / `neurostrata-0x39` (all new schema fields are optional with `#[serde(default)]`).
- Unbounded recursive graph traversals in reducers (reducers evaluate strictly across 1-hop active edges under `QueryBudget`).

## 4. Library Evaluation & Five Mandatory OxiRS Compliance Elements

Per Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`:

1. **Library Evaluation & Banned-Crate Rejection:**
   - **Used (already in `Cargo.toml`):** `oxrdf` (RDF quad projection for `ns:implementsInterface`, `ns:twinAttr_*`, `ns:twinDerived_*`), `oxttl` (SHACL Turtle shapes), `arc-swap` (`OntologyIndex` interface hierarchy cache), `sha2` (slug normalization).
   - **Dependency Policy:** Always use a suitable license-compatible library when it provides the required functionality; never optimize for a zero-new-dependency count or prefer in-house code by default. The existing Oxigraph crates already provide the required RDF and SHACL parsing functionality. Evaluate graph-inheritance and aggregation libraries rather than assuming in-house traversal is preferable; choose on functional fit, license compatibility, correctness, security, maintenance, deterministic behavior, and integration cost. Historical crate rejections are context-specific, not a closed dependency policy.
2. **Warn-First Error Discipline:**
   - Missing interface variables, unknown attributes, out-of-bounds variables, or unmet link cardinalities produce `TwinWarning` variants (`OutOfBounds`, `UnknownVariable`, `MissingInterfaceVariable`, `InvalidAttribute`, `LinkCardinalityViolation`) — warn-first on read/ingest, and strict only when invoked via a gated `ActionType` (`neurostrata-undx`) or `shacl_enforce: true`.
3. **Per-Namespace Isolation:**
   - Interface schemas, twin schemas, and reducer evaluations resolve strictly within the target `namespace`. Cross-namespace interface equivalence is opt-in via `metadata.concept_iri` (`neurostrata-pjoa`).
4. **Deterministic Output & Zero LLM in Core:**
   - Interface inheritance resolution (DFS cycle-guarded topological merge), attribute validation, and `Sum`/`Avg`/`Min`/`Max`/`Count` reducers over 1-hop active twin neighbors are 100% deterministic Rust functions (`NaN`/`±Inf` guarded).
5. **Cross-References to Prior Specs:**
   - Directly extends `jk91` (`docs/superpowers/specs/2026-10-28-digital-twin-design.md`) and Wave 39b (`neurostrata-0x39`), and composes with `9fnf` (`src/rdf/projection.rs`), `yjah` (`QueryBudget`), `2hxl` (OWL 2 RL `rdfs:subClassOf` closure in `src/rdf/ontology.rs:60`), `pjoa` (`concept_iri`), `u5o1` (canonical slugs), `4gb2` (RDF-star edge provenance), `52dq` (GraphRAG hybrid search), and `mhsp` (inter-namespace isolation).

## 5. Architecture

### 5.1 Extended `TwinSchema` & `TwinWarning` in `src/digital_twin/mod.rs`

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TwinSchema {
    #[serde(default = "default_schema_kind")]
    pub kind: String, // "object_schema" | "interface"
    #[serde(default)]
    pub interface_slug: Option<String>,
    #[serde(default)]
    pub implements_interfaces: Vec<String>,
    #[serde(default)]
    pub variables: BTreeMap<String, TwinVarBounds>,
    #[serde(default)]
    pub attributes: BTreeMap<String, TwinAttrSpec>,
    #[serde(default)]
    pub link_constraints: Vec<TwinLinkConstraint>,
    #[serde(default)]
    pub reducers: BTreeMap<String, PropertyReducerSpec>,
}
```

- **Interface Inheritance Closure (`resolve_effective_twin_schema`):**
  - Given a `TwinSchema` and the namespace's schema map (`BTreeMap<String, TwinSchema>`), walk `implements_interfaces` in BFS order with a `visited: BTreeSet<String>` cycle guard (max depth 8).
  - Merge ancestor interface `variables`, `attributes`, and `link_constraints` into the child schema; if both an ancestor interface and child schema define bounds for variable $v$, intersect the bounds (`min = child.min.max(iface.min)`, `max = child.max.min(iface.max)`).

### 5.2 RDF & OWL 2 RL Projection (`src/rdf/projection.rs` & `src/rdf/ontology.rs`)

- When projecting a `digital_twin_schema` row with `kind == "interface"` and `interface_slug == Some(slug)`:
  - Emit `urn:neurostrata:interface:<slug> rdf:type owl:Class`.
  - For each parent in `implements_interfaces`, emit `urn:neurostrata:interface:<slug> rdfs:subClassOf urn:neurostrata:interface:<parent>`.
- When projecting a `digital_twin` row whose schema implements interfaces $[I_1, \dots, I_k]$:
  - Emit `urn:neurostrata:memory:<id> rdf:type urn:neurostrata:interface:<I_j>` for each implemented interface (including transitive super-interfaces via `OntologyIndex::sub_classes` at `src/rdf/ontology.rs:60`).
  - Emit `ns:twinAttr_<sanitized_name>` (`xsd:string` / `xsd:boolean` / `xsd:integer`) for each discrete attribute.

### 5.3 Deterministic Reducer Evaluation (`compute_twin_reducers`)

- For a twin row $T$ with active schema $S$:
  - Query 1-hop active (un-retired, latest version per `twin_id`) neighbor rows along `spec.relation` (`CONTAINS`, `DEPENDS_ON`, `RELATES_TO`), bounded by `QueryBudget { max_ms: 50, max_nodes: 40 }`.
  - Filter neighbors matching `spec.target_interface_or_schema` (if specified).
  - Aggregate `source_variable` across matching neighbors:
    - `Count`: number of matching active neighbors (`f64`).
    - `Sum`: $\sum v_i$ (`0.0` if empty).
    - `Avg`: $\frac{1}{N}\sum v_i$ (`0.0` if empty — never `NaN`).
    - `Min` / `Max`: $\min v_i$ / $\max v_i$ (`None` omitted if empty).
  - Return `derived_variables: BTreeMap<String, f64>` alongside stored `variables`.

### 5.4 Formal Verification (`src/formal_kernel.rs`, `proofs/NeuroStrata/TwinInterface.lean`, `tests/lean_parity_test.rs`)

Interface bound tightening (covariance) and total derived-property reducer evaluation are extracted into `src/formal_kernel.rs` and formally proved in `proofs/NeuroStrata/TwinInterface.lean`:

1. **Pure Kernel Functions (`src/formal_kernel.rs`):**
   - `pub const fn intersect_var_bounds(iface_min: i64, iface_max: i64, twin_min: i64, twin_max: i64) -> Result<(i64, i64), TwinBoundError>`
     - Computes `eff_min = max(iface_min, twin_min)` and `eff_max = min(iface_max, twin_max)`, rejecting inverted intervals (`InvertedInterfaceBounds`, `InvertedTwinBounds`, `DisjointBounds`) and enforcing covariant bound tightening (`eff_min >= iface_min && eff_max <= iface_max`).
   - `pub const fn evaluate_reducer_total(op: ReducerOpKind, count: u32, sum_val: i64, min_val: i64, max_val: i64, any_out_of_bounds: bool) -> ReducerEvalOutcome`
     - Total, division-by-zero-free reducer kernel over integer-scaled fixed-point state (`Count`, `Sum`, `Min`, `Max`, `Avg`, `AllInBounds`, `AnyOutOfBounds`), returning deterministic zero/empty defaults when `count == 0`.
2. **Lean 4 Theorems (`proofs/NeuroStrata/TwinInterface.lean`):**
   - `intersect_bounds_tightens_interface`: whenever `intersect_var_bounds imin imax tmin tmax = .ok (emin, emax)`, `imin ≤ emin ∧ emin ≤ emax ∧ emax ≤ imax`.
   - `intersect_bounds_commutative`: `intersect_var_bounds a b c d` and `intersect_var_bounds c d a b` yield identical valid intervals `(emin, emax)`.
   - `reducer_eval_total_on_empty`: when `count = 0`, `evaluate_reducer_total` never divides by zero and returns a deterministic finite value (`0` for numeric reducers, `true` for `AllInBounds`, `false` for `AnyOutOfBounds`).
3. **Exhaustive Parity (`tests/lean_parity_test.rs`):**
   - Verifies representative interval intersections and all 7 `ReducerOpKind` variants across empty (`count = 0`) and populated (`count > 0`) inputs against `lake --dir proofs exe neurostrata-proofs`.

## 6. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Cyclic `implements_interfaces` definitions (`A -> B -> A`) | Guard BFS inheritance walk with `visited: BTreeSet<String>` (matching `src/rdf/ontology.rs:120-135`), dropping cyclic edges and emitting a warn-first `ShaclViolation`. |
| Division-by-zero `NaN` on `Avg` reducer when a twin has 0 linked children | Explicitly return `0.0` (or `None` in optional map) when matching neighbor count is `0`; never emit non-finite `f64`. |
| Backward compatibility with existing `jk91` / `0x39` tests | All new fields on `TwinSchema` and `TwinSummary` default via `#[serde(default)]`; existing `get_state`, `set_state`, and `diff` signatures remain unchanged. |

## 7. Checkable Acceptance Criteria

1. Defining an interface schema `thermal-asset` (requiring `temperature` in `[-50.0, 200.0]`) and two object schemas `cooling-pump` and `chiller-unit` with `implements_interfaces: ["thermal-asset"]` causes `validate_state` to enforce `temperature` bounds on both twin types and projects `rdf:type urn:neurostrata:interface:thermal-asset` on both in SPARQL.
2. Querying `SELECT ?twin WHERE { ?twin a <urn:neurostrata:interface:thermal-asset> }` via `neurostrata_sparql_query` returns all active `cooling-pump` and `chiller-unit` twins.
3. Defining a `PropertyReducerSpec` (`agg: Sum`, `relation: "CONTAINS"`, `source_variable: "power_kw"`) on a `cooling-loop` parent schema linked via `CONTAINS` to 3 `cooling-pump` twins (`power_kw = 10.0, 15.5, 20.0`) returns `derived_variables["total_power_kw"] == 45.5`.
4. All existing `digital_twin` and Wave 39b (`neurostrata-0x39`) tests pass without modification.
5. `proofs/NeuroStrata/TwinInterface.lean` builds cleanly with zero `sorry`, and `cargo test --locked --test lean_parity_test` passes all `TwinInterface` parity rows.

## 8. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-brs1` to produce the step-by-step TDD implementation plan.
