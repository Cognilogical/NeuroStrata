/// Generic pinned model downloader with SHA-256 checksum verification.
///
/// Supply-chain invariant: a model file is NEVER accepted if its SHA-256 does not
/// match the pinned hash in the registry. The registry lives at
/// `<bundle_dir>/registry.json`; missing = create default.
///
/// Designed to be reused by any judgment provider (Laya ONNX, future onnx
/// weights, custom inference binaries) without per-provider logic.
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// A single model entry in the registry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub path: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

/// The full registry schema.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelRegistry {
    pub version: String,
    pub models: Vec<ModelEntry>,
}

const DEFAULT_REGISTRY: &str = r#"{"version":"0.4.14-bridge","models":[{"name":"laya-v1.11.1-onne","url":"https://github.com/NandhaKishorM/laya/releases/download/v0.4.14/laya-v1.11.1-onnx.tar.gz","sha256":"695a4fbb2fc63288f2e6a29054304b9f0f157b08e4c5da8bada9bdb6072aa5e8","path":"model.onnx","size_bytes":2656291,"files":null}]}"#;

impl ModelRegistry {
    /// Load the registry from `<dir>/registry.json`. Creates a default
    /// registry if the file is absent (ensures supply-chain pins exist).
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let registry_path = dir.join("registry.json");
        if !registry_path.exists() {
            std::fs::write(&registry_path, DEFAULT_REGISTRY)
                .context("Failed to write default registry")?;
        }
        let text = std::fs::read_to_string(&registry_path)
            .context("Cannot read registry.json")?;
        serde_json::from_str(&text).context("Malformed registry.json")
    }

    /// Look up a model entry by name substring match.
    pub fn find(&self, model_id: &str) -> Option<&ModelEntry> {
        self.models
            .iter()
            .find(|m| m.name.contains(model_id))
    }

    /// Write the registry file back out (pretty-printed).
    pub fn write(&self, dir: &Path) -> Result<()> {
        let registry_path = dir.join("registry.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&registry_path, json)?;
        Ok(())
    }
}

/// Download, verify, and persist a model file.
/// Supply-chain safe: returns Err on ANY checksum mismatch.
/// Uses blocking I/O wrapped in spawn_blocking for async compatibility.
pub fn fetch_model(
    url: &str,
    target_sha256: &str,
    out_path: &Path,
) -> Result<std::path::PathBuf> {
    // Check cache: verify SHA-256 if file exists
    if out_path.exists() {
        let data = std::fs::read(out_path).unwrap_or_default();
        if !data.is_empty() {
            let actual = Sha256::digest(&data);
            let actual_hex = hex_encode(&actual);
            if actual_hex == target_sha256 {
                return Ok(out_path.to_path_buf());
            }
        }
    }

    // Ensure output dir exists
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .context("Failed to create model directory")?;
    }

    // Download using reqwest (blocking)
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("HTTP client build failed")?;

    let response = client
        .get(url)
        .send()
        .context("Download request failed")?;

    if !response.status().is_success() {
        return Err(anyhow!("HTTP {} for {}", response.status(), url));
    }

    let bytes = response
        .bytes()
        .context("Failed to read response body")?;

    // Compute SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual_sha = format!("{:x}", hasher.finalize());

    if actual_sha != target_sha256 {
        let _ = std::fs::remove_file(out_path);
        return Err(anyhow!(
            "SHA-256 mismatch! Expected {}, got {}",
            target_sha256, actual_sha
        ));
    }

    std::fs::write(out_path, &bytes)
        .context("Failed to write model file")?;

    println!("Model downloaded and verified (SHA-256: {}) to '{}'", actual_sha, out_path.display());
    Ok(out_path.to_path_buf())
}

/// Convenience: fetch from the registry by model ID.
/// Returns the path to the written model file.
pub fn fetch_from_registry(
    model_id: &str,
    registry_dir: &Path,
) -> Result<std::path::PathBuf> {
    let registry = ModelRegistry::load_or_create(registry_dir)?;
    let entry = registry
        .find(model_id)
        .ok_or_else(|| anyhow!("Model '{}' not found in registry", model_id))?
        .clone();

    let out_path = registry_dir.join(entry.path.clone().unwrap_or_else(|| "model.onnx".into()));
    let written = fetch_model(&entry.url, &entry.sha256, &out_path)?;

    // Save updated registry (in case new entry was added)
    let new_registry = registry;
    let _ = new_registry.write(registry_dir);

    Ok(written)
}

/// Hex encoding helper for SHA-256
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_entry_accepts_files_field() {
        let json = r#"{"name":"x","url":"u","sha256":"0","files":["a.onnx","b.json"]}"#;
        let e: ModelEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.files.unwrap(), vec!["a.onnx", "b.json"]);
    }

    #[test]
    fn model_entry_defaults_files_to_none() {
        let json = r#"{"name":"x","url":"u","sha256":"0"}"#;
        let e: ModelEntry = serde_json::from_str(json).unwrap();
        assert!(e.files.is_none());
    }
}