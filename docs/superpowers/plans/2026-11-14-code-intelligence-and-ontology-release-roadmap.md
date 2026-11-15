# Sourcegraph Code Intelligence & Palantir Ontology Release Master Plan (`v1.18.0` on `feat/kinetic-actions`)

**Date:** 2026-11-14
**Target Release:** `v1.18.0` (dedicated feature release following `v1.17.3` on `main`)
**Integration Branch:** `feat/kinetic-actions`
**Umbrella Epics:**
- `neurostrata-mtqx` — *Epic: Sourcegraph-Inspired Code Intelligence & Retrieval Upgrades* (5 features)
- `neurostrata-1mus` — *Epic: Palantir Ontology Kinetic, Dynamic & Semantic Upgrades* (4 features)

---

## 1. Release Scope & 100% Feature Inventory

This release bundles **all 9 capabilities** identified from the Sourcegraph Code Intelligence audit (`neurostrata-mtqx`) and the Palantir Foundry Ontology audit (`neurostrata-1mus`) into a single cohesive release (`v1.18.0`). All specs, plans, Lean 4 formal-verification contracts, and implementation commits are isolated on `feat/kinetic-actions` so `v1.17.3` ships cleanly from `main` first.

### Part A — Sourcegraph-Inspired Code Intelligence Suite (Epic `neurostrata-mtqx`)

| # | Task ID | Feature | Design Spec | Primary Code Surfaces | MCP / CLI / HTTP Surface |
|---|---------|---------|-------------|----------------------|--------------------------|
| S1 | `neurostrata-5xjo` | True Hybrid RRF Search Fusion (`mode: "hybrid"`), Code-Aware Tantivy Sub-Tokenizer (`code_text`), & Graph-Expansion Slot Reservation | `docs/superpowers/specs/2026-11-14-hybrid-rrf-and-graph-slots-design.md` | `src/lexical.rs`, `src/store/ladybug.rs`, `src/traits.rs`, `src/server.rs`, `src/formal_kernel.rs` | `neurostrata_search_memory(mode: "hybrid")` |
| S2 | `neurostrata-rbde` | Two-Tier Cross-File Call Graph: Tier 1 Workspace Tree-Sitter Symbol Linker + Tier 2 SCIP (`index.scip`) Compiler-Grade Ingestion | `docs/superpowers/specs/2026-11-14-cross-file-and-scip-ingestion-design.md` | `src/parser/ingest.rs`, `src/parser/scip.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs` | `neurostrata_ingest_scip`, `POST /ingest-scip`, `neurostrata-mcp ingest-scip` |
| S3 | `neurostrata-r3as` (blocked by `5xjo`, `rbde`) | `neurostrata_locate` — Single-Call Code Finder, Blast-Radius & Governance Locator | `docs/superpowers/specs/2026-11-14-neurostrata-locate-code-finder-design.md` | `src/graph_read/locate.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs`, `plugins/core/index.ts` | `neurostrata_locate`, `POST /locate`, `neurostrata-mcp locate` |
| S4 | `neurostrata-737r` (blocked by `rbde`) | `neurostrata_diff_tour` — Guided Diff Tour with 4-Stop AST Topological Ordering, Unchanged-Caller Blast Radius & `GOVERNS` Audit | `docs/superpowers/specs/2026-11-14-guided-diff-tour-design.md` | `src/graph_read/diff_tour.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs`, `src/formal_kernel.rs` | `neurostrata_diff_tour`, `POST /diff-tour`, `neurostrata-mcp diff-tour` |
| S5 | `neurostrata-kx2n` | `neurostrata_insights` — Longitudinal Architectural & Governance Code Insights over the Bitemporal Graph & `TimeSeriesStore` | `docs/superpowers/specs/2026-11-14-architectural-code-insights-design.md` | `src/telemetry/insights.rs`, `src/telemetry/anomaly.rs`, `src/time_series.rs`, `src/registry.rs`, `src/server.rs`, `src/main.rs` | `neurostrata_insights`, `POST /insights`, `neurostrata-mcp insights` |

### Part B — Palantir Foundry Ontology Suite (Epic `neurostrata-1mus`)

