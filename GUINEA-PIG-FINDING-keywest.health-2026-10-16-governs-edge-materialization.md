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

No Tebra endpoints, patient records, credentials, or Tebra implementation were involved. No NeuroStrata source or database contents were modified by this report.
