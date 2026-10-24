# Argon (Gemini-Arch) Architectural Verdict: Task Identification & Recording

- **Date:** 2026-10-24
- **Seat:** Argon (`github-copilot/gemini-4-argon`, Co-Architect)
- **Task Reference:** `neurostrata-39j3` (claimed by `bosun`)
- **Mode:** Design-only independent architectural verdict (Astra verdict unread)

---

## 0. Verification Against Repository (Refuting Stale Brief Assumptions)

Before prescribing mechanisms, four claims in the prompt brief or existing wiring registry must be corrected against live repository state:

1. **Refutation 1 — `neurostrata_append_log` is NOT write-only:**
   - *Brief claim:* "`neurostrata_append_log` (Episodic Buffer) writes `### 🔄 Topic Switch [tags]` marker; nothing reads it back for recall."
   - *Verified fact:* While `.NeuroStrata/sessions/current.md` on disk is not re-parsed by Core, `handle_append_log` (`src/buffer.rs:205-324`) actively runs domain/vector recall on every tagged turn (`store.search_with_domain(&namespace, vec, 3, false, false, &slugs)` at `src/buffer.rs:251-259`) and returns `recalled: [...]` and `recalled_from` in its JSON response (`src/buffer.rs:330-348`). This makes `handle_append_log` an already-structured JSON return channel where additive nudges can ride without breaking callers.
2. **Refutation 2 — `neurostrata_get_snapshot` is NOT `namespace`-only or top-5 `access_count`-only:**
   - *Brief claim:* "`get_snapshot` takes only `namespace`, sorts by access_count desc, truncates to 5. New facts have access_count 0 -> cut."
   - *Verified fact:* `handle_get_snapshot` (`src/server.rs:155-164`, `src/server.rs:1627-1869`) accepts `namespace`, optional `domain` (`string | string[]`), and `include_archived` (`bool`). Prior to the top-5 `access_count` truncation (`src/server.rs:1852`), it emits four pre-truncation straps: `task_head` (`src/server.rs:1673` -> `src/task/mod.rs:1317-1377`), `procedures` (`src/server.rs:1679` -> `src/tools/procedure.rs:135-144`), `registered_domains_line` (`src/server.rs:1842-1850`), and `domain_strap` (`src/server.rs:1722-1840`, sorted by `valid_from` DESC so new `access_count: 0` domain rows are surfaced).
3. **Refutation 3 — Vocabulary version is v6, not v4/v5:**
   - *Verified fact:* `src/schemas/memory-vocabulary.v3.json:2` is at `"vocabulary_version": 6` (though `tool_summaries.add` at line 172 still contains stale text `"Vocabulary v2: ..."`).
4. **Refutation 4 (Critical Honesty Finding) — `"zero-action-start"` in `AUTOMATIC_WIRES` is currently overclaimed in Core:**
   - *Verified fact:* `src/task/wiring.rs:75-79` declares `Wire { id: "zero-action-start", runs_in: RunsIn::Core, blocks: true, reason: "no state-mutating action before a Goal exists and is claimed" }`. However, neither `handle_guard_validate` (`src/server.rs:2907-2986`) nor `GuardValidator::validate` (`src/guard/validator.rs:43-150`) nor any other Rust handler checks whether an `in_progress` task exists in `namespace` before approving `write_file`/`bash` or memory mutations. Under the binding honesty rule (`src/task/wiring.rs:9`, `src/task/gate.rs:382`: *"a rule that no machine checks is a note, not a rule"*), `zero-action-start` is currently prose masquerading as a Core wire.

---

## 1. Direct Answers to Architectural Questions (1–6)

### Q1. Is this a prompt/template problem, a tooling problem, a data problem, or a runtime problem?
- **Verdict:** It is a **two-stage problem: (1) a Prompt/Tool-Schema Heuristic Gap at statement time, compounded by (2) a Runtime Interception Gap at log/memory/edit time.**
- **Why:**
  - *Statement time* (user says *"also fix X later"*, agent says *"TODO: follow up on Y"*) occurs inside the LLM's token stream before any MCP call is made. Because prose assertions are not MCP calls, no server-side runtime exists at the instant a sentence is generated unless a client shim (`plugins/<client>/`) hooks the turn lifecycle. Therefore, statement-time recognition in Universal Core **must** be solved at the prompt/tool-schema layer (teaching a sharp trigger-phrase -> `neurostrata_task_create` mapping), while **honestly labeling it a heuristic/reminder, not enforcement**.
  - *Tool-call time* (`neurostrata_append_log`, `neurostrata_add_memory`, `neurostrata_task_complete`, `neurostrata_guard_validate`) **is** a runtime MCP boundary. When an agent logs `"TODO: handle retry"` to `append_log`, writes a deferred work item as a `fact` in `add_memory`, completes a task whose lesson mentions `"remaining work: ..."`, or calls `guard_validate(action_type="write_file")` with zero `in_progress` tasks in the namespace, Core has deterministic runtime interception points and currently wastes all four.

