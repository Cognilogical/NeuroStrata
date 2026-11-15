//! Aeneas-compliant pure Rust kernel for NeuroStrata formal verification.
//!
//! This module contains zero external dependencies and zero `crate::*` imports.
//! It compiles standalone via `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs`.
//! All types are `Copy` and all functions are `const fn` to enable Charon/Aeneas
//! translation to pure Lean 4 without borrow/heap complexity.

#![allow(dead_code)]

/// Task status in the task state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelTaskStatus {
    Open,
    InProgress,
    Blocked,
    Done,
}

impl KernelTaskStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            KernelTaskStatus::Open => "open",
            KernelTaskStatus::InProgress => "in_progress",
            KernelTaskStatus::Blocked => "blocked",
            KernelTaskStatus::Done => "done",
        }
    }
}

/// Guard required for a task transition edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelGuard {
    None,
    RequireReason,
    ExtractionRequired,
}

/// Outcome of a successful task step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelTaskStepOutcome {
    pub next_status: KernelTaskStatus,
    pub clear_claim: bool,
}

/// Error from a task step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelTaskStepError {
    Illegal,
    MissingReason,
    ExtractionRequired,
}

/// Returns the guard for a task transition edge, or `None` if the edge is illegal.
/// Matches the 8 legal edges from `src/task/machine.rs:67-76`.
pub const fn task_edge_guard(from: KernelTaskStatus, to: KernelTaskStatus) -> Option<KernelGuard> {
    use KernelTaskStatus::*;
    match (from, to) {
        (Open, InProgress) => Some(KernelGuard::None),
        (Open, Blocked) => Some(KernelGuard::RequireReason),
        (Open, Done) => Some(KernelGuard::ExtractionRequired),
        (InProgress, Open) => Some(KernelGuard::None),
        (InProgress, Blocked) => Some(KernelGuard::RequireReason),
        (InProgress, Done) => Some(KernelGuard::ExtractionRequired),
        (Blocked, Open) => Some(KernelGuard::RequireReason),
        (Done, Open) => Some(KernelGuard::RequireReason),
        _ => None,
    }
}

/// Executes a task step, returning the outcome or an error.
/// `clear_claim` is true exactly for the InProgress -> Open edge.
pub const fn task_step(
    from: KernelTaskStatus,
    to: KernelTaskStatus,
    has_reason: bool,
    extraction_edge_exists: bool,
) -> Result<KernelTaskStepOutcome, KernelTaskStepError> {
    use KernelTaskStatus::*;
    use KernelTaskStepError::*;

    let guard = match task_edge_guard(from, to) {
        Some(g) => g,
        None => return Err(Illegal),
    };

    match guard {
        KernelGuard::None => {}
        KernelGuard::RequireReason if !has_reason => return Err(MissingReason),
        KernelGuard::ExtractionRequired if !extraction_edge_exists => {
            return Err(KernelTaskStepError::ExtractionRequired)
        }
        KernelGuard::RequireReason | KernelGuard::ExtractionRequired => {}
    }

    let clear_claim = matches!((from, to), (InProgress, Open));
    Ok(KernelTaskStepOutcome {
        next_status: to,
        clear_claim,
    })
}

/// Tag string for a task step result: "ok", "err_illegal", "err_missing_reason", "err_extraction_required".
pub const fn task_step_tag(
    from: KernelTaskStatus,
    to: KernelTaskStatus,
    has_reason: bool,
    extraction_edge_exists: bool,
) -> &'static str {
    match task_step(from, to, has_reason, extraction_edge_exists) {
        Ok(_) => "ok",
        Err(KernelTaskStepError::Illegal) => "err_illegal",
        Err(KernelTaskStepError::MissingReason) => "err_missing_reason",
        Err(KernelTaskStepError::ExtractionRequired) => "err_extraction_required",
    }
}

/// Returns true if a row is retired: `valid_to` is Some and <= `now`.
/// `valid_to == now` is retired (inclusive). `None` is never retired.
pub const fn is_retired_at(valid_to: Option<i64>, now: i64) -> bool {
    match valid_to {
        Some(vt) => vt <= now,
        None => false,
    }
}

/// Lifecycle state in the SynapticMesh state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLifecycleState {
    Active,
    Archived,
    Superseded,
    Tombstoned,
}

impl KernelLifecycleState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            KernelLifecycleState::Active => "active",
            KernelLifecycleState::Archived => "archived",
            KernelLifecycleState::Superseded => "superseded",
            KernelLifecycleState::Tombstoned => "tombstoned",
        }
    }
}

/// Returns true if `from -> to` is an allowed lifecycle transition.
/// Matches the 7 allowed edges from `src/synaptic_mesh/record.rs:159-172`.
pub const fn lifecycle_can_transition(
    from: KernelLifecycleState,
    to: KernelLifecycleState,
) -> bool {
    use KernelLifecycleState::*;
    matches!(
        (from, to),
        (Active, Archived)
            | (Archived, Active)
            | (Active, Superseded)
            | (Archived, Superseded)
            | (Active, Tombstoned)
            | (Archived, Tombstoned)
            | (Superseded, Tombstoned)
    )
}

/// Returns true if the state is terminal (no forward transitions except tombstoning).
/// Only `Tombstoned` is terminal.
pub const fn lifecycle_is_terminal(state: KernelLifecycleState) -> bool {
    matches!(state, KernelLifecycleState::Tombstoned)
}

/// Decision from the epoch gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelEpochDecision {
    Apply,
    QuarantineOld,
    RefuseFuture,
}

/// Epoch gate decision: compares candidate_epoch against manifest_epoch.
pub const fn epoch_gate_decision(candidate_epoch: u32, manifest_epoch: u32) -> KernelEpochDecision {
    if candidate_epoch == manifest_epoch {
        KernelEpochDecision::Apply
    } else if candidate_epoch < manifest_epoch {
        KernelEpochDecision::QuarantineOld
    } else {
        KernelEpochDecision::RefuseFuture
    }
}

/// Tag string for epoch gate decision: "apply", "quarantine_old", "refuse_future".
pub const fn epoch_gate_tag(candidate_epoch: u32, manifest_epoch: u32) -> &'static str {
    match epoch_gate_decision(candidate_epoch, manifest_epoch) {
        KernelEpochDecision::Apply => "apply",
        KernelEpochDecision::QuarantineOld => "quarantine_old",
        KernelEpochDecision::RefuseFuture => "refuse_future",
    }
}

/// Merge decision from the SynapticMesh concurrent merge policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelMergeDecision {
    Apply,
    ApplyCombinationArchiveEdit,
    Skip,
    RefuseTombstoneWins,
    RefuseStaleRestore,
    RefuseIllegalTransition,
    ConflictTwoReplacements,
    ConflictRestoreArchive,
    ConflictIncompatibleEdits,
    SupersedeWinsPreserveEdit,
}

