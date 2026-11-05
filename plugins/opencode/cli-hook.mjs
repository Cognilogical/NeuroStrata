#!/usr/bin/env node
import { readFileSync } from "node:fs";
import plugin, { initialize, onTurnEnd } from "./dist/index.js";

async function main() {
  // Redirect all diagnostic logging to stderr so stdout carries only the
  // JSON hook response (critical when this runner is invoked by a CLI hook
  // whose stdout is parsed as structured output).
  console.log = console.error;
  console.info = console.error;
  console.warn = console.error;

  let raw = "";
  try {
    raw = readFileSync(0, "utf8");
  } catch {
    process.exit(0);
  }
  if (!raw.trim()) process.exit(0);
  let event;
  try {
    event = JSON.parse(raw);
  } catch {
    process.exit(0);
  }
  await initialize({ config: { namespace: process.env.NEUROSTRATA_NAMESPACE } });
  // Handler chain: execute.before-style handleHookEvent takes precedence, then
  // the Claude handleClaudeHookEvent alias, falling back to the default export.
  const handler = plugin.handleHookEvent ?? plugin.handleClaudeHookEvent ?? plugin;
  if (typeof handler === "function") {
    const out = await handler(event);
    process.stdout.write(JSON.stringify(out));
    process.exit(0);
  }
  if (event.hook_event_name === "Stop" && typeof event.transcript === "string") {
    await onTurnEnd({ role: "assistant", content: event.transcript });
  }
  process.stdout.write(JSON.stringify({ continue: true }));
}
main().catch((err) => {
  process.stderr.write(String(err) + "\n");
  process.exit(2);
});
