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
- **Bitemporal revert** — restores complete before-images in reverse order (resolving multi-step `SetTwinState` chains against staged revert state via `resolve_namespace_target_row_with_staged` and verifying full `UpsertMemory` post-images via `fingerprint_live_row` and `ExpectedHead::RowFingerprint`, while normalizing legacy post-images with `canonicalize_topology` and `materialize_facets_bitemporal` using the newly appended/retracted commit timestamp), supersedes the active apply-receipt version with `reverted_at` + `reverted_by_receipt_id`, and writes a separate revert receipt in the same transaction. Double-revert and stale active heads (`ConflictWithNewerVersion`) are refused unless `force: true` is invoked by an authenticated `admin` or `owner` on all affected targets.

`neurostrata_action` (MCP), `POST /action` (Daemon), and `neurostrata-mcp action <define|validate|apply|revert|log>` (CLI) share a single deterministic application service.

## Connector Target Binding

`ActionReceiptPayload` stamps `target_pointer_id` / `target_pool_id` inside the nested `metadata.action_receipt` when the action declares both the reserved `pointer_id` and `pool_id` parameters. Connector write-back binds only through that immutable payload (receipt-id equal, both identifiers equal); receipts written before the stamp remain bound through the same nested parameter pair. Editable top-level row metadata and unbound active receipts never authorize a write-back.

## Capability E Production Learning & Reflection Hooks

v1.19.0 wires deterministic trajectory mining and outcome-verified reflection into committed lifecycle points (`src/learning/hooks.rs`). Hooks never complete a task and never apply or revert an action, so they cannot recurse into the lifecycle that invoked them.

- **Post-action bounded trajectory evidence** — after a successful, un-reverted `apply` linked to a `done` task (and on task completion), the hook collects bounded `(trigger_context, tool_or_action_sequence, verification_gate)` evidence for the miner. Rejected/dry runs, reverted receipts, and tasks without a clean receipt chain plus an extracted memory contribute nothing.
- **Explicit exhaustive fallback** — trigger normalization accepts only governed values (`session-start`, `before-edit`, `after-mutation`, bounded `every-n-*`). Unknown or malformed contexts are rejected, never silently rewritten to `session-start`; an unavailable judge or a non-finite confidence fails closed (no candidate). Mined candidates are never auto-applied.
- **Failure reflection & queue semantics** — a successful revert or a real failed-attempt exemplar decrements reliability on governing/motivating targets. A contradiction is enqueued only when the store CAS succeeds (`Ok(true)`); the exemplar row id is evidence, never a reflection target. Revert hooks fire only after a successful commit, never on rejected or dry-validation paths.
- **ReBAC / privacy / SHACL** — every write goes through `LadybugStore::commit_action_batch` (single-writer turn, expected-head CAS, privacy-aware staged writes). Inert action-type candidates pass `ActionTypeDefinition::validate_definition` and `OntologyIndex::validate_strict_post_image` before any write; `learner_writable` adds a foreign-owner ACL check because ReBAC admin bypass cannot express it. Failures are collected in a report, never reported as reflection success.

No online training, no autonomous self-modification, and no unimplemented triggers: mined procedure and action-type candidates stay inert until defined and applied through the existing gated mechanisms.

Source of truth: `src/rdf/action.rs`.
