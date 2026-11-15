# Subject-Matter Domain Expertise, SKOS Concept Taxonomy & Epistemic Briefing Engine (`neurostrata-f0bn`)

**Date:** 2026-11-15  
**Status:** Revised — Incorporates K3 Architectural Review (`ses_e22966828ffeczipLkHxeGkZCo`)  
**Branch:** `feat/kinetic-actions`  
**Task ID:** `neurostrata-f0bn`

---

## 0. Foundational Invariant: Strictly Additive Architecture & Release Blocker Resolution

This specification is **100% additive**. Every existing NeuroStrata capability, workflow gate, and subsystem is preserved in full without modification to its existing contracts:

1. **Tri-Strata Memory & Non-Destructive Lifecycle:** `global`, project namespace, and task working memory (`rule`, `fact`, `procedure`, `task`, `action_type`, `action_receipt`, `directory`, `file`, `markdown`, `doc_section`, `symbol`/`code_ast`), `neurostrata_supersede_memory`, `neurostrata_archive_memory`, and `neurostrata_append_log` remain unchanged.
2. **Central Executive & The 3 Soft Locks:** Zero-Action Start, Lock 1 (`pre-push` strict task gate), Lock 2 (`neurostrata_task_complete` mandatory memory extraction), Lock 3 (Breath working-memory note every 3–5 mutations), and `pre-commit` Graph-First query ledger remain strictly enforced.
3. **Code & Document Intelligence (`neurostrata-mtqx`):** AST & SCIP ingestion (`neurostrata_ingest_directory`, `neurostrata_ingest_scip`), Hybrid RRF search with `CodeTokenizer` (`neurostrata_search_memory`), compound code/blast-radius locator (`neurostrata_locate`), 4-stop guided diff tour (`neurostrata_diff_tour`), and longitudinal bitemporal code/governance insights (`neurostrata_insights`) remain intact.
4. **Enterprise Ontology & Operations (`neurostrata-1mus`):** Kinetic Action Types & Bitemporal Revert Log (`neurostrata_action`), Polymorphic Digital Twins & Deterministic Link Reducers (`neurostrata_interface_define`, `neurostrata_interface_query`, `neurostrata_link_reduce`), Counterfactual Copy-on-Write Scenarios (`neurostrata_scenario`), and Unified 360° Object View (`neurostrata_object_view`) remain intact.
5. **SynapticMesh & Security:** Shareable vs. private sovereignty, ReBAC principal enforcement, and `check_caller_path` daemon path authority remain intact.

### 0.1 Pre-Release Blocker Fix Included (`src/rdf/scenario.rs` Transactional CAS)

Per K3 architectural review (`ses_e22966828ffeczipLkHxeGkZCo`), `commit_scenario` in `src/rdf/scenario.rs` previously prechecked `base_fingerprints` before calling `commit_action_batch` with `expected_heads: Vec::new()`. We close this TOCTOU window by:
- Adding `ExpectedHead::RowFingerprint { row_id: String, expected_fingerprint: u64 }` to `src/rdf/action.rs` and verifying live row fingerprints inside the single-writer transaction in `LadybugStore::commit_action_batch` (`src/store/ladybug.rs`).
- Passing all `scenario.base_fingerprints` as `ExpectedHead::RowFingerprint` entries in `commit_scenario`, converting `ActionError::ConflictWithNewerVersion` into `ScenarioCommitOutcome::Rejected` (`rejected_base_conflict`) with full rollback and scenario preservation.

---

## 1. Problem Statement & Motivation

Previously, NeuroStrata optimized heavily for *operational execution* (how an agent tracks tasks, respects project rules, and navigates files/symbols/twins) while leaving *subject-matter expertise* implicit. As a result:

- Agents knew *project rules* and *file structure*, but lacked a structured **Project Thesis** (what domain the project operates in, its strategic direction, core pillars, tangential domains, and evaluation axes when comparing alternative paradigms or architectures).
- Agents lacked a higher-order **Subject-Matter Taxonomy & Concept Graph** (`skos:Concept` hierarchy of broader/narrower concepts, tangential/related concepts, contrasting approaches, canonical definitions, trade-offs, evaluation axis ratings, comparable implementations, and foundational references).
- When asked to evaluate a domain question, compare candidate approaches, or advise the owner as a subject-matter partner (e.g., *"What are the top 5 ways we could improve this project?"*), agents had no pre-built domain taxonomy, multi-perspective comparative matrix, or ontology-grounded frontier advisor to reason from.

