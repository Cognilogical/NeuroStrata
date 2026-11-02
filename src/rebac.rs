use serde_json::Value;

/// §5.2 — allowed (exact per spec, no blanket grants, owner/admin bypass, viewer ACL).
pub fn allowed(user: &str, resource_owner: &str, relation: &str, acl: &[String]) -> bool {
    // Plain string entries have no role information: they grant "viewer" only.
    let entries: Vec<(String, String)> = acl
        .iter()
        .map(|s| (s.clone(), "viewer".to_string()))
        .collect();
    allowed_with_roles(user, resource_owner, relation, &entries)
}

/// §5.2 — role-aware check over (principal, role) ACL entries. A `"*"` entry
/// matches any user. Entry roles ladder: owner ⊃ editor ⊃ viewer.
pub fn allowed_with_roles(
    user: &str,
    resource_owner: &str,
    relation: &str,
    acl: &[(String, String)],
) -> bool {
    if user.is_empty() {
        return false; // no anonymous access
    }
    if user == "admin" {
        return true; // admin bypass
    }
    if !resource_owner.is_empty() && check(resource_owner, user, "") {
        return true; // owner always allowed, via §5.2 exact-equality check
    }
    if acl.is_empty() {
        return relation == "viewer"; // unrestricted ACL grants read only
    }
    acl.iter().any(|(principal, role)| {
        (principal == "*" || principal == user)
            && matches!(
                (relation, role.as_str()),
                ("viewer", "viewer" | "editor" | "owner")
                    | ("editor", "editor" | "owner")
                    | ("owner", "owner")
            )
    })
}

/// §5.2 — check (exact equality only, no substring spoofing).
pub fn check(predicate: &str, user: &str, resource: &str) -> bool {
    if user.is_empty() {
        return false;
    }
    predicate == user || predicate == resource || user == "admin"
}

/// §5.3 — principal resolution; admin honors client claim, others ignored.
pub fn resolve_principal(authenticated_principal: &str, client_requested_principal: Option<&str>) -> String {
    if authenticated_principal == "admin" {
        client_requested_principal
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("admin")
            .to_string()
    } else {
        authenticated_principal.to_string()
    }
}

/// §5.6 / metadata helper — extract visible_to array as Vec<String>.
pub fn extract_visible_to(metadata: &Value) -> Vec<String> {
    metadata
        .get("visible_to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            let parsed: Vec<String> = arr
                .iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        Some(s.to_string())
                    } else if let Some(p) = v.get("principal").and_then(|p| p.as_str()) {
                        Some(p.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            // A non-empty array that parsed to nothing is malformed data, not
            // an unrestricted ACL: the empty Vec means public viewer access in
            // allowed_with_roles, so fail closed instead.
            if !arr.is_empty() && parsed.is_empty() {
                tracing::warn!("malformed non-empty visible_to ACL; failing closed");
                return vec!["__invalid_acl__".to_string()];
            }
            parsed
        })
        .unwrap_or_default()
}

/// §5.6 — extract visible_to as (principal, role) pairs. Plain string entries
/// default to role "viewer"; {principal, role} objects carry their role
/// (unknown/missing roles coerce to "viewer").
pub fn extract_acl_with_roles(metadata: &Value) -> Vec<(String, String)> {
    metadata
        .get("visible_to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            let parsed: Vec<(String, String)> = arr
                .iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        Some((s.to_string(), "viewer".to_string()))
                    } else if let Some(p) = v.get("principal").and_then(|p| p.as_str()) {
                        let role = v
                            .get("role")
                            .and_then(|r| r.as_str())
                            .filter(|r| matches!(*r, "viewer" | "editor" | "owner"))
                            .unwrap_or("viewer");
                        Some((p.to_string(), role.to_string()))
                    } else {
                        None
                    }
                })
                .collect();
            // Same fail-closed rule as extract_visible_to: every element of a
            // non-empty array being malformed must not look like an
            // unrestricted ACL.
            if !arr.is_empty() && parsed.is_empty() {
                tracing::warn!("malformed non-empty visible_to ACL; failing closed");
                return vec![("__invalid_acl__".to_string(), "viewer".to_string())];
            }
            parsed
        })
        .unwrap_or_default()
}

/// Validate `metadata.visible_to` shape for SHACL (`src/rdf/shacl.rs`).
pub fn validate_visible_to(vt: &Value) -> Vec<String> {
    let Some(arr) = vt.as_array() else {
        return vec!["visible_to must be an array".to_string()];
    };
    let mut errs = Vec::new();
    for (idx, item) in arr.iter().enumerate() {
        if let Some(s) = item.as_str() {
            if s.trim().is_empty() {
                errs.push(format!("visible_to[{idx}] must be a non-empty string"));
            }
        } else if let Some(obj) = item.as_object() {
            let p_ok = obj
                .get("principal")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !p_ok {
                errs.push(format!("visible_to[{idx}].principal must be a non-empty string"));
            }
            if let Some(role) = obj.get("role").and_then(|v| v.as_str()) {
                if !matches!(role, "viewer" | "editor" | "owner") {
                    errs.push(format!(
                        "visible_to[{idx}].role must be viewer, editor, or owner"
                    ));
                }
            }
        } else {
            errs.push(format!(
                "visible_to[{idx}] must be a principal string or {{principal, role}} object"
            ));
        }
    }
    errs
}

