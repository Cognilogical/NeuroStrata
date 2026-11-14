# Design: Kinetic `Action Types`, Submission-Criteria Gatekeeping & Reversible Action Log (`neurostrata_action`)

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-1mus`, Feature Task `neurostrata-undx` (Spec Task `neurostrata-oh0c`).
**Companion Specs:** `2026-10-27-neurostrata-rdf-semantic-layer-design.md` (`9fnf` / `2hxl` / `pjoa` / `u5o1`), `2026-10-27-rdf-star-provenance-design.md` (`4gb2`), `2026-10-27-sparql-query-engine-design.md`, `2026-10-28-digital-twin-design.md` (`jk91`), `2026-10-28-rebac-design.md`, and OxiRS §1.19 `QueryBudget` (`yjah`).

## 1. Problem

Palantir Foundry's **Kinetic Layer** (`Action Types`) separates *raw storage CRUD* from *governed domain mutations*. In Palantir's Ontology, operators and AI agents do not arbitrarily mutate individual database fields; they invoke typed **`Action Types`** that bundle:
1. Typed input parameters with constraint validation.
2. Role-based authorization (`ReBAC`) and cross-object **submission criteria** (pre-conditions that must hold across the graph before the action is permitted).
3. Multi-object atomic mutations (updating object properties, digital twin variables, and graph links together).
4. An immutable **`Action Log`** supporting **1-call Undo / Revert**.

In NeuroStrata today:
- Our ontology (`src/rdf/ontology.rs`, `src/rdf/shacl.rs`) is **descriptive and passive**.
- Agents mutate state through low-level CRUD primitives (`neurostrata_add_memory`, `neurostrata_supersede_memory`, `neurostrata_relink`, `neurostrata_twin_set_state`, `neurostrata_task_update`).
- `procedure` memories (`AGENTS.md` §3a) track rehearsal budgets (`remaining_fires`, `performance_count`), not parameterized transactional mutations.
- `validate_memory` (`src/rdf/shacl.rs:57`) is warn-first by default (`shacl_enforce` is opt-in per call), and there is no way to require a graph-wide pre-condition (e.g., a SPARQL `ASK` invariant over `src/rdf/sparql.rs`) before committing a multi-row change, nor a 1-call `revert` to roll back a multi-row agent action.

## 2. Goals

1. **First-Class `ActionType` Schema (`memory_type = "action_type"`):**
   - Represent kinetic action definitions as governed memory rows (`memory_type = "action_type"`, `metadata.action_type = { ... }`) compiled into the per-namespace `ArcSwap<OntologyIndex>` (`src/rdf/ontology.rs:54`).
2. **Pre-Condition Gatekeeping (`submission_criteria`):**
   - Before executing an action, deterministically evaluate:
     - **Parameter Schema:** Type, required/optional, numeric bounds (`min`/`max`, finite check), enum `in_values`, and target entity existence/type check.
     - **ReBAC Authorization (`src/rebac.rs:15`):** Verify `allowed_with_roles(principal, owner, required_role, acl)` on every target row (`viewer` / `editor` / `owner`).
     - **SPARQL `ASK` Pre-Conditions (`src/rdf/sparql.rs:49`):** Zero or more parameterized read-only SPARQL `ASK` queries evaluated under `SparqlConfig::default()` (`timeout_ms: 250`, `max_algebra_nodes: 1000`) that must return `true`.
     - **SHACL Post-Image Validation (`src/rdf/shacl.rs:57`):** Strict SHACL validation on all staged post-mutation rows before any write lands in LadybugDB.
3. **Multi-Object Atomic Execution & Action Receipt (`memory_type = "action_receipt"`):**
   - Execute the action's declarative mutation steps (`SetTwinState`, `SupersedeMemory`, `UpsertMemory`, `LinkEdge`, `TransitionTask`) and record an immutable `action_receipt` row capturing `before_ids`, `after_ids`, `twin_deltas` (`Vec<TwinDelta>` from `src/digital_twin/mod.rs:48`), and `edge_deltas`.
4. **1-Call Bitemporal Revert (`action: "revert"`):**
   - Given an `action_receipt_id`, atomically revert the action using NeuroStrata's bitemporal supersession protocol (`handle_supersede_memory` in `src/server.rs:6600-6720`): retire rows created by the action (`valid_to = now`), restore superseded prior rows (`valid_to = null` or supersede back to pre-action state), restore `digital_twin` variables from `twin_deltas`, and mark the receipt `reverted_by_receipt_id`.
5. **Warn-First Compatibility on Ingest & Non-Action Paths:**
   - Raw `ingest_directory` and `neurostrata_add_memory` keep their existing warn-first behavior (Rule `227984e4-780c-4fe4-a44c-286d7d14eabf` warn-first element); strict submission-criteria gatekeeping applies specifically when invoking `neurostrata_action` (`action: "apply"`).

## 3. Non-Goals

- Arbitrary Turing-complete user scripts or external network webhooks inside the core daemon (`neurostrata-mcp` remains 100% local, deterministic, and offline).
- Replacing `procedure` rehearsal memories (`AGENTS.md` §3a); an `ActionType` can optionally reference a `procedure_id` to decrement its rehearsal counter upon successful commit.

## 4. Library Evaluation & Five Mandatory OxiRS Compliance Elements

Per Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`:

1. **Library Evaluation & Banned-Crate Rejection:**
   - **Used (already in `Cargo.toml`):** `oxrdf` (RDF terms/datasets), `oxttl` (SHACL Turtle parsing), `spargebra` + `spareval` + `sparesults` (Oxigraph SPARQL `ASK` pre-condition evaluation in `src/rdf/sparql.rs`), `arc-swap` (lock-free per-namespace `OntologyIndex`), `sha2` (deterministic receipt/slug hashing per `u5o1`).
   - **Dependency Policy:** Always use a suitable license-compatible library when it provides the required functionality; never optimize for a zero-new-dependency count or prefer in-house code by default. The existing Oxigraph crates above already provide the required RDF and SPARQL functionality. Evaluate any additional candidate on functional fit, license compatibility, correctness, security, maintenance, deterministic behavior, and integration cost. Earlier rejections of `oxirs-core`, `scirs2`, `horned-owl`, `oxirs-arq`, and `oxirs-shacl` are historical context-specific evaluations, not a closed dependency policy.
2. **Warn-First Error Discipline:**
   - Defining an `action_type` row with malformed metadata emits a `ShaclViolation` (`src/rdf/shacl.rs:44`) on warn-first ingest/upsert without crashing or blocking unrelated writes.
   - `neurostrata_action({ action: "validate" })` returns structured `submission_failures: Vec<ActionSubmissionFailure>` with `allowed: false` rather than a hard RPC error; `action: "apply"` refuses execution cleanly when `submission_failures` is non-empty.
3. **Per-Namespace Isolation:**
   - Action definitions and receipts live strictly in their namespace (`ArcSwap<BTreeMap<String, Arc<OntologyIndex>>>` per namespace). Cross-namespace alignment is opt-in solely via `metadata.concept_iri` (`neurostrata-pjoa`) and federated read-only SPARQL (`neurostrata-mhsp`).
   - `action_receipt` rows are local operational provenance (`eligibility: private`) and never tee into the SynapticMesh outbox (`Rule 460d2cc7-0e73-4af9-a052-8aaef57e4c0a`); `action_type` schema definitions may opt into `eligibility: shareable`.
4. **Deterministic Output & Zero LLM in Core:**
   - Parameter substitution, SPARQL `ASK` evaluation, SHACL validation, mutation application, and bitemporal revert are 100% deterministic Rust functions with zero LLM calls.