---

## 2. Higher-Order Knowledge Model (Vocabulary v11)

`src/schemas/memory-vocabulary.v3.json` is incremented from `vocabulary_version: 10` to `vocabulary_version: 11` (backwards-compatible with all v10 rows and tests) by adding two normative, non-structural memory types: `thesis` and `concept`.

### 2.1 Project Thesis (`memory_type: "thesis"`)

Each project namespace maintains at most one active `thesis` row (enforced atomically under the single-writer lock via `commit_action_batch` CAS; updates bitemporally supersede the prior active `thesis` row in that namespace, preserving full lineage).

- **`content`**: Concise executive statement of the project's purpose, subject-matter domain, and strategic direction.
- **`metadata.thesis`** (typed object):
  - `primary_domain` (`string`, required): Normalized domain slug or human label of the core discipline (e.g., `"aesthetic-medicine"`, `"agentic-cognitive-memory-and-ontology"`).
  - `mission` (`string`, required): What the project exists to achieve.
  - `direction` (`string`, required): Strategic trajectory and architectural/product North Star.
  - `core_pillars` (`array<string>`, required, non-empty): Top-level subject-matter domain slugs (`^[a-z0-9]+(-[a-z0-9]+)*$`) that form the roots of the project's taxonomy.
  - `adjacent_domains` (`array<string>`, optional): Tangentially related fields and disciplines agents should understand when researching or synthesizing ideas.
  - `evaluation_axes` (`array<string>`, optional): Normalized axis slugs used to compare external approaches, literature, or designs against this project's philosophy.
  - `non_goals` (`array<string>`, optional): Explicit boundaries of what the project does not pursue.
- **`metadata.eligibility`**: Defaults to `"private"` (safe-by-default). May be explicitly set to `"shareable"` when `access_policy` is absent; `SHAREABLE_MEMORY_TYPES` includes `"thesis"` and `"concept"` so explicitly shareable domain knowledge syncs across `SynapticMesh` while private or access-controlled rows never leave the local instance.

### 2.2 Subject-Matter Concept (`memory_type: "concept"`)

Represents a higher-order unit of domain knowledge in the project's W3C SKOS taxonomy and knowledge graph.

