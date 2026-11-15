# Independent mesh verdict — OpenAI seat

Scope: supplied architecture brief and C/D designs only. No source audit,
prototype, other architect documents, or live-data inspection. This is an
independent recommendation, not a reconciled panel decision.

## 1. Position: choose refined D, not D as specified

D is materially better: record-sized files isolate unrelated writes, and Git
already stores versioned trees. Confidence: high on layout, medium on operation
until failure-injection trials. C creates a shared merge hotspot, but conflicts
are NOT guaranteed: disjoint textual changes can merge. Its deeper faults are
concurrent compaction, tail truncation, and two representations needing coherent
reconciliation. Those are fixable with a protocol, not worth fixing here. D
removes that machinery but does not supply semantic merging, durable export,
trustworthy attribution, or database reconciliation. Approve D conditionally on
those contracts. Do not ship whole-record timestamp LWW.

## 2. Operational concerns, ranked

### Critical: durable mutation-to-mesh handoff

An in-process bus subscriber is not a durable journal. A crash after DB commit
but before export loses a shared update; exporting before DB commit can publish
a rolled-back update. Every exportable mutation needs an outbox entry committed
atomically with local state, with stable operation ID and immutable payload.
If LadybugDB cannot support that transaction, design an equivalent recoverable
write-ahead protocol first. The bus wakes a worker; it does not guarantee delivery.

A serialized per-mesh worker drains the outbox into atomic file replacements
and Git commits. Crash after commit but before acknowledgement must safely retry.
Batch local commits over a short bounded window; do not spawn Git per mutation
request. Preserve logical operation provenance when batching. Imported records
must not be re-exported as fresh local mutations. Keep pending local work
separate from the last accepted shared projection.

### Critical: textual merge success is not semantic validity

Git can merge different JSON lines into an invalid lifecycle combination.
Filename diffs optimize import; they are not an import protocol. Validate
complete candidate records and cross-record invariants even after a clean merge.
Changes to different records can create a supersession cycle or dangling target.

Timestamp LWW converges but destroys intent. Clock skew or a future timestamp
can dominate legitimate changes. Entity creation time cannot order updates at
all; mutation timestamps still establish no causal ordering.

### Critical: sharing boundary and remote trust

Namespace opt-in is insufficient: private notes, paths, source snippets, task
extractions, and procedural instructions can leak. Require a shareable-field
schema plus record-level eligibility. Preview/backfill old records only with
approval. Keep tasks, claims, sessions, access statistics, embeddings, counters,
and procedure execution state local. Structural/source ingestion is not shared
by default. Lock-2 extraction does not grant permission to publish a memory.

Imported memories and procedures are untrusted data. Push permission is not
permission to execute commands, weaken local policy, or fire a procedure. Git
authors and mandatory author fields are forgeable attribution, not authentication.
Trust rests on authenticated transport, approved remotes, access controls, and
branch governance. Standard commit signing may support stronger attribution;
do not invent a bespoke signature protocol.

### High: checkout ownership and hooks form an ops subsystem

Use one dedicated integration branch per mesh, independent of source branches.
A separate repository is the simplest default, not a logical necessity: an
isolated ref/worktree could also work at greater operational cost.

Prefer a daemon-owned checkout outside source worktrees, keyed by immutable
mesh ID. Project configuration binds namespace to mesh ID and remote; local
configuration stores checkout paths and credentials. Avoid a clone per feature
worktree, nested Git accidents, and competing daemon writers.

A source pre-push hook should enqueue sync and return promptly, not wait for
network or fail the source push. It runs before source push completes: there is
no cross-repository transaction or atomic publication. Hooks are not reliably
inherited by clones and can conflict with hook managers. Offer installation
explicitly; manual sync remains first-class. Client-specific hooks belong in
client shims, not core mesh logic.

### High: deletion and old clients can resurrect records

Ordinary removal writes a stable-ID tombstone. Unlinking a file does not erase
Git history and loses the identity needed to suppress offline resurrection.
A privacy purge covers DB projections, search/embedding caches, Git history,
reflogs, hosting caches where possible, backups, and peer clones. Decentralized
sharing cannot guarantee erasure from an uncooperative peer. Disclose this
before enrollment; file deletion is not erasure.

History rewriting requires a mesh epoch change and coordinated re-enrollment.
Reject old-epoch publication; inspect/quarantine old outboxes rather than replay
them automatically into the new epoch.

### Medium: year-scale maintenance and recovery

