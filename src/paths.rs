//! # Path handling for file lookups.
//!
//! Path rules belong here, not at call sites: a separator swap, prefix strip,
//! canonicalize or "is this absolute?" test written by hand is a second copy
//! of a rule, and copies drift.
//!
//! The model:
//!
//! - **Stored paths** (node ids, memory locations, `governs`) are relative to
//!   the project root and use `/`. That relative path is the portable
//!   identity; an absolute root is local to one machine.
//! - **Absolute paths of either platform are recognized on any host**
//!   ([`is_absolute_any`]): a daemon on Linux can be handed a Windows path by
//!   its client. `\` and `/` are both separators; the Win32 verbatim (`\\?\`)
//!   and device (`\\.\`) prefixes are dropped.
//! - **Comparison** ([`relative_to`]) normalizes separators, not case. The
//!   only case folded is the drive letter, which Windows tools spell either
//!   way (`c:\x` from some shells, `C:\x` from others) for the same volume.
//!   Whether `Src` and `src` are one directory is the file system's answer,
//!   not the spelling's: [`relative_to_on_disk`] asks it by comparing
//!   canonical forms when both paths exist (case on a case-insensitive volume,
//!   symlinked roots such as macOS `/var` vs `/private/var`, 8.3 short names).
//! - **Project root of a file** ([`to_project_relative`]): the caller's
//!   `project_root`, else the namespace's registered root when it is usable on
//!   this host, else the nearest ancestor holding `.git` (a directory, or the
//!   file of a linked worktree). `.NeuroStrata` is never a root marker:
//!   `~/.NeuroStrata` exists on daemon hosts and would root every file under
//!   home at the home folder.

use std::path::{Path, PathBuf};

/// A lexically parsed absolute path: its root token and its components.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AbsPath {
    /// A drive-letter or UNC path, as opposed to a POSIX one. The two kinds
    /// never match each other.
    windows: bool,
    /// `C:` (drive letter upper-cased), `//server/share`, or `/`.
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
/// relative. `.` and `..` components are resolved lexically.
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
        (true, format!("//{server}/{share}"), tail)
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
pub fn is_absolute_any(raw: &str) -> bool {
    parse_absolute(raw).is_some()
}