5. **Cross-References to Prior Specs:**
   - Builds directly on `9fnf` (RDF canonical projection `src/rdf/projection.rs`), `yjah` (`QueryBudget` in `src/rdf/ontology.rs:11`), `2hxl` (OWL 2 RL closure), `pjoa` (`concept_iri`), `u5o1` (canonical slugs), `4gb2` (RDF-star edge provenance), `52dq` (GraphRAG hybrid), `mhsp` (inter-namespace isolation), `jk91` (`src/digital_twin/mod.rs`), and `2026-10-28-rebac-design.md` (`src/rebac.rs`).

## 5. Architecture

### 5.1 `ActionTypeDefinition` Schema (`src/rdf/action.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionTypeDefinition {
    pub slug: String,                     // canonical kebab-case slug (per u5o1), e.g. "recalibrate-cooling-loop"
    pub title: String,
    pub description: String,
    pub required_role: String,            // "editor" | "owner" (checked via src/rebac.rs::allowed_with_roles)
    pub parameters: BTreeMap<String, ActionParamSpec>,
    pub submission_criteria: Vec<SubmissionCriterion>,
    pub effects: Vec<ActionEffectTemplate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionParamSpec {
    pub param_type: String,               // "memory_id" | "twin_id" | "f64" | "i64" | "string" | "bool"
    pub required: bool,
    pub target_memory_type: Option<String>,
    pub target_schema_id: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub in_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubmissionCriterion {
    /// Parameterized SPARQL 1.1 ASK query where `$param` bindings are safely
    /// substituted as typed oxrdf::Term literals/IRIs after spargebra parse.
    SparqlAsk {
        query: String,
        failure_message: String,
    },
    /// Require target digital twin variable to lie in [min, max] before mutation.
    TwinVarPrecondition {
        twin_param: String,
        variable: String,
        min: Option<f64>,
        max: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ActionEffectTemplate {
    SetTwinState {
        twin_param: String,
        merge: bool,
        variables_from_params: BTreeMap<String, String>, // twin_var_name -> param_name
    },
    SupersedeMemory {
        target_param: String,
        new_content_template: String,
        metadata_patch: serde_json::Value,
    },
    UpsertMemory {
        memory_type: String,
        content_template: String,
        metadata_template: serde_json::Value,
    },
    LinkEdge {
        source_param: String,
        relation: String,                 // "GOVERNS" | "DEPENDS_ON" | "RELATES_TO" | "CONTAINS"
        target_param: String,
    },
    TransitionTask {
        target_param: String,
        new_status: String,               // "open" | "in_progress" | "blocked" (never "done" per Lock 2)
        note_template: String,
    },
}
```

### 5.2 Execution Pipeline (`apply_action`)

1. **Resolve & Bind Parameters:** Load `ActionTypeDefinition` by `slug` from the namespace's `OntologyIndex`. Validate all supplied `params: BTreeMap<String, Value>` against `ActionParamSpec` (rejecting unknown/missing parameters and `NaN`/`±Inf` per `src/digital_twin/mod.rs:133`). Strict `action: "define"` rejects malformed or duplicate active slugs without a write; raw ingest/upsert still stores malformed definitions with warnings but does not compile them.
2. **ReBAC Authorization Check (`src/rebac.rs:15`):** Resolve the effective principal with `rebac::resolve_principal(authenticated_principal, requested_principal)` at the server/daemon boundary; a non-admin authenticated caller cannot impersonate `admin` or another principal. For every referenced target `memory_id` / `twin_id`, task, and edge endpoint, fetch the row only from the action namespace and verify `rebac::allowed_with_roles(&principal, &row.user_id, &def.required_role, &rebac::extract_acl_with_roles(&row.metadata))`.
3. **Evaluate `submission_criteria`:**
   - Project namespace RDF dataset (`project_sparql_dataset_with_meta_async` in `src/rdf/sparql.rs:89`) and run each `SparqlAsk` criterion with `SparqlConfig::default()`. Require exactly one read-only `ASK`; reject `SERVICE`, updates, timeouts, and over-budget algebra. Bind values through `spareval::PreparedQuery::substitute_variable`, never source-string interpolation.
   - Evaluate `TwinVarPrecondition` against current twin state (`src/digital_twin/mod.rs:105`).
   - If any criterion fails, abort before any write and return `ActionApplyOutcome::Rejected { failures }`. Errors name parameter/criterion IDs but never echo supplied values or rendered content.
4. **Stage Post-Images & Run Strict SHACL (`src/rdf/shacl.rs:57`):**
   - Construct candidate post-mutation `MemoryPayload`s in memory, including complete before-images for every affected row. Run `shacl::validate_memory`, `OntologyIndex::shacl_validate`, and `ReasoningAdapter::validate_payload` on every staged post-image and receipt. Any `ShaclViolation` aborts before the write turn.
   - Content/note/metadata templates use only `{{param}}` placeholders. An entire JSON string placeholder substitutes the parameter's typed `Value`; embedded placeholders use deterministic scalar rendering. Unknown/unbound placeholders, non-scalar embedded values, template keys, and rendered strings/metadata over 1 MiB are submission failures. No expression evaluation or code execution is permitted.
   - `UpsertMemory` IDs are deterministic SHA-256 slugs over namespace, action slug, canonical params, and effect index. Receipts record whether each upsert created or replaced its target. `LinkEdge` records `existed_before`; `TransitionTask` records complete before/after payloads and never permits `done`.
5. **Commit Mutations & Write `action_receipt`:**
   - Inside one Ladybug write turn and explicit DB transaction, revalidate every expected active head/version (including twin, task, created, and edge endpoint rows), apply all row/edge mutations, and write the receipt. Any error rolls back all DB writes; vectors and post-images are computed before the write turn, and derived indexes/caches update only after successful commit. No sequential-handler compensation is considered atomic.
   - Record a `memory_type = "action_receipt"` row with forced `eligibility: "private"`:
     ```json
     {
       "action_receipt": {
         "action_slug": "recalibrate-cooling-loop",
         "principal": "bosun",
         "executed_at": 1794675600,
         "params": { "twin_id": "loop-a", "target_temp": 42.5 },
         "superseded_pairs": [{ "before_id": "mem-old", "after_id": "mem-new" }],
         "created_ids": ["mem-new"],
         "twin_deltas": [{ "twin_id": "loop-a", "name": "temperature", "old": 65.0, "new": 42.5 }],
         "edge_deltas": [{ "source": "mem-new", "relation": "RELATES_TO", "target": "mem-2" }],
         "reverted_at": null,
         "reverted_by_receipt_id": null
       }
     }
     ```
6. **1-Call Revert (`revert_action`):**
   - Verify caller has `required_role` (`editor`/`owner`), check CAS that every affected after-row/version and edge is still the expected active head, and restore complete before-images for all five effects in reverse order: retire action-created rows, create restored superseding versions for replaced rows/twins/tasks, and remove only action-created edges. Write a separate private revert receipt in the same transaction.
   - Receipts are immutable within each bitemporal version: revert supersedes the active apply-receipt version with a new version carrying `reverted_at` + `reverted_by_receipt_id`; it never mutates the historical apply version in place. Double-revert detection reads the active receipt version.
   - `force: true` requires the authenticated `admin` capability or `owner` authorization on every affected target, creates new superseding versions rather than clearing later history, and records `forced_by` + `forced_at` on the revert receipt. Non-privileged force and stale non-force reverts return structured failures with zero writes.

### 5.3 RDF Projection & Surface Additions

- **RDF Projection (`src/rdf/projection.rs` & `src/rdf/shacl.rs`):**
  - Add `"action_type"` and `"action_receipt"` to `STRUCTURAL_MEMORY_TYPES` in `src/lexical.rs:65`, `src/rdf/shacl.rs:388` (`is_structural`), and default vector recall so receipts do not pollute normative recall unless queried via `neurostrata_action` or SPARQL (`rdf:type ns:ActionType`, `rdf:type ns:ActionReceipt`).
  - Keep `SHAREABLE_MEMORY_TYPES` (`src/synaptic_mesh/outbox.rs:117`) fail-closed for `action_receipt` (`eligibility: private` in `src/events/subscribers/mesh_outbox.rs`); if `action_type` definitions opt into `eligibility: shareable`, explicitly add `"action_type"` to `SHAREABLE_MEMORY_TYPES`.
- **MCP Tool (`src/server.rs`):** `neurostrata_action({ namespace, action: "define" | "validate" | "apply" | "revert" | "log", definition?, slug?, params?, receipt_id?, principal?, force?, limit? })`. `define` requires `definition` (an `ActionTypeDefinition` JSON object); `force` defaults to `false` and is honored only for an `owner`/`admin` revert; `limit` bounds `log` results.
- **Daemon & CLI (`src/daemon.rs`, `src/main.rs`):** `POST /action` and `neurostrata-mcp action <define|validate|apply|revert|log>` call the same application service as MCP. `log` returns only active receipt versions, newest first, bounded by `limit` (default 100, maximum 1000), and redacts params, before/after content, and template values; full private receipt payloads remain available only through authorized local storage/SPARQL reads.

### 5.4 Formal Verification (`src/formal_kernel.rs`, `proofs/NeuroStrata/KineticAction.lean`, `tests/lean_parity_test.rs`)

Because kinetic `Action Types` introduce a security-critical submission gate, a bitemporal CAS revert state machine, and a `TransitionTask` effect that must never bypass Lock 2 (`neurostrata_task_complete`), its pure decision algebra is extracted into the zero-dependency Aeneas-compliant kernel (`src/formal_kernel.rs`) and formally proved in Lean 4 (`proofs/NeuroStrata/KineticAction.lean`, imported by `proofs/NeuroStrata.lean`):

1. **Pure Kernel Functions (`src/formal_kernel.rs`):**
   - `pub const fn evaluate_action_gate(principal_authorized: bool, params_valid: bool, sparql_ask_passed: bool, twin_bounds_passed: bool, post_shacl_passed: bool) -> ActionGateDecision`
     - Returns `ActionGateDecision::Allow` iff all five predicates hold; returns the deterministic first-failing `Reject*` variant otherwise.
   - `pub const fn evaluate_action_revert(receipt_state: ActionReceiptState, head_cas_matches: bool, force_requested: bool, caller_is_owner_or_admin: bool, caller_is_editor_or_above: bool) -> ActionRevertDecision`
     - Models `Active` vs. `AlreadyReverted` receipts, active-head CAS verification, and forced-revert privilege gating (`RevertCommit`, `ForcedRevertCommit`, `RejectAlreadyReverted`, `RejectPermissionDenied`, `RejectStaleHeadConflict`).
   - `pub const fn evaluate_action_task_effect(current: TaskStatus, requested: ActionTaskTargetStatus) -> Result<TaskStatus, MachineError>`
     - Proves that `ActionEffectTemplate::TransitionTask` only permits transitions to `Open`, `InProgress`, or `Blocked` and unconditionally rejects `Done` (`DoneRequiresComplete`), preserving Lock 2 memory-extraction invariants.
   - `pub const fn is_action_memory_shareable(kind: ActionMemoryKind, explicit_shareable: bool) -> bool`
     - Proves `ActionMemoryKind::ActionReceipt` is unconditionally private (`false`) regardless of caller metadata, while `ActionMemoryKind::ActionType` is shareable iff `explicit_shareable == true`.
2. **Lean 4 Theorems (`proofs/NeuroStrata/KineticAction.lean`):**
   - `action_gate_sound`: `evaluate_action_gate ... = .allow ↔ (principal_authorized ∧ params_valid ∧ sparql_ask_passed ∧ twin_bounds_passed ∧ post_shacl_passed)`.
   - `action_revert_no_double_revert`: `evaluate_action_revert .alreadyReverted cas force admin editor = .rejectAlreadyReverted`.
   - `action_revert_stale_head_requires_privileged_force`: when `head_cas_matches = false`, `evaluate_action_revert` never commits unless `force_requested = true ∧ caller_is_owner_or_admin = true`.
   - `action_task_effect_never_reaches_done`: `∀ s req, evaluate_action_task_effect s req ≠ .ok .done`.
   - `action_receipt_never_shareable`: `∀ b, is_action_memory_shareable .actionReceipt b = false`.
3. **Exhaustive Parity (`tests/lean_parity_test.rs`):**
   - Enumerates all $2^5 = 32$ `evaluate_action_gate` rows, all $2 \times 2^4 = 32$ `evaluate_action_revert` rows, all $4 \times 4 = 16$ `evaluate_action_task_effect` rows, and all $2 \times 2 = 4$ `is_action_memory_shareable` rows against `lake --dir proofs exe neurostrata-proofs`.

## 6. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| SPARQL injection via string interpolation in `SparqlAsk` | Bind action parameters into the SPARQL query exclusively by converting each parameter to an `oxrdf::Term` (`Literal` or validated `NamedNode`) — never raw string concatenation into SPARQL syntax. |
| Reverting an action whose target row was subsequently modified by a second action | `revert_action` checks that each `after_id` row is still the active (un-superseded) head (`valid_to == null`). If a newer version exists, `revert_action` refuses with `ActionError::ConflictWithNewerVersion { receipt_id, superseded_by }` unless `force: true` is passed by an `owner`/`admin`. |
| Mesh leakage of operational action logs | Mark `action_receipt` rows `eligibility: private` in `src/events/subscribers/mesh_outbox.rs` (excluded from `SHAREABLE_MEMORY_TYPES` in `src/synaptic_mesh/outbox.rs:117`) so receipts never enter the SynapticMesh outbox. |

## 7. Checkable Acceptance Criteria

1. Defining an `action_type` with a `SparqlAsk` submission criterion and a `SetTwinState` effect succeeds and compiles into the namespace's `OntologyIndex`.
2. Calling `neurostrata_action({ action: "apply" })` when the `SparqlAsk` criterion evaluates to `false` (or when `rebac::allowed_with_roles` returns `false` for a `"viewer"` principal) performs zero database writes and returns structured `submission_failures`.
3. Calling `neurostrata_action({ action: "apply" })` when all criteria pass updates the target `digital_twin` state (incrementing `metadata.twin.version`), writes an `action_receipt` row, and projects `ns:ActionReceipt` quads in `memory_to_quads`.
4. Calling `neurostrata_action({ action: "revert", receipt_id })` restores the target twin's previous variable values via a new superseding version, marks the original receipt `reverted_by_receipt_id`, and refuses double-revert.
5. `cargo tree` confirms zero `oxirs-core`, `scirs2`, or `horned-owl` dependencies.
6. All five effects apply and revert through complete before-images; `LinkEdge` removes only action-created edges and `TransitionTask` restores its exact prior status/note/history without using the `done` transition.
7. A fault after the first transactional mutation, a stale expected head, malformed/non-`ASK`/`SERVICE` criteria, strict SHACL failure, and cross-namespace target each produce zero effect/receipt writes.
8. Raw malformed `action_type` ingest remains warn-first and uncompiled; strict `define` rejects it. Typed ASK parameters containing quotes or SPARQL syntax cannot alter query structure.
9. Non-admin principal impersonation, viewer execution, and non-owner/non-admin forced revert fail closed. Forced owner/admin revert is audited and preserves later history through new versions.
10. Receipts remain private, absent from mesh import/export/outbox and default recall; `log` is bounded, deterministic, and redacted. MCP, daemon, and CLI expose identical structured outcomes.
11. `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs` compiles standalone with zero external crates, `lake --dir proofs build` verifies `NeuroStrata/KineticAction.lean` with zero `sorry`, and `cargo test --locked --test lean_parity_test` passes all 84 KineticAction truth-table rows.

## 8. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-undx` to produce the step-by-step TDD implementation plan.
