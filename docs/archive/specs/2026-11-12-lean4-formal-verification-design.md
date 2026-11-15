# Lean 4 Formal Verification Suite & SynapticMesh Merge Invariant Design

- **Date:** 2026-11-12
- **Task:** `neurostrata-ngd9`
- **Status:** Draft (Pending User Spec Review)
- **Targets:** `src/formal_kernel.rs` (new zero-dependency Charon/Aeneas-extractable Rust kernel), `proofs/` (new Lean 4 Lake package + `proofs/scripts/extract-aeneas.sh`), `src/main.rs`, `src/task/machine.rs`, `src/task/gate.rs`, `src/synaptic_mesh/record.rs`, `src/synaptic_mesh/import.rs`, `src/registry.rs`, `tests/lean_parity_test.rs`

---

## 1. Context & Motivation

NeuroStrata relies on four pure, deterministic algebraic subsystems to enforce memory safety, task completion honesty (Lock 1 / Lock 2), inter-instance SynapticMesh convergence, and domain taxonomy integrity:

1. **Task State Machine (`src/task/machine.rs`)** — A 4-state (`Open`, `InProgress`, `Blocked`, `Done`), 8-edge transition table (`TABLE`, lines 67–76) with 3 guard kinds (`Guard::None`, `Guard::RequireReason`, `Guard::ExtractionRequired`) evaluated by `apply` (lines 163–234).
2. **Task Gate Lineage & Obligation Engine (`src/task/gate.rs`)** — Cycle-safe, `MAX_CHAIN`-bounded (`MAX_CHAIN = 32`, line 129) graph traversals (`live_successor`, lines 148–171; `work_lineage`, lines 184–207) and Lock-2 obligation predicates (`is_retired`, lines 120–125; `extraction_exists`, lines 215–234; `obligation_carried`, lines 249–256; `evaluate`, lines 295–533, task loop lines 298–397).
3. **SynapticMesh Lifecycle & Concurrent Merge Engine (`src/synaptic_mesh/record.rs`, `src/synaptic_mesh/import.rs`)** — A 4-state lifecycle lattice (`LifecycleState`: `Active`, `Archived`, `Superseded`, `Tombstoned`, `can_transition_to` at `record.rs:159–172`, `is_terminal` at `record.rs:176–181`), the 3-way `epoch_gate` (`import.rs:976–987`), and the 7-row concurrent merge policy `classify_merge` (`import.rs:1167–1274`) producing the 6-variant `MergeDecision` enum (`import.rs:1136–1154`).
4. **Domain Registry DAG Validator (`src/registry.rs`)** — Parent-pointer forest and normalized/case-folded alias uniqueness validator `validate_domain_entries` (`registry.rs:129–261`).

During formal modeling of `src/synaptic_mesh/import.rs::classify_merge`, an invariant violation was discovered against `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md` (§"Concurrent merge policy", row 5: `supersede + content edit`):
- When `local_state == LifecycleState::Superseded` and `cand.lifecycle.state == LifecycleState::Archived` with `content_changed == true`, line 1206 (`Active if content_changed => MergeDecision::SupersedeWinsPreserveEdit`) fails to match `Archived` and falls through to line 1208 (`MergeDecision::Refuse("superseded; rejected stale restore")`), silently dropping the concurrent edit on the archived record.
- Symmetrically, when `local_state == LifecycleState::Archived` and `cand.lifecycle.state == LifecycleState::Superseded` with `content_changed == true`, line 1222 (`if local_state == Active && content_changed`) is false and falls through to line 1225 (`MergeDecision::Apply`), overwriting the local edited content with the superseding candidate instead of preserving the losing edit via `MergeDecision::SupersedeWinsPreserveEdit`.

This specification defines a standalone Lean 4 formal verification package (`proofs/`), machine-checked theorems for all four subsystems, the production fix in `src/synaptic_mesh/import.rs`, and an exhaustive Rust-to-Lean parity test (`tests/lean_parity_test.rs`).

---

## 2. Non-Goals

