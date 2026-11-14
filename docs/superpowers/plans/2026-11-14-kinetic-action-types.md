# Kinetic Action Types & Reversible Action Log Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver governed, namespace-local Action Types with typed submission gates, atomic five-effect mutations, private receipts, complete one-call revert, and MCP/daemon/CLI parity.

**Architecture:** Parse definitions into the namespace `OntologyIndex`; validate parameters, trusted-principal ReBAC, offline bound SPARQL `ASK`, and twin criteria before staging. Stage complete before/post-images and embeddings, strictly validate them, then commit every row/edge/receipt through one Ladybug transaction with expected-head CAS. Revert builds inverse writes from private receipt deltas and uses the same transaction seam.

**Tech Stack:** Rust, Tokio, Serde/serde_json, LadybugDB `lbug 0.22.3`, existing `oxrdf`/`spargebra`/`spareval`/`sparesults`, `arc-swap`, `sha2`, existing `Embedder`, Axum, Clap.

**Spec:** `docs/superpowers/specs/2026-11-14-kinetic-action-types-and-revert-log-design.md`

## Global Constraints

- Core execution is deterministic, offline Rust with zero LLM calls; action `ASK` rejects `SERVICE`, updates, non-`ASK` forms, timeouts, and over-budget algebra.
- Raw ingest/`neurostrata_add_memory` remains warn-first. Strict `define`, `validate`, `apply`, and `revert` return structured failures; rejected/stale/conflicting operations perform zero effect or receipt writes.
- Resolve effective principals with `rebac::resolve_principal(authenticated_principal, requested_principal)`. Only authenticated admin may impersonate; forced revert requires authenticated admin or owner on every target.
- Resolve every target only in the invocation namespace. Receipts force `eligibility: "private"`, never enter mesh import/export/outbox, and stay outside default recall.
- Use `PreparedQuery::substitute_variable` with typed `oxrdf::Term`; never interpolate parameter values into SPARQL source.
- Templates use `{{param}}` only; preserve JSON types for whole-value placeholders, reject unresolved/non-scalar embedded placeholders, and cap rendered strings/metadata at 1 MiB.
- All five effects and receipt writes share one explicit DB transaction and expected-head CAS. Never call sequential public handlers and label the result atomic.
- Apply/revert receipts are immutable within each bitemporal version. Revert supersedes the active receipt version and writes a separate revert receipt; never mutate history in place.
- `TransitionTask` permits only `open`, `in_progress`, and `blocked`; Lock 2 still owns `done`.
- Use suitable license-compatible libraries. Existing Oxigraph crates provide required RDF/binding functionality; no new dependency is currently justified.
- Do not include the pre-existing `.agents/skills/neurostrata/SKILL.md` modification in feature commits.

## Review Focus

- Unknown/missing/non-finite params, unresolved templates, and oversized renders: expect named structured failures and no writes; pinned in Tasks 2–3.
- Forged principals, cross-namespace IDs, wrong target types, and unauthorized edge endpoints: expect fail-closed behavior without data disclosure; pinned in Task 2.
- Quotes/SPARQL syntax in values, malformed queries, `SERVICE`, timeout, and algebra-budget excess: expect no injection/network and no writes; pinned in Task 2.
- Failure after a first write or concurrent head changes: expect complete rollback/conflict with no partial receipt or derived-index update; pinned in Task 4.
- Pre-existing edges, same-ID upserts, task history, stale/double/forced revert, and receipt privacy/redaction: expect exact restoration and no operational-log leakage; pinned in Tasks 5–6.

---

### Task 1: Action Schema & Namespace Ontology Compilation

**Files:**
- Create: `src/rdf/action.rs`
- Modify: `src/rdf/mod.rs`, `src/rdf/ontology.rs`, `src/store/ladybug.rs`

**Interfaces:**
- Produces the spec's `ActionTypeDefinition`, `ActionParamSpec`, `SubmissionCriterion`, and `ActionEffectTemplate` serde types.
- Produces `ActionTypeDefinition::from_payload(&MemoryPayload) -> Result<Self, String>` and `validate_definition(&self) -> Vec<ShaclViolation>`.
- Extends `OntologyIndex` with `action_types: BTreeMap<String, ActionTypeDefinition>`.

- [ ] **Step 1: Write failing schema/compilation tests**

