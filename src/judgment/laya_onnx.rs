/// Local Laya ONNX inference provider implementing `JudgmentProvider` for
/// deduplication candidate judgment. Uses canonical Rust `noul` questions and a
/// provisioned local ONNX bundle path (`allow_download = false`).
///
/// Inference is NOT implemented in this build: `load` validates the bundle
/// and the provider reports `RuntimeState::Unavailable`, `judge_duplicates`
/// returns an error, and `health_check` returns `Ok(false)`. No ONNX session
/// is ever created here.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::judgment::{CalibrationTier, DuplicateJudgment, JudgmentProvider};
use crate::traits::{MemoryPayload, SearchResult};

/// A `JudgmentProvider` for Laya ONNX models loaded and validated by the
/// runtime system. In this build the provider is a validated stub: no ONNX
/// session is created, so it reports `Unavailable` (see module docs).
pub struct LayaOnnxProvider {
    /// Path to the validated ONNX bundle directory (manifest + checkpoint).
    bundle_root: PathBuf,
    /// ONNX Runtime inference semaphore, bounding concurrent CPU inference.
    inference_semaphore: Arc<Semaphore>,
    /// The last-known runtime state of this provider.
    state: crate::judgment::RuntimeState,
}

impl LayaOnnxProvider {
    /// Load an ONNX bundle from `bundle_dir`, attaching it to the inference
    /// semaphore. Validates the bundle layout (manifest, checkpoint paths,
    /// embedder identity) before attaching.
    pub fn load(bundle_dir: &Path, semaphore: Arc<Semaphore>) -> Result<Self> {
        // Phase-0 spike validation: ensure the bundle root exists and has the
        // minimum expected artifacts. Full manifest parsing deferred to bridge.
        let canonical = std::fs::canonicalize(bundle_dir)?;
        let manifest_path = canonical.join("manifest.json");
        if !manifest_path.exists() {
            return Err(anyhow!("Bundle missing manifest.json"));
        }
        // Read manifest to validate embedder identity
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow!(format!("Cannot read manifest: {e}")))?;
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).map_err(|e| anyhow!(format!("Bad manifest JSON: {e}")))?;
        let embedder_id = manifest["embedder_id"].as_str().ok_or_else(|| {
            anyhow!("Manifest missing embedder_id")
        })?;
        let _version = manifest["model_version"].as_str();

        Ok(Self {
            bundle_root: canonical,
            inference_semaphore: semaphore,
            // No ONNX session is created by `load` in this build, so the
            // provider is not Ready: report Unavailable, consistent with
            // `judge_duplicates` erroring and `health_check` returning false.
            state: crate::judgment::RuntimeState::Unavailable,
        })
    }

    /// Probe the provider's runtime state. Does not attempt inference.
    pub fn state(&self) -> crate::judgment::RuntimeState {
        self.state
    }
}

#[async_trait]
impl JudgmentProvider for LayaOnnxProvider {
    /// Judge whether a new memory is a duplicate of existing ones.
    async fn judge_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>> {
        // Inference runs inside spawn_blocking under the semaphore; the permit
        // is acquired first so concurrent requests are bounded.
        let _permit = self.inference_semaphore.acquire().await.unwrap();
        let n = candidates.len();
        // R10: Inference is not implemented in this build.
        let _ = (&new_memory, n);
        Err(anyhow!("Laya ONNX inference is not available in this build"))
    }

    /// Health check - verify the provider is operational.
    async fn health_check(&self) -> Result<bool> {
        // R10: Provider is not operational.
        Ok(false)
    }

    /// Provider name for logging/metrics.
    fn name(&self) -> &str {
        "laya-onnx"
    }
}

/// Convenience type for a version string identifying a Laya model.
pub type ModelVersion = String;

/// A collection of learning paths (train JSONL, eval JSONL, model directories).
#[derive(Clone)]
pub struct LearningPaths {
    pub root: PathBuf,
}

impl LearningPaths {
    pub fn train_jsonl(&self) -> PathBuf {
        self.root.join("train.jsonl")
    }

    pub fn proxy_eval_jsonl(&self) -> PathBuf {
        self.root.join("proxy_eval.jsonl")
    }

    pub fn base_model_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn candidate_dir(&self, version: &ModelVersion) -> PathBuf {
        self.root.join("candidates").join(version)
    }
}

/// Local training runtime state: pinned Python executable, `laya-train`
/// executable path, and model cache directory.
#[derive(Clone)]
pub struct LocalTrainingRuntime {
    pub python: PathBuf,
    pub laya_train: PathBuf,
    pub model_cache: PathBuf,
}

/// Local runtime state consumed by `learn status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Ready,
    Provisioning,
    Unavailable,
}

/// Manages the local Laya training/runtime environment: provisioning a pinned
/// Python runtime, `laya-train`, and the selected base checkpoint; starting
/// and querying the runtime in the background without blocking daemon startup.
pub struct LearningRuntimeManager {
    paths: LearningPaths,
}

impl LearningRuntimeManager {
    /// Ensure the runtime is ready (provisioned or ready from cache). Returns
    /// `RuntimeState` reflecting the current status.
    pub async fn ensure_ready(&self) -> RuntimeState {
        // The local Laya training runtime is not provisioned in this build.
        // Report Unavailable honestly rather than claiming Ready (mirrors
        // the canonical manager in `learning::runtime`).
        RuntimeState::Unavailable
    }

    /// Return the local training runtime configuration.
    pub fn runtime(&self) -> Result<LocalTrainingRuntime> {
        // Phase-0: return minimal defaults; full provisioning deferred to Task 3.
        let python = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("python")))
            .unwrap_or_else(|| PathBuf::from("python"));
        Ok(LocalTrainingRuntime {
            python,
            laya_train: PathBuf::from("laya-train"),
            model_cache: PathBuf::from(".cache/laya"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_bundle_dir(label: &str, manifest: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ns-laya-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), manifest).unwrap();
        dir
    }

    fn payload() -> MemoryPayload {
        MemoryPayload {
            content: "hello".to_string(),
            user_id: "u1".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn health_check_reflects_provider_state() {
        let dir = temp_bundle_dir(
            "state",
            r#"{"embedder_id":"test-embedder","model_version":"v-test"}"#,
        );
        let provider = LayaOnnxProvider::load(&dir, Arc::new(Semaphore::new(1))).unwrap();
        // `load` creates no ONNX session in this build, so the provider must
        // not report Ready, and its health check must agree with its state.
        assert_eq!(provider.state(), RuntimeState::Unavailable);
        assert!(!provider.health_check().await.unwrap());
    }

    #[tokio::test]
    async fn judge_duplicates_reports_unavailable() {
        let dir = temp_bundle_dir("judge", r#"{"embedder_id":"test-embedder"}"#);
        let provider = LayaOnnxProvider::load(&dir, Arc::new(Semaphore::new(1))).unwrap();
        let err = provider
            .judge_duplicates(&payload(), &[])
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not available"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn learning_runtime_manager_does_not_claim_ready() {
        let manager = LearningRuntimeManager {
            paths: LearningPaths {
                root: PathBuf::from("/nonexistent"),
            },
        };
        // Nothing is provisioned by this manager in this build; like the
        // canonical manager in `learning::runtime`, it must not claim Ready.
        assert_eq!(manager.ensure_ready().await, RuntimeState::Unavailable);
    }
}
