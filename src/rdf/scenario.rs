use crate::digital_twin::{TwinAttrValue, TwinDelta, TwinWarning};
use crate::formal_kernel::{
    evaluate_scenario_commit, resolve_scenario_row, ScenarioCommitDecision, ScenarioRowVisibility,
};
use crate::rdf::action::{
    is_active_payload, render_template_string, render_template_value, ActionEffectTemplate,
    ActionError, ActionSecurityContext, ActionSubmissionFailure, ActionTypeDefinition,
    SubmissionCriterion,
};
use crate::rdf::shacl::ShaclViolation;
use crate::traits::MemoryPayload;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const MAX_SCENARIOS_PER_NAMESPACE: usize = 16;
pub const MAX_STAGED_ROWS_PER_SCENARIO: usize = 256;
pub const DEFAULT_SCENARIO_TTL_SECS: i64 = 1800;
const ABSENT_FINGERPRINT: &str = "__ABSENT__";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedEdge {
    pub source: String,
    pub relation: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioState {
    pub scenario_id: String,
    pub namespace: String,
    pub title: String,
    pub owner_principal: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub base_fingerprints: BTreeMap<String, String>,
    pub staged_upserts: BTreeMap<String, MemoryPayload>,
    pub staged_retirements: BTreeSet<String>,
    pub staged_edges: Vec<StagedEdge>,
    pub applied_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub scenario_id: String,
    pub namespace: String,
    pub title: String,
    pub owner_principal: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub staged_upsert_count: usize,
    pub staged_retirement_count: usize,
    pub staged_edge_count: usize,
    pub applied_actions: Vec<String>,
}

impl From<&ScenarioState> for ScenarioSummary {
    fn from(s: &ScenarioState) -> Self {
        Self {
            scenario_id: s.scenario_id.clone(),
            namespace: s.namespace.clone(),
            title: s.title.clone(),
            owner_principal: s.owner_principal.clone(),
            created_at: s.created_at,
            expires_at: s.expires_at,
            staged_upsert_count: s.staged_upserts.len(),
            staged_retirement_count: s.staged_retirements.len(),
            staged_edge_count: s.staged_edges.len(),
            applied_actions: s.applied_actions.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDiffReport {
    pub scenario_id: String,
    pub compared_against: String,
    pub added_ids: Vec<String>,
    pub retired_ids: Vec<String>,
    pub twin_deltas: BTreeMap<String, Vec<TwinDelta>>,
    pub added_edges: Vec<StagedEdge>,
    pub shacl_violations: Vec<ShaclViolation>,
    pub twin_warnings: BTreeMap<String, Vec<TwinWarning>>,
    pub base_conflicts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStageOutcome {
    pub scenario_id: String,
    pub staged_row_id: String,
    #[serde(default)]
    pub retired_row_ids: Vec<String>,
    #[serde(default)]
    pub twin_deltas: Vec<TwinDelta>,
    #[serde(default)]
    pub shacl_violations: Vec<ShaclViolation>,
    #[serde(default)]
    pub twin_warnings: Vec<TwinWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSparqlOutcome {
    pub scenario_id: String,
    pub namespace: String,
    pub format: String,
    pub results: String,
    pub solution_count: usize,
    pub truncated: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ScenarioCommitOutcome {
    Committed {
        scenario_id: String,
        committed_upserts: Vec<String>,
        committed_retirements: Vec<String>,
        committed_edges: usize,
    },
    Rejected {
        scenario_id: String,
        decision: String,
        base_conflicts: Vec<String>,
        shacl_violations: Vec<ShaclViolation>,
        message: String,
    },
}

#[derive(Debug)]
pub struct ScenarioRegistry {
    by_namespace: arc_swap::ArcSwap<BTreeMap<String, BTreeMap<String, Arc<ScenarioState>>>>,
    write_guard: std::sync::Mutex<()>,
}

impl Default for ScenarioRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ScenarioRegistry {
    pub fn new() -> Self {
        Self {
            by_namespace: arc_swap::ArcSwap::from_pointee(BTreeMap::new()),
            write_guard: std::sync::Mutex::new(()),
        }
    }

    pub fn evict_expired(&self, namespace: &str, now_ts: i64) {
        let snap = self.by_namespace.load();
        let needs_eviction = snap
            .get(namespace)
            .map(|ns_map| ns_map.values().any(|s| now_ts >= s.expires_at))
            .unwrap_or(false);
        if !needs_eviction {
            return;
        }
        let _guard = self.write_guard.lock().unwrap_or_else(|e| e.into_inner());
        let cur = self.by_namespace.load();
        let mut next = (**cur).clone();
        if let Some(ns_map) = next.get_mut(namespace) {
            ns_map.retain(|_, s| now_ts < s.expires_at);
        }
        self.by_namespace.store(Arc::new(next));
    }

    pub fn get(
        &self,
        namespace: &str,
        scenario_id: &str,
        now_ts: i64,
    ) -> Result<Arc<ScenarioState>, String> {
        self.evict_expired(namespace, now_ts);
        let snap = self.by_namespace.load();
        snap.get(namespace)
            .and_then(|m| m.get(scenario_id))
            .cloned()
            .ok_or_else(|| {
                format!("scenario '{scenario_id}' not found or expired in namespace '{namespace}'")
            })
    }

    pub fn list(&self, namespace: &str, now_ts: i64) -> Vec<ScenarioSummary> {
        self.evict_expired(namespace, now_ts);
        let snap = self.by_namespace.load();
        snap.get(namespace)
            .map(|m| m.values().map(|s| ScenarioSummary::from(&**s)).collect())
            .unwrap_or_default()
    }

    pub fn insert_new(&self, state: ScenarioState, now_ts: i64) -> Result<ScenarioSummary, String> {
        self.evict_expired(&state.namespace, now_ts);
        let _guard = self.write_guard.lock().unwrap_or_else(|e| e.into_inner());
        let cur = self.by_namespace.load();
        let mut next = (**cur).clone();
        let ns_map = next.entry(state.namespace.clone()).or_default();
        if ns_map.contains_key(&state.scenario_id) {
            return Err(format!(
                "active scenario '{}' already exists in namespace '{}'",
                state.scenario_id, state.namespace
            ));
        }
        if ns_map.len() >= MAX_SCENARIOS_PER_NAMESPACE {
            return Err(format!(
                "namespace '{}' reached maximum active scenarios ({MAX_SCENARIOS_PER_NAMESPACE})",
                state.namespace
            ));
        }
        let summary = ScenarioSummary::from(&state);
        ns_map.insert(state.scenario_id.clone(), Arc::new(state));
        self.by_namespace.store(Arc::new(next));
        Ok(summary)
    }

    pub fn update<F, R>(
        &self,
        namespace: &str,
        scenario_id: &str,
        now_ts: i64,
        f: F,
    ) -> Result<R, String>
    where
        F: FnOnce(&mut ScenarioState) -> Result<R, String>,
    {
        self.evict_expired(namespace, now_ts);
        let _guard = self.write_guard.lock().unwrap_or_else(|e| e.into_inner());
        let cur = self.by_namespace.load();
        let mut next = (**cur).clone();
        let ns_map = next.get_mut(namespace).ok_or_else(|| {
            format!("scenario '{scenario_id}' not found or expired in namespace '{namespace}'")
        })?;
        let arc_state = ns_map.get_mut(scenario_id).ok_or_else(|| {
            format!("scenario '{scenario_id}' not found or expired in namespace '{namespace}'")
        })?;
        let mut cloned = (**arc_state).clone();
        let res = f(&mut cloned)?;
        *arc_state = Arc::new(cloned);
        self.by_namespace.store(Arc::new(next));
        Ok(res)
    }

    pub fn remove(&self, namespace: &str, scenario_id: &str) -> bool {
        let _guard = self.write_guard.lock().unwrap_or_else(|e| e.into_inner());
        let cur = self.by_namespace.load();
        let mut next = (**cur).clone();
        let removed = next
            .get_mut(namespace)
            .and_then(|m| m.remove(scenario_id))
            .is_some();
        if removed {
            self.by_namespace.store(Arc::new(next));
        }
        removed
    }
}

fn canonicalize_json_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut sorted: BTreeMap<String, serde_json::Value> = BTreeMap::new();
            for (k, val) in map {
                sorted.insert(k.clone(), canonicalize_json_value(val));
            }
            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (k, val) in sorted {
                out.insert(k, val);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(canonicalize_json_value).collect())
        }
        other => other.clone(),
    }
}

pub fn sha256_fingerprint(payload: &MemoryPayload) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload.memory_type.as_bytes());
    hasher.update(b"\0");
    hasher.update(payload.user_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(payload.content.as_bytes());
    hasher.update(b"\0");
    hasher.update(payload.location.as_bytes());
    hasher.update(b"\0");
    hasher.update(payload.location_lines.as_bytes());
    hasher.update(b"\0");
    let canonical_meta = canonicalize_json_value(&payload.metadata);
    let meta_str = serde_json::to_string(&canonical_meta).unwrap_or_default();
    hasher.update(meta_str.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fingerprint_digest_u64(fingerprint: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// Row-level content fingerprint for the transactional
/// `ExpectedHead::RowFingerprint` CAS arm. Absent (or retired) rows collapse to
/// the sentinel fingerprint so staged scenarios can expect either state.
pub(crate) fn fingerprint_live_row(live: Option<&MemoryPayload>, now_ts: i64) -> u64 {
    let fingerprint = match live {
        Some(p) if is_active_payload(p, now_ts) => sha256_fingerprint(p),
        _ => ABSENT_FINGERPRINT.to_string(),
    };
    fingerprint_digest_u64(&fingerprint)
}

pub async fn current_live_fingerprint(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    row_id: &str,
    now_ts: i64,
) -> Result<String, String> {
    use crate::traits::VectorStore;
    match store
        .get(namespace, row_id)
        .await
        .map_err(|e| format!("failed to read live row '{row_id}': {e}"))?
    {
        Some((_vec, payload)) if is_active_payload(&payload, now_ts) => {
            Ok(sha256_fingerprint(&payload))
        }
        _ => Ok(ABSENT_FINGERPRINT.to_string()),
    }
}

pub fn extract_twin_id_for_row(row_id: &str, payload: &MemoryPayload) -> String {
    payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("id"))
        .or_else(|| {
            payload
                .metadata
                .get("digital_twin")
                .and_then(|t| t.get("twin_id").or_else(|| t.get("id")))
        })
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(row_id)
        .to_string()
}

pub fn extract_twin_variables_map(payload: &MemoryPayload) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let vars_val = payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("variables"))
        .or_else(|| {
            payload
                .metadata
                .get("digital_twin")
                .and_then(|t| t.get("variables").or_else(|| t.get("properties")))
        });
    if let Some(obj) = vars_val.and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(f) = v.as_f64().filter(|f| f.is_finite()) {
                out.insert(k.clone(), f);
            }
        }
    }
    out
}

/// Merge live namespace rows with a `ScenarioState` using the Lean-verified
/// `resolve_scenario_row` Copy-on-Write visibility kernel.
pub async fn collect_merged_scenario_rows_and_edges(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario: &ScenarioState,
    now_ts: i64,
) -> Result<
    (
        Vec<(String, MemoryPayload)>,
        Vec<(String, String, String, f64)>,
    ),
    String,
> {
    use crate::traits::VectorStore;
    let live_records = store
        .list(namespace, None, None)
        .await
        .map_err(|e| format!("failed to list memories for scenario projection: {e}"))?;

    let mut live_by_id: BTreeMap<String, MemoryPayload> = BTreeMap::new();
    for rec in live_records {
        if is_active_payload(&rec.payload, now_ts) {
            live_by_id.insert(rec.id, rec.payload);
        }
    }

    let mut all_candidate_ids: BTreeSet<String> = live_by_id.keys().cloned().collect();
    for id in scenario.staged_upserts.keys() {
        all_candidate_ids.insert(id.clone());
    }
    for id in &scenario.staged_retirements {
        all_candidate_ids.insert(id.clone());
    }

    let mut merged_rows: Vec<(String, MemoryPayload)> = Vec::new();
    let mut visible_ids: BTreeSet<String> = BTreeSet::new();
    for id in all_candidate_ids {
        let live_present = live_by_id.contains_key(&id);
        let staged_retired = scenario.staged_retirements.contains(&id);
        let staged_upsert = scenario.staged_upserts.contains_key(&id);
        match resolve_scenario_row(live_present, staged_retired, staged_upsert) {
            ScenarioRowVisibility::VisibleStaged => {
                if let Some(payload) = scenario.staged_upserts.get(&id) {
                    if is_active_payload(payload, now_ts) {
                        visible_ids.insert(id.clone());
                        merged_rows.push((id, payload.clone()));
                    }
                }
            }
            ScenarioRowVisibility::VisibleLive => {
                if let Some(payload) = live_by_id.get(&id) {
                    visible_ids.insert(id.clone());
                    merged_rows.push((id, payload.clone()));
                }
            }
            ScenarioRowVisibility::HiddenRetired | ScenarioRowVisibility::Absent => {}
        }
    }

    let mut merged_edges: Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)> = store
        .list_edges(namespace)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(s, _r, t, _prov)| {
            !scenario.staged_retirements.contains(s) && !scenario.staged_retirements.contains(t)
        })
        .collect();

    for edge in &scenario.staged_edges {
        let exists = merged_edges
            .iter()
            .any(|(s, r, t, _)| s == &edge.source && r == &edge.relation && t == &edge.target);
        if !exists {
            merged_edges.push((
                edge.source.clone(),
                edge.relation.clone(),
                edge.target.clone(),
                None,
            ));
        }
    }

    let weighted_edges: Vec<(String, String, String, f64)> = merged_edges
        .into_iter()
        .map(|(s, r, t, _)| (s, r, t, 1.0))
        .collect();

    Ok((merged_rows, weighted_edges))
}

