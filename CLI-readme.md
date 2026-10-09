# 💻 NeuroStrata CLI Interface Guide

NeuroStrata provides a rich, standalone CLI binary (`neurostrata-mcp`) alongside its daemon mode to allow direct manipulation, auditing, and maintenance of the cognitive memory graph.

> [!WARNING]
> **Database Locks:** Kùzu DB is an embedded database that enforces single-process write access. You **cannot** execute write/modifying CLI commands while the main NeuroStrata daemon is running (e.g., inside an active IDE editor extension). Ensure the daemon is stopped or OpenCode is closed before executing these commands.

---

## 🛠️ CLI Commands & Endpoints

### 1. `namespaces`
Lists all initialized memory namespaces within the database.
```bash
neurostrata-mcp namespaces
```
*Output Example:*
```text
Namespaces:
  - global
  - my-rust-project
  - core-api
```

---

### 2. `list`
Prints all active memory nodes currently stored in the specified namespace.
```bash
neurostrata-mcp list <namespace>
```
*Example:*
```bash
neurostrata-mcp list my-rust-project
```

---

### 3. `ingest`
Scans a target directory, extracts structural AST concepts and symbols, and embeds them into the specified namespace.
```bash
neurostrata-mcp ingest <dir_path> <namespace> [schema_path]
```
*   `dir_path`: The directory containing source code files to parse.
*   `namespace`: Destination namespace.
*   `schema_path` (Optional): JSON file defining specific parser rules and AST patterns. If omitted, defaults to the internally extracted `schema.json`.

*Example:*
```bash
neurostrata-mcp ingest ./src my-rust-project ./custom_schema.json
```

---

### 4. `export-graph`
Exports the entire relational memory graph (nodes, relationships, and metadata) as a standardized JSON structure. Used to drive visual graph renders like the web UI or NeuroVault.
```bash
neurostrata-mcp export-graph [output_json_path]
```
*   `output_json_path` (Optional): Defaults to `.NeuroStrata/graph/graph.json`.

*Example:*
```bash
neurostrata-mcp export-graph ./graph_export.json
```

---

### 5. `delete`
Deletes a specific memory node from a namespace using its unique ID.
```bash
neurostrata-mcp delete <namespace> <id>
```
*Example:*
```bash
neurostrata-mcp delete my-rust-project 550e8400-e29b-41d4-a716-446655440000
```

---

### 6. `move`
Moves a memory into another namespace, by ID. This is the command `doctor` prints when two
spellings of one project need merging: run it once per id.

It copies the row and then deletes the original. Destructive operations are CLI and GUI
commands rather than MCP tools, so a person runs them.

```bash
neurostrata-mcp move <source_namespace> <id> <target_namespace>
```

*Example:*
```bash
neurostrata-mcp move neurostrata 550e8400-e29b-41d4-a716-446655440000 NeuroStrata
```

---

### 7. `add`
Directly embeds and adds a new custom memory node to a namespace.
```bash
neurostrata-mcp add <namespace> <type> <content> [location]
```
*   `type`: The classification of the memory (e.g., `rule`, `preference`, `architecture`).
*   `content`: The raw text content of the memory.
*   `location` (Optional): File path or contextual origin string.

*Example:*
```bash
neurostrata-mcp add my-rust-project rule "Avoid using unwrap() in library modules" "src/lib.rs"
```

---

### 8. `edit`
Modifies an existing memory node's namespace, content, and location context.
```bash
neurostrata-mcp edit <namespace> <id> <new_namespace> <content> <location>
```

*Example:*
```bash
neurostrata-mcp edit my-rust-project 550e8400-e29b-41d4-a716-446655440000 my-rust-project "Avoid using expect() or unwrap() in library modules" "src/lib.rs"
```

---

### 9. `task gate`
Runs the Supervisory Attentional System — the Central Executive's enforcement check — over a namespace. This is what the pre-push hook runs.
```bash
neurostrata-mcp task gate <namespace> [--strict]
```
*   `namespace` (Required): The exact project name.
*   `--strict` (Optional): Treat infrastructure failure (unreachable database) as blocking instead of warning.

*Exit codes:*
*   `0` — clean.
*   `1` — violations (any `in_progress` task, a `done` task without a consolidated extraction, or a P0 open beyond 24h).
*   `2` — infrastructure error (without `--strict`, this warns and allows).

*Example:*
```bash
neurostrata-mcp task gate NeuroStrata --strict
```
Escape hatch for a wedged database: `NEUROSTRATA_SKIP_GATE=1 git push`.

---

### 10. `task validate`
The advisory twin of `task gate`: the same engine, printed as a human/JSON report, always exit 0. Mid-session use is encouraged.
```bash
neurostrata-mcp task validate <namespace>
```

