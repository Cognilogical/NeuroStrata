# 2026-11-24 — Dependency Advisory Remediation (#426–#429)

> **Baseline**: `cognilogical/main` @ `808e1304` (release v1.19.1)
> **Issues**: #426 (P1), #427 (P1), #428 (P2), #429 (P2)
> **Branch**: `chore/deps-426-429-advisories` > **Workflow**: contracts → offline eval RED → lockfile/package remediation →
> eval GREEN → CI and registered-suite gates

---

## 1. Scope

Four dependency-advisory findings are closed together because they share one
verification surface and one rollback surface:

| Issue     | Ecosystem      | File                                              | Baseline offender                   | Fixed floor                                      |
| --------- | -------------- | ------------------------------------------------- | ----------------------------------- | ------------------------------------------------ |
| #426      | Rust (desktop) | `src-tauri/Cargo.lock`                            | `cookie 0.18.2`, `url 2.5.8`        | `cookie >= 0.18.3`, `url >= 2.5.9`               |
| #427      | Rust (desktop) | `src-tauri/Cargo.lock` + this record              | `glib 0.18.5` (`RUSTSEC-2024-0429`) | blocked upstream; dated decision                 |
| #428/#429 | npm (web-ui)   | `web-ui/package.json`, `web-ui/package-lock.json` | nested `minimatch 10.2.6`           | `minimatch >= 11.0.1`, no nested vulnerable copy |
| #428/#429 | npm (web-ui)   | `web-ui/package.json`, `web-ui/package-lock.json` | `zod 4.6.5`                         | `zod >= 4.6.6`                                   |

Non-goals:

- No application-source changes. This work touches lockfiles, two dependency
  manifests, one offline integration test, CI, and the eval registry only.
- No `src-tauri/Cargo.toml` version-range changes. The desktop crate keeps its
  declared ranges; only the lock resolution moves.
- No upgrade of `tauri`, `wry`, `webkit2gtk`, `gtk`, or `glib`. C2 explains why
  that upgrade cannot be selected in this graph.
- No push, release, or tag in this phase.

---

## 2. Baseline evidence

Commands run in the isolated worktree on 2026-11-24 against `808e1304`:

```text
$ cargo audit --file src-tauri/Cargo.lock
error: 2 vulnerabilities found!
warning: 3 allowed warnings found
$ echo $?
1
```

- `RUSTSEC-2026-0594` — `cookie 0.18.2`, patched in `0.18.3`.
- `RUSTSEC-2026-0592` — `url 2.5.8`, patched in `2.5.9`.
- Warning (exit-0, kept visible): `RUSTSEC-2024-0429` — `glib 0.18.5`,
  `informational = "unsound"`.
- Warning (exit-0, kept visible): `RUSTSEC-2026-0543` — `chrono 0.4.45`
  unmaintained.
- Warning (exit-0, kept visible): `RUSTSEC-2024-0370` — `proc-macro-error 1.0.4`
  unmaintained.

The root `Cargo.lock` was already patched by `2dbe59ac`; the desktop crate is a
separate, excluded workspace with its own lockfile and was not covered. The
`web-ui` graph still carries `node_modules/minimatch` `11.0.8` at the top level
but a nested `node_modules/@typescript-eslint/typescript-estree/node_modules/minimatch`
at `10.2.6`, plus `node_modules/zod` at `4.6.5`.

---

## 3. Contract C1 — Desktop lockfile patch (#426)

### 3.1 Inputs

- `src-tauri/Cargo.lock` at `808e1304`: 501 `[[package]]` entries; `cookie`
  `0.18.2`, `url` `2.5.8`, `form_urlencoded` `1.2.2`.
- `src-tauri/Cargo.toml` unchanged: `tauri = "2.12.5"`, `reqwest = "0.13.5"`.
- Resolver-legal because `tauri 2.12.5`/`2.12.7` require `cookie ^0.18` and
  `reqwest 0.13.5` requires `url ^2.5`.

