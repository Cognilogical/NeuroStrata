# NeuroStrata Product Vision

## Core Thesis

NeuroStrata is a single-binary, local-first cognitive memory, enterprise ontology, digital-twin, kinetic-action, and domain-expertise engine for autonomous AI agents. It provides a durable, bitemporal knowledge substrate that agents read, write, and reason over without a cloud dependency. The core belief is that an agent's competence is bounded by what it can reliably remember and formally verify — memory is infrastructure, not an afterthought.

## General-Purpose Project Memory & Ontology

NeuroStrata is a general-purpose, project-oriented memory, knowledge-base, and ontology system. While it ships first-class support for software and document projects, a project does not have to be coding- or document-oriented. Domain facts, entities, operational knowledge, preferences, and rules shared in conversation are stored and linked directly in the project knowledge base — through property-graph memory, W3C RDF/SPARQL 1.1/SHACL, SKOS domain taxonomies, polymorphic digital twins, counterfactual scenarios, and kinetic actions.

The engine unifies:

- A property-graph memory with bitemporal versioning and supersession
- A W3C RDF semantic layer (SPARQL 1.1, SHACL validation, RDF-star provenance)
- SKOS domain taxonomies with disjointness and subsumption reasoning
- Polymorphic digital twins fed by telemetry
- Counterfactual copy-on-write ontology scenarios
- Fail-closed kinetic actions with bitemporal revert logs

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
- Formal verification of every gate, lock, and state machine (Lean 4 kernel parity and Kani Rust Verifier symbolic model checking)
- Graph-first exploration that keeps agent context windows small
- Deterministic, auditable retrieval

**Non-goals:**

- NeuroStrata is not a vector database product; embeddings are a means, not the mission
- It is not a chat application or RAG product — clients like **Digital Twins** surfaces and UI panels are thin shims over the core
- It never trades bitemporal correctness or gate safety for recall speed
- It does not couple core logic to a single agent harness; a **WorkflowConnector** and harness plugin layer keep the core tool-agnostic

The engine exists so that an agent can **remember**, **reason**, and **act** — with **Digital Twins** grounding state, **Kinetic Actions** mutating it safely, and a **SKOS Taxonomy** giving the agent subject-matter expertise — all inside one auditable local binary.
