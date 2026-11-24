//! The namespaces a `git push` must pass the task gate for (spec 6.7).
//!
//! `neurostrata-mcp namespace resolve --pushed` reads the pre-push hook's
//! stdin and prints the union of:
//! 1. the checkout's folder name (the namespace the hook always gated),
//! 2. the resolved namespace of the checkout top (`namespace resolve`),
//! 3. the namespace of every path the push changes, read from the pushed
//!    commits rather than the working tree, then from untracked markers in
//!    the working tree and the main checkout,
//! 4. the namespace a marker the push deletes or changes named at the remote
//!    tip, when the remote tip is known.
//!
//! Nothing here is stored: a split committed on another branch, a nested
//! split, a split committed in another clone and a hand-added marker are all
//! found from the push itself.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::root_marker::{self, MarkerRead, ROOT_CONFIG_REL};

/// One line of the pre-push hook's stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedRef {
    pub local_ref: String,
    pub local_sha: String,
    pub remote_ref: String,
    pub remote_sha: String,
}

fn is_object_name(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parses `<local ref> SP <local sha> SP <remote ref> SP <remote sha>`
/// lines, splitting from the right on ASCII space only: the local ref is
/// what the user typed (`HEAD@{1 second ago}` holds spaces), while the other
/// three fields never hold an ASCII space. Other whitespace, such as U+00A0
/// in a branch name, stays part of the ref. Blank lines are skipped; any
/// other line that does not parse is an error, so the hook blocks the push
/// rather than gate it without that ref.
pub fn parse_pre_push_stdin(text: &str) -> Result<Vec<PushedRef>, String> {
    let mut refs = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.rsplitn(4, ' ');
        let (Some(remote_sha), Some(remote_ref), Some(local_sha), Some(local_ref)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("cannot read the pre-push line {line:?}"));
        };
        if local_ref.is_empty()
            || remote_ref.is_empty()
            || !is_object_name(local_sha)
            || !is_object_name(remote_sha)
        {
            return Err(format!("cannot read the pre-push line {line:?}"));
        }
        refs.push(PushedRef {
            local_ref: local_ref.to_string(),
            local_sha: local_sha.to_string(),
            remote_ref: remote_ref.to_string(),
            remote_sha: remote_sha.to_string(),
        });
    }
    Ok(refs)
}

/// Runs git; injected so tests can count calls.
pub trait GitRunner {
    /// `git <args>` in `dir`: stdout on exit 0, else the stderr text.
    fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String>;

    /// Like `git`, but arguments that name git paths are raw bytes. The
    /// default is for test doubles that only handle UTF-8; `SystemGit` keeps
    /// the bytes verbatim so a path with non-UTF-8 bytes still resolves.
    fn git_bytes(&self, dir: &Path, args: &[&[u8]]) -> Result<Vec<u8>, String> {
        let owned: Vec<String> = args.iter().map(|a| show_path(a)).collect();
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.git(dir, &refs)
    }
}

/// The `git` on PATH.
pub struct SystemGit;

/// Variables git sets for a hook. Git runs pre-push in a linked worktree with
/// `GIT_DIR` naming that worktree's git directory; inherited, it makes every
/// child git answer for that worktree whatever its `current_dir` (the main
/// checkout's `HEAD` would be the worktree's).
const HOOK_GIT_ENV: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_COMMON_DIR",
];

