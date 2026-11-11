# PAR-04: abort incremental generation when carry-forward fails

Baseline: original/main `8a00da515`.
Reference: fork origin/develop `3d693fd96c5fcad07d3f740584203e05975b53ce`,
fix commit `bde7fa9`. Scope: the carry-forward pass in
`src/parser/ingest.rs`. No schema, API or dependency change.

## The gap being closed

Carry-forward is the incremental step that re-stages unchanged files into the
new generation. Today its three store calls only log:

- list previous-generation rows (`src/parser/ingest.rs:1170`)
- get a carried row (`:1230`)
- upsert a carried row (`:1221`)

The pass then proceeds to `publish_generation` (`:1267`). A transient store
fault therefore publishes a generation that is missing every unchanged file's
rows. Results look complete but silently omit unchanged code until the next
successful ingest.

This is index integrity, not source-file destruction: the source tree is
untouched, and the previous generation's rows still exist, so the defect is
recoverable.

## Acceptance contract (specified before test/production edits)

Faults are injected with a delegating test store that fails exactly one carry
call while delegating everything else, so the real ingest path runs.

| Criterion | Expected behavior | Test |
| --- | --- | --- |
| D1 list abort | A failure listing previous-generation rows returns an error from `ingest_directory`, aborts staging and publishes nothing. | `a_failed_carry_forward_list_aborts_the_ingest` |
| D2 get abort | A failure fetching one carried row aborts the same way. | `a_failed_carry_forward_get_aborts_the_ingest` |
| D3 upsert abort | A failure re-staging one carried row aborts the same way. | `a_failed_carry_forward_upsert_aborts_the_ingest` |
| D4 prior generation intact | After each abort the previously active generation id is unchanged, its rows remain readable, and no generation is published. | Shared `assert_carry_fault_aborted` |
| D5 error provenance | The returned error names the carry-forward failure and preserves the underlying cause; it does not claim the source files were deleted. | D1–D3 |
| D6 successful paths unchanged | With no fault, unchanged files are still carried into the new generation, deleted files are not resurrected, and a retry after the fault clears succeeds. | Existing carry tests; `hash_filename_with_hash_survives_carry_forward`, `deleted_file_must_not_be_resurrected_by_carry_forward` |

## Behavior contract

- Record the first carry failure in an `Option<anyhow::Error>` and stop doing
  further carry work at that point.
- After the carry loop, return the recorded error before `publish_generation`
  and before any success reporting.
- Do not roll back or delete the previous generation; it stays active.
- Do not change what counts as "unchanged" or the missing-row policy.

TDD order: this tracked spec → fault-injection tests with unchanged production
code → record assertion RED → minimal abort path → GREEN plus focused
regressions. Evidence logs go to
`NeuroStrata-parity-audit-2026-11-10/implementation/par-04/`.
