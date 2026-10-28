# Plan 5: Domain DAG, Facets & Mesh Alignment Implementation Plan (Revised)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** Complete F9 (multi-domain memories / node_set), F15 (Domain DAG + aliases), F16 (class facets), F17 (canonical entity slugs), F20 (cross-namespace concept IRIs on SynapticMesh share), F21 (ontology grounding annotate-only). Reconcile already-landed code with spec contracts and fix unvetted bugs.

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (§2.1, §2.2, §3 F9, F15, F16, F17, F20, F21, §5 Plan 5)

**Vetting:** Argon REVISE (2026-10-28), K3 REVISE (2026-10-28). This revision incorporates both.

## Verified Baseline (already landed — with bugs to fix)
- `src/domain.rs`: normalize_domain_slug, domain_key, domain_keys (F9 array), compute_facets (F16), canonical_entity_slug (F17), detect_conflicts (F10 — Plan 6 fixes).
- `src/registry.rs`: v1/v2 domain entries, validate_domain_entries (DAG cycle + alias/alias case-folded), registry_metadata_v2, **BUG: write_registry clobbers v2 to v1**, **BUG: check_domain rejects arrays**.
- `src/rdf/ontology.rs`: OntologyIndex (cycle guard via HashSet — nondeterministic), canonical_slug (does not normalize lookup key).
- `src/store/ladybug.rs`: single ArcSwap<OntologyIndex> (cross-namespace contamination), upsert canonicalizes topology in-place (destroys raw), stamps facets (already), refresh_ontology called in search.
- `src/server.rs`: node_sets filter (singular key node_set), domain_key (singular) in several paths, node_set filter post-primary-limit (false empty).
- `src/rdf/namespace.rs` + `projection.rs`: memory_iri replaces subject with concept_iri (BUG: collides memories).
- `src/synaptic_mesh/worker.rs`: projected_metadata allowlist (eligibility, source, tags) — concept_iri NOT included.
- `src/synaptic_mesh/import.rs`: clones wire metadata.
- `src/rdf/canonical.rs`: canonicalize_topology (overwrites raw), canonicalize_known_ids_alias (unused).

## Global Constraints
- JSON only; no LLM.
- Zero new dependencies; no banned crates.
- Deterministic (BTreeMap/BTreeSet, sorted outputs).
- Warn-first, never reject (except registry validation on mutation — scoped).
- Backward compatible: v1 registry deserializes; legacy rows read cleanly.
- SynapticMesh privacy: local domain/aliases stay private; concept_iri is shareable.

---

## Task 1: Fix F9 multi-domain writes end-to-end

**Files:** src/registry.rs, src/server.rs, src/task/mod.rs, src/domain.rs (callers), src/guard/testkit.rs, src/buffer.rs, src/synaptic_mesh/import.rs.

- [ ] **Fix check_domain** (registry.rs:264): accept `domain: string | string[]` via domain_keys; validate each slug; register all.
- [ ] **Migrate singular readers to multi-domain:** derived_domains (registry.rs:441), server.rs:1835/2195-2256, task/mod.rs:1324, guard/testkit.rs:188, buffer.rs:417, synaptic_mesh/import.rs:1351 — use domain_keys, iterate all.
- [ ] **node_set key:** use `metadata.node_set: string[]` (Spec §2.1) everywhere; retain `node_sets` only as MCP filter param name. Support both `node_set`/`node_sets` on read while writing `node_set`.
- [ ] **node_set stamping on ingest:** file/symbol/doc_section nodes get `metadata.node_set` (derived from path/module or []). Preserve on carry-forward (re-ingest).
- [ ] **Filtering before primary limit:** move node_set/domain/facet filtering before primary limit (server.rs:3051-3067) to avoid false empty. Pagination deferred to a future plan.
- [ ] **AND/OR semantics:** document and implement (default OR; optional `node_set_mode: "all"|"any"`).
- [ ] **Tests:** array-domain add + either-domain retrieval; legacy rows; file/symbol/doc_section node_set; matching row beyond page 1; explicit AND/OR.

## Task 2: Fix F15 registry v2 durability + alias uniqueness + per-namespace ontology

**Files:** src/registry.rs, src/store/ladybug.rs, src/rdf/ontology.rs.

- [ ] **Fix write_registry clobber:** read-merge-write preserving v2 entries (parent, aliases, concept_iri) and unrelated metadata (project_root, doctor_state, enrichment_state). Never downgrade v2 to v1.
- [ ] **v1→v2 migration:** when any v2 entry present, write v2; legacy writers upgrade non-destructively.
- [ ] **Alias uniqueness:** validate no alias collides with any slug (case-folded AND slug-normalized), and no alias-to-alias collision. Current lowercase-only insufficient.
- [ ] **Per-namespace ontology:** key OntologyIndex by namespace (BTreeMap inside ArcSwap or refresh on write). refresh_ontology before upsert canonicalization. Interleaved A/B tests.
- [ ] **Deterministic OntologyIndex:** replace HashSet/HashMap with BTreeSet/BTreeMap for cycle breaking + alias map. Deterministic collision handling.
- [ ] **canonical_slug fix:** normalize lookup key via normalize_domain_slug before alias_map check.
- [ ] **MCP tool surface:** add domain registry CRUD tools (list/set/remove domain with parent/aliases/concept_iri), exact schemas. (Currently none.)
- [ ] **Tests:** v2 survives add/import/project_root/enrichment updates; unrelated metadata survives; normalized alias/slug collisions rejected; cycles rejected; interleaved namespace A/B writes+searches.

