# Design: Counterfactual `Ontology Scenarios` — Copy-on-Write What-If Sandbox, Evaluation, Diff & Commit (`neurostrata_scenario`)

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-1mus`, Feature Task `neurostrata-ai0v` (Spec Task `neurostrata-oh0c`, blocked by `neurostrata-undx`).
**Companion Specs:** `2026-11-14-kinetic-action-types-and-revert-log-design.md` (`undx`), `2026-10-27-neurostrata-rdf-semantic-layer-design.md` (`9fnf` / `2hxl` / `pjoa` / `u5o1`), `2026-10-27-sparql-query-engine-design.md`, `2026-10-28-digital-twin-design.md` (`jk91`), `2026-10-28-rebac-design.md`, and OxiRS §1.19 `QueryBudget` (`yjah`).

## 1. Problem

An enterprise ontology's **Dynamic Layer (`Ontology Scenarios`)** allows operators and AI agents to fork a temporary, isolated **"what-if" overlay** on top of the live Ontology, apply one or more hypothetical `Actions` or `Digital Twin` state changes, run queries/models/validations against the simulated state without mutating the live database, compare multiple scenarios side-by-side, and either **Merge (Commit)** the winning scenario or discard it.

In NeuroStrata today:
- Every mutation (`neurostrata_add_memory`, `neurostrata_supersede_memory`, `neurostrata_twin_set_state`) mutates the live LadybugDB store immediately and triggers live event bus / SynapticMesh subscribers.
- While AST ingestion has two-phase `IngestGeneration` staging (`begin_generation` $\rightarrow$ `publish_generation` in `src/store/ladybug.rs:8136`), there is no general-purpose **Copy-on-Write (CoW) Scenario Overlay** for normative memories, digital twins, and RDF/SPARQL queries.
- Agents planning complex architectural refactors, policy updates, or digital-twin control sequences cannot test "If I apply Action $A$ and supersede Rule $R$, does SHACL validation pass, do any SPARQL invariants break, and how do derived twin metrics change?" before touching live state.

## 2. Goals

1. **Zero-Copy In-Memory `ScenarioOverlay` per Namespace (`src/rdf/scenario.rs`):**
   - Maintain a bounded, TTL-expiring registry of active scenarios per namespace (`ArcSwap<BTreeMap<String, Arc<ScenarioState>>>`, capped at `MAX_SCENARIOS_PER_NAMESPACE = 16`, default TTL `1800s`).
   - A `ScenarioState` stores only the delta from the base LadybugDB namespace:
     - `base_version_fingerprints: BTreeMap<String, String>` (SHA-256 content+metadata digest of every live row read/modified by the scenario, for optimistic CAS on commit).
     - `staged_upserts: BTreeMap<String, MemoryPayload>` (newly created or modified rows inside the scenario).
     - `staged_retirements: BTreeSet<String>` (live row IDs retired/superseded inside the scenario).
     - `staged_edges: Vec<(String, String, String)>` (added `(source, relation, target)` triples).
     - `applied_actions: Vec<String>` (ordered log of kinetic `ActionType` slugs applied in the scenario).
2. **Merged Overlay Evaluation (SPARQL + SHACL + Digital Twin Reducers):**
   - Provide `project_scenario_sparql_dataset_async(namespace, scenario_id, store)` extending `project_sparql_dataset_with_meta_async` (`src/rdf/sparql.rs:89`):
     - Reads active live rows from `LadybugStore`, filters out `staged_retirements`, overlays `staged_upserts` and `staged_edges`, and projects the merged `oxrdf::Dataset`.
   - Allow running any read-only SPARQL `SELECT`/`ASK` query (`src/rdf/sparql.rs`), SHACL validation (`src/rdf/shacl.rs`), OWL 2 RL closure (`src/rdf/ontology.rs`), and Digital Twin bounds/reducer evaluation (`src/digital_twin/mod.rs`) against the scenario with zero live LadybugDB writes.
3. **Structured Scenario Diff & Side-by-Side Comparison (`action: "diff"`):**
   - Compute a deterministic diff between a scenario and live state (or between `scenario_a` and `scenario_b`):
     - `added_memories`, `superseded_memories`, `twin_variable_deltas` (`BTreeMap<String, Vec<TwinDelta>>`), `edge_deltas`, and `shacl_violations_delta` (new violations introduced vs. resolved).
4. **Optimistic CAS Commit (`action: "commit"`) & Instant Drop (`action: "drop"`):**
   - `commit` verifies that every row in `base_version_fingerprints` still matches the live LadybugDB row (nobody mutated or superseded the underlying rows while the scenario was open) and that ReBAC (`src/rebac.rs:15`) permits all edits for the committing principal, then flushes `staged_upserts`, `staged_retirements`, and `staged_edges` into `LadybugStore`.
   - `drop` removes the `ScenarioState` from the in-memory overlay map in $O(1)$ time with zero disk churn.

## 3. Non-Goals

- Persisting uncommitted temporary scenarios across daemon restarts or syncing scenarios to SynapticMesh (scenarios are ephemeral local counterfactual sandboxes; only committed changes persist to LadybugDB and tee to SynapticMesh if `eligibility: shareable`).
- Multi-hour long-running physics solvers inside the daemon (all scenario evaluations obey `QueryBudget` / `SparqlConfig` 250ms–1000ms bounds).

## 4. Library Evaluation & Five Mandatory OxiRS Compliance Elements

Per Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`:

