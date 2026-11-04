# Crash Recovery & WAL Integrity

> **Why you care:** An unexpected power loss, OOM kill, or terminal crash never silently corrupts your cognitive graph — NeuroStrata records structured crash breadcrumbs, verifies the write-ahead log (WAL) on startup, and provides deterministic recovery tools.

---

## 1. Crash Breadcrumbs & Diagnostics

Whenever the `neurostrata-mcp` binary encounters an unhandled panic or fatal signal, its panic hook writes a timestamped diagnostic report to:

```text
~/.config/neurostrata/crash/
```

Each crash breadcrumb includes:
- Binary version, commit hash, and target triple.
- Thread name, panic payload, and source location (`file:line:column`).
- Full stack backtrace and active operation context.
- Lock state and WAL file byte size at the moment of failure.

Core dumps are never written into project workspaces.

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

LadybugDB maintains a write-ahead log (`*.wal`) alongside the main database file (`~/.config/neurostrata/data/db`). On startup, NeuroStrata inspects the WAL header and replay checksums before opening the store:

1. **Clean WAL Replay:** Valid uncommitted transactions are replayed and checkpointed automatically.
2. **Quarantined / Unverified WAL Refusal:** If `NEUROSTRATA_STRICT_WAL=1` (the default safety posture) detects a truncated or corrupt WAL frame, NeuroStrata **refuses to open or overwrite the WAL**, leaving the file untouched on disk for inspection:
   ```text
   Refusing to open: the write-ahead log did not verify. The log has been left in place for inspection.
   ```
3. **Guided Recovery:** Follow the step-by-step runbook in [`docs/runbooks/wal-recovery.md`](../runbooks/wal-recovery.md) to quarantine the damaged WAL or replay verified frames using `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1`.

---

## 4. Checkpointed Backups & Safe Restore

NeuroStrata provides two built-in backup mechanisms that avoid LadybugDB's native `EXPORT DATABASE` planner:

### CLI Online/Offline Backup
```bash
neurostrata-mcp backup ~/backups/neurostrata-$(date +%F)
```
When the daemon is running, `backup` coordinates over `/backup` to issue a database `CHECKPOINT`, copy the store file, and write a cryptographic `manifest.json`.

### Safe Non-Destructive Restore
`restore` never overwrites an existing database file in place. Restore into a fresh path, verify with `doctor`, and then update `db_path`:
```bash
neurostrata-mcp restore ~/backups/neurostrata-2026-11-04 --into ~/.config/neurostrata/data/db.restored
neurostrata-mcp doctor
```

### Automated Systemd Backup Script
For scheduled cron/systemd timer backups, use `scripts/backup.sh`, which gracefully stops the user service, verifies zero open file descriptors via `lsof`, copies and MD5-verifies the store, prunes old snapshots, and restarts the service.
