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

def frontierLeverageRawNumer
    (axisAlignmentBps : Nat)
    (realizationGapBps : Nat)
    (canonicalConnectivityCount : Nat)
    (hasFrontierOrLimitation : Bool)
    (hasReferencesOrComparables : Bool) : Nat :=
  35 * Nat.min axisAlignmentBps 10000
    + 25 * Nat.min realizationGapBps 10000
    + 20 * ((Nat.min canonicalConnectivityCount 10) * 1000)
    + 10 * (if hasFrontierOrLimitation then 10000 else 0)
    + 10 * (if hasReferencesOrComparables then 10000 else 0)

def frontierLeverageCompute
    (axisAlignmentBps : Nat)
    (realizationGapBps : Nat)
    (canonicalConnectivityCount : Nat)
    (hasFrontierOrLimitation : Bool)
    (hasReferencesOrComparables : Bool) : Nat :=
  Nat.min
    (frontierLeverageRawNumer
      axisAlignmentBps
      realizationGapBps
      canonicalConnectivityCount
      hasFrontierOrLimitation
      hasReferencesOrComparables / 100)
    10000

def computeFrontierLeverageBps
    (axisAlignmentBps : Nat)
    (realizationGapBps : Nat)
    (canonicalConnectivityCount : Nat)
    (hasFrontierOrLimitation : Bool)
    (hasReferencesOrLimitationOrComparables : Bool) : Nat :=
  if realizationGapBps == 0 then 0
  else if realizationGapBps ≤ 2500 then
    if hasFrontierOrLimitation then
      frontierLeverageCompute
        axisAlignmentBps
        realizationGapBps
        canonicalConnectivityCount
        hasFrontierOrLimitation
        hasReferencesOrLimitationOrComparables
    else 0
  else
    frontierLeverageCompute
      axisAlignmentBps
      realizationGapBps
      canonicalConnectivityCount
      hasFrontierOrLimitation
      hasReferencesOrLimitationOrComparables

def hasSelfLoop (broaderEdges : List (Nat × Nat)) : Bool :=
  broaderEdges.any (fun e => e.1 == e.2)

def stepTargets (edges : List (Nat × Nat)) (frontier : List Nat) : List Nat :=
  (edges.filter (fun e => frontier.contains e.1)).map Prod.snd

def reachableWithin (fuel : Nat) (edges : List (Nat × Nat)) (frontier : List Nat) : List Nat :=
  match fuel with
  | 0 => frontier
  | k + 1 =>
    let next := stepTargets edges frontier
    reachableWithin k edges (frontier ++ next)

def broaderReaches (broaderEdges : List (Nat × Nat)) (src dst : Nat) : Bool :=
  let fuel := broaderEdges.length + 1
  (reachableWithin fuel broaderEdges [src]).contains dst

def hasDirectedCycle (broaderEdges : List (Nat × Nat)) : Bool :=
  broaderEdges.any (fun e => broaderReaches broaderEdges e.2 e.1)

def skosPairDisjoint (broaderEdges : List (Nat × Nat)) (u v : Nat) : Bool :=
  if u == v then false
  else if broaderReaches broaderEdges u v then false
  else if broaderReaches broaderEdges v u then false
  else true

def validateTaxonomyDag (broaderEdges : List (Nat × Nat)) : Bool :=
  if hasSelfLoop broaderEdges then false
  else if hasDirectedCycle broaderEdges then false
  else true

def validateSkosDisjointness
    (broaderEdges : List (Nat × Nat))
    (relatedEdges : List (Nat × Nat))
    (contrastEdges : List (Nat × Nat)) : Bool :=
  if validateTaxonomyDag broaderEdges == false then false
  else if relatedEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) then false
  else if contrastEdges.any (fun e => skosPairDisjoint broaderEdges e.1 e.2 == false) then false
  else true

/-!
## 1. Epistemic Status Monotonicity & Bounds
-/

