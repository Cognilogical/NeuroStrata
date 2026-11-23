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

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Serializes marker writes and the verify-then-unlink of superseded markers
/// within this process (Codex review 4670091232). Without a shared lock a
/// concurrent `root set` can replace an old marker between its re-read and
/// its removal, deleting the successor and leaving the cache unmarked.
static MARKER_MUTATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    /// When present it must be a string: `null` is refused, as the schema
    /// refuses it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_string"
    )]
    pub source_id: Option<String>,
    /// Fields this build does not know; kept so a rewrite does not drop them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Called only for a field that is present, so `null` is an error rather
/// than `None`.
fn present_string<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    String::deserialize(de).map(Some)
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
    let reported = dir.join(ROOT_CONFIG_REL);
    let unreadable = |reason: String| MarkerRead::Unreadable {
        path: reported.clone(),
        reason,
    };
    let path = match contained_marker_path(dir) {
        Ok(Some(path)) => path,
        Ok(None) => return MarkerRead::Absent,
        Err(reason) => return unreadable(reason),
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return MarkerRead::Absent,
        Err(err) => return unreadable(err.to_string()),
    };
    parse_marker_bytes(&reported, &bytes)
}

/// Resolve the marker through canonical paths and refuse one that leaves
/// `dir`. A caller may hand `read_marker` a path derived from a request; a
/// symlinked `.NeuroStrata` or `config.json` must not turn that read into an
/// arbitrary-file read (CodeQL rust/path-injection). `None` means the folder
/// itself does not exist, which keeps `read_marker`'s `Absent` answer.
fn contained_marker_path(dir: &Path) -> Result<Option<PathBuf>, String> {
    let canonical_dir = match dir.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let candidate = canonical_dir.join(ROOT_CONFIG_REL);
    let canonical_file = match candidate.canonicalize() {
        Ok(path) => path,
        // No marker yet: the non-canonical candidate still reads as Absent.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Some(candidate)),
        Err(err) => return Err(err.to_string()),
    };
    if !canonical_file.starts_with(&canonical_dir) {
        return Err("root marker resolves outside the target directory".to_string());
    }
    Ok(Some(canonical_file))
}

/// Applies [`read_marker`]'s rules (BOM accepted; bad JSON, an unknown
/// `version` or a bad `namespace` is `Unreadable`) to a marker's bytes read
/// from anywhere, such as a commit. `path` only names the file in the answer.
pub fn parse_marker_bytes(path: &Path, bytes: &[u8]) -> MarkerRead {
    let unreadable = |reason: String| MarkerRead::Unreadable {
        path: path.to_path_buf(),
        reason,
    };
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
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
    let _guard = MARKER_MUTATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _guard = MARKER_MUTATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        return Err(MarkerError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        )));
    }
    if !marker_dir_is_safe(dir) {
        return Err(MarkerError::Unsafe(
            ".NeuroStrata is not a real folder inside the target",
        ));
    }
    let mut body = serde_json::to_vec_pretty(cfg).map_err(|err| {
        MarkerError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err))
    })?;
    body.push(b'\n');
    let target = dir.join(ROOT_CONFIG_REL);
    let tmp = temp_name(&target);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
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
    target.with_file_name(format!(
        "config.json.tmp-{}-{nanos}-{n}",
        std::process::id()
    ))
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
        let is_real_dir = entry
            .file_type()
            .is_ok_and(|t| t.is_dir() && !t.is_symlink());
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
        in_main_checkout(&self.root)
    }
}

/// `dir` spelled in its main checkout when it lies in a linked worktree (the
/// main checkout's top joined with the part below the worktree's top), else
/// `dir` itself. The registry caches a worktree's root in this spelling.
pub(crate) fn in_main_checkout(dir: &Path) -> PathBuf {
    checkout_top(dir)
        .and_then(|top| {
            let main = linked_main_checkout(&top)?;
            Some(main.join(dir.strip_prefix(&top).ok()?))
        })
        .unwrap_or_else(|| dir.to_path_buf())
}

/// The marker that decides `path`'s project root (spec 6.1), or `None`.
///
/// For a file the walk starts at its folder. That folder is canonicalized
/// through its nearest existing ancestor, so any spelling of it (drive letter
/// case, `/`, a junction or symlink) finds the same marker and the root comes
/// back in canonical spelling. A file symlink itself is not followed: the
/// link's own folder decides, so a link into another checkout does not
/// carry that checkout's marker. A path that is not absolute on this host (a
/// POSIX path on Windows, say) cannot be read here and finds none.
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

/// The folder a walk starts from, or `None` for a path that is not
/// absolute on this host. For a file it is the file's folder, canonicalized
/// itself: a file symlink is not followed, so a link to a file in another
/// checkout does not take that checkout's marker.
fn start_folder(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    if path.is_file() {
        return path.parent().map(canonical_through_existing);
    }
    Some(canonical_through_existing(path))
}

