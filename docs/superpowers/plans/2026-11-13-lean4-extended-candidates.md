# Lean 4 Extended Formal Verification Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend NeuroStrata's Aeneas-compliant pure Rust formal kernel (`src/formal_kernel.rs`), Lean 4.15.0 formal proof package (`proofs/`), and Rust-to-Lean truth-table parity test (`tests/lean_parity_test.rs`) to cover 5 additional invariants:
1. Task Claim Exclusivity & Stale Lease Takeover (`TaskClaim`)
2. SynapticMesh Sovereignty & ReBAC Gate + Private-Row Import Precheck (`Sovereignty`)
3. Procedural Memory Rehearsal Budget, Lapse, & Trust Gate (`Procedure`)
4. Digital Twin LWW CRDT, Task Status Join Semilattice, & Sequence Continuity (`TwinCrdt`)
5. AC5 Receipt Retention & Latency Budget Gate (`Retention`)

**Global Constraints:**
- `src/formal_kernel.rs` MUST remain zero-dependency (`core`/`std` primitives only) and compile standalone via `rustc -D warnings --edition=2021 --crate-type=lib src/formal_kernel.rs` (`bash proofs/scripts/extract-aeneas.sh`).
- Zero external Lean dependencies (`Init` stdlib only, no Mathlib).
- Zero `sorry` or `axiom` declarations anywhere in `proofs/`.
- `lake --dir proofs exe neurostrata-proofs` and `tests/lean_parity_test.rs` must match 1-for-1 across all 389 truth-table rows (`211` existing + `178` new = `389`).

---

### Task 1: Extend `src/formal_kernel.rs` & Wire Production Modules for the 5 New Domains

**Files:**
- Modify: `src/formal_kernel.rs`
- Modify: `src/task/mod.rs`
- Modify: `src/synaptic_mesh/frame.rs`
- Modify: `src/synaptic_mesh/import.rs`
- Modify: `src/tools/procedure.rs`
- Modify: `src/synaptic_mesh/reactor.rs`
- Modify: `src/retention/gate.rs`

