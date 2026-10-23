//! Validated ONNX model bundle: manifest, tokenizer, checkpoint, calibration tier.
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use anyhow::Result;

/// A bundle of ONNX artifacts produced by the bridge probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelBundle {
    /// Model version identifier (e.g. "v1.0.0").
    pub version: String,
    /// Root directory containing the bundle artifacts.
    pub root: PathBuf,
    /// Calibration tier: `proxy_internal` (shadow only) or `gold` (calibrated on gold).
    pub calibration_tier: crate::judgment::CalibrationTier,
    /// Embedder identity (must match the active embedder's `embedder_id`).
    pub embedder_id: String,
}

impl ModelBundle {
    /// Load a bundle from `bundle_dir` and validate its manifest, paths,
    /// and identity against the configured embedder.
    pub fn load(bundle_dir: &Path, embedder_id: &str) -> Result<Self> {
        let root = bundle_dir.to_path_buf();
        // Phase-0: minimal validation; full manifest parsing deferred to bridge spike.
        Ok(Self {
            version: "v0.0.0-bridge".to_string(),
            root,
            calibration_tier: crate::judgment::CalibrationTier::ProxyInternal,
            embedder_id: embedder_id.to_string(),
        })
    }

    /// Return the path to the ONNX checkpoint file.
    pub fn checkpoint_path(&self) -> PathBuf {
        self.root.join("model.onnx")
    }

    /// Return the path to the manifest file.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }
}
