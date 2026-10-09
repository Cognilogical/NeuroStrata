# Task Subsystem — Replacing `bd` Inside `neurostrata-mcp`

Status: implemented (v1.7.0). Bead: NeuroStrata-043. Supersedes: external `bd` CLI, dolt sync, JSONL issue files.
Naming (adopted): the subsystem is the **Central Executive**, a task record is a **Goal**, the done-funnel is **Knowledge Consolidation**, the pre-push gate is the **Supervisory Attentional System**, and Zero-Action Start is **Action Initiation**.

## 0. Thesis

Beads worked because it is a forcing function, not because it is a database. The
forcing comes from three things: a state gate before work, a gate before
completion, and a gate before push. All three survive translation into
`neurostrata-mcp`; everything else beads carries (repo-resident JSONL, dolt,
index rebuilds, commit hooks) exists only to sync that JSONL — and dies the
moment tasks live in LadybugDB, which is already shared and already durable.

**One hook survives: pre-push. One gate engine serves MCP, CLI, and hook.**

## 1. Data Model (Q1)

Tasks are memories. No new table, no new store methods, no `VectorStore` trait
change — `list`/`get`/`upsert` suffice at task volume (hundreds, not millions).

### 1.1 Task record

```json
{
  "id": "ns-x7q2",
  "payload": {
    "content": "Migrate beads issues into LadybugDB tasks",
    "user_id": "kenton",
    "memory_type": "task",
    "agent_name": "opencode",
    "location": "",
    "location_lines": "",
    "metadata": {
      "task": {
        "status": "in_progress",
        "priority": 1,
        "task_type": "task",
        "assignee": "opencode",
        "session_id": "ses_abc",
        "labels": ["migration"],
        "description": "One-shot import of .beads/beads.jsonl",
        "blocked_by": [],
        "created_at": "2026-10-08T10:00:00Z",
        "updated_at": "2026-10-08T10:12:00Z",
        "history": [
          {"from": "open", "to": "in_progress", "at": "2026-10-08T10:12:00Z",
           "by": "opencode", "note": "claim"}
        ]
      }
    }
  }
}
```

- **id**: `<ns-key>-<4 base36>` (`ns-x7q2`), collision-checked against `list`.
  Human-typeable, greppable, doubles as the record id. No UUID alongside it —
  one identifier, not two.
- **content**: the title. Embedded on create/update, so `search_memory` can
  surface tasks. Search handler excludes `memory_type="task"` by default
  (`include_tasks: true` opts in) — task chatter must not pollute rule recall.
- **status**: `open | in_progress | blocked | done`. Four states, one terminal.
  "Canceled" is `done` with `close_reason` — a fifth state buys nothing.
- **priority**: 0–4, matching beads' P0–P4.
- **history**: append-only transition log. This *is* Tier-3 task memory — the
  Breath prompt commits state here via `task_update(note=...)`, not to a
  separate log.
- **blocked_by**: plain id list in metadata, not graph edges. Task graphs are
  tiny; the handler computes readiness by joining against `list`. Reject a
  `BLOCKS` relation: one more vocabulary relation for zero query benefit.

### 1.2 Extraction edge

The Lock 2 mechanism is a graph edge, declared in the *memory's* metadata and
materialized by the existing `edge_specs` write path:

```json
{ "metadata": { "extracted_from": ["ns-x7q2"] } }
```

Requires **memory-vocabulary v2**: add relation `EXTRACTED_FROM`
(directed, source_role `memory`, target_role `task`, declaration_direction
`self_to_target`, metadata_key `extracted_from`) and memory_type `task`
(`structural: false`). The v1 test asserting exactly 3 relations moves to v2.
No other write machinery is touched — extraction is `add_memory` with one extra
metadata key.

Parent tasks: reuse `CONTAINS` (parent task → subtask). Zero new relations.

## 2. State Machine (Q5 verdict)

**Hand-rolled transition table. No library.**

Why: states are runtime JSON in a database, mutated across process invocations.
smlang/statig/rust-fsm all bind the machine to an in-memory state object whose
transitions are compile-time type or macro structure; our guards are async
database queries ("does an EXTRACTED_FROM edge exist?"). Adapting a
compile-time FSM to runtime data means re-parsing the state string on every
call and bypassing the type safety that justified the library — you pay the
dependency and get none of the proof. The actual machine is 4 states and 8
legal edges:

```rust
// src/task/machine.rs
pub enum Status { Open, InProgress, Blocked, Done }

pub struct Transition { pub from: Status, pub to: Status, pub guard: Guard }

pub enum Guard {
    None,
    RequireReason,        // note/reason param must be non-empty
    ExtractionRequired,   // >=1 inbound EXTRACTED_FROM edge, or `memory` param
}

pub const TABLE: &[Transition] = &[
    t(Open,       InProgress, None),               // claim: sets assignee+session
    t(Open,       Blocked,    RequireReason),
    t(Open,       Done,       ExtractionRequired), // via task_complete only
    t(InProgress, Open,       None),               // unclaim: clears assignee
    t(InProgress, Blocked,    RequireReason),
    t(InProgress, Done,       ExtractionRequired), // via task_complete only
    t(Blocked,    Open,       RequireReason),      // resolution note
    t(Done,       Open,       RequireReason),      // reopen; extraction edges retained
];

pub fn apply(task: &Task, to: Status, ctx: &Ctx) -> Result<Task, TransitionError>
```

`apply` is pure given `Ctx { reason, extraction_edge_exists, actor, session }`;
the handler fills `Ctx` from the DB, then upserts. ~80 lines with tests.
Illegal transitions return the legal set from the current state, so the error
message teaches the machine.

**Funnel invariant:** `task_update` rejects `status="done"` unconditionally,
pointing at `task_complete`. `done` has exactly one entrance, and it is guarded.

## 3. MCP Tools

Registered in `tools/list` / dispatched in `tools/call` in `src/server.rs`,
same hand-rolled JSON-RPC pattern as existing tools. Handlers live in
`src/task/mod.rs`; `process_mcp_request` gains eight match arms.

### 3.1 Signatures

```rust
async fn handle_task_create(args: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String;
async fn handle_task_claim(args: Value, store: Arc<dyn VectorStore>) -> String;
async fn handle_task_update(args: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String;
async fn handle_task_list(args: Value, store: Arc<dyn VectorStore>) -> String;
async fn handle_task_complete(args: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>,
                              dedup: Option<Arc<DeduplicationChecker>>) -> Result<String, String>; // Err -> JSON-RPC error
async fn handle_task_validate(args: Value, store: Arc<dyn VectorStore>) -> String;
async fn handle_bootstrap(args: Value, store: Arc<dyn VectorStore>) -> String;
async fn handle_task_setup(args: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String;
```

### 3.2 JSON schemas

**neurostrata_task_create**
```json
{ "name": "neurostrata_task_create",
  "description": "Create a tracked unit of work. Zero-Action Start: no file edits before a task exists and is claimed.",
  "inputSchema": { "type": "object", "properties": {
    "namespace":  { "type": "string" },
    "title":      { "type": "string" },
    "description":{ "type": "string" },
    "task_type":  { "type": "string", "enum": ["task","bug","feature","epic"], "default": "task" },
    "priority":   { "type": "integer", "minimum": 0, "maximum": 4, "default": 2 },
    "labels":     { "type": "array", "items": { "type": "string" } },
    "parent_id":  { "type": "string", "description": "Parent task id (epic decomposition). Writes a CONTAINS edge." },
    "blocked_by": { "type": "array", "items": { "type": "string" } }
  }, "required": ["namespace","title"] } }
```

**neurostrata_task_claim** — `{id, namespace, assignee, session_id}` →
transition open→in_progress. Error if claimed by another live session.

**neurostrata_task_update**
```json
{ "properties": {
    "id": {"type":"string"}, "namespace": {"type":"string"},
    "title": {"type":"string","maxLength":200,"description":"New title. Rewrites content, re-embeds, appends 'renamed: old -> new' to history."},
    "status": {"type":"string","enum":["open","in_progress","blocked"]},
    "note": {"type":"string","description":"Appended to history. The Breath prompt lands here."},
    "priority": {"type":"integer"}, "assignee": {"type":"string"},
    "add_labels": {"type":"array","items":{"type":"string"}},
    "blocked_by": {"type":"array","items":{"type":"string"}}
  }, "required": ["id","namespace"] }
```
`status:"done"` here ⇒ hard error: *"done is reachable only via
neurostrata_task_complete, which requires memory extraction (Lock 2)."*

**neurostrata_task_list**
```json
{ "properties": {
    "namespace": {"type":"string"},
    "status": {"type":"string","enum":["open","in_progress","blocked","done"]},
    "assignee": {"type":"string"},
    "ready": {"type":"boolean","description":"open AND all blocked_by tasks are done. This is `bd ready`."},
    "include_done": {"type":"boolean","default":false}
  }, "required": ["namespace"] }
```
Kills a separate `task_ready` tool: one filter flag instead.