**Requirements:**
1. In `src/formal_kernel.rs`, add pure `const fn` / zero-dependency definitions and tag helpers for all 5 domains:
   - **Domain 1 (`TaskClaim`):**
     ```rust
     #[derive(Clone, Copy, Debug, PartialEq, Eq)]
     pub enum KernelClaimDecision {
         IdempotentSameSession,
         RefuseLiveHolder,
         TakeoverStaleHolder,
         ClaimFromOpen,
         DelegateToFsm,
     }

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

     pub const fn claim_decision_tag(d: KernelClaimDecision) -> &'static str {
         match d {
             KernelClaimDecision::IdempotentSameSession => "idempotent_same_session",
             KernelClaimDecision::RefuseLiveHolder => "refuse_live_holder",
             KernelClaimDecision::TakeoverStaleHolder => "takeover_stale_holder",
             KernelClaimDecision::ClaimFromOpen => "claim_from_open",
             KernelClaimDecision::DelegateToFsm => "delegate_to_fsm",
         }
     }
     ```
   - **Domain 2 (`Sovereignty` — Egress/Ingress Gate & Import Precheck):**
     ```rust
     #[derive(Clone, Copy, Debug, PartialEq, Eq)]
     pub enum KernelSovereigntyDecision {
         Allow,
         ForbiddenMemoryType,
         NotShareable,
         ForbiddenRuntimeCounter,
         UnauthorizedRecipient,
     }

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

     pub const fn sovereignty_decision_tag(d: KernelSovereigntyDecision) -> &'static str {
         match d {
             KernelSovereigntyDecision::Allow => "allow",
             KernelSovereigntyDecision::ForbiddenMemoryType => "forbidden_memory_type",
             KernelSovereigntyDecision::NotShareable => "not_shareable",
             KernelSovereigntyDecision::ForbiddenRuntimeCounter => "forbidden_runtime_counter",
             KernelSovereigntyDecision::UnauthorizedRecipient => "unauthorized_recipient",
         }
     }

     #[derive(Clone, Copy, Debug, PartialEq, Eq)]
     pub enum KernelImportPrecheck {
         FreshInsert,
         SkipSameUpdateId,
         RefusePrivateSovereign,
         ProceedMeshMerge,
     }

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

     pub const fn import_precheck_tag(d: KernelImportPrecheck) -> &'static str {
         match d {
             KernelImportPrecheck::FreshInsert => "fresh_insert",
             KernelImportPrecheck::SkipSameUpdateId => "skip_same_update_id",
             KernelImportPrecheck::RefusePrivateSovereign => "refuse_private_sovereign",
             KernelImportPrecheck::ProceedMeshMerge => "proceed_mesh_merge",
         }
     }
     ```
   - **Domain 3 (`Procedure` — Trust, Lapse, and Rehearsal Budget):**
     ```rust
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

     pub const fn procedure_is_lapsed(valid_to: Option<i64>, now: i64) -> bool {
         match valid_to {
             Some(deadline) => deadline <= now,
             None => false,
         }
     }

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
     ```
   - **Domain 4 (`TwinCrdt` — Task Status Join Semilattice, Sequence Continuity, & LWW Cell):**
     ```rust
     pub const fn task_status_rank(status: KernelTaskStatus) -> u8 {
         match status {
             KernelTaskStatus::Done => 3,
             KernelTaskStatus::InProgress => 2,
             KernelTaskStatus::Blocked => 1,
             KernelTaskStatus::Open => 0,
         }
     }

     pub const fn join_task_status(a: KernelTaskStatus, b: KernelTaskStatus) -> KernelTaskStatus {
         if task_status_rank(a) >= task_status_rank(b) {
             a
         } else {
             b
         }
     }

     #[derive(Clone, Copy, Debug, PartialEq, Eq)]
     pub enum KernelSeqDecision {
         DuplicateIgnore,
         ApplyAndAdvance,
         BufferOutOfOrder,
     }

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

     pub const fn seq_decision_tag(d: KernelSeqDecision) -> &'static str {
         match d {
             KernelSeqDecision::DuplicateIgnore => "duplicate_ignore",
             KernelSeqDecision::ApplyAndAdvance => "apply_and_advance",
             KernelSeqDecision::BufferOutOfOrder => "buffer_out_of_order",
         }
     }
     ```
   - **Domain 5 (`Retention` — AC5 Receipt & Latency Budget Gate):**
     ```rust
     #[derive(Clone, Copy, Debug, PartialEq, Eq)]
     pub enum KernelRetentionVerdict {
         Retained,
         FailedOverBudget,
         FailedStoreEmpty,
         FailedReceiptMissing,
         FailedWorktreeMoved,
     }

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

     pub const fn retention_verdict_tag(v: KernelRetentionVerdict) -> &'static str {
         match v {
             KernelRetentionVerdict::Retained => "retained",
             KernelRetentionVerdict::FailedOverBudget => "failed_over_budget",
             KernelRetentionVerdict::FailedStoreEmpty => "failed_store_empty",
             KernelRetentionVerdict::FailedReceiptMissing => "failed_receipt_missing",
             KernelRetentionVerdict::FailedWorktreeMoved => "failed_worktree_moved",
         }
     }
     ```
