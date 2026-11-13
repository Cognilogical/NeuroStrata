# PR Group 2: WAL Recovery & Checksum Verification Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Harden WAL recovery and checksum verification in `src/store/ladybug.rs` (`#69`, `#94`, `#90`, `#82`, `#86`).

**Architecture:** Extract pure env precedence helper `strict_wal_verification_from`, gate `enableChecksums` with `strict_wal_verification()` when a non-empty `.wal` exists, guard post-recovery `CHECKPOINT` and reopen under `verifying_config()` so the session does not stay unverified, and add behavioral tests for all 3 recovery arms.

**Tech Stack:** Rust (`cargo test`).

---

### Task 1: Fix `strict_wal_verification` precedence (`#82`) and `enableChecksums` strict gate (`#94`)

**Files:**
- Modify: `src/store/ladybug.rs:1652-1674, 2070-2167`

- [ ] **Step 1: Write failing tests for `#82` (`STRICT_WAL=1` overrides `ALLOW_UNVERIFIED_WAL=1`) and `#94` (`enableChecksums` with non-empty `.wal` refuses when strict)**
- [ ] **Step 2: Implement `strict_wal_verification_from(strict: Option<&str>, allow_unverified: Option<&str>) -> bool` and gate the `checksum_mismatch` arm in `with_system_config` when a non-empty `.wal` exists and `strict_wal_verification()` is true**

### Task 2: Guard unverified WAL replay (`#69`), reopen with `verifying_config()` (`#90`), and add recovery-arm behavioral + census tests (`#86`)

**Files:**
- Modify: `src/store/ladybug.rs:2098-2167`

- [ ] **Step 1: Write tests for verified reopen after recovery (`#90`), zero-record/preserved `.unclean` WAL safety (`#69`), and all 3 recovery arms + `Database::new` call-site census (`#86`)**
- [ ] **Step 2: In `with_system_config`, after unverified replay + `CHECKPOINT`, drop the unverified `Database` and reopen with `verifying_config()` (unless the store itself requires checksums disabled), and verify all tests pass**
