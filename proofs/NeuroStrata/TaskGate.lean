import NeuroStrata.TaskMachine

namespace NeuroStrata.TaskGate

open NeuroStrata.TaskMachine

structure Row where
  id : Nat
  isTask : Bool
  status : Status
  validTo : Option Int
  supersededBy : Option Nat
  supersedes : List Nat
  extractedFrom : List Nat
  grandfathered : Bool
  deriving DecidableEq, Repr

def isRetired (r : Row) (now : Int) : Bool :=
  match r.validTo with
  | some vt => decide (vt ≤ now)
  | none => false

def findRow (id : Nat) (rows : List Row) : Option Row :=
  rows.find? (fun r => r.id == id)

def liveSuccessor : Nat → List Nat → Option Nat → List Row → Int → Option Row
  | 0, _, _, _, _ => none
  | fuel + 1, seen, currentId, rows, now =>
      match currentId with
      | none => none
      | some id =>
          if id ∈ seen then none
          else
            match findRow id rows with
            | none => none
            | some r =>
                if isRetired r now then
                  match r.supersededBy with
                  | some parent => liveSuccessor fuel (id :: seen) (some parent) rows now
                  | none => none
                else
                  some r

def workLineageLoop : Nat → List Nat → List Nat → List Nat → List Row → List Nat
  | 0, _, _, acc, _ => acc
  | _ + 1, [], _, acc, _ => acc
  | fuel + 1, id :: rest, seen, acc, rows =>
      if id ∈ seen then workLineageLoop fuel rest seen acc rows
      else
        match findRow id rows with
        | none => workLineageLoop fuel rest (id :: seen) (id :: acc) rows
        | some r =>
            workLineageLoop fuel (rest ++ r.supersedes) (id :: seen) (id :: acc) rows

def workLineage (rows : List Row) (taskId : Nat) : List Nat :=
  taskId :: workLineageLoop 32 [taskId] [] [] rows

def extractionExists (rows : List Row) (taskId : Nat) (now : Int) : Bool :=
  rows.any (fun r => r.id != taskId && !isRetired r now && r.extractedFrom.any (fun lid => decide (lid ∈ workLineage rows taskId)))

def obligationCarried (r : Row) (rows : List Row) (now : Int) : Bool :=
  match liveSuccessor 32 [] r.supersededBy rows now with
  | none => false
  | some succ =>
      succ.isTask && decide (succ.status = .done) && extractionExists rows succ.id now

def hasDoneWithoutExtractionViolation (r : Row) (rows : List Row) (now : Int) : Bool :=
  if r.status = .done then
    if (isRetired r now && obligationCarried r rows now) || r.grandfathered || extractionExists rows r.id now
    then false
    else true
  else false

theorem isRetired_boundary_inclusive : ∀ (r : Row) (now : Int),
    r.validTo = some now → isRetired r now = true := by
  intro r now hvt
  simp [isRetired, hvt]

theorem isRetired_none_stays_live : ∀ (r : Row) (now : Int),
    r.validTo = none → isRetired r now = false := by
  intro r now hvt
  simp [isRetired, hvt]

theorem liveSuccessor_never_retired : ∀ (fuel : Nat) (seen : List Nat) (cid : Option Nat) (rows : List Row) (now : Int) (succ : Row),
    liveSuccessor fuel seen cid rows now = some succ → isRetired succ now = false := by
  intro fuel
  induction fuel with
  | zero =>
    intro seen cid rows now succ h
    simp [liveSuccessor] at h
  | succ n ih =>
    intro seen cid rows now succ h
    cases cid with
    | none => simp [liveSuccessor] at h
    | some id =>
      simp only [liveSuccessor] at h
      split at h
      · contradiction
      · split at h
        · contradiction
        · rename_i r hfind
          split at h
          · cases hsup : r.supersededBy with
            | none => simp [hsup] at h
            | some parent =>
              simp [hsup] at h
              exact ih (id :: seen) (some parent) rows now succ h
          · rename_i hnotret
            injection h with heq
            subst heq
            exact Bool.eq_false_iff.mpr hnotret

theorem liveSuccessor_cycle_safe : ∀ (fuel : Nat) (seen : List Nat) (id : Nat) (rows : List Row) (now : Int),
    id ∈ seen → liveSuccessor (fuel + 1) seen (some id) rows now = none := by
  intro fuel seen id rows now hin
  simp [liveSuccessor, hin]

theorem workLineage_contains_root : ∀ (rows : List Row) (taskId : Nat),
    taskId ∈ workLineage rows taskId := by
  intro rows taskId
  simp [workLineage]

