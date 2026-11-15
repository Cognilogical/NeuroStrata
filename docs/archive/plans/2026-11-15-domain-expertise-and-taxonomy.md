# Subject-Matter Domain Expertise, SKOS Concept Taxonomy & Epistemic Briefing Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver NeuroStrata's Subject-Matter Domain Expertise, SKOS Concept Taxonomy, and Epistemic Briefing & Frontier Advisory Engine (`neurostrata_taxonomy`, Vocabulary v11 `thesis` + `concept`, Lean 4 formal kernel `proofs/NeuroStrata/Taxonomy.lean`, W3C SKOS RDF projection, and `v1.18.0` Scenario transactional CAS fix) as a strictly additive layer on `feat/kinetic-actions`.

**Architecture:** Fix the pre-release `commit_scenario` TOCTOU race by adding `ExpectedHead::RowFingerprint` verified inside `LadybugStore::commit_action_batch`. Add pure, Lean-proven decision functions (`classify_epistemic_status`, `validate_taxonomy_dag`, `validate_skos_disjointness`, `compute_epistemic_coverage_bps`, `compute_frontier_leverage_bps`) in `src/formal_kernel.rs` and `proofs/NeuroStrata/Taxonomy.lean`. Hydrate unrestricted `concept` rows into `OntologyIndex` (`src/rdf/ontology.rs`) and W3C SKOS RDF quads (`src/rdf/projection.rs`), expose the 7-operation unified `neurostrata_taxonomy` service across MCP, HTTP daemon (`POST /taxonomy`), and CLI (`neurostrata-mcp taxonomy`), integrate context-budgeted auto-briefings into `neurostrata_get_snapshot` and `subagent_prompt_injection`, and seed NeuroStrata's own domain thesis and taxonomy.

**Tech Stack:** Rust (`tokio`, `serde`, `serde_json`, `oxrdf`, `spargebra`, `spareval`), LadybugDB embedded property graph, Lean 4 (`lake build`).

**Spec:** `docs/superpowers/specs/2026-11-15-domain-expertise-and-taxonomy-design.md`

## Global Constraints

- Strictly additive: 100% of existing memory, task lifecycle, code intelligence (`neurostrata-mtqx`), and enterprise ontology (`neurostrata-1mus`) features and tests must continue passing without regression.
- Competitor Naming Prohibition (`9e64a5a8-aba4-4034-abfe-8b478bd9950e`): Never include competitor company, brand, or product names in any file, spec, plan, symbol, test, or comment.
- License-Compatible Libraries (`227984e4-780c-4fe4-a44c-286d7d14eabf`): Track SPDX license provenance on every `ComparableImplementation`.
- Formal Verification Guard (`b0813917-a806-4faa-8444-f7acec555ed9`): Zero `sorry` and zero `axiom` in `proofs/NeuroStrata/*.lean`; 100% parity tested in `tests/lean_parity_test.rs`.
- Privacy & ReBAC: Default `thesis` and `concept` `eligibility` to `"private"`; enforce `is_row_visible_to_principal` across all taxonomy operations; exclude `access_policy`-restricted rows from shared `OntologyIndex`, `get_snapshot`, `subagent_prompt_injection`, and `external_research_agenda`.

## Review Focus

1. Concurrent live mutation between `commit_scenario` precheck and `commit_action_batch` write lock must be rejected with `rejected_base_conflict`, rolling back all writes and preserving the scenario.
2. Transitive SKOS S27 disjointness: if `A -> B -> C` via `broader`, linking `A` and `C` via `related_concepts` or `contrasts_with` (in either direction) must be rejected on `upsert_concept`.
3. Restricted (`access_policy.is_some()`) concepts must never appear in another principal's `tree`, `briefing`, `compare`, `gaps`, `advise`, `OntologyIndex` alias expansion, or `external_research_agenda`.
4. Archiving or superseding a governing `rule`/`fact` row must immediately recompute a dependent concept's `epistemic_status` downward from `verified` to `synthesized` at read time.
5. `advise` on concepts that are `"implemented"` without `current_limitations` or `frontier_opportunities` must return `leverage_score_bps == 0` and list them in `insufficient_evidence_concepts` rather than generating hollow advice.

---

### Task 1: Fix `commit_scenario` Transactional CAS (`ExpectedHead::RowFingerprint`)

**Files:**
- Modify: `src/rdf/action.rs`
- Modify: `src/store/ladybug.rs:26707-26915`
- Modify: `src/rdf/scenario.rs:1775-1865`

**Interfaces:**
- Produces:
  - `ExpectedHead::RowFingerprint { row_id: String, expected_fingerprint: u64 }` in `src/rdf/action.rs`
  - Transactional verification of `ExpectedHead::RowFingerprint` inside `LadybugStore::commit_action_batch` using `crate::rdf::scenario::fingerprint_live_row`
  - Atomic base-fingerprint CAS enforcement in `crate::rdf::scenario::commit_scenario`