1. **Library Evaluation & Banned-Crate Rejection:**
   - **Used (already in `Cargo.toml`):** `oxrdf` (in-memory `Dataset` overlay projection), `spargebra` + `spareval` + `sparesults` (Oxigraph SPARQL evaluation over scenario dataset), `oxttl` (SHACL shape validation), `arc-swap` (per-namespace scenario store), `sha2` (SHA-256 base-row CAS fingerprinting).
   - **Dependency Policy:** Always use a suitable license-compatible library when it provides the required functionality; never optimize for a zero-new-dependency count or prefer in-house code by default. The existing Oxigraph, `arc-swap`, and `sha2` crates already provide the required dataset, query, isolation, and fingerprinting functionality. Evaluate any additional candidate on functional fit, license compatibility, correctness, security, maintenance, deterministic behavior, and integration cost. Historical crate rejections are context-specific, not a closed dependency policy.
2. **Warn-First Error Discipline:**
   - Staging a memory or twin mutation inside a scenario (`stage_memory` / `stage_twin`) is **warn-first**: it records any `ShaclViolation` (`src/rdf/shacl.rs:44`) or `TwinWarning` (`src/digital_twin/mod.rs:37`) in the scenario's inspection report without aborting the exploration, so an agent can fix violations over multiple steps inside the sandbox before calling `commit`.
3. **Per-Namespace Isolation:**
   - Scenarios are stored in `ArcSwap<BTreeMap<String, BTreeMap<String, Arc<ScenarioState>>>>` keyed by `(namespace, scenario_id)`. A scenario in namespace $A$ can never read, overlay, or commit rows in namespace $B$ (cross-namespace opt-in solely via `pjoa` `concept_iri`).
4. **Deterministic Output & Zero LLM in Core:**
   - Copy-on-write dataset projection, SHA-256 CAS fingerprinting, `TwinDelta` computation, and SPARQL/SHACL evaluation are 100% deterministic Rust logic with zero LLM involvement.
5. **Cross-References to Prior Specs:**
   - Composes with `9fnf` (`src/rdf/projection.rs`), `yjah` (`QueryBudget` in `src/rdf/ontology.rs:11`), `2hxl` (OWL 2 RL closure), `pjoa` (`concept_iri`), `u5o1` (canonical slugs), `4gb2` (RDF-star edge provenance), `52dq` (GraphRAG hybrid), `mhsp` (inter-namespace isolation), `jk91` (`src/digital_twin/mod.rs`), `2026-10-28-rebac-design.md` (`src/rebac.rs`), and `2026-11-14-kinetic-action-types-and-revert-log-design.md` (`neurostrata-undx`).

