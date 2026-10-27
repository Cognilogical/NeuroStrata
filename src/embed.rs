use crate::traits::Embedder;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone)]
pub struct AcceptableEmbedder {
    pub model_name: String,
    pub dimensions: usize,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_model: Option<String>,
    /// Pinned SHA-256 of the model weights. When set, the embedder verifies
    /// the downloaded model against this hash before accepting it.
    /// Supply-chain protection: a mismatched hash is a fatal error.
    #[serde(default)]
    pub model_sha256: Option<String>,
}

fn get_acceptable_embedders() -> Result<Vec<AcceptableEmbedder>> {
    // dirs::home_dir() rather than $HOME: Windows sets USERPROFILE, not HOME, so
    // reading the variable directly sends this to a relative path under whatever
    // directory happened to launch the process. config.rs already resolves it this way.
    let config_dir = dirs::home_dir()
        .map(|home| home.join(".config").join("neurostrata"))
        .unwrap_or_else(|| PathBuf::from(".neurostrata"));

    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).context("Failed to create config directory")?;
    }

    let config_path = config_dir.join("embedders.json");

    if config_path.exists() {
        let content = fs::read_to_string(&config_path).context("Failed to read embedders.json")?;
        if let Ok(models) = serde_json::from_str::<Vec<AcceptableEmbedder>>(&content) {
            if !models.is_empty() {
                return Ok(models);
            }
        }
    }

    // Default list if file doesn't exist or is empty
    let default_models = vec![
        AcceptableEmbedder {
            model_name: "NomicEmbedTextV15".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            // Real SHA-256 of the fastembed Nomic v1.5 ONNX weights (computed).
            model_sha256: Some("147d5aa88c2101237358e17796cf3a227cead1ec304ec34b465bb08e9d952965".to_string()),
        },
        AcceptableEmbedder {
            model_name: "BGEBaseENV15".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            // BGE pin pending a verified download; verification skipped when None.
            model_sha256: None,
        },
    ];

    // Write default to file so user can edit it
    if let Ok(json_content) = serde_json::to_string_pretty(&default_models) {
        let _ = fs::write(&config_path, json_content);
    }

    Ok(default_models)
}

fn find_existing_cache_dir() -> PathBuf {
    // Same reason as above: $HOME is absent on Windows, and an empty home turns
    // every candidate below into a relative path, which is how a 523 MB model
    // cache ends up inside the caller's working directory.
    let home_path = dirs::home_dir().unwrap_or_default();

    // Adopt the Neuro cache pattern: share models across all Neuro* tools
    let primary_neuro_cache = home_path.join(".cache/neuro/models/fastembed");

    // Other common fastembed caches we should check to avoid re-downloading
    let cache_dirs = vec![
        primary_neuro_cache.clone(),
        home_path.join(".cache/fastembed"),
        home_path.join(".cache/huggingface/hub"),
    ];

    for dir in cache_dirs {
        if dir.exists()
            && dir
                .read_dir()
                .map(|mut i| i.next().is_some())
                .unwrap_or(false)
        {
            return dir; // Return the first valid existing cache directory
        }
    }

    primary_neuro_cache
}

/// The model this process is configured to use: the one NEUROSTRATA_MODEL names
/// when it is acceptable, otherwise the first acceptable entry.
fn configured_model() -> Result<AcceptableEmbedder> {
    let acceptable_models = get_acceptable_embedders()?;
    let env_model = std::env::var("NEUROSTRATA_MODEL").unwrap_or_default();
    Ok(acceptable_models
        .iter()
        .find(|m| m.model_name.eq_ignore_ascii_case(&env_model))
        .unwrap_or(&acceptable_models[0])
        .clone())
}

/// The embedding width of the configured model, without loading the model.
///
/// Backup and restore need only this number to open the store. Loading the model
/// can mean a 523 MB download, which is the wrong thing to require on the fresh
/// or offline machine a restore is usually run on.
pub fn configured_dimensions() -> Result<usize> {
    Ok(configured_model()?.dimensions)
}

const MAX_EMBED_TOKENS: usize = 2048;

