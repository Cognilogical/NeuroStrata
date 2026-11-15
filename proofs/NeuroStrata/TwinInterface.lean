namespace NeuroStrata.TwinInterface

inductive TwinBoundError where
  | invertedInterfaceBounds
  | invertedTwinBounds
  | disjointBounds
  deriving DecidableEq, Repr

def maxInt (a b : Int) : Int := if a >= b then a else b
def minInt (a b : Int) : Int := if a <= b then a else b

def intersectVarBounds (ifaceMin ifaceMax twinMin twinMax : Int) : Except TwinBoundError (Int × Int) :=
  if ifaceMin > ifaceMax then
    .error .invertedInterfaceBounds
  else if twinMin > twinMax then
    .error .invertedTwinBounds
  else
    let effMin := maxInt ifaceMin twinMin
    let effMax := minInt ifaceMax twinMax
    if effMin > effMax then
      .error .disjointBounds
    else
      .ok (effMin, effMax)

def formatBoundsOutcome (res : Except TwinBoundError (Int × Int)) : String :=
  match res with
  | .error .invertedInterfaceBounds => "err_inverted_iface"
  | .error .invertedTwinBounds => "err_inverted_twin"
  | .error .disjointBounds => "err_disjoint"
  | .ok (emin, emax) => s!"ok_{emin}_{emax}"

inductive ReducerOpKind where
  | count
  | sum
  | min
  | max
  | avg
  | allInBounds
  | anyOutOfBounds
  deriving DecidableEq, Repr

def ReducerOpKind.asStr : ReducerOpKind → String
  | .count => "count"
  | .sum => "sum"
  | .min => "min"
  | .max => "max"
  | .avg => "avg"
  | .allInBounds => "all_in_bounds"
  | .anyOutOfBounds => "any_out_of_bounds"

structure ReducerEvalOutcome where
  numericVal : Int
  boolVal : Bool
  isDefined : Bool
  deriving DecidableEq, Repr

def evaluateReducerTotal
    (op : ReducerOpKind)
    (count : Nat)
    (sumVal minVal maxVal : Int)
    (anyOutOfBounds : Bool) : ReducerEvalOutcome :=
  if count == 0 then
    match op with
    | .count | .sum | .avg => { numericVal := 0, boolVal := false, isDefined := true }
    | .min | .max => { numericVal := 0, boolVal := false, isDefined := false }
    | .allInBounds => { numericVal := 1, boolVal := true, isDefined := true }
    | .anyOutOfBounds => { numericVal := 0, boolVal := false, isDefined := true }
  else
    match op with
    | .count => { numericVal := Int.ofNat count, boolVal := true, isDefined := true }
    | .sum => { numericVal := sumVal, boolVal := true, isDefined := true }
    | .min => { numericVal := minVal, boolVal := true, isDefined := true }
    | .max => { numericVal := maxVal, boolVal := true, isDefined := true }
    | .avg => { numericVal := Int.tdiv sumVal (Int.ofNat count), boolVal := true, isDefined := true }
    | .allInBounds =>
      { numericVal := if !anyOutOfBounds then 1 else 0
      , boolVal := !anyOutOfBounds
      , isDefined := true }
    | .anyOutOfBounds =>
      { numericVal := if anyOutOfBounds then 1 else 0
      , boolVal := anyOutOfBounds
      , isDefined := true }

theorem intersect_bounds_tightens_interface
    (imin imax tmin tmax emin emax : Int)
    (h : intersectVarBounds imin imax tmin tmax = .ok (emin, emax)) :
    imin ≤ emin ∧ emin ≤ emax ∧ emax ≤ imax := by
  unfold intersectVarBounds at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · dsimp only at h
      split at h
      · contradiction
      · rename_i h_iface h_twin h_disj
        injection h with hpair
        injection hpair with hemin hemax
        subst hemin hemax
        unfold maxInt minInt at h_disj ⊢
        by_cases h1 : imin ≥ tmin <;> by_cases h2 : imax ≤ tmax <;> simp [h1, h2] at h_disj ⊢ <;> omega

theorem intersect_bounds_commutative
    (a b c d : Int)
    (hab : a ≤ b)
    (hcd : c ≤ d) :
    intersectVarBounds a b c d = intersectVarBounds c d a b := by
  unfold intersectVarBounds
  have h1 : ¬ (a > b) := by omega
  have h2 : ¬ (c > d) := by omega
  simp [h1, h2]
  have hmin : maxInt a c = maxInt c a := by
    unfold maxInt
    by_cases h : a ≥ c <;> by_cases h' : c ≥ a <;> simp [h, h'] <;> omega
  have hmax : minInt b d = minInt d b := by
    unfold minInt
    by_cases h : b ≤ d <;> by_cases h' : d ≤ b <;> simp [h, h'] <;> omega
  rw [hmin, hmax]

theorem reducer_eval_total_on_empty
    (op : ReducerOpKind)
    (sumVal minVal maxVal : Int)
    (anyOob : Bool) :
    (evaluateReducerTotal op 0 sumVal minVal maxVal anyOob).numericVal =
      (if op = .allInBounds then 1 else 0) ∧
    (evaluateReducerTotal op 0 sumVal minVal maxVal anyOob).boolVal =
      (if op = .allInBounds then true else false) := by
  cases op <;> exact ⟨rfl, rfl⟩

end NeuroStrata.TwinInterface
