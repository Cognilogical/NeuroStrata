# Full-Text Search Lexical Upgrade Design

## Goal
Upgrade in-house BM25 to ranked lexical search with fuzzy/multi-field support or integrate Tantivy when crate conflicts resolve.

## Architecture
- Module: `src/fts/mod.rs` (in-house BM25).
- Benchmark exact-name pre-pass first before funding Tantivy.

## Constraints
- Single auditable ranking rule (no RRF in default search unless specified).
- Tombstone consistency and namespace filtering.

## Testing
- Basic search ranking.
- Tombstone exclusion.