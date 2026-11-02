//! Cross-namespace boundary policy for graph reads.
//!
//! The `related` traversal is per-namespace by default: a `GOVERNS` edge to a
//! file in another project is worse than no edge at all. Callers may opt in to
//! crossing that boundary, but only when the home namespace's policy allows it
//! and the target namespace is explicitly listed (an empty allowlist with
//! `allow_cross_namespace` means "any namespace").
//!
//! Ids are stored either bare (`rule-1`) or qualified
//! (`<namespace>::<generation>::<slug>`), so the policy also owns the prefix
//! extraction the store uses to label a target before the hop.

use serde::{Deserialize, Serialize};

/// Who may cross a namespace boundary, and where they may go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossNamespacePolicy {
    /// When false, every cross-namespace hop is refused (the default).
    #[serde(default)]
    pub allow_cross_namespace: bool,
    /// Target namespaces explicitly permitted. Empty means all of them once
    /// `allow_cross_namespace` is true.
    #[serde(default)]
    pub allowed_namespaces: Vec<String>,
}

impl CrossNamespacePolicy {
    pub fn new(allow_cross_namespace: bool, allowed_namespaces: Vec<String>) -> Self {
        Self {
            allow_cross_namespace,
            allowed_namespaces,
        }
    }

    /// True when a hop from `home_ns` to `target_ns` is permitted. Same
    /// namespace is always allowed; a cross-namespace hop requires the opt-in
    /// and (when the allowlist is non-empty) an explicit entry.
    pub fn is_allowed(&self, home_ns: &str, target_ns: &str) -> bool {
        if home_ns == target_ns {
            return true;
        }
        if !self.allow_cross_namespace {
            return false;
        }
        self.allowed_namespaces.is_empty()
            || self
                .allowed_namespaces
                .iter()
                .any(|allowed| allowed == target_ns)
    }
}

/// Extract the leading namespace from a qualified id shaped
/// `<namespace>::<generation>::<slug>` (or `<namespace>::<slug>`). Returns
/// `None` for a bare id or an empty namespace prefix.
pub fn extract_namespace_prefix(id: &str) -> Option<&str> {
    let (namespace, rest) = id.split_once("::")?;
    let namespace = namespace.trim();
    if namespace.is_empty() || rest.trim().is_empty() {
        return None;
    }
    Some(namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_namespace_is_always_allowed() {
        let policy = CrossNamespacePolicy::default();
        assert!(policy.is_allowed("probe", "probe"));
    }

    #[test]
    fn cross_namespace_requires_opt_in() {
        let policy = CrossNamespacePolicy::default();
        assert!(!policy.is_allowed("probe", "other"));
    }

    #[test]
    fn empty_allowlist_permits_any_namespace_once_opted_in() {
        let policy = CrossNamespacePolicy::new(true, Vec::new());
        assert!(policy.is_allowed("probe", "other"));
    }

    #[test]
    fn non_empty_allowlist_is_exhaustive() {
        let policy = CrossNamespacePolicy::new(
            true,
            vec!["allowed".to_string(), "also".to_string()],
        );
        assert!(policy.is_allowed("probe", "allowed"));
        assert!(policy.is_allowed("probe", "also"));
        assert!(!policy.is_allowed("probe", "other"));
    }

    #[test]
    fn extracts_qualified_namespace_prefixes() {
        assert_eq!(
            extract_namespace_prefix("probe::1::src/lib.rs"),
            Some("probe")
        );
        assert_eq!(extract_namespace_prefix("probe::src/lib.rs"), Some("probe"));
        assert_eq!(extract_namespace_prefix("bare-id"), None);
        assert_eq!(extract_namespace_prefix("::1::x"), None);
        assert_eq!(extract_namespace_prefix("probe::"), None);
    }
}
