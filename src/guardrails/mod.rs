//! Neuro-symbolic guardrails (in-house): rule-based validation gate.
pub fn guard(input: &str, forbidden: &[&str]) -> bool {
    let lower = input.to_lowercase();
    !forbidden.iter().any(|f| lower.contains(&f.to_lowercase()))
}
#[cfg(test)] mod tests { use super::*; #[test] fn blocks() { assert!(!guard("delete all", &["delete"])); assert!(guard("read", &["delete"])); } }
