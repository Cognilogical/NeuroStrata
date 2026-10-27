# Plan 4: Graph Schema, Provenance & Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** Add DEPENDS_ON relation (F7), edge_sources provenance (F6), dangling-edge report (F13), closed-world shape checks (F18).

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (F6, F7, F13, F18)

## Tasks

### Task 1: F7 schema — DEPENDS_ON relation
- Add CREATE REL TABLE DEPENDS_ON in LadybugStore::init.
- Add DEPENDS_ON to vocabulary rels (traits.rs) and bump vocabulary_version 8→9.
- related traversal includes DEPENDS_ON.
- Tree-sitter ingest captures imports → DEPENDS_ON edges (basic).

### Task 2: F6 edge_sources metadata
- Support metadata.edge_sources on Memory rows (already JSON, no schema change).
- Stamp edge_sources in relink / declaration processing (best-effort).
- Surface in search_memory output.

### Task 3: F13 dangling-edge report
- doctor and task_validate report unresolved GOVERNS/RELATES_TO/DEPENDS_ON targets with count + sample IDs. Warn-only.

### Task 4: F18 shape checks
- Validate GOVERNS (Rule→Artifact), CONTAINS (container→contained), EXTRACTED_FROM (Memory→Task) endpoints.
- Validate required fields per memory type.
- Warn-first, never reject.

## Execution Handoff
Execute natively.