## Task 3: F16 facet stamping (extend, not duplicate)

**Files:** src/store/ladybug.rs, src/domain.rs.

- [ ] Acknowledge upsert stamping already landed (ladybug.rs:1896-1901); extend: deterministic sort + dedup of metadata.facets; merge (not clobber) manual facets.
- [ ] Facets survive search_memory + export_graph; filter not post-limit.
- [ ] **Tests:** idempotent stamp, sorted, merged with manual facets; legacy rows filter; export retains facets.

## Task 4: F17 canonical slugs (preserve raw + query expansion)

**Files:** src/rdf/canonical.rs, src/store/ladybug.rs, src/server.rs (search), src/domain.rs (detect_conflicts).

- [ ] **Preserve raw:** canonicalize_topology stores canonical slugs in component/target_system AND preserves raw under `topology_original` (or component_raw/target_system_raw). Idempotent re-upsert.
- [ ] **Refresh ontology before canonicalize** in upsert (per-namespace).
- [ ] **Query expansion (F17.3):** search expands aliases to canonical slug (use OntologyIndex aliases). Demonstrate "LadybugDB" finds row written as "lbug".
- [ ] **detect_conflicts uses canonical slugs** for topology matching (Plan 6 F10 consumes these).
- [ ] Define Plan 5→Plan 6 interface; move F10 execution assertion to Plan 6.
- [ ] **Tests:** raw preserved once; repeated upsert idempotent; correct namespace alias; alias query finds canonical row.

## Task 5: F20 concept_iri mesh alignment + RDF subject fix

**Files:** src/rdf/namespace.rs, src/rdf/projection.rs, src/store/ladybug.rs (upsert), src/synaptic_mesh/worker.rs, src/synaptic_mesh/import.rs, src/synaptic_mesh/record.rs.

- [ ] **Fix RDF subject collision:** memory_iri keeps per-memory ID as subject (`urn:neurostrata:memory:{id}`); emit concept_iri as separate predicate/type quad (e.g., `ns:conceptIri` or `rdf:type` concept). Replace existing subject-override test with two-memories/one-concept non-merging regression test.
- [ ] **Write-side stamping:** on upsert, if metadata.domain resolves to v2 registry entry with concept_iri (and metadata.concept_iri absent), stamp metadata.concept_iri (string or string[] for multi-domain). So OutboxEntry captures it.
- [ ] **Allowlist:** add `concept_iri` to worker::projected_metadata (worker.rs:283). Keep domain/aliases private.
- [ ] **Import-side mapping:** on mesh import, preserve concept_iri AND map to receiver's local domain slug if receiver registry has matching concept_iri. Define 0/1/>1 matches, deterministic warnings, idempotent replay. Never merge private registries.
- [ ] **Multi-domain concept_iri:** singular IRI per domain; memory with multiple domains → string[] of concept IRIs.
- [ ] **Tests:** sender domain→IRI stamp → curated wire record → receiver local-slug mapping → receiver alias search; no-match/ambiguous; replay; private signals absent; two memories sharing one IRI keep distinct RDF subjects.

## Task 6: F21 ontology grounding (annotate/strict)

**Files:** new src/ontology_grounding.rs (or in domain.rs), src/server.rs, src/main.rs (doctor), src/task/gate.rs.

- [ ] **Per-namespace config:** `ONTOLOGY_FILE_PATH` env var + optional config key; per-namespace path/mode (annotate default, strict optional). Missing/unreadable must NOT silently become empty vocabulary flagging all rows.
- [ ] **JSON vocabulary schema:** deterministic, e.g. `{ "concepts": [{ "slug": "...", "concept_iri": "...", "aliases": [...], "parent": "..." }] }`.
- [ ] **Annotate mode:** on write, if domain matches a concept (slug or alias via canonical_slug), stamp concept_iri if absent. Never rejects.
- [ ] **Strict mode:** warn (not reject) if domain not in vocabulary; emit [neurostrata:ontology] warning.
- [ ] **Diagnostics:** out-of-vocabulary warnings surface in doctor and task_validate (warn-first, never reject; never filters/rejects content).
- [ ] **Tests:** annotate, existing-IRI precedence, domain arrays, malformed/missing file, out-of-vocab doctor/task_validate warnings, retained content, deterministic repeated output.

## Library Fit & Constraints
- Zero new dependencies.
- No banned crates.
- Deterministic.

## Checkable Acceptance Criteria
- [ ] AC1 (F9): node_set stamped; search filters; AND/OR; backward compat; multi-domain writes work.
- [ ] AC2 (F15): v2 durable across writes; alias uniqueness (slug+alias+normalized); deterministic OntologyIndex; per-namespace scoping; MCP CRUD tools; v1/v2 round-trip.
- [ ] AC3 (F16): facets idempotent/sorted/merged; search+export retain; filter not post-limit.
- [ ] AC4 (F17): raw preserved; alias query expansion; namespace ontology refresh; F10 interface defined.
- [ ] AC5 (F20): concept_iri allowlisted; write-side stamp; import-side mapping; RDF subject distinct; multi-domain IRI; privacy preserved.
- [ ] AC6 (F21): per-namespace config; annotate/strict; diagnostics in doctor+task_validate; deterministic; no LLM; no rejection.

## Cross-Plan Boundaries
- Plan 5 → Plan 6: F10 conflict detection consumes canonical slugs (F17) and concept_iri. Plan 6 must not duplicate alias resolution.
- Plan 5 → Plan 4: F18 shape checks validate domain entries (registry) as part of shape validation.

