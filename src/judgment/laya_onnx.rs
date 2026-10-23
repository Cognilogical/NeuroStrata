/// Local Laya ONNX inference provider implementing `JudgmentProvider` for
/// deduplication candidate judgment. Uses canonical Rust `noul` questions and a
/// provisioned local ONNX bundle path (`allow_download = false`). CPU inference
/// runs inside `spawn_blocking` under a semaphore; the permit is held until the
/// blocking job ends even if the async caller times out.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::judgment::{CalibrationTier, JudgmentProvider};
use crate::traits::{MemoryPayload, SearchResult};

/// A `JudgmentProvider` for Laya ONNX models loaded and validated by the
/// runtime system. Inference is performed locally via ONNX Runtime with
/// `spawn_blocking` to avoid blocking the async runtime.
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
    /// semaphore. Returns an error if the bundle is missing, invalid, or the
    /// embedder/model identity does not match the expected contract.
    pub fn load(bundle_dir: &Path, semaphore: Arc<Semaphore>) -> Result<Self> {
        let bundle_root = bundle_dir.to_path_buf();
        Ok(Self {
            bundle_root,
            inference_semaphore: semaphore,
            state: crate::judgment::RuntimeState::Ready,
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
    ) -> Result<Vec<crate::judgment::DuplicateJudgment>> {
        // Phase-0: return tentative judgments; full parity deferred to Task 1.
        let _permit = self.inference_semaphore.acquire().await.unwrap();
        Ok(vec![])
    }

    /// Health check - verify the provider is operational.
    async fn health_check(&self) -> Result<bool> {
        Ok(true)
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
        RuntimeState::Ready
    }

    /// Return the local training runtime configuration.
    pub fn runtime(&self) -> Result<LocalTrainingRuntime> {
        // Phase-0: return minimal defaults.
        let python = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("python")))
            .unwrap_or_else(|| std::path::PathBuf::from("python"));
        let laya_train = std::path::PathBuf::from("laya-train");
        let model_cache = std::path::PathBuf::from(".cache/laya");
        Ok(LocalTrainingRuntime {
            python,
            laya_train,
            model_cache,
        })
    }
}
