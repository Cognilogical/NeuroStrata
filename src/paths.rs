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
//!   and device (`\\.\`) prefixes are dropped. A UNC path spelled with
//!   backslashes (`\\server\share`) is Windows on any host. Any other pair of
//!   leading separators (`//`, `/\`, `\/`) is UNC only on a Windows host; on
//!   Linux and macOS it is the POSIX root `/`, as those systems read it. So a
//!   Windows client must spell a UNC path with backslashes for a daemon there.
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
    // POSIX leaves a leading `//` to the system; Linux and macOS read it as
    // `/`. Off Windows only two backslashes (`\\server`) are UNC: that is how
    // a Windows client spells it. Any other pair of leading separators (`//`,
    // `/\`, `\/`) is the POSIX root there.
    let lead = raw.as_bytes();
    // Win32 verbatim/device prefixes are absolute even though they begin with
    // `//`; they must not be mistaken for a POSIX double slash off Windows.
    let is_verbatim_prefix = raw.starts_with(r"\\?\")
        || raw.starts_with(r"\\.\")
        || raw.starts_with("//?/")
        || raw.starts_with("//./");
    let posix_double_slash = !is_verbatim_prefix
        && !cfg!(windows)
        && lead.len() >= 2
        && matches!(lead[0], b'/' | b'\\')
        && matches!(lead[1], b'/' | b'\\')
        && !(lead[0] == b'\\' && lead[1] == b'\\');
    let forward = raw.replace('\\', "/");
    let mut rest: &str = &forward;
    let mut unc = false;
    for verbatim in ["//?/", "//./"] {
        if posix_double_slash {
            break;
        }
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
    } else if unc || (rest.starts_with("//") && !posix_double_slash) {
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
    Some(AbsPath {
        windows,
        root,
        parts,
    })
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
    // Resolve `..` through symlinks before parsing: a link followed by `..`
    // names a different directory than lexical collapse would produce.
    let resolved = follow_parent_components(path);
    raw_host_path(&resolved)
}

/// [`host_path`] without parent-component resolution. `parse_absolute` is
/// still lexical, so this is only for [`follow_parent_components`] itself,
/// which must not recurse through `host_path`.
fn raw_host_path(path: &str) -> Option<PathBuf> {
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
    let resolved = follow_parent_components(path);
    let parsed = parse_absolute(&resolved)?;
    raw_host_path(&resolved)?;
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
    let gitdir = gitdir_of_file(&checkout)?;
    Some(linked_common_dir(&gitdir).unwrap_or(gitdir))
}

/// The gitdir a checkout's `.git` FILE names, made absolute against the
/// checkout (`worktree.useRelativePaths`, git 2.48+, writes it relative).
fn gitdir_of_file(checkout: &Path) -> Option<String> {
    let text = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let gitdir = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    Some(joined(checkout, gitdir))
}

/// The common directory a linked worktree's gitdir names in its `commondir`
/// file. `None` for any other gitdir: a submodule's, including one inside a
/// linked worktree (`<main>/.git/worktrees/<wt>/modules/<sub>`), has none.
fn linked_common_dir(gitdir: &str) -> Option<String> {
    let common = std::fs::read_to_string(Path::new(gitdir).join("commondir")).ok()?;
    Some(joined(Path::new(gitdir), common.trim()))
}

/// `rel` against `base` unless it is already absolute.
fn joined(base: &Path, rel: &str) -> String {
    if is_absolute_any(rel) {
        rel.to_string()
    } else {
        base.join(rel).to_string_lossy().to_string()
    }
}

/// Whether `a` and `b` name the same location: by spelling, else by their
/// canonical forms on this host.
fn same_location(a: &str, b: &str) -> bool {
    let lexical = |a: &str, b: &str| matches!((parse_absolute(a), parse_absolute(b)), (Some(a), Some(b)) if a == b);
    if lexical(a, b) {
        return true;
    }
    match (
        host_path(a).and_then(|p| canonical(&p)),
        host_path(b).and_then(|p| canonical(&p)),
    ) {
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

/// The git root of a DIRECTORY: `dir` itself when it holds `.git`, else its
/// nearest ancestor that does ([`find_git_root`] looks only above its input).
/// Returned in the same normalized spelling as [`find_git_root`] either way.
pub fn git_root_of_dir(dir: &str) -> Option<String> {
    let resolved = follow_parent_components(dir);
    if raw_host_path(&resolved).is_some_and(|d| d.join(".git").exists()) {
        Some(parse_absolute(&resolved)?.to_path_string())
    } else {
        find_git_root(&resolved)
    }
}

/// The main checkout of the git repository holding `dir` (a directory): `dir`'s
/// own `.git` root when that is a regular checkout, or, for a linked worktree,
/// the checkout that owns it. A submodule, a worktree of a bare repository,
/// and any worktree whose main checkout can't be found answer their own root.
/// `None` outside git. Returned in this host's plain spelling (`dunce`).
pub fn main_checkout_root(dir: &str) -> Option<PathBuf> {
    // Resolve links first: a relative gitdir read against a linked spelling
    // lands somewhere else, and a link can cross a checkout boundary.
    let dir = host_path(dir)
        .and_then(|d| canonical(&d))
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.to_string());
    let own_root = git_root_of_dir(&dir)?;
    let own = host_path(&own_root)?;
    let own = canonical(&own).unwrap_or(own);
    if own.join(".git").is_dir() {
        return Some(own);
    }
    // Only a linked worktree borrows a main checkout: its gitdir names the
    // shared directory in `commondir`. Not a submodule (no `commondir`,
    // wherever its gitdir lives).
    let main = gitdir_of_file(&own)
        .and_then(|gitdir| linked_common_dir(&gitdir))
        .and_then(|common| canonical(Path::new(&common)))
        .and_then(|common| main_checkout_of_common(&common));
    Some(main.unwrap_or(own))
}

/// The main checkout that owns the common git directory `common` (canonical):
/// `core.worktree` when set, else the parent of a common directory named
/// `.git`. `None` for a bare repository (`core.bare`, whatever the folder is
/// called), and when the parent holds nothing but that `.git`: that is how
/// `git init --separate-git-dir <dir>/.git <checkout>` leaves it, and git
/// records the real checkout nowhere in the common directory (git itself
/// then lists `<dir>` as the main worktree). A parent with only `.git` has
/// nothing for a worktree to borrow either way.
fn main_checkout_of_common(common: &Path) -> Option<PathBuf> {
    let core = core_config(common);
    if core.bare {
        return None;
    }
    if let Some(worktree) = core.worktree {
        // Relative to the git directory, as git reads it.
        let path = PathBuf::from(joined(common, &worktree));
        return canonical(&path).filter(|p| p.is_dir());
    }
    if common.file_name()? != ".git" {
        return None;
    }
    let parent = common.parent()?;
    // Ask git which checkout owns this git dir. It honours `core.bare` from
    // included config and reports the actual main worktree, so unrelated files
    // beside a separate git dir are never mistaken for a checkout.
    if let Some(main) = git_main_worktree(common) {
        if main != common && main.is_dir() {
            return Some(main);
        }
    }
    // A separate git dir with no core.worktree makes git call the gitdir itself
    // the worktree; synthetic dirs and hosts without git answer nothing at all.
    // A common dir named `.git` that holds HEAD is a real checkout, so its
    // parent owns it even when that parent holds nothing else. A `.git` dropped
    // beside unrelated data has no HEAD and owns no worktree.
    common.join("HEAD").exists().then(|| parent.to_path_buf())
}

/// The first (main) worktree git reports for the common git directory, or
/// `None` when git is unavailable or this is not a usable repository.
fn git_main_worktree(common: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(common)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let line = text.lines().find(|line| line.starts_with("worktree "))?;
    let raw = line.trim_start_matches("worktree ").trim();
    let path = host_path(raw)?;
    canonical(&path)
}

/// The `[core]` settings [`main_checkout_of_common`] reads.

#[derive(Default)]
struct CoreConfig {
    bare: bool,
    worktree: Option<String>,
    worktree_config: bool,
}

/// Parse one config file into `out`, walking `[include] path = ...`
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigSection {
    Other,
    Core,
    Include,
    Extensions,
}

/// Git config escapes: `\\`, `\"`, `\n`, `\t`, `\b`, octal `\nnn`, and a
/// backslash before any other character (which quotes it literally).
fn decode_git_config_escapes(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\x08'),
            Some(d @ '0'..='7') => {
                let mut value = d.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    let Some(next) = chars.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    value = value * 8 + next;
                    chars.next();
                }
                if let Some(ch) = char::from_u32(value) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Strip an unquoted inline comment and surrounding quotes, then decode
/// escapes. `;` and `#` start a comment only at the start or after whitespace.
fn git_config_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (body, quoted) = if let Some(rest) = raw.strip_prefix('"') {
        let mut escaped = false;
        let mut end = None;
        for (i, c) in rest.char_indices() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = Some(i);
                break;
            }
        }
        match end {
            Some(i) => (&rest[..i], true),
            None => (raw, false),
        }
    } else {
        (raw, false)
    };
    let body = if quoted {
        body
    } else {
        let bytes = body.as_bytes();
        let mut end = body.len();
        for (i, byte) in bytes.iter().enumerate() {
            if matches!(byte, b'#' | b';') && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
                end = i;
                break;
            }
        }
        &body[..end]
    };
    Some(decode_git_config_escapes(body.trim_end()))
}

/// Parse one config file into `out`, following includes at their point of
/// appearance and carrying the section context across them, as git does.
fn parse_core_config(path: &Path, out: &mut CoreConfig, section: &mut ConfigSection, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            let name = header.split(']').next().unwrap_or("").trim();
            let key = name.split_whitespace().next().unwrap_or("");
            *section = if key.eq_ignore_ascii_case("core") {
                ConfigSection::Core
            } else if key.eq_ignore_ascii_case("include") || key.eq_ignore_ascii_case("includeif") {
                ConfigSection::Include
            } else if key.eq_ignore_ascii_case("extensions") {
                ConfigSection::Extensions
            } else {
                ConfigSection::Other
            };
            continue;
        }
        if line.starts_with(['#', ';']) {
            continue;
        }
        let (key, raw) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), Some(v.trim())),
            None => (line, None),
        };
        if *section == ConfigSection::Include {
            if key.eq_ignore_ascii_case("path") {
                if let Some(inc_path) = raw.and_then(git_config_value) {
                    let include = if is_absolute_any(&inc_path) {
                        host_path(&inc_path)
                    } else {
                        path.parent().and_then(|parent| {
                            let combined = parent.join(&inc_path).to_string_lossy().to_string();
                            host_path(&combined)
                        })
                    };
                    if let Some(include) = include {
                        parse_core_config(&include, out, section, depth + 1);
                    }
                }
            }
            continue;
        }
        let truthy = |v: Option<String>| {
            v.is_none_or(|v| {
                ["true", "yes", "on", "1"]
                    .iter()
                    .any(|t| v.eq_ignore_ascii_case(t))
            })
        };
        match *section {
            ConfigSection::Core if key.eq_ignore_ascii_case("bare") => {
                out.bare = truthy(raw.and_then(git_config_value));
            }
            ConfigSection::Core if key.eq_ignore_ascii_case("worktree") => {
                out.worktree = raw.and_then(git_config_value).filter(|v| !v.is_empty());
            }
            ConfigSection::Extensions if key.eq_ignore_ascii_case("worktreeconfig") => {
                out.worktree_config = truthy(raw.and_then(git_config_value));
            }
            _ => {}
        }
    }
}

