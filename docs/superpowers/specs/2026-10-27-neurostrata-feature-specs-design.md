# NeuroStrata Feature Specs (JSON-First)

Date: 2026-10-27. Design only. No implementation authorized.

Status: revised per Argon review (approve with changes). Awaiting user review.

## 1. Scope and constraints

- JSON only. No OWL/RDF, no reasoner, no RDF store, no pilot.
- Deterministic read path: no LLM in search, dedup, guard, or routing.
- Single store: LadybugDB. No second database in normal installs.
- Preserve bi-temporal memory, 3-tier namespaces, SynapticMesh privacy rules, and the pre-push task gate.
- All new structured data is JSON conforming to `src/schemas/memory-vocabulary.v3.json` (internal `vocabulary_version` bumps from 8 to 9 when `DEPENDS_ON` lands).
- GLiNER and other models stay write-side and review-only.
- Every feature must be falsifiable with a metric or a regression test.

Non-goals: LLM completion search types, multi-tenancy, truth subspace, COGX exchange format, frontend/UI, Python sidecar in normal installs, directed calls faked as undirected relations, strict ingestion dropping, OWL/RDF.

## 2. Common contracts

### 2.1 Memory row

- `memory_type`: existing types plus inferred facet tags stored in `metadata.facets: string[]` (not new top-level types), with one exception: embedded markdown sections use `memory_type: "doc_section"` so they are not excluded by `STRUCTURAL_MEMORY_TYPES`.
- `metadata.domain`: `string | string[]` after F9.
- `metadata.node_set`: `string[]` after F9.
- `metadata.concept_iri`: optional string for cross-namespace alignment (F20).
- `metadata.facets`: `string[]` (F16).
- `metadata.feedback_score`: number in [-1, 1] (F23).
- `metadata.edge_sources`: `Array<{ rel_type, declaration_index, logical_target, file, content_hash, line_start, line_end, extractor }>` on the declaring Memory row. LadybugDB relation tables are property-less, so provenance lives on the node (F6).
- `metadata.as_of` support: search accepts `as_of: Option<i64>` to filter by bi-temporal `valid_from`/`valid_to` (deterministic replacement for killed temporal reranking).

### 2.2 Domain registry

Stored as the namespace's registry row with `eligibility: private`. Shape after F15 (v2), with backwards-compatible v1:

```json
{
  "domain_registry": {
    "version": 2,
    "domains": [
      { "slug": "auth", "parent": null, "aliases": ["authentication"], "concept_iri": null },
      { "slug": "tls", "parent": "auth", "aliases": ["ssl"], "concept_iri": null },
      { "slug": "mtls", "parent": "tls", "aliases": ["mutual-tls"], "concept_iri": "https://..." }
    ]
  }
}
```

Invariants: slugs normalized via `normalize_domain_slug`; no cycles (DAG check); aliases unique within the namespace. The deserializer accepts v1 (`domains: string[]`, mapped to `{slug, parent: null, aliases: []}`) and v2 (objects).

### 2.3 Search evidence

`SearchEvidence` (`src/traits.rs:66-104`) keeps `kind`, `path`, `explanation` and gains `derivation: Vec<String>` listing the traversal/ontology steps that produced an expanded row. Transient only; never persisted on the memory row.

### 2.4 Ingest generations and enrichment state

- Each ingest run stamps `index_generation` (u64) on nodes and edges (existing `IngestGeneration` pointer in `src/parser/generation.rs`).
- Unchanged files reuse the prior generation's nodes and embeddings via cross-generation carry-forward when `file_sha256` and the pipeline fingerprint (parser version, vocabulary version, embedder descriptor) match. No `valid_to` superseding of AST symbols.
- Enrichment state is a single record on the namespace registry row: `{ state: "pending" | "running" | "done" | "failed", watermark, lock_holder, telemetry }`. F12, F14, and F24 share this record.

## 3. Feature specifications

### F1. Search exact names

Problem: embeddings miss exact identifiers.

Requirements:
1. Thread `query_text: Option<&str>` and `exact: bool` into `VectorStore::search_with_domain` / `search_inner`.
2. Run a deterministic pre-pass matching `m.location`, `metadata.symbol`, or an exact quoted substring. Synthetic distance `0.0` so matches sort ahead of vector hits.
3. `derivation: ["exact-name"]`.
4. Add `exact: bool` (default true) and `as_of: Option<i64>` to `neurostrata_search_memory`.
5. Respect `STRUCTURAL_MEMORY_TYPES` exclusion for file/symbol containers; match against their metadata/location only.

Acceptance: Recall@5 on a 30-query identifier benchmark improves by at least 20 points over baseline vector-only; zero regressions on existing direct-match queries.

### F2. Re-ingest only what changed

