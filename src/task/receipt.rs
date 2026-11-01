//! Receipt binds to HEAD_sha + dirty_tree_hash (Task 7).
#[derive(Debug, Clone)]
pub struct Receipt {
    pub task_id: String,
    pub worktree: String,
    pub head_sha: String,
    pub dirty_tree_hash: String,
}

impl Receipt {
    pub fn is_current(&self, head_sha: &str, dirty_tree_hash: &str) -> bool {
        self.head_sha == head_sha && self.dirty_tree_hash == dirty_tree_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_matches_when_sha_and_hash_align() {
        let r = Receipt { task_id: "t1".into(), worktree: "w".into(), head_sha: "a".into(), dirty_tree_hash: "b".into() };
        assert!(r.is_current("a", "b"));
    }
}
