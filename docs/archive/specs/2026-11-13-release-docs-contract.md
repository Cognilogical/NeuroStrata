# Release, docs, and operations — contract (WP2.5)

Status: Phase 2 / WP2.5.

## Contracts

### C1 — the changelog names the remediation

`CHANGELOG.md` has an `[Unreleased]` section covering the scanner, ingest,
lexical, hygiene, metadata-authorization, guard and retention changes, plus an
"Upgrading" note pointing at `docs/ops/upgrading.md`. The ≤1.17.x reparse
section (disk 2×, pool, rollback, `files_skipped`) is already there.

### C2 — a release verifies its version and its tests before publishing

`release.yml` gains a `verify` job that runs before any platform build:

1. the tag (`vX.Y.Z`) must equal the package version in `Cargo.toml`;
2. the binary test suite must pass (`cargo test --locked --bin
   neurostrata-mcp -- --test-threads=1`), and a `test-receipt.txt` record is
   uploaded as an artifact.

`build` needs `verify`, so a mismatched tag or a red suite cannot publish
assets.

### C3 — the memory-profile knob appears in the service templates

The generated systemd unit carries a commented `Environment=NEUROSTRATA_PROFILE=1`
example; the launchd agent carries an explicit `NEUROSTRATA_PROFILE` (0) in
`EnvironmentVariables`; the NSSM install records `AppEnvironmentExtra
NEUROSTRATA_PROFILE=0`. Profile sampling stays off by default.

### C4 — growth-log write errors are surfaced

`procmem::append` returns `io::Result`; both call sites warn with the path and
error. A failed rotation warns but still attempts the append. A log the process
cannot write is a missing diagnostic, not a silent no-op.

## Tests

| Test | Contract |
|---|---|
| `daemon_unit_template_names_the_profile_knob` | C3 |
| `macos_plist_names_the_profile_knob` (macOS) | C3 |
| `growth_log_write_failure_is_surfaced` | C4 |

## Acceptance

- A tag that does not match `Cargo.toml` fails before builds; the test receipt
  artifact exists.
- Profile sampling is discoverable from every installed service and still off.
- An unwritable growth log returns an error and is warned about.
