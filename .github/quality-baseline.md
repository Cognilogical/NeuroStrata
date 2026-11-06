# Quality baseline

This file is the **one named place** where the pre-existing quality
backlog is recorded (panel row **J16**, Task 15a). The four quality jobs in
`ci.yml` — `fmt`, `clippy`, `audit`, `deny` — run **report-only**
(`continue-on-error`) against the counts recorded here. They count the
backlog; they do not block merges, and this task did not fix any of it.

Rules that keep this file honest:

- Counts shrink only when Task 15b lands a per-module cleanup slice
  upstream, with the updated counts in the same PR. A tree-wide sweep is
  never landed from a fork.
- A job is promoted from report-only to required only when its count here
  reaches zero.
- Allowlisted warning classes live **only** in this file and in the job
  configuration in `ci.yml` — never as scattered `#[allow]` attributes
  added silently. **At landing, the allowlist is empty: nothing is
  allowlisted.**

## Sizing evidence

- Base commit: `1c32496` (upstream `main`, v1.13.0).
- Sizing dispatch: CI run
  [40176094846](https://github.com/pratik-saptarshi/NeuroStrata/actions/runs/40176094846)
  on the fork, head `dd130b1` (the four jobs added in report-only mode,
  nothing else changed). Counts below are read from that run's job logs.
- Scope of the numbers: the root crate and the root `Cargo.lock`.
  `src-tauri` is a standalone crate with its own `Cargo.lock`; it is not
  covered by the audit/deny jobs' defaults. Its compilation is covered by
  the `gui` job instead.

## fmt — `cargo fmt --check`

**212 diff hunks across 38 files.** Counting rule: one finding per
`Diff in <file>:<line>` block in the `cargo fmt --check` output.

Per-file hunks (the Task 15b slice map, largest first): `src/server.rs`
44, `src/daemon.rs` 20, `src/buffer.rs` 20, `src/task/intake.rs` 15,
`src/synaptic_mesh/import.rs` 13, `src/store/ladybug.rs` 9,
`src/recall_metrics.rs` 6, `src/learning/dataset.rs` 6,
`src/judgment/mod.rs` 6, `src/task/mod.rs` 5, `src/main.rs` 5,
`src/learning/split.rs` 5, `src/handlers/bus_metrics.rs` 5,
`src/judgment/canonical.rs` 4, `src/events/subscribers/mesh_outbox.rs` 4,
`src/config.rs` 4, and 22 further files with 1–3 hunks each
(`src/learning/*`, `src/judgment/laya_onnx.rs`, `src/handlers/memory.rs`,
`src/events/bus.rs`, `src/guard/*`, `src/tools/procedure.rs`,
`src/registry.rs`, `src/traits.rs`, `tests/*`).

## clippy — `cargo clippy --locked -- -D warnings`

**210 diagnostics.** Counting rule: rustc's own tally in the job log
("could not compile `neurostrata-mcp` … due to 210 previous errors").

Lint classes named in the log's (deduplicated) notes — a floor, not a
census: rustc `unused-imports`, `unused-variables`, `unused-mut`,
`dead-code`; clippy `while-let-on-iterator`, `useless-format`,
`unwrap-or-default`, `unnecessary-sort-by`, `unnecessary-map-or`,
`type-complexity`, `too-many-arguments`, `redundant-pattern-matching`,
`redundant-closure`, `needless-borrows-for-generic-args`, and more of the
`needless-*` / `redundant-*` / `unnecessary-*` families.

## audit — `cargo audit` (RustSec advisory database)

**9 vulnerabilities** in the root lockfile's 392 dependencies, plus
**4 informational warnings**. Counting rule: the `cargo audit` JSON
report embedded in the job log.

| Advisory | Package | Version |
|---|---|---|
| RUSTSEC-2026-0204 | crossbeam-epoch | 0.9.18 |
| RUSTSEC-2026-0258 | h2 | 0.4.13 |
| RUSTSEC-2026-0285 | rustls | 0.23.39 |
| RUSTSEC-2026-0346 | rustls | 0.23.39 |
| RUSTSEC-2026-0348 | rustls | 0.23.39 |
| RUSTSEC-2026-0349 | rustls | 0.23.39 |
| RUSTSEC-2026-0382 | rustls-webpki | 0.103.13 |
| RUSTSEC-2026-0383 | webpki-root-certs | 1.0.9 |
| RUSTSEC-2026-0381 | webpki-roots | 1.0.7 |

Informational warnings: 1 unmaintained (paste 1.0.15,
RUSTSEC-2024-0436); 3 unsound (anyhow 1.0.102 RUSTSEC-2026-0190,
cxx 1.0.138 RUSTSEC-2026-0202, tokio 1.53.1 RUSTSEC-2026-0386).

## deny — `cargo deny check`

**371 errors, 12 warnings** — under cargo-deny's **default rules**,
because the tree has no `deny.toml`. Counting rule: diagnostic header
lines in the job log, grouped by class.

| Class | Count | Note |
|---|---|---|
| `error[rejected]` (licenses) | 358 | An artifact of having **no license policy configured**: the defaults reject every crate whose license is not in the small default allow set. This is a policy question for the maintainer — which licenses this project accepts — recorded here, **not** allowlisted by this task. Writing a `deny.toml` is the follow-up that turns this row into a real signal. |
| `error[vulnerability]` | 9 | The same 9 advisories as the audit job. |
| `error[unsound]` | 3 | The same 3 as audit's informational warnings (deny reports them at error level). |
| `error[unmaintained]` | 1 | paste 1.0.15, as audit. |
| `warning[duplicate]` | 11 | Crates present at multiple versions. |
| `warning[no-license-field]` | 1 | A crate with no license field. |

## Test-matrix findings — first 3-OS run

Run [40177120604](https://github.com/pratik-saptarshi/NeuroStrata/actions/runs/40177120604)
(head `ddbfa0d`). The suite had only ever run on Ubuntu; the new matrix
exposed pre-existing platform failures on the other two legs. These are
**bugs, not style backlog** — each needs its own follow-up issue, and
none of them is Task 15b's to burn down. Until they are fixed, the
Windows and macOS legs run with `report-only` set in the matrix (the
flag comes off a leg when its count here reaches zero); the Ubuntu leg
is green and remains the hard gate.

- **Windows — the test suite does not compile** (7 errors, all in test
  code using Unix-only APIs unguarded): `std::os::unix` at
  `src/synaptic_mesh/import.rs:1731` and `:1732`, `src/main.rs:1166`;
  `Permissions::set_mode` at `src/synaptic_mesh/import.rs:2034`;
  `Permissions::from_mode` / `.mode()` at `src/main.rs:1178`, `:1182`,
  `:1185`. The production binary compiles on Windows (the `build` job
  is green there); only the tests are Unix-bound.
- **macOS — 780 passed, 3 failed**, all platform-assumption failures:
  `buffer::tests::append_log_normal_path_appends` (expects the buffer
  file under the project root; macOS resolves the temp dir through
  `/private/var/folders/…`), `crash::tests::a_stack_overflow_on_a_worker_thread_leaves_a_breadcrumb`
  (expects the child to die of SIGSEGV, signal 11; macOS delivers
  SIGBUS, signal 10), and
  `store::ladybug::tests::export_graph_resolves_each_node_by_its_source_identity`
  (stored source path vs the `/var/folders/…` temp path — the same
  macOS canonicalization family).

## GUI-job finding — the GUI does not build from a clean checkout

The `gui` job's first run (same run as above) failed in the web-ui
build, before any Rust compiled: `web-ui/public/graph.json` is a
**tracked symlink** (git mode 120000) to
`../../.NeuroStrata/graph/graph.json` — a generated directory that does
not exist in a fresh clone — so vite's public-directory copy stats it
and dies: `ENOENT: no such file or directory, stat
'…/web-ui/public/graph.json'`. The production frontend build has
therefore never been reproducible from a clean checkout; it only works
on a machine where a graph export has populated `.NeuroStrata/`.

This is a **bug** (its own follow-up issue), not backlog. Consequences
recorded honestly: the `gui` job stays **report-only** until the bug is
fixed, and the Tauri **Rust** compile behind the web-ui step remains
**unverified in CI** — the cargo step has never run. When the symlink
bug is fixed, the job's `continue-on-error` comes off in the same
change that re-runs it green.

## Also landed by Task 15a (not backlog)

- The `test` job runs the 3-OS matrix (ubuntu-latest, windows-latest,
  macos-15). Ubuntu gates; the other two legs are report-only against
  the findings above — tests are behavior, not backlog.
- The `gui` job builds the Tauri app (`src-tauri`) in CI for the first
  time; report-only against the finding above.
- `release.yml` (J17): top-level `permissions: contents: read`, with
  `contents: write` (+ `attestations: write`, `id-token: write`) granted
  only to the release job; every action in all three workflows pinned to
  a full commit SHA; build-provenance attestation over the published
  artifacts. **Verification status: the grep-level governance assertion
  runs in CI as the hard-gated `release-governance` job in `ci.yml`
  (no `continue-on-error`: a governance regression fails the run); the
  draft-release rehearsal that would
  exercise `release.yml` end-to-end is NOT run — it awaits Pratik's
  authorization, and Task 15a parks on that leg rather than claiming
  inspection as verification.**
