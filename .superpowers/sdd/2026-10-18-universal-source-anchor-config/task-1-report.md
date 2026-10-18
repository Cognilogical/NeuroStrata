# Task 1 Report — Local Project-Context Configuration and CLI

## Status

Implemented Task 1 only. Core remains VCS-agnostic. The registry stores stable
source IDs mapped to canonical `.NeuroStrata` marker directories.

## Changes

- Added backward-compatible `Config.project_contexts` with an empty-map Serde
  default.
- Added explicit-path config load/save helpers. Saves serialize to a unique
  sibling temporary file, sync it, then rename it over the config.
- Added bind/unbind validation and operations. Bind rejects blank IDs, missing
  paths, non-directories, and paths whose supplied or canonical final component
  is not `.NeuroStrata`.
- Added `project-context bind`, `list`, and `unbind` CLI actions. Dispatch runs
  before daemon probing, embedder creation, and store construction. Mutations
  save only after successful validation; listing is sorted by source ID.
- Added config and CLI parsing/dispatch coverage, including unchanged config
  bytes after failed unbind and no database file creation during dispatch.

## Verification

- `cargo test config::tests:: -- --nocapture` — 5 passed.
- `cargo test project_context -- --nocapture` — 8 passed.
- `cargo test` — 555 passed, 0 failed.
- Isolated-HOME CLI smoke test — `project-context list` printed
  `No project contexts configured.` without daemon/database access.
- `git diff --check` — passed.
- `cargo fmt -- --check` — failed on existing formatting drift across unrelated
  code, including `src/task/mod.rs`, `src/store/ladybug.rs`, and `src/traits.rs`;
  broad formatting changes were intentionally avoided.

## Self-review

Reviewed the complete source diff before commit. No unrelated files were
staged. Existing untracked `docs/superpowers/plans/2026-10-18-universal-source-anchor-config.md`
was left untouched. Full-suite compilation emitted existing unused/dead-code
warnings; tests passed.
- Advisory task validation found parent task `neurostrata-836d` still in
  progress; this subtask is complete and the parent remains with its owner.

## Commit

- `bad5573 feat: configure local project contexts`

No push performed, per task brief.

## Review Fix Round 1 — Config Permissions

- Finding: atomic replacement could widen an existing restrictive config mode.
- Changed files: `src/config.rs`; this report.
- TDD RED, before production changes:
  - Command: `cargo test config::tests::save_to_preserves_existing_restrictive_permissions -- --exact --nocapture`
  - Output: `0 passed; 1 failed`; assertion observed mode `436` (0644), expected `384` (0600); exit code 101.
  - Added new-file mode test; command `cargo test config::tests::save_to_ -- --nocapture` then reported `0 passed; 2 failed`; both observed `436`, expected `384`.
- TDD GREEN:
  - Command: `cargo test config::tests::save_to_ -- --nocapture`
  - Output: `2 passed; 0 failed`.
  - Command: `cargo test config::tests:: -- --nocapture`
  - Output: `7 passed; 0 failed`.
  - Command: `cargo test`
  - Output: `557 passed; 0 failed`.
- Implementation: temporary config starts owner-only on Unix; existing
  permissions are copied before rename. Permission code is cfg-gated; portable
  `Permissions` preservation remains available on non-Unix.
- Self-review: `git diff --check` passed; only `src/config.rs` and this report
  changed. Existing untracked plan remains untouched. Existing unrelated compiler
  warnings remain.
- Commit: `54b07f5 fix: preserve config permissions`.
- No push performed, per request.

## Review Fix Round 2 — Preserve Unknown Config Fields

- Finding: config saves dropped unknown top-level JSON fields, risking data loss
  for keys introduced by newer versions or extensions.
- Changed files: `src/config.rs`; this report.
- TDD RED, before production changes:
  - Command: `cargo test config::tests::bind_and_save_preserves_unknown_top_level_config_fields -- --exact --nocapture`
  - Output: `0 passed; 1 failed`; `future_extension` was `null` after bind/save,
    instead of the original nested object; exit code 101.
- TDD GREEN:
  - Same command: `1 passed; 0 failed`.
  - `cargo test config::tests:: -- --nocapture`: `8 passed; 0 failed`.
  - `cargo test project_context -- --nocapture`: `8 passed; 0 failed`.
  - `cargo test`: `578 passed; 0 failed`.
- Implementation: `Config` stores unrecognized top-level fields in a flattened
  `HashMap<String, serde_json::Value>`. Regression test loads a nested extension
  field, binds a project context, saves, and asserts the nested value survives.
  Existing project-context data, atomic replacement, and permission handling
  remain intact.
- Self-review: `git diff --check` passed; only `src/config.rs` and this report
  changed. Test builds retain 12 existing unrelated compiler warnings. No push.
- Commit: `4a57c00 fix: preserve unknown config fields on save`.
