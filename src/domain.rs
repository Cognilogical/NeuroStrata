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

    #[test]
    fn test_f16_compute_facets_and_predicates() {
        // Nested rule enforcement
        let meta = serde_json::json!({"enforcement": "ENFORCED"});
        assert!(compute_facets("rule", &meta).contains(&"enforced-rule".to_string()));
        // Flat rule_kind
        let meta2 = serde_json::json!({"rule_kind": "required"});
        assert!(compute_facets("rule", &meta2).contains(&"enforced-rule".to_string()));
        // Fact attempt failed nested
        let meta3 = serde_json::json!({"attempt": {"outcome": "failed"}});
        assert!(compute_facets("fact", &meta3).contains(&"failed-attempt".to_string()));
        // Flat attempt_status failed
        let meta4 = serde_json::json!({"attempt_status": "failed"});
        assert!(compute_facets("fact", &meta4).contains(&"failed-attempt".to_string()));
        // Fact decision nested
        let meta5 = serde_json::json!({"decision": {"status":"accepted"}});
        assert!(compute_facets("fact", &meta5).contains(&"architecture-decision".to_string()));
        // Flat decision_type
        let meta6 = serde_json::json!({"decision_type": "architecture"});
        assert!(compute_facets("fact", &meta6).contains(&"architecture-decision".to_string()));
        // Manual facets preserved
        let meta7 = serde_json::json!({"facets": ["manual-facet"]});
        assert!(compute_facets("rule", &meta7).contains(&"manual-facet".to_string()));
        assert!(compute_facets("rule", &meta7).contains(&"manual-facet".to_string()));
    }

    #[test]
    fn test_bitemporal_facet_materialization_and_retraction() {
        let mut meta = serde_json::json!({"enforcement": "ENFORCED"});
        materialize_facets_bitemporal("rule", &mut meta, 100);
        let facets = meta.get("facets").unwrap().as_array().unwrap();
        assert!(facets.iter().any(|v| v.as_str().unwrap() == "enforced-rule"));
        let bt = meta.get("facets_bitemporal").unwrap().as_array().unwrap();
        assert!(bt.len() == 1);
        let entry: serde_json::Value = bt[0].clone();
        assert_eq!(entry.get("facet").unwrap().as_str().unwrap(), "enforced-rule");
        assert_eq!(entry.get("inferred").unwrap().as_bool().unwrap(), true);
        assert!(entry.get("valid_to").unwrap().is_null());
        assert_eq!(entry.get("valid_from").unwrap().as_i64().unwrap(), 100);

        // Remove enforcement and re-run at now+10
        meta.as_object_mut().unwrap().remove("enforcement");
        materialize_facets_bitemporal("rule", &mut meta, 110);
        let bt2 = meta.get("facets_bitemporal").unwrap().as_array().unwrap();
        assert_eq!(bt2.len(), 1);
        let entry2: serde_json::Value = bt2[0].clone();
        assert_eq!(entry2.get("valid_to").unwrap().as_i64().unwrap(), 110);
        let facets2 = meta.get("facets").unwrap().as_array().unwrap();
        assert!(!facets2.iter().any(|v| v.as_str().unwrap() == "enforced-rule"));

        // Manual facets should remain
        meta.as_object_mut().unwrap().insert("facets".to_string(), serde_json::Value::Array(vec![serde_json::Value::String("manual".to_string())]));
        meta.as_object_mut().unwrap().remove("enforcement");
        materialize_facets_bitemporal("rule", &mut meta, 120);
        let facets3 = meta.get("facets").unwrap().as_array().unwrap();
        assert!(facets3.iter().any(|v| v.as_str().unwrap() == "manual"));

        // Retract inferred facets
        meta.as_object_mut().unwrap().insert("enforcement".to_string(), serde_json::Value::String("ENFORCED".to_string()));
        materialize_facets_bitemporal("rule", &mut meta, 125);
        retract_inferred_facets(&mut meta, 130);
        let bt3 = meta.get("facets_bitemporal").unwrap().as_array().unwrap();
        assert!(bt3.iter().all(|v| v.get("valid_to").unwrap().as_i64().is_some()));
        assert_eq!(bt3.last().unwrap().get("valid_to").unwrap().as_i64(), Some(130));
    }

    #[test]
    fn test_f10_detect_conflicts_contradictory_and_near_duplicate() {
        use crate::traits::{MemoryPayload, SearchResult};
        fn make(id: &str, mem_type: &str, content: &str, meta: serde_json::Value) -> SearchResult {
            SearchResult {
                id: id.to_string(),
                score: 1.0,
                payload: MemoryPayload {
                    content: content.to_string(),
                    user_id: "u".to_string(),
                    memory_type: mem_type.to_string(),
                    agent_name: None,
                    location: "test".to_string(),
                    location_lines: "1".to_string(),
                    metadata: meta,
                },
                evidence: None,
            }
        }
        // (a) Contradictory pair on same location
        let a = make("a1", "rule", "must use TLS", serde_json::json!({"location":"loc1","enforcement":"ENFORCED"}));
        let b = make("b1", "rule", "must not use TLS", serde_json::json!({"location":"loc1","enforcement":"NOT_ENFORCED"}));
        let conflicts = detect_conflicts(&[a, b]);
        assert!(!conflicts.is_empty());
        // (b) Near-duplicate non-conflict (same polarity, not contradictory)
        let c = make("c1", "rule", "always validate tokens", serde_json::json!({"location":"loc2","enforcement":"ENFORCED"}));
        let d = make("d1", "rule", "always validate tokens on ingress", serde_json::json!({"location":"loc2","enforcement":"ENFORCED"}));
        let conflicts2 = detect_conflicts(&[c, d]);
        assert!(conflicts2.is_empty());
        // (c) Word-boundary: node/note/notification/canonical don't trigger "no" or "not"
        let e = make("e1", "rule", "notification canonical", serde_json::json!({"location":"loc3"}));
        let f = make("f1", "rule", "node note notification", serde_json::json!({"location":"loc3"}));
        let conflicts3 = detect_conflicts(&[e, f]);
        assert!(conflicts3.is_empty());
        // (d) Expired and archived rows are ignored
        let g = make("g1", "rule", "old rule", serde_json::json!({"location":"loc4","valid_to":0,"archived":true}));
        let h = make("h1", "rule", "new rule", serde_json::json!({"location":"loc4","enforcement":"ENFORCED","valid_to":9999999999_i64}));
        let conflicts4 = detect_conflicts(&[g, h]);
        assert!(conflicts4.is_empty());
        // (e) Unrelated rules sharing broad file location but no topic overlap -> no conflict
        let mut r1 = make("r1", "rule", "Never use docker containers", serde_json::json!({"location":"AGENTS.md"}));
        r1.payload.location = "AGENTS.md".to_string();
        let mut r2 = make("r2", "rule", "Always use absolute file paths", serde_json::json!({"location":"AGENTS.md"}));
        r2.payload.location = "AGENTS.md".to_string();
        assert!(detect_conflicts(&[r1, r2]).is_empty());
        // Unrelated rules sharing broad topology anchor (component+target_system) but no topic overlap -> no conflict
        let u1 = make("u1", "rule", "Never expose internal tokens", serde_json::json!({"topology":{"component":"server","target_system":"mcp"}}));
        let u2 = make("u2", "rule", "Always validate JSON schema inputs", serde_json::json!({"topology":{"component":"server","target_system":"mcp"}}));
        assert!(detect_conflicts(&[u1, u2]).is_empty());
        // Same topic + topology anchor with opposing negation -> conflict
        let s1 = make("s1", "rule", "Never cache auth tokens in server", serde_json::json!({"topology":{"component":"server","target_system":"mcp"}}));
        let s2 = make("s2", "rule", "Always cache auth tokens in server", serde_json::json!({"topology":{"component":"server","target_system":"mcp"}}));
        assert!(!detect_conflicts(&[s1, s2]).is_empty());
    }

    #[test]
    fn test_f17_canonical_entity_slug() {
        assert_eq!(canonical_entity_slug("LadybugDB"), "ladybugdb");
        assert_eq!(canonical_entity_slug("ladybug-db"), "ladybugdb");
        assert_eq!(canonical_entity_slug("ladybug_db"), "ladybugdb");
        assert_eq!(canonical_entity_slug("lbug"), "lbug");
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
    // OWL/RDFS spec §11.7 metadata predicates
    if metadata.get("rule_kind").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false) {
        if !out.contains(&"enforced-rule".to_string()) { out.push("enforced-rule".to_string()); }
    }
    if metadata.get("attempt_status").and_then(|v| v.as_str()) == Some("failed") {
        if !out.contains(&"failed-attempt".to_string()) { out.push("failed-attempt".to_string()); }
    }
    if metadata.get("decision_type").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false) {
        if !out.contains(&"architecture-decision".to_string()) { out.push("architecture-decision".to_string()); }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InferredFacetEntry {
    pub facet: String,
    pub inferred: bool,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
}

/// Computes ONLY the rule/fact inferred facets (without copying metadata.facets).
pub fn inferred_only_facets(memory_type: &str, metadata: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if memory_type == "rule" {
        if metadata.get("enforcement").is_some()
            || metadata.get("rule_kind").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false)
        {
            out.push("enforced-rule".to_string());
        }
    }
    if memory_type == "fact" {
        if let Some(attempt) = metadata.get("attempt") {
            if attempt.get("outcome").and_then(|v| v.as_str()) == Some("failed") {
                out.push("failed-attempt".to_string());
            }
        }
        if metadata.get("attempt_status").and_then(|v| v.as_str()) == Some("failed") {
            out.push("failed-attempt".to_string());
        }
        if metadata.get("decision").is_some()
            || metadata.get("decision_type").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false)
        {
            out.push("architecture-decision".to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Materializes bi-temporal facets: incremental truth maintenance and retraction.
pub fn materialize_facets_bitemporal(memory_type: &str, metadata: &mut serde_json::Value, now: i64) {
    let entailed = inferred_only_facets(memory_type, metadata);

    // Read existing bitemporal entries
    let mut existing_entries: Vec<InferredFacetEntry> = if let Some(arr) = metadata.get("facets_bitemporal").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|v| {
            serde_json::from_value(v.clone()).ok()
        }).collect()
    } else {
        Vec::new()
    };

    // Retract active entries whose facet is no longer entailed
    let entailed_set: std::collections::BTreeSet<String> = entailed.iter().cloned().collect();
    for entry in &mut existing_entries {
        if entry.inferred && entry.valid_to.is_none() && !entailed_set.contains(&entry.facet) {
            entry.valid_to = Some(now);
        }
    }

    // Add new entries for newly entailed facets that don't have an active entry
    let active_facets: std::collections::BTreeSet<String> = existing_entries
        .iter()
        .filter(|e| e.inferred && e.valid_to.is_none())
        .map(|e| e.facet.clone())
        .collect();
    for f in &entailed {
        if !active_facets.contains(f) {
            existing_entries.push(InferredFacetEntry {
                facet: f.clone(),
                inferred: true,
                valid_from: now,
                valid_to: None,
            });
        }
    }

    // Rebuild active facets: manual (non-inferred) + currently active inferred
    let manual_facets: std::collections::BTreeSet<String> = if let Some(arr) = metadata.get("facets").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|v| v.as_str().map(String::from)).filter(|s| {
            // manual if never appeared as inferred true in bitemporal
            !existing_entries.iter().any(|e| e.inferred && e.facet == *s)
        }).collect()
    } else {
        std::collections::BTreeSet::new()
    };
    let active_entailed: std::collections::BTreeSet<String> = existing_entries
        .iter()
        .filter(|e| e.inferred && e.valid_to.is_none())
        .map(|e| e.facet.clone())
        .collect();

    let mut rebuilt: Vec<String> = manual_facets.union(&active_entailed).cloned().collect();
    rebuilt.sort();
    rebuilt.dedup();

    metadata.as_object_mut().unwrap().insert("facets".to_string(), serde_json::Value::Array(rebuilt.iter().map(|s| serde_json::Value::String(s.clone())).collect()));
    metadata.as_object_mut().unwrap().insert("facets_bitemporal".to_string(), serde_json::to_value(&existing_entries).unwrap());
}

