# Central Agentic Kernel & DataConnector

Central Agentic Kernel (`AgentKernel`) unifies workflow lifecycle, cognitive recall, ontological action gates, and southbound external data pools behind strict trait boundaries.

## Layer Topology

1. Northbound transports — MCP (`src/server.rs`), HTTP daemon (`src/daemon.rs`), CLI (`src/main.rs`), workflow connectors (`src/workflow/`).
2. Central kernel — `AgentKernel` (`src/kernel/mod.rs`).
3. Internal storage — `TaskStore`, `OntologyStore`, `VectorStore`, `GraphRead`, `IntakeStore` on `LadybugStore` (`src/traits.rs`, `src/store/ladybug.rs`).
4. Southbound data — `DataConnector` registry (`src/connectors/mod.rs`).

## Internal Storage Traits

- `TaskStore` (`src/traits.rs`): `TaskSummaryRow { id, payload }`, `load_task_row`, `list_task_rows`, `count_extracted_memories_for_task`.
- `OntologyStore` (`src/traits.rs`): `list_memories_by_types`, `get_memory_by_id`, `put_scoped_memory`.

## Principal-Scoped Kernel Ports

- `MemoryPort` (`LadybugMemoryPort`): `get_visible_memory`, `register_data_pointer`, `get_data_pointer`, `list_data_pointers`.
- `ExecutivePort` (`LadybugExecutivePort`): `list_tasks`, `get_task`, `verify_task_extraction_count`, `evaluate_workflow_transition`.
- `OntologyPort` (`LadybugOntologyPort`): `list_ontology_entities`, `get_ontology_entity`, `verify_active_action_receipt`.

## Central AgentKernel

- Constructor: `AgentKernel::new`, `AgentKernel::with_ports`.
- Typed accessors: `memory()`, `executive()`, `ontology()`, `workflows()`, `connectors()`, `bus()`.
- Operations: `register_pointer`, `resolve_pointer`, `apply_external_effect`, `execute_connector_command`, `mediate_workflow_data`.

## Two-Phase DataConnector Gate

- Pre-I/O gate: `evaluate_connector_pre_gate` (`KernelConnectorOp`, `KernelConnectorPreVerdict`). Early unregistered-connector/ReBAC denials abort before `describe_pool`; subsequent pre-gate checks run after schema lookup but before `resolve_slice`/`apply_external_effect`.
- Post-I/O gate: `evaluate_connector_post_gate` (`KernelConnectorPostVerdict`). Enforces observed slice bounds, source SHA-256 digest match, CAS expectations.

## Fail-Closed Guarantees

- Zero slice/effect I/O on pre-gate deny: every `Deny*` pre-gate verdict returns before `resolve_slice`/`apply_external_effect`; early unregistered-connector/ReBAC denial also skips `describe_pool`.
- Trusted `pointer_id` only: resolve/write-back accept pointer id, never caller URI; stale pointers denied.
- `data_pointer` rows private; excluded from `SHAREABLE_MEMORY_TYPES`.
- Write-back requires active, target-bound `action_receipt` + CAS + idempotency key.

## Strict JSON Conventions

- MCP tool / HTTP endpoint / CLI dispatch all flow through `AgentKernel::execute_connector_command` with strict JSON `operation` payloads; malformed payloads fail closed.

## Transport Parity

- MCP tool: `neurostrata_data_connector`
- HTTP endpoint: `POST /data-connector`
- CLI: `neurostrata-mcp data-connector <list|describe|register-pointer|resolve|apply-effect|mediate>`

## Schema Discovery

The `neurostrata_data_connector` MCP tool discloses an operation-discriminated `args` schema (`oneOf` keyed on `operation`): each of `list_connectors`, `list_pointers`, `describe_pool`, `register_pointer`, `resolve_pointer`, `apply_effect`, and `mediate` declares its own required fields, including the nested `DataPointerRecord`, `DataSliceRequest`, and `ExternalEffectRequest` layouts. The schema is discovery-only: the runtime still accepts anything it accepts today, so clients are never constrained by the disclosure.

- `now_secs` (optional u64) is accepted by `resolve_pointer`, `apply_effect`, and `mediate`; omission defaults to the current epoch.
- `mediate` accepts optional `gate_passed` (boolean); omission defaults to `false` at runtime.
- `list_pointers` omits retired pointers from its output.

## Verification

- `cargo test --bin neurostrata-mcp kernel::`
- `cargo test --bin neurostrata-mcp connectors::tests`
- `cargo test --test lean_parity_test`
- `bash scripts/task-coverage-gate.sh`
