import test from "node:test";
import assert from "node:assert/strict";
import plugin, {
  detectTaskIntentsLocal,
  parseGuardValidate,
  resolveConfig,
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
