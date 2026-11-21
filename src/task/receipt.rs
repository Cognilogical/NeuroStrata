//! Receipt binds to HEAD_sha + dirty_tree_hash (Task 7).

#[derive(Debug, Clone)]
pub struct Receipt {
    pub task_id: String,
    pub worktree: String,
    pub head_sha: String,
    pub dirty_tree_hash: String,
}

impl Receipt {
    #[cfg(test)]
    pub fn is_current(&self, head_sha: &str, dirty_tree_hash: &str) -> bool {
        self.head_sha == head_sha && self.dirty_tree_hash == dirty_tree_hash
    }

    /// Capture the receipt for one task in one worktree, binding it to the
    /// exact (HEAD, dirty-tree-hash) pair observed right now.
    pub fn capture(task_id: impl Into<String>, worktree: &std::path::Path) -> Self {
        let (head_sha, dirty_tree_hash) = compute_worktree_state(worktree);
        Self {
            task_id: task_id.into(),
            worktree: worktree.to_string_lossy().to_string(),
            head_sha,
            dirty_tree_hash,
        }
    }
}

/// Read one worktree's (HEAD sha, hash of `git status --porcelain`). A worktree
/// that git cannot read at all reports a fallback derived from the hash of the
/// worktree path itself (e.g. `("none-<pathh>", "clean-<pathh>")`) so two
/// distinct unreadable worktrees never collide on a shared `("none", "clean")`
/// identity — a receipt bound to those values only ever matches itself, so it
/// never blocks.
pub fn compute_worktree_state(worktree: &std::path::Path) -> (String, String) {
    // Receipt binding is a trust-bearing input: it must be pure git
    // observation, never a repo-supplied manifest's declared values
    // (PR #206 V-3). The Git connector preserves the base non-git fallback
    // (`none-{path_hash}` / `clean-{path_hash}`) inside its digest.
    use crate::workflow::connector::WorkflowConnector as _;
    crate::workflow::connector::GitWorkflowConnector.capture_state_digest(worktree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn receipt_matches_when_sha_and_hash_align() {
        let r = Receipt {
            task_id: "t1".into(),
            worktree: "w".into(),
            head_sha: "a".into(),
            dirty_tree_hash: "b".into(),
        };
        assert!(r.is_current("a", "b"));
    }

    #[test]
    fn capture_binds_to_worktree_and_detects_drift() {
        let tmp = std::env::temp_dir().join(format!("receipt-test-{}", std::process::id()));
        let repo = tmp.join("repo");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&repo).unwrap();

        let run = |args: &[&str]| {
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C").arg(&repo).args(args);
            let out = cmd.output().expect("git available in test env");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&["init", "-q"]);
        run(&["config", "user.email", "worker@example.com"]);
        run(&["config", "user.name", "worker"]);
        std::fs::write(repo.join("tracked.txt"), "v1").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);

        let r = Receipt::capture("t1", Path::new(&repo));
        let (head_sha, dirty_hash) = compute_worktree_state(Path::new(&repo));
        assert!(
            r.is_current(&head_sha, &dirty_hash),
            "fresh capture must be current"
        );
        assert_ne!(r.head_sha, "none");

        // A new commit moves HEAD, so a receipt bound to the old tree is stale.
        std::fs::write(repo.join("tracked.txt"), "v2").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "change"]);

        let (head_sha_after, dirty_hash_after) = compute_worktree_state(Path::new(&repo));
        assert!(
            !r.is_current(&head_sha_after, &dirty_hash_after),
            "stale receipt must not be current"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
