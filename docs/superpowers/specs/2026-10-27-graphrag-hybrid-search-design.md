# GraphRAG Hybrid Search — Deterministic Sub-Pieces + Optional Full Hybrid Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-52dq` (GraphRAG hybrid search, vector × graph × keyword, P3).

## 1. Problem

Optional relationship-based retrieval over memory graph; need hybrid (vector × graph × keyword) for GraphRAG. NeuroStrata explicitly rejects BM25/RRF fusio; borrow only deterministic sub-pieces.

## 2. Goal

Adopt deterministic sub-pieces (community detection, entity linking, explainability, feedback reweighting); optional full hybrid mode for GraphRAG downstream.

## 3. Constraints

- Deterministic: no BM25, no RRF, no fusio by design.
- No LLM in core retrieval.
- Library hygiene: reuse `yjah` Welford/EWMA; `oxrdf` for graph; no `oxirs-core`.
- Optional hybrid mode layered on top, never mandatory core feature.

## 4. Scope

- Community detection (vhx3) sub-piece: ranks candidates.
- Entity/mention linking (q1mt): anchors candidates.
- Explainability: path evidence (yjah evidence-backed).
- Feedback reweighting: user feedback adjusts deterministic scores.
- Optional full hybrid for GraphRAG: vector + graph + keyword fused deterministically.

## 5. Approach

- Graph: deterministic path evidence via `yjah` QueryBudget.
- Vector: existing embeddings (`fastembed` in `Cargo.toml`).
- Keyword: deterministic symbol match (no BM25).
- Hybrid: combine evidence count + embedding similarity + symbol match using deterministic weights; no LLM.

## 6. Interfaces / Files Touched (expected)

- `src/graph_read/hybrid.rs` (new): deterministic hybrid scoring.
- `src/store/ladybug.rs`: retrieval hooks.
- `Cargo.toml`: `fastembed` (already present); no new deps.