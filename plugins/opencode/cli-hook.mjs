#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import plugin, {
  initialize,
  onTurnEnd,
  readHarnessConfig,
  resolveHookProjectDir,
  usableNamespace
} from "./dist/index.js";

const HARNESS = "opencode";
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url));
// The event being handled, so the catch can reply in the right hook shape.
let currentEvent = null;

async function main() {
  // Redirect all diagnostic logging to stderr so stdout carries only the
  // JSON hook response (critical when this runner is invoked by a CLI hook
  // whose stdout is parsed as structured output).
  console.log = console.error;
  console.info = console.error;
  console.warn = console.error;

  // Await the write callback so a large hook response (e.g. a big
  // `updatedInput`) is fully flushed before the runner ends. The runner then
  // returns instead of calling process.exit: on Windows, process.exit while a
  // daemon HTTP socket is still open crashes node, and the host reports the
  // hook as failed (Codex then runs the tool anyway).
  const writeStdout = (text) =>
    new Promise((resolve) => {
      process.stdout.write(text, () => resolve());
    });

  let raw = "";
  try {
    raw = readFileSync(0, "utf8");
  } catch {
    return;
  }
  if (!raw.trim()) return;
  let event;
  try {
    event = JSON.parse(raw);
  } catch {
    return;
  }
  currentEvent = event;
  // The namespace comes from the harness config the installer wrote; the
  // plugin reads NEUROSTRATA_NAMESPACE itself, after the project marker and
  // this config (the order of `neurostrata-mcp namespace resolve`).
  const projectDir = resolveHookProjectDir(HARNESS, event);
  // Pass the namespace only when it is set: an explicit undefined would
  // override the one in .NeuroStrata/neurostrata-plugin.json.
  const namespace = usableNamespace(process.env.NEUROSTRATA_NAMESPACE);
  const envConfig = namespace ? { namespace } : {};
  await initialize({
    config: { ...envConfig, ...readHarnessConfig(HARNESS, projectDir, RUNNER_DIR) },
    projectDir,
  });
  // Handler chain: execute.before-style handleHookEvent takes precedence, then
  // the Claude handleClaudeHookEvent alias, falling back to the default export.
  const handler = plugin.handleHookEvent ?? plugin.handleClaudeHookEvent ?? plugin;
  if (typeof handler === "function") {
    const out = await handler(event);
    await writeStdout(JSON.stringify(out));
    return;
  }
  if (event.hook_event_name === "Stop" && typeof event.transcript === "string") {
    await onTurnEnd({ role: "assistant", content: event.transcript });
  }
  await writeStdout(JSON.stringify({ continue: true }));
}
main().catch((err) => {
  const reason = String(err);
  process.stderr.write(reason + "\n");
  // Reply in the shape of the event that failed. A PreToolUse-only deny is
  // meaningless at Stop: the host cannot apply it, so the turn could end
  // without retry. At Stop, Claude Code treats exit code 2 as a block; stdout
  // still has to be valid JSON. PowerShell can collapse nonzero exits, so the
  // reason remains on stderr as well.
  if (currentEvent?.hook_event_name === "Stop") {
    process.stdout.write("{}");
    process.exitCode = 2;
    return;
  }
  // PreToolUse (and other tool events): an explicit deny fails closed on
  // every shell; no `continue` — Codex rejects it on PreToolUse.
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`,
      },
    })
  );
});
