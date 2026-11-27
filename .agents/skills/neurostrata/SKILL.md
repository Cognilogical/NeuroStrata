---
name: neurostrata
description: 'Manage the 3-Tier Memory Architecture (Global, Domain, Task), Central Executive Task Machine, Automatic Graph-First Exploration, and Subagent Context Propagation. Replaces legacy MEMORY.md and bd remember.'
---

# NeuroStrata (v1.18.0 — Tri-Strata Memory, Central Executive, Enterprise Ontology & Subject-Matter Domain Expertise)

## Overview

NeuroStrata (`neurostrata-mcp`, `neurostrata-lf3p`) is a single compiled Rust binary housing an embedded vector, property graph, full-text, and W3C RDF/SPARQL 1.1/SHACL engine (LadybugDB + Tantivy + `oxrdf`/`oxttl` + `spargebra`/`spareval` + `shacl` + Laya). It completely replaces `MEMORY.md`, local markdown TODO lists, `TodoWrite`, and deprecated `bd` commands with binary-enforced task and memory gates.

---

## 0. The Prime Directive: Never Defer Anything

- **Highest-priority mandate:** Never leave any task, feature, or implementation in a `"deferred"`, `"todo"`, `"stub"`, `"phase-2"`, or `"later"` state.
- Every open item must be either:
  1. Completed, verified, and committed to `git` in the current session, OR
  2. Filed as a concrete tracked task via `neurostrata_task_create` with an assignee, description, and blockers documented.

---

## 1. Zero-Touch Onboarding & Self-Upgrading Lifecycle

- **Zero-Touch Harness Auto-Install (`src/task/harness_plugins.rs`):** On init (`neurostrata_bootstrap`, `neurostrata_task_setup`, and `neurostrata_get_snapshot`), NeuroStrata automatically detects the active/installed CLI agent harness (`opencode`, `openclaw`, `claude`, `hermes`, `codex`, `github-cli`) from environment and project markers, extracts the compiled zero-dependency plugin into `.NeuroStrata/plugins/<harness>/`, and wires the harness config (`opencode.json`, `.claude/settings.local.json`, `.hermes/`, `.codex/`, `.openclaw/`, Copilot CLI) with zero user involvement.
- **Self-Healing Dual Git Hooks (`pre-push` + `pre-commit`):** `neurostrata_get_snapshot` (`hook_hygiene`) and `neurostrata-mcp hooks install` automatically install and refresh both `.git/hooks/pre-push` (strict task gate) and `.git/hooks/pre-commit` (graph-query ledger), backing up any foreign hooks safely.
- **Upgrade & Lifecycle Delta Engine (`src/task/wiring.rs`):** When a new binary version ships, the next `neurostrata_get_snapshot` automatically backfills schema defaults, upgrades stale hooks/plugins, and reports `LIFECYCLE & HOOK DELTA` if new wires or triggers need registration.

---

## 2. Central Executive — Mandatory Task Workflow & The 3 Soft Locks

- **Pre-Flight First Action (Zero-Trust Policy):** Your VERY FIRST action on any new session or task MUST be `neurostrata_get_snapshot` (`namespace`, `project_root` — `project_root` is inferred automatically if omitted) or `neurostrata_search_memory`.
- **Zero-Action Start:** You are strictly blocked from editing files (`write`, `edit`) or running mutating shell commands until a task exists and is claimed by your session:
  1. Check existing work: `neurostrata_task_list` (`namespace`, `ready: true`).
  2. Create new work if needed: `neurostrata_task_create` (`namespace`, `title`, `description`, `priority`, `task_type`: `"task" | "bug" | "feature" | "epic"` — never `"recurring"`).
  3. Claim exclusively: `neurostrata_task_claim` (`id`, `namespace`, `assignee`, `session_id`). Claims older than 60 minutes are taken over automatically.
