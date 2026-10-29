//! Guardrails (in-house).
pub fn guard(input: &str, forbidden: &[&str]) -> bool {
    let low = input.to_lowercase();
    !forbidden.iter().any(|f| low.contains(&f.to_lowercase()))
}
pub fn redact(input: &str, secrets: &[&str]) -> String {
    let mut out = input.to_string();
    for s in secrets { out = out.replace(s, "[REDACTED]"); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks() { assert!(!guard("rm -rf /", &["rm -rf"])); }
    #[test]
    fn allows() { assert!(guard("hello", &["rm -rf"])); }
    #[test]
    fn redacts() { assert_eq!(redact("token=abc", &["abc"]), "token=[REDACTED]"); }
}
