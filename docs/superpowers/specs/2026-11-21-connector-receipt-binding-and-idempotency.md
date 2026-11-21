# 2026-11-21 — Connector Receipt Binding & Durable Idempotency (W-D)

> **Baseline**: `work/main` @ `a75b243a` (follows #347 @ `d1a8d7c6`)
> **Issues**: #352 (P1), #350 (P1) — one change, paired
> **Branch**: `fix/connector-receipt-binding-idempotency`
> **Workflow**: Gate 0 trace → invariant → red evals → minimal fail-closed fix → green

---

## 1. Why these two ship together

#352 and #350 are a merge pair, not independent fixes.

1. Today the verifier rejects every receipt the production writer emits, so
   action-authorized write-back never reaches a connector. #350's duplicate
   append is therefore unreachable through a genuine receipt.
2. Teaching the verifier the production shape makes write-back reachable.
   Without #350 in the same change, **every genuine retry** of an unpinned
   `json_file` append applies the mutation twice.

Merging #352 alone converts a fail-closed capability loss into a silent
data-integrity defect. The branch and PR therefore carry both fixes; the PR
must not be split or partially merged.

---

## 2. Gate 0 trace — #352

Production writer: `apply_action` (`src/rdf/action.rs:2455-2470`) writes the
receipt row as

```
metadata = {
  "eligibility": "private",
  "valid_from": <ts>,
  "action_receipt": { receipt_id, action_slug, principal, executed_at,
                      params, superseded_pairs, created_ids, twin_deltas,
                      edge_deltas, row_deltas, task_deltas, reverted_at, ... }
}
```

There is no top-level `pointer_id`, `pool_id`, `target_pointer_id`, or
`receipt` key. `ActionReceiptPayload` (`src/rdf/action.rs:688-715`) carries no
target fields either; an action's target strings, if any, live inside
`params`, still nested in the receipt.

Verifier: `verify_active_action_receipt` (`src/kernel/ports.rs:849-903`)
requires top-level `metadata.pointer_id` **and** `metadata.pool_id`, with a
fallback through top-level `metadata.target_pointer_id` +
`metadata.receipt`. Any other shape returns `TargetMismatch`, which the
pre-gate maps to `DenyInvalidActionReceipt` before connector I/O.

Grep over `src/` confirms the only writer of top-level `pointer_id` /
`pool_id` receipt metadata is the kernel test helper `seed_receipt`
(`src/kernel/mod.rs`). The kernel suite was green on a shape production has
never written.

**Conclusion: the target is not recoverable from the production receipt.**
The fix needs a writer change plus a transitional read for receipts already
written (in-flight during rollout).

### Writer change

`ActionReceiptPayload` gains `target_pointer_id` / `target_pool_id`, stamped
at apply time from the action's declared `pointer_id` / `pool_id` parameter
pair (both or neither). The binding is part of the immutable nested receipt,
not editable row metadata, and is only present when the authorized action
actually declared the connector target.

### Verifier binding order (never "any active receipt")

1. Nested `metadata.action_receipt.target_pointer_id` + `target_pool_id`
   (new writer shape). The nested `receipt_id` must equal the requested id.
2. Transitional: nested `metadata.action_receipt.params.pointer_id` +
   `params.pool_id` for receipts written before the stamp existed. Same
   `receipt_id` equality.
3. Otherwise `TargetMismatch`.

There is no fallback to an unbound but active receipt.

---

## 3. Gate 0 trace — #350

`handle_data_connector` (`src/server.rs:2085`) builds a fresh `AgentKernel`
per request; `new_with_defaults` builds a fresh `JsonFilePoolConnector` whose
`idempotency: HashMap` starts empty. Dedup is correct within one instance and
void across instances. The receipt is never consumed, and
`expected_source_sha256` is optional (the kernel's own request shape leaves it
`None`), so a fresh-kernel retry appends again.

#347 already introduced the durable connector state row: the
`__data_pool__:<namespace>:<connector>:<pool>` registry row holding
`pool_records` + `pool_idempotency`, written before a success receipt is
returned and hydrated by the next kernel. This change reuses that row; it does
not add a second store.

Durable dedup key (digest-free, exact-value JSON array):

```
[action_receipt_id, pointer_uri, idempotency_key]
```

`pointer_uri` is the trusted record's URI (never caller-supplied). The
applied `ExternalEffectReceipt` is persisted under that key in the same row's
`pool_idempotency` map.

A retry that reaches the kernel with the same triple:
- still passes pointer lookup, ReBAC, receipt verification, and the pre-gate
  (so a revoked pointer, retired pool, read-only pool, or reverted receipt
  still denies);
- then returns the persisted original receipt **before any connector I/O**;
- never re-applies the effect.

Write-back dedup lookup, connector apply, #347 snapshot persistence, and the
retry-key persistence run under the same process-wide `POOL_WRITEBACK_LOCK`
turn, so two requests inside the serving process cannot both miss the key and
apply. (Cross-process deployment is out of scope: the daemon is single
writer; the registry row remains the source of truth.)

---

## 4. Invariants

1. A receipt accepted for a write-back is bound to that exact `pointer_id` +
   `pool_id` in its own immutable nested payload.
2. A wrong-target receipt is denied as `TargetMismatch` before connector I/O;
   zero connector calls on denial.
3. An unbound or unknown receipt shape is denied, even if active.
4. The same `(action_receipt_id, pointer_uri, idempotency_key)` applied twice
   yields one mutation and the original receipt, across kernel instances.
5. Same-instance replay keeps its current behavior (connector-local map).
6. CAS enforcement is unchanged for requests that are not exact replays:
   a stale `expected_source_sha256` still denies.
7. #347 semantics stay intact: `memory_pool` records and receipts persist;
   `json_file` stays file-backed (`snapshot_write_back` → `None`) and gains
   only the durable retry key.

---

## 5. Evals (red first)

| Eval | Red on |
| --- | --- |
| `production_receipt_binds_its_write_back_target` | #352 baseline: `TargetMismatch` for the production shape; wrong-target case already denied |
| `in_flight_receipt_binds_via_nested_params` | #352 baseline: transitional nested binding rejected |
| `connector_receipt_writer_stamps_target_binding` | #352 baseline: writer emits no nested target fields |
| `json_file_unpinned_append_retry_through_fresh_kernel_applies_once` | #350 (after #352): fresh kernel appends twice |
| `json_file_same_instance_replay_is_a_no_op` | green on baseline (regression guard) |
| `json_file_stale_pinned_retry_still_conflicts` | green on baseline (regression guard) |
| `memory_pool_*` (#347 suites) | green on baseline (regression guard) |

All kernel evals use `LadybugStore::for_testing` and the production receipt
shape (nested `metadata.action_receipt`), not the synthetic top-level shape.

---

## 6. Migration & risk

- **In-flight receipts.** Rows written by the old writer without the stamp
  remain usable through the nested `params.pointer_id`/`params.pool_id`
  transitional path; no backfill is required. Receipts whose action never
  declared the pair stay unbound and are denied — they cannot authorize a
  connector target they never named.
- **Retired test shape.** The synthetic top-level `pointer_id`/`pool_id`
  receipt metadata is no longer accepted; the kernel test helper now seeds
  the production shape. No production writer emitted the synthetic shape, so
  no stored row relies on it.
- **New pool rows.** A first successful write-back on a built-in pool (e.g.
  `json_file:default`) persists a registry row without `pool_records`, so the
  pool becomes visible in `list_pools` for that namespace. This mirrors #347's
  accepted visibility change for `memory_pool`. `json_file` records are not
  copied; the connector stays file-backed.
- **Dedup persistence failure.** If the retry-key write fails after the
  connector applied, the request fails (no success receipt). Unlike #347
  there is no prior-bytes rollback available for a self-persisting connector,
  so the error means "outcome unconfirmed; resolve before retrying" — a
  retry would be treated as new work. This path is store-failure-only and is
  covered by the same fatal error handling as #347's write-back commit.
- **Concurrency.** Exact-replay races inside the serving process are
  serialized by `POOL_WRITEBACK_LOCK`; cross-process concurrent writers are
  not supported by the single-writer daemon.

## 7. Follow-ups (not this change)

- #351 (digest collision in `expected_source_sha256` / the source digest of
  numeric records) is **not** addressed here. The new dedup key is
  digest-free precisely so it does not depend on that digest.
