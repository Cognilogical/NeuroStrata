/// Normalize a raw domain or tag into a lowercase-kebab slug token.
/// Returns None when nothing usable remains.
pub fn normalize_domain_slug(raw: &str) -> Option<String> {
    let mut slug = String::with_capacity(raw.len());
    let mut separator_pending = false;
    for ch in raw.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            // A separator is only written once an alphanumeric follows it, so
            // leading and trailing runs never reach the slug.
            if separator_pending && !slug.is_empty() {
                slug.push('-');
            }
            separator_pending = false;
            slug.push(ch);
        } else {
            separator_pending = true;
        }
    }
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// The one namespace validator (Decisions Register DR-1).
///
/// A namespace is 1..=64 characters, drawn from `[A-Za-z0-9._-]`, and does
/// not start with `.`. Every entry point that used to hand-roll its own
/// `/`+`\` check calls this instead, so a name refused at one surface is
/// refused at all of them. The register value is a mesh-interop surface:
/// peers' namespaces must fit it, not just today's fixtures — it was
/// confirmed against the fixture sweep and the live-store shape inventory
/// before it hardened (Task 8 Step 1). The error names the offending value
/// and the rule it violated.
///
/// Created by Task 7 on this branch; Task 8 creates the identical pair on
/// its branch. Whichever lands second adopts the other's unchanged — one
/// home, one definition.
pub fn validate_namespace(ns: &str) -> Result<(), String> {
    let len = ns.chars().count();
    if len == 0 || len > 64 {
        return Err(format!(
            "ERROR [NAMESPACE]: '{ns}' is not a valid namespace: \
             length must be 1..=64 characters (got {len})"
        ));
    }
    if ns.starts_with('.') {
        return Err(format!(
            "ERROR [NAMESPACE]: '{ns}' is not a valid namespace: \
             must not start with '.'"
        ));
    }
    if let Some(c) = ns
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "ERROR [NAMESPACE]: '{ns}' is not a valid namespace: \
             character '{c}' is not allowed (allowed: ASCII letters, digits, '.', '_', '-')"
        ));
    }
    Ok(())
}

/// The one id validator (Decisions Register DR-2, as amended).
///
/// A bare id is 1..=128 characters, drawn from `[A-Za-z0-9._:-]` — the
/// colon is legal here (namespace-qualified ids) though it is not legal in
/// a namespace. Confirmed against the same Step 1 sweeps as DR-1.
///
/// Amendment (ratified by Pratik, 2026-10-24): DR-2 governs bare ids. The
/// ingester mints qualified ids of the form `<namespace>::<path>`, whose
/// path half legitimately carries `/` and further `::` separators. A
/// qualified id is therefore validated compositely — the prefix must pass
/// DR-1 strictly, and the path remainder must be non-empty and free of
/// quotes, backslashes, and control characters (the characters that would
/// break out of a Cypher string literal or corrupt a log line). The
/// charset was not widened; the exemption is this named branch, and it is
/// test-pinned. The error names the offending value and the rule it
/// violated.
pub fn validate_id(id: &str) -> Result<(), String> {
    if let Some((prefix, remainder)) = id.split_once("::") {
        if let Err(rule) = validate_namespace(prefix) {
            return Err(format!(
                "ERROR [ID]: '{id}' is not a valid id: \
                 its qualified prefix is not a valid namespace ({rule})"
            ));
        }
        if remainder.is_empty() {
            return Err(format!(
                "ERROR [ID]: '{id}' is not a valid id: \
                 a qualified id's path remainder must not be empty"
            ));
        }
        if let Some(c) = remainder
            .chars()
            .find(|c| matches!(c, '\'' | '"' | '\\') || c.is_control())
        {
            return Err(format!(
                "ERROR [ID]: '{id}' is not a valid id: \
                 character {c:?} is not allowed in a qualified id's path remainder \
                 (no quotes, backslashes, or control characters)"
            ));
        }
        return Ok(());
    }
    let len = id.chars().count();
    if len == 0 || len > 128 {
        return Err(format!(
            "ERROR [ID]: '{id}' is not a valid id: \
             length must be 1..=128 characters (got {len})"
        ));
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':')))
    {
        return Err(format!(
            "ERROR [ID]: '{id}' is not a valid id: \
             character '{c}' is not allowed (allowed: ASCII letters, digits, '.', '_', '-', ':')"
        ));
    }
    Ok(())
}

