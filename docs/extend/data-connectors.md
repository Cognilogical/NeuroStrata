# Southbound DataConnector & Trusted DataPointer Model

## DataConnector Trait

```rust
pub trait DataConnector: Send + Sync {
    fn connector_id(&self) -> &str;
    async fn describe_pool(&self, pool_id: &str) -> Result<DataPoolSchema>;
    async fn register_pool(&self, schema: DataPoolSchema) -> Result<DataPoolSchema>;
    async fn seed_records(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        records: Vec<Value>,
    ) -> Result<()>;
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

## Registry

- `DataConnectorRegistry`: `new`, `new_with_defaults`, `register` (duplicate `connector_id` rejected), `get`, `list_connector_ids`.
- `new_with_defaults` pre-seeds `"default"` (`ReadWrite`, `max_slice_rows: 1000`, `max_slice_bytes: 1048576`) and `"readonly"` (`ReadOnly`, `max_slice_rows: 1000`, `max_slice_bytes: 1048576`) pools on both built-in connectors, and loads any workspace pool schemas declared in `.NeuroStrata/connector_pools.json`.
- Runtime pool schemas (and optional initial in-memory records) can also be registered and persisted via `AgentKernel::register_pool` (`execute_connector_command` with `"operation": "register_pool"` or `"upsert_pool"`, and listed via `"list_pools"`); persisted namespace overrides take precedence over built-in constructor defaults and preserve existing seeded records when `records` is omitted.

## Implementations

- `InMemoryPoolConnector` — `connector_id = "memory_pool"`.
- `JsonFilePoolConnector` — `connector_id = "json_file"`; caller-path authority via `daemon::check_caller_path`; source SHA-256 + slice SHA-256; CAS + idempotency on write-back.

## Slice & Pool Contracts

- `DataPoolAccessMode` (`ReadOnly`, `ReadWrite`) scopes pool write-back.
- `DataPoolSchema` binds `connector_id`, `pool_id`, `access_mode`, `entity_types`, `max_slice_rows`, `max_slice_bytes`, `description`.
- `DataSliceRequest` carries `pointer_id`, `offset`, `limit`, `max_bytes`.
- `DataSlicePayload` carries `records`, `payload_bytes`, `source_sha256`, `slice_sha256`, `observed_at_secs`.

## Trusted Pointer Model

- `DataPointerRecord`: `pointer_id`, `connector_id`, `pool_id`, `pointer_uri`, `entity_type`, `expected_source_sha256`, `captured_at_secs`, `max_staleness_secs`.
- Resolve/write-back accept `pointer_id` only — never caller-supplied URI.
- `data_pointer` memory type: `eligibility: "private"`, excluded from `SHAREABLE_MEMORY_TYPES`.

## Fail-Closed Pre/Post Gates

- Pre-I/O: `evaluate_connector_pre_gate` — Deny unregistered connector, ReBAC denial, read-only pool on write-back, invalid receipt, slice limit exceeded, future clock skew, stale pointer.
- Post-I/O: `evaluate_connector_post_gate` — Deny observed bounds exceeded, source digest mismatch, CAS conflict.
- Namespace-isolated pool overrides: persisted pool registrations and built-in pool overrides are hydrated under `AgentKernel::scoped_connector_pool_id(namespace, pool_id)` so overriding a pool in one namespace never mutates another namespace's connector state.
- Zero connector I/O on early denial: an unregistered connector or ReBAC denial aborts before `describe_pool`, `resolve_slice`, and `apply_external_effect`. All other pre-gate `Deny*` verdicts run after `describe_pool` but before `resolve_slice`/`apply_external_effect`; post-gate `Deny*` verdicts reject the connector's returned output.

## Write-Back Receipts

- `ExternalEffectRequest` / `ExternalEffectReceipt`; idempotency key; active target-bound `action_receipt` (`ActiveBound`). Write-back requires CAS + idempotency key.
- Target binding comes from the production receipt payload (`metadata.action_receipt`), not from editable row metadata: the nested `receipt_id` must equal the requested receipt and the stamped `target_pointer_id`/`target_pool_id` (or the pre-stamp nested `pointer_id`/`pool_id` parameter pair) must equal the write-back pointer and pool. A receipt whose action never declared that pair is unbound and denied.
- The kinetic action writer stamps `target_pointer_id`/`target_pool_id` in the nested receipt when the action declares both reserved parameters; receipt rows keep no top-level `pointer_id`/`pool_id`.

## Write-Back Durability

- `DataConnector::snapshot_write_back(pool_id, pointer_uri)` defaults to `Ok(None)`; `None` means the connector already persists its own state.
- `InMemoryPoolConnector` returns the written pointer's post-effect records, its prior records, and the pool's idempotency receipts. After a successful effect and post-gate, the kernel merges that delta into the pool registry row under a process-wide turn before returning the receipt.
- Merging is per-pointer: concurrent kernels writing different pointers both survive; a same-pointer write whose stored records no longer match `prior_records` is a conflict, not a last-writer-wins overwrite.
- A pointer missing from an existing row is a removal by a newer replacement, not an invitation to resurrect it; the commit is rejected when the snapshot has prior records.
- The persisted `pool_schema` is preserved and its access mode revalidated at commit: a pool changed to read-only after hydration rejects the write-back instead of restoring the stale read-write schema. Pool registration and write-back share one persistence turn.
- A pool archived after hydration is rejected at commit; a write-back must never revive a retired row.
- Receipt-load failures during hydration fail the request instead of silently omitting receipts and allowing a retried effect to duplicate.
- A rejected persistence rolls the request-local connector back to `prior_records`, drops the uncommitted receipt, and then returns the error: a reused kernel cannot serve the failed mutation or replay it as success.
- `pool_idempotency` is persisted with the records and restored during hydration, so a retry through a fresh kernel returns the original receipt and cannot append twice.
- A schema-only `upsert_pool` (no `records`) preserves `pool_idempotency` alongside the retained `pool_records`; it must not orphan receipts from their records.
- A snapshot carries only the receipt produced by the written pointer's latest effect. A receipt whose persistence was rejected is never promoted by a later write to another pointer, so it cannot replay as `applied: true` without durable records.
- `JsonFilePoolConnector` returns `None` (file-backed) and is not rewritten. A snapshot or persistence failure rejects the effect rather than returning an unobservable success receipt.
- Durable retry key (`pool_idempotency`, shared by every connector): `[action_receipt_id, pointer_uri, idempotency_key]` as an exact-value JSON array (digest-free, so #351's source-digest collision cannot merge two effects). The applied `ExternalEffectReceipt` is persisted under it in the same registry row the next kernel hydrates from. A fresh-kernel retry with the same triple returns the original receipt and performs no connector I/O; a different key or payload is new work and still passes CAS. The row contains no `pool_records` for `json_file`, so the connector remains file-backed while the retry key is durable.
- Retry-key lookup, connector apply, write-back persistence, and the retry-key write run under one process-wide `POOL_WRITEBACK_LOCK` turn, so two requests in the serving process cannot both miss the key. Cross-process concurrent writers are outside the single-writer daemon contract.
- A retry-key write failure fails the request after the connector applied; no success receipt is returned. A self-persisting connector has no prior-byte rollback, so the caller must resolve the pointer before retrying as new work.

## Transport Parity

- MCP tool: `neurostrata_data_connector`
- HTTP endpoint: `POST /data-connector`
- CLI: `neurostrata-mcp data-connector <list|describe|register-pointer|resolve|apply-effect|mediate>`
- Caller-originated pointer reads (`list_pointers`, `resolve_pointer`, `mediate`) require a named `principal` on every transport; omitting it is rejected at the external boundary instead of being forwarded as the port's trusted `None`, which stays kernel-internal. `apply_effect` is unchanged and fails closed in its pre-gate.

## Verification

- `cargo test --bin neurostrata-mcp kernel::`
- `cargo test --bin neurostrata-mcp connectors::tests`
- `cargo test --test lean_parity_test`
