# Subject-Matter Domain Expertise, SKOS Concept Taxonomy & Epistemic Briefing Engine (`neurostrata-f0bn`)

**Date:** 2026-11-15  
**Status:** Draft — Pending K3 Architectural Review & Owner Approval  
**Branch:** `feat/kinetic-actions`  
**Task ID:** `neurostrata-f0bn`

---

## 0. Foundational Invariant: Strictly Additive Architecture

This specification is **100% additive**. Every existing NeuroStrata capability, workflow gate, and subsystem is preserved in full without modification to its existing contracts:

1. **Tri-Strata Memory & Non-Destructive Lifecycle:** `global`, project namespace, and task working memory (`rule`, `fact`, `procedure`, `task`, `action_type`, `action_receipt`, `directory`, `file`, `markdown`, `doc_section`, `symbol`/`code_ast`), `neurostrata_supersede_memory`, `neurostrata_archive_memory`, and `neurostrata_append_log` remain unchanged.
2. **Central Executive & The 3 Soft Locks:** Zero-Action Start, Lock 1 (`pre-push` strict task gate), Lock 2 (`neurostrata_task_complete` mandatory memory extraction), Lock 3 (Breath working-memory note every 3–5 mutations), and `pre-commit` Graph-First query ledger remain strictly enforced.
3. **Code & Document Intelligence (`neurostrata-mtqx`):** AST & SCIP ingestion (`neurostrata_ingest_directory`, `neurostrata_ingest_scip`), Hybrid RRF search with `CodeTokenizer` (`neurostrata_search_memory`), compound code/blast-radius locator (`neurostrata_locate`), 4-stop guided diff tour (`neurostrata_diff_tour`), and longitudinal bitemporal code/governance insights (`neurostrata_insights`) remain intact.
4. **Enterprise Ontology & Operations (`neurostrata-1mus`):** Kinetic Action Types & Bitemporal Revert Log (`neurostrata_action`), Polymorphic Digital Twins & Deterministic Link Reducers (`neurostrata_interface_define`, `neurostrata_interface_query`, `neurostrata_link_reduce`), Counterfactual Copy-on-Write Scenarios (`neurostrata_scenario`), and Unified 360° Object View (`neurostrata_object_view`) remain intact.
5. **SynapticMesh & Security:** Shareable vs. private sovereignty, ReBAC principal enforcement, and `check_caller_path` daemon path authority remain intact.

The **Subject-Matter Domain Expertise & SKOS Concept Taxonomy** layer adds the missing second half of agent intelligence: making agents **true domain experts** in the subject matter of the project they serve (e.g., aesthetic medicine, clinical anatomy, and treatment protocols for `keywest.health`; cognitive memory architectures, W3C ontologies, graph theory, information retrieval, and formal verification for `NeuroStrata`), in addition to executing tasks inside the project.

---

## 1. Problem Statement & Motivation

Previously, NeuroStrata optimized heavily for *operational execution* (how an agent tracks tasks, respects project rules, and navigates files/symbols/twins) while leaving *subject-matter expertise* implicit. As a result:

- Agents knew *project rules* and *file structure*, but lacked a structured **Project Thesis** (what domain the project operates in, its strategic direction, core pillars, tangential domains, and evaluation axes when comparing alternative paradigms or architectures).
- Agents lacked a higher-order **Subject-Matter Taxonomy & Concept Graph** (`skos:Concept` hierarchy of broader/narrower concepts, tangential/related concepts, contrasting approaches, canonical definitions, trade-offs, evaluation axis ratings, and foundational references).
- When asked to evaluate a domain question, compare candidate approaches, or advise the owner as a subject-matter partner, agents had no pre-built domain taxonomy, multi-perspective comparative matrix, or epistemic gap map to reason from.

---

## 2. Higher-Order Knowledge Model (Vocabulary v11)

`src/schemas/memory-vocabulary.v3.json` is incremented from `vocabulary_version: 10` to `vocabulary_version: 11` (backwards-compatible with all v10 rows and tests) by adding two normative, non-structural memory types: `thesis` and `concept`.

