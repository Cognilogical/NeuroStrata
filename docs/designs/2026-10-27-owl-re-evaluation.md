# OWL Re-Evaluation for NeuroStrata

Date: 2026-10-27. Design only. No implementation authorized.

Independent seats: Argon (Gemini 4 Argon) and K3 (Kimi K3.1 Pro). Both ran in
parallel and did not read each other's output. This document adjudicates them
against the repository at HEAD `ad58e35` and against verified crate metadata.

## 1. Verdict

**PILOT — scoped.** Adopt **SKOS + a restricted OWL 2 RL subset + closed-world
shape validation (SHACL-style)**, delivered as **Option C (compiled ontology
query expansion) plus a light Option A (write-time annotator)**, with **no
persistent RDF store and no OWL 2 DL reasoner**.

One-line rationale: the credible accuracy gains come from vocabulary discipline,
alias and canonicalization, class facets, and bounded transitive traversal over
structured metadata and AST symbols; none of that requires a DL reasoner, and
none of it touches free text.

This is **not** a "bring in OWL" decision. It is a decision to use OWL's
vocabulary and SKOS's labeling machinery, plus RL forward-chaining for a small
number of curated axioms, while explicitly rejecting OWL 2 DL, open-world
validation, and any second store.

## 2. What the owner's accuracy claim plausibly is

Before choosing a design, split the claim into three cases. The pilot must
identify which case produced the owner's prior lift.

1. **Taxonomies + aliases + transitive chains.** Curated `subClassOf`,
   `skos:altLabel`, `sameAs`/`exactMatch`, and property chains. Reproducible
   in Rust in days. This is the case the proposed pilot tests.
2. **Ontology-guided extraction from prose.** An extractor (LLM or NER) emits
   triples constrained by an ontology. If this is the case, the missing piece
   in NeuroStrata is **write-time entity extraction** into `fact.topology`,
   `KnownIds`, and concept nodes — because `MemoryPayload.content` is free text
   today (`docs/COGNITIVE_ARCHITECTURE.md` §6). Ontology alone does nothing
   until extraction exists.
3. **Complex OWL 2 DL class reasoning.** Existential restrictions, negation,
   disjoint unions, qualified cardinality. If this is the case, please share one
   concrete example and the reasoner used. In Rust today this needs
   `horned-owl` (LGPL-3.0) plus an experimental reasoner, or a JVM sidecar
   (HermiT/ELK/Openllet), which breaks the single-binary offline design.

If the owner's evidence is dominated by case 1, the pilot below is the right
scope. If it is case 2, fund extraction first. If case 3, the design changes
materially and a sidecar becomes justified.

## 3. Where OWL genuinely helps (steelmanned)

Scope is **(a) memory metadata/schema plus deterministic AST symbols**, not
prose entities. Operating on free text requires extraction, which is a separate,
error-producing system.

Mechanisms, preconditions, and expected effect:

| Mechanism | Precondition | Inference / retrieval effect | Measure |
|---|---|---|---|
| Query and filter expansion | `domain_registry` upgraded from flat `Vec<String>` to a DAG + alias map | `subClassOf` closure turns `domain="auth"` into `{"auth","tls","mtls","oauth"}`; `altLabel` injects canonical terms. Fixes exact-string domain misses in `search_with_domain` | Recall@5 on domain-scoped queries using narrower sub-domains or synonyms |
| Entity canonicalization | Canonical slug mapping for `fact.topology.{component,target_system}` and `KnownIds` | `exactMatch`/controlled `sameAs` collapse `"kuzu"/"lbug"/"LadybugDB"` into one concept id, enabling 1-hop traversal across aliases | Recall@5 and MRR on cross-alias topology queries |
| Class facets | Defined classes over metadata fields | `EnforcedRule`, `FailedAttemptFact`, `ArchitectureDecision` as inferred facets without proliferating `memory_type` strings | Precision@5 on epistemic-category queries |
| Inverse and subproperty | Typed sub-relations mapped to the 4 materialized edges | `decision.superseded_by subPropertyOf RELATES_TO`; enables graph expansion over domain-specific relations | Graph-expansion hit rate on decision and topology chains |
| Disjointness | Separate dimensions for record role, artifact kind, lifecycle | Detect impossible combinations (an ENFORCED rule that is also a FailedAttempt; a superseded decision with no `superseded_by`) | Defect catch rate on malformed writes |
| Closed-world shape validation | Endpoint types on `GOVERNS`, `CONTAINS`, `EXTRACTED_FROM` | Reject or warn on malformed edges before they consume the 1-2 hop expansion budget | Reduction in invalid expansion edges |
| Cross-namespace alignment | Shared concept IRIs plus per-namespace `exactMatch` | Aligns `authn` vs `auth` across SynapticMesh without forcing identical local slugs | Cross-mesh Recall@5 on imported shared memories |
| Explanation / provenance | Derivation traces on inferred edges | Populates `SearchEvidence.path` and `explanation` with the ontology derivation chain | Downstream agent task accuracy on multi-hop constraint adherence |

