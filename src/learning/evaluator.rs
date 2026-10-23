//! Evaluation suite for candidate models.
use anyhow::Result;
use crate::judgment::CalibrationTier;

/// Report for a single evaluation suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    pub name: String,
    pub metrics: serde_json::Value,
}

/// Evaluation report for a candidate model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub candidate: String,  // Model version identifier
    pub suites: Vec<SuiteReport>,
}

impl EvaluationReport {
    /// Get gold metrics if available (currently None in Phase-0).
    pub fn gold_metrics(&self) -> Option<f64> {
        None
    }
}

impl Default for EvaluationReport {
    fn default() -> Self {
        Self {
            candidate: "v0.0.0-bridge".to_string(),
            suites: vec![],
        }
    }
}
