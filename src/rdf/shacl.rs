use crate::traits::MemoryPayload;
use oxttl::TurtleParser;
use serde_json::Value;
use std::sync::OnceLock;

static EMBEDDED_SHAPES: &[(&str, &str)] = &[
    ("memory-shapes.v1.ttl", include_str!("../schemas/shacl/memory-shapes.v1.ttl")),
    ("fact.ttl", include_str!("../schemas/shacl/fact.ttl")),
    ("rule.ttl", include_str!("../schemas/shacl/rule.ttl")),
    ("attempt-shapes.v1.ttl", include_str!("../schemas/shacl/attempt-shapes.v1.ttl")),
    ("edge-shapes.v1.ttl", include_str!("../schemas/shacl/edge-shapes.v1.ttl")),
    ("topology-shapes.v1.ttl", include_str!("../schemas/shacl/topology-shapes.v1.ttl")),
    ("mesh-record-shapes.v1.ttl", include_str!("../schemas/shacl/mesh-record-shapes.v1.ttl")),
    ("multimodal-shapes.v1.ttl", include_str!("../schemas/shacl/multimodal-shapes.v1.ttl")),
];

pub fn embedded_ttl_shapes() -> &'static [(&'static str, &'static str)] {
    EMBEDDED_SHAPES
}

static EMBEDDED_SHAPE_COUNT: OnceLock<usize> = OnceLock::new();

