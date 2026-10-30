# Time-Series / Episodic Metrics Design

> Status: **APPROVED** (2026-10-28).
> Task: `neurostrata-ejiy` (time-series episodic metrics, OxiRS §1.19 telemetry track).
> Companion: `docs/superpowers/specs/2026-10-27-closure-budget-zstd-anomaly-design.md` (anomaly detection), `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`.

## 1. Problem

`src/time_series.rs` is a stub: `Series` is an unbounded `Vec<(i64, f64)>` with no retention, no aggregation, no namespace scoping, and no integration. NeuroStrata needs deterministic, opt-in telemetry over memory usage (access counts, enrichment latency, retrieval distance) to feed anomaly detection and capacity planning, without making it a core memory primitive or adding a time-series database.

## 2. Goal

Add an **optional, in-house time-series metrics module** that:

- records episodic samples keyed by `(namespace, metric, stage)`,
- maintains bounded rolling windows (count, sum, mean, min, max, EWMA),
- supports range queries over a time window,
- persists samples to LadybugDB (sole store) as `MemoryMetric` rows,
- feeds the anomaly detector (`closure-budget` spec §5.4),
- is deterministic and LLM-free.

## 3. Constraints

- LadybugDB remains the sole store; no external TSDB.
- Optional and non-blocking: metric recording never blocks or fails a read/write path (warn-first).
- Bounded memory: ring buffer per series (default 1024 samples); aggregates are O(1) update.
- Deterministic: timestamps are caller-provided or `chrono::Utc::now().timestamp()` captured once per operation; no wall-clock-dependent result sets.
- No LLM.
- Privacy: metric samples contain no memory content, only counts/latencies/distances and opaque ids.

## 4. Scope

In scope:

- `TimeSeriesStore` with ring-buffered `Series`.
- Metric kinds: `AccessCount`, `EnrichmentLatencyMs`, `RetrievalDistance`, `MemoryCount`, `IngestLatencyMs`.
- In-memory rolling aggregates + optional LadybugDB persistence.
- Range query API.
- Integration hooks in `search_inner`, `upsert`, ingest pipeline.
- MCP tools and CLI.

Out of scope:

- External Prometheus/Grafana export (future).
- Per-user billing metrics.
- Distributed aggregation across instances.

## 5. Approach

### 5.1 Data structures (`src/time_series.rs`)

Canonical `MetricKind` is defined once in `src/telemetry/anomaly.rs` (closure-budget spec §5.4) and re-used here (no duplicate enum):

```rust
pub enum MetricKind { AccessCount, EnrichmentFailure, EnrichmentLatencyMs, RetrievalDistance, MemoryCount, IngestLatencyMs }
```

`SeriesKey = (String /* namespace */, MetricKind, String /* stage */)`.

```rust
pub struct Sample { pub id: String, pub seq: u64, pub ts: i64, pub value: f64, pub labels: BTreeMap<String,String> }

pub struct SeriesConfig { pub capacity: usize } // default 1024

pub struct RollingAgg {
    pub count: u64,
    pub sum: f64,
    pub min: f64,          // f64::INFINITY when count==0
    pub max: f64,          // f64::NEG_INFINITY when count==0
    pub ewma: f64,         // alpha default 0.2
    pub alpha: f64,
    pub next_seq: u64,
    pub flushed_seq: u64,
}

pub struct Series {
    pub key: SeriesKey,
    pub samples: VecDeque<Sample>,     // ring buffer
    pub agg: RollingAgg,
    pub capacity: usize,
    pub last_flushed: Instant,
}

pub struct TimeSeriesStore {
    pub series: RwLock<BTreeMap<SeriesKey, Series>>,
    pub config: SeriesConfig,
    pub anomaly_cb: Option<Arc<dyn Fn(&SeriesKey, &Sample) + Send + Sync>>,
}
```

- `SeriesKey = (String /* namespace */, MetricKind, String /* stage */)` (consistent with anomaly `AnomalyKey`).
- `push(key, sample)`: if series absent, create; assign `sample.seq = series.agg.next_seq` (then increment); assign `sample.id = "{namespace}::{metric}::{stage}::{ts}::{seq}"`; evict oldest when at capacity (recompute `min`/`max` over remaining ring buffer on eviction, O(capacity)); update `RollingAgg` incrementally (EWMA uses new sample); invoke `anomaly_cb` if set (synchronous, never blocks push on callback error). (neurostrata-ejiy, ns-ejiy)
- `range(key, from, to) -> Vec<Sample>`: returns in-memory samples with `from <= ts <= to` merged with DB samples for the same window, deduplicated by `id`.
- `aggregate(key) -> Option<RollingAgg>`: returns `None` if series absent; serializes `min`/`max` as `0.0` when `count == 0`.
- `list_keys(namespace) -> Vec<SeriesKey>`.
- `flush(namespace?)`: persists samples with `seq > flushed_seq` to `MemoryMetric` in one transaction; updates `flushed_seq`. Idempotent (PK collision = skip).

### 5.2 Persistence (`LadybugDB`)

`MemoryMetric` is a separate node table (not a `Memory` row), so `MATCH (m:Memory)` already excludes it. No `is_metric_row` filter is needed on Memory queries. `MemoryMetric` is created in `LadybugStore::init_schema` alongside `IntakeReceipt`/`IngestGeneration` and is omitted from `export_database`/`export_graph`.

