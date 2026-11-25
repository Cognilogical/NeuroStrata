#!/usr/bin/env node
import { createHash } from 'node:crypto'
import { readFileSync, realpathSync } from 'node:fs'
import { dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import plugin, {
  adapterStatus,
  currentSessionEpoch,
  initialize,
  loadWorkingSet,
  readHarnessConfig,
  resolveHookProjectDir,
  runWorkingSetContextCycle,
  runWorkingSetToolCycle,
  runWorkingSetTurnBoundary,
  usableNamespace
} from './dist/index.js'

const HARNESS = 'github-cli'
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url))
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
      namespace: adapterStatus().namespace,
      sessionId,
      epoch: currentSessionEpoch(sessionId)
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
  const cycle = await runWorkingSetContextCycle(scope, contextEventId(event))
  if (cycle.changed && cycle.frame) output.additionalContext = cycle.frame
  return output
}

async function postToolUseReply(projectDir, event) {
  const output = {}
  const scope = workingSetScope(projectDir, event)
  if (!scope) return output
  const before = await loadWorkingSet(scope)
  const toolResult = event.toolResult ?? {}
  const successful = toolResult.resultType === 'success'
  const rawText = successful ? asString(toolResult.textResultForLlm) : undefined
  const cycle = await runWorkingSetToolCycle({
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
      await runWorkingSetTurnBoundary(
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
  const name = eventName(currentEvent)
  let out
  if (name === 'preToolUse') {
    out = {
      permissionDecision: 'deny',
      permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`
    }
  } else if (name === 'PreToolUse') {
    out = {
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'deny',
        permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`
      }
    }
  } else {
    out = noOpReply(name)
  }
  await writeStdout(JSON.stringify(out))
})