/// Classifies a merge decision using the pure algebraic rules.
/// Parameters mirror `src/synaptic_mesh/import.rs:classify_merge_parity_tag`:
/// - `local_state`: the current row's lifecycle state
/// - `cand_state`: the candidate record's lifecycle state
/// - `same_superseded_by`: whether both rows reference the same superseded_by target
/// - `content_changed`: whether the content differs between local and candidate
/// - `concurrent`: whether the merge is on a divergent ancestry (full-tree path)
pub const fn classify_merge_decision(
    local_state: KernelLifecycleState,
    cand_state: KernelLifecycleState,
    same_superseded_by: bool,
    content_changed: bool,
    concurrent: bool,
) -> KernelMergeDecision {
    use KernelLifecycleState::*;
    use KernelMergeDecision::*;

    // Terminal (Tombstoned) local state: deletion wins.
    if lifecycle_is_terminal(local_state) {
        return match cand_state {
            Tombstoned => Skip,
            _ => RefuseTombstoneWins,
        };
    }

    // Local is Superseded (retired).
    if matches!(local_state, Superseded) {
        return match cand_state {
            Tombstoned => Apply,
            Superseded => {
                if same_superseded_by {
                    Skip
                } else {
                    ConflictTwoReplacements
                }
            }
            Active | Archived => {
                if content_changed {
                    SupersedeWinsPreserveEdit
                } else {
                    RefuseStaleRestore
                }
            }
        };
    }

    // Incoming tombstone always wins over a live row.
    if matches!(cand_state, Tombstoned) {
        return Apply;
    }

    // Incoming supersession.
    if matches!(cand_state, Superseded) {
        return match local_state {
            Active | Archived => {
                if content_changed {
                    SupersedeWinsPreserveEdit
                } else {
                    Apply
                }
            }
            _ => Apply, // Should not reach here for Superseded local (handled above)
        };
    }

    // Both live (Active/Archived), different states.
    // Since Tombstoned and Superseded were already checked above,
    // the remaining states are Active and Archived only.
    let different = !((matches!(local_state, Active) && matches!(cand_state, Active))
        || (matches!(local_state, Archived) && matches!(cand_state, Archived)));
    if different {
        let any_direction_legal = lifecycle_can_transition(local_state, cand_state)
            || lifecycle_can_transition(cand_state, local_state);

        if content_changed {
            return if any_direction_legal {
                ApplyCombinationArchiveEdit
            } else {
                RefuseIllegalTransition
            };
        }

        // Same content, one side archived and other restored.
        if concurrent {
            return ConflictRestoreArchive;
        }

        return if lifecycle_can_transition(local_state, cand_state) {
            Apply
        } else {
            RefuseIllegalTransition
        };
    }

    // Same live state.
    if content_changed {
        return if concurrent {
            ConflictIncompatibleEdits
        } else {
            Apply
        };
    }

    Skip
}

/// Tag string for a merge decision, matching the Lean parity tags.
pub const fn classify_merge_decision_tag(d: KernelMergeDecision) -> &'static str {
    use KernelMergeDecision::*;
    match d {
        Apply => "apply",
        ApplyCombinationArchiveEdit => "apply_combination_archive_edit",
        Skip => "skip",
        RefuseTombstoneWins => "refuse_tombstone_wins",
        RefuseStaleRestore => "refuse_stale_restore",
        RefuseIllegalTransition => "refuse_illegal_transition",
        ConflictTwoReplacements => "conflict_two_replacements",
        ConflictRestoreArchive => "conflict_restore_archive",
        ConflictIncompatibleEdits => "conflict_incompatible_edits",
        SupersedeWinsPreserveEdit => "supersede_wins_preserve_edit",
    }
}

/// Task claim decision: idempotency, live-holder refusal, stale takeover, or FSM delegation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelClaimDecision {
    IdempotentSameSession,
    RefuseLiveHolder,
    TakeoverStaleHolder,
    ClaimFromOpen,
    DelegateToFsm,
}

/// Classifies a task-claim attempt into the five kernel branches.
pub const fn evaluate_claim_decision(
    status: KernelTaskStatus,
    same_session: bool,
    age_secs: Option<i64>,
    stale_after_secs: i64,
) -> KernelClaimDecision {
    match status {
        KernelTaskStatus::InProgress if same_session => KernelClaimDecision::IdempotentSameSession,
        KernelTaskStatus::InProgress => match age_secs {
            Some(age) if age <= stale_after_secs => KernelClaimDecision::RefuseLiveHolder,
            _ => KernelClaimDecision::TakeoverStaleHolder,
        },
        KernelTaskStatus::Open => KernelClaimDecision::ClaimFromOpen,
        KernelTaskStatus::Blocked | KernelTaskStatus::Done => KernelClaimDecision::DelegateToFsm,
    }
}

/// Tag string for a task-claim decision: "idempotent_same_session", "refuse_live_holder",
/// "takeover_stale_holder", "claim_from_open", "delegate_to_fsm".
pub const fn claim_decision_tag(d: KernelClaimDecision) -> &'static str {
    match d {
        KernelClaimDecision::IdempotentSameSession => "idempotent_same_session",
        KernelClaimDecision::RefuseLiveHolder => "refuse_live_holder",
        KernelClaimDecision::TakeoverStaleHolder => "takeover_stale_holder",
        KernelClaimDecision::ClaimFromOpen => "claim_from_open",
        KernelClaimDecision::DelegateToFsm => "delegate_to_fsm",
    }
}

/// Sovereignty decision: allow / forbidden memory type / not shareable / forbidden runtime
/// counter / unauthorized recipient / private row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelSovereigntyDecision {
    Allow,
    ForbiddenMemoryType,
    NotShareable,
    ForbiddenRuntimeCounter,
    UnauthorizedRecipient,
}

/// Evaluates the 5-gate sovereignty rule over egress/inbound candidates.
pub const fn evaluate_sovereignty_gate(
    permitted_memory_type: bool,
    is_shareable: bool,
    has_runtime_counter: bool,
    acl_empty: bool,
    recipient_in_acl: bool,
) -> KernelSovereigntyDecision {
    if !permitted_memory_type {
        KernelSovereigntyDecision::ForbiddenMemoryType
    } else if !is_shareable {
        KernelSovereigntyDecision::NotShareable
    } else if has_runtime_counter {
        KernelSovereigntyDecision::ForbiddenRuntimeCounter
    } else if !acl_empty && !recipient_in_acl {
        KernelSovereigntyDecision::UnauthorizedRecipient
    } else {
        KernelSovereigntyDecision::Allow
    }
}

/// Tag string for a sovereignty decision: "allow", "forbidden_memory_type", "not_shareable",
/// "forbidden_runtime_counter", "unauthorized_recipient".
pub const fn sovereignty_decision_tag(d: KernelSovereigntyDecision) -> &'static str {
    match d {
        KernelSovereigntyDecision::Allow => "allow",
        KernelSovereigntyDecision::ForbiddenMemoryType => "forbidden_memory_type",
        KernelSovereigntyDecision::NotShareable => "not_shareable",
        KernelSovereigntyDecision::ForbiddenRuntimeCounter => "forbidden_runtime_counter",
        KernelSovereigntyDecision::UnauthorizedRecipient => "unauthorized_recipient",
    }
}

/// Import precheck decision over an existing-row idempotency sweep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelImportPrecheck {
    FreshInsert,
    SkipSameUpdateId,
    RefusePrivateSovereign,
    ProceedMeshMerge,
}

/// Classifies a candidate import against the existing row state.
pub const fn evaluate_import_precheck(
    row_exists: bool,
    has_mesh_provenance: bool,
    same_update_id: bool,
) -> KernelImportPrecheck {
    if !row_exists {
        KernelImportPrecheck::FreshInsert
    } else if !has_mesh_provenance {
        KernelImportPrecheck::RefusePrivateSovereign
    } else if same_update_id {
        KernelImportPrecheck::SkipSameUpdateId
    } else {
        KernelImportPrecheck::ProceedMeshMerge
    }
}

/// Tag string for an import precheck decision: "fresh_insert", "skip_same_update_id",
/// "refuse_private_sovereign", "proceed_mesh_merge".
pub const fn import_precheck_tag(d: KernelImportPrecheck) -> &'static str {
    match d {
        KernelImportPrecheck::FreshInsert => "fresh_insert",
        KernelImportPrecheck::SkipSameUpdateId => "skip_same_update_id",
        KernelImportPrecheck::RefusePrivateSovereign => "refuse_private_sovereign",
        KernelImportPrecheck::ProceedMeshMerge => "proceed_mesh_merge",
    }
}

