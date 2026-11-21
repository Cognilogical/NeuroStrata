# 2026-11-20 — `memory_pool` Write-Back Durability (W-C)

> **Baseline**: `cognilogical/main` @ `216e9d56`
> **Issue**: #331 (P1)
> **Branch**: `fix/wc-memory-pool-durability`
> **Workflow**: spec → red eval → minimal fix → green eval → regression suites

---

## 1. Scope

`AgentKernel::apply_external_effect` and the `memory_pool` connector's
in-memory write path. The serving shape builds a fresh kernel per request
(`handle_data_connector`), so an effect that only mutates the request-local
connector is lost for every subsequent request.

Non-goals: the connector trait's request/response shapes, `json_file`
behavior (already file-backed), pointer/CAS semantics, and the data-pool
pre-gates.

---

## 2. Invariant

A successful `apply_effect` receipt is durable across the kernel/request
boundary for every connector whose records are not already durable.

Equivalently: if `apply_effect` returns `applied: true` with
`new_source_sha256`, a fresh kernel over the same store must resolve the
post-write records with that same source SHA.

---

## 3. Behavior specification

1. **Snapshot after a successful effect.** After the connector effect and
   the post-gate pass, and before the receipt is returned, the kernel asks
   the connector for a full snapshot of the pool's records.
2. **Connector contract.** `DataConnector::snapshot_pool_records(pool_id)`
   defaults to `Ok(None)` — a connector whose backing store is already
   durable (e.g. `json_file`) opts out and is not rewritten.
   `InMemoryPoolConnector` implements it by cloning its complete per-pointer
   record map (not a capped slice).
3. **Persist a delta, not a full replacement.** The snapshot carries the
   written pointer's post-effect records plus the pool's idempotency
   receipts. The persistence port merges that pointer into the stored
   `pool_records` (other pointers are left intact) and merges receipts by
   key. Persistence is serialized across kernels by a process-level turn.
4. **Same-pointer concurrency is a conflict.** The snapshot also carries the
   pointer's `prior_records` (or the current records on an idempotent
   replay). If the stored row no longer matches, the effect returns a
   conflict error instead of overwriting a concurrent write.
5. **Idempotency is durable.** `pool_idempotency` is persisted with the
   records and restored during hydration, so a retry through a fresh kernel
   returns the original receipt and never appends twice.
6. **No false receipt.** If the snapshot or persistence fails, the effect
   returns an error instead of a success receipt that cannot be observed.
7. **Same-kernel visibility is unchanged**: the in-memory effect is visible
   immediately, and the persist step does not mutate the request-local
   connector again.

---

## 4. Eval (must fail on baseline)

`memory_pool_write_back_survives_a_fresh_kernel`:

- register a `memory_pool` pool `durable_mem` with `mem://items =
  [{"v":1}]` and a pointer `ptr_durable`;
- seed an active action receipt for that pointer/pool;
- `apply_effect` append `{"v":2}` through kernel #1;
- assert the receipt is `applied`, and kernel #1 resolves 2 records;
- build kernel #2 over the same store (the per-request serving shape) and
  resolve the same pointer: assert 2 records and `source_sha256` equal to
  the receipt's `new_source_sha256`.

Baseline result: kernel #2 returns 1 record and the pre-write SHA — the
write vanished despite the success receipt.

Additional evals:

- `concurrent_pool_write_backs_preserve_other_pointers` — two kernels hydrate
  the same pool, each writes a different pointer, and a third kernel sees
  both post-write records. Baseline: the second full-pool persist erases the
  first write.
- `idempotent_write_back_replay_through_fresh_kernel_does_not_duplicate` —
  replaying the same idempotency key through a fresh kernel returns the
  original receipt and leaves exactly two records. Baseline: hydration omits
  the receipt and the payload is appended twice.

Negative control (existing coverage): `json_file` registration/resolution
across kernels continues to pass; its connector returns no snapshot so the
kernel does not rewrite it.

---

## 5. Acceptance

- The new eval is red at `216e9d56` and green after the fix.
- Existing kernel connector tests (pre-gates, CAS, cross-kernel pool
  registration/hydration) stay green.
- `cargo test --bin neurostrata-mcp kernel::` and the final daemon bin suite
  pass.
- No MCP/HTTP/CLI shape change; the data-connector docs describe the
  durability rule.

## 6. Doc gate

`src/kernel/mod.rs`, `src/kernel/ports.rs`, and `src/connectors/mod.rs` are
watched surfaces: this change co-updates
`docs/features-api/agent-kernel-and-data-connectors.md` and
`docs/extend/data-connectors.md`.