fn evaluate_staged_warnings(
    scenario: &ScenarioState,
    merged_rows: &[(String, MemoryPayload)],
    merged_edges: &[(String, String, String, f64)],
) -> (Vec<ShaclViolation>, BTreeMap<String, Vec<TwinWarning>>) {
    let mut shacl_violations = Vec::new();
    let mut twin_warnings: BTreeMap<String, Vec<TwinWarning>> = BTreeMap::new();
    let twin_schemas = crate::digital_twin::collect_twin_schemas_from_rows(merged_rows);
    let prov_edges: Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)> =
        merged_edges
            .iter()
            .map(|(s, r, t, _w)| (s.clone(), r.clone(), t.clone(), None))
            .collect();

    for (row_id, payload) in &scenario.staged_upserts {
        let v = crate::rdf::shacl::validate_memory(payload);
        for item in v {
            shacl_violations.push(ShaclViolation {
                field: format!("{row_id}:{}", item.field),
                why: item.why,
            });
        }

        if payload.memory_type == "digital_twin" {
            let schema_id = payload
                .metadata
                .get("twin")
                .and_then(|t| t.get("schema_id"))
                .or_else(|| {
                    payload
                        .metadata
                        .get("digital_twin")
                        .and_then(|t| t.get("schema_id"))
                })
                .or_else(|| payload.metadata.get("schema_id"))
                .and_then(|v| v.as_str());
            if let Some(sid) = schema_id {
                if let Some(schema) = twin_schemas.get(sid) {
                    let eff =
                        crate::digital_twin::resolve_effective_twin_schema(schema, &twin_schemas);
                    let vars = extract_twin_variables_map(payload);
                    let mut warnings = crate::digital_twin::validate_state(&vars, Some(&eff));
                    let attrs: BTreeMap<String, TwinAttrValue> = payload
                        .metadata
                        .get("twin")
                        .and_then(|t| t.get("attributes"))
                        .or_else(|| {
                            payload
                                .metadata
                                .get("digital_twin")
                                .and_then(|t| t.get("attributes"))
                        })
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default();
                    warnings.extend(crate::digital_twin::validate_attributes(&attrs, Some(&eff)));
                    let twin_id = extract_twin_id_for_row(row_id, payload);
                    warnings.extend(crate::digital_twin::validate_link_constraints(
                        row_id,
                        &twin_id,
                        payload,
                        &eff,
                        merged_rows,
                        &prov_edges,
                        &twin_schemas,
                    ));
                    if !warnings.is_empty() {
                        twin_warnings.entry(twin_id).or_default().extend(warnings);
                    }
                }
            }
        }
    }

    (shacl_violations, twin_warnings)
}

pub fn create_scenario(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: Option<String>,
    title: Option<String>,
    ttl_secs: Option<i64>,
    security: &ActionSecurityContext,
) -> Result<ScenarioSummary, String> {
    if namespace.trim().is_empty() {
        return Err("namespace must be non-empty".to_string());
    }
    let now_ts = chrono::Utc::now().timestamp();
    let scen_id = scenario_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let raw = uuid::Uuid::new_v4().simple().to_string();
            format!("scen-{}", &raw[..12])
        });
    let scen_title = title
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| scen_id.clone());
    let ttl = ttl_secs
        .unwrap_or(DEFAULT_SCENARIO_TTL_SECS)
        .clamp(1, 86_400);
    let owner = security.effective_principal();

    let state = ScenarioState {
        scenario_id: scen_id,
        namespace: namespace.to_string(),
        title: scen_title,
        owner_principal: owner,
        created_at: now_ts,
        expires_at: now_ts.saturating_add(ttl),
        base_fingerprints: BTreeMap::new(),
        staged_upserts: BTreeMap::new(),
        staged_retirements: BTreeSet::new(),
        staged_edges: Vec::new(),
        applied_actions: Vec::new(),
    };

    store.scenarios().insert_new(state, now_ts)
}

