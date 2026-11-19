# Kani Symbolic Model Checking & Formal Kernel Bug Remediation Design (`neurostrata-zbe7`)

## 1. Objective (`neurostrata-zbe7`)

Integrate the Kani Rust Verifier (`https://github.com/model-checking/kani`, `0.68.0` / CBMC `6.11.0`) into NeuroStrata's formal verification pipeline (`src/formal_kernel.rs`, `proofs/scripts/extract-aeneas.sh`, `scripts/task-coverage-gate.sh`, `.github/workflows/ci.yml`), keeping all Kani harnesses and derives strictly compile-time gated (`#[cfg(kani)]` / `#[cfg_attr(kani, derive(kani::Arbitrary))]`) with zero runtime or binary overhead on `neurostrata-mcp`. Use symbolic model checking to expose, prove, and fix four concrete bug classes across the kernel and its callers (`neurostrata-zbe7`).

## 2. Architecture (`neurostrata-zbe7`)

1. **Zero-Overhead Compile-Time Gating (`neurostrata-zbe7`):**
   - Annotate `src/formal_kernel.rs` decision enums and input structs with `#[cfg_attr(kani, derive(kani::Arbitrary))]`.
   - Embed a `#[cfg(kani)] mod kani_proofs` block at the bottom of `src/formal_kernel.rs` containing bounded `#[kani::proof]` harnesses across every decision subsystem in the formal kernel.
   - Verify standalone via `kani src/formal_kernel.rs` in seconds without compiling the C++ `lbug` engine or altering `Cargo.toml`.
2. **Co-Verification with Lean 4 & Charon/Aeneas (`neurostrata-zbe7`):**
   - `proofs/scripts/extract-aeneas.sh` runs `rustc --crate-type=lib src/formal_kernel.rs`, followed by `kani src/formal_kernel.rs` (when `kani` is installed, or required when `NEUROSTRATA_REQUIRE_KANI=1`), plus Charon/Aeneas extraction when available.
   - `proofs/NeuroStrata/DataConnector.lean`, `proofs/NeuroStrata/Procedure.lean`, `proofs/Main.lean`, and `tests/lean_parity_test.rs` stay in 1:1 differential parity with the new kernel helpers.
   - `.github/workflows/ci.yml` runs Kani verification alongside the Lean 4 formal proof job.

## 3. Bugs Caught & Remediated (`neurostrata-zbe7`)

1. **Bug 1 — `AgentKernel::resolve_pointer` Post-Gate Row-Limit Bypass & `usize -> u32` Truncation (`src/kernel/mod.rs`, `src/formal_kernel.rs`, `neurostrata-zbe7`):**
   - _Defect:_ `resolve_pointer` built `KernelConnectorPostGateInput` with `observed_rows: slice.records.len() as u32` (wrapping `(u32::MAX as usize) + 1` to `0`) and `max_slice_rows: schema.max_slice_rows` (omitting `.min(req.limit)`, unlike `max_slice_bytes: req.max_bytes.min(schema.max_slice_bytes)`), allowing a connector to return more rows than `req.limit` without triggering `DenyObservedBoundsExceeded`.
   - _Fix:_ Extract pure `build_resolve_post_gate_input` into `src/formal_kernel.rs` using saturating `usize -> u32` conversion and `requested_rows.min(schema_max_rows)`, wire `AgentKernel::resolve_pointer` to it, prove with `proof_build_resolve_post_gate_input_bounds`, and add a Lean 4 theorem + unit/integration regression tests.
2. **Bug 2 — `AgentKernel::apply_external_effect` Post-Gate Unchecked Pointer Digest & Unreachable CAS Conflict (`src/kernel/mod.rs`, `src/formal_kernel.rs`, `neurostrata-zbe7`):**
   - _Defect:_ `apply_external_effect` ignored `pointer.expected_source_sha256`, wired `req.expected_source_sha256` to `source_digest_matches`, and hardcoded `cas_expected_matches: true`, leaving `KernelConnectorPostVerdict::DenyCasConflict` unreachable and allowing write-backs when a pointer's pinned source digest mismatched `receipt.prior_source_sha256`.
   - _Fix:_ Extract pure `build_writeback_post_gate_input` into `src/formal_kernel.rs` separating pointer source-digest verification (`source_digest_matches`) from request CAS verification (`cas_expected_matches`), wire `AgentKernel::apply_external_effect` to it, prove with `proof_build_writeback_post_gate_input_distinct`, and add Lean 4 parity + integration regression tests.
3. **Bug 3 — `tools::procedure` `next_due` & `handle_procedure_perform` Signed Overflow Panics & Negative Cadence (`src/tools/procedure.rs`, `src/formal_kernel.rs`, `neurostrata-zbe7`):**
   - _Defect:_ `next_due` performed unchecked `last + days * DAY_SECONDS` and `last + sessions_elapsed * SESSION_ESTIMATE_SECONDS` (panicking on large `i64` cadences/timestamps and shifting backwards on `days <= 0`), and `handle_procedure_perform` computed `let after_count = count + 1;` with unchecked `i64` addition instead of routing through `formal_kernel::evaluate_procedure_fire`.
   - _Fix:_ Extract pure `compute_procedure_next_due` into `src/formal_kernel.rs` using checked/saturating arithmetic (`days > 0`, `saturating_mul`, `saturating_add`), route `handle_procedure_perform` through `evaluate_procedure_fire` with negative counter clamping and saturation, prove panic-freedom and monotonicity via Kani (`proof_compute_procedure_next_due_no_overflow`, `proof_evaluate_procedure_fire_no_panic`), and add Lean 4 parity + regression tests.
4. **Bug 4 — `validate_taxonomy_dag` & `validate_skos_disjointness` Non-Deterministic `HashMap`/`HashSet` Entropy in Formal Kernel (`src/formal_kernel.rs`, `neurostrata-zbe7`):**
   - _Defect:_ `validate_taxonomy_dag` and `validate_skos_disjointness` used `std::collections::{HashMap, HashSet}` (which seed `RandomState` from OS entropy syscalls incompatible with standalone CBMC model checking) and allocated an unused `rev_adj` map on every call.
   - _Fix:_ Replace `HashMap`/`HashSet` in `validate_taxonomy_dag` and `validate_skos_disjointness` with deterministic iterative reachability over `&[(u32, u32)]`, make `compute_epistemic_coverage_bps` a `pub const fn` with saturating accumulation, and verify with Kani harnesses (`proof_validate_taxonomy_dag_bounded`, `proof_validate_skos_disjointness_bounded`, `proof_compute_epistemic_coverage_bps_bounds`).
