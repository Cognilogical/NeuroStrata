# Cognee → NeuroStrata: Concept & Technique Transfer Analysis

> **Review status (2026-10-27):** this report was adjudicated by Argon and K3.
> Several claims were corrected — most importantly §1.2 (incremental ingest
> already has generations and hashes; the gap is carry-forward), §1.3 (edge
> properties require an lbug migration), §1.5 (OWL/RDF is killed), §1.7
> (conflict detection is candidates-only), and §1.8 (directed calls need a
> real schema migration). Read the adjudication first:
> `docs/designs/2026-10-27-argon-k3-gliner-cognee-verdict.md`.

Source: https://github.com/topoteretes/cognee (v1.6.x, 2026-10-27).  
NeuroStrata baseline: v1.8.x, Rust MCP server, LadybugDB graph+vector, deterministic retrieval (no LLM in the read path), 3-tier namespace stratification, bi-temporal memory, neural-gain salience, procedural memory, SynapticMesh.

Goal: identify cognee concepts/techniques that could enhance NeuroStrata, ranked by fit with NeuroStrata's philosophy (deterministic, local, code-aware, LLM-free read path).

## 0. Executive summary

Cognee and NeuroStrata solve overlapping problems (persistent agent memory) but from opposite poles: cognee is a Python LLM-heavy knowledge-graph builder with session distillation and completion-style answers; NeuroStrata is a Rust, deterministic, code-centric memory engine with task/procedure coupling and a pre-push gate. The highest-value transfers are **not** cognee's LLM completion machinery, but its **deterministic ingestion/retrieval hygiene**: exact-lexical recall, incremental re-ingest, edge evidence citations, presort/dry-run, ontology grounding, node-set project scoping, and conflict review over dated facts. Several of these map directly onto gaps already documented in NeuroStrata (exact-identifier recall, dedup score bug, recall truncation).

## 1. Tier 1 — High value, strong fit (recommend filing tasks)

### 1.1 Exact-lexical / phrase recall (CHUNKS_LEXICAL)
- **Cognee**: `SearchType.CHUNKS_LEXICAL` — keyword/exact-phrase match, no LLM. Router auto-selects when the whole query is one `"quoted phrase"`.
- **NeuroStrata gap**: explicitly documented as NOT IMPLEMENTED — "Exact-identifier recall — the known gap" (docs/COGNITIVE_ARCHITECTURE.md §6). Dense embeddings miss exact identifiers (function names, env vars, config keys, error strings).
- **Adaptation**: implement the already-recommended narrow deterministic `CONTAINS`/symbol-id match reusing existing `symbol` rows and relations — no new dependency. Add a `lexical`/`exact` flag to `neurostrata_search_memory` (or a separate tool) that does substring/symbol-id match before/alongside vector scan.
- **Effort**: M. **Priority**: P1 (closes a documented correctness gap).

### 1.2 Incremental re-ingest of changed files
- **Cognee**: re-remembering the same path with changed content raises `DocumentUpdateRequiredError`; `cognee.update(data_id, data)` re-extracts only changed chunks, preserving document id. Identical content is a no-op.
- **NeuroStrata gap**: `neurostrata_ingest_directory` likely full re-ingest; no change detection / chunk-level diff.
- **Adaptation**: store content hash per file (already have `get_file_content_hash` utility concept); on re-ingest, skip unchanged files, and for changed files supersede old symbol nodes (bi-temporal `valid_to`) and add new ones. Reuse tombstone + supersede machinery.
- **Effort**: M. **Priority**: P1 (avoids duplicate/stale nodes, reduces ingest cost).

### 1.3 Edge evidence / source citations on retrieved rows
- **Cognee**: `include_references=True` attaches the document chunks supporting each graph edge used (requires edge evidence, default on). Provenance `external_metadata` stamped on every chunk and surfaced in hybrid retrieval.
- **NeuroStrata gap**: graph expansion carries evidence *paths* (relation traversal) but not the originating source chunk/text that justified the edge.
- **Adaptation**: store `source_ref`/`source_chunk_id` on `RELATES_TO`/`GOVERNS` edges and surface them in `search_memory` output and `export-graph`. Complements existing provenance metadata.
- **Effort**: S–M. **Priority**: P2.

