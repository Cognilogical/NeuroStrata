# NeuroStrata — Complete Feature & Benefit Catalog (v1.17.2)

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
| **Cloud memory tools leak proprietary code** | Single compiled Rust binary with in-process LadybugDB, Tantivy full-text, FastEmbed ONNX embeddings, fail-closed secret rejection, and git-native team sync. | 100% local by default; zero Docker, zero external DB servers, and brokerless team sync over `git`. |

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

All six plugins ship compiled inside the binary and under `plugins/` with zero external `npm` runtime dependencies.

| Harness Plugin | Target Environment | Enforced Gates & Automatic Capabilities | Path |
| :--- | :--- | :--- | :--- |
| **OpenCode Plugin** | OpenCode CLI / TUI / Desktop (`opencode.json`) | Pre-flight snapshot gate, Zero-Action Start edit/bash gate, Graph-First lookup nudge before `read`/`edit`/`grep`/`glob`, automatic `subagent` prompt context injection, Breath checkpoint gate, turn-end deferred-work watcher. | `plugins/opencode/` |
| **Claude Code Plugin** | Anthropic Claude Code CLI (`.claude/settings.local.json`) | Native `PreToolUse` & `Stop` hook protocol (`permissionDecision: "deny"` on unguarded edits, `updatedInput` automatic `Task` subagent prompt injection) plus programmatic middleware API. | `plugins/claude/` |
| **Hermes Agent Plugin** | Nous Research Hermes Agent CLI (`.hermes/plugins/`) | Pre-flight snapshot gate, Zero-Action Start gate, Graph-First lookup nudge, automatic `delegate_task` / `subagent` prompt injection, Breath gate, turn-end intent watcher. | `plugins/hermes/` |
| **OpenAI Codex CLI Plugin** | OpenAI Codex CLI (`.codex/plugins/`) | Pre-flight snapshot gate, Zero-Action Start gate, Graph-First lookup nudge, automatic `spawn_agent` / `subagent` prompt injection, Breath gate, turn-end intent watcher. | `plugins/codex/` |
| **GitHub Copilot CLI Plugin** | GitHub CLI (`gh copilot`) & Copilot Agent | Pre-flight snapshot gate, Zero-Action Start gate, Graph-First lookup nudge, automatic subagent prompt injection, Breath gate, turn-end intent watcher. | `plugins/github-cli/` |
| **OpenClaw Plugin** | OpenClaw Agent Gateway (`.openclaw/plugins/`) | Gateway lifecycle hooks, pre-flight snapshot gate, Zero-Action Start gate, automatic `subagent_prompt_injection` propagation, Breath gate. | `plugins/openclaw/` |

**One shared implementation behind four of the six (v1.16.0).** The OpenCode, Claude Code, Codex and GitHub CLI plugins are now 13-line adapters that re-export a single shared core (`plugins/core/index.ts`, ~1.8k lines, Node built-ins only) — a bug fix in the core reaches all four at once instead of four edits. The core is embedded in the binary (`include_str!`) and installed into `.NeuroStrata/plugins/core/` on init, so no npm install is needed. **The Hermes (863 lines) and OpenClaw (1000 lines) bundles still carry their own implementations and were not converted to the shared core in v1.16.0.**

| Shared Core Capability | Engineering Primitive | Benefit — What It Means For You | Path |
| :--- | :--- | :--- | :--- |
| **Project-Namespace Discovery** | `resolveConfig` + `findProjectRoot` + `discoverProjectNamespace` | The namespace is taken from the plugin config, then `NEUROSTRATA_NAMESPACE`, then the project root itself — never a hardcoded name, so the guard and the task list read the right namespace in any repository. | `plugins/core/index.ts` |
| **Canonical Tool Classification** | `TOOL_CLASSIFICATION` + `isFileTouchTool` / `isMutatingTool` / `isSubagentTool` / `isCodeExecTool` | One shared answer to "is this a read, a write, a shell call, or a subagent?" across every harness and every tool alias. | `plugins/core/index.ts` |
| **OpenCode V2 Code Mode Inspection** | `inspectCodeModeExecution` | A claim, a note or a completion written inside a Code Mode `execute` block is recognised and tracked like any other tool call. | `plugins/core/index.ts` |
| **Unified `PreToolUse` / `Stop` Handling** | `handleHookEvent` / `handleClaudeHookEvent` | Subagent prompt enrichment, file-touch recall delivery, and the mutating-tool guard behave the same in every harness. | `plugins/core/index.ts` |
| **LLM-Visible File-Touch Recall** | `prefetchMemoriesForFile` + `formatFileTouchRecallBlock` | The governing memories of a file the agent is about to touch are put in front of the model, not just recorded in a ledger. | `plugins/core/index.ts` |

