namespace NeuroStrata.Procedure

inductive ProcedureTrust where
  | untrusted
  | trusted
  | promoted
  | local
  | absentOrUnknown
  deriving DecidableEq, Repr

def ProcedureTrust.asStr : ProcedureTrust → String
  | .untrusted => "untrusted"
  | .trusted => "trusted"
  | .promoted => "promoted"
  | .local => "local"
  | .absentOrUnknown => "absent"

@[simp]
def isUntrusted (trust : ProcedureTrust) (hasImportProvenance : Bool) : Bool :=
  match trust with
  | .untrusted => true
  | .trusted | .promoted | .local => false
  | .absentOrUnknown => hasImportProvenance

@[simp]
def isLapsed (validTo : Option Int) (now : Int) : Bool :=
  match validTo with
  | some deadline => decide (deadline ≤ now)
  | none => false

inductive ProcedureFireOutcome where
  | refuseUntrusted
  | refuseLapsed
  | refuseSpent
  | fired (remainingFires : Option Nat) (performanceCount : Nat)
  deriving DecidableEq, Repr

def ProcedureFireOutcome.asTag : ProcedureFireOutcome → String
  | .refuseUntrusted => "refuse_untrusted"
  | .refuseLapsed => "refuse_lapsed"
  | .refuseSpent => "refuse_spent"
  | .fired (some _) _ => "fired_bounded"
  | .fired none _ => "fired_unbounded"

@[simp]
def evaluateProcedureFire
    (trust : ProcedureTrust)
    (hasImportProvenance : Bool)
    (validTo : Option Int)
    (now : Int)
    (remainingFires : Option Nat)
    (performanceCount : Nat) : ProcedureFireOutcome :=
  if isUntrusted trust hasImportProvenance then .refuseUntrusted
  else if isLapsed validTo now then .refuseLapsed
  else
    match remainingFires with
    | some 0 => .refuseSpent
    | some (b + 1) => .fired (some b) (performanceCount + 1)
    | none => .fired none (performanceCount + 1)

theorem imported_unstamped_never_fires :
    ∀ (vt : Option Int) (now : Int) (b : Option Nat) (c : Nat),
      evaluateProcedureFire .absentOrUnknown true vt now b c = .refuseUntrusted := by
  intro vt now b c
  rfl

theorem lapsed_procedure_never_mutates :
    ∀ (vt now : Int) (b : Option Nat) (c : Nat),
      vt ≤ now →
      evaluateProcedureFire .local false (some vt) now b c = .refuseLapsed := by
  intro vt now b c h
  simp [evaluateProcedureFire, isUntrusted, isLapsed, h]

theorem spent_procedure_never_fires :
    ∀ (now : Int) (c : Nat),
      evaluateProcedureFire .local false none now (some 0) c = .refuseSpent := by
  intro now c
  rfl

theorem bounded_fire_conserves_budget :
    ∀ (now : Int) (b c : Nat),
      0 < b →
      evaluateProcedureFire .local false none now (some b) c = .fired (some (b - 1)) (c + 1) ∧
      (b - 1) + (c + 1) = b + c := by
  intro now b c hb
  cases b with
  | zero => omega
  | succ k =>
      constructor
      · rfl
      · omega

@[simp]
def i64MaxInt : Int := 9223372036854775807

@[simp]
def i64MinInt : Int := -9223372036854775808

@[simp]
def clampI64 (x : Int) : Int :=
  if x > i64MaxInt then i64MaxInt
  else if x < i64MinInt then i64MinInt
  else x

@[simp]
def satAddI64 (a b : Int) : Int :=
  clampI64 (a + b)

@[simp]
def satMulI64 (a b : Int) : Int :=
  clampI64 (a * b)

@[simp]
def procedureDaySeconds : Int := 86400

@[simp]
def procedureSessionEstimateSeconds : Int := 3600

@[simp]
def computeProcedureNextDue
    (now : Int)
    (lastPerformedAt : Option Int)
    (dayCadence : Option Int)
    (sessionCadence : Option Int)
    (sessionsSinceLastFire : Option Int) : Int :=
  match dayCadence, lastPerformedAt with
  | some days, some last =>
    if days > 0 then
      satAddI64 last (satMulI64 days procedureDaySeconds)
    else
      match sessionCadence with
      | some nSessions =>
        if nSessions > 1 then
          let rawElapsed := match sessionsSinceLastFire with
            | some s => s
            | none => nSessions - 1
          let elapsed := if rawElapsed < 0 then 0 else rawElapsed
          satAddI64 last (satMulI64 elapsed procedureSessionEstimateSeconds)
        else now
      | none => now
  | _, _ =>
    match sessionCadence, lastPerformedAt with
    | some nSessions, some last =>
      if nSessions > 1 then
        let rawElapsed := match sessionsSinceLastFire with
          | some s => s
          | none => nSessions - 1
        let elapsed := if rawElapsed < 0 then 0 else rawElapsed
        satAddI64 last (satMulI64 elapsed procedureSessionEstimateSeconds)
      else now
    | _, _ => now

theorem non_positive_day_cadence_without_sessions_falls_back_to_now
    (now last days : Int) (h : ¬(days > 0)) :
    computeProcedureNextDue now (some last) (some days) none none = now := by
  simp [computeProcedureNextDue, h]

end NeuroStrata.Procedure