### 1.4 Presort + dry-run before ingest
- **Cognee**: `dry_run=True` returns token/cost estimate without ingesting or calling LLM; `dry_run="presort"` produces a `PresortReport` (junk, duplicates, version candidates, possible PII, proposed dataset groups) appliable later.
- **NeuroStrata gap**: no pre-ingest cost/quality preview; secret scanner exists but no junk/dup/PII report.
- **Adaptation**: add `dry_run` flag to ingest that computes counts, estimated embed cost, duplicate-file detection, and PII/secret hits without writing. Useful for agent safety and budget control.
- **Effort**: S. **Priority**: P2.

### 1.5 Ontology grounding (annotate vs strict)
- **Cognee**: `config={"ontology_config": {...}}` with `RDFLibOntologyResolver`; modes `annotate` (enrich only) vs `strict` (drop entities with no ontology match); prunes graph only, chunk text still stored. `ONTOLOGY_FILE_PATH` env.
- **NeuroStrata gap**: domain tags are free-form slugs; no controlled vocabulary/ontology. Could reduce synonym drift ("frontend" vs "ui" vs "web").
- **Adaptation**: optional ontology file (OWL/RDF or simpler JSON vocabulary) loaded per namespace; `annotate` mode adds canonical type to metadata; `strict` mode filters memories at ingest. Keep optional and local.
- **Effort**: M–L. **Priority**: P2 (nice-to-have; domain registry already partially addresses).

### 1.6 Node sets / project scoping beyond namespace
- **Cognee**: `node_set=["AI","FinTech"]` tags data; recall filters via `node_name=[...]`; session pinning with conflict detection.
- **NeuroStrata gap**: `domain` is a single soft tag; no multi-label node sets or recall filtering by project subset within a namespace.
- **Adaptation**: allow `node_set` array on `add_memory`/ingest; store as metadata array; `search_memory` accepts `node_set` filter (AND/OR). Useful for monorepos / multi-project namespaces.
- **Effort**: S. **Priority**: P2.

### 1.7 Conflict review over dated facts
- **Cognee**: `review_conflicts` stage reviews dated source facts, stores conflicts, rewrites entity descriptions; uses effective dates from document metadata; fixed attributes remain disputed even when one doc is newer.
- **NeuroStrata gap**: bi-temporal `valid_to` soft-deletes but no explicit conflict detection when two active memories contradict; no "disputed" state.
- **Adaptation**: add a conflict-detection pass (can be LLM-assisted offline or deterministic heuristic) that marks pairs of active memories sharing an entity with contradictory predicates as `disputed`, surfaced in recall and snapshot. Fits NeuroStrata's bi-temporal model.
- **Effort**: M. **Priority**: P2–P3.

### 1.8 Code graph (deterministic symbol/dependency graph)
- **Cognee**: code files go down a deterministic graph route (no LLM), searchable via `SearchType.CODE` with `code_query={...}`. Tree-sitter based.
- **NeuroStrata gap**: ingests AST symbols but retrieval is generic vector over symbol nodes; no call/dependency traversal or code-specific query operators.
- **Adaptation**: extend ingest to capture import/call edges as `CONTAINS`/`CALLS` relations; add a code-scoped search mode that traverses dependency graph. NeuroStrata already has graph expansion; mainly needs richer edge types from parser.
- **Effort**: M–L. **Priority**: P2.

## 2. Tier 2 — Medium value, needs design

### 2.1 Session memory + distillation into durable rules
- **Cognee**: fast session cache (turns, traces, feedback) keyed by session_id; `improve()` bridges session content into permanent graph via distillation stages (persist_session_qa, persist_agent_traces, extract_agent_context, distill_sessions).
- **NeuroStrata**: has task notes, append_log, episodic buffer pointers, procedures. No fast session cache separate from task history; no distillation from session traces to durable rules.
- **Adaptation**: optional session cache layer (SQLite/Redis) for conversation turns; a `distill` command that extracts candidate rules from session logs into task follow-ons or memories. Aligns with existing task/procedure model. High effort but high payoff for long sessions.
- **Effort**: L. **Priority**: P3.

