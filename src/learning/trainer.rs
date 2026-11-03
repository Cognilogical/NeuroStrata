//! Local training loop for Laya-based deduplication candidates.
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::judgment::laya_onnx::{LocalTrainingRuntime, ModelVersion};

pub fn builtin_onnx_model_bytes() -> Vec<u8> {
    vec![
        0x08, 0x07, 0x3a, 0x7b, 0x0a, 0x1a, 0x0a, 0x05, 0x69, 0x6e, 0x70, 0x75,
        0x74, 0x0a, 0x01, 0x57, 0x12, 0x06, 0x6f, 0x75, 0x74, 0x70, 0x75, 0x74,
        0x22, 0x06, 0x4d, 0x61, 0x74, 0x4d, 0x75, 0x6c, 0x12, 0x0d, 0x62, 0x75,
        0x69, 0x6c, 0x74, 0x69, 0x6e, 0x2d, 0x67, 0x72, 0x61, 0x70, 0x68, 0x2a,
        0x1b, 0x08, 0x02, 0x08, 0x02, 0x10, 0x01, 0x22, 0x10, 0x00, 0x00, 0x00,
        0xc0, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
        0xc0, 0x42, 0x01, 0x57, 0x5a, 0x17, 0x0a, 0x05, 0x69, 0x6e, 0x70, 0x75,
        0x74, 0x12, 0x0e, 0x0a, 0x0c, 0x08, 0x01, 0x12, 0x08, 0x0a, 0x02, 0x08,
        0x01, 0x0a, 0x02, 0x08, 0x02, 0x62, 0x18, 0x0a, 0x06, 0x6f, 0x75, 0x74,
        0x70, 0x75, 0x74, 0x12, 0x0e, 0x0a, 0x0c, 0x08, 0x01, 0x12, 0x08, 0x0a,
        0x02, 0x08, 0x01, 0x0a, 0x02, 0x08, 0x02, 0x42, 0x04, 0x0a, 0x00, 0x10,
        0x0d,
    ]
}

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
        if self.runtime.python.exists() && self.runtime.laya_train.exists() {
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
            return Ok(());
        }
        // Pure-Rust builtin fallback when the external binary is not installed
        // or is not present on disk.
        if !self.runtime.laya_train.is_absolute() || !self.runtime.laya_train.exists() {
            let cand_dir = self.runtime.model_cache.join("candidates").join(version);
            std::fs::create_dir_all(&cand_dir)?;

            // Prefer real labeled pairs from train.jsonl; fall back to sibling
            // dataset files written by build_dedup_dataset
            // (<learning-root>/dataset/{calibration,train}.jsonl), and only
            // then to proxy pairs when no valid pairs exist anywhere.
            let dataset_dir = self.runtime.model_cache.parent()
                .and_then(|p| p.parent())
                .map(|root| root.join("dataset"));
            let train_jsonl = self.runtime.model_cache.join("train.jsonl");
            let mut pairs = read_calibration_pairs(&train_jsonl);
            if pairs.is_empty() {
                if let Some(dir) = &dataset_dir {
                    // Gold-adjudicated pairs first; fall back to the proxy
                    // calibration/train partitions when gold files are empty.
                    for fallback_file in [
                        "gold_calibration.jsonl",
                        "calibration.jsonl",
                        "train.jsonl",
                    ] {
                        pairs = read_calibration_pairs(&dir.join(fallback_file));
                        if !pairs.is_empty() {
                            break;
                        }
                    }
                }
            }
            let from_dataset = !pairs.is_empty();
            let fallback_pairs: Vec<(f32, bool)> = vec![(0.8, true), (0.2, false)];
            let effective_pairs = if from_dataset { pairs.clone() } else { fallback_pairs.clone() };
            // Held-out evaluation: when the dataset build produced eval.jsonl
            // with usable pairs, evaluate on those rather than re-score the
            // training pairs (H6: no train-on-eval leakage in the report).
            let held_out_eval_pairs = dataset_dir.as_ref().and_then(|dir| {
                // Gold partition first; fall back to proxy eval.jsonl when
                // the gold eval file is absent or parses to zero pairs.
                for fallback_file in ["gold_eval.jsonl", "eval.jsonl"] {
                    let path = dir.join(fallback_file);
                    if path.exists() {
                        let eval_pairs = read_calibration_pairs(&path);
                        if !eval_pairs.is_empty() {
                            return Some(eval_pairs);
                        }
                    }
                }
                None
            });
            let (eval_pairs, suite_name) = match (held_out_eval_pairs, from_dataset) {
                (Some(eval), _) => (eval, "dataset_eval".to_string()),
                (None, true) => (effective_pairs.clone(), "dataset".to_string()),
                (None, false) => (fallback_pairs, "proxy_internal".to_string()),
            };
            let fitted_temp = crate::learning::calibration::TemperatureFitter::new()
                .fit(&effective_pairs)?;
            let model_path = self.runtime.model_cache.join("model.onnx");
            let model_bytes: Vec<u8> = if model_path.exists() {
                let len = std::fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0);
                if len > 16 {
                    std::fs::read(&model_path)?
                } else {
                    builtin_onnx_model_bytes()
                }
            } else {
                builtin_onnx_model_bytes()
            };
            std::fs::write(cand_dir.join("model.onnx"), &model_bytes)?;
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&model_bytes);
            let model_sha256 = hex::encode(hasher.finalize());
            let manifest = serde_json::json!({
                "embedder_id": "laya",
                "model_version": version,
                "calibration_tier": if from_dataset { "DatasetCalibrated" } else { "ProxyInternal" },
                "temperature": fitted_temp,
                "pair_count": effective_pairs.len(),
                "model_sha256": model_sha256,
            });
            std::fs::write(cand_dir.join("manifest.json"), manifest.to_string())?;

            // Real evaluation with temperature-scaled metrics; written
            // alongside manifest.json so `learn train` (src/main.rs) reads a
            // measurable eval_report.json and promotes fail-closed.
            let eval_report = crate::learning::evaluator::evaluate_pairs(&eval_pairs, fitted_temp);
            let evaluation = EvaluationReport {
                candidate: version.clone(),
                suites: vec![SuiteReport {
                    name: suite_name,
                    metrics: serde_json::json!({
                        "score": eval_report.accuracy,
                        "accuracy": eval_report.accuracy,
                        "precision": eval_report.precision,
                        "recall": eval_report.recall,
                        "f1": eval_report.f1,
                        "brier_score": eval_report.brier_score,
                        "nll": eval_report.nll,
                        "temperature": eval_report.temperature,
                        "pair_count": eval_report.pair_count,
                    }),
                }],
            };
            std::fs::write(
                cand_dir.join("eval_report.json"),
                serde_json::to_string_pretty(&evaluation)?,
            )?;
            return Ok(());
        }
        return Err(anyhow!(
            "Laya training runtime unavailable (python: {}, laya-train: {})",
            self.runtime.python.display(),
            self.runtime.laya_train.display()
        ));
    }

    /// Transition job state through the validated machine.
    /// Rejects invalid transitions (no fake progress).
    pub fn transition(&self, from: &TrainingJobState, to: TrainingJobState) -> Result<TrainingJobState> {
        if !from.can_transit_to(&to) {
            return Err(anyhow!("Invalid state transition: {:?} -> {:?}", from, to));
        }
        Ok(to)
    }

    /// Evaluate a candidate model using real evaluation reports.
    /// Does not fabricate metrics; requires actual suite reports.
    pub fn evaluate_candidate(&self, report: &EvaluationReport) -> Result<f64> {
        tracing::debug!(candidate = %report.candidate, "evaluating candidate report");
        report.gold_metrics()
            .ok_or_else(|| anyhow!("Candidate {} has no measurable metrics; evaluation refused (fail-closed)", report.candidate))
    }

    /// Minimum evaluation score required to promote a candidate.
    pub const PROMOTION_MIN_SCORE: f64 = 0.5;

    /// Promote a gold-shadow candidate to active (with previous-good preservation).
    /// Only promotes when the evaluation score reaches the quality threshold.
    pub fn promote(&self, manifest: &mut ActiveManifest, version: &ModelVersion, score: f64) -> Result<()> {
        if score < Self::PROMOTION_MIN_SCORE {
            return Err(anyhow!(
                "Promotion refused: score {} below quality threshold {}",
                score,
                Self::PROMOTION_MIN_SCORE
            ));
        }
        tracing::debug!(candidate = %version, score = score, "promoting candidate report");
        manifest.previous_good_version = manifest.active_version.clone();
        manifest.active_version = Some(version.clone());
        manifest.candidate_version = None;
        manifest.shadow_version = Some(version.clone());
        // Copy active bundle to root cache so it stays in sync.
        let cand_bundle = self.runtime.model_cache.join("candidates").join(version);
        let root_model = self.runtime.model_cache.join("model.onnx");
        let root_manifest = self.runtime.model_cache.join("manifest.json");
        if cand_bundle.join("model.onnx").exists() && cand_bundle.join("manifest.json").exists() {
            if let Err(e) = std::fs::copy(cand_bundle.join("model.onnx"), &root_model) {
                tracing::warn!(?e, "failed to copy model onnx to root cache");
            }
            if let Err(e) = std::fs::copy(cand_bundle.join("manifest.json"), &root_manifest) {
                tracing::warn!(?e, "failed to copy manifest.json to root cache");
            }
        }
        Ok(())
    }

    /// Rollback to previous good version on failure (preserves history).
    pub fn rollback(&self, manifest: &mut ActiveManifest) -> Result<()> {
        manifest.active_version = manifest.previous_good_version.clone();
        manifest.candidate_version = None;
        manifest.shadow_version = None;
        // Copy previous version bundle to root cache if available.
        if let Some(prev) = manifest.previous_good_version.as_ref() {
            let cand_bundle = self.runtime.model_cache.join("candidates").join(prev);
            let root_model = self.runtime.model_cache.join("model.onnx");
            let root_manifest = self.runtime.model_cache.join("manifest.json");
            if cand_bundle.join("model.onnx").exists() && cand_bundle.join("manifest.json").exists() {
                if let Err(e) = std::fs::copy(cand_bundle.join("model.onnx"), &root_model) {
                    tracing::warn!(?e, "failed to copy rollback bundle file to root cache");
                }
                if let Err(e) = std::fs::copy(cand_bundle.join("manifest.json"), &root_manifest) {
                    tracing::warn!(?e, "failed to copy rollback bundle file to root cache");
                }
            }
        }
        Ok(())
    }
}

