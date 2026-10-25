//! Validated ONNX model bundle: manifest, tokenizer, checkpoint, calibration tier.
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use anyhow::{anyhow, Result};

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
    /// Load a bundle from `bundle_dir`, reading its `manifest.json`.
    ///
    /// The bundle's version is the one its manifest declares
    /// (`bundle_version`, falling back to `model_version`) — never a string
    /// minted by this loader. A bundle whose manifest is missing,
    /// unparsable, or declares no version is refused.
    pub fn load(bundle_dir: &Path, embedder_id: &str) -> Result<Self> {
        let root = bundle_dir.to_path_buf();
        let manifest_path = root.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
            anyhow!("Cannot read bundle manifest {}: {e}", manifest_path.display())
        })?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
            .map_err(|e| anyhow!("Bad bundle manifest JSON: {e}"))?;
        let version = manifest["bundle_version"]
            .as_str()
            .or_else(|| manifest["model_version"].as_str())
            .ok_or_else(|| {
                anyhow!("Bundle manifest declares no bundle_version or model_version")
            })?
            .to_string();
        Ok(Self {
            version,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_bundle(label: &str, manifest: Option<&str>) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ns-bundle-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(manifest) = manifest {
            std::fs::write(dir.join("manifest.json"), manifest).unwrap();
        }
        dir
    }

    #[test]
    fn bundle_version_is_not_fabricated() {
        let dir = temp_bundle(
            "version",
            Some(
                r#"{"bundle_version":"0.4.14-bridge","model_version":"v1.11.1","embedder_id":"test-embedder"}"#,
            ),
        );
        let bundle = ModelBundle::load(&dir, "test-embedder").unwrap();
        // The version must come from the bundle's own manifest, never from a
        // hardcoded string minted by the loader.
        assert_eq!(bundle.version, "0.4.14-bridge");
    }

    #[test]
    fn bundle_version_falls_back_to_model_version() {
        let dir = temp_bundle(
            "modelversion",
            Some(r#"{"model_version":"v9.9.9","embedder_id":"test-embedder"}"#),
        );
        let bundle = ModelBundle::load(&dir, "test-embedder").unwrap();
        assert_eq!(bundle.version, "v9.9.9");
    }

    #[test]
    fn bundle_load_fails_when_manifest_declares_no_version() {
        let dir = temp_bundle("noversion", Some(r#"{"embedder_id":"test-embedder"}"#));
        assert!(ModelBundle::load(&dir, "test-embedder").is_err());
    }

    #[test]
    fn bundle_load_fails_without_a_manifest() {
        let dir = temp_bundle("nomanifest", None);
        assert!(ModelBundle::load(&dir, "test-embedder").is_err());
    }
}
