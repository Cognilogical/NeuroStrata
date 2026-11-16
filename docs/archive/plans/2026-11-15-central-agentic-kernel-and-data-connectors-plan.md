# Central Agentic Kernel & Southbound DataConnector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the central `AgentKernel` (`src/kernel/`) with async storage traits (`TaskStore`, `OntologyStore` on `LadybugStore`), ReBAC-scoped internal domain ports (`MemoryPort`, `ExecutivePort`, `OntologyPort`), a southbound `DataConnector` trait and registry (`src/connectors/`) using trusted `pointer_id` resolution, and a two-phase Lean 4-verified `DataConnectorGate` (`src/formal_kernel.rs` + `proofs/NeuroStrata/DataConnector.lean`).

**Architecture:** `LadybugStore` remains the private storage engine for the agent's brain, accessed only via `VectorStore`, `GraphRead`, `IntakeStore`, `TaskStore`, and `OntologyStore` inside principal-scoped kernel ports. External data pools are indexed via private `data_pointer` records in `LadybugStore` and accessed through `DataConnector` implementations guarded by a pure pre-I/O authorization gate (guaranteeing zero connector I/O on deny) and a post-I/O validation gate (verifying observed bounds, source SHA-256 digests, and CAS write-back preconditions).

**Tech Stack:** Rust (`tokio`, `async-trait`, `serde`, `sha2`), `LadybugDB` (`lbug`), Lean 4 (`lake`).

**Spec:** `docs/superpowers/specs/2026-11-15-central-agentic-kernel-and-data-connectors-design.md`

## Global Constraints

- Never name files, features, plans, specs, tasks, symbols, or documentation after competitor companies, products, or projects (`db6e5570-de2d-4af7-998a-0a7d7f256a2a`).
- Zero Cargo feature flags (`0f443bda-92cc-4b5d-a393-f9996b2c8b2e`).
- Lean 4 proofs (`proofs/NeuroStrata/*.lean`) must compile with zero `sorry` and zero `axiom`.
- `data_pointer` memories must carry `eligibility: "private"` and never be added to `SHAREABLE_MEMORY_TYPES`.
- All new public traits, CLI subcommands, and MCP tools in watched files must be documented in `docs/` and registered in `docs/manifest.json` so `evaluate_doc_gate` passes.

## Review Focus

1. **Pre-gate denial must perform zero connector I/O:** Any `Deny*` verdict from `evaluate_connector_pre_gate` (unregistered connector, ReBAC denial, read-only pool on write-back, missing/reverted receipt, slice row/byte limit exceeded, future clock skew, stale pointer) must return before calling `DataConnector::resolve_slice` or `DataConnector::apply_external_effect`.
2. **No caller-supplied URI at resolve/write-back time:** `resolve_pointer` and `apply_effect` accept only `pointer_id`, loading the trusted `DataPointerRecord` from `LadybugStore` to prevent SSRF and path traversal.
3. **Duplicate `connector_id` or `pointer_id` rejection:** Registering an already-registered `connector_id` in `DataConnectorRegistry` or an existing active `pointer_id` in a namespace must fail closed.
4. **Source digest vs. slice digest distinction:** `expected_source_sha256` validates the whole external pool resource (`source_sha256`), while `slice_sha256` hashes only the returned slice records.
5. **Write-back requires active, target-bound `action_receipt` + CAS + idempotency key:** Reverted, superseded, or target-mismatched receipts and CAS digest mismatches must be rejected fail-closed.

---

### Task 1: Two-Phase Lean 4 `DataConnectorGate` (`src/formal_kernel.rs` & `proofs/NeuroStrata/DataConnector.lean`)

**Files:**
- Modify: `src/formal_kernel.rs`
- Create: `proofs/NeuroStrata/DataConnector.lean`
- Modify: `proofs/NeuroStrata.lean`
- Modify: `tests/lean_parity_test.rs`

