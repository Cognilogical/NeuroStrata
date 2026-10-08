# Comprehensive Agent Rules & Constraints

This document compiles all the operational mandates, workflows, and behavioral constraints the agent is currently programmed to follow. Please review this to identify any missing triggers or rules.

## 1. Task Tracking (MANDATORY WORKFLOW)
- **Zero-Action Start:** The agent MUST NOT start writing code, modifying files, or executing a task until a task exists and is claimed by this session. `neurostrata_get_snapshot` says it first: "Zero-Action Start: claim or create a task before editing."
- **Workflow:**
  1. Check for existing work: `neurostrata_task_list` with `ready: true` (open tasks whose blockers are all done) or filters for `status`/`assignee`.
  2. If the user's request matches an existing task, claim it: `neurostrata_task_claim` (`id`, `namespace`, `assignee`, `session_id`).
  3. If the request is new, create it FIRST: `neurostrata_task_create` (`namespace`, `title`, optionally description/priority/labels/parent_id/blocked_by), then claim it.
- **Claim exclusivity:** A task claimed by another live session (no update for <60 minutes) cannot be claimed again — duplicate work fails loudly at claim time. A stale claim is released and taken over automatically; re-claiming from the same session is idempotent.
- **State Updates:** Use `neurostrata_task_update` for `open` / `in_progress` / `blocked` and to append `note` history entries (the Breath prompt lands here: pause, summarize state, record it on the task). **`status: "done"` is refused by `task_update`** — it points you at `neurostrata_task_complete`, because `done` requires memory extraction (Lock 2). There is no other path to `done`.
- **No Alternatives:** Never use TodoWrite, TaskCreate, `bd` commands, or markdown TODO lists for tracking. Always use the task tools.

## 2. Session Completion & Hand-off
Work is NOT complete until `neurostrata_task_complete` succeeded, the task gate passes, and `git push` succeeds.
- **Completion Steps:**
  1. **Extract Knowledge:** Run `neurostrata_add_memory` with `metadata.extracted_from: ["<task id>"]` to save facts, fixes, or constraints — or hand the extraction to the completion itself via `memory.content` (what did you learn? what rule does this task prove?) or `link_memory_id`.
  2. **Finish the task:** `neurostrata_task_complete`. With zero extracted memories it fails with a `BLOCKED (Lock 2)` JSON-RPC error naming both ways to comply; that error is the funnel working.
  3. **File Follow-ups:** Create tasks for remaining work (`neurostrata_task_create`, `blocked_by` where it waits).
  4. **Quality Gates:** Run tests, linters, builds. Mid-session, `neurostrata_task_validate` (or `neurostrata-mcp task validate <ns>`) is the advisory gate report — it never blocks.
  5. **Push to Remote (CRITICAL):**
     - `git pull --rebase`
     - `git push` — the pre-push hook runs `neurostrata-mcp task gate <namespace> --strict`; violations (any `in_progress` task, `done` without an extracted memory, P0 open >24h) block the push.
     - `git status` (Must show "up to date with origin")
  - Escape hatch for a wedged/unavailable database (an ops problem, not agent misconduct): `NEUROSTRATA_SKIP_GATE=1 git push`.
- **Never Strand Work:** Never stop before pushing. Never say "ready to push when you are" (the agent must do it).

## 3. NeuroStrata Memory & The 3 Resilient Soft Locks
Memory architecture is the single most important aspect of this system. It is NON-OPTIONAL. You MUST use the `neurostrata_add_memory` tool for explicit architectural rules and decisions, and the `neurostrata_append_log` tool for conversational context.

