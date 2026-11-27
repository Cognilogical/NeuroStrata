import test from 'node:test'
import assert from 'node:assert/strict'
import http from 'node:http'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
  mkdirSync
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  canonicalTempDir,
  writeMarker
} from '../../tests/fixtures/root-marker-layout.mjs'
import plugin, {
  createHarnessPlugin,
  detectTaskIntentsLocal,
  parseGuardValidate,
  parseFileMemoryHits,
  resolveConfig,
  NeuroStrataMCPClient,
  adapterStatus,
  onTurnEnd,
  fetchContextPlan,
  resetClientForTesting,
  prefetchMemoriesForFile,
  prefetchedPaths,
  injectedMemoryIds,
  PREFETCH_LRU_CAP,
  SUBAGENT_CONTEXT_MARKER,
  buildSubagentContextHeader,
  injectSubagentPrompt,
  WORKING_SET_FRAME_OPEN,
  WORKING_SET_FRAME_CLOSE
} from './dist/index.js'

function resetModuleState() {
  resetClientForTesting()
}

test('Plugin.define exports V2 setup and registers execute.before hook', async () => {
  assert.equal(plugin.id, 'neurostrata')
  assert.equal(typeof plugin.setup, 'function')

  let registeredHook = null
  let registeredCallback = null
  const ctx = {
    config: {
      namespace: 'test-ns',
      endpoint: 'http://127.0.0.1:1/mcp',
      bearerToken: 'test-token',
      strictMode: true,
      requestTimeoutMs: 100
    },
    tool: {
      hook: async (name, cb) => {
        registeredHook = name
        registeredCallback = cb
      }
    }
  }

  await plugin.setup(ctx)
  assert.equal(registeredHook, 'execute.before')
  assert.equal(typeof registeredCallback, 'function')

  // Non-mutating tool passes through without throwing
  await registeredCallback({
    tool: 'read',
    input: { path: 'foo' },
    sessionID: 's1'
  })

  // Mutating tool invokes guard_validate and fails closed when core unreachable
  await assert.rejects(
    () =>
      registeredCallback({
        tool: 'write',
        input: { path: 'foo.txt', content: 'bar' },
        sessionID: 's1'
      }),
    /Guard validation unavailable/
  )
})

test('parseGuardValidate and detectTaskIntentsLocal', () => {
  const verdict = parseGuardValidate(
    JSON.stringify({
      verdict: {
        bucket: 'DeterministicReject',
        reasons: ['claim task first']
      },
      rule_ids_triggered: ['r1']
    })
  )
  assert.equal(verdict.verdict, 'reject')
  assert.deepEqual(verdict.reasons, ['claim task first'])

  const intents = detectTaskIntentsLocal('TODO: wire up validation later')
  assert.equal(intents.length, 1)
  assert.equal(intents[0].marker, 'todo:')
})

// ─── Mock local HTTP JSON-RPC server for healthy daemon round-trip ───

function makeMockRpcResponse(id, resultObj, errorObj = null) {
  return JSON.stringify({
    jsonrpc: '2.0',
    id,
    ...(errorObj ? { error: errorObj } : { result: resultObj })
  })
}