**Interfaces:**
- Produces in `src/formal_kernel.rs`:
  - `KernelConnectorOp { RegisterPointer, ResolveSlice, ApplyWriteBack }`
  - `KernelActionReceiptState { NotRequired, ActiveBound, MissingOrNotFound, RevertedOrSuperseded, TargetMismatch }`
  - `KernelConnectorPreGateInput`, `KernelConnectorPreVerdict`, `pub const fn evaluate_connector_pre_gate(input: KernelConnectorPreGateInput) -> KernelConnectorPreVerdict`
  - `KernelConnectorPostGateInput`, `KernelConnectorPostVerdict`, `pub const fn evaluate_connector_post_gate(input: KernelConnectorPostGateInput) -> KernelConnectorPostVerdict`

- [ ] **Step 1: Add failing parity test `data_connector_gate_parity` in `tests/lean_parity_test.rs`**
  Test every `KernelConnectorPreVerdict` and `KernelConnectorPostVerdict` branch plus numeric boundary conditions (`0`, `1`, `max`, `max + 1` for rows/bytes/staleness and future clock skew).
- [ ] **Step 2: Implement `evaluate_connector_pre_gate` and `evaluate_connector_post_gate` in `src/formal_kernel.rs`**
  Follow the exact precedence order in Section 3.4 of the spec.
- [ ] **Step 3: Create `proofs/NeuroStrata/DataConnector.lean` and export in `proofs/NeuroStrata.lean`**
  Mechanize `evaluateConnectorPreGate` and `evaluateConnectorPostGate` and prove determinism + fail-closed invariants with zero `sorry`/`axiom`.
- [ ] **Step 4: Verify Lean build and Rust parity tests**
  Run: `(cd proofs && lake build) && cargo test --test lean_parity_test`
  Expected: PASS with zero warnings/errors.

---

### Task 2: Storage Traits (`TaskStore`, `OntologyStore`) & Vocabulary (`src/traits.rs`, `src/store/ladybug.rs`, `src/schemas/memory-vocabulary.v3.json`)

**Files:**
- Modify: `src/schemas/memory-vocabulary.v3.json`
- Modify: `src/traits.rs`
- Modify: `src/store/ladybug.rs`

**Interfaces:**
- Produces in `src/traits.rs`:
  - `pub struct TaskSummaryRow { pub id: String, pub payload: MemoryPayload }`
  - `pub trait TaskStore: Send + Sync` (`load_task_row`, `list_task_rows`, `count_extracted_memories_for_task`)
  - `pub trait OntologyStore: Send + Sync` (`list_memories_by_types`, `get_memory_by_id`, `put_scoped_memory`)
- Implements `TaskStore` and `OntologyStore` for `LadybugStore` in `src/store/ladybug.rs`.

- [ ] **Step 1: Add `"data_pointer"` memory type to `src/schemas/memory-vocabulary.v3.json`**
  Keep `SHAREABLE_MEMORY_TYPES` unchanged so `data_pointer` is strictly local/private.
- [ ] **Step 2: Define `TaskSummaryRow`, `TaskStore`, and `OntologyStore` in `src/traits.rs` and implement for `LadybugStore` in `src/store/ladybug.rs`**
  Include a unit test in `src/store/ladybug.rs` verifying `TaskStore` and `OntologyStore` round-trips against a temporary `LadybugStore`.
- [ ] **Step 3: Verify storage trait tests**
  Run: `cargo test store::ladybug::tests`
  Expected: PASS.

---

### Task 3: Southbound `DataConnector` Trait, Registry & Reference Connectors (`src/connectors/mod.rs`)

**Files:**
- Create: `src/connectors/mod.rs`
- Modify: `src/main.rs` (add `mod connectors;`)

