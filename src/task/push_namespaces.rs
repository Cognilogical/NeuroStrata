//! The namespaces a `git push` must pass the task gate for (spec 6.7).
//!
//! `neurostrata-mcp namespace resolve --pushed` reads the pre-push hook's
//! stdin and prints the union of:
//! 1. the checkout's folder name (the namespace the hook always gated),
//! 2. the resolved namespace of the checkout top (`namespace resolve`),
//! 3. the namespace of every path the push changes, read from the pushed
//!    commits rather than the working tree, then from untracked markers in
//!    the working tree and the main checkout (`find_root_marker`).
//!
//! Nothing here is stored: a split committed on another branch, a nested
//! split, a split committed in another clone and a hand-added marker are all
//! found from the push itself.

use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Parses `<local ref> <local sha> <remote ref> <remote sha>` lines. Blank
/// or malformed lines are skipped.
pub fn parse_pre_push_stdin(text: &str) -> Vec<PushedRef> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let r = PushedRef {
                local_ref: fields.next()?.to_string(),
                local_sha: fields.next()?.to_string(),
                remote_ref: fields.next()?.to_string(),
                remote_sha: fields.next()?.to_string(),
            };
            fields.next().is_none().then_some(r)
        })
        .collect()
}

/// Runs git; injected so tests can count calls.
pub trait GitRunner {
    /// `git <args>` in `dir`: stdout on exit 0, else the stderr text.
    fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String>;
}

/// The `git` on PATH.
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .map_err(|err| format!("cannot run git: {err}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
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

/// NUL-separated git output (`-z`), so paths with spaces or non-ASCII
/// characters come through unquoted.
fn nul_paths(bytes: Vec<u8>) -> impl Iterator<Item = String> {
    text(bytes)
        .split('\0')
        .map(|p| p.trim_matches('\n').to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
}

/// The repository-relative paths one pushed ref changes.
///
/// - Remote tip present locally: `git diff --name-only <remote> <local>`.
/// - Remote tip all zeros (a new branch) or never fetched: the paths touched
///   by `git rev-list <local> --not --remotes`, the commits on no
///   remote-tracking ref.
/// - When that lists no commit (everything pushed is already on some remote,
///   say a branch pushed under a second name), the tip commit's own paths:
///   `git diff-tree --root <local>`, which is `git diff <local>^ <local>` that
///   also answers for a root commit.
///
/// `-m` lists a merge commit's paths against each parent, so a conflict fix
/// in a merge is gated (more paths than first-parent alone, never fewer).
///
/// Renames are split into a deletion and an addition; a deleted path counts.
fn changed_paths(git: &dyn GitRunner, repo: &Path, r: &PushedRef) -> Result<Vec<String>, String> {
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
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                &r.remote_sha,
                local,
            ],
        )?;
        return Ok(nul_paths(out).collect());
    }
    let commits = text(git.git(repo, &["rev-list", local, "--not", "--remotes"])?);
    if commits.trim().is_empty() {
        let out = git.git(
            repo,
            &[
                "diff-tree",
                "-m",
                "--root",
                "-r",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-z",
                local,
            ],
        )?;
        return Ok(nul_paths(out).collect());
    }
    let out = git.git(
        repo,
        &[
            "log",
            "-m",
            "--format=",
            "--name-only",
            "--no-renames",
            "-z",
            local,
            "--not",
            "--remotes",
        ],
    )?;
    Ok(nul_paths(out).collect())
}

/// `a/b/c.rs` -> `a/b`; a top-level file -> `` (the checkout top).
fn parent_folder(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

fn depth(rel: &str) -> usize {
    if rel.is_empty() {
        0
    } else {
        rel.split('/').count()
    }
}

/// Reads markers for one push: each pushed commit's tree is listed once, and
/// each (commit, folder) and each working-tree folder is read at most once.
struct Resolver<'a> {
    git: &'a dyn GitRunner,
    top: PathBuf,
    canonical_top: PathBuf,
    /// (sha, folder) -> the committed marker in that folder of that commit.
    committed: HashMap<(String, String), Option<String>>,
    /// sha -> the folders of that commit's tree that hold a marker file.
    marker_folders: HashMap<String, HashSet<String>>,
    /// folder -> the deepest working-tree / main-checkout marker for it.
    worktree: HashMap<String, Option<(String, String)>>,
}

