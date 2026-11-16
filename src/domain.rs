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

    /// The all-pairs scan detect_conflicts used before it precomputed per-row
    /// data, kept verbatim as the reference the current version must match.
    fn detect_conflicts_reference(memories: &[crate::traits::SearchResult]) -> Vec<(String, Vec<String>)> {
        let now = chrono::Utc::now().timestamp();
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
                let loc_a = a.payload.location.trim();
                let loc_b = b.payload.location.trim();
                let valid_loc = !loc_a.is_empty()
                    && !matches!(loc_a.to_ascii_lowercase().as_str(), "n/a" | "none" | "unknown" | "global");
                let same_loc = valid_loc && loc_a == loc_b;
                let topo_a = a.payload.metadata.get("topology");
                let topo_b = b.payload.metadata.get("topology");
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

                let enf_a = a.payload.metadata.get("enforcement").and_then(|v| v.as_str()).unwrap_or("");
                let enf_b = b.payload.metadata.get("enforcement").and_then(|v| v.as_str()).unwrap_or("");
                let opposing_enf = (enf_a == "ENFORCED" && enf_b == "NOT_ENFORCED")
                    || (enf_a == "NOT_ENFORCED" && enf_b == "ENFORCED");

                let dec_a = a.payload.metadata.get("decision").and_then(|d| d.get("status")).and_then(|v| v.as_str()).unwrap_or("");
                let dec_b = b.payload.metadata.get("decision").and_then(|d| d.get("status")).and_then(|v| v.as_str()).unwrap_or("");
                let a_attempt_failed = a.payload.metadata.get("attempt").and_then(|x| x.get("outcome")).and_then(|v| v.as_str()) == Some("failed");
                let b_attempt_failed = b.payload.metadata.get("attempt").and_then(|x| x.get("outcome")).and_then(|v| v.as_str()) == Some("failed");
                let opposing_dec = (dec_a == "accepted" && (dec_b == "withdrawn" || dec_b == "superseded" || b_attempt_failed))
                    || (dec_b == "accepted" && (dec_a == "withdrawn" || dec_a == "superseded" || a_attempt_failed));

                let neg_tokens = ["not", "never", "no", "don't", "cannot", "must not"];
                let ca = a.payload.content.to_lowercase();
                let cb = b.payload.content.to_lowercase();
                let a_neg = contains_word_boundary(&ca, &neg_tokens);
                let b_neg = contains_word_boundary(&cb, &neg_tokens);
                let neg_opposition = a_neg != b_neg;

                let mut flag = false;
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
                if (same_topo_anchor || same_loc) && sim >= 0.35 && neg_opposition {
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

    /// Deterministic xorshift generator, so a failing fixture can be replayed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<'v, T>(&mut self, items: &'v [T]) -> &'v T {
            &items[self.below(items.len())]
        }
    }

    fn row(id: String, mem_type: &str, location: &str, content: String, metadata: Value) -> crate::traits::SearchResult {
        crate::traits::SearchResult {
            id,
            score: 1.0,
            payload: crate::traits::MemoryPayload {
                content,
                user_id: "u".to_string(),
                memory_type: mem_type.to_string(),
                agent_name: None,
                location: location.to_string(),
                location_lines: "1".to_string(),
                metadata,
            },
            evidence: None,
        }
    }

    /// Rows that reach every branch of the pair check: shared and unusable
    /// locations, every topology shape, opposing enforcement and decisions,
    /// negation words and look-alikes, near-duplicate contents, non-ASCII
    /// text, duplicate ids, and rows the filters must drop.
    fn varied_fixture(seed: u64, n: usize) -> Vec<crate::traits::SearchResult> {
        let mut rng = Rng(seed);
        let types = ["rule", "rule", "rule", "fact", "fact", "task", "procedure", "directory"];
        let locations = [
            "src/a.rs", " src/a.rs ", "src/b.rs", "AGENTS.md", "", "  ", "N/A", "none", "Unknown",
            "GLOBAL", "global ", "test",
        ];
        let topologies = [
            Value::Null,
            serde_json::json!("server"),
            serde_json::json!({}),
            serde_json::json!({"component": "server"}),
            serde_json::json!({"component": "server", "target_system": "mcp"}),
            serde_json::json!({"component": "server", "target_system": "mcp", "channel": "http"}),
            serde_json::json!({"component": "server", "target_system": "mcp", "channel": "stdio"}),
            serde_json::json!({"component": "server", "target_system": "mcp", "credential_boundary": "user"}),
            serde_json::json!({"target_system": "mcp"}),
            serde_json::json!({"component": 1}),
            serde_json::json!({"component": 1.0}),
            serde_json::json!({"channel": "http"}),
        ];
        let templates = [
            "must use TLS for every outbound connection",
            "always validate tokens on ingress before the handler runs",
            "cache auth tokens in the server process for five minutes",
            "use absolute file paths in every tool call",
            "run containers with podman and podman-compose",
            "the node note notification canonical form is kebab case",
            "Café ÜBER straße İstanbul x86_64 schema inputs",
            "json schema inputs are checked at the MCP boundary",
            "store embeddings next to the row they describe",
            "a",
            "",
        ];
        let negations = ["not", "never", "no", "don't", "cannot", "must not", "Never", "NOT", "nothing", "notation"];
        let extras = ["really", "strictly", "tokens", "server", "mcp", "always", "now", "café", "x86"];
        (0..n)
            .map(|k| {
                let id = if rng.below(25) == 0 { "dup".to_string() } else { format!("r{k}") };
                let mem_type = *rng.pick(&types);
                let location = *rng.pick(&locations);
                let mut words: Vec<String> =
                    rng.pick(&templates).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
                match rng.below(6) {
                    0 => words.insert(rng.below(words.len() + 1), rng.pick(&negations).to_string()),
                    1 if !words.is_empty() => {
                        words.remove(rng.below(words.len()));
                    }
                    2 => words.push(rng.pick(&extras).to_string()),
                    3 => words.iter_mut().for_each(|w| *w = w.to_uppercase()),
                    _ => {}
                }
                let content = words.join(if rng.below(3) == 0 { ", " } else { " " });
                let mut meta = serde_json::Map::new();
                if rng.below(3) == 0 {
                    meta.insert("topology".into(), rng.pick(&topologies).clone());
                }
                match rng.below(5) {
                    0 => { meta.insert("enforcement".into(), "ENFORCED".into()); }
                    1 => { meta.insert("enforcement".into(), "NOT_ENFORCED".into()); }
                    2 => { meta.insert("enforcement".into(), 5.into()); }
                    _ => {}
                }
                match rng.below(7) {
                    0 => { meta.insert("decision".into(), serde_json::json!({"status": "accepted"})); }
                    1 => { meta.insert("decision".into(), serde_json::json!({"status": "withdrawn"})); }
                    2 => { meta.insert("decision".into(), serde_json::json!({"status": "superseded"})); }
                    3 => { meta.insert("decision".into(), "accepted".into()); }
                    _ => {}
                }
                match rng.below(5) {
                    0 => { meta.insert("attempt".into(), serde_json::json!({"outcome": "failed"})); }
                    1 => { meta.insert("attempt".into(), serde_json::json!({"outcome": "ok"})); }
                    _ => {}
                }
                match rng.below(20) {
                    0 => { meta.insert("valid_to".into(), 0.into()); }
                    1 => { meta.insert("valid_to".into(), Value::Null); }
                    2 => { meta.insert("valid_to".into(), 9_999_999_999_i64.into()); }
                    3 => { meta.insert("valid_to".into(), "soon".into()); }
                    4 => { meta.insert("archived".into(), true.into()); }
                    5 => { meta.insert("archived".into(), "true".into()); }
                    6 => { meta.insert("mesh".into(), serde_json::json!({"conflict": true})); }
                    7 => { meta.insert("domain_registry".into(), serde_json::json!({"domains": []})); }
                    _ => {}
                }
                let metadata = if rng.below(40) == 0 { serde_json::json!("not an object") } else { Value::Object(meta) };
                row(id, mem_type, location, content, metadata)
            })
            .collect()
    }

    #[test]
    fn detect_conflicts_matches_the_all_pairs_reference() {
        let mut flagged = 0;
        for seed in [1_u64, 7, 42, 2026, 0x5eed_cafe] {
            let rows = varied_fixture(seed, 220);
            let expected = detect_conflicts_reference(&rows);
            assert_eq!(detect_conflicts(&rows), expected, "seed {seed}");
            flagged += expected.len();
        }
        // The fixture is only evidence if it actually produces conflicts.
        assert!(flagged > 50, "fixture flagged only {flagged} rows");
    }

    /// A namespace shaped like the one that took ~40 s per get_snapshot:
    /// ~830 rule rows of ~1,400 characters, most anchored to a file location.
    fn large_rule_set() -> Vec<crate::traits::SearchResult> {
        let mut rng = Rng(0x00c0_ffee);
        let vocabulary: Vec<String> = (0..3000).map(|k| format!("w{k}")).collect();
        let negations = ["not", "never", "no", "don't", "cannot"];
        let mut bases: Vec<Vec<String>> = Vec::new();
        (0..830)
            .map(|k| {
                // About one row in eight restates an earlier one with a small change.
                let mut words: Vec<String> = if !bases.is_empty() && rng.below(8) == 0 {
                    let mut w = bases[rng.below(bases.len())].clone();
                    let at = rng.below(w.len());
                    w[at] = rng.pick(&negations).to_string();
                    w
                } else {
                    (0..230).map(|_| rng.pick(&vocabulary).clone()).collect()
                };
                if rng.below(4) == 0 {
                    let at = rng.below(words.len());
                    words.insert(at, rng.pick(&negations).to_string());
                }
                bases.push(words.clone());
                let location = if rng.below(10) < 7 { format!("src/module_{}.rs", rng.below(40)) } else { String::new() };
                let metadata = if rng.below(10) == 0 {
                    serde_json::json!({"topology": {"component": format!("c{}", rng.below(5)), "target_system": "mcp"}})
                } else {
                    serde_json::json!({})
                };
                row(format!("rule-{k}"), "rule", &location, words.join(" "), metadata)
            })
            .collect()
    }

    #[test]
    fn detect_conflicts_stays_fast_on_a_large_rule_set() {
        let rows = large_rule_set();
        let started = std::time::Instant::now();
        let conflicts = detect_conflicts(&rows);
        let elapsed = started.elapsed();
        eprintln!("detect_conflicts: {} rows, {} flagged, {elapsed:?}", rows.len(), conflicts.len());
        // Generous on purpose: an unoptimized debug build must pass on a slow
        // runner. The per-pair re-tokenizing version took minutes here.
        assert!(elapsed < std::time::Duration::from_secs(30), "took {elapsed:?}");
    }

    /// Prints the before/after timing on the large set. Ignored by default
    /// because the reference alone takes tens of seconds:
    /// `cargo test --release --bin neurostrata-mcp -- --ignored detect_conflicts_speedup`
    #[test]
    #[ignore]
    fn detect_conflicts_speedup_against_the_reference() {
        let rows = large_rule_set();
        let started = std::time::Instant::now();
        let current = detect_conflicts(&rows);
        let current_time = started.elapsed();
        let started = std::time::Instant::now();
        let reference = detect_conflicts_reference(&rows);
        let reference_time = started.elapsed();
        eprintln!(
            "detect_conflicts on {} rows ({} flagged): reference {reference_time:?}, current {current_time:?}",
            rows.len(),
            current.len()
        );
        assert_eq!(current, reference);
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

    // Read existing bitemporal entries, skipping (and logging) malformed ones
    // so one bad entry cannot shadow the well-formed history.
    let mut existing_entries: Vec<InferredFacetEntry> = if let Some(arr) = metadata.get("facets_bitemporal").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|v| {
            match serde_json::from_value::<InferredFacetEntry>(v.clone()) {
                Ok(entry) => Some(entry),
                Err(e) => {
                    tracing::warn!("skipping malformed facets_bitemporal entry: {e}");
                    None
                }
            }
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

    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("facets".to_string(), serde_json::Value::Array(rebuilt.iter().map(|s| serde_json::Value::String(s.clone())).collect()));
        obj.insert("facets_bitemporal".to_string(), serde_json::to_value(&existing_entries).unwrap());
    }
    // else: metadata is not an object — never panic on caller-provided values.
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
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("facets".to_string(), serde_json::Value::Array(rebuilt.iter().map(|s| serde_json::Value::String(s.clone())).collect()));
        obj.insert("facets_bitemporal".to_string(), serde_json::to_value(&entries).unwrap());
    }
    // else: metadata is not an object — never panic on caller-provided values.
}

