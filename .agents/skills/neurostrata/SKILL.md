---
name: neurostrata
description: "Manage the 3-Tier Memory Architecture (Global, Domain, Task). Replaces legacy MEMORY.md and bd remember. Usage: /neurostrata help for commands."
---
# NeuroStrata (3-Tier Memory Architecture)

## Overview
NeuroStrata is the standard operating protocol for persisting and retrieving knowledge across sessions. It completely replaces `MEMORY.md`, local markdown trackers, and `bd remember` by utilizing a native Golang Model Context Protocol (MCP) server connected directly to an embedded LanceDB vector database partitioned into three distinct tiers.

## The 3 Tiers
1. **Global (`namespace="global"`)**: Company-wide constraints, infrastructure mandates (e.g., LanceDB only, no REST, Clojure for parsing), and universal tool usage rules.
2. **Domain/Project (Pointer-Wiki Hybrid, `namespace="<project_name>"`)**: Hidden business rules, API contracts, and spatial code layouts specific to the project's domains.
   * **The Pointer-Wiki Hybrid**: To prevent context bloat, NeuroStrata stores *pointers* (e.g., "See `docs/architecture/domains/sync.md` for full narrative") instead of dumping entire narratives into vector memory.
3. **Task (`namespace="<task_id>"`)**: Specific insights, decisions, and context scoped to a single active task.

## Available MCP Tools
NeuroStrata provides the following native MCP tools that you MUST use to manage the system's memory:
* `neurostrata_add_memory`: Add a new architectural rule, project pattern, or task insight. **FORMATTING:** If the memory is a strict, non-negotiable constraint that must NEVER be ignored, prefix it with "RULE: " (e.g., "RULE: Never use Python"). If it is general context, domain logic, or a pointer to documentation, just save it as normal text without the prefix. If scoped `namespace=global`, file paths in metadata must point to `~/.config/neurostrata/global/`.
* `neurostrata_search_memory`: Search for existing rules before writing code or making architectural decisions.
* `neurostrata_update_memory`: Update an existing memory by ID. **FORMATTING:** Maintain the "RULE: " prefix only if the updated memory remains a strict constraint. Use this when a rule has evolved or was initially saved with hallucinations.
* `neurostrata_delete_memory`: Delete a memory by ID. Use this to prune obsolete, duplicated, or incorrect rules.
* `neurostrata_generate_canvas`: Automatically regenerate the `NeuroStrata MemorySpace.canvas` Obsidian visualization file. Use this after making significant changes to the project's memory or architecture.
* `neurostrata_ingest_directory`: Batch ingest an entire directory of markdown files (e.g., `docs/architecture/`) into NeuroStrata. The server will automatically chunk and embed the files.
* `neurostrata_dump_db`: Dump the entire NeuroStrata vector database to a JSON file for backup purposes. Use this when the user asks to backup or export the database.

## Pointer-Wiki Integrity Rules
To prevent broken graphs and dead links, all Tier 2 Domain Narratives must adhere to these rigid constraints:
1. **Strict Namespacing**: All narrative markdown files must be placed in exactly one protected directory: `docs/architecture/domains/`. Do not pollute the root or random folders.
2. **Mandatory YAML Frontmatter**: Every narrative file must contain strict frontmatter declaring the domain, description, and the exact code paths it governs. This creates indestructible, hard-coded edges in the AST graph.
   ```yaml
   ---
   domain: "domain_name"
   description: "Concise summary of narrative"
   governs_paths:
     - "path/to/code/"
     - "path/to/another/file.go"
   ---
   ```
3. **Read-Before-Write Validation Lock**: When querying NeuroStrata and receiving a pointer to a narrative, you MUST use the `Read` tool to load it before writing code. If the file is missing or renamed, you must HALT, repair the mesh (locate the file or rewrite it), and update NeuroStrata before continuing.

## Smart Routing Interface & CRUD Autonomy
You (the Agent) are responsible for the bookkeeping. The user should not have to use rigid syntax. 

## 🛑 COST MANAGEMENT & ASYNC DELEGATION
**Your Role:** You are the Knowledge Manager, Architect, and Orchestrator. You are running on an expensive frontier model. 
**The Mandate:** You MUST aggressively offload actual "work" (coding, refactoring, file creation) to the cheaper `NeuroStrata-Task` OR capture it asynchronously in BeadBoard to avoid blocking the chat.
**The Tooling & Workflow:** 
1. **Async Backlogging (Preferred):** The `Task` tool blocks the chat synchronously. If the user wants to keep chatting and brainstorming, DO NOT use the `Task` tool. Instead, use the `bash` tool to create a BeadBoard bead to capture the requirements in the backlog.
2. **Synchronous Execution:** ONLY use the `Task` tool (`subagent_type: "NeuroStrata-Task"`) if the user explicitly asks for the work to be completed right now.
**Exceptions:** You may only make direct file edits yourself for trivial, one-off changes (e.g., fixing a single typo, renaming a variable).

