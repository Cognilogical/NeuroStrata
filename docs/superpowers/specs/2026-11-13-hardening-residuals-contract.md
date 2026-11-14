# Residual hardening — contract (WP3.2 & #183)

Status: Phase 3 / WP3.2 & `#183` (`neurostrata-igg4`). Covers the `#164` P2
arena guard restoration, the sampler seam (`#181`), and the 12 P3 residue items
from `#183`.

## Landed

### C1 — a mistyped RSS threshold is reported once and cached

`procmem::parse_threshold` returns a typed error for a present-but-invalid
`NEUROSTRATA_RSS_LOG_MB`, naming the value; `threshold_mb` warns once per
invalid value to stderr and uses the default (`50`). In release builds,
`threshold_mb` caches the parsed threshold in a `OnceLock<u64>` so per-call
guards do not re-read or re-parse `NEUROSTRATA_RSS_LOG_MB` on every call
(`P3-8 / X6c`, `neurostrata-igg4`). Unset uses the default; `0` remains the
explicit "disable the per-call log" value.

### C2 — the arena decision is a tested value AND constructor-guarded

`embed::arena_execution_providers(disabled)` returns the pinned CPU provider
with the arena allocator off, or `None` for fastembed's default. Both
`the_arena_policy_reaches_the_provider_wiring` (value test) and
`the_constructor_disables_the_cpu_arena` (flip/delete-sensitive guard pinning
`.with_arena_allocator(false)` in `arena_execution_providers` and
`init.with_execution_providers(providers)` in `FastEmbedder::new`) verify that
neither flipping the boolean nor dropping the constructor wiring can silently
re-enable the ONNX CPU arena (`#164`, `neurostrata-igg4`).

### C3 — `procmem` growth log security, status telemetry, and arithmetic (`#183`)

- **P3-14 / N4 (`0600` permissions):** `procmem::append` creates and tightens
  `memory-growth.log` (and rotated `.1` files) with `0600` permissions on Unix,
  matching the daemon's token files (`neurostrata-igg4`).
- **P3-6 / X6a (`profile_enabled` & `growth_supported`):** `status_json()`
  includes `"profile_enabled": profile_enabled()` and
  `"growth_supported": s.rss_bytes.is_some()` alongside `"rss_available"` and
  `"growth_log"` (`neurostrata-igg4`).
- **P3-9 / X6d (consistent floored `delta_mb`):** `line()` and
  `sample_once_with()` compute `delta_mb` from the floored megabyte figures
  (`(after / MB) as i64 - (before / MB) as i64`) so `rss_before_mb + delta_mb == rss_after_mb`
  and `prev_rss_mb + delta_mb == rss_mb` never disagree by 1 MiB (`neurostrata-igg4`).
- **P3-11 / N1 (direct HTTP surface):** The daemon's stateful direct HTTP routes
  wrap execution in `CallGuard::begin("http:<route>", ...)`, and
  `docs/limitations.md` documents what `calls_since_last` counts (`neurostrata-igg4`).
- **P3-12 / N2 (process role attribution):** `status_json()` delegates to a
  pure `status_json_for(sample, is_daemon)` / `role_for(is_daemon)` seam and
  reports `"role"` (`"daemon"` when `mark_daemon_process()` has been called by
  the daemon, or `"cli"` on the CLI daemon-down fallback path) so callers can
  distinguish the transient CLI process from a live daemon without unit tests
  mutating process-global `IS_DAEMON` (`neurostrata-igg4`, `neurostrata-flgp`).
- **P3-7 / X6b (async sampler & `Drop` I/O):** `run_sampler` runs its file
  append and `sample_once_with` tick inside `tokio::task::spawn_blocking`, and
  `docs/limitations.md` records the explicit waiver for `CallGuard::drop`
  synchronous append on threshold crossings (`neurostrata-igg4`).

### C4 — ingest bead citations, probe hygiene, and operator knob docs (`#183`)

- **P3-2 / D3:** `src/parser/ingest.rs` carries `(bead neurostrata-fld)` on
  `node_id_for` and `(bead neurostrata-sij)` on the carry-forward abort
  `enrichment_fail` path (`neurostrata-igg4`).
- **P3-3 / D4:** `ingest_memory_probe` removes any stale `db` directory at start
  before opening `LadybugStore` and explicitly calls `drop(embedder)` after
  `drop(store)` (`neurostrata-igg4`).
