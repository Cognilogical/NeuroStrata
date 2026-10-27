/// Normalize a raw domain or tag into a lowercase-kebab slug token.
/// Returns None when nothing usable remains.
use serde_json::Value;

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

/// The normalized slug of a metadata object's `domain` field.
/// Returns None when the field is absent, is not a string, or does not normalize.
pub fn domain_key(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get("domain")
        .and_then(|value| value.as_str())
        .and_then(normalize_domain_slug)
}

/// All normalized slugs for a metadata object's `domain` field, which may be
/// a string or an array of strings (F9).
pub fn domain_keys(metadata: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match metadata.get("domain") {
        Some(Value::String(s)) => {
            if let Some(k) = normalize_domain_slug(s) { out.push(k); }
        }
        Some(Value::Array(arr)) => {
            for v in arr {
                if let Some(s) = v.as_str() {
                    if let Some(k) = normalize_domain_slug(s) {
                        if !out.contains(&k) { out.push(k); }
                    }
                }
            }
        }
        _ => {}
    }
    out
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
}

/// Compute epistemic facets from a memory's metadata (F16).
pub fn compute_facets(memory_type: &str, metadata: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if memory_type == "rule" {
        if metadata.get("enforcement").is_some() {
            out.push("enforced-rule".to_string());
        }
    }
    if memory_type == "fact" {
        if let Some(attempt) = metadata.get("attempt") {
            if attempt.get("outcome").and_then(|v| v.as_str()) == Some("failed") {
                out.push("failed-attempt".to_string());
            }
        }
        if metadata.get("decision").is_some() {
            out.push("architecture-decision".to_string());
        }
    }
    // preserve existing facets if present
    if let Some(existing) = metadata.get("facets").and_then(|v| v.as_array()) {
        for f in existing {
            if let Some(s) = f.as_str() {
                if !out.contains(&s.to_string()) { out.push(s.to_string()); }
            }
        }
    }
    out
}
