# Agent Harness Plugin Interface

## `@neurostrata/plugin-core` Architecture

`@neurostrata/plugin-core` is the zero-dependency harness plugin bundle shipped under `plugins/core/`. It provides a universal adapter that wraps the NeuroStrata daemon/MCP server and exposes hooks for agent harnesses:

- `createHarnessPlugin` — factory that builds a plugin instance for a specific harness
- `handleHookEvent` — dispatches hook events (PreToolUse, PostToolUse, SessionStart, etc.) to the daemon
- `resolveProjectRoot` — walks up from a working directory to find the project root
- `discoverProjectNamespace` — reads `.NeuroStrata/config.json` (or falls back to directory name) to discover the project namespace

The plugin is harness-agnostic: the same `createHarnessPlugin` function works for OpenCode, Claude, Hermes, and Codex because it only depends on the harness hook interface, not on harness internals.

## Session- & worktree-aware task state

`resolveActiveTaskFromDaemon` never picks the first `in_progress` task blindly. Among in-progress tasks it prefers, in strict order: an explicit `preferredTaskId`, a task matching both `activeSessionId` and the current `effectiveProjectRoot`, a task matching `activeSessionId`, a task matching the worktree, then any in-progress task. A fallback owned by a different session never overwrites a known `activeSessionId`, which prevents cross-session / cross-worktree Breath bleed between child worktrees.

`inspectCodeModeExecution` detects key presence (`id`, `session_id`, `note`) even when the value is a variable or template literal — for example `neurostrata_task_claim({ id: created.id, session_id: ses.id })` or a note built as a Breath template literal. Such a claim is marked as claimed but clears any stale `activeTaskId`, so the next check re-resolves the real id from the daemon using `activeSessionId`. Explicit `undefined`/`null` values are not treated as real keys.

`isZeroActionExemptShellPayload` (exported from `plugins/core`) keeps the Breath mutation counter from advancing on provably read-only / verification shell commands only. It accepts `bash`/`shell`/`command`/`exec` tools, unwraps JSON `{command, cmd}` payloads, and uses a fail-closed allowlist: read-only git subcommands (`status`, `log`, `diff`, `show`, `rev-parse`, `fetch`, `stash list|show`, read-only `branch` flags), `neurostrata-mcp task|doc-gate|synaptic-mesh status`, the `cargo` check/test family, read-only `gh` subcommands and `gh api` without write flags, `node --test` / `*test.mjs`, gate-receipt scripts, and `rustc --version`. Substitutions, redirection, and chains containing any non-exempt segment fail closed. The `neurostrata_guard_validate` call itself is never skipped for an exempt command.

`neurostrata-nb9q`: both hook call sites (`executeBeforeHandler` and `handleHookEvent`) pass `is_mutating: !shellExempt` to `neurostrata_guard_validate`. The daemon does not trust this caller-declared value: it recomputes the shell verdict from the raw payload through the shared kernel (`src/formal_kernel.rs`, `is_zero_action_exempt_shell_payload`), so an under-reporting plugin cannot bypass the active-task gate. Exempt verification commands — including `git push`/`git pull` after `neurostrata_task_complete` — therefore avoid the Lock 2 / push Catch-22 in both surfaces. Force-push flags (`-f`, `--force`, `--force-with-lease`) inside short bundles (`-vf`, `-uf`), forced refspecs (`+main`), `--mirror`, `--delete`, `--prune`, and backgrounded shells (`CMD &`) stay mutating; `&&` chains of read-only commands stay exempt. Both implementations are pinned to one checked-in verdict corpus (`tests/fixtures/shell-exemption-verdicts.json`) plus Lean parity, so the two verdicts cannot drift silently.

Both call sites forward the analysable tool input as the guard payload. A raw-string input — notably Codex `apply_patch` (`input: "*** Update File: …"`) — is sent as the patch text itself instead of the `{}` that `firstRecord` would produce, so the daemon's file-mutation extraction can see the target and content. Object inputs remain JSON-encoded; the mutation counter and the Zero-Action shell exemption keep using the canonical payload.

`neurostrata-1e14`: subagent context headers sanitize namespace and task labels to `[A-Za-z0-9_.:-]` before interpolation. Code Mode's `execute`/`mcp_execute` tool output is also excluded from task-intent intake.

`neurostrata-nb9q`: subagent graph-first injection is agent- and harness-aware: read-only subagents (`reviewer`, `checker`, `checker-free`, `explore`) receive a skip note instead of directives to call MCP tools they do not have, and Code Mode harnesses reference `execute` with `tools.neurostrata.*`. Intent extraction from `read`/`grep`/`glob`/shell tool output is suppressed, because `TODO:`/`defer` in stdout or source comments is not a task obligation; user and assistant turns still nudge.

## Implementing a New Harness Adapter

To add a new harness adapter:

1. Create a new entry under `plugins/<harness>/` mirroring the structure of `plugins/core/`
2. Call `createHarnessPlugin(pluginId, harnessName)` to obtain a plugin object
3. Register the plugin's hooks with the target harness using `handleHookEvent` as the handler
4. Ensure `resolveProjectRoot` and `discoverProjectNamespace` are wired into the harness's project-root discovery

The adapter must not hardcode the project name or namespace; both are discovered at runtime from the workspace filesystem.
