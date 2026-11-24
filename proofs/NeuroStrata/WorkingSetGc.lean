namespace NeuroStrata.WorkingSetGc

def workingSetMaxFrameBytes : Nat := 4096
def workingSetStubMaxBytes : Nat := 192

inductive WorkingSetTier where
  | immortal
  | pinnedRoot
  | youngGen
  | evictedStub
  deriving DecidableEq, Repr

def WorkingSetTier.asStr : WorkingSetTier → String
  | .immortal => "Immortal"
  | .pinnedRoot => "PinnedRoot"
  | .youngGen => "YoungGen"
  | .evictedStub => "EvictedStub"

inductive GcAction where
  | retainFull
  | promoteToYoungGen
  | demoteToStub
  | retainStub
  deriving DecidableEq, Repr

def GcAction.asTag : GcAction → String
  | .retainFull => "retain_full"
  | .promoteToYoungGen => "promote_to_young_gen"
  | .demoteToStub => "demote_to_stub"
  | .retainStub => "retain_stub"

structure WorkingSetEvictionInput where
  tier : WorkingSetTier
  turnAge : Nat
  maxIdleTurns : Nat
  nodeBytes : Nat
  projectedFrameBytes : Nat
  maxFrameBytes : Nat
  isReReferenced : Bool
  deriving DecidableEq, Repr

/-- Mirror of the Rust first-match decision table. -/
def evaluateWorkingSetEviction (input : WorkingSetEvictionInput) : GcAction :=
  match input.tier with
  | .immortal => .retainFull
  | .evictedStub =>
      if input.isReReferenced && input.projectedFrameBytes ≤ input.maxFrameBytes then
        .promoteToYoungGen
      else
        .retainStub
  | .youngGen =>
      if !input.isReReferenced && input.turnAge > input.maxIdleTurns then
        .demoteToStub
      else if input.projectedFrameBytes > input.maxFrameBytes then
        .demoteToStub
      else
        .retainFull
  | .pinnedRoot =>
      if !input.isReReferenced && input.turnAge > 0 &&
          input.projectedFrameBytes > input.maxFrameBytes then
        .demoteToStub
      else
        .retainFull

/-- A state layer may permit a demotion only with this strictly smaller stub. -/
def validStub (nodeBytes stubBytes : Nat) : Prop :=
  stubBytes ≤ workingSetStubMaxBytes ∧ stubBytes < nodeBytes

/-- Nodes eligible for budget pressure; TTL-expired young nodes demote earlier. -/
def pressureEligible (input : WorkingSetEvictionInput) (stubBytes : Nat) : Prop :=
  validStub input.nodeBytes stubBytes ∧
    ((input.tier = .youngGen ∧
        (input.isReReferenced = true ∨ input.turnAge ≤ input.maxIdleTurns)) ∨
      (input.tier = .pinnedRoot ∧ input.isReReferenced = false ∧ 0 < input.turnAge))

/-- The budget theorem covers every non-immortal full tier allowed by the table. -/
def budgetEligible (input : WorkingSetEvictionInput) : Prop :=
  input.tier = .youngGen ∨
    (input.tier = .pinnedRoot ∧ input.isReReferenced = false ∧ 0 < input.turnAge)

inductive PressureTier where
  | youngGen
  | pinnedRoot
  deriving DecidableEq, Repr

def pressureTierRank : PressureTier → Nat
  | .youngGen => 0
  | .pinnedRoot => 1

structure PressureCandidate where
  tier : PressureTier
  turnAge : Nat
  byteCost : Nat
  id : String
  deriving DecidableEq, Repr

structure PressureKey where
  tierRank : Nat
  turnAge : Nat
  byteCost : Nat
  id : String
  deriving DecidableEq, Repr

/-- Canonical key: young first, older and larger first, then ascending identity. -/
def pressureKey (candidate : PressureCandidate) : PressureKey :=
  ⟨pressureTierRank candidate.tier, candidate.turnAge, candidate.byteCost, candidate.id⟩

/-- The key's order, with age and cost interpreted in descending direction. -/
@[reducible]
def pressureLEProp (a b : PressureCandidate) : Prop :=
  pressureTierRank a.tier < pressureTierRank b.tier ∨
    (pressureTierRank a.tier = pressureTierRank b.tier ∧
      (b.turnAge < a.turnAge ∨
        (a.turnAge = b.turnAge ∧
          (b.byteCost < a.byteCost ∨
            (a.byteCost = b.byteCost ∧ a.id ≤ b.id)))))

def pressureLE (a b : PressureCandidate) : Bool := decide (pressureLEProp a b)

def canonicalPressureSort (candidates : List PressureCandidate) : List PressureCandidate :=
  candidates.mergeSort pressureLE

theorem immortal_never_evicted (input : WorkingSetEvictionInput)
    (hTier : input.tier = .immortal) :
    evaluateWorkingSetEviction input = .retainFull := by
  simp [evaluateWorkingSetEviction, hTier]

theorem page_fault_promotes_stub (input : WorkingSetEvictionInput)
    (hTier : input.tier = .evictedStub) :
    evaluateWorkingSetEviction input = .promoteToYoungGen ↔
      input.isReReferenced = true ∧ input.projectedFrameBytes ≤ input.maxFrameBytes := by
  simp [evaluateWorkingSetEviction, hTier, Bool.and_eq_true]

