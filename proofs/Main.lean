import NeuroStrata

open NeuroStrata.TaskMachine
open NeuroStrata.SynapticMesh
open NeuroStrata.TaskClaim
open NeuroStrata.Sovereignty
open NeuroStrata.Procedure
open NeuroStrata.TwinCrdt
open NeuroStrata.Retention

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