- **`content`**: Canonical explanation and synthesis of the concept in the context of the project's domain.
- **`metadata.concept`** (typed object):
  - `slug` (`string`, required): Normalized lowercase-kebab identifier (`^[a-z0-9]+(-[a-z0-9]+)*$`), unique among active `concept` rows in the namespace (enforced atomically under CAS).
  - `pref_label` (`string`, required): Canonical human-readable name (`skos:prefLabel`).
  - `aliases` (`array<string>`, optional): Synonyms, acronyms, or alternate terminology (`skos:altLabel`).
  - `definition` (`string`, required): Concise formal definition (`skos:definition`).
  - `broader` (`array<string>`, optional): Canonical parent concept slugs (`skos:broader`). Empty when the concept is a top-level root pillar under its `domain`. **Single canonical hierarchy representation:** `broader` is the sole stored hierarchy edge; `narrower` is strictly derived at read/index time as the inverse of `broader` across active visible `concept` rows, preventing dual-array split-brain.
  - `related_concepts` (`array<string>`, optional): Tangential, lateral, or cross-domain concept slugs (`skos:related`). Symmetrized at read/index time. Must be disjoint from the transitive closure of `broader` (`skos:broaderTransitive`) per W3C SKOS S27.
  - `contrasts_with` (`array<string>`, optional): Competing paradigms, alternative algorithms, or differential concept slugs (`ns:contrastsWith`). Symmetrized at read/index time. Must be disjoint from `skos:broaderTransitive` per NeuroStrata ontology integrity rules.
  - `tradeoffs` (`array<string>`, optional): Key strengths, failure modes, and decision boundaries (when to use vs. when to avoid).
  - `references` (`array<string>`, optional): Foundational papers, clinical protocols, standards, or benchmarks (`dcterms:references`).
  - `comparable_implementations` (`array<ComparableImplementation>`, optional): Structured provenance for comparable libraries or exemplar systems:
    - `name` (`string`, required): Library, crate, or system identifier.
    - `kind` (`"library" | "standard" | "reference_system"`, required).
    - `license` (`string`, required): SPDX license identifier (e.g., `"MIT"`, `"Apache-2.0"`, `"proprietary-reference"`), enforcing Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`.
    - `reference` (`string`, required): URL, crate name, or citation.
    - `notes` (`string`, optional): Architectural comparison notes.
  - `realization_status` (`"unknown" | "unimplemented" | "partial" | "implemented" | "external_only"`, optional, defaults to `"unknown"`): Explicit project realization state. Anchoring to a file or rule never implicitly marks a concept `"implemented"`.
  - `current_limitations` (`array<string>`, optional): Known architectural, theoretical, or operational ceilings in the project's current realization of the concept.
  - `frontier_opportunities` (`array<string>`, optional): Concrete, SME-grade advancement vectors grounded in domain research, libraries, and the project's canonical ontology.
  - `implemented_interfaces` (`array<string>`, optional): Polymorphic `interface_def` names associated with this domain concept (validated against active `interface_def` rows in the namespace).
  - `linked_action_types` (`array<string>`, optional): Kinetic `action_type` slugs governed or informed by this domain concept (validated against active `action_type` rows in the namespace).
  - `evaluation_scores` (`object<string, integer>`, optional): Basis-point ratings (`0..10000`) keyed by `thesis.evaluation_axes` slugs.
  - `evidence_refs` (`array<string>`, optional): Concrete verification evidence (active `rule`/`fact` memory IDs in the namespace or verified citation URIs/DOIs) required to attain `EpistemicStatus::Verified`.
- **Read-Time Derived Properties (never stale):**
  - `narrower`: Inverse of `broader` across active visible `concept` rows (sorted lexicographically).
  - `epistemic_status` (`"seed" | "defined" | "synthesized" | "verified"`): Computed dynamically at read time via `formal_kernel::classify_epistemic_status` from current live definition, taxonomy links, synthesis fields, and verified active `evidence_refs` / governing `rule`/`fact` rows.
  - `epistemic_weight_bps`: Formal kernel basis-point weight (`2500 | 5000 | 8000 | 10000`).
  - `descendant_count`: Transitive count of active visible `narrower` concepts in the taxonomy DAG.
  - `grounding_count`: Count of active visible governing `rule`, `fact`, `procedure`, `interface_def`, `action_type`, and code/document anchor connections.
  - `frontier_leverage_bps`: Formal kernel priority score (`0..10000`) ranking how impactful advancing this concept is for the project.

---

## 3. Formal Verification Kernel (`src/formal_kernel.rs` & `proofs/NeuroStrata/Taxonomy.lean`)

Per the Formal Verification Guard (`b0813917-a806-4faa-8444-f7acec555ed9`), all pure decision logic for taxonomy integrity, SKOS S27 transitive disjointness, epistemic maturity, and frontier leverage is factored into `src/formal_kernel.rs`, proven in `proofs/NeuroStrata/Taxonomy.lean` with zero `sorry`/`axiom`, and verified by parity tests in `tests/lean_parity_test.rs`.

### 3.1 Epistemic Maturity Classification

Given booleans derived at read time for a concept node:
- `has_definition: bool` (`!definition.trim().is_empty()`)
- `has_taxonomy_link: bool` (`!broader.is_empty() || !derived_narrower.is_empty() || !related_concepts.is_empty() || is_core_pillar_root`)
- `has_synthesis: bool` (`!tradeoffs.is_empty() && (!contrasts_with.is_empty() || !references.is_empty() || !comparable_implementations.is_empty())`)
- `has_empirical_grounding: bool` (`has_synthesis && verified_active_evidence_count > 0`) — where `verified_active_evidence_count` counts `evidence_refs` and `governs` targets that resolve to currently active `rule`/`fact` rows in the namespace (or non-empty `evidence_refs` citations alongside active grounding).

The pure function `classify_epistemic_status(has_definition, has_taxonomy_link, has_synthesis, has_empirical_grounding) -> EpistemicStatus` returns:
1. `EpistemicStatus::Seed` (weight `2500` bps) — missing definition or taxonomic placement.
2. `EpistemicStatus::Defined` (weight `5000` bps) — has canonical definition and taxonomic link.
3. `EpistemicStatus::Synthesized` (weight `8000` bps) — `Defined` plus explicit `tradeoffs` and (`contrasts_with`, `references`, or `comparable_implementations`).
4. `EpistemicStatus::Verified` (weight `10000` bps) — `Synthesized` plus verified empirical grounding in active project rules/facts.

### 3.2 Taxonomy DAG, Transitive SKOS S27 Disjointness & Scoring Kernel

1. **`validate_taxonomy_dag(broader_edges: &[(u32, u32)]) -> bool`:**
   - Pure cycle-detection kernel over `broader` (`child -> parent`) edges. Rejects any self-loop `(u, u)` or directed cycle of length $\ge 2$.
2. **`validate_skos_disjointness(broader_edges: &[(u32, u32)], related_edges: &[(u32, u32)], contrast_edges: &[(u32, u32)]) -> bool`:**
   - Computes the **transitive closure** `broader_transitive` over `broader_edges`.
   - Enforces W3C SKOS S27 (`skos:related` is disjoint from `skos:broaderTransitive` in both directions `(u, v)` and `(v, u)`) AND NeuroStrata contrast disjointness (`ns:contrastsWith` is disjoint from `skos:broaderTransitive` in both directions).
3. **`compute_epistemic_coverage_bps(statuses: &[EpistemicStatus], unmapped_domain_count: u32) -> u32`:**
   - If `statuses.is_empty()` and `unmapped_domain_count == 0`, returns `0`.
   - Otherwise, let `sum_weights: u64 = statuses.iter().map(|s| s.weight_bps() as u64).sum()`, and `denom: u64 = statuses.len() as u64 + unmapped_domain_count as u64` (treating each unmapped domain as a `0`-weight slot). Returns `((sum_weights / denom) as u32).min(10000)`.
4. **`compute_frontier_leverage_bps(axis_alignment_bps: u32, realization_gap_bps: u32, canonical_connectivity_count: u32, has_frontier_or_limitation: bool, has_references_or_comparables: bool) -> u32`:**
   - Exact integer formula:
     - `realization_gap_bps` mapping: `"unimplemented" => 10000`, `"partial" => 7500`, `"unknown" => 6000`, `"implemented" => 2500`, `"external_only" => 0`.
     - Gate: If `realization_gap_bps == 0` (`external_only`) OR (`realization_gap_bps <= 2500` && `!has_frontier_or_limitation`), returns `0` (concept is either external-only or already implemented without recorded limitations/frontier opportunities).
     - Clamp `align = axis_alignment_bps.min(10000) as u64` (default `5000` when no axes/scores exist).
     - Clamp `gap = realization_gap_bps.min(10000) as u64`.
     - `conn_bps = (canonical_connectivity_count.min(10) as u64) * 1000`.
     - `frontier_bps = if has_frontier_or_limitation { 10000u64 } else { 0u64 }`.
     - `cite_bps = if has_references_or_comparables { 10000u64 } else { 0u64 }`.
     - `raw = (35 * align + 25 * gap + 20 * conn_bps + 10 * frontier_bps + 10 * cite_bps) / 100`.
     - Returns `(raw as u32).min(10000)`.

### 3.3 Lean 4 Theorems (`proofs/NeuroStrata/Taxonomy.lean`)

1. `classify_epistemic_monotonic`: Adding definition, taxonomy links, synthesis, or grounding never decreases `EpistemicStatus`.
2. `epistemic_status_weight_bounded`: Every status weight, `compute_epistemic_coverage_bps`, and `compute_frontier_leverage_bps` output is strictly bounded within `[0, 10000]`.
3. `verified_requires_all_pillars`: `classify_epistemic_status == Verified` implies `has_definition ∧ has_taxonomy_link ∧ has_synthesis ∧ has_empirical_grounding`.
4. `dag_rejects_self_loop_and_two_cycle`: `validate_taxonomy_dag` rejects all self-loops and directed cycles.
5. `skos_disjointness_sound`: `validate_skos_disjointness` returns `false` whenever any direct or transitive `broader` pair overlaps a symmetric `related` or `contrasts_with` pair.
6. `frontier_leverage_monotonic_and_gated`: `compute_frontier_leverage_bps` returns `0` when `realization_gap_bps == 0`, and is monotonically non-decreasing in `axis_alignment_bps`, `canonical_connectivity_count`, `has_frontier_or_limitation`, and `has_references_or_comparables`.

---

## 4. W3C SKOS Projection, Privacy & `OntologyIndex` Hydration

### 4.1 Privacy-Safe `OntologyIndex` (`src/rdf/ontology.rs`)

- `OntologyIndex` is extended with:
  - `concepts: BTreeMap<String, ConceptEntry>`
  - `concept_aliases: BTreeMap<String, BTreeSet<String>>` (multi-valued map from normalized alias / `pref_label` to concept slugs, preventing silent single-value overwrite collisions in `alias_map`)
  - `related: BTreeMap<String, BTreeSet<String>>` (symmetrized `skos:related`)
  - `contrasts_with: BTreeMap<String, BTreeSet<String>>` (symmetrized `ns:contrastsWith`)
- **Strict SKOS vs. OWL Separation:** `skos:Concept` hierarchy edges populate `broader` and derived `narrower` on `OntologyIndex` and NEVER pollute `sub_classes` (`rdfs:subClassOf`).
- **Privacy Filter on Shared Cache:** `LadybugStore::refresh_ontology` only indexes unrestricted (`access_policy == None`) `concept` rows into the shared namespace `OntologyIndex` cache so restricted concepts never leak via `expand_query_terms`, `get_snapshot`, or unauthenticated `subagent_prompt_injection`. Principal-scoped queries in `src/handlers/taxonomy.rs` build a principal-filtered view via `is_row_visible_to_principal`.

### 4.2 RDF Quad Projection (`src/rdf/projection.rs`)

Active visible `concept` rows project W3C SKOS triples into the namespace RDF graph (queryable via `neurostrata_sparql_query` and scenario `evaluate_sparql`):
- `<urn:neurostrata:concept:{ns}:{slug}> rdf:type skos:Concept`
- `skos:prefLabel "{pref_label}"`
- `skos:altLabel "{alias}"` (for each alias)
- `skos:definition "{definition}"`
- `skos:broader <urn:neurostrata:concept:{ns}:{parent}>`
- `skos:narrower <urn:neurostrata:concept:{ns}:{child}>` (derived inverse)
- `skos:related <urn:neurostrata:concept:{ns}:{related}>` (symmetrized)
- `ns:contrastsWith <urn:neurostrata:concept:{ns}:{contrast}>` (symmetrized)
- `ns:epistemicStatus "{seed|defined|synthesized|verified}"`

---

## 5. Unified Surface: `neurostrata_taxonomy` (MCP + HTTP Daemon + CLI)

All three surfaces (`neurostrata_taxonomy` MCP tool, `POST /taxonomy` in `src/daemon.rs`, and `neurostrata-mcp taxonomy <subcommand>` in `src/main.rs`) route through a single deterministic service `src/handlers/taxonomy.rs`. Every operation resolves the caller principal via `rebac::resolve_principal` and enforces `is_row_visible_to_principal` / `can_execute_action_on_row`.

### Operations (`operation` parameter)

1. **`set_thesis`**
   - Inputs: `namespace`, `primary_domain`, `mission`, `direction`, `core_pillars`, optional `adjacent_domains`, `evaluation_axes`, `non_goals`, `content`, `eligibility`, `access_policy`.
   - Behavior: Validates required fields and normalized `core_pillars` / `evaluation_axes` slugs, auto-registers `core_pillars` in the namespace domain registry, atomically supersedes any prior active `thesis` row in `namespace` via `commit_action_batch` CAS, refreshes `OntologyIndex`, and returns `{ thesis_id, superseded_id, thesis }`.
2. **`upsert_concept`**
   - Inputs: `namespace`, `slug`, `pref_label`, `definition`, optional `domain`, `content`, `aliases`, `broader`, `related_concepts`, `contrasts_with`, `tradeoffs`, `references`, `comparable_implementations`, `realization_status`, `current_limitations`, `frontier_opportunities`, `implemented_interfaces`, `linked_action_types`, `evaluation_scores`, `evidence_refs`, `governs`, `related_to`, `extracted_from`, `locations`, `eligibility`, `access_policy`.
   - Behavior:
     - Normalizes `slug` (`^[a-z0-9]+(-[a-z0-9]+)*$`) and validates `ComparableImplementation` entries (requiring non-empty `name`, `kind`, `license`, `reference`).
     - Validates referential integrity of `implemented_interfaces` (against active `interface_def` rows) and `linked_action_types` (against active `action_type` rows).
     - Enforces DAG cycle-freedom (`formal_kernel::validate_taxonomy_dag`) and transitive W3C SKOS S27 + contrast disjointness (`formal_kernel::validate_skos_disjointness`) across all active concepts in `namespace`; rejects violations with clear diagnostic errors.
     - Atomically supersedes any prior active `concept` row with the same `slug` (or inserts a new row if absent) via `commit_action_batch` CAS, writes `GOVERNS` / `RELATED_TO` / `EXTRACTED_FROM` edges in the same write batch, refreshes `OntologyIndex`, and returns `{ concept_id, superseded_id, slug, epistemic_status, epistemic_weight_bps, frontier_leverage_bps, concept }`.
3. **`tree`**
   - Inputs: `namespace`, optional `domain` or `root_slug`.
   - Behavior: Returns the deterministic (`BTreeMap`-ordered, cycle-guarded) hierarchical taxonomy forest over principal-visible concepts with derived `narrower`, `epistemic_status`, `descendant_count`, `grounding_count`, `aliases`, `related_concepts`, and `contrasts_with`.
4. **`briefing`**
   - Inputs: `namespace`, optional `query` / `topic`, optional `domain`, optional `max_hops` (default `2`, max `4`), optional `limit` (default `8`).
   - Behavior: Produces a principal-filtered **Ontology-Augmented Subject-Matter Briefing**:
     - `thesis`: The active visible Project Thesis.
     - `matched_concepts`: Seed concepts matched via lexical + `concept_aliases` (`skos:altLabel`) expansion, hydrated with `broader` lineage, derived `narrower` specializations, symmetrized `related_concepts` and `contrasts_with`, `tradeoffs`, `references`, `comparable_implementations`, and read-time `epistemic_status`.
     - `neighborhood_subgraph`: Deterministic (`BTreeSet`-ordered) **Forward-Expand / Backward-Prune** multi-hop neighborhood (`1..=max_hops`) around seed concepts—expanding across `broader`, `narrower`, `related_concepts`, and `contrasts_with`, then pruning leaf nodes at the outer hop that neither bridge two seed concepts nor carry active governing rules/facts.
     - `linked_knowledge`: Principal-visible governing `rule`, `fact`, and `procedure` rows connected to the matched concepts.
     - `epistemic_gaps`: Immediate subject-matter gaps relevant to the query/domain.
5. **`compare`**
   - Inputs: `namespace`, `slugs` (`Vec<String>`, 2 or more concept slugs), optional `axes` (`Vec<String>`, defaults to `thesis.evaluation_axes`).
   - Behavior: Generates a deterministic **Multi-Perspective Comparative Matrix** across the requested principal-visible concepts:
     - `concepts`: Full hydrated concept entries with read-time `epistemic_status` and `grounding_count`.
     - `shared_ancestors`: Common `broader` ancestors in the taxonomy DAG.
     - `direct_contrasts` & `bridging_relations`: Direct or 2-hop `contrasts_with` and `related_concepts` paths between the compared concepts.
     - `axis_matrix`: Side-by-side comparison across each evaluation axis (`evaluation_scores`, `tradeoffs`, and empirical rule/fact grounding).
     - `perspectives`: Three structured analytical viewpoints—`theoretical_lineage`, `operational_tradeoffs`, and `thesis_alignment`.
6. **`gaps`**
   - Inputs: `namespace`.
   - Behavior: Computes the principal-filtered **Epistemic Coverage & Ontology Validation Report**:
     - `epistemic_coverage_bps` (`0..10000`, via `formal_kernel::compute_epistemic_coverage_bps`)
     - `thesis_present: bool`
     - `status_counts`: `{ seed, defined, synthesized, verified }`
     - `unmapped_domains`: Registered domains that have tasks/rules/facts but zero `concept` rows.
     - `dangling_concept_refs`: Concept slugs referenced in `broader`, `related_concepts`, or `contrasts_with` that have not yet been defined as active `concept` rows.
     - `shallow_concepts`: Concepts at `"seed"` or `"defined"` status lacking synthesis or verified grounding.
     - `anti_patterns`: Deterministic ontology design lints:
       - `OrphanConcept`: Active concept with empty `broader`, derived `narrower`, and `related_concepts` whose slug/domain is not in `thesis.core_pillars`.
       - `OverloadedUmbrella`: Concept with `> 12` direct `narrower` children.
       - `UnanchoredPillar`: Entry in `thesis.core_pillars` with zero active `concept` rows.
       - `SymmetricConflict`: Any residual hierarchical vs. associative/contrast overlap in legacy rows.
7. **`advise` (Subject-Matter Expert Opportunity & Frontier Synthesis)**
   - Inputs: `namespace`, optional `query` / `topic`, optional `domain`, optional `top_k` (default `5`, max `20`).
   - Behavior: Produces deterministic, ontology-grounded SME recommendations joining the **Subject-Matter Taxonomy & External Frontier** with the **Project's Canonical Ontology & Implementation**:
     - Scores every principal-visible candidate concept using `formal_kernel::compute_frontier_leverage_bps` and sorts deterministically by `(leverage_score_bps DESC, grounding_count DESC, slug ASC)`.
     - Concepts with `leverage_score_bps > 0` and non-empty `frontier_opportunities` or `current_limitations` populate `recommendations: Vec<ExpertRecommendation>` (up to `top_k`), where each card contains:
       - `rank`, `concept_slug`, `pref_label`, `domain_pillar`, `epistemic_status`, `realization_status`, and `leverage_score_bps`.
       - `canonical_grounding`: Exact project entities connected to this subject (`implemented_interfaces`, `linked_action_types`, governing `rule`/`fact` IDs & summaries, `locations`, and open `task` IDs).
       - `current_state_and_limitations`: Recorded `realization_status` and `current_limitations`.
       - `frontier_recommendations`: Concrete SME-grade advancements from `frontier_opportunities` and `tradeoffs`, contrasted against `contrasts_with` alternatives.
       - `references_and_comparables`: Structured `references` and license-verified `comparable_implementations`.
       - `thesis_alignment`: Axis alignment breakdown (`axis_alignment_bps`, per-axis scores) and `non_goal_assessment` (`"not_assessed" | "potential_overlap"` based on lexical/slug intersection with `thesis.non_goals`, never claiming unproven `"zero_conflict"`).
     - Concepts that lack `frontier_opportunities`, `current_limitations`, or citations are returned in `insufficient_evidence_concepts: Vec<String>` rather than fabricating generic recommendations.
     - `external_research_agenda: Vec<ResearchPrompt>` (bounded to at most `5` items): Each `ResearchPrompt { target_slug_or_domain, reason, suggested_query, public_safe }` is generated ONLY from unrestricted public-safe (`access_policy == None`) pillar/concept labels, instructing the agent what literature or license-compatible libraries to research and ingest via `upsert_concept`.

---

## 6. Agent Workflow & Context-Budgeted Briefing Integration

1. **`neurostrata_get_snapshot` (`src/task/mod.rs` & `src/server.rs`):**
   - Appends a compact `### Project Thesis & Domain Expertise` section (strictly capped at `<= 800` characters, unrestricted concepts only) showing `primary_domain`, `direction`, `epistemic_coverage_bps`, and top epistemic gaps.
