use crate::rdf::shacl::ShaclViolation;
use crate::traits::MemoryPayload;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionTypeDefinition {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub required_role: String,
    #[serde(default)]
    pub parameters: BTreeMap<String, ActionParamSpec>,
    #[serde(default)]
    pub submission_criteria: Vec<SubmissionCriterion>,
    #[serde(default)]
    pub effects: Vec<ActionEffectTemplate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionParamSpec {
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub target_memory_type: Option<String>,
    #[serde(default)]
    pub target_schema_id: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub in_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubmissionCriterion {
    SparqlAsk {
        query: String,
        failure_message: String,
    },
    TwinVarPrecondition {
        twin_param: String,
        variable: String,
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ActionEffectTemplate {
    SetTwinState {
        twin_param: String,
        #[serde(default)]
        merge: bool,
        variables_from_params: BTreeMap<String, String>,
    },
    SupersedeMemory {
        target_param: String,
        new_content_template: String,
        #[serde(default)]
        metadata_patch: serde_json::Value,
    },
    UpsertMemory {
        memory_type: String,
        content_template: String,
        #[serde(default)]
        metadata_template: serde_json::Value,
    },
    LinkEdge {
        source_param: String,
        relation: String,
        target_param: String,
    },
    TransitionTask {
        target_param: String,
        new_status: String,
        note_template: String,
    },
}

fn is_canonical_kebab_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn pattern_contains_service(pattern: &spargebra::algebra::GraphPattern) -> bool {
    use spargebra::algebra::GraphPattern as GP;
    match pattern {
        GP::Service { .. } => true,
        GP::Join { left, right }
        | GP::LeftJoin { left, right, .. }
        | GP::Union { left, right }
        | GP::Minus { left, right } => {
            pattern_contains_service(left) || pattern_contains_service(right)
        }
        GP::Filter { inner, .. }
        | GP::Graph { inner, .. }
        | GP::Extend { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Project { inner, .. }
        | GP::Group { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. } => pattern_contains_service(inner),
        _ => false,
    }
}

pub(crate) fn validate_sparql_ask_syntax_and_budget(
    query_str: &str,
    max_algebra_nodes: usize,
) -> Result<spargebra::Query, String> {
    if query_str.trim().is_empty() {
        return Err("SPARQL ASK query must be non-empty".to_string());
    }
    let parsed = crate::rdf::sparql::parser_with_prefixes()
        .parse_query(query_str)
        .map_err(|e| format!("invalid SPARQL query: {e}"))?;
    match &parsed {
        spargebra::Query::Ask { pattern, .. } => {
            if pattern_contains_service(pattern) {
                return Err("SPARQL SERVICE federation is forbidden in action criteria".to_string());
            }
            let nodes = crate::rdf::sparql::count_algebra_nodes(pattern);
            if nodes > max_algebra_nodes {
                return Err(format!(
                    "SPARQL ASK exceeded max_algebra_nodes budget ({nodes} > {max_algebra_nodes})"
                ));
            }
            Ok(parsed)
        }
        _ => Err("submission criterion query must be a read-only SPARQL ASK query".to_string()),
    }
}

impl ActionTypeDefinition {
    pub fn from_payload(payload: &MemoryPayload) -> Result<Self, String> {
        if payload.memory_type != "action_type" {
            return Err(format!(
                "expected memory_type 'action_type', got '{}'",
                payload.memory_type
            ));
        }
        let raw = payload
            .metadata
            .get("action_type")
            .ok_or_else(|| "metadata.action_type object is required".to_string())?;
        serde_json::from_value(raw.clone())
            .map_err(|e| format!("invalid metadata.action_type schema: {e}"))
    }

    pub fn validate_definition(&self) -> Vec<ShaclViolation> {
        let mut out = Vec::new();

        if !is_canonical_kebab_slug(&self.slug) {
            out.push(ShaclViolation {
                field: "metadata.action_type.slug".to_string(),
                why: "slug must be canonical kebab-case ([a-z0-9-]+)".to_string(),
            });
        }
        if self.title.trim().is_empty() {
            out.push(ShaclViolation {
                field: "metadata.action_type.title".to_string(),
                why: "title must be non-empty".to_string(),
            });
        }
        if !matches!(self.required_role.as_str(), "editor" | "owner") {
            out.push(ShaclViolation {
                field: "metadata.action_type.required_role".to_string(),
                why: "required_role must be 'editor' or 'owner'".to_string(),
            });
        }

        for (name, spec) in &self.parameters {
            if name.trim().is_empty() {
                out.push(ShaclViolation {
                    field: "metadata.action_type.parameters".to_string(),
                    why: "parameter name must be non-empty".to_string(),
                });
            }
            if !matches!(
                spec.param_type.as_str(),
                "memory_id" | "twin_id" | "f64" | "i64" | "string" | "bool"
            ) {
                out.push(ShaclViolation {
                    field: format!("metadata.action_type.parameters.{name}.param_type"),
                    why: "param_type must be one of memory_id|twin_id|f64|i64|string|bool"
                        .to_string(),
                });
            }
            if let Some(min) = spec.min {
                if !min.is_finite() {
                    out.push(ShaclViolation {
                        field: format!("metadata.action_type.parameters.{name}.bounds"),
                        why: "min bound must be finite".to_string(),
                    });
                }
            }
            if let Some(max) = spec.max {
                if !max.is_finite() {
                    out.push(ShaclViolation {
                        field: format!("metadata.action_type.parameters.{name}.bounds"),
                        why: "max bound must be finite".to_string(),
                    });
                }
            }
            if let (Some(min), Some(max)) = (spec.min, spec.max) {
                if min.is_finite() && max.is_finite() && min > max {
                    out.push(ShaclViolation {
                        field: format!("metadata.action_type.parameters.{name}.bounds"),
                        why: "min bound must be <= max bound".to_string(),
                    });
                }
            }
            if let Some(vals) = &spec.in_values {
                if vals.is_empty() || vals.iter().any(|v| v.is_empty()) {
                    out.push(ShaclViolation {
                        field: format!("metadata.action_type.parameters.{name}.in_values"),
                        why: "in_values must be non-empty with non-empty strings".to_string(),
                    });
                }
            }
            if let Some(tmt) = &spec.target_memory_type {
                if tmt.trim().is_empty() {
                    out.push(ShaclViolation {
                        field: format!(
                            "metadata.action_type.parameters.{name}.target_memory_type"
                        ),
                        why: "target_memory_type must be non-empty when set".to_string(),
                    });
                }
            }
            if let Some(tsi) = &spec.target_schema_id {
                if tsi.trim().is_empty() {
                    out.push(ShaclViolation {
                        field: format!("metadata.action_type.parameters.{name}.target_schema_id"),
                        why: "target_schema_id must be non-empty when set".to_string(),
                    });
                }
            }
        }

        let default_max_nodes = crate::rdf::sparql::SparqlConfig::default().max_algebra_nodes;
        for (i, crit) in self.submission_criteria.iter().enumerate() {
            match crit {
                SubmissionCriterion::SparqlAsk {
                    query,
                    failure_message,
                } => {
                    if failure_message.trim().is_empty() {
                        out.push(ShaclViolation {
                            field: format!(
                                "metadata.action_type.submission_criteria[{i}].failure_message"
                            ),
                            why: "failure_message must be non-empty".to_string(),
                        });
                    }
                    if let Err(why) =
                        validate_sparql_ask_syntax_and_budget(query, default_max_nodes)
                    {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.submission_criteria[{i}].query"),
                            why,
                        });
                    }
                }
                SubmissionCriterion::TwinVarPrecondition {
                    twin_param,
                    variable,
                    min,
                    max,
                } => {
                    match self.parameters.get(twin_param) {
                        Some(p) if p.param_type == "twin_id" || p.param_type == "memory_id" => {}
                        _ => {
                            out.push(ShaclViolation {
                                field: format!(
                                    "metadata.action_type.submission_criteria[{i}].twin_param"
                                ),
                                why: "twin_param must reference a declared twin_id/memory_id parameter"
                                    .to_string(),
                            });
                        }
                    }
                    if variable.trim().is_empty() {
                        out.push(ShaclViolation {
                            field: format!(
                                "metadata.action_type.submission_criteria[{i}].variable"
                            ),
                            why: "variable must be non-empty".to_string(),
                        });
                    }
                    let bad_min = min.map(|v| !v.is_finite()).unwrap_or(false);
                    let bad_max = max.map(|v| !v.is_finite()).unwrap_or(false);
                    let inverted = match (min, max) {
                        (Some(a), Some(b)) if a.is_finite() && b.is_finite() => a > b,
                        _ => false,
                    };
                    if bad_min || bad_max || inverted || (min.is_none() && max.is_none()) {
                        out.push(ShaclViolation {
                            field: format!(
                                "metadata.action_type.submission_criteria[{i}].bounds"
                            ),
                            why: "twin precondition requires finite min/max with min <= max"
                                .to_string(),
                        });
                    }
                }
            }
        }

        if self.effects.is_empty() {
            out.push(ShaclViolation {
                field: "metadata.action_type.effects".to_string(),
                why: "effects must contain at least one effect".to_string(),
            });
        }

        for (i, effect) in self.effects.iter().enumerate() {
            match effect {
                ActionEffectTemplate::SetTwinState {
                    twin_param,
                    variables_from_params,
                    ..
                } => {
                    if !self.parameters.contains_key(twin_param) {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].twin_param"),
                            why: "twin_param must reference a declared parameter".to_string(),
                        });
                    }
                    if variables_from_params.is_empty()
                        || variables_from_params
                            .iter()
                            .any(|(var, p)| var.trim().is_empty() || !self.parameters.contains_key(p))
                    {
                        out.push(ShaclViolation {
                            field: format!(
                                "metadata.action_type.effects[{i}].variables_from_params"
                            ),
                            why: "variables_from_params must be non-empty and reference declared parameters"
                                .to_string(),
                        });
                    }
                }
                ActionEffectTemplate::SupersedeMemory {
                    target_param,
                    new_content_template,
                    metadata_patch,
                } => {
                    if !self.parameters.contains_key(target_param) {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].target_param"),
                            why: "target_param must reference a declared parameter".to_string(),
                        });
                    }
                    if new_content_template.trim().is_empty() {
                        out.push(ShaclViolation {
                            field: format!(
                                "metadata.action_type.effects[{i}].new_content_template"
                            ),
                            why: "new_content_template must be non-empty".to_string(),
                        });
                    }
                    if !metadata_patch.is_null() && !metadata_patch.is_object() {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].metadata_patch"),
                            why: "metadata_patch must be a JSON object or null".to_string(),
                        });
                    }
                }
                ActionEffectTemplate::UpsertMemory {
                    memory_type,
                    content_template,
                    metadata_template,
                } => {
                    if memory_type.trim().is_empty()
                        || memory_type == "action_receipt"
                        || memory_type == "task"
                    {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].memory_type"),
                            why: "memory_type must be a valid non-empty non-receipt/non-task type"
                                .to_string(),
                        });
                    }
                    if content_template.trim().is_empty() {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].content_template"),
                            why: "content_template must be non-empty".to_string(),
                        });
                    }
                    if !metadata_template.is_null() && !metadata_template.is_object() {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].metadata_template"),
                            why: "metadata_template must be a JSON object or null".to_string(),
                        });
                    }
                }
                ActionEffectTemplate::LinkEdge {
                    source_param,
                    relation,
                    target_param,
                } => {
                    if !self.parameters.contains_key(source_param) {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].source_param"),
                            why: "source_param must reference a declared parameter".to_string(),
                        });
                    }
                    if !self.parameters.contains_key(target_param) {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].target_param"),
                            why: "target_param must reference a declared parameter".to_string(),
                        });
                    }
                    if !matches!(
                        relation.as_str(),
                        "GOVERNS" | "DEPENDS_ON" | "RELATES_TO" | "CONTAINS"
                    ) {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].relation"),
                            why: "relation must be GOVERNS|DEPENDS_ON|RELATES_TO|CONTAINS"
                                .to_string(),
                        });
                    }
                }
                ActionEffectTemplate::TransitionTask {
                    target_param,
                    new_status,
                    note_template,
                } => {
                    if !self.parameters.contains_key(target_param) {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].target_param"),
                            why: "target_param must reference a declared parameter".to_string(),
                        });
                    }
                    if !matches!(new_status.as_str(), "open" | "in_progress" | "blocked") {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].new_status"),
                            why: "new_status must be open|in_progress|blocked (Lock 2 owns done)"
                                .to_string(),
                        });
                    }
                    if note_template.trim().is_empty() {
                        out.push(ShaclViolation {
                            field: format!("metadata.action_type.effects[{i}].note_template"),
                            why: "note_template must be non-empty".to_string(),
                        });
                    }
                }
            }
        }

        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSecurityContext {
    pub authenticated_principal: String,
    #[serde(default)]
    pub requested_principal: Option<String>,
}

