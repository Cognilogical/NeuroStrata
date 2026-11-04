# Dendritic Bridge — Hybrid Retrieval & Embedding Pipeline

> **Why you care:** Pure vector search misses exact symbol names (`check_caller_path`), while pure keyword search misses conceptual rules. The Dendritic Bridge fuses lexical BM25, dense HNSW vectors, graph Personalized PageRank, and neural cross-encoder reranking—running locally via ONNX with explicit degradation if an external provider is offline.

---

## 1. Local-First Embedding Architecture (`src/embedder.rs`)

The Dendritic Bridge abstracts embedding generation behind a unified trait supporting both zero-config local inference and external API providers:

| Provider Mode | Default Model | Dimensions | Operational Posture |
| :--- | :--- | :--- | :--- |
| **Local ONNX (Default)** | `BGESmallENV15` (`fastembed`) | 384 | Runs 100% offline on CPU; zero API keys or external traffic. |
| **External HTTP Providers** | Configurable (OpenAI, Ollama, vLLM, compatible endpoints) | Provider-defined | Authenticated HTTP client with automatic fallback and explicit telemetry. |

If a configured external provider becomes unreachable, the Dendritic Bridge never fails silently: it records the degradation state explicitly in the response diagnostics and falls back according to the configured policy.

---

## 2. The 4-Stage Hybrid Retrieval Pipeline

Every call to `neurostrata_search_memory` (and auto-retrieval during `neurostrata_task_claim`) executes a 4-stage retrieval pipeline:

```text
Query ("caller path validation on ingest")
  │
  ├─► Stage 1a: Tantivy BM25 Lexical Search (exact identifiers, file paths, error codes)
  ├─► Stage 1b: LadybugDB HNSW Cosine Vector Search (semantic conceptual match)
  │
  ▼
Stage 2: Reciprocal Rank Fusion (RRF, k=60) + Temporal Proximity Boost
  │
  ▼
Stage 3: 1-to-2 Hop Graph Expansion & Personalized PageRank (PPR) over GOVERNS / DEPENDS_ON / AST edges
  │
  ▼
Stage 4: FlashRank ONNX Cross-Encoder Reranking (`ms-marco-MiniLM-L-12-v2`)
  │
  ▼
Top-K High-Precision Context Nodes + Provenance Evidence Paths
```

### Why Each Stage Matters
1. **Tantivy BM25 + HNSW Vector:** Captures both exact Rust/TypeScript symbol identifiers and natural-language architectural intent.
2. **Reciprocal Rank Fusion (RRF) & Temporal Boost (`src/graph_read/temporal.rs`):** Merges lexical and dense rankings without calibration drift, while queries containing temporal expressions (`2026-10-28`, `recent`, `last week`) receive a temporal proximity boost.
3. **Personalized PageRank (PPR) Graph Walk:** Propagates relevance from seed matches across `GOVERNS`, `RELATES_TO`, `DEPENDS_ON`, and Tree-sitter AST (`calls`, `imports`) edges so rules governing a dependency are surfaced automatically.
4. **FlashRank Neural Reranking (`src/reranker.rs`):** Scores each `(query, candidate)` pair jointly through a local ONNX cross-encoder to eliminate false-positive vector neighbors before tokens ever reach your LLM context window.
