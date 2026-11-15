# Central Agentic Kernel, Internal Port/Storage Traits, and Southbound DataConnector Architecture

**Date:** 2026-11-15  
**Task:** `neurostrata-bq4l`  
**Status:** Approved by K3 Architect (Incorporating Pre/Post Formal Gate Split, Trusted Pointer Resolution & Private Store Encapsulation)

---

## 1. Objective & Architectural Vision

Transform NeuroStrata into a unified **Central Agentic Kernel (`AgentKernel`)** with strict, non-leaky Rust trait boundaries at every layer:

1. **Northbound Workflow Edge (`src/workflow/connector.rs`):** Pluggable `WorkflowConnector` implementations (`GitWorkflowConnector`, `ManifestWorkflowConnector`) driving the 8-stage, 23-edge `WorkflowStage` state machine.
2. **Central Runtime Kernel (`src/kernel/`):** `AgentKernel` mediating between workflow lifecycle stages, cognitive recall, ontological action gates, and external data pools through three internal async domain port traits (`MemoryPort`, `ExecutivePort`, `OntologyPort`). `AgentKernel` does **not** expose a raw `pub store: Arc<LadybugStore>` field—all access flows through principal-scoped port methods.
3. **Internal Storage Engine (`LadybugStore` via `src/traits.rs`):** `LadybugDB` remains the authoritative core storage engine for the agent's brain (normative/episodic memory, AST/knowledge graph, tasks, SKOS taxonomy, digital twins, bitemporal action receipts, and `data_pointer` records). Subsystems interact with `LadybugStore` through async storage traits (`VectorStore`, `GraphRead`, `IntakeStore`, `TaskStore`, `OntologyStore`).
4. **Southbound External Data Pool Edge (`src/connectors/`):** A `DataConnector` trait and registry allowing NeuroStrata to index external data pools via lightweight `data_pointer` records in `LadybugDB`, resolve bounded data slices on demand by trusted `pointer_id` (never arbitrary caller-supplied URIs at resolve/write time), and execute CAS-guarded write-backs bound to active, non-reverted Kinetic Action receipts.
5. **Two-Phase Formal Verification (`src/formal_kernel.rs` + `proofs/NeuroStrata/DataConnector.lean`):** Split into a **Pre-I/O Authorization Gate** (`evaluate_connector_pre_gate`, guaranteeing **zero connector I/O on deny**) and a **Post-I/O Validation Gate** (`evaluate_connector_post_gate`, enforcing observed row/byte bounds, source SHA-256 integrity, and CAS preconditions), mechanized in Lean 4 with zero `sorry`/`axiom`.

---

## 2. System Topology

```
                   NORTHBOUND: TRANSPORTS & WORKFLOWS
      ┌──────────────┬──────────────┬───────────────────────────┐
      │ MCP (server) │ HTTP (daemon)│ CLI / WorkflowConnector   │
      └──────┬───────┴──────┬───────┴─────────────┬─────────────┘
             └──────────────┼─────────────────────┘
                            ▼
┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃                     NeuroStrata AgentKernel                   ┃
┃                     (src/kernel/mod.rs)                       ┃
┃                                                               ┃
┃  ┌─────────────────┐  ┌─────────────────┐  ┌───────────────┐  ┃
┃  │   MemoryPort    │  │  ExecutivePort  │  │ OntologyPort  │  ┃
┃  │  (ReBAC-scoped  │  │ (Tasks, Lock 2, │  │ (Actions,     │  ┃
┃  │   Recall/Ptrs)  │  │  WorkflowStage) │  │  Taxonomy,    │  ┃
┃  └────────┬────────┘  └────────┬────────┘  │  Twins)       │  ┃
┃           │                    │           └───────┬───────┘  ┃
┃           ▼                    ▼                   ▼          ┃
┃  ┌─────────────────────────────────────────────────────────┐  ┃
┃  │          Pure Formal Decision Kernel (Lean 4)           │  ┃
┃  │   • Pre-I/O Gate (deny => zero connector I/O)           │  ┃
┃  │   • Post-I/O Gate (bounds, source digest, CAS)          │  ┃
┃  └─────────────────────────────────────────────────────────┘  ┃
┃           │                    │                   │          ┃
┃           ▼                    ▼                   ▼          ┃
┃  ┌───────────────────┐  ┌────────────┐  ┌──────────────────┐  ┃
┃  │VectorStore +      │  │ TaskStore  │  │  OntologyStore   │  ┃
┃  │GraphRead + Intake │  │   Trait    │  │      Trait       │  ┃
┃  └─────────┬─────────┘  └──────┬─────┘  └─────────┬────────┘  ┃
┗━━━━━━━━━━━━┼───────────────────┼──────────────────┼────────━━━┛
             │                   │                  │
             ▼                   ▼                  ▼
    ┌───────────────────────────────────────────────────────┐
    │        LadybugStore (Agent Memory / Brain Engine)     │
    └───────────────────────────┬───────────────────────────┘
                                │ (trusted pointer_id lookup)
                                ▼
    ┌───────────────────────────────────────────────────────┐
    │      SOUTHBOUND: DataConnector Trait & Registry       │
    │     (InMemoryPoolConnector, JsonFilePoolConnector)    │
    └───────────────────────────────────────────────────────┘
```

