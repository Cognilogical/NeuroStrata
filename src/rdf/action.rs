use crate::rdf::shacl::ShaclViolation;
use crate::traits::MemoryPayload;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
                        field: format!("metadata.action_type.parameters.{name}.target_memory_type"),
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
                            field: format!("metadata.action_type.submission_criteria[{i}].bounds"),
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
                        || variables_from_params.iter().any(|(var, p)| {
                            var.trim().is_empty() || !self.parameters.contains_key(p)
                        })
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
    #[allow(dead_code)]
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
    SubmissionRejected(Vec<ActionSubmissionFailure>),
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
            ActionError::SubmissionRejected(v) => {
                write!(f, "action submission rejected ({} failures)", v.len())
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

pub const MAX_TEMPLATE_RENDER_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedMemoryWrite {
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: MemoryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectedHead {
    ActiveRow {
        row_id: String,
        twin_id: Option<String>,
        expected_twin_version: Option<u64>,
        expected_task_status: Option<String>,
        expected_updated_at: Option<i64>,
    },
    AbsentOrInactive {
        row_id: String,
    },
    EdgeState {
        source: String,
        relation: String,
        target: String,
        expected_exists: bool,
    },
    RowFingerprint {
        row_id: String,
        expected_fingerprint: u64,
    },
    UniqueActionSlug {
        slug: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeMutationOp {
    EnsurePresent,
    Remove,
}

fn default_edge_mutation_op() -> EdgeMutationOp {
    EdgeMutationOp::EnsurePresent
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeDelta {
    pub source: String,
    pub relation: String,
    pub target: String,
    #[serde(default)]
    pub existed_before: bool,
    #[serde(default = "default_edge_mutation_op")]
    pub op: EdgeMutationOp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersededPair {
    pub before_id: String,
    pub after_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionTwinDelta {
    pub twin_id: String,
    pub before_row_id: String,
    pub after_row_id: String,
    pub before_version: u64,
    pub after_version: u64,
    pub name: String,
    pub old: Option<f64>,
    pub new: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowDelta {
    pub kind: String,
    pub before_id: Option<String>,
    pub before_payload: Option<MemoryPayload>,
    pub after_id: String,
    pub after_payload: MemoryPayload,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDelta {
    pub task_id: String,
    pub before_status: String,
    pub after_status: String,
    pub before_payload: MemoryPayload,
    pub after_payload: MemoryPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReceiptPayload {
    pub receipt_id: String,
    pub action_slug: String,
    pub principal: String,
    pub executed_at: i64,
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
    /// Connector write-back target stamped from the action's declared
    /// `pointer_id` / `pool_id` pair. Kept inside the immutable nested
    /// receipt so a verifier never has to trust editable row metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_pointer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_pool_id: Option<String>,
    #[serde(default)]
    pub superseded_pairs: Vec<SupersededPair>,
    #[serde(default)]
    pub created_ids: Vec<String>,
    #[serde(default)]
    pub twin_deltas: Vec<ActionTwinDelta>,
    #[serde(default)]
    pub edge_deltas: Vec<EdgeDelta>,
    #[serde(default)]
    pub row_deltas: Vec<RowDelta>,
    #[serde(default)]
    pub task_deltas: Vec<TaskDelta>,
    pub reverted_at: Option<i64>,
    pub reverted_by_receipt_id: Option<String>,
    #[serde(default)]
    pub reverts_receipt_id: Option<String>,
    #[serde(default)]
    pub forced_by: Option<String>,
    #[serde(default)]
    pub forced_at: Option<i64>,
}

impl ActionReceiptPayload {
    pub fn from_payload(payload: &MemoryPayload) -> Result<Self, String> {
        let raw = payload.metadata.get("action_receipt").ok_or_else(|| {
            "action_receipt object is required for an action_receipt memory".to_string()
        })?;
        if !raw.is_object() {
            return Err("action_receipt must be a JSON object".to_string());
        }
        serde_json::from_value(raw.clone())
            .map_err(|e| format!("invalid action_receipt schema: {e}"))
    }

    pub fn validate_receipt(&self, payload: &MemoryPayload) -> Vec<ShaclViolation> {
        let mut out = Vec::new();
        if !is_canonical_kebab_slug(&self.action_slug) {
            out.push(ShaclViolation {
                field: "metadata.action_receipt.action_slug".to_string(),
                why: "action_slug must be a non-empty canonical kebab-case slug".to_string(),
            });
        }
        if self.principal.trim().is_empty() {
            out.push(ShaclViolation {
                field: "metadata.action_receipt.principal".to_string(),
                why: "principal must be non-empty".to_string(),
            });
        }
        let eligibility = payload.metadata.get("eligibility").and_then(|v| v.as_str());
        if eligibility != Some("private") {
            out.push(ShaclViolation {
                field: "metadata.eligibility".to_string(),
                why: "action_receipt eligibility must be 'private'".to_string(),
            });
        }
        out
    }
}

/// Connector write-back targets are recorded through the reserved
/// `pointer_id` / `pool_id` parameter pair. Stamp both or neither: a partial
/// pair must not produce a binding that can never match a real pointer.
fn connector_target_binding(
    params: &BTreeMap<String, serde_json::Value>,
) -> (Option<String>, Option<String>) {
    let string_param = |name: &str| {
        params
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    match (string_param("pointer_id"), string_param("pool_id")) {
        (Some(pointer), Some(pool)) => (Some(pointer), Some(pool)),
        _ => (None, None),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionWriteSet {
    pub expected_heads: Vec<ExpectedHead>,
    pub writes: Vec<StagedMemoryWrite>,
    pub edge_deltas: Vec<EdgeDelta>,
}

#[derive(Debug, Clone)]
pub struct ActionStage {
    #[allow(dead_code)]
    pub namespace: String,
    pub receipt_id: String,
    pub receipt: ActionReceiptPayload,
    pub write_set: ActionWriteSet,
    #[allow(dead_code)]
    pub submission_failures: Vec<ActionSubmissionFailure>,
}

pub const DEFAULT_ACTION_LOG_LIMIT: usize = 100;
pub const MAX_ACTION_LOG_LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertInvocation {
    pub namespace: String,
    pub receipt_id: String,
    pub security: ActionSecurityContext,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionValidateOutcome {
    pub valid: bool,
    pub namespace: String,
    pub action_slug: String,
    pub effective_principal: String,
    pub failures: Vec<ActionSubmissionFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionApplyOutcome {
    Applied {
        receipt_id: String,
        receipt: ActionReceiptPayload,
    },
    Rejected {
        failures: Vec<ActionSubmissionFailure>,
    },
}

#[cfg(test)]
impl ActionApplyOutcome {
    pub fn is_applied(&self) -> bool {
        matches!(self, ActionApplyOutcome::Applied { .. })
    }

    pub fn is_rejected(&self) -> bool {
        matches!(self, ActionApplyOutcome::Rejected { .. })
    }

    pub fn receipt_id(&self) -> Option<&str> {
        match self {
            ActionApplyOutcome::Applied { receipt_id, .. } => Some(receipt_id.as_str()),
            ActionApplyOutcome::Rejected { .. } => None,
        }
    }

    pub fn receipt(&self) -> Option<&ActionReceiptPayload> {
        match self {
            ActionApplyOutcome::Applied { receipt, .. } => Some(receipt),
            ActionApplyOutcome::Rejected { .. } => None,
        }
    }

    pub fn failures(&self) -> &[ActionSubmissionFailure] {
        match self {
            ActionApplyOutcome::Applied { .. } => &[],
            ActionApplyOutcome::Rejected { failures } => failures.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRevertOutcome {
    pub reverted_receipt_id: String,
    pub active_receipt_version_id: String,
    pub revert_receipt_id: String,
    pub revert_receipt: ActionReceiptPayload,
    pub forced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionLogEntry {
    pub receipt_id: String,
    pub version_row_id: String,
    pub action_slug: String,
    pub principal: String,
    pub executed_at: i64,
    pub parameter_keys: Vec<String>,
    pub superseded_count: usize,
    pub created_count: usize,
    pub twin_delta_count: usize,
    pub edge_delta_count: usize,
    pub task_delta_count: usize,
    pub reverted_at: Option<i64>,
    pub reverted_by_receipt_id: Option<String>,
    pub reverts_receipt_id: Option<String>,
    pub forced_by: Option<String>,
    pub forced_at: Option<i64>,
}

static LAST_ACTION_TS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub(crate) fn current_action_timestamp() -> i64 {
    let wall = chrono::Utc::now().timestamp();
    let last = LAST_ACTION_TS.load(std::sync::atomic::Ordering::SeqCst);
    wall.max(last)
}

pub(crate) fn next_action_timestamp() -> i64 {
    let wall = chrono::Utc::now().timestamp();
    loop {
        let prev = LAST_ACTION_TS.load(std::sync::atomic::Ordering::SeqCst);
        let next = wall.max(prev.saturating_add(1));
        if LAST_ACTION_TS
            .compare_exchange(
                prev,
                next,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            return next;
        }
    }
}

pub(crate) fn is_active_payload(payload: &MemoryPayload, now_ts: i64) -> bool {
    let archived = payload
        .metadata
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if archived {
        return false;
    }
    if payload
        .metadata
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .map_or(false, |s| !s.trim().is_empty())
    {
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
    let empty_staged = BTreeMap::new();
    resolve_namespace_target_row_with_staged(
        store,
        namespace,
        raw_id,
        is_twin_lookup,
        &empty_staged,
    )
    .await
}

async fn resolve_namespace_target_row_with_staged(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    raw_id: &str,
    is_twin_lookup: bool,
    staged_rows: &BTreeMap<String, MemoryPayload>,
) -> Result<Option<(String, MemoryPayload)>, ActionError> {
    use crate::traits::VectorStore;
    let now_ts = current_action_timestamp();
    if let Some(staged_payload) = staged_rows.get(raw_id) {
        if is_active_payload(staged_payload, now_ts) {
            return Ok(Some((raw_id.to_string(), staged_payload.clone())));
        }
    } else if let Some((_vec, payload)) = store
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
            .list(namespace, None, Some("digital_twin"))
            .await
            .map_err(|e| ActionError::Storage(e.to_string()))?;
        let mut best: Option<(u64, String, MemoryPayload)> = None;
        let mut consider = |id: String, payload: MemoryPayload| {
            if !is_active_payload(&payload, now_ts) {
                return;
            }
            let tid = payload
                .metadata
                .get("twin")
                .and_then(|t| t.get("id"))
                .or_else(|| {
                    payload
                        .metadata
                        .get("digital_twin")
                        .and_then(|t| t.get("twin_id").or_else(|| t.get("id")))
                })
                .and_then(|v| v.as_str());
            if tid == Some(raw_id) {
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
                let replace = match &best {
                    None => true,
                    Some((best_ver, best_id, _)) => {
                        ver > *best_ver || (ver == *best_ver && id > *best_id)
                    }
                };
                if replace {
                    best = Some((ver, id, payload));
                }
            }
        };

        for r in rows {
            if staged_rows.contains_key(&r.id) {
                continue;
            }
            consider(r.id, r.payload);
        }
        for (staged_id, staged_payload) in staged_rows {
            if staged_payload.memory_type == "digital_twin" {
                consider(staged_id.clone(), staged_payload.clone());
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
                    let expected_type =
                        spec.target_memory_type
                            .as_deref()
                            .or(if spec.param_type == "twin_id" {
                                Some("digital_twin")
                            } else {
                                None
                            });
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
        .list(&invocation.namespace, None, Some("action_type"))
        .await
    {
        if let Some(def_row) = action_rows.into_iter().rev().find(|r| {
            is_active_payload(&r.payload, now_ts)
                && ActionTypeDefinition::from_payload(&r.payload)
                    .map(|d| d.slug == definition.slug)
                    .unwrap_or(false)
        }) {
            let has_explicit_acl =
                !crate::rebac::extract_acl_with_roles(&def_row.payload.metadata).is_empty();
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

fn is_valid_placeholder_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn scalar_value_to_string(
    param_name: &str,
    val: &serde_json::Value,
) -> Result<String, ActionSubmissionFailure> {
    match val {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.to_string())
            } else if let Some(u) = n.as_u64() {
                Ok(u.to_string())
            } else if let Some(f) = n.as_f64().filter(|v| v.is_finite()) {
                Ok(f.to_string())
            } else {
                Err(ActionSubmissionFailure {
                    code: "non_finite_number".to_string(),
                    target: param_name.to_string(),
                    message: format!("template parameter '{param_name}' is non-finite"),
                })
            }
        }
        _ => Err(ActionSubmissionFailure {
            code: "non_scalar_template_parameter".to_string(),
            target: param_name.to_string(),
            message: format!("template parameter '{param_name}' must be a scalar value"),
        }),
    }
}

pub fn render_template_string(
    template: &str,
    params: &BTreeMap<String, serde_json::Value>,
) -> Result<String, ActionSubmissionFailure> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(open_idx) = rest.find("{{") {
            let prefix = &rest[..open_idx];
            if prefix.contains("}}") {
                return Err(ActionSubmissionFailure {
                    code: "invalid_template_placeholder".to_string(),
                    target: "template".to_string(),
                    message: "unmatched closing '}}' in template".to_string(),
                });
            }
            out.push_str(prefix);
            let after_open = &rest[open_idx + 2..];
            let Some(close_idx) = after_open.find("}}") else {
                return Err(ActionSubmissionFailure {
                    code: "invalid_template_placeholder".to_string(),
                    target: "template".to_string(),
                    message: "unclosed '{{' placeholder in template".to_string(),
                });
            };
            let raw_name = &after_open[..close_idx];
            if !is_valid_placeholder_ident(raw_name) {
                return Err(ActionSubmissionFailure {
                    code: "invalid_template_placeholder".to_string(),
                    target: "template".to_string(),
                    message: "template placeholder must be a bare parameter identifier".to_string(),
                });
            }
            let Some(bound) = params.get(raw_name) else {
                return Err(ActionSubmissionFailure {
                    code: "unbound_template_parameter".to_string(),
                    target: raw_name.to_string(),
                    message: format!("unbound template parameter '{raw_name}'"),
                });
            };
            let rendered = scalar_value_to_string(raw_name, bound)?;
            if out.len().saturating_add(rendered.len()) > MAX_TEMPLATE_RENDER_BYTES {
                return Err(ActionSubmissionFailure {
                    code: "template_output_too_large".to_string(),
                    target: "template".to_string(),
                    message: "rendered template exceeds 1 MiB limit".to_string(),
                });
            }
            out.push_str(&rendered);
            rest = &after_open[close_idx + 2..];
        } else {
            if rest.contains("}}") {
                return Err(ActionSubmissionFailure {
                    code: "invalid_template_placeholder".to_string(),
                    target: "template".to_string(),
                    message: "unmatched closing '}}' in template".to_string(),
                });
            }
            if out.len().saturating_add(rest.len()) > MAX_TEMPLATE_RENDER_BYTES {
                return Err(ActionSubmissionFailure {
                    code: "template_output_too_large".to_string(),
                    target: "template".to_string(),
                    message: "rendered template exceeds 1 MiB limit".to_string(),
                });
            }
            out.push_str(rest);
            break;
        }
    }
    Ok(out)
}

pub fn render_template_value(
    template: &serde_json::Value,
    params: &BTreeMap<String, serde_json::Value>,
) -> Result<serde_json::Value, ActionSubmissionFailure> {
    let val = render_template_value_inner(template, params)?;
    let encoded_len = serde_json::to_vec(&val)
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    if encoded_len > MAX_TEMPLATE_RENDER_BYTES {
        return Err(ActionSubmissionFailure {
            code: "template_output_too_large".to_string(),
            target: "metadata".to_string(),
            message: "rendered metadata exceeds 1 MiB limit".to_string(),
        });
    }
    Ok(val)
}

fn render_template_value_inner(
    template: &serde_json::Value,
    params: &BTreeMap<String, serde_json::Value>,
) -> Result<serde_json::Value, ActionSubmissionFailure> {
    match template {
        serde_json::Value::String(s) => {
            if let Some(inner) = s.strip_prefix("{{").and_then(|r| r.strip_suffix("}}")) {
                if !inner.contains("{{") && !inner.contains("}}") {
                    if !is_valid_placeholder_ident(inner) {
                        return Err(ActionSubmissionFailure {
                            code: "invalid_template_placeholder".to_string(),
                            target: "template".to_string(),
                            message: "template placeholder must be a bare parameter identifier"
                                .to_string(),
                        });
                    }
                    let Some(bound) = params.get(inner) else {
                        return Err(ActionSubmissionFailure {
                            code: "unbound_template_parameter".to_string(),
                            target: inner.to_string(),
                            message: format!("unbound template parameter '{inner}'"),
                        });
                    };
                    // Validate scalar & finite before returning exact JSON type
                    let _ = scalar_value_to_string(inner, bound)?;
                    return Ok(bound.clone());
                }
            }
            Ok(serde_json::Value::String(render_template_string(
                s, params,
            )?))
        }
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(render_template_value_inner(item, params)?);
            }
            Ok(serde_json::Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if k.contains("{{") || k.contains("}}") {
                    return Err(ActionSubmissionFailure {
                        code: "template_key_not_allowed".to_string(),
                        target: "metadata".to_string(),
                        message: "template placeholders are not allowed in JSON object keys"
                            .to_string(),
                    });
                }
                out.insert(k.clone(), render_template_value_inner(v, params)?);
            }
            Ok(serde_json::Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

pub fn deterministic_upsert_id(
    namespace: &str,
    action_slug: &str,
    params: &BTreeMap<String, serde_json::Value>,
    effect_index: usize,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update(b"\0");
    hasher.update(action_slug.as_bytes());
    hasher.update(b"\0");
    let canonical_params = serde_json::to_string(params).unwrap_or_default();
    hasher.update(canonical_params.as_bytes());
    hasher.update(b"\0");
    hasher.update(effect_index.to_string().as_bytes());
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("act-upsert-{}", &digest[..24])
}

fn deterministic_effect_hash(
    namespace: &str,
    action_slug: &str,
    params: &BTreeMap<String, serde_json::Value>,
    effect_index: usize,
    now_ts: i64,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update(b"\0");
    hasher.update(action_slug.as_bytes());
    hasher.update(b"\0");
    let canonical_params = serde_json::to_string(params).unwrap_or_default();
    hasher.update(canonical_params.as_bytes());
    hasher.update(b"\0");
    hasher.update(effect_index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(now_ts.to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn record_expected_active_row(
    expected_heads: &mut Vec<ExpectedHead>,
    seen_row_ids: &mut BTreeSet<String>,
    row_id: &str,
    payload: &MemoryPayload,
) {
    if !seen_row_ids.insert(row_id.to_string()) {
        return;
    }
    let twin_id = payload
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
        .map(|s| s.to_string());
    let expected_twin_version = payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("version"))
        .or_else(|| {
            payload
                .metadata
                .get("digital_twin")
                .and_then(|t| t.get("version"))
        })
        .and_then(|v| v.as_u64());
    let expected_task_status = payload
        .metadata
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expected_updated_at = payload
        .metadata
        .get("task")
        .and_then(|t| t.get("updated_at"))
        .and_then(|v| v.as_i64());

    expected_heads.push(ExpectedHead::ActiveRow {
        row_id: row_id.to_string(),
        twin_id,
        expected_twin_version,
        expected_task_status,
        expected_updated_at,
    });
}

fn stage_payload(
    staged_rows: &mut BTreeMap<String, MemoryPayload>,
    staged_order: &mut Vec<String>,
    row_id: String,
    payload: MemoryPayload,
) {
    if !staged_rows.contains_key(&row_id) {
        staged_order.push(row_id.clone());
    }
    staged_rows.insert(row_id, payload);
}

pub async fn stage_action(
    store: &crate::store::ladybug::LadybugStore,
    eval: ActionEvaluation,
    embedder: &dyn crate::traits::Embedder,
    now_ts: i64,
) -> Result<ActionStage, ActionError> {
    use crate::traits::VectorStore;

    if !eval.submission_failures.is_empty() {
        return Err(ActionError::SubmissionRejected(eval.submission_failures));
    }

    let mut submission_failures: Vec<ActionSubmissionFailure> = Vec::new();
    let mut current_targets = eval.targets.clone();
    let mut expected_heads: Vec<ExpectedHead> = Vec::new();
    let mut seen_expected_rows: BTreeSet<String> = BTreeSet::new();
    let mut staged_rows: BTreeMap<String, MemoryPayload> = BTreeMap::new();
    let mut staged_order: Vec<String> = Vec::new();

    let mut superseded_pairs: Vec<SupersededPair> = Vec::new();
    let mut created_ids: Vec<String> = Vec::new();
    let mut twin_deltas: Vec<ActionTwinDelta> = Vec::new();
    let mut edge_deltas: Vec<EdgeDelta> = Vec::new();
    let mut row_deltas: Vec<RowDelta> = Vec::new();
    let mut task_deltas: Vec<TaskDelta> = Vec::new();

    for (effect_idx, effect) in eval.definition.effects.iter().enumerate() {
        let effect_target = format!("effects[{effect_idx}]");
        match effect {
            ActionEffectTemplate::SetTwinState {
                twin_param,
                merge,
                variables_from_params,
            } => {
                let Some(target_rec) = current_targets.get(twin_param).cloned() else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "target_not_found".to_string(),
                        target: effect_target,
                        message: format!("unresolved twin parameter '{twin_param}'"),
                    });
                    continue;
                };
                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &target_rec.row_id,
                    &target_rec.payload,
                );

                let mut new_vars = BTreeMap::new();
                for (var_name, param_name) in variables_from_params {
                    let Some(val) = eval.bound_params.get(param_name) else {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "unbound_template_parameter".to_string(),
                            target: effect_target.clone(),
                            message: format!(
                                "unbound parameter '{param_name}' for twin variable '{var_name}'"
                            ),
                        });
                        continue;
                    };
                    let Some(f) = val.as_f64().filter(|v| v.is_finite()) else {
                        submission_failures.push(ActionSubmissionFailure {
                            code: "non_finite_number".to_string(),
                            target: effect_target.clone(),
                            message: format!(
                                "parameter '{param_name}' for twin variable '{var_name}' must be finite"
                            ),
                        });
                        continue;
                    };
                    new_vars.insert(var_name.clone(), f);
                }
                if !submission_failures.is_empty() {
                    continue;
                }

                let before_row_id = target_rec.row_id.clone();
                let before_payload = target_rec.payload.clone();
                let source_tag = format!("action:{}", eval.definition.slug);
                let (twin_id, before_version, after_version, next_payload, deltas) =
                    crate::digital_twin::stage_twin_mutation(
                        &before_row_id,
                        &before_payload,
                        &new_vars,
                        *merge,
                        Some(&source_tag),
                        now_ts,
                    );

                let hash_suffix = &deterministic_effect_hash(
                    &eval.namespace,
                    &eval.definition.slug,
                    &eval.bound_params,
                    effect_idx,
                    now_ts,
                )[..12];
                let after_row_id = format!("twin-{twin_id}-v{after_version}-{hash_suffix}");
                seen_expected_rows.insert(after_row_id.clone());
                expected_heads.push(ExpectedHead::AbsentOrInactive {
                    row_id: after_row_id.clone(),
                });

                let mut retired_before = before_payload.clone();
                if !retired_before.metadata.is_object() {
                    retired_before.metadata = serde_json::json!({});
                }
                if let Some(meta) = retired_before.metadata.as_object_mut() {
                    meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
                    meta.insert("superseded_by".to_string(), serde_json::json!(after_row_id));
                }

                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    before_row_id.clone(),
                    retired_before,
                );
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    after_row_id.clone(),
                    next_payload.clone(),
                );

                superseded_pairs.push(SupersededPair {
                    before_id: before_row_id.clone(),
                    after_id: after_row_id.clone(),
                });
                if !created_ids.contains(&after_row_id) {
                    created_ids.push(after_row_id.clone());
                }
                for d in deltas {
                    twin_deltas.push(ActionTwinDelta {
                        twin_id: twin_id.clone(),
                        before_row_id: before_row_id.clone(),
                        after_row_id: after_row_id.clone(),
                        before_version,
                        after_version,
                        name: d.name,
                        old: d.old,
                        new: d.new,
                    });
                }
                row_deltas.push(RowDelta {
                    kind: "set_twin_state".to_string(),
                    before_id: Some(before_row_id),
                    before_payload: Some(before_payload),
                    after_id: after_row_id.clone(),
                    after_payload: next_payload.clone(),
                    created: true,
                });
                current_targets.insert(
                    twin_param.clone(),
                    ActionTargetRecord {
                        param_name: twin_param.clone(),
                        row_id: after_row_id,
                        payload: next_payload,
                    },
                );
            }
            ActionEffectTemplate::SupersedeMemory {
                target_param,
                new_content_template,
                metadata_patch,
            } => {
                let Some(target_rec) = current_targets.get(target_param).cloned() else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "target_not_found".to_string(),
                        target: effect_target,
                        message: format!("unresolved target parameter '{target_param}'"),
                    });
                    continue;
                };
                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &target_rec.row_id,
                    &target_rec.payload,
                );

                let new_content =
                    match render_template_string(new_content_template, &eval.bound_params) {
                        Ok(c) => c,
                        Err(mut f) => {
                            f.target = effect_target;
                            submission_failures.push(f);
                            continue;
                        }
                    };
                let patch_val = match render_template_value(metadata_patch, &eval.bound_params) {
                    Ok(v) => v,
                    Err(mut f) => {
                        f.target = effect_target;
                        submission_failures.push(f);
                        continue;
                    }
                };

                let before_id = target_rec.row_id.clone();
                let before_payload = target_rec.payload.clone();
                let hash_suffix = &deterministic_effect_hash(
                    &eval.namespace,
                    &eval.definition.slug,
                    &eval.bound_params,
                    effect_idx,
                    now_ts,
                )[..16];
                let after_id = format!("act-sup-{hash_suffix}");
                seen_expected_rows.insert(after_id.clone());
                expected_heads.push(ExpectedHead::AbsentOrInactive {
                    row_id: after_id.clone(),
                });

                let mut retired_before = before_payload.clone();
                if !retired_before.metadata.is_object() {
                    retired_before.metadata = serde_json::json!({});
                }
                if let Some(meta) = retired_before.metadata.as_object_mut() {
                    meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
                    meta.insert("superseded_by".to_string(), serde_json::json!(after_id));
                }

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

                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    before_id.clone(),
                    retired_before,
                );
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    after_id.clone(),
                    after_payload.clone(),
                );

                superseded_pairs.push(SupersededPair {
                    before_id: before_id.clone(),
                    after_id: after_id.clone(),
                });
                if !created_ids.contains(&after_id) {
                    created_ids.push(after_id.clone());
                }
                row_deltas.push(RowDelta {
                    kind: "supersede_memory".to_string(),
                    before_id: Some(before_id),
                    before_payload: Some(before_payload),
                    after_id: after_id.clone(),
                    after_payload: after_payload.clone(),
                    created: true,
                });
                current_targets.insert(
                    target_param.clone(),
                    ActionTargetRecord {
                        param_name: target_param.clone(),
                        row_id: after_id,
                        payload: after_payload,
                    },
                );
            }
            ActionEffectTemplate::UpsertMemory {
                memory_type,
                content_template,
                metadata_template,
            } => {
                let content = match render_template_string(content_template, &eval.bound_params) {
                    Ok(c) => c,
                    Err(mut f) => {
                        f.target = effect_target;
                        submission_failures.push(f);
                        continue;
                    }
                };
                let mut metadata =
                    match render_template_value(metadata_template, &eval.bound_params) {
                        Ok(v) => v,
                        Err(mut f) => {
                            f.target = effect_target;
                            submission_failures.push(f);
                            continue;
                        }
                    };
                if !metadata.is_object() {
                    metadata = serde_json::json!({});
                }
                if let Some(meta_obj) = metadata.as_object_mut() {
                    meta_obj
                        .entry("valid_from")
                        .or_insert_with(|| serde_json::json!(now_ts));
                }
                if let Some(topology) = metadata.get_mut("topology") {
                    crate::rdf::canonicalize_topology(
                        topology,
                        &store.get_ontology(&eval.namespace),
                    );
                }
                crate::domain::materialize_facets_bitemporal(&memory_type, &mut metadata, now_ts);

                let upsert_id = deterministic_upsert_id(
                    &eval.namespace,
                    &eval.definition.slug,
                    &eval.bound_params,
                    effect_idx,
                );
                let existing_active = if let Some(staged_prev) = staged_rows.get(&upsert_id) {
                    if is_active_payload(staged_prev, now_ts) {
                        Some(staged_prev.clone())
                    } else {
                        None
                    }
                } else {
                    store
                        .get(&eval.namespace, &upsert_id)
                        .await
                        .map_err(|e| ActionError::Storage(e.to_string()))?
                        .and_then(|(_vec, p)| {
                            if is_active_payload(&p, now_ts) {
                                Some(p)
                            } else {
                                None
                            }
                        })
                };

                let created = existing_active.is_none();
                if let Some(ref prev_payload) = existing_active {
                    record_expected_active_row(
                        &mut expected_heads,
                        &mut seen_expected_rows,
                        &upsert_id,
                        prev_payload,
                    );
                } else if seen_expected_rows.insert(upsert_id.clone()) {
                    expected_heads.push(ExpectedHead::AbsentOrInactive {
                        row_id: upsert_id.clone(),
                    });
                }

                if created && !created_ids.contains(&upsert_id) {
                    created_ids.push(upsert_id.clone());
                }

                let after_payload = MemoryPayload {
                    content,
                    user_id: eval.effective_principal.clone(),
                    memory_type: memory_type.clone(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata,
                };
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    upsert_id.clone(),
                    after_payload.clone(),
                );
                row_deltas.push(RowDelta {
                    kind: "upsert_memory".to_string(),
                    before_id: existing_active.as_ref().map(|_| upsert_id.clone()),
                    before_payload: existing_active,
                    after_id: upsert_id,
                    after_payload,
                    created,
                });
            }
            ActionEffectTemplate::LinkEdge {
                source_param,
                relation,
                target_param,
            } => {
                let Some(src_rec) = current_targets.get(source_param).cloned() else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "target_not_found".to_string(),
                        target: effect_target,
                        message: format!("unresolved source parameter '{source_param}'"),
                    });
                    continue;
                };
                let Some(dst_rec) = current_targets.get(target_param).cloned() else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "target_not_found".to_string(),
                        target: effect_target,
                        message: format!("unresolved target parameter '{target_param}'"),
                    });
                    continue;
                };
                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &src_rec.row_id,
                    &src_rec.payload,
                );
                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &dst_rec.row_id,
                    &dst_rec.payload,
                );

                if !crate::rdf::shacl::endpoint_role_allowed(
                    relation,
                    &src_rec.payload.memory_type,
                    &dst_rec.payload.memory_type,
                ) {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "shacl_violation".to_string(),
                        target: effect_target,
                        message: format!(
                            "edge relation '{}' is not allowed from '{}' to '{}'",
                            relation, src_rec.payload.memory_type, dst_rec.payload.memory_type
                        ),
                    });
                    continue;
                }

                let existed_before = store
                    .has_edge(&eval.namespace, &src_rec.row_id, relation, &dst_rec.row_id)
                    .await
                    .map_err(|e| ActionError::Storage(e.to_string()))?;

                expected_heads.push(ExpectedHead::EdgeState {
                    source: src_rec.row_id.clone(),
                    relation: relation.clone(),
                    target: dst_rec.row_id.clone(),
                    expected_exists: existed_before,
                });
                edge_deltas.push(EdgeDelta {
                    source: src_rec.row_id,
                    relation: relation.clone(),
                    target: dst_rec.row_id,
                    existed_before,
                    op: EdgeMutationOp::EnsurePresent,
                });
            }
            ActionEffectTemplate::TransitionTask {
                target_param,
                new_status,
                note_template,
            } => {
                let Some(target_rec) = current_targets.get(target_param).cloned() else {
                    submission_failures.push(ActionSubmissionFailure {
                        code: "target_not_found".to_string(),
                        target: effect_target,
                        message: format!("unresolved task parameter '{target_param}'"),
                    });
                    continue;
                };
                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &target_rec.row_id,
                    &target_rec.payload,
                );

                let note = match render_template_string(note_template, &eval.bound_params) {
                    Ok(n) => n,
                    Err(mut f) => {
                        f.target = effect_target;
                        submission_failures.push(f);
                        continue;
                    }
                };

                let session_tag = format!("action:{}", eval.definition.slug);
                let (before_status, after_status, after_payload) =
                    match crate::task::stage_task_transition_at(
                        &target_rec.row_id,
                        &target_rec.payload,
                        new_status,
                        &note,
                        &eval.effective_principal,
                        &session_tag,
                        now_ts,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            submission_failures.push(ActionSubmissionFailure {
                                code: "task_transition_failed".to_string(),
                                target: effect_target,
                                message: err,
                            });
                            continue;
                        }
                    };

                let task_id = target_rec.row_id.clone();
                let before_payload = target_rec.payload.clone();
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    task_id.clone(),
                    after_payload.clone(),
                );
                task_deltas.push(TaskDelta {
                    task_id: task_id.clone(),
                    before_status,
                    after_status,
                    before_payload: before_payload.clone(),
                    after_payload: after_payload.clone(),
                });
                row_deltas.push(RowDelta {
                    kind: "transition_task".to_string(),
                    before_id: Some(task_id.clone()),
                    before_payload: Some(before_payload),
                    after_id: task_id.clone(),
                    after_payload: after_payload.clone(),
                    created: false,
                });
                current_targets.insert(
                    target_param.clone(),
                    ActionTargetRecord {
                        param_name: target_param.clone(),
                        row_id: task_id,
                        payload: after_payload,
                    },
                );
            }
        }
    }

    if !submission_failures.is_empty() {
        return Err(ActionError::SubmissionRejected(submission_failures));
    }

    let receipt_hash = &deterministic_effect_hash(
        &eval.namespace,
        &eval.definition.slug,
        &eval.bound_params,
        usize::MAX,
        now_ts,
    )[..20];
    let receipt_id = format!("act-receipt-{receipt_hash}");
    if seen_expected_rows.insert(receipt_id.clone()) {
        expected_heads.push(ExpectedHead::AbsentOrInactive {
            row_id: receipt_id.clone(),
        });
    }

    let (target_pointer_id, target_pool_id) = connector_target_binding(&eval.bound_params);
    let receipt = ActionReceiptPayload {
        receipt_id: receipt_id.clone(),
        action_slug: eval.definition.slug.clone(),
        principal: eval.effective_principal.clone(),
        executed_at: now_ts,
        params: eval.bound_params.clone(),
        target_pointer_id,
        target_pool_id,
        superseded_pairs,
        created_ids,
        twin_deltas,
        edge_deltas: edge_deltas.clone(),
        row_deltas,
        task_deltas,
        reverted_at: None,
        reverted_by_receipt_id: None,
        reverts_receipt_id: None,
        forced_by: None,
        forced_at: None,
    };

    let receipt_payload = MemoryPayload {
        content: format!(
            "Action receipt '{}' for '{}' by '{}' at {}",
            receipt_id, eval.definition.slug, eval.effective_principal, now_ts
        ),
        user_id: eval.effective_principal.clone(),
        memory_type: "action_receipt".to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: serde_json::json!({
            "eligibility": "private",
            "valid_from": now_ts,
            "action_receipt": receipt
        }),
    };
    stage_payload(
        &mut staged_rows,
        &mut staged_order,
        receipt_id.clone(),
        receipt_payload,
    );

    // Run strict SHACL + custom shapes + reasoning validation on every staged active post-image and receipt.
    let ont = store.get_ontology(&eval.namespace);
    for row_id in &staged_order {
        let Some(payload) = staged_rows.get(row_id) else {
            continue;
        };
        if !is_active_payload(payload, now_ts) {
            continue;
        }
        for v in ont.validate_strict_post_image(payload) {
            submission_failures.push(ActionSubmissionFailure {
                code: "shacl_violation".to_string(),
                target: format!("{row_id}:{}", v.field),
                message: v.why,
            });
        }
    }

    let gate_decision = crate::formal_kernel::evaluate_action_gate(
        !submission_failures.iter().any(|f| f.code == "rebac_denied"),
        !submission_failures.iter().any(|f| {
            matches!(
                f.code.as_str(),
                "missing_parameter"
                    | "unknown_parameter"
                    | "invalid_parameter"
                    | "target_not_found"
                    | "template_error"
                    | "task_transition_failed"
            )
        }),
        !submission_failures
            .iter()
            .any(|f| f.code == "sparql_ask_failed" || f.code == "sparql_ask_error"),
        !submission_failures
            .iter()
            .any(|f| f.code == "twin_precondition_failed"),
        !submission_failures
            .iter()
            .any(|f| f.code == "shacl_violation"),
    );
    if gate_decision != crate::formal_kernel::ActionGateDecision::Allow
        || !submission_failures.is_empty()
    {
        return Err(ActionError::SubmissionRejected(submission_failures));
    }

    // Compute embeddings for all staged rows before any write turn.
    let texts: Vec<String> = staged_order
        .iter()
        .filter_map(|id| staged_rows.get(id).map(|p| p.content.clone()))
        .collect();
    let vectors = embedder
        .embed_batch(&texts)
        .await
        .map_err(|e| ActionError::Storage(format!("embedding failed: {e}")))?;

    let mut writes = Vec::with_capacity(staged_order.len());
    for (row_id, vector) in staged_order.into_iter().zip(vectors.into_iter()) {
        if let Some(payload) = staged_rows.remove(&row_id) {
            writes.push(StagedMemoryWrite {
                id: row_id,
                vector,
                payload,
            });
        }
    }

    Ok(ActionStage {
        namespace: eval.namespace,
        receipt_id,
        receipt,
        write_set: ActionWriteSet {
            expected_heads,
            writes,
            edge_deltas,
        },
        submission_failures: Vec::new(),
    })
}

