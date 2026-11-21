//! Offline semantic competency and four-arm retrieval evaluation gate.
//!
//! The report is strict JSON and fails closed: any fixture, execution, or
//! threshold failure produces a FAIL receipt and a nonzero `--check` exit.

use std::path::Path;

use serde_json::{json, Value};

pub mod ablation;
pub mod competency;
pub mod metrics;

/// Result of one full semantic-evaluation run: the strict receipt and whether
/// every suite passed. Errors are carried inside the receipt, never swallowed.
#[derive(Debug)]
pub struct SemanticEvalOutcome {
    pub receipt: Value,
    pub passed: bool,
}

/// Run both suites and assemble the strict, honest evidence receipt.
pub async fn run_semantic_eval(
    competency_path: &Path,
    retrieval_path: &Path,
    iterations: usize,
) -> SemanticEvalOutcome {
    let mut errors: Vec<String> = Vec::new();

    let competency = match competency::run_fixture_file(competency_path).await {
        Ok(receipt) => Some(receipt),
        Err(e) => {
            errors.push(format!("competency: {e:#}"));
            None
        }
    };
    let retrieval = match ablation::run_ablation_file(retrieval_path, iterations).await {
        Ok(receipt) => Some(receipt),
        Err(e) => {
            errors.push(format!("retrieval: {e:#}"));
            None
        }
    };

    let competency_pass = competency
        .as_ref()
        .map(|r| r.verdict == "PASS")
        .unwrap_or(false);
    let retrieval_pass = retrieval
        .as_ref()
        .map(|r| r.verdict == "PASS")
        .unwrap_or(false);
    let passed = errors.is_empty() && competency_pass && retrieval_pass;

    let competency_value = match &competency {
        Some(r) => serde_json::to_value(r)
            .unwrap_or_else(|e| json!({"serialization_error": e.to_string()})),
        None => {
            json!({"error": errors.iter().find(|e| e.starts_with("competency:")).cloned().unwrap_or_default()})
        }
    };
    let retrieval_value = match &retrieval {
        Some(r) => serde_json::to_value(r)
            .unwrap_or_else(|e| json!({"serialization_error": e.to_string()})),
        None => {
            json!({"error": errors.iter().find(|e| e.starts_with("retrieval:")).cloned().unwrap_or_default()})
        }
    };
    let gate_violations: Vec<Value> = retrieval
        .as_ref()
        .map(|r| {
            r.gate
                .violations
                .iter()
                .map(|v| json!({"code": v.code, "detail": v.detail}))
                .collect()
        })
        .unwrap_or_default();
    let not_evaluated: Vec<&str> = ablation::NOT_EVALUATED.to_vec();

    let receipt = json!({
        "schema_version": 1,
        "package_version": env!("CARGO_PKG_VERSION"),
        "evidence_class": "synthetic_deterministic",
        "production_roi_claimed": false,
        "competency": competency_value,
        "retrieval": retrieval_value,
        "competency_pass": competency_pass,
        "retrieval_pass": retrieval_pass,
        "gate_violations": gate_violations,
        "errors": errors,
        "not_evaluated": not_evaluated,
        "verdict": if passed { "PASS" } else { "FAIL" },
    });

    SemanticEvalOutcome { receipt, passed }
}
