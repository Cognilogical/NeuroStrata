# Task 0.3 — Test-Venue Runbook (measured)

Anthropic-Memory Parity program, Milestone 0. This runbook is the complete,
self-contained procedure for running a Rust RED → GREEN cycle in this
program. A fresh agent with no other context should be able to execute one
full cycle using only this document. Every number in it was measured, not
estimated from documentation; the measurements are reproduced in §5 and §6
so later agents can tell when the venue has drifted from them.

**Program rules that govern everything below (non-negotiable):**

- All code and docs land on the **fork** `pratik-saptarshi/NeuroStrata`,
  base branch `develop`. Nothing is filed, pushed, or PR'd against upstream
  `Cognilogical/NeuroStrata` without the owner's explicit per-action consent.
- All GitHub calls go through the `ghg` gateway (see §3.5). `git push` itself
  uses the git credential helper — ghg gates `gh` calls, not git transport.
- **Never contact the live NeuroStrata daemon or its store.** Do not run
  `neurostrata-mcp` commands, do not claim tasks through it, and do not use
  any skip-gate variable (in particular never `NEUROSTRATA_SKIP_GATE=1`).
  The repo's in-tree `AGENTS.md` demands daemon task-claiming and task-gated
  pushes; that conflicts with this program's rules and the conflict is
  already recorded by the program coordinator — do not comply with those
  parts of `AGENTS.md` while working this program.
- Merges are the owner's per-PR word. Nothing auto-merges. **WIP limit:
  ≤ 3 program PRs awaiting merge (§8.4).**

---

## 1. Venue facts