D eliminates application snapshot folding, not storage maintenance. Tiny commits,
record rewrites, tombstones, and file counts still grow. Illustration, not a
workload estimate: ten users at 100 mutations/day each produce 365,000 commits
per year with commit-per-mutation. Batch commits, cap record sizes, shard paths
by ID prefix when measured, and schedule standard Git maintenance.

Track repository bytes, file/commit counts, clone/import latency, outbox age,
sync failures, and conflict backlog. Benchmark representative histories before
choosing numerical limits. History truncation is an explicit epoch migration,
never unattended compaction. Budget for credentials, client upgrades, conflict
resolution, backup/recovery, and enrollment support—not just storage.

## 3. Specific refinements

### A. Exchange schema

Keep UTF-8 JSON: stable key order, one field per line, normalized representation,
no volatile counters. Require a manifest with immutable mesh ID, namespace
identity, epoch, schema version, and required capabilities. Records contain ID,
type, content/definition, lifecycle, origin, update ID, and relationship IDs.
Import identity is (mesh_id, entity_id); local namespace labels are not global IDs.

Procedure definitions may include configured initial rehearsal budgets, never
consumed/remaining runtime budgets. Import cannot reset local counters or execute
a procedure. A semantic definition revision may request a locally approved reset;
routine sync cannot. Do not recompute consumption from shared history.

Content hashes identify equivalent payloads, not equivalent entities. Identical
text under different UUIDs may have different provenance or relationships. Git
blob dedup saves storage; it does not merge identities. Present duplicate
candidates; retain IDs or explicitly alias them and safely update references.

### B. Causal merge and lifecycle state machine

Use Git merge bases and record revisions to distinguish sequential changes from
concurrency. Git ancestry supplies causal context; skip vector clocks initially.
Preserve ancestry using merge commits, not blind rebasing/overwriting. Multiple
merge bases require supported recursive-base handling or conservative conflicts.

Proposed lifecycle:

- active -> archived: explicit, reversible archive.
- archived -> active: explicit restore observing the archived revision.
- active/archived -> superseded(replacement_id): terminal retirement.
- any live state -> tombstoned: terminal; deletion wins over stale updates.
- Retired IDs never reactivate. Create a new linked ID when necessary.

Separate content from lifecycle; a content edit never implies restore. Accept
unilateral and identical concurrent changes. Automatically combine concurrency
only through an allowlist of compatible operations; preserve conflict variants.

| Concurrent operations | Required result |
| --- | --- |
| Archive + content edit | Edited content stays archived; report combination. |
| Restore + archive | Unresolved; keep inactive pending explicit resolution. |
| Supersede + stale restore | Superseded; report rejected restore. |
| Supersede + content edit | Replacement authoritative; preserve edit candidate. |
| Two different replacements | Unresolved; never choose by timestamp. |
| Incompatible content edits | Unresolved; preserve both versions. |
| Tombstone + edit/restore | Tombstone effective; never serve deleted content. |

Archive/edit combination is an explicit semantic policy, not LWW. Report every
automatic concurrency decision. Conflict retention obeys deletion policy; a
purge must not retain sensitive content in a conflict artifact. An explicit
resolution references both variants so stale resolution cannot overwrite new work.

Minimal v1 may pause namespace integration at an unresolved conflict. Keep the
last accepted projection and pending local work; suppress affected IDs through
a local conflict overlay. Disclose paused shared integration. Unrelated local
work remains available. Do not label partial application a fully accepted head.

### C. Validated import and existing tools

Fetch into isolated staging; never import a half-merged working tree. Validate
remote identity, epoch, schema, paths, bounds, records, and relationships. Reject
symlinks, executable payloads, arbitrary hooks/config, and path traversal. Never
execute material from a mesh checkout. Accept only a validated candidate tree.

Apply its shared projection and accepted-head cursor atomically if supported;
otherwise specify a recoverable idempotent staging protocol. A cursor alone is
insufficient when a DB crash can lose imported rows independently. Use stable
IDs/revisions for repeated application; do not promise exactly-once delivery.
Support additions, updates, tombstones, and explicit identity migrations.
Unexpected bare file deletion is a protocol error, not permission to erase a
private local record.

Missing cursor, divergent ancestry, or changed epoch triggers validated full-tree
reconciliation. Retire only mesh-owned projection rows, never private memories.
Fresh enrollment imports the current tree, not years of events. Git log is
revision history, not a complete domain event ledger after batching or rewriting.

search_memory and snapshot use one effective local view: private/local records
plus accepted shared records, with lifecycle and conflict suppression. Return
provenance and freshness/conflict status; network absence does not fail queries.
Imported rules cannot silently outrank local security policy. export_graph uses
that view without initiating network sync or republishing derived data.

