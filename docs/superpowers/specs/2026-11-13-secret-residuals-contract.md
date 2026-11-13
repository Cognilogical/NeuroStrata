# Secret-scanner residuals — contract (WP2.1)

Status: Phase 2 / WP2.1. Findings: #147 residuals (metadata keys, oversized
metadata, the single-line chunker boundary). Placeholder ordering was closed in
WP1.1 and is not revisited here.

## Contracts

### C1 — metadata keys are scanned

`scan_value` scans object keys as well as values. A credential used as a key
(`{"ghp_…": "benign"}`) is rejected; the location names the key
(`metadata.<key>`).

### C2 — oversized metadata has a distinct reason

A metadata string leaf larger than the scanner's window (`MAX_SCAN_BYTES`)
cannot be scanned as one string. It is rejected with category
`oversized_metadata` (not the generic `oversized_input`) and the location of
the leaf, so the caller knows which field to shorten. Content strings keep
their chunked scan and are never rejected for size alone.

### C3 — the single-line chunker boundary is pinned

`scan_chunked` splits on newlines when possible and otherwise advances with
`SCAN_CHUNK_OVERLAP`. A token that straddles the chunk boundary on one long
line must still be seen whole. The regression uses a JWT (all three segments
required, so a split cannot be rescued by another detector) placed 10 bytes
before the boundary; shrinking the overlap to 0 makes the test fail.

## Tests

| Test | Contract |
|---|---|
| `metadata_key_carrying_a_secret_is_detected` | C1 |
| `oversized_metadata_leaf_has_a_distinct_reason` | C2 |
| `single_line_token_straddling_a_chunk_boundary_is_detected` | C3 |

## Acceptance

- A credential in a key is rejected and the location names it.
- A huge metadata leaf is `oversized_metadata`; a huge clean content file is
  still admitted (existing `secret-precision` cases).
- The boundary token is found; zero overlap fails the test.