pub async fn stage_scenario_memory(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: &str,
    memory_id: Option<String>,
    memory_type: &str,
    content: &str,
    metadata: serde_json::Value,
    retire_id: Option<String>,
    edges: Vec<StagedEdge>,
    security: &ActionSecurityContext,
) -> Result<ScenarioStageOutcome, String> {
    let now_ts = chrono::Utc::now().timestamp();
    let target_id = memory_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let raw = uuid::Uuid::new_v4().simple().to_string();
            format!("scen-mem-{}", &raw[..12])
        });

    let current_state = store.scenarios().get(namespace, scenario_id, now_ts)?;
    if !current_state.staged_upserts.contains_key(&target_id)
        && current_state.staged_upserts.len() >= MAX_STAGED_ROWS_PER_SCENARIO
    {
        return Err(format!(
            "scenario '{scenario_id}' reached maximum staged rows ({MAX_STAGED_ROWS_PER_SCENARIO})"
        ));
    }

    let mut new_fps: BTreeMap<String, String> = BTreeMap::new();
    if !current_state.base_fingerprints.contains_key(&target_id) {
        let fp = current_live_fingerprint(store, namespace, &target_id, now_ts).await?;
        new_fps.insert(target_id.clone(), fp);
    }
    if let Some(ref rid) = retire_id {
        if !current_state.base_fingerprints.contains_key(rid) {
            let fp = current_live_fingerprint(store, namespace, rid, now_ts).await?;
            new_fps.insert(rid.clone(), fp);
        }
    }

    let mut meta_obj = if metadata.is_object() {
        metadata
    } else {
        serde_json::json!({})
    };
    if let Some(obj) = meta_obj.as_object_mut() {
        obj.entry("valid_from")
            .or_insert_with(|| serde_json::json!(now_ts));
        if let Some(ref rid) = retire_id {
            obj.insert("supersedes".to_string(), serde_json::json!(rid));
        }
    }

    let payload = MemoryPayload {
        content: content.to_string(),
        user_id: security.effective_principal(),
        memory_type: memory_type.to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: meta_obj,
    };

    let mut retired_row_ids = Vec::new();
    let updated_state = store
        .scenarios()
        .update(namespace, scenario_id, now_ts, |scen| {
            for (k, v) in new_fps {
                scen.base_fingerprints.entry(k).or_insert(v);
            }
            if let Some(ref rid) = retire_id {
                scen.staged_retirements.insert(rid.clone());
                if rid != &target_id {
                    scen.staged_upserts.remove(rid);
                }
                retired_row_ids.push(rid.clone());
            }
            scen.staged_upserts.insert(target_id.clone(), payload);
            for edge in edges {
                if !scen.staged_edges.contains(&edge) {
                    scen.staged_edges.push(edge);
                }
            }
            Ok(scen.clone())
        })?;

    let (merged_rows, merged_edges) =
        collect_merged_scenario_rows_and_edges(store, namespace, &updated_state, now_ts).await?;
    let (shacl_violations, twin_warnings_map) =
        evaluate_staged_warnings(&updated_state, &merged_rows, &merged_edges);
    let twin_warnings = twin_warnings_map.into_values().flatten().collect();

    Ok(ScenarioStageOutcome {
        scenario_id: scenario_id.to_string(),
        staged_row_id: target_id,
        retired_row_ids,
        twin_deltas: Vec::new(),
        shacl_violations,
        twin_warnings,
    })
}

async fn resolve_scenario_or_live_twin_row(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario: &ScenarioState,
    twin_id: &str,
    now_ts: i64,
) -> Result<Option<(String, MemoryPayload, bool)>, String> {
    // 1. Check staged_upserts first
    if let Some(p) = scenario.staged_upserts.get(twin_id) {
        return Ok(Some((twin_id.to_string(), p.clone(), false)));
    }
    for (row_id, p) in &scenario.staged_upserts {
        if p.memory_type == "digital_twin" && extract_twin_id_for_row(row_id, p) == twin_id {
            return Ok(Some((row_id.clone(), p.clone(), false)));
        }
    }

    // 2. Check live store (unless staged_retirements masks it)
    use crate::traits::VectorStore;
    if !scenario.staged_retirements.contains(twin_id) {
        if let Some((_vec, p)) = store
            .get(namespace, twin_id)
            .await
            .map_err(|e| format!("failed to read twin '{twin_id}': {e}"))?
        {
            if is_active_payload(&p, now_ts) {
                return Ok(Some((twin_id.to_string(), p, true)));
            }
        }
    }

    let rows = store
        .list(namespace, None, Some("digital_twin"))
        .await
        .map_err(|e| format!("failed to list twins in '{namespace}': {e}"))?;
    let mut best: Option<(u64, String, MemoryPayload)> = None;
    for r in rows {
        if scenario.staged_retirements.contains(&r.id) || !is_active_payload(&r.payload, now_ts) {
            continue;
        }
        if extract_twin_id_for_row(&r.id, &r.payload) == twin_id {
            let ver = r
                .payload
                .metadata
                .get("twin")
                .and_then(|t| t.get("version"))
                .or_else(|| {
                    r.payload
                        .metadata
                        .get("digital_twin")
                        .and_then(|t| t.get("version"))
                })
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let replace = match &best {
                None => true,
                Some((best_ver, best_id, _)) => {
                    ver > *best_ver || (ver == *best_ver && r.id > *best_id)
                }
            };
            if replace {
                best = Some((ver, r.id, r.payload));
            }
        }
    }
    Ok(best.map(|(_ver, id, payload)| (id, payload, true)))
}

pub async fn stage_scenario_twin(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: &str,
    twin_id: &str,
    variables: BTreeMap<String, f64>,
    attributes: Option<BTreeMap<String, TwinAttrValue>>,
    merge: bool,
    schema_id: Option<String>,
    source: Option<String>,
    security: &ActionSecurityContext,
) -> Result<ScenarioStageOutcome, String> {
    if twin_id.trim().is_empty() {
        return Err("twin_id must be non-empty".to_string());
    }
    for (k, v) in &variables {
        if !v.is_finite() {
            return Err(format!("non-finite variable '{k}': {v}"));
        }
    }

    let now_ts = chrono::Utc::now().timestamp();
    let current_state = store.scenarios().get(namespace, scenario_id, now_ts)?;

    let existing =
        resolve_scenario_or_live_twin_row(store, namespace, &current_state, twin_id, now_ts)
            .await?;

    let (target_row_id, base_payload, from_live) = match existing {
        Some((row_id, payload, from_live)) => (row_id, payload, from_live),
        None => {
            let init_meta = serde_json::json!({
                "twin": {
                    "id": twin_id,
                    "version": 0,
                    "variables": {},
                    "last_updated": now_ts,
                },
                "digital_twin": {
                    "twin_id": twin_id,
                    "version": 0,
                    "variables": {},
                    "properties": {},
                    "updated_at": now_ts,
                }
            });
            let init_payload = MemoryPayload {
                content: format!("Digital twin {twin_id}"),
                user_id: security.effective_principal(),
                memory_type: "digital_twin".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: init_meta,
            };
            (twin_id.to_string(), init_payload, false)
        }
    };

    if !current_state.staged_upserts.contains_key(&target_row_id)
        && current_state.staged_upserts.len() >= MAX_STAGED_ROWS_PER_SCENARIO
    {
        return Err(format!(
            "scenario '{scenario_id}' reached maximum staged rows ({MAX_STAGED_ROWS_PER_SCENARIO})"
        ));
    }

    let live_fp = if !current_state.base_fingerprints.contains_key(&target_row_id) {
        if from_live {
            Some(sha256_fingerprint(&base_payload))
        } else {
            Some(current_live_fingerprint(store, namespace, &target_row_id, now_ts).await?)
        }
    } else {
        None
    };

    let (_resolved_twin_id, _before_ver, _after_ver, mut next_payload, deltas) =
        crate::digital_twin::stage_twin_mutation(
            &target_row_id,
            &base_payload,
            &variables,
            merge,
            source.as_deref().or(Some("scenario")),
            now_ts,
        );

    if let Some(ref sid) = schema_id {
        if let Some(meta_obj) = next_payload.metadata.as_object_mut() {
            if let Some(t) = meta_obj.get_mut("twin").and_then(|v| v.as_object_mut()) {
                t.insert("schema_id".to_string(), serde_json::json!(sid));
            }
            if let Some(dt) = meta_obj
                .get_mut("digital_twin")
                .and_then(|v| v.as_object_mut())
            {
                dt.insert("schema_id".to_string(), serde_json::json!(sid));
            }
        }
    }
    if let Some(ref attrs) = attributes {
        let attrs_json = serde_json::to_value(attrs).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(meta_obj) = next_payload.metadata.as_object_mut() {
            if let Some(t) = meta_obj.get_mut("twin").and_then(|v| v.as_object_mut()) {
                t.insert("attributes".to_string(), attrs_json.clone());
            }
            if let Some(dt) = meta_obj
                .get_mut("digital_twin")
                .and_then(|v| v.as_object_mut())
            {
                dt.insert("attributes".to_string(), attrs_json);
            }
        }
    }

    let updated_state = store
        .scenarios()
        .update(namespace, scenario_id, now_ts, |scen| {
            if let Some(fp) = live_fp {
                scen.base_fingerprints
                    .entry(target_row_id.clone())
                    .or_insert(fp);
            }
            scen.staged_upserts
                .insert(target_row_id.clone(), next_payload);
            Ok(scen.clone())
        })?;

    let (merged_rows, merged_edges) =
        collect_merged_scenario_rows_and_edges(store, namespace, &updated_state, now_ts).await?;
    let (shacl_violations, mut twin_warnings_map) =
        evaluate_staged_warnings(&updated_state, &merged_rows, &merged_edges);
    let twin_warnings = twin_warnings_map
        .remove(twin_id)
        .or_else(|| twin_warnings_map.remove(&target_row_id))
        .unwrap_or_default();

    Ok(ScenarioStageOutcome {
        scenario_id: scenario_id.to_string(),
        staged_row_id: target_row_id,
        retired_row_ids: Vec::new(),
        twin_deltas: deltas,
        shacl_violations,
        twin_warnings,
    })
}