## Contract Specifications (added per K3 v2 required additions)

### C1 — node_set derivation (B1)
- **Per node kind:**
  - `file` / `directory`: `metadata.node_set = []` by default (free-form; ingest does not guess). If a `--node-set` CLI flag or `node_set` in ingest config is provided, stamp it on every visited node.
  - `symbol`: inherits parent file's `node_set`.
  - `doc_section`: inherits parent file's `node_set`.
  - `markdown`: inherits parent file's `node_set`.
- **Carry-forward:** unchanged files carry `node_set` unchanged (node_set is metadata; carry-forward copies the whole payload including node_set).
- **Filtering:** filtering by node_set/domain/facets happens **before** primary limit (not post-limit). No pagination in this plan (deferred). Ordering: `(score desc, id asc)` deterministic.
- **AND/OR:** default OR (`node_sets` filter matches if any filter tag present). Optional `node_set_mode: "all"` requires all filter tags present. Documented in tool schema.

### C2 — MCP domain registry CRUD (B2)

**Array ordering (deterministic):** `registry.domains` sorted by slug asc; each entry `aliases` sorted lexically asc and deduplicated. `metadata.node_set` sorted lexically asc and deduplicated. Wire `tags` preserved in input order (not sorted; ingest order). All list/set/error outputs preserve stated orders.
- Tool: `neurostrata_domain_registry` with `action: "list" | "set" | "remove"`.
  - `list`: returns `{ version, domains: [{slug, parent, aliases, concept_iri}] }`.
  - `set`: `{ slug, parent?, aliases?, concept_iri? }` — upserts entry; validates DAG + alias uniqueness; returns updated registry. If slug exists, merges (parent/aliases/concept_iri overwrite if provided).
  - `remove`: `{ slug }` — removes entry; if children reference it, error (caller must reparent first); if memories reference it via metadata.domain, warn but allow (domain becomes orphaned tag, not deleted from memories). Returns `{ removed: bool, orphaned_memories: count }`.
- **Input schema:** `{ action: enum, slug: string, parent: string|null (optional), aliases: string[] (optional), concept_iri: string|null (optional) }`.
- **Output schema:** `{ ok: bool, registry: {...}, warnings: string[] }`.

### C3 — per-namespace ontology cache (B3)
- `LadybugStore` holds `ontology: Arc<ArcSwap<BTreeMap<String, OntologyIndex>>>` (namespace-keyed).
- `refresh_ontology(namespace)`: reads namespace registry, builds OntologyIndex, inserts into map. Called before any read/write that needs ontology for that namespace.
- `get_ontology(namespace)`: returns cached or builds on demand (never cross-contaminates).
- **Invalid/missing/stale:** if registry read fails or ontology build fails, log warning, use empty OntologyIndex for that namespace (no canonicalization, no alias expansion). Never panic.
- **Atomicity:** namespace-keyed reads mean no cross-namespace leakage; no global lock needed. Contention test: interleaved A/B upserts+searches produce no alias leakage.

### C4 — raw topology schema (B4)
- On upsert, `canonicalize_topology` writes:
  - `metadata.topology.component` = canonical slug (or original if no ontology)
  - `metadata.topology.target_system` = canonical slug (or original)
  - `metadata.topology_original` = `{ "component": <raw>, "target_system": <raw> }` (only if different from canonical; omitted if identical)
- Idempotent: re-upsert produces identical metadata.
- **KnownIds disposition:** `canonicalize_known_ids_alias` remains unused by design (KnownIds is an ID resolver, not entity-alias store). Documented as retired from Plan 5 scope; entity-alias resolution lives in OntologyIndex. No code removal.
- **Plan 5→Plan 6 interface:** Plan 6 F10 consumes `metadata.topology.component`/`target_system` (canonical) for conflict matching. Plan 6 must not re-canonicalize.

### C5 — concept_iri mesh mapping (B5)
- **0 matches:** incoming concept_iri has no local domain with that IRI → keep concept_iri in metadata (alignment preserved), do not set domain. Log info "no local domain for concept_iri X".
- **1 match:** set metadata.domain to that local slug (if domain absent) or append to domain array (if multi-domain and slug not present). Preserve concept_iri.
- **>1 matches:** ambiguous → keep concept_iri, do not set domain, emit deterministic warning "ambiguous concept_iri X matches N local domains". Idempotent replay: no duplicate domains/warnings/state drift.
- **Replay:** re-importing same record is idempotent (domain not duplicated, no duplicate warnings).

### C6 — ontology grounding config (B7)
- **Config mapping:** per-namespace via registry row metadata key `ontology_config: { path: string, mode: "annotate"|"strict" }`. Falls back to env `ONTOLOGY_FILE_PATH` + env `ONTOLOGY_MODE` (default annotate) if registry has no config.
- **Precedence:** explicit metadata.concept_iri > registry v2 concept_iri > vocabulary concept_iri. Registry wins over vocabulary on conflict.
- **Missing/unreadable/malformed:** distinct warnings:
  - missing: `[neurostrata:ontology] no vocabulary configured for namespace X` (info, not warning)
  - unreadable: `[neurostrata:ontology] cannot read vocabulary at PATH: ERR` (warning)
  - malformed: `[neurostrata:ontology] invalid vocabulary JSON at PATH: ERR` (warning)
  - None of these flag rows or reject writes.
- **Stable warning records:** doctor and task_validate surface `ontology_warnings: [{ namespace, reason, path? }]` (sorted by namespace). Never in violations.

### C7 — stray EOF cleanup
- Remove any trailing `EOF`/`echo written` artifacts from plan files.

