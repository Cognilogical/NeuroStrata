namespace NeuroStrata.SynapticMesh

inductive LifecycleState where
  | active
  | archived
  | superseded
  | tombstoned
  deriving DecidableEq, Repr

def LifecycleState.asStr : LifecycleState → String
  | .active => "active"
  | .archived => "archived"
  | .superseded => "superseded"
  | .tombstoned => "tombstoned"

@[simp]
def isTerminal : LifecycleState → Bool
  | .tombstoned => true
  | _ => false

@[simp]
def canTransitionTo (fromState toState : LifecycleState) : Bool :=
  match fromState, toState with
  | .active, .archived => true
  | .archived, .active => true
  | .active, .superseded => true
  | .archived, .superseded => true
  | .active, .tombstoned => true
  | .archived, .tombstoned => true
  | .superseded, .tombstoned => true
  | _, _ => false

inductive EpochDecision where
  | apply
  | quarantineOld
  | refuseFuture
  deriving DecidableEq, Repr

def EpochDecision.asTag : EpochDecision → String
  | .apply => "apply"
  | .quarantineOld => "quarantine_old"
  | .refuseFuture => "refuse_future"

@[simp]
def epochGate (candidateEpoch manifestEpoch : Nat) : EpochDecision :=
  if candidateEpoch == manifestEpoch then .apply
  else if candidateEpoch < manifestEpoch then .quarantineOld
  else .refuseFuture

inductive MergeDecision where
  | apply
  | applyCombinationArchiveEdit
  | skip
  | refuseTombstoneWins
  | refuseStaleRestore
  | refuseIllegalTransition
  | conflictTwoReplacements
  | conflictRestoreArchive
  | conflictIncompatibleEdits
  | supersedeWinsPreserveEdit
  deriving DecidableEq, Repr

def MergeDecision.asTag : MergeDecision → String
  | .apply => "apply"
  | .applyCombinationArchiveEdit => "apply_combination_archive_edit"
  | .skip => "skip"
  | .refuseTombstoneWins => "refuse_tombstone_wins"
  | .refuseStaleRestore => "refuse_stale_restore"
  | .refuseIllegalTransition => "refuse_illegal_transition"
  | .conflictTwoReplacements => "conflict_two_replacements"
  | .conflictRestoreArchive => "conflict_restore_archive"
  | .conflictIncompatibleEdits => "conflict_incompatible_edits"
  | .supersedeWinsPreserveEdit => "supersede_wins_preserve_edit"

@[simp]
def classifyMerge (localState candState : LifecycleState) (sameSupersededBy contentChanged concurrent : Bool) : MergeDecision :=
  match localState, candState with
  | .tombstoned, .tombstoned => .skip
  | .tombstoned, .archived => .refuseTombstoneWins
  | .tombstoned, .superseded => .refuseTombstoneWins
  | .tombstoned, .active => .refuseTombstoneWins
  | .archived, .tombstoned => .apply
  | .superseded, .tombstoned => .apply
  | .active, .tombstoned => .apply
  | .superseded, .superseded =>
      if sameSupersededBy then .skip else .conflictTwoReplacements
  | .archived, .superseded =>
      if contentChanged then .supersedeWinsPreserveEdit else .apply
  | .superseded, .active =>
      if contentChanged then .supersedeWinsPreserveEdit else .refuseStaleRestore
  | .superseded, .archived =>
      if contentChanged then .supersedeWinsPreserveEdit else .refuseStaleRestore
  | .active, .superseded =>
      if contentChanged then .supersedeWinsPreserveEdit else .apply
  | .archived, .active =>
      if contentChanged then
        if canTransitionTo localState candState || canTransitionTo candState localState
        then .applyCombinationArchiveEdit else .refuseIllegalTransition
      else
        if concurrent then .conflictRestoreArchive
        else if canTransitionTo localState candState then .apply else .refuseIllegalTransition
  | .active, .archived =>
      if contentChanged then
        if canTransitionTo localState candState || canTransitionTo candState localState
        then .applyCombinationArchiveEdit else .refuseIllegalTransition
      else
        if concurrent then .conflictRestoreArchive
        else if canTransitionTo localState candState then .apply else .refuseIllegalTransition
  | .active, .active =>
      if contentChanged then
        if concurrent then .conflictIncompatibleEdits else .apply
      else .skip
  | .archived, .archived =>
      if contentChanged then
        if concurrent then .conflictIncompatibleEdits else .apply
      else .skip