### 2.1 Project Thesis (`memory_type: "thesis"`)

Each project namespace maintains at most one active `thesis` row (updates bitemporally supersede the prior active `thesis` row in that namespace, preserving full lineage).

- **`content`**: Concise executive statement of the project's purpose, subject-matter domain, and strategic direction.
- **`metadata.thesis`** (typed object):
  - `primary_domain` (`string`, required): Normalized domain slug or human label of the core discipline (e.g., `"aesthetic-medicine"`, `"agentic-cognitive-memory-and-ontology"`).
  - `mission` (`string`, required): What the project exists to achieve.
  - `direction` (`string`, required): Strategic trajectory and architectural/product North Star.
  - `core_pillars` (`array<string>`, required, non-empty): Top-level subject-matter domains that form the roots of the project's taxonomy.
  - `adjacent_domains` (`array<string>`, optional): Tangentially related fields and disciplines agents should understand when researching or synthesizing ideas.
  - `evaluation_axes` (`array<string>`, optional): Criteria used to compare external approaches, literature, or designs against this project's philosophy.
  - `non_goals` (`array<string>`, optional): Explicit boundaries of what the project does not pursue.
- **`metadata.eligibility`**: Defaults to `"shareable"` so team members on `SynapticMesh` share the project's North Star unless marked `"private"`.

### 2.2 Subject-Matter Concept (`memory_type: "concept"`)

Represents a higher-order unit of domain knowledge in the project's W3C SKOS/OWL taxonomy and knowledge graph.

- **`content`**: Canonical explanation and synthesis of the concept in the context of the project's domain.
- **`metadata.concept`** (typed object):
  - `slug` (`string`, required): Normalized lowercase-kebab identifier (`^[a-z0-9]+(-[a-z0-9]+)*$`), unique among active `concept` rows in the namespace.
  - `pref_label` (`string`, required): Canonical human-readable name (`skos:prefLabel`).
  - `aliases` (`array<string>`, optional): Synonyms, acronyms, or alternate terminology (`skos:altLabel`).
  - `definition` (`string`, required): Concise formal definition (`skos:definition`).
  - `broader` (`array<string>`, optional): Parent concept slugs (`skos:broader` / `rdfs:subClassOf`). Empty when the concept is a top-level root pillar under its `domain`.
  - `narrower` (`array<string>`, optional): Child/specialized concept slugs (`skos:narrower`).
  - `related_concepts` (`array<string>`, optional): Tangential, lateral, or cross-domain concepts (`skos:related`). Must be disjoint from `broader`/`narrower` per W3C SKOS S27.
  - `contrasts_with` (`array<string>`, optional): Competing paradigms, alternative algorithms, or differential concepts (`ns:contrastsWith`). Must be disjoint from `broader`/`narrower`.
  - `tradeoffs` (`array<string>`, optional): Key strengths, failure modes, and decision boundaries (when to use vs. when to avoid).
  - `references` (`array<string>`, optional): Foundational papers, clinical protocols, standards, or benchmarks (`dcterms:references`).
  - `implemented_interfaces` (`array<string>`, optional): Polymorphic `interface_def` names associated with this domain concept when it maps to concrete digital-twin entities.
  - `evaluation_scores` (`object<string, integer>`, optional): Basis-point ratings (`0..10000`) keyed by `thesis.evaluation_axes` slugs.
  - `epistemic_status` (`"seed" | "defined" | "synthesized" | "verified"`, optional): Computed deterministically by the formal kernel from the concept's structural completeness and empirical grounding.
- **Derived Read-Time Properties:**
  - `descendant_count`: Transitive count of active `narrower` concepts in the taxonomy DAG.
  - `grounding_count`: Count of active governing `rule`, `fact`, `procedure`, and code/document anchor connections.
  - `epistemic_weight_bps`: Formal kernel basis-point weight (`2500 | 5000 | 8000 | 10000`).
- **Graph Relations:**
  - `concept` rows can carry standard `metadata.governs`, `metadata.related_to`, `metadata.depends_on`, and `metadata.extracted_from` edges, plus optional `locations` when a domain concept is anchored to specific files or symbols.