Problem: full re-ingest re-embeds and rebuilds unchanged nodes.

Requirements:
1. Use existing `file_sha256` and `span_sha256` (`src/parser/ingest.rs`).
2. Skip files whose hash and pipeline fingerprint match the previous active generation.
3. For changed files: add new nodes in the new staging generation; carry forward unchanged file/symbol nodes and their embeddings into the new generation.
4. Re-parse when parser version or vocabulary version changes even if the file hash matches.
5. Report counts: files skipped, changed, added, removed, embeddings reused.

Acceptance: re-ingesting an unchanged tree produces zero new nodes and zero new embeddings; a one-line change touches only that file's symbols.

### F3. Make docs real

Problem: markdown files stored as empty `Path: ...` rows.

Requirements:
1. Parse markdown into sections by heading.
2. Extract backticked symbols, file paths, and links.
3. Store embedded sections as `memory_type: "doc_section"` (non-structural) with ID `path#section:slug@sha256` and `CONTAINS` edge from the file-level `markdown` container.
4. Embed the section text.
5. Connect to the graph via existing `CONTAINS`/`RELATES_TO`.

Acceptance: a search for a backticked symbol in a doc returns the doc section; `memories_for_file` returns doc sections referencing that file.

### F4. Fix the trust bugs

Problem: model pins use SHA-256 of empty string; Laya reports fake 0.75.

Requirements:
1. Replace empty-string pins in `src/embed.rs:62,71` and `src/model_downloader.rs:32` with real SHA-256 of the downloaded artifacts.
2. Fix `verify_model_checksum` (`src/embed.rs:138-167`) to check the actual post-download ONNX blob path; fail closed on mismatch or missing file (no silent fallback).
3. Retire the fake `0.75` stub in `LayaOnnxProvider::judge_duplicates` (`src/judgment/laya_onnx.rs:107-118`) and `ensure_ready` (`:188`): return explicit unavailable until real `ort` session execution is wired.
4. `ModelRegistry` (`src/model_downloader.rs:16-32`) supports multi-file bundles for F5.

Acceptance: downloader rejects a tampered artifact; Laya never returns 0.75 unless computed.

### F5. Optional GLiNER extraction

Problem: no entity/relation extraction from prose.

Requirements:
1. Opt-in enrichment stage using GLiNER2.5 via the Rust ONNX path (feature-gated: `--features gliner`).
2. Chunk prose at <= 384 words (64-word overlap) to respect the 512-token ONNX ceiling.
3. Run inference in `spawn_blocking` gated by a bounded `Semaphore`.
4. Output is review-only: write candidates to `IntakeCandidate` (not `Memory`), or add a hard `is_pending_review` exclusion across all read queries.
5. Auto-materialize only when a measured precision bar is met (configurable, default 0.9).
6. Never runs in search, dedup, guard, or routing.
7. Dry-run reports proposed edges without writing.

Acceptance: on a 50-doc corpus, proposed edges have >= 0.9 precision at the configured threshold; zero false auto-materializations.

### F6. Cite edge sources

Problem: edges carry no provenance.

Requirements:
1. Add `metadata.edge_sources` to the declaring Memory row (LadybugDB edges are property-less).
2. Key by `rel_type`, `declaration_index`, `logical_target` (via `logical_path_of`, `src/parser/generation.rs:77`) so provenance survives re-ingestion.
3. Surface in `search_memory` output and `export-graph`.
4. Store content hash for staleness detection.

Acceptance: every generated edge in a test ingest has a non-null `edge_sources` entry with matching hash.

### F7. Dependency edges

Problem: no import/call traversal.

Requirements:
1. Extend tree-sitter ingest to capture imports and module dependencies.
2. Store as `CONTAINS` (structural) and new `DEPENDS_ON` relation (directed). Do not reuse `RELATES_TO`.
3. Add `DEPENDS_ON` to vocabulary and storage schema; bump internal `vocabulary_version` from 8 to 9 and `rels.len()` from 4 to 5 (`src/traits.rs:578-584`).
4. Add `CREATE REL TABLE DEPENDS_ON (FROM Memory TO Memory)` to `LadybugStore::init` (`src/store/ladybug.rs:1616`).
5. `related` traverses `DEPENDS_ON` with bounded depth.

Acceptance: a change to a module's exports surfaces dependents via `related`.

### F8. Dry-run ingest

Problem: no pre-ingest preview.

Requirements:
1. Add `dry_run: bool` and `dry_run_mode: "estimate" | "presort"` to `neurostrata_ingest_directory`.
2. Estimate mode: counts, embeddings to compute, estimated cost, secret hits. No writes, no downloads.
3. Presort mode: junk detection, duplicate-file detection, version candidates, possible PII (pattern-based: emails, phones, SSN/credit-card Luhn, IPs, plus `src/secrets.rs` rules), proposed dataset groups. Returns a report; applies nothing.
4. Label PII output `pattern_pii_warnings`.
5. All modes produce deterministic output.

