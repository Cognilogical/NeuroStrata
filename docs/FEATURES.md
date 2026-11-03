# NeuroStrata — Complete Feature & Benefit Catalog (v1.15.0)

> Every capability in NeuroStrata exists to solve a concrete failure mode of AI coding agents: **amnesia across chats**, **ignored prompt rules**, **blind file edits without architectural context**, **blank-brain subagents**, and **unfinished work pushed to git**. Below is the complete inventory of NeuroStrata's features, what each one does for you, and where it lives in the codebase.

---

## Why Teams & Builders Use NeuroStrata

| Pain Without NeuroStrata | What NeuroStrata Does Instead | Your Payoff |
| :--- | :--- | :--- |
| **Agents forget rules between sessions** | Stores architectural rules, fixes, and failed attempts in an embedded vector + graph store anchored to exact files and symbols. | Teach a rule once; every future session recalls and enforces it automatically. |
| **Setup & upgrades require manual config** | Zero-touch init and upgrade engine detects your agent harness, installs plugins, wires git hooks, and migrates schemas automatically. | Tell your agent *"Initialize NeuroStrata"*—the agent sets up and upgrades everything itself. |
| **Subagents spawn with a blank context window** | Task creation and claiming auto-compute a hybrid vector + 1-hop graph context block and inject it into every subagent prompt. | Worker and reviewer subagents inherit governing rules and call-chain context on spawn. |
| **Agents grep blindly and break downstream callers** | Unified Tree-Sitter AST + W3C RDF/SPARQL graph exposes file governance, call chains, and transitive blast radius before edits. | Agents see what rules govern a file and what callers break before writing a line of code. |
| **Agents claim "done" with TODOs left behind** | Binary-enforced state machine (Lock 2 Done-Funnel) + pre-push git gate (Lock 1) + lexical deferred-work detector. | Work cannot close without extracting a lesson, and `git push` blocks unfinished tasks. |
| **Cloud memory tools leak proprietary code** | Single compiled Rust binary with in-process LadybugDB, Tantivy BM25, FlashRank ONNX, secret scrubbing, and git-native team sync. | 100% local by default; zero Docker, zero external DB servers, and brokerless team sync over `git`. |

---

## 1. Zero-Touch Onboarding & Self-Upgrading Lifecycle

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Zero-Touch Harness Detection** | `detect_harnesses` (`env` + repo/home marker scan) | Automatically identifies whether your agent is running in **OpenCode**, **Claude Code**, **Hermes Agent**, **OpenAI Codex CLI**, **GitHub Copilot CLI (`gh`)**, or **OpenClaw**. | `src/task/harness_plugins.rs` |
| **Embedded Zero-Dependency Plugin Installer** | `auto_install_detected_plugins` + `include_str!` bundles | Extracts pre-compiled, zero-dependency JS plugin bundles into `.NeuroStrata/plugins/<harness>/` and wires harness config files (`opencode.json`, `.claude/settings.local.json`, `.hermes/`, `.codex/`, `.openclaw/`, Copilot extension) with zero user involvement. | `src/task/harness_plugins.rs` |
| **Automatic Project Root & Namespace Inference** | `handle_get_snapshot` + `resolve_namespace` | Even if an agent calls `neurostrata_get_snapshot` without `project_root` or with case mismatches, NeuroStrata infers the repo root and resolves the canonical namespace automatically. | `src/server.rs`, `src/task/mod.rs` |
| **Self-Healing Git Hook Hygiene** | `hook_hygiene` (`pre-commit` + `pre-push`) | Every session snapshot verifies, installs, or silently upgrades stale `pre-commit` and `pre-push` git hooks (preserving backups of any custom hooks). | `src/task/mod.rs` |
| **Seamless Upgrade & Lifecycle Delta Engine** | `compute_lifecycle_delta` + `doctor_state` registry | Dropping in a new `neurostrata-mcp` binary requires zero manual migration: the next snapshot detects newly added automatic/reminder wires and triggers, backfills schema defaults on startup, and instructs the agent to apply only the delta. | `src/task/wiring.rs`, `src/store/ladybug.rs` |
| **Automated Repository Scanner (`task_setup`)** | `handle_task_setup` | Scans languages, CI workflows, existing hooks, `AGENTS.md`, and legacy `.beads` issues, returning tailored architectural rule suggestions and migration tasks. | `src/task/mod.rs` |