/// Retracts all currently active inferred facets by setting their valid_to.
pub fn retract_inferred_facets(metadata: &mut serde_json::Value, valid_to: i64) {
    let mut entries: Vec<InferredFacetEntry> = if let Some(arr) = metadata.get("facets_bitemporal").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect()
    } else {
        Vec::new()
    };
    for entry in &mut entries {
        if entry.inferred && entry.valid_to.is_none() {
            entry.valid_to = Some(valid_to);
        }
    }
    // Rebuild active facets: manual + remaining active inferred
    let manual_facets: std::collections::BTreeSet<String> = if let Some(arr) = metadata.get("facets").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|v| v.as_str().map(String::from)).filter(|s| {
            !entries.iter().any(|e| e.inferred && e.facet == *s)
        }).collect()
    } else {
        std::collections::BTreeSet::new()
    };
    let active_inferred: std::collections::BTreeSet<String> = entries
        .iter()
        .filter(|e| e.inferred && e.valid_to.is_none())
        .map(|e| e.facet.clone())
        .collect();
    let mut rebuilt: Vec<String> = manual_facets.union(&active_inferred).cloned().collect();
    rebuilt.sort();
    rebuilt.dedup();
    metadata.as_object_mut().unwrap().insert("facets".to_string(), serde_json::Value::Array(rebuilt.iter().map(|s| serde_json::Value::String(s.clone())).collect()));
    metadata.as_object_mut().unwrap().insert("facets_bitemporal".to_string(), serde_json::to_value(&entries).unwrap());
}

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
            let loc_a = a.payload.location.trim();
            let loc_b = b.payload.location.trim();
            let valid_loc = !loc_a.is_empty()
                && !matches!(loc_a.to_ascii_lowercase().as_str(), "n/a" | "none" | "unknown" | "global");
            let same_loc = valid_loc && loc_a == loc_b;
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
            // Pairwise content similarity (replaces query-score proxy). High when
            // token Jaccard similarity >= 0.85 (i.e. cosine distance <= 0.15).
            let tokens = |s: &str| -> std::collections::BTreeSet<String> {
                s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_string()).collect()
            };
            let ta = tokens(&a.payload.content);
            let tb = tokens(&b.payload.content);
            let inter = ta.intersection(&tb).count();
            let union = ta.union(&tb).count().max(1);
            let sim = inter as f64 / union as f64;
            let high_cosine = sim >= 0.85;

            if !same_loc && !same_topo_anchor && !high_cosine { continue; }

            // opposing enforcement: ENFORCED vs NOT_ENFORCED
            let enf_a = a.payload.metadata.get("enforcement").and_then(|v| v.as_str()).unwrap_or("");
            let enf_b = b.payload.metadata.get("enforcement").and_then(|v| v.as_str()).unwrap_or("");
            let opposing_enf = (enf_a == "ENFORCED" && enf_b == "NOT_ENFORCED")
                || (enf_a == "NOT_ENFORCED" && enf_b == "ENFORCED");

            // opposing decision status
            let dec_a = a.payload.metadata.get("decision").and_then(|d| d.get("status")).and_then(|v| v.as_str()).unwrap_or("");
            let dec_b = b.payload.metadata.get("decision").and_then(|d| d.get("status")).and_then(|v| v.as_str()).unwrap_or("");
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
            // Opposite polarity for rules governing same target (topology or location with topic overlap).
            if (same_topo_anchor || same_loc) && sim >= 0.35 && neg_opposition {
                // only flag if at least one is a rule
                if a.payload.memory_type == "rule" || b.payload.memory_type == "rule" {
                    flag = true;
                }
            }
            if high_cosine && neg_opposition {
                flag = true;
            }
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