/// Procedure trust level for a procedurally-firing kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelProcedureTrust {
    Untrusted,
    Trusted,
    Promoted,
    Local,
    AbsentOrUnknown,
}

impl KernelProcedureTrust {
    pub const ALL: [KernelProcedureTrust; 5] = [
        KernelProcedureTrust::Untrusted,
        KernelProcedureTrust::Trusted,
        KernelProcedureTrust::Promoted,
        KernelProcedureTrust::Local,
        KernelProcedureTrust::AbsentOrUnknown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            KernelProcedureTrust::Untrusted => "untrusted",
            KernelProcedureTrust::Trusted => "trusted",
            KernelProcedureTrust::Promoted => "promoted",
            KernelProcedureTrust::Local => "local",
            KernelProcedureTrust::AbsentOrUnknown => "absent",
        }
    }
}

/// True when a procedure firing is untrusted (no trusted/promoted/local stamp).
pub const fn procedure_is_untrusted(
    trust: KernelProcedureTrust,
    has_import_provenance: bool,
) -> bool {
    match trust {
        KernelProcedureTrust::Untrusted => true,
        KernelProcedureTrust::Trusted
        | KernelProcedureTrust::Promoted
        | KernelProcedureTrust::Local => false,
        KernelProcedureTrust::AbsentOrUnknown => has_import_provenance,
    }
}

/// True when a procedure firing is lapsed (inclusive of the deadline).
pub const fn procedure_is_lapsed(valid_to: Option<i64>, now: i64) -> bool {
    match valid_to {
        Some(deadline) => deadline <= now,
        None => false,
    }
}

/// Outcome of a procedure fire attempt against its rehearsal budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelProcedureFireOutcome {
    RefuseUntrusted,
    RefuseLapsed,
    RefuseSpent,
    Fired {
        remaining_fires: Option<u64>,
        performance_count: u64,
    },
}

/// Evaluates the procedure fire order: untrusted → lapsed → spent → fired.
pub const fn evaluate_procedure_fire(
    trust: KernelProcedureTrust,
    has_import_provenance: bool,
    valid_to: Option<i64>,
    now: i64,
    remaining_fires: Option<u64>,
    performance_count: u64,
) -> KernelProcedureFireOutcome {
    if procedure_is_untrusted(trust, has_import_provenance) {
        KernelProcedureFireOutcome::RefuseUntrusted
    } else if procedure_is_lapsed(valid_to, now) {
        KernelProcedureFireOutcome::RefuseLapsed
    } else {
        match remaining_fires {
            Some(0) => KernelProcedureFireOutcome::RefuseSpent,
            Some(b) => KernelProcedureFireOutcome::Fired {
                remaining_fires: Some(b - 1),
                performance_count: performance_count.saturating_add(1),
            },
            None => KernelProcedureFireOutcome::Fired {
                remaining_fires: None,
                performance_count: performance_count.saturating_add(1),
            },
        }
    }
}

/// Tag string for a procedure fire outcome: "refuse_untrusted", "refuse_lapsed", "refuse_spent",
/// "fired_bounded", "fired_unbounded".
pub const fn procedure_fire_tag(o: KernelProcedureFireOutcome) -> &'static str {
    match o {
        KernelProcedureFireOutcome::RefuseUntrusted => "refuse_untrusted",
        KernelProcedureFireOutcome::RefuseLapsed => "refuse_lapsed",
        KernelProcedureFireOutcome::RefuseSpent => "refuse_spent",
        KernelProcedureFireOutcome::Fired {
            remaining_fires: Some(_),
            ..
        } => "fired_bounded",
        KernelProcedureFireOutcome::Fired {
            remaining_fires: None,
            ..
        } => "fired_unbounded",
    }
}

/// Task status join semilattice rank helper (monotone max-rank join).
pub const fn task_status_rank(status: KernelTaskStatus) -> u8 {
    match status {
        KernelTaskStatus::Done => 3,
        KernelTaskStatus::InProgress => 2,
        KernelTaskStatus::Blocked => 1,
        KernelTaskStatus::Open => 0,
    }
}

/// Join task status via the max-rank semilattice (LWW cell).
pub const fn join_task_status(a: KernelTaskStatus, b: KernelTaskStatus) -> KernelTaskStatus {
    if task_status_rank(a) >= task_status_rank(b) {
        a
    } else {
        b
    }
}

/// Sequence continuity decision across twin-CRDT merges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelSeqDecision {
    DuplicateIgnore,
    ApplyAndAdvance,
    BufferOutOfOrder,
}

/// Evaluates sequence continuity: duplicates ignored, out-of-order buffered, in-order applied.
pub const fn evaluate_seq_continuity(
    already_seen: bool,
    expected_seq: u64,
    frame_seq: u64,
) -> KernelSeqDecision {
    if already_seen || frame_seq < expected_seq {
        KernelSeqDecision::DuplicateIgnore
    } else if frame_seq > expected_seq {
        KernelSeqDecision::BufferOutOfOrder
    } else {
        KernelSeqDecision::ApplyAndAdvance
    }
}

/// Tag string for a sequence continuity decision: "duplicate_ignore", "apply_and_advance",
/// "buffer_out_of_order".
pub const fn seq_decision_tag(d: KernelSeqDecision) -> &'static str {
    match d {
        KernelSeqDecision::DuplicateIgnore => "duplicate_ignore",
        KernelSeqDecision::ApplyAndAdvance => "apply_and_advance",
        KernelSeqDecision::BufferOutOfOrder => "buffer_out_of_order",
    }
}

/// Retention verdict: over-latency / store-empty / receipt-missing / worktree-moved / retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelRetentionVerdict {
    Retained,
    FailedOverBudget,
    FailedStoreEmpty,
    FailedReceiptMissing,
    FailedWorktreeMoved,
}

/// Evaluates the retention gate: latency budget + store non-empty + receipt + worktree binding.
pub const fn evaluate_retention_gate(
    within_latency_budget: bool,
    store_non_empty: bool,
    receipt_found: bool,
    worktree_bound: bool,
    worktree_matches: bool,
) -> KernelRetentionVerdict {
    if !within_latency_budget {
        KernelRetentionVerdict::FailedOverBudget
    } else if !store_non_empty {
        KernelRetentionVerdict::FailedStoreEmpty
    } else if !receipt_found {
        KernelRetentionVerdict::FailedReceiptMissing
    } else if worktree_bound && !worktree_matches {
        KernelRetentionVerdict::FailedWorktreeMoved
    } else {
        KernelRetentionVerdict::Retained
    }
}

/// Tag string for a retention verdict: "retained", "failed_over_budget", "failed_store_empty",
/// "failed_receipt_missing", "failed_worktree_moved".
pub const fn retention_verdict_tag(v: KernelRetentionVerdict) -> &'static str {
    match v {
        KernelRetentionVerdict::Retained => "retained",
        KernelRetentionVerdict::FailedOverBudget => "failed_over_budget",
        KernelRetentionVerdict::FailedStoreEmpty => "failed_store_empty",
        KernelRetentionVerdict::FailedReceiptMissing => "failed_receipt_missing",
        KernelRetentionVerdict::FailedWorktreeMoved => "failed_worktree_moved",
    }
}