/// `path` relative to `root`, by spelling alone, with `/` separators; `None`
/// unless `root` strictly contains `path`. Components compare exactly; only
/// the drive letter is case-folded (see the module documentation).
pub fn relative_to(path: &str, root: &str) -> Option<String> {
    let path = parse_absolute(path)?;
    let root = parse_absolute(root)?;
    if path.windows != root.windows
        || path.root != root.root
        || path.parts.len() <= root.parts.len()
        || path.parts[..root.parts.len()] != root.parts[..]
    {
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
/// but both paths exist on this host. A file that does not exist yet (about
/// to be written) is canonicalized through its nearest existing ancestor.
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

/// Whether `root` can serve as a root on this host: absolute here and an
/// existing directory. A cached root from another machine, or a relative one,
/// is not.
pub fn is_usable_root(root: &str) -> bool {
    host_path(root).is_some_and(|p| p.is_dir())
}

/// The nearest ancestor of `path` holding `.git` (a directory, or the file of
/// a linked worktree or submodule). Returned in the input's own spelling, so
/// [`relative_to`] can strip it. `None` when the path is not absolute on this
/// host or no ancestor is a git checkout.
pub fn find_git_root(path: &str) -> Option<String> {
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

/// Whether `dir` is the top of a checkout (holds `.git`, a directory or a
/// linked worktree's file), as opposed to a directory inside one.
fn is_checkout_root(dir: &str) -> bool {
    host_path(dir).is_some_and(|p| p.join(".git").exists())
}

/// The common git directory of the checkout at `checkout`, the one every
/// linked worktree of a repository shares. A `.git` directory is its own. A
/// `.git` FILE names its gitdir (`worktree.useRelativePaths`, git 2.48+, writes
/// it relative to the checkout); a linked worktree's gitdir holds a
/// `commondir` file naming the shared directory, a submodule's does not, so a
/// submodule is its own repository.
fn common_git_dir(checkout: &str) -> Option<String> {
    let checkout = host_path(checkout)?;
    let dot_git = checkout.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.to_string_lossy().to_string());
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let joined = |base: &Path, rel: &str| {
        if is_absolute_any(rel) {
            rel.to_string()
        } else {
            base.join(rel).to_string_lossy().to_string()
        }
    };
    let gitdir = joined(&checkout, gitdir);
    match std::fs::read_to_string(Path::new(&gitdir).join("commondir")) {
        Ok(common) => Some(joined(Path::new(&gitdir), common.trim())),
        Err(_) => Some(gitdir),
    }
}

/// Whether `a` and `b` name the same location: by spelling, else by their
/// canonical forms on this host.
fn same_location(a: &str, b: &str) -> bool {
    let lexical = |a: &str, b: &str| matches!((parse_absolute(a), parse_absolute(b)), (Some(a), Some(b)) if a == b);
    if lexical(a, b) {
        return true;
    }
    match (host_path(a).and_then(|p| canonical(&p)), host_path(b).and_then(|p| canonical(&p))) {
        (Some(a), Some(b)) => lexical(&a.to_string_lossy(), &b.to_string_lossy()),
        _ => false,
    }
}

/// Whether the checkouts at `a` and `b` belong to one repository: the main
/// checkout and its linked worktrees share a common git directory. A nested
/// independent clone or a submodule has its own.
fn same_repository(a: &str, b: &str) -> bool {
    match (common_git_dir(a), common_git_dir(b)) {
        (Some(a), Some(b)) => same_location(&a, &b),
        _ => false,
    }
}

fn depth(rel: &str) -> usize {
    rel.split('/').count()
}

/// Where the root that relativized a lookup path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// The path was already relative and is used exactly as given.
    Relative,
    /// The caller's `project_root` argument.
    ProjectRoot,
    /// The project root cached on the namespace's registry row.
    RegistryRoot,
    /// The nearest ancestor holding `.git` (directory or worktree file).
    Marker,
    /// Absolute, but under no root this namespace knows: left unchanged.
    Outside,
}

impl RootSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RootSource::Relative => "relative",
            RootSource::ProjectRoot => "project_root",
            RootSource::RegistryRoot => "registry_root",
            RootSource::Marker => "marker",
            RootSource::Outside => "outside",
        }
    }
}

/// The path to look up, how it was derived, and the root it was made relative
/// to (`None` for `Relative` and `Outside`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupPath {
    /// What the store is queried with first. For `Relative` and `Outside` this
    /// is the input byte for byte.
    pub path: String,
    pub source: RootSource,
    pub root: Option<String>,
}

