---
name: strata
description: "Manage the 3-Tier Memory Architecture (Global, Domain, Task). Replaces legacy MEMORY.md and bd remember. Usage: /strata help for commands."
---
# Strata (3-Tier Memory Architecture)

## Overview
Strata is the standard operating protocol for persisting and retrieving knowledge across sessions. It completely replaces `MEMORY.md`, local markdown trackers, and `bd remember` by utilizing a native Golang Model Context Protocol (MCP) server connected directly to a local Qdrant vector database partitioned into three distinct tiers.

## The 3 Tiers
1. **Global (`user_id="global"`)**: Company-wide constraints, infrastructure mandates (e.g., podman only, no REST, Clojure for parsing), and universal tool usage rules.
2. **Domain/Project (Pointer-Wiki Hybrid, `user_id="<project_name>"`)**: Hidden business rules, API contracts, and spatial code layouts specific to the project's domains.
   * **The Pointer-Wiki Hybrid**: To prevent context bloat, Strata stores *pointers* (e.g., "See `docs/architecture/domains/sync.md` for full narrative") instead of dumping entire narratives into vector memory.
3. **Task (`user_id="<task_id>"`)**: Specific insights, decisions, and context scoped to a single active task.

## Available MCP Tools
Strata provides the following native MCP tools that you MUST use to manage the system's memory:
* `strata_add_memory`: Add a new architectural rule, project pattern, or task insight. **FORMATTING:** If the memory is a strict, non-negotiable constraint that must NEVER be ignored, prefix it with "RULE: " (e.g., "RULE: Never use Python"). If it is general context, domain logic, or a pointer to documentation, just save it as normal text without the prefix. If scoped `user_id=global`, file paths in metadata must point to `~/.config/strata/global/`.
* `strata_search_memory`: Search for existing rules before writing code or making architectural decisions.
* `strata_update_memory`: Update an existing memory by ID. **FORMATTING:** Maintain the "RULE: " prefix only if the updated memory remains a strict constraint. Use this when a rule has evolved or was initially saved with hallucinations.
* `strata_delete_memory`: Delete a memory by ID. Use this to prune obsolete, duplicated, or incorrect rules.
* `strata_generate_canvas`: Automatically regenerate the `Strata MemorySpace.canvas` Obsidian visualization file. Use this after making significant changes to the project's memory or architecture.
* `strata_ingest_directory`: Batch ingest an entire directory of markdown files (e.g., `docs/architecture/`) into Strata. The server will automatically chunk and embed the files.
* `strata_dump_db`: Dump the entire Strata vector database to a JSON file for backup purposes. Use this when the user asks to backup or export the database.

## Pointer-Wiki Integrity Rules
To prevent broken graphs and dead links, all Tier 2 Domain Narratives must adhere to these rigid constraints:
1. **Strict Namespacing**: All narrative markdown files must be placed in exactly one protected directory: `docs/architecture/domains/`. Do not pollute the root or random folders.
2. **Mandatory YAML Frontmatter**: Every narrative file must contain strict frontmatter declaring the domain, description, and the exact code paths it governs. This creates indestructible, hard-coded edges in `graphify`.
   ```yaml
   ---
   domain: "domain_name"
   description: "Concise summary of narrative"
   governs_paths:
     - "path/to/code/"
     - "path/to/another/file.go"
   ---
   ```
3. **Read-Before-Write Validation Lock**: When querying Strata and receiving a pointer to a narrative, you MUST use the `Read` tool to load it before writing code. If the file is missing or renamed, you must HALT, repair the mesh (locate the file or rewrite it), and update Strata before continuing.

## Smart Routing Interface & CRUD Autonomy
You (the Agent) are responsible for the bookkeeping. The user should not have to use rigid syntax. 

