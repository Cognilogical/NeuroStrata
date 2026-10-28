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
- Library hygiene: `oxrdf` + `oxrdf` vocabulary parser; no `oxirs-core`.
- Backward compatible: existing namespaces without vocabulary unaffected.

## 4. Scope

- Per-namespace vocabulary file (`memory-ontology.vN.json`).
- Annotate mode: stamps canonical type.
- Strict mode: filters at ingest.
- Integration with `relink_edges`.

## 5. Approach

- Vocabulary file: `docs/schemas/memory-ontology.v1.jsonld` (existing).
- Read vocabulary at namespace initialization.
- Annotate: add `canonical_type` metadata.
- Strict: reject ingest of unmatched concepts.

## 6. Interfaces / Files Touched (expected)

- `docs/schemas/memory-ontology.v1.jsonld` (update).
- `src/store/ladybug.rs`: ingest filter.
- `src/rdf/ontology.rs`: vocabulary loader.
- `Cargo.toml`: `oxrdf` (already present).