impl Resolver<'_> {
    /// The committed marker exactly in `folder` of commit `sha`. A missing
    /// file (or folder) answers `None`; an invalid one is an error.
    fn committed_at(&mut self, sha: &str, folder: &str) -> Result<Option<String>, String> {
        let key = (sha.to_string(), folder.to_string());
        if let Some(hit) = self.committed.get(&key) {
            return Ok(hit.clone());
        }
        let rel = if folder.is_empty() {
            ROOT_CONFIG_REL.to_string()
        } else {
            format!("{folder}/{ROOT_CONFIG_REL}")
        };
        // A failed tree listing, or a failed `git show` of a file the tree
        // lists (a blob missing from a partial clone, say), is an error: a
        // marker that cannot be read must not drop its namespace.
        let listed = self.marker_folders_of(sha, &rel)?.contains(folder);
        let found = match listed {
            false => None,
            true => {
                let bytes = self
                    .git
                    .git(&self.top, &["show", &format!("{sha}:{rel}")])
                    .map_err(|e| {
                        format!("cannot read the project marker {rel} in pushed commit {sha}: {e}")
                    })?;
                let shown = PathBuf::from(format!("{sha}:{rel}"));
                match root_marker::parse_marker_bytes(&shown, &bytes) {
                    MarkerRead::Valid(cfg) => Some(cfg.namespace),
                    MarkerRead::Absent => None,
                    MarkerRead::Unreadable { reason, .. } => {
                        return Err(format!(
                            "cannot read the project marker {rel} in pushed commit {sha}: {reason}"
                        ))
                    }
                }
            }
        };
        self.committed.insert(key, found.clone());
        Ok(found)
    }

    /// The folders of commit `sha` that hold a marker file, from one
    /// `git ls-tree -r` of the commit (it needs only the commit's trees, not
    /// its blobs). `rel` names the file asked for, for the error.
    fn marker_folders_of(&mut self, sha: &str, rel: &str) -> Result<&HashSet<String>, String> {
        if !self.marker_folders.contains_key(sha) {
            let listed = self
                .git
                .git(&self.top, &["ls-tree", "-r", "-z", "--name-only", sha])
                .map_err(|e| {
                    format!("cannot look for the project marker {rel} in pushed commit {sha}: {e}")
                })?;
            let suffix = format!("/{ROOT_CONFIG_REL}");
            let folders = nul_paths(listed)
                .filter_map(|path| {
                    if path == ROOT_CONFIG_REL {
                        Some(String::new())
                    } else {
                        path.strip_suffix(&suffix).map(str::to_string)
                    }
                })
                .collect();
            self.marker_folders.insert(sha.to_string(), folders);
        }
        Ok(&self.marker_folders[sha])
    }

    /// The deepest committed marker at or above `folder` in commit `sha`:
    /// (marker folder, namespace).
    fn committed_for(
        &mut self,
        sha: &str,
        folder: &str,
    ) -> Result<Option<(String, String)>, String> {
        let mut dir = folder.to_string();
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

    /// The deepest untracked marker `find_root_marker` finds for `folder` in
    /// this checkout (own checkout and main checkout): (marker folder
    /// relative to the top, namespace). A marker git tracks is skipped: it is
    /// the checked-out branch's copy, while the pushed commit's tree already
    /// says what the push holds. The search goes on above a skipped marker.
    fn worktree_for(&mut self, folder: &str) -> Result<Option<(String, String)>, String> {
        if let Some(hit) = self.worktree.get(folder) {
            return Ok(hit.clone());
        }
        let mut start = if folder.is_empty() {
            self.top.clone()
        } else {
            self.top.join(folder)
        };
        let found = loop {
            let marker = match root_marker::find_root_marker(&start) {
                Err((file, reason)) => {
                    return Err(format!(
                        "cannot read the project marker {}: {reason}",
                        file.display()
                    ))
                }
                Ok(None) => break None,
                Ok(Some(marker)) => marker,
            };
            let Ok(rel) = marker.root.strip_prefix(&self.canonical_top) else {
                break None;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            // A failed check counts as untracked, so the marker is gated.
            let tracked = self
                .git
                .git(
                    &marker.marker_dir(),
                    &["ls-files", "-z", "--", ROOT_CONFIG_REL],
                )
                .map(|out| !out.is_empty())
                .unwrap_or(false);
            if !tracked {
                break Some((rel, marker.config.namespace));
            }
            match marker.root.parent() {
                Some(parent) if !rel.is_empty() => start = parent.to_path_buf(),
                _ => break None,
            }
        };
        self.worktree.insert(folder.to_string(), found.clone());
        Ok(found)
    }

    /// The namespace for a changed path in commit `sha`, or `None` when no
    /// marker covers it (the caller maps that to the folder name).
    fn namespace_for(&mut self, sha: &str, path: &str) -> Result<Option<String>, String> {
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
                            c_dir
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
    let canonical_top = dunce::canonicalize(&top).unwrap_or_else(|_| top.clone());

    let mut out = BTreeSet::new();
    out.insert(folder_namespace.clone());
    let resolved =
        crate::task::harness_plugins::resolve_namespace(&top).map_err(|e| e.to_string())?;
    out.insert(resolved.namespace);

    let mut resolver = Resolver {
        git,
        top,
        canonical_top,
        committed: HashMap::new(),
        marker_folders: HashMap::new(),
        worktree: HashMap::new(),
    };
    for r in refs {
        // A deletion pushes no content.
        if is_zero_sha(&r.local_sha) {
            continue;
        }
        for path in changed_paths(git, &resolver.top.clone(), r)? {
            let ns = resolver.namespace_for(&r.local_sha, &path)?;
            out.insert(ns.unwrap_or_else(|| folder_namespace.clone()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const ZERO: &str = "0000000000000000000000000000000000000000";

    struct CountingGit {
        lookups: Cell<usize>,
        shows: Cell<usize>,
    }

    impl GitRunner for CountingGit {
        fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            match args.first() {
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
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_skips_malformed_lines() {
        let refs = parse_pre_push_stdin("a 1 b 2\n\nbad line\na 1 b 2 extra\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].remote_sha, "2");
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
        let deletion = parse_pre_push_stdin(&format!("(delete) {ZERO} refs/heads/gone {sha}\n"));
        assert_eq!(
            namespaces_for_push(&dir, &deletion).unwrap(),
            set(&["marked-proj", "top-ns"])
        );
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
        ));
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
        ));
        assert_eq!(
            namespaces_for_push(&dir, &refs).unwrap(),
            set(&["proj", "new-ns"]),
            "only the commits on no remote-tracking ref are read"
        );
        // Everything already on a remote: the tip commit's own paths.
        let refs = parse_pre_push_stdin(&format!(
            "refs/heads/main {pushed} refs/heads/other {ZERO}\n"
        ));
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
        let counting = CountingGit {
            lookups: Cell::new(0),
            shows: Cell::new(0),
        };
        // Folders: "" (README.md), a, a/b, a/.NeuroStrata (the marker).
        let got = namespaces_for_push_with(&counting, &dir, &new_branch(&sha, "main")).unwrap();
        assert_eq!(got, set(&["proj", "a-ns"]));
        assert_eq!(counting.lookups.get(), 1, "one tree listing per pushed commit");
        assert_eq!(counting.shows.get(), 1, "one git show per marker found");

        // Two pushed refs at different commits: one listing each, however
        // many folders.
        write(&dir, "c/d/w.rs", "w");
        let second = commit_all(&dir, "more");
        let counting = CountingGit {
            lookups: Cell::new(0),
            shows: Cell::new(0),
        };
        let mut refs = new_branch(&sha, "main");
        refs.extend(new_branch(&second, "other"));
        let got = namespaces_for_push_with(&counting, &dir, &refs).unwrap();
        assert_eq!(got, set(&["proj", "a-ns"]));
        assert_eq!(counting.lookups.get(), 2, "one tree listing per pushed commit");
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
        git(&dir, &["commit", "-q", "--no-edit"]);
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
}