async fn resolve_scenario_or_live_row(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario: &ScenarioState,
    raw_id: &str,
    is_twin_lookup: bool,
    now_ts: i64,
) -> Result<Option<(String, MemoryPayload, bool)>, String> {
    if is_twin_lookup {
        if let Some(found) =
            resolve_scenario_or_live_twin_row(store, namespace, scenario, raw_id, now_ts).await?
        {
            return Ok(Some(found));
        }
    }
    if let Some(p) = scenario.staged_upserts.get(raw_id) {
        return Ok(Some((raw_id.to_string(), p.clone(), false)));
    }
    if scenario.staged_retirements.contains(raw_id) {
        return Ok(None);
    }
    use crate::traits::VectorStore;
    if let Some((_vec, p)) = store
        .get(namespace, raw_id)
        .await
        .map_err(|e| format!("failed to read row '{raw_id}': {e}"))?
    {
        if is_active_payload(&p, now_ts) {
            return Ok(Some((raw_id.to_string(), p, true)));
        }
    }
    Ok(None)
}

/// Apply a kinetic `ActionType` inside a scenario overlay without mutating live LadybugDB.
pub async fn apply_action_in_scenario(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: &str,
    action_slug: &str,
    params: BTreeMap<String, serde_json::Value>,
    security: &ActionSecurityContext,
) -> Result<ScenarioStageOutcome, ActionError> {
    let now_ts = chrono::Utc::now().timestamp();
    let scenario = store
        .scenarios()
        .get(namespace, scenario_id, now_ts)
        .map_err(ActionError::Storage)?;

    let mut ont = store.get_ontology(namespace);
    if !ont.action_types.contains_key(action_slug) {
        store.refresh_ontology(namespace).await;
        ont = store.get_ontology(namespace);
    }
    let definition: ActionTypeDefinition = if let Some(def) = ont.action_types.get(action_slug) {
        def.clone()
    } else {
        // Also check staged action_type rows inside the scenario
        let staged_def = scenario
            .staged_upserts
            .values()
            .filter(|p| p.memory_type == "action_type")
            .find_map(|p| {
                ActionTypeDefinition::from_payload(p)
                    .ok()
                    .filter(|d| d.slug == action_slug)
            });
        staged_def.ok_or_else(|| ActionError::UnknownActionType {
            namespace: namespace.to_string(),
            slug: action_slug.to_string(),
        })?
    };

    let def_violations = definition.validate_definition();
    if !def_violations.is_empty() {
        return Err(ActionError::InvalidDefinition(def_violations));
    }

    let mut submission_failures = Vec::new();
    let effective_principal = security.effective_principal();
    if effective_principal.trim().is_empty() {
        submission_failures.push(ActionSubmissionFailure {
            code: "unauthorized_principal".to_string(),
            target: "principal".to_string(),
            message: "authenticated principal is required".to_string(),
        });
    }

    for key in params.keys() {
        if !definition.parameters.contains_key(key) {
            submission_failures.push(ActionSubmissionFailure {
                code: "unknown_parameter".to_string(),
                target: key.clone(),
                message: format!("unknown parameter '{key}'"),
            });
        }
    }

    let mut bound_params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut rdf_bindings: Vec<(oxrdf::Variable, oxrdf::Term)> = Vec::new();
    let mut targets: BTreeMap<String, (String, MemoryPayload)> = BTreeMap::new();
    let mut touched_live_fps: BTreeMap<String, String> = BTreeMap::new();

    for (name, spec) in &definition.parameters {
        let Some(raw_val) = params.get(name) else {
            if spec.required {
                submission_failures.push(ActionSubmissionFailure {
                    code: "missing_parameter".to_string(),
                    target: name.clone(),
                    message: format!("missing required parameter '{name}'"),
                });
            }
            continue;
        };
        if raw_val.is_null() {
            if spec.required {
                submission_failures.push(ActionSubmissionFailure {
                    code: "missing_parameter".to_string(),
                    target: name.clone(),
                    message: format!("missing required parameter '{name}'"),
                });
            }
            continue;
        }

        match spec.param_type.as_str() {
            "f64" => match raw_val.as_f64().filter(|v| v.is_finite()) {
                Some(f) => {
                    if spec.min.map_or(false, |lo| f < lo) || spec.max.map_or(false, |hi| f > hi) {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "out_of_bounds".to_string(),
                            target: name.clone(),
                            message: format!("parameter '{name}' is out of bounds"),
                        });
                        continue;
                    }
                    bound_params.insert(name.clone(), serde_json::json!(f));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((var, oxrdf::Term::Literal(oxrdf::Literal::from(f))));
                    }
                }
                None => submission_failures.push(ActionSubmissionFailure {
                    code: "invalid_parameter_type".to_string(),
                    target: name.clone(),
                    message: format!("parameter '{name}' must be a finite f64"),
                }),
            },
            "i64" => match raw_val.as_i64() {
                Some(i) => {
                    if spec.min.map_or(false, |lo| (i as f64) < lo)
                        || spec.max.map_or(false, |hi| (i as f64) > hi)
                    {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "out_of_bounds".to_string(),
                            target: name.clone(),
                            message: format!("parameter '{name}' is out of bounds"),
                        });
                        continue;
                    }
                    bound_params.insert(name.clone(), serde_json::json!(i));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((var, oxrdf::Term::Literal(oxrdf::Literal::from(i))));
                    }
                }
                None => submission_failures.push(ActionSubmissionFailure {
                    code: "invalid_parameter_type".to_string(),
                    target: name.clone(),
                    message: format!("parameter '{name}' must be an i64"),
                }),
            },
            "string" => match raw_val.as_str() {
                Some(s) => {
                    if let Some(allowed) = &spec.in_values {
                        if !allowed.iter().any(|a| a == s) {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "invalid_enum_value".to_string(),
                                target: name.clone(),
                                message: format!(
                                    "parameter '{name}' is not in allowed enum values"
                                ),
                            });
                            continue;
                        }
                    }
                    bound_params.insert(name.clone(), serde_json::Value::String(s.to_string()));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((
                            var,
                            oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(s.to_string())),
                        ));
                    }
                }
                None => submission_failures.push(ActionSubmissionFailure {
                    code: "invalid_parameter_type".to_string(),
                    target: name.clone(),
                    message: format!("parameter '{name}' must be a string"),
                }),
            },
            "bool" => match raw_val.as_bool() {
                Some(b) => {
                    bound_params.insert(name.clone(), serde_json::Value::Bool(b));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((var, oxrdf::Term::Literal(oxrdf::Literal::from(b))));
                    }
                }
                None => submission_failures.push(ActionSubmissionFailure {
                    code: "invalid_parameter_type".to_string(),
                    target: name.clone(),
                    message: format!("parameter '{name}' must be a bool"),
                }),
            },
            "memory_id" | "twin_id" => match raw_val.as_str().filter(|s| !s.trim().is_empty()) {
                Some(raw_id) => {
                    let is_twin_lookup = spec.param_type == "twin_id"
                        || spec.target_memory_type.as_deref() == Some("digital_twin");
                    let Some((row_id, row_payload, from_live)) = resolve_scenario_or_live_row(
                        store,
                        namespace,
                        &scenario,
                        raw_id,
                        is_twin_lookup,
                        now_ts,
                    )
                    .await
                    .map_err(ActionError::Storage)?
                    else {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: name.clone(),
                            message: format!("target '{raw_id}' not found in scenario or live"),
                        });
                        continue;
                    };

                    if from_live && !scenario.base_fingerprints.contains_key(&row_id) {
                        touched_live_fps.insert(row_id.clone(), sha256_fingerprint(&row_payload));
                    }

                    if !crate::rebac::can_execute_action_on_row(
                        &effective_principal,
                        &row_payload.user_id,
                        &definition.required_role,
                        &row_payload.metadata,
                    ) {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "rebac_denied".to_string(),
                            target: name.clone(),
                            message: format!(
                                "principal lacks required role '{}' for '{name}'",
                                definition.required_role
                            ),
                        });
                    }

                    bound_params
                        .insert(name.clone(), serde_json::Value::String(raw_id.to_string()));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        let iri = crate::rdf::namespace::memory_iri(&row_id, None);
                        if let Ok(nn) = oxrdf::NamedNode::new(iri) {
                            rdf_bindings.push((var, oxrdf::Term::NamedNode(nn)));
                        }
                    }
                    targets.insert(name.clone(), (row_id, row_payload));
                }
                None => submission_failures.push(ActionSubmissionFailure {
                    code: "invalid_parameter_type".to_string(),
                    target: name.clone(),
                    message: format!("parameter '{name}' must be a non-empty ID string"),
                }),
            },
            _ => submission_failures.push(ActionSubmissionFailure {
                code: "invalid_parameter_type".to_string(),
                target: name.clone(),
                message: format!("unsupported parameter type for '{name}'"),
            }),
        }
    }

    for (idx, criterion) in definition.submission_criteria.iter().enumerate() {
        let target_label = format!("submission_criteria[{idx}]");
        match criterion {
            SubmissionCriterion::SparqlAsk {
                query,
                failure_message,
            } => {
                match crate::rdf::sparql::execute_scenario_action_ask(
                    store,
                    namespace,
                    scenario_id,
                    query,
                    &rdf_bindings,
                    crate::rdf::sparql::SparqlConfig::default(),
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => submission_failures.push(ActionSubmissionFailure {
                        code: "sparql_ask_failed".to_string(),
                        target: target_label,
                        message: failure_message.clone(),
                    }),
                    Err(e) => submission_failures.push(ActionSubmissionFailure {
                        code: "sparql_ask_error".to_string(),
                        target: target_label,
                        message: e,
                    }),
                }
            }
            SubmissionCriterion::TwinVarPrecondition {
                twin_param,
                variable,
                min,
                max,
            } => {
                let Some((_id, payload)) = targets.get(twin_param) else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "twin_precondition_failed".to_string(),
                        target: target_label,
                        message: format!("twin parameter '{twin_param}' was not resolved"),
                    });
                    continue;
                };
                let vars = extract_twin_variables_map(payload);
                let Some(&val) = vars.get(variable) else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "twin_precondition_failed".to_string(),
                        target: target_label,
                        message: format!("twin variable '{variable}' is missing"),
                    });
                    continue;
                };
                if min.map_or(false, |lo| val < lo) || max.map_or(false, |hi| val > hi) {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "twin_precondition_failed".to_string(),
                        target: target_label,
                        message: format!(
                            "twin variable '{variable}' is out of precondition bounds"
                        ),
                    });
                }
            }
        }
    }

    if !submission_failures.is_empty() {
        return Err(ActionError::SubmissionRejected(submission_failures));
    }

    let mut staged_upserts_patch: BTreeMap<String, MemoryPayload> = BTreeMap::new();
    let mut staged_retirements_patch: BTreeSet<String> = BTreeSet::new();
    let mut staged_edges_patch: Vec<StagedEdge> = Vec::new();
    let mut all_twin_deltas: Vec<TwinDelta> = Vec::new();
    let mut last_staged_id = String::new();

    for (effect_idx, effect) in definition.effects.iter().enumerate() {
        let effect_target = format!("effects[{effect_idx}]");
        match effect {
            ActionEffectTemplate::SetTwinState {
                twin_param,
                merge,
                variables_from_params,
            } => {
                let Some((row_id, before_payload)) = targets.get(twin_param).cloned() else {
                    return Err(ActionError::SubmissionRejected(vec![
                        ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: effect_target,
                            message: format!("unresolved twin parameter '{twin_param}'"),
                        },
                    ]));
                };
                let mut new_vars = BTreeMap::new();
                for (var_name, param_name) in variables_from_params {
                    let f = bound_params
                        .get(param_name)
                        .and_then(|v| v.as_f64())
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| {
                            ActionError::SubmissionRejected(vec![ActionSubmissionFailure {
                                code: "non_finite_number".to_string(),
                                target: effect_target.clone(),
                                message: format!("parameter '{param_name}' must be finite f64"),
                            }])
                        })?;
                    new_vars.insert(var_name.clone(), f);
                }
                let source_tag = format!("scenario_action:{}", definition.slug);
                let (_tid, _bver, _aver, next_payload, deltas) =
                    crate::digital_twin::stage_twin_mutation(
                        &row_id,
                        &before_payload,
                        &new_vars,
                        *merge,
                        Some(&source_tag),
                        now_ts,
                    );
                all_twin_deltas.extend(deltas);
                staged_upserts_patch.insert(row_id.clone(), next_payload.clone());
                targets.insert(twin_param.clone(), (row_id.clone(), next_payload));
                last_staged_id = row_id;
            }
            ActionEffectTemplate::SupersedeMemory {
                target_param,
                new_content_template,
                metadata_patch,
            } => {
                let Some((before_id, before_payload)) = targets.get(target_param).cloned() else {
                    return Err(ActionError::SubmissionRejected(vec![
                        ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: effect_target,
                            message: format!("unresolved parameter '{target_param}'"),
                        },
                    ]));
                };
                let new_content = render_template_string(new_content_template, &bound_params)
                    .map_err(|f| ActionError::SubmissionRejected(vec![f]))?;
                let patch_val = render_template_value(metadata_patch, &bound_params)
                    .map_err(|f| ActionError::SubmissionRejected(vec![f]))?;
                let after_id = format!(
                    "scen-sup-{}",
                    &crate::rdf::action::deterministic_upsert_id(
                        namespace,
                        &definition.slug,
                        &bound_params,
                        effect_idx
                    )[11..]
                );
                let mut after_payload = before_payload.clone();
                after_payload.content = new_content;
                if !after_payload.metadata.is_object() {
                    after_payload.metadata = serde_json::json!({});
                }
                if let Some(meta) = after_payload.metadata.as_object_mut() {
                    if let Some(patch_obj) = patch_val.as_object() {
                        for (k, v) in patch_obj {
                            meta.insert(k.clone(), v.clone());
                        }
                    }
                    meta.remove("valid_to");
                    meta.remove("superseded_by");
                    meta.insert("valid_from".to_string(), serde_json::json!(now_ts));
                    meta.insert("supersedes".to_string(), serde_json::json!(before_id));
                }
                staged_retirements_patch.insert(before_id);
                staged_upserts_patch.insert(after_id.clone(), after_payload.clone());
                targets.insert(target_param.clone(), (after_id.clone(), after_payload));
                last_staged_id = after_id;
            }
            ActionEffectTemplate::UpsertMemory {
                memory_type,
                content_template,
                metadata_template,
            } => {
                let content = render_template_string(content_template, &bound_params)
                    .map_err(|f| ActionError::SubmissionRejected(vec![f]))?;
                let mut metadata = render_template_value(metadata_template, &bound_params)
                    .map_err(|f| ActionError::SubmissionRejected(vec![f]))?;
                if !metadata.is_object() {
                    metadata = serde_json::json!({});
                }
                if let Some(meta_obj) = metadata.as_object_mut() {
                    meta_obj
                        .entry("valid_from")
                        .or_insert_with(|| serde_json::json!(now_ts));
                }
                let upsert_id = crate::rdf::action::deterministic_upsert_id(
                    namespace,
                    &definition.slug,
                    &bound_params,
                    effect_idx,
                );
                let payload = MemoryPayload {
                    content,
                    user_id: effective_principal.clone(),
                    memory_type: memory_type.clone(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata,
                };
                staged_upserts_patch.insert(upsert_id.clone(), payload);
                last_staged_id = upsert_id;
            }
            ActionEffectTemplate::LinkEdge {
                source_param,
                relation,
                target_param,
            } => {
                let Some((src_id, _)) = targets.get(source_param) else {
                    return Err(ActionError::SubmissionRejected(vec![
                        ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: effect_target,
                            message: format!("unresolved source '{source_param}'"),
                        },
                    ]));
                };
                let Some((dst_id, _)) = targets.get(target_param) else {
                    return Err(ActionError::SubmissionRejected(vec![
                        ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: effect_target,
                            message: format!("unresolved target '{target_param}'"),
                        },
                    ]));
                };
                staged_edges_patch.push(StagedEdge {
                    source: src_id.clone(),
                    relation: relation.clone(),
                    target: dst_id.clone(),
                });
            }
            ActionEffectTemplate::TransitionTask {
                target_param,
                new_status,
                note_template,
            } => {
                let Some((task_id, before_payload)) = targets.get(target_param).cloned() else {
                    return Err(ActionError::SubmissionRejected(vec![
                        ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: effect_target,
                            message: format!("unresolved task '{target_param}'"),
                        },
                    ]));
                };
                let note = render_template_string(note_template, &bound_params)
                    .map_err(|f| ActionError::SubmissionRejected(vec![f]))?;
                let session_tag = format!("scenario_action:{}", definition.slug);
                let (_bstat, _astat, after_payload) = crate::task::stage_task_transition_at(
                    &task_id,
                    &before_payload,
                    new_status,
                    &note,
                    &effective_principal,
                    &session_tag,
                    now_ts,
                )
                .map_err(|e| {
                    ActionError::SubmissionRejected(vec![ActionSubmissionFailure {
                        code: "task_transition_failed".to_string(),
                        target: effect_target,
                        message: e,
                    }])
                })?;
                staged_upserts_patch.insert(task_id.clone(), after_payload.clone());
                targets.insert(target_param.clone(), (task_id.clone(), after_payload));
                last_staged_id = task_id;
            }
        }
    }

    let retired_row_ids: Vec<String> = staged_retirements_patch.iter().cloned().collect();
    let updated_state = store
        .scenarios()
        .update(namespace, scenario_id, now_ts, |scen| {
            for (k, v) in touched_live_fps {
                scen.base_fingerprints.entry(k).or_insert(v);
            }
            for rid in staged_retirements_patch {
                scen.staged_retirements.insert(rid.clone());
                scen.staged_upserts.remove(&rid);
            }
            for (uid, payload) in staged_upserts_patch {
                scen.staged_upserts.insert(uid, payload);
            }
            for edge in staged_edges_patch {
                if !scen.staged_edges.contains(&edge) {
                    scen.staged_edges.push(edge);
                }
            }
            scen.applied_actions.push(definition.slug.clone());
            Ok(scen.clone())
        })
        .map_err(ActionError::Storage)?;

    let (merged_rows, merged_edges) =
        collect_merged_scenario_rows_and_edges(store, namespace, &updated_state, now_ts)
            .await
            .map_err(ActionError::Storage)?;
    let (shacl_violations, twin_warnings_map) =
        evaluate_staged_warnings(&updated_state, &merged_rows, &merged_edges);
    let twin_warnings = twin_warnings_map.into_values().flatten().collect();

    Ok(ScenarioStageOutcome {
        scenario_id: scenario_id.to_string(),
        staged_row_id: last_staged_id,
        retired_row_ids,
        twin_deltas: all_twin_deltas,
        shacl_violations,
        twin_warnings,
    })
}

