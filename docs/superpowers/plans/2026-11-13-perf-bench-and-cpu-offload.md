# Longitudinal Performance Receipts & CPU Offloading Implementation Plan

> **For agentic workers:** REQUIRED: Use `superpowers:subagent-driven-development` (if subagents available) or `superpowers:executing-plans` to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add batched CPU ONNX embedding inference (`embed_batch`), cached Tree-sitter queries, parallel multi-core file hashing and AST parsing, `spawn_blocking` offloads for SPARQL/Leiden/KGE, and a longitudinal benchmark CLI (`neurostrata-mcp bench`) emitting JSON performance receipts with baseline regression comparison.

**Architecture:** Four modular, TDD-verified tasks aligned 1-to-1 with tracked NeuroStrata tasks (`neurostrata-orbt`, `neurostrata-6h41`, `neurostrata-nduy`, `neurostrata-4qki`). Existing trait contracts remain backward-compatible via default trait methods, deterministic ordering is preserved across parallel AST workers, and all tests run offline without requiring network downloads.

**Tech Stack:** Rust 2021, Tokio (`spawn_blocking`), `std::thread::scope`, `fastembed`, `tree-sitter`, `oxigraph`/`spareval`, `leiden-rs`, `clap`, `serde_json`.

---

### Task 1: Batched CPU ONNX Embedding Inference (`embed_batch`) & Cached Tree-Sitter Queries (`neurostrata-orbt`)

**Files:**
- Modify: `src/traits.rs`
- Modify: `src/embed.rs`
- Modify: `src/parser/ingest.rs`

- **Interfaces:**
  - `src/traits.rs`: Add default `async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>` to `trait Embedder`.
  - `src/embed.rs`: Implement `embed_batch` on `FastEmbedder` (single `spawn_blocking` + single `Mutex` lock + single `guard.embed(batch, None)`) and `RemoteEmbedder` (batched `try_embed_batch` / `embed_batch_with_retry`).
  - `src/parser/ingest.rs`:
    - Add `compile_language_queries(schema: &ParserSchema) -> HashMap<String, Arc<Query>>` so Tree-sitter queries are compiled once per language instead of per file.
    - Update `run_consumer` (`IngestItem::ParsedSymbols` and `IngestItem::DocSections`) to call `embedder.embed_batch(&summaries).await` once per item batch.

