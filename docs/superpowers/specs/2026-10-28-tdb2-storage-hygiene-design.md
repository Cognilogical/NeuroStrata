# Storage Hygiene Design (TDB2 retired)

> Status: revised per Argon blockers — pending Argon re-review.
> Task: `neurostrata-rimc` (TDB2 / storage hygiene, OxiRS §1.20 storage track).
> Companion: `docs/superpowers/specs/2026-10-27-closure-budget-zstd-anomaly-design.md`, `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`.

## 1. Problem

The OxiRS §1.20 track proposed a TDB2 adapter, a live mmap snapshot cache, WAL `fsync` policy, and term interning. Repo facts make those proposals unsound:

- LadybugDB 0.20.4 already uses an on-disk buffer pool with virtual-memory mapping (`SystemConfig::default().max_db_size(...).buffer_pool_size(...)`, `src/store/ladybug.rs:917-930`). It is not a full in-memory graph loader.
- `lbug` exposes no `fsync` API; the only durability primitive is Cypher `CHECKPOINT`.
- The daemon already owns the single checkpoint loop (`src/daemon.rs:825-878`), with `CHECKPOINT_INTERVAL`, `is_dirty()` gating, and exponential backoff. Per-write checkpoints were removed because they deadlocked writers under load (`neurostrata-3fi.6.4`).
- Apache Jena TDB2 is a JVM library; no C FFI or Rust wrapper exists. `oxirs-tdb` depends on `oxirs-core`, which the prior Argon/K3 verdict banned for `ctor`/`rustls`/`scirs2` bloat.
- A separate live mmap cache would serve stale reads after writes and diverge from audit escape hatches (`include_archived`, `include_retired`).

Actual storage-hygiene gaps are narrower: checkpoint cadence/backoff is hard-coded, exported snapshots have no checksum/retention policy, and operators have no status surface for checkpoint state.

## 2. Goal

Add **LadybugDB storage hygiene** that:

- makes the daemon's single checkpoint loop configurable (interval + backoff cap),
- verifies exported snapshots with SHA-256 and enforces a retention count,
- exposes checkpoint/snapshot status via MCP and CLI without opening the database,
- keeps LadybugDB as the sole store,
- reuses the existing `VectorStore`, `GraphRead`, and `IntakeStore` traits,
- adds no TDB2 adapter, no alternate backend, no live mmap cache, no `MemoryPayload` type changes, and no stubs.

TDB2 is **formally retired**: no `tdb2` feature, no `Tdb2Backend`, no `StorageBackend` trait, no `src/storage/backend.rs`.

## 3. Constraints

- LadybugDB remains the sole store; no Apache Jena, JVM, `oxirs-tdb`, or alternate storage backend.
- Existing trait boundary (`src/traits.rs: VectorStore`, `GraphRead`, `IntakeStore`) is unchanged. No duplicate storage trait.
- Only the daemon checkpoint loop calls `checkpoint()`. No per-write `CHECKPOINT`, no `sync_wal`, no second fsync loop.
- Snapshot export reuses `VectorStore::export_database`; no new binary format and no live read cache.
- CLI status/export commands proxy to the daemon (single-writer/daemon-lock safety); only snapshot verification reads files directly.
- Nested `StorageHygieneConfig`; no flat `storage_*` top-level keys.
- No `unsafe` mmap code; no `memmap2`.
- Warn-first; no behavior stubs.

## 4. Scope

In scope:

- `StorageHygieneConfig` nested in `Config`.
- Configurable checkpoint interval and backoff cap for the daemon loop.
- `StorageStatus` tracking (dirty, last checkpoint time/duration, failure count).
- Snapshot export checksums (`checksums.json`) and retention pruning.
- `neurostrata_storage_status` MCP tool + `storage status` CLI.
- `storage export-snapshot`, `storage verify-snapshot`, `storage prune-snapshots` CLI/MCP surfaces.
- `src/storage/snapshot.rs`: pure helpers for checksums, manifest parsing, retention selection.

Out of scope (formally retired):

- TDB2 adapter, `storage_backend` config, `src/storage/backend.rs`, `StorageBackend` trait.
- Live mmap snapshot cache (`<db_path>.snapshot.mmap`).
- WAL `fsync` policy / `sync_wal`.
- Term interning in `MemoryPayload` or RDF `Quad` types.
- Cold-start benchmark that opens a second store while the daemon owns the lock.
- Cross-process/distributed/cloud storage.

## 5. Approach

### 5.1 TDB2 retirement

