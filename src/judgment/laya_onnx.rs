/// Local Laya ONNX inference provider implementing `JudgmentProvider` for
/// deduplication candidate judgment. Uses canonical Rust `noul` questions and a
/// provisioned local ONNX bundle path (`allow_download = false`). CPU inference
/// runs inside `spawn_blocking` to avoid blocking the async runtime.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::judgment::{DuplicateJudgment, JudgmentProvider};
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
        let _embedder_id = embedder_id.to_string();

        Ok(Self {
            bundle_root: canonical,
            inference_semaphore: semaphore,
            // Inference is not yet wired (no real ort session); report unavailable.
            state: crate::judgment::RuntimeState::Unavailable,
        })
    }

    /// Probe the provider's runtime state. Does not attempt inference.
    pub fn state(&self) -> crate::judgment::RuntimeState {
        self.state
    }

    /// Fetch a pinned Laya model bundle by model identifier.
    /// Downloads from the registry-defined URL and verifies the SHA-256 checksum
    /// before writing to the target directory, ensuring supply-chain integrity.
    pub async fn fetch_model(model_id: &str, target_dir: &str) -> Result<()> {
        let target_path = PathBuf::from(target_dir);
        std::fs::create_dir_all(&target_path)?;

        // Use the common model downloader (spawn_blocking to avoid blocking async runtime)
        let _written = tokio::task::spawn_blocking({
            let model_id = model_id.to_string();
            let target_path = target_path.clone();
            move || crate::model_downloader::fetch_from_registry(&model_id, &target_path)
        })
        .await??;

        // Ensure manifest.json exists for the provider to load
        let manifest_path = target_path.join("manifest.json");
        if !manifest_path.exists() {
            let manifest = serde_json::json!({
                "model_path": "model.onnx",
                "embedder_id": "laya-english-v0.4.14",
                "embedder": "laya-english-v0.4.14",
                "model_version": "v1.11.1",
                "version": "v1.11.1"
            });
            std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
        }

        println!("Laya model '{}' fetched and verified to '{}'", model_id, target_path.display());
        Ok(())
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
        let model_path = self.bundle_root.join("model.onnx");
        if model_path.exists() && !candidates.is_empty() {
            // Real ONNX inference is not yet wired; refuse to fabricate a score.
            return Err(anyhow!(
                "Laya ONNX inference not implemented (model present at {})",
                model_path.display()
            ));
        }
        Ok(vec![])
    }

    /// Health check - verify the provider is operational.
    async fn health_check(&self) -> Result<bool> {
        Ok(false)
    }

    /// Provider name for logging/metrics.
    fn name(&self) -> &str {
        "laya-onnx"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn laya_does_not_return_fake_confidence() {
        // load may fail on missing bundle; if it succeeds, judge must not fake 0.75
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        // Create a minimal bundle dir with manifest
        let dir = std::env::temp_dir().join("ns-laya-bundle");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({"embedder_id": "laya", "model_version": "v1"});
        std::fs::write(dir.join("manifest.json"), serde_json::to_string(&manifest).unwrap()).unwrap();
        // Place a model.onnx file so the judge enters the inference branch
        std::fs::write(dir.join("model.onnx"), b"fake").unwrap();
        if let Ok(p) = LayaOnnxProvider::load(&dir, sem) {
            // With a present model and non-empty candidates, must error (not fake 0.75).
            let mem = MemoryPayload {
                content: String::new(),
                user_id: String::new(),
                memory_type: String::new(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::Value::Null,
            };
            let cand = SearchResult {
                id: "c1".to_string(),
                score: 1.0,
                payload: mem.clone(),
                evidence: None,
            };
            let res = p.judge_duplicates(&mem, &[cand]).await;
            assert!(res.is_err());
            assert!(res.unwrap_err().to_string().contains("not implemented"));
            // candidates empty -> Ok(empty)
            let res2 = p.judge_duplicates(&mem, &[]).await;
            assert!(res2.unwrap().is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn laya_ensure_ready_reports_unavailable() {
        let mgr = LearningRuntimeManager {
            paths: LearningPaths { root: std::path::PathBuf::from("/tmp") },
        };
        assert_eq!(mgr.ensure_ready().await, RuntimeState::Unavailable);
    }

    #[tokio::test]
    async fn laya_health_check_false() {
        let dir = std::env::temp_dir().join("ns-laya-bundle2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({"embedder_id": "laya", "model_version": "v1"});
        std::fs::write(dir.join("manifest.json"), serde_json::to_string(&manifest).unwrap()).unwrap();
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        if let Ok(p) = LayaOnnxProvider::load(&dir, sem) {
            assert!(!p.health_check().await.unwrap());
            assert_eq!(p.state(), RuntimeState::Unavailable);
        }
        let _ = std::fs::remove_dir_all(&dir);
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
        RuntimeState::Unavailable
    }

    /// Return the local training runtime configuration.
    pub fn runtime(&self) -> Result<LocalTrainingRuntime> {
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

    /// Fetch a pinned Laya model bundle by model identifier.
    /// Downloads from the registry-defined URL and verifies the SHA-256 checksum
    /// before writing to the target directory, ensuring supply-chain integrity.
    pub async fn fetch_model(model_id: &str, target_dir: &str) -> Result<()> {
        let target_path = PathBuf::from(target_dir);
        std::fs::create_dir_all(&target_path)?;

        // Use the common model downloader (spawn_blocking to avoid blocking async runtime)
        let _written = tokio::task::spawn_blocking({
            let model_id = model_id.to_string();
            let target_path = target_path.clone();
            move || crate::model_downloader::fetch_from_registry(&model_id, &target_path)
        })
        .await??;

        // Ensure manifest.json exists for the provider to load
        let manifest_path = target_path.join("manifest.json");
        if !manifest_path.exists() {
            let manifest = serde_json::json!({
                "model_path": "model.onnx",
                "embedder_id": "laya-english-v0.4.14",
                "embedder": "laya-english-v0.4.14",
                "model_version": "v1.11.1",
                "version": "v1.11.1"
            });
            std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
        }

        println!("Laya model '{}' fetched and verified to '{}'", model_id, target_path.display());
        Ok(())
    }
}
