# Subclass/Transitive Closure + Query Budget + zstd + Anomaly Detection Design

> Status: draft — pending Argon review.
> Task: `neurostrata-yjah` (subclass/transitive closure + query budget + zstd + anomaly detection, OxiRS §1.19).
> Companion: `docs/superpowers/specs/2026-10-27-owl-rdfs-reasoning-design.md`, `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md` (RDF canonical store, not this spec), `docs/superpowers/specs/2026-10-27-sparql-query-engine-design.md`, `docs/superpowers/specs/2026-10-27-shacl-shape-validation-design.md`.

## 1. Problem

NeuroStrata's graph contains `GOVERNS` / `CONTAINS` / `EXTRACTED_FROM` / `RELATES_TO` / `DEPENDS_ON` edges and class hierarchies (`subClassOf*`), but there is no bounded transitive closure. Traversal silently exceeds budgets, deep `subClassOf*` chains require unbounded expansion, snapshot/mesh records are uncompressed, and there is no anomaly detection over memory counts/latencies. OxiRS §1.19 requires transitive closure materialization, per-query expansion budget, zstd compression of snapshots/mesh records, and online anomaly detection.

## 2. Goal

Add four bounded capabilities to NeuroStrata:

- **Transitive closure**: materialize `subClassOf*` descendants (subclasses) and `GOVERNS`-closure over `CONTAINS` / `RELATES_TO` edges; per-query expansion budget caps traversal depth/count.
- **Query budget**: typed `QueryBudget` (`max_hops`, `max_nodes`, `max_edges`, `deadline`) with `BudgetExceeded` error; warms on overflow (warn-first), never panics.
- **zstd compression**: snapshots and SynapticMesh records compressed via `zstd` crate with streaming frames and `Read::take(MAX_DECOMPRESSED_BYTES)` zip-bomb cap.
- **Anomaly detection**: online Welford + EWMA z-score detector over memory counts, enrichment failures, and per-stage latencies; emit structured anomaly events without blocking reads.

All four are read-safe: closure and budget are graph-read-only; compression is write-path only; anomaly detection is telemetry-only (does not block or reject).

## 3. Constraints

- Bounded closure: bounded hop/depth budget per query; cycle guard; never materializes infinite chains.
- Query budget applies per-query and per-stage; `BudgetExceeded` is a structured error (warn-first).
- zstd: `zstd = "0.14.3"` via Cargo (not vendored), C `libzstd` FFI; streaming frames; strict decompress-size cap.
- Anomaly detection: online Welford (mean/std_dev/min/max) + EWMA z-score; never blocks read path; emits to telemetry/log only.
- No `oxirs-core`, `oxirs-shacl`, `scirs2-*`, `oxiarc-zstd`, or `ruzstd` (decoder-only, cannot compress).
- Zero LLM usage in closure/budget/compression/anomaly path.
- LadybugDB remains sole store; closure is computed on demand (transient), not persisted as edges.

## 3.1 Library evaluation