| # | Task ID | Feature | Design Spec | Implementation Plan | Primary Code Surfaces | MCP / CLI / HTTP Surface |
|---|---------|---------|-------------|---------------------|----------------------|--------------------------|
| P1 | `neurostrata-undx` | Kinetic `Action Types`, Submission-Criteria Gatekeeping & Reversible Action Log | `docs/superpowers/specs/2026-11-14-kinetic-action-types-and-revert-log-design.md` | `docs/superpowers/plans/2026-11-14-kinetic-action-types.md` | `src/rdf/action.rs`, `src/rdf/ontology.rs`, `src/rdf/sparql.rs`, `src/store/ladybug.rs`, `src/formal_kernel.rs` | `neurostrata_action`, `POST /action`, `neurostrata-mcp action` |
| P2 | `neurostrata-brs1` | Polymorphic Digital Twin Interfaces (`implements`), Typed Link Constraints & Declarative 1-Hop Property Reducers | `docs/superpowers/specs/2026-11-14-polymorphic-digital-twins-interfaces-and-reducers-design.md` | Tracked under `neurostrata-brs1` | `src/digital_twin/mod.rs`, `src/rdf/ontology.rs`, `src/rdf/projection.rs`, `src/rdf/shacl.rs`, `src/formal_kernel.rs` | `neurostrata_twin_define`, `neurostrata_twin_get`, `neurostrata_twin_list`, `neurostrata_twin_check` |
| P3 | `neurostrata-ai0v` (blocked by `undx`) | Counterfactual `Ontology Scenarios` — Copy-on-Write What-If Sandbox, Evaluation, Diff & CAS Commit | `docs/superpowers/specs/2026-11-14-counterfactual-ontology-scenarios-design.md` | Tracked under `neurostrata-ai0v` | `src/rdf/scenario.rs`, `src/rdf/sparql.rs`, `src/store/ladybug.rs`, `src/formal_kernel.rs` | `neurostrata_scenario`, `POST /scenario`, `neurostrata-mcp scenario` |
| P4 | `neurostrata-cspr` (blocked by `undx`, `brs1`) | `neurostrata_object_view` — Single-Call 360° Ontology Object View Hub | `docs/superpowers/specs/2026-11-14-unified-ontology-object-view-design.md` | Tracked under `neurostrata-cspr` | `src/graph_read/object_view.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs` | `neurostrata_object_view`, `POST /object-view`, `neurostrata-mcp object-view` |

---

## 2. Library Reuse Mandate (Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`)

All 9 features strictly follow Rule `227984e4-780c-4fe4-a44c-286d7d14eabf` (*Always use suitable license-compatible libraries when they provide required functionality; never prefer zero-new-dependencies or hand-rolled parsers/decoders by default*):

1. **SCIP Protobuf Ingestion (`neurostrata-rbde`):** Adopt the official `scip` crate (`Apache-2.0`) and/or `prost` (`Apache-2.0`) rather than hand-rolling a protobuf wire reader.
2. **Unified Diff Parsing & Topological SCC (`neurostrata-737r`):** Evaluate and use `unidiff` / `patch` (`MIT`/`Apache-2.0`) for unified diff hunk parsing and `petgraph` (`MIT OR Apache-2.0`) for Tarjan SCC condensation and topological sorting.
3. **Code-Aware Lexical Indexing (`neurostrata-5xjo`, `neurostrata-r3as`, `neurostrata-kx2n`):** Build directly on `tantivy::tokenizer` (`MIT`) pipeline primitives.
4. **SPARQL `ASK` Gatekeeping, Scenarios & OWL/SHACL Projection (`neurostrata-undx`, `neurostrata-ai0v`, `neurostrata-brs1`, `neurostrata-cspr`, `neurostrata-kx2n`):** Use `oxrdf`, `spargebra`, `spareval` (`PreparedQuery::substitute_variable`), and `sparesults` (`MIT OR Apache-2.0`), strictly respecting the `oxirs-core` / `scirs2` / `horned-owl` dependency denylist (`f99fd262-3107-4813-a8cf-debb7a9dd577`).

---

## 3. Lean 4 Formal Verification Coverage Across the Suite

Every stateful gate, CAS state machine, bound/reducer algebra, slot reservation invariant, and topological classifier introduced by this release is extracted into the standalone Charon/Aeneas-ready kernel (`src/formal_kernel.rs`), formally proved with zero `sorry` in `proofs/NeuroStrata/*.lean`, and exhaustively verified in `tests/lean_parity_test.rs`:

