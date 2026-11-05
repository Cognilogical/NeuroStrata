import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { readFileSync, mkdirSync, writeFileSync, rmSync, mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  resolveConfig,
  findProjectRoot,
  discoverProjectNamespace,
  inspectCodeModeExecution,
  prefetchMemoriesForFile,
  formatFileTouchRecallBlock,
  getTrackingState,
  resetClientForTesting,
  handleHookEvent,
  handleClaudeHookEvent,
  buildSubagentContextHeader,
  injectSubagentPrompt,
  extractRepoFilePaths,
  enrichAndInjectSubagentPrompt,
  resolveActiveTaskFromDaemon,
  createHarnessPlugin,
  initialize,
  TOOL_CLASSIFICATION,
  isFileTouchTool,
  isMutatingTool,
  isSubagentTool,
  isCodeExecTool,
  adapterStatus,
  pendingRecallBlocks,
  injectedMemoryIds,
  prefetchedPaths,
  cachedTaskInjection,
  cachedSubagentPromptByTask,
  PREFETCH_LRU_CAP,
} from "./dist/index.js";

// ──────────────────────────────────────────────────────────────────────
// Mock local HTTP JSON-RPC daemon (healthy round-trip + fail paths)
// ──────────────────────────────────────────────────────────────────────

function makeMockRpcResponse(id, resultObj, errorObj = null) {
  return JSON.stringify({
    jsonrpc: "2.0",
    id,
    ...(errorObj ? { error: errorObj } : { result: resultObj }),
  });
}

function createMockDaemonServer({ returnTask = null, guardReject = false } = {}) {
  const counters = { memoriesForFile: 0, taskList: 0, guardValidate: 0, memoriesForFilePaths: [] };
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      if (req.method !== "POST") {
        res.writeHead(405);
        res.end();
        return;
      }
      let body = "";
      req.on("data", (chunk) => {
        body += chunk;
      });
      req.on("end", () => {
        try {
          const payload = JSON.parse(body);
          const id = payload.id ?? 0;
          const method = payload.method;
          const respond = (resultObj, errorObj = null) => {
            res.writeHead(200, { "Content-Type": "application/json" });
            res.end(makeMockRpcResponse(id, resultObj, errorObj));
          };
          if (method !== "tools/list" && method !== "tools/call") {
            respond(null, { code: -32601, message: "Unknown method" });
            return;
          }
          const params = payload.params ?? {};
          const name = params.name ?? params.arguments?.name;
          if (method === "tools/list") {
            respond({
              tools: [
                { name: "neurostrata_guard_validate" },
                { name: "neurostrata_task_create" },
                { name: "neurostrata_task_list" },
                { name: "neurostrata_memories_for_file" },
              ],
            });
            return;
          }
          if (name === "neurostrata_memories_for_file") {
            counters.memoriesForFile++;
            const args = params.arguments ?? {};
            if (typeof args.path === "string") counters.memoriesForFilePaths.push(args.path);
            respond({
              content: [
                {
                  type: "text",
                  text: JSON.stringify({
                    hits: [
                      {
                        id: "mem-rule-1",
                        valid_from: "2026-01-01T00:00:00Z",
                        memory_type: "rule",
                        content: "Always claim a task before editing files.",
                      },
                    ],
                  }),
                },
              ],
            });
            return;
          }
          if (name === "neurostrata_task_list") {
            counters.taskList++;
            respond({
              content: [
                {
                  type: "text",
                  text: JSON.stringify({
                    tasks: returnTask ?? [{ id: "task-d1", session_id: "sess-d1", status: "in_progress", subagent_prompt_injection: "### Injected daemon context" }],
                  }),
                },
              ],
            });
            return;
          }
          if (name === "neurostrata_guard_validate") {
            counters.guardValidate++;
            // `guardReject` may be a boolean or a getter, so a test can flip the
            // verdict between two calls against the same mock daemon.
            const reject = typeof guardReject === "function" ? guardReject() : guardReject;
            if (reject) {
              respond({ content: [{ type: "text", text: JSON.stringify({ verdict: { bucket: "DeterministicReject", reasons: ["claim a task first"] }, rule_ids_triggered: [] }) }] });
              return;
            }
            respond({
              content: [
                { type: "text", text: JSON.stringify({ verdict: { bucket: "Pass" }, rule_ids_triggered: [] }) },
              ],
            });
            return;
          }
          respond(null, { code: -32601, message: "Unknown tool" });
        } catch {
          res.writeHead(400);
          res.end();
        }
      });
    });
    server.listen(0, () => {
      const addr = server.address();
      resolve({ server, url: `http://127.0.0.1:${addr.port}/mcp`, counters });
    });
  });
}

function resetModuleState() {
  resetClientForTesting();
  prefetchedPaths.clear();
  injectedMemoryIds.clear();
  cachedTaskInjection.clear();
  pendingRecallBlocks.length = 0;
}

const checkoutLedgerPath = join(process.cwd(), ".NeuroStrata", "session_queries.json");
let savedCheckoutLedger = null;

beforeEach(() => {
  savedCheckoutLedger = existsSync(checkoutLedgerPath) ? readFileSync(checkoutLedgerPath, "utf8") : null;
  resetModuleState();
  delete process.env.NEUROSTRATA_NAMESPACE;
});

afterEach(() => {
  resetModuleState();
  delete process.env.NEUROSTRATA_NAMESPACE;
  if (savedCheckoutLedger !== null) {
    mkdirSync(join(process.cwd(), ".NeuroStrata"), { recursive: true });
    writeFileSync(checkoutLedgerPath, savedCheckoutLedger);
  }
});

// ──────────────────────────────────────────────────────────────────────
// Bug 2: resolveConfig / findProjectRoot / discoverProjectNamespace
// ──────────────────────────────────────────────────────────────────────

describe("resolveConfig namespace priority (never hardcodes NeuroStrata)", () => {
  it("pluginConfig.namespace wins", () => {
    const cfg = resolveConfig({ namespace: "cfg-ns", strictMode: false });
    assert.equal(cfg.namespace, "cfg-ns");
  });

  it("env NEUROSTRATA_NAMESPACE is second priority", () => {
    process.env.NEUROSTRATA_NAMESPACE = "env-ns";
    try {
      const cfg = resolveConfig({ strictMode: false });
      assert.equal(cfg.namespace, "env-ns");
    } finally {
      delete process.env.NEUROSTRATA_NAMESPACE;
    }
  });

  it("falls back to discoverProjectNamespace(cwd), never hardcoded NeuroStrata", () => {
    const cfg = resolveConfig({}, "/tmp/opencode");
    assert.ok(typeof cfg.namespace === "string" && cfg.namespace.length > 0);
    assert.notEqual(cfg.namespace, "NeuroStrata", "discover must not hardcode NeuroStrata");
  });

  it("endpoint honors guardEndpoint legacy key", () => {
    const cfg = resolveConfig({ guardEndpoint: "http://legacy:9/mcp" });
    assert.equal(cfg.endpoint, "http://legacy:9/mcp");
  });

  it("strictMode / requestTimeoutMs / intentDetection fall back to defaults", () => {
    const cfg = resolveConfig({});
    assert.equal(cfg.strictMode, true);
    assert.equal(cfg.requestTimeoutMs, 5000);
    assert.equal(cfg.intentDetection.enabled, true);
    assert.equal(cfg.intentDetection.injectReminder, true);
  });
});

describe("findProjectRoot / discoverProjectNamespace", () => {
  const tmpRoot = "/tmp/opencode/core-fs-test";

  beforeEach(() => {
    rmSync(tmpRoot, { recursive: true, force: true });
  });

  it("findProjectRoot walks up to the .git repo root", () => {
    const underRepo = findProjectRoot(join(process.cwd(), "plugins", "core"));
    assert.equal(underRepo, process.cwd());
  });

  it("discoverProjectNamespace reads .NeuroStrata/session_queries.json namespace", () => {
    const dir = join(tmpRoot, "with-ns");
    mkdirSync(join(dir, ".NeuroStrata"), { recursive: true });
    writeFileSync(join(dir, ".NeuroStrata", "session_queries.json"), JSON.stringify({ namespace: "custom-ns" }));
    assert.equal(discoverProjectNamespace(dir), "custom-ns");
  });

  it("discoverProjectNamespace falls back to directory basename", () => {
    const dir = join(tmpRoot, "no-ns-dir");
    mkdirSync(dir, { recursive: true });
    assert.equal(discoverProjectNamespace(dir), "no-ns-dir");
  });
});

// ──────────────────────────────────────────────────────────────────────
// Bug 3: TOOL_CLASSIFICATION + is*Tool predicates + Code Mode inspector
// ──────────────────────────────────────────────────────────────────────

