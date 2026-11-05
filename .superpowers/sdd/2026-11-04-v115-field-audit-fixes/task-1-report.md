# Task 1 Report — Shared `@neurostrata/plugin-core` + Thin Harness Adapters

**Task:** `v115-field-audit-fixes-noc7` (namespace `v115-field-audit-fixes`)
**Branch:** `opencode/v115-field-audit-fixes`
**Status:** Complete — all 5 test suites pass, committed, pushed.

## Summary

Implemented the shared ESM `@neurostrata/plugin-core` (`plugins/core/index.ts`, zero `require()`, Node built-ins only) and thin harness adapters for opencode, claude, codex, and github-cli. Each adapter re-exports the compiled core dist and exposes `createHarnessPlugin`. Covers Bug 2, Bug 3, Bug 4, Bug 5, and Enhancements 1, 2, 3, 5.

## Scope delivered

- `plugins/core/index.ts` — full ESM core implementation (~1256 lines).
- `plugins/core/test.mjs` — comprehensive suite (Bugs 2-5, Enh 1/2/3/5).
- `plugins/{opencode,claude,codex,github-cli}/index.ts` — thin wrappers (`export *` + explicit `createHarnessPlugin` import).
- `plugins/{opencode,claude,codex,github-cli}/cli-hook.mjs` — hook entry points with `handleHookEvent ?? handleClaudeHookEvent ?? plugin` chain.
- `.gitignore` — removed `plugins/opencode/dist/` line so adapter dists are tracked.

## Test results (all pass, run from repo root)

| Suite | Pass | Fail |
|-------|------|------|
| plugins/core/test.mjs | 46 | 0 |
| plugins/opencode/test.mjs | 13 | 0 |
| plugins/claude/test.mjs | 11 | 0 |
| plugins/codex/test.mjs | 11 | 0 |
| plugins/github-cli/test.mjs | 11 | 0 |

("NeuroStrata core is unreachable ... fetch failed" lines are expected fail-open warnings from the unreachable-daemon test.)

## Root causes & fixes

1. **Mutating-guard test (core)** — `handleHookEvent` only runs the `neurostrata_guard_validate` fail-closed guard when `config.strictMode` is true. The test set `strictMode: false`, so the guard never ran and `continue` was `true` instead of `undefined`/`deny`. Fixed: `strictMode: true` with the mock's `guardReject: DeterministicReject`.
2. **Enrich-recall test (core)** — asserted `[NeuroStrata rule mem-1]` but the mock memory id is `mem-rule-1`, so the actual snippet is `[NeuroStrata rule mem-rule-1 ...]`. Fixed assertion to match the real id. (Not state pollution; clean state injects the snippet correctly.)
3. **Adapter dist import path** — TypeScript preserves relative import paths verbatim. Source `index.ts` (at `plugins/<adapter>/`) correctly imports `../core/dist/index.js`, but the compiled `dist/index.js` (at `plugins/<adapter>/dist/`) must import `../../core/dist/index.js`. Post-processed all 4 compiled adapter `dist/index.js` + `dist/index.d.ts` to `../../core/dist/index.js`. Source `.ts` files keep `../core/dist/index.js` (type-checks from adapter dir).
4. **Adapter `injectSubagentPrompt` idempotency test** — asserted marker count `=== 1`, but `buildSubagentContextHeader` embeds `### NeuroStrata Task & Graph Context` twice (title line + body reference in the mandatory graph-first rule). Fixed to assert count stability across repeated injection (matching the core suite's existing pattern).

## Cleanup

- Removed `plugins/core/node_modules` and `plugins/core/package-lock.json` (build artifacts, not committed).

## Commit

Committed all Task 1 files on `opencode/v115-field-audit-fixes` and pushed (pre-push strict task gate passed after `neurostrata_task_complete` + extracted memory `7664bf40-e369-46e5-ace8-73b655aa2f1f`).