- [ ] **Step 1: Write failing tests for `embed_batch` and batched ingest consumer**
  - Add a unit test in `src/embed.rs` verifying `embed_batch` on empty and multi-item inputs and verifying batch call count.
  - Add a unit test in `src/parser/ingest.rs` with a batch-tracking `Embedder` test double asserting that ingesting a file with multiple symbols invokes `embed_batch` once (with all symbols in the batch) rather than calling `embed` per symbol, and that `compile_language_queries` compiles valid queries for configured languages.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --bin neurostrata-mcp embed::tests:: parser::ingest::tests::`

- [ ] **Step 3: Implement `embed_batch` and cached Tree-sitter queries**
  - Extend `Embedder` in `src/traits.rs`.
  - Implement `FastEmbedder::embed_batch` and `RemoteEmbedder::embed_batch` in `src/embed.rs`.
  - Implement `compile_language_queries` and batched `run_consumer` embedding in `src/parser/ingest.rs`.

- [ ] **Step 4: Run tests to verify they pass**
  - Run: `cargo test --bin neurostrata-mcp embed::tests:: parser::ingest::tests::`

---

### Task 2: Parallelize CPU File Hashing and Tree-Sitter AST Parsing (`neurostrata-6h41`)

**Files:**
- Modify: `src/parser/ingest.rs`

- **Interfaces:**
  - `src/parser/ingest.rs`:
    - Factor per-file CPU work (file read, SHA-256 hash, incremental skip check, Tree-sitter parse + symbol/import/call extraction, Markdown section extraction) into a worker function executed across a bounded `std::thread::scope` pool (`available_parallelism().clamp(1, 8)`), where each worker thread initializes its own `tree_sitter::Parser` and shares read-only `Arc<Query>` per language.
    - Preserve exact walk-index ordering when updating `current_paths`, `current_kinds`, `current_hashes`, `raw_imports`, and sending `IngestItem`s to `tx`.

- [ ] **Step 1: Write failing test for deterministic parallel file parsing**
  - Add a unit test in `src/parser/ingest.rs` that ingests a multi-file fixture across multiple worker threads and asserts deterministic symbol IDs, SHA-256 hashes, and `imports`/`calls` edges across runs.

- [ ] **Step 2: Run test to verify baseline behavior**
  - Run: `cargo test --bin neurostrata-mcp parser::ingest::tests::`

- [ ] **Step 3: Implement bounded multi-threaded file hashing and AST parsing in `run_producer`**
  - Collect file work items in deterministic walk order, process them in parallel with `std::thread::scope` into index-ordered result slots, and emit items in walk order.

- [ ] **Step 4: Run ingest unit and integration tests**
  - Run: `cargo test --bin neurostrata-mcp parser::ingest::tests:: && cargo test --test staged_publication_test --test version_gate_test`

---

### Task 3: Offload CPU-Bound SPARQL, Leiden Community Detection, and TransE KGE Training to `spawn_blocking` (`neurostrata-nduy`)

**Files:**
- Modify: `src/rdf/sparql.rs`
- Modify: `src/community.rs`
- Modify: `src/server.rs`

- **Interfaces:**
  - `src/rdf/sparql.rs`: Offload `QueryEvaluator` preparation, execution, and result serialization inside `execute_sparql_query_with_config` onto `tokio::task::spawn_blocking`.
  - `src/community.rs`: Add `pub async fn detect_communities_with_resolution_async(edges: Vec<(String, String, f64)>, resolution: f64) -> Result<CommunityDetectionResult, String>` inside `community_inner` and re-export it; call it from `rebuild_communities` in `src/server.rs`.
  - `src/server.rs`: Offload `crate::kge::cli_train` in `"neurostrata_kge_train"` onto `tokio::task::spawn_blocking`.

- [ ] **Step 1: Write failing async tests for `detect_communities_with_resolution_async` and non-blocking SPARQL evaluation**
  - Add an async test in `src/community.rs` calling `detect_communities_with_resolution_async` and verifying partition & modularity parity with `detect_communities_with_resolution`.
  - Add an async test in `src/rdf/sparql.rs` verifying `execute_sparql_query_with_config` completes via `spawn_blocking` with accurate solution count, truncation, and timeout semantics.

- [ ] **Step 2: Run tests to verify failure**
  - Run: `cargo test --bin neurostrata-mcp community:: rdf::sparql::tests::`

- [ ] **Step 3: Implement `spawn_blocking` offloads in `sparql.rs`, `community.rs`, and `server.rs`**
  - Update `execute_sparql_query_with_config`, add `detect_communities_with_resolution_async`, and update `src/server.rs` call sites.

- [ ] **Step 4: Run SPARQL, community, and KGE tests**
  - Run: `cargo test --bin neurostrata-mcp community:: rdf::sparql::tests:: kge::tests::`

---

### Task 4: Longitudinal Benchmark Suite & JSON Performance Receipts (`neurostrata-4qki`)

**Files:**
- Create: `src/bench.rs`
- Modify: `src/main.rs`

- **Interfaces:**
  - `src/bench.rs`:
    - `pub enum BenchSuite { All, Ingest, Embed, Lexical, Sparql, Gate }`
    - `pub struct BenchConfig { pub suite: BenchSuite, pub iterations: usize, pub output: Option<PathBuf>, pub baseline: Option<PathBuf>, pub max_regression_pct: f64, pub fail_on_regression: bool, pub record_metrics: Option<String> }`
    - `pub struct PerfReceipt`, `pub struct SuiteReceipt`, `pub struct BaselineComparison`, `pub struct SuiteDiff`
    - `pub async fn run_benchmarks(cfg: &BenchConfig) -> anyhow::Result<PerfReceipt>`
    - `pub fn compare_receipts(current: &PerfReceipt, baseline: &PerfReceipt, max_regression_pct: f64) -> BaselineComparison`
    - `pub fn record_receipt_metrics(receipt: &PerfReceipt, store: &crate::time_series::TimeSeriesStore, namespace: &str)`
  - `src/main.rs`:
    - Add `mod bench;`
    - Add `Commands::Bench { ... }` to `Commands` enum and dispatch it before opening the production database (so `neurostrata-mcp bench` is self-contained, never contends with the live daemon lock, and runs cleanly in CI/release pipelines).

- [ ] **Step 1: Write failing unit tests in `src/bench.rs` and CLI parse tests in `src/main.rs`**
  - Test percentile calculation (`p50`, `p95`, `p99`, `mean`), `compare_receipts` regression detection, `record_receipt_metrics` in `TimeSeriesStore`, full `run_benchmarks` execution across all 5 suites, and `Cli::try_parse_from(["neurostrata-mcp", "bench", ...])`.

- [ ] **Step 2: Run tests to verify failure**
  - Run: `cargo test --bin neurostrata-mcp bench::`

- [ ] **Step 3: Implement `src/bench.rs` and wire `Commands::Bench` in `src/main.rs`**
  - Implement all 5 benchmark suites, receipt generation, baseline comparison, optional file output, and CLI dispatch.

- [ ] **Step 4: Run full benchmark and test suite**
  - Run: `cargo test --bin neurostrata-mcp bench:: && cargo test --bin neurostrata-mcp tests::bench_`
