# NeuroStrata v1.19.0 Architectural Specification — Semantic Graph Reasoning, Epistemic Governance, Vault-LD Sync & Agentic Reflection

- **Status:** Implemented and verified in `v1.19.0` (capabilities A–G; Capability G retains its owner-approved honest retrieval FAIL and synthetic-only evidence boundary)
- **Spec Task:** `neurostrata-61dn`
- **Tracked Implementation Tasks:**
  - **Capability A (`neurostrata-s0l0`):** DataFrog-Backed OWL 2 RL Reasoning (`gtfierro/reasonable`) & Proof-Carrying Lean 4 Derivation Certificates
  - **Capability B (`neurostrata-miio`):** Pre-Commit Ontology Blast-Radius & Semantic Closure Diff (`ontology_plan`)
  - **Capability C (`neurostrata-50sm`):** Epistemic Trust Tiers (`generated_by` vs `verified_by`), Freshness SLAs (`stale_after`), & Governed Deprecation Annotations
  - **Capability D (`neurostrata-dlag`):** W3C `YAML-LD 1.0` / `Vault-LD` Bidirectional Markdown Knowledge Vault Sync & JSON Canvas 1.0 Topology
  - **Capability E (`neurostrata-mpau`):** Trajectory-to-Procedure Mining & Outcome-Verified Belief Reflection
  - **Capability F (`neurostrata-v6dw`):** Ontology-Scoped Bounded Classification Ingestion (Kerner bounded KG construction + Adamchic O-RAG)
  - **Capability G (`neurostrata-dyrq`):** Semantic & Retrieval Evaluation Gate (offline synthetic deterministic regression gate; not owner-labeled production pilot)
- **Upstream Foundation (`v1.19.0`):** `crates/ontobug` (`LadybugDB/ontobug`, `MIT OR Apache-2.0`), `rudof` `0.3.21` (`MIT OR Apache-2.0`), `oxigraph` `0.5.11` / `oxrdf` `0.3.4`, `LadybugStore` (`lbug` `0.22.3`), and `src/formal_kernel.rs` (Lean 4 formal verification kernel).

---

## 1. Executive Summary & Architectural Motivation

In `v1.19.0` (`neurostrata-wy2r`), NeuroStrata replaced its hand-rolled streaming Turtle SHACL parser with `crates/ontobug` (`LadybugDB/ontobug` + `rudof` `0.3.21`), adding W3C OWL/RDFS/SKOS class and property extraction, full W3C SHACL-Core and ShEx validation, and `LadybugSchemaStore` / `Catalog` property-graph schema generation over the shared in-process `Arc<Database>`.

Building on that foundation and the Autumn 2026 semantic-graph ecosystem survey (*Meaning, Traversal, Reasoning, Action: The Graph Stack for Agentic AI*), the `v1.19.0` implementation scope closes six architectural capability gaps across the **Meaning → Traversal → Reasoning → Action → Reflection → Classification** stack and adds an offline **Capability G** semantic/retrieval evaluation gate (`neurostrata-dyrq`) as the release regression contract for those capabilities, while adhering strictly to NeuroStrata's upstream-reuse policy (`MIT`, `Apache-2.0`, `BSD-3-Clause`).

| Capability | Tracked Task | Upstream Crate / Standard (`SPDX`) | Primary Codebase Touchpoints |
| :--- | :--- | :--- | :--- |
| **A. DataFrog OWL 2 RL Reasoner + Proof-Carrying Derivation Certificates** | `neurostrata-s0l0` | `gtfierro/reasonable` (`BSD-3-Clause`, `datafrog` + `oxrdf` `rdf-12`) + `src/formal_kernel.rs` (Lean 4) | `src/rdf/reasoning.rs`, `src/rdf/ontology.rs`, `src/formal_kernel.rs` |
| **B. Pre-Commit Ontology Blast-Radius & Semantic Closure Diff (`ontology_plan`)** | `neurostrata-miio` | `crates/ontobug` (`MIT OR Apache-2.0`) + `rudof` (`MIT OR Apache-2.0`) + `reasonable` (`BSD-3-Clause`) | `src/rdf/ontology.rs`, `src/rdf/scenario.rs`, `src/kernel/ports.rs`, `src/server.rs` |
| **C. Epistemic Trust Tiers, Freshness SLAs & Governed Deprecation Annotations** | `neurostrata-50sm` | `GoogleCloudPlatform/knowledge-catalog` OKF v0.2 (`Apache-2.0`), W3C `PROV-O`, `SHACL-Core` | `src/schemas/shacl/memory-shapes.v1.ttl`, `src/rdf/provenance.rs`, `src/rdf/taxonomy.rs`, `src/task/gate.rs` |
| **D. W3C `YAML-LD 1.0` / `Vault-LD` Bidirectional Markdown Vault Sync** | `neurostrata-dlag` | W3C `YAML-LD 1.0`, `The-Knowledge-Graph-Guys/vault-ld` (`Apache-2.0`), Open JSON Canvas `1.0` | `src/vault_ld/mod.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs`, `docs/features-api/ops/obsidian.md` |
| **E. Trajectory-to-Procedure Mining & Outcome-Verified Belief Reflection** | `neurostrata-mpau` | `src/judgment/laya_onnx.rs`, `src/learning/trainer.rs`, `src/task/mod.rs`, `src/rdf/action.rs` | `src/learning/trajectory.rs`, `src/learning/reflection.rs`, `src/task/mod.rs`, `src/rdf/action.rs` |
| **F. Ontology-Scoped Bounded Classification Ingestion** | `neurostrata-v6dw` | Kerner bounded KG construction + Adamchic O-RAG, `crates/ontobug` + `rudof` `0.3.21` (`MIT OR Apache-2.0`) | `src/rdf/ingest.rs` (new), `src/rdf/ontology.rs`, `src/server.rs` |
| **G. Semantic & Retrieval Evaluation Gate** | `neurostrata-dyrq` | `oxrdf`/`oxttl`/`spargebra`/`spareval`/`sparesults`, existing `LadybugStore::for_testing`, deterministic `BenchEmbedder` (offline, synthetic deterministic corpus) | `src/semantic_eval/mod.rs` (planned), `eval/semantic/competency.json` (planned), `eval/semantic/retrieval.json` (planned), `src/main.rs` (CLI, planned), `eval/registry.json` |

---

## 2. Capability A — DataFrog OWL 2 RL Reasoning & Proof-Carrying Derivation Certificates (`neurostrata-s0l0`)