2. Wire production callers to delegate to `crate::formal_kernel`:
   - `src/task/mod.rs` (`handle_task_claim`): call `crate::formal_kernel::evaluate_claim_decision(...)` to classify `IdempotentSameSession` / `RefuseLiveHolder` / `TakeoverStaleHolder` / `ClaimFromOpen` / `DelegateToFsm` while preserving all existing logging, error messages, and JSON replies.
   - `src/synaptic_mesh/frame.rs` (`validate_egress_candidate`): delegate eligibility + memory_type check through `crate::formal_kernel::evaluate_sovereignty_gate`.
   - `src/synaptic_mesh/import.rs` (`apply_one`): call `crate::formal_kernel::evaluate_import_precheck` for the `FreshInsert` / `SkipSameUpdateId` / `RefusePrivateSovereign` / `ProceedMeshMerge` discrimination.
   - `src/tools/procedure.rs` (`is_untrusted`, `is_lapsed`): delegate to `crate::formal_kernel::{procedure_is_untrusted, procedure_is_lapsed}`.
   - `src/synaptic_mesh/reactor.rs` (`task_status_rank`): delegate to `crate::formal_kernel::task_status_rank`.
   - `src/retention/gate.rs` (`retention_gate_with`): delegate boolean check to `matches!(crate::formal_kernel::evaluate_retention_gate(budget_verified(p95_plan_ms, p95_upsert_ms), true, receipt_exists, false, true), crate::formal_kernel::KernelRetentionVerdict::Retained)`.
3. Verify:
   - `bash proofs/scripts/extract-aeneas.sh` (compiles `src/formal_kernel.rs` standalone with `-D warnings`)
   - `cargo test --bin neurostrata-mcp task::`
   - `cargo test --bin neurostrata-mcp synaptic_mesh::`
   - `cargo test --bin neurostrata-mcp tools::procedure::`
   - `cargo test --bin neurostrata-mcp retention::`

---

### Task 2: Add Lean 4 Proof Modules for the 5 New Domains (`proofs/NeuroStrata/`)

**Files:**
- Create: `proofs/NeuroStrata/TaskClaim.lean`
- Create: `proofs/NeuroStrata/Sovereignty.lean`
- Create: `proofs/NeuroStrata/Procedure.lean`
- Create: `proofs/NeuroStrata/TwinCrdt.lean`
- Create: `proofs/NeuroStrata/Retention.lean`
- Modify: `proofs/NeuroStrata.lean`

**Requirements:**
1. Create `proofs/NeuroStrata/TaskClaim.lean` (`namespace NeuroStrata.TaskClaim`):
   - Import `NeuroStrata.TaskMachine` (reusing `Status`).
   - Define `inductive ClaimDecision | idempotentSameSession | refuseLiveHolder | takeoverStaleHolder | claimFromOpen | delegateToFsm deriving DecidableEq, Repr`, `ClaimDecision.asTag`, and `@[simp] def evaluateClaim (status : TaskMachine.Status) (sameSession : Bool) (ageSecs : Option Int) (staleAfterSecs : Int) : ClaimDecision` matching `src/formal_kernel.rs::evaluate_claim_decision`.
   - Prove 4 theorems (zero `sorry`/`axiom`):
     - `same_session_in_progress_idempotent`: `∀ (age : Option Int) (stale : Int), evaluateClaim .inProgress true age stale = .idempotentSameSession`
     - `live_holder_exclusivity`: `∀ (age stale : Int), age ≤ stale → evaluateClaim .inProgress false (some age) stale = .refuseLiveHolder`
     - `stale_holder_recoverable`: `∀ (age stale : Int), stale < age → evaluateClaim .inProgress false (some age) stale = .takeoverStaleHolder`
     - `missing_timestamp_recoverable`: `∀ (stale : Int), evaluateClaim .inProgress false none stale = .takeoverStaleHolder`
2. Create `proofs/NeuroStrata/Sovereignty.lean` (`namespace NeuroStrata.Sovereignty`):
   - Define `SovereigntyDecision`, `SovereigntyDecision.asTag`, `evaluateSovereigntyGate`, `ImportPrecheck`, `ImportPrecheck.asTag`, and `evaluateImportPrecheck` matching `src/formal_kernel.rs`.
   - Prove 4 theorems (zero `sorry`/`axiom`):
     - `private_never_egresses`: `∀ (perm counter aclEmpty inAcl : Bool), evaluateSovereigntyGate perm false counter aclEmpty inAcl ≠ .allow`
     - `runtime_counters_never_cross_wire`: `∀ (perm shareable aclEmpty inAcl : Bool), evaluateSovereigntyGate perm shareable true aclEmpty inAcl ≠ .allow`
     - `unlisted_recipient_blocked_when_acl_nonempty`: `∀ (perm shareable counter : Bool), evaluateSovereigntyGate perm shareable counter false false ≠ .allow`
     - `private_local_row_never_overwritten`: `∀ (sameUpd : Bool), evaluateImportPrecheck true false sameUpd = .refusePrivateSovereign`