- **Lock 3 — The Breath Working-Memory Prompt:** Every 3–5 mutation steps, pause and append a state summary to the task using `neurostrata_task_update` (`id`, `namespace`, `session_id`, `note`). Note that `status: "done"` is refused by `neurostrata_task_update`.
- **Lock 2 — Done-Funnel Mandatory Extraction:** Close a task ONLY via `neurostrata_task_complete` (`id`, `namespace`, `session_id`, `note`, and `memory: { content, memory_type: "rule" | "fact", metadata: { source, governs } }` or `link_memory_id`). With zero extracted memories, `neurostrata_task_complete` fails with `BLOCKED (Lock 2)`.
- **Lock 1 — Pre-Push Git Gate:** `git push` runs `neurostrata-mcp task gate <namespace> --strict`. Any `in_progress` task, `done` task without an extracted memory, open P0 > 24h, or untracked work-item debt blocks the push.
- **Task vs Procedure:**
  - A **task** finishes once (`neurostrata_task_complete`).
  - A **procedure** is a recurring habit with a finite rehearsal budget (`remaining_fires`) created via `neurostrata_add_memory` (`memory_type: "procedure"`, `metadata.trigger`: `"session-start" | "before-edit" | "after-mutation" | "every-n-sessions:N" | "every-n-days:N"`) and fired via `neurostrata_procedure_perform`.
- **Lexical Task-Intent Recognition:** Deferral phrases (`TODO:`, `FIXME:`, `follow-up:`, `later we need to`, `we should also`) in speech, logs, or memories are surfaced as `task_intent_signals` and `TASK NUDGE` — capture them immediately with `neurostrata_task_create`.

---

## 3. Automatic Graph-First Exploration & Subagent Context Propagation

- **Auto-Retrieved Task Context:** `neurostrata_task_create` and `neurostrata_task_claim` automatically execute a hybrid vector + 1-hop graph search on the task's title/description and return `auto_retrieved_context` plus a ready-to-paste `subagent_prompt_injection` markdown block.
- **Graph-First Lookup Reflex:** Before using blind `grep`/`glob` or editing a file, inspect governing rules (`GOVERNS`) and AST call/import edges (`DEPENDS_ON`, `calls`, `imports`) using:
  - `neurostrata_memories_for_file` (`namespace`, `path`, optional `symbol`, optional `include_ast_edges`) — deterministic, zero-embedding file governance lookup; with `include_ast_edges` (default `true`) it also returns `related_memories` and an `ast_summary` (`symbols`, `callers`, `callees`, `imports`, `importers`, `blast_radius_count`) — this is the call-chain and blast-radius lookup.
  - `neurostrata_related` (`namespace`, `id`, `depth`) — 1–2 hop graph neighborhood.
  - `neurostrata_search_memory` (`mode`: `"hybrid"` (default: HNSW + Tantivy BM25 + 1-hop graph expansion fused via Reciprocal Rank Fusion) | `"vector"` | `"lexical"` | `"lexical-inhouse"`, optional `include_references: true` for `edge_sources` provenance) — the hybrid track fuses vector, full-text and graph signals; the other modes are single-track alternatives. `include_related` is a deprecated alias for `include_references` and emits a warning.
  - `neurostrata_sparql_query` (`namespace`, `query`) — W3C SPARQL 1.1 (`SELECT`, `ASK`, `CONSTRUCT`) over the `oxrdf` RDF store.
  - Every lookup increments `task.graph_queries_count` and records into `.NeuroStrata/session_queries.json`.
- **Verifiable Graph-First predicate:** Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed under `### NeuroStrata Task & Graph Context` in your prompt. For any file you touch that is NOT listed in that block, call `neurostrata_memories_for_file` first.
- **Subagent Prompt Propagation:** Subagents start with a blank context window. On session init, write the Graph-First rule into your active plan, and whenever you spawn ANY subagent (`subagent`, `Task`, `delegate_task`, `spawn_agent`), paste the `subagent_prompt_injection` block from `neurostrata_task_claim` into the subagent's prompt (the 6 harness plugins also prepend it automatically).

---

## 4. Tri-Strata Memory, Negative Exemplars & Non-Destructive Lifecycle

- **Tri-Strata Isolation:**
  1. `namespace="global"` — Machine-wide rules ONLY when explicitly requested by the user. There is no separate global directory: `global` is a namespace **column** in the same LadybugDB store (`Memory.namespace`), and the config file is `~/.config/neurostrata/config.json`.
  2. `namespace="<ProjectName>"` — Project domain rules, facts, procedures, and AST graph nodes. Manage domain slugs via `neurostrata_domain_registry` (actions `list` / `set` / `remove`).
  3. Task Stratum — Task working memory stored on the task record via `neurostrata_task_update` (`note`) and linked via `EXTRACTED_FROM`.
