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
/// The last recorded control action reads in from `root/control.json` so
/// status reporting reflects what the operator asked for, not just the
/// effect it had. A `status.json` / `active_manifest.json` that exists but
/// cannot be read or parsed is logged and then falls back to the default
/// rather than silently discarding operator state.
pub fn load_status(root: &Path) -> LearnStatus {
    let status_path = root.join(STATUS_FILE);
    let mut status: LearnStatus = if status_path.exists() {
        match std::fs::read_to_string(&status_path)
            .map_err(anyhow::Error::from)
            .and_then(|raw| serde_json::from_str::<LearnStatus>(&raw).map_err(anyhow::Error::from))
        {
            Ok(status) => status,
            Err(e) => {
                tracing::warn!(
                    "failed to load {} ({}); defaulting to a fresh LearnStatus",
                    status_path.display(),
                    e
                );
                LearnStatus::default()
            }
        }
    } else {
        LearnStatus::default()
    };
    if let Ok(control) = read_control(root) {
        status.last_control = control;
    }
    status
}

/// Read the active manifest from `root/active_manifest.json` if present,
/// logging (but not failing on) a manifest that exists yet cannot be parsed.
fn load_manifest(root: &Path) -> ActiveManifest {
    let manifest_path = root.join(MANIFEST_FILE);
    if !manifest_path.exists() {
        return ActiveManifest::default();
    }
    match std::fs::read_to_string(&manifest_path)
        .map_err(anyhow::Error::from)
        .and_then(|raw| serde_json::from_str::<ActiveManifest>(&raw).map_err(anyhow::Error::from))
    {
        Ok(manifest) => manifest,
        Err(e) => {
            tracing::warn!(
                "failed to load {} ({}); defaulting to a fresh ActiveManifest",
                manifest_path.display(),
                e
            );
            ActiveManifest::default()
        }
    }
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
            manifest.candidate_version = None;
            manifest.shadow_version = None;
            persist_manifest(root, &manifest)?;
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

/// Promoted (active) model version from the default learning root's
/// `active_manifest.json`. Production consumers (the Laya ONNX provider's
/// gating decision) use it to check whether their verdicts may block a write.
pub fn read_active_version() -> Option<String> {
    load_manifest(&default_learning_dir()).active_version
}

/// Read the last recorded control request from `root/control.json`.
/// Returns `Ok(None)` when no control action has been applied yet.
pub fn read_control(root: &Path) -> Result<Option<LearningControlRequest>, String> {
    let raw = match std::fs::read_to_string(root.join(CONTROL_FILE)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("control.json read: {}", e)),
    };
    serde_json::from_str::<LearningControlRequest>(&raw)
        .map(Some)
        .map_err(|e| format!("control.json parse: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicI64, Ordering};
        static SEQ: AtomicI64 = AtomicI64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ns-learning-control-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
                + SEQ.fetch_add(1, Ordering::Relaxed)
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
        // The persisted active manifest must also drop candidate/shadow refs.
        let manifest_text = std::fs::read_to_string(root.join("active_manifest.json")).unwrap();
        let manifest: ActiveManifest = serde_json::from_str(&manifest_text).unwrap();
        assert!(manifest.candidate_version.is_none(), "reset must clear manifest.candidate_version");
        assert!(manifest.shadow_version.is_none(), "reset must clear manifest.shadow_version");
    }

    #[test]
    fn read_control_returns_recorded_request() {
        let root = temp_root();
        assert!(
            read_control(&root).unwrap().is_none(),
            "no control action yet => None"
        );
        let req = LearningControlRequest {
            action: LearningControlAction::Pause,
            purge_observations: false,
        };
        apply_control_at(&root, &req).unwrap();
        let recorded = read_control(&root)
            .expect("read_control must succeed on a written control.json")
            .expect("control.json must contain the recorded request");
        assert!(matches!(recorded.action, LearningControlAction::Pause));
        assert!(!recorded.purge_observations);
    }

    #[test]
    fn read_control_fails_on_corrupt_json() {
        let root = temp_root();
        std::fs::write(root.join("control.json"), b"not json").unwrap();
        assert!(read_control(&root).is_err(), "corrupt control.json must error");
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

pub fn purge_observations_offline_at(root: &Path, confirm_purge: bool) -> Result<LearningControlResult, String> {
    if !confirm_purge {
        return Err("--purge-observations requires --confirm-purge".to_string());
    }
    for dir in ["observations", "blobs", "dataset"] {
        let p = root.join(dir);
        if p.exists() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    let res = apply_control_at(root, &LearningControlRequest {
        action: LearningControlAction::Reset,
        purge_observations: false,
    })?;
    let mut status = res.status;
    status.observation_count = 0;
    status.labeled_pair_count = 0;
    persist_status(root, &status)?;
    Ok(LearningControlResult { ok: true, status })
}

pub fn purge_observations_offline(confirm_purge: bool) -> Result<LearningControlResult, String> {
    purge_observations_offline_at(&default_learning_dir(), confirm_purge)
}

#[cfg(test)]
mod offline_purge_tests {
    use super::*;

    #[test]
    fn purge_observations_offline_requires_confirm_purge() {
        let dir = std::env::temp_dir().join(format!("ns-purge-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            purge_observations_offline_at(&dir, false).unwrap_err(),
            "--purge-observations requires --confirm-purge".to_string()
        );
    }

    #[test]
    fn purge_observations_offline_discards_observations_and_dataset() {
        let dir = std::env::temp_dir().join(format!("ns-purge-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("observations")).unwrap();
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        std::fs::create_dir_all(dir.join("dataset")).unwrap();
        std::fs::write(dir.join("status.json"), b"{}").unwrap();
        let res = purge_observations_offline_at(&dir, true).unwrap();
        assert!(res.ok);
        assert!(!dir.join("observations").exists());
        assert!(!dir.join("blobs").exists());
        assert!(!dir.join("dataset").exists());
        let status: LearnStatus = serde_json::from_str(&std::fs::read_to_string(dir.join("status.json")).unwrap()).unwrap();
        assert_eq!(status.observation_count, 0);
        assert_eq!(status.labeled_pair_count, 0);
        assert!(std::fs::read_to_string(dir.join("control.json")).unwrap().contains("\"action\""));
    }
}
