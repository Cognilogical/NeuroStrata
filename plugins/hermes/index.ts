import { createHash as hermesCreateHash } from 'node:crypto'
import * as hermesFs from 'node:fs'
import * as hermesPath from 'node:path'
import {
  NeuroStrataMCPClient,
  addSessionWorktree,
  currentSessionEpoch,
  detectTaskIntentsLocal,
  discoverProjectNamespace,
  fetchContextPlan,
  injectSubagentPrompt,
  initialize,
  loadDaemonToken,
  markWorkingSetTouches,
  parseGuardValidate,
  prefetchMemoriesForFile,
  resetState,
  resolveConfig,
  resolveHookProjectDir,
  resolveProjectRoot,
  runWorkingSetTurnBoundary,
  upsertWorkingSetFrame,
  usableNamespace,
  type GuardValidateResult,
  type WorkingSetScope,
  type WorkingSetTouch
} from '@neurostrata/plugin-core'

export * from '@neurostrata/plugin-core'

export interface HermesPluginContext {
  projectDir?: string
  config?: Record<string, unknown>
  pluginConfig?: Record<string, unknown>
  tool?: {
    hook?: (name: string, handler: (input: any) => Promise<any>) => unknown
  }
  hooks?: {
    beforeToolCall?: (handler: (input: any) => Promise<any>) => unknown
    onTurnEnd?: (handler: HermesTurnEndHandler) => unknown
  }
  logger?: {
    info?: (message: string, detail?: unknown) => void
    warn?: (message: string, detail?: unknown) => void
    error?: (message: string, detail?: unknown) => void
  }
}

export interface HermesTurn {
  role: 'user' | 'assistant'
  content: string
  turnId?: string
  sessionId?: string
}

export type HermesTurnEndHandler = (turn: HermesTurn) => Promise<void>