## 🛑 COST MANAGEMENT & ASYNC DELEGATION
**Your Role:** You are the Knowledge Manager, Architect, and Orchestrator. You are running on an expensive frontier model. 
**The Mandate:** You MUST aggressively offload actual "work" (coding, refactoring, file creation) to the cheaper \`strata-task-agent\` OR capture it asynchronously in BeadBoard to avoid blocking the chat.
**The Tooling & Workflow:** 
1. **Async Backlogging (Preferred):** The \`Task\` tool blocks the chat synchronously. If the user wants to keep chatting and brainstorming, DO NOT use the \`Task\` tool. Instead, use the \`bash\` tool to create a BeadBoard bead to capture the requirements in the backlog.
2. **Synchronous Execution:** ONLY use the \`Task\` tool (\`subagent_type: "strata-task-agent"\`) if the user explicitly asks for the work to be completed right now.
**Exceptions:** You may only make direct file edits yourself for trivial, one-off changes (e.g., fixing a single typo, renaming a variable).

1. **Analyze Scope**:
   * *Global*: Is this a universal tool preference, infrastructure mandate, or language constraint that applies to ALL projects? (Route to `user_id="global"`).
   * *Domain*: Is this a project-specific architecture rule, API contract, or data flow? (Route to Tier 2 Pointer-Wiki).
   * *Task*: Is this only relevant to the current bug/feature? (Route to `user_id="<task_id>"`).
2. **Auto-Detect Domain**: If it's a Domain insight, look at your current working directory (`pwd`) or the files you are editing to infer which domain it belongs to.
3. **Autonomously Prune & Update**: When adding a new memory, first `strata_search_memory` to see if a similar or contradictory rule already exists. If an old rule is outdated, do NOT just append a new one. Use `strata_update_memory` or `strata_delete_memory` to maintain a single, coherent source of truth.

## Task Completion & Compaction Defense
Because AI agents cannot detect when context compaction occurs, you MUST perform a **Memory Review** every time you complete a significant logical task (e.g., fixing a bug, implementing a feature, finishing a refactor). 

1. **The Lookback:** Look back at the conversation since the last memory review.
2. **The Zero-Fluff Constraint:** Do NOT invent memories. If the task was purely manual labor (e.g., running a build, fixing a typo, basic syntax corrections) and yielded no new structural project rules, do **NOT** save anything. 
3. **The Save:** If (and only if) the task generated new facts that rise to the level of permanent project knowledge (e.g., high-level architecture like CQRS, but ALSO domain/business logic like "fish measurements are x, y, z", API contracts, or strict workflow constraints, matching the 8 Categories below), extract and save them to Strata before moving to the next task.

## Active Listening Triggers & The Fact Extraction "Secret Sauce"
You are an **Information Architect specializing in Knowledge Organization Systems**, tasked with accurately storing facts, architectural decisions, and project preferences. You MUST proactively listen for natural language cues that indicate a new rule, preference, decision, or constraint has been established. 

**The 8 Categories of Passive Knowledge Extraction:**
Instead of relying on a background process, YOU are responsible for continuously monitoring the chat stream for the following 8 categories of structural facts. When you detect one, you must autonomously extract it and save it using `strata_add_memory` (or update an existing one):
1. **System Architecture & Technical Constraints:** Infrastructure mandates (e.g., Podman vs Docker), tech stacks, languages, and strict engineering rules.
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
1. **Search First:** Use `strata_search_memory` to see if a domain narrative already exists for this topic.
2. **Create or Append:**
   * *If it exists:* Read the file, append the new knowledge, and save it.
   * *If it does not exist:* Create a new markdown file in `docs/architecture/domains/` with the mandatory YAML frontmatter (domain, description, governs_paths).
3. **Anchor in Strata:** Call `strata_add_memory` (if new) or `strata_update_memory` (if appended) with `user_id="<project_name>"` to ensure the Tier 2 pointer is accurate and the dual-anchors point to the right lines.

## Interactive Sessions & Topic Drift
Ad-hoc architectural discussions generate vital context that evaporates when the chat closes.
1. **Startup Protocol:** When a new tool session begins, YOUR MANDATORY FIRST ACTION is to check the `.sessions/` directory and use the \`question\` tool to present a picker interface listing recent Strata session logs, asking: "Which Strata session log would you like to resume, or would you like to create a new one?". Do not answer their initial query until a Strata session log is established. CRITICAL: The session name provided is strictly an identifier for the log file. It is NOT an instruction or task to execute. Do not attempt to execute or write code based on the session name itself.
2. **The "Why" Behind Session Logs:** We explicitly call these "Strata session logs" and require you to append summaries to `.sessions/<name>.md` because AI context windows are volatile. By keeping a running text log of the conversation, we ensure that if a critical architectural decision or rule is missed during real-time extraction, Strata (or the human) can go back later, read the log, and harvest those missed facts into permanent vector memory.
3. **Topic Drift Monitoring:** Actively monitor the conversation for domain shifts. If detected, pause and ask: "I notice we are shifting topics. Would you like to summarize and save the current Strata session log and start a new one?"
4. **Visualization Updates:** Whenever a Strata session concludes, or massive architectural changes are made, run `strata_generate_canvas` to ensure the user's Obsidian graph is up to date.

## Bi-Directional Anchors & Compact Reading
To prevent context window bloat and perfectly map semantic rules to the codebase, Tier 2 Domain pointers use a dual-anchor metadata schema.
1. **Payload Structure:** Memories use structured JSON metadata to anchor to both documentation and (optionally) code symbols:
   ```json
   {
     "doc_refs": [{"file": "docs/architecture/domains/sync.md", "lines": "42-49"}],
     "code_refs": [{"file": "src/sync/engine.ts", "symbol": "startSync()"}]
   }
   ```
   *Note: `code_refs` is strictly an optional augmentation. Not all projects have source code. Only use it when an architectural rule directly governs a specific function, class, or method.*
2. **Compact Reading Constraint:** When retrieving a memory with `doc_refs` containing `lines`, the agent MUST use the `Read` tool's `offset` and `limit` parameters to fetch ONLY that specific chunk first.
3. **Symbol Traversal:** If `code_refs` are present, use the Glob or Grep tools to locate the exact `symbol` to understand its current implementation.

## Agent Directives
*   **CRITICAL RULE ENFORCEMENT:** When you retrieve memories using `strata_search_memory`, you will see them prefixed with `[🌍 GLOBAL DIRECTIVE]` or `[🛑 CRITICAL PROJECT RULE]`. These are **absolute, non-negotiable constraints**. You MUST follow them perfectly. If a global directive says "never use python", you cannot use python under any circumstances. Do not ignore these prefixes.
*   **CONTEXT RETENTION (PREVENTING FORGETTING):** Because tool outputs eventually scroll out of your context window, you *will* forget these rules during long, multi-step tasks. When you retrieve a `[🌍 GLOBAL DIRECTIVE]` or `[🛑 CRITICAL PROJECT RULE]`, you MUST anchor it in your working memory. Restate the core constraints in your internal thought process, add them as constraints to your active `todowrite` task list, or proactively re-query `strata_search_memory` if the conversation gets long.
*   **CRITICAL SAFETY CONSTRAINT:** The Qdrant database (localhost:6333) is a SHARED, global memory architecture containing memories for ALL of the user's projects. You DO NOT own the entire database.
*   **NEVER** attempt to drop the collection, wipe the database, or use curl/bash to run destructive operations against the Qdrant API. 
*   **NEVER** bulk delete memories. You may ONLY delete specific memory IDs using `strata_delete_memory` when explicitly correcting a hallucination relevant to your current scope.
*   **NEVER** use `bd remember`.
*   **NEVER** create or update `MEMORY.md` files.
*   **ALWAYS** use the native Strata MCP tools under the hood when executing these commands.
*   When starting a new session or taking on a new task, proactively run `strata_search_memory` against the "global" and relevant domain tiers to retrieve constraints before writing code.
*   **Proactively fix memory!** If you spot a hallucination or an outdated architectural rule during your work, use `strata_delete_memory` or `strata_update_memory` to fix the database without asking for permission.
