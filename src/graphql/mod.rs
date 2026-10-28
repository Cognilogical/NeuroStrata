//! GraphQL facade (in-house): minimal query string echo.
pub fn execute(query: &str) -> String { format!("{{\"query\":\"{}\"}}", query.replace('"',"\\\"")) }
#[cfg(test)] mod tests { use super::*; #[test] fn echo() { assert!(execute("{x}").contains("x")); } }
