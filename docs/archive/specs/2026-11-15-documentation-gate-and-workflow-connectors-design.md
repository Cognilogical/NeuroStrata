# 3-Tier Documentation Manifest, Surface-Diff Documentation Gate & Pluggable Workflow/Lifecycle Connector Architecture

**Date:** 2026-11-15  
**Tasks:** `neurostrata-jpiy`, `neurostrata-31vf`  
**Status:** Approved

---

## 1. Problem Statement & Goals

1. **Documentation Sprawl & Disorder:** `docs/` currently mixes live product documentation, internal subsystem specs, and 45+ historical review verdicts and scratch plans at the top level and across `docs/designs/`, `docs/architecture/`, and `docs/superpowers/`. Several historical files also carry competitor product names in their filenames in violation of Rule `9e64a5a8-aba4-4034-abfe-8b478bd9950e`.
2. **Missing Documentation Guard:** Public feature surfaces (`neurostrata_action`, `neurostrata_taxonomy`, `neurostrata_twin`, SCIP/call-graph tools, CLI commands, and extension interfaces) can change without updating canonical documentation, while naive doc gates fail by demanding verbose documentation churn on every internal bugfix or refactor.
3. **Hardcoded Git Coupling in Workflow Lifecycle:** Although `src/task/wiring.rs` established the principle *"git-first, never git-only"*, workspace scope capture (`GateScope::capture` in `src/task/gate.rs`), working-state receipts (`compute_worktree_state` in `src/task/receipt.rs`), and lifecycle trigger installation (`install_hooks` in `src/task/mod.rs`) directly invoke `git` commands and `.git/hooks/*`. External systems (alternative VCS, CI/CD pipelines, CMS publishing workflows, or third-party orchestrators) cannot plug into the NeuroStrata workflow lifecycle via a clean state-machine and connector interface.

---

## 2. Three-Tier Documentation Architecture

All active documentation in `docs/` is partitioned into three strict, non-overlapping tiers, indexed by `docs/README.md` and governed by `docs/manifest.json`:

### Tier 1: Product Vision (`docs/vision/`)
Communicates *what* NeuroStrata is, *why* it exists as a general-purpose project memory, ontology, digital-twin, kinetic-action, and domain-expertise engine (not limited to software repositories), and its scientific/architectural foundations:
- `docs/vision/product-vision.md` — Core product thesis, general-purpose project memory & ontology model, evaluation axes, and non-goals.
- `docs/vision/cognitive-architecture.md` — The 15 neuroscience-grounded mechanisms and biological-to-systems nomenclature (`why-memory-fails.md`, `nomenclature.md`, `tri-strata.md`).

### Tier 2: Internal Features & Major API (`docs/features-api/`)
Documents built-in organs, engines, operational runbooks, and canonical API/CLI surfaces:
- `docs/features-api/mcp-and-http-api.md` — Canonical reference for all MCP tools (`src/server.rs`) and HTTP daemon endpoints (`src/daemon.rs`).
- `docs/features-api/cli-reference.md` — Canonical `neurostrata-mcp` CLI command reference (`src/main.rs`).
- `docs/features-api/feature-catalog.md` — Complete feature inventory and module mapping (`FEATURES.md`, `UI_GUIDE.md`, `recall.md`, `limitations.md`).
- `docs/features-api/subsystems/` — Internal organ and engine reference (`central-executive.md`, `synapticgraph.md`, `kinetic-actions.md`, `taxonomy-advisor.md`, `digital-twins.md`, `synaptic-mesh.md`, `prefrontal-cortex.md`, `dendritic-bridge.md`, `thalamic-bus.md`, `procedural-memory.md`, `local-learning.md`, `judgment-providers.md`, `core-installer.md`, `prescribed-wire-recipes.md`, `self-reinforced-testing.md`).
- `docs/features-api/ops/` — Build, security, upgrading, crash recovery, merge gates, and WAL recovery runbooks (`BUILDING.md`, `security.md`, `upgrading.md`, `crashes.md`, `merge-gates.md`, `obsidian.md`, `wal-recovery.md`).

