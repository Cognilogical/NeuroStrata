# 🧠 NeuroStrata

![NeuroStrata Banner](docs/assets/NeuroStrata-banner.png)

### **The World's Most Advanced Agentic Memory, Task, and Workflow System.**

**Superhuman cognitive architecture for your AI agents—without needing to be a brain surgeon to understand or operate it.**

Most AI coding tools force an impossible trade-off: either settle for flat markdown files (`AGENTS.md`, `MEMORY.md`, `TODO.md`) that your agent forgets or ignores the moment context compacts, or spend weeks manually wiring vector databases, graph engines, AST indexers, and custom git hooks.

**NeuroStrata eliminates that trade-off.**

Packed into **one compiled Rust binary**, NeuroStrata gives your coding agents **six distinct ways to think, remember, plan, and verify work**—combining an embedded vector & property graph database, a six-language Tree-sitter AST software graph, a W3C RDF/SPARQL/SHACL semantic layer with RDFS/OWL closure, three alternative deterministic retrieval surfaces, digital-twin state versioning, and binary-enforced task gates.

And here is the best part: **100% of it is automated.** Your agent installs the plugins, wires the hooks, builds the knowledge graph, propagates context to every subagent, enforces its own completion gates, and upgrades itself across versions—**with zero manual configuration from you.**

👉 **[Explore the Complete Feature & Benefit Catalog (`docs/FEATURES.md`)](docs/FEATURES.md)**



## How NeuroStrata Helps Your Agents Think (In Every Dimension)

### 1. Associative & Experiential Thinking — *Remember Every Rule, Fix, and Failed Attempt*
Every time you start a new session, switch worktrees, or compact context, ordinary agents get amnesia. NeuroStrata gives them a persistent **Tri-Strata Memory** (Global, Project Domain, and Task Working Memory):
- **Three Retrieval Surfaces, Not One Fused Pipeline:** A query runs on **one** of three surfaces, chosen with the `mode` argument — **LadybugDB HNSW** vector search (the default, `NomicEmbedTextV15` at 768 dims), **Tantivy** full-text search, or the in-house 5-field **BM25** scorer — followed by a bounded **1–2 hop graph expansion** over `GOVERNS` / `RELATES_TO` / `CONTAINS` / `DEPENDS_ON` / `EXTRACTED_FROM` edges. The surfaces are alternatives; their scores are never fused. There is no rank fusion, no Personalized PageRank, and no neural reranker.
- **Negative Exemplars (Never Repeat a Dead End):** Failed approaches are stored as first-class evidence (`outcome: "failed"`), so future sessions immediately see what *didn't* work and why.
- **Procedural Memory (Habits with Rehearsal Budgets):** Recurring routines (`session-start`, `before-edit`, `after-mutation`) fire automatically as tracked procedures (`neurostrata_procedure_perform`) until they become ingrained.

### 2. Structural & Blast-Radius Thinking — *See the Whole Codebase Before Touching a Line*
Instead of burning tokens on blind `grep` loops and breaking callers three files away, your agent thinks in terms of **live software structure**:
- **Unified Six-Language AST + Rule Graph:** Incremental **Tree-sitter** parsing (Rust, TypeScript, TSX, Python, Go, Java) links functions, structs, classes, and imports directly to the architectural `[rule]` memories that govern them (`GOVERNS`, `DEPENDS_ON`, `calls`, `imports`).
- **Automatic Graph-First Reflex:** Before editing a file, one deterministic lookup (`neurostrata_memories_for_file`, with `include_ast_edges` for the call-graph and blast radius) reveals every governing constraint and the transitive upstream/downstream impact.

