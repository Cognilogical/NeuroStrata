//! Local training loop for Laya-based deduplication candidates.
use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use crate::judgment::laya_onnx::{LocalTrainingRuntime, ModelVersion};

/// Training job state machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TrainingJobState {
    WaitingForData,
    Training,
    ProxyShadow,
    GoldShadow,
    Promoted,
    Failed(String),
}

/// A single suite report within an evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    pub name: String,
    pub metrics: serde_json::Value,
}

/// Evaluation report for a candidate model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub candidate: ModelVersion,
    pub suites: Vec<SuiteReport>,
}

impl EvaluationReport {
    pub fn gold_metrics(&self) -> Option<f64> {
        None
    }
}

/// Atomic replacement target for the promoted model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveManifest {
    pub active_version: Option<ModelVersion>,
    pub previous_good_version: Option<ModelVersion>,
    pub candidate_version: Option<ModelVersion>,
    pub shadow_version: Option<ModelVersion>,
    pub provider_mode: String,
}

impl Default for ActiveManifest {
    fn default() -> Self {
        Self {
            active_version: None,
            previous_good_version: None,
            candidate_version: None,
            shadow_version: None,
            provider_mode: "none".to_string(),
        }
    }
}

/// Local training runtime wrapper used by the coordinator.
pub struct Trainer {
    runtime: Arc<LocalTrainingRuntime>,
}

impl Trainer {
    pub fn new(runtime: Arc<LocalTrainingRuntime>) -> Self {
        Self { runtime }
    }

    /// Run a training job for candidate version.
    ///
    /// Laya training depends on an externally provisioned `laya-train`
    /// runtime and a validated model bundle. This method does not fabricate
    /// a training marker or report success when that runtime is absent.
    pub async fn train_candidate(&self, version: &ModelVersion) -> Result<()> {
        if !self.runtime.python.exists() || !self.runtime.laya_train.exists() {
            return Err(anyhow::anyhow!(
                "Laya training runtime is unavailable (python: {}, laya-train: {})",
                self.runtime.python.display(),
                self.runtime.laya_train.display()
            ));
        }
        // Real training is delegated to the provisioned laya-train runtime.
        // This repository does not bundle that runtime, so candidate training
        // is not available in this build.
        Err(anyhow::anyhow!(
            "Laya training is not wired to the local laya-train runtime"
        ))
    }

    /// Transition job state (no-ops in Phase-0 stub).
    pub fn transition(&self, state: TrainingJobState) -> TrainingJobState {
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_accepts_all_transitions() {
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        assert_eq!(t.transition(TrainingJobState::WaitingForData), TrainingJobState::WaitingForData);
        assert_eq!(t.transition(TrainingJobState::Training), TrainingJobState::Training);
        assert_eq!(t.transition(TrainingJobState::Promoted), TrainingJobState::Promoted);
    }

    #[test]
    fn evaluation_report_default_fields() {
        let report = EvaluationReport {
            candidate: "v0.0.0-bridge".to_string(),
            suites: vec![],
        };
        assert_eq!(report.candidate, "v0.0.0-bridge");
        assert!(report.gold_metrics().is_none());
    }

    #[tokio::test]
    async fn train_candidate_returns_err() {
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("/nonexistent/python"),
            laya_train: PathBuf::from("/nonexistent/laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        let res = t.train_candidate(&"v0.0.0".to_string()).await;
        assert!(res.is_err());
    }
}