//! Pluggable workflow lifecycle connectors: workspace scope capture, state
//! digests, changed-surface detection, and lifecycle trigger installation.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::task::gate::GateScope;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerInstallReport {
    pub connector_id: String,
    pub installed_paths: Vec<String>,
    pub details: String,
}

pub trait WorkflowConnector: Send + Sync {
    fn connector_id(&self) -> &str;
    fn capture_scope(&self, root: &Path) -> GateScope;
    fn capture_state_digest(&self, root: &Path) -> (String, String);
    fn changed_paths(&self, root: &Path) -> Vec<String>;
    fn install_triggers(&self, root: &Path) -> Result<TriggerInstallReport>;
}

// ---------- Git connector ----------

const GIT_CONNECTOR_ID: &str = "git";

fn run_git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root.to_string_lossy().as_ref())
        .args(args)
        .output()
        .ok()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// The ONE shared committed-diff baseline for changed-set consumers
/// (Git connector `changed_paths`, doc gate `watched_added_lines`).
///
/// Baseline, named deliberately: `@{u}..HEAD` when an upstream resolves,
/// else `origin/main..HEAD` when `origin/main` resolves, else `None`.
/// There is deliberately NO silent `HEAD~1..HEAD` degradation: a one-commit
/// range would make enforcement depend on checkout shape and under-report
/// multi-commit work. `None` means "no committed range is knowable here";
/// callers fall back to the uncommitted (porcelain) set only.
pub(crate) fn committed_diff_range(root: &Path) -> Option<String> {
    for candidate in ["@{u}", "origin/main"] {
        let ok = run_git(root, &["rev-parse", "--verify", "--quiet", candidate])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(format!("{candidate}..HEAD"));
        }
    }
    None
}

fn normalize_path(p: &str) -> String {
    let p = p.trim();
    let p = if p.starts_with('"') && p.ends_with('"') && p.len() >= 2 {
        &p[1..p.len() - 1]
    } else {
        p
    };
    let p = p
        .split(" -> ")
        .map(|s| s.to_string())
        .next()
        .unwrap_or_default();
    let p = p.trim().replace('\\', "/");
    p.trim_start_matches('/').to_string()
}

/// Default Git-backed connector preserving the pre-existing hardwired behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitWorkflowConnector;

impl WorkflowConnector for GitWorkflowConnector {
    fn connector_id(&self) -> &str {
        GIT_CONNECTOR_ID
    }