/// Claim scope gate verdict for strict pre-push evaluation of an `in_progress` task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelClaimScopeVerdict {
    BlockUnscoped,
    BlockUncapturableScope,
    BlockLegacyUnattributed,
    BlockBranchMatch,
    BlockWorktreeMatch,
    AllowIsolatedScope,
}

impl KernelClaimScopeVerdict {
    pub const fn blocks(self) -> bool {
        !matches!(self, KernelClaimScopeVerdict::AllowIsolatedScope)
    }
}

/// Evaluates whether a live `in_progress` task claim blocks the caller's push scope.
pub const fn evaluate_claim_scope_gate(
    scoped: bool,
    caller_identifiable: bool,
    claim_attributed: bool,
    branch_matches: bool,
    worktree_matches: bool,
) -> KernelClaimScopeVerdict {
    if !scoped {
        KernelClaimScopeVerdict::BlockUnscoped
    } else if !caller_identifiable {
        KernelClaimScopeVerdict::BlockUncapturableScope
    } else if !claim_attributed {
        KernelClaimScopeVerdict::BlockLegacyUnattributed
    } else if branch_matches {
        KernelClaimScopeVerdict::BlockBranchMatch
    } else if worktree_matches {
        KernelClaimScopeVerdict::BlockWorktreeMatch
    } else {
        KernelClaimScopeVerdict::AllowIsolatedScope
    }
}

/// Tag string for a claim scope gate verdict: "block_unscoped",
/// "block_uncapturable_scope", "block_legacy_unattributed",
/// "block_branch_match", "block_worktree_match", "allow_isolated_scope".
pub const fn claim_scope_verdict_tag(v: KernelClaimScopeVerdict) -> &'static str {
    match v {
        KernelClaimScopeVerdict::BlockUnscoped => "block_unscoped",
        KernelClaimScopeVerdict::BlockUncapturableScope => "block_uncapturable_scope",
        KernelClaimScopeVerdict::BlockLegacyUnattributed => "block_legacy_unattributed",
        KernelClaimScopeVerdict::BlockBranchMatch => "block_branch_match",
        KernelClaimScopeVerdict::BlockWorktreeMatch => "block_worktree_match",
        KernelClaimScopeVerdict::AllowIsolatedScope => "allow_isolated_scope",
    }
}

/// Decision from the 5-stage kinetic Action submission gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionGateDecision {
    Allow,
    RejectUnauthorized,
    RejectInvalidParams,
    RejectSparqlAsk,
    RejectTwinBounds,
    RejectPostShacl,
}

/// Evaluates the kinetic Action submission gate in deterministic fail-closed order:
/// ReBAC authorization -> typed parameters -> bound SPARQL ASK -> twin precondition bounds -> post-image SHACL.
pub const fn evaluate_action_gate(
    principal_authorized: bool,
    params_valid: bool,
    sparql_ask_passed: bool,
    twin_bounds_passed: bool,
    post_shacl_passed: bool,
) -> ActionGateDecision {
    if !principal_authorized {
        ActionGateDecision::RejectUnauthorized
    } else if !params_valid {
        ActionGateDecision::RejectInvalidParams
    } else if !sparql_ask_passed {
        ActionGateDecision::RejectSparqlAsk
    } else if !twin_bounds_passed {
        ActionGateDecision::RejectTwinBounds
    } else if !post_shacl_passed {
        ActionGateDecision::RejectPostShacl
    } else {
        ActionGateDecision::Allow
    }
}

/// Tag string for an action gate decision.
pub const fn action_gate_tag(d: ActionGateDecision) -> &'static str {
    match d {
        ActionGateDecision::Allow => "allow",
        ActionGateDecision::RejectUnauthorized => "reject_unauthorized",
        ActionGateDecision::RejectInvalidParams => "reject_invalid_params",
        ActionGateDecision::RejectSparqlAsk => "reject_sparql_ask",
        ActionGateDecision::RejectTwinBounds => "reject_twin_bounds",
        ActionGateDecision::RejectPostShacl => "reject_post_shacl",
    }
}

/// State of the active receipt version being targeted for revert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionReceiptState {
    Active,
    AlreadyReverted,
}

impl ActionReceiptState {
    pub const fn as_str(self) -> &'static str {
        match self {
            ActionReceiptState::Active => "active",
            ActionReceiptState::AlreadyReverted => "already_reverted",
        }
    }
}

/// Decision from the kinetic Action revert gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionRevertDecision {
    RevertCommit,
    ForcedRevertCommit,
    RejectAlreadyReverted,
    RejectPermissionDenied,
    RejectStaleHeadConflict,
}

/// Evaluates a kinetic Action revert attempt:
/// 1. Refuses already-reverted receipts unconditionally (no double revert).
/// 2. Requires owner/admin when `force_requested` is true; otherwise requires editor or owner/admin.
/// 3. Commits a forced revert when `force_requested` is authorized; otherwise requires `head_cas_matches`.
pub const fn evaluate_action_revert(
    receipt_state: ActionReceiptState,
    head_cas_matches: bool,
    force_requested: bool,
    caller_is_owner_or_admin: bool,
    caller_is_editor_or_above: bool,
) -> ActionRevertDecision {
    if matches!(receipt_state, ActionReceiptState::AlreadyReverted) {
        ActionRevertDecision::RejectAlreadyReverted
    } else if force_requested {
        if caller_is_owner_or_admin {
            ActionRevertDecision::ForcedRevertCommit
        } else {
            ActionRevertDecision::RejectPermissionDenied
        }
    } else if !(caller_is_editor_or_above || caller_is_owner_or_admin) {
        ActionRevertDecision::RejectPermissionDenied
    } else if !head_cas_matches {
        ActionRevertDecision::RejectStaleHeadConflict
    } else {
        ActionRevertDecision::RevertCommit
    }
}

/// Tag string for an action revert decision.
pub const fn action_revert_tag(d: ActionRevertDecision) -> &'static str {
    match d {
        ActionRevertDecision::RevertCommit => "revert_commit",
        ActionRevertDecision::ForcedRevertCommit => "forced_revert_commit",
        ActionRevertDecision::RejectAlreadyReverted => "reject_already_reverted",
        ActionRevertDecision::RejectPermissionDenied => "reject_permission_denied",
        ActionRevertDecision::RejectStaleHeadConflict => "reject_stale_head_conflict",
    }
}

/// Requested target task status in an `ActionEffectTemplate::TransitionTask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTaskTargetStatus {
    Open,
    InProgress,
    Blocked,
    Done,
}

impl ActionTaskTargetStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ActionTaskTargetStatus::Open => "open",
            ActionTaskTargetStatus::InProgress => "in_progress",
            ActionTaskTargetStatus::Blocked => "blocked",
            ActionTaskTargetStatus::Done => "done",
        }
    }
}

/// Error from evaluating an `ActionEffectTemplate::TransitionTask` transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTaskEffectError {
    DoneRequiresComplete,
    IllegalTransition,
}

/// Evaluates a kinetic `TransitionTask` effect transition.
/// Unconditionally rejects `Done` (`DoneRequiresComplete`) so Lock 2 (`neurostrata_task_complete`)
/// can never be bypassed by an ActionType; allows same-status annotation or legal FSM edges
/// into `Open`, `InProgress`, or `Blocked`.
pub const fn evaluate_action_task_effect(
    current: KernelTaskStatus,
    requested: ActionTaskTargetStatus,
) -> Result<KernelTaskStatus, ActionTaskEffectError> {
    let target = match requested {
        ActionTaskTargetStatus::Open => KernelTaskStatus::Open,
        ActionTaskTargetStatus::InProgress => KernelTaskStatus::InProgress,
        ActionTaskTargetStatus::Blocked => KernelTaskStatus::Blocked,
        ActionTaskTargetStatus::Done => return Err(ActionTaskEffectError::DoneRequiresComplete),
    };
    let same_status = matches!(
        (current, target),
        (KernelTaskStatus::Open, KernelTaskStatus::Open)
            | (KernelTaskStatus::InProgress, KernelTaskStatus::InProgress)
            | (KernelTaskStatus::Blocked, KernelTaskStatus::Blocked)
    );
    if same_status || task_edge_guard(current, target).is_some() {
        Ok(target)
    } else {
        Err(ActionTaskEffectError::IllegalTransition)
    }
}

