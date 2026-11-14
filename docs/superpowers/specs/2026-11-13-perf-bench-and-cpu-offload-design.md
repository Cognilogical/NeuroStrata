# Longitudinal Performance Receipts & CPU Offloading Design

- **Date:** 2026-11-13
- **Tasks:** `neurostrata-4qki`, `neurostrata-orbt`, `neurostrata-6h41`, `neurostrata-nduy`
- **Branch:** `feat/perf-bench-and-cpu-offload`

## 1. Problem Statement & Goals

NeuroStrata currently tracks point-in-time operational telemetry (`src/time_series.rs` EWMA rings and `src/procmem.rs` RSS watermarks), but lacks a deterministic, reproducible benchmark harness that emits branch-to-branch and release-to-release JSON performance receipts (`git_sha`, `branch`, `pkg_version`, p50/p95/p99 latency, throughput, and RSS delta).

Additionally, an audit of CPU-bound paths identified four high-impact bottlenecks where work is either serialized on a single thread, executed one item at a time across lock/thread boundaries, or run synchronously on Tokio async worker threads:

1. **Single-item CPU ONNX embedding inference (`src/embed.rs`, `src/parser/ingest.rs`)**: `FastEmbedder::embed` acquires `Mutex<TextEmbedding>` inside `spawn_blocking` and invokes `guard.embed(vec![&*text], None)` with batch size 1 for every symbol and doc section during ingestion.
2. **Per-file Tree-sitter query recompilation (`src/parser/ingest.rs`)**: `run_producer` calls `Query::new(&ts_lang, &lang_config.symbol_query)` on every visited source file instead of compiling once per language.
3. **Single-threaded file reading, SHA-256 hashing, and Tree-sitter AST parsing (`src/parser/ingest.rs`)**: `run_producer` walks, hashes, and parses all files sequentially on one OS thread.
4. **CPU-bound synchronous work on Tokio async workers (`src/rdf/sparql.rs`, `src/community.rs`, `src/kge.rs`, `src/server.rs`)**: Oxigraph `QueryEvaluator` execution, Leiden community clustering, and TransE KGE training execute directly on async worker threads without `tokio::task::spawn_blocking`.

## 2. Architecture & Component Design

### 2.1 Batched Embedding Inference (`neurostrata-orbt`)
- Extend `Embedder` in `src/traits.rs` with:
  ```rust
  async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
      let mut out = Vec::with_capacity(texts.len());
      for text in texts {
          out.push(self.embed(text).await?);
      }
      Ok(out)
  }
  ```
- Implement `embed_batch` in `FastEmbedder` (`src/embed.rs`):
  - Short-circuit `if texts.is_empty() { return Ok(Vec::new()); }`.
  - Move `texts.to_vec()` into a single `tokio::task::spawn_blocking` closure, lock `self.model` once, and call `guard.embed(owned_texts, None)` so ONNX Runtime processes the batch with vectorized multi-row matrix kernels.
  - Validate `embeddings.len() == texts.len()`.
- Implement `embed_batch` in `RemoteEmbedder` (`src/embed.rs`):
  - Send batched `input: texts` to the OpenAI-compatible `/embeddings` endpoint (with retry on 429/5xx), sort response items by `index` if present, and validate each vector for finite numbers and expected `self.dimensions`.
- Update `run_consumer` in `src/parser/ingest.rs`:
  - In `IngestItem::ParsedSymbols` and `IngestItem::DocSections`, collect all summaries for the item, call `embedder.embed_batch(&summaries).await` once, and zip the returned embeddings with the payloads for deterministic `vector_store.upsert` calls.

### 2.2 Cached Tree-Sitter Queries & Parallel AST Producer (`neurostrata-orbt`, `neurostrata-6h41`)
- Pre-compile `tree_sitter::Query` once per language in `src/parser/ingest.rs` before processing files (`compile_language_queries(schema) -> HashMap<String, Arc<Query>>`).
- Split `run_producer` into:
  1. **Deterministic walk phase**: Walk `dir_path`, filter `skipped_dirs`, record directory nodes and visited paths in deterministic walk order, and collect candidate file entries.
  2. **Bounded parallel CPU parse phase**: Process file entries across `std::thread::scope` workers (`available_parallelism().clamp(1, 8)`), where each worker thread owns a reusable `tree_sitter::Parser` and borrows the shared `HashMap<String, Arc<Query>>`, `ParserSchema`, `prev_file_map`, and `filter`. Each worker reads the file bytes, computes `sha256_hex`, checks incremental skip (`prev_file_map`) and `IngestFilter`, and extracts AST symbols/imports/calls or Markdown sections.
  3. **Deterministic emission phase**: Results are stored by original walk index and emitted to `tx` in exact walk order, preserving identical `current_paths`, `current_kinds`, `current_hashes`, `raw_imports`, and `IngestItem` ordering.

### 2.3 Offloading CPU-Bound Graph/RDF/ML Work to `spawn_blocking` (`neurostrata-nduy`)
- **`src/rdf/sparql.rs`**: In `execute_sparql_query_with_config`, move `QueryEvaluator` preparation, query evaluation, solution/triple collection, and serialization into `tokio::task::spawn_blocking`, sharing `cancel_token` and `timed_out_flag` with the timeout task.
- **`src/community.rs` & `src/server.rs`**: Provide `pub async fn detect_communities_with_resolution_async(edges: Vec<(String, String, f64)>, resolution: f64) -> Result<CommunityDetectionResult, String>` in `src/community.rs` using `tokio::task::spawn_blocking`, and call it in `rebuild_communities` (`src/server.rs`).
- **`src/server.rs` (`neurostrata_kge_train`)**: Wrap `crate::kge::cli_train` in `tokio::task::spawn_blocking` inside the async MCP tool handler so SGD epochs never block Tokio workers.

### 2.4 Longitudinal Benchmark Suite & JSON Performance Receipts (`neurostrata-4qki`)
- Create `src/bench.rs` and wire `Commands::Bench` in `src/main.rs`:
  - Subcommand flags:
    - `--suite <all|ingest|embed|lexical|sparql|gate>` (default: `all`)
    - `--iterations <usize>` (default: `5`)
    - `--output <PathBuf>` (optional path to write pretty JSON receipt)
    - `--baseline <PathBuf>` (optional path to previous JSON receipt for branch/release regression comparison)
    - `--max-regression-pct <f64>` (default: `20.0`; exits non-zero when `--fail-on-regression` is set and any suite regresses beyond threshold)
    - `--fail-on-regression` (flag to exit 1 when regression exceeds threshold)
    - `--record-metrics <String>` (optional namespace to record latency samples into `TimeSeriesStore`)
  - Receipt schema (`PerfReceipt`):
    - Provenance: `schema_version`, `timestamp`, `git_sha`, `git_branch`, `pkg_version`, `os`, `arch`, `cpu_parallelism`
    - Memory: `rss_before_mb`, `rss_after_mb`, `peak_rss_mb`, `rss_delta_mb` (via `crate::procmem::sample()`)
    - Per-suite metrics (`SuiteReceipt`): `name`, `iterations`, `items_per_iter`, `p50_ms`, `p95_ms`, `p99_ms`, `mean_ms`, `min_ms`, `max_ms`, `ops_per_sec`
    - Optional `comparison` (`BaselineComparison`): `baseline_git_sha`, `baseline_pkg_version`, `max_regression_pct`, `passed`, `suites: Vec<SuiteDiff>` (`name`, `baseline_p50_ms`, `current_p50_ms`, `p50_delta_pct`, `baseline_p95_ms`, `current_p95_ms`, `p95_delta_pct`, `regressed`).
