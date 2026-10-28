# Cross-Namespace Concept IRIs on SynapticMesh Share Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-pjoa` (Cross-namespace concept IRIs on SynapticMesh share, P2, ranked #20, M effort).

## 1. Problem

Concept IRI field exists in domain registry but is not stamped when a shareable memory crosses SynapticMesh. Receiver maps it to local domain slug; local slugs stay private.

## 2. Goal

Stamp concept IRI when shareable memory crosses SynapticMesh; receiver maps to local domain slug; ensure cross-namespace alignment.

## 3. Constraints

- Deterministic: fixed stamping rules.
- No LLM.
- Library hygiene: `oxrdf` for IRI alignment; `sha2` for hashing; no `oxirs-core`.
- Honors mesh privacy: private slugs stay private; `shareable` concept IRIs promoted.

## 4. Scope

- Concept IRI stamping on mesh import.
- Receiver maps concept IRI → local slug.
- Private slugs stay private; shareable ones promoted.

## 5. Approach

- At mesh import, stamp `concept_iri` on each shareable memory.
- Receiver resolves `concept_iri` against domain registry; maps to local slug.
- Private domain slugs: not exported; shared ones: exported with `concept_iri`.

## 6. Interfaces / Files Touched (expected)

- `src/synaptic_mesh/import.rs`: concept IRI stamping.
- `src/rdf/ontology.rs`: concept IRI → slug resolution.
- `Cargo.toml`: `oxrdf`, `sha2` (already present); no new deps.