theorem classify_epistemic_monotonic
    (d1 d2 t1 t2 s1 s2 g1 g2 : Bool)
    (hd : d1 = true → d2 = true)
    (ht : t1 = true → t2 = true)
    (hs : s1 = true → s2 = true)
    (hg : g1 = true → g2 = true) :
    EpistemicStatus.rank (classifyEpistemicStatus d1 t1 s1 g1) ≤
      EpistemicStatus.rank (classifyEpistemicStatus d2 t2 s2 g2) := by
  cases d1 <;> cases d2 <;> cases t1 <;> cases t2 <;>
    cases s1 <;> cases s2 <;> cases g1 <;> cases g2 <;>
    simp_all [classifyEpistemicStatus, EpistemicStatus.rank]

theorem classify_epistemic_weight_monotonic
    (d1 d2 t1 t2 s1 s2 g1 g2 : Bool)
    (hd : d1 = true → d2 = true)
    (ht : t1 = true → t2 = true)
    (hs : s1 = true → s2 = true)
    (hg : g1 = true → g2 = true) :
    EpistemicStatus.weightBps (classifyEpistemicStatus d1 t1 s1 g1) ≤
      EpistemicStatus.weightBps (classifyEpistemicStatus d2 t2 s2 g2) := by
  cases d1 <;> cases d2 <;> cases t1 <;> cases t2 <;>
    cases s1 <;> cases s2 <;> cases g1 <;> cases g2 <;>
    simp_all [classifyEpistemicStatus, EpistemicStatus.weightBps]

theorem epistemic_status_weight_bounded (s : EpistemicStatus) :
    EpistemicStatus.weightBps s ≤ 10000 := by
  cases s <;> decide

theorem verified_requires_all_pillars (d : Bool) (t : Bool) (s : Bool) (g : Bool) :
    classifyEpistemicStatus d t s g = .verified →
    d = true ∧ t = true ∧ s = true ∧ g = true := by
  intro h
  unfold classifyEpistemicStatus at h
  cases d <;> cases t <;> cases s <;> cases g <;> simp_all

/-!
## 2. Taxonomy DAG General Cycle-Rejection Theorems
-/

theorem mem_reachableWithin_of_mem
    (fuel : Nat)
    (edges : List (Nat × Nat))
    (frontier : List Nat)
    (x : Nat)
    (hx : x ∈ frontier) :
    x ∈ reachableWithin fuel edges frontier := by
  induction fuel generalizing frontier with
  | zero => exact hx
  | succ k ih =>
    unfold reachableWithin
    exact ih (frontier ++ stepTargets edges frontier) (List.mem_append_left _ hx)

theorem stepTargets_mem_of_edge
    (edges : List (Nat × Nat))
    (frontier : List Nat)
    (e : Nat × Nat)
    (hEdge : e ∈ edges)
    (hSrc : frontier.contains e.1 = true) :
    e.2 ∈ stepTargets edges frontier := by
  unfold stepTargets
  rw [List.mem_map]
  refine ⟨e, ?_, rfl⟩
  exact List.mem_filter.mpr ⟨hEdge, hSrc⟩

theorem mem_reachableWithin_step
    (fuel : Nat)
    (edges : List (Nat × Nat))
    (frontier : List Nat)
    (e : Nat × Nat)
    (hEdge : e ∈ edges)
    (hSrc : frontier.contains e.1 = true) :
    e.2 ∈ reachableWithin (fuel + 1) edges frontier := by
  unfold reachableWithin
  apply mem_reachableWithin_of_mem
  exact List.mem_append_right _ (stepTargets_mem_of_edge edges frontier e hEdge hSrc)

