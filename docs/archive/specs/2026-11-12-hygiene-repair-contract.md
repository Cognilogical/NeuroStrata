# Hygiene repair failure isolation — contract (#142 G1/G3/G4/G5)

Status: Phase 1 / WP1.4. Issue: Cognilogical/NeuroStrata#142.

## Problem

`repair_namespace_hygiene` repairs candidates in 25-row chunks, but each
chunk is awaited with `?`. A failure in chunk 2 discards the counts from
chunk 1 and aborts the run. The doctor auto-upgrade caller matches only
`Ok` and stamps `last_run_version` regardless, so the only automatic retry
path never runs again until the next release and leaves no trace.

## Contracts

### C1 — per-chunk isolation with partial counts

Each candidate chunk stays its own write turn. A failed chunk is logged with
the counts committed so far and the run continues to the next chunk. The
chunk runner returns `(shape_updates, dangling_updates, failed_chunks)`.

### C2 — partial failure is an error that carries the totals

`repair_namespace_hygiene` returns `Err` when any chunk failed. The error
message states the committed shape/dangling counts and the number of failed
chunks. `Ok` means every chunk committed.

### C3 — the caller does not advance on error

The doctor auto-upgrade in `handle_get_snapshot` logs a warning and appends a
finding for a failed repair, and leaves `last_run_version` unchanged so the
version-triggered retry runs again. `last_run_ts`/`session_counter` still
record the attempt.

### C4 — paging pin

The candidate pass pages with `ORDER BY m.id LIMIT page` and a strict
`m.id > last` cursor. The page size is a parameter (`repair_namespace_hygiene`
passes 500), so a test can use a tiny page and prove that a row which needs
repair after the first page is still found.

### C5 — documented partial-success semantics

The doc comment on `repair_namespace_hygiene` states that chunks are write
turns, that a failed chunk keeps earlier counts and is reported, and that the
caller must not mark the run complete on `Err`.

## Tests

Inline, binary-only crate:

```
cargo test --locked --bin neurostrata-mcp <name> -- --test-threads=1
```

| Test | Contract |
|---|---|
| `chunk_failure_keeps_prior_counts` | C1: chunk 2 fails; chunks 1 and 3 commit |
| `repair_partial_counts_are_reported` | C2: error names committed totals and failed chunks |
| `caller_does_not_advance_on_error` | C3: `Err` leaves `last_run_version` unchanged |
| `hygiene_paging_pin` | C4: page size 2 finds a repair after page one |

## Acceptance

- No chunk failure is swallowed; partial counts are reported.
- The doctor retries after a partial repair.
- Mutation: restoring `.await?` in the chunk loop makes `chunk_failure_keeps_prior_counts`
  impossible to satisfy; restoring `if let Ok` + unconditional stamp makes
  `caller_does_not_advance_on_error` fail once the version write is driven by
  the flag.
