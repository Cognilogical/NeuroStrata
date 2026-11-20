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

`isZeroActionExemptShellPayload` (exported from `plugins/core`) keeps the Breath mutation counter from advancing on provably read-only / verification shell commands only. It accepts `bash`/`shell`/`command`/`exec` tools, unwraps JSON `{command, cmd}` payloads, and uses a fail-closed allowlist: read-only git subcommands (`status`, `log`, `diff`, `show`, `rev-parse`, `fetch`, `stash list|show`, read-only `branch` flags), `neurostrata-mcp task|doc-gate|synaptic-mesh status`, the `cargo` check/test family, read-only `gh` subcommands and `gh api` without write flags, `node --test` / `*test.mjs`, gate-receipt scripts, and `rustc --version`. The command is tokenized with a quotes-aware shell-word parser (C0/NEL/BOM separators, unbalanced quotes fail closed); substitutions, redirection, backgrounding (a `&` that is not part of `&&`), and chains containing any non-exempt segment fail closed. `git push` is mutating when any arg is `+<refspec>`, `--force*`, `--force-with-lease[=<ref>]`, `--mirror`, `--delete`, `--prune`, `--no-verify`, or a short bundle containing `f`/`d` (value-less). The daemon (`src/server.rs`) and the plugin share one grammar and agree on every row of the adjudicated `scripts/classifier-corpus.json`; the daemon is the authority over mutability — a caller-declared `is_mutating: true` on an otherwise-exempt shell stays gating. The `neurostrata_guard_validate` call itself is never skipped for an exempt command.

`neurostrata-nb9q`: both hook call sites (`executeBeforeHandler` and `handleHookEvent`) pass `is_mutating: !shellExempt` to `neurostrata_guard_validate`, so read-only commands — including `git push`/`git pull` after `neurostrata_task_complete` — never trip the no-claimed-task gate (the Lock 2 / push Catch-22). Force-push flags (`-f`, `--force`, `--force-with-lease`) and backgrounded shells (`CMD &`) stay mutating; `&&` chains of read-only commands stay exempt.

`neurostrata-1e14`: the plugin and the daemon shell classifiers apply the same force-push rejection — any `-f` inside a short bundle (`-vf`/`-uf`), `--force`, `--force-with-lease[=<ref>]`, forced refspecs (`+main`), `--mirror`, `--delete` and `--prune` stay mutating; `--no-verify` and backgrounded push forms are mutating too. Both classifiers agree on every row of `scripts/classifier-corpus.json` (the two CI legs run every row), so a remote-history rewrite skips the active-task gate. Code Mode's `execute`/`mcp_execute` tool output is also excluded from task-intent intake.

`neurostrata-nb9q`: subagent graph-first injection is agent- and harness-aware: read-only subagents (`reviewer`, `checker`, `checker-free`, `explore`) receive a skip note instead of directives to call MCP tools they do not have, and Code Mode harnesses reference `execute` with `tools.neurostrata.*`. Intent extraction from `read`/`grep`/`glob`/shell tool output is suppressed, because `TODO:`/`defer` in stdout or source comments is not a task obligation; user and assistant turns still nudge.

## Implementing a New Harness Adapter

To add a new harness adapter:

1. Create a new entry under `plugins/<harness>/` mirroring the structure of `plugins/core/`
2. Call `createHarnessPlugin(pluginId, harnessName)` to obtain a plugin object
3. Register the plugin's hooks with the target harness using `handleHookEvent` as the handler
4. Ensure `resolveProjectRoot` and `discoverProjectNamespace` are wired into the harness's project-root discovery

The adapter must not hardcode the project name or namespace; both are discovered at runtime from the workspace filesystem.