## Updated Acceptance Criteria (additions)
- [ ] AC7 (F9 node_set): per-node-kind derivation as C1; pre-limit filtering; AND/OR; carry-forward.
- [ ] AC8 (F15): exact MCP CRUD schemas (C2); removal/reference behavior; v2 durability; alias collisions (slug+alias+normalized); deterministic OntologyIndex (BTreeMap/BTreeSet); per-namespace scoping (C3); interleaved tests.
- [ ] AC9 (F16): facets idempotent/sorted/merged; search+export retain; filter pre-limit.
- [ ] AC10 (F17): raw preserved per C4; alias query expansion; namespace refresh; Plan 6 interface defined.
- [ ] AC11 (F20): concept_iri allowlisted; write-side stamp; import mapping per C5; RDF subject distinct; multi-domain IRI; privacy.
- [ ] AC12 (F21): per-namespace config per C6; annotate/strict; diagnostics in doctor+task_validate; deterministic; no LLM; no rejection.

## Cross-Plan Boundaries
- Plan 5 → Plan 6: F10 conflict detection consumes canonical slugs (F17/C4) and concept_iri. Plan 6 must not re-canonicalize.
- Plan 5 → Plan 4: F18 shape checks validate domain entries (registry) as part of shape validation.

## Test Fixtures (deterministic expected outputs)

### F9 node_set fixtures
- Fixture F9-1: ingest file `src/auth/login.rs` with `--node-set security` → row metadata.node_set = ["security"]; symbol `login` inherits ["security"]; doc_section inherits ["security"].
- Fixture F9-2: ingest same file without flag → node_set = [] for file/symbol/doc_section.
- Fixture F9-3: search with node_sets=["security"], limit=10 → returns only rows whose metadata.node_set contains "security"; deterministic order (score desc, id asc); filtering before limit.
- Fixture F9-4: AND mode node_set_mode="all" with node_sets=["security","auth"] → returns rows containing both.
- Fixture F9-5: re-ingest unchanged file → node_set unchanged (carry-forward).

### F15 MCP CRUD fixtures
- Fixture F15-1: set domain {slug:"auth", parent:null, aliases:["authentication"], concept_iri:"urn:ex:auth"} → registry v2 with one entry; list returns same.
- Fixture F15-2: set child {slug:"oauth", parent:"auth"} then set {slug:"auth", parent:"oauth"} → cycle rejected, registry unchanged.
- Fixture F15-3: set {slug:"auth", aliases:["auth"]} → alias collides with slug (case-folded + slug-normalized) → rejected.
- Fixture F15-4: set {slug:"auth", aliases:["Auth-Token"]} normalized → "auth-token" is a different slug → accepted; set {slug:"auth-token", ...} → alias collision detected → rejected.
- Fixture F15-5: remove "auth" when child "oauth" references it → error "reparent child first"; registry unchanged.
- Fixture F15-6: remove "auth" when 3 memories have metadata.domain="auth" → removed:true, orphaned_memories:3, memories unchanged.
- Fixture F15-7: write memory after registry v2 → registry v2 preserved (parent/aliases/concept_iri intact); project_root/enrichment_state/doctor_state intact.

### F16 facet fixtures
- Fixture F16-1: upsert rule with metadata.domain="auth" → metadata.facets = ["domain:auth","rule"] (sorted, dedup).
- Fixture F16-2: upsert with metadata.facets=["custom"] → merged → ["custom","domain:auth","rule"] (sorted).
- Fixture F16-3: re-upsert same → identical facets (idempotent).
- Fixture F16-4: search filters by facet; export_graph node retains facets.

### F17 canonical slug fixtures
- Fixture F17-1: upsert topology {component:"LadybugDB", target_system:"lbug"} with ontology alias "lbug"→"ladybugdb" → metadata.topology = {component:"ladybugdb", target_system:"ladybugdb"}, metadata.topology_original = {component:"LadybugDB", target_system:"lbug"}.
- Fixture F17-2: re-upsert identical → byte-identical metadata.
- Fixture F17-3: search query "LadybugDB" (alias) → finds row (alias expansion).
- Fixture F17-4: no ontology → topology unchanged, no topology_original.

### F20 concept_iri mesh fixtures
- Fixture F20-1: sender upsert domain "auth" with concept_iri "urn:ex:auth" → metadata.concept_iri stamped (if absent); mesh outbox wire record includes concept_iri.
- Fixture F20-2: receiver registry has domain "authentication" with same concept_iri → import maps domain to "authentication", concept_iri preserved.
- Fixture F20-3: receiver has 0 matching domains → concept_iri preserved, domain unset, info log.
- Fixture F20-4: receiver has 2 matching domains → concept_iri preserved, domain unset, warning "ambiguous concept_iri urn:ex:auth matches 2 local domains".
- Fixture F20-5: replay import → no duplicate domain, no duplicate warning.
- Fixture F20-6: two memories share concept_iri "urn:ex:auth" → distinct RDF subjects (urn:neurostrata:memory:{id1}, {id2}); both have ns:conceptIri urn:ex:auth.
- Fixture F20-7: mesh projected_metadata excludes domain, aliases, feedback_score, enrichment_state; includes concept_iri, eligibility, source, tags.

