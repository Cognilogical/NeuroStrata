# Design: Automatic Hook Discovery & Doctor Scheduling for NeuroStrata Agents

**Date:** 2026-10-26
**Status:** Draft (pending user review)
**Approach:** C — `get_snapshot` hygiene strap + registry state + safe auto-heal (recommended by gemini-arch / Argon)

## 1. Problem

Git hooks (the pre-push task gate) and `doctor` (upgrade-consistency report) are
both manual today:

- `neurostrata-mcp hooks install` must be run per project by a human or an
  agent that already knows to run it. On upgrade, already-installed hooks are
  not refreshed, and foreign hooks are never detected.
- `neurostrata-mcp doctor` is never run by agents unless explicitly asked. The
  owner had to tell the agent to run it this session.

Both should be **agent-discovered**: the agent learns about missing/stale hooks
and stale state through its normal first action (`get_snapshot`), and heals or
reports without a human prompt, while never destroying user data.

## 2. Goals

1. An agent's first `get_snapshot` on a project with a missing or stale
   NeuroStrata `pre-push` hook auto-installs/updates it (idempotent, backed
   up), with one status line.
2. A foreign `pre-push` hook is never overwritten; the agent is told how to
   resolve it.
3. `doctor` runs on a throttled, event-driven cadence inside `get_snapshot`
   (zero extra DB queries), silently when clean, reporting only when issues
   exist.
4. Both behaviors work daemon-up and daemon-down.
5. Nothing leaves the machine: no mesh sync, no network.

## 3. Non-Goals

- No new MCP tools. The hygiene strap piggybacks on `get_snapshot`.
- No changes to the task gate's blocking semantics.
- No auto-install of the pre-commit hook (remains opt-in via
  `hooks install-pre-commit`).
- No remote/central hook management.

## 4. Architecture

### 4.1 Hook scaffold hash marker

`SCAFFOLDED_PRE_PUSH_HOOK` (in `src/task/`) gains a trailing comment line:

```
# neurostrata-hook-sha256: <blake3 of the entire scaffold bytes>
```

`install_pre_push_hook` (src/main.rs) already compares content for the
`"NeuroStrata task gate"` marker to decide whether a hook is ours. The hash
extends this: a hook is *ours-and-current* iff it contains the marker **and**
its embedded hash equals the current scaffold hash. A mismatch means stale
(upgrade) and triggers auto-refresh.

Before overwriting *any* existing hook file (ours-stale or foreign with
`--force`), copy it to `<target>.bak` (overwrite allowed) so the operator can
recover.

### 4.2 `project_root` resolution for the daemon

The daemon's CWD is not the repo root, so `get_snapshot` cannot assume
`std::env::current_dir()`. Resolution order:

1. New optional `project_root: Option<String>` arg on `neurostrata_get_snapshot`
   (MCP tool + daemon `/mcp` handler). Agents pass it when they know it.
2. Cached `project_root` in the namespace's private `registry_row` metadata
   (populated by `ingest_directory`, `task_setup`, and `append_log`).
3. If neither is available, skip hook hygiene entirely (no error) and fall
   back to CLI-side checks in `task gate` / `status` / `doctor`.

### 4.3 Hook hygiene inside `get_snapshot`

When `project_root` is known and `<root>/.git` exists:

1. Compute `hooks_dir = git_hooks_dir(root)` (honors `core.hooksPath`).
2. Read `hooks_dir/pre-push` if present.
3. **Missing file** → write scaffold, chmod +x, emit
   `HOOKS: auto-installed pre-push (v{version})`.
4. **File present, contains marker, hash matches** → no-op (silent).
5. **File present, contains marker, hash differs** → copy to `.bak`, write
   scaffold, emit `HOOKS: updated pre-push (v{old} -> v{new})`.
6. **File present, no marker (foreign)** → leave untouched, emit
   `HOOKS WARNING: foreign pre-push hook at <path>; inspect or run 'neurostrata-mcp hooks install --force'`.

The check is idempotent and runs at most once per `get_snapshot` call. It
never touches a hook without the marker unless `--force` is given (CLI only).

### 4.4 Doctor scheduling (registry state)

Extend the private `registry_row` metadata with:

```json
{
  "doctor_state": {
    "last_run_version": "1.13.0",
    "last_run_ts": 1730000000,
    "session_counter": 42
  }
}
```

`get_snapshot` increments `session_counter` on every call (cheap, in-memory
counter persisted on the row). A doctor pass runs **iff** any of:

- `last_run_version != env!("CARGO_PKG_VERSION")` (upgrade detected), or
- `session_counter % 20 == 0`, or
- `now - last_run_ts > 7 * 86400`.

The pass is **in-memory**: `get_snapshot` already loads all memories for the
namespace, so the doctor checks (unqualified AST ids, dangling edges,
case-colliding namespaces, unqualified ingested nodes) reuse that scan — zero
extra DB queries. Results:

- **0 issues** → update `doctor_state` silently (no output spam).
- **>0 issues** → prepend a `DOCTOR REPORT` block to the snapshot output,
  listing each finding with the existing per-namespace labels. Update
  `doctor_state`.

Because the registry row is `eligibility: private`, none of this state is
synced to the SynapticMesh.

### 4.5 CLI parity

- `neurostrata-mcp doctor` keeps using `ReadSource` (daemon `/cli/read` when
  up, direct store when down) — unchanged.
- `neurostrata-mcp task gate` and `status` (which run with daemon down)
  perform the same hook-hash check against the CWD git root as a fallback, so
  a fresh clone with no daemon still self-heals on first gate run.

## 5. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Daemon CWD ≠ repo root | `project_root` arg + cached registry value; skip hook hygiene if unknown (no error). |
| Clobbering user hooks (Husky/Lefthook) | Strict `"NeuroStrata task gate"` marker check + `.bak` backup before any overwrite; foreign hooks never touched without `--force`. |
| Mesh pollution | `doctor_state` + `project_root` live in private `registry_row`, never exported. |
| Token spam | Silent when clean; `DOCTOR REPORT` only on findings; hook status is one line. |
| Stale `project_root` after repo move | Re-populated by next `ingest_directory`/`task_setup`/`append_log`; arg overrides cache. |

## 6. Checkable Acceptance Criteria

1. `SCAFFOLDED_PRE_PUSH_HOOK` contains `# neurostrata-hook-sha256: <hash>`;
   `hooks install` backs up any overwritten file to `<target>.bak`.
2. `get_snapshot` on a git repo with a missing or stale-hash NeuroStrata
   `pre-push` hook auto-installs/updates it and emits exactly one status line.
3. `get_snapshot` on a repo with a foreign `pre-push` hook leaves the file
   untouched and emits a `HOOKS WARNING` line.
4. Binary version bump or 20-snapshot cadence triggers an in-memory doctor
   pass on `get_snapshot`: silent if clean, `DOCTOR REPORT` if issues, and
   updates `registry_row.doctor_state`.
5. `neurostrata-mcp doctor` exits 0 both daemon-up (`ReadSource::Daemon`) and
   daemon-down (`ReadSource::Direct`).

## 7. Open Questions

- Should the hook-hash check also cover the pre-commit hook, or stay
  pre-push only (recommended: pre-push only, matching current scope)?
- Should `doctor_state` be reset on `neurostrata-mcp hooks install --force`
  (recommended: yes, force a fresh pass)?

## 8. Next Step

On approval of this spec, invoke `writing-plans` to produce the
implementation plan. No code changes until the plan is approved.