---

## 3. Automatic Graph-First Exploration & Subagent Context Propagation

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Auto-Retrieved Task Context on Create & Claim** | `build_task_graph_context` (hybrid vector + 1-hop graph search) | Creating or claiming a task immediately returns the top governing rules, facts, procedures, and AST nodes relevant to that task—no separate search step required. | `src/task/mod.rs` |
| **Ready-to-Paste `subagent_prompt_injection` Block** | `subagent_prompt_injection` payload + plugin interception | Every claimed task produces a formatted markdown header containing the task ID, goal, governing rules (`GOVERNS`), and call/import neighbors, automatically prepended to subagent prompts. | `src/task/mod.rs`, `plugins/*` |
| **Deterministic File Governance Lookup** | `neurostrata_memories_for_file` | Zero-embedding, instant lookup of every active rule, fact, and procedure governing a specific file, symbol, or its parent directories before an agent edits it. | `src/server.rs` |
| **Session & Task Graph Query Telemetry** | `record_graph_query` + `.NeuroStrata/session_queries.json` | Tracks every graph/memory lookup (`graph_queries_count`) per active task and session so the harness knows which files have already been inspected. | `src/task/mod.rs`, `src/server.rs` |
| **AST Neighborhood Recall (v1.16.0)** | `include_ast_edges` (default `true`) on `neurostrata_memories_for_file` | One lookup returns the file's governing memories, its 1-hop `DEPENDS_ON` / `RELATES_TO` neighbours and `SUPERSEDES` targets, and a compact AST summary — `symbols`, `callers`, `callees`, `imports`, `importers`, `blast_radius_count` — so an agent sees the blast radius before it edits. | `src/store/ladybug.rs`, `src/server.rs` |
| **Daemon-Backed Active Task Resolution (v1.16.0)** | `handle_guard_validate` session fallback + `session_id` / `subagent_prompt_injection` on `in_progress` task rows | A plugin that does not track sessions still resolves the active task, its session and its prompt block in one call — the guard stops treating a stateless client as an unattributed caller. | `src/server.rs`, `src/task/mod.rs` |
| **Dynamic Subagent Prompt Injection (v1.16.0)** | `enrichAndInjectSubagentPrompt` + `extractRepoFilePaths` | Every repo-relative file path named in a subagent prompt has its governing memories prefetched and embedded in the injected header — a subagent starts with the rules for the files it was told to touch, not just the rules for the task. | `plugins/core/index.ts` |
| **Verifiable Graph-First Predicate (v1.16.0)** | `SUBAGENT_GRAPH_FIRST_RULE_SNIPPET` + `build_task_graph_context` | The Graph-First rule now names a check an agent can actually run ("for any file you touch that is not listed under `### NeuroStrata Task & Graph Context`, call `neurostrata_memories_for_file` first") instead of a prose bypass. | `src/task/mod.rs`, `plugins/core/index.ts` |

---