    fn capture_scope(&self, root: &Path) -> GateScope {
        // Exact logic from historical GateScope::capture.
        let root = root.canonicalize().ok();
        let git_root = root.as_ref().and_then(|root| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|path| std::path::PathBuf::from(path.trim()))
        });
        git_root
            .map(|git_root| {
                let worktree = git_root
                    .canonicalize()
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned());
                let branch = worktree.as_ref().and_then(|worktree| {
                    std::process::Command::new("git")
                        .args(["-C", worktree, "branch", "--show-current"])
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .and_then(|out| String::from_utf8(out.stdout).ok())
                        .map(|name| name.trim().to_string())
                        .filter(|name| !name.is_empty())
                });
                GateScope { branch, worktree }
            })
            .unwrap_or_default()
    }

    fn capture_state_digest(&self, root: &Path) -> (String, String) {
        // Exact logic from historical compute_worktree_state in receipt.rs.
        let path_hash = sha256_hex(root.to_string_lossy().as_bytes());
        let head_sha = run_git(root, &["rev-parse", "HEAD"])
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| format!("none-{path_hash}"));
        let dirty_tree_hash = run_git(root, &["status", "--porcelain"])
            .filter(|o| o.status.success())
            .map(|o| sha256_hex(&o.stdout))
            .unwrap_or_else(|| format!("clean-{path_hash}"));
        (head_sha, dirty_tree_hash)
    }

    fn changed_paths(&self, root: &Path) -> Vec<String> {
        let mut paths = std::collections::BTreeSet::new();
        // Committed changed set against the shared baseline (see
        // committed_diff_range): a clean/committed checkout — CI, a merged
        // PR, the strict task gate — must not report an empty changed set.
        if let Some(range) = committed_diff_range(root) {
            if let Some(out) = run_git(root, &["diff", "--name-only", &range]) {
                if out.status.success() {
                    for line in String::from_utf8_lossy(&out.stdout).lines() {
                        let norm = normalize_path(line);
                        if !norm.is_empty() {
                            paths.insert(norm);
                        }
                    }
                }
            }
        }
        if let Some(out) = run_git(root, &["status", "--porcelain"]) {
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if line.len() < 4 {
                        continue;
                    }
                    let path = &line[3..];
                    if let Some(arrow) = path.find(" -> ") {
                        let new = &path[arrow + 4..];
                        let norm = normalize_path(new);
                        if !norm.is_empty() {
                            paths.insert(norm);
                        }
                    } else {
                        let norm = normalize_path(path);
                        if !norm.is_empty() {
                            paths.insert(norm);
                        }
                    }
                }
            }
        }
        paths.into_iter().collect()
    }

    fn install_triggers(&self, root: &Path) -> Result<TriggerInstallReport> {
        let _ = crate::task::hook_hygiene(root);
        let hooks_dir = run_git(root, &["rev-parse", "--git-path", "hooks"])
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout).ok()
                } else {
                    None
                }
            })
            .map(|p| {
                let p = p.trim();
                if p.starts_with('/') {
                    PathBuf::from(p)
                } else {
                    root.join(p)
                }
            })
            .or_else(|| Some(root.join(".git").join("hooks")))
            .unwrap_or_else(|| root.join(".git").join("hooks"));
        if !hooks_dir.is_dir() {
            return Err(anyhow::anyhow!(
                "git hooks directory not found at {}",
                hooks_dir.display()
            ));
        }
        let pre_push_path = hooks_dir.join("pre-push");
        if !pre_push_path.exists() {
            std::fs::write(&pre_push_path, crate::task::SCAFFOLDED_PRE_PUSH_HOOK)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &pre_push_path,
                    std::fs::Permissions::from_mode(0o755),
                );
            }
        }
        let pre_commit_path = hooks_dir.join("pre-commit");
        if !pre_commit_path.exists() {
            std::fs::write(&pre_commit_path, crate::task::SCAFFOLDED_PRE_COMMIT_HOOK)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &pre_commit_path,
                    std::fs::Permissions::from_mode(0o755),
                );
            }
        }
        let ns_dir = root.join(".NeuroStrata");
        std::fs::create_dir_all(&ns_dir)?;
        let triggers_path = ns_dir.join("workflow-triggers.json");
        let mut installed_paths = vec![triggers_path.to_string_lossy().to_string()];
        if pre_push_path.exists() {
            installed_paths.push(pre_push_path.to_string_lossy().to_string());
        }
        if pre_commit_path.exists() {
            installed_paths.push(pre_commit_path.to_string_lossy().to_string());
        }
        let json = serde_json::json!({
            "connector_id": GIT_CONNECTOR_ID,
            "triggers": ["pre-checkpoint", "pre-publish", "pre-commit", "pre-push"],
            "installed_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
        std::fs::write(&triggers_path, serde_json::to_string_pretty(&json)?)?;
        Ok(TriggerInstallReport {
            connector_id: GIT_CONNECTOR_ID.to_string(),
            installed_paths,
            details: "Git pre-push and pre-commit hooks installed/verified, .NeuroStrata/workflow-triggers.json written".to_string(),
        })
    }
}

// ---------- Manifest connector ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestTriggers {
    List(Vec<String>),
    Map(std::collections::BTreeMap<String, String>),
}

