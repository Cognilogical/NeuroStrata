namespace NeuroStrata.WorkflowStage

inductive WorkflowStage where
  | unpreflighted
  | contextLoaded
  | claimed
  | mutating
  | breathRequired
  | preCheckpoint
  | prePublish
  | completed
  deriving DecidableEq, Repr

def WorkflowStage.asStr : WorkflowStage → String
  | .unpreflighted => "unpreflighted"
  | .contextLoaded => "context_loaded"
  | .claimed => "claimed"
  | .mutating => "mutating"
  | .breathRequired => "breath_required"
  | .preCheckpoint => "pre_checkpoint"
  | .prePublish => "pre_publish"
  | .completed => "completed"

inductive WorkflowEvent where
  | loadContext
  | claimTask (hasValidScope : Bool)
  | mutate (mutationsSinceNoteAfter : Nat) (breathLimit : Nat)
  | recordBreathNote
  | triggerPreCheckpoint (hasReceipt : Bool)
  | checkpointPassed
  | triggerPrePublish (taskGateOk : Bool) (docGateOk : Bool)
  | publishPassed
  | completeTask (hasExtractedMemory : Bool) (dirtyTargets : Nat)
  deriving DecidableEq, Repr

inductive WorkflowOutcome where
  | ok (next : WorkflowStage)
  | err (reason : String)
  deriving DecidableEq, Repr

def evaluateWorkflowStageTransition
    (stage : WorkflowStage)
    (event : WorkflowEvent) : WorkflowOutcome :=
  match stage, event with
  | .unpreflighted, .loadContext => .ok .contextLoaded
  | .contextLoaded, .loadContext => .ok .contextLoaded
  | .contextLoaded, .claimTask true => .ok .claimed
  | .claimed, .claimTask true => .ok .claimed
  | .mutating, .claimTask true => .ok .claimed
  | _, .claimTask false => .err "ClaimRequiresValidWorkspaceScope"
  | .unpreflighted, .claimTask true => .err "PreflightRequiredBeforeClaim"
  | .claimed, .mutate m b =>
      if m ≥ b then .ok .breathRequired else .ok .mutating
  | .mutating, .mutate m b =>
      if m ≥ b then .ok .breathRequired else .ok .mutating
  | .breathRequired, .mutate _ _ => .err "BreathNoteRequiredBeforeFurtherMutation"
  | .unpreflighted, .mutate _ _ => .err "ClaimedTaskRequiredBeforeMutation"
  | .contextLoaded, .mutate _ _ => .err "ClaimedTaskRequiredBeforeMutation"
  | .breathRequired, .recordBreathNote => .ok .mutating
  | .mutating, .recordBreathNote => .ok .mutating
  | .claimed, .recordBreathNote => .ok .mutating
  | .claimed, .triggerPreCheckpoint true => .ok .preCheckpoint
  | .mutating, .triggerPreCheckpoint true => .ok .preCheckpoint
  | _, .triggerPreCheckpoint false => .err "CheckpointRequiresValidReceipt"
  | .preCheckpoint, .checkpointPassed => .ok .mutating
  | .claimed, .triggerPrePublish true true => .ok .prePublish
  | .mutating, .triggerPrePublish true true => .ok .prePublish
  | .preCheckpoint, .triggerPrePublish true true => .ok .prePublish
  | _, .triggerPrePublish false _ => .err "PublishBlockedByTaskGate"
  | _, .triggerPrePublish true false => .err "PublishBlockedByDocumentationGate"
  | .prePublish, .publishPassed => .ok .mutating
  | .claimed, .completeTask true 0 => .ok .completed
  | .mutating, .completeTask true 0 => .ok .completed
  | .preCheckpoint, .completeTask true 0 => .ok .completed
  | .prePublish, .completeTask true 0 => .ok .completed
  | _, .completeTask false _ => .err "CompletionRequiresExtractedMemory"
  | _, .completeTask true (_ + 1) => .err "CompletionBlockedByDirtyTargets"
  | _, _ => .err "IllegalWorkflowStageTransition"

def workflowTransitionTag (stage : WorkflowStage) (event : WorkflowEvent) : String :=
  match evaluateWorkflowStageTransition stage event with
  | .ok next => s!"ok:{next.asStr}"
  | .err reason => s!"err:{reason}"

theorem claim_requires_valid_scope (s : WorkflowStage) :
    evaluateWorkflowStageTransition s (.claimTask false) = .err "ClaimRequiresValidWorkspaceScope" := by
  cases s <;> rfl

theorem breath_required_blocks_mutation (m b : Nat) :
    evaluateWorkflowStageTransition .breathRequired (.mutate m b) =
      .err "BreathNoteRequiredBeforeFurtherMutation" := by
  rfl

theorem publish_requires_both_gates (s : WorkflowStage) (tOk dOk : Bool) :
    evaluateWorkflowStageTransition s (.triggerPrePublish tOk dOk) = .ok .prePublish →
    tOk = true ∧ dOk = true := by
  cases s <;> cases tOk <;> cases dOk <;> decide

theorem completion_requires_memory_and_clean_targets (s : WorkflowStage) (hasMem : Bool) (dirty : Nat) :
    evaluateWorkflowStageTransition s (.completeTask hasMem dirty) = .ok .completed →
    hasMem = true ∧ dirty = 0 := by
  cases s <;> cases hasMem <;> cases dirty <;> intro h <;> cases h <;> decide

end NeuroStrata.WorkflowStage