/// §5.1 / §5.6 — derive resource owner from metadata fallbacks.
pub fn extract_resource_owner(user_id: &str, metadata: &Value) -> String {
    if !user_id.is_empty() {
        return user_id.to_string();
    }
    if let Some(s) = metadata.get("user_id").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(s) = metadata.get("mesh").and_then(|v| v.get("created_by")).and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(s) = metadata.get("lifecycle").and_then(|v| v.get("created_by")).and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(s) = metadata.get("mesh").and_then(|v| v.get("verified_signer_did")).and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(s) = metadata.get("verified_signer_did").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    String::new()
}

/// §5.4 / §5.6 — read permission via allowed (viewer).
pub fn can_read_memory(principal: &str, user_id: &str, metadata: &Value) -> bool {
    allowed_with_roles(
        principal,
        &extract_resource_owner(user_id, metadata),
        "viewer",
        &extract_acl_with_roles(metadata),
    )
}

/// §5.4 / §5.6 — write permission via allowed (editor).
pub fn can_write_memory(principal: &str, user_id: &str, metadata: &Value) -> bool {
    allowed_with_roles(
        principal,
        &extract_resource_owner(user_id, metadata),
        "editor",
        &extract_acl_with_roles(metadata),
    )
}

/// §5.6 — restricted when visible_to non-empty and lacks wildcard.
pub fn is_restricted(metadata: &Value) -> bool {
    let acl = extract_visible_to(metadata);
    !acl.is_empty() && !acl.iter().any(|e| e == "*")
}

