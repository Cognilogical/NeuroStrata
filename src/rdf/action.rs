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
}
