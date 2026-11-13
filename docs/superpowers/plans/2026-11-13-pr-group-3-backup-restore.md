# PR Group 3: Backup, Snapshot Export & Restore Atomicity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make snapshot export, shell backup, and CLI restore atomic and honor configured snapshot retention (`#95`, `#93`, `#89`, `#87`).

**Architecture:** Serialize `export_database` with `self.write_lock`, wire configurable `snapshot_retention_count`, stage copies in `copy_snapshot_into_place` and `scripts/backup.sh` via temp files before rename, and clean up target files if `Restore` fails post-copy verification.

**Tech Stack:** Rust (`cargo test`), Bash (`scripts/backup.sh`, `scripts/test-backup.sh`).

---

### Task 1: Serialize `export_database` (`#95`) and honor configured `snapshot_retention_count` (`#89`)

**Files:**
- Modify: `src/store/ladybug.rs:7807-7923`

- [ ] **Step 1: Write unit tests for `export_database` retention config (`keep < 5` e.g. `NEUROSTRATA_SNAPSHOT_RETENTION_COUNT=2`) and write-lock consistency**
- [ ] **Step 2: Acquire `self.write_lock.lock().await` across `CHECKPOINT` + file copy in `export_database`, verify source metadata stability, and read `NEUROSTRATA_SNAPSHOT_RETENTION_COUNT` (falling back to `StorageHygieneConfig::default().snapshot_retention_count`) with `.max(1)`**

### Task 2: Failure-atomic `copy_snapshot_into_place` and `Commands::Restore` (`#87`)

**Files:**
- Modify: `src/main.rs:3402-3452, 4002-4018`

- [ ] **Step 1: Write unit tests in `src/main.rs` verifying `copy_snapshot_into_place` leaves no partial target files when a companion copy fails and cleans up on post-copy open failure**
- [ ] **Step 2: Stage copies to `.restore-tmp-<pid>` in `copy_snapshot_into_place`, rename into place on success, and roll back target files if `LadybugStore::new` fails in `Commands::Restore`**

### Task 3: Temp-file staging and filename-sorted pruning in `scripts/backup.sh` (`#93`)

**Files:**
- Modify: `scripts/backup.sh:150-265`
- Modify: `scripts/test-backup.sh`

- [ ] **Step 1: Add test cases in `scripts/test-backup.sh` for failed mid-backup preservation of existing same-second backup and filename-order pruning when mtimes are inverted**
- [ ] **Step 2: Update `scripts/backup.sh` to stage copies to `.tmp-$$`, verify MD5s before `mv`, sort by filename (`sort -r`) during pruning, and clean up orphaned companions**
