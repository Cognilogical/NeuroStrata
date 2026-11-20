//! The project root marker: `<root>/.NeuroStrata/config.json`.
//!
//! A folder is a project root when it holds this file with a valid
//! `namespace`. The file is strict JSON; `src/schemas/root-config.schema.json`
//! documents the same rules for users, and [`read_marker`] enforces them in
//! code (the schema is not loaded at runtime).
//!
//! Writers publish a new file with a no-replace hard link so two racing
//! writers cannot both win, and rewrite an existing one with temp + rename so
//! a reader never sees half a file. Both refuse a `.NeuroStrata` that is not a
//! real directory inside the target folder. Whether a folder may be a root at
//! all (home, a drive root, a parent of checkouts) is [`refuse_root_target`],
//! which callers check before writing.

// Later tasks in the root-marker stack wire these into the writers and the
// resolver; until then nothing outside this file calls them.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The marker file, relative to the root it marks.
pub const ROOT_CONFIG_REL: &str = ".NeuroStrata/config.json";

/// The only `version` this build understands.
const CURRENT_VERSION: u32 = 1;

/// How deep [`refuse_root_target`] looks for a `.git` below a non-git folder.
/// Checkouts are usually one to three levels under a workspace folder; the
/// bound keeps the check fast on a large tree.
const NESTED_GIT_MAX_DEPTH: usize = 4;

/// Folders the nested-checkout walk never enters: dependency and build
/// output trees are large and do not hold the user's checkouts.
const NESTED_GIT_SKIP: &[&str] = &["node_modules", "target", ".git"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RootConfig {
    pub version: u32,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Fields this build does not know; kept so a rewrite does not drop them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl RootConfig {
    pub fn new(namespace: impl Into<String>) -> Self {
        RootConfig {
            version: CURRENT_VERSION,
            namespace: namespace.into(),
            source_id: None,
            extra: serde_json::Map::new(),
        }
    }

    /// Why this config may not be written or trusted, or `None` when valid.
    fn invalid_reason(&self) -> Option<String> {
        if self.version != CURRENT_VERSION {
            return Some(format!("unknown version {}", self.version));
        }
        if !is_valid_namespace(&self.namespace) {
            return Some(format!("invalid namespace {:?}", self.namespace));
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerRead {
    Absent,
    Valid(RootConfig),
    Unreadable { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriteOutcome {
    Written,
    /// Another writer published the file first; this is what it holds now.
    LostRace(MarkerRead),
}

#[derive(Debug)]
pub enum MarkerError {
    Unsafe(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for MarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkerError::Unsafe(reason) => write!(f, "refused to write the root marker: {reason}"),
            MarkerError::Io(err) => write!(f, "root marker I/O error: {err}"),
        }
    }
}

impl std::error::Error for MarkerError {}

impl From<std::io::Error> for MarkerError {
    fn from(err: std::io::Error) -> Self {
        MarkerError::Io(err)
    }
}

/// `[A-Za-z0-9][A-Za-z0-9_.:-]*`, the schema's `namespace` pattern. The
/// leading letter or digit keeps a namespace from reading as an option
/// (`-x`) or a path step (`.`).
pub fn is_valid_namespace(ns: &str) -> bool {
    let mut chars = ns.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// The marker in `dir`, if any. A UTF-8 BOM is accepted; bad JSON, an
/// unknown `version` or a bad `namespace` is `Unreadable`, never `Absent`.
pub fn read_marker(dir: &Path) -> MarkerRead {
    let path = dir.join(ROOT_CONFIG_REL);
    let unreadable = |reason: String| MarkerRead::Unreadable { path: path.clone(), reason };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return MarkerRead::Absent,
        Err(err) => return unreadable(err.to_string()),
    };
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let cfg: RootConfig = match serde_json::from_slice(body) {
        Ok(cfg) => cfg,
        Err(err) => return unreadable(format!("not a valid root config: {err}")),
    };
    match cfg.invalid_reason() {
        Some(reason) => unreadable(reason),
        None => MarkerRead::Valid(cfg),
    }
}

/// Publishes a new marker in `dir`. The file is written to a temporary name
/// in `.NeuroStrata/` and hard-linked to `config.json`, which fails when the
/// name exists: a second writer gets `LostRace` with what the first wrote.
/// There is no fallback to a replacing rename, so a filesystem without hard
/// links answers `Io`.
pub fn write_new_marker(dir: &Path, cfg: &RootConfig) -> Result<WriteOutcome, MarkerError> {
    let (tmp, target) = prepare(dir, cfg)?;
    let linked = std::fs::hard_link(&tmp, &target);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(WriteOutcome::Written),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(WriteOutcome::LostRace(read_marker(dir)))
        }
        Err(err) => Err(MarkerError::Io(err)),
    }
}

/// Writes the marker in `dir`, replacing any existing one with temp + rename
/// so a concurrent reader sees either the old or the new file.
pub fn replace_marker(dir: &Path, cfg: &RootConfig) -> Result<(), MarkerError> {
    let (tmp, target) = prepare(dir, cfg)?;
    if let Err(err) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(MarkerError::Io(err));
    }
    Ok(())
}

/// Validates `cfg`, checks `.NeuroStrata` is safe, and writes the
/// serialized config to a fresh temporary file beside the target.
fn prepare(dir: &Path, cfg: &RootConfig) -> Result<(PathBuf, PathBuf), MarkerError> {
    if let Some(reason) = cfg.invalid_reason() {
        return Err(MarkerError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, reason)));
    }
    if !marker_dir_is_safe(dir) {
        return Err(MarkerError::Unsafe(".NeuroStrata is not a real folder inside the target"));
    }
    let mut body = serde_json::to_vec_pretty(cfg)
        .map_err(|err| MarkerError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err)))?;
    body.push(b'\n');
    let target = dir.join(ROOT_CONFIG_REL);
    let tmp = temp_name(&target);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(MarkerError::Io(err));
    }
    Ok((tmp, target))
}

