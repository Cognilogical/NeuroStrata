# Comprehensive Agent Rules & Constraints

This document compiles all the operational mandates, workflows, and behavioral constraints the agent is currently programmed to follow. Please review this to identify any missing triggers or rules.

## 1. Beads Issue Tracking (MANDATORY WORKFLOW)
- **Zero-Action Start:** The agent MUST NOT start writing code, modifying files, or executing a task until it has been officially tracked in Beads.
- **Workflow:**
  1. Check for existing work: `bd ready`
  2. If the user's request matches an existing issue, claim it: `bd update <id> --claim`
  3. If the user's request is new, create it FIRST: `bd create --title="..." --description="..." --type=task` and then claim it.
- **State Updates:** Must use the `bd set-state <bead_id> state=<state> --reason "..."` command to transition states (spawning, running, working, done). *Deprecated `bd agent state` commands must not be used.*
- **No Alternatives:** Never use TodoWrite, TaskCreate, or markdown TODO lists for tracking. Always use `bd`.

## 2. Session Completion & Hand-off
Work is NOT complete until `git push` succeeds and knowledge is extracted.
- **Completion Steps:**
  1. **Extract Knowledge:** Run `neurostrata_add_memory` to save facts, fixes, or constraints.
  2. **File Follow-ups:** Create beads for remaining work.
  3. **Quality Gates:** Run tests, linters, builds.
  4. **Status:** Close finished beads.
  5. **Push to Remote (CRITICAL):**
     - `git pull --rebase`
     - `bd dolt push`
     - `git push`
     - `git status` (Must show "up to date with origin")
- **Never Strand Work:** Never stop before pushing. Never say "ready to push when you are" (the agent must do it).

## 3. NeuroStrata Memory & The 3 Resilient Soft Locks
Memory architecture is the single most important aspect of this system. It is NON-OPTIONAL. You MUST use the `neurostrata_add_memory` tool for explicit architectural rules and decisions, and the `neurostrata_append_log` tool for conversational context.

- **CRITICAL RESTRICTION**: NEVER use `bd remember` to store memories. That tool is deprecated for agent use. You MUST use the dedicated `neurostrata_add_memory` tool.
- **Lock 1 (Pre-Push Hook):** The system enforces logging via a git hook. If a push is blocked, the agent must run `neurostrata_add_memory` before retrying.
- **Lock 2 (Checklist Abstraction):** Transitioning a bead from `working` to `done` must be accompanied by an architectural summary via `neurostrata_add_memory`.
- **Lock 3 (The "Breath" Prompt):** If a task takes more than 3-5 steps, the agent must pause, summarize the current state, and commit it to Tier 3 (Task Stratum) memory before proceeding.
- **Continuous Backup Protocol:** Silently use `neurostrata_append_log` to maintain a running log of the conversation. Pass tags (e.g., "auth, database") when a topic switch occurs.
- **Retrieval Protocol (MANDATORY):** Every time you start a new session, or if a user asks about previous system design, YOU MUST proactively use `neurostrata_query` or grep `.neurostrata/sessions/*` (if local fallback is needed) to retrieve the context before answering or coding.

## 4. Bootstrapping & Ingestion
- **Docs:** If `.NeuroStrata/docs/` is missing, proactively invoke `./scripts/bootstrap.sh <pwd>`.
- **AST Ingestion:** On fresh install, entering a new codebase, or after structural changes, proactively ingest the AST using `neurostrata_ingest_directory` (or `neurostrata-mcp ingest ...`), followed by `neurostrata-mcp export-graph` to refresh the UI.

## 5. Global Database Constraints (Safety)
- **Shared Architecture:** The database (LadybugDB) is a SHARED, global memory architecture.
- **No Destructive Operations:** NEVER attempt to delete the DB directory, drop tables, or run destructive operations.
- **No Bulk Deletes:** Only delete specific memory IDs using `neurostrata_delete_memory` when explicitly correcting a hallucination.

## 6. Global Infrastructure & Tooling Constraints
- **Containers:** ALWAYS use `podman` and `podman-compose`. NEVER use `docker`.
- **Data Formats:** ALWAYS prefer strict JSON and JSON Schema over YAML, TOML, or their derivatives.
- **Non-Interactive Shells:** ALWAYS use non-interactive flags (e.g., `cp -f`, `rm -rf`, `apt-get -y`) to avoid hanging the agent on confirmation prompts.

## 7. Cost Management & Async Delegation
- **Role:** The primary agent acts as Knowledge Manager, Architect, and Orchestrator.
- **Offloading Work:** Aggressively offload "work" (coding, refactoring) to `NeuroStrata-Task-Agent` OR capture it asynchronously in BeadBoard to avoid blocking the chat.
- **Synchronous vs Asynchronous:** Only use the `Task` tool synchronously if the user explicitly asks for the work to be completed right now. Otherwise, create a BeadBoard bead to capture requirements.
- **Exceptions:** The primary agent may only make direct file edits for trivial, one-off changes (fixing typos, renaming a variable).

## 8. Core Engineering Mandates
- **Conventions:** Rigorously adhere to existing project conventions (formatting, naming, frameworks).
- **Libraries/Frameworks:** NEVER assume a library is available. Verify in configuration files first.
- **Comments:** Add comments sparingly, focusing on *why* rather than *what*. Never talk to the user through code comments.
- **Paths:** Always use absolute paths when using file system tools.