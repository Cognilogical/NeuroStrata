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
    "endpoint": "http://127.0.0.1:34343/mcp",
    "strictMode": true,
    "requestTimeoutMs": 5000,
    "intentDetection": {
      "enabled": true,
      "injectReminder": true
    }
  }
}
```

`endpoint` (or the legacy `guardEndpoint`) is the daemon's `/mcp` route. The
daemon listens on `:34343` — the setup default — not `:8080`.

## Authentication

The daemon requires a bearer token on every route except `/health`. The shim
attaches it automatically:

- `bearerToken` in the plugin config wins when it is set.
- Otherwise the token is read from `~/.config/neurostrata/daemon.token`
  (override the path with `tokenFile`). The daemon writes that file with
  mode `0600` on first start.
- A missing token is **not** an anonymous request: the shim records the
  adapter as degraded and says so.

## Degraded state

`adapterStatus()` reports what the shim could actually reach:

| state | meaning |
|---|---|
| `uninitialized` | `initialize()` has not run |
| `ready` | the core answered `tools/list` and exposes every tool this shim needs |
| `degraded` | the core is reachable but does not expose a tool this shim needs (`missingTools`) |
| `unavailable` | the core could not be reached, or the guard could not answer |

A degraded or unavailable core fails the guard closed in strict mode: the
write is refused with the reason, not waved through.

## API

### `validateBeforeWriteEdit(actionType, payload, cwd)`
Calls `neurostrata_guard_validate` with the action over `tools/call`, unwraps
`result.content[].text` and reads the verdict from its `bucket`. Returns
`{ allowed: boolean, reason?: string, verdict }`.

### `detectIntentsAndNudge(text, turnHasTaskCreate)`
Runs the lexical detector on the text. If signals are found and
`turnHasTaskCreate` is false, returns a nudge message to inject. The core
exposes no intent-detection tool — the earlier `neurostrata_detect_task_intents`
call was a phantom the core answered with "Unknown tool" — so the detector
runs in the shim and nothing is written.

## Dependencies

- NeuroStrata daemon running on `:34343` (provides `neurostrata_guard_validate` and `neurostrata_task_create`)
- Rust core: `src/task/intent.rs::detect_task_intents` (mirrored locally in the shim)
- M4 Honest Guard: `src/guard/validator.rs::GuardValidator::validate`

## Honest Advertising

This shim **advertises blocking wires honestly**:
- It does NOT silently swallow rejections — the Zero-Action Start rejection surfaces to the user.
- It does NOT fake task creation — the nudge is advisory only; the user must still call `neurostrata_task_create`.
- It reports `guard_validate` verdict verbatim (pass/reject + reasons + constraints).