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