```rust
#[test] fn action_type_serde_round_trip_preserves_all_five_effects() { /* assert_eq! */ }
#[test] fn definition_rejects_bad_slug_role_bounds_and_non_ask_service() { /* codes */ }
#[tokio::test] async fn valid_definition_compiles_only_in_its_namespace() { /* A present, B absent */ }
#[tokio::test] async fn malformed_raw_definition_warns_but_does_not_compile_or_block_write() { /* stored + warning */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked rdf::action -- --nocapture`
Expected: compile failure because `rdf::action` and `OntologyIndex::action_types` do not exist.

- [ ] **Step 3: Implement schema validation and compilation**

Implement canonical kebab-case slugs, `editor|owner` roles, valid parameter bounds/enums/target types, non-empty effects/templates, `open|in_progress|blocked` task statuses, and parse-time single read-only `ASK`/no-`SERVICE` criteria. During ontology refresh, list active `action_type` rows in deterministic `(valid_from, id)` order; skip invalid rows with warnings and compile the newest active row per slug. Refresh after raw upsert/delete/supersession of `action_type`; strict duplicate handling belongs to Task 7 `define`.

- [ ] **Step 4: Verify GREEN and raw-path compatibility**

Run: `cargo test --locked rdf:: -- --nocapture` and `cargo test --locked store::ladybug::tests -- --nocapture`
Expected: new tests and existing ontology/store tests PASS.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/rdf/action.rs src/rdf/mod.rs src/rdf/ontology.rs src/store/ladybug.rs
git commit -m "feat(ontology): compile kinetic action definitions"
```

### Task 2: Typed Submission Gates & Offline Bound `ASK`

**Files:**
- Modify: `src/rdf/action.rs`, `src/rdf/sparql.rs`, `src/rebac.rs`

**Interfaces:**
- Produces `ActionSecurityContext { authenticated_principal: String, requested_principal: Option<String> }` and `ActionInvocation { namespace, slug, security, params }`.
- Produces `evaluate_action(&LadybugStore, &ActionInvocation) -> Result<ActionEvaluation, ActionError>`; `ActionEvaluation` carries the definition, effective principal, bound params, targets, and `Vec<ActionSubmissionFailure>`.
- Produces `execute_action_ask(&LadybugStore, &str, &str, &[(Variable, Term)], SparqlConfig) -> Result<bool, String>`.

- [ ] **Step 1: Write failing gate and injection tests**

```rust
#[tokio::test] async fn submission_rejects_unknown_missing_wrong_type_nonfinite_and_bounds() { /* named failures */ }
#[tokio::test] async fn targets_are_typed_namespace_local_and_rebac_checked_for_every_effect() { /* no cross-ns leak */ }
#[tokio::test] async fn non_admin_cannot_impersonate_admin_and_viewer_cannot_apply() { /* fail closed */ }
#[tokio::test] async fn ask_binds_hostile_text_without_changing_query_structure() { /* typed false/true */ }
#[tokio::test] async fn malformed_select_service_update_timeout_and_large_algebra_fail_without_writes() { /* row count unchanged */ }
#[tokio::test] async fn twin_precondition_requires_finite_value_in_range() { /* boundaries inclusive */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked rdf::action -- --nocapture`
Expected: missing evaluation/binding APIs and failing security assertions.

- [ ] **Step 3: Implement pure binding and query evaluation**

Validate exact JSON types, required/unknown params, finite numbers, inclusive bounds, enums, target memory/schema types, and all target ACLs. Convert IDs to the canonical projected memory/twin IRIs and other values to typed RDF literals. Parse with `parser_with_prefixes`, require `Query::Ask`, reject federation, enforce default budgets, substitute variables on `PreparedQuery`, set default-graph union, and return `QueryResults::Boolean`; timeout is a submission failure.

- [ ] **Step 4: Verify GREEN and zero-write rejection**