theorem lock2_gate_conservation : ∀ (r : Row) (rows : List Row) (now : Int),
    r.isTask = true → r.status = .done → r.grandfathered = false →
    hasDoneWithoutExtractionViolation r rows now = false →
    extractionExists rows r.id now = true ∨ (isRetired r now = true ∧
      ∃ succ, liveSuccessor 32 [] r.supersededBy rows now = some succ ∧
      succ.isTask = true ∧ succ.status = .done ∧ extractionExists rows succ.id now = true) := by
  intro r rows now _ hstatus hgrand hviol
  cases hex : extractionExists rows r.id now
  · right
    cases hret : isRetired r now
    · simp [hasDoneWithoutExtractionViolation, hstatus, hgrand, hex, hret] at hviol
    · refine ⟨rfl, ?_⟩
      have hob : obligationCarried r rows now = true := by
        cases h1 : obligationCarried r rows now
        · simp [hasDoneWithoutExtractionViolation, hstatus, hgrand, hex, hret, h1] at hviol
        · rfl
      cases hls : liveSuccessor 32 [] r.supersededBy rows now with
      | none => simp [obligationCarried, hls] at hob
      | some succ =>
          simp [obligationCarried, hls] at hob
          rcases hob with ⟨⟨ht, hs⟩, he⟩
          exact ⟨succ, rfl, ht, hs, he⟩
  · left
    rfl

theorem liveSuccessor_two_node_cycle_returns_none (now : Int) :
    let rA : Row := ⟨0, true, .done, some now, some 1, [1], [], false⟩
    let rB : Row := ⟨1, true, .done, some now, some 0, [0], [], false⟩
    liveSuccessor 32 [] (some 0) [rA, rB] now = none := by
  intro rA rB
  simp [liveSuccessor, findRow, isRetired, rA, rB]

inductive ClaimScopeVerdict where
  | blockUnscoped
  | blockUncapturableScope
  | blockLegacyUnattributed
  | blockBranchMatch
  | blockWorktreeMatch
  | allowIsolatedScope
  deriving DecidableEq, Repr

def ClaimScopeVerdict.blocks : ClaimScopeVerdict → Bool
  | .allowIsolatedScope => false
  | _ => true

def ClaimScopeVerdict.asTag : ClaimScopeVerdict → String
  | .blockUnscoped => "block_unscoped"
  | .blockUncapturableScope => "block_uncapturable_scope"
  | .blockLegacyUnattributed => "block_legacy_unattributed"
  | .blockBranchMatch => "block_branch_match"
  | .blockWorktreeMatch => "block_worktree_match"
  | .allowIsolatedScope => "allow_isolated_scope"

@[simp]
def evaluateClaimScopeGate
    (isScoped callerId claimAttr branchMatch wtMatch : Bool) : ClaimScopeVerdict :=
  if !isScoped then .blockUnscoped
  else if !callerId then .blockUncapturableScope
  else if !claimAttr then .blockLegacyUnattributed
  else if branchMatch then .blockBranchMatch
  else if wtMatch then .blockWorktreeMatch
  else .allowIsolatedScope

theorem unscoped_always_blocks :
    ∀ (callerId claimAttr branchMatch wtMatch : Bool),
      (evaluateClaimScopeGate false callerId claimAttr branchMatch wtMatch).blocks = true := by
  intro callerId claimAttr branchMatch wtMatch
  rfl

theorem uncapturable_caller_fails_closed :
    ∀ (claimAttr branchMatch wtMatch : Bool),
      (evaluateClaimScopeGate true false claimAttr branchMatch wtMatch).blocks = true := by
  intro claimAttr branchMatch wtMatch
  rfl

theorem legacy_unattributed_claim_fails_closed :
    ∀ (branchMatch wtMatch : Bool),
      (evaluateClaimScopeGate true true false branchMatch wtMatch).blocks = true := by
  intro branchMatch wtMatch
  rfl

theorem matching_branch_always_blocks :
    ∀ (wtMatch : Bool),
      (evaluateClaimScopeGate true true true true wtMatch).blocks = true := by
  intro wtMatch
  rfl

theorem matching_worktree_always_blocks :
    (evaluateClaimScopeGate true true true false true).blocks = true := by
  rfl

theorem claim_scope_isolation_soundness :
    ∀ (isScoped callerId claimAttr branchMatch wtMatch : Bool),
      (evaluateClaimScopeGate isScoped callerId claimAttr branchMatch wtMatch).blocks = false ↔
        (isScoped = true ∧ callerId = true ∧ claimAttr = true ∧
          branchMatch = false ∧ wtMatch = false) := by
  intro isScoped callerId claimAttr branchMatch wtMatch
  cases isScoped <;> cases callerId <;> cases claimAttr <;> cases branchMatch <;>
    cases wtMatch <;> decide

end NeuroStrata.TaskGate
