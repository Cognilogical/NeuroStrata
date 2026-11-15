# Design: `neurostrata_insights` — Longitudinal Architectural & Governance Code Insights over the Bitemporal Graph

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-mtqx`, Feature Task `neurostrata-kx2n` (Spec Task `neurostrata-sttv`).
**Approach:** Built-in & Custom Bitemporal Graph Insights backed by `src/time_series.rs` (`TimeSeriesStore` + `MemoryMetric` table) and `src/rdf/` (`ns:validAt` / `as_of` bitemporal evaluation).

## 1. Problem

Longitudinal **Code Insights** turn code and governance queries into time-series charts tracking API migrations, deprecations, and vulnerability burn-down across time.

NeuroStrata already possesses two foundational building blocks that standalone code-search engines lack:
1. **Bitemporal Knowledge Graph (`valid_from`, `valid_to`, `IngestGeneration`):** Normative memories (`rule`, `fact`, `procedure`, `task`) carry epoch validity windows, and Oxigraph SPARQL (`src/rdf/`) supports `ns:validAt(?mem, ?ts)` and `ns:isRetired(?mem)`.
2. **Embedded Time-Series Store (`src/time_series.rs` + `MemoryMetric` node table in `src/store/ladybug.rs:5755`):** `TimeSeriesStore` already supports `SeriesKey`, `Sample`, `RollingAgg` EWMA summaries, anomaly detection (`recent_anomalies`), and persistent flushing to `MemoryMetric`.

However, `TimeSeriesStore` is currently used only for low-level operational counters (`RetrievalDistance`, `SearchLatency`). Users and agents cannot:
- Track **architectural & governance health over time** (e.g., % of files/symbols governed by active rules, count of unresolved/deferred `GOVERNS` edges, hotspots with repeated failed `attempt` memories, dead/unreferenced symbols).
- Register **custom saved insight queries** (SPARQL `SELECT (COUNT(...) AS ?value)`, lexical match counts, or AST symbol/call patterns) that automatically sample on each `ingest_directory` / `publish_generation` and evaluate retroactively across historical `as_of` timestamps.

## 2. Goals

1. **Four Zero-Config Built-In Architectural Insights:**
   - `governance_coverage_pct`: Percentage of active ingested `file` and `symbol` nodes governed (directly or via ancestor directory) by $\ge 1$ active `rule` memory.
   - `deferred_edge_debt`: Count of unresolved or ambiguous deferred `GOVERNS` and `DEPENDS_ON` targets recorded in `metadata.deferred_edges`.
   - `ungoverned_hotspot_count`: Count of high-blast-radius symbols (`blast_radius_count >= 5` or associated with a `metadata.attempt.outcome == "failed"` memory) that have zero direct or file-level `GOVERNS` rules.
   - `task_knowledge_yield`: Ratio of active extracted memories (`EXTRACTED_FROM`) to completed tasks (`status == "done"`) over time.
2. **Custom Saved Insight Definitions (`InsightDefinition`):**
   - Allow operators and agents to define named insights stored in the namespace's private `registry_row` (`metadata.insight_definitions`, never exported to SynapticMesh):
     - `query_kind: "sparql"`: Any read-only SPARQL `SELECT` query returning a numeric `?value` binding (or solution count), evaluated with the existing 250ms / 1000-node budget (`src/rdf/`).
     - `query_kind: "lexical"`: Tantivy / FTS query returning matching active memory/symbol count (e.g., tracking deprecated API call sites or `TODO`/`FIXME` debt).
     - `query_kind: "ast_calls"`: Count of `symbol` nodes calling a target symbol moniker or matching a `symbol_kind` + `path_prefix` filter.
3. **Bitemporal Backfill (`as_of` Time-Travel) & Ingestion Hook:**
   - For normative/bitemporal queries (`governance_coverage_pct`, `task_knowledge_yield`, and `sparql` queries using `ns:validAt`), support instant historical backfill across `[from_ts, to_ts, step_secs]` without requiring past snapshots.
   - For structural AST queries, automatically record a `Sample` into `TimeSeriesStore` (`MetricKind::ArchitecturalInsight`) at the end of every `publish_generation` (`ingest_directory` / `ingest_scip`) and flush to `MemoryMetric`.
4. **Unified Surface:**
   - MCP tool `neurostrata_insights` (`action: "snapshot" | "series" | "define" | "remove"`).
   - Daemon endpoint `POST /insights` and CLI subcommand `neurostrata-mcp insights`.

## 3. Non-Goals

- Syncing `MemoryMetric` samples or private `insight_definitions` to SynapticMesh (metrics remain strictly local to the store).
- Executing arbitrary shell scripts to compute metrics (all insight evaluators are pure read-only queries over `LadybugStore`, `LexicalIndex`, and Oxigraph SPARQL).

## 4. Architecture

### 4.1 Data Model (`src/telemetry/insights.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightDefinition {
    pub name: String,                 // kebab-case slug, e.g. "raw-unwrap-calls"
    pub title: String,
    pub query_kind: InsightQueryKind, // "sparql" | "lexical" | "ast_calls"
    pub query: String,
    pub path_prefix: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightQueryKind {
    Sparql,
    Lexical,
    AstCalls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightPoint {
    pub ts: i64,
    pub generation: Option<u64>,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightSeriesReport {
    pub name: String,
    pub title: String,
    pub builtin: bool,
    pub current_value: f64,
    pub delta_from_first: f64,
    pub rolling_summary: serde_json::Value, // RollingAgg::to_json_summary()
    pub points: Vec<InsightPoint>,
    pub top_offenders: Vec<String>,         // e.g. top ungoverned hotspot paths
}
```

### 4.2 Integration with `TimeSeriesStore` (`src/time_series.rs`)

1. **Extend `MetricKind` (`src/telemetry/anomaly.rs`):**
   - Add `MetricKind::ArchitecturalInsight` (`as_str() -> "architectural_insight"`).
   - Each insight series keys into `TimeSeriesStore` as `SeriesKey = (namespace, MetricKind::ArchitecturalInsight, insight_name)` with label `"generation" -> active_gen.to_string()`.
2. **Automatic Sampling on `publish_generation`:**
   - When `ingest_directory` or `ingest_scip` publishes generation $g$, evaluate the 4 built-in insights and any registered custom `InsightDefinition`s for that namespace, push samples via `store.metrics.record(...)`, and call `store.flush_metrics()`.
3. **Bitemporal Retroactive Evaluation (`evaluate_as_of`):**
   - When `neurostrata_insights` is called with `action: "series"` and `backfill_points > 1` over `[from_ts, to_ts]`:
     - If `MemoryMetric` already holds persisted samples in `[from_ts, to_ts]`, merge them via `store.metrics.range(...)` (`src/time_series.rs:320`).
     - For bitemporal normative insights, evaluate at each step timestamp $t_i = \text{from\_ts} + i \cdot \Delta t$ by filtering memories where `valid_from <= t_i && (valid_to.is_none() || valid_to > t_i)`.
4. **Library Reuse & Formal Alignment (Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`):**
   - Reuse `oxigraph` (`MIT OR Apache-2.0`) for SPARQL bitemporal `ns:validAt` insights, `tantivy` (`MIT`) for lexical code insights, and `TimeSeriesStore` (`src/time_series.rs`); private registry storage (`metadata.insight_definitions` on `registry_row`) obeys the formally verified `SynapticMesh` private-sovereignty invariants in `proofs/NeuroStrata/SynapticMesh.lean`.

## 5. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Expensive custom SPARQL queries slowing down `ingest_directory` | Cap custom `InsightDefinition`s per namespace at `MAX_CUSTOM_INSIGHTS = 16` and enforce the existing 250ms timeout + 1000 algebra node budget per SPARQL insight. |
| Mesh pollution from saved insight definitions | Store `metadata.insight_definitions` on the namespace's private `registry_row` (`eligibility: private`, sibling to `metadata.doctor_state` and `metadata.enrichment_state` in `src/registry.rs`, and strictly separate from `metadata.domain_registry`), so it never enters the SynapticMesh outbox. |
| Non-finite metric values (`NaN` / `Inf` when 0 files ingested) | Default `0 / 0` ratios to `100.0` (for coverage when 0 structural nodes exist) or `0.0` (for yield when 0 tasks exist); `TimeSeriesStore::push` (`src/time_series.rs:210`) already guards `!sample.value.is_finite()`. |

## 6. Checkable Acceptance Criteria

1. `neurostrata_insights({ namespace: "NeuroStrata", action: "snapshot" })` returns all 4 built-in series (`governance_coverage_pct`, `deferred_edge_debt`, `ungoverned_hotspot_count`, `task_knowledge_yield`) with finite `current_value` and `top_offenders` list.
2. Defining a custom insight (`action: "define"`, `query_kind: "lexical"`, `query: "check_caller_path"`) persists in the namespace's private `registry_row` (not exported to SynapticMesh) and is sampled automatically on the next `ingest_directory` call.
3. Calling `action: "series"` with `from_ts` and `to_ts` returns historical `InsightPoint` samples persisted in `MemoryMetric` via `TimeSeriesStore::query_metrics`, plus bitemporal `as_of` evaluation across validity windows.
4. `cargo test --locked` passes all new `insights::*` unit and integration tests.

## 7. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-kx2n` to produce the step-by-step TDD implementation plan.
