//! Zero-touch CLI/terminal agent harness detection and automatic plugin
//! installation during init (`neurostrata_bootstrap`, `neurostrata_task_setup`,
//! and `neurostrata_get_snapshot`).

use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Namespace resolution reads untracked config files from the checkout, which
/// a contributor can replace with a symlink to a special file or a huge file.
/// Anything past this bound is unreadable, never slurped into memory.
const MAX_CONFIG_BYTES: u64 = 1_048_576;

/// Serialize same-process config rewrites. Two refreshes that race on one
/// config must not derive the same temporary path and clobber each other.
static CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static ATOMIC_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const OPENCODE_INDEX_JS: &str = include_str!("../../plugins/opencode/dist/index.js");
const OPENCODE_PKG_JSON: &str = include_str!("../../plugins/opencode/package.json");

const OPENCLAW_INDEX_JS: &str = include_str!("../../plugins/openclaw/dist/index.js");
const OPENCLAW_PKG_JSON: &str = include_str!("../../plugins/openclaw/package.json");

const CLAUDE_INDEX_JS: &str = include_str!("../../plugins/claude/dist/index.js");
const CLAUDE_PKG_JSON: &str = include_str!("../../plugins/claude/package.json");

const HERMES_INDEX_JS: &str = include_str!("../../plugins/hermes/dist/index.js");
const HERMES_PKG_JSON: &str = include_str!("../../plugins/hermes/package.json");

const CODEX_INDEX_JS: &str = include_str!("../../plugins/codex/dist/index.js");
const CODEX_PKG_JSON: &str = include_str!("../../plugins/codex/package.json");

const GITHUB_CLI_INDEX_JS: &str = include_str!("../../plugins/github-cli/dist/index.js");
const GITHUB_CLI_PKG_JSON: &str = include_str!("../../plugins/github-cli/package.json");

// Native CLI runners stay harness-specific: each one maps only the events its
// host actually emits. The installer still owns one canonical path per harness.
const CLAUDE_CLI_HOOK_RUNNER_MJS: &str = include_str!("../../plugins/claude/cli-hook.mjs");
const CODEX_CLI_HOOK_RUNNER_MJS: &str = include_str!("../../plugins/codex/cli-hook.mjs");
const GITHUB_CLI_CLI_HOOK_RUNNER_MJS: &str = include_str!("../../plugins/github-cli/cli-hook.mjs");

// @neurostrata/plugin-core, bundled once so the harness adapters' relative
// `../../core/dist/index.js` imports resolve on disk under
// `.NeuroStrata/plugins/core/dist/index.js`.
const CORE_INDEX_JS: &str = include_str!("../../plugins/core/dist/index.js");
const CORE_PKG_JSON: &str = include_str!("../../plugins/core/package.json");

const CLI_HOOK_RUNNER_MJS: &str = r#"#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import plugin, * as mod from "./dist/index.js";

// The installer writes this runner to <root>/.NeuroStrata/plugins/<harness>/;
// the harness config of <root> is read first.
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url));
const HARNESS = basename(RUNNER_DIR);
let currentEvent = null;

const writeStdout = (text) =>
  new Promise((resolve) => {
    process.stdout.write(text, () => resolve());
  });

function eventName(event) {
  return (
    process.env.NEUROSTRATA_HOOK_EVENT ||
    event?.hook_event_name ||
    event?.failedEventName ||
    "PreToolUse"
  );
}

function noOpReply(name) {
  if (name === "Stop" || name === "SubagentStop") return { continue: true };
  return { hookSpecificOutput: { hookEventName: name } };
}
// Events whose reply carries no permission decision (the session events and
// PostToolUse), by the lowercase name every harness spelling folds to.
const NO_DECISION_EVENTS = {
  sessionstart: "SessionStart",
  sessionend: "SessionEnd",
  precompact: "PreCompact",
  posttooluse: "PostToolUse"
};
// The working-set bundle marks itself with this constant; a stand-in bundle
// without it keeps the #425 route through the plugin's own handlers.
const hasWorkingSet = typeof mod.WORKING_SET_FRAME_OPEN === "string";
// The name of the event being handled, so the catch can reply in its shape.
let failedEventName;

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
  failedEventName = [
    event.hook_event_name,
    event.hookEventName,
    event.failedEventName,
    process.env.NEUROSTRATA_HOOK_EVENT
  ].find((name) => typeof name === "string" && name);
  const noDecisionEvent = failedEventName ? NO_DECISION_EVENTS[failedEventName.toLowerCase()] : undefined;
  const handled = {
    SessionStart: HARNESS === "claude" || typeof mod.handleSessionStartEvent === "function" || hasWorkingSet,
    SessionEnd: typeof mod.writeSessionHandoff === "function" || hasWorkingSet,
    PreCompact: typeof mod.writeSessionHandoff === "function" || hasWorkingSet,
    PostToolUse: typeof mod.handlePostToolUseEvent === "function" || hasWorkingSet
  }[noDecisionEvent];
  if (noDecisionEvent && !handled) {
    await writeStdout("{}");
    return;
  }
  // The namespace comes from the harness config the installer wrote; the
  // plugin reads NEUROSTRATA_NAMESPACE itself, after the project marker and
  // this config (the order of `neurostrata-mcp namespace resolve`).
  const projectDir =
    typeof mod.resolveHookProjectDir === "function"
      ? mod.resolveHookProjectDir(HARNESS, event)
      : typeof event.cwd === "string" && event.cwd
        ? event.cwd
        : process.cwd();
  // OpenCode loads the first parseable .NeuroStrata shared config and only
  // then falls back to the historical .opencode registration. `namespace
  // resolve` uses that same precedence; passing the registration as ctx.config
  // would let `initialize` merge a stale namespace over the effective one.
  const readOpenCodeConfig = () => {
    for (const file of ["neurostrata-plugin.json", "PluginConfig.json"]) {
      try {
        const parsed = JSON.parse(readFileSync(join(projectDir, ".NeuroStrata", file), "utf8"));
        if (parsed && typeof parsed === "object") return parsed;
      } catch {}
    }
    return null;
  };
  const config =
    HARNESS === "opencode" && typeof mod.readHarnessConfig === "function"
      ? (readOpenCodeConfig() ?? mod.readHarnessConfig(HARNESS, projectDir, RUNNER_DIR))
      : typeof mod.readHarnessConfig === "function"
        ? mod.readHarnessConfig(HARNESS, projectDir, RUNNER_DIR)
        : {};
  const init = mod.initialize ?? plugin?.initialize ?? plugin?.setup;
  if (typeof init === "function") {
    // Pass the namespace only when it is set: an explicit undefined would
    // override the one in .NeuroStrata/neurostrata-plugin.json.
    const namespace = process.env.NEUROSTRATA_NAMESPACE;
    const envConfig = namespace ? { namespace } : {};
    await init({ config: { ...envConfig, ...config }, projectDir });
  }
  const handler =
    mod.handleHookEvent ??
    plugin?.handleHookEvent ??
    mod.handleClaudeHookEvent ??
    plugin?.handleClaudeHookEvent;
  if (!hasWorkingSet && typeof handler === 'function') {
    const out = await handler(event);
    await writeStdout(JSON.stringify(out));
    return;
  }
  const name = eventName(event);
  if (["PreToolUse", "Stop", "SubagentStop"].includes(name)) {
    if (typeof handler === "function") {
      const out = await handler(event);
      await writeStdout(JSON.stringify(out));
      return;
    }
    if (name === "Stop" && typeof event.transcript === "string") {
      const turnEnd = mod.onTurnEnd ?? plugin?.onTurnEnd;
      if (typeof turnEnd === "function") {
        await turnEnd({ role: "assistant", content: event.transcript });
      }
    }
    await writeStdout(JSON.stringify({ continue: true }));
    return;
  }
  await writeStdout(JSON.stringify(noOpReply(name)));
}
main().catch(async (err) => {
  const reason = String(err);
  process.stderr.write(reason + "\n");
  // Reply in the shape of the event that failed; an event without a name is
  // a PreToolUse, as for the plugin. A PreToolUse deny means nothing at Stop,
  // where Claude Code treats exit code 2 as a block; stdout still has to be
  // valid JSON. PowerShell can collapse nonzero exits, so the reason stays on
  // stderr as well.
  const failed = (failedEventName ?? "PreToolUse").toLowerCase();
  if (failed === "stop") {
    await writeStdout("{}");
    process.exitCode = 2;
    return;
  }
  // Only PreToolUse takes a permission decision. Every other event (the
  // session events, PostToolUse, each Copilot event but preToolUse) answers
  // `{}`, so the session and the tool result go on.
  if (failed !== "pretooluse") {
    await writeStdout("{}");
    return;
  }
  // Copilot CLI (its hooks file sets NEUROSTRATA_HOOK_EVENT) reads a
  // top-level decision on preToolUse and ignores hookSpecificOutput.
  if (process.env.NEUROSTRATA_HOOK_EVENT) {
    await writeStdout(JSON.stringify({ permissionDecision: "deny", permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}` }));
    return;
  }
  // PowerShell swallows `process.exit(2)`: the host sees a zero exit code
  // and runs the tool anyway. Emit a deny decision so a failed hook still
  // fails closed on Windows. No `continue` field: Codex rejects it on
  // PreToolUse (the plugin's deny replies omit it too).
  await writeStdout(JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: reason
    }
  }));
});
"#;

const EMBEDDED_NEUROSTRATA_SKILL_MD: &str =
    include_str!("../../.agents/skills/neurostrata/SKILL.md");
const EMBEDDED_NEUROSTRATA_TASK_AGENT_MD: &str =
    include_str!("../../.agents/agents/NeuroStrata-Task.md");
const SUBAGENT_RULE_MARKER: &str = "subagent_prompt_injection";
const SUBAGENT_GRAPH_FIRST_RULE_SNIPPET: &str = "\n- **NEUROSTRATA GRAPH-FIRST & SUBAGENT CONTEXT RULE:** Honor the `### NeuroStrata Task & Graph Context` (`subagent_prompt_injection`) block in your prompt — every `[rule]` entry is a binding architectural constraint. Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed under `### NeuroStrata Task & Graph Context` in your prompt. For any file you touch that is NOT listed in that block, call `neurostrata_memories_for_file` first (or `neurostrata_related` / `neurostrata_search_memory` with `mode`: `hybrid` (default) + `include_references`: true / `neurostrata_sparql_query`). Never leave `TODO`, `FIXME`, `stub`, or `phase-2` deferrals.\n";

/// Claude Code tools the installed PreToolUse hook runs for. NotebookEdit
/// writes files too, so it goes through the guard like Write and Edit.
const CLAUDE_PRE_TOOL_USE_MATCHER: &str = "Read|Write|Edit|MultiEdit|NotebookEdit|Bash|Task";
/// The matcher older installers wrote, before NotebookEdit was guarded.
const LEGACY_CLAUDE_PRE_TOOL_USE_MATCHER: &str = "Read|Write|Edit|MultiEdit|Bash|Task";

const KNOWN_HARNESSES: &[&str] = &[
    "opencode",
    "openclaw",
    "claude",
    "hermes",
    "codex",
    "github-cli",
];

fn bundle_for(harness: &str) -> Option<(&'static str, &'static str)> {
    match harness {
        "opencode" => Some((OPENCODE_INDEX_JS, OPENCODE_PKG_JSON)),
        "openclaw" => Some((OPENCLAW_INDEX_JS, OPENCLAW_PKG_JSON)),
        "claude" => Some((CLAUDE_INDEX_JS, CLAUDE_PKG_JSON)),
        "hermes" => Some((HERMES_INDEX_JS, HERMES_PKG_JSON)),
        "codex" => Some((CODEX_INDEX_JS, CODEX_PKG_JSON)),
        "github-cli" => Some((GITHUB_CLI_INDEX_JS, GITHUB_CLI_PKG_JSON)),
        _ => None,
    }
}

fn cli_hook_runner_for(harness: &str) -> &'static str {
    match harness {
        "claude" => CLAUDE_CLI_HOOK_RUNNER_MJS,
        "codex" => CODEX_CLI_HOOK_RUNNER_MJS,
        "github-cli" => GITHUB_CLI_CLI_HOOK_RUNNER_MJS,
        _ => CLI_HOOK_RUNNER_MJS,
    }
}

fn normalize_harness_name(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "opencode" | "open-code" => Some("opencode"),
        "openclaw" | "open-claw" => Some("openclaw"),
        "claude" | "claude-code" | "claudecode" => Some("claude"),
        "hermes" | "hermes-agent" => Some("hermes"),
        "codex" | "codex-cli" | "openai-codex" => Some("codex"),
        "github-cli" | "gh" | "copilot" | "copilot-cli" | "github-copilot" => Some("github-cli"),
        _ => None,
    }
}

/// Absolute path for writing into harness config that another program will
/// execute. dunce, not std: `std::fs::canonicalize` returns Windows'
/// extended-length form (`\\?\C:\...`), which a shell mangles to `C:\?\C:\...`
/// and Node then cannot open, so every hook fails with MODULE_NOT_FOUND.
/// dunce yields the plain form wherever one exists and is a no-op elsewhere.
fn plain_abs_path(path: &Path) -> String {
    dunce::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn path_basename(p: &str) -> Option<&str> {
    p.rsplit(['/', '\\']).find(|s| !s.is_empty())
}

/// One spelling of a path for equality checks: the verbatim prefix dropped,
/// `\` turned into `/`, and ASCII case folded when it starts with a drive
/// letter (Windows compares paths case-insensitively). Plain string work, so
/// Windows spellings compare the same way when the tests run on Unix.
fn path_key(p: &str) -> String {
    let p = match p.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => p.strip_prefix(r"\\?\").unwrap_or(p).to_string(),
    };
    let p = p.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        p.to_ascii_lowercase()
    } else {
        p
    }
}

/// Whether two paths name the same file: equal [`path_key`]s, or, when the
/// spellings differ but both exist, equal canonical forms. `path_key` folds
/// ASCII case only, so this catches a case-only rename with non-ASCII letters
/// (`Pröj` vs `PRÖJ`) on a case-insensitive volume, and 8.3 short names.
/// This duplicates the comparison rule of a shared paths module
/// (`relative_to_on_disk`) until one lands upstream; move it there then.
fn same_path_on_disk(a: &str, b: &str) -> bool {
    if path_key(a) == path_key(b) {
        return true;
    }
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether a hook's runner path may be pointed at the installer's runner:
/// it already names the installer's own runner (in any spelling), or nothing
/// exists there. A runner that exists anywhere else is someone's own install,
/// and so is a path the shell expands (`$VAR`, `%VAR%`, `~`), which cannot be
/// checked from here.
fn runner_is_replaceable(old_path: &str, project_root: &Path, abs_runner: &str) -> bool {
    if path_key(old_path) == path_key(abs_runner) {
        return true;
    }
    if old_path.contains(['$', '%']) || old_path.starts_with('~') {
        return false;
    }
    // A relative runner is resolved by the hook's cwd, the project root.
    let candidate = project_root.join(old_path);
    if same_path_on_disk(&candidate.to_string_lossy(), abs_runner) {
        return true;
    }
    // Only a definite "not found" counts as missing; an unreadable path is
    // left alone. Repair may repoint any missing `cli-hook.mjs` to the
    // installer's runner; namespace resolution uses the stricter
    // `runner_is_installers_hook` below.
    matches!(candidate.try_exists(), Ok(false))
}

/// Like [`runner_is_replaceable`], but a *missing* runner counts as the
/// installer's only when its path ends with the installer's plugin location.
/// Namespace resolution must not mistake a custom hook that merely names a
/// `cli-hook.mjs` for installer configuration (and capture the push gate's
/// namespace). The trailing plugin structure is compared, not the whole path,
/// so a moved checkout whose old absolute prefix no longer exists is still
/// recognised as ours.
fn runner_is_installers_hook(old_path: &str, project_root: &Path, abs_runner: &str) -> bool {
    if !runner_is_replaceable(old_path, project_root, abs_runner) {
        return false;
    }
    // A runner that exists, or is the installer's own path, was already
    // accepted. Only the "missing" branch needs the plugin-location check.
    let candidate = project_root.join(old_path);
    if matches!(candidate.try_exists(), Ok(false)) {
        let Some(suffix) = plugin_location_suffix(abs_runner) else {
            return false;
        };
        return candidate
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase()
            .ends_with(&suffix);
    }
    true
}

/// The trailing `.NeuroStrata/plugins/<harness>/cli-hook.mjs` structure of
/// the installer's runner, lowercased with forward slashes. A missing runner
/// must end with this to count as an installer hook.
fn plugin_location_suffix(abs_runner: &str) -> Option<String> {
    let normalized = abs_runner.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    let idx = parts
        .iter()
        .rposition(|p| p.eq_ignore_ascii_case(".NeuroStrata"))?;
    Some(parts[idx..].join("/").to_ascii_lowercase())
}

/// Whether a runner path is the installer's own runner (in any spelling) or
/// sits where the installer puts one, `.NeuroStrata/plugins/claude/cli-hook.mjs`
/// (a moved or renamed project's). Plain string work for the suffix, as in
/// [`path_key`].
fn at_installer_runner_place(old_path: &str, project_root: &Path, abs_runner: &str) -> bool {
    let folded = old_path.replace('\\', "/").to_ascii_lowercase();
    let rel = ".neurostrata/plugins/claude/cli-hook.mjs";
    folded == rel
        || folded.ends_with(&format!("/{rel}"))
        || same_path_on_disk(&project_root.join(old_path).to_string_lossy(), abs_runner)
}

/// Claude's plugin config, next to its bundle (as GitHub CLI's is). The Claude
/// hook command carries no namespace; the runner reads it from here.
const CLAUDE_CONFIG_REL: &str = ".NeuroStrata/plugins/claude/neurostrata.json";

/// The namespace Claude's plugin config names at `root`: `Ok(None)` when the
/// file is missing or names no usable one, an error when it exists but
/// cannot be read or parsed.
fn claude_config_namespace_checked(root: &Path) -> Result<Option<String>, UnreadableConfig> {
    Ok(read_config(&root.join(CLAUDE_CONFIG_REL))?
        .as_ref()
        .and_then(|doc| doc.get("namespace"))
        .and_then(Value::as_str)
        .and_then(usable_namespace))
}

/// The namespace Claude's plugin config names at `root`, when usable.
fn claude_config_namespace(root: &Path) -> Option<String> {
    claude_config_namespace_checked(root).ok().flatten()
}

/// Writes Claude's plugin config naming `namespace` and reads it back. `false`
/// when it cannot be written or does not read back with that namespace.
fn write_claude_config(project_root: &Path, namespace: &str) -> bool {
    let path = project_root.join(CLAUDE_CONFIG_REL);
    let doc = json!({ "id": "neurostrata-claude", "namespace": namespace });
    let Ok(text) = serde_json::to_string_pretty(&doc) else {
        return false;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let unchanged = std::fs::read_to_string(&path).is_ok_and(|existing| existing == text);
    if !unchanged && write_atomically(&path, &text).is_err() {
        return false;
    }
    claude_config_namespace(project_root).as_deref() == Some(namespace)
}

/// A Claude hook command the installer wrote, `node "<runner>"`, or an older
/// installer's `NEUROSTRATA_NAMESPACE=<ns> node "<runner>"`, whose runner
/// basename matches the installer's runner and whose runner is the
/// installer's own or is missing: the namespace it names (none for the plain
/// form or an empty assignment) and its repaired form `node "<abs_runner>"`.
/// `None` for any other command (a custom hook that merely carries the
/// namespace variable, a `cli-hook.mjs` installed elsewhere, or a namespace
/// that is not usable).
fn installer_hook_parts(
    command: &str,
    abs_runner: &str,
    project_root: &Path,
) -> Option<(Option<String>, String)> {
    let (raw_ns, rest) = match command.strip_prefix("NEUROSTRATA_NAMESPACE=") {
        Some(assigned) => assigned.split_once(' ')?,
        None => ("", command),
    };
    let rest = rest.strip_prefix("node \"")?;
    let close = rest.find('"')?;
    let old_path = &rest[..close];
    let suffix = &rest[close + 1..];
    // Windows separators are legal in a verbatim path even when the test
    // that exercises this runs on Unix, so do not use platform Path parsing.
    if path_basename(old_path) != path_basename(abs_runner) {
        return None;
    }
    // Both the plain and the `NEUROSTRATA_NAMESPACE=<ns>` forms must name
    // the installer's runner, or a missing runner at the installer's place
    // (`.NeuroStrata/plugins/claude/cli-hook.mjs` of a moved project). An
    // arbitrary `cli-hook.mjs` with a namespace prefix is someone else's
    // hook: claiming it would overwrite this project's config (#378 F6).
    if !at_installer_runner_place(old_path, project_root, abs_runner) {
        return None;
    }
    // The strict check: a missing runner counts only at the installer's plugin
    // location, so a custom `NEUROSTRATA_NAMESPACE=x node "<gone>/cli-hook.mjs"`
    // is not migrated into Claude's plugin config on the next refresh.
    if !runner_is_installers_hook(old_path, project_root, abs_runner) {
        return None;
    }
    // An empty assignment (the daemon's old startup install) named nothing.
    let namespace = if raw_ns.is_empty() {
        None
    } else {
        Some(usable_namespace(raw_ns)?)
    };
    Some((
        namespace,
        format!("{}{suffix}", claude_hook_command(abs_runner)?),
    ))
}

/// OpenCode is set up in the project: its `.opencode` folder or
/// `opencode.json` exists. The installer wires OpenCode only then.
fn opencode_in_project(root: &Path) -> bool {
    root.join(".opencode").exists() || root.join("opencode.json").exists()
}

/// The Claude hook command `node "<abs_runner>"`, or `None` when the runner
/// path holds a character that would end or expand inside the double quotes
/// when the shell runs the hook (`"`, `$`, a backtick; on Unix also `\`).
/// Writing it anyway would let a folder name run commands.
fn claude_hook_command(abs_runner: &str) -> Option<String> {
    let unsafe_char = |c: char| matches!(c, '"' | '$' | '`') || (c == '\\' && !cfg!(windows));
    (!abs_runner.contains(unsafe_char)).then(|| format!("node \"{abs_runner}\""))
}

/// Repairs one Claude hook command the installer wrote (see
/// [`installer_hook_parts`]) to `node "<abs_runner>"`. An older command's
/// namespace, whatever it is, is kept: it is written to Claude's plugin config
/// and read back before the assignment is dropped. When that fails, or the
/// command is not ours, the answer is `None` and the old command stays.
/// The installer repairs through [`repair_claude_hooks`]; only tests call this.
#[cfg(test)]
fn repair_installer_hook_command(
    command: &str,
    abs_runner: &str,
    project_root: &Path,
) -> Option<String> {
    let (namespace, repaired) = installer_hook_parts(command, abs_runner, project_root)?;
    if let Some(ns) = namespace {
        if !write_claude_config(project_root, &ns) {
            return None;
        }
    }
    Some(repaired)
}

/// The Claude tools whose PreToolUse runs the runner for file recall and the
/// guard.
const CLAUDE_FILE_TOOLS_MATCHER: &str = "Read|Write|Edit|MultiEdit|Bash|Task";

/// The NeuroStrata MCP tools whose PreToolUse runs the runner so it can pass
/// the session's project folder as `worktree` (and, with a plugin that does,
/// record the harness's session id on a claim). Claude Code and Codex both
/// name an MCP tool `mcp__<server>__<tool>` in hook payloads and matchers
/// (Codex 0.170: `McpHandler::hook_tool_name_for`, which also accepts an
/// `updatedInput` rewrite of MCP arguments).
const SESSION_SCOPE_MATCHER: &str = "mcp__.*__neurostrata_(task_claim|get_snapshot)";

/// Adds an entry running `cmd` (with `matcher`, when given) to Claude's
/// `event` hooks unless an entry whose matcher passes `matcher_counts`
/// already runs the installer's runner. A user's own entries stay; an event
/// value that is not a list is left alone.
fn ensure_claude_hook(
    hooks: &mut serde_json::Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
    matcher_counts: impl Fn(Option<&str>) -> bool,
    cmd: &str,
    runs: &impl Fn(&str) -> bool,
) {
    let Some(entries) = hooks
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
    else {
        return;
    };
    let present = entries.iter().any(|entry| {
        matcher_counts(entry.get("matcher").and_then(Value::as_str))
            && entry
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|hook| hook.get("command").and_then(Value::as_str))
                .any(runs)
    });
    if present {
        return;
    }
    let mut entry = json!({ "hooks": [{ "type": "command", "command": cmd }] });
    if let Some(m) = matcher {
        entry["matcher"] = Value::String(m.to_string());
    }
    entries.push(entry);
}

/// The order Claude hook events are read in for an older install's
/// namespace; other events follow in the settings' order.
const CLAUDE_HOOK_EVENT_ORDER: [&str; 4] = ["SessionStart", "PreToolUse", "PostToolUse", "Stop"];

/// Calls `f` on every hook of `hooks` (a Claude settings `hooks` object),
/// event by event in [`CLAUDE_HOOK_EVENT_ORDER`].
fn for_each_claude_hook(hooks: &mut serde_json::Map<String, Value>, mut f: impl FnMut(&mut Value)) {
    let mut events: Vec<String> = CLAUDE_HOOK_EVENT_ORDER
        .iter()
        .map(|e| e.to_string())
        .filter(|e| hooks.contains_key(e))
        .collect();
    events.extend(
        hooks
            .keys()
            .filter(|k| !CLAUDE_HOOK_EVENT_ORDER.contains(&k.as_str()))
            .cloned(),
    );
    for event in events {
        let Some(entries) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        for entry in entries {
            if let Some(list) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                list.iter_mut().for_each(&mut f);
            }
        }
    }
}

/// Repairs the installer's own commands in a Claude settings `hooks` object
/// (a Windows verbatim or moved runner path, or a namespace baked into the
/// command) and writes Claude's plugin config. The config names the first
/// namespace an older command baked in (events in
/// [`CLAUDE_HOOK_EVENT_ORDER`]), else `namespace`; it is written once and read
/// back. When that fails, commands that bake a namespace keep it and a
/// warning is added; the hooks are installed anyway. User hooks are left
/// untouched. Returns whether new hook entries may be added: not while old
/// commands keep a baked namespace that a new plain command would lack, and
/// not when the settings run a `cli-hook.mjs` of someone else's install.
fn repair_claude_hooks(
    hooks: &mut serde_json::Map<String, Value>,
    namespace: &str,
    abs_runner: &str,
    project_root: &Path,
    warnings: &mut Vec<String>,
) -> bool {
    let parts = |hook: &Value| {
        hook.get("command")
            .and_then(Value::as_str)
            .and_then(|command| installer_hook_parts(command, abs_runner, project_root))
    };
    let mut all_baked: Vec<String> = Vec::new();
    let mut someone_elses_runner = false;
    for_each_claude_hook(hooks, |hook| {
        let parsed = parts(hook);
        if parsed.is_none() {
            let runner = hook
                .get("command")
                .and_then(Value::as_str)
                .and_then(|c| c.split_once("node \"").map(|(_, rest)| rest))
                .and_then(|rest| rest.split_once('"').map(|(path, _)| path));
            if runner.is_some_and(|r| path_basename(r) == path_basename(abs_runner)) {
                someone_elses_runner = true;
            }
        }
        if let Some(ns) = parsed.and_then(|(ns, _)| ns) {
            if !all_baked.iter().any(|b| b.eq_ignore_ascii_case(&ns)) {
                all_baked.push(ns);
            }
        }
    });
    let baked = all_baked.first().cloned();
    let config_ns = baked.clone().or_else(|| usable_namespace(namespace));
    let config_ok = match &config_ns {
        Some(ns) => write_claude_config(project_root, ns),
        // An empty namespace names nothing to keep.
        None => true,
    };
    if config_ok && all_baked.len() > 1 {
        warnings.push(format!(
            "older Claude hook commands named different namespaces ({}); all of them now use {} from the Claude plugin config {}",
            all_baked.join(", "),
            all_baked[0],
            project_root.join(CLAUDE_CONFIG_REL).display()
        ));
    }
    if !config_ok {
        warnings.push(format!(
            "could not write the Claude plugin config {}; {}",
            project_root.join(CLAUDE_CONFIG_REL).display(),
            if baked.is_some() {
                "hook commands that name NEUROSTRATA_NAMESPACE keep it"
            } else {
                "the Claude hooks run without a configured namespace"
            }
        ));
    }
    for_each_claude_hook(hooks, |hook| {
        if let Some((ns, repaired)) = parts(hook) {
            if ns.is_none() || config_ok {
                hook["command"] = Value::String(repaired);
            }
        }
    });
    (config_ok || baked.is_none()) && !someone_elses_runner
}

/// Codex tools the hook runs for: `apply_patch` edits files and `Bash` is
/// Codex's shell tool (on every platform, including PowerShell on Windows).
const CODEX_PRE_TOOL_USE_MATCHER: &str = "apply_patch|Bash";

/// Quote `value` as one literal word for the shell Codex runs hook commands
/// in: PowerShell on Windows, `sh` elsewhere. Inside single quotes neither
/// expands `$`, backticks or `$(...)`; only an embedded single quote is
/// written differently (`''` in PowerShell, `'\''` in `sh`).
fn shell_literal(value: &str) -> String {
    if cfg!(windows) {
        powershell_literal(value)
    } else {
        posix_literal(value)
    }
}

/// `sh` single-quote rules, independent of the platform the installer runs
/// on: an embedded single quote is written `'\''`. Copilot's `bash` hook
/// command uses them even when the installer runs on Windows.
fn posix_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Read back a word written by `shell_literal`.
#[cfg(test)]
fn shell_literal_value(text: &str) -> Option<(String, &str)> {
    if cfg!(windows) {
        powershell_literal_value(text)
    } else {
        posix_literal_value(text)
    }
}

/// Read a word written by [`posix_literal`]; `None` when `text` does not
/// start with one. Returns the value and the text after it.
fn posix_literal_value(text: &str) -> Option<(String, &str)> {
    let mut rest = text.strip_prefix('\'')?;
    let mut value = String::new();
    loop {
        let close = rest.find('\'')?;
        value.push_str(&rest[..close]);
        rest = &rest[close + 1..];
        // After a closing quote, this continues the same word with a quote.
        match rest.strip_prefix(r"\''") {
            Some(after) => {
                value.push('\'');
                rest = after;
            }
            None => return Some((value, rest)),
        }
    }
}

/// Whether a runner path is the installer's canonical runner for THIS
/// project: equal to `abs_runner` in any spelling, or resolving to it when
/// taken relative to this project root. A monorepo root must not claim a
/// nested project's `cli-hook.mjs` just because its path ends with the
/// installer layout (#368 F9).
fn is_installer_runner(path: &str, project_root: &Path, abs_runner: &str) -> bool {
    path_key(path) == path_key(abs_runner)
        || same_path_on_disk(&project_root.join(path).to_string_lossy(), abs_runner)
}

/// The characters PowerShell treats as a single quote: the ASCII one and
/// the typographic ones (U+2018 to U+201B). Any of them ends a single-quoted
/// string, and one followed by another is one literal quote.
const POWERSHELL_SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'];

/// PowerShell's single-quote rules, independent of the platform the
/// installer runs on: inside single quotes the only escape is a doubled
/// quote, and every character in [`POWERSHELL_SINGLE_QUOTES`] needs it (a
/// path may hold a typographic one). Codex uses `commandWindows` on Windows
/// even when the installer was produced on another OS, so this cannot use
/// `cfg!(windows)` in [`shell_literal`].
fn powershell_literal(value: &str) -> String {
    let mut word = String::with_capacity(value.len() + 2);
    word.push('\'');
    for c in value.chars() {
        if POWERSHELL_SINGLE_QUOTES.contains(&c) {
            word.push(c);
        }
        word.push(c);
    }
    word.push('\'');
    word
}

/// Read a word written by [`powershell_literal`]; `None` when `text` does
/// not start with one. Returns the value and the text after it. As in
/// PowerShell, a quote followed by another quote is one literal quote (the
/// second), and a lone quote ends the word.
fn powershell_literal_value(text: &str) -> Option<(String, &str)> {
    let mut chars = text.char_indices().peekable();
    let (_, open) = chars.next()?;
    if !POWERSHELL_SINGLE_QUOTES.contains(&open) {
        return None;
    }
    let mut value = String::new();
    while let Some((i, c)) = chars.next() {
        if !POWERSHELL_SINGLE_QUOTES.contains(&c) {
            value.push(c);
            continue;
        }
        match chars.peek() {
            Some(&(_, next)) if POWERSHELL_SINGLE_QUOTES.contains(&next) => {
                value.push(next);
                chars.next();
            }
            _ => return Some((value, &text[i + c.len_utf8()..])),
        }
    }
    None
}

/// The two command forms the installer writes into a Codex hook entry:
/// `command` in this platform's shell quoting and `commandWindows` in
/// PowerShell quoting. Neither carries a namespace: the runner discovers it
/// from the harness config.
fn codex_hook_commands(abs_runner: &str) -> (String, String) {
    (
        format!("node {}", shell_literal(abs_runner)),
        format!("node {}", powershell_literal(abs_runner)),
    )
}

/// The installer's Codex `command` value (kept separate so tests can parse
/// the quoting without going through the hook writer).
#[cfg(test)]
fn codex_hook_command(abs_runner: &str) -> String {
    codex_hook_commands(abs_runner).0
}

/// Strip environment prefixes an older installer or a user may put in front
/// of a hook command: `env NAME=x node ...`, `NAME=x node ...`, or the cmd
/// form `set NAME=x&& node ...`. Without this a second hook is appended
/// beside an existing one (#368 F8).
fn strip_installer_env_prefix(mut command: &str) -> &str {
    loop {
        let trimmed = command.trim_start();
        let rest = if let Some(rest) = trimmed.strip_prefix("env ") {
            Some(rest.trim_start())
        } else if let Some(rest) = trimmed.strip_prefix("set NEUROSTRATA_NAMESPACE=") {
            rest.split_once("&&").map(|(_, tail)| tail.trim_start())
        } else if let Some(rest) = trimmed.strip_prefix("NEUROSTRATA_NAMESPACE=") {
            rest.split_once(' ')
                .map(|(_, tail)| tail.trim_start())
                .or_else(|| rest.split_once("&&").map(|(_, tail)| tail.trim_start()))
        } else {
            None
        };
        match rest {
            Some(next) if next.len() < trimmed.len() => command = next,
            _ => return trimmed,
        }
    }
}

/// The runner path a hook command runs after any environment prefix, read in
/// each platform's quoting; empty when it is not `node <one quoted word>`. A
/// word can read differently in the two (`'a’’b'` holds two typographic
/// quotes in `sh`, one in PowerShell), so every full reading is returned and
/// the caller checks each. A command that runs anything after the runner is
/// a user's own and is not claimed.
fn hook_command_runners(command: &str) -> Vec<String> {
    let Some(quoted) = strip_installer_env_prefix(command).strip_prefix("node ") else {
        return Vec::new();
    };
    let parsers: [fn(&str) -> Option<(String, &str)>; 2] =
        [posix_literal_value, powershell_literal_value];
    let mut paths: Vec<String> = Vec::new();
    for parse in parsers {
        if let Some((path, rest)) = parse(quoted) {
            if rest.trim().is_empty() && !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// Whether a `[hooks]` table in Codex's other config layer
/// (`.codex/config.toml`) already runs this project's canonical runner.
/// Codex reads both layers; wiring a second entry would run the guard twice.
/// Only a real hook `command` is evidence: a comment or an unrelated setting
/// that merely mentions the runner path wires nothing. Malformed TOML names
/// no hook.
fn config_toml_wires_runner(
    text: &str,
    project_root: &Path,
    abs_runner: &str,
    event: &str,
) -> bool {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return false;
    };
    let Some(hooks) = value.get("hooks") else {
        return false;
    };
    let event_key = event.to_ascii_lowercase().replace(['_', '-'], "");
    let folded_runner = path_key(abs_runner).replace('\\', "/").to_ascii_lowercase();
    let folded_root = format!(
        "{}/.neurostrata/plugins/codex/cli-hook.mjs",
        path_key(&project_root.to_string_lossy())
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    );
    let mentions_runner = |part: &str| {
        let folded = part.replace('\\', "/").to_ascii_lowercase();
        folded.contains(&folded_runner) || folded.contains(&folded_root)
    };
    let covers_event = |key: &str| key.to_ascii_lowercase().replace(['_', '-'], "") == event_key;
    let command_is_ours = |table: &toml::Table| {
        table.get("command").is_some_and(|command| match command {
            toml::Value::String(s) => mentions_runner(s),
            toml::Value::Array(items) => items
                .iter()
                .any(|item| item.as_str().is_some_and(mentions_runner)),
            _ => false,
        })
    };
    match hooks {
        // `[hooks.pre_tool_use] command = ...` (or `[hooks] command = ...`).
        toml::Value::Table(table) => {
            if command_is_ours(table) {
                return true;
            }
            table.iter().any(|(key, value)| {
                if !covers_event(key) {
                    return false;
                }
                match value {
                    toml::Value::Table(inner) => command_is_ours(inner),
                    toml::Value::Array(items) => items
                        .iter()
                        .any(|item| item.as_table().is_some_and(command_is_ours)),
                    _ => false,
                }
            })
        }
        // `[[hooks]] event = "..." command = ...`.
        toml::Value::Array(items) => items.iter().any(|item| {
            let Some(table) = item.as_table() else {
                return false;
            };
            let event_matches = table
                .get("event")
                .and_then(toml::Value::as_str)
                .is_some_and(covers_event);
            event_matches && command_is_ours(table)
        }),
        _ => false,
    }
}

/// Add the NeuroStrata hooks to `.codex/hooks.json`, keeping every other
/// entry. A command running this project's canonical runner is rewritten to
/// the current form (both command variants), so a stale spelling is repaired
/// and nothing is added twice; a foreign runner is left as-is. Codex's other
/// hook layer (`.codex/config.toml` [`hooks`]) is checked so a hook already
/// there is not duplicated. Anything unreadable or not shaped as expected
/// leaves the file alone with a warning (#368 F6).
/// SessionStart injects the session file; UserPromptSubmit and PostToolUse
/// carry the working-set prompt/result projection; SessionEnd and PreCompact
/// write the handoff note.
const CODEX_HOOK_EVENTS: [(&str, Option<&str>); 8] = [
    ("SessionStart", None),
    ("PreToolUse", Some(CODEX_PRE_TOOL_USE_MATCHER)),
    ("PreToolUse", Some(SESSION_SCOPE_MATCHER)),
    ("UserPromptSubmit", None),
    ("PostToolUse", None),
    ("Stop", None),
    ("SessionEnd", None),
    ("PreCompact", None),
];

/// Which of the installer's Codex entries for `event` an entry with
/// `entry_matcher` stands for: the one with the same matcher, else (a matcher
/// the user changed) the event's first one, so it is not added again.
fn codex_entry_slot<'a>(event: &str, entry_matcher: Option<&'a str>) -> Option<&'a str> {
    let ours: Vec<Option<&str>> = CODEX_HOOK_EVENTS
        .iter()
        .filter(|(e, _)| *e == event)
        .map(|(_, m)| *m)
        .collect();
    if ours.contains(&entry_matcher) {
        entry_matcher
    } else {
        ours.first().copied().flatten()
    }
}

/// Add the NeuroStrata hooks ([CODEX_HOOK_EVENTS]) to `.codex/hooks.json`,
/// keeping every other entry.
/// runner is rewritten to the current form (both command variants), so a
/// stale spelling is repaired and nothing is added twice; a foreign runner
/// is left as-is. Codex's other
/// hook layer (`.codex/config.toml` `[hooks]`) is checked so a hook already
/// there is not duplicated. Anything unreadable or not shaped as expected
/// is left untouched with a warning. `Some(path)` means a hook was actually
/// wired; metadata written elsewhere does not count (#368 F6).
fn wire_codex_hooks(
    project_root: &Path,
    abs_runner: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let hooks_path = project_root.join(".codex/hooks.json");
    // A repo-controlled symlink at any component would redirect the write
    // into the user's home config (e.g. a tracked `.codex -> ../.codex`).
    if let Err(e) = refuse_symlink_components(project_root, &hooks_path) {
        warnings.push(format!(
            "{} refused: {e}; no Codex hook was wired",
            hooks_path.display()
        ));
        return None;
    }
    let mut doc: Value = match read_config(&hooks_path) {
        Ok(Some(d)) if d.is_object() => d,
        Ok(Some(_)) => {
            warnings.push(format!(
                "{} is not a JSON object; no Codex hook was wired",
                hooks_path.display()
            ));
            return None;
        }
        Ok(None) => json!({}),
        Err(e) => {
            warnings.push(format!("{e}; no Codex hook was wired"));
            return None;
        }
    };
    let toml = std::fs::read_to_string(project_root.join(".codex/config.toml")).unwrap_or_default();
    let (cmd, cmd_windows) = codex_hook_commands(abs_runner);
    let obj = doc.as_object_mut()?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(h_obj) = hooks.as_object_mut() else {
        warnings.push(format!(
            "{} has a non-object hooks value; no Codex hook was wired",
            hooks_path.display()
        ));
        return None;
    };
    if let Some(event) = non_array_hook_list(h_obj) {
        warnings.push(format!(
            "{} has a non-array {event} hook list, which disables the whole file; no Codex hook was wired",
            hooks_path.display()
        ));
        return None;
    }
    for (event, matcher) in CODEX_HOOK_EVENTS {
        let entries = h_obj.entry(event).or_insert_with(|| json!([]));
        let Some(arr) = entries.as_array_mut() else {
            continue;
        };
        // `[hooks]` in config.toml names no matcher this check reads: it
        // stands for the event's first entry only. Other entries (the
        // session-scope matcher) still belong in hooks.json.
        let first_slot = CODEX_HOOK_EVENTS
            .iter()
            .find(|(e, _)| *e == event)
            .is_some_and(|(_, m)| *m == matcher);
        if first_slot && config_toml_wires_runner(&toml, project_root, abs_runner, event) {
            arr.retain_mut(|entry| {
                if entry.get("matcher").and_then(Value::as_str) != matcher {
                    return true;
                }
                let Some(entry_hooks) = entry.get_mut("hooks").and_then(|v| v.as_array_mut())
                else {
                    return true;
                };
                let mut removed_owned = false;
                entry_hooks.retain(|hook| {
                    let owned = ["command", "commandWindows"].iter().any(|key| {
                        hook.get(key)
                            .and_then(Value::as_str)
                            .map(hook_command_runners)
                            .is_some_and(|paths| {
                                paths.iter().any(|old_path| {
                                    is_installer_runner(old_path, project_root, abs_runner)
                                })
                            })
                    });
                    removed_owned |= owned;
                    !owned
                });
                !(removed_owned && entry_hooks.is_empty())
            });
        }
        let mut present = false;
        for entry in arr.iter_mut() {
            // An owned hook counts only for the entry it stands for (see
            // codex_entry_slot), so each of an event's entries is checked.
            let entry_matcher = entry
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::to_string);
            let this_slot = codex_entry_slot(event, entry_matcher.as_deref()) == matcher;
            let Some(entry_hooks) = entry.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
                continue;
            };
            for hook in entry_hooks {
                let mut owned = false;
                for (key, target) in [("command", &cmd), ("commandWindows", &cmd_windows)] {
                    let Some(command) = hook.get(key).and_then(Value::as_str) else {
                        continue;
                    };
                    if !hook_command_runners(command)
                        .iter()
                        .any(|old_path| is_installer_runner(old_path, project_root, abs_runner))
                    {
                        continue;
                    }
                    owned = true;
                    if command != target {
                        hook[key] = Value::String(target.clone());
                    }
                }
                if !owned {
                    continue;
                }
                // An older install may have written only `command`. Once the
                // hook is recognized as ours, both platform variants must be
                // present or Windows keeps running a POSIX-quoted command.
                if hook.get("command").is_none() {
                    hook["command"] = Value::String(cmd.clone());
                }
                if hook.get("commandWindows").is_none() {
                    hook["commandWindows"] = Value::String(cmd_windows.clone());
                }
                if this_slot {
                    present = true;
                }
            }
        }
        if !present
            && first_slot
            && config_toml_wires_runner(&toml, project_root, abs_runner, event)
        {
            present = true;
        }
        if !present {
            let hook_entry = json!({
                "type": "command",
                "command": cmd,
                "commandWindows": cmd_windows
            });
            let mut entry = json!({ "hooks": [hook_entry] });
            if let Some(m) = matcher {
                entry["matcher"] = json!(m);
            }
            arr.push(entry);
        }
    }
    let serialized = serde_json::to_string_pretty(&doc).ok()?;
    let needs_write = std::fs::read_to_string(&hooks_path)
        .map(|existing| existing != serialized)
        .unwrap_or(true);
    if needs_write {
        write_atomically(&hooks_path, &serialized).ok()?;
    }
    // hooks.json is untracked and carries an absolute runner path. Keep it
    // out of a commit by listing it in the checkout's local exclude file
    // (#368 F10). Best effort: a bare directory has no git.
    exclude_local_file(project_root, ".codex/hooks.json");
    Some(hooks_path.to_string_lossy().to_string())
}

/// The first event in a hooks object whose list is not an array. Codex and
/// Copilot CLI each parse the hooks file as a whole, so one such list
/// disables every hook in the file (Copilot's hooks reference: "a non-array
/// event list" rejects "the entire file"). The installer then leaves the
/// file as it is rather than add hooks that would never run.
fn non_array_hook_list(hooks: &serde_json::Map<String, Value>) -> Option<&str> {
    hooks
        .iter()
        .find(|(_, list)| !list.is_array())
        .map(|(event, _)| event.as_str())
}

/// Warn when a harness config beside a hooks file was not written: the hooks
/// still run, and the runner takes the namespace from the project marker or
/// discovers it.
fn warn_unwritten_config(written: Option<String>, cfg_path: &Path, warnings: &mut Vec<String>) {
    if written.is_none() {
        warnings.push(format!(
            "{} could not be written; the hook runner takes the namespace from the project marker or discovers it",
            cfg_path.display()
        ));
    }
}

/// Add `/<rel>` to this checkout's `.git/info/exclude` once (`rel` is a path
/// relative to the checkout, with `/` separators). A linked worktree's
/// `.git` is a file; git reads `info/exclude` from the common git directory
/// every worktree shares ([`crate::paths::common_git_dir`]), so the entry
/// goes there and covers each worktree's own copy of the file. A folder
/// that is not a git checkout gets no exclude file.
fn exclude_local_file(project_root: &Path, rel: &str) {
    let dot_git = project_root.join(".git");
    let (exclude, symlink_root) = if dot_git.is_file() {
        let Some(common) = crate::paths::common_git_dir(&project_root.to_string_lossy()) else {
            return;
        };
        let common = PathBuf::from(common);
        (common.join("info").join("exclude"), common)
    } else if dot_git.is_dir() {
        (
            dot_git.join("info").join("exclude"),
            project_root.to_path_buf(),
        )
    } else {
        // Not a git checkout: creating `.git/info` here would make the folder
        // look like a repository root to every `.git` probe.
        return;
    };
    if refuse_symlink_components(&symlink_root, &exclude).is_err() {
        return;
    }
    if let Some(parent) = exclude.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let anchored = format!("/{rel}");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim() == anchored || line.trim() == rel)
    {
        return;
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&anchored);
    updated.push('\n');
    let _ = write_atomically(&exclude, &updated);
}