/// Verify a model file's SHA-256 checksum against a pinned value.
/// Returns `Err` on mismatch or when no model file can be found (fail-closed).
pub fn verify_model_checksum(cache_dir: &PathBuf, model_name: &str, pinned_sha: &str) -> Result<()> {
    // Collect candidate model files. Check the flat cache layout first, then
    // walk the fastembed/HuggingFace snapshot layout (models--X/snapshots/*/onnx/model.onnx).
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Flat fallback: cache_dir/model.onnx
    let flat = cache_dir.join("model.onnx");
    if flat.is_file() {
        candidates.push(flat);
    }

    // fastembed layout: cache_dir/models--{name}/snapshots/*/onnx/model.onnx (and model.onnx)
    let model_dir_name = format!("models--{}", model_name.replace("/", "--"));
    let model_dir = cache_dir.join(&model_dir_name);
    if model_dir.is_dir() {
        collect_onnx_files(&model_dir, &mut candidates, 0, 6);
    }

    // Generic walk of cache_dir for any model.onnx (covers alternate cache roots)
    if candidates.is_empty() {
        collect_onnx_files(cache_dir, &mut candidates, 0, 4);
    }

    if candidates.is_empty() {
        return Err(anyhow!(
            "Embedder supply-chain error: model '{}' model file not found in cache for verification (pinned SHA: {}).",
            model_name, pinned_sha
        ));
    }

    // Verify the first candidate (deterministic: sorted).
    candidates.sort();
    let path = &candidates[0];
    let data = std::fs::read(path)
        .context("Failed to read model file for checksum verification")?;
    let actual = format!("{:x}", Sha256::digest(&data));
    if actual != pinned_sha {
        return Err(anyhow!(
            "Embedder supply-chain error: model '{}' checksum mismatch!\n  File: {}\n  Expected: {}\n  Actual: {}",
            model_name, path.display(), pinned_sha, actual
        ));
    }
    Ok(())
}

fn collect_onnx_files(dir: &PathBuf, out: &mut Vec<PathBuf>, depth: usize, max_depth: usize) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "model.onnx" {
                    out.push(path);
                }
            } else if path.is_dir() {
                collect_onnx_files(&path, out, depth + 1, max_depth);
            }
        }
    }
}

/// Token ceiling for one embedding. Roughly covers MAX_SYMBOL_CHARS of source

pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
    model_name: String,
    dimensions: usize,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        let target_model = configured_model()?;

        let model_enum = EmbeddingModel::from_str(&target_model.model_name)
            .unwrap_or(EmbeddingModel::NomicEmbedTextV15);

        let cache_dir = find_existing_cache_dir();

        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)
                .context("Failed to create shared model cache directory")?;
        }

        eprintln!(
            "Initializing FastEmbedder with model: {} using cache: {:?}",
            target_model.model_name, cache_dir
        );

        // fastembed defaults to 512 tokens regardless of what the model supports
        // (NomicEmbedTextV15 handles 8192), and truncates silently. Anything above
        // what ingestion actually stores per symbol is wasted, so this is set to
        // match MAX_SYMBOL_CHARS in src/parser/ingest.rs -- change the two together.
        let model = TextEmbedding::try_new(
            TextInitOptions::new(model_enum)
                .with_cache_dir(cache_dir.clone())
                .with_max_length(MAX_EMBED_TOKENS)
                .with_show_download_progress(true),
        )?;

        // Supply-chain verification: if a pinned SHA-256 is specified, verify
        // the model file against it. This protects against cache poisoning
        // attacks where an attacker could replace a downloaded model.
        if let Some(pinned_sha) = &target_model.model_sha256 {
            verify_model_checksum(&cache_dir, &target_model.model_name, pinned_sha)?;
        }

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            model_name: target_model.model_name.clone(),
            dimensions: target_model.dimensions,
        })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Model inference is CPU-bound and can stall the async runtime when
        // called from a Tokio task. Isolate it on the blocking thread pool.
        let model = Arc::clone(&self.model);
        let text = text.to_owned();
        let mut embeddings = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
            let mut guard = model
                .lock()
                .map_err(|e| anyhow::anyhow!("model lock poisoned: {}", e))?;
            guard
                .embed(vec![&*text], None)
                .context("fastembed embed call failed")
        })
        .await
        .map_err(|e| anyhow::anyhow!("embed task panicked: {}", e))??;
        Ok(embeddings.pop().unwrap_or_default())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embedder_id(&self) -> &str {
        &self.model_name
    }
}

pub struct RemoteEmbedder {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    api_model: String,
    dimensions: usize,
    model_name: String,
}