### 3.2 Operation

Exactly these two commands, no `cargo update` without `--precise`, no manifest
edit:

```bash
cargo update --manifest-path src-tauri/Cargo.toml -p cookie --precise 0.18.3
cargo update --manifest-path src-tauri/Cargo.toml -p url --precise 2.5.9
```

### 3.3 Postconditions

1. Every `cookie` package entry is `>= (0, 18, 3)`; the resolved entry is
   exactly `0.18.3`.
2. Every `url` package entry is `>= (2, 5, 9)`; the resolved entry is exactly
   `2.5.9`.
3. Every `form_urlencoded` package entry is `>= (1, 2, 3)`; the resolved entry
   is exactly `1.2.3` (transitive bump from `url 2.5.9`).
4. The lockfile diff is exactly four hunks and nothing else:
   - the `cookie` version/checksum block;
   - the `form_urlencoded` version/checksum block;
   - the `cssparser-macros 0.7.1` dependency edge moving from
     `syn 3.0.10` to `syn 2.0.119`;
   - the `url` version/checksum block.
5. The `cssparser-macros` edge flip is unavoidable and resolver-legal:
   `cssparser-macros 0.7.1` declares `syn = ">=2, <4"`, so the update may select
   either `syn 2.0.119` or `syn 3.0.10`. Cargo re-resolves the edge while
   updating `url`; both `syn` versions remain in the lockfile for other
   consumers, so no package is added, removed, or downgraded.
6. Package count remains **501**:
   `cargo metadata --locked --manifest-path src-tauri/Cargo.toml --format-version 1 | jq '.packages | length'`
   prints `501` and exits 0.
7. `cargo audit --file src-tauri/Cargo.lock` exits **0** with
   `0 vulnerabilities` and `3 allowed warnings` (the C2 `glib` warning plus the
   two existing unmaintained warnings).
8. `cargo check --locked --manifest-path src-tauri/Cargo.toml` exits **0** on
   macOS with the updated lockfile.
9. `cargo test --locked --test dependency_integrity srctauri_ -- --test-threads=1`
   reports `3 passed; 0 failed`.

### 3.4 Rejection criteria

Reject the change if any of the following is true: package count differs from
501; a package other than the three named crates changes version/checksum; the
`cssparser-macros` edge is the only diff but the security versions did not move;
`cargo metadata --locked` fails; audit exits non-zero; `cargo check` fails; or
the diff contains a manifest change.

---

## 4. Contract C2 — `glib` decision record (#427)

### 4.1 Advisory

- ID: **RUSTSEC-2024-0429**
- Crate/version in graph: **`glib 0.18.5`**
- Kind: `informational = "unsound"` (not a vulnerability; `cargo audit` exits 0
  for it and reports it as a warning)
- Patched floor: `glib >= 0.20.0`
- Subject: unsound `Iterator` / `DoubleEndedIterator` implementations for
  `glib::VariantStrIter`.

### 4.2 Upstream constraint matrix (verified 2026-11-24)

The matrix was read from the published crate manifests, not inferred from the
lockfile:

| Upstream crate | Version examined                         | Linux-relevant constraint                     |
| -------------- | ---------------------------------------- | --------------------------------------------- |
| `tauri`        | `2.12.7` (latest 2.x)                    | `gtk = "0.18"`; `webkit2gtk = "2"` (optional) |
| `tauri`        | `3.0.0-alpha.6` (latest pre-release)     | `gtk = "0.18"` (optional)                     |
| `wry`          | `0.57.4` (latest 0.57.x)                 | `gtk = "0.18"`; `webkit2gtk = "=2.0.2"`       |
| `webkit2gtk`   | `2.0.2` (the only version in this graph) | `glib = "^0.18.0"`; `gtk = "^0.18.0"`         |

Consequences:

- `webkit2gtk 2.0.2` is pinned exactly by `wry 0.57.x`.
- `webkit2gtk 2.0.2` requires `gtk ^0.18`, and `gtk 0.18.x` requires
  `glib ^0.18`. A `glib >= 0.20` selection therefore requires a `gtk >= 0.19`
  consumer.
