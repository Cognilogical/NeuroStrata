//! Zero-touch CLI/terminal agent harness detection and automatic plugin
//! installation during init (`neurostrata_bootstrap`, `neurostrata_task_setup`,
//! and `neurostrata_get_snapshot`).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

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

// @neurostrata/plugin-core, bundled once so the harness adapters' relative
// `../../core/dist/index.js` imports resolve on disk under
// `.NeuroStrata/plugins/core/dist/index.js`.
const CORE_INDEX_JS: &str = include_str!("../../plugins/core/dist/index.js");
const CORE_PKG_JSON: &str = include_str!("../../plugins/core/package.json");

const CLI_HOOK_RUNNER_MJS: &str = r#"#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { basename, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import plugin, * as mod from "./dist/index.js";

// The installer writes this runner to <root>/.NeuroStrata/plugins/<harness>/;
// the harness config of <root> is read first.
const RUNNER_DIR = dirname(fileURLToPath(import.meta.url));
const HARNESS = basename(RUNNER_DIR);

async function main() {
  // Redirect all diagnostic logging to stderr so stdout carries only the
  // JSON hook response (critical when this runner is invoked by a CLI hook
  // whose stdout is parsed as structured output).
  console.log = console.error;
  console.info = console.error;
  console.warn = console.error;

  // A large hook response (e.g. a big `updatedInput`) can still be buffered in
  // stdout when `process.exit(0)` runs, so the host reads a truncated JSON
  // body. Awaiting the write callback guarantees the payload is flushed first.
  const writeStdout = (text) =>
    new Promise((resolve) => {
      process.stdout.write(text, () => resolve());
    });

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
  // The namespace comes from the harness config the installer wrote; the
  // plugin reads NEUROSTRATA_NAMESPACE itself, after the project marker and
  // this config (the order of `neurostrata-mcp namespace resolve`).
  const projectDir =
    typeof mod.resolveHookProjectDir === "function"
      ? mod.resolveHookProjectDir(HARNESS, event)
      : typeof event.cwd === "string" && event.cwd
        ? event.cwd
        : process.cwd();
  const config =
    typeof mod.readHarnessConfig === "function"
      ? mod.readHarnessConfig(HARNESS, projectDir, RUNNER_DIR)
      : {};
  const init = mod.initialize ?? plugin?.initialize ?? plugin?.setup;
  if (typeof init === "function") {
    await init({ config, projectDir });
  }
  const handler =
    mod.handleHookEvent ??
    plugin?.handleHookEvent ??
    mod.handleClaudeHookEvent ??
    plugin?.handleClaudeHookEvent;
  if (typeof handler === "function") {
    const out = await handler(event);
    await writeStdout(JSON.stringify(out));
    process.exit(0);
  }
  if (event.hook_event_name === "Stop" && typeof event.transcript === "string") {
    const turnEnd = mod.onTurnEnd ?? plugin?.onTurnEnd;
    if (typeof turnEnd === "function") {
      await turnEnd({ role: "assistant", content: event.transcript });
    }
  }
  await writeStdout(JSON.stringify({ continue: true }));
}
main().catch((err) => {
  process.stderr.write(String(err) + "\n");
  process.exit(2);
});
"#;

const EMBEDDED_NEUROSTRATA_SKILL_MD: &str =
    include_str!("../../.agents/skills/neurostrata/SKILL.md");
const EMBEDDED_NEUROSTRATA_TASK_AGENT_MD: &str =
    include_str!("../../.agents/agents/NeuroStrata-Task.md");