---

## 3. Layer Specifications

### 3.1 Async Storage Traits (`src/traits.rs`)

Extend `src/traits.rs` with async `TaskStore` and `OntologyStore` traits implemented for `LadybugStore`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummaryRow {
    pub id: String,
    pub payload: MemoryPayload,
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn load_task_row(&self, namespace: &str, task_id: &str) -> Result<Option<MemoryPayload>>;
    async fn list_task_rows(&self, namespace: &str) -> Result<Vec<TaskSummaryRow>>;
    async fn count_extracted_memories_for_task(&self, namespace: &str, task_id: &str) -> Result<usize>;
}

#[async_trait]
pub trait OntologyStore: Send + Sync {
    async fn list_memories_by_types(
        &self,
        namespace: &str,
        memory_types: &[&str],
    ) -> Result<Vec<(String, MemoryPayload)>>;
    async fn get_memory_by_id(&self, namespace: &str, id: &str) -> Result<Option<MemoryPayload>>;
    async fn put_scoped_memory(
        &self,
        namespace: &str,
        id: &str,
        payload: &MemoryPayload,
    ) -> Result<()>;
}
```

### 3.2 Principal-Scoped Internal Kernel Domain Ports (`src/kernel/ports.rs`)

All port operations evaluate ReBAC (`rebac::resolve_principal`, `rebac::can_read_memory`, `rebac::can_write_namespace`) and never expose raw unguarded store mutations:

1. **`MemoryPort` (`LadybugMemoryPort`):**
   - `get_visible_memory(&self, namespace: &str, id: &str, principal: Option<&str>) -> Result<Option<MemoryPayload>>`
   - `register_data_pointer(&self, namespace: &str, record: &DataPointerRecord, principal: Option<&str>) -> Result<DataPointerRecord>`
   - `get_data_pointer(&self, namespace: &str, pointer_id: &str, principal: Option<&str>) -> Result<Option<DataPointerRecord>>`
   - `list_data_pointers(&self, namespace: &str, pool_id: Option<&str>, principal: Option<&str>) -> Result<Vec<DataPointerRecord>>`
2. **`ExecutivePort` (`LadybugExecutivePort`):**
   - `list_tasks(&self, namespace: &str, principal: Option<&str>) -> Result<Vec<TaskSummaryRow>>`
   - `get_task(&self, namespace: &str, task_id: &str, principal: Option<&str>) -> Result<Option<MemoryPayload>>`
   - `verify_task_extraction_count(&self, namespace: &str, task_id: &str) -> Result<usize>`
   - `evaluate_workflow_transition(&self, from: WorkflowStage, to: WorkflowStage, gate_passed: bool) -> KernelWorkflowTransitionVerdict`
3. **`OntologyPort` (`LadybugOntologyPort`):**
   - `list_ontology_entities(&self, namespace: &str, memory_types: &[&str], principal: Option<&str>) -> Result<Vec<(String, MemoryPayload)>>`
   - `get_ontology_entity(&self, namespace: &str, id: &str, principal: Option<&str>) -> Result<Option<MemoryPayload>>`
   - `verify_active_action_receipt(&self, namespace: &str, receipt_id: &str, pointer_id: &str, pool_id: &str) -> Result<KernelActionReceiptState>`

### 3.3 Southbound `DataConnector` Interface & Trusted Pointer Model (`src/connectors/mod.rs`)

* **Vocabulary & Privacy:**
  * `data_pointer` is added to `src/schemas/memory-vocabulary.v3.json`.
  * `data_pointer` rows are stored in `LadybugDB` with `eligibility: "private"` and are excluded from `SHAREABLE_MEMORY_TYPES` so local paths and enterprise pool endpoints never cross SynapticMesh.
  * Duplicate active `connector_id` in `DataConnectorRegistry` or duplicate active `pointer_id` in a namespace is rejected with an explicit error.
* **Trusted `pointer_id` Resolution:**
  * Callers register a `DataPointerRecord` once via `register_pointer` (gated by `RegisterPointer` pre-gate, ReBAC write permission, and `check_caller_path` for local file connectors).
  * Subsequent `resolve_pointer` and `apply_effect` calls take `pointer_id` (NOT a raw URI). The kernel loads the trusted `DataPointerRecord` from `LadybugDB` and passes its `pool_id` and `pointer_uri` to the registered `DataConnector`.
* **Source Digest vs. Slice Digest:**
  * `expected_source_sha256` on `DataPointerRecord` tracks the SHA-256 of the underlying pool resource (`source_sha256` returned by the connector).
  * `slice_sha256` on `DataSlicePayload` hashes the specific `[offset .. offset + limit]` slice returned to the caller.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataPoolAccessMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPoolSchema {
    pub connector_id: String,
    pub pool_id: String,
    pub access_mode: DataPoolAccessMode,
    pub entity_types: Vec<String>,
    pub max_slice_rows: u32,
    pub max_slice_bytes: u32,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPointerRecord {
    pub pointer_id: String,
    pub connector_id: String,
    pub pool_id: String,
    pub pointer_uri: String,
    pub entity_type: String,
    pub expected_source_sha256: Option<String>,
    pub captured_at_secs: u64,
    pub max_staleness_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSliceRequest {
    pub pointer_id: String,
    pub offset: u32,
    pub limit: u32,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSlicePayload {
    pub pointer_id: String,
    pub connector_id: String,
    pub pool_id: String,
    pub pointer_uri: String,
    pub records: Vec<Value>,
    pub payload_bytes: u32,
    pub source_sha256: String,
    pub slice_sha256: String,
    pub observed_at_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEffectRequest {
    pub pointer_id: String,
    pub operation: String,
    pub payload: Value,
    pub action_receipt_id: String,
    pub expected_source_sha256: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEffectReceipt {
    pub pointer_id: String,
    pub connector_id: String,
    pub pool_id: String,
    pub operation: String,
    pub idempotency_key: String,
    pub applied: bool,
    pub prior_source_sha256: String,
    pub new_source_sha256: String,
    pub details: String,
}

#[async_trait]
pub trait DataConnector: Send + Sync {
    fn connector_id(&self) -> &str;
    async fn describe_pool(&self, pool_id: &str) -> Result<DataPoolSchema>;
    async fn resolve_slice(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        offset: u32,
        limit: u32,
        max_bytes: u32,
    ) -> Result<DataSlicePayload>;
    async fn apply_external_effect(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        req: &ExternalEffectRequest,
    ) -> Result<ExternalEffectReceipt>;
}
```