/// `path` canonicalized through its nearest existing ancestor, with the
/// missing part appended; `path` unchanged when no ancestor resolves.
fn canonical_through_existing(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(canonical) = dunce::canonicalize(existing) {
            return missing.iter().rev().fold(canonical, |p, part| p.join(part));
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The nearest folder at or above `dir` that holds `.git` (a directory or a
/// worktree's file), in `dir`'s spelling: the rule of
/// [`crate::paths::git_root_of_dir`], which answers with `/` on Windows.
fn checkout_top(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
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
    // `.git`, at the first folder missing from `main`, and at a symlink or
    // junction or any folder whose canonical form leaves `main`: a link in
    // the main checkout may point into another repository, whose markers
    // are not this worktree's.
    let main_canonical = dunce::canonicalize(main).unwrap_or_else(|_| main.to_path_buf());
    let mut allowed = vec![main.to_path_buf()];
    let mut dir = main.to_path_buf();
    for part in rel.components() {
        dir.push(part);
        let real_dir = std::fs::symlink_metadata(&dir)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
        if !real_dir
            || dir.join(".git").exists()
            || !dunce::canonicalize(&dir).is_ok_and(|c| c.starts_with(&main_canonical))
        {
            break;
        }
        allowed.push(dir.clone());
    }
    allowed.into_iter().rev().find_map(|dir| marker_at(&dir))
}

/// The marker in `dir` when there is one to report, valid or unreadable. A
/// `.NeuroStrata` that is a link pointing elsewhere, or a folder without
/// `config.json`, is none. Only a missing `.NeuroStrata` is absent: one that
/// cannot be inspected (no permission, say) is `Unreadable`, so the walk
/// does not silently pass it for a marker further up.
fn marker_at(dir: &Path) -> Option<(PathBuf, MarkerRead)> {
    let folder = dir.join(".NeuroStrata");
    match std::fs::symlink_metadata(&folder) {
        Ok(_) => {}
        // NotADirectory: a step of a missing path that is a file.
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return None
        }
        Err(err) => {
            return Some((
                dir.to_path_buf(),
                MarkerRead::Unreadable {
                    path: folder,
                    reason: err.to_string(),
                },
            ))
        }
    }
    if !crate::source_context::is_valid_marker_directory(&folder) {
        return None;
    }
    match read_marker(dir) {
        MarkerRead::Absent => None,
        read => Some((dir.to_path_buf(), read)),
    }
}

fn depth_below(dir: &Path, top: &Path) -> usize {
    dir.strip_prefix(top)
        .map(|rel| rel.components().count())
        .unwrap_or(0)
}

/// How deep `root show` looks below its folder for other markers. Gate and
/// duplicate checks call [`nested_markers_up_to`] with no cap instead.
pub const NESTED_MARKER_MAX_DEPTH: usize = 8;

/// Folders below `dir` (not `dir` itself) holding a marker, valid or not,
/// down to [`NESTED_MARKER_MAX_DEPTH`] levels. Links are not followed, and
/// `node_modules`, `target`, `.git` and folders holding their own `.git`
/// (another checkout) are not entered.
pub fn nested_markers(dir: &Path) -> Vec<(PathBuf, MarkerRead)> {
    let mut truncated = false;
    let found = nested_markers_up_to_report(dir, NESTED_MARKER_MAX_DEPTH, &mut truncated);
    if truncated {
        tracing::warn!(
            "the marker walk below {} stopped at depth {}; use `root show` from a shallower folder to see deeper markers",
            dir.display(),
            NESTED_MARKER_MAX_DEPTH
        );
    }
    found
}

/// Every marker below `dir`, without the display cap. Used where a missed
/// marker would cause a duplicate write or an ungated push.
pub fn nested_markers_uncapped(dir: &Path) -> Vec<(PathBuf, MarkerRead)> {
    let mut truncated = false;
    nested_markers_up_to_report(dir, usize::MAX, &mut truncated)
}

/// The gate's walk: every marker below `dir`, including unreadable ones. An
/// unreadable marker is returned as `MarkerRead::Unreadable` so callers can
/// fail closed instead of treating the folder as unmarked.
pub fn nested_markers_for_gate(dir: &Path) -> Vec<(PathBuf, MarkerRead)> {
    nested_markers_uncapped(dir)
}

/// Folders below `dir` holding a marker, down to `max_depth` levels.
pub fn nested_markers_up_to(dir: &Path, max_depth: usize) -> Vec<(PathBuf, MarkerRead)> {
    let mut truncated = false;
    nested_markers_up_to_report(dir, max_depth, &mut truncated)
}

fn nested_markers_up_to_report(
    dir: &Path,
    max_depth: usize,
    truncated: &mut bool,
) -> Vec<(PathBuf, MarkerRead)> {
    let mut found = Vec::new();
    collect_nested(dir, 1, max_depth, &mut found, truncated);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn collect_nested(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    found: &mut Vec<(PathBuf, MarkerRead)>,
    truncated: &mut bool,
) {
    if depth > max_depth {
        *truncated = true;
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .is_ok_and(|t| t.is_dir() && !t.is_symlink())
        {
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
        collect_nested(&path, depth + 1, max_depth, found, truncated);
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
/// to without a marker; `bindings` are the configured project contexts, or
/// why the user config could not be read. Nested markers are listed below
/// the marker root (below `dir` when no marker applies); in a linked
/// worktree, also those that exist only in the main checkout, since they
/// apply to the worktree too.
pub fn format_root_show(
    dir: &Path,
    found: &Result<Option<FoundMarker>, (PathBuf, String)>,
    namespace: &str,
    cached: &CachedRoot,
    bindings: &Result<std::collections::HashMap<String, PathBuf>, String>,
) -> String {
    let mut out = vec![format!("folder: {}", dir.display())];
    let mut marker_root = None;
    match found {
        Ok(Some(found)) => {
            let marker_dir = found.marker_dir();
            out.push(format!("marker root: {}", found.root.display()));
            out.push(format!(
                "marker file: {}",
                marker_dir.join(ROOT_CONFIG_REL).display()
            ));
            out.push(format!("found via: {}", found.via.as_str()));
            out.push(format!("namespace: {}", found.config.namespace));
            out.push(format!(
                "source_id: {}",
                found.config.source_id.as_deref().unwrap_or("none")
            ));
            out.push(format!(
                "git tracks the marker: {}",
                git_tracks_marker(&marker_dir, found.via)
            ));
            marker_root = Some(found.root.clone());
        }
        Ok(None) => {
            out.push("marker root: none (no .NeuroStrata/config.json applies here)".to_string());
            out.push(format!("namespace without a marker: {namespace}"));
        }
        Err((path, reason)) => {
            out.push(format!(
                "marker root: unreadable marker {}: {reason}",
                path.display()
            ));
            out.push(format!("namespace without a marker: {namespace}"));
        }
    }
    for folder in miscased_marker_folders(dir) {
        out.push(format!(
            "note: {} is not spelled .NeuroStrata, so it is not a marker",
            folder.display()
        ));
    }
    match cached {
        CachedRoot::Known(Some(root)) => {
            let exists = Path::new(root).is_dir();
            out.push(format!(
                "cached root for {namespace}: {root} ({})",
                if exists { "exists" } else { "missing" }
            ));
            // A worktree's root is cached in the main checkout's spelling.
            let differs = marker_root.as_deref().is_some_and(|m| {
                !same_folder(Path::new(root), m)
                    && !same_folder(Path::new(root), &in_main_checkout(m))
            });
            if differs {
                out.push(
                    "hint: the marker root differs from the cached root; lookups do not change. \
                     `neurostrata-mcp root set <marker root>` or `neurostrata-mcp hooks install` \
                     updates the cached root"
                        .to_string(),
                );
            }
        }
        CachedRoot::Known(None) => out.push(format!("cached root for {namespace}: none")),
        CachedRoot::Unknown(why) => out.push(format!("cached root: unknown ({why})")),
    }
    let walk_root = marker_root.unwrap_or_else(|| dir.to_path_buf());
    let nested = nested_markers(&walk_root);
    // A split marker kept only in the main checkout (uncommitted, say) still
    // decides paths in this worktree; one the worktree has itself wins.
    let main_root = in_main_checkout(&walk_root);
    let main_only: Vec<_> = if main_root == walk_root {
        Vec::new()
    } else {
        nested_markers(&main_root)
            .into_iter()
            .filter(|(path, _)| {
                let rel = path.strip_prefix(&main_root).ok();
                !nested
                    .iter()
                    .any(|(own, _)| own.strip_prefix(&walk_root).ok() == rel)
            })
            .collect()
    };
    let count = nested.len() + main_only.len();
    out.push(format!(
        "nested markers below {} (up to {NESTED_MARKER_MAX_DEPTH} levels, other checkouts not entered): {}",
        walk_root.display(),
        if count == 0 { "none".to_string() } else { count.to_string() }
    ));
    let labeled = nested.iter().map(|hit| (hit, "")).chain(
        main_only
            .iter()
            .map(|hit| (hit, ", only in the main checkout")),
    );
    for ((path, read), label) in labeled {
        match read {
            MarkerRead::Valid(cfg) => out.push(format!(
                "  {} (namespace {}{label})",
                path.display(),
                cfg.namespace
            )),
            MarkerRead::Unreadable { reason, .. } => out.push(format!(
                "  {} (unreadable: {reason}{label})",
                path.display()
            )),
            MarkerRead::Absent => {}
        }
    }
    match bindings {
        Ok(bindings) => {
            let mut missing: Vec<_> = bindings.iter().filter(|(_, path)| !path.exists()).collect();
            missing.sort();
            out.push(format!(
                "project-context bindings to missing folders: {}",
                if missing.is_empty() {
                    "none".to_string()
                } else {
                    missing.len().to_string()
                }
            ));
            for (source_id, path) in missing {
                out.push(format!("  {source_id} -> {}", path.display()));
            }
        }
        Err(why) => out.push(format!(
            "project-context bindings to missing folders: unknown (config could not be read: {why})"
        )),
    }
    out.join("\n")
}

/// Folders named `.NeuroStrata` in another letter case in `dir` or the
/// folders above it that a marker walk visits (up to the checkout top, or
/// every ancestor outside git). Such a folder is not a marker, also where
/// the filesystem ignores case.
fn miscased_marker_folders(dir: &Path) -> Vec<PathBuf> {
    let top = checkout_top(dir);
    let mut found = Vec::new();
    for folder in dir.ancestors() {
        if top.as_deref().is_some_and(|top| !folder.starts_with(top)) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name != ".NeuroStrata" && name.eq_ignore_ascii_case(".NeuroStrata") {
                found.push(entry.path());
            }
        }
    }
    found
}

/// Whether `a` and `b` name one folder: by spelling, else on disk (through
/// links and the nearest existing ancestor), by the rule the path lookups
/// use. `relative_to_on_disk` answers only for a path strictly below the
/// root, so a child name that does not exist is compared.
pub(crate) fn same_folder(a: &Path, b: &Path) -> bool {
    const PROBE: &str = ".neurostrata-same-folder-probe";
    let probe = a.join(PROBE);
    crate::paths::relative_to_on_disk(&probe.to_string_lossy(), &b.to_string_lossy()).as_deref()
        == Some(PROBE)
}

// ── Writers (spec 6.4, 6.8) ─────────────────────────────────────────────────

/// The guide for correcting data before a root changes.
pub const ROOT_GUIDE: &str = "docs/features-api/ops/project-root.md";

/// What setup, bootstrap and `hooks install` did about the marker.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupMarker {
    /// A marker for this namespace already covers the project; this is its root.
    Reused(PathBuf),
    /// A new marker was written in this folder.
    Written(PathBuf),
    /// A marker for the namespace covers `root` (written now when `written`),
    /// and the cached root stays `cached`: a usable folder outside `root`'s
    /// tree (another clone, or an older copy). Only `root set` moves it.
    KeptCache {
        root: PathBuf,
        cached: PathBuf,
        written: bool,
    },
    /// The cached root is a subfolder of the checkout top: nothing written,
    /// the caller picks one with `root set`.
    AskRoot { top: PathBuf, sub: PathBuf },
    /// `hooks install` without `--namespace` and no single explicit
    /// namespace: these are the candidates.
    AskNamespace(Vec<String>),
    /// A marker for this other namespace covers the project: nothing written
    /// and nothing installed.
    OtherNamespace(String),
    /// Writing would move the namespace's root and these memories would stop
    /// matching.
    RootChangeBlocked(Vec<String>),
    /// The folder may not hold a marker, or the write failed, for this reason.
    Refused(&'static str),
    /// A marker for the namespace covers `root` (written now when `written`),
    /// but the cached root could not be set to it, for `reason`: the marker
    /// spelling stays off until it is. A failure: `hooks install` exits 1.
    CacheNotSet {
        root: PathBuf,
        written: bool,
        reason: String,
    },
}

impl SetupMarker {
    pub fn kind(&self) -> &'static str {
        match self {
            SetupMarker::Reused(_) => "reused",
            SetupMarker::Written(_) => "written",
            SetupMarker::KeptCache { written: true, .. } => "written",
            SetupMarker::KeptCache { written: false, .. } => "reused",
            SetupMarker::AskRoot { .. } => "ask_root",
            SetupMarker::AskNamespace(_) => "ask_namespace",
            SetupMarker::OtherNamespace(_) => "other_namespace",
            SetupMarker::RootChangeBlocked(_) => "root_change_blocked",
            SetupMarker::Refused(_) => "refused",
            SetupMarker::CacheNotSet { .. } => "cache_not_set",
        }
    }

    /// Whether the step failed in a way the caller must report as an error.
    pub fn failed(&self) -> bool {
        matches!(self, SetupMarker::CacheNotSet { .. })
    }

    /// The marker root, when a marker for the namespace now covers the project.
    pub fn root(&self) -> Option<&Path> {
        match self {
            SetupMarker::Reused(root) | SetupMarker::Written(root) => Some(root),
            SetupMarker::KeptCache { root, .. } | SetupMarker::CacheNotSet { root, .. } => {
                Some(root)
            }
            _ => None,
        }
    }

    /// Harness plugins are installed unless another namespace owns the folder.
    pub fn installs_plugins(&self) -> bool {
        !matches!(self, SetupMarker::OtherNamespace(_))
    }

    /// The one line a reply carries, or `None` when a marker was reused.
    pub fn message(&self, namespace: &str, project_dir: &Path) -> Option<String> {
        let dir = project_dir.display();
        Some(match self {
            SetupMarker::Reused(_) => return None,
            SetupMarker::Written(root) => format!(
                "Wrote {} for {namespace}.",
                root.join(ROOT_CONFIG_REL).display()
            ),
            SetupMarker::KeptCache { root, cached, written } => format!(
                "{} {} for {namespace}. The cached root of {namespace} stays {}, \
                 a folder outside this one. Run `neurostrata-mcp root set {} --namespace {namespace}` \
                 to make {} the cached root.",
                if *written { "Wrote" } else { "Using" },
                root.join(ROOT_CONFIG_REL).display(),
                cached.display(),
                root.display(),
                root.display()
            ),
            SetupMarker::AskRoot { top, sub } => format!(
                "No .NeuroStrata/config.json written: the cached root of {namespace} is the subfolder {}. \
                 Run `neurostrata-mcp root set {} --namespace {namespace}` to keep today's stored keys, \
                 or `neurostrata-mcp root set {} --namespace {namespace}` after applying the corrections \
                 `neurostrata-mcp root audit --for-root {}` lists ({ROOT_GUIDE}).",
                sub.display(),
                sub.display(),
                top.display(),
                top.display()
            ),
            SetupMarker::AskNamespace(candidates) => format!(
                "No .NeuroStrata/config.json written: no single namespace is configured for this project \
                 (candidates: {}). Run `neurostrata-mcp hooks install --namespace <ns>`.",
                candidates.join(", ")
            ),
            SetupMarker::OtherNamespace(other) => format!(
                "{dir} belongs to namespace {other} (.NeuroStrata/config.json), so nothing was installed \
                 for {namespace}. To change it, run `neurostrata-mcp root set {dir} --namespace {namespace} --replace`."
            ),
            SetupMarker::RootChangeBlocked(ids) => format!(
                "No .NeuroStrata/config.json written: it would move the root of {namespace}, and {} \
                 memories would stop matching ({}). Run `neurostrata-mcp root audit --for-root {dir}` and correct them \
                 ({ROOT_GUIDE}), or run `neurostrata-mcp root set {dir} --namespace {namespace} --force`.",
                ids.len(),
                ids.join(", ")
            ),
            SetupMarker::Refused(reason) => {
                format!("No .NeuroStrata/config.json written: {reason}.")
            }
            SetupMarker::CacheNotSet { root, written, reason } => format!(
                "{} {} for {namespace}, but the cached root of {namespace} could not be set to {} ({reason}), \
                 so lookups do not use the marker yet. Run `neurostrata-mcp root set {} --namespace {namespace}` to set it.",
                if *written { "Wrote" } else { "Using" },
                root.join(ROOT_CONFIG_REL).display(),
                root.display(),
                root.display()
            ),
        })
    }

    /// The outcome as a reply field.
    pub fn to_json(&self, namespace: &str, project_dir: &Path) -> serde_json::Value {
        let mut out = serde_json::json!({ "kind": self.kind() });
        match self {
            SetupMarker::Reused(root) | SetupMarker::Written(root) => {
                out["root"] = serde_json::json!(root.to_string_lossy());
            }
            SetupMarker::KeptCache { root, cached, .. } => {
                out["root"] = serde_json::json!(root.to_string_lossy());
                out["cached_root"] = serde_json::json!(cached.to_string_lossy());
            }
            SetupMarker::AskRoot { top, sub } => {
                out["top"] = serde_json::json!(top.to_string_lossy());
                out["sub"] = serde_json::json!(sub.to_string_lossy());
            }
            SetupMarker::AskNamespace(candidates) => {
                out["candidates"] = serde_json::json!(candidates)
            }
            SetupMarker::OtherNamespace(other) => out["namespace"] = serde_json::json!(other),
            SetupMarker::RootChangeBlocked(ids) => out["memory_ids"] = serde_json::json!(ids),
            SetupMarker::Refused(reason) => out["reason"] = serde_json::json!(reason),
            SetupMarker::CacheNotSet { root, reason, .. } => {
                out["root"] = serde_json::json!(root.to_string_lossy());
                out["reason"] = serde_json::json!(reason);
            }
        }
        out["message"] = serde_json::json!(self.message(namespace, project_dir));
        out
    }
}

/// The hint other calls add when `path`'s project has no marker (spec 6.4):
/// `None` when a marker covers `path` (for this namespace or another), when
/// the marker there is unreadable (reported elsewhere), or when `path` is not
/// an existing absolute path, the home folder or a filesystem root.
pub fn root_hint(namespace: &str, path: &Path) -> Option<String> {
    if namespace.trim().is_empty() || !path.is_absolute() || !path.exists() {
        return None;
    }
    if !matches!(find_root_marker(path), Ok(None)) {
        return None;
    }
    no_marker_hint(namespace, path)
}

/// [`root_hint`] for a caller that already knows `find_root_marker(path)` is
/// `Ok(None)`, so the marker walk is not repeated.
pub fn no_marker_hint(namespace: &str, path: &Path) -> Option<String> {
    if namespace.trim().is_empty() || !path.is_absolute() || !path.exists() {
        return None;
    }
    let folder = start_folder(path)?;
    if folder.parent().is_none()
        || crate::task::is_home_folder(&folder, dirs::home_dir().as_deref())
    {
        return None;
    }
    Some(format!(
        "No .NeuroStrata/config.json for {namespace}: run `neurostrata-mcp hooks install` or neurostrata_task_setup"
    ))
}

/// The system temp folder (and `/tmp`): a marker there would capture every
/// temporary project below it.
fn is_temp_root(dir: &Path) -> bool {
    [std::env::temp_dir(), PathBuf::from("/tmp")]
        .iter()
        .filter_map(|t| dunce::canonicalize(t).ok())
        .any(|t| t == dir)
}

/// What the marker writers need from the registry: the cached root, the
/// namespace's rows (for the root-change check) and a way to set the cached
/// root. [`StoreRegistry`] reads a store; the CLI also implements it over the
/// daemon's `/cli/read` and `/cli/root`, so all file work stays in the CLI
/// process and the daemon only changes the cache.
#[async_trait::async_trait]
pub trait RootRegistry: Send + Sync {
    async fn cached_root(&self, namespace: &str) -> Result<Option<String>, String>;
    async fn rows(&self, namespace: &str) -> Result<Vec<crate::traits::SearchResult>, String>;
    /// Set the cached root to `root`; `Err` says why it was not set.
    async fn set_root(&self, namespace: &str, root: &Path) -> Result<(), String>;
    /// Called once the writer is done (a store opened by the CLI checkpoints).
    async fn finish(&self) {}
}

/// [`RootRegistry`] over a store, for MCP setup/bootstrap and the CLI when no
/// daemon answers. Every call uses the namespace as the store spells it
/// (matched case-insensitively), as the daemon's routes do.
pub struct StoreRegistry {
    pub store: std::sync::Arc<dyn crate::traits::VectorStore>,
    pub emb: Option<std::sync::Arc<dyn crate::traits::Embedder>>,
    /// Checkpoint in [`RootRegistry::finish`] (a store the CLI opened itself).
    pub checkpoint: bool,
}

#[async_trait::async_trait]
impl RootRegistry for StoreRegistry {
    async fn cached_root(&self, namespace: &str) -> Result<Option<String>, String> {
        let namespace = crate::server::resolve_namespace(&self.store, namespace).await;
        crate::registry::cached_project_root(&self.store, &namespace)
            .await
            .map_err(|e| e.to_string())
    }

    async fn rows(&self, namespace: &str) -> Result<Vec<crate::traits::SearchResult>, String> {
        let namespace = crate::server::resolve_namespace(&self.store, namespace).await;
        self.store
            .list(&namespace, None, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn set_root(&self, namespace: &str, root: &Path) -> Result<(), String> {
        let namespace = crate::server::resolve_namespace(&self.store, namespace).await;
        if crate::registry::set_project_root(&self.store, self.emb.as_ref(), &namespace, root).await
        {
            Ok(())
        } else {
            Err("the registry row could not be written".to_string())
        }
    }

    async fn finish(&self) {
        if self.checkpoint {
            if let Err(e) = self.store.checkpoint().await {
                tracing::warn!("checkpoint after the root marker write failed: {e}");
            }
        }
    }
}

/// The usable cached root of `namespace`, canonicalized, or `None`.
async fn usable_cached_root(
    registry: &dyn RootRegistry,
    namespace: &str,
) -> Result<Option<PathBuf>, String> {
    let cached = registry.cached_root(namespace).await?;
    Ok(cached
        .filter(|r| crate::paths::is_usable_root(r))
        .map(|r| dunce::canonicalize(&r).unwrap_or_else(|_| PathBuf::from(r))))
}

/// Whether `a` and `b` name the same root once both are spelled in their main
/// checkout (a linked worktree's top is the same root as its main checkout's).
fn same_root(a: &Path, b: &Path) -> bool {
    same_folder(&in_main_checkout(a), &in_main_checkout(b))
}

/// Whether one of `a` and `b` contains the other (main-checkout spelling):
/// a root change inside one tree. Stored keys can only be compared there.
fn same_tree(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| {
        let p = in_main_checkout(p);
        dunce::canonicalize(&p).unwrap_or(p)
    };
    let (a, b) = (canon(a), canon(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// The memories that would stop matching if the cached root moved from
/// `current` to `new`. Empty when the roots are the same. Inside one tree a
/// key is lost when the new root spells its file differently; between
/// unrelated folders (a sibling, another clone) it is lost when its file
/// exists under `current` but not under `new`.
async fn lost_by_change(
    registry: &dyn RootRegistry,
    namespace: &str,
    current: Option<&Path>,
    new: &Path,
) -> Result<Vec<String>, String> {
    match current {
        Some(current) if !same_root(current, new) => {
            let rows = registry.rows(namespace).await?;
            Ok(if same_tree(current, new) {
                crate::root_audit::rows_lost_in(&rows, current, new)
            } else {
                crate::root_audit::rows_missing_under(&rows, current, new)
            })
        }
        _ => Ok(Vec::new()),
    }
}

/// Setup's root-change check: none when setup keeps an unrelated cached root
/// (it does not move it), else [`lost_by_change`].
async fn setup_loss_check(
    registry: &dyn RootRegistry,
    namespace: &str,
    cached: Option<&Path>,
    root: &Path,
) -> Result<Vec<String>, String> {
    if unrelated_cache(cached, root).is_some() {
        return Ok(Vec::new());
    }
    lost_by_change(registry, namespace, cached, root).await
}

/// A usable cached root outside `root`'s tree (another clone, or an older
/// copy), which setup and `hooks install` keep: only `root set` moves the
/// cache to an unrelated folder.
fn unrelated_cache(cached: Option<&Path>, root: &Path) -> Option<PathBuf> {
    let cached = cached?;
    (!same_tree(cached, root)).then(|| cached.to_path_buf())
}

/// Setup's cache update: nothing when the cache already is `root`, nothing
/// when it is an unrelated usable folder (returned), else set it (missing,
/// unusable, or in the same tree after the root-change check). `Err` says why
/// the cache could not be set.
async fn settle_cache(
    registry: &dyn RootRegistry,
    namespace: &str,
    cached: Option<&Path>,
    root: &Path,
) -> Result<Option<PathBuf>, String> {
    if cached.is_some_and(|c| same_root(c, root)) {
        return Ok(None);
    }
    if let Some(kept) = unrelated_cache(cached, root) {
        return Ok(Some(kept));
    }
    registry.set_root(namespace, root).await?;
    Ok(None)
}

/// The marker step of `neurostrata_task_setup`, `neurostrata_bootstrap` and
/// `hooks install` (spec 6.4) against a store. See [`ensure_marker_with`].
pub async fn ensure_marker_for_setup(
    store: &std::sync::Arc<dyn crate::traits::VectorStore>,
    emb: Option<&std::sync::Arc<dyn crate::traits::Embedder>>,
    project_dir: &Path,
    namespace: Option<&str>,
) -> SetupMarker {
    let registry = StoreRegistry {
        store: store.clone(),
        emb: emb.cloned(),
        checkpoint: false,
    };
    ensure_marker_with(&registry, project_dir, namespace).await
}

/// The marker step (spec 6.4). `namespace` is `None` only for `hooks install`
/// without `--namespace`, which then uses
/// [`crate::task::harness_plugins::explicit_namespace`]. Sets the cached root
/// to the marker root when a marker for the namespace now covers the folder,
/// except when the cached root is another clone with its own marker.
pub async fn ensure_marker_with(
    registry: &dyn RootRegistry,
    project_dir: &Path,
    namespace: Option<&str>,
) -> SetupMarker {
    if !project_dir.is_absolute() || !project_dir.is_dir() {
        return SetupMarker::Refused("the project folder is not an existing absolute folder");
    }
    let dir = dunce::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    if is_temp_root(&dir) {
        return SetupMarker::Refused("the system temp folder is not a project root");
    }
    let namespace = match namespace {
        Some(ns) => ns.to_string(),
        None => match crate::task::harness_plugins::explicit_namespace(&dir) {
            Ok(ns) => ns,
            Err(candidates) => return SetupMarker::AskNamespace(candidates),
        },
    };
    if !is_valid_namespace(&namespace) {
        return SetupMarker::Refused(
            "the namespace does not fit .NeuroStrata/config.json (letters, digits, '_', '.', ':' and '-', starting with a letter or digit)",
        );
    }
    let cached = match usable_cached_root(registry, &namespace).await {
        Ok(cached) => cached,
        Err(e) => {
            tracing::warn!("the cached root of '{namespace}' could not be read: {e}");
            return SetupMarker::Refused("the cached root could not be read");
        }
    };
    let finished =
        |root: PathBuf, written: bool, settled: Result<Option<PathBuf>, String>| match settled {
            Err(reason) => {
                tracing::warn!(
                    "the cached root of '{namespace}' could not be set to {}: {reason}",
                    root.display()
                );
                SetupMarker::CacheNotSet {
                    root,
                    written,
                    reason,
                }
            }
            Ok(Some(cached)) => SetupMarker::KeptCache {
                root,
                cached,
                written,
            },
            Ok(None) if written => SetupMarker::Written(root),
            Ok(None) => SetupMarker::Reused(root),
        };
    match find_root_marker(&dir) {
        Err(_) => {
            return SetupMarker::Refused(
                "the .NeuroStrata/config.json that applies here is unreadable; run `neurostrata-mcp root show`",
            )
        }
        Ok(Some(found)) if !found.config.namespace.eq_ignore_ascii_case(&namespace) => {
            return SetupMarker::OtherNamespace(found.config.namespace)
        }
        Ok(Some(found)) => {
            return match setup_loss_check(registry, &namespace, cached.as_deref(), &found.root).await {
                Ok(ids) if !ids.is_empty() => SetupMarker::RootChangeBlocked(ids),
                Ok(_) => {
                    let kept = settle_cache(registry, &namespace, cached.as_deref(), &found.root).await;
                    finished(found.root, false, kept)
                }
                Err(e) => {
                    tracing::warn!("the memories of '{namespace}' could not be listed: {e}");
                    SetupMarker::Refused("the memories could not be listed for the root-change check")
                }
            };
        }
        Ok(None) => {}
    }
    let target = match crate::paths::main_checkout_root(&dir.to_string_lossy()) {
        Some(top) => {
            if let Some(cache) = &cached {
                let cache = in_main_checkout(cache);
                if !same_folder(&cache, &top) && cache.starts_with(&top) {
                    return SetupMarker::AskRoot { top, sub: cache };
                }
            }
            top
        }
        None => dir.clone(),
    };
    if let Some(reason) = refuse_root_target(&target) {
        return SetupMarker::Refused(reason);
    }
    // A marker for this namespace in a subfolder is still a marker for this
    // namespace. Writing the checkout top's marker would split the namespace
    // across two roots; send the caller to `root set --replace` instead.
    let elsewhere = other_markers_for(&target, &namespace);
    if elsewhere.first().is_some() {
        return SetupMarker::Refused(
            "another marker for this namespace exists in a subfolder; run `neurostrata-mcp root set <dir> --namespace <ns> --replace` to move it here",
        );
    }
    match setup_loss_check(registry, &namespace, cached.as_deref(), &target).await {
        Ok(ids) if !ids.is_empty() => return SetupMarker::RootChangeBlocked(ids),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("the memories of '{namespace}' could not be listed: {e}");
            return SetupMarker::Refused(
                "the memories could not be listed for the root-change check",
            );
        }
    }
    match write_new_marker(&target, &RootConfig::new(namespace.clone())) {
        Ok(WriteOutcome::Written) => {
            let kept = settle_cache(registry, &namespace, cached.as_deref(), &target).await;
            finished(target, true, kept)
        }
        Ok(WriteOutcome::LostRace(MarkerRead::Valid(won))) => {
            if won.namespace.eq_ignore_ascii_case(&namespace) {
                let kept = settle_cache(registry, &namespace, cached.as_deref(), &target).await;
                finished(target, false, kept)
            } else {
                SetupMarker::OtherNamespace(won.namespace)
            }
        }
        Ok(WriteOutcome::LostRace(_)) => SetupMarker::Refused(
            "another writer left an unreadable .NeuroStrata/config.json; run `neurostrata-mcp root show`",
        ),
        Err(MarkerError::Unsafe(reason)) => SetupMarker::Refused(reason),
        Err(MarkerError::Io(e)) => {
            tracing::warn!("writing {} failed: {e}", target.join(ROOT_CONFIG_REL).display());
            SetupMarker::Refused("the marker file could not be written")
        }
    }
}

/// `root set`'s arguments.
#[derive(Debug, Clone)]
pub struct RootSetArgs {
    pub dir: PathBuf,
    pub namespace: String,
    pub source_id: Option<String>,
    pub replace: bool,
    pub force: bool,
}

/// The steps of `root set --replace`, for the interruption test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootSetStep {
    Published,
    RegistryUpdated,
}

/// `neurostrata-mcp root set` (spec 6.8) against a store, for tests. See
/// [`root_set_with`].
#[cfg(test)]
pub async fn root_set(
    store: &std::sync::Arc<dyn crate::traits::VectorStore>,
    emb: Option<&std::sync::Arc<dyn crate::traits::Embedder>>,
    args: &RootSetArgs,
) -> Result<Vec<String>, String> {
    let registry = StoreRegistry {
        store: store.clone(),
        emb: emb.cloned(),
        checkpoint: false,
    };
    root_set_with(&registry, args, None).await
}

/// Same-namespace markers in `target`'s checkout (or, outside git, the first
/// marker above it and those below it), other than `target`'s own. Returns
/// each folder with the source id its marker carried, so a caller that moves
/// the root can re-read and refuse to delete a marker another writer changed.
///
/// The walk is uncapped here: a duplicate at depth 9 is still a duplicate,
/// and missing one would write a second marker for the same namespace.
fn other_markers_for(target: &Path, namespace: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut found: Vec<(PathBuf, MarkerRead)> = Vec::new();
    match crate::paths::main_checkout_root(&target.to_string_lossy()) {
        Some(top) => {
            found.extend(marker_at(&top));
            found.extend(nested_markers_uncapped(&top));
        }
        None => {
            found.extend(target.ancestors().skip(1).find_map(marker_at));
            found.extend(nested_markers_uncapped(target));
        }
    }
    found
        .into_iter()
        .filter_map(|(dir, read)| match read {
            MarkerRead::Valid(cfg)
                if cfg.namespace.eq_ignore_ascii_case(namespace) && !same_folder(&dir, target) =>
            {
                Some((dir, cfg.source_id))
            }
            _ => None,
        })
        .collect()
}

/// `neurostrata-mcp root set`: every file change happens in this process;
/// `registry` only reads the cache and rows and sets the cached root. Returns
/// the lines to print, or the reason it refused. `stop_after` simulates an
/// interruption (tests).
pub(crate) async fn root_set_with(
    registry: &dyn RootRegistry,
    args: &RootSetArgs,
    stop_after: Option<RootSetStep>,
) -> Result<Vec<String>, String> {
    let ns = args.namespace.as_str();
    if !args.dir.is_absolute() || !args.dir.is_dir() {
        return Err(format!(
            "{} is not an existing absolute folder",
            args.dir.display()
        ));
    }
    if !is_valid_namespace(ns) {
        return Err(format!(
            "'{ns}' is not a valid namespace (letters, digits, '_', '.', ':' and '-', starting with a letter or digit)"
        ));
    }
    let dir = dunce::canonicalize(&args.dir).unwrap_or_else(|_| args.dir.clone());
    // A root inside a linked worktree is marked in the main checkout.
    let target = in_main_checkout(&dir);
    if is_temp_root(&target) {
        return Err(format!(
            "refused: {} is the system temp folder, which cannot be a project root",
            target.display()
        ));
    }
    if let Some(reason) = refuse_root_target(&target) {
        return Err(format!(
            "refused: {} is a {reason}, which cannot be a project root",
            target.display()
        ));
    }
    let existing = read_marker(&target);
    match &existing {
        MarkerRead::Valid(cfg) if !cfg.namespace.eq_ignore_ascii_case(ns) && !args.replace => {
            return Err(format!(
                "{} holds the marker of namespace {}; pass --replace to change it",
                target.display(),
                cfg.namespace
            ))
        }
        MarkerRead::Unreadable { path, reason } if !args.replace => {
            return Err(format!(
                "{} is unreadable ({reason}); pass --replace to overwrite it",
                path.display()
            ))
        }
        _ => {}
    }
    let old = other_markers_for(&target, ns);
    if !old.is_empty() && !args.replace {
        let listed: Vec<String> = old.iter().map(|(p, _)| p.display().to_string()).collect();
        return Err(format!(
            "another marker for {ns} exists at {}; pass --replace to move it to {}",
            listed.join(", "),
            target.display()
        ));
    }

    let cached = usable_cached_root(registry, ns).await?;
    let current = cached
        .clone()
        .or_else(|| old.first().map(|(p, _)| p.clone()));
    let lost = lost_by_change(registry, ns, current.as_deref(), &target).await?;
    let mut lines = Vec::new();
    if !lost.is_empty() {
        let from = current
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if !args.force {
            return Err(format!(
                "changing the root of {ns} from {from} to {} would stop {} memories matching: {}. \
                 Run `neurostrata-mcp root audit --for-root {}` and correct them first ({ROOT_GUIDE}), or pass --force.",
                target.display(),
                lost.len(),
                lost.join(", "),
                target.display()
            ));
        }
        lines.push(format!(
            "--force: {} memories keyed to {from} will stop matching: {}",
            lost.len(),
            lost.join(", ")
        ));
    }

    // 1. Publish the new marker.
    let mut cfg = match existing {
        MarkerRead::Valid(cfg) if cfg.namespace.eq_ignore_ascii_case(ns) => Some(cfg),
        _ => None,
    };
    // ROOT-6: `existing` was read before the registry/loss checks and may be
    // stale. Re-read before rewriting a source id: a competing writer's
    // marker must never be overwritten without an explicit --replace.
    if cfg.is_some() {
        match read_marker(&target) {
            MarkerRead::Valid(current) if current.namespace.eq_ignore_ascii_case(ns) => {
                if current.source_id != cfg.as_ref().and_then(|c| c.source_id.clone())
                    && !args.replace
                {
                    return Err(format!(
                        "{} changed while root set was running (source id now {:?}); re-run with --replace to overwrite it",
                        target.join(ROOT_CONFIG_REL).display(),
                        current.source_id
                    ));
                }
                cfg = Some(current);
            }
            MarkerRead::Valid(current) if args.replace => {
                // A different namespace now owns the file: fall through to a
                // fresh marker rather than mutating the old config.
                let _ = current;
                cfg = None;
            }
            MarkerRead::Absent => cfg = None,
            other => {
                return Err(format!(
                    "{} could not be re-read while root set was running ({other:?}); run `root show` and retry",
                    target.join(ROOT_CONFIG_REL).display()
                ));
            }
        }
    }
    let wrote = match cfg.as_mut() {
        Some(cfg) => {
            let changed = args.source_id.is_some() && cfg.source_id != args.source_id;
            if changed {
                cfg.source_id = args.source_id.clone();
                replace_marker(&target, cfg).map_err(|e| e.to_string())?;
            }
            changed
        }
        None => {
            let mut fresh = RootConfig::new(ns);
            fresh.source_id = args.source_id.clone();
            if matches!(read_marker(&target), MarkerRead::Absent) {
                match write_new_marker(&target, &fresh).map_err(|e| e.to_string())? {
                    WriteOutcome::Written => {}
                    WriteOutcome::LostRace(MarkerRead::Valid(won))
                        if won.namespace.eq_ignore_ascii_case(ns) => {}
                    WriteOutcome::LostRace(_) => {
                        return Err(format!(
                            "another writer published {} first; run `neurostrata-mcp root show`",
                            target.join(ROOT_CONFIG_REL).display()
                        ))
                    }
                }
            } else {
                replace_marker(&target, &fresh).map_err(|e| e.to_string())?;
            }
            true
        }
    };
    let file = target.join(ROOT_CONFIG_REL);
    lines.push(if wrote {
        format!("Wrote {} (namespace {ns}).", file.display())
    } else {
        format!("Kept {} (namespace {ns}).", file.display())
    });
    if stop_after == Some(RootSetStep::Published) {
        return Ok(lines);
    }

    // 2. Point the registry at it.
    if let Err(reason) = registry.set_root(ns, &target).await {
        // The marker is written but the cache still points elsewhere: a
        // failure, so scripts and the exit code see it.
        return Err(format!(
            "{}\nThe cached root of {ns} could not be set ({reason}); rerun this command to finish.",
            lines.join("\n")
        ));
    }
    lines.push(format!("Cached root of {ns} set to {}.", target.display()));
    if stop_after == Some(RootSetStep::RegistryUpdated) {
        return Ok(lines);
    }

    // 3. Remove the old markers. A concurrent `root set` may have made one
    // of them the cached root since: that marker stays, so the cache never
    // points at a folder without one.
    for (old_dir, old_source) in &old {
        let old_file = old_dir.join(ROOT_CONFIG_REL);
        // Async checks stay outside the critical section: a std MutexGuard is
        // !Send and must not be held across `.await`.
        if let Ok(Some(now)) = usable_cached_root(registry, ns).await {
            if same_root(&now, old_dir) {
                lines.push(format!(
                    "Kept the old marker {}: the cached root of {ns} is that folder again (another `root set` changed it).",
                    old_file.display()
                ));
                continue;
            }
        }
        // Re-read, then unlink under the same lock marker writers take, so a
        // concurrent `root set` cannot replace this marker between the check
        // and the removal and have its successor deleted (Codex review
        // 4670091232).
        let _guard = MARKER_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match read_marker(old_dir) {
            MarkerRead::Valid(current)
                if current.namespace.eq_ignore_ascii_case(ns)
                    && current.source_id.as_deref() == old_source.as_deref() => {}
            MarkerRead::Absent => continue,
            other => {
                lines.push(format!(
                    "Kept the old marker {}: it changed to {other:?} since the scan (another writer used it).",
                    old_file.display()
                ));
                continue;
            }
        }
        match std::fs::remove_file(&old_file) {
            Ok(()) => lines.push(format!("Removed the old marker {}.", old_file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "could not remove the old marker {}: {e}",
                    old_file.display()
                ))
            }
        }
    }

    if let Some(source_id) = &args.source_id {
        match crate::config::bind_project_context_if_free(source_id, &target.join(".NeuroStrata")) {
            Ok(crate::config::BindOutcome::Bound) => lines.push(format!(
                "Bound project context {source_id} to {}.",
                target.display()
            )),
            Ok(crate::config::BindOutcome::AlreadyBoundHere) => {}
            Ok(crate::config::BindOutcome::LiveElsewhere(path)) => lines.push(format!(
                "Project context {source_id} stays bound to {}; recorded in the marker only.",
                path.display()
            )),
            Err(e) => lines.push(format!("Project context {source_id} was not bound: {e}")),
        }
    }
    let top = crate::paths::main_checkout_root(&target.to_string_lossy());
    if top.as_deref().is_some_and(|top| !same_folder(top, &target)) {
        lines
            .push("Commit .NeuroStrata/config.json so other clones use the same root.".to_string());
    }
    Ok(lines)
}

/// For `hooks install` (the CLI may run git; the daemon does not): one line
/// when the marker at `root` is neither tracked nor ignored by git.
pub fn commit_advice(root: &Path) -> Option<String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()
            .map(|s| s.success())
    };
    if git(&["ls-files", "--error-unmatch", ROOT_CONFIG_REL])?
        || git(&["check-ignore", "-q", ROOT_CONFIG_REL])?
    {
        return None;
    }
    Some(
        "Commit .NeuroStrata/config.json so other clones use the same root, or add it to .gitignore to keep it per clone."
            .to_string(),
    )
}

/// `yes`/`no` by `git ls-files --error-unmatch`, run in the checkout that
/// holds the file: exit 1 is "no", any other failure (a checkout git
/// refuses, say) is "unknown".
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
        Ok(status) if status.code() == Some(1) => "no".to_string(),
        Ok(status) => match status.code() {
            Some(code) => format!("unknown (git exit {code})"),
            None => "unknown (git was stopped by a signal)".to_string(),
        },
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
    fn read_marker_refuses_a_marker_outside_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("config.json");
        std::fs::write(&secret, br#"{"version":1,"namespace":"example-project"}"#).unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let marker = tmp.path().join(ROOT_CONFIG_REL);
        // Windows without symlink privilege cannot build this fixture; skip
        // the symlink-specific assertion instead of panicking (Codex review
        // 4670091413). `link_file` returns false when creation is denied.
        if link_file(&secret, &marker) {
            match read_marker(tmp.path()) {
                MarkerRead::Unreadable { path, reason } => {
                    assert_eq!(path, marker);
                    assert!(reason.contains("outside"), "{reason}");
                }
                other => panic!("expected an unreadable marker, got {other:?}"),
            }
        }
        // A folder that does not exist still reads as absent.
        assert_eq!(read_marker(&tmp.path().join("missing")), MarkerRead::Absent);
    }

    #[test]
    fn read_marker_accepts_a_canonical_path_with_parent_steps() {
        let tmp = tempfile::tempdir().unwrap();
        write_raw(
            tmp.path(),
            br#"{"version":1,"namespace":"example-project"}"#,
        );
        let nested = tmp.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            read_marker(&nested.join("..")),
            MarkerRead::Valid(RootConfig::new("example-project"))
        );
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
            serde_json::from_slice(&std::fs::read(tmp.path().join(ROOT_CONFIG_REL)).unwrap())
                .unwrap();
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
        assert_eq!(
            read_marker(tmp.path()),
            MarkerRead::Valid(RootConfig::new("example-project"))
        );
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
                MarkerRead::Unreadable { path, .. } => {
                    assert_eq!(path, tmp.path().join(ROOT_CONFIG_REL))
                }
                other => panic!("{:?} read as {other:?}", String::from_utf8_lossy(body)),
            }
        }
        // A writer refuses a config it could not read back.
        let mut cfg = RootConfig::new("example-project");
        cfg.version = 2;
        assert!(matches!(
            replace_marker(tmp.path(), &cfg),
            Err(MarkerError::Io(_))
        ));
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
        assert!(matches!(
            read_marker(tmp.path()),
            MarkerRead::Unreadable { .. }
        ));
    }

    #[test]
    fn second_writer_loses_race() {
        let tmp = tempfile::tempdir().unwrap();
        let first = RootConfig::new("example-project");
        let second = RootConfig::new("other-project");
        assert_eq!(
            write_new_marker(tmp.path(), &first).unwrap(),
            WriteOutcome::Written
        );
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
        assert!(
            bad.is_empty(),
            "{} of {reads} reads failed, first: {:?}",
            bad.len(),
            bad[0]
        );
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

    /// Links `link` to the file `target`; false when this session may not
    /// create file symlinks (Windows without the privilege).
    fn link_file(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
    }

    #[test]
    fn file_symlink_into_another_checkout_does_not_take_its_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let outer = base.join("outer");
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        std::fs::create_dir_all(outer.join("src")).unwrap();
        std::fs::write(outer.join("src").join("a.rs"), "").unwrap();
        write_raw(&outer, br#"{"version":1,"namespace":"outer"}"#);
        let inner = base.join("inner");
        std::fs::create_dir_all(inner.join(".git")).unwrap();
        let link = inner.join("link.rs");
        if !link_file(&outer.join("src").join("a.rs"), &link) {
            eprintln!("skipped: file symlinks are not available here");
            return;
        }
        assert_eq!(find_root_marker(&link), Ok(None));
        // The target itself still finds its own marker.
        let found = find_root_marker(&outer.join("src").join("a.rs"))
            .unwrap()
            .unwrap();
        assert_eq!(found.root, outer);
        // A marker in the link's own checkout decides for the link.
        write_raw(&inner, br#"{"version":1,"namespace":"inner"}"#);
        let found = find_root_marker(&link).unwrap().unwrap();
        assert_eq!(found.root, inner);
        assert_eq!(found.config.namespace, "inner");
    }

    #[test]
    fn symlinked_neurostrata_is_refused() {
        // A missing folder is created as a real directory.
        let fresh = tempfile::tempdir().unwrap();
        assert!(marker_dir_is_safe(fresh.path()));
        assert!(std::fs::symlink_metadata(fresh.path().join(".NeuroStrata"))
            .unwrap()
            .is_dir());

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
        assert!(
            link_dir(elsewhere.path(), &linked.path().join(".NeuroStrata")),
            "could not create a link"
        );
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
        assert_eq!(
            refuse_root_target_with(home.path(), Some(home.path())),
            Some("home folder")
        );
        std::fs::create_dir(home.path().join(".git")).unwrap();
        assert_eq!(
            refuse_root_target_with(home.path(), Some(home.path())),
            Some("home folder")
        );

        let project = home.path().join("example-project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(&project, Some(home.path())), None);

        let fs_root = dunce::canonicalize(home.path())
            .unwrap()
            .ancestors()
            .last()
            .unwrap()
            .to_path_buf();
        assert_eq!(
            refuse_root_target_with(&fs_root, None),
            Some("filesystem root")
        );
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
        assert_eq!(
            refuse_root_target_with(ws, None),
            Some("folder contains a git checkout")
        );
        assert_eq!(
            refuse_root_target_with(&ws.join("a"), None),
            Some("folder contains a git checkout")
        );
        // The checkout and folders inside it may be roots.
        assert_eq!(refuse_root_target_with(&repo, None), None);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        assert_eq!(refuse_root_target_with(&repo.join("sub"), None), None);

        // Beyond the depth bound, or under a skipped folder, is not looked at.
        let deep = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(deep.path().join("1/2/3/4/5/.git")).unwrap();
        assert_eq!(refuse_root_target_with(deep.path(), None), None);
        std::fs::create_dir_all(deep.path().join("1/2/3/4/.git")).unwrap();
        assert_eq!(
            refuse_root_target_with(deep.path(), None),
            Some("folder contains a git checkout")
        );
        let skipped = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(skipped.path().join("node_modules/pkg/.git")).unwrap();
        std::fs::create_dir_all(skipped.path().join("target/x/.git")).unwrap();
        assert_eq!(refuse_root_target_with(skipped.path(), None), None);
    }

    const CASES: &str = include_str!("../tests/fixtures/root-marker-cases.json");

    /// `rel` (with `/`) under `base`, in this host's separators: `mklink`
    /// does not accept `/`.
    fn under(base: &Path, rel: &str) -> PathBuf {
        rel.split('/')
            .fold(base.to_path_buf(), |path, part| path.join(part))
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
                write_raw(
                    &path,
                    format!(r#"{{"version":1,"namespace":"{ns}"}}"#).as_bytes(),
                );
            } else if let Some(text) = kind.strip_prefix("marker-text:") {
                write_raw(&path, text.as_bytes());
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
                    "git-dir" => {
                        std::fs::create_dir_all(path.join(".git")).unwrap();
                        std::fs::write(path.join(".git").join("HEAD"), "ref: refs/heads/main\n")
                            .unwrap();
                    }
                    "neurostrata-dir" => {
                        std::fs::create_dir_all(path.join(".NeuroStrata")).unwrap()
                    }
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
            let got = find_root_marker(&under(tmp.path(), case["query"].as_str().unwrap()));
            if let Some(folder) = expect["unreadable"].as_str() {
                let (path, _) = got.expect_err(name);
                let want = under(&base, folder)
                    .join(".NeuroStrata")
                    .join("config.json");
                assert_eq!(
                    dunce::canonicalize(&path).unwrap_or(path),
                    dunce::canonicalize(&want).unwrap_or(want),
                    "{name}"
                );
                continue;
            }
            let got = got
                .unwrap_or_else(|(path, reason)| panic!("{name}: unreadable {path:?}: {reason}"));
            match expect["root"].as_str() {
                None => assert_eq!(got, None, "{name}"),
                Some(root) => {
                    let got = got.unwrap_or_else(|| panic!("{name}: no marker found"));
                    assert_eq!(got.root, under(&base, root), "{name}");
                    assert_eq!(
                        got.config.namespace,
                        expect["namespace"].as_str().unwrap(),
                        "{name}"
                    );
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
        let found = find_root_marker(&base.join("wt/packages/api"))
            .unwrap()
            .unwrap();
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
        assert!(
            made.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
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
        write_raw(
            &repo,
            br#"{"version":1,"namespace":"example-project","source_id":"example-src"}"#,
        );
        write_raw(
            &repo.join("packages/api"),
            br#"{"version":1,"namespace":"example-api"}"#,
        );
        write_raw(&repo.join("packages/bad"), b"nope");
        // A nested clone's marker is another checkout's, not listed.
        std::fs::create_dir_all(repo.join("clone/.git")).unwrap();
        write_raw(
            &repo.join("clone"),
            br#"{"version":1,"namespace":"example-clone"}"#,
        );

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
            &Ok(bindings.clone()),
        );
        assert!(
            text.contains(&format!("marker root: {}", repo.display())),
            "{text}"
        );
        assert!(text.contains("found via: own_checkout"), "{text}");
        assert!(text.contains("source_id: example-src"), "{text}");
        assert!(text.contains("(missing)"), "{text}");
        assert!(text.contains("hint: the marker root differs"), "{text}");
        assert!(
            text.contains(&format!("nested markers below {} (", repo.display())),
            "{text}"
        );
        assert!(text.contains("(namespace example-api)"), "{text}");
        assert!(text.contains("(unreadable:"), "{text}");
        assert!(!text.contains("example-clone"), "{text}");
        assert!(
            text.contains(&format!(
                "  gone -> {}",
                base.join("gone/.NeuroStrata").display()
            )),
            "{text}"
        );
        assert!(!text.contains("  here ->"), "{text}");

        let empty = tempfile::tempdir().unwrap();
        let text = format_root_show(
            empty.path(),
            &Ok(None),
            "example-project",
            &CachedRoot::Unknown("no daemon, store locked".into()),
            &Ok(std::collections::HashMap::new()),
        );
        assert!(
            text.contains("cached root: unknown (no daemon, store locked)"),
            "{text}"
        );
        assert!(
            text.contains("other checkouts not entered): none"),
            "{text}"
        );
        assert!(text.contains("missing folders: none"), "{text}");
    }

    fn layout(base: &Path, json: &str) {
        build_layout(
            base,
            &serde_json::from_str::<Vec<serde_json::Value>>(json).unwrap(),
        );
    }

    fn show(dir: &Path, cached: &Path) -> String {
        format_root_show(
            dir,
            &find_root_marker(dir),
            "example-project",
            &CachedRoot::Known(Some(cached.to_string_lossy().into_owned())),
            &Ok(std::collections::HashMap::new()),
        )
    }

    #[test]
    fn worktree_cached_main_top_is_not_reported_as_differing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:example-project",
                "repo/.git/worktrees/wt commondir:../..", "repo/.git/worktrees/own commondir:../..",
                "repo/.claude/worktrees/wt git-file:../../../.git/worktrees/wt",
                "own git-file:../repo/.git/worktrees/own", "own marker:example-project"]"#,
        );
        let repo = base.join("repo");
        // Marker found through the main checkout, and a worktree with its own
        // top marker: both are cached as the main checkout's top.
        let via_main = under(&base, "repo/.claude/worktrees/wt");
        let own = base.join("own");
        assert_eq!(
            find_root_marker(&via_main).unwrap().unwrap().via,
            FoundVia::MainCheckout
        );
        assert_eq!(
            find_root_marker(&own).unwrap().unwrap().via,
            FoundVia::OwnCheckout
        );
        for dir in [&via_main, &own] {
            let text = show(dir, &repo);
            assert!(!text.contains("hint:"), "{text}");
            let text = show(dir, &base.join("elsewhere"));
            assert!(text.contains("hint: the marker root differs"), "{text}");
            assert!(
                text.contains("root set") && text.contains("hooks install"),
                "{text}"
            );
        }
        // Outside a worktree the cached root is compared as it is.
        assert!(!show(&repo, &repo).contains("hint:"));
    }

    #[test]
    fn non_absolute_input_finds_no_marker() {
        assert_eq!(start_folder(Path::new("src")), None);
        assert_eq!(find_root_marker(Path::new("src/a.rs")), Ok(None));
        assert_eq!(find_root_marker(Path::new(".")), Ok(None));
        // Another machine's POSIX path is not absolute on Windows.
        #[cfg(windows)]
        assert_eq!(find_root_marker(Path::new("/home/u/repo/a.rs")), Ok(None));
    }

    #[test]
    fn missing_path_uses_nearest_existing_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:example-project", "repo/sub marker:example-sub"]"#,
        );
        let found = find_root_marker(&under(&base, "repo/gone/deeper/a.rs"))
            .unwrap()
            .unwrap();
        assert_eq!(found.root, base.join("repo"));
        let found = find_root_marker(&under(&base, "repo/sub/new/b.rs"))
            .unwrap()
            .unwrap();
        assert_eq!(found.root, under(&base, "repo/sub"));
        assert_eq!(found.config.namespace, "example-sub");
        // The root comes back in canonical spelling, whatever the input's.
        #[cfg(windows)]
        {
            let raw = under(&base, "repo/gone/x").to_string_lossy().into_owned();
            let lower = format!(
                "{}{}",
                raw[..1].to_ascii_lowercase(),
                raw[1..].replace('\\', "/")
            );
            let found = find_root_marker(Path::new(&lower)).unwrap().unwrap();
            assert_eq!(
                found.root.to_string_lossy(),
                base.join("repo").to_string_lossy()
            );
        }
    }

    #[test]
    fn nested_markers_listed_below_marker_root() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:example-project", "repo/a/b",
                "repo/c/d marker:example-d"]"#,
        );
        let dir = under(&base, "repo/a/b");
        let text = show(&dir, &base.join("repo"));
        assert!(
            text.contains(&format!(
                "nested markers below {} (",
                base.join("repo").display()
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "  {} (namespace example-d)",
                under(&base, "repo/c/d").display()
            )),
            "{text}"
        );
        // With no marker, the walk is below the given folder.
        let plain = tempfile::tempdir().unwrap();
        let plain = dunce::canonicalize(plain.path()).unwrap();
        std::fs::create_dir_all(plain.join("x")).unwrap();
        let text = format_root_show(
            &plain.join("x"),
            &Ok(None),
            "example-project",
            &CachedRoot::Known(None),
            &Ok(std::collections::HashMap::new()),
        );
        assert!(
            text.contains(&format!(
                "nested markers below {} (",
                plain.join("x").display()
            )),
            "{text}"
        );
    }

    #[test]
    fn null_source_id_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        write_raw(
            tmp.path(),
            br#"{"version":1,"namespace":"example-project","source_id":null}"#,
        );
        assert!(matches!(
            read_marker(tmp.path()),
            MarkerRead::Unreadable { .. }
        ));
        write_raw(
            tmp.path(),
            br#"{"version":1,"namespace":"example-project","source_id":7}"#,
        );
        assert!(matches!(
            read_marker(tmp.path()),
            MarkerRead::Unreadable { .. }
        ));
        // Left out is still fine.
        write_raw(
            tmp.path(),
            br#"{"version":1,"namespace":"example-project"}"#,
        );
        assert!(matches!(read_marker(tmp.path()), MarkerRead::Valid(_)));
    }

    #[test]
    fn main_checkout_walk_stops_at_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:example-project",
                "repo/.git/worktrees/wt commondir:../..", "wt/pkg/x",
                "wt git-file:../repo/.git/worktrees/wt",
                "other git-dir", "other/sub marker:example-other", "other/sub/x"]"#,
        );
        // The main checkout's `pkg` is a link into another repository.
        assert!(
            link_dir(&under(&base, "other/sub"), &under(&base, "repo/pkg")),
            "could not create a link"
        );
        let found = find_root_marker(&under(&base, "wt/pkg/x"))
            .unwrap()
            .unwrap();
        assert_eq!(found.config.namespace, "example-project");
        assert_eq!(found.root, base.join("wt"));
        assert_eq!(found.via, FoundVia::MainCheckout);
    }

    #[test]
    fn uninspectable_neurostrata_is_unreadable_not_absent() {
        let tmp = tempfile::tempdir().unwrap();
        // A name the OS refuses: inspecting `.NeuroStrata` under it fails
        // with an error other than "not found".
        let dir = tmp.path().join("bad\0name");
        match marker_at(&dir) {
            Some((at, MarkerRead::Unreadable { path, reason })) => {
                assert_eq!(at, dir);
                assert_eq!(path, dir.join(".NeuroStrata"));
                assert!(!reason.is_empty());
            }
            other => panic!("expected an unreadable marker, got {other:?}"),
        }
        // A missing folder, or a missing path below a file, is absent.
        assert_eq!(marker_at(&tmp.path().join("missing")), None);
        std::fs::write(tmp.path().join("file"), b"").unwrap();
        assert_eq!(marker_at(&tmp.path().join("file").join("x")), None);
    }

    #[test]
    fn git_failure_is_unknown_not_untracked() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        // A `.git` file pointing nowhere: git refuses the checkout (exit 128).
        let broken = base.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(".git"), "gitdir: does-not-exist\n").unwrap();
        write_raw(&broken, br#"{"version":1,"namespace":"example-project"}"#);
        let answer = git_tracks_marker(&broken, FoundVia::OwnCheckout);
        assert!(answer.starts_with("unknown (git exit "), "{answer}");

        // A real checkout with the marker untracked: exit 1, "no".
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status();
        if !init.is_ok_and(|s| s.success()) {
            eprintln!("skipped the untracked half: git init failed");
            return;
        }
        write_raw(&repo, br#"{"version":1,"namespace":"example-project"}"#);
        assert_eq!(git_tracks_marker(&repo, FoundVia::OwnCheckout), "no");
    }

    #[test]
    fn root_show_in_worktree_lists_main_only_split_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:example-project",
                "repo/packages/api marker:example-api", "repo/packages/web marker:example-web",
                "repo/.git/worktrees/wt commondir:../..", "wt/packages/web marker:example-web",
                "wt git-file:../repo/.git/worktrees/wt"]"#,
        );
        let wt = base.join("wt");
        let text = show(&wt, &base.join("repo"));
        assert!(text.contains("found via: main_checkout"), "{text}");
        assert!(text.contains(": 2\n"), "{text}");
        assert!(
            text.contains(&format!(
                "  {} (namespace example-api, only in the main checkout)",
                under(&base, "repo/packages/api").display()
            )),
            "{text}"
        );
        // The worktree's own copy is listed once, without the label.
        assert!(
            text.contains(&format!(
                "  {} (namespace example-web)",
                under(&base, "wt/packages/web").display()
            )),
            "{text}"
        );
        assert!(
            !text.contains(&under(&base, "repo/packages/web").display().to_string()),
            "{text}"
        );
        // The marker it lists is the one find_root_marker answers there.
        let found = find_root_marker(&under(&base, "wt/packages/api"))
            .unwrap()
            .unwrap();
        assert_eq!(found.config.namespace, "example-api");
    }

    #[test]
    fn root_show_reports_unreadable_config_and_miscased_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".neurostrata")).unwrap();
        std::fs::write(
            repo.join(".neurostrata/config.json"),
            br#"{"version":1,"namespace":"example-project"}"#,
        )
        .unwrap();
        let text = format_root_show(
            &repo,
            &find_root_marker(&repo),
            "example-project",
            &CachedRoot::Known(None),
            &Err("config.json: expected value at line 1 column 1".to_string()),
        );
        assert!(text.contains("marker root: none"), "{text}");
        assert!(
            text.contains(&format!(
                "note: {} is not spelled .NeuroStrata, so it is not a marker",
                repo.join(".neurostrata").display()
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "project-context bindings to missing folders: unknown (config could not be read: config.json: expected value"
            ),
            "{text}"
        );
    }

    #[test]
    fn same_folder_compares_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(same_folder(&repo, &repo));
        assert!(!same_folder(&base, &repo));
        assert!(!same_folder(&repo.join("gone"), &repo));
        // A link to the folder is the same folder.
        let alias = base.join("alias");
        assert!(link_dir(&repo, &alias), "could not create a link");
        assert!(same_folder(&alias, &repo));
        // Another spelling of the same path.
        #[cfg(windows)]
        {
            let raw = repo.to_string_lossy().into_owned();
            let other = format!(
                "{}{}",
                raw[..1].to_ascii_lowercase(),
                raw[1..].replace('\\', "/")
            );
            assert!(same_folder(Path::new(&other), &repo));
        }
    }

    // ── writers ──────────────────────────────────────────────────────────

    use crate::guard::testkit::{MockStore, StubEmbedder};
    use crate::traits::{Embedder, MemoryPayload, VectorStore};

    const NS: &str = "example-project";

    fn mock() -> (Arc<dyn VectorStore>, Arc<dyn Embedder>) {
        (Arc::new(MockStore::new()), Arc::new(StubEmbedder))
    }

    async fn cache_root(store: &Arc<dyn VectorStore>, emb: &Arc<dyn Embedder>, root: &Path) {
        crate::registry::cache_project_root(store, Some(emb), NS, &root.to_string_lossy()).await;
    }

    async fn cached(store: &Arc<dyn VectorStore>) -> Option<String> {
        crate::registry::cached_project_root(store, NS)
            .await
            .unwrap()
    }

    async fn rule_at(
        store: &Arc<dyn VectorStore>,
        id: &str,
        location: &str,
        metadata: serde_json::Value,
    ) {
        store
            .upsert(
                NS,
                id,
                vec![0.0; 4],
                MemoryPayload {
                    content: format!("rule {id}"),
                    user_id: "agent".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: location.to_string(),
                    location_lines: String::new(),
                    metadata,
                },
            )
            .await
            .unwrap();
    }

    /// A temp base folder, or `None` when the temp folder sits inside a git
    /// checkout on this host (a non-git case cannot be built there).
    fn base_outside_git(tmp: &tempfile::TempDir) -> Option<PathBuf> {
        let base = dunce::canonicalize(tmp.path()).unwrap();
        (!base.ancestors().any(|a| a.join(".git").exists())).then_some(base)
    }

    fn has_marker(dir: &Path) -> bool {
        dir.join(ROOT_CONFIG_REL).exists()
    }

    const WORKTREE: &str = r#"["repo git-dir", "repo/src", "repo/.git/worktrees/wt commondir:../..",
        "wt git-file:../repo/.git/worktrees/wt", "wt/src"]"#;

    #[tokio::test]
    async fn writes_at_main_checkout_top_when_cache_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, WORKTREE);
        let repo = base.join("repo");
        let (store, emb) = mock();
        let got = ensure_marker_for_setup(&store, Some(&emb), &repo.join("src"), Some(NS)).await;
        assert_eq!(got, SetupMarker::Written(repo.clone()));
        assert_eq!(read_marker(&repo), MarkerRead::Valid(RootConfig::new(NS)));
        assert_eq!(
            cached(&store).await,
            Some(repo.to_string_lossy().into_owned())
        );
        assert!(got.message(NS, &repo).unwrap().starts_with("Wrote "));

        // From the linked worktree: the marker goes to the main checkout's top.
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, WORKTREE);
        let (store, emb) = mock();
        let got =
            ensure_marker_for_setup(&store, Some(&emb), &under(&base, "wt/src"), Some(NS)).await;
        assert_eq!(got, SetupMarker::Written(base.join("repo")));
        assert!(!has_marker(&base.join("wt")));
        // A second setup reuses it.
        let again = ensure_marker_for_setup(&store, Some(&emb), &base.join("wt"), Some(NS)).await;
        assert_eq!(again.kind(), "reused");
        assert_eq!(again.message(NS, &base), None);
    }

    #[tokio::test]
    async fn writes_when_cache_is_top_or_its_worktree() {
        for cache in ["repo", "wt"] {
            let tmp = tempfile::tempdir().unwrap();
            let base = dunce::canonicalize(tmp.path()).unwrap();
            layout(&base, WORKTREE);
            let (store, emb) = mock();
            cache_root(&store, &emb, &base.join(cache)).await;
            let got =
                ensure_marker_for_setup(&store, Some(&emb), &base.join("repo"), Some(NS)).await;
            assert_eq!(
                got,
                SetupMarker::Written(base.join("repo")),
                "cache {cache}"
            );
            assert!(has_marker(&base.join("repo")));
        }
    }

    #[tokio::test]
    async fn asks_when_cache_is_subfolder() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir", "repo/pkg"]"#);
        let repo = base.join("repo");
        let (store, emb) = mock();
        cache_root(&store, &emb, &repo.join("pkg")).await;
        let got = ensure_marker_for_setup(&store, Some(&emb), &repo, Some(NS)).await;
        assert_eq!(
            got,
            SetupMarker::AskRoot {
                top: repo.clone(),
                sub: repo.join("pkg")
            }
        );
        assert!(!has_marker(&repo) && !has_marker(&repo.join("pkg")));
        let message = got.message(NS, &repo).unwrap();
        assert!(
            message.contains(&format!("root set {}", repo.join("pkg").display())),
            "{message}"
        );
        assert!(
            message.contains(&format!("root set {}", repo.display())),
            "{message}"
        );
        assert!(message.contains("root audit"), "{message}");
        assert_eq!(
            cached(&store).await,
            Some(repo.join("pkg").to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn other_namespace_marker_installs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:other-project", "repo/src"]"#,
        );
        let (store, emb) = mock();
        let got =
            ensure_marker_for_setup(&store, Some(&emb), &under(&base, "repo/src"), Some(NS)).await;
        assert_eq!(
            got,
            SetupMarker::OtherNamespace("other-project".to_string())
        );
        assert!(!got.installs_plugins());
        assert!(got.message(NS, &base).unwrap().contains("root set"));
        assert_eq!(cached(&store).await, None);
        // The marker is untouched.
        let MarkerRead::Valid(cfg) = read_marker(&base.join("repo")) else {
            panic!()
        };
        assert_eq!(cfg.namespace, "other-project");
    }

    #[tokio::test]
    async fn hooks_install_without_explicit_namespace_asks_and_still_installs_hooks() {
        let _env = crate::daemon::project_root_env_test_lock();
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["example-project git-dir"]"#);
        let repo = base.join("example-project");
        let (store, emb) = mock();

        // Only the folder name: no marker, asks with that one candidate.
        let got = ensure_marker_for_setup(&store, Some(&emb), &repo, None).await;
        assert_eq!(
            got,
            SetupMarker::AskNamespace(vec!["example-project".to_string()])
        );
        assert!(!has_marker(&repo));
        // Asking does not stop the hook and plugin install.
        assert!(got.installs_plugins());
        assert!(got
            .message(NS, &repo)
            .unwrap()
            .contains("hooks install --namespace <ns>"));

        // Harness configs that disagree: both candidates listed.
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "alpha"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".NeuroStrata")).unwrap();
        std::fs::write(
            repo.join(".NeuroStrata").join("session_queries.json"),
            r#"{"namespace": "beta", "queries": []}"#,
        )
        .unwrap();
        let got = ensure_marker_for_setup(&store, Some(&emb), &repo, None).await;
        assert_eq!(
            got,
            SetupMarker::AskNamespace(vec!["alpha".to_string(), "beta".to_string()])
        );
        assert!(!has_marker(&repo));
        assert_eq!(cached(&store).await, None);
    }

    #[tokio::test]
    async fn hooks_install_with_namespace_flag_writes() {
        let _env = crate::daemon::project_root_env_test_lock();
        std::env::remove_var("NEUROSTRATA_NAMESPACE");
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["example-project git-dir"]"#);
        let repo = base.join("example-project");
        let (store, emb) = mock();
        let got = ensure_marker_for_setup(&store, Some(&emb), &repo, Some("team:proj")).await;
        assert_eq!(got, SetupMarker::Written(repo.clone()));
        let MarkerRead::Valid(cfg) = read_marker(&repo) else {
            panic!()
        };
        assert_eq!(cfg.namespace, "team:proj");

        // One explicit source is enough without the flag.
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["example-project git-dir"]"#);
        let repo = base.join("example-project");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude").join("settings.local.json"),
            r#"{"env": {"NEUROSTRATA_NAMESPACE": "alpha"}}"#,
        )
        .unwrap();
        let (store, emb) = mock();
        let got = ensure_marker_for_setup(&store, Some(&emb), &repo, None).await;
        assert_eq!(got, SetupMarker::Written(repo.clone()));
        let MarkerRead::Valid(cfg) = read_marker(&repo) else {
            panic!()
        };
        assert_eq!(cfg.namespace, "alpha");
    }

    #[tokio::test]
    async fn non_git_setup_at_parent_blocked_by_root_change_check() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(base) = base_outside_git(&tmp) else {
            eprintln!("skipped: the temp folder is inside a git checkout");
            return;
        };
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join("sub")).unwrap();
        std::fs::write(proj.join("sub").join("a.rs"), b"").unwrap();
        let (store, emb) = mock();
        cache_root(&store, &emb, &proj.join("sub")).await;
        rule_at(&store, "rule-a", "a.rs", serde_json::json!({})).await;

        let got = ensure_marker_for_setup(&store, Some(&emb), &proj, Some(NS)).await;
        assert_eq!(
            got,
            SetupMarker::RootChangeBlocked(vec!["rule-a".to_string()])
        );
        assert!(!has_marker(&proj));
        assert_eq!(
            cached(&store).await,
            Some(proj.join("sub").to_string_lossy().into_owned())
        );
        let message = got.message(NS, &proj).unwrap();
        assert!(
            message.contains("rule-a") && message.contains("root audit"),
            "{message}"
        );

        // Setup at the cached root itself changes nothing and writes.
        let got = ensure_marker_for_setup(&store, Some(&emb), &proj.join("sub"), Some(NS)).await;
        assert_eq!(got, SetupMarker::Written(proj.join("sub")));
    }

    #[tokio::test]
    async fn hooks_install_in_second_clone_writes_marker_keeps_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["clone1 git-dir", "clone1 marker:example-project", "clone1/a.rs file",
                "clone2 git-dir", "clone2/a.rs file"]"#,
        );
        let (clone1, clone2) = (base.join("clone1"), base.join("clone2"));
        let (store, emb) = mock();
        cache_root(&store, &emb, &clone1).await;
        // A row keyed relative to the first clone: an unrelated folder is no
        // root change to check, so it does not block.
        rule_at(&store, "rule-a", "a.rs", serde_json::json!({})).await;

        let got = ensure_marker_for_setup(&store, Some(&emb), &clone2, Some(NS)).await;
        assert_eq!(
            got,
            SetupMarker::KeptCache {
                root: clone2.clone(),
                cached: clone1.clone(),
                written: true
            }
        );
        assert_eq!(got.kind(), "written");
        assert!(has_marker(&clone2));
        assert_eq!(
            cached(&store).await,
            Some(clone1.to_string_lossy().into_owned())
        );
        let message = got.message(NS, &clone2).unwrap();
        assert!(
            message.contains(&format!("stays {}", clone1.display())),
            "{message}"
        );
        assert!(message.contains("root set"), "{message}");
        // Again: the marker is reused and the cache still kept.
        let again = ensure_marker_for_setup(&store, Some(&emb), &clone2, Some(NS)).await;
        assert_eq!(again.kind(), "reused");
        assert_eq!(
            cached(&store).await,
            Some(clone1.to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn hooks_install_keeps_unrelated_cache_without_marker() {
        // The upgrade case: the first clone was cached before markers existed,
        // and setup or `hooks install` runs first in a second clone.
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["clone1 git-dir", "clone1/a.rs file", "clone2 git-dir", "clone2/a.rs file"]"#,
        );
        let (clone1, clone2) = (base.join("clone1"), base.join("clone2"));
        let (store, emb) = mock();
        cache_root(&store, &emb, &clone1).await;
        rule_at(&store, "rule-a", "a.rs", serde_json::json!({})).await;

        let got = ensure_marker_for_setup(&store, Some(&emb), &clone2, Some(NS)).await;
        assert_eq!(
            got,
            SetupMarker::KeptCache {
                root: clone2.clone(),
                cached: clone1.clone(),
                written: true
            }
        );
        assert!(has_marker(&clone2) && !has_marker(&clone1));
        assert_eq!(
            cached(&store).await,
            Some(clone1.to_string_lossy().into_owned())
        );
        assert_eq!(
            got.message(NS, &clone2).unwrap(),
            format!(
                "Wrote {} for {NS}. The cached root of {NS} stays {}, a folder outside this one. \
                 Run `neurostrata-mcp root set {} --namespace {NS}` to make {} the cached root.",
                clone2.join(ROOT_CONFIG_REL).display(),
                clone1.display(),
                clone2.display(),
                clone2.display()
            )
        );

        // A missing cached root is replaced.
        let (store, emb) = mock();
        cache_root(&store, &emb, &base.join("gone")).await;
        let got = ensure_marker_for_setup(&store, Some(&emb), &clone1, Some(NS)).await;
        assert_eq!(got, SetupMarker::Written(clone1.clone()));
        assert_eq!(
            cached(&store).await,
            Some(clone1.to_string_lossy().into_owned())
        );

        // `root set` still moves the cache to an unrelated folder.
        let (store, emb) = mock();
        cache_root(&store, &emb, &clone1).await;
        root_set(&store, Some(&emb), &set_args(&clone2))
            .await
            .unwrap();
        assert_eq!(
            cached(&store).await,
            Some(clone2.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn racing_setups_one_wins() {
        for round in 0..10 {
            let tmp = tempfile::tempdir().unwrap();
            let base = dunce::canonicalize(tmp.path()).unwrap();
            layout(&base, r#"["repo git-dir"]"#);
            let repo = base.join("repo");
            let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let runs: Vec<_> = ["example-a", "example-b"]
                .into_iter()
                .map(|ns| {
                    let (store, barrier, repo) = (store.clone(), barrier.clone(), repo.clone());
                    // An embedder, so the winner can also create the registry row.
                    let emb: Arc<dyn Embedder> = Arc::new(StubEmbedder);
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        barrier.wait();
                        rt.block_on(ensure_marker_for_setup(&store, Some(&emb), &repo, Some(ns)))
                    })
                })
                .collect();
            let outcomes: Vec<SetupMarker> = runs.into_iter().map(|t| t.join().unwrap()).collect();
            let written: Vec<_> = outcomes.iter().filter(|o| o.kind() == "written").collect();
            assert_eq!(written.len(), 1, "round {round}: {outcomes:?}");
            assert!(
                outcomes
                    .iter()
                    .any(|o| matches!(o, SetupMarker::OtherNamespace(_))),
                "round {round}: {outcomes:?}"
            );
            let MarkerRead::Valid(cfg) = read_marker(&repo) else {
                panic!()
            };
            let winner = if outcomes[0].kind() == "written" {
                "example-a"
            } else {
                "example-b"
            };
            assert_eq!(cfg.namespace, winner, "round {round}");
        }
    }

    #[tokio::test]
    async fn setup_refuses_linked_neurostrata_and_temp_root() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["elsewhere", "repo git-dir", "repo neurostrata-link:elsewhere"]"#,
        );
        let (store, emb) = mock();
        let got = ensure_marker_for_setup(&store, Some(&emb), &base.join("repo"), Some(NS)).await;
        assert_eq!(got.kind(), "refused", "{got:?}");
        assert!(!base.join("elsewhere").join("config.json").exists());

        let temp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let got = ensure_marker_for_setup(&store, Some(&emb), &temp, Some(NS)).await;
        assert_eq!(
            got,
            SetupMarker::Refused("the system temp folder is not a project root")
        );
        let got =
            ensure_marker_for_setup(&store, Some(&emb), Path::new("relative"), Some(NS)).await;
        assert_eq!(got.kind(), "refused");
    }

    #[test]
    fn root_hint_only_without_a_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["plain git-dir", "plain/src/a.rs file", "owned git-dir", "owned marker:other-project",
                "owned/a.rs file", "ours git-dir", "ours marker:example-project"]"#,
        );
        let hint = root_hint(NS, &under(&base, "plain/src/a.rs")).unwrap();
        assert_eq!(
            hint,
            "No .NeuroStrata/config.json for example-project: run `neurostrata-mcp hooks install` or neurostrata_task_setup"
        );
        assert_eq!(root_hint(NS, &under(&base, "owned/a.rs")), None);
        assert_eq!(root_hint(NS, &base.join("ours")), None);
        assert_eq!(root_hint(NS, Path::new("src/a.rs")), None);
        assert_eq!(root_hint(NS, &base.join("missing")), None);
        assert_eq!(root_hint("", &base.join("plain")), None);
    }

    fn set_args(dir: &Path) -> RootSetArgs {
        RootSetArgs {
            dir: dir.to_path_buf(),
            namespace: NS.to_string(),
            source_id: None,
            replace: false,
            force: false,
        }
    }

    #[tokio::test]
    async fn refuses_root_change_with_affected_rows_then_force_proceeds() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir", "repo/sub/a.rs file"]"#);
        let repo = base.join("repo");
        let (store, emb) = mock();
        cache_root(&store, &emb, &repo.join("sub")).await;
        rule_at(&store, "rule-a", "a.rs", serde_json::json!({})).await;

        let err = root_set(&store, Some(&emb), &set_args(&repo))
            .await
            .unwrap_err();
        assert!(
            err.contains("rule-a") && err.contains("root audit") && err.contains(ROOT_GUIDE),
            "{err}"
        );
        assert!(!has_marker(&repo));
        assert_eq!(
            cached(&store).await,
            Some(repo.join("sub").to_string_lossy().into_owned())
        );

        let mut forced = set_args(&repo);
        forced.force = true;
        let lines = root_set(&store, Some(&emb), &forced).await.unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("--force") && l.contains("rule-a")),
            "{lines:?}"
        );
        assert!(has_marker(&repo));
        assert_eq!(
            cached(&store).await,
            Some(repo.to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn proceeds_after_audit_corrections() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir", "repo/sub/a.rs file"]"#);
        let repo = base.join("repo");
        let (store, emb) = mock();
        cache_root(&store, &emb, &repo.join("sub")).await;
        rule_at(&store, "rule-a", "a.rs", serde_json::json!({})).await;
        assert!(root_set(&store, Some(&emb), &set_args(&repo))
            .await
            .is_err());

        // The correction: the old row is superseded by one keyed to the new root.
        rule_at(
            &store,
            "rule-a",
            "a.rs",
            serde_json::json!({ "superseded_by": "rule-a2" }),
        )
        .await;
        rule_at(
            &store,
            "rule-a2",
            "sub/a.rs",
            serde_json::json!({ "supersedes": "rule-a" }),
        )
        .await;
        let lines = root_set(&store, Some(&emb), &set_args(&repo))
            .await
            .unwrap();
        assert!(!lines.iter().any(|l| l.starts_with("--force")), "{lines:?}");
        assert!(has_marker(&repo));
        assert_eq!(
            cached(&store).await,
            Some(repo.to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn replace_order_new_then_registry_then_remove_old() {
        for stop in [RootSetStep::Published, RootSetStep::RegistryUpdated] {
            let tmp = tempfile::tempdir().unwrap();
            let base = dunce::canonicalize(tmp.path()).unwrap();
            layout(
                &base,
                r#"["repo git-dir", "repo/old marker:example-project", "repo/new"]"#,
            );
            let repo = base.join("repo");
            let (old, new) = (repo.join("old"), repo.join("new"));
            let (store, emb) = mock();
            cache_root(&store, &emb, &old).await;

            // Another marker for the namespace in this checkout: --replace needed.
            let err = root_set(&store, Some(&emb), &set_args(&new))
                .await
                .unwrap_err();
            assert!(err.contains("--replace"), "{err}");
            assert!(!has_marker(&new));

            let mut args = set_args(&new);
            args.replace = true;
            // Interrupted after the step: both markers are there and root show
            // lists both.
            root_set_with(
                &StoreRegistry {
                    store: store.clone(),
                    emb: Some(emb.clone()),
                    checkpoint: false,
                },
                &args,
                Some(stop),
            )
            .await
            .unwrap();
            assert!(has_marker(&new) && has_marker(&old), "{stop:?}");
            let shown = format_root_show(
                &repo,
                &find_root_marker(&repo),
                NS,
                &CachedRoot::Known(cached(&store).await),
                &Ok(std::collections::HashMap::new()),
            );
            assert!(
                shown.contains(&format!("  {} (namespace {NS})", old.display())),
                "{shown}"
            );
            assert!(
                shown.contains(&format!("  {} (namespace {NS})", new.display())),
                "{shown}"
            );
            let expected_cache = if stop == RootSetStep::Published {
                &old
            } else {
                &new
            };
            assert_eq!(
                cached(&store).await,
                Some(expected_cache.to_string_lossy().into_owned())
            );

            // Rerunning the same command finishes.
            let lines = root_set(&store, Some(&emb), &args).await.unwrap();
            assert!(has_marker(&new) && !has_marker(&old), "{stop:?}: {lines:?}");
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with("Removed the old marker")),
                "{lines:?}"
            );
            assert_eq!(
                cached(&store).await,
                Some(new.to_string_lossy().into_owned())
            );
        }
    }

    /// Codex review 4670091232: competing replaces must never leave the
    /// cached root pointing at a folder whose marker was deleted by the other
    /// writer's stale scan.
    #[tokio::test]
    async fn concurrent_replaces_never_leave_the_cached_root_unmarked() {
        for _ in 0..20 {
            let tmp = tempfile::tempdir().unwrap();
            let base = dunce::canonicalize(tmp.path()).unwrap();
            layout(
                &base,
                r#"["repo git-dir", "repo/old marker:example-project", "repo/new", "repo/newer"]"#,
            );
            let repo = base.join("repo");
            let (store, emb) = mock();
            cache_root(&store, &emb, &repo.join("old")).await;
            let registry = StoreRegistry {
                store: store.clone(),
                emb: Some(emb.clone()),
                checkpoint: false,
            };
            let mut left = set_args(&repo.join("new"));
            left.replace = true;
            let mut right = set_args(&repo.join("newer"));
            right.replace = true;
            let (a, b) = tokio::join!(
                root_set_with(&registry, &left, None),
                root_set_with(&registry, &right, None)
            );
            // Either writer may legitimately lose the race.
            let _ = (a, b);
            let cached_root = cached(&store).await.expect("cached root");
            assert!(
                has_marker(Path::new(&cached_root)),
                "cached root {cached_root} must still have a marker"
            );
        }
    }

    #[tokio::test]
    async fn root_set_reports_a_cache_it_could_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir"]"#);
        let repo = base.join("repo");
        // No registry row and no embedder to create one: the cache cannot be set.
        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let err = root_set(&store, None, &set_args(&repo)).await.unwrap_err();
        assert!(!err.contains("Cached root of"), "{err}");
        assert!(err.contains("could not be set"), "{err}");
        assert!(has_marker(&repo));

        let (store, emb) = mock();
        let lines = root_set(&store, Some(&emb), &set_args(&repo))
            .await
            .unwrap();
        assert!(
            lines.iter().any(|l| l.starts_with("Cached root of")),
            "{lines:?}"
        );
    }

    /// Setup whose cache write fails reports it: the marker is written, the
    /// outcome is a failure the reply and `hooks install` show.
    #[tokio::test]
    async fn setup_reports_a_cache_it_could_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir"]"#);
        let repo = base.join("repo");
        // No registry row and no embedder to create one.
        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let got = ensure_marker_for_setup(&store, None, &repo, Some(NS)).await;
        let SetupMarker::CacheNotSet {
            root,
            written,
            reason,
        } = &got
        else {
            panic!("{got:?}")
        };
        assert_eq!((root, *written), (&repo, true));
        assert_eq!(reason, "the registry row could not be written");
        assert!(got.failed() && got.installs_plugins());
        assert_eq!(got.kind(), "cache_not_set");
        assert_eq!(got.root(), Some(repo.as_path()));
        assert!(has_marker(&repo));
        let message = got.message(NS, &repo).unwrap();
        assert!(
            message.contains("could not be set") && message.contains("root set"),
            "{message}"
        );
        assert_eq!(got.to_json(NS, &repo)["reason"], serde_json::json!(reason));
        // A rerun that still cannot set it says so again (not "reused").
        let again = ensure_marker_for_setup(&store, None, &repo, Some(NS)).await;
        assert_eq!(again.kind(), "cache_not_set", "{again:?}");
        assert!(!SetupMarker::Written(repo.clone()).failed());
    }

    /// A root moved to an unrelated folder (a sibling, another clone) lists
    /// the rows whose key names a file under the old root but nothing under
    /// the new one.
    #[tokio::test]
    async fn root_set_to_a_sibling_checks_rows_by_file_existence() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["api git-dir", "api/a.rs file", "api/both.rs file", "web git-dir", "web/both.rs file"]"#,
        );
        let (api, web) = (base.join("api"), base.join("web"));
        let (store, emb) = mock();
        cache_root(&store, &emb, &api).await;
        rule_at(&store, "rule-a", "a.rs", serde_json::json!({})).await;
        rule_at(&store, "rule-both", "both.rs", serde_json::json!({})).await;

        let err = root_set(&store, Some(&emb), &set_args(&web))
            .await
            .unwrap_err();
        assert!(
            err.contains("rule-a") && !err.contains("rule-both"),
            "{err}"
        );
        assert!(
            err.contains(&format!("root audit --for-root {}", web.display())),
            "{err}"
        );
        assert!(!has_marker(&web));
        assert_eq!(
            cached(&store).await,
            Some(api.to_string_lossy().into_owned())
        );

        // Once the file exists under the new root too, nothing is lost.
        std::fs::write(web.join("a.rs"), b"").unwrap();
        root_set(&store, Some(&emb), &set_args(&web)).await.unwrap();
        assert_eq!(
            cached(&store).await,
            Some(web.to_string_lossy().into_owned())
        );
    }

    /// A registry that, like a second `root set` racing this one, moves the
    /// cached root back to `back_to` right after every write.
    struct RacedRegistry {
        inner: StoreRegistry,
        back_to: PathBuf,
    }

    #[async_trait::async_trait]
    impl RootRegistry for RacedRegistry {
        async fn cached_root(&self, namespace: &str) -> Result<Option<String>, String> {
            self.inner.cached_root(namespace).await
        }
        async fn rows(&self, namespace: &str) -> Result<Vec<crate::traits::SearchResult>, String> {
            self.inner.rows(namespace).await
        }
        async fn set_root(&self, namespace: &str, root: &Path) -> Result<(), String> {
            self.inner.set_root(namespace, root).await?;
            self.inner.set_root(namespace, &self.back_to).await
        }
    }

    #[tokio::test]
    async fn replace_keeps_an_old_marker_that_became_the_cached_root_again() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo/old marker:example-project", "repo/new"]"#,
        );
        let repo = base.join("repo");
        let (old, new) = (repo.join("old"), repo.join("new"));
        let (store, emb) = mock();
        cache_root(&store, &emb, &old).await;
        let registry = RacedRegistry {
            inner: StoreRegistry {
                store: store.clone(),
                emb: Some(emb.clone()),
                checkpoint: false,
            },
            back_to: old.clone(),
        };
        let mut args = set_args(&new);
        args.replace = true;
        let lines = root_set_with(&registry, &args, None).await.unwrap();
        assert!(
            has_marker(&old),
            "the cached root keeps its marker: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("Kept the old marker")),
            "{lines:?}"
        );
        assert_eq!(
            cached(&store).await,
            Some(old.to_string_lossy().into_owned())
        );
    }

    /// Without a daemon the CLI reads the store directly: the namespace is
    /// matched to its stored spelling, as the daemon does, so the existing
    /// cache and rows are checked instead of a second registry row.
    #[tokio::test]
    async fn store_registry_uses_the_stored_namespace_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir", "repo/sub/a.rs file"]"#);
        let repo = base.join("repo");
        let (store, emb) = mock();
        let stored = "Example-Project";
        crate::registry::cache_project_root(
            &store,
            Some(&emb),
            stored,
            &repo.join("sub").to_string_lossy(),
        )
        .await;
        store
            .upsert(
                stored,
                "rule-a",
                vec![0.0; 4],
                MemoryPayload {
                    content: "rule a".to_string(),
                    user_id: "agent".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "a.rs".to_string(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .unwrap();
        let err = root_set(&store, Some(&emb), &set_args(&repo))
            .await
            .unwrap_err();
        assert!(err.contains("rule-a"), "{err}");
        let mut forced = set_args(&repo);
        forced.force = true;
        root_set(&store, Some(&emb), &forced).await.unwrap();
        assert_eq!(
            crate::registry::cached_project_root(&store, stored)
                .await
                .unwrap(),
            Some(repo.to_string_lossy().into_owned())
        );
        assert_eq!(
            cached(&store).await,
            None,
            "no second registry row under the typed spelling"
        );
    }

    #[tokio::test]
    async fn split_under_other_namespace_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(
            &base,
            r#"["repo git-dir", "repo marker:outer-project", "repo/packages/api"]"#,
        );
        let api = under(&base, "repo/packages/api");
        let (store, emb) = mock();
        let lines = root_set(&store, Some(&emb), &set_args(&api)).await.unwrap();
        let MarkerRead::Valid(inner) = read_marker(&api) else {
            panic!()
        };
        assert_eq!(inner.namespace, NS);
        let MarkerRead::Valid(outer) = read_marker(&base.join("repo")) else {
            panic!()
        };
        assert_eq!(outer.namespace, "outer-project");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Commit .NeuroStrata/config.json")),
            "{lines:?}"
        );
        assert_eq!(
            cached(&store).await,
            Some(api.to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn refuses_folder_with_other_namespace_without_replace() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir", "repo marker:other-project"]"#);
        let repo = base.join("repo");
        let (store, emb) = mock();
        let err = root_set(&store, Some(&emb), &set_args(&repo))
            .await
            .unwrap_err();
        assert!(
            err.contains("other-project") && err.contains("--replace"),
            "{err}"
        );
        assert_eq!(cached(&store).await, None);

        let mut args = set_args(&repo);
        args.replace = true;
        root_set(&store, Some(&emb), &args).await.unwrap();
        let MarkerRead::Valid(cfg) = read_marker(&repo) else {
            panic!()
        };
        assert_eq!(cfg.namespace, NS);
    }

    #[tokio::test]
    async fn recommends_commit_when_not_checkout_top() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        layout(&base, r#"["repo git-dir", "repo/sub", "top git-dir"]"#);
        let (store, emb) = mock();
        let lines = root_set(&store, Some(&emb), &set_args(&under(&base, "repo/sub")))
            .await
            .unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l == "Commit .NeuroStrata/config.json so other clones use the same root."),
            "{lines:?}"
        );

        let (store, emb) = mock();
        let lines = root_set(&store, Some(&emb), &set_args(&base.join("top")))
            .await
            .unwrap();
        assert!(!lines.iter().any(|l| l.starts_with("Commit")), "{lines:?}");
        // A non-git parent of checkouts is refused.
        if let Some(base) = base_outside_git(&tmp) {
            let err = root_set(&store, Some(&emb), &set_args(&base))
                .await
                .unwrap_err();
            assert!(
                err.contains("refused") && err.contains("git checkout"),
                "{err}"
            );
        }
    }
}
