# Plan 4: Graph Schema, Provenance & Diagnostics Implementation Plan (Revised)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** Complete F7 (DEPENDS_ON wiring), add F6 (edge_sources provenance on Memory rows), F13 (dangling-edge report), F18 (closed-world shape checks). All warn-first; never reject.

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (§2.1, §3 F6/F7/F13/F18, §5 Plan 4)

**Vetting:** Argon APPROVE WITH CHANGES (2026-10-28), K3 REVISE BEFORE IMPLEMENTATION (2026-10-28). This revision incorporates both.

## Global Constraints
- JSON only; no LLM.
- Zero new dependencies (sha2, serde_json, std only).
- Warn-first: never reject writes or block gate on diagnostics.
- Per-namespace isolation.
- Backward compatible: rows lacking edge_sources/shape fields read cleanly.

## Verified Baseline (do NOT redo)
- F7 partial: vocabulary v9 + DEPENDS_ON rel table + traversal landed (traits.rs, ladybug.rs:1812, related.rs).
- Gaps: parse_edge_specs omits depends_on; vocabulary DEPENDS_ON entry uses "description" not "definition" and lacks metadata_key/declaration_direction; export_graph omits DEPENDS_ON; tree-sitter import capture absent; PARSER_LOGIC_VERSION still 1.

---

## Task 1: Complete F7 (DEPENDS_ON wiring)

**Files:** src/store/ladybug.rs, src/schemas/memory-vocabulary.v3.json, src/schema.json, src/parser/ingest.rs, src/parser/generation.rs.

- [ ] Add `("depends_on", "DEPENDS_ON", true)` to parse_edge_specs loop (ladybug.rs ~1224).
- [ ] Fix vocabulary DEPENDS_ON entry: use "definition", add "metadata_key": "depends_on", "declaration_direction": "self_to_target".
- [ ] Add DEPENDS_ON query to export_graph links (ladybug.rs ~3091-3156).
- [ ] Add import queries to src/schema.json; populate metadata.depends_on in ingest.rs (resolved module/file paths where unambiguous).
- [ ] Bump PARSER_LOGIC_VERSION 1 → 2 (generation.rs:100) so F2 version gate forces re-parse.
- [ ] Tests: parsed import creates DEPENDS_ON edge; bounded related() reaches dependency; export_graph includes DEPENDS_ON.

## Task 2: F6 edge_sources provenance

**Files:** src/store/ladybug.rs, src/traits.rs (search output), src/synaptic_mesh/worker.rs (exclude), tests.

**Resolved-declaration record (shared across upsert/relink/doctor):**
- `{ declaring_id, rel_type, declaration_index, declared_target, resolved_target, logical_target, orientation, file, content_hash, line_start, line_end, extractor }`
- `logical_target` = `logical_path_of(resolved_target)` (generation.rs:77), generation-invariant.
- `orientation`: self_to_target (relates_to, governs, depends_on, extracted_from) or target_to_self (contained_by, where self=contained, target=container).

**Provenance lifecycle:**
- Upsert: stamp edge_sources on declaring row for every resolved declaration, even if physical edge pre-existed or another row created it. Creation count must NOT gate provenance.
- Replace by `(rel_type, declaration_index, logical_target)` key; dedupe identical entries.
- Prune entries for removed/changed declarations (relink idempotent: repeated relink byte-identical).
- `contained_by` reverse orientation handled: self is contained, each target in contained_by is container.

**Source fields:**
- Ingest-generated edges: file hash (file_sha256/span_sha256), lines from tree-sitter.
- Directories + manual declarations without file hash/span: source fields null/absent; provenance still stamped with available fields (no "best-effort" ambiguity — explicitly nullable).

**Read surfaces:**
- search_memory and export_graph (nodes) surface edge_sources automatically (full metadata already returned).
- export_graph links include DEPENDS_ON.
- SynapticMesh projected_metadata (worker.rs:283) EXCLUDES edge_sources (local paths/hashes must not leak).