- **Pointer-Wiki Engrams & Anchors:** Pass `locations: [{path, lines, symbol}]` and `metadata.governs: ["<Namespace>::<rel_path>"]` on `neurostrata_add_memory` so rules attach directly to file and symbol nodes in the graph.
- **Negative Exemplars (Failed Attempts):** Record dead-end approaches using `metadata.attempt: { approach, outcome: "failed", failure_mode }` so future sessions never repeat a failed experiment.
- **Non-Destructive Corrections:** Never use `bd remember` or `MEMORY.md`. Correct outdated memories with `neurostrata_supersede_memory` (`id` of the memory being replaced, its `namespace`, new `content`, `reason`) or retire with `neurostrata_archive_memory`. Deletion is deliberately absent from MCP.
- **Episodic Buffer:** Maintain a running session log using `neurostrata_append_log` (`namespace`, `project_root`, `content`, `tags`), which writes rolling 500 KB transcripts to `.NeuroStrata/sessions/` and returns `task_intent_signals`.

---

## 5. Prefrontal Cortex Guard, Digital Twins, Telemetry & SynapticMesh

- **Prefrontal Cortex (`neurostrata_guard_validate`):** Evaluates proposed shell commands and file edits against learned behavioral rules (`neurostrata_guard_learn`, `neurostrata_guard_list_rules`) and dry-runs borderline commands in a rootless Podman sandbox (`podman run --rm --network=none -v=<cwd>:/workspace:ro --tmpfs=/tmp`, 5 s timeout).
- **Digital Twins & Telemetry:** Model state with `neurostrata_twin_set`, `neurostrata_twin_get`, `neurostrata_twin_list` and `neurostrata_twin_diff`, and read rolling time-series summaries with `neurostrata_metrics_query` and `neurostrata_metrics_list`.
- **SynapticMesh (`neurostrata_synaptic_mesh_*`):** Shares `eligibility: "shareable"` rules, facts, and procedure definitions (stripped of local runtime counters) over plain `git`. Private rows are sovereign; conflicts are preserved visibly (`neurostrata_synaptic_mesh_status`) and never force-pushed. The CLI surface is `neurostrata-mcp synaptic-mesh` (alias `mesh`) with `init`, `join`, `leave`, `pull`, `push` and `status`.
- **Daemon Caller-Path Authority (`J6`):** Every caller-supplied filesystem path on `/ingest`, `/backup`, `/export-graph`, and `/tasks/import` is canonicalized via `check_caller_path` and must reside inside `startup_root` or `allowed_external_dirs` in `~/.config/neurostrata/config.json`.

---

## 6. Subject-Matter Domain Expertise, SKOS Concept Taxonomy & SME Frontier Advisor (`neurostrata_taxonomy`)

- **Operations (`set_thesis`, `upsert_concept`, `tree`, `briefing`, `compare`, `gaps`, `advise`):**
  - `set_thesis`: Defines or atomically supersedes the singleton Project Thesis (`primary_domain`, `mission`, `direction`, `core_pillars`, `adjacent_domains`, `evaluation_axes`, `non_goals`, `open_questions`).
  - `upsert_concept`: Defines or atomically supersedes a canonical SKOS concept (`slug`, `pref_label`, `definition`, `domain`, `aliases`, `broader`, `related_concepts`, `contrasts_with`, `tradeoffs`, `references`, license-verified `comparable_implementations`, `realization_status`, `current_limitations`, `frontier_opportunities`, `implemented_interfaces`, `linked_action_types`, `evaluation_scores`, `evidence_refs`), enforcing DAG cycle-freedom and transitive W3C SKOS S27 disjointness.
  - `tree` / `briefing` / `compare` / `gaps`: Principal-filtered SKOS hierarchy, Forward-Expand / Backward-Prune multi-hop subject-matter briefings, multi-perspective comparative matrices, and epistemic coverage (`0..10000` bps) + ontology anti-pattern lints (`OrphanConcept`, `OverloadedUmbrella`, `UnanchoredPillar`, `SymmetricConflict`).
  - `advise` (Expert-to-Expert Advisory Protocol): Ranks SME improvement opportunities by `compute_frontier_leverage_bps`, joining external research/comparables (`references`, `comparable_implementations`, `frontier_opportunities`, `current_limitations`) with canonical project entities (`implemented_interfaces`, `linked_action_types`, governing rules/facts, files/symbols, open tasks). When asked strategic or domain-improvement questions (e.g., _"What are the top 5 ways we could improve this project?"_), run `neurostrata_taxonomy(operation: "advise", top_k: 5)`, execute any `public_safe` external research agenda queries (`websearch`, `context7`), persist newly discovered literature/comparables via `upsert_concept`, and ground recommendations in canonical project entities and domain literature.