- Both the current Tauri 2 line and the published Tauri 3 alpha still declare
  `gtk 0.18`; no released `tauri`/`wry` pair accepts `gtk >= 0.19`.
- Pinning `glib` alone is impossible: Cargo unifies the `glib` major/minor
  requirement across `gtk`, `webkit2gtk`, and the Tauri stack.

### 4.3 Decision (dated 2026-11-24)

**Keep `glib 0.18.5`; accept RUSTSEC-2024-0429 as a visible warning.** Do not
add an `.cargo/audit.toml` ignore entry. Repo convention is that only real
vulnerabilities may be suppressed with a dated exception; informational and
unmaintained warnings stay in the report so the next reviewer sees them. The
warning is `informational = "unsound"`, so `cargo audit` remains exit 0 once
the two vulnerabilities are removed.

This decision is re-evaluated — not renewed by calendar alone — on any of:

1. a `tauri` or `wry` release whose Linux target accepts **`gtk >= 0.19`**;
2. a `webkit2gtk` release that permits `glib >= 0.20`;
3. any `cargo audit` change that reclassifies RUSTSEC-2024-0429 from a warning
   to a vulnerability.

Upstream review trigger, verbatim: **"when tauri/wry accept gtk >= 0.19"**.

The regression test `srctauri_glib_decision_recorded` enforces this record: it
passes when every `glib` entry is `>= 0.20.0`, or when this file exists and
contains both `RUSTSEC-2024-0429` and `gtk >= 0.19`.

---

## 5. Contract C3 — Web UI advisories (#428, #429, `zod`)

### 5.1 Manifest targets

`web-ui/package.json` must declare or resolve:

| Package                       | Required range |
| ----------------------------- | -------------- |
| `@tauri-apps/cli`             | `^2.12.6`      |
| `eslint-plugin-react-refresh` | `^0.5.8`       |
| `typescript-eslint`           | `^8.74.0`      |
| `zod`                         | `>= 4.6.6`     |

`zod` may stay transitive if the parent bump resolves it; if it is declared
directly it takes the `>= 4.6.6` floor. The lockfile is the authority.

### 5.2 Lockfile postconditions

1. Every package entry in `web-ui/package-lock.json` whose path is
   `node_modules/minimatch` or ends in `/minimatch` has version
   `>= (11, 0, 1)`. No entry may be in `[(7, 0, 0), (11, 0, 1))`.
   At least one such entry must exist.
   - Baseline violation: `node_modules/@typescript-eslint/typescript-estree/node_modules/minimatch`
     at `10.2.6`.
   - The nested copy must be deduped away or raised; a top-level `11.x`
     alongside a nested `10.x` is a failure.
2. Every package entry whose path is `node_modules/zod` or ends in `/zod` has
   version `>= (4, 6, 6)`. Baseline violation: `4.6.5`.
3. `npm ci --prefix web-ui` exits 0 against the committed lockfile.
4. `npm run lint --prefix web-ui` exits 0.
5. `npm run build --prefix web-ui` exits 0.
6. No `web-ui/src` change is part of the dependency commit.

### 5.3 Ordering note

C3 is implemented in commit 2. Commit 1 deliberately leaves the two `web-ui`
tests RED; that is the eval-first proof that they detect the baseline and are
not vacuous.

---

## 6. Contract C4 — CI gates and the permanent eval suite

### 6.1 `src-tauri` gate

The `audit` CI job (`ubuntu-latest`, `timeout-minutes: 20`) checks the excluded
desktop workspace alongside the root graph:

```bash
cargo metadata --locked --manifest-path src-tauri/Cargo.toml --format-version 1 > /dev/null
cargo audit --file src-tauri/Cargo.lock
```

