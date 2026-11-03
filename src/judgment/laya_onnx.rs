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

use ort::session::Session;
use tokenizers::Tokenizer;

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
    /// ONNX Runtime session for inference (local-first; disabled only on remote-model config).
    inference_session: Option<Arc<std::sync::Mutex<ort::session::Session>>>,
    /// Temperature for temperature-scaled confidence.
    temperature: f32,
    /// Model version recorded in the bundle manifest.
    model_version: String,
    /// Calibration tier recorded in the bundle manifest.
    calibration_tier: crate::judgment::CalibrationTier,
    /// Embedder identity recorded in the bundle manifest.
    embedder_id: String,
}

impl LayaOnnxProvider {
    /// Load an ONNX bundle from `bundle_dir`, attaching it to the inference
    /// semaphore. Validates the bundle layout (manifest, checkpoint paths,
    /// embedder identity) before attaching.
    pub fn load(bundle_dir: &Path, semaphore: Arc<Semaphore>) -> Result<Self> {
        let canonical = std::fs::canonicalize(bundle_dir)?;
        let manifest_path = canonical.join("manifest.json");
        if !manifest_path.exists() {
            return Err(anyhow!("Bundle missing manifest.json"));
        }
        let model_path = canonical.join("model.onnx");
        if !model_path.exists() {
            return Err(anyhow!("Bundle missing model.onnx"));
        }
        // Read manifest to validate embedder identity and extract version/temperature/tier
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow!(format!("Cannot read manifest: {e}")))?;
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).map_err(|e| anyhow!(format!("Bad manifest JSON: {e}")))?;

        let model_version = manifest.get("model_version")
            .or(manifest.get("version"))
            .and_then(|x| x.as_str())
            .unwrap_or("v0.1.0-local")
            .to_string();

        let temperature = manifest.get("temperature")
            .and_then(|x| x.as_f64())
            .map(|v| v as f32)
            .unwrap_or(1.0f32);
        let temperature = if temperature > 0.0 { temperature } else { 1.0f32 };

        let calibration_tier = if let Some(t) = manifest.get("calibration_tier").and_then(|x| x.as_str()) {
            match t {
                "GoldShipped" | "gold_shipped" | "DatasetCalibrated" | "dataset_calibrated" => crate::judgment::CalibrationTier::Gold,
                "Uncalibrated" | "uncalibrated" => crate::judgment::CalibrationTier::ProxyInternal,
                _ => crate::judgment::CalibrationTier::ProxyInternal,
            }
        } else {
            crate::judgment::CalibrationTier::ProxyInternal
        };

        // SHA-256 verification if present
        if let Some(expected_sha) = manifest.get("model_sha256").and_then(|x| x.as_str()) {
            use sha2::{Digest, Sha256};
            let bytes = std::fs::read(&model_path).map_err(|e| anyhow!(format!("Cannot read model: {e}")))?;
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let actual = hex::encode(hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected_sha) {
                return Err(anyhow!("Manifest SHA-256 mismatch for model.onnx"));
            }
        }

        let embedder_id = manifest["embedder_id"].as_str().ok_or_else(|| {
            anyhow!("Manifest missing embedder_id")
        })?.to_string();

        // Build session directly from file (local-first; graceful on bad model).
        let inference_session = {
            let builder = Session::builder();
            match builder {
                Err(e) => {
                    tracing::warn!("LayaOnnx failed to build ORT session: {e}");
                    None
                }
                Ok(mut b) => match b.commit_from_file(&model_path) {
                    Ok(s) => Some(Arc::new(std::sync::Mutex::new(s))),
                    Err(e) => {
                        tracing::warn!(
                            "LayaOnnx failed to commit model from {}: {e}",
                            model_path.display()
                        );
                        None
                    }
                },
            }
        };

        let state = match inference_session.as_ref() {
            Some(_) => crate::judgment::RuntimeState::Ready,
            None => crate::judgment::RuntimeState::Unavailable,
        };

        Ok(Self {
            bundle_root: canonical,
            inference_semaphore: semaphore,
            state,
            inference_session,
            temperature,
            model_version,
            calibration_tier,
            embedder_id,
        })
    }

    /// Model version recorded in the bundle manifest.
    pub fn model_version(&self) -> &str {
        &self.model_version
    }

    /// Temperature recorded in the bundle manifest (defaults 1.0).
    pub fn temperature(&self) -> f32 {
        self.temperature
    }

    /// Calibration tier recorded in the bundle manifest.
    pub fn calibration_tier(&self) -> crate::judgment::CalibrationTier {
        self.calibration_tier
    }

    /// Embedder identity recorded in the bundle manifest.
    pub fn embedder_id(&self) -> &str {
        &self.embedder_id
    }

    /// Whether this model may gate (block) a memory write, per the dedup
    /// policy: Gold tier + currently-active version + bare standalone add.
    pub fn may_block(&self, active_version: Option<&str>, is_bare_standalone_add: bool) -> bool {
        crate::judgment::policy::model_may_block(
            self.calibration_tier,
            &self.model_version,
            active_version,
            is_bare_standalone_add,
        )
    }

    /// Probe the provider's runtime state. Does not attempt inference.
    pub fn state(&self) -> crate::judgment::RuntimeState {
        self.state
    }

    /// Get a reference to the inference session for use in judge_duplicates.
    /// Returns None if no session loaded (e.g., remote-model config).
    fn inference_session(&self) -> Option<&Arc<std::sync::Mutex<ort::session::Session>>> {
        self.inference_session.as_ref()
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

        // Do not fabricate a manifest: the downloaded bundle must be complete.
        let manifest_path = target_path.join("manifest.json");
        if !manifest_path.exists() {
            return Err(anyhow!("Downloaded Laya bundle is missing manifest.json"));
        }

        // The bundle must contain the ONNX model itself: a download with a
        // manifest but no model.onnx is unusable to the inference session,
        // so fail loudly here instead of producing a half-usable bundle.
        if !target_path.join("model.onnx").exists() {
            return Err(anyhow!(
                "Downloaded Laya bundle at {} is missing model.onnx",
                target_path.display()
            ));
        }

        println!("Laya model '{}' fetched and verified to '{}'", model_id, target_path.display());
        Ok(())
    }
}

