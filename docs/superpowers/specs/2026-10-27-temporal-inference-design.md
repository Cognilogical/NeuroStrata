# Temporal Inference — Bounded Time-Window Reasoning Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-tjxf` (Temporal inference: bounded time-window reasoning, P3).

## 1. Problem

Memory relationships have temporal dimensions (`valid_from`, `valid_to`); need bounded time-window reasoning (e.g., which relationships held at time T?) without building temporal reasoner.

## 2. Goal

Bounded time-window reasoning over relation validity windows; deterministic, transient.

## 3. Constraints

- Deterministic: same time window → same result.
- No LLM.
- Library hygiene: `oxrdf` for temporal RDF projection; `sha2` for time-key hash; `yjah` QueryBudget for bounds; no `oxirs-core`/`scirs2`/`horned-owl`.
- Backward compatible: missing time fields default to unbounded.
- Warn-first: invalid window parameters reported.

## 4. Scope

- Validity window check per edge at query time.
- Bounded query window (start/end specified by user or default unbounded).
- Integration with `search_with_domain` / `related`.

## 5. Approach

- At query: filter edges where `valid_from <= T <= valid_to` (or unbounded if missing).
- Weight by window overlap; bounded by `QueryBudget`.
- No materialization; compute at query time.

## 6. Library evaluation

- `oxrdf`: temporal RDF (shared with `4gb2` RDF-star provenance).
- `sha2`: time-window key.
- `yjah` QueryBudget: bounded traversal.
- No new core libraries.

## 7. Per-namespace isolation
- Time-window reasoning per namespace; cross-namespace temporal queries only via `mhsp` inter-namespace traversal rules.
- References `neurostrata-9fnf` (canonical IRI / RDF projection), `2hxl` (OWL reasoning), `yjah` (QueryBudget), `4gb2` (RDF-star provenance / temporal), `mhsp` (inter-namespace).

## 8. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: temporal filter on edges.
- `src/graph_read/temporal.rs` (new): time-window reasoning.
- `Cargo.toml`: `oxrdf`, `sha2` (already present).