theorem idle_young_gen_demotes_to_stub (input : WorkingSetEvictionInput)
    (hTier : input.tier = .youngGen)
    (hReference : input.isReReferenced = false)
    (hIdle : input.maxIdleTurns < input.turnAge) :
    evaluateWorkingSetEviction input = .demoteToStub := by
  simp [evaluateWorkingSetEviction, hTier, hReference, hIdle]

theorem budget_overflow_demotes_non_immortal (input : WorkingSetEvictionInput)
    (stubBytes : Nat)
    (hEligible : budgetEligible input)
    (hOverflow : input.maxFrameBytes < input.projectedFrameBytes)
    (hLarge : workingSetStubMaxBytes < input.nodeBytes)
    (hStub : validStub input.nodeBytes stubBytes) :
    evaluateWorkingSetEviction input = .demoteToStub ∧
      input.nodeBytes - workingSetStubMaxBytes ≤ input.nodeBytes - stubBytes := by
  have hStubBound : stubBytes ≤ workingSetStubMaxBytes := hStub.1
  constructor
  · rcases hEligible with hTier | ⟨hTier, hReference, hAge⟩
    · simp [evaluateWorkingSetEviction, hTier, hOverflow]
    · simp [evaluateWorkingSetEviction, hTier, hReference, hAge, hOverflow]
  · omega

theorem eviction_deterministic :
    (∀ a b : WorkingSetEvictionInput, a = b →
      evaluateWorkingSetEviction a = evaluateWorkingSetEviction b) ∧
    (∀ candidates : List PressureCandidate,
      canonicalPressureSort (canonicalPressureSort candidates) =
        canonicalPressureSort candidates) ∧
    (∀ a b : PressureCandidate, pressureKey a = pressureKey b → a = b) := by
  refine ⟨?_, ?_, ?_⟩
  · intro a b h
    rw [h]
  · intro candidates
    apply List.mergeSort_of_pairwise (le := pressureLE)
    apply List.pairwise_mergeSort (le := pressureLE)
    · intro a b c hab hbc
      simp only [pressureLE, decide_eq_true_eq] at hab hbc ⊢
      rcases hab with hr | ⟨hr, ha⟩
      · rcases hbc with hr' | ⟨hr', _⟩
        · exact Or.inl (by omega)
        · exact Or.inl (by omega)
      · rcases hbc with hr' | ⟨hr', hb⟩
        · exact Or.inl (by omega)
        · refine Or.inr ⟨by omega, ?_⟩
          rcases ha with ha | ⟨ha, hc⟩
          · rcases hb with hb | ⟨hb, _⟩
            · exact Or.inl (by omega)
            · exact Or.inl (by omega)
          · rcases hb with hb | ⟨hb, hd⟩
            · exact Or.inl (by omega)
            · refine Or.inr ⟨by omega, ?_⟩
              rcases hc with hc | ⟨hc, hi⟩
              · rcases hd with hd | ⟨hd, _⟩
                · exact Or.inl (by omega)
                · exact Or.inl (by omega)
              · rcases hd with hd | ⟨hd, hj⟩
                · exact Or.inl (by omega)
                · exact Or.inr ⟨by omega, String.le_trans hi hj⟩
    · intro a b
      simp only [pressureLE, Bool.or_eq_true, decide_eq_true_eq]
      by_cases hr : pressureTierRank a.tier < pressureTierRank b.tier
      · exact Or.inl (Or.inl hr)
      by_cases hr' : pressureTierRank b.tier < pressureTierRank a.tier
      · exact Or.inr (Or.inl hr')
      have hrank : pressureTierRank a.tier = pressureTierRank b.tier := by omega
      by_cases ha : b.turnAge < a.turnAge
      · exact Or.inl (Or.inr ⟨hrank, Or.inl ha⟩)
      by_cases ha' : a.turnAge < b.turnAge
      · exact Or.inr (Or.inr ⟨by omega, Or.inl ha'⟩)
      have hage : a.turnAge = b.turnAge := by omega
      by_cases hc : b.byteCost < a.byteCost
      · exact Or.inl (Or.inr ⟨hrank, Or.inr ⟨hage, Or.inl hc⟩⟩)
      by_cases hc' : a.byteCost < b.byteCost
      · exact Or.inr (Or.inr ⟨by omega, Or.inr ⟨by omega, Or.inl hc'⟩⟩)
      have hcost : a.byteCost = b.byteCost := by omega
      rcases String.le_total a.id b.id with hid | hid
      · exact Or.inl (Or.inr ⟨hrank, Or.inr ⟨hage, Or.inr ⟨hcost, hid⟩⟩⟩)
      · exact Or.inr (Or.inr ⟨by omega, Or.inr ⟨by omega, Or.inr ⟨by omega, hid⟩⟩⟩)
  · intro a b h
    rcases a with ⟨aTier, aAge, aCost, aId⟩
    rcases b with ⟨bTier, bAge, bCost, bId⟩
    simp only [pressureKey, PressureKey.mk.injEq] at h
    obtain ⟨hr, ha, hc, hi⟩ := h
    have ht : aTier = bTier := by
      cases aTier <;> cases bTier <;> simp_all [pressureTierRank]
    cases ht
    cases ha
    cases hc
    cases hi
    rfl

end NeuroStrata.WorkingSetGc