### 2.1 Problem Statement
`src/rdf/reasoning.rs` (`ReasoningAdapter`) currently evaluates a bounded set of SPARQL 1.1 property-path queries (`rdfs:subClassOf*`, `rdfs:subPropertyOf*`, and bounded `ns:governs` propagation). While fast for basic hierarchy checks, it does not materialize the full W3C OWL 2 RL ruleset required for v1.19.0 (`owl:TransitiveProperty`/`prp-trp`, `owl:SymmetricProperty`/`prp-symp`, `owl:inverseOf`/`prp-inv`, `owl:equivalentClass`/`cax-eqc`, `owl:equivalentProperty`/`prp-eqp`, `owl:propertyChainAxiom`/`prp-spo2` including arbitrary-length chains, `rdfs:domain`/`prp-dom`, `rdfs:range`/`prp-rng`, `rdfs:subClassOf`/`cax-sco`, `rdfs:subPropertyOf`/`prp-spo1`, relevant restriction rules such as `owl:hasValue`/`cls-hv`, `owl:someValuesFrom`/`cls-svf`, `owl:allValuesFrom`/`cls-avf`, and consistency/disjointness violations such as `owl:disjointWith`/`cax-dw` and `owl:complementOf`/`cls-com` where applicable) and cannot emit machine-checkable proof trees explaining *why* an inferred governance or dependency edge holds.

### 2.2 Upstream Library Selection
We adopt **`gtfierro/reasonable`** (`BSD-3-Clause`), a Rust OWL 2 RL reasoner built on Rust's `datafrog` Datalog engine and `oxrdf` (with `rdf-12` / RDF-star support and arbitrary-length `prp-spo2` property chains). `reasonable` performs **RDFS/OWL-RL DataFrog materialization** for the supported ruleset; NeuroStrata is responsible for sound, independently verified derivation certificates over that materialization. Full supported OWL-RL materialization and certification are required (not optional) in this capability.

### 2.2a Active Reasoning Profile Reporting
Every materialization invocation reports the active reasoning profile in `DerivationCertificate.profile` and in production summaries (`neurostrata_scenario` / `neurostrata_get_snapshot`) so an operator can tell which rule set produced an entailment.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningProfile {
    // Metadata/profile label only: bundled materialization still runs full
    // OWL-RL (profile_restricts_closure=false). Never a restricted-closure claim.
    Rdfs,
    OwlRl, // full supported DataFrog OWL 2 RL materialization + certified derivations
}
```

### 2.3 Data Structures & API Design (`src/rdf/reasoning.rs`)

```rust
/// Identifies the W3C OWL 2 RL rule that fired to produce an inferred triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owl2RlRuleId {
    /// prp-dom: (?p rdfs:domain ?c) ^ (?x ?p ?y) => (?x rdf:type ?c)
    PrpDom,
    /// prp-rng: (?p rdfs:range ?c) ^ (?x ?p ?y) => (?y rdf:type ?c)
    PrpRng,
    /// prp-trp: (?p rdf:type owl:TransitiveProperty) ^ (?x ?p ?y) ^ (?y ?p ?z) => (?x ?p ?z)
    PrpTrp,
    /// prp-symp: (?p rdf:type owl:SymmetricProperty) ^ (?x ?p ?y) => (?y ?p ?x)
    PrpSymp,
    /// prp-inv1 / prp-inv2: (?p1 owl:inverseOf ?p2) ^ (?x ?p1 ?y) => (?y ?p2 ?x)
    PrpInv,
    /// prp-spo1: (?p1 rdfs:subPropertyOf ?p2) ^ (?x ?p1 ?y) => (?x ?p2 ?y)
    PrpSpo1,
    /// prp-spo2: (?p owl:propertyChainAxiom (?p1 ... ?pn)) ^ chain => (?x ?p ?z)
    PrpSpo2,
    /// prp-eqp1 / prp-eqp2: (?p1 owl:equivalentProperty ?p2) ^ (?x ?p1 ?y) => (?x ?p2 ?y)
    PrpEqp,
    /// cax-sco: (?c1 rdfs:subClassOf ?c2) ^ (?x rdf:type ?c1) => (?x rdf:type ?c2)
    CaxSco,
    /// cax-eqc1 / cax-eqc2: (?c1 owl:equivalentClass ?c2) ^ (?x rdf:type ?c1) => (?x rdf:type ?c2)
    CaxEqc,
    /// cls-hv1 / cls-hv2: owl:hasValue inference
    ClsHv,
    /// cls-svf1 / cls-svf2: owl:someValuesFrom inference
    ClsSvf,
    /// cls-avf: owl:allValuesFrom inference
    ClsAvf,
    /// cax-dw: (?c1 owl:disjointWith ?c2) ^ (?x rdf:type ?c1) ^ (?x rdf:type ?c2) => Bottom
    CaxDwViolation,
}

/// A single derivation step in a proof-carrying inference DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivationStep {
    pub rule: Owl2RlRuleId,
    /// Canonical N-Triple strings for the premise triples that triggered `rule`.
    pub premises: Vec<String>,
    /// Canonical N-Triple string for the entailed conclusion triple.
    pub conclusion: String,
}

/// Complete proof certificate for a materialized closure pass, suitable for
/// verification by the Lean 4 formal kernel (`src/formal_kernel.rs`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DerivationCertificate {
    pub namespace: String,
    pub profile: ReasoningProfile,               // rdfs | owl-rl
    pub base_triple_count: usize,
    pub inferred_triple_count: usize,
    pub steps: Vec<DerivationStep>,
    pub disjointness_violations: Vec<DerivationStep>,
    /// Deterministic SHA-256 digest over canonicalized profile + steps.
    pub certificate_sha256: String,
}
```

### 2.4 Execution & Formal Kernel Verification Flow (required, no uncertified output)
1. **Inputs are explicit base triples (no implicit store field).** `ReasoningAdapter` remains a store-free in-memory adapter (no store handle). Materialization accepts explicit base triples (e.g. `base_triples: &[oxrdf::Triple]` or canonical N-Triple inputs converted once at the boundary), `namespace`, `max_inferred_triples` (default `50_000`), and `profile`.
2. **Narrow store integration parameter (not a store-backed no-arg `&self` signature).** Live namespace callers (ontology/scenario/snapshot paths) assemble the authoritative namespace base graph and pass it in via a narrow parameter/view (e.g. `ReasoningStoreView<'a> { store: &'a crate::store::ladybug::LadybugStore, namespace: &'a str }` used by `OntologyIndex`/scenario helpers to load base triples) rather than adding store state to `ReasoningAdapter`.
3. **Materialization engine:** `reasonable` performs DataFrog RDFS/OWL-RL materialization over the provided base triples for the selected profile. `max_inferred_triples` is enforced to prevent combinatorial blowup.
4. **Deterministic proof reconstruction/auditing (mandatory):** because `reasonable 0.5.0-a1` does not emit derivation events, NeuroStrata deterministically reconstructs and independently verifies a sound derivation for **every returned inferred triple** by auditing over the `base + materialized` triple sets against supported OWL 2 RL rule patterns (including `prp-trp`, `prp-inv`, `prp-spo2`/property chains, `prp-dom`/`prp-rng`, `cax-sco`, `prp-spo1`, `prp-eqp`/`cax-eqc`, relevant restriction rules `cls-hv`/`cls-svf`/`cls-avf`, and violations such as `cax-dw` where applicable). Each reconstructed `DerivationStep` records canonical premises and conclusion sufficient for independent verification.
5. **Fail-closed per triple/rule, never uncertified output:** if any individual inferred triple/rule derivation cannot actually be established by deterministic reconstruction/auditing, the operation fails closed for that result (error identifying the uncertifiable triple/rule) and **does not return uncertified inferred output**. `ProofUnavailable` is not the normal OWL-RL path and must not be used as a blanket substitute for certification; it may appear only for a concretely unestablishable derivation case where certification cannot be produced for a returned triple (i.e. treated as a fail-closed rejection, not a degraded success).
6. **All ship together:** OWL-RL materialization, derivation certificates, Lean verification, named-graph separation, and RDF-star annotations are all required deliverables. Inferred triples are isolated to `https://neurostrata.io/ns/graph/inferred#<namespace>` so explicit ABox/TBox assertions are never conflated with entailments. RDF-star/provenance annotations (`<< ?s ?p ?o >> ns:derivedByRule ?ruleId ; ns:certificateHash ?sha256`) are emitted only from verified/certified steps.
7. **Formal verification:** `src/formal_kernel.rs::verify_derivation_certificate` independently verifies the certificate (profile, canonicalized `profile + steps` digest, and machine-checkable rule steps/premise-conclusion validity) and rejects unverifiable or digest-mismatched certificates. No fabricated premises/conclusions.

