//! The pluggable workflow lifecycle state machine.
//!
//! This is the serde-facing layer over the pure decision kernel in
//! `crate::formal_kernel`: [`WorkflowStage`] and [`WorkflowEvent`] mirror
//! [`crate::formal_kernel::KernelWorkflowStage`] and
//! [`crate::formal_kernel::KernelWorkflowEvent`], and
//! [`evaluate_workflow_transition`] delegates directly to the const kernel so
//! there is exactly one transition table. The static [`WORKFLOW_TABLE`] mirrors
//! every legal edge for tooling (`neurostrata-mcp workflow transitions`).

use serde::{Deserialize, Serialize};

use crate::formal_kernel::{
    evaluate_workflow_stage_transition, KernelWorkflowEvent, KernelWorkflowStage,
};

/// A lifecycle stage in the pluggable workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStage {
    /// Session has not loaded project snapshot/context.
    Unpreflighted,
    /// Snapshot/memory preflight satisfied; ready to claim a task.
    ContextLoaded,
    /// Active task claimed in a valid workspace scope; ready to mutate.
    Claimed,
    /// Executing mutations within the Breath budget.
    Mutating,
    /// Mutation budget reached; requires a breath note to return to `Mutating`.
    BreathRequired,
    /// Checkpoint trigger fired; validates ledger and receipt binding.
    PreCheckpoint,
    /// Publish trigger fired; evaluates task + documentation gates.
    PrePublish,
    /// Task closed via `neurostrata_task_complete` with extraction and clean targets.
    Completed,
}

/// A lifecycle event in the pluggable workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    LoadContext,
    ClaimTask {
        has_valid_scope: bool,
    },
    Mutate {
        mutations_since_note_after: usize,
        breath_limit: usize,
    },
    RecordBreathNote,
    TriggerPreCheckpoint {
        has_receipt: bool,
    },
    CheckpointPassed,
    TriggerPrePublish {
        task_gate_ok: bool,
        doc_gate_ok: bool,
    },
    PublishPassed,
    CompleteTask {
        has_extracted_memory: bool,
        dirty_targets: usize,
    },
}

/// The result of evaluating one workflow transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTransitionOutcome {
    pub allowed: bool,
    pub from: WorkflowStage,
    pub to: Option<WorkflowStage>,
    pub reason: Option<String>,
}

/// One static edge in the workflow transition table: `from --event--> to`,
/// with a human-readable guard describing the condition that must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEdgeSpec {
    pub from: &'static str,
    pub event: &'static str,
    pub to: &'static str,
    pub guard: &'static str,
}

const BREATH_GUARD_LOW: &str = "mutations_since_note_after < breath_limit";
const BREATH_GUARD_HIGH: &str = "mutations_since_note_after >= breath_limit";
const CLAIM_GUARD: &str = "has_valid_scope";
const CHECKPOINT_GUARD: &str = "has_receipt";
const PUBLISH_GUARD: &str = "task_gate_ok && doc_gate_ok";
const COMPLETE_GUARD: &str = "has_extracted_memory && dirty_targets == 0";

