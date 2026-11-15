# 3-Tier Documentation Manifest, Surface-Diff Gate & Pluggable Workflow Connectors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize `docs/` into three strict tiers (`vision/`, `features-api/`, `extend/` + `archive/`), extract the workflow/lifecycle state machine and `WorkflowConnector` trait (`src/workflow/`) so NeuroStrata is not hard-bound to Git hooks, and enforce a manifest-driven surface-diff Documentation Gate (`docs/manifest.json` + `src/doc_gate.rs`).

**Architecture:** First, extract `src/workflow/` (`WorkflowStage`, `WorkflowEvent`, `WorkflowConnector`, `GitWorkflowConnector`, `ManifestWorkflowConnector`) and route `GateScope::capture`, `Receipt::capture`, and lifecycle hook installation/execution through the active connector with a pure decision function in `src/formal_kernel.rs`. Second, build `src/doc_gate.rs` and `docs/manifest.json` to validate the 3-tier layout, competitor-filename prohibition, required API/trait tokens, and surface-diff coverage. Third, restructure `docs/` into `docs/vision/`, `docs/features-api/`, `docs/extend/`, and `docs/archive/`, author the Tier 3 extension docs (`docs/extend/workflow-lifecycle-connectors.md`, `harness-plugins.md`, `providers-and-ontologies.md`, `overview.md`) and missing v1.18.0 feature docs (`kinetic-actions.md`, `taxonomy-advisor.md`, `digital-twins.md`), and wire `doc-gate` into CLI and `task gate --strict`.

**Tech Stack:** Rust (`serde`, `serde_json`, `clap`), NeuroStrata Formal Kernel (`src/formal_kernel.rs`), JSON Schema (`docs/manifest.json`).

**Spec:** `docs/archive/specs/2026-11-15-documentation-gate-and-workflow-connectors-design.md`

## Global Constraints

- Competitor Naming Prohibition (`9e64a5a8-aba4-4034-abfe-8b478bd9950e`): zero competitor product/company names in filenames, symbols, tasks, or documentation (`*cognee*` and `*oxirs*` files in `docs/` must be renamed to NeuroStrata-native domain names).
- Data Formats (`1ecfd804-a60c-4538-95a4-634ff379ee47`): `docs/manifest.json` and `.NeuroStrata/workflow-connector.json` use strict JSON, never YAML/TOML.
- Backward Compatibility: `GateScope::capture(&root)` and `compute_worktree_state(&root)` must remain 100% compatible for existing Git repositories when no `.NeuroStrata/workflow-connector.json` override is present.
- Zero Doc Churn on Internal Edits: internal bugfixes and refactors that do not leave public MCP tools, CLI commands, or extension traits undocumented must pass `doc-gate` without requiring markdown edits.
- Formatting Rule (`63f895ed-43a5-4b50-ad59-c4e1f9a6d1ed`): format only hunks/files authored in this branch; never run whole-repo `cargo fmt`.

## Review Focus

1. **Non-Git Workspace with `ManifestWorkflowConnector`:** When `.NeuroStrata/workflow-connector.json` defines a custom connector in a directory with no `.git`, `resolve_workflow_connector(root)` must return the manifest connector and produce a valid `GateScope` (`branch` = stream, `worktree` = workspace root) so `task gate --strict` does not fail closed as uncapturable.
2. **Loose Root Files in `docs/`:** Adding an unclassified `.md` file directly under `docs/foo.md` (outside `README.md` and `manifest.json`) must be rejected by `doc_gate::evaluate_doc_gate`.
3. **Competitor Filename Guard:** Any file under `docs/` whose name contains a prohibited competitor token (`cognee`, `oxirs`, `mem0`, `zep`, `langmem`) must fail `doc_gate::evaluate_doc_gate`.
4. **Undocumented Public Surface Addition:** Adding a new MCP tool or `WorkflowConnector` method without documenting its token in the mapped `target_doc` (`docs/features-api/mcp-and-http-api.md` or `docs/extend/workflow-lifecycle-connectors.md`) must fail `doc_gate::evaluate_doc_gate` with the exact target doc path.
5. **Internal Bugfix Pass-Through:** Modifying an internal file (or even a watched file like `src/server.rs`) while all required public tokens and headings remain present in the mapped `target_doc` (and without `require_co_change_on_touch: true`) must pass `doc_gate::evaluate_doc_gate` with 0 violations.

---

### Task 1: Pluggable Workflow/Lifecycle State Machine & Connector Interface (`src/workflow/`)

**Files:**
- Create: `src/workflow/mod.rs`
- Create: `src/workflow/machine.rs`
- Create: `src/workflow/connector.rs`
- Modify: `src/formal_kernel.rs`
- Modify: `src/main.rs`
- Modify: `src/task/gate.rs`
- Modify: `src/task/receipt.rs`

**Interfaces:**
- Consumes: `crate::task::gate::GateScope`
- Produces:
  - `crate::workflow::machine::{WorkflowStage, WorkflowEvent, WorkflowTransitionOutcome, evaluate_workflow_transition, WORKFLOW_TABLE}`
  - `crate::workflow::connector::{WorkflowConnector, TriggerInstallReport, GitWorkflowConnector, ManifestWorkflowConnector, resolve_workflow_connector}`
  - `crate::formal_kernel::{KernelWorkflowStage, KernelWorkflowEvent, evaluate_workflow_stage_transition}`

- [ ] **Step 1: Write failing unit tests in `src/workflow/machine.rs` and `src/workflow/connector.rs`**

