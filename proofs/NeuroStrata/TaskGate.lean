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

def liveSuccessor (fuel : Nat) (seen : List Nat) (currentId : Option Nat) (rows : List Row) (now : Int) : Option Row :=
  match currentId with
  | none => none
  | some id =>
      if id ∈ seen then none
      else if fuel = 0 then none
      else
        match findRow id rows with
        | none => none
        | some r =>
            if isRetired r now then
              match r.supersededBy with
              | some parent => liveSuccessor (fuel - 1) (id :: seen) (some parent) rows now
              | none => none
            else
              some r

def findRow (id : Nat) (rows : List Row) : Option Row :=
  rows.foldl (fun acc id' => if acc.isSome then acc else if id' .id = id then some id' else acc) none

def workLineage (fuel : Nat) (queue : List Nat) (seen : List Nat) (acc : List Nat) (rows : List Row) : List Nat :=
  match queue with
  | [] => acc
  | id :: rest =>
      if fuel = 0 then acc
      else if id ∈ seen then workLineage fuel rest seen acc rows
      else
        match findRow id rows with
        | none => workLineage (fuel - 1) rest (id :: seen) acc rows
        | some r =>
            workLineage (fuel - 1) (rest ++ r.supersedes) (id :: seen) (id :: acc) rows

def extractionExists (rows : List Row) (taskId : Nat) (now : Int) : Bool :=
  (List.filter (fun r => r.id ≠ taskId ∧
    List.any (r.extractedFrom) (fun lid => lid ∈ workLineage 32 [taskId] [] [] rows)))
    rows.
    foldl (fun acc r => acc ∨ isRetired r now = false) false

def obligationCarried (r : Row) (rows : List Row) (now : Int) : Bool :=
  match liveSuccessor 32 [] r.supersededBy rows now with
  | none => false
  | some succ =>
      succ.isTask = true ∧ succ.status = .done ∧ extractionExists rows succ.id now

def hasDoneWithoutExtractionViolation (r : Row) (rows : List Row) (now : Int) : Bool :=
  if r.status = .done then
    if (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now
    then false
    else true
  else false

theorem isRetired_boundary_inclusive : ∀ (r : Row) (now : Int),
    r.validTo = some now → isRetired r now = true := by
  intro r now hvt
  simp [isRetired, hvt]
  <;> decide

theorem isRetired_none_stays_live : ∀ (r : Row) (now : Int),
    r.validTo = none → isRetired r now = false := by
  intro r now hvt
  simp [isRetired, hvt]

theorem liveSuccessor_never_retired : ∀ (fuel seen cid rows now succ),
    liveSuccessor fuel seen cid rows now = some succ → isRetired succ now = false := by
  intro fuel seen cid rows now succ h
  induction fuel with
  | zero =>
      intro seen cid rows now succ h
      cases cid <;> simp [liveSuccessor] at h
  | succ n ih =>
      intro seen cid rows now succ h
      cases cid with
      | none => simp [liveSuccessor] at h
      | some id =>
          simp only [liveSuccessor] at h
          split at h
          <;> contradiction

theorem liveSuccessor_cycle_safe : ∀ (fuel seen id rows now),
    id ∈ seen → liveSuccessor (fuel + 1) seen (some id) rows now = none := by
  intro fuel seen id rows now hin
  simp [liveSuccessor, hin]

theorem workLineage_contains_root : ∀ (fuel rows id),
    fuel > 0 → id ∈ workLineage fuel [id] [] [] rows := by
  intro fuel rows id hfuel
  have h : id ∈ workLineage fuel [id] [] [] rows := by
    unfold workLineage at *
    induction fuel generalizing rows id with
    | zero => contradiction
    | succ fuel' ih =>
        unfold workLineage
        split_ifs
        <;> simp_all [List.Mem.cons, List.Mem.nil, workLineage]
        <;> (try contradiction) <;> (try { aesop })
  exact h

theorem lock2_gate_conservation : ∀ (r rows now),
    r.isTask = true → r.status = .done → r.grandfathered = false →
    hasDoneWithoutExtractionViolation r rows now = false →
    extractionExists rows r.id now ∨ (isRetired r now = true ∧
      ∃ succ, liveSuccessor 32 [] r.supersededBy rows now = some succ ∧
      succ.isTask = true ∧ succ.status = .done ∧ extractionExists rows succ.id now) := by
  intro r rows now htask hstatus hgrand hviol
  have h : r.status = .done := hstatus
  have hviol' : hasDoneWithoutExtractionViolation r rows now = false := hviol
  have hcond : (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now := by
    have h1 : hasDoneWithoutExtractionViolation r rows now = if r.status = .done then (if (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now then false else true) else false := by
      simp [hasDoneWithoutExtractionViolation, h2]
    have h2 : r.status = .done := hstatus
    have h3 : (hasDoneWithoutExtractionViolation r rows now) = (if (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now then false else true) := by
      simp [hasDoneWithoutExtractionViolation, h2]
    have h4 : (if (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now then false else true) = false := by
      rw [h3] at hviol'
      exact hviol'
    by_contra h6
    have h7 : (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now := by
      intro h8
      cases h8 with
      | inl h8 => exact h8
      | inr h8 => exact h8
    contradiction
  have hleft : (isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now := hcond
  split_ifs at hleft
  <;> simp_all
  <;> (try { aesop })

end NeuroStrata.TaskGate