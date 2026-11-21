# Pluggable Workflow & Lifecycle Connectors

## Formal Workflow State Machine (`WorkflowStage`)

The workflow lifecycle is a finite state machine with 8 stages and 23 legal edges:

| Stage            | Description                                                                   |
| ---------------- | ----------------------------------------------------------------------------- |
| `Unpreflighted`  | Session has not loaded project snapshot/context                               |
| `ContextLoaded`  | Snapshot/memory preflight satisfied; ready to claim a task                    |
| `Claimed`        | Active task claimed in a valid workspace scope; ready to mutate               |
| `Mutating`       | Executing mutations within the Breath budget                                  |
| `BreathRequired` | Mutation budget reached; requires a breath note to return to `Mutating`       |
| `PreCheckpoint`  | Checkpoint trigger fired; validates ledger and receipt binding                |
| `PrePublish`     | Publish trigger fired; evaluates task + documentation gates                   |
| `Completed`      | Task closed via `neurostrata_task_complete` with extraction and clean targets |

`WorkflowEvent` triggers transitions with named guards:

- `claim_task` → `claimed` requires `has_valid_scope`
- `mutate` → `mutating` requires `mutations_since_note_after < breath_limit`
- `mutate` → `breath_required` requires `mutations_since_note_after >= breath_limit`
- `trigger_pre_checkpoint` → `pre_checkpoint` requires `has_receipt`
- `trigger_pre_publish` → `pre_publish` requires `task_gate_ok && doc_gate_ok`
- `complete_task` → `completed` requires `has_extracted_memory && dirty_targets == 0`

The static `WORKFLOW_TABLE` mirrors every legal edge for tooling (`neurostrata-mcp workflow transitions`). The serde-facing layer delegates directly to the const kernel in `crate::formal_kernel` so there is exactly one transition table.

## The `WorkflowConnector` Trait

```rust
pub trait WorkflowConnector: Send + Sync {
    fn connector_id(&self) -> &str;
    fn capture_scope(&self, root: &Path) -> GateScope;
    fn capture_state_digest(&self, root: &Path) -> (String, String);
    fn changed_paths(&self, root: &Path) -> Vec<String>;
    fn install_triggers(&self, root: &Path) -> Result<TriggerInstallReport>;
}
```

A `WorkflowConnector` is a pluggable implementation that knows how to:

1. **Capture scope** — enumerate the workspace files and directories under the task's purview
2. **Capture state digest** — compute a SHA-256 digest of the workspace state for change detection
3. **Detect changed paths** — list files that differ from the last known state
4. **Install triggers** — wire up hooks (pre-commit, pre-push, etc.) for the workspace

The shipped implementations are:

- **`GitWorkflowConnector`** — the default, preserving the pre-existing hardwired behavior
- **`ManifestWorkflowConnector`** — a non-git connector that reads `.NeuroStrata/workflow-connector.json`

## Configuring Non-Git Workspaces (`.NeuroStrata/workflow-connector.json`)

Non-git workspaces declare their connector in `.NeuroStrata/workflow-connector.json` (or via `NEUROSTRATA_WORKFLOW_CONNECTOR_FILE`):

```json
{
  "connector_id": "manifest",
  "stream": "main",
  "workspace": "/workspace/project",
  "revision_id": "rev-001",
  "dirty_state_hash": "clean-001",
  "changed_paths": ["src/workflow/mod.rs"],
  "triggers": {
    "pre-checkpoint": "neurostrata-mcp task validate MyProject",
    "pre-publish": "neurostrata-mcp task gate MyProject --strict && neurostrata-mcp doc-gate"
  }
}
```

`WorkflowConnectorManifest` accepts:

- `connector_id` (or alias `connector`, default `"manifest"`)
- `stream` + `workspace` (or `scope: { "branch": "...", "worktree": "..." }`)
- `revision_id` and `dirty_state_hash` (optional overrides for deterministic state digests)
- `changed_paths` (explicit list of modified paths for surface-diff gates)
- `triggers` as either a stage-to-command map (`{ "pre-checkpoint": "...", "pre-publish": "..." }`) or a list of stage names (`["pre-checkpoint", "pre-publish"]`)

## Writing a Custom Third-Party Workflow Connector

To implement a custom connector, define a struct that implements `WorkflowConnector`. Note that there is **no connector registry**: `resolve_workflow_connector` is a hardcoded two-way switch — it returns a `ManifestWorkflowConnector` when a manifest is configured (via `NEUROSTRATA_WORKFLOW_CONNECTOR_FILE` or `.NeuroStrata/workflow-connector.json`), and `GitWorkflowConnector` otherwise. Wiring a custom connector therefore requires editing `resolve_workflow_connector` in core code; it cannot be registered from outside the crate:

```rust
use crate::workflow::connector::WorkflowConnector;

pub struct MyCustomConnector;

impl WorkflowConnector for MyCustomConnector {
    fn connector_id(&self) -> &str { "my-custom" }
    fn capture_scope(&self, root: &Path) -> GateScope { /* ... */ }
    fn capture_state_digest(&self, root: &Path) -> (String, String) { /* ... */ }
    fn changed_paths(&self, root: &Path) -> Vec<String> { /* ... */ }
    fn install_triggers(&self, root: &Path) -> Result<TriggerInstallReport> { /* ... */ }
}
```

`resolve_workflow_connector` inspects `.NeuroStrata/workflow-connector.json` (or falls back to `git`), constructs the appropriate connector, and returns it for non-trust uses such as `workflow status` display. The connector is **not** used by the workflow state machine: `src/workflow/machine.rs` and `src/workflow/connector.rs` never reference each other, and the machine is evaluated only through `evaluate_workflow_transition` (for example by the `workflow transitions` CLI sample). Trust-bearing consumers — receipt binding (`compute_worktree_state`), retention re-verification, gate scope (`GateScope::capture`), and the documentation gate's changed set — use `GitWorkflowConnector` directly, so a manifest connector cannot attest the state those consumers bind to.

Source of truth: `src/workflow/connector.rs` and `src/workflow/machine.rs`.