/// Tag string for an action task-effect transition outcome.
pub const fn action_task_effect_tag(
    res: Result<KernelTaskStatus, ActionTaskEffectError>,
) -> &'static str {
    match res {
        Ok(KernelTaskStatus::Open) => "ok_open",
        Ok(KernelTaskStatus::InProgress) => "ok_in_progress",
        Ok(KernelTaskStatus::Blocked) => "ok_blocked",
        Ok(KernelTaskStatus::Done) => "ok_done",
        Err(ActionTaskEffectError::DoneRequiresComplete) => "err_done_requires_complete",
        Err(ActionTaskEffectError::IllegalTransition) => "err_illegal",
    }
}

/// Kind of kinetic action memory row for SynapticMesh shareability evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionMemoryKind {
    ActionType,
    ActionReceipt,
}

impl ActionMemoryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ActionMemoryKind::ActionType => "action_type",
            ActionMemoryKind::ActionReceipt => "action_receipt",
        }
    }
}

/// Returns whether a kinetic action memory row is eligible to cross SynapticMesh.
/// `ActionReceipt` is unconditionally private (`false`); `ActionType` is shareable iff
/// `explicit_shareable` is `true`.
pub const fn is_action_memory_shareable(kind: ActionMemoryKind, explicit_shareable: bool) -> bool {
    match kind {
        ActionMemoryKind::ActionType => explicit_shareable,
        ActionMemoryKind::ActionReceipt => false,
    }
}

/// Allocation of result slots across reserved primary-normative, reserved graph-expansion,
/// and general remaining pools (`neurostrata-5xjo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HybridSlotAllocation {
    pub normative_taken: u32,
    pub expansion_taken: u32,
    pub remaining_taken: u32,
}

const fn min_u32(a: u32, b: u32) -> u32 {
    if a <= b {
        a
    } else {
        b
    }
}

/// Computes the normative slot quota: `min(3, ceil(limit / 2))` when `limit >= 3`, else `0`.
pub const fn hybrid_normative_quota(limit: u32) -> u32 {
    if limit < 3 {
        0
    } else {
        min_u32(3, limit.saturating_add(1) / 2)
    }
}

/// Computes the graph-expansion slot quota: `min(4, limit / 4)` when `limit >= 4`, else `0`.
pub const fn hybrid_expansion_quota(limit: u32) -> u32 {
    if limit < 4 {
        0
    } else {
        min_u32(4, limit / 4)
    }
}

/// Pure slot allocator for two-pool reservation (`reserve_normative_and_expansion_slots`).
pub const fn allocate_hybrid_slots(
    limit: u32,
    avail_normative: u32,
    avail_expansion: u32,
    avail_other: u32,
) -> HybridSlotAllocation {
    let norm_q = hybrid_normative_quota(limit);
    let exp_q = hybrid_expansion_quota(limit);
    let exp_taken = min_u32(exp_q, avail_expansion);
    let after_exp = limit.saturating_sub(exp_taken);
    let norm_cap = min_u32(norm_q, after_exp);
    let norm_taken = min_u32(norm_cap, avail_normative);
    let rem_slots = after_exp.saturating_sub(norm_taken);
    let leftover_norm = avail_normative.saturating_sub(norm_taken);
    let leftover_exp = avail_expansion.saturating_sub(exp_taken);
    let leftover_pool = leftover_norm
        .saturating_add(leftover_exp)
        .saturating_add(avail_other);
    let rem_taken = min_u32(rem_slots, leftover_pool);
    HybridSlotAllocation {
        normative_taken: norm_taken,
        expansion_taken: exp_taken,
        remaining_taken: rem_taken,
    }
}

/// Error from intersecting an interface's variable bounds with a child twin schema's bounds (`neurostrata-brs1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TwinBoundError {
    InvertedInterfaceBounds,
    InvertedTwinBounds,
    DisjointBounds,
}

/// Intersects an interface bound `[iface_min, iface_max]` with a child schema bound `[twin_min, twin_max]`,
/// enforcing covariant bound tightening (`eff_min >= iface_min && eff_max <= iface_max`).
pub const fn intersect_var_bounds(
    iface_min: i64,
    iface_max: i64,
    twin_min: i64,
    twin_max: i64,
) -> Result<(i64, i64), TwinBoundError> {
    if iface_min > iface_max {
        return Err(TwinBoundError::InvertedInterfaceBounds);
    }
    if twin_min > twin_max {
        return Err(TwinBoundError::InvertedTwinBounds);
    }
    let eff_min = if iface_min >= twin_min {
        iface_min
    } else {
        twin_min
    };
    let eff_max = if iface_max <= twin_max {
        iface_max
    } else {
        twin_max
    };
    if eff_min > eff_max {
        Err(TwinBoundError::DisjointBounds)
    } else {
        Ok((eff_min, eff_max))
    }
}

/// Deterministic link-reducer operator kind (`neurostrata-brs1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReducerOpKind {
    Count,
    Sum,
    Min,
    Max,
    Avg,
    AllInBounds,
    AnyOutOfBounds,
}

impl ReducerOpKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ReducerOpKind::Count => "count",
            ReducerOpKind::Sum => "sum",
            ReducerOpKind::Min => "min",
            ReducerOpKind::Max => "max",
            ReducerOpKind::Avg => "avg",
            ReducerOpKind::AllInBounds => "all_in_bounds",
            ReducerOpKind::AnyOutOfBounds => "any_out_of_bounds",
        }
    }
}

/// Total outcome of evaluating a declarative link reducer over 1-hop twin neighbors (`neurostrata-brs1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReducerEvalOutcome {
    pub numeric_val: i64,
    pub bool_val: bool,
    pub is_defined: bool,
}

/// Total, division-by-zero-free link reducer evaluation kernel (`neurostrata-brs1`).
pub const fn evaluate_reducer_total(
    op: ReducerOpKind,
    count: u32,
    sum_val: i64,
    min_val: i64,
    max_val: i64,
    any_out_of_bounds: bool,
) -> ReducerEvalOutcome {
    if count == 0 {
        match op {
            ReducerOpKind::Count | ReducerOpKind::Sum | ReducerOpKind::Avg => ReducerEvalOutcome {
                numeric_val: 0,
                bool_val: false,
                is_defined: true,
            },
            ReducerOpKind::Min | ReducerOpKind::Max => ReducerEvalOutcome {
                numeric_val: 0,
                bool_val: false,
                is_defined: false,
            },
            ReducerOpKind::AllInBounds => ReducerEvalOutcome {
                numeric_val: 1,
                bool_val: true,
                is_defined: true,
            },
            ReducerOpKind::AnyOutOfBounds => ReducerEvalOutcome {
                numeric_val: 0,
                bool_val: false,
                is_defined: true,
            },
        }
    } else {
        match op {
            ReducerOpKind::Count => ReducerEvalOutcome {
                numeric_val: count as i64,
                bool_val: true,
                is_defined: true,
            },
            ReducerOpKind::Sum => ReducerEvalOutcome {
                numeric_val: sum_val,
                bool_val: true,
                is_defined: true,
            },
            ReducerOpKind::Min => ReducerEvalOutcome {
                numeric_val: min_val,
                bool_val: true,
                is_defined: true,
            },
            ReducerOpKind::Max => ReducerEvalOutcome {
                numeric_val: max_val,
                bool_val: true,
                is_defined: true,
            },
            ReducerOpKind::Avg => ReducerEvalOutcome {
                numeric_val: sum_val / (count as i64),
                bool_val: true,
                is_defined: true,
            },
            ReducerOpKind::AllInBounds => ReducerEvalOutcome {
                numeric_val: if !any_out_of_bounds { 1 } else { 0 },
                bool_val: !any_out_of_bounds,
                is_defined: true,
            },
            ReducerOpKind::AnyOutOfBounds => ReducerEvalOutcome {
                numeric_val: if any_out_of_bounds { 1 } else { 0 },
                bool_val: any_out_of_bounds,
                is_defined: true,
            },
        }
    }
}

