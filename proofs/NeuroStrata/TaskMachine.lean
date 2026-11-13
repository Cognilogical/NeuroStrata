namespace NeuroStrata.TaskMachine

inductive Status where
  | openState
  | inProgress
  | blocked
  | done
  deriving DecidableEq, Repr

def Status.asStr : Status → String
  | .openState => "open"
  | .inProgress => "in_progress"
  | .blocked => "blocked"
  | .done => "done"

inductive Guard where
  | none
  | requireReason
  | extractionRequired
  deriving DecidableEq, Repr

structure Transition where
  fromState : Status
  toState : Status
  guard : Guard
  deriving DecidableEq, Repr

def table : List Transition :=
  [ ⟨.openState, .inProgress, .none⟩
  , ⟨.openState, .blocked, .requireReason⟩
  , ⟨.openState, .done, .extractionRequired⟩
  , ⟨.inProgress, .openState, .none⟩
  , ⟨.inProgress, .blocked, .requireReason⟩
  , ⟨.inProgress, .done, .extractionRequired⟩
  , ⟨.blocked, .openState, .requireReason⟩
  , ⟨.done, .openState, .requireReason⟩
  ]

structure Ctx where
  hasReason : Bool
  extractionEdgeExists : Bool
  deriving DecidableEq, Repr

structure TaskState where
  status : Status
  hasAssignee : Bool
  hasSession : Bool
  deriving DecidableEq, Repr

inductive TransitionError where
  | illegal (fromState toState : Status) (legal : List Status)
  | missingReason (fromState toState : Status)
  | extractionRequired
  deriving DecidableEq, Repr

@[simp]
def legalTargets (s : Status) : List Status :=
  match s with
  | .openState => [.inProgress, .blocked, .done]
  | .inProgress => [.openState, .blocked, .done]
  | .blocked => [.openState]
  | .done => [.openState]

@[simp]
def edgeGuard (f : Status) (to : Status) : Option Guard :=
  match (f, to) with
  | (.openState, .inProgress) => some .none
  | (.openState, .blocked) => some .requireReason
  | (.openState, .done) => some .extractionRequired
  | (.inProgress, .openState) => some .none
  | (.inProgress, .blocked) => some .requireReason
  | (.inProgress, .done) => some .extractionRequired
  | (.blocked, .openState) => some .requireReason
  | (.done, .openState) => some .requireReason
  | _ => none

@[simp]
def nextState (task : TaskState) (toState : Status) : TaskState :=
  if task.status = .inProgress ∧ toState = .openState then
    { task with status := toState, hasAssignee := false, hasSession := false }
  else
    { task with status := toState }

@[simp]
def apply (task : TaskState) (toState : Status) (ctx : Ctx) : Except TransitionError TaskState :=
  match edgeGuard task.status toState with
  | none => .error (.illegal task.status toState (legalTargets task.status))
  | some .none =>
      .ok (nextState task toState)
  | some .requireReason =>
      if ctx.hasReason then .ok (nextState task toState)
      else .error (.missingReason task.status toState)
  | some .extractionRequired =>
      if ctx.extractionEdgeExists then .ok (nextState task toState)
      else .error .extractionRequired

def applyTag (f : Status) (toState : Status) (hasReason : Bool) (extractionEdgeExists : Bool) : String :=
  match apply ⟨f, true, true⟩ toState ⟨hasReason, extractionEdgeExists⟩ with
  | .ok _ => "ok"
  | .error (.illegal _ _ _) => "err_illegal"
  | .error (.missingReason _ _) => "err_missing_reason"
  | .error .extractionRequired => "err_extraction_required"

theorem nextState_status (t : TaskState) (toState : Status) : (nextState t toState).status = toState := by
  dsimp [nextState]
  split <;> rfl

