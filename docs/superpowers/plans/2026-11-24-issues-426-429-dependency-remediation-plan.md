# #426–#429 Dependency Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four dependency-advisory findings in #426–#429 with an
offline, eval-first contract, an exact desktop lockfile patch, a dated `glib`
decision, web-ui manifest floors, and permanent CI/registry gates.

**Architecture:** A single offline integration test (`tests/dependency_integrity.rs`)
parses both committed lockfiles and encodes contracts C1–C3. It is written and
run RED before any dependency moves. Commit 1 changes only the desktop
lockfile plus the eval spine and the C2 decision record. Commit 2 clears the
web-ui graph and wires the CI jobs and registered eval suite. No application
source changes exist in either commit.

**Tech Stack:** Rust 2021 (`toml`, `serde_json`, std), Cargo lockfiles,
`cargo-audit` 0.22.1, npm 11 / Node 24, GitHub Actions, `eval/registry.json`.

**Spec:** `docs/superpowers/specs/2026-11-24-issues-426-429-dependency-remediation-spec.md`

---

## Global Constraints

- Worktree: `/Users/neo/Git-SCM/ns-wt/deps-426-429`, branch
  `chore/deps-426-429-advisories`, based on `cognilogical/main` @ `808e1304`.
- Do not modify `/Users/neo/Git-SCM/NeuroStrata` or its `audit-ns-71` checkout.
- Do not push, tag, or open a PR in this phase.
- Root cargo commands use
  `CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target`.
  `src-tauri` checks use
  `CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target-tauri`.
- Never run `cargo update` without `-p … --precise …`; never edit
  `src-tauri/Cargo.toml`.
- Never edit the root `Cargo.lock`; it was already patched by `2dbe59ac`.
- Commit 1 file set is exactly:
  `docs/superpowers/specs/2026-11-24-issues-426-429-dependency-remediation-spec.md`,
  `docs/superpowers/plans/2026-11-24-issues-426-429-dependency-remediation-plan.md`,
  `Cargo.toml`, `tests/dependency_integrity.rs`, `src-tauri/Cargo.lock`.
- Commit 2 file set is exactly:
  `web-ui/package.json`, `web-ui/package-lock.json`, `.github/workflows/ci.yml`,
  `eval/registry.json`.
- The eval test performs no network I/O and no cargo/npm subprocess. It reads
  `src-tauri/Cargo.lock`, `web-ui/package-lock.json`, and the C2 spec only.
- Version checks use numeric semver tuples, never string ordering.
- Keep the `glib` unsound warning visible. Do not add `RUSTSEC-2024-0429` to
  `.cargo/audit.toml`.

## Review Focus

- `cookie 0.18.10` must compare greater than `0.18.9` (tuple, not lexical).
- Nested `minimatch` at
  `node_modules/@typescript-eslint/typescript-estree/node_modules/minimatch`
  must be inspected, not just the top-level `node_modules/minimatch`.
- `node_modules/zod-validation-error` must not be mistaken for `zod`; match only
  paths equal to `node_modules/zod` or ending in `/zod`.
- The four-hunk envelope is exact: `cookie`, `form_urlencoded`,
  `cssparser-macros`' `syn` edge, `url`. Any fifth hunk is a rejection.
- Package count stays 501; `cargo metadata --locked` is the proof.
- Commit 1 intentionally leaves `webui_has_no_vulnerable_minimatch` and
  `webui_has_no_vulnerable_zod` RED. Commit 2 turns them GREEN.

---

## Verification Matrix