2. **`neurostrata_task_create` & `neurostrata_task_claim` (`build_task_graph_context` in `src/task/mod.rs`):**
   - Preserves `golden_rules` as the highest-priority, never-truncated section.
   - Adds `project_thesis` and up to `3` unrestricted matched `domain_concepts` within a strict `<= 1200` character budget in `subagent_prompt_injection`.
3. **Expert-to-Expert Advisory Protocol (`.agents/skills/neurostrata/SKILL.md` & `AGENTS.md`):**
   - When asked strategic, architectural, or domain-improvement questions (e.g., *"What are the top 5 ways we could improve this project?"*), the agent calls `neurostrata_taxonomy(operation: "advise", top_k: 5)`, executes any `public_safe` external research agenda queries (`websearch`, `context7`), persists newly discovered literature/comparables via `upsert_concept`, and presents recommendations grounded in canonical project entities and domain literature.
4. **Lock 2 Done-Funnel (`neurostrata_task_complete`):**
   - Supports extracting `memory_type: "concept"` (or linking a `concept` created via `neurostrata_taxonomy`) to satisfy Lock 2.
5. **360° Object View (`src/graph_read/object_view.rs`):**
   - Hydrates `concept` and `thesis` entities with their full taxonomic hierarchy and derived metrics.

---