theorem apply_ok_eq (t : TaskState) (toState : Status) (ctx : Ctx) (t' : TaskState)
    (h : apply t toState ctx = .ok t') : t' = nextState t toState := by
  rcases t with ⟨st, a, b⟩
  rcases ctx with ⟨hasReason, extractionEdgeExists⟩
  rcases edgeGuard st toState with
  | none =>
    simp_all [edgeGuard, nextState]
    <;> (try { contradiction }) <;> (try { simp_all })
  | some .none =>
    simp_all [edgeGuard, nextState]
    <;> (try { contradiction }) <;> (try { simp_all })
  | some .requireReason =>
    simp_all [edgeGuard, nextState]
    <;> (try { contradiction }) <;> (try { simp_all })
  | some .extractionRequired =>
    simp_all [edgeGuard, nextState]
    <;> (try { contradiction }) <;> (try { simp_all })

theorem no_self_transitions : ∀ (t : TaskState) (ctx : Ctx),
    ∃ legal, apply t t.status ctx = .error (.illegal t.status t.status legal) := by
  intro t ctx
  rcases t with ⟨st, a, b⟩
  rcases ctx with ⟨hasReason, extractionEdgeExists⟩
  refine ⟨legalTargets st, _⟩
  cases st <;> cases hasReason <;> cases extractionEdgeExists
  simp [apply, edgeGuard, legalTargets] <;> rfl

theorem lock2_soundness : ∀ (t : TaskState) (toState : Status) (ctx : Ctx) (t' : TaskState),
    apply t toState ctx = .ok t' → t'.status = .done →
    ctx.extractionEdgeExists = true ∧ (t.status = .openState ∨ t.status = .inProgress) := by
  intro t toState ctx t' hok hdone
  have hnext : t' = nextState t toState := apply_ok_eq t toState ctx t' hok
  rw [hnext] at hdone
  have hts : toState = .done := by
    have h1 : (nextState t toState).status = toState := nextState_status t toState
    rw [h1] at hdone
    exact hdone
  subst hts
  rcases t with ⟨st, a, b⟩
  rcases ctx with ⟨hasReason, extractionEdgeExists⟩
  cases st <;> cases toState <;> cases hasReason <;> cases extractionEdgeExists
  simp [apply, edgeGuard, nextState] at hok ⊢
  <;>
  (try { contradiction }) <;>
  (try { simp_all })

theorem done_only_reopens_to_open_with_reason : ∀ (t : TaskState) (toState : Status) (ctx : Ctx) (t' : TaskState),
    t.status = .done → apply t toState ctx = .ok t' →
    toState = .openState ∧ ctx.hasReason = true ∧ t'.hasAssignee = t.hasAssignee ∧ t'.hasSession = t.hasSession := by
  intro t toState ctx t' ht hok
  have hnext : t' = nextState t toState := apply_ok_eq t toState ctx t' hok
  rw [hnext] at *
  rcases t with ⟨st, a, b⟩
  rcases ctx with ⟨hasReason, extractionEdgeExists⟩
  simp at ht ⊢
  subst ht
  cases toState <;> cases hasReason <;> cases extractionEdgeExists
  simp [apply, edgeGuard, nextState] at hok ⊢
  <;>
  (try { contradiction }) <;>
  (try { simp_all }) <;>
  (try { refine ⟨rfl, ?_, ?_, ?_⟩ <;> simp_all })

theorem block_requires_reason : ∀ (t : TaskState) (ctx : Ctx) (t' : TaskState),
    apply t .blocked ctx = .ok t' →
    ctx.hasReason = true ∧ (t.status = .openState ∨ t.status = .inProgress) := by
  intro t ctx t' hok
  have hnext : t' = nextState t .blocked := apply_ok_eq t .blocked ctx t' hok
  rw [hnext] at *
  rcases t with ⟨st, a, b⟩
  rcases ctx with ⟨hasReason, extractionEdgeExists⟩
  cases st <;> cases hasReason <;> cases extractionEdgeExists
  simp [apply, edgeGuard, nextState] at hok ⊢
  <;>
  (try { contradiction }) <;>
  (try { simp_all }) <;>
  (try { refine ⟨by assumption, ?_⟩ <;> simp_all })

theorem unclaim_clears_claim_exclusively : ∀ (t : TaskState) (toState : Status) (ctx : Ctx) (t' : TaskState),
    apply t toState ctx = .ok t' →
    (t.status = .inProgress ∧ toState = .openState → t'.hasAssignee = false ∧ t'.hasSession = false) ∧
    (¬(t.status = .inProgress ∧ toState = .openState) → t'.hasAssignee = t.hasAssignee ∧ t'.hasSession = t.hasSession) := by
  intro t toState ctx t' hok
  have hnext : t' = nextState t toState := apply_ok_eq t toState ctx t' hok
  subst hnext
  rcases t with ⟨st, a, b⟩
  rcases ctx with ⟨hasReason, extractionEdgeExists⟩
  cases st <;> cases toState <;> cases hasReason <;> cases extractionEdgeExists
  dsimp [nextState] at *
  split_ifs at * <;>
  (try { simp_all }) <;>
  (try { contradiction }) <;>
  (try { constructor <;> intro h <;> simp_all }) <;>
  (try { tauto })

end NeuroStrata.TaskMachine