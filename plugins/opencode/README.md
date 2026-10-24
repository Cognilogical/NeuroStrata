# NeuroStrata OpenCode Plugin

Client shim for OpenCode that enforces Zero-Action Start and task-intent detection.

## Features

1. **Pre-edit Guard Validation** — Intercepts `write` and `edit` tool calls, calls `neurostrata_guard_validate` before execution. Rejects the action if no task is `in_progress` (Zero-Action Start rule).

2. **Turn-Level Task-Intent Detection** — Runs lexical detector (`src/task/intent.rs::detect_task_intents`) on every user/assistant turn. When task-shaped statements are detected without a corresponding `neurostrata_task_create` in the turn, injects a system reminder.

## Installation

```bash
# From NeuroStrata repo root
cp -r plugins/opencode ~/.opencode/plugins/neurostrata
```

Or add to your OpenCode config:

```json
{
  "plugins": {
    "neurostrata": "./plugins/opencode"
  }
}
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      OpenCode Host                          │
├─────────────────────────────────────────────────────────────┤
│  Tool Call: write/edit ──────► Shim Interceptor             │
│                                 │                           │
│                                 ▼                           │
│                    neurostrata_guard_validate               │
│                                 │                           │
│                    ┌────────────┴────────────┐              │
│                    ▼                         ▼              │
│              PASS: execute              REJECT: surface     │
│              tool                        zero-action error  │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                      Conversation Turn                      │
├─────────────────────────────────────────────────────────────┤
│  User/Assistant text ──────► detect_task_intents()          │
│                                 │                           │
│                    ┌────────────┴────────────┐              │
│                    ▼                         ▼              │
│           Signals found              No signals /           │
│           & no task_create             task_create          │
│                    │                         present         │
│                    ▼                         │               │
│           Inject system              Continue               │
│           reminder                                                          │
└─────────────────────────────────────────────────────────────┘
```

## Configuration

```json
{
  "neurostrata": {
    "namespace": "NeuroStrata",
    "guardEndpoint": "http://localhost:8080/mcp",
    "strictMode": true,
    "intentDetection": {
      "enabled": true,
      "injectReminder": true
    }
  }
}
```

## API

### `validateBeforeWriteEdit(actionType, payload, cwd)`
Calls `neurostrata_guard_validate` with the action. Returns `{ allowed: boolean, reason?: string, verdict }`.

### `detectIntentsAndNudge(text, turnHasTaskCreate)`
Runs `detect_task_intents` on the text. If signals found and `turnHasTaskCreate` is false, returns a nudge message to inject.

## Dependencies

- NeuroStrata MCP server running (provides `neurostrata_guard_validate` and `neurostrata_task_create`)
- Rust core: `src/task/intent.rs::detect_task_intents` (compiled into MCP tools)
- M4 Honest Guard: `src/guard/validator.rs::GuardValidator::validate`

## Honest Advertising

This shim **advertises blocking wires honestly**:
- It does NOT silently swallow rejections — the Zero-Action Start rejection surfaces to the user.
- It does NOT fake task creation — the nudge is advisory only; the user must still call `neurostrata_task_create`.
- It reports `guard_validate` verdict verbatim (pass/reject + reasons + constraints).