- **CRITICAL RESTRICTION**: NEVER use `bd remember` to store memories. That tool is deprecated for agent use. You MUST use the dedicated `neurostrata_add_memory` tool.
- **Lock 1 (Pre-Push Hook):** The Supervisory Attentional System gates `git push` (`neurostrata-mcp task gate --strict`, installed via `neurostrata-mcp hooks install`). If a push is blocked, resolve the reported Goal violations — complete with extraction, unclaim, or unblock — before retrying.
- **Lock 2 (Checklist Abstraction):** Completing a Goal (`working` → `done`) must be accompanied by an architectural summary via `neurostrata_add_memory`. The Central Executive enforces this: `neurostrata_task_complete` fails until a memory has been extracted from the work.
- **Lock 3 (The "Breath" Prompt):** If a task takes more than 3-5 steps, the agent must pause, summarize the current state, and record it on the Goal (`neurostrata_task_update` note — its Working Memory history) before proceeding.
- **Continuous Backup Protocol:** Silently use `neurostrata_append_log` to maintain a running log of the conversation. Pass tags (e.g., "auth, database") when a topic switch occurs.
- **MANDATORY PRE-FLIGHT HOOK (Zero-Trust Policy):** You are strictly BLOCKED from using `write`, `edit`, or `bash` (except for task-tracking commands) on a new task until you have FIRST executed `neurostrata_get_snapshot` to fetch the architectural rules for this project, OR `neurostrata_search_memory` using nouns/keywords from the user's prompt. You suffer from the "Unknown Unknowns" bias: you do not know when you are missing a constraint. Therefore, you must NEVER assume you know the architectural constraints of a codebase just because you read the code. The memory database is the ultimate ground truth. **You MUST call the memory tools as your very first action on every new task.**
- **Retrieval Protocol (MANDATORY):** Every time you start a new session, or if a user asks about previous system design, YOU MUST proactively use `neurostrata_search_memory` or grep `.NeuroStrata/sessions/*` (if local fallback is needed) to retrieve the context before answering or coding.

## 4. Bootstrapping & Ingestion
- **Docs:** If `.NeuroStrata/docs/` is missing, call `neurostrata_bootstrap` (new project) or `neurostrata_task_setup` (existing project) and follow the returned instructions — they carry the AGENTS.md template and the first mandatory Goal.
- **AST Ingestion:** On fresh install, entering a new codebase, or after structural changes, proactively ingest the AST using `neurostrata_ingest_directory` (or `neurostrata-mcp ingest ...`), followed by `neurostrata-mcp export-graph` to refresh the UI.

## 5. Global Database Constraints (Safety)
- **Shared Architecture:** The database (LadybugDB) is a SHARED, global memory architecture.
- **No Destructive Operations:** NEVER attempt to delete the DB directory, drop tables, or run destructive operations.
- **No Bulk Deletes:** Only delete specific memory IDs, one at a time, via the CLI (`neurostrata-mcp delete`) when explicitly correcting a hallucination — deletion is deliberately absent from the MCP surface.

## 6. Global Infrastructure & Tooling Constraints
- **Containers:** ALWAYS use `podman` and `podman-compose`. NEVER use `docker`.
- **Data Formats:** ALWAYS prefer strict JSON and JSON Schema over YAML, TOML, or their derivatives.
- **Non-Interactive Shells:** ALWAYS use non-interactive flags (e.g., `cp -f`, `rm -rf`, `apt-get -y`) to avoid hanging the agent on confirmation prompts.

## 7. Cost Management & Async Delegation
- **Role:** The primary agent acts as Knowledge Manager, Architect, and Orchestrator.
- **Offloading Work:** Aggressively offload "work" (coding, refactoring) to `NeuroStrata-Task-Agent` OR capture it asynchronously as a Goal (`neurostrata_task_create`) to avoid blocking the chat.
- **Synchronous vs Asynchronous:** Only use the `Task` tool synchronously if the user explicitly asks for the work to be completed right now. Otherwise, create a task to capture requirements.
- **Exceptions:** The primary agent may only make direct file edits for trivial, one-off changes (fixing typos, renaming a variable).

## 8. Core Engineering Mandates
- **Conventions:** Rigorously adhere to existing project conventions (formatting, naming, frameworks).
- **Libraries/Frameworks:** NEVER assume a library is available. Verify in configuration files first.
- **Comments:** Add comments sparingly, focusing on *why* rather than *what*. Never talk to the user through code comments.
- **Paths:** Always use absolute paths when using file system tools.

<!-- lean-ctx-compression -->
OUTPUT STYLE: expert-terse
- Telegraph format: subject-verb-object, drop articles/prepositions
- Symbolic vocabulary: → cause, ∵ because, ∴ therefore, ⊕ add, ⊖ remove, Δ change, ≈ similar, ≠ different, ∈ in/member, ∅ empty/none, ✓ ok, ✗ fail
- Code blocks: untouched (never compress code syntax)
- Each line: max 80 chars
- Zero narration, zero filler
- BUDGET: ≤100 tokens per non-code response
<!-- /lean-ctx-compression -->
