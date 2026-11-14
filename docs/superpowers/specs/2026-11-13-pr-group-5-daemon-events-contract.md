# PR Group 5: Daemon, Procmem, Token Test Isolation & Event Dispatcher — Contract (#173)

Status: Consolidated remediation for `#146`, `#140`, `#65`, `#168` against `main` (`9e3b2fb`). (`#68` remains open separately.)

## Contracts

### C1 — `procmem` injectable sampler seam, filename-only `growth_log`, and cached disabled-profile check (`#146`)
- `procmem::status_json()` returns `"memory-growth.log"` (filename-only) for `growth_log` instead of the full home-anchored path (`src/procmem.rs:172`).
- `procmem::sample_once_with(read_rss: impl FnOnce() -> Option<u64>, threshold_bytes: u64, log_path: &Path)` (used by `run_sampler`) allows deterministic unit tests to drive RSS growth and assert JSONL row emission without depending on OS RSS changes.
- `profile_enabled()` caches the env lookup in a `OnceLock<bool>` (with a `#[cfg(test)]` override/reset or helper) so `CallGuard::enter` performs a single relaxed load and returns `None` immediately when disabled without per-call `std::env::var` syscalls.

### C2 — Daemon token test isolation from `~/.config/neurostrata/daemon.token` (`#65`)
`crate::daemon::token_config_dir()` checks `NEUROSTRATA_TOKEN_DIR` (and under `#[cfg(test)]` defaults to a process-scoped temp directory when `NEUROSTRATA_TOKEN_DIR` is unset), and `src/server.rs` `daemon_token_path()` delegates to `crate::daemon::token_path()` / `token_config_dir()` so `cargo test` never touches the operator's real `~/.config/neurostrata/daemon.token`.

### C3 — Daemon memory advisory in `@neurostrata/plugin-core` (`#140`)
- Daemon error/status responses emit a structured `NEUROSTRATA_ADVICE[daemon_memory]` marker on the error/diagnostic channel when buffer-pool / RSS pressure is reported (never matched from user memory content).
- `plugins/core/index.ts` exports `extractDaemonAdvisory(errorChannelText)` / `maybeFormatDaemonAdvisory(errorChannelText, nowMs)` with a 10-minute per-session throttle (`advisoryLastShown`, `resetAdvisoryStateForTesting`), rebuilds all 5 plugin `dist/index.js` bundles, and tests it in `plugins/core/test.mjs`.

### C4 — Event bus 250ms subscriber timeout retry/latch for `export_freshness_dirty` (`#168`)
When a subscriber times out after `SUBSCRIBER_TIMEOUT` (250ms) in `ThalamicBus::dispatch` (`src/events/bus.rs:470`), if the subscriber opts into timeout retry (`fn retry_on_timeout(&self) -> bool { false }`, overridden to `true` by `ExportFreshnessDirty`), the dispatcher schedules a bounded background retry (or records a pending-dirty namespace latch flushed on the next tick/background retry) so a slow first attempt (`>250ms`) still lands the `export_freshness::<namespace>` dirty sentinel row.

## Tests

| Test | Contract |
|---|---|
| `procmem_status_json_uses_filename_only_and_sampler_seam_emits_row` | C1 |
| `daemon_token_tests_never_touch_real_home_config` | C2 |
| `daemon_memory_advisory_throttles_and_ignores_memory_content` (`plugins/core/test.mjs`) | C3 |
| `export_freshness_dirty_survives_first_attempt_subscriber_timeout` | C4 |
