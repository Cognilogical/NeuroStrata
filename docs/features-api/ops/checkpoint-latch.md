# The Buffer-Pool Checkpoint Latch

> **Why you care:** When a checkpoint cannot fit in the database engine's buffer
> pool, NeuroStrata stops retrying it for the rest of the process. Writes keep
> landing, but they stay in the write-ahead log (WAL) until a checkpoint
> succeeds or the process is restarted with a bigger pool. Status now says so
> (`checkpoints_stopped`, `checkpoint_skips`), and this page is the recovery
> step.

---

## 1. What the latch is

A CHECKPOINT flushes the WAL into `ladybug.db`. It needs buffer-pool memory for
the pages it is writing. If the pool is too small for the work in the log, the
engine fails the checkpoint part way: the pages it had already added stay in
`ladybug.db`, the WAL is left as it was, and every retry fails the same way
while growing the file again.

The engine retries a checkpoint after each write commit once the WAL is past its
16 MB auto-checkpoint threshold, and the daemon asks for one every checkpoint
interval. On exactly the workload that exhausted the pool, that is a retry loop
that only grows the database.

So the first time a checkpoint runs out of buffer pool, NeuroStrata latches:

- `CALL auto_checkpoint=false` and `CALL force_checkpoint_on_close=false` are
  issued on the shared database config;
- no further checkpoint is attempted, and each request the latch skips is
  counted;
- a warning is printed naming the settings to raise.

The latch is **in memory and per process**. Nothing persists it: a restart
starts with the engine's checkpointing at its defaults, which is why restart is
the recovery step.

### One standalone attempt is still allowed

A failure that comes from a write's *post-commit* checkpoint leaves one
standalone attempt armed, because that checkpoint runs inside the write with the
write's memory already held, while a standalone CHECKPOINT afterwards has the
pool to itself and may well fit. If that one attempt succeeds, the engine's
checkpointing is turned back on
(`CALL auto_checkpoint=true`, `CALL force_checkpoint_on_close=true`) and the
latch is cleared.

If it fails again, the latch stays and nothing retries. There is no retry loop:
the attempt is armed once per latch, not per interval.

## 2. What it costs while it is set

- New writes still commit: they go to the WAL, not to a lost buffer.
- `ladybug.db` stops growing (nothing is being flushed into it), but the **WAL
  grows** with the writes that arrive, and the 16 MB auto-checkpoint bound no
  longer applies because auto-checkpointing is off.
- Nothing is lost on a clean restart: the next open replays the WAL. A crash or
  a hard kill before that replay is the risk — everything since the last
  successful checkpoint is only in the log.
- Disk usage is the practical limit. "The WAL grows without the 16 MB bound"
  means until the latch clears, the process restarts, or the disk fills; it does
  not grow forever.

## 3. Seeing it in status

`storage_status` (the MCP tool and the daemon's `/health`) reports two fields
for this. The path-only `neurostrata-mcp storage status` cannot see an
in-process latch, so it reports the defaults; `neurostrata-mcp status` probes
the daemon/store and does not report these fields.

| Field | Meaning |
| :--- | :--- |
| `checkpoints_stopped` | `true` when this process has latched and is not checkpointing. `false` once a checkpoint succeeds again, or after a restart. |
| `checkpoint_skips` | How many checkpoint requests the latch skipped, for the life of the process. Skips are counted **separately** from `checkpoint_failures`: a skip is not an attempt, so it never moves the failure count. |

`checkpoint_failures` still counts only checkpoints that actually ran and
failed. A store showing one failure and a large `checkpoint_skips` is latched
after a single failed attempt, not repeatedly failing.

Both fields are optional in the JSON (serde defaults): an older client sees them
absent, and a status payload written before they existed still parses.

## 4. Recovery

1. Note the warning/status, then stop the process. The WAL is intact; the next
   open replays it.
2. Raise the pool and restart:

   ```bash
   # ~/.config/neurostrata/config.json
   { "store": { "buffer_pool_bytes": 805306368 } }
   ```

   (the deprecated `storage_hygiene.buffer_pool_mb` key is still read once,
   with a warning, for one release). Or, per process:

   ```bash
   NEUROSTRATA_BUFFER_POOL_BYTES=805306368 neurostrata-mcp daemon
   ```

   `0` hands sizing back to the engine (it sizes the pool to most of the
   machine's memory). Values below the floor are raised to it.

3. The restart replays the WAL and checkpoints it with the larger pool. Confirm
   with `storage_status`: `checkpoints_stopped` is `false`, `wal_bytes` is back
   near zero after the first checkpoint.

Do **not** delete a `*.wal` file to "fix" this. It holds the writes that are not
in `ladybug.db` yet. If a recovery open refuses, follow
[`docs/runbooks/wal-recovery.md`](../runbooks/wal-recovery.md) — the refused
bytes are preserved, not discarded.

### Why the next open says "the last shutdown was not clean"

A clean close normally checkpoints on the way out. With the latch set, the
checkpoint is skipped by design, so the WAL is still there and the next open
reports an unclean shutdown and preserves a copy of the log. That report is
expected in this one case; the open also says that a checkpoint ran out of
buffer pool, and points at the larger-pool restart above. The preserved copy is
a forensic artifact, not evidence of a crash.

When the latch cleared before the close (the standalone attempt succeeded), both
engine settings are back on, the close checkpoints, no WAL is left behind, and
the next open is clean.

## 5. What this does not do

- It does not resize the pool. Sizing is a configuration decision (see
  `store.buffer_pool_bytes`); the floor exists because a pool too small for a
  single query makes queries fail.
- It does not remove the retry loop's cause. It removes the retries.
- It does not persist the latch across restarts. A restart is the recovery.