/// The Copilot CLI hooks file the installer owns. Copilot CLI loads every
/// `.github/hooks/*.json` of the repository (`enableFileHooks`: "repository-
/// origin file hooks from `.github/hooks/`"), so NeuroStrata keeps its own
/// file there instead of editing one of the user's.
const COPILOT_HOOKS_REL: &str = ".github/hooks/neurostrata.json";

/// The Copilot CLI hook events the installer wires (camelCase, hooks file
/// version 1). sessionStart lets the runner inject the session file;
/// sessionEnd and preCompact let it write the handoff note. postToolUse lets
/// it deliver the session-start text a slow start missed and left-out rules:
/// its `additionalContext` is appended to the tool result, while a preToolUse
/// reply carries no text on an allowed call. preToolUse and postToolUse are
/// wired only for a bundle that handles them ([`copilot_hook_events`]).
const COPILOT_HOOK_EVENTS: [&str; 5] = [
    "sessionStart",
    "preToolUse",
    "postToolUse",
    "sessionEnd",
    "preCompact",
];

/// The Copilot events wired for the plugin bundle the installer ships:
/// preToolUse only when the bundle exports `copilotPreToolUseReply`, and
/// postToolUse only when it exports `handlePostToolUseEvent`. A bundle
/// without the first answers preToolUse in a shape Copilot ignores; without
/// the second the runner answers `{}`. Either way the hook would only start
/// node on every tool call.
fn copilot_hook_events(bundle: &str) -> impl Iterator<Item = &'static str> + '_ {
    COPILOT_HOOK_EVENTS
        .into_iter()
        .filter(move |event| match *event {
            "preToolUse" => bundle_exports(bundle, "copilotPreToolUseReply"),
            "postToolUse" => bundle_exports(bundle, "handlePostToolUseEvent"),
            _ => true,
        })
}

/// Whether a plugin bundle exports `name` as a top-level `export function`,
/// `export async function`, `export const` or `export let` declaration (the
/// bundles are tsc output and declare every export that way).
fn bundle_exports(bundle: &str, name: &str) -> bool {
    bundle.lines().any(|line| {
        let Some(rest) = line.strip_prefix("export ") else {
            return false;
        };
        let rest = rest.strip_prefix("async ").unwrap_or(rest);
        ["function ", "const ", "let "].iter().any(|kind| {
            rest.strip_prefix(kind)
                .and_then(|declared| declared.strip_prefix(name))
                .is_some_and(|tail| {
                    !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$')
                })
        })
    })
}

/// The environment variable that tells the runner which Copilot event it
/// handles: Copilot's hook input does not name the event.
const COPILOT_HOOK_EVENT_ENV: &str = "NEUROSTRATA_HOOK_EVENT";

/// Add the NeuroStrata hooks ([`copilot_hook_events`] for `bundle`, the
/// plugin bundle the runner loads) to [`COPILOT_HOOKS_REL`]: per event one
/// `{"type": "command", "bash", "powershell", "env"}` entry running the
/// runner (`bash` in `sh` quoting, `powershell` in PowerShell quoting, no
/// namespace), with the event name in `env`. Other entries and keys stay; an
/// entry running this project's runner is repaired in place, so nothing is
/// added twice and a second run writes nothing. For an event the bundle does
/// not handle, the runner's commands are removed instead (an entry left with
/// no command is dropped, then an emptied list). A file that is unreadable,
/// not an object, or of another `version` is left alone with a warning.
/// `Some(path)` means the hooks are wired.
fn wire_copilot_hooks(
    project_root: &Path,
    abs_runner: &str,
    bundle: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let hooks_path = project_root.join(COPILOT_HOOKS_REL);
    if let Err(e) = refuse_symlink_components(project_root, &hooks_path) {
        warnings.push(format!(
            "{} refused: {e}; no Copilot hook was wired",
            hooks_path.display()
        ));
        return None;
    }
    let mut doc: Value = match read_config(&hooks_path) {
        Ok(Some(d)) if d.is_object() => d,
        Ok(Some(_)) => {
            warnings.push(format!(
                "{} is not a JSON object; no Copilot hook was wired",
                hooks_path.display()
            ));
            return None;
        }
        Ok(None) => json!({}),
        Err(e) => {
            warnings.push(format!("{e}; no Copilot hook was wired"));
            return None;
        }
    };
    let obj = doc.as_object_mut()?;
    match obj.get("version") {
        None => {
            obj.insert("version".to_string(), json!(1));
        }
        Some(v) if v == &json!(1) => {}
        Some(_) => {
            warnings.push(format!(
                "{} has a version other than 1; no Copilot hook was wired",
                hooks_path.display()
            ));
            return None;
        }
    }
    let bash = format!("node {}", posix_literal(abs_runner));
    let powershell = format!("node {}", powershell_literal(abs_runner));
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(h_obj) = hooks.as_object_mut() else {
        warnings.push(format!(
            "{} has a non-object hooks value; no Copilot hook was wired",
            hooks_path.display()
        ));
        return None;
    };
    if let Some(event) = non_array_hook_list(h_obj) {
        warnings.push(format!(
            "{} has a non-array {event} hook list, which disables the whole file; no Copilot hook was wired",
            hooks_path.display()
        ));
        return None;
    }
    let runs_ours = |command: &str| {
        hook_command_runners(command).iter().any(|path| {
            is_installer_runner(path, project_root, abs_runner)
                || runner_is_installers_hook(path, project_root, abs_runner)
        })
    };
    let wired: Vec<&str> = copilot_hook_events(bundle).collect();
    for event in COPILOT_HOOK_EVENTS {
        if !wired.contains(&event) {
            let Some(entries) = h_obj.get_mut(event).and_then(Value::as_array_mut) else {
                continue;
            };
            let had = entries.len();
            entries.retain_mut(|entry| {
                let Some(fields) = entry.as_object_mut() else {
                    return true;
                };
                let before = fields.len();
                fields.retain(|key, command| {
                    !(matches!(key.as_str(), "bash" | "powershell")
                        && command.as_str().is_some_and(runs_ours))
                });
                before == fields.len()
                    || fields.contains_key("bash")
                    || fields.contains_key("powershell")
            });
            if had > 0 && entries.is_empty() {
                h_obj.remove(event);
            }
            continue;
        }
        let Some(entries) = h_obj
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
        else {
            continue;
        };
        let mut present = false;
        for entry in entries.iter_mut() {
            if !entry.is_object() {
                continue;
            }
            // As for Codex: only a command that runs this project's runner
            // is rewritten; a user's own command in the other shell's key
            // stays, and a missing key is filled once the entry is ours.
            let mut owned = false;
            for (key, target) in [("bash", &bash), ("powershell", &powershell)] {
                let Some(command) = entry.get(key).and_then(Value::as_str) else {
                    continue;
                };
                if !runs_ours(command) {
                    continue;
                }
                owned = true;
                if command != target.as_str() {
                    entry[key] = json!(target);
                }
            }
            if !owned {
                continue;
            }
            for (key, target) in [("bash", &bash), ("powershell", &powershell)] {
                if entry.get(key).is_none() {
                    entry[key] = json!(target);
                }
            }
            entry["type"] = json!("command");
            if !entry.get("env").is_some_and(Value::is_object) {
                entry["env"] = json!({});
            }
            entry["env"][COPILOT_HOOK_EVENT_ENV] = json!(event);
            present = true;
        }
        if !present {
            entries.push(json!({
                "type": "command",
                "bash": bash,
                "powershell": powershell,
                "env": { (COPILOT_HOOK_EVENT_ENV): event }
            }));
        }
    }
    let serialized = serde_json::to_string_pretty(&doc).ok()?;
    let needs_write = std::fs::read_to_string(&hooks_path)
        .map(|existing| existing != serialized)
        .unwrap_or(true);
    if needs_write {
        if let Some(parent) = hooks_path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        write_atomically(&hooks_path, &serialized).ok()?;
    }
    // The file carries an absolute runner path: keep it out of commits.
    exclude_local_file(project_root, COPILOT_HOOKS_REL);
    Some(hooks_path.to_string_lossy().to_string())
}

fn has_any_env(keys: &[&str]) -> bool {
    keys.iter()
        .any(|k| std::env::var_os(k).map(|v| !v.is_empty()).unwrap_or(false))
}

/// Where a resolved project namespace came from, in resolution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceSource {
    /// The project root marker (`.NeuroStrata/config.json`).
    Marker,
    /// A harness plugin config the installer wrote.
    HarnessConfig,
    /// `NEUROSTRATA_NAMESPACE` in the environment.
    Env,
    /// The `namespace` field of `.NeuroStrata/session_queries.json`.
    Ledger,
    /// The checkout's (or directory's) folder name, the last resort.
    FolderName,
}

impl NamespaceSource {
    /// Whether someone named this namespace (marker, harness config,
    /// environment), so a hook refresh may copy it into harness configs. A
    /// ledger or folder-name answer is a fallback: once written into a config
    /// it would outrank every explicit source from then on.
    pub fn is_explicit(self) -> bool {
        matches!(
            self,
            NamespaceSource::Marker | NamespaceSource::HarnessConfig | NamespaceSource::Env
        )
    }
}

/// A harness config that exists but could not be read or parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableConfig {
    pub path: PathBuf,
    pub reason: String,
}

impl std::fmt::Display for UnreadableConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} could not be read: {}",
            self.path.display(),
            self.reason
        )
    }
}

/// Why no namespace could be resolved for a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The directory does not exist or is not a directory.
    NotADirectory(PathBuf),
    /// A harness config exists but could not be read, so the namespace it
    /// names is unknown and no answer is safe.
    UnreadableConfigs(Vec<UnreadableConfig>),
    /// The project root marker (`.NeuroStrata/config.json`) that applies
    /// cannot be read, so the namespace it names is unknown.
    UnreadableMarker(UnreadableConfig),
    /// No source names a namespace and the folder name is not one.
    NoUsableNamespace { folder: String },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotADirectory(p) => write!(f, "{} is not a directory", p.display()),
            ResolveError::UnreadableConfigs(list) => {
                let listed: Vec<String> = list.iter().map(|c| c.to_string()).collect();
                write!(
                    f,
                    "the project's namespace is unknown because a harness config could not be read: {}. Fix or remove the file",
                    listed.join("; ")
                )
            }
            ResolveError::UnreadableMarker(marker) => write!(
                f,
                "the project's namespace is unknown because the project marker {} could not be read: {}. Fix or remove the file",
                marker.path.display(),
                marker.reason
            ),
            ResolveError::NoUsableNamespace { folder } => write!(
                f,
                "no namespace is configured and the folder name {folder:?} is not one (letters, digits, `-`, `_`, `.` and `:`, starting with a letter or digit); set NEUROSTRATA_NAMESPACE or configure a harness"
            ),
        }
    }
}

/// A resolved namespace, where it came from, and every harness config that
/// names one (two that disagree mean the first was used).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNamespace {
    pub namespace: String,
    pub source: NamespaceSource,
    /// `(harness, namespace)` in the fixed order (Claude, opencode, codex,
    /// hermes, openclaw, github-cli).
    pub configs: Vec<(String, String)>,
}

const CLAUDE_SETTINGS: &str = ".claude/settings.local.json";

/// Each harness config in the fixed order used when several name different
/// namespaces: Claude (handled apart, see [`harness_config_namespace`]), then
/// opencode, codex, hermes, openclaw, github-cli. Each entry is the harness,
/// the config file relative to the project root, and the JSON pointer of the
/// namespace inside it.
const PLUGIN_NAMESPACE_CONFIGS: [(&str, &str, &str); 5] = [
    (
        "opencode",
        ".opencode/neurostrata-plugin.json",
        "/neurostrata/namespace",
    ),
    ("codex", ".codex/plugins/neurostrata.json", "/namespace"),
    ("hermes", ".hermes/plugins/neurostrata.json", "/namespace"),
    (
        "openclaw",
        ".openclaw/plugins/neurostrata.json",
        "/namespace",
    ),
    (
        "github-cli",
        ".NeuroStrata/plugins/github-cli/copilot-extension.json",
        "/namespace",
    ),
];

/// A JSON config file: `Ok(None)` when it does not exist, an error naming the
/// file when it exists but cannot be read or parsed (a UTF-8 byte order mark,
/// as Windows PowerShell 5.1 writes, included).
fn read_config(path: &Path) -> Result<Option<Value>, UnreadableConfig> {
    read_config_inner(path, false)
}

/// The OpenCode loader skips a malformed shared config and tries the next
/// file. Probing those same files for a namespace must do the same, while an
/// unreadable (not merely malformed) config remains an error.
fn read_shared_config(path: &Path) -> Result<Option<Value>, UnreadableConfig> {
    read_config_inner(path, true)
}