| Gate            | Command                                                                                                       | Expected after commit 1     | Expected after commit 2 |
| --------------- | ------------------------------------------------------------------------------------------------------------- | --------------------------- | ----------------------- |
| RED evidence    | `cargo test --locked --test dependency_integrity -- --test-threads=1`                                         | 4 failed, 1 passed          | 5 passed                |
| Desktop focused | `cargo test --locked --test dependency_integrity srctauri_ -- --test-threads=1`                               | 3 passed                    | 3 passed                |
| Web focused     | `cargo test --locked --test dependency_integrity webui_ -- --test-threads=1`                                  | 2 failed                    | 2 passed                |
| Desktop audit   | `cargo audit --file src-tauri/Cargo.lock`                                                                     | exit 0, 0 vulns, 3 warnings | same                    |
| Desktop compile | `cargo check --locked --manifest-path src-tauri/Cargo.toml`                                                   | exit 0                      | exit 0                  |
| Lock count      | `cargo metadata --locked --manifest-path src-tauri/Cargo.toml --format-version 1 \| jq '.packages \| length'` | `501`                       | `501`                   |
| Web install     | `npm ci --prefix web-ui`                                                                                      | n/a                         | exit 0                  |
| Web lint        | `npm run lint --prefix web-ui`                                                                                | n/a                         | exit 0                  |
| Web build       | `npm run build --prefix web-ui`                                                                               | n/a                         | exit 0                  |
| Eval suite      | `bash scripts/remediation-eval.sh dependency-advisory`                                                        | n/a                         | PASS JSON               |
| Formatting      | `rustfmt --edition 2021 --check tests/dependency_integrity.rs`                                                | exit 0                      | exit 0                  |

---

### Task 1: Spec, eval spine, desktop lockfile patch (#426/#427)

**Files:**

- Create: `docs/superpowers/specs/2026-11-24-issues-426-429-dependency-remediation-spec.md`
- Create: `docs/superpowers/plans/2026-11-24-issues-426-429-dependency-remediation-plan.md`
- Modify: `Cargo.toml`
- Create: `tests/dependency_integrity.rs`
- Modify: `src-tauri/Cargo.lock`

**Interfaces:**

- Produces: `[[test]] name = "dependency_integrity" path = "tests/dependency_integrity.rs"`.
- Produces: five `#[test]` functions named exactly
  `srctauri_cookie_is_patched`, `srctauri_url_is_patched`,
  `srctauri_glib_decision_recorded`, `webui_has_no_vulnerable_minimatch`,
  `webui_has_no_vulnerable_zod`.
- Consumes: `CARGO_MANIFEST_DIR`-relative lockfiles and spec; `toml` and
  `serde_json` as declared root dependencies.

#### Step 1: Write the spec and this plan

Write both documents first. The spec must contain the literal strings
`RUSTSEC-2024-0429` and `gtk >= 0.19`, because
`srctauri_glib_decision_recorded` reads it.

- [x] **Step 1 complete:** both docs exist and the spec contains the two C2
      literals.

#### Step 2: Declare and write the offline eval contract

Append to `Cargo.toml`:

```toml
# Dependency-advisory eval contract: offline lockfile assertions for the
# pinned desktop and web-ui dependency graphs.
[[test]]
name = "dependency_integrity"
path = "tests/dependency_integrity.rs"
```

Create `tests/dependency_integrity.rs` with:

- `manifest_path(rel)` → `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)`.
- `version_tuple(&str) -> (u64, u64, u64)` — strip a leading `v`, split `.`,
  take at most three components, keep leading ASCII digits of each (so
  `2.0.0-alpha.6` → `(2, 0, 0)`), default missing components to `0`.
- `srctauri_versions(name)` — parse `src-tauri/Cargo.lock` with
  `toml::from_str::<toml::Value>` and collect `package[].version` where
  `package[].name == name`.
- `webui_versions(suffix)` — parse `web-ui/package-lock.json` with
  `serde_json::Value` and collect `packages[*].version` where the key is
  `node_modules/<suffix>` or ends with `/<suffix>`, and the value has a string
  `version`.