### F21 ontology grounding fixtures
- Fixture F21-1: annotate mode, vocabulary has concept {slug:"auth", concept_iri:"urn:ex:auth", aliases:["authentication"]}; upsert domain "Authentication" (alias) → concept_iri stamped "urn:ex:auth".
- Fixture F21-2: explicit metadata.concept_iri "urn:custom" present → preserved (precedence over vocabulary).
- Fixture F21-3: registry v2 entry for "auth" has concept_iri "urn:registry" → registry wins over vocabulary "urn:vocab".
- Fixture F21-4: strict mode, domain "unknown" not in vocabulary → warning emitted, row+edge persisted, no rejection.
- Fixture F21-5: missing ONTOLOGY_FILE_PATH → info "[neurostrata:ontology] no vocabulary configured for namespace X"; no rows flagged.
- Fixture F21-6: unreadable path → warning "[neurostrata:ontology] cannot read vocabulary at PATH: ERR".
- Fixture F21-7: malformed JSON → warning "[neurostrata:ontology] invalid vocabulary JSON at PATH: ERR".
- Fixture F21-8: doctor surfaces ontology_warnings: [{namespace:"ns", reason:"out_of_vocabulary", path:null}] sorted by namespace; never in violations.
- Fixture F21-9: repeated upsert → deterministic output (no duplicate warnings).

### F10 Plan 6 interface fixture (Plan 5→6)
- Fixture IF-1: Plan 6 consumes metadata.topology.component/target_system (canonical) and metadata.concept_iri for conflict matching. Plan 6 must not re-canonicalize or re-resolve aliases.

## Detailed Test Fixtures (v4 — exact inputs/outputs)

### F9 node_set fixed corpus
Corpus (5 rows, primary limit = 3, deterministic order score desc then id asc):
- R1 id=r-001 score=0.9 node_set=["security"] domain="auth"
- R2 id=r-002 score=0.8 node_set=["auth"] domain="auth"
- R3 id=r-003 score=0.7 node_set=["security","auth"] domain="core"
- R4 id=r-004 score=0.6 node_set=["core"] domain="core"
- R5 id=r-005 score=0.5 node_set=[] domain="misc"

- Fixture F9-6 (file node_set): ingest file `src/x.rs` with `--node-set security` → row kind=file node_set=["security"]; symbol `foo` node_set=["security"]; doc_section `# intro` node_set=["security"]; directory `src/` node_set=["security"].
- Fixture F9-7 (default): ingest `src/y.rs` no flag → file/symbol/doc_section/directory node_set=[].
- Fixture F9-8 (pre-limit + beyond page): search node_sets=["security"], limit=3 → returns [r-001, r-003] (only 2 match; R4/R5 filtered before limit; no false empty). Exact output: `{"results":[{"id":"r-001","score":0.9},{"id":"r-003","score":0.7}], "total":2, "limit":3, "offset":0}`.
- Fixture F9-9 (AND mode): search node_sets=["security","auth"], node_set_mode="all", limit=3 → [r-003] only. Exact output ids: ["r-003"].
- Fixture F9-10 (OR mode default): search node_sets=["security","auth"], limit=3 → [r-001, r-002, r-003].
- Fixture F9-11 (carry-forward): re-ingest unchanged file → node_set byte-identical.

### F15 MCP exact JSON Schemas
Tool: `neurostrata_domain_registry`. Common envelope (all actions):
- Success: `{ "ok": true, "registry": { "version": 1|2, "domains": [ { "slug": string, "parent": string|null, "aliases": string[], "concept_iri": string|null } ] }, "warnings": string[] }`
- Error: `{ "ok": false, "error": { "code": string, "message": string }, "registry": {...unchanged...}, "warnings": string[] }`

Action `list`:
- Input: `{ "action": "list" }` (slug/parent/aliases/concept_iri all optional/ignored).
- Output: success envelope with full registry.

Action `set`:
- Input: `{ "action": "set", "slug": string (required), "parent": string|null (optional), "aliases": string[] (optional, default []), "concept_iri": string|null (optional) }`.
- Validation errors (error codes): `INVALID_SLUG` (empty/non-normalizable), `CYCLE_DETECTED`, `ALIAS_COLLISION` (slug/alias/normalized), `PARENT_NOT_FOUND` (parent slug not in registry; error, caller must create parent first).
- Success: upserts; merges if slug exists (provided fields overwrite, omitted fields preserved).

Action `remove`:
- Input: `{ "action": "remove", "slug": string (required) }`.
- Success: `{ ..., "removed": true, "orphaned_memories": number }`.
- Error codes: `HAS_CHILDREN` (slug has child domains; must reparent first), `NOT_FOUND` (slug absent).
- On HAS_CHILDREN: registry unchanged, error envelope.

Fixtures:
- Fixture F15-8 (list): input `{"action":"list"}` → exact registry JSON as stored.
- Fixture F15-9 (set success): input `{"action":"set","slug":"auth","parent":null,"aliases":["authentication"],"concept_iri":"urn:ex:auth"}` → success envelope, registry contains entry, warnings=[].
- Fixture F15-10 (set merge): set same slug with `{"aliases":["login"]}` (omit parent/concept_iri) → merge semantics: provided fields overwrite, omitted fields preserved. Result: aliases=["login"], parent/concept_iri preserved from prior.
- Fixture F15-11 (cycle): error envelope code=CYCLE_DETECTED, registry unchanged.
- Fixture F15-12 (alias collision): error envelope code=ALIAS_COLLISION.
- Fixture F15-13 (remove with children): error envelope code=HAS_CHILDREN.
- Fixture F15-14 (remove with memory refs): success removed=true, orphaned_memories=3, warnings=[] (memories unchanged).

