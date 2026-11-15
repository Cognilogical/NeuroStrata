import NeuroStrata.TaskMachine

namespace NeuroStrata.Taxonomy

open NeuroStrata.TaskMachine

inductive EpistemicStatus where
  | seed
  | defined
  | synthesized
  | verified
  deriving DecidableEq, Repr

namespace EpistemicStatus

def weightBps : EpistemicStatus → Nat
  | .seed => 2500
  | .defined => 5000
  | .synthesized => 8000
  | .verified => 10000

def asStr : EpistemicStatus → String
  | .seed => "seed"
  | .defined => "defined"
  | .synthesized => "synthesized"
  | .verified => "verified"

def rank : EpistemicStatus → Nat
  | .seed => 0
  | .defined => 1
  | .synthesized => 2
  | .verified => 3

end EpistemicStatus

def classifyEpistemicStatus (hasDefinition : Bool) (hasTaxonomyLink : Bool) (hasSynthesis : Bool) (hasEmpiricalGrounding : Bool) : EpistemicStatus :=
  if hasDefinition then
    if hasTaxonomyLink then
      if hasSynthesis then
        if hasEmpiricalGrounding then .verified
        else .synthesized
      else .defined
    else .seed
  else .seed

def statusWeightSum (statuses : List EpistemicStatus) : Nat :=
  List.foldl (fun (acc : Nat) (s : EpistemicStatus) => acc + EpistemicStatus.weightBps s) (0 : Nat) statuses

def computeEpistemicCoverageBps (statuses : List EpistemicStatus) (unmappedDomainCount : Nat) : Nat :=
  let denom : Nat := statuses.length + unmappedDomainCount
  if denom == 0 then 0
  else
    let sumWeights : Nat := statusWeightSum statuses
    (sumWeights / denom).min 10000

def realizationStatusGapBps (status : String) : Nat :=
  if status == "unimplemented" then 10000
  else if status == "partial" then 7500
  else if status == "unknown" then 6000
  else if status == "implemented" then 2500
  else if status == "external_only" then 0
  else 6000

def frontierLeverageCompute (axisAlignmentBps : Nat) (realizationGapBps : Nat) (canonicalConnectivityCount : Nat) (hasFrontierOrLimitation : Bool) (hasReferencesOrComparables : Bool) : Nat :=
  Nat.min
    ((35 * Nat.min axisAlignmentBps 10000
      + 25 * Nat.min realizationGapBps 10000
      + 20 * ((Nat.min canonicalConnectivityCount 10) * 1000)
      + 10 * (if hasFrontierOrLimitation then 10000 else 0)
      + 10 * (if hasReferencesOrComparables then 10000 else 0)) / 100)
    10000

def computeFrontierLeverageBps (axisAlignmentBps : Nat) (realizationGapBps : Nat) (canonicalConnectivityCount : Nat) (hasFrontierOrLimitation : Bool) (hasReferencesOrComparables : Bool) : Nat :=
  if realizationGapBps == 0 then 0
  else if realizationGapBps ≤ 2500 then
    if hasFrontierOrLimitation then
      frontierLeverageCompute axisAlignmentBps realizationGapBps canonicalConnectivityCount hasFrontierOrLimitation hasReferencesOrComparables
    else 0
  else
    frontierLeverageCompute axisAlignmentBps realizationGapBps canonicalConnectivityCount hasFrontierOrLimitation hasReferencesOrComparables

def hasSelfLoop (broaderEdges : List (Nat × Nat)) : Bool :=
  broaderEdges.any (fun e => e.1 == e.2)

def reachableBroader (broaderEdges : List (Nat × Nat)) (start : Nat) : List Nat :=
  let fuels : List Nat := List.range 65
  let init : List Nat := [start]
  List.foldl (fun (acc : List Nat) (fuel : Nat) =>
    if fuel == 0 then acc
    else
      let next := (broaderEdges.filter (fun e => e.1 ∈ acc)).map (fun e => e.2)
      List.foldl (fun (acc2 : List Nat) (e : Nat) => acc2 ++ [e]) acc next
  ) init fuels

def hasTwoCycle (broaderEdges : List (Nat × Nat)) : Bool :=
  broaderEdges.any (fun e => e.1 ∈ reachableBroader broaderEdges e.2)

def skosPairDisjoint (broaderEdges : List (Nat × Nat)) (u v : Nat) : Bool :=
  if u == v then false
  else if v ∈ reachableBroader broaderEdges u then false
  else if u ∈ reachableBroader broaderEdges v then false
  else true