Two important limits:

- **Ontology expansion alone does not fix exact-identifier retrieval.** The architecture document is explicit that exact matching remains unimplemented (`docs/COGNITIVE_ARCHITECTURE.md` §6). Alias resolution must feed deterministic ID/symbol lookup as well as vector expansion, or the largest easy win is missed.
- **`SearchEvidence` already carries transient paths/explanations** (`src/traits.rs:66-104`). Ontology-derived evidence fits this existing seam without mutating payloads.

## 4. Where OWL is the wrong tool (red-team)

1. **Open-world semantics vs. record validation.** Under OWL, `rdfs:domain`/`rdfs:range` and cardinality restrictions **infer** types on bad edges instead of rejecting them; missing required fields pass silently. Record and edge-endpoint validation requires closed-world semantics (SHACL or native Rust validators), never OWL axioms.
2. **No unique name assumption.** OWL does not assume two IRIs denote different individuals unless `owl:AllDifferent` is asserted over O(N^2) pairs. Without care this produces unexpected `sameAs` collapses.
3. **Free text is opaque to description logic.** A reasoner cannot read `MemoryPayload.content`. Unless structure is in metadata or deterministically extracted, OWL does nothing.
4. **OWL does not improve embeddings, exact-token matching, relevance judgments, or poor source text.** Broad expansion can reduce precision.
5. **Bi-temporal and generation-scoped graphs break naive materialization.** Edges are filtered by `valid_to`, `archived`, `is_mesh_conflicting`, and `active_generation` (`src/store/ladybug.rs:129,136,139,1306`). Materialized inferred edges become phantom truths unless re-derived on supersede/archive/publish-generation.
6. **A second persistent store violates the single-store rule.** `export_database` (`src/traits.rs:449-457`) is a checkpointed copy of one LadybugDB file; a parallel Oxigraph store creates split-brain transactions, doubles WAL footprint, and breaks atomic export and SynapticMesh replay.
7. **Rust has no production-grade OWL 2 DL reasoner.** `horned-owl` is LGPL-3.0 (copyleft on the static binary); `rustdl` yanked `owl-dl-cli` 0.1.0-0.3.0 on 2026-09-04 over soundness/completeness bugs; `ontologos` is three months old. OWL 2 DL is N2EXPTIME worst case.
8. **Governance is the real cost.** A bad `sameAs`, equivalence, or transitivity axiom contaminates every namespace. Ontology changes are retrieval behavior changes.

## 5. Disagreements between the two seats, and adjudication