## 7. Self-Seeding NeuroStrata's Own Domain Expertise

As part of this implementation, we will populate the `NeuroStrata` namespace with:
1. **NeuroStrata Project Thesis (`thesis`):**
   - `primary_domain`: `"agentic-cognitive-memory-and-ontology"`
   - `core_pillars`: `["cognitive-memory-architecture", "formal-ontology-and-rdf", "graph-theory-and-property-graphs", "hybrid-information-retrieval", "formal-verification-and-state-machines", "decentralized-synaptic-mesh"]`
   - `adjacent_domains`: `["computational-neuroscience", "bitemporal-databases", "program-analysis-and-scip", "digital-twins-and-simulation", "epistemology-and-knowledge-representation"]`
   - `evaluation_axes`: `["deterministic-verifiability", "single-binary-local-first-sovereignty", "bitemporal-auditability", "epistemic-depth-vs-flat-rag", "zero-trust-agent-governance"]`
2. **Foundational Subject-Matter Concept Graph (`concept` rows):**
   - Core concepts across all 6 pillars with full definitions, `broader`, `related_concepts`, `contrasts_with`, `tradeoffs`, `references`, license-verified `comparable_implementations`, `realization_status`, `current_limitations`, `frontier_opportunities`, `implemented_interfaces`, `linked_action_types`, `evaluation_scores`, and `evidence_refs`.