fn temp_name(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!("config.json.tmp-{}-{nanos}-{n}", std::process::id()))
}

/// True when `dir/.NeuroStrata` is a real directory inside `dir` (not a
/// symlink or junction pointing elsewhere), by the check upstream uses for
/// its marker. A missing `.NeuroStrata` is created as a real directory first.
pub fn marker_dir_is_safe(dir: &Path) -> bool {
    let Ok(dir) = dunce::canonicalize(dir) else {
        return false;
    };
    let marker = dir.join(".NeuroStrata");
    if let Err(err) = std::fs::symlink_metadata(&marker) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return false;
        }
        if let Err(err) = std::fs::create_dir(&marker) {
            // A racing writer may have created it; the check below decides.
            if err.kind() != std::io::ErrorKind::AlreadyExists {
                return false;
            }
        }
    }
    crate::source_context::is_valid_marker_directory(&marker)
}

/// Why `dir` may not hold a root marker, or `None` when it may:
/// - `"home folder"`: a marker at `~` would capture every project below it,
///   with or without a `.git` there;
/// - `"filesystem root"`: `/` or a drive root;
/// - `"folder contains a git checkout"`: `dir` is outside git but has a
///   `.git` below it (looked for down to [`NESTED_GIT_MAX_DEPTH`] levels,
///   skipping `node_modules`, `target` and `.git`), so it is a workspace
///   folder above projects, not a project.
pub fn refuse_root_target(dir: &Path) -> Option<&'static str> {
    refuse_root_target_with(dir, dirs::home_dir().as_deref())
}

pub(crate) fn refuse_root_target_with(dir: &Path, home: Option<&Path>) -> Option<&'static str> {
    let dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if dir.parent().is_none() {
        return Some("filesystem root");
    }
    if crate::task::is_home_folder(&dir, home) {
        return Some("home folder");
    }
    if crate::paths::git_root_of_dir(&dir.to_string_lossy()).is_none() && has_git_below(&dir, 1) {
        return Some("folder contains a git checkout");
    }
    None
}

/// Whether some directory below `dir` (at `depth` and deeper, down to
/// [`NESTED_GIT_MAX_DEPTH`]) holds a `.git`. Links are not followed.
fn has_git_below(dir: &Path, depth: usize) -> bool {
    if depth > NESTED_GIT_MAX_DEPTH {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let is_real_dir = entry.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink());
        if !is_real_dir {
            return false;
        }
        let name = entry.file_name();
        if NESTED_GIT_SKIP.iter().any(|skip| name == *skip) {
            return false;
        }
        let path = entry.path();
        path.join(".git").exists() || has_git_below(&path, depth + 1)
    })
}

/// Which walk of [`find_root_marker`] found the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundVia {
    /// In the path's own checkout, at or below its top.
    OwnCheckout,
    /// In the main checkout of the linked worktree that holds the path; the
    /// root is the same folder mapped back into the worktree.
    MainCheckout,
    /// Outside git: the first marker above the path.
    NoGit,
}

