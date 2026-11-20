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

/// Reads markers for one push, each (commit, folder) and each working-tree
/// folder at most once.
struct Resolver<'a> {
    git: &'a dyn GitRunner,
    top: PathBuf,
    canonical_top: PathBuf,
    /// (sha, folder) -> the committed marker in that folder of that commit.
    committed: HashMap<(String, String), Option<String>>,
    /// folder -> the deepest working-tree / main-checkout marker for it.
    worktree: HashMap<String, Option<(String, String, bool)>>,
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
        // `git show` exits non-zero when the commit has no such file; the
        // sha itself was just listed by git, so that is the only failure.
        let found = match self.git.git(&self.top, &["show", &format!("{sha}:{rel}")]) {
            Err(_) => None,
            Ok(bytes) => {
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

    /// The marker `find_root_marker` gives `folder` in this checkout (own
    /// checkout and main checkout): (marker folder relative to the top,
    /// namespace, tracked by git).
    fn worktree_for(&mut self, folder: &str) -> Result<Option<(String, String, bool)>, String> {
        if let Some(hit) = self.worktree.get(folder) {
            return Ok(hit.clone());
        }
        let start = if folder.is_empty() {
            self.top.clone()
        } else {
            self.top.join(folder)
        };
        let found = match root_marker::find_root_marker(&start) {
            Err((file, reason)) => {
                return Err(format!(
                    "cannot read the project marker {}: {reason}",
                    file.display()
                ))
            }
            Ok(None) => None,
            Ok(Some(marker)) => match marker.root.strip_prefix(&self.canonical_top) {
                Err(_) => None,
                Ok(rel) => {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    let tracked = self
                        .git
                        .git(
                            &marker.marker_dir(),
                            &["ls-files", "-z", "--", ROOT_CONFIG_REL],
                        )
                        .map(|out| !out.is_empty())
                        .unwrap_or(false);
                    Some((rel, marker.config.namespace, tracked))
                }
            },
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
            (Some((_, ns)), None) | (None, Some((_, ns, _))) => Some(ns),
            (Some((c_dir, c_ns)), Some((l_dir, l_ns, tracked))) => {
                if c_dir == l_dir {
                    // A tracked file here is the checked-out branch's copy;
                    // the pushed commit's copy is the one being pushed.
                    if !tracked && !c_ns.eq_ignore_ascii_case(&l_ns) {
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
        shows: Cell<usize>,
    }

    impl GitRunner for CountingGit {
        fn git(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            if args.first() == Some(&"show") {
                self.shows.set(self.shows.get() + 1);
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
        let sha = commit_all(&dir, "files");
        let counting = CountingGit {
            shows: Cell::new(0),
        };
        // README.md (top), a/x.rs, a/y.rs, a/b/z.rs: folders "", a, a/b.
        let got = namespaces_for_push_with(&counting, &dir, &new_branch(&sha, "main")).unwrap();
        assert_eq!(got, set(&["proj"]));
        assert_eq!(
            counting.shows.get(),
            3,
            "one git show per folder of the pushed commit"
        );
    }
}
