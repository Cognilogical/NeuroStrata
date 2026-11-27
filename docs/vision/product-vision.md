# NeuroStrata Product Vision

## Core Thesis

NeuroStrata is a single-binary, local-first cognitive memory, enterprise ontology, digital-twin, kinetic-action, and domain-expertise engine for autonomous AI agents. It provides a durable, bitemporal knowledge substrate that agents read, write, and reason over without a cloud dependency. The core belief is that an agent's competence is bounded by what it can reliably remember and formally verify — memory is infrastructure, not an afterthought.

## General-Purpose Project Memory & Ontology

NeuroStrata is a general-purpose, project-oriented memory, knowledge-base, and ontology system. While it ships first-class support for software and document projects, a project does not have to be coding- or document-oriented. Domain facts, entities, operational knowledge, preferences, and rules shared in conversation are stored and linked directly in the project knowledge base — through property-graph memory, W3C RDF/SPARQL 1.1/SHACL, SKOS domain taxonomies, polymorphic digital twins, counterfactual scenarios, and kinetic actions.

The engine unifies:

- A property-graph memory with bitemporal versioning and supersession
- A W3C RDF semantic layer (SPARQL 1.1, SHACL validation, RDF-star provenance)
- SKOS domain taxonomies with linear DAG validation and transitive disjointness reasoning
- Polymorphic digital twins fed by telemetry
- Counterfactual copy-on-write ontology scenarios
- Fail-closed kinetic actions with bitemporal revert logs

Memory and session state belong to a project. The home folder is never a session root unless it is itself a git checkout, so for a session started in the home folder, or in a folder under it that is not a project, neither the daemon nor the harness plugins write session files into the home folder.

## Three-Strata Cognitive Architecture

The memory model is organized into three strata that mirror the cognitive sciences:

1. **Global** — cross-project, always-available knowledge
2. **Domain** — project-scoped and namespace-scoped rules and facts
3. **Task** — the working memory of a single unit of work, including the **Central Executive** task machine that enforces locks and completion gates

On top of these strata sits **SynapticGraph**, the retrieval substrate: a usage-and-provenance gain subtracted from vector distance, then a bounded directed graph expansion over `RELATES_TO`/`CONTAINS`/`GOVERNS` edges. Retrieval is deterministic and never depends on lexical search.

## Evaluation Axes & Non-Goals

**What NeuroStrata optimizes for:**

- Local-first, single-binary deployment with no mandatory cloud service
- Bitemporal correctness — every memory mutation is versioned and revertible
- Formal verification of the core decision kernels and state machines — Lean 4 parity for the taxonomy, procedure, and connector theorems, plus Kani bounded proofs for gate, budget, bounds, and ordering properties
- v1.19.1 generational context graph: LadybugDB and the Episodic Buffer form the persistent heap; `working_set` is a bounded 4,096-byte L1 cache with a 3-tier KV-cache projection (Tier 0 cache-stable Golden Rules prefix, Tier 1 quantized 8-message stride folding with a 12-message hot tail, and Tier 2 tail delta with typed `working_set.edges`, 1-hop/2-hop spreading activation, pre-turn prompt activation, and deterministic zero-LLM compaction takeover). The Rust/Lean/TypeScript GC decision kernel applies TTL and budget eviction, quantized stride folding, and cache-tier routing, replaces eligible nodes with deterministic pointer stubs of at most 192 bytes, and promotes re-referenced stubs on fitting page faults. A 3,969-row Rust/Lean parity corpus, 32 Kani symbolic proof harnesses, and an 8-mutant Kani mutation gate pin the eviction, reset, and stride-folding behavior.
- v1.19.0 certified OWL 2 RL materialization: a 26-rule formal kernel independently verifies a complete derivation certificate for every inferred triple, with Lean parity evidence. Certification covers only this implemented kernel — not arbitrary reasoning.
- v1.19.0 fail-closed gates: pre-push and staged-quality checks no longer silently succeed when a checker is missing or exits abnormally.
- The shell-exemption kernel bounds caller JSON nesting at 64 levels and fails closed on payloads it cannot fully validate, so hostile input cannot overflow the daemon stack.
- Graph-first exploration that keeps agent context windows small
- Deterministic, auditable retrieval

**Non-goals:**

- NeuroStrata is not a vector database product; embeddings are a means, not the mission
- It is not a chat application or RAG product — clients like **Digital Twins** surfaces and UI panels are thin shims over the core
- It never trades bitemporal correctness or gate safety for recall speed
- It does not couple core logic to a single agent harness; a **WorkflowConnector** and harness plugin layer keep the core tool-agnostic

The engine exists so that an agent can **remember**, **reason**, and **act** — with **Digital Twins** grounding state, **Kinetic Actions** mutating it safely, and a **SKOS Taxonomy** giving the agent subject-matter expertise — all inside one auditable local binary.
