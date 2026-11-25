#!/usr/bin/env node
import { readFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import plugin, {
  initialize,
  onTurnEnd,
  readHarnessConfig,
  resolveHookProjectDir,
  usableNamespace
} from './dist/index.js'

const HARNESS = 'opencode'
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url))
// The event being handled, so the catch can reply in the right hook shape.
let currentEvent = null

// Await the write callback so large responses flush before the runner ends.
const writeStdout = (text) =>
  new Promise((resolve) => {
    process.stdout.write(text, () => resolve())
  })

function eventName(event) {
  return event?.hook_event_name ?? event?.eventName ?? 'PreToolUse'
}

function noOpReply(name) {
  if (name === 'Stop' || name === 'SubagentStop') return { continue: true }
  return { hookSpecificOutput: { hookEventName: name } }
}

async function main() {
  // Redirect all diagnostic logging to stderr so stdout carries only the
  // JSON hook response (critical when this runner is invoked by a CLI hook
  // whose stdout is parsed as structured output).
  console.log = console.error
  console.info = console.error
  console.warn = console.error

  let raw = ''
  try {
    raw = readFileSync(0, 'utf8')
  } catch {
    return
  }
  if (!raw.trim()) return
  let event
  try {
    event = JSON.parse(raw)
  } catch {
    return
  }
  currentEvent = event
  // The namespace comes from the harness config the installer wrote; the
  // plugin reads NEUROSTRATA_NAMESPACE itself, after the project marker and
  // this config (the order of `neurostrata-mcp namespace resolve`).
  const projectDir = resolveHookProjectDir(HARNESS, event)
  // Pass the namespace only when it is set: an explicit undefined would
  // override the one in .NeuroStrata/neurostrata-plugin.json.
  const namespace = usableNamespace(process.env.NEUROSTRATA_NAMESPACE)
  const envConfig = namespace ? { namespace } : {}
  await initialize({
    config: {
      ...envConfig,
      ...readHarnessConfig(HARNESS, projectDir, RUNNER_DIR)
    },
    projectDir
  })
  // Handler chain: execute.before-style handleHookEvent takes precedence, then
  // the Claude handleClaudeHookEvent alias, falling back to the default export.
  const handler =
    plugin.handleHookEvent ?? plugin.handleClaudeHookEvent ?? plugin
  const name = eventName(event)
  if (['PreToolUse', 'Stop', 'SubagentStop'].includes(name)) {
    if (typeof handler === 'function') {
      const out = await handler(event)
      await writeStdout(JSON.stringify(out))
      return
    }
    if (name === 'Stop' && typeof event.transcript === 'string') {
      await onTurnEnd({ role: 'assistant', content: event.transcript })
    }
    await writeStdout(JSON.stringify({ continue: true }))
    return
  }
  await writeStdout(JSON.stringify(noOpReply(name)))
}
main().catch(async (err) => {
  const reason = String(err)
  process.stderr.write(reason + '\n')
  const name = eventName(currentEvent)
  // Only pre-tool failures fail closed. Every other event receives a valid,
  // neutral success response and never inherits PreToolUse semantics.
  const out =
    name === 'PreToolUse'
      ? {
          hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: 'deny',
            permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`
          }
        }
      : noOpReply(name)
  await writeStdout(JSON.stringify(out))
})
