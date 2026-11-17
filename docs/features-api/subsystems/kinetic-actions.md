# Kinetic Action Types & Bitemporal Revert Log

## 5-Stage Fail-Closed Gate

Every `neurostrata_action` mutation passes through a deterministic 5-stage fail-closed gate before any state change is committed:

1. **Trusted principal ReBAC** — resolve the caller principal; unauthenticated principals are rejected outright.
2. **Typed Parameters** — validate parameters against the `ActionTypeDefinition` with finite bounds; unbounded parameters fail.
3. **Offline SPARQL ASK** — evaluate a `SparqlAsk` bound via `spareval::PreparedQuery::substitute_variable` (read-only; `SERVICE` forbidden).
4. **Digital Twin Preconditions** — enforce `TwinVarPrecondition` bounds over current twin state.
5. **Post-Image SHACL** — validate the staged post-image across all rows plus the `action_receipt` row, in a single atomic LadybugDB write turn with active-head CAS revalidation.

Only `validate` never writes; `apply` commits all effects (`SupersedeMemory`, `UpsertMemory`, `SetTwinState`, `LinkEdge`, `TransitionTask`) plus a bitemporal `action_receipt` in one transaction. `TransitionTask` only targets `open`, `in_progress`, or `blocked` and never bypasses Lock 2 (`neurostrata_task_complete`).

## Atomic Apply & Revert Receipts

- **Atomic apply** — every effect plus the `action_receipt` commits in one database turn. `action_receipt` rows are structural and unconditionally private (`eligibility: "private"`), excluded from SynapticMesh import/export.
- **Bitemporal revert** — restores complete before-images in reverse order (resolving multi-step `SetTwinState` chains against staged revert state via `resolve_namespace_target_row_with_staged` and verifying full `UpsertMemory` post-images via `fingerprint_live_row` and `ExpectedHead::RowFingerprint`, while normalizing legacy pre-facet post-images with `materialize_facets_bitemporal` using the newly appended/retracted commit timestamp), supersedes the active apply-receipt version with `reverted_at` + `reverted_by_receipt_id`, and writes a separate revert receipt in the same transaction. Double-revert and stale active heads (`ConflictWithNewerVersion`) are refused unless `force: true` is invoked by an authenticated `admin` or `owner` on all affected targets.

`neurostrata_action` (MCP), `POST /action` (Daemon), and `neurostrata-mcp action <define|validate|apply|revert|log>` (CLI) share a single deterministic application service.
