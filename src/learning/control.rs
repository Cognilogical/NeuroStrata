//! Learning control actions: pause, resume, rollback, reset, and purge.
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

use crate::config::DedupProviderMode;
use crate::judgment::laya_onnx::RuntimeState;
use crate::learning::status::LearnStatus;
use crate::learning::trainer::TrainingJobState;

/// Control action for the learning loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningControlAction { Pause, Resume, Rollback, Reset }

/// Strict control request for `POST /learning/control`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningControlRequest {
    pub action: LearningControlAction,
    pub purge_observations: bool,
}

/// Result of a control action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningControlResult {
    pub ok: bool,
    pub status: LearnStatus,
}

/// Apply a control action. No-op persistence for Phase-0; full wiring in Task 3.
pub fn apply_control(request: &LearningControlRequest) -> Result<LearningControlResult, String> {
    if request.purge_observations {
        return Err("purge_observations is rejected while running".to_string());
    }
    let _ = request;
    let status = LearnStatus::default();
    Ok(LearningControlResult { ok: true, status })
}