```
CREATE NODE TABLE MemoryMetric (
  id STRING, namespace STRING, metric STRING, stage STRING,
  ts INT64, value DOUBLE, labels STRING, PRIMARY KEY (id)
)
```

- `id = "{namespace}::{metric}::{stage}::{ts}::{seq}"` (deterministic, seq from `RollingAgg.next_seq`).
- Write path: `TimeSeriesStore::flush(namespace?)` batches samples with `seq > flushed_seq` to LadybugDB in one transaction; called by daemon every 60s and on graceful shutdown. In-memory store is authoritative for recent range queries; DB is durable archive.
- Read path: `range` merges in-memory samples with DB samples for the window, dedup by `id`.
- `MemoryMetric` is excluded from search/export/SPARQL/RDF-star by table separation (not by row flag).
- Cross-process restart: `next_seq` is seeded from `MAX(seq)` in `MemoryMetric` for `(namespace, metric, stage)` on first push (or 0 if none), preventing PK collision within the same second.

### 5.3 Integration hooks

Canonical `stage` strings:

- `RetrievalDistance`: stage = `"search"`.
- `AccessCount`: stage = `"search"` (value = result count).
- `IngestLatencyMs`: stage = `"upsert"`.
- `EnrichmentLatencyMs`: stage = enrichment stage name (e.g. `"embed"`, `"extract"`, `"mention-link"`).
- `MemoryCount`: stage = `"housekeeping"`.
- `EnrichmentFailure`: stage = enrichment stage name.

Hooks:

- `search_inner`: after computing results, record `RetrievalDistance` sample per returned result (value = boosted distance, labels `{memory_id}`) and `AccessCount` (value = result count) with stage `"search"`. Fire-and-forget; errors logged.
- `upsert`: record `IngestLatencyMs` (value = elapsed ms) with stage `"upsert"`.
- Enrichment pipeline: record `EnrichmentLatencyMs` / `EnrichmentFailure` per stage.
- `MemoryCount`: recorded by daemon housekeeping every 5 min (count of non-retired, non-archived rows per namespace) with stage `"housekeeping"`.

`push` invokes `anomaly_cb` (if set) after updating the series, passing `(&SeriesKey, &Sample)`; the callback feeds `RollingStats::add(value)` (closure-budget §5.4 O(1) online). No `Vec<f64>` batch API.

### 5.4 Anomaly integration

- `TimeSeriesStore` exposes `samples_for_anomaly(key) -> Vec<f64>` (last N values) consumed by `RollingStats` in `closure-budget` spec §5.4.
- Anomaly detector subscribes to `push` events via a callback (no channel/async dependency; synchronous callback invoked after update).

### 5.5 Query API

- `neurostrata_metrics_query(namespace, metric, stage?, from?, to?)`:
  - if `stage` is `Some`, query that exact series.
  - if `stage` is `None`, merge samples across all stages for `(namespace, metric)` and compute aggregate over the merged set.
  - returns `{ samples: [{ts, value, labels, stage}], aggregate: {count, sum, mean, min, max, ewma} }`.
- `neurostrata_metrics_list(namespace) -> [{ metric, stage, count }]`.

### 5.6 Retention

- In-memory: ring buffer capacity 1024 per series.
- DB: `MemoryMetric` rows older than `metrics_retention_days` (default 30) are pruned by daemon housekeeping (batch delete, warn-first). Pruning never deletes memory rows.

## 6. Interfaces

- `src/time_series.rs`: `MetricKind`, `Sample`, `Series`, `TimeSeriesStore`, `RollingAgg`.
- `LadybugStore`: `flush_metrics`, `prune_metrics`, `list_metric_rows`.
- MCP tools: `neurostrata_metrics_query`, `neurostrata_metrics_list`.
- CLI: `neurostrata-mcp metrics query/list`.
- Config: `metrics_enabled: bool` (default true), `metrics_capacity: usize` (default 1024), `metrics_retention_days: u64` (default 30), `metrics_flush_interval_secs: u64` (default 60).

## 7. Edge cases

- Metrics disabled (`metrics_enabled = false`): `TimeSeriesStore` not instantiated; hooks no-op; tools return disabled message. Default is enabled (bounded in-memory; DB persistence warn-first).
- Flush failure: log warning, keep in-memory samples, retry next cycle.
- Empty series: aggregate returns zeros, range returns `[]`.
- Clock skew: samples sorted by ts on range query; duplicates allowed.
- Pruning: only `MemoryMetric` rows; never touches `Memory`.

## 8. Testing

- Unit: ring buffer eviction at capacity.
- Unit: rolling aggregate correctness (mean/min/max/EWMA).
- Unit: range filter inclusive bounds.
- Integration: flush → query from DB returns same samples.
- Integration: pruning removes old rows only.
- Determinism: identical samples → identical aggregates.

## 9. Acceptance

Optional time-series metrics available; bounded in-memory + durable DB; range queries; anomaly integration; no LLM; no core dependency; warn-first.

## 10. Files touched (expected)

- `src/time_series.rs`: full implementation.
- `src/store/ladybug.rs`: `MemoryMetric` table, flush/prune/list hooks.
- `src/server.rs`: MCP tools.
- `src/main.rs`: daemon flush housekeeping.
- `Cargo.toml`: no change.