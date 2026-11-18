# 2026-11-18 — `v1.19.0` Path Resolution, Namespace-Scoped Retrieval, and CodeQL Logging Remediation Design

> **Task**: `neurostrata-uj78` (design), parent `neurostrata-mb5w`
> **Branch**: `fix/v1-19-0-path-security-remediation`
> **Base**: `main` @ `6f805d15fd00ba59c6e6214cc961e6a2beaa9544` (post-PR #265)
> **PR shape**: one coordinated remediation PR (no stacked/per-issue PRs)
> **Closes**: GitHub issues `#266`, `#267`, `#268`, `#269`, `#270`, `#271`, `#272`, `#273`, `#274`
> **Addresses**: open CodeQL code-scanning alerts `#8`, `#9`, `#10`, `#11` on `main`
> **Explicit exclusions**: `#68`, `#264` and all unrelated refactors

---

## 1. Executive Summary

This design specifies one implementation-ready `v1.19.0` remediation PR that hardens NeuroStrata's path canonicalization, namespace-root resolution, cross-spelling graph retrieval, prefix filtering, and test-only CodeQL logging surfaces without changing unrelated product behavior.

The PR unifies path and prefix resolution around shared, host-aware primitives so absolute/relative spellings, registry-cached roots, and Windows/POSIX path forms are interpreted consistently across `memories_for_file`, `related`, `search_memory`/`locate`/`insights` prefix filters, `diff_tour`, and `context_plan`. It replaces AST winner-take-all and truncation-loss behavior with deterministic unions and explicit truncation/error signaling, preserves existing memory merge/rank/dedup/cap semantics while eliminating redundant per-form memory retrieval and per-form conceptual-bridge enrichment, and makes cached-root reads fail closed. It also removes CodeQL `rust/cleartext-logging` findings in test assertion diagnostics with static, non-interpolating messages while preserving assertions and synthetic fixtures.

All changes preserve namespace isolation, nested/foreign repository vetoes, existing output compatibility where not explicitly extended, honest semantic `FAIL` reporting, and all prohibited-claim boundaries. No `TODO`, placeholder, stub, deferred phase, or out-of-scope refactor is permitted in the implementation PR.

---

## 2. Scope, Non-Goals, and Invariants

### 2.1 In Scope (one PR)

| Track | Issues / alerts | Outcome |
|---|---|---|
| Shared path/root resolution | `#266`, `#269`, `#274` | Host-aware absolute parsing, registry-root alternate retention, and fail-closed cached-root reads feeding all lookup/prefix flows |
| File-seed and diff resolution | `#267`, `#268` | `neurostrata_related` resolves absolute file seeds to one canonical in-namespace node id; `diff_tour` separates user-visible paths from internal resolved lookup forms and records canonical resolved paths in the session ledger |
| Prefix parity | `#273` | `search_memory`, `locate`, and `insights` (`lexical`/`ast_calls`) use one shared prefix-forms helper with absolute/relative stored-spelling parity and explicit errors for truly unresolvable absolute prefixes; optional `project_root` added to `neurostrata_search_memory` and `neurostrata_insights` schemas/handlers/evaluation context (locate uses existing arg) |
| Cross-spelling retrieval correctness | `#271`, `#272` | `memories_for_file`/AST neighborhood query/union all equivalent forms with deterministic unions and preserved truncation/error semantics; memory merge preserves existing global rank/id/cap semantics while avoiding per-form early cutoffs that can lose globally better hits |
| Retrieval performance hygiene | `#270` | `context_plan` hoists registry reads once per request, dedupes request work, and reuses a concrete per-logical-file multi-form retrieval path with conceptual bridge enrichment exactly once per logical file’s final merged/ranked/deduped/capped hit set; objective before/after evidence required |
| Test-only CodeQL remediation | `#8`, `#9`, `#10`, `#11` | Static non-interpolating assertion diagnostics in `src/rdf/reasoning.rs` and `src/semantic_eval/competency.rs`; PR verification proves CodeQL analysis no longer reports the fixed lines, and merge auto-closes alerts |

### 2.2 Explicit Non-Goals / Exclusions

- Issues `#68` and `#264` are excluded.
- No stacked PRs and no per-issue PR split.
- No unrelated script, hook, pre-commit, or `libformal_kernel_check.rlib` changes.
- Four existing git stashes remain untouched.
- No suppression/dismissal-as-substitute for CodeQL alerts and no production behavior change for CodeQL fixes.
- No migration of persisted insight prefixes, stored memory locations, or stored node paths.
- No change to SPARQL prefix semantics/query engine behavior.
- No change to memory ranking policy, memory dedup identity (`id`), `MAX_LIMIT`, context-plan `project_root`/`source` behavior beyond registry-read hoisting, or prohibited-claim/semantic-`FAIL` boundaries.

### 2.3 Preserved Invariants (must remain true)

- **Namespace isolation and root safety**: nested/foreign repository vetoes, `Outside` handling, and root-priority constraints remain intact; no fallback widening on store/metadata errors.
- **Output compatibility**: existing successful response fields remain present and semantically compatible; extensions are additive and only where specified (e.g. explicit errors/truncation metadata, preserved original seed/path display fields, additive resolved seed fields).
- **Honest retrieval semantics**: "no match" is distinct from "unresolvable/invalid scope" and from "truncated". Successful-empty results are never fabricated into errors, and errors never masquerade as empty scope. For multi-form retrieval, any successful form (including successful-empty) yields overall success; the first error is surfaced only if no form succeeds.
- **Release honesty**: implementation PR docs record actual fixes/evidence only; no semantic `PASS`, ROI, blind-agent, rollback, curation-cost, or real-namespace claims.

### 2.4 Current Evidence (verified)

- `src/paths.rs`: `parse_absolute`, `is_absolute_any`, `LookupPath` (`path`, `source`, `root`, `alternates`), `to_project_relative`/`choose_root`, `lookup_forms`, and tests currently treat POSIX `//...` as Windows UNC and construct alternates without the approved broader-root/registry-root retention rule.
- `src/registry.rs`: `cached_project_root` currently returns `Option<String>` and collapses read failures/misses to `None` (`Ok(Some(...)) -> Some`, otherwise `_ => None`), enabling scope-widening fallback behavior.
- `src/server.rs`: `lookup_path_for` performs one registry read per call and assumes `Option<String>`; `memories_for_forms` queries each equivalent form separately via `store.memories_for_file(...)` and passes the same `limit` per form, then `merge_spelling_results` globally concatenates successful hits, applies global `rank_hits` when multiple forms, dedupes by memory `id` (keeping best-ranked after ranking), and caps to `min(limit, MAX_LIMIT)` while preserving any-success-including-empty and first-error-only-if-no-success semantics. Because each per-form store query is independently capped and bridge enrichment is performed in the store per-form retrieval, globally better hits can be lost by per-form cutoffs and bridge work can be duplicated/misaligned relative to the final per-logical-file hit set. `ast_neighborhood_for_forms` returns the first non-empty summary or first answer (winner-take-all) and drops truncation signals. `handle_memories_for_file` and `handle_context_plan` use per-file/per-form lookups; `handle_related` resolves seed by traversal input as given (no `project_root`/shared seed canonicalization path); `handle_diff_tour` and `handle_insights` are thin wrappers over `graph_read::diff_tour::execute_diff_tour` and `telemetry::insights::execute_insights`.
- `src/graph_read/diff_tour.rs`: parses/normalizes diff paths for processing and emits user-visible stops/rules/files; session-ledger/governance path handling must be made explicit so ingestion/governance use resolved forms while supplied display paths are preserved.
- `src/telemetry/insights.rs`, `src/server.rs`, and `src/store/ladybug.rs`: prefix filtering is tool-specific today and can silently empty on absolute prefixes / mismatched stored spellings; `neurostrata_search_memory` and `neurostrata_insights` tool schemas currently lack optional `project_root` while `locate` already accepts it.
- CodeQL open alerts on `main@6f805d15`: `src/rdf/reasoning.rs:2687,2700` and `src/semantic_eval/competency.rs:749,750` interpolate computed values into assertion diagnostics in tests (`rust/cleartext-logging`).
- Inventories: open issues `#266-#274` (+ excluded `#68,#264`) in `/tmp/opencode/open-issues-2026-11-18.json` and `/tmp/opencode/issue-triage-current.json`; open alerts in `/tmp/opencode/code-scanning-open-2026-11-18.json`.

---

## 3. Requirements

### 3.1 Functional Requirements

1. **R1 (Shared path/prefix resolution)**: All in-scope path and prefix consumers resolve through shared primitives with explicit root precedence `project_root -> namespace registry root -> .git inference`, preserving namespace isolation, nested/foreign repo vetoes, and output compatibility.
2. **R2 (Alternate retention, #266)**: When a broader valid `project_root` wins and a deeper valid registry root also contains the file, include the registry-relative spelling in `LookupPath.alternates` (only for valid, non-vetoed roots), normalized/deduped at construction using the same normalized key as `lookup_forms`, stable first-seen order, with no root-priority change.
3. **R3 (Host-native absolute parsing, #269)**: `parse_absolute`/absolute detection treat `//...` as POSIX absolute on Unix and UNC on Windows; verbatim Windows prefixes and existing conditional tests are preserved. Linux daemon behavior is POSIX for `//server/share` (documented and tested).
4. **R4 (Fail-closed cached root reads, #274)**: `cached_project_root` returns `Result<Option<String>>` (crate `Result`/equivalent). Missing or invalid cached metadata => `Ok(None)`; store/read errors => `Err(...)` propagated through lookup handlers; no fallback widening.
5. **R5 (Related seed resolution, #267)**: `neurostrata_related` accepts optional `project_root` and resolves a file-path seed through shared lookup forms to exactly one canonical in-namespace node id before a single traversal call; original seed is preserved in the response; no ambiguous multi-traversal or cursor semantic changes.
6. **R6 (Diff path duality, #268)**: `diff_tour` uses dual display path vs internal lookup forms for `changed_spans` and `diff_text`-derived spans. Ingestion/governance lookups use resolved forms; user-visible outputs preserve supplied paths; session ledger records the resolved canonical stored path. Relative behavior unchanged.
7. **R7 (Prefix parity, #273)**: `search_memory`, `locate`, and `insights` (`lexical`, `ast_calls`) use one shared prefix-forms helper supporting relative and absolute stored spellings and dynamic evaluation of persisted insight prefixes (no migration). Optional `project_root` is added to both `neurostrata_search_memory` and `neurostrata_insights` schemas/handlers/evaluation context; `locate` uses its existing `project_root` argument for the same resolution path. Empty prefix remains no filter. SPARQL unaffected. Truly unresolvable absolute path/prefix returns an explicit actionable error, never silent empty or broadened scope.
8. **R8 (Cross-form unions and memory correctness, #271/#272)**: Equivalent-form queries query/union all equivalent forms for AST neighborhoods and `related_memories` with deterministic unions, preserved truncation/error semantics, and recomputed `blast_radius_count`. For memories, a concrete per-logical-file multi-form retrieval path preserves existing global rank/id/cap semantics and gathers uncapped-enough raw candidates under existing safety budgets before global rank/dedup/cap so no per-form early cutoff can lose a globally better hit; if safety budgets cut candidates, truncation is signaled honestly. Conceptual bridge enrichment occurs exactly once on the final per-logical-file merged/ranked/deduped/capped hit set. Dedupe: memory `id`; AST `symbols/callers/callees/importers` by `id`, `imports` by `path`, `related_memories` by `(id, relation, direction, supersedes)`. Stable form/order first-seen. Truncation flags OR across forms with canonical `truncated_collections` order `symbols,callers,callees,imports,importers` (for collections present). A successful-empty result plus another-form error remains success; the first error is surfaced only if no form succeeds.
9. **R9 (Performance hygiene, #270)**: `context_plan` hoists registry root read to once per request; performs construction-time dedupe of requested file work; reuses the concrete per-logical-file multi-form retrieval path (ordered equivalent forms in one candidate-discovery operation or one store-facing multi-form API) so duplicate equivalent-form store queries for identical lookup inputs are eliminated. Conceptual bridge enrichment occurs exactly once per logical file’s final merged/ranked/deduped/capped hit set (never once globally across the whole request). No short-circuit may violate AST union/truncation semantics or per-logical-file memory correctness. Objective before/after evidence is produced per §7.
10. **R10 (CodeQL, #8-#11)**: Replace interpolating assertion diagnostics in `src/rdf/reasoning.rs` and `src/semantic_eval/competency.rs` with static non-interpolating messages; assertions and synthetic fixtures unchanged; no suppression/dismissal; no production behavior change. PR verification proves CodeQL analysis no longer reports the fixed lines, and merge auto-closes alerts.

### 3.2 Non-Functional Requirements

- **Determinism**: all merges, dedupe, truncation, and alternate/form ordering are deterministic (stable first-seen in canonical form order unless a specified global ranking step applies).
- **Fail-closed safety**: store/metadata/resolve failures do not widen namespace scope or return misleading empty-success results.
- **Compatibility**: existing callers and output consumers continue to function without migration for unchanged fields/semantics.
- **Testability**: every issue/alert has focused red-green regression coverage and explicit acceptance criteria.
- **Honesty**: docs/perf evidence/changelog report only measured, attributable outcomes.

---

## 4. Architecture and Component Design

### 4.1 Shared Path and Prefix Resolution Core (`src/paths.rs` + `src/registry.rs`)

#### 4.1.1 Host-native absolute parsing (`#269`, R3)
- `parse_absolute(raw: &str)` becomes host-aware:
  - **Unix (`cfg(unix)`)**: a leading `//` (and any number of leading slashes normalized per POSIX lexical handling) is parsed as a POSIX absolute path rooted at `/` unless the string is a Windows verbatim form explicitly preserved for cross-host recognition; for ordinary Unix host parsing, `//server/share/...` resolves as POSIX absolute (`root="/"`, parts include `server`, `share`, ...), not UNC.
  - **Windows (`cfg(windows)`)**: `//server/share`, `\\server\share`, `\\?\UNC\...`, drive-letter, and verbatim/device prefixes parse as Windows absolute/UNC as today.
  - **Cross-host recognition**: `is_absolute_any` continues to recognize absolute forms of either platform on any host so a daemon on Linux can still classify/handle a Windows path supplied by a client; however, when such a path is interpreted against a host root (`relative_to`, host-root containment), interpretation follows host-native semantics. A Linux host must not treat `//server/share` as matching a POSIX root via UNC semantics.
- Preserve verbatim prefix stripping behavior where already supported, separator normalization (`\`/`/`), lexical `.`/`..` resolution, and conditional/platform tests. Add/adjust tests to pin: on Unix, `//server/share/x` is POSIX absolute and only matches POSIX roots lexically; on Windows, UNC behavior remains; verbatim drive/UNC spellings continue to work.

#### 4.1.2 Root precedence and alternate retention (`#266`, R1/R2)
- `to_project_relative` / `choose_root` retain root precedence **`project_root` (caller) -> `registry_root` (namespace cache, usable-filtered) -> `.git` inference**, with all existing nested/foreign repository veto rules unchanged.
- Add a deterministic "competing valid roots" pass for absolute inputs when:
  1. a broader valid `project_root` is the winning resolution source, and
  2. a deeper valid (usable, non-vetoed for this file) namespace registry root also contains the same file.
  In this case, the primary `LookupPath.path/source/root` remain those of the winning `project_root` resolution (no root-priority change), but `LookupPath.alternates` additionally retains the registry-relative spelling from the deeper valid registry root.
- `LookupPath.alternates` construction rules:
  - include only spellings derived from valid, non-vetoed roots for the target path;
  - normalize every candidate with `crate::parser::ingest::normalize_node_path` for dedupe comparison (same normalized key concept used by `lookup_forms`);
  - dedupe at construction time, stable first-seen order;
  - preserve existing alternate kinds already emitted today (input/caller spelling, absolute spelling, disk/caller-relative forms as applicable) and append the retained registry-relative spelling when applicable without duplicates.
- `lookup_forms(raw, lookup, known_root)` remains the canonical form enumerator and must continue to dedupe by normalized path key in stable first-seen order; its output is the single source for equivalent-form store lookups.

#### 4.1.3 Shared prefix-forms helper (`#273`, R7)
- Introduce a shared helper in `src/paths.rs` (name: `prefix_lookup_forms` or equivalent crate-visible helper; exact symbol finalized in implementation but specified here as the sole helper) that returns ordered, deduped prefix candidates for a supplied `path_prefix` given `(project_root, registry_root, namespace context already resolved by caller)`.
- Behavior:
  - **Empty/absent prefix**: treated as "no filter" by callers (helper may return an explicit sentinel/empty-filter marker; callers must preserve current semantics).
  - **Relative prefix**: candidates include the normalized relative prefix and any valid joined/known-root-compatible spellings needed to match absolute-stored locations (without escaping/violating root containment).
  - **Absolute prefix**: resolve through the same root precedence and veto rules as file paths. If the prefix is inside a valid known root, emit canonical relative and absolute/alternate spellings needed to match stored relative/absolute variants. If the prefix is absolute on a supported host form but cannot be resolved to any valid in-namespace known root/scope (truly unresolvable), return an explicit actionable resolution error.
  - **Stored-spelling parity**: helper output must enable matching when data is stored as relative or absolute spellings; callers must not rely on a single raw string comparison for resolved absolute prefixes.
  - **Dynamic evaluation**: persisted insight prefixes are evaluated through this helper at read/evaluation time (no migration/backfill).
- SPARQL insights/queries do not use this helper and remain unchanged.

#### 4.1.4 Canonical in-namespace node-id resolution helper (`#267`, R5)
- Add a shared helper (server/paths-layer orchestration as appropriate) to resolve a file-path seed into lookup forms and then to exactly one canonical in-namespace node/file identifier before graph traversal:
  - enumerate forms via `lookup_path_for`/`lookup_forms`;
  - probe the store for the seed as a node/file target across equivalent forms in deterministic form order;
  - select the canonical id by deterministic first-seen match in form order, with stable tie-breakers (`id` lexical order only if multiple distinct ids are returned for the same form by the same probe, which must be treated as ambiguous only within that form and resolved deterministically without multi-traversal side effects);
  - if no form resolves to any in-namespace node/file id, return "not found/unresolvable in namespace" (not a scope-widening fallback).
- The helper returns both the preserved original seed string and the resolved canonical id for response/traversal use.

### 4.2 Cached Project Root: Fail-Closed Reads (`src/registry.rs`, `#274`, R4)

- Change signature:
  - `pub async fn cached_project_root(store: &Arc<dyn VectorStore>, namespace: &str) -> anyhow::Result<Option<String>>` (or crate-equivalent `Result` type used consistently by callers).
- Semantics:
  - `Ok(Some(root))`: registry row exists, is a registry row, and contains a non-empty `project_root` string.
  - `Ok(None)`: registry row absent, not a registry row, metadata missing/invalid shape for `project_root`, or empty string (treated as absent/invalid metadata).
  - `Err(e)`: store read/get failure or other unexpected retrieval error; error is contextualized with namespace and propagated.
- No warning-to-`None` collapse for store errors. Callers must propagate errors and fail closed (no `.git`/broader fallback triggered because a cache read failed).
- `cache_project_root` behavior remains best-effort write-side caching (no new failure contract required beyond existing logging) and does not weaken read-side correctness.

### 4.3 Server/Store Lookup, Multi-Form Retrieval, and Truncation Semantics (`src/server.rs`, `src/store/ladybug.rs`, `#266/#270/#271/#272/#274`, R1/R2/R4/R8/R9)

#### 4.3.1 `lookup_path_for` and request-scoped registry hoisting
- `lookup_path_for` becomes fallible-aware with respect to cached-root reads and returns a result carrying `(LookupPath, Vec<String>)` or propagates the registry/store error to the caller; it must not synthesize a widened scope on error.
- `handle_memories_for_file`, `handle_related` (new file-seed path), `handle_context_plan`, prefix-filter resolution paths, and any other in-scope callers must:
  - read the namespace registry root at most once per request context where multiple lookups/forms are needed (for `context_plan`, exactly once per request; see §4.6);
  - pass the already-resolved registry root into path/prefix resolution helpers to avoid duplicate registry reads.

#### 4.3.2 Concrete per-logical-file multi-form memory retrieval (preserve existing merge semantics)
- Introduce a concrete internal multi-form LadybugStore retrieval path for one logical file (new store-facing multi-form API or an equivalent single store operation; exact symbol name finalized in implementation, e.g. `memories_for_file_forms`) that accepts the ordered equivalent `forms` for that logical file plus existing query parameters (`namespace`, `source`, `symbol`, `include_ancestors`, `memory_types`, `exclude_ids`, requested `limit`).
- **Candidate discovery**: perform candidate discovery over the ordered equivalent forms in one candidate-discovery operation or one store-facing multi-form API. The implementation must gather uncapped-enough raw candidates under existing safety budgets before global rank/dedup/cap so that no per-form early cutoff can lose a globally better hit. Existing per-form/per-query `limit` must not be applied independently in a way that discards candidates still needed for correct global selection.
- **Bounded safety/truncation**: if an existing safety budget (query/result/byte/time budget already enforced by the store path) requires cutting candidate collection, collection proceeds in deterministic form order and the operation returns honest truncation signaling for the memory candidate set (additive/explicit flag/metadata as needed, compatible with existing response conventions) rather than silent loss.
- **Global merge/rank/dedup/cap**: merge raw candidates under the existing semantics currently provided by `merge_spelling_results`/`rank_hits`: concatenate successful candidates, apply global `rank_hits` once, dedupe by memory `id` keeping the best-ranked retained instance (stable on ties by prior first-seen order after ranking input ordering), then cap to `min(limit, MAX_LIMIT)`.
- **Error/success rule (preserved)**: any successful form/result contribution (including successful-empty) yields overall success; errors from other forms are logged/retained diagnostically as appropriate. The first encountered error (deterministic form order) is returned only if no form succeeds.
- **Conceptual bridge enrichment (once per logical file)**: conceptual bridge enrichment is computed exactly once on the final per-logical-file merged/ranked/deduped/capped hit set, not per form and not once globally across a multi-file request. Store/server responsibilities may be refactored to achieve this (e.g. raw multi-form retrieval without per-form bridges + one enrichment pass over final hits), but externally visible memory hit fields/meaning remain compatible.
- **Server integration**: `handle_memories_for_file` and per-file retrieval in `handle_context_plan` use this concrete per-logical-file path. `merge_spelling_results` may be retained/adapted only if it preserves these semantics; redundant per-form `store.memories_for_file` fan-out and per-form enrichment are eliminated for the multi-form case.

#### 4.3.3 AST neighborhood union and truncation preservation (`ast_neighborhood_for_forms`, `#271/#272`)
- Replace winner-take-all with deterministic union across all equivalent forms (AST requirement is not weakened):
  - Query `store.ast_neighborhood_for_file` for every form in deterministic order (unless a hard fatal transport/store condition makes further safe querying impossible; normal per-form query errors follow the same "first error only if no form succeeds" rule at the aggregate level).
  - For each successful per-form `(related_memories, ast_summary)`:
    - union `ast_summary.symbols`, `callers`, `callees`, `importers` with dedupe by `id`;
    - union `ast_summary.imports` with dedupe by `path`;
    - union `related_memories` with dedupe by `(id, relation, direction, supersedes)` preserving edge semantics and stable first-seen order after deterministic merge ordering;
    - OR truncation booleans/flags across forms.
  - `blast_radius_count` is recomputed from the final unioned graph/summary (not copied from any single form).
  - `truncated_collections` is the deterministic union of collection names reported truncated in any contributing form, emitted in canonical order: `["symbols", "callers", "callees", "imports", "importers"]` filtered to those present/truncated.
  - `ast_summary_is_empty` is used only as an "empty data" predicate, never as proof of non-truncation. A summary with empty arrays but truncation flags set (or any preserved truncation metadata indicating cutoff) is treated as truncated evidence and must not be discarded in favor of a later form's populated summary except by unioning/ORing metadata.
  - **Collection-level caps/byte budgets**: existing per-collection caps remain authoritative. When unioning would exceed a cap or byte budget, apply deterministic truncation in form order and mark the affected collection(s) truncated; preserve already-included deterministic prefix.
- Aggregate result:
  - Success if any form yields a successful AST result (even if empty but valid/truncated metadata is returned by store contract); union all successful contributions.
  - If no form succeeds, return the first form-order error.
  - `related_memories_truncated` (response-level flag in `handle_memories_for_file`) is ORed across union inputs/final merge as applicable.

#### 4.3.4 `handle_memories_for_file` response semantics
- Continues to return `memories`, `unsupported`, `path` (original request), and existing optional fields (`resolved_path`, `resolved_from`, `related_memories`, `related_memories_truncated`, `ast_summary`, `ast_error`) with compatible meanings.
- On cached-root/store resolution failures required to compute valid lookup scope, handler returns an explicit actionable error payload/message (fail closed), not silent empty and not broadened scope.
- `exclude_ids` filtering remains applied to returned `memories` and filtered `related_memories` as today, after union/dedupe and final per-file enrichment/selection.

### 4.4 `neurostrata_related` File-Seed Canonicalization (`src/server.rs` and store call boundary, `#267`, R5)

- Input/schema behavior:
  - Add optional `project_root` parameter to `neurostrata_related` (server tool schema/handler argument parsing). Existing callers without it remain valid.
  - If `id` is a file-path seed (absolute path of either supported platform, or otherwise identified as path-like by the shared path resolver; implementation must use a single deterministic predicate consistent with `is_absolute_any`/path handling and must not misclassify memory IDs), resolve it via shared lookup path/forms and canonical in-namespace node-id resolution (§4.1.4) before traversal.
  - If `id` is not a path seed, behavior is unchanged.
- Traversal:
  - Call `store.related(...)` exactly once using the resolved canonical in-namespace node id.
  - No multi-traversal fallback across alternate forms/ids and no cursor semantic changes.
  - Preserve namespace isolation and existing relation/direction/hop/type/limit/cursor/temporal/prioritization/cross-namespace behavior.
- Response:
  - Preserve original request seed in `seed`.
  - Add additive field `resolved_seed` (canonical node id) when resolution/canonicalization occurs (or when different from input) so callers can observe the resolved target without losing the original; existing consumers ignoring unknown fields remain compatible.
  - If path-seed resolution is truly unresolvable in namespace, return explicit actionable not-found/unresolvable error (not empty traversal result, not broadened scope).

### 4.5 `diff_tour` Dual Display vs Internal Lookup Forms (`src/graph_read/diff_tour.rs`, server/ledger integration, `#268`, R6)

- **Input normalization and resolution**:
  - For each changed span/path from `diff_text` parsing and explicit `changed_spans`, retain the supplied path string as `display_path`.
  - Compute `internal_lookup_forms` by resolving the span path through the shared path/prefix/file resolution pipeline using request `project_root` and namespace registry root precedence (with hoisted/cached registry read in request context where applicable).
  - Ingestion existence checks, governance/memory lookups, AST/symbol resolution, and any internal namespace-scoped matching use `internal_lookup_forms` (and/or resolved canonical stored path derived from them).
- **Outputs**:
  - All user-visible path fields in `DiffTourResponse` (`stops[].items[].path`, `uningested_files`, and any other path-bearing user-facing fields) preserve the supplied/display path as received/normalized for presentation today (no forced absolute/relative rewrite), except existing normalization required for display consistency must remain unchanged for relative inputs.
  - Internal diagnostics that require canonical identity may include additive canonical fields only if needed for correctness; not required by this issue. Default: preserve existing schema/field names and values' user-facing meaning.
- **Session ledger**:
  - Session ledger entries recorded for touched/resolved diff-tour files store the resolved canonical stored path (the canonical in-namespace/stored spelling selected deterministically from resolved forms), not the raw supplied spelling, enabling consistent later correlation. This applies to ledger recording paths only; user-visible response remains the supplied/display path.
- **Relative behavior unchanged**: relative span paths continue to resolve as today when resolvable; no behavior regression for existing relative-only callers.

### 4.6 `context_plan` Registry Hoisting and Per-Logical-File Retrieval Reuse (`src/server.rs`, `#270`, R9)

- **Registry read hoisting**: resolve namespace registry root once per `handle_context_plan` request and reuse it for all per-file `lookup_path_for`/form computations and any prefix/path resolution performed during the request. `context_plan` must not re-read the registry row per requested file.
- **Construction-time dedupe**: dedupe requested files/lookup work by normalized path key and preserve deterministic first-seen request order for processing/output association where order is currently meaningful.
- **Concrete per-logical-file retrieval reuse**:
  - For each deduped logical file, `context_plan` reuses the same concrete per-logical-file multi-form memory retrieval path defined in §4.3.2 (ordered equivalent forms in one candidate-discovery operation or one store-facing multi-form API), followed by global raw-hit merge under existing rank/id/cap semantics and conceptual bridge enrichment exactly once on that logical file’s final merged/ranked/deduped/capped hit set.
  - AST neighborhood retrieval for a logical file, when required by the context-plan path, continues to query/union all equivalent forms per §4.3.3 (no AST weakening/short-circuit).
  - Within the request, identical effective lookup inputs must not trigger duplicate equivalent-form store queries; request-level memoization/caching may be used only when keys include all semantics-affecting inputs (namespace, source/generation context, logical file/forms identity, symbol/filter parameters, and safety-budget-relevant limits) to avoid changing results.
  - No short-circuit optimization may skip a form/query in a way that violates AST union/truncation completeness, per-logical-file memory correctness, or successful-empty/error rules.
- **Conceptual bridge enrichment scope (explicit)**: enrichment occurs exactly once per logical file’s final merged/ranked/deduped/capped hit set. It must not run per form and must not be collapsed into one global enrichment pass across the entire multi-file `context_plan` request, because request-level global selection would change per-file semantics.
- **Preserved semantics**:
  - No change to `context_plan` `project_root`/`source` behavior beyond registry-read hoisting and equivalent-form correctness-preserving reuse.
  - Preserve memory ranking, memory `id` dedup behavior, `MAX_LIMIT`, and successful-empty/error semantics at the per-logical-file retrieval level and existing context-plan composition behavior.

### 4.7 Prefix-Filter Resolution in Search, Locate, and Insights (`src/server.rs`, `src/store/ladybug.rs`, `src/telemetry/insights.rs`, `#273`, R7)

- **Schema/handler/evaluation context**:
  - Add optional `project_root` to both `neurostrata_search_memory` and `neurostrata_insights` tool schemas, handler argument parsing, and evaluation/resolution context as owner-approved.
  - `locate` already accepts `project_root`; implementation must use that existing argument (not only session ledger behavior) for prefix/path filter resolution parity.
- **Shared helper usage**: `search_memory`, `locate`, and `insights` actions/queries of kind `lexical` and `ast_calls` resolve `path_prefix` through the shared prefix-forms helper (§4.1.3) using root precedence `project_root -> namespace registry root -> .git inference` (with request-scoped hoisted registry root where applicable).
  - Insights evaluate persisted prefix filters dynamically at execution time through the helper (no stored-data migration).
- **Matching semantics**:
  - Support namespaces/data where matching entries are stored with relative and/or absolute spellings by testing candidate prefix forms against stored spellings using normalized comparison compatible with existing stored-path normalization (no widening beyond prefix match).
  - Empty prefix => no filter (unchanged).
  - Truly unresolvable absolute prefix/path in namespace scope => explicit actionable error response for that tool/query; never silent empty and never broadened scope.
  - Resolvable prefixes that match nothing return successful empty results.
- **SPARQL unaffected**: SPARQL query parsing/execution and SPARQL insight query kinds do not adopt path-prefix filesystem resolution logic.

### 4.8 Test-Only CodeQL Logging Remediation (`#8-#11`, R10)

- **`src/rdf/reasoning.rs`** (alerts `#8`, `#9`, around current lines 2687 and 2700 in `inferred_limit_aborts_before_full_closure`):
  - Replace assertion/panic diagnostics that interpolate `full_count`/`actual` (or other computed values) into formatted strings with static, non-interpolating messages.
  - Preserve the same assertions and control flow: fixture must still produce a large closure (`full_count > 500`) and the tiny-cap path must still assert `actual < full_count` and `max == 8` on `InferredLimitExceeded`.
  - No fixture data, materialization limits, or production reasoning behavior changes.
- **`src/semantic_eval/competency.rs`** (alerts `#10`, `#11`, around current lines 749 and 750 in `i3_materialization_isolates_inferences_and_persists_provenance`):
  - Replace assertion diagnostics that interpolate certified/N-Quad-derived content into formatted strings with static, non-interpolating messages.
  - Preserve assertions validating inference-graph isolation, certified N-Quad graph placement/presence, and RDF-star provenance expectations; synthetic fixtures unchanged.
  - No suppression annotations or dismissal workflow as a substitute; fix is in test code only.
- **Merge/alert handling**: PR verification proves CodeQL analysis no longer reports the fixed lines; merge auto-closes alerts. No manual dismissal and no claim that alerts are closed before merge.

---

## 5. Data Flow

### 5.1 File/Seed Lookup Flow (memories_for_file, related file seed, context_plan per-file)
1. Caller provides `namespace`, `path`/`id`, optional `project_root`.
2. Request resolves registry root once in scope (`Result<Option<String>>`, fail-closed on `Err`).
3. Shared resolver computes `LookupPath` + ordered `lookup_forms` (host-aware parsing, root precedence, vetoes, #266 alternates).
4. Consumers query/resolve per logical file/seed:
   - `memories_for_file`: use the concrete per-logical-file multi-form memory retrieval path (§4.3.2) to gather uncapped-enough raw candidates across ordered forms, merge globally under existing rank/id/cap semantics, enrich conceptual bridges once on the final per-logical-file hit set, and union AST neighborhoods across all forms per §4.3.3.
   - `related` path seed: resolve to one canonical in-namespace node id, then single traversal.
   - `context_plan`: for each deduped logical file, reuse the same per-logical-file multi-form memory retrieval path and required AST union path, preserving per-file semantics (no request-global memory/bridge selection).
5. Responses preserve original supplied seed/path fields; additive resolved/canonical fields are included only where specified.

### 5.2 Prefix Filter Flow (search, locate, insights lexical/ast_calls)
1. Caller provides optional `path_prefix` and optional `project_root` (`neurostrata_search_memory`, `neurostrata_insights`; `locate` uses its existing `project_root`).
2. Shared prefix-forms helper evaluates prefix dynamically against known roots/registry root.
3. Empty prefix bypasses filtering; unresolvable absolute prefix returns explicit error; resolvable prefix produces equivalent-form candidates matched against relative/absolute stored spellings.
4. SPARQL path remains unchanged.

### 5.3 `diff_tour` Flow
1. Parse/collect changed spans from `diff_text` and/or `changed_spans`, preserving supplied display paths.
2. Resolve each span to internal lookup forms/canonical stored path via shared resolver.
3. Perform ingestion/governance/AST lookups using internal forms; record resolved canonical stored path in session ledger.
4. Emit user-visible response using supplied/display paths and existing response schema semantics.

### 5.4 `context_plan` Flow (performance-safe, per-file-correct)
1. Resolve registry root once.
2. Normalize/dedupe requested file inputs (construction-time).
3. For each deduped logical file, resolve forms and retrieve memory evidence via the concrete per-logical-file multi-form retrieval path (uncapped-enough raw candidates under safety budgets, global rank/id/cap merge, conceptual bridges once on final per-file hits) and retrieve AST evidence by unioning all equivalent forms when required.
4. Compose plan results preserving existing per-file/context-plan semantics, memory ranking, memory `id` dedup, `MAX_LIMIT`, and successful-empty/error behavior.
5. Return plan with preserved semantics and honest errors/truncation only as defined.

---

## 6. Error Handling, Security, and Privacy

### 6.1 Error Handling (explicit)

| Condition | Behavior |
|---|---|
| Registry/root cache store read failure (`cached_project_root` -> `Err`) | Propagate error; lookup/prefix/context operations fail closed with explicit actionable error; no fallback widening to `.git`/broader scope |
| Cached `project_root` absent/empty/invalid metadata | `Ok(None)`; normal root-precedence resolution continues without using invalid cache data |
| Truly unresolvable absolute path/prefix (cannot map into valid in-namespace known root/scope) | Explicit actionable error (never silent empty, never broadened scope) |
| Resolvable path/prefix with no matches | Successful empty result set/fields as applicable |
| Multi-form retrieval: some forms error, some succeed (including successful-empty) | Overall success; union/merge successful results; log/retain diagnostics internally as needed; no user-visible fabricated failure |
| Multi-form retrieval: all forms fail | Return first form-order error |
| Multi-form memory candidate collection cut by existing safety budget | Deterministic form-order collection cutoff with honest truncation signaling; global rank/dedup/cap applied to collected candidates (no silent winner-take-all) |
| Multi-form AST truncation under caps/budgets | Deterministic form-order truncation with honest ORed truncation flags and canonical `truncated_collections` ordering |
| `related` file-seed resolves to no canonical in-namespace node id | Explicit not-found/unresolvable error; preserve original `seed`; no traversal fallback |
| `related` non-path seed | Existing behavior unchanged |

### 6.2 Security and Namespace Safety

- Root precedence and all nested/foreign repository veto logic remain enforced before any alternate/form is accepted.
- Only valid, non-vetoed roots may contribute alternates or prefix candidates.
- No cache/store error may cause cross-namespace leakage, root escalation, or scope widening.
- `context_plan` registry hoisting and per-logical-file reuse are internal optimizations only; they must not bypass authorization, freshness/exclusion handling, namespace constraints, or per-file semantic boundaries.

### 6.3 Privacy

- No new data exfiltration surfaces. Session ledger continues to store path metadata only, now using resolved canonical stored paths for `diff_tour` entries as specified.
- CodeQL remediation removes interpolated computed values from test diagnostics; no production logging/content behavior changes.

---

## 7. Performance Design and Evidence (`#270`)

### 7.1 Required Optimizations (in implementation PR)

- Hoist namespace registry root read to once per `context_plan` request.
- Construction-time dedupe of requested file/path work items (normalized key, stable first-seen order).
- Concrete per-logical-file multi-form retrieval path (ordered equivalent forms in one candidate-discovery operation or one store-facing multi-form API) reused per logical file to eliminate duplicate equivalent-form store queries for identical lookup inputs within a request.
- Conceptual bridge enrichment exactly once per logical file’s final merged/ranked/deduped/capped hit set (not per form, not once globally across the request).
- AST neighborhoods continue to query/union all equivalent forms per logical file when required (no weakening).

### 7.2 Objective Benchmark and Evidence Requirements

Implementation PR must include deterministic before/after evidence using a synthetic namespace fixture with:
- `>= 10,000` memories and `>= 100` files with mixed stored spellings (relative and absolute/equivalent forms represented),
- `5` warmup iterations + `20` measured iterations,
- reported **median** and **p95** wall-clock time, and
- reported invocation counts for: registry root reads, store memory multi-form candidate-discovery operations/queries (or equivalent instrumented count), per-form AST neighborhood queries, and conceptual bridge enrichment passes.

**Pass criteria (all required):**
1. `context_plan` registry root reads `== 1` per multi-file request.
2. No duplicate equivalent-form store queries for the same effective lookup inputs within a request.
3. Conceptual bridge enrichment occurs exactly once per logical file’s final merged/ranked/deduped/capped hit set (count equals number of logical files whose final hit sets are enriched, not `1` for the whole request unless only one logical file is enriched).
4. Strictly fewer total redundant invocations (registry/store-form/bridge) than baseline.
5. Wall-clock time reported honestly (median/p95) with no hidden regression; any tradeoff must be explicitly reported in PR/changelog evidence text (still no prohibited performance/ROI claims).

Instrumentation may use test-only counters/hooks or deterministic tracing sinks scoped to the benchmark harness; production observability behavior must remain compatible.

---

## 8. File / Symbol Map (Grounded in Current Source)

| Area | Current file(s) / symbols (verified) | Planned change in implementation PR |
|---|---|---|
| Absolute parsing and host semantics | `src/paths.rs`: `parse_absolute`, `is_absolute_any`, `relative_to`, `AbsPath`; existing Windows/UNC and POSIX tests | Make `//` handling host-native (POSIX on Unix, UNC on Windows) while preserving cross-host absolute recognition, verbatim prefix handling, and conditional tests; add Linux `//server/share` POSIX coverage |
| Root precedence and alternates | `src/paths.rs`: `LookupPath`, `RootSource`, `to_project_relative`, `choose_root`, `lookup_forms` | Retain broader valid `project_root` win + deeper valid registry-root alternate when both contain file; dedupe/normalize alternates at construction (same normalized key as `lookup_forms`), stable first-seen; preserve vetoes and relative/Outside behavior |
| Shared prefix/path helpers | `src/paths.rs` (new helper), `src/server.rs` lookup orchestration | Add shared prefix-forms helper and shared canonical in-namespace file-seed resolver; use in `search_memory`, `locate`, `insights` (`lexical`/`ast_calls`), `related` file seeds, and `diff_tour` internal resolution |
| Registry cache fail-closed | `src/registry.rs`: `cached_project_root`, `cache_project_root` (unchanged write contract) | Change `cached_project_root` to `Result<Option<String>>`; `Ok(None)` for absent/invalid metadata, `Err` on store read failures; update all call sites to propagate and fail closed |
| Memory multi-form retrieval | `src/server.rs`: `lookup_path_for`, `memories_for_forms`, `merge_spelling_results`, `handle_memories_for_file`; `src/store/ladybug.rs`: `memories_for_file` (per-form) and conceptual-bridge enrichment path | Add concrete per-logical-file multi-form store retrieval (ordered forms, one candidate-discovery operation or one store-facing multi-form API), gather uncapped-enough raw candidates under safety budgets before global rank/dedup/cap, preserve existing rank/id/cap/any-success/first-error semantics, and perform conceptual bridge enrichment once on final per-logical-file hits; eliminate redundant per-form fan-out/enrichment |
| AST union/truncation | `src/server.rs`: `ast_neighborhood_for_forms`, `ast_summary_is_empty`, `handle_memories_for_file`; `src/store/ladybug.rs`: `ast_neighborhood_for_file` | Query/union all equivalent forms (no weakening), deterministic union/dedupe, ORed truncation flags, canonical `truncated_collections`, recomputed `blast_radius_count`, and preserved success/error precedence |
| `related` tool | `src/server.rs`: `handle_related`, tool schema/arg parsing for `neurostrata_related` | Add optional `project_root`; resolve file-path seeds via shared lookup forms to one canonical in-namespace node id before single traversal; preserve original `seed`, add additive resolved seed field; no multi-traversal/cursor changes |
| `diff_tour` | `src/graph_read/diff_tour.rs`: `DiffTourRequest`, `ChangedFileSpan`, `parse_unified_diff_spans`, `execute_diff_tour`; `src/server.rs`: `handle_diff_tour` | Dual display/internal path handling for spans and diff-derived paths; resolved-form ingestion/governance lookups; supplied/display paths preserved in outputs; session ledger records resolved canonical stored path |
| `context_plan` perf/correctness | `src/server.rs`: `handle_context_plan`, `lookup_path_for`, per-file retrieval/enrichment path | Hoist registry read once/request, construction-time dedupe, reuse concrete per-logical-file multi-form memory retrieval and per-file AST union as required, conceptual bridge enrichment once per logical file’s final hit set; preserve semantics/caps/ranking/dedup |
| Prefix parity | `src/server.rs`: `handle_search_memory`, `neurostrata_search_memory` schema, locate/insights handlers and argument plumbing, `neurostrata_insights` schema; `src/telemetry/insights.rs`: `execute_insights`, `InsightDefinition.path_prefix` (dynamic evaluation only); `src/store/ladybug.rs`: prefix-filter query paths used by search/locate/insights lexical/ast_calls | Add optional `project_root` to `neurostrata_search_memory` and `neurostrata_insights`; route `path_prefix` through shared helper for search/locate/insights lexical/ast_calls; support relative+absolute stored spellings; dynamic persisted-insight evaluation; explicit error on truly unresolvable absolute prefix; SPARQL unchanged |
| CodeQL test-only fixes | `src/rdf/reasoning.rs` (around 2687, 2700 in `inferred_limit_aborts_before_full_closure`), `src/semantic_eval/competency.rs` (around 749, 750 in `i3_materialization_isolates_inferences_and_persists_provenance`) | Static non-interpolating assertion diagnostics only; assertions/fixtures/production behavior unchanged; PR verification proves CodeQL analysis no longer reports fixed lines and merge auto-closes alerts |
| Release/docs (implementation PR) | `CHANGELOG.md`, `docs/releases/changelog/1.19.0.md` | Record actual fixes and evidence; no prohibited claims |

**Planned new internal helpers/symbols (names may vary, responsibilities fixed):** shared prefix-form resolver in `src/paths.rs`; canonical file-seed node resolver (server-side, using shared forms + store probe); concrete per-logical-file multi-form memory retrieval API/helper; AST/memory union merge helpers preserving deterministic ordering, truncation metadata, and existing memory merge semantics. Existing public MCP tool names remain unchanged.

---

## 9. Formal Kernel Evaluation (Bitemporal / Verification Reflex)

- **Scope evaluated**: host/path parsing, root selection alternates, prefix-form expansion, multi-form query aggregation/union, truncation-flag ORing, dedupe/order policy, and fail-closed error propagation for I/O/store-read outcomes.
- **Decision**: **No Lean extraction or new `src/formal_kernel.rs` decision is required** for this remediation.
- **Rationale**:
  - These changes do not introduce or modify a conservation invariant, CRDT/merge rule, sovereignty/ACL check, lease/claim rule, or a pure state-machine transition/gate whose correctness is independent of filesystem/host behavior and store query results.
  - `parse_absolute`/root containment and `..`/canonicalization behavior depend on platform/host semantics and filesystem state (`relative_to_on_disk`, canonicalization, symlink handling) and therefore are boundary logic, not a pure kernel decision suitable for extraction without modeling the OS/FS environment.
  - Cross-form union/truncation behavior and per-logical-file candidate gathering/merge are deterministic aggregation policies over fallible query results and existing caps/flags; correctness is specified by deterministic ordering/dedupe/truncation contracts and is best verified by focused Rust regression/property-style tests rather than Lean proofs of filesystem parsing.
  - No changes are made to `TransitionTask`, action gates, ReBAC policy semantics, or bitemporal ledger/state transitions beyond consuming existing resolved paths/metadata.
- **Verification consequence**: implementation PR must not modify `proofs/NeuroStrata/*.lean` or `tests/lean_parity_test.rs` for this scope; correctness is enforced through the Rust test/perf evidence in §10 and existing gates.

---

## 10. Testing, Verification, and Acceptance Criteria

### 10.1 Test Strategy (TDD, focused-first)

Implementation follows red-green regression per issue/alert in implementation lanes with **focused tests only** during lane work (no full-workspace `cargo test`, no sleep polling). Primary orchestrator later runs full serial verification and CI gates as listed in §10.3.

### 10.2 Per-Issue / Per-Alert Acceptance Criteria

#### `#266` — Retain registry-relative alternate when broader project root wins
- **Acceptance**:
  - Given a file contained by both a broader valid `project_root` (winning) and a deeper valid registered namespace root, `LookupPath.path/source/root` reflect the broader root (no priority change) and `LookupPath.alternates` includes the registry-relative spelling.
  - Alternates are deduped at construction using the same normalized key as `lookup_forms`, stable first-seen order.
  - Only valid, non-vetoed roots contribute alternates; nested/foreign repo vetoes unchanged.
- **Focused evidence**: `src/paths.rs` / `src/server.rs` lookup-form tests covering winning-broader-root + valid-registry-root alternate retention and veto preservation.

#### `#267` — Resolve absolute file seeds in `neurostrata_related`
- **Acceptance**:
  - `neurostrata_related` accepts optional `project_root`.
  - Absolute file-path seed resolves through shared lookup forms to exactly one canonical in-namespace node id before one traversal call.
  - Original `seed` preserved in response; additive resolved seed surfaced; no ambiguous multi-traversal and no cursor semantic change.
  - Relative and absolute stored-location cases resolve correctly under namespace isolation.
  - Truly unresolvable file-path seed returns explicit actionable error (not empty success).
- **Focused evidence**: server/store related-path resolution tests for relative/absolute stored locations and unresolvable seed.

#### `#268` — Resolve absolute diff_tour span paths before ingestion lookup
- **Acceptance**:
  - `changed_spans` and `diff_text` paths are resolved through shared file-path resolution before ingestion/governance lookups.
  - User-visible diff-tour outputs preserve supplied/display paths.
  - Session ledger records resolved canonical stored path.
  - Relative behavior unchanged; absolute-span regression covered even if no current sender emits one.
- **Focused evidence**: `src/graph_read/diff_tour.rs` and/or server integration tests for absolute-span ingestion/governance resolution, display-path preservation, and ledger canonical path recording.

#### `#269` — Disambiguate POSIX double-slash paths from Windows UNC paths
- **Acceptance**:
  - On Unix hosts, `//server/share/...` parses/interprets as POSIX absolute (not UNC) per owner decision; on Windows hosts, UNC parsing preserved.
  - Verbatim Windows prefixes and existing conditional tests preserved.
  - Linux daemon behavior explicitly pinned/documented and tested as POSIX for `//server/share`.
- **Focused evidence**: `src/paths.rs` host-conditional parsing tests including `//server/share`, verbatim UNC/drive forms, and root-matching expectations.

#### `#270` — Reduce multi-spelling query fan-out with per-logical-file correctness
- **Acceptance**:
  - `context_plan` performs exactly one registry root read per multi-file request.
  - No duplicate equivalent-form store queries for identical effective lookup inputs within the request.
  - Conceptual bridge enrichment runs exactly once per logical file’s final merged/ranked/deduped/capped hit set (not per form, not once globally across the request).
  - Per-logical-file memory retrieval uses the concrete multi-form path (ordered forms in one candidate-discovery operation or one store-facing multi-form API), gathers uncapped-enough raw candidates under existing safety budgets before global rank/dedup/cap, preserves existing rank/id/cap and any-success/first-error-only-if-no-success semantics, and honestly signals truncation if safety budgets cut candidates.
  - AST neighborhoods continue to query/union all equivalent forms per logical file when required.
  - Strictly fewer total redundant invocations (registry/store-form/bridge) than baseline.
  - Deterministic synthetic benchmark (`>=10,000` memories, `>=100` files, mixed spellings, 5 warmups + 20 measured iterations) reports median/p95 wall time and invocation counts honestly with no hidden regression.
  - No semantic change to `context_plan` `project_root`/`source` behavior beyond hoisting; memory ranking, memory `id` dedup, `MAX_LIMIT`, and successful-empty/error semantics preserved.
- **Focused evidence**: benchmark/instrumentation harness output captured in PR evidence plus focused semantic parity tests for `context_plan`/per-file results and bridge-enrichment counts.

#### `#271` — Preserve AST truncation signals across spelling fallback
- **Acceptance**:
  - Truncated-empty or truncated-partial AST results from one form cannot be replaced in a way that discards truncation metadata when another form contributes data.
  - Truncation flags are ORed across forms; canonical `truncated_collections` order `symbols,callers,callees,imports,importers`.
  - Truncated-first/populated-later regression passes with unioned data and honest truncation preserved.
- **Focused evidence**: `src/server.rs` AST union tests with synthetic per-form truncated/non-truncated summaries.

#### `#272` — Union AST neighborhoods across equivalent file spellings
- **Acceptance**:
  - `symbols`, `callers`, `callees`, `imports`, `importers`, and `related_memories` are unioned across forms.
  - Dedupe keys: `symbols/callers/callees/importers` by `id`; `imports` by `path`; `related_memories` by `(id, relation, direction, supersedes)`.
  - Stable first-seen deterministic ordering and recomputed `blast_radius_count` from union.
  - Mixed-spelling regression fails under winner-take-all and passes under union.
- **Focused evidence**: mixed-spelling union regression tests for memories_for_file AST/related results.

#### `#273` — Resolve absolute `path_prefix` filters in search, locate, and insights
- **Acceptance**:
  - Optional `project_root` is added to both `neurostrata_search_memory` and `neurostrata_insights` schemas/handlers/evaluation context; `locate` uses its existing `project_root` argument.
  - `search_memory`, `locate`, and `insights` (`lexical`, `ast_calls`) use one shared prefix-forms helper with root precedence and registry-root awareness.
  - Absolute and relative prefixes resolve consistently across relative/absolute stored spellings.
  - Persisted insight prefixes evaluated dynamically (no migration).
  - Empty prefix remains no filter; SPARQL unaffected.
  - Truly unresolvable absolute prefix returns explicit actionable error; resolvable-no-match returns successful empty.
  - Parity tests across the three tools pass.
- **Focused evidence**: server/store/insights focused tests for schema/arg handling, absolute-prefix parity, dynamic persisted-prefix evaluation, and explicit unresolvable-prefix errors.

#### `#274` — Do not collapse cached project-root store errors to a cache miss
- **Acceptance**:
  - `cached_project_root` returns `Result<Option<String>>`; store read errors propagated as `Err`, absent/invalid metadata as `Ok(None)`.
  - Lookup handlers fail closed on propagated cache-read errors (no fallback widening/silent empty).
  - Error-path test distinguishes store failure from genuine miss.
- **Focused evidence**: `src/registry.rs` and caller-path tests injecting store read failure vs missing/invalid metadata.

#### CodeQL `#8`, `#9` (`src/rdf/reasoning.rs`)
- **Acceptance**:
  - `inferred_limit_aborts_before_full_closure` uses static non-interpolating assertion diagnostics at/for the alerting assertions.
  - Assertions and synthetic fixture behavior unchanged (large closure threshold, `InferredLimitExceeded { max: 8, actual < full_count }`).
  - No production reasoning behavior change.
  - PR verification proves CodeQL analysis no longer reports the fixed lines; merge auto-closes alerts (no manual dismissal).

#### CodeQL `#10`, `#11` (`src/semantic_eval/competency.rs`)
- **Acceptance**:
  - `i3_materialization_isolates_inferences_and_persists_provenance` uses static non-interpolating assertion diagnostics at/for the alerting assertions.
  - Assertions and synthetic dataset/certificate/provenance checks unchanged.
  - No production behavior change.
  - PR verification proves CodeQL analysis no longer reports the fixed lines; merge auto-closes alerts (no manual dismissal).

### 10.3 Verification Gates (implementation PR completion criteria)

- **During implementation lanes**: fast, targeted/focused unit/integration tests only for touched behavior; no full-workspace `cargo test`; no `sleep` polling.
- **Primary orchestrator (pre-publication/merge readiness)**:
  - full serial test suite passes,
  - `fmt` and `clippy` pass,
  - Lean parity passes (`tests/lean_parity_test.rs` and repository-standard Lean checks) with no new kernel changes required by this design,
  - performance benchmark/evidence captured and meets §7.2 criteria,
  - independent review completed with zero Critical/Important findings,
  - all CI checks green,
  - CodeQL analysis no longer reports fixed lines for alerts `#8-#11` and merge auto-closes those alerts (no manual dismissal).
- **Push gate compliance**: task completion follows NeuroStrata task/memory extraction rules before push; unrelated working tree state (including the four preserved stashes and excluded hooks/scripts/artifacts) is not modified by this PR.

### 10.4 Regression Safety Matrix

| Surface | Must not regress |
|---|---|
| Path safety | namespace isolation, nested/foreign repo vetoes, `Outside` semantics, root precedence |
| Existing callers | current tool argument compatibility (new args optional/additive), unchanged fields/meanings preserved |
| Retrieval semantics | memory ranking, memory `id` dedup, `MAX_LIMIT`, successful-empty vs error distinction, per-logical-file memory/bridge semantics |
| `diff_tour` UX | supplied/display path preservation and relative-path behavior |
| Insights/SPARQL/search | SPARQL unaffected; persisted insight prefixes not migrated but dynamically resolved; optional `project_root` additive on search/insights |
| Product claims/docs | honest semantic `FAIL` and all prohibited-claim boundaries preserved |

---

## 11. Release and Documentation Plan (Implementation PR)

The implementation PR must update release documentation with actual, evidence-backed outcomes only:

- **`CHANGELOG.md`**: add `v1.19.0` remediation entries covering `#266-#274` and CodeQL `#8-#11` fixes, including concrete behavior changes (fail-closed cached-root errors, host-native `//` parsing, prefix parity with additive `project_root` on search/insights, deterministic cross-form unions/truncation preservation, per-logical-file multi-form memory retrieval with preserved rank/id/cap and once-per-logical-file bridge enrichment, related absolute-seed resolution, diff-tour path duality, context-plan hoisting/dedup) and measured performance evidence summary.
- **`docs/releases/changelog/1.19.0.md`**: detailed release-note entry with issue/alert mapping, verification summary, and benchmark evidence (median/p95, invocation-count deltas) as measured.
- **Claim boundaries**: no semantic `PASS`, no ROI, no blind-agent, no rollback, no curation-cost, and no real-namespace performance/generalization claims. If any metric is neutral or mixed, report it honestly without promotional framing.

---

## 12. Implementation Notes for the Subsequent Coding Phase (Non-Plan Constraints)

- This document is the approved design/spec only; task-by-task implementation sequencing is produced separately and must conform to this spec exactly.
- Any newly discovered ambiguity that would change root precedence, veto behavior, error/success semantics, dedupe keys, truncation signaling, per-logical-file vs request-level enrichment semantics, or benchmark acceptance thresholds must be escalated before coding; no silent reinterpretation.
- Preserve all four existing git stashes and avoid unrelated file churn (`scripts/hooks/pre-commit`, `libformal_kernel_check.rlib`, and other out-of-scope paths).

---

## 13. Self-Review Checklist (Spec Completeness)

- [x] One PR scope only; closes `#266-#274`; addresses CodeQL `#8-#11`; excludes `#68/#264`.
- [x] Architecture, components, data flow, error behavior, security/privacy, performance, testing, and release sections included.
- [x] Per-issue and per-alert acceptance criteria defined, including corrected competency test name and CodeQL merge/alert handling.
- [x] File/symbol map grounded in current source with planned change responsibilities, including concrete per-logical-file multi-form retrieval and additive `project_root` for `neurostrata_search_memory`/`neurostrata_insights`.
- [x] Formal-kernel evaluation completed with explicit no-extraction rationale.
- [x] No `TODO`/`TBD`/`FIXME`, no placeholders, no stubs, no deferred phases, and no implementation-plan task list.
- [x] Owner-approved constraints and invariants (namespace isolation, vetoes, honest semantics, preserved outputs/claims, per-logical-file enrichment semantics, AST union-all-forms, no per-form global-hit loss without honest bounded truncation) explicitly captured.
