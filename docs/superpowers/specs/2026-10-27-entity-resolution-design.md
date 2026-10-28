# Entity Resolution — Merge Canonical MemoryId Across Namespaces Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-epzj` (Entity resolution: merge canonical MemoryId across namespaces, P3).

## 1. Problem

Same entity appears under different MemoryIds across namespaces (e.g., `lbug` vs `LadybugDB`). `related` traversal and `KnownIds` aliases split recall. Need canonical MemoryId merge without LLM.

## 2. Goal

Merge duplicate MemoryIds into one canonical ID per entity across namespaces; deterministic, no LLM.

## 3. Constraints

- Deterministic: canonical ID derived from content hash + namespace, not wall clock.
- No LLM.
- Library hygiene: `sha2` for hashing; `oxrdf` for IRI alignment; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: existing aliases preserved; canonical ID added.
- Warn-first: merge conflicts reported, not fatal.

## 4. Scope

- Canonical MemoryId derivation.
- Cross-namespace merge.
- Integration with `relink_edges`, `KnownIds`, `u5o1` canonical slugs.

## 5. Approach

- Canonical ID: `sha2(namespace + content_hash + canonical_slug)`.
- Merge: on ingest, compare canonical ID; if exists, merge aliases.
- Cross-namespace: use `pjoa` concept IRI alignment.

## 6. Library evaluation
- `sha2` for canonical ID derivation (already present).
- `oxrdf` for IRI normalization / alignment (`9fnf`, `4gb2`).
- No `oxirs-core`/`scirs2`/`horned-owl`.

## 7. Per-namespace isolation
- Canonical MemoryId derived per namespace; cross-namespace merge opt-in only via `pjoa` concept IRI alignment (`ooa1` spec).
- References `neurostrata-9fnf` (JSON primary + RDF canonical direction), `2hxl` (OWL/RDFS reasoning), `4gb2` (RDF-star provenance), `yjah` (QueryBudget/bounded closure), `pjoa` (concept IRI alignment), `u5o1` (canonical slugs).

## 8. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: canonical ID merge.
- `src/rdf/ontology.rs`: canonical ID derivation.
- `Cargo.toml`: `sha2`, `oxrdf` (already present); no new deps.