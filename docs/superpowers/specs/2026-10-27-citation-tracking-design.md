# Citation Tracking — Source Traceability Across Transformations Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-z3zw` (Citation tracking: source traceability across transformations, P3).

## 1. Problem

Memory transformations (ingest → enrich → extract → relate) lose source traceability. Need deterministic citation chain from final memory back to original source file + extractor + generation.

## 2. Goal

Citation chain per memory: `source_file → extractor → generation → transformation → final_memory`; deterministic, queryable.

## 3. Constraints

- Deterministic: same transformation → same citation chain.
- No LLM.
- Library hygiene: `oxrdf` for citation RDF (shared with `4gb2` RDF-star); `sha2` for transformation hash; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: existing memories get minimal chain.
- Warn-first: missing citation links reported.

## 4. Scope

- Source file reference per memory.
- Extractor + generation metadata.
- Transformation steps (ingest → enrich → extract → relate) with hashes.
- Query via `doctor` / `related` for audit.

## 5. Library evaluation

- `oxrdf`: citation as RDF-star annotated triples (matches `4gb2` provenance).
- `sha2`: transformation hash (already present).
- No new core libraries.

## 6. Approach

- At each pipeline stage: stamp `citation` field = `{source, extractor, generation, transform_hash}`.
- Transform hash: `sha2(previous_citation + stage_operation)`.
- Chain: each stage appends; final memory has full chain.
- Query: `citations(memory_id) -> Vec<CitationStep>`.

## 7. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: citation field on memory row.
- `src/ingest/staged_publication.rs`: citation stamp at each stage.
- `src/doctor.rs`: citation chain surface.
- `Cargo.toml`: `oxrdf`, `sha2` (already present).