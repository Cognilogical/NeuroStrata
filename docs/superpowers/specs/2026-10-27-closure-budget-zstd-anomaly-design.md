# Subclass/Transitive Closure + Query Budget + zstd + Anomaly Detection Design

> Status: revised per Argon review — pending re-review.
> Task: `neurostrata-yjah` (subclass/transitive closure + query budget + zstd + anomaly detection, OxiRS §1.19).
> Companion: `docs/superpowers/specs/2026-10-27-owl-rdfs-reasoning-design.md`, `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-27-sparql-query-engine-design.md`, `docs/superpowers/specs/2026-10-27-shacl-shape-validation-design.md`.

## 1. Problem

NeuroStrata's graph contains `GOVERNS` / `CONTAINS` / `EXTRACTED_FROM` / `RELATES_TO` / `DEPENDS_ON` edges and class hierarchies (`subClassOf*`), but there is no bounded transitive closure. Traversal silently exceeds budgets, deep `subClassOf*` chains require unbounded expansion, snapshot/mesh records are uncompressed, and there is no anomaly detection over memory counts/latencies. OxiRS §1.19 requires transitive closure materialization, per-query expansion budget, zstd compression of snapshots/mesh records, and online anomaly detection.

## 2. Goal

Add four bounded capabilities:

- **Transitive closure**: materialize `subClassOf*` descendants (class IRIs) and `GOVERNS`-closure over `CONTAINS`/`RELATES_TO` edges (memory IRIs); per-query expansion budget caps traversal depth/count.
- **Query budget**: typed `QueryBudget` (`max_hops`, `max_nodes`, `max_edges`, `deadline`) with `BudgetExceeded` error; warn-first, never panics.
- **zstd compression**: snapshots and SynapticMesh records compressed via `zstd` crate with streaming frames and `Read::take(MAX_DECOMPRESSED_BYTES)` zip-bomb cap.
- **Anomaly detection**: online Welford + EWMA z-score detector over memory counts, enrichment failures, and per-stage latencies; emit structured anomaly events without blocking reads.

All four are read-safe: closure and budget are graph-read-only; compression is write-path only; anomaly detection is telemetry-only.

## 3. Constraints

- Bounded closure: bounded hop/depth budget per query; cycle guard; never materializes infinite chains.
- Query budget applies per-query and per-stage; `BudgetExceeded` is a structured error (warn-first).
- zstd: `zstd = "0.14.3"` via Cargo (C `libzstd` FFI); streaming frames; strict decompress-size cap.
- Anomaly detection: online Welford (mean/std_dev/min/max) + EWMA z-score; never blocks read path; emits to telemetry/log only.
- No `oxirs-core`, `oxirs-shacl`, `scirs2-*`, `oxiarc-zstd`, or `ruzstd`.
- Zero LLM usage.
- LadybugDB remains sole store; closure is computed on demand (transient), not persisted as edges.
- Determinism: `deadline` is advisory only and never changes the result set; hard caps (`max_hops`/`max_nodes`/`max_edges`) determine truncation.

## 3.1 Library evaluation

| Capability | Selection | Crate | Version | License | Notes |
|---|---|---|---|---|---|
| Transitive closure (classes) | In-tree | `std` | — | MIT/Apache | `src/rdf/ontology.rs` (class IRIs) |
| Transitive closure (memories) | In-tree | `std` | — | MIT/Apache | `src/graph_read/related.rs` (memory IRIs, crate-free) |
| Query budget | In-tree | `std` | — | MIT/Apache | single canonical `QueryBudget` in `src/graph_read/related.rs` |
| Zstd compression | External | `zstd` | `0.14.3` | BSD-3-Clause | C `libzstd` |
| Anomaly detection | In-tree | `std` | — | MIT/Apache | vendor Welford + EWMA |

- **Rejected**: `oxirs-shacl`, `scirs2-stats`, `oxiarc-zstd`, `ruzstd`.
- `zstd` via Cargo; `BSD-3-Clause` added to license allowlist (create `deny.toml` if absent).

## 4. Scope

In scope:
- Transitive closure over `subClassOf*` (class IRIs) and `GOVERNS`-closure edges (memory IRIs).
- `QueryBudget` enforcement on closure + related-graph traversals.
- zstd compression/decompression of SynapticMesh wire records and snapshot frames (not `<id>.json` Git files).
- Welford + EWMA z-score anomaly detector (telemetry/log only).
- Closure results transient.

Out of scope:
- Persisted closure edges as store rows.
- Full OWL 2 RL reasoning.
- LLM-augmented anomaly scoring.
- Datalog engine.
- Writing binary `.zst` files into SynapticMesh Git worktree `<id>.json`.

## 5. Approach

### 5.1 Transitive closure

Two separate functions, no cross-module type leakage:

