use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::digital_twin::{
    collect_twin_schemas_from_rows, compute_twin_reducers, get_state,
    resolve_effective_twin_schema, validate_attributes, validate_link_constraints, validate_state,
    TwinAttrValue, TwinWarning,
};
use crate::rdf::action::{
    ActionParamSpec, ActionReceiptPayload, ActionTypeDefinition, SubmissionCriterion,
};
use crate::rdf::shacl::ShaclViolation;
use crate::traits::MemoryPayload;

pub const DEFAULT_HISTORY_LIMIT: usize = 5;
pub const MAX_HISTORY_LIMIT: usize = 25;
pub const DEFAULT_MAX_LINKS: usize = 40;
pub const DEFAULT_MAX_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectViewRequest {
    pub namespace: String,
    /// Either a memory ID ("mem-...") or a digital twin_id ("loop-a").
    pub id: String,
    #[serde(default)]
    pub scenario_id: Option<String>,
    #[serde(default)]
    pub include_history_limit: Option<usize>,
    #[serde(default)]
    pub evaluate_action_preconditions: Option<bool>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub principal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TwinObjectState {
    pub twin_id: String,
    pub version: u64,
    pub variables: BTreeMap<String, f64>,
    pub attributes: BTreeMap<String, Value>,
    pub derived_variables: BTreeMap<String, f64>,
    pub units: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedObjectSummary {
    pub id: String,
    pub direction: String,
    pub memory_type: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectGovernanceSummary {
    pub governing_rule_ids: Vec<String>,
    pub failed_attempt_ids: Vec<String>,
    pub shacl_violations: Vec<ShaclViolation>,
    pub twin_warnings: Vec<TwinWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectLineageSummary {
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub recent_action_receipt_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectMetricSummary {
    pub series_name: String,
    pub latest_value: f64,
    pub rolling_summary: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableActionSummary {
    pub slug: String,
    pub title: String,
    pub required_role: String,
    pub authorized: bool,
    pub preconditions_met: bool,
    pub unmet_reasons: Vec<String>,
    pub parameters: BTreeMap<String, ActionParamSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectViewResponse {
    pub namespace: String,
    pub id: String,
    pub memory_type: String,
    pub concept_iri: Option<String>,
    pub domains: Vec<String>,
    pub schema_id: Option<String>,
    pub implemented_interfaces: Vec<String>,
    pub content: String,
    pub location: Option<String>,
    pub location_lines: Option<String>,
    pub twin_state: Option<TwinObjectState>,
    pub links: BTreeMap<String, Vec<LinkedObjectSummary>>,
    pub governance: ObjectGovernanceSummary,
    pub lineage: ObjectLineageSummary,
    pub metrics: Vec<ObjectMetricSummary>,
    pub available_actions: Vec<AvailableActionSummary>,
    pub truncated_by_budget: bool,
}

fn is_active_payload(payload: &MemoryPayload, now_ts: i64) -> bool {
    if payload.metadata.get("archived").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }
    if payload
        .metadata
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
    {
        return false;
    }
    if let Some(vt) = payload.metadata.get("valid_to").and_then(|v| v.as_i64()) {
        if vt > 0 && vt <= now_ts {
            return false;
        }
    }
    true
}

fn caller_can_view(principal: &str, payload: &MemoryPayload) -> bool {
    crate::rebac::can_read_memory(principal, &payload.user_id, &payload.metadata)
}

fn summarize_content(content: &str, max_chars: usize) -> String {
    let single_line = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if single_line.chars().count() <= max_chars {
        single_line
    } else {
        let mut s: String = single_line
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect();
        s.push('…');
        s
    }
}

fn extract_domains(metadata: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    match metadata.get("domain") {
        Some(Value::String(s)) if !s.trim().is_empty() => {
            out.insert(s.trim().to_string());
        }
        Some(Value::Array(arr)) => {
            for item in arr.iter().filter_map(|v| v.as_str()) {
                if !item.trim().is_empty() {
                    out.insert(item.trim().to_string());
                }
            }
        }
        _ => {}
    }
    if let Some(arr) = metadata.get("domains").and_then(|v| v.as_array()) {
        for item in arr.iter().filter_map(|v| v.as_str()) {
            if !item.trim().is_empty() {
                out.insert(item.trim().to_string());
            }
        }
    }
    out.into_iter().collect()
}

fn extract_schema_id(payload: &MemoryPayload) -> Option<String> {
    payload
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
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

fn extract_twin_id(row_id: &str, payload: &MemoryPayload) -> String {
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

fn extract_twin_version(payload: &MemoryPayload) -> u64 {
    payload
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
        .unwrap_or(1)
}

fn normalize_rel_path(p: &str) -> String {
    let trimmed = p.trim().trim_start_matches("./").trim_matches('/');
    trimmed.to_string()
}

fn path_governs_target(gov_target: &str, target_id: &str, target_location: Option<&str>) -> bool {
    let cleaned_gov = gov_target
        .rsplit("::")
        .next()
        .unwrap_or(gov_target)
        .split('#')
        .next()
        .unwrap_or(gov_target);
    let norm_gov = normalize_rel_path(cleaned_gov);
    if gov_target == target_id || cleaned_gov == target_id {
        return true;
    }
    if let Some(loc) = target_location {
        let norm_loc = normalize_rel_path(loc);
        if !norm_gov.is_empty() && !norm_loc.is_empty() {
            if norm_gov == norm_loc {
                return true;
            }
            if norm_loc.starts_with(&format!("{norm_gov}/")) {
                return true;
            }
        }
    }
    false
}

/// Hydrate a 360-degree Ontology Object View for any entity in `namespace`.
pub async fn hydrate_object_view(
    store: &crate::store::ladybug::LadybugStore,
    req: &ObjectViewRequest,
    authenticated_principal: &str,
) -> Result<ObjectViewResponse, String> {
    use crate::traits::VectorStore;

    let namespace = req.namespace.trim();
    if namespace.is_empty() {
        return Err("namespace required".to_string());
    }
    let query_id = req.id.trim();
    if query_id.is_empty() {
        return Err("id required".to_string());
    }

    let principal =
        crate::rebac::resolve_principal(authenticated_principal, req.principal.as_deref());
    let now_ts = chrono::Utc::now().timestamp();
    let history_limit = req
        .include_history_limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);
    let eval_preconditions = req.evaluate_action_preconditions.unwrap_or(true);
    let max_bytes = req.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).max(512);

    // 1. Collect active rows & edges (scenario-aware when scenario_id is provided).
    let (active_rows, active_edges): (
        Vec<(String, MemoryPayload)>,
        Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)>,
    ) = if let Some(scen_id) = req
        .scenario_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let scen = store.scenarios().get(namespace, scen_id, now_ts)?;
        let (rows, edges) = crate::rdf::scenario::collect_merged_scenario_rows_and_edges(
            store, namespace, &scen, now_ts,
        )
        .await?;
        let prov_edges = edges
            .into_iter()
            .map(|(s, r, t, _w)| (s, r, t, None))
            .collect();
        (rows, prov_edges)
    } else {
        let records = store
            .list(namespace, None, None)
            .await
            .map_err(|e| format!("failed to list namespace '{namespace}': {e}"))?;
        let rows: Vec<(String, MemoryPayload)> = records
            .into_iter()
            .filter(|r| is_active_payload(&r.payload, now_ts))
            .map(|r| (r.id, r.payload))
            .collect();
        let edges = store.list_edges(namespace).await.unwrap_or_default();
        (rows, edges)
    };

    let active_by_id: BTreeMap<String, MemoryPayload> = active_rows.iter().cloned().collect();

    // 2. Resolve target entity by exact row ID, or by digital_twin twin_id (highest active version),
    //    or by direct store.get fallback (in case a superseded/historical row ID was requested directly).
    let (resolved_id, root_payload) = if let Some(p) = active_by_id.get(query_id) {
        (query_id.to_string(), p.clone())
    } else {
        let mut best_twin: Option<(u64, String, MemoryPayload)> = None;
        for (rid, pl) in &active_rows {
            if pl.memory_type == "digital_twin" || pl.metadata.get("twin").is_some() {
                let tid = extract_twin_id(rid, pl);
                if tid == query_id {
                    let ver = extract_twin_version(pl);
                    if best_twin.as_ref().map_or(true, |(bv, brid, _)| {
                        (ver, rid.as_str()) > (*bv, brid.as_str())
                    }) {
                        best_twin = Some((ver, rid.clone(), pl.clone()));
                    }
                }
            }
        }
        if let Some((_, rid, pl)) = best_twin {
            (rid, pl)
        } else if req.scenario_id.is_none() {
            if let Some((_vec, pl)) = store
                .get(namespace, query_id)
                .await
                .map_err(|e| format!("failed to get '{query_id}': {e}"))?
            {
                (query_id.to_string(), pl)
            } else {
                return Err(format!(
                    "object '{query_id}' not found in namespace '{namespace}'"
                ));
            }
        } else {
            return Err(format!(
                "object '{query_id}' not found in scenario overlay for namespace '{namespace}'"
            ));
        }
    };

    // 3. ReBAC viewer gate on root entity (fail closed without leaking existence).
    if !caller_can_view(&principal, &root_payload) {
        return Err(format!(
            "object '{query_id}' not found or access denied for principal '{principal}'"
        ));
    }

    let visible_rows: Vec<(String, MemoryPayload)> = active_rows
        .iter()
        .filter(|(_id, pl)| caller_can_view(&principal, pl))
        .cloned()
        .collect();

    let concept_iri = root_payload
        .metadata
        .get("concept_iri")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    let domains = extract_domains(&root_payload.metadata);
    let schema_id = extract_schema_id(&root_payload);
    let location = if root_payload.location.trim().is_empty() {
        root_payload
            .metadata
            .get("source_identity")
            .and_then(|s| s.get("relative_path"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
    } else {
        Some(root_payload.location.clone())
    };
    let location_lines = if root_payload.location_lines.trim().is_empty() {
        None
    } else {
        Some(root_payload.location_lines.clone())
    };

    // 4. Digital twin state, interfaces, derived reducers, and warnings.
    let twin_schemas = collect_twin_schemas_from_rows(&active_rows);
    let mut implemented_interfaces = BTreeSet::new();
    if let Some(arr) = root_payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("implements_interfaces"))
        .or_else(|| root_payload.metadata.get("implements_interfaces"))
        .and_then(|v| v.as_array())
    {
        for iface in arr.iter().filter_map(|v| v.as_str()) {
            if !iface.trim().is_empty() {
                implemented_interfaces.insert(iface.trim().to_string());
            }
        }
    }

    let mut twin_warnings = Vec::new();
    let twin_state = if root_payload.memory_type == "digital_twin"
        || root_payload.metadata.get("twin").is_some()
    {
        let twin_id = extract_twin_id(&resolved_id, &root_payload);
        let version = extract_twin_version(&root_payload);
        let variables = get_state(&root_payload);
        let attributes: BTreeMap<String, Value> = root_payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("attributes"))
            .or_else(|| {
                root_payload
                    .metadata
                    .get("digital_twin")
                    .and_then(|t| t.get("attributes"))
            })
            .and_then(|v| v.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let mut derived_variables = BTreeMap::new();
        let mut units = BTreeMap::new();

        if let Some(ref sid) = schema_id {
            if let Some(raw_schema) = twin_schemas.get(sid) {
                let eff = resolve_effective_twin_schema(raw_schema, &twin_schemas);
                for iface in &eff.implements_interfaces {
                    implemented_interfaces.insert(iface.clone());
                }
                for iface in &eff.resolved_interfaces {
                    implemented_interfaces.insert(iface.clone());
                }
                for (var_name, spec) in &eff.variables {
                    if !spec.unit.trim().is_empty() {
                        units.insert(var_name.clone(), spec.unit.clone());
                    }
                }
                for (red_name, red_spec) in &eff.reducers {
                    if let Some(ref u) = red_spec.unit {
                        if !u.trim().is_empty() {
                            units.insert(red_name.clone(), u.clone());
                        }
                    }
                }
                derived_variables = compute_twin_reducers(
                    &resolved_id,
                    &twin_id,
                    &root_payload,
                    &eff,
                    &visible_rows,
                    &active_edges,
                    &twin_schemas,
                    None,
                );
                twin_warnings.extend(validate_state(&variables, Some(&eff)));
                let typed_attrs: BTreeMap<String, TwinAttrValue> = root_payload
                    .metadata
                    .get("twin")
                    .and_then(|t| t.get("attributes"))
                    .or_else(|| {
                        root_payload
                            .metadata
                            .get("digital_twin")
                            .and_then(|t| t.get("attributes"))
                    })
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                twin_warnings.extend(validate_attributes(&typed_attrs, Some(&eff)));
                twin_warnings.extend(validate_link_constraints(
                    &resolved_id,
                    &twin_id,
                    &root_payload,
                    &eff,
                    &visible_rows,
                    &active_edges,
                    &twin_schemas,
                ));
            }
        }

        Some(TwinObjectState {
            twin_id,
            version,
            variables,
            attributes,
            derived_variables,
            units,
        })
    } else {
        None
    };

    let implemented_interfaces_vec: Vec<String> = implemented_interfaces.into_iter().collect();

    // 5. Typed 1-hop links (bounded by DEFAULT_MAX_LINKS and filtered by ReBAC).
    let twin_id_opt = twin_state.as_ref().map(|t| t.twin_id.as_str());
    let matches_self = |node_id: &str| -> bool {
        if node_id == resolved_id || node_id == query_id {
            return true;
        }
        if let Some(tid) = twin_id_opt {
            if node_id == tid {
                return true;
            }
        }
        false
    };

    let mut raw_links: Vec<(String, LinkedObjectSummary)> = Vec::new();
    let mut seen_links: BTreeSet<(String, String, String)> = BTreeSet::new();

    for (src, rel, dst, _prov) in &active_edges {
        if matches_self(src) && !matches_self(dst) {
            if let Some(neighbor) = active_by_id.get(dst) {
                if caller_can_view(&principal, neighbor)
                    && seen_links.insert((rel.clone(), "outgoing".to_string(), dst.clone()))
                {
                    raw_links.push((
                        rel.clone(),
                        LinkedObjectSummary {
                            id: dst.clone(),
                            direction: "outgoing".to_string(),
                            memory_type: neighbor.memory_type.clone(),
                            summary: summarize_content(&neighbor.content, 140),
                        },
                    ));
                }
            }
        } else if matches_self(dst) && !matches_self(src) {
            if let Some(neighbor) = active_by_id.get(src) {
                if caller_can_view(&principal, neighbor)
                    && seen_links.insert((rel.clone(), "incoming".to_string(), src.clone()))
                {
                    raw_links.push((
                        rel.clone(),
                        LinkedObjectSummary {
                            id: src.clone(),
                            direction: "incoming".to_string(),
                            memory_type: neighbor.memory_type.clone(),
                            summary: summarize_content(&neighbor.content, 140),
                        },
                    ));
                }
            }
        }
    }

    let anchor_relations = [
        ("governs", "GOVERNS"),
        ("depends_on", "DEPENDS_ON"),
        ("relates_to", "RELATES_TO"),
        ("contains", "CONTAINS"),
        ("extracted_from", "EXTRACTED_FROM"),
    ];
    for (meta_key, rel_label) in anchor_relations {
        if let Some(arr) = root_payload
            .metadata
            .get(meta_key)
            .or_else(|| {
                root_payload
                    .metadata
                    .get("bi_directional_anchors")
                    .and_then(|b| b.get(meta_key))
            })
            .and_then(|v| v.as_array())
        {
            for target_str in arr.iter().filter_map(|v| v.as_str()) {
                if let Some((n_id, neighbor)) = resolve_neighbor_by_ref(target_str, &active_rows) {
                    if !matches_self(&n_id)
                        && caller_can_view(&principal, neighbor)
                        && seen_links.insert((
                            rel_label.to_string(),
                            "outgoing".to_string(),
                            n_id.clone(),
                        ))
                    {
                        raw_links.push((
                            rel_label.to_string(),
                            LinkedObjectSummary {
                                id: n_id,
                                direction: "outgoing".to_string(),
                                memory_type: neighbor.memory_type.clone(),
                                summary: summarize_content(&neighbor.content, 140),
                            },
                        ));
                    }
                }
            }
        }
    }

    for (other_id, other_payload) in &active_rows {
        if matches_self(other_id) || !caller_can_view(&principal, other_payload) {
            continue;
        }
        for (meta_key, rel_label) in anchor_relations {
            if let Some(arr) = other_payload
                .metadata
                .get(meta_key)
                .or_else(|| {
                    other_payload
                        .metadata
                        .get("bi_directional_anchors")
                        .and_then(|b| b.get(meta_key))
                })
                .and_then(|v| v.as_array())
            {
                let points_to_root = arr.iter().filter_map(|v| v.as_str()).any(|t| {
                    matches_self(t) || path_governs_target(t, &resolved_id, location.as_deref())
                });
                if points_to_root
                    && seen_links.insert((
                        rel_label.to_string(),
                        "incoming".to_string(),
                        other_id.clone(),
                    ))
                {
                    raw_links.push((
                        rel_label.to_string(),
                        LinkedObjectSummary {
                            id: other_id.clone(),
                            direction: "incoming".to_string(),
                            memory_type: other_payload.memory_type.clone(),
                            summary: summarize_content(&other_payload.content, 140),
                        },
                    ));
                }
            }
        }
    }

    raw_links.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.direction.cmp(&b.1.direction))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });

    let mut truncated_by_budget = false;
    if raw_links.len() > DEFAULT_MAX_LINKS {
        raw_links.truncate(DEFAULT_MAX_LINKS);
        truncated_by_budget = true;
    }

    let mut links: BTreeMap<String, Vec<LinkedObjectSummary>> = BTreeMap::new();
    for (rel, item) in raw_links {
        links.entry(rel).or_default().push(item);
    }

    // 6. Governance & Conformance (active governing rules, failed attempts, SHACL violations, twin warnings).
    let mut governing_rule_ids = BTreeSet::new();
    let mut failed_attempt_ids = BTreeSet::new();

    if let Some(gov_links) = links.get("GOVERNS") {
        for l in gov_links {
            if l.direction == "incoming" && l.memory_type == "rule" {
                governing_rule_ids.insert(l.id.clone());
            }
        }
    }

    for (rid, pl) in &active_rows {
        if !caller_can_view(&principal, pl) {
            continue;
        }
        if pl.memory_type == "rule" {
            let governs_by_loc = location.as_deref().is_some_and(|loc| {
                !pl.location.trim().is_empty()
                    && path_governs_target(&pl.location, &resolved_id, Some(loc))
            });
            let governs_by_meta = pl
                .metadata
                .get("governs")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .any(|g| path_governs_target(g, &resolved_id, location.as_deref()))
                });
            if governs_by_loc || governs_by_meta {
                governing_rule_ids.insert(rid.clone());
            }
        }

        let is_failed_attempt = pl
            .metadata
            .get("attempt")
            .and_then(|a| a.get("outcome"))
            .and_then(|v| v.as_str())
            == Some("failed");
        if is_failed_attempt {
            let matches_loc = location.as_deref().is_some_and(|loc| {
                !pl.location.trim().is_empty()
                    && normalize_rel_path(&pl.location) == normalize_rel_path(loc)
            });
            let linked_in_edges = active_edges.iter().any(|(s, _r, t, _)| {
                (s == rid && matches_self(t)) || (t == rid && matches_self(s))
            });
            let mentions_target = pl
                .metadata
                .get("governs")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .any(|g| path_governs_target(g, &resolved_id, location.as_deref()))
                });
            if matches_loc || linked_in_edges || mentions_target {
                failed_attempt_ids.insert(rid.clone());
            }
        }
    }

    let shacl_violations = crate::rdf::shacl::validate_memory(&root_payload);
    let governance = ObjectGovernanceSummary {
        governing_rule_ids: governing_rule_ids.into_iter().collect(),
        failed_attempt_ids: failed_attempt_ids.into_iter().collect(),
        shacl_violations,
        twin_warnings,
    };

    // 7. Bitemporal lineage & recent action receipts.
    let valid_from = root_payload
        .metadata
        .get("valid_from")
        .and_then(|v| v.as_i64());
    let valid_to = root_payload
        .metadata
        .get("valid_to")
        .and_then(|v| v.as_i64());
    let supersedes = root_payload
        .metadata
        .get("supersedes")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    let superseded_by = root_payload
        .metadata
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());

    let mut matching_receipts: Vec<(i64, String)> = Vec::new();
    for (_rid, pl) in &active_rows {
        if pl.memory_type != "action_receipt" {
            continue;
        }
        if let Ok(rec) = ActionReceiptPayload::from_payload(pl) {
            let touched = rec.created_ids.iter().any(|id| matches_self(id))
                || rec
                    .superseded_pairs
                    .iter()
                    .any(|p| matches_self(&p.before_id) || matches_self(&p.after_id))
                || rec.twin_deltas.iter().any(|td| {
                    matches_self(&td.twin_id)
                        || matches_self(&td.before_row_id)
                        || matches_self(&td.after_row_id)
                })
                || rec.task_deltas.iter().any(|td| matches_self(&td.task_id))
                || rec
                    .edge_deltas
                    .iter()
                    .any(|ed| matches_self(&ed.source) || matches_self(&ed.target));
            if touched {
                matching_receipts.push((rec.executed_at, rec.receipt_id));
            }
        }
    }
    matching_receipts.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    matching_receipts.truncate(history_limit);
    let recent_action_receipt_ids = matching_receipts.into_iter().map(|(_, id)| id).collect();

    let lineage = ObjectLineageSummary {
        valid_from,
        valid_to,
        supersedes,
        superseded_by,
        recent_action_receipt_ids,
    };

    // 8. Bound TimeSeriesStore metric summaries.
    let mut metrics = Vec::new();
    for (key, _count) in store.metrics().list_keys(namespace) {
        let stage = &key.2;
        let matches_metric = matches_self(stage)
            || stage.starts_with(&format!("{resolved_id}::"))
            || twin_id_opt.is_some_and(|tid| stage.starts_with(&format!("{tid}::")));
        if matches_metric {
            if let Some(agg) = store.metrics().aggregate(&key) {
                let samples = store.metrics().samples_for_anomaly(&key);
                let latest_value = samples.last().copied().unwrap_or(agg.ewma);
                metrics.push(ObjectMetricSummary {
                    series_name: format!("{}::{}", key.1.as_str(), key.2),
                    latest_value,
                    rolling_summary: agg.to_json_summary(),
                });
            }
        }
    }

    // 9. Available legal ActionTypes.
    let mut action_defs: BTreeMap<String, ActionTypeDefinition> = store
        .get_ontology(namespace)
        .action_types
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (_rid, pl) in &active_rows {
        if pl.memory_type == "action_type" {
            if let Ok(def) = ActionTypeDefinition::from_payload(pl) {
                action_defs.insert(def.slug.clone(), def);
            }
        }
    }

    let mut available_actions = Vec::new();
    for (_slug, def) in action_defs {
        let Some((target_param_name, _target_spec)) = find_matching_target_param(
            &def,
            &root_payload,
            schema_id.as_deref(),
            &implemented_interfaces_vec,
        ) else {
            continue;
        };

        let authorized = crate::rebac::can_execute_action_on_row(
            &principal,
            &root_payload.user_id,
            &def.required_role,
            &root_payload.metadata,
        );

        let mut preconditions_met = true;
        let mut unmet_reasons = Vec::new();

        if eval_preconditions {
            let sparql_cfg = crate::rdf::sparql::get_sparql_config();
            for crit in &def.submission_criteria {
                match crit {
                    SubmissionCriterion::TwinVarPrecondition {
                        twin_param,
                        variable,
                        min,
                        max,
                    } if twin_param == &target_param_name => {
                        let current_val = twin_state
                            .as_ref()
                            .and_then(|ts| {
                                ts.variables
                                    .get(variable)
                                    .or_else(|| ts.derived_variables.get(variable))
                            })
                            .copied();
                        match current_val {
                            None => {
                                preconditions_met = false;
                                unmet_reasons.push(format!(
                                    "missing twin variable '{variable}' for '{twin_param}'"
                                ));
                            }
                            Some(val) => {
                                if let Some(lo) = min {
                                    if val < *lo {
                                        preconditions_met = false;
                                        unmet_reasons.push(format!(
                                            "twin variable '{variable}' ({val}) is below min ({lo})"
                                        ));
                                    }
                                }
                                if let Some(hi) = max {
                                    if val > *hi {
                                        preconditions_met = false;
                                        unmet_reasons.push(format!(
                                            "twin variable '{variable}' ({val}) exceeds max ({hi})"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    SubmissionCriterion::SparqlAsk {
                        query,
                        failure_message,
                    } => {
                        let target_iri =
                            crate::rdf::namespace::memory_iri(&resolved_id, concept_iri.as_deref());
                        if let (Ok(var), Ok(term)) = (
                            oxrdf::Variable::new(target_param_name.clone()),
                            oxrdf::NamedNode::new(target_iri),
                        ) {
                            let bindings = vec![(var, oxrdf::Term::NamedNode(term))];
                            let ask_res = if let Some(scen_id) = req
                                .scenario_id
                                .as_deref()
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                            {
                                crate::rdf::sparql::execute_scenario_action_ask(
                                    store,
                                    namespace,
                                    scen_id,
                                    query,
                                    &bindings,
                                    sparql_cfg.clone(),
                                )
                                .await
                            } else {
                                crate::rdf::sparql::execute_action_ask(
                                    store,
                                    namespace,
                                    query,
                                    &bindings,
                                    sparql_cfg.clone(),
                                )
                                .await
                            };
                            if let Ok(false) = ask_res {
                                preconditions_met = false;
                                unmet_reasons.push(failure_message.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        available_actions.push(AvailableActionSummary {
            slug: def.slug,
            title: def.title,
            required_role: def.required_role,
            authorized,
            preconditions_met,
            unmet_reasons,
            parameters: def.parameters,
        });
    }

    let mut resp = ObjectViewResponse {
        namespace: namespace.to_string(),
        id: resolved_id,
        memory_type: root_payload.memory_type,
        concept_iri,
        domains,
        schema_id,
        implemented_interfaces: implemented_interfaces_vec,
        content: root_payload.content,
        location,
        location_lines,
        twin_state,
        links,
        governance,
        lineage,
        metrics,
        available_actions,
        truncated_by_budget,
    };

    // Enforce max_bytes ceiling on serialized JSON
    if serde_json::to_vec(&resp)
        .map(|b| b.len() > max_bytes)
        .unwrap_or(false)
    {
        resp.truncated_by_budget = true;
        if resp.content.len() > 256 {
            resp.content = summarize_content(&resp.content, 256);
        }
        while serde_json::to_vec(&resp)
            .map(|b| b.len() > max_bytes)
            .unwrap_or(false)
            && !resp.links.is_empty()
        {
            if let Some(last_key) = resp.links.keys().next_back().cloned() {
                if let Some(vec) = resp.links.get_mut(&last_key) {
                    vec.pop();
                    if vec.is_empty() {
                        resp.links.remove(&last_key);
                    }
                }
            }
        }
    }

    Ok(resp)
}

fn resolve_neighbor_by_ref<'a>(
    target_ref: &str,
    active_rows: &'a [(String, MemoryPayload)],
) -> Option<(String, &'a MemoryPayload)> {
    for (rid, pl) in active_rows {
        if rid == target_ref {
            return Some((rid.clone(), pl));
        }
    }
    let tail = target_ref.rsplit("::").next().unwrap_or(target_ref);
    for (rid, pl) in active_rows {
        if rid == tail
            || (!pl.location.is_empty()
                && normalize_rel_path(&pl.location) == normalize_rel_path(tail))
        {
            return Some((rid.clone(), pl));
        }
    }
    None
}

fn find_matching_target_param<'a>(
    def: &'a ActionTypeDefinition,
    payload: &MemoryPayload,
    schema_id: Option<&str>,
    implemented_interfaces: &[String],
) -> Option<(String, &'a ActionParamSpec)> {
    let matches_schema_filter = |target_schema: Option<&str>| -> bool {
        let Some(ts) = target_schema.map(str::trim).filter(|s| !s.is_empty()) else {
            return true;
        };
        if schema_id == Some(ts) {
            return true;
        }
        implemented_interfaces.iter().any(|i| i == ts)
    };

    for (param_name, spec) in &def.parameters {
        match spec.param_type.as_str() {
            "twin_id" => {
                if (payload.memory_type == "digital_twin" || payload.metadata.get("twin").is_some())
                    && matches_schema_filter(spec.target_schema_id.as_deref())
                {
                    return Some((param_name.clone(), spec));
                }
            }
            "memory_id" => {
                let mt_ok = spec
                    .target_memory_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map_or(true, |expected| payload.memory_type == expected);
                if mt_ok && matches_schema_filter(spec.target_schema_id.as_deref()) {
                    return Some((param_name.clone(), spec));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdf::action::{
        define_action, ActionEffectTemplate, ActionParamSpec, ActionSecurityContext,
        ActionTypeDefinition, SubmissionCriterion,
    };
    use crate::store::ladybug::LadybugStore;
    use crate::traits::{Embedder, VectorStore};

    struct DummyEmbedder(usize);
    #[async_trait::async_trait]
    impl Embedder for DummyEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.1; self.0])
        }
        fn dimensions(&self) -> usize {
            self.0
        }
    }

    #[tokio::test]
    async fn object_view_hydrates_twin_reducers_interfaces_links_metrics_and_actions_with_rebac() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "obj-view-test";
        store.init(ns).await.expect("init");

        // 1. Register TwinInterface 'ThermalAsset' and TwinSchema 'CoolingLoop' with reducer
        let iface_payload = MemoryPayload {
            content: "ThermalAsset interface".to_string(),
            user_id: "alice".to_string(),
            memory_type: "digital_twin_schema".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin_schema": {
                    "schema_id": "ThermalAsset",
                    "kind": "interface",
                    "interface_slug": "ThermalAsset",
                    "variables": {
                        "temperature": { "min": 0.0, "max": 120.0, "unit": "C" }
                    }
                }
            }),
        };
        store
            .upsert(ns, "schema-iface", vec![0.1; 4], iface_payload)
            .await
            .expect("upsert iface");

        let loop_schema = MemoryPayload {
            content: "CoolingLoop schema".to_string(),
            user_id: "alice".to_string(),
            memory_type: "digital_twin_schema".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin_schema": {
                    "schema_id": "CoolingLoop",
                    "kind": "concrete",
                    "implements_interfaces": ["ThermalAsset"],
                    "variables": {
                        "temperature": { "min": 0.0, "max": 100.0, "unit": "C" }
                    },
                    "reducers": {
                        "max_pump_temp": {
                            "relation": "CONTAINS",
                            "direction": "outgoing",
                            "source_variable": "temperature",
                            "agg": "max",
                            "unit": "C"
                        }
                    }
                }
            }),
        };
        store
            .upsert(ns, "schema-loop", vec![0.1; 4], loop_schema)
            .await
            .expect("upsert loop schema");

        // 2. Seed two child pumps (one public, one private to alice) and CoolingLoop twin
        let pump_public = MemoryPayload {
            content: "Public pump 1".to_string(),
            user_id: "alice".to_string(),
            memory_type: "digital_twin".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "twin": {
                    "id": "pump-1",
                    "version": 1,
                    "variables": { "temperature": 81.0 }
                }
            }),
        };
        store
            .upsert(ns, "twin-pump-1", vec![0.1; 4], pump_public)
            .await
            .expect("upsert pump 1");

        let pump_secret = MemoryPayload {
            content: "Classified pump 2".to_string(),
            user_id: "alice".to_string(),
            memory_type: "digital_twin".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "visible_to": [
                    { "principal": "alice", "role": "owner" }
                ],
                "twin": {
                    "id": "pump-secret",
                    "version": 1,
                    "variables": { "temperature": 95.0 }
                }
            }),
        };
        store
            .upsert(ns, "twin-pump-secret", vec![0.1; 4], pump_secret)
            .await
            .expect("upsert secret pump");

        let loop_twin = MemoryPayload {
            content: "Primary cooling loop A".to_string(),
            user_id: "alice".to_string(),
            memory_type: "digital_twin".to_string(),
            agent_name: None,
            location: "src/cooling/loop_a.rs".to_string(),
            location_lines: "1-50".to_string(),
            metadata: serde_json::json!({
                "domain": ["thermodynamics"],
                "concept_iri": "urn:neurostrata:concept:CoolingLoopA",
                "valid_from": 1700000000,
                "visible_to": [
                    { "principal": "bob", "role": "viewer" }
                ],
                "bi_directional_anchors": {
                    "contains": ["twin-pump-1", "twin-pump-secret"]
                },
                "twin": {
                    "id": "loop-a",
                    "schema_id": "CoolingLoop",
                    "version": 2,
                    "variables": { "temperature": 68.5 },
                    "attributes": { "zone": "north" }
                }
            }),
        };
        store
            .upsert(ns, "twin-loop-a", vec![0.1; 4], loop_twin)
            .await
            .expect("upsert loop twin");

        // 3. Seed an ancestor directory GOVERNS rule and a failed attempt on src/cooling/loop_a.rs
        let rule_payload = MemoryPayload {
            content: "RULE: All cooling loop controllers must enforce thermal bounds.".to_string(),
            user_id: "alice".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "src/cooling".to_string(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "governs": ["src/cooling"]
            }),
        };
        store
            .upsert(ns, "rule-cooling-dir", vec![0.1; 4], rule_payload)
            .await
            .expect("upsert rule");

        let failed_attempt = MemoryPayload {
            content: "Attempted open-loop bypass without thermal cutoff".to_string(),
            user_id: "alice".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "src/cooling/loop_a.rs".to_string(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "attempt": {
                    "approach": "open-loop bypass",
                    "outcome": "failed",
                    "failure_mode": "thermal runaway"
                }
            }),
        };
        store
            .upsert(ns, "attempt-1", vec![0.1; 4], failed_attempt)
            .await
            .expect("upsert failed attempt");

        // 4. Record a bound TimeSeriesStore metric for loop-a
        store.metrics().push(
            (
                ns.to_string(),
                crate::time_series::MetricKind::AccessCount,
                "loop-a".to_string(),
            ),
            crate::time_series::Sample {
                id: String::new(),
                seq: 0,
                ts: 1700000100,
                value: 42.0,
                labels: BTreeMap::new(),
            },
        );

        // 5. Define an editor-only ActionType targeting twin_id
        let mut params = BTreeMap::new();
        params.insert(
            "twin".to_string(),
            ActionParamSpec {
                param_type: "twin_id".to_string(),
                required: true,
                target_memory_type: None,
                target_schema_id: Some("ThermalAsset".to_string()),
                min: None,
                max: None,
                in_values: None,
            },
        );
        params.insert(
            "setpoint".to_string(),
            ActionParamSpec {
                param_type: "f64".to_string(),
                required: true,
                target_memory_type: None,
                target_schema_id: None,
                min: Some(10.0),
                max: Some(90.0),
                in_values: None,
            },
        );
        let action_def = ActionTypeDefinition {
            slug: "recalibrate-cooling-loop".to_string(),
            title: "Recalibrate Cooling Loop".to_string(),
            description: "Adjusts loop thermal setpoint".to_string(),
            required_role: "editor".to_string(),
            parameters: params,
            submission_criteria: vec![SubmissionCriterion::TwinVarPrecondition {
                twin_param: "twin".to_string(),
                variable: "temperature".to_string(),
                min: Some(10.0),
                max: Some(90.0),
            }],
            effects: vec![ActionEffectTemplate::SetTwinState {
                twin_param: "twin".to_string(),
                merge: true,
                variables_from_params: BTreeMap::from([(
                    "temperature".to_string(),
                    "setpoint".to_string(),
                )]),
            }],
        };
        let def_json = serde_json::to_value(&action_def).expect("serialize action_def");
        define_action(
            &store,
            ns,
            &ActionSecurityContext {
                authenticated_principal: "admin".to_string(),
                requested_principal: Some("alice".to_string()),
            },
            &def_json,
            &embedder,
        )
        .await
        .expect("define action");

        // 6. Query object_view as 'bob' (viewer on loop-a, excluded from twin-pump-secret)
        let req = ObjectViewRequest {
            namespace: ns.to_string(),
            id: "loop-a".to_string(),
            scenario_id: None,
            include_history_limit: Some(5),
            evaluate_action_preconditions: Some(true),
            max_bytes: None,
            project_root: None,
            principal: Some("bob".to_string()),
        };
        let view = hydrate_object_view(&store, &req, "admin")
            .await
            .expect("hydrate_object_view");

        assert_eq!(view.id, "twin-loop-a");
        assert_eq!(view.memory_type, "digital_twin");
        assert_eq!(
            view.concept_iri.as_deref(),
            Some("urn:neurostrata:concept:CoolingLoopA")
        );
        assert_eq!(view.schema_id.as_deref(), Some("CoolingLoop"));
        assert!(view
            .implemented_interfaces
            .contains(&"ThermalAsset".to_string()));

        let ts = view.twin_state.expect("twin_state present");
        assert_eq!(ts.twin_id, "loop-a");
        assert_eq!(ts.version, 2);
        assert_eq!(ts.variables.get("temperature"), Some(&68.5));
        // Because twin-pump-secret is ReBAC-filtered for bob, max_pump_temp only sees pump-1 (81.0)!
        assert_eq!(ts.derived_variables.get("max_pump_temp"), Some(&81.0));
        assert_eq!(ts.units.get("temperature").map(String::as_str), Some("C"));

        // Bob must see twin-pump-1 in CONTAINS links, but NOT twin-pump-secret (ReBAC filtered)
        let contains = view.links.get("CONTAINS").expect("CONTAINS links");
        assert_eq!(contains.len(), 1);
        assert_eq!(contains[0].id, "twin-pump-1");

        // Ancestor rule and failed attempt surfaced in governance
        assert!(view
            .governance
            .governing_rule_ids
            .contains(&"rule-cooling-dir".to_string()));
        assert!(view
            .governance
            .failed_attempt_ids
            .contains(&"attempt-1".to_string()));

        // Metric summary attached
        assert_eq!(view.metrics.len(), 1);
        assert_eq!(view.metrics[0].latest_value, 42.0);

        // Available action is listed, preconditions_met = true (68.5 in 10..90),
        // but authorized = false because bob is only a 'viewer' and action requires 'editor'.
        assert_eq!(view.available_actions.len(), 1);
        let act = &view.available_actions[0];
        assert_eq!(act.slug, "recalibrate-cooling-loop");
        assert!(!act.authorized);
        assert!(act.preconditions_met);
    }
}
