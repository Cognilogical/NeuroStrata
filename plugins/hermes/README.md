# @neurostrata/hermes-plugin

Hermes Agent CLI plugin for NeuroStrata Zero-Action Start guard, file-touch recall, and subagent prompt auto-injection.

## Overview

The Hermes plugin provides the same capabilities as the OpenCode plugin but adapted for the Hermes Agent CLI. It hooks into the agent's tool execution pipeline to:

- Enforce Zero-Action Start guards before `read`/`write`/`edit`/`bash`/`delegate_task` operations
- Prefetch governing memories when files are touched (file-touch recall)
- Auto-inject subagent context prompts for `delegate_task`/`subagent`/`Task` operations
- Detect task intents and nudge task creation

## Installation

```bash
npm install @neurostrata/hermes-plugin
# or
yarn add @neurostrata/hermes-plugin
```

Then configure your Hermes Agent to load this plugin.

## Configuration

The plugin reads from `~/.hermes/config.yaml` or accepts inline config:

```yaml
# ~/.hermes/config.yaml
plugin:
  hermes:
    namespace: "NeuroStrata"
    endpoint: "http://127.0.0.1:34343/mcp"
    bearerToken: null  # or path to token file
    strictMode: true
    requestTimeoutMs: 5000
    intentDetection:
      enabled: true
      injectReminder: true
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

## Hermes `~/.hermes/config.yaml` / MCP + Plugin Setup

1. Ensure the NeuroStrata daemon is running on port 34343
2. The daemon will generate a bearer token at `~/.config/neurostrata/daemon.token`
3. Configure Hermes to load the plugin:

```yaml
plugins:
  - name: @neurostrata/hermes-plugin
    enabled: true
```

4. The plugin registers via:
   - `ctx?.tool?.hook?.("execute.before", ...)` — pre-tool hook
   - `ctx?.hooks?.beforeToolCall?.(...)` — before tool call
   - `ctx?.hooks?.onTurnEnd?.(onTurnEnd)` — turn end hook

## Zero-Action Start Guard

The plugin enforces the Zero-Action Start rule: no `write`/`edit`/`bash` operations are allowed unless:

- A task has been claimed via `neurostrata_task_claim`
- A note has been written via `neurostrata_task_update`
- This prevents starting work without proper task tracking

## File-Touch Recall (§4.5)

When `read`/`write`/`edit` touches a file, the plugin prefetches governing memories from the NeuroStrata core and injects them as context snippets. This is fail-soft — prefetch never blocks the tool, and `read` is never guard-validated.

## Subagent Prompt Auto-Injection

When `delegate_task`/`subagent`/`Task` operations are performed, the plugin auto-injects the NeuroStrata subagent context header, including:

- Namespace and active task ID
- Mandatory Graph-First Lookup Rule (before blind `grep`/`glob` or editing files, query `neurostrata_memories_for_file`, `neurostrata_related`, `neurostrata_search_memory` (`include_related: true`), or `neurostrata_sparql_query` to inspect governing rules and AST call/import edges)

## CLI Commands

| Command | Description |
|---------|-------------|
| `hermes init` | Initialize plugin in current project |
| `hermes status` | Show plugin adapter status |
| `hermes reset` | Reset plugin state (test hook) |

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