# Accepted limitations

These are deliberate, documented trade-offs. They are not defects waiting to
be fixed; a change here needs a design decision and a test that replaces the
one named below.

## `hit_count` is bookkeeping, not a verdict

`guard` increments a rule's persisted `hit_count` when the rule fires. The
increment is one store write turn, so concurrent firings serialize and no
update is lost (`concurrent_hit_counts_are_not_lost`).

- A failed increment is reported through `tracing::warn!` and does not change
  the verdict: this binary installs no tracing subscriber (the same
  disclosure made below for the RSS knob), so the warning is discarded and in
  practice the failure is not recorded anywhere. The guard's job is the
  pass/reject decision, and a bookkeeping write failure must not turn a pass
  into a rejection (or vice versa).
- The count answers "how often did this rule fire", not "how often did this
  rule decide the outcome". Several rules can fire on one action; the first
  deterministic reject decides.
- Reading `hit_count` is a snapshot. Two readers can see counts taken at
  different turns; the store does not offer a read barrier across calls.

## `approximate: true` on a memory-growth line

`procmem` wraps one tool call and logs the resident-set delta when the process
grew past `NEUROSTRATA_RSS_LOG_MB`. Calls run concurrently, so a delta can
include allocation from a neighboring call that began inside the window. When
`in_flight > 1` the line says `"approximate": true`; the figure is still the
process delta, but the attribution to that call is not exact.

- **P3-5 / X4 — began-before & background growth (`neurostrata-igg4`):** A call
  whose delta includes growth from a neighbour that began before the window
  and finished before this call began is emitted with `"approximate": false`:
  the epoch check catches only calls that began inside the window, so that
  growth is misattributed to the logged call while the line claims exactness.
  Background growth — periodic checkpoints, background tasks, or allocator
  page retention — is captured by `"kind": "sample"` and `"kind": "sampler"`
  time-series rows rather than a single `"kind": "call"` row.
- **P3-7 / X6b — `Drop` synchronous I/O waiver (`neurostrata-igg4`):**
  Accepted as a deliberate waiver. The sampler offloads periodic log writes
  to `tokio::task::spawn_blocking`, while `CallGuard::drop` performs its
  single-line append synchronously. This is accepted because `Drop` cannot
  await: the append is a single line under `LOG_LOCK` and happens only for
  calls that crossed `NEUROSTRATA_RSS_LOG_MB` with profiling enabled, and
  moving it off `Drop` would lose the crossing call's row when the process
  exits.
- **P3-11 / N1 — `calls_since_last` & direct HTTP surface (`neurostrata-igg4`):**
  `CallGuard` wraps both MCP `tools/call` invocations and authenticated direct
  HTTP routes (`http:<path>`, excluding `/health` and `/mcp`, which guards at
  the tool layer), so `calls_since_last` counts MCP tool calls plus direct HTTP
  requests.
- **P3-12 / N2 — `process.role` on daemon-down fallback (`neurostrata-igg4`):**
  `storage_status` includes `process.role` (`"daemon"` when served by the
  running daemon, `"cli"` when the CLI computes status in-process because the
  daemon is down) so operators never mistake the short-lived CLI process's RSS
  for the daemon's.
- **P3-13 / N3 — detached-ingest attribution geometry (`neurostrata-igg4`):**
  Detached `/ingest` jobs spawn background work that outlives the initial
  HTTP/MCP request guard; memory growth during the detached run appears in
  `"kind": "sample"` / `"kind": "sampler"` rows and any overlapping call's
  `"approximate": true` window rather than on the initial enqueue call.
- A call that began before the window is not attributable to the logged call.
- On platforms that report peak only (`rss_mb: null`), the per-call log cannot
  fire; `storage_status` exposes `rss_available` so a caller can tell.
- A mistyped `NEUROSTRATA_RSS_LOG_MB` is reported (once per bad value) and falls
  back to the default; `0` disables the per-call log (the sampler still runs).
  The report goes to stderr: this binary installs no `tracing` subscriber, so a
  `tracing::warn!` would be discarded.

## Search results can be incomplete

A search that reaches its fetch budget reports `incomplete: true` rather than
pretending the page is exhaustive. Raise the limit or narrow the query; the
flag is the contract, not an error.

## Branch protection is a repository setting

The remediation train requires 5/5 checks, a same-head review, zero unresolved
threads, and a `gate-receipt` before merge. Those gates are enforced by the
scripts and by the people merging; `main` protection itself lives in the
repository settings, and the code cannot assert it.
