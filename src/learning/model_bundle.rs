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
        let manifest_path = root.join("manifest.json");
        let mut version = "v0.0.0-bridge".to_string();
        let mut calibration_tier = crate::judgment::CalibrationTier::ProxyInternal;
        let mut manifest_embedder: Option<String> = None;

        if manifest_path.exists() {
            let text = std::fs::read_to_string(&manifest_path)
                .map_err(|e| anyhow::anyhow!("Failed to read manifest: {}", e))?;
            let manifest: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("Bad manifest JSON: {}", e))?;

            // version
            if let Some(v) = manifest.get("model_version").and_then(|x| x.as_str()) {
                version = v.to_string();
            } else if let Some(v) = manifest.get("version").and_then(|x| x.as_str()) {
                version = v.to_string();
            }

            // embedder_id check
            if let Some(e) = manifest.get("embedder_id").and_then(|x| x.as_str()) {
                if !e.is_empty() {
                    manifest_embedder = Some(e.to_string());
                }
            }

            // calibration_tier
            if let Some(t) = manifest.get("calibration_tier").and_then(|x| x.as_str()) {
                calibration_tier = match t {
                    "GoldShipped" | "gold_shipped" => crate::judgment::CalibrationTier::Gold,
                    "Uncalibrated" | "uncalibrated" => crate::judgment::CalibrationTier::ProxyInternal,
                    _ => crate::judgment::CalibrationTier::ProxyInternal,
                };
            }

            // SHA-256 verification
            if let Some(expected_sha) = manifest.get("model_sha256").and_then(|x| x.as_str()) {
                if root.join("model.onnx").exists() {
                    let verified = crate::gliner::verify_bundle_sha256(&root, expected_sha)?;
                    if !verified {
                        return Err(anyhow::anyhow!("Manifest SHA-256 mismatch for model.onnx"));
                    }
                }
            }
        }

        // Embedder mismatch check
        if let Some(manifest_emb) = manifest_embedder {
            if !embedder_id.is_empty() && manifest_emb != embedder_id {
                return Err(anyhow::anyhow!(
                    "Manifest embedder_id '{}' does not match expected '{}'",
                    manifest_emb, embedder_id
                ));
            }
        }

        Ok(Self {
            version,
            root,
            calibration_tier,
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
    use std::fs;

    fn temp_bundle(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ns-model-bundle-{}", name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn load_parses_manifest_version_and_tier() {
        let d = temp_bundle("version-tier");
        fs::write(d.join("manifest.json"), r#"{"model_version":"v2.1.0","calibration_tier":"GoldShipped","embedder_id":"e1"}"#).unwrap();
        let b = ModelBundle::load(&d, "e1").unwrap();
        assert_eq!(b.version, "v2.1.0");
        assert!(matches!(b.calibration_tier, crate::judgment::CalibrationTier::Gold));
        assert_eq!(b.embedder_id, "e1");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn load_rejects_embedder_id_mismatch() {
        let d = temp_bundle("mismatch");
        fs::write(d.join("manifest.json"), r#"{"model_version":"v1","embedder_id":"other"}"#).unwrap();
        let res = ModelBundle::load(&d, "expected");
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("embedder_id"));
        assert!(msg.contains("other"));
        assert!(msg.contains("expected"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn load_verifies_model_sha256() {
        let d = temp_bundle("sha");
        let model_content = b"fake onnx content";
        fs::write(d.join("model.onnx"), model_content).unwrap();
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(model_content);
        let expected = hex::encode(hasher.finalize());
        fs::write(d.join("manifest.json"), format!(r#"{{"model_version":"v1","model_sha256":"{}","embedder_id":"e"}}"#, expected)).unwrap();
        let b = ModelBundle::load(&d, "e").unwrap();
        assert_eq!(b.version, "v1");
        // mismatch case
        let d2 = temp_bundle("sha-bad");
        fs::write(d2.join("model.onnx"), model_content).unwrap();
        fs::write(d2.join("manifest.json"), r#"{"model_version":"v1","model_sha256":"0000000000000000000000000000000000000000000000000000000000000000","embedder_id":"e"}"#).unwrap();
        assert!(ModelBundle::load(&d2, "e").is_err());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }
}