## 5. Architecture

### 5.1 Data Structures (`src/rdf/scenario.rs`)

```rust
pub const MAX_SCENARIOS_PER_NAMESPACE: usize = 16;
pub const MAX_STAGED_ROWS_PER_SCENARIO: usize = 256;
pub const DEFAULT_SCENARIO_TTL_SECS: i64 = 1800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioState {
    pub scenario_id: String,              // e.g. "scen-cooling-failover"
    pub namespace: String,
    pub title: String,
    pub owner_principal: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub base_fingerprints: BTreeMap<String, String>, // memory_id -> sha256(content + canonical_metadata)
    pub staged_upserts: BTreeMap<String, MemoryPayload>,
    pub staged_retirements: BTreeSet<String>,
    pub staged_edges: Vec<StagedEdge>,
    pub applied_actions: Vec<String>,     // action_type slugs applied in order
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedEdge {
    pub source: String,
    pub relation: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDiffReport {
    pub scenario_id: String,
    pub compared_against: String,         // "live" or another scenario_id
    pub added_ids: Vec<String>,
    pub retired_ids: Vec<String>,
    pub twin_deltas: BTreeMap<String, Vec<crate::digital_twin::TwinDelta>>,
    pub added_edges: Vec<StagedEdge>,
    pub shacl_violations: Vec<crate::rdf::shacl::ShaclViolation>,
    pub twin_warnings: BTreeMap<String, Vec<crate::digital_twin::TwinWarning>>,
    pub base_conflicts: Vec<String>,      // IDs modified in live DB since scenario branched
}
```

### 5.2 Overlay Dataset Projection (`project_scenario_sparql_dataset_async`)

When evaluating SPARQL or SHACL on `scenario_id`:
1. Call `store.list_memories(namespace, None, None)` (same active-generation filtering as `src/rdf/sparql.rs:102-125`).
2. Skip any `(id, _)` present in `scenario.staged_retirements` or overridden in `scenario.staged_upserts`.
3. Project remaining live memories via `memory_to_quads` (`src/rdf/projection.rs:27`).
4. Project every `(id, payload)` in `scenario.staged_upserts` via `memory_to_quads` and every `StagedEdge` via `edge_to_quads`.
5. Run `spargebra` + `spareval` (`SparqlConfig::default()`) or `shacl::validate_memory` over the combined `Dataset`.

### 5.3 Optimistic CAS Commit Protocol (`commit_scenario`)

1. Re-read every live row in `scenario.base_fingerprints.keys()` from `LadybugStore`.
2. Recompute `sha256_fingerprint(live_row)`: if any fingerprint differs from `base_fingerprints[id]`, abort the commit and return `ScenarioCommitOutcome::BaseConflict { conflicting_ids }`.
3. Verify `rebac::allowed_with_roles` (`src/rebac.rs:15`) for `committing_principal` (`"editor"` or `"owner"`) on all retired and modified base rows.
4. If `enforce_shacl_on_commit` is true (default `true`), verify `shacl_violations.is_empty()`.
5. Persist `staged_upserts`, retire `staged_retirements` (`valid_to = now`), and create `staged_edges` in `LadybugStore`, then remove `scenario_id` from the active scenario map.

### 5.4 Surface Additions (`src/server.rs`, `src/daemon.rs`, `src/main.rs`)

- **MCP Tool:** `neurostrata_scenario({ namespace, action: "create" | "stage_memory" | "stage_twin" | "apply_action" | "evaluate_sparql" | "diff" | "commit" | "drop" | "list", scenario_id?, compare_to_scenario_id?, ... })`.
- **Daemon Endpoint:** `POST /scenario`.
- **CLI Subcommand:** `neurostrata-mcp scenario <create|stage-twin|apply-action|eval|diff|commit|drop|list>`.

### 5.5 Formal Verification (`src/formal_kernel.rs`, `proofs/NeuroStrata/ScenarioOverlay.lean`, `tests/lean_parity_test.rs`)

