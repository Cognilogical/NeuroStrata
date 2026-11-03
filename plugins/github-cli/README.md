# @neurostrata/github-cli-plugin

GitHub CLI (`gh`) / GitHub Copilot CLI plugin for NeuroStrata Zero-Action Start guard, file-touch recall, and subagent prompt auto-injection.

## Overview

Provides the same capabilities as the Hermes plugin adapted for the GitHub Copilot CLI / `gh` extension pipeline:

- Enforce Zero-Action Start guards before `read`/`view`/`write`/`edit`/`bash`/`shell`/`gh_exec`/`subagent`/`Task`/`agent` operations
- Prefetch governing memories when files are touched (file-touch recall)
- Auto-inject subagent context prompts for `subagent`/`Task`/`agent` operations
- Detect task intents and nudge task creation
- Register via `ctx?.tool?.hook?.("execute.before", ...)`, `ctx?.hooks?.beforeCommand?.(...)`, `ctx?.hooks?.beforeToolCall?.(...)`, `ctx?.hooks?.onTurnEnd?.(onTurnEnd)`

## Installation

```bash
gh extension install neurostrata/github-cli-plugin
# or
npm install @neurostrata/github-cli-plugin
```

## GitHub Copilot CLI / MCP & Hook Configuration

Configure your `gh` / Copilot CLI to load the plugin:

```yaml
# ~/.gh/config.yml (or Copilot CLI plugin config)
plugins:
  - name: @neurostrata/github-cli-plugin
    enabled: true
    namespace: "NeuroStrata"
    endpoint: "http://127.0.0.1:34343/mcp"
    strictMode: true
```

Hook registration points:

- `ctx?.tool?.hook?.("execute.before", ...)` — pre-tool execution
- `ctx?.hooks?.beforeCommand?.(...)` — before `gh` / Copilot CLI command
- `ctx?.hooks?.beforeToolCall?.(...)` — before tool invocation
- `ctx?.hooks?.onTurnEnd?.(onTurnEnd)` — turn end (intent detection)

## Zero-Action Start Guard

No mutating `write`/`edit`/`create`/`bash`/`shell`/`gh_exec` operations allowed unless:

- A task has been claimed (`neurostrata_task_claim`)
- A note has been written (`neurostrata_task_update`)

## File-Touch Recall (§4.5)

When `read`/`view`/`write`/`edit`/`create`/`str_replace_editor` touches a file, governing memories are prefetched from the NeuroStrata core. Fail-soft: prefetch never blocks, `read` is never guard-validated.

## Subagent Prompt Auto-Injection

When `subagent`/`Task`/`agent` operations occur, the plugin injects the Graph-First Lookup header (namespace, task ID, graph query rules) into the subagent prompt.

## CLI Commands

| Command | Description |
|---------|-------------|
| `gh neurostrata init` | Initialize plugin |
| `gh neurostrata status` | Adapter status |
| `gh neurostrata reset` | Reset state (test) |

## License

MIT
