# @neurostrata/codex-plugin

OpenAI Codex CLI plugin for NeuroStrata Zero-Action Start guard, file-touch recall, and subagent prompt auto-injection.

## Overview

The Codex plugin provides the same capabilities as the OpenCode plugin but adapted for the OpenAI Codex CLI. It hooks into the agent's tool execution pipeline to:

- Enforce Zero-Action Start guards before `apply_patch`/`write_file`/`write`/`edit`/`shell`/`local_shell`/`exec_command`/`bash` operations
- Prefetch governing memories when files are touched (file-touch recall)
- Auto-inject subagent context prompts for `subagent`/`Task`/`spawn_agent` operations
- Detect task intents and nudge task creation

## Installation

```bash
npm install @neurostrata/codex-plugin
# or
yarn add @neurostrata/codex-plugin
```

Then configure your Codex CLI to load this plugin.

## Configuration

The plugin reads from `~/.codex/config.toml` or accepts inline config:

```toml
# ~/.codex/config.toml
[mcp.neurostrata]
endpoint = "http://127.0.0.1:34343/mcp"

[plugins.neurostrata-codex]
namespace = "NeuroStrata"
endpoint = "http://127.0.0.1:34343/mcp"
bearerToken = null  # or path to token file
strictMode = true
requestTimeoutMs = 5000

[plugins.neurostrata-codex.intentDetection]
enabled = true
injectReminder = true
```

### Config fields

| Field | Default | Description |
|-------|---------|-------------|
| `namespace` | `"NeuroStrata"` | Memory namespace |
| `endpoint` | `"http://127.0.0.1:34343/mcp"` | MCP daemon endpoint |
| `bearerToken` | `null` | Bearer token (or use `tokenFile`) |
| `tokenFile` | `null` | Path to daemon token file |
| `strictMode` | `true` | Enforce Zero-Action Start guard validation |
| `requestTimeoutMs` | `5000` | MCP round-trip timeout |
| `intentDetection.enabled` | `true` | Detect task intent markers |
| `intentDetection.injectReminder` | `true` | Nudge to create tasks |

## Codex CLI `~/.codex/config.toml` MCP + Plugin Setup

1. Ensure the NeuroStrata daemon is running on port 34343
2. The daemon will generate a bearer token at `~/.config/neurostrata/daemon.token`
3. Configure Codex to load the plugin:

```toml
plugins = ["@neurostrata/codex-plugin"]
```

4. The plugin registers via:
   - `ctx?.tool?.hook?.("execute.before", ...)` — pre-tool hook
   - `ctx?.hooks?.beforeToolExec?.(...)` — before tool exec
   - `ctx?.hooks?.beforeToolCall?.(...)` — before tool call
   - `ctx?.hooks?.onTurnEnd?.(onTurnEnd)` — turn end hook

## Zero-Action Start Guard

The plugin enforces the Zero-Action Start rule: no `apply_patch`/`write_file`/`write`/`edit`/`shell`/`local_shell`/`exec_command`/`bash` operations are allowed unless:

- A task has been claimed via `neurostrata_task_claim`
- A note has been written via `neurostrata_task_update`
- This prevents starting work without proper task tracking

### apply_patch target file extraction

For `apply_patch` operations, the plugin extracts the target file path from the patch body using the `*** Update File: <path>` or `*** Add File: <path>` markers, enabling file-touch recall on patched files.

## File-Touch Recall (§4.5)

When `read_file`/`read`/`write_file`/`write`/`edit`/`apply_patch` touches a file, the plugin prefetches governing memories from the NeuroStrata core and injects them as context snippets. This is fail-soft — prefetch never blocks the tool, and `read` is never guard-validated.

## Subagent Prompt Auto-Injection

When `subagent`/`Task`/`spawn_agent` operations are performed, the plugin auto-injects the NeuroStrata subagent context header, including:

- Namespace and active task ID
- Mandatory Graph-First Lookup Rule (before blind `grep`/`glob` or editing files, query `neurostrata_memories_for_file`, `neurostrata_related`, `neurostrata_search_memory` (`include_related: true`), or `neurostrata_sparql_query` to inspect governing rules and AST call/import edges)

## CLI Commands

| Command | Description |
|---------|-------------|
| `codex init` | Initialize plugin in current project |
| `codex status` | Show plugin adapter status |
| `codex reset` | Reset plugin state (test hook) |

## Development

```bash
# Build
npm run build

# Watch
npm run watch

# Test
npm test
```

## License

MIT