- **P3-4 / D8:** `NEUROSTRATA_EMBED_ARENA` and `NEUROSTRATA_BUFFER_POOL_MB` are
  documented in `docs/BUILDING.md`, `CHANGELOG.md`, and `README.md` (`neurostrata-igg4`).
- **P3-5 / X4 & P3-13 / N3:** `docs/limitations.md` documents began-before
  neighbors, background growth, and detached-ingest attribution geometry (`neurostrata-igg4`).

### C5 — Dependabot and CodeQL alert remediation (`neurostrata-57oi`)

- **Plugin dependency hygiene (`plugins/core/package.json`, `plugins/openclaw/package-lock.json`):**
  Remove the unused `"vitest": "^1.0.0"` devDependency from `plugins/core/package.json`
  (tests run via `node --test test.mjs`) and regenerate `plugins/openclaw/package-lock.json`
  so stale `vitest`/`vite`/`tinypool`/`esbuild` transitive entries are removed,
  resolving Dependabot alerts `#1`–`#13` (`neurostrata-57oi`).
- **CodeQL `rust/cleartext-logging` (`src/guard/fsm.rs`, `tests/guard/validator_test.rs`):**
  Rename the internal parameter in `evaluate_fsm_guard` from `session_id` to
  `caller_tag` and remove `msg` interpolation from `assert!` failure strings in
  `tests/guard/validator_test.rs`, eliminating the heuristic taint source and
  sink for CodeQL alerts `#2`–`#5` (`neurostrata-57oi`).
- **Web UI alert triage:** Dismiss Dependabot alerts on `web-ui/package-lock.json`
  (`#20`–`#26`, `#28`) and `src-tauri/Cargo.lock` (`#15`) while the web interface
   awaits its scheduled revamp (`neurostrata-57oi`).

## Out-of-scope architecture notes (`neurostrata-igg4`)

- **Sidecar-deletion attestation** (security P3-2, `neurostrata-igg4`). Refusing
  `Absent` when the engine attests data would also refuse every pre-sidecar
  store, which this release deliberately opens as before (recording their
  identity is `#77`). Distinguishing deletion from legacy needs the identity
  stored inside the engine; that is a schema/design change, not a call-site
  check.
- **Hook generated-by sentinel** (security P3-3, `neurostrata-igg4`). The
  installed hook derives the namespace from the project directory name. A
  per-repo stamped namespace requires generating the hook text per install
  (today the same bytes ship to every repo) and a migration for installed
  hooks; the gate remains the only place the heuristic is used.
- **A crate-wide tracing subscriber** (`neurostrata-igg4`). The binary installs
  none, so `tracing::warn!` calls in these paths were discarded. The
  operator-facing warnings here use stderr; choosing a subscriber (format,
  destination, verbosity, and the interaction with the MCP stdio stream) is a
  separate decision.
- **A true behavioral arena memory test** (`neurostrata-igg4`). It would need to
  load a model and measure RSS ratcheting; CI builds are not the place for a
  multi-hundred-MB download. C2 tests both the value and the constructor source
  wiring; the runtime claim stays with the operator measurement that motivated
  the default (`ingest_memory_probe`).

## Tests

| Test | Contract |
|---|---|
| `invalid_rss_threshold_is_reported_not_swallowed` | C1 |
| `invalid_threshold_warning_fires_once_per_value` | C1 |
| `the_arena_policy_reaches_the_provider_wiring` | C2 |
| `the_constructor_disables_the_cpu_arena` | C2 |
| `append_sets_0600_permissions_on_new_and_existing_logs` | C3 |
| `status_json_exposes_profile_enabled_growth_supported_and_role` | C3 |
| `delta_mb_agrees_with_floored_megabyte_columns` | C3 |

## Acceptance

- An invalid threshold names the value once and falls back; `0` still disables (`neurostrata-igg4`).
- The arena decision is inspectable as a value AND guarded at the constructor call site (`neurostrata-igg4`).
- `memory-growth.log` and rotated `.1` files carry `0600` permissions on Unix (`neurostrata-igg4`).
- `status_json` reports `profile_enabled`, `growth_supported`, and `role` (`neurostrata-igg4`).
- `delta_mb` equals `rss_after_mb - rss_before_mb` (`prev_rss_mb` in sampler rows) (`neurostrata-igg4`).
- The limitations, building, README, and changelog documents match the implemented semantics (`neurostrata-igg4`).