struct ValidationNoopEmbedder;

#[async_trait::async_trait]
impl crate::traits::Embedder for ValidationNoopEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.0])
    }

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![vec![0.0]; texts.len()])
    }

    fn dimensions(&self) -> usize {
        1
    }
}

pub async fn validate_action(
    store: &crate::store::ladybug::LadybugStore,
    invocation: &ActionInvocation,
) -> Result<ActionValidateOutcome, ActionError> {
    let eval = evaluate_action(store, invocation).await?;
    let namespace = eval.namespace.clone();
    let action_slug = eval.definition.slug.clone();
    let effective_principal = eval.effective_principal.clone();
    if !eval.submission_failures.is_empty() {
        return Ok(ActionValidateOutcome {
            valid: false,
            namespace,
            action_slug,
            effective_principal,
            failures: eval.submission_failures,
        });
    }

    let now_ts = current_action_timestamp();
    match stage_action(store, eval, &ValidationNoopEmbedder, now_ts).await {
        Ok(_) => Ok(ActionValidateOutcome {
            valid: true,
            namespace,
            action_slug,
            effective_principal,
            failures: Vec::new(),
        }),
        Err(ActionError::SubmissionRejected(failures)) => Ok(ActionValidateOutcome {
            valid: false,
            namespace,
            action_slug,
            effective_principal,
            failures,
        }),
        Err(other) => Err(other),
    }
}