*Report categories:*
*   `violations` — what the gate would block a push on: any `in_progress` task, a `done` task without a consolidated extraction, a P0 left `open` beyond 24h, or rule dishonesty — a rule claiming `ENFORCED` without a real `guard` wire (`rule_overclaims_enforcement`), or a rule with no `source` provenance (`rule_without_source`).
*   `stale` — advisory only: `in_progress` with no update in over 60 minutes. Surfaced, never blocking.
*   `unextracted_done` — `done` tasks with zero `EXTRACTED_FROM` edges. Impossible through `task_complete` by construction; checked anyway because imports and hand edits exist (beads-imported history is `grandfathered` and exempt).
*   `counts` — totals per status.

*The gate contract* (all gates, ours or prescribed): exit `0` pass / `1` violation found / `2` infra error. Every gate should accept `--self-test`: exit 0 iff a planted violation makes it exit 1 and the tree is byte-identical afterward — a gate that passes its own test is not a gate.

*Example:*
```bash
neurostrata-mcp task validate NeuroStrata
```

---

### 11. `task import`
One-shot, idempotent beads migration: reads a beads JSONL export and creates Goals in the namespace. Idempotent on `metadata.task.bead_id` — re-runs skip what is already imported. Closed beads come in as `done` with `grandfathered: true` (exempt from the extraction gate).
```bash
neurostrata-mcp task import <namespace> --from-beads <path>
```
*Example:*
```bash
bd export > /tmp/beads.jsonl
neurostrata-mcp task import NeuroStrata --from-beads /tmp/beads.jsonl
```
**Migration runbook:** the import needs the daemon **stopped** (`neurostrata-mcp shutdown`; it restarts automatically on the next MCP call) — the CLI lock guards the store against a live daemon. Routing import through the daemon is an open task; until then, shut down first rather than working around the lock.

---

### 12. `hooks install`
Writes `.git/hooks/pre-push` — the one enforcement hook. Tasks live in LadybugDB, never in the repo, so there is nothing to commit, sync, or rebuild.
```bash
neurostrata-mcp hooks install [--force]
```
*   `--force` (Optional): Replace an existing pre-push hook.

*Example:*
```bash
neurostrata-mcp hooks install --force
```

---

### 13. `status`
Read-only preflight: reports the store, the daemon, who holds the lock, and **which build the daemon is running** — nothing else. The command to run when an MCP connection fails *before* anyone starts anything.
```bash
neurostrata-mcp status
```
*Exit codes:*
*   `0` — a healthy daemon is serving every console, running the same build as this binary.
*   `1` — no daemon, lock free: safe to start exactly one (`neurostrata-mcp daemon`).
*   `2` — the lock is held but nothing answers: a daemon is busy or finishing. **Do not start another** — wait, or run `neurostrata-mcp shutdown`.
*   `3` — **stale build**: the daemon answers, but it was started from an older binary than the one on disk (or predates build verification entirely). Every tool and schema you see through it belongs to that older build — fixes appear to be missing. Restart it: `neurostrata-mcp shutdown`, then `neurostrata-mcp daemon` (and restart any session holding an old stdio server). The daemon reports its build identity on `GET /info`.

**One daemon per store; every console shares it; never spawn your own.** A second daemon is refused at the lock (it exits with a message, not a crash), and `status` exists so the refusal is never a surprise.

---

### 14. `backup` / `restore`
A backup is a **checkpointed file snapshot** of the store: `CHECKPOINT`, then a copy of the store file plus a `manifest.json`. (The engine's `EXPORT DATABASE` is deliberately not used — it crashes in lbug 0.20.4's planner on every store.)
```bash
neurostrata-mcp backup <dir>
neurostrata-mcp restore <dir> --into <new-db-path>
```
*   `backup` works with or without a running daemon (with one, the daemon does the work over `/backup`).
*   `restore` refuses to overwrite an existing file: restore into a new path, check it, then point `db_path` at it.

*Example:*
```bash
neurostrata-mcp backup ~/backups/neurostrata-2026-10-08
neurostrata-mcp restore ~/backups/neurostrata-2026-10-08 --into ~/.config/NeuroStrata/data/db.restored
```

---

### 15. `doctor`
Read-only upgrade-consistency report. Runs with the daemon up. **Scoped and labeled** — `--namespace <ns>` for one project (refuses a name it does not know), omitted = every namespace in deterministic order, with each finding line carrying its `[namespace]` label. Never trust an unlabeled health line.
```bash
neurostrata-mcp doctor [--namespace <ns>]
```

---

## 🔒 Safety and Daemon Locks

The CLI binary automatically checks if the daemon is currently active on port `34343` before running any database commands. If the daemon is active, it safely exits with a helpful error message to prevent database file corruption:

```text
CRITICAL ERROR: The NeuroStrata daemon is currently running (likely via OpenCode) and holds the database lock.
You cannot run database-modifying CLI commands while the daemon is active.
Please shut down OpenCode, or kill the daemon process to run this command.
```
