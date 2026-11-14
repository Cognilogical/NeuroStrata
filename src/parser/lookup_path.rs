//! Turning the file path a caller asks about into the relative path the graph
//! stores.
//!
//! Ingestion stores every file, directory and memory location relative to the
//! directory it walked (`node_id_for`), and agents name files the same way
//! (`src/lib.rs`). Harnesses, however, hand their hooks the absolute path of the
//! file being touched: Claude Code's `tool_input.file_path` is
//! `C:\dev\projects\example-project\src\lib.rs` or
//! `/home/me/example-project/src/lib.rs`. `normalize_node_path` only swaps
//! separators, so an absolute path never matched a stored location and no file
//! memory was ever returned for one.
//!
//! [`resolve_lookup_path`] strips the project root from an absolute path and
//! leaves a relative path untouched. The comparison is lexical first, so it
//! works for paths of either platform on any host (a daemon on Linux can be
//! asked about a Windows path its client sent), and falls back to the
//! filesystem's canonical form for symlinked roots (`/var` vs `/private/var` on
//! macOS, 8.3 short names on Windows).
//!
//! Which root, in order:
//! 1. the caller's `project_root`, when it contains the path -- the caller
//!    named it, so it wins;
//! 2. the nearest ancestor holding `.git` (a directory, or the FILE a linked
//!    git worktree has), so a file in a worktree under
//!    `.claude/worktrees/agent-x/` resolves against that worktree, whose layout
//!    mirrors the repository, and not against the main checkout. `.NeuroStrata`
//!    is deliberately not a marker: a global one lives in the home folder
//!    (`~/.NeuroStrata`), which would make every file under home look like
//!    part of a project rooted there;
//! 3. the namespace's registered project root.
//!
//! When the file exists, the part below the root is taken from the
//! filesystem's canonical path, so it carries the on-disk case: the store
//! matches locations exactly, and Windows volumes let a caller spell a file in
//! any case. A relative path is never rewritten, case included.
//!
//! When the namespace has a registered root, a marker root that does not share
//! its common git directory (the main checkout and its linked worktrees do; a
//! clone or submodule nested inside it does not) belongs to some other
//! repository: answering with that repository's relative path (`README.md`)
//! would return this namespace's memories for another project's file, so the
//! path is left as it was and matches nothing, as before.
//!
//! The root found here only relativizes the path. It never becomes the ingest
//! source key: a worktree that was never ingested has no active generation,
//! and pinning it would filter out every generation-scoped id.

use std::path::{Path, PathBuf};

/// Where the root that relativized a lookup path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// The path was already relative and is used exactly as given.
    Relative,
    /// The caller's `project_root` argument.
    ProjectRoot,
    /// The nearest ancestor holding `.git` (directory or worktree file).
    Marker,
    /// The project root cached on the namespace's registry row.
    RegistryRoot,
    /// Absolute, but under no root this namespace knows: left unchanged.
    Outside,
}

impl RootSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RootSource::Relative => "relative",
            RootSource::ProjectRoot => "project_root",
            RootSource::Marker => "marker",
            RootSource::RegistryRoot => "registry_root",
            RootSource::Outside => "outside",
        }
    }
}

/// The path to look up, and how it was derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupPath {
    /// What the store is queried with (it still goes through
    /// `normalize_node_path`). For `Relative` and `Outside` this is the input
    /// byte for byte.
    pub path: String,
    pub source: RootSource,
}

/// A lexically parsed absolute path: its root token and its components.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AbsPath {
    /// Windows paths (drive letter or UNC) compare case-insensitively, as the
    /// file systems they live on do by default.
    windows: bool,
    /// `C:`, `//server/share`, or `/`.
    root: String,
    parts: Vec<String>,
}

impl AbsPath {
    fn to_path_string(&self) -> String {
        let mut out = self.root.clone();
        if self.windows {
            out.push('/');
        }
        out.push_str(&self.parts.join("/"));
        out
    }
}

