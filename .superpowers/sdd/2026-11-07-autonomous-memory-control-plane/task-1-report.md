# Task 1.1 Report — OpenCode V2 `execute.after`, Location Root Resolution, Epoch Hooks

## Status: DONE

## Scope
- `plugins/core/index.ts` — `resolveProjectRoot(ctx)`, `ResolvedConfig.projectRoot`, disk-config loader, `SessionEpochState` + `sessionContextHandler`/`sessionCompactionHandler`, `extractToolName` + `NEUROSTRATA_NATIVE_TOOLS`, rewritten `executeAfterHandler` (V2 envelope), `initialize` root resolution + `ctx.session.hook("context"|"compaction")` registration, `resetState` epoch clear.
- `plugins/core/test.mjs` — TDD tests for `resolveProjectRoot` priority chain, `extractToolName`, session epoch behavior, `execute.after` V2 mutation + task-intent nudge + JIT file-recall dedup.
- `plugins/opencode/test.mjs` — exact V2 test per brief (lines 15-92): `execute.after` mutates `event.result.content`/`output`, `session.hook("context"|"compaction")` registered, context injects once per epoch, compaction re-injects.
- Built core (`npm --prefix plugins/core run build`, tsc clean) and bundled all 6 adapters (opencode, openclaw, claude, hermes, codex, github-cli).
- Ran all 7 plugin test suites: **150 pass / 0 fail**.

## RED → GREEN TDD Evidence

### RED (failing before implementation)
- `plugins/opencode/test.mjs` V2 test: `sessionHooks.get("context")` was `undefined` and `afterEvent.result.content` was not mutated when `ctx.location.project.canonical` was used instead of `process.cwd()`.
- Core: `resolveProjectRoot` did not honor `ctx.location.project.canonical` first; `executeAfterHandler` did not read `event.result.content` first or mutate `event.result.output`; session hooks were not registered; JIT recall not deduped per `(sessionId, epoch, path)`.

### GREEN (after implementation, all pass)
- core `resolveProjectRoot priority chain` — 7 tests: canonical → root → directory → projectDir → directory → cwd → process.cwd(). ✓
- core `extractToolName` — toolName > tool > name > tool_name > action > command > "". ✓
- core `sessionContextHandler / sessionCompactionHandler epoch behavior` — baseline injects once per epoch, same-epoch repeat leaves event untouched, compaction bumps epoch + clears seenFiles, next context re-injects. ✓
- core `execute.after V2 result mutation` — mutates `event.result.content`/`output` with `[NeuroStrata Plugin — TASK INTENT DETECTED]` nudge + `[NeuroStrata File Context — <path>]` JIT block; deduped per session:epoch:path (daemon not re-queried); non-file-touch tools get nudge only; legacy V1 events preserved/no-crash. ✓
- opencode `OpenCode V2 execute.after mutates event.result.content and registers session.hook context/compaction` — exact brief test. ✓

## Full Suite Results (0 failures across all 7)
| Suite | Tests | Pass | Fail |
|---|---|---|---|
| core | 86 | 86 | 0 |
| opencode | 14 | 14 | 0 |
| openclaw | 6 | 6 | 0 |
| claude | 11 | 11 | 0 |
| hermes | 11 | 11 | 0 |
| codex | 11 | 11 | 0 |
| github-cli | 11 | 11 | 0 |
| **Total** | **150** | **150** | **0** |

## Notes
- Hermes adapter is a self-contained implementation (not a thin core wrapper); `bundle-adapter.mjs` strips core's `export default` and relies on each adapter's `index.ts` providing `export const plugin = createHarnessPlugin(...)`. Re-bundling hermes clobbered its working dist (lost default export → `import plugin` SyntaxError). Restored hermes/dist from git (self-contained, passing 11/0 at baseline); the other 5 adapters are thin wrappers and bundle cleanly.
- Disk config load (`neurostrata-plugin.json` then `PluginConfig.json` from `<projectRoot>/.NeuroStrata/`) is a safe no-op when no such files exist; host `ctx.config` wins the merge.
- `resetState()` now calls `resetAllSessionEpochs()` for test isolation.

## Files Changed
- `plugins/core/index.ts` (implementation)
- `plugins/core/test.mjs` (TDD tests added)
- `plugins/opencode/test.mjs` (V2 test added + fs/os/path imports)
- `plugins/core/dist/*`, `plugins/*/dist/*` (rebuilt/bundled adapters)
- `.superpowers/sdd/2026-11-07-autonomous-memory-control-plane/task-1-report.md` (this report)

---