### 2.2 Feedback-driven weighting
- **Cognee**: `feedback_weights` stage adjusts graph element weights from rated answers (FeedbackEntry with score); `AUTO_FEEDBACK` detects implicit feedback via structured LLM.
- **NeuroStrata**: neural gain uses `access_count` only; no explicit feedback signal. Could add `feedback_score` to memory metadata and incorporate into salience (capped, like access gain).
- **Adaptation**: add `feedback_score`/`rating` to memory metadata; salience formula gains a small capped term; optional `neurostrata_feedback` tool to record ratings. Keep deterministic (no LLM) for core; feedback detection can be agent-driven.
- **Effort**: S–M. **Priority**: P3.

### 2.3 Global context index (hierarchical summaries)
- **Cognee**: `global_context_index` stage creates bucket + root summaries for global questions; `GRAPH_REPORT` search type gives hubs, cross-set links, suggested questions.
- **NeuroStrata**: `get_snapshot` returns top-5 by access; no hierarchical summary or hub report.
- **Adaptation**: generate and store summary memories at directory/namespace level (already have markdown/file nodes); `get_snapshot` could include a root summary node. Lower priority since snapshot already grounds agents.
- **Effort**: M. **Priority**: P3.

### 2.4 Temporal reranking
- **Cognee**: `TEMPORAL` search type reranks hybrid retrieval by dates in question; effective dates from document metadata.
- **NeuroStrata**: bi-temporal valid_from/valid_to filters expired; no date-aware reranking by query dates. Could parse dates in query and boost memories with matching `valid_from` windows.
- **Effort**: M. **Priority**: P3.

### 2.5 Skill gate / procedural auto-surface
- **Cognee**: when query sounds procedural, recall runs a small `SKILLS` lookup and appends hits.
- **NeuroStrata**: has procedures with triggers; `get_snapshot` surfaces `procedures_due`. Could auto-match procedures to a search query by trigger/description similarity.
- **Effort**: S. **Priority**: P3.

### 2.6 Watermarks / idempotent enrichment
- **Cognee**: watermarks track persisted counts per session/dataset so repeat `improve()` runs report `already_completed` cheaply.
- **NeuroStrata**: task completion + memory extraction; no watermark for enrichment passes. Could add `last_enriched_at`/counts to avoid redundant work.
- **Effort**: S. **Priority**: P3.

### 2.7 Per-dataset/session lock for enrichment
- **Cognee**: improve lock per dataset/session; second call returns `lock_held` with rerun passes.
- **NeuroStrata**: task claims provide exclusivity for work; enrichment (if added) should follow same pattern.
- **Effort**: S. **Priority**: P3 (only if enrichment added).

## 3. Tier 3 — Low fit / misaligned (do not adopt as-is)

- **LLM completion search types** (GRAPH_COMPLETION, COT, RAG_COMPLETION, FEELING_LUCKY, AGENTIC_COMPLETION): NeuroStrata deliberately keeps retrieval LLM-free and deterministic. Adopting would violate core design. Could be offered as an *optional* client-side layer, not in core.
- **Multi-tenancy / permissions**: NeuroStrata uses namespaces + SynapticMesh sharing; per-user DB isolation is a different trust model. Not needed.
- **Truth subspace**: LLM-built; unclear mapping to deterministic memory. Skip unless a concrete use case emerges.
- **COGX exchange format**: migration between memory systems; low priority unless interoperability requested.
- **Frontend/UI**: NeuroStrata has NeuroVault/Obsidian; cognee's Next.js UI is large and not needed.

## 4. Recommended next actions

1. File tasks for Tier 1 items 1.1 (lexical recall) and 1.2 (incremental ingest) first — both close documented gaps.
2. Then 1.3 (edge evidence), 1.4 (dry-run), 1.6 (node sets) as small enhancements.
3. Defer Tier 2 to a design spec; do not implement speculatively.

## 5. Cross-reference to existing NeuroStrata decisions

- Exact-identifier gap: docs/COGNITIVE_ARCHITECTURE.md §6 (NOT IMPLEMENTED).
- Dedup score bug: memories 0e242aec, 060b95f4 (use unboosted raw distance).
- Recall truncation bug: memory 02a79f52.
- Bi-temporal: §2.
- Neural gain: §3.
- Procedural memory: §12.

---
*Analysis performed 2026-10-27. Source repo shallow-reviewed via README, AGENTS.md, and .agents/skills/* (improve-sessions, recall, custom-graph-models, ingestion).*
