import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import plugin, {
  detectTaskIntentsLocal,
  parseGuardValidate,
  resolveConfig,
  NeuroStrataMCPClient,
  adapterStatus,
  onTurnEnd,
  fetchContextPlan,
  resetClientForTesting,
} from "./dist/index.js";

test("Plugin.define exports V2 setup and registers execute.before hook", async () => {
  assert.equal(plugin.id, "neurostrata");
  assert.equal(typeof plugin.setup, "function");

  let registeredHook = null;
  let registeredCallback = null;
  const ctx = {
    config: {
      namespace: "test-ns",
      endpoint: "http://127.0.0.1:1/mcp",
      bearerToken: "test-token",
      strictMode: true,
      requestTimeoutMs: 100,
    },
    tool: {
      hook: async (name, cb) => {
        registeredHook = name;
        registeredCallback = cb;
      },
    },
  };

  await plugin.setup(ctx);
  assert.equal(registeredHook, "execute.before");
  assert.equal(typeof registeredCallback, "function");

  // Non-mutating tool passes through without throwing
  await registeredCallback({ tool: "read", input: { path: "foo" }, sessionID: "s1" });

  // Mutating tool invokes guard_validate and fails closed when core unreachable
  await assert.rejects(
    () =>
      registeredCallback({
        tool: "write",
        input: { path: "foo.txt", content: "bar" },
        sessionID: "s1",
      }),
    /Guard validation unavailable/
  );
});

test("parseGuardValidate and detectTaskIntentsLocal", () => {
  const verdict = parseGuardValidate(
    JSON.stringify({
      verdict: {
        bucket: "DeterministicReject",
        reasons: ["claim task first"],
      },
      rule_ids_triggered: ["r1"],
    })
  );
  assert.equal(verdict.verdict, "reject");
  assert.deepEqual(verdict.reasons, ["claim task first"]);

  const intents = detectTaskIntentsLocal("TODO: wire up validation later");
  assert.equal(intents.length, 1);
  assert.equal(intents[0].marker, "todo:");
});

// ─── Mock local HTTP JSON-RPC server for healthy daemon round-trip ───

function makeMockRpcResponse(id, resultObj, errorObj = null) {
  return JSON.stringify({
    jsonrpc: "2.0",
    id,
    ...(errorObj ? { error: errorObj } : { result: resultObj }),
  });
}

function createMockDaemonServer(port = 0) {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      if (req.method !== "POST") {
        res.writeHead(405); res.end(); return;
      }
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        try {
          const payload = JSON.parse(body);
          const id = payload.id ?? 0;
          const method = payload.method;
          if (method === "tools/list") {
            res.writeHead(200, { "Content-Type": "application/json" });
            res.end(makeMockRpcResponse(id, {
              tools: [
                { name: "neurostrata_guard_validate" },
                { name: "neurostrata_task_create" },
                { name: "neurostrata_get_snapshot" },
                { name: "neurostrata_append_log" },
                { name: "neurostrata_search_memory" },
                { name: "neurostrata_context_plan" },
              ],
            }));
            return;
          }
          if (method === "tools/call") {
            const params = payload.params ?? {};
            const name = params.name ?? params.arguments?.name;
            // Healthy pass for guard; snapshot / log / search return ready + unlocked
            if (name === "neurostrata_get_snapshot") {
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(makeMockRpcResponse(id, {
                content: [{ type: "text", text: '{"status":"ready","preflight_unlocked":true}' }],
              }));
              return;
            }
            if (name === "neurostrata_append_log") {
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(makeMockRpcResponse(id, {
                content: [{ type: "text", text: '{"status":"ready","preflight_unlocked":true,"logged":true}' }],
              }));
              return;
            }
            if (name === "neurostrata_search_memory") {
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(makeMockRpcResponse(id, {
                content: [{ type: "text", text: '{"status":"ready","unlocked_preflight":true}' }],
              }));
              return;
            }
            if (name === "neurostrata_context_plan") {
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(makeMockRpcResponse(id, {
                content: [{ type: "text", text: '{"schema_version":"v1","items":[]}' }],
              }));
              return;
            }
            if (name === "neurostrata_guard_validate") {
              res.writeHead(200, { "Content-Type": "application/json" });
              res.end(makeMockRpcResponse(id, {
                content: [{ type: "text", text: '{"verdict":{"bucket":"Pass"},"rule_ids_triggered":[]}' }],
              }));
              return;
            }
            // Unknown tool → error response
            res.writeHead(200, { "Content-Type": "application/json" });
            res.end(makeMockRpcResponse(id, null, { code: -32601, message: "Unknown tool" }));
            return;
          }
          // Unknown method
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(makeMockRpcResponse(id, null, { code: -32601, message: "Unknown method" }));
        } catch {
          res.writeHead(400); res.end();
        }
      });
    });
    server.listen(port, () => {
      const addr = server.address();
      resolve({ server, url: `http://127.0.0.1:${addr.port}/mcp` });
    });
  });
}