| Disagreement | Argon | K3 | Adjudication |
|---|---|---|---|
| Transitive `GOVERNS` over `CONTAINS` | Safe and useful: `GOVERNS ∘ CONTAINS -> GOVERNS` (property chain) | Dangerous: property-chain governance inheritance can contaminate; `GOVERNS` is not inherently transitive | **Reject the axiom.** The underlying bug is real (`govern_out_q` is one hop, `src/store/ladybug.rs:251-259`, so a rule governing `src/store` misses a symbol two hops away), but a global property chain is the wrong fix: it bakes in a semantic assumption that does not always hold, it crosses bi-temporal boundaries, and it requires truth maintenance. Fix by extending the Cypher traversal to 2-3 hops with explicit provenance, not by an OWL axiom. |
| Disjointness soundness | Now sound: structural vs. normative partition exists | Still holds: v8 mixes record roles/artifacts and permits arbitrary custom types | **K3 is right.** `STRUCTURAL_MEMORY_TYPES` (`src/store/ladybug.rs:1440-1446`) is a fixed set, so structural-vs-normative disjointness is sound. But `memory_type` is a single dimension that still accepts any nonempty string (`src/schemas/memory-vocabulary.v3.json:205-213`), so role disjointness requires separating dimensions (record role vs. artifact kind vs. lifecycle) before it is sound. |
| Domain fallback | Implies exact equality, no fallback mentioned | (silent) | **Verified:** `search_with_domain` falls back to unfiltered rows when the domain key set matches nothing (`src/store/ladybug.rs:147-211`; test at `:8720`). Any pilot measuring domain expansion must account for this fallback or it will misattribute hits. |
| Pilot comparison baseline | Compares to baseline vector + 1-hop only | Requires beating a **flat alias dictionary** (ablation arm 3) | **K3 is right, and this is the most important methodological correction.** If the ontology does not beat a JSON alias map, the formalism adds nothing. Argon's pilot would have misattributed dictionary gains to OWL. |
| Pilot effort and metrics | 5 days; ≥+15% Recall@5; p95 ≤1 ms | 1-3 weeks for Option C; ≥5% relative nDCG@10 or ≥5pp Recall@5; p95 ≤25 ms cached; kill if flat aliases win | Adopt K3's methodology and thresholds; Argon's latency bar (1 ms) is too strict for any real closure, and the 5-day estimate is too optimistic once curation is included. |
| Rust tooling versions | oxigraph 0.4.x; reasonable 0.5.x; horned-owl 1.4/3.x; whelk/whelk-rs 0.1.x MIT; ontologos 1.1.4 | oxigraph 0.5.13; reasonable 0.4.4/0.5.0-a1; horned-owl 4.0.0; no whelk-rs crate; rudof_lib 0.3.35; rustdl 0.4.42; shacl 0.3.35 | **K3 verified, Argon was wrong on whelk** (the crates.io `whelk` crate is an unrelated Unix shell, 16 downloads). Verified: oxigraph 0.5.13, horned-owl 4.0.0, oxrdf 0.3.5, reasonable 0.5.0-a1, shacl 0.3.35, sophia 0.10.0. |
| `reasonable` unimplemented rules | Not mentioned | Warns that `reasonable` documents unimplemented rules | Adopt K3's caution: treat `reasonable` as a restricted RL engine, not full compliance. |
| `sameAs` scope | Controlled `sameAs`/`exactMatch` | Reviewed identity only; never synonym substitution | Adjudicate to K3: `sameAs` only for verified individual identity (e.g., `"LadybugDB" == "lbug"` as one database system), never for lexical synonyms (`altLabel` handles those). |
| Cross-namespace alignment | SKOS `exactMatch` across mesh | Requires shared versioned ontology packages; domain registries are private | K3's governance framing is right; Argon's `concept_iri` stamping is a plausible implementation but must be opt-in and must not leak private domain slugs. |
| Prior rejection ground 5 (mixed types) | Weaker: already partitioned | Holds: still mixed | K3 is right (see above). |

## 6. Architecture

**Recommended: Option C + light Option A.** Reject B and D.

- **Option C (compiled ontology query expansion).** Hold the compiled TBox + SKOS vocabulary in an in-memory Rust DAG (`ArcSwap<OntologyIndex>`). At read time, expand `domain` filters via `subClassOf` closure, expand query identifiers via `altLabel` to canonical symbols/`KnownIds`, and expand relation filters via `subPropertyOf`. Always retain the original query; on failure fall back to baseline. Single store, deterministic, offline. Effort: 1-3 weeks.
- **Option A (write-time annotator, light).** At `upsert`, `relink_edges`, and `publish_generation`, validate closed-world shapes, canonicalize `topology` entities, and infer class facets, writing results back to LadybugDB. Bi-temporal truth maintenance by tagging inferred rows with supporting source IDs or recomputing per namespace during `relink_edges` (`src/store/ladybug.rs:2321`). Effort: 3-5 weeks for the full version; the pilot needs only canonicalization + facets.
- **Reject Option B (parallel Oxigraph).** Split-brain transactions, divergent `export_database`, double C++ FFI (lbug + RocksDB), permanent ops burden. Not justified unless SPARQL itself becomes a demonstrated requirement.
- **Reject Option D (RDF-native migration).** Throws away working vector + Cypher GraphRAG, `IntakeStore` CAS, staged generations, and mesh; still needs a vector store anyway.