### 3.4 Two-Phase Lean 4 `DataConnectorGate` (`src/formal_kernel.rs` & `proofs/NeuroStrata/DataConnector.lean`)

#### Phase 1: Pre-I/O Gate (`evaluate_connector_pre_gate`)
Evaluated **before** invoking any `DataConnector` read/write method. Any `Deny*` verdict aborts immediately with **zero connector I/O**.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelConnectorOp {
    RegisterPointer,
    ResolveSlice,
    ApplyWriteBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelActionReceiptState {
    NotRequired,
    ActiveBound,
    MissingOrNotFound,
    RevertedOrSuperseded,
    TargetMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
```

**Pre-Gate Precedence:**
1. `!connector_registered` $\rightarrow$ `DenyUnregisteredConnector`
2. `!rebac_allowed` $\rightarrow$ `DenyRebacAccess`
3. `op == RegisterPointer` $\rightarrow$ `AllowPreRegister`
4. `op == ApplyWriteBack && !pool_writable` $\rightarrow$ `DenyReadOnlyPool`
5. `op == ApplyWriteBack && receipt_state != ActiveBound` $\rightarrow$ `DenyInvalidActionReceipt`
6. `op == ResolveSlice && (requested_rows == 0 || requested_rows > max_slice_rows || requested_bytes == 0 || requested_bytes > max_slice_bytes)` $\rightarrow$ `DenySliceLimitExceeded`
7. `clock_skew_future` (`now_secs < captured_at_secs`) $\rightarrow$ `DenyClockSkewFuture`
8. `max_staleness_secs > 0 && age_secs > max_staleness_secs` $\rightarrow$ `DenyPointerStale`
9. Otherwise: `AllowPreResolve` (when `ResolveSlice`) or `AllowPreWriteBack` (when `ApplyWriteBack`).

#### Phase 2: Post-I/O Gate (`evaluate_connector_post_gate`)
Evaluated **after** connector read or before finalizing write-back CAS:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelConnectorPostGateInput {
    pub op: KernelConnectorOp,
    pub observed_rows: u32,
    pub max_slice_rows: u32,
    pub observed_bytes: u32,
    pub max_slice_bytes: u32,
    pub source_digest_matches: bool,
    pub cas_expected_matches: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelConnectorPostVerdict {
    AllowPostOutput,
    DenyObservedBoundsExceeded,
    DenySourceDigestMismatch,
    DenyCasConflict,
}
```

**Post-Gate Precedence:**
1. `op == ResolveSlice && (observed_rows > max_slice_rows || observed_bytes > max_slice_bytes)` $\rightarrow$ `DenyObservedBoundsExceeded`
2. `!source_digest_matches` $\rightarrow$ `DenySourceDigestMismatch`
3. `op == ApplyWriteBack && !cas_expected_matches` $\rightarrow$ `DenyCasConflict`
4. Otherwise: `AllowPostOutput`.

### 3.5 Central `AgentKernel` & Transport Parity (`src/kernel/mod.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs`)

```rust
pub struct AgentKernel {
    memory: Arc<dyn MemoryPort>,
    executive: Arc<dyn ExecutivePort>,
    ontology: Arc<dyn OntologyPort>,
    workflows: Arc<WorkflowRegistry>,
    connectors: Arc<DataConnectorRegistry>,
    bus: Arc<ThalamicBus>,
}
```
- Exposes typed accessors (`memory()`, `executive()`, `ontology()`, `workflows()`, `connectors()`, `bus()`) and unified `execute_connector_command(...)` / `mediate_workflow_data(...)`.
- Wired across:
  - **MCP Tool:** `neurostrata_data_connector`
  - **HTTP Daemon Endpoint:** `POST /data-connector`
  - **CLI Subcommand:** `neurostrata-mcp data-connector <list|describe|register-pointer|resolve|apply-effect|mediate>`

---

## 4. Verification & Documentation Requirements

1. **Lean 4 Proofs (`proofs/NeuroStrata/DataConnector.lean`):**
   - Prove determinism, fail-closure (`unregistered_always_denied`, `rebac_denied_always_denied`, `readonly_never_allows_writeback`, `writeback_requires_active_bound_receipt`, `future_clock_denied`, `post_digest_mismatch_denied`, `post_cas_conflict_denied`).
   - Differential parity test in `tests/lean_parity_test.rs` covering boundary values (`0`, `1`, `max`, `max + 1`) and full verdict coverage.
2. **Zero-I/O on Pre-Gate Deny Test:**
   - Unit test with an atomic call-counting spy `DataConnector` proving that every `Deny*` verdict from `evaluate_connector_pre_gate` results in `0` invocations of `resolve_slice` / `apply_external_effect`.
3. **Documentation Gate (`docs/manifest.json` & `src/doc_gate.rs`):**
   - Add `docs/features-api/agent-kernel-and-data-connectors.md` (Tier 2) and `docs/extend/data-connectors.md` (Tier 3).
   - Register `src/kernel/mod.rs`, `src/kernel/ports.rs`, and `src/connectors/mod.rs` in `docs/manifest.json`.