#[async_trait]
impl JudgmentProvider for LayaOnnxProvider {
    /// Judge whether a new memory is a duplicate of existing ones.
    /// Returns actual duplicate judgments using ONNX inference when available.
    async fn judge_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        // Fail-closed: without a valid inference session and Ready state,
        // do not return fabricated judgments.
        if self.state() != crate::judgment::RuntimeState::Ready
            || self.inference_session().is_none()
        {
            anyhow::bail!("Laya ONNX inference session unavailable (model not loaded or invalid)");
        }
        // Bound concurrent inference: owned semaphore permit.
        let permit = self
            .inference_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| anyhow::anyhow!("Laya inference semaphore closed: {e}"))?;

        // Gating comes from the dedup policy: a Gold-calibrated model may gate
        // a bare standalone add only when it is the currently-promoted
        // version. Shadow (non-promoted) verdicts record alongside without
        // gating the write, and the reason string says which mode produced it.
        let inline_extraction = new_memory
            .metadata
            .get("extracted_from")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        let active_version = crate::learning::control::read_active_version();
        let gating = self.may_block(active_version.as_deref(), !inline_extraction);
        let verdict_mode = if gating { "gating" } else { "shadow" };
        let model_meta = format!(
            "; model_version={}; embedder_id={}; calibration_tier={:?}; mode={}",
            self.model_version(),
            self.embedder_id(),
            self.calibration_tier(),
            verdict_mode,
        );

        let new_content = new_memory.content.clone();
        let candidates_vec: Vec<(String, f32, String)> = candidates
            .iter()
            .map(|c| (c.id.clone(), c.score, c.payload.content.clone()))
            .collect();
        let session_arc = self.inference_session.clone();
        let temperature = self.temperature;
        let bundle_root = self.bundle_root.clone();

        let judgments = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let tokenizer_path = bundle_root.join("tokenizer.json");
            let tokenizer = if tokenizer_path.exists() {
                match Tokenizer::from_file(&tokenizer_path) {
                    Ok(t) => Some(Arc::new(t)),
                    Err(_) => None,
                }
            } else {
                None
            };
            let new_tokens = tokenize_content(&new_content, tokenizer.as_deref());
            let mut judgments = Vec::new();
            for (id, score, payload_content) in candidates_vec {
                let cand_tokens = tokenize_content(&payload_content, tokenizer.as_deref());
                let intersection: usize = new_tokens.intersection(&cand_tokens).count();
                let union: usize = new_tokens.union(&cand_tokens).count();
                let token_overlap = if union == 0 { 0.0f32 } else { intersection as f32 / union as f32 };
                let fallback_logit = (token_overlap - 0.5) * 4.0;
                let (similarity, reason) = if let Some(ref session_ref) = session_arc {
                    let mut session = session_ref.lock().unwrap_or_else(|e| e.into_inner());
                    let jaccard_sim = token_overlap;
                    let feature_vec = [jaccard_sim as f32, (1.0 - score.clamp(0.0, 1.0)) as f32];
                    let feature_array = match ndarray::Array2::from_shape_vec((1, 2), feature_vec.to_vec())
                    {
                        Ok(arr) => match ort::value::Value::from_array(arr) {
                            Ok(v) => Some(v),
                            Err(_) => None,
                        },
                        Err(_) => None,
                    };
                    match feature_array {
                        Some(arr) => match session.run(ort::inputs! {"input" => arr}) {
                            Ok(outputs) => {
                                let model_logit = outputs.iter().next().and_then(|(_name, value)| {
                                    match value.try_extract_tensor::<f32>() {
                                        Ok((_, data)) => laya_logit_from_tensor(data),
                                        Err(_) => None,
                                    }
                                });
                                match model_logit {
                                    Some(logit) => (
                                        temperature_scaled_sigmoid(logit, temperature),
                                        "laya-onnx-model-logit",
                                    ),
                                    None => (
                                        temperature_scaled_sigmoid(fallback_logit, temperature),
                                        "laya-onnx-token-overlap-fallback",
                                    ),
                                }
                            }
                            Err(_) => (
                                temperature_scaled_sigmoid(fallback_logit, temperature),
                                "laya-onnx-token-overlap-fallback",
                            ),
                        },
                        None => (
                            temperature_scaled_sigmoid(fallback_logit, temperature),
                            "laya-onnx-token-overlap-fallback",
                        ),
                    }
                } else {
                    (
                        temperature_scaled_sigmoid(fallback_logit, temperature),
                        "laya-onnx-token-overlap-fallback",
                    )
                };
                let similarity = similarity.clamp(0.0, 1.0);
                judgments.push(DuplicateJudgment {
                    existing_memory_id: id.clone(),
                    confidence: similarity,
                    reason: Some(format!("{}{}", reason, model_meta)),
                });
            }
            judgments
        })
        .await
        .map_err(|e| anyhow::anyhow!("Laya inference task panicked: {e}"))?;

        Ok(judgments)
    }

    /// Health check - verify the provider is operational.
    /// Reports true when the ONNX inference session is loaded and state is Ready.
    async fn health_check(&self) -> Result<bool> {
        {
            let is_healthy = self.inference_session().is_some() &&
                           self.state() == crate::judgment::RuntimeState::Ready;
            // Model identity is part of the health record: a green health
            // check for the wrong bundle is an operator-visible failure.
            tracing::debug!(
                provider = self.name(),
                model_version = self.model_version(),
                embedder_id = self.embedder_id(),
                calibration_tier = ?self.calibration_tier(),
                temperature = self.temperature(),
                ready = is_healthy,
                "laya-onnx provider health"
            );
            Ok(is_healthy)
        }
    }

    /// Provider name for logging/metrics.
    fn name(&self) -> &str {
        "laya-onnx"
    }
}