fn read_config_inner(path: &Path, skip_malformed: bool) -> Result<Option<Value>, UnreadableConfig> {
    let unreadable = |reason: String| UnreadableConfig {
        path: path.to_path_buf(),
        reason,
    };
    // One open, checked through its own handle, so the file cannot be
    // swapped between the check and the read. Non-blocking on Unix, so
    // opening a FIFO (or a link to one) returns at once; the handle's
    // metadata then rejects it, and the read never takes more than the limit.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    let meta = file.metadata().map_err(|e| unreadable(e.to_string()))?;
    if !meta.is_file() {
        return Err(unreadable("it is not a regular file".to_string()));
    }
    let too_big = || {
        unreadable(format!(
            "it is larger than the {MAX_CONFIG_BYTES}-byte config limit"
        ))
    };
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, MAX_CONFIG_BYTES + 1),
        &mut bytes,
    )
    .map_err(|e| unreadable(e.to_string()))?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(too_big());
    }
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        if skip_malformed {
            return Ok(None);
        }
        return Err(unreadable(
            "it starts with a UTF-8 byte order mark; save it as UTF-8 without one".to_string(),
        ));
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) if skip_malformed => return Ok(None),
        Err(_) => return Err(unreadable("it is not UTF-8".to_string())),
    };
    match serde_json::from_str(&text) {
        Ok(value) => Ok(Some(value)),
        Err(_) if skip_malformed => Ok(None),
        Err(e) => Err(unreadable(format!("it is not valid JSON ({e})"))),
    }
}

/// The runner the installer writes for `harness` under `root`.
fn installed_runner(root: &Path, harness: &str) -> PathBuf {
    root.join(".NeuroStrata")
        .join("plugins")
        .join(harness)
        .join("cli-hook.mjs")
}

/// The namespace in a Claude hook command written by the installer, in its
/// exact shape `NEUROSTRATA_NAMESPACE=<ns> node "<runner>"`, where the runner
/// is the installer's own for `root` (in any spelling) or a missing
/// `cli-hook.mjs` the next refresh repairs. A hook of anyone else's that sets
/// the variable for its own script is no answer.
fn installer_hook_namespace(command: &str, root: &Path, abs_runner: &str) -> Option<String> {
    let rest = command.strip_prefix("NEUROSTRATA_NAMESPACE=")?;
    let (ns, rest) = rest.split_once(" node \"")?;
    let ns = usable_namespace(ns)?;
    let runner = rest.strip_suffix('"')?;
    if runner.contains('"') || path_basename(runner) != path_basename(abs_runner) {
        return None;
    }
    // A missing runner counts as ours only at the installer's plugin
    // location: any other `cli-hook.mjs` is someone else's hook and must not
    // supply this project's namespace (#378 F6, #388).
    runner_is_installers_hook(runner, root, abs_runner).then_some(ns)
}

/// Claude's namespace in its settings: `NEUROSTRATA_NAMESPACE` in the `env`
/// block, else the first hook command the installer wrote.
fn claude_settings_namespace(root: &Path, settings: &Value) -> Option<String> {
    if let Some(ns) = settings
        .pointer("/env/NEUROSTRATA_NAMESPACE")
        .and_then(Value::as_str)
        .and_then(usable_namespace)
    {
        return Some(ns);
    }
    let abs_runner = plain_abs_path(&installed_runner(root, "claude"));
    let hooks = settings.get("hooks").and_then(Value::as_object)?;
    hooks
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .flat_map(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
        .find_map(|command| installer_hook_namespace(command, root, &abs_runner))
}

/// The usable namespace `harness`'s config under `root` names: `Ok(None)` when
/// the config is missing or names none, an error when it exists but cannot be
/// read or parsed.
fn harness_config_namespace(
    root: &Path,
    harness: &str,
) -> Result<Option<String>, UnreadableConfig> {
    if harness == "claude" {
        // Claude's plugin config first; older installs kept the namespace in
        // the settings (the `env` block or the hook command).
        if let Some(ns) = claude_config_namespace_checked(root)? {
            return Ok(Some(ns));
        }
        return Ok(read_config(&root.join(CLAUDE_SETTINGS))?
            .and_then(|settings| claude_settings_namespace(root, &settings)));
    }
    // OpenCode actually loads `.NeuroStrata/neurostrata-plugin.json` (then
    // `PluginConfig.json`) through the plugin core's `loadPluginConfigFromDisk`.
    // The `.opencode/neurostrata-plugin.json` registration below is historic
    // metadata the adapter never reads; when the effective config names a
    // namespace it is the authority, so a stale registration cannot make the
    // hooks resolve a different one.
    //
    // Every harness's runner loads that shared config, so it counts as
    // OpenCode's only where OpenCode is set up in the project, and only the
    // first file the loader picks counts (it never reads the second). The
    // plugin core's `resolveConfig` reads only the top-level `namespace`.
    if harness == "opencode" && opencode_in_project(root) {
        for rel in [
            ".NeuroStrata/neurostrata-plugin.json",
            ".NeuroStrata/PluginConfig.json",
        ] {
            match read_shared_config(&root.join(rel))? {
                // The first file the loader accepts is the effective one. Its
                // top-level namespace decides, even when it names none: an
                // older `.opencode/...` registration must not win by falling
                // through (#388). A malformed file is skipped just as the
                // loader skips it and tries the next one.
                Some(config) => {
                    return Ok(config
                        .get("namespace")
                        .and_then(Value::as_str)
                        .and_then(usable_namespace));
                }
                None => continue,
            }
        }
    }
    let Some((_, rel, pointer)) = PLUGIN_NAMESPACE_CONFIGS
        .iter()
        .find(|(h, _, _)| *h == harness)
    else {
        return Ok(None);
    };
    Ok(read_config(&root.join(rel))?
        .as_ref()
        .and_then(|v| v.pointer(pointer))
        .and_then(Value::as_str)
        .and_then(usable_namespace))
}

/// Every namespace the harness installer recorded in the checkout at `root`,
/// as `(harness, namespace)` in the fixed order, and every config that exists
/// but could not be read. These are the files [`auto_install_detected_plugins`]
/// writes; this reads them back. A config that names no usable namespace is
/// left out.
fn harness_namespaces_at(root: &Path) -> (Vec<(String, String)>, Vec<UnreadableConfig>) {
    let mut found = Vec::new();
    let mut unreadable = Vec::new();
    let harnesses =
        std::iter::once("claude").chain(PLUGIN_NAMESPACE_CONFIGS.iter().map(|(h, _, _)| *h));
    for harness in harnesses {
        match harness_config_namespace(root, harness) {
            Ok(Some(ns)) => found.push((harness.to_string(), ns)),
            Ok(None) => {}
            Err(e) => unreadable.push(e),
        }
    }
    (found, unreadable)
}

/// A namespace value as written, or `None` when it is not one: empty, still
/// an unexpanded shell or cmd variable (`$NS`, `%NS%`), quoted, or carrying
/// anything but `[A-Za-z0-9_.:-]`.
pub fn usable_namespace(ns: &str) -> Option<String> {
    // The value ends up in hook commands and in `task gate <ns>`: only plain
    // namespace characters are taken, so it can never carry shell syntax. It
    // starts with a letter or digit, so it is never read as an option
    // (`--help`) or as a path step (`.`, `..`).
    let plain = ns
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    let leads = ns.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    (plain && leads).then(|| ns.to_string())
}

/// [`usable_namespace`] for a value from the environment, where a shell user
/// may have kept the quotes (`NEUROSTRATA_NAMESPACE='"alpha"'`) or a space:
/// surrounding whitespace and one pair of matching quotes are dropped first.
/// A config file value gets no such allowance.
pub fn usable_env_namespace(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|&q| raw.strip_prefix(q).and_then(|r| r.strip_suffix(q)))
        .unwrap_or(raw);
    usable_namespace(unquoted)
}

/// The places a project's namespace is looked up for `dir`: `dir`'s checkout
/// root, its main checkout (for a linked worktree), and, outside git, `dir`
/// itself; plus `dir` made absolute.
struct NamespaceRoots {
    dir: PathBuf,
    own_root: Option<PathBuf>,
    main_root: Option<PathBuf>,
    outside_git: Option<PathBuf>,
}

impl NamespaceRoots {
    /// A relative `dir` is taken against the current directory; a `dir` that
    /// is not an existing directory is an error, never a made-up answer.
    fn of(dir: &Path) -> Result<Self, ResolveError> {
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(dir))
                .unwrap_or_else(|_| dir.to_path_buf())
        };
        if !dir.is_dir() {
            return Err(ResolveError::NotADirectory(dir));
        }
        let dir = dunce::canonicalize(&dir).unwrap_or(dir);
        let dir_s = dir.to_string_lossy().to_string();
        let own_root = crate::paths::git_root_of_dir(&dir_s).map(PathBuf::from);
        let main_root = crate::paths::main_checkout_root(&dir_s);
        // Outside git, `dir` itself is the only place to look.
        let outside_git = (own_root.is_none() && main_root.is_none()).then(|| dir.clone());
        Ok(NamespaceRoots {
            dir,
            own_root,
            main_root,
            outside_git,
        })
    }

    /// Harness settings are untracked, so a linked worktree usually has none
    /// of its own: its own checkout first, then the main checkout.
    fn config_order(&self) -> Vec<&PathBuf> {
        let mut order: Vec<&PathBuf> = Vec::new();
        // A nested directory keeps its own harness config. Try the path below
        // the checkout root before the root's, as root-marker lookups do.
        order.push(&self.dir);
        for root in self
            .own_root
            .iter()
            .chain(self.main_root.iter())
            .chain(self.outside_git.iter())
        {
            if !order.contains(&root) {
                order.push(root);
            }
        }
        order
    }

    /// The ledger: the main checkout's first, then `dir`'s own.
    fn ledger_order(&self) -> impl Iterator<Item = &PathBuf> {
        self.main_root
            .iter()
            .chain(self.own_root.iter())
            .chain(self.outside_git.iter())
    }

    /// Every harness config in every place in [`Self::config_order`], in that
    /// fixed order. A linked worktree and its main checkout can each name a
    /// namespace; stopping at the first root with any left the other one out
    /// of the push gate. A config that could not be read anywhere looked at
    /// fails the lookup: the namespace it names might be the answer.
    fn harness_namespaces(&self) -> Result<Vec<(String, String)>, Vec<UnreadableConfig>> {
        let mut found = Vec::new();
        let mut unreadable = Vec::new();
        for root in self.config_order() {
            let (mut here, errors) = harness_namespaces_at(root);
            unreadable.extend(errors);
            found.append(&mut here);
        }
        if unreadable.is_empty() {
            Ok(found)
        } else {
            Err(unreadable)
        }
    }
}

/// The namespace for `dir` and where it came from, in this order:
/// 0. the project root marker (`.NeuroStrata/config.json`), found as
///    [`crate::root_marker::find_root_marker`] does;
/// 1. a harness config (the first in the fixed order), read from the first
///    of `dir` itself, its checkout, its main checkout, or (outside git)
///    `dir` that has any;
/// 2. a project marker (`.NeuroStrata/config.json`), from the same roots;
/// 3. `env_namespace` (`NEUROSTRATA_NAMESPACE`), when usable;
/// 4. the `namespace` field of the graph-lookup ledger
///    (`.NeuroStrata/session_queries.json`), the main checkout's then `dir`'s;
/// 5. the main checkout's folder name; outside git, `dir`'s folder name, when
///    it is a usable namespace.
///
/// A marker or harness config that exists but cannot be read is an error,
/// never skipped: the namespace it names is unknown.
fn resolve_namespace_from(
    dir: &Path,
    env_namespace: Option<&str>,
) -> Result<ResolvedNamespace, ResolveError> {
    let roots = NamespaceRoots::of(dir)?;
    let marker = crate::root_marker::find_root_marker(&roots.dir).map_err(|(path, reason)| {
        ResolveError::UnreadableMarker(UnreadableConfig { path, reason })
    })?;
    if let Some(found) = marker {
        // The marker decides alone: a harness config that cannot be read
        // does not change the answer, and is not listed.
        return Ok(ResolvedNamespace {
            namespace: found.config.namespace,
            source: NamespaceSource::Marker,
            configs: roots.harness_namespaces().unwrap_or_default(),
        });
    }
    let configs = roots
        .harness_namespaces()
        .map_err(ResolveError::UnreadableConfigs)?;
    let resolved = |namespace: String, source: NamespaceSource| ResolvedNamespace {
        namespace,
        source,
        configs: configs.clone(),
    };
    if let Some((_, ns)) = configs.first() {
        return Ok(resolved(ns.clone(), NamespaceSource::HarnessConfig));
    }
    // The project marker is an explicit answer too: a project can name its
    // namespace in `.NeuroStrata/config.json` without any harness config.
    for root in roots.config_order() {
        let marker = root.join(".NeuroStrata").join("config.json");
        if let Some(ns) = std::fs::read_to_string(marker)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| {
                v.get("namespace")
                    .and_then(Value::as_str)
                    .and_then(usable_namespace)
            })
        {
            return Ok(resolved(ns, NamespaceSource::Marker));
        }
    }
    if let Some(ns) = env_namespace.and_then(usable_env_namespace) {
        return Ok(resolved(ns, NamespaceSource::Env));
    }
    for root in roots.ledger_order() {
        let ledger = root.join(".NeuroStrata").join("session_queries.json");
        if let Some(ns) = std::fs::read_to_string(ledger)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .as_ref()
            .and_then(|v| v.get("namespace"))
            .and_then(Value::as_str)
            .and_then(usable_namespace)
        {
            return Ok(resolved(ns, NamespaceSource::Ledger));
        }
    }
    let folder = roots
        .main_root
        .as_deref()
        .unwrap_or(roots.dir.as_path())
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match usable_namespace(&folder) {
        Some(ns) => Ok(resolved(ns, NamespaceSource::FolderName)),
        None => Err(ResolveError::NoUsableNamespace { folder }),
    }
}

/// The namespace a project's git hooks, `hooks install` and
/// `neurostrata-mcp namespace resolve` use for `dir`: the project marker
/// (`.NeuroStrata/config.json`), then the harness config, then
/// `NEUROSTRATA_NAMESPACE` from this process's environment, then the ledger,
/// then the folder name.
pub fn resolve_namespace(dir: &Path) -> Result<ResolvedNamespace, ResolveError> {
    let env = std::env::var("NEUROSTRATA_NAMESPACE").ok();
    resolve_namespace_from(dir, env.as_deref())
}