**neurostrata_task_complete** (Q2 — the Lock 2 enforcement point)
```json
{ "properties": {
    "id": {"type":"string"}, "namespace": {"type":"string"},
    "reason": {"type":"string"},
    "memory": { "type": "object", "description": "Extraction written inline, atomically with completion.",
      "properties": {
        "content": {"type":"string"},
        "memory_type": {"type":"string","default":"fact"},
        "locations": {"type":"array","items":{"type":"object"}}
      }, "required": ["content"] },
    "link_memory_id": {"type":"string","description":"Id of an already-written memory to bind via EXTRACTED_FROM."}
  }, "required": ["id","namespace"] }
```

Success requires **at least one inbound EXTRACTED_FROM edge after the call**,
achieved three ways, checked in order:
1. Edge already exists (memory written earlier with `extracted_from: [id]`).
2. `link_memory_id` given → handler fetches that memory, appends
   `extracted_from: [id]` to its metadata, re-upserts with its existing vector
   (no re-embed), edges re-materialize.
3. `memory` given → handler runs the add_memory pipeline (embed + dedup check +
   upsert) with `metadata.extracted_from = [id]`, then transitions.

Failure (none supplied, none exists) is a **JSON-RPC error**, not a success
payload — same `-32603` path as `get_snapshot` encoding failures:

```
-32603: BLOCKED (Lock 2): task ns-x7q2 cannot complete without an extracted
memory. Either call neurostrata_add_memory with metadata extracted_from:
["ns-x7q2"], or retry neurostrata_task_complete with `memory.content`
(what did you learn? what rule does this task prove?) or `link_memory_id`.
```

Success returns `{task, extracted_memory_ids, transition}` as JSON text.

**neurostrata_task_validate** — advisory report: `{violations[], stale[],
unextracted_done[], counts}`. Same engine as the git gate (§5), non-blocking.

**neurostrata_bootstrap** (Q3 — new project, MCP-as-instructor)
```json
{ "properties": { "namespace": {"type":"string"}, "project_root": {"type":"string"},
                  "project_description": {"type":"string"} },
  "required": ["namespace","project_root"] }
```
Returns (text JSON the agent executes):
```json
{
  "namespace": "MyProj",
  "files": [
    {"path": "AGENTS.md", "overwrite": false, "content": "<template incl. task rules>"},
    {"path": ".NeuroStrata/docs/.gitkeep", "overwrite": false, "content": ""}
  ],
  "first_task": {"id": "myproj-a1b2", "title": "Ingest codebase AST and write 3 initial architectural rules"},
  "hooks": {"install_command": "neurostrata-mcp hooks install"},
  "instructions": [
    {"step": 1, "action": "write_file", "path": "AGENTS.md"},
    {"step": 2, "action": "run", "cmd": "neurostrata-mcp hooks install"},
    {"step": 3, "action": "call_tool", "tool": "neurostrata_task_claim",
     "params": {"id": "myproj-a1b2"}},
    {"step": 4, "action": "call_tool", "tool": "neurostrata_ingest_directory"}
  ],
  "rule": "Zero-Action Start is now active: no file edits without a claimed task."
}
```
`first_task` is created server-side by the call — the instructor leaves
concrete work, not advice.

**neurostrata_task_setup** (Q3/Q6 — existing project)
```json
{ "properties": { "namespace": {"type":"string"}, "project_root": {"type":"string"} },
  "required": ["namespace","project_root"] }
```
Server scans (git remote, package manifests, CI configs, `.beads/`, existing
hooks, AGENTS.md) and returns:
```json
{
  "detected": {"git_remote": "github:me/proj", "languages": ["rust"],
               "ci": ["github-actions"], "agents_md": true,
               "beads_dir": true, "existing_hooks": ["pre-push"]},
  "suggested_rules": [{"content": "...", "memory_type": "rule", "rationale": "detected cargo workspace"}],
  "conflicts": [{"kind": "legacy_hook", "path": ".git/hooks/pre-push",
                 "resolution": "neurostrata-mcp hooks install --force replaces it"}],
  "tasks_created": [{"id": "proj-c3d4", "title": "Migrate 12 beads issues",
                     "metadata_hint": "run: neurostrata-mcp task import proj --from-beads .beads/beads.jsonl"}],
  "instructions": [ {"step": 1, "action": "run", "cmd": "neurostrata-mcp task import ..."}, "..." ]
}
```
Setup never writes repo files itself — it returns instructions and creates
tasks. Repo mutation happens through one explicit CLI (`hooks install`) or the
agent. Keeps the MCP surface side-effect-light and auditable.