Neither step needs GTK/WebKit system headers: `cargo metadata` only resolves the
committed lockfile, and `cargo audit` reads `src-tauri/Cargo.lock` as data. The
desktop compile is therefore not a CI step; `cargo check --locked --manifest-path
src-tauri/Cargo.toml` is the documented pre-merge verification on macOS, where
the desktop toolchain and system libraries are available.

The audit uses the same `.cargo/audit.toml` as the root gate; no new ignore
entry is added for C2.

### 6.2 `web-ui` gate

The `web-ui` CI job (`ubuntu-latest`, `timeout-minutes: 15`, Node 22) runs:

```bash
npm ci --prefix web-ui
npm run lint --prefix web-ui
npm run build --prefix web-ui
npm audit --prefix web-ui --audit-level=high
```

The cache key includes `web-ui/package-lock.json`; `npm ci` is mandatory so a
lockfile that does not match `package.json` fails instead of being rewritten.
The audit gate fails only on high/critical advisories.

### 6.3 Registered `dependency-advisory` suite

`eval/registry.json` gains a permanent suite:

```json
"dependency-advisory": {
  "commands": [
    "cargo test --locked --test dependency_integrity -- --test-threads=1"
  ],
  "tests": [
    "srctauri_cookie_is_patched",
    "srctauri_url_is_patched",
    "srctauri_glib_decision_recorded",
    "webui_has_no_vulnerable_minimatch",
    "webui_has_no_vulnerable_zod"
  ],
  "thresholds": {
    "ignored": 0
  }
}
```

The `test` CI job invokes `bash scripts/remediation-eval.sh dependency-advisory`
so a renamed or silently skipped test cannot pass. The suite is offline: it reads only the
two committed lockfiles and this spec.

### 6.4 Test-to-contract map

| Test                                | Contract   | Baseline RED reason                                          |
| ----------------------------------- | ---------- | ------------------------------------------------------------ |
| `srctauri_cookie_is_patched`        | C1.1       | `cookie 0.18.2 < 0.18.3`                                     |
| `srctauri_url_is_patched`           | C1.2, C1.3 | `url 2.5.8 < 2.5.9`; `form_urlencoded 1.2.2 < 1.2.3`         |
| `srctauri_glib_decision_recorded`   | C2.3       | Passes only because this record exists and names the trigger |
| `webui_has_no_vulnerable_minimatch` | C3.2.1     | nested `minimatch 10.2.6`                                    |
| `webui_has_no_vulnerable_zod`       | C3.2.2     | `zod 4.6.5`                                                  |

---

## 7. Eval-first implementation rules

1. The test target is declared in `Cargo.toml` and committed with the spec, so
   the RED evidence and the contract live in the same commit.
2. Version comparison is numeric semver tuple comparison, never string
   comparison (`"0.18.10" < "0.18.9"` is false lexically but true numerically).
3. The test performs no network I/O, invokes no cargo subcommand, and is
   deterministic across macOS/Linux/Windows.
4. Paths are resolved with `CARGO_MANIFEST_DIR`, so the test runs from any
   working directory.
5. `--test-threads=1` matches the repo's test convention and keeps failure
   output ordered.

---

## 8. Rollback

Commit 1 is a single revert: restore `src-tauri/Cargo.lock`, drop the test
target, test file, and this spec/plan. Commit 2 is a single revert of
`web-ui/package.json`, `web-ui/package-lock.json`, CI, and the registry entry.
Neither commit changes runtime code, so a partial rollback cannot corrupt data
or the MCP wire contract.

---

## 9. Acceptance summary

- [ ] C1: exact four-hunk desktop lockfile diff, 501 packages, audit exit 0,
      `cargo check` exit 0, 3/3 `srctauri_` tests.
- [ ] C2: dated `glib` decision with the full matrix and the upstream trigger;
      warning remains visible; `srctauri_glib_decision_recorded` passes.
- [ ] C3: manifest floors landed; no vulnerable `minimatch` or `zod`; lint and
      build green.
- [ ] C4: `src-tauri`, `web-ui`, and `dependency-advisory` gates run in CI.
