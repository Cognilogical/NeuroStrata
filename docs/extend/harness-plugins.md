# Agent Harness Plugin Interface

## `@neurostrata/plugin-core` Architecture

`@neurostrata/plugin-core` is the zero-dependency harness plugin bundle shipped under `plugins/core/`. It provides a universal adapter that wraps the NeuroStrata daemon/MCP server and exposes hooks for agent harnesses:

- `createHarnessPlugin` — factory that builds a plugin instance for a specific harness
- `handleHookEvent` — dispatches hook events (PreToolUse, PostToolUse, SessionStart, etc.) to the daemon
- `resolveProjectRoot` — walks up from a working directory to find the project root
- `discoverProjectNamespace` — the last step of the namespace order below: `.NeuroStrata/session_queries.json`, else the project folder's name
- `findRootMarker` — finds the project root marker for a folder, mirroring `neurostrata-mcp`'s walk (checked against the shared cases in `tests/fixtures/root-marker-cases.json`)

### Namespace order

`resolveConfig` uses the same order as `neurostrata-mcp namespace resolve`:

1. The project root marker, `<root>/.NeuroStrata/config.json` (`{"version": 1, "namespace": "..."}`), found from the project folder: the deepest marker in the folder's own checkout, or in its main checkout for a linked worktree; outside git, the first marker above the folder. A `.NeuroStrata` that is a link pointing elsewhere is not a marker.
2. The harness plugin config the installer writes, passed to the plugin as its config.
3. `NEUROSTRATA_NAMESPACE` from the environment.
4. `discoverProjectNamespace`.

The installer writes one config file per harness, each naming the namespace:

| Harness | Config file |
| --- | --- |
| Claude Code | `.NeuroStrata/plugins/claude/neurostrata.json` (`{"id": "neurostrata-claude", "namespace": "..."}`) |
| OpenCode | `.opencode/neurostrata-plugin.json` (`neurostrata.namespace`) |
| Codex | `.codex/plugins/neurostrata.json` |
| Hermes | `.hermes/plugins/neurostrata.json` |
| OpenClaw | `.openclaw/plugins/neurostrata.json` |
| GitHub CLI | `.NeuroStrata/plugins/github-cli/copilot-extension.json` |

Each CLI hook runner (`cli-hook.mjs`) reads its harness's config file (`readHarnessConfig`) and passes it to the plugin as config; the plugin reads `NEUROSTRATA_NAMESPACE` itself, in its own place in the order. The runner the installer writes to `<root>/.NeuroStrata/plugins/<harness>/` reads `<root>`'s config first, then the one at the project folder's checkout top (and its main checkout's, for a linked worktree). The Claude hooks (PreToolUse, SessionStart, Stop) run `node "<runner>"`, with no namespace in the command. An older install's `NEUROSTRATA_NAMESPACE=<ns> node "<runner>"` commands are repaired by the installer: the first namespace they name (SessionStart, then PreToolUse, then Stop) is written to Claude's config file and read back before the commands lose it; if that fails, those commands stay as they are and the install reply carries a warning. A fresh install whose config file cannot be written still installs the hooks, with the same warning.

The Claude runner resolves from the session's project folder, not the folder of each event (Claude Code starts a new hook process per event, and the event's `cwd` follows `cd`): `CLAUDE_PROJECT_DIR`, which Claude Code exports to hook processes as the project root where the session started; without it, the folder recorded for the session at SessionStart (`claude_session_dirs` in `.NeuroStrata/active_session.json`). A session started inside a split subfolder with its own marker therefore uses that subfolder's namespace.

Without `.git` above the folder, `findProjectRoot` takes the nearest folder holding a valid marker, else the nearest folder holding `.NeuroStrata`, but never the home folder: `~/.NeuroStrata` holds the user's own state, not a project's.

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
