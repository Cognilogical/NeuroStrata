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
import plugin, * as mod from "./dist/index.js";

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
  const init = mod.initialize ?? plugin?.initialize ?? plugin?.setup;
  if (typeof init === "function") {
    await init({ config: { namespace: process.env.NEUROSTRATA_NAMESPACE } });
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
const SUBAGENT_GRAPH_FIRST_RULE_SNIPPET: &str = "\n- **NEUROSTRATA GRAPH-FIRST & SUBAGENT CONTEXT RULE:** Honor the `### NeuroStrata Task & Graph Context` (`subagent_prompt_injection`) block in your prompt — every `[rule]` entry is a binding architectural constraint. Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed under `### NeuroStrata Task & Graph Context` in your prompt. For any file you touch that is NOT listed in that block, call `neurostrata_memories_for_file` first (or `neurostrata_related` / `neurostrata_search_memory` with `include_related: true` / `neurostrata_sparql_query`). Never leave `TODO`, `FIXME`, `stub`, or `phase-2` deferrals.\n";

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

/// Rewrite only the path of an installer-generated runner command. A command
/// shaped `NEUROSTRATA_NAMESPACE=<ns> node "<runner>"` whose runner basename
/// matches the installer's runner is ours; a custom hook that merely carries
/// the namespace environment variable is not, and is left untouched.
fn path_basename(p: &str) -> Option<&str> {
    p.rsplit(['/', '\\']).find(|s| !s.is_empty())
}

fn repair_installer_hook_command(
    command: &str,
    namespace: &str,
    abs_runner: &str,
) -> Option<String> {
    let prefix = format!("NEUROSTRATA_NAMESPACE={namespace} node \"");
    let rest = command.strip_prefix(&prefix)?;
    let close = rest.find('"')?;
    let old_path = &rest[..close];
    let suffix = &rest[close + 1..];
    // Windows separators are legal in a verbatim path even when the test
    // that exercises this runs on Unix, so do not use platform Path parsing.
    if path_basename(old_path) != path_basename(abs_runner) {
        return None;
    }
    Some(format!("{prefix}{abs_runner}\"{suffix}"))
}

fn has_any_env(keys: &[&str]) -> bool {
    keys.iter()
        .any(|k| std::env::var_os(k).map(|v| !v.is_empty()).unwrap_or(false))
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

    let env_active = if has_any_env(&["OPENCODE", "OPENCODE_PID", "OPENCODE_SESSION", "OPENCODE_CONFIG"]) {
        Some("opencode")
    } else if has_any_env(&["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_PROJECT_DIR", "ANTHROPIC_CLAUDE_CODE"]) {
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
        || home.as_ref().map(|h| h.join(".config/opencode").exists() || h.join(".local/share/opencode").exists()).unwrap_or(false)
    {
        push_unique("opencode");
    }
    if project_root.join(".openclaw").exists()
        || home.as_ref().map(|h| h.join(".openclaw").exists()).unwrap_or(false)
    {
        push_unique("openclaw");
    }
    if project_root.join(".claude").exists()
        || home.as_ref().map(|h| h.join(".claude").exists()).unwrap_or(false)
    {
        push_unique("claude");
    }
    if project_root.join(".hermes").exists()
        || home.as_ref().map(|h| h.join(".hermes").exists()).unwrap_or(false)
    {
        push_unique("hermes");
    }
    if project_root.join(".codex").exists()
        || home.as_ref().map(|h| h.join(".codex").exists()).unwrap_or(false)
    {
        push_unique("codex");
    }
    if project_root.join(".github/copilot").exists()
        || home.as_ref().map(|h| h.join(".config/gh").exists() || h.join(".config/github-copilot").exists()).unwrap_or(false)
    {
        push_unique("github-cli");
    }

    if detected.is_empty() {
        detected.extend_from_slice(KNOWN_HARNESSES);
    }

    (active, detected)
}

/// Auto-install the compiled zero-dependency plugin bundles for all detected
/// harnesses into `<project_root>/.NeuroStrata/plugins/<harness>/` and wire
/// harness-specific config files when the harness directory or active harness
/// is present.
pub fn auto_install_detected_plugins(
    project_root: &Path,
    namespace: &str,
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
                if replaced != content { Some(replaced) } else { None }
            } else if !content.contains(SUBAGENT_RULE_MARKER) && !content.contains("tools: false") {
                Some(format!("{}{}", content.trim_end(), SUBAGENT_GRAPH_FIRST_RULE_SNIPPET))
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

fn wire_harness_config(
    project_root: &Path,
    namespace: &str,
    harness: &str,
    is_active: bool,
    abs_dist: &str,
    abs_runner: &str,
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
            std::fs::write(path, serialized).ok()?;
        }
        Some(path.to_string_lossy().to_string())
    };

    match harness {
        "opencode" => {
            if is_active || project_root.join(".opencode").exists() || project_root.join("opencode.json").exists() {
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
                let cfg_path = project_root.join(".opencode/neurostrata-plugin.json");
                let doc = json!({
                    "plugin": [abs_dist],
                    "neurostrata": {
                        "namespace": namespace,
                        "pluginPath": abs_dist
                    }
                });
                let _ = write_json_if_changed(&cfg_path, &doc);

                return Some(loader_path.to_string_lossy().to_string());
            }
            None
        }
        "claude" => {
            if is_active || project_root.join(".claude").exists() {
                let cfg_path = project_root.join(".claude/settings.local.json");
                let cmd = format!("NEUROSTRATA_NAMESPACE={} node \"{}\"", namespace, abs_runner);
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
                            h_obj.entry("PreToolUse").or_insert_with(|| {
                                json!([{
                                    "matcher": "Read|Write|Edit|MultiEdit|Bash|Task",
                                    "hooks": [{ "type": "command", "command": cmd.clone() }]
                                }])
                            });
                            h_obj.entry("Stop").or_insert_with(|| {
                                json!([{
                                    "hooks": [{ "type": "command", "command": cmd.clone() }]
                                }])
                            });
                            // Repair commands written by an older installer that
                            // embedded a Windows verbatim path. Only rewrite our
                            // own hook commands; user hooks are left untouched.
                            for entries in h_obj.values_mut() {
                                if let Some(arr) = entries.as_array_mut() {
                                    for entry in arr {
                                        if let Some(hooks) = entry
                                            .get_mut("hooks")
                                            .and_then(|v| v.as_array_mut())
                                        {
                                            for hook in hooks {
                                                if let Some(command) =
                                                    hook.get("command").and_then(|v| v.as_str())
                                                {
                                                    if let Some(repaired) =
                                                        repair_installer_hook_command(
                                                            command,
                                                            namespace,
                                                            abs_runner,
                                                        )
                                                    {
                                                        hook["command"] =
                                                            Value::String(repaired);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
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
                let cfg_path = project_root.join(".NeuroStrata/plugins/github-cli/copilot-extension.json");
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

    #[test]
    fn auto_install_detected_plugins_writes_bundles_and_wires_explicit_harness() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let custom_agent_dir = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&custom_agent_dir).expect("create .claude/agents");
        let custom_agent_md = custom_agent_dir.join("custom-worker.md");
        std::fs::write(&custom_agent_md, "# Custom Worker\n\n- Run assigned tasks.\n")
            .expect("write custom-worker.md");

        let report = auto_install_detected_plugins(tmp.path(), "TestProj", Some("claude"));
        assert_eq!(report["active_harness"], json!("claude"));
        let installed = report["installed_plugins"].as_array().expect("installed array");
        assert!(!installed.is_empty());
        assert!(tmp.path().join(".NeuroStrata/plugins/claude/dist/index.js").exists());
        assert!(tmp.path().join(".claude/settings.local.json").exists());
        assert!(tmp.path().join(".agents/skills/neurostrata/SKILL.md").exists());
        assert!(tmp.path().join(".agents/agents/NeuroStrata-Task.md").exists());
        assert!(tmp.path().join(".claude/skills/neurostrata/SKILL.md").exists());
        assert!(tmp.path().join(".claude/agents/NeuroStrata-Task.md").exists());
        let patched = std::fs::read_to_string(&custom_agent_md).expect("read patched custom-worker.md");
        assert!(patched.contains("subagent_prompt_injection"));
    }

    #[test]
    fn wired_paths_never_use_the_windows_extended_length_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = auto_install_detected_plugins(tmp.path(), "TestProj", Some("claude"));
        let runner = report["installed_plugins"][0]["cli_hook_runner"]
            .as_str()
            .expect("cli_hook_runner");
        assert!(!runner.starts_with(r"\\?\"), "verbatim path leaked: {runner}");
        let settings = std::fs::read_to_string(tmp.path().join(".claude/settings.local.json"))
            .expect("settings.local.json");
        assert!(!settings.contains(r"\\\\?\\"), "verbatim path in hook command");
    }

    #[test]
    fn wired_paths_are_plain_for_every_harness() {
        let cases = [
            ("opencode", ".opencode/neurostrata-plugin.json"),
            ("claude", ".claude/settings.local.json"),
            ("hermes", ".hermes/plugins/neurostrata.json"),
            ("codex", ".codex/plugins/neurostrata.json"),
            ("openclaw", ".openclaw/plugins/neurostrata.json"),
            ("github-cli", ".NeuroStrata/plugins/github-cli/copilot-extension.json"),
        ];
        for (harness, cfg_rel) in cases {
            let tmp = tempfile::tempdir().expect("tempdir");
            let report = auto_install_detected_plugins(tmp.path(), "TestProj", Some(harness));
            assert!(report["ok"].as_bool().unwrap_or(true), "{harness} install report");
            let cfg = tmp.path().join(cfg_rel);
            assert!(cfg.exists(), "{harness} config missing at {}", cfg.display());
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
                assert!(!runner.starts_with(r"\\?\"), "{harness} runner is verbatim: {runner}");
            }
        }
    }

    #[test]
    fn repairs_an_existing_verbatim_hook_command_without_touching_user_hooks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).expect("create .claude");
        let report = auto_install_detected_plugins(tmp.path(), "TestProj", Some("claude"));
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
                                .map(|c| c.starts_with("NEUROSTRATA_NAMESPACE=TestProj node \""))
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
        let verbatim = format!(r"\\?\C:\old\{}", basename);
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

        let _ = auto_install_detected_plugins(tmp.path(), "TestProj", Some("claude"));
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
                .any(|c| c.starts_with("NEUROSTRATA_NAMESPACE=TestProj node \"")
                    && c.ends_with('"')),
            "our repaired hook is present: {commands:?}"
        );
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
            serde_json::to_string_pretty(&json!({ "plugins": ["./other.js"], "otherKey": "preserve" })).unwrap(),
        )
        .expect("write root opencode.json");

        let report = auto_install_detected_plugins(tmp.path(), "TestProj", Some("opencode"));
        assert_eq!(report["active_harness"], json!("opencode"));

        // Core plugin-core bundle must be present so relative imports resolve.
        assert!(tmp.path().join(".NeuroStrata/plugins/core/dist/index.js").exists());
        assert!(tmp.path().join(".NeuroStrata/plugins/core/package.json").exists());
        // Opencode bundle + hook runner.
        assert!(tmp.path().join(".NeuroStrata/plugins/opencode/dist/index.js").exists());
        assert!(tmp.path().join(".NeuroStrata/plugins/opencode/cli-hook.mjs").exists());

        // Thin loader wired into OpenCode.
        let loader = tmp.path().join(".opencode/plugins/neurostrata.js");
        assert!(loader.exists());
        let loader_src = std::fs::read_to_string(&loader).expect("read loader");
        assert!(
            loader_src.contains("export { default } from \"../../.NeuroStrata/plugins/opencode/dist/index.js\";"),
            "loader re-exports default"
        );
        assert!(
            loader_src.contains("export * from \"../../.NeuroStrata/plugins/opencode/dist/index.js\";"),
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
                            v.as_str()
                                == Some("./.NeuroStrata/plugins/opencode/dist/index.js")
                        })
                    })
                    .unwrap_or(true),
                None => true,
            }
        } else {
            true
        };
        assert!(oc_untouched, ".opencode/opencode.json must not contain the loader path");

        // Root opencode.json is never written: existing entries preserved, loader path never inserted.
        let root_doc: Value = serde_json::from_str(&std::fs::read_to_string(root_cfg).expect("read opencode.json"))
            .expect("parse opencode.json");
        let root_plugins = root_doc.get("plugins").and_then(|v| v.as_array()).expect("plugins array");
        assert_eq!(root_plugins.len(), 1, "root opencode.json plugins: {root_plugins:?}");
        assert_eq!(root_plugins[0].as_str(), Some("./other.js"));
        assert!(!root_plugins.iter().any(|v| v.as_str() == Some("./.NeuroStrata/plugins/opencode/dist/index.js")));
        assert_eq!(root_doc.get("otherKey").and_then(|v| v.as_str()), Some("preserve"));
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
            "TestProj",
            "opencode",
            true,
            "./.NeuroStrata/plugins/opencode/dist/index.js",
            "./.NeuroStrata/plugins/opencode/cli-hook.mjs",
        );
        assert!(wired.is_some(), "loader should be wired for opencode");
        assert!(
            root.join(".opencode/plugins/neurostrata.js").is_file(),
            "autoload shim written at .opencode/plugins/neurostrata.js"
        );

        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(opencode_dir.join("opencode.json")).expect("read opencode.json"),
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