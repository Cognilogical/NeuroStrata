import { createHash as openClawCreateHash } from 'node:crypto'
import * as openClawFs from 'node:fs'
import * as openClawPath from 'node:path'
import {
  NeuroStrataMCPClient,
  addSessionWorktree,
  currentSessionEpoch,
  detectTaskIntentsLocal,
  discoverProjectNamespace,
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

export interface OpenClawPluginContext {
  projectDir?: string
  config?: Record<string, unknown>
  hooks?: {
    beforeFileOp?: (handler: FileOpHandler) => void | Promise<void>
    onTurnEnd?: (handler: TurnEndHandler) => void | Promise<void>
  }
  conversation?: {
    injectSystemMessage?: (message: string) => void | Promise<void>
  }
  logger?: {
    info?: (message: string, detail?: unknown) => void
    warn?: (message: string, detail?: unknown) => void
    error?: (message: string, detail?: unknown) => void
  }
}

export interface FileOperation {
  type: 'write' | 'edit' | 'delete' | 'mkdir'
  path: string
  content?: string
  oldContent?: string
  newContent?: string
}

export interface FileOpResult {
  allowed: boolean
  reason?: string
  guardVerdict?: GuardValidateResult
  degraded?: boolean
}

export interface ConversationTurn {
  role: 'user' | 'assistant' | 'system'
  content: string
  turnId?: string
  sessionId?: string
}

export type FileOpHandler = (op: FileOperation) => Promise<FileOpResult>
export type TurnEndHandler = (turn: ConversationTurn) => Promise<void>

function createOpenClawShim() {
  const createHash = openClawCreateHash
  const { lstatSync, readFileSync, realpathSync } = openClawFs
  const { join, relative, resolve } = openClawPath

  type OpenClawConfig = ReturnType<typeof resolveConfig>
  type OpenClawGuard =
    | { verdict: 'pass' }
    | {
        verdict: 'reject'
        reason: string
        ruleIds: string[]
        guardVerdict: GuardValidateResult
      }
    | { verdict: 'unavailable'; message: string }

  let config: OpenClawConfig = resolveConfig({})
  let mcpClient: NeuroStrataMCPClient | null = null
  let pluginCtx: OpenClawPluginContext | null = null
  let sessionProjectDir: string | undefined
  let activeSessionId: string | null = null
  let activeTaskId: string | null = null
  let mutationsSinceNote = 0

  function asString(value: unknown): string | undefined {
    return typeof value === 'string' && value.length > 0 ? value : undefined
  }

  function asRecord(value: unknown): Record<string, unknown> {
    return value !== null && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {}
  }

  function requireCtx(): OpenClawPluginContext {
    if (!pluginCtx)
      throw new Error('NeuroStrata plugin used before initialize()')
    return pluginCtx
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
    return id ? `openclaw:${eventName}:${id}` : undefined
  }

  async function markFile(path: string, nativeId?: unknown): Promise<void> {
    const normalized = relativePath(sessionProjectDir ?? process.cwd(), path)
    const scope = workingSetScope(
      sessionProjectDir ?? process.cwd(),
      config.namespace,
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
        eventId('beforeFileOp', nativeId)
      )
    } catch {
      // GC observation never blocks a host file operation.
    }
  }

  function resetOpenClawState(): void {
    resetState()
    activeSessionId = null
    activeTaskId = null
    mutationsSinceNote = 0
  }

  async function openClawInitialize(
    ctx: (Partial<OpenClawPluginContext> & Record<string, unknown>) | undefined
  ): Promise<void> {
    resetOpenClawState()
    pluginCtx = (ctx as OpenClawPluginContext | undefined) ?? null
    sessionProjectDir = resolveProjectRoot(ctx)
    config = resolveConfig(ctx?.config ?? ctx ?? {}, sessionProjectDir)
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

    await ctx?.hooks?.beforeFileOp?.(wrapFileOp)
    await ctx?.hooks?.onTurnEnd?.(onTurnEnd)

    ctx?.logger?.info?.('NeuroStrata OpenClaw plugin initialized', {
      namespace: config.namespace,
      endpoint: config.endpoint
    })
  }

  async function ensureInitialized(
    event?: Record<string, unknown>
  ): Promise<void> {
    if (mcpClient) return
    await openClawInitialize({
      projectDir: resolveHookProjectDir('openclaw', event),
      config: {}
    })
  }

  async function runActionGuard(
    actionType: string,
    payload: string
  ): Promise<OpenClawGuard> {
    if (!config.strictMode) {
      mutationsSinceNote++
      return { verdict: 'pass' }
    }
    const cwd = process.cwd()
    try {
      if (!mcpClient) throw new Error('NeuroStrata MCP client not initialized')
      const text = await mcpClient.callTool('neurostrata_guard_validate', {
        namespace: config.namespace,
        session_id: activeSessionId ?? 'unknown',
        active_task_id: activeTaskId,
        worktree: cwd,
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
          ruleIds: result.rule_ids,
          guardVerdict: result
        }
      }
    } catch (error) {
      return { verdict: 'unavailable', message: String(error) }
    }
    mutationsSinceNote++
    return { verdict: 'pass' }
  }

  const OPENCLAW_FILE_OP_TOOLS: Record<string, FileOperation['type']> = {
    write_file: 'write',
    write: 'write',
    edit: 'edit',
    patch_file: 'edit',
    delete: 'delete',
    mkdir: 'mkdir'
  }
  const OPENCLAW_SHELL_TOOLS = ['terminal', 'bash', 'shell', 'command']

  function guardToHookResponse(guard: OpenClawGuard): Record<string, unknown> {
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
    return {
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'allow'
      }
    }
  }

  async function handleHookEvent(
    event: Record<string, unknown>
  ): Promise<Record<string, unknown>> {
    await ensureInitialized(event)
    const hookEventName =
      asString(event.hook_event_name) ??
      asString(event.eventName) ??
      'PreToolUse'
    const toolName = asString(event.tool_name) ?? asString(event.toolName) ?? ''
    const rawInput = event.tool_input ?? event.input ?? event.args ?? {}
    const toolInput = asRecord(rawInput)

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

    const fileOpType = OPENCLAW_FILE_OP_TOOLS[toolName]
    if (fileOpType) {
      const path =
        asString(toolInput.path) ?? asString(toolInput.filePath) ?? ''
      await markFile(path, event.tool_use_id ?? event.toolUseId)
      const payload = JSON.stringify({
        path,
        content: asString(toolInput.content),
        oldContent: asString(toolInput.oldContent),
        newContent: asString(toolInput.newContent)
      })
      return guardToHookResponse(await runActionGuard(fileOpType, payload))
    }
    if (OPENCLAW_SHELL_TOOLS.includes(toolName)) {
      return guardToHookResponse(
        await runActionGuard(toolName, JSON.stringify(toolInput))
      )
    }
    const scoped = addSessionWorktree(
      toolName,
      toolInput,
      sessionProjectDir ?? asString(event.cwd) ?? process.cwd()
    )
    if (toolName.includes('neurostrata_task_claim')) {
      activeSessionId =
        asString(toolInput.session_id) ??
        asString(event.session_id) ??
        activeSessionId
      activeTaskId = asString(toolInput.id) ?? activeTaskId
      mutationsSinceNote = 0
    }
    return {
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'allow',
        ...(scoped ? { updatedInput: toolInput } : {})
      }
    }
  }

  async function wrapFileOp(op: FileOperation): Promise<FileOpResult> {
    await markFile(op.path, asRecord(op).id)
    if (op.path && op.path.trim() !== '') {
      try {
        const snippets = await prefetchMemoriesForFile(
          config.namespace,
          op.path,
          { projectRoot: sessionProjectDir }
        )
        if (snippets.length > 0) {
          await requireCtx().conversation?.injectSystemMessage?.(
            `[NeuroStrata] Governing memories for ${op.path}:\n${snippets.join('\n')}`
          )
        }
      } catch (error) {
        requireCtx().logger?.warn?.('File-touch prefetch failed', {
          path: op.path,
          error: String(error)
        })
      }
    }

    if (!config.strictMode) {
      mutationsSinceNote++
      return { allowed: true }
    }

    const actionType = op.type
    const payload = JSON.stringify({
      path: op.path,
      content: op.content,
      oldContent: op.oldContent,
      newContent: op.newContent
    })
    const guard = await runActionGuard(actionType, payload)
    if (guard.verdict === 'reject') {
      requireCtx().logger?.warn?.('Guard rejected file operation', {
        actionType,
        session_id: activeSessionId ?? 'unknown',
        active_task_id: activeTaskId,
        worktree: process.cwd(),
        reason: guard.reason,
        rule_ids: guard.ruleIds
      })
      return {
        allowed: false,
        reason: `Zero-Action Start violation: ${guard.reason}`,
        guardVerdict: guard.guardVerdict,
        degraded: false
      }
    }
    if (guard.verdict === 'unavailable') {
      requireCtx().logger?.error?.('Guard validation failed', {
        error: guard.message
      })
      return {
        allowed: false,
        reason: `Guard validation unavailable (NeuroStrata core degraded): ${guard.message}`,
        degraded: true
      }
    }
    return { allowed: true }
  }

  function trackFromTurn(turn: ConversationTurn): void {
    const lower = turn.content.toLowerCase()
    if (
      !lower.includes('neurostrata_task_claim') &&
      !lower.includes('neurostrata_task_update') &&
      !lower.includes('neurostrata_task_complete')
    )
      return
    const taskId = /(?<![\w-])id["'\s]*[:=]\s*["']([^"']+)[""']/.exec(
      turn.content
    )?.[1]
    const sessionId =
      /session[_\s-]?id["'\s]*[:=]\s*["']([^"']+)[""']/i.exec(
        turn.content
      )?.[1] ?? undefined
    if (sessionId) activeSessionId = sessionId
    if (
      lower.includes('neurostrata_task_claim') ||
      lower.includes('neurostrata_task_complete') ||
      (lower.includes('neurostrata_task_update') && lower.includes('note'))
    ) {
      mutationsSinceNote = 0
    }
    if (lower.includes('neurostrata_task_complete')) activeTaskId = null
    else if (taskId) activeTaskId = taskId
  }

  async function onTurnEnd(turn: ConversationTurn): Promise<void> {
    trackFromTurn(turn)
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

    if (!config.intentDetection.enabled || turn.role === 'system') return
    if (text.includes('neurostrata_task_create')) return
    const signals = detectTaskIntentsLocal(text)
    if (signals.length === 0 || !config.intentDetection.injectReminder) return
    const markers = signals.map((signal) => signal.marker).join(', ')
    await requireCtx().conversation?.injectSystemMessage?.(
      `[NeuroStrata] Task intent detected (${markers}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`
    )
  }

  return {
    id: 'neurostrata-openclaw',
    setup: openClawInitialize,
    initialize: openClawInitialize,
    handleHookEvent,
    onTurnEnd,
    resetState: resetOpenClawState
  }
}

export default createOpenClawShim()
