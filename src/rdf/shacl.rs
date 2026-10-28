use crate::traits::MemoryPayload;
use serde_json::Value;

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
    let mut out = Vec::new();

    if payload.content.trim().is_empty() {
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
        // location present implies lines; soft warning only if location present
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
            if let Some(source) = payload.metadata.get("source") {
                let blank = source.as_str().map(|s| s.trim().is_empty()).unwrap_or(true);
                if blank {
                    out.push(ShaclViolation {
                        field: "metadata.source".to_string(),
                        why: "source must be non-blank for a rule".to_string(),
                    });
                }
            } else {
                out.push(ShaclViolation {
                    field: "metadata.source".to_string(),
                    why: "source is required for a rule".to_string(),
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
        _ => {}
    }

    out
}