/// The namespace a hook refresh may write into harness configs that name
/// none: only one someone named (a harness config, or, when `read_env`, the
/// environment). A ledger or folder-name answer gives `Ok(None)`: the refresh
/// then installs plugin files and leaves the namespace out. The daemon passes
/// `read_env: false`: one daemon serves every project, so its own
/// `NEUROSTRATA_NAMESPACE` says nothing about the project being refreshed.
pub(crate) fn namespace_for_refresh(
    dir: &Path,
    read_env: bool,
) -> Result<Option<String>, ResolveError> {
    let env = if read_env {
        std::env::var("NEUROSTRATA_NAMESPACE").ok()
    } else {
        None
    };
    match resolve_namespace_from(dir, env.as_deref()) {
        Ok(resolved) if resolved.source.is_explicit() => Ok(Some(resolved.namespace)),
        Ok(_) | Err(ResolveError::NoUsableNamespace { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The namespace `dir` names on purpose: the root marker's, else the single
/// namespace that the harness configs, `NEUROSTRATA_NAMESPACE` and the ledger
/// all agree on (a case-only difference counts as agreement; the first
/// source's spelling is kept). The folder name never counts. `Err` lists the
/// distinct candidates found (just the folder name when nothing names one);
/// a marker that cannot be read is named in a warning on stderr and always
/// gives `Err`, since the namespace it names is unknown.
pub fn explicit_namespace(dir: &Path) -> Result<String, Vec<String>> {
    let env = std::env::var("NEUROSTRATA_NAMESPACE").ok();
    explicit_namespace_from(dir, env.as_deref())
}

fn explicit_namespace_from(dir: &Path, env_namespace: Option<&str>) -> Result<String, Vec<String>> {
    let Ok(roots) = NamespaceRoots::of(dir) else {
        return Err(Vec::new());
    };
    // A marker that cannot be read names a namespace nobody can know: the
    // other sources are still listed, but none of them is the answer.
    let marker_unreadable = match crate::root_marker::find_root_marker(&roots.dir) {
        Ok(Some(found)) => return Ok(found.config.namespace),
        Ok(None) => false,
        Err((path, reason)) => {
            eprintln!("{}", unreadable_marker_warning(&path, &reason));
            true
        }
    };
    let mut candidates: Vec<String> = Vec::new();
    // A case-only difference is the same namespace (the store matches
    // namespaces case-insensitively); the first source's spelling is kept.
    let mut add = |ns: String| {
        if !candidates.iter().any(|c| c.eq_ignore_ascii_case(&ns)) {
            candidates.push(ns);
        }
    };
    // A config that cannot be read names nothing here: the answer then rests
    // on the sources that can be read, and an unreadable one never decides.
    for (_, ns) in roots.harness_namespaces().unwrap_or_default() {
        add(ns);
    }
    if let Some(ns) = env_namespace.and_then(usable_env_namespace) {
        add(ns);
    }
    if let Some(ns) = roots.ledger_order().find_map(|root| {
        let text =
            std::fs::read_to_string(root.join(".NeuroStrata").join("session_queries.json")).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        usable_namespace(v.get("namespace")?.as_str()?)
    }) {
        add(ns);
    }
    match candidates.len() {
        1 if !marker_unreadable => Ok(candidates.remove(0)),
        0 => Err(resolve_namespace_from(dir, None)
            .map(|r| r.namespace)
            .into_iter()
            .collect()),
        _ => Err(candidates),
    }
}

/// Every distinct namespace the pre-push gate must check for `dir`: the
/// root marker's first (marker-first resolution), then every harness config,
/// `NEUROSTRATA_NAMESPACE`, and the ledger. A migration can leave an old
/// namespace's in-progress tasks in a config or the ledger even after a
/// marker names a new one; gating only the resolved namespace lets them
/// escape (#378 F1). Case-only spellings are one namespace. An unreadable
/// marker or config fails closed: the namespace it names is unknown.
pub fn gate_namespaces(dir: &Path) -> Result<Vec<String>, ResolveError> {
    let Ok(roots) = NamespaceRoots::of(dir) else {
        return Ok(Vec::new());
    };
    let marker = crate::root_marker::find_root_marker(&roots.dir).map_err(|(path, reason)| {
        ResolveError::UnreadableMarker(UnreadableConfig { path, reason })
    })?;
    let mut out: Vec<String> = Vec::new();
    {
        let mut add = |ns: String| {
            if !out.iter().any(|n| n.eq_ignore_ascii_case(&ns)) {
                out.push(ns);
            }
        };
        if let Some(found) = marker {
            add(found.config.namespace);
        }
        let configs = roots
            .harness_namespaces()
            .map_err(ResolveError::UnreadableConfigs)?;
        for (_, ns) in configs {
            add(ns);
        }
        if let Some(ns) = std::env::var("NEUROSTRATA_NAMESPACE")
            .ok()
            .and_then(|v| usable_env_namespace(&v))
        {
            add(ns);
        }
        for root in roots.ledger_order() {
            let ledger = root.join(".NeuroStrata").join("session_queries.json");
            let text = match std::fs::read_to_string(&ledger) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(ResolveError::UnreadableConfigs(vec![UnreadableConfig {
                        path: ledger,
                        reason: e.to_string(),
                    }]));
                }
            };
            let value: Value = serde_json::from_str(&text).map_err(|e| {
                ResolveError::UnreadableConfigs(vec![UnreadableConfig {
                    path: ledger.clone(),
                    reason: format!("it is not valid JSON ({e})"),
                }])
            })?;
            if let Some(ns) = value
                .get("namespace")
                .and_then(Value::as_str)
                .and_then(usable_namespace)
            {
                add(ns);
            }
        }
    }
    Ok(out)
}

/// The one warning line, naming the file, for a project marker at `path`
/// that cannot be read and is skipped.
fn unreadable_marker_warning(path: &Path, reason: &str) -> String {
    format!(
        "warning: the project marker {} could not be read ({reason}); it is skipped",
        path.display()
    )
}

/// The namespace [`auto_install_detected_plugins`] writes into harness
/// configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigNamespace<'a> {
    /// The caller named it (`neurostrata_bootstrap`, `neurostrata_task_setup`):
    /// every wired config gets it.
    Set(&'a str),
    /// A refresh: each config keeps its own usable namespace; a config naming
    /// none gets this one when given, else a namespace-neutral registration
    /// (the runner discovers the namespace when it runs). A config that cannot
    /// be read is left as it is.
    Keep(Option<&'a str>),
    /// The daemon's boot refresh: each harness gets the namespace listed for
    /// it ([`boot_namespaces_for`]); a harness not listed, or whose config
    /// cannot be read, is left unwritten.
    Each(&'a [(String, String)]),
}

impl ConfigNamespace<'_> {
    /// The namespace to write into `harness`'s config under `root`, or `None`
    /// to leave that config unwritten. Never an unusable value.
    fn for_harness(self, root: &Path, harness: &str) -> Option<String> {
        match self {
            ConfigNamespace::Set(ns) => usable_namespace(ns),
            ConfigNamespace::Keep(fallback) => match harness_config_namespace(root, harness) {
                Ok(Some(own)) => Some(own),
                Ok(None) => fallback.and_then(usable_namespace),
                Err(_) => None,
            },
            ConfigNamespace::Each(listed) => match harness_config_namespace(root, harness) {
                Err(_) => None,
                Ok(_) => listed
                    .iter()
                    .find(|(h, _)| h == harness)
                    .map(|(_, ns)| ns.clone()),
            },
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Detect the active runtime harness (from env vars or explicit hint) and all
/// harnesses present in the project or user environment.
pub fn detect_harnesses(
    project_root: &Path,
    explicit_harness: Option<&str>,
) -> (Option<&'static str>, Vec<&'static str>) {
    let explicit = explicit_harness.and_then(normalize_harness_name);

    let env_active = if has_any_env(&[
        "OPENCODE",
        "OPENCODE_PID",
        "OPENCODE_SESSION",
        "OPENCODE_CONFIG",
    ]) {
        Some("opencode")
    } else if has_any_env(&[
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_PROJECT_DIR",
        "ANTHROPIC_CLAUDE_CODE",
    ]) {
        Some("claude")
    } else if has_any_env(&["HERMES_AGENT", "HERMES_HOME", "HERMES_SESSION_ID"]) {
        Some("hermes")
    } else if has_any_env(&["CODEX_HOME", "CODEX_CLI", "OPENAI_CODEX"]) {
        Some("codex")
    } else if has_any_env(&["OPENCLAW_HOME", "OPENCLAW_GATEWAY_URL", "OPENCLAW_SESSION"]) {
        Some("openclaw")
    } else if has_any_env(&["GH_COPILOT", "COPILOT_CLI", "GITHUB_COPILOT_CLI"]) {
        Some("github-cli")
    } else {
        None
    };

    let active = explicit.or(env_active);
    let home = home_dir();
    let mut detected: Vec<&'static str> = Vec::new();
    let mut push_unique = |h: &'static str| {
        if !detected.contains(&h) {
            detected.push(h);
        }
    };

    if let Some(a) = active {
        push_unique(a);
    }

    if project_root.join(".opencode").exists()
        || project_root.join(".NeuroStrata/plugins/opencode").exists()
        || project_root.join("opencode.json").exists()
        || home
            .as_ref()
            .map(|h| {
                h.join(".config/opencode").exists() || h.join(".local/share/opencode").exists()
            })
            .unwrap_or(false)
    {
        push_unique("opencode");
    }
    if project_root.join(".openclaw").exists()
        || home
            .as_ref()
            .map(|h| h.join(".openclaw").exists())
            .unwrap_or(false)
    {
        push_unique("openclaw");
    }
    if project_root.join(".claude").exists()
        || home
            .as_ref()
            .map(|h| h.join(".claude").exists())
            .unwrap_or(false)
    {
        push_unique("claude");
    }
    if project_root.join(".hermes").exists()
        || home
            .as_ref()
            .map(|h| h.join(".hermes").exists())
            .unwrap_or(false)
    {
        push_unique("hermes");
    }
    if project_root.join(".codex").exists()
        || home
            .as_ref()
            .map(|h| h.join(".codex").exists())
            .unwrap_or(false)
    {
        push_unique("codex");
    }
    if project_root.join(".github/copilot").exists()
        || project_root
            .join(".NeuroStrata/plugins/github-cli/copilot-extension.json")
            .exists()
        || home
            .as_ref()
            .map(|h| h.join(".config/gh").exists() || h.join(".config/github-copilot").exists())
            .unwrap_or(false)
    {
        push_unique("github-cli");
    }

    if detected.is_empty() {
        detected.extend_from_slice(KNOWN_HARNESSES);
    }

    (active, detected)
}

/// The namespaces the daemon's boot refresh may write into harness configs
/// for `dir`, as `(harness, namespace)`: the root marker's for every harness
/// when a marker applies, else each harness keeps the one its own config
/// already names. A harness's config is looked for in `dir` itself
/// first, then in `dir`'s checkout, its main checkout (for a linked
/// worktree), or, outside git, `dir`; the first place that has one wins, and
/// a config that cannot be read ends the search for that harness. A harness
/// with no config anywhere is left out. Never the environment, the ledger or
/// the folder name: the daemon's own environment says nothing about `dir`'s
/// project, and a guessed name would overwrite the right one. A marker that
/// cannot be read is skipped with a one-line warning naming it; each harness
/// then keeps its own config's namespace, as without a marker.
pub fn boot_namespaces_for(dir: &Path) -> Vec<(String, String)> {
    let Ok(roots) = NamespaceRoots::of(dir) else {
        return Vec::new();
    };
    let mut places: Vec<&PathBuf> = vec![&roots.dir];
    for root in roots.config_order() {
        if !places.contains(&root) {
            places.push(root);
        }
    }
    let harnesses =
        std::iter::once("claude").chain(PLUGIN_NAMESPACE_CONFIGS.iter().map(|(h, _, _)| *h));
    match crate::root_marker::find_root_marker(&roots.dir) {
        Ok(Some(marker)) => {
            return harnesses
                .map(|h| (h.to_string(), marker.config.namespace.clone()))
                .collect();
        }
        Ok(None) => {}
        Err((path, reason)) => {
            // The daemon's log, where its other boot warnings go.
            tracing::warn!("{}", unreadable_marker_warning(&path, &reason));
        }
    }
    let mut found = Vec::new();
    for harness in harnesses {
        for place in &places {
            match harness_config_namespace(place, harness) {
                Ok(Some(ns)) => {
                    found.push((harness.to_string(), ns));
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    }
    found
}

/// The daemon's boot-time refresh for `dir`. Bundles are always refreshed;
/// each harness config is rewritten only with the namespace
/// [`boot_namespaces_for`] keeps for that harness, so configs that disagree
/// stay as they are and a harness without one is never given one.
pub fn boot_install_plugins(dir: &Path) -> Value {
    let namespaces = boot_namespaces_for(dir);
    auto_install_detected_plugins(dir, ConfigNamespace::Each(&namespaces), None)
}

/// The daemon's boot-time refresh for a configured project. The config
/// stores each project as its `.NeuroStrata` folder; the refresh runs on the
/// project folder that holds it. Any other path is refreshed as given.
pub fn boot_install_project_context(context: &Path) -> Value {
    let context = dunce::simplified(context);
    let project = match (context.file_name(), context.parent()) {
        (Some(name), Some(parent)) if name == ".NeuroStrata" => parent,
        _ => context,
    };
    boot_install_plugins(project)
}

/// Auto-install the compiled zero-dependency plugin bundles for all detected
/// harnesses into `<project_root>/.NeuroStrata/plugins/<harness>/` and wire
/// harness-specific config files when the harness directory or active harness
/// is present. `namespace` says what goes into those config files; a config
/// with no usable namespace to write is left unwritten.
pub fn auto_install_detected_plugins(
    project_root: &Path,
    namespace: ConfigNamespace<'_>,
    explicit_harness: Option<&str>,
) -> Value {
    let (active, detected) = detect_harnesses(project_root, explicit_harness);
    if project_root == Path::new("/tmp") {
        return json!({
            "active_harness": active,
            "detected_harnesses": detected,
            "installed_plugins": [],
        });
    }
    let base_dir = project_root.join(".NeuroStrata/plugins");
    let mut installed = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Bundle the shared plugin-core once so every harness adapter's relative
    // `../../core/dist/index.js` import resolves on disk under
    // `.NeuroStrata/plugins/core/dist/index.js`.
    if !detected.is_empty() {
        let core_dir = base_dir.join("core");
        let core_dist = core_dir.join("dist");
        if std::fs::create_dir_all(&core_dist).is_ok() {
            let core_index = core_dist.join("index.js");
            let core_pkg = core_dir.join("package.json");
            let write_if_changed = |path: &Path, content: &str| {
                let needs_write = std::fs::read_to_string(path)
                    .map(|existing| existing != content)
                    .unwrap_or(true);
                if needs_write {
                    let _ = std::fs::write(path, content);
                }
            };
            write_if_changed(&core_index, CORE_INDEX_JS);
            write_if_changed(&core_pkg, CORE_PKG_JSON);
        }
    }

    for &harness in &detected {
        let Some((index_js, pkg_json)) = bundle_for(harness) else {
            continue;
        };
        let plugin_dir = base_dir.join(harness);
        let dist_dir = plugin_dir.join("dist");
        if std::fs::create_dir_all(&dist_dir).is_err() {
            continue;
        }
        let dist_index = dist_dir.join("index.js");
        let pkg_path = plugin_dir.join("package.json");
        let runner_path = plugin_dir.join("cli-hook.mjs");

        let write_if_changed = |path: &Path, content: &str| {
            let needs_write = std::fs::read_to_string(path)
                .map(|existing| existing != content)
                .unwrap_or(true);
            if needs_write {
                let _ = std::fs::write(path, content);
            }
        };

        write_if_changed(&dist_index, index_js);
        write_if_changed(&pkg_path, pkg_json);
        write_if_changed(&runner_path, cli_hook_runner_for(harness));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&runner_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&runner_path, perms);
            }
        }

        let abs_dist = plain_abs_path(&dist_index);
        let abs_runner = plain_abs_path(&runner_path);

        let wired_config = wire_harness_config(
            project_root,
            namespace,
            harness,
            active == Some(harness),
            &detected,
            &abs_dist,
            &abs_runner,
            &mut warnings,
        );

        installed.push(json!({
            "harness": harness,
            "bundle_path": abs_dist,
            "cli_hook_runner": abs_runner,
            "config_wired": wired_config,
        }));
    }

    let (skills_installed, patched_agents) =
        sync_skills_and_agent_rules(project_root, active, &detected);

    json!({
        "active_harness": active,
        "detected_harnesses": detected,
        "installed_plugins": installed,
        "skills_installed": skills_installed,
        "patched_agents": patched_agents,
        "warnings": warnings,
    })
}

fn sync_skills_and_agent_rules(
    project_root: &Path,
    active: Option<&str>,
    detected: &[&str],
) -> (Vec<String>, Vec<String>) {
    let mut skills_installed = Vec::new();
    let mut patched_agents = Vec::new();

    let write_file_if_changed = |path: &Path, content: &str| -> bool {
        // Home-level skills are outside the checkout; anchor the symlink
        // check at their nearest parent so a repo-controlled link is still
        // refused while the user's own install can be refreshed (#381 F3).
        let base = if path.starts_with(project_root) {
            project_root
        } else {
            path.parent().unwrap_or(path)
        };
        if refuse_symlink_components(base, path).is_err() {
            return false;
        }
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let needs_write = std::fs::read_to_string(path)
            .map(|existing| existing != content)
            .unwrap_or(true);
        !(needs_write && write_atomically(path, content).is_err())
    };

    // 1. Always scaffold project-local .agents/skills/neurostrata/SKILL.md and .agents/agents/NeuroStrata-Task.md
    let proj_skill = project_root.join(".agents/skills/neurostrata/SKILL.md");
    if write_file_if_changed(&proj_skill, EMBEDDED_NEUROSTRATA_SKILL_MD) {
        skills_installed.push(proj_skill.to_string_lossy().to_string());
    }
    let proj_task_agent = project_root.join(".agents/agents/NeuroStrata-Task.md");
    let _ = write_file_if_changed(&proj_task_agent, EMBEDDED_NEUROSTRATA_TASK_AGENT_MD);

    // 2. Sync harness-specific skill & agent directories (project-level and machine-level)
    if active == Some("opencode") || opencode_in_project(project_root) {
        let oc_skill = project_root.join(".opencode/skills/neurostrata/SKILL.md");
        if write_file_if_changed(&oc_skill, EMBEDDED_NEUROSTRATA_SKILL_MD) {
            skills_installed.push(oc_skill.to_string_lossy().to_string());
        }
    }
    if active == Some("claude") || project_root.join(".claude").exists() {
        let cl_skill = project_root.join(".claude/skills/neurostrata/SKILL.md");
        if write_file_if_changed(&cl_skill, EMBEDDED_NEUROSTRATA_SKILL_MD) {
            skills_installed.push(cl_skill.to_string_lossy().to_string());
        }
        let cl_agent = project_root.join(".claude/agents/NeuroStrata-Task.md");
        let _ = write_file_if_changed(&cl_agent, EMBEDDED_NEUROSTRATA_TASK_AGENT_MD);
    }

    if let Some(home) = home_dir() {
        if detected.contains(&"opencode") && home.join(".config/opencode").exists() {
            let global_oc_skill = home.join(".config/opencode/skills/neurostrata/SKILL.md");
            if write_file_if_changed(&global_oc_skill, EMBEDDED_NEUROSTRATA_SKILL_MD) {
                skills_installed.push(global_oc_skill.to_string_lossy().to_string());
            }
        }
        if detected.contains(&"claude") && home.join(".claude").exists() {
            let global_cl_skill = home.join(".claude/skills/neurostrata/SKILL.md");
            if write_file_if_changed(&global_cl_skill, EMBEDDED_NEUROSTRATA_SKILL_MD) {
                skills_installed.push(global_cl_skill.to_string_lossy().to_string());
            }
        }
    }

    // 3. Idempotently propagate subagent_prompt_injection + Graph-First rule to existing agent .md definitions
    let mut agent_dirs = vec![
        project_root.join(".agents/agents"),
        project_root.join(".opencode/agents"),
        project_root.join(".claude/agents"),
    ];
    if let Some(home) = home_dir() {
        if detected.contains(&"opencode") {
            agent_dirs.push(home.join(".config/opencode/agents"));
        }
        if detected.contains(&"claude") {
            agent_dirs.push(home.join(".claude/agents"));
        }
    }

    for dir in agent_dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            // Upgrade path: if an older agent .md still carries the unenforceable
            // "unless the parent prompt states ..." bypass clause, replace that
            // rule line with the verifiable Graph-First predicate.
            let updated: Option<String> = if content.contains("unless the parent prompt states") {
                let replaced = content
                    .lines()
                    .map(|line| {
                        if line.contains("unless the parent prompt states") {
                            SUBAGENT_GRAPH_FIRST_RULE_SNIPPET.trim_end()
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<&str>>()
                    .join("\n");
                if replaced != content {
                    Some(replaced)
                } else {
                    None
                }
            } else if !content.contains(SUBAGENT_RULE_MARKER) && !content.contains("tools: false") {
                Some(format!(
                    "{}{}",
                    content.trim_end(),
                    SUBAGENT_GRAPH_FIRST_RULE_SNIPPET
                ))
            } else {
                None
            };
            if let Some(updated) = updated {
                if refuse_symlink_components(&dir, &path).is_ok()
                    && write_atomically(&path, &updated).is_ok()
                {
                    patched_agents.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    (skills_installed, patched_agents)
}

/// Refuse a write whose path has a symlink component beneath `root`. A repo
/// can ship a tracked `.codex -> ../.codex` or a symlinked
/// `.NeuroStrata/plugins/...`; `create_dir_all` and `rename` would otherwise
/// follow it out of the checkout. Stops at the first missing component: the
/// installer creates the rest itself. The final component (the file) is
/// checked too, so a link to a user file is never read back or replaced.
fn refuse_symlink_components(root: &Path, path: &Path) -> std::io::Result<()> {
    let denied = |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
    let rel = path
        .strip_prefix(root)
        .map_err(|_| denied("path is outside the project root".to_string()))?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(denied(format!("{} is a symlink", current.display())));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Write `contents` to `path` through a temporary file in the same folder and
/// a rename, so a reader (a git hook running `namespace resolve`) sees the old
/// file or the new one, never a half-written one.
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    // One lock for every config in this process: a refresh touches a handful,
    // and serializing them is simpler and safer than a per-path lock table.
    let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // PID alone lets two in-process refreshes derive the same temp path.
    // A process-wide sequence plus create_new makes a planted symlink and a
    // racing refresh both fail loudly instead of following/clobbering it.
    let tmp = path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        ATOMIC_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    write_staged_atomically(&tmp, path, contents)
}

/// The security-relevant core of [`write_atomically`], with the staged path
/// supplied by the caller so a symlink planted at that exact path can be
/// exercised deterministically. Production callers get a unique staged name
/// from [`write_atomically`]; both routes share this single implementation.
/// True when `staged` still names the very file the caller opened, checked
/// without following a symlink planted at that path in the meantime.
fn staged_path_is_open_file(staged: &Path, opened: &std::fs::Metadata) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current = std::fs::symlink_metadata(staged)?;
        Ok(!current.file_type().is_symlink()
            && current.dev() == opened.dev()
            && current.ino() == opened.ino())
    }
    #[cfg(not(unix))]
    {
        let current = std::fs::symlink_metadata(staged)?;
        Ok(current.is_file() && current.len() == opened.len())
    }
}

fn write_staged_atomically(staged: &Path, path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    // Open first: an entry already at `staged` (someone else's) makes this
    // fail, and it is not ours to clean up.
    let mut file = options.open(staged)?;
    let written = (|| -> std::io::Result<()> {
        std::io::Write::write_all(&mut file, contents.as_bytes())?;
        file.sync_all()?;
        // Taken after the write: off Unix the check compares sizes, and the
        // staged file is empty right after the open.
        let opened = file.metadata()?;
        drop(file);
        // The path can be swapped between the exclusive open and these
        // path-based operations. Never chmod or publish a replacement: it is
        // not the file this write created (#388).
        if !staged_path_is_open_file(staged, &opened)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "staged config was replaced before publish",
            ));
        }
        // A refresh must never widen an existing config's access: it carries
        // tokens and namespace metadata other local users must not read.
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            std::fs::set_permissions(staged, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(staged, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    written
}

#[allow(clippy::too_many_arguments)]
fn wire_harness_config(
    project_root: &Path,
    namespace: ConfigNamespace<'_>,
    harness: &str,
    is_active: bool,
    detected: &[&str],
    abs_dist: &str,
    abs_runner: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let write_json_if_changed = |path: &Path, val: &Value| -> Option<String> {
        if refuse_symlink_components(project_root, path).is_err() {
            return None;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let serialized = serde_json::to_string_pretty(val).ok()?;
        let needs_write = std::fs::read_to_string(path)
            .map(|existing| existing != serialized)
            .unwrap_or(true);
        if needs_write {
            write_atomically(path, &serialized).ok()?;
        }
        Some(path.to_string_lossy().to_string())
    };
    // Read before anything below writes: the namespace this config keeps, or
    // the one it gets, or none. A refresh (Keep) with no usable answer writes a
    // namespace-neutral registration that discovers one at invocation time.
    // An explicit but unusable Set value, or an unreadable config, stays
    // unwritten (byte-identical) as before.
    // A daemon boot refresh (Each) with no namespace listed for a detected
    // harness is the same case as a Keep refresh with none: register the
    // runner namespace-neutrally so its hooks are installed (#388). An
    // unreadable config still leaves the harness byte-identical/unwritten.
    let wire_without_namespace = matches!(
        namespace,
        ConfigNamespace::Keep(_) | ConfigNamespace::Each(_)
    ) && namespace.for_harness(project_root, harness).is_none()
        && !harness_config_namespace(project_root, harness).is_err();
    let namespace = namespace.for_harness(project_root, harness);
    let with_namespace = |mut doc: Value, ns: Option<&str>| -> Value {
        if let Some(ns) = ns {
            doc["namespace"] = json!(ns);
        }
        doc
    };

    match harness {
        "opencode" => {
            if is_active || opencode_in_project(project_root) {
                // 1. Write the thin OpenCode loader at .opencode/plugins/neurostrata.js
                //    (creating .opencode/plugins/ if needed). The relative import
                //    resolves from the loader file's location back to the bundled core.
                let plugins_dir = project_root.join(".opencode/plugins");
                let loader_path = plugins_dir.join("neurostrata.js");
                let loader = "export { default } from \"../../.NeuroStrata/plugins/opencode/dist/index.js\";\n\
                              export * from \"../../.NeuroStrata/plugins/opencode/dist/index.js\";\n";
                let _ = std::fs::create_dir_all(&plugins_dir);
                let _ = std::fs::write(&loader_path, loader);

                // 2. Never insert the plugin path into opencode.json. If
                //    `.opencode/opencode.json` exists, strip any legacy
                //    file-path entries from "plugins"; if "plugins" becomes
                //    empty, remove the key entirely. Clean configs are left
                //    untouched (never created, never populated with the plugin path).
                let oc_cfg = project_root.join(".opencode/opencode.json");
                if oc_cfg.exists() {
                    if let Ok(raw) = std::fs::read_to_string(&oc_cfg) {
                        if let Ok(mut doc) = serde_json::from_str::<Value>(&raw) {
                            if let Some(obj) = doc.as_object_mut() {
                                let mut changed = false;
                                let mut remove_plugins = false;
                                if let Some(plugins) =
                                    obj.get_mut("plugins").and_then(|v| v.as_array_mut())
                                {
                                    let before = plugins.len();
                                    plugins.retain(|v| {
                                        v.as_str()
                                            .map(|s| {
                                                s != "./.NeuroStrata/plugins/opencode/dist/index.js"
                                                    && s != ".NeuroStrata/plugins/opencode/dist/index.js"
                                            })
                                            .unwrap_or(true)
                                    });
                                    if plugins.len() != before {
                                        changed = true;
                                    }
                                    if plugins.is_empty() {
                                        remove_plugins = true;
                                    }
                                }
                                if remove_plugins {
                                    obj.remove("plugins");
                                    changed = true;
                                }
                                if changed {
                                    let _ = write_json_if_changed(&oc_cfg, &doc);
                                }
                            }
                        }
                    }
                }

                // Root opencode.json is intentionally never written here: the
                // plugin path is never inserted into opencode.json.

                // 3. Keep writing .opencode/neurostrata-plugin.json (the historical
                //    plugin registration) unchanged.
                //    Only with a namespace to write: never an empty or made-up one.
                let cfg_path = project_root.join(".opencode/neurostrata-plugin.json");
                if let Some(namespace) = namespace.as_deref() {
                    let doc = json!({
                        "plugin": [abs_dist],
                        "neurostrata": {
                            "namespace": namespace,
                            "pluginPath": abs_dist
                        }
                    });
                    let _ = write_json_if_changed(&cfg_path, &doc);
                } else if wire_without_namespace {
                    // A refresh with no namespace still registers the runner;
                    // it discovers the namespace when the hook runs.
                    let doc = json!({
                        "plugin": [abs_dist],
                        "neurostrata": { "pluginPath": abs_dist }
                    });
                    let _ = write_json_if_changed(&cfg_path, &doc);
                }

                return Some(loader_path.to_string_lossy().to_string());
            }
            None
        }
        "claude" => {
            if is_active || project_root.join(".claude").exists() {
                if namespace.is_none() && !wire_without_namespace {
                    return None;
                }
                // A refresh with no explicit namespace still registers the
                // hooks: the plain runner discovers the namespace at
                // invocation time. "" writes no namespace into Claude's config.
                let namespace = namespace.as_deref().unwrap_or("");
                let cfg_path = project_root.join(".claude/settings.local.json");
                // The namespace lives in Claude's plugin config, as it does
                // for the other harnesses; the command carries none.
                let Some(cmd) = claude_hook_command(abs_runner) else {
                    warnings.push(format!(
                        "claude: hooks not written: the runner path {abs_runner:?} holds a character the hook's shell would expand"
                    ));
                    return None;
                };
                let mut doc: Value = json!({});
                let mut write_doc = false;
                if cfg_path.exists() {
                    if let Some(d) = std::fs::read_to_string(&cfg_path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    {
                        if d.is_object() {
                            doc = d;
                            write_doc = true;
                        }
                    }
                } else {
                    write_doc = true;
                }
                if write_doc {
                    let mut may_add = false;
                    if let Some(obj) = doc.as_object_mut() {
                        let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
                        if let Some(h_obj) = hooks.as_object_mut() {
                            may_add = repair_claude_hooks(
                                h_obj,
                                namespace,
                                abs_runner,
                                project_root,
                                warnings,
                            );
                            let runs = |command: &str| {
                                installer_hook_parts(command, abs_runner, project_root).is_some()
                            };
                            if may_add {
                                // Each event gets the runner unless an entry
                                // already runs it, beside the user's own hooks.
                                ensure_claude_hook(
                                    h_obj,
                                    "PreToolUse",
                                    Some(CLAUDE_FILE_TOOLS_MATCHER),
                                    |m| m != Some(SESSION_SCOPE_MATCHER),
                                    &cmd,
                                    &runs,
                                );
                                // task_claim and get_snapshot get the session's
                                // folder as `worktree` (claim scope from a linked
                                // worktree).
                                ensure_claude_hook(
                                    h_obj,
                                    "PreToolUse",
                                    Some(SESSION_SCOPE_MATCHER),
                                    |m| m == Some(SESSION_SCOPE_MATCHER),
                                    &cmd,
                                    &runs,
                                );
                                // SessionStart lets the runner record the session's
                                // folder when Claude Code gives no CLAUDE_PROJECT_DIR.
                                ensure_claude_hook(
                                    h_obj,
                                    "PostToolUse",
                                    Some(CLAUDE_PRE_TOOL_USE_MATCHER),
                                    |_| true,
                                    &cmd,
                                    &runs,
                                );
                                ensure_claude_hook(
                                    h_obj,
                                    "SessionStart",
                                    None,
                                    |_| true,
                                    &cmd,
                                    &runs,
                                );
                                ensure_claude_hook(h_obj, "Stop", None, |_| true, &cmd, &runs);
                                // SessionEnd and PreCompact let the runner write the
                                // handoff note before the session ends or its
                                // context is compacted.
                                for event in ["SessionEnd", "PreCompact"] {
                                    ensure_claude_hook(h_obj, event, None, |_| true, &cmd, &runs);
                                }
                            }
                            // An older installer wrote a matcher without
                            // NotebookEdit, so notebook edits skipped the
                            // guard. Widen only our own entry, and only when
                            // it still has that exact matcher.
                            if let Some(entries) =
                                h_obj.get_mut("PreToolUse").and_then(|v| v.as_array_mut())
                            {
                                for entry in entries {
                                    let ours = entry["hooks"]
                                        .as_array()
                                        .map(|hooks| {
                                            hooks.iter().any(|h| {
                                                h["command"].as_str() == Some(cmd.as_str())
                                            })
                                        })
                                        .unwrap_or(false);
                                    if ours
                                        && entry["matcher"].as_str()
                                            == Some(LEGACY_CLAUDE_PRE_TOOL_USE_MATCHER)
                                    {
                                        entry["matcher"] = json!(CLAUDE_PRE_TOOL_USE_MATCHER);
                                    }
                                }
                            }
                        }
                    }
                    // A caller that already resolved the namespace (get_snapshot,
                    // hook_hygiene_with_namespace) leaves an explicit answer in the
                    // settings' env block; hook commands stay plain. Reading back the
                    // plugin config keeps this consistent with an older install's
                    // baked namespace and adds nothing when that write failed.
                    if may_add {
                        if let (Some(persisted), Some(obj)) =
                            (claude_config_namespace(project_root), doc.as_object_mut())
                        {
                            let env = obj.entry("env".to_string()).or_insert_with(|| json!({}));
                            if let Some(env_obj) = env.as_object_mut() {
                                env_obj.insert(
                                    "NEUROSTRATA_NAMESPACE".to_string(),
                                    Value::String(persisted),
                                );
                            }
                        }
                    }
                    return write_json_if_changed(&cfg_path, &doc);
                }
            }
            None
        }
        "hermes" => {
            if is_active || project_root.join(".hermes").exists() {
                if namespace.is_none() && !wire_without_namespace {
                    return None;
                }
                let cfg_path = project_root.join(".hermes/plugins/neurostrata.json");
                let doc = with_namespace(
                    json!({
                        "id": "neurostrata-hermes",
                        "entry": abs_dist,
                        "cliHookRunner": abs_runner
                    }),
                    namespace.as_deref(),
                );
                return write_json_if_changed(&cfg_path, &doc);
            }
            None
        }
        "codex" => {
            // A Codex install in the user's home is detected even on a fresh
            // clone with no `.codex` folder; wire the hooks then too and let
            // the writer create the folder (#368 F7).
            if is_active || detected.contains(&"codex") || project_root.join(".codex").exists() {
                if namespace.is_none() && !wire_without_namespace {
                    return None;
                }
                let cfg_path = project_root.join(".codex/plugins/neurostrata.json");
                // A namespace-neutral refresh writes no baked namespace; an
                // explicit one is still persisted for older readers (#388).
                let doc = with_namespace(
                    json!({
                        "id": "neurostrata-codex",
                        "entry": abs_dist,
                        "cliHookRunner": abs_runner
                    }),
                    namespace.as_deref(),
                );
                warn_unwritten_config(write_json_if_changed(&cfg_path, &doc), &cfg_path, warnings);
                // Codex CLI never reads the metadata file: it runs hooks from
                // `.codex/hooks.json` (or `[hooks]` in config.toml). Report
                // only a hook that was actually wired, never the metadata as
                // a fallback; say so loudly when wiring failed (#368 F6).
                let hooks_wired = wire_codex_hooks(project_root, abs_runner, warnings);
                if hooks_wired.is_none() {
                    warnings.push(
                        "no Codex hook was wired; check .codex/hooks.json and .codex/config.toml"
                            .to_string(),
                    );
                }
                return hooks_wired;
            }
            None
        }
        "openclaw" => {
            if is_active || project_root.join(".openclaw").exists() {
                if namespace.is_none() && !wire_without_namespace {
                    return None;
                }
                let cfg_path = project_root.join(".openclaw/plugins/neurostrata.json");
                let doc = with_namespace(
                    json!({
                        "id": "neurostrata-openclaw",
                        "entry": abs_dist
                    }),
                    namespace.as_deref(),
                );
                return write_json_if_changed(&cfg_path, &doc);
            }
            None
        }
        "github-cli" => {
            let cfg_path =
                project_root.join(".NeuroStrata/plugins/github-cli/copilot-extension.json");
            // An installed config is evidence this harness is present: a refresh
            // must rewrite it even without an active hint or a .github folder.
            if is_active || project_root.join(".github").exists() || cfg_path.exists() {
                if namespace.is_none() && !wire_without_namespace {
                    return None;
                }
                let doc = with_namespace(
                    json!({
                        "id": "neurostrata-github-cli",
                        "entry": abs_dist,
                        "cliHookRunner": abs_runner
                    }),
                    namespace.as_deref(),
                );
                warn_unwritten_config(write_json_if_changed(&cfg_path, &doc), &cfg_path, warnings);
                // Copilot CLI never reads the extension file: it runs hooks
                // from `.github/hooks/*.json`. Report only a hook that was
                // actually wired, as for Codex.
                let hooks_wired =
                    wire_copilot_hooks(project_root, abs_runner, GITHUB_CLI_INDEX_JS, warnings);
                if hooks_wired.is_none() {
                    warnings.push(format!(
                        "no Copilot hook was wired; check {}",
                        project_root.join(COPILOT_HOOKS_REL).display()
                    ));
                }
                return hooks_wired;
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace the harness installer recorded for the checkout at
    /// `root`: the first of [`harness_namespaces_at`].
    fn configured_namespace(root: &Path) -> Option<String> {
        harness_namespaces_at(root)
            .0
            .into_iter()
            .next()
            .map(|(_, ns)| ns)
    }

    /// The namespace `namespace resolve` prints for `dir`.
    fn ns_of(dir: &Path) -> String {
        resolve_namespace(dir).expect("resolves").namespace
    }

    /// The namespace and its source for `dir`.
    fn src_of(dir: &Path) -> (String, NamespaceSource) {
        let resolved = resolve_namespace(dir).expect("resolves");
        (resolved.namespace, resolved.source)
    }

    /// Holds the crate's env lock with `NEUROSTRATA_NAMESPACE` unset, so a
    /// test asserting a config, ledger or folder-name answer cannot see
    /// another test's value.
    fn no_namespace_env() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::daemon::project_root_env_test_lock();
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        guard
    }

    fn write_file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A harness config the installer wrote outranks the environment, which
    /// outranks the ledger and the folder name.
    #[test]
    fn harness_config_beats_env() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let dir = dir.as_path();
        write_file(
            &dir.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "alpha"}}"#,
        );
        std::env::set_var("NEUROSTRATA_NAMESPACE", "beta");
        assert_eq!(
            src_of(dir),
            ("alpha".into(), NamespaceSource::HarnessConfig)
        );
        std::fs::remove_file(dir.join(".claude").join("settings.local.json")).unwrap();
        write_file(
            &dir.join(".NeuroStrata").join("session_queries.json"),
            r#"{"namespace": "from-ledger", "queries": []}"#,
        );
        assert_eq!(src_of(dir), ("beta".into(), NamespaceSource::Env));
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        assert_eq!(src_of(dir), ("from-ledger".into(), NamespaceSource::Ledger));
        // The daemon's hook refresh never reads its own environment, and a
        // ledger answer is never written into harness configs.
        std::env::set_var("NEUROSTRATA_NAMESPACE", "beta");
        assert_eq!(namespace_for_refresh(dir, false), Ok(None));
        assert_eq!(
            namespace_for_refresh(dir, true),
            Ok(Some("beta".to_string()))
        );
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        std::fs::remove_file(dir.join(".NeuroStrata").join("session_queries.json")).unwrap();
        assert_eq!(
            src_of(dir),
            ("example-project".into(), NamespaceSource::FolderName)
        );
    }

    fn marker_at(dir: &Path, ns: &str) {
        let cfg = crate::root_marker::RootConfig::new(ns);
        std::fs::create_dir_all(dir).unwrap();
        crate::root_marker::replace_marker(dir, &cfg).unwrap();
    }

    fn git_project() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(dir.join(".git").join("config"), "[core]\n\tbare = false\n").unwrap();
        (tmp, dir)
    }

    #[test]
    fn marker_beats_harness_config_and_env() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(
            &dir.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "alpha"}}"#,
        );
        marker_at(&dir, "from-marker");
        std::env::set_var("NEUROSTRATA_NAMESPACE", "beta");
        assert_eq!(
            src_of(&dir),
            ("from-marker".into(), NamespaceSource::Marker)
        );
        // The daemon side honors the marker too.
        assert_eq!(
            namespace_for_refresh(&dir, false),
            Ok(Some("from-marker".to_string()))
        );
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
    }

    #[test]
    fn split_marker_namespace_from_subfolder() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        marker_at(&dir, "whole");
        let sub = dir.join("packages").join("api");
        marker_at(&sub, "api-part");
        std::fs::create_dir_all(sub.join("src")).unwrap();
        assert_eq!(ns_of(&sub.join("src")), "api-part");
        assert_eq!(ns_of(&dir), "whole");
        assert_eq!(ns_of(&dir.join("packages")), "whole");
    }

    #[test]
    fn worktree_uses_main_checkout_marker() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        marker_at(&dir, "from-main");
        let wt = dir.join(".claude").join("worktrees").join("agent-1");
        let gitdir = dir.join(".git").join("worktrees").join("agent-1");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", gitdir.to_string_lossy().replace('\\', "/")),
        )
        .unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        assert_eq!(src_of(&wt), ("from-main".into(), NamespaceSource::Marker));
        assert!(boot_namespaces_for(&wt)
            .iter()
            .all(|(_, ns)| ns == "from-main"));
        assert!(!boot_namespaces_for(&wt).is_empty());
    }

    #[test]
    fn explicit_namespace_rejects_folder_name_only() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        assert_eq!(
            explicit_namespace(&dir),
            Err(vec!["example-project".to_string()])
        );
        // A single source is explicit.
        std::env::set_var("NEUROSTRATA_NAMESPACE", "beta");
        assert_eq!(explicit_namespace(&dir), Ok("beta".to_string()));
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        marker_at(&dir, "from-marker");
        assert_eq!(explicit_namespace(&dir), Ok("from-marker".to_string()));
    }

    #[test]
    fn explicit_namespace_rejects_disagreement() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(
            &dir.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "alpha"}}"#,
        );
        write_file(
            &dir.join(".NeuroStrata").join("session_queries.json"),
            r#"{"namespace": "from-ledger", "queries": []}"#,
        );
        assert_eq!(
            explicit_namespace_from(&dir, Some("beta")),
            Err(vec![
                "alpha".to_string(),
                "beta".to_string(),
                "from-ledger".to_string()
            ])
        );
        assert_eq!(
            explicit_namespace_from(&dir, Some("alpha")),
            Err(vec!["alpha".to_string(), "from-ledger".to_string()])
        );
    }

    #[test]
    fn boot_uses_marker_namespace() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(
            &dir.join(".hermes/plugins/neurostrata.json"),
            r#"{"id": "neurostrata-hermes", "namespace": "from-config"}"#,
        );
        assert_eq!(
            boot_namespaces_for(&dir),
            vec![("hermes".to_string(), "from-config".to_string())]
        );
        marker_at(&dir, "from-marker");
        let boot = boot_namespaces_for(&dir);
        assert!(boot.contains(&("hermes".to_string(), "from-marker".to_string())));
        assert!(boot.iter().all(|(_, ns)| ns == "from-marker"));
    }

    #[test]
    fn namespace_resolve_cli_fails_on_unreadable_marker() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(&dir.join(".NeuroStrata").join("config.json"), "{ not json");
        let err = resolve_namespace(&dir)
            .expect_err("unreadable marker must fail")
            .to_string();
        assert!(err.contains("config.json"), "names the file: {err}");
        // A refresh writes no namespace it cannot know.
        assert!(namespace_for_refresh(&dir, true).is_err());
        marker_at(&dir, "fixed");
        assert_eq!(ns_of(&dir), "fixed");
    }

    #[test]
    fn boot_install_keeps_existing_plugin_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let cfg = dir.join(".hermes/plugins/neurostrata.json");
        write_file(
            &cfg,
            r#"{"id": "neurostrata-hermes", "namespace": "example-project"}"#,
        );
        boot_install_plugins(&dir);
        assert_eq!(namespace_in(&cfg), "example-project");
    }

    #[test]
    fn boot_install_without_any_config_writes_a_namespace_neutral_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".hermes")).unwrap();
        assert!(boot_namespaces_for(&dir).is_empty());
        boot_install_plugins(&dir);
        // PR #388: a detected harness with no listed namespace is still
        // registered — the runner discovers the namespace when it runs.
        let cfg = dir.join(".hermes/plugins/neurostrata.json");
        assert!(cfg.exists(), "detected Hermes harness left unwired");
        assert_eq!(namespace_in(&cfg), Value::Null);
        assert!(dir
            .join(".NeuroStrata/plugins/hermes/dist/index.js")
            .exists());
    }

    fn namespace_in(cfg: &Path) -> Value {
        field_in(cfg, "namespace")
    }

    fn field_in(cfg: &Path, key: &str) -> Value {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap();
        doc[key].clone()
    }

    /// Configs that disagree each keep their own namespace at boot, and a
    /// harness with no config gets a namespace-neutral registration.
    #[test]
    fn boot_install_keeps_each_harness_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let codex = dir.join(".codex/plugins/neurostrata.json");
        let hermes = dir.join(".hermes/plugins/neurostrata.json");
        write_file(
            &codex,
            r#"{"id": "neurostrata-codex", "namespace": "alpha", "entry": "stale"}"#,
        );
        write_file(
            &hermes,
            r#"{"id": "neurostrata-hermes", "namespace": "beta", "entry": "stale"}"#,
        );
        std::fs::create_dir_all(dir.join(".openclaw")).unwrap();
        boot_install_plugins(&dir);
        assert_eq!(namespace_in(&codex), "alpha");
        assert_eq!(namespace_in(&hermes), "beta");
        // A refresh must actually rewrite plugin paths, not just preserve
        // the pre-seeded namespace (PR #375 review).
        assert_ne!(field_in(&codex, "entry"), json!("stale"));
        assert_ne!(field_in(&hermes, "entry"), json!("stale"));
        let openclaw = dir.join(".openclaw/plugins/neurostrata.json");
        assert!(openclaw.exists(), "detected OpenClaw harness left unwired");
        assert_eq!(namespace_in(&openclaw), Value::Null);
        assert!(dir
            .join(".NeuroStrata/plugins/openclaw/dist/index.js")
            .exists());
    }

    /// A folder below the checkout top with its own harness config keeps
    /// that config's namespace; the checkout top's config is not copied in.
    #[test]
    fn boot_install_in_subfolder_reads_the_folder_itself_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let top = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(top.join(".git")).unwrap();
        let top_cfg = top.join(".hermes/plugins/neurostrata.json");
        write_file(
            &top_cfg,
            r#"{"id": "neurostrata-hermes", "namespace": "outer", "entry": "stale"}"#,
        );
        let sub = top.join("packages").join("api");
        let sub_cfg = sub.join(".hermes/plugins/neurostrata.json");
        write_file(
            &sub_cfg,
            r#"{"id": "neurostrata-hermes", "namespace": "api", "entry": "stale"}"#,
        );
        boot_install_plugins(&sub);
        assert_eq!(namespace_in(&sub_cfg), "api");
        assert_eq!(namespace_in(&top_cfg), "outer");
        assert_ne!(field_in(&sub_cfg, "entry"), json!("stale"));
        // The enclosing checkout must not be refreshed by a subfolder call.
        assert_eq!(field_in(&top_cfg, "entry"), json!("stale"));
    }

    /// A configured project is stored as its `.NeuroStrata` folder; the boot
    /// refresh installs into the project folder that holds it, never into
    /// `<project>/.NeuroStrata/.NeuroStrata`.
    #[test]
    fn boot_install_for_configured_project_uses_the_project_folder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".NeuroStrata")).unwrap();
        let hermes = dir.join(".hermes/plugins/neurostrata.json");
        write_file(
            &hermes,
            r#"{"id": "neurostrata-hermes", "namespace": "example-project"}"#,
        );
        boot_install_project_context(&dir.join(".NeuroStrata"));
        assert!(dir
            .join(".NeuroStrata/plugins/hermes/dist/index.js")
            .exists());
        assert!(!dir.join(".NeuroStrata/.NeuroStrata").exists());
        assert_eq!(namespace_in(&hermes), "example-project");
    }

    #[test]
    fn colon_namespace_is_usable() {
        assert_eq!(usable_namespace("team:proj"), Some("team:proj".into()));
        assert_eq!(usable_namespace(":x"), None);
    }

    /// Configs that disagree resolve in the fixed order (Claude, opencode,
    /// codex, hermes, openclaw, github-cli), and every one is listed.
    #[test]
    fn disagreeing_harness_configs_use_fixed_order_and_are_listed() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let dir = dir.as_path();
        write_file(
            &dir.join(".codex").join("plugins").join("neurostrata.json"),
            r#"{"namespace": "b"}"#,
        );
        write_file(
            &dir.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "a"}}"#,
        );
        assert_eq!(ns_of(dir), "a");
        assert_eq!(
            resolve_namespace(dir).unwrap().configs,
            vec![
                ("claude".to_string(), "a".to_string()),
                ("codex".to_string(), "b".to_string())
            ]
        );
        // codex comes before hermes in the fixed order.
        std::fs::remove_file(dir.join(".claude").join("settings.local.json")).unwrap();
        write_file(
            &dir.join(".hermes").join("plugins").join("neurostrata.json"),
            r#"{"namespace": "c"}"#,
        );
        assert_eq!(ns_of(dir), "b");
    }

    /// The hooks' namespace comes from the harness settings the installer
    /// wrote, then the main checkout's, and the folder name only last.
    #[test]
    fn the_project_namespace_is_the_configured_one_and_the_folder_name_last() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let main = base.join("platform");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
        std::fs::write(main.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(main.join(".git").join("config"), "[core]\n\tbare = false\n").unwrap();
        std::fs::create_dir_all(main.join("src")).unwrap();
        // No configuration yet: the folder name, from any directory inside.
        assert_eq!(ns_of(&main), "platform");
        assert_eq!(ns_of(&main.join("src")), "platform");

        // Claude Code settings as the installer writes them.
        std::fs::create_dir_all(main.join(".claude")).unwrap();
        let runner = plain_abs_path(&installed_runner(&main, "claude"));
        std::fs::write(
            main.join(".claude").join("settings.local.json"),
            serde_json::to_string(&json!({"hooks": {"PreToolUse": [{"matcher": "Read",
                "hooks": [{"type": "command",
                    "command": format!("NEUROSTRATA_NAMESPACE=example-project node \"{runner}\"")}]}]}}))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            configured_namespace(&main).as_deref(),
            Some("example-project")
        );
        assert_eq!(ns_of(&main.join("src")), "example-project");

        // A linked worktree beside it, with no settings of its own, inherits it.
        // Its gitdir names the shared `.git` in `commondir`, as git writes it.
        let wt_gitdir = main.join(".git").join("worktrees").join("wt");
        std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
        let wt = base.join("platform-wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!(
                "gitdir: {}\n",
                main.join(".git").join("worktrees").join("wt").display()
            ),
        )
        .unwrap();
        assert_eq!(ns_of(&wt), "example-project");

        // An explicit env block wins over the hook commands; opencode's file
        // is read when Claude's names nothing.
        std::fs::write(
            main.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "from-env"}}"#,
        )
        .unwrap();
        assert_eq!(configured_namespace(&main).as_deref(), Some("from-env"));
        std::fs::write(main.join(".claude").join("settings.local.json"), "{}").unwrap();
        std::fs::create_dir_all(main.join(".opencode")).unwrap();
        std::fs::write(
            main.join(".opencode").join("neurostrata-plugin.json"),
            r#"{"neurostrata": {"namespace": "from-opencode"}}"#,
        )
        .unwrap();
        assert_eq!(ns_of(&wt), "from-opencode");
        // An empty namespace (the daemon's startup install writes "") is no
        // answer, in either file; the word after an empty `NEUROSTRATA_NAMESPACE=`
        // is the command, not a namespace.
        std::fs::write(
            main.join(".opencode").join("neurostrata-plugin.json"),
            r#"{"neurostrata": {"namespace": ""}}"#,
        )
        .unwrap();
        std::fs::write(
            main.join(".claude").join("settings.local.json"),
            serde_json::to_string(&json!({"hooks": {"Stop": [{"hooks": [{"type": "command",
                "command": "NEUROSTRATA_NAMESPACE= node \"C:/x/cli-hook.mjs\""}]}]}}))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(configured_namespace(&main), None);
        assert_eq!(ns_of(&wt), "platform");

        // With no harness setting, the main checkout's ledger names it before
        // the folder name does.
        std::fs::create_dir_all(main.join(".NeuroStrata")).unwrap();
        std::fs::write(
            main.join(".NeuroStrata").join("session_queries.json"),
            r#"{"namespace": "from-ledger", "queries": []}"#,
        )
        .unwrap();
        assert_eq!(ns_of(&wt), "from-ledger");
        assert_eq!(ns_of(&main), "from-ledger");

        // Another harness's plugin config counts as configuration.
        std::fs::create_dir_all(main.join(".codex").join("plugins")).unwrap();
        std::fs::write(
            main.join(".codex").join("plugins").join("neurostrata.json"),
            r#"{"namespace": "from-codex"}"#,
        )
        .unwrap();
        assert_eq!(ns_of(&wt), "from-codex");
    }

    /// Only a command in the installer's exact shape, running the
    /// installer's runner for this checkout (or a missing one the next
    /// refresh repairs), names Claude's namespace.
    #[test]
    fn a_hook_command_names_the_namespace_only_in_the_installers_shape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let runner_path = installed_runner(&root, "claude");
        write_file(&runner_path, "// runner");
        let runner = plain_abs_path(&runner_path);
        let ns = |command: &str| installer_hook_namespace(command, &root, &runner);
        let ours = |n: &str| format!("NEUROSTRATA_NAMESPACE={n} node \"{runner}\"");
        assert_eq!(
            ns(&ours("example-project")).as_deref(),
            Some("example-project")
        );
        // A moved checkout's runner that no longer exists is still ours.
        assert_eq!(
            ns("NEUROSTRATA_NAMESPACE=moved node \"C:/gone/.NeuroStrata/plugins/claude/cli-hook.mjs\"")
                .as_deref(),
            Some("moved")
        );
        // Someone else's script, or a runner that exists elsewhere, is not.
        assert_eq!(
            ns("NEUROSTRATA_NAMESPACE=other node \"C:/x/notify.js\""),
            None
        );
        let elsewhere = root.join("elsewhere").join("cli-hook.mjs");
        write_file(&elsewhere, "// someone's own install");
        assert_eq!(
            ns(&format!(
                "NEUROSTRATA_NAMESPACE=other node \"{}\"",
                elsewhere.display()
            )),
            None
        );
        // Other shapes are not the installer's.
        assert_eq!(
            ns(&format!("set NEUROSTRATA_NAMESPACE=a&& node \"{runner}\"")),
            None
        );
        assert_eq!(
            ns(&format!("env NEUROSTRATA_NAMESPACE=a node \"{runner}\"")),
            None
        );
        assert_eq!(
            ns(&format!("NEUROSTRATA_NAMESPACE=\"a\" node \"{runner}\"")),
            None
        );
        assert_eq!(
            ns(&format!(
                "NEUROSTRATA_NAMESPACE=My Project node \"{runner}\""
            )),
            None
        );
        assert_eq!(
            ns(&format!("NEUROSTRATA_NAMESPACE= node \"{runner}\"")),
            None
        );
        assert_eq!(
            ns(&format!("NEUROSTRATA_NAMESPACE=$NS node \"{runner}\"")),
            None
        );
        assert_eq!(ns(&format!("{} --flag", ours("a"))), None);
        assert_eq!(ns(&format!("MY_{}", ours("a"))), None);
    }

    /// #374 (finding r), lost in the #380 merge: a missing `cli-hook.mjs`
    /// outside the installer's plugin location is a custom hook, not
    /// installer config; it must never capture the namespace the push gate
    /// uses.
    #[test]
    fn a_missing_unrelated_runner_does_not_capture_the_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let runner = plain_abs_path(&installed_runner(&root, "claude"));
        let ns = |command: &str| installer_hook_namespace(command, &root, &runner);
        assert_eq!(
            ns("NEUROSTRATA_NAMESPACE=attacker node \"C:/somewhere/cli-hook.mjs\"").as_deref(),
            None,
            "a missing unrelated cli-hook.mjs must not be treated as ours"
        );
        assert_eq!(
            ns(&format!(
                "NEUROSTRATA_NAMESPACE=attacker node \"{}\"",
                root.join("unrelated/cli-hook.mjs").display()
            )),
            None
        );
        // The installer-shaped path is still ours (moved checkout).
        assert_eq!(
            ns("NEUROSTRATA_NAMESPACE=moved node \"D:/old/.NeuroStrata/plugins/claude/cli-hook.mjs\"")
                .as_deref(),
            Some("moved")
        );
    }

    /// #374 (finding y), lost in the #380 merge: namespace lookup rejects
    /// config files that are not regular files or that exceed the size bound
    /// before reading them.
    #[test]
    fn namespace_lookup_rejects_non_regular_and_oversized_configs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        assert!(read_config(&dir.join("missing.json")).unwrap().is_none());
        let as_dir = dir.join("as-dir.json");
        std::fs::create_dir(&as_dir).unwrap();
        assert!(
            read_config(&as_dir).is_err(),
            "a directory is not a regular file"
        );
        let big = dir.join("big.json");
        // Valid JSON, but past the bound: it must not be parsed.
        let mut padded = br#"{"namespace":"alpha"}"#.to_vec();
        padded.resize(MAX_CONFIG_BYTES as usize + 1, b' ');
        std::fs::write(&big, padded).unwrap();
        assert!(
            read_config(&big).is_err(),
            "oversized config rejected before parsing"
        );
        let ok = dir.join("ok.json");
        std::fs::write(&ok, r#"{"namespace":"alpha"}"#).unwrap();
        assert!(read_config(&ok).unwrap().is_some());
    }

    /// #374 (finding Hq), lost in the #380 merge: OpenCode resolves its
    /// namespace from the config the adapter actually loads, not the
    /// historical registration file.
    #[test]
    fn opencode_effective_config_outweighs_the_historical_registration() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".NeuroStrata")).unwrap();
        std::fs::create_dir_all(root.join(".opencode")).unwrap();
        std::fs::write(
            root.join(".NeuroStrata/neurostrata-plugin.json"),
            r#"{"namespace":"actual"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".opencode/neurostrata-plugin.json"),
            r#"{"neurostrata":{"namespace":"stale"}}"#,
        )
        .unwrap();
        assert_eq!(
            harness_config_namespace(&root, "opencode")
                .unwrap()
                .as_deref(),
            Some("actual")
        );
    }

    /// Codex review on #385: every runner loads the shared `.NeuroStrata`
    /// config, so in a project without OpenCode it is not OpenCode's and
    /// adds no OpenCode namespace ahead of Codex's own.
    #[test]
    fn the_shared_config_is_not_opencodes_without_opencode() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        write_file(
            &root.join(".NeuroStrata/neurostrata-plugin.json"),
            r#"{"namespace":"fallback"}"#,
        );
        assert_eq!(harness_config_namespace(&root, "opencode").unwrap(), None);
    }

    /// Codex review on #385: OpenCode's loader uses the first config file
    /// that parses and never reads `PluginConfig.json` after it, so a stale
    /// namespace there does not count.
    #[test]
    fn opencode_stops_at_the_first_config_file_it_loads() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".opencode")).unwrap();
        write_file(&root.join(".NeuroStrata/neurostrata-plugin.json"), "{}");
        write_file(
            &root.join(".NeuroStrata/PluginConfig.json"),
            r#"{"namespace":"stale"}"#,
        );
        assert_eq!(harness_config_namespace(&root, "opencode").unwrap(), None);
    }

    /// Codex review on #385: the plugin core's `resolveConfig` reads only the
    /// shared config's top-level `namespace`, so a nested
    /// `neurostrata.namespace` there is not OpenCode's namespace.
    #[test]
    fn opencode_ignores_a_nested_namespace_in_the_shared_config() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".opencode")).unwrap();
        write_file(
            &root.join(".NeuroStrata/neurostrata-plugin.json"),
            r#"{"neurostrata":{"namespace":"stale"}}"#,
        );
        assert_eq!(harness_config_namespace(&root, "opencode").unwrap(), None);
    }

    /// PR #388: once an effective shared config exists its top-level namespace
    /// decides, even when it names none. The historical registration must not
    /// be consulted as a fallback and disagree with the loader and the gate.
    #[test]
    fn the_first_shared_config_decides_even_without_a_namespace() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".opencode")).unwrap();
        write_file(&root.join(".NeuroStrata/neurostrata-plugin.json"), "{}");
        write_file(
            &root.join(".opencode/neurostrata-plugin.json"),
            r#"{"neurostrata":{"namespace":"stale"}}"#,
        );
        assert_eq!(harness_config_namespace(&root, "opencode").unwrap(), None);
    }

    /// PR #388 (Sourcery + Codex): the loader skips a malformed first config
    /// and tries `PluginConfig.json`; namespace resolution must report the
    /// file the plugin actually loads, not stop with a parse error.
    #[test]
    fn a_malformed_shared_config_falls_through_to_the_next_file() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".opencode")).unwrap();
        write_file(
            &root.join(".NeuroStrata/neurostrata-plugin.json"),
            "{ not json",
        );
        write_file(
            &root.join(".NeuroStrata/PluginConfig.json"),
            r#"{"namespace":"actual"}"#,
        );
        write_file(
            &root.join(".opencode/neurostrata-plugin.json"),
            r#"{"neurostrata":{"namespace":"stale"}}"#,
        );
        assert_eq!(
            harness_config_namespace(&root, "opencode")
                .unwrap()
                .as_deref(),
            Some("actual")
        );
    }

    /// PR #388: skipping malformed files is not license to swallow every
    /// unreadable shared config; a directory at the config path stays an
    /// error, as it is for every other harness.
    #[test]
    fn an_unreadable_shared_config_is_still_an_error() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join(".opencode")).unwrap();
        std::fs::create_dir_all(root.join(".NeuroStrata/neurostrata-plugin.json")).unwrap();
        assert!(harness_config_namespace(&root, "opencode").is_err());
    }

    /// Sourcery review on #385: a runner path that would end or expand inside
    /// the hook's double quotes is never written into a Claude hook command.
    #[test]
    fn a_claude_hook_is_not_written_for_an_unsafe_runner_path() {
        assert_eq!(
            claude_hook_command("/p/x/cli-hook.mjs").as_deref(),
            Some("node \"/p/x/cli-hook.mjs\"")
        );
        for bad in [
            "/p/a\"; id; \"/cli-hook.mjs",
            "/p/$(id)/cli-hook.mjs",
            "/p/`id`/cli-hook.mjs",
        ] {
            assert_eq!(claude_hook_command(bad), None, "{bad}");
        }
    }

    /// #374 (findings p/q/x/Hu), lost in the #380 merge: an atomic config
    /// refresh keeps the original restrictive permissions and leaves no
    /// temporary file behind.
    #[test]
    #[cfg(unix)]
    fn atomic_write_preserves_permissions_and_leaves_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("cfg.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomically(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "refresh must not widen config permissions");
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// #374, lost in the #380 merge: two config writes in one process get
    /// distinct temp files, so neither follows nor clobbers the other's.
    #[test]
    fn concurrent_config_writes_each_land_whole() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = dir.join(format!("cfg-{}.json", i % 2));
                std::thread::spawn(move || write_atomically(&path, &format!("{i}").repeat(4096)))
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("each write succeeds");
        }
        for name in ["cfg-0.json", "cfg-1.json"] {
            let text = std::fs::read_to_string(dir.join(name)).unwrap();
            let first = text.chars().next().unwrap();
            assert!(text.chars().all(|c| c == first), "torn write in {name}");
        }
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A custom Claude hook that sets the variable for its own script does not
    /// shadow the namespace in the installer's hook.
    #[test]
    fn a_custom_claude_hook_does_not_shadow_the_installers() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let runner = plain_abs_path(&installed_runner(&dir, "claude"));
        write_file(
            &dir.join(CLAUDE_SETTINGS),
            &serde_json::to_string(&json!({"hooks": {
                "Notification": [{"hooks": [{"type": "command",
                    "command": "NEUROSTRATA_NAMESPACE=other node \"C:/x/notify.js\""}]}],
                "PreToolUse": [{"hooks": [{"type": "command",
                    "command": format!("NEUROSTRATA_NAMESPACE=alpha node \"{runner}\"")}]}]
            }}))
            .unwrap(),
        );
        assert_eq!(
            src_of(&dir),
            ("alpha".to_string(), NamespaceSource::HarnessConfig)
        );
    }

    /// A harness config that exists but cannot be read (invalid JSON, a UTF-8
    /// byte order mark) fails the lookup and names the file, even when another
    /// config names a namespace: the unreadable one might name another.
    #[test]
    fn an_unreadable_harness_config_fails_the_lookup() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let codex = dir.join(".codex").join("plugins").join("neurostrata.json");
        write_file(&codex, r#"{"namespace": "alpha"}"#);
        assert_eq!(ns_of(&dir), "alpha");
        let claude = dir.join(CLAUDE_SETTINGS);
        for broken in [
            "{\"env\": ",
            "\u{feff}{\"env\": {\"NEUROSTRATA_NAMESPACE\": \"beta\"}}",
        ] {
            write_file(&claude, broken);
            match resolve_namespace(&dir) {
                Err(ResolveError::UnreadableConfigs(list)) => {
                    assert_eq!(list.len(), 1, "{list:?}");
                    assert_eq!(list[0].path, claude);
                    let text = ResolveError::UnreadableConfigs(list).to_string();
                    assert!(text.contains("settings.local.json"), "{text}");
                }
                other => panic!("expected an unreadable config error, got {other:?}"),
            }
            assert!(namespace_for_refresh(&dir, true).is_err());
        }
        let text = read_config(&claude).unwrap_err().to_string();
        assert!(text.contains("byte order mark"), "{text}");
    }

    /// `namespace resolve <dir>` for a directory that does not exist is an
    /// error, never a made-up namespace.
    #[test]
    fn a_missing_directory_is_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("no-such-project");
        assert!(matches!(
            resolve_namespace(&missing),
            Err(ResolveError::NotADirectory(_))
        ));
    }

    /// A folder name that is not a namespace (a space in it) is never the
    /// answer: the lookup says so, and a refresh writes no namespace.
    #[test]
    fn an_unusable_folder_name_yields_no_namespace() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("My Project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        match resolve_namespace(&dir) {
            Err(ResolveError::NoUsableNamespace { folder }) => assert_eq!(folder, "My Project"),
            other => panic!("expected no usable namespace, got {other:?}"),
        }
        assert_eq!(namespace_for_refresh(&dir, false), Ok(None));
        std::env::set_var("NEUROSTRATA_NAMESPACE", "alpha");
        assert_eq!(ns_of(&dir), "alpha");
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
    }

    /// Quotes around an environment value are dropped; a config value with
    /// quotes is no namespace.
    #[test]
    fn only_environment_values_lose_their_quotes() {
        assert_eq!(
            usable_env_namespace(" \"alpha\" ").as_deref(),
            Some("alpha")
        );
        assert_eq!(usable_env_namespace("'alpha'").as_deref(), Some("alpha"));
        assert_eq!(usable_env_namespace("\"alpha'"), None);
        assert_eq!(usable_namespace("\"alpha\""), None);
        assert_eq!(usable_namespace(" alpha"), None);
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        write_file(
            &dir.join(".codex").join("plugins").join("neurostrata.json"),
            r#"{"namespace": "\"alpha\""}"#,
        );
        std::env::set_var("NEUROSTRATA_NAMESPACE", "\"beta\"");
        assert_eq!(src_of(&dir), ("beta".to_string(), NamespaceSource::Env));
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
    }

    /// Detected harness folders for the refresh tests: Claude and codex.
    fn refresh_project() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        (tmp, dir)
    }

    /// #374 (findings z/Ho), lost in the #380 merge: a refresh with no
    /// explicit namespace installs the plugin files AND wires
    /// namespace-neutral registrations, so detected harnesses can invoke the
    /// bundle; the runner discovers the namespace at invocation time. No
    /// namespace value is baked into a config.
    #[test]
    fn a_refresh_without_an_explicit_namespace_wires_namespace_neutral_hooks() {
        let (_tmp, dir) = refresh_project();
        let _ = auto_install_detected_plugins(&dir, ConfigNamespace::Keep(None), Some("claude"));
        assert!(
            installed_runner(&dir, "claude").is_file(),
            "plugin files are installed"
        );
        let cmds = hook_commands(&dir.join(CLAUDE_SETTINGS));
        assert!(!cmds.is_empty(), "detected Claude harness is registered");
        assert!(
            cmds.iter()
                .all(|c| c.starts_with("node \"") && !c.contains("NEUROSTRATA_NAMESPACE=")),
            "namespace-neutral Claude hooks: {cmds:?}"
        );
        let codex = std::fs::read_to_string(dir.join(".codex/plugins/neurostrata.json"))
            .expect("detected Codex harness is registered");
        assert!(!codex.contains("namespace"), "no baked namespace: {codex}");
        assert_eq!(harness_namespaces_at(&dir), (Vec::new(), Vec::new()));
    }

    /// PR #388: a daemon boot refresh (Each) with no namespace listed for a
    /// detected harness must wire it namespace-neutrally, like Keep.
    #[test]
    fn a_boot_refresh_wires_a_detected_harness_without_a_listed_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let listed: Vec<(String, String)> = Vec::new();
        let report =
            auto_install_detected_plugins(&root, ConfigNamespace::Each(&listed), Some("hermes"));
        let installed = report["installed_plugins"].as_array().unwrap();
        assert!(
            installed
                .iter()
                .any(|v| v["harness"].as_str() == Some("hermes")),
            "detected Hermes harness left unwired: {installed:?}"
        );
        let cfg = root.join(".hermes/plugins/neurostrata.json");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(doc.get("namespace").is_none(), "baked namespace: {doc}");
        assert!(doc["cliHookRunner"].is_string(), "no hook runner: {doc}");
    }

    /// A refresh keeps each config's own usable namespace and gives the
    /// explicit one only to configs that name none; an unreadable config is
    /// left byte-identical.
    #[test]
    fn a_refresh_keeps_each_configs_own_namespace() {
        let (_tmp, dir) = refresh_project();
        let codex = dir.join(".codex/plugins/neurostrata.json");
        write_file(&codex, r#"{"namespace": "beta"}"#);
        let _ = auto_install_detected_plugins(
            &dir,
            ConfigNamespace::Keep(Some("alpha")),
            Some("claude"),
        );
        assert_eq!(
            harness_namespaces_at(&dir).0,
            vec![
                ("claude".to_string(), "alpha".to_string()),
                ("codex".to_string(), "beta".to_string())
            ]
        );
        let broken = "\u{feff}{\"namespace\": \"beta\"}";
        write_file(&codex, broken);
        let _ = auto_install_detected_plugins(
            &dir,
            ConfigNamespace::Keep(Some("alpha")),
            Some("claude"),
        );
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), broken);
        // No temporary file is left beside a config the refresh wrote.
        let leftovers: Vec<_> = std::fs::read_dir(dir.join(".claude"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// An unusable namespace is never written, not even when a caller names it.
    #[test]
    fn an_unusable_namespace_is_never_written() {
        for bad in ["", "My Project"] {
            let (_tmp, dir) = refresh_project();
            let _ = auto_install_detected_plugins(&dir, ConfigNamespace::Set(bad), Some("claude"));
            assert!(!dir.join(CLAUDE_SETTINGS).exists(), "{bad:?}");
            assert!(
                !dir.join(".codex/plugins/neurostrata.json").exists(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_namespace_starts_with_a_letter_or_digit() {
        assert_eq!(
            usable_namespace("example-project").as_deref(),
            Some("example-project")
        );
        assert_eq!(usable_namespace("v1.2_x").as_deref(), Some("v1.2_x"));
        for bad in ["", ".", "..", "--help", "-x", "_x", ".hidden", "a b", "$NS"] {
            assert_eq!(usable_namespace(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn outside_git_the_directory_itself_is_read() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("loose-folder");
        std::fs::create_dir_all(dir.join(".opencode")).unwrap();
        if crate::paths::git_root_of_dir(&dir.to_string_lossy()).is_some() {
            eprintln!("skipped: the temp dir is inside a git checkout");
            return;
        }
        assert_eq!(ns_of(&dir), "loose-folder");
        std::fs::write(
            dir.join(".opencode").join("neurostrata-plugin.json"),
            r#"{"neurostrata": {"namespace": "from-opencode"}}"#,
        )
        .unwrap();
        assert_eq!(ns_of(&dir), "from-opencode");
    }

    /// `namespace resolve some/dir` takes a relative directory against the
    /// current one, as a shell user expects.
    #[test]
    fn a_relative_directory_resolves_against_the_current_one() {
        let _env = no_namespace_env();
        let here = std::env::current_dir().unwrap();
        // Outside a git checkout (a source tarball) `src` is its own folder.
        if crate::paths::git_root_of_dir(&here.to_string_lossy()).is_none() {
            eprintln!("skipped: the crate is not inside a git checkout");
            return;
        }
        let expected = resolve_namespace(&here).map(|r| r.namespace);
        assert_eq!(
            resolve_namespace(Path::new(".")).map(|r| r.namespace),
            expected
        );
        assert_eq!(
            resolve_namespace(Path::new("src")).map(|r| r.namespace),
            expected
        );
    }

    #[test]
    fn auto_install_detected_plugins_writes_bundles_and_wires_explicit_harness() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let custom_agent_dir = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&custom_agent_dir).expect("create .claude/agents");
        let custom_agent_md = custom_agent_dir.join("custom-worker.md");
        std::fs::write(
            &custom_agent_md,
            "# Custom Worker\n\n- Run assigned tasks.\n",
        )
        .expect("write custom-worker.md");

        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        assert_eq!(report["active_harness"], json!("claude"));
        let installed = report["installed_plugins"]
            .as_array()
            .expect("installed array");
        assert!(!installed.is_empty());
        assert!(tmp
            .path()
            .join(".NeuroStrata/plugins/claude/dist/index.js")
            .exists());
        assert!(tmp.path().join(".claude/settings.local.json").exists());
        assert!(tmp
            .path()
            .join(".agents/skills/neurostrata/SKILL.md")
            .exists());
        assert!(tmp
            .path()
            .join(".agents/agents/NeuroStrata-Task.md")
            .exists());
        assert!(tmp
            .path()
            .join(".claude/skills/neurostrata/SKILL.md")
            .exists());
        assert!(tmp
            .path()
            .join(".claude/agents/NeuroStrata-Task.md")
            .exists());
        let patched =
            std::fs::read_to_string(&custom_agent_md).expect("read patched custom-worker.md");
        assert!(patched.contains("subagent_prompt_injection"));
    }

    fn claude_pre_tool_use_matchers(root: &Path) -> Vec<(String, String)> {
        let raw = std::fs::read_to_string(root.join(".claude/settings.local.json"))
            .expect("read settings");
        let doc: Value = serde_json::from_str(&raw).expect("parse settings");
        doc["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse array")
            .iter()
            .map(|e| {
                (
                    e["matcher"].as_str().unwrap_or_default().to_string(),
                    e["hooks"][0]["command"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect()
    }

    /// #333: NotebookEdit writes files, so the installed hook must run for
    /// it, and an install made by an older version must be widened on the
    /// next run without touching user hooks.
    #[test]
    fn claude_hook_matcher_covers_notebook_edit_and_widens_older_installs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        // The installer also wires a session-scope hook; only the file-tools
        // entry guards NotebookEdit.
        let file_hooks = |root: &Path| {
            claude_pre_tool_use_matchers(root)
                .into_iter()
                .filter(|(matcher, _)| !matcher.contains("mcp__"))
                .collect::<Vec<_>>()
        };
        let fresh = file_hooks(tmp.path());
        assert_eq!(fresh.len(), 1);
        assert!(
            fresh
                .iter()
                .any(|(m, _)| m.split('|').any(|t| t == "NotebookEdit")),
            "fresh install must guard NotebookEdit: {:?}",
            fresh
        );
        let ours = fresh[0].1.clone();

        let cfg = tmp.path().join(".claude/settings.local.json");
        let older = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": LEGACY_CLAUDE_PRE_TOOL_USE_MATCHER,
                        "hooks": [{ "type": "command", "command": ours }]
                    },
                    {
                        "matcher": LEGACY_CLAUDE_PRE_TOOL_USE_MATCHER,
                        "hooks": [{ "type": "command", "command": "echo user-hook" }]
                    }
                ],
                "Stop": [{ "hooks": [{ "type": "command", "command": ours }] }]
            }
        });
        std::fs::write(&cfg, serde_json::to_string_pretty(&older).unwrap())
            .expect("write older settings");

        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let after = claude_pre_tool_use_matchers(tmp.path());
        assert_eq!(
            after,
            vec![
                (CLAUDE_PRE_TOOL_USE_MATCHER.to_string(), ours.clone()),
                (
                    LEGACY_CLAUDE_PRE_TOOL_USE_MATCHER.to_string(),
                    "echo user-hook".to_string()
                ),
                (SESSION_SCOPE_MATCHER.to_string(), ours.clone()),
            ],
            "only the installer's own entry is widened; session-scope stays"
        );
    }

    #[test]
    fn wired_paths_never_use_the_windows_extended_length_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner");
        assert!(
            !runner.starts_with(r"\\?\"),
            "verbatim path leaked: {runner}"
        );
        let settings = std::fs::read_to_string(tmp.path().join(".claude/settings.local.json"))
            .expect("settings.local.json");
        assert!(
            !settings.contains(r"\\\\?\\"),
            "verbatim path in hook command"
        );
    }

    #[test]
    fn wired_paths_are_plain_for_every_harness() {
        let cases = [
            ("opencode", ".opencode/neurostrata-plugin.json"),
            ("claude", ".claude/settings.local.json"),
            ("hermes", ".hermes/plugins/neurostrata.json"),
            ("codex", ".codex/plugins/neurostrata.json"),
            ("openclaw", ".openclaw/plugins/neurostrata.json"),
            (
                "github-cli",
                ".NeuroStrata/plugins/github-cli/copilot-extension.json",
            ),
        ];
        for (harness, cfg_rel) in cases {
            let tmp = tempfile::tempdir().expect("tempdir");
            let report = auto_install_detected_plugins(
                tmp.path(),
                ConfigNamespace::Set("TestProj"),
                Some(harness),
            );
            assert!(
                report["ok"].as_bool().unwrap_or(true),
                "{harness} install report"
            );
            let cfg = tmp.path().join(cfg_rel);
            assert!(
                cfg.exists(),
                "{harness} config missing at {}",
                cfg.display()
            );
            let raw = std::fs::read_to_string(&cfg).expect("read harness config");
            assert!(
                !raw.contains(r"\\?\") && !raw.contains(r"\\\\?\\"),
                "{harness} config leaked a verbatim path: {raw}"
            );
            let runner = report["installed_plugins"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|p| p.get("cli_hook_runner"))
                .and_then(|v| v.as_str());
            if let Some(runner) = runner {
                assert!(
                    !runner.starts_with(r"\\?\"),
                    "{harness} runner is verbatim: {runner}"
                );
            }
        }
    }

    #[test]
    fn repairs_an_existing_verbatim_hook_command_without_touching_user_hooks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        assert_eq!(report["active_harness"], json!("claude"));
        let cfg = tmp.path().join(".claude/settings.local.json");
        let raw = std::fs::read_to_string(&cfg).expect("read settings");
        let mut doc: Value = serde_json::from_str(&raw).expect("parse settings");
        let entries = doc["hooks"]["PreToolUse"]
            .as_array_mut()
            .expect("PreToolUse array");
        let ours = entries
            .iter_mut()
            .find(|entry| {
                entry["hooks"]
                    .as_array()
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h["command"]
                                .as_str()
                                .map(|c| c.starts_with("node \""))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
            .expect("installer hook present");
        let command = ours["hooks"][0]["command"]
            .as_str()
            .expect("command")
            .to_string();
        let open = command.find('"').expect("open quote");
        let close = command[open + 1..].find('"').expect("close quote") + open + 1;
        let old_path = &command[open + 1..close];
        let basename = Path::new(old_path)
            .file_name()
            .expect("runner basename")
            .to_string_lossy()
            .to_string();
        // A moved project's runner at the installer's place, in verbatim form;
        // a missing runner anywhere else is not the installer's to repair.
        let verbatim = format!(r"\\?\C:\old\.NeuroStrata\plugins\claude\{}", basename);
        ours["hooks"][0]["command"] = json!(format!(
            "{}{}{}",
            &command[..open + 1],
            verbatim,
            &command[close..]
        ));
        // A custom hook carrying the namespace variable but not using the
        // installer's runner must survive verbatim.
        entries.push(json!({
            "matcher": "Bash",
            "hooks": [{
                "type": "command",
                "command": "NEUROSTRATA_NAMESPACE=TestProj neurostrata-mcp task gate TestProj --strict"
            }]
        }));
        entries.push(json!({
            "matcher": "Bash",
            "hooks": [{ "type": "command", "command": "echo user-hook" }]
        }));
        std::fs::write(&cfg, serde_json::to_string_pretty(&doc).unwrap())
            .expect("write broken settings");

        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let raw = std::fs::read_to_string(&cfg).expect("read settings");
        assert!(
            raw.contains("neurostrata-mcp task gate TestProj --strict"),
            "custom gate hook must be preserved: {raw}"
        );
        assert!(
            raw.contains("echo user-hook"),
            "user hook must be preserved: {raw}"
        );
        let doc: Value = serde_json::from_str(&raw).expect("parse settings");
        let commands: Vec<&str> = doc["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse array")
            .iter()
            .flat_map(|entry| entry["hooks"].as_array().into_iter().flatten())
            .filter_map(|hook| hook["command"].as_str())
            .collect();
        assert!(
            commands.iter().all(|c| !c.contains(r"\\?\")),
            "verbatim path survived repair: {commands:?}"
        );
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with("node \"") && c.ends_with('"')),
            "our repaired hook is present: {commands:?}"
        );
    }

    /// Settings whose PreToolUse, Stop and SessionStart hooks all run
    /// `NEUROSTRATA_NAMESPACE=TestProj node "<runner>"`, as older installers
    /// wrote them.
    fn settings_with_runner(runner: &str) -> Value {
        settings_with_command(&format!("NEUROSTRATA_NAMESPACE=TestProj node \"{runner}\""))
    }

    fn settings_with_command(cmd: &str) -> Value {
        json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Read|Write|Edit|MultiEdit|Bash|Task",
                    "hooks": [{ "type": "command", "command": cmd }]
                }],
                "SessionStart": [{ "hooks": [{ "type": "command", "command": cmd }] }],
                "Stop": [{ "hooks": [{ "type": "command", "command": cmd }] }]
            }
        })
    }

    fn hook_commands(cfg: &Path) -> Vec<String> {
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(cfg).expect("read settings"))
                .expect("parse settings");
        doc["hooks"]
            .as_object()
            .expect("hooks object")
            .values()
            .flat_map(|entries| entries.as_array().cloned().unwrap_or_default())
            .flat_map(|entry| entry["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|hook| hook["command"].as_str().map(str::to_string))
            .collect()
    }

    #[test]
    fn leaves_a_custom_existing_runner_outside_the_project_byte_identical() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".claude")).expect("create .claude");
        let custom_dir = tmp.path().join("plugins-local/claude");
        std::fs::create_dir_all(&custom_dir).expect("create custom plugin dir");
        let custom_runner = custom_dir.join("cli-hook.mjs");
        std::fs::write(&custom_runner, "// custom runner\n").expect("write custom runner");
        // Forward slashes, the way a hand-written Windows hook usually spells it.
        let runner = custom_runner.to_string_lossy().replace('\\', "/");
        let cfg = project.join(".claude/settings.local.json");
        let before = serde_json::to_string_pretty(&settings_with_runner(&runner)).unwrap();
        std::fs::write(&cfg, &before).expect("write settings");

        let _ = auto_install_detected_plugins(
            &project,
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let after = std::fs::read_to_string(&cfg).expect("read settings");
        assert_eq!(after, before, "custom hook commands were rewritten");
    }

    #[test]
    fn repairs_a_runner_path_that_no_longer_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        // A moved project keeps the installer layout; the old runner sat at
        // the installer's plugin location (#378 F6, #388).
        let gone = tmp
            .path()
            .join("moved-away/.NeuroStrata/plugins/claude/cli-hook.mjs");
        let cfg = tmp.path().join(".claude/settings.local.json");
        std::fs::write(
            &cfg,
            serde_json::to_string_pretty(&settings_with_runner(&gone.to_string_lossy())).unwrap(),
        )
        .expect("write settings");

        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner");
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 7, "{commands:?}");
        assert!(
            commands.iter().all(|c| *c == want),
            "missing runner not repaired: {commands:?}"
        );
        assert_eq!(
            claude_config_namespace(tmp.path()).as_deref(),
            Some("TestProj")
        );
    }

    fn claude_config(root: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(CLAUDE_CONFIG_REL)).unwrap())
            .unwrap()
    }

    #[test]
    fn fresh_claude_install_writes_config_and_plain_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("example-project"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .unwrap();
        assert_eq!(
            claude_config(tmp.path()),
            json!({"id": "neurostrata-claude", "namespace": "example-project"})
        );
        let commands = hook_commands(&tmp.path().join(".claude/settings.local.json"));
        assert_eq!(commands.len(), 7, "{commands:?}");
        let want = format!("node \"{abs_runner}\"");
        assert!(commands.iter().all(|c| *c == want), "{commands:?}");
        assert_eq!(
            configured_namespace(tmp.path()).as_deref(),
            Some("example-project")
        );
    }

    #[test]
    fn old_claude_install_is_repaired_keeping_its_namespace() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        // The marker names another namespace; the old command's is kept.
        marker_at(&dir, "example-other");
        let runner = dir.join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        let old = format!(
            "NEUROSTRATA_NAMESPACE=example-project node \"{}\"",
            runner.display()
        );
        assert_eq!(
            repair_installer_hook_command(&old, &runner.to_string_lossy(), &dir),
            Some(format!("node \"{}\"", runner.display()))
        );
        assert_eq!(
            claude_config_namespace(&dir).as_deref(),
            Some("example-project")
        );

        // Through the installer, called with the marker's namespace.
        std::fs::remove_file(dir.join(CLAUDE_CONFIG_REL)).unwrap();
        let cfg = dir.join(".claude/settings.local.json");
        std::fs::write(
            &cfg,
            serde_json::to_string_pretty(&settings_with_command(&old)).unwrap(),
        )
        .unwrap();
        let report = auto_install_detected_plugins(
            &dir,
            ConfigNamespace::Set("example-other"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .unwrap();
        assert_eq!(claude_config(&dir)["namespace"], "example-project");
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 7, "{commands:?}");
        assert!(commands.iter().all(|c| *c == want), "{commands:?}");
    }

    #[test]
    fn repair_keeps_old_command_when_config_write_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        // A folder where the config file goes: every write of it fails, on
        // every platform (a read-only folder does not stop writes on Windows).
        std::fs::create_dir_all(root.join(CLAUDE_CONFIG_REL)).unwrap();
        let runner = root.join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        let old = format!(
            "NEUROSTRATA_NAMESPACE=example-project node \"{}\"",
            runner.display()
        );
        assert_eq!(
            repair_installer_hook_command(&old, &runner.to_string_lossy(), root),
            None
        );

        let cfg = root.join(".claude/settings.local.json");
        let before = serde_json::to_string_pretty(&settings_with_command(&old)).unwrap();
        std::fs::write(&cfg, &before).unwrap();
        let report = auto_install_detected_plugins(
            root,
            ConfigNamespace::Set("example-project"),
            Some("claude"),
        );
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            before,
            "old command must stay"
        );
        assert_eq!(
            report["warnings"].as_array().map(Vec::len),
            Some(1),
            "{report}"
        );

        // A fresh install still installs the hooks (fail-soft) and warns.
        std::fs::remove_file(&cfg).unwrap();
        let report = auto_install_detected_plugins(
            root,
            ConfigNamespace::Set("example-project"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .unwrap();
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 7, "{commands:?}");
        assert!(
            commands
                .iter()
                .all(|c| *c == format!("node \"{abs_runner}\"")),
            "{commands:?}"
        );
        let warning = report["warnings"][0].as_str().unwrap_or_default();
        assert!(warning.contains("neurostrata.json"), "{report}");
    }

    #[test]
    fn old_hooks_with_different_namespaces_keep_the_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let runner = root.join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        let baked = |ns: &str| format!("NEUROSTRATA_NAMESPACE={ns} node \"{}\"", runner.display());
        let cfg = root.join(".claude/settings.local.json");
        let settings = json!({"hooks": {
            "PreToolUse": [{"matcher": "Read", "hooks": [{"type": "command", "command": baked("example-second")}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": baked("example-first")}]}],
            "Stop": [{"hooks": [{"type": "command", "command": baked("example-third")}]}]
        }});
        std::fs::write(&cfg, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
        let report = auto_install_detected_plugins(
            root,
            ConfigNamespace::Set("example-other"),
            Some("claude"),
        );
        // SessionStart is read first, whatever order the settings keep.
        assert_eq!(claude_config(root)["namespace"], "example-first");
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .unwrap();
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 7, "{commands:?}");
        assert!(
            commands
                .iter()
                .all(|c| *c == format!("node \"{abs_runner}\"")),
            "{commands:?}"
        );
        // The collapse is said, naming every namespace the hooks carried.
        let warnings = report["warnings"].as_array().expect("warnings");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = warnings[0].as_str().unwrap();
        for ns in ["example-first", "example-second", "example-third"] {
            assert!(warning.contains(ns), "{warning}");
        }
    }

    #[test]
    fn explicit_namespace_never_answers_past_an_unreadable_marker() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(
            &dir.join(".hermes/plugins/neurostrata.json"),
            r#"{"id": "neurostrata-hermes", "namespace": "alpha"}"#,
        );
        assert_eq!(explicit_namespace(&dir), Ok("alpha".to_string()));
        write_file(
            &dir.join(".NeuroStrata").join("config.json"),
            r#"{"version": 2, "namespace": "beta"}"#,
        );
        assert_eq!(explicit_namespace(&dir), Err(vec!["alpha".to_string()]));
    }

    #[test]
    fn explicit_namespace_treats_a_case_only_difference_as_one_namespace() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(
            &dir.join(".hermes/plugins/neurostrata.json"),
            r#"{"id": "neurostrata-hermes", "namespace": "Example-Project"}"#,
        );
        write_file(
            &dir.join(".NeuroStrata").join("session_queries.json"),
            r#"{"namespace": "example-project", "queries": []}"#,
        );
        // The first source's spelling, in the fixed order.
        assert_eq!(
            explicit_namespace_from(&dir, Some("EXAMPLE-PROJECT")),
            Ok("Example-Project".to_string())
        );
    }

    #[test]
    fn boot_skips_an_unreadable_marker_and_keeps_each_config() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        write_file(
            &dir.join(".hermes/plugins/neurostrata.json"),
            r#"{"id": "neurostrata-hermes", "namespace": "from-config"}"#,
        );
        write_file(&dir.join(".NeuroStrata").join("config.json"), "{ not json");
        assert_eq!(
            boot_namespaces_for(&dir),
            vec![("hermes".to_string(), "from-config".to_string())]
        );
    }

    #[test]
    fn a_plain_runner_is_repaired_only_at_the_installers_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let abs_runner = plain_abs_path(&installed_runner(&root, "claude"));
        // Someone's own runner, missing: not ours.
        let other = root.join("tools").join("cli-hook.mjs");
        assert_eq!(
            installer_hook_parts(&format!("node \"{}\"", other.display()), &abs_runner, &root),
            None
        );
        // A moved project's runner at the installer's place, missing: ours.
        let moved = root
            .join("moved")
            .join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        assert_eq!(
            installer_hook_parts(&format!("node \"{}\"", moved.display()), &abs_runner, &root),
            Some((None, format!("node \"{abs_runner}\"")))
        );
        // The installer's own runner.
        assert!(
            installer_hook_parts(&format!("node \"{abs_runner}\""), &abs_runner, &root).is_some()
        );
    }

    /// Review of the #374 restore: an older-style command naming a namespace
    /// and a missing `cli-hook.mjs` outside the plugin location is someone's
    /// own hook. Repair must not migrate its namespace into Claude's plugin
    /// config, which `namespace resolve` reads first.
    #[test]
    fn a_prefixed_custom_runner_is_not_migrated_into_the_plugin_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let abs_runner = plain_abs_path(&installed_runner(&root, "claude"));
        let other = root.join("tools").join("cli-hook.mjs");
        let custom = format!("NEUROSTRATA_NAMESPACE=x node \"{}\"", other.display());
        assert_eq!(installer_hook_parts(&custom, &abs_runner, &root), None);
        let moved = root
            .join("moved")
            .join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        let older = format!("NEUROSTRATA_NAMESPACE=x node \"{}\"", moved.display());
        assert_eq!(
            installer_hook_parts(&older, &abs_runner, &root),
            Some((Some("x".to_string()), format!("node \"{abs_runner}\"")))
        );
    }

    #[test]
    fn the_runner_is_added_beside_a_users_own_hooks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let cfg = root.join(".claude/settings.local.json");
        let settings = json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": "echo user-start"}]}],
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo user-bash"}]}]
        }});
        std::fs::write(&cfg, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
        let report = auto_install_detected_plugins(
            root,
            ConfigNamespace::Set("example-project"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .unwrap();
        let ours = format!("node \"{abs_runner}\"");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        let entries = |event: &str| doc["hooks"][event].as_array().cloned().unwrap_or_default();
        let runs_ours = |entry: &Value| entry["hooks"][0]["command"] == json!(ours);
        let start = entries("SessionStart");
        assert_eq!(start.len(), 2, "{start:?}");
        assert_eq!(start[0]["hooks"][0]["command"], "echo user-start");
        assert!(runs_ours(&start[1]), "{start:?}");
        let pre = entries("PreToolUse");
        assert_eq!(pre[0]["hooks"][0]["command"], "echo user-bash");
        let matchers: Vec<&str> = pre
            .iter()
            .filter(|e| runs_ours(e))
            .filter_map(|e| e["matcher"].as_str())
            .collect();
        assert_eq!(
            matchers,
            vec![CLAUDE_PRE_TOOL_USE_MATCHER, SESSION_SCOPE_MATCHER]
        );
        assert!(entries("Stop").iter().any(runs_ours));
        for event in ["SessionEnd", "PreCompact"] {
            let list = entries(event);
            assert_eq!(list.len(), 1, "{event}: {list:?}");
            assert!(
                runs_ours(&list[0]) && list[0].get("matcher").is_none(),
                "{event}"
            );
        }
        // A second install adds nothing.
        let before = std::fs::read_to_string(&cfg).unwrap();
        auto_install_detected_plugins(
            root,
            ConfigNamespace::Set("example-project"),
            Some("claude"),
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), before);
    }

    #[test]
    fn configured_namespace_reads_claude_config_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_file(
            &root.join(CLAUDE_CONFIG_REL),
            r#"{"id": "neurostrata-claude", "namespace": "example-project"}"#,
        );
        assert_eq!(
            configured_namespace(root).as_deref(),
            Some("example-project")
        );
        // The config file outranks what older installs left in the settings.
        write_file(
            &root.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "from-settings"}}"#,
        );
        assert_eq!(
            configured_namespace(root).as_deref(),
            Some("example-project")
        );
        std::fs::remove_file(root.join(CLAUDE_CONFIG_REL)).unwrap();
        assert_eq!(configured_namespace(root).as_deref(), Some("from-settings"));
    }

    #[cfg(windows)]
    #[test]
    fn repairs_the_installers_own_runner_in_verbatim_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        // First install puts the runner on disk and reports where it is.
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner")
            .to_string();
        assert!(
            Path::new(&abs_runner).exists(),
            "runner written: {abs_runner}"
        );
        let cfg = tmp.path().join(".claude/settings.local.json");
        let verbatim = format!(r"\\?\{abs_runner}");
        std::fs::write(
            &cfg,
            serde_json::to_string_pretty(&settings_with_runner(&verbatim)).unwrap(),
        )
        .expect("write settings");

        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 6, "{commands:?}");
        assert!(
            commands.iter().all(|c| *c == want),
            "own verbatim runner not repaired: {commands:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn repairs_the_installers_own_runner_spelled_with_other_non_ascii_case() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("Pröj");
        std::fs::create_dir_all(project.join(".claude")).expect("create .claude");
        let report = auto_install_detected_plugins(
            &project,
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner")
            .to_string();
        assert!(
            abs_runner.contains("Pröj"),
            "runner under the project: {abs_runner}"
        );
        // Same file on a case-insensitive volume, but not equal after ASCII
        // case folding.
        let other_case = format!(r"\\?\{}", abs_runner.replace("Pröj", "PRÖJ"));
        assert!(
            Path::new(&other_case).exists(),
            "volume is case-insensitive: {other_case}"
        );
        let cfg = project.join(".claude/settings.local.json");
        std::fs::write(
            &cfg,
            serde_json::to_string_pretty(&settings_with_runner(&other_case)).unwrap(),
        )
        .expect("write settings");

        let _ = auto_install_detected_plugins(
            &project,
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 6, "{commands:?}");
        assert!(
            commands.iter().all(|c| *c == want),
            "own runner in other case not repaired: {commands:?}"
        );
    }

    #[test]
    fn runner_is_replaceable_matches_the_installers_own_path_in_any_spelling() {
        let own = r"C:\proj\.NeuroStrata\plugins\claude\cli-hook.mjs";
        let root = Path::new(r"C:\proj");
        for spelling in [
            own,
            r"\\?\C:\proj\.NeuroStrata\plugins\claude\cli-hook.mjs",
            "C:/proj/.NeuroStrata/plugins/claude/cli-hook.mjs",
            "c:/proj/.NeuroStrata/plugins/claude/cli-hook.mjs",
            r"c:\PROJ\.neurostrata\plugins\claude\cli-hook.mjs",
        ] {
            assert!(runner_is_replaceable(spelling, root, own), "{spelling}");
        }
        assert_eq!(
            path_key(r"\\?\UNC\server\share\cli-hook.mjs"),
            path_key(r"\\server\share\cli-hook.mjs")
        );

        // An existing runner elsewhere is someone's own install.
        let tmp = tempfile::tempdir().expect("tempdir");
        let custom = tmp.path().join("cli-hook.mjs");
        std::fs::write(&custom, "").expect("write custom runner");
        let own_here = tmp
            .path()
            .join("project/.NeuroStrata/plugins/claude/cli-hook.mjs");
        assert!(!runner_is_replaceable(
            &custom.to_string_lossy(),
            &tmp.path().join("project"),
            &own_here.to_string_lossy(),
        ));
        // Shell-expanded paths cannot be checked, so they are left alone.
        for expanded in [
            "$CLAUDE_PROJECT_DIR/hooks/cli-hook.mjs",
            "%USERPROFILE%/hooks/cli-hook.mjs",
            "~/hooks/cli-hook.mjs",
        ] {
            assert!(
                !runner_is_replaceable(expanded, tmp.path(), &own_here.to_string_lossy()),
                "{expanded}"
            );
        }
    }

    #[test]
    fn auto_install_detected_plugins_writes_core_and_opencode_loader() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".opencode")).expect("create .opencode");

        // Root opencode.json pre-exists with unrelated entries; it must be
        // updated without clobbering them.
        let root_cfg = tmp.path().join("opencode.json");
        std::fs::write(
            &root_cfg,
            serde_json::to_string_pretty(
                &json!({ "plugins": ["./other.js"], "otherKey": "preserve" }),
            )
            .unwrap(),
        )
        .expect("write root opencode.json");

        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("opencode"),
        );
        assert_eq!(report["active_harness"], json!("opencode"));

        // Core plugin-core bundle must be present so relative imports resolve.
        assert!(tmp
            .path()
            .join(".NeuroStrata/plugins/core/dist/index.js")
            .exists());
        assert!(tmp
            .path()
            .join(".NeuroStrata/plugins/core/package.json")
            .exists());
        // Opencode bundle + hook runner.
        assert!(tmp
            .path()
            .join(".NeuroStrata/plugins/opencode/dist/index.js")
            .exists());
        assert!(tmp
            .path()
            .join(".NeuroStrata/plugins/opencode/cli-hook.mjs")
            .exists());

        // Thin loader wired into OpenCode.
        let loader = tmp.path().join(".opencode/plugins/neurostrata.js");
        assert!(loader.exists());
        let loader_src = std::fs::read_to_string(&loader).expect("read loader");
        assert!(
            loader_src.contains(
                "export { default } from \"../../.NeuroStrata/plugins/opencode/dist/index.js\";"
            ),
            "loader re-exports default"
        );
        assert!(
            loader_src
                .contains("export * from \"../../.NeuroStrata/plugins/opencode/dist/index.js\";"),
            "loader re-exports all"
        );

        // `.opencode/opencode.json` is never created or populated with the plugin path.
        let oc_cfg = tmp.path().join(".opencode/opencode.json");
        let oc_untouched = if oc_cfg.exists() {
            match std::fs::read_to_string(&oc_cfg)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            {
                Some(doc) => doc
                    .get("plugins")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        !arr.iter().any(|v| {
                            v.as_str() == Some("./.NeuroStrata/plugins/opencode/dist/index.js")
                        })
                    })
                    .unwrap_or(true),
                None => true,
            }
        } else {
            true
        };
        assert!(
            oc_untouched,
            ".opencode/opencode.json must not contain the loader path"
        );

        // Root opencode.json is never written: existing entries preserved, loader path never inserted.
        let root_doc: Value =
            serde_json::from_str(&std::fs::read_to_string(root_cfg).expect("read opencode.json"))
                .expect("parse opencode.json");
        let root_plugins = root_doc
            .get("plugins")
            .and_then(|v| v.as_array())
            .expect("plugins array");
        assert_eq!(
            root_plugins.len(),
            1,
            "root opencode.json plugins: {root_plugins:?}"
        );
        assert_eq!(root_plugins[0].as_str(), Some("./other.js"));
        assert!(!root_plugins
            .iter()
            .any(|v| v.as_str() == Some("./.NeuroStrata/plugins/opencode/dist/index.js")));
        assert_eq!(
            root_doc.get("otherKey").and_then(|v| v.as_str()),
            Some("preserve")
        );
    }

    #[test]
    fn opencode_wire_harness_config_removes_invalid_file_plugin_entry_and_installs_autoload_shim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let opencode_dir = root.join(".opencode");
        std::fs::create_dir_all(&opencode_dir).expect("mkdir .opencode");
        std::fs::write(
            opencode_dir.join("opencode.json"),
            r#"{
  "plugins": [
    "./.NeuroStrata/plugins/opencode/dist/index.js",
    "custom-plugin-dir"
  ]
}"#,
        )
        .expect("seed opencode.json");

        let wired = wire_harness_config(
            root,
            ConfigNamespace::Set("TestProj"),
            "opencode",
            true,
            &["opencode"],
            "./.NeuroStrata/plugins/opencode/dist/index.js",
            "./.NeuroStrata/plugins/opencode/cli-hook.mjs",
            &mut Vec::new(),
        );
        assert!(wired.is_some(), "loader should be wired for opencode");
        assert!(
            root.join(".opencode/plugins/neurostrata.js").is_file(),
            "autoload shim written at .opencode/plugins/neurostrata.js"
        );

        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(opencode_dir.join("opencode.json"))
                .expect("read opencode.json"),
        )
        .expect("valid json");
        // Invalid file-path entry stripped; valid entry preserved.
        let plugins = parsed
            .get("plugins")
            .and_then(|v| v.as_array())
            .expect("plugins array preserved for remaining entry");
        assert_eq!(plugins.len(), 1, "plugins: {plugins:?}");
        assert_eq!(plugins[0].as_str(), Some("custom-plugin-dir"));
    }

    #[test]
    fn subagent_graph_first_rule_is_verifiable() {
        let snippet = SUBAGENT_GRAPH_FIRST_RULE_SNIPPET;
        assert!(
            snippet.contains("check whether its path and governing memory IDs"),
            "snippet must use the verifiable path/governing-IDs predicate"
        );
        assert!(
            !snippet.contains("unless the parent prompt states"),
            "snippet must not contain the unenforceable parent-prompt bypass"
        );
        assert!(
            snippet.contains("memories_for_file"),
            "snippet must still direct to memories_for_file"
        );
    }

    /// PR #375 review: an existing GitHub CLI config is evidence the harness
    /// is present, so a refresh rewrites its stale paths.
    #[test]
    fn existing_github_cli_config_alone_triggers_a_refresh() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let cfg = dir.join(".NeuroStrata/plugins/github-cli/copilot-extension.json");
        write_file(
            &cfg,
            r#"{"id":"neurostrata-github-cli","namespace":"beta","entry":"stale"}"#,
        );
        let listed = vec![("github-cli".to_string(), "beta".to_string())];
        let report =
            auto_install_detected_plugins(&dir, ConfigNamespace::Each(&listed), Some("claude"));
        let installed = report["installed_plugins"].as_array().unwrap();
        assert!(
            installed
                .iter()
                .any(|v| v["harness"].as_str() == Some("github-cli")),
            "github-cli not refreshed: {installed:?}"
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_ne!(doc["entry"], json!("stale"), "stale config left in place");
    }

    /// PR #375 review: a nested folder's own harness config wins, also when
    /// the folder is named by a RELATIVE path; the fixture's namespace differs
    /// from the enclosing checkout's.
    #[test]
    fn a_relative_nested_project_resolves_its_own_namespace() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let top = base.join("outer-project");
        std::fs::create_dir_all(top.join(".git")).unwrap();
        write_file(
            &top.join(CLAUDE_SETTINGS),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "outer"}}"#,
        );
        let nested = top.join("packages").join("inner");
        write_file(
            &nested.join(CLAUDE_SETTINGS),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "inner"}}"#,
        );
        assert_eq!(ns_of(&top), "outer");
        assert_eq!(ns_of(&nested), "inner");
        let here = dunce::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let Some(relative) = relative_path_from(&here, &nested) else {
            eprintln!("skipped: the temp dir is on another drive than the cwd");
            return;
        };
        assert_eq!(
            resolve_namespace(&relative)
                .map(|r| r.namespace)
                .ok()
                .as_deref(),
            Some("inner"),
            "relative path {relative:?} resolved to the checkout namespace"
        );
    }

    /// `target` spelled relative to `base`; `None` when they share no root
    /// (another drive on Windows).
    fn relative_path_from(base: &Path, target: &Path) -> Option<PathBuf> {
        let b: Vec<_> = base.components().collect();
        let t: Vec<_> = target.components().collect();
        let common = b.iter().zip(t.iter()).take_while(|(a, b)| a == b).count();
        if common == 0 {
            return None;
        }
        let mut out = PathBuf::new();
        for _ in common..b.len() {
            out.push("..");
        }
        for c in &t[common..] {
            out.push(c.as_os_str());
        }
        Some(out)
    }

    /// A staged path must never be used as a symlink hop: a write into a
    /// symlink placed there would overwrite the link's target and then rename
    /// the link itself into place.
    #[test]
    #[cfg(unix)]
    fn write_atomically_refuses_a_symlink_at_the_temp_path() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let target = dir.join("config.json");
        let victim = dir.join("victim.json");
        std::fs::write(&victim, "original").unwrap();
        let staged = dir.join(".config.json.tmp-planted");
        symlink(&victim, &staged).unwrap();

        let result = write_staged_atomically(&staged, &target, "{\"ok\":true}");

        assert!(
            result.is_err(),
            "an existing symlink must refuse exclusive creation"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        assert!(
            !target.exists(),
            "the symlink must not be renamed into place"
        );
    }

    /// PR #388: between the exclusive open and the path-based chmod/rename a
    /// replaced staged entry must be rejected without touching its target.
    #[test]
    #[cfg(unix)]
    fn a_replaced_staged_path_is_not_the_open_file() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let staged = dir.join(".staged");
        std::fs::write(&staged, "ours").unwrap();
        let opened = std::fs::metadata(&staged).unwrap();
        assert!(staged_path_is_open_file(&staged, &opened).unwrap());

        // Moved aside, not removed: a removed file's inode can be reused by
        // the next file created, which would then look like the opened one.
        // The symlink points at the opened file itself, so a check that
        // followed it would wrongly say yes.
        std::fs::rename(&staged, dir.join(".ours")).unwrap();
        symlink(dir.join(".ours"), &staged).unwrap();
        assert!(
            !staged_path_is_open_file(&staged, &opened).unwrap(),
            "a symlink planted at the staged path must not be followed"
        );

        std::fs::remove_file(&staged).unwrap();
        std::fs::write(&staged, "theirs").unwrap();
        assert!(
            !staged_path_is_open_file(&staged, &opened).unwrap(),
            "another file at the staged path is not the one we opened"
        );
    }

    /// PR #388: the staged identity check must run before either path-based
    /// operation, never after one has already touched the replacement.
    #[test]
    fn a_staged_config_is_verified_before_it_is_published() {
        let source = include_str!("harness_plugins.rs");
        let check = source
            .find("staged_path_is_open_file(staged, &opened)")
            .expect("identity check");
        let chmod = source
            .find("std::fs::set_permissions(staged")
            .expect("permission update");
        let rename = source
            .find("std::fs::rename(staged, path)")
            .expect("publish rename");
        assert!(check < chmod && check < rename);
    }

    /// PR #388: the OpenCode runner must pass the same effective config the
    /// loader and `namespace resolve` use, not the stale registration alone.
    #[test]
    fn installed_runner_reads_opencodes_effective_config_first() {
        assert!(CLI_HOOK_RUNNER_MJS.contains("HARNESS === \"opencode\""));
        assert!(CLI_HOOK_RUNNER_MJS.contains("readOpenCodeConfig()"));
        assert!(CLI_HOOK_RUNNER_MJS.contains("\"neurostrata-plugin.json\", \"PluginConfig.json\""));
        // Other harnesses keep reading their own registration.
        assert!(CLI_HOOK_RUNNER_MJS.contains("readHarnessConfig(HARNESS, projectDir, RUNNER_DIR)"));
    }

    /// Concurrent config writes to the same path must not share a staged
    /// filename: an occupied predictable path must not break a write.
    #[test]
    fn write_atomically_uses_a_unique_staged_name_per_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let target = dir.join("config.json");
        // Occupy the old PID-based staged path with a directory. A unique
        // staged name is unaffected; a shared one cannot be opened at all.
        let staged = dir.join(format!(".config.json.tmp-{}", std::process::id()));
        std::fs::create_dir(&staged).unwrap();

        write_atomically(&target, "{\"ok\":true}").expect("write with a fresh staged name");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"ok\":true}");
    }

    /// Review on #385: a file already at the staged path belongs to someone
    /// else; a write that cannot create its own staged file leaves it alone.
    #[test]
    fn a_failed_write_keeps_an_entry_already_at_the_staged_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let staged = dir.join(".config.json.tmp-taken");
        std::fs::write(&staged, "someone else's").unwrap();

        assert!(write_staged_atomically(&staged, &dir.join("config.json"), "{}").is_err());

        assert_eq!(std::fs::read_to_string(&staged).unwrap(), "someone else's");
    }

    /// An atomic replacement must not widen a private config's permissions:
    /// the staged file starts restrictive and inherits the old mode.
    #[test]
    #[cfg(unix)]
    fn write_atomically_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let target = dir.join("settings.local.json");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();

        write_atomically(&target, "new").expect("write");

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "the original mode must survive the rename");
    }

    /// A project marker that names a namespace outranks the process
    /// environment, even when no harness config names one.
    #[test]
    fn project_marker_namespace_beats_environment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        marker_at(&dir, "from-marker");
        let got = resolve_namespace_from(&dir, Some("from-env")).expect("resolves");
        assert_eq!(got.namespace, "from-marker");
        assert_eq!(got.source, NamespaceSource::Marker);
    }

    fn read_codex_hooks(root: &Path) -> Value {
        let raw = std::fs::read_to_string(root.join(".codex/hooks.json")).expect("read hooks.json");
        serde_json::from_str(&raw).expect("parse hooks.json")
    }

    fn codex_commands(doc: &Value, event: &str) -> Vec<String> {
        doc["hooks"][event]
            .as_array()
            .expect("event array")
            .iter()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect()
    }

    /// #333: Codex CLI runs hooks from `.codex/hooks.json`; the
    /// `.codex/plugins/neurostrata.json` file alone never ran anything.
    #[test]
    fn codex_install_wires_hooks_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).expect("create .codex");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner")
            .to_string();
        let expected = format!("node {}", shell_literal(&runner));

        let doc = read_codex_hooks(tmp.path());
        let pre = doc["hooks"]["PreToolUse"].as_array().expect("PreToolUse");
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["matcher"], json!("apply_patch|Bash"));
        // NeuroStrata's own MCP calls (Codex names them mcp__<server>__<tool>).
        assert_eq!(pre[1]["matcher"], json!(SESSION_SCOPE_MATCHER));
        assert_eq!(pre[0]["hooks"][0]["type"], json!("command"));
        assert_eq!(
            codex_commands(&doc, "PreToolUse"),
            vec![expected.clone(), expected.clone()]
        );
        assert_eq!(codex_commands(&doc, "Stop"), vec![expected.clone()]);
        // The session-start injection and the handoff write (session file
        // contract) run on these, with no matcher.
        for event in ["SessionStart", "SessionEnd", "PreCompact"] {
            assert_eq!(
                codex_commands(&doc, event),
                vec![expected.clone()],
                "{event}"
            );
            assert!(doc["hooks"][event][0].get("matcher").is_none(), "{event}");
        }
        // No namespace in the command: the callers that refresh it only have
        // a folder-name guess, and PowerShell cannot run a NAME=value prefix.
        assert!(!expected.contains("TestProj") && !expected.contains("NEUROSTRATA_NAMESPACE="));
        assert!(tmp.path().join(".codex/plugins/neurostrata.json").exists());

        // A second run adds nothing.
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        assert_eq!(read_codex_hooks(tmp.path()), doc);
    }

    /// Ownership is this project's runner only: a foreign `cli-hook.mjs`
    /// (even at the installer layout under another folder) is kept as-is and
    /// our hook is added beside it, not repointed (#368 F9).
    #[test]
    fn codex_hooks_json_keeps_a_foreign_runner_and_adds_ours() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).expect("create .codex");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner")
            .to_string();
        let expected = format!("node {}", shell_literal(&runner));
        let gone = tmp
            .path()
            .join("moved-away/.NeuroStrata/plugins/codex/cli-hook.mjs");
        let stale = format!("node {}", shell_literal(&gone.to_string_lossy()));

        let user = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "echo user-hook" }] },
                    { "matcher": "apply_patch|Bash", "hooks": [{ "type": "command", "command": stale.clone() }] }
                ],
                "SessionStart": [{ "hooks": [{ "type": "command", "command": "echo start" }] }]
            },
            "other": true
        });
        let path = tmp.path().join(".codex/hooks.json");
        std::fs::write(&path, serde_json::to_string_pretty(&user).unwrap()).expect("write");

        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let doc = read_codex_hooks(tmp.path());
        assert_eq!(
            codex_commands(&doc, "PreToolUse"),
            vec![
                "echo user-hook".to_string(),
                stale,
                expected.clone(),
                expected.clone()
            ],
            "user hook kept, foreign runner untouched, ours added once per entry"
        );
        assert_eq!(codex_commands(&doc, "Stop"), vec![expected.clone()]);
        assert_eq!(
            codex_commands(&doc, "SessionStart"),
            vec!["echo start".to_string(), expected],
            "the user's SessionStart hook kept, ours added beside it"
        );
        assert_eq!(doc["other"], json!(true));

        // A hooks.json that is not a JSON object is the user's to fix.
        std::fs::write(&path, "not json").expect("write");
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "not json");

        // Neither is one that is not UTF-8 (e.g. saved as UTF-16).
        let utf16: Vec<u8> = vec![0xFF, 0xFE, b'{', 0, b'}', 0];
        std::fs::write(&path, &utf16).expect("write");
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        assert_eq!(std::fs::read(&path).expect("read"), utf16);
    }

    /// Refreshes call the installer with other namespaces: `hook_hygiene`
    /// (get_snapshot, hooks install) passes the checkout folder name and
    /// daemon boot passes none. None of them may change the hooks or pin a
    /// guessed namespace (Codex review on #368).
    #[test]
    fn codex_hooks_do_not_depend_on_the_callers_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).expect("create .codex");
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let before = read_codex_hooks(tmp.path());
        for other in ["TestProj", "", "folder-name"] {
            let _ = auto_install_detected_plugins(
                tmp.path(),
                ConfigNamespace::Set(other),
                Some("codex"),
            );
            assert_eq!(
                read_codex_hooks(tmp.path()),
                before,
                "refresh with {other:?} must change nothing"
            );
        }
        for command in codex_commands(&before, "PreToolUse") {
            assert!(!command.contains("folder-name"), "guess pinned: {command}");
        }
    }

    /// The runner path is quoted as one literal word: `$`, backticks and
    /// `$(...)` in a checkout path must not expand in sh or PowerShell, and an
    /// embedded single quote must round-trip (Codex review on #368).
    #[test]
    fn codex_hook_command_quotes_the_runner_path_literally() {
        for path in [
            "/home/u/Team$QA/.NeuroStrata/plugins/codex/cli-hook.mjs",
            "/home/u/R$(printf injected)/`id`/cli-hook.mjs",
            r"C:\Users\o'brien\proj dir\.NeuroStrata\plugins\codex\cli-hook.mjs",
        ] {
            let command = codex_hook_command(path);
            let word = command.strip_prefix("node ").expect("node prefix");
            assert!(word.starts_with('\'') && word.ends_with('\''), "{command}");
            let (back, rest) = shell_literal_value(word).expect("parse back");
            assert_eq!(back, path);
            assert_eq!(rest, "");
        }
        if cfg!(windows) {
            assert_eq!(shell_literal("a'b"), "'a''b'");
        } else {
            assert_eq!(shell_literal("a'b"), r"'a'\''b'");
        }
    }

    /// A user hook that runs the runner and then more is not the
    /// installer's: refreshes must not strip its suffix (Codex review on
    /// #368).
    #[test]
    fn codex_refresh_keeps_a_user_command_that_extends_the_runner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).expect("create .codex");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner")
            .to_string();
        let ours = format!("node {}", shell_literal(&runner));
        let custom = format!("{ours} && project-policy-check");
        let user = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "apply_patch|Bash", "hooks": [{ "type": "command", "command": custom }] }
                ]
            }
        });
        std::fs::write(
            tmp.path().join(".codex/hooks.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .expect("write");

        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let doc = read_codex_hooks(tmp.path());
        assert_eq!(
            codex_commands(&doc, "PreToolUse"),
            vec![custom, ours.clone(), ours.clone()],
            "the user command keeps its suffix"
        );
        assert_eq!(codex_commands(&doc, "Stop"), vec![ours]);
    }

    /// No runner may pass `namespace: undefined` to initialize: it would
    /// override the namespace in .NeuroStrata/neurostrata-plugin.json when
    /// the hook sets no NEUROSTRATA_NAMESPACE (Codex review on #368).
    #[test]
    fn installed_runner_passes_a_namespace_only_when_set() {
        assert!(!CLI_HOOK_RUNNER_MJS.contains("{ namespace: process.env.NEUROSTRATA_NAMESPACE }"));
        // The namespace now comes from the harness config the installer wrote:
        // `readHarnessConfig` returns `{}` when none is set, so an explicit
        // `namespace: undefined` is never passed, and `projectDir` travels
        // with it for root-lookup spelling.
        assert!(CLI_HOOK_RUNNER_MJS.contains("readHarnessConfig(HARNESS, projectDir, RUNNER_DIR)"));
        assert!(CLI_HOOK_RUNNER_MJS.contains(", projectDir });"));
    }

    /// Ownership is this project's canonical runner, not any path ending in
    /// the installer layout: a nested/moved checkout's `cli-hook.mjs` (even
    /// one holding `$` or `%`) is left untouched and ours is added beside it
    /// (#368 F9).
    #[test]
    fn codex_refresh_owns_only_this_projects_runner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).expect("create .codex");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner")
            .to_string();
        let ours = format!("node {}", shell_literal(&runner));
        let user_policy = format!(
            "node {}",
            shell_literal(
                &tmp.path()
                    .join("tools/project-policy/cli-hook.mjs")
                    .to_string_lossy()
            )
        );
        let moved = format!(
            "node {}",
            shell_literal(
                &tmp.path()
                    .join("old $HOME %USERPROFILE% copy/.NeuroStrata/plugins/codex/cli-hook.mjs")
                    .to_string_lossy()
            )
        );
        let user = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": user_policy.clone() }] },
                    { "matcher": "apply_patch|Bash", "hooks": [{ "type": "command", "command": moved.clone() }] }
                ],
                "Stop": [{ "hooks": [{ "type": "command", "command": moved.clone() }] }]
            }
        });
        std::fs::write(
            tmp.path().join(".codex/hooks.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .expect("write");

        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let doc = read_codex_hooks(tmp.path());
        assert_eq!(
            codex_commands(&doc, "PreToolUse"),
            vec![user_policy, moved.clone(), ours.clone(), ours.clone()],
            "user hook kept as written; foreign runner kept; ours added once per entry"
        );
        assert_eq!(
            codex_commands(&doc, "Stop"),
            vec![moved, ours],
            "foreign Stop hook kept as written; ours added beside it"
        );
    }

    /// The installed runner works with a plugin build that has no session
    /// handlers (merged in either order): it answers `{}` on a session event
    /// the bundle cannot handle, since that bundle's default reply carries a
    /// permission decision Codex's SessionStart and PreCompact output schemas
    /// reject (`additionalProperties: false`, Codex 0.170). A failed session
    /// event also answers `{}`, and Copilot gets its top-level deny shape.
    #[test]
    fn runner_answers_session_events_an_older_plugin_cannot_handle() {
        for needle in [
            "typeof mod.handleSessionStartEvent === \"function\"",
            "typeof mod.writeSessionHandoff === \"function\"",
            "process.env.NEUROSTRATA_HOOK_EVENT",
            "if (noDecisionEvent && !handled) {\n    await writeStdout(\"{}\");",
            "JSON.stringify({ permissionDecision: \"deny\", permissionDecisionReason: `NeuroStrata hook runner failed: ${reason}` })",
        ] {
            assert!(CLI_HOOK_RUNNER_MJS.contains(needle), "{needle}");
        }
    }

    /// Runs the installed runner under node with a stand-in plugin bundle.
    /// The hook input names `named` in `hook_event_name` (Claude, Codex);
    /// `copilot` goes into `NEUROSTRATA_HOOK_EVENT` (Copilot names its event
    /// only there). `flags` become env variables the bundle reads. Returns
    /// the reply and the exit code; `None` when node is not installed.
    fn run_installed_runner(
        named: Option<&str>,
        copilot: Option<&str>,
        flags: &[&str],
    ) -> Option<(Value, i32)> {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("github-cli");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(dir.join("cli-hook.mjs"), CLI_HOOK_RUNNER_MJS).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"type": "module"}"#).unwrap();
        // HANDLERS exports every handler a newer bundle has; THROW fails the
        // handler; THROW_EARLY fails the project folder lookup, before the
        // plugin is initialized.
        std::fs::write(
            dir.join("dist/index.js"),
            r#"const on = (name) => Boolean(process.env[name]);
const marker = on("HANDLERS") ? async () => ({}) : undefined;
export const handleSessionStartEvent = marker;
export const writeSessionHandoff = marker;
export const handlePostToolUseEvent = marker;
export const resolveHookProjectDir = on("THROW_EARLY")
  ? () => { throw new Error("early"); }
  : undefined;
export async function handleHookEvent() {
  if (on("THROW")) throw new Error("boom");
  return { ran: "handleHookEvent" };
}
export default {};
"#,
        )
        .unwrap();
        let mut command = std::process::Command::new("node");
        command
            .arg(dir.join("cli-hook.mjs"))
            .current_dir(tmp.path())
            .env_remove("NEUROSTRATA_HOOK_EVENT")
            .env_remove("NEUROSTRATA_NAMESPACE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(event) = copilot {
            command.env("NEUROSTRATA_HOOK_EVENT", event);
        }
        for name in ["HANDLERS", "THROW", "THROW_EARLY"] {
            command.env_remove(name);
        }
        for flag in flags {
            command.env(flag, "1");
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                eprintln!("skipped: node is not installed");
                return None;
            }
        };
        let mut input = json!({
            "sessionId": "s1",
            "timestamp": 1,
            "cwd": tmp.path().to_string_lossy(),
            "toolName": "bash",
            "toolArgs": "{}"
        });
        if let Some(event) = named {
            input["hook_event_name"] = json!(event);
        }
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let reply = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "{named:?} {copilot:?} {flags:?}: stdout {stdout:?} is not JSON ({e}); stderr {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        Some((reply, out.status.code().unwrap_or(-1)))
    }

    /// Merged before the plugin handles postToolUse, the runner answers `{}`
    /// to it instead of running the bundle's PreToolUse work a second time;
    /// a bundle that exports `handlePostToolUseEvent` gets it through
    /// `handleHookEvent`. A failed event replies in its own shape: Stop gets
    /// `{}` with exit code 2 (a block for Claude Code), PreToolUse a deny (an
    /// unnamed event counts as one), and every other event `{}`.
    #[test]
    fn installed_runner_replies_in_the_shape_of_the_event() {
        let ran = json!({ "ran": "handleHookEvent" });
        let empty = json!({});
        let cases: [(Option<&str>, Option<&str>, &[&str], &Value, i32); 16] = [
            (None, Some("postToolUse"), &[], &empty, 0),
            (None, Some("postToolUse"), &["HANDLERS"], &ran, 0),
            (None, Some("postToolUse"), &["HANDLERS", "THROW"], &empty, 0),
            (
                None,
                Some("postToolUse"),
                &["HANDLERS", "THROW_EARLY"],
                &empty,
                0,
            ),
            (None, Some("preToolUse"), &[], &ran, 0),
            (
                None,
                Some("sessionEnd"),
                &["HANDLERS", "THROW_EARLY"],
                &empty,
                0,
            ),
            (None, Some("sessionEnd"), &["HANDLERS"], &ran, 0),
            (Some("SessionStart"), None, &["HANDLERS"], &ran, 0),
            (Some("PreCompact"), None, &["HANDLERS"], &ran, 0),
            (
                None,
                Some("sessionStart"),
                &["HANDLERS", "THROW"],
                &empty,
                0,
            ),
            (Some("PreCompact"), None, &[], &empty, 0),
            (Some("PreCompact"), None, &["HANDLERS", "THROW"], &empty, 0),
            (Some("PostToolUse"), None, &[], &empty, 0),
            (Some("SubagentStop"), None, &["THROW"], &empty, 0),
            (Some("Stop"), None, &["THROW"], &empty, 2),
            (Some("Stop"), None, &[], &ran, 0),
        ];
        for (named, copilot, flags, expected, code) in cases {
            let Some((reply, status)) = run_installed_runner(named, copilot, flags) else {
                return;
            };
            assert_eq!(
                (&reply, status),
                (expected, code),
                "{named:?} {copilot:?} {flags:?}"
            );
        }
        // Copilot reads a top-level decision on preToolUse (with the package
        // runners' reason); the others read hookSpecificOutput. A deny has
        // no other field: Codex rejects one (such as `continue`) on
        // PreToolUse.
        let copilot_deny = |reason: &str| {
            json!({
                "permissionDecision": "deny",
                "permissionDecisionReason": format!("NeuroStrata hook runner failed: {reason}")
            })
        };
        let deny = json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "Error: boom"
        }});
        let denies: [(Option<&str>, Option<&str>, &[&str], Value); 4] = [
            (
                None,
                Some("preToolUse"),
                &["THROW"],
                copilot_deny("Error: boom"),
            ),
            (
                None,
                Some("preToolUse"),
                &["THROW_EARLY"],
                copilot_deny("Error: early"),
            ),
            (Some("PreToolUse"), None, &["THROW"], deny.clone()),
            (None, None, &["THROW"], deny),
        ];
        for (named, copilot, flags, expected) in denies {
            let Some((reply, status)) = run_installed_runner(named, copilot, flags) else {
                return;
            };
            assert_eq!(
                (&reply, status),
                (&expected, 0),
                "{named:?} {copilot:?} {flags:?}"
            );
        }
    }

    // ---- issue #382 regressions ----

    /// #368 F5/F10: both command forms are written, and the embedded runner
    /// answers a failed hook with a PreToolUse deny instead of `exit 2`
    /// (PowerShell swallows a nonzero exit code and the tool runs anyway).
    #[test]
    fn codex_hooks_carry_windows_variant_and_runner_denies_on_error() {
        assert!(CLI_HOOK_RUNNER_MJS.contains("permissionDecision: \"deny\""));
        assert!(!CLI_HOOK_RUNNER_MJS.contains("\n  process.exit(2);"));
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner");
        let doc = read_codex_hooks(tmp.path());
        for (event, _) in CODEX_HOOK_EVENTS {
            let hook = &doc["hooks"][event][0]["hooks"][0];
            assert_eq!(
                hook["command"],
                json!(format!("node {}", shell_literal(runner)))
            );
            assert_eq!(
                hook["commandWindows"],
                json!(format!("node {}", powershell_literal(runner)))
            );
        }
    }

    /// #368 F4/F10: a tracked `.codex` symlink must not redirect the hook or
    /// metadata write into a user file; the absolute runner path in the
    /// untracked hooks.json is added once to the local git exclude.
    #[cfg(unix)]
    #[test]
    fn codex_writes_refuse_symlinks_and_exclude_hooks_once() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let target = outside.join("hooks.json");
        std::fs::write(&target, "{}").unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        std::fs::write(tmp.path().join(".git/info/exclude"), "# local\n").unwrap();
        symlink(&outside, tmp.path().join(".codex")).unwrap();

        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("symlink"))),
            "{report}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}");
        assert!(!outside.join("plugins/neurostrata.json").exists());

        std::fs::remove_file(tmp.path().join(".codex")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        for _ in 0..2 {
            let _ = auto_install_detected_plugins(
                tmp.path(),
                ConfigNamespace::Set("TestProj"),
                Some("codex"),
            );
        }
        let exclude = std::fs::read_to_string(tmp.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude.matches("/.codex/hooks.json").count(),
            1,
            "{exclude}"
        );
    }

    /// #368 F6/F8: an unreadable hooks.json yields no `config_wired` claim
    /// plus a warning; env/argument-prefixed commands and `[hooks]` in the
    /// other Codex config layer are recognized so no second hook is added.
    #[test]
    fn codex_wiring_reports_hook_state_and_recognizes_other_layers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let ours = format!("node {}", shell_literal(&runner));
        let path = tmp.path().join(".codex/hooks.json");

        // A BOM'd hooks.json is left alone and is not reported as wired.
        std::fs::write(&path, "\u{feff}{\"hooks\":{}}").unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        assert!(
            report["installed_plugins"][0]["config_wired"].is_null(),
            "{report}"
        );
        assert!(report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("byte order mark"))));

        // A prefixed form of our own command is repaired, not duplicated.
        let prefixed = format!("env NEUROSTRATA_NAMESPACE=TestProj {ours}");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({"hooks": {
                "PreToolUse": [{"hooks": [{"type": "command", "command": prefixed}]}]
            }}))
            .unwrap(),
        )
        .unwrap();
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        // The repaired entry has no matcher, so it stands for the file-tool
        // entry; the session-scope entry is added beside it.
        assert_eq!(
            codex_commands(&read_codex_hooks(tmp.path()), "PreToolUse"),
            vec![ours.clone(), ours.clone()]
        );

        // A `[hooks.pre_tool_use]` entry in config.toml covers PreToolUse;
        // Stop is still wired because that section does not cover it.
        std::fs::write(&path, serde_json::to_string(&json!({"hooks": {}})).unwrap()).unwrap();
        std::fs::write(
            tmp.path().join(".codex/config.toml"),
            format!(
                "[hooks.pre_tool_use]\ncommand = \"{}\"\n",
                runner.replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let doc = read_codex_hooks(tmp.path());
        // The TOML entry stands for the file-tool entry only.
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "{doc}");
        assert_eq!(pre[0]["matcher"], json!(SESSION_SCOPE_MATCHER));
        assert_eq!(codex_commands(&doc, "Stop"), vec![ours]);
    }

    /// An older install (or a checkout shared with POSIX) may carry only
    /// `command`. Recognizing our hook must repair both platform variants;
    /// otherwise Windows keeps running the POSIX-quoted command.
    #[test]
    fn codex_refresh_adds_the_missing_windows_command_to_an_owned_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let (cmd, cmd_windows) = codex_hook_commands(&runner);
        std::fs::write(
            tmp.path().join(".codex/hooks.json"),
            serde_json::to_string_pretty(&json!({"hooks": {
                "PreToolUse": [{"hooks": [{"type": "command", "command": cmd.clone()}]}],
                "Stop": [{"hooks": [{"type": "command", "command": cmd.clone()}]}]
            }}))
            .unwrap(),
        )
        .unwrap();
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let doc = read_codex_hooks(tmp.path());
        for (event, entries) in [("PreToolUse", 2), ("Stop", 1)] {
            let hook = &doc["hooks"][event][0]["hooks"][0];
            assert_eq!(hook["command"], json!(cmd), "{event}");
            assert_eq!(hook["commandWindows"], json!(cmd_windows), "{event}");
            assert_eq!(codex_commands(&doc, event).len(), entries, "{event}");
        }
    }

    /// A comment or an unrelated setting mentioning the runner path is not a
    /// hook command. It must not suppress wiring the real hooks.json entry.
    #[test]
    fn a_comment_mentioning_the_runner_does_not_wire_the_codex_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let ours = codex_hook_command(&runner);
        std::fs::write(
            tmp.path().join(".codex/hooks.json"),
            serde_json::to_string(&json!({"hooks": {}})).unwrap(),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join(".codex/config.toml"),
            format!(
                "# the canonical runner is {runner}\n[hooks.pre_tool_use]\nenv = \"{}\"\n",
                runner.replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let doc = read_codex_hooks(tmp.path());
        assert_eq!(codex_commands(&doc, "PreToolUse"), vec![ours.clone(), ours]);
    }

    fn read_copilot_hooks(root: &Path) -> Value {
        let raw = std::fs::read_to_string(root.join(COPILOT_HOOKS_REL)).expect("read hooks file");
        serde_json::from_str(&raw).expect("parse hooks file")
    }

    /// Copilot CLI runs hooks only from `.github/hooks/*.json`; the
    /// extension file alone never ran anything.
    #[test]
    fn github_cli_install_wires_copilot_hooks_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".github")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/info")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        let entry = &report["installed_plugins"][0];
        assert_eq!(entry["harness"], json!("github-cli"), "{report}");
        let runner = entry["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let path = tmp.path().join(COPILOT_HOOKS_REL);
        assert_eq!(entry["config_wired"], json!(path.to_string_lossy()));
        assert!(tmp
            .path()
            .join(".NeuroStrata/plugins/github-cli/copilot-extension.json")
            .exists());

        let doc = read_copilot_hooks(tmp.path());
        assert_eq!(doc["version"], json!(1));
        let mut events: Vec<&str> = doc["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        events.sort_unstable();
        let wired: Vec<&str> = copilot_hook_events(GITHUB_CLI_INDEX_JS).collect();
        let mut expected = wired.clone();
        expected.sort_unstable();
        assert_eq!(events, expected, "{doc}");
        for event in wired {
            assert_eq!(
                doc["hooks"][event],
                json!([{
                    "type": "command",
                    "bash": format!("node {}", posix_literal(&runner)),
                    "powershell": format!("node {}", powershell_literal(&runner)),
                    "env": { "NEUROSTRATA_HOOK_EVENT": event }
                }]),
                "{event}"
            );
        }

        // A second run leaves the file as it is and excludes it only once.
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let exclude = std::fs::read_to_string(tmp.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude.matches("/.github/hooks/neurostrata.json").count(),
            1,
            "{exclude}"
        );
    }

    /// The installer's file keeps the user's entries and keys; its own entry
    /// is repaired in place (old quoting, extra env kept), never duplicated.
    /// A file that is not a version 1 JSON object is left alone.
    #[test]
    fn copilot_hooks_file_merges_and_keeps_user_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".github")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let bash = format!("node {}", posix_literal(&runner));
        let powershell = format!("node {}", powershell_literal(&runner));
        let path = tmp.path().join(COPILOT_HOOKS_REL);
        let user_entry = json!({ "type": "command", "bash": "echo hi", "powershell": "echo hi" });
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "version": 1,
                "hooks": {
                    "sessionStart": [user_entry.clone()],
                    "preCompact": [{
                        "type": "command",
                        "bash": format!("env NEUROSTRATA_NAMESPACE=x {bash}"),
                        "env": { "KEEP": "1" },
                        "timeoutSec": 10
                    }],
                    "sessionEnd": [{
                        "type": "command",
                        "bash": bash,
                        "powershell": "echo custom"
                    }],
                    "postToolUse": [user_entry.clone()]
                },
                "other": true
            }))
            .unwrap(),
        )
        .unwrap();
        let _ = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        let doc = read_copilot_hooks(tmp.path());
        let ours = |event: &str| {
            json!({
                "type": "command",
                "bash": bash,
                "powershell": powershell,
                "env": { "NEUROSTRATA_HOOK_EVENT": event }
            })
        };
        assert_eq!(
            doc["hooks"]["sessionStart"],
            json!([user_entry.clone(), ours("sessionStart")])
        );
        assert_eq!(
            doc["hooks"]["preCompact"],
            json!([{
                "type": "command",
                "bash": bash,
                "powershell": powershell,
                "env": { "KEEP": "1", "NEUROSTRATA_HOOK_EVENT": "preCompact" },
                "timeoutSec": 10
            }])
        );
        // A user's own command in the other shell's key is kept.
        assert_eq!(
            doc["hooks"]["sessionEnd"],
            json!([{
                "type": "command",
                "bash": bash,
                "powershell": "echo custom",
                "env": { "NEUROSTRATA_HOOK_EVENT": "sessionEnd" }
            }])
        );
        // postToolUse gets our entry only when the shipped bundle handles it.
        let post_tool_use = if bundle_exports(GITHUB_CLI_INDEX_JS, "handlePostToolUseEvent") {
            json!([user_entry.clone(), ours("postToolUse")])
        } else {
            json!([user_entry.clone()])
        };
        assert_eq!(doc["hooks"]["postToolUse"], post_tool_use);
        assert_eq!(doc["other"], json!(true));

        for left_alone in ["not json", r#"{"version": 2, "hooks": {}}"#] {
            std::fs::write(&path, left_alone).unwrap();
            let report = auto_install_detected_plugins(
                tmp.path(),
                ConfigNamespace::Set("TestProj"),
                Some("github-cli"),
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), left_alone);
            assert!(
                report["installed_plugins"][0]["config_wired"].is_null(),
                "{report}"
            );
            assert!(
                report["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w.as_str().is_some_and(|s| s.contains("no Copilot hook"))),
                "{report}"
            );
        }
    }

    /// postToolUse is wired only for a bundle that exports
    /// `handlePostToolUseEvent`; otherwise the runner would answer `{}` after
    /// starting node on every tool call. For a bundle without it, the
    /// runner's commands leave an entry the installer wrote earlier (dropped
    /// when no command is left, then the emptied list); a user's own entries
    /// and commands stay.
    #[test]
    fn copilot_post_tool_use_hook_follows_the_bundle() {
        let handles =
            "export async function handlePostToolUseEvent(event, dialect = 'copilot') {\n}\n";
        let lacks = "// handlePostToolUseEvent is only mentioned here\n\
                     export async function handlePostToolUseEventLater() {}\n\
                     export async function handleHookEvent(event) {}\n";
        assert!(bundle_exports(handles, "handlePostToolUseEvent"));
        assert!(bundle_exports(
            "export const handlePostToolUseEvent = async () => ({});",
            "handlePostToolUseEvent"
        ));
        assert!(!bundle_exports(lacks, "handlePostToolUseEvent"));

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".github")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let bash = format!("node {}", posix_literal(&runner));
        let ours = json!({
            "type": "command",
            "bash": bash,
            "powershell": format!("node {}", powershell_literal(&runner)),
            "env": { "NEUROSTRATA_HOOK_EVENT": "postToolUse" }
        });
        let wire = |bundle: &str| {
            let mut warnings = Vec::new();
            let wired = wire_copilot_hooks(tmp.path(), &runner, bundle, &mut warnings);
            assert!(wired.is_some(), "{warnings:?}");
            read_copilot_hooks(tmp.path())
        };

        let mut doc = wire(handles);
        assert_eq!(doc["hooks"]["postToolUse"], json!([ours.clone()]));
        assert!(wire(lacks)["hooks"].get("postToolUse").is_none());

        let user_entry = json!({ "type": "command", "bash": "echo hi", "powershell": "echo hi" });
        doc["hooks"]["postToolUse"] = json!([
            user_entry.clone(),
            ours.clone(),
            { "type": "command", "bash": bash, "powershell": "echo custom" }
        ]);
        std::fs::write(
            tmp.path().join(COPILOT_HOOKS_REL),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        let users = json!([
            user_entry,
            { "type": "command", "powershell": "echo custom" }
        ]);
        assert_eq!(wire(lacks)["hooks"]["postToolUse"], users);
        let mut with_ours = users.as_array().unwrap().clone();
        with_ours.push(ours);
        assert_eq!(wire(handles)["hooks"]["postToolUse"], json!(with_ours));
    }

    /// preToolUse is wired only for a bundle that exports
    /// `copilotPreToolUseReply`, the reply in the shape Copilot reads (a
    /// top-level decision); without it the bundle's deny would be ignored.
    #[test]
    fn copilot_pre_tool_use_hook_follows_the_bundle() {
        let handles = "export function copilotPreToolUseReply(reply, originalArgs) {
}
";
        let lacks = "// copilotPreToolUseReply is only mentioned here
                     export async function handleHookEvent(event) {}
