# Generation retention — contract (WP2.6)

Status: Phase 2 / WP2.6. Issue: Cognilogical/NeuroStrata#148 (retention
half). The 1.2a loop-breaker landed in #153; this is the prune API and its
rollback constraint.

## Problem

An ingest publishes a new generation and leaves the previous one as history, so
the store holds every generation ever published. A prune needs a delete API
that cannot delete the row set a reader is using, and the previous generation
is the rollback target when a publish is reversed (and, per the panel, is the
generation a secret purge was meant to remove — so pruning it must be an
explicit decision, not a side effect).

## Contracts

### C1 — delete is explicit and namespace-scoped

`delete_generation(namespace, generation)` deletes rows with
`m.namespace = namespace` and the `<namespace>::<generation>::` id prefix.
Generation 0 is refused (not generation-scoped). The return value is the
number of rows deleted; zero is a no-op.

### C2 — the active generation is refused, inside the delete's write turn

The active generation of any source in the namespace is the live read set.
`delete_generation` refuses it and names the rollback constraint in the error;
the caller must move the pointer first (or roll back to it).

The active check runs **inside the same `write_with_deadline` turn as the
delete**, not before it. `publish_generation`/rollback take the same writer
lock, so they cannot flip the pointer between the check and the delete. The
check uses a fallible lookup: a failed query fails the delete rather than
reading as "no active generations" (which would make every generation look
superseded).

### C2a — each source's previous generation is retained

`publish_generation` records the pointer's prior value as
`previous_generation` in the same turn as the flip. `prune_generations`
retains every source's previous generation. Stores upgraded from before the
column existed have no recorded value; the prune recovers each source's
rollback target from the retained rows (their `source_identity.worktree_id`
and generation) instead of deleting that history on the first run. The newest-`keep` window is
namespace-wide, but rollback is per source: without this, a quiet source's
rollback target can fall outside the window when another source publishes
newer generations.

The pruning path re-checks the rollback target inside each delete's own write
turn (`delete_generation_protected`), because a concurrent publish can make the
prune's pointer snapshot stale between selection and deletion. The public
`delete_generation` keeps the explicit-purge behavior and does not apply that
protection. Recovered (backfilled) targets are persisted with a
`previous_generation = 0` CAS before deletion, so the recheck sees either the
fresh publish value or the recovered one.

### C2b — a staging generation is never pruned

`begin_generation` inserts one `StagingGeneration` row per in-flight ingest;
publish and abort delete only that generation's row. A staged generation is
not superseded: `prune_generations` retains every staging generation, and
`delete_generation` refuses one. Per-row tracking (not a scalar on the source
pointer) is required because overlapping ingests of one source are permitted:
a scalar would keep only the newer number and leave the older generation
unprotected.

Each staging row also records the **base generation** that was active when the
ingest began. Carry-forward reads that generation, and a newer publish from
another ingest can push it out of the newest-`keep` window, so the base is
retained while the ingest is in flight.

A staging marker older than `STAGING_TTL_SECS` (24 h) is treated as abandoned:
a process that died between begin and publish/abort cannot remove its row.
`prune_generations` clears expired markers before selecting, so an interrupted
ingest does not make a generation unprunable forever. The TTL is well above any
real ingest; a slower one would have its base/staging protection lapse, which
is the documented trade-off for recoverability.

Otherwise a
long-running ingest could publish a pointer onto rows retention deleted
because newer generations pushed its number out of the newest-`keep` window.

### C3 — retention keeps active + previous

`prune_generations(namespace, keep)` always retains every active generation and
the newest `keep` generations (clamped to at least 2, so the previous
generation — the rollback target — survives). It deletes only the rest, one
`delete_generation` call per generation, and returns the deleted row count.
Generation numbers are namespace-wide across sources, so "newest" is taken
over the namespace; a source's older-but-active generation is retained because
every active generation is in the keep set.

### C4 — deleting a previous generation is explicit

`prune_generations` is the supported retention path. Calling
`delete_generation` directly on a previous generation is possible by design (an
operator may need to purge a known-bad generation), and that removes the
rollback target; the doc comment says so.

## Tests

| Test | Contract |
|---|---|
| `retention_keeps_active_and_previous` | C3: prune 1 of 3, previous + active survive, second prune is a no-op |
| `delete_generation_is_namespace_scoped` | C1: a foreign row sharing the prefix survives |
| `rollback_constraint_documented` | C2: the active generation is refused; the error names the rollback target |

## Acceptance

- Pruning keeps the rollback target by default.
- A foreign row with the same prefix is never deleted.
- The active generation cannot be deleted through the API.
- Mutation: keeping one fewer generation prunes the rollback target; dropping
  the namespace predicate deletes the foreign row; removing the active check
  deletes the live generation.