## 4. Central Executive — Enforced Task Lifecycle & The 3 Soft Locks

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **4-State Task Machine** | `open → in_progress → blocked → done` (`src/task/machine.rs`) | Strict state transitions prevent phantom completions; `neurostrata_task_update` refuses `status: "done"` and points the agent to the guarded completion funnel. | [Central Executive](organs/central-executive.md) |
| **Zero-Action Start & Exclusive Claim Leases** | `neurostrata_task_claim` (60-min stale lease takeover) | Agents cannot edit files without a claimed task; concurrent sessions cannot accidentally clobber the same task. | `src/task/mod.rs` |
| **Lock 1 — SAS Pre-Push Git Gate** | `neurostrata-mcp task gate <ns> --strict` | Blocks `git push` if any task is still `in_progress`, any `done` task lacks an extracted memory, or any P0 task has been open > 24h. | `src/task/gate.rs` |
| **Lock 2 — Done-Funnel Mandatory Extraction** | `neurostrata_task_complete` (`ExtractionRequired`) | A task *cannot* transition to `done` unless it extracts or links at least one architectural rule, fix, or fact (`EXTRACTED_FROM`). Every finished task makes the repo smarter. | `src/task/mod.rs` |
| **Lock 3 — The Breath Working-Memory Prompt** | `neurostrata_task_update` (`note` history) | Every 3–5 mutation steps, agents summarize state onto the task's immutable history log, enabling instant crash/session recovery without log archaeology. | `src/task/mod.rs` |
| **Lexical Task-Intent Detector** | `detect_task_intent` (`task_intent_signals`) | Scans logs and memories for deferral language (`TODO:`, `FIXME:`, `later we need to`, `we should also`) and nudges the agent to file a tracked follow-up task instead of forgetting it. | `src/task/intent.rs` | ns-0cmo |
| **Idempotent Beads Importer** | `neurostrata-mcp task import --from-beads <path/to/issues.jsonl> <ns>` | Migrates an exported `.beads` issue tracker (`bd export .beads/beads.jsonl`) into NeuroStrata tasks with dependency preservation. | `src/task/mod.rs` |

---

## 5. Memory Core — SynapticGraph, Tri-Strata & Three Retrieval Surfaces

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Embedded LadybugDB Vector + Graph Store** | In-process C++20 columnar HNSW vector + property graph engine | Runs inside the single Rust binary—zero Docker containers, zero Postgres/Qdrant setup, sub-millisecond local reads. | [SynapticGraph](architecture/synapticgraph.md) |
| **Tri-Strata Isolation & Domain Registry** | Global / Domain / Task strata + per-namespace domain slug registry | Separates machine-wide preferences (`global`), domain architecture rules, and ephemeral task state; rejects typoed domains with a clear remedy. | [Tri-Strata](architecture/tri-strata.md) |
| **Pointer-Wiki Engrams** | `locations: [{path, lines, symbol}]` + `GOVERNS` edges | Stores compact architectural rules anchored to exact file/line/symbol coordinates instead of bloating context windows with raw document dumps. | `src/server.rs` |
| **Deterministic Retrieval — Three Surfaces, Not One Fused Pipeline** | `mode: vector` (LadybugDB HNSW + boosted distance) / `mode: lexical` (Tantivy full-text) / `mode: lexical-inhouse` (5-field BM25), plus bounded 1–2 hop graph expansion | The three modes are alternatives, not a fusion — the caller picks one. The vector surface filters row classes first, then subtracts a usage-and-provenance gain (capped at 0.5 × distance) and a decision-shape boost; the graph expansion is a bounded walk over `RELATES_TO` / `CONTAINS` / `GOVERNS` / `EXTRACTED_FROM` with per-family caps, ranked by co-occurrence count. **There is no rank fusion (RRF), no Personalized PageRank, and no neural reranker in the search path.** | `src/server.rs`, `src/store/ladybug.rs`, `src/search.rs`, `src/fts/mod.rs`, `src/lexical.rs` |
| **Eidetic Recall Boot Snapshot** | `neurostrata_get_snapshot` + Leiden community hubs | Grounds every new session in milliseconds with active tasks, due procedures, domain map, community hubs, and top high-salience rules. | `src/community.rs`, `src/server.rs` |
| **Negative Exemplars (Failed Attempts)** | `metadata.attempt: {approach, outcome: "failed", failure_mode}` | Records dead-end approaches as first-class evidence so future sessions never waste hours repeating a failed experiment. | `src/server.rs` |
| **Non-Destructive Lifecycle (`supersede` & `archive`)** | `neurostrata_supersede_memory` + `neurostrata_archive_memory` | Outdated rules are superseded with provenance chains and reason tracking rather than silently overwritten; MCP exposes zero destructive delete tools. | `src/server.rs` |

---