#[cfg(unix)]
fn os_part(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_part(bytes: &[u8]) -> std::ffi::OsString {
    std::ffi::OsString::from(show_path(bytes))
}

fn run_git(dir: &Path, args: &[std::ffi::OsString]) -> Result<Vec<u8>, String> {
    let mut cmd = std::process::Command::new("git");
    for var in HOOK_GIT_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|err| format!("cannot run git: {err}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

impl GitRunner for SystemGit {
    fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        let owned: Vec<std::ffi::OsString> =
            args.iter().map(|a| std::ffi::OsString::from(*a)).collect();
        run_git(dir, &owned)
    }

    fn git_bytes(&self, dir: &Path, args: &[&[u8]]) -> Result<Vec<u8>, String> {
        let owned: Vec<std::ffi::OsString> = args.iter().map(|a| os_part(a)).collect();
        run_git(dir, &owned)
    }
}

/// See the module doc. `repo` is any folder in the checkout being pushed.
pub fn namespaces_for_push(repo: &Path, refs: &[PushedRef]) -> Result<BTreeSet<String>, String> {
    namespaces_for_push_with(&SystemGit, repo, refs)
}

fn is_zero_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|b| b == b'0')
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Display-only rendering of a raw git path. Never used for lookups: a
/// lossy conversion could map two distinct paths to the same string.
fn show_path(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// NUL-separated git output (`-z`). Git paths are arbitrary bytes except NUL:
/// keep them verbatim. Trimming newlines or replacing invalid UTF-8 here would
/// target a different folder and let a split namespace escape the gate.
fn nul_paths(bytes: Vec<u8>) -> Vec<Vec<u8>> {
    bytes
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_vec())
        .collect()
}

/// The paths one pushed ref changes, and the remote tip they were compared
/// with when git has it.
struct Changes {
    paths: Vec<Vec<u8>>,
    remote_base: Option<String>,
}

/// The repository-relative paths one pushed ref changes.
///
/// A path changed and later reverted still names a split namespace the
/// destination may not have, so collect paths from the whole history being
/// pushed, not from the endpoint tree diff. `-m` lists a merge commit's paths
/// against each parent; renames are split into a deletion and an addition.
///
/// - Remote tip present locally: `git log <remote>..<local>`.
/// - Remote tip all zeros (a new branch) or never fetched: the paths touched
///   by `git rev-list <local> --not --remotes`, the commits on no
///   remote-tracking ref.
/// - When that lists no commit (everything pushed is already on some remote,
///   say a branch pushed under a second name), fall back to the full history
///   of `<local>`: an earlier split must be gated even if the tip is unrelated.
///
/// Every `git log` passes `--no-show-signature`: with the user's
/// `log.showSignature` on, git prints signature checks into the path list.
fn changed_paths(git: &dyn GitRunner, repo: &Path, r: &PushedRef) -> Result<Changes, String> {
    let local = r.local_sha.as_str();
    let remote_known = !is_zero_sha(&r.remote_sha)
        && git
            .git(
                repo,
                &["cat-file", "-e", &format!("{}^{{commit}}", r.remote_sha)],
            )
            .is_ok();
    if remote_known {
        let out = git.git(
            repo,
            &[
                "log",
                "--no-show-signature",
                "-m",
                "--format=",
                "--name-only",
                "--no-renames",
                "-z",
                &format!("{}..{local}", r.remote_sha),
            ],
        )?;
        return Ok(Changes {
            paths: nul_paths(out),
            remote_base: Some(r.remote_sha.clone()),
        });
    }
    let commits = text(git.git(repo, &["rev-list", local, "--not", "--remotes"])?);
    let args: &[&str] = if commits.trim().is_empty() {
        &[
            "log",
            "--no-show-signature",
            "-m",
            "--format=",
            "--name-only",
            "--no-renames",
            "-z",
            local,
        ]
    } else {
        &[
            "log",
            "--no-show-signature",
            "-m",
            "--format=",
            "--name-only",
            "--no-renames",
            "-z",
            local,
            "--not",
            "--remotes",
        ]
    };
    Ok(Changes {
        paths: nul_paths(git.git(repo, args)?),
        remote_base: None,
    })
}

/// `a/b/c.rs` -> `a/b`; a top-level file -> `` (the checkout top). Raw bytes
/// stay intact: folder names may hold newlines or non-UTF-8 bytes.
fn parent_folder(path: &[u8]) -> Vec<u8> {
    match path.iter().rposition(|&b| b == b'/') {
        Some(at) => path[..at].to_vec(),
        None => Vec::new(),
    }
}

fn marker_rel(folder: &[u8]) -> Vec<u8> {
    if folder.is_empty() {
        ROOT_CONFIG_REL.as_bytes().to_vec()
    } else {
        [folder, b"/", ROOT_CONFIG_REL.as_bytes()].concat()
    }
}

/// The folder whose marker a tree path is, or lies under (a tree that holds
/// a folder where the marker file belongs): `(folder, is the file itself)`.
fn marker_entry(path: &[u8]) -> Option<(Vec<u8>, bool)> {
    let root = ROOT_CONFIG_REL.as_bytes();
    if path == root {
        return Some((Vec::new(), true));
    }
    let suffix: Vec<u8> = [b"/", root].concat();
    if path.ends_with(&suffix) && path.len() > suffix.len() {
        return Some((path[..path.len() - suffix.len()].to_vec(), true));
    }
    let below: Vec<u8> = [root, b"/"].concat();
    if path.len() < below.len() {
        return None;
    }
    (0..=path.len() - below.len())
        .find(|&at| {
            &path[at..at + below.len()] == below.as_slice() && (at == 0 || path[at - 1] == b'/')
        })
        .map(|at| (path[..at.saturating_sub(1)].to_vec(), false))
}

/// `git ls-tree -r` of `treeish` (it needs only the trees, not the blobs):
/// folder -> whether its marker path is a file (`false`: a folder sits where
/// the marker file belongs).
fn marker_entries(
    git: &dyn GitRunner,
    dir: &Path,
    treeish: &str,
) -> Result<HashMap<Vec<u8>, bool>, String> {
    let listed = git.git(dir, &["ls-tree", "-r", "-z", "--name-only", treeish])?;
    let mut entries = HashMap::new();
    for (folder, is_file) in nul_paths(listed).iter().filter_map(|p| marker_entry(p)) {
        let slot = entries.entry(folder).or_insert(is_file);
        *slot &= is_file;
    }
    Ok(entries)
}

/// A folder of the working tree that may hold a marker for a pushed path.
struct Level {
    /// Relative to the checkout top, `/`-separated; `` for the top. Raw bytes.
    rel: Vec<u8>,
    dir: PathBuf,
}

/// The folders from `top` down to `top/<folder>` that belong to the checkout
/// at `top`, top first. The list stops before a folder that is missing, a
/// link or junction, holds its own `.git` (a submodule or nested clone,
/// whose markers are not this checkout's) or resolves outside `top`.
fn levels(top: &Path, folder: &[u8]) -> Vec<Level> {
    let mut out = vec![Level {
        rel: Vec::new(),
        dir: top.to_path_buf(),
    }];
    if folder.is_empty() {
        return out;
    }
    let mut dir = top.to_path_buf();
    let mut rel = Vec::new();
    for part in folder.split(|&b| b == b'/') {
        dir.push(os_part(part));
        let real_dir = std::fs::symlink_metadata(&dir)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
        if !real_dir
            || dir.join(".git").exists()
            || !dunce::canonicalize(&dir).is_ok_and(|c| c.starts_with(top))
        {
            break;
        }
        if !rel.is_empty() {
            rel.push(b'/');
        }
        rel.extend_from_slice(part);
        out.push(Level {
            rel: rel.clone(),
            dir: dir.clone(),
        });
    }
    out
}

/// The marker in working-tree folder `dir`, valid or unreadable; `None`
/// when there is none. The rules of `find_root_marker`: a `.NeuroStrata`
/// that is a link is none, one that cannot be inspected is unreadable.
fn working_tree_marker(dir: &Path) -> Option<MarkerRead> {
    let folder = dir.join(".NeuroStrata");
    match std::fs::symlink_metadata(&folder) {
        Ok(_) => {}
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return None
        }
        Err(err) => {
            return Some(MarkerRead::Unreadable {
                path: folder,
                reason: err.to_string(),
            })
        }
    }
    if !crate::source_context::is_valid_marker_directory(&folder) {
        return None;
    }
    match root_marker::read_marker(dir) {
        MarkerRead::Absent => None,
        read => Some(read),
    }
}