---

## 3. Capability B — Pre-Commit Ontology Blast-Radius & Semantic Closure Diff (`ontology_plan`, `neurostrata-miio`)

### 3.1 Problem Statement
When an agent or domain architect modifies a TBox class hierarchy (`rdfs:subClassOf`), adds an `owl:propertyChainAxiom`, updates a SKOS concept's `disjoint_with` list, or tightens a SHACL shape, the mutation can silently invalidate existing memories, alter kinetic action `SparqlAsk` preconditions, or expand file governance (`ns:governs`) across hundreds of source files. Today, SHACL checks run *after* or *during* individual row writes, without showing the full graph-wide semantic closure diff beforehand.

### 3.2 Architecture & Operation (`ontology_plan`, zero-mutation)
We introduce a zero-mutation, sandboxed dry-run operation exposed on `OntologyIndex::plan_mutation_diff`, `OntologyPort`, and `neurostrata_scenario(operation: "plan_ontology_diff")` and wired end-to-end through the shared scenario path (not a docs-only type). **Implementation status:** `crates/ontobug/src/diff.rs` and `crates/ontobug/src/lib.rs` publicly provide `diff_ontologies`, `generate_cypher_ddl`, and `OntologyDiff`. Capability B production integration is now wired end-to-end: `OntologyIndex::plan_mutation_diff` (zero-mutation ephemeral sandbox), read-only `OntologyPort::plan_ontology_diff` with the Ladybug implementation, the shared `rdf::scenario::plan_ontology_diff`, MCP `neurostrata_scenario(operation: "plan_ontology_diff")` schema + dispatch, generic `/scenario` HTTP parity through the existing handler, and CLI `scenario plan-ontology-diff`. Evidence (this session): `cargo check --locked --bin neurostrata-mcp` clean; `cargo test --locked --bin neurostrata-mcp rdf::ontology::plan_tests` 5 passed; `rdf::scenario::` 3 passed; `scenario_plan_ontology_diff_dispatches_zero_mutation_report` passed; ontobug baseline `cargo test -p ontobug` passed. Task stays open pending independent review.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyPlanRequest {
    pub namespace: String,
    /// Optional overlay scenario ID; if omitted, clones the active namespace RDF dataset.
    pub scenario_id: Option<String>,
    /// Turtle / N-Quads additions to stage in the ephemeral sandbox.
    pub add_turtle: Option<String>,
    /// Canonical N-Triple / N-Quad patterns to retract in the ephemeral sandbox.
    pub retract_triples: Vec<String>,
    /// Optional updated SHACL shapes Turtle document to test against the post-image.
    pub candidate_shacl_turtle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyPlanReport {
    pub namespace: String,
    /// Newly entailed triples under selected closure/profile (absent from pre-image closure).
    pub added_entailments: Vec<DerivationStep>,
    /// Previously entailed triples that no longer hold after the mutation.
    pub removed_entailments: Vec<String>,
    /// SHACL/ShEx validation report (`rudof`) over the post-image graph.
    pub validation_conforms: bool,
    pub new_shacl_violations: Vec<ShaclViolationSummary>,
    /// Disjointness (`cax-dw`) or SKOS hierarchy cycle violations introduced.
    pub consistency_violations: Vec<String>,
    /// Downstream impact: memory IDs, file paths (`GOVERNS`), tasks, and `ActionTypeDefinition` slugs whose governance or `SparqlAsk` evaluation changes.
    pub impacted_memories: Vec<String>,
    pub impacted_files: Vec<String>,
    pub impacted_action_slugs: Vec<String>,
    /// LadybugDB DDL diff produced by ontobug helpers `diff_ontologies` + `generate_cypher_ddl` (implemented and verified). Empty only when those helpers compute no DDL changes for the staged mutation.
    pub schema_ddl_diff: Vec<String>,
}
```

**Production wiring contract (implemented; no "DDL diff pending implementation" escape):**
- `OntologyIndex::plan_mutation_diff` clones the active namespace (or merges the named scenario overlay state) into an **ephemeral** `oxigraph::store::Store`/sandbox dataset, applies `add_turtle`/`retract_triples`, computes pre/post closures and deltas via `ReasoningAdapter` with explicit base-triple inputs (Capability A fail-closed contract), validates candidate shapes/data via `ontobug::validate::shacl_turtle` (`rudof`, existing), and computes `schema_ddl_diff` via `crates/ontobug` helpers `diff_ontologies` + `generate_cypher_ddl` (implemented and verified, including parser/order/metadata, idempotent MERGE, graph scoping, escaping, and modified-annotation fixes). `schema_ddl_diff` is produced from those helpers when integrated; it must not be replaced by a placeholder/"pending implementation" output.
- **Zero mutation:** never commits to live store/scenario rows/ontology index cache state/tasks. No `commit`, no `put_scoped_memory`, no edge writes, no task mutation.
- **`OntologyPort` wiring:** `src/kernel/ports.rs` exposes the read-only plan method (e.g. `async fn plan_ontology_diff(&self, namespace: &str, request: &OntologyPlanRequest, principal: Option<&str>) -> Result<OntologyPlanReport>`) on `OntologyPort` with `LadybugOntologyPort` implementation delegating to ontology/scenario planning logic under ReBAC read visibility for impacted entities.
- **`neurostrata_scenario(plan_ontology_diff)` wiring:** `src/rdf/scenario.rs` implements/dispatches the zero-mutation plan operation (separate from mutating scenario actions), `src/server.rs` adds MCP schema+dispatch for `neurostrata_scenario` operation `"plan_ontology_diff"`, `src/daemon.rs` routes it through the existing scenario HTTP path (`/scenario`/scenario handler family) to the same shared implementation, and `src/main.rs` exposes CLI parity. All surfaces share identical semantics and request/response types.

### 3.3 Integration with `crates/ontobug`
Structural class/property diffing and LadybugDB DDL generation use the implemented `crates/ontobug` helpers `diff_ontologies` and `generate_cypher_ddl` (`OntologyDiff` result) from `crates/ontobug/src/diff.rs` and `src/lib.rs`, integrated and verified in v1.19.0. Production `ontology_plan` integration consumes them for `schema_ddl_diff`. Constraint validation uses `ontobug::validate::shacl_turtle`; entailment delta uses Capability A's fail-closed reasoning/certificate contract (no fabricated proof steps).

---

## 4. Capability C — Epistemic Trust Tiers, Freshness SLAs & Governed Deprecation Annotations (`neurostrata-50sm`)

### 4.1 Problem Statement
In long-lived agentic repositories, not all memories carry equal epistemic authority: an unverified hypothesis inferred by a subagent during debugging should not rank equally with a rule proven by a passing test suite, checked by the Lean 4 formal kernel, or approved by the repository owner. Furthermore, architectural facts about external APIs, cloud schemas, or volatile modules decay over time unless governed by an explicit freshness SLA, and deprecated concepts/rules need structured negative boundary definitions (`not_definition`) and machine-readable `SUPERSEDED BY` pointers so retrieval surfaces never serve stale guidance as authoritative.

### 4.2 Design Informed by Open Knowledge Framework (OKF v0.2, `Apache-2.0`)
Adapting the provenance, freshness, and deprecation governance patterns from `GoogleCloudPlatform/knowledge-catalog` (`Apache-2.0`) into NeuroStrata's W3C RDF/SHACL and SKOS schemas:

1. **Separated Epistemic Provenance (`generated_by` vs `verified_by`) & Trust Tiers (`EpistemicTier`):**
   - `metadata.generated_by` (`ns:generatedBy`): The actor/process that authored the memory (e.g., `"agent:ses_..."`, `"ast-ingestor"`, `"trajectory-miner"`, `"human:owner"`).
   - `metadata.verified_by` (`ns:verifiedBy`): Independent verification evidence (e.g., `"cargo-test:<test_name>@<commit>"`, `"formal-kernel:<cert_sha256>"`, `"action-receipt:<id>"`, `"human:owner"`).
   - `metadata.epistemic_tier` (`ns:epistemicTier`) — **exact tier vocabulary (normative):**
     - `unverified_agent` (default when `verified_by` is absent and `generated_by` is an agent session)
     - `empirical_verified` (`verified_by` cites a passing test, command receipt, or kinetic action receipt)
     - `formal_verified` (`verified_by` cites a Lean 4 formal kernel / OWL 2 RL derivation certificate)
     - `human_governed` (`verified_by` cites an authenticated owner/admin principal)
   - **Invariant enforced in `src/schemas/shacl/memory-shapes.v1.ttl` and `src/rdf/provenance.rs`:** A memory cannot claim `empirical_verified`, `formal_verified`, or `human_governed` unless `metadata.verified_by` is non-empty and distinct from an unverified self-assertion (`generated_by != verified_by` for autonomous agent claims). **Embedded SHACL inventory remains eight files total:** modify `src/schemas/shacl/memory-shapes.v1.ttl` (no ninth shape file).

2. **Freshness SLAs (`metadata.stale_after` / `ns:staleAfter`):**
   - Optional `stale_after` value in **RFC3339/ISO-8601 string** form or **Unix epoch seconds** (integer). Parsing/normalization and comparison live in production helpers (e.g. `src/rdf/provenance.rs` and/or task/gate/snapshot surfaces) so both forms are accepted deterministically.
   - `neurostrata_get_snapshot` and `neurostrata_task_validate` surface an **advisory** `stale_memories` list whenever active rows have `stale_after < now`. Advisory means it is surfaced for re-verification/supersession and does not by itself block pushes unless a separate strict policy explicitly requires it; output must label it advisory.

3. **Governed Deprecation Annotations & Negative Boundary Definitions (`not_definition`):**
   - Extending `src/rdf/taxonomy.rs` (`ConceptRecord`/concept metadata) and `memory_type = "rule" | "fact"` metadata with:
     - `metadata.not_definition` (`ns:notDefinition`): Explicit negative scope boundary stating what a concept or rule is *not* (disambiguating near-neighbor concepts for both LLM prompts and contradiction checks).
     - `metadata.deprecation` (`{ status: "deprecated", superseded_by: "<id>", reason: "...", effective_at: <ts> }`): When a memory or SKOS concept is superseded/deprecated, retrieval surfaces emit a deterministic banner: `[DEPRECATED — SUPERSEDED BY <id>: <reason>]`.
   - **Production surfaces for banner and `not_definition` (not helper-only):** `neurostrata_search_memory`, `neurostrata_memories_for_file`, `neurostrata_related`/graph recall where applicable, `neurostrata_get_snapshot`, `neurostrata_task_validate` and task views that present governing memories, and `neurostrata_taxonomy` briefing/compare/gaps/advise outputs must expose `not_definition` in structured/rendered payloads and attach/prepend the deprecation banner deterministically for deprecated rows/concepts. Shared formatting belongs in production modules (e.g. `src/rdf/provenance.rs`, `src/rdf/taxonomy.rs`, server/task presentation paths) invoked by those surfaces.

---

## 5. Capability D — W3C `YAML-LD 1.0` / `Vault-LD` Bidirectional Markdown Knowledge Vault Sync (`neurostrata-dlag`)

### 5.1 Problem Statement & Resolution of `neurostrata-dlag`
Task `neurostrata-dlag` tracks the gap where earlier documentation described an Obsidian / NeuroVault bidirectional Markdown sync that had not yet been implemented in `src/`. The Autumn 2026 semantic-graph ecosystem established **W3C `YAML-LD 1.0`** and **`The-Knowledge-Graph-Guys/vault-ld`** (`Apache-2.0`) as the open standard for treating a Markdown vault's YAML frontmatter and `[[WikiLinks]]` as a dual-compiled W3C Linked Data (JSON-LD / RDF) knowledge graph. We resolve `neurostrata-dlag` via Path (A) by implementing `src/vault_ld/mod.rs` using W3C `YAML-LD 1.0` conventions and Open JSON Canvas `1.0`.

### 5.2 Dual-Compiled `YAML-LD 1.0` Frontmatter Schema
Every exported Markdown file (`<vault_dir>/<strata>/<memory_type>/<slug-or-id>.md`) is simultaneously a clean, human-readable Markdown note for Obsidian/Logseq/Marksman and a deterministic W3C `YAML-LD 1.0` document:

```markdown
---
"@context": "https://neurostrata.io/ns/yaml-ld/v1.jsonld"
"@id": "urn:neurostrata:memory:494a50ec-9333-49b8-b318-b7567f23e83f"
"@type": "ns:RuleMemory"
id: "494a50ec-9333-49b8-b318-b7567f23e83f"
namespace: "NeuroStrata"
memory_type: "rule"
strata: "domain"
domain: "architecture"
epistemic_tier: "human_governed"
valid_from: 1791706134
valid_to: null
canonical_sha256: "f40b9c631f69ac01518e97d0bfdc509161d229be739327604deb1353ae80607a"
governs:
  - "[[NeuroStrata::src/server.rs]]"
  - "[[NeuroStrata::docs]]"
relates_to:
  - "[[463b7dd6-4434-4448-824c-7d7dc30f20d1]]"
extracted_from:
  - "[[neurostrata-1pf7]]"
---

# Rule: Identifier Discipline

Always quote commit hashes, memory IDs, and task IDs directly from tool output.
```

### 5.3 Bidirectional Sync & Safety Contract (`src/vault_ld/mod.rs`)
1. **Shared implementation & exact production naming:** MCP tool `neurostrata_vault_sync`, CLI command `vault-sync`. A single shared `src/vault_ld` implementation is invoked by `src/server.rs` (tool schema + dispatch/handler), `src/daemon.rs` (HTTP route/handler in the daemon server), and `src/main.rs` (CLI subcommand). No remote vault context fetching or URL ingestion.
2. **Caller-Path Authority (`J6`):** Any target vault directory passed to `neurostrata_vault_sync` / `vault-sync` is canonicalized via `check_caller_path` (`src/daemon.rs`) and must reside inside the project root or `allowed_external_dirs`.
3. **Export (`Database → Vault`):**
   - Queries active (`valid_to IS NULL`) memories, SKOS concepts (`thesis`, `concept`), and tasks for `<namespace>`, filtered by `rebac::can_read_memory`.
   - Renders each record to a deterministic `YAML-LD 1.0` Markdown file and writes an Open JSON Canvas `1.0` spatial map (`<vault_dir>/topology.canvas`) grouped by Tri-Strata tier, Leiden community (`src/community.rs`), and governed source directories.
4. **Import (`Vault → Database`, strict/fail-closed):**
   - Parses modified `.md` files, maps `YAML-LD 1.0` frontmatter + `[[WikiLinks]]` into RDF quads, and runs **secret scanning (`src/guard/`) and `ontobug::validate::shacl_turtle` (`rudof` W3C SHACL-Core) before any store write**. Any secret finding or SHACL violation fails closed with zero writes for rejected data.
   - Unresolved `[[WikiLinks]]` are minted as queryable `stub:true` reference nodes in the RDF graph rather than silently dropped.
   - Preserves bitemporal audit history: edits to an existing note's body or frontmatter invoke `supersede_memory` (or create a new bitemporal version under `LadybugStore::write_lock`) rather than destructively mutating historical versions in place.
   - Honours SynapticMesh sovereignty and access control: private rows (`eligibility: "private"`), mesh-owned foreign records, and ReBAC-disallowed targets cannot be silently clobbered.

### 5.4 Implemented integration contract (Capability D, neurostrata-dlag)
- **Core:** implement/complete `src/vault_ld/mod.rs` (and submodules as needed) as the shared sync engine: YAML-LD frontmatter parse/render, `[[WikiLinks]]` mapping, diff detection, export, import-to-RDF, canvas generation, `stub:true` minting, and bitemporal supersession.
- **Wiring (all required):** `src/server.rs` registers `neurostrata_vault_sync` in tool listing/schema and dispatches to shared handler; `src/daemon.rs` adds HTTP route/handler delegating to same shared handler; `src/main.rs` adds `vault-sync` CLI command/subcommand delegating to same shared handler. Use existing `check_caller_path`, guard secret scan, `ontobug::validate::shacl_turtle` / ontology validation, `rebac::can_read_memory` (and write checks), and store write-lock/supersession APIs.
- **Concrete targets:** `src/vault_ld/mod.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs`, `docs/features-api/ops/obsidian.md` (and API/CLI docs if surfaced by implementation conventions).
- **Focused tests:** `cargo test --locked --bin neurostrata-mcp vault_ld::` plus handler wiring tests as needed; cover export determinism/canvas, import supersession, unresolved wiki links → `stub:true`, secret fail-closed, SHACL fail-closed before write, `J6` path rejection, ReBAC/privacy/SynapticMesh non-clobber. No full-workspace suite. No test claims without evidence.

---

## 6. Capability E — Trajectory-to-Procedure Mining & Outcome-Verified Belief Reflection (`neurostrata-mpau`)

### 6.1 Problem Statement
Agents repeatedly execute multi-step workflows (recorded across task history notes, Episodic Buffer logs, and bitemporal `action_receipt` rows), yet procedural habits (`memory_type = "procedure"`) today must be authored manually via `neurostrata_add_memory`. Conversely, when an existing rule or procedure leads to a reverted kinetic action (`neurostrata_action(revert)`) or a failed attempt (`metadata.attempt.outcome == "failed"`), its retrieval salience and confidence are not automatically penalized or flagged for revision.

### 6.2 Architecture (`src/learning/trajectory.rs` & `src/learning/reflection.rs`)

1. **Deterministic Trajectory-to-Procedure Miner (`src/learning/trajectory.rs`):**
   - Inspects completed tasks (`status == "done"`), their `EXTRACTED_FROM` memories, `action_receipt` chains, and `.NeuroStrata/sessions/*.md` episodic sequences.
   - Identifies recurring ordered n-grams of `(trigger_context, tool_or_action_sequence, verification_gate)` that appear across $\ge 3$ completed tasks with a $100\%$ non-reverted completion rate.
   - **Pure miner core (deterministic, side-effect free):** computes and returns candidate payloads/structs (procedure candidates and optional candidate `ActionTypeDefinition` payloads) without store writes; unit-testable in isolation.
   - Synthesizes candidate `memory_type = "procedure"` payloads with valid SHACL metadata (`trigger`, `remaining_fires: 10`, `performance_count: 0`, `generated_by: "trajectory-miner"`, `verified_by: "task-receipts:<id1>,<id2>,<id3>"`, `epistemic_tier: "empirical_verified"`), deduplicating against existing procedures via the local Laya ONNX judge (`src/judgment/laya_onnx.rs`) with fail-closed behavior when safety cannot be established (neurostrata-mpau).
   - **Candidate `ActionTypeDefinition`s:** emitted as candidates only. Any registration/persistence must pass the existing action define gate in `src/rdf/action.rs` (`ActionTypeDefinition::from_payload` / `validate_definition` / `define_action` path including slug/schema/budget checks and namespace uniqueness semantics). **Never auto-applied**; no `neurostrata_action(apply)` side effects at mining/proposal time.

2. **Outcome-Verified Belief Reflection (`src/learning/reflection.rs`):**
   - **Positive Reinforcement:** When `neurostrata_task_complete` succeeds or a kinetic `apply` receipt remains un-reverted across a completed task, governing rules and fired procedures referenced in the task's `session_queries` / `subagent_prompt_injection` receive a bounded Bayesian reliability increment (`metadata.outcome_stats: { successes, reverts, failures, reliability_score }`).
   - **Negative Reflection on Revert / Failure:** When `neurostrata_action(action: "revert")` succeeds or a negative exemplar (`metadata.attempt.outcome == "failed"`) is recorded, `reflection::on_negative_outcome` decrements reliability for the governing/motivating rule/procedure and queues a Laya contradiction check (`src/judgment/laya_onnx.rs`) plus a `stale_after` review flag so the next `neurostrata_get_snapshot` prompts refinement or supersession.
   - **Pure reflection core + production hooks:** scoring/flag computation is pure; store updates/supersessions/review flags/candidate persistence occur only through production hook integrations.

### 6.3 Actual hook integration contract (Capability E)
- **Task completion hook (actual):** integrate in `src/task/mod.rs` completion finalization (`task_complete` / `handle_task_complete` path and any shared completion helper) **after** Lock 2 extraction requirements are satisfied, passing namespace, task id, session/query evidence, governing memory ids, and outcome context to reflection/miner entrypoints. Candidate persistence (if enabled) must be deterministic, ReBAC/privacy-respecting, and go through validated define/store paths only.
- **Action revert hook (actual):** integrate in `src/rdf/action.rs::revert_action` at successful revert-commit points (not on rejected/failed revert attempts, not on dry validation paths) to invoke reflection for the reverted receipt's motivating/governing targets; revert-receipt metadata must be available for idempotent outcome accounting.
- **Non-goals at hook time:** no automatic action application, no bypass of `neurostrata_task_complete`/Lock 2, no unvalidated action-type definitions. All mined action-type candidates remain define-gated and inert until explicitly defined/applied through existing mechanisms.

---

## 6.5 Capability F — Ontology-Scoped Bounded Classification Ingestion (`neurostrata-v6dw`)

### 6.5.1 Problem Statement
Long-lived agentic repositories need domain-scoped classification: agents should classify incoming memories or findings only against governed SKOS/OWL classes and predicates from the relevant domain subgraph, never inventing classes/predicates that are not part of the governing ontology.

### 6.5.2 Design & Implementation (strict, production-wired)
- Add `src/rdf/ingest.rs` bounded classification ingestion using Kerner bounded KG construction and Adamchic O-RAG over the **relevant governed domain subgraph** (SKOS/OWL classes, predicates, constraints) loaded from ontology/index state for the requested namespace + domain/vocabulary scope.
- **New bounded ingestion is always strict (no opt-out):** no permissive flag may bypass ontology bounds or SHACL validation on this path.
- **Empty domain/vocabulary fails closed:** if the resolved governed domain subgraph, allowed class set, or allowed predicate/vocabulary set is empty/missing for scope, ingestion returns a validation error and performs **zero writes**.
- **Whole-batch validation before any writes:** classify/construct/validate all candidate quads and payload projections for the batch (including invented-term checks and SHACL/post-image constraints) before committing; on failure, no partial writes.
- Reject invented classes/predicates not present in the governed vocabulary.
- Wrap ingestion in ontobug/rudof SHACL fail-closed validation (via `ontobug::validate::shacl_turtle` / `ontobug::validate::shex` where applicable and `OntologyIndex::validate_rdf_graph` / strict post-image validation) **before** writes.
- Emit `task_intent_signals`/`task_intent` metadata on resulting memories for tracking.

### 6.5.3 Implemented integration contract (Capability F, neurostrata-v6dw)
- **Core:** implement production logic in `src/rdf/ingest.rs` (vocabulary/domain resolution, bounded classification, candidate graph construction, batch validation, atomic commit).
- **Production handler exposure:** expose the capability through a real production handler/surface (MCP tool/operation schema + dispatch in `src/server.rs`, and daemon/CLI parity if included in this release scope) rather than an unreachable module-only API.
- **Concrete targets:** `src/rdf/ingest.rs`, `src/rdf/ontology.rs` (vocabulary/subgraph helpers as needed), `src/server.rs` (+ `src/daemon.rs`/`src/main.rs` if parity wired), and focused tests.
- **Focused tests:** `cargo test --locked --bin neurostrata-mcp rdf::ingest::` plus handler tests as applicable; must cover strict bounded acceptance, empty domain/vocabulary fail-closed, invented class/predicate rejection, SHACL fail-closed, and whole-batch validation with no partial writes. No full-workspace suite. No test claims without evidence.

---

## 7. Capability G — Semantic & Retrieval Evaluation Gate (`neurostrata-dyrq`)

### 7.1 Purpose and evidence boundary (normative)

`neurostrata-dyrq` defines an offline, deterministic, fail-closed release regression gate for semantic competency and retrieval behavior over a versioned **synthetic deterministic** corpus. It is **not** an owner-labeled production pilot and **not** the `owl-re-evaluation.md` §10 pilot design.

**Explicit distinctions (non-claims):**
- This gate makes **no** production ROI claim (`production_roi_claimed` MUST be `false`).
- This gate makes **no** owner-labeled corpus claim, **no** blind agent-answer accuracy claim, **no** rollback claim, **no** curation-cost claim, and **no** real-namespace claim.
- Labels such as `owner_labeled_production_roi`, `blind_agent_answer_accuracy`, `owner_labeled_corpus`, `rollback_validation`, `curation_cost`, and `real_namespace_accuracy` MUST be absent from pass/fail assertions and MUST appear only under `not_evaluated` (or equivalent explicit exclusion fields) in the receipt. They MUST NOT be silently claimed by fixture names, wording, or a passing gate.
- The bundled fixture/corpus is synthetic only (`urn:neurostrata:eval:*` or equivalent synthetic identifiers; no private data; no external files/downloads; no assertions copied from the live global database).

### 7.2 Offline ASK/SELECT competency fixtures (normative)

- **Fixture form:** strict JSON fixture(s) with `schema_version = 1`, stable `fixture_id`, `reasoning_profile`, bounded `dataset_nquads`, and `queries[]`. Unknown fields MUST be rejected (`deny_unknown_fields` / equivalent strict parsing).
- **Reasoning profile:** competency execution uses the canonical `owl-rl` reasoning profile for RDFS/OWL-RL materialization cases as specified by Capability A contracts; the spec/receipt MUST NOT claim that an `Rdfs` profile restricts rules beyond what is explicitly implemented and evidenced. Profile and materialization evidence/certificate digest MUST be reported in the receipt when reasoning is exercised.
- **Query forms:** only offline `ASK` and `SELECT` competency questions are permitted. `SERVICE`, `CONSTRUCT`, `DESCRIBE`, `DELETE`, and `INSERT` MUST be rejected before evaluation (fail-closed; no HTTP/service handler constructed).
- **Execution/evaluator contract:** parse N-Quads via `oxttl::NQuadsParser` (bounded/default-graph-only materialization input as required by fixture); for RDFS/OWL-RL cases call the certified materialization path with bounded inferred-triple cap and include only certified inferred triples; parse SPARQL via the existing parser/prefix path, permit only `spargebra::Query::Ask` and `Select`, inspect and reject `SERVICE`; evaluate offline with bounded controls (including algebra-node, dataset-quad, solution, and timeout budgets) and fail closed on truncation, timeout, malformed RDF/SPARQL, wrong result form, wrong expected bindings, duplicate query ids, or unknown fixture fields.
- **Custom functions / time:** competency evaluation MUST use production custom functions with empty metadata and fixed `now` (deterministic). No wall-clock timestamps or latency fields in the competency receipt.
- **Expected results:** `ASK` compares boolean value; `SELECT` compares a canonical, sorted multiset of complete bindings preserving RDF term type/value/datatype/language (not display-string-only comparison) and duplicate solutions. Partial output is never compared as success.
- **Coverage/content requirements (minimum):** versioned competency fixture includes at least eight named questions with source-grounded rationale covering direct type assertion present/absent, RDFS subclass entailment, multi-level subclass `SELECT` with exact expected bindings, OWL inverse relation, OWL transitive relation, SKOS preferred/alternative label `SELECT` preserving literal values, and a negative cross-identity assertion proving two distinct resources are not silently merged. All resources are synthetic. Competency keeps its own expected ASK/SELECT fixtures and negative assertions (no unspecified `curation_notes` DTO field).
- **Determinism/fail-closed:** repeated execution produces the same canonical competency receipt bytes/digest (excluding non-canonical incidental fields, none permitted). Any violation, budget breach, uncertified/malformed materialization, or mismatch fails the fixture with query-specific diagnostics.

### 7.3 Retrieval ablation corpus and arms (normative)

- **Corpus:** strict versioned retrieval fixture with `schema_version = 1`, `evidence_class = "synthetic_deterministic"`, fixed `dimensions = 64` (bundled fixture), documents, queries, `flat_aliases`, and `domains`. Unknown fields MUST be rejected.
- **Distribution:** exactly **60 queries total** — **20 alias/canonicalization**, **15 exact identifier**, **15 hierarchy/type**, **10 ambiguous/adversarial**. Validation MUST reject any other distribution/count, duplicate document/query ids, unknown relevance/forbidden ids, zero grades, empty queries/documents, non-finite configuration, or queries without at least one relevant id.
- **Labels/grades:** grades 1–3; each query names relevant documents and explicit `forbidden_ids` for cross-project/identity-negative controls where applicable. Corpus MUST exercise ontology hierarchy rather than merely renaming aliases, and MUST include negative controls / adversarial lexical decoys.
- **Curation quality controls:** retrieval fixture curation MUST include independent label review, negative controls, and `curation_notes` (normative).
- **Four real-store arms (same corpus, embedder, candidate limits, graph-expansion behavior, byte-identical documents):**
  1. **Baseline:** `SearchMode::Default`, `exact = false`, no ontology/domain-registry row, shipped vocabulary retained.
  2. **Exact:** `SearchMode::Default`, `exact = true`, no ontology/domain-registry row, shipped vocabulary retained.
  3. **Flat aliases (arm 3):** `SearchMode::Default`, `exact = true`, production alias-only `OntologyIndex`/domain registry installed with **all parents `null`** (same aliases as arm 4, no hierarchy); both arm 3 and arm 4 pass the **original query text** through the same production path (no arm-3-only alias rewrite or re-embed). Hierarchy is the only arm3→arm4 mechanism change.
  4. **Ontology (arm 4):** `SearchMode::Default`, `exact = true`, same aliases as arm 3 plus hierarchy installed via compiled registry/`OntologyIndex`; same production path and original query text as arm 3, using the existing production expansion path (no second hand-coded ontology ranker).
- **Isolation:** four independent temporary LadybugDB stores per run context as required, built via `tempfile::tempdir()` with no ontology/state leakage between arms. Each arm uses the real `LadybugStore::search_hybrid_with_domain` path and collects returned ids in rank order; no checked-in ranked lists or metric overrides.
- **Iterations/timing:** warm up once, then execute **>= 3 identical iterations**. Record per-query rankings for every arm and compute p95 from **per-query medians across iterations** (per corrected contract). Latency samples/timings are excluded from canonical comparison/determinism digests.

### 7.4 Metrics, thresholds, determinism, and fail-closed behavior (normative)

- **Metrics:** per-query and arm aggregate `ndcg_at_10` (DCG `(2^gain - 1) / log2(rank + 2)`), `recall_at_5`, `precision_at_5`, ontology-targeted `recall_at_5` (alias + hierarchy targeted subset as specified), forbidden hits, p95 overhead, and derived-storage percentage. Duplicate returned ids count once for recall/precision as specified. All serialized metric values MUST be finite.
- **Accepted thresholds (gate pass requires all applicable conditions):**
  - Arm 4 vs arm 3: **>= 5% relative nDCG@10 gain OR >= 5 percentage points Recall@5 gain**.
  - Alias + hierarchy subset (ontology-targeted): **>= 10 percentage points Recall@5 gain**.
  - **Precision@5 drop <= 2 percentage points** (arm 4 vs arm 3).
  - **Zero** arm-4 forbidden identity hits.
  - **p95 overhead <= 25 ms and <= 10%**; zero-baseline overhead fails only when absolute overhead exceeds 25 ms (no NaN/div-by-zero false result).
  - **Derived storage < 5%** of fixture payload bytes, where derived storage = **canonical serialized arm4 index/registry bytes / retrieval fixture JSON bytes**.
  - Two executions MUST produce identical ranked ids, forbidden hits, and finite metrics; canonical comparison/determinism SHA-256 MUST match (latency/timing excluded).
  - All competency queries pass and all retrieval acceptance comparisons pass; any store/evaluation error is a gate failure (never an empty-result success).
- **Fail-closed behavior:** malformed fixtures/config/categories/counts, unknown ids, budget breaches, service/network use, truncation/timeout, uncertified/mismatched reasoning artifacts, non-finite metrics, or any threshold/determinism violation yields `FAIL` and a nonzero `--check` exit after writing the receipt.

### 7.5 CLI, strict JSON receipt, and registration (normative contract)

- **CLI (implemented):**
  ```text
  neurostrata-mcp semantic-eval \
    --competency eval/semantic/competency.json \
    --retrieval eval/semantic/retrieval.json \
    --iterations 3 \
    --output /tmp/opencode/semantic-eval.json \
    --check
  ```
  (`--iterations >= 3` required by this spec's corrected timing contract; `--check` writes the receipt and exits nonzero on any gate failure or validation/execution error. Fail-closed: an error never yields an empty-result PASS.)
- **Strict receipt (minimum fields):** `schema_version`, package version, fixture ids and SHA-256 digests, `evidence_class: "synthetic_deterministic"`, `production_roi_claimed: false`, competency per-query results and materialization/certificate digest evidence, all four arms' aggregate and per-query metrics/rankings/forbidden hits, ontology-targeted Recall@5, p95 overhead, derived-storage percentage, determinism comparison SHA-256, gate violations, overall `PASS`/`FAIL`, and `not_evaluated` containing at least `owner_labeled_production_roi`, `blind_agent_answer_accuracy`, `owner_labeled_corpus`, `rollback_validation`, `curation_cost`, and `real_namespace_accuracy`.
- **Evidence boundary:** the bundled 60-query corpus is `synthetic_deterministic`, not owner-labeled or production. Receipts set `production_roi_claimed: false` and carry the required `not_evaluated` exclusions; no production ROI, owner-labeled-corpus, blind-agent accuracy, rollback, curation-cost, or real-namespace lift is claimed.
- **Actual first-run status (honest FAIL):** the registered mixed alias+hierarchy subset produces `insufficient_ontology_subset_lift 0.043 < 0.10`; competency queries PASS; overall `verdict = "FAIL"` and `--check` exits nonzero. This is the fail-closed gate behaving correctly, not a CLI or registration defect. A passing semantic gate is **not** claimed here.
- **Registry/docs/changelog (implemented and verified):** `eval/registry.json` defines suite `semantic-evaluation` (2 commands, 65 explicit expected test names, `ignored: 0`): 36 tests in named target `semantic_eval_metrics_test`, 17 `semantic_eval::competency::tests::*`, and 12 `semantic_eval::ablation::tests::*`. Numerical acceptance remains owned by the strict CLI receipt, not registry counts. CLI docs (`docs/features-api/cli-reference.md`), this spec, the parent plan, and the 1.19.0 changelogs describe the exact CLI, gate corrections, and evidence boundary. **Final evidence:** remediation 65/65 with zero ignored/failed; two strict `--check` runs exit nonzero honestly; competency 8/8 PASS; retrieval FAIL solely on mixed alias+hierarchy Recall@5 lift `0.042857 < 0.10`; identical determinism SHA-256 `5f378d7b6d2666dbaef44a06fa80de1b75f5dec2c99951559593f52db4f45583`; six `not_evaluated` exclusions and `production_roi_claimed=false`. The release record (`docs/releases/changelog/1.19.0.md`) preserves this owner-approved honest FAIL. Independent findings-first review closed with zero Critical/Important findings; no semantic-gate PASS or full-workspace-green claim is made.

## 8. Verification & Quality Gate Matrix (`v1.19.0`, implementation in progress)

Every capability in this specification is gated by focused, deterministic unit and integration suites (no full-workspace `cargo test` runs during inner-loop development). **No command below is claimed passing in this spec without parent/coder evidence.**

1. **Capability A (`neurostrata-s0l0`):** `cargo test --locked --bin neurostrata-mcp rdf::reasoning::` and `cargo test --locked --bin neurostrata-mcp formal_kernel::` — target: full supported RDFS/OWL-RL materialization via `reasonable`, deterministic reconstruction/auditing with a sound independently verified derivation for **every returned inferred triple** (supported rule coverage including transitive/inverse/property chains/domain/range/class+property equivalence and relevant restriction rules), fail-closed rejection for any individual triple/rule that cannot be established with **no uncertified inferred output** (no blanket/normal-path `ProofUnavailable`), `ReasoningProfile`/certificate digest reporting, Lean 4 certificate verification, named-graph separation, and RDF-star annotations from verified steps.
2. **Capability B (`neurostrata-miio`):** `cargo test --locked --bin neurostrata-mcp rdf::ontology::` and `cargo test --locked --bin neurostrata-mcp rdf::scenario::` — target: zero-mutation `plan_mutation_diff`, `added_entailments`/`removed_entailments`, SHACL/consistency reporting, impact sets, `schema_ddl_diff` from `diff_ontologies`/`generate_cypher_ddl`, and `OntologyPort`/`neurostrata_scenario(plan_ontology_diff)` wiring.
3. **Capability C (`neurostrata-50sm`):** `cargo test --locked --bin neurostrata-mcp rdf::provenance::`, `cargo test --locked --bin neurostrata-mcp rdf::shacl::`, and `cargo test --locked --bin neurostrata-mcp rdf::taxonomy::` (plus focused task/snapshot surface tests as added) — target: exact tier enforcement, `stale_after` RFC3339/epoch advisory surfacing, and `[DEPRECATED — SUPERSEDED BY ...]` / `not_definition` on production surfaces.
4. **Capability D (`neurostrata-dlag`):** `cargo test --locked --bin neurostrata-mcp vault_ld::` and handler/wiring tests — target: `neurostrata_vault_sync`/`vault-sync` shared path, export/import round-trip, secret+SHACL fail-closed before writes, bitemporal supersession, `stub:true` unresolved links, `J6` `check_caller_path`, ReBAC/privacy/SynapticMesh non-clobber.
5. **Capability E (`neurostrata-mpau`):** `cargo test --locked --bin neurostrata-mcp learning::`, `cargo test --locked --bin neurostrata-mcp rdf::action::`, and focused task-complete hook tests — target: deterministic mining, SHACL-valid candidates, action-define-gated non-auto-applied proposals, and actual `task_complete`/successful-revert hook integration.
6. **Capability F (`neurostrata-v6dw`):** `cargo test --locked --bin neurostrata-mcp rdf::ingest::` and handler tests — target: strict bounded classification, empty domain/vocabulary fail-closed, invented-term rejection, SHACL fail-closed, and whole-batch validation with no partial writes.
7. **Capability G (`neurostrata-dyrq`):** focused `semantic_eval` suites and CLI evidence (planned; no pass claims without evidence): `cargo test --locked --test semantic_eval_metrics_test -- --test-threads=1`, `cargo test --locked --bin neurostrata-mcp semantic_eval:: -- --test-threads=1`, `EVAL_ALLOW_DIRTY=1 scripts/remediation-eval.sh semantic-evaluation --json /tmp/opencode/semantic-evaluation-remediation.json` (only when worktree intentionally dirty), and `cargo run --locked --bin neurostrata-mcp -- semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check` — target: offline ASK/SELECT competency pass, 60-query 20/15/15/10 synthetic deterministic four-arm ablation, corrected arm/search/iteration/p95-median/derived-storage/competency contracts, exact thresholds, determinism and fail-closed behavior, strict JSON receipt with `evidence_class = "synthetic_deterministic"`, `production_roi_claimed = false`, and required `not_evaluated` exclusions (no owner-labeled/production-ROI/blind-agent/rollback/curation-cost/real-namespace claims).
8. **Repository Gates:** `neurostrata-mcp doc-gate` and `neurostrata-mcp task gate NeuroStrata --strict` (report status with evidence; do not claim pass without evidence).