/// Effective `core.bare` / `core.worktree`: `<common>/config` plus the
/// worktree-specific config when `extensions.worktreeConfig` is enabled,
/// with includes resolved at their point of appearance.
fn core_config(common: &Path) -> CoreConfig {
    let mut out = CoreConfig {
        bare: false,
        worktree: None,
        worktree_config: false,
    };
    let mut section = ConfigSection::Other;
    parse_core_config(&common.join("config"), &mut out, &mut section, 0);
    if out.worktree_config {
        section = ConfigSection::Other;
        parse_core_config(&common.join("config.worktree"), &mut out, &mut section, 0);
    }
    out
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
    /// The root of the project marker (`.NeuroStrata/config.json`) for the
    /// call's namespace, when it is also the namespace's cached root
    /// ([`with_marker_spelling`]), matched when its namespace matches the
    /// caller's.
    RootConfig,
    /// Absolute, but under no root this namespace knows: left unchanged.
    Outside,
}

impl RootSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RootSource::Relative => "relative",
            RootSource::ProjectRoot => "project_root",
            RootSource::RegistryRoot => "registry_root",
            RootSource::RootConfig => "root_config",
            RootSource::Marker => "marker",
            RootSource::RootConfig => "root_config",
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
    /// For a resolved absolute path, the other spellings it may be stored
    /// under, after `path`: the part below the root as the caller spelled it
    /// (when the on-disk spelling differs, as through a symlinked file), then
    /// the absolute path itself. Empty for `Relative` and `Outside`.
    pub alternates: Vec<String>,
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
/// not this project: a nested clone or submodule is never resolved against
/// that candidate, but only vetoes it — the lookup falls through to the next
/// known root. A linked worktree inside the root resolves against the worktree
/// only when the root is the top of a checkout of the same repository. Under a
/// project registered at a subdirectory (`repo/platform`), stored paths are
/// relative to that subdirectory, which another checkout's paths would not
/// match.
///
/// Step 3 with a known root (a supplied `project_root`, else a usable
/// registered root) applies the same rule: another checkout of that repository
/// resolves only when the known root is a checkout root; anything else is
/// another project and stays `Outside`, so it never borrows this namespace's
/// memories. A supplied `project_root` vetoes here even when it is unusable on
/// this host (a moved root), while an unusable registered root is ignored.
///
/// When the file exists, the part below the root is taken from its canonical
/// path, so it carries the on-disk spelling the store matches exactly.
pub fn to_project_relative(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> LookupPath {
    if !is_absolute_any(raw) {
        return LookupPath {
            path: raw.to_string(),
            source: RootSource::Relative,
            root: None,
            alternates: Vec::new(),
        };
    }
    let caller_raw = raw.replace('\\', "/");
    let input = follow_parent_components(raw);
    match choose_root(&input, project_root, registry_root) {
        Some((root, rel, source)) => {
            let path = with_disk_spelling(&input, &root, rel.clone());
            let mut alternates = vec![rel.clone(), input.clone()];
            alternates.extend(parse_absolute(&input).map(|p| p.to_path_string()));
            // A legacy memory may be stored under the caller's literal `..`
            // spelling; keep it queryable alongside the resolved form.
            if !caller_raw.is_empty() && caller_raw != rel && !alternates.contains(&caller_raw) {
                alternates.push(caller_raw);
            }
            // A broader `project_root` wins for `path`/`source`, but a deeper
            // usable registry root may hold the file under a shorter relative
            // spelling. Keep that spelling as an alternate — only when the
            // root resolves the file and is not vetoed by a nested foreign
            // checkout — without changing the primary resolution.
            if source == RootSource::ProjectRoot {
                if let Some(reg_root) = registry_root.filter(|r| is_usable_root(r)) {
                    let marker = find_git_root(&input);
                    let marker_rel = marker.as_deref().and_then(|m| relative_to(&input, m));
                    if let Some((reg_resolved_root, reg_rel, _)) = resolve_candidate_root(
                        &input,
                        reg_root,
                        RootSource::RegistryRoot,
                        &marker,
                        &marker_rel,
                    ) {
                        if depth(&reg_rel) < depth(&rel) {
                            alternates.push(with_disk_spelling(
                                &input,
                                &reg_resolved_root,
                                reg_rel.clone(),
                            ));
                            alternates.push(reg_rel);
                        }
                    }
                }
            }
            let mut seen = std::collections::HashSet::new();
            alternates.retain(|a| seen.insert(crate::parser::ingest::normalize_node_path(a)));
            LookupPath {
                path,
                source,
                root: Some(root),
                alternates,
            }
        }
        None => LookupPath {
            path: raw.to_string(),
            source: RootSource::Outside,
            root: None,
            alternates: Vec::new(),
        },
    }
}

/// Whether `marker`'s root is `cached_root`, the namespace's cached registry
/// root. A worktree's root matches in its main checkout's spelling (the
/// registry caches it that way) or as itself (a root cached at the
/// worktree), both sides compared canonically, so an alias of the same
/// folder matches. No cached root, or one missing here, never matches.
pub fn marker_root_is_cached_root(
    marker: &crate::root_marker::FoundMarker,
    cached_root: Option<&str>,
) -> bool {
    let Some(cached) = cached_root.and_then(host_path).and_then(|p| canonical(&p)) else {
        return false;
    };
    let mapped = crate::root_marker::in_main_checkout(&marker.root);
    [mapped, marker.root.clone()]
        .into_iter()
        .any(|root| canonical(&root).unwrap_or(root) == cached)
}

/// [`with_marker_spelling`] after checking that `marker` names
/// `call_namespace` (ignoring case) and that its root is the cached root
/// ([`marker_root_is_cached_root`]); in every other case `lookup` comes back
/// unchanged. Tests use it to state those rules in one call.
#[cfg(test)]
pub fn with_root_config(
    lookup: LookupPath,
    input: &str,
    marker: &crate::root_marker::FoundMarker,
    cached_root: Option<&str>,
    call_namespace: &str,
) -> LookupPath {
    if !marker.config.namespace.eq_ignore_ascii_case(call_namespace)
        || !marker_root_is_cached_root(marker, cached_root)
    {
        return lookup;
    }
    with_marker_spelling(lookup, input, marker)
}

/// Add the project marker's spelling to a file lookup of absolute `input`
/// (spec 6.2), for a caller that has already checked that `marker` names
/// the call's namespace (under `server::resolve_namespace`'s case rules)
/// and that its root is the cached root ([`marker_root_is_cached_root`]).
/// Unless `lookup` is `Outside` or `Relative`, the path below the marker
/// root, in its on-disk spelling, becomes `path`, and today's `path` and
/// `alternates` follow in order. The
/// marker root is the cached root, so this is a key today's resolution could
/// already produce for the file; it only stops depending on the caller's
/// `project_root`. In every other case `lookup` comes back unchanged.
pub fn with_marker_spelling(
    lookup: LookupPath,
    input: &str,
    marker: &crate::root_marker::FoundMarker,
) -> LookupPath {
    if matches!(lookup.source, RootSource::Outside | RootSource::Relative)
        || !is_absolute_any(input)
    {
        return lookup;
    }
    let input = follow_parent_components(input);
    let root = marker.root.to_string_lossy().to_string();
    let Some(rel) = relative_to_on_disk(&input, &root) else {
        return lookup;
    };
    let path = with_disk_spelling(&input, &root, rel);
    let mut alternates = vec![lookup.path];
    alternates.extend(lookup.alternates);
    let mut seen = std::collections::HashSet::new();
    seen.insert(crate::parser::ingest::normalize_node_path(&path));
    alternates.retain(|a| seen.insert(crate::parser::ingest::normalize_node_path(a)));
    LookupPath {
        path,
        source: RootSource::RootConfig,
        root: Some(root),
        alternates,
    }
}

/// `raw` with its `..` components resolved by the filesystem when the path
/// exists. The OS follows a directory symlink before `..`, so
/// `/repo/link/../f.rs` with `link` pointing at `/repo/a/b` names
/// `/repo/a/f.rs`; collapsing `..` by spelling would name `/repo/f.rs`, another
/// file. Only the directory part is resolved, so the file keeps its own name
/// (a symlinked file is not replaced by its target here) and one not created
/// yet still resolves. Without a `..` component, or when the directory does
/// not exist here, `raw` is returned and `..` is collapsed by spelling as
/// before.
fn follow_parent_components(raw: &str) -> String {
    if !raw.split(['/', '\\']).any(|c| c == "..") {
        return raw.to_string();
    }
    if raw_host_path(raw).is_none() {
        return raw.to_string();
    }
    let host = PathBuf::from(raw);
    // Walk existing ancestors longest-first. The OS resolves a directory
    // symlink before `..`, so canonicalizing the longest surviving prefix
    // pins the real parent even when a later component does not exist yet.
    // Skip the file itself: a symlinked file keeps its own name here.
    for ancestor in host.ancestors().skip(1) {
        if let Some(dir) = canonical(ancestor) {
            let tail = host
                .strip_prefix(ancestor)
                .unwrap_or_else(|_| Path::new(""));
            let mut resolved = dir;
            resolved.push(tail);
            return resolved.to_string_lossy().to_string();
        }
    }
    raw.to_string()
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

/// Evaluate a single candidate root for absolute `raw`: the part below it and
/// where the root came from, or `None` when this candidate cannot answer.
/// A nested foreign checkout vetoes only this candidate — the exact rule
/// shared by [`choose_root`] and the deeper-alternate retention in
/// [`to_project_relative`]. The candidate itself is never usability-filtered
/// here (a supplied `project_root` vetoes even when moved/unusable); callers
/// filter registry roots before passing them in.
fn resolve_candidate_root(
    raw: &str,
    root: &str,
    source: RootSource,
    marker: &Option<String>,
    marker_rel: &Option<String>,
) -> Option<(String, String, RootSource)> {
    let rel = relative_to_on_disk(raw, root)?;
    match (marker, marker_rel) {
        // A checkout nested inside the root (deeper than it). A foreign one
        // vetoes only this candidate; a later known root may answer.
        (Some(inner), Some(inner_rel)) if depth(inner_rel) < depth(&rel) => {
            (is_checkout_root(root) && same_repository(inner, root))
                .then(|| (inner.clone(), inner_rel.clone(), RootSource::Marker))
        }
        _ => Some((root.to_string(), rel, source)),
    }
}

/// The root that relativizes absolute `raw`, the part below it, and where the
/// root came from; `None` when no root this namespace knows contains it.
/// Candidates are tried in order and a nested foreign checkout vetoes only the
/// candidate it is nested in, so the lookup falls through to a later known
/// root before the `.git` fallback. That fallback is still vetoed by a supplied
/// `project_root` even when unusable here; the registry root is usable-filtered.
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
        if let Some(resolved) = resolve_candidate_root(raw, root, source, &marker, &marker_rel) {
            return Some(resolved);
        }
    }
    let (marker, marker_rel) = (marker?, marker_rel?);
    // A supplied root constrains the fallback even when unusable here: a moved
    // root still vetoes another repository. The registry root was filtered
    // above, so an unusable cached root remains ignored.
    let known_root = project_root.or(registry_root);
    match known_root {
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
/// - a resolved absolute path is also queried as the caller spelled it below
///   the root, and in its absolute spelling ([`LookupPath::alternates`]);
/// - a relative path is also queried joined onto `known_root` (when it is
///   usable here and the join stays inside it);
/// - an `Outside` path is queried only as given.
pub fn lookup_forms(raw: &str, lookup: &LookupPath, known_root: Option<&str>) -> Vec<String> {
    let mut forms = vec![lookup.path.clone()];
    match lookup.source {
        RootSource::Outside => {}
        RootSource::Relative => {
            let has_parent = raw.split(['/', '\\']).any(|c| c == "..");
            let joined = known_root.filter(|r| is_usable_root(r)).and_then(|root| {
                let normalized = raw.replace('\\', "/");
                let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
                // Resolve `..` on disk before parsing: across a symlinked
                // directory the lexical join names a different file.
                let joined_raw = format!("{}/{}", root.trim_end_matches('/'), normalized);
                let joined = if has_parent {
                    follow_parent_components(&joined_raw)
                } else {
                    joined_raw
                };
                let joined = parse_absolute(&joined)?.to_path_string();
                relative_to(&joined, root).map(|_| joined)
            });
            if let Some(joined) = joined {
                if has_parent {
                    // The raw spelling may cross a symlink; also query the
                    // on-disk target under its project-relative name.
                    if let Some(rel) = known_root
                        .filter(|r| is_usable_root(r))
                        .and_then(|root| relative_to(&joined, root))
                    {
                        if rel != raw {
                            forms.push(rel);
                        }
                    }
                }
                forms.push(joined);
            }
        }
        RootSource::ProjectRoot
        | RootSource::RegistryRoot
        | RootSource::RootConfig
        | RootSource::Marker => {
            forms.extend(lookup.alternates.iter().cloned());
        }
    }
    let mut seen = std::collections::HashSet::new();
    forms.retain(|f| seen.insert(crate::parser::ingest::normalize_node_path(f)));
    forms
}

/// Whether a path names a single file or a directory prefix. Prefixes accept
/// an empty raw input (they resolve to no forms); files do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathResolutionScope {
    File,
    Prefix,
}

/// The scoped lookup forms for a raw path: every spelling a store query must
/// try, plus the normalized first form used as its canonical identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPathForms {
    pub lookup: LookupPath,
    pub forms: Vec<String>,
    pub canonical_form: Option<String>,
}

