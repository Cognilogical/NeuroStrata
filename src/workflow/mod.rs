//! Pluggable Workflow/Lifecycle State Machine & Connector Interface.
//!
//! Decouples workspace identity, state receipts, changed-surface detection,
//! and lifecycle trigger installation from hardcoded Git commands.

pub mod connector;
pub mod machine;

pub use connector::{
    GitWorkflowConnector, ManifestWorkflowConnector, TriggerInstallReport,
    WorkflowConnector, resolve_workflow_connector,
};
pub use machine::{
    WorkflowEdgeSpec, WorkflowEvent, WorkflowStage, WorkflowTransitionOutcome, WORKFLOW_TABLE,
    evaluate_workflow_transition,
};