- Assertions per the spec:
  1. `srctauri_cookie_is_patched`: entries non-empty; every entry
     `>= (0, 18, 3)`.
  2. `srctauri_url_is_patched`: `url` entries non-empty and every one
     `>= (2, 5, 9)`; `form_urlencoded` entries non-empty and every one
     `>= (1, 2, 3)`.
  3. `srctauri_glib_decision_recorded`: either every `glib` entry is
     `>= (0, 20, 0)`, or the spec path exists and its text contains
     `RUSTSEC-2024-0429` and `gtk >= 0.19`.
  4. `webui_has_no_vulnerable_minimatch`: entries non-empty; no version in
     `[(7, 0, 0), (11, 0, 1))`; at least one `>= (11, 0, 1)`.
  5. `webui_has_no_vulnerable_zod`: entries non-empty; every version
     `>= (4, 6, 6)`.
- Failure messages list the offending package paths/versions so the RED output
  is evidence, not just a boolean.

- [x] **Step 2 complete:** target declared, file compiles conceptually, no
      network/subprocess calls.

#### Step 3: Run RED (eval first)

```bash
CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target \
  cargo test --locked --test dependency_integrity -- --test-threads=1
```

Expected:

```text
test srctauri_cookie_is_patched ... FAILED
test srctauri_url_is_patched ... FAILED
test srctauri_glib_decision_recorded ... ok
test webui_has_no_vulnerable_minimatch ... FAILED
test webui_has_no_vulnerable_zod ... FAILED
test result: FAILED. 1 passed; 4 failed; 0 ignored
```

Save the full output as RED evidence. If `srctauri_glib_decision_recorded`
fails, the spec is missing a C2 literal; fix Step 1, do not weaken the test.

- [x] **Step 3 complete:** exactly the four expected failures, exit non-zero.

#### Step 4: Capture the baseline audit

```bash
cargo audit --file src-tauri/Cargo.lock
echo "audit_exit=$?"
```

Expected: exit `1`; vulnerabilities `RUSTSEC-2026-0594` (`cookie 0.18.2`) and
`RUSTSEC-2026-0592` (`url 2.5.8`); settings summary
`2 vulnerabilities found!` and `3 allowed warnings found`.

- [x] **Step 4 complete:** baseline exit 1 and both advisory IDs captured.

#### Step 5: Apply the exact desktop lockfile updates

```bash
cargo update --manifest-path src-tauri/Cargo.toml -p cookie --precise 0.18.3
cargo update --manifest-path src-tauri/Cargo.toml -p url --precise 2.5.9
```

- [x] **Step 5 complete:** both commands exit 0.

#### Step 6: Verify the four-hunk envelope and package count

```bash
git diff --stat -- src-tauri/Cargo.lock
git diff -- src-tauri/Cargo.lock
cargo metadata --locked --manifest-path src-tauri/Cargo.toml --format-version 1 \
  | jq '.packages | length'
```

Expected:

- `git diff --stat` reports `1 file changed, 7 insertions(+), 7 deletions(-)`:
  seven line replacements across four hunks (no line is added or removed).
- Hunks touch only: `cookie` (version + checksum), `form_urlencoded` (version +
  checksum), `cssparser-macros` dependency line `"syn 3.0.10"` →
  `"syn 2.0.119"`, `url` (version + checksum).
- `jq` prints `501`.

If a fifth hunk appears or the count changes, stop and report the diff before
continuing; do not hand-edit the lockfile.

- [x] **Step 6 complete:** exact envelope, `501`.

#### Step 7: Run the GREEN gates

```bash
CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target \
  cargo test --locked --test dependency_integrity srctauri_ -- --test-threads=1

cargo audit --file src-tauri/Cargo.lock
echo "audit_exit=$?"

CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target-tauri \
  cargo check --locked --manifest-path src-tauri/Cargo.toml
```

Expected:

- `3 passed; 0 failed` (`srctauri_cookie_is_patched`,
  `srctauri_url_is_patched`, `srctauri_glib_decision_recorded`).
- Audit exit `0`; `0 vulnerabilities found`; `3 allowed warnings found`,
  including `RUSTSEC-2024-0429` / `glib 0.18.5` / `informational = "unsound"`.
- `cargo check` exits 0 (desktop crate compiles on macOS with the new
  lockfile).
- The two `webui_` tests remain RED by design; commit 2 owns them.

- [x] **Step 7 complete:** 3/3 desktop tests, audit exit 0, check exit 0.

