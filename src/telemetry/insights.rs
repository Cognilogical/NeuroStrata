use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sparesults::QueryResultsFormat;

use crate::formal_kernel::{evaluate_insight_define, is_memory_valid_at, InsightDefineDecision};
use crate::rebac::{can_read_memory, resolve_principal};
use crate::registry::{registry_content, registry_metadata, registry_row_id};
use crate::telemetry::anomaly::MetricKind;
use crate::time_series::RollingAgg;
use crate::traits::{MemoryPayload, SearchResult, VectorStore};

pub const MAX_CUSTOM_INSIGHTS: usize = 16;

pub const BUILTIN_INSIGHTS: [(&str, &str); 4] = [
    ("governance_coverage_pct", "Governance Coverage (%)"),
    ("deferred_edge_debt", "Deferred Edge Debt"),
    ("ungoverned_hotspot_count", "Ungoverned Hotspot Count"),
    ("task_knowledge_yield", "Task Knowledge Yield"),
];

pub fn is_builtin_insight_slug(slug: &str) -> bool {
    BUILTIN_INSIGHTS.iter().any(|(name, _)| *name == slug)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightQueryKind {
    Sparql,
    Lexical,
    AstCalls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightDefinition {
    pub name: String,
    pub title: String,
    pub query_kind: InsightQueryKind,
    pub query: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightPoint {
    pub ts: i64,
    #[serde(default)]
    pub generation: Option<u64>,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightSeriesReport {
    pub name: String,
    pub title: String,
    pub builtin: bool,
    pub current_value: f64,
    pub delta_from_first: f64,
    pub rolling_summary: Value,
    pub points: Vec<InsightPoint>,
    pub top_offenders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightsRequest {
    pub namespace: String,
    /// "snapshot" | "series" | "define" | "remove"
    pub action: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub query_kind: Option<InsightQueryKind>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub from_ts: Option<i64>,
    #[serde(default)]
    pub to_ts: Option<i64>,
    #[serde(default)]
    pub backfill_points: Option<usize>,
    #[serde(default)]
    pub principal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightsResponse {
    pub namespace: String,
    pub action: String,
    pub series: Vec<InsightSeriesReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defined: Option<InsightDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
}

fn normalize_insight_slug(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Err("insight name cannot be empty".to_string());
    }
    let mut slug = String::with_capacity(trimmed.len());
    let mut prev_sep = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_sep = false;
        } else if (ch == '-' || ch == '_' || ch == ' ') && !slug.is_empty() && !prev_sep {
            slug.push(if ch == '_' { '_' } else { '-' });
            prev_sep = true;
        }
    }
    while slug.ends_with('-') || slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(format!(
            "insight name '{raw}' must contain ASCII alphanumeric characters"
        ));
    }
    Ok(slug)
}

/// Resolve an insight's persisted prefix through the shared prefix-forms
/// helper at evaluation time. SPARQL insights and empty prefixes need no
/// resolution; lexical/ast_calls prefixes fail closed on unresolvable absolute
/// values.
fn resolve_insight_prefix_forms(
    def: &InsightDefinition,
    project_root: Option<&str>,
    registry_root: Option<&str>,
) -> Result<Vec<String>, String> {
    if matches!(def.query_kind, InsightQueryKind::Sparql) {
        return Ok(Vec::new());
    }
    let Some(raw) = def
        .path_prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(Vec::new());
    };
    crate::paths::prefix_lookup_forms(raw, project_root, registry_root).map_err(|e| e.to_string())
}

pub async fn load_custom_insights(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
) -> Vec<InsightDefinition> {
    let reg_id = registry_row_id(namespace);
    let Ok(Some((_, payload))) = store.get(namespace, &reg_id).await else {
        return Vec::new();
    };
    let Some(arr) = payload
        .metadata
        .get("insight_definitions")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let mut defs: Vec<InsightDefinition> = arr
        .iter()
        .filter_map(|v| serde_json::from_value::<InsightDefinition>(v.clone()).ok())
        .collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs
}

pub async fn save_custom_insight(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    mut def: InsightDefinition,
) -> Result<InsightDefinition, String> {
    // The registry row's one lock: a cached-root or domain write between
    // this read and the upsert below would otherwise be reverted.
    let _guard = crate::registry::REGISTRY_ROW_LOCK.lock().await;
    def.name = normalize_insight_slug(&def.name)?;
    if def.title.trim().is_empty() {
        def.title = def.name.clone();
    }

    let reg_id = registry_row_id(namespace);
    let (vector, mut payload) = match store.get(namespace, &reg_id).await {
        Ok(Some(found)) => found,
        Ok(None) => (
            vec![0.0; 384],
            MemoryPayload {
                content: registry_content(namespace),
                user_id: "system".to_string(),
                memory_type: "fact".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: registry_metadata(&[]),
            },
        ),
        Err(e) => return Err(format!("failed to read registry row: {e}")),
    };

    let mut existing_defs: Vec<InsightDefinition> = payload
        .metadata
        .get("insight_definitions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<InsightDefinition>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let is_existing = existing_defs.iter().any(|d| d.name == def.name);
    let decision = evaluate_insight_define(
        !def.query.trim().is_empty(),
        is_builtin_insight_slug(&def.name),
        is_existing,
        existing_defs.len() as u32,
        MAX_CUSTOM_INSIGHTS as u32,
    );
    match decision {
        InsightDefineDecision::DefineAllowed => {}
        InsightDefineDecision::RejectedEmptyQuery => {
            return Err("custom insight query must not be empty".to_string());
        }
        InsightDefineDecision::RejectedBuiltinCollision => {
            return Err(format!(
                "insight '{}' is a built-in insight and cannot be overwritten",
                def.name
            ));
        }
        InsightDefineDecision::RejectedQuotaExceeded => {
            return Err(format!(
                "namespace '{namespace}' already has the maximum of {MAX_CUSTOM_INSIGHTS} custom insights"
            ));
        }
    }

    if let Some(slot) = existing_defs.iter_mut().find(|d| d.name == def.name) {
        *slot = def.clone();
    } else {
        existing_defs.push(def.clone());
    }
    existing_defs.sort_by(|a, b| a.name.cmp(&b.name));

    if !payload.metadata.is_object() {
        payload.metadata = registry_metadata(&[]);
    }
    if let Some(obj) = payload.metadata.as_object_mut() {
        if !obj.contains_key("domain_registry") {
            obj.insert(
                "domain_registry".to_string(),
                serde_json::json!({ "domains": [] }),
            );
        }
        obj.insert("eligibility".to_string(), serde_json::json!("private"));
        obj.insert(
            "insight_definitions".to_string(),
            serde_json::to_value(&existing_defs).unwrap_or_else(|_| serde_json::json!([])),
        );
    }

    let vec_to_use = if vector.is_empty() {
        vec![0.0; 384]
    } else {
        vector
    };
    store
        .upsert(namespace, &reg_id, vec_to_use, payload)
        .await
        .map_err(|e| format!("failed to persist insight definition: {e}"))?;

    Ok(def)
}

pub async fn remove_custom_insight(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    raw_name: &str,
) -> Result<bool, String> {
    // The registry row's one lock: a cached-root or domain write between
    // this read and the upsert below would otherwise be reverted.
    let _guard = crate::registry::REGISTRY_ROW_LOCK.lock().await;
    let slug = normalize_insight_slug(raw_name)?;
    if is_builtin_insight_slug(&slug) {
        return Err(format!("built-in insight '{slug}' cannot be removed"));
    }

    let reg_id = registry_row_id(namespace);
    let Ok(Some((vector, mut payload))) = store.get(namespace, &reg_id).await else {
        return Ok(false);
    };
    let mut existing_defs: Vec<InsightDefinition> = payload
        .metadata
        .get("insight_definitions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<InsightDefinition>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let before_len = existing_defs.len();
    existing_defs.retain(|d| d.name != slug);
    if existing_defs.len() == before_len {
        return Ok(false);
    }

    if let Some(obj) = payload.metadata.as_object_mut() {
        obj.insert("eligibility".to_string(), serde_json::json!("private"));
        obj.insert(
            "insight_definitions".to_string(),
            serde_json::to_value(&existing_defs).unwrap_or_else(|_| serde_json::json!([])),
        );
    }
    let vec_to_use = if vector.is_empty() {
        vec![0.0; 384]
    } else {
        vector
    };
    store
        .upsert(namespace, &reg_id, vec_to_use, payload)
        .await
        .map_err(|e| format!("failed to update registry row: {e}"))?;
    Ok(true)
}

fn parse_timestamp_value(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Ok(n) = trimmed.parse::<i64>() {
            return Some(n);
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
            return Some(dt.timestamp());
        }
    }
    None
}

fn row_is_valid_at(meta: &Value, as_of_ts: Option<i64>) -> bool {
    if meta
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    let Some(as_of) = as_of_ts else {
        if meta
            .get("superseded_by")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            return false;
        }
        if let Some(vt) = meta.get("valid_to").and_then(parse_timestamp_value) {
            let now = Utc::now().timestamp();
            if vt <= now {
                return false;
            }
        }
        return true;
    };

    let vf = meta
        .get("valid_from")
        .or_else(|| meta.get("created_at"))
        .and_then(parse_timestamp_value)
        .unwrap_or(0);
    let vt = meta.get("valid_to").and_then(parse_timestamp_value);
    is_memory_valid_at(vf, vt.is_some(), vt.unwrap_or(0), as_of)
}

fn is_structural_row(row: &SearchResult) -> bool {
    if row
        .payload
        .metadata
        .get("stub")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    matches!(
        row.payload.memory_type.as_str(),
        "file" | "symbol" | "code_ast"
    )
}

fn extract_node_path(row: &SearchResult) -> Option<String> {
    let loc = row.payload.location.trim();
    if !loc.is_empty() && loc != "global" && loc != "unknown" {
        let base = loc.split('#').next().unwrap_or(loc);
        let norm = crate::parser::ingest::normalize_node_path(base);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    row.payload
        .metadata
        .get("relative_path")
        .or_else(|| row.payload.metadata.get("path"))
        .and_then(|v| v.as_str())
        .map(crate::parser::ingest::normalize_node_path)
        .filter(|s| !s.is_empty())
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

fn extract_rule_governed_paths(row: &SearchResult) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let loc = row.payload.location.trim();
    if !loc.is_empty() && loc != "global" && loc != "unknown" {
        let base = crate::parser::ingest::normalize_node_path(loc.split('#').next().unwrap_or(loc));
        if !base.is_empty() {
            paths.insert(base);
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
                    paths.insert(norm);
                }
            }
        }
    }
    if let Some(gov) = row.payload.metadata.get("governs") {
        match gov {
            Value::String(s) => {
                let norm = crate::parser::ingest::normalize_node_path(s);
                if !norm.is_empty() {
                    paths.insert(norm);
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let norm = crate::parser::ingest::normalize_node_path(s);
                        if !norm.is_empty() {
                            paths.insert(norm);
                        }
                    } else if let Some(p) = item.get("path").and_then(|v| v.as_str()) {
                        let norm = crate::parser::ingest::normalize_node_path(p);
                        if !norm.is_empty() {
                            paths.insert(norm);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    paths
}

fn path_is_governed_by_any(node_path: &str, governed_targets: &BTreeSet<String>) -> bool {
    let norm = crate::parser::ingest::normalize_node_path(node_path);
    if norm.is_empty() {
        return false;
    }
    if governed_targets.contains(&norm) {
        return true;
    }
    for target in governed_targets {
        let prefix = format!("{}/", target.trim_end_matches('/'));
        if norm.starts_with(&prefix) {
            return true;
        }
    }
    false
}

/// Evaluate a single built-in or custom insight at `as_of` (`None` = live active state),
/// returning `(value, top_offenders)`.
async fn evaluate_insight_value(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    insight_name: &str,
    custom_def: Option<&InsightDefinition>,
    visible_rows: &[SearchResult],
    prefix_forms: &[String],
    as_of: Option<i64>,
) -> (f64, Vec<String>) {
    let valid_rows: Vec<&SearchResult> = visible_rows
        .iter()
        .filter(|r| row_is_valid_at(&r.payload.metadata, as_of))
        .collect();

    // Collect all governed paths across active `rule` memories at `as_of`.
    let mut governed_targets: BTreeSet<String> = BTreeSet::new();
    for r in &valid_rows {
        if r.payload.memory_type == "rule" {
            governed_targets.extend(extract_rule_governed_paths(r));
        }
    }

    match insight_name {
        "governance_coverage_pct" => {
            let mut total_structural = 0usize;
            let mut governed_structural = 0usize;
            let mut ungoverned_offenders: Vec<String> = Vec::new();
            for r in &valid_rows {
                if !is_structural_row(r) {
                    continue;
                }
                let Some(path) = extract_node_path(r) else {
                    continue;
                };
                total_structural += 1;
                if path_is_governed_by_any(&path, &governed_targets) {
                    governed_structural += 1;
                } else if ungoverned_offenders.len() < 10 {
                    let label = match extract_symbol_name(r) {
                        Some(sym) => format!("{path}::{sym}"),
                        None => path,
                    };
                    if !ungoverned_offenders.contains(&label) {
                        ungoverned_offenders.push(label);
                    }
                }
            }
            let pct = if total_structural == 0 {
                100.0
            } else {
                ((governed_structural as f64) / (total_structural as f64)) * 100.0
            };
            (pct.clamp(0.0, 100.0), ungoverned_offenders)
        }
        "deferred_edge_debt" => {
            let mut debt_count = 0usize;
            let mut offenders: Vec<String> = Vec::new();
            for r in &valid_rows {
                if let Some(arr) = r
                    .payload
                    .metadata
                    .get("deferred_edges")
                    .and_then(|v| v.as_array())
                {
                    for entry in arr {
                        debt_count += 1;
                        if offenders.len() < 10 {
                            let target = entry
                                .get("target")
                                .or_else(|| entry.get("path"))
                                .or_else(|| entry.get("symbol"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("unresolved");
                            offenders.push(format!("{} -> {}", r.id, target));
                        }
                    }
                }
            }
            (debt_count as f64, offenders)
        }
        "ungoverned_hotspot_count" => {
            // Identify paths/symbols associated with failed attempt memories.
            let mut failed_attempt_paths: BTreeSet<String> = BTreeSet::new();
            for r in &valid_rows {
                let is_failed = r
                    .payload
                    .metadata
                    .get("attempt")
                    .and_then(|a| a.get("outcome"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.eq_ignore_ascii_case("failed"))
                    .unwrap_or(false);
                if is_failed {
                    failed_attempt_paths.extend(extract_rule_governed_paths(r));
                }
            }

            // Count incoming dependencies per structural node id.
            let mut incoming_counts: HashMap<String, usize> = HashMap::new();
            for r in &valid_rows {
                if !is_structural_row(r) {
                    continue;
                }
                for key in ["depends_on", "calls_resolved"] {
                    if let Some(arr) = r.payload.metadata.get(key).and_then(|v| v.as_array()) {
                        for target in arr.iter().filter_map(|v| v.as_str()) {
                            if target != r.id {
                                *incoming_counts.entry(target.to_string()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }

            let mut hotspot_count = 0usize;
            let mut offenders: Vec<String> = Vec::new();
            for r in &valid_rows {
                if !is_structural_row(r) {
                    continue;
                }
                let Some(path) = extract_node_path(r) else {
                    continue;
                };
                if path_is_governed_by_any(&path, &governed_targets) {
                    continue;
                }
                let explicit_blast = r
                    .payload
                    .metadata
                    .get("blast_radius_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let caller_list_len = r
                    .payload
                    .metadata
                    .get("cross_file_callers")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let in_deg = incoming_counts
                    .get(&r.id)
                    .copied()
                    .unwrap_or(0)
                    .max(explicit_blast)
                    .max(caller_list_len);
                let has_failed_attempt = path_is_governed_by_any(&path, &failed_attempt_paths);

                if in_deg >= 5 || has_failed_attempt {
                    hotspot_count += 1;
                    if offenders.len() < 10 {
                        let label = match extract_symbol_name(r) {
                            Some(sym) => format!("{path}::{sym} (blast={in_deg})"),
                            None => format!("{path} (blast={in_deg})"),
                        };
                        offenders.push(label);
                    }
                }
            }
            (hotspot_count as f64, offenders)
        }
        "task_knowledge_yield" => {
            let mut extracted_count = 0usize;
            let mut offenders: Vec<String> = Vec::new();
            let mut tasks_with_extractions: BTreeSet<String> = BTreeSet::new();

            for r in &valid_rows {
                if let Some(ext) = r.payload.metadata.get("extracted_from") {
                    match ext {
                        Value::String(tid) if !tid.trim().is_empty() => {
                            extracted_count += 1;
                            tasks_with_extractions.insert(tid.trim().to_string());
                        }
                        Value::Array(arr) => {
                            let mut has_any = false;
                            for v in arr.iter().filter_map(|v| v.as_str()) {
                                if !v.trim().is_empty() {
                                    has_any = true;
                                    tasks_with_extractions.insert(v.trim().to_string());
                                }
                            }
                            if has_any {
                                extracted_count += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }

            let mut done_tasks = 0usize;
            for r in visible_rows {
                if r.payload.memory_type != "task" {
                    continue;
                }
                let status = r
                    .payload
                    .metadata
                    .get("status")
                    .or_else(|| r.payload.metadata.get("task").and_then(|t| t.get("status")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if status != "done" {
                    continue;
                }
                if let Some(cutoff) = as_of {
                    let updated_ts =
                        r.payload
                            .metadata
                            .get("updated_at")
                            .or_else(|| r.payload.metadata.get("created_at"))
                            .or_else(|| {
                                r.payload.metadata.get("task").and_then(|t| {
                                    t.get("updated_at").or_else(|| t.get("created_at"))
                                })
                            })
                            .and_then(parse_timestamp_value)
                            .unwrap_or(0);
                    if updated_ts > cutoff {
                        continue;
                    }
                }
                done_tasks += 1;
                if !tasks_with_extractions.contains(&r.id) && offenders.len() < 10 {
                    offenders.push(r.id.clone());
                }
            }

            let yield_ratio = if done_tasks == 0 {
                0.0
            } else {
                (extracted_count as f64) / (done_tasks as f64)
            };
            (yield_ratio, offenders)
        }
        _ => {
            let Some(def) = custom_def else {
                return (0.0, Vec::new());
            };
            evaluate_custom_insight(store, namespace, def, &valid_rows, prefix_forms, as_of).await
        }
    }
}

async fn evaluate_custom_insight(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    def: &InsightDefinition,
    valid_rows: &[&SearchResult],
    prefix_forms: &[String],
    as_of: Option<i64>,
) -> (f64, Vec<String>) {
    match def.query_kind {
        InsightQueryKind::Lexical => {
            let mut matched_ids: BTreeSet<String> = BTreeSet::new();
            let mut offenders: Vec<String> = Vec::new();
            let valid_by_id: HashMap<&str, &SearchResult> =
                valid_rows.iter().map(|r| (r.id.as_str(), *r)).collect();

            if as_of.is_none() {
                if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
                    .downcast_ref::<crate::store::LadybugStore>()
                {
                    if let Ok(hits) = ls
                        .search_lexical(namespace, &def.query, 500, false, false, &[], "admin", 0)
                        .await
                    {
                        for hit in hits.results {
                            if let Some(r) = valid_by_id.get(hit.id.as_str()) {
                                if !prefix_forms.is_empty() {
                                    let p = extract_node_path(r).unwrap_or_default();
                                    if !crate::search::row_matches_prefix_forms(
                                        &p,
                                        &r.payload.metadata,
                                        prefix_forms,
                                    ) {
                                        continue;
                                    }
                                }
                                if matched_ids.insert(r.id.clone()) && offenders.len() < 10 {
                                    let label =
                                        extract_node_path(r).unwrap_or_else(|| r.id.clone());
                                    offenders.push(label);
                                }
                            }
                        }
                    }
                }
            }

            // Also run case-insensitive token/substring scan across `valid_rows` so
            // historical `as_of` slices and unindexed rows are always covered.
            let needle = def.query.trim().to_ascii_lowercase();
            if !needle.is_empty() {
                for r in valid_rows {
                    if matched_ids.contains(&r.id) {
                        continue;
                    }
                    if !prefix_forms.is_empty() {
                        let p = extract_node_path(r).unwrap_or_default();
                        if !crate::search::row_matches_prefix_forms(
                            &p,
                            &r.payload.metadata,
                            prefix_forms,
                        ) {
                            continue;
                        }
                    }
                    let hay_content = r.payload.content.to_ascii_lowercase();
                    let hay_sym = extract_symbol_name(r)
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if hay_content.contains(&needle) || hay_sym.contains(&needle) {
                        matched_ids.insert(r.id.clone());
                        if offenders.len() < 10 {
                            let label = extract_node_path(r).unwrap_or_else(|| r.id.clone());
                            offenders.push(label);
                        }
                    }
                }
            }

            (matched_ids.len() as f64, offenders)
        }
        InsightQueryKind::AstCalls => {
            let needle = def.query.trim().to_ascii_lowercase();
            let mut count = 0usize;
            let mut offenders: Vec<String> = Vec::new();
            for r in valid_rows {
                if !is_structural_row(r) {
                    continue;
                }
                let path = extract_node_path(r).unwrap_or_default();
                if !prefix_forms.is_empty()
                    && !crate::search::row_matches_prefix_forms(
                        &path,
                        &r.payload.metadata,
                        prefix_forms,
                    )
                {
                    continue;
                }
                let mut calls_match = false;
                if let Some(arr) = r.payload.metadata.get("calls").and_then(|v| v.as_array()) {
                    for c in arr.iter().filter_map(|v| v.as_str()) {
                        let c_low = c.to_ascii_lowercase();
                        if c_low == needle || c_low.ends_with(&format!("::{needle}")) {
                            calls_match = true;
                            break;
                        }
                    }
                }
                if !calls_match {
                    for key in ["cross_file_callees", "scip_callees"] {
                        if let Some(arr) = r.payload.metadata.get(key).and_then(|v| v.as_array()) {
                            for item in arr {
                                let sym = item
                                    .get("symbol_name")
                                    .or_else(|| item.get("scip_symbol"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_ascii_lowercase();
                                if !sym.is_empty() && sym.contains(&needle) {
                                    calls_match = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if calls_match {
                    count += 1;
                    if offenders.len() < 10 {
                        let label = match extract_symbol_name(r) {
                            Some(sym) => format!("{path}::{sym}"),
                            None => path,
                        };
                        offenders.push(label);
                    }
                }
            }
            (count as f64, offenders)
        }
        InsightQueryKind::Sparql => {
            let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
                .downcast_ref::<crate::store::LadybugStore>()
            else {
                return (0.0, Vec::new());
            };
            let config = crate::rdf::sparql::SparqlConfig {
                timeout_ms: 250,
                max_solutions: 1000,
                max_dataset_quads: 250_000,
                max_algebra_nodes: 1000,
            };
            let query_str = if let Some(ts) = as_of {
                def.query.replace("$AS_OF_TS", &ts.to_string())
            } else {
                def.query.clone()
            };
            let Ok((json_str, count, _, _)) = crate::rdf::sparql::execute_sparql_query_with_config(
                ls,
                namespace,
                &query_str,
                QueryResultsFormat::Json,
                as_of.is_some(),
                false,
                config,
            )
            .await
            else {
                return (0.0, Vec::new());
            };
            if let Ok(parsed) = serde_json::from_str::<Value>(&json_str) {
                if let Some(bindings) = parsed
                    .get("results")
                    .and_then(|r| r.get("bindings"))
                    .and_then(|b| b.as_array())
                {
                    if let Some(first) = bindings.first() {
                        if let Some(val_str) = first
                            .get("value")
                            .and_then(|v| v.get("value"))
                            .and_then(|v| v.as_str())
                        {
                            if let Ok(num) = val_str.parse::<f64>() {
                                if num.is_finite() {
                                    return (num, Vec::new());
                                }
                            }
                        }
                    }
                }
            }
            (count as f64, Vec::new())
        }
    }
}

async fn build_series_report(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    name: &str,
    title: &str,
    builtin: bool,
    custom_def: Option<&InsightDefinition>,
    visible_rows: &[SearchResult],
    prefix_forms: &[String],
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    backfill_points: Option<usize>,
) -> InsightSeriesReport {
    let (current_value, top_offenders) = evaluate_insight_value(
        store,
        namespace,
        name,
        custom_def,
        visible_rows,
        prefix_forms,
        None,
    )
    .await;
    let now_ts = Utc::now().timestamp();

    let mut points_by_ts: BTreeMap<i64, InsightPoint> = BTreeMap::new();
    let mut agg = RollingAgg::new(0.2);

    if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
        .downcast_ref::<crate::store::LadybugStore>()
    {
        if let Ok(samples) = ls
            .query_db_metrics(
                namespace,
                MetricKind::ArchitecturalInsight,
                name,
                from_ts,
                to_ts,
                500,
            )
            .await
        {
            for s in samples {
                let gen = s
                    .labels
                    .get("generation")
                    .and_then(|g| g.parse::<u64>().ok());
                points_by_ts.insert(
                    s.ts,
                    InsightPoint {
                        ts: s.ts,
                        generation: gen,
                        value: s.value,
                    },
                );
            }
        }
        let key = (
            namespace.to_string(),
            MetricKind::ArchitecturalInsight,
            name.to_string(),
        );
        if let Some(existing_agg) = ls.metrics.aggregate(&key) {
            agg = existing_agg;
        }
    }

    // Bitemporal `as_of` backfill across `[from_ts, to_ts]` when requested.
    if let (Some(f_ts), Some(t_ts)) = (from_ts, to_ts) {
        let steps = backfill_points.unwrap_or(0).clamp(0, 64);
        if steps > 1 && t_ts > f_ts {
            let span = t_ts - f_ts;
            for i in 0..steps {
                let step_ts = f_ts + ((span * (i as i64)) / ((steps - 1) as i64));
                if !points_by_ts.contains_key(&step_ts) {
                    let (val, _) = evaluate_insight_value(
                        store,
                        namespace,
                        name,
                        custom_def,
                        visible_rows,
                        prefix_forms,
                        Some(step_ts),
                    )
                    .await;
                    points_by_ts.insert(
                        step_ts,
                        InsightPoint {
                            ts: step_ts,
                            generation: None,
                            value: val,
                        },
                    );
                }
            }
        }
    }

    if points_by_ts.is_empty() {
        points_by_ts.insert(
            now_ts,
            InsightPoint {
                ts: now_ts,
                generation: None,
                value: current_value,
            },
        );
    }

    let points: Vec<InsightPoint> = points_by_ts.into_values().collect();
    let first_val = points.first().map(|p| p.value).unwrap_or(current_value);
    let delta_from_first = current_value - first_val;

    if agg.count == 0 {
        agg.count = points.len() as u64;
        agg.sum = points.iter().map(|p| p.value).sum();
        agg.min = points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min);
        agg.max = points
            .iter()
            .map(|p| p.value)
            .fold(f64::NEG_INFINITY, f64::max);
        agg.ewma = current_value;
    }

    InsightSeriesReport {
        name: name.to_string(),
        title: title.to_string(),
        builtin,
        current_value,
        delta_from_first,
        rolling_summary: agg.to_json_summary(),
        points,
        top_offenders,
    }
}

/// Automatically sample all 4 built-in insights and any custom `InsightDefinition`s
/// for `namespace` into `TimeSeriesStore` (`MetricKind::ArchitecturalInsight`) and
/// flush to `MemoryMetric` (`neurostrata-kx2n`).
pub async fn sample_insights_after_ingest(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    generation: Option<u64>,
) {
    let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
        .downcast_ref::<crate::store::LadybugStore>()
    else {
        return;
    };

    let all_rows = store.list(namespace, None, None).await.unwrap_or_default();
    let visible_rows: Vec<SearchResult> = all_rows
        .into_iter()
        .filter(|r| !crate::registry::is_registry_row(&r.payload.metadata))
        .filter(|r| r.payload.memory_type != "action_receipt")
        .collect();

    let mut labels = BTreeMap::new();
    if let Some(gen) = generation {
        labels.insert("generation".to_string(), gen.to_string());
    }
    let now_ts = Utc::now().timestamp();

    for (slug, _) in BUILTIN_INSIGHTS {
        let (val, _) =
            evaluate_insight_value(store, namespace, slug, None, &visible_rows, &[], None).await;
        ls.metrics.record(
            namespace,
            MetricKind::ArchitecturalInsight,
            slug,
            now_ts,
            val,
            labels.clone(),
        );
    }

    let custom_defs = load_custom_insights(store, namespace).await;
    let Ok(registry_root) = crate::registry::cached_project_root(store, namespace).await else {
        let _ = ls.flush_metrics().await;
        return;
    };
    for def in &custom_defs {
        let prefix_forms = match resolve_insight_prefix_forms(def, None, registry_root.as_deref()) {
            Ok(forms) => forms,
            Err(_) => continue,
        };
        let (val, _) = evaluate_insight_value(
            store,
            namespace,
            &def.name,
            Some(def),
            &visible_rows,
            &prefix_forms,
            None,
        )
        .await;
        ls.metrics.record(
            namespace,
            MetricKind::ArchitecturalInsight,
            &def.name,
            now_ts,
            val,
            labels.clone(),
        );
    }

    let _ = ls.flush_metrics().await;
}

pub async fn execute_insights(
    store: &Arc<dyn VectorStore>,
    request: &InsightsRequest,
    trusted_principal: Option<&str>,
) -> Result<InsightsResponse, String> {
    let namespace = crate::server::resolve_namespace(store, &request.namespace).await;
    let action = request.action.trim().to_ascii_lowercase();
    let principal = resolve_principal(
        trusted_principal.unwrap_or("admin"),
        request.principal.as_deref(),
    );

    let load_visible_rows = || async {
        let all_rows = store.list(&namespace, None, None).await.unwrap_or_default();
        all_rows
            .into_iter()
            .filter(|r| !crate::registry::is_registry_row(&r.payload.metadata))
            .filter(|r| r.payload.memory_type != "action_receipt")
            .filter(|r| can_read_memory(&principal, &r.payload.user_id, &r.payload.metadata))
            .collect::<Vec<SearchResult>>()
    };

    match action.as_str() {
        "define" => {
            let registry_root = crate::registry::cached_project_root(store, &namespace)
                .await
                .map_err(|e| e.to_string())?;
            let raw_name = request
                .name
                .as_deref()
                .ok_or_else(|| "name is required for action 'define'".to_string())?;
            let query_kind = request
                .query_kind
                .ok_or_else(|| "query_kind ('sparql' | 'lexical' | 'ast_calls') is required for action 'define'".to_string())?;
            let query = request
                .query
                .clone()
                .unwrap_or_default();
            let def = InsightDefinition {
                name: raw_name.to_string(),
                title: request
                    .title
                    .clone()
                    .unwrap_or_else(|| raw_name.to_string()),
                query_kind,
                query,
                path_prefix: request.path_prefix.clone(),
                created_at: Utc::now().timestamp(),
            };
            // Resolve before saving so an unresolvable absolute prefix fails
            // closed on `define` rather than persisting an unusable insight.
            let prefix_forms = resolve_insight_prefix_forms(
                &def,
                request.project_root.as_deref(),
                registry_root.as_deref(),
            )?;
            let saved = save_custom_insight(store, &namespace, def).await?;
            let visible_rows = load_visible_rows().await;
            let (val, _) = evaluate_insight_value(
                store,
                &namespace,
                &saved.name,
                Some(&saved),
                &visible_rows,
                &prefix_forms,
                None,
            )
            .await;
            if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
                .downcast_ref::<crate::store::LadybugStore>()
            {
                ls.metrics.record(
                    &namespace,
                    MetricKind::ArchitecturalInsight,
                    &saved.name,
                    Utc::now().timestamp(),
                    val,
                    BTreeMap::new(),
                );
                let _ = ls.flush_metrics().await;
            }
            let report = build_series_report(
                store,
                &namespace,
                &saved.name,
                &saved.title,
                false,
                Some(&saved),
                &visible_rows,
                &prefix_forms,
                request.from_ts,
                request.to_ts,
                request.backfill_points,
            )
            .await;
            Ok(InsightsResponse {
                namespace,
                action,
                series: vec![report],
                defined: Some(saved),
                removed: None,
            })
        }
        "remove" => {
            let raw_name = request
                .name
                .as_deref()
                .ok_or_else(|| "name is required for action 'remove'".to_string())?;
            let removed = remove_custom_insight(store, &namespace, raw_name).await?;
            Ok(InsightsResponse {
                namespace,
                action,
                series: Vec::new(),
                defined: None,
                removed: Some(removed),
            })
        }
        "snapshot" | "series" => {
            let registry_root = crate::registry::cached_project_root(store, &namespace)
                .await
                .map_err(|e| e.to_string())?;
            let visible_rows = load_visible_rows().await;
            let custom_defs = load_custom_insights(store, &namespace).await;
            let filter_name = request
                .name
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());

            let mut series = Vec::new();
            let no_prefix_forms: Vec<String> = Vec::new();
            for (slug, title) in BUILTIN_INSIGHTS {
                if let Some(target) = filter_name {
                    if target != slug {
                        continue;
                    }
                }
                let report = build_series_report(
                    store,
                    &namespace,
                    slug,
                    title,
                    true,
                    None,
                    &visible_rows,
                    &no_prefix_forms,
                    request.from_ts,
                    request.to_ts,
                    request.backfill_points,
                )
                .await;
                series.push(report);
            }

            for def in &custom_defs {
                if let Some(target) = filter_name {
                    if target != def.name {
                        continue;
                    }
                }
                let prefix_forms = resolve_insight_prefix_forms(
                    def,
                    request.project_root.as_deref(),
                    registry_root.as_deref(),
                )?;
                let report = build_series_report(
                    store,
                    &namespace,
                    &def.name,
                    &def.title,
                    false,
                    Some(def),
                    &visible_rows,
                    &prefix_forms,
                    request.from_ts,
                    request.to_ts,
                    request.backfill_points,
                )
                .await;
                series.push(report);
            }

            if let Some(target) = filter_name {
                if series.is_empty() {
                    return Err(format!(
                        "unknown insight '{target}' in namespace '{namespace}'"
                    ));
                }
            }

            crate::task::record_graph_query(
                store,
                &namespace,
                "neurostrata_insights",
                filter_name.unwrap_or("snapshot"),
            )
            .await;

            Ok(InsightsResponse {
                namespace,
                action,
                series,
                defined: None,
                removed: None,
            })
        }
        other => Err(format!(
            "unsupported insights action '{other}'; expected 'snapshot', 'series', 'define', or 'remove'"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LadybugStore;
    use serde_json::json;

    #[tokio::test]
    async fn snapshot_custom_define_and_bitemporal_series_work_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn VectorStore> =
            Arc::new(LadybugStore::new(dir.path().join("db"), 384).expect("store init"));
        let ns = "InsightsSpecNs";
        store.init(ns).await.unwrap();

        // 1. Governed symbol in src/daemon.rs
        store
            .upsert(
                ns,
                "sym-daemon-check",
                vec![0.1; 384],
                MemoryPayload {
                    content:
                        "pub(crate) fn check_caller_path(raw: &Path) -> Result<PathBuf, String> {}"
                            .to_string(),
                    location: "src/daemon.rs".to_string(),
                    location_lines: "590-620".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "check_caller_path",
                        "symbol_kind": "functions",
                        "valid_from": 1000,
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 2. Ungoverned hotspot symbol in src/legacy.rs with blast_radius_count = 6 and deferred_edges
        store
            .upsert(
                ns,
                "sym-legacy-hotspot",
                vec![0.1; 384],
                MemoryPayload {
                    content: "pub fn legacy_unwrap_helper() { check_caller_path(); }".to_string(),
                    location: "src/legacy.rs".to_string(),
                    location_lines: "10-40".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "legacy_unwrap_helper",
                        "symbol_kind": "functions",
                        "blast_radius_count": 6,
                        "calls": ["check_caller_path"],
                        "deferred_edges": [{ "target": "src/missing.rs" }],
                        "valid_from": 1000,
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 3. Rule governing src/daemon.rs introduced at t = 2000
        store
            .upsert(
                ns,
                "rule-j6-caller-path",
                vec![0.1; 384],
                MemoryPayload {
                    content: "Every caller-supplied path must pass check_caller_path.".to_string(),
                    location: "src/daemon.rs".to_string(),
                    location_lines: "590-620".to_string(),
                    memory_type: "rule".to_string(),
                    metadata: json!({
                        "governs": ["src/daemon.rs"],
                        "valid_from": 2000,
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 4. Snapshot returns all 4 built-in series with finite values
        let snap_req = InsightsRequest {
            namespace: ns.to_string(),
            action: "snapshot".to_string(),
            name: None,
            title: None,
            query_kind: None,
            query: None,
            path_prefix: None,
            project_root: None,
            from_ts: None,
            to_ts: None,
            backfill_points: None,
            principal: Some("admin".to_string()),
        };
        let snap = execute_insights(&store, &snap_req, Some("admin"))
            .await
            .expect("snapshot");
        assert_eq!(snap.series.len(), 4);

        let cov = snap
            .series
            .iter()
            .find(|s| s.name == "governance_coverage_pct")
            .unwrap();
        assert!((cov.current_value - 50.0).abs() < 1e-6);
        assert!(cov
            .top_offenders
            .iter()
            .any(|o| o.contains("src/legacy.rs")));

        let debt = snap
            .series
            .iter()
            .find(|s| s.name == "deferred_edge_debt")
            .unwrap();
        assert!((debt.current_value - 1.0).abs() < 1e-6);

        let hotspots = snap
            .series
            .iter()
            .find(|s| s.name == "ungoverned_hotspot_count")
            .unwrap();
        assert!((hotspots.current_value - 1.0).abs() < 1e-6);

        // 5. Define custom lexical insight on private registry_row
        let def_req = InsightsRequest {
            namespace: ns.to_string(),
            action: "define".to_string(),
            name: Some("caller-path-mentions".to_string()),
            title: Some("Caller Path Mentions".to_string()),
            query_kind: Some(InsightQueryKind::Lexical),
            query: Some("check_caller_path".to_string()),
            path_prefix: None,
            project_root: None,
            from_ts: None,
            to_ts: None,
            backfill_points: None,
            principal: Some("admin".to_string()),
        };
        let def_resp = execute_insights(&store, &def_req, Some("admin"))
            .await
            .expect("define custom insight");
        assert_eq!(def_resp.series.len(), 1);
        assert!(def_resp.series[0].current_value >= 2.0);

        // Verify registry row remains private so SynapticMesh never exports it
        let reg_row = store
            .get(ns, &registry_row_id(ns))
            .await
            .unwrap()
            .expect("registry row exists");
        assert_eq!(reg_row.1.metadata["eligibility"], json!("private"));
        assert!(reg_row.1.metadata["insight_definitions"].is_array());

        // 6. Bitemporal series backfill across [1000, 3000] shows governance_coverage_pct
        // rising from 0% (at t=1000, before rule-j6-caller-path at t=2000) to 50% (at t=3000).
        let series_req = InsightsRequest {
            namespace: ns.to_string(),
            action: "series".to_string(),
            name: Some("governance_coverage_pct".to_string()),
            title: None,
            query_kind: None,
            query: None,
            path_prefix: None,
            project_root: None,
            from_ts: Some(1000),
            to_ts: Some(3000),
            backfill_points: Some(3),
            principal: Some("admin".to_string()),
        };
        let series_resp = execute_insights(&store, &series_req, Some("admin"))
            .await
            .expect("bitemporal series");
        let pts = &series_resp.series[0].points;
        assert!(pts.len() >= 3);
        let p_1000 = pts.iter().find(|p| p.ts == 1000).expect("t=1000 point");
        let p_3000 = pts.iter().find(|p| p.ts == 3000).expect("t=3000 point");
        assert!((p_1000.value - 0.0).abs() < 1e-6);
        assert!((p_3000.value - 50.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn custom_insight_quotas_builtin_protection_and_ingest_sampling_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn VectorStore> =
            Arc::new(LadybugStore::new(dir.path().join("db"), 384).expect("store init"));
        let ns = "InsightsQuotaNs";
        store.init(ns).await.unwrap();

        // Built-in slug collision is rejected
        let builtin_err = save_custom_insight(
            &store,
            ns,
            InsightDefinition {
                name: "governance_coverage_pct".to_string(),
                title: "Override Builtin".to_string(),
                query_kind: InsightQueryKind::Lexical,
                query: "fn".to_string(),
                path_prefix: None,
                created_at: 100,
            },
        )
        .await
        .unwrap_err();
        assert!(builtin_err.contains("built-in insight"));

        // Empty query is rejected
        let empty_err = save_custom_insight(
            &store,
            ns,
            InsightDefinition {
                name: "empty-q".to_string(),
                title: "Empty".to_string(),
                query_kind: InsightQueryKind::Lexical,
                query: "   ".to_string(),
                path_prefix: None,
                created_at: 100,
            },
        )
        .await
        .unwrap_err();
        assert!(empty_err.contains("must not be empty"));

        // Register MAX_CUSTOM_INSIGHTS (16) custom insights
        for idx in 0..MAX_CUSTOM_INSIGHTS {
            save_custom_insight(
                &store,
                ns,
                InsightDefinition {
                    name: format!("custom-metric-{idx}"),
                    title: format!("Custom Metric {idx}"),
                    query_kind: InsightQueryKind::AstCalls,
                    query: "check_caller_path".to_string(),
                    path_prefix: Some("src/".to_string()),
                    created_at: 100 + (idx as i64),
                },
            )
            .await
            .expect("within quota");
        }

        // Updating an existing slug at quota is allowed
        save_custom_insight(
            &store,
            ns,
            InsightDefinition {
                name: "custom-metric-0".to_string(),
                title: "Updated Custom Metric 0".to_string(),
                query_kind: InsightQueryKind::AstCalls,
                query: "check_caller_path".to_string(),
                path_prefix: Some("src/".to_string()),
                created_at: 200,
            },
        )
        .await
        .expect("updating existing slug allowed at quota");

        // 17th distinct custom insight is rejected
        let quota_err = save_custom_insight(
            &store,
            ns,
            InsightDefinition {
                name: "custom-metric-overflow".to_string(),
                title: "Overflow".to_string(),
                query_kind: InsightQueryKind::Lexical,
                query: "overflow".to_string(),
                path_prefix: None,
                created_at: 300,
            },
        )
        .await
        .unwrap_err();
        assert!(quota_err.contains("maximum of 16 custom insights"));

        // Removing one frees a slot and sample_insights_after_ingest records generation label
        assert!(remove_custom_insight(&store, ns, "custom-metric-15")
            .await
            .unwrap());
        sample_insights_after_ingest(&store, ns, Some(7)).await;

        let series_req = InsightsRequest {
            namespace: ns.to_string(),
            action: "series".to_string(),
            name: Some("custom-metric-0".to_string()),
            title: None,
            query_kind: None,
            query: None,
            path_prefix: None,
            project_root: None,
            from_ts: None,
            to_ts: None,
            backfill_points: None,
            principal: Some("admin".to_string()),
        };
        let series_resp = execute_insights(&store, &series_req, Some("admin"))
            .await
            .expect("series after ingest sample");
        assert_eq!(series_resp.series.len(), 1);
        assert!(series_resp.series[0]
            .points
            .iter()
            .any(|p| p.generation == Some(7)));
    }

    /// Custom-insight writers read, change and upsert the registry row, so
    /// they wait for the row lock every other registry-row writer takes; a
    /// cached-root write in between would otherwise be reverted.
    #[tokio::test]
    async fn custom_insight_writers_wait_for_the_registry_row_lock() {
        let store: Arc<dyn VectorStore> = Arc::new(crate::guard::testkit::MockStore::new());
        let ns = "example-project";
        let def = InsightDefinition {
            name: "lock-probe".to_string(),
            title: "Lock probe".to_string(),
            query_kind: InsightQueryKind::Lexical,
            query: "fn".to_string(),
            path_prefix: None,
            created_at: 100,
        };
        let held = crate::registry::REGISTRY_ROW_LOCK.lock().await;
        let save = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            save_custom_insight(&store, ns, def.clone()),
        )
        .await;
        assert!(save.is_err(), "save_custom_insight ran while the row lock was held");
        let remove = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            remove_custom_insight(&store, ns, "lock-probe"),
        )
        .await;
        assert!(remove.is_err(), "remove_custom_insight ran while the row lock was held");
        drop(held);
        save_custom_insight(&store, ns, def).await.expect("saves once the lock is free");
        assert_eq!(remove_custom_insight(&store, ns, "lock-probe").await, Ok(true));
    }
}
