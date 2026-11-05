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
  const counters = { memoriesForFile: 0, taskList: 0, guardValidate: 0 };
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
            if (guardReject) {
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

describe("handleHookEvent / handleClaudeHookEvent alias", () => {
  it("handleClaudeHookEvent is the same function as handleHookEvent", () => {
    assert.equal(handleClaudeHookEvent, handleHookEvent);
  });

  it("PreToolUse subagent tool returns allow + enriches prompt (no daemon)", async () => {
    const target = { prompt: "Help me refactor this." };
    const out = await handleHookEvent({
      hook_event_name: "PreToolUse",
      tool_name: "subagent",
      tool_input: target,
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
    resetClientForTesting();
    const out = await handleHookEvent({
      hook_event_name: "PreToolUse",
      tool_name: "read",
      tool_input: { file_path: "src/foo.ts" },
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
});