#### Step 8: Local CI lint gates

```bash
rustfmt --edition 2021 --check tests/dependency_integrity.rs
git diff --check
```

Repo-wide `cargo fmt --check` is not a usable gate at `808e1304`: it already
flags unrelated drift in `src/main.rs` and `src/task/push_namespaces.rs`. The
new test must be rustfmt-clean on its own, and the staged quality gate remains
the authority for changed files. Stage the exact commit-1 file set, then run
the repository's staged quality gate (it applies rustfmt and Clippy to staged
Rust and skips unchanged languages):

```bash
git add Cargo.toml tests/dependency_integrity.rs src-tauri/Cargo.lock \
  docs/superpowers/specs/2026-11-24-issues-426-429-dependency-remediation-spec.md \
  docs/superpowers/plans/2026-11-24-issues-426-429-dependency-remediation-plan.md
bash scripts/check-staged-quality.sh
```

- [x] **Step 8 complete:** fmt clean, whitespace clean, staged quality exit 0.

#### Step 9: Commit 1

```bash
git commit -m "chore(deps): patch cookie and url in src-tauri lockfile and record #427 glib decision"
git rev-parse HEAD
git status --short
```

Expected: one commit on `chore/deps-426-429-advisories`; `git status --short`
is empty.

- [x] **Step 9 complete:** commit SHA captured, tree clean.

---

### Task 2: Web UI advisories, CI gates, permanent eval suite (#428/#429)

**Files:**

- Modify: `web-ui/package.json`
- Modify: `web-ui/package-lock.json`
- Modify: `.github/workflows/ci.yml`
- Modify: `eval/registry.json`

**Interfaces:**

- Produces: manifest floors `@tauri-apps/cli ^2.12.6`,
  `eslint-plugin-react-refresh ^0.5.8`, `typescript-eslint ^8.74.0`,
  `zod >= 4.6.6`.
- Produces: no lockfile `minimatch` entry in `[7.0.0, 11.0.1)` and at least
  one `>= 11.0.1`; every `zod` entry `>= 4.6.6`.
- Produces: `dependency-advisory` suite in `eval/registry.json` with the five
  C1–C3 test names.
- Produces: the `audit` (`src-tauri` metadata + advisory), `web-ui`, and
  `test`-job `dependency-advisory` CI gates from spec §6.

#### Step 1: Raise the manifest floors

Edit `web-ui/package.json` to the ranges in spec §5.1. Keep dependency
placement: `@tauri-apps/cli` and `eslint-plugin-react-refresh` and
`typescript-eslint` stay in `devDependencies`; add `zod` only if the resolved
graph needs a direct floor.

- [ ] **Step 1 complete:** ranges match the spec.

#### Step 2: Re-resolve and dedupe the lockfile

Use npm to regenerate rather than hand-editing `package-lock.json`:

```bash
npm install --prefix web-ui --package-lock-only
npm dedupe --prefix web-ui
```

Then inspect every match:

```bash
jq -r '.packages | to_entries[]
  | select(.key == "node_modules/minimatch" or (.key | endswith("/minimatch")))
  | "\(.key) \(.value.version)"' web-ui/package-lock.json

jq -r '.packages | to_entries[]
  | select(.key == "node_modules/zod" or (.key | endswith("/zod")))
  | "\(.key) \(.value.version)"' web-ui/package-lock.json
```

Expected: no `minimatch` below `11.0.1`; at least one `>= 11.0.1`; every `zod`
`>= 4.6.6`. If npm leaves a nested `10.x`, raise the parent
(`typescript-eslint`) or add an npm `overrides` entry for `minimatch` pinned to
`^11.0.1`, then re-run dedupe.

- [ ] **Step 2 complete:** both `jq` audits clean.

#### Step 3: Turn the web eval GREEN

```bash
CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target \
  cargo test --locked --test dependency_integrity webui_ -- --test-threads=1
```

Expected: `2 passed; 0 failed`.

- [ ] **Step 3 complete:** both web tests pass.