Run: `cargo test --locked rdf:: -- --nocapture` and `cargo test --locked rebac::tests -- --nocapture`
Expected: all gate/injection tests PASS; rejection fixtures show identical row counts.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/rdf/action.rs src/rdf/sparql.rs src/rebac.rs
git commit -m "feat(actions): add typed submission criteria gates"
```

### Task 3: Five-Effect Staging, Templates & Strict Post-Image SHACL

**Files:**
- Modify: `src/rdf/action.rs`, `src/digital_twin/mod.rs`, `src/task/mod.rs`, `src/rdf/ontology.rs`

**Interfaces:**
- Produces `StagedMemoryWrite { id, vector, payload }`, `ExpectedHead`, `EdgeDelta`, `ActionWriteSet`, and `ActionStage`.
- Produces `stage_action(&LadybugStore, ActionEvaluation, &dyn Embedder, i64) -> Result<ActionStage, ActionError>`.
- `ActionWriteSet { expected_heads, writes, edge_deltas }` includes the private apply receipt in `writes`.

- [ ] **Step 1: Write failing staging tests for every effect**

```rust
#[tokio::test] async fn set_twin_state_stages_version_and_complete_delta() { /* old/new values */ }
#[tokio::test] async fn supersede_and_upsert_capture_full_before_images_and_created_ids() { /* same-ID replace too */ }
#[tokio::test] async fn link_edge_records_existed_before_and_task_transition_captures_history() { /* exact inverses */ }
#[test] fn templates_preserve_whole_json_types_and_reject_bad_or_oversized_placeholders() { /* {{param}} */ }
#[tokio::test] async fn any_staged_shacl_violation_rejects_all_rows_and_receipt_before_write() { /* zero rows */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked rdf::action -- --nocapture`
Expected: missing staging/write-set APIs.

- [ ] **Step 3: Implement deterministic staging**

Reuse `digital_twin::set_state` on cloned payloads, pure task transition machinery, and existing supersession inheritance conventions without calling their write handlers. Expand templates recursively, compute deterministic upsert IDs with SHA-256, compute new embeddings before storage, deduplicate repeated target writes in effect order, and capture full before payloads plus `RowDelta`/`TwinDelta`/`LinkEdgeDelta`/`TaskDelta`. Force receipt privacy. Validate each active post-image and receipt through all three existing SHACL/custom-shape/reasoning validators.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test --locked rdf::action -- --nocapture`, `cargo test --locked digital_twin::tests -- --nocapture`, and `cargo test --locked task::tests -- --nocapture`
Expected: all five stage shapes and strict rejection PASS; no store mutation occurs.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/rdf/action.rs src/digital_twin/mod.rs src/task/mod.rs src/rdf/ontology.rs
git commit -m "feat(actions): stage five effects with strict SHACL"
```

### Task 4: Atomic Ladybug Batch Commit & Expected-Head CAS

**Files:**
- Modify: `src/store/ladybug.rs`, `src/rdf/action.rs`

**Interfaces:**
- Produces `LadybugStore::commit_action_batch(&self, namespace: &str, batch: ActionWriteSet) -> Result<()>`.
- Consumes Task 3's fully embedded `ActionWriteSet`; durable IDs return only after commit.

- [ ] **Step 1: Write failing atomicity tests**

```rust
#[tokio::test] async fn action_batch_commits_every_row_edge_and_receipt_together() { /* all visible */ }
#[tokio::test] async fn fault_after_first_write_rolls_back_effects_receipt_edges_and_indexes() { /* pre-state exact */ }
#[tokio::test] async fn stale_expected_head_conflicts_before_any_write() { /* newer version preserved */ }
#[tokio::test] async fn derived_indexes_refresh_only_after_successful_commit() { /* no ghost hits */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked action_batch_ -- --nocapture`, `cargo test --locked stale_expected_head -- --nocapture`, and `cargo test --locked derived_indexes_refresh -- --nocapture`
Expected: missing `commit_action_batch` or observed partial state.

- [ ] **Step 3: Implement the narrow transaction seam**

Extract reusable connection-level row-write helpers from `upsert`; do not expose `write_lock` or accept arbitrary Cypher. In one deadline-bound write turn: `BEGIN TRANSACTION`, re-read and compare every expected active head/version, apply writes/edge changes/receipt, and `COMMIT`; on any error attempt `ROLLBACK` and return the original error. Add a `#[cfg(test)]` fail-after-N-writes hook to prove rollback. Defer FTS/ontology/cache mutation until commit succeeds and refresh them as one published post-commit state.

- [ ] **Step 4: Verify GREEN and regression safety**