- **No FFI/C++ storage verification:** LadybugDB C++ bindings (`src/store/ladybug.rs`), Tantivy full-text indexes, and Podman CLI sandboxes are I/O boundaries outside the pure state machines.
- **No floating-point ONNX embedding proofs:** IEEE-754 vector distances and neural inference (`src/embed.rs`) are non-algebraic.
- **No Cargo build coupling:** `cargo build` and `cargo test` must not require network access or invoke `lake` during normal Rust compilation; `tests/lean_parity_test.rs` checks the exhaustive finite-domain truth tables in pure Rust and optionally invokes `lake build` when the `lake` binary is present on `PATH` or `~/.elan/bin/lake`.

---

## 3. Architecture & Directory Layout: Aeneas Kernel + Lean 4 Lake Suite

We adopt a **Hybrid Aeneas + Lean 4 Architecture**:
1. **Aeneas-Compliant Pure Rust Kernel (`src/formal_kernel.rs`)**:
   - Following the Aeneas functional translation methodology (https://lean-lang.org/use-cases/aeneas/) and NeuroStrata's crate-free module pattern (`src/fts/mod.rs`), we factor out the pure algebraic state machines into `src/formal_kernel.rs`.
   - `src/formal_kernel.rs` uses **zero external crates** (no `serde_json`, `chrono`, `uuid`, `anyhow`, `lbug`, or `crate::*` imports) and stays strictly within the Charon/Aeneas supported safe Rust subset (`Copy` algebraic enums/structs, `bool`, `u32`, `i64`, `Option`, `Result`, total pattern matching, no interior mutability or trait objects).
   - It compiles standalone as a crate root via `rustc --edition=2021 --crate-type=lib src/formal_kernel.rs` (and `charon` -> LLBC -> `aeneas -backend lean`).
   - Production modules (`src/task/machine.rs`, `src/task/gate.rs`, `src/synaptic_mesh/record.rs`, `src/synaptic_mesh/import.rs`) delegate their pure transition, retirement, lifecycle, epoch-gate, and merge-classification decisions directly to `crate::formal_kernel`.
2. **Standalone Lean 4 Lake Package (`proofs/`) + Aeneas Extraction Script (`proofs/scripts/extract-aeneas.sh`)**:
   - Proves the mathematical theorems over the pure functional translation of `src/formal_kernel.rs` and the bounded graph traversals.
   - `proofs/scripts/extract-aeneas.sh` validates standalone compilation of `src/formal_kernel.rs` and invokes `charon` + `aeneas -backend lean` (locally or via `podman` image `ghcr.io/aeneasverif/aeneas`) when available.
3. **Exhaustive Parity & Extractability Gate (`tests/lean_parity_test.rs`)**:
   - Compiles `#[path = "../src/formal_kernel.rs"] mod formal_kernel;` directly (matching `tests/fts/lexical_test.rs`), verifies standalone `rustc --crate-type=lib src/formal_kernel.rs` compilation with zero `--extern` flags (guaranteeing Aeneas extractability never regresses), and asserts 100% truth-table equivalence against `lake --dir proofs exe neurostrata-proofs`.

```text
src/
└── formal_kernel.rs                # Zero-dep Charon/Aeneas-compliant pure Rust kernel
proofs/
├── lean-toolchain                  # Pinned Lean 4 release (leanprover/lean4:v4.15.0)
├── lakefile.lean                   # Zero-external-dependency Lake config (stdlib only, no Mathlib)
├── Main.lean                       # Executable truth-table emitter (211 canonical lines)
├── NeuroStrata.lean                # Root module importing all four formalization modules
├── scripts/
│   └── extract-aeneas.sh           # Standalone rustc check + Charon/Aeneas LLBC->Lean4 extractor
└── NeuroStrata/
    ├── TaskMachine.lean            # Formal model & theorems for formal_kernel / src/task/machine.rs
    ├── TaskGate.lean               # Formal model & theorems for formal_kernel / src/task/gate.rs
    ├── SynapticMesh.lean           # Formal model & theorems for formal_kernel / src/synaptic_mesh/
    └── DomainRegistry.lean         # Formal model & theorems for src/registry.rs
```

### Why Zero Mathlib Dependency?
All four NeuroStrata state machines operate over finite enums, bounded natural numbers (`Nat`), `Bool`, `Option`, and finite `List` structures. Using Lean 4's standard library (`Init`) with `DecidableEq` and kernel decision procedures (`decide`) keeps `lake build` under ~5 seconds with zero external gigabyte cache downloads.

---

## 4. Formal Models & Theorem Specifications

### 4.1 Module 1: `NeuroStrata/TaskMachine.lean` (`src/task/machine.rs`)

#### Model Definitions
- `inductive Status | open | inProgress | blocked | done deriving DecidableEq, Repr`
- `inductive Guard | none | requireReason | extractionRequired deriving DecidableEq, Repr`
- `structure Transition where (fromState : Status) (toState : Status) (guard : Guard)`
- `def table : List Transition` — Exact 8 edges matching `src/task/machine.rs:67–76`:
  1. `(.open, .inProgress, .none)`
  2. `(.open, .blocked, .requireReason)`
  3. `(.open, .done, .extractionRequired)`
  4. `(.inProgress, .open, .none)`
  5. `(.inProgress, .blocked, .requireReason)`
  6. `(.inProgress, .done, .extractionRequired)`
  7. `(.blocked, .open, .requireReason)`
  8. `(.done, .open, .requireReason)`
- `structure Ctx where (hasReason : Bool) (extractionEdgeExists : Bool)`
- `structure TaskState where (status : Status) (hasAssignee : Bool) (hasSession : Bool)`
- `inductive TransitionError | illegal (fromState toState : Status) (legal : List Status) | missingReason (fromState toState : Status) | extractionRequired`
- `def apply (task : TaskState) (toState : Status) (ctx : Ctx) : Except TransitionError TaskState` — Mirrors `src/task/machine.rs:163–234`, including clearing `hasAssignee` and `hasSession` iff `task.status = .inProgress ∧ toState = .open` (lines 210–213).

#### Required Theorems
1. **`no_self_transitions`**: `∀ s ctx, ∃ legal, apply s s ctx = .error (.illegal s s legal)` — A task cannot transition to its own current status via `apply` (matches test `a_task_cannot_stay_in_its_own_state_via_apply` at `machine.rs:365–379`).
2. **`lock2_soundness`**: `∀ t toState ctx t', apply t toState ctx = .ok t' → t'.status = .done → ctx.extractionEdgeExists = true ∧ (t.status = .open ∨ t.status = .inProgress)` — Reaching `Done` is impossible without an extraction edge, and only from `Open` or `InProgress`.
3. **`done_only_reopens_to_open_with_reason`**: `∀ t toState ctx t', t.status = .done → apply t toState ctx = .ok t' → toState = .open ∧ ctx.hasReason = true ∧ t'.hasAssignee = t.hasAssignee ∧ t'.hasSession = t.hasSession` — From `Done`, the only legal transition is to `Open`, requiring a non-empty reason and preserving historical assignee/session markers.
4. **`block_requires_reason`**: `∀ t ctx t', apply t .blocked ctx = .ok t' → ctx.hasReason = true ∧ (t.status = .open ∨ t.status = .inProgress)` — Transitioning to `Blocked` always requires a reason.
5. **`unclaim_clears_claim_exclusively`**: `∀ t toState ctx t', apply t toState ctx = .ok t' → (t.status = .inProgress ∧ toState = .open → t'.hasAssignee = false ∧ t'.hasSession = false) ∧ (¬(t.status = .inProgress ∧ toState = .open) → t'.hasAssignee = t.hasAssignee ∧ t'.hasSession = t.hasSession)` — Only the `InProgress -> Open` unclaim edge clears claim metadata.

---

### 4.2 Module 2: `NeuroStrata/TaskGate.lean` (`src/task/gate.rs`)

#### Model Definitions
- `structure Row where (id : Nat) (isTask : Bool) (status : Status) (validTo : Option Int) (supersededBy : Option Nat) (supersedes : List Nat) (extractedFrom : List Nat) (grandfathered : Bool)`
- `def isRetired (r : Row) (now : Int) : Bool := match r.validTo with | some vt => decide (vt ≤ now) | none => false` (matches `gate.rs:120–125`).
- `def liveSuccessor (fuel : Nat) (seen : List Nat) (currentId : Option Nat) (rows : List Row) (now : Int) : Option Row` — Fuel-bounded (`MAX_CHAIN = 32`), cycle-safe (`seen` list check) walk along `supersededBy` returning `some row` on the first non-retired row, or `none` on cycle, missing row, missing `supersededBy`, or zero fuel (`gate.rs:148–171`).
- `def workLineage (fuel : Nat) (queue : List Nat) (seen : List Nat) (acc : List Nat) (rows : List Row) : List Nat` — Fuel-bounded BFS along `supersedes` (`gate.rs:184–207`).
- `def extractionExists (rows : List Row) (taskId : Nat) (now : Int) : Bool` — True iff some non-retired row `m` with `m.id ≠ taskId` references any `id ∈ workLineage` in `m.extractedFrom` (`gate.rs:215–234`).
- `def obligationCarried (r : Row) (rows : List Row) (now : Int) : Bool` — True iff `liveSuccessor` returns `some succ` with `succ.isTask = true ∧ succ.status = .done ∧ extractionExists rows succ.id now = true` (`gate.rs:249–256`).
- `def hasDoneWithoutExtractionViolation (r : Row) (rows : List Row) (now : Int) : Bool` — Mirrors `gate.rs:369–395`: for a task row with `r.status = .done`, returns `false` if `(isRetired r now ∧ obligationCarried r rows now) ∨ r.grandfathered ∨ extractionExists rows r.id now`, and `true` otherwise.

#### Required Theorems
1. **`isRetired_boundary_inclusive`**: `∀ r now, r.validTo = some now → isRetired r now = true` — `valid_to == now` is already history (`gate.rs:124`).
2. **`isRetired_none_stays_live`**: `∀ r now, r.validTo = none → isRetired r now = false` — Missing/malformed expiry fails closed (stays live).
3. **`liveSuccessor_never_retired`**: `∀ fuel seen cid rows now succ, liveSuccessor fuel seen cid rows now = some succ → isRetired succ now = false` — Any row returned by `liveSuccessor` is guaranteed live (`¬isRetired`).
4. **`liveSuccessor_cycle_safe`**: `∀ fuel seen id rows now, id ∈ seen → liveSuccessor (fuel + 1) seen (some id) rows now = none` — Revisiting a seen ID in a supersession loop immediately returns `none`.
5. **`workLineage_contains_root`**: `∀ fuel rows id, fuel > 0 → id ∈ workLineage fuel [id] [] [] rows` — Every task's work lineage includes its own ID first.
6. **`lock2_gate_conservation`**: `∀ r rows now, r.isTask = true → r.status = .done → r.grandfathered = false → hasDoneWithoutExtractionViolation r rows now = false → extractionExists rows r.id now = true ∨ (isRetired r now = true ∧ ∃ succ, liveSuccessor 32 [] r.supersededBy rows now = some succ ∧ succ.isTask = true ∧ succ.status = .done ∧ extractionExists rows succ.id now = true)` — A non-grandfathered `Done` task never passes the gate unless either its own work lineage has a live extraction or it is retired and its terminal live successor is a `Done` task whose work lineage has a live extraction.

---

### 4.3 Module 3: `NeuroStrata/SynapticMesh.lean` (`src/synaptic_mesh/record.rs`, `src/synaptic_mesh/import.rs`)

#### Model Definitions
- `inductive LifecycleState | active | archived | superseded | tombstoned deriving DecidableEq, Repr`
- `def canTransitionTo (self next : LifecycleState) : Bool` — Exact 7 allowed pairs matching `record.rs:159–172`:
  - `(.active, .archived)`, `(.archived, .active)`, `(.active, .superseded)`, `(.archived, .superseded)`, `(.active, .tombstoned)`, `(.archived, .tombstoned)`, `(.superseded, .tombstoned)`.
- `def isTerminal (s : LifecycleState) : Bool := s == .tombstoned` (`record.rs:176–181`).
- `inductive EpochDecision | apply | quarantineOld | refuseFuture deriving DecidableEq, Repr`
- `def epochGate (candidateEpoch manifestEpoch : Nat) : EpochDecision` — Matches `import.rs:976–987`.
- `inductive MergeDecision` — Models the 6 Rust `MergeDecision` enum variants (`import.rs:1136–1154`: `Apply`, `ApplyCombination(&'static str)`, `Skip`, `Refuse(String)`, `Conflict(&'static str)`, `SupersedeWinsPreserveEdit`) refined into 10 payload-specific tags so Lean can reason over exact refusal and conflict reasons without string parsing:
  - `| apply`
  - `| applyCombinationArchiveEdit`
  - `| skip`
  - `| refuseTombstoneWins`
  - `| refuseStaleRestore`
  - `| refuseIllegalTransition`
  - `| conflictTwoReplacements`
  - `| conflictRestoreArchive`
  - `| conflictIncompatibleEdits`
  - `| supersedeWinsPreserveEdit`
- `def classifyMerge (localState candState : LifecycleState) (sameSupersededBy : Bool) (contentChanged : Bool) (concurrent : Bool) : MergeDecision` — Models the repaired `classify_merge` (`import.rs:1167–1274`) where both `Active` and `Archived` with `contentChanged = true` against `Superseded` produce `.supersedeWinsPreserveEdit`.
- `structure ResolutionOutcome where (effectiveState : LifecycleState) (disposition : DispositionClass)` — Maps `(localState, candState, classifyMerge ...)` to the post-merge lifecycle state and semantic disposition (`clean`, `archiveEditCombined`, `preserveSupersededEdit`, `conflictTwoReplacements`, `conflictRestoreArchive`, `conflictIncompatibleEdits`), abstracting directional asymmetry (`Apply` vs `Skip`/`Refuse`) into canonical replica state.

#### Required Theorems
1. **`tombstone_absorbing`**: `∀ next, canTransitionTo .tombstoned next = false` — No transition exits `Tombstoned`.
2. **`superseded_never_reactivates`**: `canTransitionTo .superseded .active = false ∧ canTransitionTo .superseded .archived = false` — Retired IDs never transition back to live states.
3. **`tombstone_dominates_merge`**:
   - `∀ cand sameSup chg conc, classifyMerge .tombstoned cand sameSup chg conc = (if cand == .tombstoned then .skip else .refuseTombstoneWins)`
   - `∀ loc sameSup chg conc, loc ≠ .tombstoned → classifyMerge loc .tombstoned sameSup chg conc = .apply`
   - Combined consequence: when either side is `.tombstoned`, `(resolveOutcome loc cand ...).effectiveState = .tombstoned` and it never produces a conflict artifact (`conflict*`) or preserved edit (`supersedeWinsPreserveEdit`).
4. **`no_silent_edit_drop_on_supersede`**:
   - `∀ liveState sameSup conc, (liveState = .active ∨ liveState = .archived) → classifyMerge .superseded liveState sameSup true conc = .supersedeWinsPreserveEdit`
   - `∀ liveState sameSup conc, (liveState = .active ∨ liveState = .archived) → classifyMerge liveState .superseded sameSup true conc = .supersedeWinsPreserveEdit`
   - Proves that a concurrent content edit on either an `Active` or `Archived` record paired with `Superseded` is always routed to `.supersedeWinsPreserveEdit` and never silently dropped via `.refuseStaleRestore` or overwritten via `.apply`.
5. **`archive_edit_commutativity`**: `∀ conc, classifyMerge .active .archived true true conc = .applyCombinationArchiveEdit ∧ classifyMerge .archived .active true true conc = .applyCombinationArchiveEdit` — Concurrent or sequential `archive + edit` commutes in both directions.
6. **`classifyMerge_concurrent_symmetry`**: `∀ s1 s2 sameSup chg, resolveOutcome s1 s2 sameSup chg true = resolveOutcome s2 s1 sameSup chg true` — Under concurrent merge (`concurrent = true`), swapping local and incoming states yields the exact same effective lifecycle state and semantic disposition class (including directional pairs such as `(.tombstoned, .active) -> .refuseTombstoneWins` vs `(.active, .tombstoned) -> .apply`, both yielding `(.tombstoned, .clean)`).
7. **`epochGate_trichotomy`**: `∀ c m, (c = m ↔ epochGate c m = .apply) ∧ (c < m ↔ epochGate c m = .quarantineOld) ∧ (c > m ↔ epochGate c m = .refuseFuture)`.

---

### 4.4 Module 4: `NeuroStrata/DomainRegistry.lean` (`src/registry.rs`)

#### Model Definitions
- `structure DomainEntry where (slug : Nat) (parent : Option Nat) (aliases : List Nat)` (over normalized domain identifiers)
- `def parentDeclared (entries : List DomainEntry) : Bool` — Checks that every `some p` in `entry.parent` appears in `entries.map (·.slug)` (`registry.rs:201–207`).
- `def reachesRoot (fuel : Nat) (seen : List Nat) (cur : Option Nat) (entries : List DomainEntry) : Bool` — Cycle check matching `registry.rs:209–224`: returns `true` iff walking `parent` from `cur` reaches `none` without repeating any node in `seen` before `fuel` runs out.
- `def noAliasCollisions (entries : List DomainEntry) : Bool` — Checks that all slugs and aliases across `entries` are pairwise distinct (`registry.rs:153–155, 228–258`).
- `def validateDomainEntries (entries : List DomainEntry) : Bool := slugsUnique entries && parentDeclared entries && allReachRoot entries && noAliasCollisions entries`.

#### Required Theorems
1. **`reachesRoot_rejects_self_loop`**: `∀ fuel seen id entries, id ∈ seen → reachesRoot (fuel + 1) seen (some id) entries = false` — Any cycle in the parent chain is rejected.
2. **`valid_registry_has_declared_parents`**: `∀ entries e p, validateDomainEntries entries = true → e ∈ entries → e.parent = some p → p ∈ entries.map (·.slug)` — Every parent reference in a validated registry resolves to a declared slug.
3. **`valid_registry_acyclic`**: `∀ entries e, validateDomainEntries entries = true → e ∈ entries → reachesRoot entries.length [] (some e.slug) entries = true` — Every entry in a validated registry reaches a root (`parent = none`) in at most `entries.length` hops without cycling.
4. **`valid_registry_disjoint_identifiers`**: `∀ entries, validateDomainEntries entries = true → (entries.map (·.slug) ++ entries.bind (·.aliases)).Nodup` — All normalized slugs and aliases in a validated registry are globally unique.

---

## 5. Production Bug Fix in `src/synaptic_mesh/import.rs`

In `src/synaptic_mesh/import.rs::classify_merge`:

```rust
    if local_state == Superseded {
        return match cand_state {
            // Tombstone wins even over a retired id.
            Tombstoned => MergeDecision::Apply,
            Superseded => {
                if cand_sup != local_superseded_by {
                    MergeDecision::Conflict("two different replacements")
                } else {
                    MergeDecision::Skip
                }
            }
            // supersede + content edit: replacement authoritative; the losing
            // edit (whether on an active or archived revision) is preserved as
            // a new record (handled by the caller).
            Active | Archived if content_changed => MergeDecision::SupersedeWinsPreserveEdit,
            // restore (or archive without edit) of a retired id is refused, never stomped.
            _ => MergeDecision::Refuse("superseded; rejected stale restore".to_string()),
        };
    }
```

And at lines 1218–1226:

```rust
    // Live -> superseded is always legal.
    if cand_state == Superseded {
        // supersede + content edit (reverse direction): the incoming
        // supersession is authoritative, the local edit (active or archived)
        // is preserved as a new record.
        if matches!(local_state, Active | Archived) && content_changed {
            return MergeDecision::SupersedeWinsPreserveEdit;
        }
        return MergeDecision::Apply;
    }
```

### Downstream Verification of `preserve_superseded_edit`
`preserve_superseded_edit` (`src/synaptic_mesh/import.rs:1999–2097`, branch extraction at `2018–2043`) branches on `if local_state == LifecycleState::Superseded` (line 2019):
- When `local_state == Superseded` and `cand_state == Archived` with `content_changed == true`, it extracts `cand.content`, `cand.memory_type`, `cand.lifecycle.created_by`, `cand.relationships`, `cand.metadata`, sets `apply_supersession = false`, and writes the preserved edit as a new active record referencing the superseded ID.
- When `local_state == Archived` and `cand_state == Superseded` with `content_changed == true`, it takes the `else` branch (line 2029), extracts the local row's content/metadata/relationships, sets `apply_supersession = true` to apply the incoming supersession to `id`, and writes the preserved local edit as a new record referencing `id`.
Thus no changes are required inside `preserve_superseded_edit` itself, and we will add end-to-end integration tests for both directions (`Superseded` vs `Archived` with `content_changed == true`).

---

## 6. Rust-to-Lean Parity Bridge (`tests/lean_parity_test.rs`)

To guarantee that the Lean 4 models in `proofs/NeuroStrata/` and the Rust implementation in `src/` never drift:

1. **Exhaustive Finite-State Parity Test (`tests/lean_parity_test.rs`)**:
   - Evaluates `task::machine::apply` across all `4 × 4 × 2 × 2 = 64` `(from, to, has_reason, extraction_edge_exists)` combinations and compares them against the canonical truth table exported by `proofs/NeuroStrata.lean`.
   - Evaluates `synaptic_mesh::record::LifecycleState::can_transition_to` across all `4 × 4 = 16` state pairs and `classify_merge` across all `4 × 4 × 2 × 2 × 2 = 128` `(local_state, cand_state, same_superseded_by, content_changed, concurrent)` combinations.
   - Evaluates `synaptic_mesh::import::epoch_gate` across `<, ==, >` cases.
2. **Live Lean Kernel Gate (`lake build` + `lake env lean --run`)**:
   - When `lake` is available (`PATH` or `~/.elan/bin/lake`), `tests/lean_parity_test.rs` invokes `lake build` in `proofs/` to verify all theorems type-check without `sorry` (`grep -rn '\bsorry\b' proofs/NeuroStrata*` must be empty) and runs the Lean executable to assert byte-for-byte JSON/CSV parity against Rust's live evaluation.
   - To make `classify_merge` testable from `tests/lean_parity_test.rs` without exposing internal implementation details unnecessarily, a thin `pub mod formal_parity` helper (or `pub fn` wrapper on the pure decision table) will be exposed in `src/synaptic_mesh/import.rs`.

---

## 7. Verification & Acceptance Criteria

1. `elan` toolchain installed with pinned `proofs/lean-toolchain`; `lake build` in `proofs/` exits `0` with zero warnings and zero occurrences of `sorry` or `axiom` in `proofs/`.
2. Unit tests in `src/synaptic_mesh/import.rs` verify `(Superseded, Archived, content_changed = true)` and `(Archived, Superseded, content_changed = true)` both return `MergeDecision::SupersedeWinsPreserveEdit` and preserve the losing edit in an end-to-end import replay.
3. `cargo test --test lean_parity_test` passes all 64 Task FSM cases, 16 Lifecycle transition cases, 128 SynapticMesh merge cases, and live `lake` parity check.
4. Full project quality gates pass (`cargo check`, `cargo clippy`, `cargo test -- --test-threads=1`, `neurostrata_task_validate`).
