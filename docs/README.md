# 📚 NeuroStrata Documentation Hub

Welcome to the official documentation for **NeuroStrata** — the single-binary Rust cognitive memory, graph reasoning, and task-gate engine for AI coding agents.

---

## Documentation Layout

NeuroStrata documentation is organized into **three canonical tiers**, strictly separated, plus an archive for historical records:

| Tier | Directory | Purpose |
|------|-----------|---------|
| **Tier 1 — Vision** | [`docs/vision/`](vision/) | Product vision, core thesis, cognitive architecture, and scientific rationale. Audience: product owners, architects, evaluators. |
| **Tier 2 — Features & API** | [`docs/features-api/`](features-api/) | Internal features, major subsystems, MCP/HTTP API reference, CLI reference, and ops runbooks. Audience: developers, contributors, operators. |
| **Tier 3 — Extend** | [`docs/extend/`](extend/) | Third-party extensibility: workflow/lifecycle connectors, harness plugins, custom providers and ontologies. Audience: plugin authors, integrators. |
| **Archive** | `docs/archive/` | Historical verdicts, superseded specs, plans, and design records. Read-only; do not link to from active docs. |

The machine-readable documentation manifest lives at [`docs/manifest.json`](manifest.json). It enumerates required files per tier, prohibited filename substrings (competitor-naming guard), and surface-diff rules that bind code changes to documentation updates. The documentation gate (`neurostrata-mcp doc-gate`) consumes this manifest.

---

## Tier 1 — Product Vision

| Document | Description |
|:---|:---|
| [`product-vision.md`](vision/product-vision.md) | NeuroStrata product vision: single-binary local-first cognitive memory, ontology, digital-twin, kinetic-action, and domain-expertise engine. |
| [`cognitive-architecture.md`](vision/cognitive-architecture.md) | Formal specification of the neuroscience-inspired three-strata architecture and SynapticGraph retrieval. |
| [`why-memory-fails.md`](vision/why-memory-fails.md) | Why traditional flat Markdown and naive RAG memory fails — and how NeuroStrata fixes it. |
| [`nomenclature.md`](vision/nomenclature.md) | Translation table between neuroscience nomenclature and Rust systems-engineering primitives. |
| [`tri-strata.md`](vision/tri-strata.md) | Global / Domain / Task memory partitioning plus multi-worktree source anchoring. |

---

## Tier 2 — Internal Features & Major API

