//! Ontology evolution drift (in-house): detect slug changes between two sets.
pub fn drift(old: &[String], new: &[String]) -> Vec<String> {
    old.iter().filter(|x| !new.contains(x)).cloned().collect()
}
#[cfg(test)] mod tests { use super::*; #[test] fn detects() { assert_eq!(drift(&["a".into()],&["b".into()]),vec!["a"]); } }
