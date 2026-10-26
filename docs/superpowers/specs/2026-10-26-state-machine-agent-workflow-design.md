# Spec: State-Machine Enforcement of Agent Workflow
(Auto-use of NeuroStrata graph + memory systems)

> Captures the panel review from K3, Argon (gemini-arch), and Astra (openai-arch), incorporating Astra's revisions and Argon's rereview.

## Status
Approved for spec (post-rereview REVISE→approved-pending-AC). Next: user review → writing-plans.

## Purpose
The agent currently only reads/consults graph+memory when explicitly directed (doctor, normalize-metadata, manual search). We want auto-use when advantageous: file-scoped memory injected on `read`/`edit`/`write`, stale edges auto-reconciled, task-completion gate enforced without prompt-only mandates.

## Evidence
- NeuroStrata v1.14.3 daemon healthy on 127.0.0.1:34343, build in sync.
- `neurostrata-mcp doctor` → NeuroStrata namespace: 6 resolved / 5 unmatched targets.
- `neurostrata-mcp normalize-metadata NeuroStrata` → fixed 7 stale targets ("Scanned 775 memories, updated 4").
- `ContextService::plan` is currently a stub (`src/context/service.rs`).
- `src/task/machine.rs` already has a 4-state data-level task FSM (write-time, not session-wide).

## Architecture (Two-Layer, host-enforced)

**Layer 1 — Per-Task Workflow FSM.** Scoped to a single task claim, not a global session.

States:
- `Unpreflighted` — no snapshot, no active task claim. Blocks mutating tools (`write`, `edit`, mutating `bash`).
- `ContextLoaded` — snapshot claimed. Reads permitted freely.
- `TaskClaimed` — `task_claim` returned for `session_id`. Mutating tools allowed.
- `Mutating` — at least one write/edit since last `task_update(note)`. Step-counted.
- `BreathRequired` — >=5 mutations since last `task_update` note (Lock 3). Mutations blocked until note recorded.
- `Verifying` — post-mutation; read-only tools bypass.
- `Completing` — `task_complete` requested; pre-flight check.

Transitions:
- `Unpreflighted` → `ContextLoaded` via `neurostrata_get_snapshot` (MCP).
- `ContextLoaded` → `TaskClaimed` via `task_claim` (DB row, `(session_id, active_task_id)`).
- `TaskClaimed`/`Mutating` → `Verifying` via any read-only tool; → `Mutating` via write/edit.
- `Mutating` → `BreathRequired` at 5 writes since last note.
- `Mutating` → `Completing` via `task_complete` (Lock 2: requires extracted memory first).
- `Verifying` → `Mutating` loop allowed (re-mutate, re-verify). NOT strict linear.
- Any → `Completing` blocked if `task gate --strict` fails.

**Layer 2 — Graph Target Lifecycle FSM.** Event-driven.

Edge target states:
- `Declared (Deferred)` — unqualified/legacy path in metadata; edge not yet materialized.
- `Resolvable (Unqualified)` — suffix matches an ingested node; `KnownIds::resolve` can qualify.
- `Materialized (Edge Active)` — edge written to LadybugDB.
- `Orphaned (File Deleted/Moved)` — target matches no ingested node, file deleted/moved.

Transitions (host/ThalamicBus):
- file edit → mark `GraphDirty`.
- post-ingest (`ThalamicPulse::IngestDone`) → auto-run `KnownIds::resolve` + relink (eliminates manual `normalize-metadata` on existing nodes).
- `task_complete` → pre-check: normalize dirty targets; classify remaining unmatched.

## Changes to Original ACs (from Argon rereview)

- **AC1 (unchanged):** `ContextService::plan` wired to `graph_read::memories_for_file` + `rank_dedup_budget` (512 tok/call, 1500/turn).
- **AC2 (revised):** `guard_validate` must check `(session_id, active_task_id, worktree)` state and return the legal next transition on rejection — not just a session-level check.
- **AC3 (unchanged):** `LadybugStore::upsert` canonicalizes resolvable targets at write time via `KnownIds::resolve`.
- **AC4 (revised):** Before task completion, `doctor --namespace <ns>` must report **0 resolvable** targets. The 5 unmatched targets must be classified into `Materialized`, `Declared`, or `Orphaned` — not forced to zero total. Unrelated orphans warn, not block.
- **AC5 (new):** Benchmark `ContextService::plan` + `upsert` canonicalization p95 latency on NeuroStrata repo before merge; assert < 100 ms.

## Receipt Binding (Astra)
Receipts bind to `(task_id, worktree, HEAD_sha, dirty_tree_hash)` computed via git + working-tree hash delta — NOT by parsing `bash` strings. Edits to the worktree invalidate the revision mark → stale receipts surfaced.

## Trade-offs
| Concern | Mitigation |
|---|---|
| Over-blocking reads | Reads free at all states; only mutate + complete gated |
| Loopback deadlock | `Verifying → Mutating` explicit; breath note clears BreathRequired |
| Latency | 5–15 ms assumed → AC5 p95 benchmark required |
| False blocks | `guard_validate` returns exact legal transition so agent can comply |

## Implementation Order (proposed)
0. AC5: baseline p95 measurement on current code.
1. Layer 1 in MCP server: session+task+worktree state table + `guard_validate` check.
2. Layer 1 in OpenCode plugin shim: block mutating tools in `Unpreflighted`/`BreathRequired`; allow reads.
3. AC1: wire `ContextService::plan` (de-stub).
4. Layer 2: `ThalamicBus` subscriber for post-ingest normalize; `task_complete` pre-check.
5. AC3: upsert canonicalization.
6. AC4: classification of unmatched targets; warnings not blocks.
7. Receipt binding to HEAD_sha + dirty_tree_hash.

## Files of Interest
- `src/task/machine.rs` — existing task FSM (write-time).
- `src/context/service.rs` — `ContextService::plan` stub.
- `src/graph_read/memories_for_file.rs` — file-scoped read.
- `src/store/ladybug.rs` — `LadybugStore::upsert`, `KnownIds::resolve`.
- `src/guard/validator.rs` — `guard_validate` (to be extended).
- `src/server.rs` — MCP handlers.
- `plugins/opencode/index.ts` — host plugin hook points.
- `src/main.rs` — `normalize-metadata` subcommand.
