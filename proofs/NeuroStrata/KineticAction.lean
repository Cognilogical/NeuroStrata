import NeuroStrata.TaskMachine

namespace NeuroStrata.KineticAction

open NeuroStrata.TaskMachine

inductive ActionGateDecision where
  | allow
  | rejectUnauthorized
  | rejectInvalidParams
  | rejectSparqlAsk
  | rejectTwinBounds
  | rejectPostShacl
  deriving DecidableEq, Repr

def ActionGateDecision.asTag : ActionGateDecision → String
  | .allow => "allow"
  | .rejectUnauthorized => "reject_unauthorized"
  | .rejectInvalidParams => "reject_invalid_params"
  | .rejectSparqlAsk => "reject_sparql_ask"
  | .rejectTwinBounds => "reject_twin_bounds"
  | .rejectPostShacl => "reject_post_shacl"

@[simp]
def evaluateActionGate
    (principalAuthorized paramsValid sparqlAskPassed twinBoundsPassed postShaclPassed : Bool)
    : ActionGateDecision :=
  if !principalAuthorized then .rejectUnauthorized
  else if !paramsValid then .rejectInvalidParams
  else if !sparqlAskPassed then .rejectSparqlAsk
  else if !twinBoundsPassed then .rejectTwinBounds
  else if !postShaclPassed then .rejectPostShacl
  else .allow

theorem action_gate_sound :
    ∀ (p par sp tw sh : Bool),
      evaluateActionGate p par sp tw sh = .allow ↔
        (p = true ∧ par = true ∧ sp = true ∧ tw = true ∧ sh = true) := by
  intro p par sp tw sh
  cases p <;> cases par <;> cases sp <;> cases tw <;> cases sh <;> decide

inductive ActionReceiptState where
  | active
  | alreadyReverted
  deriving DecidableEq, Repr

def ActionReceiptState.asStr : ActionReceiptState → String
  | .active => "active"
  | .alreadyReverted => "already_reverted"

inductive ActionRevertDecision where
  | revertCommit
  | forcedRevertCommit
  | rejectAlreadyReverted
  | rejectPermissionDenied
  | rejectStaleHeadConflict
  deriving DecidableEq, Repr

def ActionRevertDecision.asTag : ActionRevertDecision → String
  | .revertCommit => "revert_commit"
  | .forcedRevertCommit => "forced_revert_commit"
  | .rejectAlreadyReverted => "reject_already_reverted"
  | .rejectPermissionDenied => "reject_permission_denied"
  | .rejectStaleHeadConflict => "reject_stale_head_conflict"

@[simp]
def evaluateActionRevert
    (receiptState : ActionReceiptState)
    (headCasMatches forceRequested callerIsOwnerOrAdmin callerIsEditorOrAbove : Bool)
    : ActionRevertDecision :=
  match receiptState with
  | .alreadyReverted => .rejectAlreadyReverted
  | .active =>
      if forceRequested then
        if callerIsOwnerOrAdmin then .forcedRevertCommit
        else .rejectPermissionDenied
      else if !(callerIsEditorOrAbove || callerIsOwnerOrAdmin) then .rejectPermissionDenied
      else if !headCasMatches then .rejectStaleHeadConflict
      else .revertCommit

theorem action_revert_no_double_revert :
    ∀ (cas force admin editor : Bool),
      evaluateActionRevert .alreadyReverted cas force admin editor = .rejectAlreadyReverted := by
  intro cas force admin editor
  rfl

theorem action_revert_stale_head_requires_privileged_force :
    ∀ (r : ActionReceiptState) (force admin editor : Bool),
      (evaluateActionRevert r false force admin editor = .revertCommit ∨
       evaluateActionRevert r false force admin editor = .forcedRevertCommit) ↔
        (r = .active ∧ force = true ∧ admin = true) := by
  intro r force admin editor
  cases r <;> cases force <;> cases admin <;> cases editor <;> decide

inductive ActionTaskTargetStatus where
  | openState
  | inProgress
  | blocked
  | done
  deriving DecidableEq, Repr

def ActionTaskTargetStatus.asStr : ActionTaskTargetStatus → String
  | .openState => "open"
  | .inProgress => "in_progress"
  | .blocked => "blocked"
  | .done => "done"

inductive ActionTaskEffectError where
  | doneRequiresComplete
  | illegalTransition
  deriving DecidableEq, Repr

inductive ActionTaskEffectOutcome where
  | ok (s : Status)
  | err (e : ActionTaskEffectError)
  deriving DecidableEq, Repr

@[simp]
def evaluateActionTaskEffect
    (current : Status)
    (requested : ActionTaskTargetStatus)
    : ActionTaskEffectOutcome :=
  match requested with
  | .done => .err .doneRequiresComplete
  | .openState =>
      if current = .openState || (edgeGuard current .openState).isSome then .ok .openState
      else .err .illegalTransition
  | .inProgress =>
      if current = .inProgress || (edgeGuard current .inProgress).isSome then .ok .inProgress
      else .err .illegalTransition
  | .blocked =>
      if current = .blocked || (edgeGuard current .blocked).isSome then .ok .blocked
      else .err .illegalTransition

def actionTaskEffectTag (res : ActionTaskEffectOutcome) : String :=
  match res with
  | .ok .openState => "ok_open"
  | .ok .inProgress => "ok_in_progress"
  | .ok .blocked => "ok_blocked"
  | .ok .done => "ok_done"
  | .err .doneRequiresComplete => "err_done_requires_complete"
  | .err .illegalTransition => "err_illegal"

theorem action_task_effect_never_reaches_done :
    ∀ (s : Status) (req : ActionTaskTargetStatus),
      evaluateActionTaskEffect s req ≠ .ok .done := by
  intro s req
  cases s <;> cases req <;> decide

inductive ActionMemoryKind where
  | actionType
  | actionReceipt
  deriving DecidableEq, Repr

def ActionMemoryKind.asStr : ActionMemoryKind → String
  | .actionType => "action_type"
  | .actionReceipt => "action_receipt"

@[simp]
def isActionMemoryShareable (kind : ActionMemoryKind) (explicitShareable : Bool) : Bool :=
  match kind with
  | .actionType => explicitShareable
  | .actionReceipt => false

theorem action_receipt_never_shareable :
    ∀ (b : Bool),
      isActionMemoryShareable .actionReceipt b = false := by
  intro b
  rfl

end NeuroStrata.KineticAction
