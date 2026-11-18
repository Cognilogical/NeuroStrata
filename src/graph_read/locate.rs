use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::formal_kernel::allocate_hybrid_slots;
use crate::graph_read::hybrid::{compute_symbol_match, SymbolMatchGrade};
use crate::rebac::{can_read_memory, resolve_principal};
use crate::traits::{Embedder, GraphRead, SearchMode, SearchResult, VectorStore};

fn rrf_gain(rank: usize) -> f32 {
    1.0 / (60.0 + (rank + 1) as f32)
}

fn query_identifiers(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 2)
        .map(String::from)
        .collect()
}

fn symbol_identifier_matches(symbol: &str, idents: &[String]) -> bool {
    matches!(
        compute_symbol_match(symbol, idents),
        SymbolMatchGrade::Exact | SymbolMatchGrade::Prefix | SymbolMatchGrade::TokenSubstring
    )
}

pub const DEFAULT_MAX_LOCATIONS: usize = 8;
pub const MAX_LOCATIONS_CAP: usize = 20;
pub const DEFAULT_MAX_BYTES: usize = 6144;
pub const MIN_BUDGET_BYTES: usize = 1024;
pub const RULE_SUMMARY_MAX_CHARS: usize = 280;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateRequest {
    pub namespace: String,
    pub query: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub symbols_only: Option<bool>,
    #[serde(default)]
    pub max_locations: Option<usize>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub principal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocatedSpan {
    pub path: String,
    pub lines: Option<String>,
    pub symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub scip_symbol: Option<String>,
    pub signature: Option<String>,
    pub score: f32,
    pub why: String,
    pub callers_count: usize,
    pub callees_count: usize,
    pub blast_radius_count: usize,
    pub governing_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocateRuleSummary {
    pub id: String,
    pub memory_type: String,
    pub summary: String,
    pub governs_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocateFailedAttempt {
    pub id: String,
    pub approach: String,
    pub failure_mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocateResponse {
    pub namespace: String,
    pub query: String,
    pub locations: Vec<LocatedSpan>,
    pub governing_rules: Vec<LocateRuleSummary>,
    pub failed_attempts: Vec<LocateFailedAttempt>,
    pub conceptual_bridges: Vec<String>,
    #[serde(default = "default_true")]
    pub ast_ingested: bool,
    pub truncated_by_budget: bool,
}

fn default_true() -> bool {
    true
}

fn is_structural_type(memory_type: &str) -> bool {
    matches!(
        memory_type,
        "code_ast" | "symbol" | "file" | "doc_section" | "directory" | "markdown"
    )
}

fn is_normative_type(memory_type: &str) -> bool {
    matches!(memory_type, "rule" | "fact" | "procedure" | "context")
}

fn is_superseded_or_archived(meta: &Value) -> bool {
    if meta
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if meta
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(vt) = meta.get("valid_to") {
        if !vt.is_null() {
            if let Some(s) = vt.as_str() {
                if !s.is_empty() {
                    return true;
                }
            } else if vt.as_i64().is_some() {
                return true;
            }
        }
    }
    false
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{prefix}...")
}

fn extract_span_path(row: &SearchResult) -> Option<String> {
    let loc = row.payload.location.trim();
    if !loc.is_empty() && loc != "global" && loc != "unknown" {
        let base = loc.split('#').next().unwrap_or(loc);
        let norm = crate::parser::ingest::normalize_node_path(base);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    if let Some(refs) = row.payload.metadata.get("refs").and_then(|v| v.as_array()) {
        for r in refs {
            if let Some(f) = r.get("file").and_then(|v| v.as_str()) {
                let norm = crate::parser::ingest::normalize_node_path(f);
                if !norm.is_empty() {
                    return Some(norm);
                }
            }
        }
    }
    if let Some(path) = row
        .payload
        .metadata
        .get("relative_path")
        .or_else(|| row.payload.metadata.get("path"))
        .and_then(|v| v.as_str())
    {
        let norm = crate::parser::ingest::normalize_node_path(path);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    None
}

fn extract_span_lines(row: &SearchResult) -> Option<String> {
    let lines = row.payload.location_lines.trim();
    if !lines.is_empty() {
        return Some(lines.to_string());
    }
    if let Some(refs) = row.payload.metadata.get("refs").and_then(|v| v.as_array()) {
        for r in refs {
            if let Some(l) = r.get("lines").and_then(|v| v.as_str()) {
                let trimmed = l.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

fn extract_symbol_name(row: &SearchResult) -> Option<String> {
    if let Some(s) = row
        .payload
        .metadata
        .get("symbol")
        .or_else(|| row.payload.metadata.get("symbol_name"))
        .and_then(|v| v.as_str())
    {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let loc = row.payload.location.trim();
    if let Some((_, frag)) = loc.split_once('#') {
        let after_colon = frag.split_once(':').map(|(_, n)| n).unwrap_or(frag);
        let before_at = after_colon
            .split_once('@')
            .map(|(n, _)| n)
            .unwrap_or(after_colon)
            .trim();
        if !before_at.is_empty() {
            return Some(before_at.to_string());
        }
    }
    None
}

fn extract_symbol_kind(row: &SearchResult) -> Option<String> {
    row.payload
        .metadata
        .get("symbol_kind")
        .or_else(|| row.payload.metadata.get("ast_kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn extract_scip_symbol(row: &SearchResult) -> Option<String> {
    row.payload
        .metadata
        .get("scip_symbol")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn extract_signature_preview(row: &SearchResult) -> Option<String> {
    if let Some(sig) = row
        .payload
        .metadata
        .get("signature")
        .and_then(|v| v.as_str())
    {
        let trimmed = sig.trim();
        if !trimmed.is_empty() {
            return Some(truncate_chars(trimmed, 140));
        }
    }
    let content = row.payload.content.trim();
    if content.is_empty() {
        return None;
    }
    // Standard ingested symbol body format:
    // "<kind> <name> in <path>\nlines <start>-<end>\n\n<signature_line>\n..."
    if let Some((_, body)) = content.split_once("\n\n") {
        for line in body.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('#') {
                return Some(truncate_chars(trimmed, 140));
            }
        }
        if let Some(first) = body.lines().find(|l| !l.trim().is_empty()) {
            return Some(truncate_chars(first.trim(), 140));
        }
    }
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(truncate_chars(trimmed, 140));
        }
    }
    None
}

fn extract_governed_targets(row: &SearchResult) -> Vec<(String, Option<String>, Option<String>)> {
    let mut out: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    let loc = row.payload.location.trim();
    if !loc.is_empty() && loc != "global" && loc != "unknown" {
        let base = crate::parser::ingest::normalize_node_path(loc.split('#').next().unwrap_or(loc));
        if !base.is_empty() {
            let lines = (!row.payload.location_lines.trim().is_empty())
                .then(|| row.payload.location_lines.trim().to_string());
            let sym = extract_symbol_name(row);
            out.push((base, sym, lines));
        }
    }
    if let Some(locs) = row
        .payload
        .metadata
        .get("locations")
        .and_then(|v| v.as_array())
    {
        for entry in locs {
            if let Some(p) = entry.get("path").and_then(|v| v.as_str()) {
                let norm = crate::parser::ingest::normalize_node_path(p);
                if !norm.is_empty() {
                    let sym = entry
                        .get("symbol")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let lines = entry
                        .get("lines")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    out.push((norm, sym, lines));
                }
            }
        }
    }
    if let Some(gov) = row.payload.metadata.get("governs") {
        match gov {
            Value::String(s) => {
                let norm = crate::parser::ingest::normalize_node_path(s);
                if !norm.is_empty() {
                    out.push((norm, None, None));
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let norm = crate::parser::ingest::normalize_node_path(s);
                        if !norm.is_empty() {
                            out.push((norm, None, None));
                        }
                    } else if let Some(p) = item.get("path").and_then(|v| v.as_str()) {
                        let norm = crate::parser::ingest::normalize_node_path(p);
                        if !norm.is_empty() {
                            let sym = item
                                .get("symbol")
                                .and_then(|v| v.as_str())
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty());
                            let lines = item
                                .get("lines")
                                .and_then(|v| v.as_str())
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty());
                            out.push((norm, sym, lines));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn path_matches_governance_target(file_path: &str, target_path: &str) -> bool {
    let f = crate::parser::ingest::normalize_node_path(file_path);
    let t = crate::parser::ingest::normalize_node_path(target_path);
    if f.is_empty() || t.is_empty() {
        return false;
    }
    if f == t {
        return true;
    }
    let prefix = format!("{}/", t.trim_end_matches('/'));
    f.starts_with(&prefix)
}

fn extract_failed_attempt(row: &SearchResult) -> Option<LocateFailedAttempt> {
    let attempt = row.payload.metadata.get("attempt")?.as_object()?;
    let outcome = attempt
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !outcome.eq_ignore_ascii_case("failed") {
        return None;
    }
    let approach = attempt
        .get("approach")
        .and_then(|v| v.as_str())
        .map(|s| truncate_chars(s, 200))
        .unwrap_or_else(|| truncate_chars(&row.payload.content, 200));
    let failure_mode = attempt
        .get("failure_mode")
        .and_then(|v| v.as_str())
        .map(|s| truncate_chars(s, 200))
        .unwrap_or_else(|| "unspecified failure mode".to_string());
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for (p, _, _) in extract_governed_targets(row) {
        paths.insert(p);
    }
    Some(LocateFailedAttempt {
        id: row.id.clone(),
        approach,
        failure_mode,
        paths: paths.into_iter().collect(),
    })
}

fn extract_dependency_ids(meta: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for key in [
        "depends_on",
        "calls_resolved",
        "cross_file_callees",
        "scip_callees",
    ] {
        if let Some(arr) = meta.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        out.insert(s.to_string());
                    }
                } else if let Some(id) = item
                    .get("target_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                {
                    if !id.is_empty() {
                        out.insert(id.to_string());
                    }
                }
            }
        }
    }
    out
}

fn extract_caller_ids(meta: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for key in ["cross_file_callers", "scip_callers"] {
        if let Some(arr) = meta.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        out.insert(s.to_string());
                    }
                } else if let Some(id) = item
                    .get("caller_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                {
                    if !id.is_empty() {
                        out.insert(id.to_string());
                    }
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct CandidateSpan {
    row_id: Option<String>,
    path: String,
    lines: Option<String>,
    symbol: Option<String>,
    symbol_kind: Option<String>,
    scip_symbol: Option<String>,
    signature: Option<String>,
    score: f32,
    reasons: Vec<String>,
    governing_rule_ids: BTreeSet<String>,
}

impl CandidateSpan {
    fn add_reason(&mut self, reason: impl Into<String>) {
        let r = reason.into();
        if !r.is_empty() && !self.reasons.contains(&r) {
            self.reasons.push(r);
        }
    }
}

fn enforce_budget(resp: &mut LocateResponse, max_bytes: usize) {
    let effective_budget = max_bytes;
    if effective_budget < MIN_BUDGET_BYTES {
        return;
    }
    let fits = |r: &LocateResponse| -> bool {
        serde_json::to_vec(r)
            .map(|b| b.len() <= effective_budget)
            .unwrap_or(false)
    };
    if fits(resp) {
        return;
    }
    resp.truncated_by_budget = true;

    while !resp.conceptual_bridges.is_empty() && !fits(resp) {
        resp.conceptual_bridges.pop();
    }
    if fits(resp) {
        return;
    }

    while resp.failed_attempts.len() > 1 && !fits(resp) {
        resp.failed_attempts.pop();
    }
    if fits(resp) {
        return;
    }

    while resp.locations.len() > 1 && !fits(resp) {
        resp.locations.pop();
    }
    if fits(resp) {
        return;
    }

    // Prune governing_rules that are no longer referenced by remaining locations,
    // or trim trailing rules until within budget.
    let referenced: HashSet<String> = resp
        .locations
        .iter()
        .flat_map(|l| l.governing_rule_ids.iter().cloned())
        .collect();
    resp.governing_rules.retain(|r| referenced.contains(&r.id));
    while !resp.governing_rules.is_empty() && !fits(resp) {
        if let Some(removed) = resp.governing_rules.pop() {
            for loc in &mut resp.locations {
                loc.governing_rule_ids.retain(|id| id != &removed.id);
            }
        }
    }
    if fits(resp) {
        return;
    }

    if !resp.failed_attempts.is_empty() && !fits(resp) {
        resp.failed_attempts.clear();
    }
    if !fits(resp) {
        for loc in &mut resp.locations {
            loc.signature = None;
            loc.why = truncate_chars(&loc.why, 80);
        }
    }
}

fn record_paths_in_project_ledger(
    project_root: Option<&str>,
    namespace: &str,
    paths: &[String],
    fallback_query: &str,
) {
    let Some(root) = project_root.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let ledger_path = std::path::Path::new(root).join(".NeuroStrata/session_queries.json");
    if let Some(parent) = ledger_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut doc: Value = std::fs::read_to_string(&ledger_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "namespace": namespace, "queries": [] }));
    let now = chrono::Utc::now().timestamp();
    if let Some(arr) = doc.get_mut("queries").and_then(|v| v.as_array_mut()) {
        if paths.is_empty() {
            arr.push(json!({
                "ts": now,
                "tool": "neurostrata_locate",
                "query": fallback_query,
                "governed_prefixes": [format!("{}/", fallback_query.trim_end_matches('/'))],
            }));
        } else {
            for path in paths {
                arr.push(json!({
                    "ts": now,
                    "tool": "neurostrata_locate",
                    "query": path,
                    "governed_prefixes": [path],
                }));
            }
        }
        if let Ok(serialized) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::write(&ledger_path, serialized);
        }
    }
}

pub async fn execute_locate(
    store: &Arc<dyn VectorStore>,
    embedder: Option<&Arc<dyn Embedder>>,
    request: &LocateRequest,
    trusted_principal: Option<&str>,
) -> anyhow::Result<LocateResponse> {
    let namespace = crate::server::resolve_namespace(store, &request.namespace).await;
    let query = request.query.trim().to_string();
    let max_locations = request
        .max_locations
        .unwrap_or(DEFAULT_MAX_LOCATIONS)
        .clamp(1, MAX_LOCATIONS_CAP);
    let max_bytes = request.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let symbols_only = request.symbols_only.unwrap_or(false);
    // Every spelling of the prefix; empty means no filter.
    let path_prefix: Vec<String> = match request.path_prefix.as_deref() {
        Some(prefix) => {
            crate::server::resolve_path_prefix(
                store,
                &namespace,
                prefix,
                request.project_root.as_deref(),
            )
            .await
        }
        None => Vec::new(),
    };

    let principal = resolve_principal(
        trusted_principal.unwrap_or("admin"),
        request.principal.as_deref(),
    );

    let all_rows = store.list(&namespace, None, None).await.unwrap_or_default();

    let visible_rows: Vec<SearchResult> = all_rows
        .into_iter()
        .filter(|r| !crate::registry::is_registry_row(&r.payload.metadata))
        .filter(|r| r.payload.memory_type != "action_receipt" && r.payload.memory_type != "task")
        .filter(|r| !is_superseded_or_archived(&r.payload.metadata))
        .filter(|r| can_read_memory(&principal, &r.payload.user_id, &r.payload.metadata))
        .collect();

    let ast_ingested = visible_rows.iter().any(|r| {
        matches!(
            r.payload.memory_type.as_str(),
            "code_ast" | "symbol" | "file"
        )
    });

    let rows_by_id: HashMap<String, SearchResult> = visible_rows
        .iter()
        .map(|r| (r.id.clone(), r.clone()))
        .collect();

    // Build in-memory AST dependency graph over visible structural rows so we can
    // compute callers_count, callees_count, blast_radius_count, and 1-hop caller
    // expansion in a single deterministic pass without N+1 database queries.
    let mut symbol_callees: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut symbol_callers: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut symbols_by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut symbols_in_file: HashMap<String, Vec<String>> = HashMap::new();

    for row in &visible_rows {
        if is_structural_type(&row.payload.memory_type) {
            if let Some(p) = extract_span_path(row) {
                symbols_in_file.entry(p).or_default().push(row.id.clone());
            }
            if let Some(sym) = extract_symbol_name(row) {
                symbols_by_name
                    .entry(sym.to_ascii_lowercase())
                    .or_default()
                    .push(row.id.clone());
            }
        }
    }

    for row in &visible_rows {
        if !is_structural_type(&row.payload.memory_type) {
            continue;
        }
        let mut callees = extract_dependency_ids(&row.payload.metadata);
        // Also resolve raw `calls` identifier names against workspace symbols.
        if let Some(raw_calls) = row.payload.metadata.get("calls").and_then(|v| v.as_array()) {
            for c in raw_calls.iter().filter_map(|v| v.as_str()) {
                let terminal = c
                    .rsplit(|ch: char| ch == ':' || ch == '.')
                    .next()
                    .unwrap_or(c)
                    .to_ascii_lowercase();
                if let Some(target_ids) = symbols_by_name.get(&terminal) {
                    for tid in target_ids {
                        if tid != &row.id {
                            callees.insert(tid.clone());
                        }
                    }
                }
            }
        }
        callees.retain(|tid| tid != &row.id && rows_by_id.contains_key(tid));
        for tid in &callees {
            symbol_callers
                .entry(tid.clone())
                .or_default()
                .insert(row.id.clone());
        }
        if !callees.is_empty() {
            symbol_callees
                .entry(row.id.clone())
                .or_default()
                .extend(callees);
        }
        for caller_id in extract_caller_ids(&row.payload.metadata) {
            if caller_id != row.id && rows_by_id.contains_key(&caller_id) {
                symbol_callers
                    .entry(row.id.clone())
                    .or_default()
                    .insert(caller_id.clone());
                symbol_callees
                    .entry(caller_id)
                    .or_default()
                    .insert(row.id.clone());
            }
        }
    }

    // Step 1: Hybrid RRF Seed Retrieval (reusing TantivyLexicalIndex + HybridSlots).
    let fetch_limit = (max_locations * 4).max(16);
    let slot_plan = allocate_hybrid_slots(
        fetch_limit as u32,
        fetch_limit as u32,
        fetch_limit as u32,
        fetch_limit as u32,
    );
    let mut rrf_scores: HashMap<String, f32> = HashMap::new();
    let mut match_reasons: HashMap<String, Vec<String>> = HashMap::new();

    if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
        .downcast_ref::<crate::store::LadybugStore>()
    {
        if let Ok(lex_outcome) = ls
            .search_tantivy_lexical_with_options(
                &namespace,
                &query,
                fetch_limit,
                false,
                false,
                &[],
                &principal,
                true,
            )
            .await
        {
            for (rank, hit) in lex_outcome.results.iter().enumerate() {
                if rows_by_id.contains_key(&hit.id) {
                    *rrf_scores.entry(hit.id.clone()).or_insert(0.0) += rrf_gain(rank);
                    match_reasons
                        .entry(hit.id.clone())
                        .or_default()
                        .push("BM25 lexical".to_string());
                }
            }
        }
        if let Some(emb) = embedder {
            if let Ok(vec) = emb.embed(&query).await {
                let hyb_limit = (slot_plan.normative_taken
                    + slot_plan.expansion_taken
                    + slot_plan.remaining_taken) as usize;
                if let Ok(hyb_outcome) = ls
                    .search_hybrid_with_domain(
                        &namespace,
                        vec,
                        hyb_limit.max(fetch_limit),
                        false,
                        false,
                        true,
                        &[],
                        Some(&query),
                        false,
                        SearchMode::Hybrid,
                    )
                    .await
                {
                    for (rank, hit) in hyb_outcome.results.iter().enumerate() {
                        if rows_by_id.contains_key(&hit.id) {
                            *rrf_scores.entry(hit.id.clone()).or_insert(0.0) += rrf_gain(rank);
                            let reasons = match_reasons.entry(hit.id.clone()).or_default();
                            if !reasons.iter().any(|r| r == "semantic vector") {
                                reasons.push("semantic vector".to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Deterministic identifier & token scoring across visible rows (handles exact
    // symbol matches, SCIP symbols, file paths, and non-Ladybug test stores).
    let q_lower = query.to_ascii_lowercase();
    let q_idents = query_identifiers(&query);
    let q_tokens: Vec<String> = q_lower
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '/' && c != '.' && c != '-')
        .filter(|s| s.len() >= 2)
        .map(String::from)
        .collect();

    let mut exact_symbol_seeds: Vec<(String, String)> = Vec::new();
    for row in &visible_rows {
        let mut bonus = 0.0f32;
        if let Some(sym) = extract_symbol_name(row) {
            if sym.eq_ignore_ascii_case(&query)
                || q_idents.iter().any(|qid| sym.eq_ignore_ascii_case(qid))
            {
                bonus += 0.50;
                match_reasons
                    .entry(row.id.clone())
                    .or_default()
                    .insert(0, format!("exact symbol '{sym}'"));
                exact_symbol_seeds.push((row.id.clone(), sym));
            } else if symbol_identifier_matches(&sym, &q_idents) {
                bonus += 0.22;
                let reasons = match_reasons.entry(row.id.clone()).or_default();
                if !reasons.iter().any(|r| r == "BM25 lexical") {
                    reasons.push("BM25 lexical".to_string());
                }
            }
        }
        if let Some(scip_sym) = extract_scip_symbol(row) {
            if scip_sym.to_ascii_lowercase().contains(&q_lower) {
                bonus += 0.25;
                match_reasons
                    .entry(row.id.clone())
                    .or_default()
                    .push("SCIP symbol match".to_string());
            }
        }
        if let Some(p) = extract_span_path(row) {
            let p_lower = p.to_ascii_lowercase();
            if p_lower == q_lower || p_lower.ends_with(&format!("/{q_lower}")) {
                bonus += 0.35;
                match_reasons
                    .entry(row.id.clone())
                    .or_default()
                    .push(format!("path match '{p}'"));
            }
        }
        let content_lower = row.payload.content.to_ascii_lowercase();
        let matched_tokens = q_tokens
            .iter()
            .filter(|tok| content_lower.contains(tok.as_str()))
            .count();
        if matched_tokens > 0 {
            bonus += 0.06 * (matched_tokens as f32 / q_tokens.len().max(1) as f32);
            let reasons = match_reasons.entry(row.id.clone()).or_default();
            if reasons.is_empty() {
                reasons.push("BM25 lexical".to_string());
            }
        }
        if bonus > 0.0 {
            *rrf_scores.entry(row.id.clone()).or_insert(0.0) += bonus;
        }
    }

    // 1-hop caller expansion from exact symbol seeds.
    for (seed_id, seed_sym) in &exact_symbol_seeds {
        if let Some(callers) = symbol_callers.get(seed_id) {
            for caller_id in callers {
                *rrf_scores.entry(caller_id.clone()).or_insert(0.0) += 0.08;
                match_reasons
                    .entry(caller_id.clone())
                    .or_default()
                    .push(format!("1-hop caller of {seed_sym}"));
            }
        }
    }

    // Partition scored rows into Structural Spans and Normative Anchors.
    // Key for candidate coalescing: (path, symbol)
    let mut span_map: BTreeMap<(String, Option<String>), CandidateSpan> = BTreeMap::new();
    let mut normative_hits: Vec<(SearchResult, f32)> = Vec::new();

    for (row_id, score) in &rrf_scores {
        let Some(row) = rows_by_id.get(row_id) else {
            continue;
        };
        if is_structural_type(&row.payload.memory_type) {
            let Some(path) = extract_span_path(row) else {
                continue;
            };
            let symbol = extract_symbol_name(row);
            let key = (path.clone(), symbol.clone());
            let entry = span_map.entry(key).or_insert_with(|| CandidateSpan {
                row_id: Some(row.id.clone()),
                path: path.clone(),
                lines: extract_span_lines(row),
                symbol: symbol.clone(),
                symbol_kind: extract_symbol_kind(row),
                scip_symbol: extract_scip_symbol(row),
                signature: extract_signature_preview(row),
                score: 0.0,
                reasons: Vec::new(),
                governing_rule_ids: BTreeSet::new(),
            });
            entry.score += *score;
            if entry.lines.is_none() {
                entry.lines = extract_span_lines(row);
            }
            if entry.scip_symbol.is_none() {
                entry.scip_symbol = extract_scip_symbol(row);
            }
            if entry.signature.is_none() {
                entry.signature = extract_signature_preview(row);
            }
            if let Some(reasons) = match_reasons.get(row_id) {
                for r in reasons {
                    entry.add_reason(r.clone());
                }
            }
        } else if is_normative_type(&row.payload.memory_type) {
            normative_hits.push((row.clone(), *score));
        }
    }

    // Step 2: Anchor Projection (+0.25 * rule_gain onto governed structural spans,
    // or synthesize spans when no structural AST rows exist for that file).
    let mut rule_table: BTreeMap<String, LocateRuleSummary> = BTreeMap::new();
    for (rule_row, rule_score) in &normative_hits {
        if extract_failed_attempt(rule_row).is_some() {
            continue;
        }
        let targets = extract_governed_targets(rule_row);
        if targets.is_empty() {
            continue;
        }
        let boost = (0.25 * rule_score).max(0.02);
        let mut governed_paths_set: BTreeSet<String> = BTreeSet::new();
        for (target_path, target_sym, target_lines) in targets {
            governed_paths_set.insert(target_path.clone());
            let mut matched_any = false;
            for ((span_path, span_sym), span) in span_map.iter_mut() {
                if path_matches_governance_target(span_path, &target_path)
                    && (target_sym.is_none() || span_sym == &target_sym)
                {
                    span.score += boost;
                    span.governing_rule_ids.insert(rule_row.id.clone());
                    span.add_reason(format!("governed by matched rule {}", rule_row.id));
                    matched_any = true;
                }
            }
            if !matched_any {
                // Check if any structural row in the store lives under target_path.
                let mut found_structural = false;
                for row in &visible_rows {
                    if !is_structural_type(&row.payload.memory_type) {
                        continue;
                    }
                    let Some(p) = extract_span_path(row) else {
                        continue;
                    };
                    if path_matches_governance_target(&p, &target_path) {
                        let sym = extract_symbol_name(row);
                        if target_sym.is_some() && sym != target_sym {
                            continue;
                        }
                        found_structural = true;
                        let key = (p.clone(), sym.clone());
                        let entry = span_map.entry(key).or_insert_with(|| CandidateSpan {
                            row_id: Some(row.id.clone()),
                            path: p,
                            lines: extract_span_lines(row),
                            symbol: sym,
                            symbol_kind: extract_symbol_kind(row),
                            scip_symbol: extract_scip_symbol(row),
                            signature: extract_signature_preview(row),
                            score: 0.0,
                            reasons: Vec::new(),
                            governing_rule_ids: BTreeSet::new(),
                        });
                        entry.score += boost;
                        entry.governing_rule_ids.insert(rule_row.id.clone());
                        entry.add_reason(format!("governed by matched rule {}", rule_row.id));
                    }
                }
                if !found_structural && (!symbols_only || target_sym.is_some()) {
                    let key = (target_path.clone(), target_sym.clone());
                    let entry = span_map.entry(key).or_insert_with(|| CandidateSpan {
                        row_id: None,
                        path: target_path.clone(),
                        lines: target_lines.clone(),
                        symbol: target_sym.clone(),
                        symbol_kind: None,
                        scip_symbol: None,
                        signature: None,
                        score: 0.0,
                        reasons: Vec::new(),
                        governing_rule_ids: BTreeSet::new(),
                    });
                    entry.score += boost;
                    entry.governing_rule_ids.insert(rule_row.id.clone());
                    entry.add_reason(format!("governed by matched rule {}", rule_row.id));
                }
            }
        }
        rule_table.insert(
            rule_row.id.clone(),
            LocateRuleSummary {
                id: rule_row.id.clone(),
                memory_type: rule_row.payload.memory_type.clone(),
                summary: truncate_chars(&rule_row.payload.content, RULE_SUMMARY_MAX_CHARS),
                governs_paths: governed_paths_set.into_iter().collect(),
            },
        );
    }

    // Step 3: Span Deduplication & Line-Range Coalescing.
    // If both a file-level span (symbol == None) and symbol-level span(s) (symbol == Some(_))
    // exist for the same path, fold the file-level span's score and governing rules into
    // the symbol-level spans for that path and drop the bare file-level span.
    let paths_with_symbols: HashSet<String> = span_map
        .keys()
        .filter_map(|(path, sym)| sym.as_ref().map(|_| path.clone()))
        .collect();
    let mut file_level_spans: HashMap<String, CandidateSpan> = HashMap::new();
    for path in &paths_with_symbols {
        if let Some(file_span) = span_map.remove(&(path.clone(), None)) {
            file_level_spans.insert(path.clone(), file_span);
        }
    }
    for ((path, _), span) in span_map.iter_mut() {
        if let Some(file_span) = file_level_spans.get(path) {
            span.score += file_span.score * 0.5;
            for rule_id in &file_span.governing_rule_ids {
                span.governing_rule_ids.insert(rule_id.clone());
            }
            for reason in &file_span.reasons {
                span.add_reason(reason.clone());
            }
        }
    }

    let mut candidates: Vec<CandidateSpan> = span_map
        .into_values()
        .filter(|span| !symbols_only || span.symbol.is_some())
        .filter(|span| {
            path_prefix.is_empty()
                || path_prefix.iter().any(|prefix| span.path.starts_with(prefix.as_str()))
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    candidates.truncate(max_locations);

    // Step 4: Governance, Failed-Attempt & AST Topology Hydration for top candidates.
    let selected_paths: BTreeSet<String> = candidates.iter().map(|c| c.path.clone()).collect();
    let mut conceptual_bridges: BTreeSet<String> = BTreeSet::new();

    // Hydrate ancestor/exact GOVERNS via GraphRead::memories_for_file PLUS visible normative scan.
    if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
        .downcast_ref::<crate::store::LadybugStore>()
    {
        for path in &selected_paths {
            if let Ok(hits) = ls
                .memories_for_file(&namespace, None, path, None, true, &[], &[], 20)
                .await
            {
                for hit in hits {
                    if let Some(row) = rows_by_id.get(&hit.id) {
                        if extract_failed_attempt(row).is_some() {
                            continue;
                        }
                        for bridge in &hit.conceptual_bridges {
                            conceptual_bridges.insert(format!(
                                "{} -> {} -> {} ({})",
                                hit.id, bridge.via_id, bridge.target_id, bridge.target_domain
                            ));
                        }
                        let entry = rule_table.entry(row.id.clone()).or_insert_with(|| {
                            let gov_paths: BTreeSet<String> = extract_governed_targets(row)
                                .into_iter()
                                .map(|(p, _, _)| p)
                                .collect();
                            LocateRuleSummary {
                                id: row.id.clone(),
                                memory_type: row.payload.memory_type.clone(),
                                summary: truncate_chars(
                                    &row.payload.content,
                                    RULE_SUMMARY_MAX_CHARS,
                                ),
                                governs_paths: gov_paths.into_iter().collect(),
                            }
                        });
                        if !entry.governs_paths.contains(path) {
                            entry.governs_paths.push(path.clone());
                            entry.governs_paths.sort();
                        }
                        for cand in &mut candidates {
                            if &cand.path == path {
                                cand.governing_rule_ids.insert(row.id.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // Also attach any visible normative rule whose location / locations / governs
    // matches a selected path (including ancestor directories) and any failed attempts.
    let mut failed_attempts_map: BTreeMap<String, LocateFailedAttempt> = BTreeMap::new();
    for row in &visible_rows {
        let targets = extract_governed_targets(row);
        if let Some(mut attempt) = extract_failed_attempt(row) {
            let matches_selected_path = targets.iter().any(|(tp, _, _)| {
                selected_paths
                    .iter()
                    .any(|sp| path_matches_governance_target(sp, tp))
            });
            let matches_query = rrf_scores.contains_key(&row.id);
            if matches_selected_path || matches_query {
                for sp in &selected_paths {
                    if targets
                        .iter()
                        .any(|(tp, _, _)| path_matches_governance_target(sp, tp))
                        && !attempt.paths.contains(sp)
                    {
                        attempt.paths.push(sp.clone());
                    }
                }
                attempt.paths.sort();
                failed_attempts_map.insert(attempt.id.clone(), attempt);
            }
            continue;
        }
        if !is_normative_type(&row.payload.memory_type) {
            continue;
        }
        for sp in &selected_paths {
            let governs_sp = targets
                .iter()
                .any(|(tp, _, _)| path_matches_governance_target(sp, tp));
            if governs_sp {
                let entry = rule_table.entry(row.id.clone()).or_insert_with(|| {
                    let gov_paths: BTreeSet<String> =
                        targets.iter().map(|(p, _, _)| p.clone()).collect();
                    LocateRuleSummary {
                        id: row.id.clone(),
                        memory_type: row.payload.memory_type.clone(),
                        summary: truncate_chars(&row.payload.content, RULE_SUMMARY_MAX_CHARS),
                        governs_paths: gov_paths.into_iter().collect(),
                    }
                });
                if !entry.governs_paths.contains(sp) {
                    entry.governs_paths.push(sp.clone());
                    entry.governs_paths.sort();
                }
                for cand in &mut candidates {
                    if &cand.path == sp {
                        let matches_sym = targets
                            .iter()
                            .all(|(_, sym, _)| sym.is_none() || sym == &cand.symbol);
                        if matches_sym {
                            cand.governing_rule_ids.insert(row.id.clone());
                        }
                    }
                }
            }
        }
    }

    // Step 5: Compute AST topology counts and deterministic `why` explanation per span.
    let mut locations: Vec<LocatedSpan> = Vec::with_capacity(candidates.len());
    for mut cand in candidates {
        let mut callers_set: BTreeSet<String> = BTreeSet::new();
        let mut callees_set: BTreeSet<String> = BTreeSet::new();

        if let Some(rid) = &cand.row_id {
            if let Some(c) = symbol_callers.get(rid) {
                callers_set.extend(c.iter().cloned());
            }
            if let Some(c) = symbol_callees.get(rid) {
                callees_set.extend(c.iter().cloned());
            }
        } else if let Some(file_syms) = symbols_in_file.get(&cand.path) {
            for sid in file_syms {
                if let Some(c) = symbol_callers.get(sid) {
                    callers_set.extend(c.iter().cloned());
                }
                if let Some(c) = symbol_callees.get(sid) {
                    callees_set.extend(c.iter().cloned());
                }
            }
        }

        // 2-hop upstream blast radius (direct callers + callers of callers).
        let mut blast_radius_set: BTreeSet<String> = callers_set.clone();
        for caller_id in &callers_set {
            if let Some(second_hop) = symbol_callers.get(caller_id) {
                for hop_id in second_hop {
                    if Some(hop_id) != cand.row_id.as_ref() {
                        blast_radius_set.insert(hop_id.clone());
                    }
                }
            }
        }

        let callers_count = callers_set.len();
        let callees_count = callees_set.len();
        let blast_radius_count = blast_radius_set.len();
        let governing_rule_ids: Vec<String> = cand.governing_rule_ids.into_iter().collect();
        let rules_count = governing_rule_ids.len();

        // Add small centrality tie-breaker to score.
        let centrality_boost = ((callers_count + blast_radius_count) as f32).ln_1p() * 0.015;
        let final_score = ((cand.score + centrality_boost) * 10000.0).round() / 10000.0;

        if cand.reasons.is_empty() {
            cand.reasons.push("BM25 lexical".to_string());
        }
        let match_summary = cand.reasons.join(" + ");
        let rule_clause = if governing_rule_ids.is_empty() {
            String::new()
        } else {
            format!("; governed by {}", governing_rule_ids.join(", "))
        };
        let why = format!(
            "{match_summary}{rule_clause} (callers={callers_count}, callees={callees_count}, blast_radius={blast_radius_count}, rules={rules_count})"
        );

        locations.push(LocatedSpan {
            path: cand.path,
            lines: cand.lines,
            symbol: cand.symbol,
            symbol_kind: cand.symbol_kind,
            scip_symbol: cand.scip_symbol,
            signature: cand.signature,
            score: final_score,
            why,
            callers_count,
            callees_count,
            blast_radius_count,
            governing_rule_ids,
        });
    }

    // Only keep governing_rules that apply to returned locations (or matched the query directly).
    let referenced_rules: HashSet<String> = locations
        .iter()
        .flat_map(|l| l.governing_rule_ids.iter().cloned())
        .collect();
    let governing_rules: Vec<LocateRuleSummary> = rule_table
        .into_values()
        .filter(|r| referenced_rules.contains(&r.id))
        .collect();

    let mut resp = LocateResponse {
        namespace: namespace.clone(),
        query: query.clone(),
        locations,
        governing_rules,
        failed_attempts: failed_attempts_map.into_values().collect(),
        conceptual_bridges: conceptual_bridges.into_iter().collect(),
        ast_ingested,
        truncated_by_budget: false,
    };

    enforce_budget(&mut resp, max_bytes);

    // Step 6: Automatic Graph-First Ledger Accounting.
    let returned_paths: Vec<String> = {
        let mut seen = BTreeSet::new();
        for loc in &resp.locations {
            seen.insert(loc.path.clone());
        }
        seen.into_iter().collect()
    };
    if returned_paths.is_empty() {
        crate::task::record_graph_query(store, &namespace, "neurostrata_locate", &query).await;
    } else {
        for p in &returned_paths {
            crate::task::record_graph_query(store, &namespace, "neurostrata_locate", p).await;
        }
    }
    record_paths_in_project_ledger(
        request.project_root.as_deref(),
        &namespace,
        &returned_paths,
        &query,
    );

    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LadybugStore;
    use crate::traits::MemoryPayload;

    #[tokio::test]
    async fn locate_returns_symbol_span_callers_governing_rule_and_failed_attempt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn VectorStore> =
            Arc::new(LadybugStore::new(dir.path().join("db"), 384).expect("store init"));
        let ns = "LocateSpecNs";
        store.init(ns).await.unwrap();

        // 1. Target AST symbol: check_caller_path in src/daemon.rs
        store
            .upsert(
                ns,
                "sym-check-caller-path",
                vec![0.1; 384],
                MemoryPayload {
                    content: "functions check_caller_path in src/daemon.rs\nlines 142-188\n\npub fn check_caller_path(raw: &str, startup_root: &Path) -> Result<PathBuf>".to_string(),
                    location: "src/daemon.rs".to_string(),
                    location_lines: "142-188".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "check_caller_path",
                        "symbol_kind": "functions",
                        "scip_symbol": "rust-analyzer cargo neurostrata 0.1.0 daemon/check_caller_path().",
                        "calls": ["canonicalize_root"],
                        "calls_resolved": ["sym-canonicalize-root"],
                        "depends_on": ["sym-canonicalize-root"],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 2. Callee symbol
        store
            .upsert(
                ns,
                "sym-canonicalize-root",
                vec![0.1; 384],
                MemoryPayload {
                    content: "functions canonicalize_root in src/daemon.rs\nlines 100-140\n\nfn canonicalize_root(p: &Path) -> Result<PathBuf>".to_string(),
                    location: "src/daemon.rs".to_string(),
                    location_lines: "100-140".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "canonicalize_root",
                        "symbol_kind": "functions",
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 3. Two callers of check_caller_path
        for (cid, cname, lines) in [
            ("sym-ingest-handler", "handle_ingest", "210-250"),
            ("sym-backup-handler", "handle_backup", "260-300"),
        ] {
            store
                .upsert(
                    ns,
                    cid,
                    vec![0.1; 384],
                    MemoryPayload {
                        content: format!(
                            "functions {cname} in src/daemon.rs\nlines {lines}\n\nasync fn {cname}() {{ check_caller_path(); }}"
                        ),
                        location: "src/daemon.rs".to_string(),
                        location_lines: lines.to_string(),
                        memory_type: "code_ast".to_string(),
                        metadata: json!({
                            "domain": "code_ast",
                            "symbol": cname,
                            "symbol_kind": "functions",
                            "calls": ["check_caller_path"],
                            "calls_resolved": ["sym-check-caller-path"],
                            "depends_on": ["sym-check-caller-path"],
                        }),
                        user_id: "test".to_string(),
                        agent_name: None,
                    },
                )
                .await
                .unwrap();
        }

        // 4. Governing rule neurostrata-p1ex on src/daemon.rs
        store
            .upsert(
                ns,
                "neurostrata-p1ex",
                vec![0.1; 384],
                MemoryPayload {
                    content: "Every caller-supplied filesystem path on /ingest, /backup, /export-graph, and /tasks/import MUST be canonicalized via check_caller_path.".to_string(),
                    location: "src/daemon.rs".to_string(),
                    location_lines: "142-188".to_string(),
                    memory_type: "rule".to_string(),
                    metadata: json!({
                        "governs": ["src/daemon.rs"],
                        "locations": [{ "path": "src/daemon.rs", "symbol": "check_caller_path", "lines": "142-188" }],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 5. Failed attempt memory on src/daemon.rs
        store
            .upsert(
                ns,
                "attempt-symlink-bypass",
                vec![0.1; 384],
                MemoryPayload {
                    content: "Lexical starts_with without canonicalize allowed symlink escape.".to_string(),
                    location: "src/daemon.rs".to_string(),
                    location_lines: String::new(),
                    memory_type: "fact".to_string(),
                    metadata: json!({
                        "attempt": {
                            "approach": "Use Path::starts_with without std::fs::canonicalize",
                            "outcome": "failed",
                            "failure_mode": "Symlink inside workspace pointing to /etc bypassed prefix check"
                        },
                        "locations": [{ "path": "src/daemon.rs" }],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 6. Active in_progress task to verify graph_queries_count increment
        store
            .upsert(
                ns,
                "task-locate-1",
                vec![0.0; 384],
                MemoryPayload {
                    content: "Verify locate".to_string(),
                    location: "global".to_string(),
                    location_lines: String::new(),
                    memory_type: "task".to_string(),
                    metadata: json!({
                        "task": {
                            "id": "task-locate-1",
                            "title": "Verify locate",
                            "status": "in_progress",
                            "graph_queries_count": 0
                        }
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        let project_dir = tempfile::tempdir().expect("project_dir");
        let req = LocateRequest {
            namespace: ns.to_string(),
            query: "check_caller_path".to_string(),
            path_prefix: None,
            symbols_only: None,
            max_locations: Some(8),
            max_bytes: Some(6144),
            project_root: Some(project_dir.path().to_string_lossy().to_string()),
            principal: Some("admin".to_string()),
        };

        let resp = execute_locate(&store, None, &req, Some("admin"))
            .await
            .expect("execute_locate");

        assert!(resp.ast_ingested);
        assert!(!resp.locations.is_empty());
        let top = &resp.locations[0];
        assert_eq!(top.path, "src/daemon.rs");
        assert_eq!(top.symbol.as_deref(), Some("check_caller_path"));
        assert_eq!(top.lines.as_deref(), Some("142-188"));
        assert_eq!(top.callers_count, 2);
        assert_eq!(top.callees_count, 1);
        assert!(top.blast_radius_count >= 2);
        assert!(top
            .governing_rule_ids
            .contains(&"neurostrata-p1ex".to_string()));
        assert!(top.why.contains("check_caller_path"));

        assert!(resp
            .governing_rules
            .iter()
            .any(|r| r.id == "neurostrata-p1ex"));
        assert_eq!(resp.failed_attempts.len(), 1);
        assert_eq!(resp.failed_attempts[0].id, "attempt-symlink-bypass");

        // Acceptance Criterion 3: increments graph_queries_count and writes session_queries.json
        let (_, task_after) = store
            .get(ns, "task-locate-1")
            .await
            .unwrap()
            .expect("task exists");
        let count = task_after.metadata["task"]["graph_queries_count"]
            .as_i64()
            .unwrap_or(0);
        assert!(
            count >= 1,
            "graph_queries_count must increment, got {count}"
        );

        let ledger_text =
            std::fs::read_to_string(project_dir.path().join(".NeuroStrata/session_queries.json"))
                .expect("ledger written");
        assert!(ledger_text.contains("neurostrata_locate"));
        assert!(ledger_text.contains("src/daemon.rs"));

        // Acceptance Criterion 4: max_bytes >= 1024 budget enforcement
        let tight_req = LocateRequest {
            max_bytes: Some(1024),
            ..req
        };
        let tight_resp = execute_locate(&store, None, &tight_req, Some("admin"))
            .await
            .expect("tight locate");
        let bytes = serde_json::to_vec(&tight_resp).unwrap();
        assert!(
            bytes.len() <= 1024,
            "serialized size {} exceeded 1024",
            bytes.len()
        );
    }
}
