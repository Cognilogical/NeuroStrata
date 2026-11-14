# PR Group 5: Daemon, Procmem, Token Test Isolation & Event Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix `procmem` items (`#146`, task `neurostrata-sayg`), isolate daemon token tests from `~/.config/neurostrata/daemon.token` (`#65`), add the throttled daemon memory advisory in `@neurostrata/plugin-core` (`#140`), and prevent 250ms subscriber timeouts from dropping `export_freshness_dirty` flips (`#168`).

**Architecture:** Update `src/procmem.rs` (`status_json`, injectable sampler step, cached `profile_enabled`), add `NEUROSTRATA_TOKEN_DIR` + `#[cfg(test)]` process temp dir in `src/daemon.rs` and `src/server.rs`, add `retry_on_timeout` recovery in `src/events/bus.rs` + `src/events/subscribers/export_freshness_dirty.rs`, and add `NEUROSTRATA_ADVICE[daemon_memory]` handling in `plugins/core/index.ts`.

**Tech Stack:** Rust (`cargo test`), TypeScript/Node.js (`node --test plugins/core/test.mjs`).

---

### Task 1: `procmem` filename-only `growth_log`, injectable sampler step, and cached disabled check (`#146`)

**Files:**
- Modify: `src/procmem.rs`

- [x] **Step 1: Write unit tests for `status_json()["growth_log"] == "memory-growth.log"` and deterministic `sample_once_with` growth-log emission**
- [x] **Step 2: Implement filename-only `growth_log`, `sample_once_with`, and cached `profile_enabled`**

### Task 2: Daemon token test isolation (`#65`)

**Files:**
- Modify: `src/daemon.rs:17-60`
- Modify: `src/server.rs:8775-8850`

- [x] **Step 1: Write sentinel test verifying `~/.config/neurostrata/daemon.token` is never read/written/deleted during token tests**
- [x] **Step 2: Support `NEUROSTRATA_TOKEN_DIR` and `#[cfg(test)]` isolated temp dir in `daemon::token_config_dir()` and wire `server.rs` tests to use it**

### Task 3: Event bus 250ms subscriber timeout retry for `export_freshness_dirty` (`#168`)

**Files:**
- Modify: `src/events/mod.rs`
- Modify: `src/events/bus.rs`
- Modify: `src/events/subscribers/export_freshness_dirty.rs`

- [x] **Step 1: Write deterministic test where `ExportFreshnessDirty` (or a slow-first-attempt wrapper) exceeds 250ms on attempt 1 and verify the dirty sentinel row still lands in the store**
- [x] **Step 2: Add `fn retry_on_timeout(&self) -> bool { false }` to `MemorySubscriber` (`true` on `ExportFreshnessDirty`) and retry once with an extended recovery deadline on `Err(_elapsed)` in `ThalamicBus::dispatch`**

### Task 4: Daemon memory advisory in `@neurostrata/plugin-core` (`#140`)

**Files:**
- Modify: `plugins/core/index.ts`
- Modify: `plugins/core/test.mjs`
- Regenerate: `plugins/*/dist/index.js`

- [x] **Step 1: Write unit tests in `plugins/core/test.mjs` for `NEUROSTRATA_ADVICE[daemon_memory]` extraction from error channel only (never memory content) and 10-minute throttle (`advisoryLastShown`, `resetAdvisoryStateForTesting`)**
- [x] **Step 2: Implement advisory helper in `plugins/core/index.ts`, rebuild all 5 plugin bundles, and run `node --test plugins/core/test.mjs`**