**Backward compat:** rows without edge_sources read/search/validate cleanly.

**Tests (required):** late target then relink; pre-existing edge; same edge declared by two rows; reverse CONTAINS; relink/re-ingest idempotence; declaration removal/reorder; changed hash; generation-independent logical target; search/export visibility; every ingest-generated edge joined to declaring row's citation.

## Task 3: F13 dangling-edge report

**Files:** src/main.rs (doctor), src/server.rs (snapshot auto-doctor + handle), src/task/gate.rs (GateReport).

- Shared deterministic helper: unresolved declarations across GOVERNS, RELATES_TO, DEPENDS_ON, CONTAINS, EXTRACTED_FROM.
- Report per declaration: `{ rel_type, declaring_id, declaration_index, declared_target, reason }` where reason ∈ {missing, ambiguous, unresolvable}.
- Reuse write/relink resolution semantics (qualified targets, resolvable legacy paths) — do NOT count resolvable legacy as dangling.
- Retired/archived declaring rows excluded; active KnownIds only.
- Doctor (main.rs + server.rs) surfaces `dangling_edges: { rel_type, count, samples }` where `samples` is a deterministic array of full per-declaration records `{ rel_type, declaring_id, declaration_index, declared_target, reason }`, sorted by `(rel_type, declaring_id, declaration_index, declared_target, reason)`, take(5). `count` is total unresolved declarations.
- task_validate (validate_json) adds `dangling_edges: { total, by_relation: [{rel_type, count}], samples: [{rel_type, declaring_id, declaration_index, declared_target, reason}] }` to GateReport. **gate_json() / ok() UNCHANGED** — never blocks git push. Warn-only.
- Reporting is read-only (no mutation of declarations or rows).

**Tests (required):** one unresolved declaration per each of five relations; ambiguous target; legacy-resolvable and qualified-target controls (not counted); exact count/sample identity; deterministic ordering; no mutation; gate remains green.

## Task 4: F18 closed-world shape checks

**Files:** src/store/ladybug.rs (write_shape_warnings + rdf/shacl.rs unify), src/task/gate.rs (shape_warnings advisory).

- Unify validators: single shared shape module used by upsert, relink, task_validate. Remove duplicate topology/attempt checks between ladybug.rs and rdf/shacl.rs.
- Endpoint role matrix:
  - GOVERNS: source=rule, target=artifact (file, directory, markdown, symbol, code_ast, doc_section).
  - CONTAINS: container (directory, file, markdown) → contained (directory, file, markdown, symbol, code_ast, doc_section). Respect contained_by inverse: self=contained, targets=containers.
  - EXTRACTED_FROM: source=non-structural memory (rule, fact, procedure, custom), target=task.
  - Custom types: configurable/allowlisted per vocabulary; default allow.
- Required fields per memory_type (shared table/schema, not prose):
  - rule: metadata.source non-blank; metadata.enforcement (if present) ∈ {ENFORCED, PARTIAL, NOT_ENFORCED}.
  - procedure: metadata.trigger (valid enum or every-n-sessions:N / every-n-days:N); metadata.performance_count (int if present).
  - task: metadata.task object with valid status.
  - fact: validate topology/attempt/decision keys per vocabulary when present.
- Warn-first: emit [neurostrata:write-shape] stderr + include in UpsertReport warnings; persist row+edge. task_validate exposes `shape_warnings` advisory array; never add to GateReport.violations.

**Tests (required):** wrong source/target for each constrained relation; warning contains row ID + reason; row+edge still persist; valid + custom-type controls; required-field matrix; task_validate warns without blocking.

## Task 5: Plan 5 prerequisite

- Author docs/superpowers/plans/2026-10-27-plan5-domain-facets-mesh.md reconciling already-landed src/rdf/ontology.rs & src/domain.rs against remaining F9/F15/F16/F17/F20/F21 before Plan 5 execution. (ns-fui0)

## Execution Handoff
Execute natively after Argon+K3 re-vet of this revised plan.