/// Parse `raw` as an absolute path of either platform, or `None` when it is
/// relative. Backslashes count as separators on every host, as they already do
/// in `normalize_node_path`. The Win32 verbatim (`\\?\`) and device (`\\.\`)
/// prefixes are dropped, so `\\?\C:\x` and `C:\x` are the same path.
fn parse_absolute(raw: &str) -> Option<AbsPath> {
    let forward = raw.replace('\\', "/");
    let mut rest: &str = &forward;
    let mut unc = false;
    for verbatim in ["//?/", "//./"] {
        if let Some(after) = rest.strip_prefix(verbatim) {
            if let Some(share) = after
                .strip_prefix("UNC/")
                .or_else(|| after.strip_prefix("unc/"))
            {
                rest = share;
                unc = true;
            } else {
                rest = after;
            }
            break;
        }
    }

    let bytes = rest.as_bytes();
    let (windows, root, tail): (bool, String, &str) = if !unc
        && bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/')
    {
        (
            true,
            format!("{}:", (bytes[0] as char).to_ascii_uppercase()),
            rest.get(2..).unwrap_or(""),
        )
    } else if unc || rest.starts_with("//") {
        let mut segments = rest.trim_start_matches('/').splitn(3, '/');
        let server = segments.next().filter(|s| !s.is_empty())?;
        let share = segments.next().filter(|s| !s.is_empty())?;
        let tail = segments.next().unwrap_or("");
        (
            true,
            format!("//{}/{}", server.to_lowercase(), share.to_lowercase()),
            tail,
        )
    } else if rest.starts_with('/') {
        (false, "/".to_string(), rest)
    } else {
        return None;
    };

    let mut parts: Vec<String> = Vec::new();
    for part in tail.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p.to_string()),
        }
    }
    Some(AbsPath { windows, root, parts })
}

/// Whether `raw` is an absolute path of either platform.
pub fn is_absolute_lookup_path(raw: &str) -> bool {
    parse_absolute(raw).is_some()
}