pub async fn evaluate_scenario_sparql(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: &str,
    query_str: &str,
    format_str: Option<&str>,
) -> Result<ScenarioSparqlOutcome, String> {
    let fmt_label = format_str.unwrap_or("json").to_ascii_lowercase();
    let fmt = match fmt_label.as_str() {
        "xml" => sparesults::QueryResultsFormat::Xml,
        "csv" => sparesults::QueryResultsFormat::Csv,
        "tsv" => sparesults::QueryResultsFormat::Tsv,
        _ => sparesults::QueryResultsFormat::Json,
    };
    let (results, solution_count, truncated, timed_out) =
        crate::rdf::sparql::execute_scenario_sparql_query_with_config(
            store,
            namespace,
            scenario_id,
            query_str,
            fmt,
            crate::rdf::sparql::SparqlConfig::default(),
        )
        .await?;

    Ok(ScenarioSparqlOutcome {
        scenario_id: scenario_id.to_string(),
        namespace: namespace.to_string(),
        format: fmt_label,
        results,
        solution_count,
        truncated,
        timed_out,
    })
}

async fn detect_base_conflicts(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario: &ScenarioState,
    now_ts: i64,
) -> Result<Vec<String>, String> {
    let mut conflicts = Vec::new();
    for (row_id, expected_fp) in &scenario.base_fingerprints {
        let actual_fp = current_live_fingerprint(store, namespace, row_id, now_ts).await?;
        if &actual_fp != expected_fp {
            conflicts.push(row_id.clone());
        }
    }
    conflicts.sort();
    Ok(conflicts)
}