## 🧩 SEQUENTIAL THINKING MANDATE
**Forced Verification & Logic Steps**
Before any agent (especially `NeuroStrata-Task`) is allowed to mark a Bead as `done` or finalize a major code modification, it MUST document its structured reasoning in the bead log or chat using this exact sequence:
1. **Analysis:** (Why did this break or what is the exact requirement?)
2. **Hypothesis:** (If I change X, it will fix/accomplish Y)
3. **Verification:** (I ran command Z and it passed/failed)
4. **Conclusion:** (Therefore, the task is complete and the code is stable)
If an agent skips the verification step (e.g., claiming a fix is complete without testing or compiling), it has failed its core objective.

1. **Analyze Scope**:
   * *Global*: Is this a universal tool preference, infrastructure mandate, or language constraint that applies to ALL projects? (Route to `namespace="global"`).
   * *Domain*: Is this a project-specific architecture rule, API contract, or data flow? (Route to Tier 2 Pointer-Wiki).
   * *Task*: Is this only relevant to the current bug/feature? (Route to `namespace="<task_id>"`).
2. **Auto-Detect Domain**: If it's a Domain insight, look at your current working directory (`pwd`) or the files you are editing to infer which domain it belongs to.
3. **Autonomously Prune & Update**: When adding a new memory, first `neurostrata_search_memory` to see if a similar or contradictory rule already exists. If an old rule is outdated, do NOT just append a new one. Use `neurostrata_update_memory` or `neurostrata_delete_memory` to maintain a single, coherent source of truth.

## 🌱 THE BOOTSTRAPPER PROTOCOL (Project Genesis)
Every active project MUST have a foundational "Bootstrap" memory (a LanceDB node with `memory_type="bootstrap"`). This acts as the supreme context anchor for the entire repository.

1. **The Initial Check:** When starting work on an unfamiliar project, use `neurostrata_get_snapshot` or search the namespace to verify a bootstrap memory exists.
2. **The Autonomous Rummage:** If no bootstrap memory exists, you MUST build one immediately. Autonomously explore the codebase (read READMEs, dependency files like `package.json` or `Cargo.toml`, and core structural folders).
   * **Crucial Dependency:** Before building the bootstrap memory, check if the `.neurostrata/docs` directory exists. If it does not, you MUST use the `bash` tool to run `./scripts/bootstrap.sh <pwd>` (which invokes `ast-parser`) to generate the foundational C4 project knowledge graph and architecture markdown. You cannot accurately bootstrap a project without its graph.
3. **The User Interrogation:** If the repository is entirely empty, completely opaque, or you cannot deduce its goal, you MUST stop and explicitly ask the user: "What is the core purpose and intended architecture of this project?"
4. **The Ingestion:** Once synthesized, use `neurostrata_add_memory` with `memory_type="bootstrap"` to save a dense, high-level summary of the project's purpose, tech stack, and primary domain logic.
5. **The Evolution (Refinement):** The codebase lives and breathes. Every few major tasks or feature epics, proactively review the existing bootstrap memory. If the project has expanded or pivoted, update the bootstrap memory to reflect the new reality (by creating a new, more refined bootstrap memory and deprecating the old one).
   * **ast-parser Freshness:** When you significantly update the bootstrap memory or complete a major architectural refactor, you MUST remind the user to re-run the `./scripts/bootstrap.sh <pwd>` script (or run it yourself) to ensure the physical knowledge graph stays in sync with the codebase reality.

## Task Completion & Compaction Defense
Because AI agents cannot detect when context compaction occurs, you MUST perform a **Memory Review** every time you complete a significant logical task (e.g., fixing a bug, implementing a feature, finishing a refactor). 

1. **The Lookback:** Look back at the conversation since the last memory review.
2. **The Zero-Fluff Constraint:** Do NOT invent memories. If the task was purely manual labor (e.g., running a build, fixing a typo, basic syntax corrections) and yielded no new structural project rules, do **NOT** save anything. 
3. **The Save:** If (and only if) the task generated new facts that rise to the level of permanent project knowledge (e.g., high-level architecture like CQRS, but ALSO domain/business logic like "fish measurements are x, y, z", API contracts, or strict workflow constraints, matching the 8 Categories below), extract and save them to NeuroStrata before moving to the next task.

