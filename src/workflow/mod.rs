//! Pluggable Workflow/Lifecycle State Machine & Connector Interface.
//!
//! Decouples workspace identity, state receipts, changed-surface detection,
//! and lifecycle trigger installation from hardcoded Git commands.

pub mod connector;
pub mod machine;

pub use connector::{
    resolve_workflow_connector, GitWorkflowConnector, ManifestWorkflowConnector,
    TriggerInstallReport, WorkflowConnector,
};
pub use machine::{
    evaluate_workflow_transition, WorkflowEdgeSpec, WorkflowEvent, WorkflowStage,
    WorkflowTransitionOutcome, WORKFLOW_TABLE,
};