";
        assert!(bundle_exports(handles, "copilotPreToolUseReply"));
        assert!(!bundle_exports(lacks, "copilotPreToolUseReply"));
        assert!(copilot_hook_events(handles).any(|e| e == "preToolUse"));
        assert!(!copilot_hook_events(lacks).any(|e| e == "preToolUse"));

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".github")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let wire = |bundle: &str| {
            let mut warnings = Vec::new();
            let wired = wire_copilot_hooks(tmp.path(), &runner, bundle, &mut warnings);
            assert!(wired.is_some(), "{warnings:?}");
            read_copilot_hooks(tmp.path())
        };
        assert_eq!(
            wire(handles)["hooks"]["preToolUse"],
            json!([{
                "type": "command",
                "bash": format!("node {}", posix_literal(&runner)),
                "powershell": format!("node {}", powershell_literal(&runner)),
                "env": { "NEUROSTRATA_HOOK_EVENT": "preToolUse" }
            }])
        );
        assert!(wire(lacks)["hooks"].get("preToolUse").is_none());
    }

    /// The `bash` command is quoted for `sh` on every platform, so a path
    /// with a single quote round-trips through both parsers.
    #[test]
    fn copilot_bash_command_uses_sh_quoting_on_every_platform() {
        let path = r"/home/o'brien/$HOME/.NeuroStrata/plugins/github-cli/cli-hook.mjs";
        let word = posix_literal(path);
        assert_eq!(
            word,
            r"'/home/o'\''brien/$HOME/.NeuroStrata/plugins/github-cli/cli-hook.mjs'"
        );
        assert_eq!(posix_literal_value(&word), Some((path.to_string(), "")));
        let command = format!("node {word}");
        assert_eq!(hook_command_runners(&command), vec![path.to_string()]);
        let ps = format!("node {}", powershell_literal(path));
        assert_eq!(hook_command_runners(&ps), vec![path.to_string()]);
    }

    /// PowerShell ends a single-quoted string at a typographic quote too
    /// (U+2018 to U+201B), so each one in a path is doubled like `'`. The
    /// PowerShell parser reads `'C:\o’’brien\x‘‘y\a''b\cli-hook.mjs'` as
    /// `C:\o’brien\x‘y\a'b\cli-hook.mjs` (checked with
    /// `[System.Management.Automation.Language.Parser]::ParseInput`).
    #[test]
    fn powershell_literal_doubles_typographic_quotes() {
        let path = "C:\\o\u{2019}brien\\x\u{2018}y\\a'b\\cli-hook.mjs";
        let word = powershell_literal(path);
        assert_eq!(
            word,
            "'C:\\o\u{2019}\u{2019}brien\\x\u{2018}\u{2018}y\\a''b\\cli-hook.mjs'"
        );
        assert_eq!(
            powershell_literal_value(&word),
            Some((path.to_string(), ""))
        );
        for quote in POWERSHELL_SINGLE_QUOTES {
            let path = format!("C:\\a{quote}b\\cli-hook.mjs");
            let word = powershell_literal(&path);
            assert_eq!(powershell_literal_value(&word), Some((path.clone(), "")));
            // `sh` reads the typographic quotes literally, so the command has
            // two readings; the PowerShell one is among them.
            assert!(hook_command_runners(&format!("node {word}")).contains(&path));
        }
        // A lone typographic quote ends the word, as in PowerShell.
        assert_eq!(
            powershell_literal_value("'a\u{2019} rest"),
            Some(("a".to_string(), " rest"))
        );
    }

    /// The installer recognizes its own PowerShell command for a runner path
    /// with a typographic quote (on Windows both Codex commands use it), so a
    /// second run repairs it in place instead of adding a second entry.
    #[test]
    fn hooks_for_a_path_with_a_typographic_quote_are_written_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("o\u{2019}brien");
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        for harness in ["github-cli", "codex"] {
            for _ in 0..2 {
                let _ = auto_install_detected_plugins(
                    &root,
                    ConfigNamespace::Set("TestProj"),
                    Some(harness),
                );
            }
        }
        let copilot = read_copilot_hooks(&root);
        for event in copilot_hook_events(GITHUB_CLI_INDEX_JS) {
            assert_eq!(
                copilot["hooks"][event].as_array().unwrap().len(),
                1,
                "{copilot}"
            );
        }
        let codex = read_codex_hooks(&root);
        for (event, _) in CODEX_HOOK_EVENTS {
            let ours = CODEX_HOOK_EVENTS
                .iter()
                .filter(|(e, _)| *e == event)
                .count();
            assert_eq!(
                codex["hooks"][event].as_array().unwrap().len(),
                ours,
                "{codex}"
            );
        }
    }

    /// Codex fires PreToolUse for MCP tools as `mcp__<server>__<tool>` (0.170
    /// `McpHandler::hook_tool_name_for`), so a second PreToolUse entry runs the
    /// runner on NeuroStrata's task_claim and get_snapshot. An older install
    /// (file tools only) gets it added once; a matcher the user changed still
    /// stands for the file-tool entry; a second run writes nothing.
    #[test]
    fn codex_pre_tool_use_covers_neurostrata_mcp_calls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let report = auto_install_detected_plugins(
            tmp.path(),
            ConfigNamespace::Set("TestProj"),
            Some("codex"),
        );
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("runner")
            .to_string();
        let (cmd, cmd_windows) = codex_hook_commands(&runner);
        let ours = json!({"type": "command", "command": cmd, "commandWindows": cmd_windows});
        let matcher = regex::Regex::new(SESSION_SCOPE_MATCHER).unwrap();
        for name in [
            "mcp__neurostrata__neurostrata_task_claim",
            "mcp__neurostrata__neurostrata_get_snapshot",
        ] {
            assert!(matcher.is_match(name), "{name}");
        }
        assert!(!matcher.is_match("mcp__neurostrata__neurostrata_search_memory"));
        for user_matcher in ["apply_patch|Bash", "apply_patch|Bash|Read"] {
            let path = tmp.path().join(".codex/hooks.json");
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&json!({"hooks": {
                    "PreToolUse": [{"matcher": user_matcher, "hooks": [ours.clone()]}]
                }}))
                .unwrap(),
            )
            .unwrap();
            for _ in 0..2 {
                let _ = auto_install_detected_plugins(
                    tmp.path(),
                    ConfigNamespace::Set("TestProj"),
                    Some("codex"),
                );
            }
            let doc = read_codex_hooks(tmp.path());
            let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
            assert_eq!(pre.len(), 2, "{user_matcher}: {doc}");
            assert_eq!(pre[0]["matcher"], json!(user_matcher));
            assert_eq!(pre[1]["matcher"], json!(SESSION_SCOPE_MATCHER));
            assert_eq!(pre[1]["hooks"], json!([ours.clone()]));
        }
    }

    /// Copilot rejects the whole hooks file when any event list is not an
    /// array, and Codex parses its file as a whole: the installer leaves such
    /// a file as it is and reports no hook wired.
    #[test]
    fn hooks_file_with_a_non_array_event_list_is_left_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".github/hooks")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let copilot = r#"{"version": 1, "hooks": {"postToolUse": {"type": "command"}}}"#;
        let codex = r#"{"hooks": {"PostToolUse": "echo user"}}"#;
        std::fs::write(tmp.path().join(COPILOT_HOOKS_REL), copilot).unwrap();
        std::fs::write(tmp.path().join(".codex/hooks.json"), codex).unwrap();
        for (harness, rel, text, event) in [
            ("github-cli", COPILOT_HOOKS_REL, copilot, "postToolUse"),
            ("codex", ".codex/hooks.json", codex, "PostToolUse"),
        ] {
            let report = auto_install_detected_plugins(
                tmp.path(),
                ConfigNamespace::Set("TestProj"),
                Some(harness),
            );
            assert_eq!(
                std::fs::read_to_string(tmp.path().join(rel)).unwrap(),
                text,
                "{harness}"
            );
            assert!(
                report["installed_plugins"][0]["config_wired"].is_null(),
                "{harness}: {report}"
            );
            let warned = report["warnings"].as_array().unwrap().iter().any(|w| {
                w.as_str()
                    .is_some_and(|s| s.contains(&format!("non-array {event} hook list")))
            });
            assert!(warned, "{harness}: {report}");
        }
    }

    /// A linked worktree's `.git` is a file: the exclude entry goes into the
    /// common git directory's `info/exclude`, which git reads for every
    /// worktree.
    #[test]
    fn linked_worktree_hooks_file_is_excluded_in_the_common_git_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("main");
        let gitdir = main.join(".git/worktrees/wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(wt.join(".github")).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", gitdir.to_string_lossy().replace('\\', "/")),
        )
        .unwrap();
        let _ = auto_install_detected_plugins(
            &wt,
            ConfigNamespace::Set("TestProj"),
            Some("github-cli"),
        );
        assert!(wt.join(COPILOT_HOOKS_REL).exists());
        let exclude = std::fs::read_to_string(main.join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude.matches("/.github/hooks/neurostrata.json").count(),
            1,
            "{exclude}"
        );
        assert!(!gitdir.join("info/exclude").exists());
    }

    /// A folder that is not a git checkout gets its hooks files but no
    /// `.git`: a stray `.git/info` would make it look like a repository root.
    #[test]
    fn non_git_folder_gets_no_git_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".github")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        for harness in ["github-cli", "codex"] {
            let _ = auto_install_detected_plugins(
                tmp.path(),
                ConfigNamespace::Set("TestProj"),
                Some(harness),
            );
        }
        assert!(tmp.path().join(COPILOT_HOOKS_REL).exists());
        assert!(tmp.path().join(".codex/hooks.json").exists());
        assert!(!tmp.path().join(".git").exists());
    }

    /// #368 F7: Codex detected only through the user's home install still
    /// gets hooks wired on a fresh clone without a project `.codex` folder.
    #[test]
    fn codex_detected_in_home_wires_a_fresh_clone() {
        let _env = no_namespace_env();
        let old_home = std::env::var_os("HOME");
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::env::set_var("HOME", &home);
        let project = tmp.path().join("fresh-clone");
        std::fs::create_dir_all(&project).unwrap();
        let report =
            auto_install_detected_plugins(&project, ConfigNamespace::Set("TestProj"), None);
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert!(project.join(".codex/hooks.json").exists(), "{report}");
        let codex = report["installed_plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["harness"] == "codex")
            .expect("codex installed");
        assert!(!codex["config_wired"].is_null());
    }

    /// #378 F1: the gate's namespace list keeps marker-first resolution but
    /// also covers every harness config and the ledger; case-only spellings
    /// are one namespace.
    #[test]
    fn gate_namespaces_unions_marker_configs_and_ledger() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("migrated");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        marker_at(&dir, "New");
        write_file(
            &dir.join(".codex/plugins/neurostrata.json"),
            r#"{"namespace":"Legacy"}"#,
        );
        write_file(
            &dir.join(".NeuroStrata/session_queries.json"),
            r#"{"namespace":"Ledger"}"#,
        );
        assert_eq!(
            gate_namespaces(&dir).expect("namespaces"),
            vec!["New", "Legacy", "Ledger"]
        );
        write_file(
            &dir.join(".hermes/plugins/neurostrata.json"),
            r#"{"namespace":"legacy"}"#,
        );
        assert_eq!(
            gate_namespaces(&dir).expect("namespaces"),
            vec!["New", "Legacy", "Ledger"]
        );
    }

    /// #378 F6: only the installer's own place may carry a baked namespace;
    /// an arbitrary `cli-hook.mjs` beside it is someone else's hook.
    #[test]
    fn an_assigned_hook_only_claims_the_installers_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let runner = plain_abs_path(&installed_runner(&root, "claude"));
        let parts = |cmd: &str| installer_hook_parts(cmd, &runner, &root);
        assert!(parts(&format!("NEUROSTRATA_NAMESPACE=x node \"{runner}\"")).is_some());
        assert!(parts(&format!(
            "NEUROSTRATA_NAMESPACE=x node \"{}\"",
            root.join("tools/cli-hook.mjs").display()
        ))
        .is_none());
        assert!(parts(
            "NEUROSTRATA_NAMESPACE=x node \"C:/gone/.NeuroStrata/plugins/claude/cli-hook.mjs\""
        )
        .is_some());
    }

    /// #381 F3: skill scaffolding never writes through a symlinked target.
    #[cfg(unix)]
    #[test]
    fn skill_writes_refuse_a_symlinked_target() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = dunce::canonicalize(tmp.path()).unwrap().join("project");
        std::fs::create_dir_all(project.join(".agents/skills/neurostrata")).unwrap();
        let victim = tmp.path().join("victim.md");
        std::fs::write(&victim, "do not touch").unwrap();
        let skill = project.join(".agents/skills/neurostrata/SKILL.md");
        symlink(&victim, &skill).unwrap();
        let report = auto_install_detected_plugins(
            &project,
            ConfigNamespace::Set("TestProj"),
            Some("claude"),
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "{report}"
        );
        assert!(std::fs::symlink_metadata(&skill)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    mod working_set_gc {
        use super::*;
        use std::collections::BTreeSet;

        fn project() -> (tempfile::TempDir, PathBuf) {
            let tmp = tempfile::tempdir().expect("tempdir");
            let root = dunce::canonicalize(tmp.path()).unwrap().join("project");
            std::fs::create_dir_all(root.join(".git/info")).unwrap();
            (tmp, root)
        }

        fn installed_runner_for(report: &Value, harness: &str) -> String {
            report["installed_plugins"]
                .as_array()
                .expect("installed array")
                .iter()
                .find(|entry| entry["harness"] == harness)
                .expect("harness installed")["cli_hook_runner"]
                .as_str()
                .expect("runner path")
                .to_string()
        }

        fn event_keys(doc: &Value) -> BTreeSet<String> {
            doc["hooks"]
                .as_object()
                .expect("hooks object")
                .keys()
                .cloned()
                .collect()
        }

        fn command_count(doc: &Value, event: &str, command: &str) -> usize {
            doc["hooks"][event]
                .as_array()
                .expect("event array")
                .iter()
                .flat_map(|entry| entry["hooks"].as_array().into_iter().flatten())
                .filter(|hook| {
                    hook["command"].as_str() == Some(command)
                        || hook["bash"].as_str() == Some(command)
                })
                .count()
        }

        #[test]
        fn claude_post_tool_use_is_added_once_and_preserves_foreign_hooks() {
            let (_tmp, root) = project();
            std::fs::create_dir_all(root.join(".claude")).unwrap();
            let foreign = json!({
                "matcher": "Read",
                "hooks": [{ "type": "command", "command": "echo foreign-post" }]
            });
            write_file(
                &root.join(".claude/settings.local.json"),
                &serde_json::to_string_pretty(&json!({
                    "hooks": { "PostToolUse": [foreign.clone()] },
                    "permissions": { "allow": ["Read"] }
                }))
                .unwrap(),
            );

            let report = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("claude"),
            );
            let runner = installed_runner_for(&report, "claude");
            let installed_runner =
                std::fs::read_to_string(root.join(".NeuroStrata/plugins/claude/cli-hook.mjs"))
                    .unwrap();
            assert!(installed_runner.contains("runWorkingSetToolCycle"));
            assert!(installed_runner.contains("const HARNESS = 'claude'"));
            let command = claude_hook_command(&runner).expect("safe runner");
            let path = root.join(".claude/settings.local.json");
            let first = std::fs::read_to_string(&path).unwrap();
            let doc: Value = serde_json::from_str(&first).unwrap();
            assert_eq!(
                event_keys(&doc),
                [
                    "PostToolUse",
                    "PreCompact",
                    "PreToolUse",
                    "SessionEnd",
                    "SessionStart",
                    "Stop"
                ]
                .into_iter()
                .map(str::to_string)
                .collect()
            );
            assert_eq!(doc["permissions"]["allow"], json!(["Read"]));
            let post = doc["hooks"]["PostToolUse"].as_array().unwrap();
            assert_eq!(post.len(), 2);
            assert_eq!(post[0], foreign);
            assert_eq!(post[1]["matcher"], json!(CLAUDE_PRE_TOOL_USE_MATCHER));
            assert_eq!(post[1]["hooks"][0]["command"], json!(command));
            assert_eq!(command_count(&doc, "PostToolUse", &command), 1);
            assert_eq!(
                report["installed_plugins"][0]["config_wired"],
                json!(path.to_string_lossy())
            );

            let _ = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("claude"),
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        }

        #[test]
        fn codex_context_and_result_events_are_wired_and_deduplicated() {
            let (_tmp, root) = project();
            std::fs::create_dir_all(root.join(".codex")).unwrap();
            let report = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("codex"),
            );
            let runner = installed_runner_for(&report, "codex");
            let installed_runner =
                std::fs::read_to_string(root.join(".NeuroStrata/plugins/codex/cli-hook.mjs"))
                    .unwrap();
            assert!(installed_runner.contains("runWorkingSetToolCycle"));
            assert!(installed_runner.contains("const HARNESS = 'codex'"));
            let (command, command_windows) = codex_hook_commands(&runner);
            let path = root.join(".codex/hooks.json");
            let first = std::fs::read_to_string(&path).unwrap();
            let doc: Value = serde_json::from_str(&first).unwrap();
            assert_eq!(
                event_keys(&doc),
                [
                    "PostToolUse",
                    "PreCompact",
                    "PreToolUse",
                    "SessionEnd",
                    "SessionStart",
                    "Stop",
                    "UserPromptSubmit",
                ]
                .into_iter()
                .map(str::to_string)
                .collect()
            );
            assert_eq!(
                doc["hooks"]["PreToolUse"][0]["matcher"],
                json!(CODEX_PRE_TOOL_USE_MATCHER)
            );
            for event in ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"] {
                let entries = doc["hooks"][event].as_array().unwrap();
                assert_eq!(entries.len(), 1, "{event}");
                assert!(entries[0].get("matcher").is_none(), "{event}");
                assert_eq!(entries[0]["hooks"][0]["command"], json!(command));
                assert_eq!(
                    entries[0]["hooks"][0]["commandWindows"],
                    json!(command_windows)
                );
            }
            let _ = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("codex"),
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

            let mut doc: Value = serde_json::from_str(&first).unwrap();
            let foreign_prompt = json!({
                "hooks": [{ "type": "command", "command": "echo foreign-prompt" }]
            });
            let foreign_result = json!({
                "hooks": [{ "type": "command", "command": "echo foreign-result" }]
            });
            doc["hooks"]["UserPromptSubmit"]
                .as_array_mut()
                .unwrap()
                .push(foreign_prompt.clone());
            doc["hooks"]["PostToolUse"]
                .as_array_mut()
                .unwrap()
                .push(foreign_result.clone());
            std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
            write_file(
                &root.join(".codex/config.toml"),
                &format!(
                    "[hooks.session_start]\ncommand = \"{}\"\n",
                    runner.replace('\\', "\\\\")
                ),
            );
            let _ = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("codex"),
            );
            let doc: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(doc["hooks"]["SessionStart"].as_array().unwrap().is_empty());
            assert_eq!(doc["hooks"]["UserPromptSubmit"][1], foreign_prompt);
            assert_eq!(doc["hooks"]["PostToolUse"][1], foreign_result);
            for event in ["UserPromptSubmit", "PostToolUse", "Stop"] {
                assert_eq!(command_count(&doc, event, &command), 1, "{event}");
            }
            // The file-tool and session-scope matchers each carry the
            // canonical command once.
            assert_eq!(command_count(&doc, "PreToolUse", &command), 2);
            let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
            assert_eq!(exclude.matches("/.codex/hooks.json").count(), 1);
        }

        #[test]
        fn github_cli_native_camel_case_hooks_are_written_atomically() {
            let (_tmp, root) = project();
            let path = root.join(".github/hooks/neurostrata.json");
            let foreign = json!({
                "type": "command",
                "bash": "echo foreign-start",
                "powershell": "echo foreign-start",
                "cwd": ".",
                "timeoutSec": 10,
                "env": { "NEUROSTRATA_HOOK_EVENT": "sessionStart", "KEEP": "yes" }
            });
            write_file(
                &path,
                &serde_json::to_string_pretty(&json!({
                    "version": 1,
                    "hooks": { "sessionStart": [foreign.clone()] },
                    "other": true
                }))
                .unwrap(),
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            }
            let report = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("github-cli"),
            );
            let runner = installed_runner_for(&report, "github-cli");
            let installed_runner =
                std::fs::read_to_string(root.join(".NeuroStrata/plugins/github-cli/cli-hook.mjs"))
                    .unwrap();
            assert!(installed_runner.contains("runWorkingSetToolCycle"));
            assert!(installed_runner.contains("NEUROSTRATA_HOOK_EVENT"));
            let bash = format!("node '{}'", runner.replace('\'', r"'\''"));
            let powershell = format!("node '{}'", runner.replace('\'', "''"));
            let first = std::fs::read_to_string(&path).unwrap();
            let doc: Value = serde_json::from_str(&first).unwrap();
            assert_eq!(doc["version"], json!(1));
            assert_eq!(doc["other"], json!(true));
            // The shipped bundle gates preToolUse/postToolUse on its own
            // exports; sessionStart/sessionEnd/preCompact are always wired.
            assert_eq!(
                event_keys(&doc),
                ["preCompact", "sessionEnd", "sessionStart"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            );
            assert_eq!(doc["hooks"]["sessionStart"][0], foreign);
            for event in ["preCompact", "sessionEnd", "sessionStart"] {
                let entries = doc["hooks"][event].as_array().unwrap();
                let owned = entries
                    .iter()
                    .find(|entry| {
                        entry["bash"] == json!(bash) && entry["powershell"] == json!(powershell)
                    })
                    .expect("owned native hook");
                assert_eq!(
                    owned
                        .as_object()
                        .unwrap()
                        .keys()
                        .cloned()
                        .collect::<BTreeSet<_>>(),
                    ["bash", "env", "powershell", "type"]
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                );
                assert_eq!(owned["type"], json!("command"));
                assert_eq!(owned["bash"], json!(bash));
                assert_eq!(owned["powershell"], json!(powershell));
                assert_eq!(owned["env"], json!({ "NEUROSTRATA_HOOK_EVENT": event }));
            }
            assert_eq!(
                report["installed_plugins"][0]["config_wired"],
                json!(path.to_string_lossy())
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o640
                );
            }
            assert!(std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-")));

            let _ = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("github-cli"),
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        }

        #[test]
        fn working_set_gc_refresh_repairs_owned_paths_and_rejects_symlinks() {
            let (_tmp, root) = project();
            let report = auto_install_detected_plugins(
                &root,
                ConfigNamespace::Set("TestProj"),
                Some("github-cli"),
            );
            let runner = installed_runner_for(&report, "github-cli");
            let path = root.join(".github/hooks/neurostrata.json");
            let mut doc: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let stale = root.join("moved/.NeuroStrata/plugins/github-cli/cli-hook.mjs");
            let stale = stale.to_string_lossy();
            let old_bash = format!("node '{}'", stale.replace('\'', r"'\''"));
            let old_powershell = format!("node '{}'", stale.replace('\'', "''"));
            let foreign = json!({
                "type": "command",
                "bash": format!("node {}", shell_literal(&root.join("tools/cli-hook.mjs").to_string_lossy())),
                "powershell": format!("node {}", powershell_literal(&root.join("tools/cli-hook.mjs").to_string_lossy())),
                "cwd": ".",
                "timeoutSec": 10,
                "env": { "NEUROSTRATA_HOOK_EVENT": "sessionStart" }
            });
            doc["hooks"]["sessionStart"]
                .as_array_mut()
                .unwrap()
                .push(foreign.clone());
            for entry in doc["hooks"]["sessionStart"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .take(1)
            {
                entry["bash"] = json!(old_bash);
                entry["powershell"] = json!(old_powershell);
            }
            std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
            for _ in 0..2 {
                let _ = auto_install_detected_plugins(
                    &root,
                    ConfigNamespace::Set("TestProj"),
                    Some("github-cli"),
                );
            }
            let doc: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let bash = format!("node '{}'", runner.replace('\'', r"'\''"));
            let powershell = format!("node '{}'", runner.replace('\'', "''"));
            assert_eq!(doc["hooks"]["sessionStart"][0]["bash"], json!(bash));
            assert_eq!(
                doc["hooks"]["sessionStart"][0]["powershell"],
                json!(powershell)
            );
            assert_eq!(doc["hooks"]["sessionStart"][1], foreign);
            assert_eq!(doc["hooks"]["sessionStart"].as_array().unwrap().len(), 2);
            let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
            assert_eq!(
                exclude.matches("/.github/hooks/neurostrata.json").count(),
                1
            );

            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let (tmp, linked) = project();
                let outside = tmp.path().join("outside");
                std::fs::create_dir_all(&outside).unwrap();
                let victim = outside.join("neurostrata.json");
                std::fs::write(&victim, "do not touch").unwrap();
                symlink(&outside, linked.join(".github")).unwrap();
                let report = auto_install_detected_plugins(
                    &linked,
                    ConfigNamespace::Set("TestProj"),
                    Some("github-cli"),
                );
                assert_eq!(std::fs::read_to_string(&victim).unwrap(), "do not touch");
                assert!(report["installed_plugins"][0]["config_wired"].is_null());
                assert!(report["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| {
                        warning
                            .as_str()
                            .is_some_and(|text| text.contains("symlink"))
                    }));
            }
        }
    }
}
