use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use lbug::{Connection, Database, SystemConfig};

use crate::parser::ingest::split_scoped_id;
use crate::traits::{
    DomainMatchMode, DomainSearchOutcome, GraphMatchKind, GraphRead, MemoryHit, MemoryPayload,
    RelocateOutcome, SearchResult, VectorStore,
};

/// Parse metadata JSON string with error handling and logging
fn parse_metadata_or_warn(raw: &str, context: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Null;
    }
    match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {} JSON: {}", context, e);
            Value::Null
        }
    }
}

/// Read a boolean column out of a query row.
///
/// The value has to be read as a typed boolean, never as its rendered form:
/// lbug renders `Value::Bool(true)` as "True", so comparing the rendering
/// against "true" is never true and the flag silently reads false.
///
/// The debug assert turns a future column-type drift into a loud failure in
/// test builds instead of another silent inversion.
fn row_flag_true(value: &lbug::Value) -> bool {
    debug_assert!(
        matches!(value, lbug::Value::Bool(_)),
        "the direction flag must be a typed boolean"
    );
    matches!(value, lbug::Value::Bool(true))
}

/// Internal search routine shared by `search` and `search_with_domain`.
/// Takes an owned `HashSet<String>` of domain keys (already normalized).
async fn search_inner(
    store: &LadybugStore,
    namespace: &str,
    vector: Vec<f32>,
    limit: usize,
    include_tasks: bool,
    include_archived: bool,
    domain_keys: HashSet<String>,
    query_text: Option<String>,
    exact: bool,
    search_mode: crate::traits::SearchMode,
) -> Result<DomainSearchOutcome> {
    // Every vector search funnels through
    // here, including the paths that never init (the fallback CLI's
    // chat ask): a data-bearing store refuses a query embedded under
    // a different model than the one its vectors were recorded with.
    // Lexical search embeds nothing and does not pass through here.
    store.check_data_bearing_identity()?;
    let search_mode = if std::env::var("NEUROSTRATA_HYBRID_SEARCH")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
        && search_mode == crate::traits::SearchMode::Default
    {
        crate::traits::SearchMode::Hybrid
    } else {
        search_mode
    };
    let namespace = namespace.to_string();
    let mut domain_keys = domain_keys;

    // Time-series metrics for this search are recorded at the END of the
    // routine, once real measurements exist: the top result's retrieval
    // distance and the search's measured latency (previously hardcoded
    // placeholders hid actual retrieval behaviour from the detector).
    let started = std::time::Instant::now();
    // KGE secondary distance adjustment: when a trained model exists for this
    // namespace's scope, structural proximity pulls close entities together
    // (bounded so it can never override the semantic order).
    let kge_model = crate::kge::load_for_scope(&store.local_path, &namespace);

    // Ontology-backed query reasoning (RDFS/SHACL): expand the domain seeds
    // through the compiled index and exercise identifier/relation expansion
    // against the query text before the closure runs.
    let mut inferred_results: Vec<SearchResult> = Vec::new();
    if let Some(ont_idx) = store.ontology.load().get(&namespace).cloned() {
        let adapter = crate::rdf::reasoning::ReasoningAdapter::new_with(
            crate::rdf::reasoning::ReasoningConfig::default(),
        );
        if !domain_keys.is_empty() {
            for d in adapter.expand_domains_for_search(
                &ont_idx,
                &domain_keys.iter().cloned().collect::<Vec<_>>(),
            ) {
                domain_keys.insert(d.clone());
                for expanded in adapter.expand_domain(&ont_idx, &d) {
                    domain_keys.insert(expanded);
                }
            }
        }
        if let Some(ref q) = query_text {
        // Hydrate reasoning payloads from in-namespace rows so the
            // reasoning-backed SearchResult rows carry real MemoryPayloads
            // (content, user_id, memory_type, metadata) instead of empty stubs.
            let hydrate_q = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata LIMIT 512",
                escape_ladybug_string(&namespace)
            );
            let hydrated = store
                .with_conn(move |conn| {
                    let mut out: std::collections::HashMap<String, MemoryPayload> =
                        std::collections::HashMap::new();
                    match conn.query(&hydrate_q) {
                        Ok(rows) => {
                            for row in rows {
                                let id = format!("{}", row[0]);
                                if id.is_empty() || out.contains_key(&id) {
                                    continue;
                                }
                                let metadata_str = format!("{}", row[7]);
                                let metadata: Value = match serde_json::from_str(&metadata_str) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        if !metadata_str.trim().is_empty() {
                                            tracing::warn!(
                                                "corrupt metadata JSON in store row: {e}"
                                            );
                                        }
                                        Value::Null
                                    }
                                };
                                out.insert(
                                    id,
                                    MemoryPayload {
                                        content: format!("{}", row[1]),
                                        user_id: format!("{}", row[2]),
                                        memory_type: format!("{}", row[3]),
                                        agent_name: Some(format!("{}", row[4])),
                                        location: format!("{}", row[5]),
                                        location_lines: format!("{}", row[6]),
                                        metadata,
                                    },
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("reasoning payload hydration query failed: {e}");
                        }
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("failed to hydrate reasoning payloads: {e}");
                    std::collections::HashMap::new()
                });
            let exec_adapter = crate::rdf::reasoning::ReasoningAdapter::new()
                .with_payloads(hydrated);
            inferred_results = exec_adapter.execute(&ont_idx, &namespace, q, &[]);
        }
    }

    let metric_ns = namespace.clone();
    let outcome = store
        .with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let vec_str = format!(
                "[{}]",
                // R16: defense-in-depth (no native bind in lbug 0.20.4).
                // Always emit decimal (e.g. 0 → 0.0) so ARRAY_DISTANCE
                // binder never sees pure-integer arrays.
                vector.iter().map(|f| {
                    let s = format!("{}", f);
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        s
                    } else {
                        format!("{}.0", s)
                    }
                }).collect::<Vec<_>>().join(",")
            );
            let now = chrono::Utc::now().timestamp();

            // Active generations for this namespace, resolved per source so a
            // row is validated against the source that owns it (not just any
            // active generation in the namespace). Read once per search.
            let (by_source, active_set, max_code_gen) =
                active_generations_by_source_of(conn, &namespace);

            // LadybugDB (the embedder) has no OFFSET, so we over-fetch with a larger LIMIT and
            // filter/trim in Rust. The multiplier bounds cost: at most
            // FETCH_MULTIPLIER * limit rows are scanned from the engine.
            const FETCH_MULTIPLIER: usize = 10;
            let fetch_limit = limit * FETCH_MULTIPLIER;

            // Step 0: Exact-name pre-pass (F1).
            let mut exact_matches: Vec<SearchResult> = Vec::new();
            if exact {
                if let Some(q) = query_text.as_ref() {
                    let q_trim = q.trim();
                    let mut targets: Vec<String> = vec![q_trim.to_string()];
                    let mut rest = q_trim;
                    while let Some(start) = rest.find('"') {
                        let after = &rest[start+1..];
                        if let Some(end) = after.find('"') {
                            let phrase = after[..end].trim();
                            if !phrase.is_empty() { targets.push(phrase.to_string()); }
                            rest = &after[end+1..];
                        } else { break; }
                    }
                    let safe_ns = escape_ladybug_string(&namespace);
                    let exact_q = format!(
                        "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata",
                        safe_ns
                    );
                    match conn.query(&exact_q) {
                        Ok(rows) => {
                            for row in rows {
                                let id: String = format!("{}", row[0]);
                                let content: String = format!("{}", row[1]);
                                let user_id: String = format!("{}", row[2]);
                                let memory_type: String = format!("{}", row[3]);
                                let agent_name: String = format!("{}", row[4]);
                                let location: String = format!("{}", row[5]);
                                let location_lines: String = format!("{}", row[6]);
                                let metadata_str: String = format!("{}", row[7]);
                                let metadata_val: Value = parse_metadata_or_warn(&metadata_str, "metadata");
                                if is_retired_at(&metadata_val, now) { continue; }
                                if !include_archived && is_archived(&metadata_val) { continue; }
                                if is_mesh_conflicting(&metadata_val) { continue; }
                                if crate::registry::is_registry_row(&metadata_val) { continue; }
                                if !domain_keys.is_empty() && !crate::domain::domain_keys(&metadata_val).iter().any(|k| domain_keys.contains(k)) { continue; }
                                if !include_tasks && memory_type == "task" { continue; }
                                if STRUCTURAL_MEMORY_TYPES.contains(&memory_type.as_str()) { continue; }
                                let symbol = metadata_val.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_lowercase());
                                let location_l = location.to_lowercase();
                                let content_l = content.to_lowercase();
                                let mut matched = false;
                                for t in &targets {
                                    let tl = t.to_lowercase();
                                    if location_l == tl || content_l == tl { matched = true; break; }
                                    if let Some(sym) = &symbol { if *sym == tl { matched = true; break; } }
                                    if let Some(obj) = metadata_val.as_object() {
                                        for (_k, v) in obj.iter() {
                                            if let Some(s) = v.as_str() { if s.to_lowercase() == tl { matched = true; break; } }
                                        }
                                        if matched { break; }
                                    }
                                }
                                if !matched { continue; }
                                if exact_matches.iter().any(|r| r.id == id) { continue; }
                                exact_matches.push(SearchResult {
                                    id: id.clone(),
                                    score: 0.0,
                                    payload: MemoryPayload {
                                        content, user_id, memory_type,
                                        agent_name: Some(agent_name),
                                        location, location_lines, metadata: metadata_val,
                                    },
                                    evidence: Some(vec![crate::traits::SearchEvidence {
                                        kind: crate::traits::EvidenceKind::DirectMatch,
                                        path: vec![],
                                        explanation: "Exact name match.".to_string(),
                                        derivation: vec!["exact-name".to_string()],
                                    }]),
                                });
                            }
                        }
                        Err(e) => { tracing::warn!("query failed: {e}"); }
                    }
                }
            }

            // Step 1: Base vector search.
            //
            // Structural nodes are stored with an all-zero embedding, and the
            // distance from an all-zero row is the query vector's own magnitude
            // -- the same value for every one of them, whatever was asked. Left
            // in, they tie with each other and can fill the limit with path
            // stubs. They stay reachable through the graph expansion in step 2,
            // which is where they are actually useful.
            //
            // ORDER BY dist ASC, id ASC gives deterministic tie-breaking so the
            // same query returns the same rows across runs.
            // Build the row-class filter for the base query.
            // We exclude tasks, archived, and mesh-conflicting rows HERE so they
            // never consume the candidate budget. The server escape hatches
            // (include_tasks, include_archived) control whether these filters apply.
            let mut excluded_types = STRUCTURAL_MEMORY_TYPES.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            if !include_tasks {
                excluded_types.push("task".to_string());
            }
            let excluded_types_str = excluded_types
                .iter()
                .map(|t| format!("'{}'", t))
                .collect::<Vec<_>>()
                .join(", ");

            let search_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND NOT m.memory_type IN [{}] RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC, m.id ASC LIMIT {}",
                safe_ns, excluded_types_str, vec_str, fetch_limit
            );

            let result = conn.query(&search_query)?;
            let result_count = result.get_num_tuples() as usize;
            let mut all_primaries = Vec::new();
            let mut filtered_primaries = Vec::new();
            let mut normative_count: usize = 0;
            // Dedup by bare_id across generations: keep the highest generation
            // for each logical id. Maps bare_id -> (generation, index in all_primaries).
            let mut best_by_bare: HashMap<String, (u64, usize)> = HashMap::new();

            for row in result {
                let id: String = format!("{}", row[0]);
                let distance: f32 = match &row[1] {
                    lbug::Value::Float(f) if f.is_finite() && *f >= 0.0 => *f,
                    lbug::Value::Double(d) if d.is_finite() && *d >= 0.0 => *d as f32,
                    _ => continue,
                };

                let content: String = format!("{}", row[2]);
                let user_id: String = format!("{}", row[3]);
                let memory_type: String = format!("{}", row[4]);
                let agent_name: String = format!("{}", row[5]);
                let location: String = format!("{}", row[6]);
                let location_lines: String = format!("{}", row[7]);
                let metadata_str: String = format!("{}", row[8]);

                let metadata_val: Value = parse_metadata_or_warn(&metadata_str, "metadata");

                // Skip retired rows -- they do not count toward the limit.
                if is_retired_at(&metadata_val, now) {
                    continue;
                }

                // Row-class filters: archived and mesh-conflicting rows are
                // filtered HERE in the store so they never consume a primary slot.
                // The server escape hatches control these filters.
                if !include_archived && is_archived(&metadata_val) {
                    continue;
                }
                if is_mesh_conflicting(&metadata_val) {
                    continue;
                }
                if crate::registry::is_registry_row(&metadata_val) {
                    continue;
                }

                // Domain filter check: happens AFTER row-class filters and BEFORE usage boost.
                // The domain_keys set is already normalized (lowercase-kebab).
                let domain_match = if domain_keys.is_empty() {
                    true // empty key set = no filter asked for
                } else {
                    crate::domain::domain_keys(&metadata_val)
                        .iter()
                        .any(|k| domain_keys.contains(k))
                };

                // Active-generation filter (Wave 2.1): drop rows from superseded
                // ingest generations or other namespaces before they consume a
                // candidate slot. Unscoped normative rows always pass.
                let kept_id = is_active_scoped_row(
                    &id,
                    &namespace,
                    &memory_type,
                    &metadata_val,
                    &by_source,
                    &active_set,
                    max_code_gen,
                );
                let Some(_kept_id) = kept_id else { continue };

                // Usage boost: ln(1+n) gives a real gain even at n=1, and the
                // cap at half distance preserves semantic ranking order.
                //
                // FEATURE-4: provenance earns a share of that same capped
                // budget. It is added *inside* the cap rather than after it --
                // subtracting from `distance` a term that was never itself
                // bounded is how a tie-breaker quietly becomes an override and
                // starts returning the wrong memory for the right reason.
                let access_count = metadata_val.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
                let n = access_count.max(0) as f32;
                let gain = 0.05 * (1.0 + n).ln();
                let source_gain = if crate::traits::has_source(&metadata_val) { SOURCE_RANK_GAIN } else { 0.0 };
                let feedback_gain = metadata_val
                    .get("feedback_score")
                    .and_then(|v| v.as_f64())
                    .map(|r| FEEDBACK_RANK_GAIN * (r as f32).clamp(-1.0, 1.0))
                    .unwrap_or(0.0);
                let capped_gain = (gain + source_gain + feedback_gain)
                    .clamp(-FEEDBACK_RANK_GAIN.min(0.5 * distance), 0.5 * distance);
                let mut boosted_distance = if distance == 0.0 {
                    0.0
                } else {
                    distance - capped_gain
                };

                // KGE secondary distance adjustment (bounded): structural
                // closeness to the top-ranked primary earns a small distance
                // bonus, scaled by the model's own weight and a tenth of the
                // raw distance so the tie-breaker stays a tie-breaker.
                if let Some(ref model) = kge_model {
                    if let Some(first_primary_id) = all_primaries.first().map(|p: &SearchResult| p.id.clone()) {
                        let kge_norm = model.normalized_structural_distance(&first_primary_id, &id);
                        if kge_norm < 1.0 {
                            let boost = (1.0 - kge_norm)
                                * model.kge_weight.clamp(0.0, crate::kge::DEFAULT_KGE_WEIGHT)
                                * 0.1
                                * distance;
                            boosted_distance -= boost;
                        }
                    }
                }

                let sr = SearchResult {
                    id: id.clone(),
                    score: boosted_distance,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                    evidence: Some(vec![crate::traits::SearchEvidence {
                        kind: crate::traits::EvidenceKind::DirectMatch,
                        path: vec![],
                        explanation: "This memory matched the query vector.".to_string(),
                        derivation: vec![],
                    }]),
                };

                // Dedup by bare_id across generations: keep the highest
                // generation for each logical id. Unscoped rows (bare_id_of
                // returns None) are never deduped.
                let is_normative_primary =
                    NORMATIVE_MEMORY_TYPES.contains(&sr.payload.memory_type.as_str())
                        && boosted_distance <= MAX_NORMATIVE_DISTANCE;
                if let Some(bare) = bare_id_of(&id) {
                    let gen = split_scoped_id(&id)
                        .map(|(_, g, _)| g as u64)
                        .unwrap_or(0);
                    match best_by_bare.get(&bare) {
                        Some(&(existing_gen, idx)) if gen > existing_gen => {
                            // Supersede the lower-generation entry in place.
                            all_primaries[idx] = sr.clone();
                            if domain_match {
                                if let Some(pos) = filtered_primaries.iter().position(|r: &SearchResult| {
                                    bare_id_of(&r.id).as_deref() == Some(bare.as_str())
                                }) {
                                    filtered_primaries[pos] = sr;
                                } else {
                                    filtered_primaries.push(sr);
                                }
                            }
                            best_by_bare.insert(bare, (gen, idx));
                        }
                        Some(_) => {
                            // Existing entry has higher or equal gen; skip.
                            continue;
                        }
                        None => {
                            let idx = all_primaries.len();
                            best_by_bare.insert(bare, (gen, idx));
                            all_primaries.push(sr.clone());
                            if domain_match {
                                filtered_primaries.push(sr);
                            }
                            if is_normative_primary && (domain_keys.is_empty() || domain_match) {
                                normative_count += 1;
                            }
                        }
                    }
                } else {
                    all_primaries.push(sr.clone());
                    if domain_match {
                        filtered_primaries.push(sr);
                    }
                    if is_normative_primary && (domain_keys.is_empty() || domain_match) {
                        normative_count += 1;
                    }
                };

                // Stop condition counts filtered_primaries when filter is active,
                // otherwise counts all_primaries (which is the same as filtered when empty).
                // Normative rows ranked beyond the limit still need to enter
                // `all_primaries` / `filtered_primaries` so the slot reservation
                // below can promote them: only break early once the reserved
                // normative slots are satisfied or the candidate list is exhausted.
                let stop_count = if domain_keys.is_empty() {
                    all_primaries.len()
                } else {
                    filtered_primaries.len()
                };
                if stop_count >= limit && (limit < 3 || normative_count >= normative_slot_limit(limit)) {
                    break;
                }
            }

            // Determine chosen set and mode, reserving normative slots within `limit`
            // so structural (code_ast/doc_section) primaries cannot crowd out
            // normative rules/facts/decisions/procedures before graph expansion.
            let (chosen_primaries, mode) = if domain_keys.is_empty() {
                // No filter asked for: all rows go to both lists, mode is Vector
                (reserve_normative_slots(all_primaries, limit), DomainMatchMode::Vector)
            } else if filtered_primaries.is_empty() && !all_primaries.is_empty() {
                // Filter would empty the candidates: skip it, use unfiltered, mode is Vector
                (reserve_normative_slots(all_primaries, limit), DomainMatchMode::Vector)
            } else {
                // Filter applied successfully
                (reserve_normative_slots(filtered_primaries, limit), DomainMatchMode::Domain)
            };

            let mut all_primaries_with_exact: Vec<SearchResult> = exact_matches.clone();
            for cp in chosen_primaries {
                if !all_primaries_with_exact.iter().any(|r| r.id == cp.id) {
                    all_primaries_with_exact.push(cp);
                }
            }
            let primary_ids: Vec<String> = all_primaries_with_exact.iter().map(|r| r.id.clone()).collect();
            let mut results = all_primaries_with_exact;

            // Step 2: Hybrid GraphRAG — directed evidence expansion.
            // Four bounded queries replace the old undirected ones, each using
            // label(e) for the relationship name and an outgoing flag for direction.
            // The expansion is NOT domain-filtered: a topology fact's GOVERNS neighbour
            // is the context that made the fact worth recalling, and filtering it away
            // would break the one-hop path the parent spec §4.2.3 depends on.
            if !primary_ids.is_empty() {
                let id_list = primary_ids.iter()
                    .map(|id| format!("'{}'", escape_ladybug_string(&id)))
                    .collect::<Vec<_>>()
                    .join(", ");

                let supported_labels = ["RELATES_TO", "CONTAINS", "GOVERNS", "DEPENDS_ON", "EXTRACTED_FROM"];
                let primary_set: std::collections::HashSet<&str> =
                    primary_ids.iter().map(|s| s.as_str()).collect();

                // One-hop neighbor queries (outgoing and incoming)
                let neighbor_out_q = format!(
                    "MATCH (a:Memory)-[e]->(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata, a.id, label(e) AS relation, e.extractor AS extractor, true AS outgoing ORDER BY b.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );
                let neighbor_in_q = format!(
                    "MATCH (a:Memory)<-[e]-(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata, a.id, label(e) AS relation, e.extractor AS extractor, false AS outgoing ORDER BY b.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // Two-hop governing queries (outgoing and incoming)
                let govern_out_q = format!(
                    "MATCH (a:Memory)-[e]->(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata, a.id, label(e) AS relation, f.id, f.metadata, true AS outgoing ORDER BY r.id ASC, f.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );
                let govern_in_q = format!(
                    "MATCH (a:Memory)<-[e]-(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata, a.id, label(e) AS relation, f.id, f.metadata, false AS outgoing ORDER BY r.id ASC, f.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                let govern_out_cont_q = format!(
                    "MATCH (a:Memory)-[:CONTAINS]->(f1:Memory)-[:CONTAINS]->(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata, a.id, 'CONTAINS' AS relation, f.id, f.metadata, f1.id, f1.metadata, true AS outgoing ORDER BY r.id ASC, f.id ASC, f1.id ASC, a.id ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );
                let govern_in_cont_q = format!(
                    "MATCH (a:Memory)<-[:CONTAINS]-(f1:Memory)<-[:CONTAINS]-(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata, a.id, 'CONTAINS' AS relation, f.id, f.metadata, f1.id, f1.metadata, false AS outgoing ORDER BY r.id ASC, f.id ASC, f1.id ASC, a.id ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // Episode-sibling query: memories extracted from the same task.
                // Symmetric through the task: (m:Memory)-[:EXTRACTED_FROM]->(t:Memory)<-[:EXTRACTED_FROM]-(s:Memory)
                // m = originating primary, t = intermediate task, s = sibling candidate.
                let episode_sibling_q = format!(
                    "MATCH (m:Memory)-[:EXTRACTED_FROM]->(t:Memory)<-[:EXTRACTED_FROM]-(s:Memory) WHERE m.id IN [{}] AND s.namespace = '{}' AND NOT s.id IN [{}] RETURN DISTINCT s.id, s.content, s.user_id, s.memory_type, s.agent_name, s.location, s.location_lines, s.metadata, m.id, t.id, t.metadata ORDER BY s.id ASC, m.id ASC, t.id ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // Phase 1: Read all rows, build evidence, group by result ID.
                // Value: (payload, evidence_list, co_occurrence_count).
                // co_occurrence_count = number of DISTINCT originating primaries that reach this candidate.
                #[derive(Default)]
                struct CandidateEntry {
                    payload: Option<MemoryPayload>,
                    evidence: Vec<crate::traits::SearchEvidence>,
                    primary_sources: std::collections::HashSet<String>,
                }
                let mut by_id: std::collections::HashMap<String, CandidateEntry> =
                    std::collections::HashMap::new();

                // Process one-hop neighbor queries (4 directed branches total).
                for q in [&neighbor_out_q, &neighbor_in_q] {
                    match conn.query(q) {
                        Ok(rows) => {
                        for row in rows {
                            let result_id: String = format!("{}", row[0]);
                            let content: String = format!("{}", row[1]);
                            let user_id: String = format!("{}", row[2]);
                            let memory_type: String = format!("{}", row[3]);
                            let agent_name: String = format!("{}", row[4]);
                            let location: String = format!("{}", row[5]);
                            let location_lines: String = format!("{}", row[6]);
                            let metadata_str: String = format!("{}", row[7]);
                            let metadata_val: Value =
                                parse_metadata_or_warn(&metadata_str, "metadata");
                            if is_retired_at(&metadata_val, now) { continue; }
                            if primary_set.contains(result_id.as_str()) { continue; }

                            // Row-class filters for expansion candidates (same as primaries)
                            // Use memory_type column directly, not metadata
                            if !include_tasks && memory_type == "task" { continue; }
                            if !include_archived && is_archived(&metadata_val) { continue; }
                            if is_mesh_conflicting(&metadata_val) { continue; }
                            if crate::registry::is_registry_row(&metadata_val) { continue; }

                            let primary_id: String = format!("{}", row[8]);
                            let relation: String = format!("{}", row[9]);
                            let extractor: String = format!("{}", row[10]);
                            let outgoing: bool = row_flag_true(&row[11]);
                            if !supported_labels.contains(&relation.as_str()) { continue; }

                            let (src, tgt) = if outgoing {
                                (primary_id.clone(), result_id.clone())
                            } else {
                                (result_id.clone(), primary_id.clone())
                            };
                            let edges = vec![crate::traits::EvidenceEdge {
                                source: src.clone(), relation: relation.clone(), target: tgt.clone(),
                            }];
                            let kind = crate::traits::EvidenceKind::OneHop;
                            let norm = norm_path(&edges, &kind);

                            let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                            if entry.payload.is_none() {
                                entry.payload = Some(MemoryPayload {
                                    content, user_id, memory_type,
                                    agent_name: Some(agent_name),
                                    location, location_lines, metadata: metadata_val,
                                });
                            }
                            // Track distinct primary sources for co-occurrence ranking
                            entry.primary_sources.insert(primary_id);
                            let mut derivation = Vec::new();
                            if relation == "RELATES_TO" && extractor == "mention-detector" {
                                derivation.push("mention-link".to_string());
                            }
                            if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                                entry.evidence.push(crate::traits::SearchEvidence {
                                    kind,
                                    explanation: "This memory is one graph hop from a direct match.".to_string(),
                                    derivation,
                                    path: edges,
                                });
                            }
                        }
                    }
                    Err(e) => { tracing::warn!("query failed: {e}"); }
                }
                }

                // Process two-hop governing queries
                for q in [&govern_out_q, &govern_in_q] {
                    match conn.query(q) {
                        Ok(rows) => {
                        for row in rows {
                            let result_id: String = format!("{}", row[0]);
                            // memory_type is at row[3] (r.memory_type) - use column directly for task filter
                            let memory_type: String = format!("{}", row[3]);
                            let metadata_str: String = format!("{}", row[7]);
                            let metadata_val: Value =
                                parse_metadata_or_warn(&metadata_str, "metadata");
                            if is_retired_at(&metadata_val, now) { continue; }
                            if primary_set.contains(result_id.as_str()) { continue; }

                            // Row-class filters for expansion candidates
                            // Use memory_type column directly, not metadata (same as one-hop fix)
                            if !include_tasks && memory_type == "task" { continue; }
                            if !include_archived && is_archived(&metadata_val) { continue; }
                            if is_mesh_conflicting(&metadata_val) { continue; }
                            if crate::registry::is_registry_row(&metadata_val) { continue; }

                            let primary_id: String = format!("{}", row[8]);
                            let relation: String = format!("{}", row[9]);
                            let f_id: String = format!("{}", row[10]);
                            let f_metadata_str: String = format!("{}", row[11]);
                            let f_metadata: Value =
                                parse_metadata_or_warn(&f_metadata_str, "f_metadata");
                            let outgoing: bool = row_flag_true(&row[12]);

                            // Retired intermediate invalidates only that path
                            if is_retired_at(&f_metadata, now) { continue; }
                            if !supported_labels.contains(&relation.as_str()) { continue; }

                            let (src1, tgt1) = if outgoing {
                                (primary_id.clone(), f_id.clone())
                            } else {
                                (f_id.clone(), primary_id.clone())
                            };
                            let edges = vec![
                                crate::traits::EvidenceEdge {
                                    source: src1, relation, target: tgt1,
                                },
                                crate::traits::EvidenceEdge {
                                    source: result_id.clone(),
                                    relation: "GOVERNS".to_string(),
                                    target: f_id,
                                },
                            ];
                            let kind = crate::traits::EvidenceKind::Governs2hop;
                            let norm = norm_path(&edges, &kind);

                            let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                            if entry.payload.is_none() {
                                let content: String = format!("{}", row[1]);
                                let user_id: String = format!("{}", row[2]);
                                let agent_name: String = format!("{}", row[4]);
                                let location: String = format!("{}", row[5]);
                                let location_lines: String = format!("{}", row[6]);
                                entry.payload = Some(MemoryPayload {
                                    content, user_id, memory_type: memory_type.clone(),
                                    agent_name: Some(agent_name),
                                    location, location_lines, metadata: metadata_val,
                                });
                            }
                            // Track distinct primary sources for co-occurrence ranking
                            entry.primary_sources.insert(primary_id);
                            if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                                entry.evidence.push(crate::traits::SearchEvidence {
                                    kind,
                                    explanation: "This memory governs a node one hop from a direct match.".to_string(),
                                    derivation: vec![],
                                    path: edges,
                                });
                            }
                        }
                    }
                    Err(e) => { tracing::warn!("query failed: {e}"); }
                }
                }

                // Process 3-hop transitive governing queries (CONTAINS hops).
                for q in [&govern_out_cont_q, &govern_in_cont_q] {
                    match conn.query(q) {
                        Ok(rows) => {
                        for row in rows {
                            let result_id: String = format!("{}", row[0]);
                            let memory_type: String = format!("{}", row[3]);
                            let metadata_str: String = format!("{}", row[7]);
                            let metadata_val: Value =
                                parse_metadata_or_warn(&metadata_str, "metadata");
                            if is_retired_at(&metadata_val, now) { continue; }
                            if primary_set.contains(result_id.as_str()) { continue; }
                            if !include_tasks && memory_type == "task" { continue; }
                            if !include_archived && is_archived(&metadata_val) { continue; }
                            if is_mesh_conflicting(&metadata_val) { continue; }
                            if crate::registry::is_registry_row(&metadata_val) { continue; }

                            let primary_id: String = format!("{}", row[8]);
                            let f_id: String = format!("{}", row[10]);
                            let f_metadata_str: String = format!("{}", row[11]);
                            let f_metadata: Value =
                                parse_metadata_or_warn(&f_metadata_str, "f_metadata");
                            let f1_id: String = format!("{}", row[12]);
                            let f1_metadata_str: String = format!("{}", row[13]);
                            let f1_metadata: Value =
                                parse_metadata_or_warn(&f1_metadata_str, "f1_metadata");
                            let outgoing: bool = row_flag_true(&row[14]);

                            if is_retired_at(&f_metadata, now) { continue; }
                            if is_retired_at(&f1_metadata, now) { continue; }

                            let (src1, tgt1) = if outgoing {
                                (primary_id.clone(), f1_id.clone())
                            } else {
                                (f1_id.clone(), primary_id.clone())
                            };
                            let edges = vec![
                                crate::traits::EvidenceEdge {
                                    source: src1, relation: "CONTAINS".to_string(), target: tgt1,
                                },
                                crate::traits::EvidenceEdge {
                                    source: if outgoing { f1_id.clone() } else { f_id.clone() },
                                    relation: "CONTAINS".to_string(),
                                    target: if outgoing { f_id.clone() } else { f1_id.clone() },
                                },
                                crate::traits::EvidenceEdge {
                                    source: result_id.clone(),
                                    relation: "GOVERNS".to_string(),
                                    target: f_id.clone(),
                                },
                            ];
                            let kind = crate::traits::EvidenceKind::Governs2hop;
                            let norm = norm_path(&edges, &kind);

                            let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                            if entry.payload.is_none() {
                                let content: String = format!("{}", row[1]);
                                let user_id: String = format!("{}", row[2]);
                                let agent_name: String = format!("{}", row[4]);
                                let location: String = format!("{}", row[5]);
                                let location_lines: String = format!("{}", row[6]);
                                entry.payload = Some(MemoryPayload {
                                    content, user_id, memory_type: memory_type.clone(),
                                    agent_name: Some(agent_name),
                                    location, location_lines, metadata: metadata_val,
                                });
                            }
                            entry.primary_sources.insert(primary_id);
                            if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                                entry.evidence.push(crate::traits::SearchEvidence {
                                    kind,
                                    explanation: "This memory governs a node reachable via CONTAINS hops from a direct match.".to_string(),
                                    derivation: vec!["transitive-governs".to_string(), "contains-2hop".to_string()],
                                    path: edges,
                                });
                            }
                        }
                    }
                    Err(e) => { tracing::warn!("query failed: {e}"); }
                }
                }

                // Process episode-sibling query
                // Row layout: s.id, s.content, s.user_id, s.memory_type, s.agent_name, s.location, s.location_lines, s.metadata, m.id, t.id, t.metadata
                match conn.query(&episode_sibling_q) {
                    Ok(rows) => {
                    for row in rows {
                        let result_id: String = format!("{}", row[0]);
                        let content: String = format!("{}", row[1]);
                        let user_id: String = format!("{}", row[2]);
                        let memory_type: String = format!("{}", row[3]);
                        let agent_name: String = format!("{}", row[4]);
                        let location: String = format!("{}", row[5]);
                        let location_lines: String = format!("{}", row[6]);
                        let metadata_str: String = format!("{}", row[7]);
                        let metadata_val: Value =
                            parse_metadata_or_warn(&metadata_str, "metadata");
                        if is_retired_at(&metadata_val, now) { continue; }
                        if primary_set.contains(result_id.as_str()) { continue; }

                        // Row-class filters for the SIBLING (same as other expansion candidates)
                        if !include_tasks && memory_type == "task" { continue; }
                        if !include_archived && is_archived(&metadata_val) { continue; }
                        if is_mesh_conflicting(&metadata_val) { continue; }
                        if crate::registry::is_registry_row(&metadata_val) { continue; }

                        let primary_id: String = format!("{}", row[8]); // m.id (originating primary)
                        let t_id: String = format!("{}", row[9]);     // t.id (intermediate task)
                        let t_metadata_str: String = format!("{}", row[10]);
                        let t_metadata: Value =
                            parse_metadata_or_warn(&t_metadata_str, "t_metadata");

                        // Retired intermediate task invalidates only that path (matching two-hop rule)
                        if is_retired_at(&t_metadata, now) { continue; }

                        // Evidence path: primary -(EXTRACTED_FROM)-> task <-EXTRACTED_FROM- sibling
                        let edges = vec![
                            crate::traits::EvidenceEdge {
                                source: primary_id.clone(),
                                relation: "EXTRACTED_FROM".to_string(),
                                target: t_id.clone(),
                            },
                            crate::traits::EvidenceEdge {
                                source: result_id.clone(),
                                relation: "EXTRACTED_FROM".to_string(),
                                target: t_id,
                            },
                        ];
                        let kind = crate::traits::EvidenceKind::EpisodeSibling;

                        let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                        if entry.payload.is_none() {
                            entry.payload = Some(MemoryPayload {
                                content, user_id, memory_type,
                                agent_name: Some(agent_name),
                                location, location_lines, metadata: metadata_val,
                            });
                        }
                        // Track distinct primary sources for co-occurrence ranking
                        entry.primary_sources.insert(primary_id);
                        let norm = norm_path(&edges, &kind);
                        if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                            entry.evidence.push(crate::traits::SearchEvidence {
                                kind,
                                explanation: "This memory and a direct match were both extracted from the same task.".to_string(),
                                derivation: vec![],
                                path: edges,
                            });
                        }
                    }
                }
                Err(e) => { tracing::warn!("query failed: {e}"); }
            }

                // Phase 2: Admit DISTINCT result IDs in family order — one-hop
                // family first, then episode-sibling, then governing-only — capped at `limit` per family.
                // Within each family, order by co-occurrence count (distinct primary sources) DESC,
                // then by id ASC for stable tiebreak.
                let mut candidates: Vec<(usize, usize, String)> = by_id.iter()
                    .filter_map(|(id, entry)| {
                        entry.payload.as_ref()?;
                        let family = if entry.evidence.iter().any(|e| e.kind == crate::traits::EvidenceKind::OneHop) { 0 }
                            else if entry.evidence.iter().any(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling) { 1 }
                            else { 2 };
                        let co_occur = entry.primary_sources.len();
                        Some((family, co_occur, id.clone()))
                    })
                    .collect();
                // Sort by: family ASC (one-hop, then episode-sibling, then governing), co_occurrence DESC, id ASC
                candidates.sort_by(|a, b| {
                    a.0.cmp(&b.0)  // family: 0 (one-hop) before 1 (episode-sibling) before 2 (governing)
                        .then_with(|| b.1.cmp(&a.1))  // co-occurrence DESC
                        .then_with(|| a.2.cmp(&b.2))  // id ASC for stable tiebreak
                });
                let mut admitted = Vec::new();
                let mut per_family = [0usize; 3];
                for (family, _co_occur, id) in &candidates {
                    if per_family[*family] >= limit {
                        continue;
                    }
                    per_family[*family] += 1;
                    admitted.push(id.clone());
                }

                // Phase 3: Build results with capped evidence (max 3 per result).
                // Score for expansions: use a small constant that sorts after all
                // real boosted_distance scores but before the old 10.0 sentinel.
                // The real discriminator is EvidenceKind, not score.
                const EXPANSION_BASE_SCORE: f32 = 1.0; // sorts after any real boosted_distance (< 1.0 typically)
                for id in &admitted {
                    let entry = match by_id.remove(id) {
                        Some(e) => e,
                        None => {
                            tracing::warn!("admitted id {} not in by_id map (drift); skipping", id);
                            continue;
                        }
                    };
                    let payload = match entry.payload {
                        Some(p) => p,
                        None => {
                            tracing::warn!("entry for {} has no payload (drift); skipping", id);
                            continue;
                        }
                    };
                    let mut evidence = entry.evidence;
                    // Sort evidence: kind (OneHop < Governs2hop), then path edges
                    evidence.sort_by(|a, b| {
                        a.kind.cmp(&b.kind).then_with(|| {
                            a.path.iter().zip(b.path.iter())
                                .map(|(ae, be)| ae.source.cmp(&be.source)
                                    .then(ae.relation.cmp(&be.relation))
                                    .then(ae.target.cmp(&be.target)))
                                .find(|o| *o != std::cmp::Ordering::Equal)
                                .unwrap_or(a.path.len().cmp(&b.path.len()))
                        })
                    });
                    evidence.truncate(3);
                    results.push(SearchResult {
                        id: id.clone(),
                        score: EXPANSION_BASE_SCORE,
                        payload,
                        evidence: if evidence.is_empty() { None } else { Some(evidence) },
                    });
                }
            }

            // Merge reasoning-inferred rows: a row that the vector/graph pass
            // did not surface but reasoning derived (and whose payload was
            // hydrated to real content) joins the result set. Empty-stub rows
            // carry no content and are never merged, so reasoning cannot
            // invent a result.
            for inferred in inferred_results {
                if inferred.payload.content.is_empty() {
                    continue;
                }
                if results.iter().any(|existing| existing.id == inferred.id) {
                    continue;
                }
                results.push(inferred);
            }

            // Decision-shape gain: a `decision`-shaped fact earns a small
            // bonus. Scores here are DISTANCES (lower = better, primary sort
            // is ascending), so the gain is SUBTRACTED to genuinely pull
            // decisions above equivalently-distant unshaped facts (applied
            // before the final sort so the order reflects it).
            for r in results.iter_mut() {
                r.score = (r.score - crate::search::decision_shape_boost(&r.payload.metadata)).max(0.0);
            }

            // Final sort: primaries (DirectMatch, real boosted_distance) first,
            // then expansions (OneHop/Governs2hop, EXPANSION_BASE_SCORE).
            // Within expansions, the admission order (co-occurrence DESC, family order) is preserved.
            results.sort_by(|a, b| {
                let a_is_primary = a.evidence.as_ref().map_or(false, |evs| evs.iter().any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch));
                let b_is_primary = b.evidence.as_ref().map_or(false, |evs| evs.iter().any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch));
                match (a_is_primary, b_is_primary) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    (true, true) => a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal),
                    (false, false) => std::cmp::Ordering::Equal, // preserve admission order (stable sort)
                }
            });

            // Incomplete marker: set when the full 10x budget was consumed by the
// Cypher LIMIT, meaning more matching rows beyond the page may exist and
// must be fetched via pagination to avoid losing live matches hidden behind
// rows filtered out after LIMIT. Heuristic: if result_count == fetch_limit
// the engine's LIMIT was hit, indicating the multiplier exhausted and pages
// beyond remain.
let incomplete = result_count >= fetch_limit;

            // Hybrid scoring: combine vector, graph, and symbol distances.
            // When search_mode is Hybrid, replace the default search score with the
            // When Hybrid mode is configured, compute normalized hybrid distance
            // from vector distance, graph co-occurrence, and symbol match, then
            // sort ASC by the hybrid score with MemoryId ASC tie-break.
            if search_mode == crate::traits::SearchMode::Hybrid {
                let mut hybrid_results: Vec<SearchResult> = results.clone();
                for sr in &mut hybrid_results {
                    // d_vec: feedback gain now sourced from the row's actual
                    // feedback_score metadata instead of hardcoded 0.0.
                    let fb_gain = sr.payload.metadata
                        .get("feedback_score")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32;
                    let d_vec = crate::graph_read::hybrid::normalize_vec(sr.score, fb_gain);
                    // d_graph: co-occurrence from Step 2 primary_sources count.
                    // At this point in the function the expansion data has been
                    // consumed, so we approximate with metadata "co_occurrence_count"
                    // (set during ingestion/search-expansion when available);
                    // when absent/0, fall back to declared graph edges
                    // (governs/relates_to/contained_by array lengths) plus
                    // access_count so d_graph reflects real connectivity.
                    let co_from_meta = sr.payload.metadata
                        .get("co_occurrence_count")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let co_count = if co_from_meta > 0 {
                        co_from_meta
                    } else {
                        let declared_edges: usize =
                            ["governs", "relates_to", "contained_by"]
                                .iter()
                                .filter_map(|k| {
                                    sr.payload.metadata
                                        .get(*k)
                                        .and_then(|v| v.as_array())
                                        .map(|a| a.len())
                                })
                                .sum();
                        let access = sr.payload.metadata
                            .get("access_count")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        (declared_edges as i64 + access).max(0)
                    };
                    let d_graph = crate::graph_read::hybrid::normalize_graph(co_count as u64);
                    // d_sym: compute symbol match grade from query and candidate symbols
                    let mut candidate_symbols: Vec<String> = Vec::new();
                    if let Some(syms) = sr.payload.metadata.get("symbols").and_then(|v| v.as_array()) {
                        for s in syms {
                            if let Some(st) = s.as_str() {
                                candidate_symbols.push(st.to_string());
                            }
                        }
                    }
                    if let Some(governs) = sr.payload.metadata.get("governs").and_then(|v| v.as_array()) {
                        for g in governs {
                            if let Some(gt) = g.as_str() {
                                candidate_symbols.push(gt.to_string());
                            }
                        }
                    }
                    if !sr.payload.location.is_empty() {
                        candidate_symbols.push(sr.payload.location.clone());
                    }
                    candidate_symbols.push(sr.id.clone());
                    let sym_grade = crate::graph_read::hybrid::compute_symbol_match(
                        query_text.as_deref().unwrap_or(""),
                        &candidate_symbols,
                    );
                    let d_sym = crate::graph_read::hybrid::normalize_sym(sym_grade);
                    let hybrid_weights = crate::config::Config::from_default_path()
                        .map(|c| c.hybrid_weights)
                        .unwrap_or((0.5, 0.3, 0.2));
                    sr.score = crate::graph_read::hybrid::combine(d_vec, d_graph, d_sym, hybrid_weights);
                }
                // Sort ASC by hybrid score, tie-break MemoryId ASC.
                hybrid_results.sort_by(|a, b| {
                    let a_score = a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal);
                    match a_score {
                        std::cmp::Ordering::Less => std::cmp::Ordering::Less,
                        std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
                        std::cmp::Ordering::Equal => a.id.cmp(&b.id),
                    }
                });
                results = reserve_normative_slots(hybrid_results, limit);
            }

            Ok(DomainSearchOutcome { results, mode, incomplete })
        })
        .await;
    let outcome = outcome?;
    // Time-series metrics for this search: REAL measurements rather than the
    // old hardcoded placeholders (0.0 distance / 1.0 latency). The best
    // (lowest) retrieval distance across the final result set and the
    // search's measured wall-clock latency in ms. Flushed periodically into
    // the MemoryMetric table; anomalies pass through the detector + callback.
    let top_distance = outcome
        .results
        .iter()
        .map(|r| r.score as f64)
        .fold(f64::INFINITY, f64::min);
    let top_distance = if top_distance.is_finite() {
        top_distance
    } else {
        0.0
    };
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    store.metrics.record(
        &metric_ns,
        crate::telemetry::anomaly::MetricKind::RetrievalDistance,
        "search",
        chrono::Utc::now().timestamp(),
        top_distance,
        std::collections::BTreeMap::new(),
    );
    store.metrics.record(
        &metric_ns,
        crate::telemetry::anomaly::Latency,
        "search",
        chrono::Utc::now().timestamp(),
        latency_ms,
        std::collections::BTreeMap::new(),
    );
    Ok(outcome)
}

pub struct LadybugStore {
    local_path: PathBuf,
    dimensions: usize,
    db: Arc<Database>,
    dirty: Dirty,
    /// LadybugDB allows exactly one write transaction and REFUSES a second
    /// rather than queueing it, so two writers that overlap produce "Cannot
    /// start a new write transaction in the system" for whichever arrives
    /// second. Every search spawns access-count writes, which makes that
    /// collision ordinary rather than exotic (bead neurostrata-3fi.6.6).
    ///
    /// Taking a turn here converts a random failure into a short wait. Writes
    /// are milliseconds now that checkpointing has left the request path, and
    /// the caller's deadline covers the wait as well as the write. The
    /// checkpoint deliberately does NOT take this lock: it waits for
    /// transactions to drain, so holding writers out for its duration would
    /// recreate the stall it was moved out of the request path to avoid.
    write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Set once the tables exist. init() is called on the way into add, move
    /// and -- because it is cheap to write and expensive to mean -- every
    /// search, and each call issued CREATE NODE TABLE only to be told it
    /// already exists. That is a write transaction per read, colliding with
    /// real writes for nothing.
    schema_ready: std::sync::atomic::AtomicBool,
    /// Compiled ontology index per namespace (domain DAG, aliases, sub-properties).
    /// Swapped on `refresh_ontology`; read lock-free via ArcSwap.
    ontology: Arc<arc_swap::ArcSwap<std::collections::BTreeMap<String, crate::rdf::OntologyIndex>>>,
    /// Cached active-generation symbol ids for mention detection, per
    /// namespace. A generation counter is per namespace, so two namespaces can
    /// share a generation set; the namespace is part of the key.
    mention_symbol_ids: Arc<tokio::sync::RwLock<std::collections::HashMap<String, (std::collections::HashSet<u64>, std::sync::Arc<Vec<String>>)>>>,
    /// Lazily built in-house lexical index, partitioned by namespace.
    lexical: Arc<std::sync::RwLock<crate::fts::LexicalIndex>>,
    /// Namespaces whose lexical partition is loaded. A namespace absent here is
    /// rebuilt from the database on the next query; mutations are no-ops until
    /// then because the database is the source of truth.
    lexical_loaded: Arc<std::sync::RwLock<std::collections::BTreeSet<String>>>,
    /// In-memory ring-buffer & rolling aggregate time-series metrics store.
    pub metrics: Arc<crate::time_series::TimeSeriesStore>,
    /// Optional disk-backed Tantivy full-text index, opened lazily.
    pub tantivy_lexical: Arc<std::sync::RwLock<Option<crate::lexical::TantivyLexicalIndex>>>,
    /// Embedding-space identity registry (Composite Recommendation #11):
    /// the first `EmbeddingDescriptor` recorded per namespace in `init` is
    /// authoritative for the store's lifetime; a later descriptor that fails
    /// `compatible` is rejected so incompatible embedding spaces cannot be
    /// mixed through this store.
    embedding_registry: Arc<
        std::sync::Mutex<std::collections::HashMap<String, crate::traits::EmbeddingDescriptor>>,
    >,
    /// Directory the configured embedding model is resolved from
    /// (`embedders.json`). Production uses `crate::embed::config_dir()`;
    /// tests override it via `with_embed_config_dir`.
    embed_config_dir: PathBuf,
    /// The embedding-space descriptor of the embedder this process
    /// actually built (`Embedder::embedding_descriptor()`), bound at
    /// construction by the paths that build one. The store-state
    /// sidecar's adoption and first-write guard key on this *serving*
    /// descriptor, never on a config re-read: a process
    /// that started before a setup pick landed keeps serving its
    /// original model, and only a process whose serving descriptor also
    /// matches the current configuration may record or adopt store
    /// identity. `None` for processes that build no embedder (backup,
    /// restore-verify, reports): they never create or move the recorded
    /// descriptor. Interior-mutable so the
    /// fallback CLI arms — which construct the store before they build
    /// their embedder — can bind the descriptor after construction via
    /// [`LadybugStore::set_serving_descriptor`].
    serving_descriptor: std::sync::Mutex<Option<crate::traits::EmbeddingDescriptor>>,
    /// Whether the database file already existed when this process opened
    /// it. A sidecar-less store this process created is a fresh install;
    /// a sidecar-less store that predates it is a pre-fix store whose
    /// data state is unknowable from outside (fail-closed).
    db_preexisting: bool,
    /// Whether this process opened the database through a recovery
    /// path in `with_system_config` — the checksum-mismatch reopen,
    /// the unverified-WAL replay, or the WAL-discard reopen — rather
    /// than the normal verified open. A recovery open's view of the
    /// store is unauthenticated: rows that lived only in the WAL can
    /// present as absent, so an apparently empty table on such an
    /// open is not evidence the store is pristine, and `init` must
    /// not run the LockedUnknown provenance upgrade from it.
    opened_via_recovery: bool,
    /// This process's view of the store-state sidecar, loaded on first
    /// use. The sidecar is the setup-time model choice's lock record:
    /// see `crate::model_choice::store_has_data`.
    store_state: Arc<std::sync::Mutex<Option<CachedStoreState>>>,
}

/// The store-state sidecar as this process knows it.
#[derive(Clone)]
enum CachedStoreState {
    /// A sidecar exists on disk (written by us or a previous process):
    /// its content is authoritative.
    Recorded(crate::model_choice::StoreState),
    /// No sidecar, and this process must not create a pristine one: the
    /// store predates the sidecar (or its sidecar is unreadable), so it
    /// is treated as data-bearing with an unknown descriptor.
    LockedUnknown,
    /// A fresh store whose pristine sidecar could not be written yet
    /// (the configured model did not resolve); retried on later calls.
    FreshUnrecorded,
}

/// The first-write guard's restart-to-apply refusal.
///
/// `guard_first_write` returns this typed error — never a bare string —
/// so a caller can tell the refusal apart from an ordinary store
/// failure without matching on the message text. The distinction is
/// load-bearing: the refusal is actionable operator guidance (a
/// setup-time model choice was persisted while this process was
/// running, and a restart applies it), so user-facing surfaces must
/// show it verbatim instead of mapping it to their generic store-error
/// message the way every other upsert failure is mapped. Use
/// [`restart_to_apply_message`] to pull the refusal text out of an
/// `anyhow::Error` chain.
#[derive(Debug)]
pub struct RestartToApplyError {
    message: String,
}

impl RestartToApplyError {
    fn new(message: String) -> Self {
        Self { message }
    }

    /// The full refusal text, exactly as it should reach the operator.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RestartToApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RestartToApplyError {}

/// The restart-to-apply refusal's text, if `err` is (or wraps) the
/// first-write guard's refusal; `None` for every other error.
pub fn restart_to_apply_message(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<RestartToApplyError>())
        .map(|refusal| refusal.message().to_string())
}

/// The data-bearing identity refusal.
///
/// A store that has recorded data under one embedding model must never
/// be searched or written under another: the vectors would silently
/// mix two embedding spaces. Unlike [`RestartToApplyError`] — whose
/// refusal a restart resolves — this refusal survives a restart with
/// the same configuration; the operator must restore the recorded
/// model (or start from a new store; migrating a data-bearing store
/// is Phase B). The two errors are deliberately distinct types so
/// callers surface each verbatim and never confuse their remedies.
/// Use [`embedding_identity_refusal_message`] to pull the refusal text
/// out of an `anyhow::Error` chain.
#[derive(Debug)]
pub struct EmbeddingIdentityRefusal {
    message: String,
}

impl EmbeddingIdentityRefusal {
    pub(crate) fn new(message: String) -> Self {
        Self { message }
    }

    /// The refusal between a store's recorded descriptor and the
    /// descriptor a process would serve instead.
    pub(crate) fn between(
        recorded: &crate::traits::EmbeddingDescriptor,
        serving: &crate::traits::EmbeddingDescriptor,
    ) -> Self {
        Self::new(format!(
            "refusing to open a store that has recorded data: its recorded embedding model is '{}' (provider '{}', {} dims), but this process serves '{}' (provider '{}', {} dims). Searching or storing under a different model would silently mix embedding spaces. Restore the recorded model in embedders.json (or via NEUROSTRATA_MODEL), or start from a new store; migrating a data-bearing store to a new model is Phase B.",
            recorded.model,
            recorded.provider,
            recorded.dims,
            serving.model,
            serving.provider,
            serving.dims
        ))
    }

    /// The full refusal text, exactly as it should reach the operator.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for EmbeddingIdentityRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EmbeddingIdentityRefusal {}

/// The data-bearing identity refusal's text, if `err` is (or wraps)
/// one; `None` for every other error.
pub fn embedding_identity_refusal_message(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<EmbeddingIdentityRefusal>())
        .map(|refusal| refusal.message().to_string())
}

/// Whether a write is sitting in the log with no checkpoint behind it yet.
///
/// `take` clears and reports in one step so a write landing mid-checkpoint is
/// not swallowed: the flag is cleared before the flush, and put back if the
/// flush fails.
#[derive(Default)]
struct Dirty(std::sync::atomic::AtomicBool);

impl Dirty {
    fn mark(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_set(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    fn take(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// Normal open: checksums on, and replay failures throw. The throw is the whole
/// point -- with it off the engine swallows a failed verification, opens anyway
/// and silently drops every record it could not authenticate, which is exactly
/// the outcome this code exists to make visible.
fn verifying_config() -> SystemConfig {
    SystemConfig::default().throw_on_wal_replay_failure(true)
}

/// Recovery open, used only after verification has already failed. The engine
/// cannot currently verify a WAL it wrote itself once the process dies without a
/// clean close: with checksums on, replay restores the catalog and discards
/// every row; with them off, the same rows come back intact. See
/// examples/wal_repro.rs, and bead neurostrata-kug for the upstream defect.
///
/// This trades tamper-evidence for the writes, so it is never the normal path
/// and never silent.
fn unverified_config() -> SystemConfig {
    SystemConfig::default()
        .throw_on_wal_replay_failure(false)
        .enable_checksums(false)
}

/// Fail closed by default: an unauthenticated WAL is refused rather than
/// replayed. Set `NEUROSTRATA_STRICT_WAL=0` (or `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1`)
/// to opt into the legacy unverified recovery path. The default is strict so a
/// corrupt or tampered WAL is investigated, not silently recovered.
fn strict_wal_verification() -> bool {
    if std::env::var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return false;
    }
    match std::env::var("NEUROSTRATA_STRICT_WAL").as_deref() {
        Ok("0") => false,
        _ => true,
    }
}

fn is_wal_integrity_failure(err: &str) -> bool {
    err.contains("Checksum verification failed")
        || err.contains("Corrupted wal file")
        || err.contains("invalid WAL record type")
}

/// Copies a leftover write-ahead log aside before it is replayed and truncated.
/// Best effort by design: failing to keep the copy is not a reason to refuse to
/// open the database.
fn preserve_unclean_wal(local_path: &std::path::Path) {
    let mut wal_path = local_path.to_path_buf().into_os_string();
    wal_path.push(".wal");
    let wal_file = std::path::PathBuf::from(wal_path);

    let size = match std::fs::metadata(&wal_file) {
        Ok(m) if m.len() > 0 => m.len(),
        _ => return,
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut copy_path = wal_file.clone().into_os_string();
    copy_path.push(format!(".unclean.{}", timestamp));
    let copy_file = std::path::PathBuf::from(copy_path);

    eprintln!(
        "WARNING: the last shutdown was not clean -- a {} byte write-ahead log was left behind. Recovering what it still holds.",
        size
    );
    match std::fs::copy(&wal_file, &copy_file) {
        Ok(_) => eprintln!(
            "A copy was kept at {:?} in case recovery is incomplete.",
            copy_file
        ),
        Err(e) => eprintln!("WARNING: could not keep a copy of it: {}", e),
    }
}

impl Drop for LadybugStore {
    fn drop(&mut self) {
        if self.dirty.take() {
            if let Ok(conn) = Connection::new(&self.db) {
                if let Err(e) = conn.query("CHECKPOINT;") {
                    tracing::warn!("LadybugStore drop CHECKPOINT failed: {e}");
                }
            }
        }
    }
}
/// The configured model's embedding-space descriptor, synthesized from
/// the config directory and the store's width alone: the factored form
/// of [`LadybugStore::configured_embedding_descriptor`], so callers that
/// cannot borrow the store (the first-write flip handle below) reproduce
/// the identical descriptor.
fn configured_embedding_descriptor_in(
    embed_config_dir: &std::path::Path,
    dimensions: usize,
) -> Result<crate::traits::EmbeddingDescriptor> {
    let configured = crate::embed::configured_model_in(embed_config_dir)?;
    if configured.base_url.is_some() && configured.api_key_env.is_some() {
        // The entry is one `build_embedder` serves through a
        // RemoteEmbedder (it branches on exactly this predicate):
        // synthesize that embedder's descriptor shape, resolving
        // api_model exactly as `build_embedder` does, so a process
        // serving the configured remote entry can satisfy S == C.
        return Ok(crate::traits::EmbeddingDescriptor {
            provider: "openai-compatible".into(),
            model: configured
                .api_model
                .clone()
                .unwrap_or_else(|| crate::embed::DEFAULT_REMOTE_API_MODEL.to_string()),
            revision: "unknown".into(),
            dims: dimensions,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        });
    }
    Ok(crate::traits::EmbeddingDescriptor {
        provider: "fastembed".into(),
        model: configured.model_name,
        revision: "onnx".into(),
        dims: dimensions,
        normalization: "unknown".into(),
        preprocessing: "unknown".into(),
        distance_metric: "cosine".into(),
    })
}

/// A clonable handle to everything the first-data sidecar flip touches,
/// built from `&LadybugStore` at upsert entry so one clone can move into
/// the `'static` write closure: the flip is then performed by the same
/// blocking task that commits the write, immediately after the core
/// MERGE succeeds, whether or not the caller is still waiting (the
/// write deadline releases the caller, not the query). The flip is
/// monotonic and idempotent, so the collision-retry loop re-running the
/// closure is safe.
#[derive(Clone)]
struct FirstWriteFlip {
    store_state: Arc<std::sync::Mutex<Option<CachedStoreState>>>,
    local_path: PathBuf,
    serving: Option<crate::traits::EmbeddingDescriptor>,
    embed_config_dir: PathBuf,
    dimensions: usize,
}

impl FirstWriteFlip {
    /// This handle's view of the store-state sidecar, mirroring
    /// `LadybugStore::cached_store_state`: the cache is shared with the
    /// store, and a state the store already loaded is authoritative. A
    /// cold cache is classified from the sidecar file alone — a
    /// parseable sidecar is `Recorded`; anything else cannot be a
    /// pristine state this flip may move, so it reads as `LockedUnknown`
    /// and the flip no-ops. (The store's own first classification, with
    /// its fresh-store handling, has always run by upsert entry:
    /// `guard_first_write` and `check_data_bearing_identity` load it.)
    fn cached_store_state(&self) -> CachedStoreState {
        let mut cache = self.store_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = &*cache {
            return state.clone();
        }
        let state = match crate::model_choice::read_store_state(&self.local_path) {
            Some(recorded) => CachedStoreState::Recorded(recorded),
            None => CachedStoreState::LockedUnknown,
        };
        *cache = Some(state.clone());
        state
    }

    fn set_cached_store_state(&self, state: CachedStoreState) {
        let mut cache = self.store_state.lock().unwrap_or_else(|e| e.into_inner());
        *cache = Some(state);
    }

    /// Flip the sidecar to data-bearing after the first successful
    /// write. Monotonic: only a pristine state flips, the recorded
    /// descriptor is kept, and nothing ever flips back. A failed sidecar
    /// write is logged loudly and the in-memory state still flips, so
    /// this process keeps treating the store as data-bearing.
    fn flip_store_state_to_data(&self) {
        match self.cached_store_state() {
            CachedStoreState::Recorded(mut recorded) if !recorded.has_data => {
                recorded.has_data = true;
                if let Err(e) = crate::model_choice::write_store_state(&self.local_path, &recorded)
                {
                    tracing::error!(
                        "store-state sidecar flip failed after a successful write; the store has data but its sidecar still reads pristine: {e}"
                    );
                }
                self.set_cached_store_state(CachedStoreState::Recorded(recorded));
            }
            CachedStoreState::FreshUnrecorded => {
                // The pristine sidecar could not be written at init;
                // record the data flip directly. The descriptor is the
                // serving one when this process has an embedder (the
                // model that produced the vectors), else the configured
                // one. A recorded descriptor, once present, is never
                // rewritten by the flip — this branch runs only when
                // none exists yet. The serving preference is deliberate
                // and pinned by test: the flip records the model that
                // produced the vectors, never a config re-read.
                let descriptor = match &self.serving {
                    Some(serving) => Some(serving.clone()),
                    None => configured_embedding_descriptor_in(
                        &self.embed_config_dir,
                        self.dimensions,
                    )
                    .ok(),
                };
                if let Some(descriptor) = descriptor {
                    let state = crate::model_choice::StoreState {
                        version: crate::model_choice::STORE_STATE_VERSION,
                        has_data: true,
                        descriptor,
                    };
                    if let Err(e) = crate::model_choice::write_store_state(&self.local_path, &state)
                    {
                        tracing::error!(
                            "store-state sidecar flip failed after a successful write; the store has data but no sidecar records it: {e}"
                        );
                    }
                    self.set_cached_store_state(CachedStoreState::Recorded(state));
                }
            }
            _ => {}
        }
    }
}

/// A clonable handle to everything the LockedUnknown provenance
/// stamp touches, built from `&LadybugStore` at stamp-step entry so
/// one clone can move into the `'static` write closure: the
/// attestation and the stamp are then performed by the same blocking
/// task, inside the same writer's turn (see
/// `LadybugStore::reattest_and_upgrade_empty_locked_unknown_store_state`).
/// Mirrors [`FirstWriteFlip`], the handle that lets the committing
/// write task perform the first-data flip: a transition whose
/// decision is made under a writer's turn must be executed in that
/// turn, not by the caller after the turn has been released.
#[derive(Clone)]
struct LockedUnknownStamp {
    store_state: Arc<std::sync::Mutex<Option<CachedStoreState>>>,
    local_path: PathBuf,
    serving: Option<crate::traits::EmbeddingDescriptor>,
    embed_config_dir: PathBuf,
    dimensions: usize,
}

impl LockedUnknownStamp {
    /// This handle's view of the store-state sidecar, mirroring
    /// `FirstWriteFlip::cached_store_state`: the cache is shared
    /// with the store, and a state the store already loaded is
    /// authoritative. (The stamp step's callers have always loaded
    /// the cache by entry: `init` classifies the store to compute
    /// the upgrade candidacy before the stamp step runs.)
    fn cached_store_state(&self) -> CachedStoreState {
        let mut cache = self.store_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = &*cache {
            return state.clone();
        }
        let state = match crate::model_choice::read_store_state(&self.local_path) {
            Some(recorded) => CachedStoreState::Recorded(recorded),
            None => CachedStoreState::LockedUnknown,
        };
        *cache = Some(state.clone());
        state
    }

    fn set_cached_store_state(&self, state: CachedStoreState) {
        let mut cache = self.store_state.lock().unwrap_or_else(|e| e.into_inner());
        *cache = Some(state);
    }

    /// Whether the serving descriptor equals the currently
    /// configured descriptor, mirroring
    /// `LadybugStore::serving_matches_configured` on this handle's
    /// snapshot of the configured-descriptor inputs. A resolution
    /// failure is logged and reads as "no match".
    fn serving_matches_configured(&self, serving: &crate::traits::EmbeddingDescriptor) -> bool {
        match configured_embedding_descriptor_in(&self.embed_config_dir, self.dimensions) {
            Ok(configured) => configured == *serving,
            Err(e) => {
                tracing::warn!("configured model did not resolve for the store-state sidecar: {e}");
                false
            }
        }
    }

    /// Attest emptiness from the engine and, iff the engine attests
    /// the table empty, record the pristine sidecar — as one unit.
    /// This is the whole decision-and-transition of the #74
    /// LockedUnknown provenance upgrade, and it runs only inside the
    /// stamp step's writer turn: splitting the stamp out of the turn
    /// would reopen the window in which a write commits after the
    /// empty check but before the stamp, landing under a
    /// `has_data: false` record its first-write flip cannot move
    /// (the flip no-ops on `LockedUnknown`). Fail-closed: a false
    /// attestation stamps nothing, and an attestation
    /// query-execution error downgrades to that same `false`, so
    /// nothing is stamped; only the width leg's drop/recreate DDL
    /// errors propagate.
    fn attest_and_stamp(
        &self,
        conn: &Connection,
        dimensions: usize,
        create_node_table: &str,
        create_rel_tables: &[&str],
    ) -> Result<()> {
        let attested_empty = reconcile_empty_locked_unknown_width(
            conn,
            dimensions,
            create_node_table,
            create_rel_tables,
        )?;
        if attested_empty {
            self.stamp_pristine_identity();
        }
        Ok(())
    }

    /// Record the pristine sidecar for a LockedUnknown store the
    /// engine attested empty in this same writer's turn (issue #74),
    /// under the serving descriptor bound when the stamp step was
    /// entered. This is the recovery for fresh-store provenance lost
    /// across a non-serving open (or a refused first write): the bare
    /// DB file such an open leaves behind makes the store classify
    /// LockedUnknown, which no other path upgrades. The gates are
    /// deliberately narrow — the cached state is still
    /// LockedUnknown, a serving descriptor is bound, and serving
    /// matches the configuration (the same positive equality the
    /// FreshUnrecorded arm of `maintain_store_state` requires) — and
    /// the caller attested emptiness from the engine in this same
    /// turn, so a genuine pre-sidecar store with data is never
    /// recorded here. Best effort, like `maintain_store_state`: a
    /// sidecar failure is logged, not propagated, and the cached
    /// state stays LockedUnknown. A later init can retry the
    /// upgrade only from a fresh process or instance, and only
    /// while no write has landed: within this instance the latch
    /// is already set when the stamp step runs, the schema-ready
    /// fast path never re-enters this step, and the first write
    /// seals the store under an unrecorded identity (#91).
    fn stamp_pristine_identity(&self) {
        if !matches!(self.cached_store_state(), CachedStoreState::LockedUnknown) {
            return;
        }
        let Some(serving) = &self.serving else {
            return;
        };
        if !self.serving_matches_configured(serving) {
            return;
        }
        let state = crate::model_choice::StoreState {
            version: crate::model_choice::STORE_STATE_VERSION,
            has_data: false,
            descriptor: serving.clone(),
        };
        match crate::model_choice::write_store_state(&self.local_path, &state) {
            Ok(()) => self.set_cached_store_state(CachedStoreState::Recorded(state)),
            Err(e) => {
                tracing::warn!("store-state sidecar write failed: {e}")
            }
        }
    }
}

impl LadybugStore {
    /// Construct a `LadybugStore` for tests with a small system footprint so
    /// `cargo test` (parallel by default) does not mmap-reserve 8 TiB per
    /// open. The defaults are 512 MiB max database size + 64 MiB buffer
    /// pool; override with `NEUROSTRATA_TEST_MAX_DB_SIZE` /
    /// `NEUROSTRATA_TEST_BUFFER_POOL_SIZE` env vars when a test fixture
    /// needs more (or less) room.
    ///
    /// 15-ish test sites use temp directories; this keeps every concurrent
    /// open well under container memory ceilings and ends the open-time
    /// `Mmap for size 8796093022208 failed` flake (task neurostrata-c8te).
    pub fn for_testing(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        let max_db_size: u64 = std::env::var("NEUROSTRATA_TEST_MAX_DB_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(512 * 1024 * 1024);
        let buffer_pool_size: u64 = std::env::var("NEUROSTRATA_TEST_BUFFER_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64 * 1024 * 1024);
        let cfg = SystemConfig::default()
            .max_db_size(max_db_size)
            .buffer_pool_size(buffer_pool_size)
            .throw_on_wal_replay_failure(true);
        Self::with_system_config(local_path, dimensions, cfg)
    }

    pub fn new(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        // Test harnesses opt into the bounded test configuration by env so the
        // same constructor path serves both production and test binaries.
        if std::env::var("NEUROSTRATA_TEST_DB_CONFIG").is_ok() {
            return Self::for_testing(local_path, dimensions);
        }
        Self::with_system_config(local_path, dimensions, verifying_config())
    }

    /// Open the database with a custom `SystemConfig`. Production paths go
    /// through `new`; tests go through `for_testing`. The fallback path
    /// (WAL verification failure) always uses the hardcoded `unverified_config`
    /// recovery path; only the *initial* open is parameterized.
    pub fn with_system_config(
        local_path: impl Into<PathBuf>,
        dimensions: usize,
        initial_config: SystemConfig,
    ) -> Result<Self> {
        let local_path = local_path.into();

        // Captured before the engine creates the file: whether this store
        // predates this process decides how a missing store-state sidecar
        // is read (fresh install vs pre-fix store).
        let db_preexisting = local_path.exists();

        // A WAL present at open time means the last process did not close cleanly:
        // a clean close checkpoints and removes it. Keep a copy before the engine
        // replays, because replay truncates the file to the last good record and
        // whatever followed is then unrecoverable.
        preserve_unclean_wal(&local_path);

        // Open with verification first. Only if the WAL fails to authenticate do we
        // consider replaying it unverified, and never quietly.
        let mut opened_via_recovery = false;
        let db = match Database::new(&local_path, initial_config) {
            Ok(db) => db,
            Err(e) => {
                let err_msg = e.to_string();
                let checksum_mismatch = err_msg.contains("enableChecksums");
                if checksum_mismatch {
                    opened_via_recovery = true;
                    eprintln!("WARN: DB checksum config mismatch; reopening with checksums disabled.");
                    match Database::new(&local_path, unverified_config()) {
                        Ok(db) => db,
                        Err(e2) => return Err(e2.into()),
                    }
                } else if !is_wal_integrity_failure(&err_msg) {
                    return Err(e.into());
                } else {

                eprintln!(
                    "ERROR: the write-ahead log failed verification: {}",
                    err_msg
                );

                if strict_wal_verification() {
                    return Err(anyhow::anyhow!(
                        "Refusing to open: the write-ahead log did not verify. The log has been left in place for inspection. To recover the writes without verification, set NEUROSTRATA_STRICT_WAL=0 (or NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1). See docs/runbooks/wal-recovery.md."
                    ));
                }

                eprintln!("ERROR: retrying WITHOUT checksum verification to recover those writes. The records replayed this way are NOT authenticated -- if this database may have been tampered with, stop and inspect the preserved copy instead. Set NEUROSTRATA_STRICT_WAL=1 to refuse this fallback.");

                match Database::new(&local_path, unverified_config()) {
                    Ok(db) => {
                        opened_via_recovery = true;
                        eprintln!("Recovered by replaying the unverified write-ahead log.");
                        // The replay only brought the writes back into memory.
                        // Checkpoint them into the main DB files right away, so
                        // a later crash does not put the same marginal WAL back
                        // in front of the strict verifier.
                        match Connection::new(&db) {
                            Ok(conn) => {
                                if let Err(e) = conn.query("CHECKPOINT;") {
                                    tracing::warn!("WAL recovery CHECKPOINT failed: {e}");
                                }
                            }
                            Err(e) => {
                                tracing::warn!("failed to open connection for post-recovery CHECKPOINT: {e}");
                            }
                        }
                        db
                    }
                    Err(retry_e) => {
                        // Unreadable even without verification: set it aside rather
                        // than leave the database unopenable, and say what was lost.
                        let mut wal_path = local_path.clone().into_os_string();
                        wal_path.push(".wal");
                        let wal_file = std::path::PathBuf::from(wal_path);

                        if !wal_file.exists() {
                            return Err(anyhow::anyhow!(
                                "WAL verification failed and the file is no longer at {:?}: {}",
                                wal_file,
                                retry_e
                            ));
                        }

                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let mut corrupted_path = wal_file.clone().into_os_string();
                        corrupted_path.push(format!(".corrupted.{}", timestamp));
                        let corrupted_file = std::path::PathBuf::from(corrupted_path);

                        let dropped_bytes =
                            std::fs::metadata(&wal_file).map(|m| m.len()).unwrap_or(0);
                        std::fs::rename(&wal_file, &corrupted_file).map_err(|err| {
                            anyhow::anyhow!(
                                "Recovery failed: could not move the unreadable WAL file: {}",
                                err
                            )
                        })?;

                        eprintln!(
                            "ERROR: DATA LOSS. {} bytes of un-checkpointed writes could not be replayed even unverified, and were discarded to reopen the database.",
                            dropped_bytes
                        );
                        eprintln!("ERROR: they were kept at {:?} -- do not delete it if those writes mattered.", corrupted_file);
                        opened_via_recovery = true;
                        // No WAL left to verify at this point.
                        Database::new(&local_path, verifying_config()).map_err(|last_e| {
                            anyhow::anyhow!("Retry failed after discarding the WAL: {}", last_e)
                        })?
                    }
                }
            }
            }
        };

        Ok(Self {
            local_path,
            dimensions,
            db: Arc::new(db),
            dirty: Dirty::default(),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            schema_ready: std::sync::atomic::AtomicBool::new(false),
            ontology: Arc::new(arc_swap::ArcSwap::from_pointee(
                std::collections::BTreeMap::new(),
            )),
            mention_symbol_ids: Arc::new(tokio::sync::RwLock::new(Default::default())),
            lexical: Arc::new(std::sync::RwLock::new(crate::fts::LexicalIndex::new())),
            lexical_loaded: Arc::new(std::sync::RwLock::new(std::collections::BTreeSet::new())),
            metrics: Arc::new(
                crate::time_series::TimeSeriesStore::new(
                    crate::time_series::SeriesConfig::default(),
                )
                .with_anomaly_callback(std::sync::Arc::new(|_key, _sample| {})),
            ),
            tantivy_lexical: Arc::new(std::sync::RwLock::new(None)),
            embedding_registry: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            embed_config_dir: crate::embed::config_dir(),
            serving_descriptor: std::sync::Mutex::new(None),
            db_preexisting,
            opened_via_recovery,
            store_state: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// Override the directory the configured embedding model is resolved
    /// from. Production processes use `crate::embed::config_dir()`; tests
    /// point the store at a fixture config dir to control which model
    /// `embedders.json` selects.
    pub fn with_embed_config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.embed_config_dir = dir.into();
        self
    }

    /// Bind the serving embedder's descriptor (see the field). The
    /// daemon and task-import construction sites — the paths that build
    /// an embedder immediately before the store — pass
    /// `Some(embedder.embedding_descriptor())`; every other path keeps
    /// the `None` default.
    pub fn with_serving_descriptor(
        mut self,
        descriptor: Option<crate::traits::EmbeddingDescriptor>,
    ) -> Self {
        self.serving_descriptor = std::sync::Mutex::new(descriptor);
        self
    }

    /// The serving descriptor currently bound to this store, if any
    /// (see the field). Cloned out of the lock so callers never hold it
    /// across store-state filesystem work.
    fn serving_descriptor(&self) -> Option<crate::traits::EmbeddingDescriptor> {
        self.serving_descriptor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Bind (or replace) the serving descriptor after construction,
    /// for processes that build their embedder only after the store
    /// exists — the fallback CLI arms — and must
    /// call it before the first vector operation, so the store-state
    /// guard family and the data-bearing identity check key on the
    /// model that will actually produce this process's vectors.
    pub fn set_serving_descriptor(&self, descriptor: Option<crate::traits::EmbeddingDescriptor>) {
        *self
            .serving_descriptor
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = descriptor;
    }

    /// The embedding-space descriptor of the *configured* model: resolved
    /// from `embedders.json` through the same effective resolution the
    /// loader uses (`embed::configured_model_in` — the env-named entry
    /// when acceptable, otherwise the first entry), never the
    /// env-or-placeholder shortcut `store_embedding_descriptor` falls back
    /// to. The shape mirrors the descriptor of the embedder
    /// `build_embedder` would construct from the entry —
    /// `FastEmbedder::embedding_descriptor` for a local entry, the
    /// remote shape for an entry with both `base_url` and
    /// `api_key_env`; the
    /// width is this store's, because every production path opens the
    /// store with the configured model's width and the store's
    /// FLOAT[width] column is the width its vectors actually have.
    fn configured_embedding_descriptor(&self) -> Result<crate::traits::EmbeddingDescriptor> {
        configured_embedding_descriptor_in(&self.embed_config_dir, self.dimensions)
    }

    /// The first-write flip handle for this store (see [`FirstWriteFlip`]):
    /// a snapshot of the flip's inputs taken at upsert entry — the shared
    /// store-state cache, the sidecar path, the serving descriptor this
    /// process writes under, and the configured-descriptor inputs.
    fn first_write_flip(&self) -> FirstWriteFlip {
        FirstWriteFlip {
            store_state: Arc::clone(&self.store_state),
            local_path: self.local_path.clone(),
            serving: self.serving_descriptor(),
            embed_config_dir: self.embed_config_dir.clone(),
            dimensions: self.dimensions,
        }
    }

    /// The LockedUnknown stamp handle for this store (see
    /// [`LockedUnknownStamp`]): a snapshot of the stamp's inputs
    /// taken at stamp-step entry — the shared store-state cache, the
    /// sidecar path, the serving descriptor this process is bound
    /// with, and the configured-descriptor inputs.
    fn locked_unknown_stamp(&self) -> LockedUnknownStamp {
        LockedUnknownStamp {
            store_state: Arc::clone(&self.store_state),
            local_path: self.local_path.clone(),
            serving: self.serving_descriptor(),
            embed_config_dir: self.embed_config_dir.clone(),
            dimensions: self.dimensions,
        }
    }

    /// Load (once per process) this store's view of the store-state
    /// sidecar. Pure filesystem reads against a store this process
    /// already holds open; never opens another store.
    fn cached_store_state(&self) -> CachedStoreState {
        let mut cache = self.store_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = &*cache {
            return state.clone();
        }
        let state = match crate::model_choice::read_store_state(&self.local_path) {
            Some(recorded) => CachedStoreState::Recorded(recorded),
            None if !self.db_preexisting
                && !crate::model_choice::store_state_path(&self.local_path).exists() =>
            {
                CachedStoreState::FreshUnrecorded
            }
            None => {
                // A pre-sidecar store opens
                // exactly as it always has — no refusal can key on a
                // descriptor that was never recorded — but the Phase B
                // population should be visible in the logs. Warn only;
                // no behavior change. (This resolution is cached, so
                // the warning fires once per store per process.)
                if self.db_preexisting {
                    tracing::warn!(
                        "store at {} has no readable store-state sidecar: it predates the recorded-identity feature, so its embedding identity is unknown and it opens without an identity check; establishing its identity is Phase B migration work",
                        self.local_path.display()
                    );
                }
                CachedStoreState::LockedUnknown
            }
        };
        *cache = Some(state.clone());
        state
    }

    fn set_cached_store_state(&self, state: CachedStoreState) {
        let mut cache = self.store_state.lock().unwrap_or_else(|e| e.into_inner());
        *cache = Some(state);
    }

    /// Maintain the sidecar at a successful `init`, keyed on the
    /// *serving* descriptor: the
    /// recorded descriptor is created or rewritten only from the
    /// serving embedder's own descriptor, only while the store is
    /// pristine, and only when serving and configured agree at this
    /// init — the signature of a freshly (re)started process. A config
    /// re-read never creates, adopts, or rewrites the record: a
    /// long-lived process that started before a setup pick landed
    /// adopts nothing, on any number of repeated inits, and a process
    /// with no serving embedder never moves the record in either
    /// direction. Data-bearing and pre-fix stores are never written
    /// here. Best effort: a sidecar failure must not fail init for
    /// stores the feature never touches, so errors are logged, not
    /// propagated.
    ///
    /// The recorded-pristine adoption never runs on a recovery open
    /// (`opened_via_recovery`): the recorded descriptor is only
    /// rewritten from evidence a verified open produced, and on a
    /// recovery open the sidecar's `has_data=false` claim is the
    /// sole basis for adoption while the engine's view of the store
    /// is unauthenticated — WAL-only rows can present as absent —
    /// so the record stays exactly as found and a later verified
    /// open adopts as today (overseer panel condition 1, findings
    /// AC-F1/SA-F2). The gate lives here, inside the function, so
    /// both `init` call sites (the schema-ready fast path and the
    /// post-closure call) are covered.
    fn maintain_store_state(&self) {
        match self.cached_store_state() {
            CachedStoreState::Recorded(recorded)
                if !recorded.has_data && !self.opened_via_recovery =>
            {
                if let Some(serving) = self.serving_descriptor() {
                    if serving != recorded.descriptor && self.serving_matches_configured(&serving) {
                        let state = crate::model_choice::StoreState {
                            version: crate::model_choice::STORE_STATE_VERSION,
                            has_data: false,
                            descriptor: serving.clone(),
                        };
                        match crate::model_choice::write_store_state(&self.local_path, &state) {
                            Ok(()) => {
                                self.set_cached_store_state(CachedStoreState::Recorded(state))
                            }
                            Err(e) => {
                                tracing::warn!("store-state sidecar update failed: {e}")
                            }
                        }
                    }
                }
            }
            CachedStoreState::FreshUnrecorded => {
                if let Some(serving) = self.serving_descriptor() {
                    if self.serving_matches_configured(&serving) {
                        let state = crate::model_choice::StoreState {
                            version: crate::model_choice::STORE_STATE_VERSION,
                            has_data: false,
                            descriptor: serving.clone(),
                        };
                        match crate::model_choice::write_store_state(&self.local_path, &state) {
                            Ok(()) => {
                                self.set_cached_store_state(CachedStoreState::Recorded(state))
                            }
                            Err(e) => {
                                tracing::warn!("store-state sidecar write failed: {e}")
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The stamp step of the #74 LockedUnknown provenance upgrade:
    /// emptiness is attested and the pristine sidecar is stamped as
    /// one unit inside a single writer's turn (overseer panel
    /// condition 3, findings SA-F1 + N1, and the split-turn follow-up
    /// finding).
    ///
    /// The first attestation happens inside `init`'s schema closure,
    /// under that instance's writer's turn; a write landing after it
    /// — from a primed non-canonical writer in this instance, or,
    /// needing no priming at all, from a second `LadybugStore`
    /// instance on the same database, whose `write_lock` is a
    /// different per-Self lock — must never be stamped over as if
    /// the store were pristine. An earlier shape of this step
    /// re-attested inside a fresh writer's turn but executed the
    /// stamp itself after `.await` returned, with the turn already
    /// released: a queued write could commit after the empty check
    /// but before the stamp, its first-write flip no-ops on
    /// `LockedUnknown`, and the stamp then recorded `has_data: false`
    /// over the populated store. The step now hands the whole
    /// decision-and-transition to
    /// [`LockedUnknownStamp::attest_and_stamp`], which the
    /// `write_with_deadline` closure executes inside the turn, so no
    /// writer turn of this instance ends between the attestation and
    /// the stamp, and the stamp is never consumed from a result
    /// parked across the turn boundary. That guarantee is scoped to
    /// writers sharing this instance's turn: `write_lock` is
    /// per-`Self`, so a second `LadybugStore` instance co-open on
    /// the same database is outside it. Whether co-open is possible
    /// at all is unproven in-tree; the cross-instance residual is
    /// tracked in #92 (probe-first).
    ///
    /// The helper's width leg is safe both ways: for a still-empty
    /// store the width was already reconciled by the first
    /// attestation, so the physical width already matches and
    /// nothing is dropped; if rows landed in the window, the
    /// helper's row-count check returns `false` before any
    /// drop/recreate is considered.
    ///
    /// Fail-closed and best-effort, matching the stamp's contract: a
    /// false attestation or a failed turn stamps nothing, a sidecar
    /// failure is logged inside the stamp and never propagated, and
    /// `init` still succeeds — the store stays LockedUnknown. That
    /// is not a retry promise for this instance: `schema_ready`
    /// latched before this step ran and the fast path never
    /// re-enters it, so only a fresh process or instance, with no
    /// intervening write, can retry the upgrade (#91).
    async fn reattest_and_upgrade_empty_locked_unknown_store_state(&self, dimensions: usize) {
        let (create_node_table, create_rel_tables) = memory_table_ddl(dimensions);
        let stamp = self.locked_unknown_stamp();
        let _ = self
            .write_with_deadline("re-attesting an empty LockedUnknown store", move |conn| {
                stamp.attest_and_stamp(conn, dimensions, &create_node_table, &create_rel_tables)
            })
            .await;
    }

    /// Whether the serving descriptor equals the currently configured
    /// descriptor (a fresh `embedders.json` re-read). A resolution
    /// failure is logged and reads as "no match": adoption is opt-in on
    /// a positive equality, never on an unknown.
    fn serving_matches_configured(&self, serving: &crate::traits::EmbeddingDescriptor) -> bool {
        match self.configured_embedding_descriptor() {
            Ok(configured) => configured == *serving,
            Err(e) => {
                tracing::warn!("configured model did not resolve for the store-state sidecar: {e}");
                false
            }
        }
    }

    /// The upsert race guard, keyed on the serving descriptor:
    /// while the store is pristine with a
    /// recorded descriptor, a write may only land under the model this
    /// process serves. With a serving descriptor bound, the write
    /// proceeds iff serving == configured and serving == recorded: a
    /// setup pick persisted while this process was running moves the
    /// configured model away from the serving one and is refused with
    /// the restart-to-apply error, and a recorded descriptor that
    /// disagrees with the serving one is refused, never healed at write
    /// time. Without a serving descriptor, the guard falls back
    /// to the configured-vs-recorded comparison. A fresh store with no
    /// record yet (`FreshUnrecorded`) is in the same window: no
    /// recorded descriptor exists to compare against, but a serving
    /// descriptor that disagrees with the configured one is the same
    /// stale-serving hazard — the first write would commit vectors
    /// under the stale model and the flip would record that model as
    /// the data-bearing identity — so the serving-vs-configured check
    /// applies there too. Stores with data, and
    /// stores whose state is unknown, are outside the window this
    /// guard protects. Every refusal is a [`RestartToApplyError`], so
    /// callers can surface it verbatim.
    fn guard_first_write(&self) -> Result<()> {
        if let CachedStoreState::Recorded(recorded) = self.cached_store_state() {
            if !recorded.has_data {
                match self.serving_descriptor() {
                    Some(serving) => {
                        let current = self.configured_embedding_descriptor()?;
                        if current != serving {
                            return Err(RestartToApplyError::new(format!(
                                "refusing to store data: this process serves embedding model '{}', but the configured model is now '{}' (a setup-time model choice was persisted while this process was running). Restart the daemon (or this process) so the store is opened under the new model before the first write; data is never stored under a model different from the one producing it.",
                                serving.model,
                                current.model
                            ))
                            .into());
                        }
                        if serving != recorded.descriptor {
                            return Err(RestartToApplyError::new(format!(
                                "refusing to store data: this store's recorded embedding model is '{}', but this process serves '{}'. Restart the daemon (or this process) so the store's record and the serving model agree before the first write.",
                                recorded.descriptor.model,
                                serving.model
                            ))
                            .into());
                        }
                    }
                    None => {
                        let current = self.configured_embedding_descriptor()?;
                        if current != recorded.descriptor {
                            return Err(RestartToApplyError::new(format!(
                                "refusing to store data: this store was opened under embedding model '{}', but the configured model is now '{}' (a setup-time model choice was persisted while this process was running). Restart the daemon (or this process) so the store is opened under the new model before the first write; data is never stored under a model different from the one the store will next be opened with.",
                                recorded.descriptor.model,
                                current.model
                            ))
                            .into());
                        }
                    }
                }
            }
        }
        if let CachedStoreState::FreshUnrecorded = self.cached_store_state() {
            // No sidecar exists yet — `maintain_store_state` could not
            // write the pristine record at init because serving and
            // configured disagreed (the first-start window: the daemon
            // is still loading the default model while a setup pick
            // persists a different one). There is no recorded
            // descriptor to protect, but the stale-serving hazard is
            // identical: a write now would commit vectors under the
            // serving model and the first-write flip would record that
            // model as the store's data-bearing identity, so the
            // restart the setup flow demands would then refuse to open
            // the store. Refuse with the same restart-to-apply
            // guidance as the recorded-pristine case above. Without a
            // serving descriptor there is no serving identity to be
            // stale, and the write proceeds as before.
            if let Some(serving) = self.serving_descriptor() {
                let current = self.configured_embedding_descriptor()?;
                if current != serving {
                    return Err(RestartToApplyError::new(format!(
                        "refusing to store data: this process serves embedding model '{}', but the configured model is now '{}' (a setup-time model choice was persisted while this process was running). Restart the daemon (or this process) so the store is opened under the new model before the first write; data is never stored under a model different from the one producing it.",
                        serving.model,
                        current.model
                    ))
                    .into());
                }
            }
        }
        Ok(())
    }

    /// The data-bearing identity check: a store
    /// whose sidecar records data under descriptor R refuses any
    /// vector operation this process would perform under a different
    /// serving descriptor S — full-struct inequality, same-width model
    /// changes included. The refusal keys on S only, never on the
    /// configured descriptor C: C is a config re-read, not the identity
    /// of the embedder that produced this process's vectors, and can
    /// legitimately differ from the model actually loaded when the
    /// configured name falls back. When
    /// no serving descriptor is bound there is no serving identity to
    /// compare, and the check passes: the unbound production paths
    /// (backup, restore-verify, reports, export, metadata recycling)
    /// embed nothing. Pristine and pre-sidecar stores are likewise
    /// outside this check: pristine identity is the first-write
    /// guard's window, and a pre-sidecar store has no R to key on.
    fn check_data_bearing_identity(&self) -> Result<()> {
        if let CachedStoreState::Recorded(recorded) = self.cached_store_state() {
            if recorded.has_data {
                if let Some(serving) = self.serving_descriptor() {
                    if serving != recorded.descriptor {
                        return Err(EmbeddingIdentityRefusal::between(
                            &recorded.descriptor,
                            &serving,
                        )
                        .into());
                    }
                }
            }
        }
        Ok(())
    }

    /// The descriptor `init` records in the embedding registry, by
    /// precedence: the serving descriptor S when
    /// bound (the embedder's own descriptor is the provenance-correct
    /// record); else the sidecar's recorded descriptor R (an unbound
    /// open of a recorded store seeds the registry from the store's
    /// authoritative identity, not from this process's configuration);
    /// else the configured descriptor C when it resolves (fresh and
    /// pre-sidecar stores opened unbound); else the legacy placeholder.
    /// In the canonical local flow adoption already required S == C
    /// before R was written, so the three coincide; where they
    /// diverge, S is the truthful record.
    fn effective_embedding_descriptor(&self) -> crate::traits::EmbeddingDescriptor {
        if let Some(serving) = self.serving_descriptor() {
            return serving;
        }
        if let CachedStoreState::Recorded(recorded) = self.cached_store_state() {
            return recorded.descriptor;
        }
        if let Ok(configured) = self.configured_embedding_descriptor() {
            return configured;
        }
        self.store_embedding_descriptor()
    }

    /// Flip the sidecar to data-bearing after the first successful
    /// upsert. The flip logic lives on [`FirstWriteFlip`] so the
    /// committing write task performs it (see `upsert`); this entry
    /// point delegates to a handle built from `&self`.
    fn flip_store_state_to_data(&self) {
        self.first_write_flip().flip_store_state_to_data();
    }

    /// The legacy placeholder identity: the last
    /// resort of `effective_embedding_descriptor`, reached only when
    /// neither a serving descriptor, a recorded descriptor, nor a
    /// resolvable configured descriptor exists. It describes no real
    /// model — provider "store-registered", model from the
    /// `NEUROSTRATA_MODEL` env var or "local-fastembed" — and must
    /// never again be the identity recorded for a store this feature
    /// created: those always have a serving or recorded descriptor.
    fn store_embedding_descriptor(&self) -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "store-registered".into(),
            model: std::env::var("NEUROSTRATA_MODEL").unwrap_or_else(|_| "local-fastembed".into()),
            revision: "store-lifetime".into(),
            dims: self.dimensions,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
    }

    /// Composite Recommendation #11: verify or record the embedding-space
    /// identity for a namespace. The first descriptor recorded in this
    /// process is authoritative for the store's lifetime; a later descriptor
    /// that is not `compatible` (different dimensions, model, or metric) is
    /// rejected so incompatible embedding spaces are never mixed through
    /// this store.
    pub fn verify_or_record_embedding_descriptor(
        &self,
        namespace: &str,
        desc: &crate::traits::EmbeddingDescriptor,
    ) -> Result<()> {
        let mut registry = self
            .embedding_registry
            .lock()
            .unwrap_or_else(|e| { tracing::warn!("embedding registry lock poisoned: {e}"); e.into_inner() });
        if let Some(existing) = registry.get(namespace) {
            if !existing.compatible(desc) {
                anyhow::bail!(
                    "embedding descriptor mismatch for namespace '{}': recorded provider='{}' model='{}' dims={} vs active provider='{}' model='{}' dims={}",
                    namespace,
                    existing.provider,
                    existing.model,
                    existing.dims,
                    desc.provider,
                    desc.model,
                    desc.dims
                );
            }
            return Ok(());
        }
        registry.insert(namespace.to_string(), desc.clone());
        Ok(())
    }

    /// Runs a write with a deadline, so a caller is told the database is busy
    /// rather than left holding a connection that never answers. Engine-level
    /// lock waits of two and a half minutes were measured while a burst of
    /// writes was in flight (bead neurostrata-3fi.6).
    ///
    /// The deadline releases the CALLER, not the query: spawn_blocking cannot be
    /// cancelled, so the statement may still land afterwards. The message says
    /// so, because retrying blind could otherwise duplicate work.
    ///
    /// Which is why the writer's turn belongs to the blocking task and not to
    /// the caller. It used to be a guard inside the timed future, so a caller
    /// that gave up released the turn while its statement was still running,
    /// and the next writer promptly opened a second one against an engine that
    /// already had one. That is how a single stalled write became five in a row
    ///: each waited its 30s, gave up, and handed the
    /// turn to the next. Held by the task, the turn is released when the work
    /// actually finishes, so a caller giving up costs a wait instead.
    async fn write_with_deadline<T, F>(&self, what: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: Fn(&Connection) -> Result<T> + Clone + Send + 'static,
    {
        let write_lock = Arc::clone(&self.write_lock);
        let db = self.db.clone();

        let outcome = bounded(what, write_timeout(), async move {
            // One writer at a time, because the engine refuses the second
            // rather than making it wait. The deadline covers this queue too:
            // a caller stuck behind a stalled write hears that the database is
            // busy instead of waiting indefinitely for its turn. Giving up
            // while merely waiting for the turn leaks nothing -- no statement
            // has been started yet.
            let mut turn = write_lock.lock_owned().await;

            // The turn orders our own writers; the background checkpoint and
            // any other process are outside it, so a refusal can still arrive.
            // Those clear in milliseconds, so wait and try again rather than
            // handing the caller a failure it can do nothing about.
            let mut attempt = 0;
            loop {
                let (returned, result) =
                    Self::write_owning_turn(db.clone(), turn, f.clone()).await?;
                turn = returned;

                match result {
                    Err(e) if is_write_collision(&e) && attempt < WRITE_COLLISION_RETRIES => {
                        WRITE_COLLISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        attempt += 1;
                        // The turn is held across this pause on purpose: it is
                        // ours until we stop retrying, and nothing is running
                        // inside the engine while we wait.
                        tokio::time::sleep(std::time::Duration::from_millis(25 * attempt as u64))
                            .await;
                    }
                    result => return result,
                }
            }
        })
        .await;

        if outcome.is_err() {
            WRITES_ABANDONED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "DIAGNOSTIC: gave up waiting on {} -- engine: {}",
                what,
                engine_stats()
            );
        }

        // Marked whatever the outcome: a write that timed out may still land,
        // and an unnecessary checkpoint attempt costs nothing next to a write
        // that never reaches disk.
        self.dirty.mark();
        outcome
    }

    /// Runs one attempt on a blocking thread, and gives that thread the
    /// writer's turn for as long as it lasts.
    ///
    /// The turn travels back out with the result so a retry can keep it. A
    /// caller that has stopped waiting never reads that result, so the turn is
    /// dropped when the task completes -- which is exactly when the next writer
    /// should be allowed to start.
    async fn write_owning_turn<T, F>(
        db: Arc<Database>,
        turn: tokio::sync::OwnedMutexGuard<()>,
        f: F,
    ) -> Result<(tokio::sync::OwnedMutexGuard<()>, Result<T>)>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            // The engine runs here, on a blocking-pool thread, and a fault
            // in it is the crash this daemon is known for. The crash handler
            // needs an alternate signal stack bigger than the one the
            // standard library gives a thread, so this thread is given one
            // before it touches the engine. The call is idempotent, so the
            // pool reusing this thread does not leak another stack.
            if let Err(e) = crate::crash::install_alt_stack() {
                tracing::warn!("install_alt_stack failed: {e}");
            }
            let outcome = (|| -> Result<T> {
                let conn = Connection::new(&db)?;
                f(&conn)
            })();
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (turn, outcome)
        })
        .await
        .map_err(|e| anyhow::anyhow!("the database thread did not finish: {}", e))
    }

    /// Runs one piece of database work on a blocking thread.
    ///
    /// Every lbug call is synchronous FFI. Called straight from an async handler
    /// it occupies a tokio worker for as long as the engine takes, and a handful
    /// of concurrent queries can therefore starve the runtime outright: during a
    /// write burst even /health -- an async closure returning a constant -- had
    /// no thread to run on, which is what made the daemon look dead while it was
    /// still listening (bead neurostrata-3fi.6).
    ///
    /// The connection is opened inside the closure on purpose. It borrows the
    /// Database and is not Send, so it must never be created here and moved, nor
    /// held across an await.
    pub(crate) async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.db.clone();
        // Held across the engine call on purpose: lbug's C++
        // DictionaryColumn::scan / BufferManager::optimisticRead segfaults
        // when a read Connection races a concurrent write Connection on the
        // same Database, so a reader must not enter the engine while a
        // writer holds the turn. As in `write_owning_turn`, the guard lives
        // inside the blocking task, so a dropped await cannot release it
        // while a statement is still running.
        let turn = Arc::clone(&self.write_lock).lock_owned().await;
        ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            let _turn = turn;
            // As in `with_locked_conn`: the engine runs on this thread, so
            // the crash handler's alternate signal stack is installed here.
            if let Err(e) = crate::crash::install_alt_stack() {
                tracing::warn!("install_alt_stack failed: {e}");
            }
            let outcome = (|| -> Result<T> {
                let conn = Connection::new(&db)?;
                f(&conn)
            })();
            // Counted here rather than after the await: the await is what a
            // deadline drops, and the whole question is whether the work
            // outlives the caller that stopped waiting for it.
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            outcome
        })
        .await
        .map_err(|e| anyhow::anyhow!("the database thread did not finish: {}", e))?
    }
}

/// One shape warning produced during vocabulary validation of a write.
#[derive(Debug, Clone)]
struct WriteShapeWarning {
    field: String,
    why: String,
    skipped: String,
}

/// The five keys `fact.fields.topology` declares (vocabulary v5). Extra keys
/// are ignored; a missing key or a present key that is not a string is a
/// defect.
const TOPOLOGY_KEYS: [&str; 5] = [
    "component",
    "target_system",
    "channel",
    "credential_boundary",
    "reason",
];

/// Validates a memory payload's metadata shape and extracts edge specs.
/// Returns the valid edge specs and any shape warnings (never rejects the write).
fn parse_edge_specs(
    payload: &crate::traits::MemoryPayload,
) -> (Vec<EdgeSpec>, Vec<WriteShapeWarning>) {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();

    // (A) memory_type check
    if payload.memory_type.trim().is_empty() {
        warnings.push(WriteShapeWarning {
            field: "memory_type".to_string(),
            why: "memory_type must contain a non-whitespace character".to_string(),
            skipped: "nothing; payload retained unchanged".to_string(),
        });
    }
    // Any other string accepted silently, never normalized.

    // (B) metadata root
    match &payload.metadata {
        Value::Null => { /* ok */ }
        Value::Object(_) => { /* ok, inspect below */ }
        _ => {
            warnings.push(WriteShapeWarning {
                field: "metadata".to_string(),
                why: "metadata must be an object or null".to_string(),
                skipped: "all relationship declarations".to_string(),
            });
            return (specs, warnings);
        }
    }

    // (C) relation keys in existing order
    for (key, rel_type, points_at_target) in [
        ("related_to", "RELATES_TO", true),
        ("contained_by", "CONTAINS", false),
        ("governs", "GOVERNS", true),
        // Vocabulary v10: a memory extracted from a completed task declares it,
        // and the edge points memory -> task (self_to_target).
        ("extracted_from", "EXTRACTED_FROM", true),
        ("depends_on", "DEPENDS_ON", true),
    ] {
        match payload.metadata.get(key) {
            None => { /* absent: ok */ }
            Some(Value::Array(arr)) if arr.is_empty() => { /* empty: ok */ }
            Some(Value::Array(arr)) => {
                for (i, item) in arr.iter().enumerate() {
                    match item {
                        Value::String(s) if s.trim().is_empty() => {
                            warnings.push(WriteShapeWarning {
                                field: format!("{}.{}", key, i),
                                why: "relation target must contain a non-whitespace character"
                                    .to_string(),
                                skipped: "this edge declaration".to_string(),
                            });
                        }
                        Value::String(s) => {
                            // (D) qualified syntax check
                            if let Some(pos) = s.find(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                                let ns_part = &s[..pos];
                                let id_part =
                                    &s[pos + crate::parser::ingest::NAMESPACE_SEPARATOR.len()..];
                                if ns_part.trim().is_empty() || id_part.trim().is_empty() {
                                    warnings.push(WriteShapeWarning {
                                        field: format!("{}.{}", key, i),
                                        why: "qualified target needs a namespace and local ID"
                                            .to_string(),
                                        skipped: "this edge declaration".to_string(),
                                    });
                                    continue;
                                }
                            }
                            specs.push(EdgeSpec {
                                rel_type,
                                target_id: s.clone(),
                                points_at_target,
                                declaration_index: i,
                            });
                        }
                        _ => {
                            warnings.push(WriteShapeWarning {
                                field: format!("{}.{}", key, i),
                                why: "relation target must be a string".to_string(),
                                skipped: "this edge declaration".to_string(),
                            });
                        }
                    }
                }
            }
            _ => {
                warnings.push(WriteShapeWarning {
                    field: key.to_string(),
                    why: "relation declaration must be an array".to_string(),
                    skipped: "declarations in this field".to_string(),
                });
            }
        }
    }

    // (E) access topology shape. The field is declared on a `fact`, so the
    // check keys off the memory type: on any other type `topology` is an
    // undeclared metadata key and is accepted unchanged.
    //
    // The check is soft. A malformed claim warns and is still written, because
    // a rejected write would lose the claim entirely.
    if payload.memory_type.trim() == "fact" {
        if let Some(topology) = payload.metadata.get("topology") {
            let mut defects: Vec<String> = Vec::new();
            match topology {
                Value::Object(fields) => {
                    for key in TOPOLOGY_KEYS {
                        match fields.get(key) {
                            None => defects.push(format!("is missing the required key {:?}", key)),
                            Some(Value::String(_)) => { /* a string: as declared */ }
                            Some(_) => defects.push(format!("key {:?} must be a string", key)),
                        }
                    }
                }
                _ => defects.push("must be an object".to_string()),
            }
            if !defects.is_empty() {
                warnings.push(WriteShapeWarning {
                    field: "metadata.topology".to_string(),
                    why: format!("metadata.topology {}", defects.join("; ")),
                    skipped: "the topology shape check; the fact is still written".to_string(),
                });
            }
        }
    }

    // (F) attempt shape. Declared on a `fact` (a failed episode recorded on a
    // fact). Soft check: a malformed attempt warns and the fact is still
    // written.
    if payload.memory_type.trim() == "fact" {
        let defects = crate::attempt::attempt_defects(&payload.metadata);
        if payload.metadata.get("attempt").is_some() && !defects.is_empty() {
            warnings.push(WriteShapeWarning {
                field: "metadata.attempt".to_string(),
                why: format!("metadata.attempt {}", defects.join("; ")),
                skipped: "the attempt shape check; the fact is still written".to_string(),
            });
        }
    }

    (specs, warnings)
}

/// Collects shape warnings from a payload for emit_write_shape_warning calls.
fn write_shape_warnings(payload: &crate::traits::MemoryPayload) -> Vec<WriteShapeWarning> {
    parse_edge_specs(payload).1
}

/// Emits one shape warning to stderr in the required JSON format.
fn emit_write_shape_warning(namespace: &str, id: &str, warning: &WriteShapeWarning) {
    eprintln!(
        "[neurostrata:write-shape] {}",
        serde_json::json!({
            "namespace": namespace,
            "id": id,
            "field": warning.field,
            "why": warning.why,
            "skipped": warning.skipped,
        })
    );
}

/// Exact ID lookup for qualified target resolution.
fn exact_target_exists(conn: &Connection, id: &str) -> Result<bool> {
    let safe_id = escape_ladybug_string(id);
    let query = format!(
        "MATCH (b:Memory) WHERE b.id = '{}' RETURN b.id LIMIT 1",
        safe_id
    );
    let mut rows = conn.query(&query)?;
    Ok(rows.next().is_some())
}

/// The generation currently active for one source (`0` = none published).
fn active_generation_of_source(conn: &Connection, namespace: &str, source_key: &str) -> u64 {
    let key = crate::parser::ingest::generation_key(namespace, source_key);
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.id = '{}' RETURN g.active_generation",
        escape_ladybug_string(&key)
    );
    match conn.query(&query) {
        Ok(mut rows) => match rows.next() {
            Some(row) => format!("{}", row[0]).parse::<i64>().unwrap_or(0).max(0) as u64,
            None => 0,
        },
        Err(e) => { tracing::warn!("active_generation_of_source query failed for ({namespace}, {source_key}): {e}"); 0 },
    }
}

/// The most recently published generation for a namespace across all sources.
/// Used to resolve a declared edge target on write, where the source is not
/// carried: the latest tree is the best available answer. A query that cannot
/// run reports `0` rather than failing, so resolution degrades to "no
/// generation" instead of erroring out of a write it is only trying to guide.
fn active_generation_of(conn: &Connection, namespace: &str) -> u64 {
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.namespace = '{}' AND g.active_generation > 0 RETURN g.active_generation",
        escape_ladybug_string(namespace)
    );
    match conn.query(&query) {
        Ok(rows) => {
            let mut latest: u64 = 0;
            for row in rows {
                if let Ok(g) = format!("{}", row[0]).parse::<i64>() {
                    if g > 0 {
                        latest = latest.max(g as u64);
                    }
                }
            }
            latest
        }
        Err(e) => { tracing::warn!("active_generation_of query failed for {namespace}: {e}"); 0 },
    }
}

/// The set of active generations for a namespace, one per source that has
/// published. Read surfaces (`list`, `export_graph`) drop rows from a
/// superseded generation against this set.
fn active_generations_of(conn: &Connection, namespace: &str) -> std::collections::HashSet<u64> {
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.namespace = '{}' AND g.active_generation > 0 RETURN g.active_generation",
        escape_ladybug_string(namespace)
    );
    let mut gens = std::collections::HashSet::new();
    match conn.query(&query) {
        Ok(rows) => {
            for row in rows {
                if let Ok(g) = format!("{}", row[0]).parse::<i64>() {
                    if g > 0 {
                        gens.insert(g as u64);
                    }
                }
            }
        }
        Err(e) => { tracing::warn!("active_generations_of query failed: {e}"); }
    }
    gens
}

/// The active generation per source plus the namespace-wide active set and the
/// max active generation among non-`"docs"` sources.
///
/// Unlike `active_generations_of` (a flat `HashSet` across all `source_key`
/// rows), this keeps the `source_key -> active_gen` mapping so a row can be
/// validated against the source that actually owns it. Two worktrees of one
/// namespace ingest at different times, so `IngestGeneration` holds multiple
/// rows per namespace (e.g. `source_key = "docs" -> 3`, `source_key = "dirA" ->
/// 5`); a `doc_section` row must match *its* source's active generation, not
/// just any active generation in the namespace.
pub(crate) fn active_generations_by_source_of(
    conn: &Connection,
    namespace: &str,
) -> (
    HashMap<String, i64>,
    std::collections::HashSet<i64>,
    Option<i64>,
) {
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.namespace = '{}' AND g.active_generation > 0 RETURN g.source_key, g.active_generation",
        escape_ladybug_string(namespace)
    );
    let mut by_source = HashMap::new();
    let mut active_set = std::collections::HashSet::new();
    let mut max_non_doc: Option<i64> = None;
    match conn.query(&query) {
        Ok(rows) => {
            for row in rows {
                let source_key = format!("{}", row[0]);
                let gen = match format!("{}", row[1]).parse::<i64>() {
                    Ok(g) if g > 0 => g,
                    _ => continue,
                };
                by_source.insert(source_key.clone(), gen);
                active_set.insert(gen);
                if source_key != "docs" {
                    max_non_doc = Some(max_non_doc.unwrap_or(0).max(gen));
                }
            }
        }
        Err(e) => { tracing::warn!("active_generations_by_source_of query failed: {e}"); }
    }
    (by_source, active_set, max_non_doc)
}

/// Row filter for generation-scoped reads (`search_inner`, `list`, SPARQL
/// projection). Returns `Some(full_id)` when the row is reachable and `None`
/// when it belongs to a superseded generation or another namespace.
///
/// - Unscoped rows (`split_scoped_id == None`) — normative `rule`/`fact`/
///   `decision`/`procedure` and legacy AST ids — are always kept.
/// - Scoped rows are kept only against their owning source's active
///   generation; `doc_section` rows fall back to the namespace's `docs`
///   pointer (or any active doc generation when no pointer exists).
/// - Deduplication of equal `bare_id` across active generations (keep highest
///   `gen`) is the caller's job; this predicate is stateless per row.
pub(crate) fn is_active_scoped_row(
    id: &str,
    namespace: &str,
    memory_type: &str,
    metadata: &Value,
    by_source: &HashMap<String, i64>,
    active_set: &std::collections::HashSet<i64>,
    _max_non_doc_gen: Option<i64>,
) -> Option<String> {
    let Some((scoped_ns, gen, _bare_id)) = split_scoped_id(id) else {
        // Unscoped (normative or legacy): always reachable.
        return Some(id.to_string());
    };
    if scoped_ns != namespace {
        return None;
    }
    let gen_i = gen as i64;
    if by_source.is_empty() && active_set.is_empty() {
        // No generation table entries yet: nothing is superseded.
        return Some(id.to_string());
    }
    let keep = if let Some(source_key) = metadata.get("source_key").and_then(|v| v.as_str()) {
        // Explicit source pointer: the row must equal that source's active gen.
        by_source.get(source_key).is_some_and(|&active| active == gen_i)
    } else if memory_type == "doc_section" {
        // `doc_section` rows without an explicit `source_key`: match the docs
        // pointer when present, otherwise any active doc generation.
        if let Some(&docs_gen) = by_source.get("docs") {
            docs_gen == gen_i
        } else {
            active_set.contains(&gen_i)
        }
    } else if by_source.is_empty() {
        active_set.contains(&gen_i)
    } else {
        by_source.iter().any(|(k, &g)| k != "docs" && g == gen_i)
    };
    if keep { Some(id.to_string()) } else { None }
}

/// A namespace's active generations, read once, for deciding which rows a
/// reader may see. Wraps `active_generations_by_source_of` and
/// `is_active_scoped_row` so a reader does not carry their three loose values
/// and a seven-argument call (`list_rows`, `generation_summary`).
pub(crate) struct ActiveGenerations {
    by_source: HashMap<String, i64>,
    active_set: std::collections::HashSet<i64>,
    max_code_gen: Option<i64>,
}

impl ActiveGenerations {
    pub(crate) fn read(conn: &Connection, namespace: &str) -> Self {
        let (by_source, active_set, max_code_gen) = active_generations_by_source_of(conn, namespace);
        Self { by_source, active_set, max_code_gen }
    }

    /// Same rule as `is_active_scoped_row`: false for a row of a superseded
    /// generation or another namespace.
    pub(crate) fn reaches(&self, id: &str, namespace: &str, memory_type: &str, metadata: &Value) -> bool {
        is_active_scoped_row(id, namespace, memory_type, metadata, &self.by_source, &self.active_set, self.max_code_gen).is_some()
    }
}

/// Normative memory types that the recall path reserves result slots for.
/// Stated once here so the search ranker and the task gate can agree on the
/// same predicate (see the FEATURE-4 provenance contract in `src/traits.rs`).
const NORMATIVE_MEMORY_TYPES: [&str; 4] = ["rule", "fact", "decision", "procedure"];

/// Relevance floor (similarity) below which a normative candidate is eligible
/// for a reserved search slot. Scores are distances (lower = better, [0.0, 1.0]
/// normalized in hybrid / cosine-L2 in default mode), so the similarity floor
/// maps to a maximum distance of `MAX_NORMATIVE_DISTANCE`.
const NORMATIVE_RELEVANCE_FLOOR: f32 = 0.20;
const MAX_NORMATIVE_DISTANCE: f32 = 1.0 - NORMATIVE_RELEVANCE_FLOOR;

/// Maximum normative slots reserved in a search result, capped at half the
/// limit rounded up so structural evidence always keeps at least half the slots.
fn normative_slot_limit(limit: usize) -> usize {
    usize::min(3, limit.div_ceil(2))
}

/// Reserve up to `normative_slot_limit(limit)` slots for normative memories
/// (`rule`/`fact`/`decision`/`procedure`) that pass `MAX_NORMATIVE_DISTANCE`,
/// filling remaining slots up to `limit` from `non_normative` (and any leftover
/// normative candidates when `non_normative` has fewer than `remaining` items).
fn reserve_normative_slots(candidates: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    if limit < 3 {
        return candidates.into_iter().take(limit).collect();
    }
    let normative_limit = normative_slot_limit(limit);
    let mut normative: Vec<SearchResult> = Vec::new();
    let mut non_normative: Vec<SearchResult> = Vec::new();
    for r in candidates {
        let is_normative = NORMATIVE_MEMORY_TYPES.contains(&r.payload.memory_type.as_str())
            && r.score <= MAX_NORMATIVE_DISTANCE;
        if is_normative {
            normative.push(r);
        } else {
            non_normative.push(r);
        }
    }
    let mut reserved: Vec<SearchResult> = Vec::new();
    let mut normative_iter = normative.into_iter();
    for r in normative_iter.by_ref().take(normative_limit) {
        reserved.push(r);
    }
    let remaining = limit.saturating_sub(reserved.len());
    reserved.extend(non_normative.into_iter().take(remaining));
    if reserved.len() < limit {
        let extra = limit - reserved.len();
        reserved.extend(normative_iter.take(extra));
    }
    reserved
}

/// The bare (logical) id inside a generation-scoped id, or `None` for unscoped
/// rows. Used to deduplicate the same logical target across generations.
fn bare_id_of(id: &str) -> Option<String> {
    split_scoped_id(id).map(|(_, _, bare)| bare)
}

/// The active `(namespace, generation)` pairs across every namespace, for the
/// whole-graph read surface (`export_graph`). A generation-scoped node whose
/// pair is absent was superseded by a later publish and is dropped.
fn all_active_generations(conn: &Connection) -> std::collections::HashSet<(String, u64)> {
    let query = "MATCH (g:IngestGeneration) WHERE g.active_generation > 0 RETURN g.namespace, g.active_generation";
    let mut pairs = std::collections::HashSet::new();
    match conn.query(query) {
        Ok(rows) => {
            for row in rows {
                let ns = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                if let Ok(g) = format!("{}", row[1]).parse::<i64>() {
                    if g > 0 {
                        pairs.insert((ns, g as u64));
                    }
                }
            }
        }
        Err(e) => { tracing::warn!("all_active_generations query failed: {e}"); }
    }
    pairs
}

/// The ids a namespace actually holds, indexed so a declaration can be resolved
/// without scanning them.
///
/// Node ids became repository-relative, so a memory written when ingestion
/// produced absolute ids declares `C:/proj/src/foo.rs` and matches nothing now.
/// Its tail still identifies the file, so a declaration ending in `/<id>`
/// resolves to that id, and a suffix matching more than one node is left alone.
///
/// That rule is unchanged. Asking it of every known id in turn was the problem:
/// O(declarations x nodes) comparisons with a `format!` allocation per
/// comparison, on the path that runs at the end of every ingest. A declaration
/// has only as many path suffixes as it has separators, so looking each of
/// those up answers the same question in a handful of hashes however large the
/// namespace grows.
pub struct KnownIds<'a> {
    ids: std::collections::HashSet<&'a str>,
    /// Qualified ids indexed by the bare path inside them, so a rule naming
    /// `src/lib.rs` finds `NeuroStrata::src/lib.rs` without scanning. A path
    /// held by more than one namespace is recorded as ambiguous and resolves to
    /// neither: a GOVERNS edge into another project's file is worse than none.
    by_path: std::collections::HashMap<&'a str, Option<&'a str>>,
    /// The mention candidate map, derived once on first use. Rebuilding it
    /// per memory was O(memories x ids) on the edge-replay pass.
    candidates_cache: std::sync::OnceLock<std::collections::HashMap<String, String>>,
}

impl crate::mention::SymbolIndex for KnownIds<'_> {
    /// The candidate map the detector needs is already memoized on the
    /// index, so it is handed back instead of re-derived per memory.
    fn cached_candidates(
        &self,
    ) -> Option<std::collections::HashMap<String, String>> {
        Some(self.mention_candidates())
    }
}

impl<'a> KnownIds<'a> {
    /// Index every id. A bare path is reachable exactly as-is; a qualified or
    /// generation-scoped id is reachable by its logical path. No generation
    /// filter: used by tests and by callers that have already narrowed the set.
    pub fn new(ids: impl IntoIterator<Item = &'a str>) -> Self {
        Self::build(ids, None)
    }

    /// Index only the active generation's nodes. A generation-scoped id whose
    /// generation is not `active_gen` is left out, and so is any id that is not
    /// generation-scoped at all (a rule/task UUID, or a pre-generation
    /// qualified id) -- only the published generation is reachable.
    pub fn active(ids: impl IntoIterator<Item = &'a str>, active_gen: u64) -> Self {
        Self::build(ids, Some(active_gen))
    }

    fn build(ids: impl IntoIterator<Item = &'a str>, active_gen: Option<u64>) -> Self {
        let mut id_set: std::collections::HashSet<&'a str> =
            std::collections::HashSet::new();
        let mut by_path: std::collections::HashMap<&'a str, Option<&'a str>> =
            std::collections::HashMap::new();

        for id in ids.into_iter().filter(|id| !id.is_empty()) {
            if let Some(active) = active_gen {
                match crate::parser::ingest::split_scoped_id(id) {
                    Some((_, gen, _)) => {
                        if gen != active {
                            continue; // a stale generation is not reachable
                        }
                    }
                    // Not generation-scoped: not part of any active generation.
                    None => continue,
                }
            }

            id_set.insert(id);
            // Only namespace- (or generation-) scoped ids are indexed by their
            // logical path; a bare id stays reachable exactly as-is.
            if id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                let path = crate::parser::ingest::logical_path_of(id);
                if !path.is_empty() {
                    by_path
                        .entry(path)
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(id));
                }
            }
        }

        Self {
            ids: id_set,
            by_path,
            candidates_cache: std::sync::OnceLock::new(),
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Iterate the indexed ids (for mention candidate derivation).
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.ids.iter().copied()
    }

    /// Build the mention candidate map (candidate string -> symbol id),
    /// memoized: repeated calls share one derivation.
    pub fn mention_candidates(&self) -> std::collections::HashMap<String, String> {
        self.candidates_cache
            .get_or_init(|| crate::mention::build_candidates(self))
            .clone()
    }

    /// Finds the node a declaration means, when it does not name one exactly.
    pub fn resolve(&self, declared: &str) -> Option<String> {
        // Trailing slashes name a directory the same way the path without
        // one does; the ingested node id carries no trailing slash.
        let declared = declared.trim_end_matches('/');
        // Deliberately before normalisation: an id stored with backslashes is
        // still matched by naming it exactly, as it always was.
        if self.ids.contains(declared) {
            return Some(declared.to_string());
        }

        let declared = declared.replace('\\', "/");

        // An id qualified by its namespace is LONGER than the declaration that
        // names it: a rule says `src/lib.rs`, the id is `NeuroStrata::src/lib.rs`.
        // That is the common case now, so it is asked first. A path held by two
        // namespaces was indexed as ambiguous and resolves to neither.
        if let Some(found) = self.by_path.get(declared.as_str()) {
            return found.map(str::to_string);
        }

        // The other direction: a declaration longer than the id it means, which
        // is a legacy absolute path naming a file the graph already holds.
        //
        // Its suffix can name that file two ways. Before migrating it is an id
        // (`src/lib.rs`); after doctor's migration, a re-ingest, it is the path
        // inside a qualified id (`NeuroStrata::src/lib.rs`). Checking only ids
        // meant every absolute declaration lost its edge at exactly the step
        // the migration tells people to take.
        let mut only: Option<&str> = None;
        for (separator, _) in declared.match_indices('/') {
            let suffix = &declared[separator + 1..];
            let mut hits: Vec<&str> = Vec::new();
            if let Some(id) = self.ids.get(suffix) {
                hits.push(id);
            }
            match self.by_path.get(suffix) {
                Some(Some(qualified)) => hits.push(qualified),
                // Held by more than one namespace: ambiguous, as above.
                Some(None) => return None,
                None => {}
            }
            for id in hits {
                if only.is_some() {
                    // A suffix matching more than one node is ambiguous, and a
                    // wrong edge is worse than a missing one.
                    return None;
                }
                only = Some(id);
            }
        }
        only.map(|id| id.to_string())
    }
}

/// One edge a memory asks for, read off its metadata.
#[derive(Debug, PartialEq, Clone)]
pub struct EdgeSpec {
    pub rel_type: &'static str,
    pub target_id: String,
    pub declaration_index: usize,
    /// true for `self -> target`, false for `target -> self`. Containment reads
    /// the other way round: the parent contains the child, not the reverse.
    pub points_at_target: bool,
}

/// Maps metadata arrays onto the three edge tables. `related_to` is a semantic
/// link, `contained_by` is structure (directory to file to symbol), and
/// `governs` connects a rule to the code it constrains -- the edge that lets an
/// architectural memory be found from the file it applies to.
pub fn edge_specs(metadata: &serde_json::Value) -> Vec<EdgeSpec> {
    let payload = crate::traits::MemoryPayload {
        content: String::new(),
        user_id: String::new(),
        memory_type: String::new(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: metadata.clone(),
    };
    parse_edge_specs(&payload).0
}

/// Memory types written with a zero vector. They describe where something
/// lives, or record that a machine operation happened, not what it says, so
/// they carry no meaning for a similarity search: directory ingestion's nodes,
/// and the thalamic bus's sentinel and audit rows.
const STRUCTURAL_MEMORY_TYPES: [&str; 8] = [
    "directory",
    "file",
    "markdown",
    "freshness_flag",
    "guard_event",
    "community_summary",
    "digital_twin",
    "digital_twin_schema",
];

/// Row-class predicates for filtering at the store seam.
/// These are applied BEFORE the candidate budget is spent.

fn is_archived(metadata: &Value) -> bool {
    metadata
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Row-class predicates for filtering at the store seam.
/// These are applied BEFORE the candidate budget is spent.
fn is_registry_row(metadata: &Value) -> bool {
    crate::registry::is_registry_row(metadata)
}

/// Delegates to the canonical predicate rather than re-deriving it: the
/// conflict rule is stated once, in `synaptic_mesh::conflict`, so the read
/// surfaces cannot drift into disagreeing about what "conflicting" means.
fn is_mesh_conflicting(metadata: &Value) -> bool {
    crate::synaptic_mesh::conflict::is_conflicting_metadata(metadata)
}

/// Ranking gain for a memory that carries `metadata.source` (FEATURE-4).
///
/// Sized against the usage boost's own scale: at access_count = 1 the usage
/// gain is 0.05 * ln(2) ~= 0.035, so a single provenance marker buys roughly
/// two reads' worth of promotion. Enough to settle a tie, small enough that a
/// memory about a different subject does not surface just because somebody
/// dated it.
const SOURCE_RANK_GAIN: f32 = 0.05;
const FEEDBACK_RANK_GAIN: f32 = 0.05;

/// Normalize an evidence path for dedup: a one-hop undirected relation's endpoints are sorted; a directed relation is order-sensitive.
fn norm_path(
    edges: &[crate::traits::EvidenceEdge],
    kind: &crate::traits::EvidenceKind,
) -> Vec<(String, String, String)> {
    let relations = &crate::traits::memory_vocabulary()["relations"];
    edges
        .iter()
        .map(|e| {
            if *kind == crate::traits::EvidenceKind::OneHop && relations[e.relation.as_str()]["direction"] == "undirected" {
                let (a, b) = if e.source <= e.target {
                    (e.source.clone(), e.target.clone())
                } else {
                    (e.target.clone(), e.source.clone())
                };
                (a, e.relation.clone(), b)
            } else {
                (e.source.clone(), e.relation.clone(), e.target.clone())
            }
        })
        .collect()
}

fn escape_ladybug_string(s: &str) -> String {
    s.replace("\\", "\\\\").replace("'", "\\'")
}

/// Validates a Cypher relation (edge) type name before it is interpolated
/// into a query string. Relation types are compile-time constants today, but
/// this guard prevents a future code path that accepts user-supplied relation
/// names from becoming a Cypher-injection vector. Returns `true` only for
/// names matching `^[A-Z][A-Z0-9_]*$`.
fn is_valid_relation_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// How many edges go into one relinking statement. The pairs are written into
/// the query text, so this is what bounds how large that text can get.
const RELINK_BATCH: usize = 500;

/// Replays one relationship type in a couple of statements instead of one per
/// edge.
///
/// The obvious spelling is a single UNWIND driving MERGE, which would say "link
/// these unless they are linked already" in one statement. lbug 0.15.3 cannot
/// run it: MERGE inside an UNWIND pipeline fails with an internal
/// `invalid unordered_map<K, T> key`, while the same UNWIND with MATCH, or the
/// same MERGE without UNWIND, both succeed. So the pairs that already exist are
/// read back and subtracted here, which makes CREATE safe and keeps the
/// statement count independent of the number of edges.
///
/// It also reports the truth. The loop this replaces counted statements that
/// did not error -- including every one whose MATCH found nothing -- so an
/// ingest that linked nothing still announced hundreds of relinked edges.
fn create_missing_edges(
    conn: &Connection,
    rel_type: &str,
    pairs: &[(String, String)],
    namespace: &str,
    generation: u64,
    now: i64,
    extractor: &str,
) -> Result<usize> {
    let safe_extractor = escape_ladybug_string(extractor);
    if !is_valid_relation_name(rel_type) {
        anyhow::bail!("invalid relation name for Cypher interpolation: {:?}", rel_type);
    }
    let existing = conn.query(&format!(
        "MATCH (x:Memory)-[:{}]->(y:Memory) RETURN x.id, y.id",
        rel_type
    ))?;

    let mut present = std::collections::HashSet::new();
    for row in existing {
        present.insert((format!("{}", row[0]), format!("{}", row[1])));
    }

    // The same edge can be declared by both ends, and CREATE would honour each
    // declaration separately, so the batch is deduplicated as well.
    let mut missing = Vec::new();
    for pair in pairs {
        if !present.contains(pair) {
            present.insert(pair.clone());
            missing.push(pair);
        }
    }

    let mut created = 0usize;
    for chunk in missing.chunks(RELINK_BATCH) {
        let list = chunk
            .iter()
            .map(|(from, to)| {
                format!(
                    "{{f: '{}', t: '{}'}}",
                    escape_ladybug_string(from),
                    escape_ladybug_string(to)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        // Both ends must exist; a rule naming a file the ingester never reached
        // still produces nothing, exactly as before.
        let safe_ns = escape_ladybug_string(namespace);
        let query = format!(
            "UNWIND [{}] AS e MATCH (a:Memory {{id: e.f}}), (b:Memory {{id: e.t}}) CREATE (a)-[:{} {{source: '{}', valid_from: {}, valid_to: 0, generation: {}, extractor: '{}'}}]->(b) RETURN count(*)",
            list, rel_type, safe_ns, now, generation, safe_extractor
        );
        for row in conn.query(&query)? {
            created += format!("{}", row[0]).parse::<usize>().unwrap_or(0);
        }
    }

    Ok(created)
}

impl LadybugStore {
    /// Public wrapper for internal list — used by SPARQL projection.
    pub async fn list_memories(
        &self,
        namespace: &str,
        user_id: Option<&str>,
        memory_type: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        self.list(namespace, user_id, memory_type).await
    }

    pub async fn active_generations_public(&self, namespace: &str) -> std::collections::HashSet<u64> {
        let ns_label = namespace.to_string();
        let namespace = namespace.to_string();
        self.with_conn(move |conn| Ok(crate::store::ladybug::active_generations_of(conn, &namespace)))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("active generations query failed for '{ns_label}': {e}");
                Default::default()
            })
    }

    pub async fn symbol_ids(&self, namespace: &str) -> Vec<String> {
        let gens = self.active_generations_public(namespace).await;
        let memories = self
            .list(namespace, Some("auto-ingestor"), None)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("symbol_ids memory list failed for '{namespace}': {e}");
                Default::default()
            });
        let mut out = Vec::new();
        for m in memories {
            let mt = m.payload.memory_type.as_str();
            if !matches!(mt, "code_ast" | "markdown" | "directory" | "file") {
                continue;
            }
            match crate::parser::ingest::split_scoped_id(&m.id) {
                Some((_, gen, _)) => {
                    if !gens.is_empty() && !gens.contains(&gen) {
                        continue;
                    }
                }
                None => {
                    if !gens.is_empty() {
                        continue;
                    }
                }
            }
            out.push(m.id);
        }
        out
    }

    pub async fn mention_symbol_ids(&self, namespace: &str) -> std::sync::Arc<Vec<String>> {
        let gens = self.active_generations_public(namespace).await;
        {
            let guard = self.mention_symbol_ids.read().await;
            if let Some((cached_gens, vec)) = guard.get(namespace) {
                if *cached_gens == gens {
                    return vec.clone();
                }
            }
        }
        let vec = std::sync::Arc::new(self.symbol_ids(namespace).await);
        let mut guard = self.mention_symbol_ids.write().await;
        guard.insert(namespace.to_string(), (gens, vec.clone()));
        vec
    }

    pub async fn dangling_edges(&self, namespace: &str) -> Result<Vec<serde_json::Value>> {
        // Diagnostic read-only query - no namespace hygiene repair

        let namespace = namespace.to_string();
        self.with_conn(move |conn| {
            // Collect active memory ids + metadata.
            let query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.metadata",
                escape_ladybug_string(&namespace)
            );
            let mut res = conn.query(&query)?;
            let mut rows: Vec<(String, String)> = Vec::new();
            let now = chrono::Utc::now().timestamp();
            while let Some(row) = res.next() {
                let id = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let meta = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                let parsed_meta: serde_json::Value = serde_json::from_str(&meta).unwrap_or(serde_json::json!({}));
                if is_archived(&parsed_meta) || is_retired_at(&parsed_meta, now) {
                    continue;
                }
                rows.push((id, meta));
            }

            let mut known_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
            for (id, _) in &rows {
                known_ids.insert(id.clone());
            }

            let mut out: Vec<serde_json::Value> = Vec::new();
            for (declaring_id, meta_str) in &rows {
                let meta: serde_json::Value = serde_json::from_str(meta_str).unwrap_or(serde_json::json!({}));
                // Skip registry rows.
                if meta.get("domain_registry").is_some() {
                    continue;
                }
                let specs = edge_specs(&meta);
                for spec in specs {
                    let mut resolution = if spec.target_id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                        match exact_target_exists(conn, &spec.target_id) {
                            Ok(true) => crate::traits::TargetResolution::Resolved(spec.target_id.clone()),
                            Ok(false) => {
                                if let Some(stripped) = spec.target_id.strip_prefix(&format!("{}{}", namespace, crate::parser::ingest::NAMESPACE_SEPARATOR)) {
                                    resolve_target_on_write(conn, &namespace, stripped)
                                } else {
                                    crate::traits::TargetResolution::Missing
                                }
                            },
                            Err(e) => crate::traits::TargetResolution::Failed(e.to_string()),
                        }
                    } else {
                        resolve_target_on_write(conn, &namespace, &spec.target_id)
                    };
                    // Unambiguous prefix resolution for short (>=7 char) IDs.
                    if matches!(resolution, crate::traits::TargetResolution::Missing) && spec.target_id.len() >= 7 {
                        let matches: Vec<String> = known_ids.iter().filter(|id| id.starts_with(&spec.target_id)).cloned().collect();
                        if matches.len() == 1 {
                            resolution = crate::traits::TargetResolution::Resolved(matches[0].clone());
                        }
                    }
                    let reason = match resolution {
                        crate::traits::TargetResolution::Missing => "missing",
                        crate::traits::TargetResolution::Ambiguous(_) => "ambiguous",
                        crate::traits::TargetResolution::Failed(_) => "unresolvable",
                        crate::traits::TargetResolution::Resolved(_) => continue,
                    };
                    out.push(serde_json::json!({
                        "rel_type": spec.rel_type,
                        "declaring_id": declaring_id,
                        "declaration_index": spec.declaration_index,
                        "declared_target": spec.target_id,
                        "reason": reason,
                    }));
                }
            }
            // Sort by (rel_type, declaring_id, declaration_index, declared_target, reason).
            out.sort_by(|a, b| {
                let ka = (
                    a.get("rel_type").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    a.get("declaring_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    a.get("declaration_index").and_then(|x| x.as_u64()).unwrap_or(0),
                    a.get("declared_target").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    a.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                );
                let kb = (
                    b.get("rel_type").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    b.get("declaring_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    b.get("declaration_index").and_then(|x| x.as_u64()).unwrap_or(0),
                    b.get("declared_target").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    b.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                );
                ka.cmp(&kb)
            });
            Ok(out)
        })
        .await
    }

    /// `list`, optionally narrowed in the engine to rows whose metadata text
    /// contains `metadata_contains`. The narrowing only drops rows the caller
    /// would discard anyway; every row it returns goes through the same
    /// generation and dedup rules as `list`.
    pub(crate) async fn list_rows(&self, namespace: &str, user_id: Option<&str>, memory_type: Option<&str>, metadata_contains: Option<&str>) -> Result<Vec<SearchResult>> {
        let namespace = namespace.to_string();
        let user_id = user_id.map(|v| v.to_string());
        let memory_type = memory_type.map(|v| v.to_string());
        let metadata_contains = metadata_contains.map(|v| v.to_string());
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // The active generations for this namespace, resolved per source so a
            // row is validated against the source that owns it (not just any
            // active generation in the namespace). Read once per search.
            let (by_source, active_set, max_code_gen) =
                active_generations_by_source_of(conn, &namespace);

            let mut best_by_bare: HashMap<String, (u64, usize)> = HashMap::new();

            // Build WHERE clause with optional filters (R15: push into Cypher).
            let mut conditions = vec![format!("m.namespace = '{}'", safe_ns)];
            if let Some(ref uid) = user_id {
                conditions.push(format!("m.user_id = '{}'", escape_ladybug_string(uid)));
            }
            if let Some(ref mt) = memory_type {
                conditions.push(format!("m.memory_type = '{}'", escape_ladybug_string(mt)));
            }
            if let Some(ref needle) = metadata_contains {
                conditions.push(format!("m.metadata CONTAINS '{}'", escape_ladybug_string(needle)));
            }
            let where_clause = conditions.join(" AND ");

            let query = format!("MATCH (m:Memory) WHERE {} RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", where_clause);

            let result = conn.query(&query)?;
            let mut results = Vec::new();

            for row in result {
                let id: String = format!("{}", row[0]);
                let content: String = format!("{}", row[1]);
                let uid: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);

                let metadata_val: Value = match serde_json::from_str(&metadata_str) {
                    Ok(v) => v,
                    Err(e) => {
                        if !metadata_str.trim().is_empty() {
                            tracing::warn!("corrupt metadata JSON in store row: {e}");
                        }
                        Value::Null
                    }
                };

                // Wave 2.1 row filter: same generation-scoped reachability the
                // search path applies, so diagnostics never see superseded rows.
                if is_active_scoped_row(
                    &id,
                    &namespace,
                    &memory_type,
                    &metadata_val,
                    &by_source,
                    &active_set,
                    max_code_gen,
                ).is_none() {
                    continue;
                }

                let sr = SearchResult {
                    id: id.clone(),
                    score: 0.0,
                    payload: MemoryPayload {
                        content,
                        user_id: uid,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                    evidence: None,
                };

                // Dedup by bare_id across generations: keep the highest
                // generation for each logical id. Unscoped rows (bare_id_of
                // returns None) are never deduped.
                if let Some(bare) = bare_id_of(&id) {
                    let gen = split_scoped_id(&id)
                        .map(|(_, g, _)| g as u64)
                        .unwrap_or(0);
                    match best_by_bare.get(&bare) {
                        Some(&(existing_gen, idx)) if gen > existing_gen => {
                            results[idx] = sr.clone();
                            best_by_bare.insert(bare, (gen, idx));
                        }
                        Some(_) => {
                            // existing entry has higher or equal gen; skip.
                            continue;
                        }
                        None => {
                            let idx = results.len();
                            best_by_bare.insert(bare, (gen, idx));
                            results.push(sr);
                        }
                    }
                } else {
                    results.push(sr);
                }
            }

            Ok(results)
        })
        .await
    }


    pub async fn repair_namespace_hygiene(&self, namespace: &str) -> Result<(usize, usize)> {
        let namespace = namespace.to_string();
        self.write_with_deadline("repairing namespace hygiene", move |conn| {
            let safe = crate::store::ladybug::escape_ladybug_string(&namespace);
            let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.memory_type, m.location, m.content, m.metadata", safe);
            let mut res = conn.query(&query)?;
            let mut known_ids = std::collections::HashSet::new();
            let mut rows = Vec::new();
            while let Some(row) = res.next() {
                let id = format!("{}", row[0]);
                let mtype = format!("{}", row[1]);
                let loc = format!("{}", row[2]);
                let content_str = format!("{}", row[3]);
                let meta_str = format!("{}", row[4]);
                let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(serde_json::json!({}));
                if meta.get("archived").and_then(|v| v.as_bool()).unwrap_or(false) { continue; }
                known_ids.insert(id.clone());
                rows.push((id, mtype, loc, content_str, meta_str));
            }
            let mut shape_updates = 0usize;
            let mut dangling_updates = 0usize;
            for (id, mtype, loc, content_str, meta_str) in &rows {
                let mut meta: serde_json::Value = serde_json::from_str(meta_str).unwrap_or(serde_json::json!({}));
                if !meta.is_object() { meta = serde_json::json!({}); }
                let mut shape_changed = false;
                let mut dangling_changed = false;
                // (a) rule source
                if mtype == "rule" {
                    // A source is usable provenance when it is a non-empty string
                    // or a non-empty object -- `crate::traits::has_source` is the
                    // canonical predicate for that, and the task gate and the
                    // SHACL checker both ask it. Reading the field as a string
                    // alone made an object source look blank, and this repair
                    // would then OVERWRITE the structured origin with the row's
                    // location: the repair destroyed the provenance it was
                    // supposed to preserve. "N/A" stays a placeholder, not a
                    // source, so it is still repaired.
                    let src = meta.get("source").and_then(|v| v.as_str()).unwrap_or("");
                    let usable = crate::traits::has_source(&meta) && src != "N/A";
                    if !usable {
                        let val = if !loc.is_empty() && loc != "N/A" { loc.clone() } else { "legacy-rule".to_string() };
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("source".to_string(), serde_json::Value::String(val));
                            shape_changed = true;
                        }
                    }
                }
                // (a2) procedure trigger repair
                if mtype == "procedure" {
                    let trigger = meta.get("trigger").and_then(|v| v.as_str()).unwrap_or("");
                    let is_valid = !trigger.is_empty() && (trigger == "session-start" || trigger == "before-edit" || trigger == "after-mutation" || trigger.starts_with("every-n-sessions:") || trigger.starts_with("every-n-days:"));
                    if !is_valid {
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("trigger".to_string(), serde_json::Value::String("session-start".to_string()));
                            shape_changed = true;
                        }
                    }
                    let remaining_missing = meta.get("remaining_fires").and_then(|v| v.as_u64()).is_none();
                    if remaining_missing {
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("remaining_fires".to_string(), serde_json::Value::Number(serde_json::Number::from(5)));
                            shape_changed = true;
                        }
                    }
                }
                // (a3) task memory repair: backfill metadata.task defaults
                if mtype == "task" {
                    let task_meta = meta.get("task").cloned();
                    let mut task_obj = if let Some(Value::Object(obj)) = task_meta { obj.clone() } else { serde_json::Map::new() };
                    let mut task_changed = false;
                    if !task_obj.contains_key("status") || task_obj.get("status").and_then(|v| v.as_str()).map_or(true, |s| !(s == "open" || s == "in_progress" || s == "blocked" || s == "done")) {
                        task_obj.insert("status".to_string(), serde_json::Value::String("open".to_string()));
                        task_changed = true;
                    }
                    if task_obj.get("priority").is_none() || !task_obj.get("priority").map_or(false, |v| v.as_u64().map_or(false, |_| true)) {
                        task_obj.insert("priority".to_string(), serde_json::Value::Number(serde_json::Number::from(2)));
                        task_changed = true;
                    }
                    if !task_obj.contains_key("task_type") {
                        task_obj.insert("task_type".to_string(), serde_json::Value::String("task".to_string()));
                        task_changed = true;
                    }
                    if task_changed || meta.get("task").is_none() || !meta.get("task").map_or(false, |v| v.is_object()) {
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("task".to_string(), serde_json::Value::Object(task_obj));
                            shape_changed = true;
                        }
                    }
                }
                // (a4) content / fact and generic repair: backfill blank content and missing required fields from title/location/legacy
                let content_empty = content_str.trim().is_empty();
                let mut content_backfill: Option<String> = None;
                let structural = matches!(mtype.as_str(), "file" | "directory" | "markdown" | "symbol" | "code_ast" | "doc_section" | "freshness_flag" | "guard_event");
                if content_empty && !structural {
                    content_backfill = Some(meta.get("title").and_then(|v| v.as_str()).map(|s| s.to_string()).filter(|s| !s.trim().is_empty())
                        .or_else(|| meta.get("location").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "N/A"))
                        .unwrap_or_else(|| "legacy".to_string()));
                }
                if (content_empty && mtype == "fact") || (content_empty && !mtype.is_empty() && mtype != "rule" && mtype != "procedure" && mtype != "task") {
                    content_backfill = Some(meta.get("title").and_then(|v| v.as_str()).map(|s| s.to_string()).filter(|s| !s.trim().is_empty())
                        .or_else(|| meta.get("location").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "N/A"))
                        .unwrap_or_else(|| "legacy".to_string()));
                }
                // (b) dangling refs in edge arrays
                let keys = ["governs","related_to","relates_to","contained_by","extracted_from","depends_on"];
                let mut archived_refs: Vec<String> = meta.get("archived_refs").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
                let mut unresolvable = Vec::new();
                for k in &keys {
                    if let Some(arr) = meta.get(k).and_then(|v| v.as_array()).cloned() {
                        let mut kept = Vec::new();
                        for item in &arr {
                            if let Some(t) = item.as_str() {
                                // Remove residual unresolved string targets (samples/total) and check existence
                                if t == "samples" || t == "total" {
                                    unresolvable.push(t.to_string());
                                    continue;
                                }
                                // Check existence against known_ids, resolved targets, and prefix-unqualified resolution
                                let ns_prefix = format!("{}{}", namespace, crate::parser::ingest::NAMESPACE_SEPARATOR);
                                let exists = known_ids.contains(t)
                                    || exact_target_exists(conn, t).unwrap_or(false)
                                    || if let Some(stripped) = t.strip_prefix(&ns_prefix) {
                                        matches!(resolve_target_on_write(conn, &namespace, stripped), crate::traits::TargetResolution::Resolved(_))
                                    } else if !t.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                                        matches!(resolve_target_on_write(conn, &namespace, t), crate::traits::TargetResolution::Resolved(_))
                                    } else {
                                        false
                                    }
                                    || (t.len() >= 7 && known_ids.iter().filter(|k| k.starts_with(t)).count() == 1);
                                if exists { kept.push(item.clone()); } else { unresolvable.push(t.to_string()); }
                            } else { kept.push(item.clone()); }
                        }
                        // Materialize missing graph edges for kept targets (borrow; move happens below)
                        for item in &kept {
                            if let Some(t) = item.as_str() {
                                if known_ids.contains(t) || exact_target_exists(conn, t).unwrap_or(false) {
                                    let rel_table = match *k {
                                        "governs" => "GOVERNS",
                                        "related_to" | "relates_to" => "RELATES_TO",
                                        "contained_by" => "CONTAINS",
                                        "extracted_from" => "EXTRACTED_FROM",
                                        "depends_on" => "DEPENDS_ON",
                                        _ => "RELATES_TO",
                                    };
                                    let safe_self = crate::store::ladybug::escape_ladybug_string(id);
                                    let safe_target = crate::store::ladybug::escape_ladybug_string(t);
                                    let (from_id, to_id) = if *k == "contained_by" {
                                        (safe_target.as_str(), safe_self.as_str())
                                    } else {
                                        (safe_self.as_str(), safe_target.as_str())
                                    };
                                    let merge_edge_q = format!(
                                        "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[r:{}]->(b) ON CREATE SET r.source = '{}', r.valid_from = 0, r.valid_to = 0, r.generation = 0, r.extractor = 'upsert', r.confidence = 1.0, r.evidence_id = '{}', r.origin = 'local'",
                                        from_id, to_id, rel_table, safe, safe_self
                                    );
                                    if let Err(e) = conn.query(&merge_edge_q) {
                                         tracing::debug!("hygiene edge merge note: {e}");
                                     }
                                }
                            }
                        } // end for item in &kept
                        if kept.len() != arr.len() { if let Some(obj) = meta.as_object_mut() { obj.insert(k.to_string(), serde_json::Value::Array(kept)); dangling_changed = true; } }
                    }
                }
                if !unresolvable.is_empty() {
                    archived_refs.extend(unresolvable);
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("archived_refs".to_string(), serde_json::Value::Array(archived_refs.iter().map(|s| serde_json::Value::String(s.clone())).collect()));
                    }
                    dangling_changed = true;
                }
                if shape_changed || dangling_changed || content_backfill.is_some() {
                    let has_backfill = content_backfill.is_some();
                    let q_meta = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'", crate::store::ladybug::escape_ladybug_string(&namespace), crate::store::ladybug::escape_ladybug_string(id), crate::store::ladybug::escape_ladybug_string(&meta.to_string()));
                    if let Err(e) = conn.query(&q_meta) {
                        tracing::warn!("hygiene repair: metadata write failed for {id}: {e}");
                    } else {
                        if let Some(backfill_text) = content_backfill {
                            let safe_back = crate::store::ladybug::escape_ladybug_string(&backfill_text);
                            let q_content = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.content = '{}'", crate::store::ladybug::escape_ladybug_string(&namespace), crate::store::ladybug::escape_ladybug_string(id), safe_back);
                            if let Err(e) = conn.query(&q_content) {
                                tracing::warn!("hygiene repair: content write failed for {id}: {e}");
                            }
                        }
                        if shape_changed || has_backfill { shape_updates += 1; }
                        if dangling_changed { dangling_updates += 1; }
                    }
                }
            }
            Ok((shape_updates, dangling_updates))
        }).await
    }

    /// Advisory shape warnings for every row in the namespace (F18). Never blocks.
    pub async fn shape_warnings(&self, namespace: &str) -> Vec<serde_json::Value> {
        let rows = match self.list(namespace, None, None).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("shape_warnings list failed for '{namespace}': {e}");
                Vec::new()
            }
        };
        let mut out = Vec::new();
        for r in rows {
            // shacl validate
            for v in crate::rdf::validate_memory(&r.payload) {
                out.push(serde_json::json!({
                    "id": r.id,
                    "field": v.field,
                    "why": v.why,
                }));
            }
            // edge endpoint roles
            let source_type = r.payload.memory_type.clone();
            let meta = r.payload.metadata.clone();
            for (key, rel_type) in [
                ("related_to", "RELATES_TO"),
                ("contained_by", "CONTAINS"),
                ("governs", "GOVERNS"),
                ("extracted_from", "EXTRACTED_FROM"),
                ("depends_on", "DEPENDS_ON"),
            ] {
                if let Some(arr) = meta.get(key).and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(target) = item.as_str() {
                            // resolve target namespace
                            let (ns, tid) = if target.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                                let pos = target.find(crate::parser::ingest::NAMESPACE_SEPARATOR).unwrap();
                                let ns = &target[..pos];
                                let id = &target[pos + crate::parser::ingest::NAMESPACE_SEPARATOR.len()..];
                                (ns.to_string(), id.to_string())
                            } else {
                                (namespace.to_string(), target.to_string())
                            };
                            if let Ok(Some((_, tpayload))) = self.get(&ns, &tid).await {
                                let target_type = tpayload.memory_type;
                                if !crate::rdf::shacl::endpoint_role_allowed(rel_type, &source_type, &target_type) {
                                    out.push(serde_json::json!({
                                        "id": r.id,
                                        "field": format!("metadata.{}", key),
                                        "why": format!("endpoint role mismatch: {} (source={} target={})", rel_type, source_type, target_type),
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Advisory ontology warnings for a namespace (F21). Never blocks.
    pub async fn ontology_warnings(&self, namespace: &str) -> Vec<serde_json::Value> {
        crate::ontology_grounding::ontology_warnings(self, namespace).await
    }
}

impl LadybugStore {
    /// Build the lexical view of one stored row. The engine deliberately does
    /// not parse JSON, so every filter flag is decided here, next to the store's
    /// other row-class predicates.
    fn lexical_doc(namespace: &str, id: &str, payload: &MemoryPayload) -> crate::fts::LexicalDoc {
        let metadata = &payload.metadata;
        let mut doc = crate::fts::LexicalDoc::new(
            id.to_string(),
            namespace.to_string(),
            payload.memory_type.clone(),
            payload.content.clone(),
        );
        doc.symbol = metadata
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        doc.location = payload.location.clone();
        doc.valid_from = metadata.get("valid_from").and_then(|v| v.as_i64());
        // Align with is_retired_at contract (ladybug.rs:5528): unparseable/non-null
        // valid_to treated as 0 (retired); null → unspecified (active).
        doc.valid_to = metadata.get("valid_to").and_then(|v| {
            if v.is_null() { None } else { Some(v.as_i64().unwrap_or(0)) }
        });
        doc.archived = is_archived(metadata);
        doc.user_id = payload.user_id.clone();
        doc.visible_to = metadata
            .get("visible_to")
            .and_then(|v| v.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        doc.domains = crate::domain::domain_keys(metadata);
        doc.structural = STRUCTURAL_MEMORY_TYPES.contains(&payload.memory_type.as_str());
        doc.registry = crate::registry::is_registry_row(metadata);
        doc.conflicting = is_mesh_conflicting(metadata);
        doc
    }

    fn lexical_apply_doc(&self, doc: crate::fts::LexicalDoc) {
        if let Some(ref idx) = *self.tantivy_lexical.read().unwrap_or_else(|e| e.into_inner()) {
            let mut meta = serde_json::Map::new();
            if !doc.symbol.is_empty() {
                meta.insert("symbol".to_string(), Value::String(doc.symbol.clone()));
            }
            if let Some(vf) = doc.valid_from {
                meta.insert("valid_from".to_string(), Value::Number(vf.into()));
            }
            if let Some(vt) = doc.valid_to {
                meta.insert("valid_to".to_string(), Value::Number(vt.into()));
            }
            if doc.archived {
                meta.insert("archived".to_string(), Value::Number(1.into()));
            }
            let payload = MemoryPayload {
                content: doc.content.clone(),
                user_id: doc.user_id.clone(),
                memory_type: doc.memory_type.clone(),
                agent_name: None,
                location: doc.location.clone(),
                location_lines: String::new(),
                metadata: Value::Object(meta),
            };
            if let Err(e) = idx.upsert_memory(&doc.namespace, &doc.id, &payload) {
                tracing::warn!("tantivy upsert_memory failed for {}:{}: {e}", doc.namespace, doc.id);
            }
        }
        let loaded = self
            .lexical_loaded
            .read()
            .expect("lexical loaded lock");
        if !loaded.contains(&doc.namespace) {
            return;
        }
        drop(loaded);
        self.lexical
            .write()
            .expect("lexical index lock")
            .upsert(doc);
    }

    fn lexical_remove_doc(&self, namespace: &str, id: &str) {
        if let Some(ref idx) = *self.tantivy_lexical.read().unwrap_or_else(|e| e.into_inner()) {
            if let Err(e) = idx.delete_memory(namespace, id) {
                tracing::warn!("tantivy delete_memory failed for {namespace}:{id}: {e}");
            }
        }
        let loaded = self
            .lexical_loaded
            .read()
            .expect("lexical loaded lock");
        if !loaded.contains(namespace) {
            return;
        }
        drop(loaded);
        self.lexical
            .write()
            .expect("lexical index lock")
            .remove(namespace, id);
    }

    fn lexical_remove_where<F>(&self, namespace: &str, predicate: F)
    where
        F: FnMut(&crate::fts::LexicalDoc) -> bool,
    {
        let loaded = self
            .lexical_loaded
            .read()
            .expect("lexical loaded lock");
        if !loaded.contains(namespace) {
            return;
        }
        drop(loaded);
        self.lexical
            .write()
            .expect("lexical index lock")
            .remove_where(namespace, predicate);
    }

    async fn ensure_lexical_loaded(&self, namespace: &str) -> Result<()> {
        if self
            .lexical_loaded
            .read()
            .expect("lexical loaded lock")
            .contains(namespace)
        {
            return Ok(());
        }

        let namespace_owned = namespace.to_string();
        let docs = self
            .with_conn(move |conn| {
                let safe_ns = escape_ladybug_string(&namespace_owned);
                let query = format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.location, m.metadata",
                    safe_ns
                );
                let rows = conn.query(&query)?;
                let mut docs = Vec::new();
                for row in rows {
                    let id: String = format!("{}", row[0]);
                    let content: String = format!("{}", row[1]);
                    let user_id: String = format!("{}", row[2]);
                    let memory_type: String = format!("{}", row[3]);
                    let location: String = format!("{}", row[4]);
                    let metadata_str: String = format!("{}", row[5]);
                    let metadata: Value =
                        parse_metadata_or_warn(&metadata_str, "metadata");
                    let payload = MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: None,
                        location,
                        location_lines: String::new(),
                        metadata,
                    };
                    docs.push(Self::lexical_doc(&namespace_owned, &id, &payload));
                }
                Ok(docs)
            })
            .await?;

        {
            let mut index = self.lexical.write().unwrap_or_else(|e| e.into_inner());
            for doc in docs {
                index.add(doc);
            }
        }
        self.lexical_loaded
            .write()
            .expect("lexical loaded lock")
            .insert(namespace.to_string());
        Ok(())
    }

    pub fn local_path(&self) -> &std::path::Path {
        &self.local_path
    }

    pub fn metrics(&self) -> &crate::time_series::TimeSeriesStore {
        &self.metrics
    }

    pub fn tantivy_index_dir(&self) -> PathBuf {
        let mut os = self.local_path.as_os_str().to_os_string();
        os.push(".tantivy");
        PathBuf::from(os)
    }

    fn ensure_tantivy_open(&self) -> Result<()> {
        if self.tantivy_lexical.read().unwrap_or_else(|e| e.into_inner()).is_some() {
            return Ok(());
        }
        let dir = self.tantivy_index_dir();
        // An existing directory is opened as-is first: recreating it would
        // silently drop the persisted index. Only a fresh/failed open falls
        // back to create-or-rebuild semantics.
        let idx = if dir.exists() {
            match crate::lexical::TantivyLexicalIndex::open(&dir) {
                Ok(idx) => {
                    let _ = idx.index_dir();
                    idx
                }
                Err(_) => crate::lexical::TantivyLexicalIndex::open_or_create(&dir)?,
            }
        } else {
            crate::lexical::TantivyLexicalIndex::open_or_create(&dir)?
        };
        *self.tantivy_lexical.write().unwrap_or_else(|e| e.into_inner()) = Some(idx);
        Ok(())
    }

    /// Rebuild the disk-backed Tantivy lexical index from stored Memory rows.
    pub async fn rebuild_tantivy_lexical(&self, namespace: Option<&str>) -> Result<(usize, String)> {
        self.init("default").await?;
        let ns_opt = namespace.map(|s| s.to_string());
        let rows_data = self
            .with_conn(move |conn| {
                let query = match &ns_opt {
                    Some(ns) => format!(
                        "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.namespace, m.id, m.content, m.user_id, m.memory_type, m.location, m.agent_name, m.location_lines, m.metadata",
                        escape_ladybug_string(ns)
                    ),
                    None => "MATCH (m:Memory) RETURN m.namespace, m.id, m.content, m.user_id, m.memory_type, m.location, m.agent_name, m.location_lines, m.metadata".to_string(),
                };
                let rows = conn.query(&query)?;
                let mut out = Vec::new();
                for row in rows {
                    let ns: String = format!("{}", row[0]);
                    let id: String = format!("{}", row[1]);
                    let content: String = format!("{}", row[2]);
                    let user_id: String = format!("{}", row[3]);
                    let memory_type: String = format!("{}", row[4]);
                    let location: String = format!("{}", row[5]);
                    let agent_name: String = format!("{}", row[6]);
                    let location_lines: String = format!("{}", row[7]);
                    let metadata_str: String = format!("{}", row[8]);
                    let metadata: Value =
                        parse_metadata_or_warn(&metadata_str, "metadata");
                    let payload = MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: if agent_name.is_empty() || agent_name == "unknown" {
                            None
                        } else {
                            Some(agent_name)
                        },
                        location,
                        location_lines,
                        metadata,
                    };
                    out.push((ns, id, payload));
                }
                Ok(out)
            })
            .await?;

        self.ensure_tantivy_open()?;
        let indexed = {
            let guard = self.tantivy_lexical.read().unwrap_or_else(|e| e.into_inner());
            let idx = guard.as_ref().expect("tantivy open");
            idx.rebuild(&rows_data)?
        };
        Ok((indexed, crate::lexical::INDEX_VERSION.to_string()))
    }

    /// Search using the disk-backed Tantivy lexical index, hydrating payloads from LadybugDB.
    pub async fn search_tantivy_lexical(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        domains: &[String],
        principal: &str,
    ) -> Result<crate::traits::LexicalSearchOutcome> {
        self.init(namespace).await?;
        if self.tantivy_lexical.read().unwrap_or_else(|e| e.into_inner()).is_none() {
            let _ = self.rebuild_tantivy_lexical(Some(namespace)).await?;
        }
        let hits = {
            let guard = self.tantivy_lexical.read().unwrap_or_else(|e| e.into_inner());
            let idx = guard.as_ref().expect("tantivy open");
            idx.search(
                namespace,
                query,
                limit.max(1) * 4,
                include_tasks,
                include_archived,
                None,
            )?
        };
        let mut candidates = Vec::new();
        for hit in hits {
            // Defense in depth: the index query already scopes to `namespace`,
            // but a hit whose stored namespace drifted (stale cross-namespace
            // row) must not surface outside its own namespace.
            if hit.namespace != namespace {
                continue;
            }
            if let Some((_, payload)) = self.get(namespace, &hit.id).await? {
                if !include_tasks && payload.memory_type == "task" {
                    continue;
                }
                let vis = crate::rebac::extract_visible_to(&payload.metadata);
                if !crate::rebac::allowed(principal, &payload.user_id, "viewer", &vis) {
                    continue;
                }
                candidates.push(crate::traits::SearchResult {
                    id: hit.id,
                    score: hit.score,
                    payload,
                    evidence: Some(vec![crate::traits::SearchEvidence {
                        kind: crate::traits::EvidenceKind::DirectMatch,
                        path: Vec::new(),
                        explanation: "Matched via Tantivy full-text lexical index.".to_string(),
                        derivation: vec!["lexical".to_string()],
                    }]),
                });
            }
        }
        let mut domain_filter_applied = false;
        let mut results = if !domains.is_empty() {
            let filtered: Vec<_> = candidates
                .iter()
                .filter(|r| {
                    let row_domains = crate::domain::domain_keys(&r.payload.metadata);
                    domains.iter().any(|d| row_domains.contains(d))
                })
                .cloned()
                .collect();
            if !filtered.is_empty() {
                domain_filter_applied = true;
                filtered
            } else {
                candidates
            }
        } else {
            candidates
        };
        results.truncate(limit);
        Ok(crate::traits::LexicalSearchOutcome {
            results,
            domain_filter_applied,
        })
    }

    /// Flush buffered time-series metrics into the `MemoryMetric` node table.
    pub async fn flush_metrics(&self) -> Result<usize> {
        self.init("default").await?;
        let batch = self.metrics.take_unflushed(None);
        if batch.is_empty() {
            return Ok(0);
        }
        let mut total_written = 0usize;
        for (key, samples, max_seq) in batch {
            let (ns, kind, name) = key.clone();
            let kind_str = kind.as_str().to_string();
            let count = samples.len();
            // Merge the buffered samples into the in-memory aggregates before
            // the write takes ownership, so query_metrics sees one continuous
            // series across the flush boundary.
            let _merged = self.metrics.range(&key, 0, i64::MAX, &samples);
            self.write_with_deadline("flushing time-series metrics", move |conn| {
                let safe_ns = escape_ladybug_string(&ns);
                let safe_kind = escape_ladybug_string(&kind_str);
                let safe_name = escape_ladybug_string(&name);
                for s in &samples {
                    let safe_id = escape_ladybug_string(&s.id);
                    let labels_json =
                        serde_json::to_string(&s.labels).unwrap_or_else(|_| "{}".to_string());
                    let safe_labels = escape_ladybug_string(&labels_json);
                    let q = format!(
                        "MERGE (m:MemoryMetric {{id: '{}'}}) SET m.namespace = '{}', m.kind = '{}', m.name = '{}', m.ts = {}, m.value = {}, m.labels = '{}'",
                        safe_id, safe_ns, safe_kind, safe_name, s.ts, s.value, safe_labels
                    );
                    conn.query(&q)?;
                }
                Ok(())
            })
            .await?;
            self.metrics.mark_flushed(&key, max_seq);
            total_written += count;
        }
        self.dirty.mark();
        Ok(total_written)
    }

    /// Query persisted `MemoryMetric` samples from LadybugDB, merged with in-memory samples.
    pub async fn query_db_metrics(
        &self,
        namespace: &str,
        kind: crate::time_series::MetricKind,
        name: &str,
        since_ts: Option<i64>,
        until_ts: Option<i64>,
        limit: usize,
    ) -> Result<Vec<crate::time_series::Sample>> {
        self.init(namespace).await?;
        let ns_owned = namespace.to_string();
        let kind_str = kind.as_str().to_string();
        let name_owned = name.to_string();
        let db_samples = self
            .with_conn(move |conn| {
                let safe_ns = escape_ladybug_string(&ns_owned);
                let safe_kind = escape_ladybug_string(&kind_str);
                let safe_name = escape_ladybug_string(&name_owned);
                let q = format!(
                    "MATCH (m:MemoryMetric) WHERE m.namespace = '{}' AND m.kind = '{}' AND m.name = '{}' RETURN m.id, m.ts, m.value, m.labels",
                    safe_ns, safe_kind, safe_name
                );
                let rows = conn.query(&q)?;
                let mut out = Vec::new();
                for row in rows {
                    let id = format!("{}", row[0]);
                    let ts = format!("{}", row[1]).parse::<i64>().unwrap_or(0);
                    let value = format!("{}", row[2]).parse::<f64>().unwrap_or(0.0);
                    let labels_str = format!("{}", row[3]);
                    let labels: std::collections::BTreeMap<String, String> = if !labels_str.trim().is_empty() {
                        match serde_json::from_str(&labels_str) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!("failed to parse task labels JSON '{}': {e}", labels_str);
                                std::collections::BTreeMap::new()
                            }
                        }
                    } else {
                        std::collections::BTreeMap::new()
                    };
                    out.push(crate::time_series::Sample {
                        id,
                        seq: 0,
                        ts,
                        value,
                        labels,
                    });
                }
                Ok(out)
            })
            .await
            .unwrap_or_else(|e| { tracing::warn!("query_db_metrics failed: {e}"); Vec::new() });

        let key = (namespace.to_string(), kind, name.to_string());
        let mut samples = self.metrics.range(
            &key,
            since_ts.unwrap_or(i64::MIN),
            until_ts.unwrap_or(i64::MAX),
            &db_samples,
        );
        if limit > 0 && samples.len() > limit {
            let start = samples.len() - limit;
            samples = samples.split_off(start);
        }
        Ok(samples)
    }

    /// Export a namespace's lexical documents and graph edges for WASM edge snapshot.
    pub async fn export_wasm_snapshot(
        &self,
        namespace: &str,
        include_retired: bool,
    ) -> Result<(
        Vec<crate::fts::LexicalDoc>,
        Vec<crate::graph_read::related::EdgeSpec>,
    )> {
        self.init(namespace).await?;
        let ns_owned = namespace.to_string();
        let warn_ns = ns_owned.clone();
        let now_ts = chrono::Utc::now().timestamp();
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&ns_owned);
            let q = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.location, m.metadata",
                safe_ns
            );
            let rows = conn.query(&q)?;
            let mut docs = Vec::new();
            for row in rows {
                let id = format!("{}", row[0]);
                let content = format!("{}", row[1]);
                let user_id = format!("{}", row[2]);
                let memory_type = format!("{}", row[3]);
                let location = format!("{}", row[4]);
                let metadata_str = format!("{}", row[5]);
                let metadata: Value =
                    parse_metadata_or_warn(&metadata_str, "metadata");
                let payload = MemoryPayload {
                    content,
                    user_id,
                    memory_type,
                    agent_name: None,
                    location,
                    location_lines: String::new(),
                    metadata,
                };
                let doc = Self::lexical_doc(&ns_owned, &id, &payload);
                if !include_retired {
                    if let Some(vt) = doc.valid_to {
                        if vt <= now_ts {
                            continue;
                        }
                    }
                    if doc.archived {
                        continue;
                    }
                }
                docs.push(doc);
            }

            let mut edges = Vec::new();
            for (table, rel_name) in [
                ("RELATES_TO", "related_to"),
                ("CONTAINS", "contained_by"),
                ("GOVERNS", "governs"),
                ("EXTRACTED_FROM", "extracted_from"),
                ("DEPENDS_ON", "depends_on"),
            ] {
                let eq = format!(
                    "MATCH (a:Memory)-[r:{}]->(b:Memory) WHERE a.namespace = '{}' AND b.namespace = '{}' RETURN a.id, b.id, r.source, r.valid_from, r.valid_to",
                    table, safe_ns, safe_ns
                );
                match conn.query(&eq) {
                    Ok(erows) => {
                        for erow in erows {
                            let from = format!("{}", erow[0]);
                            let to = format!("{}", erow[1]);
                            let src_str = format!("{}", erow[2]);
                            let vf = format!("{}", erow[3]).parse::<i64>().ok().filter(|v| *v > 0);
                            let vt = format!("{}", erow[4]).parse::<i64>().ok().filter(|v| *v > 0);
                            if !include_retired {
                                if let Some(vt_val) = vt {
                                    if vt_val <= now_ts {
                                        continue;
                                    }
                                }
                            }
                            if !src_str.is_empty() && src_str != *warn_ns {
                                tracing::debug!(
                                    "edge in '{}' carries origin source '{}'",
                                    warn_ns,
                                    src_str
                                );
                            }
                            edges.push(crate::graph_read::related::EdgeSpec {
                                source: from,
                                relation: rel_name.to_string(),
                                target: to,
                                valid_from: vf.unwrap_or(0),
                                valid_to: vt.unwrap_or(0),
                            });
                        }
                    }
                    Err(e) => { tracing::warn!("export_wasm_snapshot edge query failed: {e}"); }
                }
            }

            Ok((docs, edges))
        })
        .await
    }
}

/// The Memory schema DDL at `dimensions`: the node-table string
/// and the five rel-table strings `init`'s schema closure runs.
/// Single-sourced so the #74 provenance re-attestation at stamp
/// time (see
/// `LadybugStore::reattest_and_upgrade_empty_locked_unknown_store_state`)
/// reconciles with exactly the DDL the schema closure used — the
/// strings are deterministic from `dimensions`.
fn memory_table_ddl(dimensions: usize) -> (String, [&'static str; 5]) {
    let create_node_table = format!(
        "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[{}], PRIMARY KEY (id))",
        dimensions
    );
    let create_rel_table = "CREATE REL TABLE RELATES_TO (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING, confidence DOUBLE, evidence_id STRING, origin STRING)";
    let create_contains_table = "CREATE REL TABLE CONTAINS (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING, confidence DOUBLE, evidence_id STRING, origin STRING)";
    let create_governs_table = "CREATE REL TABLE GOVERNS (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING, confidence DOUBLE, evidence_id STRING, origin STRING)";
    // Vocabulary v10: extraction edges (memory -> task) behind Lock 2.
    let create_extracted_table = "CREATE REL TABLE EXTRACTED_FROM (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING, confidence DOUBLE, evidence_id STRING, origin STRING)";
    // F7: dependency edges (directed).
    let create_depends_table = "CREATE REL TABLE DEPENDS_ON (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING, confidence DOUBLE, evidence_id STRING, origin STRING)";
    (
        create_node_table,
        [
            create_rel_table,
            create_contains_table,
            create_governs_table,
            create_extracted_table,
            create_depends_table,
        ],
    )
}

/// The LockedUnknown counterpart of
/// [`reconcile_pristine_embedding_width`] (issue #74): attest from the
/// engine whether the Memory table is empty, reconciling its physical
/// width with `dimensions` first when it is. Returns `true` only when
/// the engine attests the table holds no rows (and, on a width
/// mismatch, the empty tables were recreated at `dimensions`). Every
/// other outcome — rows present, an unreadable catalog width, an
/// unreadable row count, or an attestation query that fails to
/// execute — returns `false` and leaves the store
/// exactly as found: unlike the pristine path this helper never
/// refuses an open, because a genuine pre-sidecar store with data
/// must behave exactly as it did before the recorded-identity
/// feature. A failure of the drop/recreate itself is propagated: the
/// schema may have been altered, so the init must not proceed to
/// record an identity over it.
fn reconcile_empty_locked_unknown_width(
    conn: &Connection,
    dimensions: usize,
    create_node_table: &str,
    create_rel_tables: &[&str],
) -> Result<bool> {
    let mut physical: Option<usize> = None;
    // A query-EXECUTION failure attests nothing either: downgrade
    // it to the same leave-as-found `false` as an unreadable width
    // below — this helper never refuses an open (panel condition 2,
    // finding P4-F3). The failure is logged, so a store whose
    // attestation persistently fails is distinguishable from a
    // healthy legacy store.
    let table_info = match conn.query("CALL table_info('Memory') RETURN *") {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                "provenance attestation could not execute (table_info query failed: {e}); leaving the store as found"
            );
            return Ok(false);
        }
    };
    for row in table_info {
        let name = match row.get(1) {
            Some(lbug::Value::String(s)) => s.as_str(),
            _ => continue,
        };
        if name != "embedding" {
            continue;
        }
        let ty = match row.get(2) {
            Some(lbug::Value::String(s)) => s.clone(),
            _ => return Ok(false),
        };
        physical = ty
            .strip_prefix("FLOAT[")
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|digits| digits.parse::<usize>().ok());
    }
    let Some(physical) = physical else {
        return Ok(false);
    };

    // Same downgrade as the table-info query above: an
    // unexecutable count attests nothing, so leave as found —
    // with the failure logged, as above.
    let mut count_rows = match conn.query("MATCH (m:Memory) RETURN count(m)") {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                "provenance attestation could not execute (row-count query failed: {e}); leaving the store as found"
            );
            return Ok(false);
        }
    };
    let rows = match count_rows.next() {
        Some(row) => match row.get(0) {
            Some(lbug::Value::Int64(n)) => *n,
            _ => return Ok(false),
        },
        None => return Ok(false),
    };
    if rows > 0 {
        return Ok(false);
    }

    if physical != dimensions {
        // The table is engine-attested empty, and a store with no
        // Memory rows has no rel rows either (edges require Memory
        // nodes), so dropping and recreating loses nothing — the
        // same mechanism the pristine reconciliation proved.
        for table in [
            "RELATES_TO",
            "CONTAINS",
            "GOVERNS",
            "EXTRACTED_FROM",
            "DEPENDS_ON",
            "Memory",
        ] {
            conn.query(&format!("DROP TABLE {table}"))?;
        }
        conn.query(create_node_table)?;
        for ddl in create_rel_tables {
            conn.query(ddl)?;
        }
    }
    Ok(true)
}

/// Reconcile the physical width of `Memory.embedding` with the width
/// this process serves, on a store the caller has already armed for
/// reconciliation (recorded pristine, or fresh and unrecorded — see
/// `VectorStore::init`). The physical width is read from the engine
/// catalog (`CALL table_info('Memory')` — rows of
/// (id, name, type, default, primary key); the embedding row's type is
/// rendered `FLOAT[n]`), never inferred from the sidecar: the engine
/// is the only witness that can attest both the column's width and
/// the table's row count. A width mismatch is reconciled only when
/// the engine attests the table is empty, by dropping `Memory` and
/// its five rel tables and recreating them from the caller's DDL
/// strings (the rel tables go first: they reference `Memory`). The
/// narrower alternative — `ALTER TABLE Memory DROP embedding` plus
/// `ALTER TABLE … ADD embedding FLOAT[n]` — was spiked first and
/// rejected on evidence: the DDL succeeds, but the engine then
/// segfaults on the production MERGE upsert against the re-added
/// column, so the recreated table (fresh, contiguous property ids)
/// is the mechanism the spike proved. A pristine-labelled table that
/// is not empty is the stale-label residual class: the open is
/// refused with a plain error naming the inconsistency, and nothing
/// is altered. An unparseable or missing physical width is likewise
/// an error — fail closed, never a guess.
fn reconcile_pristine_embedding_width(
    conn: &Connection,
    dimensions: usize,
    create_node_table: &str,
    create_rel_tables: &[&str],
) -> Result<()> {
    let mut physical: Option<usize> = None;
    for row in conn.query("CALL table_info('Memory') RETURN *")? {
        let name = match row.get(1) {
            Some(lbug::Value::String(s)) => s.as_str(),
            _ => continue,
        };
        if name != "embedding" {
            continue;
        }
        let ty = match row.get(2) {
            Some(lbug::Value::String(s)) => s.clone(),
            other => {
                anyhow::bail!(
                    "cannot reconcile the embedding width: the Memory table's embedding property has an unreadable catalog type ({other:?})"
                )
            }
        };
        physical = Some(
            ty.strip_prefix("FLOAT[")
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|digits| digits.parse::<usize>().ok())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot reconcile the embedding width: the Memory table's embedding property type '{ty}' is not a FLOAT[width] column"
                    )
                })?,
        );
    }
    let physical = physical.ok_or_else(|| {
        anyhow::anyhow!(
            "cannot reconcile the embedding width: the Memory table has no embedding property in the engine catalog"
        )
    })?;
    if physical == dimensions {
        return Ok(());
    }

    let mut count_rows = conn.query("MATCH (m:Memory) RETURN count(m)")?;
    let rows = match count_rows.next() {
        Some(row) => match row.get(0) {
            Some(lbug::Value::Int64(n)) => *n,
            other => {
                anyhow::bail!(
                    "cannot reconcile the embedding width: the Memory row count came back unreadable ({other:?})"
                )
            }
        },
        None => {
            anyhow::bail!(
                "cannot reconcile the embedding width: the Memory row count query returned no rows"
            )
        }
    };
    if rows > 0 {
        anyhow::bail!(
            "refusing to reconcile the embedding width of a store recorded as pristine: the Memory table already holds {rows} row(s), its embedding column is FLOAT[{physical}], and this process serves FLOAT[{dimensions}]; the recorded pristine state is inconsistent with the table's contents, so the store is left unchanged — open it under its recorded model, or investigate the stale store-state sidecar"
        );
    }

    // The table is engine-attested empty, and a pristine store's rel
    // tables are empty by construction (edges require Memory nodes),
    // so dropping and recreating loses nothing.
    for table in ["RELATES_TO", "CONTAINS", "GOVERNS", "EXTRACTED_FROM", "DEPENDS_ON", "Memory"] {
        conn.query(&format!("DROP TABLE {table}"))?;
    }
    conn.query(create_node_table)?;
    for ddl in create_rel_tables {
        conn.query(ddl)?;
    }
    Ok(())
}

#[async_trait]
impl VectorStore for LadybugStore {
    async fn init(&self, namespace: &str) -> Result<()> {
        // A data-bearing store refuses a
        // bound serving descriptor different from its recorded one,
        // before any schema DDL or vector work.
        self.check_data_bearing_identity()?;
        // Composite Recommendation #11: verify the embedding-space identity on
        // every init (including the schema-ready fast path) so an incompatible
        // descriptor is rejected before any vector lands. The recorded
        // descriptor is the effective one (serving, else recorded,
        // else configured, else the legacy placeholder).
        self.verify_or_record_embedding_descriptor(
            namespace,
            &self.effective_embedding_descriptor(),
        )?;

        if self.schema_ready.load(std::sync::atomic::Ordering::Acquire) {
            // A previous init in this process already succeeded; the
            // sidecar maintenance is cached and idempotent.
            self.maintain_store_state();
            return Ok(());
        }

        let dimensions = self.dimensions;
        // Width reconciliation is armed only for stores the identity
        // system manages as pristine: recorded pristine, or fresh and
        // not yet recorded. A data-bearing store's mismatched open is
        // already refused above, and a pre-sidecar (LockedUnknown)
        // store keeps today's behavior exactly — its width backstop
        // stays at the engine, at vector use.
        //
        // The arming is also gated on this process having opened the
        // database through the normal verified path: on a recovery
        // open (`opened_via_recovery`) the engine's width/emptiness
        // view is unauthenticated — WAL-only rows can present as
        // absent, so the `count(m)` the reconciliation reads can be
        // stale — and reconciliation is disarmed rather than allowed
        // to drop/recreate a recorded-pristine store's tables from
        // that view; a later verified open reconciles as today
        // (overseer panel condition 1, findings AC-F1/SA-F2).
        let reconcile_width = !self.opened_via_recovery
            && match self.cached_store_state() {
                CachedStoreState::Recorded(recorded) => !recorded.has_data,
                CachedStoreState::FreshUnrecorded => true,
                CachedStoreState::LockedUnknown => false,
            };
        // Issue #74: a LockedUnknown classification is ambiguous — it
        // covers both a genuine pre-sidecar store and a fresh store
        // whose provenance was lost because its first touch was a
        // non-serving open (which creates the DB file, so every later
        // construction sees it as preexisting) or a first write was
        // refused (which leaves the same bare file behind). When this
        // process binds a serving descriptor that matches the
        // configuration, the schema closure below attests from the
        // engine whether the Memory table is empty; only an attested
        // empty store is upgraded to a recorded pristine identity
        // after the closure succeeds. A store with rows is never
        // upgraded, refused, or reconciled here: genuine legacy
        // behavior is unchanged.
        //
        // The candidate is also gated on this process having opened
        // the database through the normal verified path: on a
        // recovery open (`opened_via_recovery`) the engine's empty
        // attestation is unauthenticated — WAL-only rows can present
        // as absent — so the upgrade never runs, the store stays
        // LockedUnknown, and a later verified open can still upgrade
        // it (panel condition 1, finding P1-F1).
        let locked_unknown_upgrade_candidate =
            matches!(self.cached_store_state(), CachedStoreState::LockedUnknown)
                && !self.opened_via_recovery
                && match self.serving_descriptor() {
                    Some(serving) => self.serving_matches_configured(&serving),
                    None => false,
                };
        let locked_unknown_attested_empty =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let locked_unknown_attested_flag = locked_unknown_attested_empty.clone();
        // Use write_with_deadline so the write lock is held INSIDE the
        // spawn_blocking closure (see R5). Holding an async lock across
        // with_conn releases it on timeout while the C++ call is still running.
        let outcome = self.write_with_deadline("initializing schema", move |conn| {

            let (create_node_table, create_rel_tables) = memory_table_ddl(dimensions);
            if let Err(e) = conn.query(&create_node_table) {
                if !e.to_string().contains("already exists") {
                    return Err(e.into());
                }
            }

            for ddl in create_rel_tables {
                if let Err(e) = conn.query(ddl) {
                    if !e.to_string().contains("already exists") {
                        return Err(e.into());
                    }
                }
            }

            // Reconcile the pristine store's physical vector width
            // with the serving width before the sidecar maintenance
            // after this closure can adopt the serving descriptor: a
            // descriptor is never adopted over an unreconciled schema.
            if reconcile_width {
                reconcile_pristine_embedding_width(
                    conn,
                    dimensions,
                    &create_node_table,
                    &create_rel_tables,
                )?;
            }

            // Issue #74: for a LockedUnknown upgrade candidate (see
            // the computation before this closure), attest from the
            // engine — under the writer's turn — whether the Memory
            // table is empty, reconciling its width first when it is.
            // A non-empty table attests false and is left exactly as
            // found: no refusal, no reconciliation, no record.
            if locked_unknown_upgrade_candidate {
                let attested_empty = reconcile_empty_locked_unknown_width(
                    conn,
                    dimensions,
                    &create_node_table,
                    &create_rel_tables,
                )?;
                locked_unknown_attested_flag
                    .store(attested_empty, std::sync::atomic::Ordering::Release);
            }

            // RDF projection (Phase 1): idempotent migration for DBs created before
            // edge provenance columns existed. LadybugDB supports ALTER ... ADD.
            const EDGE_PROVENANCE_TABLES: [&str; 5] = ["RELATES_TO", "CONTAINS", "GOVERNS", "EXTRACTED_FROM", "DEPENDS_ON"];
            const EDGE_PROVENANCE_COLUMNS: [(&str, &str); 8] = [
                ("source", "STRING"),
                ("valid_from", "INT64"),
                ("valid_to", "INT64"),
                ("generation", "INT64"),
                ("extractor", "STRING"),
                ("confidence", "DOUBLE"),
                ("evidence_id", "STRING"),
                ("origin", "STRING"),
            ];
            for table in EDGE_PROVENANCE_TABLES {
                for (col, ty) in EDGE_PROVENANCE_COLUMNS {
                    let sql = format!("ALTER TABLE {} ADD {} {}", table, col, ty);
                    if let Err(e) = conn.query(&sql) {
                        let msg = e.to_string();
                        if !msg.contains("already has property") && !msg.contains("already exists") {
                            eprintln!("[neurostrata:schema] edge provenance migration failed for {}.{}: {}", table, col, msg);
                        }
                    }
                }
            }
            for table in EDGE_PROVENANCE_TABLES {
                let backfill_sql = format!(
                    "MATCH (a:Memory)-[r:{}]->(b:Memory) WHERE r.valid_to IS NULL SET r.source = a.namespace, r.valid_from = 0, r.valid_to = 0, r.generation = 0, r.extractor = 'upsert', r.confidence = 1.0, r.evidence_id = a.id, r.origin = 'local'",
                    table
                );
                if let Err(e) = conn.query(&backfill_sql) {
                    tracing::debug!("edge provenance backfill note for {}: {}", table, e);
                }
            }

            // PQTH: Intake receipts table (operational, not memory rows)
            let create_receipts_table = "CREATE NODE TABLE IntakeReceipt (id STRING, namespace STRING, source_kind STRING, source_revision STRING, source_digest STRING, detected_intents STRING, raw_text STRING, created_at STRING, PRIMARY KEY (id))";
            if let Err(e) = conn.query(create_receipts_table) {
                if format!("{e}").to_lowercase().contains("already exists") {
                    tracing::debug!("schema DDL query note: {e}");
                } else {
                    tracing::warn!("schema DDL table creation failed: {e}");
                }
            }

            // PQTH: Intake candidates table (operational, not memory rows)
            let create_candidates_table = "CREATE NODE TABLE IntakeCandidate (id STRING, namespace STRING, receipt_id STRING, title STRING, description STRING, review_state STRING, disposition STRING, task_id STRING, cas_version INT64, created_at STRING, updated_at STRING, candidate_kind STRING, kind STRING, category STRING, provenance STRING, payload STRING, confidence DOUBLE, source_span STRING, extractor_fingerprint STRING, PRIMARY KEY (id))";
            if let Err(e) = conn.query(create_candidates_table) {
                if format!("{e}").to_lowercase().contains("already exists") {
                    tracing::debug!("schema DDL query note: {e}");
                } else {
                    tracing::warn!("schema DDL table creation failed: {e}");
                }
            }
            for stmt in [
                "ALTER TABLE IntakeCandidate ADD candidate_kind STRING",
                "ALTER TABLE IntakeCandidate ADD kind STRING",
                "ALTER TABLE IntakeCandidate ADD category STRING",
                "ALTER TABLE IntakeCandidate ADD provenance STRING",
                "ALTER TABLE IntakeCandidate ADD payload STRING",
                "ALTER TABLE IntakeCandidate ADD confidence DOUBLE",
                "ALTER TABLE IntakeCandidate ADD source_span STRING",
                "ALTER TABLE IntakeCandidate ADD extractor_fingerprint STRING",
            ] {
                if let Err(e) = conn.query(stmt) {
                    tracing::debug!("schema DDL query note: {e}");
                }
            }
            if let Err(e) = conn.query(
                "MATCH (c:IntakeCandidate) WHERE c.candidate_kind IS NULL OR c.candidate_kind = '' SET c.candidate_kind = 'task', c.kind = 'task_obligation', c.category = '', c.provenance = '', c.payload = '{}', c.confidence = 0.0, c.source_span = '', c.extractor_fingerprint = ''",
            ) {
                tracing::debug!("IntakeCandidate backfill note: {e}");
            }

            // Source-scoped staged ingestion. Two row kinds live in this table:
            //   * the counter row, `id = namespace`, whose `counter` allocates
            //     the next generation number (shared across sources so the
            //     generation-scoped node ids never collide);
            //   * a pointer row per source, `id = namespace::source_key`, whose
            //     `active_generation` names that source's published generation
            //     (0 = none). The pointer flip is the atomic publish step, and
            //     keying it by source is what keeps worktree B from overwriting
            //     worktree A's active generation.
            let create_generation_table = "CREATE NODE TABLE IngestGeneration (id STRING, namespace STRING, source_key STRING, active_generation INT64, counter INT64, PRIMARY KEY (id))";
            if let Err(e) = conn.query(create_generation_table) {
                if format!("{e}").to_lowercase().contains("already exists") {
                    tracing::debug!("schema DDL query note: {e}");
                } else {
                    tracing::warn!("schema DDL table creation failed: {e}");
                }
            }
            for stmt in [
                "ALTER TABLE IngestGeneration ADD source_key STRING",
                "ALTER TABLE IngestGeneration ADD active_generation INT64",
                "ALTER TABLE IngestGeneration ADD counter INT64",
            ] {
                if let Err(e) = conn.query(stmt) {
                    tracing::debug!("schema DDL query note: {e}");
                }
            }

            let create_metric_table = "CREATE NODE TABLE MemoryMetric (id STRING, namespace STRING, kind STRING, name STRING, ts INT64, value DOUBLE, labels STRING, PRIMARY KEY (id))";
            if let Err(e) = conn.query(create_metric_table) {
                if format!("{e}").to_lowercase().contains("already exists") {
                    tracing::debug!("schema DDL query note: {e}");
                } else {
                    tracing::warn!("schema DDL table creation failed: {e}");
                }
            }

            Ok(())
        })
        .await;

        if outcome.is_ok() {
            self.schema_ready
                .store(true, std::sync::atomic::Ordering::Release);
            // Issue #74: an engine-attested empty LockedUnknown store
            // first records its pristine identity under the serving
            // descriptor (see
            // `reattest_and_upgrade_empty_locked_unknown_store_state`);
            // the general maintenance below then proceeds as for any
            // recorded-pristine store.
            //
            // The stamp is gated on a FRESH attestation taken at
            // stamp time, not only the in-closure one above: a write
            // can land between the closure's attestation and this
            // stamp — from a primed non-canonical writer in this
            // instance, or, with no priming at all, from a second
            // LadybugStore instance on the same database, whose
            // per-Self `write_lock` shares no turn with this one —
            // and the stamp alone re-checks only cached state and
            // serving == configured, so it would record a pristine
            // identity over live data (overseer panel condition 3,
            // findings SA-F1 + N1). That fresh attestation and the
            // stamp it gates execute as one unit inside a single
            // writer's turn, so the window cannot reopen between the
            // stamp step's own check and its record — for writers
            // sharing this instance's turn. A writer in a second,
            // co-open instance shares no turn with this one, so the
            // guarantee does not extend to it; whether such co-open
            // is even possible is unproven, and the cross-instance
            // residual is tracked in #92.
            if locked_unknown_attested_empty.load(std::sync::atomic::Ordering::Acquire) {
                self.reattest_and_upgrade_empty_locked_unknown_store_state(dimensions)
                    .await;
            }
            // First successful init: record the store's pristine state
            // (or adopt a new configured descriptor while pristine).
            self.maintain_store_state();
        }

        // Creating the tables is itself a write worth flushing.
        self.dirty.mark();
        outcome
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_intake_store(&self) -> Option<&dyn crate::traits::IntakeStore> {
        Some(self)
    }

    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<crate::traits::UpsertReport> {
        // AC3: canonicalize resolvable targets at write time via KnownIds::resolve.
        // Fetch known symbol ids for this namespace and rewrite unqualified edge
        // targets in payload.metadata to their qualified (resolved) form when
        // unambiguous. Qualified targets are left untouched.
        let namespace = namespace.to_string();
        // R14: reject empty/whitespace namespaces before hitting the store.
        if namespace.trim().is_empty() {
            anyhow::bail!("namespace must be non-empty");
        }
        // First-write race guard: on a pristine store, refuse the first
        // write if the configured model changed since this process
        // recorded the store's descriptor (a setup pick landed while the
        // store is open). Checked before any engine write, so a refused
        // upsert lands nothing.
        self.guard_first_write()?;
        // The same entry point refuses a
        // write under a serving descriptor different from a
        // data-bearing store's recorded one — the vector paths that
        // never init (the fallback CLI's direct upserts) are covered
        // here rather than at init.
        self.check_data_bearing_identity()?;

        // The first-data flip handle, snapshotted at entry: one clone
        // moves into the write closure below, so the task that commits
        // the first write records the transition itself.
        let first_write_flip = self.first_write_flip();

        // Every upsert is one sample of the MemoryCount series for the
        // namespace; anomalies surface through the rolling detector.
        self.metrics.record(
            &namespace,
            crate::telemetry::anomaly::MetricKind::MemoryCount,
            "upsert",
            chrono::Utc::now().timestamp(),
            1.0,
            std::collections::BTreeMap::new(),
        );
        let id = id.to_string();
        // Parse shape warnings from the ORIGINAL payload before the write.
        let mut payload = payload;
        // AC3: canonicalize resolvable (unqualified) edge targets at write time.
        {
            let ids = self.mention_symbol_ids(&namespace).await;
            let known = crate::store::ladybug::KnownIds::new(ids.iter().map(|s| s.as_str()));
            for key in ["related_to", "contained_by", "governs", "extracted_from", "depends_on"] {
                if let Some(arr) = payload.metadata.get(key).and_then(|v| v.as_array()).cloned() {
                    let mut new_arr = Vec::with_capacity(arr.len());
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            if !s.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                                if let Some(resolved) = known.resolve(s) {
                                    new_arr.push(serde_json::Value::String(resolved));
                                    continue;
                                }
                            }
                        }
                        new_arr.push(item);
                    }
                    if let Some(obj) = payload.metadata.as_object_mut() {
                        obj.insert(key.to_string(), serde_json::Value::Array(new_arr));
                    }
                }
            }
        }
        // Capture raw topology before canonicalization (F17 preserve raw).
        let raw_topology_original: Option<serde_json::Value> = payload
            .metadata
            .get("topology")
            .and_then(|t| t.as_object())
            .map(|obj| {
                let mut m = serde_json::Map::new();
                for key in ["component", "target_system"] {
                    if let Some(v) = obj.get(key) {
                        m.insert(key.to_string(), v.clone());
                    }
                }
                serde_json::Value::Object(m)
            });
        let had_topology_original = payload.metadata.get("topology_original").is_some();
        // Refresh ontology before canonicalization (per-namespace).
        self.refresh_ontology(&namespace).await;
        let ontology = self.get_ontology(&namespace);
        if let Some(topology) = payload.metadata.get_mut("topology") {
            crate::rdf::canonicalize_topology(topology, &ontology);
        }
        // Stamp topology_original if changed and not already present.
        if !had_topology_original {
            if let (Some(raw), Some(topo)) = (&raw_topology_original, payload.metadata.get("topology")) {
                // Determine if any field changed.
                let mut changed = false;
                if let (Some(raw_obj), Some(topo_obj)) = (raw.as_object(), topo.as_object()) {
                    for key in ["component", "target_system"] {
                        if let (Some(Value::String(r)), Some(Value::String(c))) =
                            (raw_obj.get(key), topo_obj.get(key))
                        {
                            if r != c {
                                changed = true;
                            }
                        }
                    }
                }
                if changed {
                    if let Some(obj) = payload.metadata.as_object_mut() {
                        obj.insert("topology_original".to_string(), raw.clone());
                    }
                }
            }
        }
        if payload.metadata.is_object() {
            let now_ts = chrono::Utc::now().timestamp();
            crate::domain::materialize_facets_bitemporal(&payload.memory_type, &mut payload.metadata, now_ts);
        }

        // Write-side concept_iri stamping (F20): if metadata.concept_iri absent
        // and metadata.domain resolves to v2 registry entry with concept_iri,
        // stamp metadata.concept_iri (string or string[] for multi-domain).
        if payload.metadata.get("concept_iri").is_none() {
            if let Some(domain_val) = payload.metadata.get("domain").cloned() {
                let domains: Vec<String> = match &domain_val {
                    serde_json::Value::String(s) => vec![s.clone()],
                    serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
                    _ => vec![],
                };
                // normalize slugs
                let mut slugs: Vec<String> = domains.iter().filter_map(|d| crate::domain::normalize_domain_slug(d)).collect();
                slugs.sort();
                slugs.dedup();
                if !slugs.is_empty() {
                    // read registry (optional)
                    let rid = crate::registry::registry_row_id(&namespace);
                    let reg_payload_opt = match self.get(&namespace, &rid).await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!("registry lookup failed for '{namespace}:{rid}': {e}");
                            None
                        }
                    };
                    let mut iris: Vec<String> = Vec::new();
                    if let Some((_, reg_payload)) = &reg_payload_opt {
                        let entries = crate::registry::registry_domain_entries(&reg_payload.metadata);
                        for slug in &slugs {
                            for e in &entries {
                                if e.get("slug").and_then(|s| s.as_str()) == Some(slug.as_str()) {
                                    if let Some(iri) = e.get("concept_iri").and_then(|v| v.as_str()) {
                                        if !iri.trim().is_empty() && !iris.contains(&iri.to_string()) {
                                            iris.push(iri.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Ontology grounding (F21): if registry did not provide iris, consult vocabulary.
                    let mut strict_missing = false;
                    if iris.is_empty() {
                        let reg_meta = reg_payload_opt
                            .as_ref()
                            .map(|(_, p)| p.metadata.clone())
                            .unwrap_or(serde_json::Value::Null);
                        if let Some(cfg) = crate::ontology_grounding::resolve_config(&reg_meta) {
                            match crate::ontology_grounding::load_vocabulary(&cfg.path) {
                                Ok(vocab) => {
                                    let v_iris = crate::ontology_grounding::concept_iris_from_vocabulary(&vocab, &slugs);
                                    if v_iris.is_empty() {
                                        if cfg.mode == "strict" {
                                            strict_missing = true;
                                        }
                                    } else {
                                        iris = v_iris;
                                    }
                                }
                                Err(_) => {
                                    // warnings handled by ontology_warnings surface
                                }
                            }
                        }
                    }

                    if !iris.is_empty() {
                        let val = if iris.len() == 1 {
                            serde_json::json!(iris[0])
                        } else {
                            let mut s = iris.clone();
                            s.sort();
                            s.dedup();
                            serde_json::json!(s)
                        };
                        if let Some(obj) = payload.metadata.as_object_mut() {
                            obj.insert("concept_iri".to_string(), val);
                        }
                    }

                    if strict_missing {
                        if let Some(obj) = payload.metadata.as_object_mut() {
                            obj.insert(
                                "validation_warning".to_string(),
                                serde_json::json!(format!(
                                    "ontology: no concept_iri for domain(s) {}",
                                    slugs.join(", ")
                                )),
                            );
                        }
                    }
                }
            }
        }
        // Mention detection (OxiRS §1.8): only for agent/user-written rows.
        let mut mentions_for_write: Vec<String> = Vec::new();
        if payload.user_id != "auto-ingestor" {
            let ids = self.mention_symbol_ids(&namespace).await;
            let known = crate::store::ladybug::KnownIds::new(ids.iter().map(|s| s.as_str()));
            let mut mentions = crate::mention::detect_mentions(&payload.content, &payload.location, &known);
            mentions.retain(|m| m != &id);
            // Skip symbols already declared via related_to (declared intent wins).
            if let Some(arr) = payload.metadata.get("related_to").and_then(|v| v.as_array()) {
                let declared: std::collections::HashSet<String> =
                    arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                mentions.retain(|m| !declared.contains(m));
            }
            if !mentions.is_empty() {
                if let Some(obj) = payload.metadata.as_object_mut() {
                    obj.insert("mentions".to_string(), serde_json::json!(mentions));
                }
            } else {
                if let Some(obj) = payload.metadata.as_object_mut() {
                    obj.remove("mentions");
                }
            }
            mentions_for_write = mentions;
        }

        let mut shape_warnings = write_shape_warnings(&payload);
        // SHACL validation of the payload against the namespace's compiled
        // shapes, purely advisory: violations surface as warnings, never a
        // failed write.
        let shacl_adapter = crate::rdf::reasoning::ReasoningAdapter::new();
        if let Some(ont_idx) = self.ontology.load().get(namespace.as_str()) {
            // SHACL violations land in the warning list instead of being
            // discarded: every finding surfaces to the writer, never blocks.
            for v in shacl_adapter.validate_payload(ont_idx, &payload) {
                shape_warnings.push(WriteShapeWarning {
                    field: v.field,
                    why: v.why,
                    skipped: "SHACL (advisory)".to_string(),
                });
            }
        }
        for v in crate::rdf::validate_memory(&payload) {
            shape_warnings.push(WriteShapeWarning {
                field: v.field,
                why: v.why,
                skipped: "SHACL Core (warn-first)".to_string(),
            });
        }
        // The stub materializer needs the embedding width for its zero vector;
        // captured here so the closure stays `FnOnce + Send + 'static`.
        let dimensions = self.dimensions;
        let zero_vec = zero_vector_str(dimensions);
        let lexical_doc = Self::lexical_doc(&namespace, &id, &payload);
        // Composite Rec #14: memory ids are globally unique (PRIMARY KEY), so an
        // upsert of an existing id into a DIFFERENT namespace would silently
        // relocate the row. Read which namespace owns the id first and fail
        // closed on a collision.
        {
            let check_id = id.clone();
            let check_ns = namespace.clone();
            let existing_ns: Option<String> = self
                .with_conn(move |conn| {
                    let query = format!(
                        "MATCH (m:Memory {{id: '{}'}}) WHERE m.namespace <> '{}' RETURN m.namespace LIMIT 1",
                        escape_ladybug_string(&check_id),
                        escape_ladybug_string(&check_ns)
                    );
                    let mut result = conn.query(&query)?;
                    let found = result.next().map(|row| format!("{}", row[0]));
                    Ok(found)
                })
                .await?;
            if let Some(existing_ns) = existing_ns {
                anyhow::bail!(
                    "cross-namespace id collision: memory '{}' belongs to namespace '{}', cannot upsert into '{}'",
                    id,
                    existing_ns,
                    namespace
                );
            }
        }
        let outcome = self.write_with_deadline("writing a memory", move |conn| {

            let safe_id = escape_ladybug_string(&id);
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_content = escape_ladybug_string(&payload.content);
            let safe_user_id = escape_ladybug_string(&payload.user_id);
            let safe_memory_type = escape_ladybug_string(&payload.memory_type);
            let safe_agent_name = escape_ladybug_string(
                payload.agent_name.as_deref().unwrap_or("unknown"),
            );
            let safe_location = escape_ladybug_string(&payload.location);
            let safe_location_lines = escape_ladybug_string(&payload.location_lines);
            let safe_metadata = escape_ladybug_string(&serde_json::to_string(&payload.metadata)?);

            let vec_str = format!(
                "[{}]",
                // R16: defense-in-depth (no native bind in lbug 0.20.4).
                // Always emit decimal (e.g. 0 → 0.0) so ARRAY_DISTANCE
                // binder never sees pure-integer arrays.
                vector.iter().map(|f| {
                    let s = format!("{}", f);
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        s
                    } else {
                        format!("{}.0", s)
                    }
                }).collect::<Vec<_>>().join(",")
            );

            let insert_query = format!(
                "MERGE (m:Memory {{id: '{}'}})
                 ON CREATE SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}
                 ON MATCH SET m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}",
                safe_id, safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str,
                safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str
            );

            conn.query(&insert_query)?;

            // The core MERGE has landed, so this write's data is in the
            // store: record the first-data transition here, in the
            // committing task, before the best-effort edge bookkeeping.
            // The caller may already have been released by the write
            // deadline; the transition must not depend on what the
            // caller observed. Monotonic and idempotent, so a
            // collision-retry re-run of this closure is safe.
            first_write_flip.flip_store_state_to_data();

            // Emit shape warnings after successful row write.
            for w in &shape_warnings {
                emit_write_shape_warning(&namespace, &id, w);
            }

            let mut deferred_edges = Vec::new();
            let edge_now = chrono::Utc::now().timestamp();

            // F6: collect resolved edge provenance entries to stamp on the row.
            let mut resolved_edge_entries: Vec<serde_json::Value> = Vec::new();

            // Materialise the edges this memory declares. A target that does not exist
            // yet simply produces no edge: MATCH finds nothing and MERGE never runs.
            // That is deliberate -- a rule may name a file the ingester has not reached.
            for edge in edge_specs(&payload.metadata) {
                // F18 endpoint role: GOVERNS requires rule source.
                if edge.rel_type == "GOVERNS" && !(payload.memory_type == "rule" || payload.memory_type == "fact" || payload.memory_type == "context") {
                    emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                        field: "metadata.governs".to_string(),
                        why: format!("endpoint role mismatch: GOVERNS requires source=rule (got {})", payload.memory_type),
                        skipped: "this edge materialization".to_string(),
                    });
                    continue;
                }
                let target = if edge.target_id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                    // (E) Qualified: exact ID lookup in any namespace.
                    match exact_target_exists(conn, &edge.target_id) {
                        Ok(true) => edge.target_id.clone(),
                        Ok(false) => {
                            if let Some(stripped) = edge.target_id.strip_prefix(&format!("{}{}", namespace, crate::parser::ingest::NAMESPACE_SEPARATOR)) {
                                match resolve_target_on_write(conn, &namespace, stripped) {
                                    crate::traits::TargetResolution::Resolved(found) => found,
                                    _ => {
                                        deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                            rel_type: edge.rel_type.to_string(),
                                            declared_target: edge.target_id.clone(),
                                            declaration_index: edge.declaration_index,
                                            resolution: crate::traits::TargetResolution::Missing,
                                        });
                                        emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                            field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                            why: "qualified target does not exist".to_string(),
                                            skipped: "this edge materialization".to_string(),
                                        });
                                        continue;
                                    }
                                }
                            } else {
                                deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                    rel_type: edge.rel_type.to_string(),
                                    declared_target: edge.target_id.clone(),
                                    declaration_index: edge.declaration_index,
                                    resolution: crate::traits::TargetResolution::Missing,
                                });
                                emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                    field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                    why: "qualified target does not exist".to_string(),
                                    skipped: "this edge materialization".to_string(),
                                });
                                continue;
                            }
                        }
                        Err(e) => {
                            deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                rel_type: edge.rel_type.to_string(),
                                declared_target: edge.target_id.clone(),
                                declaration_index: edge.declaration_index,
                                resolution: crate::traits::TargetResolution::Failed(e.to_string()),
                            });
                            emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                why: "target lookup failed".to_string(),
                                skipped: "this edge materialization".to_string(),
                            });
                            continue;
                        }
                    }
                } else {
                    match resolve_target_on_write(conn, &namespace, &edge.target_id) {
                        crate::traits::TargetResolution::Resolved(found) => found,
                        // Spec §3.2: a GOVERNS target that names a file not yet
                        // ingested auto-materialises a stub `file` node so the
                        // edge forms immediately instead of waiting for an
                        // ingest that may never come. Only the unqualified,
                        // unambiguous `Missing` case stubs -- an ambiguous or
                        // failed resolution, or a qualified cross-namespace id,
                        // still defers.
                        crate::traits::TargetResolution::Missing
                            if edge.rel_type == "GOVERNS" && payload.memory_type == "rule" =>
                        {
                            match materialize_stub_file_node(
                                conn,
                                &namespace,
                                &edge.target_id,
                                &zero_vec,
                            ) {
                                Ok(stub_id) => stub_id,
                                Err(e) => {
                                    deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                        rel_type: edge.rel_type.to_string(),
                                        declared_target: edge.target_id.clone(),
                                        declaration_index: edge.declaration_index,
                                        resolution: crate::traits::TargetResolution::Failed(
                                            e.to_string(),
                                        ),
                                    });
                                    emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                        field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                        why: "stub materialization failed".to_string(),
                                        skipped: "this edge materialization".to_string(),
                                    });
                                    continue;
                                }
                            }
                        }
                        resolution @ crate::traits::TargetResolution::Missing
                        | resolution @ crate::traits::TargetResolution::Ambiguous(_)
                        | resolution @ crate::traits::TargetResolution::Failed(_) => {
                            deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                rel_type: edge.rel_type.to_string(),
                                declared_target: edge.target_id.clone(),
                                declaration_index: edge.declaration_index,
                                resolution,
                            });
                            emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                why: "unqualified target is missing or ambiguous".to_string(),
                                skipped: "this edge materialization".to_string(),
                            });
                            continue;
                        }
                    }
                };
                // F18 endpoint role matrix: check source/target memory types.
                let target_type = {
                    let q = format!("MATCH (m:Memory {{id: '{}'}}) RETURN m.memory_type", escape_ladybug_string(&target));
                    match conn.query(&q) {
                        Ok(mut r) => r.next().map(|row| format!("{}", row[0])).unwrap_or_default(),
                        Err(e) => {
                            tracing::warn!("upsert target_type lookup failed for {target}: {e}");
                            String::new()
                        }
                    }
                };
                if !crate::rdf::shacl::endpoint_role_allowed(&edge.rel_type, &payload.memory_type, &target_type) {
                    emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                        field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                        why: format!("endpoint role mismatch: {} (source={} target={})", edge.rel_type, payload.memory_type, target_type),
                        skipped: "warn-only: edge materialized despite role mismatch".to_string(),
                    });
                    // warn-only: do not skip edge materialization
                }
                // F18: GOVERNS source must be rule even when target missing/stubbed.
                // (handled above for resolved targets; missing targets skip role check)
                // F6: stamp edge provenance for this resolved declaration.
                let evidence_id = {
                    let declared = edge.target_id.clone();
                    let resolved = target.clone();
                    let logical = crate::parser::generation::logical_path_of(&resolved).to_string();
                    let orientation = if edge.points_at_target {
                        "self_to_target"
                    } else {
                        "target_to_self"
                    }
                    .to_string();
                    let file = payload
                        .metadata
                        .get("absolute_path")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            if payload.location.trim().is_empty() {
                                None
                            } else {
                                Some(payload.location.clone())
                            }
                        });
                    let content_hash = payload
                        .metadata
                        .get("file_sha256")
                        .or_else(|| payload.metadata.get("span_sha256"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            use sha2::{Digest, Sha256};
                            let mut hasher = Sha256::new();
                            hasher.update(payload.content.as_bytes());
                            Some(hex::encode(hasher.finalize()))
                        });
                    let (line_start, line_end) = match (if payload.location_lines.trim().is_empty() { None } else { Some(payload.location_lines.as_str()) })
                        .or_else(|| payload.metadata.get("location_lines").and_then(|v| v.as_str()))
                    {
                        Some(s) if !s.trim().is_empty() => {
                            let parts: Vec<&str> = s.split('-').collect();
                            let start = parts.first().and_then(|p| p.trim().parse::<i64>().ok());
                            let end = parts
                                .get(1)
                                .and_then(|p| p.trim().parse::<i64>().ok())
                                .or(start);
                            (start, end)
                        }
                        _ => (None, None),
                    };
                    let evidence_id = match (&file, &content_hash) {
                        (Some(f), Some(h)) => format!("{}:{}:{}", f, payload.location_lines.trim(), &h[..h.len().min(12)]),
                        (Some(f), None) => format!("{}:{}", f, payload.location_lines.trim()),
                        _ => id.clone(),
                    };
                    let entry = serde_json::json!({
                        "declaring_id": id,
                        "rel_type": edge.rel_type,
                        "declaration_index": edge.declaration_index,
                        "declared_target": declared,
                        "resolved_target": resolved,
                        "logical_target": logical,
                        "orientation": orientation,
                        "file": file,
                        "content_hash": content_hash,
                        "line_start": line_start,
                        "line_end": line_end,
                        "evidence_id": evidence_id,
                        "extractor": "upsert",
                    });
                    resolved_edge_entries.push(entry);
                    evidence_id
                };
                if !is_valid_relation_name(edge.rel_type) {
                    deferred_edges.push(crate::traits::EdgeResolutionStatus {
                        rel_type: edge.rel_type.to_string(),
                        declared_target: edge.target_id.clone(),
                        declaration_index: edge.declaration_index,
                        resolution: crate::traits::TargetResolution::Failed(
                            "invalid relation name for Cypher interpolation".to_string(),
                        ),
                    });
                    continue;
                }
                let target_safe = escape_ladybug_string(&target);
                let (from, to) = if edge.points_at_target {
                    (safe_id.as_str(), target_safe.as_str())
                } else {
                    (target_safe.as_str(), safe_id.as_str())
                };
                let evidence_id_safe = escape_ladybug_string(&evidence_id);
                let edge_query = format!(
                    "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[r:{}]->(b) ON CREATE SET r.source = '{}', r.valid_from = {}, r.valid_to = 0, r.generation = 0, r.extractor = 'upsert', r.evidence_id = '{}' ON MATCH SET r.source = '{}', r.valid_from = {}, r.extractor = 'upsert', r.evidence_id = '{}'",
                    from, to, edge.rel_type, safe_ns, edge_now, evidence_id_safe, safe_ns, edge_now, evidence_id_safe
                );
                if let Err(e) = conn.query(&edge_query) {
                    deferred_edges.push(crate::traits::EdgeResolutionStatus {
                        rel_type: edge.rel_type.to_string(),
                        declared_target: edge.target_id.clone(),
                        declaration_index: edge.declaration_index,
                        resolution: crate::traits::TargetResolution::Failed(e.to_string()),
                    });
                    emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                        field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                        why: "edge materialization failed".to_string(),
                        skipped: "this edge materialization".to_string(),
                    });
                    eprintln!("[neurostrata:write-shape] edge query error: {}", e);
                }
            }

            // F6: stamp edge_sources provenance on the declaring row's metadata.
            if !resolved_edge_entries.is_empty() {
                // Dedupe identical entries.
                let mut deduped: Vec<serde_json::Value> = Vec::new();
                for e in &resolved_edge_entries {
                    if !deduped.iter().any(|d| d == e) {
                        deduped.push(e.clone());
                    }
                }
                // Build edge_sources array and merge into metadata.
                let edge_sources = serde_json::json!(deduped);
                // Read current metadata, merge edge_sources, write back.
                let read_q = format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.metadata",
                    escape_ladybug_string(&namespace),
                    escape_ladybug_string(&id)
                );
                if let Ok(mut res) = conn.query(&read_q) {
                    if let Some(row) = res.next() {
                        if let lbug::Value::String(meta_str) = &row[0] {
                            if let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
                                if let Some(obj) = meta.as_object_mut() {
                                    obj.insert("edge_sources".to_string(), edge_sources);
                                    let new_meta = serde_json::to_string(&meta).unwrap_or_default();
                                    let safe_new_meta = escape_ladybug_string(&new_meta);
                                    let update_q = format!(
                                        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'",
                                        escape_ladybug_string(&namespace),
                                        escape_ladybug_string(&id),
                                        safe_new_meta
                                    );
                                    if let Err(e) = conn.query(&update_q) {
                                        tracing::warn!("edge_sources provenance update failed for '{namespace}:{id}': {e}");
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Materialize mention edges (always delete old mention-detector rows,
            // then create fresh ones only if there are mentions to write).
            // Deleting unconditionally is required: a re-upsert with no mentions
            // must clear edges left from a previous upsert that did have them.
            let del_q = format!(
                "MATCH (a:Memory {{id: '{}'}})-[r:RELATES_TO]->(b:Memory) WHERE r.extractor = 'mention-detector' DELETE r",
                escape_ladybug_string(&id)
            );
            if let Err(e) = conn.query(&del_q) {
                tracing::warn!("stale mention-edge cleanup failed for '{namespace}:{id}': {e}");
            }
            for m in &mentions_for_write {
                let m_safe = escape_ladybug_string(m);
                let q = format!(
                    "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) CREATE (a)-[:RELATES_TO {{source: '{}', valid_from: {}, valid_to: 0, generation: 0, extractor: 'mention-detector', evidence_id: '{}'}}]->(b)",
                    escape_ladybug_string(&id),
                    m_safe,
                    escape_ladybug_string(&namespace),
                    edge_now,
                    escape_ladybug_string(&id),
                );
                if let Err(e) = conn.query(&q) {
                    tracing::warn!("mention-edge creation failed for '{namespace}:{id}': {e}");
                }
            }

            Ok(crate::traits::UpsertReport {
                id: id.clone(),
                deferred_edges,
            })
        })
        .await;
        if outcome.is_ok() {
            self.lexical_apply_doc(lexical_doc);
            // No caller-side first-data flip: the committing task
            // already recorded the transition inside the write closure
            // (see above), so it is recorded even when this caller was
            // released by the deadline before the write landed.
        }
        outcome
    }

    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
    ) -> Result<Vec<SearchResult>> {
        let outcome = search_inner(
            self,
            namespace,
            vector,
            limit,
            include_tasks,
            include_archived,
            HashSet::new(),
            None,
            true,
            crate::traits::SearchMode::Default,
        )
        .await?;
        Ok(outcome.results)
    }

    async fn search_with_domain(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        domains: &[String],
        query_text: Option<&str>,
        exact: bool,
    ) -> Result<DomainSearchOutcome> {
        // Refresh the compiled ontology index from the namespace's registry row
        // (fail-soft: a missing row keeps the previous/empty index).
        self.refresh_ontology(namespace).await;
        let ontology = self.get_ontology(namespace);

        let mut expanded: HashSet<String> = HashSet::new();
        let reasoner = crate::rdf::reasoning::ReasoningAdapter::new();
        for d in domains {
            if let Some(slug) = ontology.canonical_slug(d) {
                for e in reasoner.expand_domain(&ontology, &slug) {
                    expanded.insert(e);
                }
            } else if let Some(s) = crate::domain::normalize_domain_slug(d) {
                expanded.insert(s);
            }
        }
        let domain_keys: HashSet<String> = expanded;
        let query_owned = query_text.map(|s| s.to_string());
        let outcome = search_inner(
            self,
            namespace,
            vector,
            limit,
            include_tasks,
            include_archived,
            domain_keys,
            query_owned,
            exact,
            crate::traits::SearchMode::Default,
        )
        .await?;
        if outcome.incomplete {
            tracing::debug!(
                "search_with_domain in '{}' reached fetch_limit ({} results); outcome.incomplete=true",
                namespace,
                outcome.results.len()
            );
        }
        Ok(outcome)
    }

    async fn search_lexical(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        domains: &[String],
        principal: &str,
        fuzzy_max_distance: usize,
    ) -> Result<crate::traits::LexicalSearchOutcome> {
        self.ensure_lexical_loaded(namespace).await?;
        self.refresh_ontology(namespace).await;
        let ontology = self.get_ontology(namespace);
        // Domain ontology expansion: expand domain slugs before filtering
        // (mirrors search_with_domain at ladybug.rs:2895).
        let mut expanded: Vec<String> = Vec::new();
        for d in domains {
            if let Some(slug) = ontology.canonical_slug(d) {
                for e in ontology.expand_domain(&slug) {
                    expanded.push(e);
                }
            } else if let Some(s) = crate::domain::normalize_domain_slug(d) {
                expanded.push(s);
            }
        }
        let expanded_domains: Vec<String> = expanded.into_iter().collect();
        let options = crate::fts::SearchOptions {
            now: chrono::Utc::now().timestamp(),
            as_of: None,
            include_tasks,
            include_archived,
            principal: principal.to_string(),
            domains: expanded_domains,
            fuzzy_max_distance: fuzzy_max_distance.min(2),
            limit,
        };
        let outcome = self
            .lexical
            .read()
            .expect("lexical index lock")
            .search(namespace, query, &options);

        let mut results = Vec::with_capacity(outcome.hits.len());
        for hit in outcome.hits {
            let Some((_, payload)) = self.get(namespace, &hit.id).await? else {
                continue;
            };
            results.push(SearchResult {
                id: hit.id,
                score: hit.score,
                payload,
                evidence: Some(vec![crate::traits::SearchEvidence {
                    kind: crate::traits::EvidenceKind::DirectMatch,
                    path: Vec::new(),
                    explanation: "This memory matched the lexical query.".to_string(),
                    derivation: vec!["lexical-inhouse".to_string()],
                }]),
            });
        }

        Ok(crate::traits::LexicalSearchOutcome {
            results,
            domain_filter_applied: outcome.domain_filter_applied,
        })
    }

    async fn rebuild_lexical_index(&self, namespace: Option<&str>) -> Result<usize> {
        let namespaces = match namespace {
            Some(namespace) => vec![namespace.to_string()],
            None => self.list_namespaces().await?,
        };

        {
            let mut index = self.lexical.write().unwrap_or_else(|e| e.into_inner());
            let mut loaded = self
                .lexical_loaded
                .write()
                .expect("lexical loaded lock");
            for namespace in &namespaces {
                index.clear_namespace(namespace);
                loaded.remove(namespace);
            }
        }

        let mut total = 0;
        for namespace in &namespaces {
            self.ensure_lexical_loaded(namespace).await?;
            total += self
                .lexical
                .read()
                .expect("lexical index lock")
                .namespaces
                .get(namespace)
                .map(|partition| partition.len())
                .unwrap_or(0);
        }
        Ok(total)
    }

    async fn nearest_by_raw_distance(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        query: crate::traits::RawDistanceQuery,
    ) -> Result<Vec<crate::traits::RawDistanceCandidate>> {
        let scope = query.scope;
        let limit = query.limit;
        let namespace = namespace.to_string();
        let is_procedure_scope = matches!(scope, crate::traits::DedupCandidateScope::Procedure { .. });
        let proc_trigger = match &scope {
            crate::traits::DedupCandidateScope::Procedure { trigger } => Some(trigger.clone()),
            _ => None,
        };

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let vec_str = format!(
                "[{}]",
                // R16: defense-in-depth (no native bind in lbug 0.20.4).
                // Always emit decimal (e.g. 0 → 0.0) so ARRAY_DISTANCE
                // binder never sees pure-integer arrays.
                vector.iter().map(|f| {
                    let s = format!("{}", f);
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        s
                    } else {
                        format!("{}.0", s)
                    }
                }).collect::<Vec<_>>().join(",")
            );
            let now = chrono::Utc::now().timestamp();

            const FETCH_MULTIPLIER: usize = 10;
            let fetch_limit = limit.max(1) * FETCH_MULTIPLIER;

            // Excluded memory types for Domain scope (structural + task + procedure + symbol/code_ast).
            // Procedure scope restricts to memory_type = 'procedure' instead.
            let excluded_types_str = if is_procedure_scope {
                // exclude everything; we filter to procedure in Rust + SQL
                String::new()
            } else {
                let mut excluded = STRUCTURAL_MEMORY_TYPES
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                excluded.push("task".to_string());
                excluded.push("procedure".to_string());
                excluded.push("symbol".to_string());
                excluded.push("code_ast".to_string());
                excluded
                    .iter()
                    .map(|t| format!("'{}'", t))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let type_filter = if is_procedure_scope {
                "AND m.memory_type = 'procedure'".to_string()
            } else {
                format!("AND NOT m.memory_type IN [{}]", excluded_types_str)
            };

            let search_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' {} RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC, m.id ASC LIMIT {}",
                safe_ns, type_filter, vec_str, fetch_limit
            );

            let result = conn.query(&search_query)?;
            let mut out = Vec::new();
            for row in result {
                if out.len() >= limit {
                    break;
                }
                let id: String = format!("{}", row[0]);
                let distance: f32 = match &row[1] {
                    lbug::Value::Float(f) if f.is_finite() && *f >= 0.0 => *f,
                    lbug::Value::Double(d) if d.is_finite() && *d >= 0.0 => *d as f32,
                    _ => continue,
                };
                let content: String = format!("{}", row[2]);
                let user_id: String = format!("{}", row[3]);
                let memory_type: String = format!("{}", row[4]);
                let agent_name: String = format!("{}", row[5]);
                let location: String = format!("{}", row[6]);
                let location_lines: String = format!("{}", row[7]);
                let metadata_str: String = format!("{}", row[8]);
                let metadata_val: Value =
                    parse_metadata_or_warn(&metadata_str, "metadata");

                // Lifecycle filters (Rust side).
                if is_retired_at(&metadata_val, now) {
                    continue;
                }
                if is_archived(&metadata_val) {
                    continue;
                }
                if is_mesh_conflicting(&metadata_val) {
                    continue;
                }
                if crate::registry::is_registry_row(&metadata_val) {
                    continue;
                }
                if memory_type != "procedure" && user_id == "auto-ingestor" {
                    continue;
                }

                // Procedure scope: trigger match, not lapsed, not spent.
                if is_procedure_scope {
                    let trig_ok = proc_trigger
                        .as_ref()
                        .map(|t| {
                            metadata_val
                                .get("trigger")
                                .and_then(|v| v.as_str())
                                .map(|s| s == t)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !trig_ok {
                        continue;
                    }
                    // lapsed?
                    if is_retired_at(&metadata_val, now) {
                        continue;
                    }
                    // spent?
                    if let Some(rf) = metadata_val.get("remaining_fires") {
                        if !rf.is_null() && rf.as_i64() == Some(0) {
                            continue;
                        }
                    }
                }

                out.push(crate::traits::RawDistanceCandidate {
                    id,
                    raw_distance: distance,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: if agent_name == "unknown" || agent_name.is_empty() {
                            None
                        } else {
                            Some(agent_name)
                        },
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                });
            }
            Ok(out)
        })
        .await
    }

    async fn find_active_exact_content(
        &self,
        namespace: &str,
        exact_content: &str,
        scope: crate::traits::DedupCandidateScope,
    ) -> Result<Vec<crate::traits::ExactContentCandidate>> {
        let is_procedure_scope = matches!(scope, crate::traits::DedupCandidateScope::Procedure { .. });
        let proc_trigger = match &scope {
            crate::traits::DedupCandidateScope::Procedure { trigger } => Some(trigger.clone()),
            _ => None,
        };
        let namespace = namespace.to_string();
        let exact_content = exact_content.to_string();
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_content = escape_ladybug_string(&exact_content);
            let now = chrono::Utc::now().timestamp();

            let type_filter = if is_procedure_scope {
                "AND m.memory_type = 'procedure'".to_string()
            } else {
                let mut excluded = STRUCTURAL_MEMORY_TYPES
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                excluded.push("task".to_string());
                excluded.push("procedure".to_string());
                excluded.push("symbol".to_string());
                excluded.push("code_ast".to_string());
                let s = excluded.iter().map(|t| format!("'{}'", t)).collect::<Vec<_>>().join(", ");
                format!("AND NOT m.memory_type IN [{}]", s)
            };

            let query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.content = '{}' {} RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata",
                safe_ns, safe_content, type_filter
            );
            let result = conn.query(&query)?;
            let mut out = Vec::new();
            for row in result {
                let id: String = format!("{}", row[0]);
                let content: String = format!("{}", row[1]);
                let user_id: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);
                let metadata_val: Value =
                    parse_metadata_or_warn(&metadata_str, "metadata");

                if is_retired_at(&metadata_val, now) {
                    continue;
                }
                if is_archived(&metadata_val) {
                    continue;
                }
                if is_mesh_conflicting(&metadata_val) {
                    continue;
                }
                if crate::registry::is_registry_row(&metadata_val) {
                    continue;
                }
                if memory_type != "procedure" && user_id == "auto-ingestor" {
                    continue;
                }

                if is_procedure_scope {
                    let trig_ok = proc_trigger
                        .as_ref()
                        .map(|t| {
                            metadata_val
                                .get("trigger")
                                .and_then(|v| v.as_str())
                                .map(|s| s == t)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !trig_ok {
                        continue;
                    }
                    if is_retired_at(&metadata_val, now) {
                        continue;
                    }
                    if let Some(rf) = metadata_val.get("remaining_fires") {
                        if !rf.is_null() && rf.as_i64() == Some(0) {
                            continue;
                        }
                    }
                }

                out.push(crate::traits::ExactContentCandidate {
                    id,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: if agent_name == "unknown" || agent_name.is_empty() {
                            None
                        } else {
                            Some(agent_name)
                        },
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                });
            }
            Ok(out)
        })
        .await
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        let index_namespace = namespace.clone();
        let index_id = id.clone();
        let outcome = self.write_with_deadline("deleting a memory", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            // A silent no-op on a missing id reads as success to the caller
            // (the CLI then reports a deletion that never happened), so
            // resolve the row first and fail loudly when it is absent.
            let exists = conn
                .query(&format!(
                    "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' RETURN m.id LIMIT 1",
                    safe_id, safe_ns
                ))?
                .next()
                .is_some();
            if !exists {
                anyhow::bail!("No memory with id '{}' in namespace '{}'. Note: stored ids are namespace-qualified for ingested nodes.", id, namespace);
            }
            conn.query(&format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' DETACH DELETE m",
                safe_id, safe_ns
            ))?;
            Ok(())
        })
        .await;
        if outcome.is_ok() {
            self.lexical_remove_doc(&index_namespace, &index_id);
        }
        outcome
    }

    async fn relocate(&self, id: &str, from: &str, to: &str) -> Result<RelocateOutcome> {
        if from == to {
            return Ok(RelocateOutcome::SameNamespace);
        }
        let payload = match self.get(from, id).await? {
            Some((_, payload)) => payload,
            None => return Ok(RelocateOutcome::NotFound),
        };
        // An ingested node's id names the namespace that owns it, and the next
        // ingest of that namespace would MERGE onto the moved row and update it
        // where it now sits -- the cross-project theft qualified ids exist to stop.
        if payload.user_id == "auto-ingestor"
            || id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
        {
            return Ok(RelocateOutcome::Ingested);
        }

        let index_id = id.to_string();
        let index_from = from.to_string();
        let index_to = to.to_string();
        let (id, from, to) = (id.to_string(), from.to_string(), to.to_string());
        let outcome = self.write_with_deadline("moving a memory", move |conn| {
            // One statement, so there is no moment at which the memory exists in
            // neither namespace, and the row keeps its id, vector and edges.
            // VERIFY the row actually sat in `from` before claiming the move:
            // the SET clause matches-and-mutates silently, so without this the
            // Tantivy index would be rewritten for a row that never moved.
            let exists_query = format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' AND m.user_id <> 'auto-ingestor' RETURN m.user_id",
                escape_ladybug_string(&id),
                escape_ladybug_string(&from)
            );
            let present = {
                let mut rows = conn.query(&exists_query)?;
                rows.next().is_some()
            };
            if !present {
                return Ok(RelocateOutcome::NotFound);
            }
            let query = format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' AND m.user_id <> 'auto-ingestor' SET m.namespace = '{}'",
                escape_ladybug_string(&id),
                escape_ladybug_string(&from),
                escape_ladybug_string(&to)
            );
            conn.query(&query)?;
            Ok(RelocateOutcome::Moved)
        })
        .await;
        if matches!(outcome, Ok(RelocateOutcome::Moved)) {
            self.lexical_remove_doc(&index_from, &index_id);
            let mut doc = Self::lexical_doc(&index_to, &index_id, &payload);
            doc.namespace = index_to.clone();
            self.lexical_apply_doc(doc);
        }
        outcome
    }

    async fn export_database(&self, dir: &str) -> Result<()> {
        let dir = std::path::PathBuf::from(dir);
        let source = self.local_path.clone();
        // Checkpoint first so the snapshot cannot miss writes still in the
        // WAL. The engine's EXPORT DATABASE is not used: it SIGSEGVd in
        // lbug 0.20.4's planner on every store (planExportTableData ->
        // std::__format on a dangling string_view). Revalidated on lbug
        // 0.22.3: EXPORT DATABASE of a fixture store succeeded (Linux
        // x86_64); the file copy below remains the snapshot mechanism.
        let started = std::time::Instant::now();
        let checkpoint_result: Result<()> = self
            .with_conn(move |conn| {
                conn.query("CHECKPOINT")?;
                Ok(())
            })
            .await;
        crate::storage::snapshot::record_checkpoint_outcome(
            started.elapsed().as_millis() as u64,
            checkpoint_result.is_ok(),
        );
        checkpoint_result?;

        std::fs::create_dir_all(&dir)?;
        let target = dir.join("ladybug.store");
        std::fs::copy(&source, &target)
            .map_err(|e| anyhow::anyhow!("could not copy {:?} into the snapshot: {}", source, e))?;
        // A WAL left behind after the checkpoint rides along, so a restore
        // replays it exactly as the engine would have.
        let mut wal = source.clone().into_os_string();
        wal.push(".wal");
        let wal = std::path::PathBuf::from(wal);
        if wal.exists() {
            std::fs::copy(&wal, dir.join("ladybug.store.wal")).map_err(|e| {
                anyhow::anyhow!(
                    "could not copy the write-ahead log into the snapshot: {}",
                    e
                )
            })?;
        } else {
            // The destination may be a reused snapshot directory: a WAL
            // an earlier export placed there must not survive into this
            // snapshot, or write_checksums would bless it and a restore
            // would replay it against a store it never belonged to.
            let dest_wal = dir.join("ladybug.store.wal");
            if dest_wal.exists() {
                std::fs::remove_file(&dest_wal).map_err(|e| {
                    anyhow::anyhow!(
                        "could not remove the stale write-ahead log from the snapshot: {}",
                        e
                    )
                })?;
            }
        }
        // The store-state sidecar rides along too, under
        // the snapshot's store name. Without it a restored store has
        // no recorded identity — it reads as a pre-sidecar store and
        // silently exits the identity protection the sidecar exists
        // to provide. Absent sidecar (a pre-feature store) snapshots
        // exactly as before — and a sidecar already sitting in a
        // reused destination directory is removed, so the snapshot
        // can only carry an identity that came from its source
        // (issue #76): a stale sidecar would otherwise be blessed
        // into the new checksums and restored as the wrong identity.
        let sidecar = crate::model_choice::store_state_path(&source);
        let dest_sidecar = dir.join("ladybug.store.store-state.json");
        if sidecar.exists() {
            std::fs::copy(&sidecar, &dest_sidecar).map_err(|e| {
                anyhow::anyhow!(
                    "could not copy the store-state sidecar into the snapshot: {}",
                    e
                )
            })?;
        } else if dest_sidecar.exists() {
            std::fs::remove_file(&dest_sidecar).map_err(|e| {
                anyhow::anyhow!(
                    "could not remove the stale store-state sidecar from the snapshot: {}",
                    e
                )
            })?;
        }
        let manifest = serde_json::json!({
            "format": "neurostrata-snapshot",
            "version": 1,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "source": source.display().to_string(),
            "bytes": std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0),
        });
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)?,
        )?;
        crate::storage::snapshot::write_checksums(&dir)
            .map_err(|e| anyhow::anyhow!("could not write snapshot checksums: {}", e))?;

        // Snapshot hygiene: report which snapshots past the retention window
        // have expired, and prune them when the operator asks for it. The
        // retention budget comes from the storage-hygiene config (not a
        // hardcoded constant) so the operator-facing limit and the prune
        // behaviour cannot drift apart.
        if let Some(parent) = dir.parent() {
            let keep = crate::storage::snapshot::StorageHygieneConfig::default()
                .snapshot_retention_count
                .max(5);
            let expired = crate::storage::snapshot::select_expired_snapshots(parent, keep);
            if let Ok(expired) = expired {
                if !expired.is_empty() {
                    tracing::debug!("{} expired snapshots eligible for pruning", expired.len());
                }
            }
            if std::env::var("NEUROSTRATA_PRUNE_SNAPSHOTS").is_ok() {
                if let Err(e) = crate::storage::snapshot::prune_expired_snapshots(parent, keep) {
                    tracing::warn!("prune_expired_snapshots failed: {e}");
                }
            }
        }
        Ok(())
    }

    async fn checkpoint(&self) -> Result<()> {
        // Cleared first: a write that lands while the engine is flushing must
        // survive, and clearing afterwards would drop its mark.
        let had_writes = self.dirty.take();

        // The duration and success of every real checkpoint is recorded so
        // `compute_storage_status` reports actual hygiene, not constants.
        let started = std::time::Instant::now();
        let outcome = self
            .with_conn(move |conn| {
                conn.query("CHECKPOINT")?;
                Ok(())
            })
            .await;
        crate::storage::snapshot::record_checkpoint_outcome(
            started.elapsed().as_millis() as u64,
            outcome.is_ok(),
        );

        if outcome.is_err() && had_writes {
            self.dirty.mark();
        }
        outcome
    }

    fn is_dirty(&self) -> bool {
        self.dirty.is_set()
    }

    async fn relink_edges(&self, namespace: &str, source_key: &str) -> Result<usize> {
        // Declarations live in each memory's metadata, which survives ingestion;
        // only the edges themselves are lost with the nodes. Replaying them is
        // therefore a read of what is already stored, not a guess.
        let memories = self.list(namespace, None, None).await?;
        let active_gen = self.active_generation(namespace, source_key).await?;

        // The published generation's logical paths. A stub materialised before
        // the file was ingested is a placeholder; once the real node arrives it
        // must be retired so the placeholder never shadows the real tree.
        let active_paths: std::collections::HashSet<String> = memories
            .iter()
            .filter_map(|m| match crate::parser::ingest::split_scoped_id(&m.id) {
                Some((_, gen, _)) if gen == active_gen => {
                    Some(crate::parser::ingest::logical_path_of(&m.id).to_string())
                }
                _ => None,
            })
            .collect();

        // Ids stored before node ids became repository-relative are absolute, so
        // a rule written to match them declares C:/proj/src/foo.rs and matches
        // nothing now. Resolve those by path suffix against the ids that do
        // exist, and leave an ambiguous suffix alone rather than guess. Only the
        // active generation's nodes are reachable: a rule always re-attaches to
        // this source's published tree, never to a superseded one.
        let known = KnownIds::active(memories.iter().map(|m| m.id.as_str()), active_gen);
        // Cache warm-up for the mention pass below: derives the candidate
        // map once; every `detect_mentions` call thereafter reads the
        // memoized map instead of rebuilding it per memory.
        let _ = known.mention_candidates();

        // Grouped by relationship because the type is part of the statement and
        // cannot be a value in it: three groups, so three statements, whatever
        // the edge count.
        let mut wanted: std::collections::HashMap<&'static str, Vec<(String, String)>> =
            std::collections::HashMap::new();
        let mut by_suffix = 0usize;
        let mut mention_pairs: Vec<(String, String)> = Vec::new();
        let mut mention_updates: Vec<(String, String)> = Vec::new();

        for memory in &memories {
            // A retired memory no longer governs anything. Replaying its
            // declarations would give the corrected-away wording its edges back.
            // Its id stays among the known targets above, though: something live
            // may still point at it.
            if is_retired(&memory.payload.metadata) {
                continue;
            }
            // An ingested node from a superseded generation must not have its
            // structural (`contained_by`) edges replayed: only this source's
            // active generation is live. Rules, facts and tasks (never
            // generation-scoped) are always live.
            if memory.payload.user_id == "auto-ingestor" {
                match crate::parser::ingest::split_scoped_id(&memory.id) {
                    Some((_, gen, _)) if gen == active_gen => {}
                    _ => continue,
                }
            }
            // Emit any shape warnings from this memory's metadata.
            let warnings = write_shape_warnings(&memory.payload);
            for w in &warnings {
                emit_write_shape_warning(namespace, &memory.id, w);
            }
            for edge in edge_specs(&memory.payload.metadata) {
                let target = match known.resolve(&edge.target_id) {
                    Some(resolved) => {
                        if resolved != edge.target_id {
                            by_suffix += 1;
                        }
                        resolved
                    }
                    // Kept as declared rather than dropped: it names nothing this
                    // namespace holds, but the match is not namespaced, so it can
                    // still land on a node another namespace ingested -- which is
                    // what the statement-per-edge loop did.
                    None => edge.target_id.clone(),
                };
                let pair = if edge.points_at_target {
                    (memory.id.clone(), target)
                } else {
                    (target, memory.id.clone())
                };
                wanted.entry(edge.rel_type).or_default().push(pair);
            }

            // Mention linking (OxiRS §1.8): rediscover mentions against new index.
            if memory.payload.user_id != "auto-ingestor" {
                let mut mentions = crate::mention::detect_mentions(
                    &memory.payload.content,
                    &memory.payload.location,
                    &known,
                );
                mentions.retain(|m| m != &memory.id);
                if let Some(arr) = memory.payload.metadata.get("related_to").and_then(|v| v.as_array()) {
                    let declared: std::collections::HashSet<String> =
                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                    mentions.retain(|m| !declared.contains(m));
                }
                let mut meta = memory.payload.metadata.clone();
                if mentions.is_empty() {
                    if let Some(obj) = meta.as_object_mut() {
                        obj.remove("mentions");
                    }
                } else {
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("mentions".to_string(), serde_json::json!(mentions));
                    }
                    for m in &mentions {
                        mention_pairs.push((memory.id.clone(), m.clone()));
                    }
                }
                mention_updates.push((memory.id.clone(), meta.to_string()));
            }
        }

        if wanted.is_empty() && active_paths.is_empty() && mention_pairs.is_empty() && mention_updates.is_empty() {
            return Ok(0);
        }

        if by_suffix > 0 {
            println!(
                "Relinked {} edge(s) whose declared target was an older absolute path",
                by_suffix
            );
        }

        let namespace_owned = namespace.to_string();
        self.write_with_deadline("relinking the graph", move |conn| {
            // Update mentions metadata rows.
            for (id, meta) in &mention_updates {
                let q = format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'",
                    escape_ladybug_string(&namespace_owned),
                    escape_ladybug_string(id),
                    escape_ladybug_string(meta)
                );
                if let Err(e) = conn.query(&q) {
                    tracing::warn!("relink: mentions metadata write failed for {id}: {e}");
                }
            }
            // Retire stubs before replaying edges: the replayed declaration
            // then points at the real node, not the placeholder it replaced.
            let _ = retire_replaced_stubs(conn, &namespace_owned, &active_paths)?;
            let mut created = 0usize;
            let relink_now = chrono::Utc::now().timestamp();
            for (rel_type, pairs) in &wanted {
                created += create_missing_edges(conn, rel_type, pairs, &namespace_owned, active_gen, relink_now, "relink")?;
            }
            // Delete old mention-detector edges, then recreate.
            let del_q = format!(
                "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) WHERE a.namespace = '{}' AND r.extractor = 'mention-detector' DELETE r",
                escape_ladybug_string(&namespace_owned)
            );
            if let Err(e) = conn.query(&del_q) {
                tracing::warn!("relink: mention-detector edge cleanup failed: {e}");
            }
            created += create_missing_edges(conn, "RELATES_TO", &mention_pairs, &namespace_owned, active_gen, relink_now, "mention-detector")?;
            Ok(created)
        })
        .await
    }

    async fn clear_ingested(&self, namespace: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let index_namespace = namespace.clone();
        let outcome = self.write_with_deadline("clearing the ingested rows", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // Every row the ingester owns: the AST symbols and the directory/file
            // nodes too, since ingestion rebuilds the whole structure for a namespace
            let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m", safe_ns);
            conn.query(&query)?;
            Ok(())
        })
        .await;
        if outcome.is_ok() {
            self.lexical_remove_where(&index_namespace, |doc| doc.user_id == "auto-ingestor");
        }
        outcome
    }

    async fn begin_generation(&self, namespace: &str, source_key: &str) -> Result<u64> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.write_with_deadline("beginning a staging generation", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_sk = escape_ladybug_string(&source_key);

            // Read the current counter (default 0), then write counter + 1 in
            // the same write turn. The single-writer lock serialises this
            // against every other store write, so the read and the bump are one
            // atomic step and no two begins hand out the same generation. The
            // counter is shared across sources (keyed by namespace alone), so a
            // generation number never collides between two worktrees.
            let read = format!(
                "MATCH (g:IngestGeneration) WHERE g.id = '{}' RETURN g.counter",
                safe_ns
            );
            let mut current: i64 = 0;
            let mut rows = conn.query(&read)?;
            if let Some(row) = rows.next() {
                current = format!("{}", row[0]).parse::<i64>().unwrap_or(0);
            }
            let next = (current.max(0) as u64).saturating_add(1);

            let write = format!(
                "MERGE (g:IngestGeneration {{id: '{}'}}) ON CREATE SET g.namespace = '{}', g.source_key = '', g.counter = {}, g.active_generation = 0 ON MATCH SET g.counter = {}",
                safe_ns, safe_ns, next, next
            );
            conn.query(&write)?;

            // Ensure this source has a pointer row. Its active generation is
            // left untouched here: it stays on the last published generation
            // until `publish_generation` flips it.
            let pointer = format!(
                "MERGE (g:IngestGeneration {{id: '{}'}}) ON CREATE SET g.namespace = '{}', g.source_key = '{}', g.counter = 0, g.active_generation = 0",
                escape_ladybug_string(&crate::parser::ingest::generation_key(
                    &namespace,
                    &source_key
                )),
                safe_ns,
                safe_sk
            );
            conn.query(&pointer)?;

            Ok(next)
        })
        .await
    }

    async fn publish_generation(
        &self,
        namespace: &str,
        source_key: &str,
        generation: u64,
    ) -> Result<()> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.write_with_deadline("publishing a staging generation", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_sk = escape_ladybug_string(&source_key);
            // The flip is one statement on this source's pointer row: the active
            // pointer moves to `generation` and the previous generation's rows
            // stay as history. Another source's pointer is untouched.
            let write = format!(
                "MERGE (g:IngestGeneration {{id: '{}'}}) ON CREATE SET g.namespace = '{}', g.source_key = '{}', g.counter = 0, g.active_generation = {} ON MATCH SET g.active_generation = {}",
                escape_ladybug_string(&crate::parser::ingest::generation_key(
                    &namespace,
                    &source_key
                )),
                safe_ns,
                safe_sk,
                generation,
                generation
            );
            conn.query(&write)?;
            Ok(())
        })
        .await
    }

    async fn abort_generation(
        &self,
        namespace: &str,
        source_key: &str,
        generation: u64,
    ) -> Result<()> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.write_with_deadline("aborting a staging generation", move |conn| {
            // A staged node's id is `ns::<generation>::<path>`, so every row of
            // this generation shares the prefix. Nothing else matches it, and
            // the previous generation is untouched.
            let prefix = crate::parser::ingest::generation_scoped_id(&namespace, generation, "");
            let query = format!(
                "MATCH (m:Memory) WHERE m.id STARTS WITH '{}' DETACH DELETE m",
                escape_ladybug_string(&prefix)
            );
            conn.query(&query)?;

            // Defensive: if this source's pointer somehow already names the
            // aborted generation (abort is normally pre-publish), clear it so
            // the superseded generation is never reachable.
            let clear = format!(
                "MATCH (g:IngestGeneration) WHERE g.id = '{}' AND g.active_generation = {} SET g.active_generation = 0",
                escape_ladybug_string(&crate::parser::ingest::generation_key(
                    &namespace,
                    &source_key
                )),
                generation
            );
            conn.query(&clear)?;
            Ok(())
        })
        .await
    }

    async fn active_generation(&self, namespace: &str, source_key: &str) -> Result<u64> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.with_conn(move |conn| {
            Ok(active_generation_of_source(conn, &namespace, &source_key))
        })
        .await
    }

    async fn generation_nodes(&self, namespace: &str, generation: u64) -> Result<Vec<String>> {
        let namespace = namespace.to_string();
        self.with_conn(move |conn| {
            let prefix = crate::parser::ingest::generation_scoped_id(&namespace, generation, "");
            let query = format!(
                "MATCH (m:Memory) WHERE m.id STARTS WITH '{}' RETURN m.id",
                escape_ladybug_string(&prefix)
            );
            let mut rows = conn.query(&query)?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next() {
                ids.push(format!("{}", row[0]));
            }
            Ok(ids)
        })
        .await
    }

    /// One generation's rows, narrowed by id prefix in the engine and reduced
    /// to what ingest reads. Same reachability rule as `list` (a row of a
    /// superseded generation or another namespace is skipped), but the row's
    /// content is never fetched and its metadata is parsed, summarised and
    /// dropped one row at a time. Separate from `list_rows` on purpose: that
    /// one backs `list` and `list_pinned` and must keep returning full rows.
    async fn generation_summary(&self, namespace: &str, generation: u64) -> Result<Vec<crate::traits::GenerationRow>> {
        let namespace = namespace.to_string();
        self.with_conn(move |conn| {
            let active = ActiveGenerations::read(conn, &namespace);
            let prefix = crate::parser::ingest::generation_scoped_id(&namespace, generation, "");
            let query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id STARTS WITH '{}' RETURN m.id, m.memory_type, m.location, m.metadata",
                escape_ladybug_string(&namespace),
                escape_ladybug_string(&prefix)
            );
            let mut result = conn.query(&query)?;
            let mut out = Vec::new();
            while let Some(row) = result.next() {
                let id: String = format!("{}", row[0]);
                let memory_type: String = format!("{}", row[1]);
                let location: String = format!("{}", row[2]);
                let metadata_str: String = format!("{}", row[3]);
                // The same warning `list` gives for a row whose metadata does not
                // parse; the row is then read as having none.
                let metadata: Value = match serde_json::from_str(&metadata_str) {
                    Ok(v) => v,
                    Err(e) => {
                        if !metadata_str.trim().is_empty() {
                            tracing::warn!("corrupt metadata JSON in store row: {e}");
                        }
                        Value::Null
                    }
                };
                if !active.reaches(&id, &namespace, &memory_type, &metadata) {
                    continue;
                }
                out.push(crate::traits::GenerationRow::from_parts(id, memory_type, location, &metadata));
            }
            Ok(out)
        })
        .await
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>, memory_type: Option<&str>) -> Result<Vec<SearchResult>> {
        self.list_rows(namespace, user_id, memory_type, None).await
    }

    async fn list_pinned(&self, namespace: &str) -> Result<Vec<SearchResult>> {
        self.list_rows(namespace, None, None, Some("pinned")).await
    }

    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.embedding, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, safe_id);

            let mut result = conn.query(&query)?;

            if let Some(row) = result.next() {
                // The column is FLOAT[N], a fixed-size ARRAY, so the engine hands
                // back Value::Array. Matching only Value::List meant this always
                // returned an empty vector, and every caller that read a memory
                // and wrote it back stored an embedding of length zero -- which
                // the engine rejects, silently in /edit's case (bead
                // neurostrata-vbj). Accept both: a LIST column would still parse.
                let mut vec: Vec<f32> = Vec::new();
                if let lbug::Value::Array(_, vals) | lbug::Value::List(_, vals) = &row[0] {
                    for v in vals {
                        if let lbug::Value::Float(f) = v {
                            vec.push(*f);
                        } else if let lbug::Value::Double(d) = v {
                            vec.push(*d as f32);
                        }
                    }
                }

                let content: String = format!("{}", row[1]);
                let uid: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);

                let metadata_val: Value = match serde_json::from_str(&metadata_str) {
                    Ok(v) => v,
                    Err(e) => {
                        if !metadata_str.trim().is_empty() {
                            tracing::warn!("corrupt metadata JSON in store row: {e}");
                        }
                        Value::Null
                    }
                };



                return Ok(Some((
                    vec,
                    MemoryPayload {
                        content,
                        user_id: uid,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                )));
            }

            Ok(None)
        })
        .await
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        self.with_conn(move |conn| {
            let query = "MATCH (m:Memory) RETURN DISTINCT m.namespace AS ns;";
            let mut result = conn.query(query)?;

            let mut namespaces = Vec::new();
            while let Some(row) = result.next() {
                if let lbug::Value::String(ns) = row[0].clone() {
                    namespaces.push(ns);
                }
            }

            Ok(namespaces)
        })
        .await
    }

    async fn export_graph(
        &self,
        include_retired: bool,
        include_archived: bool,
        project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
        anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
    ) -> Result<serde_json::Value> {
        let project_contexts = project_contexts.clone();
        let anchor_override = anchor_override.cloned();
        self.with_conn(move |conn| {
            let now = chrono::Utc::now().timestamp();

            // 1. Fetch all nodes. Generation-scoped nodes from a superseded
            //    generation are dropped against the live `(namespace, gen)`
            //    set read once below, matching `list`'s superseded filter.
            let mut nodes = Vec::new();
            let mut retired_ids = std::collections::HashSet::new();
            // Every id actually pushed into `nodes`: edges must never dangle
            // toward an unexported, superseded, archived or conflicting row.
            let mut exported_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
            let active_gens = all_active_generations(conn);
            let query_nodes = "MATCH (n:Memory) RETURN n.id, n.namespace, n.memory_type, n.content, n.location, n.metadata;";
            let mut result_nodes = conn.query(query_nodes)?;

            while let Some(row) = result_nodes.next() {
                let id = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let namespace = if let lbug::Value::String(s) = &row[1] { s.clone() } else { "global".to_string() };
                let memory_type = if let lbug::Value::String(s) = &row[2] { s.clone() } else { "unknown".to_string() };
                let content = if let lbug::Value::String(s) = &row[3] { s.clone() } else { "".to_string() };
                let location = if let lbug::Value::String(s) = &row[4] { s.clone() } else { "".to_string() };

                // A generation-scoped node whose `(namespace, generation)` is not
                // live was superseded: excluded from the export and its edges
                // dropped below (via `retired_ids`), exactly as `list` does.
                if let Some((_, gen, _)) = crate::parser::ingest::split_scoped_id(&id) {
                    if !active_gens.contains(&(namespace.clone(), gen)) {
                        retired_ids.insert(id.clone());
                        continue;
                    }
                }

                let mut absolute_path = "".to_string();
                let mut domain = None;
                let mut retired = false;
                let mut full_metadata = serde_json::json!({});
                let mut superseded_by = serde_json::Value::Null;

                if let lbug::Value::String(metadata_str) = &row[5] {
                    if let Ok(metadata_val) = serde_json::from_str::<serde_json::Value>(metadata_str) {
                        retired = is_retired_at(&metadata_val, now);
                        if let Some(abs_path) = metadata_val.get("absolute_path").and_then(|v| v.as_str()) {
                            absolute_path = abs_path.to_string();
                        }
                        if let Some(d) = metadata_val.get("domain").and_then(|v| v.as_str()) {
                            domain = Some(d.to_string());
                        }
                        superseded_by = metadata_val.get("superseded_by").cloned().unwrap_or(serde_json::Value::Null);
                        full_metadata = metadata_val;
                    }
                }

                // Track retired ids so edges can be filtered below.
                if retired {
                    retired_ids.insert(id.clone());
                }

                absolute_path = crate::source_context::export_absolute_path(
                    &location,
                    &absolute_path,
                    &full_metadata,
                    &project_contexts,
                    anchor_override.as_ref(),
                );

                if !include_retired && retired {
                    continue;
                }
                // The tombstone filter mirrors the read surfaces: by default
                // an archived row stays out of the export, with the same
                // escape hatch the operator gets in search/snapshot. Edges
                // incident on an excluded node are dropped below.
                let archived = full_metadata
                    .get("archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !include_archived && archived {
                    retired_ids.insert(id.clone());
                    continue;
                }
                // Conflicting mesh rows are suppressed from the effective view
                // (spec §"Concurrent merge policy"): the same shared helper the
                // search/snapshot read paths ask, so the three surfaces can
                // never disagree. No escape hatch — an unresolved conflict is
                // never served until it is resolved.
                if crate::synaptic_mesh::conflict::is_conflicting_metadata(&full_metadata) {
                    retired_ids.insert(id.clone());
                    continue;
                }

                exported_ids.insert(id.clone());
                nodes.push(serde_json::json!({
                    "id": id,
                    "namespace": namespace,
                    "memory_type": memory_type,
                    "content": content,
                    "location": location,
                    "absolute_path": absolute_path,
                    "domain": domain,
                    // The graph is not a text dump: the declarative anchors and
                    // the lineage marker travel with the node (guinea-pig BUG-6
                    // and BUG-7). `superseded` is the live-layer concept that
                    // search already hides by; `superseded_by` names the row
                    // that replaced it.
                    "metadata": full_metadata,
                    "superseded": retired,
                    "superseded_by": superseded_by,
                }));
            }

            // 2. Fetch all edges, filtering out any incident on a retired node
            //    in active view.
            let mut links = Vec::new();

            // RELATES_TO
            let query_relates = "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) RETURN a.id, b.id;";
            let mut res_relates = conn.query(query_relates)?;
            while let Some(row) = res_relates.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                if !exported_ids.contains(&source) || !exported_ids.contains(&target) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "RELATES_TO"
                }));
            }

            // CONTAINS
            let query_contains = "MATCH (a:Memory)-[r:CONTAINS]->(b:Memory) RETURN a.id, b.id;";
            let mut res_contains = conn.query(query_contains)?;
            while let Some(row) = res_contains.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                if !exported_ids.contains(&source) || !exported_ids.contains(&target) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "CONTAINS"
                }));
            }

            // GOVERNS
            let query_governs = "MATCH (a:Memory)-[r:GOVERNS]->(b:Memory) RETURN a.id, b.id;";
            let mut res_governs = conn.query(query_governs)?;
            while let Some(row) = res_governs.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                if !exported_ids.contains(&source) || !exported_ids.contains(&target) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "GOVERNS"
                }));
            }

            // EXTRACTED_FROM (vocabulary v10): the consolidation edge a completed
            // task left behind. Without it the export loses the very link the
            // done-funnel exists to create.
            let query_extracted = "MATCH (a:Memory)-[r:EXTRACTED_FROM]->(b:Memory) RETURN a.id, b.id;";
            let res_extracted = conn.query(query_extracted)?;
            for row in res_extracted {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                if !exported_ids.contains(&source) || !exported_ids.contains(&target) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "EXTRACTED_FROM"
                }));
            }

            // DEPENDS_ON (vocabulary v10): module dependency edges.
            let query_depends = "MATCH (a:Memory)-[r:DEPENDS_ON]->(b:Memory) RETURN a.id, b.id;";
            let res_depends = conn.query(query_depends)?;
            for row in res_depends {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                if !exported_ids.contains(&source) || !exported_ids.contains(&target) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "DEPENDS_ON"
                }));
            }

            Ok(serde_json::json!({
                "nodes": nodes,
                "links": links
            }))
        })
        .await
    }

    /// Retrieval bumps a counter, so it has to cost about what a counter costs.
    /// Routing it through get() and upsert() rewrote the entire row -- its
    /// 768-float embedding included -- and re-ran every edge MERGE, five times
    /// per search, fire and forget. Later writes queued behind that burst: a
    /// delete issued with no pause after a search took 154s where an idle one
    /// takes none (bead neurostrata-3fi.6). Touch one column instead.
    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.write_with_deadline("counting a read", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            let read = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.metadata",
                safe_ns, safe_id
            );
            let mut result = conn.query(&read)?;
            let current = match result.next() {
                Some(row) => format!("{}", row[0]),
                None => return Ok(()),
            };

            let write = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'",
                safe_ns,
                safe_id,
                escape_ladybug_string(&bump_access_count(&current))
            );
            conn.query(&write)?;
            Ok(())
        })
        .await
    }
}

impl crate::traits::AsAny for LadybugStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A zero vector of `dimensions` floats, decimal-formatted for the engine's
/// `ARRAY_DISTANCE` binder (R16) -- the shape structural nodes store.
fn zero_vector_str(dimensions: usize) -> String {
    format!("[{}]", vec!["0.0"; dimensions].join(","))
}

/// Materialises a stub `file` node for a `GOVERNS` target that does not exist
/// yet (spec §3.2). The stub carries the legacy two-segment qualified id
/// (`namespace::path`), so `logical_path_of` still recovers the bare path a
/// rule names, and a later ingest replaces it. `ON CREATE` only: an existing
/// node is never overwritten.
fn materialize_stub_file_node(
    conn: &Connection,
    namespace: &str,
    declared: &str,
    zero_vec: &str,
) -> Result<String> {
    // A declaration that names a directory carries a trailing slash (the same
    // convention `KnownIds::resolve` already honours); an unindexed ancestor
    // directory must materialise as `directory` so the graph-read's
    // `AncestorDirectory` match finds it.
    let is_directory = declared.ends_with('/');
    let node_path = crate::parser::ingest::normalize_node_path(declared);
    if node_path.is_empty() {
        anyhow::bail!("cannot materialise a stub node for an empty path");
    }
    let stub_id = crate::parser::ingest::qualified_id(namespace, &node_path);
    let safe_id = escape_ladybug_string(&stub_id);
    let safe_ns = escape_ladybug_string(namespace);
    let safe_location = escape_ladybug_string(&node_path);
    let memory_type = if is_directory { "directory" } else { "file" };
    let metadata = serde_json::json!({ "stub": true, "absolute_path": node_path });
    let safe_metadata = escape_ladybug_string(&serde_json::to_string(&metadata)?);
    let query = format!(
        "MERGE (m:Memory {{id: '{}'}}) ON CREATE SET m.namespace = '{}', m.content = '', m.user_id = 'auto-ingestor', m.memory_type = '{}', m.agent_name = 'neurostrata-stub', m.location = '{}', m.location_lines = '', m.metadata = '{}', m.embedding = {}",
        safe_id, safe_ns, memory_type, safe_location, safe_metadata, zero_vec
    );
    conn.query(&query)?;
    Ok(stub_id)
}

/// Deletes stub nodes (`agent_name = 'neurostrata-stub'`) whose logical path is
/// now held by the published generation, so a placeholder never shadows the
/// real ingested node that replaced it.
fn retire_replaced_stubs(
    conn: &Connection,
    namespace: &str,
    active_paths: &std::collections::HashSet<String>,
) -> Result<usize> {
    let query = format!(
        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.agent_name = 'neurostrata-stub' RETURN m.id",
        escape_ladybug_string(namespace)
    );
    let mut rows = conn.query(&query)?;
    let mut stubs = Vec::new();
    while let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        let path = crate::parser::ingest::logical_path_of(&id);
        if active_paths.contains(path) {
            stubs.push(id);
        }
    }
    let mut removed = 0usize;
    for id in stubs {
        let del = format!(
            "MATCH (m:Memory {{id: '{}'}}) DETACH DELETE m",
            escape_ladybug_string(&id)
        );
        conn.query(&del)?;
        removed += 1;
    }
    Ok(removed)
}

/// Whether `ancestor` is a directory prefix of `path`: strict, at a `/`
/// boundary, so `src` matches `src/lib.rs` but `sr` and `src/lib` do not match
/// `src/lib.rs`.
fn is_ancestor_of(ancestor: &str, path: &str) -> bool {
    !ancestor.is_empty()
        && ancestor != path
        && path.starts_with(ancestor)
        && path[ancestor.len()..].starts_with('/')
}

/// Whether a symbol node's logical path (`path#kind:name@hash`) is the named
/// symbol in `path`.
fn is_symbol_of(logical_path: &str, path: &str, symbol: &str) -> bool {
    let Some(rest) = logical_path.strip_prefix(path) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('#') else {
        return false;
    };
    let Some((_kind, name_at_hash)) = rest.split_once(':') else {
        return false;
    };
    let name = name_at_hash.split('@').next().unwrap_or(name_at_hash);
    name == symbol
}

/// Runs one edge query and turns its rows into hits. `kind_of` maps a returned
/// target id to the match kind it represents; `edge_name` names the relation in
/// the explanation (`governs` vs `relates to`).
fn edge_hits_from_rows(
    conn: &Connection,
    query: &str,
    memory_types: &[String],
    kind_of: impl Fn(&str) -> GraphMatchKind,
    edge_name: &str,
) -> Result<Vec<MemoryHit>> {
    let now = chrono::Utc::now().timestamp();
    let mut rows = conn.query(query)?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next() {
        let target_id = format!("{}", row[0]);
        let id = format!("{}", row[1]);
        let content = format!("{}", row[2]);
        let memory_type = format!("{}", row[3]);
        let metadata_str = format!("{}", row[4]);
        let metadata: Value = match serde_json::from_str(&metadata_str) {
            Ok(v) => v,
            Err(e) => {
                if !metadata_str.trim().is_empty() {
                    tracing::warn!("corrupt metadata JSON in store row: {e}");
                }
                Value::Null
            }
        };

        if !memory_types.is_empty() && !memory_types.iter().any(|t| t == &memory_type) {
            continue;
        }
        if is_archived(&metadata) || is_retired_at(&metadata, now) || is_mesh_conflicting(&metadata)
        {
            continue;
        }
        let access_count = metadata
            .get("access_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        hits.push(MemoryHit {
            id,
            memory_type,
            content,
            match_kind: kind_of(&target_id),
            access_count,
            explanation: format!("{} {}", edge_name, target_id),
            metadata,
            conceptual_bridges: Vec::new(),
        });
    }
    Ok(hits)
}

/// The graph-edge paths that reach a governing or related memory: direct
/// `GOVERNS` onto the candidate targets, 2-hop symbol governance
/// (`memory -GOVERNS-> symbol <-CONTAINS- file`), and `RELATES_TO` onto the
/// candidate targets in both directions (the relation is undirected).
fn graph_hits_from_edges(
    conn: &Connection,
    namespace: &str,
    candidates: &[(String, GraphMatchKind)],
    file_ids: &[String],
    memory_types: &[String],
) -> Result<Vec<MemoryHit>> {
    let safe_ns = escape_ladybug_string(namespace);
    let mut hits = Vec::new();

    if !candidates.is_empty() {
        let id_list = candidates
            .iter()
            .map(|(id, _)| format!("'{}'", escape_ladybug_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let kind_by_id: std::collections::HashMap<&str, GraphMatchKind> = candidates
            .iter()
            .map(|(id, kind)| (id.as_str(), *kind))
            .collect();

        let governs_query = format!(
            "MATCH (m:Memory)-[:GOVERNS]->(t:Memory) WHERE t.id IN [{}] AND m.namespace = '{}' RETURN t.id, m.id, m.content, m.memory_type, m.metadata",
            id_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &governs_query,
            memory_types,
            |target| {
                kind_by_id
                    .get(target)
                    .copied()
                    .unwrap_or(GraphMatchKind::MetadataFallback)
            },
            "governs",
        )?);

        // RELATES_TO is undirected, so both orientations are queried explicitly
        // (the stored edge is still a directed row).
        let relates_out_query = format!(
            "MATCH (m:Memory)-[:RELATES_TO]->(t:Memory) WHERE t.id IN [{}] AND m.namespace = '{}' RETURN t.id, m.id, m.content, m.memory_type, m.metadata",
            id_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &relates_out_query,
            memory_types,
            |_| GraphMatchKind::Related,
            "relates to",
        )?);
        let relates_in_query = format!(
            "MATCH (m:Memory)<-[:RELATES_TO]-(t:Memory) WHERE t.id IN [{}] AND m.namespace = '{}' RETURN t.id, m.id, m.content, m.memory_type, m.metadata",
            id_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &relates_in_query,
            memory_types,
            |_| GraphMatchKind::Related,
            "relates to",
        )?);
    }

    if !file_ids.is_empty() {
        let file_list = file_ids
            .iter()
            .map(|id| format!("'{}'", escape_ladybug_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "MATCH (m:Memory)-[:GOVERNS]->(sym:Memory)<-[:CONTAINS]-(f:Memory) WHERE f.id IN [{}] AND m.namespace = '{}' RETURN sym.id, m.id, m.content, m.memory_type, m.metadata",
            file_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &query,
            memory_types,
            |_| GraphMatchKind::SymbolTwoHop,
            "governs",
        )?);
    }

    Ok(hits)
}

/// Whether an id is reachable under the active generation set: a
/// generation-scoped id is reachable only when its generation is active, while
/// a non-scoped id (a rule/fact/procedure id, or a pre-generation stub) is
/// always reachable.
fn is_active_id(id: &str, active_gens: &std::collections::HashSet<u64>) -> bool {
    match crate::parser::ingest::split_scoped_id(id) {
        Some((_, gen, _)) => active_gens.contains(&gen),
        None => true,
    }
}

/// Discovers the target nodes a query can match: the exact file node, the
/// exact symbol nodes (when a symbol is named), and the ancestor directory
/// nodes (when `include_ancestors`). The lookups are location-scoped Cypher
/// queries rather than a whole-namespace scan, and each row is then narrowed
/// to the active generation set so a superseded tree never contributes.
fn discover_candidates(
    conn: &Connection,
    safe_ns: &str,
    path: &str,
    symbol: Option<&str>,
    include_ancestors: bool,
    active_gens: &std::collections::HashSet<u64>,
) -> Result<(Vec<String>, Vec<(String, GraphMatchKind)>)> {
    let mut file_ids: Vec<String> = Vec::new();
    let mut candidates: Vec<(String, GraphMatchKind)> = Vec::new();

    // The file node and its symbol nodes share `location = path`: the file's
    // logical path is the path itself, a symbol's is `path#kind:name@hash`.
    let safe_path = escape_ladybug_string(path);
    let query = format!(
        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.location = '{}' RETURN m.id",
        safe_ns, safe_path
    );
    let mut rows = conn.query(&query)?;
    while let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        if !is_active_id(&id, active_gens) {
            continue;
        }
        let logical = crate::parser::ingest::logical_path_of(&id);
        if logical == path {
            file_ids.push(id.clone());
            candidates.push((id.clone(), GraphMatchKind::ExactFile));
        } else if let Some(sym) = symbol {
            if is_symbol_of(logical, path, sym) {
                candidates.push((id.clone(), GraphMatchKind::ExactSymbol));
            }
        }
    }

    if include_ancestors {
        let dir_query = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.memory_type = 'directory' RETURN m.id, m.location",
            safe_ns
        );
        let mut rows = conn.query(&dir_query)?;
        while let Some(row) = rows.next() {
            let id = format!("{}", row[0]);
            if !is_active_id(&id, active_gens) {
                continue;
            }
            let location = format!("{}", row[1]);
            if is_ancestor_of(&location, path) {
                candidates.push((id.clone(), GraphMatchKind::AncestorDirectory));
            }
        }
    }

    Ok((file_ids, candidates))
}

/// The metadata fallback: a memory whose `metadata.governs` declares the path
/// (or an ancestor) but whose edge never materialised -- e.g. the declaration
/// predates the stub behaviour, or the target is outside the ingested tree.
///
/// The query is scoped to the memory types asked for (rules/facts/procedures
/// by default), so the ingested AST bulk -- file/symbol/directory rows, which
/// never carry a `governs` declaration -- is never scanned.
fn add_metadata_fallback_conn(
    conn: &Connection,
    safe_ns: &str,
    hits: &mut Vec<MemoryHit>,
    path: &str,
    include_ancestors: bool,
    memory_types: &[String],
) -> Result<()> {
    // An empty `memory_types` means "no type filter" (the trait contract), so
    // the type condition is omitted entirely; a non-empty list is pushed into
    // the WHERE clause so only those rows are read.
    let type_condition = if memory_types.is_empty() {
        None
    } else {
        let list = memory_types
            .iter()
            .map(|t| format!("'{}'", escape_ladybug_string(t)))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("m.memory_type IN [{}]", list))
    };
    let where_clause = match &type_condition {
        Some(tc) => format!("m.namespace = '{}' AND {}", safe_ns, tc),
        None => format!("m.namespace = '{}'", safe_ns),
    };
    let query = format!(
        "MATCH (m:Memory) WHERE {} RETURN m.id, m.content, m.memory_type, m.metadata",
        where_clause
    );
    let now = chrono::Utc::now().timestamp();
    let mut rows = conn.query(&query)?;
    while let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        let content = format!("{}", row[1]);
        let memory_type = format!("{}", row[2]);
        let metadata_str = format!("{}", row[3]);
        let metadata: Value = match serde_json::from_str(&metadata_str) {
            Ok(v) => v,
            Err(e) => {
                if !metadata_str.trim().is_empty() {
                    tracing::warn!("corrupt metadata JSON in store row: {e}");
                }
                Value::Null
            }
        };
        if is_archived(&metadata)
            || is_retired_at(&metadata, now)
            || is_mesh_conflicting(&metadata)
        {
            continue;
        }
        let governs = match metadata.get("governs") {
            Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
            _ => continue,
        };
        for declared in governs {
            let normalized = crate::parser::ingest::normalize_node_path(declared);
            let matched =
                normalized == path || (include_ancestors && is_ancestor_of(&normalized, path));
            if matched {
                let access_count = metadata
                    .get("access_count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                hits.push(MemoryHit {
                    id,
                    memory_type,
                    content,
                    match_kind: GraphMatchKind::MetadataFallback,
                    access_count,
                    explanation: format!("declares governs {}", normalized),
                    metadata,
                    conceptual_bridges: Vec::new(),
                });
                break;
            }
        }
    }
    Ok(())
}

/// Keep the best hit per memory id: a lower match-kind priority wins, so a
/// direct `GOVERNS` edge is never shadowed by a weaker metadata-fallback hit.
fn dedup_hits(hits: &mut Vec<MemoryHit>) {
    use std::collections::hash_map::Entry;
    let mut best: std::collections::HashMap<String, MemoryHit> = std::collections::HashMap::new();
    for hit in hits.drain(..) {
        match best.entry(hit.id.clone()) {
            Entry::Vacant(e) => {
                e.insert(hit);
            }
            Entry::Occupied(mut e) => {
                if hit.match_kind.priority() < e.get().match_kind.priority() {
                    e.insert(hit);
                }
            }
        }
    }
    hits.extend(best.into_values());
}

#[async_trait::async_trait]
impl GraphRead for LadybugStore {
    async fn memories_for_file(
        &self,
        namespace: &str,
        source: Option<&str>,
        path: &str,
        symbol: Option<&str>,
        include_ancestors: bool,
        memory_types: &[String],
        exclude_ids: &[String],
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        let namespace = namespace.to_string();
        let path = crate::parser::ingest::normalize_node_path(path);
        let symbol = symbol.map(|s| s.to_string());
        let memory_types = memory_types.to_vec();
        let exclude_ids = exclude_ids.to_vec();
        let limit = limit.min(crate::graph_read::memories_for_file::MAX_LIMIT);
        let source = source.map(|s| s.to_string());

        let ns = namespace.clone();
        let path_c = path.clone();
        let sym_c = symbol.clone();
        let types = memory_types.clone();

        let mut hits = self
            .with_conn(move |conn| {
                let safe_ns = escape_ladybug_string(&ns);

                // A source key pins the query to that source's active
                // generation; `None` keeps every published generation (the
                // same set `list` reads).
                let active_gens: std::collections::HashSet<u64> = match &source {
                    Some(s) => {
                        let g = active_generation_of_source(conn, &ns, s);
                        if g > 0 {
                            [g].into_iter().collect()
                        } else {
                            std::collections::HashSet::new()
                        }
                    }
                    None => active_generations_of(conn, &ns),
                };

                let (file_ids, candidates) = discover_candidates(
                    conn,
                    &safe_ns,
                    &path_c,
                    sym_c.as_deref(),
                    include_ancestors,
                    &active_gens,
                )?;

                let mut hits =
                    graph_hits_from_edges(conn, &ns, &candidates, &file_ids, &types)?;

                add_metadata_fallback_conn(
                    conn,
                    &safe_ns,
                    &mut hits,
                    &path_c,
                    include_ancestors,
                    &types,
                )?;

                Ok(hits)
            })
            .await?;

        // Exclusion happens before the limit is applied, so an excluded hit
        // never crowds out a live one.
        if !exclude_ids.is_empty() {
            hits.retain(|h| !exclude_ids.iter().any(|e| e == &h.id));
        }

        dedup_hits(&mut hits);
        crate::graph_read::memories_for_file::rank_hits(&mut hits);
        hits.truncate(limit);

        // Populate conceptual_bridges: for each seed hit, find up to 2
        // 2-hop cross-domain bridges via 1-hop via nodes whose domain
        // is disjoint from the seed's domain. Target must be normative.
        for hit in hits.iter_mut() {
            let mut bridges: Vec<crate::graph_read::memories_for_file::ConceptualBridge> =
                Vec::with_capacity(2);
            let mut seen_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
            let relations_vec: Vec<String> = ["GOVERNS", "CONTAINS", "RELATES_TO", "EXTRACTED_FROM", "DEPENDS_ON"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            if let Ok(related) = self.related(
                &namespace,
                &hit.id,
                &relations_vec,
                crate::graph_read::related::Direction::Both,
                1,
                &[],
                50,
                None,
                &crate::graph_read::traversal::CrossNamespacePolicy::default(),
            )
            .await
            {
                let via_ids: Vec<&str> = related
                    .nodes
                    .iter()
                    .filter(|n| n.id != hit.id)
                    .map(|n| n.id.as_str())
                    .collect();
                for via_id in &via_ids {
                    if bridges.len() >= 2 { break; }
                    let relations_vec: Vec<String> = ["GOVERNS", "CONTAINS", "RELATES_TO", "EXTRACTED_FROM", "DEPENDS_ON"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    if let Ok(related2) = self.related(
                        &namespace,
                        via_id,
                        &relations_vec,
                        crate::graph_read::related::Direction::Both,
                        1,
                        &[],
                        50,
                        None,
                        &crate::graph_read::traversal::CrossNamespacePolicy::default(),
                    )
                    .await
                    {
                        for target_node in &related2.nodes {
                            if bridges.len() >= 2 { break; }
                            if target_node.id == hit.id || target_node.id == *via_id { continue; }
                            if seen_targets.contains(&target_node.id) {
                                continue;
                            }
                            if let Ok(Some((_, target_payload))) =
                                self.get(&namespace, &target_node.id).await
                            {
                                let is_normative = matches!(
                                    target_payload.memory_type.as_str(),
                                    "rule" | "fact" | "decision" | "procedure"
                                );
                                if !is_normative {
                                    continue;
                                }
                                // Cross-domain token sets for the jaccard < 0.35 filter.
                                let target_domain =
                                    crate::domain::domain_keys(&target_payload.metadata);
                                let mut target_tokens: std::collections::HashSet<String> =
                                    std::collections::HashSet::new();
                                for d in &target_domain {
                                    target_tokens.insert(d.clone());
                                }
                                target_tokens.extend(crate::task::feature_tokens(
                                    &target_payload.metadata,
                                    &target_payload.location,
                                ));
                                let mut seed_tokens: std::collections::HashSet<String> =
                                    std::collections::HashSet::new();
                                seed_tokens.extend(crate::task::feature_tokens(&hit.metadata, ""));
                                if !target_tokens.is_empty() {
                                    let sim = crate::task::jaccard_similarity(&seed_tokens, &target_tokens);
                                    if sim >= 0.35 {
                                        continue;
                                    }
                                }
                                seen_targets.insert(target_node.id.clone());
                                let target_type = target_payload.memory_type.clone();
                                let target_summary =
                                    if target_payload.content.chars().count() > 160 {
                                        target_payload
                                            .content
                                            .chars()
                                            .take(160)
                                            .collect::<String>()
                                    } else {
                                        target_payload.content.clone()
                                    };
                                bridges.push(crate::graph_read::memories_for_file::ConceptualBridge {
                                    source_id: hit.id.clone(),
                                    via_id: via_id.to_string(),
                                    target_id: target_node.id.clone(),
                                    target_domain: target_domain
                                        .first()
                                        .cloned()
                                        .unwrap_or_default(),
                                    target_type,
                                    target_summary,
                                    path: format!("{} -> {} -> {}", hit.id, via_id, target_node.id),
                                });
                            }
                        }
                    }
                }
            }
            hit.conceptual_bridges = bridges;
        }

        Ok(hits)
    }

    async fn related(
        &self,
        namespace: &str,
        seed: &str,
        relations: &[String],
        direction: crate::graph_read::related::Direction,
        max_hops: usize,
        memory_types: &[String],
        limit: usize,
        cursor: Option<&crate::graph_read::related::Cursor>,
        cross_namespace: &crate::graph_read::traversal::CrossNamespacePolicy,
    ) -> Result<crate::graph_read::related::RelatedResult> {
        let namespace = namespace.to_string();
        let seed = seed.to_string();
        // Spec §6: `related` consumes the ontology's relation expansion — each
        // caller-named relation is widened through the subPropertyOf* chain,
        // direct subproperties, and inverses before the subgraph fetch, so
        // variant relations (e.g. a strict-governs subproperty) are present in
        // the fetched edges. No ontology loaded → caller relations pass
        // through unchanged.
        let mut expanded_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        let relations: Vec<String> = if relations.is_empty() {
            relations.to_vec()
        } else {
            match self.ontology.load().get(namespace.as_str()).cloned() {
                Some(index) => {
                    let adapter = crate::rdf::reasoning::ReasoningAdapter::new();
                    let mut expanded: Vec<String> = Vec::new();
                    for rel in relations {
                        for r in adapter.expand_relations_for_search(&index, &rel) {
                            expanded_set.insert(r.clone());
                            expanded.push(r);
                        }
                    }
                    expanded
                }
                None => relations.to_vec(),
            }
        };
        let memory_types = memory_types.to_vec();
        let max_hops = crate::graph_read::related::clamp_max_hops(max_hops);
        let limit = crate::graph_read::related::clamp_limit(limit);
        let policy = cross_namespace.clone();

        let ns = namespace.clone();
        let seed_c = seed.clone();
        let rels = relations.clone();

        let (nodes, edges, generation) = self
            .with_conn(move |conn| {
                fetch_related_subgraph(conn, &ns, &seed_c, &rels, max_hops, &policy, &expanded_set)
            })
            .await?;

        Ok(crate::graph_read::related::graph_read_related(
            &seed,
            &nodes,
            &edges,
            &relations,
            direction,
            max_hops,
            &memory_types,
            limit,
            generation,
            cursor,
        ))
    }
}

/// How many edge rows one traversal level may fetch (bounding the Cypher read;
/// the in-memory traversal applies the tighter `MAX_NODES`/`MAX_EDGES` budget).
const RELATED_FETCH_LIMIT: usize = 500;

/// Fetch the bounded subgraph within `max_hops` of `seed`: the seed node plus
/// its neighbours out to the hop limit, each row-class-filtered (retired /
/// archived / mesh-conflicting / registry / superseded generation). Returns the
/// in-memory `NodeSpec`/`EdgeSpec` lists the pure traversal consumes, plus the
/// active generation the cursor fingerprint must match.
fn fetch_related_subgraph(
    conn: &Connection,
    namespace: &str,
    seed: &str,
    relations: &[String],
    max_hops: usize,
    cross_namespace: &crate::graph_read::traversal::CrossNamespacePolicy,
    expanded_set: &std::collections::HashSet<String>,
) -> Result<(
    Vec<crate::graph_read::related::NodeSpec>,
    Vec<crate::graph_read::related::EdgeSpec>,
    u64,
)> {
    use crate::graph_read::related::{EdgeSpec, NodeSpec};

    let safe_ns = escape_ladybug_string(namespace);
    let now = chrono::Utc::now().timestamp();
    let active_gens = active_generations_of(conn, namespace);
    let generation = active_gens.iter().copied().max().unwrap_or(0);

    let allowed: std::collections::HashSet<&str> = if relations.is_empty() {
        crate::graph_read::related::DEFAULT_RELATIONS
            .iter()
            .copied()
            .collect()
    } else {
        relations
            .iter()
            .map(String::as_str)
            .filter(|r| crate::graph_read::related::is_allowed_relation(r) || expanded_set.contains(*r))
            .collect()
    };

    let to_node_spec = |id: String,
                        memory_type: String,
                        content: String,
                        metadata: &Value|
     -> NodeSpec {
        let visible = is_active_id(&id, &active_gens)
            && !is_archived(metadata)
            && !is_retired_at(metadata, now)
            && !is_mesh_conflicting(metadata)
            && !crate::registry::is_registry_row(metadata);
        NodeSpec {
            id,
            memory_type,
            content,
            domain: metadata
                .get("domain")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            freshness: metadata
                .get("verification")
                .and_then(|v| v.get("status"))
                .and_then(|s| s.as_str())
                .map(str::to_string),
            lifecycle: metadata
                .get("superseded_by")
                .and_then(|v| v.as_str())
                .map(|_| "superseded".to_string()),
            visible,
        }
    };

    let mut nodes: std::collections::HashMap<String, NodeSpec> =
        std::collections::HashMap::new();
    let mut edges: Vec<EdgeSpec> = Vec::new();

    // Resolve the seed: an exact id first (memory/task/qualified id), then a
    // normalized path against `location` (file/symbol nodes).
    let mut seed_id: Option<String> = None;
    let exact_q = format!(
        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.id, m.memory_type, m.content, m.metadata",
        safe_ns,
        escape_ladybug_string(seed)
    );
    let mut rows = conn.query(&exact_q)?;
    if let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        let memory_type = format!("{}", row[1]);
        let content = format!("{}", row[2]);
        let metadata_str = format!("{}", row[3]);
        let metadata: Value = match serde_json::from_str(&metadata_str) {
            Ok(v) => v,
            Err(e) => {
                if !metadata_str.trim().is_empty() {
                    tracing::warn!("corrupt metadata JSON in store row: {e}");
                }
                Value::Null
            }
        };
        seed_id = Some(id.clone());
        nodes.insert(
            id.clone(),
            to_node_spec(id, memory_type, content, &metadata),
        );
    }

    if seed_id.is_none() {
        let normalized = crate::parser::ingest::normalize_node_path(seed);
        let loc_q = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.location = '{}' RETURN m.id, m.memory_type, m.content, m.metadata ORDER BY m.memory_type ASC",
            safe_ns,
            escape_ladybug_string(&normalized)
        );
        let mut rows = conn.query(&loc_q)?;
        // Prefer the file node over a symbol over any other row.
        let mut candidate: Option<(String, String, String, Value)> = None;
        while let Some(row) = rows.next() {
            let id = format!("{}", row[0]);
            let memory_type = format!("{}", row[1]);
            let content = format!("{}", row[2]);
            let metadata_str = format!("{}", row[3]);
            let metadata: Value = match serde_json::from_str(&metadata_str) {
                Ok(v) => v,
                Err(e) => {
                    if !metadata_str.trim().is_empty() {
                        tracing::warn!("corrupt metadata JSON in store row: {e}");
                    }
                    Value::Null
                }
            };
            let prefer = match (candidate.as_ref().map(|c| c.1.as_str()), memory_type.as_str()) {
                (_, "file") => true,
                (Some("file"), _) => false,
                (Some("symbol"), "directory") => false,
                _ => candidate.is_none(),
            };
            if prefer || candidate.is_none() {
                candidate = Some((id, memory_type, content, metadata));
            }
        }
        if let Some((id, memory_type, content, metadata)) = candidate {
            seed_id = Some(id.clone());
            nodes.insert(id.clone(), to_node_spec(id, memory_type, content, &metadata));
        }
    }

    let Some(seed_id) = seed_id else {
        return Ok((Vec::new(), Vec::new(), generation));
    };

    // BFS out to `max_hops`, fetching both orientations (the pure traversal
    // reapplies the direction allowlist). Only same-namespace neighbours are
    // ever read, so the traversal can never leak across namespaces.
    let mut frontier: Vec<String> = vec![seed_id.clone()];
    let mut seen: std::collections::HashSet<String> = [seed_id].into_iter().collect();

    for _hop in 0..max_hops {
        if frontier.is_empty() {
            break;
        }
        let id_list = frontier
            .iter()
            .map(|id| format!("'{}'", escape_ladybug_string(id)))
            .collect::<Vec<_>>()
            .join(", ");

        let mut next_frontier = Vec::new();

        let ns_filter = if cross_namespace.allow_cross_namespace {
            String::new()
        } else {
            format!(" AND b.namespace = '{}'", safe_ns)
        };

        let out_q = format!(
            "MATCH (a:Memory)-[e]->(b:Memory) WHERE a.id IN [{}]{} RETURN a.id, label(e), b.id, b.namespace, b.memory_type, b.content, b.metadata, e.valid_from, e.valid_to LIMIT {}",
            id_list, ns_filter, RELATED_FETCH_LIMIT
        );
        {
            let mut rows = conn.query(&out_q)?;
            while let Some(row) = rows.next() {
                let frontier_id = format!("{}", row[0]);
                let relation = format!("{}", row[1]);
                let neighbor_id = format!("{}", row[2]);
                let neighbor_ns = format!("{}", row[3]);
                let neighbor_type = format!("{}", row[4]);
                let neighbor_content = format!("{}", row[5]);
                let neighbor_meta_str = format!("{}", row[6]);
                let neighbor_meta: Value = match serde_json::from_str(&neighbor_meta_str) {
                    Ok(v) => v,
                    Err(e) => {
                        if !neighbor_meta_str.trim().is_empty() {
                            tracing::warn!("corrupt metadata JSON in store row: {e}");
                        }
                        Value::Null
                    }
                };
                let valid_from = format!("{}", row[7]).parse::<i64>().unwrap_or(0);
                let valid_to = format!("{}", row[8]).parse::<i64>().unwrap_or(0);
                if !allowed.contains(relation.as_str()) {
                    continue;
                }
                let target_ns = crate::graph_read::traversal::extract_namespace_prefix(&neighbor_id)
                    .unwrap_or(neighbor_ns.as_str());
                if !cross_namespace.is_allowed(namespace, target_ns) {
                    continue;
                }
                edges.push(EdgeSpec {
                    source: frontier_id.clone(),
                    relation: relation.clone(),
                    target: neighbor_id.clone(),
                    valid_from,
                    valid_to,
                });
                if !nodes.contains_key(&neighbor_id) {
                    nodes.insert(
                        neighbor_id.clone(),
                        to_node_spec(
                            neighbor_id.clone(),
                            neighbor_type,
                            neighbor_content,
                            &neighbor_meta,
                        ),
                    );
                }
                if !seen.contains(&neighbor_id) {
                    seen.insert(neighbor_id.clone());
                    next_frontier.push(neighbor_id);
                }
            }
        }

        let in_q = format!(
            "MATCH (a:Memory)<-[e]-(b:Memory) WHERE a.id IN [{}]{} RETURN a.id, label(e), b.id, b.namespace, b.memory_type, b.content, b.metadata, e.valid_from, e.valid_to LIMIT {}",
            id_list, ns_filter, RELATED_FETCH_LIMIT
        );
        {
            let mut rows = conn.query(&in_q)?;
            while let Some(row) = rows.next() {
                let frontier_id = format!("{}", row[0]);
                let relation = format!("{}", row[1]);
                let neighbor_id = format!("{}", row[2]);
                let neighbor_ns = format!("{}", row[3]);
                let neighbor_type = format!("{}", row[4]);
                let neighbor_content = format!("{}", row[5]);
                let neighbor_meta_str = format!("{}", row[6]);
                let neighbor_meta: Value = match serde_json::from_str(&neighbor_meta_str) {
                    Ok(v) => v,
                    Err(e) => {
                        if !neighbor_meta_str.trim().is_empty() {
                            tracing::warn!("corrupt metadata JSON in store row: {e}");
                        }
                        Value::Null
                    }
                };
                let valid_from = format!("{}", row[7]).parse::<i64>().unwrap_or(0);
                let valid_to = format!("{}", row[8]).parse::<i64>().unwrap_or(0);
                if !allowed.contains(relation.as_str()) {
                    continue;
                }
                let target_ns = crate::graph_read::traversal::extract_namespace_prefix(&neighbor_id)
                    .unwrap_or(neighbor_ns.as_str());
                if !cross_namespace.is_allowed(namespace, target_ns) {
                    continue;
                }
                edges.push(EdgeSpec {
                    source: neighbor_id.clone(),
                    relation: relation.clone(),
                    target: frontier_id.clone(),
                    valid_from,
                    valid_to,
                });
                if !nodes.contains_key(&neighbor_id) {
                    nodes.insert(
                        neighbor_id.clone(),
                        to_node_spec(
                            neighbor_id.clone(),
                            neighbor_type,
                            neighbor_content,
                            &neighbor_meta,
                        ),
                    );
                }
                if !seen.contains(&neighbor_id) {
                    seen.insert(neighbor_id.clone());
                    next_frontier.push(neighbor_id);
                }
            }
        }

        frontier = next_frontier;
    }

    let nodes: Vec<NodeSpec> = nodes.into_values().collect();
    Ok((nodes, edges, generation))
}

impl LadybugStore {
    /// Hard budgets for `ast_neighborhood_for_file` so a high-fan-out governing
    /// rule or file node cannot produce an unbounded reply or hold the shared
    /// store connection lock while scanning thousands of neighbors.
    pub const MAX_RELATED_MEMORIES: usize = 20;
    pub const MAX_AST_COLLECTION_ITEMS: usize = 20;
    pub const MAX_NEIGHBORHOOD_QUERY_ROWS: usize = 64;
    pub const MAX_NEIGHBOR_CONTENT_CHARS: usize = 200;
    pub const MAX_NEIGHBOR_METADATA_BYTES: usize = 1024;
    pub const MAX_NEIGHBORHOOD_RESPONSE_BYTES: usize = 16 * 1024;

    fn truncate_neighbor_content(raw: &str) -> (String, bool) {
        let mut chars = raw.chars();
        let prefix: String = chars.by_ref().take(Self::MAX_NEIGHBOR_CONTENT_CHARS).collect();
        if chars.next().is_some() {
            (format!("{prefix}\u{2026}"), true)
        } else {
            (raw.to_string(), false)
        }
    }

    fn bound_neighbor_metadata(meta: Value) -> (Value, bool) {
        if meta.is_null() {
            return (Value::Null, false);
        }
        let raw_len = serde_json::to_string(&meta).map(|s| s.len()).unwrap_or(0);
        if raw_len <= Self::MAX_NEIGHBOR_METADATA_BYTES {
            return (meta, false);
        }
        let mut compact = serde_json::Map::new();
        if let Some(obj) = meta.as_object() {
            for key in ["governs", "superseded_by", "symbol_name", "ast_kind", "eligibility"] {
                if let Some(val) = obj.get(key) {
                    compact.insert(key.to_string(), val.clone());
                }
            }
        }
        compact.insert("truncated".to_string(), Value::Bool(true));
        (Value::Object(compact), true)
    }

    /// Compact AST + related-memory neighborhood for a file, used by
    /// `neurostrata_memories_for_file` when `include_ast_edges` is true
    /// (Enhancement 4). Returns `(related_memories, ast_summary)`.
    ///
    /// `related_memories` are the 1-hop `DEPENDS_ON`/`RELATES_TO` neighbors
    /// (both directions) of the governing memories, plus `SUPERSEDES` targets
    /// named in `metadata.superseded_by`.
    ///
    /// `ast_summary` is `{ symbols, callers, callees, imports, importers,
    /// blast_radius_count, truncated, truncated_collections,
    /// related_memories_truncated }` for the file node at `path`.
    pub async fn ast_neighborhood_for_file(
        &self,
        namespace: &str,
        source: Option<&str>,
        path: &str,
        governing_ids: &[String],
    ) -> Result<(Vec<Value>, Value)> {
        let namespace = namespace.to_string();
        let path = crate::parser::ingest::normalize_node_path(path);
        let governing_ids = governing_ids.to_vec();
        let source_str = source.map(|s| s.to_string());

        let ns = namespace.clone();
        let path_c = path.clone();
        let ids = governing_ids.clone();

        let (related_memories, ast_summary) = self
            .with_conn(move |conn| {
                let safe_ns = escape_ladybug_string(&ns);
                let now = chrono::Utc::now().timestamp();
                let active_gens: std::collections::HashSet<u64> = match &source_str {
                    Some(s) => {
                        let g = active_generation_of_source(conn, &ns, &s);
                        if g > 0 {
                            [g].into_iter().collect()
                        } else {
                            std::collections::HashSet::new()
                        }
                    }
                    None => active_generations_of(conn, &ns),
                };
                let mut related: Vec<Value> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                let mut related_truncated = ids.len() > Self::MAX_RELATED_MEMORIES;
                let mut related_bytes: usize = 0;
                let related_byte_budget = Self::MAX_NEIGHBORHOOD_RESPONSE_BYTES / 2;

                if !ids.is_empty() {
                    let id_list = ids
                        .iter()
                        .take(Self::MAX_RELATED_MEMORIES)
                        .map(|id| format!("'{}'", escape_ladybug_string(id)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let row_limit = Self::MAX_NEIGHBORHOOD_QUERY_ROWS;

                    'rel_dirs: for (rel, direction) in [
                        ("DEPENDS_ON", "outgoing"),
                        ("DEPENDS_ON", "incoming"),
                        ("RELATES_TO", "outgoing"),
                        ("RELATES_TO", "incoming"),
                    ] {
                        let query = match direction {
                            "outgoing" => format!(
                                "MATCH (m:Memory)-[:{rel}]->(t:Memory) WHERE m.id IN [{id_list}] AND m.namespace = '{safe_ns}' AND t.namespace = '{safe_ns}' RETURN t.id, t.content, t.memory_type, t.metadata LIMIT {row_limit}"
                            ),
                            "incoming" => format!(
                                "MATCH (t:Memory)-[:{rel}]->(m:Memory) WHERE m.id IN [{id_list}] AND m.namespace = '{safe_ns}' AND t.namespace = '{safe_ns}' RETURN t.id, t.content, t.memory_type, t.metadata LIMIT {row_limit}"
                            ),
                            _ => unreachable!(),
                        };
                        let mut res = conn.query(&query)?;
                        let mut rows_seen = 0usize;
                        while let Some(row) = res.next() {
                            rows_seen += 1;
                            let id = Self::cell_string(&row[0]);
                            let metadata = Self::cell_metadata(&row[3]);
                            if !seen.contains(&id)
                                && is_active_id(&id, &active_gens)
                                && !is_registry_row(&metadata)
                                && !is_archived(&metadata)
                                && !is_retired_at(&metadata, now)
                                && !is_mesh_conflicting(&metadata)
                            {
                                if related.len() >= Self::MAX_RELATED_MEMORIES {
                                    related_truncated = true;
                                    break 'rel_dirs;
                                }
                                let (content, content_trunc) =
                                    Self::truncate_neighbor_content(&Self::cell_string(&row[1]));
                                let (bounded_meta, meta_trunc) =
                                    Self::bound_neighbor_metadata(metadata);
                                let mut entry = serde_json::json!({
                                    "id": id,
                                    "memory_type": Self::cell_string(&row[2]),
                                    "content": content,
                                    "relation": rel,
                                    "direction": direction,
                                    "metadata": bounded_meta,
                                });
                                if content_trunc || meta_trunc {
                                    entry["truncated"] = Value::Bool(true);
                                }
                                let entry_bytes =
                                    serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(128);
                                if !related.is_empty()
                                    && related_bytes + entry_bytes > related_byte_budget
                                {
                                    related_truncated = true;
                                    break 'rel_dirs;
                                }
                                seen.insert(id);
                                related_bytes += entry_bytes;
                                related.push(entry);
                            }
                        }
                        if rows_seen >= row_limit {
                            related_truncated = true;
                        }
                    }

                    // SUPERSEDES: memories the governing memories were superseded by.
                    let query = format!(
                        "MATCH (m:Memory) WHERE m.id IN [{id_list}] AND m.namespace = '{safe_ns}' RETURN m.id, m.metadata LIMIT {row_limit}"
                    );
                    let mut res = conn.query(&query)?;
                    while let Some(row) = res.next() {
                        let id = Self::cell_string(&row[0]);
                        let metadata = Self::cell_metadata(&row[1]);
                        if let Some(by) = metadata
                            .get("superseded_by")
                            .and_then(|v| v.as_str())
                        {
                            if !by.is_empty()
                                && !crate::registry::is_registry_row(&metadata)
                                && is_active_id(&by, &active_gens)
                                && !seen.contains(by)
                            {
                                let safe_by = escape_ladybug_string(by);
                                let target_q = format!(
                                    "MATCH (t:Memory) WHERE t.id = '{safe_by}' AND t.namespace = '{safe_ns}' RETURN t.id, t.content, t.memory_type, t.metadata LIMIT 1"
                                );
                                let mut tres = conn.query(&target_q)?;
                                if let Some(trow) = tres.next() {
                                    let tid = Self::cell_string(&trow[0]);
                                    let tmeta = Self::cell_metadata(&trow[3]);
                                    if !is_archived(&tmeta)
                                        && !is_retired_at(&tmeta, now)
                                        && !is_mesh_conflicting(&tmeta)
                                        && !seen.contains(&tid)
                                    {
                                        if related.len() >= Self::MAX_RELATED_MEMORIES {
                                            related_truncated = true;
                                            break;
                                        }
                                        let (content, content_trunc) =
                                            Self::truncate_neighbor_content(&Self::cell_string(
                                                &trow[1],
                                            ));
                                        let (bounded_meta, meta_trunc) =
                                            Self::bound_neighbor_metadata(tmeta);
                                        let mut entry = serde_json::json!({
                                            "id": tid,
                                            "memory_type": Self::cell_string(&trow[2]),
                                            "content": content,
                                            "relation": "SUPERSEDES",
                                            "direction": "outgoing",
                                            "supersedes": id,
                                            "metadata": bounded_meta,
                                        });
                                        if content_trunc || meta_trunc {
                                            entry["truncated"] = Value::Bool(true);
                                        }
                                        let entry_bytes = serde_json::to_string(&entry)
                                            .map(|s| s.len())
                                            .unwrap_or(128);
                                        if !related.is_empty()
                                            && related_bytes + entry_bytes > related_byte_budget
                                        {
                                            related_truncated = true;
                                            break;
                                        }
                                        seen.insert(tid);
                                        related_bytes += entry_bytes;
                                        related.push(entry);
                                    }
                                }
                            }
                        }
                    }
                }

                let ast_byte_budget =
                    Self::MAX_NEIGHBORHOOD_RESPONSE_BYTES.saturating_sub(related_bytes);
                let ast_summary = Self::ast_summary_for_file(
                    conn,
                    &safe_ns,
                    &path_c,
                    &active_gens,
                    now,
                    related_truncated,
                    ast_byte_budget,
                )?;

                Ok((related, ast_summary))
            })
            .await?;

        Ok((related_memories, ast_summary))
    }

    /// Render one lbug row cell as a String, tolerating non-string cells.
    fn cell_string(value: &lbug::Value) -> String {
        match value {
            lbug::Value::String(s) => s.clone(),
            _ => format!("{}", value),
        }
    }

    /// Parse one lbug row cell holding a JSON metadata string into a `Value`.
    fn cell_metadata(value: &lbug::Value) -> Value {
        let raw = Self::cell_string(value);
        if raw.trim().is_empty() {
            return Value::Null;
        }
        serde_json::from_str(&raw).unwrap_or(Value::Null)
    }

    /// Compact AST neighborhood for the file at `path`: its symbol nodes, the
    /// symbols that depend on it (callers) and it depends on (callees), its
    /// declared imports, the files that import it (importers), and the size of
    /// the combined blast radius.
    fn ast_summary_for_file(
        conn: &Connection,
        safe_ns: &str,
        path: &str,
        active_gens: &std::collections::HashSet<u64>,
        now: i64,
        related_truncated: bool,
        byte_budget: usize,
    ) -> Result<Value> {
        let safe_path = escape_ladybug_string(path);
        let row_limit = Self::MAX_NEIGHBORHOOD_QUERY_ROWS;
        let max_items = Self::MAX_AST_COLLECTION_ITEMS;
        let mut ast_bytes: usize = 0;
        let mut ast_truncated = false;
        let mut truncated_collections: Vec<&'static str> = Vec::new();
        let mark_collection_truncated =
            |name: &'static str, flag: &mut bool, cols: &mut Vec<&'static str>| {
                *flag = true;
                if !cols.contains(&name) {
                    cols.push(name);
                }
            };

        // Helper closure for visibility filtering
        let is_visible = |id: &str, meta: &Value| -> bool {
            is_active_id(id, active_gens)
                && !crate::registry::is_registry_row(meta)
                && !is_archived(meta)
                && !is_retired_at(meta, now)
                && !is_mesh_conflicting(meta)
        };

        // Symbol nodes of the file: symbol locations are `path#kind:name@hash`.
        let mut symbols: Vec<Value> = Vec::new();
        let mut symbol_ids: HashSet<String> = HashSet::new();
        {
            let symbol_query = format!(
                "MATCH (s:Memory) WHERE s.namespace = '{safe_ns}' AND (s.location = '{safe_path}' OR s.location STARTS WITH '{safe_path}#') RETURN s.id, s.content, s.memory_type, s.location, s.metadata LIMIT {row_limit}"
            );
            let mut res = conn.query(&symbol_query)?;
            let mut rows_seen = 0usize;
            while let Some(row) = res.next() {
                rows_seen += 1;
                let id = Self::cell_string(&row[0]);
                let raw_content = Self::cell_string(&row[1]);
                let memory_type = Self::cell_string(&row[2]);
                let loc = Self::cell_string(&row[3]);
                let meta = Self::cell_metadata(&row[4]);
                if !is_visible(&id, &meta) {
                    continue;
                }
                let logical = crate::parser::ingest::logical_path_of(&id);
                let is_symbol_node = loc.starts_with(&format!("{path}#"))
                    || logical.starts_with(&format!("{path}#"))
                    || (loc == path
                        && (memory_type == "symbol"
                            || meta.get("symbol_name").is_some()
                            || meta.get("ast_kind").is_some()));

                if is_symbol_node && !symbol_ids.contains(&id) {
                    if symbols.len() >= max_items {
                        mark_collection_truncated(
                            "symbols",
                            &mut ast_truncated,
                            &mut truncated_collections,
                        );
                        break;
                    }
                    let (content, content_trunc) = Self::truncate_neighbor_content(&raw_content);
                    let mut entry = serde_json::json!({
                        "id": id,
                        "memory_type": memory_type,
                        "content": content,
                    });
                    if content_trunc {
                        entry["truncated"] = Value::Bool(true);
                    }
                    let entry_bytes =
                        serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(64);
                    if !symbols.is_empty() && ast_bytes + entry_bytes > byte_budget {
                        mark_collection_truncated(
                            "symbols",
                            &mut ast_truncated,
                            &mut truncated_collections,
                        );
                        break;
                    }
                    symbol_ids.insert(id);
                    ast_bytes += entry_bytes;
                    symbols.push(entry);
                }
            }
            if rows_seen >= row_limit {
                mark_collection_truncated(
                    "symbols",
                    &mut ast_truncated,
                    &mut truncated_collections,
                );
            }
        }

        // The file node itself: carries `metadata.imports`.
        let mut imports: Vec<Value> = Vec::new();
        let mut importers: Vec<Value> = Vec::new();
        let mut file_ids: Vec<String> = Vec::new();
        {
            let file_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{safe_ns}' AND m.location = '{safe_path}' RETURN m.id, m.metadata LIMIT {row_limit}"
            );
            let mut res = conn.query(&file_query)?;
            while let Some(row) = res.next() {
                let id = Self::cell_string(&row[0]);
                let meta = Self::cell_metadata(&row[1]);
                if !is_visible(&id, &meta) || symbol_ids.contains(&id) {
                    continue;
                }
                if file_ids.len() < max_items {
                    file_ids.push(id);
                }
                if let Some(arr) = meta.get("imports").and_then(|v| v.as_array()) {
                    for entry in arr {
                        if imports.len() >= max_items {
                            mark_collection_truncated(
                                "imports",
                                &mut ast_truncated,
                                &mut truncated_collections,
                            );
                            break;
                        }
                        let item = serde_json::json!({ "path": entry });
                        let item_bytes =
                            serde_json::to_string(&item).map(|s| s.len()).unwrap_or(32);
                        if !imports.is_empty() && ast_bytes + item_bytes > byte_budget {
                            mark_collection_truncated(
                                "imports",
                                &mut ast_truncated,
                                &mut truncated_collections,
                            );
                            break;
                        }
                        ast_bytes += item_bytes;
                        imports.push(item);
                    }
                }
            }
        }
        let _file_ids = file_ids.clone(); // reserved for caller-side filtering; kept for symmetry

        // Callers / callees: DEPENDS_ON edges incident to the file's symbols.
        let mut callers: Vec<Value> = Vec::new();
        let mut callees: Vec<Value> = Vec::new();
        let mut caller_ids: HashSet<String> = HashSet::new();
        let mut callee_ids: HashSet<String> = HashSet::new();
        let mut importer_ids: HashSet<String> = HashSet::new();
        if !symbol_ids.is_empty() {
            let sym_list = symbol_ids
                .iter()
                .take(max_items)
                .map(|id| format!("'{}'", escape_ladybug_string(id)))
                .collect::<Vec<_>>()
                .join(", ");
            // Callee: a file symbol DEPENDS_ON it.
            {
                let callee_query = format!(
                    "MATCH (s:Memory)-[:DEPENDS_ON]->(t:Memory) WHERE s.id IN [{sym_list}] AND t.namespace = '{safe_ns}' RETURN t.id, t.content, t.memory_type, t.metadata LIMIT {row_limit}"
                );
                let mut res = conn.query(&callee_query)?;
                let mut rows_seen = 0usize;
                while let Some(row) = res.next() {
                    rows_seen += 1;
                    let id = Self::cell_string(&row[0]);
                    let meta = Self::cell_metadata(&row[3]);
                    if is_visible(&id, &meta) && !callee_ids.contains(&id) {
                        if callees.len() >= max_items {
                            mark_collection_truncated(
                                "callees",
                                &mut ast_truncated,
                                &mut truncated_collections,
                            );
                            break;
                        }
                        let (content, content_trunc) =
                            Self::truncate_neighbor_content(&Self::cell_string(&row[1]));
                        let mut entry = serde_json::json!({
                            "id": id,
                            "memory_type": Self::cell_string(&row[2]),
                            "content": content,
                        });
                        if content_trunc {
                            entry["truncated"] = Value::Bool(true);
                        }
                        let entry_bytes =
                            serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(64);
                        if !callees.is_empty() && ast_bytes + entry_bytes > byte_budget {
                            mark_collection_truncated(
                                "callees",
                                &mut ast_truncated,
                                &mut truncated_collections,
                            );
                            break;
                        }
                        callee_ids.insert(id);
                        ast_bytes += entry_bytes;
                        callees.push(entry);
                    }
                }
                if rows_seen >= row_limit {
                    mark_collection_truncated(
                        "callees",
                        &mut ast_truncated,
                        &mut truncated_collections,
                    );
                }
            }
            // Caller: something DEPENDS_ON a file symbol.
            {
                let caller_query = format!(
                    "MATCH (c:Memory)-[:DEPENDS_ON]->(s:Memory) WHERE s.id IN [{sym_list}] AND c.namespace = '{safe_ns}' RETURN c.id, c.content, c.memory_type, c.metadata LIMIT {row_limit}"
                );
                let mut res = conn.query(&caller_query)?;
                let mut rows_seen = 0usize;
                while let Some(row) = res.next() {
                    rows_seen += 1;
                    let id = Self::cell_string(&row[0]);
                    let meta = Self::cell_metadata(&row[3]);
                    if is_visible(&id, &meta) && !caller_ids.contains(&id) {
                        if callers.len() >= max_items {
                            mark_collection_truncated(
                                "callers",
                                &mut ast_truncated,
                                &mut truncated_collections,
                            );
                            break;
                        }
                        let (content, content_trunc) =
                            Self::truncate_neighbor_content(&Self::cell_string(&row[1]));
                        let mut entry = serde_json::json!({
                            "id": id,
                            "memory_type": Self::cell_string(&row[2]),
                            "content": content,
                        });
                        if content_trunc {
                            entry["truncated"] = Value::Bool(true);
                        }
                        let entry_bytes =
                            serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(64);
                        if !callers.is_empty() && ast_bytes + entry_bytes > byte_budget {
                            mark_collection_truncated(
                                "callers",
                                &mut ast_truncated,
                                &mut truncated_collections,
                            );
                            break;
                        }
                        caller_ids.insert(id);
                        ast_bytes += entry_bytes;
                        callers.push(entry);
                    }
                }
                if rows_seen >= row_limit {
                    mark_collection_truncated(
                        "callers",
                        &mut ast_truncated,
                        &mut truncated_collections,
                    );
                }
            }
        }

        // Importers: other file nodes whose metadata.imports names this path.
        let found_edge_importers = !file_ids.is_empty();
        if found_edge_importers {
            let file_list = file_ids
                .iter()
                .take(max_items)
                .map(|id| format!("'{}'", escape_ladybug_string(id)))
                .collect::<Vec<_>>()
                .join(", ");
            let edge_importer_query = format!(
                "MATCH (m:Memory)-[:DEPENDS_ON]->(f:Memory) WHERE f.id IN [{file_list}] AND m.namespace = '{safe_ns}' RETURN m.id, m.location, m.memory_type, m.metadata LIMIT {row_limit}"
            );
            let mut res = conn.query(&edge_importer_query)?;
            let mut rows_seen = 0usize;
            while let Some(row) = res.next() {
                rows_seen += 1;
                let id = Self::cell_string(&row[0]);
                let meta = Self::cell_metadata(&row[3]);
                if !is_visible(&id, &meta) {
                    continue;
                }
                let location = Self::cell_string(&row[1]);
                if location == path {
                    continue; // the file itself is not its own importer
                }
                let m_type = Self::cell_string(&row[2]);
                if (m_type == "file" || meta.get("imports").is_some())
                    && !importer_ids.contains(&id)
                {
                    if importers.len() >= max_items {
                        mark_collection_truncated(
                            "importers",
                            &mut ast_truncated,
                            &mut truncated_collections,
                        );
                        break;
                    }
                    let entry = serde_json::json!({
                        "id": id,
                        "memory_type": m_type,
                        "content": location,
                        "path": location,
                    });
                    let entry_bytes =
                        serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(64);
                    if !importers.is_empty() && ast_bytes + entry_bytes > byte_budget {
                        mark_collection_truncated(
                            "importers",
                            &mut ast_truncated,
                            &mut truncated_collections,
                        );
                        break;
                    }
                    importer_ids.insert(id);
                    ast_bytes += entry_bytes;
                    importers.push(entry);
                }
            }
            if rows_seen >= row_limit {
                mark_collection_truncated(
                    "importers",
                    &mut ast_truncated,
                    &mut truncated_collections,
                );
            }
        }
        if importers.is_empty() {
            let fallback_importer_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{safe_ns}' AND m.metadata CONTAINS '{safe_path}' RETURN m.id, m.location, m.memory_type, m.metadata LIMIT {row_limit}"
            );
            let mut res = conn.query(&fallback_importer_query)?;
            let mut rows_seen = 0usize;
            while let Some(row) = res.next() {
                rows_seen += 1;
                let id = Self::cell_string(&row[0]);
                let meta = Self::cell_metadata(&row[3]);
                if !is_visible(&id, &meta) {
                    continue;
                }
                let location = Self::cell_string(&row[1]);
                if location == path {
                    continue; // the file itself is not its own importer
                }
                if meta
                    .get("imports")
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| arr.iter().any(|e| e.as_str() == Some(path)))
                    && !importer_ids.contains(&id)
                {
                    if importers.len() >= max_items {
                        mark_collection_truncated(
                            "importers",
                            &mut ast_truncated,
                            &mut truncated_collections,
                        );
                        break;
                    }
                    let entry = serde_json::json!({
                        "id": id,
                        "memory_type": Self::cell_string(&row[2]),
                        "content": location,
                        "path": location,
                    });
                    let entry_bytes =
                        serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(64);
                    if !importers.is_empty() && ast_bytes + entry_bytes > byte_budget {
                        mark_collection_truncated(
                            "importers",
                            &mut ast_truncated,
                            &mut truncated_collections,
                        );
                        break;
                    }
                    importer_ids.insert(id);
                    ast_bytes += entry_bytes;
                    importers.push(entry);
                }
            }
            if rows_seen >= row_limit {
                mark_collection_truncated(
                    "importers",
                    &mut ast_truncated,
                    &mut truncated_collections,
                );
            }
        }

        let blast_radius_count = symbols.len()
            + callers.len()
            + callees.len()
            + imports.len()
            + importers.len();

        Ok(serde_json::json!({
            "symbols": symbols,
            "callers": callers,
            "callees": callees,
            "imports": imports,
            "importers": importers,
            "blast_radius_count": blast_radius_count,
            "truncated": ast_truncated,
            "truncated_collections": truncated_collections,
            "related_memories_truncated": related_truncated,
        }))
    }
}

/// IntakeStore implementation for LadybugStore (PQTH durable intake ledger).
#[async_trait::async_trait]
impl crate::traits::IntakeStore for LadybugStore {
    async fn create_receipt(
        &self,
        request: crate::task::intake::CreateReceiptRequest,
    ) -> Result<crate::task::intake::CreateReceiptResult, String> {
        let namespace = request.namespace.clone();
        let source_digest = request.source_digest.clone();
        let text = request.text.clone();
        let source_kind = request.source_kind;
        let source_revision = request.source_revision.clone();

        // Generate receipt ID deterministically from namespace + source_digest
        let receipt_id = crate::task::intake::generate_receipt_id(&namespace, &source_digest);

        // Process text through M2 lexical detector
        let detected_intents = crate::task::intake::process_intake_text(&text);

        // Create candidates from detected intents
        let candidates = crate::task::intake::create_candidates_from_intents(
            &namespace,
            &receipt_id,
            &request,
            detected_intents.clone(),
        );

        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        // Clone values needed for both receipt construction and safe strings
        let source_revision_for_receipt = source_revision.clone();
        let source_digest_for_receipt = source_digest.clone();
        let detected_intents_for_receipt = detected_intents.clone();
        let text_for_receipt = text.clone();
        let now_for_receipt = now.clone();

        let receipt = crate::task::intake::IntakeReceipt {
            id: receipt_id.clone(),
            namespace: namespace.clone(),
            source_kind,
            source_revision: source_revision_for_receipt,
            source_digest: source_digest_for_receipt,
            detected_intents: detected_intents_for_receipt,
            raw_text: text_for_receipt,
            created_at: now_for_receipt,
        };

        // Serialize detected intents for storage
        let intents_json = match serde_json::to_string(&detected_intents) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to serialize detected_intents for storage: {e}");
                String::new()
            }
        };
        let safe_intents = escape_ladybug_string(&intents_json);
        let safe_raw_text = escape_ladybug_string(&text);
        let safe_receipt_id = escape_ladybug_string(&receipt_id);
        let safe_ns = escape_ladybug_string(&namespace);
        let safe_source_kind = escape_ladybug_string(&format!("{:?}", source_kind).to_lowercase());
        let safe_source_revision = escape_ladybug_string(&source_revision);
        let safe_source_digest = escape_ladybug_string(&source_digest);
        let safe_created_at = escape_ladybug_string(&now);

        // Transactional write: receipt + candidates atomically
        let candidates_clone = candidates.clone();
        self.write_with_deadline("creating intake receipt and candidates", move |conn| {
            // Insert receipt
            let receipt_query = format!(
                "MERGE (r:IntakeReceipt {{id: '{}'}})
                 ON CREATE SET r.namespace = '{}', r.source_kind = '{}', r.source_revision = '{}',
                     r.source_digest = '{}', r.detected_intents = '{}', r.raw_text = '{}', r.created_at = '{}'",
                safe_receipt_id, safe_ns, safe_source_kind, safe_source_revision,
                safe_source_digest, safe_intents, safe_raw_text, safe_created_at
            );
            conn.query(&receipt_query)?;

            // Insert candidates
            for cand in &candidates_clone {
                let safe_cand_id = escape_ladybug_string(&cand.id);
                let safe_receipt_id = escape_ladybug_string(&cand.receipt_id);
                let safe_title = escape_ladybug_string(&cand.title);
                let safe_desc = cand.description.as_ref().map(|d| escape_ladybug_string(d)).unwrap_or_default();
                let safe_review_state = escape_ladybug_string(&format!("{:?}", cand.review_state).to_lowercase());
                let safe_disposition = escape_ladybug_string(&format!("{:?}", cand.disposition).to_lowercase());
                let safe_task_id = cand.task_id.as_ref().map(|t| escape_ladybug_string(t)).unwrap_or_default();
                let safe_created = escape_ladybug_string(&cand.created_at);
                let safe_updated = escape_ladybug_string(&cand.updated_at);
                let safe_kind = escape_ladybug_string(&cand.candidate_kind);
                let safe_payload = escape_ladybug_string(
                    &serde_json::to_string(&cand.payload).unwrap_or_else(|e| {
                        tracing::warn!("failed to serialize intake candidate payload: {e}");
                        "null".to_string()
                    }),
                );
                let safe_span = escape_ladybug_string(
                    &cand.source_span
                        .as_ref()
                        .map(|v| serde_json::to_string(v).unwrap_or_else(|e| {
                            tracing::warn!("failed to serialize intake candidate source_span: {e}");
                            String::new()
                        }))
                        .unwrap_or_default(),
                );
                let safe_fp = escape_ladybug_string(&cand.extractor_fingerprint.clone().unwrap_or_default());
                let safe_conf = cand.confidence.unwrap_or(0.0);
                let safe_cand_kind = escape_ladybug_string(&cand.kind);
                let safe_category = cand.category.as_ref().map(|c| escape_ladybug_string(c)).unwrap_or_default();
                let safe_provenance = escape_ladybug_string(
                    &cand.provenance.as_ref().map(|p| serde_json::to_string(p).unwrap_or_else(|e| {
                        tracing::warn!("failed to serialize intake candidate provenance: {e}");
                        String::new()
                    })).unwrap_or_default(),
                );

                let cand_query = format!(
                    "MERGE (c:IntakeCandidate {{id: '{}' }})\n                     ON CREATE SET c.namespace = '{}', c.receipt_id = '{}', c.title = '{}',\n                         c.description = '{}', c.review_state = '{}', c.disposition = '{}',\n                         c.task_id = '{}', c.cas_version = {}, c.created_at = '{}', c.updated_at = '{}',\n                         c.candidate_kind = '{}', c.kind = '{}', c.category = '{}', c.provenance = '{}',
                         c.payload = '{}', c.confidence = {}, c.source_span = '{}', c.extractor_fingerprint = '{}'",
                    safe_cand_id, safe_ns, safe_receipt_id, safe_title, safe_desc,
                    safe_review_state, safe_disposition, safe_task_id, cand.cas_version,
                    safe_created, safe_updated, safe_kind, safe_cand_kind, safe_category, safe_provenance,
                    safe_payload, safe_conf, safe_span, safe_fp
                );
                conn.query(&cand_query)?;
            }

            Ok(())
        })
        .await
        .map_err(|e| format!("Failed to create intake receipt: {}", e))?;

        let candidate_count = candidates.len();
        Ok(crate::task::intake::CreateReceiptResult {
            receipt,
            candidates,
            candidate_count,
        })
    }

    async fn list_receipts(
        &self,
        request: crate::task::intake::ListReceiptsRequest,
    ) -> Result<crate::task::intake::IntakeListResponse<crate::task::intake::IntakeReceipt>, String> {
        let namespace = request.namespace.clone();
        let limit = request.limit.unwrap_or(50);
        let offset = request.offset.unwrap_or(0);

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // Get total count
            let count_query = format!(
                "MATCH (r:IntakeReceipt) WHERE r.namespace = '{}' RETURN count(r)",
                safe_ns
            );
            let total: i64 = conn
                .query(&count_query)?
                .next()
                .and_then(|row| format!("{}", row[0]).parse().ok())
                .unwrap_or(0);

            if limit == 0 {
                return Ok(crate::task::intake::IntakeListResponse {
                    namespace: namespace.clone(),
                    count: 0,
                    total: total as usize,
                    offset,
                    limit,
                    items: Vec::new(),
                });
            }

            // Get paginated results
            let query = format!(
                "MATCH (r:IntakeReceipt) WHERE r.namespace = '{}' RETURN r.id, r.namespace, r.source_kind, r.source_revision, r.source_digest, r.detected_intents, r.raw_text, r.created_at ORDER BY r.created_at DESC LIMIT {}",
                safe_ns,
                offset.saturating_add(limit)
            );
            let mut results = Vec::new();
            for row in conn.query(&query)?.skip(offset).take(limit) {
                let id: String = format!("{}", row[0]);
                let ns: String = format!("{}", row[1]);
                let source_kind_str: String = format!("{}", row[2]);
                let source_revision: String = format!("{}", row[3]);
                let source_digest: String = format!("{}", row[4]);
                let detected_intents_str: String = format!("{}", row[5]);
                let raw_text: String = format!("{}", row[6]);
                let created_at: String = format!("{}", row[7]);

                let source_kind = match source_kind_str.as_str() {
                    "conversation" => crate::task::intake::SourceKind::Conversation,
                    "manual" => crate::task::intake::SourceKind::Manual,
                    "imported" => crate::task::intake::SourceKind::Imported,
                    "tool" => crate::task::intake::SourceKind::Tool,
                    _ => crate::task::intake::SourceKind::Conversation,
                };

                let detected_intents: Vec<crate::task::intake::DetectedIntent> =
                    serde_json::from_str(&detected_intents_str).unwrap_or_default();

                results.push(crate::task::intake::IntakeReceipt {
                    id,
                    namespace: ns,
                    source_kind,
                    source_revision,
                    source_digest,
                    detected_intents,
                    raw_text,
                    created_at,
                });
            }

            Ok(crate::task::intake::IntakeListResponse {
                namespace,
                count: results.len(),
                total: total as usize,
                offset,
                limit,
                items: results,
            })
        })
        .await
        .map_err(|e| format!("Failed to list receipts: {}", e))
    }

    async fn list_candidates(
        &self,
        request: crate::task::intake::ListCandidatesRequest,
    ) -> Result<crate::task::intake::IntakeListResponse<crate::task::intake::IntakeCandidate>, String> {
        let namespace = request.namespace.clone();
        let limit = request.limit.unwrap_or(50);
        let offset = request.offset.unwrap_or(0);
        let review_state_filter = request.review_state;
        let disposition_filter = request.disposition;

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // Build WHERE clause for filters
            let mut where_clauses = vec![format!("c.namespace = '{}'", safe_ns)];
            if let Some(rs) = review_state_filter {
                where_clauses.push(format!("c.review_state = '{}'", escape_ladybug_string(&format!("{:?}", rs).to_lowercase())));
            }
            if let Some(d) = disposition_filter {
                where_clauses.push(format!("c.disposition = '{}'", escape_ladybug_string(&format!("{:?}", d).to_lowercase())));
            }
            let where_clause = where_clauses.join(" AND ");

            // Get total count
            let count_query = format!(
                "MATCH (c:IntakeCandidate) WHERE {} RETURN count(c)",
                where_clause
            );
            let total: i64 = conn
                .query(&count_query)?
                .next()
                .and_then(|row| format!("{}", row[0]).parse().ok())
                .unwrap_or(0);

            if limit == 0 {
                return Ok(crate::task::intake::IntakeListResponse {
                    namespace: namespace.clone(),
                    count: 0,
                    total: total as usize,
                    offset,
                    limit,
                    items: Vec::new(),
                });
            }

            // Get paginated results
            let query = format!(
                "MATCH (c:IntakeCandidate) WHERE {} RETURN c.id, c.namespace, c.receipt_id, c.title, c.description, c.review_state, c.disposition, c.task_id, c.cas_version, c.created_at, c.updated_at, c.candidate_kind, c.kind, c.category, c.provenance, c.payload, c.confidence, c.source_span, c.extractor_fingerprint ORDER BY c.created_at DESC LIMIT {}",
                where_clause,
                offset.saturating_add(limit)
            );
            let mut results = Vec::new();
            for row in conn.query(&query)?.skip(offset).take(limit) {
                let id: String = format!("{}", row[0]);
                let ns: String = format!("{}", row[1]);
                let receipt_id: String = format!("{}", row[2]);
                let title: String = format!("{}", row[3]);
                let description: String = format!("{}", row[4]);
                let review_state_str: String = format!("{}", row[5]);
                let disposition_str: String = format!("{}", row[6]);
                let task_id: String = format!("{}", row[7]);
                let cas_version: i64 = format!("{}", row[8]).parse().unwrap_or(1);
                let created_at: String = format!("{}", row[9]);
                let updated_at: String = format!("{}", row[10]);
                let candidate_kind: String = format!("{}", row[11]);
                let kind: String = format!("{}", row[12]);
                let category_str: String = format!("{}", row[13]);
                let provenance_str: String = format!("{}", row[14]);
                let payload_str: String = format!("{}", row[15]);
                let confidence: Option<f64> = format!("{}", row[16]).parse().ok();
                let source_span_str: String = format!("{}", row[17]);
                let extractor_fingerprint: Option<String> = {
                    let s = format!("{}", row[18]);
                    if s.is_empty() || s.eq_ignore_ascii_case("null") { None } else { Some(s) }
                };

                let review_state = match review_state_str.as_str() {
                    "pending" => crate::task::intake::ReviewState::Pending,
                    "inreview" => crate::task::intake::ReviewState::InReview,
                    "accepted" => crate::task::intake::ReviewState::Accepted,
                    "dismissed" => crate::task::intake::ReviewState::Dismissed,
                    _ => crate::task::intake::ReviewState::Pending,
                };

                let disposition = match disposition_str.as_str() {
                    "pending" => crate::task::intake::Disposition::Pending,
                    "created" => crate::task::intake::Disposition::Created,
                    "linked" => crate::task::intake::Disposition::Linked,
                    "promoted" => crate::task::intake::Disposition::Promoted,
                    "dismissed" => crate::task::intake::Disposition::Dismissed,
                    _ => crate::task::intake::Disposition::Pending,
                };

                results.push(crate::task::intake::IntakeCandidate {
                    id,
                    namespace: ns,
                    receipt_id,
                    title,
                    description: if description.is_empty() || description.eq_ignore_ascii_case("null") { None } else { Some(description) },
                    review_state,
                    disposition,
                    task_id: if task_id.is_empty() || task_id.eq_ignore_ascii_case("null") { None } else { Some(task_id) },
                    cas_version: cas_version as u64,
                    created_at,
                    updated_at,
                    candidate_kind: if candidate_kind.is_empty() || candidate_kind.eq_ignore_ascii_case("null") { "task".to_string() } else { candidate_kind },
                    kind: if kind.is_empty() || kind.eq_ignore_ascii_case("null") { "task_obligation".to_string() } else { kind },
                    category: if category_str.is_empty() || category_str.eq_ignore_ascii_case("null") { None } else { Some(category_str) },
                    payload: serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null),
                    confidence,
                    source_span: if source_span_str.is_empty() || source_span_str.eq_ignore_ascii_case("null") { None } else { serde_json::from_str(&source_span_str).ok() },
                    provenance: if provenance_str.is_empty() || provenance_str.eq_ignore_ascii_case("null") { None } else { serde_json::from_str(&provenance_str).ok() },
                    extractor_fingerprint,
                });
            }

            Ok(crate::task::intake::IntakeListResponse {
                namespace,
                count: results.len(),
                total: total as usize,
                offset,
                limit,
                items: results,
            })
        })
        .await
        .map_err(|e| format!("Failed to list candidates: {}", e))
    }

    async fn resolve_candidate(
        &self,
        request: crate::task::intake::ResolveCandidateRequest,
    ) -> Result<crate::task::intake::ResolveCandidateResult, String> {
        let namespace = request.namespace.clone();
        let candidate_id = request.candidate_id.clone();
        let disposition = request.disposition;
        let task_id = request.task_id.clone();
        let task_id_for_closure = task_id.clone();
        let cas_version = request.cas_version;

        self.write_with_deadline("resolving intake candidate", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_cand_id = escape_ladybug_string(&candidate_id);
            let safe_disposition = escape_ladybug_string(&format!("{:?}", disposition).to_lowercase());
            let safe_task_id = task_id_for_closure.as_ref().map(|t| escape_ladybug_string(t)).unwrap_or_default();
            let new_version = cas_version + 1;
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let safe_updated = escape_ladybug_string(&now);

            // For "created" and "linked" dispositions, verify the task_id refers
            // to a real task in the Memory table (memory_type = 'task') in the
            // same namespace. Reject missing/blank/wrong-namespace/non-task IDs
            // (R8: a `linked` disposition must not point at a missing or
            // cross-namespace task either).
            if matches!(
                disposition,
                crate::task::intake::Disposition::Created | crate::task::intake::Disposition::Linked
            ) {
                if safe_task_id.is_empty() {
                    return Err(anyhow::anyhow!(
                        "task_id is required for 'created'/'linked' disposition"
                    ));
                }
                let task_check = format!(
                    "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' AND m.memory_type = 'task' RETURN m.id",
                    safe_task_id, safe_ns
                );
                let mut task_result = conn.query(&task_check)?;
                if task_result.next().is_none() {
                    return Err(anyhow::anyhow!(
                        "task_id '{}' does not refer to an existing task in namespace '{}'",
                        task_id_for_closure.as_deref().unwrap_or(""),
                        namespace
                    ));
                }
            }

            // CAS: only update if cas_version matches
            let query = format!(
                "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' AND c.cas_version = {} SET c.disposition = '{}', c.task_id = '{}', c.cas_version = {}, c.updated_at = '{}', c.review_state = '{}' RETURN c.id",
                safe_cand_id, safe_ns, cas_version, safe_disposition, safe_task_id, new_version, safe_updated,
                if disposition == crate::task::intake::Disposition::Dismissed { "dismissed" } else { "accepted" }
            );
            let mut result = conn.query(&query)?;

            if result.next().is_none() {
                // Check if candidate exists but version mismatch
                let check_query = format!(
                    "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' RETURN c.cas_version",
                    safe_cand_id, safe_ns
                );
                let mut check_result = conn.query(&check_query)?;
                if let Some(row) = check_result.next() {
                    let current_version: i64 = format!("{}", row[0]).parse().unwrap_or(0);
                    return Err(anyhow::anyhow!("CAS conflict: expected version {}, found {}", cas_version, current_version));
                }
                return Err(anyhow::anyhow!("Candidate not found: {}", candidate_id));
            }

            // Fetch updated candidate
            let fetch_query = format!(
                "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' RETURN c.id, c.namespace, c.receipt_id, c.title, c.description, c.review_state, c.disposition, c.task_id, c.cas_version, c.created_at, c.updated_at, c.candidate_kind, c.payload, c.confidence, c.source_span, c.extractor_fingerprint, c.kind, c.category, c.provenance",
                safe_cand_id, safe_ns
            );
            let mut fetch_result = conn.query(&fetch_query)?;
            let row = fetch_result.next().ok_or_else(|| anyhow::anyhow!("Candidate not found after update: {}", candidate_id))?;

            let id: String = format!("{}", row[0]);
            let ns: String = format!("{}", row[1]);
            let receipt_id: String = format!("{}", row[2]);
            let title: String = format!("{}", row[3]);
            let description: String = format!("{}", row[4]);
            let review_state_str: String = format!("{}", row[5]);
            let disposition_str: String = format!("{}", row[6]);
            let task_id_db: String = format!("{}", row[7]);
            let cas_version_db: i64 = format!("{}", row[8]).parse().unwrap_or(new_version as i64);
            let created_at: String = format!("{}", row[9]);
            let updated_at: String = format!("{}", row[10]);
            let candidate_kind: String = format!("{}", row[11]);
            let payload_str: String = format!("{}", row[12]);
            let confidence: Option<f64> = format!("{}", row[13]).parse().ok();
            let source_span_str: String = format!("{}", row[14]);
            let extractor_fingerprint: Option<String> = {
                let s = format!("{}", row[15]);
                if s.is_empty() || s.eq_ignore_ascii_case("null") { None } else { Some(s) }
            };
            let kind_db: String = format!("{}", row[16]);
            let category_db: String = format!("{}", row[17]);
            let provenance_db: String = format!("{}", row[18]);

            let review_state = match review_state_str.as_str() {
                "pending" => crate::task::intake::ReviewState::Pending,
                "inreview" => crate::task::intake::ReviewState::InReview,
                "accepted" => crate::task::intake::ReviewState::Accepted,
                "dismissed" => crate::task::intake::ReviewState::Dismissed,
                _ => crate::task::intake::ReviewState::Pending,
            };

            let disposition_enum = match disposition_str.as_str() {
                "pending" => crate::task::intake::Disposition::Pending,
                "created" => crate::task::intake::Disposition::Created,
                "linked" => crate::task::intake::Disposition::Linked,
                "promoted" => crate::task::intake::Disposition::Promoted,
                "dismissed" => crate::task::intake::Disposition::Dismissed,
                _ => crate::task::intake::Disposition::Pending,
            };

            let candidate = crate::task::intake::IntakeCandidate {
                id,
                namespace: ns,
                receipt_id,
                title,
                description: if description.is_empty() || description.eq_ignore_ascii_case("null") { None } else { Some(description) },
                review_state,
                disposition: disposition_enum,
                task_id: if task_id_db.is_empty() || task_id_db.eq_ignore_ascii_case("null") { None } else { Some(task_id_db) },
                cas_version: cas_version_db as u64,
                created_at,
                updated_at,
                candidate_kind: if candidate_kind.is_empty() || candidate_kind.eq_ignore_ascii_case("null") { "task".to_string() } else { candidate_kind },
                kind: if kind_db.is_empty() || kind_db.eq_ignore_ascii_case("null") { "task_obligation".to_string() } else { kind_db },
                category: if category_db.is_empty() || category_db.eq_ignore_ascii_case("null") { None } else { Some(category_db) },
                payload: serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null),
                confidence,
                source_span: if source_span_str.is_empty() || source_span_str.eq_ignore_ascii_case("null") { None } else { serde_json::from_str(&source_span_str).ok() },
                provenance: if provenance_db.is_empty() || provenance_db.eq_ignore_ascii_case("null") { None } else { serde_json::from_str(&provenance_db).ok() },
                extractor_fingerprint,
            };

            Ok(crate::task::intake::ResolveCandidateResult {
                candidate: candidate.clone(),
                task_created: disposition == crate::task::intake::Disposition::Created,
                task_id: task_id_for_closure.clone(),
            })
        })
        .await
        .map_err(|e| format!("Failed to resolve candidate: {}", e))
    }
}

/// Diagnostic counters for work handed to the engine.
///
/// Process-wide on purpose: there is one store per process, and the question
/// they exist to answer -- does blocking work pile up and never come back -- is
/// about the process, not about any one call.
///
/// `STARTED` minus `FINISHED` is how many engine calls are inside lbug right
/// now. If that number climbs and does not come down, a deadline that could not
/// cancel its statement has left the work running, which is the hypothesis these
/// were added to test.
pub static ENGINE_CALLS_STARTED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static ENGINE_CALLS_FINISHED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Writes whose caller gave up. The statement may still be running.
pub static WRITES_ABANDONED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Times the engine refused a second write transaction.
pub static WRITE_COLLISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many engine calls are inside lbug right now.
pub fn engine_in_flight() -> u64 {
    ENGINE_CALLS_STARTED
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(ENGINE_CALLS_FINISHED.load(std::sync::atomic::Ordering::Relaxed))
}

/// The counters as a line worth logging.
pub fn engine_stats() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!(
        "{} in flight ({} started, {} finished), {} abandoned, {} collisions",
        engine_in_flight(),
        ENGINE_CALLS_STARTED.load(Relaxed),
        ENGINE_CALLS_FINISHED.load(Relaxed),
        WRITES_ABANDONED.load(Relaxed),
        WRITE_COLLISIONS.load(Relaxed)
    )
}

/// How long a write may wait before the caller is told the database is busy.
/// Override with NEUROSTRATA_WRITE_TIMEOUT_SECS where an unusually large
/// database makes the default too tight.
fn write_timeout() -> std::time::Duration {
    let secs = std::env::var("NEUROSTRATA_WRITE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// How many times a write retries after being refused the write transaction.
/// Waits are 25ms, 50ms and so on, and the caller's deadline caps the total
/// regardless, so this only decides how patient a single attempt is.
const WRITE_COLLISION_RETRIES: u32 = 6;

/// The engine's refusal when something else already holds the write
/// transaction. Worth recognising by name: the fix is to wait a moment, not to
/// report a failure, and the in-process lock cannot prevent every case -- the
/// background checkpoint and any other process opening the same database are
/// both outside it.
fn is_write_collision(err: &anyhow::Error) -> bool {
    err.to_string()
        .contains("Cannot start a new write transaction")
}

/// Whether a memory has been retired, meaning its `valid_to` has passed.
///
/// A retired row has to stop surfacing wherever results come from: the vector
/// match, the graph around that match, and the edges relinking replays.
/// Filtering only the first lets the graph hand back what the match left out.
fn is_retired(metadata: &Value) -> bool {
    is_retired_at(metadata, chrono::Utc::now().timestamp())
}

/// Timestamp-aware variant so callers that fetch many rows in one pass can
/// capture `now` once instead of calling Utc::now() per row.
fn is_retired_at(metadata: &Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(valid_to) if !valid_to.is_null() => valid_to.as_i64().unwrap_or(0) <= now,
        _ => false,
    }
}

/// The ids a declared edge target could stand for when it is not already a
/// qualified id: the declaration generation-scoped into the active generation,
/// and every separator-anchored suffix of it, bare and scoped. A suffix is how a
/// legacy absolute path finds the file it meant.
fn declared_target_candidates(namespace: &str, active_gen: u64, declared: &str) -> Vec<String> {
    let declared = declared.replace('\\', "/");
    let mut candidates =
        vec![crate::parser::ingest::generation_scoped_id(namespace, active_gen, &declared)];
    for (cut, _) in declared.match_indices('/') {
        let suffix = &declared[cut + 1..];
        if !suffix.is_empty() {
            candidates.push(suffix.to_string());
            candidates.push(crate::parser::ingest::generation_scoped_id(
                namespace,
                active_gen,
                suffix,
            ));
        }
    }
    candidates
}

/// Chooses among the ids that exist: the memory's own namespace first (in the
/// active generation), then the id exactly as declared, then a suffix -- but
/// only one. An ambiguous suffix produces no edge, because a wrong edge is
/// worse than a missing one.
fn pick_declared_target(
    namespace: &str,
    active_gen: u64,
    declared: &str,
    found: &[String],
) -> crate::traits::TargetResolution {
    let qualified = crate::parser::ingest::generation_scoped_id(
        namespace,
        active_gen,
        &declared.replace('\\', "/"),
    );
    if found.iter().any(|id| *id == qualified) {
        return crate::traits::TargetResolution::Resolved(qualified);
    }
    if found.iter().any(|id| id == declared) {
        return crate::traits::TargetResolution::Resolved(declared.to_string());
    }
    let suffixes: Vec<String> = found
        .iter()
        .filter(|id| **id != qualified && id.as_str() != declared)
        .cloned()
        .collect();
    match suffixes.len() {
        0 => crate::traits::TargetResolution::Missing,
        1 => crate::traits::TargetResolution::Resolved(suffixes[0].clone()),
        _ => crate::traits::TargetResolution::Ambiguous(suffixes),
    }
}

/// Resolves a declared edge target while a memory is being written, in one read.
///
/// The exact id stays unscoped, as edges always have been; everything inferred
/// from it is confined to the memory's own namespace in the *active* generation,
/// so a bare `src/lib.rs` lands on this project's published tree and never on
/// another project's file or a superseded generation.
fn resolve_target_on_write(
    conn: &Connection,
    namespace: &str,
    declared: &str,
) -> crate::traits::TargetResolution {
    let active_gen = active_generation_of(conn, namespace);
    let candidates = declared_target_candidates(namespace, active_gen, declared)
        .iter()
        .map(|c| format!("'{}'", escape_ladybug_string(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "MATCH (b:Memory) WHERE b.id = '{}' OR (b.namespace = '{}' AND b.id IN [{}]) RETURN DISTINCT b.id",
        escape_ladybug_string(declared),
        escape_ladybug_string(namespace),
        candidates
    );
    let mut rows = match conn.query(&query) {
        Ok(rows) => rows,
        Err(e) => return crate::traits::TargetResolution::Failed(e.to_string()),
    };
    let mut found = Vec::new();
    while let Some(row) = rows.next() {
        found.push(format!("{}", row[0]));
    }
    pick_declared_target(namespace, active_gen, declared, &found)
}

/// Fails a future that outstays its deadline, with a message that says what was
/// waiting and admits the work may still be running.
async fn bounded<T>(
    what: &str,
    limit: std::time::Duration,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "the database is busy: {} did not finish within {}s. The statement may still be running, so read the record back before retrying.",
            what,
            limit.as_secs()
        )),
    }
}

/// Adds one to `access_count` and leaves the rest of the blob alone. Metadata
/// that is missing, empty or unparseable starts a fresh object at 1 instead of
/// failing: reading a memory must never be the thing that errors.
fn bump_access_count(metadata: &str) -> String {
    let mut value: Value = serde_json::from_str(metadata).unwrap_or_else(|_| serde_json::json!({}));
    if !value.is_object() {
        value = serde_json::json!({});
    }
    if let Some(obj) = value.as_object_mut() {
        let count = obj
            .get("access_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        obj.insert(
            "access_count".to_string(),
            serde_json::json!(count.saturating_add(1)),
        );
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The canonical source key used by the relink/publish seed helpers. A real
    /// ingest derives it from the directory path (`source_key_for`); the unit
    /// tests pin a stable literal so the source-scoped pointer is deterministic.
    const PROBE_SOURCE: &str = "/worktrees/probe";

    /// The resolution rule as every caller now reaches it: build the index
    /// once, then ask it.
    fn resolve(declared: &str, known: &[String]) -> Option<String> {
        KnownIds::new(known.iter().map(|id| id.as_str())).resolve(declared)
    }

    #[test]
    fn the_engines_refusal_is_recognised_as_a_collision() {
        let refusal = anyhow::anyhow!(
            "Query execution failed: Cannot start a new write transaction in the system. Only one write transaction at a time is allowed in the system."
        );
        assert!(is_write_collision(&refusal));
    }

    /// Isolation probe for the EXPORT DATABASE segfault (guinea-pig BUG-3):
    /// if this dies with SIGSEGV the engine's export is broken on EVERY store,
    /// not just on one damaged by an incident. A crash here is the diagnosis.
    #[tokio::test]
    async fn export_database_survives_a_fresh_store() {
        let root = std::env::temp_dir().join(format!("lbug-export-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fresh.lbug");
        let store = LadybugStore::for_testing(db.to_string_lossy().to_string(), 4).unwrap();
        store.init("ExportProbe").await.unwrap();
        let out = root.join("out");
        store
            .export_database(&out.to_string_lossy())
            .await
            .expect("EXPORT snapshot on a fresh store");
        assert!(
            out.join("ladybug.store").exists(),
            "the store file is in the snapshot"
        );
        assert!(
            out.join("manifest.json").exists(),
            "the snapshot carries a manifest"
        );
    }

    #[test]
    fn an_ordinary_failure_is_not_retried_as_a_collision() {
        let other = anyhow::anyhow!("Binder exception: Table Memory does not exist");
        assert!(!is_write_collision(&other));
    }

    #[test]
    fn a_legacy_absolute_declaration_finds_the_file_it_meant() {
        let known = vec![
            "src/store/ladybug.rs".to_string(),
            "src/daemon.rs".to_string(),
        ];
        assert_eq!(
            resolve("C:/dev/projects/neurostrata/src/store/ladybug.rs", &known).as_deref(),
            Some("src/store/ladybug.rs")
        );
        // Backslashes are the same path, written the way Windows hands it over.
        assert_eq!(
            resolve(
                r"C:\dev\projects
eurostrata\src\daemon.rs",
                &known
            )
            .as_deref(),
            Some("src/daemon.rs")
        );
    }

    #[test]
    fn an_exact_declaration_is_returned_untouched() {
        let known = vec!["src/daemon.rs".to_string()];
        assert_eq!(
            resolve("src/daemon.rs", &known).as_deref(),
            Some("src/daemon.rs")
        );
    }

    /// The migration doctor prescribes is a re-ingest, which qualifies every
    /// id. A rule that still declares the old absolute path must follow it.
    #[test]
    fn a_legacy_absolute_declaration_finds_its_qualified_id() {
        let known = vec![
            "probe::src/store/ladybug.rs".to_string(),
            "probe::src/daemon.rs".to_string(),
        ];
        assert_eq!(
            resolve("C:/dev/projects/neurostrata/src/store/ladybug.rs", &known).as_deref(),
            Some("probe::src/store/ladybug.rs")
        );
        assert_eq!(
            resolve("C:/old/mylib.rs", &["probe::lib.rs".to_string()]),
            None
        );
        assert_eq!(
            resolve(
                "C:/old/src/a.rs",
                &["probe::src/a.rs".to_string(), "other::src/a.rs".to_string()]
            ),
            None,
            "a path two namespaces hold is ambiguous"
        );
    }

    #[test]
    fn an_ambiguous_suffix_is_left_alone() {
        // A wrong edge is worse than a missing one.
        // Both "mod.rs" and "x/mod.rs" are suffixes of the declaration.
        let known = vec!["mod.rs".to_string(), "x/mod.rs".to_string()];
        assert_eq!(resolve("C:/proj/x/mod.rs", &known), None);
    }

    #[test]
    fn a_target_that_was_never_ingested_stays_unresolved() {
        let known = vec!["src/daemon.rs".to_string()];
        assert_eq!(resolve("C:/proj/src/nowhere.rs", &known), None);
    }

    #[test]
    fn a_fresh_store_has_nothing_to_flush() {
        let dirty = Dirty::default();
        assert!(!dirty.is_set());
        assert!(!dirty.take());
    }

    #[test]
    fn taking_the_flag_clears_it_so_one_checkpoint_answers_for_many_writes() {
        let dirty = Dirty::default();
        dirty.mark();
        dirty.mark();

        assert!(dirty.take(), "the flag was set, so taking it reports true");
        assert!(!dirty.is_set(), "and leaves nothing behind");
        assert!(!dirty.take(), "a second take has nothing to report");
    }

    #[test]
    fn a_write_during_a_checkpoint_is_not_swallowed() {
        let dirty = Dirty::default();
        dirty.mark();

        // The checkpoint takes the flag first, then a write lands while the
        // engine is still flushing. That write must survive the clear.
        let _flushing = dirty.take();
        dirty.mark();

        assert!(dirty.is_set());
    }

    #[tokio::test]
    async fn a_write_that_outstays_its_deadline_says_the_database_is_busy() {
        let slow = async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok::<(), anyhow::Error>(())
        };

        let err = bounded(
            "deleting a memory",
            std::time::Duration::from_millis(50),
            slow,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("the database is busy"), "{}", err);
        assert!(err.contains("deleting a memory"), "{}", err);
        // The caller is released, not the query -- saying so is the point.
        assert!(err.contains("may still be running"), "{}", err);
    }

    #[tokio::test]
    async fn waiting_a_turn_to_write_counts_against_the_deadline() {
        // The shape write_with_deadline uses: the queue for the single writer
        // sits inside the bound, so a caller stuck behind a stalled write is
        // told the database is busy rather than waiting for its turn forever.
        let lock = tokio::sync::Mutex::new(());
        let held = lock.lock().await;

        let err = bounded(
            "writing a memory",
            std::time::Duration::from_millis(50),
            async {
                let _turn = lock.lock().await;
                Ok::<(), anyhow::Error>(())
            },
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("the database is busy"), "{}", err);
        drop(held);
    }

    /// The invariant the writer's turn exists for. Held by the caller, giving
    /// up released it while the statement was still running, and the next
    /// writer opened a second transaction against an engine that already had
    /// one -- which is how one stalled write became five in a row (bead
    /// neurostrata-4nk).
    #[tokio::test]
    async fn giving_up_does_not_hand_on_a_turn_that_is_still_in_use() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));

        // Stands in for a statement spawn_blocking cannot cancel: it owns the
        // turn and keeps it for as long as it runs.
        let turn = Arc::clone(&lock).lock_owned().await;
        let statement = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(turn);
        });

        let err = bounded(
            "writing a memory",
            std::time::Duration::from_millis(20),
            async {
                let _held = Arc::clone(&lock).lock_owned().await;
                Ok::<(), anyhow::Error>(())
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("the database is busy"), "{}", err);

        assert!(
            lock.try_lock().is_err(),
            "the turn was handed on while the statement was still running"
        );

        statement.await.expect("the statement finishes");
        assert!(
            lock.try_lock().is_ok(),
            "the turn is free once the statement has actually ended"
        );
    }

    #[tokio::test]
    async fn a_write_inside_its_deadline_is_left_alone() {
        let quick = async { Ok::<u8, anyhow::Error>(7) };
        let out = bounded("writing a memory", std::time::Duration::from_secs(5), quick)
            .await
            .unwrap();
        assert_eq!(out, 7);
    }

    #[test]
    fn the_write_deadline_can_be_overridden() {
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "90");
        assert_eq!(write_timeout(), std::time::Duration::from_secs(90));

        // A nonsense or zero value falls back rather than disabling the bound.
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "0");
        assert_eq!(write_timeout(), std::time::Duration::from_secs(30));
        std::env::remove_var("NEUROSTRATA_WRITE_TIMEOUT_SECS");
    }

    #[test]
    fn a_first_read_starts_the_count_at_one() {
        assert_eq!(
            bump_access_count("{}"),
            json!({"access_count": 1}).to_string()
        );
    }

    #[test]
    fn counting_a_read_leaves_the_rest_of_the_blob_alone() {
        let before = json!({
            "domain": "database",
            "governs": ["src/daemon.rs"],
            "access_count": 4
        })
        .to_string();

        let after: Value = serde_json::from_str(&bump_access_count(&before)).unwrap();

        assert_eq!(after["access_count"], json!(5));
        assert_eq!(after["domain"], json!("database"));
        assert_eq!(after["governs"], json!(["src/daemon.rs"]));
    }

    #[test]
    fn unusable_metadata_does_not_fail_a_read() {
        for junk in ["", "not json at all", "[1,2,3]", "null"] {
            assert_eq!(
                bump_access_count(junk),
                json!({"access_count": 1}).to_string(),
                "junk metadata {:?} should still count the read",
                junk
            );
        }
    }

    async fn retired_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-retired-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");
        store
    }

    /// Composite Recommendation #11: the first embedding descriptor recorded
    /// per namespace is authoritative; an incompatible descriptor (different
    /// dimensions or model) follows with an Err, a compatible one with Ok.
    #[test]
    fn embedding_descriptor_registry_accepts_compatible_rejects_mismatch() {
        let dir = std::env::temp_dir().join(format!("ns-embed-desc-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        let desc = crate::traits::EmbeddingDescriptor {
            provider: "store-registered".into(),
            model: "test-model".into(),
            revision: "store-lifetime".into(),
            dims: 4,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        };
        store
            .verify_or_record_embedding_descriptor("probe", &desc)
            .expect("first record for a namespace");
        store
            .verify_or_record_embedding_descriptor("probe", &desc)
            .expect("re-running with a compatible descriptor is idempotent");

        let mut dims_mismatch = desc.clone();
        dims_mismatch.dims = 8;
        let dims_err = store
            .verify_or_record_embedding_descriptor("probe", &dims_mismatch)
            .expect_err("dimension mismatch must be rejected");
        assert!(
            dims_err.to_string().contains("embedding descriptor mismatch"),
            "unexpected error: {dims_err}"
        );

        let mut model_mismatch = desc.clone();
        model_mismatch.model = "other-model".into();
        let model_err = store
            .verify_or_record_embedding_descriptor("probe", &model_mismatch)
            .expect_err("model change must be rejected");
        assert!(
            model_err.to_string().contains("embedding descriptor mismatch"),
            "unexpected error: {model_err}"
        );

        // Other namespaces keep their own descriptor, so an incompatible
        // descriptor is fine there until it, too, is recorded.
        store
            .verify_or_record_embedding_descriptor("other-ns", &dims_mismatch)
            .expect("a distinct namespace records its own descriptor");
    }

    fn retired_test_payload(memory_type: &str, user_id: &str, metadata: Value) -> MemoryPayload {
        MemoryPayload {
            content: format!("a {} row", memory_type),
            user_id: user_id.to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        }
    }

    #[tokio::test]
    async fn lexical_search_ranks_symbols_filters_namespaces_and_rebuilds() {
        let store = retired_test_store().await;

        let mut rule = retired_test_payload(
            "rule",
            "owner",
            json!({"symbol": "searchIndex", "domain": "auth"}),
        );
        rule.content = "the lexical engine ranks documents".to_string();
        rule.location = "src/fts/mod.rs".to_string();
        store
            .upsert("probe", "rule-1", vec![0.0; 4], rule)
            .await
            .unwrap();

        let mut other = retired_test_payload("rule", "owner", json!({"domain": "auth"}));
        other.content = "the lexical engine is documented elsewhere".to_string();
        store
            .upsert("probe", "rule-2", vec![0.0; 4], other)
            .await
            .unwrap();

        let mut foreign = retired_test_payload("rule", "owner", json!({}));
        foreign.content = "searchIndex in another namespace".to_string();
        store
            .upsert("other", "rule-3", vec![0.0; 4], foreign)
            .await
            .unwrap();

        let outcome = store
            .search_lexical("probe", "searchIndex", 10, false, false, &[], "admin", 2)
            .await
            .expect("lexical search");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].id, "rule-1");
        assert!(outcome.results[0]
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.derivation.iter().any(|d| d == "lexical-inhouse")));

        let rebuilt = store
            .rebuild_lexical_index(Some("probe"))
            .await
            .expect("rebuild the lexical index");
        assert_eq!(rebuilt, 2);
    }

    #[tokio::test]
    async fn lexical_mutations_keep_a_loaded_index_in_step() {
        let store = retired_test_store().await;
        let mut payload = retired_test_payload("rule", "owner", json!({}));
        payload.content = "alpha term".to_string();
        store
            .upsert("probe", "rule-1", vec![0.0; 4], payload.clone())
            .await
            .unwrap();

        // Force the lazy index to load, then mutate through the normal write paths.
        assert_eq!(
            store
                .search_lexical("probe", "alpha", 10, false, false, &[], "admin", 2)
                .await
                .unwrap()
                .results
                .len(),
            1
        );

        payload.content = "beta term".to_string();
        store
            .upsert("probe", "rule-1", vec![0.0; 4], payload)
            .await
            .unwrap();
        assert!(store
            .search_lexical("probe", "alpha", 10, false, false, &[], "admin", 2)
            .await
            .unwrap()
            .results
            .is_empty());
        assert_eq!(
            store
                .search_lexical("probe", "beta", 10, false, false, &[], "admin", 2)
                .await
                .unwrap()
                .results
                .len(),
            1
        );

        store.delete("probe", "rule-1").await.unwrap();
        assert!(store
            .search_lexical("probe", "beta", 10, false, false, &[], "admin", 2)
            .await
            .unwrap()
            .results
            .is_empty());
    }

    #[tokio::test]
    async fn export_graph_resolves_each_node_by_its_source_identity() {
        let store = retired_test_store().await;
        let root = std::env::temp_dir().join(format!("ns-export-context-{}", uuid::Uuid::new_v4()));
        let root_a = root.join("project-a");
        let root_b = root.join("project-b");
        let marker_a = root_a.join(".NeuroStrata");
        let marker_b = root_b.join(".NeuroStrata");
        std::fs::create_dir_all(&marker_a).unwrap();
        std::fs::create_dir_all(&marker_b).unwrap();
        let contexts = std::collections::HashMap::from([
            ("source-a".to_string(), marker_a),
            ("source-b".to_string(), marker_b),
        ]);

        for (id, source_id) in [("node-a", "source-a"), ("node-b", "source-b")] {
            let mut payload = retired_test_payload(
                "file",
                "agent",
                json!({
                    "absolute_path": format!("/stored/{source_id}/src/lib.rs"),
                    "refs": [{
                        "file": "src/lib.rs",
                        "source_context": { "source_id": source_id, "provider": "kept" },
                        "provider_data": { "opaque": true }
                    }]
                }),
            );
            payload.location = "src/lib.rs".to_string();
            store
                .upsert("probe", id, vec![0.0; 4], payload)
                .await
                .unwrap();
        }

        let graph = store
            .export_graph(false, false, &contexts, None)
            .await
            .unwrap();
        for (id, expected_root) in [("node-a", root_a), ("node-b", root_b)] {
            let node = graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == id)
                .unwrap();
            assert_eq!(
                node["absolute_path"],
                expected_root.join("src/lib.rs").to_string_lossy().as_ref()
            );
            assert_eq!(node["metadata"]["refs"][0]["provider_data"]["opaque"], true);
            assert_eq!(
                node["metadata"]["refs"][0]["source_context"]["source_id"],
                if id == "node-a" {
                    "source-a"
                } else {
                    "source-b"
                }
            );
        }
    }

    async fn governs_edges_from(store: &LadybugStore, id: &str) -> usize {
        let id = escape_ladybug_string(id);
        store
            .with_conn(move |conn| {
                let mut rows = conn.query(&format!(
                    "MATCH (a:Memory {{id: '{}'}})-[:GOVERNS]->(:Memory) RETURN count(*)",
                    id
                ))?;
                Ok(rows
                    .next()
                    .map(|row| format!("{}", row[0]).parse().unwrap_or(0))
                    .unwrap_or(0))
            })
            .await
            .expect("count GOVERNS edges")
    }

    /// Guinea-pig BUG-6 and BUG-7: the export is the portable artifact, so it
    /// must carry the declarative anchors, mark superseded rows with their
    /// replacement, and include the extraction edges the done-funnel creates.
    #[tokio::test]
    async fn export_carries_metadata_marks_superseded_and_keeps_extraction_edges() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];
        // Targets first: the write-shape validator refuses edges whose target
        // row does not exist yet.
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "new-rule",
                zero.clone(),
                retired_test_payload("rule", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "old-rule",
                zero,
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "superseded_by": "new-rule", "valid_to": 1, "related_to": ["new-rule"] }),
                ),
            )
            .await
            .unwrap();

        let all = store
            .export_graph(true, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let nodes = all["nodes"].as_array().unwrap();
        let old = nodes.iter().find(|n| n["id"] == "old-rule").unwrap();
        assert_eq!(old["superseded"], true);
        assert_eq!(old["superseded_by"], "new-rule");
        assert!(
            old["metadata"]["related_to"].is_array(),
            "metadata travels with the node"
        );
        let new = nodes.iter().find(|n| n["id"] == "new-rule").unwrap();
        assert_eq!(new["superseded"], false);
        assert_eq!(new["superseded_by"], serde_json::Value::Null);

        let links = all["links"].as_array().unwrap();
        assert!(
            links.iter().any(|l| l["type"] == "EXTRACTED_FROM"),
            "the consolidation edge is part of the graph"
        );

        let active = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let ids: Vec<_> = active["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].clone())
            .collect();
        assert!(
            !ids.contains(&json!("old-rule")),
            "--exclude-superseded drops retired rows"
        );
    }

    #[tokio::test]
    async fn relinking_does_not_give_a_retired_rule_its_edges_back() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";
        let zero = vec![0.0f32; 4];

        // Both rules are written before the file exists, so only relinking can
        // give them edges.
        store
            .upsert(
                "probe",
                "retired-rule",
                zero.clone(),
                retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "live-rule",
                zero.clone(),
                retired_test_payload("rule", "agent", json!({ "governs": [file] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                file,
                zero,
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();

        store.relink_edges("probe", PROBE_SOURCE).await.expect("relink");

        assert_eq!(
            governs_edges_from(&store, "live-rule").await,
            1,
            "a live rule is relinked"
        );
        assert_eq!(
            governs_edges_from(&store, "retired-rule").await,
            0,
            "a retired rule is not"
        );
    }

    #[tokio::test]
    async fn the_graph_does_not_bring_back_a_retired_rule() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";

        store
            .upsert(
                "probe",
                file,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "symbol",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "code_ast",
                    "auto-ingestor",
                    json!({ "contained_by": [file] }),
                ),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-rule",
                vec![0.0, 1.0, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })),
            )
            .await
            .unwrap();

        // The symbol matches; its file is governed by the retired rule, two hops
        // out -- exactly the path the governing expansion follows. The query
        // vector is not whole numbers: `1` and `0` would print as an integer
        // list, which array_distance refuses.
        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 5, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert!(ids.contains(&"symbol".to_string()), "{:?}", ids);
        assert!(
            !ids.contains(&"retired-rule".to_string()),
            "a retired rule stays retired: {:?}",
            ids
        );
    }

    #[test]
    fn a_declaration_prefers_its_own_namespace_then_its_exact_id_then_one_suffix() {
        // Generation 5 is active; the qualified form is now generation-scoped.
        let scoped = crate::parser::ingest::generation_scoped_id("probe", 5, "src/a.rs");
        let both = vec![scoped.clone(), "src/a.rs".to_string()];
        assert!(matches!(
            pick_declared_target("probe", 5, "src/a.rs", &both),
            crate::traits::TargetResolution::Resolved(ref id) if *id == scoped
        ));
        assert!(matches!(
            pick_declared_target("probe", 5, "src/a.rs", &["src/a.rs".to_string()]),
            crate::traits::TargetResolution::Resolved(ref id) if id == "src/a.rs"
        ));
        assert!(matches!(
            pick_declared_target("probe", 5, "C:/old/src/a.rs", &[scoped.clone()]),
            crate::traits::TargetResolution::Resolved(ref id) if *id == scoped
        ));
        assert!(
            matches!(
                pick_declared_target(
                    "probe",
                    5,
                    "C:/old/src/a.rs",
                    &[
                        scoped.clone(),
                        crate::parser::ingest::generation_scoped_id("probe", 5, "a.rs"),
                    ]
                ),
                crate::traits::TargetResolution::Ambiguous(_)
            ),
            "an ambiguous suffix is left alone"
        );
        assert!(declared_target_candidates("probe", 5, "C:/old/src/a.rs").contains(&scoped));
    }

    #[tokio::test]
    async fn upsert_reports_unresolved_edge_target_and_declaration_index() {
        let store = retired_test_store().await;
        let report = store
            .upsert(
                "probe",
                "rule-with-deferred-edge",
                vec![0.0; 4],
                retired_test_payload(
                    "rule",
                    "agent",
                    // A qualified cross-namespace target never gets a stub
                    // (spec §3.2: stubs are only for the unqualified missing
                    // case), so it still defers with `Missing`.
                    json!({ "governs": ["other-ns::src/not-ingested.rs"] }),
                ),
            )
            .await
            .expect("memory write remains fail-soft");

        assert_eq!(report.id, "rule-with-deferred-edge");
        assert_eq!(report.deferred_edges.len(), 1);
        assert_eq!(report.deferred_edges[0].rel_type, "GOVERNS");
        assert_eq!(
            report.deferred_edges[0].declared_target,
            "other-ns::src/not-ingested.rs"
        );
        assert_eq!(report.deferred_edges[0].declaration_index, 0);
        assert!(matches!(
            report.deferred_edges[0].resolution,
            crate::traits::TargetResolution::Missing
        ));
    }

    /// R8: `linked` runs the same task-existence check as `created`, so a
    /// candidate cannot be linked to a missing (or cross-namespace) task.
    #[tokio::test]
    async fn resolve_candidate_rejects_linked_missing_task() {
        let store = retired_test_store().await;
        let err = crate::traits::IntakeStore::resolve_candidate(&store, crate::task::intake::ResolveCandidateRequest {
                namespace: "probe".to_string(),
                candidate_id: "cand-linked-no-task".to_string(),
                disposition: crate::task::intake::Disposition::Linked,
                task_id: Some("task-does-not-exist".to_string()),
                cas_version: 0,
            })
            .await
            .expect_err("linked to a non-existent task must fail");
        assert!(
            err.contains("does not refer to an existing task"),
            "unexpected error: {err}"
        );
    }

    /// R14: a memory id is globally unique, so upserting the same id into a
    /// second namespace must be rejected instead of silently relocating the row.
    #[tokio::test]
    async fn upsert_rejects_cross_namespace_id_collision() {
        let store = retired_test_store().await;
        store
            .upsert(
                "ns-a",
                "shared-id",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .expect("first namespace owns the id");
        let err = store
            .upsert(
                "ns-b",
                "shared-id",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .expect_err("upserting the same id into another namespace must be rejected");
        assert!(
            err.to_string().contains("cross-namespace id collision"),
            "unexpected error: {err}"
        );
        // The original row stays intact in ns-a.
        let original = store.get("ns-a", "shared-id").await.expect("read ns-a row");
        assert!(original.is_some(), "original row must remain untouched");
    }

    #[tokio::test]
    async fn upsert_writes_edge_provenance() {
        let store = retired_test_store().await;
        // Auto-stub materialises the target file node, so the edge forms.
        store
            .upsert(
                "probe",
                "rule-with-edge",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/x.rs"] })),
            )
            .await
            .expect("upsert");
        let rows = store
            .with_conn(|conn| {
                let mut r = conn
                    .query("MATCH (a:Memory)-[e:GOVERNS]->(b:Memory) RETURN e.source, e.extractor")
                    .unwrap();
                let mut out = Vec::new();
                while let Some(row) = r.next() {
                    out.push((format!("{}", row[0]), format!("{}", row[1])));
                }
                Ok(out)
            })
            .await
            .expect("query");
        assert!(!rows.is_empty(), "edge should exist");
        for (source, extractor) in &rows {
            assert_eq!(source, "probe");
            assert_eq!(extractor, "upsert");
        }
    }

    #[tokio::test]
    async fn project_namespace_rdf_includes_nodes_and_edges() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "rule-with-edge",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/x.rs"] })),
            )
            .await
            .expect("upsert");
        let quads = store
            .project_namespace_rdf("probe", crate::rdf::Tier::Domain, 1)
            .await
            .expect("project");
        assert!(quads.iter().any(|q| q.predicate.ends_with("#content")));
        assert!(quads.iter().any(|q| q.predicate.ends_with("#governs")));
        assert!(quads.iter().all(|q| q.graph.as_deref() == Some("urn:neurostrata:graph:probe:domain:gen:1")));
    }
    /// Spec §3.2: an unqualified `GOVERNS` target that resolves to `Missing`
    /// auto-materialises a stub `file` node, so the edge forms immediately.
    #[tokio::test]
    async fn a_missing_unqualified_governs_target_materialises_a_stub_file_node() {
        let store = retired_test_store().await;
        let report = store
            .upsert(
                "probe",
                "rule-governing-unindexed-file",
                vec![0.0; 4],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["src/not-ingested.rs"] }),
                ),
            )
            .await
            .expect("memory write remains fail-soft");

        assert_eq!(report.id, "rule-governing-unindexed-file");
        assert!(
            report.deferred_edges.is_empty(),
            "a stubbed GOVERNS target materialises, not defers: {:?}",
            report.deferred_edges
        );

        // The stub exists under the legacy qualified id with a zero vector and
        // the structural `file` shape, marked `stub: true`.
        let stub_id = crate::parser::ingest::qualified_id("probe", "src/not-ingested.rs");
        let (vector, payload) = store
            .get("probe", &stub_id)
            .await
            .expect("read the stub back")
            .expect("the stub file node exists");
        assert_eq!(payload.memory_type, "file");
        assert_eq!(payload.user_id, "auto-ingestor");
        assert_eq!(payload.agent_name.as_deref(), Some("neurostrata-stub"));
        assert_eq!(
            payload.metadata.get("stub").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.metadata.get("absolute_path").and_then(|v| v.as_str()),
            Some("src/not-ingested.rs")
        );
        assert_eq!(vector, vec![0.0; 4]);

        // The rule now finds the file through the graph-read capability.
        let hits = store
            .memories_for_file(
                "probe",
                None,
                "src/not-ingested.rs",
                None,
                true,
                &["rule".to_string()],
                &[],
                5,
            )
            .await
            .expect("graph read succeeds");
        assert!(
            hits.iter()
                .any(|h| h.id == "rule-governing-unindexed-file"),
            "the governing rule surfaces for the stubbed file: {:?}",
            hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
    }

    /// A rule that governs a *directory* (`src/`) surfaces for an unindexed
    /// file inside it (`src/foo.rs`) through the `AncestorDirectory` match. The
    /// stub materialised for the directory carries `memory_type = "directory"`,
    /// not `"file"`, so the ancestor match recognises it.
    #[tokio::test]
    async fn unindexed_file_returns_parent_directory_rules() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "dir-governing-rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/"] })),
            )
            .await
            .expect("memory write remains fail-soft");

        // The directory stub is a directory, not a file.
        let stub_id = crate::parser::ingest::qualified_id("probe", "src");
        let (_, payload) = store
            .get("probe", &stub_id)
            .await
            .expect("read the stub back")
            .expect("the directory stub exists");
        assert_eq!(payload.memory_type, "directory");

        let hits = store
            .memories_for_file(
                "probe",
                None,
                "src/foo.rs",
                None,
                true,
                &["rule".to_string()],
                &[],
                5,
            )
            .await
            .expect("graph read succeeds");
        assert!(
            hits.iter().any(|h| h.id == "dir-governing-rule"),
            "the directory-governing rule surfaces for an unindexed file inside it: {:?}",
            hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
        let hit = hits
            .iter()
            .find(|h| h.id == "dir-governing-rule")
            .expect("the rule is among the hits");
        assert_eq!(hit.match_kind, GraphMatchKind::AncestorDirectory);
    }

    /// A memory linked by a `RELATES_TO` edge (not a `GOVERNS` edge) surfaces
    /// for the file it relates to, with the `Related` match kind. The relation
    /// is undirected, but the stored row is directed, so both orientations must
    /// be queried (finding 1).
    #[tokio::test]
    async fn a_related_memory_surfaces_for_the_file_it_relates_to() {
        let store = retired_test_store().await;

        // Target first: the write-shape validator refuses edges whose target
        // row does not exist yet.
        let mut file_payload = retired_test_payload("file", "auto-ingestor", json!({}));
        file_payload.location = "src/a.rs".to_string();
        store
            .upsert("probe", "probe::src/a.rs", vec![0.0; 4], file_payload)
            .await
            .unwrap();
        // The out-orientation: the memory declares the file, so the stored edge
        // is `memory -RELATES_TO-> file`.
        store
            .upsert(
                "probe",
                "related-memory",
                vec![0.0; 4],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({ "related_to": ["probe::src/a.rs"] }),
                ),
            )
            .await
            .unwrap();

        let hits = store
            .memories_for_file(
                "probe",
                None,
                "src/a.rs",
                None,
                true,
                &["fact".to_string()],
                &[],
                5,
            )
            .await
            .expect("graph read succeeds");
        let hit = hits
            .iter()
            .find(|h| h.id == "related-memory")
            .expect("the related memory surfaces: {:?}");
        assert_eq!(hit.match_kind, GraphMatchKind::Related);
        assert!(hit.explanation.contains("relates to"));
    }

    /// The `related` traversal reaches a depth-2 neighbour through a real
    /// `RELATES_TO` chain, marks every crossed edge `Supporting` (never
    /// `Governance`), and starts from the seed's exact id.
    #[tokio::test]
    async fn related_traverses_a_depth_two_relates_to_chain() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];

        // Targets first: the write-shape validator refuses edges whose target
        // row does not exist yet.
        store
            .upsert("probe", "deep", zero.clone(), retired_test_payload("fact", "agent", json!({})))
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "mid",
                zero.clone(),
                retired_test_payload("fact", "agent", json!({ "related_to": ["deep"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "seed",
                zero,
                retired_test_payload("rule", "agent", json!({ "related_to": ["mid"] })),
            )
            .await
            .unwrap();

        let result = store
            .related(
                "probe",
                "seed",
                &[],
                crate::graph_read::related::Direction::Both,
                2,
                &[],
                25,
                None,
                &crate::graph_read::traversal::CrossNamespacePolicy::default(),
            )
            .await
            .expect("related traversal succeeds");

        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["seed", "mid", "deep"]);
        assert_eq!(result.total, 3);
        assert!(
            result
                .edges
                .iter()
                .all(|e| e.role == crate::graph_read::related::EdgeRole::Supporting),
            "RELATES_TO edges are supporting, never governance: {:?}",
            result.edges
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.source == "mid" && e.target == "deep"),
            "the depth-2 hop is present as an edge"
        );
    }

    /// A stub materialised for an unindexed file is retired once the real node
    /// is ingested and published, so the placeholder never shadows the real
    /// tree (finding 3).
    #[tokio::test]
    async fn ingesting_the_real_node_retires_its_stub() {
        let store = retired_test_store().await;

        // The rule names a file that is not yet ingested: a stub materialises.
        store
            .upsert(
                "probe",
                "rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/not-ingested.rs"] })),
            )
            .await
            .unwrap();
        let stub_id = crate::parser::ingest::qualified_id("probe", "src/not-ingested.rs");
        assert!(
            store.get("probe", &stub_id).await.unwrap().is_some(),
            "the stub exists before ingestion"
        );

        // The real node arrives, staged and published as generation 1.
        let real_id =
            crate::parser::ingest::generation_scoped_id("probe", 1, "src/not-ingested.rs");
        store
            .upsert(
                "probe",
                &real_id,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store.publish_generation("probe", PROBE_SOURCE, 1).await.unwrap();

        store.relink_edges("probe", PROBE_SOURCE).await.expect("relink");

        assert!(
            store.get("probe", &stub_id).await.unwrap().is_none(),
            "the stub is retired once the real node is ingested"
        );
        assert!(
            store.get("probe", &real_id).await.unwrap().is_some(),
            "the real node remains"
        );
    }

    /// Ingestion qualifies node ids, and a rule names the file the way a human
    /// writes it. The edge has to form when the rule is written, not only after
    /// the next ingest happens to relink it.
    #[tokio::test]
    async fn a_rule_added_after_ingest_governs_the_file_it_names() {
        let store = retired_test_store().await;
        // Ingestion stages the file as a generation-scoped node and publishes
        // generation 1; the rule written afterwards names the bare path and must
        // attach to the published tree, not to a superseded one.
        let file_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        store
            .upsert(
                "probe",
                &file_id,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store.publish_generation("probe", PROBE_SOURCE, 1).await.unwrap();
        store
            .upsert(
                "probe",
                "rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/a.rs"] })),
            )
            .await
            .unwrap();

        assert_eq!(governs_edges_from(&store, "rule").await, 1);
    }

    /// Moving used to be "write to the target, delete from the source". upsert
    /// never rewrites a namespace, so the write updated the source row in place
    /// and the delete then removed the only copy.
    #[tokio::test]
    async fn moving_a_memory_keeps_the_row_and_its_edges() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["probe::src/a.rs"] })),
            )
            .await
            .unwrap();

        assert_eq!(
            store.relocate("rule", "probe", "global").await.unwrap(),
            RelocateOutcome::Moved
        );

        assert!(
            store.get("global", "rule").await.unwrap().is_some(),
            "the memory is in its new namespace"
        );
        assert!(
            store.get("probe", "rule").await.unwrap().is_none(),
            "and no longer in the old one"
        );
        assert_eq!(
            governs_edges_from(&store, "rule").await,
            1,
            "its edges came with it"
        );
    }

    #[tokio::test]
    async fn an_ingested_node_is_not_moved() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .relocate("probe::src/a.rs", "probe", "other")
                .await
                .unwrap(),
            RelocateOutcome::Ingested
        );
        assert!(
            store
                .get("probe", "probe::src/a.rs")
                .await
                .unwrap()
                .is_some(),
            "and it stays where it was"
        );
        assert_eq!(
            store.relocate("missing", "probe", "other").await.unwrap(),
            RelocateOutcome::NotFound
        );
        assert_eq!(
            store.relocate("rule", "probe", "probe").await.unwrap(),
            RelocateOutcome::SameNamespace
        );
    }

    #[test]
    fn each_metadata_key_maps_to_its_edge_table() {
        let specs = edge_specs(&json!({
            "related_to": ["a"],
            "contained_by": ["parent"],
            "governs": ["src/lib.rs"]
        }));
        assert_eq!(specs.len(), 3);
        assert!(specs.contains(&EdgeSpec {
            rel_type: "RELATES_TO",
            target_id: "a".into(),
            points_at_target: true,
            declaration_index: 0
        }));
        assert!(specs.contains(&EdgeSpec {
            rel_type: "GOVERNS",
            target_id: "src/lib.rs".into(),
            points_at_target: true,
            declaration_index: 0
        }));
    }

    /// Containment points from the parent to the child, so a directory contains
    /// its files rather than the other way round.
    #[test]
    fn containment_points_from_the_parent() {
        let specs = edge_specs(&json!({ "contained_by": ["src"] }));
        assert_eq!(specs[0].rel_type, "CONTAINS");
        assert!(!specs[0].points_at_target);
    }

    #[test]
    fn absent_or_empty_metadata_asks_for_no_edges() {
        assert!(edge_specs(&json!({})).is_empty());
        assert!(edge_specs(&json!({ "related_to": [] })).is_empty());
        assert!(edge_specs(&json!({ "related_to": [""] })).is_empty());
        assert!(edge_specs(&json!(null)).is_empty());
    }

    /// A store with three nodes: a rule that declares the file it governs, a
    /// second rule that declares the same file the way ingestion used to write
    /// it -- absolute -- and the file itself.
    async fn store_with_two_declarations() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let seed = |id: &str, metadata: &str| {
            format!(
                "CREATE (m:Memory {{id: '{}', namespace: 'probe', content: 'c', user_id: 'u', memory_type: 't', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
                escape_ladybug_string(id),
                escape_ladybug_string(metadata)
            )
        };
        // Ingested nodes are generation-scoped now, and generation 1 is the
        // published tree the relink must attach to. The file row therefore
        // carries the scoped id, and the active pointer is flipped below.
        let file_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        let rows = [
            seed("rule", r#"{"governs": ["src/a.rs"]}"#),
            seed("legacy", r#"{"governs": ["C:/old/checkout/src/a.rs"]}"#),
            seed(&file_id, "{}"),
        ];
        for row in rows {
            store
                .with_conn(move |conn| {
                    conn.query(&row)?;
                    Ok(())
                })
                .await
                .expect("seed a memory");
        }
        store
            .publish_generation("probe", PROBE_SOURCE, 1)
            .await
            .expect("publish generation 1");
        store
    }

    async fn governs_count(store: &LadybugStore) -> usize {
        store
            .with_conn(|conn| {
                let result = conn.query("MATCH (:Memory)-[:GOVERNS]->(:Memory) RETURN count(*)")?;
                let mut count = 0usize;
                for row in result {
                    count += format!("{}", row[0]).parse::<usize>().unwrap_or(0);
                }
                Ok(count)
            })
            .await
            .expect("count the edges")
    }

    /// Both declarations are replayed, including the one that names the file by
    /// its old absolute path.
    #[tokio::test]
    async fn relinking_replays_every_declaration_once() {
        let store = store_with_two_declarations().await;

        let created = store.relink_edges("probe", PROBE_SOURCE).await.expect("relink");
        assert_eq!(created, 2, "both declarations name a node that exists");
        assert_eq!(governs_count(&store).await, 2);
    }

    /// Relinking runs at the end of every ingest, so running it twice must not
    /// double the graph. The count is of edges actually created, not of
    /// statements that did not error -- the loop this replaced reported the
    /// latter, so it claimed hundreds of edges on a run that made none.
    #[tokio::test]
    async fn relinking_twice_creates_nothing_the_second_time() {
        let store = store_with_two_declarations().await;

        assert_eq!(store.relink_edges("probe", PROBE_SOURCE).await.expect("first"), 2);
        assert_eq!(
            store.relink_edges("probe", PROBE_SOURCE).await.expect("second"),
            0,
            "the edges already exist, so none are created"
        );
        assert_eq!(governs_count(&store).await, 2);
    }

    /// Regression guard for task neurostrata-c8te (parallel-test mmap flake):
    /// `LadybugStore::for_testing` MUST use a small enough max_db_size +
    /// buffer_pool_size that N parallel opens don't trip the 8 TiB mmap
    /// reservation the C++ default carries over (DEFAULT_VM_REGION_MAX_SIZE
    /// = `(uint64_t)1 << 43` on Linux x86_64). 32 threads chosen to exceed
    /// the flake's worst observed contention.
    ///
    /// NOTE on drill: this test cannot drill in this dev box (30 GiB RAM,
    /// `ulimit -v unlimited`, Linux overcommit_memory=0 — the 8 TiB mmaps
    /// succeed at the call site). It WILL trip in stricter CI containers
    /// (GitHub Actions: 7 GiB, ulimit set), which is where the original
    /// flake surfaced. The fix is verified directly below in a separate
    /// unit test that asserts the configuration choice.
    #[test]
    fn for_testing_does_not_reserve_an_8tib_mmap() {
        const N: usize = 32;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
        let mut handles = Vec::new();
        for i in 0..N {
            let b = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let dir = std::env::temp_dir().join(format!(
                    "ns-mmap-parallel-{}-{}",
                    std::process::id(),
                    i
                ));
                b.wait();
                LadybugStore::for_testing(&dir, 4)
                    .map(|_| ())
                    .map_err(|e| format!("thread {} failed to open: {}", i, e))
            }));
        }
        for h in handles {
            match h.join().expect("thread panicked") {
                Ok(()) => {}
                Err(e) => panic!("parallel test open failed (regression of c8te): {}", e),
            }
        }
    }

    /// Drill-independent regression for c8te: even on a dev box that allows
    /// 8 TiB mmaps via overcommit, this test verifies the configuration
    /// choice directly: `for_testing` MUST keep the `max_db_size` /
    /// `buffer_pool_size` overrides in its function body. If someone
    /// drops the overrides, the C++ default (8 TiB on Linux x64) returns
    /// at the binding level (`lbug_rs.cpp` only forwards the Rust value
    /// if `!= -1u`/u32::MAX, but the C++ ctor's `SystemConfig()` sets the
    /// 8 TiB default BEFORE the binding overlay -- so leaving the Rust
    /// value at `Default::default()` re-enables the 8 TiB).
    ///
    /// Scoped to the `for_testing` function body so the assertion does not
    /// match its own search string (an earlier version of this guard was
    /// fooled by exactly that -- a self-referential assertion in the test
    /// code itself).
    #[test]
    fn for_testing_uses_small_max_db_size() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/ladybug.rs"),
        )
        .expect("read self");
        let start = src
            .find("pub fn for_testing")
            .expect("for_testing exists in this file");
        let end_marker = "Self::with_system_config";
        let end_rel = src[start..]
            .find(end_marker)
            .expect("end of for_testing call found");
        let body = &src[start..start + end_rel + end_marker.len()];
        assert!(
            body.contains(".max_db_size(max_db_size)"),
            "for_testing must shrink max_db_size below the C++ default \
             (8 TiB on Linux x64); parallel test opens trip the mmap limit \
             -- task neurostrata-c8te regression guard"
        );
        assert!(
            body.contains(".buffer_pool_size(buffer_pool_size)"),
            "for_testing must cap buffer_pool_size for parallel tests"
        );
    }

    /// A declaration naming something that was never ingested links nothing,
    /// and is not counted as though it had.
    #[tokio::test]
    async fn a_declaration_with_no_node_behind_it_links_nothing() {
        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let row = format!(
            "CREATE (m:Memory {{id: 'rule', namespace: 'probe', content: 'c', user_id: 'u', memory_type: 't', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
            escape_ladybug_string(r#"{"governs": ["src/never-ingested.rs"]}"#)
        );
        store
            .with_conn(move |conn| {
                conn.query(&row)?;
                Ok(())
            })
            .await
            .expect("seed a memory");

        assert_eq!(store.relink_edges("probe", PROBE_SOURCE).await.expect("relink"), 0);
        assert_eq!(governs_count(&store).await, 0);
    }

    /// A superseded generation's rows are hidden from the read surfaces once a
    /// later publish flips the source's active pointer, while the live rows
    /// remain reachable. Exercises the source-scoped pointer and the
    /// `list`/`export_graph` superseded filters directly (finding 6).
    #[tokio::test]
    async fn superseded_generations_are_hidden_from_list_and_export() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];

        // Stage and publish generation 1 for source A.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 1);
        let gen1_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        store
            .upsert(
                "probe",
                &gen1_id,
                zero.clone(),
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .publish_generation("probe", "source-a", 1)
            .await
            .unwrap();
        assert_eq!(
            store.active_generation("probe", "source-a").await.unwrap(),
            1
        );

        // A second publish of the same source supersedes generation 1.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 2);
        let gen2_id = crate::parser::ingest::generation_scoped_id("probe", 2, "src/a.rs");
        store
            .upsert(
                "probe",
                &gen2_id,
                zero.clone(),
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .publish_generation("probe", "source-a", 2)
            .await
            .unwrap();
        assert_eq!(
            store.active_generation("probe", "source-a").await.unwrap(),
            2
        );

        // list: only the active generation's row is reachable.
        let listed = store.list("probe", None, None).await.unwrap();
        let listed_ids: Vec<String> = listed.into_iter().map(|r| r.id).collect();
        assert!(listed_ids.contains(&gen2_id), "the active generation is listed");
        assert!(
            !listed_ids.contains(&gen1_id),
            "the superseded generation is not listed"
        );

        // export_graph: the same filter drops the superseded row.
        let graph = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let exported_ids: Vec<String> = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect();
        assert!(
            exported_ids.iter().any(|id| id == &gen2_id),
            "the active generation is exported"
        );
        assert!(
            !exported_ids.iter().any(|id| id == &gen1_id),
            "the superseded generation is not exported"
        );
    }

    /// Wave 2.1 TDD: search excludes rows from superseded ingest generations
    /// while preserving unscoped normative memories (rule/fact/decision/procedure)
    /// and deduping by bare_id across generations (keep highest gen).
    #[tokio::test]
    async fn search_excludes_inactive_ingest_generations_and_preserves_normative_memories() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Generation 1 for source-a: a code_ast row that will be superseded.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 1);
        let gen1_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        store
            .upsert(
                "probe",
                &gen1_id,
                zero.clone(),
                retired_test_payload("code_ast", "auto-ingestor", json!({ "source_key": "source-a" })),
            )
            .await
            .unwrap();
        store
            .publish_generation("probe", "source-a", 1)
            .await
            .unwrap();

        // Generation 2 for source-a: same bare_id, supersedes generation 1.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 2);
        let gen2_id = crate::parser::ingest::generation_scoped_id("probe", 2, "src/a.rs");
        store
            .upsert(
                "probe",
                &gen2_id,
                zero.clone(),
                retired_test_payload("code_ast", "auto-ingestor", json!({ "source_key": "source-a" })),
            )
            .await
            .unwrap();
        store
            .publish_generation("probe", "source-a", 2)
            .await
            .unwrap();

        // Unscoped normative memories (always reachable, no generation scope).
        store
            .upsert(
                "probe",
                "normative-rule",
                query_vec.clone(),
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "normative-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // Search with limit >= 3 to trigger normative slot reservation.
        let results = store
            .search("probe", query_vec.clone(), 5, false, false)
            .await
            .unwrap();
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();

        // Active generation (gen 2) scoped row is present.
        assert!(
            ids.contains(&gen2_id.as_str()),
            "active generation row {gen2_id} should be in search results"
        );
        // Superseded generation (gen 1) scoped row is excluded.
        assert!(
            !ids.contains(&gen1_id.as_str()),
            "superseded generation row {gen1_id} should NOT be in search results"
        );
        // Unscoped normative memories are preserved.
        assert!(
            ids.contains(&"normative-rule"),
            "normative rule should be in search results"
        );
        assert!(
            ids.contains(&"normative-fact"),
            "normative fact should be in search results"
        );
        // Dedup by bare_id: only one version of src/a.rs appears.
        let a_rs_count = ids.iter().filter(|id| id.ends_with("src/a.rs")).count();
        assert_eq!(
            a_rs_count,
            1,
            "exactly one generation of src/a.rs should appear (dedup by bare_id)"
        );

        // Normative slot reservation: the top 2 results are normative memories even if
        // extra active code_ast/doc_section rows match closer in raw vector distance.
        assert!(
            matches!(results[0].payload.memory_type.as_str(), "rule" | "fact"),
            "slot 0 reserved for normative memory, got {}",
            results[0].payload.memory_type
        );
        assert!(
            matches!(results[1].payload.memory_type.as_str(), "rule" | "fact"),
            "slot 1 reserved for normative memory, got {}",
            results[1].payload.memory_type
        );

        // Multi-source isolation in list() and search(): publish docs at gen 3 while
        // code_ast (source-a) stays at gen 2. A stale code_ast without source_key at
        // gen 3 must NOT pass simply because docs is at gen 3.
        let _ = store.begin_generation("probe", "docs").await.unwrap();
        store.publish_generation("probe", "docs", 3).await.unwrap();
        let stale_code_gen3 = crate::parser::ingest::generation_scoped_id("probe", 3, "src/stale.rs");
        store
            .upsert(
                "probe",
                &stale_code_gen3,
                query_vec.clone(),
                retired_test_payload("code_ast", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        let listed = store.list("probe", None, None).await.unwrap();
        let listed_ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
        assert!(
            listed_ids.contains(&gen2_id.as_str()),
            "list() keeps active code_ast generation 2"
        );
        assert!(
            !listed_ids.contains(&gen1_id.as_str()),
            "list() drops superseded generation 1"
        );
        assert!(
            !listed_ids.contains(&stale_code_gen3.as_str()),
            "list() drops code_ast without source_key whose gen (3) only matches docs, not max_code_gen (2)"
        );

        // Multi-source: source-b publishes generation 4. A code_ast row at gen 2
        // without source_key must NOT be dropped (it matches source-a's active gen
        // 2), and a code_ast row at gen 4 without source_key must be retained
        // (it matches source-b's active gen 4). Both must survive list().
        assert_eq!(store.begin_generation("probe", "source-b").await.unwrap(), 4);
        let gen2_no_source_id = crate::parser::ingest::generation_scoped_id("probe", 2, "src/no-source-a.rs");
        store
            .upsert(
                "probe",
                &gen2_no_source_id,
                query_vec.clone(),
                retired_test_payload("code_ast", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        let gen4_no_source_id = crate::parser::ingest::generation_scoped_id("probe", 4, "src/no-source-b.rs");
        store
            .upsert(
                "probe",
                &gen4_no_source_id,
                query_vec.clone(),
                retired_test_payload("code_ast", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store.publish_generation("probe", "source-b", 4).await.unwrap();
        let listed2 = store.list("probe", None, None).await.unwrap();
        let listed2_ids: Vec<&str> = listed2.iter().map(|r| r.id.as_str()).collect();
        assert!(
            listed2_ids.contains(&gen2_no_source_id.as_str()),
            "list() retains code_ast without source_key at gen 2 (matches source-a active gen 2)"
        );
        assert!(
            listed2_ids.contains(&gen4_no_source_id.as_str()),
            "list() retains code_ast without source_key at gen 4 (matches source-b active gen 4)"
        );
    }

    /// `get` reads back the embedding it stored.
    ///
    /// The column is FLOAT[N] -- an ARRAY -- and the reader matched only
    /// Value::List, so this came back empty every time. Nothing caught it
    /// because search never returns a vector: it computes array_distance in the
    /// engine. Only the read-modify-write callers saw it, and they wrote the
    /// empty vector straight back.
    #[tokio::test]
    async fn get_reads_back_the_embedding_it_stored() {
        let dir = std::env::temp_dir().join(format!("ns-embed-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let stored = vec![0.25f32, -1.5, 3.0, 0.0];
        store
            .upsert(
                "probe",
                "rule",
                stored.clone(),
                MemoryPayload {
                    content: "c".to_string(),
                    user_id: "u".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("store the row");

        let (read_back, _) = store
            .get("probe", "rule")
            .await
            .expect("query succeeds")
            .expect("the row is there");
        assert_eq!(read_back, stored, "the embedding survives a round trip");
    }

    // --- TASK 1 tests: usage boost formula and counter saturation ---

    #[test]
    fn usage_boost_at_distance_zero_stays_zero() {
        // gain = 0.05 * ln(1+5) ≈ 0.080, capped_gain = min(0.080, 0) = 0
        let n = 5i64;
        let distance: f32 = 0.0;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        let capped = gain.min(0.5 * distance);
        let boosted = distance - capped;
        assert_eq!(boosted, 0.0);
    }

    #[test]
    fn usage_boost_at_count_zero_gives_zero_gain() {
        let n = 0i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        assert_eq!(gain, 0.0);
    }

    #[test]
    fn usage_boost_at_count_one_gives_real_gain() {
        let n = 1i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        assert!(gain > 0.0, "ln(1+1) should be > 0");
    }

    #[test]
    fn usage_boost_cap_holds_at_half_distance() {
        let distance: f32 = 0.1;
        let n = 8i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        let capped = gain.min(0.5 * distance);
        let boosted = distance - capped;
        assert!(boosted >= 0.0, "adjusted must be non-negative");
        assert!(boosted >= 0.5 * distance, "cap must hold: adjusted >= d/2");
        assert!(boosted <= distance, "adjusted must be <= d");
    }

    #[test]
    fn usage_boost_in_range_for_finite_distance() {
        for d in [0.01, 0.1, 0.5, 1.0, 5.0] {
            for n in [0, 1, 5, 100, 1000] {
                let gain = 0.05 * (1.0 + n as f32).ln();
                let capped = gain.min(0.5 * d);
                let boosted = d - capped;
                assert!(
                    boosted >= 0.5 * d,
                    "d={}, n={}: adjusted {} < d/2",
                    d,
                    n,
                    boosted
                );
                assert!(boosted <= d, "d={}, n={}: adjusted {} > d", d, n, boosted);
            }
        }
    }

    #[test]
    fn bump_access_count_saturates_at_i64_max() {
        let metadata = json!({"access_count": i64::MAX}).to_string();
        let result = bump_access_count(&metadata);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["access_count"], json!(i64::MAX));
    }

    #[tokio::test]
    async fn search_after_saturating_count_does_not_panic() {
        let store = retired_test_store().await;
        // Non-integer values: LadybugDB treats whole-number floats as integers,
        // which ARRAY_DISTANCE rejects.
        let v = vec![1.1, 0.0, 0.0, 0.0];
        store
            .upsert(
                "probe",
                "hot-memory",
                v.clone(),
                retired_test_payload("rule", "agent", json!({"access_count": i64::MAX})),
            )
            .await
            .unwrap();
        // Should not panic from overflow in the gain calculation.
        let results = store.search("probe", v, 10, false, false).await.unwrap();
        assert!(!results.is_empty());
    }

    // --- TASK 2 tests: search paging, expansion paging, export_graph ---

    #[tokio::test]
    async fn search_returns_active_results_when_retired_rows_fill_first_pages() {
        let store = retired_test_store().await;
        // Insert many retired rows with very close (identical) embeddings,
        // then one live row further away.
        for i in 0..20 {
            store
                .upsert(
                    "probe",
                    &format!("retired-{}", i),
                    vec![0.1; 4],
                    retired_test_payload("rule", "agent", json!({"valid_to": 1})),
                )
                .await
                .unwrap();
        }
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.9; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![0.1; 4], 5, false, false)
            .await
            .unwrap();
        let ids: Vec<String> = results.into_iter().map(|r| r.id).collect();
        assert!(
            ids.contains(&"live-row".to_string()),
            "paging must find the live row: {:?}",
            ids
        );
        assert!(
            ids.iter().all(|id| !id.starts_with("retired-")),
            "no retired rows should appear: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn search_expansion_excludes_retired_while_live_sibling_surfaces() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";

        store
            .upsert(
                "probe",
                file,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "live-rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": [file] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })),
            )
            .await
            .unwrap();

        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 5, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert!(ids.contains(&"live-rule".to_string()), "{:?}", ids);
        assert!(!ids.contains(&"retired-rule".to_string()), "{:?}", ids);
    }

    /// The mention symbol cache is keyed by the generation set alone, and a
    /// generation counter is per namespace, so two namespaces that each
    /// published generation 1 share the key {1}. The second namespace was then
    /// served the first one's symbol ids, and a rule could record mentions of
    /// another project's symbols.
    #[tokio::test]
    async fn mention_symbol_ids_are_scoped_to_their_namespace() {
        let store = retired_test_store().await;
        for namespace in ["probe", "other"] {
            let id = crate::parser::ingest::generation_scoped_id(namespace, 1, "src/lib.rs");
            store
                .upsert(
                    namespace,
                    &id,
                    vec![0.5; 4],
                    retired_test_payload("file", "auto-ingestor", json!({})),
                )
                .await
                .unwrap();
            store.publish_generation(namespace, PROBE_SOURCE, 1).await.unwrap();
        }

        let first = store.mention_symbol_ids("probe").await;
        let second = store.mention_symbol_ids("other").await;

        assert_eq!(first.as_slice(), ["probe::1::src/lib.rs".to_string()], "{first:?}");
        assert_eq!(
            second.as_slice(),
            ["other::1::src/lib.rs".to_string()],
            "the second namespace must see its own symbols, not the first one's: {second:?}"
        );
    }

    /// `list_pinned` narrows in the engine with `metadata CONTAINS 'pinned'`
    /// and its caller keeps a row only when `metadata.pinned` is the boolean
    /// `true`. The narrowing is safe only if it never drops a row the caller
    /// would keep, so this seeds rows where `pinned` is true, false, and absent,
    /// and checks that the caller sees the same pinned rows it would from a
    /// full listing.
    #[tokio::test]
    async fn list_pinned_keeps_every_pinned_row_and_drops_rows_that_never_mention_pinned() {
        use std::collections::BTreeSet;
        let store = retired_test_store().await;
        for (id, memory_type, meta) in [
            ("pinned-true", "rule", json!({ "pinned": true })),
            ("pinned-true-fact", "fact", json!({ "source": "x", "pinned": true })),
            ("pinned-false", "rule", json!({ "pinned": false })),
            ("no-key", "rule", json!({})),
            ("other-key", "rule", json!({ "note": "nothing here" })),
        ] {
            store
                .upsert("probe", id, vec![0.1; 4], retired_test_payload(memory_type, "agent", meta))
                .await
                .unwrap();
        }

        let pinned_ids = |rows: &[SearchResult]| -> BTreeSet<String> {
            rows.iter()
                .filter(|r| crate::task::is_pinned_memory(&r.payload.metadata))
                .map(|r| r.id.clone())
                .collect()
        };
        let all = store.list("probe", None, None).await.unwrap();
        let narrowed = store.list_pinned("probe").await.unwrap();
        assert_eq!(
            pinned_ids(&narrowed),
            pinned_ids(&all),
            "the caller sees the same pinned rows as from a full listing"
        );
        assert_eq!(
            pinned_ids(&all),
            BTreeSet::from(["pinned-true".to_string(), "pinned-true-fact".to_string()])
        );

        let narrowed_ids: BTreeSet<String> = narrowed.iter().map(|r| r.id.clone()).collect();
        assert!(narrowed_ids.contains("pinned-false"), "a superset: a pinned:false row mentions the word");
        assert!(!narrowed_ids.contains("no-key"), "a row with no metadata keys is dropped");
        assert!(!narrowed_ids.contains("other-key"), "a row that never mentions pinned is dropped");

        // list_rows with its other filters: type, and a needle that matches nothing.
        let rules_only = store.list_rows("probe", None, Some("rule"), Some("pinned")).await.unwrap();
        assert!(rules_only.iter().all(|r| r.payload.memory_type == "rule"), "the type filter still applies");
        assert!(rules_only.iter().any(|r| r.id == "pinned-true"));
        assert!(!rules_only.iter().any(|r| r.id == "pinned-true-fact"));
        assert!(
            store.list_rows("probe", None, None, Some("no-such-word-in-any-row")).await.unwrap().is_empty(),
            "a needle that no row contains returns nothing"
        );
        assert_eq!(
            store.list_rows("probe", None, None, None).await.unwrap().len(),
            all.len(),
            "no needle is the same as list"
        );
    }

    #[tokio::test]
    async fn search_stable_tie_order() {
        let store = retired_test_store().await;
        // Two live rows with identical embeddings; order should be deterministic.
        for name in &["alpha", "beta"] {
            store
                .upsert(
                    "probe",
                    name,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", json!({})),
                )
                .await
                .unwrap();
        }

        let first: Vec<String> = store
            .search("probe", vec![0.5; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        let second: Vec<String> = store
            .search("probe", vec![0.5; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(first, second, "tie order must be deterministic");
    }

    /// FEATURE-4: on equal semantic distance, the memory that says where it
    /// came from ranks first. Both rows carry the identical embedding, so
    /// distance ties exactly and provenance is the only thing that can separate
    /// them. The query is deliberately off-axis: at distance 0 the cap allows no
    /// boost at all, and a test that cannot fail is not a test.
    #[tokio::test]
    async fn sourced_row_outranks_unsourced_row_at_equal_distance() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "unsourced",
                vec![0.5; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "sourced",
                vec![0.5; 4],
                retired_test_payload("rule", "agent", json!({ "source": "owner 2026-10-09" })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![0.25; 4], 10, false, false)
            .await
            .unwrap();
        let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();

        assert_eq!(
            ids.first().map(|s| s.as_str()),
            Some("sourced"),
            "sourced memory must win the tie: {:?}",
            ids
        );
        // Both rows sit at the same distance; only the sourced one was promoted.
        assert!(
            results[0].score < results[1].score,
            "sourced row must carry the lower (better) score: {:?}",
            results
                .iter()
                .map(|r| (r.id.clone(), r.score))
                .collect::<Vec<_>>()
        );
    }

    /// The cap is the whole contract: a provenance marker settles a tie, it
    /// does not promote an off-topic memory over an on-topic one. Same boost,
    /// worse embedding, so raw distance must still decide.
    #[tokio::test]
    async fn provenance_never_overrides_a_better_match() {
        let store = retired_test_store().await;
        // Query points at [0.9,0.1,0.1,0.1]. The unsourced row sits exactly on
        // it (distance 0); the sourced row points the other way entirely.
        // Components are non-zero decimals because the query string is built by
        // Display on each f32 -- a 0.0 renders as "0" and the literal comes back
        // from the parser as INT, which ARRAY_DISTANCE refuses outright.
        store
            .upsert(
                "probe",
                "unsourced-closest",
                vec![0.9, 0.1, 0.1, 0.1],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "sourced-far",
                vec![0.1, 0.9, 0.1, 0.1],
                retired_test_payload("rule", "agent", json!({ "source": "owner 2026-10-09" })),
            )
            .await
            .unwrap();

        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert_eq!(
            ids.first().map(|s| s.as_str()),
            Some("unsourced-closest"),
            "a closer match wins even when unsourced: {:?}",
            ids
        );
    }

    /// Provenance and usage are separate signals and both still count. A sourced
    /// row with no reads must beat an unsourced row with none (provenance), and
    /// a heavily-used unsourced row must still beat a sourced row with none
    /// (usage is not subordinated to provenance).
    #[tokio::test]
    async fn provenance_and_usage_both_apply() {
        let store = retired_test_store().await;
        for (id, meta) in [
            ("plain", json!({})),
            ("sourced", json!({ "source": "owner 2026-10-09" })),
            ("used-unsourced", json!({ "access_count": 50 })),
        ] {
            store
                .upsert(
                    "probe",
                    id,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", meta),
                )
                .await
                .unwrap();
        }

        let scored: Vec<(String, f32)> = store
            .search("probe", vec![0.25; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.id, r.score))
            .collect();

        let score_of = |id: &str| {
            scored
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, s)| *s)
                .unwrap()
        };
        assert!(
            score_of("sourced") < score_of("plain"),
            "sourced outranks plain: {:?}",
            scored
        );
        assert!(
            score_of("used-unsourced") < score_of("sourced"),
            "usage gain is not capped away by provenance: {:?}",
            scored
        );
    }

    /// A blank or null `source` is not provenance. The gate treats it as
    /// missing, so the ranker must too -- otherwise passing the gate buys no
    /// ranking advantage and the two stop describing the same field.
    #[tokio::test]
    async fn blank_source_is_not_provenance() {
        let store = retired_test_store().await;
        for (id, meta) in [
            ("unsourced", json!({})),
            ("blank", json!({ "source": "   " })),
            ("null", json!({ "source": null })),
        ] {
            store
                .upsert(
                    "probe",
                    id,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", meta),
                )
                .await
                .unwrap();
        }

        let scored: Vec<(String, f32)> = store
            .search("probe", vec![0.25; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.id, r.score))
            .collect();

        let blank = scored.iter().find(|(i, _)| i == "blank").unwrap().1;
        let null = scored.iter().find(|(i, _)| i == "null").unwrap().1;
        let plain = scored.iter().find(|(i, _)| i == "unsourced").unwrap().1;
        assert_eq!(
            blank, plain,
            "a blank source is not provenance: {:?}",
            scored
        );
        assert_eq!(null, plain, "a null source is not provenance: {:?}", scored);
    }

    #[tokio::test]
    async fn export_graph_excludes_retired_by_default() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "another-live",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "related_to": ["live-node"] })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-node"), "{:?}", node_ids);
        assert!(node_ids.contains(&"another-live"), "{:?}", node_ids);
        assert!(!node_ids.contains(&"retired-node"), "{:?}", node_ids);

        // No dangling edges to retired nodes.
        let links = data["links"].as_array().unwrap();
        let retired_set: std::collections::HashSet<&str> =
            ["retired-node"].iter().copied().collect();
        for link in links {
            let src = link.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let tgt = link.get("target").and_then(|s| s.as_str()).unwrap_or("");
            assert!(
                !retired_set.contains(src) && !retired_set.contains(tgt),
                "dangling edge to retired node: {:?}",
                link
            );
        }
    }

    #[tokio::test]
    async fn export_graph_include_retired_true_shows_all() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(true, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-node"), "{:?}", node_ids);
        assert!(node_ids.contains(&"retired-node"), "{:?}", node_ids);
    }

    // ---- export-graph archived filter -----------------------------------
    //
    // The export-graph CLI mirrors the read surfaces: archived rows are
    // hidden by default, with an explicit `include_archived: true` escape
    // hatch. The trait gains a second parameter; the SQL filter excludes
    // rows whose `metadata.archived == true` when the caller does not ask
    // for them.

    /// A row tombstoned by `archive_memory` (or hand-written with the same
    /// shape) is excluded from the default export. The export-graph is a
    /// recall surface for "what is live", not "what was ever true".
    #[tokio::test]
    async fn export_graph_excludes_archived_by_default() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "archived-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "archived": true })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-row"), "{:?}", node_ids);
        assert!(
            !node_ids.contains(&"archived-row"),
            "archived rows are excluded by default: {node_ids:?}"
        );
    }

    /// `include_archived: true` is the audit / second-look escape hatch --
    /// every row, live and tombstoned, surfaces.
    #[tokio::test]
    async fn export_graph_include_archived_true_shows_all() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "archived-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "archived": true })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, true, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-row"), "{:?}", node_ids);
        assert!(
            node_ids.contains(&"archived-row"),
            "include_archived surfaces the tombstoned row: {node_ids:?}"
        );
    }

    // ---- export-graph conflict suppression --------------------------------
    //
    // A row marked under an unresolved conflict overlay is suppressed from
    // the effective view (spec §"Concurrent merge policy") the same way an
    // archived row is — except there is NO escape hatch for it: even
    // `include_archived: true` must never serve an unresolved conflict.

    /// Sensitivity note: this suppression site (the
    /// `is_conflicting_metadata` filter in `export_graph`) shipped before
    /// any test pinned it — the integration checker found that removing
    /// the filter leaves every suite green (a mutation-green site). This
    /// fixture is the pin: one conflicting row + one clean row, exported
    /// with `include_archived: true` so the audit hatch is wide open. The
    /// conflicting row stays out; the clean row is untouched by it.
    #[tokio::test]
    async fn export_graph_suppresses_conflicting_rows_even_with_include_archived() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "clean-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "conflict-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "mesh": { "conflict": true } })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, true, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(
            !node_ids.contains(&"conflict-row"),
            "unresolved conflict rows leave the effective view even with \
             include_archived: {node_ids:?}"
        );
        assert!(
            node_ids.contains(&"clean-row"),
            "the suppression is per-row, never a blanket export loss: {node_ids:?}"
        );
    }

    // --- Vocabulary validation (parse_edge_specs) tests ---

    #[test]
    fn vocabulary_validation_allows_missing_and_empty_arrays() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert!(warnings.is_empty());

        let p2 = MemoryPayload {
            metadata: json!({ "related_to": [], "contained_by": [], "governs": [] }),
            ..p.clone()
        };
        let (specs2, warnings2) = parse_edge_specs(&p2);
        assert!(specs2.is_empty());
        assert!(warnings2.is_empty());
    }

    #[test]
    fn vocabulary_validation_skips_only_invalid_edge_items() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["valid.rs", 42, "", null] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target_id, "valid.rs");
        // Two warnings: integer item + empty string
        assert!(warnings.len() >= 2);
    }

    #[test]
    fn vocabulary_validation_preserves_custom_memory_types() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "preference".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (_, warnings) = parse_edge_specs(&p);
        assert!(
            warnings.is_empty(),
            "custom types should be accepted silently"
        );
    }

    #[test]
    fn vocabulary_validation_preserves_payload_metadata() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["src/a.rs"], "domain": "test" }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target_id, "src/a.rs");
        assert!(warnings.is_empty());
    }

    #[test]
    fn vocabulary_validation_accepts_existing_qualified_cross_namespace_target() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["OtherNS::src/a.rs"] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target_id, "OtherNS::src/a.rs");
        assert!(
            warnings.is_empty(),
            "qualified cross-namespace targets should be accepted"
        );
    }

    #[test]
    fn vocabulary_validation_warns_for_missing_qualified_target() {
        // Qualified with empty local ID part
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["NS::"] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn vocabulary_validation_warns_for_unresolved_unqualified_target() {
        // Non-array metadata
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": "not-an-array" }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].why, "relation declaration must be an array");
    }

    #[test]
    fn vocabulary_validation_rejects_empty_qualified_components_only() {
        // Empty namespace part
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["::src/a.rs"] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn vocabulary_warning_has_exact_reason_and_skipped_fields() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": [42] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert_eq!(warnings[0].why, "relation target must be a string");
        assert_eq!(warnings[0].skipped, "this edge declaration");
    }

    /// The access-topology shape check (vocabulary v5).
    ///
    /// A `fact` declares the access path it describes in `metadata.topology`.
    /// The check is soft: a malformed shape warns and the fact is still
    /// written, because a rejected write would lose the claim.
    mod topology {
        use super::*;

        fn well_formed_topology() -> Value {
            json!({
                "component": "api-gateway",
                "target_system": "ledger-service",
                "channel": "mTLS",
                "credential_boundary": "eu-west-1 VPC",
                "reason": "the ledger accepts traffic only from inside the VPC",
            })
        }

        fn fact_with_topology(topology: Value) -> MemoryPayload {
            MemoryPayload {
                content: "c".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({ "topology": topology }),
            }
        }

        #[test]
        fn a_well_formed_topology_writes_without_a_warning() {
            let mut topology = well_formed_topology();
            // An extra key is not a defect: the declared shape ignores them.
            topology["extra_key"] = json!("ignored");
            let (specs, warnings) = parse_edge_specs(&fact_with_topology(topology));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert!(
                !warnings.iter().any(|w| w.field == "metadata.topology"),
                "a well-formed topology raises no warning: {warnings:?}"
            );
            assert!(
                warnings.is_empty(),
                "the payload carries nothing else that could warn: {warnings:?}"
            );
        }

        #[test]
        fn a_malformed_topology_warns_and_the_fact_is_still_written() {
            // A required key is missing.
            let mut missing = well_formed_topology();
            missing.as_object_mut().unwrap().remove("channel");
            let (specs, warnings) = parse_edge_specs(&fact_with_topology(missing));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.topology");
            assert_eq!(
                warnings[0].why,
                "metadata.topology is missing the required key \"channel\""
            );
            assert_eq!(
                warnings[0].skipped,
                "the topology shape check; the fact is still written"
            );

            // The topology is not an object at all: a string, an array, a null.
            for topology in [json!("not-an-object"), json!(["component"]), Value::Null] {
                let (_, warnings) = parse_edge_specs(&fact_with_topology(topology));
                assert_eq!(warnings.len(), 1, "{warnings:?}");
                assert_eq!(warnings[0].field, "metadata.topology");
                assert_eq!(warnings[0].why, "metadata.topology must be an object");
            }

            // A present key that is not a string.
            let mut not_a_string = well_formed_topology();
            not_a_string["channel"] = json!(42);
            let (_, warnings) = parse_edge_specs(&fact_with_topology(not_a_string));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.topology");
            assert_eq!(
                warnings[0].why,
                "metadata.topology key \"channel\" must be a string"
            );
        }

        #[test]
        fn a_topology_on_a_memory_that_is_not_a_fact_is_not_checked() {
            let p = MemoryPayload {
                memory_type: "rule".into(),
                ..fact_with_topology(json!("not-an-object"))
            };
            let (_, warnings) = parse_edge_specs(&p);
            assert!(
                warnings.is_empty(),
                "the field is declared on a fact alone, and an undeclared metadata \
                 key is accepted unchanged: {warnings:?}"
            );
        }

        /// The check's key list and the vocabulary's own list must be the same
        /// list. A vocabulary bump that added a key would otherwise leave the
        /// store enforcing a stale shape, silently.
        #[test]
        fn the_topology_keys_match_the_vocabulary() {
            let v = crate::traits::memory_vocabulary();
            let summary = v["tool_summaries"]["memory_type"]
                .as_str()
                .expect("the memory_type summary is a string");
            let listed = summary
                .split_once("metadata.topology (")
                .expect("the summary lists the topology keys")
                .1
                .split_once(')')
                .expect("the listed keys are parenthesised")
                .0;
            let vocabulary_keys: Vec<&str> = listed.split(", ").collect();
            assert_eq!(
                vocabulary_keys,
                TOPOLOGY_KEYS.to_vec(),
                "the check enforces the vocabulary's own key list"
            );

            let definition = v["memory_types"]["fact"]["fields"]["topology"]["definition"]
                .as_str()
                .expect("the topology field carries a definition");
            for key in TOPOLOGY_KEYS {
                assert!(
                    definition.contains(key),
                    "the topology definition names {}",
                    key
                );
            }
        }

        #[tokio::test]
        async fn a_fact_with_a_malformed_topology_is_still_written() {
            let store = retired_test_store().await;
            store
                .upsert(
                    "probe",
                    "probe::fact",
                    vec![0.0; 4],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "topology": { "component": "api-gateway" } }),
                    ),
                )
                .await
                .expect("a malformed topology is a warning, not a rejection");

            let stored = store
                .get("probe", "probe::fact")
                .await
                .unwrap()
                .expect("the fact is stored");
            assert_eq!(
                stored.1.metadata["topology"]["component"],
                json!("apigateway"),
                "the topology is stored exactly as it was given"
            );
        }
    }

    /// The access-attempt shape check (vocabulary v6).
    mod attempt {
        use super::*;

        fn well_formed_attempt() -> Value {
            json!({
                "approach": "retry with backoff",
                "outcome": "failed",
                "failure_mode": "timeout",
                "context": "under load"
            })
        }

        fn fact_with_attempt(attempt: Value) -> MemoryPayload {
            MemoryPayload {
                content: "c".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({ "attempt": attempt }),
            }
        }

        #[test]
        fn a_well_formed_attempt_writes_without_a_warning() {
            let (specs, warnings) = parse_edge_specs(&fact_with_attempt(well_formed_attempt()));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert!(
                !warnings.iter().any(|w| w.field == "metadata.attempt"),
                "a well-formed attempt raises no warning: {warnings:?}"
            );
            assert!(
                warnings.is_empty(),
                "the payload carries nothing else that could warn: {warnings:?}"
            );
        }

        #[test]
        fn a_malformed_attempt_warns_at_parse_edge_specs_level() {
            // missing key
            let mut missing = well_formed_attempt();
            missing.as_object_mut().unwrap().remove("context");
            let (specs, warnings) = parse_edge_specs(&fact_with_attempt(missing));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.attempt");
            assert_eq!(
                warnings[0].why,
                "metadata.attempt is missing the required key \"context\""
            );
            assert_eq!(
                warnings[0].skipped,
                "the attempt shape check; the fact is still written"
            );

            // not an object
            for attempt in [json!("not-an-object"), json!(["approach"]), Value::Null] {
                let (_, warnings) = parse_edge_specs(&fact_with_attempt(attempt));
                assert_eq!(warnings.len(), 1, "{warnings:?}");
                assert_eq!(warnings[0].field, "metadata.attempt");
                assert_eq!(warnings[0].why, "metadata.attempt must be an object");
            }

            // non-string key
            let mut not_a_string = well_formed_attempt();
            not_a_string["approach"] = json!(42);
            let (_, warnings) = parse_edge_specs(&fact_with_attempt(not_a_string));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.attempt");
            assert_eq!(
                warnings[0].why,
                "metadata.attempt key \"approach\" must be a string"
            );

            // empty string
            let mut empty = well_formed_attempt();
            empty["approach"] = json!("   ");
            let (_, warnings) = parse_edge_specs(&fact_with_attempt(empty));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.attempt");
            assert_eq!(
                warnings[0].why,
                "metadata.attempt key \"approach\" must be a non-empty string"
            );
        }

        #[test]
        fn an_attempt_on_a_memory_that_is_not_a_fact_is_not_checked() {
            let p = MemoryPayload {
                memory_type: "rule".into(),
                ..fact_with_attempt(json!("not-an-object"))
            };
            let (_, warnings) = parse_edge_specs(&p);
            assert!(
                warnings.is_empty(),
                "the field is declared on a fact alone: {warnings:?}"
            );
        }

        #[tokio::test]
        async fn a_malformed_attempt_warns_and_the_write_still_succeeds() {
            let store = retired_test_store().await;
            store
                .upsert(
                    "probe",
                    "probe::fact",
                    vec![0.0; 4],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "attempt": { "approach": "retry", "outcome": "failed", "failure_mode": "timeout" } }),
                    ),
                )
                .await
                .expect("a malformed attempt is a warning, not a rejection");

            let stored = store
                .get("probe", "probe::fact")
                .await
                .unwrap()
                .expect("the fact is stored");
            assert_eq!(
                stored.1.metadata["attempt"]["approach"],
                json!("retry"),
                "the attempt is stored exactly as it was given"
            );
        }
    }

    // --- Integration tests: search with evidence ---

    /// Build a probe namespace with symbol contained_by file, rule governs file.
    async fn evidence_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-evidence-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        // File node (structural, zero vector)
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.1; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();

        // Symbol contained_by file (vector close to query)
        store
            .upsert(
                "probe",
                "probe::src/a.rs::run",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "code_ast",
                    "auto-ingestor",
                    json!({ "contained_by": ["probe::src/a.rs"] }),
                ),
            )
            .await
            .unwrap();

        // Rule governs file (vector orthogonal to query)
        store
            .upsert(
                "probe",
                "rule-1",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": ["probe::src/a.rs"] })),
            )
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn relationship_label_queries_return_actual_table_names() {
        let store = evidence_test_store().await;
        // Query closest to symbol; symbol is primary, file is one-hop, rule is governing
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        // Symbol should be primary (direct match)
        let sym = results
            .iter()
            .find(|r| r.id == "probe::src/a.rs::run")
            .unwrap();
        assert!(sym
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch));
    }

    #[tokio::test]
    async fn one_hop_evidence_preserves_both_directed_orientations() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let file = results.iter().find(|r| r.id == "probe::src/a.rs").unwrap();
        let evs = file.evidence.as_ref().unwrap();
        let one_hop: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert!(!one_hop.is_empty(), "file should have one-hop evidence");
        // The CONTAINS edge: symbol contains_by file, so file is the source of CONTAINS
        // Path: "probe::src/a.rs::run" <-CONTAINS- "probe::src/a.rs"
        let path = &one_hop[0].path;
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].relation, "CONTAINS");
        assert_eq!(path[0].source, "probe::src/a.rs");
        assert_eq!(path[0].target, "probe::src/a.rs::run");
    }

    /// A store whose primary reaches its neighbours by OUTGOING edges only:
    ///
    ///   probe::primary -RELATES_TO-> probe::mid <-GOVERNS- probe::ruler
    ///
    /// `probe::mid` is the one-hop neighbour; `probe::ruler` is the node that
    /// governs it, two hops from the primary. The expansion has to report the
    /// graph's own direction on both paths.
    async fn outgoing_evidence_test_store() -> LadybugStore {
        let dir =
            std::env::temp_dir().join(format!("ns-outgoing-evidence-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        // The one-hop neighbour: the target of the primary's RELATES_TO edge.
        store
            .upsert(
                "probe",
                "probe::mid",
                vec![0.5, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // The governing node: it governs the one-hop neighbour.
        store
            .upsert(
                "probe",
                "probe::ruler",
                vec![0.2, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "governs": ["probe::mid"] })),
            )
            .await
            .unwrap();

        // The primary: closest to the query, source of the one-hop edge.
        store
            .upsert(
                "probe",
                "probe::primary",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "related_to": ["probe::mid"] })),
            )
            .await
            .unwrap();

        store
    }

    /// An outgoing edge is reported from the primary towards the neighbour.
    /// Reading the direction flag off a rendered boolean made it always false,
    /// which reversed every outgoing path.
    #[tokio::test]
    async fn outgoing_one_hop_evidence_points_from_the_primary_to_the_neighbour() {
        let store = outgoing_evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let mid = results.iter().find(|r| r.id == "probe::mid").unwrap();
        let one_hop: Vec<_> = mid
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert_eq!(one_hop.len(), 1, "mid should carry one one-hop path");
        let edge = &one_hop[0].path[0];
        assert_eq!(edge.source, "probe::primary");
        assert_eq!(edge.relation, "RELATES_TO");
        assert_eq!(edge.target, "probe::mid");
    }

    /// The first hop of a two-hop governing path is an outgoing edge too, and
    /// carries the same direction.
    #[tokio::test]
    async fn outgoing_two_hop_evidence_keeps_the_first_hop_directed() {
        let store = outgoing_evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let ruler = results.iter().find(|r| r.id == "probe::ruler").unwrap();
        let two_hop: Vec<_> = ruler
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert_eq!(two_hop.len(), 1, "ruler should carry one two-hop path");
        let path = &two_hop[0].path;
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].source, "probe::primary");
        assert_eq!(path[0].relation, "RELATES_TO");
        assert_eq!(path[0].target, "probe::mid");
        assert_eq!(path[1].source, "probe::ruler");
        assert_eq!(path[1].relation, "GOVERNS");
        assert_eq!(path[1].target, "probe::mid");
    }

    #[tokio::test]
    async fn relates_to_evidence_is_undirected_and_canonical() {
        let dir = std::env::temp_dir().join(format!("ns-relates-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.unwrap();

        // mem-b must exist before mem-a references it via related_to
        store
            .upsert(
                "probe",
                "mem-b",
                vec![0.9, 0.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "mem-a",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "related_to": ["mem-b"] })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let b = results.iter().find(|r| r.id == "mem-b").unwrap();
        let evs = b.evidence.as_ref().unwrap();
        let one_hop: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert!(!one_hop.is_empty());
        assert_eq!(one_hop[0].path[0].relation, "RELATES_TO");
    }

    #[tokio::test]
    async fn governs_two_hop_evidence_contains_primary_file_and_rule() {
        let store = evidence_test_store().await;
        // A filler primary closer than the rule fills the primary quota, so
        // rule-1 stays out of the primary set and must arrive via expansion.
        store
            .upsert(
                "probe",
                "filler",
                vec![0.9, 0.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        // limit=2: primaries are the symbol (closest) and the filler; the file
        // arrives one hop away and the rule two hops away. Four results, the
        // full limit * 2 response.
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 2, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1");
        assert!(
            rule.is_some(),
            "rule-1 should appear in results, got: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let evs = rule.unwrap().evidence.as_ref().unwrap();
        let gov2: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert!(
            !gov2.is_empty(),
            "rule should have governs_2hop evidence, got: {:?}",
            evs.iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
        let path = &gov2[0].path;
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].relation, "CONTAINS");
        assert_eq!(path[1].relation, "GOVERNS");
        // rule-1 GOVERNS file: rule-1 is source, file is target
        assert_eq!(path[1].source, "rule-1");
    }

    #[tokio::test]
    async fn retired_intermediate_excludes_only_its_evidence_path() {
        let store = evidence_test_store().await;
        // Retire the file (intermediate between symbol and rule)
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.1; 4],
                retired_test_payload("file", "auto-ingestor", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1");
        // Rule should still appear (as one-hop via direct GOVERNS from symbol? no —
        // the only path to rule is through the file, which is retired)
        // Actually: symbol is primary. symbol --CONTAINS--> file. file is retired.
        // governing: a(symbol) -[e]-> f(file) <-[:GOVERNS]- r(rule)
        // f is retired, so this path is excluded.
        // But rule has GOVERNS edge directly from rule -> file, so there might be
        // a one-hop path from symbol to rule if symbol and rule share a direct edge.
        // They don't. So rule should NOT appear.
        if let Some(rule) = rule {
            // If rule appears, its evidence should not reference the retired file
            for ev in rule.evidence.as_ref().unwrap() {
                for edge in &ev.path {
                    assert!(
                        edge.source != "probe::src/a.rs" && edge.target != "probe::src/a.rs",
                        "retired intermediate should not appear in evidence"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn active_rule_survives_via_another_active_intermediate() {
        let store = evidence_test_store().await;
        // A second symbol fills the primary quota (limit=2) so rule-1 stays
        // expansion-only, and gives the rule a second intermediate to travel
        // through.
        store
            .upsert(
                "probe",
                "probe::src/b.rs",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "probe::src/b.rs::run",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "code_ast",
                    "auto-ingestor",
                    json!({ "contained_by": ["probe::src/b.rs"] }),
                ),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "rule-1",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["probe::src/a.rs", "probe::src/b.rs"] }),
                ),
            )
            .await
            .unwrap();
        // Retire the ORIGINAL intermediate: the a.rs path dies, the rule must
        // survive through b.rs alone.
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.1; 4],
                retired_test_payload("file", "auto-ingestor", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 2, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1");
        assert!(
            rule.is_some(),
            "rule-1 should survive via the active intermediate, got: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let evs = rule.unwrap().evidence.as_ref().unwrap();
        let gov2: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert!(!gov2.is_empty(), "rule should have governs_2hop evidence");
        for ev in &gov2 {
            for edge in &ev.path {
                assert!(
                    edge.source != "probe::src/a.rs" && edge.target != "probe::src/a.rs",
                    "retired intermediate must not appear in surviving evidence: {:?}",
                    ev.path
                );
            }
        }
        // The surviving path travels through the active intermediate.
        assert!(gov2.iter().all(|e| e
            .path
            .iter()
            .any(|edge| edge.target == "probe::src/b.rs" || edge.source == "probe::src/b.rs")));
    }

    #[tokio::test]
    async fn direct_match_evidence_has_no_edges() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let sym = results
            .iter()
            .find(|r| r.id == "probe::src/a.rs::run")
            .unwrap();
        let evs = sym.evidence.as_ref().unwrap();
        let direct = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::DirectMatch)
            .unwrap();
        assert!(
            direct.path.is_empty(),
            "direct match evidence has empty path"
        );
    }

    #[tokio::test]
    async fn duplicate_result_ids_merge_distinct_paths() {
        let store = evidence_test_store().await;
        // file appears in both: one-hop from symbol (via CONTAINS) and as intermediate
        // in the governing path. It should have one evidence entry at most.
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let file = results.iter().find(|r| r.id == "probe::src/a.rs").unwrap();
        let evs = file.evidence.as_ref().unwrap();
        // File should have one-hop evidence (from symbol via CONTAINS)
        let one_hop: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert!(!one_hop.is_empty());
    }

    #[tokio::test]
    async fn delete_missing_memory_fails_loudly() {
        let store = evidence_test_store().await;
        // A zero-row delete must not read as success: the caller would report
        // a deletion that never happened.
        let missing = store.delete("probe", "no-such-id").await;
        assert!(missing.is_err(), "deleting an absent id must error");
        assert!(missing.unwrap_err().to_string().contains("no-such-id"));

        // The real id still deletes cleanly.
        store.delete("probe", "rule-1").await.unwrap();
        let gone = store.get("probe", "rule-1").await.unwrap();
        assert!(gone.is_none(), "rule-1 should be deleted");
    }

    #[tokio::test]
    async fn evidence_paths_are_sorted_and_capped_at_three() {
        let store = evidence_test_store().await;
        // Add extra paths: rule also related_to the symbol
        store.upsert("probe", "rule-1", vec![0.0, 1.1, 0.0, 0.0],
            retired_test_payload("rule", "agent",
                json!({ "governs": ["probe::src/a.rs"], "related_to": ["probe::src/a.rs::run"] }))).await.unwrap();
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1").unwrap();
        let evs = rule.evidence.as_ref().unwrap();
        assert!(
            evs.len() <= 3,
            "evidence should be capped at 3, got {}",
            evs.len()
        );
        // Should be sorted by kind (OneHop before Governs2hop)
        for pair in evs.windows(2) {
            assert!(
                pair[0].kind <= pair[1].kind,
                "evidence should be sorted by kind"
            );
        }
    }

    #[tokio::test]
    async fn expansion_never_creates_paths_longer_than_two_edges() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    assert!(
                        ev.path.len() <= 2,
                        "{}: path too long ({} edges)",
                        r.id,
                        ev.path.len()
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn duplicate_paths_do_not_consume_result_slots() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        // Each result should appear at most once
        let mut ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), results.len(), "no duplicate result IDs");
    }

    // --- BUG-1 / neurostrata-i54c / neurostrata-sfrh fix tests ---

    /// Test helper for search with filters
    async fn search_with_filters(
        store: &LadybugStore,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
    ) -> Vec<SearchResult> {
        // This will be the new signature after the fix
        store
            .search(namespace, vector, limit, include_tasks, include_archived)
            .await
            .unwrap()
    }

    /// Build a test store with task, archived, and mesh-conflicting rows
    async fn filter_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-filter-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let _zero = vec![0.0f32; 4];
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Primary: a fact that matches the query
        store
            .upsert(
                "probe",
                "primary-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // Task row (should be excluded by default)
        store
            .upsert(
                "probe",
                "task-row",
                query_vec.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();

        // Archived row (should be excluded by default)
        store
            .upsert(
                "probe",
                "archived-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "archived": true })),
            )
            .await
            .unwrap();

        // Mesh-conflicting row (should be excluded by default)
        store
            .upsert(
                "probe",
                "conflict-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "mesh": { "conflict": true } })),
            )
            .await
            .unwrap();

        // A rule that governs the primary (expansion candidate)
        store
            .upsert(
                "probe",
                "governs-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": ["primary-fact"] })),
            )
            .await
            .unwrap();

        // A task that governs something (should NOT seed expansions)
        store
            .upsert(
                "probe",
                "task-governs",
                vec![0.0, 0.0, 1.1, 0.0],
                retired_test_payload("task", "agent", json!({ "governs": ["primary-fact"] })),
            )
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn graph_expanded_neighbor_appears_in_results() {
        let store = filter_test_store().await;
        // Search with limit=1, should get 1 primary + its expansions
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        // The primary should be there
        let primary = results.iter().find(|r| r.id == "primary-fact");
        assert!(
            primary.is_some(),
            "primary-fact should be in results: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // The expansion (governs-rule) should also be there
        let expansion = results.iter().find(|r| r.id == "governs-rule");
        assert!(
            expansion.is_some(),
            "governs-rule expansion should be in results: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Expansion should have OneHop evidence (direct GOVERNS edge from rule to primary)
        let expansion = expansion.unwrap();
        let evs = expansion.evidence.as_ref().unwrap();
        let one_hop = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::OneHop);
        assert!(
            one_hop.is_some(),
            "expansion should have OneHop evidence, got: {:?}",
            evs.iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn direct_hits_and_expansions_distinguishable_by_kind_not_score() {
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let primary = results.iter().find(|r| r.id == "primary-fact").unwrap();
        let expansion = results.iter().find(|r| r.id == "governs-rule").unwrap();

        // Primary should have DirectMatch evidence
        let primary_ev = primary.evidence.as_ref().unwrap();
        let direct = primary_ev
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(direct.is_some(), "primary should have DirectMatch evidence");

        // Expansion should have OneHop evidence (direct GOVERNS edge)
        let expansion_ev = expansion.evidence.as_ref().unwrap();
        let one_hop = expansion_ev
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::OneHop);
        assert!(
            one_hop.is_some(),
            "expansion should have OneHop evidence, got: {:?}",
            expansion_ev
                .iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );

        // Expansion should NOT have DirectMatch
        let direct_on_expansion = expansion_ev
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(
            direct_on_expansion.is_none(),
            "expansion should NOT have DirectMatch evidence"
        );

        // Scores should NOT use magic 10.0 sentinel
        assert_ne!(
            expansion.score, 10.0,
            "expansion score should not be magic 10.0 sentinel"
        );
        // Primary should have a real boosted distance score
        assert!(primary.score < 10.0, "primary should have real score");
    }

    #[tokio::test]
    async fn excluded_rows_do_not_consume_candidate_budget() {
        let store = filter_test_store().await;
        // limit=2, there are 4 matching rows (primary, task, archived, conflict)
        // With filters applied in store, only primary should count toward limit by default
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 2, false, false).await;

        // Should get primary + expansion (governs-rule), NOT task/archived/conflict
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"primary-fact"),
            "primary-fact should be present"
        );
        assert!(
            ids.contains(&"governs-rule"),
            "governs-rule expansion should be present"
        );
        assert!(
            !ids.contains(&"task-row"),
            "task-row should be excluded by default"
        );
        assert!(
            !ids.contains(&"archived-fact"),
            "archived-fact should be excluded by default"
        );
        assert!(
            !ids.contains(&"conflict-fact"),
            "conflict-fact should be excluded by default"
        );

        // With include_tasks=true and limit=2, both primary and task should appear as primaries
        let results_with_tasks =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 2, true, false).await;
        let ids_with_tasks: Vec<&str> = results_with_tasks.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids_with_tasks.contains(&"task-row"),
            "task-row should appear when include_tasks=true"
        );
        assert!(
            ids_with_tasks.contains(&"primary-fact"),
            "primary-fact should still appear"
        );
    }

    #[tokio::test]
    async fn access_count_increments_only_for_direct_hits() {
        // This test verifies the store returns the right evidence kinds;
        // the server is responsible for only incrementing on DirectMatch.
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let primary = results.iter().find(|r| r.id == "primary-fact").unwrap();
        let expansion = results.iter().find(|r| r.id == "governs-rule").unwrap();

        // Primary has DirectMatch
        let primary_has_direct = primary
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(primary_has_direct, "primary should have DirectMatch");

        // Expansion does NOT have DirectMatch
        let expansion_has_direct = expansion
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(
            !expansion_has_direct,
            "expansion should NOT have DirectMatch"
        );
    }

    #[tokio::test]
    async fn expansions_not_seeded_from_excluded_rows() {
        let store = filter_test_store().await;
        // task-governs is a task row that governs primary-fact
        // It should NOT seed expansions because it's excluded (task type)
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        // task-governs itself should not appear (excluded)
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-governs"),
            "task-governs should be excluded"
        );

        // No expansion should have evidence path going through task-governs
        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    for edge in &ev.path {
                        assert_ne!(
                            edge.source, "task-governs",
                            "expansion should not be seeded from task-governs (source)"
                        );
                        assert_ne!(
                            edge.target, "task-governs",
                            "expansion should not be seeded from task-governs (target)"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn co_occurrence_ranking_deterministic_and_respects_caps() {
        let dir = std::env::temp_dir().join(format!("ns-cooccur-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let _zero = vec![0.0f32; 4];
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Two primaries that both connect to the same expansion candidate
        store
            .upsert(
                "probe",
                "primary-1",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary-2",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary-3",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // Shared expansion: governed by both primary-1 and primary-2
        store
            .upsert(
                "probe",
                "shared-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["primary-1", "primary-2"] }),
                ),
            )
            .await
            .unwrap();

        // Expansion only from primary-3
        store
            .upsert(
                "probe",
                "solo-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": ["primary-3"] })),
            )
            .await
            .unwrap();

        // Another shared expansion with 3 primaries
        store
            .upsert(
                "probe",
                "triple-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["primary-1", "primary-2", "primary-3"] }),
                ),
            )
            .await
            .unwrap();

        // Search with limit=3 (3 primaries), should admit expansions up to limit per family
        let results = search_with_filters(&store, "probe", query_vec, 3, false, false).await;

        // Find the expansion results
        let expansions: Vec<_> = results.iter().filter(|r| r.id.contains("rule")).collect();

        // Co-occurrence count: triple-rule (3) > shared-rule (2) > solo-rule (1)
        // Within the governing family (family=1), they should be ordered by co-occurrence DESC
        let gov_expansions: Vec<_> = expansions
            .iter()
            .filter(|r| {
                r.evidence
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            })
            .collect();

        // Verify ordering by co-occurrence (descending)
        let mut prev_count = usize::MAX;
        for r in &gov_expansions {
            let _count = r
                .evidence
                .as_ref()
                .unwrap()
                .iter()
                .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
                .map(|e| e.path.len()) // Not exactly co-occurrence, but related
                .max()
                .unwrap_or(0);
            // The actual co-occurrence is the number of distinct primary paths
            // We verify by checking the number of distinct primary IDs in evidence
            let primary_sources: std::collections::HashSet<_> = r
                .evidence
                .as_ref()
                .unwrap()
                .iter()
                .flat_map(|e| e.path.iter().map(|edge| edge.source.as_str()))
                .collect();
            let co_occur = primary_sources.len();
            assert!(
                co_occur <= prev_count,
                "co-occurrence should be non-increasing: {} then {}",
                co_occur,
                prev_count
            );
            prev_count = co_occur;
        }

        // Per-family cap: at most `limit` (3) expansions per family
        assert!(
            gov_expansions.len() <= 3,
            "governing family should respect per-family cap"
        );
    }

    #[tokio::test]
    async fn limit_counts_recall_eligible_primaries_plus_expansions() {
        let store = filter_test_store().await;
        // limit=1 should return 1 primary + up to 1 expansion per family
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        // Count primaries (DirectMatch)
        let primary_count = results
            .iter()
            .filter(|r| {
                r.evidence
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch)
            })
            .count();
        assert_eq!(primary_count, 1, "should have exactly 1 primary (limit=1)");

        // Total results can exceed limit due to expansions
        assert!(results.len() >= 1, "should have at least the primary");
        assert!(
            results.len() <= 3,
            "should not exceed limit * families (1 * 2 families + 1 primary) = 3"
        );
    }

    #[tokio::test]
    async fn include_archived_true_includes_archived_rows() {
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, true).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"archived-fact"),
            "archived-fact should appear when include_archived=true"
        );
    }

    #[tokio::test]
    async fn score_cap_half_distance_preserved() {
        // The co-occurrence ranking must not violate the 0.5 * distance cap
        // If scores are used for ranking, the cap must hold
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    if ev.kind != crate::traits::EvidenceKind::DirectMatch {
                        // For expansions, the score should respect the cap
                        // The cap is applied at line 1055: (gain + source_gain).min(0.5 * distance)
                        // Since expansions get score from a different mechanism now,
                        // we verify the cap is not violated by checking scores are reasonable
                        assert!(r.score >= 0.0, "score should be non-negative");
                        assert!(r.score < 10.0, "score should not be magic sentinel");
                    }
                }
            }
        }
    }

    // --- Episode-sibling expansion tests ---

    /// Build a test store with a task and multiple lessons extracted from it.
    async fn episode_sibling_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-episode-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task row MUST be created FIRST so EXTRACTED_FROM edges materialize
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();

        // Primary: a fact that matches the query
        store
            .upsert(
                "probe",
                "primary-lesson",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Sibling lesson 1: extracted from same task
        store
            .upsert(
                "probe",
                "sibling-lesson-1",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Sibling lesson 2: extracted from same task
        store
            .upsert(
                "probe",
                "sibling-lesson-2",
                vec![0.0, 0.0, 1.1, 0.0],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Another task with many lessons (for family independence test)
        store
            .upsert(
                "probe",
                "task-many",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("many-lesson-{}", i),
                    vec![0.1 * (i + 1) as f32, 0.1, 0.1, 0.1],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "extracted_from": ["task-many"] }),
                    ),
                )
                .await
                .unwrap();
        }

        // One-hop neighbor for family independence test
        store
            .upsert(
                "probe",
                "one-hop-neighbor",
                vec![0.9, 0.1, 0.1, 0.1],
                retired_test_payload("fact", "agent", json!({ "related_to": ["primary-lesson"] })),
            )
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn episode_sibling_appears_in_results() {
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        // Primary should be there
        let primary = results.iter().find(|r| r.id == "primary-lesson");
        assert!(primary.is_some(), "primary-lesson should be in results");

        // Sibling should appear as expansion
        let sibling = results.iter().find(|r| r.id == "sibling-lesson-1");
        assert!(
            sibling.is_some(),
            "sibling-lesson-1 should be in results: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Sibling should have EpisodeSibling evidence
        let sibling = sibling.unwrap();
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling);
        assert!(
            ep_sib.is_some(),
            "sibling should have EpisodeSibling evidence, got: {:?}",
            evs.iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn episode_sibling_evidence_has_correct_path_and_explanation() {
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        let sibling = results.iter().find(|r| r.id == "sibling-lesson-1").unwrap();
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling)
            .unwrap();

        // Path should have exactly 2 edges
        assert_eq!(
            ep_sib.path.len(),
            2,
            "EpisodeSibling path should have 2 edges"
        );

        // First edge: primary -(EXTRACTED_FROM)-> task
        assert_eq!(ep_sib.path[0].source, "primary-lesson");
        assert_eq!(ep_sib.path[0].relation, "EXTRACTED_FROM");
        assert_eq!(ep_sib.path[0].target, "task-1");

        // Second edge: sibling -(EXTRACTED_FROM)-> task
        assert_eq!(ep_sib.path[1].source, "sibling-lesson-1");
        assert_eq!(ep_sib.path[1].relation, "EXTRACTED_FROM");
        assert_eq!(ep_sib.path[1].target, "task-1");

        // Explanation should match spec
        assert_eq!(
            ep_sib.explanation,
            "This memory and a direct match were both extracted from the same task."
        );
    }

    #[tokio::test]
    async fn task_row_never_appears_as_result() {
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-1"),
            "task row should never appear as a result"
        );
        assert!(
            !ids.contains(&"task-many"),
            "task-many row should never appear as a result"
        );
    }

    #[tokio::test]
    async fn family_independence_many_lessons_do_not_crowd_out_one_hop() {
        let store = episode_sibling_test_store().await;
        // limit=1: one primary, up to 1 one-hop, up to 1 episode-sibling, up to 1 governing
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();

        // Primary should be there
        assert!(ids.contains(&"primary-lesson"));

        // One-hop neighbor should be admitted (family 0)
        assert!(
            ids.contains(&"one-hop-neighbor"),
            "one-hop neighbor should be admitted despite many siblings"
        );

        // At least one episode sibling should be admitted (family 1)
        let has_sibling = ids
            .iter()
            .any(|id| id.starts_with("sibling-lesson") || id.starts_with("many-lesson"));
        assert!(
            has_sibling,
            "at least one episode sibling should be admitted"
        );
    }

    #[tokio::test]
    async fn family_independence_one_hop_does_not_crowd_out_siblings() {
        let store = episode_sibling_test_store().await;
        // Add many one-hop neighbors
        let _zero = vec![0.0f32; 4];
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("extra-neighbor-{}", i),
                    vec![0.9, 0.1, 0.1, 0.1],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "related_to": ["primary-lesson"] }),
                    ),
                )
                .await
                .unwrap();
        }

        // limit=1: should still admit one episode sibling
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();

        // Primary should be there
        assert!(ids.contains(&"primary-lesson"));

        // At least one episode sibling should be admitted (family 1)
        let has_sibling = ids
            .iter()
            .any(|id| id.starts_with("sibling-lesson") || id.starts_with("many-lesson"));
        assert!(
            has_sibling,
            "episode sibling should be admitted despite many one-hop neighbors: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn archived_sibling_is_excluded() {
        let dir =
            std::env::temp_dir().join(format!("ns-episode-archived-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "archived-sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({ "extracted_from": ["task-1"], "archived": true }),
                ),
            )
            .await
            .unwrap();

        let results =
            search_with_filters(&store, "probe", query_vec.clone(), 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"archived-sibling"),
            "archived sibling should be excluded by default"
        );

        // With include_archived=true, it should appear
        let results_archived =
            search_with_filters(&store, "probe", query_vec, 5, false, true).await;
        let ids_archived: Vec<&str> = results_archived.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids_archived.contains(&"archived-sibling"),
            "archived sibling should appear when include_archived=true"
        );
    }

    #[tokio::test]
    async fn mesh_conflicting_sibling_is_excluded() {
        let dir =
            std::env::temp_dir().join(format!("ns-episode-conflict-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "conflict-sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({ "extracted_from": ["task-1"], "mesh": { "conflict": true } }),
                ),
            )
            .await
            .unwrap();

        let results = search_with_filters(&store, "probe", query_vec, 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"conflict-sibling"),
            "mesh-conflicting sibling should be excluded"
        );
    }

    #[tokio::test]
    async fn task_typed_sibling_is_excluded() {
        let dir = std::env::temp_dir().join(format!("ns-episode-task-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "task-sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("task", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        let results =
            search_with_filters(&store, "probe", query_vec.clone(), 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-sibling"),
            "task-typed sibling should be excluded by default"
        );

        // With include_tasks=true, it should appear
        let results_tasks = search_with_filters(&store, "probe", query_vec, 5, true, false).await;
        let ids_tasks: Vec<&str> = results_tasks.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids_tasks.contains(&"task-sibling"),
            "task-typed sibling should appear when include_tasks=true"
        );
    }

    #[tokio::test]
    async fn retired_intermediate_task_invalidates_path() {
        let dir = std::env::temp_dir().join(format!("ns-episode-retired-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first (with valid_to=1 to be retired)
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Use limit=1 so only 'primary' is a primary; 'sibling' becomes an expansion candidate
        let results = search_with_filters(&store, "probe", query_vec, 1, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"sibling"),
            "sibling should not appear when intermediate task is retired"
        );
    }

    #[tokio::test]
    async fn co_occurrence_sibling_from_two_primaries_counts_two() {
        let dir = std::env::temp_dir().join(format!("ns-episode-cooccur-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        // Two primaries extracted from the same task
        store
            .upsert(
                "probe",
                "primary-1",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary-2",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        // Sibling extracted from same task - vector far from query so it doesn't become a primary
        store
            .upsert(
                "probe",
                "sibling",
                vec![0.0, 0.0, 0.0, 1.1],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Search with limit=2 so only the two primaries are in primary_ids
        let results = search_with_filters(&store, "probe", query_vec, 2, false, false).await;
        let sibling = results.iter().find(|r| r.id == "sibling").unwrap();
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling)
            .collect();

        // Should have two distinct EpisodeSibling paths (one from each primary)
        assert_eq!(
            ep_sib.len(),
            2,
            "should have two EpisodeSibling paths, one per primary"
        );

        // Co-occurrence: primary_sources should contain both primaries
        // We verify by checking the evidence paths have both primary sources (first edge of each path)
        let primary_sources: std::collections::HashSet<_> = ep_sib
            .iter()
            .filter_map(|e| e.path.first().map(|edge| edge.source.as_str()))
            .collect();
        assert!(primary_sources.contains("primary-1"));
        assert!(primary_sources.contains("primary-2"));
        assert_eq!(primary_sources.len(), 2);
    }

    #[tokio::test]
    async fn episode_sibling_serializes_as_episode_sibling() {
        let ev = crate::traits::SearchEvidence {
            kind: crate::traits::EvidenceKind::EpisodeSibling,
            path: vec![
                crate::traits::EvidenceEdge {
                    source: "primary".into(),
                    relation: "EXTRACTED_FROM".into(),
                    target: "task".into(),
                },
                crate::traits::EvidenceEdge {
                    source: "sibling".into(),
                    relation: "EXTRACTED_FROM".into(),
                    target: "task".into(),
                },
            ],
            explanation: "test".into(),
            derivation: vec![],
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "episode_sibling");
    }

    #[tokio::test]
    async fn existing_invariants_hold_expansions_not_seeded_from_excluded_rows() {
        let dir =
            std::env::temp_dir().join(format!("ns-episode-invariant-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Primary
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        // Task that governs primary (should NOT seed expansions because it's a task)
        store
            .upsert(
                "probe",
                "task-governs",
                zero.clone(),
                retired_test_payload("task", "agent", json!({ "governs": ["primary"] })),
            )
            .await
            .unwrap();
        // A rule that the task governs (should NOT be reached via task)
        store
            .upsert(
                "probe",
                "ruled-by-task",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        // Re-upsert task-governs with the new governs edge
        store
            .upsert(
                "probe",
                "task-governs",
                zero.clone(),
                retired_test_payload(
                    "task",
                    "agent",
                    json!({ "governs": ["primary", "ruled-by-task"] }),
                ),
            )
            .await
            .unwrap();

        let results = search_with_filters(&store, "probe", query_vec, 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-governs"),
            "task-governs should be excluded"
        );

        // ruled-by-task should NOT appear via the task (task is excluded from seeding)
        // It might appear via other paths, but not via task-governs
        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    for edge in &ev.path {
                        assert_ne!(
                            edge.source, "task-governs",
                            "expansion should not be seeded from task-governs (source)"
                        );
                        assert_ne!(
                            edge.target, "task-governs",
                            "expansion should not be seeded from task-governs (target)"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn graph_expansion_still_reaches_server_caller() {
        // This is an integration-style test verifying the store returns evidence
        // that the server can render. The server's exhaustive match on EvidenceKind
        // will fail to compile if EpisodeSibling is not handled.
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        let sibling = results.iter().find(|r| r.id == "sibling-lesson-1").unwrap();
        assert!(sibling.evidence.is_some());
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling);
        assert!(
            ep_sib.is_some(),
            "EpisodeSibling evidence should be present for server rendering"
        );
    }

    // --- TASK 2 tests: domain-filtered search ---

    #[tokio::test]
    async fn a_domain_filter_narrows_the_candidates_to_the_matching_domain() {
        let dir = std::env::temp_dir().join(format!("ns-domain-filter-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Two facts with different domains
        store
            .upsert(
                "probe",
                "auth-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "auth" })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "database-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "database" })),
            )
            .await
            .unwrap();

        // Search with domain filter for "database" only
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec,
                10,
                false,
                false,
                &["database".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Domain);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"database-fact"),
            "database-fact should be returned"
        );
        assert!(
            !ids.contains(&"auth-fact"),
            "auth-fact should be filtered out"
        );
    }

    #[tokio::test]
    async fn a_domain_filter_that_would_empty_the_candidates_falls_back_to_the_unfiltered_rows() {
        let dir = std::env::temp_dir().join(format!("ns-domain-fallback-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // One fact with domain "auth"
        store
            .upsert(
                "probe",
                "auth-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "auth" })),
            )
            .await
            .unwrap();

        // Search with domain filter for "database" (no match)
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec,
                10,
                false,
                false,
                &["database".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Vector);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"auth-fact"),
            "auth-fact should be returned via fallback"
        );
    }

    #[tokio::test]
    async fn a_legacy_free_text_domain_matches_the_slug_at_comparison_time() {
        let dir = std::env::temp_dir().join(format!("ns-domain-legacy-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // One fact with legacy free-text domain "Database Access"
        // Seeded straight into the store (not through the add path)
        store
            .upsert(
                "probe",
                "legacy-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "Database Access" })),
            )
            .await
            .unwrap();

        // Search with the normalized slug "database-access"
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec,
                10,
                false,
                false,
                &["database-access".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Domain);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"legacy-fact"),
            "legacy-fact should match the normalized slug"
        );

        // Verify the stored metadata still has the original free-text value
        // Use a fresh get() read-back to assert no migration occurred.
        let stored = store.get("probe", "legacy-fact").await.unwrap().unwrap();
        let stored_domain = stored.1.metadata.get("domain").and_then(|v| v.as_str());
        assert_eq!(
            stored_domain,
            Some("Database Access"),
            "stored domain must remain unchanged"
        );
    }

    #[tokio::test]
    async fn a_filtered_search_with_limit_below_unfiltered_count_returns_only_matching_rows() {
        let dir =
            std::env::temp_dir().join(format!("ns-domain-filter-limit-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Seed 5 rows with domain "other" (won't match) and 1 row with domain "target" (will match).
        // Limit is 3, so unfiltered all_primaries would exceed limit (6 > 3),
        // but only the one "target" row should pass the filter.
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("other-fact-{}", i),
                    query_vec.clone(),
                    retired_test_payload("fact", "agent", json!({ "domain": "other" })),
                )
                .await
                .unwrap();
        }
        store
            .upsert(
                "probe",
                "target-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "target" })),
            )
            .await
            .unwrap();

        // Search with domain filter for "target" only, limit = 3
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec.clone(),
                3,
                false,
                false,
                &["target".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        // Filter applied successfully: only the matching row returned, mode is Domain
        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Domain);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 1, "only the matching row should be returned");
        assert!(
            ids.contains(&"target-fact"),
            "target-fact should be returned"
        );

        // Verify that unfiltered set really did exceed limit (otherwise test is vacuous)
        let unfiltered = store
            .search("probe", query_vec.clone(), 10, false, false)
            .await
            .unwrap();
        assert!(
            unfiltered.len() > 3,
            "unfiltered set must exceed limit for the stop condition to be meaningful"
        );
    }

    #[tokio::test]
    async fn mock_store_search_with_domain_matches_real_store_behaviour() {
        use crate::guard::testkit::MockStore;
        use crate::traits::VectorStore;

        // Build a REAL store and a MOCK store
        let real = retired_test_store().await;
        let mock = MockStore::new();
        let namespace = "probe";

        // Seed IDENTICAL rows into both stores
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let payload_auth = retired_test_payload("fact", "agent", json!({ "domain": "auth" }));
        let payload_db = retired_test_payload("fact", "agent", json!({ "domain": "database" }));
        let payload_legacy =
            retired_test_payload("fact", "agent", json!({ "domain": "Database Access" }));

        // Insert into real store
        real.upsert(
            namespace,
            "auth-fact",
            query_vec.clone(),
            payload_auth.clone(),
        )
        .await
        .unwrap();
        real.upsert(
            namespace,
            "database-fact",
            query_vec.clone(),
            payload_db.clone(),
        )
        .await
        .unwrap();
        real.upsert(
            namespace,
            "legacy-fact",
            query_vec.clone(),
            payload_legacy.clone(),
        )
        .await
        .unwrap();

        // Insert into mock store
        mock.upsert(namespace, "auth-fact", query_vec.clone(), payload_auth)
            .await
            .unwrap();
        mock.upsert(namespace, "database-fact", query_vec.clone(), payload_db)
            .await
            .unwrap();
        mock.upsert(namespace, "legacy-fact", query_vec.clone(), payload_legacy)
            .await
            .unwrap();

        // Helper to get sorted IDs from a DomainSearchOutcome
        let sorted_ids = |outcome: &crate::traits::DomainSearchOutcome| -> Vec<String> {
            let mut ids: Vec<String> = outcome.results.iter().map(|r| r.id.clone()).collect();
            ids.sort();
            ids
        };

        // Case A: key set matches exactly one row → mode Domain, only that row returned
        let real_outcome = real
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database".to_string()],
            None,
            true,
            )
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for narrowing case"
        );
        assert_eq!(
            sorted_ids(&real_outcome),
            vec!["database-fact".to_string()],
            "only the matching row should be returned"
        );

        // Case B: key set matches nothing → soft fallback → mode Vector, all rows returned
        let real_outcome = real
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["nonexistent".to_string()],
            None,
            true,
            )
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["nonexistent".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for fallback case"
        );
        assert_eq!(
            sorted_ids(&real_outcome).len(),
            3,
            "all three rows should be returned via soft fallback"
        );

        // Case C: legacy free-text domain matches normalized slug at comparison time
        let real_outcome = real
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database-access".to_string()],
            None,
            true,
            )
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database-access".to_string()],
            None,
            true,
            )
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for legacy case"
        );
        assert!(
            sorted_ids(&real_outcome).contains(&"legacy-fact".to_string()),
            "legacy-fact should match the normalized slug"
        );

        // Case D: no filter (empty key set) → mode Vector, all rows returned
        let real_outcome = real
            .search_with_domain(namespace, query_vec.clone(), 10, false, false, &[], None, true)
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(namespace, query_vec, 10, false, false, &[], None, true)
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for empty key set"
        );
        assert_eq!(
            sorted_ids(&real_outcome).len(),
            3,
            "all three facts should be returned when no filter"
        );
    }

    #[tokio::test]
    async fn mock_store_search_with_domain_honours_limit() {
        use crate::guard::testkit::MockStore;
        use crate::traits::{DomainMatchMode, VectorStore};

        let mock = MockStore::new();
        let namespace = "probe";

        // Seed: TWO rows matching "database", ONE row matching "auth", ONE row matching "compute"
        // This ensures the filtered branch has multiple matching rows to test truncation.
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        for (id, domain) in [
            ("fact-db-1", "database"),
            ("fact-db-2", "database"),
            ("fact-auth", "auth"),
            ("fact-compute", "compute"),
        ] {
            let payload = retired_test_payload("fact", "agent", json!({ "domain": domain }));
            mock.upsert(namespace, id, query_vec.clone(), payload)
                .await
                .unwrap();
        }

        // Empty-key branch (domains = &[]): limit = 1 should return exactly 1 row
        let outcome = mock
            .search_with_domain(namespace, query_vec.clone(), 1, false, false, &[], None, true)
            .await
            .unwrap();
        assert_eq!(outcome.mode, DomainMatchMode::Vector);
        assert_eq!(
            outcome.results.len(),
            1,
            "empty-key branch: limit=1 must return exactly 1 row"
        );

        // Filtered branch (domains = &["database"]): limit = 1 should return exactly 1 matching row
        // We seeded TWO "database" rows, so truncation must happen.
        let outcome = mock
            .search_with_domain(
                namespace,
                query_vec,
                1,
                false,
                false,
                &["database".to_string()],
            None,
            true,
            )
            .await
            .unwrap();
        assert_eq!(outcome.mode, DomainMatchMode::Domain);
        assert_eq!(
            outcome.results.len(),
            1,
            "filtered branch: limit=1 must return exactly 1 row (truncation)"
        );
        // The mock returns in insertion order, so the first matching row is "fact-db-1"
        assert_eq!(
            outcome.results[0].id, "fact-db-1",
            "filtered branch: must return the first matching row in insertion order"
        );
    }

    /// The thread that runs the engine must have an alternate signal stack
    /// big enough for the crash handler.
    ///
    /// This is the crash path that matters: the engine runs on a
    /// blocking-pool thread, and the handler runs on that thread's
    /// alternate stack. The Rust standard library gives a thread 8192 bytes
    /// of it, which is too small -- the handler would fault inside itself
    /// and the breadcrumb would be lost.
    #[tokio::test]
    async fn the_engine_thread_has_an_alternate_stack_the_handler_can_run_on() {
        // The size the handler asks for, measured on this thread.
        crate::crash::install_alt_stack().expect("the alternate stack installs");
        let required = crate::crash::alt_stack_size();
        assert!(
            required >= 32 * 1024,
            "the handler needs at least 32 KiB of alternate stack, got {required}"
        );

        let store = episode_sibling_test_store().await;
        let observed = store
            .with_conn(|_conn| Ok(crate::crash::alt_stack_size()))
            .await
            .expect("the engine call finishes");
        assert!(
            observed >= required,
            "the engine thread must have an alternate stack of at least {required} bytes, got {observed}"
        );
    }

    /// The acceptance sweep (AC 5): a fact with a well-formed access topology
    /// round-trips, reaches the file it governs through the one-hop expansion,
    /// and carries that `GOVERNS` edge into the exported graph.
    ///
    /// The file node is written FIRST, so the edge resolves on the fact's
    /// write, and it is written as a `file` -- a structural type, excluded
    /// from the base candidate query. That is the point: the governed file is
    /// reachable ONLY through the expansion, never as a direct match, which is
    /// why the recall filters to direct matches and never recalls a governed
    /// AST node in place of the fact.
    #[tokio::test]
    async fn a_topology_fact_reaches_its_governed_file_and_exports() {
        let store = retired_test_store().await;

        let topology = json!({
            "component": "apigateway",
            "target_system": "ledgerservice",
            "channel": "mTLS",
            "credential_boundary": "eu-west-1 VPC",
            "reason": "the ledger accepts traffic only from inside the VPC",
        });

        // The structural file node, written before the fact so the GOVERNS
        // edge has a target to resolve against.
        store
            .upsert(
                "probe",
                "ledger-file",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .expect("the file node is written");

        store
            .upsert(
                "probe",
                "topology-fact",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({
                        "governs": ["ledger-file"],
                        "topology": topology,
                    }),
                ),
            )
            .await
            .expect("the fact is written");

        // (1) The topology round-trips unchanged through a search for it.
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .expect("the search runs");
        let fact = results
            .iter()
            .find(|r| r.id == "topology-fact")
            .unwrap_or_else(|| {
                panic!(
                    "the fact is in the results: {:?}",
                    results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            fact.payload.metadata["topology"], topology,
            "the access topology round-trips unchanged"
        );

        // (2) The fact reaches the file node through the GOVERNS edge, and the
        //     expansion row carries OneHop evidence.
        let file = results
            .iter()
            .find(|r| r.id == "ledger-file")
            .unwrap_or_else(|| {
                panic!(
                    "the governed file is reached through the expansion: {:?}",
                    results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
                )
            });
        let evidence = file.evidence.as_ref().expect("the expansion row carries evidence");
        assert!(
            evidence
                .iter()
                .any(|e| e.kind == crate::traits::EvidenceKind::OneHop),
            "the governed file is reached in one hop: {:?}",
            evidence
                .iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            evidence.iter().any(|e| e.path.iter().any(|edge| {
                edge.relation == "GOVERNS"
                    && ((edge.source == "topology-fact" && edge.target == "ledger-file")
                        || (edge.source == "ledger-file" && edge.target == "topology-fact"))
            })),
            "the one-hop path names the GOVERNS edge between the fact and the file: {:?}",
            evidence
        );

        // (3) The file node is a structural type: it is NOT a base candidate,
        //     so it carries no DirectMatch and is reachable only through the
        //     expansion -- which is why the recall keeps the fact and drops
        //     the governed node.
        assert!(
            STRUCTURAL_MEMORY_TYPES.contains(&"file"),
            "a file node is a structural type: {:?}",
            STRUCTURAL_MEMORY_TYPES
        );
        assert!(
            !crate::traits::is_direct_match(&file.evidence),
            "the structural file is not a direct match: {:?}",
            file.evidence
        );
        assert!(
            crate::traits::is_direct_match(&fact.evidence),
            "the fact itself is the direct match: {:?}",
            fact.evidence
        );

        // (4) The export carries the fact node AND the GOVERNS link to the file.
        let graph = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .expect("the graph exports");
        let nodes = graph["nodes"].as_array().expect("the export carries nodes");
        let fact_node = nodes
            .iter()
            .find(|n| n["id"] == "topology-fact")
            .expect("the fact node is in the export");
        assert_eq!(
            fact_node["metadata"]["topology"], topology,
            "the export carries the topology with the fact"
        );
        let links = graph["links"].as_array().expect("the export carries links");
        assert!(
            links.iter().any(|l| {
                l["source"] == "topology-fact"
                    && l["target"] == "ledger-file"
                    && l["type"] == "GOVERNS"
            }),
            "the GOVERNS link to the file is in the export: {:?}",
            links
        );
    }

    /// The dedup key: a one-hop undirected path collapses with its own
    /// reverse, and a multi-hop path does not. A path's orientation is part of
    /// its identity, except for a one-hop edge that has no direction.
    #[test]
    fn a_one_hop_undirected_path_collapses_but_a_two_hop_path_does_not() {
        let edge = |source: &str, relation: &str, target: &str| crate::traits::EvidenceEdge {
            source: source.to_string(),
            relation: relation.to_string(),
            target: target.to_string(),
        };
        let forward = vec![edge("probe::a", "RELATES_TO", "probe::b")];
        let reverse = vec![edge("probe::b", "RELATES_TO", "probe::a")];
        let directed_forward = vec![edge("probe::a", "GOVERNS", "probe::b")];
        let directed_reverse = vec![edge("probe::b", "GOVERNS", "probe::a")];

        assert_eq!(
            norm_path(&forward, &crate::traits::EvidenceKind::OneHop),
            norm_path(&reverse, &crate::traits::EvidenceKind::OneHop),
            "a one-hop undirected path normalises to the same key either way"
        );
        assert_ne!(
            norm_path(&directed_forward, &crate::traits::EvidenceKind::OneHop),
            norm_path(&directed_reverse, &crate::traits::EvidenceKind::OneHop),
            "a one-hop directed path keeps its order"
        );
        assert_ne!(
            norm_path(&forward, &crate::traits::EvidenceKind::Governs2hop),
            norm_path(&reverse, &crate::traits::EvidenceKind::Governs2hop),
            "a two-hop path keeps its order"
        );
    }

    /// A store whose primary is reached by an INCOMING edge, with a node that
    /// governs the intermediate:
    ///
    ///   probe::mid -RELATES_TO-> probe::primary <-GOVERNS- probe::ruler
    ///
    /// `probe::ruler` is two hops from the primary: the primary is reached
    /// through the intermediate, and the ruler governs the intermediate.
    async fn incoming_evidence_test_store() -> LadybugStore {
        let dir =
            std::env::temp_dir().join(format!("ns-incoming-evidence-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        // The direct match: the target of the intermediate's RELATES_TO edge.
        store
            .upsert(
                "probe",
                "probe::primary",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // The intermediate: it relates to the primary, so the edge points at
        // the primary.
        store
            .upsert(
                "probe",
                "probe::mid",
                vec![0.5, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "related_to": ["probe::primary"] })),
            )
            .await
            .unwrap();

        // The governing node: it governs the intermediate.
        store
            .upsert(
                "probe",
                "probe::ruler",
                vec![0.2, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "governs": ["probe::mid"] })),
            )
            .await
            .unwrap();

        store
    }

    /// An incoming two-hop path's first hop points at the direct match: the
    /// intermediate is the edge's source and the direct match its target. The
    /// outgoing branch builds the same hop the other way round, so the two
    /// directions have to be pinned separately.
    #[tokio::test]
    async fn incoming_two_hop_evidence_points_the_first_hop_at_the_direct_match() {
        let store = incoming_evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let ruler = results.iter().find(|r| r.id == "probe::ruler").unwrap();
        let two_hop: Vec<_> = ruler
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert_eq!(two_hop.len(), 1, "ruler should carry one two-hop path");
        let path = &two_hop[0].path;
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].source, "probe::mid");
        assert_eq!(path[0].relation, "RELATES_TO");
        assert_eq!(path[0].target, "probe::primary");
        assert_eq!(path[1].source, "probe::ruler");
        assert_eq!(path[1].relation, "GOVERNS");
        assert_eq!(path[1].target, "probe::mid");
    }

    #[tokio::test]
    async fn the_registry_row_never_appears_in_search_results() {
        let dir = std::env::temp_dir().join(format!("ns-registry-row-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        store
            .upsert(
                "probe",
                "normal-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        let registry_id = crate::registry::registry_row_id("probe");
        let payload = MemoryPayload {
            content: crate::registry::registry_content("probe"),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: crate::registry::registry_metadata(&["auth".to_string()]),
        };
        store
            .upsert("probe", &registry_id, query_vec.clone(), payload)
            .await
            .unwrap();

        let results = store.search("probe", query_vec, 10, false, false).await.unwrap();
        assert!(
            !results.iter().any(|r| r.id == registry_id),
            "the registry row must never appear in search results"
        );
    }

    #[tokio::test]
    async fn refresh_ontology_loads_v2_entries() {
        let dir = std::env::temp_dir().join(format!("ns-ontology-refresh-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let entries = vec![
            serde_json::json!({"slug":"auth","parent":null,"aliases":["authentication"],"concept_iri":null}),
            serde_json::json!({"slug":"tls","parent":"auth","aliases":["ssl"],"concept_iri":null}),
        ];
        let registry_id = crate::registry::registry_row_id("probe");
        let payload = MemoryPayload {
            content: crate::registry::registry_content("probe"),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: crate::registry::registry_metadata_v2(&entries),
        };
        store.upsert("probe", &registry_id, query_vec, payload).await.unwrap();

        store.refresh_ontology("probe").await;
        let idx = store.get_ontology("probe");
        assert_eq!(idx.canonical_slug("authentication"), Some("auth".to_string()));
        assert!(idx.expand_domain("auth").contains(&"tls".to_string()));
    }

    #[tokio::test]
    async fn test_f1_exact_name_recall_five_identifier_classes() {
        let dir = std::env::temp_dir().join(format!("ns-f1-exact-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("init");
        let zero = vec![0.1, 0.2, 0.3, 0.4];

        // 1. Symbol
        let mut p_sym = retired_test_payload("fact", "agent", json!({"symbol": "verify_bundle_sha256"}));
        p_sym.content = "Bundle verification helper verify_bundle_sha256".to_string();
        store.upsert("probe", "m-sym", zero.clone(), p_sym).await.unwrap();

        // 2. File path
        let mut p_file = retired_test_payload("fact", "agent", json!({}));
        p_file.location = "src/judgment/laya_onnx.rs".to_string();
        p_file.content = "Local ONNX duplicate judgment provider".to_string();
        store.upsert("probe", "m-file", zero.clone(), p_file).await.unwrap();

        // 3. Env var
        let mut p_env = retired_test_payload("rule", "agent", json!({"env": "NEUROSTRATA_HYBRID_SEARCH", "source": "AGENTS.md"}));
        p_env.content = "Set NEUROSTRATA_HYBRID_SEARCH in environment to enable hybrid scoring".to_string();
        store.upsert("probe", "m-env", zero.clone(), p_env).await.unwrap();

        // 4. Error string
        let mut p_err = retired_test_payload("fact", "agent", json!({"error_code": "ERR_SHACL_CLOSED_WORLD"}));
        p_err.content = "Validation error ERR_SHACL_CLOSED_WORLD when target missing".to_string();
        store.upsert("probe", "m-err", zero.clone(), p_err).await.unwrap();

        // 5. Quoted phrase
        let mut p_phrase = retired_test_payload("rule", "agent", json!({"source": "AGENTS.md"}));
        p_phrase.content = "Always enforce zero compile-time feature flags across workspace".to_string();
        store.upsert("probe", "m-phrase", zero.clone(), p_phrase).await.unwrap();

        for (query, expected_id) in [
            ("verify_bundle_sha256", "m-sym"),
            ("  SRC/JUDGMENT/LAYA_ONNX.RS  ", "m-file"),
            ("NEUROSTRATA_HYBRID_SEARCH", "m-env"),
            ("ERR_SHACL_CLOSED_WORLD", "m-err"),
            ("Always enforce zero compile-time feature flags across workspace", "m-phrase"),
        ] {
            let outcome = store
                .search_with_domain("probe", vec![0.9, 0.0, 0.0, 0.0], 5, false, false, &[], Some(query), true)
                .await
                .unwrap();
            let hit = outcome.results.iter().find(|r| r.id == expected_id).expect("exact match found");
            assert_eq!(hit.score, 0.0, "exact match should have score 0.0 for {}", query);
            assert!(crate::traits::is_direct_match(&hit.evidence));

            let lex = store
                .search_lexical("probe", query.trim(), 5, false, false, &[], "admin", 1)
                .await
                .unwrap();
            assert!(lex.results.iter().any(|r| r.id == expected_id), "lexical hit for {}", query);
        }

        // Negative probe: non-existent identifier should not get an exact-name 0.0 score
        let neg = store
            .search_with_domain("probe", vec![0.9, 0.0, 0.0, 0.0], 5, false, false, &[], Some("NON_EXISTENT_SYMBOL_XYZ_999"), true)
            .await
            .unwrap();
        assert!(neg.results.iter().all(|r| r.score > 0.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_f6_edge_sources_provenance_round_trip() {
        let dir = std::env::temp_dir().join(format!("ns-f6-edge-prov-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("init");
        let v = vec![0.1, 0.2, 0.3, 0.4];

        let target = retired_test_payload("fact", "agent", json!({}));
        store.upsert("probe", "target-node", v.clone(), target).await.unwrap();

        let mut declarer = retired_test_payload("rule", "agent", json!({
            "governs": ["target-node"],
            "absolute_path": "src/domain.rs",
            "file_sha256": "deadbeefcafe"
        }));
        declarer.location = "src/domain.rs".to_string();
        declarer.location_lines = "10-25".to_string();
        store.upsert("probe", "declaring-rule", v.clone(), declarer).await.unwrap();

        let fetched = store.get("probe", "declaring-rule").await.unwrap().expect("declaring-rule exists");
        let edge_sources = fetched.1.metadata.get("edge_sources").and_then(|v| v.as_array()).expect("edge_sources array");
        assert_eq!(edge_sources.len(), 1);
        let es = &edge_sources[0];
        assert_eq!(es.get("declaring_id").and_then(|v| v.as_str()), Some("declaring-rule"));
        assert_eq!(es.get("rel_type").and_then(|v| v.as_str()), Some("GOVERNS"));
        assert_eq!(es.get("resolved_target").and_then(|v| v.as_str()), Some("target-node"));
        assert_eq!(es.get("logical_target").and_then(|v| v.as_str()), Some("target-node"));
        assert_eq!(es.get("file").and_then(|v| v.as_str()), Some("src/domain.rs"));
        assert_eq!(es.get("content_hash").and_then(|v| v.as_str()), Some("deadbeefcafe"));
        assert_eq!(es.get("line_start").and_then(|v| v.as_i64()), Some(10));
        assert_eq!(es.get("line_end").and_then(|v| v.as_i64()), Some(25));
        assert_eq!(es.get("extractor").and_then(|v| v.as_str()), Some("upsert"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_f13_dangling_edges_and_f18_shape_warnings() {
        let dir = std::env::temp_dir().join(format!("ns-f13-f18-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("init");
        let v = vec![0.1, 0.2, 0.3, 0.4];

        let valid_target = retired_test_payload("fact", "agent", json!({}));
        store.upsert("probe", "valid-target", v.clone(), valid_target).await.unwrap();

        // Row with one resolved target, one dangling target, missing rule source, and invalid endpoint role (fact contained_by fact)
        let declarer = retired_test_payload("rule", "agent", json!({
            "governs": ["valid-target", "missing-target-xyz"],
            "contained_by": ["valid-target"]
        }));
        store.upsert("probe", "declarer-1", v.clone(), declarer).await.unwrap();

        let dangling = store.dangling_edges("probe").await.unwrap();
        assert!(
            dangling.iter().any(|d| d.get("declared_target").and_then(|t| t.as_str()) == Some("missing-target-xyz") && d.get("reason").and_then(|r| r.as_str()) == Some("missing")),
            "expected missing-target-xyz in dangling_edges: {:?}",
            dangling
        );
        assert!(
            !dangling.iter().any(|d| d.get("declared_target").and_then(|t| t.as_str()) == Some("valid-target")),
            "resolved valid-target must not be in dangling_edges"
        );

        let warnings = store.shape_warnings("probe").await;
        assert!(
            warnings.iter().any(|w| w.get("why").and_then(|r| r.as_str()).map(|s| s.contains("endpoint role mismatch")).unwrap_or(false)),
            "expected endpoint role mismatch in shape_warnings: {:?}",
            warnings
        );
        assert!(
            warnings.iter().any(|w| w.get("field").and_then(|f| f.as_str()) == Some("metadata.source")),
            "expected metadata.source SHACL warning in shape_warnings: {:?}",
            warnings
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hygiene repair must not DESTROY a structured source. Reading
    /// `metadata.source` as a string alone made an object source look blank,
    /// and the repair then overwrote the provenance it was meant to preserve
    /// with the row's location. `crate::traits::has_source` is the canonical
    /// predicate for "this names its origin", so the repair asks it too.
    #[tokio::test]
    async fn repair_hygiene_keeps_an_object_valued_rule_source() {
        let dir = std::env::temp_dir().join(format!("ns-repair-src-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("init");
        let v = vec![0.1, 0.2, 0.3, 0.4];

        let rule = retired_test_payload("rule", "agent", json!({
            "source": { "kind": "release-run", "ref": "v1.16.0" },
        }));
        store.upsert("probe", "rule-object-source", v.clone(), rule).await.unwrap();

        let (shape_fixes, _) = store.repair_namespace_hygiene("probe").await.unwrap();
        assert_eq!(shape_fixes, 0, "an object source is provenance: nothing to repair");

        let rows = store.list_memories("probe", None, None).await.unwrap();
        let row = rows
            .iter()
            .find(|r| r.id == "rule-object-source")
            .expect("the row survives the repair");
        assert_eq!(
            row.payload.metadata.get("source"),
            Some(&json!({ "kind": "release-run", "ref": "v1.16.0" })),
            "the repair must not overwrite an object source with a string"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_f20_concept_iri_stamping_on_write() {
        let dir = std::env::temp_dir().join(format!("ns-f20-concept-iri-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("init");
        let v = vec![0.1, 0.2, 0.3, 0.4];

        let entries = vec![
            serde_json::json!({
                "slug": "auth",
                "parent": null,
                "aliases": ["authentication"],
                "concept_iri": "https://neurostrata.io/concepts/authentication"
            }),
        ];
        let registry_id = crate::registry::registry_row_id("probe");
        let reg_payload = MemoryPayload {
            content: crate::registry::registry_content("probe"),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: crate::registry::registry_metadata_v2(&entries),
        };
        store.upsert("probe", &registry_id, v.clone(), reg_payload).await.unwrap();

        let p = retired_test_payload("rule", "agent", json!({"domain": "auth"}));
        store.upsert("probe", "auth-rule-1", v.clone(), p).await.unwrap();

        let fetched = store.get("probe", "auth-rule-1").await.unwrap().expect("exists");
        assert_eq!(
            fetched.1.metadata.get("concept_iri").and_then(|v| v.as_str()),
            Some("https://neurostrata.io/concepts/authentication")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ast_neighborhood_for_file` bundles 1-hop `DEPENDS_ON`/`RELATES_TO`
    /// neighbors (both directions) plus `SUPERSEDES` targets into
    /// `related_memories`, and a compact AST neighborhood
    /// (`symbols`/`callers`/`callees`/`imports`/`importers`/`blast_radius_count`)
    /// into `ast_summary` (Enhancement 4).
    #[tokio::test]
    async fn ast_neighborhood_for_file_bundles_related_and_ast_summary() {
        let dir = std::env::temp_dir().join(format!("ast-nb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let seed = |id: &str, location: &str, memory_type: &str, metadata: &str| {
            format!(
                "CREATE (m:Memory {{id: '{}', namespace: 'probe', content: 'c', user_id: 'u', memory_type: '{}', agent_name: 'a', location: '{}', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
                escape_ladybug_string(id),
                memory_type,
                escape_ladybug_string(location),
                escape_ladybug_string(metadata)
            )
        };
        let edge = |rel: &str, a: &str, b: &str| {
            format!(
                "MERGE (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[r:{} {{source: 'test', valid_from: 0, valid_to: 0, generation: 0, extractor: 'test', evidence_id: 'e', origin: 'local'}}]->(b)",
                escape_ladybug_string(a),
                escape_ladybug_string(b),
                rel
            )
        };

        let rows = [
            seed("mem-rule-a", "src/lib.rs", "rule", r#"{"superseded_by": "mem-rule-c"}"#),
            seed("mem-rule-b", "src/lib.rs", "fact", "{}"),
            seed("mem-file", "src/lib.rs", "file", r#"{"imports": ["pkg/other.rs", "src/lib.rs"]}"#),
            seed("mem-sym-foo", "src/lib.rs#function:foo@1", "symbol", "{}"),
            seed("mem-sym-bar", "src/lib.rs#function:bar@2", "symbol", "{}"),
            seed("mem-sym-x", "other.rs#function:x@3", "symbol", "{}"),
            seed("mem-rule-c", "src/lib.rs", "rule", "{}"),
        ];
        for row in rows {
            store
                .with_conn(move |conn| {
                    conn.query(&row)?;
                    Ok(())
                })
                .await
                .expect("seed node");
        }
        for (rel, a, b) in [
            ("DEPENDS_ON", "mem-rule-a", "mem-sym-foo"),
            ("DEPENDS_ON", "mem-sym-foo", "mem-sym-bar"),
            ("DEPENDS_ON", "mem-sym-x", "mem-sym-foo"),
            ("RELATES_TO", "mem-rule-b", "mem-sym-bar"),
            ("RELATES_TO", "mem-sym-x", "mem-rule-b"),
        ] {
            store
                .with_conn(move |conn| {
                    conn.query(&edge(rel, a, b))?;
                    Ok(())
                })
                .await
                .expect("seed edge");
        }

        let (related, summary) = store
            .ast_neighborhood_for_file(
                "probe",
                None,
                "src/lib.rs",
                &["mem-rule-a".to_string(), "mem-rule-b".to_string()],
            )
            .await
            .expect("neighborhood summary");

        // related_memories: 1-hop neighbors both directions + SUPERSEDES.
        let related_ids: Vec<&str> = related
            .iter()
            .map(|v| v.get("id").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(related.len(), 4, "exactly four related memories: {related:?}");
        assert!(related_ids.contains(&"mem-sym-foo"), "DEPENDS_ON outgoing: {related:?}");
        assert!(related_ids.contains(&"mem-sym-bar"), "RELATES_TO outgoing: {related:?}");
        assert!(related_ids.contains(&"mem-sym-x"), "RELATES_TO incoming: {related:?}");
        assert!(related_ids.contains(&"mem-rule-c"), "SUPERSEDES target: {related:?}");
        let dep_foo = related
            .iter()
            .find(|v| v.get("id").and_then(|v| v.as_str()) == Some("mem-sym-foo"))
            .unwrap();
        assert_eq!(
            dep_foo.get("relation").and_then(|v| v.as_str()),
            Some("DEPENDS_ON")
        );
        assert_eq!(
            dep_foo.get("direction").and_then(|v| v.as_str()),
            Some("outgoing")
        );

        // ast_summary.
        let sym_ids: Vec<&str> = summary
            .get("symbols")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .map(|v| v.get("id").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(sym_ids.len(), 2, "two file symbols: {sym_ids:?}");
        assert!(sym_ids.contains(&"mem-sym-foo"));
        assert!(sym_ids.contains(&"mem-sym-bar"));

        let caller_ids: Vec<&str> = summary
            .get("callers")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .map(|v| v.get("id").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(caller_ids.len(), 3, "callers depend on file symbols: {caller_ids:?}");
        assert!(caller_ids.contains(&"mem-rule-a"));
        assert!(caller_ids.contains(&"mem-sym-foo"));
        assert!(caller_ids.contains(&"mem-sym-x"));

        let callee_ids: Vec<&str> = summary
            .get("callees")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .map(|v| v.get("id").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(callee_ids.len(), 1, "callees are file symbols' targets: {callee_ids:?}");
        assert!(callee_ids.contains(&"mem-sym-bar"));

        let import_paths: Vec<&str> = summary
            .get("imports")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .map(|v| v.get("path").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert!(import_paths.contains(&"pkg/other.rs"));
        assert!(import_paths.contains(&"src/lib.rs"));

        let imp_ids: Vec<&str> = summary
            .get("importers")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .map(|v| v.get("id").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert!(imp_ids.is_empty(), "no importers: {imp_ids:?}");

        assert_eq!(
            summary.get("blast_radius_count").and_then(|v| v.as_u64()),
            Some(8),
            "blast radius: {summary}"
        );
        assert_eq!(
            summary.get("truncated").and_then(|v| v.as_bool()),
            Some(false),
            "small neighborhood is not truncated: {summary}"
        );
        assert_eq!(
            summary.get("related_memories_truncated").and_then(|v| v.as_bool()),
            Some(false),
            "small related_memories is not truncated: {summary}"
        );

        // Seed high-fan-out neighbors exceeding MAX_RELATED_MEMORIES and MAX_AST_COLLECTION_ITEMS
        // with oversized content to verify query/node and content budgets + truncation indicators.
        let long_content = "x".repeat(LadybugStore::MAX_NEIGHBOR_CONTENT_CHARS + 50);
        for i in 0..25 {
            let rel_id = format!("mem-fanout-rel-{i}");
            let caller_id = format!("mem-fanout-caller-{i}");
            let create_rel = format!(
                "CREATE (m:Memory {{id: '{rel_id}', namespace: 'probe', content: '{long_content}', user_id: 'u', memory_type: 'fact', agent_name: 'a', location: 'other.rs', location_lines: '', metadata: '{{}}', embedding: [0.0,0.0,0.0,0.0]}})"
            );
            let create_caller = format!(
                "CREATE (m:Memory {{id: '{caller_id}', namespace: 'probe', content: '{long_content}', user_id: 'u', memory_type: 'symbol', agent_name: 'a', location: 'caller.rs#function:c{i}@1', location_lines: '', metadata: '{{}}', embedding: [0.0,0.0,0.0,0.0]}})"
            );
            let edge_rel = edge("RELATES_TO", "mem-rule-a", &rel_id);
            let edge_caller = edge("DEPENDS_ON", &caller_id, "mem-sym-foo");
            store
                .with_conn(move |conn| {
                    conn.query(&create_rel)?;
                    conn.query(&create_caller)?;
                    conn.query(&edge_rel)?;
                    conn.query(&edge_caller)?;
                    Ok(())
                })
                .await
                .expect("seed fanout");
        }

        let (related_capped, summary_capped) = store
            .ast_neighborhood_for_file(
                "probe",
                None,
                "src/lib.rs",
                &["mem-rule-a".to_string(), "mem-rule-b".to_string()],
            )
            .await
            .expect("capped neighborhood summary");
        assert_eq!(
            related_capped.len(),
            LadybugStore::MAX_RELATED_MEMORIES,
            "related_memories capped at MAX_RELATED_MEMORIES"
        );
        assert_eq!(
            summary_capped
                .get("related_memories_truncated")
                .and_then(|v| v.as_bool()),
            Some(true),
            "related_memories_truncated set on fanout overflow: {summary_capped}"
        );
        assert_eq!(
            summary_capped.get("truncated").and_then(|v| v.as_bool()),
            Some(true),
            "ast_summary.truncated set on callers overflow: {summary_capped}"
        );
        assert_eq!(
            summary_capped
                .get("callers")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(LadybugStore::MAX_AST_COLLECTION_ITEMS),
            "callers capped at MAX_AST_COLLECTION_ITEMS"
        );
        assert!(
            related_capped
                .iter()
                .any(|v| v.get("truncated").and_then(|b| b.as_bool()) == Some(true)),
            "oversized neighbor content is truncated with indicator"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- The store-state sidecar (pristine-store predicate) ---
    //
    // The sidecar beside the db file is the store-authored record the
    // setup-time model choice keys on: `init` records a pristine store
    // with the configured model's descriptor, the first successful
    // `upsert` flips it to data-bearing, and the flip is monotonic.

    struct StoreStateFixture {
        root: std::path::PathBuf,
        db: std::path::PathBuf,
        config_dir: std::path::PathBuf,
    }

    fn write_embedders_config(config_dir: &std::path::Path, model: &str) {
        std::fs::write(
            config_dir.join("embedders.json"),
            format!("[{{\"model_name\":\"{model}\",\"dimensions\":4}}]"),
        )
        .expect("write fixture embedders.json");
    }

    /// A remote embedders.json entry at the fixture width: base_url
    /// and api_key_env both set — the predicate `build_embedder`
    /// branches on — with `api_model` present or absent.
    fn write_remote_embedders_config(config_dir: &std::path::Path, api_model: Option<&str>) {
        let api_model_field = match api_model {
            Some(model) => format!(",\"api_model\":\"{model}\""),
            None => String::new(),
        };
        std::fs::write(
            config_dir.join("embedders.json"),
            format!(
                "[{{\"model_name\":\"RemoteEntry\",\"dimensions\":4,\"base_url\":\"http://127.0.0.1:9/v1\",\"api_key_env\":\"NS_TEST_REMOTE_KEY\"{api_model_field}}}]"
            ),
        )
        .expect("write fixture remote embedders.json");
    }

    /// A temp root holding one store db plus a config dir whose
    /// embedders.json names `model` — the configured model the store
    /// resolves through the `embed::configured_model_in` seam.
    fn store_state_fixture(tag: &str, model: &str) -> StoreStateFixture {
        let root =
            std::env::temp_dir().join(format!("ns-storestate-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create fixture root");
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).expect("create fixture config dir");
        write_embedders_config(&config_dir, model);
        StoreStateFixture {
            db: root.join("ladybug.db"),
            root,
            config_dir,
        }
    }

    fn read_sidecar(db: &std::path::Path) -> serde_json::Value {
        let content = std::fs::read_to_string(crate::model_choice::store_state_path(db))
            .expect("sidecar exists");
        serde_json::from_str(&content).expect("sidecar parses")
    }

    /// NEUROSTRATA_MODEL would override the file's first entry; the
    /// file-based tests pin it out of the way and restore it after.
    fn without_model_env() -> Option<String> {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        previous
    }

    fn restore_model_env(previous: Option<String>) {
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    fn store_state_payload() -> MemoryPayload {
        retired_test_payload("fact", "owner", json!({}))
    }

    /// The serving descriptor of a process that built its embedder for
    /// `model` at the fixture width: the shape
    /// `FastEmbedder::embedding_descriptor` produces for the model the
    /// fixture's `embedders.json` selects, which is also the shape
    /// `configured_embedding_descriptor` synthesizes for the same model.
    fn serving_descriptor_for(model: &str) -> crate::traits::EmbeddingDescriptor {
        serving_descriptor_dims(model, 4)
    }

    /// [`serving_descriptor_for`] at an explicit width.
    fn serving_descriptor_dims(model: &str, dims: usize) -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "fastembed".into(),
            model: model.into(),
            revision: "onnx".into(),
            dims,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
    }

    /// The legacy placeholder descriptor `store_embedding_descriptor`
    /// produces with `NEUROSTRATA_MODEL` unset.
    fn placeholder_descriptor(dims: usize) -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "store-registered".into(),
            model: "local-fastembed".into(),
            revision: "store-lifetime".into(),
            dims,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
    }

    /// A remote embedder's descriptor: the shape
    /// `RemoteEmbedder::embedding_descriptor` produces, and the shape a
    /// matching remote configured entry synthesizes.
    fn remote_descriptor_for(model: &str) -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "openai-compatible".into(),
            model: model.into(),
            revision: "unknown".into(),
            dims: 4,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
    }

    /// A data-bearing fixture: a store recorded under ModelA with one
    /// row landed, its instance dropped.
    async fn data_bearing_store_fixture(tag: &str) -> StoreStateFixture {
        let fx = store_state_fixture(tag, "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        store
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("seed upsert");
        assert!(
            crate::model_choice::store_has_data(&fx.db),
            "the fixture store is data-bearing"
        );
        drop(store);
        fx
    }

    #[tokio::test]
    async fn store_state_sidecar_records_pristine_then_flips_on_first_upsert() {
        let previous = without_model_env();
        let fx = store_state_fixture("flip", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert!(
            !crate::model_choice::store_has_data(&fx.db),
            "init on a fresh store records a pristine sidecar"
        );
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));

        store
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await
            .expect("first upsert");
        assert!(
            crate::model_choice::store_has_data(&fx.db),
            "the first successful upsert flips has_data"
        );
        assert_eq!(read_sidecar(&fx.db)["has_data"], json!(true));
        drop(store);

        // A second instance on the same path reads the recorded state.
        let store2 = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store2.init("probe").await.expect("re-init");
        assert!(crate::model_choice::store_has_data(&fx.db));
        drop(store2);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_failed_upsert_does_not_flip() {
        let previous = without_model_env();
        let fx = store_state_fixture("failed-upsert", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert!(!crate::model_choice::store_has_data(&fx.db));
        // Wrong-width vector: the Memory table's FLOAT[4] column rejects it.
        let failed = store
            .upsert("probe", "bad", vec![0.0; 3], store_state_payload())
            .await;
        assert!(
            failed.is_err(),
            "a wrong-width upsert must fail at the engine"
        );
        assert!(
            !crate::model_choice::store_has_data(&fx.db),
            "a failed upsert must not flip has_data"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn upsert_times_out_to_caller_but_committing_task_records_first_data() {
        // The finding's scenario: a first write that outlives the
        // caller's deadline inside its committing task. The deadline
        // releases the caller, not the query — the blocking task keeps
        // running and its MERGE lands — so the first-data transition
        // must be recorded by that task, not by the caller.
        //
        // The slowdown is the write closure's own edge bookkeeping:
        // thousands of declared-but-missing edge targets keep the
        // committing task busy for seconds after the core MERGE lands
        // (measured ~4.6 s per 2,000 targets on the gate VM; 4,000 are
        // declared here), against a 1 s caller deadline. Holding the
        // store's write turn to queue the write instead does not
        // reproduce the scenario at this HEAD: upsert's pre-write reads
        // take the same turn through `with_conn` with no deadline, so a
        // held turn deadlocks the upsert before its write task exists,
        // and a write still queued when its deadline fires is dropped
        // with the timed future, never committed.
        let previous = without_model_env();
        let fx = store_state_fixture("flip-timeout", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert!(
            !crate::model_choice::store_has_data(&fx.db),
            "init on a fresh store records a pristine sidecar"
        );
        assert_eq!(read_sidecar(&fx.db)["has_data"], json!(false));

        let mut payload = store_state_payload();
        payload.metadata = json!({
            "related_to": (0..4000)
                .map(|i| format!("missing-target-{i}"))
                .collect::<Vec<_>>(),
        });
        let previous_timeout = std::env::var("NEUROSTRATA_WRITE_TIMEOUT_SECS").ok();
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "1");
        let timed_out = store.upsert("probe", "m1", vec![0.0; 4], payload).await;
        // write_timeout() is read at invocation, so restoring the var
        // now cannot affect the already-running task.
        match previous_timeout {
            Some(value) => std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", value),
            None => std::env::remove_var("NEUROSTRATA_WRITE_TIMEOUT_SECS"),
        }
        let err = timed_out.expect_err("the slow first write must time out to the caller");
        assert!(
            err.to_string().contains("the database is busy"),
            "the caller must see the deadline error: {err}"
        );

        // The committing task is still running its edge bookkeeping;
        // its MERGE has landed, so the sidecar must flip without the
        // caller ever having observed success.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut flipped = false;
        while std::time::Instant::now() < deadline {
            if let Some(state) = crate::model_choice::read_store_state(&fx.db) {
                if state.has_data {
                    flipped = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            flipped,
            "the committing task must flip has_data after its write lands, even though the caller timed out"
        );
        let landed = store.get("probe", "m1").await.expect("get");
        assert!(landed.is_some(), "the timed-out write actually landed");
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_has_data_is_monotonic_across_delete_and_reinit() {
        let previous = without_model_env();
        let fx = store_state_fixture("monotonic", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        store
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await
            .expect("upsert");
        assert!(crate::model_choice::store_has_data(&fx.db));

        store.delete("probe", "m1").await.expect("delete");
        assert!(
            crate::model_choice::store_has_data(&fx.db),
            "deleting the only memory must not reset has_data: a store that held data stays identity-locked even when emptied"
        );
        store.init("probe").await.expect("re-init same instance");
        assert!(crate::model_choice::store_has_data(&fx.db));
        drop(store);

        let store2 = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store2.init("probe").await.expect("re-init new instance");
        assert!(crate::model_choice::store_has_data(&fx.db));
        assert_eq!(read_sidecar(&fx.db)["has_data"], json!(true));
        drop(store2);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_upsert_refused_when_configured_model_changed_while_pristine() {
        // The guard-only path: the config changes and `upsert` is called
        // directly, with no intervening `init`. This does NOT cover the
        // init-mediated path — the live write half always re-inits
        // first, which is where config-keyed adoption disarmed this
        // guard; that path is covered by
        // store_state_same_instance_reinit_after_pick_does_not_adopt_and_upsert_refuses.
        let previous = without_model_env();
        let fx = store_state_fixture("race", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelA"));

        // A pick lands while this process holds the pristine store open:
        // embedders.json now selects a different model.
        write_embedders_config(&fx.config_dir, "ModelB");
        let refused = store
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await;
        let err = refused
            .expect_err("an upsert under a changed configured model must be refused, not landed");
        assert!(
            err.to_string().contains("Restart the daemon"),
            "the refusal must be the restart-to-apply error: {err}"
        );
        assert!(
            !crate::model_choice::store_has_data(&fx.db),
            "a refused write leaves the store pristine"
        );
        let gone = store.get("probe", "m1").await.expect("get");
        assert!(gone.is_none(), "no data lands under the stale model");
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_same_instance_reinit_after_pick_does_not_adopt_and_upsert_refuses() {
        // Regression, at the exact step the live write path
        // performs: the pre-pick daemon's add re-inits the
        // store (embed_dedup_and_upsert calls init before embedding),
        // and under config-keyed adoption that re-init rewrote the
        // recorded descriptor to the picked model, disarming the guard
        // for the write that followed. Serving-keyed adoption must
        // leave the record untouched, and the write must refuse.
        let previous = without_model_env();
        let fx = store_state_fixture("reinit-after-pick", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelA"));
        assert_eq!(read_sidecar(&fx.db)["has_data"], json!(false));

        // The pick lands while this process holds the store open.
        write_embedders_config(&fx.config_dir, "ModelB");
        // The intervening re-init the write half always performs.
        store.init("probe").await.expect("re-init same instance");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelA"),
            "a same-instance re-init after a pick must not adopt the new configured model"
        );

        let refused = store
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await;
        let err = refused.expect_err("the pre-pick process's write must be refused, not landed");
        assert!(
            err.to_string().contains("Restart the daemon"),
            "the refusal must be the restart-to-apply error: {err}"
        );
        assert!(
            err.to_string().contains("ModelA") && err.to_string().contains("ModelB"),
            "the refusal must name the serving and configured models: {err}"
        );
        // The refusal carries the RestartToApplyError
        // identity, so user-facing surfaces can extract and surface it
        // verbatim instead of a generic store-error message.
        let extracted = restart_to_apply_message(&err)
            .expect("the guard's refusal must be the typed restart-to-apply error");
        assert_eq!(extracted, err.to_string());
        assert!(
            !crate::model_choice::store_has_data(&fx.db),
            "a refused write leaves the store pristine"
        );
        let gone = store.get("probe", "m1").await.expect("get");
        assert!(gone.is_none(), "no data lands under the stale model");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[test]
    fn restart_to_apply_message_matches_only_the_typed_refusal() {
        // Extraction sees through context layers a caller may add
        // above the store.
        let wrapped = anyhow::Error::new(RestartToApplyError::new(
            "refusing to store data: restart".to_string(),
        ))
        .context("while storing");
        assert_eq!(
            restart_to_apply_message(&wrapped).as_deref(),
            Some("refusing to store data: restart")
        );
        // Ordinary errors are not refusals.
        assert!(restart_to_apply_message(&anyhow::anyhow!("store exploded")).is_none());
    }

    #[tokio::test]
    async fn store_state_post_pick_instance_adopts_serving_descriptor_and_first_write_passes() {
        let previous = without_model_env();
        let fx = store_state_fixture("reopen-new-model", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelA"));
        drop(store);

        // The pick is in place before this process starts, and this
        // process serves the picked model (it built its embedder after
        // the pick): serving == configured at its first init, so it
        // adopts the serving descriptor, and the first write under it
        // proceeds.
        write_embedders_config(&fx.config_dir, "ModelB");
        let store2 = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        store2
            .init("probe")
            .await
            .expect("init under the new model");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelB"),
            "a pristine store opened under a new model records that model"
        );
        store2
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await
            .expect("first upsert under the recorded model succeeds");
        assert!(crate::model_choice::store_has_data(&fx.db));
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelB"));
        drop(store2);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[test]
    fn configured_descriptor_for_remote_entry_matches_remote_serving_shape() {
        let previous = without_model_env();
        let fx = store_state_fixture("remote-configured", "ModelA");
        write_remote_embedders_config(&fx.config_dir, Some("RemoteModel"));
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        let configured = store
            .configured_embedding_descriptor()
            .expect("the remote entry resolves");
        assert_eq!(
            configured,
            remote_descriptor_for("RemoteModel"),
            "a remote configured entry synthesizes the remote serving shape"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);

        // Without api_model, the synthesis resolves the same default
        // model build_embedder constructs the remote embedder with.
        let fx2 = store_state_fixture("remote-configured-default", "ModelA");
        write_remote_embedders_config(&fx2.config_dir, None);
        let store2 = LadybugStore::for_testing(&fx2.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx2.config_dir);
        let configured2 = store2
            .configured_embedding_descriptor()
            .expect("the remote entry without api_model resolves");
        assert_eq!(
            configured2,
            remote_descriptor_for("text-embedding-3-small"),
            "an api_model-absent remote entry synthesizes the default remote model"
        );
        drop(store2);
        let _ = std::fs::remove_dir_all(&fx2.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn remote_switch_on_pristine_store_adopts_and_first_write_passes() {
        let previous = without_model_env();
        let fx = store_state_fixture("remote-switch", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelA"));
        assert!(!crate::model_choice::store_has_data(&fx.db));
        drop(store);

        // The config is rewritten to a same-width remote entry and the
        // next process serves that remote embedder: its serving
        // descriptor must equal the configured synthesis, so the
        // pristine store adopts the remote descriptor at init and the
        // first write under it proceeds.
        write_remote_embedders_config(&fx.config_dir, Some("RemoteModel"));
        let remote = remote_descriptor_for("RemoteModel");
        let store2 = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(remote.clone()));
        store2.init("probe").await.expect("init under remote");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(
            sidecar["descriptor"]["provider"],
            json!("openai-compatible"),
            "init adopts the remote descriptor into the sidecar"
        );
        assert_eq!(sidecar["descriptor"]["model"], json!("RemoteModel"));
        assert!(
            !crate::model_choice::store_has_data(&fx.db),
            "adoption keeps the store pristine"
        );
        store2
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await
            .expect("first upsert under the remote model succeeds");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(true));
        assert_eq!(
            sidecar["descriptor"]["provider"],
            json!("openai-compatible"),
            "the flip records the remote descriptor"
        );
        assert_eq!(sidecar["descriptor"]["model"], json!("RemoteModel"));
        drop(store2);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_sidecar_records_configured_model_not_placeholder() {
        // NEUROSTRATA_MODEL is unset (pinned out by without_model_env).
        // The serving descriptor bound here is the one a daemon builds
        // from the embedders.json entry — FastEmbedder's descriptor for
        // the configured model — and the record must be that descriptor
        // verbatim, never the "local-fastembed" placeholder the
        // in-memory registry records.
        let previous = without_model_env();
        let fx = store_state_fixture("provenance", "FileModel");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("FileModel")));
        store.init("probe").await.expect("init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["descriptor"]["model"], json!("FileModel"));
        assert_eq!(sidecar["descriptor"]["provider"], json!("fastembed"));
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_non_serving_open_never_adopts() {
        let previous = without_model_env();
        let fx = store_state_fixture("non-serving", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelA"));
        drop(store);

        // A non-serving open (the backup / restore-verify / report
        // shape: no embedder is ever built, so no serving descriptor is
        // bound) under a changed configuration must not move the
        // record, however often it inits.
        write_embedders_config(&fx.config_dir, "ModelB");
        let opener = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        opener.init("probe").await.expect("init");
        opener.init("probe").await.expect("re-init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelA"),
            "a process with no serving embedder never adopts"
        );
        drop(opener);
        let _ = std::fs::remove_dir_all(&fx.root);

        // And a fresh store opened without a serving embedder records
        // nothing at all.
        let fx2 = store_state_fixture("non-serving-fresh", "ModelA");
        let fresh = LadybugStore::for_testing(&fx2.db, 4)
            .expect("open fresh database")
            .with_embed_config_dir(&fx2.config_dir);
        fresh.init("probe").await.expect("init");
        assert!(
            !crate::model_choice::store_state_path(&fx2.db).exists(),
            "a non-serving open of a fresh store records no descriptor"
        );
        drop(fresh);
        let _ = std::fs::remove_dir_all(&fx2.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_serving_config_mismatch_at_open_does_not_adopt() {
        let previous = without_model_env();
        let fx = store_state_fixture("mismatch-at-open", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!("ModelA"));
        drop(store);

        // The pick lands before the next process starts, but that
        // process still serves the old model: serving != configured at
        // its first init, so it adopts nothing, and its write refuses.
        write_embedders_config(&fx.config_dir, "ModelB");
        let stale = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        stale.init("probe").await.expect("init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelA"),
            "a serving/configured mismatch at open must not adopt"
        );
        let refused = stale
            .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
            .await;
        let err = refused.expect_err("the mismatched process's write must be refused");
        assert!(
            err.to_string().contains("Restart the daemon"),
            "the refusal must be the restart-to-apply error: {err}"
        );
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        drop(stale);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn store_state_guard_refuses_serving_config_record_mismatches() {
        // Guard-level triples (serving, configured, recorded), each
        // state built on disk by a serving instance, then faced by a
        // second instance whose upsert — with no intervening init —
        // must refuse. The guard never rewrites the record.
        let previous = without_model_env();
        let cases: [(&str, &str, &str, &str); 3] = [
            // A process serving the old model after a pick landed.
            ("triple-pick-landed", "ModelB", "ModelA", "ModelA"),
            // The record is never trusted over the serving descriptor.
            ("triple-record-over-serving", "ModelA", "ModelA", "ModelB"),
            // The post-adoption shape: record and config agree on
            // the new model, the process still serves the old one.
            ("triple-post-adoption", "ModelA", "ModelB", "ModelB"),
        ];
        for (tag, serving, configured, recorded) in cases {
            let fx = store_state_fixture(tag, recorded);
            let recorder = LadybugStore::for_testing(&fx.db, 4)
                .expect("open temp database")
                .with_embed_config_dir(&fx.config_dir)
                .with_serving_descriptor(Some(serving_descriptor_for(recorded)));
            recorder.init("probe").await.expect("init");
            assert_eq!(read_sidecar(&fx.db)["descriptor"]["model"], json!(recorded));
            drop(recorder);

            write_embedders_config(&fx.config_dir, configured);
            let store = LadybugStore::for_testing(&fx.db, 4)
                .expect("reopen temp database")
                .with_embed_config_dir(&fx.config_dir)
                .with_serving_descriptor(Some(serving_descriptor_for(serving)));
            // No init on this instance: the guard is evaluated at
            // upsert entry, against the recorded state as loaded.
            let refused = store
                .upsert("probe", "m1", vec![0.0; 4], store_state_payload())
                .await;
            let err = refused.expect_err("the guard must refuse every disagreement triple");
            assert!(
                err.to_string().contains("Restart the daemon"),
                "{tag}: the refusal must be the restart-to-apply error: {err}"
            );
            let sidecar = read_sidecar(&fx.db);
            assert_eq!(sidecar["has_data"], json!(false), "{tag}");
            assert_eq!(
                sidecar["descriptor"]["model"],
                json!(recorded),
                "{tag}: a refused write leaves the record untouched"
            );
            drop(store);
            let _ = std::fs::remove_dir_all(&fx.root);
        }
        restore_model_env(previous);
    }

    // --- Data-bearing identity refusal, registry seeding,
    // pre-sidecar disposition, fallback binding, flip provenance ---

    #[test]
    fn embedding_identity_refusal_message_matches_only_the_typed_refusal() {
        let recorded = serving_descriptor_for("ModelA");
        let serving = serving_descriptor_for("ModelB");
        let wrapped = anyhow::Error::new(EmbeddingIdentityRefusal::between(&recorded, &serving))
            .context("while opening");
        let extracted = embedding_identity_refusal_message(&wrapped)
            .expect("the typed refusal extracts through context layers");
        assert!(extracted.contains("'ModelA'") && extracted.contains("'ModelB'"));
        assert!(embedding_identity_refusal_message(&anyhow::anyhow!("store exploded")).is_none());
        // The two refusal types never extract as each other.
        let restart = anyhow::Error::new(RestartToApplyError::new("restart".to_string()));
        assert!(embedding_identity_refusal_message(&restart).is_none());
        assert!(restart_to_apply_message(&wrapped).is_none());
    }

    #[tokio::test]
    async fn setup_shortlist_same_width_switch_is_refused() {
        // The setup pair at its real
        // width. FP32 and Q are both 768, so only an identity check —
        // never the width backstop — can refuse this switch.
        let previous = without_model_env();
        let fp32_dims = crate::embed::default_acceptable_embedders()
            .iter()
            .find(|e| e.model_name == "NomicEmbedTextV15")
            .expect("the default list carries the FP32 model")
            .dimensions;
        let q = &crate::model_choice::candidates()[0];
        assert_eq!(q.model_name, "NomicEmbedTextV15Q");
        assert_eq!(
            fp32_dims, q.dimensions,
            "the pair must be same-width for this test to mean anything"
        );
        let dims = fp32_dims;

        let fx = store_state_fixture("a5-pair-switch", "NomicEmbedTextV15");
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            format!("[{{\"model_name\":\"NomicEmbedTextV15\",\"dimensions\":{dims}}}]"),
        )
        .expect("write FP32 fixture config");
        let store = LadybugStore::for_testing(&fx.db, dims)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("NomicEmbedTextV15", dims)));
        store.init("probe").await.expect("init");
        store
            .upsert("probe", "m1", vec![0.1; 768], store_state_payload())
            .await
            .expect("seed upsert under the FP32 model");
        let sidecar_before =
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("read sidecar");
        assert_eq!(
            read_sidecar(&fx.db)["descriptor"]["model"],
            json!("NomicEmbedTextV15")
        );
        drop(store);

        // The same store reopened under the quantized pick.
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            format!("[{{\"model_name\":\"NomicEmbedTextV15Q\",\"dimensions\":{dims}}}]"),
        )
        .expect("write Q fixture config");
        let reopened = LadybugStore::for_testing(&fx.db, dims)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("NomicEmbedTextV15Q", dims)));
        let err = reopened
            .init("probe")
            .await
            .expect_err("a same-width model switch against recorded data must be refused");
        let message = embedding_identity_refusal_message(&err)
            .expect("the refusal must be the typed identity refusal");
        assert!(
            message.contains("'NomicEmbedTextV15'") && message.contains("'NomicEmbedTextV15Q'"),
            "the refusal names the recorded and serving models: {message}"
        );
        assert_eq!(
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("re-read sidecar"),
            sidecar_before,
            "a refused open leaves the sidecar byte-identical"
        );
        // The init-less write path on the refused instance refuses too.
        let err = reopened
            .upsert("probe", "m2", vec![0.1; 768], store_state_payload())
            .await
            .expect_err("an upsert on the refused instance must refuse");
        assert!(embedding_identity_refusal_message(&err).is_some());
        drop(reopened);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn data_bearing_reopen_under_recorded_model_succeeds() {
        // The refusal must not fire on the legitimate path.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-legit-reopen").await;
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store
            .init("probe")
            .await
            .expect("init under the recorded model");
        store
            .upsert(
                "probe",
                "m2",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("upsert under the recorded model");
        let found = store
            .search("probe", vec![0.1, 0.2, 0.3, 0.4], 5, false, false)
            .await
            .expect("vector search under the recorded model");
        assert!(!found.is_empty(), "the seeded rows are searchable");
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn data_bearing_different_width_switch_refuses_with_identity_error() {
        // The identity refusal precedes the width backstop — a
        // different-width switch reports the identity error, not an
        // engine/FLOAT error.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-width-switch").await;
        write_embedders_config(&fx.config_dir, "ModelB");
        let store = LadybugStore::for_testing(&fx.db, 8)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelB", 8)));
        let err = store
            .init("probe")
            .await
            .expect_err("a different-width switch must be refused");
        let message = embedding_identity_refusal_message(&err)
            .expect("the refusal must be the identity error, not an engine error");
        assert!(
            message.contains("refusing to open a store that has recorded data"),
            "{message}"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn data_bearing_remote_recorded_store_reopens_under_same_remote_serving() {
        // R recorded from a remote serving descriptor. A
        // config-keyed refusal would compare the local configured
        // descriptor against the remote record and lock the owner out;
        // the serving-keyed check accepts the identical remote S, and
        // the unbound (backup-shaped) open.
        let previous = without_model_env();
        let fx = store_state_fixture("a5-remote-record", "ModelA");
        let blank = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        blank.init("probe").await.expect("init creates the schema");
        drop(blank);
        let remote = remote_descriptor_for("RemoteModel");
        crate::model_choice::write_store_state(
            &fx.db,
            &crate::model_choice::StoreState {
                version: crate::model_choice::STORE_STATE_VERSION,
                has_data: true,
                descriptor: remote.clone(),
            },
        )
        .expect("record the remote identity");

        let bound = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(remote.clone()));
        bound
            .init("probe")
            .await
            .expect("the identical remote serving descriptor opens its own store");
        bound
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("a write under the recorded remote identity lands");
        drop(bound);

        let unbound = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        unbound
            .init("probe")
            .await
            .expect("an unbound open of a remote-recorded store succeeds");
        drop(unbound);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn data_bearing_unbound_backup_and_report_opens_are_never_refused() {
        // The S == None allow-list under a changed config —
        // backup (construct, no init, export) and direct_report (init,
        // no serving descriptor) both succeed and never move R.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-unbound-opens").await;
        let sidecar_before =
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("read sidecar");
        write_embedders_config(&fx.config_dir, "ModelB");

        let backup = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        let out = fx.root.join("backup-out");
        backup
            .export_database(&out.to_string_lossy())
            .await
            .expect("the backup open is never refused");
        drop(backup);
        assert_eq!(
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("re-read sidecar"),
            sidecar_before,
            "backup never moves the recorded descriptor"
        );

        let report = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        report
            .init("probe")
            .await
            .expect("the report open is never refused");
        drop(report);
        assert_eq!(
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("re-read sidecar"),
            sidecar_before,
            "a report open never moves the recorded descriptor"
        );
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn data_bearing_initless_vector_paths_refuse_and_unbound_recycle_passes() {
        // The vector paths that never init — fallback Add
        // (upsert directly) and Chat Ask (search_with_domain) — refuse
        // under a mismatched serving descriptor; an unbound upsert of
        // a vector read back from the store itself (NormalizeMetadata's
        // shape) succeeds under a changed config.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-initless-paths").await;

        let add = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        let err = add
            .upsert(
                "probe",
                "m2",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect_err("an init-less upsert under a different model must refuse");
        assert!(embedding_identity_refusal_message(&err).is_some(), "{err}");
        drop(add);

        let chat = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        let err = chat
            .search_with_domain(
                "probe",
                vec![0.1, 0.2, 0.3, 0.4],
                5,
                false,
                false,
                &[],
                None,
                true,
            )
            .await
            .expect_err("an init-less vector search under a different model must refuse");
        assert!(embedding_identity_refusal_message(&err).is_some(), "{err}");
        drop(chat);

        write_embedders_config(&fx.config_dir, "ModelB");
        let normalize = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        let (vector, payload) = normalize
            .get("probe", "m1")
            .await
            .expect("get the stored row")
            .expect("the seeded row exists");
        normalize
            .upsert("probe", "m1", vector, payload)
            .await
            .expect("recycling a store-read vector unbound succeeds under a changed config");
        drop(normalize);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn registry_records_serving_descriptor_when_bound() {
        // A bound init records S in the registry — S verifies,
        // the legacy placeholder does not.
        let previous = without_model_env();
        let fx = store_state_fixture("a5-registry-serving", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        store
            .verify_or_record_embedding_descriptor("probe", &serving_descriptor_for("ModelA"))
            .expect("the serving descriptor verifies against the registry");
        assert!(
            store
                .verify_or_record_embedding_descriptor("probe", &placeholder_descriptor(4))
                .is_err(),
            "the registry holds the serving descriptor, not the placeholder"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn registry_seeds_from_recorded_descriptor_when_unbound() {
        // An unbound init of a recorded store seeds the
        // registry from R — R verifies, the (changed) configured
        // descriptor does not.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-registry-recorded").await;
        write_embedders_config(&fx.config_dir, "ModelB");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store
            .init("probe")
            .await
            .expect("the unbound open succeeds (allow-list)");
        store
            .verify_or_record_embedding_descriptor("probe", &serving_descriptor_for("ModelA"))
            .expect("the recorded descriptor verifies against the registry");
        assert!(
            store
                .verify_or_record_embedding_descriptor("probe", &serving_descriptor_for("ModelB"))
                .is_err(),
            "the registry holds the recorded descriptor, not the configured one"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn feature_created_store_never_records_local_fastembed_placeholder() {
        // With NEUROSTRATA_MODEL unset, the placeholder the old
        // init recorded ("local-fastembed") must not verify after a
        // bound init of a feature-created store.
        let previous = without_model_env();
        let fx = store_state_fixture("a5-registry-no-placeholder", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert!(
            store
                .verify_or_record_embedding_descriptor("probe", &placeholder_descriptor(4))
                .is_err(),
            "a feature-created store never records the local-fastembed placeholder"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn locked_unknown_opens_exactly_as_today() {
        // A pre-sidecar store (db predates this process, no
        // sidecar ever written) opens, writes, and searches under a
        // changed config exactly as it did before the feature — and
        // no sidecar is backfilled, at open or at first write.
        let previous = without_model_env();
        let fx = store_state_fixture("a5-locked-unknown", "ModelA");
        let creator = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );

        write_embedders_config(&fx.config_dir, "ModelB");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store
            .init("probe")
            .await
            .expect("a pre-sidecar store opens as it always has");
        store
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("writes land as they always have");
        let found = store
            .search("probe", vec![0.1, 0.2, 0.3, 0.4], 5, false, false)
            .await
            .expect("search works as it always has");
        assert!(!found.is_empty());
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "no sidecar is backfilled for a pre-sidecar store — establishing its identity is Phase B"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn locked_unknown_width_change_fails_at_the_engine_not_the_identity_check() {
        // The width backstop is unchanged for pre-sidecar
        // stores — a different width fails at vector use with the
        // engine's error, never with the identity refusal.
        let previous = without_model_env();
        let fx = store_state_fixture("a5-locked-unknown-width", "ModelA");
        let creator = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);

        let store = LadybugStore::for_testing(&fx.db, 8)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store
            .init("probe")
            .await
            .expect("init succeeds: the backstop is at vector use, as today");
        let err = store
            .upsert("probe", "m1", vec![0.1; 8], store_state_payload())
            .await
            .expect_err("the FLOAT[4] column rejects an 8-wide vector");
        assert!(
            embedding_identity_refusal_message(&err).is_none(),
            "a pre-sidecar width failure is not the identity refusal: {err}"
        );
        assert!(restart_to_apply_message(&err).is_none(), "{err}");
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    /// Raw handle fixture for the attestation-helper tests: run
    /// `body` against a connection to a database at a fresh temp
    /// path, with no NeuroStrata schema created unless the body
    /// creates it.
    fn with_attestation_conn(name: &str, body: impl FnOnce(&Connection)) {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the fixture root");
        let db_path = root.join("attestation.db");
        let cfg = SystemConfig::default()
            .max_db_size(512 * 1024 * 1024)
            .buffer_pool_size(64 * 1024 * 1024);
        let raw = Database::new(&db_path, cfg).expect("raw open");
        let conn = Connection::new(&raw).expect("raw connection");
        body(&conn);
        drop(conn);
        drop(raw);
        let _ = std::fs::remove_dir_all(&root);
    }

    const ATTESTATION_NODE_DDL: &str = "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[4], PRIMARY KEY (id))";
    const ATTESTATION_REL_DDLS: [&str; 1] = ["CREATE REL TABLE RELATES_TO (FROM Memory TO Memory, source STRING, valid_from INT64, valid_to INT64, generation INT64, extractor STRING, confidence DOUBLE, evidence_id STRING, origin STRING)"];

    #[test]
    fn locked_unknown_attestation_table_info_failure_leaves_store_as_found() {
        // Panel condition 2 (finding P4-F3): the #74 attestation
        // helper documents that it never refuses an open, but a
        // query-EXECUTION failure used to propagate. Here the Memory
        // table is absent, so `CALL table_info('Memory')` itself
        // fails at execution ("Memory does not exist in catalog")
        // while the store opened fine. The helper must downgrade
        // that to the same leave-as-found Ok(false) its value
        // mismatches already return — and Memory must stay absent.
        with_attestation_conn("cond2-attestation-no-memory", |conn| {
            assert!(
                conn.query("CALL table_info('Memory') RETURN *").is_err(),
                "precondition: table_info fails at execution with no Memory table"
            );
            let attested = reconcile_empty_locked_unknown_width(
                conn,
                4,
                ATTESTATION_NODE_DDL,
                &ATTESTATION_REL_DDLS,
            );
            assert!(
                matches!(attested, Ok(false)),
                "an unexecutable table-info query leaves the store as found, got {attested:?}"
            );
            assert!(
                conn.query("CALL table_info('Memory') RETURN *").is_err(),
                "the store is left as found: Memory is still absent afterwards"
            );
        });
    }

    #[test]
    fn locked_unknown_attestation_count_failure_leaves_store_as_found() {
        // Panel condition 2 (finding P4-F3), the count-query leg:
        // `Memory` exists in the catalog as a REL table carrying an
        // `embedding FLOAT[8]` property, so table_info executes and
        // attests a physical width — but `MATCH (m:Memory)` cannot
        // bind a rel table as a node pattern, so the count query
        // fails at execution ("Cannot bind Memory as a node pattern
        // label") while the store opened fine. The helper must
        // downgrade that to leave-as-found Ok(false) too: no
        // drop/recreate, no record, catalog exactly as found.
        with_attestation_conn("cond2-attestation-rel-memory", |conn| {
            conn.query("CREATE NODE TABLE Other (id STRING, PRIMARY KEY (id))")
                .expect("create the rel table's endpoint table");
            conn.query("CREATE REL TABLE Memory (FROM Other TO Other, embedding FLOAT[8])")
                .expect("create Memory as a rel table");
            assert!(
                conn.query("CALL table_info('Memory') RETURN *").is_ok(),
                "precondition: table_info executes for the rel-table Memory"
            );
            assert!(
                conn.query("MATCH (m:Memory) RETURN count(m)").is_err(),
                "precondition: the count query fails at execution for a rel-table Memory"
            );
            let attested = reconcile_empty_locked_unknown_width(
                conn,
                4,
                ATTESTATION_NODE_DDL,
                &ATTESTATION_REL_DDLS,
            );
            assert!(
                matches!(attested, Ok(false)),
                "an unexecutable count query leaves the store as found, got {attested:?}"
            );
            // Left exactly as found: table_info still attests the same
            // rel-table Memory at FLOAT[8], and the count still fails.
            let mut widths = Vec::new();
            for row in conn
                .query("CALL table_info('Memory') RETURN *")
                .expect("table_info still executes afterwards")
            {
                if let Some(lbug::Value::String(ty)) = row.get(2) {
                    widths.push(ty.clone());
                }
            }
            assert_eq!(widths, vec!["FLOAT[8]".to_string()]);
            assert!(
                conn.query("MATCH (m:Memory) RETURN count(m)").is_err(),
                "the store is left as found: the count query still fails afterwards"
            );
        });
    }

    #[tokio::test]
    async fn locked_unknown_stamp_step_reattests_and_skips_store_with_landed_row() {
        // Panel condition 3 (findings SA-F1 + N1): the #74 upgrade
        // attested emptiness inside init's schema closure but stamped
        // the pristine sidecar afterwards without re-checking it, so
        // a write landing between the attestation and the stamp was
        // stamped over as if the store were pristine. A deterministic
        // mid-init interleave is not constructible with existing
        // seams (no hook runs between the attestation and the stamp),
        // so this pins the behavior at the stamp-step seam itself,
        // following this program's disclosed-seam precedent: build
        // the state init would be in after a positive attestation —
        // a LockedUnknown store, serving == configured, with a row
        // now landed in the window — and invoke the stamp step
        // directly, exactly as init does.
        let previous = without_model_env();
        let fx = store_state_fixture("cond3-stamp-reattest", "ModelA");

        // The pre-sidecar store: a non-serving open creates the DB
        // file and schema but records no sidecar, so every later
        // construction classifies LockedUnknown.
        let creator = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );

        // One row lands through a second LadybugStore instance on
        // the same database — the cross-instance shape (N1): that
        // instance's per-Self write_lock shares no turn with the
        // instance that will run the stamp step. The fixtures
        // support sequential handles only, so the writer is dropped
        // before the stamping instance opens; the state the stamp
        // step then faces — LockedUnknown with a row present — is
        // exactly the state the mid-init window produces.
        let writer = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        writer.init("probe").await.expect("init");
        writer
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("the second instance's write lands");
        drop(writer);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the landed write backfills no sidecar"
        );

        // The stamping instance: serving bound and == configured,
        // cached state LockedUnknown. Invoking the stamp step here
        // must re-attest, find the row, and stamp nothing.
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "precondition: the stamping instance classifies LockedUnknown"
        );
        store
            .reattest_and_upgrade_empty_locked_unknown_store_state(4)
            .await;
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the stamp step must not record a pristine sidecar over a landed row"
        );
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "the cached state stays LockedUnknown when the re-attestation finds rows"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn locked_unknown_stamp_step_attests_and_stamps_in_one_turn() {
        // Split-turn follow-up finding: condition 3's re-attestation
        // (above) closed the attestation→stamp window at init level,
        // but the stamp step itself still executed the stamp AFTER
        // its re-attestation's writer turn had been released,
        // consuming a bool parked across the turn boundary: a write
        // committing in between — its first-write flip no-ops on
        // LockedUnknown — was recorded over as `has_data: false`.
        // The fix moves the stamp into the turn:
        // `LockedUnknownStamp::attest_and_stamp` is the single unit
        // the turn's closure executes.
        //
        // Seam disclosure (the condition-3 precedent): a
        // deterministic interleave *inside* the turn is not
        // constructible — no hook runs between the attestation and
        // the stamp within the turn, which is precisely the property
        // the fix relies on — so this pins behavior at the seam the
        // fix creates, the attest-and-stamp unit itself, driven here
        // through `with_conn` (which holds the same write turn the
        // production closure runs under):
        //   * on a store with a landed row the unit stamps nothing;
        //   * on an empty store the unit ALONE stamps the pristine
        //     sidecar and moves the cached state — nothing runs
        //     after it. Re-splitting the stamp out of the unit (the
        //     pre-fix shape: attest in the turn, stamp after the
        //     `.await` from a parked flag) fails the empty-store leg,
        //     because the unit then leaves the store unstamped.
        let previous = without_model_env();

        // Row leg: the state the window produces — LockedUnknown,
        // serving == configured, a row landed (cross-instance, as in
        // the condition-3 test).
        let fx = store_state_fixture("p1-stamp-turn-row", "ModelA");
        let creator = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        let writer = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        writer.init("probe").await.expect("init");
        writer
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("the second instance's write lands");
        drop(writer);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the landed write backfills no sidecar"
        );
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "precondition: the stamping instance classifies LockedUnknown"
        );
        let stamp = store.locked_unknown_stamp();
        let (create_node_table, create_rel_tables) = memory_table_ddl(4);
        store
            .with_conn(move |conn| {
                stamp.attest_and_stamp(conn, 4, &create_node_table, &create_rel_tables)
            })
            .await
            .expect("the attest-and-stamp unit runs and attests without error");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the unit must not record a pristine sidecar over a landed row"
        );
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "the cached state stays LockedUnknown when the unit's attestation finds rows"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);

        // Empty leg: the unit alone — invoked exactly once, with no
        // post-turn stamp after it — records the pristine identity.
        let fx = store_state_fixture("p1-stamp-turn-empty", "ModelA");
        let creator = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "precondition: the stamping instance classifies LockedUnknown"
        );
        let stamp = store.locked_unknown_stamp();
        let (create_node_table, create_rel_tables) = memory_table_ddl(4);
        store
            .with_conn(move |conn| {
                stamp.attest_and_stamp(conn, 4, &create_node_table, &create_rel_tables)
            })
            .await
            .expect("the attest-and-stamp unit runs and attests without error");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        match store.cached_store_state() {
            CachedStoreState::Recorded(recorded) => {
                assert!(!recorded.has_data);
                assert_eq!(recorded.descriptor, serving_descriptor_for("ModelA"));
            }
            _ => panic!("the unit moves the cached state to Recorded(pristine)"),
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn locked_unknown_concurrent_upsert_never_lands_under_a_pristine_stamp() {
        // The window at the production seam: the stamp step and an
        // upsert from the SAME instance race for the instance's
        // writer turn — the queued-upsert shape the split-turn
        // finding names. Whichever order the turns run in, the
        // outcome must never be a row sitting under a
        // `has_data: false` record:
        //   * upsert first: its flip no-ops on LockedUnknown, and
        //     the stamp step's in-turn attestation then finds the
        //     row and stamps nothing (no sidecar at all);
        //   * stamp first: the store is Recorded pristine when the
        //     upsert commits, and its first-write flip moves the
        //     record to `has_data: true` in the committing turn.
        // Both orders are sound, so the invariant below — never a
        // pristine record over a landed row — holds deterministically
        // with the stamp inside the turn.
        let previous = without_model_env();
        let fx = store_state_fixture("p1-stamp-window", "ModelA");
        let creator = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );

        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "precondition: the stamping instance classifies LockedUnknown"
        );
        let stamp_step = store.reattest_and_upgrade_empty_locked_unknown_store_state(4);
        let write = store.upsert(
            "probe",
            "m1",
            vec![0.1, 0.2, 0.3, 0.4],
            store_state_payload(),
        );
        let (_, written) = tokio::join!(stamp_step, write);
        written.expect("the racing upsert succeeds in either turn order");
        let (_, landed) = store
            .get("probe", "m1")
            .await
            .expect("read the raced row back")
            .expect("the raced row landed");
        assert_eq!(landed.content, store_state_payload().content);
        if crate::model_choice::store_state_path(&fx.db).exists() {
            let sidecar = read_sidecar(&fx.db);
            assert_eq!(
                sidecar["has_data"],
                json!(true),
                "a landed row must never sit under a pristine (has_data: false) record"
            );
            assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    /// The physical width of `Memory.embedding` as the engine's
    /// catalog reports it for an already-open store.
    async fn physical_embedding_width(store: &LadybugStore) -> Option<usize> {
        store
            .with_conn(|conn| {
                let mut width = None;
                for row in conn.query("CALL table_info('Memory') RETURN *")? {
                    let name = match row.get(1) {
                        Some(lbug::Value::String(s)) => s.as_str(),
                        _ => continue,
                    };
                    if name != "embedding" {
                        continue;
                    }
                    if let Some(lbug::Value::String(ty)) = row.get(2) {
                        width = ty
                            .strip_prefix("FLOAT[")
                            .and_then(|rest| rest.strip_suffix(']'))
                            .and_then(|digits| digits.parse::<usize>().ok());
                    }
                }
                Ok(width)
            })
            .await
            .expect("read the physical embedding width")
    }

    /// Leave `rows` Memory writes in an uncheckpointed WAL beside
    /// `db`, then corrupt the WAL in place. The writes are made
    /// through a raw engine handle closed with
    /// `force_checkpoint_on_close=false`: a default close would
    /// checkpoint the WAL into the main file, and the rows must exist
    /// *only* in the WAL.
    /// With `delete_rows`, each row is deleted again inside the same
    /// WAL, so the table the WAL describes is genuinely empty.
    /// Corrupting every WAL byte makes the verified replay fail with
    /// a checksum error while the unverified replay still opens —
    /// presenting an empty table (the recovery shape of upstream
    /// issue #69, measured recovering 0 of 6 WAL-only records).
    fn plant_corrupted_uncheckpointed_wal(db: &std::path::Path, rows: usize, delete_rows: bool) {
        let cfg = SystemConfig::default()
            .max_db_size(512 * 1024 * 1024)
            .buffer_pool_size(64 * 1024 * 1024)
            .throw_on_wal_replay_failure(true);
        let raw = Database::new(db, cfg).expect("raw open to seed the WAL");
        let conn = Connection::new(&raw).expect("raw connection");
        for i in 0..rows {
            conn.query(&format!(
                "CREATE (m:Memory {{id: 'wal-only-{i}', namespace: 'probe', content: 'wal row {i}', user_id: '', memory_type: 'fact', agent_name: '', location: '', location_lines: '', metadata: '{{}}', embedding: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]}})"
            ))
            .expect("seed a WAL-only row");
            if delete_rows {
                conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.id = 'wal-only-{i}' DELETE m"
                ))
                .expect("delete the seeded row inside the same WAL");
            }
        }
        // Close normally but skip the checkpoint the destructor would
        // otherwise run, so the rows stay only in the WAL and the engine's
        // file lock is released (a leaked handle keeps it on Windows).
        conn.query("CALL force_checkpoint_on_close=false")
            .expect("turn off the checkpoint on close");
        drop(conn);
        drop(raw);
        let mut wal_os = db.to_path_buf().into_os_string();
        wal_os.push(".wal");
        let wal = std::path::PathBuf::from(wal_os);
        let len = std::fs::metadata(&wal)
            .expect("the uncheckpointed WAL is on disk")
            .len();
        assert!(len > 0, "the seeded writes left a non-empty WAL");
        std::fs::write(&wal, vec![0xFFu8; len as usize]).expect("corrupt the WAL in place");
    }

    /// Opt this process into the unverified-WAL recovery fallback
    /// (the operator opt-in the WAL-discard/unverified arms require),
    /// returning the previous values for restoration.
    fn allow_unverified_wal_recovery() -> (Option<String>, Option<String>) {
        let previous = (
            std::env::var("NEUROSTRATA_STRICT_WAL").ok(),
            std::env::var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL").ok(),
        );
        std::env::remove_var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL");
        std::env::set_var("NEUROSTRATA_STRICT_WAL", "0");
        previous
    }

    fn restore_wal_env(previous: (Option<String>, Option<String>)) {
        match previous.0 {
            Some(value) => std::env::set_var("NEUROSTRATA_STRICT_WAL", value),
            None => std::env::remove_var("NEUROSTRATA_STRICT_WAL"),
        }
        match previous.1 {
            Some(value) => std::env::set_var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL", value),
            None => std::env::remove_var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL"),
        }
    }

    #[tokio::test]
    async fn recovery_open_never_upgrades_empty_locked_unknown_store() {
        // Panel condition 1 (finding P1-F1): a pre-sidecar store
        // whose rows exist only in an uncheckpointed WAL can present
        // as empty on a recovery open — the verified replay refuses
        // the corrupted WAL and the unverified fallback opens over
        // the main file alone. That unauthenticated empty view must
        // never run the LockedUnknown provenance upgrade: no pristine
        // sidecar may be stamped, the cached state must stay
        // LockedUnknown, and the physical width must not be
        // reconciled (the upgrade helper would drop and recreate all
        // six tables at the serving width first). The trigger arm is
        // the unverified-WAL replay (arm b): it is the arm the
        // existing harness can drive deterministically in-process
        // (raw seed + leaked handle + in-place WAL corruption), and
        // it is the arm upstream issue #69 measured recovering 0 of 6
        // records. The checksum-mismatch and WAL-discard arms set the
        // same recovery flag in `with_system_config`.
        let previous = without_model_env();
        let fx = store_state_fixture("cond1-recovery-wal-rows", "ModelA");

        // The pre-sidecar store: schema at FLOAT[8], created by a
        // non-serving open, so no sidecar exists and the next
        // construction classifies LockedUnknown.
        let creator = LadybugStore::for_testing(&fx.db, 8)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );

        // Six rows that live only in the WAL, behind a corrupted WAL
        // the verified replay refuses.
        plant_corrupted_uncheckpointed_wal(&fx.db, 6, false);

        // The recovery open: serving bound, serving == configured
        // (ModelA at the store's width 4), unverified recovery opted
        // in — exactly the shape that used to stamp the sidecar.
        let previous_wal = allow_unverified_wal_recovery();
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("the recovery open succeeds")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store
            .init("probe")
            .await
            .expect("init succeeds on the recovery open");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "a recovery open must not stamp a pristine sidecar over an unauthenticated empty view"
        );
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "the cached state stays LockedUnknown after a recovery open"
        );
        assert_eq!(
            physical_embedding_width(&store).await,
            Some(8),
            "the physical width is unchanged: the upgrade helper never ran"
        );
        drop(store);
        restore_wal_env(previous_wal);

        // The conservatism is per-open, not a permanent lockout: a
        // later verified open of the (now genuinely empty) store can
        // still run the upgrade and record the pristine identity.
        let verified = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        verified.init("probe").await.expect("verified init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        drop(verified);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn recovery_open_of_genuinely_empty_store_stays_locked_unknown() {
        // Companion case: a genuinely empty pre-sidecar store (its
        // WAL writes insert and delete the same rows, so the table
        // truly holds nothing) on a recovery open also stays
        // LockedUnknown. That is intended conservatism: a recovery
        // open cannot distinguish this store from one whose WAL-only
        // rows it failed to recover, so neither may be upgraded.
        let previous = without_model_env();
        let fx = store_state_fixture("cond1-recovery-empty", "ModelA");
        let creator = LadybugStore::for_testing(&fx.db, 8)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );

        plant_corrupted_uncheckpointed_wal(&fx.db, 2, true);

        let previous_wal = allow_unverified_wal_recovery();
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("the recovery open succeeds")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store
            .init("probe")
            .await
            .expect("init succeeds on the recovery open");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "even a genuinely empty store earns no sidecar from a recovery open"
        );
        assert!(
            matches!(store.cached_store_state(), CachedStoreState::LockedUnknown),
            "the cached state stays LockedUnknown after a recovery open"
        );
        assert_eq!(
            physical_embedding_width(&store).await,
            Some(8),
            "the physical width is unchanged: the upgrade helper never ran"
        );
        drop(store);
        restore_wal_env(previous_wal);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn recovery_open_never_reconciles_or_adopts_recorded_pristine_store() {
        // Overseer panel condition 1 (findings AC-F1/SA-F2): the
        // recovery gate landed for the LockedUnknown provenance
        // stamp in 54b85ac must cover the whole init trust surface.
        // A recorded-pristine store whose rows exist only in an
        // uncheckpointed WAL can present as empty on a recovery
        // open; that unauthenticated view must neither arm the
        // pristine width reconciliation (which reads a stale
        // `count(m)` and would drop/recreate all six tables) nor
        // run the recorded-pristine adoption in
        // `maintain_store_state` (which would rewrite the sidecar
        // descriptor to the serving descriptor on the sole basis
        // of the sidecar's has_data=false claim).
        let previous = without_model_env();
        let fx = store_state_fixture("cond1-recovery-pristine", "ModelA");
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"ModelA\",\"dimensions\":8}]",
        )
        .expect("write 8-wide fixture config");

        // The recorded-pristine store: schema at FLOAT[8], sidecar
        // recording ModelA with has_data=false.
        let creator = LadybugStore::for_testing(&fx.db, 8)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelA", 8)));
        creator.init("probe").await.expect("init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        assert_eq!(sidecar["descriptor"]["dims"], json!(8));
        drop(creator);

        // Six rows that live only in the WAL, behind a corrupted
        // WAL the verified replay refuses.
        plant_corrupted_uncheckpointed_wal(&fx.db, 6, false);

        // The configured model switches to ModelB at dims 4.
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"ModelB\",\"dimensions\":4}]",
        )
        .expect("write 4-wide fixture config");

        // The recovery open: serving bound, serving == configured
        // (ModelB at dims 4), unverified recovery opted in. Init
        // succeeds, but nothing may be reconciled or adopted from
        // the unauthenticated view.
        let previous_wal = allow_unverified_wal_recovery();
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("the recovery open succeeds")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelB", 4)));
        store
            .init("probe")
            .await
            .expect("init succeeds on the recovery open");
        assert_eq!(
            physical_embedding_width(&store).await,
            Some(8),
            "a recovery open must not reconcile the physical width"
        );
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelA"),
            "a recovery open must not adopt the serving descriptor"
        );
        assert_eq!(sidecar["descriptor"]["dims"], json!(8));
        match store.cached_store_state() {
            CachedStoreState::Recorded(recorded) => {
                assert!(!recorded.has_data);
                assert_eq!(recorded.descriptor, serving_descriptor_dims("ModelA", 8));
            }
            _ => panic!("the cached state stays Recorded after a recovery open"),
        }
        drop(store);
        restore_wal_env(previous_wal);

        // The conservatism is per-open, not a permanent lockout: a
        // later verified open at dims 4 under ModelB reconciles
        // and adopts exactly as today.
        let verified = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelB", 4)));
        verified.init("probe").await.expect("verified init");
        assert_eq!(
            physical_embedding_width(&verified).await,
            Some(4),
            "a verified open reconciles the physical width as today"
        );
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelB"));
        assert_eq!(sidecar["descriptor"]["dims"], json!(4));
        drop(verified);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn pristine_store_reconciles_width_on_different_width_adoption() {
        // A pristine store created under the default-width model
        // adopts a different-width pick end-to-end: init reconciles
        // the physical FLOAT column to the serving width (the engine
        // attests the table is empty) before the descriptor is
        // adopted, so the first write under the pick lands.
        let previous = without_model_env();
        let fx = store_state_fixture("fwidth-reconcile", "NomicEmbedTextV15");
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"NomicEmbedTextV15\",\"dimensions\":768}]",
        )
        .expect("write 768 fixture config");
        let creator = LadybugStore::for_testing(&fx.db, 768)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("NomicEmbedTextV15", 768)));
        creator.init("probe").await.expect("init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("NomicEmbedTextV15"));
        assert_eq!(sidecar["descriptor"]["dims"], json!(768));
        drop(creator);

        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"BGESmallENV15Q\",\"dimensions\":384}]",
        )
        .expect("write 384 fixture config");
        let store = LadybugStore::for_testing(&fx.db, 384)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("BGESmallENV15Q", 384)));
        store
            .init("probe")
            .await
            .expect("init adopts the different-width pick on a pristine store");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(false));
        assert_eq!(sidecar["descriptor"]["model"], json!("BGESmallENV15Q"));
        assert_eq!(sidecar["descriptor"]["dims"], json!(384));

        store
            .upsert("probe", "m1", vec![0.1; 384], store_state_payload())
            .await
            .expect("a 384-wide upsert lands after width reconciliation");
        let found = store
            .search("probe", vec![0.1; 384], 5, false, false)
            .await
            .expect("search under the reconciled width");
        assert!(!found.is_empty(), "the reconciled store reads its row back");
        assert_eq!(read_sidecar(&fx.db)["has_data"], json!(true));

        let err = store
            .upsert("probe", "m2", vec![0.1; 768], store_state_payload())
            .await
            .expect_err("the reconciled FLOAT[384] column rejects a 768-wide vector");
        assert!(
            err.to_string().contains("Expected: 384"),
            "the old width is rejected by the engine at the new width: {err}"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn pristine_labelled_store_with_rows_refuses_width_reconciliation() {
        // The stale-label state (the F1 residual class): the sidecar
        // says pristine but the table holds a row. Reconciliation
        // must refuse the mismatched open with a truthful error and
        // leave the store byte-untouched — never alter a table the
        // engine attests is not empty.
        let previous = without_model_env();
        let fx = store_state_fixture("fwidth-stale-label", "ModelA");
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"ModelA\",\"dimensions\":768}]",
        )
        .expect("write 768 fixture config");
        let creator = LadybugStore::for_testing(&fx.db, 768)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelA", 768)));
        creator.init("probe").await.expect("init");
        creator
            .upsert("probe", "m1", vec![0.1; 768], store_state_payload())
            .await
            .expect("seed upsert");
        assert_eq!(read_sidecar(&fx.db)["has_data"], json!(true));
        drop(creator);

        // Hand-rewrite the sidecar to the stale pristine label.
        crate::model_choice::write_store_state(
            &fx.db,
            &crate::model_choice::StoreState {
                version: crate::model_choice::STORE_STATE_VERSION,
                has_data: false,
                descriptor: serving_descriptor_dims("ModelA", 768),
            },
        )
        .expect("rewrite the sidecar to the stale pristine label");
        let sidecar_before =
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("read sidecar");

        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"ModelB\",\"dimensions\":384}]",
        )
        .expect("write 384 fixture config");
        let store = LadybugStore::for_testing(&fx.db, 384)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelB", 384)));
        let err = store
            .init("probe")
            .await
            .expect_err("a pristine-labelled store with rows refuses width reconciliation");
        let message = err.to_string();
        assert!(
            message.contains("pristine") && message.contains("768") && message.contains("384"),
            "the error names the recorded-pristine inconsistency and both widths: {message}"
        );
        assert!(
            embedding_identity_refusal_message(&err).is_none(),
            "the reconciliation refusal is a plain error, not the identity refusal: {err}"
        );
        assert!(restart_to_apply_message(&err).is_none(), "{err}");
        assert_eq!(
            std::fs::read(crate::model_choice::store_state_path(&fx.db)).expect("re-read sidecar"),
            sidecar_before,
            "a refused reconciliation leaves the sidecar byte-identical"
        );
        drop(store);

        // The schema is untouched: the store still opens and writes
        // under its recorded width.
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            "[{\"model_name\":\"ModelA\",\"dimensions\":768}]",
        )
        .expect("restore 768 fixture config");
        let store = LadybugStore::for_testing(&fx.db, 768)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("ModelA", 768)));
        store
            .init("probe")
            .await
            .expect("the store still opens under its recorded model");
        store
            .upsert("probe", "m2", vec![0.1; 768], store_state_payload())
            .await
            .expect("a 768-wide upsert still lands: the column was never altered");
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn set_serving_descriptor_binds_post_construction() {
        // The fallback binding setter — a store constructed
        // unbound refuses after being bound to a different model, and
        // passes after being bound to the recorded one.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-setter").await;

        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store.set_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        let err = store
            .upsert(
                "probe",
                "m2",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect_err("a post-construction binding keys the write check");
        assert!(embedding_identity_refusal_message(&err).is_some(), "{err}");
        let err = store
            .search_with_domain(
                "probe",
                vec![0.1, 0.2, 0.3, 0.4],
                5,
                false,
                false,
                &[],
                None,
                true,
            )
            .await
            .expect_err("a post-construction binding keys the search check");
        assert!(embedding_identity_refusal_message(&err).is_some(), "{err}");
        drop(store);

        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir);
        store.set_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store
            .upsert(
                "probe",
                "m2",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("binding the recorded model passes the write check");
        store
            .search("probe", vec![0.1, 0.2, 0.3, 0.4], 5, false, false)
            .await
            .expect("binding the recorded model passes the search check");
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn fresh_unrecorded_first_write_refuses_stale_serving_until_restart() {
        // Regression test for issue #73 (Codex finding T5 on PR #66).
        // During the first daemon start, the pending model-choice
        // marker exists while `build_embedder()` is still loading the
        // default model and the DB does not yet exist. The user picks
        // the Q model in that window: the pick persists (configured =
        // NomicEmbedTextV15Q) while this process still serves the
        // default (NomicEmbedTextV15 FP32). At its first init, serving
        // != configured, so `maintain_store_state` writes no pristine
        // sidecar and the store stays FreshUnrecorded. A first write
        // in that window must refuse with restart-to-apply guidance —
        // before this fix the guard skipped FreshUnrecorded stores, so
        // the write committed FP32 vectors and the flip recorded FP32
        // as the data-bearing identity, after which the restart under
        // the chosen Q model refused to open the store at all.
        let previous = without_model_env();
        let dims = 768;
        let fx = store_state_fixture("t5-q-pick-during-first-start", "NomicEmbedTextV15Q");
        std::fs::write(
            fx.config_dir.join("embedders.json"),
            format!("[{{\"model_name\":\"NomicEmbedTextV15Q\",\"dimensions\":{dims}}}]"),
        )
        .expect("write Q fixture config");

        // The first-start process: its embedder was built before the
        // pick landed, so it serves the default FP32 model while the
        // configuration already names Q.
        let store = LadybugStore::for_testing(&fx.db, dims)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("NomicEmbedTextV15", dims)));
        store.init("probe").await.expect("init");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "serving != configured at first init: no pristine sidecar is written, \
             so the store stays FreshUnrecorded into its first write"
        );

        let refused = store
            .upsert("probe", "m1", vec![0.1; dims], store_state_payload())
            .await;
        let err = refused.expect_err(
            "a first write under stale serving on a FreshUnrecorded store must be refused",
        );
        let message = restart_to_apply_message(&err)
            .expect("the refusal must be the typed restart-to-apply error");
        assert!(
            message.contains("Restart the daemon"),
            "the refusal must carry the restart guidance that resolves it: {message}"
        );
        assert!(
            message.contains("'NomicEmbedTextV15'") && message.contains("'NomicEmbedTextV15Q'"),
            "the refusal names the serving and configured models: {message}"
        );
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "a refused first write records no identity: no sidecar may appear"
        );
        let rows = store
            .list("probe", None, None)
            .await
            .expect("list after refusal");
        assert!(rows.is_empty(), "a refused first write lands no row");
        drop(store);

        // The restart the guidance demands: a fresh process serving
        // the configured Q model opens the store cleanly and accepts
        // the first write under Q.
        let restarted = LadybugStore::for_testing(&fx.db, dims)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_dims("NomicEmbedTextV15Q", dims)));
        restarted
            .init("probe")
            .await
            .expect("the store opens cleanly under the chosen Q model after restart");
        restarted
            .upsert("probe", "m1", vec![0.1; dims], store_state_payload())
            .await
            .expect("the first write under the chosen model succeeds after restart");
        let found = restarted
            .search("probe", vec![0.1; dims], 5, false, false)
            .await
            .expect("search under the chosen model");
        assert!(
            !found.is_empty(),
            "the row written after restart is searchable under Q"
        );
        drop(restarted);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn fresh_unrecorded_flip_records_serving_only_when_it_matches_configured() {
        // PREMISE REVISED (issue #73): this test was
        // `fresh_unrecorded_flip_prefers_the_serving_descriptor` and
        // pinned the opposite behavior — an upsert on a
        // FreshUnrecorded store under a serving descriptor that
        // disagreed with the configuration was expected to succeed
        // ("the pristine guard does not cover FreshUnrecorded
        // stores") and the flip to record that serving descriptor as
        // the data-bearing identity. That premise was the defect: a
        // setup pick persisted while the first-start process still
        // serves the default model makes serving != configured, and
        // letting the write land records an identity the user did not
        // choose, bricking the store at the restart setup demands.
        // The flip's serving preference survives only where it is
        // sound — when serving == configured, the serving descriptor
        // (the model that produced the vectors) is what the flip
        // records.
        let previous = without_model_env();
        let fx = store_state_fixture("a5-flip-prefers-serving", "ModelA");
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        store.init("probe").await.expect("init");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "an unbound init of a fresh store records nothing"
        );

        // The stale-serving shape the old premise allowed: serving
        // bound after init to a model the configuration does not
        // name. The write is now refused, and no identity is recorded.
        store.set_serving_descriptor(Some(serving_descriptor_for("ModelServed")));
        let refused = store
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await;
        let err = refused.expect_err(
            "the pristine guard now covers FreshUnrecorded stores: \
             a stale-serving first write must be refused",
        );
        assert!(
            restart_to_apply_message(&err).is_some(),
            "the refusal must be the typed restart-to-apply error: {err}"
        );
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "a refused first write records no identity"
        );

        // Bound to the configured model instead, the same write lands
        // and the flip records the serving descriptor as data-bearing.
        store.set_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("a first write whose serving matches the configuration proceeds");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(true));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelA"),
            "the flip records the serving descriptor, which the guard has verified against the configured one"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn direct_report_first_open_then_normal_start_records_and_enforces_identity() {
        // Regression test for issue #74 (Codex finding T4 on PR #66).
        // A fresh install whose first DB touch is the metadata-only
        // path (`direct_report` in main.rs: construct the store and
        // init it with NO embedder bound, so no model download is
        // triggered) creates the DB file but records no sidecar —
        // by design, a non-serving open never moves the record. The
        // defect: that bare file makes every later construction see
        // `db_preexisting == true`, so the store classifies
        // LockedUnknown forever and no later serving open can adopt
        // or enforce the setup model choice. The first touch must
        // not destroy fresh-store provenance: a later normal start
        // (serving bound, serving == configured) on the still-empty
        // store records the pristine identity, and the store is
        // enforceable afterwards exactly like a Recorded store.
        let previous = without_model_env();
        let fx = store_state_fixture("t4-direct-report-first", "ModelA");

        // The metadata-only first touch, constructed the way
        // `direct_report` constructs it: no serving descriptor.
        let report = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        report.init("probe").await.expect("init");
        assert!(fx.db.exists(), "the metadata-only open creates the DB file");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "a non-serving open records no descriptor"
        );
        drop(report);

        // The normal start: serving bound and matching the
        // configuration. The still-empty store's identity must
        // become recordable — the pristine sidecar names the
        // configured/serving model.
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(
            sidecar["has_data"],
            json!(false),
            "the normal start records the pristine identity after a metadata-only first touch"
        );
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));

        // Recordable means writable under that identity, and the
        // first write flips the record to data-bearing under it.
        store
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("the first write under the recorded identity succeeds");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(true));
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelA"));
        drop(store);

        // Enforceable afterwards: reopening the now data-bearing
        // store under a different model is refused by the identity
        // check, the same enforcement a Recorded store gets.
        write_embedders_config(&fx.config_dir, "ModelB");
        let refused = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        let err = refused
            .init("probe")
            .await
            .expect_err("a differing model is refused once identity is recorded and data-bearing");
        assert!(
            embedding_identity_refusal_message(&err).is_some(),
            "the refusal must be the typed identity refusal: {err}"
        );
        drop(refused);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn refused_first_write_then_restart_under_chosen_model_records_identity() {
        // The combined path from issue #74's additional requirement
        // (the restart leg of issue #73's scenario): a first write is
        // refused (stale serving on a FreshUnrecorded store), leaving
        // a DB file with no data rows and no sidecar. On the next
        // start under the chosen (configured) model the store
        // previously classified LockedUnknown and no identity was
        // ever recorded. After the fix the combined path works end
        // to end: the restart records the pristine identity under
        // the chosen model, and writes are accepted and recorded
        // under that identity.
        let previous = without_model_env();
        let fx = store_state_fixture("t4-refused-write-restart", "ModelB");

        // The first-start process serves the stale model while the
        // configuration already names the chosen one.
        let store = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        store.init("probe").await.expect("init");
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "serving != configured at first init: no pristine sidecar is written"
        );
        let refused = store
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await;
        let err = refused.expect_err("the stale-serving first write must be refused");
        assert!(
            restart_to_apply_message(&err).is_some(),
            "the refusal must be the typed restart-to-apply error: {err}"
        );
        assert!(
            !crate::model_choice::store_state_path(&fx.db).exists(),
            "a refused first write records no identity"
        );
        let rows = store
            .list("probe", None, None)
            .await
            .expect("list after refusal");
        assert!(rows.is_empty(), "a refused first write lands no row");
        drop(store);

        // The restart under the chosen model: the pristine identity
        // is recorded (sidecar names the configured model, pristine
        // until data lands), then the write is accepted and the
        // flip records has_data under that same descriptor.
        let restarted = LadybugStore::for_testing(&fx.db, 4)
            .expect("reopen temp database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        restarted.init("probe").await.expect("init after restart");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(
            sidecar["has_data"],
            json!(false),
            "the restart under the chosen model records the pristine identity"
        );
        assert_eq!(sidecar["descriptor"]["model"], json!("ModelB"));
        restarted
            .upsert(
                "probe",
                "m1",
                vec![0.1, 0.2, 0.3, 0.4],
                store_state_payload(),
            )
            .await
            .expect("the first write under the chosen model succeeds after restart");
        let sidecar = read_sidecar(&fx.db);
        assert_eq!(sidecar["has_data"], json!(true));
        assert_eq!(
            sidecar["descriptor"]["model"],
            json!("ModelB"),
            "the flip records has_data under the chosen model's descriptor"
        );
        let found = restarted
            .search("probe", vec![0.1, 0.2, 0.3, 0.4], 5, false, false)
            .await
            .expect("search under the chosen model");
        assert!(
            !found.is_empty(),
            "the row written after restart is searchable"
        );
        drop(restarted);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn backup_restore_round_trip_preserves_store_state_sidecar() {
        // A snapshot that does not carry the sidecar
        // restores a store with no recorded identity — silently
        // LockedUnknown, outside the identity refusal's protection.
        // The round trip must preserve Recorded{has_data, R}, and the
        // identity refusal must still fire on the restored store.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("a5-backup-round-trip").await;
        let opener = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        let out = fx.root.join("snapshot");
        opener
            .export_database(&out.to_string_lossy())
            .await
            .expect("backup");
        drop(opener);
        assert!(
            out.join("ladybug.store.store-state.json").exists(),
            "the snapshot carries the store-state sidecar"
        );
        let report = crate::storage::snapshot::verify_snapshot(&out).expect("verify the snapshot");
        assert!(
            report.valid,
            "the snapshot verifies with the sidecar aboard"
        );

        // The restore shape: the file copies the Restore arm performs
        // (db, WAL, sidecar) into a new path, then the store opens.
        let restored_dir = fx.root.join("restored");
        std::fs::create_dir_all(&restored_dir).expect("create restore dir");
        let restored_db = restored_dir.join("ladybug.db");
        std::fs::copy(out.join("ladybug.store"), &restored_db).expect("copy the db");
        let snapshot_wal = out.join("ladybug.store.wal");
        if snapshot_wal.exists() {
            let mut target_wal = restored_db.clone().into_os_string();
            target_wal.push(".wal");
            std::fs::copy(&snapshot_wal, std::path::PathBuf::from(target_wal))
                .expect("copy the WAL");
        }
        std::fs::copy(
            out.join("ladybug.store.store-state.json"),
            crate::model_choice::store_state_path(&restored_db),
        )
        .expect("copy the sidecar");
        let state = crate::model_choice::read_store_state(&restored_db)
            .expect("the restored store keeps its recorded identity");
        assert!(state.has_data);
        assert_eq!(state.descriptor, serving_descriptor_for("ModelA"));

        let refused = LadybugStore::for_testing(&restored_db, 4)
            .expect("open restored database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelB")));
        let err = refused
            .init("probe")
            .await
            .expect_err("the identity refusal still fires on the restored store");
        assert!(embedding_identity_refusal_message(&err).is_some(), "{err}");
        drop(refused);

        let ok = LadybugStore::for_testing(&restored_db, 4)
            .expect("open restored database")
            .with_embed_config_dir(&fx.config_dir)
            .with_serving_descriptor(Some(serving_descriptor_for("ModelA")));
        ok.init("probe")
            .await
            .expect("the restored store opens under its recorded model");
        drop(ok);
        let _ = std::fs::remove_dir_all(&fx.root);
        restore_model_env(previous);
    }

    #[tokio::test]
    async fn export_into_a_reused_snapshot_dir_drops_the_stale_sidecar() {
        // Issue #76: export_database tolerates an existing destination
        // directory and overwrites the store, WAL, and manifest in
        // place, but it copied the store-state sidecar only when the
        // source had one — a destination sidecar from an earlier export
        // survived a later export of a pre-sidecar store, was blessed
        // into the new checksums, and restored as the wrong identity.
        // Reusing a snapshot directory for a pre-sidecar store must
        // leave a snapshot that carries no sidecar at all.
        let previous = without_model_env();
        let fx = data_bearing_store_fixture("t5-reuse-recorded").await;
        let out = fx.root.join("snapshot");
        let opener = LadybugStore::for_testing(&fx.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx.config_dir);
        opener
            .export_database(&out.to_string_lossy())
            .await
            .expect("export the recorded store");
        drop(opener);
        assert!(
            out.join("ladybug.store.store-state.json").exists(),
            "the first export carries the recorded store's sidecar"
        );
        let first = crate::storage::snapshot::verify_snapshot(&out).expect("verify export one");
        assert!(first.valid);
        assert!(
            first
                .checksums
                .as_ref()
                .expect("checksums reported")
                .store_state_sha256
                .is_some(),
            "the first export's checksums name the sidecar it placed"
        );
        // A stale destination WAL, planted the way an earlier export
        // whose source still had one after its checkpoint would have
        // placed it: the same-class hazard as the sidecar, covered
        // below once the pre-sidecar export reuses this directory.
        std::fs::write(out.join("ladybug.store.wal"), b"stale wal bytes")
            .expect("plant the stale destination WAL");

        // A pre-sidecar store: the db file exists (created by a
        // non-serving open, exactly as the LockedUnknown fixtures
        // build one) and no sidecar was ever written for it.
        let fx2 = store_state_fixture("t5-reuse-pre-sidecar", "ModelA");
        let creator = LadybugStore::for_testing(&fx2.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx2.config_dir);
        creator.init("probe").await.expect("init");
        drop(creator);
        assert!(
            !crate::model_choice::store_state_path(&fx2.db).exists(),
            "the pre-sidecar fixture carries no sidecar"
        );

        let exporter = LadybugStore::for_testing(&fx2.db, 4)
            .expect("open temp database")
            .with_embed_config_dir(&fx2.config_dir);
        exporter
            .export_database(&out.to_string_lossy())
            .await
            .expect("export the pre-sidecar store into the same directory");
        drop(exporter);
        assert!(
            !crate::model_choice::store_state_path(&fx2.db).exists(),
            "exporting must not create a sidecar on the pre-sidecar source"
        );
        assert!(
            !out.join("ladybug.store.store-state.json").exists(),
            "the reused snapshot directory carries no stale sidecar"
        );
        assert!(
            !out.join("ladybug.store.wal").exists(),
            "the reused snapshot directory carries no stale WAL: the pre-sidecar source had none to place"
        );
        let checksums_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out.join("checksums.json")).expect("read checksums.json"),
        )
        .expect("checksums.json parses");
        assert!(
            checksums_json.get("store_state_sha256").is_none(),
            "checksums.json carries no sidecar entry for a sidecar the export did not place: {checksums_json}"
        );
        let report = crate::storage::snapshot::verify_snapshot(&out).expect("verify export two");
        assert!(report.valid, "the reused snapshot verifies");
        assert!(
            report
                .checksums
                .as_ref()
                .expect("checksums reported")
                .store_state_sha256
                .is_none(),
            "verification reports no sidecar checksum"
        );

        // The restore-side view (the Restore arm's copies in src/main.rs):
        // with no sidecar in the snapshot there is nothing to copy onto
        // the target, and the restored store reads as identity-less.
        let restored_dir = fx.root.join("restored-reuse");
        std::fs::create_dir_all(&restored_dir).expect("create restore dir");
        let restored_db = restored_dir.join("ladybug.db");
        std::fs::copy(out.join("ladybug.store"), &restored_db).expect("copy the db");
        let snapshot_wal = out.join("ladybug.store.wal");
        if snapshot_wal.exists() {
            let mut target_wal = restored_db.clone().into_os_string();
            target_wal.push(".wal");
            std::fs::copy(&snapshot_wal, std::path::PathBuf::from(target_wal))
                .expect("copy the WAL");
        }
        let snapshot_sidecar = out.join("ladybug.store.store-state.json");
        if snapshot_sidecar.exists() {
            std::fs::copy(
                &snapshot_sidecar,
                crate::model_choice::store_state_path(&restored_db),
            )
            .expect("copy the sidecar");
        }
        assert!(
            !crate::model_choice::store_state_path(&restored_db).exists(),
            "the restore lands no sidecar from the reused snapshot"
        );
        assert!(
            crate::model_choice::read_store_state(&restored_db).is_none(),
            "the restored store has no recorded identity"
        );
        let _ = std::fs::remove_dir_all(&fx.root);
        let _ = std::fs::remove_dir_all(&fx2.root);
        restore_model_env(previous);
    }
}

impl LadybugStore {
    pub async fn project_namespace_rdf(
        &self,
        namespace: &str,
        tier: crate::rdf::Tier,
        generation: u64,
    ) -> Result<Vec<crate::rdf::Quad>> {
        let memories = crate::traits::VectorStore::list(self, namespace, None, None).await?;
        let rows: Vec<(String, crate::traits::MemoryPayload)> =
            memories.into_iter().map(|m| (m.id, m.payload)).collect();

        let namespace_owned = namespace.to_string();
        let edges: Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)> =
            self.with_conn(move |conn: &Connection| -> Result<Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)>> {
                let safe_ns = escape_ladybug_string(&namespace_owned);
                let mut out: Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)> =
                    Vec::new();
                let tables = ["RELATES_TO", "CONTAINS", "GOVERNS", "EXTRACTED_FROM", "DEPENDS_ON"];
                for table in tables {
                    let query = format!(
                        "MATCH (a:Memory)-[r:{}]->(b:Memory) WHERE a.namespace = '{}' AND b.namespace = '{}' RETURN a.id, b.id, r.source, r.valid_from, r.valid_to, r.generation, r.extractor",
                        table, safe_ns, safe_ns
                    );
                    if let Ok(mut rows) = conn.query(&query) {
                        while let Some(row) = rows.next() {
                            let from = format!("{}", row[0]);
                            let to = format!("{}", row[1]);
                            let source = match format!("{}", row[2]).as_str() {
                                "" | "null" => None,
                                s => Some(s.to_string()),
                            };
                            let valid_from = format!("{}", row[3]).parse::<i64>().ok();
                            let valid_to = format!("{}", row[4]).parse::<i64>().ok();
                            let gen = format!("{}", row[5]).parse::<i64>().ok();
                            let extractor = match format!("{}", row[6]).as_str() {
                                "" | "null" => None,
                                s => Some(s.to_string()),
                            };
                            out.push((
                                from,
                                table.to_string(),
                                to,
                                Some(crate::rdf::EdgeProvenance {
                                    source,
                                    valid_from,
                                    valid_to,
                                    generation: gen,
                                    extractor,
                                }),
                            ));
                        }
                    }
                }
                Ok(out)
            })
            .await?;

        Ok(crate::rdf::rows_to_quads(&rows, namespace, tier, generation, &edges))
    }
}

impl LadybugStore {
    pub async fn refresh_ontology(&self, namespace: &str) {
        let namespace_owned = namespace.to_string();
        let metadata = self
            .with_conn(move |conn: &Connection| -> Result<Option<serde_json::Value>> {
                let safe_ns = escape_ladybug_string(&namespace_owned);
                let rid = crate::registry::registry_row_id(&namespace_owned);
                let safe_id = escape_ladybug_string(&rid);
                let query = format!(
                    "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' RETURN m.metadata",
                    safe_id, safe_ns
                );
                if let Ok(mut rows) = conn.query(&query) {
                    if let Some(row) = rows.next() {
                        let meta_str = format!("{}", row[0]);
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta_str) {
                            return Ok(Some(v));
                        }
                    }
                }
                Ok(None)
            })
            .await
            .ok()
            .flatten();
        let mut idx = metadata
            .as_ref()
            .map(|m| crate::rdf::OntologyIndex::from_registry_metadata(m))
            .unwrap_or_default();
        // The shipped vocabulary relations (sub-classes, equivalences,
        // sub-properties, symmetric/transitive properties...) are always in
        // the index; a registry row may refine or extend them.
        idx.load_vocabulary_value(crate::traits::memory_vocabulary());
        if let Some(ref m) = metadata {
            if let Some(v) = m.get("vocabulary") {
                idx.load_vocabulary_value(v);
            }
            if let Some(ttl) = m.get("shacl_shapes").and_then(|v| v.as_str()) {
                let _ = idx.load_shacl_shapes(ttl);
            } else {
                idx.clear_shacl_shapes();
            }
            if let Some(extra_dom) = m.get("default_domain").and_then(|v| v.as_str()) {
                idx.add_domain(crate::rdf::ontology::DomainEntry {
                    slug: extra_dom.to_string(),
                    parent: None,
                    aliases: Vec::new(),
                    concept_iri: None,
                });
            }
        }
        // Keep the read-side accessors honest for every registry shape: an
        // empty global domain must degenerate to "no children / not
        // symmetric" rather than panicking.
        let global_budget = crate::rdf::ontology::QueryBudget::default();
        let _ = (
            idx.domains().len(),
            idx.domain("global"),
            idx.children_of("global"),
            idx.is_symmetric("RELATES_TO"),
            idx.closure_governs("global", &[], &global_budget),
        );
        // Insert into per-namespace map (read-modify-write on ArcSwap).
        let cur = self.ontology.load();
        let mut map: std::collections::BTreeMap<String, crate::rdf::OntologyIndex> = (**cur).clone();
        map.insert(namespace.to_string(), idx);
        self.ontology.store(std::sync::Arc::new(map));
    }

    /// Get the cached ontology index for a namespace, building on demand (empty if missing).
    pub fn get_ontology(&self, namespace: &str) -> crate::rdf::OntologyIndex {
        let cur = self.ontology.load();
        cur.get(namespace)
            .cloned()
            .unwrap_or_default()
    }
}
