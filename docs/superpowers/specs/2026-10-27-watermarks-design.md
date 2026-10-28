# Watermarks — Record Last Ingested / Enriched With Fingerprints Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-svr0` (Watermarks: record last ingested/enriched with fingerprints, P2, ranked #13, S effort).

## 1. Problem

Ingest and enrichment pipelines lack watermarks (last ingested/enriched with fingerprints) to support incremental / idempotent runs.

## 2. Goal

Add watermark storage for ingest and enrichment runs, keyed by fingerprints (content hash, extractor hash).

## 3. Constraints

- Deterministic: fingerprints derived from content hashes, not wall clock.
- No LLM.
- Library hygiene: `sha2` for fingerprints; no `oxirs-core`.
- Backward compatible: missing watermarks mean full re-ingest (or no-op per `emrz` spec).

## 4. Scope

- Watermark record per namespace / file / stage.
- Fingerprint recording.
- Idempotent re-runs.

## 5. Approach

- `sha2` fingerprint over file content + parser version.
- Watermark keyed by namespace + file + stage.
- Reuse with `emrz` hash-reuse logic.

## 6. Interfaces / Files Touched (expected)

- `src/ingest/staged_publication.rs`: watermark records.
- `src/store/ladybug.rs`: watermark persistence.
- `Cargo.toml`: `sha2 = "0.10"` (already present).