### 3. Deductive & Semantic Thinking — *Exact W3C Logic When Fuzzy Search Isn't Enough*
Vector similarity is great for concepts, but terrible for strict invariants. NeuroStrata embeds a native **W3C Semantic Web engine** (`oxrdf` / `oxttl` for the quads, `spargebra` for SPARQL 1.1, `shacl` for shapes) alongside its property graph:
- **SPARQL 1.1 Queries (`SELECT` / `ASK` / `CONSTRUCT`):** Agents run exact multi-hop logical queries across code symbols, tasks, and domain rules (`neurostrata_sparql_query`).
- **RDFS/OWL Closure & SHACL Closed-World Validation:** Automatically materializes transitive `subPropertyOf` and subclass closure (plus the `broader` relation between domain slugs) and validates memory/graph writes against strict structural shapes. The closure is computed inside the store; there is no separate `neurostrata_owl_reason` tool.

### 4. Executive & Workflow Thinking — *Tasks That Cannot Be Half-Finished or Deferred*
Prompts get rationalized away; binary locks do not. NeuroStrata's **Central Executive** governs the entire work lifecycle through **Three Resilient Soft Locks**:
- **Zero-Action Start:** Agents are mechanically blocked from editing files or running mutating commands until they call `neurostrata_get_snapshot` and claim an active task (`neurostrata_task_claim`).
- **Lock 3 — The Breath Working-Memory Prompt:** Every 3–5 mutation steps, the agent pauses and records a state checkpoint on the task (`neurostrata_task_update`) so crash recovery is instant.
- **Lock 2 — Done-Funnel Mandatory Extraction:** A task **cannot** transition to `done` (`neurostrata_task_complete`) unless the agent extracts at least one architectural rule or lesson into permanent memory. Every finished task makes the system smarter.
- **Lock 1 — Pre-Push Git Gate:** `git push` runs `neurostrata-mcp task gate <namespace> --strict`. Any `in_progress` task, any `done` task without an extracted memory, or any P0 task open for more than 24 hours blocks the push automatically. Deferral wording is caught separately by the lexical task-intent detector, which nudges the agent to file a tracked task instead of letting it evaporate; the vocabulary it detects is listed in the [feature catalog](docs/FEATURES.md).

### 5. Collective & Subagent Thinking — *Subagents Never Spawn With a Blank Brain*
When an orchestrator delegates to a subagent (`worker`, `checker`, `craftsman`, `Task`), context normally evaporates. NeuroStrata makes context propagation automatic:
- **Auto-Retrieved Subagent Injection (`subagent_prompt_injection`):** Creating or claiming a task automatically runs a hybrid vector + 1-hop graph search and returns a ready-to-paste `subagent_prompt_injection` block containing the active Goal, governing `[rule]` constraints, and AST neighbors.
- **Zero-Dependency Harness Interception:** Embedded plugins for **OpenCode**, **Claude Code**, **Hermes Agent**, **OpenAI Codex CLI**, **GitHub Copilot CLI**, and **OpenClaw** automatically prepend that context block into every spawned subagent call.
- **Brokerless Team Sync (`SynapticMesh`):** Share approved rules and procedures across machines and teammates over standard `git push` and `git pull --ff-only` while keeping private memories sovereign on your disk.

### 6. Predictive & Conscience Thinking — *Simulate State & Sandbox Risky Commands First*
- **Digital Twins & Neuroplasticity Telemetry:** Agents model complex system state with versioned **Digital Twins** (`neurostrata_twin_set`, `neurostrata_twin_get`, `neurostrata_twin_list`, `neurostrata_twin_diff`) and read rolling time-series performance summaries (`neurostrata_metrics_query`, `neurostrata_metrics_list`).
- **Prefrontal Cortex Safety Sandbox:** Proposed shell commands and file edits are evaluated against semantic `guard_rule` memories (`neurostrata_guard_validate`) and dry-run inside an ephemeral, rootless `podman --network=none` container with the workspace mounted read-only, before touching your real filesystem.



## Why You Don't Need to Be a Brain Surgeon to Run It

All of that cognitive machinery lives inside **one self-managing binary**. There is no Docker Compose stack, no Python virtualenv to babysit, no YAML configuration labyrinth, and no manual database maintenance.

### 1. First-Time Install (Once Per Machine)