impl Default for ManifestTriggers {
    fn default() -> Self {
        Self::List(default_triggers())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestScope {
    Gate(GateScope),
    Paths(Vec<String>),
}

/// Config struct for the manifest connector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowConnectorManifest {
    #[serde(default = "default_connector_id", alias = "connector")]
    pub connector_id: String,
    #[serde(default)]
    pub scope: Option<ManifestScope>,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub revision_id: Option<String>,
    #[serde(default)]
    pub dirty_state_hash: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub triggers: ManifestTriggers,
}

fn default_connector_id() -> String {
    "manifest".to_string()
}

fn default_triggers() -> Vec<String> {
    vec!["pre-checkpoint".to_string(), "pre-publish".to_string()]
}

/// Manifest-backed connector for non-Git workspaces.
#[derive(Debug, Clone)]
pub struct ManifestWorkflowConnector {
    pub manifest: WorkflowConnectorManifest,
    pub manifest_path: PathBuf,
}

impl ManifestWorkflowConnector {
    pub fn from_file(root: &Path) -> Result<Self> {
        // Check NEUROSTRATA_WORKFLOW_CONNECTOR_FILE env var
        if let Some(env_path) = std::env::var("NEUROSTRATA_WORKFLOW_CONNECTOR_FILE").ok() {
            let env_path = PathBuf::from(env_path);
            if env_path.is_file() {
                let json = std::fs::read_to_string(&env_path)?;
                let manifest: WorkflowConnectorManifest = serde_json::from_str(&json)?;
                return Ok(Self {
                    manifest,
                    manifest_path: env_path,
                });
            }
        }
        // Check for .NeuroStrata/workflow-connector.json in root and ancestors
        let mut path = root.to_path_buf();
        for _ in 0..4 {
            let candidate = path.join(".NeuroStrata").join("workflow-connector.json");
            if candidate.is_file() {
                let json = std::fs::read_to_string(&candidate)?;
                let manifest: WorkflowConnectorManifest = serde_json::from_str(&json)?;
                return Ok(Self {
                    manifest,
                    manifest_path: candidate,
                });
            }
            path.pop();
        }
        Err(anyhow::anyhow!("no workflow-connector.json found"))
    }
}

impl WorkflowConnector for ManifestWorkflowConnector {
    fn connector_id(&self) -> &str {
        &self.manifest.connector_id
    }

    fn capture_scope(&self, root: &Path) -> GateScope {
        let scope_gate = match &self.manifest.scope {
            Some(ManifestScope::Gate(g)) => Some(g),
            _ => None,
        };
        let stream = scope_gate
            .and_then(|s| s.branch.clone())
            .or_else(|| self.manifest.stream.clone());
        let workspace = scope_gate
            .and_then(|s| s.worktree.clone())
            .or_else(|| self.manifest.workspace.clone())
            .or_else(|| {
                root.canonicalize()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
                    .or_else(|| Some(root.to_string_lossy().to_string()))
            });
        GateScope {
            branch: stream,
            worktree: workspace,
        }
    }

    fn capture_state_digest(&self, root: &Path) -> (String, String) {
        let path_hash = sha256_hex(root.to_string_lossy().as_bytes());
        let revision_id = self
            .manifest
            .revision_id
            .clone()
            .unwrap_or_else(|| format!("manifest-rev-{path_hash}"));
        let dirty_state_hash = self
            .manifest
            .dirty_state_hash
            .clone()
            .unwrap_or_else(|| format!("manifest-clean-{path_hash}"));
        (revision_id, dirty_state_hash)
    }

    fn changed_paths(&self, _root: &Path) -> Vec<String> {
        let mut paths = self.manifest.changed_paths.clone();
        paths.sort();
        paths.dedup();
        paths
    }

