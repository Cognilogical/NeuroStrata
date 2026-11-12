# Lean 4 Formal Verification Suite & SynapticMesh Merge Bugfix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the SynapticMesh `classify_merge` `Superseded` + `Archived` content-edit silent-drop bug, implement the standalone Lean 4 formal verification package (`proofs/`) proving core invariants across `TaskMachine`, `TaskGate`, `SynapticMesh`, and `DomainRegistry`, and pin Rust-to-Lean parity in `tests/lean_parity_test.rs`.

**Architecture:** We keep `cargo build` fast and offline-safe by placing a zero-Mathlib Lean 4 Lake package in `proofs/` that models the exact finite state machines of `src/task/machine.rs`, `src/task/gate.rs`, `src/synaptic_mesh/{record,import}.rs`, and `src/registry.rs`. An executable target (`neurostrata-proofs`) emits canonical truth tables over the finite input spaces, and `tests/lean_parity_test.rs` asserts both in-process Rust truth-table parity and live `lake exe neurostrata-proofs` output identity plus zero `sorry`/`axiom` tokens.

**Tech Stack:** Rust (2021 edition), Lean 4 (`leanprover/lean4:v4.15.0` via `elan` / `lake`, `Init` stdlib only).

**Spec:** `docs/superpowers/specs/2026-11-12-lean4-formal-verification-design.md`

## Global Constraints

- Zero external Lean dependencies (no Mathlib; `Init` stdlib only) so `lake build` finishes in seconds offline.
- Zero `sorry` or `axiom` declarations anywhere in `proofs/`.
- `cargo build` and `cargo test` must not require network access.
- All tests running against LadybugDB or shared state use `--test-threads=1` when running the full suite.
- Pre-commit hook hygiene: call `neurostrata_memories_for_file` before editing/creating any tracked file.

## Review Focus

1. **Archived + Superseded without content change:** `(Superseded, Archived, content_changed = false)` must still return `MergeDecision::Refuse("superseded; rejected stale restore")`, and `(Archived, Superseded, content_changed = false)` must still return `MergeDecision::Apply` — only `content_changed = true` triggers `SupersedeWinsPreserveEdit`.
2. **Whitespace-only reason strings in TaskMachine:** `ctx.reason = "   "` trims to empty in `src/task/machine.rs:177` and must be treated as `hasReason = false` in `tests/lean_parity_test.rs`.
3. **Retired task with `valid_to == now` boundary:** `is_retired` treats `valid_to == now` as retired (`<=`), while `valid_to == None` stays live (`false`).
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

- [ ] **Step 1: Write the failing unit tests in `src/synaptic_mesh/import.rs`**

