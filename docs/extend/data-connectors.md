# Southbound DataConnector & Trusted DataPointer Model

## DataConnector Trait

```rust
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

## Registry

- `DataConnectorRegistry`: `new`, `new_with_defaults`, `register` (duplicate `connector_id` rejected), `get`, `list_connector_ids`.

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
- Zero connector I/O on early denial: an unregistered connector or ReBAC denial aborts before `describe_pool`, `resolve_slice`, and `apply_external_effect`. All other pre-gate `Deny*` verdicts run after `describe_pool` but before `resolve_slice`/`apply_external_effect`; post-gate `Deny*` verdicts reject the connector's returned output.

## Write-Back Receipts

- `ExternalEffectRequest` / `ExternalEffectReceipt`; idempotency key; active target-bound `action_receipt` (`ActiveBound`). Write-back requires CAS + idempotency key.

## Transport Parity

- MCP tool: `neurostrata_data_connector`
- HTTP endpoint: `POST /data-connector`
- CLI: `neurostrata-mcp data-connector <list|describe|register-pointer|resolve|apply-effect|mediate>`

## Verification

- `cargo test --bin neurostrata-mcp kernel::`
- `cargo test --bin neurostrata-mcp connectors::tests`
- `cargo test --test lean_parity_test`
