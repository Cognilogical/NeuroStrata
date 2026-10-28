# Enrichment Telemetry — Per-Stage Counts, Failures, Latency, Fingerprints Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-qylk` (Enrichment telemetry, P2, ranked #14, S-M effort).

## 1. Problem

Registry has a telemetry field but per-stage counts, failures, latency, and fingerprints are not recorded. Enrichment pipeline is opaque and hard to debug.

## 2. Goal

Record per-stage counts, failures, latency, and fingerprints for ingest/enrichment pipeline; surface in `doctor` and `task_validate`.

## 3. Constraints

- Deterministic: timestamps are monotonic, not wall-clock.
- No LLM in telemetry path.
- Library hygiene: use `std` + `tracing` for telemetry; no `oxirs-core`.
- Non-blocking: telemetry does not block reads or writes.
- Warn-first: telemetry failures reported, not fatal.

## 4. Scope

- Per-stage counts (ingest, enrichment, extraction).
- Failure counts and reasons.
- Latency per stage.
- Fingerprints for repeat detection.
- Integration with `doctor` and `task_validate`.

## 5. Approach

- Use `tracing` spans for per-stage timing.
- Store telemetry in `src/telemetry/` (new) with `RwLock<HashMap<String, StageStats>>`.
- Fingerprints: hash of input + extractor name to detect repeat runs.

## 6. Interfaces / Files Touched (expected)

- `src/telemetry/enrichment.rs` (new): per-stage stats, fingerprints, latency.
- `src/store/ladybug.rs`: telemetry hooks.
- `src/doctor.rs`: telemetry surface.
- `Cargo.toml`: `tracing = "0.1"` (already present).