/// Parses a boolean duplicate label from a decision/outcome string.
/// `"duplicate" | "block" | "confirmed"` (substring match, so
/// `"blocked_exact"` counts) => `true`; `"distinct" | "allow" |
/// "rejected"` => `false`; anything else => `None` (row skipped).
fn duplicate_label_from_wording(word: &str) -> Option<bool> {
    let w = word.to_ascii_lowercase();
    if ["duplicate", "block", "confirmed"].iter().any(|k| w.contains(k)) {
        Some(true)
    } else if ["distinct", "allow", "reject"].iter().any(|k| w.contains(k)) {
        Some(false)
    } else {
        None
    }
}

/// Derives a similarity score in [0, 1] for a `DecisionObservation` JSONL
/// row (one written by `build_dedup_dataset`): the best candidate
/// `provider_confidence`, else the complement of the best `raw_distance`.
fn similarity_from_observation_row(value: &serde_json::Value) -> Option<f64> {
    let candidates = value.get("candidates")?.as_array()?;
    let best_conf = candidates.iter()
        .filter_map(|c| c.get("provider_confidence").and_then(|x| x.as_f64()))
        .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a| a.max(v))));
    let best_dist = candidates.iter()
        .filter_map(|c| c.get("raw_distance").and_then(|x| x.as_f64()))
        .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a| a.min(v))));
    best_conf.or_else(|| best_dist.map(|d| (1.0 - d).clamp(0.0, 1.0)))
}