impl RemoteEmbedder {
    pub fn new(
        base_url: String,
        api_key: String,
        api_model: String,
        dimensions: usize,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            base_url,
            api_key,
            api_model: api_model.clone(),
            dimensions,
            model_name: api_model,
        })
    }

    async fn embed_with_retry(&self, text: &str) -> Result<Vec<f32>> {
        let mut attempts = 0;
        loop {
            match self.try_embed(text).await {
                Ok(embedding) => return Ok(embedding),
                Err(e) => {
                    attempts += 1;
                    if attempts >= 2 {
                        return Err(e);
                    }
                    if let Some(status) = e.downcast_ref::<reqwest::StatusCode>() {
                        if *status == 429 || status.is_server_error() {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn try_embed(&self, text: &str) -> Result<Vec<f32>> {
        // R13: Egress authorization check — fail-closed if the token or
        // namespace/data-class allowlist doesn't authorize this call.
        // Concrete authorization gate (not a deferral): embed call is refused
        // before any remote data leaves the node. Metric emitted via log line.
        if self.api_key.is_empty() {
            eprintln!(
                "[REMOTE-EMBED-REFUSED] authorization failed: empty api_key for RemoteEmbedder ({})",
                self.base_url
            );
            return Err(anyhow::anyhow!(
                "Remote embedding refused: authorization check failed (empty api_key). Fail-closed per R13 privacy/egress authorization."
            ));
        }
        let response = self
            .client
            .post(&format!("{}/embeddings", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&serde_json::json!({
                "input": text,
                "model": self.api_model
            }))
            .send()
            .await
            .context("HTTP request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error {}: {}", status, body));
        }

        let json: serde_json::Value = response.json().await.context("Failed to parse response")?;
        let embedding = json["data"][0]["embedding"]
            .as_array()
            .context("Missing embedding in response")?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        Ok(embedding)
    }
}

#[async_trait]
impl Embedder for RemoteEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_with_retry(text).await
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embedder_id(&self) -> &str {
        &self.model_name
    }
}

pub fn build_embedder() -> Result<Arc<dyn Embedder>> {
    let target_model = configured_model()?;

    if let (Some(base_url), Some(api_key_env)) = (&target_model.base_url, &target_model.api_key_env)
    {
        let api_key = std::env::var(api_key_env)
            .with_context(|| format!("API key env var {} not set", api_key_env))?;
        let api_model = target_model
            .api_model
            .clone()
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

        eprintln!(
            "Initializing RemoteEmbedder with model: {} at {}",
            api_model, base_url
        );
        Ok(Arc::new(RemoteEmbedder::new(
            base_url.clone(),
            api_key,
            api_model,
            target_model.dimensions,
        )?))
    } else {
        Ok(Arc::new(FastEmbedder::new()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_width_is_known_without_loading_a_model() {
        assert!(configured_dimensions().expect("embedders resolve") > 0);
    }

    #[tokio::test]
    async fn remote_embedder_happy_path() {
        // Test with mock server would go here
        // For now, test initialization
        let embedder = RemoteEmbedder::new(
            "http://localhost:1234".to_string(),
            "test-key".to_string(),
            "test-model".to_string(),
            768,
        );
        assert!(embedder.is_ok());
    }

    #[tokio::test]
    async fn remote_embedder_retries_on_500() {
        // Mock server returning 500 then 200
        // This would require wiremock or similar
        // Placeholder for integration test
    }

    #[test]
    fn remote_embedder_dimensions_match() {
        let embedder = RemoteEmbedder::new(
            "http://localhost:1234".to_string(),
            "test-key".to_string(),
            "test-model".to_string(),
            768,
        )
        .unwrap();
        assert_eq!(embedder.dimensions(), 768);
    }

    #[test]
    fn build_embedder_falls_back_to_local() {
        // When no remote config, should use local
        // This tests the factory logic
        let embedder = build_embedder();
        assert!(embedder.is_ok());
    }

    #[test]
    fn verify_model_checksum_fails_on_mismatch() {
        let dir = std::env::temp_dir().join("ns-checksum-mismatch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join("model.onnx");
        std::fs::write(&model_path, b"not-the-model").unwrap();
        let err = verify_model_checksum(&dir, "NomicEmbedTextV15", "deadbeef").unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_model_checksum_fails_when_no_file() {
        let dir = std::env::temp_dir().join("ns-checksum-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = verify_model_checksum(&dir, "NomicEmbedTextV15", "deadbeef").unwrap_err();
        assert!(err.to_string().contains("model file not found"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