/// Resolution tier for Tier-1 cross-file call graph linking (`neurostrata-rbde`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrossFileResolution {
    SameFile,
    ImportedFile,
    WorkspaceUnique,
    UnresolvedSelfCall,
    UnresolvedAmbiguousOrStd,
}

impl CrossFileResolution {
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::SameFile => "SAME_FILE",
            Self::ImportedFile => "IMPORTED_FILE",
            Self::WorkspaceUnique => "WORKSPACE_UNIQUE",
            Self::UnresolvedSelfCall => "UNRESOLVED_SELF_CALL",
            Self::UnresolvedAmbiguousOrStd => "UNRESOLVED_AMBIGUOUS_OR_STD",
        }
    }
}

/// Pure priority-ordered cross-file call resolution kernel (`neurostrata-rbde`).
///
/// 1. Self-calls are rejected (`UnresolvedSelfCall`) to prevent self-loops.
/// 2. Same-file definitions take highest priority (`SameFile`).
/// 3. Imported-file definitions (`dependency_map`) take second priority (`ImportedFile`).
/// 4. Unambiguous workspace definitions (`workspace_match_count == 1`) with
///    `ident_len >= 4` and `!is_common_std` resolve as `WorkspaceUnique`.
pub const fn resolve_cross_file_call(
    is_self_call: bool,
    in_same_file: bool,
    in_imported_file: bool,
    workspace_match_count: u32,
    ident_len: u32,
    is_common_std: bool,
) -> CrossFileResolution {
    if is_self_call {
        CrossFileResolution::UnresolvedSelfCall
    } else if in_same_file {
        CrossFileResolution::SameFile
    } else if in_imported_file {
        CrossFileResolution::ImportedFile
    } else if workspace_match_count == 1 && ident_len >= 4 && !is_common_std {
        CrossFileResolution::WorkspaceUnique
    } else {
        CrossFileResolution::UnresolvedAmbiguousOrStd
    }
}

/// Copy-on-Write row visibility inside a counterfactual `ScenarioOverlay` (`neurostrata-ai0v`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioRowVisibility {
    VisibleStaged,
    HiddenRetired,
    VisibleLive,
    Absent,
}

impl ScenarioRowVisibility {
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::VisibleStaged => "VISIBLE_STAGED",
            Self::HiddenRetired => "HIDDEN_RETIRED",
            Self::VisibleLive => "VISIBLE_LIVE",
            Self::Absent => "ABSENT",
        }
    }
}

/// Pure Copy-on-Write precedence kernel for `ScenarioOverlay` (`neurostrata-ai0v`):
/// 1. `staged_upsert` overrides both retirement and live state (`VisibleStaged`).
/// 2. `staged_retired` masks `live_present` (`HiddenRetired`).
/// 3. Otherwise visibility equals `live_present` (`VisibleLive` vs `Absent`).
pub const fn resolve_scenario_row(
    live_present: bool,
    staged_retired: bool,
    staged_upsert: bool,
) -> ScenarioRowVisibility {
    if staged_upsert {
        ScenarioRowVisibility::VisibleStaged
    } else if staged_retired {
        ScenarioRowVisibility::HiddenRetired
    } else if live_present {
        ScenarioRowVisibility::VisibleLive
    } else {
        ScenarioRowVisibility::Absent
    }
}

/// Decision outcome for optimistic CAS scenario commit (`neurostrata-ai0v`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioCommitDecision {
    CommitAllowed,
    RejectedExpired,
    RejectedBaseConflict,
    RejectedUnauthorized,
    RejectedShaclViolations,
}

impl ScenarioCommitDecision {
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::CommitAllowed => "COMMIT_ALLOWED",
            Self::RejectedExpired => "REJECTED_EXPIRED",
            Self::RejectedBaseConflict => "REJECTED_BASE_CONFLICT",
            Self::RejectedUnauthorized => "REJECTED_UNAUTHORIZED",
            Self::RejectedShaclViolations => "REJECTED_SHACL_VIOLATIONS",
        }
    }
}

/// Pure optimistic CAS + ReBAC + SHACL commit gate for `ScenarioOverlay` (`neurostrata-ai0v`).
pub const fn evaluate_scenario_commit(
    expired: bool,
    all_base_fingerprints_match: bool,
    rebac_authorized: bool,
    enforce_shacl: bool,
    shacl_violation_count: u32,
) -> ScenarioCommitDecision {
    if expired {
        ScenarioCommitDecision::RejectedExpired
    } else if !all_base_fingerprints_match {
        ScenarioCommitDecision::RejectedBaseConflict
    } else if !rebac_authorized {
        ScenarioCommitDecision::RejectedUnauthorized
    } else if enforce_shacl && shacl_violation_count > 0 {
        ScenarioCommitDecision::RejectedShaclViolations
    } else {
        ScenarioCommitDecision::CommitAllowed
    }
}

/// Four-stage guided diff tour stop category (`neurostrata-737r`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourStopCategory {
    Foundations,
    CoreLogic,
    SurfaceAndCallers,
    TestsAndDocs,
}

impl TourStopCategory {
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::Foundations => "FOUNDATIONS",
            Self::CoreLogic => "CORE_LOGIC",
            Self::SurfaceAndCallers => "SURFACE_AND_CALLERS",
            Self::TestsAndDocs => "TESTS_AND_DOCS",
        }
    }

    pub const fn stop_number(self) -> u8 {
        match self {
            Self::Foundations => 1,
            Self::CoreLogic => 2,
            Self::SurfaceAndCallers => 3,
            Self::TestsAndDocs => 4,
        }
    }
}

/// Pure 4-stage stop classifier for `neurostrata_diff_tour` (`neurostrata-737r`).
///
/// 1. Tests and documentation (`is_test_or_doc`) always route to Stop 4 (`TestsAndDocs`).
/// 2. Non-test foundation symbols (`is_foundation_kind_or_path`) or pure dependency
///    sinks in the diff subgraph (`in_degree_in_diff > 0 && out_degree_in_diff == 0`)
///    route to Stop 1 (`Foundations`).
/// 3. Non-test entrypoint paths (`is_entrypoint_path`) or pure callers in the diff
///    subgraph (`in_degree_in_diff == 0 && out_degree_in_diff > 0`) route to Stop 3
///    (`SurfaceAndCallers`).
/// 4. Remaining modified symbols route to Stop 2 (`CoreLogic`).
pub const fn classify_tour_stop(
    is_test_or_doc: bool,
    is_foundation_kind_or_path: bool,
    is_entrypoint_path: bool,
    in_degree_in_diff: u32,
    out_degree_in_diff: u32,
) -> TourStopCategory {
    if is_test_or_doc {
        TourStopCategory::TestsAndDocs
    } else if is_foundation_kind_or_path || (in_degree_in_diff > 0 && out_degree_in_diff == 0) {
        TourStopCategory::Foundations
    } else if is_entrypoint_path || (in_degree_in_diff == 0 && out_degree_in_diff > 0) {
        TourStopCategory::SurfaceAndCallers
    } else {
        TourStopCategory::CoreLogic
    }
}

