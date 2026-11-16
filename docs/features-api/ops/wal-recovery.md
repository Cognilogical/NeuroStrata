# WAL Recovery Runbook

## Symptom
NeuroStrata refuses to start with an error like:
- `Refusing to open: the write-ahead log did not verify.`
- `Refusing to open: DB checksum config mismatch (...) with a write-ahead log present under strict WAL verification.`

## Default policy (strict)
By default the daemon **fails closed**: an unauthenticated or checksum-mismatched
write-ahead log (`<db>.wal` or `<db>.wal.checkpoint`) is not replayed. The
database is left untouched so the preserved copy can be inspected or backed up
before recovery.

## Recovery workflow

1. **Inspect storage state (does not open the database):**
   ```sh
   neurostrata-mcp storage status
   ```
   This reports `store_bytes`, `wal_bytes` (including `<db>.wal.checkpoint`),
   `wal_age_secs`, `wal_recovery_artifact_count`,
   `wal_recovery_artifact_bytes`, `recovery_provenance`,
   `last_recovery_at`, and `opened_via_recovery` directly from the filesystem
   without opening the engine.
   - `wal_age_secs` is the age of the **newest** regular `<db>.wal` or
     `<db>.wal.checkpoint` (null when neither exists). A symlink at either
     name is not counted: it is not a WAL this store wrote. `wal_bytes` is
     measured over the names as the engine sees them (so it follows a
     symlink's target); the two fields can therefore describe slightly
     different file sets.
   - Preserved pre-replay WAL copies are kept at `<db>.wal.unclean.<timestamp>`
     (and `<db>.wal.checkpoint.unclean.<timestamp>`) with `0600` permissions.
   - Unreadable WALs that had to be moved aside during opt-in recovery are kept
     at `<db>.wal.corrupted.<timestamp>`.

2. **Take a no-open snapshot backup before attempting recovery:**
   ```sh
   neurostrata-mcp backup /path/to/pre-recovery-backup --no-open
   neurostrata-mcp storage verify-snapshot /path/to/pre-recovery-backup
   ```
   `backup --no-open` (and `backup` automatically when strict WAL verification
   refuses an open) copies `<db>`, `<db>.wal` / `<db>.wal.checkpoint`, the
   checkpoint state (`<db>.shadow` and `<db>.pinned.tmp` when present), and
   `<db>.store-state.json` directly into a checksummed snapshot directory
   without opening or checkpointing the store.

   `--no-open` **refuses while a process holds the store lock** (`#209`): the
   store may be mid-checkpoint, so the database, its WAL, and the shadow would
   be copied at different instants. Run `backup <dir>` without `--no-open` to
   have the daemon take a coordinated snapshot, or `shutdown` first — the
   recovery path in this runbook runs with the daemon down, so it is not
   affected.

   The snapshot manifest records `coordinated: false` and
   `provenance: "unverified"` for these copies, plus `wal_kind`
   (`active` / `frozen` / `none`). `storage verify-snapshot` prints
   `integrity ok, uncoordinated` for them rather than `valid`: the bytes match
   their checksums, but nothing proves they came from one instant. They remain
   restorable, and `restore` warns when it places one.

   The WAL is always stored as `ladybug.store.wal` inside the snapshot; its
   `wal_kind` decides where `restore` puts it back (`<db>.wal` for `active`,
   `<db>.wal.checkpoint` for `frozen`), together with the shadow and pinned
   marker when the snapshot carries them.

3. **Recover committed WAL prefix records with lenient tail truncation (operator opt-in):**
   ```sh
   NEUROSTRATA_STRICT_WAL=0 neurostrata-mcp daemon
   # or
   NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1 neurostrata-mcp daemon
   ```
   - Matching the WAL header's `enableChecksums` flag with
     `throw_on_wal_replay_failure(false)` replays all valid committed prefix
     records, truncates any corrupted tail record, runs `CHECKPOINT` to drain
     the WAL into the main database file, and reopens the live session under
     strict `verifying_config()`.
   - If the lenient replay recovers **zero** committed records from a non-empty
     WAL, NeuroStrata refuses to checkpoint over the WAL and restores `<db>.wal`
     from the preserved `<db>.wal.unclean.<timestamp>` copy so no WAL bytes are
     silently destroyed. The restore never writes through a symlink planted at
     `<db>.wal`: a symlink at the target is refused (and named in the refusal),
     and the target is reopened `O_NOFOLLOW` and left `0600` — explicitly, so a
     pre-existing looser mode is tightened rather than inherited (`#212`).

4. **Discard an unreadable WAL or restore from a known-good snapshot:**
   - If the WAL cannot be opened even with lenient replay, opt-in recovery
     moves it aside to `<db>.wal.corrupted.<timestamp>` (`0600`) and reopens the
     checkpointed main database file under `verifying_config()`.
   - To restore a verified snapshot into a fresh database path instead:
     ```sh
     neurostrata-mcp restore /path/to/snapshot --into /path/to/restored.lbug
     ```
     Then update `db_path` in `~/.config/neurostrata/config.json` after
     inspecting the restored store.

## Durable provenance in the store-state sidecar (`#212`)

Recovery provenance used to live only in the process that recovered, so
`storage status` in a fresh process had no provenance to report. It is now
also written to `<db>.store-state.json` (`#212`), beside the identity fields
the model-choice guard already reads — so a path-only status reports
`recovery_provenance` and `last_recovery_at`, while `opened_via_recovery`
stays handle-scoped:

| field | meaning |
|---|---|
| `last_recovery.at` | RFC 3339 UTC timestamp of the recorded recovery open |
| `last_recovery.kind` | recovery arm: `checksum_config_mismatch`, `unverified_wal_replay`, or `discarded_unreadable_wal` |
| `last_recovery.wal_bytes` | bytes across the WAL pair observed before the recovery open |
| `last_checkpoint_at` | RFC 3339 UTC timestamp of the last recorded checkpoint |
| `last_checkpoint_duration_ms` | its duration in milliseconds |
| `checkpoint_failures` | failed checkpoints recorded for this store (monotonic) |

- The fields are `serde(default)`: a sidecar written before `#212` (or by a
  build without the fields) parses unchanged and reads as "nothing recorded".
- A store with **no** sidecar does not gain one from a recovery: the recipe
  would have to invent an embedding identity this process cannot attest, and a
  false `has_data` would unlock model choice. For those stores the in-process
  map is still the only provenance layer.
- The live flag and kind are **handle-derived**, but the map behind them is
  **process-scoped**: a live status reflects any handle this process opened for
  that path, and both the flag and the map vanish when the process exits —
  only the sidecar (and therefore `last_recovery_at`) survives.
- A **downgraded** build that predates `#212` does not know the health fields:
  its identity writes (the pristine stamp, the data flip, pristine adoption)
  rewrite the sidecar without them, so the persisted provenance and checkpoint
  health are dropped by the first such write after a downgrade. Re-upgrading
  starts the history over from the next recovery/checkpoint.
- `last_recovery` is history, not the state of the current process: a later
  verified open does not erase it. **Two fields carry it, and they mean
  different things:**
  - `opened_via_recovery` is **handle-scoped**: `true` only when the live
    handle that answered this status was opened through recovery. A verified
    reopen after a recovery reports `false` — a health check on a healthy
    daemon must not alarm on a past recovery.
  - `last_recovery_at` is **history**, on every surface: it answers "did this
    store ever recover, and when?" for a live daemon and a path-only
    `storage status` alike. `recovery_provenance` carries the live kind when
    there is one, and the persisted kind for a path-only query (where
    `opened_via_recovery` is necessarily `false`, because nothing was opened).
- `last_checkpoint_at` is absent only when neither a checkpoint has been
  recorded here nor the store directory has a readable mtime; the directory
  mtime remains the last-resort proxy.

## Accounting verdicts (`#207`)

Every recovery arm classifies the WAL pair before it checkpoints. The refusal
message carries one of seven machine-readable slugs:

| slug | meaning | operator action |
|---|---|---|
| `no_wal` | no WAL before or after the open | none |
| `nothing_committed` | bytes were replayed, nothing had committed | none; the replay is lossless |
| `committed_lost` | a committed record existed and no bytes survived | stop, inspect the preserved copy |
| `replayed` | bytes survived, or a completed checkpoint was consumed | none; the message reports the truncation |
| `foreign_wal` | the engine reports a different database id for the WAL | the WAL is quarantined as `.corrupted.<ts>` with every byte kept; confirm the attribution before deleting it |
| `unknown` | the bytes could not be attributed | stop, inspect the preserved copy |
| `deferred` | the WAL is larger than the 64 MiB scan cap, so it was not classified | the verified open ran; if it failed, treat the store as `committed_lost` |

How the verdict reaches you:

- `committed_lost` and `unknown` **refuse** the open. The message names the
  verdict, both WAL sizes, the restored side(s), and every preserved
  `.wal.unclean.<ts>` / `.wal.checkpoint.unclean.<ts>` path. The WAL is never
  checkpointed over. Any side that lost every byte is restored from its
  preserved copy, so a zero-record refusal leaves **both** `<db>.wal` and
  `<db>.wal.checkpoint` on disk again (byte-identical to the pre-replay files);
  a side that still has bytes is left as the engine truncated it.
- `foreign_wal` **quarantines**: the named side (active, frozen, or both) is
  renamed to `.corrupted.<ts>` and the store reopens with
  `DiscardedUnreadableWal`, exactly as it always has for an unreadable WAL.
- `deferred` is not a verdict on the data: the WAL is larger than the 64 MiB
  scan cap, so the normal verified open runs first. Only if that open fails does
  the arm refuse, because it will not checkpoint a WAL it could not classify.
- Under `NEUROSTRATA_STRICT_WAL=1` (the default) every one of these is a plain
  refusal first: strict mode never replays, including a clean foreign WAL.

Two policies are worth knowing before interpreting a refusal:

- **A WAL written without checksums cannot be verified.** If such a WAL is
  discarded and no commit record is found, the verdict is `unknown`
  (`UnverifiableWal`) even when the WAL was legitimately empty. This
  over-refusal is deliberate: an unchecksummed WAL is indistinguishable from one
  whose commit record was destroyed. Inspect the preserved copy, then either
  restore it or delete it to let the store open.
- **A WAL larger than 64 MiB is not scanned.** The recovery pre-flight defers to
  the normal verified open (`deferred`); if that open succeeds, nothing else
  happens. If it fails, the WAL is not classified and the arm must not
  checkpoint it: treat the store like a `committed_lost` case and use the
  preserved copy / snapshot path above.
- **A preservation failure refuses the open before any replay.** If the active
  or frozen WAL cannot be copied aside (a symlink, a directory, an I/O error),
  the daemon refuses with `could not preserve <path>: …; the <side> copy was
  kept at <path>` and leaves every copy it did make in place, unpruned. Fix the
  cause (remove the symlink, free the space) and retry; do not delete the
  partial `.wal.unclean.<ts>`/`.wal.checkpoint.unclean.<ts>` copies.
- **A quarantined `foreign_wal` is preserved, not "nothing lost".** The message
  says nothing of *this* store was discarded, because a WAL carrying another
  database's id is not this store's data. If the attribution itself is wrong —
  for example a checksums-off WAL whose database id could not be authenticated —
  the bytes are still in the `.corrupted.<ts>` artifact. Restore from it instead
  of treating the message as proof that nothing was lost.

## Configuration

| Variable | Effect |
|---|---|
| `NEUROSTRATA_STRICT_WAL=1` (default) | Refuse to open when the WAL or frozen checkpoint WAL fails verification. |
| `NEUROSTRATA_STRICT_WAL=0` | Replay the valid WAL prefix with lenient tail truncation, checkpoint, and reopen verified. |
| `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1` | Alias for `NEUROSTRATA_STRICT_WAL=0` (`NEUROSTRATA_STRICT_WAL=1` wins if both are set). |
| `NEUROSTRATA_SNAPSHOT_RETENTION_COUNT` | Retention count applied to snapshots and `.wal.unclean.*` / `.wal.corrupted.*` artifacts. |
