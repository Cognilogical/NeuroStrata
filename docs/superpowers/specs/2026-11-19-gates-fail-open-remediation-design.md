# Design — Fail-Open Gate Remediation (issues #310–#324)

- **Date:** 2026-11-19
- **Status:** in-chat design approved by owner 2026-11-19; written spec pending owner review
- **Branch:** `fix/gates-fail-open-310-324` (isolated worktree, base `f768810a` / v1.19.0)
- **Tracking:** `neurostrata-hano` (umbrella); `gpy5`, `bgpx`, `48t7`, `vyga` (workstreams)

## 1. Intent

Post-release audit filed 15 issues (#310–#324) whose common root is that security
and quality gates are advisory rather than enforced: hook runners fall through to
`{"continue":true}`, push classifiers miss git's alternative delete/force
spellings, the daemon discards the plugin's mutability verdict, session claims
write to ambient ancestors, and staged/CI gates skip their checks on valid input
variations. The intended outcome is one coherent fix set on one local branch,
closing all 15 issues with regression evidence and a green gate suite.

**Success criteria**

1. All 15 issues have a code or documentation fix and a regression test or
   source-verified doc assertion.
2. Plugin and daemon push-exemption predicates return identical verdicts on a
   shared checked-in corpus.
3. Every gate named in the issues fails closed on the failure modes described.
4. All verification commands in section 7 pass.
5. No pull request is opened until the owner explicitly authorizes it after
   section 7 and independent review are complete.

## 2. Owner constraints (non-negotiable)

- **No PR until 100%.** Implementation, docs, focused tests, local verification,
  and independent pre-publication review must all be complete with zero Critical
  or Important findings (owner rule `019bcf9a`, reiterated 2026-11-19). A draft PR
  is not a workaround.
- **Keep `NEUROSTRATA_SKIP_GATE=1` as an operational escape hatch** for a wedged
  or unavailable database (AGENTS.md §2), but remove its advertisement from the
  hook's blocked message and emit an auditable stderr line when used.
- **Free-model subagents only.** Pin `opencode-go/beam-free` or
  `opencode-go/longcat-2.5-free`; on rate limit fall back to
  `openrouter-free/openrouter-free-router`. Never use a frontier seat for
  delegation on this branch.
- **No deferred state** (Prime Directive §0): every issue is fixed in this
  branch or the spec is amended with an owner-approved retirement. No stubs.

## 3. Source basis (verified against branch `f768810a`)

| Workstream | Issue → source site                                                                                                                                                                                                     |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A          | #313 `plugins/hermes/index.ts:604-798,859-863`, `dist/index.js:639-643`, `src/task/harness_plugins.rs:32-91`                                                                                                            |
| A          | #310 `plugins/openclaw/index.ts:805-840` (`ctx.hooks` at 826), `harness_plugins.rs:65-68`                                                                                                                               |
| A          | #311 `plugins/core/index.ts:2119-2129,2161-2314` (`extractToolName`, `executeAfterHandler`), reminder 2210-2219                                                                                                         |
| A          | #314 `plugins/core/index.ts:872-963` (push arm 909-919), `src/server.rs:11971-12088` (push arm 12016-12022)                                                                                                             |
| A          | #315 `src/server.rs:12090-12137` (`&` split 12129, `gh api` 12064-12072), `handle_guard_validate` 12141-12185; `is_mutating` sent at `plugins/core/index.ts:2459,2715`, read 0 times in `server.rs`                     |
| A          | #316 `plugins/core/index.ts:1491-1517` (raw interpolation 1512), standalone copies `plugins/hermes/index.ts:551-561`, `plugins/openclaw/index.ts:759-769`, all six dists                                                |
| A          | #317 `src/task/mod.rs:3228-3283` (`SCAFFOLDED_PRE_PUSH_HOOK`), hash marker 3282, tests 5070-5080 and 5820-5912, `src/main.rs:3615-3750`                                                                                 |
| B          | #312 `scripts/check-staged-quality.sh:57-65`; fixture harness `scripts/test-staged-quality.sh:945-981`                                                                                                                  |
| B          | #318 `.github/workflows/ci.yml:372` (`ACMR`), local counterpart `check-staged-quality.sh:25` (`ACMRT -z`)                                                                                                               |
| B          | #319 `scripts/check-staged-quality.sh:208-216`; ESLint fixtures `test-staged-quality.sh:560-650`                                                                                                                        |
| B          | #321 `scripts/check-staged-quality.sh:18-25` (unchecked process substitution)                                                                                                                                           |
| C          | #322 `src/task/mod.rs:1061-1077` (`resolve_session_root_dir`), `:1493-1511`, `:1734-1739`; failing test `:6613-6713`; callers `src/server.rs:5520,5532`                                                                 |
| C          | #323.1 `src/server.rs:3605-3616,3813,15137-15271` (global counter, parallel-only failures)                                                                                                                              |
| C          | #323.2 `plugins/core/test.mjs:334-337,1441-1458,1912-1925` (cwd-coupled fixtures)                                                                                                                                       |
| C          | #323.3 `scripts/test-staged-quality.sh:1122,1166,1168` (BSD `sed -i`)                                                                                                                                                   |
| C          | #323.4 `shellcheck scripts/*.sh scripts/hooks/pre-commit` → 4 warnings, 28 info                                                                                                                                         |
| D          | #324 `docs/features-api/mcp-and-http-api.md:19,20,29,43,44,50,65,71,76,81`; `README.md:141,150`; `docs/vision/product-vision.md:9,11-18`; `docs/extend/harness-plugins.md:20-26`; `tests/lean_parity_test.rs:1262-1263` |

Confirmed divergences (root cause of #314/#315): plugin guards lone `&`, daemon
does not; plugin `hasFlag` matches attached `gh api` flags, daemon matches exact
tokens; plugin sends `is_mutating`, daemon never reads it; both miss
`-d/-vd/:refspec/--dele/quoted force/--no-verify` and both false-positive
`-oformat=terse`.

## 4. Approach selection

| Approach                                                     | Assessment                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **1. Shared deterministic core + thin consumers (selected)** | Factor the push-exemption and session-root decisions into `src/formal_kernel.rs` with Lean proofs and parity rows; the TS implementation becomes an option-aware parser validated against one checked-in verdict corpus shared by Node and Rust test jobs. Larger diff and Lean work, but removes the duplication that caused this audit and satisfies GOLDEN rule `b0813917`. |
| 2. Patch each call site independently                        | Smaller diff, leaves duplicated predicates and the same drift vector; likely re-found by the next audit. Rejected.                                                                                                                                                                                                                                                             |
| 3. Rewrite the guard layer                                   | Out of scope for a remediation branch. Rejected.                                                                                                                                                                                                                                                                                                                               |

## 5. Design

### Section A — security / hook runners (issue group A)

**A1. Push exemption (shared core).** Add a pure decision
`evaluate_shell_exemption(payload) -> bool` in `src/formal_kernel.rs`, with a
zero-`sorry`/`axiom` Lean proof and parity rows in `tests/lean_parity_test.rs`.
`src/server.rs::is_zero_action_exempt_shell_payload` delegates to it.
`plugins/core/index.ts::isZeroActionExemptShellSegment` becomes an option-aware
parser (strip matching quotes; short bundle `d|f` only before `=`; empty-source
`:refspec`; unambiguous long-option prefixes such as `--dele*` → `--delete`;
`--no-verify` always mutating). Both implementations are asserted against one
checked-in JSON verdict corpus (`tests/fixtures/shell-exemption-verdicts.json`)
covering every spelling in #314/#315 plus the `-oformat` false positive.

**A2. `is_mutating` contract.** The daemon deliberately recomputes mutability
from the raw payload and never trusts the client flag; this is a security
property, not a bug. The plugin keeps sending the field for diagnostics, and
`docs/extend/harness-plugins.md` states the contract. No daemon trust is added.

**A3. Hook runners.** Hermes: factor the `execute.before` closure body into a
named `runBeforeTool(input)`, export `handleHookEvent` plus the legacy alias, and
rebuild `plugins/hermes/dist`. A missing handler for a mutating PreToolUse is a
**deny**, never `{"continue":true}`. OpenClaw: optional-chain `ctx.hooks` /
`ctx.logger` and dispatch through `handleHookEvent` so the guard actually runs.
Rebuild every affected dist bundle; committed bundles must match a fresh build.

**A4. Code Mode intake.** Resolve the tool name from both handler arguments
(`extractToolName(target) || extractToolName(output) || extractToolName(meta)`)
before `isReadOnlyInspectionTool`. Apply the same inspection filter to the
`TASK INTENT DETECTED` reminder detector so `bash`, `execute`, and `mcp_execute`
output no longer receives it. Update the existing fixture that currently expects
a nudge on `bash` output.

**A5. Namespace escaping.** Constrain namespace and task label to
`[A-Za-z0-9_.:-]` at `resolveConfig` and escape them in every
`buildSubagentContextHeader` copy (core, Hermes, OpenClaw) before interpolation.
Rebuild all dists.

**A6. Pre-push hook.** In `SCAFFOLDED_PRE_PUSH_HOOK`: fail closed on missing
binary and on any gate exit other than 0/1; remove the advertised
`NEUROSTRATA_SKIP_GATE` line from the blocked message while retaining the escape
hatch with an auditable stderr notice; classify `git push --no-verify` as
mutating in both predicates; regenerate the hook hash marker; update the tests in
`src/task/mod.rs` and `src/main.rs` that pin the old fail-open behavior. Update
`docs/features-api/subsystems/central-executive.md` to state the real trust
model (hook is the required default; `--no-verify` is out of band).

### Section B — gate reliability (issue group B)

**B1.** Replace the exact shebang `case` with a pure `classify_shebang()` returning
`bash|sh|""`, tolerant of trailing CR, interpreter flags (`#!/bin/bash -e`), and
`env -S bash`. Use bash-3.2-compatible regex held in a variable; no `mapfile`.
**B2.** Enumerate staged paths into a temp file, check `git diff` status
explicitly, fail nonzero on enumeration error, and read NUL-delimited entries.
Fold temp-file cleanup into the existing trap.
**B3.** Symlink the install root's `web-ui/node_modules` into the snapshot before
running ESLint, preserving the snapshot cwd and relative file arguments.
**B4.** CI staged-shell selection uses `--diff-filter=ACMRT -z` with NUL reads,
matching the local gate. Add static assertions to the CI-wiring fixture.
**B5.** Add fixtures for all four to `scripts/test-staged-quality.sh` and
register them in `run_test`.

### Section C — test health (issue group C)

**C1.** Make session-root selection a pure decision: an explicit caller cwd wins;
otherwise prefer the nearest `.git` ancestor and stop there; ambient `.NeuroStrata`
is only a fallback inside that boundary. Factor the decision per GOLDEN
`b0813917`, update `resolve_task_scope` and the session-write path, and make the
shipped failing test pass.
**C2.** Serialize the three `related_*` tests that reset a process-global counter
with a shared test mutex spanning reset → calls → assert; keep exact assertions.
**C3.** Replace `process.cwd()`-derived fixture paths in `plugins/core/test.mjs`
with paths derived from `import.meta.url`. No production change needed in
`findProjectRoot`, which is already correct.
**C4.** Change BSD-incompatible `sed -i` fixture sites to `sed -i.bak … && rm -f`.
**C5.** Clear ShellCheck warnings in `scripts/*.sh` and `scripts/hooks/pre-commit`
with narrow, justified suppressions only for intentional single-quoted awk blocks.
**C6.** Correct the Kani harness count comment from `40+` to `28`.

### Section D — documentation (issue #324)

Remove six unregistered tool names and the `/twin` route row; change `/mcp` and
`/taxonomy` to POST; replace the taxonomy operation list with the seven real
operations; state that Vault-LD sync ships in v1.19.0; correct the `is_mutating`
causal claim; add the shipped Vault-LD capability to `product-vision.md`; fix the
Kani count. All edits verified against `src/server.rs` and `src/daemon.rs`.

### Section E — verification bar (no PR before all pass)

1. `cargo test --bin neurostrata-mcp` — full suite, parallel and `--test-threads=1`.
2. `npm --prefix plugins/core test` from repo root **and** from `plugins/core/`.
3. `bash scripts/test-staged-quality.sh` — all fixtures pass.
4. `shellcheck -S warning scripts/*.sh scripts/hooks/pre-commit` — zero warnings.
5. Plugin dist bundles byte-identical to a fresh TypeScript build.
6. Lean proofs: zero `sorry`/`axiom`; `tests/lean_parity_test.rs` green; 28 Kani
   harnesses present.
7. `git diff --check`, `bash scripts/check-release-references.sh`.
8. Independent pre-publication review (free model) with zero Critical/Important.
9. Explicit owner authorization to open the PR.

## 6. Libraries, determinism, isolation

**Library evaluation:** no new dependency is justified. Rust kernel, Lean proofs,
Node's built-in test runner, git, bash, `mktemp`, `shellcheck`, and the already
vendored ESLint toolchain cover every requirement. This conclusion follows
evaluation of available capabilities and licenses (all existing, compatible);
it is not a zero-new-dependency preference.

All decisions are deterministic and zero-LLM. Gate inputs are payload strings and
file paths; outputs are booleans or enumerated classifications. Namespace scope
is per-project; no cross-namespace data is consulted. No network calls occur in
the decision paths.

## 7. Cross-references

- `docs/superpowers/specs/2026-11-18-v1-19-0-path-security-remediation-design.md`
  — path-security remediation; this spec extends its fail-closed posture to
  shell classification.
- `docs/superpowers/specs/2026-11-18-staged-format-lint-design.md` — staged
  quality gate design; sections B1–B5 extend its coverage.
- `docs/superpowers/specs/2026-11-18-kani-formal-verification-design.md` — proof
  and parity conventions used by A1 and C6.
- `docs/superpowers/specs/2026-11-13-release-docs-contract.md` — release reference
  gate invoked in section E7.

## 8. Sequencing

Edits are sequenced A → C → B → D because `src/server.rs`, `src/task/mod.rs`,
and `plugins/core/test.mjs` are shared between A and C. Each workstream lands as
its own focused commit with its own regression tests; the branch is verified as
a whole before the PR gate in section E is even considered.

## 9. Non-goals

None. Every issue #310–#324 is in scope. No item is deferred, stubbed, or
relegated to a later phase.