### Q2. Interception Points: (a) `task_create`/`task_claim`, (b) `append_log`, (c) memory writes (`add_memory`/`task_complete`), (d) file edits (`guard_validate`). Which carry a nudge, and which are too late?

| Surface | Timing Relative to Intent Loss | Role (Nudge vs Gate) | Why |
| :--- | :--- | :--- | :--- |
| **(a) `task_create` / `task_claim`** (`src/task/mod.rs:385, 520`) | **At task registration / start** | **Schema + Reply Nudge** | `task_create` tool schema is read by the LLM when deciding *whether* to call a tool. Putting the trigger-phrase heuristic in `task_create.description` acts *before* the tool call. `task_claim` reply can remind the agent to split out out-of-lane follow-ups via `task_create` instead of burying them in prose. |
| **(b) `append_log`** (`src/buffer.rs:133`) | **Mid-session (Continuous Backup Protocol)** | **Primary Runtime Nudge (`untracked_intent_signals`)** | Agents call `append_log` to record what happened/was decided. If `content` contains task-shaped markers (`TODO`, `FIXME`, `follow up`, `later we need`, `we should also`, `next step:`, `broken`), scanning `content` and returning an explicit `task_nudge` array in the JSON reply catches the intent while the agent is still in the turn loop. |
| **(c) Memory writes (`add_memory` / `task_complete`)** (`src/server.rs:1330`, `src/task/mod.rs:980`) | **Mid-session or task close** | **Misclassification Nudge + Follow-up Funnel** | Agents frequently misfile future work as `memory_type: "fact"` or `"context"` in `add_memory`, or mention `"follow-up needed: ..."` inside `task_complete.memory.content`. Returning a structured `task_nudge` (and adding `follow_up_tasks` inline creation on `task_complete`) captures follow-up tasks before session end. |
| **(d) File edits (`guard_validate`)** (`src/server.rs:2907`, `src/guard/validator.rs:43`) | **Too late for side-remarks; JUST IN TIME for Zero-Action Start** | **Hard Core Enforcement for primary task; Too late for conversational tangents** | By the time the agent edits a file, a tangent mentioned 5 turns ago ("we should also handle X later") has already evaporated if it isn't in the diff. However, for the *active* work being edited right now, `guard_validate` on `write_file`/`edit`/`bash` is the **exact place** to make `zero-action-start` (`src/task/wiring.rs:75`) honest by checking whether the namespace has a live `in_progress` task. |