---

## 8. Testing & Verification Plan

1. **Pre-Release Blocker Test (`src/rdf/scenario.rs`):**
   - Transactional CAS test proving that mutating a live base row causes `commit_action_batch` / `commit_scenario` to reject with `rejected_base_conflict`, roll back all writes, and preserve the scenario.
2. **Lean 4 Proof & Parity Suite:**
   - `lake build` in `proofs/` compiles `proofs/NeuroStrata/Taxonomy.lean` with zero warnings and zero `sorry`/`axiom`.
   - `tests/lean_parity_test.rs` verifies 100% parity between `formal_kernel::{classify_epistemic_status, validate_taxonomy_dag, validate_skos_disjointness, compute_epistemic_coverage_bps, compute_frontier_leverage_bps}` and Lean theorems (including bounds, monotonicity, transitive S27 disjointness, and gating).
3. **Unit & Integration Tests:**
   - Atomic CAS enforcement for singleton `set_thesis` and unique `upsert_concept` slug.
   - Two-principal ReBAC non-leakage across `tree`, `briefing`, `compare`, `gaps`, `advise`, `OntologyIndex::expand_query_terms`, `get_snapshot`, and `subagent_prompt_injection`.
   - Transitive SKOS S27 disjointness rejection, DAG cycle rejection, and multi-target `concept_aliases` collision preservation.
   - Dynamic read-time `epistemic_status` recomputation when governing rules/facts are added or archived.
   - Round-trip and ranking tests for `advise` (`compute_frontier_leverage_bps`, tie-breaking, `insufficient_evidence_concepts`, `non_goal_assessment`, and `public_safe` research prompts).
   - CLI (`neurostrata-mcp taxonomy ...`) and HTTP daemon (`POST /taxonomy`) parity.
