# NeuroStrata OpenClaw Plugin

Client shim for OpenClaw that enforces Zero-Action Start and task-intent detection.

## Features

1. **Pre-edit Guard Validation** — Intercepts file write/edit operations, calls `neurostrata_guard_validate` before execution. Rejects the action if no task is `in_progress` (Zero-Action Start rule).

2. **Turn-Level Task-Intent Detection** — Runs lexical detector on every user/assistant turn. When task-shaped statements are detected without a corresponding `neurostrata_task_create` in the turn, injects a system reminder.

## Installation

```bash
# From NeuroStrata repo root
cp -r plugins/openclaw ~/.openclaw/plugins/neurostrata
```

Or add to your OpenClaw config:

```yaml
plugins:
  neurostrata:
    path: ./plugins/openclaw
    config:
      namespace: "NeuroStrata"
      endpoint: "http://127.0.0.1:34343/mcp"
      strictMode: true
```

## Architecture

Same as OpenCode plugin — see `../opencode/README.md` for diagram.

## Configuration

```yaml
neurostrata:
  namespace: "NeuroStrata"
  endpoint: "http://127.0.0.1:34343/mcp"
  strictMode: true
  requestTimeoutMs: 5000
  intentDetection:
    enabled: true
    injectReminder: true
```

`endpoint` (or the legacy `guardEndpoint`) is the daemon's `/mcp` route. The
daemon listens on `:34343` — the setup default — not `:8080`.

## Authentication

The daemon requires a bearer token on every route except `/health`. The shim
attaches it automatically: `bearerToken` from the plugin config when set,
otherwise the token read from `~/.config/neurostrata/daemon.token`
(override the path with `tokenFile`). A missing token is **not** an anonymous
request: the shim records the adapter as degraded and says so.

## Degraded state

`adapterStatus()` reports what the shim could actually reach: `ready` when the
core answers `tools/list` with every tool this shim needs, `degraded` when a
tool is missing, `unavailable` when the core cannot be reached. A degraded or
unavailable core fails the guard closed in strict mode.

## API

### `validateBeforeFileOp(actionType, payload, cwd)`
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