//! Validated ONNX model bundle: manifest, checkpoint, calibration tier.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
            anyhow::anyhow!(
                "Cannot read bundle manifest {}: {e}",
                manifest_path.display()
            )
        })?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
            .map_err(|e| anyhow::anyhow!("Bad bundle manifest JSON: {e}"))?;
        let version = manifest["bundle_version"]
            .as_str()
            .or_else(|| manifest["model_version"].as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Bundle manifest declares no bundle_version or model_version")
            })?
            .to_string();
        let mut calibration_tier = crate::judgment::CalibrationTier::ProxyInternal;

        // calibration_tier
        if let Some(t) = manifest.get("calibration_tier").and_then(|x| x.as_str()) {
            calibration_tier = match t {
                "gold" | "Gold" | "GoldShipped" => crate::judgment::CalibrationTier::Gold,
                "proxy_internal" | "ProxyInternal" | "DatasetCalibrated" | "Uncalibrated" => {
                    crate::judgment::CalibrationTier::ProxyInternal
                }
                other => {
                    tracing::warn!(
                        "unknown calibration_tier '{other}' in {}, defaulting to ProxyInternal",
                        manifest_path.display()
                    );
                    crate::judgment::CalibrationTier::ProxyInternal
                }
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

        // Embedder mismatch check
        if let Some(e) = manifest.get("embedder_id").and_then(|x| x.as_str()) {
            if !e.is_empty() && !embedder_id.is_empty() && e != embedder_id {
                return Err(anyhow::anyhow!(
                    "Manifest embedder_id '{}' does not match expected '{}'",
                    e,
                    embedder_id
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

    fn temp_bundle_with_manifest(label: &str, manifest: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ns-bundle-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(manifest) = manifest {
            std::fs::write(dir.join("manifest.json"), manifest).unwrap();
        }
        dir
    }

    #[test]
    fn bundle_version_is_not_fabricated() {
        let dir = temp_bundle_with_manifest(
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
        let dir = temp_bundle_with_manifest(
            "modelversion",
            Some(r#"{"model_version":"v9.9.9","embedder_id":"test-embedder"}"#),
        );
        let bundle = ModelBundle::load(&dir, "test-embedder").unwrap();
        assert_eq!(bundle.version, "v9.9.9");
    }

    #[test]
    fn bundle_load_fails_when_manifest_declares_no_version() {
        let dir =
            temp_bundle_with_manifest("noversion", Some(r#"{"embedder_id":"test-embedder"}"#));
        assert!(ModelBundle::load(&dir, "test-embedder").is_err());
    }

    #[test]
    fn bundle_load_fails_without_a_manifest() {
        let dir = temp_bundle_with_manifest("nomanifest", None);
        assert!(ModelBundle::load(&dir, "test-embedder").is_err());
    }

    fn temp_bundle(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ns-model-bundle-{}", name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn load_parses_manifest_version_and_tier() {
        let d = temp_bundle("version-tier");
        fs::write(
            d.join("manifest.json"),
            r#"{"model_version":"v2.1.0","calibration_tier":"gold","embedder_id":"e1"}"#,
        )
        .unwrap();
        let b = ModelBundle::load(&d, "e1").unwrap();
        assert_eq!(b.version, "v2.1.0");
        assert!(matches!(
            b.calibration_tier,
            crate::judgment::CalibrationTier::Gold
        ));
        assert_eq!(b.embedder_id, "e1");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn load_rejects_embedder_id_mismatch() {
        let d = temp_bundle("mismatch");
        fs::write(
            d.join("manifest.json"),
            r#"{"model_version":"v1","embedder_id":"other"}"#,
        )
        .unwrap();
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
        fs::write(
            d.join("manifest.json"),
            format!(
                r#"{{"model_version":"v1","model_sha256":"{}","embedder_id":"e"}}"#,
                expected
            ),
        )
        .unwrap();
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