## 4. Daemon Route

`src/daemon.rs` gains one route beside `/backup`:

```
POST /tasks/gate  { "namespace": "ns", "strict": true }
              ->  { "ok": bool, "violations": [ { "id", "kind", "detail" } ] }
```

Pure metadata query over `list` — no embedder, no judgment, milliseconds.

## 5. Git Hook (Q4)

### 5.1 CLI surface

```
neurostrata-mcp task gate <namespace> [--strict]   # exit 0 clean, 1 violations, 2 infra error
neurostrata-mcp task validate <namespace>          # human/JSON report, always exit 0
neurostrata-mcp task import <namespace> --from-beads <path>
neurostrata-mcp hooks install [--force]            # writes .git/hooks/pre-push
```

`task gate` mirrors the `Backup` dispatch in `main.rs`:
1. `probe_daemon()` responsive → `POST /tasks/gate`, print verdict, exit by it.
   ~10 ms, no DB open, no lock fight with the daemon (which is the single
   writer — direct open while the daemon runs is already refused elsewhere).
2. Daemon absent → open `LadybugStore` with `embed::configured_dimensions()`
   (no model load — gate is metadata-only), run the gate engine, exit.
   Embedded LadybugStore cold open is tens of ms; imperceptible inside a `git push`.
3. Daemon silent/busy → exit 2.

Default policy: exit 2 **warns loudly and allows** the push — an unavailable DB
is an ops problem, not agent misconduct, and blocking all pushes on a wedged
daemon re-creates the two-minute stall main.rs documents. `strict` mode
(`--strict`, set by the hook installer) flips exit 2 to blocking. Both policies
print the escape hatch (`NEUROSTRATA_SKIP_GATE=1`), mirroring the existing
`NEUROSTRATA_SKIP_CHECK` convention.

### 5.2 Hook script (`.git/hooks/pre-push`, installed by `hooks install`)

```bash
#!/bin/bash
# NeuroStrata task gate. One hook is the whole enforcement surface:
# tasks live in LadybugDB, never in the repo, so there is nothing to
# commit, sync, or rebuild.
command -v neurostrata-mcp >/dev/null || exit 0
[ -n "$NEUROSTRATA_SKIP_GATE" ] && exit 0

NAMESPACE=$(basename "$(git rev-parse --show-toplevel)")
neurostrata-mcp task gate "$NAMESPACE" --strict
code=$?
if [ $code -eq 1 ]; then
    echo "Push blocked: resolve the tasks above, or NEUROSTRATA_SKIP_GATE=1 git push" >&2
    exit 1
fi
exit 0   # 0 and 2 both pass here unless --strict flips 2 to blocking
```

### 5.3 Gate rules (`src/task/gate.rs`, shared engine)

Violation iff:
1. Any task `in_progress` in the namespace (unfinished claimed work).
2. Any `done` task with zero inbound EXTRACTED_FROM edges — impossible by
   construction via `task_complete`, checked anyway because import paths and
   hand edits exist. `grandfathered: true` (beads import) exempt.
3. Any task `open` with priority 0 older than 24h (P0 rot).

Stale (advisory, never blocking): `in_progress` with no update in >60 min —
surfaced by `task_validate` and `get_snapshot`, not the gate.

### 5.4 Session completion (Q7)

"Done" for a session = `git push` exits 0. The pre-push gate is the single
enforcement point; `task_validate` is its advisory twin for mid-session use.
The old Lock 1 (DB-mtime heuristic) is deleted — the gate subsumes it strictly:
extraction is now checked per-task by graph edge, not inferred from a
directory timestamp.

## 6. Stickiness — Why Tasks Beat Memories (req 5)

Consequences, not documentation:

1. **Snapshot injection.** `handle_get_snapshot` prepends claimed/ready tasks
   and the line *"Zero-Action Start: claim or create a task before editing."*
   Every session starts inside the task system because the mandatory pre-flight
   tool carries it. No new tool call required of the agent.