impl FoundVia {
    pub fn as_str(self) -> &'static str {
        match self {
            FoundVia::OwnCheckout => "own_checkout",
            FoundVia::MainCheckout => "main_checkout",
            FoundVia::NoGit => "no_git",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundMarker {
    /// The project root, in the path's own checkout (for `MainCheckout`, the
    /// marker's folder mapped into the worktree, which holds no file).
    pub root: PathBuf,
    pub config: RootConfig,
    pub via: FoundVia,
}

impl FoundMarker {
    /// The folder that holds `.NeuroStrata/config.json`: `root`, or for
    /// `MainCheckout` the same folder in the main checkout.
    pub fn marker_dir(&self) -> PathBuf {
        if self.via != FoundVia::MainCheckout {
            return self.root.clone();
        }
        let mapped = checkout_top(&self.root).and_then(|top| {
            let main = crate::paths::main_checkout_root(&top.to_string_lossy())?;
            Some(main.join(self.root.strip_prefix(&top).ok()?))
        });
        mapped.unwrap_or_else(|| self.root.clone())
    }
}

/// The marker that decides `path`'s project root (spec 6.1), or `None`.
///
/// For a file the walk starts at its folder. A path that exists is
/// canonicalized first, so any spelling of it (drive letter case, `/`, a
/// junction or symlink) finds the same marker.
/// 1. Own checkout: from the start folder up to the nearest folder holding
///    `.git` (inclusive), never above it; the deepest marker wins.
/// 2. Linked worktree: the start folder is mapped into the main checkout
///    (keeping the part below the worktree top), and that walk runs from the
///    mapped folder's nearest existing ancestor up to the main checkout's top,
///    never entering a folder that holds its own `.git` (a submodule or a
///    nested clone there). Steps 1 and 2 both run; the deeper marker wins,
///    and at equal depth the own checkout's.
/// 3. Outside git: the first marker above the start folder.
///
/// A `.NeuroStrata` that is a link pointing elsewhere is not a marker. When
/// the winning marker cannot be read, the answer is `Err((file, reason))`,
/// never a silent skip to a marker further up.
pub fn find_root_marker(path: &Path) -> Result<Option<FoundMarker>, (PathBuf, String)> {
    let Some(start) = start_folder(path) else {
        return Ok(None);
    };
    let Some(top) = checkout_top(&start) else {
        return match start.ancestors().find_map(marker_at) {
            None => Ok(None),
            Some((dir, read)) => decide(dir, read, FoundVia::NoGit),
        };
    };

    let own = start
        .ancestors()
        .take_while(|dir| dir.starts_with(&top))
        .find_map(marker_at)
        .map(|(dir, read)| (depth_below(&dir, &top), dir, read, FoundVia::OwnCheckout));

    let main = linked_main_checkout(&top).and_then(|main| {
        let rel = start.strip_prefix(&top).ok()?;
        let (dir, read) = main_checkout_walk(&main, rel)?;
        let depth = depth_below(&dir, &main);
        let root = top.join(dir.strip_prefix(&main).ok()?);
        Some((depth, root, read, FoundVia::MainCheckout))
    });

    let winner = match (own, main) {
        (Some(own), Some(main)) => Some(if main.0 > own.0 { main } else { own }),
        (own, main) => own.or(main),
    };
    match winner {
        None => Ok(None),
        Some((_, root, read, via)) => decide(root, read, via),
    }
}

fn decide(
    root: PathBuf,
    read: MarkerRead,
    via: FoundVia,
) -> Result<Option<FoundMarker>, (PathBuf, String)> {
    match read {
        MarkerRead::Valid(config) => Ok(Some(FoundMarker { root, config, via })),
        MarkerRead::Unreadable { path, reason } => Err((path, reason)),
        MarkerRead::Absent => Ok(None),
    }
}

/// The absolute folder a walk starts from, canonical when it exists.
fn start_folder(path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let path = dunce::canonicalize(&path).unwrap_or(path);
    if path.is_file() {
        path.parent().map(Path::to_path_buf)
    } else {
        Some(path)
    }
}

/// The nearest folder at or above `dir` that holds `.git` (a directory or a
/// worktree's file), in `dir`'s spelling: the rule of
/// [`crate::paths::git_root_of_dir`], which answers with `/` on Windows.
fn checkout_top(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// The main checkout when `top` is a linked worktree's top, else `None`.
fn linked_main_checkout(top: &Path) -> Option<PathBuf> {
    let main = crate::paths::main_checkout_root(&top.to_string_lossy())?;
    let own = dunce::canonicalize(top).unwrap_or_else(|_| top.to_path_buf());
    (main != own).then_some(main)
}

/// Step 2's walk in the main checkout `main` for the worktree-relative `rel`.
fn main_checkout_walk(main: &Path, rel: &Path) -> Option<(PathBuf, MarkerRead)> {
    // Built top down so the walk stops before a folder that holds its own
    // `.git`, and at the first folder missing from `main`.
    let mut allowed = vec![main.to_path_buf()];
    let mut dir = main.to_path_buf();
    for part in rel.components() {
        dir.push(part);
        if !dir.is_dir() || dir.join(".git").exists() {
            break;
        }
        allowed.push(dir.clone());
    }
    allowed.into_iter().rev().find_map(|dir| marker_at(&dir))
}

/// The marker in `dir` when there is one to report, valid or unreadable. A
/// `.NeuroStrata` that is a link pointing elsewhere, or a folder without
/// `config.json`, is none.
fn marker_at(dir: &Path) -> Option<(PathBuf, MarkerRead)> {
    let folder = dir.join(".NeuroStrata");
    if std::fs::symlink_metadata(&folder).is_err()
        || !crate::source_context::is_valid_marker_directory(&folder)
    {
        return None;
    }
    match read_marker(dir) {
        MarkerRead::Absent => None,
        read => Some((dir.to_path_buf(), read)),
    }
}

fn depth_below(dir: &Path, top: &Path) -> usize {
    dir.strip_prefix(top).map(|rel| rel.components().count()).unwrap_or(0)
}

/// How deep `root show` looks below its folder for other markers.
const NESTED_MARKER_MAX_DEPTH: usize = 8;

/// Folders below `dir` (not `dir` itself) holding a marker, valid or not,
/// down to [`NESTED_MARKER_MAX_DEPTH`] levels. Links are not followed, and
/// `node_modules`, `target`, `.git` and folders holding their own `.git`
/// (another checkout) are not entered.
pub fn nested_markers(dir: &Path) -> Vec<(PathBuf, MarkerRead)> {
    let mut found = Vec::new();
    collect_nested(dir, 1, &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn collect_nested(dir: &Path, depth: usize, found: &mut Vec<(PathBuf, MarkerRead)>) {
    if depth > NESTED_MARKER_MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()) {
            continue;
        }
        let name = entry.file_name();
        if NESTED_GIT_SKIP.iter().any(|skip| name == *skip) || name == ".NeuroStrata" {
            continue;
        }
        let path = entry.path();
        if path.join(".git").exists() {
            continue;
        }
        if let Some(hit) = marker_at(&path) {
            found.push(hit);
        }
        collect_nested(&path, depth + 1, found);
    }
}

/// The registry's cached root as `root show` could read it: only the caller
/// can, through the daemon or the store.
#[derive(Debug, Clone, PartialEq)]
pub enum CachedRoot {
    Known(Option<String>),
    Unknown(String),
}

/// The text of `neurostrata-mcp root show` for the absolute folder `dir`.
/// `namespace` is the marker's namespace, else the one the project resolves
/// to without a marker; `bindings` are the configured project contexts.
pub fn format_root_show(
    dir: &Path,
    found: &Result<Option<FoundMarker>, (PathBuf, String)>,
    namespace: &str,
    cached: &CachedRoot,
    bindings: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let mut out = vec![format!("folder: {}", dir.display())];
    let mut marker_root = None;
    match found {
        Ok(Some(found)) => {
            let marker_dir = found.marker_dir();
            out.push(format!("marker root: {}", found.root.display()));
            out.push(format!("marker file: {}", marker_dir.join(ROOT_CONFIG_REL).display()));
            out.push(format!("found via: {}", found.via.as_str()));
            out.push(format!("namespace: {}", found.config.namespace));
            out.push(format!(
                "source_id: {}",
                found.config.source_id.as_deref().unwrap_or("none")
            ));
            out.push(format!("git tracks the marker: {}", git_tracks_marker(&marker_dir, found.via)));
            marker_root = Some(found.root.clone());
        }
        Ok(None) => {
            out.push("marker root: none (no .NeuroStrata/config.json applies here)".to_string());
            out.push(format!("namespace without a marker: {namespace}"));
        }
        Err((path, reason)) => {
            out.push(format!("marker root: unreadable marker {}: {reason}", path.display()));
            out.push(format!("namespace without a marker: {namespace}"));
        }
    }
    match cached {
        CachedRoot::Known(Some(root)) => {
            let exists = Path::new(root).is_dir();
            out.push(format!(
                "cached root for {namespace}: {root} ({})",
                if exists { "exists" } else { "missing" }
            ));
            if marker_root.as_deref().is_some_and(|m| !same_folder(Path::new(root), m)) {
                out.push(
                    "hint: the marker root differs from the cached root; lookups still try both"
                        .to_string(),
                );
            }
        }
        CachedRoot::Known(None) => out.push(format!("cached root for {namespace}: none")),
        CachedRoot::Unknown(why) => out.push(format!("cached root: unknown ({why})")),
    }
    let nested = nested_markers(dir);
    out.push(format!(
        "nested markers below this folder (up to {NESTED_MARKER_MAX_DEPTH} levels, other checkouts not entered): {}",
        if nested.is_empty() { "none".to_string() } else { nested.len().to_string() }
    ));
    for (path, read) in &nested {
        match read {
            MarkerRead::Valid(cfg) => {
                out.push(format!("  {} (namespace {})", path.display(), cfg.namespace))
            }
            MarkerRead::Unreadable { reason, .. } => {
                out.push(format!("  {} (unreadable: {reason})", path.display()))
            }
            MarkerRead::Absent => {}
        }
    }
    let mut missing: Vec<_> = bindings.iter().filter(|(_, path)| !path.exists()).collect();
    missing.sort();
    out.push(format!(
        "project-context bindings to missing folders: {}",
        if missing.is_empty() { "none".to_string() } else { missing.len().to_string() }
    ));
    for (source_id, path) in missing {
        out.push(format!("  {source_id} -> {}", path.display()));
    }
    out.join("\n")
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `yes`/`no` by `git ls-files --error-unmatch`, run in the checkout that
/// holds the file.
fn git_tracks_marker(marker_dir: &Path, via: FoundVia) -> String {
    if via == FoundVia::NoGit {
        return "no (not in a git checkout)".to_string();
    }
    match std::process::Command::new("git")
        .arg("-C")
        .arg(marker_dir)
        .args(["ls-files", "--error-unmatch", ROOT_CONFIG_REL])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) if status.success() => "yes".to_string(),
        Ok(_) => "no".to_string(),
        Err(err) => format!("unknown (git could not run: {err})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn write_raw(dir: &Path, body: &[u8]) {
        std::fs::create_dir_all(dir.join(".NeuroStrata")).unwrap();
        std::fs::write(dir.join(ROOT_CONFIG_REL), body).unwrap();
    }

    #[test]
    fn marker_round_trips_and_keeps_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_marker(tmp.path()), MarkerRead::Absent);
        write_raw(
            tmp.path(),
            br#"{"version":1,"namespace":"example-project","source_id":"example-project","future":{"a":1}}"#,
        );
        let MarkerRead::Valid(mut cfg) = read_marker(tmp.path()) else {
            panic!("expected a valid marker");
        };
        assert_eq!(cfg.namespace, "example-project");
        assert_eq!(cfg.source_id.as_deref(), Some("example-project"));
        assert_eq!(cfg.extra.get("future"), Some(&serde_json::json!({"a": 1})));

        cfg.namespace = "example-project-2".into();
        replace_marker(tmp.path(), &cfg).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join(ROOT_CONFIG_REL)).unwrap()).unwrap();
        assert_eq!(raw["future"], serde_json::json!({"a": 1}));
        assert_eq!(raw["namespace"], "example-project-2");
        assert_eq!(read_marker(tmp.path()), MarkerRead::Valid(cfg));