Run: `cargo test --locked action_batch_ -- --nocapture` and `cargo test --locked store::ladybug::tests -- --nocapture`
Expected: rollback/CAS/index tests and all existing store tests PASS.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/store/ladybug.rs src/rdf/action.rs
git commit -m "feat(store): atomically commit action write sets"
```

### Task 5: Apply, Complete Revert & Redacted Action Log Service

**Files:**
- Modify: `src/rdf/action.rs`, `src/store/ladybug.rs`

**Interfaces:**
- Produces `apply_action(&LadybugStore, &ActionInvocation, &dyn Embedder) -> Result<ActionApplyOutcome, ActionError>`.
- Produces `revert_action(&LadybugStore, &RevertInvocation, &dyn Embedder) -> Result<ActionRevertOutcome, ActionError>`.
- Produces `action_log(&LadybugStore, &str, &ActionSecurityContext, usize) -> Result<Vec<ActionLogEntry>, ActionError>`; default/max limits are 100/1000.

- [ ] **Step 1: Write failing end-to-end apply/revert/log tests**

```rust
#[tokio::test] async fn apply_all_effects_then_revert_restores_complete_pre_state() { /* all five inverses */ }
#[tokio::test] async fn revert_refuses_stale_and_double_revert_without_writes() { /* structured conflict */ }
#[tokio::test] async fn force_requires_admin_or_owner_and_is_audited() { /* later history preserved */ }
#[tokio::test] async fn receipt_versions_are_immutable_and_log_is_bounded_newest_first_and_redacted() { /* no params/content */ }
#[tokio::test] async fn rejected_apply_has_no_receipt_and_validate_never_writes() { /* counts unchanged */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked apply_all_effects -- --nocapture`, `cargo test --locked revert_refuses -- --nocapture`, `cargo test --locked force_requires -- --nocapture`, `cargo test --locked receipt_versions -- --nocapture`, and `cargo test --locked rejected_apply -- --nocapture`
Expected: missing apply/revert/log service.

- [ ] **Step 3: Implement apply/revert/log**

Apply chains evaluation → staging → `commit_action_batch`. Revert loads the active logical receipt, authorizes the original definition and current targets, CAS-checks every after-head/edge, stages inverse writes in reverse effect order, restores full before-images with fresh bitemporal versions, supersedes the receipt version with revert markers, and commits a separate revert receipt atomically. Owner/admin force bypasses only head staleness, not validation or history preservation. Log selects active receipt versions, sorts `(executed_at, receipt_id)` descending, clamps limit, authorizes visibility, and omits params/content/before-images.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test --locked rdf::action::tests -- --nocapture`
Expected: apply/revert/log suite PASS, including every inverse and zero-write conflict.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/rdf/action.rs src/store/ladybug.rs
git commit -m "feat(actions): add atomic apply revert and action log"
```

### Task 6: RDF Projection, Structural Recall & Mesh Privacy

**Files:**
- Modify: `src/rdf/projection.rs`, `src/rdf/shacl.rs`, `src/lexical.rs`, `src/store/ladybug.rs`
- Modify: `src/events/subscribers/mesh_outbox.rs`, `src/synaptic_mesh/outbox.rs`, `src/synaptic_mesh/import.rs`, `src/synaptic_mesh/record.rs`

**Interfaces:**
- Projects `ns:ActionType` and `ns:ActionReceipt` plus deterministic definition/delta/revert-link quads.
- Keeps `action_receipt` absent from every shareable allowlist; adds `action_type` consistently only where shareable types are validated.

- [ ] **Step 1: Write failing projection/privacy tests**

```rust
#[test] fn action_types_and_receipts_project_expected_rdf_classes_and_revert_links() { /* exact quads */ }
#[tokio::test] async fn structural_actions_stay_out_of_default_vector_and_lexical_recall() { /* direct SPARQL still works */ }
#[test] fn shareable_action_type_round_trips_but_receipt_is_rejected_everywhere() { /* outbox + import + record */ }
#[test] fn forged_receipt_eligibility_is_private_and_log_fields_are_not_projected_publicly() { /* fail closed */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked action_types_and_receipts -- --nocapture`, `cargo test --locked structural_actions -- --nocapture`, `cargo test --locked shareable_action_type -- --nocapture`, and `cargo test --locked forged_receipt -- --nocapture`
Expected: missing RDF classes/structural exclusions; any receipt allowlist inclusion is a failure.

- [ ] **Step 3: Implement projection and fail-closed exclusions**

Add both structural memory types at every exclusion call site, project canonical action metadata and lineage, force private receipt eligibility during staging/export, and thread shareable `action_type` through subscriber/worker/import/record checks together. Do not add receipts to `SHAREABLE_MEMORY_TYPES` or mesh import allowlists.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test --locked rdf::projection -- --nocapture`, `cargo test --locked lexical::tests -- --nocapture`, `cargo test --locked synaptic_mesh::outbox::tests -- --nocapture`, and `cargo test --locked synaptic_mesh::import::tests -- --nocapture`
Expected: projection/recall/privacy tests PASS.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/rdf/projection.rs src/rdf/shacl.rs src/lexical.rs src/store/ladybug.rs src/events/subscribers/mesh_outbox.rs src/synaptic_mesh
git commit -m "feat(rdf): project actions while keeping receipts private"
```