2. **The done funnel** (§2): `done` unreachable except through extraction.
3. **Push denial** (§5): the only path to "session finished" runs the gate.
4. **Claim exclusivity:** `task_claim` errors if another live session holds
   the task — duplicate work fails loudly at claim time, not at merge time.
5. **Breath prompt lands on the task:** `task_update(note=...)` appends
   history; Tier-3 state is the task record, so mid-task recovery is `task_list`
   + `get`, not log archaeology.
6. **Future shim point (core stays agnostic):** `guard` may query the gate
   verdict to refuse file writes with no claimed task. Core exposes the verdict;
   `plugins/<client>/` decides to enforce. Not in v1.

## 7. Migration from beads (req 6)

One-shot, idempotent, CLI-only (daemon stopped, like other DB-mutating CLI):

```
bd export .beads/beads.jsonl   # or: bd list --json > beads.json
neurostrata-mcp task import NeuroStrata --from-beads .beads/beads.jsonl
```

Field map: `id → metadata.task.bead_id` (original key preserved, e.g.
`NeuroStrata-e49`), `title → content`, `description → metadata.task.description`,
`type/priority/labels` direct, `closed → done` + `close_reason`, deps →
`blocked_by`, timestamps preserved. Imported closed tasks get
`metadata.task.grandfathered = true` — gate exempts them from the extraction
check (retroactive extraction demands on 40 historical issues is friction, not
enforcement). Idempotency key: `bead_id`; re-runs skip. After import,
`.beads/` is deleted from the repo and `bd` uninstalled — one system, not two.

## 8. Existing-Project Integration (Q6, step-by-step)

1. Agent calls `neurostrata_task_setup` → gets `detected`, `suggested_rules`,
   `tasks_created`, `instructions`.
2. Agent runs `neurostrata-mcp hooks install --force` (replaces legacy
   mtime hook; `--force` because one exists).
3. If `beads_dir` detected: agent runs the §7 import (task created for it).
4. Agent writes suggested rules via `neurostrata_add_memory` (each is a task
   if there are >3 — otherwise just do it).
5. `neurostrata_task_complete` on the setup tasks — which *forces the first
   extractions*, seeding the memory graph from the migration itself.
6. First push runs the gate. System is live.

## 9. Rejected Alternatives (one line each)

- **smlang / statig / rust-fsm / typed-fsm**: compile-time state objects for a
  runtime-persisted 4-state record — dependency without the proof.
- **Temporal/Restate/Camunda/Windmill**: distributed durable-execution for
  distributed long-running services; we have a 4-state row.
- **Separate tasks table in LadybugDB**: splits the memory model; tasks are memories
  with a vocabulary entry, and GraphRAG traversal should reach them unchanged.
- **`VectorStore` trait extensions (`list_tasks` etc.)**: `list`+filter suffices
  at task volume; trait surface is load-bearing and stays frozen.
- **JSONL mirror in-repo (beads-style sync)**: reintroduces pre-commit/
  post-commit/post-merge hooks and dolt to sync what the DB already shares.
- **BLOCKS graph relation**: readiness is a metadata join over hundreds of
  rows; a vocabulary relation buys no query anyone runs.
- **Daemon-only gate**: makes push depend on a live daemon; the direct-open
  fallback keeps the gate to git + our binary.
- **`task_ready` tool**: a `ready` flag on `task_list`; one filter, not one tool.
- **Fifth state (`canceled`)**: `done` + `close_reason`; terminal-state arity
  is not expressive power.
- **File-writing `setup`**: MCP returns instructions; repo mutation goes
  through one auditable CLI command or the agent's own edits.
- **Blocking gate on daemon-silent by default**: punishes ops failure as agent
  misconduct; `--strict` opts in.

## 10. Implementation Footprint (not built here)

- `src/task/mod.rs` — handlers, ~400 lines.
- `src/task/machine.rs` — §2 table + guards, ~80 lines with tests.
- `src/task/gate.rs` — §5.3 engine shared by MCP validate / CLI / daemon route.
- `src/server.rs` — 8 tool schemas + 8 dispatch arms; snapshot injection;
  task-exclusion filter in search.
- `src/daemon.rs` — one route.
- `src/main.rs` — 4 CLI subcommands (`task gate|validate|import`, `hooks install`).
- `src/schemas/memory-vocabulary.v2.json` — +`task` type, +`EXTRACTED_FROM`.
- `scripts/install_hooks.sh` — deleted, replaced by `hooks install`.
- `AGENTS.md` — §1 rewritten from `bd` commands to task tools at adoption.
