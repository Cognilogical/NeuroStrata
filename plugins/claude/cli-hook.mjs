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
  runWorkingSetPromptCycle,
  runWorkingSetToolCycle,
  runWorkingSetTurnBoundary,
  usableNamespace
} from './dist/index.js'

const HARNESS = 'claude'
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url))
// The event being handled, so the catch can reply in the right hook shape.
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
  return id ? `claude:${name}:${id}` : undefined
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
  if (name === 'Stop' || name === 'SubagentStop') return { continue: true }
  return { hookSpecificOutput: { hookEventName: name } }
}

async function contextReply(scope, name, nativeId) {
  const output = { hookSpecificOutput: { hookEventName: name } }
  if (!scope) return output
  const cycle = await runWorkingSetContextCycle(
    scope,
    canonicalEventId(name, nativeId)
  )
  if (cycle.changed && cycle.frame) {
    output.hookSpecificOutput.additionalContext = [cycle.frame]
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
  if (cycle.replacementText !== null) {
    output.hookSpecificOutput.updatedToolOutput = cycle.replacementText
  }
  if (cycle.changed && cycle.frame) {
    output.hookSpecificOutput.additionalContext = [cycle.frame]
  }
  return output
}

async function main() {
  // Redirect all diagnostic logging to stderr so stdout carries only the
  // JSON hook response (critical when this runner is invoked by a CLI hook
  // whose stdout is parsed as structured output).
  console.log = console.error
  console.info = console.error
  console.warn = console.error

  // Await the write callback so a large hook response is fully flushed before
  // the runner ends. Returning avoids process.exit with an open daemon socket.
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
  const handler =
    plugin.handleHookEvent ?? plugin.handleClaudeHookEvent ?? plugin
  const name = eventName(event)
  let out
  if (name === 'PreToolUse') {
    out = await handler(event)
  } else if (name === 'SessionStart') {
    out = await contextReply(
      workingSetScope(projectDir, event),
      name,
      event.event_id ?? event.eventId ?? event.id
    )
  } else if (name === 'UserPromptSubmit') {
    const scope = workingSetScope(projectDir, event)
    const output = { hookSpecificOutput: { hookEventName: name } }
    if (scope) {
      const prompt = asString(event.prompt) ?? ''
      const cycle = await runWorkingSetPromptCycle({
        scope,
        promptText: prompt,
        turnId: asString(event.turn_id ?? event.turnId),
        eventId: canonicalEventId(
          name,
          event.event_id ?? event.eventId ?? event.id
        )
      })
      if (cycle.changed && cycle.frame) {
        output.hookSpecificOutput.additionalContext = [cycle.frame]
      }
    }
    out = output
  } else if (name === 'PostToolUse') {
    out = await postToolUseReply(projectDir, event)
  } else if (name === 'Stop') {
    await handler(event)
    const scope = workingSetScope(projectDir, event)
    if (scope) {
      const eventId = stopEventId(event)
      await runWorkingSetTurnBoundary(
        scope,
        eventId,
        asString(event.turn_id ?? event.turnId)
      )
      const cycle = await runWorkingSetContextCycle(scope, eventId)
      if (cycle.changed && cycle.frame) {
        out = {
          continue: true,
          hookSpecificOutput: {
            hookEventName: 'Stop',
            additionalContext: [cycle.frame]
          }
        }
      }
    }
    out ??= { continue: true }
  } else if (name === 'SubagentStop') {
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