| Document | Description |
|:---|:---|
| [`mcp-and-http-api.md`](features-api/mcp-and-http-api.md) | MCP Tools & HTTP Daemon API Reference — all 30 MCP tools and HTTP daemon endpoints. |
| [`cli-reference.md`](features-api/cli-reference.md) | CLI Reference for `neurostrata-mcp` — including workflow, documentation gate, action, taxonomy, scenario, twin, task, and synaptic-mesh commands. |
| [`feature-catalog.md`](features-api/feature-catalog.md) | Exhaustive inventory of v1.18.0 features, MCP tools, and codebase module mappings. |
| [`ui-guide.md`](features-api/ui-guide.md) | Visual Interface & Obsidian Guide — using the 3D/2D SynapticGraph Web UI and the Task Board. |
| [`episodic-recall.md`](features-api/episodic-recall.md) | How `neurostrata_append_log` piggybacks tag-matched memory recall onto every conversational turn. |
| [`limitations.md`](features-api/limitations.md) | Known limitations and non-goals. |
| `subsystems/central-executive.md` | Central Executive — 4-state Task FSM, Zero-Action Start, claim leases, Lock 1/2/3, subagent context injection. |
| `subsystems/kinetic-actions.md` | Kinetic Action Types & Bitemporal Revert Log — 5-stage fail-closed gate, atomic apply & revert receipts. |
| `subsystems/taxonomy-advisor.md` | Subject-Matter Domain Expertise & SKOS Taxonomy Advisor — concept graph, disjointness, SME frontier `advise`. |
| `subsystems/digital-twins.md` | Digital Twins & Counterfactual Ontology Scenarios — state schemas, telemetry ingestion, CoW scenario overlays. |
| `subsystems/judgment-providers.md` | Judgment providers — Laya local ONNX and TypeSafe Jev duplicate-judgment providers. |
| `subsystems/local-learning.md` | Local learning — experience capture, dataset curation, evidence-gated model promotion. |
| `subsystems/core-installer.md` | Core installer and harness plugin architecture. |
| `subsystems/prescribed-wire-recipes.md` | Reference recipes for project-level enforcement gates. |
| `subsystems/synapticgraph.md` | SynapticGraph Engine — embedded LadybugDB, bi-temporal validity, Tree-sitter AST graph, W3C RDF/SPARQL/OWL/SHACL. |
| `subsystems/synaptic-mesh.md` | SynapticMesh Team Sync — brokerless Git team memory synchronization, private row sovereignty, conflict preservation. |
| `subsystems/dendritic-bridge.md` | Dendritic Bridge — embedding backends and retrieval surfaces. |
| `subsystems/thalamic-bus.md` | Thalamic Bus — typed pub/sub event relay, post-ingest edge normalizer, export-freshness gate. |
| `subsystems/procedural-memory.md` | Procedural Memory — triggered engineering habits with finite rehearsal budgets. |
| `subsystems/self-reinforced-testing.md` | Self-reinforced behavioral verification architecture. |
| `ops/building.md` | Building from source — toolchain prerequisites and cross-platform build scripts. |
| `ops/security.md` | Security & OWASP mitigations — bearer auth, Caller-Path Authority (`J6`), fail-closed secret rejection. |
| `ops/upgrading.md` | Zero-touch upgrading — schema migration, git hook self-healing, harness plugin refresh. |

---

## Tier 3 — Extend (Third-Party Connectors & Interfaces)

Tier 3 is kept strictly separate from the internal feature docs above. It describes surfaces external integrators can extend without modifying the NeuroStrata core.

| Document | Description |
|:---|:---|
| [`overview.md`](extend/overview.md) | Extension philosophy and the three extension surfaces (WorkflowConnector, harness plugins, Embedder/JudgmentProvider). |
| [`workflow-lifecycle-connectors.md`](extend/workflow-lifecycle-connectors.md) | Pluggable Workflow & Lifecycle Connectors — `WorkflowStage` state machine, `WorkflowConnector` trait, `.NeuroStrata/workflow-connector.json`, writing custom connectors. |
| [`harness-plugins.md`](extend/harness-plugins.md) | Agent Harness Plugin Interface — `createHarnessPlugin`, `handleHookEvent`, `resolveProjectRoot`, `discoverProjectNamespace`. |
| [`providers-and-ontologies.md`](extend/providers-and-ontologies.md) | Custom `Embedder` & `VectorStore` implementations, `JudgmentProvider` adapters, domain ontologies, and kinetic action packs (`ActionTypeDefinition`). |

---

## Archive

Historical verdicts, superseded specifications, plans, and design records live under `docs/archive/`. These documents describe work that has been retired, superseded, or completed. Read-only — do not link to archive records from active-tier docs.

---

## Operations & Releases

| Document | Description |
|:---|:---|
| [`docs/releases/changelog/`](releases/changelog/) | Per-version release notes and historical changelog archive. |
| [`docs/assets/`](assets/) | Project banner, logos, and infographics (before/after, cognitive strata, soft locks, graph-first blast radius, zero-touch subagent mesh). |

---

## Getting Started

1. Read [`vision/product-vision.md`](vision/product-vision.md) to understand what NeuroStrata is and why it exists.
2. Read [`cli-reference.md`](features-api/cli-reference.md) to learn the `neurostrata-mcp` CLI commands.
3. Read [`docs/manifest.json`](manifest.json) to see the documentation manifest and surface-diff rules.
4. Run the documentation gate: `neurostrata-mcp doc-gate`.

---

For the project-level README with build instructions and zero-touch setup, see [`../README.md`](../README.md).
