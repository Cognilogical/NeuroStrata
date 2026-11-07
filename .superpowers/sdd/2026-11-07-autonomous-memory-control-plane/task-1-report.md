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
