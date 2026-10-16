# Keywest guinea-pig report: skipped `GOVERNS` edge materialization

**Observed:** 2026-10-16
**Reporter task:** `neurostrata-mjcg`
**Scope:** one observed `neurostrata_add_memory` interaction; no database or source changes.

## Expected / question for triage

When a memory write is accepted but one or more declared relationship targets cannot be resolved, preserve the memory (as the current behavior does) and make the missing relationship visible to the caller in a structured result, or clearly document that graph-edge materialization is best-effort and may only be visible in daemon logs. A deferred edge may be valid until a later directory ingest; the request is to expose which edge was deferred, not to reject the memory write.

This is a triage request, not a claim that the resolver is wrong: the source comment says a missing target intentionally produces no edge while allowing a rule to name a file the ingester has not reached.

## Actual

The daemon emitted repeated warnings for two accepted memory writes:

- `fec59209-4cf1-4a35-a2cb-53b76c726a86`
- `70701845-7d0b-4386-977f-70b0e43cb398`

Each warning reported `field: metadata.governs`, `why: unqualified target is missing or ambiguous`, and `skipped: this edge materialization`; it omitted `edge.target_id` and the array index. The `neurostrata_add_memory` call returned only success text, without the new memory ID or skipped-edge details. Subsequent `neurostrata_get_memory` calls confirmed both memory records, content, and `Governs` path strings were persisted; this was not a failed memory write or data loss.

## Reproduction context

A successful `neurostrata_add_memory` call declared file paths through `locations`, which produced `metadata.governs`. For the Gemma fact, `project_root` was the main Key West checkout while several referenced files existed only in a linked worktree. Source inspection shows `handle_add_memory` (`src/server.rs:1101-1113`) uses `project_root` only to check for `.NeuroStrata`; it is not passed to target resolution. `resolve_target_on_write` searches existing NeuroStrata `Memory` graph nodes, not the filesystem. The corresponding graph nodes for all referenced paths were not inspected, so the precise missing/ambiguous targets remain unknown.

Source inspection found `LadybugStore::upsert` inserts the memory row before edge creation (`src/store/ladybug.rs:798-901`); unqualified targets resolve against existing `Memory` nodes via `resolve_target_on_write`. `pick_declared_target` returns no target both for zero matches and ambiguous suffix matches (`src/store/ladybug.rs:1886-1927`), so the warning conflates those cases and omits which target failed (`:869-873`). The source comment explicitly treats a target not yet ingested as a valid reason to omit an edge (`:840-842`). A later directory ingest invokes `relink_edges` (`src/parser/ingest.rs:539`; implementation at `src/store/ladybug.rs:1342`), which replays stored declarations once target nodes exist.

## Why triage may help

The persisted `Governs` strings are visible when fetching the memory, but a consumer relying on materialized graph edges may not traverse those relationships until a subsequent ingest/relink. The MCP success response did not identify a deferred edge, its target, or the new memory ID; the generic diagnostic appeared only in daemon output. Please assess whether this is the intended contract or whether callers should receive structured deferred-edge information (relation, target, reason) and the created memory ID. Preserve the designed fail-soft write behavior for targets that will be added by later ingestion.

## Related product-level finding: external precondition evidence

A separate Key West dogfooding case exposed a second, related limitation: a
memory can record a safety concern, but that record does not enforce a
precondition on an external action. Before destructive Tebra test-record
cleanup, the operator wanted evidence of a fresh backup that includes the
schedule and a credible recovery path. Metadata-only inspection found a
successful latest Cloud Run execution (2026-10-16 06:31:18 UTC) and all six
expected encrypted collection objects: patients, appointments, transactions,
appointment reasons, providers, and service locations. It did not establish
that the latest snapshot can be decrypted or restored. The Key West backup
writer logs individual collection/upload failures and continues
(`keywest.health/tebra-backup/main.go:195-223`), so execution success alone is
not a reliable completeness signal.

NeuroStrata's task `blocked_by` field can represent declared dependencies, but
it does not verify that an external backup exists or that its evidence is
fresh. `neurostrata_task_validate` is advisory; the push gate checks task
lifecycle conditions, not whether an external client is safe to execute. A
memory or task note is useful provenance, but cannot by itself stop a separate
CLI from mutating a system.

Potential product direction for triage: consider a generic, vendor-neutral
precondition/evidence contract for tasks, with a machine-verifiable result,
observation time, expiry/freshness, and a reference to safe aggregate evidence.
An action-capable client would have to enforce that contract and fail closed
when evidence is missing, stale, incomplete, or unknown. NeuroStrata should
store and surface the evidence, not fetch Tebra data or treat an unchecked
task status as enforcement. Keep the evidence PHI-free; do not store backup
payloads, patient identifiers, credentials, or raw vendor responses.

NeuroStrata source anchors: `docs/design-task-subsystem.md` §5.3;
`src/task/gate.rs:54-87`; `src/task/mod.rs:1197-1211`. The backup behavior is
source-reviewed in `keywest.health/tebra-backup/main.go`; only GCP execution
and object metadata were inspected for the live observation.

This is a product-design lead, not a claim that the task subsystem currently
promises to gate arbitrary external actions. The initial implementation
question is whether a generic evidence/precondition primitive belongs in
NeuroStrata or whether clients should own both verification and enforcement
while linking an evidence reference to a task.

## Scope

No live Tebra endpoints, patient records, credentials, or backup payloads
were accessed. The backup observation used only Cloud Run/GCS metadata and
source review. This addendum changes only this report; no NeuroStrata source
or database contents were modified.