/// An absolute path (or prefix) that is under no root this namespace knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvablePathError {
    pub input: String,
    pub reason: String,
}

impl std::fmt::Display for UnresolvablePathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unresolvable path {} (outside every expected root): {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for UnresolvablePathError {}

/// The explicit error for an absolute path outside every expected root. The
/// message names the expected root(s) so a caller can see what would make the
/// path resolvable.
pub fn unresolvable_absolute_path_error(
    input: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> UnresolvablePathError {
    let mut roots: Vec<&str> = Vec::new();
    if let Some(root) = project_root {
        roots.push(root);
    }
    if let Some(root) = registry_root {
        roots.push(root);
    }
    let reason = if roots.is_empty() {
        "no project root is registered".to_string()
    } else {
        format!("expected root(s): {}", roots.join(", "))
    };
    UnresolvablePathError {
        input: input.to_string(),
        reason,
    }
}

/// When a directory prefix equals a known root (`project_root`, usable
/// `registry_root`, or an unvetoed `.git` checkout root), resolve it to `"."`
/// under that root so prefix queries over the root directory match both
/// relative and absolute stored paths inside that root.
fn resolve_prefix_root_lookup(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> Option<LookupPath> {
    if !is_absolute_any(raw) {
        return None;
    }
    let input = follow_parent_components(raw);
    let registry_root = registry_root.filter(|root| is_usable_root(root));
    let matched = [
        (project_root, RootSource::ProjectRoot),
        (registry_root, RootSource::RegistryRoot),
    ]
    .into_iter()
    .find_map(|(root, source)| {
        let root = root?;
        same_location(&input, root).then(|| (root.to_string(), source))
    })
    .or_else(|| {
        if !is_checkout_root(&input) {
            return None;
        }
        let marker = parse_absolute(&input)
            .map(|p| p.to_path_string())
            .unwrap_or_else(|| input.clone());
        let known_root = project_root.or(registry_root);
        match known_root {
            None => Some((marker, RootSource::Marker)),
            Some(root) if is_checkout_root(root) && same_repository(&marker, root) => {
                Some((marker, RootSource::Marker))
            }
            Some(_) => None,
        }
    });
    let (root, source) = matched?;
    let mut alternates = Vec::new();
    alternates.extend(parse_absolute(&root).map(|p| p.to_path_string()));
    alternates.push(root.clone());
    alternates.extend(parse_absolute(&input).map(|p| p.to_path_string()));
    alternates.push(input);
    let mut seen = std::collections::HashSet::new();
    alternates.retain(|a| seen.insert(crate::parser::ingest::normalize_node_path(a)));
    Some(LookupPath {
        path: ".".to_string(),
        source,
        root: Some(root),
        alternates,
    })
}

/// Every store-query spelling for `raw` at the given scope. Beyond
/// [`lookup_forms`], a prefix also carries its normalized relative form and
/// the absolute forms joined onto the known roots. Only joins that stay
/// inside their root (or equal the root for a root-level prefix) are added,
/// so the scope never widens to a sibling or parent directory.
pub fn lookup_forms_for_scope(
    raw: &str,
    lookup: &LookupPath,
    known_root: Option<&str>,
    scope: PathResolutionScope,
) -> Vec<String> {
    if scope == PathResolutionScope::Prefix && raw.trim().is_empty() {
        return Vec::new();
    }
    let mut forms = lookup_forms(raw, lookup, known_root);
    if scope == PathResolutionScope::Prefix {
        let normalized = crate::parser::ingest::normalize_node_path(&lookup.path);
        if !normalized.is_empty() {
            forms.push(normalized);
        }
        let mut roots: Vec<&str> = Vec::new();
        if let Some(root) = lookup.root.as_deref() {
            roots.push(root);
        }
        if let Some(root) = known_root {
            roots.push(root);
        }
        let has_parent_traversal = lookup.path.split(['/', '\\']).any(|c| c == "..");
        for root in roots {
            if !is_usable_root(root) {
                continue;
            }
            let joined_raw = format!(
                "{}/{}",
                root.trim_end_matches('/'),
                lookup.path.trim_start_matches('/')
            );
            let joined = if has_parent_traversal {
                follow_parent_components(&joined_raw)
            } else {
                joined_raw
            };
            let Some(joined) = parse_absolute(&joined).map(|p| p.to_path_string()) else {
                continue;
            };
            if (!has_parent_traversal && same_location(&joined, root))
                || relative_to(&joined, root).is_some()
            {
                forms.push(joined);
            }
        }
        let mut seen = std::collections::HashSet::new();
        forms.retain(|f| seen.insert(crate::parser::ingest::normalize_node_path(f)));
    }
    forms
}

/// Resolve `raw` into every spelling it may be stored under at the given
/// scope. An absolute path or prefix under no known root is an explicit
/// [`UnresolvablePathError`] — it is never silently widened to a different
/// repository. A relative path additionally gets a joined form under a
/// distinct usable registry root so either stored spelling is found.
pub fn resolve_in_scope_forms(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
    scope: PathResolutionScope,
) -> Result<ResolvedPathForms, UnresolvablePathError> {
    if scope == PathResolutionScope::Prefix && raw.trim().is_empty() {
        return Ok(ResolvedPathForms {
            lookup: to_project_relative(raw, project_root, registry_root),
            forms: Vec::new(),
            canonical_form: None,
        });
    }
    let mut lookup = to_project_relative(raw, project_root, registry_root);
    if lookup.source == RootSource::Outside && scope == PathResolutionScope::Prefix {
        if let Some(root_lookup) = resolve_prefix_root_lookup(raw, project_root, registry_root) {
            lookup = root_lookup;
        }
    }
    if lookup.source == RootSource::Outside {
        return Err(unresolvable_absolute_path_error(
            raw,
            project_root,
            registry_root,
        ));
    }
    let known_root = project_root.or(registry_root.filter(|r| is_usable_root(r)));
    let mut forms = lookup_forms_for_scope(raw, &lookup, known_root, scope);
    if lookup.source == RootSource::Relative {
        if let (Some(project), Some(reg)) =
            (project_root, registry_root.filter(|r| is_usable_root(r)))
        {
            if reg != project {
                let has_parent_traversal = raw.split(['/', '\\']).any(|c| c == "..");
                let joined_raw = format!(
                    "{}/{}",
                    reg.trim_end_matches('/'),
                    raw.trim_start_matches('/')
                );
                let joined = if has_parent_traversal {
                    follow_parent_components(&joined_raw)
                } else {
                    joined_raw
                };
                if let Some(joined) = parse_absolute(&joined).map(|p| p.to_path_string()) {
                    if (scope == PathResolutionScope::Prefix
                        && !has_parent_traversal
                        && same_location(&joined, reg))
                        || relative_to(&joined, reg).is_some()
                    {
                        if has_parent_traversal {
                            if let Some(rel) = relative_to(&joined, reg) {
                                if rel != raw {
                                    forms.push(rel);
                                }
                            }
                        }
                        forms.push(joined);
                    }
                }
                let mut seen = std::collections::HashSet::new();
                forms.retain(|f| seen.insert(crate::parser::ingest::normalize_node_path(f)));
            }
        }
    }
    let canonical_form = forms
        .first()
        .map(|f| crate::parser::ingest::normalize_node_path(f))
        .filter(|s| !s.is_empty());
    Ok(ResolvedPathForms {
        lookup,
        forms,
        canonical_form,
    })
}

/// Every spelling to query for a directory prefix, or an explicit error when
/// the prefix is absolute but outside every known root.
pub fn prefix_lookup_forms(
    raw: &str,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> Result<Vec<String>, UnresolvablePathError> {
    resolve_in_scope_forms(
        raw,
        project_root,
        registry_root,
        PathResolutionScope::Prefix,
    )
    .map(|resolved| resolved.forms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// #374 (finding w): a non-bare main checkout holding only `.git` is still
    /// the main checkout a linked worktree borrows.
    #[test]
    fn real_worktree_of_only_dot_git_main_checkout_uses_main_folder_name() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let main = base.join("the-main-project");
        run_git(&base, &["init", "-q", "the-main-project"]);
        std::fs::write(main.join("README"), "x").unwrap();
        run_git(&main, &["add", "README"]);
        run_git(&main, &["commit", "-qm", "init"]);
        let wt = base.join("linked");
        run_git(
            &main,
            &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
        );
        // Leave the main checkout holding only `.git`.
        std::fs::remove_file(main.join("README")).unwrap();
        assert_eq!(std::fs::read_dir(&main).unwrap().count(), 1);
        assert_eq!(
            main_checkout_of_common(&main.join(".git")),
            Some(main.clone())
        );
        assert_eq!(main_checkout_root(&s(&wt)), Some(main));
    }

    /// #374 (findings v/Hw): a bare repo whose `core.bare` comes from an
    /// included config must not let a linked worktree borrow its parent.
    #[test]
    fn real_bare_repo_via_included_config_is_not_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let common = base.join("project").join(".git");
        run_git(&base, &["init", "-q", "--bare", common.to_str().unwrap()]);
        // Move `core.bare` into an included file, leaving the main config false.
        std::fs::write(common.join("included.config"), "[core]\n\tbare = true\n").unwrap();
        let cfg = std::fs::read_to_string(common.join("config")).unwrap();
        let cfg = cfg
            .lines()
            .filter(|l| !l.contains("bare"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            common.join("config"),
            format!("{cfg}\n[include]\n\tpath = included.config\n"),
        )
        .unwrap();
        let wt = base.join("project").join("main");
        run_git(
            &common,
            &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
        );
        assert_eq!(main_checkout_of_common(&common), None);
        assert_eq!(main_checkout_root(&s(&wt)), Some(wt));
    }

    /// #374 (finding Hw): effective `core.bare`/`core.worktree` can come from
    /// an included config; reading only `<common>/config` misses it.
    #[test]
    fn core_config_follows_includes() {
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path().to_path_buf();
        std::fs::write(
            common.join("config"),
            "[include]\n\tpath = extra.config\n[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        std::fs::write(
            common.join("extra.config"),
            "[core]\n\tbare = true\n\tworktree = /tmp/from-include\n",
        )
        .unwrap();
        let core = core_config(&common);
        assert!(core.bare, "included core.bare must be honored");
        assert_eq!(core.worktree.as_deref(), Some("/tmp/from-include"));
    }

    /// #374 (finding Hw): a bounded include cycle cannot hang resolution.
    #[test]
    fn core_config_include_cycle_is_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path().to_path_buf();
        std::fs::write(
            common.join("config"),
            "[include]\n\tpath = config\n[core]\n\tbare = false\n",
        )
        .unwrap();
        let core = core_config(&common);
        assert!(!core.bare);
    }

    // ---- pure, host-independent ----

    #[test]
    fn relative_input_is_returned_byte_for_byte() {
        for raw in [
            "src/lib.rs",
            r"src\lib.rs",
            "./src/lib.rs",
            "lib.rs",
            "C:lib.rs",
            "",
        ] {
            let got = to_project_relative(raw, Some("/home/me/example-project"), Some(r"C:\x"));
            assert_eq!(
                got,
                LookupPath {
                    path: raw.to_string(),
                    source: RootSource::Relative,
                    root: None,
                    alternates: Vec::new(),
                }
            );
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
    fn a_backslash_unc_path_is_windows_on_every_host() {
        let unc = parse_absolute(r"\\server\share\example-project\src\lib.rs").unwrap();
        assert!(unc.windows);
        assert_eq!(unc.root, "//server/share");
    }

    #[test]
    #[cfg(not(windows))]
    fn a_forward_double_slash_is_the_posix_root_off_windows() {
        // Linux and macOS read `//repo/src/lib.rs` as `/repo/src/lib.rs`.
        let parsed = parse_absolute("//repo/src/lib.rs").unwrap();
        assert!(!parsed.windows);
        assert_eq!(parsed.to_path_string(), "/repo/src/lib.rs");
        assert_eq!(
            relative_to("//repo/src/lib.rs", "/repo").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            relative_to("/repo/src/lib.rs", "//repo").as_deref(),
            Some("src/lib.rs")
        );
        // Mixed separators follow the same rule; two backslashes stay UNC.
        for mixed in [r"/\repo/src/lib.rs", r"\/repo/src/lib.rs"] {
            assert!(!parse_absolute(mixed).unwrap().windows, "{mixed}");
        }
        assert!(parse_absolute(r"\\server\share\x").unwrap().windows);
    }

    #[test]
    #[cfg(windows)]
    fn a_forward_double_slash_is_unc_on_windows() {
        let parsed = parse_absolute("//server/share/src/lib.rs").unwrap();
        assert!(parsed.windows);
        assert_eq!(parsed.root, "//server/share");
        assert_eq!(
            relative_to("//server/share/src/lib.rs", r"\\server\share").as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn windows_spellings_strip_across_separators_verbatim_and_drive_case() {
        let root = r"C:\dev\projects\example-project\platform";
        for path in [
            r"C:\dev\projects\example-project\platform\renderer\x.py",
            "c:/dev/projects/example-project/platform/renderer/x.py",
            r"\\?\C:\dev\projects\example-project\platform\renderer\x.py",
        ] {
            assert_eq!(
                relative_to(path, root).as_deref(),
                Some("renderer/x.py"),
                "{path}"
            );
        }
        assert_eq!(
            relative_to(r"C:\dev\a\b.rs", r"\\?\c:\dev\").as_deref(),
            Some("a/b.rs")
        );
        assert_eq!(
            relative_to(
                r"\\?\UNC\server\share\example-project\src\lib.rs",
                r"\\server\share\example-project"
            )
            .as_deref(),
            Some("src/lib.rs")
        );
        // Separators and drive letters are normalized, other case is not: only
        // the file system can say whether `Dev` is `dev`.
        assert_eq!(relative_to(r"C:\Dev\a.rs", r"C:\dev"), None);
        // Another drive, a sibling with a shared name prefix, the root itself.
        assert_eq!(relative_to(r"D:\dev\a.rs", r"C:\dev"), None);
        assert_eq!(
            relative_to(r"C:\dev\projects2\a.rs", r"C:\dev\projects"),
            None
        );
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
            relative_to(
                "/home/me/example-project/./src/../src/lib.rs",
                "/home/me/example-project/"
            )
            .as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            relative_to("/home/me/Example-Project/src/lib.rs", root),
            None
        );
        assert_eq!(
            relative_to("/home/me/example-project2/src/lib.rs", root),
            None
        );
        // Traversal out of the root is resolved first, so it does not match.
        assert_eq!(
            relative_to("/home/me/example-project/../other/lib.rs", root),
            None
        );
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
        assert_eq!(
            got,
            LookupPath {
                path: raw.into(),
                source: RootSource::Outside,
                root: None,
                alternates: Vec::new(),
            }
        );
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
        std::fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n\tbare = false\n").unwrap();
        touch(&root.join("src").join("lib.rs"));
        let worktree = root.join(".claude").join("worktrees").join("agent-1");
        touch(&worktree.join("src").join("lib.rs"));
        link_worktree(
            &worktree,
            &root.join(".git").join("worktrees").join("agent-1"),
        );
        (tmp, root, worktree)
    }

    /// What `git worktree add` writes: the checkout's `.git` file naming its
    /// gitdir, and the gitdir's `commondir` pointing back at the shared `.git`.
    fn link_worktree(worktree: &Path, gitdir: &Path) {
        std::fs::create_dir_all(gitdir).unwrap();
        std::fs::create_dir_all(worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", s(gitdir).replace('\\', "/")),
        )
        .unwrap();
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

    /// Forms are deduplicated by their normalized (`/`) spelling, so on
    /// Windows a backslash spelling may be stored in its slash form.
    fn has_form(forms: &[String], want: &str) -> bool {
        let want = crate::parser::ingest::normalize_node_path(want);
        forms
            .iter()
            .any(|f| crate::parser::ingest::normalize_node_path(f) == want)
    }

    fn resolved(path: &str, source: RootSource) -> (String, RootSource) {
        (path.to_string(), source)
    }

    fn got(
        raw: &Path,
        project_root: Option<&Path>,
        registry: Option<&Path>,
    ) -> (String, RootSource) {
        let p = project_root.map(s);
        let r = registry.map(s);
        let l = to_project_relative(&s(raw), p.as_deref(), r.as_deref());
        (l.path, l.source)
    }

    #[test]
    fn git_fallback_finds_the_root_without_any_root_argument() {
        let (_tmp, root, _) = project_with_worktree();
        assert_eq!(
            got(&root.join("src").join("lib.rs"), None, None),
            resolved("src/lib.rs", RootSource::Marker)
        );
        // A file about to be created resolves the same way.
        let new_file = root.join("src").join("new").join("mod.rs");
        assert_eq!(
            got(&new_file, None, None),
            resolved("src/new/mod.rs", RootSource::Marker)
        );
    }

    #[test]
    fn an_unusable_registered_root_does_not_block_the_git_fallback() {
        // A root cached as a relative path, or one from another machine, is
        // ignored; it must not veto the `.git` fallback.
        let (_tmp, root, _) = project_with_worktree();
        let file = s(&root.join("src").join("lib.rs"));
        for cached in [
            "example-project",
            "./src",
            ".",
            "/nonexistent-ns-lookup/example-project",
            r"Q:\nonexistent\example-project",
        ] {
            let l = to_project_relative(&file, None, Some(cached));
            assert_eq!(
                (l.path.as_str(), l.source),
                ("src/lib.rs", RootSource::Marker),
                "{cached}"
            );
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
        assert_eq!(
            got(&file, None, Some(&platform)),
            resolved("renderer/x.py", RootSource::RegistryRoot)
        );
    }

    #[test]
    fn a_checkout_root_registration_resolves_its_own_linked_worktrees() {
        let (tmp, root, worktree) = project_with_worktree();
        let inside = worktree.join("src").join("lib.rs");
        assert_eq!(
            got(&inside, None, Some(&root)),
            resolved("src/lib.rs", RootSource::Marker)
        );
        assert_eq!(
            got(&inside, Some(&root), None),
            resolved("src/lib.rs", RootSource::Marker)
        );
        let beside = canonical(tmp.path()).unwrap().join("example-project-wt");
        touch(&beside.join("src").join("lib.rs"));
        link_worktree(&beside, &root.join(".git").join("worktrees").join("wt"));
        assert_eq!(
            got(&beside.join("src").join("lib.rs"), None, Some(&root)),
            resolved("src/lib.rs", RootSource::Marker)
        );
        // A registered worktree resolves the main checkout and its siblings.
        for checkout in [&root, &beside] {
            assert_eq!(
                got(&checkout.join("src").join("lib.rs"), None, Some(&worktree)),
                resolved("src/lib.rs", RootSource::Marker)
            );
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
            for (project_root, registry) in [(None, Some(&root)), (Some(&root), None), (None, None)]
            {
                let expected = if project_root.is_none() && registry.is_none() {
                    // No root known: the nested repository is the only root.
                    resolved("src/lib.rs", RootSource::Marker)
                } else {
                    resolved(&s(&file), RootSource::Outside)
                };
                assert_eq!(
                    got(
                        &file,
                        project_root.map(|p| p.as_path()),
                        registry.map(|p| p.as_path())
                    ),
                    expected,
                    "{}",
                    s(&file)
                );
            }
            // From a registered worktree, the main checkout's submodule is
            // still another repository.
            assert_eq!(
                got(&file, None, Some(&worktree)),
                resolved(&s(&file), RootSource::Outside)
            );
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
        for file in [
            nested.join("platform").join("x.py"),
            beside.join("platform").join("x.py"),
        ] {
            touch(&file);
            assert_eq!(
                got(&file, None, Some(&platform)),
                resolved(&s(&file), RootSource::Outside),
                "{}",
                s(&file)
            );
        }
        assert_eq!(
            got(&platform.join("x.py"), None, Some(&platform)),
            resolved("x.py", RootSource::RegistryRoot)
        );
    }

    #[test]
    fn another_repository_is_not_answered_for_when_the_namespace_has_a_root() {
        let (tmp, root, _) = project_with_worktree();
        let other = canonical(tmp.path()).unwrap().join("other-repo");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        let file = other.join("src").join("lib.rs");
        touch(&file);
        assert_eq!(
            got(&file, None, Some(&root)),
            resolved(&s(&file), RootSource::Outside)
        );
        // The caller's project_root constrains it the same way, with or
        // without a registered root.
        assert_eq!(
            got(&file, Some(&root), None),
            resolved(&s(&file), RootSource::Outside)
        );
        assert_eq!(
            got(&file, Some(&root), Some(&root)),
            resolved(&s(&file), RootSource::Outside)
        );
    }

    #[test]
    fn an_unusable_caller_root_still_vetoes_a_file_in_another_repository() {
        // The caller's root need not exist here to constrain the `.git`
        // fallback: a moved root must not let a different repository borrow
        // this namespace's memories.
        let (_tmp, root, _) = project_with_worktree();
        let other = root.with_file_name("other-repo");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        let file = other.join("src").join("lib.rs");
        touch(&file);
        let missing = root.with_file_name("moved-away");
        assert!(!missing.exists());
        assert_eq!(
            got(&file, Some(&missing), None),
            resolved(&s(&file), RootSource::Outside)
        );
    }

    #[test]
    fn a_nested_foreign_checkout_falls_through_to_the_registered_root() {
        // `project_root` is an outer checkout; an independent repository
        // nested in it vetoes that root, but the lookup continues to the
        // registered nested root instead of ending as `Outside`.
        let tmp = tempfile::tempdir().unwrap();
        let outer = canonical(tmp.path()).unwrap().join("outer");
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        let nested = outer.join("nested");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        let file = nested.join("src").join("lib.rs");
        touch(&file);
        assert_eq!(
            got(&file, Some(&outer), Some(&nested)),
            resolved("src/lib.rs", RootSource::RegistryRoot)
        );
    }

    #[test]
    fn a_noncheckout_caller_root_falls_through_to_a_deeper_registered_checkout() {
        // The supplied root exists but is not itself a checkout; a nested
        // independent checkout vetoes it, and the deeper registered root
        // answers the file.
        let tmp = tempfile::tempdir().unwrap();
        let broad = canonical(tmp.path()).unwrap().join("workdir");
        std::fs::create_dir_all(&broad).unwrap();
        let project = broad.join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let file = project.join("src").join("lib.rs");
        touch(&file);
        assert!(!broad.join(".git").exists());
        assert_eq!(
            got(&file, Some(&broad), Some(&project)),
            resolved("src/lib.rs", RootSource::RegistryRoot)
        );
    }

    #[test]
    fn a_neurostrata_folder_alone_is_not_a_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = canonical(tmp.path()).unwrap().join("home");
        std::fs::create_dir_all(home.join(".NeuroStrata")).unwrap();
        let file = home.join("scratch").join("notes.py");
        touch(&file);
        let above_has_git = std::iter::successors(Some(home.as_path()), |p| p.parent())
            .any(|p| p.join(".git").exists());
        if !above_has_git {
            assert_eq!(
                got(&file, None, None),
                resolved(&s(&file), RootSource::Outside)
            );
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
        assert_eq!(
            lookup_forms("src/lib.rs", &rel, Some("example-project")),
            vec!["src/lib.rs".to_string()]
        );
        let up = to_project_relative("../other/lib.rs", None, None);
        assert_eq!(
            lookup_forms("../other/lib.rs", &up, Some(&s(&root))),
            vec!["../other/lib.rs".to_string()]
        );
    }

    #[test]
    fn parent_components_resolve_to_the_file_they_name() {
        let (_tmp, root, _) = project_with_worktree();
        let file = root.join("src").join("lib.rs");
        let via_parent = s(&root.join("src").join("..").join("src").join("lib.rs"));
        assert_eq!(
            got(Path::new(&via_parent), Some(&root), None),
            resolved("src/lib.rs", RootSource::ProjectRoot)
        );
        // A file not created yet resolves through its directory.
        let new_file = s(&root.join("src").join("..").join("docs").join("new.md"));
        std::fs::create_dir_all(root.join("docs")).unwrap();
        assert_eq!(
            got(Path::new(&new_file), Some(&root), None),
            resolved("docs/new.md", RootSource::ProjectRoot)
        );
        assert!(file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_also_looked_up_as_the_caller_spelled_it() {
        // `src/config.rs` links to `shared/config.rs`: the disk spelling comes
        // first, and the caller's own spelling (with its `src` ancestor) is
        // still queried, so rules on `src/config.rs` are not lost.
        let (_tmp, root, _) = project_with_worktree();
        touch(&root.join("shared").join("config.rs"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let link = root.join("src").join("config.rs");
        std::os::unix::fs::symlink(root.join("shared").join("config.rs"), &link).unwrap();
        let l = to_project_relative(&s(&link), Some(&s(&root)), None);
        assert_eq!(l.path, "shared/config.rs");
        let forms = lookup_forms(&s(&link), &l, Some(&s(&root)));
        assert!(forms.contains(&"src/config.rs".to_string()), "{forms:?}");
    }

    #[cfg(unix)]
    #[test]
    fn parent_components_follow_a_symlinked_directory_first() {
        // `link` points at `a/b`, so `link/../file.rs` is `a/file.rs`, not the
        // `file.rs` that collapsing `..` by spelling would give.
        let (_tmp, root, _) = project_with_worktree();
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        touch(&root.join("a").join("file.rs"));
        touch(&root.join("file.rs"));
        std::os::unix::fs::symlink(root.join("a").join("b"), root.join("link")).unwrap();
        let raw = s(&root.join("link").join("..").join("file.rs"));
        let l = to_project_relative(&raw, Some(&s(&root)), None);
        assert_eq!(l.path, "a/file.rs");
        let forms = lookup_forms(&raw, &l, Some(&s(&root)));
        assert!(
            !forms
                .iter()
                .any(|f| f == "file.rs" || f.ends_with("/example-project/file.rs")),
            "the collapsed spelling names another file: {forms:?}"
        );
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
        assert_eq!(
            got(&file, Some(&link), None),
            resolved("src/lib.rs", RootSource::ProjectRoot)
        );
        assert_eq!(
            got(&link.join("src").join("lib.rs"), None, Some(&real)),
            resolved("src/lib.rs", RootSource::RegistryRoot)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_root_spelled_in_another_case_or_verbatim_matches_on_disk() {
        let (_tmp, root, _) = project_with_worktree();
        let file = s(&root.join("src").join("lib.rs"));
        for given in [
            s(&root).to_uppercase(),
            format!(r"\\?\{}", s(&root)),
            s(&root).replace('\\', "/"),
        ] {
            let l = to_project_relative(&file, Some(&given), None);
            assert_eq!(
                (l.path.as_str(), l.source),
                ("src/lib.rs", RootSource::ProjectRoot),
                "{given}"
            );
        }
        // A file spelled in another case resolves to the on-disk case.
        let shouted = format!(r"{}\SRC\LIB.RS", s(&root).to_uppercase());
        let l = to_project_relative(&shouted, None, None);
        assert_eq!(
            (l.path.as_str(), l.source),
            ("src/lib.rs", RootSource::Marker)
        );
    }

    // ---- Task 2: host-native `//` and scoped path/prefix resolution ----

    #[cfg(unix)]
    #[test]
    fn linux_double_slash_path_is_posix_absolute_not_unc() {
        assert!(is_absolute_any("//server/share/repo/src/lib.rs"));
        let root = "/";
        let lookup = to_project_relative("//server/share/repo/src/lib.rs", Some(root), Some(root));
        assert_eq!(lookup.source, RootSource::ProjectRoot);
        assert_eq!(lookup.path, "server/share/repo/src/lib.rs");
        assert!(lookup.root.as_deref() == Some("/"));
        let forms = lookup_forms("//server/share/repo/src/lib.rs", &lookup, Some(root));
        assert!(forms.iter().any(|f| f == "server/share/repo/src/lib.rs"));
        assert!(forms.iter().all(|f| !f.starts_with("\\\\")));
    }

    #[test]
    fn unresolvable_absolute_path_in_scope_returns_explicit_error() {
        let root = std::env::temp_dir().join("ns-v1190-path-root");
        std::fs::create_dir_all(&root).expect("create temp root");
        let root_s = root.to_string_lossy().to_string();
        let err = resolve_in_scope_forms(
            "/definitely/outside/ns-v1190.rs",
            Some(&root_s),
            Some(&root_s),
            PathResolutionScope::File,
        )
        .expect_err("outside absolute path must be unresolvable");
        let msg = err.to_string();
        assert!(msg.contains("unresolvable") || msg.contains("outside"));
        assert!(msg.contains(&root_s));
    }

    #[test]
    fn prefix_forms_match_absolute_and_relative_stored_spellings() {
        let root = std::env::temp_dir().join("ns-v1190-prefix-root");
        std::fs::create_dir_all(root.join("src/graph")).expect("create prefix root");
        let root_s = root.to_string_lossy().to_string();
        let resolved = resolve_in_scope_forms(
            &format!("{}/src/graph", root_s),
            Some(&root_s),
            Some(&root_s),
            PathResolutionScope::Prefix,
        )
        .expect("in-scope prefix");
        assert_eq!(resolved.lookup.source, RootSource::ProjectRoot);
        assert!(resolved
            .forms
            .iter()
            .any(|f| f == "src/graph" || f == "src/graph/"));
        assert!(resolved.forms.iter().any(
            |f| f == &format!("{}/src/graph", root_s) || f == &format!("{}/src/graph/", root_s)
        ));
    }

    #[test]
    fn prefix_resolution_does_not_widen_outside_scope() {
        let root = std::env::temp_dir().join("ns-v1190-prefix-scope");
        std::fs::create_dir_all(&root).expect("create temp root");
        let root_s = root.to_string_lossy().to_string();
        let err = resolve_in_scope_forms(
            "/other/repo/src",
            Some(&root_s),
            Some(&root_s),
            PathResolutionScope::Prefix,
        )
        .expect_err("outside prefix must be unresolvable");
        assert!(err.to_string().contains("unresolvable") || err.to_string().contains("outside"));
    }

    #[test]
    fn to_project_relative_retains_deeper_valid_registry_relative_alternate_when_broader_project_root_wins(
    ) {
        let tmp = std::env::temp_dir().join("ns-v1190-alternates");
        let project = tmp.join("repo");
        let registry = project.join("platform");
        std::fs::create_dir_all(registry.join("src")).expect("create dirs");
        std::fs::create_dir_all(project.join(".git")).expect("create git marker");
        let file = registry.join("src/lib.rs");
        std::fs::write(&file, "pub fn demo() {}\n").expect("write file");
        let file_s = file.to_string_lossy().to_string();
        let project_s = project.to_string_lossy().to_string();
        let registry_s = registry.to_string_lossy().to_string();
        let lookup = to_project_relative(&file_s, Some(&project_s), Some(&registry_s));
        assert_eq!(lookup.source, RootSource::ProjectRoot);
        assert_eq!(lookup.path, "platform/src/lib.rs");
        assert!(lookup.alternates.iter().any(|a| a == "src/lib.rs"));
        let norm = lookup
            .alternates
            .iter()
            .map(|a| crate::parser::ingest::normalize_node_path(a))
            .collect::<Vec<_>>();
        let mut dedup = norm.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(
            norm.len(),
            dedup.len(),
            "alternates must be normalized/deduped stable-first-seen"
        );
    }

    #[test]
    fn prefix_equal_to_configured_or_checkout_root_resolves_to_root_forms() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = canonical(tmp.path()).expect("canonical tmp").join("repo");
        std::fs::create_dir_all(root.join(".git")).expect("create repo .git");
        let foreign = canonical(tmp.path())
            .expect("canonical tmp")
            .join("foreign");
        std::fs::create_dir_all(foreign.join(".git")).expect("create foreign .git");

        let root_s = root.to_string_lossy().to_string();
        let root_slash = format!("{root_s}/");
        let foreign_s = foreign.to_string_lossy().to_string();

        for raw in [root_s.as_str(), root_slash.as_str()] {
            let by_project =
                resolve_in_scope_forms(raw, Some(&root_s), None, PathResolutionScope::Prefix)
                    .expect("prefix equal to project_root must resolve");
            assert_eq!(by_project.lookup.source, RootSource::ProjectRoot);
            assert_eq!(by_project.lookup.path, ".");
            assert!(by_project.forms.iter().any(|f| f == "."));
            assert!(has_form(&by_project.forms, &root_s));

            let by_registry =
                resolve_in_scope_forms(raw, None, Some(&root_s), PathResolutionScope::Prefix)
                    .expect("prefix equal to usable registry_root must resolve");
            assert_eq!(by_registry.lookup.source, RootSource::RegistryRoot);
            assert_eq!(by_registry.lookup.path, ".");
            assert!(by_registry.forms.iter().any(|f| f == "."));
            assert!(has_form(&by_registry.forms, &root_s));

            let by_marker = resolve_in_scope_forms(raw, None, None, PathResolutionScope::Prefix)
                .expect("prefix equal to checkout root must resolve via .git marker");
            assert_eq!(by_marker.lookup.source, RootSource::Marker);
            assert_eq!(by_marker.lookup.path, ".");
            assert!(by_marker.forms.iter().any(|f| f == "."));
            assert!(has_form(&by_marker.forms, &root_s));
        }

        // Foreign checkout root stays vetoed when a project_root or registry_root is configured.
        assert!(resolve_in_scope_forms(
            &foreign_s,
            Some(&root_s),
            None,
            PathResolutionScope::Prefix,
        )
        .is_err());

        // File scope still rejects the root directory itself.
        assert!(
            resolve_in_scope_forms(&root_s, Some(&root_s), None, PathResolutionScope::File,)
                .is_err()
        );
    }

    // ---- Regression: post-merge #286 (items 1-3) and #293 ----

    /// Root with `a/b`, `a/file.rs`, a same-named root `file.rs`, and `link`
    /// symlinked to `a/b`. A lexical `..` collapse names the wrong root file.
    #[cfg(unix)]
    fn layout_with_symlinked_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path()).unwrap().join("repo");
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        touch(&root.join("a").join("file.rs"));
        touch(&root.join("file.rs"));
        std::os::unix::fs::symlink(root.join("a").join("b"), root.join("link")).unwrap();
        (tmp, root)
    }

    /// #286 item 1: the caller's own `..` spelling must stay queryable even
    /// though `follow_parent_components` resolves the path for the primary form.
    #[test]
    fn caller_raw_parent_spelling_is_kept_as_an_alternate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path()).unwrap().join("repo");
        touch(&root.join("src").join("lib.rs"));
        let root_s = s(&root);
        let caller = s(&root.join("src").join("..").join("src").join("lib.rs"));
        let lookup = to_project_relative(&caller, Some(&root_s), None);
        assert_eq!(lookup.path, "src/lib.rs");
        let forms = lookup_forms(&caller, &lookup, Some(&root_s));
        assert!(has_form(&forms, &caller), "{forms:?}");
        assert!(forms.iter().any(|f| f == "src/lib.rs"), "{forms:?}");
        let abs = s(&root.join("src").join("lib.rs"));
        assert!(forms.iter().any(|f| f == &abs), "{forms:?}");
    }

    /// #286 item 2: a missing component after `..` must not defeat on-disk
    /// symlink resolution of the prefix.
    #[cfg(unix)]
    #[test]
    fn parent_traversal_before_a_missing_component_follows_the_symlink() {
        let (_tmp, root) = layout_with_symlinked_dir();
        let root_s = s(&root);
        let true_rel = "a/newdir/file.rs".to_string();
        let true_abs = s(&root.join("a").join("newdir").join("file.rs"));
        let wrong_abs = s(&root.join("newdir").join("file.rs"));
        let wrong_rel = "newdir/file.rs".to_string();
        assert!(!root.join("newdir").exists());

        let raws = [
            s(&root.join("link").join("..").join("newdir").join("file.rs")),
            s(Path::new("link/../newdir/file.rs")),
        ];
        for raw in raws {
            let resolved =
                resolve_in_scope_forms(&raw, Some(&root_s), None, PathResolutionScope::File)
                    .unwrap();
            assert!(
                resolved.forms.contains(&true_rel),
                "{raw}: {:?}",
                resolved.forms
            );
            assert!(
                resolved.forms.contains(&true_abs),
                "{raw}: {:?}",
                resolved.forms
            );
            assert!(
                !resolved.forms.contains(&wrong_rel) && !resolved.forms.contains(&wrong_abs),
                "the collapsed spelling names another file: {raw}: {:?}",
                resolved.forms
            );
        }
    }

    /// #286 item 3: forward-slash verbatim prefixes are stripped on every host.
    #[test]
    fn forward_slash_verbatim_prefixes_strip_on_every_host() {
        for path in ["//?/C:/repo/src/lib.rs", "//./C:/repo/src/lib.rs"] {
            assert_eq!(
                relative_to(path, "C:/repo").as_deref(),
                Some("src/lib.rs"),
                "{path}"
            );
        }
        let verbatim_unc = "//?/UNC/server/share/repo/src/lib.rs";
        let parsed = parse_absolute(verbatim_unc).unwrap();
        assert!(parsed.windows);
        assert_eq!(parsed.root, "//server/share");
        assert_eq!(parsed.parts, ["repo", "src", "lib.rs"]);
        // A UNC root is spelled with backslashes for a cross-host lookup: a
        // leading `//` is the POSIX root off Windows by design (#269).
        assert_eq!(
            relative_to(verbatim_unc, r"\\server\share\repo").as_deref(),
            Some("src/lib.rs")
        );
        #[cfg(windows)]
        assert_eq!(
            relative_to(verbatim_unc, "//server/share/repo").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            relative_to(r"\\?\C:\repo\src\lib.rs", "C:/repo").as_deref(),
            Some("src/lib.rs")
        );
        // A non-verbatim leading `//` is still host-native.
        #[cfg(not(windows))]
        {
            let parsed = parse_absolute("//work/src/lib.rs").unwrap();
            assert!(!parsed.windows);
            assert_eq!(parsed.to_path_string(), "/work/src/lib.rs");
        }
        #[cfg(windows)]
        {
            let parsed = parse_absolute("//work/src/lib.rs").unwrap();
            assert!(parsed.windows);
            assert_eq!(parsed.root, "//work/src");
        }
    }

    /// #293: a relative `..` that crosses a symlinked directory names the
    /// on-disk target, never the lexical sibling. Both `lookup_forms` and
    /// `resolve_in_scope_forms` must agree.
    #[cfg(unix)]
    #[test]
    fn relative_parent_traversal_follows_a_symlinked_directory() {
        let (_tmp, root) = layout_with_symlinked_dir();
        let root_s = s(&root);
        let rel = "link/../file.rs";
        let true_rel = "a/file.rs".to_string();
        let true_abs = s(&root.join("a").join("file.rs"));
        let wrong_rel = "file.rs".to_string();
        let wrong_abs = s(&root.join("file.rs"));

        let lookup = to_project_relative(rel, Some(&root_s), None);
        assert_eq!(lookup.source, RootSource::Relative);
        let forms = lookup_forms(rel, &lookup, Some(&root_s));
        assert!(forms.contains(&true_rel), "{forms:?}");
        assert!(forms.contains(&true_abs), "{forms:?}");
        assert!(!forms.contains(&wrong_rel), "{forms:?}");
        assert!(!forms.contains(&wrong_abs), "{forms:?}");

        let resolved =
            resolve_in_scope_forms(rel, Some(&root_s), None, PathResolutionScope::File).unwrap();
        assert!(resolved.forms.contains(&true_rel), "{:?}", resolved.forms);
        assert!(resolved.forms.contains(&true_abs), "{:?}", resolved.forms);
        assert!(!resolved.forms.contains(&wrong_rel), "{:?}", resolved.forms);
        assert!(!resolved.forms.contains(&wrong_abs), "{:?}", resolved.forms);

        std::fs::create_dir_all(root.join("a").join("src")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let rel_prefix = "link/../src";
        let true_prefix_rel = "a/src".to_string();
        let true_prefix_abs = s(&root.join("a").join("src"));
        let wrong_prefix_rel = "src".to_string();
        let wrong_prefix_abs = s(&root.join("src"));

        let resolved_prefix =
            resolve_in_scope_forms(rel_prefix, Some(&root_s), None, PathResolutionScope::Prefix)
                .unwrap();
        assert!(
            resolved_prefix.forms.contains(&true_prefix_rel),
            "{:?}",
            resolved_prefix.forms
        );
        assert!(
            resolved_prefix.forms.contains(&true_prefix_abs),
            "{:?}",
            resolved_prefix.forms
        );
        assert!(
            !resolved_prefix.forms.contains(&wrong_prefix_rel),
            "{:?}",
            resolved_prefix.forms
        );
        assert!(
            !resolved_prefix.forms.contains(&wrong_prefix_abs),
            "{:?}",
            resolved_prefix.forms
        );
    }

    #[test]
    fn the_main_checkout_of_a_linked_worktree_is_the_checkout_that_owns_it() {
        let (tmp, root, worktree) = project_with_worktree();
        assert_eq!(main_checkout_root(&s(&root)), Some(root.clone()));
        assert_eq!(
            main_checkout_root(&s(&root.join("src"))),
            Some(root.clone())
        );
        assert_eq!(main_checkout_root(&s(&worktree)), Some(root.clone()));
        assert_eq!(
            main_checkout_root(&s(&worktree.join("src"))),
            Some(root.clone())
        );
        // A submodule's gitdir is <super>/.git/modules/<name>: its own root.
        let sub = canonical(tmp.path()).unwrap().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join(".git"),
            "gitdir: ../example-project/.git/modules/sub\n",
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&sub)), Some(sub.clone()));
        // A submodule inside the linked worktree: its gitdir lives under the
        // worktree's gitdir (`<main>/.git/worktrees/<wt>/modules/<sub>`) but
        // has no `commondir`, so it is still its own root, not `root`.
        let wt_gitdir = root.join(".git").join("worktrees").join("agent-1");
        let wt_sub_gitdir = wt_gitdir.join("modules").join("lib");
        std::fs::create_dir_all(&wt_sub_gitdir).unwrap();
        let wt_sub = worktree.join("lib");
        std::fs::create_dir_all(&wt_sub).unwrap();
        std::fs::write(
            wt_sub.join(".git"),
            format!("gitdir: {}\n", s(&wt_sub_gitdir).replace('\\', "/")),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&wt_sub)), Some(wt_sub.clone()));
        // A worktree of a bare repository: the common directory is no
        // checkout's `.git`, so the worktree answers its own root.
        let bare = canonical(tmp.path()).unwrap().join("bare.git");
        let bare_wt = canonical(tmp.path()).unwrap().join("bare-wt");
        link_worktree(&bare_wt, &bare.join("worktrees").join("bare-wt"));
        assert_eq!(main_checkout_root(&s(&bare_wt)), Some(bare_wt.clone()));
        let outside = canonical(tmp.path()).unwrap().join("plain");
        std::fs::create_dir_all(&outside).unwrap();
        let above_has_git = std::iter::successors(Some(outside.as_path()), |p| p.parent())
            .any(|p| p.join(".git").exists());
        if !above_has_git {
            assert_eq!(main_checkout_root(&s(&outside)), None);
        }
    }

    #[test]
    fn main_checkout_root_of_separate_git_dir_is_itself() {
        // `git init --separate-git-dir`: the `.git` file names a gitdir that
        // has no `commondir`, so the checkout is its own root.
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let checkout = base.join("example-project");
        let gitdir = base.join("elsewhere").join("example-project.git");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", s(&gitdir).replace('\\', "/")),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&checkout)), Some(checkout.clone()));
    }

    #[test]
    fn git_root_of_dir_is_the_dir_itself_or_its_nearest_checkout() {
        let (_tmp, root, worktree) = project_with_worktree();
        // One spelling whether `dir` is the checkout top or below it.
        let want = |p: &Path| parse_absolute(&s(p)).map(|p| p.to_path_string());
        assert_eq!(git_root_of_dir(&s(&root)), want(&root));
        assert_eq!(git_root_of_dir(&s(&root.join("src"))), want(&root));
        assert_eq!(git_root_of_dir(&s(&worktree)), want(&worktree));
        assert_eq!(git_root_of_dir(&s(&worktree.join("src"))), want(&worktree));
        let trailing = format!("{}{}", s(&root), std::path::MAIN_SEPARATOR);
        assert_eq!(git_root_of_dir(&trailing), want(&root));
        let dotted = format!("{}/src/..", s(&root));
        assert_eq!(git_root_of_dir(&dotted), want(&root));
    }

    /// `git clone --bare <url> <dir>/.git` + `git worktree add`: the common
    /// directory is named `.git` but its config says `core.bare = true`.
    #[test]
    fn worktree_of_a_bare_repo_named_dot_git_is_its_own_root() {
        let tmp = tempfile::tempdir().unwrap();
        let project = canonical(tmp.path()).unwrap().join("example-project");
        let common = project.join(".git");
        let wt = project.join("main");
        link_worktree(&wt, &common.join("worktrees").join("main"));
        std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(common.join("config"), "[core]\n\tBare = TRUE\n").unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
        assert_eq!(main_checkout_root(&s(&wt.join("src"))), Some(wt.clone()));
        // The same layout with `bare = false` and the parent holding that
        // `.git` is an ordinary main checkout.
        std::fs::write(common.join("config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(project.clone()));
    }

    /// Unrelated files beside a separate git directory do not make its parent
    /// a checkout: only Git's own metadata establishes ownership.
    #[test]
    fn unrelated_entries_do_not_make_a_parent_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let data = base.join("data");
        let common = data.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        touch(&data.join("unrelated.txt"));
        std::fs::write(common.join("config"), "[core]\n\tbare = false\n").unwrap();
        let wt = base.join("agent-1");
        link_worktree(&wt, &common.join("worktrees").join("agent-1"));

        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
    }

    /// A main checkout whose working directory holds nothing but `.git` is
    /// still a checkout when that git directory has Git's own `HEAD` metadata.
    #[test]
    fn main_checkout_with_only_dot_git_and_head_is_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let project = base.join("example-project");
        let common = project.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        touch(&common.join("HEAD"));
        std::fs::write(common.join("config"), "[core]\n\tbare = false\n").unwrap();
        let wt = base.join("agent-1");
        link_worktree(&wt, &common.join("worktrees").join("agent-1"));

        assert_eq!(main_checkout_root(&s(&wt)), Some(project.clone()));
    }

    /// `git init --separate-git-dir data/proj/.git sep` + `git worktree add`:
    /// the common directory is `data/proj/.git`, but `data/proj` is no
    /// checkout. The real main checkout `sep` is named nowhere git keeps
    /// (no `core.worktree`), so the worktree answers its own root; with
    /// `core.worktree` set, that folder is the main checkout.
    #[test]
    fn worktree_of_a_separate_git_dir_named_dot_git_is_not_owned_by_its_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let common = base.join("data").join("proj").join(".git");
        let sep = base.join("sep");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::create_dir_all(&sep).unwrap();
        std::fs::write(
            sep.join(".git"),
            format!("gitdir: {}\n", s(&common).replace('\\', "/")),
        )
        .unwrap();
        std::fs::write(common.join("config"), "[core]\n\tbare = false\n").unwrap();
        let wt = base.join("wt");
        link_worktree(&wt, &common.join("worktrees").join("wt"));
        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
        assert_eq!(main_checkout_root(&s(&sep)), Some(sep.clone()));
        std::fs::write(
            common.join("config"),
            format!(
                "[core]\n\tbare = false\n\tworktree = {}\n",
                s(&sep).replace('\\', "/")
            ),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(sep.clone()));
    }

    /// Every way the main checkout can't be found answers the worktree's own
    /// root: an unreadable gitdir, and a `commondir` naming a missing folder.
    #[test]
    fn main_checkout_root_failures_answer_the_worktrees_own_root() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let wt = base.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../missing/.git/worktrees/wt\n").unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
        let gone = base.join("gone").join(".git");
        let wt2 = base.join("wt2");
        link_worktree(&wt2, &base.join("gitdir2"));
        std::fs::write(
            base.join("gitdir2").join("commondir"),
            format!("{}\n", s(&gone).replace('\\', "/")),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&wt2)), Some(wt2.clone()));
    }

    /// A directory link (symlink, or a junction on Windows) to `target` at
    /// `link`; `false` when this host or user can't make one.
    fn dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
                || std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(link)
                    .arg(target)
                    .output()
                    .is_ok_and(|o| o.status.success())
        }
    }

    /// A worktree reached through a directory link, with a relative gitdir
    /// (`worktree.useRelativePaths`): the link is resolved first, so the
    /// relative gitdir is read against the real folder.
    #[test]
    fn main_checkout_root_resolves_a_linked_spelling_first() {
        let (tmp, root, worktree) = project_with_worktree();
        std::fs::write(
            worktree.join(".git"),
            "gitdir: ../../../.git/worktrees/agent-1\n",
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&worktree)), Some(root.clone()));
        let link = canonical(tmp.path()).unwrap().join("wt-link");
        if !dir_link(&worktree, &link) {
            eprintln!("skipped: no directory links on this host");
            return;
        }
        assert_eq!(main_checkout_root(&s(&link)), Some(root.clone()));
        assert_eq!(
            main_checkout_root(&s(&link.join("src"))),
            Some(root.clone())
        );
    }

    // ---- the project marker's spelling (spec 6.2) ----

    fn found_marker(root: &Path, namespace: &str) -> crate::root_marker::FoundMarker {
        crate::root_marker::FoundMarker {
            root: root.to_path_buf(),
            config: crate::root_marker::RootConfig::new(namespace),
            via: crate::root_marker::FoundVia::OwnCheckout,
        }
    }

    /// `<tmp>/repo` (a checkout) with `sub/a.rs`; returns (repo, file).
    fn repo_with_sub_file(tmp: &Path) -> (PathBuf, String) {
        let repo = dunce::canonicalize(tmp).unwrap().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(repo.join(".git").join("config"), "[core]\n\tbare = false\n").unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub").join("a.rs"), "fn a() {}").unwrap();
        let file = s(&repo.join("sub").join("a.rs"));
        (repo, file)
    }

    #[test]
    fn deeper_caller_root_also_queries_marker_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, file) = repo_with_sub_file(tmp.path());
        let (repo_s, sub_s) = (s(&repo), s(&repo.join("sub")));
        let today = to_project_relative(&file, Some(&sub_s), Some(&repo_s));
        assert_eq!(today.path, "a.rs");
        let today_forms = lookup_forms(&file, &today, Some(&sub_s));

        // The marker's namespace is matched ignoring case.
        let marker = found_marker(&repo, "Example-Project");
        let got = with_root_config(
            today.clone(),
            &file,
            &marker,
            Some(&repo_s),
            "example-project",
        );
        assert_eq!(got.path, "sub/a.rs");
        assert_eq!(got.root.as_deref(), Some(repo_s.as_str()));
        let forms = lookup_forms(&file, &got, Some(&sub_s));
        assert_eq!(forms[0], "sub/a.rs");
        // Every spelling today's resolution queried is still queried, in order.
        assert_eq!(forms[1..].to_vec(), today_forms, "{forms:?}");
        assert!(forms.contains(&"a.rs".to_string()), "{forms:?}");
    }

    #[test]
    fn resolved_from_is_root_config() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, file) = repo_with_sub_file(tmp.path());
        let repo_s = s(&repo);
        let today = to_project_relative(&file, None, Some(&repo_s));
        let got = with_root_config(
            today,
            &file,
            &found_marker(&repo, "ns"),
            Some(&repo_s),
            "ns",
        );
        assert_eq!(got.source, RootSource::RootConfig);
        assert_eq!(got.source.as_str(), "root_config");
        assert_eq!(got.path, "sub/a.rs");
    }

    #[test]
    fn existing_marker_wire_value_unchanged() {
        // `marker` keeps meaning the nearest `.git` ancestor.
        assert_eq!(RootSource::Marker.as_str(), "marker");
        let tmp = tempfile::tempdir().unwrap();
        let (_, file) = repo_with_sub_file(tmp.path());
        let today = to_project_relative(&file, None, None);
        assert_eq!(today.source, RootSource::Marker);
        assert_eq!(today.source.as_str(), "marker");
    }

    #[test]
    fn marker_root_differing_from_cache_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, file) = repo_with_sub_file(tmp.path());
        let sub_s = s(&repo.join("sub"));
        let marker = found_marker(&repo, "ns");
        let today = to_project_relative(&file, None, Some(&sub_s));
        for cached in [Some(sub_s.as_str()), None, Some("/no/such/folder")] {
            let got = with_root_config(today.clone(), &file, &marker, cached, "ns");
            assert_eq!(got, today, "cached {cached:?}");
        }
    }

    #[test]
    fn other_namespace_marker_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, file) = repo_with_sub_file(tmp.path());
        let (repo_s, sub_s) = (s(&repo), s(&repo.join("sub")));
        let today = to_project_relative(&file, Some(&sub_s), Some(&repo_s));
        let got = with_root_config(
            today.clone(),
            &file,
            &found_marker(&repo, "other"),
            Some(&repo_s),
            "ns",
        );
        assert_eq!(got, today);
    }

    #[test]
    fn outside_relative_prefix_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, _) = repo_with_sub_file(tmp.path());
        let repo_s = s(&repo);
        let marker = found_marker(&repo, "ns");

        let relative = to_project_relative("sub/a.rs", None, Some(&repo_s));
        let got = with_root_config(relative.clone(), "sub/a.rs", &marker, Some(&repo_s), "ns");
        assert_eq!(got, relative);

        let outside = LookupPath {
            path: file_in_other_place(tmp.path()),
            source: RootSource::Outside,
            root: None,
            alternates: Vec::new(),
        };
        let input = outside.path.clone();
        let got = with_root_config(outside.clone(), &input, &marker, Some(&repo_s), "ns");
        assert_eq!(got, outside);

        // Prefix lookups never go through the marker: a folder below the
        // cached root resolves relative to it, as before.
        let forms = prefix_lookup_forms(&s(&repo.join("sub")), None, Some(&repo_s)).unwrap();
        assert_eq!(forms[0], "sub", "{forms:?}");
    }

    fn file_in_other_place(tmp: &Path) -> String {
        let other = dunce::canonicalize(tmp).unwrap().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("b.rs"), "").unwrap();
        s(&other.join("b.rs"))
    }

    #[test]
    fn worktree_marker_root_matches_its_main_checkout_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, _) = repo_with_sub_file(tmp.path());
        let admin = repo.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let wt = dunce::canonicalize(tmp.path()).unwrap().join("wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", s(&admin))).unwrap();
        std::fs::write(wt.join("sub").join("a.rs"), "").unwrap();
        let file = s(&wt.join("sub").join("a.rs"));

        let repo_s = s(&repo);
        let marker = crate::root_marker::FoundMarker {
            via: crate::root_marker::FoundVia::MainCheckout,
            ..found_marker(&wt, "ns")
        };
        assert!(marker_root_is_cached_root(&marker, Some(&repo_s)));
        // A root cached at the worktree itself matches too.
        assert!(marker_root_is_cached_root(&marker, Some(&s(&wt))));
        let today = to_project_relative(&file, None, Some(&repo_s));
        let got = with_root_config(today, &file, &marker, Some(&repo_s), "ns");
        assert_eq!(got.source, RootSource::RootConfig);
        assert_eq!(got.path, "sub/a.rs");
    }
    /// A directory symlink followed by `..` must resolve on disk before the
    /// parent is collapsed; lexical normalization searches the link's parent.
    #[test]
    fn paths_resolve_symlinks_before_collapsing_parent_components() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n\tbare = false\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let link = base.join("shortcut");
        if !dir_link(&root.join("src"), &link) {
            eprintln!("skipped: no directory links on this host");
            return;
        }
        let via_link = format!("{}/..", s(&link));
        assert_eq!(
            git_root_of_dir(&via_link),
            Some(parse_absolute(&s(&root)).unwrap().to_path_string())
        );
        // A linked worktree through the same spelling resolves to the main
        // checkout, not the link's lexical parent.
        let wt = root.join(".claude").join("worktrees").join("agent-1");
        link_worktree(&wt, &root.join(".git").join("worktrees").join("agent-1"));
        let wt_link = base.join("wt-shortcut");
        if !dir_link(&wt, &wt_link) {
            eprintln!("skipped: no directory links on this host");
            return;
        }
        let via_wt = format!("{}/src/..", s(&wt_link));
        assert_eq!(main_checkout_root(&via_wt), Some(root.clone()));
    }

    /// Inline comments and Git config escapes are decoded before `core.bare`
    /// and `core.worktree` are interpreted.
    #[test]
    fn core_config_decodes_inline_comments_and_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let common = base.join("dotgit");
        let wt = base.join("wt");
        let escaped = base.join("sep#1");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::create_dir_all(&escaped).unwrap();
        link_worktree(&wt, &common.join("worktrees").join("wt"));
        std::fs::write(
            common.join("config"),
            format!(
                "[core]\n\tbare = false # harmless\n\tworktree = {}\\#1 ; trailing\n",
                // `\` is an escape in a Git config value, so spell the
                // Windows path with `/`.
                s(&base.join("sep")).replace('\\', "/")
            ),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(escaped.clone()));
        std::fs::write(common.join("config"), "[core]\n\tbare = true # yes\n").unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
    }

    /// A commented `bare = true` is true: a bare repo with a `.git` common dir
    /// must not lend its parent to a linked worktree as a main checkout.
    #[test]
    fn commented_core_bare_true_is_still_bare() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let project = base.join("bare-project");
        let common = project.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        touch(&project.join("unrelated.txt"));
        std::fs::write(common.join("config"), "[core]\n\tbare = true # comment\n").unwrap();
        let wt = base.join("wt");
        link_worktree(&wt, &common.join("worktrees").join("wt"));

        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
    }

    /// `extensions.worktreeConfig` and config includes supply the main
    /// checkout's `core.worktree` even when the literal config does not.
    #[test]
    fn core_config_reads_worktree_config_and_includes() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let common = base.join("dotgit");
        let wt = base.join("wt");
        let sep = base.join("sep");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::create_dir_all(&sep).unwrap();
        link_worktree(&wt, &common.join("worktrees").join("wt"));
        std::fs::write(
            common.join("config"),
            "[core]\n\tbare = false\n[extensions]\n\tworktreeConfig = true\n",
        )
        .unwrap();
        std::fs::write(
            common.join("config.worktree"),
            format!("[core]\n\tworktree = {}\n", s(&sep).replace('\\', "/")),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(sep.clone()));
        std::fs::remove_file(common.join("config.worktree")).unwrap();
        let inc = base.join("included-config");
        std::fs::write(
            &inc,
            format!("[core]\n\tworktree = {}\n", s(&sep).replace('\\', "/")),
        )
        .unwrap();
        std::fs::write(
            common.join("config"),
            format!(
                "[core]\n\tbare = false\n[include]\n\tpath = {}\n",
                s(&inc).replace('\\', "/")
            ),
        )
        .unwrap();
        assert_eq!(main_checkout_root(&s(&wt)), Some(sep.clone()));
    }

    /// An included config's `core.bare = true` is the effective value: a
    /// top-level `bare = false` must not make the common dir's parent a main
    /// checkout.
    #[test]
    fn included_core_bare_true_overrides_the_top_level_value() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let project = base.join("bare-project");
        let common = project.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        touch(&project.join("unrelated.txt"));
        let inc = base.join("included-config");
        std::fs::write(&inc, "[core]\n\tbare = true\n").unwrap();
        std::fs::write(
            common.join("config"),
            format!(
                "[core]\n\tbare = false\n[include]\n\tpath = {}\n",
                s(&inc).replace('\\', "/")
            ),
        )
        .unwrap();
        let wt = base.join("wt");
        link_worktree(&wt, &common.join("worktrees").join("wt"));

        assert_eq!(main_checkout_root(&s(&wt)), Some(wt.clone()));
    }

    /// A checkout holding only its `.git` is still the main checkout; a
    /// common `.git` dropped beside unrelated data is not a checkout.
    #[test]
    fn main_checkout_of_common_identifies_the_actual_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path()).unwrap();
        let checkout = base.join("checkout");
        let common = checkout.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(common.join("config"), "[core]\n\tbare = false\n").unwrap();
        let wt = base.join("wt");
        link_worktree(&wt, &common.join("worktrees").join("wt"));
        assert_eq!(main_checkout_root(&s(&wt)), Some(checkout.clone()));

        let data = base.join("data");
        let data_common = data.join(".git");
        std::fs::create_dir_all(&data_common).unwrap();
        std::fs::create_dir_all(data.join("unrelated")).unwrap();
        std::fs::write(data_common.join("config"), "[core]\n\tbare = false\n").unwrap();
        let wt2 = base.join("wt2");
        link_worktree(&wt2, &data_common.join("worktrees").join("wt2"));
        assert_eq!(main_checkout_root(&s(&wt2)), Some(wt2.clone()));
    }

}