fn collect_twins_by_id(
    rows: &[(String, MemoryPayload)],
) -> BTreeMap<String, BTreeMap<String, f64>> {
    let mut out: BTreeMap<String, (u64, BTreeMap<String, f64>)> = BTreeMap::new();
    for (row_id, payload) in rows {
        if payload.memory_type != "digital_twin" {
            continue;
        }
        let tid = extract_twin_id_for_row(row_id, payload);
        let ver = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("version"))
            .or_else(|| {
                payload
                    .metadata
                    .get("digital_twin")
                    .and_then(|t| t.get("version"))
            })
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let vars = extract_twin_variables_map(payload);
        let replace = match out.get(&tid) {
            None => true,
            Some((prev_ver, _)) => ver >= *prev_ver,
        };
        if replace {
            out.insert(tid, (ver, vars));
        }
    }
    out.into_iter().map(|(k, (_v, vars))| (k, vars)).collect()
}

pub async fn diff_scenario(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: &str,
    compare_to_scenario_id: Option<&str>,
) -> Result<ScenarioDiffReport, String> {
    use crate::traits::VectorStore;
    let now_ts = chrono::Utc::now().timestamp();
    let scenario = store.scenarios().get(namespace, scenario_id, now_ts)?;
    let (scen_rows, scen_edges) =
        collect_merged_scenario_rows_and_edges(store, namespace, &scenario, now_ts).await?;
    let (shacl_violations, twin_warnings) =
        evaluate_staged_warnings(&scenario, &scen_rows, &scen_edges);
    let base_conflicts = detect_base_conflicts(store, namespace, &scenario, now_ts).await?;

    let (compared_against, base_rows, added_ids, retired_ids, added_edges) =
        if let Some(other_id) = compare_to_scenario_id.filter(|s| !s.trim().is_empty()) {
            let other = store.scenarios().get(namespace, other_id, now_ts)?;
            let (other_rows, _other_edges) =
                collect_merged_scenario_rows_and_edges(store, namespace, &other, now_ts).await?;
            let other_map: BTreeMap<String, String> = other_rows
                .iter()
                .map(|(id, p)| (id.clone(), sha256_fingerprint(p)))
                .collect();
            let scen_map: BTreeMap<String, String> = scen_rows
                .iter()
                .map(|(id, p)| (id.clone(), sha256_fingerprint(p)))
                .collect();

            let mut added: Vec<String> = scen_map
                .iter()
                .filter(|(id, fp)| other_map.get(*id) != Some(*fp))
                .map(|(id, _)| id.clone())
                .collect();
            added.sort();

            let mut retired: Vec<String> = other_map
                .keys()
                .filter(|id| !scen_map.contains_key(*id))
                .cloned()
                .collect();
            retired.sort();

            let edge_diff: Vec<StagedEdge> = scenario
                .staged_edges
                .iter()
                .filter(|e| !other.staged_edges.contains(e))
                .cloned()
                .collect();

            (other_id.to_string(), other_rows, added, retired, edge_diff)
        } else {
            let live_records = store
                .list(namespace, None, None)
                .await
                .map_err(|e| format!("failed to list live rows: {e}"))?;
            let live_rows: Vec<(String, MemoryPayload)> = live_records
                .into_iter()
                .filter(|r| is_active_payload(&r.payload, now_ts))
                .map(|r| (r.id, r.payload))
                .collect();
            let mut added: Vec<String> = scenario.staged_upserts.keys().cloned().collect();
            added.sort();
            let mut retired: Vec<String> = scenario
                .staged_retirements
                .iter()
                .filter(|id| !scenario.staged_upserts.contains_key(*id))
                .cloned()
                .collect();
            retired.sort();
            (
                "live".to_string(),
                live_rows,
                added,
                retired,
                scenario.staged_edges.clone(),
            )
        };

    let base_twins = collect_twins_by_id(&base_rows);
    let scen_twins = collect_twins_by_id(&scen_rows);
    let mut all_twin_ids: BTreeSet<String> = base_twins.keys().cloned().collect();
    for k in scen_twins.keys() {
        all_twin_ids.insert(k.clone());
    }

    let mut twin_deltas: BTreeMap<String, Vec<TwinDelta>> = BTreeMap::new();
    for tid in all_twin_ids {
        let old_vars = base_twins.get(&tid);
        let new_vars = scen_twins.get(&tid);
        let mut var_names: BTreeSet<String> = BTreeSet::new();
        if let Some(m) = old_vars {
            var_names.extend(m.keys().cloned());
        }
        if let Some(m) = new_vars {
            var_names.extend(m.keys().cloned());
        }
        let mut deltas_for_twin = Vec::new();
        for name in var_names {
            let old = old_vars.and_then(|m| m.get(&name).copied());
            let new = new_vars.and_then(|m| m.get(&name).copied());
            if old != new {
                deltas_for_twin.push(TwinDelta { name, old, new });
            }
        }
        if !deltas_for_twin.is_empty() {
            twin_deltas.insert(tid, deltas_for_twin);
        }
    }

    Ok(ScenarioDiffReport {
        scenario_id: scenario_id.to_string(),
        compared_against,
        added_ids,
        retired_ids,
        twin_deltas,
        added_edges,
        shacl_violations,
        twin_warnings,
        base_conflicts,
    })
}