theorem dag_rejects_self_loop_edge
    (edges : List (Nat × Nat))
    (e : Nat × Nat)
    (hMem : e ∈ edges)
    (hLoop : (e.1 == e.2) = true) :
    validateTaxonomyDag edges = false := by
  have hSelf : hasSelfLoop edges = true := by
    unfold hasSelfLoop
    exact List.any_eq_true.mpr ⟨e, hMem, hLoop⟩
  unfold validateTaxonomyDag
  simp [hSelf]

theorem dag_rejects_directed_cycle_edge
    (edges : List (Nat × Nat))
    (e : Nat × Nat)
    (hMem : e ∈ edges)
    (hReach : broaderReaches edges e.2 e.1 = true) :
    validateTaxonomyDag edges = false := by
  have hCyc : hasDirectedCycle edges = true := by
    unfold hasDirectedCycle
    exact List.any_eq_true.mpr ⟨e, hMem, hReach⟩
  unfold validateTaxonomyDag
  by_cases hSelf : hasSelfLoop edges = true
  · simp [hSelf]
  · simp [hSelf, hCyc]

theorem dag_valid_implies_no_self_loop_or_cycle
    (edges : List (Nat × Nat))
    (hValid : validateTaxonomyDag edges = true)
    (e : Nat × Nat)
    (hMem : e ∈ edges) :
    (e.1 == e.2) = false ∧ broaderReaches edges e.2 e.1 = false := by
  constructor
  · cases hEq : (e.1 == e.2) with
    | false => rfl
    | true =>
      have hRej := dag_rejects_self_loop_edge edges e hMem hEq
      rw [hValid] at hRej
      contradiction
  · cases hR : broaderReaches edges e.2 e.1 with
    | false => rfl
    | true =>
      have hRej := dag_rejects_directed_cycle_edge edges e hMem hR
      rw [hValid] at hRej
      contradiction

/-!
## 3. SKOS S27 & Contrast Disjointness Soundness
-/

theorem skos_pair_disjoint_sound
    (broader : List (Nat × Nat))
    (u v : Nat)
    (hDisj : skosPairDisjoint broader u v = true) :
    (u == v) = false ∧ broaderReaches broader u v = false ∧ broaderReaches broader v u = false := by
  unfold skosPairDisjoint at hDisj
  cases h1 : (u == v) <;> cases h2 : broaderReaches broader u v <;> cases h3 : broaderReaches broader v u <;>
    simp_all

theorem skos_disjointness_requires_dag
    (broader : List (Nat × Nat))
    (related : List (Nat × Nat))
    (contrast : List (Nat × Nat))
    (hOk : validateSkosDisjointness broader related contrast = true) :
    validateTaxonomyDag broader = true := by
  unfold validateSkosDisjointness at hOk
  cases hDag : validateTaxonomyDag broader with
  | true => rfl
  | false => simp [hDag] at hOk

theorem skos_disjointness_rejects_related_violation
    (broader : List (Nat × Nat))
    (related : List (Nat × Nat))
    (contrast : List (Nat × Nat))
    (e : Nat × Nat)
    (hMem : e ∈ related)
    (hBad : skosPairDisjoint broader e.1 e.2 = false) :
    validateSkosDisjointness broader related contrast = false := by
  have hAny : (related.any (fun x => skosPairDisjoint broader x.1 x.2 == false)) = true := by
    refine List.any_eq_true.mpr ⟨e, hMem, ?_⟩
    simp [hBad]
  unfold validateSkosDisjointness
  cases hDag : validateTaxonomyDag broader with
  | false => rfl
  | true =>
    dsimp
    rw [hAny]
    rfl

theorem skos_disjointness_rejects_contrast_violation
    (broader : List (Nat × Nat))
    (related : List (Nat × Nat))
    (contrast : List (Nat × Nat))
    (e : Nat × Nat)
    (hMem : e ∈ contrast)
    (hBad : skosPairDisjoint broader e.1 e.2 = false) :
    validateSkosDisjointness broader related contrast = false := by
  have hAny : (contrast.any (fun x => skosPairDisjoint broader x.1 x.2 == false)) = true := by
    refine List.any_eq_true.mpr ⟨e, hMem, ?_⟩
    simp [hBad]
  unfold validateSkosDisjointness
  cases hDag : validateTaxonomyDag broader with
  | false => rfl
  | true =>
    dsimp
    cases hRel : related.any (fun x => skosPairDisjoint broader x.1 x.2 == false) with
    | true => rfl
    | false =>
      dsimp
      rw [hAny]
      rfl