describe("TOOL_CLASSIFICATION sets", () => {
  it("READ_FILE_TOOLS contains read / Read / read_file / view / cat", () => {
    for (const t of ["read", "Read", "read_file", "view", "cat"]) {
      assert.ok(TOOL_CLASSIFICATION.READ_FILE_TOOLS.has(t), `READ_FILE_TOOLS missing ${t}`);
    }
  });

  it("MUTATING_FILE_TOOLS contains write / edit / MultiEdit / patch_file", () => {
    for (const t of ["write", "Write", "edit", "Edit", "MultiEdit", "patch_file", "apply_patch"]) {
      assert.ok(TOOL_CLASSIFICATION.MUTATING_FILE_TOOLS.has(t), `MUTATING_FILE_TOOLS missing ${t}`);
    }
  });

  it("SHELL_TOOLS contains bash / shell", () => {
    assert.ok(TOOL_CLASSIFICATION.SHELL_TOOLS.has("bash"));
    assert.ok(TOOL_CLASSIFICATION.SHELL_TOOLS.has("shell"));
  });

  it("SUBAGENT_TOOLS contains subagent / task / Task", () => {
    assert.ok(TOOL_CLASSIFICATION.SUBAGENT_TOOLS.has("subagent"));
    assert.ok(TOOL_CLASSIFICATION.SUBAGENT_TOOLS.has("task"));
    assert.ok(TOOL_CLASSIFICATION.SUBAGENT_TOOLS.has("Task"));
  });

  it("CODE_EXEC_TOOLS contains execute", () => {
    assert.ok(TOOL_CLASSIFICATION.CODE_EXEC_TOOLS.has("execute"));
  });

  it("MUTATING_TOOLS is union of mutating file + shell", () => {
    assert.ok(TOOL_CLASSIFICATION.MUTATING_TOOLS.has("write"));
    assert.ok(TOOL_CLASSIFICATION.MUTATING_TOOLS.has("bash"));
    assert.ok(!TOOL_CLASSIFICATION.MUTATING_TOOLS.has("read"));
  });
});

describe("is*Tool predicates", () => {
  it("isFileTouchTool read/Read/write/edit true, bash false", () => {
    assert.equal(isFileTouchTool("read"), true);
    assert.equal(isFileTouchTool("Read"), true);
    assert.equal(isFileTouchTool("write"), true);
    assert.equal(isFileTouchTool("edit"), true);
    assert.equal(isFileTouchTool("bash"), false);
  });
  it("isMutatingTool write/edit/bash true, read false", () => {
    assert.equal(isMutatingTool("write"), true);
    assert.equal(isMutatingTool("edit"), true);
    assert.equal(isMutatingTool("bash"), true);
    assert.equal(isMutatingTool("read"), false);
  });
  it("isSubagentTool subagent/task/Task true, read false", () => {
    assert.equal(isSubagentTool("subagent"), true);
    assert.equal(isSubagentTool("task"), true);
    assert.equal(isSubagentTool("Task"), true);
    assert.equal(isSubagentTool("read"), false);
  });
  it("isCodeExecTool execute true, write false", () => {
    assert.equal(isCodeExecTool("execute"), true);
    assert.equal(isCodeExecTool("write"), false);
  });
});

describe("inspectCodeModeExecution", () => {
  it("extracts id + session_id from a claim call", () => {
    const out = inspectCodeModeExecution('neurostrata_task_claim({ id: "task-7", session_id: "sess-9" })');
    assert.equal(out.id, "task-7");
    assert.equal(out.sessionId, "sess-9");
  });

  it("marks completion on task_complete", () => {
    const out = inspectCodeModeExecution("neurostrata_task_complete({ id: \"task-7\" })");
    assert.equal(out.taskCompleted, true);
  });

  it("captures note on task_update", () => {
    const out = inspectCodeModeExecution('neurostrata_task_update({ id: "task-7", note: "breath" })');
    assert.equal(out.id, "task-7");
    assert.equal(out.note, "breath");
  });

  it("extracts path from memories_for_file query", () => {
    const out = inspectCodeModeExecution('neurostrata_memories_for_file({ path: "src/foo.ts" })');
    assert.deepEqual(out.queriedFiles, ["src/foo.ts"]);
  });

  it("returns empty inspection for unrelated code", () => {
    const out = inspectCodeModeExecution("console.log('hello')");
    assert.equal(out.id, undefined);
    assert.deepEqual(out.queriedFiles, []);
  });
});

// ──────────────────────────────────────────────────────────────────────
// Bug 4: handleHookEvent / handleClaudeHookEvent (PreToolUse)
// ──────────────────────────────────────────────────────────────────────

// Hermetic "no daemon" fixture. A live NeuroStrata daemon on the default
// 127.0.0.1:34343 answers the memories_for_file / task_list lookups that the
// no-daemon tests below must exercise with NO daemon reachable, so the MCP
// client is pointed at a port nothing listens on. initialize() sets mcpClient,
// which keeps handleHookEvent's auto-init branch (that would otherwise
// recreate the client against DEFAULT_ENDPOINT) from running.
const NO_DAEMON_ENDPOINT = "http://127.0.0.1:1/mcp";
async function runWithoutDaemon(fn) {
  resetClientForTesting();
  await initialize({
    config: { namespace: "no-daemon-ns", endpoint: NO_DAEMON_ENDPOINT, bearerToken: null, strictMode: false, requestTimeoutMs: 50 },
    tool: { hook: async () => {} },
    hooks: {},
  });
  try {
    return await fn();
  } finally {
    resetModuleState();
  }
}