impl ActionSecurityContext {
    pub fn effective_principal(&self) -> String {
        crate::rebac::resolve_principal(
            &self.authenticated_principal,
            self.requested_principal.as_deref(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionInvocation {
    pub namespace: String,
    pub slug: String,
    pub security: ActionSecurityContext,
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSubmissionFailure {
    pub code: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionTargetRecord {
    pub param_name: String,
    pub row_id: String,
    pub payload: MemoryPayload,
}

#[derive(Debug, Clone)]
pub struct ActionEvaluation {
    pub namespace: String,
    pub definition: ActionTypeDefinition,
    pub effective_principal: String,
    pub bound_params: BTreeMap<String, serde_json::Value>,
    pub rdf_bindings: Vec<(oxrdf::Variable, oxrdf::Term)>,
    pub targets: BTreeMap<String, ActionTargetRecord>,
    pub submission_failures: Vec<ActionSubmissionFailure>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionError {
    UnknownActionType {
        namespace: String,
        slug: String,
    },
    DuplicateActionSlug {
        namespace: String,
        slug: String,
    },
    InvalidDefinition(Vec<ShaclViolation>),
    ReceiptNotFound {
        namespace: String,
        receipt_id: String,
    },
    AlreadyReverted {
        receipt_id: String,
        reverted_by_receipt_id: String,
    },
    ConflictWithNewerVersion {
        receipt_id: String,
        superseded_by: String,
    },
    PermissionDenied(String),
    Storage(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::UnknownActionType { namespace, slug } => {
                write!(f, "unknown action_type '{slug}' in namespace '{namespace}'")
            }
            ActionError::DuplicateActionSlug { namespace, slug } => {
                write!(
                    f,
                    "active action_type '{slug}' already exists in namespace '{namespace}'"
                )
            }
            ActionError::InvalidDefinition(v) => {
                write!(f, "invalid action_type definition ({} violations)", v.len())
            }
            ActionError::ReceiptNotFound {
                namespace,
                receipt_id,
            } => write!(
                f,
                "action_receipt '{receipt_id}' not found in namespace '{namespace}'"
            ),
            ActionError::AlreadyReverted {
                receipt_id,
                reverted_by_receipt_id,
            } => write!(
                f,
                "action_receipt '{receipt_id}' was already reverted by '{reverted_by_receipt_id}'"
            ),
            ActionError::ConflictWithNewerVersion {
                receipt_id,
                superseded_by,
            } => write!(
                f,
                "action_receipt '{receipt_id}' conflicts with newer head '{superseded_by}'"
            ),
            ActionError::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
            ActionError::Storage(msg) => write!(f, "storage error: {msg}"),
        }
    }
}

impl std::error::Error for ActionError {}

pub(crate) fn is_active_payload(payload: &MemoryPayload, now_ts: i64) -> bool {
    let archived = payload
        .metadata
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if archived {
        return false;
    }
    if crate::synaptic_mesh::conflict::is_conflicting_metadata(&payload.metadata) {
        return false;
    }
    match payload.metadata.get("valid_to") {
        None => true,
        Some(v) if v.is_null() => true,
        Some(v) => v.as_i64().map_or(false, |ts| ts == 0 || ts > now_ts),
    }
}

async fn resolve_namespace_target_row(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    raw_id: &str,
    is_twin_lookup: bool,
) -> Result<Option<(String, MemoryPayload)>, ActionError> {
    use crate::traits::VectorStore;
    let now_ts = chrono::Utc::now().timestamp();
    if let Some((_vec, payload)) = store
        .get(namespace, raw_id)
        .await
        .map_err(|e| ActionError::Storage(e.to_string()))?
    {
        if is_active_payload(&payload, now_ts) {
            return Ok(Some((raw_id.to_string(), payload)));
        }
    }

    if is_twin_lookup {
        let rows = store
            .list(namespace, Some("digital_twin"), None)
            .await
            .map_err(|e| ActionError::Storage(e.to_string()))?;
        let mut best: Option<(u64, String, MemoryPayload)> = None;
        for r in rows {
            if !is_active_payload(&r.payload, now_ts) {
                continue;
            }
            let tid = r
                .payload
                .metadata
                .get("twin")
                .and_then(|t| t.get("id"))
                .or_else(|| {
                    r.payload
                        .metadata
                        .get("digital_twin")
                        .and_then(|t| t.get("twin_id").or_else(|| t.get("id")))
                })
                .and_then(|v| v.as_str());
            if tid == Some(raw_id) {
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
        if let Some((_ver, id, payload)) = best {
            return Ok(Some((id, payload)));
        }
    }

    Ok(None)
}

fn is_non_finite_number_string(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "nan" | "+nan" | "-nan" | "inf" | "+inf" | "-inf" | "infinity" | "+infinity" | "-infinity"
    )
}

pub async fn evaluate_action(
    store: &crate::store::ladybug::LadybugStore,
    invocation: &ActionInvocation,
) -> Result<ActionEvaluation, ActionError> {
    use crate::traits::VectorStore;

    let mut ont = store.get_ontology(&invocation.namespace);
    if !ont.action_types.contains_key(&invocation.slug) {
        store.refresh_ontology(&invocation.namespace).await;
        ont = store.get_ontology(&invocation.namespace);
    }
    let definition = ont
        .action_types
        .get(&invocation.slug)
        .cloned()
        .ok_or_else(|| ActionError::UnknownActionType {
            namespace: invocation.namespace.clone(),
            slug: invocation.slug.clone(),
        })?;

    let def_violations = definition.validate_definition();
    if !def_violations.is_empty() {
        return Err(ActionError::InvalidDefinition(def_violations));
    }

    let mut submission_failures = Vec::new();
    let effective_principal = invocation.security.effective_principal();
    if effective_principal.trim().is_empty() {
        submission_failures.push(ActionSubmissionFailure {
            code: "unauthorized_principal".to_string(),
            target: "principal".to_string(),
            message: "authenticated principal is required".to_string(),
        });
    }
    if invocation.security.authenticated_principal != "admin" {
        if let Some(req) = invocation
            .security
            .requested_principal
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if req != invocation.security.authenticated_principal {
                submission_failures.push(ActionSubmissionFailure {
                    code: "unauthorized_principal".to_string(),
                    target: "principal".to_string(),
                    message: "non-admin caller cannot impersonate another principal".to_string(),
                });
            }
        }
    }

    // 1. Reject unknown parameters without echoing caller-supplied values.
    for key in invocation.params.keys() {
        if !definition.parameters.contains_key(key) {
            submission_failures.push(ActionSubmissionFailure {
                code: "unknown_parameter".to_string(),
                target: key.clone(),
                message: format!("unknown parameter '{key}'"),
            });
        }
    }

    let mut bound_params = BTreeMap::new();
    let mut rdf_bindings: Vec<(oxrdf::Variable, oxrdf::Term)> = Vec::new();
    let mut targets: BTreeMap<String, ActionTargetRecord> = BTreeMap::new();

    for (name, spec) in &definition.parameters {
        let Some(raw_val) = invocation.params.get(name) else {
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
            "f64" => match raw_val {
                serde_json::Value::String(s) if is_non_finite_number_string(s) => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "non_finite_number".to_string(),
                        target: name.clone(),
                        message: format!("parameter '{name}' must be a finite f64"),
                    });
                }
                serde_json::Value::Number(n) => {
                    let Some(f) = n.as_f64().filter(|v| v.is_finite()) else {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "non_finite_number".to_string(),
                            target: name.clone(),
                            message: format!("parameter '{name}' must be a finite f64"),
                        });
                        continue;
                    };
                    if let Some(min) = spec.min {
                        if f < min {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "out_of_bounds".to_string(),
                                target: name.clone(),
                                message: format!("parameter '{name}' is below minimum bound"),
                            });
                            continue;
                        }
                    }
                    if let Some(max) = spec.max {
                        if f > max {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "out_of_bounds".to_string(),
                                target: name.clone(),
                                message: format!("parameter '{name}' is above maximum bound"),
                            });
                            continue;
                        }
                    }
                    bound_params.insert(name.clone(), serde_json::json!(f));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((var, oxrdf::Term::Literal(oxrdf::Literal::from(f))));
                    }
                }
                _ => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "invalid_parameter_type".to_string(),
                        target: name.clone(),
                        message: format!("parameter '{name}' must have type f64"),
                    });
                }
            },
            "i64" => match raw_val {
                serde_json::Value::String(s) if is_non_finite_number_string(s) => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "non_finite_number".to_string(),
                        target: name.clone(),
                        message: format!("parameter '{name}' must be a finite i64"),
                    });
                }
                serde_json::Value::Number(n) => {
                    let Some(i) = n.as_i64() else {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "invalid_parameter_type".to_string(),
                            target: name.clone(),
                            message: format!("parameter '{name}' must have integer type i64"),
                        });
                        continue;
                    };
                    if let Some(min) = spec.min {
                        if (i as f64) < min {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "out_of_bounds".to_string(),
                                target: name.clone(),
                                message: format!("parameter '{name}' is below minimum bound"),
                            });
                            continue;
                        }
                    }
                    if let Some(max) = spec.max {
                        if (i as f64) > max {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "out_of_bounds".to_string(),
                                target: name.clone(),
                                message: format!("parameter '{name}' is above maximum bound"),
                            });
                            continue;
                        }
                    }
                    bound_params.insert(name.clone(), serde_json::json!(i));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((var, oxrdf::Term::Literal(oxrdf::Literal::from(i))));
                    }
                }
                _ => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "invalid_parameter_type".to_string(),
                        target: name.clone(),
                        message: format!("parameter '{name}' must have type i64"),
                    });
                }
            },
            "string" => match raw_val {
                serde_json::Value::String(s) => {
                    if let Some(allowed) = &spec.in_values {
                        if !allowed.contains(s) {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "invalid_enum_value".to_string(),
                                target: name.clone(),
                                message: format!(
                                    "parameter '{name}' is not in the allowed enum values"
                                ),
                            });
                            continue;
                        }
                    }
                    bound_params.insert(name.clone(), serde_json::Value::String(s.clone()));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((
                            var,
                            oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(s.clone())),
                        ));
                    }
                }
                _ => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "invalid_parameter_type".to_string(),
                        target: name.clone(),
                        message: format!("parameter '{name}' must have type string"),
                    });
                }
            },
            "bool" => match raw_val {
                serde_json::Value::Bool(b) => {
                    bound_params.insert(name.clone(), serde_json::Value::Bool(*b));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        rdf_bindings.push((var, oxrdf::Term::Literal(oxrdf::Literal::from(*b))));
                    }
                }
                _ => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "invalid_parameter_type".to_string(),
                        target: name.clone(),
                        message: format!("parameter '{name}' must have type bool"),
                    });
                }
            },
            "memory_id" | "twin_id" => match raw_val {
                serde_json::Value::String(raw_id) if !raw_id.trim().is_empty() => {
                    if let Some(allowed) = &spec.in_values {
                        if !allowed.contains(raw_id) {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "invalid_enum_value".to_string(),
                                target: name.clone(),
                                message: format!(
                                    "parameter '{name}' is not in the allowed enum values"
                                ),
                            });
                            continue;
                        }
                    }
                    let is_twin_lookup = spec.param_type == "twin_id"
                        || spec.target_memory_type.as_deref() == Some("digital_twin");
                    let Some((row_id, row_payload)) = resolve_namespace_target_row(
                        store,
                        &invocation.namespace,
                        raw_id,
                        is_twin_lookup,
                    )
                    .await?
                    else {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "target_not_found".to_string(),
                            target: name.clone(),
                            message: format!(
                                "target for parameter '{name}' does not exist in namespace"
                            ),
                        });
                        continue;
                    };

                    // Validate target memory_type against spec and referencing effects.
                    let expected_type = spec.target_memory_type.as_deref().or(
                        if spec.param_type == "twin_id" {
                            Some("digital_twin")
                        } else {
                            None
                        },
                    );
                    let mut type_ok = true;
                    if let Some(exp) = expected_type {
                        if row_payload.memory_type != exp {
                            type_ok = false;
                        }
                    }
                    for effect in &definition.effects {
                        match effect {
                            ActionEffectTemplate::SetTwinState { twin_param, .. }
                                if twin_param == name =>
                            {
                                if row_payload.memory_type != "digital_twin" {
                                    type_ok = false;
                                }
                            }
                            ActionEffectTemplate::TransitionTask { target_param, .. }
                                if target_param == name =>
                            {
                                if row_payload.memory_type != "task" {
                                    type_ok = false;
                                }
                            }
                            ActionEffectTemplate::SupersedeMemory { target_param, .. }
                                if target_param == name =>
                            {
                                if row_payload.memory_type == "task"
                                    || row_payload.memory_type == "action_receipt"
                                {
                                    type_ok = false;
                                }
                            }
                            _ => {}
                        }
                    }
                    if !type_ok {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "target_type_mismatch".to_string(),
                            target: name.clone(),
                            message: format!(
                                "target for parameter '{name}' does not match required memory_type"
                            ),
                        });
                    }

                    if let Some(expected_schema_id) = &spec.target_schema_id {
                        let actual_schema_id = row_payload
                            .metadata
                            .get("twin")
                            .and_then(|t| t.get("schema_id"))
                            .or_else(|| {
                                row_payload
                                    .metadata
                                    .get("digital_twin")
                                    .and_then(|t| t.get("schema_id"))
                            })
                            .or_else(|| row_payload.metadata.get("schema_id"))
                            .and_then(|v| v.as_str());
                        if actual_schema_id != Some(expected_schema_id.as_str()) {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "target_schema_mismatch".to_string(),
                                target: name.clone(),
                                message: format!(
                                    "target for parameter '{name}' does not match required target_schema_id"
                                ),
                            });
                        }
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
                                "principal lacks required role '{}' for target parameter '{name}'",
                                definition.required_role
                            ),
                        });
                    }

                    bound_params.insert(name.clone(), serde_json::Value::String(raw_id.clone()));
                    if let Ok(var) = oxrdf::Variable::new(name.as_str()) {
                        let iri = crate::rdf::namespace::memory_iri(&row_id, None);
                        if let Ok(nn) = oxrdf::NamedNode::new(iri) {
                            rdf_bindings.push((var, oxrdf::Term::NamedNode(nn)));
                        }
                    }
                    targets.insert(
                        name.clone(),
                        ActionTargetRecord {
                            param_name: name.clone(),
                            row_id,
                            payload: row_payload,
                        },
                    );
                }
                _ => {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "invalid_parameter_type".to_string(),
                        target: name.clone(),
                        message: format!(
                            "parameter '{name}' must be a non-empty {} string",
                            spec.param_type
                        ),
                    });
                }
            },
            _ => {
                submission_failures.push(ActionSubmissionFailure {
                    code: "invalid_parameter_type".to_string(),
                    target: name.clone(),
                    message: format!("unsupported parameter type for '{name}'"),
                });
            }
        }
    }

    // For targetless actions (or when the action_type definition row carries an explicit visible_to ACL),
    // verify the caller satisfies required_role on the action_type definition row itself.
    let now_ts = chrono::Utc::now().timestamp();
    if let Ok(action_rows) = store
        .list(&invocation.namespace, Some("action_type"), None)
        .await
    {
        if let Some(def_row) = action_rows.into_iter().rev().find(|r| {
            is_active_payload(&r.payload, now_ts)
                && ActionTypeDefinition::from_payload(&r.payload)
                    .map(|d| d.slug == definition.slug)
                    .unwrap_or(false)
        }) {
            let has_explicit_acl = !crate::rebac::extract_acl_with_roles(&def_row.payload.metadata)
                .is_empty();
            if targets.is_empty() || has_explicit_acl {
                if !crate::rebac::can_execute_action_on_row(
                    &effective_principal,
                    &def_row.payload.user_id,
                    &definition.required_role,
                    &def_row.payload.metadata,
                ) {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "rebac_denied".to_string(),
                        target: definition.slug.clone(),
                        message: format!(
                            "principal lacks required role '{}' for action '{}'",
                            definition.required_role, definition.slug
                        ),
                    });
                }
            }
        }
    }

    // 2. Evaluate submission_criteria (SPARQL ASK + TwinVarPrecondition)
    for (idx, criterion) in definition.submission_criteria.iter().enumerate() {
        let target_label = format!("submission_criteria[{idx}]");
        match criterion {
            SubmissionCriterion::SparqlAsk {
                query,
                failure_message,
            } => {
                match crate::rdf::sparql::execute_action_ask(
                    store,
                    &invocation.namespace,
                    query,
                    &rdf_bindings,
                    crate::rdf::sparql::SparqlConfig::default(),
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "sparql_ask_failed".to_string(),
                            target: target_label,
                            message: failure_message.clone(),
                        });
                    }
                    Err(err) => {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "sparql_ask_error".to_string(),
                            target: target_label,
                            message: err,
                        });
                    }
                }
            }
            SubmissionCriterion::TwinVarPrecondition {
                twin_param,
                variable,
                min,
                max,
            } => {
                let Some(target_rec) = targets.get(twin_param) else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "twin_precondition_failed".to_string(),
                        target: target_label,
                        message: format!("twin parameter '{twin_param}' was not resolved"),
                    });
                    continue;
                };
                let raw_var = target_rec
                    .payload
                    .metadata
                    .get("twin")
                    .and_then(|t| t.get("variables"))
                    .or_else(|| {
                        target_rec
                            .payload
                            .metadata
                            .get("digital_twin")
                            .and_then(|t| t.get("variables").or_else(|| t.get("properties")))
                    })
                    .and_then(|vars| vars.get(variable));
                let Some(val) = raw_var.and_then(|v| v.as_f64()).filter(|v| v.is_finite()) else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "twin_precondition_failed".to_string(),
                        target: target_label,
                        message: format!(
                            "twin variable '{variable}' on '{twin_param}' is missing or non-finite"
                        ),
                    });
                    continue;
                };
                if let Some(lo) = min {
                    if val < *lo {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "twin_precondition_failed".to_string(),
                            target: target_label,
                            message: format!(
                                "twin variable '{variable}' on '{twin_param}' is below min bound"
                            ),
                        });
                        continue;
                    }
                }
                if let Some(hi) = max {
                    if val > *hi {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "twin_precondition_failed".to_string(),
                            target: target_label,
                            message: format!(
                                "twin variable '{variable}' on '{twin_param}' is above max bound"
                            ),
                        });
                        continue;
                    }
                }
            }
        }
    }

    Ok(ActionEvaluation {
        namespace: invocation.namespace.clone(),
        definition,
        effective_principal,
        bound_params,
        rdf_bindings,
        targets,
        submission_failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{MemoryPayload, VectorStore};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn action_type_serde_round_trip_preserves_all_five_effects() {
        let mut params = BTreeMap::new();
        params.insert(
            "twin_id".to_string(),
            ActionParamSpec {
                param_type: "twin_id".to_string(),
                required: true,
                target_memory_type: Some("digital_twin".to_string()),
                target_schema_id: Some("cooling-loop".to_string()),
                min: None,
                max: None,
                in_values: None,
            },
        );
        params.insert(
            "target_temp".to_string(),
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
        params.insert(
            "rule_id".to_string(),
            ActionParamSpec {
                param_type: "memory_id".to_string(),
                required: true,
                target_memory_type: Some("rule".to_string()),
                target_schema_id: None,
                min: None,
                max: None,
                in_values: None,
            },
        );
        params.insert(
            "task_id".to_string(),
            ActionParamSpec {
                param_type: "memory_id".to_string(),
                required: true,
                target_memory_type: Some("task".to_string()),
                target_schema_id: None,
                min: None,
                max: None,
                in_values: None,
            },
        );
        let mut vars = BTreeMap::new();
        vars.insert("temperature".to_string(), "target_temp".to_string());

        let def = ActionTypeDefinition {
            slug: "recalibrate-cooling-loop".to_string(),
            title: "Recalibrate Cooling Loop".to_string(),
            description: "Adjust cooling loop temperature and update governance".to_string(),
            required_role: "editor".to_string(),
            parameters: params,
            submission_criteria: vec![
                SubmissionCriterion::SparqlAsk {
                    query: "ASK WHERE { ?s a class:Rule }".to_string(),
                    failure_message: "At least one rule must exist".to_string(),
                },
                SubmissionCriterion::TwinVarPrecondition {
                    twin_param: "twin_id".to_string(),
                    variable: "temperature".to_string(),
                    min: Some(0.0),
                    max: Some(120.0),
                },
            ],
            effects: vec![
                ActionEffectTemplate::SetTwinState {
                    twin_param: "twin_id".to_string(),
                    merge: true,
                    variables_from_params: vars,
                },
                ActionEffectTemplate::SupersedeMemory {
                    target_param: "rule_id".to_string(),
                    new_content_template: "Updated rule for {{twin_id}} at {{target_temp}}".to_string(),
                    metadata_patch: json!({"source": "recalibrate-cooling-loop"}),
                },
                ActionEffectTemplate::UpsertMemory {
                    memory_type: "fact".to_string(),
                    content_template: "Observed recalibration to {{target_temp}}".to_string(),
                    metadata_template: json!({"domain": "ops"}),
                },
                ActionEffectTemplate::LinkEdge {
                    source_param: "rule_id".to_string(),
                    relation: "GOVERNS".to_string(),
                    target_param: "twin_id".to_string(),
                },
                ActionEffectTemplate::TransitionTask {
                    target_param: "task_id".to_string(),
                    new_status: "in_progress".to_string(),
                    note_template: "Recalibrated {{twin_id}}".to_string(),
                },
            ],
        };

        let val = serde_json::to_value(&def).expect("serialize");
        let round: ActionTypeDefinition = serde_json::from_value(val).expect("deserialize");
        assert_eq!(round, def);
        assert!(round.validate_definition().is_empty());
    }

    #[test]
    fn definition_rejects_bad_slug_role_bounds_and_non_ask_service() {
        let mut params = BTreeMap::new();
        params.insert(
            "bad_bounds".to_string(),
            ActionParamSpec {
                param_type: "f64".to_string(),
                required: true,
                target_memory_type: None,
                target_schema_id: None,
                min: Some(100.0),
                max: Some(10.0),
                in_values: None,
            },
        );
        let def = ActionTypeDefinition {
            slug: "Bad_Slug!!".to_string(),
            title: "".to_string(),
            description: "desc".to_string(),
            required_role: "viewer".to_string(),
            parameters: params,
            submission_criteria: vec![
                SubmissionCriterion::SparqlAsk {
                    query: "SELECT ?s WHERE { ?s ?p ?o }".to_string(),
                    failure_message: "not ask".to_string(),
                },
                SubmissionCriterion::SparqlAsk {
                    query: "ASK WHERE { SERVICE <https://example.org/sparql> { ?s ?p ?o } }".to_string(),
                    failure_message: "has service".to_string(),
                },
            ],
            effects: vec![ActionEffectTemplate::TransitionTask {
                target_param: "missing_param".to_string(),
                new_status: "done".to_string(),
                note_template: "".to_string(),
            }],
        };

        let violations = def.validate_definition();
        let fields: Vec<&str> = violations.iter().map(|v| v.field.as_str()).collect();
        assert!(fields.contains(&"metadata.action_type.slug"));
        assert!(fields.contains(&"metadata.action_type.title"));
        assert!(fields.contains(&"metadata.action_type.required_role"));
        assert!(fields.contains(&"metadata.action_type.parameters.bad_bounds.bounds"));
        assert!(fields.contains(&"metadata.action_type.submission_criteria[0].query"));
        assert!(fields.contains(&"metadata.action_type.submission_criteria[1].query"));
        assert!(fields.contains(&"metadata.action_type.effects[0].new_status"));
        assert!(fields.contains(&"metadata.action_type.effects[0].target_param"));
    }

    #[tokio::test]
    async fn valid_definition_compiles_only_in_its_namespace() {
        let tmp = std::env::temp_dir().join(format!("ns-action-compile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-a").await.unwrap();
        store.init("ns-b").await.unwrap();

        let payload = MemoryPayload {
            content: "Action definition: reset-loop".to_string(),
            user_id: "bosun".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "action_type": {
                    "slug": "reset-loop",
                    "title": "Reset Loop",
                    "description": "Reset cooling loop",
                    "required_role": "editor",
                    "parameters": {
                        "note": {
                            "param_type": "string",
                            "required": true
                        }
                    },
                    "submission_criteria": [],
                    "effects": [
                        {
                            "op": "upsert_memory",
                            "memory_type": "fact",
                            "content_template": "Loop reset: {{note}}",
                            "metadata_template": {"domain": "ops"}
                        }
                    ]
                }
            }),
        };

        store
            .upsert("ns-a", "act-1", vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let ont_a = store.get_ontology("ns-a");
        let ont_b = store.get_ontology("ns-b");
        assert!(ont_a.action_types.contains_key("reset-loop"));
        assert!(!ont_b.action_types.contains_key("reset-loop"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn malformed_raw_definition_warns_but_does_not_compile_or_block_write() {
        let tmp = std::env::temp_dir().join(format!("ns-action-malformed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-warn").await.unwrap();

        let bad_payload = MemoryPayload {
            content: "Malformed action".to_string(),
            user_id: "bosun".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "action_type": {
                    "slug": "Invalid Slug!",
                    "title": "Bad",
                    "description": "Bad",
                    "required_role": "viewer",
                    "parameters": {},
                    "submission_criteria": [],
                    "effects": []
                }
            }),
        };

        let violations = crate::rdf::shacl::validate_memory(&bad_payload);
        assert!(!violations.is_empty(), "expected SHACL violations on malformed action_type");

        store
            .upsert("ns-warn", "act-bad", vec![0.1, 0.2, 0.3, 0.4], bad_payload)
            .await
            .expect("warn-first raw upsert must not fail");

        let stored = store.get("ns-warn", "act-bad").await.unwrap();
        assert!(stored.is_some(), "malformed raw row should still be stored warn-first");
        let ont = store.get_ontology("ns-warn");
        assert!(ont.action_types.is_empty(), "malformed action_type must not compile into OntologyIndex");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn submission_rejects_unknown_missing_wrong_type_nonfinite_and_bounds() {
        let tmp = std::env::temp_dir().join(format!("ns-action-params-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-params").await.unwrap();

        let action_payload = MemoryPayload {
            content: "Action definition: tune-pump".to_string(),
            user_id: "owner-1".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "action_type": {
                    "slug": "tune-pump",
                    "title": "Tune Pump",
                    "description": "Tune pump speed and mode",
                    "required_role": "editor",
                    "parameters": {
                        "speed": {
                            "param_type": "f64",
                            "required": true,
                            "min": 10.0,
                            "max": 100.0
                        },
                        "retries": {
                            "param_type": "i64",
                            "required": true,
                            "min": 1.0,
                            "max": 5.0
                        },
                        "mode": {
                            "param_type": "string",
                            "required": true,
                            "in_values": ["auto", "manual"]
                        },
                        "enabled": {
                            "param_type": "bool",
                            "required": true
                        }
                    },
                    "submission_criteria": [],
                    "effects": [
                        {
                            "op": "upsert_memory",
                            "memory_type": "fact",
                            "content_template": "Pump tuned: {{mode}}",
                            "metadata_template": {"domain": "ops"}
                        }
                    ]
                }
            }),
        };
        store
            .upsert("ns-params", "act-tune", vec![0.1; 4], action_payload)
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("unknown_extra".to_string(), json!("secret-token-value"));
        params.insert("speed".to_string(), json!("NaN"));
        params.insert("retries".to_string(), json!(99));
        params.insert("mode".to_string(), json!("turbo-secret"));
        // "enabled" omitted -> missing_parameter

        let inv = ActionInvocation {
            namespace: "ns-params".to_string(),
            slug: "tune-pump".to_string(),
            security: ActionSecurityContext {
                authenticated_principal: "owner-1".to_string(),
                requested_principal: None,
            },
            params,
        };

        let eval = evaluate_action(&store, &inv).await.expect("evaluate");
        let codes: Vec<&str> = eval
            .submission_failures
            .iter()
            .map(|f| f.code.as_str())
            .collect();
        assert!(codes.contains(&"unknown_parameter"), "got: {:?}", eval.submission_failures);
        assert!(codes.contains(&"missing_parameter"), "got: {:?}", eval.submission_failures);
        assert!(codes.contains(&"non_finite_number"), "got: {:?}", eval.submission_failures);
        assert!(codes.contains(&"out_of_bounds"), "got: {:?}", eval.submission_failures);
        assert!(codes.contains(&"invalid_enum_value"), "got: {:?}", eval.submission_failures);
        for f in &eval.submission_failures {
            assert!(!f.message.contains("secret-token-value"));
            assert!(!f.message.contains("turbo-secret"));
        }

        // Also test wrong JSON type (string for bool) and below-min f64
        let mut params2 = BTreeMap::new();
        params2.insert("speed".to_string(), json!(5.0));
        params2.insert("retries".to_string(), json!(2));
        params2.insert("mode".to_string(), json!("auto"));
        params2.insert("enabled".to_string(), json!("not-a-bool"));
        let inv2 = ActionInvocation {
            namespace: "ns-params".to_string(),
            slug: "tune-pump".to_string(),
            security: ActionSecurityContext {
                authenticated_principal: "owner-1".to_string(),
                requested_principal: None,
            },
            params: params2,
        };
        let eval2 = evaluate_action(&store, &inv2).await.expect("evaluate2");
        let codes2: Vec<&str> = eval2
            .submission_failures
            .iter()
            .map(|f| f.code.as_str())
            .collect();
        assert!(codes2.contains(&"out_of_bounds"));
        assert!(codes2.contains(&"invalid_parameter_type"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn targets_are_typed_namespace_local_and_rebac_checked_for_every_effect() {
        let tmp = std::env::temp_dir().join(format!("ns-action-targets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-a").await.unwrap();
        store.init("ns-b").await.unwrap();

        // Target exists in ns-b, NOT in ns-a -> must not leak across namespaces
        store
            .upsert(
                "ns-b",
                "twin-foreign",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Foreign twin".to_string(),
                    user_id: "owner-1".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "twin": { "id": "twin-foreign", "schema_id": "pump-schema", "version": 1, "variables": { "rpm": 50.0 } }
                    }),
                },
            )
            .await
            .unwrap();

        // Local twin with wrong schema_id and viewer-only ACL for bob
        store
            .upsert(
                "ns-a",
                "twin-local",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Local twin".to_string(),
                    user_id: "owner-1".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "visible_to": [{"principal": "bob", "role": "viewer"}],
                        "twin": { "id": "twin-local", "schema_id": "wrong-schema", "version": 1, "variables": { "rpm": 50.0 } }
                    }),
                },
            )
            .await
            .unwrap();

        // Local fact where a task is expected
        store
            .upsert(
                "ns-a",
                "not-a-task",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Just a fact".to_string(),
                    user_id: "owner-1".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({"domain": "ops"}),
                },
            )
            .await
            .unwrap();

        let action_payload = MemoryPayload {
            content: "Action definition: inspect-targets".to_string(),
            user_id: "owner-1".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "action_type": {
                    "slug": "inspect-targets",
                    "title": "Inspect Targets",
                    "description": "Multi-effect target check",
                    "required_role": "editor",
                    "parameters": {
                        "twin_id": {
                            "param_type": "twin_id",
                            "required": true,
                            "target_memory_type": "digital_twin",
                            "target_schema_id": "pump-schema"
                        },
                        "task_id": {
                            "param_type": "memory_id",
                            "required": true,
                            "target_memory_type": "task"
                        }
                    },
                    "submission_criteria": [],
                    "effects": [
                        {
                            "op": "transition_task",
                            "target_param": "task_id",
                            "new_status": "in_progress",
                            "note_template": "Started on {{twin_id}}"
                        }
                    ]
                }
            }),
        };
        store
            .upsert("ns-a", "act-targets", vec![0.1; 4], action_payload)
            .await
            .unwrap();

        // 1. Foreign target in ns-b must fail as target_not_found in ns-a
        let mut p1 = BTreeMap::new();
        p1.insert("twin_id".to_string(), json!("twin-foreign"));
        p1.insert("task_id".to_string(), json!("not-a-task"));
        let eval1 = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-a".to_string(),
                slug: "inspect-targets".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "owner-1".to_string(),
                    requested_principal: None,
                },
                params: p1,
            },
        )
        .await
        .unwrap();
        let codes1: Vec<&str> = eval1.submission_failures.iter().map(|f| f.code.as_str()).collect();
        assert!(codes1.contains(&"target_not_found"), "got: {:?}", eval1.submission_failures);
        assert!(codes1.contains(&"target_type_mismatch"), "got: {:?}", eval1.submission_failures);

        // 2. Local twin with wrong schema_id and bob (viewer) -> schema mismatch + rebac_denied
        let mut p2 = BTreeMap::new();
        p2.insert("twin_id".to_string(), json!("twin-local"));
        p2.insert("task_id".to_string(), json!("not-a-task"));
        let eval2 = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-a".to_string(),
                slug: "inspect-targets".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "bob".to_string(),
                    requested_principal: None,
                },
                params: p2,
            },
        )
        .await
        .unwrap();
        let codes2: Vec<&str> = eval2.submission_failures.iter().map(|f| f.code.as_str()).collect();
        assert!(codes2.contains(&"target_schema_mismatch"), "got: {:?}", eval2.submission_failures);
        assert!(codes2.contains(&"rebac_denied"), "got: {:?}", eval2.submission_failures);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn non_admin_cannot_impersonate_admin_and_viewer_cannot_apply() {
        let tmp = std::env::temp_dir().join(format!("ns-action-rebac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-sec").await.unwrap();

        store
            .upsert(
                "ns-sec",
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Original rule".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "domain": "ops",
                        "source": "spec",
                        "visible_to": [
                            {"principal": "bob-viewer", "role": "viewer"},
                            {"principal": "carol-editor", "role": "editor"}
                        ]
                    }),
                },
            )
            .await
            .unwrap();

        let action_payload = MemoryPayload {
            content: "Action definition: update-rule".to_string(),
            user_id: "alice".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "action_type": {
                    "slug": "update-rule",
                    "title": "Update Rule",
                    "description": "Supersede rule",
                    "required_role": "editor",
                    "parameters": {
                        "rule_id": {
                            "param_type": "memory_id",
                            "required": true,
                            "target_memory_type": "rule"
                        }
                    },
                    "submission_criteria": [],
                    "effects": [
                        {
                            "op": "supersede_memory",
                            "target_param": "rule_id",
                            "new_content_template": "Updated rule on {{rule_id}}",
                            "metadata_patch": {}
                        }
                    ]
                }
            }),
        };
        store
            .upsert("ns-sec", "act-rule", vec![0.1; 4], action_payload)
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("rule_id".to_string(), json!("rule-1"));

        // Viewer attempting to impersonate admin -> fails closed
        let eval_impersonate = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-sec".to_string(),
                slug: "update-rule".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "bob-viewer".to_string(),
                    requested_principal: Some("admin".to_string()),
                },
                params: params.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(eval_impersonate.effective_principal, "bob-viewer");
        let codes_imp: Vec<&str> = eval_impersonate
            .submission_failures
            .iter()
            .map(|f| f.code.as_str())
            .collect();
        assert!(codes_imp.contains(&"unauthorized_principal"));
        assert!(codes_imp.contains(&"rebac_denied"));

        // Viewer without impersonation -> rebac_denied
        let eval_viewer = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-sec".to_string(),
                slug: "update-rule".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "bob-viewer".to_string(),
                    requested_principal: None,
                },
                params: params.clone(),
            },
        )
        .await
        .unwrap();
        assert!(!eval_viewer.submission_failures.is_empty());

        // Editor without impersonation -> passes
        let eval_editor = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-sec".to_string(),
                slug: "update-rule".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "carol-editor".to_string(),
                    requested_principal: None,
                },
                params: params.clone(),
            },
        )
        .await
        .unwrap();
        assert!(eval_editor.submission_failures.is_empty());

        // Admin acting on behalf of carol-editor -> passes
        let eval_admin = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-sec".to_string(),
                slug: "update-rule".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "admin".to_string(),
                    requested_principal: Some("carol-editor".to_string()),
                },
                params,
            },
        )
        .await
        .unwrap();
        assert_eq!(eval_admin.effective_principal, "carol-editor");
        assert!(eval_admin.submission_failures.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn ask_binds_hostile_text_without_changing_query_structure() {
        let tmp = std::env::temp_dir().join(format!("ns-action-inject-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-inj").await.unwrap();

        store
            .upsert(
                "ns-inj",
                "mem-safe",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Safe content".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({"domain": "ops"}),
                },
            )
            .await
            .unwrap();

        let query = "ASK WHERE { ?target ns:content ?expected_text }";
        let target_var = oxrdf::Variable::new("target").unwrap();
        let text_var = oxrdf::Variable::new("expected_text").unwrap();
        let target_term = oxrdf::Term::NamedNode(
            oxrdf::NamedNode::new("urn:neurostrata:memory:mem-safe").unwrap(),
        );

        // Hostile injection payload attempting to close quotes and inject UNION
        let hostile_text = "\" } UNION { ?s ?p ?o } #";
        let hostile_term =
            oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(hostile_text));

        let res_false = crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-inj",
            query,
            &[(target_var.clone(), target_term.clone()), (text_var.clone(), hostile_term)],
            crate::rdf::sparql::SparqlConfig::default(),
        )
        .await
        .expect("bound ASK must succeed without syntax alteration");
        assert!(!res_false, "hostile text must evaluate to false, not inject UNION");

        let exact_term =
            oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal("Safe content"));
        let res_true = crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-inj",
            query,
            &[(target_var, target_term), (text_var, exact_term)],
            crate::rdf::sparql::SparqlConfig::default(),
        )
        .await
        .expect("bound ASK with matching literal");
        assert!(res_true);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn malformed_select_service_update_timeout_and_large_algebra_fail_without_writes() {
        let tmp = std::env::temp_dir().join(format!("ns-action-ask-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-ask").await.unwrap();

        store
            .upsert(
                "ns-ask",
                "seed-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Seed fact".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({"domain": "ops"}),
                },
            )
            .await
            .unwrap();

        let before_count = store.list("ns-ask", None, None).await.unwrap().len();
        let cfg = crate::rdf::sparql::SparqlConfig::default();

        // 1. Malformed syntax
        assert!(crate::rdf::sparql::execute_action_ask(&store, "ns-ask", "ASK WHERE { ??? }", &[], cfg.clone()).await.is_err());
        // 2. SELECT instead of ASK
        assert!(crate::rdf::sparql::execute_action_ask(&store, "ns-ask", "SELECT ?s WHERE { ?s ?p ?o }", &[], cfg.clone()).await.is_err());
        // 3. SERVICE clause
        assert!(crate::rdf::sparql::execute_action_ask(&store, "ns-ask", "ASK WHERE { SERVICE <https://example.com/sparql> { ?s ?p ?o } }", &[], cfg.clone()).await.is_err());
        // 4. UPDATE statement
        assert!(crate::rdf::sparql::execute_action_ask(&store, "ns-ask", "INSERT DATA { <urn:a> <urn:b> <urn:c> }", &[], cfg.clone()).await.is_err());
        // 5. Timeout (0ms budget)
        let timeout_cfg = crate::rdf::sparql::SparqlConfig {
            timeout_ms: 0,
            ..cfg.clone()
        };
        assert!(crate::rdf::sparql::execute_action_ask(&store, "ns-ask", "ASK WHERE { ?s ?p ?o }", &[], timeout_cfg).await.is_err());
        // 6. Large algebra (max_algebra_nodes = 0)
        let small_algebra_cfg = crate::rdf::sparql::SparqlConfig {
            max_algebra_nodes: 0,
            ..cfg
        };
        assert!(crate::rdf::sparql::execute_action_ask(&store, "ns-ask", "ASK WHERE { ?s ?p ?o }", &[], small_algebra_cfg).await.is_err());

        let after_count = store.list("ns-ask", None, None).await.unwrap().len();
        assert_eq!(before_count, after_count, "row count must remain unchanged");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn twin_precondition_requires_finite_value_in_range() {
        let tmp = std::env::temp_dir().join(format!("ns-action-twin-pre-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-twin").await.unwrap();

        let action_payload = MemoryPayload {
            content: "Action definition: adjust-valve".to_string(),
            user_id: "alice".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "action_type": {
                    "slug": "adjust-valve",
                    "title": "Adjust Valve",
                    "description": "Requires pressure in [20.0, 80.0]",
                    "required_role": "editor",
                    "parameters": {
                        "twin_id": {
                            "param_type": "twin_id",
                            "required": true,
                            "target_memory_type": "digital_twin"
                        },
                        "new_pressure": {
                            "param_type": "f64",
                            "required": true
                        }
                    },
                    "submission_criteria": [
                        {
                            "kind": "twin_var_precondition",
                            "twin_param": "twin_id",
                            "variable": "pressure",
                            "min": 20.0,
                            "max": 80.0
                        }
                    ],
                    "effects": [
                        {
                            "op": "set_twin_state",
                            "twin_param": "twin_id",
                            "merge": true,
                            "variables_from_params": { "pressure": "new_pressure" }
                        }
                    ]
                }
            }),
        };
        store
            .upsert("ns-twin", "act-valve", vec![0.1; 4], action_payload)
            .await
            .unwrap();

        for (id, pressure_val, should_pass) in [
            ("twin-min", json!(20.0), true),
            ("twin-max", json!(80.0), true),
            ("twin-low", json!(19.999), false),
            ("twin-high", json!(80.001), false),
            ("twin-nan", json!("NaN"), false),
        ] {
            store
                .upsert(
                    "ns-twin",
                    id,
                    vec![0.1; 4],
                    MemoryPayload {
                        content: format!("Twin {id}"),
                        user_id: "alice".to_string(),
                        memory_type: "digital_twin".to_string(),
                        agent_name: None,
                        location: "".to_string(),
                        location_lines: "".to_string(),
                        metadata: json!({
                            "twin": {
                                "id": id,
                                "version": 1,
                                "variables": { "pressure": pressure_val }
                            }
                        }),
                    },
                )
                .await
                .unwrap();

            let mut params = BTreeMap::new();
            params.insert("twin_id".to_string(), json!(id));
            params.insert("new_pressure".to_string(), json!(50.0));
            let eval = evaluate_action(
                &store,
                &ActionInvocation {
                    namespace: "ns-twin".to_string(),
                    slug: "adjust-valve".to_string(),
                    security: ActionSecurityContext {
                        authenticated_principal: "alice".to_string(),
                        requested_principal: None,
                    },
                    params,
                },
            )
            .await
            .unwrap();
            assert_eq!(
                eval.submission_failures.is_empty(),
                should_pass,
                "case {id} failed: {:?}",
                eval.submission_failures
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
