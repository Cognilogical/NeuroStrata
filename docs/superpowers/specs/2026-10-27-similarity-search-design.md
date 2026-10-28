# Similarity Search — Vector Similarity Search Using Embeddings Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-wtnn` (Similarity search: vector similarity search using embeddings, P3).

## 1. Problem

Vector embeddings generated (`fastembed` in Cargo.toml) but no similarity search exposed over memory graph. Need vector similarity (cosine/Euclidean) as retrieval sub-piece for hybrid (`52dq`).

## 2. Goal

Vector similarity search over memory embeddings; deterministic ranking by cosine similarity; per-namespace isolation; capped results via `jwoa` retrieval budget.

## 3. Constraints

- Deterministic: same query vector → same ranked results.
- No LLM.
- Library hygiene: `fastembed` embeddings (already present); `std` similarity; `yjah` QueryBudget caps results; no `oxirs-core`/`scirs2`/`horned-owl`.
- Warn-first: missing index → empty results + warning.

## 4. Scope

- Cosine similarity over normalized embedding vectors.
- Per-namespace index isolation.
- Integration with `jwoa` retrieval budget (cap top-K).

## 5. Library evaluation

- `fastembed = "0.3"`: deterministic embeddings (already in Cargo.toml).
- `std::collections::BinaryHeap`: top-K heap for deterministic ranking.
- `yjah` QueryBudget: cap max results.
- No new core libraries.

## 6. Approach

- At query: embed query text (normalized).
- Scan namespace index (normalized vectors) compute cosine similarity.
- Top-K via BinaryHeap; budget capped.
- Results with similarity score + memory ID.

## 7. Interfaces / Files Touched (expected)

- `src/search/similarity.rs` (new): vector similarity search.
- `src/store/ladybug.rs`: embedding storage/lookup.
- `Cargo.toml`: `fastembed` already present; no new deps.