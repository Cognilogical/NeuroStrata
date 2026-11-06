# 📚 NeuroStrata Documentation Hub

Welcome to the official documentation for **NeuroStrata** — the single-binary Rust cognitive memory, graph reasoning, and task-gate engine for AI coding agents.

---

## 1. Product Overview & Getting Started

| Document | Description |
| :--- | :--- |
| [**Main README (`../README.md`)**](../README.md) | 60-second quickstart, zero-touch installation, and high-level system overview. |
| [**Complete Feature & Benefit Catalog (`FEATURES.md`)**](FEATURES.md) | Exhaustive inventory of v1.16.1 features, MCP tools, and codebase module mappings. |
| [**Building from Source (`BUILDING.md`)**](BUILDING.md) | Toolchain prerequisites (Rust, CMake, C++20) and cross-platform build scripts (`scripts/build.sh`, `scripts/build.ps1`). |
| [**Visual Interface & Obsidian Guide (`UI_GUIDE.md`)**](UI_GUIDE.md) | Using the 3D/2D SynapticGraph Web UI and the Task Board. (The Obsidian NeuroVault sync is planned, not implemented — see `ops/obsidian.md`.) |

---

## 2. Architecture & Cognitive Science

| Document | Description |
| :--- | :--- |
| [**Cognitive Architecture (`COGNITIVE_ARCHITECTURE.md`)**](COGNITIVE_ARCHITECTURE.md) | Formal specification of the 15 neuroscience-inspired mechanisms powering NeuroStrata. |
| [**SynapticGraph Engine (`architecture/synapticgraph.md`)**](architecture/synapticgraph.md) | Embedded LadybugDB storage, bi-temporal validity (`valid_from`/`valid_to`), six-language Tree-sitter AST graph, and W3C RDF/SPARQL/OWL/SHACL reasoning. |
| [**Tri-Strata Isolation (`architecture/tri-strata.md`)**](architecture/tri-strata.md) | Global, Domain/Project, and Task memory partitioning plus multi-worktree source anchoring. |
| [**Episodic Log Recall (`recall.md`)**](recall.md) | How `neurostrata_append_log` piggybacks tag-matched memory recall onto every conversational turn. |
| [**Why Traditional AI Memory Fails (`science/why-memory-fails.md`)**](science/why-memory-fails.md) | Analysis of the 5 failure modes of flat Markdown and naive RAG—and how NeuroStrata eliminates them. |
| [**Biological Nomenclature (`science/nomenclature.md`)**](science/nomenclature.md) | 1-to-1 translation table between neuroscience nomenclature and Rust systems-engineering primitives. |

---

## 3. The Five Cognitive Organs

| Organ | Document | Role |
| :--- | :--- | :--- |
| **Central Executive** | [`organs/central-executive.md`](organs/central-executive.md) | 4-state Task FSM, Zero-Action Start, 60-minute claim leases, Lock 1 (`pre-push` gate), Lock 2 (`EXTRACTED_FROM` done-funnel), Lock 3 (Breath notes), and subagent context injection. |
| **Prefrontal Cortex** | [`organs/prefrontal-cortex.md`](organs/prefrontal-cortex.md) | Semantic & pattern action interceptor (`neurostrata_guard_validate`), learned behavioral-rule enforcement, and Podman sandboxing. |
| **Dendritic Bridge** | [`organs/dendritic-bridge.md`](organs/dendritic-bridge.md) | Local FastEmbed ONNX (`NomicEmbedTextV15`, 768 dims) + external embedders, and three alternative retrieval surfaces (HNSW vector, Tantivy lexical, in-house BM25) with a bounded graph expansion. |
| **Thalamic Bus** | [`organs/thalamic-bus.md`](organs/thalamic-bus.md) | Non-blocking in-process typed pub/sub event relay, post-ingest edge normalizer, and export-freshness gate. |
| **Procedural Memory** | [`organs/procedural-memory.md`](organs/procedural-memory.md) | Triggered engineering habits (`memory_type: "procedure"`) with finite rehearsal budgets (`neurostrata_procedure_perform`). |

---

## 4. Operations, CLI, Team Sync & Security

| Document | Description |
| :--- | :--- |
| [**CLI Reference (`ops/cli-reference.md`)**](ops/cli-reference.md) | Complete guide to all `neurostrata-mcp` subcommands (`daemon`, `status`, `namespaces`, `ingest`, `export-graph`, `task gate`, `synaptic-mesh`, `learn`, `kge`, `storage`, `metrics`, `backup`, `doctor`, etc.). |
| [**Zero-Touch Upgrading (`ops/upgrading.md`)**](ops/upgrading.md) | Automatic in-place schema migration, git hook self-healing, and harness plugin refresh on startup. |
| [**SynapticMesh Team Sync (`mesh/synaptic-mesh.md`)**](mesh/synaptic-mesh.md) | Brokerless Git team memory synchronization, private row sovereignty, and conflict preservation. |
| [**Security & OWASP Mitigations (`ops/security.md`)**](ops/security.md) | Loopback bearer auth, Caller-Path Authority (`J6`), fail-closed secret rejection, query escaping, and non-destructive MCP design. |
| [**Obsidian & NeuroVault (`ops/obsidian.md`)**](ops/obsidian.md) | *Planned, not implemented.* No `neurostrata_sync_obsidian` tool or `obsidian-sync` command ships today — tracked by task `neurostrata-dlag`. |
| [**Crash Recovery (`ops/crashes.md`)**](ops/crashes.md) | Crash breadcrumbs (`~/.config/NeuroStrata/data/db/crash/`), lock recovery, and checkpointed backups. |
| [**WAL Recovery Runbook (`runbooks/wal-recovery.md`)**](runbooks/wal-recovery.md) | Step-by-step runbook for inspecting and recovering quarantined LadybugDB write-ahead logs. |
| [**Release Changelogs (`releases/changelog/`)**](releases/changelog/) | Per-version release notes and historical changelog archive. |

---

## 5. Domain Specifications & Engineering Design Records

- **Domain Specifications (`architecture/domains/`):**
  - [`core_installer.md`](architecture/domains/core_installer.md) — Zero-touch installer and harness plugin architecture.
  - [`judgment-providers.md`](architecture/domains/judgment-providers.md) — Laya local ONNX and TypeSafe Jev duplicate-judgment providers.
  - [`local-learning.md`](architecture/domains/local-learning.md) — Local experience capture, dataset curation, and evidence-gated model promotion.
- **Subsystem Specifications:**
  - [`design-task-subsystem.md`](design-task-subsystem.md) — Central Executive task state machine and gate specification.
  - [`design-wiring-panel.md`](design-wiring-panel.md) — Prescribed guard & gate wiring contract.
  - [`architecture/prescribed-wire-recipes.md`](architecture/prescribed-wire-recipes.md) — Reference recipes for project-level enforcement gates.
  - [`architecture/self_reinforced_testing_framework.md`](architecture/self_reinforced_testing_framework.md) — Self-reinforced behavioral verification architecture.
  - [`superpowers/specs/`](superpowers/specs/) — Detailed engineering specs for SynapticMesh, SPARQL 1.1, Leiden Community Detection, TransE KGE, GLiNER, and Local Self-Improvement.