pub async fn apply_action(
    store: &crate::store::ladybug::LadybugStore,
    invocation: &ActionInvocation,
    embedder: &dyn crate::traits::Embedder,
) -> Result<ActionApplyOutcome, ActionError> {
    // Handlers hold the per-task lock across their load -> save sequence. Take
    // the same locks here, in id order, across evaluate -> stage -> commit so
    // a handler cannot save a stale payload over a committed action's status
    // or history. The target ids are known from the caller's named parameters
    // before the read-only evaluation, so the lock is held for the whole
    // window in which a stale load could race the commit.
    let mut task_ids = std::collections::BTreeSet::new();
    {
        let mut ont = store.get_ontology(&invocation.namespace);
        if !ont.action_types.contains_key(&invocation.slug) {
            store.refresh_ontology(&invocation.namespace).await;
            ont = store.get_ontology(&invocation.namespace);
        }
        if let Some(definition) = ont.action_types.get(&invocation.slug) {
            for effect in &definition.effects {
                if let ActionEffectTemplate::TransitionTask { target_param, .. } = effect {
                    if let Some(serde_json::Value::String(id)) = invocation.params.get(target_param)
                    {
                        task_ids.insert(id.clone());
                    }
                }
            }
        }
    }
    let mut locks = Vec::new();
    for id in &task_ids {
        locks.push(crate::task::claim_lock(id));
    }
    let mut _held = Vec::with_capacity(locks.len());
    for lock in &locks {
        _held.push(lock.lock().await);
    }

    let eval = evaluate_action(store, invocation).await?;
    if !eval.submission_failures.is_empty() {
        return Ok(ActionApplyOutcome::Rejected {
            failures: eval.submission_failures,
        });
    }

    let now_ts = next_action_timestamp();
    let stage = match stage_action(store, eval, embedder, now_ts).await {
        Ok(s) => s,
        Err(ActionError::SubmissionRejected(failures)) => {
            return Ok(ActionApplyOutcome::Rejected { failures });
        }
        Err(other) => return Err(other),
    };

    store
        .commit_action_batch(&invocation.namespace, stage.write_set)
        .await?;

    let outcome = ActionApplyOutcome::Applied {
        receipt_id: stage.receipt_id,
        receipt: stage.receipt,
    };
    // Capability E: post-COMMIT learning hook. Runs only after the action batch
    // is durable; a revert of this receipt is the only thing that flips the
    // reinforcement to negative. Failures are logged inside the hook and never
    // roll back the committed action. No recursion: the hook only reads rows
    // and stages its own memory writes.
    crate::learning::hooks::on_action_applied(
        store,
        &invocation.namespace,
        &outcome,
        &crate::daemon::resolve_project_root(),
    )
    .await;
    Ok(outcome)
}

async fn follow_superseded_chain(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    start_id: &str,
    start_payload: MemoryPayload,
) -> Result<(String, MemoryPayload), ActionError> {
    use crate::traits::VectorStore;
    let mut cur_id = start_id.to_string();
    let mut cur_payload = start_payload;
    for _ in 0..32 {
        let Some(next_id) = cur_payload
            .metadata
            .get("superseded_by")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
        else {
            break;
        };
        let Some((_vec, next_payload)) = store
            .get(namespace, &next_id)
            .await
            .map_err(|e| ActionError::Storage(e.to_string()))?
        else {
            break;
        };
        cur_id = next_id;
        cur_payload = next_payload;
    }
    Ok((cur_id, cur_payload))
}

