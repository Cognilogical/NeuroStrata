import NeuroStrata.TaskMachine

namespace NeuroStrata.TwinCrdt

open NeuroStrata.TaskMachine

@[simp]
def statusRank : Status → Nat
  | .done => 3
  | .inProgress => 2
  | .blocked => 1
  | .openState => 0

@[simp]
def joinStatus (a b : Status) : Status :=
  if statusRank a ≥ statusRank b then a else b

inductive SeqDecision where
  | duplicateIgnore
  | applyAndAdvance
  | bufferOutOfOrder
  deriving DecidableEq, Repr

def SeqDecision.asTag : SeqDecision → String
  | .duplicateIgnore => "duplicate_ignore"
  | .applyAndAdvance => "apply_and_advance"
  | .bufferOutOfOrder => "buffer_out_of_order"

@[simp]
def evaluateSeqContinuity (alreadySeen : Bool) (expectedSeq frameSeq : Nat) : SeqDecision :=
  if alreadySeen || decide (frameSeq < expectedSeq) then .duplicateIgnore
  else if decide (expectedSeq < frameSeq) then .bufferOutOfOrder
  else .applyAndAdvance

theorem joinStatus_idempotent :
    ∀ (s : Status), joinStatus s s = s := by
  intro s
  cases s <;> rfl

theorem joinStatus_commutative :
    ∀ (a b : Status), joinStatus a b = joinStatus b a := by
  intro a b
  cases a <;> cases b <;> rfl

theorem joinStatus_associative :
    ∀ (a b c : Status), joinStatus (joinStatus a b) c = joinStatus a (joinStatus b c) := by
  intro a b c
  cases a <;> cases b <;> cases c <;> rfl

theorem done_is_top_of_status_lattice :
    ∀ (s : Status), joinStatus .done s = .done ∧ joinStatus s .done = .done := by
  intro s
  cases s <;> decide

theorem duplicate_or_old_seq_never_advances :
    ∀ (exp seq : Nat), evaluateSeqContinuity true exp seq = .duplicateIgnore := by
  intro exp seq
  rfl

end NeuroStrata.TwinCrdt
