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

- `MemoryPort` (`LadybugMemoryPort`): `get_visible_memory`, `register_data_pointer`, `get_data_pointer`, `list_data_pointers`, `register_data_pool`, `list_data_pools`.
- `ExecutivePort` (`LadybugExecutivePort`): `list_tasks`, `get_task`, `verify_task_extraction_count`, `evaluate_workflow_transition`.
- `OntologyPort` (`LadybugOntologyPort`): `list_ontology_entities`, `get_ontology_entity`, `verify_active_action_receipt`. Receipt verification reads the production shape: the receipt is parsed from the nested `metadata.action_receipt`, its `receipt_id` must equal the requested id, and its stamped `target_pointer_id`/`target_pool_id` (or, for receipts written before the stamp, the same reserved `pointer_id`/`pool_id` parameter pair) must equal the pointer and pool being written. Top-level row metadata is never a binding source, and an active receipt with no binding is denied.

## Central AgentKernel

- Constructor: `AgentKernel::new`, `AgentKernel::with_ports`.
- Typed accessors: `memory()`, `executive()`, `ontology()`, `workflows()`, `connectors()`, `bus()`.
- Operations: `register_pool` (persisting namespace-scoped pool schemas via `scoped_connector_pool_id` and preserving stored `pool_records` when `records` is omitted; `describe_pool_with_hydration` applies persisted namespace overrides without mutating other namespaces' built-in defaults), `register_pointer`, `resolve_pointer`, `apply_external_effect`, `execute_connector_command` (`list_connectors`, `describe_pool`, `register_pool`, `upsert_pool`, `list_pools`, `register_pointer`, `list_pointers`, `resolve_pointer`, `apply_effect`, `mediate`), `mediate_workflow_data`.
- Write-back durability: before an `apply_external_effect` receipt is returned, the kernel snapshots connectors whose records are not self-persisted (`DataConnector::snapshot_write_back`: the written pointer's post-effect records, its prior records, and the receipt produced by that effect) and merges them into the pool registry row (`register_pool_writeback`) under a process-wide turn shared with pool registration. Other pointers in the pool are preserved; a same-pointer concurrent write, a pointer removed by a newer record replacement, a pool archived after hydration, and a pool that became read-only are reported as conflicts rather than overwritten or revived; the persisted schema is preserved and revalidated at commit. A rejected persistence also rolls the request-local connector back to its pre-effect records and drops the uncommitted receipt, so a reused kernel cannot leak the failed write. `pool_idempotency` is restored on hydration (a receipt-load failure fails the request) so fresh-kernel retries return the original receipt instead of duplicating the effect. A schema-only `upsert_pool` retains both `pool_records` and `pool_idempotency`, and a receipt from a rejected persistence is never included in a later snapshot — only committed effects replay as no-ops. Connectors that already persist return `None` and are not rewritten.

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
- `list_pointers`, `resolve_pointer`, `apply_effect`, `register_pointer`, and pool operations exclude retired/inactive rows (`valid_to` expired, `archived == true`, `retired == true`, or non-empty `superseded_by`).
- `handle_data_connector` defaults omitted `namespace` to `"global"` and omitted `principal` to `None`, while rejecting present non-string, blank/whitespace, or whitespace-padded `namespace` and `principal` values before `AgentKernel` construction; pool operations (`register_pool`, `upsert_pool`, `list_pools`) are also accepted at runtime via `execute_connector_command`.

## Verification

- `cargo test --bin neurostrata-mcp kernel::`
- `cargo test --bin neurostrata-mcp connectors::tests`
- `cargo test --test lean_parity_test`
- `bash scripts/task-coverage-gate.sh`
