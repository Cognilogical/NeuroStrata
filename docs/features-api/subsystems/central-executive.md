# Central Executive — Goal Lifecycle, State Machine & The 3 Soft Locks

> **Why you care:** Work is not closed until the agent learns from it — experience is forced into long-term memory before a goal can transition to `done`. The Central Executive enforces that single non-negotiable rule through a four-state machine guarded by three soft locks, plus an auto-retrieved context block injected into every spawned subagent.

## 4-State Task Machine

Every Goal in NeuroStrata moves through a strict sequential lifecycle in `src/task/machine.rs`:

| State | Trigger | Prohibition |
| :--- | :--- | :--- |
| `open` | `neurostrata_task_create` | Edits are blocked until a task exists; no bench-mark mutations succeed. |
| `in_progress` | `neurostrata_task_update` (status set to `in_progress`) | The task is active but not yet committed. |
| `blocked` | External event / declared dependency | The task refuses transitions out of itself; surfaces only the blocking cause (`src/task/machine.rs`). |
| `done` | `neurostrata_task_complete` **only after** knowledge extraction | Refused unless at least one architectural rule, fix, or fact was extracted or linked to the goal (`EXTRACTED_FROM`). |

This ordering prevents phantom completions: a subagent cannot self-close a task mid-edit by claiming it finished, because the funnel still requires actual extracted memory.

## Zero-Action Start & Exclusive Claim Leases

Before an agent touches any file or runs any mutating command, it must first claim an active goal:

1. **Snapshot gate:** Call `neurostrata_get_snapshot` — returns top-5 high-salience cognitive nodes for the project. This grounds the session in the codebase's governing rules immediately.
2. **Claim lease:** `neurostrata_task_claim` (`src/task/mod.rs`) takes an exclusive, time-bounded claim. A stale 60-minute lease is automatically taken over by a fresh claiming session; concurrent sessions targeting the same task yield and retry rather than clobber each other.
3. **Edit lock:** Once claimed, edits and shell commands proceed; unclaimed attempts are rejected at the tool boundary so the file system is never mutated without an asserted goal.

## The Three Soft Locks

### Lock 1 — Pre-Push Git Gate

`git push` runs `neurostrata-mcp task gate <namespace> --strict`. The gate refuses to ship when any of the following exist:

**Task conditions** (`src/task/gate.rs`):

- Any task in `in_progress` (unfinished work blocks everyone).
- Any task marked `done` that lacks an extracted memory (the Done-Funnel guard).
- Any P0 task open for more than 24 hours.

**Rule-honesty conditions** (`src/task/gate.rs:399-530`) — a `rule` row is a load-bearing claim, and the gate refuses to ship one that overclaims:

- A live rule with no `metadata.source`.
- A rule whose `metadata.enforcement` is not `ENFORCED`, `PARTIAL` or `NOT_ENFORCED`.
- A rule claiming `ENFORCED` whose `metadata.guard` names no known wire, or names an advisory/reminder wire that never blocks.
- A rule claiming `PARTIAL` whose `metadata.guard` names no known wire.
- A rule claiming `NOT_ENFORCED` that still names a guard.
- A rule that names a guard but declares no enforcement value.

The pre-push hook (`pre-push` binary hook, installed via `neurostrata-mcp hooks install`) enforces this — including on merged branches and pushed tags. A hard failure exits the hook non-zero so git refuses the push automatically.

### Lock 2 — Done-Funnel Mandatory Extraction

`neurostrata_task_complete` fails unless the task has produced at least one architectural rule, fix, or lesson captured permanently. `neurostrata_add_memory` is the extraction surface — writing a `rule` or `fact` row whose `EXTRACTED_FROM` references the completed goal is what makes the funnel close. If an agent claims `done` without this step, the tool throws and the session errors out rather than silently closing the goal.

This is the central Anti-Amnesia mechanism: every finished task makes the codebase smarter. The requirement is not optional metadata; the engine treats its absence as an illegal transition.

### Lock 3 — The Breath Working-Memory Prompt

Every 3–5 mutation steps, the agent pauses and records a state checkpoint onto the task's immutable history log via `neurostrata_task_update` (`note` field). This gives instant crash recovery — if a session dies mid-edit, the replay log reconstructs exactly where the agent was, rather than losing weeks of reasoning archaeology. The working-memory notes are stamped with a timestamp, session ID, and a compact summary of the current intent.

The checkpoint budget is **enforced, not advisory**: the guard state machine (`src/guard/fsm.rs`) returns a `BreathRequired` rejection once `mutations_since_note >= 5`, which blocks further mutations until the agent records a note via `neurostrata_task_update(note=...)`. The rejection names the session, the task and the worktree, plus the next legal transition.

## Auto-Retrieved Graph Context & Subagent Prompt Injection

Creating or claiming a task triggers `build_task_graph_context` (`src/task/mod.rs`) — a hybrid vector + 1-hop graph search across governing rules, facts, procedures, and AST nodes relevant to the goal. The result is returned as a ready-to-paste block (`subagent_prompt_injection`) containing the task ID, governing rules, and call/import neighbors.

Each spawned subagent plugin (OpenCode, Claude Code, Hermes, Codex CLI, GitHub Copilot CLI, OpenClaw) prepends this block automatically. A blank-brain subagent never spawns — context arrives before the prompt text begins. This is the brokerless team sync equivalent at the individual-subagent level: the graph does not care whether you share code over SSH or docker, the same context comes to every worker.
