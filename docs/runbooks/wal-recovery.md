# WAL Recovery Runbook

## Symptom
NeuroStrata refuses to start with an error like:
`the write-ahead log failed verification`.

## Default policy (strict)
By default the daemon **fails closed**: an unauthenticated WAL is not replayed.
The database is left untouched so the preserved copy can be inspected.

## Recovery options

1. Inspect first:
   - The original WAL copy is at `<db>.wal.unclean.<timestamp>` (created by `preserve_unclean_wal`).
   - A failed retry leaves `<db>.wal.corrupted.<timestamp>`.

2. Recover writes without verification (operator opt-in):
   ```sh
   NEUROSTRATA_STRICT_WAL=0 neurostrata-mcp daemon ...
   # or
   NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1 neurostrata-mcp daemon ...
   ```
   This replays the WAL without checksum verification. Records recovered this way
   are NOT authenticated; use only when tampering is not a concern.

3. Discard the WAL and reopen (data loss):
   If even unverified replay fails, the engine moves the WAL aside as
   `<db>.wal.corrupted.<timestamp>` and reopens without it. The moved file is
   retained for forensics; do not delete it until the writes are confirmed
   dispensable.

## Notes
- A WAL present at open time means the previous process did not close cleanly.
- Healthy databases are unaffected: verification only fails on a corrupt/tampered WAL.
- `NEUROSTRATA_STRICT_WAL=1` remains accepted for backward compatibility (strict).