---

## 3. Formal Verification Kernel (`src/formal_kernel.rs` & `proofs/NeuroStrata/Taxonomy.lean`)

Per the Formal Verification Guard (`b0813917-a806-4faa-8444-f7acec555ed9`), the pure decision logic for taxonomy integrity, SKOS disjointness, and epistemic maturity is factored into `src/formal_kernel.rs`, proven in `proofs/NeuroStrata/Taxonomy.lean` with zero `sorry`/`axiom`, and verified by parity tests in `tests/lean_parity_test.rs`.

### 3.1 Epistemic Maturity Classification

Given booleans for a concept node:
- `has_definition: bool` (non-empty `definition`)
- `has_taxonomy_link: bool` (`!broader.is_empty() || !narrower.is_empty() || !related_concepts.is_empty()`)
- `has_synthesis: bool` (`!tradeoffs.is_empty() && (!contrasts_with.is_empty() || !references.is_empty())`)
- `has_empirical_grounding: bool` (`has_synthesis && (linked_rules_or_facts_count > 0 || verified_flag)`)

The pure function `classify_epistemic_status(has_definition, has_taxonomy_link, has_synthesis, has_empirical_grounding) -> EpistemicStatus` returns:
1. `EpistemicStatus::Seed` (weight `2500` bps) — stub or missing definition/taxonomy placement.
2. `EpistemicStatus::Defined` (weight `5000` bps) — has canonical definition and taxonomic link (`broader`/`narrower`/`related_concepts`).
3. `EpistemicStatus::Synthesized` (weight `8000` bps) — `Defined` plus explicit `tradeoffs` and either `contrasts_with` or `references`.
4. `EpistemicStatus::Verified` (weight `10000` bps) — `Synthesized` plus empirical grounding in project rules, facts, or verified citations.

### 3.2 Taxonomy DAG, SKOS S27 Disjointness & Coverage Kernel

- `validate_taxonomy_dag(edges: &[(u32, u32)]) -> bool`: Pure cycle-detection kernel over `broader` (`child -> parent`) edges. Any self-loop or directed cycle is rejected deterministically on `upsert_concept`, while `OntologyIndex` hydration sanitizes legacy cycles fail-softly.
- `validate_skos_disjointness(hierarchical_edges: &[(u32, u32)], associative_edges: &[(u32, u32)]) -> bool`: Enforces W3C SKOS S27 integrity—no concept pair `(u, v)` connected in `broader`/`narrower` (in either direction) may simultaneously appear in `related_concepts` or `contrasts_with`.
- `compute_epistemic_coverage_bps(statuses: &[EpistemicStatus], unmapped_domain_count: usize) -> u32`: Computes overall namespace epistemic maturity in basis points `[0..10000]`, penalizing registered domains that have rules/tasks but zero `concept` nodes.

### 3.3 Lean 4 Theorems (`proofs/NeuroStrata/Taxonomy.lean`)

1. `classify_epistemic_monotonic`: Adding definition, taxonomy links, synthesis, or grounding never decreases `EpistemicStatus`.
2. `epistemic_status_weight_bounded`: Every status weight and `compute_epistemic_coverage_bps` output is strictly bounded within `[0, 10000]`.
3. `verified_requires_all_pillars`: `classify_epistemic_status == Verified` implies `has_definition ∧ has_taxonomy_link ∧ has_synthesis ∧ has_empirical_grounding`.
4. `dag_rejects_self_loop_and_two_cycle`: `validate_taxonomy_dag` rejects all self-loops and directed cycles.
5. `skos_disjointness_sound`: `validate_skos_disjointness` returns `false` whenever any hierarchical pair overlaps a symmetric associative/contrast pair.

---

## 4. W3C SKOS/OWL Projection & `OntologyIndex` Hydration

### 4.1 `OntologyIndex` (`src/rdf/ontology.rs`)