pub async fn revert_action(
    store: &crate::store::ladybug::LadybugStore,
    invocation: &RevertInvocation,
    embedder: &dyn crate::traits::Embedder,
) -> Result<ActionRevertOutcome, ActionError> {
    use crate::traits::VectorStore;

    let effective_principal = invocation.security.effective_principal();
    if effective_principal.trim().is_empty() {
        return Err(ActionError::PermissionDenied(
            "anonymous principal cannot revert actions".to_string(),
        ));
    }

    // 1. Locate initial receipt row in the action namespace and follow superseded_by to the active receipt version.
    let initial_row = match store
        .get(&invocation.namespace, &invocation.receipt_id)
        .await
        .map_err(|e| ActionError::Storage(e.to_string()))?
    {
        Some((_vec, payload)) if payload.memory_type == "action_receipt" => {
            Some((invocation.receipt_id.clone(), payload))
        }
        _ => {
            let rows = store
                .list(&invocation.namespace, None, Some("action_receipt"))
                .await
                .map_err(|e| ActionError::Storage(e.to_string()))?;
            rows.into_iter().find_map(|r| {
                let matches_logical = r
                    .payload
                    .metadata
                    .get("action_receipt")
                    .and_then(|ar| ar.get("receipt_id"))
                    .and_then(|v| v.as_str())
                    == Some(invocation.receipt_id.as_str());
                if matches_logical {
                    Some((r.id, r.payload))
                } else {
                    None
                }
            })
        }
    };

    let Some((initial_row_id, initial_payload)) = initial_row else {
        return Err(ActionError::ReceiptNotFound {
            namespace: invocation.namespace.clone(),
            receipt_id: invocation.receipt_id.clone(),
        });
    };

    let (active_receipt_row_id, active_receipt_payload) = follow_superseded_chain(
        store,
        &invocation.namespace,
        &initial_row_id,
        initial_payload,
    )
    .await?;

    let active_receipt = ActionReceiptPayload::from_payload(&active_receipt_payload)
        .map_err(ActionError::Storage)?;

    let receipt_state = if active_receipt.reverted_by_receipt_id.is_some()
        || active_receipt.reverted_at.is_some()
    {
        crate::formal_kernel::ActionReceiptState::AlreadyReverted
    } else {
        crate::formal_kernel::ActionReceiptState::Active
    };
    if matches!(
        crate::formal_kernel::evaluate_action_revert(
            receipt_state,
            true,
            invocation.force,
            true,
            true,
        ),
        crate::formal_kernel::ActionRevertDecision::RejectAlreadyReverted
    ) {
        return Err(ActionError::AlreadyReverted {
            receipt_id: active_receipt.receipt_id.clone(),
            reverted_by_receipt_id: active_receipt
                .reverted_by_receipt_id
                .clone()
                .unwrap_or_else(|| "reverted".to_string()),
        });
    }

    let now_ts = next_action_timestamp();
    if !is_active_payload(&active_receipt_payload, now_ts) {
        return Err(ActionError::ConflictWithNewerVersion {
            receipt_id: active_receipt.receipt_id.clone(),
            superseded_by: "retired".to_string(),
        });
    }

    // 2. Determine required role from action definition and check authorization.
    if !store
        .get_ontology(&invocation.namespace)
        .action_types
        .contains_key(&active_receipt.action_slug)
    {
        store.refresh_ontology(&invocation.namespace).await;
    }
    let ont = store.get_ontology(&invocation.namespace);
    let def_role = ont
        .action_types
        .get(&active_receipt.action_slug)
        .map(|d| d.required_role.as_str())
        .unwrap_or("editor");

    let is_admin = invocation.security.authenticated_principal.trim() == "admin"
        && effective_principal == "admin";
    let required_role = if invocation.force { "owner" } else { def_role };

    let check_target_auth = |payload: &MemoryPayload, label: &str| -> Result<(), ActionError> {
        if is_admin {
            return Ok(());
        }
        let acl = crate::rebac::extract_acl_with_roles(&payload.metadata);
        if crate::rebac::allowed_with_roles(
            &effective_principal,
            &payload.user_id,
            required_role,
            &acl,
        ) {
            return Ok(());
        }
        Err(ActionError::PermissionDenied(format!(
            "principal '{effective_principal}' lacks '{required_role}' role on {label}"
        )))
    };

    // Verify receipt access when there are no row/edge deltas, or check every target below.
    if active_receipt.row_deltas.is_empty() && active_receipt.edge_deltas.is_empty() {
        check_target_auth(&active_receipt_payload, &active_receipt.receipt_id)?;
    }

    let mut expected_heads: Vec<ExpectedHead> = Vec::new();
    let mut seen_expected_rows: BTreeSet<String> = BTreeSet::new();
    let mut staged_rows: BTreeMap<String, MemoryPayload> = BTreeMap::new();
    let mut staged_order: Vec<String> = Vec::new();

    record_expected_active_row(
        &mut expected_heads,
        &mut seen_expected_rows,
        &active_receipt_row_id,
        &active_receipt_payload,
    );

    let mut revert_params_seed = BTreeMap::new();
    revert_params_seed.insert(
        "reverts_receipt_id".to_string(),
        serde_json::json!(active_receipt.receipt_id),
    );
    let revert_master_hash = deterministic_effect_hash(
        &invocation.namespace,
        &active_receipt.action_slug,
        &revert_params_seed,
        usize::MAX - 1,
        now_ts,
    );
    let revert_receipt_id = format!("act-revert-{}", &revert_master_hash[..20]);
    let superseded_apply_row_id = format!(
        "{}-rev-{}",
        active_receipt.receipt_id,
        &revert_master_hash[20..32]
    );

    let mut revert_superseded_pairs: Vec<SupersededPair> = Vec::new();
    let mut revert_created_ids: Vec<String> = Vec::new();
    let mut revert_twin_deltas: Vec<ActionTwinDelta> = Vec::new();
    let mut revert_edge_deltas: Vec<EdgeDelta> = Vec::new();
    let mut revert_row_deltas: Vec<RowDelta> = Vec::new();
    let mut revert_task_deltas: Vec<TaskDelta> = Vec::new();

    // 3. Stage inverse edge mutations in reverse order.
    for ed in active_receipt.edge_deltas.iter().rev() {
        if let Some((_v, src_p)) = store
            .get(&invocation.namespace, &ed.source)
            .await
            .map_err(|e| ActionError::Storage(e.to_string()))?
        {
            check_target_auth(&src_p, &ed.source)?;
        }
        if let Some((_v, dst_p)) = store
            .get(&invocation.namespace, &ed.target)
            .await
            .map_err(|e| ActionError::Storage(e.to_string()))?
        {
            check_target_auth(&dst_p, &ed.target)?;
        }

        let exists_now = store
            .has_edge(&invocation.namespace, &ed.source, &ed.relation, &ed.target)
            .await
            .map_err(|e| ActionError::Storage(e.to_string()))?;

        if !invocation.force && !exists_now {
            return Err(ActionError::ConflictWithNewerVersion {
                receipt_id: active_receipt.receipt_id.clone(),
                superseded_by: format!(
                    "edge_missing:{}-[{}]->{}",
                    ed.source, ed.relation, ed.target
                ),
            });
        }

        if !ed.existed_before {
            if !invocation.force {
                expected_heads.push(ExpectedHead::EdgeState {
                    source: ed.source.clone(),
                    relation: ed.relation.clone(),
                    target: ed.target.clone(),
                    expected_exists: true,
                });
            }
            revert_edge_deltas.push(EdgeDelta {
                source: ed.source.clone(),
                relation: ed.relation.clone(),
                target: ed.target.clone(),
                existed_before: exists_now,
                op: EdgeMutationOp::Remove,
            });
        } else if !invocation.force {
            expected_heads.push(ExpectedHead::EdgeState {
                source: ed.source.clone(),
                relation: ed.relation.clone(),
                target: ed.target.clone(),
                expected_exists: true,
            });
        }
    }

    // 4. Stage inverse row mutations in reverse effect order.
    struct StagedTwinRevertChain {
        expected_prev_after_id: Option<String>,
        superseded_by_after_id: String,
        staged_head_row_id: String,
    }
    let mut staged_twin_reverts: BTreeMap<String, StagedTwinRevertChain> = BTreeMap::new();

    for (rev_idx, delta) in active_receipt.row_deltas.iter().rev().enumerate() {
        let step_hash = deterministic_effect_hash(
            &invocation.namespace,
            &active_receipt.action_slug,
            &revert_params_seed,
            rev_idx,
            now_ts,
        );

        match delta.kind.as_str() {
            "set_twin_state" => {
                let twin_id = delta
                    .after_payload
                    .metadata
                    .get("twin")
                    .and_then(|t| t.get("id"))
                    .or_else(|| {
                        delta
                            .after_payload
                            .metadata
                            .get("digital_twin")
                            .and_then(|t| t.get("twin_id").or_else(|| t.get("id")))
                    })
                    .and_then(|v| v.as_str())
                    .unwrap_or(&delta.after_id)
                    .to_string();
                let expected_after_ver = delta
                    .after_payload
                    .metadata
                    .get("twin")
                    .and_then(|t| t.get("version"))
                    .or_else(|| {
                        delta
                            .after_payload
                            .metadata
                            .get("digital_twin")
                            .and_then(|t| t.get("version"))
                    })
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                let after_row_opt = if let Some(staged) = staged_rows.get(&delta.after_id) {
                    Some(staged.clone())
                } else {
                    store
                        .get(&invocation.namespace, &delta.after_id)
                        .await
                        .map_err(|e| ActionError::Storage(e.to_string()))?
                        .map(|(_v, p)| p)
                };
                let active_twin_opt = resolve_namespace_target_row_with_staged(
                    store,
                    &invocation.namespace,
                    &twin_id,
                    true,
                    &staged_rows,
                )
                .await?;

                let Some((head_row_id, head_payload)) = active_twin_opt else {
                    return Err(ActionError::ConflictWithNewerVersion {
                        receipt_id: active_receipt.receipt_id.clone(),
                        superseded_by: "missing_twin".to_string(),
                    });
                };

                check_target_auth(&head_payload, &twin_id)?;
                if let Some(ref before_p) = delta.before_payload {
                    check_target_auth(before_p, &twin_id)?;
                }

                let head_version = head_payload
                    .metadata
                    .get("twin")
                    .and_then(|t| t.get("version"))
                    .or_else(|| {
                        head_payload
                            .metadata
                            .get("digital_twin")
                            .and_then(|t| t.get("version"))
                    })
                    .and_then(|v| v.as_u64())
                    .unwrap_or(expected_after_ver);

                if !invocation.force {
                    if let Some(chain) = staged_twin_reverts.get(&twin_id) {
                        let persisted_after_matches = after_row_opt.as_ref().map_or(false, |p| {
                            let ver = p
                                .metadata
                                .get("twin")
                                .and_then(|t| t.get("version"))
                                .or_else(|| {
                                    p.metadata
                                        .get("digital_twin")
                                        .and_then(|t| t.get("version"))
                                })
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let sup_by = p.metadata.get("superseded_by").and_then(|v| v.as_str());
                            ver == expected_after_ver
                                && sup_by == Some(chain.superseded_by_after_id.as_str())
                        });
                        if head_row_id != chain.staged_head_row_id
                            || chain.expected_prev_after_id.as_deref()
                                != Some(delta.after_id.as_str())
                            || !persisted_after_matches
                        {
                            return Err(ActionError::ConflictWithNewerVersion {
                                receipt_id: active_receipt.receipt_id.clone(),
                                superseded_by: head_row_id,
                            });
                        }
                    } else {
                        let after_active = after_row_opt
                            .as_ref()
                            .map_or(false, |p| is_active_payload(p, now_ts));
                        if !after_active
                            || head_row_id != delta.after_id
                            || head_version != expected_after_ver
                        {
                            return Err(ActionError::ConflictWithNewerVersion {
                                receipt_id: active_receipt.receipt_id.clone(),
                                superseded_by: head_row_id,
                            });
                        }
                    }
                }

                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &head_row_id,
                    &head_payload,
                );

                let before_payload = delta.before_payload.as_ref().ok_or_else(|| {
                    ActionError::Storage(
                        "missing before_payload on set_twin_state delta".to_string(),
                    )
                })?;

                let next_version = head_version + 1;
                let restored_row_id =
                    format!("twin-{twin_id}-v{next_version}-rev-{}", &step_hash[..12]);
                if seen_expected_rows.insert(restored_row_id.clone()) {
                    expected_heads.push(ExpectedHead::AbsentOrInactive {
                        row_id: restored_row_id.clone(),
                    });
                }

                let mut retired_head = head_payload.clone();
                if !retired_head.metadata.is_object() {
                    retired_head.metadata = serde_json::json!({});
                }
                if let Some(meta) = retired_head.metadata.as_object_mut() {
                    meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
                    meta.insert(
                        "superseded_by".to_string(),
                        serde_json::json!(restored_row_id),
                    );
                }

                let mut restored_payload = before_payload.clone();
                restored_payload.content = format!(
                    "Digital twin '{}' v{} (reverted:{})",
                    twin_id, next_version, active_receipt.receipt_id
                );
                if !restored_payload.metadata.is_object() {
                    restored_payload.metadata = serde_json::json!({});
                }
                if let Some(meta) = restored_payload.metadata.as_object_mut() {
                    meta.remove("valid_to");
                    meta.remove("superseded_by");
                    meta.insert("valid_from".to_string(), serde_json::json!(now_ts));
                    meta.insert("supersedes".to_string(), serde_json::json!(head_row_id));
                    meta.insert(
                        "reverted_from_receipt".to_string(),
                        serde_json::json!(active_receipt.receipt_id),
                    );
                    if let Some(twin_obj) = meta.get_mut("twin").and_then(|v| v.as_object_mut()) {
                        twin_obj.insert("version".to_string(), serde_json::json!(next_version));
                        twin_obj.insert("updated_at".to_string(), serde_json::json!(now_ts));
                    }
                    if let Some(dt_obj) =
                        meta.get_mut("digital_twin").and_then(|v| v.as_object_mut())
                    {
                        dt_obj.insert("version".to_string(), serde_json::json!(next_version));
                        dt_obj.insert(
                            "observed_at".to_string(),
                            serde_json::json!(now_ts.to_string()),
                        );
                    }
                }

                for d in crate::digital_twin::diff(&head_payload, &restored_payload) {
                    revert_twin_deltas.push(ActionTwinDelta {
                        twin_id: twin_id.clone(),
                        before_row_id: head_row_id.clone(),
                        after_row_id: restored_row_id.clone(),
                        before_version: head_version,
                        after_version: next_version,
                        name: d.name,
                        old: d.old,
                        new: d.new,
                    });
                }

                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    head_row_id.clone(),
                    retired_head,
                );
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    restored_row_id.clone(),
                    restored_payload.clone(),
                );
                staged_twin_reverts.insert(
                    twin_id.clone(),
                    StagedTwinRevertChain {
                        expected_prev_after_id: delta.before_id.clone(),
                        superseded_by_after_id: delta.after_id.clone(),
                        staged_head_row_id: restored_row_id.clone(),
                    },
                );

                revert_superseded_pairs.push(SupersededPair {
                    before_id: head_row_id.clone(),
                    after_id: restored_row_id.clone(),
                });
                revert_created_ids.push(restored_row_id.clone());
                revert_row_deltas.push(RowDelta {
                    kind: "set_twin_state".to_string(),
                    before_id: Some(head_row_id),
                    before_payload: Some(head_payload),
                    after_id: restored_row_id,
                    after_payload: restored_payload,
                    created: true,
                });
            }
            "supersede_memory" => {
                let Some((_v, after_row)) = store
                    .get(&invocation.namespace, &delta.after_id)
                    .await
                    .map_err(|e| ActionError::Storage(e.to_string()))?
                else {
                    return Err(ActionError::ConflictWithNewerVersion {
                        receipt_id: active_receipt.receipt_id.clone(),
                        superseded_by: "missing".to_string(),
                    });
                };

                let (head_row_id, head_payload) = if invocation.force {
                    follow_superseded_chain(
                        store,
                        &invocation.namespace,
                        &delta.after_id,
                        after_row,
                    )
                    .await?
                } else {
                    if !is_active_payload(&after_row, now_ts)
                        || after_row.content != delta.after_payload.content
                    {
                        let sup = after_row
                            .metadata
                            .get("superseded_by")
                            .and_then(|v| v.as_str())
                            .unwrap_or("modified")
                            .to_string();
                        return Err(ActionError::ConflictWithNewerVersion {
                            receipt_id: active_receipt.receipt_id.clone(),
                            superseded_by: sup,
                        });
                    }
                    (delta.after_id.clone(), after_row)
                };

                check_target_auth(&head_payload, &head_row_id)?;
                if let Some(ref before_p) = delta.before_payload {
                    check_target_auth(before_p, &head_row_id)?;
                }

                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &head_row_id,
                    &head_payload,
                );

                let before_payload = delta.before_payload.as_ref().ok_or_else(|| {
                    ActionError::Storage(
                        "missing before_payload on supersede_memory delta".to_string(),
                    )
                })?;

                let restored_row_id = format!("act-rev-sup-{}", &step_hash[..16]);
                if seen_expected_rows.insert(restored_row_id.clone()) {
                    expected_heads.push(ExpectedHead::AbsentOrInactive {
                        row_id: restored_row_id.clone(),
                    });
                }

                let mut retired_head = head_payload.clone();
                if !retired_head.metadata.is_object() {
                    retired_head.metadata = serde_json::json!({});
                }
                if let Some(meta) = retired_head.metadata.as_object_mut() {
                    meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
                    meta.insert(
                        "superseded_by".to_string(),
                        serde_json::json!(restored_row_id),
                    );
                }

                let mut restored_payload = before_payload.clone();
                if !restored_payload.metadata.is_object() {
                    restored_payload.metadata = serde_json::json!({});
                }
                if let Some(meta) = restored_payload.metadata.as_object_mut() {
                    meta.remove("valid_to");
                    meta.remove("superseded_by");
                    meta.insert("valid_from".to_string(), serde_json::json!(now_ts));
                    meta.insert("supersedes".to_string(), serde_json::json!(head_row_id));
                    meta.insert(
                        "reverted_from_receipt".to_string(),
                        serde_json::json!(active_receipt.receipt_id),
                    );
                }

                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    head_row_id.clone(),
                    retired_head,
                );
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    restored_row_id.clone(),
                    restored_payload.clone(),
                );

                revert_superseded_pairs.push(SupersededPair {
                    before_id: head_row_id.clone(),
                    after_id: restored_row_id.clone(),
                });
                revert_created_ids.push(restored_row_id.clone());
                revert_row_deltas.push(RowDelta {
                    kind: "supersede_memory".to_string(),
                    before_id: Some(head_row_id),
                    before_payload: Some(head_payload),
                    after_id: restored_row_id,
                    after_payload: restored_payload,
                    created: true,
                });
            }
            "upsert_memory" => {
                let current_row = if let Some(staged) = staged_rows.get(&delta.after_id) {
                    staged.clone()
                } else {
                    let Some((_v, persisted)) = store
                        .get(&invocation.namespace, &delta.after_id)
                        .await
                        .map_err(|e| ActionError::Storage(e.to_string()))?
                    else {
                        return Err(ActionError::ConflictWithNewerVersion {
                            receipt_id: active_receipt.receipt_id.clone(),
                            superseded_by: "missing".to_string(),
                        });
                    };
                    persisted
                };

                check_target_auth(&current_row, &delta.after_id)?;
                if let Some(ref before_p) = delta.before_payload {
                    check_target_auth(before_p, &delta.after_id)?;
                }

                let mut expected_after_payload = delta.after_payload.clone();
                // Legacy rows written before #251 may still carry raw topology
                // too. Compare and fingerprint a canonicalized clone without
                // touching the persisted payload used for staging.
                let mut comparison_current_row = current_row.clone();
                if expected_after_payload.metadata != current_row.metadata
                    && expected_after_payload.metadata.is_object()
                {
                    let ontology = store.get_ontology(&invocation.namespace);
                    // Legacy receipts stored raw topology before #251 normalized
                    // it on write; replay their expected after-image against the
                    // same canonical form the persisted row carries.
                    if let Some(topology) = expected_after_payload.metadata.get_mut("topology") {
                        crate::rdf::canonicalize_topology(topology, &ontology);
                    }
                    if let Some(topology) = comparison_current_row.metadata.get_mut("topology") {
                        crate::rdf::canonicalize_topology(topology, &ontology);
                    }
                    if current_row.metadata.get("facets").is_some()
                        || current_row.metadata.get("facets_bitemporal").is_some()
                    {
                        let old_entries = expected_after_payload
                            .metadata
                            .get("facets_bitemporal")
                            .and_then(|v| v.as_array());
                        let new_entries = current_row
                            .metadata
                            .get("facets_bitemporal")
                            .and_then(|v| v.as_array());
                        let old_len = old_entries.map(|a| a.len()).unwrap_or(0);
                        let appended_ts = new_entries
                            .and_then(|arr| arr.get(old_len))
                            .and_then(|entry| entry.get("valid_from"))
                            .and_then(|v| v.as_i64());
                        let retracted_ts =
                            old_entries.zip(new_entries).and_then(|(old_arr, new_arr)| {
                                old_arr.iter().zip(new_arr.iter()).find_map(|(o, n)| {
                                    if o.get("valid_to").map(|v| v.is_null()).unwrap_or(true) {
                                        n.get("valid_to").and_then(|v| v.as_i64())
                                    } else {
                                        None
                                    }
                                })
                            });
                        let facet_ts = appended_ts
                            .or(retracted_ts)
                            .unwrap_or(active_receipt.executed_at);
                        crate::domain::materialize_facets_bitemporal(
                            &expected_after_payload.memory_type,
                            &mut expected_after_payload.metadata,
                            facet_ts,
                        );
                    }
                }

                let comparison_current_fp = crate::rdf::scenario::fingerprint_live_row(
                    Some(&comparison_current_row),
                    now_ts,
                );
                let expected_after_fp = crate::rdf::scenario::fingerprint_live_row(
                    Some(&expected_after_payload),
                    now_ts,
                );

                if !invocation.force {
                    if !is_active_payload(&current_row, now_ts)
                        || comparison_current_fp != expected_after_fp
                        || comparison_current_row.content != expected_after_payload.content
                        || comparison_current_row.memory_type != expected_after_payload.memory_type
                        || comparison_current_row.user_id != expected_after_payload.user_id
                        || comparison_current_row.location != expected_after_payload.location
                        || comparison_current_row.location_lines
                            != expected_after_payload.location_lines
                        || comparison_current_row.metadata != expected_after_payload.metadata
                    {
                        let sup = current_row
                            .metadata
                            .get("superseded_by")
                            .and_then(|v| v.as_str())
                            .unwrap_or("modified")
                            .to_string();
                        return Err(ActionError::ConflictWithNewerVersion {
                            receipt_id: active_receipt.receipt_id.clone(),
                            superseded_by: sup,
                        });
                    }
                }

                if seen_expected_rows.insert(delta.after_id.clone()) {
                    // The commit CAS fingerprints the row as persisted, so a
                    // legacy raw row must be expected in its raw form even when
                    // the comparison used a canonicalized clone.
                    let expected_fingerprint =
                        crate::rdf::scenario::fingerprint_live_row(Some(&current_row), now_ts);
                    expected_heads.push(ExpectedHead::RowFingerprint {
                        row_id: delta.after_id.clone(),
                        expected_fingerprint,
                    });
                }

                if delta.created || delta.before_payload.is_none() {
                    let mut retired = current_row.clone();
                    if !retired.metadata.is_object() {
                        retired.metadata = serde_json::json!({});
                    }
                    if let Some(meta) = retired.metadata.as_object_mut() {
                        meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
                        meta.insert(
                            "reverted_by_receipt_id".to_string(),
                            serde_json::json!(revert_receipt_id),
                        );
                    }
                    stage_payload(
                        &mut staged_rows,
                        &mut staged_order,
                        delta.after_id.clone(),
                        retired.clone(),
                    );
                    revert_row_deltas.push(RowDelta {
                        kind: "upsert_memory".to_string(),
                        before_id: Some(delta.after_id.clone()),
                        before_payload: Some(current_row),
                        after_id: delta.after_id.clone(),
                        after_payload: retired,
                        created: false,
                    });
                } else if let Some(ref before_payload) = delta.before_payload {
                    let mut restored = before_payload.clone();
                    if !restored.metadata.is_object() {
                        restored.metadata = serde_json::json!({});
                    }
                    if let Some(meta) = restored.metadata.as_object_mut() {
                        meta.remove("valid_to");
                        meta.remove("superseded_by");
                        meta.insert("valid_from".to_string(), serde_json::json!(now_ts));
                        meta.insert(
                            "reverted_from_receipt".to_string(),
                            serde_json::json!(active_receipt.receipt_id),
                        );
                    }
                    stage_payload(
                        &mut staged_rows,
                        &mut staged_order,
                        delta.after_id.clone(),
                        restored.clone(),
                    );
                    revert_row_deltas.push(RowDelta {
                        kind: "upsert_memory".to_string(),
                        before_id: Some(delta.after_id.clone()),
                        before_payload: Some(current_row),
                        after_id: delta.after_id.clone(),
                        after_payload: restored,
                        created: false,
                    });
                }
            }
            "transition_task" => {
                // Repeated transitions of one task land here in reverse: the
                // persisted row already carries the chain's final status, so
                // the comparison head is the before-image staged by the newer
                // delta. The persisted row still anchors the commit CAS.
                let staged_task = staged_rows.get(&delta.after_id).cloned();
                let Some((_v, persisted_task)) = store
                    .get(&invocation.namespace, &delta.after_id)
                    .await
                    .map_err(|e| ActionError::Storage(e.to_string()))?
                else {
                    return Err(ActionError::ConflictWithNewerVersion {
                        receipt_id: active_receipt.receipt_id.clone(),
                        superseded_by: "missing_task".to_string(),
                    });
                };
                let current_task = staged_task.unwrap_or_else(|| persisted_task.clone());

                check_target_auth(&current_task, &delta.after_id)?;
                if let Some(ref before_p) = delta.before_payload {
                    check_target_auth(before_p, &delta.after_id)?;
                }

                let expected_status = delta
                    .after_payload
                    .metadata
                    .get("task")
                    .and_then(|t| t.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let expected_upd = delta
                    .after_payload
                    .metadata
                    .get("task")
                    .and_then(|t| t.get("updated_at"))
                    .and_then(|v| v.as_i64());
                let actual_status = current_task
                    .metadata
                    .get("task")
                    .and_then(|t| t.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let actual_upd = current_task
                    .metadata
                    .get("task")
                    .and_then(|t| t.get("updated_at"))
                    .and_then(|v| v.as_i64());

                if !invocation.force {
                    if !is_active_payload(&current_task, now_ts)
                        || actual_status != expected_status
                        || actual_upd != expected_upd
                    {
                        return Err(ActionError::ConflictWithNewerVersion {
                            receipt_id: active_receipt.receipt_id.clone(),
                            superseded_by: format!("task:{}:{actual_status}", delta.after_id),
                        });
                    }
                }

                record_expected_active_row(
                    &mut expected_heads,
                    &mut seen_expected_rows,
                    &delta.after_id,
                    &persisted_task,
                );

                let before_payload = delta.before_payload.as_ref().ok_or_else(|| {
                    ActionError::Storage(
                        "missing before_payload on transition_task delta".to_string(),
                    )
                })?;
                let before_status = before_payload
                    .metadata
                    .get("task")
                    .and_then(|t| t.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("open")
                    .to_string();

                let restored_task = before_payload.clone();
                stage_payload(
                    &mut staged_rows,
                    &mut staged_order,
                    delta.after_id.clone(),
                    restored_task.clone(),
                );
                revert_task_deltas.push(TaskDelta {
                    task_id: delta.after_id.clone(),
                    before_status: actual_status.to_string(),
                    after_status: before_status,
                    before_payload: current_task.clone(),
                    after_payload: restored_task.clone(),
                });
                revert_row_deltas.push(RowDelta {
                    kind: "transition_task".to_string(),
                    before_id: Some(delta.after_id.clone()),
                    before_payload: Some(current_task),
                    after_id: delta.after_id.clone(),
                    after_payload: restored_task,
                    created: false,
                });
            }
            _ => {}
        }
    }

    // 5. Supersede the active apply-receipt version with a new bitemporal version carrying reverted_at + reverted_by_receipt_id,
    // leaving the historical apply-receipt row's action_receipt payload completely untouched.
    if seen_expected_rows.insert(superseded_apply_row_id.clone()) {
        expected_heads.push(ExpectedHead::AbsentOrInactive {
            row_id: superseded_apply_row_id.clone(),
        });
    }
    if seen_expected_rows.insert(revert_receipt_id.clone()) {
        expected_heads.push(ExpectedHead::AbsentOrInactive {
            row_id: revert_receipt_id.clone(),
        });
    }

    let mut retired_apply_receipt_payload = active_receipt_payload.clone();
    if !retired_apply_receipt_payload.metadata.is_object() {
        retired_apply_receipt_payload.metadata = serde_json::json!({});
    }
    if let Some(meta) = retired_apply_receipt_payload.metadata.as_object_mut() {
        meta.insert("valid_to".to_string(), serde_json::json!(now_ts));
        meta.insert(
            "superseded_by".to_string(),
            serde_json::json!(superseded_apply_row_id),
        );
    }

    let mut superseded_apply_receipt = active_receipt.clone();
    superseded_apply_receipt.reverted_at = Some(now_ts);
    superseded_apply_receipt.reverted_by_receipt_id = Some(revert_receipt_id.clone());
    if invocation.force {
        superseded_apply_receipt.forced_by = Some(effective_principal.clone());
        superseded_apply_receipt.forced_at = Some(now_ts);
    }

    let superseded_apply_payload = MemoryPayload {
        content: active_receipt_payload.content.clone(),
        user_id: active_receipt_payload.user_id.clone(),
        memory_type: "action_receipt".to_string(),
        agent_name: active_receipt_payload.agent_name.clone(),
        location: active_receipt_payload.location.clone(),
        location_lines: active_receipt_payload.location_lines.clone(),
        metadata: serde_json::json!({
            "eligibility": "private",
            "valid_from": now_ts,
            "supersedes": active_receipt_row_id,
            "action_receipt": superseded_apply_receipt
        }),
    };

    let revert_receipt = ActionReceiptPayload {
        receipt_id: revert_receipt_id.clone(),
        action_slug: active_receipt.action_slug.clone(),
        principal: effective_principal.clone(),
        executed_at: now_ts,
        params: BTreeMap::new(),
        target_pointer_id: None,
        target_pool_id: None,
        superseded_pairs: revert_superseded_pairs,
        created_ids: revert_created_ids,
        twin_deltas: revert_twin_deltas,
        edge_deltas: revert_edge_deltas.clone(),
        row_deltas: revert_row_deltas,
        task_deltas: revert_task_deltas,
        reverted_at: None,
        reverted_by_receipt_id: None,
        reverts_receipt_id: Some(active_receipt.receipt_id.clone()),
        forced_by: if invocation.force {
            Some(effective_principal.clone())
        } else {
            None
        },
        forced_at: if invocation.force { Some(now_ts) } else { None },
    };

    let revert_receipt_payload = MemoryPayload {
        content: format!(
            "Action revert receipt '{}' reverting '{}' by '{}' at {}",
            revert_receipt_id, active_receipt.receipt_id, effective_principal, now_ts
        ),
        user_id: effective_principal,
        memory_type: "action_receipt".to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: serde_json::json!({
            "eligibility": "private",
            "valid_from": now_ts,
            "action_receipt": revert_receipt
        }),
    };

    stage_payload(
        &mut staged_rows,
        &mut staged_order,
        active_receipt_row_id,
        retired_apply_receipt_payload,
    );
    stage_payload(
        &mut staged_rows,
        &mut staged_order,
        superseded_apply_row_id.clone(),
        superseded_apply_payload,
    );
    stage_payload(
        &mut staged_rows,
        &mut staged_order,
        revert_receipt_id.clone(),
        revert_receipt_payload,
    );

    // 6. Strict SHACL post-image validation on all active staged rows in the revert batch.
    let mut shacl_failures = Vec::new();
    for row_id in &staged_order {
        let Some(payload) = staged_rows.get(row_id) else {
            continue;
        };
        if !is_active_payload(payload, now_ts) {
            continue;
        }
        for v in ont.validate_strict_post_image(payload) {
            shacl_failures.push(ActionSubmissionFailure {
                code: "shacl_violation".to_string(),
                target: format!("{row_id}:{}", v.field),
                message: v.why,
            });
        }
    }
    if !shacl_failures.is_empty() {
        return Err(ActionError::SubmissionRejected(shacl_failures));
    }

    // 7. Embed all staged rows and commit atomically.
    let texts: Vec<String> = staged_order
        .iter()
        .filter_map(|id| staged_rows.get(id).map(|p| p.content.clone()))
        .collect();
    let vectors = embedder
        .embed_batch(&texts)
        .await
        .map_err(|e| ActionError::Storage(format!("embedding failed: {e}")))?;

    let mut writes = Vec::with_capacity(staged_order.len());
    for (row_id, vector) in staged_order.into_iter().zip(vectors.into_iter()) {
        if let Some(payload) = staged_rows.remove(&row_id) {
            writes.push(StagedMemoryWrite {
                id: row_id,
                vector,
                payload,
            });
        }
    }

    store
        .commit_action_batch(
            &invocation.namespace,
            ActionWriteSet {
                expected_heads,
                writes,
                edge_deltas: revert_edge_deltas,
            },
        )
        .await?;

    let outcome = ActionRevertOutcome {
        reverted_receipt_id: active_receipt.receipt_id,
        active_receipt_version_id: superseded_apply_row_id,
        revert_receipt_id,
        revert_receipt,
        forced: invocation.force,
    };
    // Capability E: post-COMMIT revert learning hook. Runs only after the
    // revert batch is durable (never on validation failure or dry run). Negative
    // reflection targets only the governing/motivating rules actually linked to
    // the reverted receipt and uses the revert receipt id for idempotency.
    crate::learning::hooks::on_action_reverted(store, &invocation.namespace, &outcome).await;
    Ok(outcome)
}