Data scope for the pilot: **(a) only** — metadata/schema plus existing deterministic AST symbols. Do not extract prose entities with GLiNER during the pilot; that would confound ontology value with NER/entity-linking errors.

## 7. OWL profile and feature selection

Use **SKOS + OWL 2 RL (restricted) + closed-world shapes**. Do not use OWL 2 DL.

OWL 2 RL is PTIME Datalog materialization with deterministic forward-chaining and no backtracking tableau search. OWL 2 EL is also PTIME but targets existential-heavy TBoxes. OWL 2 QL targets relational query rewriting. None of these bounds closure size or curation cost, so caps are still required.

| Feature | Decision | Notes |
|---|---|---|
| `skos:prefLabel`, `altLabel`, `hiddenLabel` | Adopt | Lexical synonym/acronym mapping to canonical domains, components, symbols |
| `skos:exactMatch`, cautious `closeMatch` | Adopt | Cross-vocabulary alignment without merging memory rows |
| `rdfs:subClassOf`, `skos:broader` | Adopt | Hierarchical taxonomy for domain slugs and `memory_type` facets |
| `rdfs:subPropertyOf`, `owl:inverseOf` | Adopt | Map domain-specific relations into the 4 core edges |
| `owl:equivalentClass` (restricted) | Adopt | Controlled class equivalence for facets |
| `owl:disjointWith` | Adopt carefully | Only across soundly separated dimensions |
| `owl:sameAs` | Restricted | Reviewed individual identity only; never synonym substitution |
| `owl:TransitiveProperty` | Restricted | Only where semantics genuinely hold; not `GOVERNS` or `RELATES_TO` |
| `owl:propertyChainAxiom` | Reject | Use bounded Cypher traversal instead (see §5) |
| `rdfs:domain`/`rdfs:range` | Inference/documentation only | Never as validation; implement endpoint checks as closed-world validators |
| `owl:FunctionalProperty`/`InverseFunctionalProperty` | Reject | Equality-generating dependencies merge distinct memory UUIDs |
| Existential/universal restrictions, unions, complements, nominals, qualified cardinality, SWRL | Reject | Overkill; not available in Rust DL reasoners anyway |
| SHACL Core | Adopt | Required fields, datatypes, cardinality, edge endpoint classes; warn-first to match current behavior |

Materialization rules: named individuals only, no blank-node generation, fixed rule set, sorted fixed-point output, derivation provenance, explicit closure and time caps.

## 8. Rust tooling (verified 2026-10-27)

| Crate | Version | License | Role | Verdict |
|---|---|---|---|---|
| `oxrdf` | 0.3.5 | Apache-2.0 OR MIT | RDF data structures | Adopt |
| `oxttl` / `oxjsonld` | 0.1.x | Apache-2.0 OR MIT | Turtle/JSON-LD parse/serialize | Adopt |
| `oxigraph` | 0.5.13 | Apache-2.0 OR MIT | SPARQL/RDF store (RocksDB or in-memory) | Use in-memory only if needed; no built-in reasoner |
| `reasonable` | 0.5.0-a1 (also 0.4.4) | BSD-3-Clause | OWL 2 RL Datalog materializer | Best pilot candidate; treat as restricted RL, not full compliance |
| `rudof_lib` / `shacl` | 0.3.35 | Apache-2.0 OR MIT | SHACL/ShEx validation | Adopt for closed-world shapes |
| `horned-owl` | 4.0.0 | LGPL-3.0 | OWL ontology model/parsers | License review required; static linking infects the binary |
| `sophia` | 0.10.0 | CECILL-B OR Apache-2.0 | RDF toolkit, RDFS entailment only | Not needed |
| `whelk` (crates.io) | 0.2.0 | (unrelated) | Unix shell | **Not an OWL reasoner** — do not use |
| `rustdl` | 0.4.42 | Apache-2.0 OR MIT (pulls horned-owl LGPL) | SROIQ effort, young | Empirically near-complete beyond EL/Horn; not production-proven |
| `ontologos` | 1.1.4 (Jul 2026) | Apache-2.0 OR MIT | Orchestration layer | Too young (~69 downloads/month) |