- `OntologyIndex` is extended with `concepts: BTreeMap<String, ConceptEntry>`, `related: BTreeMap<String, BTreeSet<String>>`, and `contrasts_with: BTreeMap<String, BTreeSet<String>>`.
- When hydrating `OntologyIndex` from a namespace's active rows (`LadybugStore::refresh_ontology_index`):
  - Every active `memory_type == "concept"` row registers its `slug`, `pref_label`, and `aliases` into `alias_map` and `alt_labels`.
  - Its `broader` slugs register into `broader` (and inverse `narrower`) plus `sub_classes`.
  - Its `related_concepts` register symmetrically into `related`.
  - Its `contrasts_with` register symmetrically into `contrasts_with`.
- Consequently, `OntologyIndex::expand_query_terms` automatically expands search queries with concept `pref_label`, `aliases` (`skos:altLabel`), and narrower concepts across the entire codebase.

### 4.2 RDF Quad Projection (`src/rdf/projection.rs`)

Active `concept` rows project W3C SKOS triples into the namespace RDF graph (queryable via `neurostrata_sparql_query` and scenario `evaluate_sparql`):
- `<urn:neurostrata:concept:{ns}:{slug}> rdf:type skos:Concept`
- `skos:prefLabel "{pref_label}"`
- `skos:altLabel "{alias}"` (for each alias)
- `skos:definition "{definition}"`
- `skos:broader <urn:neurostrata:concept:{ns}:{parent}>`
- `skos:narrower <urn:neurostrata:concept:{ns}:{child}>`
- `skos:related <urn:neurostrata:concept:{ns}:{related}>`
- `ns:contrastsWith <urn:neurostrata:concept:{ns}:{contrast}>`
- `ns:epistemicStatus "{seed|defined|synthesized|verified}"`

---

## 5. Unified Surface: `neurostrata_taxonomy` (MCP + HTTP Daemon + CLI)

All three surfaces (`neurostrata_taxonomy` MCP tool, `POST /taxonomy` in `src/daemon.rs`, and `neurostrata-mcp taxonomy <subcommand>` in `src/main.rs`) route through a single deterministic handler `src/handlers/taxonomy.rs`.

### Operations (`operation` parameter)

1. **`set_thesis`**
   - Inputs: `namespace`, `primary_domain`, `mission`, `direction`, `core_pillars`, optional `adjacent_domains`, `evaluation_axes`, `non_goals`, `content`.
   - Behavior: Validates required fields, auto-registers `core_pillars` in the namespace domain registry if missing, bitemporally supersedes any prior active `thesis` row in `namespace`, and returns `{ thesis_id, superseded_id, thesis }`.
2. **`upsert_concept`**
   - Inputs: `namespace`, `slug`, `pref_label`, `definition`, optional `domain`, `content`, `aliases`, `broader`, `narrower`, `related_concepts`, `contrasts_with`, `tradeoffs`, `references`, `implemented_interfaces`, `evaluation_scores`, `governs`, `related_to`, `extracted_from`, `locations`, `verified`.
   - Behavior:
     - Normalizes `slug` (`^[a-z0-9]+(-[a-z0-9]+)*$`).
     - Enforces DAG cycle-freedom (`formal_kernel::validate_taxonomy_dag`) and W3C SKOS S27 hierarchical/associative disjointness (`formal_kernel::validate_skos_disjointness`); rejects violations with clear diagnostic errors.
     - Computes `epistemic_status` via `formal_kernel::classify_epistemic_status`.
     - If an active `concept` with `slug` already exists in `namespace`, bitemporally supersedes it; otherwise inserts a new `concept` row.
     - Automatically registers `domain` if provided, updates reciprocal `narrower`/`broader`/`related_concepts`/`contrasts_with` index state, refreshes `OntologyIndex`, and returns `{ concept_id, superseded_id, slug, epistemic_status, epistemic_weight_bps, concept }`.
3. **`tree`**
   - Inputs: `namespace`, optional `domain` or `root_slug`.
   - Behavior: Returns the hierarchical taxonomy forest (domains -> root concepts -> narrower concepts) with `epistemic_status`, `descendant_count`, `grounding_count`, `aliases`, `related_concepts`, `contrasts_with`, and linked governing rule/fact counts.