> **No plugin build step needed for a fresh checkout.** `src/task/harness_plugins.rs`
> embeds `plugins/opencode/dist/index.js` with `include_str!`, and that bundle is
> tracked in the repository, so it is present on a fresh clone and the Rust build
> compiles without any manual step. The CI include-str guard runs before the first
> compiling step to assert the bundle exists.

> **Rebuilding the OpenCode plugin.** If you edit plugin sources under
> `plugins/opencode/`, you must rebuild the bundle and commit it before the change
> ships — the Rust binary embeds the committed `dist/index.js`, not the source.
> From the repository root:
>
> ```bash
> (cd plugins/opencode && npm install && npm run build)
> ```
>
> This step needs Node 18 or newer (the plugin declares `engines >= 18`).
> The same applies to every other embedded plugin under `plugins/` — each one
> ships a tracked `dist/` bundle that the Rust binary embeds.

```bash
# 1. Build and install the single Rust binary (includes embedded LadybugDB + Tantivy + FastEmbed ONNX)
scripts/build.sh

# 2. Register and start the local loopback daemon (127.0.0.1:34343)
neurostrata-mcp setup
```

### 2. Initialize Any Project in One Sentence
Open your AI coding agent in any repository (new or existing) and say:
> **"Initialize NeuroStrata for this project."**

**NeuroStrata and your agent handle everything else automatically:**
1. Calls `neurostrata_bootstrap` (new repo) or `neurostrata_task_setup` (existing repo).
2. Auto-detects your active CLI/terminal harness (**OpenCode**, **Claude Code**, **Hermes**, **Codex CLI**, **GitHub Copilot CLI**, or **OpenClaw**), extracts and wires the embedded zero-dependency plugin under `.NeuroStrata/plugins/<harness>/`, installs the `neurostrata` skill (`.agents/skills/neurostrata/SKILL.md` + global harness skill dir), and patches existing subagent `.md` definitions with the Graph-First & `subagent_prompt_injection` rules.
3. Installs and activates both `.git/hooks/pre-commit` (graph-query ledger) and `.git/hooks/pre-push` (strict task gate + SynapticMesh sync).
4. Scans your repository, ingests the multi-language AST graph, and seeds your initial architectural rules.

### 3. Zero-Touch Upgrades
When a new version of NeuroStrata ships, replace the binary (`scripts/build.sh` or download the release binary) and keep working. On the very next session start (`neurostrata_get_snapshot`), NeuroStrata **automatically detects the version delta, upgrades your git hooks, refreshes your harness plugins and skills, backfills schema defaults, and reconciles any new lifecycle wires.**



## Feature Overview at a Glance

> For the complete 12-part breakdown of every subsystem, tool, and code reference, see **[`docs/FEATURES.md`](docs/FEATURES.md)**.