- Delete the proposed `tdb2` feature, `Tdb2Backend`, `StorageBackend`, and `src/storage/backend.rs` from the design.
- No code is written for TDB2; the task's TDB2 portion is retired, not deferred.
- Rationale recorded in this spec and as an extracted memory at task completion.

### 5.2 Config

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageHygieneConfig {
    pub checkpoint_interval_secs: u64,      // default 60, clamp 10..=3600
    pub checkpoint_backoff_max_secs: u64,   // default 60, clamp 5..=600
    pub snapshot_export_dir: Option<PathBuf>, // default None -> <data_dir>/snapshots
    pub snapshot_retention: usize,          // default 3, clamp 1..=50
    pub snapshot_verify_on_export: bool,    // default true
}
```

- `Config.storage_hygiene: StorageHygieneConfig` with `#[serde(default)]`.
- `checkpoint_interval_secs` replaces the hard-coded `CHECKPOINT_INTERVAL` constant used by the daemon loop.
- `checkpoint_backoff_max_secs` caps the existing exponential backoff; current behavior is retained when the cap is higher than the computed wait.
- Invalid values are clamped with a warning at startup, never a panic.

### 5.3 Checkpoint policy

- The daemon loop at `src/daemon.rs:825` remains the only checkpoint caller.
- It reads `StorageHygieneConfig.checkpoint_interval_secs` and `checkpoint_backoff_max_secs`.
- `is_dirty()` gating, final-shutdown checkpoint, and failure counting are unchanged.
- `StorageStatus` (shared `Arc<Mutex<...>>` or atomics):

```rust
pub struct StorageStatus {
    pub last_checkpoint_at: Option<DateTime<Utc>>,
    pub last_checkpoint_duration_ms: Option<u64>,
    pub checkpoint_failures: u32,
    pub checkpoint_interval_secs: u64,
    pub snapshot_export_dir: PathBuf,
    pub snapshot_count: usize,
}
```

- Updated after each checkpoint attempt; never on the write path.
- No `sync_wal`; no `CHECKPOINT` per write.

### 5.4 Snapshot export, checksums, retention

- `VectorStore::export_database` remains the exporter. It already runs `CHECKPOINT`, copies `ladybug.store`, copies `ladybug.store.wal` when present, and writes `manifest.json`.
- New `src/storage/snapshot.rs`:

```rust
pub fn verify_snapshot(dir: &Path) -> Result<SnapshotVerifyReport, SnapshotError>
pub fn write_checksums(dir: &Path) -> Result<SnapshotChecksums, SnapshotError>
pub fn select_expired_snapshots(root: &Path, keep: usize) -> Result<Vec<PathBuf>, SnapshotError>
```

- `SnapshotChecksums { ladybug_store_sha256, wal_sha256: Option<String>, manifest_sha256 }` is written to `checksums.json` next to `manifest.json` after a successful export.
- `verify_snapshot` recomputes SHA-256 (streaming, `sha2` already a dependency), compares to `checksums.json`, and validates `manifest.json` has `format == "neurostrata-snapshot"` and `version == 1`.
- `select_expired_snapshots` only considers directories whose `manifest.json` has `format == "neurostrata-snapshot"`; it never selects the live DB directory, the configured export dir itself, or a directory without a valid manifest.
- Retention: after a successful export, keep the newest `snapshot_retention` valid snapshots; delete older ones recursively with `remove_dir_all`, warn-first. A failed export or failed verification never triggers pruning.
- Snapshot directories are named `snapshot-<UTC timestamp>`; timestamp comes from `created_at` in the manifest for ordering, not filesystem mtime.

### 5.5 Storage status surfaces

MCP tool:

```
neurostrata_storage_status() -> {
  dirty: bool,
  checkpoint_interval_secs: u64,
  last_checkpoint_at: Option<String>,
  last_checkpoint_duration_ms: Option<u64>,
  checkpoint_failures: u32,
  snapshot_export_dir: String,
  snapshot_count: usize
}
```

CLI:

- `neurostrata-mcp storage status` — proxies to the daemon via `POST /storage/status`; if the daemon is not running, prints a clear error (does not open the DB).
- `neurostrata-mcp storage export-snapshot` — proxies to daemon; daemon runs `export_database`, writes checksums, prunes.
- `neurostrata-mcp storage verify-snapshot --dir <path>` — pure file check, no daemon/DB needed.
- `neurostrata-mcp storage prune-snapshots --keep <n>` — proxies to daemon; validates manifests, deletes oldest beyond `n`.

Daemon endpoints:

- `POST /storage/status` → status JSON.
- `POST /storage/export-snapshot` → export + checksum + prune.
- `POST /storage/prune-snapshots` → prune only.

### 5.6 Removed/rejected mechanisms

- **No live mmap cache**: LadybugDB already mmaps; serving from a separate static snapshot would be stale and inconsistent. All reads continue through `LadybugStore`.
- **No WAL fsync policy**: `lbug` has no fsync API; the existing checkpoint loop is the durability boundary.
- **No term interning**: changing `MemoryPayload` strings to `Arc<str>` is a breaking change across `server.rs`, `rdf/`, `synaptic_mesh/`, and tests; RDF `Quad` strings are per-query allocations and not the dominant RSS source. If interning is ever justified by profiling, it belongs in `src/rdf/` as a separate task, not in this spec.
- **No storage benchmark**: opening a second `LadybugStore` while the daemon holds the lock is unsafe. `storage status` reports checkpoint timings instead.

## 6. Interfaces

- `src/config.rs`: `StorageHygieneConfig`, `Config.storage_hygiene`.
- `src/daemon.rs`: checkpoint loop reads config; `StorageStatus`; `/storage/*` handlers.
- `src/store/ladybug.rs`: unchanged trait surface; reuse `checkpoint`, `is_dirty`, `export_database`.
- `src/storage/snapshot.rs` (new): `verify_snapshot`, `write_checksums`, `select_expired_snapshots`, `SnapshotError`, `SnapshotChecksums`, `SnapshotVerifyReport`.
- `src/server.rs`: `neurostrata_storage_status` MCP tool.
- `src/main.rs`: `storage status|export-snapshot|verify-snapshot|prune-snapshots` CLI subcommands.
- `Cargo.toml`: no new dependencies (`sha2` already present; no `memmap2`).
- Removed from scope: `src/storage/backend.rs`, `Tdb2Backend`, `StorageBackend`, `storage_backend`, `tdb2` feature, `memmap2`, `TermInterner`.

## 7. Edge cases

- Daemon not running: CLI status/export/prune returns an error telling the operator to start the daemon; verify-snapshot still works.
- Checkpoint failure: existing backoff applies, capped by `checkpoint_backoff_max_secs`; status records the failure.
- Export failure: no checksums, no pruning, existing snapshots untouched.
- Checksum mismatch: `verify_snapshot` reports `ChecksumMismatch`; no deletion.
- Corrupt/missing manifest: snapshot is not eligible for retention pruning; warn.
- Retention `0`: clamped to 1 (never delete the newest snapshot).
- Concurrent export: daemon serializes exports with a mutex; a second request returns `ExportInProgress`.
- Snapshot dir on another filesystem: export still uses `std::fs::copy`; errors propagate.
- No `MemoryPayload`, `Quad`, or `VectorStore` type changes.

## 8. Testing

- Unit: `StorageHygieneConfig` clamps and defaults.
- Unit: `write_checksums` + `verify_snapshot` round-trip; tampered file → `ChecksumMismatch`.
- Unit: `select_expired_snapshots` keeps newest N, ignores invalid manifests, never selects live DB.
- Unit: `checkpoint_backoff` cap.
- Integration: daemon checkpoint interval from config (short interval in test).
- Integration: `/storage/status` reports dirty/checkpoint fields.
- Integration: export → checksums exist → retention prunes oldest.
- Integration: CLI status proxies to daemon and does not open the DB.
- Regression: no `Tdb2Backend`, `StorageBackend`, `memmap2`, or `sync_wal` symbols exist.

## 9. Acceptance

Checkpoint cadence/backoff configurable; snapshot exports checksummed and retention-pruned; storage status available via MCP/CLI; LadybugDB sole store; existing traits unchanged; TDB2 formally retired; no live mmap cache; no `MemoryPayload` changes; no stubs; no new dependencies.

## 10. Files touched (expected)

- `src/config.rs`: nested `StorageHygieneConfig`.
- `src/daemon.rs`: checkpoint config, `StorageStatus`, `/storage/*` routes.
- `src/server.rs`: `neurostrata_storage_status` tool.
- `src/main.rs`: `storage` CLI subcommands + daemon proxy.
- `src/storage/snapshot.rs` (new): checksums, verification, retention selection.
- `src/storage/mod.rs` (new or updated): module wiring.
- `Cargo.toml`: no change.
- **Not touched**: `src/traits.rs`, `src/storage/backend.rs` (does not exist), `MemoryPayload`, RDF `Quad` types.