/// Turn the path a caller sent into the project-relative path the graph
/// stores. A relative path is returned byte for byte, without touching the
/// disk. An absolute one is made relative to, in order:
/// 1. `project_root`, when it contains the path;
/// 2. `registry_root`, when it is usable on this host ([`is_usable_root`]) and
///    contains the path. An unusable one is ignored, never a veto;
/// 3. the nearest ancestor holding `.git`.
///
/// Within 1 and 2, a `.git` between the root and the file keeps apart what is
/// not this project: a nested clone or submodule is never resolved, and a
/// linked worktree inside the root resolves against the worktree only when
/// the root is the top of a checkout of the same repository. Under a project
/// registered at a subdirectory (`repo/platform`), stored paths are relative
/// to that subdirectory, which another checkout's paths would not match.
///
/// Step 3 with a usable registered root applies the same rule: another
/// checkout of the registered repository resolves only when the registered
/// root is a checkout root; anything else is another project and stays
/// `Outside`, so it never borrows this namespace's memories.
///
/// When the file exists, the part below the root is taken from its canonical
/// path, so it carries the on-disk spelling the store matches exactly.
pub fn to_project_relative(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> LookupPath {
    if !is_absolute_any(raw) {
        return LookupPath { path: raw.to_string(), source: RootSource::Relative, root: None };
    }
    match choose_root(raw, project_root, registry_root) {
        Some((root, rel, source)) => LookupPath {
            path: with_disk_spelling(raw, &root, rel),
            source,
            root: Some(root),
        },
        None => LookupPath { path: raw.to_string(), source: RootSource::Outside, root: None },
    }
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

/// The root that relativizes absolute `raw`, the part below it, and where the
/// root came from; `None` when no root this namespace knows contains it.
fn choose_root(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> Option<(String, String, RootSource)> {
    let registry_root = registry_root.filter(|root| is_usable_root(root));
    let marker = find_git_root(raw);
    let marker_rel = marker.as_deref().and_then(|m| relative_to(raw, m));
    for (root, source) in [
        (project_root, RootSource::ProjectRoot),
        (registry_root, RootSource::RegistryRoot),
    ] {
        let Some(root) = root else { continue };
        let Some(rel) = relative_to_on_disk(raw, root) else { continue };
        return match (&marker, &marker_rel) {
            // A checkout nested inside the root (deeper than it).
            (Some(inner), Some(inner_rel)) if depth(inner_rel) < depth(&rel) => {
                (is_checkout_root(root) && same_repository(inner, root))
                    .then(|| (inner.clone(), inner_rel.clone(), RootSource::Marker))
            }
            _ => Some((root.to_string(), rel, source)),
        };
    }
    let (marker, marker_rel) = (marker?, marker_rel?);
    match registry_root {
        None => Some((marker, marker_rel, RootSource::Marker)),
        Some(root) if is_checkout_root(root) && same_repository(&marker, root) => {
            Some((marker, marker_rel, RootSource::Marker))
        }
        Some(_) => None,
    }
}

/// Every spelling to query the store with for `raw`, the resolved form first.
/// Stored locations are normally relative, but older memories may carry an
/// absolute path; they are read as they are, never rewritten:
/// - a resolved absolute path is also queried in the caller's own absolute
///   spelling;
/// - a relative path is also queried joined onto `known_root` (when it is
///   usable here and the join stays inside it);
/// - an `Outside` path is queried only as given.
pub fn lookup_forms(raw: &str, lookup: &LookupPath, known_root: Option<&str>) -> Vec<String> {
    let mut forms = vec![lookup.path.clone()];
    match lookup.source {
        RootSource::Outside => {}
        RootSource::Relative => {
            let joined = known_root.filter(|r| is_usable_root(r)).and_then(|root| {
                let joined = parse_absolute(&format!("{root}/{raw}"))?.to_path_string();
                relative_to(&joined, root).map(|_| joined)
            });
            forms.extend(joined);
        }
        RootSource::ProjectRoot | RootSource::RegistryRoot | RootSource::Marker => {
            forms.push(raw.to_string());
            forms.extend(parse_absolute(raw).map(|p| p.to_path_string()));
        }
    }
    let mut seen = std::collections::HashSet::new();
    forms.retain(|f| seen.insert(crate::parser::ingest::normalize_node_path(f)));
    forms
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure, host-independent ----

    #[test]
    fn relative_input_is_returned_byte_for_byte() {
        for raw in ["src/lib.rs", r"src\lib.rs", "./src/lib.rs", "lib.rs", "C:lib.rs", ""] {
            let got = to_project_relative(raw, Some("/home/me/example-project"), Some(r"C:\x"));
            assert_eq!(got, LookupPath { path: raw.to_string(), source: RootSource::Relative, root: None });
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
            assert!(is_absolute_any(raw), "{raw}");
        }
        for raw in ["src/lib.rs", r"src\lib.rs", "C:lib.rs", "./a", "../a"] {
            assert!(!is_absolute_any(raw), "{raw}");
        }
    }

    #[test]
    fn windows_spellings_strip_across_separators_verbatim_and_drive_case() {
        let root = r"C:\dev\projects\example-project\platform";
        for path in [
            r"C:\dev\projects\example-project\platform\renderer\x.py",
            "c:/dev/projects/example-project/platform/renderer/x.py",
            r"\\?\C:\dev\projects\example-project\platform\renderer\x.py",
        ] {
            assert_eq!(relative_to(path, root).as_deref(), Some("renderer/x.py"), "{path}");
        }
        assert_eq!(relative_to(r"C:\dev\a\b.rs", r"\\?\c:\dev\").as_deref(), Some("a/b.rs"));
        assert_eq!(
            relative_to(r"\\?\UNC\server\share\example-project\src\lib.rs", r"\\server\share\example-project")
                .as_deref(),
            Some("src/lib.rs")
        );
        // Separators and drive letters are normalized, other case is not: only
        // the file system can say whether `Dev` is `dev`.
        assert_eq!(relative_to(r"C:\Dev\a.rs", r"C:\dev"), None);
        // Another drive, a sibling with a shared name prefix, the root itself.
        assert_eq!(relative_to(r"D:\dev\a.rs", r"C:\dev"), None);
        assert_eq!(relative_to(r"C:\dev\projects2\a.rs", r"C:\dev\projects"), None);
        assert_eq!(relative_to(r"C:\dev\projects", r"C:\dev\projects"), None);
    }

    #[test]
    fn posix_paths_strip_exactly() {
        let root = "/home/me/example-project";
        assert_eq!(relative_to("/home/me/example-project/src/lib.rs", root).as_deref(), Some("src/lib.rs"));
        assert_eq!(
            relative_to("/home/me/example-project/./src/../src/lib.rs", "/home/me/example-project/").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(relative_to("/home/me/Example-Project/src/lib.rs", root), None);
        assert_eq!(relative_to("/home/me/example-project2/src/lib.rs", root), None);
        // Traversal out of the root is resolved first, so it does not match.
        assert_eq!(relative_to("/home/me/example-project/../other/lib.rs", root), None);
        // Mixed platforms never match.
        assert_eq!(relative_to("/c/dev/a.rs", r"C:\dev"), None);
        assert_eq!(relative_to(r"C:\home\me\a.rs", "/home/me"), None);
    }

    #[test]
    fn caller_project_root_wins_when_it_contains_the_path() {
        let got = to_project_relative(
            r"C:\dev\projects\example-project\platform\renderer\x.py",
            Some(r"C:\dev\projects\example-project\platform"),
            Some(r"C:\dev\projects\example-project"),
        );
        assert_eq!(got.path, "renderer/x.py");
        assert_eq!(got.source, RootSource::ProjectRoot);
    }

    #[test]
    fn an_absolute_path_under_no_known_root_is_left_unchanged() {
        let raw = "/nonexistent-ns-lookup/elsewhere/src/lib.rs";
        let got = to_project_relative(raw, Some("/nonexistent-ns-lookup/example-project"), None);
        assert_eq!(got, LookupPath { path: raw.into(), source: RootSource::Outside, root: None });
        assert_eq!(lookup_forms(raw, &got, None), vec![raw.to_string()]);
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
        std::fs::create_dir_all(root.join(".git")).unwrap();
        touch(&root.join("src").join("lib.rs"));
        let worktree = root.join(".claude").join("worktrees").join("agent-1");
        touch(&worktree.join("src").join("lib.rs"));
        link_worktree(&worktree, &root.join(".git").join("worktrees").join("agent-1"));
        (tmp, root, worktree)
    }

    /// What `git worktree add` writes: the checkout's `.git` file naming its
    /// gitdir, and the gitdir's `commondir` pointing back at the shared `.git`.
    fn link_worktree(worktree: &Path, gitdir: &Path) {
        std::fs::create_dir_all(gitdir).unwrap();
        std::fs::create_dir_all(worktree).unwrap();
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", s(gitdir).replace('\\', "/"))).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    }

    /// A submodule at `dir`: a `.git` file into `.git/modules/<name>`, which
    /// has no `commondir`, so it is its own repository.
    fn submodule(root: &Path, dir: &Path) {
        let modules = root.join(".git").join("modules").join("sub");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(".git"), format!("gitdir: {}\n", s(&modules))).unwrap();
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().to_string()
    }

    fn resolved(path: &str, source: RootSource) -> (String, RootSource) {
        (path.to_string(), source)
    }

    fn got(raw: &Path, project_root: Option<&Path>, registry: Option<&Path>) -> (String, RootSource) {
        let p = project_root.map(s);
        let r = registry.map(s);
        let l = to_project_relative(&s(raw), p.as_deref(), r.as_deref());
        (l.path, l.source)
    }

    #[test]
    fn git_fallback_finds_the_root_without_any_root_argument() {
        let (_tmp, root, _) = project_with_worktree();
        assert_eq!(got(&root.join("src").join("lib.rs"), None, None), resolved("src/lib.rs", RootSource::Marker));
        // A file about to be created resolves the same way.
        let new_file = root.join("src").join("new").join("mod.rs");
        assert_eq!(got(&new_file, None, None), resolved("src/new/mod.rs", RootSource::Marker));
    }

    #[test]
    fn an_unusable_registered_root_does_not_block_the_git_fallback() {
        // A root cached as a relative path, or one from another machine, is
        // ignored; it must not veto the `.git` fallback.
        let (_tmp, root, _) = project_with_worktree();
        let file = s(&root.join("src").join("lib.rs"));
        for cached in ["example-project", "/nonexistent-ns-lookup/example-project", r"Q:\nonexistent\example-project"] {
            let l = to_project_relative(&file, None, Some(cached));
            assert_eq!((l.path.as_str(), l.source), ("src/lib.rs", RootSource::Marker), "{cached}");
        }
    }

    #[test]
    fn a_registered_root_wins_over_the_git_root_above_it() {
        // Repository at `repo`, project registered at `repo/platform`: stored
        // paths are relative to `platform`.
        let tmp = tempfile::tempdir().unwrap();
        let repo = canonical(tmp.path()).unwrap().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let platform = repo.join("platform");
        let file = platform.join("renderer").join("x.py");
        touch(&file);
        assert_eq!(got(&file, None, Some(&platform)), resolved("renderer/x.py", RootSource::RegistryRoot));
    }

    #[test]
    fn a_checkout_root_registration_resolves_its_own_linked_worktrees() {
        let (tmp, root, worktree) = project_with_worktree();
        let inside = worktree.join("src").join("lib.rs");
        assert_eq!(got(&inside, None, Some(&root)), resolved("src/lib.rs", RootSource::Marker));
        assert_eq!(got(&inside, Some(&root), None), resolved("src/lib.rs", RootSource::Marker));
        let beside = canonical(tmp.path()).unwrap().join("example-project-wt");
        touch(&beside.join("src").join("lib.rs"));
        link_worktree(&beside, &root.join(".git").join("worktrees").join("wt"));
        assert_eq!(got(&beside.join("src").join("lib.rs"), None, Some(&root)), resolved("src/lib.rs", RootSource::Marker));
        // A registered worktree resolves the main checkout and its siblings.
        for checkout in [&root, &beside] {
            assert_eq!(got(&checkout.join("src").join("lib.rs"), None, Some(&worktree)), resolved("src/lib.rs", RootSource::Marker));
        }
    }

    #[test]
    fn a_nested_repository_inside_the_root_is_not_resolved() {
        let (_tmp, root, worktree) = project_with_worktree();
        let clone = root.join("vendor").join("other");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        let sub = root.join("vendor").join("sub");
        submodule(&root, &sub);
        for dir in [&clone, &sub] {
            let file = dir.join("src").join("lib.rs");
            touch(&file);
            for (project_root, registry) in [(None, Some(&root)), (Some(&root), None), (None, None)] {
                let expected = if project_root.is_none() && registry.is_none() {
                    // No root known: the nested repository is the only root.
                    resolved("src/lib.rs", RootSource::Marker)
                } else {
                    resolved(&s(&file), RootSource::Outside)
                };
                assert_eq!(got(&file, project_root.map(|p| p.as_path()), registry.map(|p| p.as_path())), expected, "{}", s(&file));
            }
            // From a registered worktree, the main checkout's submodule is
            // still another repository.
            assert_eq!(got(&file, None, Some(&worktree)), resolved(&s(&file), RootSource::Outside));
        }
    }

    #[test]
    fn a_linked_worktree_under_a_registered_subdirectory_is_not_resolved_against_it() {
        // Registered at `repo/platform`, worktree at
        // `repo/platform/.claude/worktrees/wt`: its `platform/x.py` is not
        // `.claude/worktrees/wt/platform/x.py` below `platform`, nor is its
        // repository-relative path one `platform` stores.
        let (tmp, root, _) = project_with_worktree();
        let platform = root.join("platform");
        touch(&platform.join("x.py"));
        let nested = platform.join(".claude").join("worktrees").join("wt");
        link_worktree(&nested, &root.join(".git").join("worktrees").join("wt"));
        let beside = canonical(tmp.path()).unwrap().join("example-project-wt");
        link_worktree(&beside, &root.join(".git").join("worktrees").join("wt2"));
        for file in [nested.join("platform").join("x.py"), beside.join("platform").join("x.py")] {
            touch(&file);
            assert_eq!(got(&file, None, Some(&platform)), resolved(&s(&file), RootSource::Outside), "{}", s(&file));
        }
        assert_eq!(got(&platform.join("x.py"), None, Some(&platform)), resolved("x.py", RootSource::RegistryRoot));
    }

    #[test]
    fn another_repository_is_not_answered_for_when_the_namespace_has_a_root() {
        let (tmp, root, _) = project_with_worktree();
        let other = canonical(tmp.path()).unwrap().join("other-repo");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        let file = other.join("src").join("lib.rs");
        touch(&file);
        assert_eq!(got(&file, None, Some(&root)), resolved(&s(&file), RootSource::Outside));
    }

    #[test]
    fn a_neurostrata_folder_alone_is_not_a_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = canonical(tmp.path()).unwrap().join("home");
        std::fs::create_dir_all(home.join(".NeuroStrata")).unwrap();
        let file = home.join("scratch").join("notes.py");
        touch(&file);
        let above_has_git = std::iter::successors(Some(home.as_path()), |p| p.parent()).any(|p| p.join(".git").exists());
        if !above_has_git {
            assert_eq!(got(&file, None, None), resolved(&s(&file), RootSource::Outside));
        }
    }

    #[test]
    fn lookup_forms_add_the_absolute_spelling_both_ways() {
        let (_tmp, root, _) = project_with_worktree();
        let file = s(&root.join("src").join("lib.rs"));
        let l = to_project_relative(&file, None, None);
        let forms = lookup_forms(&file, &l, None);
        assert_eq!(forms[0], "src/lib.rs");
        assert!(forms.contains(&file), "{forms:?}");
        let rel = to_project_relative("src/lib.rs", None, None);
        let forms = lookup_forms("src/lib.rs", &rel, Some(&s(&root)));
        let joined = parse_absolute(&file).unwrap().to_path_string();
        assert_eq!(forms, vec!["src/lib.rs".to_string(), joined]);
        // No usable root, or a join that leaves the root: relative form only.
        assert_eq!(lookup_forms("src/lib.rs", &rel, Some("example-project")), vec!["src/lib.rs".to_string()]);
        let up = to_project_relative("../other/lib.rs", None, None);
        assert_eq!(lookup_forms("../other/lib.rs", &up, Some(&s(&root))), vec!["../other/lib.rs".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_matches_through_its_canonical_form() {
        let tmp = tempfile::tempdir().unwrap();
        let real = canonical(tmp.path()).unwrap().join("real-project");
        let file = real.join("src").join("lib.rs");
        touch(&file);
        let link = canonical(tmp.path()).unwrap().join("linked-project");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(got(&file, Some(&link), None), resolved("src/lib.rs", RootSource::ProjectRoot));
        assert_eq!(got(&link.join("src").join("lib.rs"), None, Some(&real)), resolved("src/lib.rs", RootSource::RegistryRoot));
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_root_spelled_in_another_case_or_verbatim_matches_on_disk() {
        let (_tmp, root, _) = project_with_worktree();
        let file = s(&root.join("src").join("lib.rs"));
        for given in [s(&root).to_uppercase(), format!(r"\\?\{}", s(&root)), s(&root).replace('\\', "/")] {
            let l = to_project_relative(&file, Some(&given), None);
            assert_eq!((l.path.as_str(), l.source), ("src/lib.rs", RootSource::ProjectRoot), "{given}");
        }
        // A file spelled in another case resolves to the on-disk case.
        let shouted = format!(r"{}\SRC\LIB.RS", s(&root).to_uppercase());
        let l = to_project_relative(&shouted, None, None);
        assert_eq!((l.path.as_str(), l.source), ("src/lib.rs", RootSource::Marker));
    }
}