Acceptance: dry-run on a 100-file tree completes without writing or downloading; report matches a subsequent real ingest's counts.

### F9. Multi-domain memories / node sets

Problem: one domain per memory; no subset filtering.

Requirements:
1. `metadata.domain` accepts `string | string[]`.
2. Add `metadata.node_set: string[]`.
3. `neurostrata_search_memory` accepts `domains` (OR) and `node_sets` (AND/OR) filters.
4. Update add schema, registry, storage, and search.
5. `domain_keys(metadata) -> Vec<String>` in `src/domain.rs`.
6. `node_set` is ungoverned (free-form project/monorepo subset tags); `domain` remains registry-governed.

Acceptance: a memory with `domain: ["auth","mtls"]` is found by either domain; node_set filter narrows results.

### F10. Conflict candidates

Problem: contradictory active memories go unnoticed.

Requirements:
1. Deterministic heuristic only: two active rules/facts governing the same `location` or `topology.{component,target_system}` with opposing `enforcement` or `decision.status`, or high cosine similarity with opposite negation/modal tokens.
2. Add `disputed: bool` and `disputed_with: string[]` to metadata.
3. Surface in recall and snapshot as a warning badge. Never auto-resolve.
4. `disputed: true` must not hide the row (unlike `is_mesh_conflicting`).

Acceptance: a synthetic contradictory pair is flagged; a near-duplicate pair is not.

### F11. Surface procedures

Problem: due procedures are not surfaced in search.

Requirements:
1. Add optional `query: Option<String>` and `task_id: Option<String>` to `neurostrata_get_snapshot` (`src/server.rs:158`).
2. Include procedures whose trigger matches the query/task.
3. Also surface matching procedures in `neurostrata_search_memory` when the query matches a procedure's trigger/content.
4. Read-only; no mutation.

Acceptance: a due procedure matching the query appears in snapshot/search output.

### F12. Watermarks

Problem: no record of enrichment progress.

Requirements:
1. Store `last_enriched_at`, `enriched_count`, and `extractor_fingerprint` per namespace in the enrichment state record (§2.4).
2. Skip work already covered by the watermark.
3. Expose in `doctor`.

Acceptance: a second enrichment run reports zero new work.

### F13. Dangling-edge report

Problem: broken links are invisible.

Requirements:
1. `doctor` and `task_validate` report unresolved `GOVERNS`/`RELATES_TO`/`DEPENDS_ON` targets.
2. Include count and sample IDs.
3. Warn-only, no mutation.

Acceptance: a store with one dangling edge reports it by ID.

### F14. Enrichment telemetry

Problem: enrichment is a black box.

Requirements:
1. Per-stage counts, failures, latency, and fingerprints in the enrichment state record.
2. Expose via `doctor` and logs.
3. Deterministic output (sorted).

Acceptance: a failed stage reports its error and count; totals reconcile.

### F15. Domain DAG + aliases

Problem: flat domain list; no hierarchy or synonyms.

Requirements:
1. Upgrade `domain_registry` to the DAG shape in §2.2 (v2), with v1 backwards compat.
2. Expand `search_with_domain` by `subClassOf` closure.
3. Expand query terms via `altLabel` to canonical slugs.
4. Validate no cycles on write.

Acceptance: `domain="auth"` returns rows tagged `mtls`; an alias query returns canonical results.

### F16. Class facets

Problem: no epistemic filtering.

Requirements:
1. Compute facets from metadata (`rule` with enforcement -> `enforced-rule`; `fact` with attempt.outcome=failed -> `failed-attempt`; `fact` with decision -> `architecture-decision`).
2. Store in `metadata.facets`.
3. Filter by facet in search.

Acceptance: filtering by `failed-attempt` returns only failed attempts.

### F17. Canonical entity slugs

Problem: aliases split recall.

Requirements:
1. Normalize `fact.topology.{component,target_system}` and `KnownIds` aliases to one slug per concept.
2. Store canonical slug alongside raw value.
3. Search expands aliases to canonical slug.

Acceptance: querying `LadybugDB` finds rows mentioning `lbug`.

### F18. Closed-world shape checks

Problem: malformed edges and rows are silent.

Requirements:
1. Validate `GOVERNS` (Rule -> Artifact), `CONTAINS` (container -> contained), `EXTRACTED_FROM` (Memory -> Task) endpoints.
2. Validate required fields per memory type.
3. Warn-first in `task_validate` and on write; never reject writes (matches current policy).

Acceptance: a malformed edge produces a warning with the row ID and reason.

### F19. Bounded transitive traversal