theorem skos_disjointness_sound
    (broader : List (Nat × Nat))
    (related : List (Nat × Nat))
    (contrast : List (Nat × Nat))
    (hOk : validateSkosDisjointness broader related contrast = true) :
    validateTaxonomyDag broader = true ∧
    (∀ e ∈ related,
      (e.1 == e.2) = false ∧
      broaderReaches broader e.1 e.2 = false ∧
      broaderReaches broader e.2 e.1 = false) ∧
    (∀ e ∈ contrast,
      (e.1 == e.2) = false ∧
      broaderReaches broader e.1 e.2 = false ∧
      broaderReaches broader e.2 e.1 = false) := by
  refine ⟨skos_disjointness_requires_dag broader related contrast hOk, ?_, ?_⟩
  · intro e hMem
    apply skos_pair_disjoint_sound
    cases hPair : skosPairDisjoint broader e.1 e.2 with
    | true => rfl
    | false =>
      have hRej := skos_disjointness_rejects_related_violation broader related contrast e hMem hPair
      rw [hOk] at hRej
      contradiction
  · intro e hMem
    apply skos_pair_disjoint_sound
    cases hPair : skosPairDisjoint broader e.1 e.2 with
    | true => rfl
    | false =>
      have hRej := skos_disjointness_rejects_contrast_violation broader related contrast e hMem hPair
      rw [hOk] at hRej
      contradiction

/-!
## 4. Epistemic Coverage & Frontier Leverage Bounds, Gating, and Monotonicity
-/

theorem epistemic_coverage_bounded (statuses : List EpistemicStatus) (u : Nat) :
    computeEpistemicCoverageBps statuses u ≤ 10000 := by
  unfold computeEpistemicCoverageBps
  by_cases h : (statuses.length + u == 0) = true
  · simp [h]
  · simp [h, Nat.min_le_right]

theorem frontier_leverage_bounded
    (a : Nat) (g : Nat) (c : Nat) (f : Bool) (r : Bool) :
    computeFrontierLeverageBps a g c f r ≤ 10000 := by
  unfold computeFrontierLeverageBps frontierLeverageCompute
  by_cases h0 : (g == 0) = true
  · simp [h0]
  · by_cases h1 : g ≤ 2500
    · cases f <;> simp [h0, h1, Nat.min_le_right]
    · simp [h0, h1, Nat.min_le_right]

theorem frontier_leverage_gated_external_only
    (a : Nat) (c : Nat) (f : Bool) (r : Bool) :
    computeFrontierLeverageBps a 0 c f r = 0 := by
  rfl

theorem frontier_leverage_gated_implemented_without_frontier
    (a : Nat) (g : Nat) (c : Nat) (r : Bool)
    (hImplemented : g ≤ 2500) :
    computeFrontierLeverageBps a g c false r = 0 := by
  unfold computeFrontierLeverageBps
  by_cases h0 : (g == 0) = true
  · simp [h0]
  · simp [h0, hImplemented]

private theorem min_mono_left {x y k : Nat} (h : x ≤ y) : Nat.min x k ≤ Nat.min y k := by
  simp only [Nat.min_def]
  split <;> split <;> omega

private theorem bool_bonus_mono {b1 b2 : Bool} (hb : b1 = true → b2 = true) :
    (if b1 then 10000 else 0) ≤ (if b2 then 10000 else 0) := by
  cases b1 <;> cases b2 <;> simp_all

