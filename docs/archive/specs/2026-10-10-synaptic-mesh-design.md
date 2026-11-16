# SynapticMesh — Distributed Memory Fabric — Design Spec

**Date:** 2026-10-10
**Author:** BOSUN session (Bosun, Kimi K3 architect verdict, GPT-6 Astra architect verdict)
**Path:** architectural (per `superpowers:brainstorming` ratchet)
**Status:** concept + written spec approved by owner 2026-10-10; ready for development
**Decisions locked by owner:**
- Storage path: `~/.NeuroStrata/synaptic-mesh/<mesh_id>/` (daemon-owned, separate repo, parent-gitignored) + `<project>/.NeuroStrata/synaptic-mesh-ref` pointer file.
- **v1 ships the full concurrent-merge table** (not minimal). Per owner: "no rush of this; no reason to defer and make it more complicated; just implement everything we need."
- CLI subcommand name: `synaptic-mesh` (brand-explicit); `mesh` is a documented short alias.
- Imported procedure default: **visible-in-search + untrusted**; promotion is explicit (`trust: "promoted"`) before the procedure can fire locally.
- **No separate implementation plan doc.** The spec IS the plan; tasks decompose from spec sections; development proceeds slice-by-slice.
**Target release:** post-1.8.0 (no version-locked decision yet; owner's call)

> **Naming:** SynapticMesh — the *inter*-instance synaptic layer that complements the *intra*-instance SynapticGraph. Same root word, opposite scales. Approved by owner 2026-10-10.
>
> **Verdicts of record:**
> - K3 (anthropic-arch, Kimi K3) — `docs/design-mesh-verdict-kimi.md` — recommended Approach D over Approach C; supplied the merge-conflict C-supersession-race analysis.
> - openai-arch (GPT-6 Astra, OpenAI seat) — `docs/design-mesh-verdict-openai.md` — confirmed D conditionally; supplied the durable-outbox, semantic-merge, sharing-boundary, lifecycle-state-machine, and trust-model refinements.

---

## Intent

NeuroStrata is local-first and single-machine. A team of humans + their agents, each running a private NeuroStrata instance, **cannot share what they learn about the same project**. A team member's agent discovers "this codebase uses the Adapter pattern for the third-party integrations" — that knowledge stays on their machine. The team pays the discovery cost again per member.

**SynapticMesh** closes that gap without centralization, without real-time semantics, and without sacrificing NeuroStrata's local-first guarantees. Teams get **eventual consistency** on session start via git as the transport. Every node keeps its own LadybugDB; the mesh is a *projection layer*, not a shared database.

Success looks like:
- A team of N humans + N agents on N machines share memories + procedures for a single project.
- A memory added on machine A appears on machines B, C, D after their next session start (or after an explicit `mesh pull`).
- A memory archived on machine A is gone from all machines within one sync cycle.
- Duplicates from independent learning are deduped or explicitly preserved per the lifecycle policy.
- No machine is blocked on a network call during normal mutation.
- An archived (tombstoned) memory never resurrects, even from an old-epoch peer.

---

## Non-goals (v1)

- **No real-time sync.** The user explicitly does not want "everyone needs what every node remembers right away."
- **No central server.** Git (a bare repo or a fork-and-PR workflow) is the transport; nothing else.
- **No sharing of tasks, claims, sessions, embeddings, or procedure counters.** Those stay strictly local.
- **No sharing of structural/source-ingestion memories.** Project AST graphs are local.
- **No procedure execution from imported data alone.** Imported procedures are untrusted until a team member explicitly approves them.
- **No vector-clock machinery.** Per K3: "vector clocks *detect* concurrency but don't resolve it; skip them in v1." Causal context comes from git ancestry.
- **No automatic force-push, destructive repair, or unapproved historical export.**
- **No silent conflict resolution that drops a teammate's work without trace.**
- **No serving unvalidated or partially integrated shared state.**
- **No exactly-once delivery guarantee.** Stable IDs + revision numbers + replay-on-restart make at-least-once sufficient.

---

## Architecture overview

```
                Local NeuroStrata instance (per machine)
                ─────────────────────────────────────────
                       │
   LadybugDB  ─────────► Thalamic Bus ────────► durable outbox
   (source of truth)         │                       │
                             │                       ▼
                             │              serialized mesh-worker
                             │                       │
                             ▼                       ▼
                       (other in-proc         git commits → SynapticMesh
                        subscribers)           (separate repo, parent-gitignored)
                                                        │
                                                        │ git push
                                                        ▼
                                              bare repo / fork-and-PR
                                                        │
                                                        │ git pull (on session start)
                                                        ▼
                                              validated staging area
                                                     │
                                                      ▼
                                          replay into local LadybugDB
```

The **LadybugDB is the source of truth**; the **mesh is a projection**. On session start the daemon pulls the latest mesh projection, replays it through validation into a staging table, and atomically applies the staging table to LadybugDB. Mutations flow the other way through the durable outbox: every mutation writes to LadybugDB and to the outbox in one transaction; the mesh-worker drains the outbox into atomic file replacements + git commits; the user explicitly or implicitly triggers push.

---

## Storage layout (Approach D — per-record files, separate repo)

**Mesh lives in its own git repository**, parent-gitignored from the project:

```
~/.NeuroStrata/synaptic-mesh/<mesh_id>/     # daemon-owned, per-machine
                                            # this dir IS the mesh repo
   ├── manifest.json                        # mesh-level metadata (see §"Mesh manifest")
   ├── <namespace>/                         # one dir per mesh-joined namespace
   │    ├── memories/<memory_id>.json       # one file per memory record
   │    ├── procedures/<procedure_id>.json  # one file per procedure definition
   │    └── import-pending/                 # staging area (validated records only)
```

The project tree holds only a small **pointer file** so other tooling can find the mesh:

```
<project>/.NeuroStrata/synaptic-mesh-ref    # text file: mesh UUID + remote URL
```

The mesh repo is **gitignored** from the project repo (orthogonal history; no branch entanglement).

### Why per-record files

- The directory tree **is** the snapshot — git's content-addressable tree gives us snapshots for free.
- `git log` **is** the event log — free.
- Different-record writes touch different files → **zero conflicts** between writers, ever.
- Same-record concurrent writes → genuine semantic conflicts, surfaced explicitly (see §"Lifecycle state machine").
- File-scoped purge (`rm <id>.json`) is a first-class git operation.
- Diff-based idempotent pull replay: `git diff --name-only <old_head>..HEAD` → upsert changed records.

### Why a separate repo

- Mesh must not follow project feature branches. Memory is branch-orthogonal.
- The mesh can be force-pushed or rewritten for epoch migrations without disturbing the project's git history.
- Each machine holds a daemon-owned clone keyed by mesh UUID. No nested-git accidents.

---

## Mesh manifest

`manifest.json` at the mesh root carries mesh-level metadata:

```json
{
  "schema_version": "synaptic-mesh/v1",
  "mesh_id": "uuid-v4",
  "mesh_name": "stripe-api-team",
  "created_at": "2026-10-10T12:00:00Z",
  "created_by": "node_id of originator",
  "epoch": 1,
  "namespaces": ["stripe-api"],
  "policy": {
    "require_record_eligibility": true,
    "imported_procedures_trust": "untrusted",
    "allow_history_purge": false,
    "auto_compact": false
  },
  "remote": "https://github.com/team-cognilogical/mesh-stripe-api.git"
}
```

**Epoch** increments on any coordinated history rewrite (filter-repo purge, schema migration). Old-epoch peers cannot publish; their outboxes are quarantined, not replayed.

---

## Record format (per memory / per procedure)

UTF-8 JSON. Stable key order. One field per line. Normalized representation. No volatile counters in shared records.

```json
{
  "schema_version": "synaptic-mesh-record/v1",
  "id": "uuid-v4",                           // stable, server-side assigned
  "namespace": "stripe-api",
  "type": "rule" | "fact" | "procedure" | "structural",
  "lifecycle": {
    "state": "active" | "archived" | "superseded" | "tombstoned",
    "created_at": "2026-10-10T12:00:00Z",
    "created_by": "node_id of origin",
    "superseded_by": "uuid-v4 or null",
    "archived_at": "iso-8601 or null",
    "tombstoned_at": "iso-8601 or null"
  },
  "content": "...",                          // type-specific
  "metadata": {
    "tags": [...],
    "source": "...",
    "eligibility": "shareable" | "private"   // per-record gate (see §"Sharing boundary")
  },
  "relationships": [
    { "kind": "supersedes", "target": "uuid-v4" },
    { "kind": "references", "target": "uuid-v4" }
  ],
  "update_id": "uuid-v4",                    // monotonic per record, bumps on every change
  "origin_mesh_epoch": 1                     // epoch when record was first published
}
```

### Procedure-specific fields

A procedure record carries the **definition + lifecycle only**, never counters:

```json
{
  "...": "...",
  "type": "procedure",
  "definition": {
    "trigger": "session-start" | "before-edit" | "...",
    "remaining_fires_initial": 10,           // INITIAL budget only
    "valid_to": "2027-01-01T00:00:00Z",
    "rehearsal_script": "..."
  }
}
```

Runtime counters (`remaining_fires`, `performance_count`, `last_performed_at`) are **strictly local**; they never leave the node. Import cannot reset local counters. A semantic definition revision may request a locally approved reset; routine sync cannot.

### Content-hash dedup caveat (openai-arch correction)

Git blob dedup saves storage; it does **not** merge identities. Identical text under different UUIDs has different provenance or relationships. Present duplicate candidates to the user; retain IDs or explicitly alias them and update references.

---

## Lifecycle state machine

The **shared** lifecycle has four states. State transitions are explicit; transitions are recorded in `lifecycle` metadata.

```
                 ┌────────────┐
                 │   active   │ ◄────────────────┐
                 └─────┬──────┘                  │ restore
                       │                         │ (observing archived revision)
                archive│                         │
                       ▼                         │
                 ┌────────────┐                  │
                 │  archived  │ ─────────────────┘
                 └─────┬──────┘
                       │
       supersede(replacement_id)
                       │
                       ▼
                 ┌────────────┐
                 │superseded  │  (terminal; new linked ID if needed)
                 └────────────┘

   any live state ──► tombstoned  (terminal; deletion wins over stale updates)
```

**Rules:**

| Transition | Allowed? | Notes |
|---|---|---|
| `active → archived` | yes | reversible |
| `archived → active` | yes | observing the archived revision |
| `active/archived → superseded(replacement_id)` | yes | terminal retirement; replacement_id is a new linked record |
| `archived → superseded(replacement_id)` | yes | allowed; preserves tombstone-free intent |
| `any → tombstoned` | yes | terminal; **deletion wins over stale updates** |
| `retired ID reactivates` | **never** | create a new linked ID instead |

Retired IDs (superseded or tombstoned) **never reactivate**. New work always gets a new linked ID.

---

## Concurrent merge policy

When two nodes independently mutate the same record, the policy depends on the operation pair. **Never pick by timestamp.**

| Concurrent ops | Result |
|---|---|
| archive + content edit | Edited content stays archived; report combination. |
| restore + archive | **Unresolved**; keep inactive pending explicit resolution. |
| supersede + stale restore | Superseded wins; report rejected restore. |
| supersede + content edit | Replacement authoritative; preserve edit candidate as a new record with `relationships: [{kind: "references", target: original_id}]`. |
| **two different replacements** (X→Y vs X→Z) | **Unresolved; never pick by timestamp; preserve both candidates.** |
| incompatible content edits | Unresolved; preserve both versions as new records; each carries `relationships: [{kind: "references", target: original_id}]`. |
| tombstone + edit/restore | **Tombstone wins**; never serve deleted content. |

**Unresolved conflicts pause namespace integration.** The mesh-worker fetches both candidates, validates them, and surfaces them in `mesh status`. The namespace integration is paused at the affected IDs. A local **conflict overlay** suppresses those IDs from search/snapshot/export. Unrelated local work continues unaffected. Disclose paused integration in `mesh status`.

A v2 candidate is a fully automatic CRDT-style merge. v1 stays on the explicit-conflict policy.

---

## Sharing boundary (record + field eligibility)

Namespace opt-in is necessary but **not sufficient**. Every record carries an `eligibility` field. The mesh refuses to publish a record with `eligibility: private`. Local memories default to `private`; the user must explicitly mark shareable records (CLI flag, MCP argument, or batch operation).

### What is not shareable (v1 — hardcoded)

- **Tasks** (`task_*` records, claims, heartbeats, session ids)
- **Procedure runtime counters** (`remaining_fires`, `performance_count`, `last_performed_at`)
- **Embeddings** (vectors are non-portable across embedders/dims)
- **Structural / source-ingestion memories** (project AST graphs are local)
- **Lock-2 extracted memories** are *not* auto-published; the completing task must explicitly mark them shareable (or the namespace policy auto-publishes)

### What is shareable (v1)

- `rule` memories explicitly tagged shareable
- `fact` memories explicitly tagged shareable
- `procedure` definitions (no counters) — **untrusted** until explicitly approved locally

### Trust levels (per-record metadata)

```json
"metadata": {
  "trust": "trusted" | "untrusted" | "promoted"
}
```

- **trusted** — created locally or promoted by local approval
- **untrusted** — imported from a peer; not yet approved
- **promoted** — was untrusted, locally approved for execution

Imported procedures start as **untrusted**. They are visible in `search_memory` (so the team member knows they exist), but **cannot be fired** by `procedure_perform`. The team member must explicitly promote them to `trusted` (CLI or MCP) before they can fire locally.

---

## Durable mutation → mesh handoff (the outbox)

The Thalamic Bus tees events into a **durable outbox**, not directly into git. The outbox is a local table that is **committed atomically with the LadybugDB mutation**. The mesh-worker drains the outbox.

### Why an outbox (not just bus-tee)

A bus subscriber is not durable. Crash between DB commit and bus dispatch loses the update. Crash between bus dispatch and git commit leaves an in-progress commit. The outbox pattern guarantees **no lost update** and **no half-applied commit**.

### Outbox semantics

```
LadybugDB transaction:
  write memory
  write outbox entry: {stable_op_id, payload, status: "pending"}

mesh-worker (serialized per mesh, single thread):
  read pending outbox entries (FOR UPDATE)
  replace mesh file atomically (write tmp → rename)
  git add the file
  git commit --author=<node_id> (single batched commit per drain cycle)
  mark outbox entries as "committed"
```

### Crash recovery

- Crash before outbox write: nothing to recover; the memory never landed in LadybugDB.
- Crash after outbox write but before git commit: outbox entry stays `pending`; mesh-worker retries on restart.
- Crash after git commit but before outbox update: outbox entry stays `pending`; mesh-worker detects the file already exists (idempotent) and marks `committed`.

### Batching

The mesh-worker **batches commits** over a short bounded window (default: 5s, configurable). One commit per drain cycle, not one commit per mutation. This keeps git history readable and avoids I/O storms at the cluster.

### Imported records do not re-export

A record imported from a peer must not be re-exported as a fresh local mutation. The outbox writer checks the record's `origin_mesh_epoch` and `origin_node_id`; records not originating locally are skipped.

---

## Import validation (fetch into isolated staging)

Pull fetches into an **isolated staging directory**, never directly into the mesh working tree. The daemon validates every candidate before applying it.

### What gets validated

- Remote identity (mesh_id matches manifest.json)
- Epoch compatibility (peer's `epoch` ≥ our `min_supported_epoch`)
- Schema version (manifest and record schema versions are supported)
- Path safety (rejects symlinks, executable payloads, arbitrary hooks/config, path traversal)
- Per-record integrity (UUID format, lifecycle state valid, no supersession of unknown target)
- Cross-record invariants (no cycles in the supersession graph, no dangling references)

A failed validation refuses the pull. The mesh-worker logs the failure with detail. Local state is unaffected.

### What gets applied

Only the validated candidate tree gets applied. Application is **idempotent**: re-applying the same record is a no-op. Stable IDs + `update_id` together make at-least-once delivery sufficient.

### Cursor and full-tree reconciliation

The mesh tracks a **last accepted head** cursor. On session start, fetch + diff from that cursor. Missing cursor, divergent ancestry, or changed epoch triggers a **full-tree reconciliation** (validated, not blindly applied).

Fresh enrollment imports the **current tree**, not years of events. Git log is revision history, not a complete domain event ledger after batching or rewriting.

---

## Trust model

### Push permission = transport trust, not authentication

Anyone who can push to the mesh's bare repo can write memories. The mesh records the **author** (node_id ↔ git user.name/email), but git authors are forgeable by anyone with commit access. v1 accepts this; signed commits are a v2 hardening.

### Imported data is untrusted

Imported records carry `trust: "untrusted"` until locally promoted. They are **visible** but not **executable** for procedures. A team member who wants to fire an imported procedure must explicitly promote it.

### Mandatory author field

Every event must carry `created_by: node_id`. Nodes are identified by a UUID assigned at first run; the mapping `node_id ↔ git user.name/email` is in the mesh-worker config.

### Capability matrix (per-node)

A node's push/pull capability is governed by the bare repo's access controls. The mesh itself enforces **no** additional authentication. v1 trust = "anyone the team has granted push access."

---

## Triggers

Per openai-arch's "D. Onboarding, triggers, recovery" — no silent timed push, no real-time sync.

| Trigger | Behavior |
|---|---|
| `neurostrata-mcp synaptic-mesh push` | Explicit push. Pull-before-push mandatory. Fail-soft. |
| User's `git push` of the project | Piggybacked via project-side hook: enqueue mesh push, return promptly, do not block. |
| Session end | Optional best-effort flush; never blocks session exit. |
| Session start | Best-effort pull + validation; never blocks session start beyond a timeout (default 5s, fail-soft). |
| Long session (no recent pull) | Manual refresh or consented periodic fetch at turn boundary. |

**No silent timed push.** No background timer pushing without user action. No blocking network ops in the mutation path.

### Non-interactive

All git operations set `GIT_TERMINAL_PROMPT=0`. A credential prompt wedges the session. Pending pushes queue locally; the user resolves auth at their leisure.

### Pull-before-push

Every push does `git fetch && rebase/merge && retry push`. A rejected push triggers bounded fetch/merge retries. Never force-overwrite.

---

## Hooks (project-side)

A project-side pre-push hook can enqueue a mesh push, but **hooks are not reliable** (not inherited by clones, conflict with hook managers). Offer installation explicitly; manual sync remains first-class. Client-specific hooks belong in client shims, not in core mesh logic.

The mesh command itself never depends on a project-side hook.

---

## Recovery

The mesh-worker must finish/recover import before claiming "synchronized" state or publishing. The mesh tracks `local_durable`, `local_committed`, `imported`, and `published` states distinctly.

| Failure | Behavior |
|---|---|
| Dirty working tree | Pause sync, do not touch local store. Provide recovery command. |
| Detached HEAD | Pause sync. Recovery: reattach or reclone. |
| Interrupted merge | Pause sync. Recovery: `git merge --abort` or commit. |
| Auth failure | Queue pending pushes; do not block local operation. |
| Full disk | Fail the mutation honestly. Do not falsely acknowledge a saved memory. |
| Corrupt objects | Pause sync; preserve diagnostics, outbox, and unpublished commits. Recovery: reclone after backing up unpublished work. **Never** automatic `reset --hard`. |
| Disk exhaustion during LadybugDB commit | Fail the mutation. Do not write to outbox if LadybugDB commit failed. |

`mesh status` reports:
- Local store: `{count}` records
- Outbox: `{n} pending`, oldest `{age}`
- Last accepted head: `{cursor}`
- Last published head: `{cursor}` (or `none`)
- Conflicts: `{n} unresolved`
- Health: `healthy` | `paused: <reason>`

---

## Deletion & erasure

### Tombstone semantics in live state

`archive_memory` (or a future `tombstone_memory`) writes a tombstone record. The tombstone is published like any other lifecycle change.

### Git never forgets

A tombstone removes the live record but the **content lives in ledger history** and every teammate's clone. A documented purge procedure (`filter-repo --path` per file) is required for actual erasure. The purge is an **epoch migration**: all peers bump epoch, old-epoch outboxes are quarantined on receipt.

### Disclosure

`mesh join` and `mesh status` disclose plainly: *"The mesh is a permanent record shared with the team. Tombstones suppress content from search; full erasure requires coordinated history rewrite."*

### Decentralized sharing cannot guarantee erasure

If a peer is uncooperative, their clone retains the content. Document this in the join flow. v1 cannot enforce erasure against an uncooperative peer.

---

## Status surfaces

`mesh status` reports:

- Mesh ID, name, epoch, schema versions
- Joined namespaces and their sync state
- Outbox: pending count, oldest age
- Last accepted head: cursor + age
- Last published head: cursor + age
- Unresolved conflicts (per-namespace count)
- Health: `healthy` | `paused: <reason>`
- Local store: shared vs private record counts

`search_memory`, `get_snapshot`, and `export_graph` all use the **effective local view**: private records + accepted shared records, with lifecycle and conflict suppression. Network absence never fails a query. Returned records carry provenance and freshness/conflict status.

---

## Acceptance evidence (the worker must demonstrate)

Per openai-arch §5 — the implementer runs:

1. **Two offline nodes, disjoint adds** — converge cleanly; no conflicts.
2. **Two offline nodes, lifecycle cases per §"Concurrent merge policy"** — preserve intent, signal conflicts, never resurrect retired records.
3. **Crash at every DB/outbox/file/commit/import/cursor boundary** — no lost accepted update, no duplicate side effect, no partial snapshot, no re-export feedback loop.
4. **Duplicate IDs, identical text/different IDs, malformed or oversized JSON, invalid graphs, unknown schemas, forged authors** — all fail safely.
5. **Old-epoch offline peers** — cannot resurrect purged identities.
6. **Missing hooks, credential failures, dirty checkouts, rejected pushes, daemon restart, multiple source worktrees** — local operation preserved.
7. **Full-tree recovery** — preserves private rows and procedure consumption state.
8. **Year-scale data** — representative size; meets explicit latency/storage thresholds before any auto-compaction.

---

## Open questions deferred to v2

- **Vector clocks / Dotted Version Vectors** for richer causal ordering (per K3, not needed in v1).
- **CRDT-style automatic merge** for unresolved concurrent cases (v1 pauses for explicit resolution).
- **Signed commits / cryptographic author attribution** (v1 accepts "push = trust").
- **Auto-compaction** of the mesh repo (v1 is append-only; year-scale maintenance is explicit operation, not unattended compaction).
- **Shared task list projection** (tasks stay local in v1; a future shared-flag projection can be added without changing the record format).
- **Federation** of meshes (mesh-of-meshes).

---

## References

- `docs/design-mesh-verdict-kimi.md` — K3 (anthropic-arch, Kimi K3) verdict; identified the breaking C-flush merge-conflict; recommended Approach D.
- `docs/design-mesh-verdict-openai.md` — openai-arch (GPT-6 Astra) verdict; supplied durable-outbox, semantic-merge, lifecycle-state-machine, sharing-boundary, and trust-model refinements.
- `docs/superpowers/specs/2026-10-09-memory-event-bus-design.md` — Thalamic Bus (1.8.0); the durable-outbox pattern extends this.
- `docs/superpowers/specs/2026-10-09-procedure-memory.md` — `procedure` memory type (1.8.0); the mesh's procedure record reuses this definition.

---

## Open issues to confirm with owner before spec approval

1. **Storage path**: `~/.NeuroStrata/synaptic-mesh/<mesh_id>/` vs project tree under `<project>/.NeuroStrata/synaptic-mesh/`. Recommendation: **daemon-owned at `~/.NeuroStrata/synaptic-mesh/<mesh_id>/`** with a project-side pointer file (`<project>/.NeuroStrata/synaptic-mesh-ref`). Keeps the mesh independent of the project tree; daemon can manage it without touching project files.
2. **v1 scope**: full concurrent-merge table vs minimal state machine + pause-on-conflict. Spec above is **full** (the merge table is in §"Concurrent merge policy"). Owner choice: full in v1, or strip to minimal and defer the table.
3. **Mesh CLI subcommand name**:
   - `neurostrata-mcp synaptic-mesh {init,join,push,pull,status,leave}` (brand-explicit)
   - `neurostrata-mcp mesh {init,join,push,pull,status,leave}` (terser)
   - `neurostrata-mcp fabric {init,join,push,pull,status,leave}` (single-word alternate)
   - Recommendation: **`synaptic-mesh`** for the brand consistency, with `mesh` as the documented short alias.
4. **Imported procedure trust**: trust=untrusted default + explicit local promotion to fire. Owner confirmation: is "see it in search, must promote to fire" the right model, or should untrusted procedures be **hidden** from search until promoted?