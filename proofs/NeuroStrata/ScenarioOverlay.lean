namespace NeuroStrata

inductive ScenarioRowVisibility where
  | visibleStaged
  | hiddenRetired
  | visibleLive
  | absent
  deriving DecidableEq, Repr

namespace ScenarioRowVisibility

def asTag : ScenarioRowVisibility → String
  | .visibleStaged => "VISIBLE_STAGED"
  | .hiddenRetired => "HIDDEN_RETIRED"
  | .visibleLive => "VISIBLE_LIVE"
  | .absent => "ABSENT"

end ScenarioRowVisibility

def resolveScenarioRow
    (livePresent : Bool)
    (stagedRetired : Bool)
    (stagedUpsert : Bool) : ScenarioRowVisibility :=
  if stagedUpsert then
    .visibleStaged
  else if stagedRetired then
    .hiddenRetired
  else if livePresent then
    .visibleLive
  else
    .absent

theorem scenario_upsert_overrides_retirement_and_live
    (livePresent : Bool) :
    resolveScenarioRow livePresent true true = .visibleStaged ∧
    resolveScenarioRow livePresent true false = .hiddenRetired := by
  constructor <;> rfl

inductive ScenarioCommitDecision where
  | commitAllowed
  | rejectedExpired
  | rejectedBaseConflict
  | rejectedUnauthorized
  | rejectedShaclViolations
  deriving DecidableEq, Repr

namespace ScenarioCommitDecision

def asTag : ScenarioCommitDecision → String
  | .commitAllowed => "COMMIT_ALLOWED"
  | .rejectedExpired => "REJECTED_EXPIRED"
  | .rejectedBaseConflict => "REJECTED_BASE_CONFLICT"
  | .rejectedUnauthorized => "REJECTED_UNAUTHORIZED"
  | .rejectedShaclViolations => "REJECTED_SHACL_VIOLATIONS"

end ScenarioCommitDecision

def evaluateScenarioCommit
    (expired : Bool)
    (allBaseFingerprintsMatch : Bool)
    (rebacAuthorized : Bool)
    (enforceShacl : Bool)
    (shaclViolationCount : Nat) : ScenarioCommitDecision :=
  if expired then
    .rejectedExpired
  else if !allBaseFingerprintsMatch then
    .rejectedBaseConflict
  else if !rebacAuthorized then
    .rejectedUnauthorized
  else if enforceShacl && shaclViolationCount > 0 then
    .rejectedShaclViolations
  else
    .commitAllowed

theorem scenario_commit_cas_sound
    (expired allBaseFingerprintsMatch rebacAuthorized enforceShacl : Bool)
    (shaclViolationCount : Nat)
    (h : evaluateScenarioCommit expired allBaseFingerprintsMatch rebacAuthorized enforceShacl shaclViolationCount = .commitAllowed) :
    allBaseFingerprintsMatch = true ∧ expired = false ∧ rebacAuthorized = true := by
  unfold evaluateScenarioCommit at h
  cases expired <;> simp at h
  cases allBaseFingerprintsMatch <;> simp at h
  cases rebacAuthorized <;> simp at h
  exact ⟨rfl, rfl, rfl⟩

theorem scenario_commit_shacl_gate
    (expired allBaseFingerprintsMatch rebacAuthorized : Bool)
    (shaclViolationCount : Nat)
    (hCount : shaclViolationCount > 0) :
    evaluateScenarioCommit expired allBaseFingerprintsMatch rebacAuthorized true shaclViolationCount ≠ .commitAllowed := by
  unfold evaluateScenarioCommit
  cases expired <;> simp
  cases allBaseFingerprintsMatch <;> simp
  cases rebacAuthorized <;> simp [hCount]

end NeuroStrata