/// Negation words whose presence on one side only marks opposite polarity (F10-5).
const NEGATION_TOKENS: [&str; 6] = ["not", "never", "no", "don't", "cannot", "must not"];

/// Everything the pair check reads from one rule/fact row, computed once per
/// row so the pair loop does no tokenizing or lower-casing.
struct ConflictRow<'a> {
    id: &'a str,
    is_rule: bool,
    /// Trimmed location, or None when it cannot anchor a conflict.
    location: Option<&'a str>,
    /// (component, target_system), or None without topology or without either key.
    topo_anchor: Option<(Option<&'a Value>, Option<&'a Value>)>,
    topo_channel: Option<&'a Value>,
    topo_credential_boundary: Option<&'a Value>,
    enforcement: &'a str,
    decision_status: &'a str,
    attempt_failed: bool,
    negated: bool,
    /// Distinct content tokens as sorted interned ids.
    tokens: Vec<u32>,
}

impl<'a> ConflictRow<'a> {
    fn new(m: &'a crate::traits::SearchResult, interner: &mut std::collections::HashMap<String, u32>) -> Self {
        let meta = &m.payload.metadata;
        let loc = m.payload.location.trim();
        let location = (!loc.is_empty()
            && !matches!(loc.to_ascii_lowercase().as_str(), "n/a" | "none" | "unknown" | "global"))
        .then_some(loc);
        let topology = meta.get("topology");
        let topo_get = |key: &str| topology.and_then(|t| t.get(key));
        let (component, target_system) = (topo_get("component"), topo_get("target_system"));
        let topo_anchor = (topology.is_some() && (component.is_some() || target_system.is_some()))
            .then_some((component, target_system));
        let content = m.payload.content.to_lowercase();
        let mut tokens: Vec<u32> = content
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| match interner.get(t) {
                Some(&token_id) => token_id,
                None => {
                    let token_id = interner.len() as u32;
                    interner.insert(t.to_string(), token_id);
                    token_id
                }
            })
            .collect();
        tokens.sort_unstable();
        tokens.dedup();
        ConflictRow {
            id: &m.id,
            is_rule: m.payload.memory_type == "rule",
            location,
            topo_anchor,
            topo_channel: topo_get("channel"),
            topo_credential_boundary: topo_get("credential_boundary"),
            enforcement: meta.get("enforcement").and_then(|v| v.as_str()).unwrap_or(""),
            decision_status: meta
                .get("decision")
                .and_then(|d| d.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            attempt_failed: meta
                .get("attempt")
                .and_then(|x| x.get("outcome"))
                .and_then(|v| v.as_str())
                == Some("failed"),
            negated: contains_word_boundary(&content, &NEGATION_TOKENS),
            tokens,
        }
    }
}