---

## 2. Six Zero-Dependency CLI & Terminal Agent Harness Plugins

All six plugins ship compiled inside the binary and under `plugins/` with zero external `npm` runtime dependencies:

| Harness Plugin | Target Environment | Enforced Gates & Automatic Capabilities | Path |
| :--- | :--- | :--- | :--- |
| **OpenCode Plugin** | OpenCode CLI / TUI / Desktop (`opencode.json`) | Pre-flight snapshot gate, Zero-Action Start edit/bash gate, Graph-First lookup nudge before `read`/`edit`/`grep`/`glob`, automatic `subagent` prompt context injection, Breath checkpoint gate, turn-end deferred-work watcher. | `plugins/opencode/` |
| **Claude Code Plugin** | Anthropic Claude Code CLI (`.claude/settings.local.json`) | Native `PreToolUse` & `Stop` hook protocol (`permissionDecision: "deny"` on unguarded edits, `updatedInput` automatic `Task` subagent prompt injection) plus programmatic middleware API. | `plugins/claude/` |
| **Hermes Agent Plugin** | Nous Research Hermes Agent CLI (`.hermes/plugins/`) | Pre-flight snapshot gate, Zero-Action Start gate, Graph-First lookup nudge, automatic `delegate_task` / `subagent` prompt injection, Breath gate, turn-end intent watcher. | `plugins/hermes/` |
| **OpenAI Codex CLI Plugin** | OpenAI Codex CLI (`.codex/plugins/`) | Pre-flight snapshot gate, Zero-Action Start gate, Graph-First lookup nudge, automatic `spawn_agent` / `subagent` prompt injection, Breath gate, turn-end intent watcher. | `plugins/codex/` |
| **GitHub Copilot CLI Plugin** | GitHub CLI (`gh copilot`) & Copilot Agent | Pre-flight snapshot gate, Zero-Action Start gate, Graph-First lookup nudge, automatic subagent prompt injection, Breath gate, turn-end intent watcher. | `plugins/github-cli/` |
| **OpenClaw Plugin** | OpenClaw Agent Gateway (`.openclaw/plugins/`) | Gateway lifecycle hooks, pre-flight snapshot gate, Zero-Action Start gate, automatic `subagent_prompt_injection` propagation, Breath gate. | `plugins/openclaw/` |

---

## 3. Automatic Graph-First Exploration & Subagent Context Propagation

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Auto-Retrieved Task Context on Create & Claim** | `build_task_graph_context` (hybrid vector + 1-hop graph search) | Creating or claiming a task immediately returns the top governing rules, facts, procedures, and AST nodes relevant to that task—no separate search step required. | `src/task/mod.rs` |
| **Ready-to-Paste `subagent_prompt_injection` Block** | `subagent_prompt_injection` payload + plugin interception | Every claimed task produces a formatted markdown header containing the task ID, goal, governing rules (`GOVERNS`), and call/import neighbors, automatically prepended to subagent prompts. | `src/task/mod.rs`, `plugins/*` |
| **Deterministic File Governance Lookup** | `neurostrata_memories_for_file` | Zero-embedding, instant lookup of every active rule, fact, and procedure governing a specific file, symbol, or its parent directories before an agent edits it. | `src/server.rs` |
| **Session & Task Graph Query Telemetry** | `record_graph_query` + `.NeuroStrata/session_queries.json` | Tracks every graph/memory lookup (`graph_queries_count`) per active task and session so the harness knows which files have already been inspected. | `src/task/mod.rs`, `src/server.rs` |

---

