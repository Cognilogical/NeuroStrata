# Hook & Doctor Auto-Discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make git-hook installation/refresh and `doctor` runs agent-discovered via the agent's normal first action (`get_snapshot`), instead of requiring a human prompt.

**Architecture:** `get_snapshot` gains an optional `project_root` arg and performs two hygiene checks in-memory: (a) hook hash check that auto-installs/updates a stale NeuroStrata `pre-push` hook (backing up foreign/old ones), and (b) a throttled, event-driven `doctor` pass whose state lives in the private `registry_row`. CLI commands provide a fallback when the daemon is down. Full design in `docs/superpowers/specs/2026-10-26-hook-doctor-auto-discovery-design.md`.

**Tech Stack:** Rust, LadybugDB (Kuzu), axum daemon, clap CLI, blake3 (or sha2) for hook hashing.

**Spec:** docs/superpowers/specs/2026-10-26-hook-doctor-auto-discovery-design.md

## Global Constraints

- Backup before overwrite: any hook file being replaced is copied to `<target>.bak` first.
- Foreign hooks (no `"NeuroStrata task gate"` marker) are never touched without `--force`.
- `doctor_state` and `project_root` live in the private `registry_row`; never synced to SynapticMesh.
- Silent when clean: no token spam on healthy state.
- Daemon up/down parity: CLI fallbacks work when the daemon is down.
- No new MCP tools; piggyback on `get_snapshot`.

## Review Focus

- A fresh clone with no hook and no daemon: the first `task gate` run must self-heal the hook, not deadlock. (Task 7 covers CLI fallback.)
- A repo with a Husky/Lefthook `pre-push`: must be left untouched and reported, not clobbered. (Task 2/3 marker check.)
- `project_root` unknown to daemon (no ingest/setup/append_log yet): hook hygiene must skip silently, not error. (Task 4 fallback.)
- Upgrade detection: `last_run_version != CARGO_PKG_VERSION` must trigger a doctor pass even if cadence hasn't elapsed. (Task 5.)
- Dirty-worktree risk: `.bak` files must be gitignored or not created inside the repo's tracked tree. (Task 2 uses `.git/hooks/`, which is untracked — safe.)

---

### Task 1: Hook scaffold hash marker + helper

**Files:**
- Modify: `src/task/mod.rs` (SCAFFOLDED_PRE_PUSH_HOOK constant + new helpers)
- Test: `src/task/mod.rs` (existing test module)

**Interfaces:**
- Produces: `pub(crate) fn hook_sha256() -> String`, `pub(crate) fn is_ours_hook(content: &str) -> bool`, `pub(crate) fn is_current_hook(content: &str) -> bool`.

- [ ] **Step 1: Write failing test** for `hook_sha256()` returning a 64-hex-char string and `is_current_hook` returning true only when content contains both the marker and the matching hash.

- [ ] **Step 2: Run test** — expect FAIL (functions undefined).

- [ ] **Step 3: Implement** `hook_sha256()` (blake3 or sha2 of `SCAFFOLDED_PRE_PUSH_HOOK` bytes), append `# neurostrata-hook-sha256: <hash>` to the scaffold constant, and `is_ours_hook`/`is_current_hook` helpers.

- [ ] **Step 4: Run test** — expect PASS.

- [ ] **Step 5: Commit** `feat(hooks): add hash marker to scaffolded pre-push hook`.

---

### Task 2: `install_pre_push_hook` backup-on-overwrite

**Files:**
- Modify: `src/main.rs:754` (`install_pre_push_hook`)

**Interfaces:**
- Consumes: `is_ours_hook` from Task 1.
- Produces: unchanged signature; behavior change only.

- [ ] **Step 1: Write failing test** asserting that when a foreign hook exists and `--force` is given, the original is copied to `pre-push.bak` before overwrite.

- [ ] **Step 2: Run test** — expect FAIL.

- [ ] **Step 3: Implement** backup copy (best-effort, ignore errors) before `std::fs::write`, for both the ours-stale and force paths.

- [ ] **Step 4: Run test** — expect PASS.

- [ ] **Step 5: Commit** `feat(hooks): back up overwritten hook to .bak`.

---

### Task 3: `project_root` arg on `get_snapshot` MCP tool

**Files:**
- Modify: `src/server.rs` (handle_get_snapshot, tool registration in process_mcp_request)
- Modify: MCP tool schema (tools/list) to add optional `project_root`