## 6. Six-Language Tree-Sitter AST Software Graph & Blast-Radius Analysis

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Six-Language Incremental AST Ingestion** | Tree-sitter parsers (`rust`, `typescript`, `tsx`, `python`, `go`, `java` — see `src/parser/mod.rs::get_language`) | Parses functions, structs, classes, traits, imports, and call sites into the same graph as your memories. | `src/parser/` |
| **Content-Hash Version-Gated Re-Ingestion** | `sha256` file digest watermarking + `pre-commit` hook | Only re-parses and re-embeds files whose content actually changed, keeping incremental ingestion fast enough to run automatically on commit. | `src/parser/generation.rs` |
| **Upstream & Downstream Call-Chain Tracing** | `ast_summary` on `neurostrata_memories_for_file` | Returns `callers`, `callees`, `imports` and `importers` for a file, computed from the ingested Tree-sitter AST edges. | `src/store/ladybug.rs` |
| **Transitive Blast-Radius & Impact Analysis** | `blast_radius_count` + `related_memories` on `neurostrata_memories_for_file` | Reports how many AST neighbours a file has and returns the 1-hop `DEPENDS_ON` / `RELATES_TO` neighbours plus `SUPERSEDES` targets, alongside the rules that `GOVERNS` them. | `src/store/ladybug.rs` |
| **GLiNER Neural Concept & Entity Extraction** | Local ONNX GLiNER extractor (`extract_entities: true` on `neurostrata_add_memory`) | Extracts domain entities and relationships from prose into reviewable candidates stored as `metadata.gliner_candidates` on the memory — there are no separate enrichment MCP tools. | `src/gliner.rs` |

---

## 7. W3C Semantic Knowledge Graph — RDF, SPARQL 1.1, RDFS/OWL Closure & SHACL

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Embedded W3C RDF Quad Projection** | `oxrdf` / `oxttl` quad store (`https://neurostrata.io/ns#`) | Automatically projects memories, tasks, files, symbols, and provenance edges into standard W3C RDF triples/quads. | `src/rdf/mod.rs` |
| **SPARQL 1.1 Query Engine** | `neurostrata_sparql_query` (`spargebra` / `sparopt` / `spareval` / `sparesults`) | Lets agents run expressive `SELECT`, `ASK`, and `CONSTRUCT` graph queries across architecture rules, tasks, and code symbols. | `src/rdf/sparql.rs` |
| **RDFS/OWL Closure Inference** | `src/rdf/ontology.rs` + `src/rdf/reasoning.rs` (transitive `subPropertyOf`, subclass closure, bounded `GOVERNS` closure) | Infers transitive governance and dependency edges across your knowledge graph, with a query budget. Computed inside the store — there is no separate `neurostrata_owl_reason` MCP tool. | `src/rdf/ontology.rs` |
| **Closed-World SHACL Shape Validation** | the `shacl` crate + closed-world task/memory shape checks | Enforces structural integrity on memory and task metadata (`shacl_enforce: true`) and surfaces ontology/shape warnings during `neurostrata_task_validate`. | `src/rdf/shacl.rs` |

---

## 8. Prefrontal Cortex — Semantic Behavioral Guard & Podman Sandbox

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Semantic Guard Rules (`guard_rule`)** | `neurostrata_guard_validate` (+ `neurostrata_guard_learn`, `neurostrata_guard_list_rules`) | Evaluates proposed commands and file mutations against natural-language safety rules stored in your memory graph before execution. | [Prefrontal Cortex](organs/prefrontal-cortex.md) |
| **Rootless Podman Dry-Run Sandbox** | `podman run --rm --network=none -v=<cwd>:/workspace:ro --tmpfs=/tmp` (5s timeout, fail-closed) | Borderline shell commands are dry-run inside an ephemeral, network-isolated container before touching your real workspace. | `src/guard/sandbox.rs` |
| **Hostile-Path Canonicalization** | Symlink + `../` traversal resolution | Prevents agents from bypassing protected-path rules via symlinks or relative path tricks. | `src/guard/` |

---

## 9. Digital Twins, State Versioning & Neuroplasticity Telemetry

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Versioned Digital Twins** | `neurostrata_twin_set`, `neurostrata_twin_get`, `neurostrata_twin_list` | Models live system or application state as a versioned state vector inside the memory store. | `src/digital_twin/` |
| **Twin Structural Diff** | `neurostrata_twin_diff` | Compares recorded state versions structurally so an agent can see what changed before mutating production. | `src/digital_twin/` |
| **Time-Series Telemetry & Rolling Summaries** | `neurostrata_metrics_query`, `neurostrata_metrics_list` | Reads rolling telemetry summaries (latency, token burn, pass rates) with min/max/avg/p95 rollups per namespace. There is no `neurostrata_metrics_record` tool — telemetry is written by the runtime, not by the agent. | `src/time_series.rs` |

---