3. Create `proofs/NeuroStrata/Procedure.lean` (`namespace NeuroStrata.Procedure`):
   - Define `ProcedureTrust`, `ProcedureTrust.asStr`, `isUntrusted`, `isLapsed`, `ProcedureFireOutcome`, `ProcedureFireOutcome.asTag`, and `evaluateProcedureFire` matching `src/formal_kernel.rs`.
   - Prove 4 theorems (zero `sorry`/`axiom`):
     - `imported_unstamped_never_fires`: `∀ (vt : Option Int) (now : Int) (b : Option Nat) (c : Nat), evaluateProcedureFire .absentOrUnknown true vt now b c = .refuseUntrusted`
     - `lapsed_procedure_never_mutates`: `∀ (vt now : Int) (b : Option Nat) (c : Nat), vt ≤ now → evaluateProcedureFire .local false (some vt) now b c = .refuseLapsed`
     - `spent_procedure_never_fires`: `∀ (now : Int) (c : Nat), evaluateProcedureFire .local false none now (some 0) c = .refuseSpent`
     - `bounded_fire_conserves_budget`: `∀ (now : Int) (b c : Nat), 0 < b → evaluateProcedureFire .local false none now (some b) c = .fired (some (b - 1)) (c + 1) ∧ (b - 1) + (c + 1) = b + c`
4. Create `proofs/NeuroStrata/TwinCrdt.lean` (`namespace NeuroStrata.TwinCrdt`):
   - Import `NeuroStrata.TaskMachine` (reusing `Status`).
   - Define `statusRank`, `joinStatus`, `SeqDecision`, `SeqDecision.asTag`, `evaluateSeqContinuity`, and `TwinCell` (`value : Int`, `lamport : Nat`, `origin : Nat`) + `mergeTwinCell` matching `src/formal_kernel.rs`.
   - Prove 4 theorems (zero `sorry`/`axiom`):
     - `joinStatus_idempotent`: `∀ (s : TaskMachine.Status), joinStatus s s = s`
     - `joinStatus_commutative`: `∀ (a b : TaskMachine.Status), joinStatus a b = joinStatus b a`
     - `joinStatus_associative`: `∀ (a b c : TaskMachine.Status), joinStatus (joinStatus a b) c = joinStatus a (joinStatus b c)`
     - `done_is_top_of_status_lattice`: `∀ (s : TaskMachine.Status), joinStatus .done s = .done ∧ joinStatus s .done = .done`
     - `duplicate_or_old_seq_never_advances`: `∀ (exp seq : Nat), evaluateSeqContinuity true exp seq = .duplicateIgnore`
5. Create `proofs/NeuroStrata/Retention.lean` (`namespace NeuroStrata.Retention`):
   - Define `RetentionVerdict`, `RetentionVerdict.asTag`, and `evaluateRetentionGate` matching `src/formal_kernel.rs`.
   - Prove 4 theorems (zero `sorry`/`axiom`):
     - `over_budget_always_rejects`: `∀ (ne rf wb wm : Bool), evaluateRetentionGate false ne rf wb wm = .failedOverBudget`
     - `missing_receipt_never_passes`: `∀ (wb wm : Bool), evaluateRetentionGate true true false wb wm = .failedReceiptMissing`
     - `moved_worktree_invalidates_bound_receipt`: `evaluateRetentionGate true true true true false = .failedWorktreeMoved`
     - `retained_iff_all_guards_hold`: `∀ (b ne rf wb wm : Bool), evaluateRetentionGate b ne rf wb wm = .retained ↔ (b = true ∧ ne = true ∧ rf = true ∧ (wb = false ∨ wm = true))`
