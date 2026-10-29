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

const DEFAULT_REGISTRY: &str = r#"{"version":"0.4.14-bridge","models":[{"name":"laya-v1.11.1-onnx","url":"https://github.com/NandhaKishorM/laya/releases/download/v0.4.14/laya-v1.11.1-onnx.tar.gz","sha256":"695a4fbb2fc63288f2e6a29054304b9f0f157b08e4c5da8bada9bdb6072aa5e8","path":"model.onnx","size_bytes":2656291,"files":null}]}"#;

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
    if target_sha256.trim().is_empty() {
        return Err(anyhow!("target SHA-256 must not be empty"));
    }
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

    // Download bytes: support file:// for local, testable fetches.
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(path).context("Failed to read local model file")?
    } else {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .context("HTTP client build failed")?;
        let response = client.get(url).send().context("Download request failed")?;
        if !response.status().is_success() {
            return Err(anyhow!("HTTP {} for {}", response.status(), url));
        }
        response.bytes().context("Failed to read response body")?.to_vec()
    };

    // Compute SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual_sha = hex::encode(hasher.finalize());

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
///
/// Handles both single-file entries and `.tar.gz` archives. For archives, the
/// SHA-256 is verified over the archive bytes before extraction; extracted
/// members are written under `registry_dir`. `entry.files`, when present, is
/// used to verify that the expected artifacts exist after extraction.
pub fn fetch_from_registry(
    model_id: &str,
    registry_dir: &Path,
) -> Result<std::path::PathBuf> {
    let registry = ModelRegistry::load_or_create(registry_dir)?;
    let entry = registry
        .find(model_id)
        .ok_or_else(|| anyhow!("Model '{}' not found in registry", model_id))?
        .clone();

    std::fs::create_dir_all(registry_dir).context("Failed to create model directory")?;

    let out_path = registry_dir.join(entry.path.clone().unwrap_or_else(|| "model.onnx".into()));

    if entry.sha256.trim().is_empty() {
        return Err(anyhow!("registry entry SHA-256 must not be empty"));
    }

    if entry.url.ends_with(".tar.gz") || entry.url.ends_with(".tgz") {
        // Download the archive into memory first so its SHA-256 can be verified
        // before any bytes are written to disk.
        let archive_bytes = if let Some(path) = entry.url.strip_prefix("file://") {
            std::fs::read(path).context("Failed to read local archive")?
        } else {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .context("HTTP client build failed")?;
            let response = client.get(&entry.url).send().context("Download request failed")?;
            if !response.status().is_success() {
                return Err(anyhow!("HTTP {} for {}", response.status(), entry.url));
            }
            response.bytes().context("Failed to read response body")?.to_vec()
        };

        let mut hasher = Sha256::new();
        hasher.update(&archive_bytes);
        let actual_sha = hex::encode(hasher.finalize());
        if actual_sha != entry.sha256 {
            return Err(anyhow!(
                "SHA-256 mismatch! Expected {}, got {}",
                entry.sha256,
                actual_sha
            ));
        }

        extract_tar_gz(&archive_bytes, registry_dir)?;

        if let Some(files) = &entry.files {
            for file in files {
                if !registry_dir.join(file).exists() {
                    return Err(anyhow!("Archive missing expected file '{}'", file));
                }
            }
        }

        if !out_path.exists() {
            return Err(anyhow!("Archive did not contain '{}'", out_path.display()));
        }

        let new_registry = registry;
        let _ = new_registry.write(registry_dir);
        Ok(out_path)
    } else {
        let written = fetch_model(&entry.url, &entry.sha256, &out_path)?;
        if let Some(files) = &entry.files {
            for file in files {
                if !registry_dir.join(file).exists() {
                    return Err(anyhow!("Downloaded bundle missing expected file '{}'", file));
                }
            }
        }
        let new_registry = registry;
        let _ = new_registry.write(registry_dir);
        Ok(written)
    }
}

