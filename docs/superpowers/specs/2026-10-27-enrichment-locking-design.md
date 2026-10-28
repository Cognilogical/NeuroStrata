# Enrichment Locking Per Namespace Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-bum5` (Enrichment locking per namespace, P2, ranked #24, S effort).

## 1. Problem

Enrichment locking per namespace is not implemented. A second enrichment call for the same namespace reports lock held and rerun passes. Prevents duplicate enrichment work.

## 2. Goal

Add per-namespace enrichment lock; non-blocking read, exclusive write.

## 3. Constraints

- Deterministic: lock state in memory, not in store.
- No LLM.
- Library hygiene: `std::sync::Mutex` or `tokio::sync::Mutex`; no `oxirs-core`.
- Warn-first: lock contention reported, not fatal.

## 4. Scope

- Per-namespace enrichment lock.
- Non-blocking reads.
- Lock release on completion or timeout.

## 5. Approach

- `HashMap<String, Mutex<()>>` per namespace.
- Acquire lock at enrichment start; release on completion or timeout.
- Warn on contention.

## 6. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: enrichment lock map.
- `src/enrichment/` (new): lock acquisition / release.
- `Cargo.toml`: no new deps; `std` + `tokio::sync`.