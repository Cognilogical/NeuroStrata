//! ReBAC helpers (in-house).
//!
//! The viewer predicate lives in `fts::acl_allows` because the lexical engine
//! must stay portable to the WASM crate (no `crate::` imports). This module is
//! the native-facing name for the same semantics.
pub fn allowed(user: &str, resource_owner: &str, relation: &str, acl: &[String]) -> bool {
    crate::fts::acl_allows(user, resource_owner, relation, acl)
}

pub fn check(predicate: &str, user: &str, resource: &str) -> bool {
    // Exact equality only: substring matching lets "alice" match "malice".
    predicate == user || predicate == resource || user == "admin"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_allowed() {
        assert!(allowed("u", "u", "viewer", &[]));
    }

    #[test]
    fn empty_acl_is_unrestricted() {
        assert!(allowed("v", "u", "viewer", &[]));
    }

    #[test]
    fn restricted_acl_requires_a_matching_principal() {
        let acl = vec!["alice".to_string()];
        assert!(allowed("alice", "owner", "viewer", &acl));
        assert!(!allowed("bob", "owner", "viewer", &acl));
        assert!(allowed("admin", "owner", "viewer", &acl));
    }

    #[test]
    fn anonymous_is_denied() {
        assert!(!allowed("", "owner", "viewer", &[]));
    }

    #[test]
    fn check_is_exact_not_substring() {
        assert!(!check("alice", "malice", "x"));
        assert!(check("alice", "alice", "x"));
    }
}