/// Computes the dedup logit from ONNX output tensor values:
/// `[neg_logit, pos_logit]` => `pos - neg`; `[logit]` => logit;
/// empty or unsupported shapes => `None` (caller must fall back).
fn laya_logit_from_tensor(values: &[f32]) -> Option<f32> {
    match values {
        [neg, pos] => Some(pos - neg),
        [logit] => Some(*logit),
        _ => None,
    }
}

/// Temperature-scaled sigmoid confidence derived from a model logit.
fn temperature_scaled_sigmoid(logit: f32, temperature: f32) -> f32 {
    1.0 / (1.0 + (-logit / temperature).exp())
}

/// Tokenizes text content, using the tokenizer file when available,
/// otherwise falling back to subword/whitespace token IDs.
fn tokenize_content(content: &str, tokenizer: Option<&tokenizers::Tokenizer>) -> std::collections::HashSet<u32> {
    let mut ids = std::collections::HashSet::new();
    if let Some(tok) = tokenizer {
        // When tokenizer.json exists: use tokenizer.encode to produce IDs.
        // We only need deterministic token IDs, so encode and collect token IDs.
        match tok.encode(content, false) {
            Ok(encoded) => {
                for token_id in encoded.get_ids() {
                    ids.insert(*token_id as u32);
                }
                return ids;
            }
            Err(e) => {
                tracing::warn!("Laya tokenizer encode failed: {e}; falling back to whitespace hashing");
            }
        }
    }
    // FNV-1a whitespace hashing loop (used directly when no tokenizer,
    // or as fallback when tokenizer encode fails): split on whitespace and hash.
    for word in content.split_whitespace() {
        if !word.is_empty() {
            let mut hash_id: u32 = 2_166_136_261;
            for &b in word.as_bytes() {
                hash_id ^= b as u32;
                hash_id = hash_id.wrapping_mul(16_777_619);
            }
            ids.insert(hash_id);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laya_logit_extraction_computes_different_confidences_for_different_slices() {
        let conf = |slice: &[f32]| {
            laya_logit_from_tensor(slice)
                .map(|logit| temperature_scaled_sigmoid(logit, 1.0))
        };
        // [neg, pos] => pos - neg = 5.0 => high confidence
        let high = conf(&[-2.0, 3.0]);
        // [neg, pos] flipped => pos - neg = -5.0 => low confidence
        let low = conf(&[3.0, -2.0]);
        assert_eq!(high, Some(1.0 / (1.0 + (-5.0_f32).exp())));
        assert_eq!(low, Some(1.0 / (1.0 + 5.0_f32.exp())));
        assert_ne!(high, low, "different tensor slices must yield different confidences");
        // Single-element tensor: the logit is used directly.
        assert_eq!(conf(&[0.0]), Some(0.5));
        // Empty tensor or unsupported shape: extraction fails => caller falls back.
        assert_eq!(conf(&[]), None);
        assert_eq!(conf(&[1.0, 2.0, 3.0]), None);
    }

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
            {
                // With gliner feature, session fails on fake model, so error expected
                assert!(res.is_err() || res.unwrap().is_empty());
            }
            // candidates empty -> Ok(empty)
            let res2 = p.judge_duplicates(&mem, &[]).await;
            assert!(res2.unwrap().is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn laya_health_check_reflects_session() {
        {
            let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
            let dir = std::env::temp_dir().join("ns-laya-health-check");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let manifest = serde_json::json!({"embedder_id": "laya", "model_version": "v1"});
            std::fs::write(dir.join("manifest.json"), serde_json::to_string(&manifest).unwrap()).unwrap();
            // Place model.onnx file
            std::fs::write(dir.join("model.onnx"), b"fake").unwrap();
            // With fake model, session creation will fail -> health_check false
            let p = LayaOnnxProvider::load(&dir, sem).expect("bundle with manifest and model loads");
            assert!(!p.health_check().await.unwrap());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn laya_provider_state_unavailable_with_fake_model() {
        {
            let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
            let dir = std::env::temp_dir().join("ns-laya-ready");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let manifest = serde_json::json!({"embedder_id": "laya", "model_version": "v1"});
            std::fs::write(dir.join("manifest.json"), serde_json::to_string(&manifest).unwrap()).unwrap();
            std::fs::write(dir.join("model.onnx"), b"fake").unwrap();
            let p = LayaOnnxProvider::load(&dir, sem).expect("bundle with manifest and model loads");
            assert_eq!(p.state(), RuntimeState::Unavailable); // fake model fails session creation
            // Manifest fields must be stored, not discarded.
            assert_eq!(p.model_version(), "v1");
            assert_eq!(p.embedder_id(), "laya");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn laya_may_block_follows_policy() {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let dir = std::env::temp_dir().join("ns-laya-may-block");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "embedder_id": "laya",
            "model_version": "v1",
            "calibration_tier": "GoldShipped"
        });
        std::fs::write(dir.join("manifest.json"), serde_json::to_string(&manifest).unwrap()).unwrap();
        std::fs::write(dir.join("model.onnx"), b"fake").unwrap();
        let p = LayaOnnxProvider::load(&dir, sem).expect("bundle loads");
        assert_eq!(p.calibration_tier(), crate::judgment::CalibrationTier::Gold);
        // Gold + active + bare standalone add -> may block.
        assert!(p.may_block(Some("v1"), true));
        // Not the active version (shadow) -> may not block.
        assert!(!p.may_block(Some("v0"), true));
        // Inline task extraction -> never model-blocks.
        assert!(!p.may_block(Some("v1"), false));
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
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root_dir(&self) -> PathBuf {
        self.root.clone()
    }
    /// Dataset partition written by `build_dedup_dataset` and consumed by
    /// the builtin training path (`<root>/dataset/train.jsonl`).
    pub fn train_jsonl(&self) -> PathBuf {
        self.root.join("dataset").join("train.jsonl")
    }

    /// Held-out evaluation pairs written by `build_dedup_dataset`
    /// (`<root>/dataset/eval.jsonl`).
    pub fn proxy_eval_jsonl(&self) -> PathBuf {
        self.root.join("dataset").join("eval.jsonl")
    }

    pub fn base_model_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    /// The model cache directory used by `LearningRuntimeManager::runtime`
    /// and the builtin training path (`<root>/.cache/laya`).
    pub fn model_cache_dir(&self) -> PathBuf {
        self.root.join(".cache").join("laya")
    }

    /// The directory the builtin training path actually writes candidate
    /// artifacts into (`<root>/.cache/laya/candidates/<version>`).
    pub fn candidate_dir(&self, version: &ModelVersion) -> PathBuf {
        self.model_cache_dir().join("candidates").join(version)
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

/// Local training runtime readiness with an explicit degraded reason.
/// Unlike the blind "directory exists ⇒ Ready" probe, this reflects whether a
/// real model artifact or trainer binary is present on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayaReadiness {
    Ready,
    Degraded { reason: String },
}

/// Manages the local Laya training/runtime environment: provisioning a pinned
/// Python runtime, `laya-train`, and the selected base checkpoint; starting
/// and querying the runtime in the background without blocking daemon startup.
pub struct LearningRuntimeManager {
    paths: LearningPaths,
}

impl LearningRuntimeManager {
    pub fn new(paths: LearningPaths) -> Self {
        Self { paths }
    }

    /// Ensure the runtime is ready. Fail-closed: returns `Ready` only when a
    /// real artifact exists — `model.onnx` inside the model cache, or a
    /// present `laya-train` binary. An empty cache directory is `Degraded`.
    pub async fn ensure_ready(&self) -> LayaReadiness {
        let runtime = match self.runtime() {
            Ok(r) => r,
            Err(e) => {
                return LayaReadiness::Degraded {
                    reason: format!("runtime unavailable: {}", e),
                }
            }
        };
        let model_artifact = runtime.model_cache.join("model.onnx");
        if model_artifact.exists() {
            LayaReadiness::Ready
        } else if runtime.laya_train.exists() {
            LayaReadiness::Ready
        } else {
            LayaReadiness::Degraded {
                reason: format!(
                    "no model artifact at {} and no laya-train binary at {}",
                    model_artifact.display(),
                    runtime.laya_train.display()
                ),
            }
        }
    }

    /// Return the local training runtime configuration.
    pub fn runtime(&self) -> Result<LocalTrainingRuntime> {
        let python = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("python")))
            .unwrap_or_else(|| PathBuf::from("python"));
        let root = self.paths.root_dir();
        Ok(LocalTrainingRuntime {
            python,
            laya_train: root.join("laya-train"),
            model_cache: self.paths.model_cache_dir(),
        })
    }
}

#[cfg(test)]
mod ensure_ready_tests {
    use super::*;
    use LayaReadiness::{Degraded, Ready};

    #[tokio::test]
    async fn learning_runtime_manager_ensure_ready_degrades_on_empty_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = LearningRuntimeManager::new(LearningPaths::new(tmp.path()));
        let state = mgr.ensure_ready().await;
        assert!(
            matches!(state, Degraded { .. }),
            "empty cache dir must be Degraded, got {:?}",
            state
        );
        if let Degraded { reason } = state {
            assert!(!reason.is_empty(), "degraded must carry a reason");
        }
    }

    #[tokio::test]
    async fn learning_runtime_manager_ensure_ready_ready_when_model_onnx_present() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join(".cache").join("laya");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("model.onnx"), b"stub model artifact").unwrap();
        let mgr = LearningRuntimeManager::new(LearningPaths::new(tmp.path()));
        let state = mgr.ensure_ready().await;
        assert_eq!(
            state,
            Ready,
            "model.onnx present must report Ready, got {:?}",
            state
        );
    }

    #[tokio::test]
    async fn learning_runtime_manager_ensure_ready_ready_when_trainer_binary_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("laya-train"), b"#!/bin/sh\n").unwrap();
        let mgr = LearningRuntimeManager::new(LearningPaths::new(tmp.path()));
        let state = mgr.ensure_ready().await;
        assert_eq!(
            state,
            Ready,
            "laya-train binary present must report Ready, got {:?}",
            state
        );
    }
}