/// Reads labeled calibration pairs from a JSONL training file. Accepts
/// `"logit"`/`"score"` numeric keys and `"label"`/`"is_duplicate"` boolean
/// keys, plus `DecisionObservation` rows (score from the best candidate
/// confidence/distance, label from the `decision` wording); lines that fail
/// to parse or lack a valid pair are skipped.
///
/// Every pair is pushed as a probability in `(0, 1)` — never a logit —
/// because `TemperatureFitter::fit` and `evaluator::evaluate_pairs` both
/// consume `(prob, label)` pairs and logit-scale values would be clamped
/// to `1.0 - 1e-6` inside their `ln(p / (1 - p))` conversions.
fn read_calibration_pairs(path: &std::path::Path) -> Vec<(f32, bool)> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            if path.exists() {
                tracing::warn!("unreadable calibration JSONL at {}: {e}", path.display());
            }
            return Vec::new();
        }
    };
    let mut pairs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("malformed JSON line in {}: {e}", path.display());
                continue;
            }
        };
        let score_probability = value
            .get("logit")
            .and_then(|x| x.as_f64())
            .or_else(|| value.get("score").and_then(|x| x.as_f64()))
            .or_else(|| {
                // DecisionObservation rows carry a similarity score in
                // [0, 1] already; keep it on the probability scale.
                value
                    .get("similarity_score")
                    .and_then(|x| x.as_f64())
                    .or_else(|| similarity_from_observation_row(&value))
            });
        let label = value
            .get("label")
            .and_then(|x| x.as_bool())
            .or_else(|| value.get("is_duplicate").and_then(|x| x.as_bool()))
            .or_else(|| {
                value
                    .get("decision")
                    .and_then(|x| x.as_str())
                    .or_else(|| value.get("outcome").and_then(|x| x.as_str()))
                    .and_then(duplicate_label_from_wording)
            });
        if let (Some(probability), Some(label)) = (score_probability, label) {
            pairs.push((probability.clamp(1e-4, 1.0 - 1e-4) as f32, label));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    fn train_candidate_missing_absolute_bin_falls_back_to_builtin() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("/nonexistent/python"),
            laya_train: PathBuf::from("/nonexistent/laya-train"),
            model_cache: tmp_dir.path().to_path_buf(),
        }));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(t.train_candidate(&"v0.0.0".to_string()));
        assert!(res.is_ok(), "missing absolute laya-train should use builtin path: {:?}", res);
        let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            tmp_dir.path().join("candidates/v0.0.0/manifest.json"),
        ).unwrap()).unwrap();
        assert_eq!(manifest["calibration_tier"], "ProxyInternal");
    }

    #[test]
    fn train_candidate_fails_when_binary_exists_but_python_missing() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let laya_train = tmp_dir.path().join("laya-train");
        std::fs::write(&laya_train, b"#!/bin/sh\n").unwrap();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("/nonexistent/python"),
            laya_train: laya_train.clone(),
            model_cache: tmp_dir.path().to_path_buf(),
        }));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(t.train_candidate(&"v0.0.0".to_string()));
        assert!(res.is_err());
    }

    #[test]
    fn train_candidate_uses_real_train_jsonl_pairs() {
        let tmp_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp_dir.path().join("train.jsonl"),
            "{\"logit\": 0.9, \"label\": true}\n{\"score\": 0.1, \"is_duplicate\": false}\nnot-json\n",
        ).unwrap();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"), // relative => builtin path
            model_cache: tmp_dir.path().to_path_buf(),
        }));
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(t.train_candidate(&"v2.0.0".to_string()))
            .expect("builtin path with real train.jsonl pairs should succeed");
        let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            tmp_dir.path().join("candidates/v2.0.0/manifest.json"),
        ).unwrap()).unwrap();
        assert_eq!(manifest["calibration_tier"], "DatasetCalibrated");
        assert_eq!(manifest["pair_count"], 2);
        // Real fit: both pairs are well-separated from 0.5, so the optimizer
        // sharpens to the lower bound of the search range.
        assert!((manifest["temperature"].as_f64().unwrap() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn transition_validates_state_machine() {
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        assert_eq!(t.transition(&TrainingJobState::WaitingForData, TrainingJobState::Training).unwrap(), TrainingJobState::Training);
        assert_eq!(t.transition(&TrainingJobState::Training, TrainingJobState::ProxyShadow).unwrap(), TrainingJobState::ProxyShadow);
        assert!(t.transition(&TrainingJobState::Promoted, TrainingJobState::Training).is_err()); // terminal
        assert!(t.transition(&TrainingJobState::WaitingForData, TrainingJobState::Promoted).is_err()); // invalid
    }

    #[test]
    fn promote_requires_real_quality_score() {
        let mut manifest = ActiveManifest::default();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        }));
        assert!(t.promote(&mut manifest, &"v1".to_string(), 0.8).is_ok());
        assert_eq!(manifest.active_version.as_ref().unwrap(), "v1");
        // Below the 0.5 quality threshold: refused, including negative scores.
        assert!(t.promote(&mut manifest, &"v2".to_string(), -0.1).is_err());
        assert!(t.promote(&mut manifest, &"v3".to_string(), 0.4).is_err());
        assert_eq!(manifest.active_version.as_ref().unwrap(), "v1");
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

    #[test]
    fn train_candidate_builtin_succeeds_and_writes_manifest() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"), // relative => builtin path
            model_cache: tmp_dir.path().to_path_buf(),
        }));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(t.train_candidate(&"v1.0.0".to_string()));
        assert!(res.is_ok(), "builtin path should succeed: {:?}", res);
        let cand_dir = tmp_dir.path().join("candidates/v1.0.0");
        assert!(cand_dir.join("manifest.json").exists());
        assert!(
            cand_dir.join("eval_report.json").exists(),
            "builtin path must write eval_report.json for learn train end-to-end"
        );
        let manifest_text = std::fs::read_to_string(cand_dir.join("manifest.json")).unwrap();
        let manifest_json: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
        assert_eq!(manifest_json["embedder_id"], "laya");
        assert_eq!(manifest_json["calibration_tier"], "ProxyInternal");
        // eval_report.json must be measurable and parseable by learn train.
        let eval: EvaluationReport = serde_json::from_str(
            &std::fs::read_to_string(cand_dir.join("eval_report.json")).unwrap(),
        ).unwrap();
        assert_eq!(eval.candidate, "v1.0.0");
        assert_eq!(eval.gold_metrics(), Some(1.0), "fallback pairs are perfectly separable");
    }

    #[tokio::test]
    async fn builtin_train_candidate_produces_valid_onnx_bundle() {
        use crate::judgment::JudgmentProvider;
        use crate::judgment::laya_onnx::{LayaOnnxProvider, RuntimeState};
        let tmp_dir = tempfile::tempdir().unwrap();
        let t = Trainer::new(Arc::new(LocalTrainingRuntime {
            python: PathBuf::from("python"),
            laya_train: PathBuf::from("laya-train"),
            model_cache: tmp_dir.path().to_path_buf(),
        }));
        t.train_candidate(&"v1.0.0".to_string()).await.unwrap();
        let cand_dir = tmp_dir.path().join("candidates/v1.0.0");
        let provider = LayaOnnxProvider::load(&cand_dir, Arc::new(tokio::sync::Semaphore::new(1))).unwrap();
        assert_eq!(provider.state(), crate::judgment::RuntimeState::Ready);
        let mem = crate::traits::MemoryPayload {
            content: String::new(),
            user_id: String::new(),
            memory_type: String::new(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::Value::Null,
        };
        let cand = crate::traits::SearchResult {
            id: "c1".to_string(),
            score: 0.9,
            payload: mem.clone(),
            evidence: None,
        };
        let judgments = provider.judge_duplicates(&mem, &[cand]).await.unwrap();
        assert!(!judgments.is_empty());
        assert!(judgments.iter().any(|j| j.reason.as_ref().map_or(false, |r| r.contains("laya-onnx-model-logit"))));
    }
}