pub async fn commit_scenario(
    store: &crate::store::ladybug::LadybugStore,
    embedder: &dyn crate::traits::Embedder,
    namespace: &str,
    scenario_id: &str,
    enforce_shacl_on_commit: bool,
    security: &ActionSecurityContext,
) -> Result<ScenarioCommitOutcome, String> {
    use crate::traits::VectorStore;
    let now_ts = chrono::Utc::now().timestamp();
    let scenario = store.scenarios().get(namespace, scenario_id, now_ts)?;
    let expired = now_ts >= scenario.expires_at;

    let base_conflicts = detect_base_conflicts(store, namespace, &scenario, now_ts).await?;
    let all_base_fingerprints_match = base_conflicts.is_empty();

    let effective_principal = security.effective_principal();
    let mut rebac_authorized = !effective_principal.trim().is_empty();
    if rebac_authorized && security.authenticated_principal != "admin" {
        for row_id in scenario.base_fingerprints.keys() {
            if let Some((_vec, live_payload)) = store
                .get(namespace, row_id)
                .await
                .map_err(|e| format!("failed to read live row '{row_id}' for ReBAC check: {e}"))?
            {
                if is_active_payload(&live_payload, now_ts) {
                    if !crate::rebac::can_execute_action_on_row(
                        &effective_principal,
                        &live_payload.user_id,
                        "editor",
                        &live_payload.metadata,
                    ) {
                        rebac_authorized = false;
                        break;
                    }
                }
            }
        }
    }

    let (merged_rows, merged_edges) =
        collect_merged_scenario_rows_and_edges(store, namespace, &scenario, now_ts).await?;
    let (shacl_violations, _twin_warnings) =
        evaluate_staged_warnings(&scenario, &merged_rows, &merged_edges);

    let decision = evaluate_scenario_commit(
        expired,
        all_base_fingerprints_match,
        rebac_authorized,
        enforce_shacl_on_commit,
        shacl_violations.len() as u32,
    );

    if decision != ScenarioCommitDecision::CommitAllowed {
        let message = match decision {
            ScenarioCommitDecision::RejectedExpired => {
                format!("scenario '{scenario_id}' has expired")
            }
            ScenarioCommitDecision::RejectedBaseConflict => format!(
                "scenario '{scenario_id}' has {} base CAS conflict(s): {:?}",
                base_conflicts.len(),
                base_conflicts
            ),
            ScenarioCommitDecision::RejectedUnauthorized => format!(
                "principal '{effective_principal}' is not authorized to commit scenario '{scenario_id}'"
            ),
            ScenarioCommitDecision::RejectedShaclViolations => format!(
                "scenario '{scenario_id}' failed SHACL commit gate with {} violation(s)",
                shacl_violations.len()
            ),
            ScenarioCommitDecision::CommitAllowed => String::new(),
        };
        return Ok(ScenarioCommitOutcome::Rejected {
            scenario_id: scenario_id.to_string(),
            decision: decision.as_tag().to_string(),
            base_conflicts,
            shacl_violations,
            message,
        });
    }

    let mut writes: Vec<crate::rdf::action::StagedMemoryWrite> = Vec::new();
    let mut committed_retirements = Vec::new();
    for rid in &scenario.staged_retirements {
        if scenario.staged_upserts.contains_key(rid) {
            continue;
        }
        if let Some((vec, mut live_payload)) = store
            .get(namespace, rid)
            .await
            .map_err(|e| format!("failed to read row '{rid}' during scenario commit: {e}"))?
        {
            if is_active_payload(&live_payload, now_ts) {
                if !live_payload.metadata.is_object() {
                    live_payload.metadata = serde_json::json!({});
                }
                if let Some(meta) = live_payload.metadata.as_object_mut() {
                    meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
                }
                writes.push(crate::rdf::action::StagedMemoryWrite {
                    id: rid.clone(),
                    vector: vec,
                    payload: live_payload,
                });
                committed_retirements.push(rid.clone());
            }
        }
    }

    let mut committed_upserts = Vec::new();
    let mut refresh_ontology_needed = false;
    for (uid, payload) in &scenario.staged_upserts {
        if matches!(
            payload.memory_type.as_str(),
            "action_type"
                | "digital_twin_schema"
                | "ontology"
                | "concept"
                | "thesis"
        ) {
            refresh_ontology_needed = true;
        }
        let vec = embedder
            .embed(&payload.content)
            .await
            .unwrap_or_else(|_| vec![0.0; embedder.dimensions()]);
        writes.push(crate::rdf::action::StagedMemoryWrite {
            id: uid.clone(),
            vector: vec,
            payload: payload.clone(),
        });
        committed_upserts.push(uid.clone());
    }

    let mut edge_deltas: Vec<crate::rdf::action::EdgeDelta> = Vec::new();
    for edge in &scenario.staged_edges {
        edge_deltas.push(crate::rdf::action::EdgeDelta {
            source: edge.source.clone(),
            relation: edge.relation.clone(),
            target: edge.target.clone(),
            existed_before: false,
            op: crate::rdf::action::EdgeMutationOp::EnsurePresent,
        });
    }
    let committed_edges = edge_deltas.len();

    store
        .init(namespace)
        .await
        .map_err(|e| format!("failed to init store for scenario commit '{scenario_id}': {e}"))?;

    let expected_heads: Vec<crate::rdf::action::ExpectedHead> = scenario
        .base_fingerprints
        .iter()
        .map(|(row_id, expected_fp)| crate::rdf::action::ExpectedHead::RowFingerprint {
            row_id: row_id.clone(),
            expected_fingerprint: fingerprint_digest_u64(expected_fp),
        })
        .collect();

    let commit_outcome = store
        .commit_action_batch(
            namespace,
            crate::rdf::action::ActionWriteSet {
                expected_heads,
                writes,
                edge_deltas,
            },
        )
        .await;

    if let Err(crate::rdf::action::ActionError::ConflictWithNewerVersion { receipt_id, .. }) =
        commit_outcome
    {
        let mut base_conflicts = detect_base_conflicts(store, namespace, &scenario, now_ts)
            .await
            .unwrap_or_default();
        if base_conflicts.is_empty() {
            base_conflicts.push(receipt_id.clone());
        }
        return Ok(ScenarioCommitOutcome::Rejected {
            scenario_id: scenario_id.to_string(),
            decision: ScenarioCommitDecision::RejectedBaseConflict.as_tag().to_string(),
            base_conflicts,
            shacl_violations,
            message: format!(
                "scenario '{scenario_id}' aborted on transactional CAS conflict for '{receipt_id}'"
            ),
        });
    }
    if let Err(e) = commit_outcome {
        return Err(format!("failed to commit scenario '{scenario_id}': {e:?}"));
    }

    if refresh_ontology_needed {
        store.refresh_ontology(namespace).await;
    }

    store.scenarios().remove(namespace, scenario_id);

    Ok(ScenarioCommitOutcome::Committed {
        scenario_id: scenario_id.to_string(),
        committed_upserts,
        committed_retirements,
        committed_edges,
    })
}