/// The static workflow transition table. Every row is a legal edge; guards
/// name the runtime condition that selects the edge.
pub const WORKFLOW_TABLE: &[WorkflowEdgeSpec] = &[
    WorkflowEdgeSpec { from: "unpreflighted", event: "load_context", to: "context_loaded", guard: "" },
    WorkflowEdgeSpec { from: "context_loaded", event: "load_context", to: "context_loaded", guard: "" },
    WorkflowEdgeSpec { from: "context_loaded", event: "claim_task", to: "claimed", guard: CLAIM_GUARD },
    WorkflowEdgeSpec { from: "claimed", event: "claim_task", to: "claimed", guard: CLAIM_GUARD },
    WorkflowEdgeSpec { from: "mutating", event: "claim_task", to: "claimed", guard: CLAIM_GUARD },
    WorkflowEdgeSpec { from: "claimed", event: "mutate", to: "mutating", guard: BREATH_GUARD_LOW },
    WorkflowEdgeSpec { from: "claimed", event: "mutate", to: "breath_required", guard: BREATH_GUARD_HIGH },
    WorkflowEdgeSpec { from: "mutating", event: "mutate", to: "mutating", guard: BREATH_GUARD_LOW },
    WorkflowEdgeSpec { from: "mutating", event: "mutate", to: "breath_required", guard: BREATH_GUARD_HIGH },
    WorkflowEdgeSpec { from: "breath_required", event: "record_breath_note", to: "mutating", guard: "" },
    WorkflowEdgeSpec { from: "mutating", event: "record_breath_note", to: "mutating", guard: "" },
    WorkflowEdgeSpec { from: "claimed", event: "record_breath_note", to: "mutating", guard: "" },
    WorkflowEdgeSpec { from: "claimed", event: "trigger_pre_checkpoint", to: "pre_checkpoint", guard: CHECKPOINT_GUARD },
    WorkflowEdgeSpec { from: "mutating", event: "trigger_pre_checkpoint", to: "pre_checkpoint", guard: CHECKPOINT_GUARD },
    WorkflowEdgeSpec { from: "pre_checkpoint", event: "checkpoint_passed", to: "mutating", guard: "" },
    WorkflowEdgeSpec { from: "claimed", event: "trigger_pre_publish", to: "pre_publish", guard: PUBLISH_GUARD },
    WorkflowEdgeSpec { from: "mutating", event: "trigger_pre_publish", to: "pre_publish", guard: PUBLISH_GUARD },
    WorkflowEdgeSpec { from: "pre_checkpoint", event: "trigger_pre_publish", to: "pre_publish", guard: PUBLISH_GUARD },
    WorkflowEdgeSpec { from: "pre_publish", event: "publish_passed", to: "mutating", guard: "" },
    WorkflowEdgeSpec { from: "claimed", event: "complete_task", to: "completed", guard: COMPLETE_GUARD },
    WorkflowEdgeSpec { from: "mutating", event: "complete_task", to: "completed", guard: COMPLETE_GUARD },
    WorkflowEdgeSpec { from: "pre_checkpoint", event: "complete_task", to: "completed", guard: COMPLETE_GUARD },
    WorkflowEdgeSpec { from: "pre_publish", event: "complete_task", to: "completed", guard: COMPLETE_GUARD },
];

fn to_kernel_stage(stage: WorkflowStage) -> KernelWorkflowStage {
    match stage {
        WorkflowStage::Unpreflighted => KernelWorkflowStage::Unpreflighted,
        WorkflowStage::ContextLoaded => KernelWorkflowStage::ContextLoaded,
        WorkflowStage::Claimed => KernelWorkflowStage::Claimed,
        WorkflowStage::Mutating => KernelWorkflowStage::Mutating,
        WorkflowStage::BreathRequired => KernelWorkflowStage::BreathRequired,
        WorkflowStage::PreCheckpoint => KernelWorkflowStage::PreCheckpoint,
        WorkflowStage::PrePublish => KernelWorkflowStage::PrePublish,
        WorkflowStage::Completed => KernelWorkflowStage::Completed,
    }
}

fn from_kernel_stage(stage: KernelWorkflowStage) -> WorkflowStage {
    match stage {
        KernelWorkflowStage::Unpreflighted => WorkflowStage::Unpreflighted,
        KernelWorkflowStage::ContextLoaded => WorkflowStage::ContextLoaded,
        KernelWorkflowStage::Claimed => WorkflowStage::Claimed,
        KernelWorkflowStage::Mutating => WorkflowStage::Mutating,
        KernelWorkflowStage::BreathRequired => WorkflowStage::BreathRequired,
        KernelWorkflowStage::PreCheckpoint => WorkflowStage::PreCheckpoint,
        KernelWorkflowStage::PrePublish => WorkflowStage::PrePublish,
        KernelWorkflowStage::Completed => WorkflowStage::Completed,
    }
}

