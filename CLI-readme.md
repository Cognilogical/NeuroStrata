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

It is deliberately a CLI command and not an MCP tool. It copies the row and then deletes the
original, so it destroys something, and every destructive operation in NeuroStrata requires a
person at the keyboard rather than an agent asking permission.

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

## 🔒 Safety and Daemon Locks

The CLI binary automatically checks if the daemon is currently active on port `34343` before running any database commands. If the daemon is active, it safely exits with a helpful error message to prevent database file corruption:

```text
CRITICAL ERROR: The NeuroStrata daemon is currently running (likely via OpenCode) and holds the database lock.
You cannot run database-modifying CLI commands while the daemon is active.
Please shut down OpenCode, or kill the daemon process to run this command.
```
