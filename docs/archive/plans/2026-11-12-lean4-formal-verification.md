# Lean 4 Formal Verification Suite, Aeneas Kernel & SynapticMesh Merge Bugfix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the SynapticMesh `classify_merge` `Superseded` + `Archived` content-edit silent-drop bug, extract a zero-dependency Charon/Aeneas-compliant pure Rust kernel (`src/formal_kernel.rs`) called by production state machines, build the Lean 4 formal verification package (`proofs/`), and pin Rust-to-Lean parity plus standalone Aeneas extractability in `tests/lean_parity_test.rs`.

**Architecture:** Following the Aeneas functional translation methodology (https://lean-lang.org/use-cases/aeneas/) and NeuroStrata's crate-free module pattern (`src/fts/mod.rs`), we factor out the pure algebraic decision logic into `src/formal_kernel.rs` (zero external dependencies, compilable standalone via `rustc --crate-type=lib` and `charon` -> `aeneas -backend lean`). Production modules (`src/task/machine.rs`, `src/task/gate.rs`, `src/synaptic_mesh/{record,import}.rs`) delegate their pure transitions to `crate::formal_kernel`. A zero-Mathlib Lean 4 Lake package in `proofs/` proves all invariants over the functional models and emits canonical truth tables (`neurostrata-proofs`), which `tests/lean_parity_test.rs` compares against `src/formal_kernel.rs` while enforcing standalone `rustc` compilation of `src/formal_kernel.rs`.

**Tech Stack:** Rust (2021 edition, Charon/Aeneas safe subset in `src/formal_kernel.rs`), Lean 4 (`leanprover/lean4:v4.15.0` via `elan` / `lake`, `Init` stdlib only).

**Spec:** `docs/superpowers/specs/2026-11-12-lean4-formal-verification-design.md`

## Global Constraints

- `src/formal_kernel.rs` must have zero external crate imports and zero `crate::*` imports, compiling standalone via `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs` for Charon/Aeneas extraction.
- Zero external Lean dependencies (no Mathlib; `Init` stdlib only) so `lake build` finishes in seconds offline.
- Zero `sorry` or `axiom` declarations anywhere in `proofs/`.
- `cargo build` and `cargo test` must not require network access.
- All tests running against LadybugDB or shared state use `--test-threads=1` when running the full suite.
- Pre-commit hook hygiene: call `neurostrata_memories_for_file` before editing/creating any tracked file.

## Review Focus

1. **Archived + Superseded without content change:** `(Superseded, Archived, content_changed = false)` must still return `MergeDecision::Refuse("superseded; rejected stale restore")` (`KernelMergeDecision::RefuseStaleRestore`), and `(Archived, Superseded, content_changed = false)` must still return `MergeDecision::Apply` (`KernelMergeDecision::Apply`) — only `content_changed = true` triggers `SupersedeWinsPreserveEdit`.
2. **Whitespace-only reason strings in TaskMachine:** `ctx.reason = "   "` trims to empty in `src/task/machine.rs:177` and passes `has_reason = false` into `formal_kernel::task_step`.
3. **Retired task with `valid_to == now` boundary:** `is_retired_at(Some(now), now)` treats `valid_to == now` as retired (`true`), while `is_retired_at(None, now)` stays live (`false`).
4. **Cyclic `superseded_by` chain in `live_successor`:** A 2-node cycle (`A -> B -> A`) where both rows are retired must return `None` (`none`) rather than hanging or returning a retired node.
5. **Domain Registry self-loop (`slug == parent`):** A domain whose `parent` is `Some(self.slug)` passes `parentDeclared` (since `self.slug` is declared) and must be caught specifically by `reachesRoot` cycle detection.

---

### Task 1: Fix `classify_merge` `Superseded` + `Archived` Edit Preservation & Expose Formal Parity Helper

**Files:**
- Modify: `src/synaptic_mesh/import.rs:1193-1227`
- Test: `src/synaptic_mesh/import.rs` (unit tests module)

**Interfaces:**
- Consumes: `crate::synaptic_mesh::record::{LifecycleState, MeshRecord}`
- Produces:
  - `pub fn classify_merge_parity_tag(local_state: LifecycleState, cand_state: LifecycleState, same_superseded_by: bool, content_changed: bool, concurrent: bool) -> &'static str` in `src/synaptic_mesh/import.rs`
  - Repaired `classify_merge` routing `(Superseded, Archived, content_changed = true)` and `(Archived, Superseded, content_changed = true)` to `MergeDecision::SupersedeWinsPreserveEdit`

- [x] **Step 1: Write the failing unit tests in `src/synaptic_mesh/import.rs`**
- [x] **Step 2: Run the unit tests to verify they fail**
- [x] **Step 3: Implement the `classify_merge` fix and `classify_merge_parity_tag` in `src/synaptic_mesh/import.rs`**
- [x] **Step 4: Run the unit tests to verify they pass**
- [x] **Step 5: Commit (`1bf4989`)**

---

### Task 2: Extract Aeneas-Compliant Pure Rust Kernel (`src/formal_kernel.rs`), Wire Callers & Add `proofs/scripts/extract-aeneas.sh`

**Files:**
- Create: `src/formal_kernel.rs`
- Create: `proofs/scripts/extract-aeneas.sh`
- Modify: `src/main.rs`
- Modify: `src/task/machine.rs`
- Modify: `src/task/gate.rs`
- Modify: `src/synaptic_mesh/record.rs`
- Modify: `src/synaptic_mesh/import.rs`

**Interfaces:**
- Consumes: Pure algebraic state rules of `task::machine`, `task::gate`, `synaptic_mesh::record`, `synaptic_mesh::import`
- Produces in `src/formal_kernel.rs` (zero external dependencies, compilable standalone via `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs`):
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KernelTaskStatus { Open, InProgress, Blocked, Done }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KernelGuard { None, RequireReason, ExtractionRequired }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct KernelTaskStepOutcome { pub next_status: KernelTaskStatus, pub clear_claim: bool }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KernelTaskStepError { Illegal, MissingReason, ExtractionRequired }`
  - `pub const fn task_edge_guard(from: KernelTaskStatus, to: KernelTaskStatus) -> Option<KernelGuard>`
  - `pub const fn task_step(from: KernelTaskStatus, to: KernelTaskStatus, has_reason: bool, extraction_edge_exists: bool) -> Result<KernelTaskStepOutcome, KernelTaskStepError>`
  - `pub const fn task_step_tag(from: KernelTaskStatus, to: KernelTaskStatus, has_reason: bool, extraction_edge_exists: bool) -> &'static str` (`"ok"`, `"err_illegal"`, `"err_missing_reason"`, `"err_extraction_required"`)
  - `pub const fn is_retired_at(valid_to: Option<i64>, now: i64) -> bool`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KernelLifecycleState { Active, Archived, Superseded, Tombstoned }`
  - `pub const fn lifecycle_can_transition(from: KernelLifecycleState, to: KernelLifecycleState) -> bool`
  - `pub const fn lifecycle_is_terminal(state: KernelLifecycleState) -> bool`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KernelEpochDecision { Apply, QuarantineOld, RefuseFuture }`
  - `pub const fn epoch_gate_decision(candidate_epoch: u32, manifest_epoch: u32) -> KernelEpochDecision`
  - `pub const fn epoch_gate_tag(candidate_epoch: u32, manifest_epoch: u32) -> &'static str` (`"apply"`, `"quarantine_old"`, `"refuse_future"`)
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KernelMergeDecision { Apply, ApplyCombinationArchiveEdit, Skip, RefuseTombstoneWins, RefuseStaleRestore, RefuseIllegalTransition, ConflictTwoReplacements, ConflictRestoreArchive, ConflictIncompatibleEdits, SupersedeWinsPreserveEdit }`
  - `pub const fn classify_merge_decision(local_state: KernelLifecycleState, cand_state: KernelLifecycleState, same_superseded_by: bool, content_changed: bool, concurrent: bool) -> KernelMergeDecision`
  - `pub const fn classify_merge_decision_tag(d: KernelMergeDecision) -> &'static str`

- [ ] **Step 1: Create `src/formal_kernel.rs` and wire `pub mod formal_kernel;` in `src/main.rs`**

Write `src/formal_kernel.rs` using only `core`/`std` primitives (`Copy` enums/structs, `const fn`, pattern matching) so Charon/Aeneas can translate every function directly to pure Lean 4 without borrow/heap complexity.

- [ ] **Step 2: Delegate production state machine decisions to `crate::formal_kernel`**

1. In `src/task/machine.rs`: map `Status <-> KernelTaskStatus` and delegate the transition/guard check and `clear_claim` check in `apply` to `crate::formal_kernel::task_step`.
2. In `src/task/gate.rs`: delegate `is_retired` to `crate::formal_kernel::is_retired_at(metadata.get("valid_to").and_then(Value::as_i64), now)`.
3. In `src/synaptic_mesh/record.rs`: map `LifecycleState <-> KernelLifecycleState` and delegate `can_transition_to` and `is_terminal` to `crate::formal_kernel::{lifecycle_can_transition, lifecycle_is_terminal}`.
4. In `src/synaptic_mesh/import.rs`: delegate `epoch_gate`, `classify_merge`, and `classify_merge_parity_tag` to `crate::formal_kernel::{epoch_gate_decision, classify_merge_decision, classify_merge_decision_tag}`.

- [ ] **Step 3: Create `proofs/scripts/extract-aeneas.sh` (executable)**

Create `proofs/scripts/extract-aeneas.sh` (`chmod +x`) that:
1. Verifies standalone compilation of `src/formal_kernel.rs` with bare `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs -o /tmp/opencode/libformal_kernel.rlib`.
2. When `charon` and `aeneas` are present (or `--podman` is passed), runs Charon LLBC extraction and `aeneas -backend lean` to generate Lean 4 definitions from `src/formal_kernel.rs`.

- [ ] **Step 4: Verify standalone `rustc` kernel check and Cargo unit tests**

Run: `bash proofs/scripts/extract-aeneas.sh && cargo test --bin neurostrata-mcp task::machine && cargo test --bin neurostrata-mcp task::gate && cargo test --bin neurostrata-mcp synaptic_mesh`
Expected: PASS (standalone `rustc` exits 0 with zero warnings; all `task::machine`, `task::gate`, and `synaptic_mesh` unit tests pass).

- [ ] **Step 5: Commit**

```bash
git add src/formal_kernel.rs src/main.rs src/task/machine.rs src/task/gate.rs src/synaptic_mesh/record.rs src/synaptic_mesh/import.rs proofs/scripts/extract-aeneas.sh
git commit -m "feat(formal-kernel): extract Aeneas-compliant pure Rust kernel and wire state machines (neurostrata-ngd9)"
```

---

### Task 3: Bootstrap Lean 4 Lake Package & Prove `TaskMachine` and `TaskGate` Invariants

**Files:**
- Create: `proofs/lean-toolchain`
- Create: `proofs/lakefile.lean`
- Create: `proofs/NeuroStrata/TaskMachine.lean`
- Create: `proofs/NeuroStrata/TaskGate.lean`

**Interfaces:**
- Consumes: `src/formal_kernel.rs` (`task_step`, `is_retired_at`), Spec §4.1 (`TaskMachine`) and §4.2 (`TaskGate`)
- Produces:
  - `NeuroStrata.TaskMachine`: `Status`, `Guard`, `Transition`, `table`, `Ctx`, `TaskState`, `TransitionError`, `apply`, `applyTag`, and 5 machine-checked theorems (`no_self_transitions`, `lock2_soundness`, `done_only_reopens_to_open_with_reason`, `block_requires_reason`, `unclaim_clears_claim_exclusively`)
  - `NeuroStrata.TaskGate`: `Row`, `isRetired`, `liveSuccessor`, `workLineage`, `extractionExists`, `obligationCarried`, `hasDoneWithoutExtractionViolation`, and 6 machine-checked theorems (`isRetired_boundary_inclusive`, `isRetired_none_stays_live`, `liveSuccessor_never_retired`, `liveSuccessor_cycle_safe`, `workLineage_contains_root`, `lock2_gate_conservation`)

- [ ] **Step 1: Ensure `elan` and Lean `v4.15.0` toolchain are installed**

Run: `command -v lake || ~/.elan/bin/lake --version || (curl -sSf https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh | sh -s -- -y --default-toolchain leanprover/lean4:v4.15.0)`
Expected: `~/.elan/bin/lake --version` prints `Lake version 5.0.0... (Lean version 4.15.0)`.

- [ ] **Step 2: Create `proofs/lean-toolchain` and `proofs/lakefile.lean`**
- [ ] **Step 3: Implement `proofs/NeuroStrata/TaskMachine.lean` and `proofs/NeuroStrata/TaskGate.lean`**
- [ ] **Step 4: Compile both modules with Lean 4 kernel and verify zero `sorry`/`axiom`**

Run: `~/.elan/bin/lake --dir proofs env lean proofs/NeuroStrata/TaskMachine.lean && ~/.elan/bin/lake --dir proofs env lean proofs/NeuroStrata/TaskGate.lean && ! grep -rnE '\b(sorry|axiom)\b' proofs/NeuroStrata/`
Expected: Exit code `0` with zero warnings and zero matches for `sorry` or `axiom`.

- [ ] **Step 5: Commit**

```bash
git add proofs/lean-toolchain proofs/lakefile.lean proofs/NeuroStrata/TaskMachine.lean proofs/NeuroStrata/TaskGate.lean
git commit -m "feat(proofs): add Lean 4 TaskMachine and TaskGate formal proofs (neurostrata-ngd9)"
```

---

### Task 4: Prove `SynapticMesh` and `DomainRegistry` Invariants & Build Truth-Table CLI

**Files:**
- Create: `proofs/NeuroStrata/SynapticMesh.lean`
- Create: `proofs/NeuroStrata/DomainRegistry.lean`
- Create: `proofs/NeuroStrata.lean`
- Create: `proofs/Main.lean`

**Interfaces:**
- Consumes: `NeuroStrata.TaskMachine`, `NeuroStrata.TaskGate`, Spec §4.3 (`SynapticMesh`) and §4.4 (`DomainRegistry`)
- Produces:
  - `NeuroStrata.SynapticMesh`: `LifecycleState`, `canTransitionTo`, `isTerminal`, `EpochDecision`, `epochGate`, `MergeDecision`, `classifyMerge`, `DispositionClass`, `ResolutionOutcome`, `resolveOutcome`, and 7 theorems (`tombstone_absorbing`, `superseded_never_reactivates`, `tombstone_dominates_merge`, `no_silent_edit_drop_on_supersede`, `archive_edit_commutativity`, `classifyMerge_concurrent_symmetry`, `epochGate_trichotomy`)
  - `NeuroStrata.DomainRegistry`: `DomainEntry`, `slugsUnique`, `parentDeclared`, `reachesRoot`, `allReachRoot`, `noAliasCollisions`, `validateDomainEntries`, and 4 theorems (`reachesRoot_rejects_self_loop`, `valid_registry_has_declared_parents`, `valid_registry_acyclic`, `valid_registry_disjoint_identifiers`)
  - `Main.lean`: executable `neurostrata-proofs` printing 211 deterministic truth-table lines:
    - `TASK_FSM|<from>|<to>|<has_reason>|<has_extraction>|<outcome_tag>` (64 lines)
    - `LIFECYCLE|<from>|<to>|<bool>` (16 lines)
    - `MESH_MERGE|<local>|<cand>|<same_sup>|<content_changed>|<concurrent>|<tag>` (128 lines)
    - `EPOCH_GATE|<cand_epoch>|<manifest_epoch>|<tag>` (3 lines: `1|2`, `2|2`, `3|2`)

- [ ] **Step 1: Implement `proofs/NeuroStrata/SynapticMesh.lean` and `proofs/NeuroStrata/DomainRegistry.lean`**
- [ ] **Step 2: Implement `proofs/NeuroStrata.lean` and `proofs/Main.lean`**
- [ ] **Step 3: Build and run `lake exe neurostrata-proofs` and check for `sorry`/`axiom`**

Run: `~/.elan/bin/lake --dir proofs build && ~/.elan/bin/lake --dir proofs exe neurostrata-proofs | wc -l && ! grep -rnE '\b(sorry|axiom)\b' proofs/`
Expected: `lake build` succeeds with zero warnings, output line count is `211`, and `grep` finds zero `sorry`/`axiom` tokens.

- [ ] **Step 4: Commit**

```bash
git add proofs/NeuroStrata/SynapticMesh.lean proofs/NeuroStrata/DomainRegistry.lean proofs/NeuroStrata.lean proofs/Main.lean
git commit -m "feat(proofs): add SynapticMesh and DomainRegistry proofs and truth-table CLI (neurostrata-ngd9)"
```

---

### Task 5: Add Exhaustive Rust-to-Lean & Aeneas Kernel Extractability Integration Test (`tests/lean_parity_test.rs`)

**Files:**
- Create: `tests/lean_parity_test.rs`
- Modify: `Cargo.toml` (register `[[test]] name = "lean_parity_test" path = "tests/lean_parity_test.rs"`)

**Interfaces:**
- Consumes:
  - `#[path = "../src/formal_kernel.rs"] mod formal_kernel;` (compiled directly from source path like `tests/fts/lexical_test.rs`)
  - `proofs/scripts/extract-aeneas.sh`
  - `proofs/` Lake package (`lake --dir proofs exe neurostrata-proofs`)
- Produces:
  - Integration test suite `tests/lean_parity_test.rs` verifying:
    1. `formal_kernel_compiles_standalone_for_aeneas_extraction`: runs `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs` with zero `--extern` flags to guarantee `src/formal_kernel.rs` stays 100% Charon/Aeneas-extractable.
    2. `rust_formal_kernel_truth_table_matches_lean_cli`: generates all 211 canonical lines (`64 TASK_FSM + 16 LIFECYCLE + 128 MESH_MERGE + 3 EPOCH_GATE`) from `formal_kernel` and asserts exact line-for-line equality with `lake --dir proofs exe neurostrata-proofs` stdout.
    3. `lean_proof_sources_contain_no_sorry_or_axiom`: scans `proofs/**/*.lean` and asserts zero `sorry` or `axiom` declarations.

- [ ] **Step 1: Write `tests/lean_parity_test.rs` and register `[[test]]` in `Cargo.toml`**
- [ ] **Step 2: Run `cargo test --test lean_parity_test`**

Run: `cargo test --test lean_parity_test`
Expected: PASS (all 3 tests pass, including standalone `rustc` Aeneas check and live Lean CLI parity).

- [ ] **Step 3: Run full workspace verification gates**

Run: `cargo clippy --all-targets && cargo test -- --test-threads=1`
Expected: PASS with zero new warnings and all tests green.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml tests/lean_parity_test.rs
git commit -m "test(parity): add Aeneas kernel standalone check and Lean 4 truth-table parity test (neurostrata-ngd9)"
```