| Fact | Value (measured / verified 2026-11-08) |
|---|---|
| Repository | fork `pratik-saptarshi/NeuroStrata`, base `develop` |
| Base SHA for all M0 work | `5b8f9a7c3b7808da1f0c550e8eb081667335954c` ("fix(cli): send the auto-spawned daemon's stderr to daemon.log instead of /dev/null (#27)") — verify with `git rev-parse origin/develop` after a fetch; do not trust a SHA quoted in any document, this one included |
| Local Rust toolchain | **No *sanctioned* local toolchain for this program.** System and rustup cargos do exist on this VM (`command -v cargo` → `/usr/bin/cargo`, 1.75.0; a rustup shim at `~/.cargo/bin/cargo` reports 1.99.1) — they are not the venue and must not be used. Do not build or test locally, and do not install a toolchain — whether to install one is an explicit owner election at the Milestone 1 gate. CI is the venue. |
| Test venue | Fork GitHub Actions CI, workflow `CI` (`.github/workflows/ci.yml`), triggered on every pull request |
| Iteration signal | The **`test (ubuntu-latest)`** check — the only hard test leg |
| Gate loop | The full 12-check suite on a PR (§2) |
| Suite shape | `cargo test --locked -- --test-threads=1` (single-threaded by design; the suite's per-test temp databases exhaust the mmap budget in parallel) — 1,050 tests in the main binary on the M0 base, 1,051 with the pilot test added |
| Superseded runs | `ci.yml` sets `cancel-in-progress: true` per ref: **pushing a new commit cancels the previous run on that branch.** A cancelled run produces no verdict — never quote one |

## 2. Hard legs vs report-only legs

Observed check list (identical on PRs #28, #31 and the pilot PR #32):

| Check | Class | Notes |
|---|---|---|
| `build (ubuntu-latest)` | **hard** | `cargo build --release --locked` + smoke + version-vs-`Cargo.toml` check |
| `build (macos-15)` | **hard** | same, macOS arm64 runner |
| `build (windows-latest)` | **hard** | same, MSVC — the long pole on a normal day (§6) |
| `test (ubuntu-latest)` | **hard** | the iteration signal |
| `release governance` | **hard** | grep-level assertions on `release.yml` permissions / action pinning; runs in seconds |
| `test (windows-latest) (report-only)` | report-only | pre-existing baseline: the test suite does not compile on Windows (Unix-only APIs in test code) — see `.github/quality-baseline.md` |
| `test (macos-15) (report-only)` | report-only | pre-existing baseline: 3 platform-assumption failures recorded in `.github/quality-baseline.md` |
| `fmt (report-only)` | report-only | baseline: 212 diff hunks / 38 files |
| `clippy (report-only)` | report-only | baseline: 210 diagnostics |
| `audit (report-only)` | report-only | baseline: 9 RustSec vulnerabilities + 4 informational |
| `deny (report-only)` | report-only | baseline: 371 errors / 12 warnings under default rules (no `deny.toml`) |
| `gui (tauri build) (report-only)` | report-only | baseline: broken tracked symlink `web-ui/public/graph.json` fails the web-ui build on a clean checkout |

Rules for reading this table:

- A check whose name contains **`(report-only)`** failing is **baseline
  noise**, not your result. Those legs run with `continue-on-error` and do
  not block a merge. Expect **all seven** report-only legs to show `fail`
  on a healthy PR — PR #28, a docs-only PR, shows exactly that.
- A **hard** leg failing is a real result until the log proves it is the
  known baseline abort in §2.1 — and even then it is a *hard-leg failure
  with a baseline cause*, never a pass.
- Promotion rule (from `ci.yml` / `.github/quality-baseline.md`): a
  report-only leg becomes hard only when its recorded baseline count
  reaches zero. Do not promote, demote, or "fix" baseline counts inside an
  unrelated PR.

### 2.1 Known hard-leg baseline failure — 2026-11-08 `test (ubuntu-latest)` abort

On 2026-11-07, PR #28's `test (ubuntu-latest)` **passed** (5m1s; main
binary: 1,049 passed / 0 failed / 1 ignored, test execution 95.9s).

On 2026-11-08 the same leg **aborted mid-suite in four out of four
observed runs** (PR #31, both pilot RED runs, and the pilot GREEN run —
§5), including PR #31, which contains **no Rust source change at all**
(docs + a shell hook). Signature, identical in all four runs:

- The test binary dies with SIGABRT (signal 6, cargo exit code 101) on a
  LadybugDB C++ assertion:
  `unique_ptr.h:455: … std::unique_ptr<lbug::common::ValueVector>::operator*() … Assertion 'get() != pointer()' failed.`
- The abort happens in the `parser::ingest` area, at/just before
  `parser::ingest::tests::a_failed_relink_rolls_back_to_the_previous_generation`
  (the last test name printed in the log). Tests alphabetically after
  `parser::` in the main binary **never execute** in an aborted run.
- Runner image was identical to the passing #28 run
  (`ubuntu-26.04`, image version `20261101.186.1`), so this is not an image
  change. The 2026-11-08 runs were also uniformly ~5× slower (§5, §6),
  which points at a throttled/slow runner pool interacting with a
  timing-sensitive test — that mechanism is a hypothesis, not a proven
  cause. What is proven: the abort is **not caused by the change under
  test** (PR #31 proves it) and **was not present** on 2026-11-07.

Consequences for every agent using this venue:

1. **A `test (ubuntu-latest)` failure is not automatically your failure.**
   Always read the job log (§4) and classify: (a) your test's `FAILED`
   line / assertion output, (b) the §2.1 abort signature, (c) something
   else — report (c) verbatim, do not guess.
2. **An aborted run gives no verdict on tests ordered after `parser::`.**
   If your new test lives in a late-alphabet module (`util::`, `store::`,
   `server::`, …), an abort can silently erase it — the pilot's first RED
   run failed exactly this way (§5.1). **Place probe/pilot tests in an
   early module** (`attempt::` is the proven choice) so their per-test
   `ok` / `FAILED` line is emitted before the abort point.
3. A rerun may pass (the leg passed on 2026-11-07). One rerun to confirm a
   suspected baseline abort is legitimate; rerunning until green and
   quoting only the green run is not — record every attempt.

## 3. The iteration loop, step by step

All timestamps in this program are UTC: `date -u +%FT%TZ`. Record them at
each step; a cycle without timestamps is not measured and does not count
toward any budget estimate.

### 3.1 Create the worktree and branch

Worktrees live under `~/workspace/repos/`. The main clone is
`~/workspace/repos/NeuroStrata`.

```bash
cd ~/workspace/repos/NeuroStrata
git fetch origin develop
git rev-parse origin/develop        # must print 5b8f9a7c3b7808da1f0c550e8eb081667335954c on the M0 base;
                                    # if it prints anything else, STOP — the base moved, re-census first
date -u +%FT%TZ                     # record: branch created
git worktree add -b <branch-name> ~/workspace/repos/NeuroStrata-<slug> origin/develop
cd ~/workspace/repos/NeuroStrata-<slug>
git log --oneline -1                # verify HEAD is the base commit
```

Branch naming as used by M0: `docs/memory-parity-m0…` for plan docs,
`pilot/test-venue-measure` for the §5 pilot, ordinary `feat/…` / `fix/…` /
`test/…` prefixes for code.

### 3.2 Make the change; commit conventions

- One logical change per commit; conventional-commit subjects, e.g.
  `test(attempt): add pilot venue test with deliberately wrong expectation (RED)`,
  `fix(store): …`, `docs(plans): …`.
- For a RED commit: the change must **compile** and fail as a **test
  assertion**. A compile error is not RED — CI cannot distinguish your
  intent from breakage, and the build legs will fail instead of the test
  leg. Keep the wrong expected value obviously wrong (the pilot used
  `999` against a true value of `4`) and name the test so it is findable
  in a 1,000-line log (the pilot used the `pilot_venue_` prefix).
- Scrub before committing (§7).

### 3.3 Push (and what the pre-push hook will do)

```bash
date -u +%FT%TZ                     # record: push start
git push -u origin <branch-name>    # first push; later pushes: git push
date -u +%FT%TZ                     # record: push done
```

The clone's pre-push hook has two guards; both are designed behaviour,
neither requires any action from you:

- **Guard 1 — upstream-push guard.** Rejects (exit 1) any push whose remote
  URL path is `Cognilogical/NeuroStrata`, in any common form. The fork URL
  (`https://github.com/pratik-saptarshi/NeuroStrata.git`) passes. Never push
  with an upstream remote URL, and never add an upstream remote to push to.
- **Guard 2 — task gate.** In this environment `neurostrata-mcp` is not on
  `PATH`, so the hook prints
  `warning: neurostrata-mcp not found — gate NOT run for this push`
  and passes (exit 0). That is its designed fail-soft here. **Do not**
  "fix" it, do not install or invoke the daemon, and do not set any
  skip-gate variable — the warning line in the push output is the expected,
  correct behaviour for this program, and it is worth copying into your
  task report as evidence the push went through Guard 1 + fail-soft
  Guard 2. Qualification: this fail-soft expectation holds for the agent
  exec shell, where `neurostrata-mcp` is absent from `PATH`. In a login
  shell, `~/go/bin` is on `PATH` and `neurostrata-mcp` would actually be
  invoked as the task gate. If the "gate NOT run" warning does **NOT**
  appear on a push, **STOP and report before proceeding** — the task gate
  must not execute against the live store under this program's rules.

### 3.4 Open the PR (ghg, with pre-flight)

Every GitHub write is pre-flighted with `ghg check` using the **same
arguments** (see §3.5 for the gate rules), then executed with `ghg run`:

```bash
export GHG_ACTOR=parity-m0
~/go/bin/ghg check pr create --repo pratik-saptarshi/NeuroStrata \
  --base develop --head <branch-name> \
  --title "<conventional title>" --body "<what, why, not-for-merge if a pilot>"
# only if check exits 0:
date -u +%FT%TZ                     # record: PR open start
~/go/bin/ghg run -- pr create --repo pratik-saptarshi/NeuroStrata \
  --base develop --head <branch-name> \
  --title "<conventional title>" --body "<what, why, not-for-merge if a pilot>"
date -u +%FT%TZ                     # record: PR open done; note the PR number/URL printed
```

Note the asymmetry: `ghg check <args>` takes the `gh` arguments **without**
a leading `gh` and without `--`; `ghg run -- <args>` takes them after `--`.

### 3.5 ghg budget-gate behaviour

- `ghg check` prints `ghg check: OK — charges fit current headroom` and
  exits 0 when the call is within budget.
- **Exit 75 (budget denial) or any 403 from ghg: STOP that step.** Do not
  retry around the gate, do not rephrase the call to dodge it, and never
  set `GHG_BYPASS=1` (operator-only). Report the exact stage reached
  (which call, which arguments, what had already succeeded) to the
  coordinator / owner.
- ghg is an accounting control, not an authorization control: a passing
  `ghg check` never by itself authorizes a push, merge, or deletion.

### 3.6 Read the result

The iteration signal is one check. Poll it — do not poll the whole suite:

```bash
export GHG_ACTOR=parity-m0
# Terminal state + exact start/finish for the iteration leg:
~/go/bin/ghg run -- api repos/pratik-saptarshi/NeuroStrata/commits/<head-sha>/check-runs \
  --jq '.check_runs[] | select(.name=="test (ubuntu-latest)") | [.name,.status,.conclusion,.started_at,.completed_at,.id] | @tsv'
# Whole-suite snapshot (gh prints report-only failures in this list too — use §2 to classify):
~/go/bin/ghg run -- pr checks <pr-number> --repo pratik-saptarshi/NeuroStrata
```

Reading notes, all learned on the pilot:

- In `pr checks` output an in-progress leg prints as `pending` with
  duration `0`. The `check-runs` API above is the authoritative source for
  `in_progress` vs `queued`. Qualification on start times: while a job is
  `queued`, its `started_at` may be populated but is provisional and can
  shift when the job actually starts (observed 10:29:17Z → 10:29:24Z);
  the `status` field is authoritative for state, and start times are real
  once the job is `in_progress`/`completed`.
- A check-runs poll can transiently return an empty result before any row
  exists (observed 4s before `started_at` on a GREEN cycle) — empty is
  not an error or a failed query; keep polling.
- `ghg run -- pr checks <n>` exits 1 whenever any leg has failed —
  including the routine report-only baseline failures — while still
  printing a fully usable snapshot; exit 1 there does not mean the
  command failed.
- Queue time (push → check `started_at`) was 9–25s in every pilot cycle.
  The wall-clock that matters for budgeting is **push → terminal result
  for `test (ubuntu-latest)`**, job duration + queue + push time.
- Job logs: `gh run view --log` refuses while the overall run is still in
  progress (`run … is still in progress; logs will be available when it is
  complete`). A **completed job's** log is available immediately via the
  API — the `--allow-escape-sequences` flag is required, without it `gh
  api` refuses with "the response contains terminal escape sequences":

```bash
~/go/bin/ghg run -- api repos/pratik-saptarshi/NeuroStrata/actions/jobs/<job-id>/logs \
  --allow-escape-sequences > /tmp/job.log
grep -a "pilot_venue_\|FAILED\|Assertion\|test result" /tmp/job.log
```

  (Job IDs come from the `check-runs` query above — the `.id` field — or
  from the job URLs in `pr checks` output.)
- In the log, find **your test's own line** first:
  `test <module>::tests::<your_test_name> ... FAILED` (RED) or `... ok`
  (GREEN). Only then look at the overall conclusion, and classify any
  failure with §2 / §2.1. Because an aborted binary never prints its
  end-of-suite `failures:` detail section, the per-test line plus the
  commit diff may be all the evidence there is — say so when it is.

### 3.7 GREEN, and the gate loop

- GREEN = the same test with the corrected assertion, committed
  (`test(attempt): correct pilot venue test expectation (GREEN)`) and
  pushed. The push cancels the RED run's remaining legs (§1) — that is
  expected; the gate loop is evaluated on the **final** head, not per push.
- The **gate loop** is: final head pushed → all 12 checks terminal → hard
  legs green (or a hard-leg failure classified under §2.1 with log
  evidence) → report-only legs matching the §2 baseline (new report-only
  failures beyond baseline are a finding, not noise) → review census (§8).
- Never call a PR merge-ready from the iteration leg alone.

## 4. A complete RED → GREEN recipe (copy-paste grade)

This is the exact pilot cycle of §5, generalized only in the branch name.
Target: `src/attempt.rs`, a small pure-function module whose tests run
early in the suite (see §2.1, consequence 2). `ATTEMPT_KEYS` is a
`[&str; 4]` constant — a true, stable property.

```rust
// in src/attempt.rs, inside #[cfg(test)] mod tests:
    #[test]
    fn pilot_venue_attempt_keys_count_is_stable() {
        assert_eq!(ATTEMPT_KEYS.len(), 999);   // RED: deliberately wrong
    }
```

1. Worktree + branch per §3.1, using a **NEW branch name** — for example
   `pilot/test-venue-measure-acceptance` — and a matching new worktree
   slug. The original pilot branch `pilot/test-venue-measure` is retained
   (it exists locally and on origin at `918bae0`, the GREEN head) and
   must **NOT** be reused — reusing it breaks worktree creation and makes
   the RED step unreproducible. (`pilot/test-venue-measure` was the branch
   used in the original pilot, §5.)
2. Add the test exactly as above. Commit:
   `test(attempt): add pilot venue test with deliberately wrong expectation (RED)`
   — in the executed pilot this test first landed in `src/util.rs` and was
   moved to `src/attempt.rs` by a second still-RED commit (§5.1); a fresh
   run should add it in `src/attempt.rs` directly and needs only the two
   commits below.
3. Push (§3.3), open the PR (§3.4) titled as a measurement pilot,
   **not for merge**.
4. Wait for `test (ubuntu-latest)` (§3.6). Expect: check `failure`;
   log line `test attempt::tests::pilot_venue_attempt_keys_count_is_stable ... FAILED`.
   If instead the log shows the §2.1 abort **and** your test's `FAILED`
   line, RED is still confirmed (your line precedes the abort point). If
   the log shows the abort and **no** line for your test, RED is **not**
   confirmed — your test never ran; move it earlier and repeat.
5. Change `999` to `4`, and remove the trailing
   `// RED: deliberately wrong` comment — a correct assertion must not
   keep a comment calling it deliberately wrong. Commit:
   `test(attempt): correct pilot venue test expectation (GREEN)`. Push.
6. Wait for `test (ubuntu-latest)` again. Expect: your test's `... ok`
   line. The check conclusion is `success` on a healthy day; on a
   §2.1-abort day it will read `failure` with your test `ok` and the abort
   signature later in the log — record both facts, exactly as §5.3 does.
7. Close the pilot PR (§9) and leave the branch in place.

## 5. Measured pilot — PR #32 (2026-11-08)

Pilot PR: **#32** — `test(util): pilot venue measurement for Task 0.3 (RED, do not merge)`,
branch `pilot/test-venue-measure`, base `develop` @ `5b8f9a7`.
URL: https://github.com/pratik-saptarshi/NeuroStrata/pull/32
**Closed 2026-11-08 (10:22:54Z)** with a comment stating it was a Task 0.3
measurement pilot, that its evidence is recorded in this runbook, and
that it is not for merge (§9 procedure; both the comment and the close
were ghg-pre-flighted). The pilot branch was left in place.
All times UTC. "Round trip" = push start → `test (ubuntu-latest)`
`completed_at` from the check-runs API.

### 5.1 RED attempt 1 — the late-module trap (commit `f139ba2`)

| Step | UTC |
|---|---|
| Branch created | 2026-11-08T08:46:00Z |
| RED commit `f139ba2e4a74de06991718f7744587ff58f1fa6b` (test in `src/util.rs`, `assert_eq!(*guard, 999)` against a mutex holding `7`) | 2026-11-08T08:46:12Z |
| Push start → done | 08:46:14Z → 08:46:16Z |
| PR #32 opened | 08:46:23Z → 08:46:25Z |
| `test (ubuntu-latest)` started (job 128680261344) | 08:46:31Z (17s after push start) |
| `test (ubuntu-latest)` completed — **failure**, job duration **24m0s** | 09:10:31Z |
| **Round trip** | **24m17s** |

Verdict: **RED not confirmed.** The job log contains no line at all for
`util::tests::pilot_venue_lock_helper_preserves_value` — the binary aborted
with the §2.1 SIGABRT signature at 09:10:28Z in the `parser::ingest` area,
before the alphabetically-later `util::` tests could run. The failure was
real but it was the baseline abort, not the pilot assertion. This run is
the reason §2.1 consequence 2 and the §4 recipe exist.

### 5.2 RED attempt 2 — early module (commit `9c38d23`)

The second commit moved the pilot test unchanged-in-intent to
`src/attempt.rs` (`assert_eq!(ATTEMPT_KEYS.len(), 999)`, true value `4`)
and removed the `util.rs` test.

| Step | UTC |
|---|---|
| RED-2 commit `9c38d23862034ded56d6f5a05d040aa04bca000b` | 2026-11-08T09:11:45Z |
| Push start → done | 09:11:46Z → 09:11:48Z |
| `test (ubuntu-latest)` started (job 128686082091) | 09:12:11Z (25s after push start) |
| Pilot test line in log | 09:37:50Z — `test attempt::tests::pilot_venue_attempt_keys_count_is_stable ... FAILED` |
| `test (ubuntu-latest)` completed — **failure**, job duration **29m21s** | 09:41:32Z |
| **Round trip** | **29m46s** |

Verdict: **RED confirmed.** The test compiled (the suite ran) and the
harness recorded that exact test as `FAILED` — its only assertion is the
deliberately wrong `999`. The same job log *also* contains the §2.1 abort
(09:41:29Z, after the pilot test had already failed at 09:37:50Z), which
is why no end-of-suite `failures:` detail section (with left/right values)
exists in the log: an aborted binary never prints it. Both facts are in
the log; neither cancels the other.

### 5.3 GREEN (commit `918bae0`)

| Step | UTC |
|---|---|
| GREEN commit `918bae0b47337bacd0290ca84427cbb23bb62618` (`999` → `4`) | 2026-11-08T09:42:27Z |
| Push start → done | 09:42:27Z → 09:42:29Z |
| `test (ubuntu-latest)` started (job 128692922511, run 42468323211) | 2026-11-08T09:42:36Z (9s after push start — the fastest queue observed) |
| Pilot test line in log | 2026-11-08T10:06:14Z — `test attempt::tests::pilot_venue_attempt_keys_count_is_stable ... ok` |
| `test (ubuntu-latest)` completed — **failure (see verdict)**, job duration **26m5s** | 2026-11-08T10:08:41Z |
| **Round trip** | **26m14s** |

Verdict: **GREEN confirmed at the test level; the leg's overall
conclusion is a §2.1 baseline failure, not a pilot failure.** The
corrected test ran and passed (`... ok` at 10:06:14Z). The binary then
aborted with the identical §2.1 SIGABRT signature at 10:08:36Z in the
`parser::ingest` relink area — after the pilot test had already passed,
in a module the pilot does not touch, and identically to the no-Rust-change
PR #31 run of the same morning. No other test failure appears in the log
before the abort. So the cycle's GREEN evidence is the per-test `ok`
line, and the leg conclusion must be quoted as
"failure — baseline abort (§2.1), pilot test ok", never as either a
clean pass or a pilot failure.

Rerun attempt, recorded for completeness: a single-job rerun of the
GREEN `test (ubuntu-latest)` job was requested at 2026-11-08T10:09:22Z
(ghg pre-flight passed) and GitHub refused it — `job 128692922511 cannot
be rerun` — because the parent run (42468323211) was still in progress
(its `build (windows-latest)` leg was still running; on the same morning
PR #31's windows build leg took 67m58s, §6.2). A rerun therefore cannot
be used to rescue a leg while its gate run is still open, and with four
of four 2026-11-08 ubuntu test runs aborting identically (§2.1), a
post-completion rerun that day had no realistic prospect of a different
verdict; none was claimed. Re-attempting the GREEN leg on a later day —
when the §2.1 baseline may have cleared, as it was clear on 2026-11-07 —
remains open to any agent: the pilot test's `ok` line is the GREEN
evidence in the meantime.

### 5.4 What the pilot numbers mean

- **Good-day iteration round trip: ~5 minutes.** On 2026-11-07 PR #28's
  `test (ubuntu-latest)` job took 5m1s end to end.
- **Slow-day iteration round trip: 24–30 minutes.** All three 2026-11-08
  pilot cycles landed in this band (24m17s, 29m46s, 26m14s), and every
  other leg observed that day was similarly inflated (§6). There is no
  in-between data point yet; budget with the slow-day number and treat
  the good-day number as a bonus.
- Queue + push + PR-open overhead is negligible in both regimes
  (≤ ~30s total). **All of the variance is runner-side job duration.**
- A full RED → GREEN cycle costs **two** iteration round trips plus, on
  the final head, one full gate loop (§6): plan ~35 minutes of wall-clock
  per cycle on a good day (2 × ~5 min + ~21 min gate) and ~2 hours on a
  slow day (2 × ~27 min + 68 min gate), most of it waiting.

## 6. Gate-loop timings (measured)

### 6.1 PR #28, run 42425775075 (2026-11-07, the "good day" reference)

Run wall-clock: created 23:33:18Z → completed 23:53:47Z = **20m29s** for
all 12 jobs (they run in parallel; the wall-clock is set by the slowest
job, not the sum).

| Job | Result | Duration | Started → completed (UTC) |
|---|---|---|---|
| release governance | pass (hard) | 5s | 23:33:21Z → 23:33:26Z |
| fmt (report-only) | fail (baseline) | 7s | 23:33:21Z → 23:33:28Z |
| deny (report-only) | fail (baseline) | 29s | 23:33:21Z → 23:33:50Z |
| gui (tauri build) (report-only) | fail (baseline) | 52s | 23:33:21Z → 23:34:13Z |
| clippy (report-only) | fail (baseline) | 2m49s | 23:33:21Z → 23:36:10Z |
| audit (report-only) | fail (baseline) | 2m53s | 23:33:21Z → 23:36:14Z |
| build (ubuntu-latest) | pass (hard) | 3m53s | 23:33:21Z → 23:37:14Z |
| build (macos-15) | pass (hard) | 4m20s | 23:33:22Z → 23:37:42Z |
| test (ubuntu-latest) | pass (hard) | 5m1s | 23:33:21Z → 23:38:22Z |
| test (windows-latest) (report-only) | fail (baseline) | 11m35s | 23:33:21Z → 23:44:56Z |
| build (windows-latest) | pass (hard) | 14m41s | 23:33:20Z → 23:48:01Z |
| test (macos-15) (report-only) | fail (baseline) | **20m24s** | 23:33:22Z → 23:53:46Z |

**Gate long poles:** `test (macos-15) (report-only)` 20m24s sets the
run's wall-clock, followed by `build (windows-latest)` 14m41s (the
longest *hard* leg) and `test (windows-latest) (report-only)` 11m35s.
The iteration signal (`test (ubuntu-latest)`, 5m1s) arrives long before
the gate loop closes — that gap is the whole point of iterating on the
one leg and gating on the twelve.

Note the report-only warning in its concrete form: **all seven
report-only legs failed on PR #28** (a docs-only PR), and the run's
overall conclusion was still `success`, because report-only jobs use
`continue-on-error`. All seven failures are the pre-existing baseline of
§2, recorded in `.github/quality-baseline.md`.

### 6.2 PR #31 (2026-11-08, the "slow day" cross-check)

PR #31 (Task 0.2, docs + pre-push guard, **no Rust change**) was opened
2026-11-08T08:44:42Z, run 42464221650. The run completed at
09:52:47Z — total wall-clock **68m2s** (the CI *run* duration, run
created→completed; measured from the PR-open time above it would be
68m5s) (vs 20m29s for #28) — with overall
conclusion `failure`, driven solely by the hard `test (ubuntu-latest)`
leg's §2.1 abort (all other hard legs passed). Slow-day gate long pole:
`build (windows-latest)` at **67m58s** (08:44:48Z → 09:52:46Z), vs
14m41s on #28. The legs measured:

| Job | Result | Duration |
|---|---|---|
| release governance | pass (hard) | 5s |
| fmt (report-only) | fail (baseline) | 8s |
| deny (report-only) | fail (baseline) | 32s |
| gui (tauri build) (report-only) | fail (baseline) | 1m12s |
| audit (report-only) | fail (baseline) | 2m22s |
| test (macos-15) (report-only) | fail (baseline) | 3m34s |
| test (windows-latest) (report-only) | fail (baseline) | 10m15s |
| build (macos-15) | pass (hard) | 4m22s |
| clippy (report-only) | fail (baseline) | 16m28s |
| build (ubuntu-latest) | pass (hard) | **17m36s** (vs 3m53s on #28) |
| test (ubuntu-latest) | **fail — §2.1 abort**, job 128679881511 | **27m26s** (vs 5m1s pass on #28) |
| build (windows-latest) | pass (hard) | **67m58s** (vs 14m41s on #28) — slow-day gate long pole |

Two lessons: (1) the §2.1 abort is baseline, proven by a no-Rust-change
PR; (2) slow-day inflation is not uniform per leg (macOS test got
*faster*, ubuntu build/test got ~5× slower) — do not budget by scaling a
single leg.

## 7. Hygiene (before every push)

- Scrub anything that ships in a PR of machine paths, home-directory
  paths, hostnames, and agent/session identities. Write paths as
  `~/workspace/repos/…` or repo-relative, never as an absolute local path
  naming the machine's layout. (M0 precedent: Task 0.2's PR #31 needed a
  dedicated follow-up commit scrubbing local clone paths from its install
  evidence.)
- No secrets, tokens, or credential material in any file, commit message,
  PR title, or PR body — ghg accounting lines and check output are fine,
  credential values are never fine.
- The pilot/final diff should be exactly the change the PR claims: check
  `git diff origin/develop...HEAD --stat` before opening, and re-check it
  before calling the PR ready.

## 8. Before calling anything merge-ready

A PR is merge-ready only when **all** of these hold, verified on the
**exact final head SHA**:

1. Gate loop (§3.7) terminal on that head; hard legs green, or a hard-leg
   failure classified under §2.1 with the job-log signature quoted and
   every attempt (including reruns) recorded.
2. Report-only legs match the §2 baseline — no *new* report-only failure
   modes.
3. **Review-thread + Codex census on that head:** enumerate *all* review
   threads (untruncated — a paginated/truncated list is not a census),
   confirm each is resolved or explicitly dispositioned, and check whether
   Codex actually reviewed the final head. **A Codex no-show is
   "pending", never a pass.** A census is provisional for several minutes
   after a review completes — re-run it at report time.
4. WIP limit respected: ≤ 3 program PRs awaiting merge.
5. Then stop. **The merge itself is the owner's per-PR word** — a
   merge-ready report is the deliverable, not the merge.

## 9. Closing a pilot / measurement PR

Pilots are closed, never merged, and their branches are **left in place**
(branch deletion follows the owner's deletion rule — confirmation, a
1-hour delay, and reconfirmation — and is never part of a pilot):

```bash
export GHG_ACTOR=parity-m0
~/go/bin/ghg check pr comment <n> --repo pratik-saptarshi/NeuroStrata \
  --body "Measurement pilot for Task 0.3 … evidence recorded in <where the run's evidence is recorded — the runbook for the original pilot, the run report for re-runs> … not for merge."
~/go/bin/ghg run -- pr comment <n> --repo pratik-saptarshi/NeuroStrata \
  --body "Measurement pilot for Task 0.3 … evidence recorded in <where the run's evidence is recorded — the runbook for the original pilot, the run report for re-runs> … not for merge."
~/go/bin/ghg check pr close <n> --repo pratik-saptarshi/NeuroStrata
~/go/bin/ghg run -- pr close <n> --repo pratik-saptarshi/NeuroStrata
```

## 10. Per-phase CI-run budget estimates

Derivation, stated so it can be re-derived: **one push = one CI run**
(12 jobs; a re-push cancels the previous run, §1). A code PR's life is
RED push → GREEN push → review-fix pushes. The pilot used **3 pushes**
for a deliberately two-stage RED plus one GREEN; an ordinary program PR
is budgeted at **3 pushes** (initial + GREEN/fix + one review round) and
a heavy Rust PR at **5**. Run counts below are pushes; wall-clock uses
§5.4 (≈5 min iteration leg good day / 24–30 min slow day; gate loop
≈21 min good day / **68m2s slow day**, measured on PR #31's complete run,
§6.2 — its long pole was a hard leg, `build (windows-latest)` 67m58s).

**M0 actuals (measured, not budgeted):** 3 PRs — #28 (plan + census),
#31 (sync resolution + guard), #32 (this pilot, 3 pushes / 3 runs) —
plus their review-fix pushes. CI budget per phase is estimated in runs:

| Phase | PRs (plan estimate) | Assumed runs/PR | Estimated CI runs | Iteration-leg wall-clock (slow-day planning number, 30 min/run) |
|---|---|---|---|---|
| Phase 0 — benchmark tooling (mostly Python; CI still runs the full Rust suite per push) | ≈4–5 | 3 | **≈12–15 runs** | ≈6–7.5 h of leg-wait across the phase |
| Phase 1 — consolidation tool (heaviest Rust work) | ≈8–10 | 3–5 | **≈24–50 runs** | ≈12–25 h |
| Phase 2 — wiki + fused retrieval | ≈3–4 | 3–5 | **≈9–20 runs** | ≈4.5–10 h |
| Phase 3 — curator + goal profiles | ≈3 | 3 | **≈9 runs** | ≈4.5 h |

Reading the table honestly: the binding constraint is not GitHub minutes
(the repo is public and the jobs run in parallel) — it is **agent
wall-clock spent waiting on the iteration leg** and **owner merge
bandwidth** (WIP limit §8.4). If slow days like 2026-11-08 are the norm
rather than the exception, Phase 1's wait budget roughly quintuples
versus the good-day numbers; re-measure after the next three program PRs
and update §5/§6 rather than letting this table go stale silently.

## Appendix — command index

```bash
export GHG_ACTOR=parity-m0                                    # every ghg call
~/go/bin/ghg check <gh args, no leading gh, no -->            # pre-flight any GitHub write
~/go/bin/ghg run -- <gh args>                                 # execute
~/go/bin/ghg run -- pr checks <n> --repo pratik-saptarshi/NeuroStrata
~/go/bin/ghg run -- api repos/pratik-saptarshi/NeuroStrata/commits/<sha>/check-runs --jq '…'
~/go/bin/ghg run -- api repos/pratik-saptarshi/NeuroStrata/actions/jobs/<job-id>/logs --allow-escape-sequences
date -u +%FT%TZ                                               # timestamp every step
```