inductive DispositionClass where
  | converged
  | conflictPreserved
  | supersedeWinsPreserveEdit
  | refused
  deriving DecidableEq, Repr

structure ResolutionOutcome where
  effectiveState : LifecycleState
  disposition : DispositionClass
  deriving DecidableEq, Repr

@[simp]
def resolveOutcome (localState candState : LifecycleState) (d : MergeDecision) : ResolutionOutcome :=
  match d with
  | .apply => ⟨candState, .converged⟩
  | .applyCombinationArchiveEdit => ⟨.archived, .converged⟩
  | .skip => ⟨localState, .converged⟩
  | .refuseTombstoneWins => ⟨.tombstoned, .converged⟩
  | .refuseStaleRestore => ⟨localState, .converged⟩
  | .refuseIllegalTransition => ⟨localState, .refused⟩
  | .conflictTwoReplacements => ⟨.superseded, .conflictPreserved⟩
  | .conflictRestoreArchive => ⟨.archived, .conflictPreserved⟩
  | .conflictIncompatibleEdits => ⟨localState, .conflictPreserved⟩
  | .supersedeWinsPreserveEdit => ⟨.superseded, .supersedeWinsPreserveEdit⟩

theorem tombstone_absorbing : ∀ (target : LifecycleState), canTransitionTo .tombstoned target = false := by
  intro target
  cases target <;> decide

theorem superseded_never_reactivates : canTransitionTo .superseded .active = false ∧ canTransitionTo .superseded .archived = false := by
  decide

theorem tombstone_dominates_merge : ∀ (other : LifecycleState) (sameSup contentChanged concurrent : Bool),
    resolveOutcome .tombstoned other (classifyMerge .tombstoned other sameSup contentChanged concurrent) = ⟨.tombstoned, .converged⟩ ∧
    resolveOutcome other .tombstoned (classifyMerge other .tombstoned sameSup contentChanged concurrent) = ⟨.tombstoned, .converged⟩ := by
  intro other s c k
  cases other <;> cases s <;> cases c <;> cases k <;> decide

theorem no_silent_edit_drop_on_supersede : ∀ (sameSup concurrent : Bool),
    classifyMerge .superseded .active sameSup true concurrent = .supersedeWinsPreserveEdit ∧
    classifyMerge .active .superseded sameSup true concurrent = .supersedeWinsPreserveEdit ∧
    classifyMerge .superseded .archived sameSup true concurrent = .supersedeWinsPreserveEdit ∧
    classifyMerge .archived .superseded sameSup true concurrent = .supersedeWinsPreserveEdit := by
  intro s k
  cases s <;> cases k <;> decide

theorem archive_edit_commutativity : ∀ (sameSup : Bool),
    classifyMerge .archived .active sameSup true true = .applyCombinationArchiveEdit ∧
    classifyMerge .active .archived sameSup true true = .applyCombinationArchiveEdit := by
  intro s
  cases s <;> decide

theorem classifyMerge_concurrent_symmetry : ∀ (a b : LifecycleState) (sameSup contentChanged : Bool),
    resolveOutcome a b (classifyMerge a b sameSup contentChanged true) = resolveOutcome b a (classifyMerge b a sameSup contentChanged true) := by
  intro a b s c
  cases a <;> cases b <;> cases s <;> cases c <;> rfl

theorem epochGate_trichotomy : ∀ (c m : Nat),
    (c = m ∧ epochGate c m = .apply) ∨ (c < m ∧ epochGate c m = .quarantineOld) ∨ (m < c ∧ epochGate c m = .refuseFuture) := by
  intro c m
  dsimp [epochGate]
  by_cases h1 : c = m
  · left
    simp [h1]
  · by_cases h2 : c < m
    · right
      left
      simp [h1, h2]
    · right
      right
      have h3 : m < c := by omega
      simp [h1, h2, h3]

theorem supersede_archived_without_edit_behavior : ∀ (sameSup concurrent : Bool),
    classifyMerge .superseded .archived sameSup false concurrent = .refuseStaleRestore ∧
    classifyMerge .archived .superseded sameSup false concurrent = .apply := by
  intro s k
  cases s <;> cases k <;> decide

end NeuroStrata.SynapticMesh
