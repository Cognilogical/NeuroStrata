# Plan 6: Enrichment Pipeline, Distillation & Signals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** Enrichment state machine (F12 watermarks, F14 telemetry, F24 locking), F5 GLiNER scaffolding, F10 conflict candidates, F22 distillation, F23 feedback weighting.

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (F5, F10, F12, F14, F22, F23, F24)

## Tasks

### Task 1: F23 feedback weighting
- Add neurostrata_feedback tool (id, rating -1..1).
- Store feedback_score in metadata.
- Neural gain capped term.

### Task 2: F10 conflict candidates
- Heuristic: two active rules/facts with same location or topology with opposing enforcement/decision, or high cosine + opposite negation tokens.
- Add disputed/disputed_with metadata.
- Surface in recall + snapshot as warning badge. Never auto-resolve.

### Task 3: F12/F14/F24 enrichment state
- Enrichment state record on registry row: {state, watermark, lock_holder, telemetry}.
- Lock per namespace; second call returns lock_held.
- Watermark skip.
- Doctor exposure.

### Task 4: F22 distillation
- distill command scanning append_log output → candidate rules/tasks (review-only).

### Task 5: F5 GLiNER (feature-gated)
- Opt-in enrichment stage; candidates to IntakeCandidate.

## Execution Handoff
Execute natively.
