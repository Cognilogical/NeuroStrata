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

/// Detect conflict candidates among active memories (F10). Returns (id, disputed_with_ids).
/// Detect conflict candidates among active memories (F10). Returns (id, disputed_with_ids).
pub fn detect_conflicts(memories: &[crate::traits::SearchResult]) -> Vec<(String, Vec<String>)> {
    let now = chrono::Utc::now().timestamp();
    // Filter expired memories first (F10-6). Also skip archived/mesh-conflicting/registry
    // rows defensively; callers filter these too, but the function must not flag them.
    let active: Vec<&crate::traits::SearchResult> = memories
        .iter()
        .filter(|m| {
            if crate::registry::is_registry_row(&m.payload.metadata) { return false; }
            if crate::synaptic_mesh::conflict::is_conflicting(&m.payload) { return false; }
            if crate::handlers::archive_memory::is_archived(&m.payload) { return false; }
            match m.payload.metadata.get("valid_to") {
                None => true,
                Some(v) => v.is_null() || v.as_i64().unwrap_or(0) > now,
            }
        })
        .collect();

    let mut out = Vec::new();
    for i in 0..active.len() {
        let a = active[i];
        if a.payload.memory_type != "rule" && a.payload.memory_type != "fact" { continue; }
        let mut disputed_with = Vec::new();
        for j in 0..active.len() {
            if i == j { continue; }
            let b = active[j];
            if b.payload.memory_type != "rule" && b.payload.memory_type != "fact" { continue; }
            // same location or topology
            let same_loc = !a.payload.location.is_empty() && a.payload.location == b.payload.location;
            let topo_a = a.payload.metadata.get("topology");
            let topo_b = b.payload.metadata.get("topology");
            // Topology conflict (K2): same (component, target_system) anchor but
            // differing channel or credential_boundary (or opposing enforcement/negation).
            let (same_topo_anchor, topo_differs) = match (topo_a, topo_b) {
                (Some(ta), Some(tb)) => {
                    let same_anchor = ta.get("component") == tb.get("component")
                        && ta.get("target_system") == tb.get("target_system")
                        && (ta.get("component").is_some() || ta.get("target_system").is_some());
                    if !same_anchor { (false, false) } else {
                        let chan_diff = ta.get("channel") != tb.get("channel");
                        let cred_diff = ta.get("credential_boundary") != tb.get("credential_boundary");
                        (true, chan_diff || cred_diff)
                    }
                }
                _ => (false, false),
            };
            // High cosine (cosine distance <= 0.15). score is distance (lower better).
            let high_cosine = a.score <= 0.15 && b.score <= 0.15;

            if !same_loc && !same_topo_anchor && !high_cosine { continue; }

            // opposing enforcement: ENFORCED vs NOT_ENFORCED
            let enf_a = a.payload.metadata.get("enforcement").and_then(|v| v.as_str()).unwrap_or("");
            let enf_b = b.payload.metadata.get("enforcement").and_then(|v| v.as_str()).unwrap_or("");
            let opposing_enf = (enf_a == "ENFORCED" && enf_b == "NOT_ENFORCED")
                || (enf_a == "NOT_ENFORCED" && enf_b == "ENFORCED");

            // opposing decision status
            let dec_a = a.payload.metadata.get("decision").and_then(|d| d.get("status")).and_then(|v| v.as_str()).unwrap_or("");
            let dec_b = b.payload.metadata.get("decision").and_then(|d| d.get("status")).and_then(|v| v.as_str()).unwrap_or("");
            let b_failed = a.payload.metadata.get("attempt").and_then(|x| x.get("outcome")).and_then(|v| v.as_str()) == Some("failed")
                || b.payload.metadata.get("attempt").and_then(|x| x.get("outcome")).and_then(|v| v.as_str()) == Some("failed");
            // attempt outcome on the *other* row
            let a_attempt_failed = a.payload.metadata.get("attempt").and_then(|x| x.get("outcome")).and_then(|v| v.as_str()) == Some("failed");
            let b_attempt_failed = b.payload.metadata.get("attempt").and_then(|x| x.get("outcome")).and_then(|v| v.as_str()) == Some("failed");
            let opposing_dec = (dec_a == "accepted" && (dec_b == "withdrawn" || dec_b == "superseded" || b_attempt_failed))
                || (dec_b == "accepted" && (dec_a == "withdrawn" || dec_a == "superseded" || a_attempt_failed));

            // negation token opposition (word-boundary)
            let neg_tokens = ["not", "never", "no", "don't", "cannot", "must not"];
            let ca = a.payload.content.to_lowercase();
            let cb = b.payload.content.to_lowercase();
            let a_neg = contains_word_boundary(&ca, &neg_tokens);
            let b_neg = contains_word_boundary(&cb, &neg_tokens);
            let neg_opposition = a_neg != b_neg;

            let mut flag = false;
            // Topology facts: same anchor with differing channel/cred boundary -> conflict.
            if same_topo_anchor && topo_differs {
                flag = true;
            }
            if same_loc || same_topo_anchor {
                if opposing_enf || opposing_dec {
                    flag = true;
                }
                if neg_opposition && high_cosine {
                    flag = true;
                }
            }
            // Opposite polarity for rules governing same target (topology or location).
            if (same_loc || same_topo_anchor) && neg_opposition {
                // only flag if at least one is a rule
                if a.payload.memory_type == "rule" || b.payload.memory_type == "rule" {
                    flag = true;
                }
            }
            if high_cosine && neg_opposition {
                flag = true;
            }
            // avoid unused
            let _ = b_failed;
            if flag {
                disputed_with.push(b.id.clone());
            }
        }
        if !disputed_with.is_empty() {
            out.push((a.id.clone(), disputed_with));
        }
    }
    out
}

/// Word-boundary-aware negation token check (F10-5).
fn contains_word_boundary(text: &str, tokens: &[&str]) -> bool {
    for tok in tokens {
        // find all occurrences with boundary checks
        let mut start = 0;
        while let Some(pos) = text[start..].find(tok) {
            let abs = start + pos;
            let before_ok = abs == 0 || !text.as_bytes()[abs - 1].is_ascii_alphanumeric();
            let after = abs + tok.len();
            let after_ok = after == text.len() || !text.as_bytes()[after].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            start = abs + 1;
            if start >= text.len() { break; }
        }
    }
    false
}

/// Canonical slug for an entity alias (F17). Lowercase, strip non-alnum.
pub fn canonical_entity_slug(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() { out.push(c); }
    }
    out
}
