//! Learning status types and handlers for the daemon `learn status` surface.
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

use crate::config::DedupProviderMode;
use crate::judgment::laya_onnx::RuntimeState;
use crate::learning::trainer::{TrainingJobState, EvaluationReport};

/// Status snapshot returned by the daemon learning status handler.
///
/// `#[serde(default)]` at the struct level: older `status.json` files missing
/// newer fields deserialize into a complete `LearnStatus` instead of failing
/// the whole load.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LearnStatus {
    pub provider_mode: DedupProviderMode,
    pub active_version: Option<String>,
    pub previous_good_version: Option<String>,
    pub candidate_version: Option<String>,
    pub shadow_version: Option<String>,
    pub runtime_state: RuntimeState,
    pub training_state: TrainingJobState,
    pub pending_reason: Option<String>,
    pub unlabeled_observations: usize,
    pub observation_count: usize,
    pub labeled_pair_count: usize,
    pub counts_by_label_tier: BTreeMap<String, usize>,
    pub latest_evaluation: Option<EvaluationReport>,
    /// The last recorded control request (`control.json`), read at load time
    /// so status reporting names what the operator asked the loop to do.
    #[serde(default)]
    pub last_control: Option<crate::learning::control::LearningControlRequest>,
}

impl Default for LearnStatus {
    fn default() -> Self {
        Self {
            provider_mode: DedupProviderMode::None,
            active_version: None,
            previous_good_version: None,
            candidate_version: None,
            shadow_version: None,
            runtime_state: RuntimeState::Unavailable,
            training_state: TrainingJobState::WaitingForData,
            pending_reason: None,
            unlabeled_observations: 0,
            observation_count: 0,
            labeled_pair_count: 0,
            counts_by_label_tier: BTreeMap::new(),
            latest_evaluation: None,
            last_control: None,
        }
    }
}