**Interfaces:**
- Consumes: nothing new.
- Produces: `handle_get_snapshot(namespace, domain, include_archived, project_root: Option<String>)`.

- [ ] **Step 1: Write failing test** that calling `get_snapshot` with `project_root` returns without error and the value is echoed/used (smoke).

- [ ] **Step 2: Run test** — expect FAIL.

- [ ] **Step 3: Implement** arg parsing + plumbing through to handle_get_snapshot; cache the value into registry_row when present.

- [ ] **Step 4: Run test** — expect PASS.

- [ ] **Step 5: Commit** `feat(snapshot): accept optional project_root arg`.

---

### Task 4: Hook hygiene inside `get_snapshot`

**Files:**
- Modify: `src/server.rs` (`handle_get_snapshot`) — add hook hygiene block
- Modify: `src/task/mod.rs` — reuse `git_hooks_dir`

**Interfaces:**
- Consumes: `project_root` (Task 3), `is_ours_hook`/`is_current_hook`/`hook_sha256` (Task 1), `SCAFFOLDED_PRE_PUSH_HOOK`.
- Produces: appends a status line to snapshot output when action taken or foreign hook detected.

- [ ] **Step 1: Write failing test** (integration-ish, in-process) that with a temp git repo lacking a hook, `get_snapshot` creates it and reports `HOOKS: auto-installed`.

- [ ] **Step 2: Run test** — expect FAIL.

- [ ] **Step 3: Implement** hygiene block per spec §4.3, gated on `project_root` being `Some` and `.git` existing.

- [ ] **Step 4: Run test** — expect PASS.

- [ ] **Step 5: Commit** `feat(snapshot): auto-heal pre-push hook on get_snapshot`.

---

### Task 5: Doctor scheduling via registry state

**Files:**
- Modify: `src/server.rs` (`handle_get_snapshot`) — increment session_counter, decide trigger, run in-memory doctor pass
- Modify: `src/registry.rs` (or wherever registry_row is read/written) — add `doctor_state` to metadata

**Interfaces:**
- Consumes: all memories already loaded by `get_snapshot`.
- Produces: `DOCTOR REPORT` block (only when findings) + updated `doctor_state`.

- [ ] **Step 1: Write failing test** that a version mismatch triggers a doctor pass and updates `last_run_version`.

- [ ] **Step 2: Run test** — expect FAIL.

- [ ] **Step 3: Implement** trigger logic + in-memory checks (unqualified AST ids, dangling edges, case-colliding namespaces) + registry_row update.

- [ ] **Step 4: Run test** — expect PASS.

- [ ] **Step 5: Commit** `feat(snapshot): throttled in-memory doctor pass`.

---

### Task 6: CLI fallback hook-hash check

**Files:**
- Modify: `src/main.rs` (`task gate`, `status`, `doctor` dispatch) — run hook hygiene against CWD git root when daemon is down.

**Interfaces:**
- Consumes: Task 1 helpers, `git_hooks_dir`.

- [ ] **Step 1: Write failing test** that `task gate` on a repo with a stale hook refreshes it (or at least reports).

- [ ] **Step 2: Run test** — expect FAIL.

- [ ] **Step 3: Implement** fallback check in the daemon-down path.

- [ ] **Step 4: Run test** — expect PASS.

- [ ] **Step 5: Commit** `feat(cli): fallback hook-hash check when daemon down`.

---

### Task 7: End-to-end verification

- [ ] **Step 1: Build** (`cargo build`) — no new warnings/errors.
- [ ] **Step 2: Run full test suite** (`cargo test`) — all pass.
- [ ] **Step 3: Manual smoke**: fresh temp git repo, run `get_snapshot` via MCP, confirm hook auto-installed + status line; re-run, confirm silent; tamper hook hash, confirm refresh + `.bak`.
- [ ] **Step 4: Commit any test fixes**, push.

---

## Self-Review Notes

- Spec coverage: every acceptance criterion (§6) maps to a task (1-6) plus verification (7). ✓
- Type consistency: `hook_sha256() -> String`, `is_ours_hook(&str) -> bool`, `is_current_hook(&str) -> bool` used consistently. ✓
- Review Focus: all five lines have corresponding tests (Tasks 2, 3, 4, 5, 6). ✓
- Proportion: plan is ~2x spec length; acceptable for 7 tasks. ✓