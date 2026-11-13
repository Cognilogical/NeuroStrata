import NeuroStrata.TaskMachine

namespace NeuroStrata.TaskClaim

open NeuroStrata.TaskMachine

inductive ClaimDecision where
  | idempotentSameSession
  | refuseLiveHolder
  | takeoverStaleHolder
  | claimFromOpen
  | delegateToFsm
  deriving DecidableEq, Repr

def ClaimDecision.asTag : ClaimDecision → String
  | .idempotentSameSession => "idempotent_same_session"
  | .refuseLiveHolder => "refuse_live_holder"
  | .takeoverStaleHolder => "takeover_stale_holder"
  | .claimFromOpen => "claim_from_open"
  | .delegateToFsm => "delegate_to_fsm"

@[simp]
def evaluateClaim (status : Status) (sameSession : Bool) (ageSecs : Option Int) (staleAfterSecs : Int) : ClaimDecision :=
  match status with
  | .inProgress =>
      if sameSession then .idempotentSameSession
      else
        match ageSecs with
        | some age => if decide (age ≤ staleAfterSecs) then .refuseLiveHolder else .takeoverStaleHolder
        | none => .takeoverStaleHolder
  | .openState => .claimFromOpen
  | .blocked => .delegateToFsm
  | .done => .delegateToFsm

theorem same_session_in_progress_idempotent :
    ∀ (age : Option Int) (stale : Int),
      evaluateClaim .inProgress true age stale = .idempotentSameSession := by
  intro age stale
  rfl

theorem live_holder_exclusivity :
    ∀ (age stale : Int),
      age ≤ stale →
      evaluateClaim .inProgress false (some age) stale = .refuseLiveHolder := by
  intro age stale h
  simp [evaluateClaim, h]

theorem stale_holder_recoverable :
    ∀ (age stale : Int),
      stale < age →
      evaluateClaim .inProgress false (some age) stale = .takeoverStaleHolder := by
  intro age stale h
  have hnot : ¬ (age ≤ stale) := by omega
  simp [evaluateClaim, hnot]

theorem missing_timestamp_recoverable :
    ∀ (stale : Int),
      evaluateClaim .inProgress false none stale = .takeoverStaleHolder := by
  intro stale
  rfl

end NeuroStrata.TaskClaim