### Tier 3: Extending NeuroStrata — Connectors & Interfaces (`docs/extend/`)
Strictly separated from internal feature documentation. Written for third-party integrators extending NeuroStrata or connecting it to non-Git and external environments:
- `docs/extend/overview.md` — Extension architecture, stability guarantees, and boundary contracts.
- `docs/extend/workflow-lifecycle-connectors.md` — Complete specification and integration guide for the `WorkflowLifecycleMachine` state machine and `WorkflowConnector` interface (connecting NeuroStrata to custom VCS, CMS pipelines, issue trackers, or webhook orchestrators without Git hooks).
- `docs/extend/harness-plugins.md` — Building CLI/IDE agent harness adapters over `@neurostrata/plugin-core`.
- `docs/extend/providers-and-ontologies.md` — Implementing custom `Embedder`, `JudgmentProvider`, SKOS domain taxonomies, and Kinetic Action packs.

### Non-Tier Support & Archive Directories (`docs/releases/`, `docs/assets/`, `docs/archive/`)
- `docs/releases/changelog/` — Per-release changelogs.
- `docs/assets/` — Static diagrams and media.
- `docs/archive/` — Historical architecture decision records, panel verdicts, specs, and implementation plans (`docs/archive/verdicts/`, `docs/archive/specs/`, `docs/archive/plans/`). All competitor-named filenames (`*cognee*`, `*oxirs*`) are renamed to NeuroStrata-native domain terminology (`entity-extraction-pipeline`, `w3c-rdf-sparql-engine`).

---

## 3. Pluggable Workflow/Lifecycle State Machine & Connector Interface (`src/workflow/`)

### 3.1 Formal Workflow Lifecycle State Machine (`WorkflowStage` & `WorkflowEvent`)
We factor the end-to-end lifecycle into a deterministic state machine (`src/workflow/machine.rs` + pure decision mirror in `src/formal_kernel.rs`):

