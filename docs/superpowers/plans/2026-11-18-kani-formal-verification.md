# Kani Symbolic Model Checking & Formal Kernel Bug Remediation Plan (`neurostrata-zbe7`)

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task (`neurostrata-zbe7`).

**Goal:** Integrate compile-time gated Kani (`#[cfg(kani)]`) proof harnesses into `src/formal_kernel.rs`, fix 4 concrete bug classes across `src/formal_kernel.rs`, `src/kernel/mod.rs`, and `src/tools/procedure.rs`, maintain 1:1 Lean 4 differential parity, and wire Kani into `proofs/scripts/extract-aeneas.sh`, `scripts/task-coverage-gate.sh`, and `.github/workflows/ci.yml` (`neurostrata-zbe7`).

**Architecture:** Standalone `kani src/formal_kernel.rs` symbolic verification using `#[cfg_attr(kani, derive(kani::Arbitrary))]` and `#[cfg(kani)] mod kani_proofs`, paired with Lean 4 proofs in `proofs/NeuroStrata/` and differential parity tests in `tests/lean_parity_test.rs` (`neurostrata-zbe7`).

**Tech Stack:** Rust, Kani Rust Verifier `0.68.0` (CBMC `6.11.0`), Lean 4 (`lake build`), GitHub Actions CI (`neurostrata-zbe7`).

---

### Task 1: Write Failing Unit & Integration Regression Tests for the 4 Discovered Bugs (`neurostrata-zbe7`)

**Files:**

- Modify: `src/formal_kernel.rs`
- Modify: `src/kernel/mod.rs`
- Modify: `src/tools/procedure.rs`

- [ ] **Step 1 (`neurostrata-zbe7`):** Add failing unit tests in `src/formal_kernel.rs` for `build_resolve_post_gate_input`, `build_writeback_post_gate_input`, and `compute_procedure_next_due`.
- [ ] **Step 2 (`neurostrata-zbe7`):** Add failing integration tests in `src/kernel/mod.rs` proving:
  - `resolve_pointer` rejects slices where `records.len() > req.limit` even when `records.len() <= schema.max_slice_rows`.
  - `apply_external_effect` rejects write-back when `pointer.expected_source_sha256` mismatches `receipt.prior_source_sha256` (`DenySourceDigestMismatch`) and when `req.expected_source_sha256` mismatches `receipt.prior_source_sha256` (`DenyCasConflict`).
- [ ] **Step 3 (`neurostrata-zbe7`):** Add failing unit/async tests in `src/tools/procedure.rs` proving:
  - `next_due` does not panic on `i64::MAX` day/session cadences or negative day cadences (`every-n-days:-5`, `every-n-days:0`).
  - `handle_procedure_perform` saturates at `i64::MAX` performance count and clamps negative `performance_count` metadata via `evaluate_procedure_fire`.

### Task 2: Implement Kernel Helpers, Deterministic DAG/SKOS Reachability, and Kani Harnesses (`neurostrata-zbe7`)

**Files:**

- Modify: `src/formal_kernel.rs`
- Modify: `src/kernel/mod.rs`
- Modify: `src/tools/procedure.rs`

- [ ] **Step 1 (`neurostrata-zbe7`):** Implement `build_resolve_post_gate_input`, `build_writeback_post_gate_input`, and `compute_procedure_next_due` in `src/formal_kernel.rs`.
- [ ] **Step 2 (`neurostrata-zbe7`):** Replace `HashMap`/`HashSet` in `validate_taxonomy_dag` and `validate_skos_disjointness` with deterministic iterative reachability (`is_reachable_in_edges`) and make `compute_epistemic_coverage_bps` a `pub const fn`.
- [ ] **Step 3 (`neurostrata-zbe7`):** Add `#[cfg_attr(kani, derive(kani::Arbitrary))]` on formal kernel types and `#[cfg(kani)] mod kani_proofs` covering all kernel decision functions.
- [ ] **Step 4 (`neurostrata-zbe7`):** Wire `src/kernel/mod.rs` and `src/tools/procedure.rs` to the new formal kernel functions and verify `kani src/formal_kernel.rs` and `cargo test` pass.

### Task 3: Update Lean 4 Proofs, Parity Runner, and Documentation Co-Change (`neurostrata-zbe7`)

**Files:**

- Modify: `proofs/NeuroStrata/DataConnector.lean`
- Modify: `proofs/NeuroStrata/Procedure.lean`
- Modify: `proofs/Main.lean`
- Modify: `tests/lean_parity_test.rs`
- Modify: `docs/vision/product-vision.md`

- [ ] **Step 1 (`neurostrata-zbe7`):** Mechanize `buildResolvePostGateInput`, `buildWritebackPostGateInput`, and `computeProcedureNextDue` (plus soundness theorems) in `proofs/NeuroStrata/DataConnector.lean` and `proofs/NeuroStrata/Procedure.lean`.
- [ ] **Step 2 (`neurostrata-zbe7`):** Expose the new vectors in `proofs/Main.lean` and assert 1:1 parity in `tests/lean_parity_test.rs`.
- [ ] **Step 3 (`neurostrata-zbe7`):** Update `docs/vision/product-vision.md` (required co-change target for `src/formal_kernel.rs` under `docs/manifest.json`) to document Kani symbolic model checking alongside Lean 4 verification.

### Task 4: Wire Kani into `extract-aeneas.sh`, `task-coverage-gate.sh`, and `.github/workflows/ci.yml` (`neurostrata-zbe7`)

**Files:**

- Modify: `proofs/scripts/extract-aeneas.sh`
- Modify: `scripts/task-coverage-gate.sh`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1 (`neurostrata-zbe7`):** Update `proofs/scripts/extract-aeneas.sh` and `scripts/task-coverage-gate.sh` to execute `kani src/formal_kernel.rs` (honoring `~/.cargo/bin/kani` and `NEUROSTRATA_REQUIRE_KANI=1`).
- [ ] **Step 2 (`neurostrata-zbe7`):** Update `.github/workflows/ci.yml` (`lean-formal-proofs` job) to install and run Kani against `src/formal_kernel.rs` with `NEUROSTRATA_REQUIRE_KANI=1`.
- [ ] **Step 3 (`neurostrata-zbe7`):** Run `kani src/formal_kernel.rs`, `lake build`, `cargo test --test lean_parity_test`, `bash scripts/task-coverage-gate.sh`, and unit/integration tests to verify all gates pass.