### F15 cache fixtures (C3)
- Fixture F15-15 (cold miss): namespace "ns-a" first upsert → refresh_ontology builds index; subsequent search uses cached index; no cross-namespace leakage to "ns-b".
- Fixture F15-16 (stale after registry change): set domain in ns-a → refresh_ontology called → next search sees new alias. Exact: search "authentication" (alias) finds rows after set.
- Fixture F15-17 (read failure): registry read fails (simulated) → empty OntologyIndex for ns-a, warning logged, no panic, canonicalization skipped (topology preserved raw).
- Fixture F15-18 (contention): interleaved ns-a upsert + ns-b search → ns-b search uses ns-b's cached index, no alias leakage. Exact: ns-b search for ns-a-only alias returns empty.

### F17 raw topology + KnownIds (C4)
- Fixture F17-5 (topology_original survives re-upsert): if metadata already has topology_original and current topology already equals canonical, re-upsert preserves topology_original unchanged (does not overwrite/reset). Exact: byte-identical metadata after second upsert.
- Fixture F17-6 (KnownIds disposition): canonicalize_known_ids_alias remains unused; KnownIds is ID resolver only. No alias resolution in KnownIds. Documented retirement (no code removal).
- Fixture IF-1 (Plan 6 handoff): captured input to Plan 6 = `{ "topology": { "component": "ladybugdb", "target_system": "ladybugdb" }, "topology_original": { "component": "LadybugDB", "target_system": "lbug" }, "concept_iri": "urn:ex:db" }`. Plan 6 F10 assertion: conflict matching uses topology.component/target_system (canonical) and concept_iri; does NOT re-canonicalize or re-resolve aliases. Test: feed captured input, verify Plan 6 reads canonical values verbatim.

### F20 replay + multi-IRI (C5)
- Fixture F20-8 (replay, first import): first import of record with concept_iri "urn:ex:auth", receiver has 1 matching domain "authentication" → domain set to "authentication", concept_iri preserved, warnings=[]. Metadata after: `{domain:"authentication", concept_iri:"urn:ex:auth"}`.
- Fixture F20-9 (replay, second import): re-import same record → domain NOT duplicated (still "authentication", not ["authentication","authentication"]), concept_iri preserved, warnings=[] (no new warning). Exact: metadata identical to F20-8, warnings=[].
- Fixture F20-10 (ambiguous replay): first import ambiguous (2 matches) → warnings=["ambiguous concept_iri urn:ex:auth matches 2 local domains"], domain unset. Replay must be idempotent: warnings=[] on replay (warning already emitted once; replay does not re-emit). Warnings emitted only on state change.
- Fixture F20-11 (multi-IRI warning ordering): if multiple ambiguous IRIs in one import, warnings sorted by concept_iri asc. Dedup: identical (concept_iri, match_count) warning not re-emitted.

### F21 ontology grounding (C6)
Config precedence (full order):
1. metadata.ontology_config (per-namespace registry row) if present.
2. env ONTOLOGY_FILE_PATH + env ONTOLOGY_MODE (default annotate).
3. none → missing case (info, not warning).

Partial registry config fallback:
- If registry has ontology_config with only `path` (no mode) → mode defaults to "annotate".
- If registry has only `mode` (no path) → fall back to env path; if env also absent → missing case.

Stable reason enum for ontology_warnings records:
- `no_vocabulary` (info-level, missing config)
- `unreadable` (warning)
- `malformed` (warning)
- `out_of_vocabulary` (warning, strict mode only)

Warning record schema: `{ "namespace": string, "reason": enum, "path": string|null }`.

Sort order: (namespace asc, reason asc, path asc with nulls last).

Fixtures:
- Fixture F21-10 (doctor): namespace "ns-a" strict mode, domain "unknown" → doctor ontology_warnings = `[{"namespace":"ns-a","reason":"out_of_vocabulary","path":"/vocab.json"}]`.
- Fixture F21-11 (task_validate): same scenario → task_validate ontology_warnings identical array (same schema, same sort). Never in violations.
- Fixture F21-12 (multi-namespace sort): ns-b no_vocabulary, ns-a unreadable → sorted: ns-a/unreadable first, then ns-b/no_vocabulary.
- Fixture F21-13 (partial config): registry has `{path:"/v.json"}` only → mode defaults annotate; no warning for missing mode.
- Fixture F21-14 (path placeholder): unreadable → reason="unreadable", path="/bad/path.json", message includes OS error but warning record uses stable reason (not OS string). Doctor/task_validate records stable reason only.

### F16 facet sort (correction)
Expected arrays use BTreeSet lexical ordering:
- Fixture F16-1: ["domain:auth","rule"] (domain:auth < rule because 'd' < 'r').
- Fixture F16-2: ["custom","domain:auth","rule"].

## JSON Schemas (draft 2020-12)

### neurostrata_domain_registry input schema
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "DomainRegistryInput",
  "type": "object",
  "required": ["action"],
  "additionalProperties": false,
  "properties": {
    "action": { "enum": ["list", "set", "remove"] },
    "slug": { "type": "string", "minLength": 1 },
    "parent": { "type": ["string", "null"] },
    "aliases": { "type": "array", "items": { "type": "string" }, "uniqueItems": true },
    "concept_iri": { "type": ["string", "null"] }
  },
  "oneOf": [
    { "properties": { "action": { "const": "list" } } },
    { "properties": { "action": { "const": "set" } }, "required": ["slug"] },
    { "properties": { "action": { "const": "remove" } }, "required": ["slug"] }
  ]
}
```

### neurostrata_domain_registry success envelope
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "DomainRegistrySuccess",
  "type": "object",
  "required": ["ok", "registry", "warnings"],
  "additionalProperties": false,
  "properties": {
    "ok": { "const": true },
    "registry": {
      "type": "object",
      "required": ["version", "domains"],
      "additionalProperties": false,
      "properties": {
        "version": { "enum": [1, 2] },
        "domains": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["slug", "parent", "aliases", "concept_iri"],
            "additionalProperties": false,
            "properties": {
              "slug": { "type": "string" },
              "parent": { "type": ["string", "null"] },
              "aliases": { "type": "array", "items": { "type": "string" } },
              "concept_iri": { "type": ["string", "null"] }
            }
          }
        }
      }
    },
    "warnings": { "type": "array", "items": { "type": "string" } },
    "removed": { "type": "boolean" },
    "orphaned_memories": { "type": "integer", "minimum": 0 }
  }
}
```

