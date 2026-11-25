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

const HARNESS = 'codex'
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
    asString(event?.hook_event_name) ??
    asString(event?.eventName) ??
    'PreToolUse'
  )
}

function workingSetScope(projectDir, event) {
  const sessionId = asString(event?.session_id ?? event?.sessionId)
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
  return id ? `codex:${name}:${id}` : undefined
}

function contextEventId(event) {
  return canonicalEventId(
    eventName(event),
    event?.event_id ??
      event?.eventId ??
      event?.id ??
      event?.turn_id ??
      event?.turnId
  )
}

function resultEventId(event) {
  return canonicalEventId(
    'PostToolUse',
    event?.tool_use_id ??
      event?.toolUseId ??
      event?.tool_call_id ??
      event?.toolCallId ??
      event?.event_id ??
      event?.eventId ??
      event?.id
  )
}

function stopEventId(event) {
  return canonicalEventId(
    'Stop',
    event?.turn_id ??
      event?.turnId ??
      event?.event_id ??
      event?.eventId ??
      event?.id
  )
}

function textResult(value) {
  if (typeof value === 'string') {
    return { rawText: value, outputIsText: true }
  }
  if (
    value &&
    typeof value === 'object' &&
    Array.isArray(value.content) &&
    value.content.every(
      (part) => part && part.type === 'text' && typeof part.text === 'string'
    )
  ) {
    return {
      rawText: value.content.map((part) => part.text).join('\n'),
      outputIsText: true
    }
  }
  return { rawText: null, outputIsText: false }
}

function noOpReply(name) {
  if (name === 'Stop') return { continue: true }
  return { hookSpecificOutput: { hookEventName: name } }
}

async function contextReply(scope, name) {
  const output = { hookSpecificOutput: { hookEventName: name } }
  if (!scope) return output
  const cycle = await runWorkingSetContextCycle(
    scope,
    contextEventId(currentEvent)
  )
  if (cycle.changed && cycle.frame) {
    output.hookSpecificOutput.additionalContext = cycle.frame
  }
  return output
}

async function postToolUseReply(projectDir, event) {
  const name = 'PostToolUse'
  const output = { hookSpecificOutput: { hookEventName: name } }
  const scope = workingSetScope(projectDir, event)
  if (!scope) return output
  const before = await loadWorkingSet(scope)
  const result = textResult(event.tool_response)
  const cycle = await runWorkingSetToolCycle({
    scope,
    turnId: asString(event.turn_id ?? event.turnId),
    eventId: resultEventId(event),
    toolName: asString(event.tool_name ?? event.toolName) ?? '',
    toolInput: event.tool_input ?? event.toolInput ?? {},
    rawText: result.rawText,
    outputIsText: result.outputIsText,
    isError:
      event.is_error === true ||
      event.isError === true ||
      event.tool_response?.is_error === true ||
      event.tool_response?.isError === true,
    advanceTurn: false
  })
  if (cycle.changed && cycle.frame) {
    output.hookSpecificOutput.additionalContext = cycle.frame
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
  if (name === 'PreToolUse') {
    out = await handler(event)
  } else if (name === 'SessionStart' || name === 'UserPromptSubmit') {
    out = await contextReply(workingSetScope(projectDir, event), name)
  } else if (name === 'PostToolUse') {
    out = await postToolUseReply(projectDir, event)
  } else if (name === 'Stop') {
    out = await handler(event)
    const scope = workingSetScope(projectDir, event)
    if (scope) {
      await runWorkingSetTurnBoundary(
        scope,
        stopEventId(event),
        asString(event.turn_id ?? event.turnId)
      )
    }
  } else {
    out = noOpReply(name)
  }
  await writeStdout(JSON.stringify(out))
}

main().catch(async (err) => {
  const reason = String(err)
  process.stderr.write(reason + '\n')
  const name = eventName(currentEvent)
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