pub fn parse_embedded_shapes() -> usize {
    *EMBEDDED_SHAPE_COUNT.get_or_init(|| {
        let mut count = 0usize;
        for (_, content) in embedded_ttl_shapes() {
            let parser = TurtleParser::new();
            let mut ok = true;
            for result in parser.for_reader(content.as_bytes()) {
                if result.is_err() {
                    ok = false;
                    break;
                }
            }
            if ok {
                count += 1;
            }
        }
        count
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaclViolation {
    pub field: String,
    pub why: String,
}

const TOPOLOGY_KEYS: [&str; 5] = [
    "component",
    "target_system",
    "channel",
    "credential_boundary",
    "reason",
];

pub fn validate_memory(payload: &MemoryPayload) -> Vec<ShaclViolation> {
    // Embedded .ttl SHACL schemas must parse cleanly. A decode error here
    // means the shipped schemas drifted — surface it as a violation instead
    // of silently validating against nothing.
    let loaded = parse_embedded_shapes();
    let mut out = Vec::new();
    if loaded < embedded_ttl_shapes().len() {
        out.push(ShaclViolation {
            field: "shacl.embedded_shapes".to_string(),
            why: format!(
                "only {}/{} embedded SHACL shapes parsed cleanly",
                loaded,
                embedded_ttl_shapes().len()
            ),
        });
    }

    if payload.content.trim().is_empty() && !is_structural(payload.memory_type.as_str()) {
        out.push(ShaclViolation {
            field: "content".to_string(),
            why: "content must be non-empty".to_string(),
        });
    }
    if payload.user_id.trim().is_empty() {
        out.push(ShaclViolation {
            field: "user_id".to_string(),
            why: "user_id must be non-empty".to_string(),
        });
    }
    if payload.memory_type.trim().is_empty() {
        out.push(ShaclViolation {
            field: "memory_type".to_string(),
            why: "memory_type must be non-empty".to_string(),
        });
    }
    if payload.location_lines.trim().is_empty() && !payload.location.trim().is_empty() {
        tracing::debug!("location '{}' present without location_lines; location implies lines", payload.location);
    }

    match &payload.metadata {
        Value::Null => {}
        Value::Object(_) => {}
        _ => {
            out.push(ShaclViolation {
                field: "metadata".to_string(),
                why: "metadata must be an object or null".to_string(),
            });
            return out;
        }
    }

    if let Some(domain) = payload.metadata.get("domain") {
        match domain {
            Value::String(_) => {}
            Value::Array(arr) => {
                if !arr.iter().all(|v| v.is_string()) {
                    out.push(ShaclViolation {
                        field: "metadata.domain".to_string(),
                        why: "domain array must contain only strings".to_string(),
                    });
                }
            }
            _ => {
                out.push(ShaclViolation {
                    field: "metadata.domain".to_string(),
                    why: "domain must be a string or array of strings".to_string(),
                });
            }
        }
    }

    if let Some(mentions) = payload.metadata.get("mentions") {
        match mentions {
            Value::Array(arr) => {
                if !arr.iter().all(|v| v.is_string()) {
                    out.push(ShaclViolation {
                        field: "metadata.mentions".to_string(),
                        why: "mentions must be an array of strings".to_string(),
                    });
                }
            }
            _ => {
                out.push(ShaclViolation {
                    field: "metadata.mentions".to_string(),
                    why: "mentions must be an array of strings".to_string(),
                });
            }
        }
    }

    if let Some(facets) = payload.metadata.get("facets") {
        match facets {
            Value::Array(arr) => {
                if !arr.iter().all(|v| v.is_string()) {
                    out.push(ShaclViolation {
                        field: "metadata.facets".to_string(),
                        why: "facets must be an array of strings".to_string(),
                    });
                }
            }
            _ => {
                out.push(ShaclViolation {
                    field: "metadata.facets".to_string(),
                    why: "facets must be an array of strings".to_string(),
                });
            }
        }
    }

    if let Some(score) = payload.metadata.get("feedback_score") {
        if let Some(n) = score.as_f64() {
            if n < -1.0 || n > 1.0 {
                out.push(ShaclViolation {
                    field: "metadata.feedback_score".to_string(),
                    why: "feedback_score must be in [-1, 1]".to_string(),
                });
            }
        } else {
            out.push(ShaclViolation {
                field: "metadata.feedback_score".to_string(),
                why: "feedback_score must be a number".to_string(),
            });
        }
    }

    if let Some(topology) = payload.metadata.get("topology") {
        match topology {
            Value::Object(fields) => {
                for key in TOPOLOGY_KEYS {
                    match fields.get(key) {
                        None => {
                            out.push(ShaclViolation {
                                field: format!("metadata.topology.{}", key),
                                why: "required key missing".to_string(),
                            });
                        }
                        Some(Value::String(_)) => {}
                        Some(_) => {
                            out.push(ShaclViolation {
                                field: format!("metadata.topology.{}", key),
                                why: "must be a string".to_string(),
                            });
                        }
                    }
                }
            }
            _ => {
                out.push(ShaclViolation {
                    field: "metadata.topology".to_string(),
                    why: "must be an object".to_string(),
                });
            }
        }
    }

    if let Some(attempt) = payload.metadata.get("attempt") {
        if !attempt.is_object() {
            out.push(ShaclViolation {
                field: "metadata.attempt".to_string(),
                why: "must be an object".to_string(),
            });
        }
    }

    // Required fields per memory_type (F18).
    match payload.memory_type.as_str() {
        "rule" => {
            // A rule carries provenance only if `has_source` reports true: a
            // non-empty string OR a non-empty object. Blank/whitespace string,
            // empty object {}, and other scalar types read as unsourced, so we
            // delegate to crate::traits::has_source rather than re-implementing
            // the rule — the task gate and search ranker already share this
            // predicate, so any future drift between them begins here.
            let sourced = crate::traits::has_source(&payload.metadata);
            if !sourced {
                out.push(ShaclViolation {
                    field: "metadata.source".to_string(),
                    why: if payload.metadata.get("source").is_none() {
                        "source is required for a rule".to_string()
                    } else {
                        "source must be non-blank for a rule".to_string()
                    },
                });
            }
            if let Some(enf) = payload.metadata.get("enforcement") {
                match enf.as_str() {
                    Some("ENFORCED") | Some("PARTIAL") | Some("NOT_ENFORCED") => {}
                    _ => {
                        out.push(ShaclViolation {
                            field: "metadata.enforcement".to_string(),
                            why: "enforcement must be ENFORCED, PARTIAL, or NOT_ENFORCED"
                                .to_string(),
                        });
                    }
                }
            }
        }
        "procedure" => {
            if let Some(trigger) = payload.metadata.get("trigger") {
                if let Some(s) = trigger.as_str() {
                    let valid = s == "session-start"
                        || s == "before-edit"
                        || s == "after-mutation"
                        || s.starts_with("every-n-sessions:")
                        || s.starts_with("every-n-days:");
                    if !valid {
                        out.push(ShaclViolation {
                            field: "metadata.trigger".to_string(),
                            why: "trigger must be a valid enum or every-n-sessions:N / every-n-days:N".to_string(),
                        });
                    }
                } else {
                    out.push(ShaclViolation {
                        field: "metadata.trigger".to_string(),
                        why: "trigger must be a string".to_string(),
                    });
                }
            } else {
                out.push(ShaclViolation {
                    field: "metadata.trigger".to_string(),
                    why: "trigger is required for a procedure".to_string(),
                });
            }
            if let Some(pc) = payload.metadata.get("performance_count") {
                if !pc.is_i64() && !pc.is_u64() {
                    out.push(ShaclViolation {
                        field: "metadata.performance_count".to_string(),
                        why: "performance_count must be an integer".to_string(),
                    });
                }
            }
        }
        "task" => {
            if let Some(task) = payload.metadata.get("task") {
                if !task.is_object() {
                    out.push(ShaclViolation {
                        field: "metadata.task".to_string(),
                        why: "task must be an object".to_string(),
                    });
                } else if let Some(status) = task.get("status") {
                    let ok = status
                        .as_str()
                        .map(|s| matches!(s, "open" | "in_progress" | "blocked" | "done"))
                        .unwrap_or(false);
                    if !ok {
                        out.push(ShaclViolation {
                            field: "metadata.task.status".to_string(),
                            why: "status must be open/in_progress/blocked/done".to_string(),
                        });
                    }
                }
            } else {
                out.push(ShaclViolation {
                    field: "metadata.task".to_string(),
                    why: "task object is required for a task memory".to_string(),
                });
            }
        }
        "digital_twin" => {
            let dt = payload
                .metadata
                .get("digital_twin")
                .or_else(|| payload.metadata.get("twin"));
            if let Some(dt) = dt {
                if let Some(obj) = dt.as_object() {
                    if obj.get("twin_id").or_else(|| obj.get("id")).and_then(|v| v.as_str()).map(|s| s.trim().is_empty()).unwrap_or(true) {
                        out.push(ShaclViolation {
                            field: "metadata.digital_twin.twin_id".to_string(),
                            why: "twin_id is required and must be a non-empty string".to_string(),
                        });
                    }
                    let observed_at = obj
                        .get("observed_at")
                        .or_else(|| payload.metadata.get("valid_from"));
                    if observed_at.and_then(|v| v.as_i64()).map(|ts| ts <= 0).unwrap_or(true) {
                        out.push(ShaclViolation {
                            field: "metadata.digital_twin.observed_at".to_string(),
                            why: "observed_at is required and must be a positive integer".to_string(),
                        });
                    }
                    let properties = obj.get("properties").or_else(|| obj.get("variables"));
                    if !properties.map(|v| v.is_object()).unwrap_or(false) {
                        out.push(ShaclViolation {
                            field: "metadata.digital_twin.properties".to_string(),
                            why: "properties is required and must be an object".to_string(),
                        });
                    }
                } else {
                    out.push(ShaclViolation {
                        field: "metadata.digital_twin".to_string(),
                        why: "digital_twin must be an object".to_string(),
                    });
                }
            } else {
                out.push(ShaclViolation {
                    field: "metadata.digital_twin".to_string(),
                    why: "digital_twin object is required for a digital_twin memory".to_string(),
                });
            }
        }
        _ => {}
    }

    if let Some(vt) = payload.metadata.get("visible_to") {
        for err in crate::rebac::validate_visible_to(vt) {
            out.push(ShaclViolation {
                field: "metadata.visible_to".to_string(),
                why: err,
            });
        }
    }

    if payload.metadata.get("attachments").is_some() {
        let cfg = crate::multimodal::MultimodalConfig::default();
        for w in crate::multimodal::validate_metadata_attachments(&payload.metadata, &cfg, None) {
            if w.is_strict_failure(cfg.require_dimensions) {
                out.push(ShaclViolation {
                    field: "metadata.attachments".to_string(),
                    why: w.to_string(),
                });
            }
        }
    }

    out
}

const ARTIFACT_TYPES: [&str; 6] = ["file", "directory", "markdown", "symbol", "code_ast", "doc_section"];
const CONTAINER_TYPES: [&str; 3] = ["directory", "file", "markdown"];
const CONTAINED_TYPES: [&str; 6] = ["directory", "file", "markdown", "symbol", "code_ast", "doc_section"];

fn is_structural(t: &str) -> bool {
    ARTIFACT_TYPES.contains(&t)
        || t == "freshness_flag"
        || t == "guard_event"
        || t == "community_summary"
        || t == "digital_twin"
        || t == "digital_twin_schema"
}

pub fn endpoint_role_allowed(rel_type: &str, source_type: &str, target_type: &str) -> bool {
    match rel_type {
        "GOVERNS" => source_type == "rule" || source_type == "fact",
        "CONTAINS" => (CONTAINED_TYPES.contains(&source_type) && CONTAINER_TYPES.contains(&target_type))
            || (source_type == "task" && target_type == "task"),
        "EXTRACTED_FROM" => !is_structural(source_type) && target_type == "task",
        _ => true,
    }
}

/// Enforce shapes at a boundary: returns Err if any violations.
pub fn enforce(payload: &crate::traits::MemoryPayload) -> std::result::Result<(), Vec<ShaclViolation>> {
    let v = validate_memory(payload);
    if v.is_empty() { Ok(()) } else { Err(v) }
}

/// Infer a minimal shape report from a payload (field presence).
pub fn infer_shape(payload: &crate::traits::MemoryPayload) -> Vec<String> {
    let mut fields = vec!["content".to_string(), "user_id".to_string(), "memory_type".to_string()];
    if let Some(obj) = payload.metadata.as_object() {
        for k in obj.keys() { fields.push(format!("metadata.{}", k)); }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_embedded_shapes_returns_count() {
        assert!(parse_embedded_shapes() > 0, "embedded SHACL .ttl shapes must parse");
    }

    #[test]
    fn endpoint_contains_task_task_and_rejects_invalid() {
        assert!(endpoint_role_allowed("CONTAINS", "task", "task"), "task contains task should be valid");
        assert!(!endpoint_role_allowed("CONTAINS", "rule", "fact"), "rule contains fact should be invalid");
    }

    #[test]
    fn rule_source_nonempty_object_is_sourced() {
        let payload = MemoryPayload {
            content: "x".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::json!({ "source": { "kind": "release-run", "ref": "v1.16.0" } }),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().all(|vi| vi.field != "metadata.source"), "object source must be sourced: {:?}", v);
    }

    #[test]
    fn rule_source_empty_object_is_blank() {
        let payload = MemoryPayload {
            content: "x".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::json!({ "source": {} }),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().any(|vi| vi.field == "metadata.source" && vi.why.contains("must be non-blank")), "empty object source is blank: {:?}", v);
    }

    #[test]
    fn rule_source_whitespace_string_is_blank() {
        let payload = MemoryPayload {
            content: "x".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::json!({ "source": "   " }),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().any(|vi| vi.field == "metadata.source" && vi.why.contains("must be non-blank")), "whitespace string is blank: {:?}", v);
    }

    #[test]
    fn rule_source_nonempty_string_is_sourced() {
        let payload = MemoryPayload {
            content: "x".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::json!({ "source": "owner 2026-10-08" }),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().all(|vi| vi.field != "metadata.source"), "string source must be sourced: {:?}", v);
    }

    #[test]
    fn rule_source_missing_is_required() {
        let payload = MemoryPayload {
            content: "x".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::json!({}),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().any(|vi| vi.field == "metadata.source" && vi.why.contains("source is required")), "missing source is required: {:?}", v);
    }

    #[test]
    fn rule_source_null_is_present_but_unusable() {
        // A key that is PRESENT but null is not the same as an absent key:
        // the absent case says "required", the present-but-unusable case says
        // "must be non-blank". Which message comes back tells the caller
        // whether to supply a source or to fix the one it already supplied.
        let payload = MemoryPayload {
            content: "x".to_string(),
            user_id: "u".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::json!({ "source": null }),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().any(|vi| vi.field == "metadata.source" && vi.why.contains("must be non-blank")), "a null source is present but unusable: {:?}", v);
        assert!(!v.iter().any(|vi| vi.field == "metadata.source" && vi.why.contains("source is required")), "a present source is not a missing source: {:?}", v);
    }

    #[test]
    fn validate_memory_file_empty_content_no_content_violation() {
        let payload = MemoryPayload {
            content: "".to_string(),
            user_id: "u1".to_string(),
            memory_type: "file".to_string(),
            agent_name: None,
            location: "".to_string(),
            location_lines: "".to_string(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        let v = validate_memory(&payload);
        assert!(v.iter().all(|vi| vi.field != "content"), "structural file with empty content must not emit content violation: got {:?}", v);
    }
}
