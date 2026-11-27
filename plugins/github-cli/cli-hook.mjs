#!/usr/bin/env node
import { createHash } from 'node:crypto'
import { readFileSync, realpathSync } from 'node:fs'
import { dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import plugin, {
  initialize,
  readHarnessConfig,
  resolveHookProjectDir,
  usableNamespace
} from './dist/index.js'

const HARNESS = 'github-cli'
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
let currentEvent = null

const writeStdout = (text) =>
  new Promise((resolve) => {
    process.stdout.write(text, () => resolve())
  })

function asString(value) {
  return typeof value === 'string' && value.length > 0 ? value : undefined
}

function eventName(event) {
  return (
    asString(process.env.NEUROSTRATA_HOOK_EVENT) ??
    asString(event?.hook_event_name) ??
    asString(event?.eventName) ??
    'PreToolUse'
  )
}

function workingSetScope(projectDir, event) {
  const sessionId = asString(event?.sessionId ?? event?.session_id)
  if (!sessionId) return null
  try {
    const real = realpathSync.native(projectDir).replace(/\\/g, '/')
    const projectRoot = real.length > 1 ? real.replace(/\/+$/u, '') : real
    return {
      projectRoot,
      projectId: createHash('sha256').update(projectRoot, 'utf8').digest('hex'),
      namespace: mod.adapterStatus().namespace,
      sessionId,
      epoch: mod.currentSessionEpoch(sessionId)
    }
  } catch {
    return null
  }
}

function canonicalEventId(name, nativeId) {
  const id = asString(nativeId)
  return id ? `github-cli:${name}:${id}` : undefined
}

function contextEventId(event) {
  return canonicalEventId(
    eventName(event),
    event?.eventId ?? event?.event_id ?? event?.id
  )
}

function resultEventId(event) {
  return canonicalEventId(
    'postToolUse',
    event?.toolCallId ??
      event?.tool_call_id ??
      event?.toolUseId ??
      event?.tool_use_id ??
      event?.eventId ??
      event?.event_id ??
      event?.id
  )
}

function stopEventId(event) {
  return canonicalEventId(
    'agentStop',
    event?.turnId ??
      event?.turn_id ??
      event?.eventId ??
      event?.event_id ??
      event?.id
  )
}

function noOpReply(name) {
  if (name === 'Stop') return { continue: true }
  return {}
}

function normalizedPreToolUse(event) {
  return {
    ...event,
    hook_event_name: 'PreToolUse',
    session_id: event.sessionId ?? event.session_id,
    tool_name: event.toolName ?? event.tool_name,
    tool_input: event.toolArgs ?? event.tool_args ?? event.tool_input ?? {}
  }
}

function mapPreToolUseReply(reply) {
  const hook = reply?.hookSpecificOutput ?? {}
  return {
    permissionDecision: hook.permissionDecision,
    ...(hook.permissionDecisionReason
      ? { permissionDecisionReason: hook.permissionDecisionReason }
      : {}),
    ...(hook.updatedInput !== undefined
      ? { modifiedArgs: hook.updatedInput }
      : {})
  }
}

async function sessionStartReply(projectDir, event) {
  const output = {}
  const scope = workingSetScope(projectDir, event)
  if (!scope) return output
  const cycle = await mod.runWorkingSetContextCycle(
    scope,
    contextEventId(event)
  )
  if (cycle.changed && cycle.frame) output.additionalContext = cycle.frame
  return output
}

async function postToolUseReply(projectDir, event) {
  const output = {}
  const scope = workingSetScope(projectDir, event)
  if (!scope) return output
  const before = await mod.loadWorkingSet(scope)
  const toolResult = event.toolResult ?? {}
  const successful = toolResult.resultType === 'success'
  const rawText = successful ? asString(toolResult.textResultForLlm) : undefined
  const cycle = await mod.runWorkingSetToolCycle({
    scope,
    turnId: asString(event.turnId ?? event.turn_id),
    eventId: resultEventId(event),
    toolName: asString(event.toolName ?? event.tool_name) ?? '',
    toolInput: event.toolArgs ?? event.tool_args ?? event.tool_input ?? {},
    rawText: rawText ?? null,
    outputIsText: rawText !== undefined,
    isError: !successful,
    advanceTurn: false
  })
  if (successful && cycle.replacementText !== null) {
    output.modifiedResult = {
      resultType: 'success',
      textResultForLlm: cycle.replacementText
    }
  }
  if (cycle.changed && cycle.frame) {
    output.additionalContext = cycle.frame
  }
  return output
}

async function main() {
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
  const projectDir = resolveHookProjectDir(HARNESS, event)
  const namespace = usableNamespace(process.env.NEUROSTRATA_NAMESPACE)
  const envConfig = namespace ? { namespace } : {}
  await initialize({
    config: {
      ...envConfig,
      ...readHarnessConfig(HARNESS, projectDir, RUNNER_DIR)
    },
    projectDir
  })
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
  let out
  if (name === 'preToolUse') {
    out = mapPreToolUseReply(await handler(normalizedPreToolUse(event)))
  } else if (name === 'PreToolUse') {
    out = await handler(event)
  } else if (name === 'sessionStart') {
    out = await sessionStartReply(projectDir, event)
  } else if (name === 'postToolUse') {
    out = await postToolUseReply(projectDir, event)
  } else if (name === 'agentStop') {
    await handler({ ...event, hook_event_name: 'Stop' })
    const scope = workingSetScope(projectDir, event)
    if (scope) {
      await mod.runWorkingSetTurnBoundary(
        scope,
        stopEventId(event),
        asString(event.turnId ?? event.turn_id)
      )
    }
    out = {}
  } else if (name === 'Stop') {
    out = await handler(event)
  } else {
    out = noOpReply(name)
  }
  await writeStdout(JSON.stringify(out))
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