    fn install_triggers(&self, root: &Path) -> Result<TriggerInstallReport> {
        let ns_dir = root.join(".NeuroStrata");
        std::fs::create_dir_all(&ns_dir)?;
        let triggers_path = ns_dir.join("workflow-triggers.json");
        let json = serde_json::json!({
            "connector_id": self.manifest.connector_id,
            "triggers": self.manifest.triggers,
            "installed_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
        std::fs::write(&triggers_path, serde_json::to_string_pretty(&json)?)?;
        Ok(TriggerInstallReport {
            connector_id: self.manifest.connector_id.clone(),
            installed_paths: vec![triggers_path.to_string_lossy().to_string()],
            details: "manifested workflow triggers written".to_string(),
        })
    }
}

// ---------- Resolve workflow connector ----------

pub fn resolve_workflow_connector(root: &Path) -> Box<dyn WorkflowConnector> {
    if let Ok(env_path) = std::env::var("NEUROSTRATA_WORKFLOW_CONNECTOR_FILE") {
        let env_path = PathBuf::from(env_path);
        if env_path.is_file() {
            if let Ok(connector) = ManifestWorkflowConnector::from_file(&env_path) {
                return Box::new(connector);
            }
        }
    }
    let mut path = root.to_path_buf();
    for _ in 0..4 {
        let candidate = path.join(".NeuroStrata").join("workflow-connector.json");
        if candidate.is_file() {
            if let Ok(connector) = ManifestWorkflowConnector::from_file(root) {
                return Box::new(connector);
            }
        }
        path.pop();
    }
    Box::new(GitWorkflowConnector)
}

// ---------- Unit tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn manifest_connector_captures_scope_and_digest_without_git() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let manifest_path = root.join(".NeuroStrata").join("workflow-connector.json");
        std::fs::create_dir_all(root.join(".NeuroStrata")).unwrap();
        std::fs::write(
            &manifest_path,
            serde_json::json!({
                "connector_id": "manifest",
                "stream": "main",
                "workspace": "/custom/workspace",
                "revision_id": "manifest-rev-123",
                "dirty_state_hash": "manifest-clean-456",
                "changed_paths": ["src/workflow/mod.rs"],
                "triggers": ["pre-checkpoint", "pre-publish"]
            })
            .to_string(),
        )
        .unwrap();

        let connector = resolve_workflow_connector(root);
        let scope = crate::task::gate::GateScope::capture(root);
        assert_eq!(scope.branch.as_deref(), Some("main"));
        assert_eq!(scope.worktree.as_deref(), Some("/custom/workspace"));

        let digest = crate::task::receipt::compute_worktree_state(root);
        assert_eq!(digest.0, "manifest-rev-123");
        assert_eq!(digest.1, "manifest-clean-456");

        let report = connector.install_triggers(root).unwrap();
        assert_eq!(report.connector_id, "manifest");
        assert!(report
            .installed_paths
            .iter()
            .any(|p| p.ends_with("workflow-triggers.json")));

        let changed = connector.changed_paths(root);
        assert_eq!(changed, vec!["src/workflow/mod.rs".to_string()]);
    }

    #[test]
    fn manifest_connector_parses_documented_object_triggers_and_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let manifest_path = root.join(".NeuroStrata").join("workflow-connector.json");
        std::fs::create_dir_all(root.join(".NeuroStrata")).unwrap();
        std::fs::write(
            &manifest_path,
            serde_json::json!({
                "connector": "jj",
                "scope": { "branch": "feat/custom", "worktree": "/workspace/project" },
                "triggers": {
                    "pre_checkpoint": "jj status",
                    "pre_publish": "neurostrata-mcp task gate MyProject --strict && neurostrata-mcp doc-gate"
                }
            })
            .to_string(),
        )
        .unwrap();

        let connector = resolve_workflow_connector(root);
        assert_eq!(connector.connector_id(), "jj");
        let scope = connector.capture_scope(root);
        assert_eq!(scope.branch.as_deref(), Some("feat/custom"));
        assert_eq!(scope.worktree.as_deref(), Some("/workspace/project"));
    }

    #[test]
    fn git_connector_changed_paths_include_committed_changes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .expect("git available in test env");
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "worker@example.com"]);
        run(&["config", "user.name", "worker"]);
        std::fs::write(root.join("watched.txt"), "v1").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "baseline"]);
        // Pin the deliberate baseline: origin/main at the baseline commit.
        run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(root.join("watched.txt"), "v2").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "committed surface change"]);
        // Tree is clean: a porcelain-only changed set would be empty here.
        let changed = GitWorkflowConnector.changed_paths(root);
        assert!(
            changed.iter().any(|p| p == "watched.txt"),
            "committed change must be in changed_paths, got {changed:?}"
        );
    }

    #[test]
    fn git_connector_install_triggers_writes_hooks_in_fresh_repo() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let hooks_dir = root.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        let report = GitWorkflowConnector.install_triggers(root).unwrap();
        assert_eq!(report.connector_id, "git");
        assert!(hooks_dir.join("pre-push").is_file());
        assert!(hooks_dir.join("pre-commit").is_file());
        assert!(report
            .installed_paths
            .iter()
            .any(|p| p.ends_with("pre-push")));
        assert!(report
            .installed_paths
            .iter()
            .any(|p| p.ends_with("pre-commit")));
    }
}