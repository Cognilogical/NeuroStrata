namespace NeuroStrata

inductive TourStopCategory where
  | foundations
  | coreLogic
  | surfaceAndCallers
  | testsAndDocs
  deriving DecidableEq, Repr

namespace TourStopCategory

def asTag : TourStopCategory → String
  | .foundations => "FOUNDATIONS"
  | .coreLogic => "CORE_LOGIC"
  | .surfaceAndCallers => "SURFACE_AND_CALLERS"
  | .testsAndDocs => "TESTS_AND_DOCS"

def stopNumber : TourStopCategory → Nat
  | .foundations => 1
  | .coreLogic => 2
  | .surfaceAndCallers => 3
  | .testsAndDocs => 4

end TourStopCategory

def classifyTourStop
    (isTestOrDoc : Bool)
    (isFoundationKindOrPath : Bool)
    (isEntrypointPath : Bool)
    (inDegreeInDiff : Nat)
    (outDegreeInDiff : Nat) : TourStopCategory :=
  if isTestOrDoc then
    .testsAndDocs
  else if isFoundationKindOrPath || (inDegreeInDiff > 0 && outDegreeInDiff == 0) then
    .foundations
  else if isEntrypointPath || (inDegreeInDiff == 0 && outDegreeInDiff > 0) then
    .surfaceAndCallers
  else
    .coreLogic

theorem diff_tour_classification_total_and_disjoint
    (isTestOrDoc isFoundationKindOrPath isEntrypointPath : Bool)
    (inDeg outDeg : Nat) :
    let cat := classifyTourStop isTestOrDoc isFoundationKindOrPath isEntrypointPath inDeg outDeg
    (cat = .foundations ∨ cat = .coreLogic ∨ cat = .surfaceAndCallers ∨ cat = .testsAndDocs) ∧
    1 ≤ cat.stopNumber ∧ cat.stopNumber ≤ 4 := by
  dsimp
  cases h : classifyTourStop isTestOrDoc isFoundationKindOrPath isEntrypointPath inDeg outDeg <;>
    decide

theorem diff_tour_tests_always_stop_4
    (isFoundationKindOrPath isEntrypointPath : Bool)
    (inDeg outDeg : Nat) :
    classifyTourStop true isFoundationKindOrPath isEntrypointPath inDeg outDeg = .testsAndDocs := by
  rfl

theorem diff_tour_foundations_precede_core_and_surface
    (isEntrypointPath : Bool)
    (inDeg outDeg : Nat) :
    classifyTourStop false true isEntrypointPath inDeg outDeg = .foundations ∧
    (inDeg > 0 → outDeg = 0 →
      classifyTourStop false false isEntrypointPath inDeg outDeg = .foundations) := by
  refine ⟨rfl, ?_⟩
  intro hIn hOut
  subst hOut
  unfold classifyTourStop
  cases inDeg with
  | zero => contradiction
  | succ _ => simp

end NeuroStrata
