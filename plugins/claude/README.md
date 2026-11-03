# @neurostrata/claude-plugin

NeuroStrata Claude Code CLI plugin. Provides Seamless Lifecycle + Guard validation,
file-touch memory prefetch, subagent prompt injection, and Zero-Action Start
enforcement inside Claude Code hook events.

## Install

```
npm install @neurostrata/claude-plugin
```

## MCP configuration

Add the NeuroStrata daemon to your Claude Code MCP config
(`~/.config/claude/mcp.json` or project-level `.mcp.json`):

```json
{
  "mcpServers": {
    "neurostrata": {
      "command": "node",
      "args": ["node_modules/@neurostrata/claude-plugin/dist/index.js"],
      "env": {
        "NEUROSTRATA_ENDPOINT": "http://127.0.0.1:34343/mcp",
        "NEUROSTRATA_TOKEN": "<daemon bearer token>",
        "NEUROSTRATA_NAMESPACE": "NeuroStrata"
      }
    }
  }
}
```

## Hook configuration (`.claude/settings.json`)

Register the plugin on Claude Code hook events:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "",
        "command": "node /path/to/@neurostrata/claude-plugin/dist/index.js handle-claude-hook-event",
        "timeout": 30
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "command": "node /path/to/@neurostrata/claude-plugin/dist/index.js handle-claude-hook-event",
        "timeout": 30
      }
    ],
    "SubagentStop": [
      {
        "matcher": "",
        "command": "node /path/to/@neurostrata/claude-plugin/dist/index.js handle-claude-hook-event",
        "timeout": 30
      }
    ]
  }
}
```

When invoked as a command hook, Claude Code passes the hook event as JSON on
stdin. The plugin reads it, processes it, and returns JSON on stdout with
`hookSpecificOutput`.

## Behavior by hook event

### PreToolUse

- **Task / subagent** tool calls — injects the subagent context header
  (`SUBAGENT_CONTEXT_MARKER`) into `tool_input.prompt` and returns
  `{ permissionDecision: "allow", updatedInput }`.
- **Read** tool calls — runs `prefetchMemoriesForFile` on the touched file
  (fail-soft, never blocks the tool).
- **Write / Edit / MultiEdit / Bash** — runs `neurostrata_guard_validate`
  when `strictMode` is true. On rejection returns
  `{ permissionDecision: "deny", permissionDecisionReason }`; otherwise
  increments the mutation counter and allows.

### Stop / SubagentStop

Returns `{ permissionDecision: "allow" }` — a no-op acknowledge.

## Build

```
cd plugins/claude
npm install
npm run build   # tsc → dist/index.js + dist/index.d.ts
```

## Defaults

- Endpoint: `http://127.0.0.1:34343/mcp`
- Token file: `~/.config/neurostrata/daemon.token`
- Namespace: `NeuroStrata`
- strictMode: `true`
- Request timeout: 5000 ms
