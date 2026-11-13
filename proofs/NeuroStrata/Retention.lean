namespace NeuroStrata.Retention

inductive RetentionVerdict where
  | retained
  | failedOverBudget
  | failedStoreEmpty
  | failedReceiptMissing
  | failedWorktreeMoved
  deriving DecidableEq, Repr

def RetentionVerdict.asTag : RetentionVerdict → String
  | .retained => "retained"
  | .failedOverBudget => "failed_over_budget"
  | .failedStoreEmpty => "failed_store_empty"
  | .failedReceiptMissing => "failed_receipt_missing"
  | .failedWorktreeMoved => "failed_worktree_moved"

@[simp]
def evaluateRetentionGate
    (withinLatencyBudget storeNonEmpty receiptFound worktreeBound worktreeMatches : Bool)
    : RetentionVerdict :=
  if !withinLatencyBudget then .failedOverBudget
  else if !storeNonEmpty then .failedStoreEmpty
  else if !receiptFound then .failedReceiptMissing
  else if worktreeBound && !worktreeMatches then .failedWorktreeMoved
  else .retained

theorem over_budget_always_rejects :
    ∀ (ne rf wb wm : Bool),
      evaluateRetentionGate false ne rf wb wm = .failedOverBudget := by
  intro ne rf wb wm
  rfl

theorem missing_receipt_never_passes :
    ∀ (wb wm : Bool),
      evaluateRetentionGate true true false wb wm = .failedReceiptMissing := by
  intro wb wm
  rfl

theorem moved_worktree_invalidates_bound_receipt :
    evaluateRetentionGate true true true true false = .failedWorktreeMoved := by
  rfl

theorem retained_iff_all_guards_hold :
    ∀ (b ne rf wb wm : Bool),
      evaluateRetentionGate b ne rf wb wm = .retained ↔
        (b = true ∧ ne = true ∧ rf = true ∧ (wb = false ∨ wm = true)) := by
  intro b ne rf wb wm
  cases b <;> cases ne <;> cases rf <;> cases wb <;> cases wm <;> decide

end NeuroStrata.Retention