/// Reads markers for one push: each pushed commit's tree and each
/// checkout's `HEAD` tree is listed once, and each (commit, folder) and each
/// working-tree folder is read at most once.
struct Resolver<'a> {
    git: &'a dyn GitRunner,
    top: PathBuf,
    /// The checkout's top canonicalized, and the main checkout's when this
    /// is a linked worktree.
    own_top: PathBuf,
    main_top: Option<PathBuf>,
    /// (sha, folder) -> the committed marker in that folder of that commit:
    /// its namespace, or why it cannot be read.
    committed: HashMap<(String, Vec<u8>), Option<Result<String, String>>>,
    /// sha -> the marker entries of that commit's tree.
    marker_folders: HashMap<String, HashMap<Vec<u8>, bool>>,
    /// checkout top -> the marker folders its `HEAD` tracks, or the failure.
    tracked: HashMap<PathBuf, Result<HashMap<Vec<u8>, bool>, String>>,
    /// folder -> the deepest untracked working-tree / main-checkout marker.
    worktree: HashMap<Vec<u8>, Option<(Vec<u8>, String)>>,
}

impl Resolver<'_> {
    /// The committed marker exactly in `folder` of commit `sha`: `None` when
    /// missing, `Some(Err(reason))` when it cannot be parsed. A failed tree
    /// listing, or a failed `git show` of a file the tree lists (a blob
    /// missing from a partial clone, say), is the outer error: a marker that
    /// cannot be read must not drop its namespace.
    fn committed_marker(
        &mut self,
        sha: &str,
        folder: &[u8],
    ) -> Result<Option<Result<String, String>>, String> {
        let key = (sha.to_string(), folder.to_vec());
        if let Some(hit) = self.committed.get(&key) {
            return Ok(hit.clone());
        }
        let rel = marker_rel(folder);
        let rel_shown = show_path(&rel);
        if !self.marker_folders.contains_key(sha) {
            let entries = marker_entries(self.git, &self.top, sha).map_err(|e| {
                format!(
                    "cannot look for the project marker {rel_shown} in pushed commit {sha}: {e}"
                )
            })?;
            self.marker_folders.insert(sha.to_string(), entries);
        }
        let found = match self.marker_folders[sha].get(folder) {
            None => None,
            Some(false) => Some(Err("it is a folder, not a file".to_string())),
            Some(true) => {
                let mut spec = format!("{sha}:").into_bytes();
                spec.extend_from_slice(&rel);
                let bytes = self
                    .git
                    .git_bytes(&self.top, &[&b"show"[..], spec.as_slice()])
                    .map_err(|e| {
                        format!(
                            "cannot read the project marker {rel_shown} in pushed commit {sha}: {e}"
                        )
                    })?;
                let shown = PathBuf::from(format!("{sha}:{rel_shown}"));
                match root_marker::parse_marker_bytes(&shown, &bytes) {
                    MarkerRead::Valid(cfg) => Some(Ok(cfg.namespace)),
                    MarkerRead::Absent => None,
                    MarkerRead::Unreadable { reason, .. } => Some(Err(reason)),
                }
            }
        };
        self.committed.insert(key, found.clone());
        Ok(found)
    }

    /// The committed marker exactly in `folder` of commit `sha`; one that
    /// cannot be read is an error.
    fn committed_at(&mut self, sha: &str, folder: &[u8]) -> Result<Option<String>, String> {
        match self.committed_marker(sha, folder)? {
            None => Ok(None),
            Some(Ok(ns)) => Ok(Some(ns)),
            Some(Err(reason)) => Err(format!(
                "cannot read the project marker {} in pushed commit {sha}: {reason}",
                show_path(&marker_rel(folder))
            )),
        }
    }

    /// The deepest committed marker at or above `folder` in commit `sha`:
    /// (marker folder, namespace).
    fn committed_for(
        &mut self,
        sha: &str,
        folder: &[u8],
    ) -> Result<Option<(Vec<u8>, String)>, String> {
        let mut dir = folder.to_vec();
        loop {
            if let Some(ns) = self.committed_at(sha, &dir)? {
                return Ok(Some((dir, ns)));
            }
            if dir.is_empty() {
                return Ok(None);
            }
            dir = parent_folder(&dir);
        }
    }

    /// Whether the `HEAD` of the checkout at `checkout` tracks the marker of
    /// `rel` (as a file, or with a folder in its place). One `git ls-tree`
    /// per checkout. A failed listing is an error, never "untracked": an
    /// unreadable HEAD must not let a deeper working-tree marker override the
    /// pushed commit's shallower one.
    fn tracked_in_head(&mut self, checkout: &Path, rel: &[u8]) -> Result<bool, String> {
        if !self.tracked.contains_key(checkout) {
            let entries = marker_entries(self.git, checkout, "HEAD").map_err(|e| {
                format!(
                    "cannot list the tracked markers in the HEAD of {}: {e}",
                    checkout.display()
                )
            });
            self.tracked.insert(checkout.to_path_buf(), entries);
        }
        match &self.tracked[checkout] {
            Ok(entries) => Ok(entries.contains_key(rel)),
            Err(e) => Err(e.clone()),
        }
    }

    /// The deepest untracked marker in this checkout or its main checkout
    /// for `folder`: (marker folder relative to the top, namespace).
    ///
    /// Folder by folder from `folder` up to the top, the own checkout's
    /// marker first, then the main checkout's at the same folder. A marker
    /// the checkout's `HEAD` tracks is skipped, valid or not: it is the
    /// checked-out branch's copy, while the pushed commit's tree already says
    /// what the push holds. An untracked marker that cannot be read is an
    /// error. The walk does not enter a nested checkout.
    fn worktree_for(&mut self, folder: &[u8]) -> Result<Option<(Vec<u8>, String)>, String> {
        if let Some(hit) = self.worktree.get(folder) {
            return Ok(hit.clone());
        }
        let own = levels(&self.own_top, folder);
        let main = self
            .main_top
            .as_deref()
            .map(|m| levels(m, folder))
            .unwrap_or_default();
        let main_top = self.main_top.clone().unwrap_or_default();
        let mut found = None;
        'walk: for depth in (0..own.len().max(main.len())).rev() {
            // At equal depth the own checkout's marker is asked first.
            let candidates = [
                (self.own_top.clone(), own.get(depth)),
                (main_top.clone(), main.get(depth)),
            ];
            for (checkout, level) in candidates {
                let Some(level) = level else { continue };
                let Some(read) = working_tree_marker(&level.dir) else {
                    continue;
                };
                if self.tracked_in_head(&checkout, &level.rel)? {
                    continue;
                }
                match read {
                    MarkerRead::Valid(cfg) => {
                        found = Some((level.rel.clone(), cfg.namespace));
                        break 'walk;
                    }
                    MarkerRead::Unreadable { path, reason } => {
                        return Err(format!(
                            "cannot read the project marker {}: {reason}",
                            path.display()
                        ))
                    }
                    MarkerRead::Absent => {}
                }
            }
        }
        self.worktree.insert(folder.to_vec(), found.clone());
        Ok(found)
    }

    /// The namespace for a changed path in commit `sha`, or `None` when no
    /// marker covers it (the caller maps that to the folder name).
    fn namespace_for(&mut self, sha: &str, path: &[u8]) -> Result<Option<String>, String> {
        // A changed marker names its namespace at the pushed SHA itself.
        // Validate it there directly: parent_folder() of a marker path is the
        // `.NeuroStrata` folder, not the folder the marker configures.
        if let Some((marker_folder, true)) = marker_entry(path) {
            match self.committed_marker(sha, &marker_folder)? {
                Some(Ok(ns)) => return Ok(Some(ns)),
                Some(Err(reason)) => {
                    return Err(format!(
                        "cannot read the project marker {} in pushed commit {sha}: {reason}",
                        show_path(&marker_rel(&marker_folder))
                    ))
                }
                None => {}
            }
        }
        let folder = parent_folder(path);
        let committed = self.committed_for(sha, &folder)?;
        let local = self.worktree_for(&folder)?;
        Ok(match (committed, local) {
            (None, None) => None,
            (Some((_, ns)), None) | (None, Some((_, ns))) => Some(ns),
            (Some((c_dir, c_ns)), Some((l_dir, l_ns))) => {
                if c_dir == l_dir {
                    if !c_ns.eq_ignore_ascii_case(&l_ns) {
                        let shown = if c_dir.is_empty() {
                            ".".to_string()
                        } else {
                            show_path(&c_dir)
                        };
                        return Err(format!(
                            "the project marker in {shown} names {c_ns:?} in pushed commit {sha} \
                             but {l_ns:?} in this checkout; make them agree"
                        ));
                    }
                    Some(c_ns)
                } else if depth(&l_dir) > depth(&c_dir) {
                    Some(l_ns)
                } else {
                    Some(c_ns)
                }
            }
        })
    }
}

