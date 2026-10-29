# GraphRAG Hybrid Search Design

> Status: revised per Argon review — pending re-review.
> Task: `neurostrata-52dq` (GraphRAG hybrid search, vector × graph × keyword, P3).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-27-community-detection-design.md`, `docs/superpowers/specs/2026-10-27-entity-mention-linking-design.md`, `docs/superpowers/specs/2026-10-27-closure-budget-zstd-anomaly-design.md`.

## 1. Problem

`LadybugStore::search_inner` already implements a 3-step deterministic pipeline (Step 0 exact-name pre-pass, Step 1 vector search with capped `access_count`/`source`/`feedback_score` gain, Step 2 1-hop/2-hop/3-hop/episode-sibling graph expansion with `SearchEvidence`). This spec does not replace that pipeline; it defines an **opt-in hybrid mode** (`mode = "hybrid"` or `hybrid_enabled` config) that layers deterministic sub-pieces (community detection, entity/mention linking, explainability, feedback reweighting) on top of the existing pipeline, producing a single normalized distance with tunable weights. BM25, RRF, and probabilistic fusion remain excluded by design.

## 2. Goal

- Add an opt-in hybrid retrieval mode that combines vector distance, graph distance, and symbol-match distance into one normalized score.
- Reuse existing sub-pieces: community detection (`vhx3`), entity/mention linking (`q1mt`), explainability (path evidence), feedback reweighting (`ladybug.rs:244-250`).
- Keep default retrieval unchanged (text/vector + graph expansion).
- No LLM, no BM25, no RRF, no fusion beyond bounded linear combination of normalized distances.

## 3. Constraints

- LadybugDB remains the sole store. No second in-memory vector/graph index (`ArcSwap<BTreeMap<String, RetrievalIndex>>` is NOT introduced).
- `src/graph_read/hybrid.rs` is crate-free (`std` + `serde` only) for pure scoring/traversal types; store queries live in `src/store/ladybug.rs`.
- No `oxrdf`, `petgraph`, `fastembed`, `scirs2`, `oxirs-core`, `horned-owl` are added by this spec; reuse existing dependencies (`fastembed` already in `Cargo.toml` at version `7`).
- Welford/EWMA anomaly detector is telemetry-only and has no role in retrieval scoring.
- Determinism: hard caps (`max_hops`, `max_nodes`, `max_edges`) over canonically sorted neighbor IDs (`MemoryId ASC`) determine truncation; wall-clock `deadline` is advisory/telemetry-only and never alters the returned candidate set for identical inputs (per `yjah`/Argon).
- Row-class, lifecycle, and privacy filters apply before budget consumption: exclude `is_retired_at`, `is_archived`, `is_mesh_conflicting`, `is_registry_row`, `STRUCTURAL_MEMORY_TYPES` plus `community_summary`, honor `include_tasks`, and exclude non-shareable/private rows unless explicitly authorized.
- `community_summary` rows are excluded from Step 2 expansion per `vhx3` §5.4a; hybrid mode may apply a community hub boost only on non-summary candidates.

## 4. Scope

In scope:
- Opt-in hybrid mode layered on `search_inner`.
- Normalized distance combination (vector × graph × symbol).
- Sub-piece integration: community detection, mention linking, explainability, feedback reweighting.
- Deterministic tie-breaking and row filters.

Out of scope:
- BM25 / RRF / probabilistic fusion.
- Replacing `search_inner` default pipeline.
- New storage or second index.
- LLM in scoring.

## 5. Sub-piece contracts

- **Community detection (`vhx3`):** candidate boost via community hub score OR exclusion per `vhx3` §5.4a; `community_summary` rows never appear in expansion.
- **Entity/mention linking (`q1mt`):** edges with `extractor == "mention-detector"` contribute `derivation: ["mention-link"]` in `SearchEvidence`.
- **Explainability:** path evidence recorded in `SearchEvidence.derivation` (existing).
- **Feedback reweighting:** `feedback_gain = FEEDBACK_RANK_GAIN * feedback_score.clamp(-1.0, 1.0)` inside the `0.5 * distance` cap, already in `ladybug.rs:244-250`; hybrid mode reuses it without change.

## 6. Approach

### 6.1 Hybrid mode activation
`search_inner` gains `mode: SearchMode` (`Default` | `Hybrid`). Default mode is unchanged. Hybrid mode is enabled via config `hybrid_enabled: bool` (default false) or per-call `mode = "hybrid"`.

### 6.2 Normalized distances
All sub-piece scores are normalized distances in `[0.0, 1.0]` (lower = better, sorted `ASC`, ties broken by `MemoryId ASC`):

- `d_vec`: normalized vector distance in `[0.0, 1.0]` (existing capped usage + provenance + feedback gain).
- `d_graph`: normalized graph distance in `[0.0, 1.0]`, e.g. `1.0 / (1.0 + co_occurrence_count as f32)` or hop/role-weighted distance (`GOVERNS` < `CONTAINS` < `RELATES_TO`), bounded so expansions cannot silently override direct matches unless in explicit hybrid mode.
- `d_sym`: deterministic lexical/symbol distance in `[0.0, 1.0]` (`0.0` exact, `0.25` prefix, `0.5` token/substring, `1.0` none, matching `ui0x`).
- `d_hybrid = w_vec * d_vec + w_graph * d_graph + w_sym * d_sym` with `w_vec + w_graph + w_sym == 1.0` (defaults `0.5, 0.3, 0.2`).

### 6.3 Candidate pipeline (hybrid mode)
1. Apply row-class/lifecycle/privacy filters.
2. Run Step 0/1/2 as in default mode to obtain candidates + evidence.
3. Compute `d_vec`, `d_graph`, `d_sym` per candidate.
4. Compute `d_hybrid`; sort `ASC` by `d_hybrid`, tie-break `MemoryId ASC`.
5. Truncate to `limit`.

### 6.4 Determinism
Hard caps determine truncation; wall-clock `deadline` advisory only. Neighbor iteration uses `MemoryId ASC`.

## 7. Interfaces

- `SearchMode` enum (`Default` | `Hybrid`) added to `search_inner` signature (backward-compatible default).
- Config: `hybrid_enabled: bool` (default false), `hybrid_weights: (f32, f32, f32)` (default `(0.5, 0.3, 0.2)`).
- `src/graph_read/hybrid.rs`: pure functions `normalize_vec`, `normalize_graph`, `normalize_sym`, `combine` (crate-free, `std` + `serde`).

## 8. Edge cases

- No vector match: `d_vec = 1.0` for all; graph/symbol terms decide.
- No graph expansion: `d_graph = 1.0`.
- No symbol match: `d_sym = 1.0`.
- Empty namespace: empty results.
- Budget exceeded: partial results + warning; truncation by hard caps only.

## 9. Testing

- Unit: normalized distance math, tie-breaking by `MemoryId ASC`, weight sum == 1.0.
- Unit: row-class filter enforcement (retired/archived/conflicting/structural/community_summary excluded).
- Integration: hybrid mode returns candidates ranked by `d_hybrid`; default mode unchanged.
- Determinism: identical inputs → identical outputs under `QueryBudget` deadlines.

## 10. Acceptance

Opt-in hybrid mode available via config/per-call; combines normalized distances deterministically; reuses existing sub-pieces; no BM25/RRF/LLM; default retrieval unchanged; no second index.

## 11. Files touched (expected)

- `src/graph_read/hybrid.rs` (new, crate-free).
- `src/store/ladybug.rs`: `SearchMode` parameter, hybrid scoring branch.
- Config: `hybrid_enabled`, `hybrid_weights`.