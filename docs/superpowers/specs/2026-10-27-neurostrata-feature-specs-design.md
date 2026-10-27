# NeuroStrata Feature Specs (JSON-First)

Date: 2026-10-27. Design only. No implementation authorized.

Status: draft for user review.

## 1. Scope and constraints

- JSON only. No OWL/RDF, no reasoner, no RDF store, no pilot.
- Deterministic read path: no LLM in search, dedup, guard, or routing.
- Single store: LadybugDB. No second database in normal installs.
- Preserve bi-temporal memory, 3-tier namespaces, SynapticMesh privacy rules, and the pre-push task gate.
- All new structured data is JSON conforming to `src/schemas/memory-vocabulary.v3.json` (bump to v4 for semantic changes).
- GLiNER and other models stay write-side and review-only.
- Every feature must be falsifiable with a metric or a regression test.

Non-goals: LLM completion search types, multi-tenancy, truth subspace, COGX exchange format, frontend/UI, Python sidecar in normal installs, directed calls faked as undirected relations.

## 2. Common contracts

### 2.1 Memory row

- `memory_type`: existing types plus inferred facet tags stored in `metadata.facets: string[]` (not new top-level types).
- `metadata.domain`: `string | string[]` after feature 9.
- `metadata.node_set`: `string[]` after feature 9.
- `metadata.concept_iri`: optional string for cross-namespace alignment (feature 20).
- `metadata.facets`: `string[]` (feature 16).
- `metadata.feedback_score`: number in [-1, 1] (feature 23).
- `metadata.source_ref`: `{ file, content_hash, line_start, line_end, extractor }` on generated edges (feature 6).

### 2.2 Domain registry

Stored as the namespace's registry row with `eligibility: private`. Shape after feature 15:

```json
{
  "domain_registry": {
    "version": 2,
    "domains": [
      { "slug": "auth", "parent": null, "aliases": ["authentication"] },
      { "slug": "tls", "parent": "auth", "aliases": ["ssl"] },
      { "slug": "mtls", "parent": "tls", "aliases": ["mutual-tls"] }
    ]
  }
}
```

Invariants: slugs normalized via `normalize_domain_slug`; no cycles (DAG check); aliases unique within the namespace.

### 2.3 Search evidence

`SearchEvidence` (`src/traits.rs:66-104`) gains an optional `derivation: string[]` listing the ontology/traversal steps that produced an expanded row. Transient only; never persisted on the memory row.

### 2.4 Ingest generations

Each ingest run stamps `ingest_generation` (u64) on nodes and edges. Unchanged files reuse the prior generation's nodes and embeddings (feature 2). Superseded nodes get `valid_to` set.

## 3. Feature specifications

### F1. Search exact names

Problem: embeddings miss exact identifiers.

Requirements:
1. Add an exact-match pass before vector scan in `search_inner`.
2. Match when the query or any quoted phrase equals a `symbol` name, file path, env var pattern, or error string stored in node content/metadata.
3. Reuse existing `symbol` and `file` rows; no new index or dependency.
4. Return matches as primary results with `EvidenceKind::DirectMatch` and `derivation: ["exact-name"]`.
5. Add `exact: bool` parameter to `neurostrata_search_memory` (default true).

Acceptance: Recall@5 on a 30-query identifier benchmark improves by at least 20 points over baseline vector-only; zero regressions on existing direct-match queries.

### F2. Re-ingest only what changed

Problem: full re-ingest re-embeds and rebuilds unchanged nodes.

Requirements:
1. Store `content_hash` (SHA-256 of file bytes) per file node.
2. On ingest, skip files whose hash matches the stored hash and whose parser/schema version matches.
3. For changed files: supersede old symbol nodes (set `valid_to`), add new nodes in the new generation.
4. Re-parse when parser version or vocabulary version changes even if the file hash matches.
5. Report counts: files skipped, changed, added, removed, embeddings reused.

Acceptance: re-ingesting an unchanged tree produces zero new nodes and zero new embeddings; a one-line change touches only that file's symbols.

### F3. Make docs real

Problem: markdown files stored as empty `Path: ...` rows.

Requirements:
1. Parse markdown into sections by heading.
2. Extract backticked symbols, file paths, and links.
3. Create `markdown` nodes with real content and `CONTAINS` edges to extracted symbols/paths.
4. Connect to the graph via existing `CONTAINS`/`RELATES_TO`.
5. Embed the section text (not an empty path string).

Acceptance: a search for a backticked symbol in a doc returns the doc section; `memories_for_file` returns doc sections referencing that file.

### F4. Fix the trust bugs

Problem: model pins use SHA-256 of empty string; Laya reports fake 0.75.

Requirements:
1. Replace empty-string pins in `src/embed.rs:62,71` and `src/model_downloader.rs:32` with real SHA-256 of the downloaded artifacts.
2. Either repair Laya's duplicate confidence (`src/judgment/laya_onnx.rs:97-120`) to return a real score or remove the Laya path and document retirement.
3. Add a verification step in the downloader that fails on mismatch (no silent fallback).

Acceptance: downloader rejects a tampered artifact; Laya never returns 0.75 unless computed.

### F5. Optional GLiNER extraction

Problem: no entity/relation extraction from prose.

Requirements:
1. Add an opt-in enrichment stage using GLiNER2.5 (via the Rust ONNX path) that proposes entities and relations.
2. Output is review-only: writes candidate edges with `eligibility: pending-review`, never auto-materializes.
3. Auto-materialize only when a measured precision bar is met (configurable, default 0.9).
4. Never runs in search, dedup, guard, or routing.
5. Dry-run reports proposed edges without writing.