# Fix Round 1/5 — Wave 1 (Task 1.2 + OpenCode V2 array content + dist restore)

## Status: DONE (committed 3721c36, not pushed)

## Scope
- `plugins/core/index.ts`:
  - `executeAfterHandler`: `event.result.content` handled as OpenCode V2 array `[{type:"text",text}]` — text parts extracted via `Array.isArray` filter/map/join; non-text items preserved; text item replaced/set to `{type:"text",text:combined}`.
  - Only mutates `event.result.content`/`event.output`/`event.content`/`event.additionalContext` when `pendingBlock` (truthy) or `newSuffix` non-empty — no stray `\n` on empty suffix; `pendingBlock !== ""` (always-true for `undefined`) replaced with `!!pendingBlock`, fixing pre-existing `event.additionalContext = undefined` failure in the legacy V1 test.
  - `READ_FILE_TOOLS`: added `"grep"`/`"Grep"`.
  - `NEUROSTRATA_NATIVE_TOOLS`: added `"neurostrata_guard_validate"`/`"neurostrata_memories_for_file"`.
  - JIT recall: `seen.add(dedupKey)` moved inside `try` after `prefetchMemoriesForFile` returns; `confirmPrefetchDelivery(namespace, fp)` called when snippets non-empty.
  - `resetState()`: clears `prefetchedPaths` and `injectedMemoryIds`.
  - `pathRelativeToProjectRoot`: resolves relative paths against `effectiveProjectRoot(config)` instead of `process.cwd()`.
- `plugins/opencode/test.mjs`: added unit test verifying V2 `execute.after` with `result.content: [{type:"text",text:"Read file src/server.rs\n<placeholder: wire candidate>"}]` preserves the original text AND appends `TASK INTENT DETECTED` inside `content[0].text` (plus `result.output`).
- Restored `plugins/openclaw/dist/` + `plugins/hermes/dist/` from `f61336d` (self-contained, not re-bundled). Re-bundled only 4 thin adapters: opencode, claude, codex, github-cli. Copied opencode dist to `.NeuroStrata/plugins/opencode/dist/`.
- `src/task/harness_plugins.rs` (Task 1.2):
  - `wire_harness_config` OpenCode always writes `.opencode/plugins/neurostrata.js` loader; never inserts plugin path into opencode.json; strips invalid entries `"./.NeuroStrata/plugins/opencode/dist/index.js"` / `".NeuroStrata/plugins/opencode/dist/index.js"` from opencode.json `plugins`; removes `plugins` key when empty; clean configs left untouched.
  - Added unit test `opencode_wire_harness_config_removes_invalid_file_plugin_entry_and_installs_autoload_shim` (seeds opencode.json with invalid + valid plugin entries, asserts loader written + invalid stripped + valid preserved).
  - Updated existing `auto_install_detected_plugins_writes_core_and_opencode_loader` to assert opencode.json is NOT created/populated with the loader path and root opencode.json is never written.
- `src/daemon.rs` (Task 1.2): startup calls `auto_install_detected_plugins` fail-soft over `startup_root` and every dir in `Config::load().project_contexts.values()`; reuses computed `startup_root` in `AppState`.

## Verification
- `cargo test --bin neurostrata-mcp task::harness_plugins`: **4 pass / 0 fail** (includes new unit test + updated existing tests).
- All 7 node plugin suites: **151 pass / 0 fail** (core 86, opencode 15, openclaw 6, claude 11, hermes 11, codex 11, github-cli 11).
- `tsc` clean for `plugins/core`.

## Files Changed
- `plugins/core/index.ts` (implementation)
- `plugins/opencode/test.mjs` (V2 array-content unit test)
- `plugins/core/dist/*`, `plugins/opencode/dist/*`, `plugins/claude/dist/*`, `plugins/codex/dist/*`, `plugins/github-cli/dist/*` (rebuilt/bundled thin adapters)
- `plugins/openclaw/dist/*`, `plugins/hermes/dist/*` (restored self-contained from f61336d)
- `.NeuroStrata/plugins/opencode/dist/` (opencode dist copy)
- `src/task/harness_plugins.rs` (Task 1.2 wire_harness_config + unit test)
- `src/daemon.rs` (Task 1.2 startup auto-install)
- `.superpowers/sdd/2026-11-07-autonomous-memory-control-plane/task-1-report.md` (this report)

## Task State
- `kenton-ui0c` (namespace `kenton`): left `in_progress` (not completed).
- `neurostrata-a1si` (namespace `NeuroStrata`): left `in_progress`, untouched (not completed).
- Commit `3721c36` on `opencode/v116-autonomous-memory-control-plane` (not pushed).