Conclusion: no production-grade, permissively licensed OWL 2 DL reasoner exists in Rust. Fallback is restricted `reasonable` RL plus a small deterministic closure module, or a JVM sidecar if DL is later proven necessary. The pilot does not need a reasoner at all — Option C computes closure in Rust.

## 9. Governance, versioning, and SynapticMesh

- **Two-tier ontology.** TBox core compiled into the binary as a versioned JSON-LD context (`src/schemas/memory-vocabulary.v4.json`, pinned in `src/traits.rs`), obeying the project's strict-JSON rule. Per-namespace SKOS/domain extension lives in the existing `domain_registry` row (`src/registry.rs`), extended to carry `ontology_version`, `subClassOf`, `altLabels`, and `exactMatch`.
- **Authoring.** Agents never write raw RDF/OWL. Hierarchy and aliases mutate through validated MCP parameters (`register_domain` with optional `parent_domain`/`aliases`) that reject cycles (DAG check) and normalize slugs via `normalize_domain_slug` (`src/domain.rs`).
- **Drift.** Additive aliases are minor changes. Changed equivalence, disjointness, or transitivity requires a major version, full rematerialization, regression queries, and atomic rollback.
- **SynapticMesh.** Local `domains` and `altLabels` stay `eligibility: private` (`src/registry.rs:50-57`). When a shareable memory crosses the mesh, stamp `metadata.concept_iri` on the exported record so receivers map it to their local slug. Imported ontologies start untrusted and activate only after validation/promotion. Same IRI with a different hash is a conflict, never last-write-wins.
- **Version pinning.** Pin `(vocabulary_version, ontology_version, content_hash)` in every derived overlay and evidence record. Option C computes expansion in-memory and Option A recomputes during `relink_edges`, requiring no destructive DB migration.

## 10. Pilot design (merged, falsifiable)

Run on the existing `NeuroStrata` namespace plus one other namespace. No prose NER. No persistent RDF.

**Three features only:**

1. SKOS/RDFS domain and alias expansion (Option C): upgrade `domain_registry` with a ~25-node hierarchy and alias map (e.g., `auth ⊃ tls ⊃ mtls`; `altLabels`: `{"lbug":"ladybugdb","fts":"tantivy","sas":"pre-push-gate","lock-2":"task-complete-extraction"}`). Expand `search_with_domain` by `subClassOf*` closure and expand query terms via `altLabels`.
2. Canonicalization of `fact.topology` entities and `KnownIds` aliases (light Option A).
3. Closed-world endpoint validation in `task_validate` / write-warn mode (SHACL-style).

**Dataset:** 60 owner-labeled queries — 20 alias/canonicalization, 15 exact identifiers, 15 type/hierarchy, 10 ambiguous/adversarial. Plus a subset of fixed-agent end-to-end answers.

**Ablation arms (same corpus, embedder, candidate limits, graph expansion):**

1. Current retrieval.
2. Deterministic exact ID/symbol lookup.
3. Arm 2 + flat alias dictionary.
4. Arm 2 + compiled SKOS/RDFS/restricted-RL closure.

**Acceptance:**

- Arm 4 beats arm 3, not merely baseline: ≥5% relative nDCG@10 improvement **or** ≥5 percentage-point Recall@5 gain; ≥10-point gain on ontology-targeted queries.
- Precision@5 drops ≤2 points; no cross-project false identity.
- p95 added latency ≤25 ms cached; ≤10% end-to-end; derived storage <5% of namespace.
- Repeated runs byte-identical; rollback restores baseline.
- Blind end-to-end answer accuracy improves without increased unsupported claims.

**Kill criteria:** no lift over flat aliases; precision regression; >50 ms p95 overhead; recurring ontology conflicts; or curation exceeding one hour per week. If flat aliases win, ship them without OWL.

## 11. Reassessment of the 2026-09-19 rejection

Memory `aa0651d3-d1f2-4ead-b309-bbfa56133777` recorded seven grounds. Status now:

1. **"LLM consumers read text, don't run DL reasoners."** Weaker. True that LLMs read text; false that reasoning doesn't help them — the retrieval engine decides which 5 primaries and expansions reach the context window. If a memory is missed due to a domain subclass mismatch or uncanonicalized alias, the LLM never sees the text.
2. **"Property-graph-to-RDF mapping + reasoner lifecycle costs weeks."** Weaker for Option C + A (days to weeks), still holds for Option B/D.
3. **"OWL open-world semantics mismatch record validation."** Still holds, 100%. OWL OWA cannot validate records. Keep closed-world (native Rust / SHACL).
4. **"A second RDF store violates the single-store rule."** Still holds for Option B only. Not an argument against C/A.
5. **"`memory_type` mixes record roles and structural artifacts, so no sound disjointness."** Still holds. v8 still mixes roles/artifacts and accepts arbitrary custom types. Separate dimensions are a prerequisite.
6. **"Scale (7 namespaces, single user) makes formal reasoning overhead pointless."** Weaker. Vocabulary v8 added procedure, topology, attempt, decision, domain_registry, staged generations, and SynapticMesh. Small scale is an advantage for in-memory RL closure over <10k nodes (microseconds), and multi-daemon mesh makes cross-namespace alignment a real problem.
7. **"Rust RDF crates: sophia 0.10 (RDFS only), oxrdf 0.3."** Nuanced. For RDFS/RL/SHACL, Rust is materially better (`reasonable`, `rudof_lib`/`shacl`). For OWL 2 DL, still unready for production. Reinforces choosing SKOS + RL + shapes over DL.

## 12. Open questions for the owner

1. Which of the three cases in §2 produced the prior OWL accuracy lift?
2. Can you share the prior ontology, the reasoner, and before/after queries?
3. Who curates the TBox/taxonomy per project — agents proposing edges during task completion, or gated registration?
4. Is cross-namespace federation an actual requirement today?
5. Are there examples requiring existential restrictions, consistency repair, or non-rule DL axioms? If yes, a JVM sidecar becomes justified and the design changes.

## 13. Source verification

| Claim | Source | Status |
|---|---|---|
| Domain filter exact equality + fallback to unfiltered | `src/store/ladybug.rs:147-211`, test `:8720` | Verified |
| One-hop `govern_out_q` (misses 2-hop chains) | `src/store/ladybug.rs:251-259` | Verified |
| Structural types fixed set of 5 | `src/store/ladybug.rs:1440-1446` | Verified |
| Custom types accepted; role enforcement descriptive | `src/schemas/memory-vocabulary.v3.json:205-213` | Verified |
| `domain_registry` private, never travels mesh | `src/registry.rs:50-57` | Verified |
| `export_database` is single-file checkpoint | `src/traits.rs:449-457` | Verified |
| `SearchEvidence` transient paths/explanations | `src/traits.rs:66-104` | Verified |
| Exact-identifier recall not implemented | `docs/COGNITIVE_ARCHITECTURE.md` §6 | Verified |
| `relink_edges` location | `src/store/ladybug.rs:2321` | Verified |
| oxigraph 0.5.13, horned-owl 4.0.0, oxrdf 0.3.5, reasonable 0.5.0-a1, shacl 0.3.35, sophia 0.10.0 | crates.io API, 2026-10-27 | Verified |
| `whelk` crates.io is an unrelated shell | crates.io API | Verified (Argon's claim corrected) |
| `rustdl` yanked owl-dl-cli 0.1.0-0.3.0 on 2026-09-04 | Argon/K3 research | Cited; not independently re-verified beyond seats |

## 14. Decision requested

Adopt the scoped pilot (§10) and file it as a concrete task, or reject OWL outright and invest the same effort in the deterministic exact-identifier matcher and real markdown ingestion, which need no model and have no governance cost. The pilot is the only way to test the owner's accuracy claim without committing to a DL reasoner or a second store.


## Owner decision (2026-10-27, superseded)

Prior decision ("No pilot. Stick to JSON. ... OWL-specific pilot and any reasoner/RDF work is retired.") is superseded by owner correction 2026-10-27: OWL/RDF, reasoners, and RDF stores are no longer categorically rejected. JSON remains the current implementation; an RDF/OWL migration is now a candidate strategic direction. See docs/design-oxirs-concepts.md. The JSON-surviving features (items 15-20) remain valid regardless.
