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

const CLI_HOOK_RUNNER_MJS: &str = r#"#!/usr/bin/env node
import { readFileSync } from "node:fs";
import plugin, { initialize, onTurnEnd } from "./dist/index.js";

async function main() {
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
  if (typeof plugin.handleClaudeHookEvent === "function") {
    const out = await plugin.handleClaudeHookEvent(event);
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
"#;

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

        let abs_dist = dist_index
            .canonicalize()
            .unwrap_or_else(|_| dist_index.clone())
            .to_string_lossy()
            .to_string();
        let abs_runner = runner_path
            .canonicalize()
            .unwrap_or_else(|_| runner_path.clone())
            .to_string_lossy()
            .to_string();

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

    json!({
        "active_harness": active,
        "detected_harnesses": detected,
        "installed_plugins": installed,
    })
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
                let cfg_path = project_root.join(".opencode/neurostrata-plugin.json");
                let doc = json!({
                    "plugin": [abs_dist],
                    "neurostrata": {
                        "namespace": namespace,
                        "pluginPath": abs_dist
                    }
                });
                let res = write_json_if_changed(&cfg_path, &doc);
                let root_cfg = project_root.join("opencode.json");
                if root_cfg.exists() {
                    if let Ok(raw) = std::fs::read_to_string(&root_cfg) {
                        if let Ok(mut root_doc) = serde_json::from_str::<Value>(&raw) {
                            if let Some(obj) = root_doc.as_object_mut() {
                                let plugins = obj.entry("plugin").or_insert_with(|| json!([]));
                                if let Some(arr) = plugins.as_array_mut() {
                                    let already = arr.iter().any(|v| {
                                        v.as_str()
                                            .map(|s| s == abs_dist || s.contains("plugins/opencode"))
                                            .unwrap_or(false)
                                    });
                                    if !already {
                                        arr.push(json!(abs_dist));
                                    }
                                }
                            }
                            let _ = write_json_if_changed(&root_cfg, &root_doc);
                        }
                    }
                }
                return res;
            }
            None
        }
        "claude" => {
            if is_active || project_root.join(".claude").exists() {
                let cfg_path = project_root.join(".claude/settings.local.json");
                let cmd = format!("NEUROSTRATA_NAMESPACE={} node \"{}\"", namespace, abs_runner);
                let mut doc: Value = std::fs::read_to_string(&cfg_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!({}));
                if let Some(obj) = doc.as_object_mut() {
                    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
                    if let Some(h_obj) = hooks.as_object_mut() {
                        h_obj.entry("PreToolUse").or_insert_with(|| {
                            json!([{
                                "matcher": "Read|Write|Edit|MultiEdit|Bash|Task",
                                "hooks": [{ "type": "command", "command": cmd }]
                            }])
                        });
                        h_obj.entry("Stop").or_insert_with(|| {
                            json!([{
                                "hooks": [{ "type": "command", "command": cmd }]
                            }])
                        });
                    }
                }
                return write_json_if_changed(&cfg_path, &doc);
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
        let report = auto_install_detected_plugins(tmp.path(), "TestProj", Some("claude"));
        assert_eq!(report["active_harness"], json!("claude"));
        let installed = report["installed_plugins"].as_array().expect("installed array");
        assert!(!installed.is_empty());
        assert!(tmp.path().join(".NeuroStrata/plugins/claude/dist/index.js").exists());
        assert!(tmp.path().join(".claude/settings.local.json").exists());
    }
}