### Task 7: Strict Define, MCP, Daemon & CLI Parity

**Files:**
- Modify: `src/server.rs`, `src/daemon.rs`, `src/main.rs`

**Interfaces:**
- Adds `neurostrata_action({ namespace, action, definition?, slug?, params?, receipt_id?, principal?, force?, limit? })`.
- Adds `POST /action` and `neurostrata-mcp action <define|validate|apply|revert|log>`, all returning the same service JSON.

- [ ] **Step 1: Write failing surface contract tests**

```rust
#[tokio::test] async fn tool_list_and_mcp_dispatch_expose_all_five_action_operations() { /* schema + outcomes */ }
#[tokio::test] async fn define_is_strict_about_schema_and_duplicate_active_slug() { /* no write on failure */ }
#[tokio::test] async fn daemon_action_route_matches_mcp_and_cli_parses_every_subcommand() { /* parity */ }
#[tokio::test] async fn surface_resolves_principal_before_service_and_never_echoes_secret_params() { /* redaction */ }
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked tool_list_and_mcp -- --nocapture`, `cargo test --locked define_is_strict -- --nocapture`, `cargo test --locked daemon_action_route -- --nocapture`, and `cargo test --locked surface_resolves -- --nocapture`
Expected: unknown tool/route/subcommand.

- [ ] **Step 3: Implement thin adapters**

Implement strict `define` using validated payload + normal warn-first storage compatibility, refresh ontology after success, and dispatch other operations to Task 5. Pass daemon/local authenticated identity separately from requested principal. Add one Axum route and Clap enums/JSON request rendering; do not duplicate action semantics in adapters.

- [ ] **Step 4: Verify GREEN and CLI smoke**

Run: `cargo test --locked server::tests -- --nocapture`, `cargo test --locked daemon::tests -- --nocapture`, and `cargo test --locked tests::the_action_commands_parse -- --nocapture`
Run: `cargo run --locked -- action --help`
Expected: contract tests PASS; help lists `define`, `validate`, `apply`, `revert`, `log`.

- [ ] **Step 5: Commit after git-mutation confirmation**

```bash
git add src/server.rs src/daemon.rs src/main.rs
git commit -m "feat(api): expose kinetic actions through MCP daemon and CLI"
```

### Task 8: Documentation, Full Gates & Independent Verification

**Files:**
- Modify: `AGENTS.md`
- Modify if behavior differs: `docs/superpowers/specs/2026-11-14-kinetic-action-types-and-revert-log-design.md`

- [ ] **Step 1: Add missing acceptance regression tests before documentation**

Map each spec acceptance item 1–10 to its named test above; add any missing test first and verify it fails for the intended reason. Include a mixed multi-effect action, raw warn-first compatibility, all rejection paths, all inverses, forced revert, redaction, and MCP/daemon/CLI parity.

- [ ] **Step 2: Update operational documentation**

Document strict-vs-warn-first boundaries, trusted principal model, offline criteria, transaction/conflict behavior, private/redacted receipts, force authorization, and the five tool/CLI operations in `AGENTS.md`. Keep the spec synchronized with actual behavior rather than documenting unimplemented aspirations.

- [ ] **Step 3: Run complete verification**

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
if cargo tree --locked | grep -E '^(oxirs-core|scirs2|horned-owl) | (oxirs-core|scirs2|horned-owl) v'; then exit 1; fi
git diff --check
```

Expected: all commands exit 0; baseline 1603 unit tests plus integration suites remain green with new action tests.

- [ ] **Step 4: Request fresh `checker-free` verification**

Provide spec, plan, complete diff, and command outputs. Require source-grounded confirmation of every acceptance criterion, privacy claim, atomic rollback/CAS claim, and no unrelated `.agents/skills/neurostrata/SKILL.md` inclusion. Fix findings through TDD and rerun affected plus full gates.

- [ ] **Step 5: Commit, integrate, complete, and push after explicit confirmation**

```bash
git add AGENTS.md docs/superpowers/specs/2026-11-14-kinetic-action-types-and-revert-log-design.md docs/superpowers/plans/2026-11-14-kinetic-action-types.md
git commit -m "docs(actions): document kinetic action operations and verification"
git pull --rebase
git push
git status
```

Complete `neurostrata-undx` only after memory extraction, strict task-gate success, and confirmed up-to-date remote state.