## 4. Central Executive — Enforced Task Lifecycle & The 3 Soft Locks

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **4-State Task Machine** | `open → in_progress → blocked → done` (`src/task/machine.rs`) | Strict state transitions prevent phantom completions; `neurostrata_task_update` refuses `status: "done"` and points the agent to the guarded completion funnel. | [Central Executive](organs/central-executive.md) |
| **Zero-Action Start & Exclusive Claim Leases** | `neurostrata_task_claim` (60-min stale lease takeover) | Agents cannot edit files without a claimed task; concurrent sessions cannot accidentally clobber the same task. | `src/task/mod.rs` |
| **Lock 1 — SAS Pre-Push Git Gate** | `neurostrata-mcp task gate <ns> --strict` | Blocks `git push` if any task is still `in_progress`, any `done` task lacks an extracted memory, any P0 task is open > 24h, or untracked `TODO`/`FIXME` items exist in diffs. | `src/task/gate.rs` |
| **Lock 2 — Done-Funnel Mandatory Extraction** | `neurostrata_task_complete` (`ExtractionRequired`) | A task *cannot* transition to `done` unless it extracts or links at least one architectural rule, fix, or fact (`EXTRACTED_FROM`). Every finished task makes the repo smarter. | `src/task/mod.rs` |
| **Lock 3 — The Breath Working-Memory Prompt** | `neurostrata_task_update` (`note` history) | Every 3–5 mutation steps, agents summarize state onto the task's immutable history log, enabling instant crash/session recovery without log archaeology. | `src/task/mod.rs` |
| **Lexical Task-Intent Detector** | `detect_task_intent` (`task_intent_signals`) | Scans logs and memories for deferral language (`TODO:`, `FIXME:`, `later we need to`, `we should also`) and nudges the agent to file a tracked follow-up task instead of forgetting it. | `src/task/intent.rs` | ns-0cmo |
| **Idempotent Beads Importer** | `neurostrata-mcp task import <ns> --from-beads` | Migrates existing `.beads/issues.jsonl` issue trackers into NeuroStrata tasks with dependency preservation. | `src/task/mod.rs` |

---

## 5. Memory Core — SynapticGraph, Tri-Strata & 4-Stage Hybrid Retrieval

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Embedded LadybugDB Vector + Graph Store** | In-process C++20 columnar HNSW vector + property graph engine | Runs inside the single Rust binary—zero Docker containers, zero Postgres/Qdrant setup, sub-millisecond local reads. | [SynapticGraph](architecture/synapticgraph.md) |
| **Tri-Strata Isolation & Domain Registry** | Global / Domain / Task strata + per-namespace domain slug registry | Separates machine-wide preferences (`global`), domain architecture rules, and ephemeral task state; rejects typoed domains with a clear remedy. | [Tri-Strata](architecture/tri-strata.md) |
| **Pointer-Wiki Engrams** | `locations: [{path, lines, symbol}]` + `GOVERNS` edges | Stores compact architectural rules anchored to exact file/line/symbol coordinates instead of bloating context windows with raw document dumps. | `src/server.rs` |
| **4-Stage Hybrid Search Pipeline** | Tantivy BM25 + HNSW Vector → RRF ($k=60$) → PPR Graph Expansion → FlashRank ONNX Reranker → Bi-Temporal Salience | Combines exact keyword matching, semantic similarity, 1–2 hop graph neighbors, neural cross-encoder reranking, and recency/access salience in one call (`neurostrata_search_memory`). | `src/search.rs`, `src/rerank.rs` |
| **Eidetic Recall Boot Snapshot** | `neurostrata_get_snapshot` + Leiden/Louvain community hubs | Grounds every new session in milliseconds with active tasks, due procedures, domain map, community hubs, and top high-salience rules. | `src/communities.rs`, `src/server.rs` |
| **Negative Exemplars (Failed Attempts)** | `metadata.attempt: {approach, outcome: "failed", failure_mode}` | Records dead-end approaches as first-class evidence so future sessions never waste hours repeating a failed experiment. | `src/server.rs` |
| **Non-Destructive Lifecycle (`supersede` & `archive`)** | `neurostrata_supersede_memory` + `neurostrata_archive_memory` | Outdated rules are superseded with provenance chains and reason tracking rather than silently overwritten; MCP exposes zero destructive delete tools. | `src/server.rs` |

---