/// Decision for registering a custom `InsightDefinition` (`neurostrata-kx2n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightDefineDecision {
    DefineAllowed,
    RejectedEmptyQuery,
    RejectedBuiltinCollision,
    RejectedQuotaExceeded,
}

impl InsightDefineDecision {
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::DefineAllowed => "DEFINE_ALLOWED",
            Self::RejectedEmptyQuery => "REJECTED_EMPTY_QUERY",
            Self::RejectedBuiltinCollision => "REJECTED_BUILTIN_COLLISION",
            Self::RejectedQuotaExceeded => "REJECTED_QUOTA_EXCEEDED",
        }
    }
}

/// Evaluate whether a custom `InsightDefinition` may be stored (`neurostrata-kx2n`).
///
/// 1. Empty query strings are rejected (`RejectedEmptyQuery`).
/// 2. Collisions with the 4 built-in insight slugs are rejected (`RejectedBuiltinCollision`).
/// 3. Exceeding `max_custom_insights` (16) when creating a new slug is rejected
///    (`RejectedQuotaExceeded`); updating an existing slug (`is_existing_slug == true`) is allowed.
pub const fn evaluate_insight_define(
    query_non_empty: bool,
    is_builtin_slug: bool,
    is_existing_slug: bool,
    existing_custom_count: u32,
    max_custom_insights: u32,
) -> InsightDefineDecision {
    if !query_non_empty {
        InsightDefineDecision::RejectedEmptyQuery
    } else if is_builtin_slug {
        InsightDefineDecision::RejectedBuiltinCollision
    } else if !is_existing_slug && existing_custom_count >= max_custom_insights {
        InsightDefineDecision::RejectedQuotaExceeded
    } else {
        InsightDefineDecision::DefineAllowed
    }
}

/// Bitemporal point-in-time validity check (`valid_from <= as_of_ts < valid_to`)
/// used by `neurostrata_insights` retroactive `as_of` evaluation (`neurostrata-kx2n`).
pub const fn is_memory_valid_at(
    valid_from: i64,
    has_valid_to: bool,
    valid_to: i64,
    as_of_ts: i64,
) -> bool {
    valid_from <= as_of_ts && (!has_valid_to || valid_to > as_of_ts)
}

/// Epistemic maturity status of a taxonomy concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EpistemicStatus {
    Seed,
    Defined,
    Synthesized,
    Verified,
}

impl EpistemicStatus {
    /// Weight in basis points (0-10000).
    pub const fn weight_bps(self) -> u32 {
        match self {
            EpistemicStatus::Seed => 2500,
            EpistemicStatus::Defined => 5000,
            EpistemicStatus::Synthesized => 8000,
            EpistemicStatus::Verified => 10000,
        }
    }

    /// String representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            EpistemicStatus::Seed => "seed",
            EpistemicStatus::Defined => "defined",
            EpistemicStatus::Synthesized => "synthesized",
            EpistemicStatus::Verified => "verified",
        }
    }
}

/// Classifies epistemic status from four boolean pillars.
pub const fn classify_epistemic_status(
    has_definition: bool,
    has_taxonomy_link: bool,
    has_synthesis: bool,
    has_empirical_grounding: bool,
) -> EpistemicStatus {
    if !has_definition || !has_taxonomy_link {
        EpistemicStatus::Seed
    } else if !has_synthesis {
        EpistemicStatus::Defined
    } else if !has_empirical_grounding {
        EpistemicStatus::Synthesized
    } else {
        EpistemicStatus::Verified
    }
}

/// Validates that `broader_edges` form a DAG (no self-loops, no cycles).
pub fn validate_taxonomy_dag(broader_edges: &[(u32, u32)]) -> bool {
    // Check self-loops
    for &(u, v) in broader_edges {
        if u == v {
            return false;
        }
    }
    // Detect cycles using DFS with color marking
    use std::collections::HashMap;
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(u, v) in broader_edges {
        adj.entry(u).or_default().push(v);
    }
    let mut color: HashMap<u32, u8> = HashMap::new(); // 0=unvisited, 1=visiting, 2=done
    fn dfs(u: u32, adj: &HashMap<u32, Vec<u32>>, color: &mut HashMap<u32, u8>) -> bool {
        color.insert(u, 1);
        if let Some(neighbors) = adj.get(&u) {
            for &v in neighbors {
                match color.get(&v) {
                    Some(1) => return false, // back edge = cycle
                    Some(2) => {}
                    _ => {
                        if !dfs(v, adj, color) {
                            return false;
                        }
                    }
                }
            }
        }
        color.insert(u, 2);
        true
    }
    for &u in adj.keys() {
        if !color.contains_key(&u) {
            if !dfs(u, &adj, &mut color) {
                return false;
            }
        }
    }
    true
}

/// Validates SKOS disjointness: related/contrast edges must not overlap with
/// transitive broader hierarchy in either direction.
pub fn validate_skos_disjointness(
    broader_edges: &[(u32, u32)],
    related_edges: &[(u32, u32)],
    contrast_edges: &[(u32, u32)],
) -> bool {
    // Cycle guard: reject if broader_edges contain cycles (self-loop or cycles)
    if !validate_taxonomy_dag(broader_edges) {
        return false;
    }
    use std::collections::{HashMap, HashSet};

    // Build adjacency for broader hierarchy
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut rev_adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(u, v) in broader_edges {
        adj.entry(u).or_default().push(v);
        rev_adj.entry(v).or_default().push(u);
    }

    // Compute transitive reachability: for each node, all nodes reachable via broader+
    let mut reachable: HashMap<u32, HashSet<u32>> = HashMap::new();

    fn compute_reachable(
        u: u32,
        adj: &HashMap<u32, Vec<u32>>,
        memo: &mut HashMap<u32, HashSet<u32>>,
    ) -> HashSet<u32> {
        if let Some(cached) = memo.get(&u) {
            return cached.clone();
        }
        let mut set = HashSet::new();
        if let Some(neighbors) = adj.get(&u) {
            for &v in neighbors {
                set.insert(v);
                let child_reachable = compute_reachable(v, adj, memo);
                set.extend(child_reachable);
            }
        }
        memo.insert(u, set.clone());
        set
    }

    // Compute for all nodes in adj
    for &u in adj.keys() {
        compute_reachable(u, &adj, &mut reachable);
    }

    // A related/contrast pair is a violation when the two concepts are comparable
    // in the broader hierarchy in either direction, or identical.
    let pair_disjoint = |u: u32, v: u32| -> bool {
        if u == v {
            return false;
        }
        if let Some(reach_u) = reachable.get(&u) {
            if reach_u.contains(&v) {
                return false;
            }
        }
        if let Some(reach_v) = reachable.get(&v) {
            if reach_v.contains(&u) {
                return false;
            }
        }
        true
    };

    for &(u, v) in related_edges {
        if !pair_disjoint(u, v) {
            return false;
        }
    }
    for &(u, v) in contrast_edges {
        if !pair_disjoint(u, v) {
            return false;
        }
    }

    true
}

