import NeuroStrata

open NeuroStrata.TaskMachine
open NeuroStrata.SynapticMesh

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