/// §5.6 — publish allowed when unrestricted, wildcard, or peer intersection.
pub fn can_publish_to_mesh(metadata: &Value, peer_principals: &[String]) -> bool {
    if !is_restricted(metadata) {
        return true; // empty ACL or wildcard: nothing restricted to carve back
    }
    let acl = extract_visible_to(metadata);
    acl.iter().any(|e| peer_principals.iter().any(|p| p == e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // §5.2 allowed — owner, stranger, wildcard, admin, empty user, unrestricted, restricted.
    #[test]
    fn allowed_owner_self() {
        assert!(allowed("alice", "alice", "viewer", &[]));
        assert!(allowed("alice", "alice", "editor", &[]));
    }

    #[test]
    fn allowed_stranger_restricted() {
        assert!(!allowed("bob", "alice", "viewer", &["alice".into()]));
        assert!(!allowed("bob", "alice", "editor", &["alice".into()]));
    }

    #[test]
    fn allowed_wildcard() {
        assert!(allowed("bob", "alice", "viewer", &["*".into()]));
        assert!(!allowed("bob", "alice", "editor", &["*".into()]));
    }

    #[test]
    fn allowed_admin_bypass() {
        assert!(allowed("admin", "alice", "viewer", &[]));
        assert!(allowed("admin", "alice", "editor", &[]));
    }

    #[test]
    fn allowed_empty_user_denied() {
        assert!(!allowed("", "alice", "viewer", &[]));
        assert!(!allowed("", "", "viewer", &[]));
    }

    #[test]
    fn allowed_unrestricted_empty_acl() {
        assert!(allowed("bob", "alice", "viewer", &[])); // owner handled above; stranger allowed when unrestricted
        assert!(allowed("charlie", "alice", "viewer", &[]));
    }

    #[test]
    fn allowed_restricted_acl_stranger_denied() {
        assert!(!allowed("bob", "alice", "viewer", &["alice".into(), "charlie".into()]));
    }

    // §5.2 check — exact match, no substring spoofing.
    #[test]
    fn check_exact_match() {
        assert!(check("alice", "alice", "resource"));
        assert!(check("resource", "alice", "resource"));
    }

    #[test]
    fn check_admin_bypass() {
        assert!(check("bob", "admin", "resource"));
    }

    #[test]
    fn check_negative_no_substring_spoof() {
        assert!(!check("alice", "malice", "resource")); // "alice" must NOT match "malice"
        assert!(!check("alice", "bob", "malice")); // exact only; predicate != resource when different
    }

    #[test]
    fn check_empty_user_denied() {
        assert!(!check("alice", "", "resource"));
    }

    // §5.3 resolve_principal — non-admin cannot spoof; admin can.
    #[test]
    fn resolve_principal_non_admin_ignored() {
        assert_eq!(resolve_principal("bob", Some("eve")), "bob");
        assert_eq!(resolve_principal("bob", None), "bob");
    }

    #[test]
    fn resolve_principal_admin_honors() {
        assert_eq!(resolve_principal("admin", Some("eve")), "eve");
        assert_eq!(resolve_principal("admin", Some("  eve  ")), "  eve  "); // filter trims empty only, not whitespace
        assert_eq!(resolve_principal("admin", Some("")), "admin");
        assert_eq!(resolve_principal("admin", None), "admin");
    }

    // §5.6 / §5.7 backward compatibility — visible_to absent → allowed.
    #[test]
    fn can_read_memory_backward_compat_no_visible_to() {
        let meta = serde_json::json!({"user_id":"alice"});
        assert!(can_read_memory("bob", "alice", &meta));
    }

    #[test]
    fn can_write_memory_backward_compat_no_visible_to() {
        let meta = serde_json::json!({"user_id":"alice"});
        assert!(!can_write_memory("bob", "alice", &meta)); // editor restricted to owner/admin
        assert!(can_write_memory("alice", "alice", &meta));
    }

    #[test]
    fn can_read_memory_restricted_stranger_denied() {
        let meta = serde_json::json!({"visible_to":["alice"],"user_id":"alice"});
        assert!(!can_read_memory("bob", "alice", &meta));
        assert!(can_read_memory("alice", "alice", &meta));
    }

    #[test]
    fn extract_visible_to_empty_and_array() {
        assert!(extract_visible_to(&serde_json::json!({})).is_empty());
        assert_eq!(extract_visible_to(&serde_json::json!({"visible_to":["a","b"]})), vec!["a","b"]);
    }

    // §5.6 role-aware ACLs — {principal, role} objects honor their role.
    #[test]
    fn extract_acl_with_roles_strings_default_viewer() {
        let entries = extract_acl_with_roles(
            &serde_json::json!({"visible_to":["bob", {"principal":"alice","role":"editor"}]}),
        );
        assert_eq!(
            entries,
            vec![
                ("bob".to_string(), "viewer".to_string()),
                ("alice".to_string(), "editor".to_string()),
            ]
        );
    }

    #[test]
    fn role_editor_entry_grants_editor_and_viewer_not_owner() {
        let meta = serde_json::json!({
            "user_id": "alice",
            "visible_to": [{ "principal": "bob", "role": "editor" }]
        });
        assert!(can_write_memory("bob", "alice", &meta), "editor entry grants editor");
        assert!(can_read_memory("bob", "alice", &meta), "editor entry grants viewer");
        assert!(
            !allowed_with_roles("bob", "alice", "owner", &extract_acl_with_roles(&meta)),
            "editor entry must NOT grant owner"
        );
    }

    #[test]
    fn wildcard_editor_entry_grants_editor_only() {
        let meta = serde_json::json!({
            "visible_to": [{ "principal": "*", "role": "editor" }]
        });
        assert!(allowed_with_roles("stranger", "alice", "editor", &extract_acl_with_roles(&meta)));
        assert!(allowed_with_roles("stranger", "alice", "viewer", &extract_acl_with_roles(&meta)));
        assert!(!allowed_with_roles("stranger", "alice", "owner", &extract_acl_with_roles(&meta)));
    }

    #[test]
    fn is_restricted_and_publish() {
        assert!(!is_restricted(&serde_json::json!({"visible_to":[]})));
        assert!(is_restricted(&serde_json::json!({"visible_to":["alice"]})));
        assert!(!is_restricted(&serde_json::json!({"visible_to":["*"]})));
        assert!(can_publish_to_mesh(&serde_json::json!({"visible_to":["alice"]}), &["alice".into()]));
        assert!(can_publish_to_mesh(&serde_json::json!({"visible_to":["*"]}), &[]));
    }

    // §5.6 fail-closed — a non-empty visible_to array whose every element is
    // malformed must NOT collapse to an unrestricted ACL (which
    // allowed_with_roles treats as public viewer access).
    #[test]
    fn malformed_non_empty_visible_to_fails_closed() {
        let meta = serde_json::json!({"user_id":"alice","visible_to":[42]});
        assert_eq!(extract_visible_to(&meta), vec!["__invalid_acl__"]);
        assert_eq!(
            extract_acl_with_roles(&meta),
            vec![("__invalid_acl__".to_string(), "viewer".to_string())]
        );
        assert!(!allowed("bob", "alice", "viewer", &extract_visible_to(&meta)));
        assert!(!can_read_memory("bob", "alice", &meta));
        // Malformed elements paired with a valid one still parse the valid one.
        let mixed = serde_json::json!({"user_id":"alice","visible_to":[42,"bob"]});
        assert!(can_read_memory("bob", "alice", &mixed));
        assert!(!can_read_memory("carol", "alice", &mixed));
    }
}