Unsupported required schema semantics stop integration with an actionable error.
Preserve unknown optional fields; old clients must not destructively rewrite
newer records. No permissive replay of lifecycle semantics an old client cannot
understand. Coordinate migrations through manifest capability requirements.

### D. Onboarding, triggers, recovery

1. Owner creates mesh remote and assigns authenticated team access.
2. Member explicitly approves remote, mesh ID, namespace mapping, sharing policy,
   and trust level; daemon clones into managed storage.
3. Validate/import current tree. Do not bulk-export historical local records;
   offer a separate dry-run publication preview.
4. Show freshness, pending operations, unpublished commits, conflicts, and health.
   Offer hooks, but never depend on them.

Serialize sync: flush durable outbox -> fetch -> semantic merge/validation ->
commit accepted candidate locally -> import transaction/cursor -> push. Keep
Git candidate head distinct from DB accepted head until import succeeds. Restart
must finish/recover import before claiming synchronized state or publishing.
Rejected push triggers bounded fetch/merge retries, never forced overwrite.

Distinguish locally durable, locally committed, imported, and published states.
"Saved" must not imply teammates received it. A separately authoritative task
extracts memory locally; publication remains asynchronous and policy-controlled.

Triggers: explicit sync/push/pull, best-effort session-start sync, optional
session-end flush, enqueue on source push. Session start is not a universal MCP
event: client shims signal it or core uses throttled attach-time checks.
Long sessions need manual refresh and optionally consented periodic fetch with
controlled import at a turn boundary. No silent timed push or real-time sync.
Warn about staleness without blocking offline agent work. Before destructive
procedure execution, show freshness and require local approval as policy requires;
eventual consistency cannot establish current remote permission.

Use a per-mesh lock across daemon processes. Dirty files, detached HEAD,
interrupted merges, auth failures, full disk, or corrupt objects pause sync, not
the local store. Preserve diagnostics, outbox, and unpublished commits. Provide
an actionable recovery command. Re-clone only after backing up unpublished work;
never automatic reset --hard/clean. Disk exhaustion affecting local durability
must fail the mutation honestly, not falsely acknowledge a saved memory.

### E. Alternative layouts/primitives

- One file per type: fewer files, same hot-file contention; reject.
- Immutable operation files: credible for full audit/replay and explicit conflict
  evidence, but add operation growth and projection/GC protocols. Prefer only
  if event fidelity becomes a real requirement.
- Direct Git objects: possible backend optimization, not a simpler protocol;
  loses ordinary file inspection and adds tooling/GC responsibilities.
- Automerge/CRDT: text convergence does not define safe retirement, purge, or
  procedural authority. Unjustified for deliberate, non-real-time sharing.
- Separate lifecycle files: reduce some textual contention, not semantic
  invariants. Defer until measured; prefer record-sized JSON now.

## 4. Red lines

- No bus-only durability promise or blocking Git/network mutation path.
- No silent whole-record LWW or resurrection of retired IDs.
- No sharing without namespace AND record/field eligibility controls.
- No task/session state or consumed procedure budgets in shared records.
- No procedure execution or privilege elevation from imported data alone.
- No automatic force-push, destructive repair, or unapproved historical export.
- No serving unvalidated or partially integrated shared state.
- No unqualified exactly-once, authenticated-author, or guaranteed-erasure claims.
- No destructive downgrade of newer schemas.
- Source branch independence is mandatory; separate repository is the preferred
  mechanism, not the invariant itself. Future branch-specific applicability
  belongs in explicit metadata, not checkout-branch coupling.

## 5. Acceptance evidence after reconciliation

Not run by this seat. Worker should demonstrate:

- Two offline nodes: disjoint adds converge; lifecycle cases above preserve
  intent, signal conflicts, and never resurrect retired records.
- Crash at every DB/outbox/file/commit/import/cursor boundary: no lost accepted
  update, duplicate side effect, partial snapshot, or re-export feedback loop.
- Duplicate IDs, identical text/different IDs, malformed or oversized JSON,
  invalid graphs, unknown schemas, and forged authors fail safely.
- Old-epoch offline peers cannot resurrect purged identities.
- Missing hooks, credential failures, dirty checkouts, rejected pushes,
  daemon restart, and multiple source worktrees preserve local operation.
- Full-tree recovery preserves private rows and procedure consumption;
  representative year-scale data meets explicitly chosen latency/storage limits.

Bottom line: D solves file contention, not replication correctness. Shipping
risk lies in export/import transactions, lifecycle semantics, and trust—not JSON.
