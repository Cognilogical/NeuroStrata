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
// The bundle's optional handlers, checked before an event goes to the plugin.
import * as mod from './dist/index.js'

// Events whose reply carries no permission decision (the session events and
// PostToolUse), by the lowercase name every harness spelling folds to.
const NO_DECISION_EVENTS = {
  sessionstart: 'SessionStart',
  sessionend: 'SessionEnd',
  precompact: 'PreCompact',
  posttooluse: 'PostToolUse'
}
// The working-set bundle marks itself with this constant; a stand-in bundle
// without it still gets the #425 route through the plugin's own handlers.
const hasWorkingSet = typeof mod.WORKING_SET_FRAME_OPEN === 'string'
// The event name for a failure reply, in the harness's own spelling.
let failedEventName
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

  failedEventName = [
    event.hook_event_name,
    event.hookEventName,
    event.eventName,
    process.env.NEUROSTRATA_HOOK_EVENT
  ].find((name) => typeof name === 'string' && name)
  const noDecisionEvent = failedEventName
    ? NO_DECISION_EVENTS[failedEventName.toLowerCase()]
    : undefined
  const handled = {
    SessionStart:
      HARNESS === 'claude' ||
      typeof mod.handleSessionStartEvent === 'function' ||
      hasWorkingSet,
    SessionEnd: typeof mod.writeSessionHandoff === 'function' || hasWorkingSet,
    PreCompact: typeof mod.writeSessionHandoff === 'function' || hasWorkingSet,
    PostToolUse:
      typeof mod.handlePostToolUseEvent === 'function' || hasWorkingSet
  }[noDecisionEvent]
  if (noDecisionEvent && !handled) {
    await writeStdout('{}')
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
  // A bundle without working-set helpers keeps the plugin's own handling of
  // the session events and PostToolUse.
  if (!hasWorkingSet && typeof handler === 'function') {
    const out = await handler(event)
    await writeStdout(JSON.stringify(out))
    return
  }
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
  // Reply in the shape of the event that failed; an event without a name is
  // a PreToolUse, as for the plugin. A PreToolUse-only deny is meaningless at
  // Stop: the host cannot apply it, so the turn could end without retry. At
  // Stop, Claude Code treats exit code 2 as a block; stdout still has to be
  // valid JSON. PowerShell can collapse nonzero exits, so the reason remains
  // on stderr as well.
  const failed = (failedEventName ?? 'PreToolUse').toLowerCase()
  if (failed === 'stop') {
    await writeStdout('{}')
    process.exitCode = 2
    return
  }
  // Only PreToolUse takes a permission decision. Every other event (the
  // session events, PostToolUse, each Copilot event but preToolUse) answers
  // `{}`, so the session and the tool result go on.
  if (failed !== 'pretooluse') {
    await writeStdout('{}')
    return
  }
  // Copilot CLI (its hooks file sets NEUROSTRATA_HOOK_EVENT) reads a
  // top-level decision on preToolUse and ignores hookSpecificOutput.
  if (process.env.NEUROSTRATA_HOOK_EVENT) {
    await writeStdout(
      JSON.stringify({
        permissionDecision: 'deny',
        permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`
      })
    )
    return
  }
  // PreToolUse: an explicit deny fails closed on every shell; no `continue`
  // — Codex rejects it on PreToolUse.
  await writeStdout(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'deny',
        permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`
      }
    })
  )
})