function createMockDaemonServer(port = 0) {
  // Cross-test call counter, surfaced so tests can assert a deduped touch
  // did not re-query the daemon.
  const counters = { memoriesForFile: 0 }
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      if (req.method !== 'POST') {
        res.writeHead(405)
        res.end()
        return
      }
      let body = ''
      req.on('data', (chunk) => (body += chunk))
      req.on('end', () => {
        try {
          const payload = JSON.parse(body)
          const id = payload.id ?? 0
          const method = payload.method
          if (method === 'tools/list') {
            res.writeHead(200, { 'Content-Type': 'application/json' })
            res.end(
              makeMockRpcResponse(id, {
                tools: [
                  { name: 'neurostrata_guard_validate' },
                  { name: 'neurostrata_task_create' },
                  { name: 'neurostrata_get_snapshot' },
                  { name: 'neurostrata_append_log' },
                  { name: 'neurostrata_search_memory' },
                  { name: 'neurostrata_context_plan' },
                  { name: 'neurostrata_memories_for_file' }
                ]
              })
            )
            return
          }
          if (method === 'tools/call') {
            const params = payload.params ?? {}
            const name = params.name ?? params.arguments?.name
            if (name === 'neurostrata_memories_for_file') {
              counters.memoriesForFile++
              res.writeHead(200, { 'Content-Type': 'application/json' })
              res.end(
                makeMockRpcResponse(id, {
                  content: [
                    {
                      type: 'text',
                      text: JSON.stringify({
                        hits: [
                          {
                            id: 'mem-rule-1',
                            valid_from: '2026-01-01T00:00:00Z',
                            memory_type: 'rule',
                            content: 'Always claim a task before editing files.'
                          },
                          {
                            id: 'mem-fact-2',
                            valid_from: '2026-01-02T00:00:00Z',
                            memory_type: 'fact',
                            content: 'The daemon serves MCP on 127.0.0.1:34343.'
                          }
                        ]
                      })
                    }
                  ]
                })
              )
              return
            }
            // Healthy pass for guard; snapshot / log / search return ready + unlocked
            if (name === 'neurostrata_get_snapshot') {
              res.writeHead(200, { 'Content-Type': 'application/json' })
              res.end(
                makeMockRpcResponse(id, {
                  content: [
                    {
                      type: 'text',
                      text: '{"status":"ready","preflight_unlocked":true}'
                    }
                  ]
                })
              )
              return
            }
            if (name === 'neurostrata_append_log') {
              res.writeHead(200, { 'Content-Type': 'application/json' })
              res.end(
                makeMockRpcResponse(id, {
                  content: [
                    {
                      type: 'text',
                      text: '{"status":"ready","preflight_unlocked":true,"logged":true}'
                    }
                  ]
                })
              )
              return
            }
            if (name === 'neurostrata_search_memory') {
              res.writeHead(200, { 'Content-Type': 'application/json' })
              res.end(
                makeMockRpcResponse(id, {
                  content: [
                    {
                      type: 'text',
                      text: '{"status":"ready","unlocked_preflight":true}'
                    }
                  ]
                })
              )
              return
            }
            if (name === 'neurostrata_context_plan') {
              res.writeHead(200, { 'Content-Type': 'application/json' })
              res.end(
                makeMockRpcResponse(id, {
                  content: [
                    { type: 'text', text: '{"schema_version":"v1","items":[]}' }
                  ]
                })
              )
              return
            }
            if (name === 'neurostrata_guard_validate') {
              res.writeHead(200, { 'Content-Type': 'application/json' })
              res.end(
                makeMockRpcResponse(id, {
                  content: [
                    {
                      type: 'text',
                      text: '{"verdict":{"bucket":"Pass"},"rule_ids_triggered":[]}'
                    }
                  ]
                })
              )
              return
            }
            // Unknown tool → error response
            res.writeHead(200, { 'Content-Type': 'application/json' })
            res.end(
              makeMockRpcResponse(id, null, {
                code: -32601,
                message: 'Unknown tool'
              })
            )
            return
          }
          // Unknown method
          res.writeHead(200, { 'Content-Type': 'application/json' })
          res.end(
            makeMockRpcResponse(id, null, {
              code: -32601,
              message: 'Unknown method'
            })
          )
        } catch {
          res.writeHead(400)
          res.end()
        }
      })
    })
    server.listen(port, () => {
      const addr = server.address()
      resolve({ server, url: `http://127.0.0.1:${addr.port}/mcp`, counters })
    })
  })
}

// ─── (a) Healthy daemon round-trip + adapter status "ready" / unlocked ───

test('Healthy mock daemon: JSON-RPC 2.0 round-trip and unlocked pre-flight', async () => {
  const { server, url } = await createMockDaemonServer()
  try {
    const client = new NeuroStrataMCPClient({
      endpoint: url,
      token: 'mock-token',
      timeoutMs: 2000
    })

    // Snapshot
    const snapshotText = await client.callTool('neurostrata_get_snapshot', {})
    const snapshot = JSON.parse(snapshotText)
    assert.equal(snapshot.status, 'ready')
    assert.equal(snapshot.preflight_unlocked, true)

    // Append log
    const logText = await client.callTool('neurostrata_append_log', {
      tags: ['test']
    })
    const log = JSON.parse(logText)
    assert.equal(log.status, 'ready')
    assert.equal(log.preflight_unlocked, true)
    assert.equal(log.logged, true)

    // Search memory
    const searchText = await client.callTool('neurostrata_search_memory', {
      query: 'preflight'
    })
    const search = JSON.parse(searchText)
    assert.equal(search.status, 'ready')
    assert.equal(search.unlocked_preflight, true)

    // Adapter status via initialize on a healthy endpoint should report ready
    const ctxHealthy = {
      config: {
        namespace: 'healthy-ns',
        endpoint: url,
        bearerToken: 'mock-token',
        strictMode: false,
        requestTimeoutMs: 500
      },
      tool: { hook: async () => {} }
    }
    await plugin.setup(ctxHealthy)
    const status = adapterStatus()
    assert.equal(
      status.state,
      'ready',
      'adapter should be ready with healthy daemon'
    )
    assert.equal(status.endpoint, url)
  } finally {
    server.close()
  }
})

