//! Cross-repo linking helpers (in-house).
pub fn link(a: &str, b: &str) -> bool { !a.is_empty() && !b.is_empty() && a != b }
pub fn same_repo(a: &str, b: &str) -> bool { a.split('/').next() == b.split('/').next() && !a.is_empty() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_distinct() { assert!(link("repo/a","repo/b")); assert!(!link("x","x")); }
    #[test]
    fn same_repo_detects() { assert!(same_repo("r/a","r/b")); assert!(!same_repo("r/a","s/b")); }
}
