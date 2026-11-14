namespace NeuroStrata.HybridSlots

/-- Allocation of result slots across reserved primary-normative, reserved graph-expansion,
    and general remaining pools (`neurostrata-5xjo`). -/
structure HybridSlotAllocation where
  normativeTaken : Nat
  expansionTaken : Nat
  remainingTaken : Nat
  deriving DecidableEq, Repr

/-- Computes the normative slot quota: `min(3, (limit + 1) / 2)` when `limit >= 3`, else `0`. -/
def hybridNormativeQuota (limit : Nat) : Nat :=
  if limit < 3 then 0 else min 3 ((limit + 1) / 2)

/-- Computes the graph-expansion slot quota: `min(4, limit / 4)` when `limit >= 4`, else `0`. -/
def hybridExpansionQuota (limit : Nat) : Nat :=
  if limit < 4 then 0 else min 4 (limit / 4)

/-- Pure slot allocator for two-pool reservation (`reserve_normative_and_expansion_slots`). -/
def allocateHybridSlots
    (limit availNormative availExpansion availOther : Nat) : HybridSlotAllocation :=
  let normQ := hybridNormativeQuota limit
  let expQ := hybridExpansionQuota limit
  let expTaken := min expQ availExpansion
  let afterExp := limit - expTaken
  let normCap := min normQ afterExp
  let normTaken := min normCap availNormative
  let remSlots := afterExp - normTaken
  let leftoverNorm := availNormative - normTaken
  let leftoverExp := availExpansion - expTaken
  let leftoverPool := leftoverNorm + leftoverExp + availOther
  let remTaken := min remSlots leftoverPool
  { normativeTaken := normTaken
  , expansionTaken := expTaken
  , remainingTaken := remTaken }

/-- Theorem 1: Total slots allocated across all three pools never exceed `limit`. -/
theorem hybrid_slots_never_exceed_limit
    (limit availNormative availExpansion availOther : Nat) :
    let alloc := allocateHybridSlots limit availNormative availExpansion availOther
    alloc.normativeTaken + alloc.expansionTaken + alloc.remainingTaken ≤ limit := by
  dsimp [allocateHybridSlots, hybridNormativeQuota, hybridExpansionQuota]
  by_cases h3 : limit < 3 <;> by_cases h4 : limit < 4 <;> simp [h3, h4] <;> omega

/-- Theorem 2: Each reserved pool's allocation never exceeds its available candidate count,
    and the total allocated never exceeds total available candidates. -/
theorem hybrid_slots_never_exceed_available
    (limit availNormative availExpansion availOther : Nat) :
    let alloc := allocateHybridSlots limit availNormative availExpansion availOther
    alloc.normativeTaken ≤ availNormative ∧
    alloc.expansionTaken ≤ availExpansion ∧
    alloc.normativeTaken + alloc.expansionTaken + alloc.remainingTaken ≤
      availNormative + availExpansion + availOther := by
  dsimp [allocateHybridSlots, hybridNormativeQuota, hybridExpansionQuota]
  omega

/-- Theorem 3: Whenever `limit >= 4` and at least one graph-expanded candidate is available,
    at least one expansion slot is guaranteed. -/
theorem hybrid_slots_guarantees_expansion_when_available
    (limit availNormative availExpansion availOther : Nat)
    (hLimit : limit ≥ 4)
    (hAvail : availExpansion ≥ 1) :
    (allocateHybridSlots limit availNormative availExpansion availOther).expansionTaken ≥ 1 := by
  dsimp [allocateHybridSlots, hybridExpansionQuota]
  by_cases h4 : limit < 4 <;> simp [h4] <;> omega

end NeuroStrata.HybridSlots
