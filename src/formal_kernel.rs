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