fn to_kernel_event(event: WorkflowEvent) -> KernelWorkflowEvent {
    match event {
        WorkflowEvent::LoadContext => KernelWorkflowEvent::LoadContext,
        WorkflowEvent::ClaimTask { has_valid_scope } => {
            KernelWorkflowEvent::ClaimTask { has_valid_scope }
        }
        WorkflowEvent::Mutate { mutations_since_note_after, breath_limit } => {
            KernelWorkflowEvent::Mutate { mutations_since_note_after, breath_limit }
        }
        WorkflowEvent::RecordBreathNote => KernelWorkflowEvent::RecordBreathNote,
        WorkflowEvent::TriggerPreCheckpoint { has_receipt } => {
            KernelWorkflowEvent::TriggerPreCheckpoint { has_receipt }
        }
        WorkflowEvent::CheckpointPassed => KernelWorkflowEvent::CheckpointPassed,
        WorkflowEvent::TriggerPrePublish { task_gate_ok, doc_gate_ok } => {
            KernelWorkflowEvent::TriggerPrePublish { task_gate_ok, doc_gate_ok }
        }
        WorkflowEvent::PublishPassed => KernelWorkflowEvent::PublishPassed,
        WorkflowEvent::CompleteTask { has_extracted_memory, dirty_targets } => {
            KernelWorkflowEvent::CompleteTask { has_extracted_memory, dirty_targets }
        }
    }
}