/// Extract a gzip-compressed tar archive into `registry_dir`, rejecting unsafe
/// paths and link entries.
pub fn extract_tar_gz(archive_bytes: &[u8], registry_dir: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(decoder);
    for member in archive.entries().context("Failed to read tar entries")? {
        let mut member = member.context("Failed to read tar member")?;
        let entry_type = member.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(anyhow!("Unsafe link entry in archive"));
        }
        let path = member.path().context("Failed to read member path")?.into_owned();
        if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow!("Unsafe path in archive: {}", path.display()));
        }
        let dest = registry_dir.join(&path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).context("Failed to create extraction directory")?;
        }
        member.unpack(&dest).context("Failed to extract archive member")?;
    }
    Ok(())
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

    #[test]
    fn fetch_model_verifies_sha256() {
        use sha2::{Digest, Sha256};
        let tmp = std::env::temp_dir().join("ns-fetch-sha");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("source.bin");
        let data = b"model-bytes";
        std::fs::write(&src, data).unwrap();
        let hash = hex::encode(Sha256::digest(data));
        let out = tmp.join("out.bin");
        let url = format!("file://{}", src.display());
        let written = fetch_model(&url, &hash, &out).unwrap();
        assert_eq!(written, out);
        assert_eq!(std::fs::read(&out).unwrap(), data);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fetch_model_rejects_sha_mismatch() {
        let tmp = std::env::temp_dir().join("ns-fetch-mismatch");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("source.bin");
        std::fs::write(&src, b"model-bytes").unwrap();
        let out = tmp.join("out.bin");
        let url = format!("file://{}", src.display());
        assert!(fetch_model(&url, "0000000000000000000000000000000000000000000000000000000000000000", &out).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fetch_model_rejects_empty_pin() {
        let tmp = std::env::temp_dir().join("ns-fetch-empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("source.bin");
        std::fs::write(&src, b"model-bytes").unwrap();
        let out = tmp.join("out.bin");
        let url = format!("file://{}", src.display());
        assert!(fetch_model(&url, "", &out).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fetch_from_registry_extracts_local_archive() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use sha2::{Digest, Sha256};
        use tar::Builder;

        let tmp = std::env::temp_dir().join("ns-registry-e2e");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Build a local tar.gz containing model.onnx.
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        let data = b"model-bytes";
        {
            let mut builder = Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "model.onnx", &data[..]).unwrap();
            builder.finish().unwrap();
        }
        let archive = encoder.finish().unwrap();
        let archive_path = tmp.join("bundle.tar.gz");
        std::fs::write(&archive_path, &archive).unwrap();
        let hash = hex::encode(Sha256::digest(&archive));

        // Write a registry pointing at the local archive.
        let registry_json = format!(
            r#"{{"version":"test","models":[{{"name":"laya-test","url":"file://{}","sha256":"{}","path":"model.onnx","files":["model.onnx"]}}]}}"#,
            archive_path.display(),
            hash
        );
        std::fs::write(tmp.join("registry.json"), registry_json).unwrap();

        let out = fetch_from_registry("laya-test", &tmp).unwrap();
        assert_eq!(out, tmp.join("model.onnx"));
        assert_eq!(std::fs::read(&out).unwrap(), data);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_tar_gz_writes_member() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        use tar::Builder;

        let tmp = std::env::temp_dir().join("ns-tar-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = Builder::new(&mut encoder);
            let data = b"hello";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "model.onnx", &data[..]).unwrap();
            builder.finish().unwrap();
        }
        let archive = encoder.finish().unwrap();

        extract_tar_gz(&archive, &tmp).unwrap();
        assert_eq!(std::fs::read(tmp.join("model.onnx")).unwrap(), b"hello");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_tar_gz_rejects_symlink() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::Builder;

        let tmp = std::env::temp_dir().join("ns-tar-symlink");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = Builder::new(&mut encoder);
            builder.append_link(
                &mut tar::Header::new_gnu(),
                "escape.link",
                "/etc/passwd",
            ).unwrap();
            builder.finish().unwrap();
        }
        let archive = encoder.finish().unwrap();

        assert!(extract_tar_gz(&archive, &tmp).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}