The Copy-on-Write (CoW) overlay read precedence and optimistic CAS commit gate are extracted into `src/formal_kernel.rs` and proved in `proofs/NeuroStrata/ScenarioOverlay.lean`:

1. **Pure Kernel Functions (`src/formal_kernel.rs`):**
   - `pub const fn resolve_scenario_row(live_present: bool, staged_retired: bool, staged_upsert: bool) -> ScenarioRowVisibility`
     - Formalizes the CoW precedence rule: `staged_retired` masks `live_present` (`HiddenRetired`), `staged_upsert` overrides both (`VisibleStaged`), and otherwise visibility equals `live_present` (`VisibleLive` / `Absent`).
   - `pub const fn evaluate_scenario_commit(expired: bool, all_base_fingerprints_match: bool, rebac_authorized: bool, enforce_shacl: bool, shacl_violation_count: u32) -> ScenarioCommitDecision`
     - Returns `ScenarioCommitDecision::CommitAllowed` iff `!expired && all_base_fingerprints_match && rebac_authorized && (!enforce_shacl || shacl_violation_count == 0)`.
2. **Lean 4 Theorems (`proofs/NeuroStrata/ScenarioOverlay.lean`):**
   - `scenario_upsert_overrides_retirement_and_live`: staging a replacement upsert after retiring a row yields `VisibleStaged`; retiring without replacement yields `HiddenRetired`.
   - `scenario_commit_cas_sound`: `evaluate_scenario_commit ... = .commitAllowed → all_base_fingerprints_match = true ∧ expired = false ∧ rebac_authorized = true`.
   - `scenario_commit_shacl_gate`: when `enforce_shacl = true` and `shacl_violation_count > 0`, `evaluate_scenario_commit` never returns `.commitAllowed`.
3. **Exhaustive Parity (`tests/lean_parity_test.rs`):**
   - Verifies all $2^3 = 8$ `resolve_scenario_row` cases and all $2^4 \times 3 = 48$ `evaluate_scenario_commit` cases against the Lean executable.

## 6. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Unbounded memory growth from abandoned scenarios | Evict expired scenarios (`now >= expires_at`) lazily on every `neurostrata_scenario` call, enforce `MAX_SCENARIOS_PER_NAMESPACE = 16`, and cap `staged_upserts.len() <= 256` per scenario. |
| Stale base writes overwriting concurrent live mutations on `commit` | Mandatory SHA-256 fingerprint CAS check on every base row read/retired by the scenario before any write is applied to `LadybugStore`. |
| Event bus / SynapticMesh firing on hypothetical staged rows | `ScenarioOverlay` operates purely in-memory in `src/rdf/scenario.rs` and does **not** emit `ThalamicPulse` events until `action: "commit"` writes to `LadybugStore`. |

## 7. Checkable Acceptance Criteria

1. Creating a scenario (`action: "create"`), staging a `digital_twin` variable change (`temperature: 180.0`) and a new `rule` memory inside the scenario leaves the live `LadybugStore` completely unchanged (`neurostrata_twin_get_state` on live returns the original value).
2. Running `neurostrata_scenario({ action: "evaluate_sparql" })` sees the staged twin variable (`180.0`) and staged rule in the SPARQL result set, while live `neurostrata_sparql_query` still sees the pre-scenario state.
3. Calling `neurostrata_scenario({ action: "diff" })` returns the exact `TwinDelta` (`old` vs `new`), `added_ids`, `retired_ids`, and any `TwinWarning` / `ShaclViolation` warnings.
4. If a base row touched by the scenario is concurrently modified in the live store before `commit`, `neurostrata_scenario({ action: "commit" })` refuses the commit with `base_conflicts: [id]` and leaves live state intact.
5. When no base conflict exists, `neurostrata_scenario({ action: "commit" })` atomically persists the staged changes to `LadybugStore` and removes the temporary scenario.
6. `proofs/NeuroStrata/ScenarioOverlay.lean` builds cleanly with zero `sorry`, and `cargo test --locked --test lean_parity_test` passes all 56 `ScenarioOverlay` parity rows.

## 8. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-ai0v` to produce the step-by-step TDD implementation plan.