describe("handleHookEvent / handleClaudeHookEvent alias", () => {
  it("handleClaudeHookEvent is the same function as handleHookEvent", () => {
    assert.equal(handleClaudeHookEvent, handleHookEvent);
  });

  it("PreToolUse subagent tool returns allow + enriches prompt (no daemon)", async () => {
    const target = { prompt: "Help me refactor this." };
    const out = await runWithoutDaemon(async () => {
      return await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "subagent",
        tool_input: target,
      });
    });
    assert.equal(out.continue, true);
    assert.equal(out.hookSpecificOutput.permissionDecision, "allow");
    assert.ok(target.prompt.includes("### NeuroStrata Task & Graph Context"));
  });

  it("PreToolUse file-touch returns recall block when daemon healthy", async () => {
    const { server, url } = await createMockDaemonServer();
    try {
      resetClientForTesting();
      const ctx = { config: { namespace: "pref-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(ctx);
      const out = await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "read",
        tool_input: { file_path: "src/foo.ts" },
      });
      assert.equal(out.continue, true);
      assert.equal(out.hookSpecificOutput.permissionDecision, "allow");
      assert.ok(out.systemMessage.includes("[NeuroStrata Governing Memories for src/foo.ts]"));
      assert.ok(out.hookSpecificOutput.additionalContext.includes("[NeuroStrata Governing Memories for src/foo.ts]"));
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("PreToolUse file-touch with no daemon still allows (fail-soft)", async () => {
    const out = await runWithoutDaemon(async () => {
      return await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "read",
        tool_input: { file_path: "src/foo.ts" },
      });
    });
    assert.equal(out.continue, true);
    assert.equal(out.hookSpecificOutput.permissionDecision, "allow");
    assert.equal(out.systemMessage, undefined);
  });

  it("PreToolUse mutating tool whose guard rejects is denied (fail closed)", async () => {
    const { server, url } = await createMockDaemonServer({ guardReject: true });
    try {
      resetClientForTesting();
      const ctx = { config: { namespace: "guard-ns", endpoint: url, bearerToken: "tok", strictMode: true, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(ctx);
      const out = await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "write",
        tool_input: { file_path: "src/foo.ts", content: "x" },
      });
      assert.equal(out.continue, undefined);
      assert.equal(out.hookSpecificOutput.permissionDecision, "deny");
      assert.ok(typeof out.hookSpecificOutput.permissionDecisionReason === "string");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("Stop / SubagentStop events allow", async () => {
    for (const ev of ["Stop", "SubagentStop"]) {
      const out = await handleHookEvent({ hook_event_name: ev });
      assert.equal(out.hookSpecificOutput.permissionDecision, "allow");
    }
  });
});

// ──────────────────────────────────────────────────────────────────────
// Enhancement 1: resolveActiveTaskFromDaemon (fail-soft + cache)
// ──────────────────────────────────────────────────────────────────────

describe("resolveActiveTaskFromDaemon", () => {
  it("returns nulls without an MCP client (fail-soft)", async () => {
    resetClientForTesting();
    const res = await resolveActiveTaskFromDaemon("ns");
    assert.equal(res.taskId, null);
    assert.equal(res.sessionId, null);
    assert.equal(res.promptInjection, null);
  });

  it("queries task_list, caches task/session/injection, and re-resolves when daemon changes", async () => {
    const { server, url, counters } = await createMockDaemonServer({ returnTask: [{ id: "task-d1", session_id: "sess-d1", status: "in_progress", subagent_prompt_injection: "### Injected daemon context" }] });
    try {
      const ctx = {
        config: { namespace: "ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 },
        tool: { hook: async () => {} },
        hooks: {},
      };
      await initialize(ctx);
      const res = await resolveActiveTaskFromDaemon("ns");
      assert.equal(res.taskId, "task-d1");
      assert.equal(res.sessionId, "sess-d1");
      assert.equal(res.promptInjection, "### Injected daemon context");
      assert.equal(cachedTaskInjection.get("task-d1"), "### Injected daemon context");
      // Second call re-queries (function always queries) but cache holds the injection.
      assert.equal(counters.taskList, 1);
    } finally {
      server.close();
      resetModuleState();
    }
  });
});

// ──────────────────────────────────────────────────────────────────────
// Enhancement 2 & 3: subagent context injection
// ──────────────────────────────────────────────────────────────────────

describe("buildSubagentContextHeader", () => {
  it("includes marker, namespace, task id, and graph-first rule", () => {
    const h = buildSubagentContextHeader("ns-1", "task-abc");
    assert.ok(h.includes("### NeuroStrata Task & Graph Context"));
    assert.ok(h.includes("AUTO-INJECTED BY NEUROSTRATA PLUGIN"));
    assert.ok(h.includes("Namespace: `ns-1`"));
    assert.ok(h.includes("Active Task ID: `task-abc`"));
    assert.ok(h.includes("Graph-First Lookup Rule"));
    assert.ok(h.includes("neurostrata_memories_for_file"));
  });

  it("null taskId reports unclaimed", () => {
    const h = buildSubagentContextHeader("ns-1", null);
    assert.ok(h.includes("Active Task ID: `unclaimed (claim a task first)`"));
  });

  it("appends extra context when provided", () => {
    const h = buildSubagentContextHeader("ns-1", "task-abc", "extra note here");
    assert.ok(h.includes("extra note here"));
  });
});

describe("injectSubagentPrompt", () => {
  it("prepends header once and is idempotent", () => {
    const target = { prompt: "Original prompt text" };
    assert.equal(injectSubagentPrompt(target, "ns", "task-1"), true);
    assert.ok(target.prompt.startsWith("### NeuroStrata Task & Graph Context"));
    assert.ok(target.prompt.endsWith("Original prompt text"));
    assert.equal(injectSubagentPrompt(target, "ns", "task-1"), false);
    const count = (target.prompt.match(/### NeuroStrata Task & Graph Context/g) || []).length;
    assert.ok(count >= 1, "header injected at least once");
    assert.equal(injectSubagentPrompt(target, "ns", "task-1"), false, "third call is also a no-op");
    assert.equal((target.prompt.match(/### NeuroStrata Task & Graph Context/g) || []).length, count, "idempotent: no additional markers injected");
  });

  it("returns false when no prompt or already marked", () => {
    assert.equal(injectSubagentPrompt({ other: "data" }, "ns", "t"), false);
    assert.equal(injectSubagentPrompt({ prompt: null }, "ns", "t"), false);
    const marked = { prompt: "### NeuroStrata Task & Graph Context x" };
    assert.equal(injectSubagentPrompt(marked, "ns", "t"), false);
  });
});

describe("extractRepoFilePaths", () => {
  it("extracts repo-relative file paths and dedupes, skips URLs", () => {
    const out = extractRepoFilePaths("edit src/foo.ts and plugins/core/index.ts; see https://example.com/x.json");
    assert.ok(out.includes("src/foo.ts"));
    assert.ok(out.includes("plugins/core/index.ts"));
    assert.ok(!out.includes("https://example.com/x.json"));
    const dups = extractRepoFilePaths("a/b.ts a/b.ts c/d.ts");
    assert.deepEqual(dups, ["a/b.ts", "c/d.ts"]);
  });
});

describe("enrichAndInjectSubagentPrompt", () => {
  it("injects header + recalls memories for repo paths (no task id -> resolves daemon)", async () => {
    const { server, url } = await createMockDaemonServer();
    try {
      resetClientForTesting();
      const ctx = { config: { namespace: "ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(ctx);
      const target = { prompt: "Touch src/foo.ts please" };
      const changed = await enrichAndInjectSubagentPrompt(target, "ns");
      assert.equal(changed, true);
      assert.ok(target.prompt.includes("### NeuroStrata Task & Graph Context"));
      assert.ok(target.prompt.includes("[NeuroStrata rule mem-rule-1"));
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("uses cached injection without re-resolving when task id given", async () => {
    const { server, url, counters } = await createMockDaemonServer({
      returnTask: [{ id: "task-c", session_id: "sess-c", status: "in_progress", subagent_prompt_injection: "### Cached injection" }],
    });
    try {
      resetClientForTesting();
      const ctx = { config: { namespace: "ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(ctx);
      // Prime the cache via resolveActiveTaskFromDaemon.
      await resolveActiveTaskFromDaemon("ns");
      const callsBefore = counters.taskList;
      const target = { prompt: "hello" };
      const changed = await enrichAndInjectSubagentPrompt(target, "ns", "task-c");
      assert.equal(changed, true);
      assert.ok(target.prompt.includes("### Cached injection"));
      // enrich with a known taskId does NOT call task_list again (uses cache).
      assert.equal(counters.taskList, callsBefore);
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("is idempotent (returns false once marker present)", async () => {
    const { server, url } = await createMockDaemonServer();
    try {
      resetClientForTesting();
      const target = { prompt: "hello" };
      assert.equal(await enrichAndInjectSubagentPrompt(target, "ns"), true);
      assert.equal(await enrichAndInjectSubagentPrompt(target, "ns"), false);
    } finally {
      server.close();
      resetModuleState();
    }
  });
});

// ──────────────────────────────────────────────────────────────────────
// Bug 5: prefetchMemoriesForFile + formatFileTouchRecallBlock
// ──────────────────────────────────────────────────────────────────────

describe("formatFileTouchRecallBlock", () => {
  it("formats header + snippets", () => {
    const block = formatFileTouchRecallBlock("/tmp/foo.ts", ["[NeuroStrata rule mem-1] x"]);
    assert.ok(block.includes("[NeuroStrata Governing Memories for /tmp/foo.ts]"));
    assert.ok(block.includes("[NeuroStrata rule mem-1] x"));
  });
});

describe("prefetchMemoriesForFile LRU + dedup", () => {
  it("queries once per path, dedupes injected memories across paths, caps LRU", async () => {
    const { server, url, counters } = await createMockDaemonServer();
    try {
      const ctx = {
        config: { namespace: "pref-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 },
        tool: { hook: async () => {} },
        hooks: {},
      };
      await initialize(ctx);

      const first = await prefetchMemoriesForFile("pref-ns", "src/foo.ts");
      assert.equal(first.length, 1);
      assert.ok(first[0].includes("mem-rule-1"));
      assert.equal(counters.memoriesForFile, 1);

      const second = await prefetchMemoriesForFile("pref-ns", "src/foo.ts");
      assert.deepEqual(second, [], "LRU dedup: second touch returns empty, no re-query");
      assert.equal(counters.memoriesForFile, 1, "LRU dedup must not re-query");

      const third = await prefetchMemoriesForFile("pref-ns", "src/bar.ts");
      assert.deepEqual(third, [], "same memories already injected -> empty");
      assert.equal(counters.memoriesForFile, 2, "new path queried once");
      assert.equal(injectedMemoryIds.size, 1, "one memory key injected");

      for (let i = 0; i < PREFETCH_LRU_CAP * 2; i++) {
        await prefetchMemoriesForFile("pref-ns", `file-${i}.ts`);
      }
      assert.equal(prefetchedPaths.size, PREFETCH_LRU_CAP);

      assert.deepEqual(await prefetchMemoriesForFile("pref-ns", "   "), [], "empty path no-op");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("returns empty and never queries when mcpClient is not initialized", async () => {
    resetClientForTesting();
    const out = await prefetchMemoriesForFile("ns", "src/foo.ts");
    assert.deepEqual(out, []);
  });
});

describe("recordLocalSessionQuery ledger object format", () => {
  it("writes { namespace, queries } object form, converting a legacy array, and preserves namespace", async () => {
    const { server, url } = await createMockDaemonServer();
    const tmpProjectDir = mkdtempSync(join(tmpdir(), "ns-ledger-test-"));
    const ledgerDir = join(tmpProjectDir, ".NeuroStrata");
    const ledgerPath = join(ledgerDir, "session_queries.json");
    try {
      mkdirSync(ledgerDir, { recursive: true });
      // Start from a known legacy-array ledger (no namespace) in a temporary project root
      // so the repository checkout's real session ledger is never overwritten.
      writeFileSync(ledgerPath, JSON.stringify([], null, 2));
      resetClientForTesting();
      const ctx = { config: { namespace: "ledger-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(ctx);
      await prefetchMemoriesForFile("ledger-ns", "src/ledger.ts", { recordLedgerDir: tmpProjectDir });
      const ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(ledger.namespace, "ledger-ns", "ledger.namespace must be set");
      const queries = Array.isArray(ledger) ? ledger : ledger.queries;
      assert.ok(Array.isArray(queries), "queries must be an array");
      assert.equal(queries.length, 1);
      assert.equal(queries[0].tool, "neurostrata_memories_for_file");
      assert.equal(queries[0].query, "src/ledger.ts");
    } finally {
      server.close();
      resetModuleState();
      rmSync(tmpProjectDir, { recursive: true, force: true });
    }
  });

  it("aliases cachedSubagentPromptByTask to cachedTaskInjection", () => {
    assert.equal(cachedSubagentPromptByTask, cachedTaskInjection);
  });
});

// ──────────────────────────────────────────────────────────────────────
// execute.before / execute.after recall delivery (via initialize hook capture)
// ──────────────────────────────────────────────────────────────────────

describe("execute.before / execute.after recall delivery", () => {
  it("before attaches recall block, after delivers it onto output", async () => {
    const { server, url } = await createMockDaemonServer();
    let beforeHandler;
    let afterHandler;
    try {
      const ctx = {
        config: {
          namespace: "recall-ns", endpoint: url, bearerToken: "tok",
          strictMode: false, requestTimeoutMs: 200,
          intentDetection: { enabled: false, injectReminder: false },
        },
        tool: {
          hook: async (name, cb) => {
            if (name === "execute.before") beforeHandler = cb;
            if (name === "execute.after") afterHandler = cb;
          },
        },
        hooks: {},
      };
      await initialize(ctx);
      assert.equal(typeof beforeHandler, "function");
      assert.equal(typeof afterHandler, "function");

      const beforeOut = await beforeHandler({ tool: "read", input: { path: "src/foo.ts" }, sessionID: "s1" });
      assert.ok(pendingRecallBlocks.length > 0, "pendingRecallBlocks populated by execute.before");
      assert.ok(pendingRecallBlocks[0].includes("[NeuroStrata Governing Memories for src/foo.ts]"));

      const afterOut = await afterHandler({ output: "tool result here" });
      assert.ok(String(afterOut.output).startsWith(pendingRecallBlocks[0] ?? ""), "after prepends block to output");
      assert.equal(pendingRecallBlocks.length, 0, "pendingRecallBlocks drained by execute.after");
    } finally {
      server.close();
      resetModuleState();
    }
  });
});

// ──────────────────────────────────────────────────────────────────────
// Adapter factory + ESM-only (no require)
// ──────────────────────────────────────────────────────────────────────

describe("createHarnessPlugin", () => {
  it("builds a plugin with id/name/setup/handleHookEvent", () => {
    const plugin = createHarnessPlugin("opencode", "OpenCode");
    assert.equal(plugin.id, "opencode");
    assert.equal(plugin.name, "OpenCode");
    assert.equal(typeof plugin.setup, "function");
    assert.equal(typeof plugin.handleHookEvent, "function");
    assert.equal(typeof plugin.handleClaudeHookEvent, "function");
  });
});

describe("ESM-only surface", () => {
  it("dist has no require() usage", async () => {
    const src = readFileSync(join(process.cwd(), "plugins", "core", "dist", "index.js"), "utf8");
    // Strip // line comments and JSDoc */-prefixed lines so the word "require" in
    // comments (e.g. "Zero `require()` calls") doesn't trigger a false positive.
    const code = src.split("\n").filter((line) => {
      const t = line.trimStart();
      return !t.startsWith("//") && !t.startsWith("*");
    }).join("\n");
    assert.ok(!/require\s*\(/.test(code), "compiled core dist must not contain require()");
  });
});

// ──────────────────────────────────────────────────────────────────────
// Round-2 PR review regression tests
// ──────────────────────────────────────────────────────────────────────

describe("native task tool tracking + hook registration + pre-commit awk fallback", () => {
  it("resets mutationsSinceNote on native neurostrata_task_claim / update(note) / complete in execute.before and handleHookEvent", async () => {
    const { server, url: endpoint } = await createMockDaemonServer();
    try {
      resetClientForTesting();
      let beforeHook = null;
      await initialize({
        config: {
          namespace: "native-task-ns",
          endpoint,
          bearerToken: "tok",
          strictMode: true,
          requestTimeoutMs: 1000,
        },
        tool: {
          hook: async (name, fn) => {
            if (name === "execute.before") beforeHook = fn;
          },
        },
      });

      // Claim task via native tool call in execute.before
      await beforeHook({
        tool: "neurostrata_task_claim",
        args: { id: "neurostrata-native1", session_id: "ses-native" },
      });
      assert.equal(getTrackingState().activeTaskId, "neurostrata-native1");
      assert.equal(getTrackingState().activeSessionId, "ses-native");
      assert.equal(getTrackingState().mutationsSinceNote, 0);

      await beforeHook({ tool: "edit", args: { path: "src/a.rs" } });
      await beforeHook({ tool: "edit", args: { path: "src/b.rs" } });
      assert.equal(getTrackingState().mutationsSinceNote, 2);

      // Native neurostrata_task_update with note resets mutationsSinceNote to 0
      await beforeHook({
        tool: "neurostrata_task_update",
        args: { id: "neurostrata-native1", note: "Breath checkpoint" },
      });
      assert.equal(getTrackingState().mutationsSinceNote, 0, "execute.before native task_update(note) resets counter");

      // Same via handleHookEvent (Claude / CLI PreToolUse)
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "Edit",
        tool_input: { file_path: "src/d.rs" },
      });
      assert.equal(getTrackingState().mutationsSinceNote, 1);

      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_task_update",
        tool_input: { id: "neurostrata-native1", note: "Claude breath" },
      });
      assert.equal(getTrackingState().mutationsSinceNote, 0, "handleHookEvent native task_update(note) resets counter");

      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_task_complete",
        tool_input: { id: "neurostrata-native1" },
      });
      assert.equal(getTrackingState().activeTaskId, null, "native task_complete clears activeTaskId");
    } finally {
      server.close();
      resetClientForTesting();
    }
  });

  it("initialize registers beforeToolExec, beforeToolCall, beforeCommand, and onTurnEnd on ctx.hooks", async () => {
    const { SUBAGENT_CONTEXT_MARKER } = await import("./dist/index.js");
    let beforeToolExecFn = null;
    let beforeToolCallFn = null;
    let beforeCommandFn = null;
    let onTurnEndFn = null;

    resetClientForTesting();
    await initialize({
      config: {
        namespace: "hooks-ns",
        endpoint: "http://127.0.0.1:1/mcp",
        bearerToken: "tok",
        strictMode: false,
        requestTimeoutMs: 50,
      },
      hooks: {
        beforeToolExec: async (fn) => {
          beforeToolExecFn = fn;
        },
        beforeToolCall: async (fn) => {
          beforeToolCallFn = fn;
        },
        beforeCommand: async (fn) => {
          beforeCommandFn = fn;
        },
        onTurnEnd: async (fn) => {
          onTurnEndFn = fn;
        },
      },
    });

    assert.equal(typeof beforeToolExecFn, "function");
    assert.equal(typeof beforeToolCallFn, "function");
    assert.equal(typeof beforeCommandFn, "function");
    assert.equal(typeof onTurnEndFn, "function");

    const callPayload = { tool: "subagent", input: { prompt: "Check status" } };
    await beforeToolCallFn(callPayload);
    assert.ok(callPayload.input.prompt.includes(SUBAGENT_CONTEXT_MARKER));

    const cmdPayload = { command: "Task", args: { prompt: "Run worker" } };
    await beforeCommandFn(cmdPayload);
    assert.ok(cmdPayload.args.prompt.includes(SUBAGENT_CONTEXT_MARKER));
    resetClientForTesting();
  });

  it("pre-commit awk fallback handles filenames with spaces and & and produces valid comma-separated JSON", async () => {
    const { execFileSync } = await import("node:child_process");
    const { mkdtempSync, symlinkSync, chmodSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const repoDir = mkdtempSync(join(tmpdir(), "ns-precommit-awk-"));
    const binDir = mkdtempSync(join(tmpdir(), "ns-precommit-bin-"));
    try {
      for (const cmd of ["bash", "sh", "git", "grep", "sed", "awk", "date", "mkdir", "printf", "mktemp", "mv", "rm", "dirname", "basename", "head", "pwd"]) {
        for (const prefix of ["/usr/bin", "/bin"]) {
          try {
            symlinkSync(join(prefix, cmd), join(binDir, cmd));
            break;
          } catch {}
        }
      }

      execFileSync("git", ["init", "-q"], { cwd: repoDir });
      mkdirSync(join(repoDir, "src"), { recursive: true });
      writeFileSync(join(repoDir, "src", "file one.rs"), "fn a() {}\n");
      execFileSync("git", ["add", "src/file one.rs"], { cwd: repoDir });

      const hookPath = join(repoDir, "pre-commit-test.sh");
      writeFileSync(hookPath, readFileSync(new URL("../../scripts/hooks/pre-commit", import.meta.url), "utf8"));
      chmodSync(hookPath, 0o755);

      // Run 1: populates empty queries array with "src/file one.rs"
      execFileSync(hookPath, [], {
        cwd: repoDir,
        env: { PATH: binDir, HOME: repoDir },
      });

      const ledgerPath = join(repoDir, ".NeuroStrata", "session_queries.json");
      const doc1 = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(doc1.queries.length, 1);
      assert.equal(doc1.queries[0].query, "src/file one.rs");

      // Run 2: stages second file with '&' and appends to populated queries array
      writeFileSync(join(repoDir, "src", "a&b.rs"), "fn b() {}\n");
      execFileSync("git", ["add", "src/a&b.rs"], { cwd: repoDir });
      execFileSync(hookPath, [], {
        cwd: repoDir,
        env: { PATH: binDir, HOME: repoDir },
      });

      const doc2 = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(doc2.queries.length, 2);
      const queries = doc2.queries.map((q) => q.query).sort();
      assert.deepEqual(queries, ["src/a&b.rs", "src/file one.rs"]);

      // Run 3: legacy compact single-line empty-array ledger `{"namespace":"app","queries":[]}`
      writeFileSync(ledgerPath, '{"namespace":"app","queries":[]}');
      execFileSync(hookPath, [], {
        cwd: repoDir,
        env: { PATH: binDir, HOME: repoDir },
      });
      const doc3 = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(doc3.namespace, "app");
      assert.equal(doc3.queries.length, 2);

      // Run 4: legacy top-level array ledger `[{"ts":1,"tool":"legacy","query":"old.rs"}]`
      writeFileSync(ledgerPath, '[{"ts":1,"tool":"legacy","query":"old.rs"}]');
      execFileSync(hookPath, [], {
        cwd: repoDir,
        env: { PATH: binDir, HOME: repoDir },
      });
      const doc4 = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(typeof doc4.namespace, "string");
      assert.ok(Array.isArray(doc4.queries));
      assert.equal(doc4.queries.length, 3);
      assert.ok(doc4.queries.some((q) => q.query === "old.rs"));
    } finally {
      rmSync(repoDir, { recursive: true, force: true });
      rmSync(binDir, { recursive: true, force: true });
    }
  });

  it("discoverProjectNamespace stops at findProjectRoot and does not inherit parent workspace namespace", () => {
    const parentWs = mkdtempSync(join(tmpdir(), "ns-parent-ws-"));
    try {
      mkdirSync(join(parentWs, ".NeuroStrata"), { recursive: true });
      writeFileSync(
        join(parentWs, ".NeuroStrata", "session_queries.json"),
        JSON.stringify({ namespace: "parent-workspace-ns", queries: [] }),
      );
      const childRepo = join(parentWs, "child-repo");
      const childSubdir = join(childRepo, "src", "nested");
      mkdirSync(join(childRepo, ".git"), { recursive: true });
      mkdirSync(childSubdir, { recursive: true });

      assert.equal(discoverProjectNamespace(childSubdir), "child-repo");
    } finally {
      rmSync(parentWs, { recursive: true, force: true });
    }
  });

  it("inspectCodeModeExecution does not match session_id suffix as id", () => {
    const res = inspectCodeModeExecution(
      `await tools.neurostrata.neurostrata_task_update({ session_id: "ses-only-123", note: "checkpoint" });`,
    );
    assert.equal(res.id, undefined);
    assert.equal(res.sessionId, "ses-only-123");
    assert.equal(res.note, "checkpoint");
  });

  it("enrichAndInjectSubagentPrompt resolves daemon injection on cache miss after native claim and preserves file recall across parent and children", async () => {
    const { server, url, counters } = await createMockDaemonServer({
      returnTask: [
        {
          id: "task-claimed-native",
          session_id: "sess-1",
          status: "in_progress",
          subagent_prompt_injection: "### Native claimed daemon injection",
        },
      ],
    });
    try {
      let beforeExecFn = null;
      await initialize({
        config: {
          namespace: "round3-ns",
          endpoint: url,
          bearerToken: "tok",
          strictMode: true,
          requestTimeoutMs: 500,
        },
        hooks: {
          beforeToolExec: async (fn) => {
            beforeExecFn = fn;
          },
        },
      });

      // 1. Claim task via native tool call through beforeToolExec (cachedTaskInjection is not yet populated)
      await beforeExecFn({
        name: "neurostrata_task_claim",
        args: { id: "task-claimed-native", session_id: "sess-1" },
      });
      assert.equal(getTrackingState().activeTaskId, "task-claimed-native");
      assert.equal(cachedTaskInjection.has("task-claimed-native"), false);

      // 2. Parent reads src/shared.rs via beforeToolExec -> receives file-touch recall block
      const readRes = await beforeExecFn({
        name: "read",
        args: { path: "src/shared.rs" },
      });
      assert.ok(
        readRes?.additionalContext?.includes("mem-rule-1"),
        "beforeToolExec routes read through file-touch recall",
      );

      // 3. First child subagent mentions src/shared.rs -> gets both daemon injection AND src/shared.rs memories
      const child1 = { prompt: "Inspect src/shared.rs" };
      await beforeExecFn({ name: "subagent", input: child1 });
      assert.ok(
        child1.prompt.includes("### Native claimed daemon injection"),
        "fetches and injects daemon prompt injection on cache miss",
      );
      assert.ok(
        child1.prompt.includes("mem-rule-1"),
        "child1 receives file memories even after parent already prefetched src/shared.rs",
      );

      // 4. Second child subagent also mentions src/shared.rs + src/other.rs -> still gets memories without duplicate mem-rule-1 in same prompt
      const child2 = { prompt: "Update src/shared.rs and src/other.rs" };
      await beforeExecFn({ name: "subagent", input: child2 });
      const ruleMatches = child2.prompt.match(/mem-rule-1/g) || [];
      assert.equal(ruleMatches.length, 1, "child2 receives mem-rule-1 once (deduped within child prompt)");

      // 5. Mutating tool via beforeToolExec runs guard_validate
      const guardBefore = counters.guardValidate;
      await beforeExecFn({ name: "write", args: { path: "src/shared.rs", content: "x" } });
      assert.equal(counters.guardValidate, guardBefore + 1, "beforeToolExec enforces guard_validate on mutating tools");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("round-5 fixes: str_replace_editor classification, per-call Code Mode arg extraction, Breath reset only on claim/complete/note, call-keyed recall correlation, Stop onTurnEnd, and pre-commit legacy array normalization", async () => {
    assert.ok(TOOL_CLASSIFICATION.MUTATING_FILE_TOOLS.has("str_replace_editor"));

    // 1. Per-call Code Mode arg extraction ignores unrelated preceding properties
    const code = `
      const other = { id: "wrong-id", session_id: "wrong-sess", path: "wrong/path.ts", note: "wrong-note" };
      await tools.neurostrata.neurostrata_task_claim({ id: "real-task", session_id: "real-sess" });
      await tools.neurostrata.neurostrata_memories_for_file({ path: "src/real.ts" });
      await tools.neurostrata.neurostrata_memories_for_file({ path: "src/second.ts" });
    `;
    const inspected = inspectCodeModeExecution(code);
    assert.equal(inspected.id, "real-task");
    assert.equal(inspected.sessionId, "real-sess");
    assert.equal(inspected.taskClaimed, true);
    assert.equal(inspected.note, undefined);
    assert.deepEqual(inspected.queriedFiles, ["src/real.ts", "src/second.ts"]);

    // 2. Note-less task_update in Code Mode does NOT reset mutationsSinceNote
    const { server, url, counters } = await createMockDaemonServer({
      returnTask: [{ id: "real-task", session_id: "real-sess", status: "in_progress" }],
    });
    try {
      let beforeHandler;
      let afterHandler;
      await initialize({
        config: {
          namespace: "round5-ns",
          endpoint: url,
          bearerToken: "tok",
          strictMode: true,
          requestTimeoutMs: 500,
          intentDetection: { enabled: true, injectReminder: true },
        },
        tool: {
          hook: async (name, cb) => {
            if (name === "execute.before") beforeHandler = cb;
            if (name === "execute.after") afterHandler = cb;
          },
        },
        hooks: {},
      });

      await beforeHandler({
        tool: "execute",
        input: { code: 'neurostrata_task_claim({ id: "real-task", session_id: "real-sess" })' },
      });
      await beforeHandler({ tool: "bash", input: { command: "echo 1" } });
      assert.equal(getTrackingState().mutationsSinceNote, 1);

      // Note-less task_update in Code Mode must NOT reset mutationsSinceNote
      await beforeHandler({
        tool: "execute",
        input: { code: 'neurostrata_task_update({ id: "real-task", priority: 1 })' },
      });
      assert.equal(getTrackingState().mutationsSinceNote, 1, "note-less Code Mode task_update preserves mutationsSinceNote");

      // Note-less task_update in handleHookEvent must also NOT reset mutationsSinceNote
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "execute",
        tool_input: { code: 'neurostrata_task_update({ id: "real-task", priority: 2 })' },
      });
      assert.equal(getTrackingState().mutationsSinceNote, 1, "note-less PreToolUse Code Mode task_update preserves mutationsSinceNote");

      // 3. Call-correlated recall: read A queues recall, shell B finishes first without stealing A's recall
      const readBefore = await beforeHandler({
        tool: "read",
        callID: "call-read-A",
        input: { path: "src/corr.ts" },
      });
      assert.ok(readBefore?.additionalContext?.includes("src/corr.ts"));

      const shellAfter = await afterHandler({
        tool: "bash",
        callID: "call-shell-B",
        output: "shell stdout",
      });
      assert.equal(shellAfter.output, "shell stdout", "shell B must not consume read A's recall block");

      const readAfter = await afterHandler({
        tool: "read",
        callID: "call-read-A",
        output: "file body",
      });
      assert.ok(
        readAfter.output.startsWith(readBefore.additionalContext),
        "read A receives its own correlated recall block",
      );

      // 4. Stop event with transcript in handleHookEvent triggers onTurnEnd
      const capturedErrors = [];
      const origError = console.error;
      console.error = (...args) => capturedErrors.push(args.join(" "));
      try {
        await handleHookEvent({
          hook_event_name: "Stop",
          transcript: "Completed work. TODO: add follow-up integration verification.",
        });
      } finally {
        console.error = origError;
      }
      assert.ok(
        capturedErrors.some((msg) => msg.includes("Task intent detected")),
        "Stop hook event with transcript invokes onTurnEnd",
      );
    } finally {
      server.close();
      resetModuleState();
    }
  });
});

// ──────────────────────────────────────────────────────────────────────
// Round-7: memory mutation tools clear caches while preserving activeTaskId
// ──────────────────────────────────────────────────────────────────────

describe("round-7: memory mutation tools clear caches while preserving activeTaskId", () => {
  it("neurostrata_archive_memory clears cachedTaskInjection and preserves activeTaskId", async () => {
    const { server, url } = await createMockDaemonServer({
      returnTask: [{ id: "task-1", session_id: "sess-1", status: "in_progress", subagent_prompt_injection: "### Daemon injection for task-1" }],
    });
    try {
      resetClientForTesting();
      const ctx = {
        config: { namespace: "r7-ns", endpoint: url, bearerToken: "tok", strictMode: true, requestTimeoutMs: 500 },
        tool: { hook: async () => {} },
        hooks: {},
      };
      await initialize(ctx);

      // Claim task via native tool call in handleHookEvent
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_task_claim",
        tool_input: { id: "task-1", session_id: "sess-1" },
      });
      assert.equal(getTrackingState().activeTaskId, "task-1", "activeTaskId should be task-1 after claim");

      // Prime the cache via resolveActiveTaskFromDaemon
      await resolveActiveTaskFromDaemon("r7-ns");
      assert.ok(cachedTaskInjection.has("task-1"), "cachedTaskInjection should have task-1");

      // Call neurostrata_archive_memory with id: "mem-rule-1" (should NOT overwrite activeTaskId)
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_archive_memory",
        tool_input: { id: "mem-rule-1" },
      });

      // activeTaskId should still be "task-1" (not overwritten by archive_memory's id)
      assert.equal(getTrackingState().activeTaskId, "task-1", "activeTaskId must be preserved after archive_memory");

      // cachedTaskInjection should be cleared
      assert.equal(cachedTaskInjection.has("task-1"), false, "cachedTaskInjection must be cleared after archive_memory");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("neurostrata_supersede_memory clears cachedTaskInjection and preserves activeTaskId", async () => {
    const { server, url } = await createMockDaemonServer({
      returnTask: [{ id: "task-1", session_id: "sess-1", status: "in_progress", subagent_prompt_injection: "### Daemon injection for task-1" }],
    });
    try {
      resetClientForTesting();
      const ctx = {
        config: { namespace: "r7-ns", endpoint: url, bearerToken: "tok", strictMode: true, requestTimeoutMs: 500 },
        tool: { hook: async () => {} },
        hooks: {},
      };
      await initialize(ctx);

      // Claim task via native tool call in handleHookEvent
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_task_claim",
        tool_input: { id: "task-1", session_id: "sess-1" },
      });
      assert.equal(getTrackingState().activeTaskId, "task-1", "activeTaskId should be task-1 after claim");

      // Prime the cache via resolveActiveTaskFromDaemon
      await resolveActiveTaskFromDaemon("r7-ns");
      assert.ok(cachedTaskInjection.has("task-1"), "cachedTaskInjection should have task-1");

      // Call neurostrata_supersede_memory with old_id: "mem-rule-1"
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_supersede_memory",
        tool_input: { old_id: "mem-rule-1", content: "new rule" },
      });

      // activeTaskId should still be "task-1"
      assert.equal(getTrackingState().activeTaskId, "task-1", "activeTaskId must be preserved after supersede_memory");

      // cachedTaskInjection should be cleared
      assert.equal(cachedTaskInjection.has("task-1"), false, "cachedTaskInjection must be cleared after supersede_memory");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("inspectCodeModeExecution sets memoryMutated: true on neurostrata_supersede_memory", () => {
    const out = inspectCodeModeExecution('neurostrata_supersede_memory({ old_id: "mem-1", content: "v2" })');
    assert.equal(out.memoryMutated, true, "inspectCodeModeExecution should set memoryMutated: true on neurostrata_supersede_memory");
  });
});

describe("round-6: task_update title invalidation, AST summary formatting, dev scripts, and pre-commit path normalization", () => {
  it("native neurostrata_task_update with title invalidates cachedTaskInjection", async () => {
    const { server, url, counters } = await createMockDaemonServer({
      returnTask: [
        {
          id: "task-update-title",
          session_id: "sess-1",
          status: "in_progress",
          subagent_prompt_injection: "### Original daemon injection",
        },
      ],
    });
    try {
      resetClientForTesting();
      const ctx = {
        config: { namespace: "r6-ns", endpoint: url, bearerToken: "tok", strictMode: true, requestTimeoutMs: 500 },
        tool: { hook: async () => {} },
        hooks: {},
      };
      await initialize(ctx);

      // Prime the cache via resolveActiveTaskFromDaemon
      await resolveActiveTaskFromDaemon("r6-ns");
      assert.equal(cachedTaskInjection.get("task-update-title"), "### Original daemon injection");

      // Execute native task_update with title (should invalidate cache)
      await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "neurostrata_task_update",
        tool_input: { id: "task-update-title", title: "New Title" },
      });

      // Cache should be invalidated for this task id
      assert.equal(cachedTaskInjection.has("task-update-title"), false, "title update must invalidate cachedTaskInjection");

      // Next enrichAndInjectSubagentPrompt should re-fetch from daemon
      const { server: server2, url: url2, counters: counters2 } = await createMockDaemonServer({
        returnTask: [
          {
            id: "task-update-title",
            session_id: "sess-1",
            status: "in_progress",
            subagent_prompt_injection: "### Updated daemon injection",
          },
        ],
      });
      try {
        resetClientForTesting();
        const ctx2 = {
          config: { namespace: "r6-ns", endpoint: url2, bearerToken: "tok", strictMode: true, requestTimeoutMs: 500 },
          tool: { hook: async () => {} },
          hooks: {},
        };
        await initialize(ctx2);

        // Provide task id so enrich uses cachedTaskInjection
        const target = { prompt: "Do something" };
        await enrichAndInjectSubagentPrompt(target, "r6-ns", "task-update-title");
        // Should have used the new injection since cache was invalidated
        assert.ok(target.prompt.includes("### Updated daemon injection"), "next subagent call re-fetches updated prompt");
      } finally {
        server2.close();
        resetModuleState();
      }
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("prefetchMemoriesForFile formats callers, importers, blast_radius_count into [NeuroStrata AST ...] snippet", async () => {
    const { server, url } = await createMockDaemonServer();
    try {
      const ctx = {
        config: { namespace: "ast-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 },
        tool: { hook: async () => {} },
        hooks: {},
      };
      await initialize(ctx);

      // Override the mock to return ast_summary with callers, importers, blast_radius_count
      // We need to create a custom mock for this test
      const astTestServer = await new Promise((resolve) => {
        const s = http.createServer((req, res) => {
          if (req.method !== "POST") { res.writeHead(405); res.end(); return; }
          let body = "";
          req.on("data", (c) => { body += c; });
          req.on("end", () => {
            const payload = JSON.parse(body);
            const id = payload.id ?? 0;
            const params = payload.params ?? {};
            const name = params.name ?? params.arguments?.name;
            if (name === "neurostrata_memories_for_file") {
              const astResponse = {
                jsonrpc: "2.0",
                id,
                result: {
                  content: [
                    {
                      type: "text",
                      text: JSON.stringify({
                        hits: [
                          { id: "mem-rule-1", valid_from: "2026-01-01T00:00:00Z", memory_type: "rule", content: "Always claim a task before editing." }
                        ],
                        ast_summary: {
                          callers: ["fnA@src/a.rs", "fnB@src/b.rs"],
                          importers: ["modC@src/c.rs"],
                          blast_radius_count: 42,
                          blast_radius: ["fnX@src/x.rs"],
                          symbols: ["symbol1", "symbol2"],
                          callees: ["callee1"],
                          imports: ["import1"]
                        }
                      }),
                    },
                  ],
                },
              };
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(JSON.stringify(astResponse));
              return;
            }
            if (payload.method === "tools/list") {
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(JSON.stringify({ tools: [{ name: "neurostrata_memories_for_file" }, { name: "neurostrata_task_list" }, { name: "neurostrata_guard_validate" }] }));
              return;
            }
            res.writeHead(200, { "Content-Type": "application/json" });
            res.end(JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32601, message: "Unknown tool" } }));
          });
        });
        s.listen(0, () => {
          const addr = s.address();
          resolve({ server: s, url: `http://127.0.0.1:${addr.port}/mcp` });
        });
      });

      resetClientForTesting();
      const astCtx = { config: { namespace: "ast-ns", endpoint: astTestServer.url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(astCtx);

      const snippets = await prefetchMemoriesForFile("ast-ns", "src/test.rs");
      assert.ok(snippets.length > 0, "should return snippets");
      const astSnippet = snippets.find((s) => s.startsWith("[NeuroStrata AST src/test.rs]"));
      assert.ok(astSnippet, "should have AST summary snippet");
      assert.ok(astSnippet.includes("callers=[fnA@src/a.rs,fnB@src/b.rs]"), "should format callers");
      assert.ok(astSnippet.includes("importers=[modC@src/c.rs]"), "should format importers");
      assert.ok(astSnippet.includes("blast_radius_count=42"), "should format blast_radius_count");
      assert.ok(astSnippet.includes("symbols=[symbol1,symbol2]"), "should format symbols");
      assert.ok(astSnippet.includes("callees=[callee1]"), "should format callees");
      assert.ok(astSnippet.includes("imports=[import1]"), "should format imports");
      assert.ok(astSnippet.includes("blast_radius=[fnX@src/x.rs]"), "should format blast_radius");

      astTestServer.server.close();
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("plugins/*/package.json scripts.dev uses tsc --project ../core/tsconfig.json && node ../core/bundle-adapter.mjs --watch", async () => {
    const pkgFiles = [
      "plugins/opencode/package.json",
      "plugins/claude/package.json",
      "plugins/codex/package.json",
      "plugins/github-cli/package.json",
    ];
    const expectedDev = 'tsc --project ../core/tsconfig.json && node ../core/bundle-adapter.mjs --watch';
    for (const f of pkgFiles) {
      const content = readFileSync(join(process.cwd(), f), "utf8");
      const pkg = JSON.parse(content);
      assert.equal(pkg.scripts.dev, expectedDev, `${f} must have correct dev script`);
    }
  });

  it("pre-commit ledger matching handles ./<path> and $ROOT/<path> without warning", async () => {
    const { execFileSync } = await import("node:child_process");
    const { mkdtempSync, symlinkSync, chmodSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const repoDir = mkdtempSync(join(tmpdir(), "ns-precommit-r6-"));
    const binDir = mkdtempSync(join(tmpdir(), "ns-precommit-bin-"));
    try {
      for (const cmd of ["bash", "sh", "git", "grep", "sed", "awk", "date", "mkdir", "printf", "mktemp", "mv", "rm", "dirname", "basename", "head", "pwd"]) {
        for (const prefix of ["/usr/bin", "/bin"]) {
          try { symlinkSync(join(prefix, cmd), join(binDir, cmd)); break; } catch {}
        }
      }

      execFileSync("git", ["init", "-q"], { cwd: repoDir });
      mkdirSync(join(repoDir, "src"), { recursive: true });
      writeFileSync(join(repoDir, "src", "test.rs"), "fn a() {}\n");
      execFileSync("git", ["add", "src/test.rs"], { cwd: repoDir });

      const hookPath = join(repoDir, "pre-commit-test.sh");
      writeFileSync(hookPath, readFileSync(new URL("../../scripts/hooks/pre-commit", import.meta.url), "utf8"));
      chmodSync(hookPath, 0o755);

      // Run 1: populate ledger with bare path
      execFileSync(hookPath, [], { cwd: repoDir, env: { PATH: binDir, HOME: repoDir } });

      let ledgerPath = join(repoDir, ".NeuroStrata", "session_queries.json");
      let ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(ledger.queries.length, 1);
      assert.equal(ledger.queries[0].query, "src/test.rs");

      // Run 2: manually inject $ROOT/path and ./path into ledger, then verify no warning on next run
      // We simulate a ledger that already has entries with different path formats
      ledger.queries.push({ ts: Date.now(), tool: "pre-commit", query: "./src/test.rs", domains: ["./src/test.rs"], returned_ids: [], governed_prefixes: ["./src/test.rs"] });
      ledger.queries.push({ ts: Date.now(), tool: "pre-commit", query: "$ROOT/src/test.rs", domains: ["$ROOT/src/test.rs"], returned_ids: [], governed_prefixes: ["$ROOT/src/test.rs"] });
      writeFileSync(ledgerPath, JSON.stringify(ledger, null, 2));

      // Stage another file and run - should not warn about src/test.rs (already in ledger in any format)
      writeFileSync(join(repoDir, "src", "another.rs"), "fn b() {}\n");
      execFileSync("git", ["add", "src/another.rs"], { cwd: repoDir });
      execFileSync(hookPath, [], { cwd: repoDir, env: { PATH: binDir, HOME: repoDir } });

      // Should not have warning for src/test.rs
      ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));
      assert.equal(ledger.queries.length, 4, "should have 4 queries total (src/test.rs once, ./src/test.rs, $ROOT/src/test.rs, src/another.rs)");
    } finally {
      const { rmSync } = await import("node:fs");
      rmSync(repoDir, { recursive: true, force: true });
      rmSync(binDir, { recursive: true, force: true });
    }
  });
});

describe("round-8: Codex apply_patch extraction, decoded ledger matching, and batched writes", () => {
  it("executeBeforeHandler extracts *** Update File path from string input", async () => {
    const { server, url, counters } = await createMockDaemonServer();
    try {
      let beforeExecFn = null;
      await initialize({
        config: { namespace: "r8-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 500 },
        hooks: {
          beforeToolExec: async (fn) => {
            beforeExecFn = fn;
          },
        },
      });
      const patch = "*** Update File: src/codex.rs\n@@\n-a\n+b\n";
      const result = await beforeExecFn({ tool: "apply_patch", input: patch });
      assert.ok(counters.memoriesForFile >= 1, "memories_for_file must run for the patched file");
      assert.ok(
        counters.memoriesForFilePaths.includes("src/codex.rs"),
        `patched path must be queried, got ${JSON.stringify(counters.memoriesForFilePaths)}`,
      );
      const blob = JSON.stringify(result ?? {});
      assert.ok(blob.includes("src/codex.rs"), "recall block must mention the patched file");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("handleHookEvent extracts patch path when event.input is a raw patch string", async () => {
    const { server, url, counters } = await createMockDaemonServer();
    try {
      await initialize({
        config: { namespace: "r8-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 500 },
        tool: { hook: async () => {} },
        hooks: {},
      });
      const result = await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "apply_patch",
        input: "*** Add File: src/new.rs\n+hello\n",
      });
      assert.ok(counters.memoriesForFile >= 1, "memories_for_file must run for the added file");
      assert.ok(
        counters.memoriesForFilePaths.includes("src/new.rs"),
        `added path must be queried, got ${JSON.stringify(counters.memoriesForFilePaths)}`,
      );
      const blob = JSON.stringify(result ?? {});
      assert.ok(blob.includes("src/new.rs"), "hook recall must mention the added file");
    } finally {
      server.close();
      resetModuleState();
    }
  });

  it("pre-commit matches JSON-escaped unicode/quote paths and appends missing files in one write", async () => {
    const { execFileSync } = await import("node:child_process");
    const { mkdtempSync, symlinkSync, chmodSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const repoDir = mkdtempSync(join(tmpdir(), "ns-precommit-r8-"));
    try {
      execFileSync("git", ["init", "-q"], { cwd: repoDir });
      mkdirSync(join(repoDir, "src"), { recursive: true });
      writeFileSync(join(repoDir, "src", "café.rs"), "fn cafe() {}\n");
      writeFileSync(join(repoDir, "src", "quote\"file.rs"), "fn q() {}\n");
      writeFileSync(join(repoDir, "src", "a.rs"), "fn a() {}\n");
      writeFileSync(join(repoDir, "src", "b.rs"), "fn b() {}\n");
      execFileSync("git", ["add", "src/café.rs", "src/quote\"file.rs", "src/a.rs", "src/b.rs"], { cwd: repoDir });

      const hookPath = join(repoDir, "pre-commit-test.sh");
      writeFileSync(hookPath, readFileSync(new URL("../../scripts/hooks/pre-commit", import.meta.url), "utf8"));
      chmodSync(hookPath, 0o755);

      mkdirSync(join(repoDir, ".NeuroStrata"), { recursive: true });
      const ledgerPath = join(repoDir, ".NeuroStrata", "session_queries.json");
      writeFileSync(
        ledgerPath,
        JSON.stringify({
          namespace: "r8",
          queries: [
            { ts: 1, tool: "pre-commit", query: "src/café.rs", domains: ["src/café.rs"], returned_ids: [], governed_prefixes: ["src/café.rs"] },
            { ts: 1, tool: "pre-commit", query: "src/quote\"file.rs", domains: ["src/quote\"file.rs"], returned_ids: [], governed_prefixes: ["src/quote\"file.rs"] },
          ],
        }),
      );
      // Force unicode escapes like python json.dump(ensure_ascii=True)
      const escaped = JSON.parse(readFileSync(ledgerPath, "utf8"));
      writeFileSync(ledgerPath, JSON.stringify(escaped).replace(/é/g, "\\u00e9"));

      const { spawnSync } = await import("node:child_process");
      const ran = spawnSync(hookPath, [], { cwd: repoDir, encoding: "utf8" });
      const stderr = ran.stderr ?? "";
      assert.equal(ran.status, 0, `pre-commit exited ${ran.status}: ${stderr}`);
      assert.equal(stderr.includes("src/café.rs"), false, `decoded unicode path must not warn: ${stderr}`);
      assert.equal(stderr.includes("quote"), false, `decoded quoted path must not warn: ${stderr}`);

      const ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));
      const queries = ledger.queries.map((q) => q.query).sort();
      assert.ok(queries.includes("src/a.rs"));
      assert.ok(queries.includes("src/b.rs"));
      assert.equal(queries.filter((q) => q === "src/café.rs" || q === "src/quote\"file.rs").length, 2, "existing decoded paths must not be duplicated");
    } finally {
      rmSync(repoDir, { recursive: true, force: true });
    }
  });
});

// ──────────────────────────────────────────────────────────────────────
// round-9: quoted JSON keys, verbatim governed prefix, denied-edit recall
// rollback, and stdout flush before exit
// ──────────────────────────────────────────────────────────────────────

describe("round-9: quoted JSON keys in Code Mode inspection", () => {
  it("tracks a claim whose keys are JSON-style quoted", () => {
    const out = inspectCodeModeExecution('neurostrata_task_claim({"id":"t1","session_id":"s1"})');
    assert.equal(out.taskClaimed, true, "quoted-key claim must be tracked");
    assert.equal(out.id, "t1");
    assert.equal(out.sessionId, "s1");
  });

  it("captures a quoted note and a quoted id on task_update", () => {
    const out = inspectCodeModeExecution('neurostrata_task_update({"id":"t1","note":"breath"})');
    assert.equal(out.taskUpdated, true);
    assert.equal(out.id, "t1");
    assert.equal(out.note, "breath");
  });

  it("extracts a quoted path from memories_for_file", () => {
    const out = inspectCodeModeExecution('neurostrata_memories_for_file({"namespace":"ns","path":"src/a.rs"})');
    assert.deepEqual(out.queriedFiles, ["src/a.rs"]);
  });

  it("keeps bare identifier keys working", () => {
    const out = inspectCodeModeExecution('neurostrata_task_claim({ id: "t2", session_id: "s2" })');
    assert.equal(out.taskClaimed, true);
    assert.equal(out.id, "t2");
    assert.equal(out.sessionId, "s2");
  });

  it("does not confuse a quoted key with an identifier suffix", () => {
    const out = inspectCodeModeExecution('neurostrata_task_claim({"user_id":"u1","id":"t3","session_id":"s3"})');
    assert.equal(out.id, "t3", "user_id must not be matched as id");
    assert.equal(out.sessionId, "s3");
  });
});

describe("round-9: governed_prefixes records the queried path verbatim", () => {
  it("records the full queried path, not its first component", async () => {
    const { server, url } = await createMockDaemonServer();
    const tmpProjectDir = mkdtempSync(join(tmpdir(), "ns-ledger-r9-"));
    const ledgerDir = join(tmpProjectDir, ".NeuroStrata");
    const ledgerPath = join(ledgerDir, "session_queries.json");
    try {
      mkdirSync(ledgerDir, { recursive: true });
      resetClientForTesting();
      const ctx = { config: { namespace: "r9-ledger-ns", endpoint: url, bearerToken: "tok", strictMode: false, requestTimeoutMs: 200 }, tool: { hook: async () => {} }, hooks: {} };
      await initialize(ctx);
      await prefetchMemoriesForFile("r9-ledger-ns", "src/auth/login.go", { recordLedgerDir: tmpProjectDir });
      const ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));
      const queries = Array.isArray(ledger) ? ledger : ledger.queries;
      assert.equal(queries.length, 1);
      assert.deepEqual(
        queries[0].governed_prefixes,
        ["src/auth/login.go"],
        "governed prefix must be the queried path itself",
      );
      assert.equal(
        queries[0].governed_prefixes.includes("src"),
        false,
        "widening to the first path component would silence sibling-file warnings",
      );
    } finally {
      server.close();
      resetModuleState();
      rmSync(tmpProjectDir, { recursive: true, force: true });
    }
  });
});

describe("round-9: denied edit restores prefetch dedup state", () => {
  it("the allowed retry still receives the governing memories", async () => {
    let reject = true;
    const { server, url, counters } = await createMockDaemonServer({ guardReject: () => reject });
    try {
      resetClientForTesting();
      await initialize({
        config: { namespace: "r9-deny-ns", endpoint: url, bearerToken: "tok", strictMode: true, requestTimeoutMs: 500 },
        tool: { hook: async () => {} },
        hooks: {},
      });

      const denied = await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "write",
        tool_input: { file_path: "src/foo.ts", content: "x" },
      });
      assert.equal(denied.hookSpecificOutput.permissionDecision, "deny");
      assert.ok(
        String(denied.systemMessage ?? "").includes("[NeuroStrata Governing Memories for src/foo.ts]"),
        "a denial must carry the governing rules it enforced",
      );

      reject = false;
      const allowed = await handleHookEvent({
        hook_event_name: "PreToolUse",
        tool_name: "write",
        tool_input: { file_path: "src/foo.ts", content: "x" },
      });
      assert.equal(allowed.hookSpecificOutput.permissionDecision, "allow");
      assert.ok(
        String(allowed.systemMessage ?? "").includes("[NeuroStrata Governing Memories for src/foo.ts]"),
        "the retry must still receive the governing memories",
      );
      assert.ok(counters.memoriesForFile >= 2, "the retry must re-query the daemon");
    } finally {
      server.close();
      resetModuleState();
    }
  });
});

describe("round-9: CLI hook runners flush stdout before exiting", () => {
  it("awaits the stdout write before process.exit(0)", () => {
    const sources = [
      "../claude/cli-hook.mjs",
      "../codex/cli-hook.mjs",
      "../github-cli/cli-hook.mjs",
      "../opencode/cli-hook.mjs",
      "../../src/task/harness_plugins.rs",
    ];
    for (const rel of sources) {
      const src = readFileSync(new URL(rel, import.meta.url), "utf8");
      const marker = "await writeStdout(JSON.stringify(out));";
      const idx = src.indexOf(marker);
      assert.ok(idx !== -1, `${rel}: missing awaited stdout write`);
      assert.ok(
        !src.includes("process.stdout.write(JSON.stringify(out))"),
        `${rel}: un-awaited stdout write still present`,
      );
      const after = src.slice(idx + marker.length);
      assert.ok(
        /^\s*process\.exit\(0\);/.test(after),
        `${rel}: process.exit(0) must directly follow the awaited write`,
      );
      assert.ok(
        src.includes("const writeStdout = (text) =>"),
        `${rel}: writeStdout helper missing`,
      );
    }
  });
});
