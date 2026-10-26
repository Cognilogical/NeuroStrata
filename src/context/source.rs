//! Canonical source/worktree resolution for a context request.
//!
//! `source::resolve` is the single place that turns a file path + project root
//! into a [`SourceIdentity`]. Keeping it here means the adapter, the server and
//! the graph-read tools cannot disagree about which worktree a request is
//! about (concurrent worktrees must not conflate source identity).

use super::model::SourceIdentity;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Normalize a path the same way `parser::ingest::normalize_node_path` does,
/// so the context layer and ingestion can never drift on path spelling.
pub fn normalize_node_path(path: &str) -> String {
    let forward = path.replace('\\', "/");
    let trimmed = forward.strip_prefix("./").unwrap_or(&forward);
    trimmed.trim_end_matches('/').to_string()
}

/// Canonical absolute form of a path, falling back to the input on failure.
pub fn canonical(path: &str) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .to_string()
}

fn git_output(root: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolve the source identity for `path` within `project_root`.
///
/// `path` may be absolute or relative to `project_root`. Fields that cannot be
/// determined (no git, no remote) fall back to canonical filesystem paths and
/// `"unknown"` rather than failing: a context request must always be
/// answerable, and an honest "unknown" is better than a panic.
pub fn resolve(path: impl AsRef<str>, project_root: impl AsRef<str>) -> SourceIdentity {
    let path = path.as_ref();
    let root = project_root.as_ref();
    let root_path = Path::new(root);
    let abs_path = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root_path.join(path)
    };

    let relative_path = abs_path
        .strip_prefix(root_path)
        .map(|p| normalize_node_path(&p.to_string_lossy()))
        .unwrap_or_else(|_| normalize_node_path(path));

    let root_canonical = canonical(root);
    let repo_id = git_output(root, &["remote", "get-url", "origin"])
        .unwrap_or_else(|| root_canonical.clone());
    let checkout_id = root_canonical.clone();
    let worktree_id = root_canonical;
    let revision = git_output(root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git_output(root, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    SourceIdentity {
        repo_id,
        checkout_id,
        worktree_id,
        revision,
        dirty,
        relative_path,
    }
}

/// Alias kept for callers that name the operation differently.
pub fn resolve_source(path: impl AsRef<str>, project_root: impl AsRef<str>) -> SourceIdentity {
    resolve(path, project_root)
}

/// The stable source key (canonical worktree path) used by ingestion's
/// generation pointer table. Mirrors `parser::ingest::source_key_for` so the
/// two stay aligned.
pub fn source_key(project_root: impl AsRef<str>) -> String {
    canonical(project_root.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative_path() {
        let root = std::env::temp_dir();
        let root_str = root.to_string_lossy().to_string();
        let id = resolve("src/lib.rs", &root_str);
        assert_eq!(id.relative_path, "src/lib.rs");
    }

    #[test]
    fn resolve_normalizes_backslashes() {
        let root = std::env::temp_dir();
        let root_str = root.to_string_lossy().to_string();
        let id = resolve(r"src\lib.rs", &root_str);
        assert_eq!(id.relative_path, "src/lib.rs");
    }
}
