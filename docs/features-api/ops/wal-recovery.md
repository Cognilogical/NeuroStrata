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
   This reports `db_bytes`, `wal_bytes` (including `<db>.wal.checkpoint`),
   `wal_recovery_artifact_count`, `wal_recovery_artifact_bytes`, and
   `recovery_provenance` directly from the filesystem without opening the
   engine.
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
   refuses an open) copies `<db>`, `<db>.wal` / `<db>.wal.checkpoint`, and
   `<db>.store-state.json` directly into a checksummed snapshot directory
   without opening or checkpointing the store.

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
     silently destroyed.

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

## A removed frozen WAL is a replay only on evidence (`#228`)

The engine removes a frozen WAL (`<db>.wal.checkpoint`) when it replays it. That
removal is **not** proof of a replay: the engine also removes a frozen WAL whose
replay stopped at its very first record — the "semantic early failure" of a
record the engine cannot deserialize (an unknown table, a truncated owner name,
a record type it does not know). The open then succeeds, the committed records
are gone, and nothing was applied.

So the recovery accounting treats a **removed** frozen WAL as a replay only on
**positive evidence**:

| evidence | verdict | why |
|---|---|---|
| the pre-delete scan observed a complete `CHECKPOINT` record | `replayed` | the engine applies a checkpoint before it removes the file, so the checkpoint's records were applied |
| the recovery replayed the WAL's records and the engine then removed the file | `replayed` | the applied-record count is at least one, so the engine had records to apply before it removed the file |
| the pre-delete scan **proved** a committed record and the file is gone | `committed_lost` | a commit existed, the file no longer does, and nothing was applied: the committed records were lost |
| the pre-delete scan saw a commit it could not prove (no checksums, an unverified frame) | `unknown` | the bytes cannot be attributed to a commit |
| the pre-delete scan held no commit at all | `nothing_committed` | nothing was lost; the removal is lossless |

A *proved* commit means a verified walk: checksums present in the file, a
verified header, every complete frame's trailer verified, and a clean tail. A
checksums-off WAL never proves anything, so it lands in `unknown` — the
deliberate over-refusal described above.

What this means for an operator:

- A refusal naming `committed_lost` for a frozen WAL is **not** a failed
  recovery. It means the engine deleted a WAL that held committed records
  without applying them. The frozen WAL is restored byte-for-byte from its
  preserved copy, and the message never claims a replay.
- A refusal naming `unknown` for a frozen WAL means the same thing is possible
  but unproven. Inspect the preserved copy before deciding.
- The recovery line `Recovered by replaying the write-ahead log …` is printed
  only **after** the verified reopen has succeeded, and only when the verdict
  really is `replayed`. A successful open on its own is never reported as a
  replay.

The engine's own replay result (`WALReplayInfo::offsetDeserialized`) is not
exposed by the lbug binding yet, so the accounting **emulates** the
applied-record count from what the failed open already proved about the WAL:

- A **record-level** failure (`Corrupted wal file`, `invalid WAL record type`)
  means the WAL holds a record the engine cannot deserialize. The lenient
  replay reads the same bytes and stops at the same record, so the engine
  applies nothing and removes the frozen WAL. The applied-record count is
  zero, and a removed frozen WAL with a proven commit is `committed_lost`.
- A **header-level** failure (the checksum-config mismatch) or a **torn tail**
  leaves the records themselves intact. The engine replays them under the WAL
  header's own checksum setting and only then retires the file. The
  applied-record count is at least one, so a removed frozen WAL is `replayed`.
- A removed WAL that held no complete record applies nothing, whatever the
  failure was.

The emulation never raises the replay claim on a record failure, so a frozen
WAL whose records are broken is never reported as a replay. Its one weakness:
a WAL whose *later* record is broken may have had its earlier records applied
and checkpointed before the failure, and the accounting still refuses it as
`committed_lost`. The engine's own replay offset would settle that case, which
is why the binding exposure is tracked on its own.

## Configuration

| Variable | Effect |
|---|---|
| `NEUROSTRATA_STRICT_WAL=1` (default) | Refuse to open when the WAL or frozen checkpoint WAL fails verification. |
| `NEUROSTRATA_STRICT_WAL=0` | Replay the valid WAL prefix with lenient tail truncation, checkpoint, and reopen verified. |
| `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1` | Alias for `NEUROSTRATA_STRICT_WAL=0` (`NEUROSTRATA_STRICT_WAL=1` wins if both are set). |
| `NEUROSTRATA_SNAPSHOT_RETENTION_COUNT` | Retention count applied to snapshots and `.wal.unclean.*` / `.wal.corrupted.*` artifacts. |
