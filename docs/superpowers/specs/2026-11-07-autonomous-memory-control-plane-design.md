# NeuroStrata Autonomous Memory & Task Control Plane Design

- **Date:** 2026-11-07
- **Task:** `neurostrata-a1si`
- **Status:** Draft — Pending Owner Review
- **Baseline Commit:** `584567e` (`fix(rdf): stop reading metadata.source as a string only (v1.16.1)`) — substrate claims are as of `584567e` and anchors are verified live against the working tree.
- **Architects:** Gemini 4 Argon (orchestrator) & Kimi K3.1 Pro (`k3-arch`, session `ses_e4bee6294ffeDNlxnHr2ehILzg`)

---

## 1. Problem Statement & Empirical Field Audit (597 OpenCode V2 Sessions)

An empirical audit of **597 OpenCode V2 (`v2.2.6`) sessions** (`167,887` `session_message` rows across `NeuroStrata` and `keywest.health` from `2026-11-03` through `2026-11-07`) and decompilation of `/home/kenton/.opencode/bin/opencode` established two root causes for why post-`v1.15.1` / `v1.16.1` autonomous recall, memory capture, and task intake did not fire as intended:

### 1.1 OpenCode V2 Plugin: Propagation Gap + 3 Code Defects in `v1.16.1`

Both failure modes occurred simultaneously on this machine:

1. **Lazy-Install Propagation Gap (`src/task/harness_plugins.rs:222`, `src/task/mod.rs:2817`, `src/server.rs:3431`):**
   - `/home/kenton/.cargo/bin/neurostrata-mcp` was built `2026-11-05 19:39` (with PR #52 / `v1.16.0`), but the running `neurostrata-mcp daemon` process (`PID 9936`) did not restart until `2026-11-06 15:01 EST` (`1793995260000` ms).
   - `auto_install_detected_plugins` is only invoked inside `hook_hygiene` when an agent calls `neurostrata_get_snapshot` (or `bootstrap` / `task_setup`). Because zero sessions ran in `keywest.health` after the daemon restart, `keywest.health` still has its `Nov 3` `.opencode/` directory (missing `.opencode/plugins/neurostrata.js`), and `NeuroStrata` only received `.opencode/plugins/neurostrata.js` at `2026-11-07T03:03:47Z` during this audit session.
2. **Defect A — `executeAfterHandler` Payload Contract Mismatch (`plugins/core/index.ts:1310-1331`):**
   - Verified against `/home/kenton/.opencode/bin/opencode` (`v2.2.6`, byte offset `176924277` and `opencode.tool.input.repair` at offset `177909005`): OpenCode V2 invokes `ctx.tool.hook("execute.after", cb)` with a single event object:
     `event = { tool, sessionID, agent, messageID, id, input, status: "completed", result: { content, output?, metadata? } }`
     and reads back `event.result.content` and `event.result.output`.
   - `executeAfterHandler` checks `target.output` (undefined on `event`, because the nested field is `event.result`) and then tests `typeof out.output === "string"` / `typeof out.content === "string"` on top-level `event`. It mutates `event.additionalContext` (ignored by OpenCode V2) and leaves `event.result.content` untouched — resulting in **0 real `[NeuroStrata Governing Memories` injections** on `read`/`edit`/`write` across all 597 sessions.
3. **Defect B — Invalid `.opencode/opencode.json` `"plugins"` Wiring (`src/task/harness_plugins.rs:496-546`):**
   - `wire_harness_config("opencode")` writes `"plugins": ["./.NeuroStrata/plugins/opencode/dist/index.js"]` into `.opencode/opencode.json`.
   - OpenCode V2 (`offset 177438800`) resolves relative paths in `.opencode/opencode.json` against `.opencode/`, emitting `ENOENT: stat '/home/kenton/Documents/NeuroStrata/.opencode/.NeuroStrata/plugins/opencode/dist/index.js'` in `opencode.log`, and rejects file paths (`configured plugin path must be a directory`). Meanwhile, OpenCode V2 already auto-discovers `.opencode/plugins/neurostrata.js` without any `"plugins"` entry in `.opencode/opencode.json`.
4. **Defect C — Project Identity & CWD Resolution (`plugins/core/index.ts:1501`):**
   - `initialize(ctx)` calls `resolveConfig(ctx?.config ?? ctx?.pluginConfig ?? {})` with default `cwd = process.cwd()` instead of reading `ctx?.location?.project?.canonical ?? ctx?.location?.directory` and `ctx?.options`, risking cross-repository namespace contamination in a multi-project OpenCode V2 server.

### 1.2 Empirical Gaps Across the Four Target Capability Areas

| Area | Empirical Finding (597 Sessions) | Root Cause Anchor |
|---|---|---|
| **1. Automatic Memory Capture** | `>85%` of `neurostrata_add_memory` calls occur only at `task_complete` (Lock 2); `neurostrata_append_log` called `49` times total (`8` post-`1.16.0`); `neurostrata_distill` called `0` times. | Capture relies on voluntary LLM tool calls instead of automatic hook observation + daemon-side candidate extraction. |
| **2. Relevant Recall** | `84%` of file mutations occur in `worker` (`1,190` edits, `6` `memories_for_file`) and `craftsman` (`105` edits, `0` `memories_for_file`). `auto_retrieved_context` returned duplicate generations (`::3::`, `::4::`, `::5::`) in `100%` of Era A and `52.4%` of Era B calls; `81%` of normative rules have `access_count == 0`. | `search_inner` (`src/store/ladybug.rs:47-609`) neither filters superseded generations via `split_scoped_id` nor prioritizes normative rows (`rule`/`fact`/`decision`/`procedure`) ahead of `code_ast`/`doc_section`. |
| **3. Non-Obvious Connections** | `neurostrata_related` called `13` times (`2` post-`1.16.0`); `neurostrata_sparql_query` fails on `NeuroStrata` with `Dataset projection exceeded max_dataset_quads budget (256550 > 250000)`. | `project_sparql_dataset_with_meta_async` (`src/rdf/sparql.rs:85-141`) loads all rows across multi-source generations and duplicates every quad into `DefaultGraph` before applying query scope; 2-hop bridges are not surfaced automatically. |
| **4. Task Identification & Capture** | `task_intent_signals` on `neurostrata_append_log` returned `[]` on `49/49` (`100%`) calls; `neurostrata_task_intake` holds `0` candidates and `0` receipts. | Agents pass terse summaries to `append_log`; user prompts, subagent review verdicts, assistant completion summaries, and staged `TODO`/`FIXME` diffs never feed `task_intake`. |

---

## 2. Substrate Anchor Table (Pinned to `584567e`)

| File & Anchor | Current Behavior at `584567e` | Design Delta |
|---|---|---|
| `plugins/core/index.ts:1310-1331` (`executeAfterHandler`) | Reads top-level `output`/`content` and mutates `additionalContext`; ignores V2 `event.result.content`. | Mutate `event.result.content` (supporting both `string` and `Array<{type:"text",text:string}>` shapes) and `event.result.output`; post async capture telemetry to daemon. |
| `plugins/core/index.ts:1495-1535` (`initialize`, `createHarnessPlugin`) | Resolves project root from `process.cwd()`; registers only `ctx.tool.hook("execute.before")` and `ctx.tool.hook("execute.after")`. | Resolve project root from `ctx?.location?.project?.canonical ?? ctx?.location?.directory`; register `ctx.session.hook("context")`, `ctx.session.hook("compaction")`, `ctx.tool.hook("execute.before")`, and `ctx.tool.hook("execute.after")`; perform daemon handshake. |
| `src/task/harness_plugins.rs:222-555` (`auto_install_detected_plugins`, `wire_harness_config`) | Writes `"plugins": ["./.NeuroStrata/plugins/opencode/dist/index.js"]` into `.opencode/opencode.json`; runs only on lazy `hook_hygiene`. | Stop writing file paths into `.opencode/opencode.json` `"plugins"` (prune stale `./.NeuroStrata/plugins/opencode/dist/index.js` entries); rely on `.opencode/plugins/neurostrata.js` auto-discovery; run eager sync across known project roots at daemon startup + write `.NeuroStrata/plugins/manifest.json`. |
| `src/store/ladybug.rs:47-609` (`search_inner`) | Does not filter superseded AST/doc generations (`split_scoped_id`) or deduplicate logical targets across generations; `code_ast` and `doc_section` compete equally with normative rows. | Filter superseded generations and deduplicate by `(memory_type, logical_target)` keeping only the highest generation; apply normative authority boost for `rule`, `fact`, `decision`, `procedure` and support two-lane retrieval (`normative` vs `structural_evidence`). |
| `src/task/mod.rs:392-453` (`build_task_graph_context`) | Takes top 5 vector hits directly from `store.search`, allowing duplicate `code_ast`/`doc_section` rows to crowd out governing rules. | Partition `auto_retrieved_context` into reserved normative slots (top 3 `rule`/`fact`/`decision`/`procedure` + file-scoped `GOVERNS` matches) + top 2 active-generation structural/bridge hits. |
| `src/rdf/sparql.rs:85-141` (`project_sparql_dataset_with_meta_async`) | Materializes all rows and duplicates every quad into `DefaultGraph`, hitting `256,550 > 250,000` quads on `NeuroStrata`. | Deduplicate scoped IDs to the latest generation per logical target, exclude zero-vector structural rows (`code_ast`, `file`, `directory`, `doc_section`) unless explicitly referenced by predicate/IRI in the query, and project within `max_dataset_quads`. |

---

## 3. Architecture & Five Implementation Waves

### Wave 1: OpenCode V2 Control Plane, Eager Auto-Install & Project Isolation

1. **OpenCode V2 Config & Discovery Cleanup (`src/task/harness_plugins.rs`):**
   - `wire_harness_config("opencode", ...)` writes `.opencode/plugins/neurostrata.js` (auto-discovered by OpenCode V2's `.opencode/plugins/` scanner) and `.opencode/neurostrata-plugin.json`.
   - If `.opencode/opencode.json` contains the legacy broken entry `"./.NeuroStrata/plugins/opencode/dist/index.js"` in `"plugins"`, `wire_harness_config` removes that entry (and removes `"plugins": []` if empty) so OpenCode V2 stops logging `ENOENT` warnings.
2. **Eager Daemon Startup Propagation & Manifest (`src/daemon.rs`, `src/task/harness_plugins.rs`):**
   - On daemon startup and on `neurostrata-mcp hooks install`, scan `startup_root`, `allowed_external_dirs`, and registered project roots in LadybugDB (`is_registry_row` / `directory` roots containing `.git`) and run `auto_install_detected_plugins` eagerly so upgraded plugin bundles deploy to `keywest.health` and `NeuroStrata` immediately without waiting for `neurostrata_get_snapshot`.
   - Write `.NeuroStrata/plugins/manifest.json` containing `{ abi_version: 2, plugin_version, bundle_sha256, installed_at, harnesses }`.
3. **Canonical Project Identity in OpenCode V2 `setup(ctx)` (`plugins/core/index.ts`):**
   - Extract `projectRoot` from `ctx?.location?.project?.canonical ?? ctx?.location?.directory ?? ctx?.directory ?? ctx?.cwd` before falling back to `process.cwd()`.
   - Key plugin session state (`activeTaskId`, `contextEpoch`, `deliveredMemoryIds`, `prefetchedPaths`) per `(projectRoot, sessionID)` map rather than singleton module variables so multi-repo and parent/child subagent sessions in the same OpenCode V2 daemon never collide.

### Wave 2: OpenCode V2 Hook Spine (`context`, `compaction`, `execute.before`, `execute.after`)

1. **`execute.after` V2 Payload Contract Fix (`plugins/core/index.ts`):**
   - Detect V2's `event.result` wrapper (`const res = target?.result && typeof target.result === "object" ? target.result : target`).
   - When appending a recall/governance/bridge block:
     - If `Array.isArray(res.content)` (OpenCode V2's standard `[{ type: "text", text: "..." }]` tool output format), append the block to the last `{ type: "text" }` item (or push `{ type: "text", text: block }`) when not already present.
     - If `typeof res.content === "string"`, append `\n\n${block}` to `res.content`.
     - If `typeof res.output === "string"`, also append `\n\n${block}` to `res.output`.
   - Maintain backward compatibility with legacy/Claude/Codex top-level `{ output, content, additionalContext }` callers.
2. **Session Context & Compaction Hooks (`plugins/core/index.ts`):**
   - Register `ctx.session.hook("context", async (event) => ...)` and `ctx.session.hook("compaction", async (event) => ...)`:
     - Track `contextEpoch` per `sessionID` (incremented whenever `compaction` fires for that `sessionID`).
     - On `context` hook: if the active task's baseline normative rules and 2-hop graph bridges have not yet been injected in the current `(sessionID, contextEpoch)`, inject a compact (<600 tokens) `[NeuroStrata Session Context]` block and mark those memory IDs delivered for the epoch.
     - Skip duplicate injection on subsequent steps within the same epoch (`0` repeated memory bodies per epoch).
3. **`execute.before` Subagent & File-Touch Preflight (`plugins/core/index.ts`):**
   - On `subagent` / `Task` tool calls: resolve the parent session's active task, extract all repo file paths mentioned in `input.prompt`, call `neurostrata_memories_for_file` for each target file, and inject a compact `### NeuroStrata Task & Graph Context` block containing:
     - Active task ID, namespace, and goal
     - Exact file-governing `[rule]` / `[fact]` / `[decision]` rows for the files named in the subagent prompt
     - 1–2 non-obvious 2-hop dependency/co-change bridges (`Because A -> B -> C, also inspect C`)
   - On `edit` / `write` / `patch` / `read`: prefetch `neurostrata_memories_for_file` (cached per `(namespace, filePath, generation)`), enforce the Zero-Action Start task guard on mutating calls, and queue governing memories not yet delivered in the current epoch for `execute.after` attachment.

### Wave 3: Scoped Normative-First Recall & Generation Deduplication (Area 2)

1. **Strict Generation Deduplication in `search_inner` (`src/store/ladybug.rs`):**
   - Load `active_gens = active_generations_of(conn, &namespace)` and `latest_gen = active_generation_of(conn, &namespace)` once at the start of `search_inner`.
   - For any row whose ID parses via `crate::parser::ingest::split_scoped_id(&id)` as `(ns, gen, logical_rest)`:
     - Drop the row if `!active_gens.is_empty() && !active_gens.contains(&gen)`.
     - Deduplicate across multiple active source generations by `(memory_type, logical_rest)`, keeping only the row with the highest `gen` (preferring `gen == latest_gen`). This guarantees that `::1::`, `::2::`, `::3::`, `::4::`, and `::5::` of the same AST symbol or `doc_section` can never occupy multiple search slots.
2. **Normative Authority Ranking & Structural Separation (`src/store/ladybug.rs`, `src/task/mod.rs`):**
   - Define `NORMATIVE_MEMORY_TYPES = ["rule", "fact", "decision", "procedure"]` and `EVIDENCE_STRUCTURAL_TYPES = ["code_ast", "doc_section"]`.
   - In `search_inner`: apply a bounded normative authority gain (`NORMATIVE_RANK_GAIN = 0.08 * distance`) to active `rule`, `fact`, `decision`, and `procedure` rows so normative constraints rank ahead of raw `code_ast` / `doc_section` chunks at comparable cosine distance, while enforcing per-category slot caps (`max 2` `code_ast`/`doc_section` rows in default search results when normative hits exist).
   - In `build_task_graph_context` (`src/task/mod.rs:392-453`):
     - Extract any repo file paths mentioned in the task title/description and query `ast_neighborhood_for_file` (`GOVERNS` rules/facts) directly.
     - Reserve up to 4 slots of `auto_retrieved_context` for deduplicated normative memories (`rule`, `fact`, `decision`, `procedure`) and at most 2 slots for active-generation `code_ast`/`doc_section` symbols.

### Wave 4: Evidence-Bound Auto-Capture & Tiered Task Intake (Areas 1 & 4)

1. **Automatic Observation Pipeline (`plugins/core/index.ts`, `src/server.rs`, `src/task/mod.rs`):**
   - Add an MCP tool / daemon endpoint `neurostrata_observe_turn` (and wire `execute.after` + `session.hook("prompt")` in `plugins/core/index.ts` to call it asynchronously, fail-soft):
     - **Inputs:** `{ namespace, project_root, session_id, task_id, actor, event_kind: "user_prompt" | "subagent_verdict" | "tool_error_resolved" | "git_diff_markers", text, file_paths }`.
     - **Idempotency:** Deterministic `event_id = sha256(namespace + session_id + event_kind + content_hash)` stored in the intake receipt ledger so replays never duplicate writes.
2. **Area 1 — Tiered Memory Auto-Capture Gate (`src/task/mod.rs`, `src/handlers/`):**
   - Deterministic + evidence-bound extraction over observed text:
     - **Explicit User Directives & Corrections** (e.g., `"always use..."`, `"never do..."`, `"rule:"`, `"architecture decision:"`, owner corrections): staged with exact quote span, `source: "session:<session_id>"`, and `governs` inferred from `file_paths`.
     - **Verified Bug Fixes & Root Causes** (when a failing test/build command is followed by an edit and passing test/build, or a `subagent` reviewer/checker verdict reports a verified root cause): auto-committed as a `fact` or `decision` with `metadata.extracted_from: [task_id]`, `metadata.source`, and `metadata.evidence_span`, after checking vector/NLI novelty (`cosine_similarity < 0.92`) to update rather than duplicate existing facts.
     - **Safety Invariant (K3 Ruling):** Agent-inferred patterns are **never** auto-promoted to `memory_type: "rule"`. Only explicit user/owner directives or accepted review candidates may become `rule` rows, and every `rule` row carries compliant `metadata.source` and `metadata.enforcement` (`NOT_ENFORCED` or a valid `PARTIAL`/`ENFORCED` wire ID per `5d25375d-3765-4bba-8e51-a587cb47ee17`).
   - Because verified `fact`/`decision` rows are linked via `extracted_from: [task_id]` during work, `neurostrata_task_complete` (Lock 2) is already satisfied by real in-flight learning rather than end-of-task compliance boilerplate.
3. **Area 4 — Tiered Task Identification & Intake Reconciliation (`src/task/intake.rs`, `src/task/mod.rs`, `src/task/gate.rs`):**
   - Feed four sources into the existing `task_intake` engine (`src/task/intake.rs`):
     1. User prompts (`event_kind: "user_prompt"`)
     2. Subagent outputs (`reviewer`, `checker`, `checker-free`, `k3-arch`, `gemini-arch` findings containing deferred items, `follow-up`, `TODO`, `FIXME`, `remaining work`, or numbered action items)
     3. Assistant turn logs (`neurostrata_append_log`)
     4. Pre-commit staged diff lines adding `+.*(TODO|FIXME|HACK|XXX):`
   - **Three-Tier Classification:**
     - **High-Confidence Obligation (`confidence >= 0.85`):** Explicit user deferral directive, unresolved checker/reviewer defect, or newly introduced staged-diff `TODO:`/`FIXME:` with concrete target file/action. Matched against existing open tasks by normalized intent + file path; if novel, staged as a high-confidence `candidate` (or auto-linked as a subtask of the active task).
     - **Speculative / Low-Confidence Mention (`0.50 <= confidence < 0.85`):** Recorded as an advisory intake candidate; never blocks completion.
     - **Excluded (`< 0.50`):** Negated statements (`"no TODOs left"`, `"zero FIXMEs"`, `"never leave TODO"`), historical prose, or completed items.
   - **Pre-Completion Reconciliation Gate (`handle_task_complete` in `src/task/mod.rs`):**
     - Before marking a task `done`, check for undisposed high-confidence intake candidates captured during that task's `session_id`. If any exist, return a structured prompt listing the candidate IDs so the agent either promotes them (`neurostrata_task_create` / `follow_up_tasks`) or dismisses them with a reason (`neurostrata_task_intake_decide`).

### Wave 5: Evidence-Path Concept Bridges, Contradiction Surfacing & Scoped SPARQL (Area 3)

1. **Bounded 2-Hop Evidence-Path Bridges (`src/graph_read/memories_for_file.rs`, `src/task/mod.rs`):**
   - Extend `neurostrata_memories_for_file` and `build_task_graph_context` with a `concept_bridges` array (capped at 3 high-salience bridges):
     - **Co-Task / Episode Bridge:** `(TargetFile)<-[:GOVERNS]-(RuleA)-[:EXTRACTED_FROM]->(TaskT)<-[:EXTRACTED_FROM]-(RuleB)-[:GOVERNS]->(OtherFile)` — surfaces files and rules coupled by past bugfixes/tasks even when no direct AST import exists.
     - **2-Hop Call/Dependency Governance Bridge:** `(TargetFile)-[:CONTAINS]->(SymA)-[:RELATES_TO|DEPENDS_ON]->(SymB)<-[:CONTAINS]-(FileB)<-[:GOVERNS]-(RuleB)` — surfaces governing rules on downstream/upstream dependencies 2 hops away.
     - **Contradiction / Supersession Alert:** When two active memories govern the same file or share subject/predicate facets with conflicting polarity or NLI contradiction score, surface both memory IDs, timestamps, and provenance spans explicitly (`[Contradiction Alert: mem-A vs mem-B]`).
   - Every bridge carries a concrete explanation string: `"Because <A> --[REL1]--> <B> --[REL2]--> <C>, also inspect <C> (<memory_id>)"`. Inferred bridges are never persisted as durable facts without explicit verification.
2. **Scoped SPARQL Dataset Projection Pushdown (`src/rdf/sparql.rs`):**
   - Fix the `256,550 > 250,000` quad budget failure in `project_sparql_dataset_with_meta_async` (`src/rdf/sparql.rs:85-141`) without raising `max_dataset_quads`:
     - Deduplicate generation-scoped IDs (`split_scoped_id`) to only the highest active generation per `(memory_type, logical_target)` so stale generations (`::1::`..`::4::`) are never projected into RDF quads.
     - Inspect the parsed SPARQL query text before projection: unless the query explicitly references structural AST types/predicates (`code_ast`, `doc_section`, `directory`, `file`, `CONTAINS`, `location_lines`), project only normative, task, ontological, and edge-linked nodes into the active SPARQL dataset, cutting default dataset size on `NeuroStrata` by >85% while keeping `max_dataset_quads = 250,000` intact.

---

## 4. Mandatory Governance & OxiRS / Architecture Compliance

1. **Library Evaluation & Crate Justification (`c5169fc7-e2af-4fec-83b3-0967d0b8325d`):**
   - Uses existing workspace crates (`lbug`, `oxrdf`, `spargebra`, `oxigraph`/`QueryEvaluator`, `serde_json`, `sha2`) and Node built-ins (`node:fs`, `node:path`, `node:os`, `node:crypto`).
   - Explicitly rejects `oxirs-core`, `scirs2`, and `horned-owl` (banned across the workspace).
   - Zero Cargo feature flags (`0f443bda-92cc-4b5d-a393-f9996b2c8b2e`): all Rust modules remain unconditionally compiled.
2. **Warn-First & Fail-Soft Semantics:**
   - All plugin hooks (`context`, `compaction`, `execute.before`, `execute.after`) and `neurostrata_observe_turn` calls are fail-soft (`try / catch` with bounded timeouts) and never crash tool execution or ingest if the daemon is unreachable.
3. **Per-Namespace Isolation:**
   - All recall, auto-capture, task intake, and SPARQL dataset projections are strictly scoped to the resolved project `namespace` (`ctx.location.project.canonical`), with cross-namespace bridges disabled by default unless opted in via concept IRIs (`pjoa`).
4. **Deterministic Core Path:**
   - Generation deduplication, normative ranking, 2-hop evidence-path bridges, and task-intent/directive extraction are 100% deterministic and require zero external LLM calls in the core Rust path.
5. **Prior Spec Alignment:**
   - Preserves canonical RDF direction (`9fnf`), `QueryBudget` limits (`yjah`), OWL reasoning hydration (`2hxl`), concept IRIs (`pjoa`), canonical domain slugs (`u5o1`), RDF-star edge provenance (`4gb2`), directed Hybrid GraphRAG (`52dq`), and SynapticMesh isolation (`mhsp`).
   - All tests opening `LadybugStore` MUST use `LadybugStore::for_testing` (`8c85f445-8904-4239-8f08-7f4de4f241d8`).

---

## 5. Acceptance Criteria & Verification Gates

1. **Wave 1 & 2 (OpenCode V2 Plugin & Hook Contract):**
   - `.opencode/opencode.json` no longer contains `"./.NeuroStrata/plugins/opencode/dist/index.js"`; OpenCode V2 loads `.opencode/plugins/neurostrata.js` with zero `ENOENT` warnings.
   - Daemon startup and `hooks install` eagerly deploy `.opencode/plugins/neurostrata.js` and `.NeuroStrata/plugins/manifest.json` to both `NeuroStrata` and `keywest.health`.
   - `executeAfterHandler` mutates `event.result.content` (both `Array<{type:"text",text:string}>` and `string` formats) on `read`/`edit`/`write` calls and preserves original tool output.
   - `ctx.session.hook("context")` injects epoch-deduplicated session context once per `(sessionID, contextEpoch)` and resets on `ctx.session.hook("compaction")`.
   - `executeBeforeHandler` enriches `subagent` prompts with target-file governing memories and 2-hop bridges.
2. **Wave 3 (Scoped Normative Recall & Generation Dedup):**
   - `search_inner` returns zero superseded or multi-generation duplicate rows (`::3::`, `::4::`, `::5::` of the same logical target).
   - `build_task_graph_context` populates reserved normative slots (`rule`/`fact`/`decision`/`procedure`) ahead of `code_ast`/`doc_section`.
3. **Wave 4 (Evidence-Bound Auto-Capture & Tiered Task Intake):**
   - `neurostrata_observe_turn` extracts explicit user directives, verified bugfix facts (linked via `extracted_from`), and high-confidence task obligations idempotently.
   - Zero agent-inferred memories are auto-promoted to `memory_type: "rule"`.
   - Negated statements (`"zero TODOs"`, `"never leave FIXME"`) produce zero false-positive intake candidates.
   - Undisposed high-confidence task candidates surface at `neurostrata_task_complete` for promotion or reasoned dismissal.
4. **Wave 5 (2-Hop Concept Bridges & Scoped SPARQL):**
   - `neurostrata_memories_for_file` and `build_task_graph_context` return up to 3 evidence-backed `concept_bridges` (`co-task`, `2-hop dependency governance`, or `contradiction`).
   - `neurostrata_sparql_query` on `NeuroStrata` succeeds within the default `250,000` `max_dataset_quads` budget without raising the cap.
   - `cargo test` and plugin test suites pass 100% with zero warnings or regressions.
