# 💻 NeuroStrata CLI Reference (`neurostrata-mcp`)

NeuroStrata provides a standalone compiled CLI binary (`neurostrata-mcp`) alongside its HTTP/stdio MCP daemon mode for direct administration, graph inspection, task-gate verification, team mesh synchronization, and local model lifecycle management.

> [!WARNING]
> **Single-Writer Database Lock:** LadybugDB enforces single-process write access. Commands that mutate the store directly on disk (`delete`, `move`, `add`, `edit`, `archive`) require the daemon to be stopped (`neurostrata-mcp shutdown`), whereas daemon-proxied commands (`status`, `doctor`, `bus-metrics`, `backup`, `ingest`, `export-graph`, `task import`) communicate with a live daemon automatically when one is running.

---

## 1. Core Daemon & Setup Commands

### `daemon`
Starts the NeuroStrata MCP server (HTTP loopback on `127.0.0.1:34343` and/or stdio transport). Only one daemon instance may hold the store lock at a time.
```bash
neurostrata-mcp daemon
```

### `status`
Read-only preflight check reporting store health, lock holder, and whether the running daemon matches the on-disk binary build.
```bash
neurostrata-mcp status
```
* **Exit `0`:** Healthy daemon running the current build.
* **Exit `1`:** No daemon running; lock is free.
* **Exit `2`:** Lock held but daemon is busy or shutting down.
* **Exit `3`:** Stale daemon build running; restart with `neurostrata-mcp shutdown && neurostrata-mcp daemon`.

### `shutdown`
Gracefully checkpoints the database and stops the running daemon.
```bash
neurostrata-mcp shutdown
```

### `setup`
Installs or refreshes the user-level background service (`systemd` on Linux, `launchd` on macOS) and initializes default configuration under `~/.config/neurostrata/`.
```bash
neurostrata-mcp setup
```

---

## 2. Namespace & Memory Management

### `namespaces`
Lists all initialized memory namespaces in the database.
```bash
neurostrata-mcp namespaces
```

### `list`
Prints all active memory nodes stored in a namespace.
```bash
neurostrata-mcp list <namespace>
```

### `add`
Directly embeds and stores a new memory node in a namespace.
```bash
neurostrata-mcp add <namespace> <type> <content> [location]
```
*Example:*
```bash
neurostrata-mcp add my-rust-project rule "Avoid unwrap() in library modules" "src/lib.rs"
```

### `edit`
Modifies an existing memory node's namespace, content, and location anchor.
```bash
neurostrata-mcp edit <namespace> <id> <new_namespace> <content> <location>
```

### `archive`
Tombstones a memory by ID (`metadata.archived: true`) without deleting its historical audit record. Refuses `global` without `--allow-global`.
```bash
neurostrata-mcp archive <namespace> <id> [--allow-global]
```

### `move`
Copies a memory node into a target namespace and removes the original row. Used to consolidate split namespace spellings reported by `doctor`.
```bash
neurostrata-mcp move <source_namespace> <id> <target_namespace>
```

### `delete`
Permanently deletes a single memory node by ID. Deliberately restricted to the CLI so an autonomous agent cannot invoke it via MCP.
```bash
neurostrata-mcp delete <namespace> <id>
```

---

## 3. Codebase Ingestion & Graph Export

### `ingest`
Scans a source directory with the six-language Tree-sitter AST engine (`rust`, `typescript`, `tsx`, `python`, `go`, `java`), extracts structural symbols (`function`, `class`, `struct`, `interface`, `trait`, `method`, `calls`, `imports`), and indexes them into the namespace.
```bash
neurostrata-mcp ingest <dir_path> <namespace> [schema_path]
```
*Example:*
```bash
neurostrata-mcp ingest ./src my-rust-project
```

