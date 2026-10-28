//! Cross-repo symbol linking (in-house): link symbols by name.
pub fn link(a: &str, b: &str) -> bool { a == b && !a.is_empty() }
#[cfg(test)] mod tests { use super::*; #[test] fn links() { assert!(link("Foo","Foo")); assert!(!link("Foo","Bar")); } }
