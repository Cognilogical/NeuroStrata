# 2026-11-21 — Release-Reference Scripts Must Pass the Shell Lint Gate (#338)

> **Baseline**: `cognilogical/main` @ `a75b243a`
> **Issue**: #338 (P3)
> **Branch**: `fix/task-revert-and-shellcheck`
> **Workflow**: spec → red eval → minimal fix → green eval → regression suites

---

## 1. Scope

`scripts/check-release-references.sh` and
`scripts/check-release-references.test.sh`, plus the shell-lint exercise in
`scripts/test-staged-quality.sh`. No behavior of the reference checker or its
fixtures changes.

Non-goals: the logger, other shell scripts, shellcheck version pinning,
rewriting the fixtures.

---

## 2. Problem

The repo's staged shell gate is `scripts/check-staged-quality.sh` Check 5
(plain `shellcheck` over changed `*.sh`). Both scripts fail it:

- `check-release-references.test.sh`: 17 × SC2016 on single-quoted fixture
  strings that must contain literal backtick-quoted names (lines 96-183
  feeding `write_narrative`, and line ~196's `grep -qF '(`tree`)'`). The
  quoting is deliberate; rewriting the fixtures would break what they
  assert.
- `check-release-references.sh`: SC2016 on the `CLI_AWK` program (awk
  variables are not shell expansions), and SC2094 on the release-note
  `while read` loop because `check_line "$NOTES" ...` passes the loop's
  input file to a command.

CI is green only because Check 5 inspects files changed against the PR
base. These files are unchanged on `main`, so the findings surface on any
future touch or branch-sync PR and read as if the sync introduced them.

## 3. Invariant

`shellcheck scripts/check-release-references.sh
scripts/check-release-references.test.sh` exits 0, and that fact is
exercised by the gate's own fixture suite so a future drift cannot stay
latent behind "file unchanged".

## 4. Behavior Specification

1. **SC2016 annotations, no rewrites.** Every intentional literal-string site
   carries a targeted `# shellcheck disable=SC2016` with a why-only comment
   naming the literal fixture intent. The fixture text is byte-identical.
   Disable scope is the fixture region only; SC2016 stays active for the
   test harness code itself.
2. **SC2094 resolved by removing the false-positive shape.** The
   release-note loop passes a display label
   (`docs/releases/changelog/${VERSION}.md`) to `check_line` instead of the
   loop's input path variable. Diagnostics remain identical (same path
   string). No temp file, no second read of the file, no annotation needed.
3. **CLI_AWK annotation.** A single `# shellcheck disable=SC2016` above the
   `CLI_AWK=` assignment, with the awk-vs-shell reason.
4. **Latent findings impossible.** `scripts/test-staged-quality.sh` gains a
   fixture test that stages the real copies of both scripts in a throwaway
   repo and runs the gate with shellcheck delegated to the real binary
   (logged so the test proves both files were linted). The gate must exit 0.
   The test fails loudly if `shellcheck` is absent — the gate already
   requires it (`missing_tool shellcheck`).
5. **Runner unchanged.** `bash scripts/test-staged-quality.sh` remains the
   single entry point; the new test is registered in its `run_test` table.

## 5. Evals

1. Red: `shellcheck` over both scripts exits 1 with the enumerated findings
   (recorded before the fix).
2. Green: `shellcheck` over both scripts exits 0.
3. `bash scripts/test-staged-quality.sh` green, including the new
   release-reference shell-lint test.
4. `bash scripts/check-release-references.test.sh` green (fixture semantics
   unchanged).

## 6. Risks

- **Over-broad disable.** A region-scoped SC2016 disable could hide a real
  expansion bug added later inside the fixture block. Mitigated by keeping
  the region to the literal-fixture section and re-enabling immediately
  after the last fixture assertion; the harness helpers above stay covered.
- **New test needs a real tool.** The suite currently uses fake tools; this
  one test deliberately uses the real `shellcheck` because a fake cannot
  prove cleanliness. CI's `staged-quality` job installs shellcheck, and the
  gate itself refuses to run without it, so the dependency matches the
  gate's own contract.
- **Gate path assumptions.** The fixture repo must mirror the layout the
  gate snapshots (`git checkout-index`), so the test stages through git
  exactly like the existing hook tests.