| Capability | Selection | Crate | Version | License | MSRV | Source |
|---|---|---|---|---|---|---|
| Transitive closure | In-tree | `std` | — | MIT/Apache | 1.0 | `src/rdf/ontology.rs`, `src/graph_read/related.rs` |
| Query budget | In-tree | `std` | — | MIT/Apache | 1.0 | `src/graph_read/related.rs` (`MAX_NODES`/`MAX_EDGES`) |
| Zstd compression | External | `zstd` | `0.14.3` | BSD-3-Clause | 1.64 | `Cargo.toml` (C `libzstd`) |
| Anomaly detection | In-tree | `std` | — | MIT/Apache | 1.0 | vendor `WelfordAccumulator` + EWMA z-score (mirroring oxirs-shacl's detector without scirs2) |
| Anomaly (alt) | External | `rolling-stats` | `0.8.0` | MIT/Apache | ~1.60 | online Welford (as clean reference, provided zero-variance guard added) |

- **Rejected**: `oxirs-shacl` (pulls `oxirs-core` → `ctor` pre-main TLS hook + `scirs2` bloat), `scirs2-stats` (banned), `oxiarc-zstd` (low maturity), `ruzstd` (decoder-only, cannot compress snapshots/mesh records).
- **In-tree**: closure (bounded BFS/DFS + cycle guard), budget (`QueryBudget` struct + `BudgetExceeded`), anomaly (vendor Welford + EWMA z-score, mirroring `oxirs-shacl`'s Welford detector without `scirs2`).
- `zstd` via Cargo; `BSD-3-Clause` added to license allowlist alongside `MIT OR Apache-2.0`.

## 4. Scope

In scope:

- Transitive closure over `subClassOf*` and `GOVERNS`-closure edges (GOVERNS authority respects `related.rs` caps: `governs_max_hops = 2` default, `1..=5` max).
- `QueryBudget` enforcement on all closure + related-graph traversals.
- zstd compression/decompression of SynapticMesh wire records and snapshot frames (not `<id>.json` tracked in Git; applies to snapshots/mesh bundles).
- Welford + EWMA z-score anomaly detector (telemetry/log only).
- Closure results transient — never materialized as LadybugDB edges.
- Warn-first on budget overflow (`truncated` flag + structured `BudgetExceeded`).

Out of scope:

- Persisted closure edges as store rows.
- Full OWL 2 RL reasoning (use `petgraph` topo-sort only if needed later).
- LLM-augmented anomaly scoring.
- Datalog engine (`datafrog` is backup only if rules outgrow BFS).
- Writing binary `.zst` files into SynapticMesh Git worktree `<id>.json` (stay with JSON).

## 5. Approach

### 5.1 Transitive closure

1. Build an adjacency map over `subClassOf*` edges (and optionally `GOVERNS`-closure over `CONTAINS` / `RELATES_TO`) from the local memory graph.
2. Run bounded BFS/DFS with cycle guard (`visited: HashSet`) from each root class/entity.
3. Honor `QueryBudget.max_hops` / `max_nodes` / `max_edges`; return partial results + `truncated: true` + `BudgetExceeded` if a budget is hit.
4. Return descendant set (closure) as an ordered `Vec<MemoryId>`; stable ordering via insertion sort keyed by memory id.
5. Closure is computed transiently per query; no edge materialization in LadybugDB.

### 5.2 Query budget

Add typed budget carried through graph-read traversals:

```rust
pub struct QueryBudget {
  pub max_hops: usize,      // default 10
  pub max_nodes: usize,     // default 5000
  pub max_edges: usize,     // default 50000
  pub deadline: Instant,    // default 250ms
}

pub struct BudgetExceeded {
  pub stage: String,
  pub visited: usize,
  pub truncated: bool,
  pub deadline_remaining_ms: u64,
}
```

- `BudgetExceeded` error includes `stage`, `visited`, `truncated`, and `deadline_remaining_ms`.
- Extend `src/graph_read/related.rs` (`MAX_NODES`/`MAX_EDGES`) into `QueryBudget`; keep defaults (`2 / 20 / 40`) unchanged for `related_test.rs`.
- Warn-first: never reject a query solely for budget; return partial results with `truncated`.

### 5.3 zstd compression

- Compress SynapticMesh wire records and snapshot frames via `zstd = "0.14.3"` crate, C `libzstd` FFI.
- Stream frames (`zstd::stream::read::Encoder` / `Decoder`).
- Zip-bomb defense: decompress with `Read::take(MAX_DECOMPRESSED_BYTES as u64 + 1)`; return `Err(DecompressionError::PayloadTooLarge)` if `buf.len() > MAX_DECOMPRESSED_BYTES`.
- Snapshot frames and mesh bundles compressed; `<id>.json` Git files remain plain JSON.
- License: `BSD-3-Clause` added to `cargo-deny` allowlist.

### 5.4 Anomaly detection

- Vendor online `WelfordAccumulator` (mean/std_dev/min/max over `f64`) + EWMA z-score detector (~45 LoC, mirroring `oxirs-shacl`'s Welford detector without `scirs2`).
- Track per-namespace: memory count rate, enrichment failure rate, per-stage latency.
- EWMA z-score > threshold (default 3.0) emits a structured `AnomalyEvent` to telemetry/log.
- Thread-safe: use `RwLock<HashMap<MetricKind, RollingStats>>`; `MetricKind` is a finite enum (`MemoryCount`, `EnrichmentFailure`, `Latency`) to bound keys.
- Zero-variance guard: when `std_dev <= f64::EPSILON`, treat `(value - mean).abs() <= EPSILON` as `z_score = 0.0`; otherwise clamp denominator to `min_std_dev = 1e-6`.
- Alternative: `rolling-stats = "0.8.0"` via Cargo as reference; if used, provide zero-variance guard, thread-safe wrapper, and `enum MetricKind`.

## 6. Interfaces

- Graph read:
  - `closure_subclasses(root: &str, index: &OntologyIndex, budget: &QueryBudget) -> ClosureResult<MemoryId>`
  - `closure_governs(root: &str, index: &OntologyIndex, edges: &[EdgeSpec], budget: &QueryBudget) -> ClosureResult<MemoryId>`
  - `pub struct QueryBudget { pub max_hops, max_nodes, max_edges, deadline }`
  - `pub struct BudgetExceeded { pub stage, pub visited, pub truncated, pub deadline_remaining_ms }`
- Compression:
  - `compress_snapshot(bytes: &[u8]) -> Vec<u8>` (zstd)
  - `decompress_snapshot(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError>`
  - `compress_mesh_bundle(bytes: &[u8]) -> Vec<u8>`
  - `decompress_mesh_bundle(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError>`
- Anomaly detection:
  - `WelfordAccumulator: add(sample: f64)`; `mean()`, `std_dev()`, `min()`, `max()`
  - `pub enum MetricKind { MemoryCount, EnrichmentFailure, Latency }`
  - `pub struct RollingStats { welford: WelfordAccumulator, ewma: f64, alpha: f64, z_threshold: f64, last_emit: Instant }`
  - `pub struct AnomalyEvent { metric: MetricKind, value: f64, mean: f64, std_dev: f64, z_score: f64, namespace: String, stage: String }`
- Configuration:
  - `closure_max_hops: usize` (default 10, but `related.rs` / `governs_max_hops` still caps at 2 default, max 5)
  - `closure_max_nodes: usize` (default 5000)
  - `closure_max_edges: usize` (default 50000)
  - `closure_deadline_ms: u64` (default 250)
  - `snapshot_max_decompressed_bytes: usize` (default 16 MiB, but for mesh snapshots use 1 MiB to align with `MAX_RECORD_BYTES`)
  - `anomaly_zscore_threshold: f64` (default 3.0)

## 7. Edge cases

- Self-loop (`subClassOf` to itself): cycle guard prevents infinite recursion.
- Deep chain (> `max_hops`): partial closure + `truncated: true` + `BudgetExceeded`.
- Disconnected entity (no subclasses): empty result set.
- Oversized mesh bundle (> `max_decompressed_bytes`): reject with `DecompressionError::PayloadTooLarge`.
- Zero-variance (`std_dev == 0.0`) division-by-zero in z-score: guard with `EPSILON` handling.
- First sample in accumulator: no anomaly (`count < 2`).
- Budget deadline reached mid-traversal: return partial results, do not block.
- Private/structural/task rows (`eligibility != "shareable"`): exclude from closure and validation results (inherited from prior specs).

### 7.1 Determinism guarantee (K3 review)

- **Deadline is a soft cap, not a determinism boundary.** The deterministic budget is `max_hops` / `max_nodes` / `max_edges`; `deadline` is advisory only and **never changes the result set** across identical inputs. If a deadline is hit, the traversal returns the same partial set it would have returned at the equivalent `max_hops`/`max_nodes`/`max_edges` cap — the truncation is purely a function of the hard caps, not wall-clock time.
- **Neighbor ordering is deterministic before budget cuts.** BFS/DFS expands neighbors in a stable, sorted order (sorted by canonical IRI / MemoryId) before any budget-based truncation. `ClosureResult.truncated` reflects only the hard-cap hit, not timing.
- **Anomaly detection is deterministic** (Welford/EWMA over fixed samples; no wall-clock dependence).
- **`governs_max_hops` default = 2** (aligns with `related.rs` default in `2hxl` spec; range `1..=5`).

## 8. Testing

- Unit: closure over known subClassOf* chain returns all descendants.
- Unit: cycle guard prevents infinite loop on self-referencing edge.
- Unit: `BudgetExceeded` returned when `max_hops` exceeded.
- Unit: `ClosureResult` carries `truncated: true` and partial `Vec<MemoryId>`.
- Unit: zstd compress/decompress snapshot roundtrip.
- Unit: `decompress_bounded` rejects payload > `max_decompressed_bytes` when `buf.len() > max`.
- Unit: Welford mean/std_dev match manual computation.
- Unit: zero-variance guard (all samples identical) => `z_score = 0.0`.
- Unit: EWMA z-score emits `AnomalyEvent` for injected outlier; no anomaly for normal data.
- Integration: closure applied to `related` graph traversal does not exceed deadline.
- Integration: `related_for_seed_with_budget` exists and passes `related_test.rs`.
- Performance: closure + budget stay within configured `deadline_ms`.

## 9. Acceptance

Bounded transitive closure over `subClassOf*` / `GOVERNS`-closure with per-query budget (warn-first, partial results, preserving `related.rs` constants); zstd compression of snapshots and mesh bundles with zip-bomb cap (no binary files in Git); online Welford + EWMA z-score anomaly detector (thread-safe, finite-key enum, zero-variance guard, telemetry/log only); all without `oxirs-core`, `scirs2`, `oxirs-shacl`, `oxiarc-zstd`, or `ruzstd`; zero LLM usage.

## 10. Files touched (expected)

- `src/rdf/ontology.rs`: add `closure_subclasses` and `closure_governs` with bounded BFS/DFS.
- `src/graph_read/related.rs`: add `related_for_seed_with_budget`; keep defaults (`2 / 20 / 40`) unchanged; add `pub struct QueryBudget` if `graph_read` is crate-free; add `pub struct BudgetExceeded`.
- `src/compression/zstd.rs` (new): `compress_snapshot`, `decompress_snapshot`, `compress_mesh_bundle`, `decompress_mesh_bundle` with `Read::take` cap and exact-boundary unit test.
- `src/telemetry/anomaly.rs` (new): `WelfordAccumulator`, `RollingStats` (with `MetricKind` enum and thread-safe `RwLock`), anomaly detection logic and `AnomalyEvent`.
- `Cargo.toml`: add `zstd = "0.14.3"` and `BSD-3-Clause` to license allowlist.