// ─── (b) Unreachable daemon fail-open fallback (degraded / unavailable) ───

test('Unreachable daemon fail-open fallback keeps degraded endpoint visible', async () => {
  // Intentionally use unreachable endpoint (127.0.0.1:1)
  const ctxUnreachable = {
    config: {
      namespace: 'fail-open',
      endpoint: 'http://127.0.0.1:1/mcp',
      bearerToken: null,
      strictMode: true,
      requestTimeoutMs: 100
    },
    tool: {
      hook: async (name, cb) => {
        // Mutating tool should throw because guard is unreachable
        if (name === 'execute.before') {
          await assert.rejects(
            () =>
              cb({
                tool: 'write',
                input: { path: 'x.txt', content: 'y' },
                sessionID: 's1'
              }),
            /Guard validation unavailable/
          )
        }
      }
    }
  }
  await plugin.setup(ctxUnreachable)
  const status = adapterStatus()
  assert.equal(
    status.endpoint,
    'http://127.0.0.1:1/mcp',
    'degraded endpoint preserved'
  )
  assert.equal(
    status.state,
    'unavailable',
    'adapter should be unavailable when daemon unreachable'
  )
})

// ─── (c) onTurnEnd + fetchContextPlan ───

test('onTurnEnd resolves without throwing', async () => {
  await onTurnEnd({ role: 'user', content: 'TODO: track this work' })
  await onTurnEnd({ role: 'assistant', content: 'no intents here' })
})

test('fetchContextPlan calls neurostrata_context_plan and returns text', async () => {
  const { server, url } = await createMockDaemonServer()
  try {
    const ctx = {
      config: {
        namespace: 'ctx-ns',
        endpoint: url,
        bearerToken: 'mock-token',
        strictMode: false,
        requestTimeoutMs: 500
      },
      tool: { hook: async () => {} }
    }
    await plugin.setup(ctx)
    const text = await fetchContextPlan({
      namespace: 'ctx-ns',
      query: 'foo',
      max_items: 2
    })
    assert.equal(typeof text, 'string')
    assert.ok(text.includes('schema_version') || text.includes('items'))
  } finally {
    server.close()
  }
})

test('fetchContextPlan throws when MCP client not initialized', async () => {
  // Drop the cached client so the module is back to "not initialized".
  resetClientForTesting()
  await assert.rejects(
    () => fetchContextPlan({ namespace: 'ctx-ns' }),
    /NeuroStrata MCP client not initialized/
  )
})

// ─── (d) File-touch prefetch (design §4.5) ───

test('parseFileMemoryHits reads hits and memories arrays', () => {
  const asHits = parseFileMemoryHits(
    JSON.stringify({
      hits: [{ id: 'm1', valid_from: 't0', memory_type: 'rule', content: 'x' }]
    })
  )
  assert.equal(asHits.length, 1)
  assert.equal(asHits[0].id, 'm1')

  const asMemories = parseFileMemoryHits(
    JSON.stringify({
      memories: [
        { id: 'm2', valid_from: 't1', memory_type: 'fact', content: 'y' }
      ]
    })
  )
  assert.equal(asMemories.length, 1)
  assert.equal(asMemories[0].id, 'm2')

  // A hit without an id can never be deduped, so it is never surfaced.
  const noId = parseFileMemoryHits(
    JSON.stringify({
      hits: [
        { valid_from: 't2', content: 'z' },
        { id: 'm3', content: 'w' }
      ]
    })
  )
  assert.equal(noId.length, 1)
  assert.equal(noId[0].id, 'm3')
})

