//! Learning control actions: pause, resume, rollback, reset, and purge.
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::learning::status::LearnStatus;
use crate::learning::trainer::ActiveManifest;

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

/// The local learning root: env override first, then the per-user data dir.
pub fn default_learning_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("NEUROSTRATA_LEARNING_DIR") {
        if !dir.trim().is_empty() {
            return Path::new(&dir).to_path_buf();
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("neurostrata")
        .join("learning")
}

const STATUS_FILE: &str = "status.json";
const MANIFEST_FILE: &str = "active_manifest.json";
const CONTROL_FILE: &str = "control.json";

/// Read the current status snapshot from `root/status.json`, defaulting to a
/// fresh `LearnStatus` when the file is absent (the loop has never run).
pub fn load_status(root: &Path) -> LearnStatus {
    std::fs::read_to_string(root.join(STATUS_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<LearnStatus>(&raw).ok())
        .unwrap_or_default()
}

/// Read the active manifest from `root/active_manifest.json` if present.
fn load_manifest(root: &Path) -> ActiveManifest {
    std::fs::read_to_string(root.join(MANIFEST_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<ActiveManifest>(&raw).ok())
        .unwrap_or_default()
}

/// Apply one control action against a concrete root directory and persist the
/// state: `control.json` records the request, `status.json` records the effect.
pub fn apply_control_at(
    root: &Path,
    request: &LearningControlRequest,
) -> Result<LearningControlResult, String> {
    if request.purge_observations {
        return Err("purge_observations is rejected while running".to_string());
    }
    std::fs::create_dir_all(root)
        .map_err(|e| format!("learning dir {}: {}", root.display(), e))?;
    let mut status = load_status(root);
    let mut manifest = load_manifest(root);

    match request.action {
        LearningControlAction::Pause => {
            status.pending_reason = Some("paused".to_string());
        }
        LearningControlAction::Resume => {
            status.pending_reason = None;
        }
        LearningControlAction::Rollback => {
            let manager = crate::judgment::laya_onnx::LearningRuntimeManager::new(
                crate::judgment::laya_onnx::LearningPaths::new(root.to_path_buf()),
            );
            let runtime = manager
                .runtime()
                .map_err(|e| format!("rollback runtime unavailable: {}", e))?;
            let trainer = crate::learning::trainer::Trainer::new(std::sync::Arc::new(runtime));
            trainer
                .rollback(&mut manifest)
                .map_err(|e| format!("rollback failed: {}", e))?;
            status.active_version = manifest.active_version.clone();
            status.candidate_version = manifest.candidate_version.clone();
            status.shadow_version = manifest.shadow_version.clone();
            persist_manifest(root, &manifest)?;
        }
        LearningControlAction::Reset => {
            status.candidate_version = None;
            status.shadow_version = None;
            status.training_state = crate::learning::trainer::TrainingJobState::WaitingForData;
        }
    }

    persist_control(root, request)?;
    persist_status(root, &status)?;

    Ok(LearningControlResult { ok: true, status })
}

fn persist_status(root: &Path, status: &LearnStatus) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(status)
        .map_err(|e| format!("status serialize: {}", e))?;
    std::fs::write(root.join(STATUS_FILE), raw)
        .map_err(|e| format!("status.json write: {}", e))
}

fn persist_manifest(root: &Path, manifest: &ActiveManifest) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("manifest serialize: {}", e))?;
    std::fs::write(root.join(MANIFEST_FILE), raw)
        .map_err(|e| format!("active_manifest.json write: {}", e))
}

fn persist_control(root: &Path, request: &LearningControlRequest) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(request)
        .map_err(|e| format!("control serialize: {}", e))?;
    std::fs::write(root.join(CONTROL_FILE), raw)
        .map_err(|e| format!("control.json write: {}", e))
}

/// Apply a control action against the default learning root.
pub fn apply_control(request: &LearningControlRequest) -> Result<LearningControlResult, String> {
    apply_control_at(&default_learning_dir(), request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ns-learning-control-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn purge_observations_rejected_while_running() {
        let req = LearningControlRequest {
            action: LearningControlAction::Reset,
            purge_observations: true,
        };
        assert!(apply_control_at(&temp_root(), &req)
            .err()
            .unwrap()
            .contains("purge_observations is rejected while running"));
    }

    #[test]
    fn pause_resume_round_trip_persists_status() {
        let root = temp_root();
        let req = LearningControlRequest {
            action: LearningControlAction::Pause,
            purge_observations: false,
        };
        let res = apply_control_at(&root, &req).unwrap();
        assert!(res.ok);
        assert_eq!(res.status.pending_reason.as_deref(), Some("paused"));
        // Persisted: a fresh load reads the pause back.
        assert_eq!(load_status(&root).pending_reason.as_deref(), Some("paused"));

        let res = apply_control_at(
            &root,
            &LearningControlRequest {
                action: LearningControlAction::Resume,
                purge_observations: false,
            },
        )
        .unwrap();
        assert!(res.status.pending_reason.is_none());
        assert!(load_status(&root).pending_reason.is_none());
    }

    #[test]
    fn reset_clears_derived_state() {
        let root = temp_root();
        let req = LearningControlRequest {
            action: LearningControlAction::Reset,
            purge_observations: false,
        };
        let res = apply_control_at(&root, &req).unwrap();
        assert!(res.status.candidate_version.is_none());
        assert!(res.status.shadow_version.is_none());
        assert_eq!(
            res.status.training_state,
            crate::learning::trainer::TrainingJobState::WaitingForData
        );
        assert!(root.join("control.json").exists());
        assert!(root.join("status.json").exists());
    }

    #[test]
    fn load_status_defaults_when_absent() {
        let root = temp_root();
        let status = load_status(&root);
        assert!(status.active_version.is_none());
        assert!(status.pending_reason.is_none());
        assert_eq!(
            status.training_state,
            crate::learning::trainer::TrainingJobState::WaitingForData
        );
    }
}
