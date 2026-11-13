# Ingest correctness — contract (WP2.3)

Status: Phase 2 / WP2.3. Findings: panel 7 (a transient `generation_summary`
failure disables the version gate and pre-fix rows overwrite fresh ones) and
the correctness P2/P3 residuals (namespace predicate on
`abort_generation`/`generation_nodes`, `fs::read` failure, file→directory
invariant).

## Contracts

### C1 — a failed or empty previous-generation summary forces a re-parse

`ingest_directory` reads the previous generation's stamps to decide whether
the parser version changed. A failed summary (and a succeeded-but-empty stamp
list) must be treated as a gate trip: `force_reparse` is true, so the producer
re-parses every file and the carry pass is skipped. Otherwise the carry pass
reads the same generation again and re-stages pre-fix rows over the fresh ones,
stamping them current.

### C2 — abort and generation listing are namespace-scoped

`abort_generation` deletes and `generation_nodes` lists only rows whose
`namespace` equals the requested namespace in addition to the
`<namespace>::<generation>::` id prefix. A foreign row whose id happens to
share the prefix (a namespace containing the separator, a corrupt import) is
neither deleted nor reported.

### C3 — an unreadable file aborts the ingest

`fs::read` failure is an error, not a warning: publishing a generation without
the file's bytes would drop its row or carry a stale copy over it. The staging
generation is aborted and the previous one stays active.

### C4 — file→directory publishes and kills the old rows

When a path changes from a file to a directory, the new generation serves the
directory node and no row of the deleted file (the kind comparison is what
makes the path a change).

## Tests

| Test | Contract |
|---|---|
| `transient_summary_failure_forces_reparse` | C1 |
| `abort_generation_scoped_to_namespace` | C2 |
| `read_failure_aborts_generation` (unix) | C3 |
| `file_to_directory_asserts_old_rows_dead` | C4 |

## Acceptance

- A transient summary failure produces a current generation with no pre-fix
  content.
- A foreign row with the aborted prefix survives.
- An unreadable file leaves the active generation unchanged.
- No deleted file row is served after a kind change.
- Mutation: restoring `offending_stamp.is_some()` alone carries stale rows;
  restoring the warn-and-continue read publishes the ingest; dropping the
  namespace predicate deletes the foreign row.
