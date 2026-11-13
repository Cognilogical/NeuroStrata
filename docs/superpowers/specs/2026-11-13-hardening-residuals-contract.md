# Residual hardening — contract (WP3.2)

Status: Phase 3 / WP3.2. Covers the tractable P3s from the panel's hardening
follow-up and records the deferred ones with a ruling.

## Landed

### C1 — a mistyped RSS threshold is reported

`procmem::parse_threshold` returns a typed error for a present-but-invalid
`NEUROSTRATA_RSS_LOG_MB`, naming the value; `threshold_mb` warns and uses the
default. Unset uses the default; `0` remains the explicit "disable the per-call
log" value.

### C2 — the arena decision is a tested value

`embed::arena_execution_providers(disabled)` returns the pinned CPU provider
with the arena allocator off, or `None` for fastembed's default. The
constructor consumes it, so the env parse, the decision and the provider wiring
are one path a test can inspect without loading a model. The old
source-substring assertion is gone.

### C3 — accepted limitations are recorded

`docs/limitations.md` states the `hit_count` semantics (bookkeeping, failed
increment does not change a verdict, snapshot reads), the growth log's
`approximate: true` attribution, `incomplete` search pages, and the fact that
branch protection is a repository setting.

## Deferred with a ruling

- **Sidecar-deletion attestation** (security P3-2). Refusing `Absent` when the
  engine attests data would also refuse every pre-sidecar store, which this
  release deliberately opens as before (recording their identity is #77).
  Distinguishing deletion from legacy needs the identity stored inside the
  engine; that is a schema/design change, not a call-site check.
- **Hook generated-by sentinel** (security P3-3). The installed hook derives
  the namespace from the project directory name. A per-repo stamped namespace
  requires generating the hook text per install (today the same bytes ship to
  every repo) and a migration for installed hooks. Deferred as a design
  change; the gate remains the only place the heuristic is used.
- **A crate-wide tracing subscriber.** The binary installs none, so
  `tracing::warn!` calls in these paths were discarded. The operator-facing
  warnings here use stderr; choosing a subscriber (format, destination,
  verbosity, and the interaction with the MCP stdio stream) is a separate
  decision.
- **A true behavioral arena memory test.** It would need to load a model and
  measure RSS ratcheting; CI builds are not the place for a multi-hundred-MB
  download. C2 tests the wiring; the runtime claim stays with the operator
  measurement that motivated the default.

## Tests

| Test | Contract |
|---|---|
| `invalid_rss_threshold_is_reported_not_swallowed` | C1 |
| `the_arena_policy_reaches_the_provider_wiring` | C2 |

## Acceptance

- An invalid threshold names the value and falls back; `0` still disables.
- The arena decision is inspectable as a value, not text.
- The limitations document matches the implemented semantics.