fn depth(rel: &[u8]) -> usize {
    if rel.is_empty() {
        0
    } else {
        rel.split(|&b| b == b'/').count()
    }
}

/// [`namespaces_for_push`] with an injected git.
pub fn namespaces_for_push_with(
    git: &dyn GitRunner,
    repo: &Path,
    refs: &[PushedRef],
) -> Result<BTreeSet<String>, String> {
    let top = PathBuf::from(text(git.git(repo, &["rev-parse", "--show-toplevel"])?).trim());
    let folder_namespace = top
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("the checkout top {} has no folder name", top.display()))?;
    let own_top = dunce::canonicalize(&top).unwrap_or_else(|_| top.clone());
    let main_top = crate::paths::main_checkout_root(&own_top.to_string_lossy())
        .filter(|main| *main != own_top);

    // A basename starting with `-` would be parsed as a `task gate` option
    // when the hook feeds `--pushed` back as arguments: never emit one.
    let gate_ready = |ns: String| (!ns.is_empty() && !ns.starts_with('-')).then_some(ns);
    let mut out = BTreeSet::new();
    if let Some(ns) = gate_ready(folder_namespace.clone()) {
        out.insert(ns);
    }
    let resolved =
        crate::task::harness_plugins::resolve_namespace(&top).map_err(|e| e.to_string())?;
    let fallback =
        gate_ready(folder_namespace.clone()).unwrap_or_else(|| resolved.namespace.clone());
    if let Some(ns) = gate_ready(resolved.namespace) {
        out.insert(ns);
    }

    let mut resolver = Resolver {
        git,
        top,
        own_top,
        main_top,
        committed: HashMap::new(),
        marker_folders: HashMap::new(),
        tracked: HashMap::new(),
        worktree: HashMap::new(),
    };
    for r in refs {
        // A deletion pushes no content.
        if is_zero_sha(&r.local_sha) {
            continue;
        }
        let changes = changed_paths(git, &resolver.top.clone(), r)?;
        for path in &changes.paths {
            let ns = resolver.namespace_for(&r.local_sha, path)?;
            if let Some(ns) = gate_ready(ns.unwrap_or_else(|| fallback.clone())) {
                out.insert(ns);
            }
            // A marker the push deletes or changes: the namespace it named
            // at the remote tip is gated too, so a push cannot drop a split
            // from the gate by removing its marker. One that was not valid
            // there named no namespace.
            if let (Some(base), Some((folder, _))) = (&changes.remote_base, marker_entry(path)) {
                if let Some(Ok(ns)) = resolver.committed_marker(base, &folder)? {
                    if let Some(ns) = gate_ready(ns) {
                        out.insert(ns);
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const ZERO: &str = "0000000000000000000000000000000000000000";

    #[derive(Default)]
    struct CountingGit {
        /// `git ls-tree` of a pushed commit.
        lookups: Cell<usize>,
        /// `git ls-tree` of a checkout's `HEAD` (is a marker tracked?).
        heads: Cell<usize>,
        shows: Cell<usize>,
    }

    impl GitRunner for CountingGit {
        fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            match args.first() {
                Some(&"ls-tree") if args.last() == Some(&"HEAD") => {
                    self.heads.set(self.heads.get() + 1)
                }
                Some(&"ls-tree") => self.lookups.set(self.lookups.get() + 1),
                Some(&"show") => self.shows.set(self.shows.get() + 1),
                _ => {}
            }
            SystemGit.git(dir, args)
        }
    }

    /// Real git, except that every `git show` fails, as it does for a blob
    /// missing from a partial clone.
    struct ShowFailsGit;

    impl GitRunner for ShowFailsGit {
        fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            if args.first() == Some(&"show") {
                return Err("fatal: missing blob".to_string());
            }
            SystemGit.git(dir, args)
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        // Hermetic test repositories: a fixed identity and no signing.
        let mut all = vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ];
        all.extend_from_slice(args);
        text(SystemGit.git(dir, &all).unwrap_or_else(|e| panic!("{e}")))
            .trim()
            .to_string()
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn marker(dir: &Path, folder: &str, ns: &str) {
        let rel = if folder.is_empty() {
            ROOT_CONFIG_REL.to_string()
        } else {
            format!("{folder}/{ROOT_CONFIG_REL}")
        };
        write(
            dir,
            &rel,
            &format!("{{\"version\":1,\"namespace\":\"{ns}\"}}"),
        );
    }

    fn commit_all(dir: &Path, msg: &str) -> String {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A fresh checkout `<tmp>/<name>` on branch `main` with one commit.
    fn repo(tmp: &Path, name: &str) -> PathBuf {
        let dir = tmp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir, "README.md", "x\n");
        commit_all(&dir, "init");
        dir
    }

    fn new_branch(local_sha: &str, branch: &str) -> Vec<PushedRef> {
        parse_pre_push_stdin(&format!(
            "refs/heads/{branch} {local_sha} refs/heads/{branch} {ZERO}\n"
        ))
        .unwrap()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    const SHA: &str = "1234567890abcdef1234567890abcdef12345678";

    #[test]
    fn parse_reads_from_the_right_and_rejects_malformed_lines() {
        let refs =
            parse_pre_push_stdin(&format!("refs/heads/a {SHA} refs/heads/b {ZERO}\n\n")).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].remote_sha, ZERO);

        // The local ref is what the user typed and may hold spaces.
        let refs = parse_pre_push_stdin(&format!(
            "HEAD@{{1 second ago}} {SHA} refs/heads/x {ZERO}\n"
        ))
        .unwrap();
        assert_eq!(refs[0].local_ref, "HEAD@{1 second ago}");
        assert_eq!(refs[0].local_sha, SHA);
        assert_eq!(refs[0].remote_ref, "refs/heads/x");

        // U+00A0 is valid in a ref name and is not a field separator.
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/a\u{a0}b {SHA} refs/heads/a\u{a0}b {ZERO}\n"
        ))
        .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].remote_ref, "refs/heads/a\u{a0}b");

        for bad in [
            "bad line\n".to_string(),
            "a 1 b 2\n".to_string(),
            format!("refs/heads/a {SHA} refs/heads/b\n"),
            format!("refs/heads/a {SHA} refs/heads/b {ZERO} extra\n"),
        ] {
            assert!(parse_pre_push_stdin(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn folder_name_always_included() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "plain-proj");
        write(&dir, "src/a.rs", "a");
        let sha = commit_all(&dir, "a");
        assert_eq!(
            namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap(),
            set(&["plain-proj"])
        );

        let dir = repo(tmp.path(), "marked-proj");
        marker(&dir, "", "top-ns");
        let sha = commit_all(&dir, "marker");
        assert_eq!(
            namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap(),
            set(&["marked-proj", "top-ns"]),
            "the folder name stays gated next to the marker's namespace"
        );
        // A branch deletion pushes nothing; the two top namespaces remain.
        let deletion =
            parse_pre_push_stdin(&format!("(delete) {ZERO} refs/heads/gone {sha}\n")).unwrap();
        assert_eq!(
            namespaces_for_push(&dir, &deletion).unwrap(),
            set(&["marked-proj", "top-ns"])
        );
    }

    /// With `log.showSignature` on, git prints signature checks into `git log`
    /// output, where they would read as changed paths. Every `git log` the
    /// gate runs turns that off, on all three routes of `changed_paths`.
    #[test]
    fn every_path_listing_turns_off_signature_output() {
        struct LogArgsGit(std::cell::RefCell<Vec<Vec<String>>>);
        impl GitRunner for LogArgsGit {
            fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
                if args.first() == Some(&"log") {
                    self.0
                        .borrow_mut()
                        .push(args.iter().map(|a| a.to_string()).collect());
                }
                SystemGit.git(dir, args)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        let base = git(&dir, &["rev-parse", "HEAD"]);
        write(&dir, "sub/a.rs", "a");
        let tip = commit_all(&dir, "change");
        let runner = LogArgsGit(Default::default());
        // A new branch with commits on no remote.
        namespaces_for_push_with(&runner, &dir, &new_branch(&tip, "feature")).unwrap();
        // An update of a remote tip known locally.
        let update = format!("refs/heads/main {tip} refs/heads/main {base}
");
        namespaces_for_push_with(&runner, &dir, &parse_pre_push_stdin(&update).unwrap())
            .unwrap();
        // A new branch whose commits are all on a remote-tracking ref.
        git(&dir, &["update-ref", "refs/remotes/origin/main", &tip]);
        namespaces_for_push_with(&runner, &dir, &new_branch(&tip, "other")).unwrap();
        let logs = runner.0.into_inner();
        assert_eq!(logs.len(), 3, "{logs:?}");
        for args in logs {
            assert!(args.iter().any(|a| a == "--no-show-signature"), "{args:?}");
        }
    }

    #[test]
    fn split_marker_committed_only_on_pushed_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        marker(&dir, "sub", "split-ns");
        write(&dir, "sub/a.rs", "a");
        let sha = commit_all(&dir, "split");
        git(&dir, &["checkout", "-q", "main"]);
        assert!(!dir.join("sub").join(".NeuroStrata").exists());
        let got = namespaces_for_push(&dir, &new_branch(&sha, "feature")).unwrap();
        assert_eq!(got, set(&["proj", "split-ns"]));
    }

    #[test]
    fn deleted_folder_resolves_from_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "pkg", "pkg-ns");
        write(&dir, "pkg/gone/f.rs", "f");
        let before = commit_all(&dir, "add");
        std::fs::remove_dir_all(dir.join("pkg").join("gone")).unwrap();
        let after = commit_all(&dir, "delete");
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {after} refs/heads/main {before}\n"
        ))
        .unwrap();
        assert_eq!(
            namespaces_for_push(&dir, &refs).unwrap(),
            set(&["proj", "pkg-ns"])
        );
    }

    #[test]
    fn missing_remote_tip_uses_unpushed_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "old", "old-ns");
        write(&dir, "old/a.rs", "a");
        let pushed = commit_all(&dir, "old");
        let remote = tmp.path().join("remote.git");
        git(
            tmp.path(),
            &["init", "-q", "--bare", remote.to_str().unwrap()],
        );
        git(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
        // Pushed without the hook: this test repo has none installed.
        git(&dir, &["push", "-q", "origin", "main"]);
        marker(&dir, "new", "new-ns");
        write(&dir, "new/b.rs", "b");
        let sha = commit_all(&dir, "new");
        let never_fetched = "1234567890123456789012345678901234567890";
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {sha} refs/heads/main {never_fetched}\n"
        ))
        .unwrap();
        assert_eq!(
            namespaces_for_push(&dir, &refs).unwrap(),
            set(&["proj", "new-ns"]),
            "only the commits on no remote-tracking ref are read"
        );
        // Everything already on a remote: the tip commit's own paths.
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {pushed} refs/heads/other {ZERO}\n"
        ))
        .unwrap();
        assert_eq!(
            namespaces_for_push(&dir, &refs).unwrap(),
            set(&["proj", "old-ns"])
        );
    }

    #[test]
    fn nested_split_and_hand_added_marker_gated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "a", "a-ns");
        marker(&dir, "a/b", "b-ns");
        write(&dir, "a/d.rs", "d");
        write(&dir, "a/b/c.rs", "c");
        write(&dir, "h/f.rs", "f");
        let sha = commit_all(&dir, "nested");
        // Added by hand after the commit, never committed.
        marker(&dir, "h", "h-ns");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap();
        assert_eq!(got, set(&["proj", "a-ns", "b-ns", "h-ns"]));
    }

    #[test]
    fn linked_worktree_push() {
        let tmp = tempfile::tempdir().unwrap();
        let main = repo(tmp.path(), "proj");
        write(&main, "pkg/keep", "k");
        commit_all(&main, "pkg");
        // Untracked marker in the main checkout only.
        marker(&main, "pkg", "pkg-ns");
        let wt = tmp.path().join("proj-wt");
        git(
            &main,
            &["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()],
        );
        write(&wt, "pkg/f.rs", "f");
        let sha = commit_all(&wt, "in worktree");
        let got = namespaces_for_push(&wt, &new_branch(&sha, "wtb")).unwrap();
        // The worktree's folder name (what the hook always gated), the
        // resolved namespace (the main checkout's folder name), and the
        // main checkout's untracked marker mapped into the worktree.
        assert_eq!(got, set(&["proj", "proj-wt", "pkg-ns"]));
    }

    #[test]
    fn committed_and_untracked_markers_disagree_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        write(&dir, "pkg/keep", "k");
        commit_all(&dir, "pkg");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        marker(&dir, "pkg", "one");
        write(&dir, "pkg/a.rs", "a");
        let sha = commit_all(&dir, "one");
        git(&dir, &["checkout", "-q", "main"]);
        marker(&dir, "pkg", "two");
        let err = namespaces_for_push(&dir, &new_branch(&sha, "feature")).unwrap_err();
        assert!(err.contains("\"one\"") && err.contains("\"two\""), "{err}");

        // A committed marker that is not valid on a pushed path blocks too.
        std::fs::remove_dir_all(dir.join("pkg").join(".NeuroStrata")).unwrap();
        git(&dir, &["checkout", "-q", "-b", "bad"]);
        write(
            &dir,
            &format!("bad/{ROOT_CONFIG_REL}"),
            "{\"version\":9,\"namespace\":\"x\"}",
        );
        write(&dir, "bad/a.rs", "a");
        let sha = commit_all(&dir, "bad marker");
        let err = namespaces_for_push(&dir, &new_branch(&sha, "bad")).unwrap_err();
        assert!(err.contains("unknown version 9"), "{err}");
    }

    #[test]
    fn pushed_paths_with_spaces() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "my dir", "sp-ns");
        write(&dir, "my dir/a b.rs", "a");
        let sha = commit_all(&dir, "spaces");
        assert_eq!(
            namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap(),
            set(&["proj", "sp-ns"])
        );
    }

    #[test]
    fn pushed_reads_each_folder_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        write(&dir, "a/x.rs", "x");
        write(&dir, "a/y.rs", "y");
        write(&dir, "a/b/z.rs", "z");
        marker(&dir, "a", "a-ns");
        let sha = commit_all(&dir, "files");
        let counting = CountingGit::default();
        // Folders: "" (README.md), a, a/b, a/.NeuroStrata (the marker).
        let got = namespaces_for_push_with(&counting, &dir, &new_branch(&sha, "main")).unwrap();
        assert_eq!(got, set(&["proj", "a-ns"]));
        assert_eq!(
            counting.lookups.get(),
            1,
            "one tree listing per pushed commit"
        );
        assert_eq!(counting.shows.get(), 1, "one git show per marker found");
        assert_eq!(
            counting.heads.get(),
            1,
            "one HEAD listing however many folders ask whether the marker is tracked"
        );

        // Two pushed refs at different commits: one listing each, however
        // many folders.
        write(&dir, "c/d/w.rs", "w");
        let second = commit_all(&dir, "more");
        let counting = CountingGit::default();
        let mut refs = new_branch(&sha, "main");
        refs.extend(new_branch(&second, "other"));
        let got = namespaces_for_push_with(&counting, &dir, &refs).unwrap();
        assert_eq!(got, set(&["proj", "a-ns"]));
        assert_eq!(
            counting.lookups.get(),
            2,
            "one tree listing per pushed commit"
        );
        assert_eq!(counting.heads.get(), 1, "one HEAD listing per checkout");
    }

    /// Real git, except that every `git ls-tree` fails.
    struct LsTreeFailsGit;

    impl GitRunner for LsTreeFailsGit {
        fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            if args.first() == Some(&"ls-tree") {
                return Err("fatal: not a tree object".to_string());
            }
            SystemGit.git(dir, args)
        }
    }

    #[test]
    fn failed_tree_listing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "pkg", "pkg-ns");
        write(&dir, "pkg/a.rs", "a");
        let sha = commit_all(&dir, "split");
        let err =
            namespaces_for_push_with(&LsTreeFailsGit, &dir, &new_branch(&sha, "main")).unwrap_err();
        assert!(err.contains("cannot look for the project marker"), "{err}");
    }

    #[test]
    fn failed_show_of_a_listed_marker_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "pkg", "pkg-ns");
        write(&dir, "pkg/a.rs", "a");
        let sha = commit_all(&dir, "split");
        let err =
            namespaces_for_push_with(&ShowFailsGit, &dir, &new_branch(&sha, "main")).unwrap_err();
        assert!(err.contains("cannot read the project marker pkg/"), "{err}");
    }

    #[test]
    fn merge_conflict_fix_in_split_is_gated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "sub", "sub-ns");
        write(&dir, "sub/f.rs", "base\n");
        commit_all(&dir, "split");
        let remote = tmp.path().join("remote.git");
        git(
            tmp.path(),
            &["init", "-q", "--bare", remote.to_str().unwrap()],
        );
        git(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&dir, &["checkout", "-q", "-b", "left"]);
        write(&dir, "sub/f.rs", "left\n");
        commit_all(&dir, "left");
        git(&dir, &["checkout", "-q", "main"]);
        git(&dir, &["checkout", "-q", "-b", "right"]);
        write(&dir, "sub/f.rs", "right\n");
        commit_all(&dir, "right");
        git(&dir, &["push", "-q", "origin", "main", "left", "right"]);
        git(&dir, &["checkout", "-q", "-b", "merged", "left"]);
        // Conflicts on sub/f.rs; the fix below is the merge's only change.
        let _ = SystemGit.git(&dir, &["merge", "-q", "right"]);
        write(&dir, "sub/f.rs", "fixed\n");
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "merge fix"]);
        let sha = git(&dir, &["rev-parse", "HEAD"]);
        let got = namespaces_for_push(&dir, &new_branch(&sha, "merged")).unwrap();
        assert_eq!(got, set(&["proj", "sub-ns"]));
    }

    #[test]
    fn tracked_worktree_marker_missing_on_pushed_branch_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "pkg", "pkg-ns");
        write(&dir, "pkg/keep", "k");
        commit_all(&dir, "pkg");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        write(&dir, "pkg/inner/f.rs", "f");
        let sha = commit_all(&dir, "feature");
        git(&dir, &["checkout", "-q", "main"]);
        // The checked-out branch commits a deeper split the pushed one lacks.
        marker(&dir, "pkg/inner", "inner-ns");
        write(&dir, "pkg/inner/g.rs", "g");
        commit_all(&dir, "inner split");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "feature")).unwrap();
        assert_eq!(got, set(&["proj", "pkg-ns"]));
    }

    #[test]
    fn marker_folder_in_pushed_commit_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        write(&dir, &format!("pkg/{ROOT_CONFIG_REL}/placeholder"), "x");
        write(&dir, "pkg/a.rs", "a");
        let sha = commit_all(&dir, "folder where the marker belongs");
        let err = namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap_err();
        assert!(
            err.contains("cannot read the project marker pkg/") && err.contains("not a file"),
            "{err}"
        );
    }

    #[test]
    fn untracked_marker_in_nested_checkout_is_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        write(&dir, "pkg/a.rs", "a");
        let sha = commit_all(&dir, "pkg");
        git(&dir, &["checkout", "-q", "main"]);
        // The checked-out branch leaves `pkg` free for a nested clone.
        let nested = dir.join("pkg");
        std::fs::create_dir_all(&nested).unwrap();
        git(&nested, &["init", "-q"]);
        marker(&nested, "", "nested-ns");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "feature")).unwrap();
        assert_eq!(got, set(&["proj"]));
    }

    #[test]
    fn staged_marker_not_in_head_is_gated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        write(&dir, "h/f.rs", "f");
        let sha = commit_all(&dir, "h");
        marker(&dir, "h", "h-ns");
        git(&dir, &["add", "-A"]);
        let got = namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap();
        assert_eq!(got, set(&["proj", "h-ns"]));
    }

    #[test]
    fn broken_tracked_marker_of_checked_out_branch_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        marker(&dir, "pkg", "pkg-ns");
        write(&dir, "pkg/a.rs", "a");
        let sha = commit_all(&dir, "split");
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, &format!("pkg/{ROOT_CONFIG_REL}"), "{not json");
        commit_all(&dir, "broken marker on main");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "feature")).unwrap();
        assert_eq!(got, set(&["proj", "pkg-ns"]));
    }

    #[test]
    fn marker_changed_or_deleted_by_push_gates_its_old_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "pkg", "old-ns");
        write(&dir, "pkg/a.rs", "a");
        let first = commit_all(&dir, "split");
        marker(&dir, "pkg", "new-ns");
        let renamed = commit_all(&dir, "renamed");
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {renamed} refs/heads/main {first}\n"
        ))
        .unwrap();
        assert_eq!(
            namespaces_for_push(&dir, &refs).unwrap(),
            set(&["proj", "old-ns", "new-ns"])
        );

        std::fs::remove_dir_all(dir.join("pkg").join(".NeuroStrata")).unwrap();
        let deleted = commit_all(&dir, "deleted");
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {deleted} refs/heads/main {renamed}\n"
        ))
        .unwrap();
        assert_eq!(
            namespaces_for_push(&dir, &refs).unwrap(),
            set(&["proj", "new-ns"])
        );
    }

    /// A linked worktree `proj-wt` on branch `wtb`, which commits a marker
    /// in `pkg`; the main checkout `proj` holds an untracked marker in `pkg`;
    /// branch `feature` changes `pkg/f.rs` with no marker. Returns
    /// (worktree, feature tip).
    fn tracked_worktree_marker_over_main_marker(tmp: &Path) -> (PathBuf, String) {
        let main = repo(tmp, "proj");
        write(&main, "pkg/keep", "k");
        commit_all(&main, "pkg");
        let wt = tmp.join("proj-wt");
        git(
            &main,
            &["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()],
        );
        marker(&wt, "pkg", "wt-ns");
        commit_all(&wt, "worktree split");
        git(&main, &["checkout", "-q", "-b", "feature"]);
        write(&main, "pkg/f.rs", "f");
        let sha = commit_all(&main, "feature");
        git(&main, &["checkout", "-q", "main"]);
        marker(&main, "pkg", "main-ns");
        (wt, sha)
    }

    #[test]
    fn tracked_worktree_marker_does_not_hide_main_checkout_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let (wt, sha) = tracked_worktree_marker_over_main_marker(tmp.path());
        let got = namespaces_for_push(&wt, &new_branch(&sha, "feature")).unwrap();
        assert_eq!(got, set(&["proj", "proj-wt", "main-ns"]));
    }

    const HOOK_CHILD_ENV: &str = "NEUROSTRATA_TEST_PUSH_HOOK_CHILD";

    /// Runs [`hook_env_child`] in a child process with the environment git
    /// gives a pre-push hook in a linked worktree: `GIT_DIR` set to the
    /// worktree's absolute git directory, no `GIT_WORK_TREE`, the worktree
    /// top as the current folder.
    #[test]
    fn linked_worktree_push_under_hook_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let (wt, sha) = tracked_worktree_marker_over_main_marker(tmp.path());
        let git_dir = git(&wt, &["rev-parse", "--absolute-git-dir"]);
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "task::push_namespaces::tests::hook_env_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(&wt)
            .env(HOOK_CHILD_ENV, &sha)
            .env("GIT_DIR", &git_dir)
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The child of [`linked_worktree_push_under_hook_environment`]; passes
    /// at once when run on its own.
    #[test]
    fn hook_env_child() {
        let Ok(sha) = std::env::var(HOOK_CHILD_ENV) else {
            return;
        };
        let wt = std::env::current_dir().unwrap();
        let got = namespaces_for_push(&wt, &new_branch(&sha, "feature")).unwrap();
        assert_eq!(got, set(&["proj", "proj-wt", "main-ns"]));
    }

    /// Git paths are arbitrary bytes except NUL: a lossy conversion or a
    /// newline trim would make the marker lookup target a different folder
    /// and let a split namespace escape the gate.
    #[test]
    fn nul_paths_preserve_raw_bytes_and_newlines() {
        let raw = b"dir/a.rs\0dir/\nodd.rs\0dir/\xffbad.rs\0".to_vec();
        assert_eq!(
            nul_paths(raw),
            vec![
                b"dir/a.rs".to_vec(),
                b"dir/\nodd.rs".to_vec(),
                b"dir/\xffbad.rs".to_vec(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_with_a_leading_newline_gates_its_marker() {
        use std::os::unix::ffi::OsStringExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        let folder = PathBuf::from(std::ffi::OsString::from_vec(b"\nnl".to_vec()));
        let marker_path = dir.join(&folder).join(ROOT_CONFIG_REL);
        std::fs::create_dir_all(marker_path.parent().unwrap()).unwrap();
        std::fs::write(&marker_path, b"{\"version\":1,\"namespace\":\"nl-ns\"}").unwrap();
        std::fs::write(dir.join(&folder).join("a.rs"), "a").unwrap();
        let sha = commit_all(&dir, "newline");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap();
        assert!(
            got.contains("nl-ns"),
            "a newline in a folder name must not lose its split: {got:?}"
        );
    }

    /// A path added and then reverted leaves the endpoint tree unchanged, but
    /// the split it touched still has to be gated: collect paths over the whole
    /// history pushed, not just the diff between the two tips.
    #[test]
    fn reverted_split_path_still_gates_its_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "split", "split-ns");
        write(&dir, "split/.keep", "k");
        let remote = commit_all(&dir, "marker");
        write(&dir, "split/a.rs", "a");
        commit_all(&dir, "add");
        std::fs::remove_file(dir.join("split/a.rs")).unwrap();
        let local = commit_all(&dir, "revert");
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {local} refs/heads/main {remote}\n"
        ))
        .unwrap();
        let got = namespaces_for_push_with(&SystemGit, &dir, &refs).unwrap();
        assert_eq!(
            got,
            set(&["proj", "split-ns"]),
            "a reverted split must still be gated: {got:?}"
        );
    }

    /// A push whose only change is a marker must validate that marker at the
    /// pushed SHA: parent_folder() of a marker path is `.NeuroStrata`, not the
    /// folder the marker configures.
    #[test]
    fn a_marker_only_push_gates_and_validates_the_new_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "", "top-ns");
        marker(&dir, "nested", "nested-ns");
        let sha = commit_all(&dir, "add nested marker");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap();
        assert_eq!(
            got,
            set(&["proj", "top-ns", "nested-ns"]),
            "a marker-only push must gate its new namespace: {got:?}"
        );

        // The same marker-only push with an invalid body must be rejected.
        write(&dir, "nested/.NeuroStrata/config.json", "not json\n");
        let bad = commit_all(&dir, "invalid nested marker");
        let err = namespaces_for_push(&dir, &new_branch(&bad, "main")).unwrap_err();
        assert!(
            err.contains("cannot read the project marker nested/"),
            "an invalid changed marker must block: {err}"
        );
    }

    /// Real git, except that listing the checkout's `HEAD` fails. An unknown
    /// tracked status must fail closed instead of reading a deeper working-tree
    /// marker as untracked and letting it hide the pushed namespace.
    struct HeadListFailsGit;

    impl GitRunner for HeadListFailsGit {
        fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            if args.first() == Some(&"ls-tree") && args.last() == Some(&"HEAD") {
                return Err("fatal: not a commit".to_string());
            }
            SystemGit.git(dir, args)
        }
    }

    #[test]
    fn failed_head_listing_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "proj");
        marker(&dir, "", "top-ns");
        write(&dir, "src/a.rs", "a");
        let sha = commit_all(&dir, "split");
        let err = namespaces_for_push_with(&HeadListFailsGit, &dir, &new_branch(&sha, "main"))
            .unwrap_err();
        assert!(
            err.contains("cannot list the tracked markers"),
            "an unreadable HEAD must fail the resolution: {err}"
        );
    }

    /// A checkout whose folder name starts with `-` would be passed to
    /// `task gate` as an option and block the push, so it is never emitted;
    /// the marker namespace still gates the push.
    #[test]
    fn option_like_checkout_basename_is_not_emitted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "-worktree");
        marker(&dir, "", "good-ns");
        write(&dir, "src/a.rs", "a");
        let sha = commit_all(&dir, "split");
        let got = namespaces_for_push(&dir, &new_branch(&sha, "main")).unwrap();
        assert!(
            !got.contains("-worktree"),
            "an option-like basename must not reach task gate: {got:?}"
        );
        assert!(
            got.contains("good-ns"),
            "the marker namespace still gates the push: {got:?}"
        );
    }
}
