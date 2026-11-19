# Staged Format and Lint Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Block commits whose staged supported-language files fail formatting or lint checks without modifying the worktree or index.

**Architecture:** A standalone staged-file quality runner owns language routing and local-tool execution. Clippy emits JSON and a Node filter rejects only diagnostics rooted in staged Rust paths, avoiding the existing 548-warning global baseline. The generated pre-commit hook delegates to the runner while preserving ledger behavior and byte-identical checked-in/embedded hook sources.

**Tech Stack:** Bash, Git, Rust/rustfmt/Clippy, Node built-in test, Prettier, ESLint, ShellCheck.

**Spec:** `docs/superpowers/specs/2026-11-18-staged-format-lint-design.md`

## Global Constraints

- Open exactly one new PR containing both Kani integration and all staged formatting/lint gates after all tasks and verification finish; multiple local commits on the existing branch are allowed.
- Only staged added/copied/modified/renamed paths are checked; deletions are excluded.
- Never rewrite source, stage changes, download packages, invoke `npx`, or introduce Python tooling.
- Missing tools required by staged files fail nonzero with a setup diagnostic.
- `SCAFFOLDED_PRE_COMMIT_HOOK` and `scripts/hooks/pre-commit` stay byte-identical.
- Formatting and Clippy lint migration are incremental by staged file.

## Review Focus

- Spaces/non-ASCII names must remain one argument: Task 1 Step 1 creates `src/a b.rs` and `web-ui/src/Ä x.ts` and asserts argument boundaries in fake-tool logs.
- Unstaged-only, deleted, and generated paths must not trigger tools: Task 1 Step 1 stages fixtures independently and asserts empty invocation logs.
- Missing local dependencies must not trigger network installs: Task 1 Step 1 omits fake binaries and asserts setup diagnostics plus nonzero status.
- Unrelated Clippy warnings must not block staged-file enforcement: Task 1 Step 3 tests mixed staged/unstaged primary spans and absolute/relative path spellings.
- Worktree/index integrity must survive successful and failed checks: Task 1 Step 1 hashes both before and after each fixture.

---

### Task 1: Staged Quality Runner and Local Formatter

**Files:**

- Create: `scripts/check-staged-quality.sh`
- Create: `scripts/clippy-staged-diagnostics.mjs`
- Create: `scripts/test-clippy-staged-diagnostics.mjs`
- Create: `scripts/test-staged-quality.sh`
- Create: `package.json`
- Create: `package-lock.json`
- Create: `.prettierrc.json`
- Create: `.prettierignore`
- Modify: `.gitignore`

**Interfaces:**

- Consumes: Git staged paths, Clippy JSON, and repository-local tool installations.
- Produces: `scripts/check-staged-quality.sh` exits zero only when all relevant checks pass and never writes project files or the index.
- Produces: `node scripts/clippy-staged-diagnostics.mjs --root <root> --staged <repo-relative-path> [...]` reads Clippy JSON on stdin, prints matched diagnostics, and exits `1` for a staged warning/error, `2` for malformed input, otherwise `0`.

- [ ] **Step 1: Write failing staged-routing integration tests**

Create isolated repositories and fake `rustfmt`, `cargo`, `node`, `shellcheck`, Prettier, and ESLint tools. Assert the staged routing table in the spec, Rust/Cargo Clippy selection, `skip_children=true`, no generated-bundle formatting, exact whitespace-safe arguments, deletion/internal-file filtering, missing-tool diagnostics, propagated failures, and unchanged file/index hashes. Fake cargo emits Clippy JSON fixtures so unrelated warnings pass and staged warnings fail.

- [ ] **Step 2: Run tests to verify they fail**

Run: `bash scripts/test-staged-quality.sh`
Expected: FAIL because `scripts/check-staged-quality.sh` does not exist.

- [ ] **Step 3: Write failing Clippy diagnostic-filter tests**

Use Node built-in tests for warning and error primary spans, non-primary spans, normal cargo messages, malformed JSON, absolute paths, `./`-relative paths, Cargo manifests, and unrelated baseline warnings. Pin the exit codes from the Interfaces block.

- [ ] **Step 4: Run filter tests to verify they fail**

Run: `node --test scripts/test-clippy-staged-diagnostics.mjs`
Expected: FAIL because the filter module does not exist.

- [ ] **Step 5: Implement the Clippy filter and staged-quality runner**

Export a pure `findStagedDiagnostics(messages, root, stagedPaths)` function plus a CLI entry point. Normalize path separators without filesystem access, match primary `warning`/`error` spans only, and preserve each matching diagnostic's `rendered` text. In the runner, use NUL-delimited Git output and Bash arrays; capture both cargo and Node `PIPESTATUS` values so either failure blocks. Resolve Prettier/ESLint only through local paths and emit local setup diagnostics when absent.