## 6. Multi-Language Tree-Sitter AST Software Graph & Blast-Radius Analysis

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **11-Language Incremental AST Ingestion** | Tree-sitter parsers (`rs`, `ts/tsx`, `py`, `go`, `cs`, `java`, `c/cpp`, `html`, `css`, `sh`, `md`) | Parses functions, structs, classes, traits, imports, call sites, and markdown sections into the same graph as your memories. | `src/ingest/` |
| **Content-Hash Version-Gated Re-Ingestion** | `blake3` / `sha256` file digest watermarking + `pre-commit` hook | Only re-parses and re-embeds files whose content actually changed, keeping incremental ingestion fast enough to run automatically on commit. | `src/ingest/staged.rs` |
| **Upstream & Downstream Call-Chain Tracing** | `neurostrata_trace_call_chain` | Traces who calls a function (`upstream`), what it calls (`downstream`), or `both` up to N hops across the codebase. | `src/server.rs` |
| **Transitive Blast-Radius & Impact Analysis** | `neurostrata_impact_analysis` | Computes every file and symbol affected by modifying a target, plus every architectural rule (`GOVERNS`) that constrains those affected nodes. | `src/server.rs` |
| **GLiNER Neural Concept & Entity Extraction** | Local ONNX GLiNER extractor + candidate review/promotion | Extracts domain entities and relationships from prose into reviewable candidates (`neurostrata_enrichment_candidates`, `neurostrata_enrichment_promote`). | `src/enrichment/gliner.rs` |

---

## 7. W3C Semantic Knowledge Graph — RDF, SPARQL 1.1, OWL 2 RL & SHACL

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Embedded W3C RDF Quad Projection** | OxiRS (`oxirs-core`) quad store (`https://neurostrata.io/ns#`) | Automatically projects memories, tasks, files, symbols, and provenance edges into standard W3C RDF triples/quads. | `src/rdf/mod.rs` |
| **SPARQL 1.1 Query Engine** | `neurostrata_sparql_query` (`oxirs-arq`) | Lets agents run expressive `SELECT`, `ASK`, and `CONSTRUCT` graph queries across architecture rules, tasks, and code symbols. | `src/rdf/sparql.rs` |
| **OWL 2 RL Forward-Chaining Inference** | `oxirs-rule` ontology materializer | Automatically infers transitive and inverse governance/dependency edges across your knowledge graph. | `src/rdf/ontology.rs` |
| **Closed-World SHACL Shape Validation** | `oxirs-shacl` + closed-world task/memory shape checks | Enforces structural integrity on memory and task metadata (`shacl_enforce: true`) and surfaces ontology/shape warnings during `neurostrata_task_validate`. | `src/rdf/shacl.rs` |

---

## 8. Prefrontal Cortex — Semantic Behavioral Guard & Podman Sandbox

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Semantic Guard Rules (`guard_rule`)** | `neurostrata_guard_action` + `/validate` | Evaluates proposed commands and file mutations against natural-language safety rules stored in your memory graph before execution. | [Prefrontal Cortex](organs/prefrontal-cortex.md) |
| **Rootless Podman Dry-Run Sandbox** | `podman run --rm --network=none --read-only` (5s timeout, fail-closed) | Borderline shell commands are dry-run inside an ephemeral, network-isolated container before touching your real workspace. | `src/guard/sandbox.rs` |
| **Hostile-Path Canonicalization** | Symlink + `../` traversal resolution | Prevents agents from bypassing protected-path rules via symlinks or relative path tricks. | `src/guard/` |

---

## 9. Digital Twins, State Simulation & Neuroplasticity Telemetry

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Versioned Digital Twins** | `neurostrata_twin_create`, `update_state`, `get`, `list` | Models live system or application state as a versioned state vector inside the memory store. | `src/twin.rs` |
| **Twin Structural Diff, Rollback & Simulation** | `neurostrata_twin_diff`, `twin_rollback`, `twin_simulate` | Compares state versions, rolls back bad transitions, and simulates multi-step policy rules before applying changes to production. | `src/twin.rs` |
| **Time-Series Telemetry & Rolling Summaries** | `neurostrata_metrics_record`, `metrics_query`, `metrics_list` | Records numeric telemetry (latency, token burn, pass rates) with automatic min/max/avg/p95 rollups per namespace. | `src/metrics.rs` |

---

