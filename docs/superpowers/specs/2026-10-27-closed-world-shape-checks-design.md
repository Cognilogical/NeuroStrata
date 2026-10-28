# Closed-World Shape Checks for Edges and Required Fields Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-rzda` (Closed-world shape checks for edges and required fields, P2, ranked #18, S-M effort).

## 1. Problem

Validate GOVERNS / CONTAINS / RELATES_TO / EXTRACTED_FROM / DEPENDS_ON edges and required fields at write time. Currently silently skipped. OWL cannot do this; closed-world code can.

## 2. Goal

Add closed-world shape checks at upsert/write time for edge required fields; warn-first, not reject.

## 3. Constraints

- Deterministic: fixed schema per edge type, fixed field order.
- No LLM in validation path.
- Library hygiene: use `std` + manual schema; no `oxirs-core`/`oxirs-shacl`.
- Warn-first: invalid fields produce warnings, not errors.
- Backward compatible: pre-migration edges without fields read as default.

## 4. Scope

- Edge field presence and type checks.
- Required vs optional fields per relation table.
- Integration with `upsert`, `publish_generation`.
- Compatibility with `yjah` QueryBudget and `9fnf` canonical direction.

## 5. Approach

- Manual schema per relation table (`GOVERNS`, `CONTAINS`, `RELATES_TO`, `EXTRACTED_FROM`, `DEPENDS_ON`).
- Check at upsert time.
- Warn-first: return `ValidationWarning` with missing/erroneous fields; continue write with defaults.

## 6. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: upsert-time shape validation hooks.
- `src/doctor.rs`: surface shape warnings.
- `src/telemetry/enrichment.rs`: validate and emit warnings.
- `Cargo.toml`: no new deps; use existing `std` + `tracing`.