1. `closure_subclasses(root: &str, index: &OntologyIndex, budget: &QueryBudget) -> ClosureResult<String>` in `src/rdf/ontology.rs`: returns class/domain IRIs (not MemoryIds). `OntologyIndex::sub_classes` maps `child -> parents`, `children` maps `parent -> children` (per OWL spec §11.4). BFS/DFS with cycle guard (`HashSet`). No `EdgeSpec` or `MemoryId` knowledge in `ontology.rs`.

2. `closure_governs(root: &str, edges: &[EdgeSpec], budget: &QueryBudget) -> ClosureResult<String>` in `src/graph_read/related.rs` (crate-free, compiled standalone by `tests/graph_read/related_test.rs`): returns memory IRIs. Operates on `EdgeSpec` from `related.rs`. No `OntologyIndex` import.

Both:
- Honor `QueryBudget.max_hops`/`max_nodes`/`max_edges`; return partial results + `truncated: true` + `BudgetExceeded` if a budget is hit.
- Stable neighbor ordering (sorted by IRI) before budget cuts.
- `ClosureResult<T> = { items: Vec<T>, truncated: bool, budget_exceeded: Option<BudgetExceeded> }`.

### 5.2 Query budget (canonical definition)

Single canonical definition in `src/graph_read/related.rs` (crate-free). `src/rdf/reasoning.rs::QueryBudget` is deprecated/removed; callers migrate to this one (or `reasoning.rs` re-exports it).

```rust
pub struct QueryBudget {
    pub max_hops: usize,      // default 10
    pub max_nodes: usize,     // default 5000
    pub max_edges: usize,     // default 50000
    pub deadline: Instant,    // default 250ms (advisory)
}
pub struct BudgetExceeded {
    pub stage: String,
    pub visited: usize,
    pub truncated: bool,
    pub deadline_remaining_ms: u64, // ns-yjah
}
```

- `deadline` is advisory: if `Instant::now() > deadline`, emit a telemetry warning (`BudgetExceeded` with `deadline_remaining_ms: 0`) but continue returning the same partial set the hard caps would yield; never alter the result set by timing. (ns-yjah)
- `src/graph_read/related.rs` keeps its existing constants (`MAX_NODES=20`, `MAX_EDGES=40`, `governs_max_hops=2 default, 1..=5`) unchanged for `related_test.rs`; `QueryBudget` defaults are the closure-level caps, not the related-level caps.

### 5.3 zstd compression

- `compress_snapshot(bytes: &[u8]) -> Result<Vec<u8>, CompressionError>` and `compress_mesh_bundle(bytes: &[u8]) -> Result<Vec<u8>, CompressionError>` (infallible removed; errors propagate).
- `decompress_snapshot(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError>` and `decompress_mesh_bundle(...)` (zip-bomb cap).
- `DecompressionError::PayloadTooLarge` when `buf.len() > max`.
- Streaming frames via `zstd::stream::read::Encoder`/`Decoder`; `Read::take(max as u64 + 1)`.
- Call sites:
  - `LadybugStore::export_database` (`src/store/ladybug.rs:3070`) compresses the exported snapshot frame.
  - SynapticMesh: compress mesh bundle on publish (`src/synaptic_mesh/worker.rs`), decompress on import (`src/synaptic_mesh/import.rs`). `<id>.json` Git files remain plain JSON (compression applies to in-memory bundle frames, not Git-tracked files).
- `src/closure_budget/mod.rs` stub: deleted; replaced by `src/compression/zstd.rs` and `src/telemetry/anomaly.rs` (or retained as re-export shim). Disposition: delete the stub module and remove from `mod.rs` declarations.

### 5.4 Anomaly detection

- Vendor online `WelfordAccumulator` (mean/std_dev/min/max over `f64`) + EWMA z-score detector.
- Key: `RwLock<HashMap<AnomalyKey, RollingStats>>` where `AnomalyKey = (String /* namespace */, MetricKind, String /* stage */)` to avoid collapsing namespaces/stages.
- `MetricKind` finite enum: `MemoryCount`, `EnrichmentFailure`, `Latency`.
- EWMA z-score formula: `z_score = (sample - ewma) / std_dev.max(min_std_dev)` where `min_std_dev = 1e-6`; if `std_dev <= f64::EPSILON`, treat as `z_score = 0.0` when `|sample - mean| <= EPSILON`.
- EWMA update: `ewma = alpha * sample + (1 - alpha) * ewma`, default `alpha = 0.2`.
- Sample variance: `count - 1` (sample std_dev) when `count >= 2`; `0.0` otherwise.
- Cooldown: `last_emit: Instant`; emit `AnomalyEvent` only if `now - last_emit >= cooldown` (default 60s) and `|z_score| > z_threshold` (default 3.0).
- `AnomalyEvent { metric: MetricKind, value: f64, mean: f64, std_dev: f64, z_score: f64, namespace: String, stage: String }`.
- Thread-safe; telemetry/log only; never blocks reads.
- Alternative `rolling-stats = "0.8.0"` not used (in-tree chosen).