- [ ] **Step 1: Write the failing test in `src/rdf/scenario.rs`**

Add test `commit_scenario_rejects_live_mutation_via_transactional_expected_heads` verifying that `commit_action_batch` with `ExpectedHead::RowFingerprint` rejects a mismatched live row fingerprint with `ActionError::ConflictWithNewerVersion` and that `commit_scenario` populates `expected_heads` from `scenario.base_fingerprints` and preserves the scenario on conflict.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib rdf::scenario::tests::commit_scenario_rejects_live_mutation_via_transactional_expected_heads`
Expected: FAIL (`ExpectedHead::RowFingerprint` does not exist)

- [ ] **Step 3: Implement `ExpectedHead::RowFingerprint` in `src/rdf/action.rs`, `src/store/ladybug.rs`, and `src/rdf/scenario.rs`**

In `src/rdf/action.rs`, add `RowFingerprint { row_id: String, expected_fingerprint: u64 }` to `ExpectedHead`.
In `src/store/ladybug.rs` (`commit_action_batch`), query `m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata` for `row_id`, compute `crate::rdf::scenario::fingerprint_live_row(live_payload.as_ref(), now_ts)`, and return `Err(ActionError::ConflictWithNewerVersion { receipt_id: row_id.clone(), superseded_by: format!("fingerprint:{actual_fp:016x}") })` on mismatch.
In `src/rdf/scenario.rs` (`commit_scenario`), populate `expected_heads` from `scenario.base_fingerprints` and map `Err(ActionError::ConflictWithNewerVersion { receipt_id, .. })` to `Ok(ScenarioCommitOutcome::Rejected { decision: ScenarioCommitDecision::RejectedBaseConflict.as_tag().to_string(), .. })` before `store.scenarios().remove(...)`. Also trigger `refresh_ontology_needed` when `payload.memory_type` is `"concept"` or `"thesis"`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib rdf::scenario::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/rdf/action.rs src/store/ladybug.rs src/rdf/scenario.rs
git commit -m "fix(scenario): enforce atomic base-row fingerprint CAS inside commit_action_batch (neurostrata-f0bn)"
```

---

### Task 2: Lean 4 Formal Kernel & Parity Suite for Taxonomy & Frontier Leverage

**Files:**
- Modify: `src/formal_kernel.rs`
- Create: `proofs/NeuroStrata/Taxonomy.lean`
- Modify: `proofs/NeuroStrata.lean`
- Modify: `tests/lean_parity_test.rs`

**Interfaces:**
- Produces in `src/formal_kernel.rs`:
  - `pub enum EpistemicStatus { Seed, Defined, Synthesized, Verified }` with `pub const fn weight_bps(self) -> u32` and `pub const fn as_str(self) -> &'static str`
  - `pub const fn classify_epistemic_status(has_definition: bool, has_taxonomy_link: bool, has_synthesis: bool, has_empirical_grounding: bool) -> EpistemicStatus`
  - `pub fn validate_taxonomy_dag(broader_edges: &[(u32, u32)]) -> bool`
  - `pub fn validate_skos_disjointness(broader_edges: &[(u32, u32)], related_edges: &[(u32, u32)], contrast_edges: &[(u32, u32)]) -> bool`
  - `pub fn compute_epistemic_coverage_bps(statuses: &[EpistemicStatus], unmapped_domain_count: u32) -> u32`
  - `pub const fn realization_status_gap_bps(status: &str) -> u32`
  - `pub const fn compute_frontier_leverage_bps(axis_alignment_bps: u32, realization_gap_bps: u32, canonical_connectivity_count: u32, has_frontier_or_limitation: bool, has_references_or_comparables: bool) -> u32`

- [ ] **Step 1: Write failing parity tests in `tests/lean_parity_test.rs` and unit tests in `src/formal_kernel.rs`**