| Subsystem | How It Helps Agents Think & Work | Deep-Dive Docs |
| :--- | :--- | :--- |
| **Zero-Touch Harness Auto-Installer** | Auto-detects and wires plugins, skills (`SKILL.md`), and subagent rules for **OpenCode, Claude Code, Hermes, Codex CLI, GitHub Copilot CLI, and OpenClaw**. | [Features §1–2](docs/FEATURES.md#1-zero-touch-onboarding--self-upgrading-lifecycle) |
| **Graph-First & Subagent Injection** | Auto-retrieves vector + 1-hop AST/rule context on task claim and injects `subagent_prompt_injection` into every spawned subagent. | [Features §3](docs/FEATURES.md#3-automatic-graph-first-exploration--subagent-context-propagation) |
| **Central Executive (Tasks & 3 Soft Locks)** | Zero-Action Start, Lock 3 Breath checkpoints, Lock 2 Done-Funnel (mandatory memory extraction), Lock 1 Pre-Push Gate, and lexical `TODO` detection. | [Central Executive](docs/organs/central-executive.md) |
| **Memory Core (SynapticGraph)** | Embedded LadybugDB vector + graph store, three alternative retrieval surfaces (HNSW vector, Tantivy full-text, in-house BM25) with bounded graph expansion, negative exemplars, and Leiden communities. | [SynapticGraph](docs/architecture/synapticgraph.md) · [Tri-Strata](docs/architecture/tri-strata.md) |
| **Six-Language AST & Impact Graph** | Incremental Tree-sitter AST ingestion for six languages, call-graph and blast-radius summaries, and GLiNER entity extraction. | [Features §6](docs/FEATURES.md#6-six-language-tree-sitter-ast-software-graph--blast-radius-analysis) |
| **W3C Semantic Graph** | Native RDF quad projection, **SPARQL 1.1** queries (`SELECT`/`ASK`/`CONSTRUCT`), RDFS/OWL closure, and closed-world **SHACL** validation. | [Features §7](docs/FEATURES.md#7-w3c-semantic-knowledge-graph--rdf-sparql-11-rdfsowl-closure--shacl) |
| **Prefrontal Cortex (Safety Guard)** | Semantic rule matching against `guard_rule` memories + rootless `podman --network=none` dry-run sandbox before disk mutation. | [Prefrontal Cortex](docs/organs/prefrontal-cortex.md) |
| **Digital Twins & Telemetry** | Versioned state modeling and structural diffs (`neurostrata_twin_set` / `_get` / `_list` / `_diff`), and rolling time-series metrics (`neurostrata_metrics_query` / `_list`). | [Features §9](docs/FEATURES.md#9-digital-twins-state-versioning--neuroplasticity-telemetry) |
| **Procedural Memory & Laya Judge** | Triggered habits with finite rehearsal budgets (`neurostrata_procedure_perform`) + local trainable **Laya** duplicate/contradiction judgment. | [Procedural Memory](docs/organs/procedural-memory.md) · [Judgment](docs/architecture/domains/judgment-providers.md) |
| **SynapticMesh (Git Team Sync)** | Brokerless team memory sharing over standard `git push` / `git pull --ff-only` with sovereign private rows and visible conflict resolution. | [Features §11](docs/FEATURES.md#11-synapticmesh--brokerless-git-native-team-memory-sync) |
| **Thalamic Bus & Security** | In-process pub/sub bus, rolling 500 KB episodic session logs, fail-closed secret rejection, and caller-path authority (`J6`). *(The Obsidian vault sync is planned, not implemented — see [Obsidian](docs/ops/obsidian.md).)* | [Thalamic Bus](docs/organs/thalamic-bus.md) · [Security](docs/ops/security.md) · [Obsidian](docs/ops/obsidian.md) |



## Technical Documentation & Architecture Links

- **Documentation Index:** [NeuroStrata Documentation Hub (`docs/README.md`)](docs/README.md) · [Complete Feature & Benefit Catalog (`docs/FEATURES.md`)](docs/FEATURES.md)
- **Architecture & Cognitive Science:** [Cognitive Architecture](docs/COGNITIVE_ARCHITECTURE.md) · [SynapticGraph Engine](docs/architecture/synapticgraph.md) · [Tri-Strata Isolation](docs/architecture/tri-strata.md) · [Why Agent Memory Fails](docs/science/why-memory-fails.md) · [Nomenclature](docs/science/nomenclature.md)
- **Cognitive Organs:** [Central Executive](docs/organs/central-executive.md) · [Prefrontal Cortex](docs/organs/prefrontal-cortex.md) · [Dendritic Bridge (Embedders)](docs/organs/dendritic-bridge.md) · [Thalamic Bus](docs/organs/thalamic-bus.md) · [Procedural Memory](docs/organs/procedural-memory.md)
- **Operations & Reference:** [Zero-Touch Upgrading](docs/ops/upgrading.md) · [CLI Reference](docs/ops/cli-reference.md) · [Security & OWASP Mitigations](docs/ops/security.md) · [SynapticMesh Team Sync](docs/mesh/synaptic-mesh.md) · [Obsidian & NeuroVault (planned)](docs/ops/obsidian.md) · [Crash & WAL Recovery](docs/ops/crashes.md) · [Visual UI Guide](docs/UI_GUIDE.md) · [Building from Source](docs/BUILDING.md) · [Release Changelogs](docs/releases/changelog/)

## License

MIT — [`LICENSE`](LICENSE).
