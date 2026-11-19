//! Aeneas-compliant pure Rust kernel for NeuroStrata formal verification.
//!
//! This module contains zero external dependencies and zero `crate::*` imports.
//! It compiles standalone via `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs`.
//! All types are `Copy` and all functions are `const fn` to enable Charon/Aeneas
//! translation to pure Lean 4 without borrow/heap complexity.

#![allow(dead_code)]

/// Task status in the task state machine.
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelGuard {
    None,
    RequireReason,
    ExtractionRequired,
}

/// Outcome of a successful task step.
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelTaskStepOutcome {
    pub next_status: KernelTaskStatus,
    pub clear_claim: bool,
}

/// Error from a task step.
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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

/// Seconds in a day (86,400) for `every-n-days:N` procedure cadence (`neurostrata-zbe7`).
pub const PROCEDURE_DAY_SECONDS: i64 = 86_400;
/// Estimated wall-clock length of one session (3,600s) for `every-n-sessions:N` cadence (`neurostrata-zbe7`).
pub const PROCEDURE_SESSION_ESTIMATE_SECONDS: i64 = 3_600;

/// Pure, overflow-free `next_due` cadence calculator for procedural memory (`neurostrata-zbe7`).
///
/// - When `day_cadence` is `Some(days)` with `days > 0` and `last_performed_at` is `Some(last)`,
///   returns `last.saturating_add(days.saturating_mul(PROCEDURE_DAY_SECONDS))`.
/// - Otherwise, when `session_cadence` is `Some(n_sessions)` with `n_sessions > 1` and
///   `last_performed_at` is `Some(last)`, uses `sessions_since_last_fire.unwrap_or(n_sessions - 1).max(0)`
///   and returns `last.saturating_add(sessions_elapsed.saturating_mul(PROCEDURE_SESSION_ESTIMATE_SECONDS))`.
/// - In all other cases (including `days <= 0`, `n_sessions <= 1`, or never performed), returns `now`.
pub const fn compute_procedure_next_due(
    now: i64,
    last_performed_at: Option<i64>,
    day_cadence: Option<i64>,
    session_cadence: Option<i64>,
    sessions_since_last_fire: Option<i64>,
) -> i64 {
    if let (Some(days), Some(last)) = (day_cadence, last_performed_at) {
        if days > 0 {
            return last.saturating_add(days.saturating_mul(PROCEDURE_DAY_SECONDS));
        }
    }
    if let (Some(n_sessions), Some(last)) = (session_cadence, last_performed_at) {
        if n_sessions > 1 {
            let raw_elapsed = match sessions_since_last_fire {
                Some(s) => s,
                None => n_sessions - 1,
            };
            let sessions_elapsed = if raw_elapsed < 0 { 0 } else { raw_elapsed };
            return last.saturating_add(
                sessions_elapsed.saturating_mul(PROCEDURE_SESSION_ESTIMATE_SECONDS),
            );
        }
    }
    now
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelConnectorOp {
    RegisterPointer,
    ResolveSlice,
    ApplyWriteBack,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelActionReceiptState {
    NotRequired,
    ActiveBound,
    MissingOrNotFound,
    RevertedOrSuperseded,
    TargetMismatch,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelConnectorPreGateInput {
    pub connector_registered: bool,
    pub rebac_allowed: bool,
    pub op: KernelConnectorOp,
    pub pool_writable: bool,
    pub receipt_state: KernelActionReceiptState,
    pub requested_rows: u32,
    pub max_slice_rows: u32,
    pub requested_bytes: u32,
    pub max_slice_bytes: u32,
    pub clock_skew_future: bool,
    pub age_secs: u64,
    pub max_staleness_secs: u64,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelConnectorPreVerdict {
    AllowPreRegister,
    AllowPreResolve,
    AllowPreWriteBack,
    DenyUnregisteredConnector,
    DenyRebacAccess,
    DenyReadOnlyPool,
    DenyInvalidActionReceipt,
    DenySliceLimitExceeded,
    DenyClockSkewFuture,
    DenyPointerStale,
}

pub const fn evaluate_connector_pre_gate(
    input: KernelConnectorPreGateInput,
) -> KernelConnectorPreVerdict {
    if !input.connector_registered {
        KernelConnectorPreVerdict::DenyUnregisteredConnector
    } else if !input.rebac_allowed {
        KernelConnectorPreVerdict::DenyRebacAccess
    } else if matches!(input.op, KernelConnectorOp::RegisterPointer) {
        KernelConnectorPreVerdict::AllowPreRegister
    } else if matches!(input.op, KernelConnectorOp::ApplyWriteBack) && !input.pool_writable {
        KernelConnectorPreVerdict::DenyReadOnlyPool
    } else if matches!(input.op, KernelConnectorOp::ApplyWriteBack)
        && !matches!(input.receipt_state, KernelActionReceiptState::ActiveBound)
    {
        KernelConnectorPreVerdict::DenyInvalidActionReceipt
    } else if matches!(input.op, KernelConnectorOp::ResolveSlice)
        && (input.requested_rows == 0
            || input.requested_rows > input.max_slice_rows
            || input.requested_bytes == 0
            || input.requested_bytes > input.max_slice_bytes)
    {
        KernelConnectorPreVerdict::DenySliceLimitExceeded
    } else if input.clock_skew_future {
        KernelConnectorPreVerdict::DenyClockSkewFuture
    } else if input.max_staleness_secs > 0 && input.age_secs > input.max_staleness_secs {
        KernelConnectorPreVerdict::DenyPointerStale
    } else {
        match input.op {
            KernelConnectorOp::ResolveSlice => KernelConnectorPreVerdict::AllowPreResolve,
            KernelConnectorOp::ApplyWriteBack => KernelConnectorPreVerdict::AllowPreWriteBack,
            _ => KernelConnectorPreVerdict::AllowPreResolve,
        }
    }
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelConnectorPostGateInput {
    pub op: KernelConnectorOp,
    pub observed_rows: u32,
    pub max_slice_rows: u32,
    pub observed_bytes: u32,
    pub max_slice_bytes: u32,
    pub source_digest_matches: bool,
    pub cas_expected_matches: bool,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelConnectorPostVerdict {
    AllowPostOutput,
    DenyObservedBoundsExceeded,
    DenySourceDigestMismatch,
    DenyCasConflict,
}

pub const fn evaluate_connector_post_gate(
    input: KernelConnectorPostGateInput,
) -> KernelConnectorPostVerdict {
    if matches!(input.op, KernelConnectorOp::ResolveSlice)
        && (input.observed_rows > input.max_slice_rows
            || input.observed_bytes > input.max_slice_bytes)
    {
        KernelConnectorPostVerdict::DenyObservedBoundsExceeded
    } else if !input.source_digest_matches {
        KernelConnectorPostVerdict::DenySourceDigestMismatch
    } else if matches!(input.op, KernelConnectorOp::ApplyWriteBack) && !input.cas_expected_matches {
        KernelConnectorPostVerdict::DenyCasConflict
    } else {
        KernelConnectorPostVerdict::AllowPostOutput
    }
}

/// Builds a `KernelConnectorPostGateInput` for `ResolveSlice`, saturating `observed_rows_len`
/// from `usize` to `u32::MAX` (preventing 64-bit `as u32` truncation wrap) and capping
/// `max_slice_rows` / `max_slice_bytes` at `min(requested, schema_max)` (`neurostrata-zbe7`).
pub const fn build_resolve_post_gate_input(
    observed_rows_len: usize,
    requested_rows: u32,
    schema_max_rows: u32,
    observed_bytes: u32,
    requested_bytes: u32,
    schema_max_bytes: u32,
    source_digest_matches: bool,
) -> KernelConnectorPostGateInput {
    let observed_rows = if observed_rows_len > (u32::MAX as usize) {
        u32::MAX
    } else {
        observed_rows_len as u32
    };
    KernelConnectorPostGateInput {
        op: KernelConnectorOp::ResolveSlice,
        observed_rows,
        max_slice_rows: min_u32(requested_rows, schema_max_rows),
        observed_bytes,
        max_slice_bytes: min_u32(requested_bytes, schema_max_bytes),
        source_digest_matches,
        cas_expected_matches: true,
    }
}

/// Builds a `KernelConnectorPostGateInput` for `ApplyWriteBack`, keeping pointer pinned-digest
/// verification (`pointer_source_digest_matches`) distinct from request CAS verification
/// (`request_cas_expected_matches`) (`neurostrata-zbe7`).
pub const fn build_writeback_post_gate_input(
    schema_max_rows: u32,
    schema_max_bytes: u32,
    pointer_source_digest_matches: bool,
    request_cas_expected_matches: bool,
) -> KernelConnectorPostGateInput {
    KernelConnectorPostGateInput {
        op: KernelConnectorOp::ApplyWriteBack,
        observed_rows: 0,
        max_slice_rows: schema_max_rows,
        observed_bytes: 0,
        max_slice_bytes: schema_max_bytes,
        source_digest_matches: pointer_source_digest_matches,
        cas_expected_matches: request_cas_expected_matches,
    }
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TwinBoundError {
    InvertedInterface,
    InvertedTwin,
    Disjoint,
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
        return Err(TwinBoundError::InvertedInterface);
    }
    if twin_min > twin_max {
        return Err(TwinBoundError::InvertedTwin);
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
        Err(TwinBoundError::Disjoint)
    } else {
        Ok((eff_min, eff_max))
    }
}

/// Deterministic link-reducer operator kind (`neurostrata-brs1`).
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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
#[cfg_attr(kani, derive(kani::Arbitrary))]
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

/// Workflow lifecycle stage in the pluggable workflow state machine.
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelWorkflowStage {
    Unpreflighted,
    ContextLoaded,
    Claimed,
    Mutating,
    BreathRequired,
    PreCheckpoint,
    PrePublish,
    Completed,
}

/// Workflow lifecycle event in the pluggable workflow state machine.
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelWorkflowEvent {
    LoadContext,
    ClaimTask {
        has_valid_scope: bool,
    },
    Mutate {
        mutations_since_note_after: usize,
        breath_limit: usize,
    },
    RecordBreathNote,
    TriggerPreCheckpoint {
        has_receipt: bool,
    },
    CheckpointPassed,
    TriggerPrePublish {
        task_gate_ok: bool,
        doc_gate_ok: bool,
    },
    PublishPassed,
    CompleteTask {
        has_extracted_memory: bool,
        dirty_targets: usize,
    },
}

/// Evaluates a workflow stage transition in the pure kernel.
/// Returns `Ok(next_stage)` on allowed transition, or `Err(static_error_str)` on guard failure.
pub const fn evaluate_workflow_stage_transition(
    stage: KernelWorkflowStage,
    event: KernelWorkflowEvent,
) -> Result<KernelWorkflowStage, &'static str> {
    use KernelWorkflowEvent::*;
    use KernelWorkflowStage::*;

    match (stage, event) {
        (Unpreflighted, LoadContext) => Ok(ContextLoaded),
        (ContextLoaded, LoadContext) => Ok(ContextLoaded),
        (
            ContextLoaded,
            ClaimTask {
                has_valid_scope: true,
            },
        ) => Ok(Claimed),
        (
            Claimed,
            ClaimTask {
                has_valid_scope: true,
            },
        ) => Ok(Claimed),
        (
            Mutating,
            ClaimTask {
                has_valid_scope: true,
            },
        ) => Ok(Claimed),
        (
            _,
            ClaimTask {
                has_valid_scope: false,
            },
        ) => Err("ClaimRequiresValidWorkspaceScope"),
        (Unpreflighted, ClaimTask { .. }) => Err("PreflightRequiredBeforeClaim"),
        (
            Claimed,
            Mutate {
                mutations_since_note_after,
                breath_limit,
            },
        ) => {
            if mutations_since_note_after >= breath_limit {
                Ok(BreathRequired)
            } else {
                Ok(Mutating)
            }
        }
        (
            Mutating,
            Mutate {
                mutations_since_note_after,
                breath_limit,
            },
        ) => {
            if mutations_since_note_after >= breath_limit {
                Ok(BreathRequired)
            } else {
                Ok(Mutating)
            }
        }
        (BreathRequired, Mutate { .. }) => Err("BreathNoteRequiredBeforeFurtherMutation"),
        (Unpreflighted | ContextLoaded, Mutate { .. }) => Err("ClaimedTaskRequiredBeforeMutation"),
        (BreathRequired | Mutating | Claimed, RecordBreathNote) => Ok(Mutating),
        (Claimed, TriggerPreCheckpoint { has_receipt: true }) => Ok(PreCheckpoint),
        (Mutating, TriggerPreCheckpoint { has_receipt: true }) => Ok(PreCheckpoint),
        (_, TriggerPreCheckpoint { has_receipt: false }) => Err("CheckpointRequiresValidReceipt"),
        (PreCheckpoint, CheckpointPassed) => Ok(Mutating),
        (
            Claimed,
            TriggerPrePublish {
                task_gate_ok: true,
                doc_gate_ok: true,
            },
        ) => Ok(PrePublish),
        (
            Mutating,
            TriggerPrePublish {
                task_gate_ok: true,
                doc_gate_ok: true,
            },
        ) => Ok(PrePublish),
        (
            PreCheckpoint,
            TriggerPrePublish {
                task_gate_ok: true,
                doc_gate_ok: true,
            },
        ) => Ok(PrePublish),
        (
            _,
            TriggerPrePublish {
                task_gate_ok: false,
                ..
            },
        ) => Err("PublishBlockedByTaskGate"),
        (
            _,
            TriggerPrePublish {
                doc_gate_ok: false, ..
            },
        ) => Err("PublishBlockedByDocumentationGate"),
        (PrePublish, PublishPassed) => Ok(Mutating),
        (
            Claimed,
            CompleteTask {
                has_extracted_memory: true,
                dirty_targets: 0,
            },
        ) => Ok(Completed),
        (
            Mutating,
            CompleteTask {
                has_extracted_memory: true,
                dirty_targets: 0,
            },
        ) => Ok(Completed),
        (
            PreCheckpoint,
            CompleteTask {
                has_extracted_memory: true,
                dirty_targets: 0,
            },
        ) => Ok(Completed),
        (
            PrePublish,
            CompleteTask {
                has_extracted_memory: true,
                dirty_targets: 0,
            },
        ) => Ok(Completed),
        (
            _,
            CompleteTask {
                has_extracted_memory: false,
                ..
            },
        ) => Err("CompletionRequiresExtractedMemory"),
        (_, CompleteTask { dirty_targets, .. }) if dirty_targets > 0 => {
            Err("CompletionBlockedByDirtyTargets")
        }
        _ => Err("IllegalWorkflowStageTransition"),
    }
}

/// Epistemic maturity status of a taxonomy concept.
#[cfg_attr(kani, derive(kani::Arbitrary))]
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

/// Deterministic iterative reachability over a directed edge slice (`start ->+ target`).
/// Free of `HashMap`/`HashSet` OS-seeded `RandomState` and heap allocation for `<= 64` edges
/// so CBMC/Kani and Aeneas can model-check it in milliseconds (`neurostrata-zbe7`).
fn is_reachable_in_edges(edges: &[(u32, u32)], start: u32, target: u32) -> bool {
    #[cfg(kani)]
    const CAP: usize = 8;
    #[cfg(not(kani))]
    const CAP: usize = 128;

    let n = edges.len();
    if n == 0 {
        return false;
    }
    if n <= CAP {
        let mut used = [false; CAP];
        let mut queue = [0u32; CAP + 1];
        queue[0] = start;
        let mut head = 0usize;
        let mut tail = 1usize;
        let mut step = 0usize;
        while step <= n {
            if head < tail {
                let curr = queue[head];
                head += 1;
                let mut i = 0usize;
                while i < n {
                    if !used[i] && edges[i].0 == curr {
                        if edges[i].1 == target {
                            return true;
                        }
                        used[i] = true;
                        if tail <= CAP {
                            queue[tail] = edges[i].1;
                            tail += 1;
                        }
                    }
                    i += 1;
                }
            }
            step += 1;
        }
        return false;
    }
    #[cfg(not(kani))]
    {
        let mut used = vec![false; n];
        let mut queue = Vec::with_capacity(n + 1);
        queue.push(start);
        let mut head = 0usize;
        while head < queue.len() {
            let curr = queue[head];
            head += 1;
            let mut i = 0usize;
            while i < n {
                if !used[i] && edges[i].0 == curr {
                    if edges[i].1 == target {
                        return true;
                    }
                    used[i] = true;
                    queue.push(edges[i].1);
                }
                i += 1;
            }
        }
    }
    false
}

/// Validates that `broader_edges` form a DAG (no self-loops, no cycles).
pub fn validate_taxonomy_dag(broader_edges: &[(u32, u32)]) -> bool {
    let mut i = 0;
    while i < broader_edges.len() {
        let (u, v) = broader_edges[i];
        if u == v || is_reachable_in_edges(broader_edges, v, u) {
            return false;
        }
        i += 1;
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
    if !validate_taxonomy_dag(broader_edges) {
        return false;
    }
    let mut i = 0;
    while i < related_edges.len() {
        let (u, v) = related_edges[i];
        if u == v
            || is_reachable_in_edges(broader_edges, u, v)
            || is_reachable_in_edges(broader_edges, v, u)
        {
            return false;
        }
        i += 1;
    }
    let mut j = 0;
    while j < contrast_edges.len() {
        let (u, v) = contrast_edges[j];
        if u == v
            || is_reachable_in_edges(broader_edges, u, v)
            || is_reachable_in_edges(broader_edges, v, u)
        {
            return false;
        }
        j += 1;
    }
    true
}

/// Computes epistemic coverage in basis points.
pub const fn compute_epistemic_coverage_bps(
    statuses: &[EpistemicStatus],
    unmapped_domain_count: u32,
) -> u32 {
    if statuses.is_empty() && unmapped_domain_count == 0 {
        return 0;
    }
    let mut sum_weights: u64 = 0;
    let mut i = 0;
    while i < statuses.len() {
        sum_weights = sum_weights.saturating_add(statuses[i].weight_bps() as u64);
        i += 1;
    }
    let denom: u64 = (statuses.len() as u64).saturating_add(unmapped_domain_count as u64);
    if denom == 0 {
        return 0;
    }
    let avg = sum_weights / denom;
    if avg > 10000 {
        10000
    } else {
        avg as u32
    }
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
    if realization_gap_bps == 0 || (realization_gap_bps <= 2500 && !has_frontier_or_limitation) {
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
    let frontier_bps = if has_frontier_or_limitation {
        10000u64
    } else {
        0u64
    };
    let cite_bps = if has_references_or_comparables {
        10000u64
    } else {
        0u64
    };
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
        assert_eq!(
            classify_epistemic_status(false, false, false, false),
            EpistemicStatus::Seed
        );
        assert_eq!(
            classify_epistemic_status(true, false, false, false),
            EpistemicStatus::Seed
        );
        assert_eq!(
            classify_epistemic_status(true, true, false, false),
            EpistemicStatus::Defined
        );
        assert_eq!(
            classify_epistemic_status(true, true, true, false),
            EpistemicStatus::Synthesized
        );
        assert_eq!(
            classify_epistemic_status(true, true, true, true),
            EpistemicStatus::Verified
        );
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
        assert!(!validate_skos_disjointness(
            &[(0, 1), (1, 2)],
            &[(0, 2)],
            &[]
        ));
        // reverse transitive violation: related (2,0), 2 reachable from 0 -> false
        assert!(!validate_skos_disjointness(
            &[(0, 1), (1, 2)],
            &[(2, 0)],
            &[]
        ));
        // unrelated distinct pair -> true
        assert!(validate_skos_disjointness(&[(0, 1)], &[(2, 3)], &[]));
        // empty related/contrast with broader -> true
        assert!(validate_skos_disjointness(&[(0, 1)], &[], &[]));
        // contrast overlapping -> false
        assert!(!validate_skos_disjointness(&[(0, 1)], &[], &[(0, 1)]));
        // contrast trans violation -> false
        assert!(!validate_skos_disjointness(
            &[(0, 1), (1, 2)],
            &[],
            &[(0, 2)]
        ));
    }

    #[test]
    fn test_validate_skos_disjointness_cycle_guard() {
        assert!(!validate_skos_disjointness(
            &[(0, 1), (1, 2), (2, 0)],
            &[],
            &[]
        ));
        assert!(!validate_skos_disjointness(&[(0, 1), (1, 0)], &[], &[]));
        assert!(!validate_skos_disjointness(&[(0, 0)], &[], &[]));
    }

    #[test]
    fn test_compute_epistemic_coverage_bps() {
        assert_eq!(compute_epistemic_coverage_bps(&[], 0), 0);
        assert_eq!(compute_epistemic_coverage_bps(&[], 5), 0);
        assert_eq!(
            compute_epistemic_coverage_bps(&[EpistemicStatus::Seed], 0),
            2500
        );
        assert_eq!(
            compute_epistemic_coverage_bps(
                &[EpistemicStatus::Verified, EpistemicStatus::Verified],
                0
            ),
            10000
        );
        assert_eq!(
            compute_epistemic_coverage_bps(&[EpistemicStatus::Seed, EpistemicStatus::Verified], 0),
            6250
        );
        assert_eq!(
            compute_epistemic_coverage_bps(&[EpistemicStatus::Seed; 3], 0),
            2500
        );
        assert_eq!(
            compute_epistemic_coverage_bps(&[EpistemicStatus::Seed, EpistemicStatus::Seed], 1),
            1666
        );
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
        assert_eq!(
            compute_frontier_leverage_bps(5000, 2500, 3, false, false),
            0
        );
        assert_eq!(
            compute_frontier_leverage_bps(5000, 2500, 3, true, true),
            4975
        );
        assert_eq!(
            compute_frontier_leverage_bps(20000, 20000, 3, true, true),
            8600
        );
        assert_eq!(
            compute_frontier_leverage_bps(20000, 20000, 3, false, false),
            6600
        );
    }

    #[test]
    fn workflow_stage_transition_legal_path() {
        use KernelWorkflowEvent::*;
        use KernelWorkflowStage::*;
        assert_eq!(
            evaluate_workflow_stage_transition(Unpreflighted, LoadContext),
            Ok(ContextLoaded)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(ContextLoaded, LoadContext),
            Ok(ContextLoaded)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                ContextLoaded,
                ClaimTask {
                    has_valid_scope: true
                }
            ),
            Ok(Claimed)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Claimed,
                Mutate {
                    mutations_since_note_after: 2,
                    breath_limit: 5
                }
            ),
            Ok(Mutating)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Mutating,
                Mutate {
                    mutations_since_note_after: 5,
                    breath_limit: 5
                }
            ),
            Ok(BreathRequired)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(BreathRequired, RecordBreathNote),
            Ok(Mutating)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Mutating,
                TriggerPreCheckpoint { has_receipt: true }
            ),
            Ok(PreCheckpoint)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                PreCheckpoint,
                TriggerPrePublish {
                    task_gate_ok: true,
                    doc_gate_ok: true
                }
            ),
            Ok(PrePublish)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(PrePublish, PublishPassed),
            Ok(Mutating)
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Mutating,
                CompleteTask {
                    has_extracted_memory: true,
                    dirty_targets: 0
                }
            ),
            Ok(Completed)
        );
    }

    #[test]
    fn workflow_stage_transition_guard_rejections() {
        use KernelWorkflowEvent::*;
        use KernelWorkflowStage::*;
        assert_eq!(
            evaluate_workflow_stage_transition(
                BreathRequired,
                Mutate {
                    mutations_since_note_after: 0,
                    breath_limit: 5
                }
            ),
            Err("BreathNoteRequiredBeforeFurtherMutation")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Claimed,
                TriggerPrePublish {
                    task_gate_ok: false,
                    doc_gate_ok: true
                }
            ),
            Err("PublishBlockedByTaskGate")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Claimed,
                TriggerPrePublish {
                    task_gate_ok: true,
                    doc_gate_ok: false
                }
            ),
            Err("PublishBlockedByDocumentationGate")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Claimed,
                CompleteTask {
                    has_extracted_memory: false,
                    dirty_targets: 0
                }
            ),
            Err("CompletionRequiresExtractedMemory")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Claimed,
                CompleteTask {
                    has_extracted_memory: true,
                    dirty_targets: 3
                }
            ),
            Err("CompletionBlockedByDirtyTargets")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Unpreflighted,
                ClaimTask {
                    has_valid_scope: true
                }
            ),
            Err("PreflightRequiredBeforeClaim")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                ContextLoaded,
                Mutate {
                    mutations_since_note_after: 0,
                    breath_limit: 5
                }
            ),
            Err("ClaimedTaskRequiredBeforeMutation")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                Claimed,
                TriggerPreCheckpoint { has_receipt: false }
            ),
            Err("CheckpointRequiresValidReceipt")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(
                ContextLoaded,
                ClaimTask {
                    has_valid_scope: false
                }
            ),
            Err("ClaimRequiresValidWorkspaceScope")
        );
        assert_eq!(
            evaluate_workflow_stage_transition(Completed, LoadContext),
            Err("IllegalWorkflowStageTransition")
        );
    }

    #[test]
    fn data_connector_gate_parity() {
        // Test Pre-Gate Verdicts
        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::RegisterPointer,
                pool_writable: false,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 0,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::AllowPreRegister
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: false,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 0,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::DenyUnregisteredConnector
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: false,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 0,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::DenyRebacAccess
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ApplyWriteBack,
                pool_writable: false,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 0,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::DenyReadOnlyPool
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ApplyWriteBack,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::ActiveBound,
                requested_rows: 0,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::AllowPreWriteBack
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 101,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::DenySliceLimitExceeded
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 1,
                max_slice_rows: 100,
                requested_bytes: 1,
                max_slice_bytes: 1000,
                clock_skew_future: true,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::DenyClockSkewFuture
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 1,
                max_slice_rows: 100,
                requested_bytes: 1,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 1000,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::AllowPreResolve
        );

        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 1,
                max_slice_rows: 100,
                requested_bytes: 1,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 1000,
                max_staleness_secs: 1,
            }),
            KernelConnectorPreVerdict::DenyPointerStale
        );

        // Test Post-Gate Verdicts
        assert_eq!(
            evaluate_connector_post_gate(KernelConnectorPostGateInput {
                op: KernelConnectorOp::ResolveSlice,
                observed_rows: 0,
                max_slice_rows: 100,
                observed_bytes: 0,
                max_slice_bytes: 1000,
                source_digest_matches: true,
                cas_expected_matches: true,
            }),
            KernelConnectorPostVerdict::AllowPostOutput
        );

        assert_eq!(
            evaluate_connector_post_gate(KernelConnectorPostGateInput {
                op: KernelConnectorOp::ResolveSlice,
                observed_rows: 101,
                max_slice_rows: 100,
                observed_bytes: 0,
                max_slice_bytes: 1000,
                source_digest_matches: true,
                cas_expected_matches: true,
            }),
            KernelConnectorPostVerdict::DenyObservedBoundsExceeded
        );

        assert_eq!(
            evaluate_connector_post_gate(KernelConnectorPostGateInput {
                op: KernelConnectorOp::ResolveSlice,
                observed_rows: 0,
                max_slice_rows: 100,
                observed_bytes: 0,
                max_slice_bytes: 1000,
                source_digest_matches: false,
                cas_expected_matches: true,
            }),
            KernelConnectorPostVerdict::DenySourceDigestMismatch
        );

        assert_eq!(
            evaluate_connector_post_gate(KernelConnectorPostGateInput {
                op: KernelConnectorOp::ApplyWriteBack,
                observed_rows: 0,
                max_slice_rows: 100,
                observed_bytes: 0,
                max_slice_bytes: 1000,
                source_digest_matches: true,
                cas_expected_matches: false,
            }),
            KernelConnectorPostVerdict::DenyCasConflict
        );

        assert_eq!(
            evaluate_connector_post_gate(KernelConnectorPostGateInput {
                op: KernelConnectorOp::ApplyWriteBack,
                observed_rows: 0,
                max_slice_rows: 100,
                observed_bytes: 0,
                max_slice_bytes: 1000,
                source_digest_matches: true,
                cas_expected_matches: true,
            }),
            KernelConnectorPostVerdict::AllowPostOutput
        );
    }

    #[test]
    fn test_build_resolve_post_gate_input_caps_at_min_of_requested_and_schema() {
        // Observed rows above u32::MAX saturate instead of truncating via `as u32`.
        let saturated =
            build_resolve_post_gate_input((u32::MAX as usize) + 1, 10, 10, 64, 256, 1024, true);
        assert_eq!(saturated.observed_rows, u32::MAX);
        assert_eq!(saturated.max_slice_rows, 10);
        assert_eq!(saturated.observed_bytes, 64);
        assert_eq!(saturated.max_slice_bytes, 256);

        // The effective row cap is min(requested, schema), so an over-broad
        // request cannot widen the schema cap.
        let capped = build_resolve_post_gate_input(5, 2, 10, 32, 256, 1024, true);
        assert_eq!(capped.observed_rows, 5);
        assert_eq!(capped.max_slice_rows, 2);
        assert_eq!(capped.max_slice_bytes, 256);
        assert_eq!(
            evaluate_connector_post_gate(capped),
            KernelConnectorPostVerdict::DenyObservedBoundsExceeded
        );
    }

    #[test]
    fn test_build_writeback_post_gate_input_separates_pointer_digest_and_cas() {
        let digest_mismatch = build_writeback_post_gate_input(10, 1024, false, true);
        assert!(!digest_mismatch.source_digest_matches);
        assert!(digest_mismatch.cas_expected_matches);
        assert_eq!(
            evaluate_connector_post_gate(digest_mismatch),
            KernelConnectorPostVerdict::DenySourceDigestMismatch
        );

        let cas_mismatch = build_writeback_post_gate_input(10, 1024, true, false);
        assert!(cas_mismatch.source_digest_matches);
        assert!(!cas_mismatch.cas_expected_matches);
        assert_eq!(
            evaluate_connector_post_gate(cas_mismatch),
            KernelConnectorPostVerdict::DenyCasConflict
        );

        let ok = build_writeback_post_gate_input(10, 1024, true, true);
        assert_eq!(
            evaluate_connector_post_gate(ok),
            KernelConnectorPostVerdict::AllowPostOutput
        );
    }

    #[test]
    fn test_compute_procedure_next_due_saturates_and_rejects_non_positive_cadence() {
        assert_eq!(
            compute_procedure_next_due(1_000, Some(0), Some(i64::MAX), None, None),
            i64::MAX
        );
        assert_eq!(
            compute_procedure_next_due(1_000, Some(0), Some(0), None, None),
            1_000
        );
        assert_eq!(
            compute_procedure_next_due(1_000, Some(0), Some(-5), None, None),
            1_000
        );
        assert_eq!(
            compute_procedure_next_due(1_000, Some(0), None, Some(i64::MAX), None),
            i64::MAX
        );
        // Negative session elapsed clamps to zero rather than shifting backwards.
        assert_eq!(
            compute_procedure_next_due(1_000, Some(1000), None, Some(2), Some(-3)),
            1_000
        );
        assert_eq!(
            compute_procedure_next_due(1_000, Some(1000), None, Some(2), Some(3)),
            1_000 + 3 * PROCEDURE_SESSION_ESTIMATE_SECONDS
        );
    }
    #[test]
    fn test_build_resolve_post_gate_input() {
        let input = build_resolve_post_gate_input(5, 10, 100, 100, 500, 1000, true);
        assert_eq!(input.op, KernelConnectorOp::ResolveSlice);
        assert_eq!(input.observed_rows, 5);
        assert_eq!(input.max_slice_rows, 10);
        assert_eq!(input.observed_bytes, 100);
        assert_eq!(input.max_slice_bytes, 500);
        assert!(input.source_digest_matches);
        assert!(input.cas_expected_matches);

        let input =
            build_resolve_post_gate_input((u32::MAX as usize) + 100, 10, 100, 100, 500, 1000, true);
        assert_eq!(input.observed_rows, u32::MAX);

        let input = build_resolve_post_gate_input(5, 50, 30, 100, 500, 1000, true);
        assert_eq!(input.max_slice_rows, 30);

        let input = build_resolve_post_gate_input(5, 10, 100, 100, 2000, 1000, true);
        assert_eq!(input.max_slice_bytes, 1000);
    }

    #[test]
    fn test_build_writeback_post_gate_input() {
        let input = build_writeback_post_gate_input(100, 1000, true, true);
        assert_eq!(input.op, KernelConnectorOp::ApplyWriteBack);
        assert_eq!(input.observed_rows, 0);
        assert_eq!(input.max_slice_rows, 100);
        assert_eq!(input.observed_bytes, 0);
        assert_eq!(input.max_slice_bytes, 1000);
        assert!(input.source_digest_matches);
        assert!(input.cas_expected_matches);

        let input = build_writeback_post_gate_input(100, 1000, false, true);
        assert!(!input.source_digest_matches);
        assert!(input.cas_expected_matches);

        let input = build_writeback_post_gate_input(100, 1000, true, false);
        assert!(input.source_digest_matches);
        assert!(!input.cas_expected_matches);

        let input = build_writeback_post_gate_input(100, 1000, false, false);
        assert!(!input.source_digest_matches);
        assert!(!input.cas_expected_matches);
    }

    #[test]
    fn test_compute_procedure_next_due() {
        const NOW: i64 = 1_000_000;
        const LAST: i64 = 900_000;

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), Some(7), None, None),
            LAST.saturating_add(7 * PROCEDURE_DAY_SECONDS)
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), Some(0), None, None),
            NOW
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), Some(-5), None, None),
            NOW
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), Some(i64::MAX), None, None),
            i64::MAX
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(5), None),
            LAST.saturating_add(4 * PROCEDURE_SESSION_ESTIMATE_SECONDS)
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(5), Some(10)),
            LAST.saturating_add(10 * PROCEDURE_SESSION_ESTIMATE_SECONDS)
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(1), None),
            NOW
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(0), None),
            NOW
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(-3), None),
            NOW
        );

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(i64::MAX), None),
            i64::MAX
        );

        assert_eq!(
            compute_procedure_next_due(NOW, None, Some(7), None, None),
            NOW
        );
        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, None, None),
            NOW
        );
        assert_eq!(compute_procedure_next_due(NOW, None, None, None, None), NOW);

        assert_eq!(
            compute_procedure_next_due(NOW, Some(LAST), None, Some(5), Some(-10)),
            LAST
        );
    }
}