/// Token Jaccard similarity of two sorted, deduplicated token lists, or None
/// as soon as it is certain to be below `floor`.
///
/// The cut is exact: `upper` never undercounts the intersection, the union is
/// at least `|a| + |b| - upper`, and f64 division is monotonic, so the
/// computed bound is never below the computed similarity.
fn token_jaccard_at_least(a: &[u32], b: &[u32], floor: f64) -> Option<f64> {
    let below_floor = |upper: usize| (upper as f64 / (a.len() + b.len() - upper).max(1) as f64) < floor;
    let (mut i, mut j, mut inter) = (0, 0, 0usize);
    let (mut missing_a, mut missing_b) = (0usize, 0usize);
    if below_floor(a.len().min(b.len())) {
        return None;
    }
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                missing_a += 1;
                i += 1;
                if below_floor((a.len() - missing_a).min(b.len() - missing_b)) {
                    return None;
                }
            }
            std::cmp::Ordering::Greater => {
                missing_b += 1;
                j += 1;
                if below_floor((a.len() - missing_a).min(b.len() - missing_b)) {
                    return None;
                }
            }
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = (a.len() + b.len() - inter).max(1);
    Some(inter as f64 / union as f64)
}

/// Whether one pair of rows is a conflict candidate. Symmetric in (a, b).
fn conflict_pair(a: &ConflictRow, b: &ConflictRow) -> bool {
    // same location or topology
    let same_loc = matches!((a.location, b.location), (Some(x), Some(y)) if x == y);
    // Topology conflict (K2): same (component, target_system) anchor but
    // differing channel or credential_boundary (or opposing enforcement/negation).
    let same_topo_anchor = matches!((&a.topo_anchor, &b.topo_anchor), (Some(x), Some(y)) if x == y);
    let shared_target = same_loc || same_topo_anchor;
    // negation token opposition (word-boundary)
    let neg_opposition = a.negated != b.negated;
    // Every similarity test below also needs neg_opposition, and without a
    // shared target only `high_cosine && neg_opposition` can flag.
    if !shared_target && !neg_opposition {
        return false;
    }

    // Topology facts: same anchor with differing channel/cred boundary -> conflict.
    if same_topo_anchor
        && (a.topo_channel != b.topo_channel || a.topo_credential_boundary != b.topo_credential_boundary)
    {
        return true;
    }
    if shared_target {
        // opposing enforcement: ENFORCED vs NOT_ENFORCED
        let opposing_enf = (a.enforcement == "ENFORCED" && b.enforcement == "NOT_ENFORCED")
            || (a.enforcement == "NOT_ENFORCED" && b.enforcement == "ENFORCED");
        // opposing decision status; attempt outcome on the *other* row
        let (dec_a, dec_b) = (a.decision_status, b.decision_status);
        let opposing_dec = (dec_a == "accepted" && (dec_b == "withdrawn" || dec_b == "superseded" || b.attempt_failed))
            || (dec_b == "accepted" && (dec_a == "withdrawn" || dec_a == "superseded" || a.attempt_failed));
        if opposing_enf || opposing_dec {
            return true;
        }
    }
    if !neg_opposition {
        return false;
    }

    // Pairwise content similarity (replaces query-score proxy). High when
    // token Jaccard similarity >= 0.85 (i.e. cosine distance <= 0.15).
    // Opposite polarity for rules governing the same target (topology or
    // location) also flags at topic overlap >= 0.35 when one side is a rule.
    let overlap_rule = shared_target && (a.is_rule || b.is_rule);
    let floor = if overlap_rule { 0.35 } else { 0.85 };
    match token_jaccard_at_least(&a.tokens, &b.tokens, floor) {
        None => false,
        Some(sim) => sim >= 0.85 || (overlap_rule && sim >= 0.35),
    }
}