/// Evaluate one workflow transition against the pure kernel decision table.
pub fn evaluate_workflow_transition(
    stage: WorkflowStage,
    event: WorkflowEvent,
) -> WorkflowTransitionOutcome {
    match evaluate_workflow_stage_transition(to_kernel_stage(stage), to_kernel_event(event)) {
        Ok(next) => WorkflowTransitionOutcome {
            allowed: true,
            from: stage,
            to: Some(from_kernel_stage(next)),
            reason: None,
        },
        Err(reason) => WorkflowTransitionOutcome {
            allowed: false,
            from: stage,
            to: None,
            reason: Some(reason.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(s: WorkflowStage) -> WorkflowStage {
        s
    }

    #[test]
    fn workflow_fsm_transitions_and_guards() {
        // Legal path: Unpreflighted -> ContextLoaded -> Claimed -> Mutating ->
        // BreathRequired -> Mutating -> PreCheckpoint -> PrePublish -> Completed.
        let mut current = WorkflowStage::Unpreflighted;

        let out = evaluate_workflow_transition(current, WorkflowEvent::LoadContext);
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::ContextLoaded)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::ClaimTask { has_valid_scope: true });
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::Claimed)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::Mutate { mutations_since_note_after: 2, breath_limit: 5 });
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::Mutating)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::Mutate { mutations_since_note_after: 5, breath_limit: 5 });
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::BreathRequired)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::RecordBreathNote);
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::Mutating)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::TriggerPreCheckpoint { has_receipt: true });
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::PreCheckpoint)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::CheckpointPassed);
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::Mutating)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::TriggerPrePublish { task_gate_ok: true, doc_gate_ok: true });
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::PrePublish)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::PublishPassed);
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::Mutating)));
        current = out.to.unwrap();

        let out = evaluate_workflow_transition(current, WorkflowEvent::CompleteTask { has_extracted_memory: true, dirty_targets: 0 });
        assert!(out.allowed);
        assert_eq!(out.to, Some(stage(WorkflowStage::Completed)));
        assert_eq!(out.reason, None);
    }

    #[test]
    fn workflow_fsm_guard_rejections() {
        let out = evaluate_workflow_transition(WorkflowStage::BreathRequired, WorkflowEvent::Mutate { mutations_since_note_after: 0, breath_limit: 5 });
        assert!(!out.allowed);
        assert_eq!(out.to, None);
        assert_eq!(out.reason.as_deref(), Some("BreathNoteRequiredBeforeFurtherMutation"));

        let out = evaluate_workflow_transition(WorkflowStage::Claimed, WorkflowEvent::TriggerPrePublish { task_gate_ok: true, doc_gate_ok: false });
        assert!(!out.allowed);
        assert_eq!(out.reason.as_deref(), Some("PublishBlockedByDocumentationGate"));

        let out = evaluate_workflow_transition(WorkflowStage::Claimed, WorkflowEvent::CompleteTask { has_extracted_memory: false, dirty_targets: 0 });
        assert!(!out.allowed);
        assert_eq!(out.reason.as_deref(), Some("CompletionRequiresExtractedMemory"));

        let out = evaluate_workflow_transition(WorkflowStage::Claimed, WorkflowEvent::CompleteTask { has_extracted_memory: true, dirty_targets: 2 });
        assert!(!out.allowed);
        assert_eq!(out.reason.as_deref(), Some("CompletionBlockedByDirtyTargets"));
    }

    #[test]
    fn workflow_table_matches_kernel_edges() {
        // Every table row must be a real kernel edge when its guard is
        // satisfied: evaluate each row through the kernel and check the
        // exact target stage. (Previously this asserted only that the
        // row strings were non-empty — any corruption kept it green.)
        fn parse_stage(s: &str) -> WorkflowStage {
            match s {
                "unpreflighted" => WorkflowStage::Unpreflighted,
                "context_loaded" => WorkflowStage::ContextLoaded,
                "claimed" => WorkflowStage::Claimed,
                "mutating" => WorkflowStage::Mutating,
                "breath_required" => WorkflowStage::BreathRequired,
                "pre_checkpoint" => WorkflowStage::PreCheckpoint,
                "pre_publish" => WorkflowStage::PrePublish,
                "completed" => WorkflowStage::Completed,
                other => panic!("unknown stage in WORKFLOW_TABLE: {other}"),
            }
        }
        fn stage_str(s: WorkflowStage) -> &'static str {
            match s {
                WorkflowStage::Unpreflighted => "unpreflighted",
                WorkflowStage::ContextLoaded => "context_loaded",
                WorkflowStage::Claimed => "claimed",
                WorkflowStage::Mutating => "mutating",
                WorkflowStage::BreathRequired => "breath_required",
                WorkflowStage::PreCheckpoint => "pre_checkpoint",
                WorkflowStage::PrePublish => "pre_publish",
                WorkflowStage::Completed => "completed",
            }
        }
        // The representative event that satisfies the row's guard.
        fn representative_event(edge: &WorkflowEdgeSpec) -> WorkflowEvent {
            match edge.event {
                "load_context" => WorkflowEvent::LoadContext,
                "claim_task" => WorkflowEvent::ClaimTask { has_valid_scope: true },
                "mutate" => {
                    if edge.to == "breath_required" {
                        WorkflowEvent::Mutate { mutations_since_note_after: 5, breath_limit: 5 }
                    } else {
                        WorkflowEvent::Mutate { mutations_since_note_after: 1, breath_limit: 5 }
                    }
                }
                "record_breath_note" => WorkflowEvent::RecordBreathNote,
                "trigger_pre_checkpoint" => {
                    WorkflowEvent::TriggerPreCheckpoint { has_receipt: true }
                }
                "checkpoint_passed" => WorkflowEvent::CheckpointPassed,
                "trigger_pre_publish" => {
                    WorkflowEvent::TriggerPrePublish { task_gate_ok: true, doc_gate_ok: true }
                }
                "publish_passed" => WorkflowEvent::PublishPassed,
                "complete_task" => {
                    WorkflowEvent::CompleteTask { has_extracted_memory: true, dirty_targets: 0 }
                }
                other => panic!("unknown event in WORKFLOW_TABLE: {other}"),
            }
        }
        assert_eq!(WORKFLOW_TABLE.len(), 23, "WORKFLOW_TABLE must hold 23 edges");
        for edge in WORKFLOW_TABLE {
            let out = evaluate_workflow_transition(
                parse_stage(edge.from),
                representative_event(edge),
            );
            assert!(
                out.allowed,
                "table row {} --{}--> {} rejected by kernel: {:?}",
                edge.from, edge.event, edge.to, out.reason
            );
            assert_eq!(
                out.to.map(stage_str),
                Some(edge.to),
                "table row {} --{}--> {} lands elsewhere in the kernel",
                edge.from, edge.event, edge.to
            );
        }
    }
}