> **Correction (PR #206 remediation, V-7):** this state machine is a **new
> formalisation**, not an extraction of an existing FSM — no workflow state
> machine existed in the codebase before this work. It **supersedes** the
> 2026-10-26 state-machine spec
> (`docs/archive/specs/2026-10-26-state-machine-agent-workflow-design.md`):
> that spec's `Verifying` stage is dropped, and `PreCheckpoint` /
> `PrePublish` are added. The only verbatim extractions in this work are
> the two git functions named in §3.3.

- **States (`WorkflowStage`):**
  1. `Unpreflighted` — Session has not loaded project snapshot/context.
  2. `ContextLoaded` — Snapshot/memory preflight satisfied; ready to claim a task.
  3. `Claimed` — Active task claimed in a valid `WorkflowScope`; ready to mutate.
  4. `Mutating` — Executing mutations within the Breath budget (`mutations_since_note < 5`).
  5. `BreathRequired` — Mutation budget reached (`>= 5`); requires `task_update(note)` to return to `Mutating`.
  6. `PreCheckpoint` — Checkpoint trigger fired (e.g., Git `pre-commit`, CMS draft save, pipeline stage checkpoint); validates graph-query ledger and receipt binding.
  7. `PrePublish` — Publish trigger fired (e.g., Git `pre-push`, release publish, deployment gate); evaluates strict task gate + surface-diff documentation gate.
  8. `Completed` — Task closed via `neurostrata_task_complete` with `>= 1` extracted memory and normalized targets.

- **Events (`WorkflowEvent`):**
  - `LoadContext`, `ClaimTask`, `Mutate { count_after: usize }`, `RecordBreathNote`, `TriggerPreCheckpoint`, `CheckpointPassed`, `TriggerPrePublish`, `PublishPassed`, `CompleteTask { has_extracted_memory: bool, dirty_targets: usize }`.

### 3.2 `WorkflowConnector` Trait (`src/workflow/connector.rs`)
Decouples workspace identity, state receipts, changed-surface detection, and lifecycle trigger installation from hardcoded Git commands:

```rust
pub trait WorkflowConnector: Send + Sync {
    /// Stable connector identifier (e.g. "git", "manifest", "custom-command").
    fn connector_id(&self) -> &str;

    /// Capture the active workspace/stream scope (`GateScope`) for claim isolation.
    fn capture_scope(&self, root: &Path) -> GateScope;

    /// Capture the current state digest `(revision_id, dirty_state_hash)` for receipts.
    fn capture_state_digest(&self, root: &Path) -> (String, String);

    /// Return workspace-relative paths modified in the current working change set
    /// (used by surface-diff gates such as the Documentation Gate).
    fn changed_paths(&self, root: &Path) -> Vec<String>;

    /// Install or verify lifecycle triggers (e.g. `.git/hooks/pre-push` for Git,
    /// or `.NeuroStrata/workflow-triggers.json` for external connectors).
    fn install_triggers(&self, root: &Path) -> anyhow::Result<TriggerInstallReport>;
}
```

### 3.3 Built-in Connectors & Configuration
1. **`GitWorkflowConnector` (`connector_id = "git"`):** Default when `.git` is present and no override connector is configured. Backward compatibility is claimed **only** for the two extracted git functions — `GateScope::capture` and `compute_worktree_state` — and **only** on the no-manifest Git path (no `.NeuroStrata/workflow-connector.json` override present); the `WorkflowConnector` trait routing itself is new, and no broader compatibility claim is made.
2. **`ManifestWorkflowConnector` (`connector_id = "manifest"`):** Reads `.NeuroStrata/workflow-connector.json` (or `NEUROSTRATA_WORKFLOW_CONNECTOR` env) allowing non-Git workspaces or third-party plugins to supply static or command-driven `stream`/`workspace` scope, state digest, changed files, and webhook/CLI trigger bindings.
3. **CLI & MCP Surface:** `neurostrata-mcp workflow <status|transitions|fire|install>` lets external systems inspect the active connector, query the state machine table, and fire lifecycle stage checks (`pre-checkpoint`, `pre-publish`) directly without Git hooks.

---

## 4. Documentation Manifest (`docs/manifest.json`) & Surface-Diff Gate (`src/doc_gate.rs`)

### 4.1 `docs/manifest.json` Schema
- `schema_version`: `1`
- `allowed_root_files`: `["README.md", "manifest.json"]`
- `tiers`:
  - `vision`: `{ "dir": "docs/vision", "description": "...", "required_files": [...] }`
  - `features_api`: `{ "dir": "docs/features-api", "description": "...", "required_files": [...] }`
  - `extend`: `{ "dir": "docs/extend", "description": "...", "required_files": [...] }`
- `support_dirs`: `["docs/releases", "docs/assets", "docs/archive"]`
- `prohibited_filename_substrings`: `["cognee", "oxirs", "mem0", "zep", "langmem"]` (enforces Rule `9e64a5a8` across `docs/`)
- `surface_rules`: Array of mappings:
  - `id`: e.g. `"mcp-tools-surface"`, `"cli-commands-surface"`, `"workflow-connector-extension"`, `"harness-plugin-extension"`, `"core-provider-traits"`, `"kinetic-actions-surface"`, `"taxonomy-surface"`
  - `tier`: `"vision" | "features_api" | "extend"`
  - `watch_paths`: glob/prefix paths in `src/` or `plugins/`
  - `trigger_patterns`: optional regex/literal markers indicating public surface additions/changes (e.g. `"inputSchema"` in `src/server.rs`, `pub trait ` in `src/workflow/` or `src/traits.rs`)
  - `target_doc`: path under `docs/`
  - `required_headings`: headings that must exist in `target_doc`
  - `required_tokens`: canonical identifiers (e.g. MCP tool names, CLI subcommands, trait names) that must remain documented in `target_doc`

### 4.2 Gate Evaluation (`src/doc_gate.rs`)
1. **Tier & Hygiene Invariant (Always Checked):**
   - No loose files in `docs/` outside `allowed_root_files`, the 3 tier directories, and `support_dirs`.
   - No filenames in `docs/` matching `prohibited_filename_substrings`.
   - All `required_files`, `target_doc` paths, `required_headings`, and `required_tokens` exist and are non-empty.
   - Tier 3 (`docs/extend/`) files never reside inside `docs/features-api/` and vice versa.
2. **Surface-Diff Invariant (Checked when `changed_paths` is non-empty):**
   - For each `surface_rule`, if any path in `changed_paths` matches `watch_paths` (and, when `data/diff` or `trigger_patterns` apply, touches public surface tokens) while `target_doc` is NOT in `changed_paths` AND a public token is undocumented or explicitly flagged, emit a `doc-gate` violation naming the exact `target_doc` and tier.
   - Internal edits (bugfixes, internal store logic, tests) that keep all `required_tokens` documented pass with **zero doc churn**.
3. **Wiring & CLI Integration:**
   - Added as wire `"doc-manifest-gate"` in `src/task/wiring.rs` (`AUTOMATIC_WIRES`, `RunsIn::ProjectPipeline` / `task gate`), exposed via `neurostrata-mcp doc-gate [project_root]`, and invoked during `task gate --strict`.
