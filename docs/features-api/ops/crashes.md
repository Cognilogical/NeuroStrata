# Crash Recovery & WAL Integrity

> **Why you care:** An unexpected power loss, OOM kill, or terminal crash never silently corrupts your cognitive graph — NeuroStrata records structured crash breadcrumbs, verifies the write-ahead log (WAL) on startup, and provides deterministic recovery tools.

---

## 1. Crash Breadcrumbs & Diagnostics

`neurostrata-mcp` installs a signal handler (`src/crash.rs`) for the four fatal signals `SIGSEGV`, `SIGBUS`, `SIGABRT` and `SIGFPE`. When one of them fires, the handler writes a breadcrumb file into:

```text
~/.config/NeuroStrata/data/db/crash/
```

The directory is `<db_path parent>/crash` — it follows the store wherever `db_path` points, and falls back to the config directory when the config cannot be read. Each breadcrumb is named `crash-<pid>-<epoch>.txt` and contains:

- The signal number and its symbolic name (`signal=11`, `signal_name=SIGSEGV`).
- The process id and the id of the thread that faulted (`pid`, `thread_id`).
- The wall-clock second of the crash (`epoch_seconds`).
- The size of the alternate signal stack the handler ran on (`alt_stack_bytes`) — it says why a backtrace may be missing, because the capture needs 32 KiB of it.
- A `--- backtrace ---` section, when the alternate stack had room for the capture.

Two things the breadcrumb is **not**: it does not record the binary version, commit hash or target triple, and it does not record the lock state or the WAL size. It is a minimal, allocation-free report written with async-signal-safe calls only (`open` / `write` / `close`), because a handler that allocates or takes a lock can deadlock against the thread that is already dying.

There is no Rust panic hook, so an ordinary `panic!` — which unwinds rather than faulting — does **not** produce a breadcrumb; it unwinds normally and prints the standard panic message to stderr.

After writing the breadcrumb the handler re-raises the signal with the default disposition, so the process dies *of the signal* rather than of an exit code: systemd still reports `status=11/SEGV`, and the kernel writes a core dump if the host is configured to keep one. NeuroStrata does not suppress or redirect core dumps.

---

## 2. Checking Daemon & Lock State After a Crash

Before restarting a crashed session, run the read-only preflight check:

```bash
neurostrata-mcp status
```

| Exit Code | Meaning | Recommended Action |
| :--- | :--- | :--- |
| `0` | Daemon is healthy and serving the current build | None — ready for MCP calls. |
| `1` | No daemon running and lock is free | Start the daemon (`neurostrata-mcp daemon`) or let your harness spawn it. |
| `2` | Lock held by a wedged or shutting-down process | Run `neurostrata-mcp shutdown` to release the lock cleanly. |
| `3` | Daemon is running an older binary build | Run `neurostrata-mcp shutdown && neurostrata-mcp daemon`. |

---

## 3. Write-Ahead Log (WAL) Verification

LadybugDB maintains a write-ahead log (`*.wal`) alongside the main database file (`~/.config/NeuroStrata/data/db/ladybug.db`). On startup, NeuroStrata inspects the WAL header and replay checksums before opening the store:

1. **Clean WAL Replay:** Valid uncommitted transactions are replayed and checkpointed automatically.
2. **Quarantined / Unverified WAL Refusal:** If `NEUROSTRATA_STRICT_WAL=1` (the default safety posture) detects a truncated or corrupt WAL frame, NeuroStrata **refuses to open or overwrite the WAL**, leaving the file untouched on disk for inspection:
   ```text
   Refusing to open: the write-ahead log did not verify. The log has been left in place for inspection. To recover the writes without verification, set NEUROSTRATA_STRICT_WAL=0 (or NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1). See docs/runbooks/wal-recovery.md.
   ```
3. **Guided Recovery:** Follow the step-by-step runbook in [`docs/runbooks/wal-recovery.md`](../runbooks/wal-recovery.md) to quarantine the damaged WAL or replay verified frames using `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1`.

---

## 4. Checkpointed Backups & Safe Restore

NeuroStrata provides two built-in backup mechanisms that avoid LadybugDB's native `EXPORT DATABASE` planner:

### CLI Online/Offline Backup
```bash
neurostrata-mcp backup ~/backups/neurostrata-$(date +%F-%H%M%S)
```
The target directory **must not already exist** — `backup` refuses an existing path rather than writing into it, so include a timestamp in the name if you run it more than once a day. When the daemon is running, `backup` coordinates over `/backup` to issue a database `CHECKPOINT`, copy the store file, write a `manifest.json` (format, version, creation time, source path, byte count) and the snapshot checksums.

### Safe Non-Destructive Restore
`restore` never overwrites an existing database file in place — the `--into` path must also not exist. Restore into a fresh path, verify with `doctor`, and then update `db_path`:
```bash
neurostrata-mcp restore ~/backups/neurostrata-2026-11-04 --into ~/.config/NeuroStrata/data/db.restored
neurostrata-mcp doctor
```

### Automated Systemd Backup Script
For scheduled cron/systemd timer backups, use `scripts/backup.sh`, which gracefully stops the user service, verifies zero open file descriptors via `lsof`, copies and MD5-verifies the store, prunes old snapshots, and restarts the service.