4. **`briefing`**
   - Inputs: `namespace`, optional `query` / `topic`, optional `domain`, optional `max_hops` (default `2`, max `4`), optional `limit` (default `8`).
   - Behavior: Produces a complete **Ontology-Augmented Subject-Matter Briefing** for an agent:
     - `thesis`: The active Project Thesis (`primary_domain`, `mission`, `direction`, `core_pillars`, `adjacent_domains`, `evaluation_axes`, `non_goals`).
     - `matched_concepts`: Seed concepts matched via RRF hybrid lexical + alias/synonym expansion, hydrated with `broader` lineage, `narrower` specializations, `related_concepts`, `contrasts_with`, `tradeoffs`, `references`, and `evaluation_scores`.
     - `neighborhood_subgraph`: **Forward-Expand / Backward-Prune** multi-hop neighborhood (`1..=max_hops`) around the seed concepts—expanding across `broader`, `narrower`, `related_concepts`, and `contrasts_with`, then pruning ungrounded leaf nodes that neither connect two seed concepts nor carry governing rules/facts.
     - `linked_knowledge`: Governing `rule`, `fact`, and `procedure` rows connected to the matched concepts.
     - `epistemic_gaps`: Immediate subject-matter gaps relevant to the query/domain (dangling concept references or `"seed"` concepts lacking synthesis).
5. **`compare`**
   - Inputs: `namespace`, `slugs` (`Vec<String>`, 2 or more concept slugs), optional `axes` (`Vec<String>`, defaults to `thesis.evaluation_axes`).
   - Behavior: Generates a structured **Multi-Perspective Comparative Matrix** across the requested concepts:
     - `concepts`: Full hydrated concept entries with `epistemic_status` and `grounding_count`.
     - `shared_ancestors`: Common `broader` ancestors in the taxonomy DAG.
     - `direct_contrasts` & `bridging_relations`: Direct or 2-hop `contrasts_with` and `related_concepts` paths between the compared concepts.
     - `axis_matrix`: Side-by-side comparison across each evaluation axis (`evaluation_scores`, `tradeoffs`, and empirical rule/fact grounding).
     - `perspectives`: Three structured analytical viewpoints—`theoretical_lineage` (taxonomic position and formal guarantees), `operational_tradeoffs` (failure modes, decision boundaries, and linked project rules), and `thesis_alignment` (fit against `thesis.direction`, `evaluation_axes`, and `non_goals`).
6. **`gaps`**
   - Inputs: `namespace`.
   - Behavior: Computes the namespace-wide **Epistemic Coverage & Ontology Validation Report**:
     - `epistemic_coverage_bps` (`0..10000`)
     - `thesis_present: bool`
     - `status_counts`: `{ seed, defined, synthesized, verified }`
     - `unmapped_domains`: Registered domains that have tasks/rules/facts but zero `concept` rows.
     - `dangling_concept_refs`: Concept slugs referenced in `broader`, `narrower`, `related_concepts`, or `contrasts_with` that have not yet been defined as `concept` rows.
     - `shallow_concepts`: Concepts still at `"seed"` or `"defined"` status that lack `tradeoffs`, `contrasts_with`, or `references`.
     - `anti_patterns`: Deterministic ontology design lints (`OrphanConcept`, `OverloadedUmbrella`, `UnanchoredPillar`, `SymmetricConflict`).

---

## 6. Agent Workflow & Automatic Briefing Integration

1. **`neurostrata_get_snapshot` (`src/task/mod.rs` & `src/server.rs`):**
   - Appends a `### Project Thesis & Domain Expertise` section to the snapshot output when a `thesis` or `concept` rows exist (or a concise prompt to establish the Project Thesis & Taxonomy via `neurostrata_taxonomy` when absent), including `epistemic_coverage_bps` and top epistemic gaps.
2. **`neurostrata_task_create` & `neurostrata_task_claim` (`build_task_graph_context` in `src/task/mod.rs`):**
   - Alongside `golden_rules`, `normative_memories`, `structural_anchors`, and `conceptual_bridges`, `auto_retrieved_context` and `subagent_prompt_injection` now include:
     - `project_thesis`: Concise summary of the active `thesis` (`primary_domain`, `direction`, `evaluation_axes`).
     - `domain_concepts`: Up to 3 matched `concept` rows relevant to the task, formatted with their definition, `broader`/`related_concepts`/`contrasts_with`, and `tradeoffs`.
   - Every subagent spawned with `subagent_prompt_injection` immediately receives both the operational rules AND the domain-expertise briefing.
