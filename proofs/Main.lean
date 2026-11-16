import NeuroStrata

open NeuroStrata.TaskMachine
open NeuroStrata.SynapticMesh
open NeuroStrata.TaskClaim
open NeuroStrata.Sovereignty
open NeuroStrata.Procedure
open NeuroStrata.TwinCrdt
open NeuroStrata.Retention
open NeuroStrata.TaskGate
open NeuroStrata.KineticAction
open NeuroStrata.HybridSlots
open NeuroStrata.TwinInterface
open NeuroStrata
open NeuroStrata.Taxonomy
open NeuroStrata.WorkflowStage

def boolToStr (b : Bool) : String := if b then "true" else "false"

def main : IO Unit := do
  let statuses : List Status := [.openState, .inProgress, .blocked, .done]
  let lifecycles : List LifecycleState := [.active, .archived, .superseded, .tombstoned]
  for f in statuses do
    for t in statuses do
      for hasReason in [false, true] do
        for hasExtraction in [false, true] do
          let tag := applyTag f t hasReason hasExtraction
          IO.println s!"TASK_FSM|{f.asStr}|{t.asStr}|{boolToStr hasReason}|{boolToStr hasExtraction}|{tag}"
  for f in lifecycles do
    for t in lifecycles do
      IO.println s!"LIFECYCLE|{f.asStr}|{t.asStr}|{boolToStr (canTransitionTo f t)}"
  for l in lifecycles do
    for c in lifecycles do
      for sameSup in [false, true] do
        for contentChanged in [false, true] do
          for concurrent in [false, true] do
            let tag := classifyMerge l c sameSup contentChanged concurrent |>.asTag
            IO.println s!"MESH_MERGE|{l.asStr}|{c.asStr}|{boolToStr sameSup}|{boolToStr contentChanged}|{boolToStr concurrent}|{tag}"
  for (c, m) in [(1, 2), (2, 2), (3, 2)] do
    IO.println s!"EPOCH_GATE|{c}|{m}|{epochGate c m |>.asTag}"
  let ageCases : List (String × Option Int) := [("none", none), ("live", some 1800), ("stale", some 3601)]
  for s in statuses do
    for sameSession in [false, true] do
      for (ageLabel, ageVal) in ageCases do
        let tag := evaluateClaim s sameSession ageVal 3600 |>.asTag
        IO.println s!"TASK_CLAIM|{s.asStr}|{boolToStr sameSession}|{ageLabel}|{tag}"
  for perm in [false, true] do
    for shareable in [false, true] do
      for counter in [false, true] do
        for aclEmpty in [false, true] do
          for inAcl in [false, true] do
            let tag := evaluateSovereigntyGate perm shareable counter aclEmpty inAcl |>.asTag
            IO.println s!"SOVEREIGNTY_GATE|{boolToStr perm}|{boolToStr shareable}|{boolToStr counter}|{boolToStr aclEmpty}|{boolToStr inAcl}|{tag}"
  for rowExists in [false, true] do
    for hasProv in [false, true] do
      for sameUpd in [false, true] do
        let tag := evaluateImportPrecheck rowExists hasProv sameUpd |>.asTag
        IO.println s!"IMPORT_PRECHECK|{boolToStr rowExists}|{boolToStr hasProv}|{boolToStr sameUpd}|{tag}"
  let trusts : List ProcedureTrust := [.untrusted, .trusted, .promoted, .local, .absentOrUnknown]
  let lapsedCases : List (String × Option Int) := [("live", none), ("lapsed", some 1000)]
  let budgetCases : List (String × Option Nat) := [("unbounded", none), ("zero", some 0), ("two", some 2)]
  for tr in trusts do
    for imported in [false, true] do
      for (lapsedLabel, vt) in lapsedCases do
        for (budgetLabel, b) in budgetCases do
          let tag := evaluateProcedureFire tr imported vt 1000 b 0 |>.asTag
          IO.println s!"PROCEDURE_FIRE|{tr.asStr}|{boolToStr imported}|{lapsedLabel}|{budgetLabel}|{tag}"
  for a in statuses do
    for b in statuses do
      let joined := joinStatus a b
      IO.println s!"TWIN_CRDT_STATUS|{a.asStr}|{b.asStr}|{joined.asStr}"
  for alreadySeen in [false, true] do
    for seq in [1, 2, 3] do
      let tag := evaluateSeqContinuity alreadySeen 2 seq |>.asTag
      IO.println s!"TWIN_CRDT_SEQ|{boolToStr alreadySeen}|{seq}|{tag}"
  for budget in [false, true] do
    for nonEmpty in [false, true] do
      for found in [false, true] do
        for bound in [false, true] do
          for matchesWt in [false, true] do
            let tag := evaluateRetentionGate budget nonEmpty found bound matchesWt |>.asTag
            IO.println s!"RETENTION_GATE|{boolToStr budget}|{boolToStr nonEmpty}|{boolToStr found}|{boolToStr bound}|{boolToStr matchesWt}|{tag}"
  for isScoped in [false, true] do
    for callerId in [false, true] do
      for claimAttr in [false, true] do
        for branchMatch in [false, true] do
          for wtMatch in [false, true] do
            let tag := evaluateClaimScopeGate isScoped callerId claimAttr branchMatch wtMatch |>.asTag
            IO.println s!"CLAIM_SCOPE_GATE|{boolToStr isScoped}|{boolToStr callerId}|{boolToStr claimAttr}|{boolToStr branchMatch}|{boolToStr wtMatch}|{tag}"
  for principal in [false, true] do
    for params in [false, true] do
      for sparql in [false, true] do
        for twin in [false, true] do
          for shacl in [false, true] do
            let tag := evaluateActionGate principal params sparql twin shacl |>.asTag
            IO.println s!"ACTION_GATE|{boolToStr principal}|{boolToStr params}|{boolToStr sparql}|{boolToStr twin}|{boolToStr shacl}|{tag}"
  let receiptStates : List ActionReceiptState := [.active, .alreadyReverted]
  for rstate in receiptStates do
    for cas in [false, true] do
      for force in [false, true] do
        for ownerAdmin in [false, true] do
          for editor in [false, true] do
            let tag := evaluateActionRevert rstate cas force ownerAdmin editor |>.asTag
            IO.println s!"ACTION_REVERT|{rstate.asStr}|{boolToStr cas}|{boolToStr force}|{boolToStr ownerAdmin}|{boolToStr editor}|{tag}"
  let targetStatuses : List ActionTaskTargetStatus := [.openState, .inProgress, .blocked, .done]
  for curr in statuses do
    for req in targetStatuses do
      let tag := actionTaskEffectTag (evaluateActionTaskEffect curr req)
      IO.println s!"ACTION_TASK_EFFECT|{curr.asStr}|{req.asStr}|{tag}"
  let kinds : List ActionMemoryKind := [.actionType, .actionReceipt]
  for kind in kinds do
    for explicit in [false, true] do
      let shareable := isActionMemoryShareable kind explicit
      IO.println s!"ACTION_SHAREABLE|{kind.asStr}|{boolToStr explicit}|{boolToStr shareable}"
  for limit in [2, 4, 5, 8] do
    for availNorm in [0, 2, 4] do
      for availExp in [0, 1, 3] do
        for availOther in [0, 3] do
          let alloc := allocateHybridSlots limit availNorm availExp availOther
          IO.println s!"HYBRID_SLOTS|{limit}|{availNorm}|{availExp}|{availOther}|{alloc.normativeTaken}|{alloc.expansionTaken}|{alloc.remainingTaken}"
  let boundCases : List (Int × Int × Int × Int) :=
    [(10, 0, 0, 100), (0, 100, 20, 10), (0, 10, 20, 30), (-50, 200, 0, 120), (0, 100, -10, 80)]
  for (imin, imax, tmin, tmax) in boundCases do
    let tag := formatBoundsOutcome (intersectVarBounds imin imax tmin tmax)
    IO.println s!"TWIN_BOUNDS|{imin}|{imax}|{tmin}|{tmax}|{tag}"
  let reducerOps : List ReducerOpKind := [.count, .sum, .min, .max, .avg, .allInBounds, .anyOutOfBounds]
  let reducerInputs : List (String × Nat × Int × Int × Int × Bool) :=
    [("empty", 0, 0, 0, 0, false), ("populated_ok", 3, 45, 10, 20, false), ("populated_oob", 3, 45, 10, 20, true)]
  for op in reducerOps do
    for (label, cnt, sumV, minV, maxV, oob) in reducerInputs do
      let out := evaluateReducerTotal op cnt sumV minV maxV oob
      IO.println s!"TWIN_REDUCER|{op.asStr}|{label}|{out.numericVal}|{boolToStr out.boolVal}|{boolToStr out.isDefined}"
  for selfCall in [false, true] do
    for sameFile in [false, true] do
      for imported in [false, true] do
        for wsCount in [0, 1, 2] do
          for identLen in [3, 6] do
            for isStd in [false, true] do
              let tag := resolveCrossFileCall selfCall sameFile imported wsCount identLen isStd |>.asTag
              IO.println s!"CROSS_FILE_CALL|{boolToStr selfCall}|{boolToStr sameFile}|{boolToStr imported}|{wsCount}|{identLen}|{boolToStr isStd}|{tag}"
  for livePresent in [false, true] do
    for stagedRetired in [false, true] do
      for stagedUpsert in [false, true] do
        let tag := resolveScenarioRow livePresent stagedRetired stagedUpsert |>.asTag
        IO.println s!"SCENARIO_ROW|{boolToStr livePresent}|{boolToStr stagedRetired}|{boolToStr stagedUpsert}|{tag}"
  for expired in [false, true] do
    for casMatch in [false, true] do
      for rebacOk in [false, true] do
        for enforceShacl in [false, true] do
          for shaclViolations in [0, 1, 2] do
            let tag := evaluateScenarioCommit expired casMatch rebacOk enforceShacl shaclViolations |>.asTag
            IO.println s!"SCENARIO_COMMIT|{boolToStr expired}|{boolToStr casMatch}|{boolToStr rebacOk}|{boolToStr enforceShacl}|{shaclViolations}|{tag}"
  for isTestOrDoc in [false, true] do
    for isFoundation in [false, true] do
      for isEntrypoint in [false, true] do
        for inDeg in [0, 1, 2] do
          for outDeg in [0, 1, 2] do
            let cat := classifyTourStop isTestOrDoc isFoundation isEntrypoint inDeg outDeg
            IO.println s!"DIFF_TOUR|{boolToStr isTestOrDoc}|{boolToStr isFoundation}|{boolToStr isEntrypoint}|{inDeg}|{outDeg}|{cat.asTag}|{cat.stopNumber}"
  for qNonEmpty in [false, true] do
    for isBuiltin in [false, true] do
      for isExisting in [false, true] do
        for existingCount in [0, 15, 16, 17] do
          let dec := evaluateInsightDefine qNonEmpty isBuiltin isExisting existingCount 16
          IO.println s!"INSIGHT_DEFINE|{boolToStr qNonEmpty}|{boolToStr isBuiltin}|{boolToStr isExisting}|{existingCount}|{dec.asTag}"
  for vf in ([10, 20] : List Int) do
    for hasVt in [false, true] do
      for vt in ([15, 25] : List Int) do
        for asOf in ([5, 15, 20, 25] : List Int) do
          let valid := isMemoryValidAt vf hasVt vt asOf
          IO.println s!"INSIGHT_VALID_AT|{vf}|{boolToStr hasVt}|{vt}|{asOf}|{boolToStr valid}"
  for d in [false, true] do
    for t in [false, true] do
      for s in [false, true] do
        for g in [false, true] do
          let st := classifyEpistemicStatus d t s g |>.asStr
          IO.println s!"TAXONOMY_CLASSIFY|{boolToStr d}|{boolToStr t}|{boolToStr s}|{boolToStr g}|{st}"
  let dagCases : List (String × List (Nat × Nat)) := [("empty", []), ("single_edge", [(1, 2)]), ("self_loop", [(1, 1)]), ("two_cycle", [(1, 2), (2, 1)]), ("three_cycle", [(1, 2), (2, 3), (3, 1)]), ("diamond", [(1, 2), (1, 3), (2, 4), (3, 4)]), ("four_cycle", [(1, 2), (2, 3), (3, 4), (4, 1)]), ("disconnected_cycle", [(1, 2), (3, 4), (4, 5), (5, 3)]), ("multi_parent_dag", [(1, 3), (2, 3), (3, 4), (1, 4)])]
  for (label, edges) in dagCases do
    let dagValid := validateTaxonomyDag edges
    IO.println s!"TAXONOMY_DAG|{label}|{boolToStr dagValid}"
  let candidate3Edges : List (Nat × Nat) := [(0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1)]
  for mask in List.range 64 do
    let edges := (List.range 6).filterMap (fun bit =>
      if (mask / (2 ^ bit)) % 2 == 1 then candidate3Edges[bit]? else none)
    let dagValid := validateTaxonomyDag edges
    IO.println s!"TAXONOMY_DAG_3NODE|{mask}|{boolToStr dagValid}"
  let skosBroader : List (Nat × Nat) := [(0, 1), (1, 2)]
  let skosCases : List (String × List (Nat × Nat) × List (Nat × Nat)) := [("related_direct", [(0, 1)], []), ("related_trans", [(0, 2)], []), ("related_reverse", [(1, 0)], []), ("related_reverse_trans", [(2, 0)], []), ("related_self", [(0, 0)], []), ("contrast_trans", [], [(0, 2)]), ("contrast_reverse", [], [(2, 1)]), ("contrast_self", [], [(1, 1)]), ("unrelated", [(0, 5)], []), ("empty_all", [], [])]
  for (label, rel, con) in skosCases do
    let skosValid := validateSkosDisjointness skosBroader rel con
    IO.println s!"TAXONOMY_SKOS|{label}|{boolToStr skosValid}"
  let cyclicBroader : List (Nat × Nat) := [(0, 1), (1, 0)]
  IO.println s!"TAXONOMY_SKOS|cyclic_broader_empty|{boolToStr (validateSkosDisjointness cyclicBroader [] [])}"
  IO.println s!"TAXONOMY_SKOS|cyclic_broader_unrelated|{boolToStr (validateSkosDisjointness cyclicBroader [(2, 3)] [])}"
  let coverageCases : List (String × List EpistemicStatus × Nat) := [("empty_zero", [], 0), ("seed", [.seed], 0), ("verified", [.verified], 0), ("seed_verified", [.seed, .verified], 0), ("seed_verified_unmapped", [.seed, .verified], 1), ("four_seed", [.seed, .seed, .seed, .seed], 0), ("mixed", [.seed, .defined, .synthesized, .verified], 0)]
  for (label, statuses, unmapped) in coverageCases do
    IO.println s!"TAXONOMY_COVERAGE|{label}|{computeEpistemicCoverageBps statuses unmapped}"
  let realizationCases : List String := ["unimplemented", "partial", "unknown", "implemented", "external_only", "other"]
  for status in realizationCases do
    IO.println s!"TAXONOMY_REALIZATION|{status}|{realizationStatusGapBps status}"
  let frontierCases : List (String × Nat × Nat × Nat × Bool × Bool) := [("zero_gap", 5000, 0, 3, true, true), ("low_gap_no_frontier", 5000, 2000, 3, false, true), ("low_gap_frontier", 5000, 2000, 3, true, true), ("full", 10000, 10000, 10, true, true), ("capped", 20000, 20000, 20, true, true), ("partial", 3000, 5000, 2, false, false)]
  for (label, axis, gap, conn, frontier, cite) in frontierCases do
    IO.println s!"TAXONOMY_FRONTIER|{label}|{computeFrontierLeverageBps axis gap conn frontier cite}"
  let wfStages : List WorkflowStage := [.unpreflighted, .contextLoaded, .claimed, .mutating, .breathRequired, .preCheckpoint, .prePublish, .completed]
  let wfEvents : List (String × WorkflowEvent) :=
    [ ("load_context", .loadContext)
    , ("claim_ok", .claimTask true)
    , ("claim_bad_scope", .claimTask false)
    , ("mutate_under_limit", .mutate 1 5)
    , ("mutate_at_limit", .mutate 5 5)
    , ("record_breath", .recordBreathNote)
    , ("pre_checkpoint_ok", .triggerPreCheckpoint true)
    , ("pre_checkpoint_no_receipt", .triggerPreCheckpoint false)
    , ("checkpoint_passed", .checkpointPassed)
    , ("pre_publish_ok", .triggerPrePublish true true)
    , ("pre_publish_bad_doc", .triggerPrePublish true false)
    , ("pre_publish_bad_task", .triggerPrePublish false true)
    , ("pre_publish_both_bad", .triggerPrePublish false false)
    , ("publish_passed", .publishPassed)
    , ("complete_ok", .completeTask true 0)
    , ("complete_no_mem", .completeTask false 0)
    , ("complete_dirty", .completeTask true 1)
    , ("complete_no_mem_dirty", .completeTask false 1)
    ]
  for st in wfStages do
    for (evLabel, ev) in wfEvents do
      IO.println s!"WORKFLOW_STAGE|{st.asStr}|{evLabel}|{workflowTransitionTag st ev}"
