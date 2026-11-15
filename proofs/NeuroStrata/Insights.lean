namespace NeuroStrata

inductive InsightDefineDecision where
  | defineAllowed
  | rejectedEmptyQuery
  | rejectedBuiltinCollision
  | rejectedQuotaExceeded
  deriving DecidableEq, Repr

namespace InsightDefineDecision

def asTag : InsightDefineDecision → String
  | .defineAllowed => "DEFINE_ALLOWED"
  | .rejectedEmptyQuery => "REJECTED_EMPTY_QUERY"
  | .rejectedBuiltinCollision => "REJECTED_BUILTIN_COLLISION"
  | .rejectedQuotaExceeded => "REJECTED_QUOTA_EXCEEDED"

end InsightDefineDecision

def evaluateInsightDefine
    (queryNonEmpty : Bool)
    (isBuiltinSlug : Bool)
    (isExistingSlug : Bool)
    (existingCustomCount : Nat)
    (maxCustomInsights : Nat) : InsightDefineDecision :=
  if !queryNonEmpty then
    .rejectedEmptyQuery
  else if isBuiltinSlug then
    .rejectedBuiltinCollision
  else if !isExistingSlug && existingCustomCount >= maxCustomInsights then
    .rejectedQuotaExceeded
  else
    .defineAllowed

def isMemoryValidAt
    (validFrom : Int)
    (hasValidTo : Bool)
    (validTo : Int)
    (asOfTs : Int) : Bool :=
  decide (validFrom ≤ asOfTs) && (!hasValidTo || decide (validTo > asOfTs))

theorem insight_define_allowed_iff
    (queryNonEmpty isBuiltinSlug isExistingSlug : Bool)
    (existingCustomCount maxCustomInsights : Nat) :
    evaluateInsightDefine queryNonEmpty isBuiltinSlug isExistingSlug existingCustomCount maxCustomInsights = .defineAllowed ↔
      queryNonEmpty = true ∧
      isBuiltinSlug = false ∧
      (isExistingSlug = true ∨ existingCustomCount < maxCustomInsights) := by
  unfold evaluateInsightDefine
  cases queryNonEmpty <;> cases isBuiltinSlug <;> cases isExistingSlug <;> simp [Nat.not_le]

theorem bitemporal_open_ended_valid_from_onward
    (validFrom validTo asOfTs : Int)
    (h : validFrom ≤ asOfTs) :
    isMemoryValidAt validFrom false validTo asOfTs = true := by
  unfold isMemoryValidAt
  simp [h]

theorem bitemporal_retired_invalid_at_or_after_valid_to
    (validFrom validTo asOfTs : Int)
    (h : validTo ≤ asOfTs) :
    isMemoryValidAt validFrom true validTo asOfTs = false := by
  unfold isMemoryValidAt
  have hNotGt : ¬ (validTo > asOfTs) := Int.not_lt.mpr h
  simp [hNotGt]

end NeuroStrata