/// Computes epistemic coverage in basis points.
pub fn compute_epistemic_coverage_bps(statuses: &[EpistemicStatus], unmapped_domain_count: u32) -> u32 {
    if statuses.is_empty() && unmapped_domain_count == 0 {
        return 0;
    }
    let sum_weights: u64 = statuses.iter().map(|s| s.weight_bps() as u64).sum();
    let denom: u64 = statuses.len() as u64 + unmapped_domain_count as u64;
    ((sum_weights / denom) as u32).min(10000)
}

/// Returns realization status gap in basis points.
pub const fn realization_status_gap_bps(status: &str) -> u32 {
    match status.as_bytes() {
        b"unimplemented" => 10000,
        b"partial" => 7500,
        b"unknown" => 6000,
        b"implemented" => 2500,
        b"external_only" => 0,
        _ => 6000,
    }
}

/// Computes frontier leverage score in basis points.
pub const fn compute_frontier_leverage_bps(
    axis_alignment_bps: u32,
    realization_gap_bps: u32,
    canonical_connectivity_count: u32,
    has_frontier_or_limitation: bool,
    has_references_or_comparables: bool,
) -> u32 {
    if realization_gap_bps == 0
        || (realization_gap_bps <= 2500 && !has_frontier_or_limitation)
    {
        return 0;
    }
    let align = if axis_alignment_bps > 10000 {
        10000u64
    } else {
        axis_alignment_bps as u64
    };
    let gap = if realization_gap_bps > 10000 {
        10000u64
    } else {
        realization_gap_bps as u64
    };
    let conn_capped = if canonical_connectivity_count > 10 {
        10u64
    } else {
        canonical_connectivity_count as u64
    };
    let conn_bps = conn_capped * 1000;
    let frontier_bps = if has_frontier_or_limitation { 10000u64 } else { 0u64 };
    let cite_bps = if has_references_or_comparables { 10000u64 } else { 0u64 };
    let raw = (35 * align + 25 * gap + 20 * conn_bps + 10 * frontier_bps + 10 * cite_bps) / 100;
    if raw > 10000 {
        10000
    } else {
        raw as u32
    }
}
// Unit tests for formal_kernel.rs – covering classify_epistemic_status, validate_taxonomy_dag, validate_skos_disjointness, compute_epistemic_coverage_bps, realization_status_gap_bps, compute_frontier_leverage_bps
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_classify_epistemic_status() {
        assert_eq!(classify_epistemic_status(false, false, false, false), EpistemicStatus::Seed);
        assert_eq!(classify_epistemic_status(true, false, false, false), EpistemicStatus::Seed);
        assert_eq!(classify_epistemic_status(true, true, false, false), EpistemicStatus::Defined);
        assert_eq!(classify_epistemic_status(true, true, true, false), EpistemicStatus::Synthesized);
        assert_eq!(classify_epistemic_status(true, true, true, true), EpistemicStatus::Verified);
        // Monotonic progression as pillars are added
        assert!(EpistemicStatus::Seed < classify_epistemic_status(true, true, false, false));
        assert!(EpistemicStatus::Defined < classify_epistemic_status(true, true, true, false));
        assert!(EpistemicStatus::Synthesized < classify_epistemic_status(true, true, true, true));
        assert_eq!(EpistemicStatus::Verified.as_str(), "verified");
    }

    #[test]
    fn test_validate_taxonomy_dag() {
        assert!(!validate_taxonomy_dag(&[(1, 1)]));
        assert!(validate_taxonomy_dag(&[(1, 2)]));
        assert!(!validate_taxonomy_dag(&[(1, 2), (2, 1)]));
        assert!(!validate_taxonomy_dag(&[(1, 2), (2, 3), (3, 1)]));
        assert!(validate_taxonomy_dag(&[(1, 2), (1, 3), (2, 4), (3, 4)]));
    }

    #[test]
    fn test_validate_skos_disjointness() {
        // direct overlap broader == related -> violation -> false
        assert!(!validate_skos_disjointness(&[(0, 1)], &[(0, 1)], &[]));
        // transitive violation: broader 0->1->2, related (0,2) comparable -> false
        assert!(!validate_skos_disjointness(&[(0, 1), (1, 2)], &[(0, 2)], &[]));
        // reverse transitive violation: related (2,0), 2 reachable from 0 -> false
        assert!(!validate_skos_disjointness(&[(0, 1), (1, 2)], &[(2, 0)], &[]));
        // unrelated distinct pair -> true
        assert!(validate_skos_disjointness(&[(0, 1)], &[(2, 3)], &[]));
        // empty related/contrast with broader -> true
        assert!(validate_skos_disjointness(&[(0, 1)], &[], &[]));
        // contrast overlapping -> false
        assert!(!validate_skos_disjointness(&[(0, 1)], &[], &[(0, 1)]));
        // contrast trans violation -> false
        assert!(!validate_skos_disjointness(&[(0, 1), (1, 2)], &[], &[(0, 2)]));
    }

    #[test]
    fn test_validate_skos_disjointness_cycle_guard() {
        assert!(!validate_skos_disjointness(&[(0, 1), (1, 2), (2, 0)], &[], &[]));
        assert!(!validate_skos_disjointness(&[(0, 1), (1, 0)], &[], &[]));
        assert!(!validate_skos_disjointness(&[(0, 0)], &[], &[]));
    }

    #[test]
    fn test_compute_epistemic_coverage_bps() {
        assert_eq!(compute_epistemic_coverage_bps(&[], 0), 0);
        assert_eq!(compute_epistemic_coverage_bps(&[], 5), 0);
        assert_eq!(compute_epistemic_coverage_bps(&[EpistemicStatus::Seed], 0), 2500);
        assert_eq!(compute_epistemic_coverage_bps(&[EpistemicStatus::Verified, EpistemicStatus::Verified], 0), 10000);
        assert_eq!(compute_epistemic_coverage_bps(&[EpistemicStatus::Seed, EpistemicStatus::Verified], 0), 6250);
        assert_eq!(compute_epistemic_coverage_bps(&[EpistemicStatus::Seed; 3], 0), 2500);
        assert_eq!(compute_epistemic_coverage_bps(&[EpistemicStatus::Seed, EpistemicStatus::Seed], 1), 1666);
    }

    #[test]
    fn test_realization_status_gap_bps() {
        assert_eq!(realization_status_gap_bps("unimplemented"), 10000);
        assert_eq!(realization_status_gap_bps("partial"), 7500);
        assert_eq!(realization_status_gap_bps("unknown"), 6000);
        assert_eq!(realization_status_gap_bps("implemented"), 2500);
        assert_eq!(realization_status_gap_bps("external_only"), 0);
        assert_eq!(realization_status_gap_bps("unknown_status"), 6000);
        assert_eq!(realization_status_gap_bps("UNIMPLEMENTED"), 6000);
    }

    #[test]
    fn test_realization_status_gap_bps_const_context() {
        const GAP: u32 = realization_status_gap_bps("implemented");
        assert_eq!(GAP, 2500);
    }

    #[test]
    fn test_compute_frontier_leverage_bps() {
        assert_eq!(compute_frontier_leverage_bps(5000, 0, 3, true, true), 0);
        assert_eq!(compute_frontier_leverage_bps(5000, 2500, 3, false, false), 0);
        assert_eq!(compute_frontier_leverage_bps(5000, 2500, 3, true, true), 4975);
        assert_eq!(compute_frontier_leverage_bps(20000, 20000, 3, true, true), 8600);
        assert_eq!(compute_frontier_leverage_bps(20000, 20000, 3, false, false), 6600);
    }
}