fn same_component(windows: bool, a: &str, b: &str) -> bool {
    if windows {
        a == b || a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// `path` relative to `root`, by spelling alone, with `/` separators; `None`
/// unless `root` strictly contains `path`. Windows spellings compare
/// case-insensitively; POSIX spellings compare exactly.
pub fn relative_to(path: &str, root: &str) -> Option<String> {
    let path = parse_absolute(path)?;
    let root = parse_absolute(root)?;
    if path.windows != root.windows || path.parts.len() <= root.parts.len() {
        return None;
    }
    let same_root = if path.windows {
        path.root.eq_ignore_ascii_case(&root.root)
    } else {
        path.root == root.root
    };
    if !same_root {
        return None;
    }
    let prefix_matches = path
        .parts
        .iter()
        .zip(root.parts.iter())
        .all(|(a, b)| same_component(path.windows, a, b));
    if !prefix_matches {
        return None;
    }
    Some(path.parts[root.parts.len()..].join("/"))
}

/// The host's own form of an absolute path, or `None` when this host would
/// read it as relative (a `C:\` path on Linux, say). Probing the disk with a
/// path the host reads as relative would probe the daemon's working directory.
fn host_path(path: &str) -> Option<PathBuf> {
    let parsed = parse_absolute(path)?;
    let host = PathBuf::from(parsed.to_path_string());
    host.is_absolute().then_some(host)
}

/// Canonical form via `dunce`, which on Windows returns `C:\x` rather than the
/// `\\?\C:\x` that `std::fs::canonicalize` gives.
fn canonical(path: &Path) -> Option<PathBuf> {
    dunce::canonicalize(path).ok()
}

/// [`relative_to`], retried on the canonical forms when the spellings differ
/// but both paths exist on this host: a symlinked root (`/var` vs
/// `/private/var`), a different case on a case-insensitive volume, an 8.3
/// short name. A file that does not exist yet (about to be written) is
/// canonicalized through its nearest existing ancestor.
pub fn relative_to_on_disk(path: &str, root: &str) -> Option<String> {
    if let Some(rel) = relative_to(path, root) {
        return Some(rel);
    }
    let root_canonical = canonical(&host_path(root)?)?;
    let host = host_path(path)?;
    let mut existing = host.as_path();
    let mut missing: Vec<String> = Vec::new();
    let base = loop {
        if let Some(c) = canonical(existing) {
            break c;
        }
        missing.push(existing.file_name()?.to_string_lossy().to_string());
        existing = existing.parent()?;
    };
    let mut full = base;
    for part in missing.iter().rev() {
        full.push(part);
    }
    relative_to(&full.to_string_lossy(), &root_canonical.to_string_lossy())
}

/// The nearest ancestor of `path` holding `.git` (a directory, or the file of
/// a linked worktree or submodule) -- the first pass of the plugins'
/// `findProjectRoot`. Returned in the input's own spelling, so [`relative_to`]
/// can strip it. `None` when the path is not absolute on this host or no
/// ancestor is a git checkout.
pub fn marker_root(path: &str) -> Option<String> {
    let parsed = parse_absolute(path)?;
    host_path(path)?;
    for depth in (0..parsed.parts.len()).rev() {
        let candidate = AbsPath {
            windows: parsed.windows,
            root: parsed.root.clone(),
            parts: parsed.parts[..depth].to_vec(),
        };
        let candidate = candidate.to_path_string();
        if Path::new(&candidate).join(".git").exists() {
            return Some(candidate);
        }
    }
    None
}

/// `rel` (the part of `path` below `root`) re-read from the filesystem when
/// the file exists: the canonical path carries the on-disk case and resolves
/// symlinked roots. Falls back to `rel` when the file does not exist yet, or
/// its canonical path leaves the root (a symlink pointing out of the project).
fn with_disk_spelling(path: &str, root: &str, rel: String) -> String {
    let (Some(file), Some(root_host)) = (host_path(path), host_path(root)) else {
        return rel;
    };
    let (Some(file), Some(root_c)) = (canonical(&file), canonical(&root_host)) else {
        return rel;
    };
    relative_to(&file.to_string_lossy(), &root_c.to_string_lossy()).unwrap_or(rel)
}

/// `rel` read relative to `base` unless it is absolute already.
fn joined(base: &Path, rel: &str) -> String {
    if is_absolute_lookup_path(rel) {
        rel.to_string()
    } else {
        base.join(rel).to_string_lossy().to_string()
    }
}

/// The common git directory of the checkout at `checkout`, the one every
/// linked worktree of a repository shares. A `.git` directory is its own. A
/// `.git` FILE names its gitdir (`worktree.useRelativePaths`, git 2.48+, writes
/// it relative to the checkout); a linked worktree's gitdir holds a
/// `commondir` file naming the shared directory, a submodule's does not, so a
/// submodule is its own repository.
fn common_git_dir(checkout: &str) -> Option<String> {
    let dot_git = Path::new(checkout).join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.to_string_lossy().to_string());
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let gitdir = joined(Path::new(checkout), gitdir);
    match std::fs::read_to_string(Path::new(&gitdir).join("commondir")) {
        Ok(common) => Some(joined(Path::new(&gitdir), common.trim())),
        Err(_) => Some(gitdir),
    }
}

/// Whether `a` and `b` name the same location: by spelling (`..` resolved,
/// Windows case-insensitive), else by their canonical forms on this host.
fn same_location(a: &str, b: &str) -> bool {
    let lexical = |a: &str, b: &str| match (parse_absolute(a), parse_absolute(b)) {
        (Some(a), Some(b)) => {
            a.windows == b.windows
                && same_component(a.windows, &a.root, &b.root)
                && a.parts.len() == b.parts.len()
                && a.parts.iter().zip(&b.parts).all(|(x, y)| same_component(a.windows, x, y))
        }
        _ => false,
    };
    if lexical(a, b) {
        return true;
    }
    match (host_path(a).and_then(|p| canonical(&p)), host_path(b).and_then(|p| canonical(&p))) {
        (Some(a), Some(b)) => lexical(&a.to_string_lossy(), &b.to_string_lossy()),
        _ => false,
    }
}

/// The checkout holding the registered `root`: `root` itself when it has a
/// `.git`, else its nearest ancestor that does (a project registered below
/// the repository root).
fn checkout_of(root: &str) -> Option<String> {
    if Path::new(root).join(".git").exists() {
        Some(root.to_string())
    } else {
        marker_root(root)
    }
}

/// Whether the checkouts at `a` and `b` belong to one repository: the main
/// checkout and its linked worktrees share a common git directory. A nested
/// independent clone or a submodule has its own, so it is another repository.
pub fn same_repository(a: &str, b: &str) -> bool {
    match (common_git_dir(a), common_git_dir(b)) {
        (Some(a), Some(b)) => same_location(&a, &b),
        _ => false,
    }
}

fn depth(rel: &str) -> usize {
    rel.split('/').count()
}

/// Resolve the path a `memories_for_file` caller sent into the relative path
/// the graph stores. See the module documentation for the order of roots.
pub fn resolve_lookup_path(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> LookupPath {
    if !is_absolute_lookup_path(raw) {
        return LookupPath { path: raw.to_string(), source: RootSource::Relative };
    }
    match choose_root(raw, project_root, registry_root) {
        Some((root, rel, source)) => {
            LookupPath { path: with_disk_spelling(raw, &root, rel), source }
        }
        None => LookupPath { path: raw.to_string(), source: RootSource::Outside },
    }
}

/// The root that relativizes absolute `raw`, the part below it, and where the
/// root came from; `None` when no root this namespace knows contains it.
fn choose_root(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> Option<(String, String, RootSource)> {
    if let Some(root) = project_root {
        if let Some(rel) = relative_to_on_disk(raw, root) {
            return Some((root.to_string(), rel, RootSource::ProjectRoot));
        }
    }
    let registry = registry_root
        .and_then(|root| relative_to_on_disk(raw, root).map(|rel| (root.to_string(), rel)));
    if let Some(marker) = marker_root(raw) {
        if let Some(marker_rel) = relative_to(raw, &marker) {
            let by_marker = Some((marker.clone(), marker_rel.clone(), RootSource::Marker));
            let Some(root) = registry_root else {
                return by_marker;
            };
            let home = checkout_of(root);
            let same_repo = home.as_deref().is_some_and(|h| same_repository(&marker, h));
            match &registry {
                // Both contain the file: the deeper root is the more specific
                // one. A worktree inside the registered checkout is deeper; a
                // project registered below the repository's `.git` is deeper.
                Some((reg_root, reg_rel)) => {
                    if depth(reg_rel) < depth(&marker_rel) {
                        return Some((reg_root.clone(), reg_rel.clone(), RootSource::RegistryRoot));
                    }
                    if depth(reg_rel) == depth(&marker_rel) || same_repo {
                        return by_marker;
                    }
                    // A clone or submodule nested in the registered checkout
                    // is another repository: its `src/lib.rs` is not ours.
                    return None;
                }
                // Another checkout of the same repository: the main checkout
                // or a sibling worktree, seen from a registered worktree. Only
                // when the registered root is a checkout root: a project
                // registered at `repo/platform` stores paths below `platform`,
                // which another checkout's root-relative path would not match.
                None if same_repo
                    && home.as_deref().is_some_and(|h| same_location(root, h))
                    && !home.as_deref().is_some_and(|h| same_location(&marker, h)) =>
                {
                    return by_marker;
                }
                // Another repository's file: answering with its relative path
                // would hand back this namespace's memories for a stranger.
                None => {}
            }
        }
    }
    registry.map(|(root, rel)| (root, rel, RootSource::RegistryRoot))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure, host-independent ----

    #[test]
    fn relative_input_is_returned_byte_for_byte() {
        for raw in ["src/lib.rs", r"src\lib.rs", "./src/lib.rs", "lib.rs", "C:lib.rs", ""] {
            let got = resolve_lookup_path(raw, Some("/home/me/example-project"), Some(r"C:\x"));
            assert_eq!(got, LookupPath { path: raw.to_string(), source: RootSource::Relative });
        }
    }

    #[test]
    fn absolute_forms_of_both_platforms_are_recognized() {
        for raw in [
            r"C:\dev\example-project\src\lib.rs",
            "c:/dev/example-project/src/lib.rs",
            r"\\?\C:\dev\example-project\src\lib.rs",
            r"\\server\share\example-project\src\lib.rs",
            r"\\?\UNC\server\share\example-project\src\lib.rs",
            "/home/me/example-project/src/lib.rs",
        ] {
            assert!(is_absolute_lookup_path(raw), "{raw}");
        }
        for raw in ["src/lib.rs", r"src\lib.rs", "C:lib.rs", "./a", "../a"] {
            assert!(!is_absolute_lookup_path(raw), "{raw}");
        }
    }

    #[test]
    fn windows_paths_strip_case_insensitively_across_separators_and_verbatim() {
        let root = r"C:\dev\projects\example-project\platform";
        for path in [
            r"C:\dev\projects\example-project\platform\renderer\lightpanda_process.py",
            "c:/Dev/Projects/Example-Project/platform/renderer/lightpanda_process.py",
            r"\\?\C:\dev\projects\example-project\platform\renderer\lightpanda_process.py",
        ] {
            assert_eq!(
                relative_to(path, root).as_deref(),
                Some("renderer/lightpanda_process.py"),
                "{path}"
            );
        }
        assert_eq!(
            relative_to(r"C:\dev\a\b.rs", r"\\?\c:\dev\").as_deref(),
            Some("a/b.rs")
        );
        assert_eq!(
            relative_to(
                r"\\?\UNC\Server\Share\example-project\src\lib.rs",
                r"\\server\share\example-project"
            )
            .as_deref(),
            Some("src/lib.rs")
        );
        // Another drive, a sibling with a shared name prefix, the root itself.
        assert_eq!(relative_to(r"D:\dev\a.rs", r"C:\dev"), None);
        assert_eq!(relative_to(r"C:\dev\projects2\a.rs", r"C:\dev\projects"), None);
        assert_eq!(relative_to(r"C:\dev\projects", r"C:\dev\projects"), None);
    }

    #[test]
    fn posix_paths_strip_exactly() {
        let root = "/home/me/example-project";
        assert_eq!(
            relative_to("/home/me/example-project/src/lib.rs", root).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            relative_to("/home/me/example-project/./src/../src/lib.rs", "/home/me/example-project/")
                .as_deref(),
            Some("src/lib.rs")
        );
        // POSIX names are case-sensitive; a case difference is left to the
        // on-disk fallback, which only a real volume can answer.
        assert_eq!(relative_to("/home/me/Example-Project/src/lib.rs", root), None);
        assert_eq!(relative_to("/home/me/example-project2/src/lib.rs", root), None);
        // Mixed platforms never match.
        assert_eq!(relative_to("/c/dev/a.rs", r"C:\dev"), None);
        assert_eq!(relative_to(r"C:\home\me\a.rs", "/home/me"), None);
    }

    #[test]
    fn caller_project_root_wins_when_it_contains_the_path() {
        let got = resolve_lookup_path(
            r"C:\dev\projects\example-project\platform\renderer\x.py",
            Some(r"C:\dev\projects\example-project\platform"),
            Some(r"C:\dev\projects\example-project"),
        );
        assert_eq!(got.path, "renderer/x.py");
        assert_eq!(got.source, RootSource::ProjectRoot);
    }

    #[test]
    fn registry_root_strips_a_path_with_no_marker_on_this_host() {
        // A Windows path asked of a POSIX daemon, or a path that does not
        // exist here: no marker can be probed, the registered root still names
        // the project.
        let got = resolve_lookup_path(
            "/nonexistent-ns-lookup/example-project/src/lib.rs",
            None,
            Some("/nonexistent-ns-lookup/example-project"),
        );
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::RegistryRoot });
        let got = resolve_lookup_path(
            r"Q:\nonexistent-ns-lookup\example-project\src\lib.rs",
            None,
            Some(r"q:/nonexistent-ns-lookup/example-project/"),
        );
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::RegistryRoot });
    }

    #[test]
    fn an_absolute_path_under_no_known_root_is_left_unchanged() {
        let raw = "/nonexistent-ns-lookup/elsewhere/src/lib.rs";
        let got = resolve_lookup_path(raw, Some("/nonexistent-ns-lookup/example-project"), None);
        assert_eq!(got, LookupPath { path: raw.into(), source: RootSource::Outside });
    }

    // ---- filesystem layouts (temp dirs) ----

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// `<tmp>/example-project` with a `.git` directory, `src/lib.rs`, and a
    /// linked worktree at `.claude/worktrees/agent-1` whose `.git` is a file.
    fn project_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path()).unwrap().join("example-project");
        std::fs::create_dir_all(root.join(".git").join("worktrees").join("agent-1")).unwrap();
        touch(&root.join("src").join("lib.rs"));
        let worktree = root.join(".claude").join("worktrees").join("agent-1");
        touch(&worktree.join("src").join("lib.rs"));
        let gitdir = root.join(".git").join("worktrees").join("agent-1");
        link_worktree(&worktree, &gitdir);
        (tmp, root, worktree)
    }

    /// What `git worktree add` writes: the checkout's `.git` file naming its
    /// gitdir, and the gitdir's `commondir` pointing back at the shared `.git`.
    fn link_worktree(worktree: &Path, gitdir: &Path) {
        std::fs::create_dir_all(gitdir).unwrap();
        std::fs::create_dir_all(worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.to_string_lossy().replace('\\', "/")),
        )
        .unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().to_string()
    }

    #[test]
    fn marker_walk_finds_the_git_root_without_any_root_argument() {
        let (_tmp, root, _) = project_with_worktree();
        let file = root.join("src").join("lib.rs");
        let got = resolve_lookup_path(&s(&file), None, None);
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::Marker });
        // A file about to be created (does not exist yet) resolves the same way.
        let new_file = root.join("src").join("new").join("mod.rs");
        assert_eq!(resolve_lookup_path(&s(&new_file), None, None).path, "src/new/mod.rs");
    }

    #[test]
    fn a_worktree_file_resolves_against_the_worktree_not_the_main_checkout() {
        let (_tmp, root, worktree) = project_with_worktree();
        let file = s(&worktree.join("src").join("lib.rs"));
        // No registry root, and with the main checkout registered: both give
        // the repository-relative path, never `.claude/worktrees/agent-1/...`.
        for registry in [None, Some(s(&root))] {
            let got = resolve_lookup_path(&file, None, registry.as_deref());
            assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::Marker });
        }
    }

    #[test]
    fn a_worktree_beside_the_main_checkout_is_recognized_by_its_gitdir() {
        let (tmp, root, _) = project_with_worktree();
        let beside = canonical(tmp.path()).unwrap().join("example-project-wt");
        touch(&beside.join("src").join("lib.rs"));
        link_worktree(&beside, &root.join(".git").join("worktrees").join("wt"));
        let got = resolve_lookup_path(&s(&beside.join("src").join("lib.rs")), None, Some(&s(&root)));
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::Marker });
    }

    #[test]
    fn another_repository_is_not_answered_for_when_the_namespace_has_a_root() {
        let (tmp, root, _) = project_with_worktree();
        let other = canonical(tmp.path()).unwrap().join("other-repo");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        let file = s(&other.join("src").join("lib.rs"));
        touch(Path::new(&file));
        let got = resolve_lookup_path(&file, None, Some(&s(&root)));
        assert_eq!(got, LookupPath { path: file.clone(), source: RootSource::Outside });
    }

    #[test]
    fn a_clone_or_submodule_nested_in_the_registered_checkout_is_another_repository() {
        let (_tmp, root, worktree) = project_with_worktree();
        // An independent clone with its own `.git` directory.
        let clone = root.join("vendor").join("other");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        // A submodule: a `.git` file into `.git/modules/<name>`, which has no
        // `commondir`, so it is not a linked worktree of the registered root.
        let submodule = root.join("vendor").join("sub");
        let modules = root.join(".git").join("modules").join("sub");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::create_dir_all(&submodule).unwrap();
        std::fs::write(submodule.join(".git"), format!("gitdir: {}\n", s(&modules))).unwrap();
        for dir in [&clone, &submodule] {
            let file = s(&dir.join("src").join("lib.rs"));
            touch(Path::new(&file));
            let got = resolve_lookup_path(&file, None, Some(&s(&root)));
            assert_eq!(got, LookupPath { path: file.clone(), source: RootSource::Outside });
        }
        // A linked worktree in the same place still resolves.
        let got = resolve_lookup_path(&s(&worktree.join("src").join("lib.rs")), None, Some(&s(&root)));
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::Marker });
    }

    #[test]
    fn a_registered_worktree_resolves_the_main_checkout_and_its_siblings() {
        let (tmp, root, worktree) = project_with_worktree();
        let sibling = canonical(tmp.path()).unwrap().join("example-project-wt");
        touch(&sibling.join("src").join("lib.rs"));
        link_worktree(&sibling, &root.join(".git").join("worktrees").join("wt"));
        let registered = s(&worktree);
        for checkout in [&root, &sibling] {
            let got = resolve_lookup_path(&s(&checkout.join("src").join("lib.rs")), None, Some(&registered));
            assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::Marker });
        }
        // A submodule of the main checkout is still another repository.
        let submodule = root.join("vendor").join("sub");
        let modules = root.join(".git").join("modules").join("sub");
        std::fs::create_dir_all(&modules).unwrap();
        touch(&submodule.join("src").join("lib.rs"));
        std::fs::write(submodule.join(".git"), format!("gitdir: {}\n", s(&modules))).unwrap();
        let file = s(&submodule.join("src").join("lib.rs"));
        let got = resolve_lookup_path(&file, None, Some(&registered));
        assert_eq!(got, LookupPath { path: file.clone(), source: RootSource::Outside });
    }

    #[test]
    fn a_project_registered_below_the_git_root_does_not_resolve_other_worktrees() {
        // Registered at `repo/platform`: stored paths are relative to
        // `platform`, so a sibling worktree's `platform/x.py` must not
        // resolve to the repository-relative `platform/x.py`.
        let (tmp, root, _) = project_with_worktree();
        let platform = root.join("platform");
        touch(&platform.join("x.py"));
        let sibling = canonical(tmp.path()).unwrap().join("example-project-wt");
        link_worktree(&sibling, &root.join(".git").join("worktrees").join("wt"));
        for file in [sibling.join("platform").join("x.py"), sibling.join("x.py")] {
            touch(&file);
            let got = resolve_lookup_path(&s(&file), None, Some(&s(&platform)));
            assert_eq!(got, LookupPath { path: s(&file), source: RootSource::Outside });
        }
    }

    #[test]
    fn a_project_registered_below_the_git_root_wins_over_the_git_root() {
        // Repository at `repo`, NeuroStrata project (and ingest root) at
        // `repo/platform`: stored paths are relative to `platform`.
        let tmp = tempfile::tempdir().unwrap();
        let repo = canonical(tmp.path()).unwrap().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let platform = repo.join("platform");
        let file = platform.join("renderer").join("x.py");
        touch(&file);
        let got = resolve_lookup_path(&s(&file), None, Some(&s(&platform)));
        assert_eq!(got, LookupPath { path: "renderer/x.py".into(), source: RootSource::RegistryRoot });
    }

    #[test]
    fn a_neurostrata_folder_alone_is_not_a_project_root() {
        // `~/.NeuroStrata` exists on machines running the daemon; treating it
        // as a marker would root every file under home at the home folder.
        let tmp = tempfile::tempdir().unwrap();
        let home = canonical(tmp.path()).unwrap().join("home");
        std::fs::create_dir_all(home.join(".NeuroStrata")).unwrap();
        let file = home.join("scratch").join("notes.py");
        touch(&file);
        let above_has_git = std::iter::successors(Some(home.as_path()), |p| p.parent())
            .any(|p| p.join(".git").exists());
        if !above_has_git {
            let got = resolve_lookup_path(&s(&file), None, None);
            assert_eq!(got, LookupPath { path: s(&file), source: RootSource::Outside });
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_matches_through_its_canonical_form() {
        // macOS: /var/folders/... is /private/var/folders/...; the same shape
        // is built here with an explicit symlink so Linux exercises it too.
        let tmp = tempfile::tempdir().unwrap();
        let real = canonical(tmp.path()).unwrap().join("real-project");
        let file = real.join("src").join("lib.rs");
        touch(&file);
        let link = canonical(tmp.path()).unwrap().join("linked-project");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let got = resolve_lookup_path(&s(&file), Some(&s(&link)), None);
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::ProjectRoot });
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_root_spelled_in_another_case_or_verbatim_matches() {
        let (_tmp, root, _) = project_with_worktree();
        let file = s(&root.join("src").join("lib.rs"));
        let upper = s(&root).to_uppercase();
        let verbatim = format!(r"\\?\{}", s(&root));
        for given in [upper, verbatim] {
            let got = resolve_lookup_path(&file, Some(&given), None);
            assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::ProjectRoot }, "{given}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_file_spelled_in_another_case_resolves_to_the_on_disk_case() {
        // The store matches locations exactly; Windows lets the caller spell
        // `SRC\LIB.RS` for `src\lib.rs`.
        let (_tmp, root, _) = project_with_worktree();
        let shouted = format!(r"{}\SRC\LIB.RS", s(&root).to_uppercase());
        let got = resolve_lookup_path(&shouted, None, None);
        assert_eq!(got, LookupPath { path: "src/lib.rs".into(), source: RootSource::Marker });
    }
}
