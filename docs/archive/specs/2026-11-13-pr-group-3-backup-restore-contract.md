# PR Group 3: Backup, Snapshot Export & Restore Atomicity — Contract (#171)

Status: Consolidated remediation for `#95`, `#93`, `#89`, `#87` against `main` (`9e3b2fb`).

## Contracts

### C1 — Consistent snapshot copy in `LadybugStore::export_database` (`#95`)
`LadybugStore::export_database` holds the store's `write_lock` across `CHECKPOINT` and the file copies (`ladybug.store`, `ladybug.store.wal`, `ladybug.store.store-state.json`) and verifies pre/post file metadata (size + mtime) on `ladybug.store` (retrying once if an external mutation occurred) before writing `manifest.json` and `write_checksums(&dir)`.

### C2 — Configured `snapshot_retention_count` in `export_database` (`#89`)
`LadybugStore::export_database` reads the configured `snapshot_retention_count` (via `StorageHygieneConfig` / `NEUROSTRATA_SNAPSHOT_RETENTION_COUNT` env override, defaulting to `StorageHygieneConfig::default().snapshot_retention_count` and floored at `.max(1)`) instead of hardcoding `StorageHygieneConfig::default().snapshot_retention_count.max(5)`.

### C3 — Staging and filename-timestamp pruning in `scripts/backup.sh` (`#93`)
`scripts/backup.sh` copies the DB, sidecar, and WAL companions to temporary `.tmp-$$` files, verifies MD5 checksums on the staged files, and atomically `mv`s the verified set into `db-$TIMESTAMP*` (removing stale companions on same-second reuse only when committing). Pruning sorts backups by filename (`ls -1 "$BACKUP_DIR"/db-* | sort -r`) rather than `ls -1t` mtime, and removes orphan `.tmp-*` or orphan companions whose base `db-$TIMESTAMP` no longer exists. `scripts/test-backup.sh` covers staging failure cleanup and filename-timestamp pruning.

### C4 — Failure-atomic `copy_snapshot_into_place` and `Commands::Restore` (`#87`)
`copy_snapshot_into_place` stages `ladybug.store`, `.wal`, and `.store-state.json` at temporary sibling paths (`<target>.restore-tmp-<pid>`) and renames them into place only after all copies succeed, cleaning up any staged files on error. In `Commands::Restore`, if post-copy `LadybugStore::new` open fails, the newly placed target files (`.store`, `.wal`, `.store-state.json`) are removed so a failed restore never leaves a broken store at `target`.

## Tests

| Test | Contract |
|---|---|
| `export_database_holds_write_lock_and_honors_configured_retention` | C1, C2 |
| `copy_snapshot_into_place_is_failure_atomic_and_cleans_up_on_error` | C4 |
| `bash scripts/test-backup.sh` | C3 |