#### Step 4: Prove the install, lint, and build

```bash
npm ci --prefix web-ui
npm run lint --prefix web-ui
npm run build --prefix web-ui
```

Expected: all exit 0. `npm ci` must not rewrite the lockfile; confirm
`git diff --exit-code -- web-ui/package-lock.json` after it.

- [ ] **Step 4 complete:** install/lint/build green, lockfile stable.

#### Step 5: Add the CI jobs

Wire the CI gates into `.github/workflows/ci.yml` per spec §6:

- `audit` job on `ubuntu-latest` with `timeout-minutes: 20`, alongside the root
  `cargo audit`:
  `cargo metadata --locked --manifest-path src-tauri/Cargo.toml --format-version 1 > /dev/null`
  and `cargo audit --file src-tauri/Cargo.lock`. Neither step needs GTK/WebKit
  headers; `cargo check --locked --manifest-path src-tauri/Cargo.toml` stays the
  documented pre-merge verification on macOS.
- `web-ui` job on `ubuntu-latest` with `timeout-minutes: 15` and Node 22,
  caching `web-ui/package-lock.json`: `npm ci --prefix web-ui`,
  `npm run lint --prefix web-ui`, `npm run build --prefix web-ui`, and
  `npm audit --prefix web-ui --audit-level=high`.
- `test` job, after `scripts/test-remediation-eval.sh`:
  `bash scripts/remediation-eval.sh dependency-advisory`.

Pin action SHAs to the versions already used in the file. Do not add a new
audit ignore entry.

- [ ] **Step 5 complete:** YAML parses (`python3 -c 'import yaml,sys; yaml.safe_load(open(".github/workflows/ci.yml"))'`
      or `ruby -e 'require "yaml"; YAML.load_file(".github/workflows/ci.yml")'`),
      jobs present.

#### Step 6: Register the permanent eval suite

Add the `dependency-advisory` object from spec §6.3 to `eval/registry.json`
under `suites`, preserving JSON key order only where the file already groups
related suites. Validate:

```bash
jq -e '.suites["dependency-advisory"].tests | length == 5' eval/registry.json
jq -e '.suites["dependency-advisory"].thresholds.ignored == 0' eval/registry.json
```

- [ ] **Step 6 complete:** both `jq` checks exit 0.

#### Step 7: Run the registered suite end-to-end

```bash
git add web-ui/package.json web-ui/package-lock.json \
  .github/workflows/ci.yml eval/registry.json
git stash push --keep-index --include-untracked -m "eval-runner-clean-tree"
CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target \
  bash scripts/remediation-eval.sh dependency-advisory
git stash pop
```

`remediation-eval.sh` refuses a tracked dirty tree, hence the stash dance. If
the stash is risky in the current state, commit first and run the suite on the
clean commit instead; the verdict must be `PASS` with `passed == 5`,
`failed == 0`, `ignored == 0`.

- [ ] **Step 7 complete:** PASS JSON with five expected tests.

#### Step 8: Commit 2

```bash
git add web-ui/package.json web-ui/package-lock.json \
  .github/workflows/ci.yml eval/registry.json
git commit -m "chore(deps): clear web-ui advisories and gate dependency eval in CI"
git status --short
```

- [ ] **Step 8 complete:** second commit exists, tree clean.

---

## Final cross-checks (both commits)

```bash
CARGO_TARGET_DIR=/Users/neo/Git-SCM/ns-wt/deps-426-429/.cargo-target \
  cargo test --locked --test dependency_integrity -- --test-threads=1   # 5 passed
cargo audit --file src-tauri/Cargo.lock                                 # exit 0
cargo check --locked --manifest-path src-tauri/Cargo.toml               # exit 0
npm ci --prefix web-ui && npm run lint --prefix web-ui && npm run build --prefix web-ui
git log --oneline -2
git status --short
```

Expected: 5/5, audit exit 0 with the three visible warnings, desktop check 0,
web install/lint/build 0, exactly two commits, clean tree. Push remains
blocked until the owner authorizes it.
