# Canonical Entity Slugs for Topology and KnownIds Aliases Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-u5o1` (Canonical entity slugs for topology and KnownIds aliases, P2, ranked #17, S-M effort).

## 1. Problem

Domain slugs are normalized, but `fact.topology.{component,target_system}` and `KnownIds` aliases are not normalized to one slug per concept. This splits recall between `lbug` and `LadybugDB`.

## 2. Goal

Normalize topology fields and `KnownIds` aliases to canonical entity slugs; prevent duplicate representations.

## 3. Constraints

- Deterministic: fixed normalization rules, fixed slug format.
- No LLM.
- Library hygiene: `oxrdf` / `oxrdf` for IRI normalization; `sha2` for canonical slug derivation; no `oxirs-core`.
- Backward compatible: existing aliases preserved, new canonical slug added.

## 4. Scope

- Canonicalization of `fact.topology` component / target fields.
- `KnownIds` alias normalization to slug.
- Integration with `neurostrata-9fnf` RDF canonical layer (slug used as `concept_iri`).

## 5. Approach

- Normalization rules: lowercase, remove non-alphanumeric, collapse whitespace to `-`.
- Derive canonical slug from `concept_iri` via `oxrdf` IRI normalization.
- Write back to `metadata.canonical_slug` at `upsert` / `relink_edges`.
- Read-time projection uses canonical slug.

## 6. Library evaluation (explicit)
- `sha2` for canonical slug derivation; `oxrdf` for IRI normalization; NO `oxirs-core`/`scirs2`/`horned-owl`.

## 7. Per-namespace isolation / references / constraints
- Per-namespace isolation: canonical slug derived per namespace; cross-namespace alignment via `pjoa` concept IRI only (`ooa1`).
- References `neurostrata-9fnf` (JSON primary + RDF canonical direction), `yjah` (QueryBudget / transient inference), `2hxl` (BFS / ontology index), `pjoa` (concept IRI alignment), `4gb2` (RDF-star provenance), `52dq` (GraphRAG hybrid), `mhsp` (inter-namespace).
- Warn-first: unknown class/property / normalization failure → warning (non-fatal).
- Deterministic: fixed normalization rules, sorted output.

## 8. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: slug normalization hooks.
- `src/rdf/ontology.rs`: `canonical_slug()` method.
- `Cargo.toml`: `sha2 = "0.10"` / `oxrdf` (already present).
