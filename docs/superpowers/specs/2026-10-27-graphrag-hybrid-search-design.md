# GraphRAG Hybrid Search — Deterministic Sub-Pieces + Optional Full Hybrid Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-52dq` (GraphRAG hybrid search, vector × graph × keyword, P3).

## 1. Problem

Optional relationship-based retrieval over memory graph; need hybrid (vector × graph × keyword) for GraphRAG. NeuroStrata explicitly rejects BM25/RRF fusion; borrow only deterministic sub-pieces.

## 2. Goal

Adopt deterministic sub-pieces (community detection, entity linking, explainability, feedback reweighting); optional full hybrid mode for GraphRAG downstream.

## 3. Constraints

- Deterministic: no BM25, no RRF, no fusion by design.
- No LLM in core retrieval.
- Library hygiene: reuse `yjah` Welford/EWMA anomaly detector, `oxrdf` for graph projection; `fastembed` for embeddings; NO `oxirs-core`/`scirs2`/`horned-owl`; use `oxrdf` not `scirs2` for graph.
- Optional hybrid mode layered on top, never mandatory core feature.
- Warn-first: retrieval failures reported, not fatal.
- Per-namespace isolation: retrieval index scoped per namespace.

## 4. Scope

- Community detection (`vhx3`) sub-piece: ranks candidates.
- Entity/mention linking (`q1mt`): anchors candidates.
- Explainability: path evidence (`yjah` evidence-backed).
- Feedback reweighting: user feedback adjusts deterministic scores.
- Optional full hybrid for GraphRAG: vector + graph + keyword fused deterministically.

## 5. Library evaluation

Evaluate crates against required sub-pieces:

- `oxrdf` / `oxttl` / `sha2`: RDF projection for graph sub-piece (matches `9fnf`, `2hxl`).
- `fastembed = "0.3"` (already in `Cargo.toml`): vector embeddings; BSD-3-Clause, deterministic.
- `petgraph = "0.8.3"`: backup for graph traversal (as `2hxl` uses for complex DAGs).
- No `scirs2`: rejected (heavy ML + rustls hooks via `oxirs-core` chain).
- No `oxirs-core`: rejected (pre-main constructor hooks, per `yjah` AC-1).
- No `horned-owl`: LGPL, not needed for sub-piece retrieval.
- `yjah` `QueryBudget` used for bounded graph traversal (deterministic path evidence).

## 6. Approach

- Graph: deterministic path evidence via `yjah` QueryBudget (max_hops/max_nodes/max_edges/deadline).
- Vector: existing `fastembed` embeddings (normalization fixed).
- Keyword: deterministic symbol match (case-insensitive, sorted) — NO BM25.
- Hybrid (optional): combine evidence count + embedding cosine-sim + symbol match with fixed weights (e.g., w_graph=0.5, w_vec=0.3, w_sym=0.2); score computed deterministically; no LLM.
- Per-namespace index isolation: `ArcSwap<BTreeMap<String, RetrievalIndex>>` per namespace.
- Warn-first: retrieval failure (missing index, budget exceeded) → partial results + warning.

## 7. Interfaces / Files Touched (expected)

- `src/graph_read/hybrid.rs` (new): deterministic hybrid scoring.
- `src/store/ladybug.rs`: namespace-scoped retrieval hooks (use `ArcSwap` per namespace).
- `Cargo.toml`: `oxrdf`, `fastembed` (already present); no new core dependencies.