6. Register all 5 modules in `proofs/NeuroStrata.lean` and verify `~/.elan/bin/lake --dir proofs build` succeeds with zero warnings and zero `sorry`/`axiom`.

---

### Task 3: Extend `proofs/Main.lean` and `tests/lean_parity_test.rs` to 389-Row Truth-Table Parity

**Files:**
- Modify: `proofs/Main.lean`
- Modify: `tests/lean_parity_test.rs`

**Requirements:**
1. Extend `proofs/Main.lean` and `tests/lean_parity_test.rs::generate_rust_truth_table` in identical canonical order after the existing 211 rows:
   - `TASK_CLAIM|<status>|<same_session>|<age_case>|<tag>` (24 rows):
     - `status` in `[open, in_progress, blocked, done]` (4)
     - `same_session` in `[false, true]` (2)
     - `age_case` in `[("none", None), ("live", Some(1800)), ("stale", Some(3601))]` against `stale_after_secs = 3600` (3)
     - `4 * 2 * 3 = 24` rows.
   - `SOVEREIGNTY_GATE|<perm>|<shareable>|<counter>|<acl_empty>|<in_acl>|<tag>` (32 rows):
     - All 5 booleans in `[false, true]` -> `2^5 = 32` rows.
   - `IMPORT_PRECHECK|<exists>|<has_prov>|<same_upd>|<tag>` (8 rows):
     - All 3 booleans in `[false, true]` -> `2^3 = 8` rows.
   - `PROCEDURE_FIRE|<trust>|<imported>|<lapsed_case>|<budget_case>|<tag>` (60 rows):
     - `trust` in `[untrusted, trusted, promoted, local, absent]` (5)
     - `imported` in `[false, true]` (2)
     - `lapsed_case` (`now = 1000`) in `[("live", None), ("lapsed", Some(1000))]` (2)
     - `budget_case` (`count = 0`) in `[("unbounded", None), ("zero", Some(0)), ("two", Some(2))]` (3)
     - `5 * 2 * 2 * 3 = 60` rows.
   - `TWIN_CRDT_STATUS|<status_a>|<status_b>|<joined_status>` (16 rows):
     - `status_a` in `[open, in_progress, blocked, done]` (4)
     - `status_b` in `[open, in_progress, blocked, done]` (4)
     - `4 * 4 = 16` rows.
   - `TWIN_CRDT_SEQ|<already_seen>|<seq_case>|<tag>` (9 rows — wait: `2 * 3 = 6` rows! Let's use `already_seen` in `[false, true]` (2) and `seq` in `[1, 2, 3]` against `expected_seq = 2` (3) = `6` rows, so total new rows = `24 + 32 + 8 + 60 + 16 + 6 + 32 = 178` rows, total = `211 + 178 = 389` rows!).
   - `RETENTION_GATE|<budget>|<non_empty>|<found>|<bound>|<matches>|<tag>` (32 rows):
     - All 5 booleans in `[false, true]` -> `2^5 = 32` rows.
2. Update `tests/lean_parity_test.rs` section count assertions (`total = 389`: `64 TASK_FSM`, `16 LIFECYCLE`, `128 MESH_MERGE`, `3 EPOCH_GATE`, `24 TASK_CLAIM`, `32 SOVEREIGNTY_GATE`, `8 IMPORT_PRECHECK`, `60 PROCEDURE_FIRE`, `16 TWIN_CRDT_STATUS`, `6 TWIN_CRDT_SEQ`, `32 RETENTION_GATE`) and pin explicit assertions for key rows from each new domain.
3. Run `~/.elan/bin/lake --dir proofs build && cargo test --test lean_parity_test && cargo clippy --test lean_parity_test -- -D warnings`.