pub fn drop_scenario(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    scenario_id: &str,
) -> bool {
    store.scenarios().remove(namespace, scenario_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ladybug::LadybugStore;
    use crate::traits::{Embedder, VectorStore};
    use async_trait::async_trait;

    struct DummyEmbedder(usize);

    #[async_trait]
    impl Embedder for DummyEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.1; self.0])
        }
        fn dimensions(&self) -> usize {
            self.0
        }
    }

    fn admin_sec() -> ActionSecurityContext {
        ActionSecurityContext {
            authenticated_principal: "admin".to_string(),
            requested_principal: Some("bosun".to_string()),
        }
    }

    #[tokio::test]
    async fn scenario_overlay_isolates_live_sparql_diffs_and_enforces_cas_on_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "scen-test";
        store.init(ns).await.expect("init");
        let sec = admin_sec();

        // Seed a live digital twin schema and a live digital twin (temperature = 72.0)
        let schema_payload = MemoryPayload {
            content: "ThermalPump schema".to_string(),
            user_id: "bosun".to_string(),
            memory_type: "digital_twin_schema".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin_schema": {
                    "schema_id": "ThermalPump",
                    "variables": {
                        "temperature": { "min": 0.0, "max": 120.0, "unit": "C" }
                    }
                }
            }),
        };
        store
            .upsert(ns, "schema-thermal-pump", vec![0.1; 4], schema_payload)
            .await
            .expect("seed schema");

        let twin_payload = MemoryPayload {
            content: "Pump-1 twin".to_string(),
            user_id: "bosun".to_string(),
            memory_type: "digital_twin".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin": {
                    "id": "pump-1",
                    "schema_id": "ThermalPump",
                    "version": 1,
                    "variables": { "temperature": 72.0 },
                    "last_updated": 1000
                },
                "digital_twin": {
                    "twin_id": "pump-1",
                    "schema_id": "ThermalPump",
                    "version": 1,
                    "variables": { "temperature": 72.0 },
                    "properties": { "temperature": 72.0 },
                    "updated_at": 1000
                }
            }),
        };
        store
            .upsert(ns, "pump-1", vec![0.1; 4], twin_payload)
            .await
            .expect("seed twin");

        // 1. Create scenario and stage twin temperature = 180.0 (out of bounds -> warn-first!) + new rule
        let summary = create_scenario(
            &store,
            ns,
            Some("scen-overheat".to_string()),
            Some("Overheat Failover Simulation".to_string()),
            Some(1800),
            &sec,
        )
        .expect("create scenario");
        assert_eq!(summary.scenario_id, "scen-overheat");

        let mut new_vars = BTreeMap::new();
        new_vars.insert("temperature".to_string(), 180.0);
        let stage_twin_out = stage_scenario_twin(
            &store,
            ns,
            "scen-overheat",
            "pump-1",
            new_vars,
            None,
            true,
            None,
            None,
            &sec,
        )
        .await
        .expect("stage twin warn-first");
        assert_eq!(stage_twin_out.twin_deltas.len(), 1);
        assert_eq!(stage_twin_out.twin_deltas[0].old, Some(72.0));
        assert_eq!(stage_twin_out.twin_deltas[0].new, Some(180.0));
        // Warn-first: 180.0 > max 120.0 produces OutOfBounds TwinWarning without failing
        assert_eq!(stage_twin_out.twin_warnings.len(), 1);

        let _stage_rule_out = stage_scenario_memory(
            &store,
            ns,
            "scen-overheat",
            Some("rule-emergency-cutoff".to_string()),
            "rule",
            "Emergency cutoff engages when pump temperature exceeds 150C.",
            serde_json::json!({ "domain": "thermal" }),
            None,
            vec![StagedEdge {
                source: "rule-emergency-cutoff".to_string(),
                relation: "GOVERNS".to_string(),
                target: "pump-1".to_string(),
            }],
            &sec,
        )
        .await
        .expect("stage rule");

        // Live store is untouched (still 72.0, and rule-emergency-cutoff is absent in live)
        let (_v, live_pump) = store
            .get(ns, "pump-1")
            .await
            .expect("get")
            .expect("live pump");
        assert_eq!(
            extract_twin_variables_map(&live_pump).get("temperature"),
            Some(&72.0)
        );
        assert!(store
            .get(ns, "rule-emergency-cutoff")
            .await
            .expect("get")
            .is_none());

        // 2. SPARQL query on scenario sees 180.0 and staged rule, while live SPARQL sees 72.0
        let ask_180 = "ASK { ?s <urn:neurostrata:ns#twinVar_temperature> ?t . FILTER(?t > 150.0) }";
        let scen_sparql =
            evaluate_scenario_sparql(&store, ns, "scen-overheat", ask_180, Some("json"))
                .await
                .expect("scen sparql");
        assert!(
            scen_sparql.results.contains("true"),
            "scenario SPARQL should see temperature 180.0: {}",
            scen_sparql.results
        );

        let (live_sparql, _, _, _) = crate::rdf::sparql::execute_sparql_query_with_config(
            &store,
            ns,
            ask_180,
            sparesults::QueryResultsFormat::Json,
            false,
            false,
            crate::rdf::sparql::SparqlConfig::default(),
        )
        .await
        .expect("live sparql");
        assert!(
            live_sparql.contains("false"),
            "live SPARQL must still see 72.0: {}",
            live_sparql
        );

        // 3. Diff report returns exact TwinDelta, added_ids, added_edges, and twin_warnings
        let diff = diff_scenario(&store, ns, "scen-overheat", None)
            .await
            .expect("diff");
        assert_eq!(diff.compared_against, "live");
        assert!(diff
            .added_ids
            .contains(&"rule-emergency-cutoff".to_string()));
        assert!(diff.added_ids.contains(&"pump-1".to_string()));
        let pump_deltas = diff.twin_deltas.get("pump-1").expect("pump-1 delta");
        assert_eq!(pump_deltas.len(), 1);
        assert_eq!(pump_deltas[0].old, Some(72.0));
        assert_eq!(pump_deltas[0].new, Some(180.0));
        assert_eq!(diff.added_edges.len(), 1);
        assert!(diff.base_conflicts.is_empty());

        // 4. Concurrent modification of pump-1 in live triggers CAS BaseConflict on commit
        let mut concurrent_vars = BTreeMap::new();
        concurrent_vars.insert("temperature".to_string(), 85.0);
        let (_, _, _, concurrent_payload, _) = crate::digital_twin::stage_twin_mutation(
            "pump-1",
            &live_pump,
            &concurrent_vars,
            true,
            Some("live_sensor"),
            1005,
        );
        store
            .upsert(ns, "pump-1", vec![0.1; 4], concurrent_payload)
            .await
            .expect("concurrent live update");

        let conflict_commit = commit_scenario(&store, &embedder, ns, "scen-overheat", true, &sec)
            .await
            .expect("commit outcome");
        match conflict_commit {
            ScenarioCommitOutcome::Rejected {
                decision,
                base_conflicts,
                ..
            } => {
                assert_eq!(decision, "REJECTED_BASE_CONFLICT");
                assert_eq!(base_conflicts, vec!["pump-1".to_string()]);
            }
            other => panic!("expected RejectBaseConflict, got {other:?}"),
        }

        // 5. Create a clean scenario on top of current live state (85.0 -> 110.0) and commit it
        create_scenario(
            &store,
            ns,
            Some("scen-nominal".to_string()),
            Some("Nominal Adjustment".to_string()),
            Some(1800),
            &sec,
        )
        .expect("create scen-nominal");
        let mut nominal_vars = BTreeMap::new();
        nominal_vars.insert("temperature".to_string(), 110.0);
        stage_scenario_twin(
            &store,
            ns,
            "scen-nominal",
            "pump-1",
            nominal_vars,
            None,
            true,
            None,
            None,
            &sec,
        )
        .await
        .expect("stage nominal twin");

        let ok_commit = commit_scenario(&store, &embedder, ns, "scen-nominal", true, &sec)
            .await
            .expect("commit nominal");
        match ok_commit {
            ScenarioCommitOutcome::Committed {
                committed_upserts, ..
            } => {
                assert_eq!(committed_upserts, vec!["pump-1".to_string()]);
            }
            other => panic!("expected Committed, got {other:?}"),
        }

        // Verify live store now has temperature = 110.0 and scen-nominal is removed
        let (_v, committed_pump) = store
            .get(ns, "pump-1")
            .await
            .expect("get")
            .expect("committed pump");
        assert_eq!(
            extract_twin_variables_map(&committed_pump).get("temperature"),
            Some(&110.0)
        );
        assert!(store.scenarios().get(ns, "scen-nominal", 2000).is_err());
    }

    #[tokio::test]
    async fn commit_scenario_rejects_live_mutation_via_transactional_expected_heads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "scen-cas-tx";
        store.init(ns).await.expect("init");
        let sec = admin_sec();

        let schema_payload = MemoryPayload {
            content: "ThermalPump schema".to_string(),
            user_id: "bosun".to_string(),
            memory_type: "digital_twin_schema".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin_schema": {
                    "schema_id": "ThermalPump",
                    "variables": {
                        "temperature": { "min": 0.0, "max": 120.0, "unit": "C" }
                    }
                }
            }),
        };
        store
            .upsert(ns, "schema-thermal-pump", vec![0.1; 4], schema_payload)
            .await
            .expect("seed schema");

        let twin_payload = MemoryPayload {
            content: "Pump-1 twin".to_string(),
            user_id: "bosun".to_string(),
            memory_type: "digital_twin".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin": {
                    "id": "pump-1",
                    "schema_id": "ThermalPump",
                    "version": 1,
                    "variables": { "temperature": 72.0 },
                    "last_updated": 1000
                },
                "digital_twin": {
                    "twin_id": "pump-1",
                    "schema_id": "ThermalPump",
                    "version": 1,
                    "variables": { "temperature": 72.0 },
                    "properties": { "temperature": 72.0 },
                    "updated_at": 1000
                }
            }),
        };
        store
            .upsert(ns, "pump-1", vec![0.1; 4], twin_payload)
            .await
            .expect("seed twin");

        let summary = create_scenario(
            &store,
            ns,
            Some("scen-cas-tx".to_string()),
            Some("CAS Tx".to_string()),
            Some(1800),
            &sec,
        )
        .expect("create scenario");
        assert_eq!(summary.scenario_id, "scen-cas-tx");

        let mut new_vars = BTreeMap::new();
        new_vars.insert("temperature".to_string(), 180.0);
        let stage_twin_out = stage_scenario_twin(
            &store,
            ns,
            "scen-cas-tx",
            "pump-1",
            new_vars,
            None,
            true,
            None,
            None,
            &sec,
        )
        .await
        .expect("stage twin warn-first");
        assert_eq!(stage_twin_out.twin_deltas.len(), 1);

        let bad_head_commit = store
            .commit_action_batch(
                ns,
                crate::rdf::action::ActionWriteSet {
                    expected_heads: vec![crate::rdf::action::ExpectedHead::RowFingerprint {
                        row_id: "pump-1".to_string(),
                        expected_fingerprint: 4242,
                    }],
                    writes: Vec::new(),
                    edge_deltas: Vec::new(),
                },
            )
            .await;
        assert!(
            matches!(bad_head_commit, Err(ActionError::ConflictWithNewerVersion { .. })),
            "expected ConflictWithNewerVersion on mismatched RowFingerprint head, got {bad_head_commit:?}"
        );

        let (_v, live_pump) = store
            .get(ns, "pump-1")
            .await
            .expect("get")
            .expect("live pump");

        let mut concurrent_vars = BTreeMap::new();
        concurrent_vars.insert("temperature".to_string(), 85.0);
        let (_, _, _, concurrent_payload, _) = crate::digital_twin::stage_twin_mutation(
            "pump-1",
            &live_pump,
            &concurrent_vars,
            true,
            Some("live_sensor"),
            1005,
        );
        store
            .upsert(ns, "pump-1", vec![0.1; 4], concurrent_payload)
            .await
            .expect("concurrent live update");

        let conflict_commit = commit_scenario(&store, &embedder, ns, "scen-cas-tx", true, &sec)
            .await
            .expect("commit outcome");
        match conflict_commit {
            ScenarioCommitOutcome::Rejected {
                decision,
                base_conflicts,
                ..
            } => {
                assert_eq!(decision, "REJECTED_BASE_CONFLICT");
                assert_eq!(base_conflicts, vec!["pump-1".to_string()]);
            }
            other => panic!("expected RejectBaseConflict, got {other:?}"),
        }

        assert!(store.scenarios().get(ns, "scen-cas-tx", 2000).is_ok());
    }
}