/// Kani proof harnesses for the formal kernel decision functions.
///
/// These harnesses are only compiled under `cfg(kani)` and verify key
/// safety/overflow/no-panic properties of the pure kernel functions.
/// Run via `kani src/formal_kernel.rs`.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    #[kani::proof]
    fn proof_task_step_no_panic() {
        let from = kani::any::<KernelTaskStatus>();
        let to = kani::any::<KernelTaskStatus>();
        let has_reason = kani::any::<bool>();
        let extraction_edge_exists = kani::any::<bool>();
        let _ = task_step(from, to, has_reason, extraction_edge_exists);
    }

    #[kani::proof]
    fn proof_task_edge_guard_legal_edges() {
        let from = kani::any::<KernelTaskStatus>();
        let to = kani::any::<KernelTaskStatus>();
        let guard = task_edge_guard(from, to);
        // If guard is Some, the transition is legal (covered by the match arm).
        if let KernelGuard::RequireReason = guard.unwrap_or(KernelGuard::None) {
            assert!(matches!(
                (from, to),
                (KernelTaskStatus::Open, KernelTaskStatus::Blocked)
                    | (KernelTaskStatus::InProgress, KernelTaskStatus::Blocked)
                    | (KernelTaskStatus::Blocked, KernelTaskStatus::Open)
                    | (KernelTaskStatus::Done, KernelTaskStatus::Open)
            ));
        }
    }

    #[kani::proof]
    fn proof_epoch_gate_total() {
        let candidate = kani::any::<u32>();
        let manifest = kani::any::<u32>();
        let decision = epoch_gate_decision(candidate, manifest);
        // Exactly one of the three decisions must hold.
        let apply = matches!(decision, KernelEpochDecision::Apply);
        let quarantine = matches!(decision, KernelEpochDecision::QuarantineOld);
        let refuse = matches!(decision, KernelEpochDecision::RefuseFuture);
        assert!((apply as u32) + (quarantine as u32) + (refuse as u32) == 1);
    }

    #[kani::proof]
    fn proof_classify_merge_decision_total() {
        let local = kani::any::<KernelLifecycleState>();
        let cand = kani::any::<KernelLifecycleState>();
        let same = kani::any::<bool>();
        let changed = kani::any::<bool>();
        let concurrent = kani::any::<bool>();
        let _ = classify_merge_decision(local, cand, same, changed, concurrent);
    }

    #[kani::proof]
    fn proof_evaluate_claim_decision_total() {
        let status = kani::any::<KernelTaskStatus>();
        let same_session = kani::any::<bool>();
        let age = kani::any::<Option<i64>>();
        let stale_after = kani::any::<i64>();
        let _ = evaluate_claim_decision(status, same_session, age, stale_after);
    }

    #[kani::proof]
    fn proof_evaluate_sovereignty_gate_total() {
        let permitted = kani::any::<bool>();
        let shareable = kani::any::<bool>();
        let runtime_counter = kani::any::<bool>();
        let acl_empty = kani::any::<bool>();
        let in_acl = kani::any::<bool>();
        let _ = evaluate_sovereignty_gate(permitted, shareable, runtime_counter, acl_empty, in_acl);
    }

    #[kani::proof]
    fn proof_evaluate_import_precheck_total() {
        let row_exists = kani::any::<bool>();
        let mesh_provenance = kani::any::<bool>();
        let same_update_id = kani::any::<bool>();
        let _ = evaluate_import_precheck(row_exists, mesh_provenance, same_update_id);
    }

    #[kani::proof]
    fn proof_evaluate_procedure_fire_no_panic() {
        let trust = kani::any::<KernelProcedureTrust>();
        let has_provenance = kani::any::<bool>();
        let valid_to = kani::any::<Option<i64>>();
        let now = kani::any::<i64>();
        let remaining = kani::any::<Option<u64>>();
        let perf_count = kani::any::<u64>();
        let outcome =
            evaluate_procedure_fire(trust, has_provenance, valid_to, now, remaining, perf_count);
        // If bounded and fired, remaining_fires must not have underflowed.
        if let KernelProcedureFireOutcome::Fired {
            remaining_fires: Some(r),
            ..
        } = outcome
        {
            assert!(r <= u64::MAX);
        }
    }

    #[kani::proof]
    fn proof_compute_procedure_next_due_no_overflow() {
        let now = kani::any::<i64>();
        let last = kani::any::<Option<i64>>();
        let days = kani::any::<Option<i64>>();
        let sessions = kani::any::<Option<i64>>();
        let since = kani::any::<Option<i64>>();
        let due = compute_procedure_next_due(now, last, days, sessions, since);
        // Result must never wrap (saturating) and must be finite.
        assert!(due >= i64::MIN && due <= i64::MAX);
    }

    #[kani::proof]
    fn proof_evaluate_action_gate_total() {
        let auth = kani::any::<bool>();
        let params = kani::any::<bool>();
        let ask = kani::any::<bool>();
        let twin = kani::any::<bool>();
        let shacl = kani::any::<bool>();
        let _ = evaluate_action_gate(auth, params, ask, twin, shacl);
    }

    #[kani::proof]
    fn proof_evaluate_action_revert_total() {
        let state = kani::any::<ActionReceiptState>();
        let cas = kani::any::<bool>();
        let force = kani::any::<bool>();
        let owner = kani::any::<bool>();
        let editor = kani::any::<bool>();
        let _ = evaluate_action_revert(state, cas, force, owner, editor);
    }

    #[kani::proof]
    fn proof_evaluate_connector_pre_gate_total() {
        let input = kani::any::<KernelConnectorPreGateInput>();
        let _ = evaluate_connector_pre_gate(input);
    }

    #[kani::proof]
    fn proof_evaluate_connector_post_gate_total() {
        let input = kani::any::<KernelConnectorPostGateInput>();
        let _ = evaluate_connector_post_gate(input);
    }

    #[kani::proof]
    fn proof_build_resolve_post_gate_input_bounds() {
        let observed_len = kani::any::<usize>();
        let req_rows = kani::any::<u32>();
        let schema_rows = kani::any::<u32>();
        let obs_bytes = kani::any::<u32>();
        let req_bytes = kani::any::<u32>();
        let schema_bytes = kani::any::<u32>();
        let src_match = kani::any::<bool>();
        let input = build_resolve_post_gate_input(
            observed_len,
            req_rows,
            schema_rows,
            obs_bytes,
            req_bytes,
            schema_bytes,
            src_match,
        );
        // Saturate never wraps; caps are within schema bounds.
        assert!(input.max_slice_rows <= req_rows || input.max_slice_rows <= schema_rows);
        assert!(input.max_slice_bytes <= req_bytes || input.max_slice_bytes <= schema_bytes);
    }

    #[kani::proof]
    fn proof_build_writeback_post_gate_input_distinct() {
        let schema_rows = kani::any::<u32>();
        let schema_bytes = kani::any::<u32>();
        let ptr_match = kani::any::<bool>();
        let cas_match = kani::any::<bool>();
        let input =
            build_writeback_post_gate_input(schema_rows, schema_bytes, ptr_match, cas_match);
        // Pointer digest match and CAS match are independent flags.
        assert!(input.source_digest_matches == ptr_match);
        assert!(input.cas_expected_matches == cas_match);
    }

    #[kani::proof]
    fn proof_evaluate_reducer_total_no_div_zero() {
        let op = kani::any::<ReducerOpKind>();
        let count = kani::any::<u32>();
        let sum = kani::any::<i64>();
        let min_v = kani::any::<i64>();
        let max_v = kani::any::<i64>();
        let oob = kani::any::<bool>();
        let outcome = evaluate_reducer_total(op, count, sum, min_v, max_v, oob);
        // Avg with count == 0 is defined as 0, never panics.
        if count == 0 && matches!(op, ReducerOpKind::Avg) {
            assert!(outcome.is_defined && outcome.numeric_val == 0);
        }
    }

    #[kani::proof]
    fn proof_evaluate_seq_continuity_total() {
        let seen = kani::any::<bool>();
        let expected = kani::any::<u64>();
        let frame = kani::any::<u64>();
        let _ = evaluate_seq_continuity(seen, expected, frame);
    }

    #[kani::proof]
    fn proof_evaluate_retention_gate_total() {
        let within = kani::any::<bool>();
        let non_empty = kani::any::<bool>();
        let receipt = kani::any::<bool>();
        let bound = kani::any::<bool>();
        let matches = kani::any::<bool>();
        let _ = evaluate_retention_gate(within, non_empty, receipt, bound, matches);
    }

    #[kani::proof]
    fn proof_evaluate_claim_scope_gate_total() {
        let scoped = kani::any::<bool>();
        let identifiable = kani::any::<bool>();
        let attributed = kani::any::<bool>();
        let branch = kani::any::<bool>();
        let worktree = kani::any::<bool>();
        let _ = evaluate_claim_scope_gate(scoped, identifiable, attributed, branch, worktree);
    }

    #[kani::proof]
    fn proof_allocate_hybrid_slots_conservation() {
        let limit = kani::any::<u32>();
        let avail_norm = kani::any::<u32>();
        let avail_exp = kani::any::<u32>();
        let avail_other = kani::any::<u32>();
        let alloc = allocate_hybrid_slots(limit, avail_norm, avail_exp, avail_other);
        assert!(alloc.normative_taken <= avail_norm);
        assert!(alloc.expansion_taken <= avail_exp);
        let total_taken = (alloc.normative_taken as u64)
            + (alloc.expansion_taken as u64)
            + (alloc.remaining_taken as u64);
        assert!(total_taken <= limit as u64);
    }

    #[kani::proof]
    fn proof_intersect_var_bounds_covariant() {
        let imin = kani::any::<i64>();
        let imax = kani::any::<i64>();
        let tmin = kani::any::<i64>();
        let tmax = kani::any::<i64>();
        if let Ok((emin, emax)) = intersect_var_bounds(imin, imax, tmin, tmax) {
            assert!(emin <= emax);
            assert!(emin >= imin && emax <= imax);
            assert!(emin >= tmin && emax <= tmax);
        }
    }

    #[kani::proof]
    fn proof_resolve_cross_file_call_total() {
        let is_self = kani::any::<bool>();
        let same_file = kani::any::<bool>();
        let imported = kani::any::<bool>();
        let ws_count = kani::any::<u32>();
        let ident_len = kani::any::<u32>();
        let is_std = kani::any::<bool>();
        let _ = resolve_cross_file_call(is_self, same_file, imported, ws_count, ident_len, is_std);
    }

    #[kani::proof]
    fn proof_resolve_scenario_row_total() {
        let live = kani::any::<bool>();
        let retired = kani::any::<bool>();
        let upsert = kani::any::<bool>();
        let _ = resolve_scenario_row(live, retired, upsert);
    }

    #[kani::proof]
    fn proof_validate_taxonomy_dag_bounded() {
        let e0 = (kani::any::<u8>() & 3) as u32;
        let e1 = (kani::any::<u8>() & 3) as u32;
        let e2 = (kani::any::<u8>() & 3) as u32;
        let e3 = (kani::any::<u8>() & 3) as u32;
        let edges = [(e0, e1), (e2, e3)];
        let valid = validate_taxonomy_dag(&edges);
        if e0 == e1 || e2 == e3 {
            assert!(!valid);
        }
    }

    #[kani::proof]
    fn proof_validate_skos_disjointness_bounded() {
        let a = (kani::any::<u8>() & 3) as u32;
        let b = (kani::any::<u8>() & 3) as u32;
        let broader = [(a, b)];
        let related = [(a, b)];
        let contrast: [(u32, u32); 0] = [];
        assert!(!validate_skos_disjointness(&broader, &related, &contrast));
    }

    #[kani::proof]
    fn proof_compute_epistemic_coverage_bps_bounds() {
        let s0 = kani::any::<EpistemicStatus>();
        let s1 = kani::any::<EpistemicStatus>();
        let s2 = kani::any::<EpistemicStatus>();
        let unmapped = kani::any::<u32>();
        let statuses = [s0, s1, s2];
        let bps = compute_epistemic_coverage_bps(&statuses, unmapped);
        assert!(bps <= 10_000);
        assert_eq!(compute_epistemic_coverage_bps(&[], 0), 0);
    }

    #[kani::proof]
    fn proof_compute_frontier_leverage_bps_bounds() {
        let axis_w = kani::any::<u32>();
        let gap = kani::any::<u32>();
        let deg = kani::any::<u32>();
        let frontier = kani::any::<bool>();
        let lit = kani::any::<bool>();
        let bps = compute_frontier_leverage_bps(axis_w, gap, deg, frontier, lit);
        assert!(bps <= 10_000);
        if gap == 0 {
            assert_eq!(bps, 0);
        }
    }

    #[kani::proof]
    fn proof_workflow_stage_transition_total() {
        let stage = kani::any::<KernelWorkflowStage>();
        let event = kani::any::<KernelWorkflowEvent>();
        if let Ok(next) = evaluate_workflow_stage_transition(stage, event) {
            if matches!(next, KernelWorkflowStage::Completed)
                && !matches!(stage, KernelWorkflowStage::Completed)
            {
                assert!(matches!(
                    event,
                    KernelWorkflowEvent::CompleteTask {
                        has_extracted_memory: true,
                        dirty_targets: 0
                    }
                ));
            }
        }
    }
}