theorem frontier_leverage_raw_numer_monotonic
    (a1 a2 g1 g2 c1 c2 : Nat)
    (f1 f2 r1 r2 : Bool)
    (ha : a1 ≤ a2)
    (hg : g1 ≤ g2)
    (hc : c1 ≤ c2)
    (hf : f1 = true → f2 = true)
    (hr : r1 = true → r2 = true) :
    frontierLeverageRawNumer a1 g1 c1 f1 r1 ≤
      frontierLeverageRawNumer a2 g2 c2 f2 r2 := by
  unfold frontierLeverageRawNumer
  have ha' : Nat.min a1 10000 ≤ Nat.min a2 10000 := min_mono_left ha
  have hg' : Nat.min g1 10000 ≤ Nat.min g2 10000 := min_mono_left hg
  have hc' : Nat.min c1 10 ≤ Nat.min c2 10 := min_mono_left hc
  have hf' : (if f1 then 10000 else 0) ≤ (if f2 then 10000 else 0) := bool_bonus_mono hf
  have hr' : (if r1 then 10000 else 0) ≤ (if r2 then 10000 else 0) := bool_bonus_mono hr
  omega

theorem frontier_leverage_compute_monotonic
    (a1 a2 g1 g2 c1 c2 : Nat)
    (f1 f2 r1 r2 : Bool)
    (ha : a1 ≤ a2)
    (hg : g1 ≤ g2)
    (hc : c1 ≤ c2)
    (hf : f1 = true → f2 = true)
    (hr : r1 = true → r2 = true) :
    frontierLeverageCompute a1 g1 c1 f1 r1 ≤
      frontierLeverageCompute a2 g2 c2 f2 r2 := by
  unfold frontierLeverageCompute
  have hNum := frontier_leverage_raw_numer_monotonic a1 a2 g1 g2 c1 c2 f1 f2 r1 r2 ha hg hc hf hr
  have hDiv :
      frontierLeverageRawNumer a1 g1 c1 f1 r1 / 100 ≤
        frontierLeverageRawNumer a2 g2 c2 f2 r2 / 100 := by
    omega
  exact min_mono_left hDiv

theorem frontier_leverage_monotonic_and_gated
    (a1 a2 g1 g2 c1 c2 : Nat)
    (f1 f2 r1 r2 : Bool)
    (ha : a1 ≤ a2)
    (hg : g1 ≤ g2)
    (hc : c1 ≤ c2)
    (hf : f1 = true → f2 = true)
    (hr : r1 = true → r2 = true) :
    computeFrontierLeverageBps a1 g1 c1 f1 r1 ≤
      computeFrontierLeverageBps a2 g2 c2 f2 r2 := by
  have hComp := frontier_leverage_compute_monotonic a1 a2 g1 g2 c1 c2 f1 f2 r1 r2 ha hg hc hf hr
  unfold computeFrontierLeverageBps
  by_cases hg1_0 : (g1 == 0) = true
  · simp [hg1_0]
  · have hg1_pos : g1 > 0 := by
      cases g1 with
      | zero => contradiction
      | succ n => omega
    have hg2_pos : g2 > 0 := by omega
    have hg2_0 : (g2 == 0) = false := by
      cases g2 with
      | zero => omega
      | succ n => rfl
    by_cases hg1_le : g1 ≤ 2500
    · cases f1 with
      | false =>
        simp [hg1_0, hg1_le]
      | true =>
        have hf2 : f2 = true := hf rfl
        subst hf2
        by_cases hg2_le : g2 ≤ 2500
        · simp [hg1_0, hg2_0, hg1_le, hg2_le, hComp]
        · simp [hg1_0, hg2_0, hg1_le, hg2_le, hComp]
    · have hg2_gt : ¬ (g2 ≤ 2500) := by omega
      simp [hg1_0, hg2_0, hg1_le, hg2_gt, hComp]

end NeuroStrata.Taxonomy