**Interfaces:**
- Produces in `src/connectors/mod.rs`:
  - `DataPoolAccessMode`, `DataPoolSchema`, `DataPointerRecord`, `DataSliceRequest`, `DataSlicePayload`, `ExternalEffectRequest`, `ExternalEffectReceipt`
  - `pub trait DataConnector: Send + Sync`
  - `pub struct DataConnectorRegistry` (`new_with_defaults`, `register` rejecting duplicate `connector_id`, `get`, `list_connector_ids`)
  - `pub struct InMemoryPoolConnector` (`connector_id = "memory_pool"`)
  - `pub struct JsonFilePoolConnector` (`connector_id = "json_file"`, validating paths with `daemon::check_caller_path` or canonical workspace prefix, computing `source_sha256` and `slice_sha256`, and enforcing CAS + idempotency key on write-back)

- [ ] **Step 1: Implement `src/connectors/mod.rs` with unit tests**
  Test duplicate connector rejection, `InMemoryPoolConnector` slice/write-back/CAS/idempotency, and `JsonFilePoolConnector` slice/digest/path-guard behavior.
- [ ] **Step 2: Run connector unit tests**
  Run: `cargo test connectors::tests`
  Expected: PASS.

---

### Task 4: Central `AgentKernel` & Principal-Scoped Domain Ports (`src/kernel/mod.rs`, `src/kernel/ports.rs`)

**Files:**
- Create: `src/kernel/ports.rs`
- Create: `src/kernel/mod.rs`
- Modify: `src/main.rs` (add `mod kernel;`)

**Interfaces:**
- Produces in `src/kernel/ports.rs`:
  - `pub trait MemoryPort: Send + Sync`, `pub struct LadybugMemoryPort`
  - `pub trait ExecutivePort: Send + Sync`, `pub struct LadybugExecutivePort`
  - `pub trait OntologyPort: Send + Sync`, `pub struct LadybugOntologyPort`
- Produces in `src/kernel/mod.rs`:
  - `pub struct AgentKernel` (private fields, typed port accessors, `register_pointer`, `resolve_pointer`, `apply_external_effect`, `mediate_workflow_data`)

- [ ] **Step 1: Implement `src/kernel/ports.rs` and `src/kernel/mod.rs` with spy connector zero-I/O test**
  Ensure `AgentKernel` enforces `evaluate_connector_pre_gate` before any connector call and `evaluate_connector_post_gate` after any connector call, rejects duplicate `pointer_id`, enforces `rebac::can_read_memory` / `rebac::can_write_namespace`, and verifies active target-bound `action_receipt` on write-back.
- [ ] **Step 2: Run kernel unit tests**
  Run: `cargo test kernel::`
  Expected: PASS (including `pre_gate_deny_performs_zero_connector_io`).

---

### Task 5: MCP, Daemon, CLI Surfaces & Documentation Gate (`src/server.rs`, `src/daemon.rs`, `src/main.rs`, `docs/`)

**Files:**
- Modify: `src/server.rs`
- Modify: `src/daemon.rs`
- Modify: `src/main.rs`
- Create: `docs/features-api/agent-kernel-and-data-connectors.md`
- Create: `docs/extend/data-connectors.md`
- Modify: `docs/extend/overview.md`
- Modify: `docs/manifest.json`

**Interfaces:**
- MCP tool: `neurostrata_data_connector`
- HTTP endpoint: `POST /data-connector`
- CLI subcommand: `#[command(name = "data-connector")]` with `list`, `describe`, `register-pointer`, `resolve`, `apply-effect`, `mediate`

- [ ] **Step 1: Wire `AgentKernel` and `data-connector` across `src/server.rs`, `src/daemon.rs`, and `src/main.rs`**
- [ ] **Step 2: Create `docs/features-api/agent-kernel-and-data-connectors.md` and `docs/extend/data-connectors.md`, and update `docs/manifest.json`**
  Ensure every new public trait (`TaskStore`, `OntologyStore`, `MemoryPort`, `ExecutivePort`, `OntologyPort`, `DataConnector`), MCP tool (`neurostrata_data_connector`), and CLI command (`data-connector`) is documented in its target doc.
- [ ] **Step 3: Run full test & documentation gates**
  Run: `cargo test doc_gate::tests && bash scripts/task-coverage-gate.sh`
  Expected: PASS (`0` doc gate violations, `0` task coverage gate violations).
