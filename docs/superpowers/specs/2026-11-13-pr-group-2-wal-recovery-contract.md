# PR Group 2: WAL Recovery & Checksum Verification Hardening — Contract (#170)

Status: Consolidated remediation for `#69`, `#94`, `#90`, `#82`, `#86` against `main` (`9e3b2fb`).

## Contracts

### C1 — Precedence of `NEUROSTRATA_STRICT_WAL=1` over `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1` (`#82`)
`strict_wal_verification_from(strict: Option<&str>, allow_unverified: Option<&str>) -> bool` evaluates `NEUROSTRATA_STRICT_WAL` first:
- `Some("1")` -> `true` (even if `NEUROSTRATA_ALLOW_UNVERIFIED_WAL == Some("1")`, emitting a warning when both conflict)
- `Some("0")` -> `false`
- Otherwise -> `!matches!(allow_unverified, Some("1"))` (default `true`).

### C2 — `enableChecksums` mismatch arm honors `strict_wal_verification()` (`#94`)
When `Database::new` fails with `"enableChecksums"` in `with_system_config`:
- If `strict_wal_verification()` is `true` and a non-empty `.wal` file exists at `<local_path>.wal`, `with_system_config` refuses to open with an actionable error instead of silently falling back to `unverified_config()`.
- If `strict_wal_verification()` is `false` (or no non-empty `.wal` exists), it logs the warning, opens with `unverified_config()`, checkpoints if a WAL was present, and records the recovery provenance.

### C3 — Zero-record unverified WAL replay guard before `CHECKPOINT` (`#69`)
In the unverified WAL replay arm (`Database::new(&local_path, unverified_config())` after a WAL integrity failure):
- Before issuing `CHECKPOINT;` on the unverified handle, compare the pre-open preserved WAL size (`had_nonempty_wal`) against whether `.wal` was truncated to 0 / removed without landing changes or when the engine threw/truncated 0 records. Specifically, preserve the `.unclean.<ts>` copy before replay, and if the `.wal` existed and was non-empty before `unverified_config()` open, only checkpoint after verifying the connection is healthy; furthermore, after `CHECKPOINT`, reopen the database with `verifying_config()` (`#90`) so the running session never stays on `unverified_config()` (`enable_checksums(false)`).

### C4 — Session-scoped verified reopen after recovery (`#90`)
After `unverified_config()` replay + `CHECKPOINT;` succeeds, drop the unverified `Database` handle and reopen `Database::new(&local_path, verifying_config())` (falling back to the unverified handle only if the store itself was created without checksums). Expose `storage_status(&self)` / `opened_via_recovery` in `status` telemetry so degraded/recovered opens are visible to operators.

### C5 — Behavioral tests and recovery-arm census guard (`#86`)
Unit tests cover all 3 recovery arms (`enableChecksums`, `is_wal_integrity_failure` strict refusal + unverified opt-in, and unreadable-WAL `.corrupted.<ts>` rename) and a census test verifies `Database::new` call sites in `src/store/ladybug.rs`.

## Tests

| Test | Contract |
|---|---|
| `strict_wal_one_overrides_allow_unverified_wal_one` | C1 |
| `enable_checksums_mismatch_with_wal_refuses_under_strict_wal` | C2 |
| `unverified_wal_recovery_reopens_with_verifying_config_after_checkpoint` | C3, C4 |
| `wal_recovery_arms_census_and_corrupted_rename` | C5 |