const SUBAGENT_RULE_MARKER: &str = "subagent_prompt_injection";
const SUBAGENT_GRAPH_FIRST_RULE_SNIPPET: &str = "\n- **NEUROSTRATA GRAPH-FIRST & SUBAGENT CONTEXT RULE:** Honor the `### NeuroStrata Task & Graph Context` (`subagent_prompt_injection`) block in your prompt — every `[rule]` entry is a binding architectural constraint. Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed under `### NeuroStrata Task & Graph Context` in your prompt. For any file you touch that is NOT listed in that block, call `neurostrata_memories_for_file` first (or `neurostrata_related` / `neurostrata_search_memory` with `mode`: `hybrid` (default) + `include_references`: true / `neurostrata_sparql_query`). Never leave `TODO`, `FIXME`, `stub`, or `phase-2` deferrals.\n";

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
    // left alone.
    matches!(candidate.try_exists(), Ok(false))
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
    if !unchanged && std::fs::write(&path, &text).is_err() {
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
    let (raw_ns, rest, plain) = match command.strip_prefix("NEUROSTRATA_NAMESPACE=") {
        Some(assigned) => {
            let (ns, rest) = assigned.split_once(' ')?;
            (ns, rest, false)
        }
        None => ("", command, true),
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
    // A plain `node "<...>/cli-hook.mjs"` is anyone's command; only one that
    // runs the installer's runner, or a missing runner at the installer's
    // place (`.NeuroStrata/plugins/claude/cli-hook.mjs` of a moved project),
    // is ours to repair.
    if plain && !at_installer_runner_place(old_path, project_root, abs_runner) {
        return None;
    }
    if !runner_is_replaceable(old_path, project_root, abs_runner) {
        return None;
    }
    // An empty assignment (the daemon's old startup install) named nothing.
    let namespace = if raw_ns.is_empty() {
        None
    } else {
        Some(usable_namespace(raw_ns)?)
    };
    Some((namespace, format!("node \"{abs_runner}\"{suffix}")))
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
/// the session's project folder as `worktree`.
const CLAUDE_SESSION_SCOPE_MATCHER: &str = "mcp__.*__neurostrata_(task_claim|get_snapshot)";

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
    let Some(entries) = hooks.entry(event).or_insert_with(|| json!([])).as_array_mut() else {
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
const CLAUDE_HOOK_EVENT_ORDER: [&str; 3] = ["SessionStart", "PreToolUse", "Stop"];

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
        write!(f, "{} could not be read: {}", self.path.display(), self.reason)
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
    ("opencode", ".opencode/neurostrata-plugin.json", "/neurostrata/namespace"),
    ("codex", ".codex/plugins/neurostrata.json", "/namespace"),
    ("hermes", ".hermes/plugins/neurostrata.json", "/namespace"),
    ("openclaw", ".openclaw/plugins/neurostrata.json", "/namespace"),
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
    let unreadable = |reason: String| UnreadableConfig { path: path.to_path_buf(), reason };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        return Err(unreadable(
            "it starts with a UTF-8 byte order mark; save it as UTF-8 without one".to_string(),
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| unreadable("it is not UTF-8".to_string()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| unreadable(format!("it is not valid JSON ({e})")))
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
    runner_is_replaceable(runner, root, abs_runner).then_some(ns)
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
        .flat_map(|entry| entry.get("hooks").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
        .find_map(|command| installer_hook_namespace(command, root, &abs_runner))
}

/// The usable namespace `harness`'s config under `root` names: `Ok(None)` when
/// the config is missing or names none, an error when it exists but cannot be
/// read or parsed.
fn harness_config_namespace(root: &Path, harness: &str) -> Result<Option<String>, UnreadableConfig> {
    if harness == "claude" {
        // Claude's plugin config first; older installs kept the namespace in
        // the settings (the `env` block or the hook command).
        if let Some(ns) = claude_config_namespace_checked(root)? {
            return Ok(Some(ns));
        }
        return Ok(read_config(&root.join(CLAUDE_SETTINGS))?
            .and_then(|settings| claude_settings_namespace(root, &settings)));
    }
    let Some((_, rel, pointer)) = PLUGIN_NAMESPACE_CONFIGS.iter().find(|(h, _, _)| *h == harness)
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
        Ok(NamespaceRoots { dir, own_root, main_root, outside_git })
    }

    /// Harness settings are untracked, so a linked worktree usually has none
    /// of its own: its own checkout first, then the main checkout.
    fn config_order(&self) -> Vec<&PathBuf> {
        let mut order: Vec<&PathBuf> = Vec::new();
        for root in self.own_root.iter().chain(self.main_root.iter()).chain(self.outside_git.iter()) {
            if !order.contains(&root) {
                order.push(root);
            }
        }
        order
    }

    /// The ledger: the main checkout's first, then `dir`'s own.
    fn ledger_order(&self) -> impl Iterator<Item = &PathBuf> {
        self.main_root.iter().chain(self.own_root.iter()).chain(self.outside_git.iter())
    }

    /// The harness configs of the first place in [`Self::config_order`] that
    /// has any. A config that could not be read in a place looked at fails
    /// the lookup: the namespace it names might be the answer.
    fn harness_namespaces(&self) -> Result<Vec<(String, String)>, Vec<UnreadableConfig>> {
        let mut unreadable = Vec::new();
        for root in self.config_order() {
            let (found, errors) = harness_namespaces_at(root);
            unreadable.extend(errors);
            if !found.is_empty() {
                return if unreadable.is_empty() { Ok(found) } else { Err(unreadable) };
            }
        }
        if unreadable.is_empty() {
            Ok(Vec::new())
        } else {
            Err(unreadable)
        }
    }
}

/// The namespace for `dir` and where it came from, in this order:
/// 0. the project root marker (`.NeuroStrata/config.json`), found as
///    [`crate::root_marker::find_root_marker`] does;
/// 1. a harness config (the first in the fixed order), read from the first
///    of `dir`'s checkout, its main checkout, or (outside git) `dir` itself
///    that has any;
/// 2. `env_namespace` (`NEUROSTRATA_NAMESPACE`), when usable;
/// 3. the `namespace` field of the graph-lookup ledger
///    (`.NeuroStrata/session_queries.json`), the main checkout's then `dir`'s;
/// 4. the main checkout's folder name; outside git, `dir`'s folder name, when
///    it is a usable namespace.
///
/// A marker or harness config that exists but cannot be read is an error,
/// never skipped: the namespace it names is unknown.
fn resolve_namespace_from(
    dir: &Path,
    env_namespace: Option<&str>,
) -> Result<ResolvedNamespace, ResolveError> {
    let roots = NamespaceRoots::of(dir)?;
    let marker = crate::root_marker::find_root_marker(&roots.dir)
        .map_err(|(path, reason)| ResolveError::UnreadableMarker(UnreadableConfig { path, reason }))?;
    if let Some(found) = marker {
        // The marker decides alone: a harness config that cannot be read
        // does not change the answer, and is not listed.
        return Ok(ResolvedNamespace {
            namespace: found.config.namespace,
            source: NamespaceSource::Marker,
            configs: roots.harness_namespaces().unwrap_or_default(),
        });
    }
    let configs = roots.harness_namespaces().map_err(ResolveError::UnreadableConfigs)?;
    let resolved = |namespace: String, source: NamespaceSource| ResolvedNamespace {
        namespace,
        source,
        configs: configs.clone(),
    };
    if let Some((_, ns)) = configs.first() {
        return Ok(resolved(ns.clone(), NamespaceSource::HarnessConfig));
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
        let text = std::fs::read_to_string(root.join(".NeuroStrata").join("session_queries.json")).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        usable_namespace(v.get("namespace")?.as_str()?)
    }) {
        add(ns);
    }
    match candidates.len() {
        1 if !marker_unreadable => Ok(candidates.remove(0)),
        0 => Err(resolve_namespace_from(dir, None).map(|r| r.namespace).into_iter().collect()),
        _ => Err(candidates),
    }
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
    /// none gets this one when given, else is left unwritten (plugin files
    /// only). A config that cannot be read is left as it is.
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
                Ok(_) => listed.iter().find(|(h, _)| h == harness).map(|(_, ns)| ns.clone()),
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
            return harnesses.map(|h| (h.to_string(), marker.config.namespace.clone())).collect();
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
        write_if_changed(&runner_path, CLI_HOOK_RUNNER_MJS);
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
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let needs_write = std::fs::read_to_string(path)
            .map(|existing| existing != content)
            .unwrap_or(true);
        if needs_write && std::fs::write(path, content).is_err() {
            return false;
        }
        true
    };

    // 1. Always scaffold project-local .agents/skills/neurostrata/SKILL.md and .agents/agents/NeuroStrata-Task.md
    let proj_skill = project_root.join(".agents/skills/neurostrata/SKILL.md");
    if write_file_if_changed(&proj_skill, EMBEDDED_NEUROSTRATA_SKILL_MD) {
        skills_installed.push(proj_skill.to_string_lossy().to_string());
    }
    let proj_task_agent = project_root.join(".agents/agents/NeuroStrata-Task.md");
    let _ = write_file_if_changed(&proj_task_agent, EMBEDDED_NEUROSTRATA_TASK_AGENT_MD);

    // 2. Sync harness-specific skill & agent directories (project-level and machine-level)
    if active == Some("opencode")
        || project_root.join(".opencode").exists()
        || project_root.join("opencode.json").exists()
    {
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
                if std::fs::write(&path, updated).is_ok() {
                    patched_agents.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    (skills_installed, patched_agents)
}

/// Write `contents` to `path` through a temporary file in the same folder and
/// a rename, so a reader (a git hook running `namespace resolve`) sees the old
/// file or the new one, never a half-written one.
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn wire_harness_config(
    project_root: &Path,
    namespace: ConfigNamespace<'_>,
    harness: &str,
    is_active: bool,
    abs_dist: &str,
    abs_runner: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let write_json_if_changed = |path: &Path, val: &Value| -> Option<String> {
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
    // the one it gets, or none (the config is then left unwritten).
    let namespace = namespace.for_harness(project_root, harness);

    match harness {
        "opencode" => {
            if is_active
                || project_root.join(".opencode").exists()
                || project_root.join("opencode.json").exists()
            {
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
                if let Some(namespace) = namespace.as_deref() {
                    let cfg_path = project_root.join(".opencode/neurostrata-plugin.json");
                    let doc = json!({
                        "plugin": [abs_dist],
                        "neurostrata": {
                            "namespace": namespace,
                            "pluginPath": abs_dist
                        }
                    });
                    let _ = write_json_if_changed(&cfg_path, &doc);
                }

                return Some(loader_path.to_string_lossy().to_string());
            }
            None
        }
        "claude" => {
            if is_active || project_root.join(".claude").exists() {
                let namespace = namespace.as_deref()?;
                let cfg_path = project_root.join(".claude/settings.local.json");
                // The namespace lives in Claude's plugin config, as it does
                // for the other harnesses; the command carries none.
                let cmd = format!("node \"{abs_runner}\"");
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
                    if let Some(obj) = doc.as_object_mut() {
                        let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
                        if let Some(h_obj) = hooks.as_object_mut() {
                            let may_add = repair_claude_hooks(
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
                                    |m| m != Some(CLAUDE_SESSION_SCOPE_MATCHER),
                                    &cmd,
                                    &runs,
                                );
                                // task_claim and get_snapshot get the session's
                                // folder as `worktree` (claim scope from a linked
                                // worktree).
                                ensure_claude_hook(
                                    h_obj,
                                    "PreToolUse",
                                    Some(CLAUDE_SESSION_SCOPE_MATCHER),
                                    |m| m == Some(CLAUDE_SESSION_SCOPE_MATCHER),
                                    &cmd,
                                    &runs,
                                );
                                // SessionStart lets the runner record the session's
                                // folder when Claude Code gives no CLAUDE_PROJECT_DIR.
                                ensure_claude_hook(h_obj, "SessionStart", None, |_| true, &cmd, &runs);
                                ensure_claude_hook(h_obj, "Stop", None, |_| true, &cmd, &runs);
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
                let namespace = namespace.as_deref()?;
                let cfg_path = project_root.join(".hermes/plugins/neurostrata.json");
                let doc = json!({
                    "id": "neurostrata-hermes",
                    "namespace": namespace,
                    "entry": abs_dist,
                    "cliHookRunner": abs_runner
                });
                return write_json_if_changed(&cfg_path, &doc);
            }
            None
        }
        "codex" => {
            if is_active || project_root.join(".codex").exists() {
                let namespace = namespace.as_deref()?;
                let cfg_path = project_root.join(".codex/plugins/neurostrata.json");
                let doc = json!({
                    "id": "neurostrata-codex",
                    "namespace": namespace,
                    "entry": abs_dist,
                    "cliHookRunner": abs_runner
                });
                return write_json_if_changed(&cfg_path, &doc);
            }
            None
        }
        "openclaw" => {
            if is_active || project_root.join(".openclaw").exists() {
                let namespace = namespace.as_deref()?;
                let cfg_path = project_root.join(".openclaw/plugins/neurostrata.json");
                let doc = json!({
                    "id": "neurostrata-openclaw",
                    "namespace": namespace,
                    "entry": abs_dist
                });
                return write_json_if_changed(&cfg_path, &doc);
            }
            None
        }
        "github-cli" => {
            if is_active || project_root.join(".github").exists() {
                let namespace = namespace.as_deref()?;
                let cfg_path =
                    project_root.join(".NeuroStrata/plugins/github-cli/copilot-extension.json");
                let doc = json!({
                    "id": "neurostrata-github-cli",
                    "namespace": namespace,
                    "entry": abs_dist,
                    "cliHookRunner": abs_runner
                });
                return write_json_if_changed(&cfg_path, &doc);
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
        harness_namespaces_at(root).0.into_iter().next().map(|(_, ns)| ns)
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
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
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
        assert_eq!(
            src_of(dir),
            ("beta".into(), NamespaceSource::Env)
        );
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        assert_eq!(
            src_of(dir),
            ("from-ledger".into(), NamespaceSource::Ledger)
        );
        // The daemon's hook refresh never reads its own environment, and a
        // ledger answer is never written into harness configs.
        std::env::set_var("NEUROSTRATA_NAMESPACE", "beta");
        assert_eq!(namespace_for_refresh(dir, false), Ok(None));
        assert_eq!(namespace_for_refresh(dir, true), Ok(Some("beta".to_string())));
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
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
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
        assert_eq!(src_of(&dir), ("from-marker".into(), NamespaceSource::Marker));
        // The daemon side honors the marker too.
        assert_eq!(namespace_for_refresh(&dir, false), Ok(Some("from-marker".to_string())));
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
        assert!(boot_namespaces_for(&wt).iter().all(|(_, ns)| ns == "from-main"));
        assert!(!boot_namespaces_for(&wt).is_empty());
    }

    #[test]
    fn explicit_namespace_rejects_folder_name_only() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        assert_eq!(explicit_namespace(&dir), Err(vec!["example-project".to_string()]));
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
            Err(vec!["alpha".to_string(), "beta".to_string(), "from-ledger".to_string()])
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
        let err = resolve_namespace(&dir).expect_err("unreadable marker must fail").to_string();
        assert!(err.contains("config.json"), "names the file: {err}");
        // A refresh writes no namespace it cannot know.
        assert!(namespace_for_refresh(&dir, true).is_err());
        marker_at(&dir, "fixed");
        assert_eq!(ns_of(&dir), "fixed");
    }

    #[test]
    fn boot_install_keeps_existing_plugin_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let cfg = dir.join(".hermes/plugins/neurostrata.json");
        write_file(&cfg, r#"{"id": "neurostrata-hermes", "namespace": "example-project"}"#);
        boot_install_plugins(&dir);
        assert_eq!(namespace_in(&cfg), "example-project");
    }

    #[test]
    fn boot_install_without_any_config_writes_no_namespace_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".hermes")).unwrap();
        assert!(boot_namespaces_for(&dir).is_empty());
        boot_install_plugins(&dir);
        assert!(!dir.join(".hermes/plugins/neurostrata.json").exists());
        assert!(dir.join(".NeuroStrata/plugins/hermes/dist/index.js").exists());
    }

    fn namespace_in(cfg: &Path) -> Value {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap();
        doc["namespace"].clone()
    }

    /// Configs that disagree each keep their own namespace at boot, and a
    /// harness with no config gets plugin files only, not a config.
    #[test]
    fn boot_install_keeps_each_harness_namespace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let codex = dir.join(".codex/plugins/neurostrata.json");
        let hermes = dir.join(".hermes/plugins/neurostrata.json");
        write_file(&codex, r#"{"id": "neurostrata-codex", "namespace": "alpha"}"#);
        write_file(&hermes, r#"{"id": "neurostrata-hermes", "namespace": "beta"}"#);
        std::fs::create_dir_all(dir.join(".openclaw")).unwrap();
        boot_install_plugins(&dir);
        assert_eq!(namespace_in(&codex), "alpha");
        assert_eq!(namespace_in(&hermes), "beta");
        assert!(!dir.join(".openclaw/plugins/neurostrata.json").exists());
        assert!(dir.join(".NeuroStrata/plugins/openclaw/dist/index.js").exists());
    }

    /// A folder below the checkout top with its own harness config keeps
    /// that config's namespace; the checkout top's config is not copied in.
    #[test]
    fn boot_install_in_subfolder_reads_the_folder_itself_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let top = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(top.join(".git")).unwrap();
        let top_cfg = top.join(".hermes/plugins/neurostrata.json");
        write_file(&top_cfg, r#"{"id": "neurostrata-hermes", "namespace": "outer"}"#);
        let sub = top.join("packages").join("api");
        let sub_cfg = sub.join(".hermes/plugins/neurostrata.json");
        write_file(&sub_cfg, r#"{"id": "neurostrata-hermes", "namespace": "api"}"#);
        boot_install_plugins(&sub);
        assert_eq!(namespace_in(&sub_cfg), "api");
        assert_eq!(namespace_in(&top_cfg), "outer");
    }

    /// A configured project is stored as its `.NeuroStrata` folder; the boot
    /// refresh installs into the project folder that holds it, never into
    /// `<project>/.NeuroStrata/.NeuroStrata`.
    #[test]
    fn boot_install_for_configured_project_uses_the_project_folder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".NeuroStrata")).unwrap();
        let hermes = dir.join(".hermes/plugins/neurostrata.json");
        write_file(&hermes, r#"{"id": "neurostrata-hermes", "namespace": "example-project"}"#);
        boot_install_project_context(&dir.join(".NeuroStrata"));
        assert!(dir.join(".NeuroStrata/plugins/hermes/dist/index.js").exists());
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
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
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
            vec![("claude".to_string(), "a".to_string()), ("codex".to_string(), "b".to_string())]
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
        assert_eq!(configured_namespace(&main).as_deref(), Some("example-project"));
        assert_eq!(ns_of(&main.join("src")), "example-project");

        // A linked worktree beside it, with no settings of its own, inherits it.
        // Its gitdir names the shared `.git` in `commondir`, as git writes it.
        let wt_gitdir = main.join(".git").join("worktrees").join("wt");
        std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
        let wt = base.join("platform-wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git").join("worktrees").join("wt").display()),
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
        assert_eq!(ns(&ours("example-project")).as_deref(), Some("example-project"));
        // A moved checkout's runner that no longer exists is still ours.
        assert_eq!(
            ns("NEUROSTRATA_NAMESPACE=moved node \"C:/gone/.NeuroStrata/plugins/claude/cli-hook.mjs\"")
                .as_deref(),
            Some("moved")
        );
        // Someone else's script, or a runner that exists elsewhere, is not.
        assert_eq!(ns("NEUROSTRATA_NAMESPACE=other node \"C:/x/notify.js\""), None);
        let elsewhere = root.join("elsewhere").join("cli-hook.mjs");
        write_file(&elsewhere, "// someone's own install");
        assert_eq!(
            ns(&format!("NEUROSTRATA_NAMESPACE=other node \"{}\"", elsewhere.display())),
            None
        );
        // Other shapes are not the installer's.
        assert_eq!(ns(&format!("set NEUROSTRATA_NAMESPACE=a&& node \"{runner}\"")), None);
        assert_eq!(ns(&format!("env NEUROSTRATA_NAMESPACE=a node \"{runner}\"")), None);
        assert_eq!(ns(&format!("NEUROSTRATA_NAMESPACE=\"a\" node \"{runner}\"")), None);
        assert_eq!(ns(&format!("NEUROSTRATA_NAMESPACE=My Project node \"{runner}\"")), None);
        assert_eq!(ns(&format!("NEUROSTRATA_NAMESPACE= node \"{runner}\"")), None);
        assert_eq!(ns(&format!("NEUROSTRATA_NAMESPACE=$NS node \"{runner}\"")), None);
        assert_eq!(ns(&format!("{} --flag", ours("a"))), None);
        assert_eq!(ns(&format!("MY_{}", ours("a"))), None);
    }

    /// A custom Claude hook that sets the variable for its own script does not
    /// shadow the namespace in the installer's hook.
    #[test]
    fn a_custom_claude_hook_does_not_shadow_the_installers() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
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
        assert_eq!(src_of(&dir), ("alpha".to_string(), NamespaceSource::HarnessConfig));
    }

    /// A harness config that exists but cannot be read (invalid JSON, a UTF-8
    /// byte order mark) fails the lookup and names the file, even when another
    /// config names a namespace: the unreadable one might name another.
    #[test]
    fn an_unreadable_harness_config_fails_the_lookup() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
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
        assert_eq!(usable_env_namespace(" \"alpha\" ").as_deref(), Some("alpha"));
        assert_eq!(usable_env_namespace("'alpha'").as_deref(), Some("alpha"));
        assert_eq!(usable_env_namespace("\"alpha'"), None);
        assert_eq!(usable_namespace("\"alpha\""), None);
        assert_eq!(usable_namespace(" alpha"), None);
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
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
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        (tmp, dir)
    }

    /// A refresh with no explicit namespace installs the plugin files and
    /// writes no namespace into any config.
    #[test]
    fn a_refresh_without_an_explicit_namespace_writes_plugin_files_only() {
        let (_tmp, dir) = refresh_project();
        let _ = auto_install_detected_plugins(&dir, ConfigNamespace::Keep(None), Some("claude"));
        assert!(installed_runner(&dir, "claude").is_file(), "plugin files are installed");
        assert!(!dir.join(CLAUDE_SETTINGS).exists(), "no Claude hooks without a namespace");
        assert!(!dir.join(".codex/plugins/neurostrata.json").exists());
        assert_eq!(harness_namespaces_at(&dir), (Vec::new(), Vec::new()));
    }

    /// A refresh keeps each config's own usable namespace and gives the
    /// explicit one only to configs that name none; an unreadable config is
    /// left byte-identical.
    #[test]
    fn a_refresh_keeps_each_configs_own_namespace() {
        let (_tmp, dir) = refresh_project();
        let codex = dir.join(".codex/plugins/neurostrata.json");
        write_file(&codex, r#"{"namespace": "beta"}"#);
        let _ =
            auto_install_detected_plugins(&dir, ConfigNamespace::Keep(Some("alpha")), Some("claude"));
        assert_eq!(
            harness_namespaces_at(&dir).0,
            vec![
                ("claude".to_string(), "alpha".to_string()),
                ("codex".to_string(), "beta".to_string())
            ]
        );
        let broken = "\u{feff}{\"namespace\": \"beta\"}";
        write_file(&codex, broken);
        let _ =
            auto_install_detected_plugins(&dir, ConfigNamespace::Keep(Some("alpha")), Some("claude"));
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
            assert!(!dir.join(".codex/plugins/neurostrata.json").exists(), "{bad:?}");
        }
    }

    #[test]
    fn a_namespace_starts_with_a_letter_or_digit() {
        assert_eq!(usable_namespace("example-project").as_deref(), Some("example-project"));
        assert_eq!(usable_namespace("v1.2_x").as_deref(), Some("v1.2_x"));
        for bad in ["", ".", "..", "--help", "-x", "_x", ".hidden", "a b", "$NS"] {
            assert_eq!(usable_namespace(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn outside_git_the_directory_itself_is_read() {
        let _env = no_namespace_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dunce::canonicalize(tmp.path()).unwrap().join("loose-folder");
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
        assert_eq!(resolve_namespace(Path::new(".")).map(|r| r.namespace), expected);
        assert_eq!(resolve_namespace(Path::new("src")).map(|r| r.namespace), expected);
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

        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
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

    #[test]
    fn wired_paths_never_use_the_windows_extended_length_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
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
            let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some(harness));
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
        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
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

        let _ = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
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
            commands.iter().any(|c| c.starts_with("node \"") && c.ends_with('"')),
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

        let _ = auto_install_detected_plugins(&project, ConfigNamespace::Set("TestProj"), Some("claude"));
        let after = std::fs::read_to_string(&cfg).expect("read settings");
        assert_eq!(after, before, "custom hook commands were rewritten");
    }

    #[test]
    fn repairs_a_runner_path_that_no_longer_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        let gone = tmp.path().join("moved-away/cli-hook.mjs");
        let cfg = tmp.path().join(".claude/settings.local.json");
        std::fs::write(
            &cfg,
            serde_json::to_string_pretty(&settings_with_runner(&gone.to_string_lossy())).unwrap(),
        )
        .expect("write settings");

        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner");
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 4, "{commands:?}");
        assert!(
            commands.iter().all(|c| *c == want),
            "missing runner not repaired: {commands:?}"
        );
        assert_eq!(claude_config_namespace(tmp.path()).as_deref(), Some("TestProj"));
    }

    fn claude_config(root: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(CLAUDE_CONFIG_REL)).unwrap()).unwrap()
    }

    #[test]
    fn fresh_claude_install_writes_config_and_plain_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("example-project"), Some("claude"));
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"].as_str().unwrap();
        assert_eq!(
            claude_config(tmp.path()),
            json!({"id": "neurostrata-claude", "namespace": "example-project"})
        );
        let commands = hook_commands(&tmp.path().join(".claude/settings.local.json"));
        assert_eq!(commands.len(), 4, "{commands:?}");
        let want = format!("node \"{abs_runner}\"");
        assert!(commands.iter().all(|c| *c == want), "{commands:?}");
        assert_eq!(configured_namespace(tmp.path()).as_deref(), Some("example-project"));
    }

    #[test]
    fn old_claude_install_is_repaired_keeping_its_namespace() {
        let _env = no_namespace_env();
        let (_tmp, dir) = git_project();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        // The marker names another namespace; the old command's is kept.
        marker_at(&dir, "example-other");
        let runner = dir.join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        let old = format!("NEUROSTRATA_NAMESPACE=example-project node \"{}\"", runner.display());
        assert_eq!(
            repair_installer_hook_command(&old, &runner.to_string_lossy(), &dir),
            Some(format!("node \"{}\"", runner.display()))
        );
        assert_eq!(claude_config_namespace(&dir).as_deref(), Some("example-project"));

        // Through the installer, called with the marker's namespace.
        std::fs::remove_file(dir.join(CLAUDE_CONFIG_REL)).unwrap();
        let cfg = dir.join(".claude/settings.local.json");
        std::fs::write(&cfg, serde_json::to_string_pretty(&settings_with_command(&old)).unwrap()).unwrap();
        let report = auto_install_detected_plugins(&dir, ConfigNamespace::Set("example-other"), Some("claude"));
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"].as_str().unwrap();
        assert_eq!(claude_config(&dir)["namespace"], "example-project");
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 4, "{commands:?}");
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
        let old = format!("NEUROSTRATA_NAMESPACE=example-project node \"{}\"", runner.display());
        assert_eq!(repair_installer_hook_command(&old, &runner.to_string_lossy(), root), None);

        let cfg = root.join(".claude/settings.local.json");
        let before = serde_json::to_string_pretty(&settings_with_command(&old)).unwrap();
        std::fs::write(&cfg, &before).unwrap();
        let report = auto_install_detected_plugins(root, ConfigNamespace::Set("example-project"), Some("claude"));
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), before, "old command must stay");
        assert_eq!(report["warnings"].as_array().map(Vec::len), Some(1), "{report}");

        // A fresh install still installs the hooks (fail-soft) and warns.
        std::fs::remove_file(&cfg).unwrap();
        let report = auto_install_detected_plugins(root, ConfigNamespace::Set("example-project"), Some("claude"));
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"].as_str().unwrap();
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 4, "{commands:?}");
        assert!(commands.iter().all(|c| *c == format!("node \"{abs_runner}\"")), "{commands:?}");
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
        let report = auto_install_detected_plugins(root, ConfigNamespace::Set("example-other"), Some("claude"));
        // SessionStart is read first, whatever order the settings keep.
        assert_eq!(claude_config(root)["namespace"], "example-first");
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"].as_str().unwrap();
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 4, "{commands:?}");
        assert!(commands.iter().all(|c| *c == format!("node \"{abs_runner}\"")), "{commands:?}");
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
        write_file(&dir.join(".NeuroStrata").join("config.json"), r#"{"version": 2, "namespace": "beta"}"#);
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
        let moved = root.join("moved").join(".NeuroStrata/plugins/claude/cli-hook.mjs");
        assert_eq!(
            installer_hook_parts(&format!("node \"{}\"", moved.display()), &abs_runner, &root),
            Some((None, format!("node \"{abs_runner}\"")))
        );
        // The installer's own runner.
        assert!(installer_hook_parts(&format!("node \"{abs_runner}\""), &abs_runner, &root).is_some());
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
        let report = auto_install_detected_plugins(root, ConfigNamespace::Set("example-project"), Some("claude"));
        let abs_runner = report["installed_plugins"][0]["cli_hook_runner"].as_str().unwrap();
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
        assert_eq!(matchers, vec![CLAUDE_FILE_TOOLS_MATCHER, CLAUDE_SESSION_SCOPE_MATCHER]);
        assert!(entries("Stop").iter().any(runs_ours));
        // A second install adds nothing.
        let before = std::fs::read_to_string(&cfg).unwrap();
        auto_install_detected_plugins(root, ConfigNamespace::Set("example-project"), Some("claude"));
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
        assert_eq!(configured_namespace(root).as_deref(), Some("example-project"));
        // The config file outranks what older installs left in the settings.
        write_file(
            &root.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "from-settings"}}"#,
        );
        assert_eq!(configured_namespace(root).as_deref(), Some("example-project"));
        std::fs::remove_file(root.join(CLAUDE_CONFIG_REL)).unwrap();
        assert_eq!(configured_namespace(root).as_deref(), Some("from-settings"));
    }

    #[cfg(windows)]
    #[test]
    fn repairs_the_installers_own_runner_in_verbatim_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        // First install puts the runner on disk and reports where it is.
        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
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

        let _ = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("claude"));
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 4, "{commands:?}");
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
        let report = auto_install_detected_plugins(&project, ConfigNamespace::Set("TestProj"), Some("claude"));
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

        let _ = auto_install_detected_plugins(&project, ConfigNamespace::Set("TestProj"), Some("claude"));
        let want = format!("node \"{abs_runner}\"");
        let commands = hook_commands(&cfg);
        assert_eq!(commands.len(), 4, "{commands:?}");
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

        let report = auto_install_detected_plugins(tmp.path(), ConfigNamespace::Set("TestProj"), Some("opencode"));
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
}