function createHermesShim() {
  const createHash = hermesCreateHash
  const { lstatSync, readFileSync, realpathSync } = hermesFs
  const { join, relative, resolve } = hermesPath

  type HermesConfig = ReturnType<typeof resolveConfig>
  type HermesGuard =
    | { verdict: 'pass' }
    | { verdict: 'reject'; reason: string; log: Record<string, unknown> }
    | { verdict: 'unavailable'; message: string }

  let config: HermesConfig = resolveConfig({})
  let mcpClient: NeuroStrataMCPClient | null = null
  let pluginCtx: HermesPluginContext | null = null
  let sessionProjectDir: string | undefined
  let activeSessionId: string | null = null
  let activeTaskId: string | null = null
  let mutationsSinceNote = 0
  let executeRegistered = false

  const HERMES_FILE_TOOLS = [
    'read_file',
    'read',
    'write_file',
    'write',
    'patch_file',
    'edit'
  ]
  const HERMES_SUBAGENT_TOOLS = [
    'delegate_task',
    'subagent',
    'Task',
    'task',
    'dispatch_agent'
  ]
  const HERMES_MUTATING_TOOLS = [
    'write_file',
    'write',
    'patch_file',
    'edit',
    'terminal',
    'bash'
  ]

  function asString(value: unknown): string | undefined {
    return typeof value === 'string' && value.length > 0 ? value : undefined
  }

  function asRecord(value: unknown): Record<string, unknown> {
    return value !== null && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {}
  }

  function firstRecord(...candidates: unknown[]): Record<string, unknown> {
    for (const candidate of candidates) {
      const record = asRecord(candidate)
      if (Object.keys(record).length > 0) return record
    }
    return {}
  }

  function canonicalRoot(root: string): string | null {
    try {
      const real = realpathSync.native(root).replace(/\\/g, '/')
      return real.length > 1 ? real.replace(/\/+$/u, '') : real
    } catch {
      return null
    }
  }

  function parentSessionId(root: string): string | null {
    try {
      const stateDir = join(root, '.NeuroStrata')
      const stateStat = lstatSync(stateDir)
      if (!stateStat.isDirectory() || stateStat.isSymbolicLink()) return null
      const path = join(stateDir, 'active_session.json')
      const stat = lstatSync(path)
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 1048576)
        return null
      return (
        asString(asRecord(JSON.parse(readFileSync(path, 'utf8'))).session_id) ??
        null
      )
    } catch {
      return null
    }
  }

  function workingSetScope(
    root: string,
    namespace: string,
    sessionId?: string | null
  ): WorkingSetScope | null {
    const canonical = canonicalRoot(root)
    if (!canonical) return null
    const stableSession =
      asString(sessionId) ?? parentSessionId(canonical) ?? activeSessionId
    const ns = usableNamespace(namespace) ?? discoverProjectNamespace(canonical)
    if (!stableSession || !ns) return null
    return {
      projectRoot: canonical,
      projectId: createHash('sha256').update(canonical, 'utf8').digest('hex'),
      namespace: ns,
      sessionId: stableSession,
      epoch: currentSessionEpoch(stableSession)
    }
  }

  function relativePath(root: string, path: string): string | null {
    const trimmed = path.trim()
    if (!trimmed) return null
    const rel = relative(resolve(root), resolve(root, trimmed)).replace(
      /\\/g,
      '/'
    )
    if (!rel || rel === '..' || rel.startsWith('../')) return null
    return rel
  }

  function eventId(eventName: string, nativeId: unknown): string | undefined {
    const id = asString(nativeId)
    return id ? `hermes:${eventName}:${id}` : undefined
  }

  function toolName(input: any): string {
    return (
      asString(input?.tool) ??
      asString(input?.name) ??
      asString(input?.action) ??
      asString(input?.command) ??
      ''
    )
  }

  async function markTool(input: any): Promise<void> {
    const name = toolName(input)
    if (!HERMES_FILE_TOOLS.includes(name)) return
    const args = firstRecord(input?.input, input?.args)
    const path =
      asString(args.filePath) ??
      asString(args.path) ??
      asString(input?.filePath) ??
      asString(input?.path)
    if (!path) return
    const normalized = relativePath(sessionProjectDir ?? process.cwd(), path)
    const scope = workingSetScope(
      sessionProjectDir ?? process.cwd(),
      config.namespace,
      asString(input?.session_id) ??
        asString(input?.sessionId) ??
        asString(input?.sessionID) ??
        activeSessionId
    )
    if (!normalized || !scope) return
    const touch: WorkingSetTouch = {
      id: `file:${normalized}`,
      kind: 'file',
      path: normalized
    }
    try {
      await markWorkingSetTouches(
        scope,
        [touch],
        eventId(
          'beforeToolCall',
          input?.tool_use_id ?? input?.toolUseId ?? input?.id
        )
      )
    } catch {
      // GC observation never blocks a Hermes tool call.
    }
  }

  function resetHermesState(): void {
    resetState()
    activeSessionId = null
    activeTaskId = null
    mutationsSinceNote = 0
  }

  async function injectSubagent(input: any): Promise<void> {
    const name = toolName(input)
    if (!HERMES_SUBAGENT_TOOLS.includes(name)) return
    for (const key of ['input', 'args']) {
      const target = asRecord(input?.[key])
      if (Object.keys(target).length > 0) {
        injectSubagentPrompt(target, config.namespace, activeTaskId)
      }
    }
  }

  async function hermesInitialize(ctx: HermesPluginContext): Promise<void> {
    resetHermesState()
    pluginCtx = ctx
    sessionProjectDir = resolveProjectRoot(ctx)
    config = resolveConfig(
      ctx?.config ?? ctx?.pluginConfig ?? ctx,
      sessionProjectDir
    )
    const token = config.bearerToken ?? loadDaemonToken(config.tokenFile)
    mcpClient = new NeuroStrataMCPClient({
      endpoint: config.endpoint,
      token,
      timeoutMs: config.requestTimeoutMs
    })

    await initialize({
      ...ctx,
      projectDir: sessionProjectDir,
      config,
      hooks: undefined,
      tool: undefined,
      session: undefined
    })

    executeRegistered = typeof ctx?.tool?.hook === 'function'
    if (executeRegistered) await ctx.tool!.hook!('execute.before', beforeTool)
    await ctx?.hooks?.beforeToolCall?.(async (input: any) => {
      if (executeRegistered) {
        await markTool(input)
        await injectSubagent(input)
        return
      }
      await beforeTool(input)
    })
    await ctx?.hooks?.onTurnEnd?.(onTurnEnd)

    console.info('[NeuroStrata] Hermes plugin initialized', {
      namespace: config.namespace,
      endpoint: config.endpoint
    })
  }

  async function ensureInitialized(
    event?: Record<string, unknown>
  ): Promise<void> {
    if (mcpClient) return
    await hermesInitialize({
      projectDir: resolveHookProjectDir('hermes', event),
      config: {}
    })
  }

  function callState(call: {
    args: Record<string, unknown>
    session_id?: string
  }): {
    session_id: string
    active_task_id: string | null
    worktree: string
  } {
    const args = call.args
    return {
      session_id:
        asString(call.session_id) ??
        asString(args.session_id) ??
        activeSessionId ??
        'unknown',
      active_task_id: asString(args.active_task_id) ?? activeTaskId,
      worktree: asString(args.worktree) ?? process.cwd()
    }
  }

  async function runMutatingToolGuard(call: {
    name: string
    args: Record<string, unknown>
    session_id?: string
  }): Promise<HermesGuard> {
    if (!config.strictMode) {
      mutationsSinceNote++
      return { verdict: 'pass' }
    }
    const actionType = call.name
    const payload = JSON.stringify(call.args)
    const cwd = process.cwd()
    const { session_id, active_task_id, worktree } = callState(call)
    try {
      if (!mcpClient) throw new Error('NeuroStrata MCP client not initialized')
      const text = await mcpClient.callTool('neurostrata_guard_validate', {
        namespace: config.namespace,
        session_id,
        active_task_id,
        worktree,
        action_type: actionType,
        payload,
        cwd,
        is_mutating: true,
        use_sandbox: false,
        mutations_since_note: mutationsSinceNote
      })
      const result = parseGuardValidate(text)
      if (result.verdict === 'reject') {
        const reason =
          result.reasons.join('; ') ||
          result.constraints.join('; ') ||
          result.bucket
        return {
          verdict: 'reject',
          reason,
          log: {
            actionType,
            session_id,
            active_task_id,
            worktree,
            reason,
            rule_ids: result.rule_ids
          }
        }
      }
    } catch (error) {
      const message = String(error)
      if (
        message.includes('Zero-Action Start violation:') ||
        message.includes('Guard rejected')
      ) {
        return {
          verdict: 'reject',
          reason: message,
          log: { actionType, error: message }
        }
      }
      return { verdict: 'unavailable', message }
    }
    mutationsSinceNote++
    return { verdict: 'pass' }
  }

  async function beforeTool(input: any): Promise<void> {
    const name = toolName(input)
    const args = firstRecord(input?.input, input?.args)
    addSessionWorktree(name, args, sessionProjectDir)
    await markTool(input)
    await injectSubagent(input)

    if (HERMES_FILE_TOOLS.includes(name)) {
      const filePath =
        asString(args.filePath) ??
        asString(args.path) ??
        asString(input?.filePath) ??
        asString(input?.path)
      if (filePath) {
        try {
          const snippets = await prefetchMemoriesForFile(
            config.namespace,
            filePath,
            { projectRoot: sessionProjectDir }
          )
          if (snippets.length > 0) {
            console.info(
              `[NeuroStrata] File-touch recall for ${filePath}\n${snippets.join('\n')}`
            )
          }
        } catch (error) {
          console.warn('[NeuroStrata] File-touch prefetch failed', {
            file: filePath,
            error: String(error)
          })
        }
      }
    }

    if (
      [
        'neurostrata_task_claim',
        'neurostrata_task_update',
        'neurostrata_task_complete'
      ].includes(name)
    ) {
      const sessionId =
        asString(args.session_id) ??
        asString(input?.session_id) ??
        asString(input?.sessionID)
      if (sessionId) activeSessionId = sessionId
      const taskId = asString(args.id)
      if (
        name === 'neurostrata_task_claim' ||
        name === 'neurostrata_task_complete' ||
        args.note !== undefined
      ) {
        mutationsSinceNote = 0
      }
      if (name === 'neurostrata_task_complete') activeTaskId = null
      else if (taskId) activeTaskId = taskId
    }

    if (HERMES_MUTATING_TOOLS.includes(name)) {
      const guard = await runMutatingToolGuard({
        name,
        args,
        session_id:
          asString(input?.session_id) ??
          asString(input?.sessionID) ??
          activeSessionId ??
          undefined
      })
      if (guard.verdict === 'reject') {
        console.warn('[NeuroStrata] Guard rejected mutating tool', guard.log)
        throw new Error(`Zero-Action Start violation: ${guard.reason}`)
      }
      if (guard.verdict === 'unavailable') {
        console.error('[NeuroStrata] Guard validation failed', {
          error: guard.message
        })
        throw new Error(
          `Guard validation unavailable (NeuroStrata core degraded): ${guard.message}`
        )
      }
    }
  }

  async function onTurnEnd(turn: HermesTurn): Promise<void> {
    const text = turn.content
    const scope = workingSetScope(
      sessionProjectDir ?? process.cwd(),
      config.namespace,
      turn.sessionId ?? activeSessionId
    )
    if (scope) {
      try {
        const result = await runWorkingSetTurnBoundary(
          scope,
          eventId('onTurnEnd', turn.turnId),
          turn.turnId
        )
        if (result.frame) upsertWorkingSetFrame(turn, result.frame)
      } catch {
        // Turn-boundary GC is fail-soft.
      }
    }

    if (!config.intentDetection.enabled) return
    if (text.includes('neurostrata_task_create')) return
    const signals = detectTaskIntentsLocal(text)
    if (signals.length === 0 || !config.intentDetection.injectReminder) return
    const markers = signals.map((signal) => signal.marker).join(', ')
    console.info(
      '[NeuroStrata] ' +
        `Task intent detected (${markers}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`
    )
  }

  async function handleHookEvent(
    event: Record<string, unknown>
  ): Promise<Record<string, unknown>> {
    await ensureInitialized(event)
    const hookEventName =
      asString(event.hook_event_name) ??
      asString(event.eventName) ??
      'PreToolUse'
    const name = asString(event.tool_name) ?? asString(event.toolName) ?? ''
    const toolInput = firstRecord(
      event.tool_input ?? event.input ?? event.args ?? {}
    )

    if (hookEventName !== 'PreToolUse') {
      if (hookEventName === 'Stop' && typeof event.transcript === 'string') {
        try {
          await onTurnEnd({ role: 'assistant', content: event.transcript })
        } catch {
          // Turn-end handling is fail-soft for the runner.
        }
      }
      return { continue: true }
    }

    await markTool({
      tool: name,
      input: toolInput,
      session_id: event.session_id,
      tool_use_id: event.tool_use_id
    })
    if (HERMES_MUTATING_TOOLS.includes(name)) {
      const guard = await runMutatingToolGuard({
        name,
        args: toolInput,
        session_id: asString(event.session_id) ?? activeSessionId ?? undefined
      })
      if (guard.verdict === 'reject') {
        return {
          hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: 'deny',
            permissionDecisionReason: `Zero-Action Start violation: ${guard.reason}`
          }
        }
      }
      if (guard.verdict === 'unavailable') {
        return {
          hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: 'deny',
            permissionDecisionReason: `Guard validation unavailable (NeuroStrata core degraded): ${guard.message}`
          }
        }
      }
    }
    const scoped = addSessionWorktree(
      name,
      toolInput,
      sessionProjectDir ?? asString(event.cwd) ?? process.cwd()
    )
    return {
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'allow',
        ...(scoped ? { updatedInput: toolInput } : {})
      }
    }
  }

  return {
    id: 'neurostrata-hermes',
    setup: hermesInitialize,
    initialize: hermesInitialize,
    handleHookEvent,
    handleClaudeHookEvent: handleHookEvent,
    onTurnEnd,
    fetchContextPlan,
    resetState: resetHermesState
  }
}

export default createHermesShim()