## 10. Procedural Memory & Judgment Organ (Laya + Jev)

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Triggered Procedural Memory** | `memory_type: "procedure"` + `neurostrata_procedure_perform` | Encodes recurring habits (`session-start`, `before-edit`, `after-mutation`, `every-n-sessions:N`, `every-n-days:N`) with finite rehearsal budgets (`remaining_fires`) surfaced automatically in every snapshot. | [Procedural Memory](organs/procedural-memory.md) | ns-dad0 |
| **Local Laya Duplicate & Contradiction Judge** | `src/judgment/laya_onnx.rs` + `neurostrata-mcp laya train` | Locally scores candidate memories for duplicates or contradictions using cosine similarity, token Jaccard, structural anchor overlap, and trainable centroid-margin weights—100% offline. | `src/judgment/laya_onnx.rs` |
| **Optional TypeSafe Jev Judgment Bridge** | `src/judgment/typesafe_jev.rs` | Optional remote duplicate judgment integration via TypeSafe AI without moving your vector database off-machine. | [Judgment Providers](architecture/domains/judgment-providers.md) |

---

## 11. SynapticMesh — Brokerless Git-Native Team Memory Sync

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Git-Transport Team Memory Sharing** | `synaptic-mesh-record/v1` + durable WAL outbox | Syncs `eligibility: "shareable"` rules, facts, and procedure definitions across teammates over standard `git pull --ff-only` and `git push`—no SaaS or message broker required. | [SynapticMesh](mesh/synaptic-mesh.md) |
| **Sovereign Private Rows & Loud Conflict Preservation** | `neurostrata_mesh_status`, `mesh_conflicts`, `mesh_resolve` | Private memories, tasks, and local procedure counters never leave your machine; conflicting edits between teammates are preserved visibly and never silently overwritten. | `src/synaptic_mesh/` |

---

## 12. Thalamic Bus, Episodic Buffer, Obsidian Sync & Local Security

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Thalamic In-Process Event Bus** | `Arc<ThalamicBus>` (`Created`, `Superseded`, `Archived`, `GuardedActionFired`) | Automatically updates episodic pointers, marks graph exports dirty, and logs guard events whenever a memory mutates. | [Thalamic Bus](organs/thalamic-bus.md) |
| **Episodic Buffer Session Logs** | `neurostrata_append_log` + `### 🔄 Topic Switch` + 500 KB rollover | Maintains a rolling, searchable Markdown transcript of reasoning and topic switches in `.NeuroStrata/sessions/`. | `src/server.rs` |
| **Bidirectional Obsidian Vault Sync** | `neurostrata-mcp obsidian-sync` | Exports and syncs your project's memory graph as human-readable Markdown notes with YAML frontmatter and `[[wikilinks]]`. | [Obsidian Integration](ops/obsidian.md) |
| **Loopback-Only Daemon & Secret Scrubbing** | `127.0.0.1:34343` + regex/entropy secret scrubber + Cypher escaping | Automatically redacts API keys, tokens, and credentials before storage, enforces strict Host/Origin headers, and exposes WAL health (`neurostrata_wal_status`, `neurostrata_storage_status`). | [Security](ops/security.md) |

---

## Complete Technical Documentation Index

- **Architecture & Core Science**
  - [Cognitive Architecture Overview](COGNITIVE_ARCHITECTURE.md)
  - [SynapticGraph — Embedded Vector & Graph Engine](architecture/synapticgraph.md)
  - [Tri-Strata — Global, Domain, and Task Isolation](architecture/tri-strata.md)
  - [Why Agent Memory Fails](science/why-memory-fails.md)
  - [Cognitive Nomenclature Guide](science/nomenclature.md)
- **The Cognitive Organs**
  - [Central Executive — Task Lifecycle & Soft Locks](organs/central-executive.md)
  - [Prefrontal Cortex — Semantic Guard & Podman Sandbox](organs/prefrontal-cortex.md)
  - [Dendritic Bridge — Local & Remote Embedding Providers](organs/dendritic-bridge.md)
  - [Thalamic Bus — In-Process Pub/Sub & Freshness Gate](organs/thalamic-bus.md)
  - [Procedural Memory — Triggered Habits & Rehearsal Budgets](organs/procedural-memory.md)
- **Team Sync & Operations**
  - [SynapticMesh — Brokerless Git Team Memory](mesh/synaptic-mesh.md)
  - [CLI Reference](ops/cli-reference.md)
  - [Security & OWASP LLM Mitigations](ops/security.md)
  - [Upgrading & Zero-Touch Migrations](ops/upgrading.md)
  - [Obsidian Vault Sync](ops/obsidian.md)
  - [Crash Diagnostics & WAL Recovery](ops/crashes.md)
  - [Building from Source](BUILDING.md)
  - [Release Changelogs](releases/changelog/)