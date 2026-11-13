# Accepted limitations

These are deliberate, documented trade-offs. They are not defects waiting to
be fixed; a change here needs a design decision and a test that replaces the
one named below.

## `hit_count` is bookkeeping, not a verdict

`guard` increments a rule's persisted `hit_count` when the rule fires. The
increment is one store write turn, so concurrent firings serialize and no
update is lost (`concurrent_hit_counts_are_not_lost`).

- A failed increment is logged and does not change the verdict: the guard's job
  is the pass/reject decision, and a bookkeeping write failure must not turn a
  pass into a rejection (or vice versa).
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

- A call that began before the window is not attributable to the logged call.
- On platforms that report peak only (`rss_mb: null`), the per-call log cannot
  fire; `storage_status` exposes `rss_available` so a caller can tell.
- A mistyped `NEUROSTRATA_RSS_LOG_MB` is reported and falls back to the
  default; `0` disables the per-call log (the sampler still runs). The report
  goes to stderr: this binary installs no `tracing` subscriber, so a
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