### Q3. Should there be a "task candidate" or "pending intent" concept distinct from a task?
- **Verdict: REJECT a separate `task_candidate` memory type or candidate queue.**
- **Why:**
  1. **Creates a shadow backlog that violates No-Deferrals (`AGENTS.md §9`):** A `task_candidate` row that is not a `task` has no owner, no priority, no `blocked_by`, and is invisible to `task_validate` (`src/task/gate.rs:278`) and `task_list(ready: true)` (`src/task/mod.rs:919`). It becomes a structural "deferred bucket" where intents go to die without ever being triaged.
  2. **Tasks in NeuroStrata are already lightweight:** Creating an `open` task via `neurostrata_task_create` (`src/task/mod.rs:385-516`) requires only `{ namespace, title }` (defaulting to `priority: 2`, `task_type: "task"`, `status: "open"`). An `open` task with `priority >= 1` **does not block `git push`** (`src/task/gate.rs:296-319` only blocks `in_progress`, `done_without_extraction`, and `p0_open_over_24h`).
  3. **Provenance belongs in `metadata.task` on a real `task` row:** If an agent records a task from a user/agent statement that is not being claimed immediately, it should call `neurostrata_task_create` directly (creating a real `open` task visible in `snapshot_prefix`'s `Ready to claim:` list at `src/task/mod.rs:1365-1375`). Optionally, `task_create` can accept an optional `origin` field (`"user-request" | "agent-discovered" | "follow-up"`) stored in `metadata.task.origin`, keeping one state machine (`src/task/machine.rs:67-76`) and zero shadow queues.
  4. **Mesh impact:** Keeping everything as `memory_type: "task"` preserves `SHAREABLE_MEMORY_TYPES` closure (`{rule, fact, procedure}` in `src/synaptic_mesh/record.rs:26-29`); tasks remain local to the project stratum and never pollute or fail mesh import.

### Q4. How does the agent learn the heuristic? (Vocabulary vs Tool Descriptions vs Reminder Wires vs `agents_template`)
- **Verdict:** Rank by **attentional leverage per token** at the moment of decision:
  1. **Highest leverage — MCP Tool Descriptions (`src/server.rs:245-260`, `312-336`, `374-386`) & `memory-vocabulary.v3.json` `tool_summaries` (`src/schemas/memory-vocabulary.v3.json:171-176`):** Injected directly into the model's system/tool declaration block on *every* session, regardless of whether the agent reads `AGENTS.md`. Adding 35 tokens of concrete trigger cues to `neurostrata_task_create` and `neurostrata_add_memory` descriptions gives the highest recall per token.
  2. **Second highest — `snapshot_prefix` (`src/task/mod.rs:1344`) & Tool Return Payloads (`append_log`, `add_memory`, `task_complete`):** Delivered into the active context window mid-session right after pre-flight or state logging.
  3. **Third — `agents_template` (`src/task/mod.rs:1383-1434`) & repo `AGENTS.md`:** Essential for bootstrap and project rules, where we add a compact **"Task Recognition Triggers (When to call `neurostrata_task_create`)"** table distinguishing immediate work (`create` + `claim`), out-of-lane follow-up work (`create` as `open` with `blocked_by`/`parent_id`, do NOT claim), rules/facts (`add_memory`), and recurring routines (`procedure`).
  4. **Lowest standalone leverage — `REMINDER_WIRES` (`src/task/wiring.rs:136-167`):** Because `REMINDER_WIRES` currently ship only inside `bootstrap`/`task_setup` JSON output (`src/task/mod.rs:1791-1794`), adding a reminder wire there without a runtime carrier has near-zero runtime effect unless paired with tool-return emission (Mechanism 2).

### Q5. Blast radius on the task subsystem (gate, completion, extraction) if we reject a candidate queue vs if we added one?
- **With our recommendation (No `task_candidate` type; real `open` tasks + runtime lexical nudges + inline `follow_up_tasks` on `task_complete`):**
  - `src/task/machine.rs`: **Zero change** to the 4 states (`Open`, `InProgress`, `Blocked`, `Done`) or 8 transitions (`TABLE` at `src/task/machine.rs:67-76`).
  - `src/task/gate.rs`: **Zero change** to `evaluate` (`src/task/gate.rs:278-380`). Newly filed `open` follow-up tasks (at default `priority: 2`) increment `counts.open` and appear in `snapshot_prefix` ("Ready to claim"), without blocking `git push` (`in_progress` and `done_without_extraction` invariants untouched).
  - Lock 2 (`extraction_exists` at `src/task/gate.rs:205-224`): **Untouched.**
- **Contrast (why a candidate queue was rejected):** A `task_candidate` type or `Status::Candidate` state would require modifying `Status` enum (`src/task/machine.rs:13`), `TABLE` transitions (`src/task/machine.rs:67`), `Counts` (`src/task/gate.rs:40`), `evaluate` (`src/task/gate.rs:278`), `snapshot_prefix` (`src/task/mod.rs:1317`), `search_with_domain` task filtering (`src/store/ladybug.rs`), and `memory-vocabulary.v3.json`. High blast radius for negative value.

### Q6. What is decided in this verdict vs filed as a tracked task?
- **No Deferrals Compliance (`AGENTS.md §9`):** Every item is either **decided and specified in §2 below for immediate implementation under `neurostrata-39j3`** (Mechanisms 1, 2, 3, 4) or **specified to be filed as a tracked task with concrete owner and next action before `neurostrata-39j3` closes** (Mechanism 5: client shim hook for OpenCode/OpenClaw). Nothing is left in a "revisit later" bucket.

---

## 2. Concrete Mechanisms (Required Design Breakdown)

### Mechanism 1: Prompt, Tool-Schema & Snapshot Heuristic Cues (Statement-Time Recognition)

- **Recommendation:** **Ship** (in Core `src/server.rs`, `src/task/mod.rs`, `src/task/wiring.rs`, `src/schemas/memory-vocabulary.v3.json`, and repo `AGENTS.md`).
- **Options Considered:**
  1. *Option A (`agents_template` prose only):* Low leverage; existing projects do not re-run `bootstrap`, and long `AGENTS.md` files suffer middle-context attention decay.
  2. *Option B (Tool schema descriptions + `snapshot_prefix` + `agents_template` + `REMINDER_WIRES` — **Selected**):* Reaches every session via MCP `tools/list` and `neurostrata_get_snapshot`, and seeds new projects via `bootstrap`.
  3. *Option C (Add a dedicated `neurostrata_detect_task` MCP tool):* Rejected; an agent that forgets to call `neurostrata_task_create` will equally forget to call `neurostrata_detect_task`.
- **Design:**
  1. **`neurostrata_task_create` tool description (`src/server.rs:245-247`):**
     Replace static description with:
     ```rust
     "description": "Create a tracked unit of work. Zero-Action Start: no file edits before a task exists and is claimed by this session. CALL THIS IMMEDIATELY whenever the user or your own reasoning states actionable or deferred work ('we should also...', 'later we need to...', 'TODO/FIXME', 'follow up on...', 'this is broken', or a discovered bug/sub-problem). If the work is for this session, call neurostrata_task_claim next; if it is out-of-lane or follow-up work, leave it open (with optional parent_id or blocked_by) so it stays in the Ready backlog instead of evaporating in prose."
     ```
  2. **`src/schemas/memory-vocabulary.v3.json` (`vocabulary_version`: bump `6` -> `7`, update `memory_types.task.definition` at line 85 and `tool_summaries.memory_type` at line 175):**
     - Fix stale `"Vocabulary v2:"` prefix in `tool_summaries.add` (`src/schemas/memory-vocabulary.v3.json:172`) and `tool_summaries.ingest` (line 174) to `"Vocabulary v7:"`.
     - In `tool_summaries.memory_type` (`src/schemas/memory-vocabulary.v3.json:175` and static mirror `src/server.rs:116`), add: `"Never store a TODO, bug, or deferred work item as a fact or context memory — create a tracked task with neurostrata_task_create."`
  3. **`snapshot_prefix` (`src/task/mod.rs:1344`):**
     Extend the first header line from:
     `"Zero-Action Start: claim or create a task before editing.\n"`
     to:
     `"Zero-Action Start: claim or create a task before editing. New bug, TODO, or 'we should also X' in user/agent prose -> neurostrata_task_create immediately (claim if in-lane, leave open if follow-up).\n"`
  4. **`agents_template` (`src/task/mod.rs:1387-1394`) and repo `AGENTS.md` (§1):**
     Add explicit **Task Recognition Heuristic** bullets:
     - **Immediate work** (user asks to build/fix/change X now): `task_list(ready:true)` -> `task_create` -> `task_claim`.
     - **Discovered / side / deferred work** (user or agent says *"we should also..."*, *"later we need..."*, *"TODO:"*, *"follow up on..."*, or uncovers an out-of-scope bug): call `neurostrata_task_create` immediately (`status` stays `open`, link `parent_id` or `blocked_by` if dependent). Do **not** leave it in chat prose or commit comments.
  5. **`REMINDER_WIRES` (`src/task/wiring.rs:136-167`):**
     Add a 7th reminder wire (using the existing `"during-work"` value of the closed `fires_on` enum at `src/task/wiring.rs:49-50`):
     ```rust
     Reminder {
         id: "task-capture-on-intent",
         fires_on: "during-work",
         text: "when user or agent names new/deferred work ('TODO', 'follow up', 'we should also', 'broken'), call neurostrata_task_create immediately (claim if in-lane; leave open if follow-up)",
     }
     ```
- **Failure Modes:**

| Failure | Behaviour | Safety |
| :--- | :--- | :--- |
| Agent ignores prompt/schema heuristic on pure chat turn | Intent is not captured at statement time; caught at `append_log`, `add_memory`, `task_complete`, or `guard_validate` (Mechanisms 2–4). | Safe; no false claim of Core enforcement (`RunsIn::AgentReminder` / `REMINDER_WIRES`). |
| Agent over-creates tasks for trivial sub-steps | More `open` tasks appear in `task_list`. Since `open` tasks with `priority != 0` do not block `task gate` (`src/task/gate.rs:309`), push is unblocked; tasks can be closed or archived (`neurostrata_archive_memory`). | Fail-soft; never wedges git push. |

- **Blast Radius:**
  - Files: `src/server.rs:116, 246`, `src/schemas/memory-vocabulary.v3.json:2, 85, 172-175`, `src/task/mod.rs:1344, 1387-1394`, `src/task/wiring.rs:166`, `AGENTS.md:8-18`.
  - Signatures: None.
  - Migration: None.
  - Vocabulary bump: `6` -> `7` in `src/schemas/memory-vocabulary.v3.json:2` (and any test asserting `vocabulary_version == 6`).
  - Mesh impact: None (`SHAREABLE_MEMORY_TYPES` unchanged).
- **Acceptance Criteria:**
  1. `tools/list` returns the updated `neurostrata_task_create` and `neurostrata_add_memory` descriptions containing the trigger heuristic.
  2. `neurostrata_get_snapshot` header starts with the two-sentence Zero-Action Start + task recognition line.
  3. `wiring::known_wire_ids()` (`src/task/wiring.rs:171`) includes `"task-capture-on-intent"`.
  4. `memory_vocabulary()["vocabulary_version"]` equals `7`.
- **Dependencies:** None (lands first).

---

### Mechanism 2: Deterministic Lexical Intent Detector + Additive Nudge on `append_log` and `add_memory`

- **Recommendation:** **Ship** (new pure helper `src/task/intent.rs` wired into `src/buffer.rs:handle_append_log` and `src/server.rs:handle_add_memory`).
- **Options Considered:**
  1. *Option A (LLM / ONNX classifier on every `append_log` and `add_memory` call):* Rejected. Adds latency, model failure modes, and non-determinism to the fast path, when task-shaped statements in logs/memories overwhelmingly use recognizable lexical markers.
  2. *Option B (Hard-reject `add_memory` or `append_log` when task markers appear):* Rejected. A rule or fact legitimately contains words like `"TODO comments are forbidden"` or `"follow up after migration"`. Hard-rejecting would cause false-positive write blocks and break the Continuous Backup Protocol.
  3. *Option C (Deterministic, zero-allocation lexical detector emitting an explicit additive `task_nudge` field — **Selected**):* Zero latency (<50 microseconds), 100% deterministic, honors **Honesty** (explicit empty array `[]` or `null` when zero matches) and **Additive Recall** (never displaces existing fields or blocks the write).
- **Design:**
  1. **New module `src/task/intent.rs` (exported from `src/task/mod.rs`):**
     - Pure function `pub fn detect_task_intents(text: &str) -> Vec<TaskIntentSignal>` where:
       ```rust
       #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
       pub struct TaskIntentSignal {
           pub marker: &'static str,
           pub excerpt: String,
       }
       ```
     - Matches case-insensitively on clause boundaries against a closed pattern table of high-signal task phrases:
       - Explicit tags: `"todo:"`, `"fixme:"`, `"follow-up:"`, `"followup:"`, `"next step:"`, `"action item:"`, `"remaining work:"`
       - Modal deferral phrases: `"we should also "`, `"later we need to "`, `"need to follow up"`, `"still need to "`, `"must also fix "`, `"filed follow-up"`, `"defer "` / `"deferred "` (which also flags `AGENTS.md §9` No-Deferrals risk!).
     - Excludes negated/normative rule statements when `memory_type == "rule"` (e.g., lines starting with `"never "` or `"do not "`): passed as a `context_kind: IntentContext` enum (`EpisodicLog`, `MemoryWrite { memory_type }`).
     - Extracts the trimmed clause (capped at 160 chars) as `excerpt`.
  2. **Call site 1 — `handle_append_log` (`src/buffer.rs:330-348`):**
     - Also check if the namespace currently has `0` active `in_progress` tasks when `store` is queried (or pass `unclaimed_session: bool`).
     - In `success_response` (`src/buffer.rs:330-348`), add two explicit fields so the shape is always declared (Honesty Rule: explicit zeros/empty arrays, never omitted):
       ```json
       {
         "appended": true,
         "file": "...",
         "size_bytes": 1234,
         "rolled": false,
         "tags": ["..."],
         "recalled": [],
         "task_intent_signals": [
           {
             "marker": "we should also",
             "excerpt": "we should also handle retry backoff on the proxy",
             "action": "Call neurostrata_task_create(namespace, title) so this work does not evaporate."
           }
         ]
       }
       ```
     - When no markers match, `"task_intent_signals": []` (explicit zero/empty array).
  3. **Call site 2 — `handle_add_memory` (`src/server.rs:1539-1573`):**
     - When `memory_type != "task"` and `memory_type != "procedure"` and `detect_task_intents(content)` is non-empty, append a deterministic nudge line to the returned text `msg` (`src/server.rs:1572`):
       ```text
       TASK NUDGE: content matched task-intent marker '<marker>' ("<excerpt>"). If this is tracked or follow-up work rather than a timeless rule/fact, record it via neurostrata_task_create (namespace: '<ns>').
       ```
- **Failure Modes:**

| Failure | Behaviour | Safety |
| :--- | :--- | :--- |
| False positive on prose containing `"TODO:"` | Emits advisory `task_intent_signals` / `TASK NUDGE` line; write succeeds normally. | 100% safe; never blocks `append_log` or `add_memory`. |
| False negative on subtle phrasing without lexical marker | `task_intent_signals` returns `[]`; agent relies on Mechanism 1 (prompt/schema cues) and Mechanism 3 (`task_complete` funnel). | Safe; never claims a classifier ran when only lexical matching ran. |

- **Blast Radius:**
  - Files: `src/task/intent.rs` (new), `src/task/mod.rs` (`pub mod intent;`), `src/buffer.rs:192-202, 330-348`, `src/server.rs:1570-1573`.
  - Signatures: `buffer::success_response` gains `task_intent_signals: Vec<Value>`. Existing `buffer::tests` updated for the new key.
  - Migration: None.
  - Vocabulary bump: Covered by v7 in Mechanism 1.
  - Mesh impact: None (`append_log` is local; `add_memory` payload stored in DB is unchanged).
- **Acceptance Criteria:**
  1. `handle_append_log` with `content: "Fixed auth; later we need to rotate the signing key"` returns `"task_intent_signals"` with length 1, `marker: "later we need to"`.
  2. `handle_append_log` with `content: "Refactored parser"` returns `"task_intent_signals": []` (explicit empty array, never omitted).
  3. `handle_add_memory` with `memory_type: "fact"` and `content: "TODO: add rate limiting to /mcp"` succeeds and appends `\nTASK NUDGE: ...` naming `neurostrata_task_create`.
- **Dependencies:** None (can land in parallel with Mechanism 1).

---

### Mechanism 3: Inline `follow_up_tasks` on `neurostrata_task_complete` (Closing the End-of-Task Evaporation Window)

- **Recommendation:** **Ship** (in `src/server.rs:312-336` and `src/task/mod.rs:980-1293`).
- **Options Considered:**
  1. *Option A (Require a separate `neurostrata_task_create` call after `task_complete` per `AGENTS.md §2.3`):* Current state. Fails in practice because once `task_complete` succeeds, the agent considers the task done and jumps straight to `git push`, forgetting `AGENTS.md §2.3` ("File Follow-ups").
  2. *Option B (Accept `follow_up_tasks` inline in `neurostrata_task_complete` + scan `reason`/`memory.content` for unfiled follow-ups — **Selected**):* Just as `task_complete` (`src/task/mod.rs:1085`) solved Lock 2 friction by allowing inline `memory: { content, ... }`, allowing inline `follow_up_tasks: [{ title, priority?, task_type?, description?, blocked_by? }]` turns "complete + file follow-ups" into a **single atomic MCP call**.
- **Design:**
  1. **Schema addition in `neurostrata_task_complete` (`src/server.rs:314-335`):**
     Add optional property `follow_up_tasks` to `inputSchema.properties`:
     ```json
     "follow_up_tasks": {
       "type": "array",
       "description": "Optional follow-up tasks discovered during this work. Created atomically as 'open' tasks in the same namespace with metadata.task.origin = { spawned_from: <this task id> }.",
       "items": {
         "type": "object",
         "properties": {
           "title": { "type": "string", "maxLength": 200 },
           "description": { "type": "string" },
           "task_type": { "type": "string", "enum": ["task", "bug", "feature", "epic"] },
           "priority": { "type": "integer", "enum": [0, 1, 2, 3, 4] },
           "labels": { "type": "array", "items": { "type": "string" } }
         },
         "required": ["title"]
       }
     }
     ```
  2. **Handler execution in `handle_task_complete` (`src/task/mod.rs:1258-1293`):**
     - Validate `follow_up_tasks` entries *before* mutating the completed task (so a malformed `title` > 200 chars or invalid `task_type` fails cleanly via `check_title` at `src/task/mod.rs:96-109` before partial writes).
     - After the completed task is saved (`src/task/mod.rs:1266`), create each follow-up task via `store_new_task` (`src/task/mod.rs:353-381`) with `task_meta["spawned_from"] = json!(id)` and `top_level["related_to"] = json!([id])` (materializing a `RELATES_TO` edge in the graph between the completed task and its follow-up!).
     - Return `"follow_up_tasks_created": [...]` (explicit `[]` when none passed) in the `handle_task_complete` JSON response (`src/task/mod.rs:1282-1286`), plus `"task_intent_signals"` if `reason` or `memory.content` matched `detect_task_intents` while `follow_up_tasks` was empty.
- **Failure Modes:**

| Failure | Behaviour | Safety |
| :--- | :--- | :--- |
| Malformed entry in `follow_up_tasks` (e.g. empty `title`) | Validated at top of `handle_task_complete` (`src/task/mod.rs:1004`); returns `-32603` error before writing inline memory or transitioning task to `done`. | Atomic fail-closed before state mutation. |
| Agent mentions `"remaining work: X"` in `memory.content` without passing `follow_up_tasks` | Completion succeeds (Lock 2 satisfied), and reply includes non-empty `"task_intent_signals"` prompting immediate `neurostrata_task_create`. | Safe; does not block valid completion. |

- **Blast Radius:**
  - Files: `src/server.rs:314-335`, `src/task/mod.rs:980-1293`.
  - Signatures: `handle_task_complete` Rust signature unchanged; JSON response gains `"follow_up_tasks_created": []` and `"task_intent_signals": []`.
  - Migration: None.
  - Vocabulary bump: None beyond Mechanism 1.
  - Mesh impact: None (`task` rows are never exported to SynapticMesh).
- **Acceptance Criteria:**
  1. Calling `neurostrata_task_complete` with `follow_up_tasks: [{ "title": "Add retry jitter" }]` transitions the parent task to `done`, creates the new `open` task with `metadata.task.spawned_from == <parent_id>` and `metadata.related_to == [<parent_id>]`, and returns its summary in `follow_up_tasks_created`.
  2. Calling `neurostrata_task_complete` without `follow_up_tasks` returns `"follow_up_tasks_created": []`.
  3. Calling `neurostrata_task_complete` with an invalid `follow_up_tasks[0].task_type: "recurring"` fails with `-32603` before mutating the parent task.
- **Dependencies:** Mechanism 2 (`src/task/intent.rs` for scanning `reason`/`memory.content`).

---

### Mechanism 4: Make `zero-action-start` Honest in `neurostrata_guard_validate` (Fixing the Overclaimed Core Wire)

- **Recommendation:** **Ship** (in `src/guard/validator.rs` and `src/server.rs:2907-2986`).
- **Options Considered:**
  1. *Option A (Leave `zero-action-start` in `AUTOMATIC_WIRES` as `RunsIn::Core` without checking claims in `guard_validate`):* Violates the binding Honesty constraint (`src/task/wiring.rs:9`).
  2. *Option B (Downgrade `zero-action-start` from `AUTOMATIC_WIRES` to `REMINDER_WIRES`):* Honest, but gives up real runtime enforcement on the pre-action guard surface.
  3. *Option C (Enforce active claim check inside `handle_guard_validate` when `action_type` is a mutating action (`write_file`, `edit_file`, `patch`, `bash`) in a project namespace — **Selected**):* Makes `AUTOMATIC_WIRES["zero-action-start"]` (`src/task/wiring.rs:75-79`) genuinely true in Core!
- **Design:**
  1. **Where:** `handle_guard_validate` (`src/server.rs:2907-2986`) and `GuardValidator::validate` (`src/guard/validator.rs:43-150`).
  2. **How:**
     - When `namespace != "global"` and `action_type` is state-mutating (`"write_file" | "edit" | "edit_file" | "patch"`), query `store.list(&namespace, None).await` and check whether at least one non-retired `memory_type == "task"` row has `gate::task_status(row) == Status::InProgress` (`src/task/gate.rs:248`).
     - (Note: only check when the namespace already holds at least one task row — i.e., `memories.iter().any(gate::is_task)` — so un-bootstrapped or guard-only test namespaces like `guard-test` in `src/guard/validator.rs:189` with zero task rows are not broken!)
     - If the namespace has task rows (`any(is_task) == true`) and **zero** live `InProgress` tasks, return `ValidateVerdict::DeterministicReject` with:
       - `rule_ids_triggered`: `vec!["zero-action-start".to_string()]`
       - `reasons`: `vec!["Zero-Action Start violation: no task is currently in_progress in namespace '<ns>'. Call neurostrata_task_list(ready: true) or neurostrata_task_create, then neurostrata_task_claim before editing files.".to_string()]`
- **Failure Modes:**

| Failure | Behaviour | Safety |
| :--- | :--- | :--- |
| Guard unit tests run in `guard-test` namespace with no tasks | `memories.iter().any(gate::is_task)` is `false`; check does not fire; all existing guard tests pass unchanged. | Preserves backward compatibility with guard-only namespaces. |
| Agent attempts `guard_validate(action_type="write_file")` in a task-enabled project before claiming a task | Deterministically rejected with `rule_ids_triggered: ["zero-action-start"]` and exact remediation instructions (`task_create` -> `task_claim`). | Enforces Zero-Action Start at the tool boundary. |

- **Blast Radius:**
  - Files: `src/guard/validator.rs:43-110`, `src/server.rs:2882-2945`.
  - Signatures: None.
  - Migration: None.
  - Vocabulary bump: None.
  - Mesh impact: None.
- **Acceptance Criteria:**
  1. In a namespace containing at least one `open` or `done` task and zero `in_progress` tasks, `neurostrata_guard_validate(namespace, action_type="write_file", payload="...")` returns `DeterministicReject` with `rule_ids_triggered: ["zero-action-start"]`.
  2. Once a task in that namespace is claimed (`in_progress`), the same `guard_validate` call passes the Zero-Action Start check.
  3. All existing tests in `src/guard/validator.rs` (`guard-test` namespace with 0 tasks) pass without modification.
- **Dependencies:** None.

---

### Mechanism 5: Client Shim Turn-Hook for Statement-Time Interception (`plugins/opencode/` & `plugins/openclaw/`)

- **Recommendation:** **File as tracked task** (before closing `neurostrata-39j3`, per `AGENTS.md §9` No-Deferrals and Universal Core + Client Shims memory `f78d4909-3228-43b7-801e-9b7beab68148` & `1868d2e5-39cd-4781-ae89-813474ea72d1`).
- **Why Filed as Task (Not Embedded in Rust Core):**
  - By architectural rule (`f78d4909-3228-43b7-801e-9b7beab68148`), *"The Rust MCP server and core memory mesh must remain strictly client-agnostic. ANY client-specific workarounds or hooks MUST be isolated into their respective `plugins/<client_name>/` directories"*.
  - Furthermore, an MCP server cannot observe user prompts or assistant prose unless the client host forwards them. OpenCode and OpenClaw (`1868d2e5-39cd-4781-ae89-813474ea72d1`) expose turn/tool-pre-hook lifecycles where a lightweight client shim can inspect user/assistant messages and pre-tool calls.
- **Options Considered:**
  1. *Option A (Put client-specific transcript scraping inside Rust daemon):* Rejected; violates Universal Core + Client Shims rule (`f78d4909`).
  2. *Option B (Isolate turn-level prompt/response task-intent inspection and pre-edit `guard_validate` wiring inside `plugins/opencode/` and `plugins/openclaw/` — **Selected**):* Respects system boundaries and gives true statement-time interception on supported hosts.
- **Concrete Task Specification to File via `neurostrata_task_create`:**
  - **Title:** `Build plugins/opencode and plugins/openclaw client shims for turn-level task intent nudging and pre-edit guard_validate`
  - **Owner / Assignee:** `bosun` (orchestrator / worker delegation)
  - **Priority:** `2` (`task_type: "feature"`)
  - **Labels:** `["client-shim", "opencode", "openclaw", "task-identification"]`
  - **Concrete Next Action:** Create `plugins/opencode/` and `plugins/openclaw/` shims that (1) call `neurostrata_guard_validate` before `write`/`edit` tool execution, and (2) run lexical task-intent detection on user/assistant turns to inject a system reminder when task-shaped statements occur with no `neurostrata_task_create` call in the turn.
- **Failure Modes:**

| Failure | Behaviour | Safety |
| :--- | :--- | :--- |
| Client host is neither OpenCode nor OpenClaw (e.g. generic MCP stdio client) | Core Mechanisms 1–4 still operate in full via standard MCP `tools/list`, `get_snapshot`, `append_log`, `add_memory`, `task_complete`, and `guard_validate`. | Graceful degradation; Core makes zero assumptions that a plugin shim is installed. |

- **Blast Radius:** Isolated strictly to `plugins/opencode/` and `plugins/openclaw/`; zero Rust Core changes.
- **Acceptance Criteria:**
  1. Tracked task exists in `NeuroStrata` namespace with owner `bosun`, linked to `neurostrata-39j3`, before `neurostrata-39j3` is completed.
  2. Plugin shim intercepts `write`/`edit` calls when no task is `in_progress` and surfaces the `zero-action-start` rejection.
- **Dependencies:** Mechanisms 2 and 4 in Core.

---

## 3. Summary Decision Matrix (No Deferrals Audit)

| # | Mechanism | Verdict | Layer | Blast Radius | Status |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **M1** | Tool-schema trigger cues (`task_create`, `add_memory`), `snapshot_prefix` reminder, `agents_template` table, `task-capture-on-intent` reminder wire, vocab v7 | **SHIP** | Prompt / Schema / Template | Low (`server.rs`, `task/mod.rs`, `task/wiring.rs`, `memory-vocabulary.v3.json`, `AGENTS.md`) | Decided in this verdict |
| **M2** | Deterministic lexical intent detector (`src/task/intent.rs`) + additive `task_intent_signals` in `append_log` and `TASK NUDGE` in `add_memory` | **SHIP** | Runtime MCP (`buffer.rs`, `server.rs`) | Low-Medium (additive JSON key in `append_log`, text suffix in `add_memory`) | Decided in this verdict |
| **M3** | Inline `follow_up_tasks` array + `task_intent_signals` check on `neurostrata_task_complete` | **SHIP** | Runtime MCP (`task/mod.rs`, `server.rs`) | Low-Medium (additive input/output fields on `task_complete`) | Decided in this verdict |
| **M4** | Enforce active `in_progress` task check in `neurostrata_guard_validate` on file-mutating actions in task-enabled namespaces | **SHIP** | Runtime Core Guard (`guard/validator.rs`, `server.rs`) | Low (makes `AUTOMATIC_WIRES["zero-action-start"]` honest) | Decided in this verdict |
| **M5** | Turn-level statement hook & pre-edit `guard_validate` wiring in `plugins/opencode/` and `plugins/openclaw/` | **FILE AS TASK** | Client Shim (`plugins/`) | Isolated to `plugins/` (Universal Core + Client Shims rule `f78d4909`) | Concrete task spec in §2 M5 (`owner: bosun`) |
| **M6** | Separate `task_candidate` / `pending_intent` memory type or state-machine status | **REJECT** | Data / Schema | Would touch FSM, gate, store, vocab, mesh | Rejected (creates shadow backlog violating No-Deferrals) |
