# PAR-03: secret scans on metadata updates and live ingest

Baseline: original/main `8a00da515` (current `origin/main`).
Reference: fork origin/develop `3d693fd96c5fcad07d3f740584203e05975b53ce`
(`scan_for_set_metadata`, `scan_for_ingest`, `IngestItem::SkippedFile`).
Scope: `server::set_metadata` and the live ingest producer. No scanner,
schema, API or dependency change.

## The gap being closed

`add_memory` and `edit_memory` already run `secrets::scan_entry_point`.
`set_metadata` replaces metadata without scanning, and the live ingest
producer emits file and symbol nodes without scanning while
`dry_run_directory` counts secret hits. Both are ordinary write paths, so a
credential can persist in the store and graph through them.

## Acceptance contract (specified before test/production edits)

Synthetic stores use an isolated temporary LadybugDB with four fake vector
dimensions. Scanner fixtures are built at runtime from a non-literal prefix and
repeated body so no secret-shaped literal sits in the source.

| Criterion | Expected behavior | Test |
| --- | --- | --- |
| C1 metadata refusal | `set_metadata` with a scanner-detectable value returns an error before the store read/write. The error uses the existing `ERROR [SECURITY]` refusal shape and names the category (`github_pat`), not the secret. | `par03_secret_in_set_metadata_is_refused` |
| C2 metadata untouched | After a refused metadata update, the stored row's metadata is byte-for-byte the previous value; its content, id and vector are unchanged. | `par03_secret_in_set_metadata_is_refused` |
| C3 live ingest skip | A parseable file whose text trips the scanner contributes no file node, no symbols and no document sections. The rest of the walk completes and clean files land whole. | `par03_secret_in_ingest_is_refused` |
| C4 explicit skip report | The observer receives the skipped file's path and the scanner verdict through `file_skipped(path, reason)`; the reason names the category and does not echo the credential. | `par03_secret_in_ingest_is_refused` |
| C5 non-secret paths unchanged | Metadata without a secret still updates, and clean files still ingest; the unchanged-file incremental skip keeps its existing behavior. | Existing `set_metadata` / ingest suite |
| C6 dry-run parity | Dry-run already reports `Secret hits`; live ingest now refuses the same fixture. No change to the dry-run report shape. | C3 fixture; existing `dry_run` tests |

## Behavior contract

- The scanner is the existing `crate::secrets::scan_entry_point`; this unit does
  not add patterns, redaction or a new error type.
- Metadata is scanned as a value (`""` content plus the metadata value), which
  is the same contract `add_memory` applies to metadata.
- Live ingest scans the bytes already read for hashing, before any node is
  emitted, so the scanned bytes are exactly the parsed bytes.
- A skipped file is reported, not fatal: the walk continues and the ingest
  succeeds. This matches the existing dry-run contract and the fork behavior.
- Previously indexed rows for a path that now trips the scanner are handled by
  the existing carry-forward policy; this unit does not redesign carry-forward
  (PAR-04 owns failure disposition).

TDD order: this tracked spec → regression tests with unchanged production code
→ record assertion RED (build failures do not count) → minimal scan calls and
skip plumbing → execute GREEN plus focused regressions. Evidence logs go to
`NeuroStrata-parity-audit-2026-11-10/implementation/par-03/`.