pub async fn action_log(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    security: &ActionSecurityContext,
    limit: usize,
) -> Result<Vec<ActionLogEntry>, ActionError> {
    use crate::traits::VectorStore;

    let effective_principal = security.effective_principal();
    if effective_principal.trim().is_empty() {
        return Err(ActionError::PermissionDenied(
            "anonymous principal cannot read action_log".to_string(),
        ));
    }

    let effective_limit = if limit == 0 {
        DEFAULT_ACTION_LOG_LIMIT
    } else {
        limit.min(MAX_ACTION_LOG_LIMIT)
    };

    let now_ts = current_action_timestamp();
    let rows = store
        .list(namespace, None, Some("action_receipt"))
        .await
        .map_err(|e| ActionError::Storage(e.to_string()))?;

    let mut entries = Vec::new();
    for row in rows {
        if row.payload.memory_type != "action_receipt" {
            continue;
        }
        if !is_active_payload(&row.payload, now_ts) {
            continue;
        }
        let acl = crate::rebac::extract_acl_with_roles(&row.payload.metadata);
        if !crate::rebac::allowed_with_roles(
            &effective_principal,
            &row.payload.user_id,
            "viewer",
            &acl,
        ) {
            continue;
        }
        let Ok(receipt) = ActionReceiptPayload::from_payload(&row.payload) else {
            continue;
        };
        entries.push(ActionLogEntry {
            receipt_id: receipt.receipt_id,
            version_row_id: row.id,
            action_slug: receipt.action_slug,
            principal: receipt.principal,
            executed_at: receipt.executed_at,
            parameter_keys: receipt.params.keys().cloned().collect(),
            superseded_count: receipt.superseded_pairs.len(),
            created_count: receipt.created_ids.len(),
            twin_delta_count: receipt.twin_deltas.len(),
            edge_delta_count: receipt.edge_deltas.len(),
            task_delta_count: receipt.task_deltas.len(),
            reverted_at: receipt.reverted_at,
            reverted_by_receipt_id: receipt.reverted_by_receipt_id,
            reverts_receipt_id: receipt.reverts_receipt_id,
            forced_by: receipt.forced_by,
            forced_at: receipt.forced_at,
        });
    }

    entries.sort_by(|a, b| {
        b.executed_at
            .cmp(&a.executed_at)
            .then_with(|| b.receipt_id.cmp(&a.receipt_id))
    });
    entries.truncate(effective_limit);
    Ok(entries)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDefinedOutcome {
    pub id: String,
    pub slug: String,
    pub title: String,
}

pub async fn define_action(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    security: &ActionSecurityContext,
    definition_value: &serde_json::Value,
    embedder: &dyn crate::traits::Embedder,
) -> Result<ActionDefinedOutcome, ActionError> {
    use crate::traits::VectorStore;

    let effective_principal = security.effective_principal();
    if effective_principal.trim().is_empty() {
        return Err(ActionError::PermissionDenied(
            "anonymous principal cannot define action_type".to_string(),
        ));
    }

    if !definition_value.is_object() {
        return Err(ActionError::InvalidDefinition(vec![ShaclViolation {
            field: "metadata.action_type".to_string(),
            why: "action_type must be a JSON object".to_string(),
        }]));
    }

    let definition: ActionTypeDefinition = serde_json::from_value(definition_value.clone())
        .map_err(|e| {
            ActionError::InvalidDefinition(vec![ShaclViolation {
                field: "metadata.action_type".to_string(),
                why: format!("invalid action_type schema: {e}"),
            }])
        })?;

    let violations = definition.validate_definition();
    if !violations.is_empty() {
        return Err(ActionError::InvalidDefinition(violations));
    }

    let now_ts = next_action_timestamp();
    store
        .init(namespace)
        .await
        .map_err(|e| ActionError::Storage(e.to_string()))?;
    let existing = store
        .list(namespace, None, Some("action_type"))
        .await
        .map_err(|e| ActionError::Storage(e.to_string()))?;
    for row in existing {
        if row.payload.memory_type != "action_type" || !is_active_payload(&row.payload, now_ts) {
            continue;
        }
        let row_slug = row
            .payload
            .metadata
            .get("action_type")
            .and_then(|v| v.get("slug"))
            .and_then(|v| v.as_str());
        if row_slug == Some(definition.slug.as_str()) {
            return Err(ActionError::DuplicateActionSlug {
                namespace: namespace.to_string(),
                slug: definition.slug.clone(),
            });
        }
    }

    let content = format!(
        "ActionType '{}' ({}): {}",
        definition.slug, definition.title, definition.description
    );
    let payload = MemoryPayload {
        content: content.clone(),
        user_id: effective_principal,
        memory_type: "action_type".to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: serde_json::json!({
            "valid_from": now_ts,
            "action_type": definition,
        }),
    };

    let shacl_errs = store
        .get_ontology(namespace)
        .validate_strict_post_image(&payload);
    if !shacl_errs.is_empty() {
        return Err(ActionError::InvalidDefinition(shacl_errs));
    }

    let vector = embedder
        .embed(&content)
        .await
        .map_err(|e| ActionError::Storage(format!("embedding failed: {e}")))?;
    let row_id = format!("action-type-{}-{}", definition.slug, uuid::Uuid::new_v4());

    store
        .init(namespace)
        .await
        .map_err(|e| ActionError::Storage(e.to_string()))?;
    store
        .commit_action_batch(
            namespace,
            ActionWriteSet {
                expected_heads: vec![ExpectedHead::UniqueActionSlug {
                    slug: definition.slug.clone(),
                }],
                writes: vec![StagedMemoryWrite {
                    id: row_id.clone(),
                    vector,
                    payload,
                }],
                edge_deltas: Vec::new(),
            },
        )
        .await?;

    Ok(ActionDefinedOutcome {
        id: row_id,
        slug: definition.slug,
        title: definition.title,
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
                    new_content_template: "Updated rule for {{twin_id}} at {{target_temp}}"
                        .to_string(),
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
                    query: "ASK WHERE { SERVICE <https://example.org/sparql> { ?s ?p ?o } }"
                        .to_string(),
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
        assert!(
            !violations.is_empty(),
            "expected SHACL violations on malformed action_type"
        );

        store
            .upsert("ns-warn", "act-bad", vec![0.1, 0.2, 0.3, 0.4], bad_payload)
            .await
            .expect("warn-first raw upsert must not fail");

        let stored = store.get("ns-warn", "act-bad").await.unwrap();
        assert!(
            stored.is_some(),
            "malformed raw row should still be stored warn-first"
        );
        let ont = store.get_ontology("ns-warn");
        assert!(
            ont.action_types.is_empty(),
            "malformed action_type must not compile into OntologyIndex"
        );
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
        assert!(
            codes.contains(&"unknown_parameter"),
            "got: {:?}",
            eval.submission_failures
        );
        assert!(
            codes.contains(&"missing_parameter"),
            "got: {:?}",
            eval.submission_failures
        );
        assert!(
            codes.contains(&"non_finite_number"),
            "got: {:?}",
            eval.submission_failures
        );
        assert!(
            codes.contains(&"out_of_bounds"),
            "got: {:?}",
            eval.submission_failures
        );
        assert!(
            codes.contains(&"invalid_enum_value"),
            "got: {:?}",
            eval.submission_failures
        );
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

    /// #352: the production writer must stamp the connector target inside the
    /// nested receipt so the kernel verifier never depends on editable row
    /// metadata or on an untyped parameter bag being re-read.
    #[tokio::test]
    async fn apply_stamps_connector_target_binding_in_nested_receipt() {
        let tmp = std::env::temp_dir().join(format!("ns-action-binding-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-bind";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "act-write-back",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action definition: connector-write-back".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "connector-write-back",
                            "title": "Connector Write Back",
                            "description": "Authorize one connector write-back target",
                            "required_role": "editor",
                            "parameters": {
                                "pointer_id": { "param_type": "string", "required": true },
                                "pool_id": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Approved write-back for {{pointer_id}}",
                                    "metadata_template": {"domain": "ops"}
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let apply_out = apply_action(
            &store,
            &ActionInvocation {
                namespace: ns.to_string(),
                slug: "connector-write-back".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("pointer_id".to_string(), json!("ptr-bound")),
                    ("pool_id".to_string(), json!("default")),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("apply connector-write-back");
        let (receipt_id, receipt) = match apply_out {
            ActionApplyOutcome::Applied {
                receipt_id,
                receipt,
            } => (receipt_id, receipt),
            other => panic!("expected Applied, got {other:?}"),
        };
        assert_eq!(receipt.target_pointer_id.as_deref(), Some("ptr-bound"));
        assert_eq!(receipt.target_pool_id.as_deref(), Some("default"));

        let row = store
            .get(ns, &receipt_id)
            .await
            .unwrap()
            .expect("receipt row");
        assert_eq!(
            row.1.metadata["action_receipt"]["target_pointer_id"],
            json!("ptr-bound")
        );
        assert_eq!(
            row.1.metadata["action_receipt"]["target_pool_id"],
            json!("default")
        );
        assert!(
            row.1.metadata.get("pointer_id").is_none(),
            "the target must live inside the receipt, not in row metadata"
        );
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
        let codes1: Vec<&str> = eval1
            .submission_failures
            .iter()
            .map(|f| f.code.as_str())
            .collect();
        assert!(
            codes1.contains(&"target_not_found"),
            "got: {:?}",
            eval1.submission_failures
        );
        assert!(
            codes1.contains(&"target_type_mismatch"),
            "got: {:?}",
            eval1.submission_failures
        );

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
        let codes2: Vec<&str> = eval2
            .submission_failures
            .iter()
            .map(|f| f.code.as_str())
            .collect();
        assert!(
            codes2.contains(&"target_schema_mismatch"),
            "got: {:?}",
            eval2.submission_failures
        );
        assert!(
            codes2.contains(&"rebac_denied"),
            "got: {:?}",
            eval2.submission_failures
        );
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
        let hostile_term = oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(hostile_text));

        let res_false = crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-inj",
            query,
            &[
                (target_var.clone(), target_term.clone()),
                (text_var.clone(), hostile_term),
            ],
            crate::rdf::sparql::SparqlConfig::default(),
        )
        .await
        .expect("bound ASK must succeed without syntax alteration");
        assert!(
            !res_false,
            "hostile text must evaluate to false, not inject UNION"
        );

        let exact_term = oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal("Safe content"));
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
        assert!(crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-ask",
            "ASK WHERE { ??? }",
            &[],
            cfg.clone()
        )
        .await
        .is_err());
        // 2. SELECT instead of ASK
        assert!(crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-ask",
            "SELECT ?s WHERE { ?s ?p ?o }",
            &[],
            cfg.clone()
        )
        .await
        .is_err());
        // 3. SERVICE clause
        assert!(crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-ask",
            "ASK WHERE { SERVICE <https://example.com/sparql> { ?s ?p ?o } }",
            &[],
            cfg.clone()
        )
        .await
        .is_err());
        // 4. UPDATE statement
        assert!(crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-ask",
            "INSERT DATA { <urn:a> <urn:b> <urn:c> }",
            &[],
            cfg.clone()
        )
        .await
        .is_err());
        // 5. Timeout (0ms budget)
        let timeout_cfg = crate::rdf::sparql::SparqlConfig {
            timeout_ms: 0,
            ..cfg.clone()
        };
        assert!(crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-ask",
            "ASK WHERE { ?s ?p ?o }",
            &[],
            timeout_cfg
        )
        .await
        .is_err());
        // 6. Large algebra (max_algebra_nodes = 0)
        let small_algebra_cfg = crate::rdf::sparql::SparqlConfig {
            max_algebra_nodes: 0,
            ..cfg
        };
        assert!(crate::rdf::sparql::execute_action_ask(
            &store,
            "ns-ask",
            "ASK WHERE { ?s ?p ?o }",
            &[],
            small_algebra_cfg
        )
        .await
        .is_err());

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

    struct DummyActionEmbedder;

    #[async_trait::async_trait]
    impl crate::traits::Embedder for DummyActionEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let n = (text.len() % 17) as f32 / 17.0;
            Ok(vec![n, 0.25, 0.5, 0.75])
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    #[tokio::test]
    async fn set_twin_state_stages_version_and_complete_delta() {
        let tmp = std::env::temp_dir().join(format!("ns-action-stage-twin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-stage-twin").await.unwrap();

        store
            .upsert(
                "ns-stage-twin",
                "twin-row-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Digital twin 'loop-a' v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": {
                                "temperature": 65.0,
                                "flow_rate": 12.0
                            }
                        }
                    }),
                },
            )
            .await
            .unwrap();

        store
            .upsert(
                "ns-stage-twin",
                "act-recal",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: recalibrate-cooling-loop".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "recalibrate-cooling-loop",
                            "title": "Recalibrate Cooling Loop",
                            "description": "Set target temperature",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": {
                                    "param_type": "twin_id",
                                    "required": true,
                                    "target_memory_type": "digital_twin",
                                    "target_schema_id": "cooling-loop"
                                },
                                "target_temp": {
                                    "param_type": "f64",
                                    "required": true,
                                    "min": 10.0,
                                    "max": 90.0
                                }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": {
                                        "temperature": "target_temp"
                                    }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("twin_id".to_string(), json!("loop-a"));
        params.insert("target_temp".to_string(), json!(42.5));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-stage-twin".to_string(),
                slug: "recalibrate-cooling-loop".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        assert!(eval.submission_failures.is_empty());

        let before_rows = store.list("ns-stage-twin", None, None).await.unwrap();
        let stage = stage_action(&store, eval, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage_action");
        let after_rows = store.list("ns-stage-twin", None, None).await.unwrap();
        assert_eq!(
            before_rows.len(),
            after_rows.len(),
            "stage_action must not write to store"
        );

        assert_eq!(stage.receipt.twin_deltas.len(), 1);
        let td = &stage.receipt.twin_deltas[0];
        assert_eq!(td.twin_id, "loop-a");
        assert_eq!(td.name, "temperature");
        assert_eq!(td.old, Some(65.0));
        assert_eq!(td.new, Some(42.5));
        assert_eq!(td.before_version, 1);
        assert_eq!(td.after_version, 2);

        // Staged writes include retired old twin row, new v2 twin row, and private receipt row
        assert_eq!(stage.write_set.writes.len(), 3);
        let new_twin_write = stage
            .write_set
            .writes
            .iter()
            .find(|w| w.id == td.after_row_id)
            .expect("new twin row staged");
        assert_eq!(new_twin_write.payload.metadata["twin"]["version"], json!(2));
        assert_eq!(
            new_twin_write.payload.metadata["twin"]["variables"]["temperature"],
            json!(42.5)
        );
        assert_eq!(
            new_twin_write.payload.metadata["twin"]["variables"]["flow_rate"],
            json!(12.0)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn supersede_and_upsert_capture_full_before_images_and_created_ids() {
        let tmp = std::env::temp_dir().join(format!("ns-action-stage-sup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-stage-sup").await.unwrap();

        let orig_rule = MemoryPayload {
            content: "Old cooling rule".to_string(),
            user_id: "alice".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "src/cooling.rs".to_string(),
            location_lines: "10-20".to_string(),
            metadata: json!({
                "domain": "ops",
                "source": "owner 2026-10-01"
            }),
        };
        store
            .upsert("ns-stage-sup", "rule-old", vec![0.1; 4], orig_rule.clone())
            .await
            .unwrap();

        store
            .upsert(
                "ns-stage-sup",
                "act-sup-up",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: supersede-and-log".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "supersede-and-log",
                            "title": "Supersede and Upsert",
                            "description": "Supersedes rule and upserts deterministic fact",
                            "required_role": "editor",
                            "parameters": {
                                "rule_id": {
                                    "param_type": "memory_id",
                                    "required": true,
                                    "target_memory_type": "rule"
                                },
                                "summary": {
                                    "param_type": "string",
                                    "required": true
                                }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "supersede_memory",
                                    "target_param": "rule_id",
                                    "new_content_template": "Updated rule: {{summary}}",
                                    "metadata_patch": { "source": "action supersede-and-log" }
                                },
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Audit note: {{summary}}",
                                    "metadata_template": { "domain": "ops" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("rule_id".to_string(), json!("rule-old"));
        params.insert("summary".to_string(), json!("tightened threshold"));

        let eval1 = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-stage-sup".to_string(),
                slug: "supersede-and-log".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: params.clone(),
            },
        )
        .await
        .unwrap();
        let stage1 = stage_action(&store, eval1, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage1");

        assert_eq!(stage1.receipt.superseded_pairs.len(), 1);
        assert_eq!(stage1.receipt.superseded_pairs[0].before_id, "rule-old");
        let new_rule_id = &stage1.receipt.superseded_pairs[0].after_id;
        assert!(stage1.receipt.created_ids.contains(new_rule_id));

        let upsert_delta = stage1
            .receipt
            .row_deltas
            .iter()
            .find(|d| d.kind == "upsert_memory")
            .expect("upsert delta");
        assert!(upsert_delta.created);
        assert!(upsert_delta.before_payload.is_none());
        assert!(stage1.receipt.created_ids.contains(&upsert_delta.after_id));

        let sup_delta = stage1
            .receipt
            .row_deltas
            .iter()
            .find(|d| d.kind == "supersede_memory")
            .expect("supersede delta");
        let stored_orig_rule = store
            .get("ns-stage-sup", "rule-old")
            .await
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(
            serde_json::to_value(sup_delta.before_payload.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&stored_orig_rule).unwrap()
        );

        // Now pre-populate the deterministic upsert ID to verify same-ID replace captures before_payload and created=false
        let existing_upsert_payload = MemoryPayload {
            content: "Pre-existing deterministic fact".to_string(),
            user_id: "alice".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({"domain": "ops"}),
        };
        store
            .upsert(
                "ns-stage-sup",
                &upsert_delta.after_id,
                vec![0.2; 4],
                existing_upsert_payload.clone(),
            )
            .await
            .unwrap();
        let stored_existing_upsert = store
            .get("ns-stage-sup", &upsert_delta.after_id)
            .await
            .unwrap()
            .unwrap()
            .1;

        let eval2 = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-stage-sup".to_string(),
                slug: "supersede-and-log".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        let stage2 = stage_action(&store, eval2, &DummyActionEmbedder, 1794675610)
            .await
            .expect("stage2");
        let upsert_delta2 = stage2
            .receipt
            .row_deltas
            .iter()
            .find(|d| d.kind == "upsert_memory")
            .expect("upsert delta 2");
        assert!(
            !upsert_delta2.created,
            "same-ID replace must set created=false"
        );
        assert_eq!(
            serde_json::to_value(upsert_delta2.before_payload.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&stored_existing_upsert).unwrap()
        );
        assert!(!stage2.receipt.created_ids.contains(&upsert_delta2.after_id));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn link_edge_records_existed_before_and_task_transition_captures_history() {
        let tmp = std::env::temp_dir().join(format!("ns-action-stage-edge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-stage-edge").await.unwrap();

        store
            .upsert(
                "ns-stage-edge",
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Rule 1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "domain": "ops",
                        "source": "owner 2026-10-01",
                        "governs": ["fact-existing"]
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-stage-edge",
                "fact-existing",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Fact existing".to_string(),
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
        // Re-upsert rule-1 so GOVERNS edge to fact-existing materializes
        store
            .upsert(
                "ns-stage-edge",
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Rule 1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "domain": "ops",
                        "source": "owner 2026-10-01",
                        "governs": ["fact-existing"]
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-stage-edge",
                "fact-new-edge",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Fact without edge".to_string(),
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
        let orig_task_payload = MemoryPayload {
            content: "Task 1".to_string(),
            user_id: "alice".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: json!({
                "task": {
                    "title": "Task 1",
                    "status": "open",
                    "priority": 1,
                    "created_at": 1700000000,
                    "updated_at": 1700000000,
                    "history": []
                }
            }),
        };
        store
            .upsert(
                "ns-stage-edge",
                "task-1",
                vec![0.1; 4],
                orig_task_payload.clone(),
            )
            .await
            .unwrap();

        store
            .upsert(
                "ns-stage-edge",
                "act-edge-task",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: link-and-transition".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "link-and-transition",
                            "title": "Link and Transition",
                            "description": "Links edges and transitions task",
                            "required_role": "editor",
                            "parameters": {
                                "rule_id": { "param_type": "memory_id", "required": true, "target_memory_type": "rule" },
                                "existing_target": { "param_type": "memory_id", "required": true, "target_memory_type": "fact" },
                                "fresh_target": { "param_type": "memory_id", "required": true, "target_memory_type": "fact" },
                                "task_id": { "param_type": "memory_id", "required": true, "target_memory_type": "task" },
                                "reason": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "link_edge",
                                    "source_param": "rule_id",
                                    "relation": "GOVERNS",
                                    "target_param": "existing_target"
                                },
                                {
                                    "op": "link_edge",
                                    "source_param": "rule_id",
                                    "relation": "GOVERNS",
                                    "target_param": "fresh_target"
                                },
                                {
                                    "op": "transition_task",
                                    "target_param": "task_id",
                                    "new_status": "blocked",
                                    "note_template": "Blocked on {{reason}}"
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("rule_id".to_string(), json!("rule-1"));
        params.insert("existing_target".to_string(), json!("fact-existing"));
        params.insert("fresh_target".to_string(), json!("fact-new-edge"));
        params.insert("task_id".to_string(), json!("task-1"));
        params.insert("reason".to_string(), json!("upstream valve maintenance"));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-stage-edge".to_string(),
                slug: "link-and-transition".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        let stage = stage_action(&store, eval, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage");

        assert_eq!(stage.receipt.edge_deltas.len(), 2);
        let existing_ed = stage
            .receipt
            .edge_deltas
            .iter()
            .find(|e| e.target == "fact-existing")
            .unwrap();
        assert!(existing_ed.existed_before);
        let fresh_ed = stage
            .receipt
            .edge_deltas
            .iter()
            .find(|e| e.target == "fact-new-edge")
            .unwrap();
        assert!(!fresh_ed.existed_before);

        let stored_orig_task = store
            .get("ns-stage-edge", "task-1")
            .await
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(stage.receipt.task_deltas.len(), 1);
        let td = &stage.receipt.task_deltas[0];
        assert_eq!(td.task_id, "task-1");
        assert_eq!(td.before_status, "open");
        assert_eq!(td.after_status, "blocked");
        assert_eq!(
            serde_json::to_value(&td.before_payload).unwrap(),
            serde_json::to_value(&stored_orig_task).unwrap()
        );
        let history = td.after_payload.metadata["task"]["history"]
            .as_array()
            .expect("history array");
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0]["note"],
            json!("Blocked on upstream valve maintenance")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn templates_preserve_whole_json_types_and_reject_bad_or_oversized_placeholders() {
        let mut params = BTreeMap::new();
        params.insert("count".to_string(), json!(42));
        params.insert("temp".to_string(), json!(36.5));
        params.insert("flag".to_string(), json!(true));
        params.insert("name".to_string(), json!("cooling-loop"));

        let val = render_template_value(
            &json!({
                "whole_int": "{{count}}",
                "whole_float": "{{temp}}",
                "whole_bool": "{{flag}}",
                "embedded": "Loop {{name}} has count={{count}} and flag={{flag}}"
            }),
            &params,
        )
        .expect("render valid template");

        assert_eq!(val["whole_int"], json!(42));
        assert_eq!(val["whole_float"], json!(36.5));
        assert_eq!(val["whole_bool"], json!(true));
        assert_eq!(
            val["embedded"],
            json!("Loop cooling-loop has count=42 and flag=true")
        );

        // Reject unbound parameter
        assert!(render_template_string("Hello {{unknown}}", &params).is_err());
        // Reject expression / bad placeholder syntax
        assert!(render_template_string("Hello {{count + 1}}", &params).is_err());
        // Reject unbalanced braces
        assert!(render_template_string("Hello {{count", &params).is_err());
        // Reject template in JSON object key
        assert!(render_template_value(&json!({"{{name}}": "value"}), &params).is_err());
        // Reject oversized output (> 1 MiB)
        let mut big_params = BTreeMap::new();
        big_params.insert("blob".to_string(), json!("x".repeat(600_000)));
        assert!(render_template_string("{{blob}}{{blob}}", &big_params).is_err());
    }

    #[tokio::test]
    async fn any_staged_shacl_violation_rejects_all_rows_and_receipt_before_write() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-stage-shacl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-stage-shacl").await.unwrap();

        // Action upserts a 'rule' memory without required 'metadata.source', which violates SHACL!
        store
            .upsert(
                "ns-stage-shacl",
                "act-bad-shacl",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: upsert-sourceless-rule".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "upsert-sourceless-rule",
                            "title": "Upsert Sourceless Rule",
                            "description": "Stages a rule missing source",
                            "required_role": "editor",
                            "parameters": {
                                "text": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "rule",
                                    "content_template": "Rule: {{text}}",
                                    "metadata_template": { "domain": "ops" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let before_count = store
            .list("ns-stage-shacl", None, None)
            .await
            .unwrap()
            .len();
        let mut params = BTreeMap::new();
        params.insert("text".to_string(), json!("must have source"));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-stage-shacl".to_string(),
                slug: "upsert-sourceless-rule".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        assert!(eval.submission_failures.is_empty());

        let stage_res = stage_action(&store, eval, &DummyActionEmbedder, 1794675600).await;
        assert!(
            stage_res.is_err(),
            "post-image SHACL violation must reject staging"
        );
        let after_count = store
            .list("ns-stage-shacl", None, None)
            .await
            .unwrap()
            .len();
        assert_eq!(
            before_count, after_count,
            "zero rows written on SHACL rejection"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn action_batch_commits_every_row_edge_and_receipt_together() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-batch-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-batch-commit").await.unwrap();

        store
            .upsert(
                "ns-batch-commit",
                "twin-row-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Digital twin 'loop-a' v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-batch-commit",
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Cooling governance rule".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({"domain": "ops", "source": "owner 2026-10-01"}),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-batch-commit",
                "fact-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Cooling fact".to_string(),
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

        store
            .upsert(
                "ns-batch-commit",
                "act-combo",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: combo-commit".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "combo-commit",
                            "title": "Combo Commit",
                            "description": "Mutates twin, links edge, upserts fact",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "rule_id": { "param_type": "memory_id", "required": true, "target_memory_type": "rule" },
                                "fact_id": { "param_type": "memory_id", "required": true, "target_memory_type": "fact" },
                                "temp": { "param_type": "f64", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                },
                                {
                                    "op": "link_edge",
                                    "source_param": "rule_id",
                                    "relation": "GOVERNS",
                                    "target_param": "fact_id"
                                },
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Committed temp {{temp}}",
                                    "metadata_template": { "domain": "ops" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("twin_id".to_string(), json!("loop-a"));
        params.insert("rule_id".to_string(), json!("rule-1"));
        params.insert("fact_id".to_string(), json!("fact-1"));
        params.insert("temp".to_string(), json!(48.0));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-batch-commit".to_string(),
                slug: "combo-commit".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        let stage = stage_action(&store, eval, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage");

        store
            .commit_action_batch("ns-batch-commit", stage.write_set.clone())
            .await
            .expect("commit_action_batch");

        // Verify twin v1 retired, twin v2 active, upsert row created, receipt stored, edge created
        let (_, twin_v1) = store
            .get("ns-batch-commit", "twin-row-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(twin_v1.metadata["valid_to"], json!(1794675600));
        let new_twin_id = &stage.receipt.twin_deltas[0].after_row_id;
        let (_, twin_v2) = store
            .get("ns-batch-commit", new_twin_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(twin_v2.metadata["twin"]["version"], json!(2));
        assert_eq!(
            twin_v2.metadata["twin"]["variables"]["temperature"],
            json!(48.0)
        );

        let (_, receipt_row) = store
            .get("ns-batch-commit", &stage.receipt_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt_row.memory_type, "action_receipt");
        assert_eq!(receipt_row.metadata["eligibility"], json!("private"));

        assert!(store
            .has_edge("ns-batch-commit", "rule-1", "GOVERNS", "fact-1")
            .await
            .unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn fault_after_first_write_rolls_back_effects_receipt_edges_and_indexes() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-batch-rollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-batch-rb").await.unwrap();

        store
            .upsert(
                "ns-batch-rb",
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Original rule before fault".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({"domain": "ops", "source": "owner 2026-10-01"}),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-batch-rb",
                "fact-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Target fact".to_string(),
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
        store
            .upsert(
                "ns-batch-rb",
                "act-rb",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: rb-action".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "rb-action",
                            "title": "Rollback Action",
                            "description": "Supersedes rule, links edge, upserts fact",
                            "required_role": "editor",
                            "parameters": {
                                "rule_id": { "param_type": "memory_id", "required": true, "target_memory_type": "rule" },
                                "fact_id": { "param_type": "memory_id", "required": true, "target_memory_type": "fact" }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "supersede_memory",
                                    "target_param": "rule_id",
                                    "new_content_template": "Ghost rule content that must roll back",
                                    "metadata_patch": { "source": "action rb-action" }
                                },
                                {
                                    "op": "link_edge",
                                    "source_param": "rule_id",
                                    "relation": "GOVERNS",
                                    "target_param": "fact_id"
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("rule_id".to_string(), json!("rule-1"));
        params.insert("fact_id".to_string(), json!("fact-1"));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-batch-rb".to_string(),
                slug: "rb-action".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        let stage = stage_action(&store, eval, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage");

        let pre_rows = store.list("ns-batch-rb", None, None).await.unwrap();
        // Inject fault after 1 row write inside the transaction
        store.set_action_batch_fail_after_writes(Some(1));
        let commit_res = store
            .commit_action_batch("ns-batch-rb", stage.write_set.clone())
            .await;
        store.set_action_batch_fail_after_writes(None);
        assert!(
            commit_res.is_err(),
            "injected fault must fail commit_action_batch"
        );

        // Verify rule-1 is still active (valid_to absent), new rule row does not exist, receipt does not exist, edge does not exist
        let post_rows = store.list("ns-batch-rb", None, None).await.unwrap();
        assert_eq!(
            pre_rows.len(),
            post_rows.len(),
            "row count must match pre-state after rollback"
        );
        let (_, rule1) = store.get("ns-batch-rb", "rule-1").await.unwrap().unwrap();
        assert!(
            rule1.metadata.get("valid_to").is_none(),
            "rule-1 must remain active after rollback"
        );
        assert!(store
            .get("ns-batch-rb", &stage.receipt_id)
            .await
            .unwrap()
            .is_none());
        for created_id in &stage.receipt.created_ids {
            assert!(store
                .get("ns-batch-rb", created_id)
                .await
                .unwrap()
                .is_none());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn stale_expected_head_conflicts_before_any_write() {
        let tmp = std::env::temp_dir().join(format!("ns-action-batch-cas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-batch-cas").await.unwrap();

        store
            .upsert(
                "ns-batch-cas",
                "twin-row-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Digital twin 'loop-a' v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-batch-cas",
                "act-cas",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: cas-action".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "cas-action",
                            "title": "CAS Action",
                            "description": "Updates twin",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "temp": { "param_type": "f64", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("twin_id".to_string(), json!("loop-a"));
        params.insert("temp".to_string(), json!(50.0));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-batch-cas".to_string(),
                slug: "cas-action".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        let stage = stage_action(&store, eval, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage");

        // Simulate concurrent writer superseding twin-row-1 before commit_action_batch runs
        store
            .upsert(
                "ns-batch-cas",
                "twin-row-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Digital twin 'loop-a' v1 retired".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "valid_to": 1794675500,
                        "superseded_by": "twin-row-concurrent-v2",
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let res = store
            .commit_action_batch("ns-batch-cas", stage.write_set.clone())
            .await;
        assert!(
            matches!(res, Err(ActionError::ConflictWithNewerVersion { .. })),
            "expected ConflictWithNewerVersion on stale head, got {:?}",
            res
        );
        assert!(store
            .get("ns-batch-cas", &stage.receipt_id)
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn derived_indexes_refresh_only_after_successful_commit() {
        let tmp = std::env::temp_dir().join(format!("ns-action-batch-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-batch-idx").await.unwrap();

        store
            .upsert(
                "ns-batch-idx",
                "act-idx",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: idx-action".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "idx-action",
                            "title": "Index Action",
                            "description": "Upserts searchable fact",
                            "required_role": "editor",
                            "parameters": {
                                "token": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "UniqueLexicalToken {{token}}",
                                    "metadata_template": { "domain": "ops" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("token".to_string(), json!("zephyrquartz99"));

        let eval = evaluate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-batch-idx".to_string(),
                slug: "idx-action".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
        )
        .await
        .unwrap();
        let stage = stage_action(&store, eval, &DummyActionEmbedder, 1794675600)
            .await
            .expect("stage");

        // 1. Failed commit must not populate lexical index
        store.set_action_batch_fail_after_writes(Some(1));
        assert!(store
            .commit_action_batch("ns-batch-idx", stage.write_set.clone())
            .await
            .is_err());
        store.set_action_batch_fail_after_writes(None);

        let hits_after_abort = store
            .search_lexical(
                "ns-batch-idx",
                "zephyrquartz99",
                10,
                false,
                false,
                &[],
                "alice",
                0,
            )
            .await
            .unwrap();
        assert!(
            hits_after_abort.results.is_empty(),
            "aborted commit must not publish ghost lexical hits"
        );

        // 2. Successful commit refreshes lexical index
        store
            .commit_action_batch("ns-batch-idx", stage.write_set)
            .await
            .expect("commit");
        let hits_after_commit = store
            .search_lexical(
                "ns-batch-idx",
                "zephyrquartz99",
                10,
                false,
                false,
                &[],
                "alice",
                0,
            )
            .await
            .unwrap();
        assert_eq!(hits_after_commit.results.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn apply_all_effects_then_revert_restores_complete_pre_state() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-apply-revert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-all-five").await.unwrap();

        // 1. Seed twin, rule to supersede, existing deterministic upsert row, two facts for LinkEdge (one pre-existing edge, one fresh edge), and a task.
        store
            .upsert(
                "ns-all-five",
                "twin-row-v1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Digital twin 'loop-a' v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0, "pressure": 101.3 }
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-all-five",
                "rule-orig",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Original safety rule v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "domain": "ops",
                        "source": "owner 2026-10-01",
                        "valid_from": 1700000000
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-all-five",
                "fact-existing-edge",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Fact with pre-existing edge".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({ "domain": "ops" }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-all-five",
                "fact-fresh-edge",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Fact with fresh action edge".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({ "domain": "ops" }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-all-five",
                "edge-anchor-rule",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Anchor rule for edge tests".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "domain": "ops",
                        "source": "owner 2026-10-01",
                        "governs": ["fact-existing-edge"]
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-all-five",
                "task-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "[P1] [open] Inspect cooling valve — initial".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "task": {
                            "id": "task-1",
                            "title": "Inspect cooling valve",
                            "description": "initial",
                            "status": "open",
                            "priority": 1,
                            "task_type": "task",
                            "created_at": 1700000000,
                            "updated_at": 1700000000,
                            "history": []
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let mut params = BTreeMap::new();
        params.insert("twin_id".to_string(), json!("loop-a"));
        params.insert("rule_id".to_string(), json!("rule-orig"));
        params.insert("anchor_id".to_string(), json!("edge-anchor-rule"));
        params.insert("existing_fact_id".to_string(), json!("fact-existing-edge"));
        params.insert("fresh_fact_id".to_string(), json!("fact-fresh-edge"));
        params.insert("task_id".to_string(), json!("task-1"));
        params.insert("temp".to_string(), json!(44.0));

        // Pre-seed deterministic upsert row for effect index 3 so we test both created=true (effect 2) and created=false (effect 3)
        let replaced_upsert_id = deterministic_upsert_id("ns-all-five", "all-five-op", &params, 3);
        store
            .upsert(
                "ns-all-five",
                &replaced_upsert_id,
                vec![0.1; 4],
                MemoryPayload {
                    content: "Pre-existing deterministic fact v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({ "domain": "ops", "tag": "before-action" }),
                },
            )
            .await
            .unwrap();

        let pre_task = store.get("ns-all-five", "task-1").await.unwrap().unwrap().1;
        let pre_replaced_upsert = store
            .get("ns-all-five", &replaced_upsert_id)
            .await
            .unwrap()
            .unwrap()
            .1;

        store
            .upsert(
                "ns-all-five",
                "act-all-five",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: all-five-op".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "all-five-op",
                            "title": "All Five Effects",
                            "description": "Applies all 5 kinetic effects",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "rule_id": { "param_type": "memory_id", "required": true, "target_memory_type": "rule" },
                                "anchor_id": { "param_type": "memory_id", "required": true, "target_memory_type": "rule" },
                                "existing_fact_id": { "param_type": "memory_id", "required": true, "target_memory_type": "fact" },
                                "fresh_fact_id": { "param_type": "memory_id", "required": true, "target_memory_type": "fact" },
                                "task_id": { "param_type": "memory_id", "required": true, "target_memory_type": "task" },
                                "temp": { "param_type": "f64", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                },
                                {
                                    "op": "supersede_memory",
                                    "target_param": "rule_id",
                                    "new_content_template": "Updated safety rule temp={{temp}}",
                                    "metadata_patch": { "source": "action:all-five-op" }
                                },
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Brand new created fact temp={{temp}}",
                                    "metadata_template": { "domain": "ops" }
                                },
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Replaced deterministic fact temp={{temp}}",
                                    "metadata_template": { "domain": "ops", "tag": "after-action" }
                                },
                                {
                                    "op": "link_edge",
                                    "source_param": "anchor_id",
                                    "relation": "GOVERNS",
                                    "target_param": "existing_fact_id"
                                },
                                {
                                    "op": "link_edge",
                                    "source_param": "anchor_id",
                                    "relation": "GOVERNS",
                                    "target_param": "fresh_fact_id"
                                },
                                {
                                    "op": "transition_task",
                                    "target_param": "task_id",
                                    "new_status": "in_progress",
                                    "note_template": "Started work at temp={{temp}}"
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let apply_out = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-all-five".to_string(),
                slug: "all-five-op".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: params.clone(),
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("apply_action");

        let (receipt_id, receipt) = match apply_out {
            ActionApplyOutcome::Applied {
                receipt_id,
                receipt,
            } => (receipt_id, receipt),
            other => panic!("expected Applied, got {other:?}"),
        };

        let created_upsert_id = deterministic_upsert_id("ns-all-five", "all-five-op", &params, 2);
        assert!(store
            .has_edge(
                "ns-all-five",
                "edge-anchor-rule",
                "GOVERNS",
                "fact-fresh-edge"
            )
            .await
            .unwrap());
        let (_, task_after_apply) = store.get("ns-all-five", "task-1").await.unwrap().unwrap();
        assert_eq!(
            task_after_apply.metadata["task"]["status"],
            json!("in_progress")
        );

        // Now revert via 1-call revert_action
        let rev_out = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-all-five".to_string(),
                receipt_id: receipt_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("revert_action");

        assert_eq!(rev_out.reverted_receipt_id, receipt_id);
        assert!(!rev_out.forced);

        // 1. Twin restored to temperature=65.0, pressure=101.3 via a new superseding version (v3)
        let active_twin = resolve_namespace_target_row(&store, "ns-all-five", "loop-a", true)
            .await
            .unwrap()
            .expect("active twin");
        assert_eq!(active_twin.1.metadata["twin"]["version"], json!(3));
        assert_eq!(
            crate::digital_twin::get_state(&active_twin.1),
            BTreeMap::from([
                ("temperature".to_string(), 65.0),
                ("pressure".to_string(), 101.3)
            ])
        );

        // 2. Superseded rule: after_id is retired and a restored superseding version has original content
        let sup_after_id = &receipt.superseded_pairs[1].after_id;
        let (_, sup_after_row) = store
            .get("ns-all-five", sup_after_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!is_active_payload(&sup_after_row, i64::MAX / 2));
        let restored_rule_id = sup_after_row.metadata["superseded_by"]
            .as_str()
            .expect("superseded_by on reverted rule");
        let (_, restored_rule) = store
            .get("ns-all-five", restored_rule_id)
            .await
            .unwrap()
            .unwrap();
        assert!(is_active_payload(&restored_rule, i64::MAX / 2));
        assert_eq!(restored_rule.content, "Original safety rule v1");

        // 3. Created upsert row is retired; replaced upsert row is restored to pre-action payload
        let (_, created_upsert_row) = store
            .get("ns-all-five", &created_upsert_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !is_active_payload(&created_upsert_row, i64::MAX / 2),
            "action-created upsert row must be retired on revert"
        );
        let (_, restored_upsert_row) = store
            .get("ns-all-five", &replaced_upsert_id)
            .await
            .unwrap()
            .unwrap();
        assert!(is_active_payload(&restored_upsert_row, i64::MAX / 2));
        assert_eq!(restored_upsert_row.content, pre_replaced_upsert.content);
        assert_eq!(
            restored_upsert_row.metadata["tag"],
            pre_replaced_upsert.metadata["tag"]
        );

        // 4. Pre-existing edge is preserved; action-created edge is removed
        assert!(
            store
                .has_edge(
                    "ns-all-five",
                    "edge-anchor-rule",
                    "GOVERNS",
                    "fact-existing-edge"
                )
                .await
                .unwrap(),
            "pre-existing edge must be preserved on revert"
        );
        assert!(
            !store
                .has_edge(
                    "ns-all-five",
                    "edge-anchor-rule",
                    "GOVERNS",
                    "fact-fresh-edge"
                )
                .await
                .unwrap(),
            "action-created edge must be removed on revert"
        );

        // 5. Task restored to exact prior status, note/description, and history
        let (_, task_after_revert) = store.get("ns-all-five", "task-1").await.unwrap().unwrap();
        assert_eq!(task_after_revert.content, pre_task.content);
        assert_eq!(
            task_after_revert.metadata["task"]["status"],
            pre_task.metadata["task"]["status"]
        );
        assert_eq!(
            task_after_revert.metadata["task"]["history"],
            pre_task.metadata["task"]["history"]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn revert_refuses_stale_and_double_revert_without_writes() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-revert-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-rev-stale").await.unwrap();

        store
            .upsert(
                "ns-rev-stale",
                "twin-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Twin loop-a v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-rev-stale",
                "act-set-temp",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: set-temp".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "set-temp",
                            "title": "Set Temp",
                            "description": "Sets twin temperature",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "temp": { "param_type": "f64", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let apply1 = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-rev-stale".to_string(),
                slug: "set-temp".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(50.0)),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();
        let receipt1_id = apply1.receipt_id().unwrap().to_string();

        // Apply a second action that supersedes v2 -> v3
        let apply2 = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-rev-stale".to_string(),
                slug: "set-temp".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(40.0)),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();
        let receipt2_id = apply2.receipt_id().unwrap().to_string();

        // 1. Reverting receipt1 without force must fail with ConflictWithNewerVersion and perform zero writes
        let rows_before_stale = store.list("ns-rev-stale", None, None).await.unwrap().len();
        let stale_err = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-rev-stale".to_string(),
                receipt_id: receipt1_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await;
        assert!(
            matches!(stale_err, Err(ActionError::ConflictWithNewerVersion { .. })),
            "expected ConflictWithNewerVersion, got {stale_err:?}"
        );
        let rows_after_stale = store.list("ns-rev-stale", None, None).await.unwrap().len();
        assert_eq!(
            rows_before_stale, rows_after_stale,
            "zero writes on stale conflict"
        );

        // 2. Reverting receipt2 (the current active head) succeeds
        let rev2 = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-rev-stale".to_string(),
                receipt_id: receipt2_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("revert receipt2");

        // 3. Reverting receipt2 a second time must fail with AlreadyReverted and perform zero writes
        let rows_before_double = store.list("ns-rev-stale", None, None).await.unwrap().len();
        let double_err = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-rev-stale".to_string(),
                receipt_id: receipt2_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await;
        assert_eq!(
            double_err.unwrap_err(),
            ActionError::AlreadyReverted {
                receipt_id: receipt2_id,
                reverted_by_receipt_id: rev2.revert_receipt_id,
            }
        );
        let rows_after_double = store.list("ns-rev-stale", None, None).await.unwrap().len();
        assert_eq!(
            rows_before_double, rows_after_double,
            "zero writes on double revert"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn force_requires_admin_or_owner_and_is_audited() {
        let tmp = std::env::temp_dir().join(format!("ns-action-force-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-force").await.unwrap();

        // Twin owned by alice, bob granted only editor in ACL
        store
            .upsert(
                "ns-force",
                "twin-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Twin loop-a v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "visible_to": [{ "principal": "bob", "role": "editor" }],
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-force",
                "act-set-temp",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: set-temp".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "set-temp",
                            "title": "Set Temp",
                            "description": "Sets twin temperature",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "temp": { "param_type": "f64", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        // Action 1 by bob (editor): 65 -> 50 (v2)
        let apply1 = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-force".to_string(),
                slug: "set-temp".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "bob".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(50.0)),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();
        let receipt1_id = apply1.receipt_id().unwrap().to_string();

        // Action 2 by bob (editor): 50 -> 40 (v3)
        let apply2 = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-force".to_string(),
                slug: "set-temp".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "bob".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(40.0)),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();
        let v3_row_id = apply2.receipt().unwrap().twin_deltas[0]
            .after_row_id
            .clone();

        // 1. Editor bob attempting force=true (even attempting to spoof requested_principal="admin") must be rejected with PermissionDenied and zero writes
        let before_count = store.list("ns-force", None, None).await.unwrap().len();
        let bob_force = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-force".to_string(),
                receipt_id: receipt1_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "bob".to_string(),
                    requested_principal: Some("admin".to_string()),
                },
                force: true,
            },
            &DummyActionEmbedder,
        )
        .await;
        assert!(
            matches!(bob_force, Err(ActionError::PermissionDenied(_))),
            "editor must be denied force revert: {bob_force:?}"
        );
        let after_count = store.list("ns-force", None, None).await.unwrap().len();
        assert_eq!(before_count, after_count, "zero writes on denied force");

        // 2. Owner alice running force=true on stale receipt1 succeeds, preserves v3 in history, creates v4 with temperature=65.0, and audits forced_by + forced_at
        let forced_rev = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-force".to_string(),
                receipt_id: receipt1_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: true,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("owner forced revert");

        assert!(forced_rev.forced);
        assert_eq!(
            forced_rev.revert_receipt.forced_by.as_deref(),
            Some("alice")
        );
        assert!(forced_rev.revert_receipt.forced_at.is_some());

        // Intermediate v3 row still exists in storage (retired, not deleted)
        let (_, v3_row) = store
            .get("ns-force", &v3_row_id)
            .await
            .unwrap()
            .expect("v3 preserved");
        assert_eq!(
            v3_row.metadata["twin"]["variables"]["temperature"],
            json!(40.0)
        );
        assert!(v3_row.metadata.get("valid_to").is_some());

        // Active twin is now v4 with restored temperature=65.0
        let active_twin = resolve_namespace_target_row(&store, "ns-force", "loop-a", true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active_twin.1.metadata["twin"]["version"], json!(4));
        assert_eq!(
            active_twin.1.metadata["twin"]["variables"]["temperature"],
            json!(65.0)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn receipt_versions_are_immutable_and_log_is_bounded_newest_first_and_redacted() {
        let tmp = std::env::temp_dir().join(format!("ns-action-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-log").await.unwrap();

        store
            .upsert(
                "ns-log",
                "act-secret",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: log-secret".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "log-secret",
                            "title": "Log Secret",
                            "description": "Upserts fact with sensitive param",
                            "required_role": "editor",
                            "parameters": {
                                "secret_token": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "SensitiveContent={{secret_token}}",
                                    "metadata_template": { "domain": "ops" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let apply1 = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-log".to_string(),
                slug: "log-secret".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([(
                    "secret_token".to_string(),
                    json!("TOP_SECRET_ALPHA_111"),
                )]),
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();
        let r1_id = apply1.receipt_id().unwrap().to_string();
        let (_, r1_before_revert) = store.get("ns-log", &r1_id).await.unwrap().unwrap();
        let r1_receipt_json_before = r1_before_revert.metadata["action_receipt"].clone();

        // Revert r1
        let rev1 = revert_action(
            &store,
            &RevertInvocation {
                namespace: "ns-log".to_string(),
                receipt_id: r1_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();

        // Historical apply receipt version r1 must keep its action_receipt object immutable (not mutated in place)
        let (_, r1_after_revert) = store.get("ns-log", &r1_id).await.unwrap().unwrap();
        assert_eq!(
            r1_after_revert.metadata["action_receipt"], r1_receipt_json_before,
            "historical apply receipt version must never have its action_receipt mutated in place"
        );
        assert!(r1_after_revert.metadata.get("valid_to").is_some());
        assert_eq!(
            r1_after_revert.metadata["superseded_by"],
            json!(rev1.active_receipt_version_id)
        );

        // Apply a second action so we have 3 active receipt versions total: r1 (superseded active version), rev1 (revert receipt), and r2
        let apply2 = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-log".to_string(),
                slug: "log-secret".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([(
                    "secret_token".to_string(),
                    json!("TOP_SECRET_BETA_222"),
                )]),
            },
            &DummyActionEmbedder,
        )
        .await
        .unwrap();
        let r2_id = apply2.receipt_id().unwrap().to_string();

        // Query action_log with limit=2 and limit=10
        let sec = ActionSecurityContext {
            authenticated_principal: "alice".to_string(),
            requested_principal: None,
        };
        let bounded = action_log(&store, "ns-log", &sec, 2).await.unwrap();
        assert_eq!(bounded.len(), 2, "limit must bound returned entries");

        let full_log = action_log(&store, "ns-log", &sec, 10).await.unwrap();
        assert_eq!(
            full_log.len(),
            3,
            "only active receipt versions should appear in log"
        );

        // Verify newest-first ordering by (executed_at, receipt_id) descending
        for pair in full_log.windows(2) {
            assert!(
                (pair[0].executed_at, &pair[0].receipt_id)
                    >= (pair[1].executed_at, &pair[1].receipt_id),
                "action_log must be ordered newest-first"
            );
        }

        // Verify reverted r1 entry carries reverted_by_receipt_id
        let r1_entry = full_log
            .iter()
            .find(|e| e.receipt_id == r1_id)
            .expect("active version of r1 in log");
        assert_eq!(
            r1_entry.reverted_by_receipt_id.as_deref(),
            Some(rev1.revert_receipt_id.as_str())
        );
        assert!(full_log.iter().any(|e| e.receipt_id == r2_id));

        // Verify redaction: serialized log JSON must never contain secret parameter values or rendered content
        let log_json = serde_json::to_string(&full_log).unwrap();
        assert!(!log_json.contains("TOP_SECRET_ALPHA_111"));
        assert!(!log_json.contains("TOP_SECRET_BETA_222"));
        assert!(!log_json.contains("SensitiveContent="));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn rejected_apply_has_no_receipt_and_validate_never_writes() {
        let tmp = std::env::temp_dir().join(format!("ns-action-reject-val-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("ns-rej-val").await.unwrap();

        store
            .upsert(
                "ns-rej-val",
                "twin-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Twin loop-a v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns-rej-val",
                "act-guarded",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: guarded-temp".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "guarded-temp",
                            "title": "Guarded Temp",
                            "description": "Requires twin temp >= 60",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "temp": { "param_type": "f64", "required": true, "min": 10.0, "max": 80.0 }
                            },
                            "submission_criteria": [
                                {
                                    "kind": "twin_var_precondition",
                                    "twin_param": "twin_id",
                                    "variable": "temperature",
                                    "min": 60.0,
                                    "max": 90.0
                                }
                            ],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let initial_rows = store.list("ns-rej-val", None, None).await.unwrap().len();

        // 1. validate_action on a valid invocation succeeds with zero writes
        let val_ok = validate_action(
            &store,
            &ActionInvocation {
                namespace: "ns-rej-val".to_string(),
                slug: "guarded-temp".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(50.0)),
                ]),
            },
        )
        .await
        .expect("validate_action");
        assert!(val_ok.valid);
        assert!(val_ok.failures.is_empty());
        assert_eq!(
            store.list("ns-rej-val", None, None).await.unwrap().len(),
            initial_rows,
            "validate_action must never write"
        );

        // 2. apply_action with out-of-range temp (99.0 > 80.0) returns Rejected { failures } and writes zero receipts or rows
        let rej_out = apply_action(
            &store,
            &ActionInvocation {
                namespace: "ns-rej-val".to_string(),
                slug: "guarded-temp".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(99.0)),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("rejected apply returns Ok(Rejected)");
        assert!(rej_out.is_rejected());
        assert!(!rej_out.failures().is_empty());
        assert_eq!(
            store.list("ns-rej-val", None, None).await.unwrap().len(),
            initial_rows,
            "rejected apply must leave row count unchanged"
        );
        assert!(
            store
                .list("ns-rej-val", None, Some("action_receipt"))
                .await
                .unwrap()
                .is_empty(),
            "rejected apply must write zero receipts"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn concurrent_define_action_same_slug_rejects_duplicate_atomically() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-concurrent-slug-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store"),
        );
        store.init("ns-conc-slug").await.unwrap();

        let def_json = json!({
            "slug": "atomic-slug-check",
            "title": "Atomic Slug Check",
            "description": "Verifies transactional slug reservation",
            "required_role": "editor",
            "parameters": {},
            "submission_criteria": [],
            "effects": [
                {
                    "op": "upsert_memory",
                    "memory_type": "fact",
                    "content_template": "ok",
                    "metadata_template": { "domain": "ops" }
                }
            ]
        });
        let sec = ActionSecurityContext {
            authenticated_principal: "alice".to_string(),
            requested_principal: None,
        };

        let s1 = store.clone();
        let d1 = def_json.clone();
        let sec1 = sec.clone();
        let s2 = store.clone();
        let d2 = def_json.clone();
        let sec2 = sec.clone();

        let (r1, r2) = tokio::join!(
            define_action(&s1, "ns-conc-slug", &sec1, &d1, &DummyActionEmbedder),
            define_action(&s2, "ns-conc-slug", &sec2, &d2, &DummyActionEmbedder),
        );

        let successes = usize::from(r1.is_ok()) + usize::from(r2.is_ok());
        assert_eq!(
            successes, 1,
            "expected exactly one concurrent define_action to succeed: r1={r1:?}, r2={r2:?}"
        );
        let dup_err = r1.err().or_else(|| r2.err()).expect("one duplicate error");
        assert!(
            matches!(dup_err, ActionError::DuplicateActionSlug { .. }),
            "expected DuplicateActionSlug, got {dup_err:?}"
        );

        let rows = store
            .list("ns-conc-slug", None, Some("action_type"))
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "store must contain exactly one active action_type row"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn revert_multi_effect_set_twin_state_on_same_twin_chains_through_staged_state() {
        let tmp = std::env::temp_dir().join(format!(
            "ns-action-multi-twin-revert-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-multi-twin-rev";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "twin-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Twin loop-a v1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "digital_twin".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "valid_from": 1700000000,
                        "twin": {
                            "id": "loop-a",
                            "version": 1,
                            "schema_id": "cooling-loop",
                            "variables": { "temperature": 65.0, "pressure": 100.0 }
                        }
                    }),
                },
            )
            .await
            .unwrap();

        store
            .upsert(
                ns,
                "act-two-step-twin",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: two-step-twin".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "two-step-twin",
                            "title": "Two Step Twin",
                            "description": "Applies two SetTwinState effects to the same twin in one action",
                            "required_role": "editor",
                            "parameters": {
                                "twin_id": { "param_type": "twin_id", "required": true, "target_memory_type": "digital_twin", "target_schema_id": "cooling-loop" },
                                "temp": { "param_type": "f64", "required": true },
                                "press": { "param_type": "f64", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "temperature": "temp" }
                                },
                                {
                                    "op": "set_twin_state",
                                    "twin_param": "twin_id",
                                    "merge": true,
                                    "variables_from_params": { "pressure": "press" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let apply_out = apply_action(
            &store,
            &ActionInvocation {
                namespace: ns.to_string(),
                slug: "two-step-twin".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: BTreeMap::from([
                    ("twin_id".to_string(), json!("loop-a")),
                    ("temp".to_string(), json!(50.0)),
                    ("press".to_string(), json!(120.0)),
                ]),
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("apply two-step-twin");
        let receipt_id = apply_out.receipt_id().expect("applied receipt").to_string();

        let active_after_apply = resolve_namespace_target_row(&store, ns, "loop-a", true)
            .await
            .unwrap()
            .expect("active twin after apply");
        assert_eq!(active_after_apply.1.metadata["twin"]["version"], json!(3));
        assert_eq!(
            active_after_apply.1.metadata["twin"]["variables"]["temperature"],
            json!(50.0)
        );
        assert_eq!(
            active_after_apply.1.metadata["twin"]["variables"]["pressure"],
            json!(120.0)
        );

        let rev_out = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("non-forced revert of multi-effect SetTwinState action must succeed (#248)");

        assert!(!rev_out.forced);
        assert_eq!(rev_out.revert_receipt.row_deltas.len(), 2);

        let active_after_revert = resolve_namespace_target_row(&store, ns, "loop-a", true)
            .await
            .unwrap()
            .expect("active twin after revert");
        assert_eq!(active_after_revert.1.metadata["twin"]["version"], json!(5));
        assert_eq!(
            active_after_revert.1.metadata["twin"]["variables"]["temperature"],
            json!(65.0)
        );
        assert_eq!(
            active_after_revert.1.metadata["twin"]["variables"]["pressure"],
            json!(100.0)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn revert_multi_effect_transition_task_on_same_task_chains_through_staged_state() {
        let tmp = std::env::temp_dir().join(format!(
            "ns-action-multi-task-revert-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-multi-task-rev";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "task-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Task 1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "task": {
                            "title": "Task 1",
                            "status": "open",
                            "priority": 1,
                            "created_at": 1700000000,
                            "updated_at": 1700000000,
                            "history": []
                        }
                    }),
                },
            )
            .await
            .unwrap();

        store
            .upsert(
                ns,
                "act-two-step-task",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: two-step-task".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "two-step-task",
                            "title": "Two Step Task",
                            "description": "Applies two TransitionTask effects to the same task in one action",
                            "required_role": "editor",
                            "parameters": {
                                "task_id": { "param_type": "memory_id", "required": true, "target_memory_type": "task" },
                                "reason": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "transition_task",
                                    "target_param": "task_id",
                                    "new_status": "in_progress",
                                    "note_template": "Claimed by two-step-task"
                                },
                                {
                                    "op": "transition_task",
                                    "target_param": "task_id",
                                    "new_status": "blocked",
                                    "note_template": "Blocked on {{reason}}"
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let params = BTreeMap::from([
            ("task_id".to_string(), json!("task-1")),
            ("reason".to_string(), json!("upstream valve")),
        ]);
        let apply_out = apply_action(
            &store,
            &ActionInvocation {
                namespace: ns.to_string(),
                slug: "two-step-task".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params: params.clone(),
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("apply two-step-task");
        let (receipt_id, receipt) = match apply_out {
            ActionApplyOutcome::Applied {
                receipt_id,
                receipt,
            } => (receipt_id, receipt),
            other => panic!("expected Applied, got {other:?}"),
        };
        assert_eq!(
            receipt
                .row_deltas
                .iter()
                .filter(|d| d.kind == "transition_task")
                .count(),
            2
        );
        assert_eq!(receipt.task_deltas.len(), 2);

        let (_, applied_task) = store.get(ns, "task-1").await.unwrap().unwrap();
        assert_eq!(applied_task.metadata["task"]["status"], json!("blocked"));

        let rev_out = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id: receipt_id.clone(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("non-forced revert of a two-transition task action must succeed (#349)");

        assert!(!rev_out.forced);
        assert_eq!(rev_out.revert_receipt.task_deltas.len(), 2);
        let statuses: Vec<(&str, &str)> = rev_out
            .revert_receipt
            .task_deltas
            .iter()
            .map(|d| (d.before_status.as_str(), d.after_status.as_str()))
            .collect();
        assert_eq!(
            statuses,
            vec![("blocked", "in_progress"), ("in_progress", "open")]
        );

        let (_, reverted_task) = store.get(ns, "task-1").await.unwrap().unwrap();
        assert_eq!(reverted_task.metadata["task"]["status"], json!("open"));

        // A stale revert must still be refused and must write nothing.
        let apply_again = apply_action(
            &store,
            &ActionInvocation {
                namespace: ns.to_string(),
                slug: "two-step-task".to_string(),
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                params,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("re-apply two-step-task");
        let stale_receipt_id = apply_again
            .receipt_id()
            .expect("applied receipt")
            .to_string();

        store
            .upsert(
                ns,
                "task-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Task 1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "task": {
                            "title": "Task 1",
                            "status": "blocked",
                            "priority": 1,
                            "created_at": 1700000000,
                            "updated_at": 1700009999,
                            "history": []
                        }
                    }),
                },
            )
            .await
            .unwrap();
        let receipts_before = store
            .list(ns, None, Some("action_receipt"))
            .await
            .unwrap()
            .len();

        let stale_rev = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id: stale_receipt_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await;
        assert!(
            matches!(stale_rev, Err(ActionError::ConflictWithNewerVersion { .. })),
            "expected ConflictWithNewerVersion on external write, got {stale_rev:?}"
        );
        let receipts_after = store
            .list(ns, None, Some("action_receipt"))
            .await
            .unwrap()
            .len();
        assert_eq!(receipts_before, receipts_after);
        let (_, untouched_task) = store.get(ns, "task-1").await.unwrap().unwrap();
        assert_eq!(untouched_task.metadata["task"]["status"], json!("blocked"));
        assert_eq!(
            untouched_task.metadata["task"]["updated_at"],
            json!(1700009999)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn revert_first_of_two_identical_upserts_is_refused_without_retiring_newer_payload() {
        let tmp =
            std::env::temp_dir().join(format!("ns-action-upsert-rev-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-upsert-rev-guard";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "act-upsert-same",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: upsert-same".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "upsert-same",
                            "title": "Upsert Same",
                            "description": "Upserts deterministic row",
                            "required_role": "editor",
                            "parameters": {
                                "tag": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Deterministic fact tag={{tag}}",
                                    "metadata_template": { "domain": "ops" }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let inv = ActionInvocation {
            namespace: ns.to_string(),
            slug: "upsert-same".to_string(),
            security: ActionSecurityContext {
                authenticated_principal: "alice".to_string(),
                requested_principal: None,
            },
            params: BTreeMap::from([("tag".to_string(), json!("steady"))]),
        };

        let apply1 = apply_action(&store, &inv, &DummyActionEmbedder)
            .await
            .expect("apply1");
        let r1_id = apply1.receipt_id().expect("r1").to_string();
        let upsert_row_id = apply1.receipt().unwrap().row_deltas[0].after_id.clone();

        let apply2 = apply_action(&store, &inv, &DummyActionEmbedder)
            .await
            .expect("apply2");
        let r2_id = apply2.receipt_id().expect("r2").to_string();
        let (_, payload_after_apply2) = store
            .get(ns, &upsert_row_id)
            .await
            .unwrap()
            .expect("row exists");

        // Attempting to revert receipt 1 (without force) after receipt 2 rewrote the same deterministic row
        // must fail with ConflictWithNewerVersion and leave the newer payload active (#247).
        let stale_rev = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id: r1_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await;
        assert!(
            matches!(stale_rev, Err(ActionError::ConflictWithNewerVersion { .. })),
            "reverting superseded first upsert must fail with ConflictWithNewerVersion: {stale_rev:?}"
        );

        let (_, payload_after_failed_revert) = store
            .get(ns, &upsert_row_id)
            .await
            .unwrap()
            .expect("row still exists");
        assert!(
            is_active_payload(&payload_after_failed_revert, current_action_timestamp()),
            "newer upsert payload must remain active"
        );
        assert_eq!(
            payload_after_failed_revert.metadata, payload_after_apply2.metadata,
            "newer upsert metadata (including valid_from) must be untouched"
        );

        // Reverting receipt 2 (whose post-image matches the current row) succeeds and restores receipt 1's payload.
        let rev2 = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id: r2_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("reverting latest upsert receipt succeeds");
        assert!(!rev2.forced);
        let (_, payload_after_rev2) = store
            .get(ns, &upsert_row_id)
            .await
            .unwrap()
            .expect("restored row");
        assert!(is_active_payload(
            &payload_after_rev2,
            current_action_timestamp()
        ));

        // Simulate a legacy pre-normalization receipt whose embedded delta.after_payload.metadata
        // lacks "facets" and "facets_bitemporal" even though commit_action_batch materialized them
        // on the persisted row. Non-forced revert of that receipt must still succeed.
        let apply3 = apply_action(&store, &inv, &DummyActionEmbedder)
            .await
            .expect("apply3");
        let r3_id = apply3.receipt_id().expect("r3").to_string();
        let (r3_vec, mut r3_payload) = store
            .get(ns, &r3_id)
            .await
            .unwrap()
            .expect("r3 receipt row");
        if let Some(meta_obj) = r3_payload.metadata["action_receipt"]["row_deltas"][0]
            ["after_payload"]["metadata"]
            .as_object_mut()
        {
            meta_obj.remove("facets");
            meta_obj.insert(
                "facets_bitemporal".to_string(),
                json!([{
                    "facet": "CustomManualPrior",
                    "inferred": false,
                    "valid_from": 100,
                    "valid_to": null
                }]),
            );
        }
        let (row_vec, mut row_payload) = store
            .get(ns, &upsert_row_id)
            .await
            .unwrap()
            .expect("persisted upsert row");
        if let Some(meta_obj) = row_payload.metadata.as_object_mut() {
            meta_obj.remove("facets");
            meta_obj.insert(
                "facets_bitemporal".to_string(),
                json!([{
                    "facet": "CustomManualPrior",
                    "inferred": false,
                    "valid_from": 100,
                    "valid_to": null
                }]),
            );
        }
        crate::domain::materialize_facets_bitemporal(
            &row_payload.memory_type,
            &mut row_payload.metadata,
            1700000999,
        );
        store
            .upsert(ns, &upsert_row_id, row_vec, row_payload)
            .await
            .expect("persist row with historical + newly materialized facets");
        store
            .upsert(ns, &r3_id, r3_vec, r3_payload)
            .await
            .expect("persist legacy pre-normalization receipt shape");

        let rev3 = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id: r3_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("pre-normalization upsert_memory receipt must remain revertible");
        assert!(!rev3.forced);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn legacy_receipt_with_raw_topology_reverts_without_force() {
        let tmp = std::env::temp_dir().join(format!(
            "ns-action-legacy-topology-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-legacy-topology-revert";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "act-legacy-topology",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: legacy-topology".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "legacy-topology",
                            "title": "Legacy Topology",
                            "description": "Upserts a row carrying topology metadata",
                            "required_role": "editor",
                            "parameters": {
                                "tag": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Topology fact tag={{tag}}",
                                    "metadata_template": {
                                        "domain": "ops",
                                        "topology": {
                                            "component": "Ladybug DB",
                                            "target_system": "Storage Subsystem",
                                            "channel": "ipc",
                                            "credential_boundary": "local",
                                            "reason": "test"
                                        }
                                    }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let inv = ActionInvocation {
            namespace: ns.to_string(),
            slug: "legacy-topology".to_string(),
            security: ActionSecurityContext {
                authenticated_principal: "alice".to_string(),
                requested_principal: None,
            },
            params: BTreeMap::from([("tag".to_string(), json!("steady"))]),
        };

        let raw_topology = json!({
            "component": "Ladybug DB",
            "target_system": "Storage Subsystem",
            "channel": "ipc",
            "credential_boundary": "local",
            "reason": "test"
        });

        let apply = apply_action(&store, &inv, &DummyActionEmbedder)
            .await
            .expect("apply");
        let receipt_id = apply.receipt_id().expect("receipt id").to_string();
        let row_id = apply.receipt().unwrap().row_deltas[0].after_id.clone();

        // Write path canonicalized topology on the persisted row (#251).
        let (_, persisted_row) = store
            .get(ns, &row_id)
            .await
            .unwrap()
            .expect("persisted row");
        let mut canonical_topology = raw_topology.clone();
        crate::rdf::canonicalize_topology(&mut canonical_topology, &store.get_ontology(ns));
        assert_ne!(
            raw_topology, canonical_topology,
            "raw topology must differ from canonical topology"
        );
        assert_eq!(
            persisted_row.metadata["topology"], canonical_topology,
            "apply_action must persist canonical topology"
        );

        // Simulate a pre-#251 receipt whose embedded after-image still carries
        // the raw topology alias, with no other intervening edit.
        let (receipt_vec, mut receipt_payload) = store
            .get(ns, &receipt_id)
            .await
            .unwrap()
            .expect("receipt row");
        receipt_payload.metadata["action_receipt"]["row_deltas"][0]["after_payload"]["metadata"]
            ["topology"] = raw_topology;
        store
            .upsert(ns, &receipt_id, receipt_vec, receipt_payload)
            .await
            .expect("persist legacy receipt with raw topology");

        let rev = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("legacy pre-#251 receipt with raw topology must revert cleanly");
        assert!(!rev.forced);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Issue #299: a pre-#251 row that still carries the raw topology alias
    /// must not false-conflict with a pre-#251 receipt that also carries the
    /// raw alias. The comparison canonicalizes both sides.
    #[tokio::test]
    async fn legacy_raw_row_and_receipt_revert_without_force() {
        let tmp = std::env::temp_dir().join(format!(
            "ns-action-legacy-raw-both-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-legacy-raw-both-revert";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "act-legacy-raw-both",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: legacy-raw-both".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: "".to_string(),
                    location_lines: "".to_string(),
                    metadata: json!({
                        "action_type": {
                            "slug": "legacy-raw-both",
                            "title": "Legacy Raw Both",
                            "description": "Upserts a row carrying topology metadata",
                            "required_role": "editor",
                            "parameters": {
                                "tag": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [
                                {
                                    "op": "upsert_memory",
                                    "memory_type": "fact",
                                    "content_template": "Topology fact tag={{tag}}",
                                    "metadata_template": {
                                        "domain": "ops",
                                        "decision": "choose-raw-legacy",
                                        "topology": {
                                            "component": "Ladybug DB",
                                            "target_system": "Storage Subsystem",
                                            "channel": "ipc",
                                            "credential_boundary": "local",
                                            "reason": "test"
                                        }
                                    }
                                }
                            ]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let inv = ActionInvocation {
            namespace: ns.to_string(),
            slug: "legacy-raw-both".to_string(),
            security: ActionSecurityContext {
                authenticated_principal: "alice".to_string(),
                requested_principal: None,
            },
            params: BTreeMap::from([("tag".to_string(), json!("steady"))]),
        };

        let raw_topology = json!({
            "component": "Ladybug DB",
            "target_system": "Storage Subsystem",
            "channel": "ipc",
            "credential_boundary": "local",
            "reason": "test"
        });

        let apply = apply_action(&store, &inv, &DummyActionEmbedder)
            .await
            .expect("apply");
        let receipt_id = apply.receipt_id().expect("receipt id").to_string();
        let row_id = apply.receipt().unwrap().row_deltas[0].after_id.clone();

        // Rewrite the persisted row through the raw test hook: pre-#251 rows
        // were written before the store canonicalized topology.
        let (_, mut raw_row) = store
            .get(ns, &row_id)
            .await
            .unwrap()
            .expect("persisted row");
        assert!(
            raw_row.metadata.get("facets").is_some(),
            "fixture must entail a facet so the legacy comparison block runs"
        );
        raw_row.metadata["topology"] = raw_topology.clone();
        store
            .overwrite_row_metadata_for_test(ns, &row_id, raw_row.metadata.clone())
            .await
            .expect("persist legacy raw row");

        // And the receipt's embedded after-image is pre-#251: raw topology and
        // no facets, so the comparison block must replay both reconciliations.
        let (receipt_vec, mut receipt_payload) = store
            .get(ns, &receipt_id)
            .await
            .unwrap()
            .expect("receipt row");
        let after_metadata = &mut receipt_payload.metadata["action_receipt"]["row_deltas"][0]
            ["after_payload"]["metadata"];
        after_metadata["topology"] = raw_topology;
        if let Some(obj) = after_metadata.as_object_mut() {
            obj.remove("facets");
            obj.remove("facets_bitemporal");
        }
        store
            .upsert(ns, &receipt_id, receipt_vec, receipt_payload)
            .await
            .expect("persist legacy receipt with raw topology");

        let rev = revert_action(
            &store,
            &RevertInvocation {
                namespace: ns.to_string(),
                receipt_id,
                security: ActionSecurityContext {
                    authenticated_principal: "alice".to_string(),
                    requested_principal: None,
                },
                force: false,
            },
            &DummyActionEmbedder,
        )
        .await
        .expect("legacy raw row and raw receipt must revert cleanly");
        assert!(!rev.forced);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A TransitionTask must not commit while a task handler holds the row's
    /// lock: otherwise a handler's stale load could be saved over the action's
    /// status and history. Both sides now take the same per-task lock.
    #[tokio::test]
    async fn action_transition_shares_the_task_lock_with_handlers() {
        let tmp = std::env::temp_dir().join(format!("ns-action-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        let ns = "ns-action-lock";
        store.init(ns).await.unwrap();

        store
            .upsert(
                ns,
                "task-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Task 1".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "task": {
                            "title": "Task 1",
                            "status": "open",
                            "priority": 1,
                            "created_at": 1700000000,
                            "updated_at": 1700000000,
                            "history": []
                        }
                    }),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                ns,
                "act-lock",
                vec![0.1; 4],
                MemoryPayload {
                    content: "Action: lock-transition".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "action_type".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "action_type": {
                            "slug": "lock-transition",
                            "title": "Lock Transition",
                            "description": "Transitions a task for the lock regression",
                            "required_role": "editor",
                            "parameters": {
                                "task_id": {
                                    "param_type": "memory_id",
                                    "required": true,
                                    "target_memory_type": "task"
                                },
                                "reason": { "param_type": "string", "required": true }
                            },
                            "submission_criteria": [],
                            "effects": [{
                                "op": "transition_task",
                                "target_param": "task_id",
                                "new_status": "blocked",
                                "note_template": "Blocked on {{reason}}"
                            }]
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let inv = ActionInvocation {
            namespace: ns.to_string(),
            slug: "lock-transition".to_string(),
            security: ActionSecurityContext {
                authenticated_principal: "alice".to_string(),
                requested_principal: None,
            },
            params: BTreeMap::from([
                ("task_id".to_string(), json!("task-1")),
                ("reason".to_string(), json!("upstream valve maintenance")),
            ]),
        };

        // A handler holds the row lock across its load -> save window.
        let task_lock = crate::task::claim_lock("task-1");
        let guard = task_lock.lock().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            apply_action(&store, &inv, &DummyActionEmbedder),
        )
        .await;
        assert!(
            blocked.is_err(),
            "the action waited on the shared task lock"
        );
        drop(guard);

        let outcome = apply_action(&store, &inv, &DummyActionEmbedder)
            .await
            .expect("the action commits once the handler releases the lock");
        assert!(outcome.is_applied(), "{outcome:?}");
        let (_, payload) = store.get(ns, "task-1").await.unwrap().unwrap();
        assert_eq!(payload.metadata["task"]["status"], json!("blocked"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
