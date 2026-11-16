# Dendritic Bridge — Hybrid Retrieval & Embedding Pipeline

> **Why you care:** Pure vector search misses exact symbol names (`check_caller_path`), while pure keyword search misses conceptual rules. The Dendritic Bridge offers three deterministic retrieval surfaces — a dense HNSW vector index, a disk-backed Tantivy full-text index, and an in-house BM25 scorer — plus a bounded graph expansion, so you can pick the surface that matches the question instead of paying for a reranking pipeline that does not exist.

---

## 1. Local-First Embedding Architecture (`src/embed.rs`)

The Dendritic Bridge abstracts embedding generation behind a unified trait supporting both zero-config local inference and external API providers:

| Provider Mode | Default Model | Dimensions | Operational Posture |
| :--- | :--- | :--- | :--- |
| **Local ONNX (Default)** | `NomicEmbedTextV15` (`fastembed`) | 768 | Runs 100% offline on CPU; zero API keys or external traffic. |
| **External HTTP Providers** | Configurable (OpenAI, Ollama, vLLM, compatible endpoints) | Provider-defined | Authenticated HTTP client with automatic fallback and explicit telemetry. |

If a configured external provider becomes unreachable, the Dendritic Bridge never fails silently: it records the degradation state explicitly in the response diagnostics and falls back according to the configured policy.

---

## 2. The Three Retrieval Surfaces

Every call to `neurostrata_search_memory` (and auto-retrieval during `neurostrata_task_claim`) picks **one** retrieval surface with the `mode` argument. The surfaces are alternatives — their scores are never fused together.

```text
Query ("caller path validation on ingest")
  │
  ├─► mode: vector          → LadybugDB HNSW cosine (NomicEmbedTextV15, 768 dims)
  │                            + capped neural-gain / decision-shape boosts
  ├─► mode: lexical         → disk-backed Tantivy full-text index
  └─► mode: lexical-inhouse → deterministic 5-field BM25 (src/fts/mod.rs)
  │
  ▼
Temporal proximity boost (src/graph_read/temporal.rs) when the query carries a date
  │
  ▼
Bounded graph expansion over GOVERNS / RELATES_TO / CONTAINS / DEPENDS_ON /
EXTRACTED_FROM edges, ranked by distinct-primary co-occurrence
  │
  ▼
Top-K Context Nodes + Provenance Evidence Paths
```

### Why Each Surface Matters
1. **Vector surface (`src/store/ladybug.rs`):** Captures natural-language architectural intent. Candidates are filtered by row class first, then the raw HNSW cosine distance has a capped usage-and-provenance gain (`≤ 0.5 × d`) and a decision-shape boost subtracted from it.
2. **Lexical surfaces (`src/lexical.rs`, `src/fts/mod.rs`):** Capture exact Rust/TypeScript symbol identifiers, config keys, file paths, and error codes. The in-house scorer runs a deterministic BM25 over `Content` / `Symbol` / `Location` / `Namespace` / `MemoryType` with domain-ontology expansion.
3. **Temporal proximity boost (`src/graph_read/temporal.rs`):** Queries containing temporal expressions (`2026-10-28`, `recent`, `last week`) receive a boost so chronologically aligned memories surface first.
4. **Bounded graph expansion (`src/store/ladybug.rs`):** Relevance propagates from seed matches across `GOVERNS`, `RELATES_TO`, `CONTAINS`, `DEPENDS_ON` and `EXTRACTED_FROM` edges, so rules governing a dependency are surfaced automatically. Each expanded node carries its provenance path and is ranked by how many distinct primaries reach it.

**What does not exist:** no Reciprocal Rank Fusion, no Personalized PageRank, and no neural cross-encoder reranker — there is no `src/rerank.rs`, no `src/reranker.rs`, and no FlashRank dependency in this codebase.