### neurostrata_domain_registry error envelope
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "DomainRegistryError",
  "$defs": {
    "registry": {
      "type": "object",
      "required": ["version", "domains"],
      "additionalProperties": false,
      "properties": {
        "version": { "enum": [1, 2] },
        "domains": { "type": "array", "items": { "$ref": "#/$defs/domain" } }
      }
    },
    "domain": {
      "type": "object",
      "required": ["slug", "parent", "aliases", "concept_iri"],
      "additionalProperties": false,
      "properties": {
        "slug": { "type": "string" },
        "parent": { "type": ["string", "null"] },
        "aliases": { "type": "array", "items": { "type": "string" } },
        "concept_iri": { "type": ["string", "null"] }
      }
    }
  },
  "type": "object",
  "required": ["ok", "error", "registry", "warnings"],
  "additionalProperties": false,
  "properties": {
    "ok": { "const": false },
    "error": {
      "type": "object",
      "required": ["code", "message"],
      "additionalProperties": false,
      "properties": {
        "code": { "enum": ["INVALID_SLUG", "CYCLE_DETECTED", "ALIAS_COLLISION", "PARENT_NOT_FOUND", "HAS_CHILDREN", "NOT_FOUND"] },
        "message": { "type": "string" }
      }
    },
    "registry": { "$ref": "#/$defs/registry" },
    "warnings": { "type": "array", "items": { "type": "string" } }
  }
}
```

### ontology_warnings record schema
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "OntologyWarning",
  "type": "object",
  "required": ["namespace", "reason", "path"],
  "additionalProperties": false,
  "properties": {
    "namespace": { "type": "string" },
    "reason": { "enum": ["no_vocabulary", "unreadable", "malformed", "out_of_vocabulary"] },
    "path": { "type": ["string", "null"] }
  }
}
```

### Complete literal I/O fixtures

#### F15-8 list
Input: `{"action":"list"}`
Expected output:
```json
{"ok":true,"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":["authentication"],"concept_iri":"urn:ex:auth"}]},"warnings":[]}
```

#### F15-9 set success
Input: `{"action":"set","slug":"auth","parent":null,"aliases":["authentication"],"concept_iri":"urn:ex:auth"}`
Expected output:
```json
{"ok":true,"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":["authentication"],"concept_iri":"urn:ex:auth"}]},"warnings":[]}
```

#### F15-10 set merge
Prior registry: auth entry with parent=null, aliases=["authentication"], concept_iri="urn:ex:auth".
Input: `{"action":"set","slug":"auth","aliases":["login"]}`
Expected output:
```json
{"ok":true,"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":["login"],"concept_iri":"urn:ex:auth"}]},"warnings":[]}
```

#### F15-11 cycle
Input: set oauth parent=auth, then set auth parent=oauth.
Expected output:
```json
{"ok":false,"error":{"code":"CYCLE_DETECTED","message":"domain cycle: auth -> oauth -> auth"},"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":[],"concept_iri":null},{"slug":"oauth","parent":"auth","aliases":[],"concept_iri":null}]},"warnings":[]}
```

#### F15-12 alias collision
Input: set auth aliases=["auth"].
Expected output:
```json
{"ok":false,"error":{"code":"ALIAS_COLLISION","message":"alias 'auth' collides with slug 'auth'"},"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":[],"concept_iri":null}]},"warnings":[]}
```

#### F15-13 remove with children
Input: remove "auth" while "oauth" has parent "auth".
Expected output:
```json
{"ok":false,"error":{"code":"HAS_CHILDREN","message":"cannot remove 'auth': child domain 'oauth' references it"},"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":[],"concept_iri":null},{"slug":"oauth","parent":"auth","aliases":[],"concept_iri":null}]},"warnings":[]}
```

#### F15-14 remove with memory refs
Input: remove "auth" with 3 memories referencing it.
Expected output:
```json
{"ok":true,"registry":{"version":2,"domains":[]},"warnings":[],"removed":true,"orphaned_memories":3}
```

#### F15-15 cold miss (ns-a)
Call sequence: search ns-a for alias "authentication" (no registry yet) → empty index → build from registry (empty) → no canonicalization → search returns [].
Expected: `{"results":[],"warnings":[]}`; registry read attempted once.

#### F15-16 stale after registry change
Call sequence: set domain auth aliases=["authentication"] in ns-a; search "authentication" → returns rows whose domain canonicalizes to auth.
Expected: rows with domain="auth" returned; warning=[].

#### F15-17 read failure
Simulated: registry read returns error.
Expected: empty OntologyIndex for ns-a; warning logged `[neurostrata:ontology] registry read failed for ns-a: <err>`; upsert preserves raw topology; search no alias expansion.