Test `classify_epistemic_status` monotonicity and `Verified` soundness, `validate_taxonomy_dag` cycle rejection (1-cycle, 2-cycle, 3-cycle, diamond DAG valid), `validate_skos_disjointness` transitive S27 rejection (`0 -> 1 -> 2` in `broader` with `(0, 2)` or `(2, 0)` in `related_edges` or `contrast_edges`), `compute_epistemic_coverage_bps` bounds `[0, 10000]`, and `compute_frontier_leverage_bps` gating, bounds, and monotonicity.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test lean_parity_test`
Expected: FAIL (missing taxonomy kernel functions)

- [ ] **Step 3: Implement the kernel functions in `src/formal_kernel.rs` and prove all 6 theorems in `proofs/NeuroStrata/Taxonomy.lean`**

Export `NeuroStrata.Taxonomy` in `proofs/NeuroStrata.lean`. Ensure zero `sorry` and zero `axiom`.

- [ ] **Step 4: Run `lake build` and `cargo test --test lean_parity_test`**

Run: `cd proofs && lake build && cd .. && cargo test --test lean_parity_test`
Expected: PASS with zero warnings and zero `sorry`

- [ ] **Step 5: Commit**

```bash
git add src/formal_kernel.rs proofs/NeuroStrata/Taxonomy.lean proofs/NeuroStrata.lean tests/lean_parity_test.rs
git commit -m "feat(formal-kernel): add Lean-verified SKOS taxonomy, S27 disjointness & frontier leverage kernel (neurostrata-f0bn)"
```

---

### Task 3: Vocabulary v11, Privacy-Safe `OntologyIndex` Hydration & W3C SKOS RDF Projection

**Files:**
- Modify: `src/schemas/memory-vocabulary.v3.json`
- Modify: `src/synaptic_mesh.rs`
- Modify: `src/rdf/ontology.rs`
- Modify: `src/rdf/projection.rs`
- Modify: `src/store/ladybug.rs`

**Interfaces:**
- Produces:
  - Vocabulary v11 with `thesis` and `concept` in `src/schemas/memory-vocabulary.v3.json`
  - `"thesis"` and `"concept"` added to `SHAREABLE_MEMORY_TYPES` in `src/synaptic_mesh.rs` (only rows explicitly marked `eligibility: "shareable"` sync; default on write is `"private"`)
  - `ConceptIndexEntry` and `OntologyIndex::{concepts, concept_aliases, related, contrasts_with, load_concept_rows, concept_narrower_of, concept_broader_of, expand_concept_query_terms}` in `src/rdf/ontology.rs`
  - W3C SKOS quad projection (`rdf:type skos:Concept`, `skos:prefLabel`, `skos:altLabel`, `skos:definition`, `skos:broader`, `skos:narrower`, `skos:related`, `ns:contrastsWith`, `ns:epistemicStatus`) in `src/rdf/projection.rs`
  - Unrestricted `concept` hydration in `LadybugStore::refresh_ontology` (`src/store/ladybug.rs`)

- [ ] **Step 1: Write failing unit tests in `src/rdf/ontology.rs` and `src/rdf/projection.rs`**

Test multi-target `concept_aliases` collision preservation, derived `narrower` from canonical `broader`, strict separation from `sub_classes`, exclusion of `access_policy`-restricted concepts from `load_concept_rows`, and W3C SKOS quad emission in `project_Namespace_to_quads` / `project_memory`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib rdf::ontology::tests rdf::projection::tests`
Expected: FAIL

- [ ] **Step 3: Implement Vocabulary v11, `OntologyIndex` concept hydration, `refresh_ontology` extension, and SKOS RDF projection**

Update any test asserting `vocabulary_version == 10` to `11`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib rdf::ontology::tests rdf::projection::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/schemas/memory-vocabulary.v3.json src/synaptic_mesh.rs src/rdf/ontology.rs src/rdf/projection.rs src/store/ladybug.rs
git commit -m "feat(ontology): add Vocabulary v11 thesis/concept, SKOS RDF projection & OntologyIndex hydration (neurostrata-f0bn)"
```

---

### Task 4: Core Domain Expertise & Taxonomy Engine (`src/rdf/taxonomy.rs`)

**Files:**
- Create: `src/rdf/taxonomy.rs`
- Modify: `src/rdf/mod.rs`
- Modify: `src/graph_read/object_view.rs`

**Interfaces:**
- Produces in `src/rdf/taxonomy.rs`:
  - `ThesisSpec`, `ComparableImplementation`, `ConceptSpec`, `HydratedConcept`, `OntologyAntiPattern`, `ExpertRecommendation`, `ResearchPrompt`, `TaxonomyRequest`, `TaxonomyResponse`
  - `pub async fn execute_taxonomy(store: &LadybugStore, embedder: &dyn Embedder, req: TaxonomyRequest, security: &ActionSecurityContext) -> Result<Value, String>` supporting all 7 operations:
    1. `set_thesis` (singleton CAS via `commit_action_batch`)
    2. `upsert_concept` (unique slug CAS, DAG cycle check, transitive S27 + contrast disjointness check, interface/action_type referential check, atomic edge writes)
    3. `tree` (principal-filtered, cycle-guarded, derived `narrower`, `descendant_count`, `grounding_count`)
    4. `briefing` (principal-filtered, RRF/lexical + `concept_aliases` seed match, deterministic Forward-Expand / Backward-Prune neighborhood subgraph)
    5. `compare` (principal-filtered multi-perspective comparative matrix across `evaluation_axes`, LCA `shared_ancestors`, 2-hop contrast/related paths, 3 perspectives)
    6. `gaps` (principal-filtered `epistemic_coverage_bps`, `unmapped_domains`, `dangling_concept_refs`, `shallow_concepts`, `anti_patterns`)
    7. `advise` (principal-filtered `compute_frontier_leverage_bps` ranking, tie-breaking, `canonical_grounding`, `insufficient_evidence_concepts`, `non_goal_assessment`, public-safe `external_research_agenda`)
  - Hydration of `concept` and `thesis` in `src/graph_read/object_view.rs`

- [ ] **Step 1: Write failing comprehensive tests in `src/rdf/taxonomy.rs`**

Cover:
1. Singleton `set_thesis` supersession and unique `upsert_concept` slug supersession under CAS.
2. Transitive SKOS S27 disjointness rejection (`A -> B -> C` broader + `A <-> C` related or contrasts_with) and DAG cycle rejection.
3. Dynamic read-time `epistemic_status` recomputation when governing `rule`/`fact` rows are added and then archived/retired.
4. Two-principal ReBAC non-leakage across `tree`, `briefing`, `compare`, `gaps`, `advise`, and `external_research_agenda`.
5. `briefing` Forward-Expand / Backward-Prune pruning ungrounded outer leaf nodes while preserving bridging concepts.
6. `compare` multi-perspective matrix and `advise` frontier leverage ranking, tie-breaking, `insufficient_evidence_concepts`, and `non_goal_assessment`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib rdf::taxonomy::tests`
Expected: FAIL