Acceptance: on a 50-doc corpus, proposed edges have >= 0.9 precision at the configured threshold; zero false auto-materializations.

### F6. Cite edge sources

Problem: edges carry no provenance.

Requirements:
1. Add `source_ref` to every generated edge (feature 2/5/7 outputs).
2. Surface `source_ref` in `search_memory` output and `export-graph`.
3. Store content hash so staleness is detectable.

Acceptance: every generated edge in a test ingest has a non-null `source_ref` with matching hash.

### F7. Dependency edges

Problem: no import/call traversal.

Requirements:
1. Extend tree-sitter ingest to capture imports and module dependencies.
2. Store as `CONTAINS` (structural) and a new `DEPENDS_ON` relation (directed). Do not reuse `RELATES_TO`.
3. Add `DEPENDS_ON` to the vocabulary and storage schema.
4. `related` traverses `DEPENDS_ON` with bounded depth.

Acceptance: a change to a module's exports surfaces dependents via `related`.

### F8. Dry-run ingest

Problem: no pre-ingest preview.

Requirements:
1. Add `dry_run: bool` and `dry_run_mode: "estimate" | "presort"` to `neurostrata_ingest_directory`.
2. Estimate mode: counts, embeddings to compute, estimated cost, secret hits. No writes, no downloads.
3. Presort mode: junk detection, duplicate-file detection, version candidates, possible PII, proposed dataset groups. Returns a report; applies nothing.
4. All modes produce deterministic output.

Acceptance: dry-run on a 100-file tree completes without writing or downloading; report matches a subsequent real ingest's counts.

### F9. Multi-domain memories / node sets

Problem: one domain per memory; no subset filtering.

Requirements:
1. `metadata.domain` accepts `string | string[]`.
2. Add `metadata.node_set: string[]`.
3. `neurostrata_search_memory` accepts `domains` (OR) and `node_sets` (AND/OR) filters.
4. Update add schema, registry, storage, and search.

Acceptance: a memory with `domain: ["auth","mtls"]` is found by either domain; node_set filter narrows results.

### F10. Conflict candidates

Problem: contradictory active memories go unnoticed.

Requirements:
1. Add a detection pass (deterministic heuristic or offline LLM) that marks pairs of active memories sharing an entity with contradictory predicates as `disputed`.
2. Add `disputed: bool` and `disputed_with: string[]` to metadata.
3. Surface in recall and snapshot. Never auto-resolve.

Acceptance: a synthetic contradictory pair is flagged; a near-duplicate pair is not.

### F11. Surface procedures

Problem: due procedures are not surfaced in search.

Requirements:
1. In `get_snapshot`, include procedures whose trigger matches the current query/task.
2. Match by trigger string and description similarity.
3. Read-only; no mutation.

Acceptance: a due procedure matching the query appears in snapshot output.

### F12. Watermarks

Problem: no record of enrichment progress.

Requirements:
1. Store `last_enriched_at`, `enriched_count`, and `extractor_fingerprint` per namespace.
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
1. Per-stage counts, failures, latency, and fingerprints.
2. Expose via `doctor` and logs.
3. Deterministic output (sorted).

Acceptance: a failed stage reports its error and count; totals reconcile.

### F15. Domain DAG + aliases

Problem: flat domain list; no hierarchy or synonyms.

Requirements:
1. Upgrade `domain_registry` to the DAG shape in §2.2.
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
1. Extend `govern_out_q`/`govern_in_q` to traverse 2-3 hops over `CONTAINS`.
2. Record the path in `SearchEvidence.derivation`.
3. Cap depth at 3; no unbounded traversal.

Acceptance: a rule governing `src/store` is returned for a symbol in `src/store/ladybug.rs`.

### F20. Cross-namespace concept IRIs

Problem: domain slugs are private; shared memories lose alignment.

Requirements:
1. When a shareable memory crosses SynapticMesh, stamp `metadata.concept_iri` if the local domain has one.
2. Receiver maps `concept_iri` to its local slug.
3. Local slugs and aliases remain private.

Acceptance: a shared memory queried with the receiver's local alias returns the memory.

### F21. Ontology grounding (annotate/strict)

Problem: synonym drift across namespaces.

Requirements:
1. Optional JSON vocabulary file per namespace (`ONTOLOGY_FILE_PATH` env or config).
2. `annotate` mode: add canonical type to metadata.
3. `strict` mode: filter memories at ingest whose type has no vocabulary entry.
4. Keep chunk text even when pruned.

Acceptance: strict mode drops an out-of-vocabulary memory; annotate mode tags it.

### F22. Session distillation

Problem: no durable rules from session logs.

Requirements:
1. Add a `distill` command that scans `neurostrata_append_log` output and proposes candidate rules/tasks.
2. Candidates are review-only (task rows or memories with `eligibility: pending-review`).
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
1. Lock per namespace during enrichment.
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

## 5. Implementation order (suggested, not mandatory)

1. F4 (trust bugs) — prerequisite for all model work.
2. F1, F19 — retrieval correctness.
3. F2, F3, F8 — ingest hygiene.
4. F9, F15, F17 — domain/alias.
5. F6, F7, F13 — graph quality.
6. F18, F16, F10, F11 — validation and surfacing.
7. F12, F14, F24 — observability.
8. F5, F21 — extraction and grounding.
9. F20, F22, F23 — cross-cutting.
10. B1 — plugin build.

## 6. Open questions

1. Should F1 exact-match run before or alongside vector scan? (Recommend before.)
2. Should F5 GLiNER be a separate binary or an optional feature flag? (Recommend feature flag.)
3. Should F22 distillation propose tasks or memories by default? (Recommend tasks.)