test('prefetchMemoriesForFile: LRU path dedup and injectedMemoryIds dedup', async () => {
  const { server, url, counters } = await createMockDaemonServer()
  try {
    const ctx = {
      config: {
        namespace: 'prefetch-ns',
        endpoint: url,
        bearerToken: 'mock-token',
        strictMode: false,
        requestTimeoutMs: 500
      },
      tool: { hook: async () => {} }
    }
    await plugin.setup(ctx)

    // Isolate from any earlier test's module-level state.
    prefetchedPaths.clear()
    injectedMemoryIds.clear()
    counters.memoriesForFile = 0

    // First touch of a path: one daemon query, both governing memories returned.
    const first = await prefetchMemoriesForFile('prefetch-ns', 'src/foo.rs')
    assert.equal(first.length, 2)
    assert.ok(first[0].startsWith('[NeuroStrata rule mem-rule-1'))
    assert.ok(first[1].startsWith('[NeuroStrata fact mem-fact-2'))
    assert.equal(counters.memoriesForFile, 1)

    // Second touch of the same path: LRU dedup — no re-query, empty answer.
    const second = await prefetchMemoriesForFile('prefetch-ns', 'src/foo.rs')
    assert.deepEqual(second, [])
    assert.equal(counters.memoriesForFile, 1, 'LRU dedup must not re-query')

    // A different file returning the same memories: injectedMemoryIds dedup
    // — the query happens but yields no new snippets.
    const third = await prefetchMemoriesForFile('prefetch-ns', 'src/bar.rs')
    assert.deepEqual(third, [])
    assert.equal(counters.memoriesForFile, 2, 'new path was queried once')
    assert.equal(injectedMemoryIds.size, 2, 'no duplicate memory keys injected')

    // LRU cap: the prefetched-path map never grows past 128 entries.
    for (let i = 0; i < PREFETCH_LRU_CAP * 2; i++) {
      await prefetchMemoriesForFile('prefetch-ns', `file-${i}.rs`)
    }
    assert.equal(prefetchedPaths.size, PREFETCH_LRU_CAP)

    // Empty path is a no-op that touches nothing.
    const empty = await prefetchMemoriesForFile('prefetch-ns', '   ')
    assert.deepEqual(empty, [])

    prefetchedPaths.clear()
    injectedMemoryIds.clear()
  } finally {
    server.close()
  }
})