| Domain Module | Feature(s) | Kernel Functions (`src/formal_kernel.rs`) | Proved Theorems (`proofs/NeuroStrata/*.lean`) |
|---------------|------------|-------------------------------------------|-----------------------------------------------|
| `KineticAction.lean` | `neurostrata-undx`, `neurostrata-cspr` | `evaluate_action_gate`, `evaluate_action_revert`, `evaluate_action_task_effect`, `is_action_memory_shareable` | `action_gate_sound`, `action_revert_no_double_revert`, `action_revert_stale_head_requires_privileged_force`, `action_task_effect_never_reaches_done`, `action_receipt_never_shareable` |
| `ScenarioOverlay.lean` | `neurostrata-ai0v`, `neurostrata-cspr` | `resolve_scenario_row`, `evaluate_scenario_commit` | `scenario_upsert_overrides_retirement_and_live`, `scenario_commit_cas_sound`, `scenario_commit_shacl_gate` |
| `TwinInterface.lean` | `neurostrata-brs1`, `neurostrata-cspr` | `intersect_var_bounds`, `evaluate_reducer_total` | `intersect_bounds_tightens_interface`, `intersect_bounds_commutative`, `reducer_eval_total_on_empty` |
| `HybridSlots.lean` | `neurostrata-5xjo`, `neurostrata-r3as` | `allocate_hybrid_slots` | `hybrid_slots_never_exceed_limit`, `hybrid_slots_never_exceed_available`, `hybrid_slots_guarantees_expansion_when_available` |
| `DiffTour.lean` | `neurostrata-737r` | `classify_tour_stop` | `diff_tour_classification_total_and_disjoint`, `diff_tour_tests_always_stop_4`, `diff_tour_foundations_precede_core_and_surface` |

---

## 4. Dependency-Ordered Execution Waves on `feat/kinetic-actions`

Once `v1.17.3` is tagged and pushed on `main`, rebase `feat/kinetic-actions` onto `main` and execute the 9 features in 3 dependency-ordered waves:

### Wave 1 — Independent Foundations (4 features in parallel / sequence)
1. **P1 (`neurostrata-undx`):** Kinetic `Action Types` & Reversible Action Log (`docs/superpowers/plans/2026-11-14-kinetic-action-types.md`) + `KineticAction.lean`.
2. **P2 (`neurostrata-brs1`):** Polymorphic Digital Twin Interfaces, Typed Link Constraints & Property Reducers + `TwinInterface.lean`.
3. **S1 (`neurostrata-5xjo`):** True Hybrid RRF Search Fusion, Code-Aware Tantivy Sub-Tokenizer & Graph-Expansion Slot Quota + `HybridSlots.lean`.
4. **S2 (`neurostrata-rbde`):** Two-Tier Cross-File Call Graph (Workspace Tree-Sitter Linker + SCIP `index.scip` Ingestion via `scip`/`prost`).

### Wave 2 — Dependent Kinetic/Semantic & Code Intelligence Surfaces (4 features)
5. **P3 (`neurostrata-ai0v`, unblocked by `undx`):** Counterfactual `Ontology Scenarios` Copy-on-Write Sandbox + `ScenarioOverlay.lean`.
6. **P4 (`neurostrata-cspr`, unblocked by `undx` + `brs1`):** Unified 360° `Object View` (`neurostrata_object_view`).
7. **S3 (`neurostrata-r3as`, unblocked by `5xjo` + `rbde`):** Single-Call Code Finder (`neurostrata_locate`) + harness plugin graph-ledger registration.
8. **S4 (`neurostrata-737r`, unblocked by `rbde`):** Guided Diff Tour (`neurostrata_diff_tour`) + `DiffTour.lean`.

### Wave 3 — Longitudinal Bitemporal Telemetry & `v1.18.0` Release Cut (1 feature + release)
9. **S5 (`neurostrata-kx2n`):** Longitudinal Architectural & Governance Code Insights (`neurostrata_insights`).
10. **Release `v1.18.0` Verification & Cut:**
    - Run `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs`, `lake --dir proofs build`, `cargo test --locked --test lean_parity_test`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, and plugin test suites.
    - Complete all 9 feature tasks (`neurostrata-5xjo`, `neurostrata-rbde`, `neurostrata-r3as`, `neurostrata-737r`, `neurostrata-kx2n`, `neurostrata-undx`, `neurostrata-brs1`, `neurostrata-ai0v`, `neurostrata-cspr`) and both umbrella Epics (`neurostrata-mtqx`, `neurostrata-1mus`) with Lock 2 memory extractions.
    - Cut and tag release `v1.18.0`.
