# Ontology Alignment — Align Per-Namespace Ontologies Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-ooa1` (Ontology alignment: align per-namespace ontologies, P3).

## 1. Problem

Per-namespace ontologies drift apart; need deterministic alignment without LLM to keep domain concepts coherent across namespaces.

## 2. Goal

Deterministic ontology alignment across namespaces: shared concept IRIs mapped via `pjoa` concept IRIs; canonical slugs as alignment keys (`u5o1`).

## 3. Constraints

- Deterministic: same inputs → same alignment.
- No LLM.
- Library hygiene: `oxrdf` for IRI alignment; `sha2` for canonical slug; `pjoa` concept IRIs; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: unaligned ontologies unaffected.
- Warn-first: alignment conflicts reported.

## 4. Scope

- Cross-namespace concept IRI mapping.
- Canonical slug as alignment key.
- Integration with `relink_edges`, `pjoa`, `u5o1`.

## 5. Library evaluation

- `oxrdf`: IRI normalization / alignment (`9fnf`, `2hxl`, `4gb2`).
- `sha2`: canonical slug derivation (`u5o1`).
- No new core libraries.

## 6. Approach

- Concept IRI → canonical slug via `sha2(IRI)`.
- Map per-namespace ontology concepts by canonical slug.
- Conflict detection: same slug, different IRIs → warn-first.
- Write aligned slugs to `metadata.canonical_slug` on ingest/re-link.

## 7. Interfaces / Files Touched (expected)

- `src/rdf/ontology.rs`: alignment logic.
- `src/store/ladybug.rs`: canonical slug write.
- `Cargo.toml`: `oxrdf`, `sha2` (already present).