/// The normalized slug of a metadata object's `domain` field.
/// Returns None when the field is absent, is not a string, or does not normalize.
pub fn domain_key(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get("domain")
        .and_then(|value| value.as_str())
        .and_then(normalize_domain_slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_normalizes_to_a_lowercase_kebab_slug() {
        assert_eq!(
            normalize_domain_slug("Database Access").as_deref(),
            Some("database-access")
        );
        assert_eq!(
            normalize_domain_slug("database_access").as_deref(),
            Some("database-access")
        );
        assert_eq!(
            normalize_domain_slug("database-access").as_deref(),
            Some("database-access")
        );
        assert_eq!(normalize_domain_slug("  Auth  ").as_deref(), Some("auth"));
        assert_eq!(
            normalize_domain_slug("eu-west-1 VPC").as_deref(),
            Some("eu-west-1-vpc")
        );
        assert_eq!(normalize_domain_slug("mTLS").as_deref(), Some("mtls"));
    }

    #[test]
    fn a_domain_that_normalizes_to_nothing_is_none() {
        assert_eq!(normalize_domain_slug("!!!").as_deref(), None);
        assert_eq!(normalize_domain_slug("   ").as_deref(), None);
        assert_eq!(normalize_domain_slug("").as_deref(), None);
    }

    #[test]
    fn a_non_string_domain_is_never_guessed() {
        assert_eq!(domain_key(&serde_json::json!({})), None);
        assert_eq!(domain_key(&serde_json::json!({"domain": 42})), None);
        assert_eq!(
            domain_key(&serde_json::json!({"domain": {"slug": "auth"}})),
            None
        );
        assert_eq!(
            domain_key(&serde_json::json!({"domain": "Database Access"})).as_deref(),
            Some("database-access")
        );
    }

    // ── DR-1 / DR-2: the one namespace / id validator ─────────────────────

    #[test]
    fn valid_namespaces_accepted() {
        // Every namespace literal used by the tree's fixtures/tests (Task 8
        // Step 1 fixture sweep) and every namespace present in the live
        // store (Step 1 shape inventory: dap, global, guard) stays legal.
        for ns in [
            "global",
            "dap",
            "guard",
            "MyProj",
            "NewProj",
            "NeuroStrata",
            "proj",
            "webproj",
            "alpha",
            "beta",
            "bus-test",
            "probe",
            "probe-counts",
            "promo-ns",
            "single-domain-ns",
            "a",
            "0",
            "a.b-c_d",
        ] {
            assert!(validate_namespace(ns).is_ok(), "{ns}");
        }
        // Boundary: exactly 64 characters is legal under DR-1.
        assert!(validate_namespace(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn invalid_namespaces_rejected() {
        for ns in [
            "", ".abc", ".", "a b", "a/b", "a\\b", "a'b", "a\"b", "café", "a:b", "a,b",
        ] {
            assert!(validate_namespace(ns).is_err(), "{ns:?} must be refused");
        }
        // Boundary: 65 characters is over the DR-1 length cap.
        assert!(validate_namespace(&"a".repeat(65)).is_err());
        // The refusal names the offending value.
        let err = validate_namespace("a/b").unwrap_err();
        assert!(err.contains("a/b"), "{err}");
    }

    #[test]
    fn valid_ids_accepted() {
        // Live-store ids are UUIDs; fixture ids include beads-style and
        // colon-qualified shapes (Task 8 Step 1 sweeps).
        for id in [
            "b997f5bf-9036-46e6-9860-c0cd345fe09b",
            "strata-1",
            "ms-1",
            "proc-live",
            "myproj-zzzz",
            "dap-jo9.1.1",
            "ns:task-1",
            "a:b:c",
            "x",
        ] {
            assert!(validate_id(id).is_ok(), "{id}");
        }
        // Boundary: exactly 128 characters is legal under DR-2.
        assert!(validate_id(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn invalid_ids_rejected() {
        for id in ["", "a b", "a'b", "a\"b", "a/b", "a\\b", "café"] {
            assert!(validate_id(id).is_err(), "{id:?} must be refused");
        }
        // Boundary: 129 characters is over the DR-2 length cap.
        assert!(validate_id(&"a".repeat(129)).is_err());
        // The refusal names the offending value.
        let err = validate_id("a b").unwrap_err();
        assert!(err.contains("a b"), "{err}");
    }

    #[test]
    fn qualified_ids_validate_compositely() {
        // The ingester's minted shape (DR-2 amendment, ratified 2026-10-24):
        // the path half carries `/` and further `::` separators legally.
        for id in ["probe::src/a.rs", "probe::src/a.rs::run", "a::b"] {
            assert!(validate_id(id).is_ok(), "{id}");
        }
        // The prefix takes DR-1 strictly, and the remainder refuses the
        // literal-breaking characters — every refusal in the id voice.
        for id in [
            "bad ns::src/a.rs",
            "::src/a.rs",
            "probe::",
            "probe::it's.rs",
            "probe::a\\b.rs",
            "probe::a\tb.rs",
        ] {
            let err = validate_id(id).unwrap_err();
            assert!(err.contains("ERROR [ID]"), "{id:?}: {err}");
        }
    }
}