- [ ] **Step 3: Implement `src/rdf/taxonomy.rs` and wire `concept`/`thesis` hydration into `src/graph_read/object_view.rs`**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib rdf::taxonomy::tests graph_read::object_view::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/rdf/taxonomy.rs src/rdf/mod.rs src/graph_read/object_view.rs
git commit -m "feat(taxonomy): implement 7-op SKOS taxonomy, briefing, comparison & SME frontier advisor engine (neurostrata-f0bn)"
```

---

### Task 5: Unified MCP, HTTP Daemon, CLI, Auto-Briefing & Self-Seeding

**Files:**
- Create: `src/handlers/taxonomy.rs`
- Modify: `src/handlers/mod.rs`
- Modify: `src/server.rs`
- Modify: `src/daemon.rs`
- Modify: `src/main.rs`
- Modify: `src/task/mod.rs`
- Modify: `.agents/skills/neurostrata/SKILL.md`
- Modify: `AGENTS.md`
- Modify: `Cargo.toml`

**Interfaces:**
- Produces:
  - `neurostrata_taxonomy` MCP tool in `src/server.rs`
  - `POST /taxonomy` route in `src/daemon.rs`
  - `neurostrata-mcp taxonomy <set-thesis|upsert-concept|tree|briefing|compare|gaps|advise>` CLI subcommand in `src/main.rs`
  - Context-budgeted `### Project Thesis & Domain Expertise` in `neurostrata_get_snapshot` (`<= 800` chars) and `project_thesis` + `domain_concepts` in `build_task_graph_context` / `subagent_prompt_injection` (`<= 1200` chars, unrestricted concepts only, preserving `golden_rules` priority) in `src/task/mod.rs`
  - Version bump to `1.18.0` in `Cargo.toml`

- [ ] **Step 1: Write failing integration tests in `src/task/mod.rs` and `src/handlers/taxonomy.rs`**

Verify that `build_task_graph_context` and `get_snapshot` include unrestricted `project_thesis` and `domain_concepts` within budget while excluding `access_policy`-restricted concepts, and verify MCP/HTTP/CLI handler parity.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib handlers::taxonomy task::tests`
Expected: FAIL

- [ ] **Step 3: Implement `src/handlers/taxonomy.rs`, wire MCP/HTTP/CLI surfaces, add auto-briefing in `src/task/mod.rs`, update docs, and bump `Cargo.toml` to `1.18.0`**

- [ ] **Step 4: Build release binary, seed NeuroStrata's own Project Thesis & Domain Concept Graph, and run full verification**

Run `cargo test --lib && cargo test --test lean_parity_test && cargo build --release`, then seed NeuroStrata's own `thesis` and foundational `concept` rows across all 6 core pillars and verify `neurostrata_taxonomy` (`tree`, `briefing`, `compare`, `gaps`, `advise`).

- [ ] **Step 5: Commit**

```bash
git add src/handlers/taxonomy.rs src/handlers/mod.rs src/server.rs src/daemon.rs src/main.rs src/task/mod.rs .agents/skills/neurostrata/SKILL.md AGENTS.md Cargo.toml Cargo.lock
git commit -m "feat(v1.18.0): wire neurostrata_taxonomy across MCP/Daemon/CLI and auto-briefing in snapshot & tasks (neurostrata-f0bn)"
```