Add unit tests in `src/synaptic_mesh/import.rs` (`mod tests`) asserting:
1. `classify_merge(LifecycleState::Superseded, Some(u1), "orig", &archived_cand_with_content("edited"), true) == MergeDecision::SupersedeWinsPreserveEdit`
2. `classify_merge(LifecycleState::Archived, None, "edited", &superseded_cand_with_content("orig", u1), true) == MergeDecision::SupersedeWinsPreserveEdit`
3. `classify_merge(LifecycleState::Superseded, Some(u1), "same", &archived_cand_with_content("same"), true) == MergeDecision::Refuse("superseded; rejected stale restore".to_string())` (Review Focus #1)
4. `classify_merge(LifecycleState::Archived, None, "same", &superseded_cand_with_content("same", u1), true) == MergeDecision::Apply` (Review Focus #1)
5. `classify_merge_parity_tag` maps all 6 `MergeDecision` variants and their payload strings to the 10 canonical tags:
   - `"apply"`, `"apply_combination_archive_edit"`, `"skip"`, `"refuse_tombstone_wins"`, `"refuse_stale_restore"`, `"refuse_illegal_transition"`, `"conflict_two_replacements"`, `"conflict_restore_archive"`, `"conflict_incompatible_edits"`, `"supersede_wins_preserve_edit"`.

- [ ] **Step 2: Run the unit tests to verify they fail**

Run: `cargo test --lib synaptic_mesh::import::tests::supersede_and_archived`
Expected: FAIL (`classify_merge_parity_tag` not found / `Refuse("superseded; rejected stale restore")` instead of `SupersedeWinsPreserveEdit`).

- [ ] **Step 3: Implement the `classify_merge` fix and `classify_merge_parity_tag` in `src/synaptic_mesh/import.rs`**

In `src/synaptic_mesh/import.rs`:
1. At line 1206, replace `Active if content_changed => MergeDecision::SupersedeWinsPreserveEdit,` with `Active | Archived if content_changed => MergeDecision::SupersedeWinsPreserveEdit,`.
2. At line 1222, replace `if local_state == Active && content_changed {` with `if matches!(local_state, Active | Archived) && content_changed {`.
3. Add `pub fn classify_merge_parity_tag(local_state: LifecycleState, cand_state: LifecycleState, same_superseded_by: bool, content_changed: bool, concurrent: bool) -> &'static str` that constructs a synthetic `MeshRecord` with `cand_state`, `superseded_by` matching or differing from `local_superseded_by` according to `same_superseded_by`, and content matching or differing from `"base"` according to `content_changed`, calls `classify_merge`, and returns the canonical tag string.

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test --lib synaptic_mesh::import`
Expected: PASS (all `synaptic_mesh::import` unit tests pass).

- [ ] **Step 5: Commit**

```bash
git add src/synaptic_mesh/import.rs
git commit -m "fix(synaptic-mesh): preserve concurrent edits on archived records when superseded (neurostrata-ngd9)"
```

---

### Task 2: Bootstrap Lean 4 Lake Package & Prove `TaskMachine` and `TaskGate` Invariants

**Files:**
- Create: `proofs/lean-toolchain`
- Create: `proofs/lakefile.lean`
- Create: `proofs/NeuroStrata/TaskMachine.lean`
- Create: `proofs/NeuroStrata/TaskGate.lean`

**Interfaces:**
- Consumes: Spec §4.1 (`TaskMachine`) and §4.2 (`TaskGate`)
- Produces:
  - `NeuroStrata.TaskMachine`: `Status`, `Guard`, `Transition`, `table`, `Ctx`, `TaskState`, `TransitionError`, `apply`, `applyTag`, and 5 machine-checked theorems (`no_self_transitions`, `lock2_soundness`, `done_only_reopens_to_open_with_reason`, `block_requires_reason`, `unclaim_clears_claim_exclusively`)
  - `NeuroStrata.TaskGate`: `Row`, `isRetired`, `liveSuccessor`, `workLineage`, `extractionExists`, `obligationCarried`, `hasDoneWithoutExtractionViolation`, and 6 machine-checked theorems (`isRetired_boundary_inclusive`, `isRetired_none_stays_live`, `liveSuccessor_never_retired`, `liveSuccessor_cycle_safe`, `workLineage_contains_root`, `lock2_gate_conservation`)

- [ ] **Step 1: Ensure `elan` and Lean `v4.15.0` toolchain are installed**

Run: `command -v lake || ~/.elan/bin/lake --version || (curl -sSf https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh | sh -s -- -y --default-toolchain leanprover/lean4:v4.15.0)`
Expected: `~/.elan/bin/lake --version` prints `Lake version 5.0.0... (Lean version 4.15.0)`.

- [ ] **Step 2: Create `proofs/lean-toolchain` and `proofs/lakefile.lean`**

`proofs/lean-toolchain`:
```text
leanprover/lean4:v4.15.0
```

`proofs/lakefile.lean`:
```lean
import Lake
open Lake DSL

package «neurostrata-proofs» where
  version := v!"0.1.0"

lean_lib «NeuroStrata» where
  roots := #[`NeuroStrata]

@[default_target]
lean_exe «neurostrata-proofs» where
  root := `Main
```

- [ ] **Step 3: Implement `proofs/NeuroStrata/TaskMachine.lean` and `proofs/NeuroStrata/TaskGate.lean`**

Implement the exact definitions and theorems specified in Spec §4.1 and §4.2, plus a decidable check test (`example`) for Review Focus #3 (`valid_to == now` vs `none`) and Review Focus #4 (2-node retired cycle `A -> B -> A` in `liveSuccessor` returning `none`).

- [ ] **Step 4: Compile both modules with Lean 4 kernel and verify zero `sorry`/`axiom`**

Run: `~/.elan/bin/lake --dir proofs env lean proofs/NeuroStrata/TaskMachine.lean && ~/.elan/bin/lake --dir proofs env lean proofs/NeuroStrata/TaskGate.lean && ! grep -rnE '\b(sorry|axiom)\b' proofs/NeuroStrata/`
Expected: Exit code `0` with zero warnings and zero matches for `sorry` or `axiom`.

- [ ] **Step 5: Commit**

```bash
git add proofs/lean-toolchain proofs/lakefile.lean proofs/NeuroStrata/TaskMachine.lean proofs/NeuroStrata/TaskGate.lean
git commit -m "feat(proofs): add Lean 4 TaskMachine and TaskGate formal proofs (neurostrata-ngd9)"
```

---

### Task 3: Prove `SynapticMesh` and `DomainRegistry` Invariants & Build Truth-Table CLI

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
  - `Main.lean`: executable `neurostrata-proofs` printing deterministic truth-table lines:
    - `TASK_FSM|<from>|<to>|<has_reason>|<has_extraction>|<outcome_tag>` (64 lines)
    - `LIFECYCLE|<from>|<to>|<bool>` (16 lines)
    - `MESH_MERGE|<local>|<cand>|<same_sup>|<content_changed>|<concurrent>|<tag>` (128 lines)
    - `EPOCH_GATE|<cand_epoch>|<manifest_epoch>|<tag>` (3 lines: `1|2`, `2|2`, `3|2`)

- [ ] **Step 1: Implement `proofs/NeuroStrata/SynapticMesh.lean` and `proofs/NeuroStrata/DomainRegistry.lean`**

Implement all types, functions, and theorems from Spec §4.3 and §4.4, including Review Focus #1 (`classifyMerge .superseded .archived true false true = .refuseStaleRestore` and `classifyMerge .archived .superseded true false true = .apply`) and Review Focus #5 (self-loop `slug == parent` rejected by `validateDomainEntries`).

- [ ] **Step 2: Implement `proofs/NeuroStrata.lean` and `proofs/Main.lean`**

Import all four modules in `proofs/NeuroStrata.lean` and emit the 211 canonical truth-table lines (`64 + 16 + 128 + 3`) in `proofs/Main.lean`.

- [ ] **Step 3: Build and run `lake exe neurostrata-proofs` and check for `sorry`/`axiom`**

Run: `~/.elan/bin/lake --dir proofs build && ~/.elan/bin/lake --dir proofs exe neurostrata-proofs | wc -l && ! grep -rnE '\b(sorry|axiom)\b' proofs/`
Expected: `lake build` succeeds with zero warnings, output line count is `211`, and `grep` finds zero `sorry`/`axiom` tokens.

- [ ] **Step 4: Commit**

```bash
git add proofs/NeuroStrata/SynapticMesh.lean proofs/NeuroStrata/DomainRegistry.lean proofs/NeuroStrata.lean proofs/Main.lean
git commit -m "feat(proofs): add SynapticMesh and DomainRegistry proofs and truth-table CLI (neurostrata-ngd9)"
```

---

### Task 4: Add Exhaustive Rust-to-Lean Parity Integration Test (`tests/lean_parity_test.rs`)

**Files:**
- Create: `tests/lean_parity_test.rs`

**Interfaces:**
- Consumes:
  - `neurostrata::task::machine::{apply, Ctx, Status, Task, TransitionError}`
  - `neurostrata::synaptic_mesh::record::LifecycleState`
  - `neurostrata::synaptic_mesh::import::{classify_merge_parity_tag, epoch_gate}`
  - `proofs/` Lake package (`lake exe neurostrata-proofs`)
- Produces:
  - Integration test suite `tests/lean_parity_test.rs` verifying all 211 finite-domain truth-table rows in Rust, whitespace-only reason rejection (Review Focus #2), zero `sorry`/`axiom` tokens in `proofs/**/*.lean`, and byte-for-byte parity with `lake exe neurostrata-proofs`.

- [ ] **Step 1: Write `tests/lean_parity_test.rs`**

Include tests:
1. `rust_truth_table_matches_lean_specification_and_cli`: generates the 211 canonical lines (`64 TASK_FSM + 16 LIFECYCLE + 128 MESH_MERGE + 3 EPOCH_GATE`) directly from Rust calls to `task::machine::apply`, `LifecycleState::can_transition_to`, `classify_merge_parity_tag`, and `epoch_gate`, and compares against `lake --dir proofs exe neurostrata-proofs` stdout.
2. `whitespace_only_reason_is_treated_as_missing_reason`: verifies `ctx.reason = "   \t\n "` matches `hasReason = false` across all 16 `(from, to)` pairs (Review Focus #2).
3. `lean_proof_sources_contain_no_sorry_or_axiom`: walks `proofs/` and verifies every `.lean` file contains zero standalone `sorry` or `axiom` tokens outside comments.

- [ ] **Step 2: Run `cargo test --test lean_parity_test`**

Run: `cargo test --test lean_parity_test`
Expected: PASS (all 3 tests pass, including live Lean CLI parity).

- [ ] **Step 3: Run full workspace verification gates**

Run: `cargo clippy --all-targets && cargo test -- --test-threads=1`
Expected: PASS with zero new warnings and all tests green.

- [ ] **Step 4: Commit**

```bash
git add tests/lean_parity_test.rs
git commit -m "test(parity): add exhaustive Rust-to-Lean truth-table parity test (neurostrata-ngd9)"
```