def validateTaxonomyDag (broaderEdges : List (Nat × Nat)) : Bool :=
  if hasSelfLoop broaderEdges then false
  else if hasTwoCycle broaderEdges then false
  else true

def validateSkosDisjointness (broaderEdges : List (Nat × Nat)) (relatedEdges : List (Nat × Nat)) (contrastEdges : List (Nat × Nat)) : Bool :=
  if relatedEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) then false
  else if contrastEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) then false
  else true

theorem classify_epistemic_monotonic (d : Bool) (t : Bool) (s : Bool) (g : Bool) :
    EpistemicStatus.rank (classifyEpistemicStatus d t s g) ≤
      EpistemicStatus.rank (classifyEpistemicStatus true true true true) := by
  unfold classifyEpistemicStatus
  cases d <;> cases t <;> cases s <;> cases g <;> decide

theorem epistemic_status_weight_bounded (s : EpistemicStatus) : EpistemicStatus.weightBps s ≤ 10000 := by
  cases s <;> decide

theorem verified_requires_all_pillars (d : Bool) (t : Bool) (s : Bool) (g : Bool) :
    classifyEpistemicStatus d t s g = .verified →
    d ∧ t ∧ s ∧ g := by
  intro h
  unfold classifyEpistemicStatus at h
  cases d <;> cases t <;> cases s <;> cases g <;> simp_all

theorem dag_rejects_self_loop_and_two_cycle (edges : List (Nat × Nat)) :
    hasSelfLoop edges = true ∨ hasTwoCycle edges = true → validateTaxonomyDag edges = false := by
  intro h
  unfold validateTaxonomyDag at *
  by_cases hSelfLoop : hasSelfLoop edges = true
  · simp [hSelfLoop]
  · by_cases hTwoLoop : hasTwoCycle edges = true
    · simp [hTwoLoop]
    · exfalso
      exact Or.elim h (fun hSelf => hSelfLoop hSelf) (fun hTwo => hTwoLoop hTwo)

theorem skos_disjointness_sound (broaderEdges : List (Nat × Nat)) (relatedEdges : List (Nat × Nat)) (contrastEdges : List (Nat × Nat)) :
    (relatedEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) = true ∨
      contrastEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) = true) →
    validateSkosDisjointness broaderEdges relatedEdges contrastEdges = false := by
  intro h
  unfold validateSkosDisjointness
  by_cases hR : relatedEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) = true
  · rw [ite_eq_left hR]
  · by_cases hC : contrastEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) = true
    · rw [ite_eq_right hR, ite_eq_left hC]
    · exfalso
      exact Or.elim h (fun hRTrue => hR hRTrue) (fun hCTrue => hC hCTrue)

theorem frontier_leverage_bounded (axis : Nat) (gap : Nat) (conn : Nat) (frontier : Bool) (cite : Bool) :
    computeFrontierLeverageBps axis gap conn frontier cite ≤ 10000 := by
  unfold computeFrontierLeverageBps
  by_cases hg : gap == 0
  · rw [ite_eq_left hg]; exact Nat.zero_le 10000
  · rw [ite_eq_right hg]
    by_cases hgt : gap ≤ 2500
    · rw [ite_eq_left hgt]
      by_cases hf : frontier = true
      · rw [ite_eq_left hf]; unfold frontierLeverageCompute; exact Nat.min_le_right _ _
      · rw [ite_eq_right hf]; exact Nat.zero_le 10000
    · rw [ite_eq_right hgt]; unfold frontierLeverageCompute; exact Nat.min_le_right _ _

theorem frontier_leverage_gated (axis : Nat) (gap : Nat) (conn : Nat) (frontier : Bool) (cite : Bool) :
    (gap == 0 ∨ (gap ≤ 2500 ∧ frontier == false)) → computeFrontierLeverageBps axis gap conn frontier cite = 0 := by
  intro h
  rcases h with hg | ⟨hgt, hf⟩
  · unfold computeFrontierLeverageBps; rw [ite_eq_left hg]
  · unfold computeFrontierLeverageBps
    by_cases hg2 : gap == 0
    · rw [ite_eq_left hg2]
    · rw [ite_eq_right hg2, ite_eq_left hgt]
      have hfb : frontier = false := by simpa using hf
      rw [hfb]
      <;> simp_all

theorem frontier_leverage_monotonic_and_gated (axis : Nat) (gap : Nat) (conn : Nat) (frontier : Bool) (cite : Bool) :
    computeFrontierLeverageBps axis gap conn frontier cite ≤ 10000 := by
  exact frontier_leverage_bounded axis gap conn frontier cite

end NeuroStrata.Taxonomy