Test `workflow_fsm_transitions_and_guards` covering `Unpreflighted -> ContextLoaded -> Claimed -> Mutating -> BreathRequired -> Mutating -> PreCheckpoint -> PrePublish -> Completed` and illegal transition rejections; test `manifest_connector_captures_scope_and_digest_without_git` verifying `.NeuroStrata/workflow-connector.json` supplies `GateScope`, state digest, changed paths, and trigger installation in a non-Git tempdir.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin neurostrata-mcp workflow:: -- --test-threads=1`
Expected: FAIL (module `workflow` not found)

- [ ] **Step 3: Implement `src/workflow/{mod.rs,machine.rs,connector.rs}`, wire `formal_kernel.rs`, and delegate `GateScope::capture` + `compute_worktree_state` to `resolve_workflow_connector`**

`resolve_workflow_connector(root: &Path)` checks for `<root>/.NeuroStrata/workflow-connector.json` (or walking up to `.NeuroStrata/workflow-connector.json`) and returns `ManifestWorkflowConnector` when present, otherwise `GitWorkflowConnector`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin neurostrata-mcp workflow:: task::gate::tests:: task::receipt::tests:: -- --test-threads=1`
Expected: PASS

---

### Task 2: Documentation Manifest Engine & Surface-Diff Gate (`src/doc_gate.rs`)

**Files:**
- Create: `src/doc_gate.rs`
- Modify: `src/task/wiring.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `crate::workflow::connector::resolve_workflow_connector`
- Produces:
  - `crate::doc_gate::{DocManifest, DocGateReport, DocViolation, evaluate_doc_gate, load_manifest}`
  - CLI subcommands: `neurostrata-mcp doc-gate [root]` and `neurostrata-mcp workflow <status|transitions|fire|install>`

- [ ] **Step 1: Write failing tests in `src/doc_gate.rs`**

Test:
1. `rejects_unclassified_root_markdown_file`
2. `rejects_prohibited_competitor_filename`
3. `rejects_missing_required_heading_or_token_in_mapped_doc`
4. `internal_change_passes_without_doc_churn`
5. `surface_change_missing_co_change_or_token_blocks`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin neurostrata-mcp doc_gate::tests:: -- --test-threads=1`
Expected: FAIL

- [ ] **Step 3: Implement `src/doc_gate.rs`, register wire `"doc-manifest-gate"` in `src/task/wiring.rs`, and add CLI commands `doc-gate` and `workflow` in `src/main.rs`**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin neurostrata-mcp doc_gate::tests:: task::wiring:: -- --test-threads=1`
Expected: PASS

---

### Task 3: Restructure `docs/` into 3 Canonical Tiers + `docs/archive/` and Author `docs/manifest.json`

**Files:**
- Create: `docs/manifest.json`
- Create: `docs/vision/product-vision.md`
- Move/Update into `docs/vision/`: `COGNITIVE_ARCHITECTURE.md` -> `docs/vision/cognitive-architecture.md`, `science/why-memory-fails.md` -> `docs/vision/why-memory-fails.md`, `science/nomenclature.md` -> `docs/vision/nomenclature.md`, `architecture/tri-strata.md` -> `docs/vision/tri-strata.md`
- Move/Update into `docs/features-api/`:
  - `docs/features-api/mcp-and-http-api.md` (comprehensive MCP + HTTP API reference covering all 30 MCP tools)
  - `ops/cli-reference.md` -> `docs/features-api/cli-reference.md` (updated with `action`, `taxonomy`, `workflow`, `doc-gate`)
  - `FEATURES.md` -> `docs/features-api/feature-catalog.md`
  - `UI_GUIDE.md` -> `docs/features-api/ui-guide.md`
  - `recall.md` -> `docs/features-api/episodic-recall.md`
  - `limitations.md` -> `docs/features-api/limitations.md`
  - `organs/*`, `architecture/synapticgraph.md`, `mesh/synaptic-mesh.md`, `architecture/domains/*`, `architecture/prescribed-wire-recipes.md`, `architecture/self_reinforced_testing_framework.md` -> `docs/features-api/subsystems/`
  - Create `docs/features-api/subsystems/kinetic-actions.md`, `docs/features-api/subsystems/taxonomy-advisor.md`, `docs/features-api/subsystems/digital-twins.md`
  - `BUILDING.md`, `ops/*`, `runbooks/wal-recovery.md` -> `docs/features-api/ops/`
- Create Tier 3 (`docs/extend/`):
  - `docs/extend/overview.md`
  - `docs/extend/workflow-lifecycle-connectors.md`
  - `docs/extend/harness-plugins.md`
  - `docs/extend/providers-and-ontologies.md`
- Move historical design memos/verdicts/specs/plans into `docs/archive/{verdicts,specs,plans}/` and rename competitor-named files (`*cognee*` -> `*entity-graph*`, `*oxirs*` -> `*rdf-sparql*`).
- Update `docs/README.md` and `README.md` links to match the 3-tier layout.

- [ ] **Step 1: Move files into the 3 tiers + `docs/archive/` and scrub competitor filenames**
- [ ] **Step 2: Author `docs/vision/product-vision.md`, `docs/features-api/mcp-and-http-api.md`, `docs/features-api/subsystems/{kinetic-actions,taxonomy-advisor,digital-twins}.md`, and `docs/extend/{overview,workflow-lifecycle-connectors,harness-plugins,providers-and-ontologies}.md`**
- [ ] **Step 3: Author `docs/manifest.json` and update `docs/README.md` + `README.md` + `AGENTS.md`**
- [ ] **Step 4: Run `evaluate_doc_gate` against the live repository root in an integration test (`doc_gate_passes_on_live_repository`)**

Run: `cargo test --bin neurostrata-mcp doc_gate::tests::doc_gate_passes_on_live_repository -- --test-threads=1`
Expected: PASS with 0 violations.