3. **Lock 2 Done-Funnel (`neurostrata_task_complete`):**
   - Explicitly documents and supports extracting `memory_type: "concept"` (or linking a `concept` created via `neurostrata_taxonomy`) to satisfy Lock 2 when completing research, domain-modeling, or architectural tasks.
4. **360° Object View (`src/graph_read/object_view.rs`):**
   - Hydrates `concept` and `thesis` entities with their taxonomic hierarchy (`broader`, `narrower`, `related_concepts`, `contrasts_with`, `tradeoffs`, `epistemic_status`, `descendant_count`, `grounding_count`).

---

## 7. Self-Seeding NeuroStrata's Own Domain Expertise

As part of this implementation, we will populate the `NeuroStrata` namespace with:
1. **NeuroStrata Project Thesis (`thesis`):**
   - `primary_domain`: `"agentic-cognitive-memory-and-ontology"`
   - `core_pillars`: `["cognitive-memory-architecture", "formal-ontology-and-rdf", "graph-theory-and-property-graphs", "hybrid-information-retrieval", "formal-verification-and-state-machines", "decentralized-synaptic-mesh"]`
   - `adjacent_domains`: `["computational-neuroscience", "bitemporal-databases", "program-analysis-and-scip", "digital-twins-and-simulation", "epistemology-and-knowledge-representation"]`
   - `evaluation_axes`: `["deterministic-verifiability", "single-binary-local-first-sovereignty", "bitemporal-auditability", "epistemic-depth-vs-flat-rag", "zero-trust-agent-governance"]`
2. **Foundational Subject-Matter Concept Graph (`concept` rows):**
   - Core concepts across all 6 pillars (e.g., `tri-strata-memory`, `episodic-vs-semantic-memory`, `skos-concept-taxonomy`, `shacl-closed-world-validation`, `bitemporal-property-graph`, `reciprocal-rank-fusion`, `tarjan-scc-condensation`, `lean4-formal-kernel-parity`, `counterfactual-cow-sandbox`, `kinetic-action-reversibility`, `crdt-lww-mesh-sovereignty`) with full definitions, `broader`/`narrower`/`related_concepts`, `contrasts_with`, `tradeoffs`, `evaluation_scores`, and `references`.

---

## 8. Testing & Verification Plan

1. **Lean 4 Proof & Parity Suite:**
   - `lake build` in `proofs/` compiles `proofs/NeuroStrata/Taxonomy.lean` with zero warnings and zero `sorry`/`axiom`.
   - `tests/lean_parity_test.rs` verifies 100% parity between `formal_kernel::{classify_epistemic_status, validate_taxonomy_dag, validate_skos_disjointness, compute_epistemic_coverage_bps}` and Lean theorems.
2. **Unit & Integration Tests:**
   - `set_thesis` creation and bitemporal supersession.
   - `upsert_concept` creation, automatic `epistemic_status` progression (`seed -> defined -> synthesized -> verified`), DAG cycle rejection, SKOS S27 disjointness enforcement, and `OntologyIndex` alias/narrower query expansion.
   - SKOS RDF quad projection (`rdf:type skos:Concept`, `skos:broader`, `skos:narrower`, `skos:related`, `ns:contrastsWith`) verified via `neurostrata_sparql_query`.
   - `tree`, `briefing` (including Forward-Expand / Backward-Prune neighborhood), `compare` (multi-perspective comparative matrix), and `gaps` (including ontology anti-pattern lints) output accuracy.
   - `neurostrata_get_snapshot` and `build_task_graph_context` (`subagent_prompt_injection`) inclusion of `project_thesis` and `domain_concepts`.
   - CLI (`neurostrata-mcp taxonomy ...`) and HTTP daemon (`POST /taxonomy`) parity.