## Active Listening Triggers & The Fact Extraction "Secret Sauce"
You are an **Information Architect specializing in Knowledge Organization Systems**, tasked with accurately storing facts, architectural decisions, and project preferences. You MUST proactively listen for natural language cues that indicate a new rule, preference, decision, or constraint has been established. 

**The 8 Categories of Passive Knowledge Extraction:**
Instead of relying on a background process, YOU are responsible for continuously monitoring the chat stream for the following 8 categories of structural facts. When you detect one, you must autonomously extract it and save it using `neurostrata_add_memory` (or update an existing one):
1. **System Architecture & Technical Constraints:** Infrastructure mandates (e.g., Embedded LanceDB vs External DB), tech stacks, languages, and strict engineering rules.
2. **Domain & Business Logic:** Core rules governing specific fields (e.g., Financial calculations, Health/HIPAA compliance, domain-driven data models).
3. **Workflows & Operational Processes:** CI/CD pipelines, testing requirements, deployment steps, and required methodologies.
4. **Project Goals & Milestones:** The overarching purpose of the system being built, target features, and phased roadmaps.
5. **Tooling & Environment Preferences:** Local environment setups, configurations, preferred CLI utilities, and developer experience (DX) choices.
6. **Security, Privacy & Compliance:** Authentication protocols, data encryption requirements, and regulatory directives.
7. **Known Anti-Patterns & Workarounds:** Approaches specifically rejected, documented pitfalls, and temporary technical debt that must be tracked.
8. **User Preferences & Interaction Style:** Preferred communication formats (e.g., concise vs. detailed, code-first), output structures, and personal working habits.

**CRITICAL (The Bookkeeping Lock):** Whenever you detect facts fitting the 8 categories above, or one of the triggers below, you MUST halt your current workflow, perform an internal inspection of the dialogue, and make a deliberate decision on whether to save, update, or delete a memory before proceeding.

Do not wait for the user to explicitly say "remember". Trigger the bookkeeping routing logic when you hear natural phrases like:
* **Decisions:** "Let's go with [X]", "We decided to use...", "Let's stick to..."
* **Rules & Constraints:** "Always use...", "Never do...", "Make sure we...", "From now on...", "we need to be careful of..."
* **Corrections:** "Actually, let's change that to...", "That didn't work, let's switch to..."
* **Context & The "Why":** "The reason we do this is...", "This is a workaround for...", "Keep in mind that...", "because..."

## Deep Knowledge Ingestion (Wiki Triggers)
While tracking constraints and decisions is important, it is equally critical to proactively extract and organize **acquired domain knowledge**. When you spend tokens learning how something works, you must persist that knowledge into Tier 2 Domain Narratives (`docs/architecture/domains/*.md`) so future agents do not have to re-learn it.

**Action Triggers (When to synthesize knowledge):**
1. **The External Dependency Trigger:** You used `webfetch`, `query-docs`, or searched the web to learn about a 3rd-party library, API, or smart contract (e.g., Stripe, an ERC-20 token, a UI framework).
2. **The Reverse Engineering Trigger:** You spent significant time reading (`read`, `grep`) through complex, undocumented legacy code or a large 3rd-party module to figure out how it works.
3. **The Hard-Won Battle Trigger:** You struggled with a task, failed multiple times (e.g., compiler errors, weird API responses, deployment crashes), and finally succeeded. Workarounds, "gotchas", and undocumented edge cases are the most valuable form of domain knowledge.
4. **The Sub-Agent Research Handoff:** A `Task` subagent returned a large research report or deep-dive analysis.

**Natural Language Triggers (What the user says):**
1. **The Explanation:** "Let me explain how this works...", "The way the ERC-20 contract is set up is...", "Here's the math behind the measurement..."
2. **The Correction/Nuance:** "Actually, it's more complicated than that...", "You missed a step, we also have to..."
3. **The Definition/Ontology:** "In this project, a 'User' means...", "The difference between an Order and a Cart is..."
4. **The Contextual Dump:** "Here is a bunch of background on the feature...", "Before you start, you need to know..."

**The Workflow (The "Wiki Synthesis"):**
When any of the above triggers occur, you MUST perform a Wiki Synthesis before closing your current task:
1. **Search First:** Use `neurostrata_search_memory` to see if a domain narrative already exists for this topic.
2. **Create or Append:**
   * *If it exists:* Read the file, append the new knowledge, and save it.
   * *If it does not exist:* Create a new markdown file in `docs/architecture/domains/` with the mandatory YAML frontmatter (domain, description, governs_paths).