Problem: one-hop `GOVERNS` misses leaf symbols.

Requirements:
1. Extend `govern_out_q`/`govern_in_q` to traverse 2-3 hops over `CONTAINS`, upward from matched symbols/files to governing rules (never downward from root directories).
2. Check `is_retired_at` on all intermediate nodes.
3. Record the path in `SearchEvidence.derivation`.
4. Cap depth at 3; no unbounded traversal.

Acceptance: a rule governing `src/store` is returned for a symbol in `src/store/ladybug.rs`.

### F20. Cross-namespace concept IRIs

Problem: domain slugs are private; shared memories lose alignment.

Requirements:
1. When a shareable memory crosses SynapticMesh, stamp `metadata.concept_iri` if the local domain has one.
2. Receiver maps `concept_iri` to its local slug.
3. Add `concept_iri` to the shareable metadata allowlist in `src/synaptic_mesh/record.rs` and to the v2 domain entry.
4. Local slugs and aliases remain private.

Acceptance: a shared memory queried with the receiver's local alias returns the memory.

### F21. Ontology grounding (annotate only)

Problem: synonym drift across namespaces.

Requirements:
1. Optional JSON vocabulary file per namespace (`ONTOLOGY_FILE_PATH` env or config).
2. `annotate` mode: add canonical type to metadata.
3. No strict ingestion filtering; out-of-vocabulary items produce a `task_validate`/`doctor` warning (matches F18 warn-first policy).
4. Keep chunk text even when flagged.

Acceptance: an out-of-vocabulary memory is tagged with a warning, not dropped.

### F22. Session distillation

Problem: no durable rules from session logs.

Requirements:
1. Add a `distill` command that scans `neurostrata_append_log` output and proposes candidate rules/tasks.
2. Candidates are review-only: write to `IntakeCandidate` (or `eligibility: pending-review` with a hard read-path exclusion).
3. No separate session store; uses existing logs.

Acceptance: distill on a session log produces at least one candidate rule for a stated preference.

### F23. Feedback weighting

Problem: no explicit quality signal.

Requirements:
1. Add `feedback_score` to memory metadata.
2. Add `neurostrata_feedback` tool to record ratings.
3. Neural gain gains a small capped term for feedback.
4. Deterministic; no LLM.

Acceptance: a rated memory's salience reflects the rating within the cap.

### F24. Enrichment locking

Problem: concurrent enrichment duplicates work.

Requirements:
1. Lock per namespace during enrichment (shared enrichment state record).
2. A second call returns `lock_held` with rerun passes.
3. Follow the task-claim exclusivity pattern.

Acceptance: two concurrent enrichment calls result in one lock holder and one `lock_held` response.

## 4. Bug B1. OpenCode plugin shim build

Problem: shim does not build against `@opencode/plugin` 2.0.32.

Requirements:
1. Remove imports of `ToolCall`, `ToolResult`, `PluginContext`.
2. Rewrite entry point to `Plugin.define({ id, setup(ctx) })`.
3. Use `ctx.tool.transform(...)` to intercept `write`/`edit`/`bash`.
4. Use `ctx.permission.hook(...)` if needed.
5. Preserve all existing behavior: guard validation before writes, intent detection, degraded-state reporting.

Acceptance: `npm run build` succeeds; shim loads in OpenCode 2.0.32; guard runs before a write.

## 5. Implementation order (6 sequential plans)

1. **Plan 1 — Trust, Hygiene & Unblocking (P0):** F4, B1. Unblocks guard hooks and supply-chain safety.
2. **Plan 2 — Core Retrieval & Traversal (P0):** F1, F19, F11. Retrieval correctness.
3. **Plan 3 — Incremental & Doc Ingestion (P0):** F2, F3, F8. Ingest hygiene.
4. **Plan 4 — Graph Schema, Provenance & Diagnostics:** F7, F6, F13, F18. Graph quality.
5. **Plan 5 — Domain DAG, Facets & Mesh Alignment:** F9, F15, F16, F17, F20, F21. Domain/alias.
6. **Plan 6 — Enrichment Pipeline, Distillation & Signals:** F12, F14, F24 (unified state machine), F5, F10, F22, F23. Enrichment.

MCP tool schema additions: `exact`, `domains`, `node_sets`, `facets`, `as_of` on `search_memory`; `dry_run`, `dry_run_mode` on `ingest_directory`; `query`, `task_id` on `get_snapshot`; new `neurostrata_feedback` and `neurostrata_distill` tools.

## 6. Open questions

1. Should F1 exact-match run before or alongside vector scan? (Recommend before.)
2. Should F5 GLiNER be a separate binary or an optional feature flag? (Recommend feature flag.)
3. Should F22 distillation propose tasks or memories by default? (Recommend tasks.)
