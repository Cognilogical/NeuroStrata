# Ontology Grounding — Optional JSON Vocabulary Annotate/Strict Mode Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-3pqo` (Ontology grounding: optional JSON vocabulary annotate/strict mode, P3, ranked #21, effort M-L).

## 1. Problem

Domain slugs normalized; fact.topology.component/target_system and KnownIds aliases are not normalized to one slug per concept. Optional per-namespace JSON vocabulary; annotate mode adds canonical type to metadata; strict mode filters memories at ingest. Reduces synonym drift.

## 2. Goal

Optional per-namespace JSON vocabulary; annotate mode; strict mode filtering at ingest.

## 3. Constraints

- Optional feature (not default).
- No LLM.
- Library hygiene: `oxrdf` + `oxrdf` vocabulary parser; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: existing namespaces without vocabulary unaffected.
- Warn-first: strict mode warns mismatches, filters unsupported concepts.
- Per-namespace isolation: vocabulary per namespace, not global.

## 4. Scope

- Per-namespace vocabulary file (`memory-ontology.vN.json`).
- Annotate mode: stamps canonical type on ingested memories.
- Strict mode: warn-first only — unsupported concepts preserved with a diagnostic `validation_warning` flag; **never dropped** (honors never-reject-writes policy).
- Integration with `relink_edges`.

## 5. Approach

- Vocabulary file: per-namespace `docs/schemas/namespace-X/memory-ontology.vN.jsonld` (or fallback to global `docs/schemas/memory-ontology.v1.jsonld`).
- Read vocabulary at namespace initialization.
- Annotate: add `canonical_type` metadata to ingested memories.
- Strict: warn-first (ValidationWarning) and preserve unsupported concepts with `canonical_type = null`; continue ingest (no data loss).

## 6. Library evaluation

Prefer existing crates over an in-house schema engine:

- `oxrdf` for RDF terms and dataset operations (shared with `9fnf` RDF canonical direction).
- `oxrdf` (if treating vocabulary as RDF) or `serde_json` for JSON-LD parsing; both deterministic and BSD-3-Clause.
- No `oxirs-core` (no preprocessor hooks), `scirs2` (heavy ML), or `horned-owl` (LGPL) needed; vocabulary is limited JSON/LD.
- Evaluate `oxrdf` parser: deterministic, zero-LLM, optional.

## 7. Interfaces / Files Touched (expected)

- `docs/schemas/namespace-X/memory-ontology.vN.jsonld` (new per-namespace).
- `src/store/ladybug.rs`: warn-first strict filtering.
- `src/rdf/ontology.rs`: per-namespace vocabulary loader.
- `Cargo.toml`: `oxrdf` (already present); no new deps.

## 8. Alignment with prior specs

- References `neurostrata-9fnf` (JSON primary + RDF canonical via dual-write).
- Uses `oxrdf` stack as shared with `2hxl` reasoning layer.
- Aligns with `yjah` spec for warn-first error handling.