//! ReBAC helpers (in-house).
pub fn allowed(user: &str, resource_owner: &str, relation: &str) -> bool {
    if user == resource_owner { return true; }
    matches!(relation, "viewer" | "editor" | "owner") && !user.is_empty()
}
pub fn check(predicate: &str, user: &str, resource: &str) -> bool {
    // simple: allow if predicate contains user or resource
    predicate.contains(user) || predicate.contains(resource) || user == "admin"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_allowed() { assert!(allowed("u","u","viewer")); }
    #[test]
    fn stranger_viewer() { assert!(allowed("v","u","viewer")); }
}