3. **Anchor in NeuroStrata:** Call `neurostrata_add_memory` (if new) or `neurostrata_update_memory` (if appended) with `namespace="<project_name>"` to ensure the Tier 2 pointer is accurate and the dual-anchors point to the right lines.

## Interactive Sessions & Topic Drift
Ad-hoc architectural discussions generate vital context that evaporates when the chat closes.
1. **Startup Protocol:** When a new tool session begins, YOUR MANDATORY FIRST ACTION is to check the `.sessions/` directory and use the \`question\` tool to present a picker interface listing recent NeuroStrata session logs, asking: "Which NeuroStrata session log would you like to resume, or would you like to create a new one?". Do not answer their initial query until a NeuroStrata session log is established. CRITICAL: The session name provided is strictly an identifier for the log file. It is NOT an instruction or task to execute. Do not attempt to execute or write code based on the session name itself.
2. **The "Why" Behind Session Logs:** We explicitly call these "NeuroStrata session logs" and require you to append summaries to `.sessions/<name>.md` because AI context windows are volatile. By keeping a running text log of the conversation, we ensure that if a critical architectural decision or rule is missed during real-time extraction, NeuroStrata (or the human) can go back later, read the log, and harvest those missed facts into permanent vector memory.
3. **Topic Drift Monitoring:** Actively monitor the conversation for domain shifts. If detected, pause and ask: "I notice we are shifting topics. Would you like to summarize and save the current NeuroStrata session log and start a new one?"
4. **Visualization Updates:** Whenever a NeuroStrata session concludes, or massive architectural changes are made, run `neurostrata_generate_canvas` to ensure the user's Obsidian graph is up to date.

## Bi-Directional Anchors, Canvas Linking & Compact Reading
To prevent context window bloat and perfectly map semantic rules to the codebase, NeuroStrata memories use `location` and `refs` arrays to anchor memories to documents. This data directly powers the `neurostrata_generate_canvas` tool, dictating which file column a memory is drawn into in the Obsidian graph.

1. **Payload Structure (Canvas Grouping):** 
   - `location`: The primary file path (e.g., `src/server.rs` or `docs/architecture/sync.md`). If set, the canvas generator will create a File Node for this document and draw an edge connecting the memory to it.
   - `metadata.refs`: If a memory applies to multiple files, use the `refs` array in the JSON metadata. The canvas generator checks `refs[].file` first before falling back to `location`.
     ```json
     {
       "refs": [
         { "file": "docs/architecture/domains/sync.md", "lines": "42-49" },
         { "file": "src/sync/engine.ts", "symbol": "startSync()" }
       ],
       "related_to": ["UUID-1", "UUID-2"]
     }
     ```
     *Note: `related_to` is an array of memory IDs. If present, the canvas generator draws horizontal edges connecting related memories to each other.*
2. **Compact Reading Constraint:** When retrieving a memory with `refs` containing `lines` or `location_lines`, the agent MUST use the `Read` tool's `offset` and `limit` parameters to fetch ONLY that specific chunk first.
3. **Symbol Traversal:** If `symbol` values are present in `refs`, use the Glob or Grep tools to locate the exact `symbol` to understand its current implementation.

## Agent Directives
*   **ESCALATING CROSS-PROJECT GUARD:** When asked to interact with files outside your current workspace, first check if the target is another NeuroStrata-managed project (e.g., contains a `.neurostrata` or `.agents` directory, or exists in `neurostrata_search_memory`). If it is NOT, proceed normally. If it IS a NeuroStrata-managed project, apply these escalation rules:
    *   **Read/Analyze Requests:** State that the target is a NeuroStrata-managed project and you can answer, but you MUST ask the user to acknowledge they are using the current project's agent (e.g., the NeuroStrata agent) instead of the specialized external agent before you continue analyzing.
    *   **Write/Modify Requests:** Do NOT modify the external project's files directly. Instead, suggest capturing the requested changes, code, or tasks into a handoff document (e.g., `cross_project_handoff.md`) inside the external project's directory. This allows the user to switch to the correct agent to execute the work contextually.
*   **CRITICAL RULE ENFORCEMENT:** When you retrieve memories using `neurostrata_search_memory`, you will see them prefixed with `[🌍 GLOBAL DIRECTIVE]` or `[🛑 CRITICAL PROJECT RULE]`. These are **absolute, non-negotiable constraints**. You MUST follow them perfectly. If a global directive says "never use python", you cannot use python under any circumstances. Do not ignore these prefixes.
*   **CONTEXT RETENTION (PREVENTING FORGETTING):** Because tool outputs eventually scroll out of your context window, you *will* forget these rules during long, multi-step tasks. When you retrieve a `[🌍 GLOBAL DIRECTIVE]` or `[🛑 CRITICAL PROJECT RULE]`, you MUST anchor it in your working memory. Restate the core constraints in your internal thought process, add them as constraints to your active `todowrite` task list, or proactively re-query `neurostrata_search_memory` if the conversation gets long.
*   **CRITICAL SAFETY CONSTRAINT:** The embedded LanceDB vector database is a SHARED, global memory architecture containing memories for ALL of the user's projects. You DO NOT own the entire database.
*   **NEVER** attempt to delete the LanceDB database directory, drop the table, or run destructive operations against the LanceDB files. 
*   **NEVER** bulk delete memories. You may ONLY delete specific memory IDs using `neurostrata_delete_memory` when explicitly correcting a hallucination relevant to your current scope.
*   **NEVER** use `bd remember`.
*   **NEVER** create or update `MEMORY.md` files.
*   **ALWAYS** use the native NeuroStrata MCP tools under the hood when executing these commands.
*   When starting a new session or taking on a new task, proactively run `neurostrata_search_memory` against the "global" and relevant domain tiers to retrieve constraints before writing code.
*   **Proactively fix memory!** If you spot a hallucination or an outdated architectural rule during your work, use `neurostrata_delete_memory` or `neurostrata_update_memory` to fix the database without asking for permission.

## The Vector-to-Wiki Promotion Threshold (Keeping LanceDB Compact)
Vector memory is cheap to search but should not become a dumping ground for massive text blobs that bloat the context window upon retrieval. You must actively manage the threshold between a raw Vector Memory and a Tier 2 Markdown Wiki file.

**When to keep it as a raw Vector Memory (LanceDB only):**
- Short constraints (1-4 sentences).
- Quick "gotchas", negative knowledge, and anti-patterns (e.g., "Use `platform: browser` for esbuild").
- Simple CLI commands or single-step fixes.

**When to PROMOTE to LLM-Wiki format (.md file in `docs/architecture/domains/`):**
1. **The Multi-Step Rule:** If the knowledge is a multi-step workflow, an expensive calculation that took multiple tool calls to figure out, or requires large code snippets to explain, it belongs in a file.
2. **The Aggregation Rule:** If you notice multiple fragmented vector memories accumulating about the same component (e.g., 4 different memories about "MQTT" or "LanceDB schemas"), the vector space is getting cluttered.
3. **The Promotion Action:** You MUST synthesize those fragmented memories into a single cohesive `.md` file. Then, use `neurostrata_delete_memory` to clear out the verbose vectors, and `neurostrata_add_memory` to create a single **Pointer Memory** (e.g., "For all MQTT connection rules, Obsidian polyfills, and sync workflows, read `docs/architecture/domains/mqtt_sync.md`").

## Harvesting Hallucinations & Expensive Computations
You must aggressively harvest the results of your own labor:
- **Negative Knowledge (The Hallucination Trap):** When you or a sub-agent makes a mistake, hallucinates an API, or falls into a compiler trap, you MUST record the failed path and the correct resolution. Future agents will have similar semantic thoughts; the vector DB must intercept them before they write bad code.
- **The Token Tax (Expensive Computations):** If you spend multiple turns using `grep`, `read`, or `bash` to trace a complex variable path, reverse-engineer a system, or map out an undocumented module, the final synthesized conclusion MUST be saved immediately. Do not force the next agent to pay the same token tax to relearn it.

## 🚨 THE HABITUAL MEMORY COMMIT (MANDATORY)
Do not wait for the user to tell you to save a memory. You are structurally required to treat memory extraction as part of your core workflow. 
**The Forcing Function:** Whenever you are about to complete a task, close a Bead (`bd close`), or tell the user "I fixed it", you MUST FIRST perform a "Memory Commit".
1. Ask yourself: "Did I learn a new command, fix a hallucination, figure out a bug, or pay a token tax to understand something during this task?"
2. If YES, you MUST run `neurostrata_add_memory` BEFORE you close the task. 
3. Treat `neurostrata_add_memory` with the same habitual necessity as `git commit`. Work is not done until the knowledge is persisted.