test('injectSubagentPrompt prepends context header once and is idempotent', () => {
  const target = { prompt: 'Original prompt text' }
  const namespace = 'test-ns'
  const taskId = 'task-123'

  // First call should inject
  const first = injectSubagentPrompt(target, namespace, taskId)
  assert.equal(first, true, 'first call returns true')
  assert.ok(target.prompt.includes(SUBAGENT_CONTEXT_MARKER))
  assert.ok(
    target.prompt.startsWith(
      `### NeuroStrata Task & Graph Context (AUTO-INJECTED BY NEUROSTRATA PLUGIN)`
    )
  )
  assert.ok(target.prompt.includes(`Namespace: \`${namespace}\``))
  assert.ok(target.prompt.includes(`Active Task ID: \`${taskId}\``))
  assert.ok(target.prompt.endsWith('Original prompt text'))

  // Second call should be idempotent (no double-injection)
  const second = injectSubagentPrompt(target, namespace, taskId)
  assert.equal(second, false, 'second call returns false')
  // Header embeds the marker twice (title line + body reference); assert count
  // stability across repeated injection rather than an exact count of 1.
  const afterFirst = (
    target.prompt.match(/### NeuroStrata Task & Graph Context/g) || []
  ).length
  assert.ok(afterFirst >= 1, 'header injected at least once')
  assert.equal(
    injectSubagentPrompt(target, namespace, taskId),
    false,
    'third call is also a no-op'
  )
  assert.equal(
    (target.prompt.match(/### NeuroStrata Task & Graph Context/g) || []).length,
    afterFirst,
    'idempotent: no additional markers injected'
  )

  // Missing prompt property returns false
  const noPrompt = { other: 'data' }
  assert.equal(injectSubagentPrompt(noPrompt, namespace, taskId), false)

  // null prompt property returns false
  const nullPrompt = { prompt: null }
  assert.equal(injectSubagentPrompt(nullPrompt, namespace, taskId), false)
})

test('buildSubagentContextHeader includes required fields', () => {
  const header = buildSubagentContextHeader('my-ns', 'task-abc')
  assert.ok(header.includes('### NeuroStrata Task & Graph Context'))
  assert.ok(header.includes('AUTO-INJECTED BY NEUROSTRATA PLUGIN'))
  assert.ok(header.includes('Namespace: \`my-ns\`'))
  assert.ok(header.includes('Active Task ID: \`task-abc\`'))
  assert.ok(header.includes('Graph-First Lookup Rule'))
  assert.ok(header.includes('neurostrata_memories_for_file'))
  assert.ok(header.includes('neurostrata_search_memory'))

  // With null taskId
  const headerNull = buildSubagentContextHeader('other-ns', null)
  assert.ok(
    headerNull.includes('Active Task ID: \`unclaimed (claim a task first)\`')
  )
})

// ─── Thin-wrapper regression: opencode adapter is a re-export of core ───

test('opencode thin wrapper re-exports core surface and builds its own plugin', () => {
  const plugin2 = createHarnessPlugin('opencode', 'OpenCode')
  assert.equal(plugin2.id, 'opencode')
  assert.equal(plugin2.name, 'OpenCode')
  assert.equal(typeof plugin2.setup, 'function')
  assert.equal(typeof plugin2.handleHookEvent, 'function')
  assert.equal(typeof plugin2.handleClaudeHookEvent, 'function')
  assert.equal(typeof plugin2.onTurnEnd, 'function')
  assert.equal(typeof plugin2.fetchContextPlan, 'function')

  assert.equal(plugin.id, 'neurostrata')
  assert.equal(plugin.name, 'OpenCode')
  assert.equal(plugin.handleHookEvent, plugin.handleClaudeHookEvent)
})

test('opencode default export handleHookEvent and handleClaudeHookEvent are aliased', async () => {
  const target = { prompt: 'refactor this file' }
  const out = await plugin.handleHookEvent({
    hook_event_name: 'PreToolUse',
    tool_name: 'subagent',
    tool_input: target
  })
  assert.equal(out.continue, undefined) // #368 F3: no continue on PreToolUse
  assert.equal(out.hookSpecificOutput.permissionDecision, 'allow')
  assert.ok(target.prompt.includes('### NeuroStrata Task & Graph Context'))

  const target2 = { prompt: 'refactor this file too' }
  const out2 = await plugin.handleClaudeHookEvent({
    hook_event_name: 'PreToolUse',
    tool_name: 'subagent',
    tool_input: target2
  })
  assert.equal(out2.continue, undefined) // #368 F3: no continue on PreToolUse
  assert.equal(out2.hookSpecificOutput.permissionDecision, 'allow')
  assert.ok(target2.prompt.includes('### NeuroStrata Task & Graph Context'))
})

test('OpenCode V2 execute.after mutates event.result.content and registers session.hook context/compaction', async () => {
  const tmpDir = mkdtempSync(join(tmpdir(), 'ns-oc2-v226-'))
  const nsDir = join(tmpDir, '.NeuroStrata')
  mkdirSync(nsDir, { recursive: true })
  writeFileSync(
    join(nsDir, 'active_session.json'),
    JSON.stringify({
      namespace: 'NeuroStrata',
      task_id: 'neurostrata-a1si',
      status: 'in_progress',
      session_id: 'ses_v226',
      updated_at: new Date().toISOString()
    })
  )
  writeFileSync(
    join(nsDir, 'PluginConfig.json'),
    JSON.stringify({
      guardMode: 'warn',
      enableSubagentContextPropagation: true,
      daemonUrl: 'http://127.0.0.1:59999'
    })
  )

  const sessionHooks = new Map()
  const toolHooks = new Map()
  const ctx = {
    location: {
      directory: tmpDir,
      project: { root: tmpDir, canonical: tmpDir }
    },
    session: {
      hook(name, fn) {
        sessionHooks.set(name, fn)
      }
    },
    tool: {
      hook(name, fn) {
        toolHooks.set(name, fn)
      }
    }
  }

  await plugin.initialize(ctx)
  assert.equal(typeof sessionHooks.get('context'), 'function')
  assert.equal(typeof sessionHooks.get('compaction'), 'function')
  assert.equal(typeof toolHooks.get('execute.after'), 'function')

  const afterEvent = {
    name: 'write',
    callID: 'call_1',
    args: { path: 'src/server.rs' },
    status: 'completed',
    result: {
      content: 'TODO: wire task candidate provenance in server.rs',
      output: 'TODO: wire task candidate provenance in server.rs'
    }
  }
  await toolHooks.get('execute.after')(afterEvent)
  assert.match(afterEvent.result.content, /TASK INTENT DETECTED/)
  assert.match(afterEvent.result.output, /TASK INTENT DETECTED/)

  const ctxEvent1 = { sessionID: 'ses_v226', messages: [] }
  await sessionHooks.get('context')(ctxEvent1)
  assert.equal(typeof ctxEvent1.additionalContext, 'string')
  assert.match(ctxEvent1.additionalContext, /neurostrata-a1si/)

  const ctxEvent2 = { sessionID: 'ses_v226', messages: [] }
  await sessionHooks.get('context')(ctxEvent2)
  assert.equal(ctxEvent2.additionalContext, undefined)

  await sessionHooks.get('compaction')({ sessionID: 'ses_v226' })
  const ctxEvent3 = { sessionID: 'ses_v226', messages: [] }
  await sessionHooks.get('context')(ctxEvent3)
  assert.match(ctxEvent3.additionalContext, /neurostrata-a1si/)
})

test('OpenCode V2 execute.after preserves array content[0].text and appends TASK INTENT DETECTED', async () => {
  const tmpDir = mkdtempSync(join(tmpdir(), 'ns-oc2-array-'))
  const nsDir = join(tmpDir, '.NeuroStrata')
  mkdirSync(nsDir, { recursive: true })
  writeFileSync(
    join(nsDir, 'active_session.json'),
    JSON.stringify({
      namespace: 'NeuroStrata',
      task_id: 'neurostrata-a1si',
      status: 'in_progress',
      session_id: 'ses_array',
      updated_at: new Date().toISOString()
    })
  )

  const toolHooks = new Map()
  const ctx = {
    location: {
      directory: tmpDir,
      project: { root: tmpDir, canonical: tmpDir }
    },
    session: {
      hook() {}
    },
    tool: {
      hook(name, fn) {
        toolHooks.set(name, fn)
      }
    }
  }

  await plugin.initialize(ctx)
  assert.equal(typeof toolHooks.get('execute.after'), 'function')

  const afterEvent = {
    name: 'write',
    callID: 'call_array_1',
    args: { path: 'src/server.rs' },
    status: 'completed',
    result: {
      content: [
        { type: 'text', text: 'Read file src/server.rs\nTODO: wire candidate' }
      ],
      output: 'Read file src/server.rs\nTODO: wire candidate'
    }
  }
  await toolHooks.get('execute.after')(afterEvent)
  assert.ok(
    Array.isArray(afterEvent.result.content),
    'result.content stays an array'
  )
  assert.equal(afterEvent.result.content[0].type, 'text', 'text item preserved')
  assert.match(
    afterEvent.result.content[0].text,
    /Read file src\/server\.rs/,
    'original text kept'
  )
  assert.match(
    afterEvent.result.content[0].text,
    /TASK INTENT DETECTED/,
    'nudge appended to text'
  )
  assert.match(
    afterEvent.result.output,
    /TASK INTENT DETECTED/,
    'result.output also mutated'
  )
})

async function registerWorkingSetHooks(root) {
  resetClientForTesting()
  const sessionHooks = new Map()
  const toolHooks = new Map()
  await plugin.initialize({
    location: { directory: root, project: { root, canonical: root } },
    session: { hook: (name, fn) => sessionHooks.set(name, fn) },
    tool: { hook: (name, fn) => toolHooks.set(name, fn) },
    config: {
      namespace: 'gc-opencode',
      endpoint: 'http://127.0.0.1:1/mcp',
      requestTimeoutMs: 50,
      intentDetection: { enabled: false, injectReminder: false }
    }
  })
  return { sessionHooks, toolHooks }
}

function writeOpenCodeWorkingSetParent(
  root,
  goldenRule = 'always claim first'
) {
  writeMarker(root, 'gc-opencode')
  writeFileSync(
    join(root, '.NeuroStrata', 'active_session.json'),
    JSON.stringify({
      namespace: 'gc-opencode',
      session_id: 'ses-gc',
      task_id: 'task-gc',
      status: 'in_progress',
      golden_rules: [goldenRule]
    })
  )
}

test('working-set GC replaces eligible execute.after text exactly once', async () => {
  const root = canonicalTempDir('ns-opencode-ws-after-')
  try {
    writeOpenCodeWorkingSetParent(root)
    const { toolHooks } = await registerWorkingSetHooks(root)
    const after = toolHooks.get('execute.after')
    const raw = `HEAD${'x'.repeat(4000)}TAIL`
    const event = {
      name: 'read',
      callID: 'call-gc',
      sessionID: 'ses-gc',
      args: { path: 'src/a.ts' },
      result: { content: raw, output: raw }
    }
    await after(event)
    assert.match(event.result.content, /\[NeuroStrata GC Stub ->/)
    assert.equal(event.result.output, event.result.content)
    assert.ok(!event.result.content.includes(raw))

    const duplicate = {
      ...event,
      result: { content: raw, output: raw }
    }
    await after(duplicate)
    assert.equal(duplicate.result.content, raw)
    assert.equal(duplicate.result.output, raw)

    const image = { type: 'image', url: 'https://example.invalid/image.png' }
    const mixed = {
      name: 'read',
      callID: 'call-mixed',
      sessionID: 'ses-gc',
      args: { path: 'src/mixed.ts' },
      result: { content: [image, { type: 'text', text: 'z'.repeat(4000) }] }
    }
    await after(mixed)
    assert.deepEqual(mixed.result.content, [
      image,
      { type: 'text', text: 'z'.repeat(4000) }
    ])

    const state = JSON.parse(
      readFileSync(join(root, '.NeuroStrata', 'active_session.json'), 'utf8')
    ).working_set
    assert.equal(state.tool_output_digests.length, 1)
    assert.equal(state.tool_output_digests[0].read_count, 1)
  } finally {
    resetModuleState()
    rmSync(root, { recursive: true, force: true })
  }
})

test('working-set GC upserts mutable context without duplicate additionalContext', async () => {
  const root = canonicalTempDir('ns-opencode-ws-context-')
  try {
    writeOpenCodeWorkingSetParent(root)
    const { sessionHooks } = await registerWorkingSetHooks(root)
    const context = sessionHooks.get('context')
    const stale = `${WORKING_SET_FRAME_OPEN} stale\n${WORKING_SET_FRAME_CLOSE}\n`
    const event = { sessionID: 'ses-gc', context: ['host', stale] }
    await context(event)
    assert.equal(event.context.length, 2)
    assert.equal(event.context[0], 'host')
    assert.match(event.context[1], /task:task-gc/)
    assert.equal(event.additionalContext, undefined)
    const first = event.context[1]

    writeOpenCodeWorkingSetParent(root, 'changed golden rule')
    await context(event)
    assert.equal(event.context.length, 2)
    assert.notEqual(event.context[1], first)
    assert.equal(
      event.context.filter((value) =>
        String(value).includes(WORKING_SET_FRAME_OPEN)
      ).length,
      1
    )
    assert.equal(event.additionalContext, undefined)

    writeOpenCodeWorkingSetParent(root, 'changed again')
    const appendOnly = { sessionID: 'ses-gc', additionalContext: first }
    await context(appendOnly)
    assert.equal(
      appendOnly.additionalContext.split(WORKING_SET_FRAME_OPEN).length - 1,
      1
    )
    assert.match(appendOnly.additionalContext, /changed again/)
  } finally {
    resetModuleState()
    rmSync(root, { recursive: true, force: true })
  }
})

// ── PR #400: event-specific hook failure replies ────────────────────────
test('hook runner replies in the shape of the failing event (#400)', async () => {
  const { spawnSync } = await import('node:child_process')
  const { mkdirSync, rmSync } = await import('node:fs')
  const { dirname } = await import('node:path')
  const { fileURLToPath } = await import('node:url')
  const runner = fileURLToPath(new URL('./cli-hook.mjs', import.meta.url))
  const base = mkdtempSync(join(tmpdir(), 'ns-400-oc-runner-'))
  try {
    const config = join(base, '.opencode', 'neurostrata-plugin.json')
    mkdirSync(dirname(config), { recursive: true })
    writeFileSync(config, '{ broken json')
    const env = { ...process.env }
    delete env.NEUROSTRATA_NAMESPACE
    delete env.CLAUDE_PROJECT_DIR
    delete env.NEUROSTRATA_HOOK_EVENT
    const run = (event) =>
      spawnSync(process.execPath, [runner], {
        input: JSON.stringify(event),
        cwd: base,
        env,
        encoding: 'utf8',
        timeout: 30000
      })
    const stop = run({
      hook_event_name: 'Stop',
      cwd: base,
      session_id: 'failing'
    })
    assert.equal(
      stop.status,
      2,
      `Stop must exit 2; stderr=${stop.stderr.slice(0, 200)}`
    )
    assert.equal(stop.stdout.trim(), '{}')
    const pre = run({
      hook_event_name: 'PreToolUse',
      cwd: base,
      tool_name: 'Write',
      tool_input: {}
    })
    assert.equal(pre.status, 0)
    const reply = JSON.parse(pre.stdout)
    assert.equal(reply.hookSpecificOutput.hookEventName, 'PreToolUse')
    assert.equal(reply.hookSpecificOutput.permissionDecision, 'deny')
    // An event without a permission decision answers {}, whether the bundle
    // lacks its handler or the hook failed.
    for (const name of [
      'SessionStart',
      'SessionEnd',
      'PreCompact',
      'PostToolUse'
    ]) {
      const quiet = run({
        hook_event_name: name,
        cwd: base,
        session_id: 'failing'
      })
      assert.equal(quiet.status, 0, name)
      assert.deepEqual(JSON.parse(quiet.stdout), {}, name)
    }
  } finally {
    rmSync(base, { recursive: true, force: true })
  }
})

// ── Task 5: OpenCode V2 3-tier projection, prompt activation, compaction ──
test('OpenCode V2 context projects tier 0 into system and tier 2 into message tail', async () => {
  const root = canonicalTempDir('ns-opencode-v2-tier-')
  try {
    writeMarker(root, 'gc-opencode-v2')
    mkdirSync(join(root, '.NeuroStrata'), { recursive: true })
    writeFileSync(
      join(root, '.NeuroStrata', 'active_session.json'),
      JSON.stringify({
        namespace: 'gc-opencode-v2',
        session_id: 'ses-v2-tier',
        task_id: 'task-v2-tier',
        status: 'in_progress',
        golden_rules: ['always claim first']
      })
    )
    resetClientForTesting()
    const sessionHooks = new Map()
    await plugin.initialize({
      location: { directory: root, project: { root, canonical: root } },
      session: { hook: (name, fn) => sessionHooks.set(name, fn) },
      tool: { hook() {} },
      config: {
        namespace: 'gc-opencode-v2',
        endpoint: 'http://127.0.0.1:1/mcp',
        requestTimeoutMs: 50,
        intentDetection: { enabled: false, injectReminder: false }
      }
    })
    assert.equal(typeof sessionHooks.get('prompt'), 'function')

    const event = {
      sessionID: 'ses-v2-tier',
      system: [],
      messages: [{ role: 'user', content: 'hello' }]
    }
    await sessionHooks.get('context')(event)
    assert.equal(event.system.length, 1)
    assert.match(
      event.system[0].text,
      /\[NeuroStrata Golden Rules — IMMORTAL PREFIX\]/
    )
    assert.ok(!/Turn:/u.test(event.system[0].text))
    assert.ok(!/Epoch:/u.test(event.system[0].text))
    assert.equal(event.messages[0].content, 'hello')
    assert.equal(event.messages.length, 2)
    assert.match(event.messages[1].content, /\[NeuroStrata Active Context\]/)
    assert.match(event.messages[1].content, /task-v2-tier/)
    assert.equal(event.additionalContext, undefined)

    await sessionHooks.get('prompt')({
      sessionID: 'ses-v2-tier',
      prompt: 'inspect file:src/a.ts'
    })

    const compaction = { sessionID: 'ses-v2-tier' }
    await sessionHooks.get('compaction')(compaction)
    assert.equal(typeof compaction.result.summary, 'string')
    assert.match(compaction.result.summary, /always claim first/)
    assert.match(compaction.result.summary, /task-v2-tier/)
  } finally {
    resetModuleState()
    rmSync(root, { recursive: true, force: true })
  }
})
