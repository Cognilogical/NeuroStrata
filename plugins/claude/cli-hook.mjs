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
// The bundle's optional handlers, checked before an event goes to the plugin.
import * as mod from "./dist/index.js";

const HARNESS = "claude";
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url));
// Events whose reply carries no permission decision (the session events and
// PostToolUse), by the lowercase name every harness spelling folds to
// (Copilot names its event only in NEUROSTRATA_HOOK_EVENT, camelCase).
const NO_DECISION_EVENTS = {
  sessionstart: "SessionStart",
  sessionend: "SessionEnd",
  precompact: "PreCompact",
  posttooluse: "PostToolUse"
};
// The name of the event being handled (Copilot names it only in
// NEUROSTRATA_HOOK_EVENT), so the catch can reply in the right hook shape.
let eventName;

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
  eventName = [
    event.hook_event_name,
    event.hookEventName,
    event.eventName,
    process.env.NEUROSTRATA_HOOK_EVENT
  ].find((name) => typeof name === "string" && name);
  const noDecisionEvent = eventName ? NO_DECISION_EVENTS[eventName.toLowerCase()] : undefined;
  // A plugin build without a handler for this event would answer with its
  // PreToolUse-shaped default, which Codex rejects on SessionStart and
  // PreCompact, and would run the PreToolUse work a second time on
  // PostToolUse. Answer `{}` (valid for every harness) instead. Claude's
  // SessionStart keeps going to the plugin, as it always has.
  const handled = {
    SessionStart: HARNESS === "claude" || typeof mod.handleSessionStartEvent === "function",
    SessionEnd: typeof mod.writeSessionHandoff === "function",
    PreCompact: typeof mod.writeSessionHandoff === "function",
    PostToolUse: typeof mod.handlePostToolUseEvent === "function"
  }[noDecisionEvent];
  if (noDecisionEvent && !handled) {
    await writeStdout("{}");
    return;
  }
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
  // Reply in the shape of the event that failed; an event without a name is
  // a PreToolUse, as for the plugin. A PreToolUse-only deny is meaningless at
  // Stop: the host cannot apply it, so the turn could end without retry. At
  // Stop, Claude Code treats exit code 2 as a block; stdout still has to be
  // valid JSON. PowerShell can collapse nonzero exits, so the reason remains
  // on stderr as well.
  const failed = (eventName ?? "PreToolUse").toLowerCase();
  if (failed === "stop") {
    process.stdout.write("{}");
    process.exitCode = 2;
    return;
  }
  // Only PreToolUse takes a permission decision. Every other event (the
  // session events, PostToolUse, each Copilot event but preToolUse) answers
  // `{}`, so the session and the tool result go on.
  if (failed !== "pretooluse") {
    process.stdout.write("{}");
    return;
  }
  // Copilot CLI (its hooks file sets NEUROSTRATA_HOOK_EVENT) reads a
  // top-level decision on preToolUse and ignores hookSpecificOutput.
  if (process.env.NEUROSTRATA_HOOK_EVENT) {
    process.stdout.write(
      JSON.stringify({
        permissionDecision: "deny",
        permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}`,
      })
    );
    return;
  }
  // PreToolUse: an explicit deny fails closed on every shell; no `continue`
  // — Codex rejects it on PreToolUse.
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
