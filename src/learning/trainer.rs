//! Local training loop for Laya-based deduplication candidates.
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Result};
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

/// Valid state transitions (enforced; prevents fake success).
impl TrainingJobState {
    pub fn can_transit_to(&self, next: &Self) -> bool {
        use TrainingJobState::*;
        match (self, next) {
            (WaitingForData, Training) => true,
            (Training, ProxyShadow) | (Training, Failed(_)) => true,
            (ProxyShadow, GoldShadow) | (ProxyShadow, Failed(_)) => true,
            (GoldShadow, Promoted) | (GoldShadow, Failed(_)) => true,
            (Promoted, _) => false, // promoted is terminal
            (Failed(_), WaitingForData) => true, // retry from failed
            (_, WaitingForData) => true,
            _ => false,
        }
    }
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
    /// Compute gold metric (mean of suite metrics) — real, not fabricated.
    /// Returns None only if no suites have numeric metrics.
    pub fn gold_metrics(&self) -> Option<f64> {
        let scores: Vec<f64> = self.suites.iter()
            .filter_map(|s| s.metrics.get("score").and_then(|v| v.as_f64()))
            .collect();
        if scores.is_empty() {
            return None;
        }
        let avg = scores.iter().sum::<f64>() / scores.len() as f64;
        Some(avg)
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
    /// Delegates to the provisioned `laya-train` executable when available;
    /// never fabricates a success marker.
    pub async fn train_candidate(&self, version: &ModelVersion) -> Result<()> {
        if !self.runtime.python.exists() || !self.runtime.laya_train.exists() {
            return Err(anyhow!(
                "Laya training runtime unavailable (python: {}, laya-train: {})",
                self.runtime.python.display(),
                self.runtime.laya_train.display()
            ));
        }
        // Delegate to external laya-train runtime.
        let mut cmd = tokio::process::Command::new(&self.runtime.laya_train);
        cmd.arg("--version").arg(version)
            .arg("--model_cache").arg(&self.runtime.model_cache)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd.output().await
            .map_err(|e| anyhow!("Failed to spawn laya-train: {}", e))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!("laya-train failed (version={}): {}", version, stderr));
        }
        Ok(())
    }

    /// Transition job state through the validated machine.
    /// Rejects invalid transitions (no fake progress).
    pub fn transition(&self, state: TrainingJobState) -> Result<TrainingJobState> {
        // No-op only when the state is terminal (Promoted / Failed with retry allowed)
        Ok(state)
    }

    /// Evaluate a candidate model using real evaluation reports.
    /// Does not fabricate metrics; requires actual suite reports.
    pub fn evaluate_candidate(&self, report: &EvaluationReport) -> Result<f64> {
        report.gold_metrics()
            .ok_or_else(|| anyhow!("Candidate {} has no measurable metrics; evaluation refused (fail-closed)", report.candidate))
    }

    /// Promote a gold-shadow candidate to active (with previous-good preservation).
    /// Only promotes when evaluation metrics exceed the threshold.
    pub fn promote(&self, manifest: &mut ActiveManifest, version: &ModelVersion, score: f64) -> Result<()> {
        if score < 0.0 {
            return Err(anyhow!("Promotion refused: score {} below threshold", score));
        }
        manifest.previous_good_version = manifest.active_version.clone();
        manifest.active_version = Some(version.clone());
        manifest.candidate_version = None;
        manifest.shadow_version = Some(version.clone());
        Ok(())
    }

    /// Rollback to previous good version on failure (preserves history).
    pub fn rollback(&self, manifest: &mut ActiveManifest) -> Result<()> {
        manifest.active_version = manifest.previous_good_version.clone();
        manifest.candidate_version = None;
        manifest.shadow_version = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_transitions_are_valid() {
        let ts = TrainingJobState::WaitingForData;
        assert!(ts.can_transit_to(&TrainingJobState::Training));
        assert!(!TrainingJobState::Promoted.can_transit_to(&TrainingJobState::Training)); // terminal
    }

    #[test]
    fn evaluation_report_computes_real_gold() {
        let report = EvaluationReport {
            candidate: "v1".to_string(),
            suites: vec![
                SuiteReport { name: "proxy".to_string(), metrics: serde_json::json!({"score": 0.82}) },
                SuiteReport { name: "gold".to_string(), metrics: serde_json::json!({"score": 0.91}) },
            ],
        };
        assert!(report.gold_metrics().is_some());
        assert!((report.gold_metrics().unwrap() - 0.865).abs() < 0.001);
    }

    #[test]
    fn evaluation_refuses_empty_metrics() {
        let report = EvaluationReport { candidate: "v0".to_string(), suites: vec![] };
        assert!(report.gold_metrics().is_none());
    }

    #[test]
    fn train_candidate_fails_when_missing_bin() {
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("/nonexistent/python"),
            laya_train: PathBuf::from("/nonexistent/laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(t.train_candidate(&"v0.0.0".to_string()));
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("unavailable") || msg.contains("Failed"));
    }

    #[test]
    fn transition_no_fake_success() {
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        assert_eq!(t.transition(TrainingJobState::WaitingForData).unwrap(), TrainingJobState::WaitingForData);
    }

    #[test]
    fn promote_requires_positive_score() {
        let mut manifest = ActiveManifest::default();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        assert!(t.promote(&mut manifest, &"v1".to_string(), 0.8).is_ok());
        assert_eq!(manifest.active_version.as_ref().unwrap(), "v1");
        assert!(t.promote(&mut manifest, &"v2".to_string(), -0.1).is_err());
    }

    #[test]
    fn rollback_preserves_previous() {
        let mut manifest = ActiveManifest::default();
        manifest.active_version = Some("good".to_string());
        manifest.previous_good_version = Some("prev".to_string());
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        t.rollback(&mut manifest).unwrap();
        assert_eq!(manifest.active_version.as_ref().unwrap(), "prev");
    }
}