        // No source_id: the field is left out, not written as null.
        let plain = RootConfig::new("example-project");
        replace_marker(tmp.path(), &plain).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join(ROOT_CONFIG_REL)).unwrap();
        assert!(!raw.contains("source_id"), "{raw}");
        assert_eq!(read_marker(tmp.path()), MarkerRead::Valid(plain));
    }

    #[test]
    fn marker_with_bom_and_crlf_reads() {
        let tmp = tempfile::tempdir().unwrap();
        write_raw(
            tmp.path(),
            b"\xEF\xBB\xBF{\r\n  \"version\": 1,\r\n  \"namespace\": \"example-project\"\r\n}\r\n",
        );
        assert_eq!(read_marker(tmp.path()), MarkerRead::Valid(RootConfig::new("example-project")));
    }

    #[test]
    fn unknown_version_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        for body in [
            &br#"{"version":2,"namespace":"example-project"}"#[..],
            br#"{"namespace":"example-project"}"#,
            br#"{"version":1}"#,
            b"not json",
        ] {
            write_raw(tmp.path(), body);
            match read_marker(tmp.path()) {
                MarkerRead::Unreadable { path, .. } => assert_eq!(path, tmp.path().join(ROOT_CONFIG_REL)),
                other => panic!("{:?} read as {other:?}", String::from_utf8_lossy(body)),
            }
        }
        // A writer refuses a config it could not read back.
        let mut cfg = RootConfig::new("example-project");
        cfg.version = 2;
        assert!(matches!(replace_marker(tmp.path(), &cfg), Err(MarkerError::Io(_))));
    }

    #[test]
    fn namespace_charset() {
        for ok in ["team:proj", "example-project", "a", "A1_b.c:d-e"] {
            assert!(is_valid_namespace(ok), "{ok}");
        }
        for bad in ["-x", "", ".x", "_x", "a b", "a/b", "a$b", "é"] {
            assert!(!is_valid_namespace(bad), "{bad}");
        }
        let tmp = tempfile::tempdir().unwrap();
        write_raw(tmp.path(), br#"{"version":1,"namespace":"team:proj"}"#);
        assert!(matches!(read_marker(tmp.path()), MarkerRead::Valid(_)));
        write_raw(tmp.path(), br#"{"version":1,"namespace":"-x"}"#);
        assert!(matches!(read_marker(tmp.path()), MarkerRead::Unreadable { .. }));
    }

    #[test]
    fn second_writer_loses_race() {
        let tmp = tempfile::tempdir().unwrap();
        let first = RootConfig::new("example-project");
        let second = RootConfig::new("other-project");
        assert_eq!(write_new_marker(tmp.path(), &first).unwrap(), WriteOutcome::Written);
        assert_eq!(
            write_new_marker(tmp.path(), &second).unwrap(),
            WriteOutcome::LostRace(MarkerRead::Valid(first))
        );
        // No temporary files are left behind.
        let left: Vec<_> = std::fs::read_dir(tmp.path().join(".NeuroStrata"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["config.json".to_string()]);
    }

    #[test]
    fn replace_is_atomic_for_readers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        replace_marker(&dir, &RootConfig::new("example-a")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (dir, stop) = (dir.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut bad = Vec::new();
                let mut reads = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    reads += 1;
                    let got = read_marker(&dir);
                    if !matches!(got, MarkerRead::Valid(_)) {
                        bad.push(got);
                    }
                }
                (reads, bad)
            })
        };
        for i in 0..200 {
            let ns = if i % 2 == 0 { "example-b" } else { "example-a" };
            replace_marker(&dir, &RootConfig::new(ns)).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let (reads, bad) = reader.join().unwrap();
        assert!(reads > 0);
        assert!(bad.is_empty(), "{} of {reads} reads failed, first: {:?}", bad.len(), bad[0]);
    }

    /// Links `link` to `target` as a directory symlink, or on Windows as a
    /// junction when symlinks need a privilege this session lacks.
    fn link_dir(target: &Path, link: &Path) -> bool {
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

    #[test]
    fn symlinked_neurostrata_is_refused() {
        // A missing folder is created as a real directory.
        let fresh = tempfile::tempdir().unwrap();
        assert!(marker_dir_is_safe(fresh.path()));
        assert!(std::fs::symlink_metadata(fresh.path().join(".NeuroStrata")).unwrap().is_dir());

        // A file named .NeuroStrata is not a folder.
        let file = tempfile::tempdir().unwrap();
        std::fs::write(file.path().join(".NeuroStrata"), b"").unwrap();
        assert!(!marker_dir_is_safe(file.path()));
        assert!(matches!(
            write_new_marker(file.path(), &RootConfig::new("example-project")),
            Err(MarkerError::Unsafe(_))
        ));

        // A link pointing elsewhere is refused and nothing is written there.
        let elsewhere = tempfile::tempdir().unwrap();
        let linked = tempfile::tempdir().unwrap();
        assert!(link_dir(elsewhere.path(), &linked.path().join(".NeuroStrata")), "could not create a link");
        assert!(!marker_dir_is_safe(linked.path()));
        assert!(matches!(
            write_new_marker(linked.path(), &RootConfig::new("example-project")),
            Err(MarkerError::Unsafe(_))
        ));
        assert!(matches!(
            replace_marker(linked.path(), &RootConfig::new("example-project")),
            Err(MarkerError::Unsafe(_))
        ));
        assert!(!elsewhere.path().join("config.json").exists());
    }

    #[test]
    fn home_and_root_are_refused() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(refuse_root_target_with(home.path(), Some(home.path())), Some("home folder"));
        std::fs::create_dir(home.path().join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(home.path(), Some(home.path())), Some("home folder"));

        let project = home.path().join("example-project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(&project, Some(home.path())), None);

        let fs_root = dunce::canonicalize(home.path()).unwrap().ancestors().last().unwrap().to_path_buf();
        assert_eq!(refuse_root_target_with(&fs_root, None), Some("filesystem root"));
    }

    #[test]
    fn non_git_parent_of_checkout_is_refused() {
        let ws = tempfile::tempdir().unwrap();
        let ws = ws.path();
        let plain = ws.join("notes");
        std::fs::create_dir_all(plain.join("sub")).unwrap();
        assert_eq!(refuse_root_target_with(&plain, None), None);

        let repo = ws.join("a").join("b").join("example-project");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(ws, None), Some("folder contains a git checkout"));
        assert_eq!(refuse_root_target_with(&ws.join("a"), None), Some("folder contains a git checkout"));
        // The checkout and folders inside it may be roots.
        assert_eq!(refuse_root_target_with(&repo, None), None);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        assert_eq!(refuse_root_target_with(&repo.join("sub"), None), None);

        // Beyond the depth bound, or under a skipped folder, is not looked at.
        let deep = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(deep.path().join("1/2/3/4/5/.git")).unwrap();
        assert_eq!(refuse_root_target_with(deep.path(), None), None);
        std::fs::create_dir_all(deep.path().join("1/2/3/4/.git")).unwrap();
        assert_eq!(refuse_root_target_with(deep.path(), None), Some("folder contains a git checkout"));
        let skipped = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(skipped.path().join("node_modules/pkg/.git")).unwrap();
        std::fs::create_dir_all(skipped.path().join("target/x/.git")).unwrap();
        assert_eq!(refuse_root_target_with(skipped.path(), None), None);
    }

    const CASES: &str = include_str!("../tests/fixtures/root-marker-cases.json");

    /// `rel` (with `/`) under `base`, in this host's separators: `mklink`
    /// does not accept `/`.
    fn under(base: &Path, rel: &str) -> PathBuf {
        rel.split('/').fold(base.to_path_buf(), |path, part| path.join(part))
    }

    /// Builds one fixture `layout` under `base`; the format is documented in
    /// the fixture's `_comment`.
    fn build_layout(base: &Path, entries: &[serde_json::Value]) {
        for entry in entries {
            let entry = entry.as_str().expect("layout entries are strings");
            let (rel, kind) = match entry.split_once(' ') {
                Some((rel, kind)) => (rel, Some(kind)),
                None => (entry, None),
            };
            let path = under(base, rel);
            let Some(kind) = kind else {
                std::fs::create_dir_all(&path).unwrap();
                continue;
            };
            if let Some(ns) = kind.strip_prefix("marker:") {
                write_raw(&path, format!(r#"{{"version":1,"namespace":"{ns}"}}"#).as_bytes());
            } else if let Some(gitdir) = kind.strip_prefix("git-file:") {
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join(".git"), format!("gitdir: {gitdir}\n")).unwrap();
            } else if let Some(common) = kind.strip_prefix("commondir:") {
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("commondir"), format!("{common}\n")).unwrap();
            } else if let Some(target) = kind.strip_prefix("neurostrata-link:") {
                std::fs::create_dir_all(&path).unwrap();
                assert!(
                    link_dir(&under(base, target), &path.join(".NeuroStrata")),
                    "could not create a link for {entry}"
                );
            } else {
                match kind {
                    "git-dir" => std::fs::create_dir_all(path.join(".git")).unwrap(),
                    "neurostrata-dir" => std::fs::create_dir_all(path.join(".NeuroStrata")).unwrap(),
                    "file" => {
                        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                        std::fs::write(&path, b"").unwrap();
                    }
                    other => panic!("unknown layout kind {other:?} in {entry}"),
                }
            }
        }
    }

    #[test]
    fn fixture_cases_match() {
        let fixture: serde_json::Value = serde_json::from_str(CASES).unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() >= 12, "{} cases", cases.len());
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let base = dunce::canonicalize(tmp.path()).unwrap();
            build_layout(&base, case["layout"].as_array().unwrap());
            let expect = &case["expect"];
            if expect["via"] == "no_git" && base.ancestors().any(|a| a.join(".git").exists()) {
                // The temp folder sits inside a checkout on this host; the
                // case cannot be outside git here.
                eprintln!("skipped {name:?}: the temp folder is inside a git checkout");
                continue;
            }
            let got = find_root_marker(&under(tmp.path(), case["query"].as_str().unwrap()))
                .unwrap_or_else(|(path, reason)| panic!("{name}: unreadable {path:?}: {reason}"));
            match expect["root"].as_str() {
                None => assert_eq!(got, None, "{name}"),
                Some(root) => {
                    let got = got.unwrap_or_else(|| panic!("{name}: no marker found"));
                    assert_eq!(got.root, under(&base, root), "{name}");
                    assert_eq!(got.config.namespace, expect["namespace"].as_str().unwrap(), "{name}");
                    assert_eq!(got.via.as_str(), expect["via"].as_str().unwrap(), "{name}");
                }
            }
        }
    }

    #[test]
    fn main_checkout_marker_file_is_in_main() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        build_layout(
            &base,
            &serde_json::from_str::<Vec<serde_json::Value>>(
                r#"["repo git-dir", "repo/packages/api marker:example-api",
                    "repo/.git/worktrees/wt commondir:../..", "wt/packages/api",
                    "wt git-file:../repo/.git/worktrees/wt"]"#,
            )
            .unwrap(),
        );
        let found = find_root_marker(&base.join("wt/packages/api")).unwrap().unwrap();
        assert_eq!(found.via, FoundVia::MainCheckout);
        assert_eq!(found.root, base.join("wt/packages/api"));
        assert_eq!(found.marker_dir(), base.join("repo/packages/api"));
    }

    #[cfg(windows)]
    #[test]
    fn marker_found_from_any_windows_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub").join("deep")).unwrap();
        write_raw(&repo, br#"{"version":1,"namespace":"example-project"}"#);

        let sub = repo.join("sub").to_string_lossy().into_owned();
        let (drive, rest) = sub.split_at(1);
        let spellings = [
            format!("{}{rest}", drive.to_ascii_lowercase()),
            format!("{}{rest}", drive.to_ascii_uppercase()),
            sub.replace('\\', "/"),
            format!("{}/deep/", sub.replace('\\', "/")),
        ];
        for spelling in &spellings {
            let found = find_root_marker(Path::new(spelling)).unwrap().unwrap();
            assert_eq!(found.root, repo, "{spelling}");
            assert_eq!(found.via, FoundVia::OwnCheckout, "{spelling}");
        }

        let junction = base.join("alias");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(repo.join("sub"))
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&made.stderr));
        for through in [junction.clone(), junction.join("deep")] {
            let found = find_root_marker(&through).unwrap().unwrap();
            assert_eq!(found.root, repo, "{}", through.display());
        }
    }

    #[test]
    fn unreadable_marker_on_walk_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write_raw(&repo, br#"{"version":1,"namespace":"example-project"}"#);
        let sub = repo.join("sub");
        write_raw(&sub, b"{ not json");
        std::fs::create_dir_all(sub.join("x")).unwrap();

        // The deepest marker is unreadable: an error naming it, not the
        // valid marker above.
        let (path, reason) = find_root_marker(&sub.join("x")).unwrap_err();
        assert_eq!(path, sub.join(ROOT_CONFIG_REL));
        assert!(!reason.is_empty());

        // A valid marker deeper still decides; the unreadable one is above it.
        write_raw(&sub.join("x"), br#"{"version":1,"namespace":"example-x"}"#);
        let found = find_root_marker(&sub.join("x")).unwrap().unwrap();
        assert_eq!(found.config.namespace, "example-x");

        // Outside git as well.
        let plain = base.join("plain");
        write_raw(&plain, br#"{"version":2,"namespace":"example-project"}"#);
        std::fs::create_dir_all(plain.join("a")).unwrap();
        if !base.ancestors().any(|a| a.join(".git").exists()) {
            assert!(find_root_marker(&plain.join("a")).is_err());
        }
    }

    #[test]
    fn root_show_lists_nested_markers_and_missing_bindings() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write_raw(&repo, br#"{"version":1,"namespace":"example-project","source_id":"example-src"}"#);
        write_raw(&repo.join("packages/api"), br#"{"version":1,"namespace":"example-api"}"#);
        write_raw(&repo.join("packages/bad"), b"nope");
        // A nested clone's marker is another checkout's, not listed.
        std::fs::create_dir_all(repo.join("clone/.git")).unwrap();
        write_raw(&repo.join("clone"), br#"{"version":1,"namespace":"example-clone"}"#);

        let found = find_root_marker(&repo);
        let bindings = std::collections::HashMap::from([
            ("gone".to_string(), base.join("gone/.NeuroStrata")),
            ("here".to_string(), repo.join(".NeuroStrata")),
        ]);
        let text = format_root_show(
            &repo,
            &found,
            "example-project",
            &CachedRoot::Known(Some(base.join("old").to_string_lossy().into_owned())),
            &bindings,
        );
        assert!(text.contains(&format!("marker root: {}", repo.display())), "{text}");
        assert!(text.contains("found via: own_checkout"), "{text}");
        assert!(text.contains("source_id: example-src"), "{text}");
        assert!(text.contains("(missing)"), "{text}");
        assert!(text.contains("hint: the marker root differs"), "{text}");
        assert!(text.contains("nested markers below this folder"), "{text}");
        assert!(text.contains("(namespace example-api)"), "{text}");
        assert!(text.contains("(unreadable:"), "{text}");
        assert!(!text.contains("example-clone"), "{text}");
        assert!(text.contains(&format!("  gone -> {}", base.join("gone/.NeuroStrata").display())), "{text}");
        assert!(!text.contains("  here ->"), "{text}");

        let empty = tempfile::tempdir().unwrap();
        let text = format_root_show(
            empty.path(),
            &Ok(None),
            "example-project",
            &CachedRoot::Unknown("no daemon, store locked".into()),
            &std::collections::HashMap::new(),
        );
        assert!(text.contains("cached root: unknown (no daemon, store locked)"), "{text}");
        assert!(text.contains("other checkouts not entered): none"), "{text}");
        assert!(text.contains("missing folders: none"), "{text}");
    }
}
