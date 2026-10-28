# RDF Canonical / JSON Vocabulary Transition Design

> Status: draft — user review required before implementation planning.
> Task: `neurostrata-9fnf` (Confirm RDF migration direction — strategic decision, gates §1.1, §1.2, §1.4).
> Companion specs: `2026-10-27-rdf-canonical-memory-data-model-migration-design.md` (z9e7, canonical migration design), `2026-10-27-owl-rdfs-reasoning-design.md` (2hxl, OWL reasoning), `2026-10-27-shacl-shape-validation-design.md` (7zvo), `2026-10-27-sparql-query-engine-design.md` (damg).

## 1. Problem

NeuroStrata store writes JSON-serialized memory objects (via `serde_json`). The RDF canonical-layer spec (z9e7) and OWL reasoning spec (2hxl) assume RDF quads / N-Quads / RDF Patch are available as the canonical representation, but no strategic decision has been made on whether JSON is transitional or permanent. The decision gates §1.1 (RDF canonical data model migration), §1.2 (OWL/RDFS reasoning over memory graph), and §1.4 (SPARQL endpoint + projection), and determines which library stack (`oxrdf`/`oxttl`/`sha2` vs `serde_json` only) carries canonical state.

## 2. Goal

Make the strategic direction explicit: choose JSON vs RDF/OWL canonical representation and the migration path, then allow z9e7 (canonical migration), 2hxl (OWL reasoning), 7zvo (SHACL validation), damg (SPARQL query engine), and the federation spec (k8tc) to proceed against it.

## 3. Constraints

- Existing binary (`Cargo.toml` binary `neurostrata-mcp`) uses `serde_json`; modules (`graph_read/related.rs`, `context/`) compile crate-free via `#[path]`.
- SynapticMesh wire records (`<id>.json` in Git working tree, `import.rs` JSON parsing, `MAX_RECORD_BYTES = 1 MiB`) are JSON.
- LadybugDB is the sole store; any canonical format must map to/from it.
- Library hygiene: `oxirs-core` avoided directly; prefer `oxrdf`/`oxttl` + `sha2`; `oxirs-vec` only if needed; `oxirs-shacl` only if SHACL evaluation requires it (rejected previously due to `scirs2`/`ctor`).
- Zero LLM in canonical layer.

## 3.1 Library evaluation

| Need | Library | Verdict | Note |
|---|---|---|---|
| JSON serialization (existing) | `serde_json` | **Keep** | Binary pipeline; no change required |
| RDF dataset / quads | `oxrdf` (rdf-12) | **Use for canonical layer** | `oxttl` parser; `oxsdatatypes`; avoids `oxirs-core` |
| Turtle / N-Quads / RDF Patch | `oxttl` | **Use** | Serializes RDF to wire format |
| Canonical hash (stable ordering) | `sha2` | **Use** | `sha2 = "0.10"`; hash over sorted N-Quads |
| SHACL evaluation | `oxirs-shacl` | **Reject direct** | Pulls `oxirs-core`/`scirs2`; use manual `oxrdf` + SHACL parser if needed |
| OWL reasoning / transitive closure | `oxrdf` + custom BFS | **In-tree** (per yjah spec) | `petgraph` backup if multi-relation DAG needed |

- No `oxirs-core` dependency added.
- `oxrdf` + `oxttl` selected over `oxirs-arq` (same `oxirs-core` concern).

## 4. Scope

In scope (decision only; no code changes yet):

- Confirm direction: JSON-primary / transitional, RDF-canonical / transition target.
- Define canonical representation format (N-Quads / RDF Patch / JSON-LD transition).
- Define mapping contract (`MemoryId` ↔ `URI`, `namespace` ↔ `graph`, properties ↔ `predicates`).
- Define dual-write / migration sequence (dual-write first; JSON drop after RDF-canonical verified over mesh).
- Confirm library choices (`oxrdf`, `oxttl`, `sha2` for RDF layer; `serde_json` preserved for wire/mesh).

Out of scope (deferred to implementation specs):
- Actual dual-write code (z9e7 / 2hxl / damg / 7zvo will carry implementation).
- Mesh wire-format change to binary `.json.zst` (already decided: stay JSON; zstd applies to archive bundles per yjah spec).
- Full OWL 2 RL rules (yjah keeps BFS; 2hxl may use `petgraph` if needed).

## 5. Approaches

### Approach A — JSON-primary, RDF-canonical transition (Recommended)

- JSON stays primary for wire, mesh, user-facing serialization; RDF is the long-term canonical layer.
- Phase 1 (dual-write): write both JSON (mesh/web) and RDF quads (canonical store) from the same memory operation; hash RDF via `sha2` for version fingerprint.
- Phase 2 (transition): after mesh / query / SHACL / federation specs verify RDF correctness over ≥1 window, JSON becomes a derived serialization (read from RDF via `oxttl`).
- Phase 3 (retire JSON source): JSON writes only happen from RDF projection when needed for external clients.

Trade-offs: preserves existing binary pipeline; allows gradual verification; avoids breaking mesh records; requires dual-write overhead temporarily.

### Approach B — RDF-canonical from start (Aggressive)

- Make RDF/N-Quads canonical immediately; JSON is derived via `oxttl` projection.
- Trade-off: faster semantic consistency, but breaks mesh wire-format (needs JSON derivation on every read/write), requires full `oxrdf` integration before SHACL / SPARQL / federation specs complete.

### Approach C — Dual-canonical (Both first-class)

- Both formats maintained independently; no transitional drop.
- Trade-off: highest overhead; no single source of truth; mesh must handle both.

**Recommendation: Approach A.** Matches existing `serde_json` binary, the `z9e7` dual-write spec already drafted, and allows `2hxl` / `7zvo` / `damg` / `k8tc` to build against a verified RDF layer without forcing JSON retirement prematurely.

## 6. Interfaces

- Decision record (this spec): `docs/superpowers/specs/2026-10-27-rdf-canonical-direction-design.md`
- Canonical representation: RDF N-Quads over `oxrdf::Dataset` (graph per namespace).
- Hash / fingerprint: `sha2` over sorted N-Quads (no timestamp fields).
- Mesh wire format: keep `<id>.json` (JSON); RDF projection only in `project_sparql_dataset` / `project_federated_sparql_dataset`.
- Migration stages documented in `z9e7` spec (§4, §5).

## 7. Testing / Acceptance

- Decision documented; companion specs (z9e7, 2hxl, damg, 7zvo, k8tc, yjah) can proceed against it.
- Library choices verified (`oxrdf`, `oxttl`, `sha2`; no `oxirs-core`).
- Mesh format unchanged (JSON); zstd only for archive / bundle (per yjah).
- Argon review completed; user approved direction.

## 8. Files touched (expected — decision only, no implementation yet)

- `docs/superpowers/specs/2026-10-27-rdf-canonical-direction-design.md` (this file)
- `docs/superpowers/specs/2026-10-27-rdf-canonical-memory-data-model-migration-design.md` (reference for Phase 1 dual-write)
- `src/store/canonical.rs` (future — not this spec)