## 6. Interfaces

- Graph read (`src/graph_read/related.rs`, crate-free):
  - `pub struct QueryBudget { max_hops, max_nodes, max_edges, deadline }`
  - `pub struct BudgetExceeded { stage, visited, truncated, deadline_remaining_ms }` (ns-yjah)
  - `pub struct ClosureResult<T> { items: Vec<T>, truncated: bool, budget_exceeded: Option<BudgetExceeded> }`
  - `pub struct EdgeSpec { ... }` (existing)
- RDF (`src/rdf/ontology.rs`):
  - `closure_subclasses(root: &str, index: &OntologyIndex, budget: &QueryBudget) -> ClosureResult<String>`
- Graph read:
  - `closure_governs(root: &str, edges: &[EdgeSpec], budget: &QueryBudget) -> ClosureResult<String>`
- Compression (`src/compression/zstd.rs`):
  - `compress_snapshot(bytes: &[u8]) -> Result<Vec<u8>, CompressionError>`
  - `decompress_snapshot(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError>`
  - `compress_mesh_bundle(bytes: &[u8]) -> Result<Vec<u8>, CompressionError>`
  - `decompress_mesh_bundle(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError>`
- Anomaly (`src/telemetry/anomaly.rs`):
  - `WelfordAccumulator { add, mean, std_dev, min, max }`
  - `RollingStats { welford, ewma, alpha, z_threshold, last_emit }`
  - `AnomalyEvent { ... }`
  - `AnomalyKey = (String, MetricKind, String)`
- Configuration:
  - `closure_max_hops: usize` (default 10)
  - `closure_max_nodes: usize` (default 5000)
  - `closure_max_edges: usize` (default 50000)
  - `closure_deadline_ms: u64` (default 250)
  - `snapshot_max_decompressed_bytes: usize` (default 1 MiB for mesh; 16 MiB for snapshots)
  - `anomaly_zscore_threshold: f64` (default 3.0)
  - `anomaly_cooldown_secs: u64` (default 60)
  - `anomaly_alpha: f64` (default 0.2)

## 7. Edge cases

- Self-loop: cycle guard prevents infinite recursion.
- Deep chain (> max_hops): partial closure + truncated + BudgetExceeded.
- Disconnected entity: empty result.
- Oversized bundle (> max): `DecompressionError::PayloadTooLarge`.
- Zero-variance: z_score = 0.0 (guarded).
- First sample: no anomaly (count < 2).
- Budget deadline mid-traversal: return partial, don't block.
- Private/structural/task rows: excluded (inherited).

### 7.1 Determinism

- `deadline` advisory only; never changes result set.
- Neighbor ordering sorted before budget cuts.
- Anomaly detection deterministic (Welford/EWMA over fixed samples).

## 8. Testing

- Unit: closure over known chain returns all descendants.
- Unit: cycle guard on self-loop.
- Unit: BudgetExceeded when max_hops exceeded.
- Unit: ClosureResult truncated + partial Vec.
- Unit: zstd roundtrip.
- Unit: decompress_bounded rejects payload > max (buf.len() > max).
- Unit: Welford mean/std_dev manual.
- Unit: zero-variance guard.
- Unit: EWMA emits AnomalyEvent for outlier.
- Integration: closure + budget within deadline.
- Integration: related_for_seed_with_budget passes related_test.rs.
- Performance: closure + budget within deadline_ms.

## 9. Acceptance

Bounded transitive closure (class IRIs + memory IRIs) with canonical QueryBudget; zstd compression with zip-bomb cap and defined call sites; Welford+EWMA anomaly detector with namespace/stage-keyed state; no oxirs-core/scirs2/oxirs-shacl/oxiarc-zstd/ruzstd; zero LLM; deadline advisory only.

## 10. Files touched (expected)

- `src/rdf/ontology.rs`: `closure_subclasses`.
- `src/graph_read/related.rs`: `closure_governs`, canonical `QueryBudget`/`BudgetExceeded`/`ClosureResult`/`EdgeSpec`.
- `src/rdf/reasoning.rs`: remove or re-export `QueryBudget` (migrate callers).
- `src/compression/zstd.rs` (new): compress/decompress functions.
- `src/telemetry/anomaly.rs` (new): Welford, RollingStats, AnomalyEvent, AnomalyKey.
- `src/store/ladybug.rs`: export_database compression call site.
- `src/synaptic_mesh/worker.rs`, `import.rs`: bundle compression call sites.
- `src/closure_budget/mod.rs`: deleted (or re-export shim).
- `Cargo.toml`: add `zstd = "0.14.3"`; license allowlist (create deny.toml if absent).