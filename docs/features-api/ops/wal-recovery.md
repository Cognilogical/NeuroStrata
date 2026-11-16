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

## Configuration

| Variable | Effect |
|---|---|
| `NEUROSTRATA_STRICT_WAL=1` (default) | Refuse to open when the WAL or frozen checkpoint WAL fails verification. |
| `NEUROSTRATA_STRICT_WAL=0` | Replay the valid WAL prefix with lenient tail truncation, checkpoint, and reopen verified. |
| `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1` | Alias for `NEUROSTRATA_STRICT_WAL=0` (`NEUROSTRATA_STRICT_WAL=1` wins if both are set). |
| `NEUROSTRATA_SNAPSHOT_RETENTION_COUNT` | Retention count applied to snapshots and `.wal.unclean.*` / `.wal.corrupted.*` artifacts. |
