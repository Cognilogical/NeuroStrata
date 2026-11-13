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
        KernelGuard::ExtractionRequired if !extraction_edge_exists => return Err(KernelTaskStepError::ExtractionRequired),
        KernelGuard::RequireReason | KernelGuard::ExtractionRequired => {}
    }

    let clear_claim = matches!((from, to), (InProgress, Open));
    Ok(KernelTaskStepOutcome { next_status: to, clear_claim })
}

/// Tag string for a task step result: "ok", "err_illegal", "err_missing_reason", "err_extraction_required".
pub const fn task_step_tag(
    from: KernelTaskStatus,
    to: KernelTaskStatus,
    has_reason: bool,
    extraction_edge_exists: bool,
) -> &'static str {
    use KernelTaskStatus::*;

    let guard = match task_edge_guard(from, to) {
        Some(g) => g,
        None => return "err_illegal",
    };

    match guard {
        KernelGuard::None => "ok",
        KernelGuard::RequireReason if !has_reason => "err_missing_reason",
        KernelGuard::ExtractionRequired if !extraction_edge_exists => "err_extraction_required",
        KernelGuard::RequireReason | KernelGuard::ExtractionRequired => "ok",
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
pub const fn lifecycle_can_transition(from: KernelLifecycleState, to: KernelLifecycleState) -> bool {
    use KernelLifecycleState::*;
    match (from, to) {
        (Active, Archived) => true,
        (Archived, Active) => true,
        (Active, Superseded) => true,
        (Archived, Superseded) => true,
        (Active, Tombstoned) => true,
        (Archived, Tombstoned) => true,
        (Superseded, Tombstoned) => true,
        _ => false,
    }
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