/// Detect conflict candidates among active memories (F10). Returns (id, disputed_with_ids).
///
/// Order: one entry per flagged rule/fact row in input order, each listing the
/// rows it conflicts with in input order.
pub fn detect_conflicts(memories: &[crate::traits::SearchResult]) -> Vec<(String, Vec<String>)> {
    let now = chrono::Utc::now().timestamp();
    // Filter expired memories first (F10-6). Also skip archived/mesh-conflicting/registry
    // rows defensively; callers filter these too, but the function must not flag them.
    // Only rule and fact rows take part on either side of a pair.
    let mut interner = std::collections::HashMap::new();
    let rows: Vec<ConflictRow> = memories
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
        .filter(|m| m.payload.memory_type == "rule" || m.payload.memory_type == "fact")
        .map(|m| ConflictRow::new(m, &mut interner))
        .collect();

    // The pair check is symmetric, so each pair is checked once. Pushing j into
    // i's list and i into j's list while i and j both ascend leaves every list
    // in ascending row order, the order the all-pairs scan produced.
    let mut disputed: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    for i in 0..rows.len() {
        for j in (i + 1)..rows.len() {
            if conflict_pair(&rows[i], &rows[j]) {
                disputed[i].push(j);
                disputed[j].push(i);
            }
        }
    }
    rows.iter()
        .zip(disputed)
        .filter(|(_, with)| !with.is_empty())
        .map(|(row, with)| (row.id.to_string(), with.into_iter().map(|j| rows[j].id.to_string()).collect()))
        .collect()
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