## 10. Procedural Memory & Judgment Organ (Laya + Jev)

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Triggered Procedural Memory** | `memory_type: "procedure"` + `neurostrata_procedure_perform` | Encodes recurring habits (`session-start`, `before-edit`, `after-mutation`, `every-n-sessions:N`, `every-n-days:N`) with finite rehearsal budgets (`remaining_fires`) surfaced automatically in every snapshot. | [Procedural Memory](organs/procedural-memory.md) | ns-dad0 |
| **Local Laya Duplicate & Contradiction Judge** | `src/judgment/laya_onnx.rs` + `neurostrata-mcp laya <model>` (fetches and verifies the pinned ONNX bundle) | Scores candidate memories for duplicates or contradictions locally against the bundled ONNX model—100% offline. A pure-Rust training loop lives in `src/learning/trainer.rs` and is driven by `neurostrata-mcp learn train`. | `src/judgment/laya_onnx.rs` |
| **Optional TypeSafe Jev Judgment Bridge** | `src/judgment/typesafe_jev.rs` | Optional remote duplicate judgment integration via TypeSafe AI without moving your vector database off-machine. | [Judgment Providers](architecture/domains/judgment-providers.md) |

---

## 11. SynapticMesh — Brokerless Git-Native Team Memory Sync

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Git-Transport Team Memory Sharing** | `synaptic-mesh-record/v1` + durable WAL outbox | Syncs `eligibility: "shareable"` rules, facts, and procedure definitions across teammates over standard `git pull --ff-only` and `git push`—no SaaS or message broker required. | [SynapticMesh](mesh/synaptic-mesh.md) |
| **Sovereign Private Rows & Loud Conflict Preservation** | `neurostrata_synaptic_mesh_status` + the `neurostrata-mcp synaptic-mesh` CLI | Private memories, tasks, and local procedure counters never leave your machine; conflicting edits between teammates are preserved visibly and never silently overwritten. | `src/synaptic_mesh/` |

---

## 12. Thalamic Bus, Episodic Buffer & Local Security

| Feature | Engineering Primitive | Benefit — What It Means For You | Source / Docs |
| :--- | :--- | :--- | :--- |
| **Thalamic In-Process Event Bus** | `Arc<ThalamicBus>` (`Created`, `Superseded`, `Archived`, `GuardedActionFired`) | Automatically updates episodic pointers, marks graph exports dirty, and logs guard events whenever a memory mutates. | [Thalamic Bus](organs/thalamic-bus.md) |
| **Episodic Buffer Session Logs** | `neurostrata_append_log` + `### 🔄 Topic Switch` + 500 KB rollover | Maintains a rolling, searchable Markdown transcript of reasoning and topic switches in `.NeuroStrata/sessions/`. | `src/server.rs` |
| **Obsidian / NeuroVault Sync — *planned, not implemented*** | — | **Not shipped.** There is no `neurostrata_sync_obsidian` MCP tool, no `obsidian-sync` CLI subcommand, and no `src/obsidian.rs` module. The feature is tracked by task `neurostrata-dlag`; use the web UI or `neurostrata-mcp export-graph` to read the graph today. | [Obsidian Integration](ops/obsidian.md) |
| **Loopback-Only Daemon & Secret Rejection** | `127.0.0.1:34343` + regex/entropy secret scanner + Cypher string escaping | **Rejects** any write whose payload contains an API key, token, JWT or other credential, so the secret never reaches a vector index, the Episodic Buffer or a Git-shared mesh record; enforces strict Host/Origin headers, and exposes store health (`neurostrata_storage_status`). | [Security](ops/security.md) |
| **Daemon Caller-Path Authority (`J6`)** | `check_caller_path` + `allowed_external_dirs` (`src/daemon.rs`, `src/config.rs`) | Every caller-supplied filesystem path on `/ingest`, `/backup`, `/export-graph`, and `/tasks/import` is canonicalized and verified against the startup project root or `allowed_external_dirs` allowlist before the daemon reads or writes a byte (`neurostrata-p1ex`). | `src/daemon.rs`, [Security](ops/security.md) |

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
  - [Obsidian Vault Sync — planned, not implemented](ops/obsidian.md)
  - [Crash Diagnostics & WAL Recovery](ops/crashes.md)
  - [Building from Source](BUILDING.md)
  - [Release Changelogs](releases/changelog/)