### `project-context` & `export-graph`
Binds stable source identifiers to `.NeuroStrata` project markers and exports the relational memory graph as JSON for the Web UI and Obsidian canvas.
```bash
neurostrata-mcp project-context bind <source-id> <marker-path>
neurostrata-mcp project-context list
neurostrata-mcp project-context unbind <source-id>

neurostrata-mcp export-graph [output_json_path] [--source-id <id>] [--exclude-superseded] [--include-archived]
```
* Enforces the **Export-Freshness Gate**: refuses when a namespace has unexported mutations marked `dirty: true` until reconciled.
* Excludes tombstoned (`archived: true`) rows by default unless `--include-archived` is passed.

---

## 4. Central Executive Task Gates & Hooks

### `task gate`
Runs the Central Executive enforcement check over a namespace. Executed automatically by `.git/hooks/pre-push`.
```bash
neurostrata-mcp task gate <namespace> [--strict] [--self-test]
```
* **Exit `0`:** Clean; zero blocking violations.
* **Exit `1`:** Blocking violation detected (`in_progress` task, `done` task without an `EXTRACTED_FROM` memory, or `P0` task open > 24h).
* **Exit `2`:** Infrastructure error (blocks when `--strict` is set).
* `--self-test`: Verifies the gate engine against an in-memory planted-violation fixture without touching the database.

### `task validate`
Advisory twin of `task gate`. Prints the full JSON/human readiness report (`violations`, `stale`, `unextracted_done`, `dangling_edges`, `counts`) and always exits `0`.
```bash
neurostrata-mcp task validate <namespace>
```

### `task import`
Idempotently imports legacy JSONL task records into the Central Executive store.
```bash
neurostrata-mcp task import <namespace> --from-beads <path>
```

### `hooks install`
Installs `.git/hooks/pre-push` (strict task gate) and `.git/hooks/pre-commit` (graph-query ledger) plus detected agent harness plugins.
```bash
neurostrata-mcp hooks install [--force]
```

---

## 5. SynapticMesh (Git Team Memory Sync)

Synchronizes shareable `rule`, `fact`, and `procedure` definitions across teammates over standard Git (`--ff-only` pull and push) while preserving local private sovereignty.
```bash
neurostrata-mcp synaptic-mesh init <mesh_name> [--namespace <ns>] [--remote <url>]
neurostrata-mcp synaptic-mesh join <mesh_repo_path>
neurostrata-mcp synaptic-mesh push
neurostrata-mcp synaptic-mesh pull
neurostrata-mcp synaptic-mesh status
neurostrata-mcp synaptic-mesh leave
neurostrata-mcp synaptic-mesh recover
```

---

## 6. Local Learning & Laya

### `learn`
Manages the local deduplication learning loop: datasets, candidate training, evaluation gates, promotion, and rollback.
```bash
neurostrata-mcp learn status
neurostrata-mcp learn pause
neurostrata-mcp learn resume
neurostrata-mcp learn train
neurostrata-mcp learn rollback
neurostrata-mcp learn reset
```

### `laya`
Fetches a pinned, checksum-verified Laya ONNX duplicate-judgment bundle into `artifacts/laya-bundle` by default.
```bash
neurostrata-mcp laya [MODEL] [--out-dir <dir>]
```

### GLiNER (no CLI subcommand)
GLiNER entity/relation extraction runs **inside the daemon**, not as a CLI subcommand: `neurostrata_add_memory` with `extract_entities: true` runs the local ONNX GLiNER engine (`src/gliner.rs`) and stores the extracted entities as `metadata.gliner_candidates` on the memory, each marked `review_only`. There is no `neurostrata-mcp gliner` command.

---

## 7. Diagnostics, Telemetry & Backups

### `doctor`
Read-only upgrade and schema consistency audit across one or all namespaces, with every finding labeled by `[namespace]`.
```bash
neurostrata-mcp doctor [--namespace <ns>]
```

### `bus-metrics`
Queries the Thalamic Bus event dispatcher counters (`published`, `delivered`, `dropped`, `queue_depth`) over HTTP.
```bash
neurostrata-mcp bus-metrics
```

### `backup` & `restore`
Creates a checkpointed snapshot (`CHECKPOINT` + store copy + `manifest.json`) or restores a snapshot into a new database path.
```bash
neurostrata-mcp backup <backup_dir>
neurostrata-mcp restore <backup_dir> --into <new_db_path>
```