// ─── (a) Healthy daemon round-trip + adapter status "ready" / unlocked ───

test("Healthy mock daemon: JSON-RPC 2.0 round-trip and unlocked pre-flight", async () => {
  const { server, url } = await createMockDaemonServer();
  try {
    const client = new NeuroStrataMCPClient({
      endpoint: url,
      token: "mock-token",
      timeoutMs: 2000,
    });

    // Snapshot
    const snapshotText = await client.callTool("neurostrata_get_snapshot", {});
    const snapshot = JSON.parse(snapshotText);
    assert.equal(snapshot.status, "ready");
    assert.equal(snapshot.preflight_unlocked, true);

    // Append log
    const logText = await client.callTool("neurostrata_append_log", { tags: ["test"] });
    const log = JSON.parse(logText);
    assert.equal(log.status, "ready");
    assert.equal(log.preflight_unlocked, true);
    assert.equal(log.logged, true);

    // Search memory
    const searchText = await client.callTool("neurostrata_search_memory", { query: "preflight" });
    const search = JSON.parse(searchText);
    assert.equal(search.status, "ready");
    assert.equal(search.unlocked_preflight, true);

    // Adapter status via initialize on a healthy endpoint should report ready
    const ctxHealthy = {
      config: {
        namespace: "healthy-ns",
        endpoint: url,
        bearerToken: "mock-token",
        strictMode: false,
        requestTimeoutMs: 500,
      },
      tool: { hook: async () => {} },
    };
    await plugin.setup(ctxHealthy);
    const status = adapterStatus();
    assert.equal(status.state, "ready", "adapter should be ready with healthy daemon");
    assert.equal(status.endpoint, url);
  } finally {
    server.close();
  }
});

// ─── (b) Unreachable daemon fail-open fallback (degraded / unavailable) ───

test("Unreachable daemon fail-open fallback keeps degraded endpoint visible", async () => {
  // Intentionally use unreachable endpoint (127.0.0.1:1)
  const ctxUnreachable = {
    config: {
      namespace: "fail-open",
      endpoint: "http://127.0.0.1:1/mcp",
      bearerToken: null,
      strictMode: true,
      requestTimeoutMs: 100,
    },
    tool: {
      hook: async (name, cb) => {
        // Mutating tool should throw because guard is unreachable
        if (name === "execute.before") {
          await assert.rejects(
            () =>
              cb({
                tool: "write",
                input: { path: "x.txt", content: "y" },
                sessionID: "s1",
              }),
            /Guard validation unavailable/
          );
        }
      },
    },
  };
  await plugin.setup(ctxUnreachable);
  const status = adapterStatus();
  assert.equal(status.endpoint, "http://127.0.0.1:1/mcp", "degraded endpoint preserved");
  assert.equal(status.state, "unavailable", "adapter should be unavailable when daemon unreachable");
});

// ─── (c) onTurnEnd + fetchContextPlan ───

test("onTurnEnd resolves without throwing", async () => {
  await onTurnEnd({ role: "user", content: "TODO: track this work" });
  await onTurnEnd({ role: "assistant", content: "no intents here" });
});

test("fetchContextPlan calls neurostrata_context_plan and returns text", async () => {
  const { server, url } = await createMockDaemonServer();
  try {
    const ctx = {
      config: {
        namespace: "ctx-ns",
        endpoint: url,
        bearerToken: "mock-token",
        strictMode: false,
        requestTimeoutMs: 500,
      },
      tool: { hook: async () => {} },
    };
    await plugin.setup(ctx);
    const text = await fetchContextPlan({ namespace: "ctx-ns", query: "foo", max_items: 2 });
    assert.equal(typeof text, "string");
    assert.ok(text.includes("schema_version") || text.includes("items"));
  } finally {
    server.close();
  }
});

test("fetchContextPlan throws when MCP client not initialized", async () => {
  // Drop the cached client so the module is back to "not initialized".
  resetClientForTesting();
  await assert.rejects(
    () => fetchContextPlan({ namespace: "ctx-ns" }),
    /NeuroStrata MCP client not initialized/
  );
});