#### F15-18 contention
Interleaved: ns-a upsert auth alias "authentication"; ns-b search for "authentication" (ns-a-only alias).
Expected: ns-b returns [] (no leakage); ns-a search returns rows. Warning=[].

#### F17-5 topology_original survives
First upsert input topology: {component:"LadybugDB", target_system:"lbug"} with ontology alias lbug→ladybugdb.
Expected metadata after first: `{"topology":{"component":"ladybugdb","target_system":"ladybugdb"},"topology_original":{"component":"LadybugDB","target_system":"lbug"}}`.
Second upsert same input → byte-identical metadata (topology_original unchanged).

#### F20-8 first import (1 match)
Sender wire record:
```json
{"id":"mem-1","content":"...","metadata":{"concept_iri":"urn:ex:auth","eligibility":"shareable","source":"ingest","tags":["auth"]}}
```
Receiver registry: domain "authentication" with concept_iri "urn:ex:auth".
Expected receiver metadata after import: `{"domain":"authentication","concept_iri":"urn:ex:auth"}`. warnings=[].

#### F20-9 replay
Re-import same wire record.
Expected: metadata identical to F20-8; warnings=[] (no duplicate).

#### F20-10 ambiguous replay
Receiver registry: domains "auth-a" and "auth-b" both concept_iri "urn:ex:auth".
First import expected: metadata `{"concept_iri":"urn:ex:auth"}` (domain unset), warnings=["ambiguous concept_iri urn:ex:auth matches 2 local domains"].
Replay expected: metadata identical, warnings=[] (no re-emit).

#### F20-11 multi-IRI warning order
Import with two ambiguous IRIs "urn:ex:z" (3 matches) and "urn:ex:a" (2 matches).
Expected warnings sorted by concept_iri asc: ["ambiguous concept_iri urn:ex:a matches 2 local domains", "ambiguous concept_iri urn:ex:z matches 3 local domains"].

#### F21-10 doctor
Namespace "ns-a" strict mode, domain "unknown", vocabulary has no "unknown".
Expected doctor ontology_warnings:
```json
[{"namespace":"ns-a","reason":"out_of_vocabulary","path":"/vocab.json"}]
```

#### F21-11 task_validate
Same scenario.
Expected task_validate ontology_warnings: identical array. Never in violations.

#### F21-12 multi-namespace sort + tie-break
Inputs: ns-a strict mode with domain "unknown" (vocabulary lacks it) at path "/vocab.json"; ns-a with unreadable path "/bad.json"; ns-b with no vocabulary configured.
Expected sorted by (namespace, reason, path nulls last):
```json
[{"namespace":"ns-a","reason":"out_of_vocabulary","path":"/vocab.json"},{"namespace":"ns-a","reason":"unreadable","path":"/bad.json"},{"namespace":"ns-b","reason":"no_vocabulary","path":null}]
```
Reason sort: "out_of_vocabulary" < "unreadable" ('o' < 'u').

#### F21-13 partial config
Registry ontology_config: {"path":"/v.json"} (no mode).
Expected: mode defaults to "annotate"; no warning for missing mode; vocabulary loaded from /v.json.

#### F21-14 unreadable stable reason
Path "/bad/path.json" unreadable.
Expected warning record: `{"namespace":"ns-a","reason":"unreadable","path":"/bad/path.json"}` (stable reason, not OS error string). Message (separate) may include OS error.

### F9 literal metadata fixtures
#### F9-6 file node_set (with flag)
Ingest input: file src/x.rs, flag --node-set security.
Expected row metadata: `{"node_set":["security"]}` for kind=file, symbol, doc_section, directory.

#### F9-7 default
Ingest input: file src/y.rs, no flag.
Expected row metadata: `{"node_set":[]}` for all kinds.

#### F9-11 carry-forward
Re-ingest unchanged file → node_set byte-identical (part of carried payload).

### F20 array ordering contracts
- metadata.domain (multi): sorted ascending, dedup. Example: domains ["auth","core"] (not ["core","auth"]).
- metadata.concept_iri (multi): sorted ascending, dedup. Example: ["urn:ex:a","urn:ex:z"].

### F20 wire contract (complete curated record)
```json
{
  "id": "mem-1",
  "content": "rule: auth required",
  "metadata": {
    "concept_iri": "urn:ex:auth",
    "eligibility": "shareable",
    "source": "ingest",
    "tags": ["auth"]
  },
  "embedding": null
}
```
Receiver with 1 matching domain "authentication" → metadata.domain becomes "authentication" (string, not array), concept_iri preserved. Array ordering: N/A (single).

Receiver with 0 matches → metadata.domain unset (absent), concept_iri preserved.
Receiver with 2+ matches → metadata.domain absent, concept_iri preserved, warning emitted (idempotent on replay).

#### F15-19 multi-entry ordering
Prior registry: domains oauth (aliases ["oauth2","login"]), auth (aliases ["authentication"]).
Expected list output (domains slug-sorted, aliases lexically sorted):
```json
{"ok":true,"registry":{"version":2,"domains":[{"slug":"auth","parent":null,"aliases":["authentication"],"concept_iri":null},{"slug":"oauth","parent":"auth","aliases":["login","oauth2"],"concept_iri":null}]},"warnings":[]}
```

#### F20-12 array ordering literal
Import multi-domain concept_iri ["urn:ex:z","urn:ex:a"] with receiver domains auth-a and auth-z (both match one IRI each).
Expected metadata.domain (sorted asc): ["auth-a","auth-z"].
Expected metadata.concept_iri (sorted asc): ["urn:ex:a","urn:ex:z"].