- [ ] **Step 6: Add pinned local formatting configuration**

Declare a private root package with exact Prettier dependency, add minimal Prettier settings/ignores, generate `package-lock.json` using repository-local `npm install --package-lock-only`, and add `/node_modules/` to `.gitignore`.

- [ ] **Step 7: Run focused checks**

Run: `node --test scripts/test-clippy-staged-diagnostics.mjs`
Run: `bash scripts/test-staged-quality.sh`
Run: `bash -n scripts/check-staged-quality.sh scripts/test-staged-quality.sh`
Run: `shellcheck scripts/check-staged-quality.sh scripts/test-staged-quality.sh`
Expected: all commands exit zero.

- [ ] **Step 8: Commit after explicit owner approval**

```bash
git add scripts/check-staged-quality.sh scripts/clippy-staged-diagnostics.mjs scripts/test-clippy-staged-diagnostics.mjs scripts/test-staged-quality.sh package.json package-lock.json .prettierrc.json .prettierignore .gitignore docs/superpowers/specs/2026-11-18-staged-format-lint-design.md docs/superpowers/plans/2026-11-18-staged-format-lint.md
git commit -m "feat: add staged formatting and lint checks"
```

### Task 2: Hook Wiring and Documentation

**Files:**

- Modify: `scripts/hooks/pre-commit`
- Modify: `src/task/mod.rs` embedded hook and tests
- Modify: `CONTRIBUTING.md`
- Modify: `AGENTS.md`
- Execute: `scripts/install-hooks.sh`

**Interfaces:**

- Consumes: `scripts/check-staged-quality.sh` from Task 1.
- Produces: generated pre-commit fails when quality checks fail, and active `.git/hooks/pre-commit` contains identical delegation.

- [ ] **Step 1: Write failing hook contract tests**

Add `pre_commit_hook_sources_stay_byte_identical` in `src/task/mod.rs` using `include_str!`. Extend `scripts/test-staged-quality.sh` with a failing quality-tool fixture asserting nonzero pre-commit status and preservation of graph-ledger behavior.

- [ ] **Step 2: Run tests to verify they fail**

Run: `TMPDIR=/var/tmp/ns-tmp cargo test --bin neurostrata-mcp pre_commit_hook_sources_stay_byte_identical -- --nocapture`
Run: `bash scripts/test-staged-quality.sh`
Expected: the propagation test FAILs because the hook does not delegate yet.

- [ ] **Step 3: Add identical hook delegation in both sources**

After staged-path collection, invoke `bash "$ROOT/scripts/check-staged-quality.sh"` only when the file exists, returning nonzero immediately. Preserve ledger behavior. Resolve existing ShellCheck informational findings without broadening intentional ASCII-only case folding. Because the embedded hook change makes `src/task/mod.rs` staged, normalize that file with rustfmt and clear its 15 current Clippy diagnostics without lint suppression or unrelated-file edits; keep the cleanup behavior-preserving. Prove the real runner accepts the staged file after cleanup.

- [ ] **Step 4: Document setup and enforcement**

Document `npm ci`, `npm --prefix web-ui ci`, Node/Rust components and ShellCheck, repair commands, incremental staged-file policy, no automatic restaging, and hook installation. Add the quality gate to `AGENTS.md` Section 2.

- [ ] **Step 5: Install and verify the hook**

Run: `bash scripts/install-hooks.sh`
Run: `cmp scripts/hooks/pre-commit .git/hooks/pre-commit`
Run: `TMPDIR=/var/tmp/ns-tmp cargo test --bin neurostrata-mcp pre_commit -- --nocapture`
Run: `bash scripts/test-staged-quality.sh`
Expected: all commands exit zero and files are byte-identical.

- [ ] **Step 6: Run final verification**

Run: `node --test scripts/test-clippy-staged-diagnostics.mjs`
Run: `./node_modules/.bin/prettier --check` on every added/modified supported non-Rust file
Run: `TMPDIR=/var/tmp/ns-tmp cargo test --locked -- --test-threads=1`
Run: `TMPDIR=/var/tmp/ns-tmp bash scripts/task-coverage-gate.sh`
Expected: all commands exit zero.

- [ ] **Step 7: Commit after explicit owner approval**

```bash
git add scripts/hooks/pre-commit src/task/mod.rs CONTRIBUTING.md AGENTS.md scripts/test-staged-quality.sh
git commit -m "feat: enforce staged formatting and linting on commit"
```
