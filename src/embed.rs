use crate::traits::Embedder;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
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

/// The built-in default acceptable-embedder list: the single full-size
/// Nomic entry every fresh install starts from.
pub(crate) fn default_acceptable_embedders() -> Vec<AcceptableEmbedder> {
    vec![
        AcceptableEmbedder {
            model_name: "NomicEmbedTextV15".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            // Real SHA-256 of the fastembed Nomic v1.5 ONNX weights (computed).
            model_sha256: Some("147d5aa88c2101237358e17796cf3a227cead1ec304ec34b465bb08e9d952965".to_string()),
        },
    ]
}

/// The operator's config directory (`~/.config/neurostrata`).
pub(crate) fn config_dir() -> PathBuf {
    // dirs::home_dir() rather than $HOME: Windows sets USERPROFILE, not HOME, so
    // reading the variable directly sends this to a relative path under whatever
    // directory happened to launch the process. config.rs already resolves it this way.
    dirs::home_dir()
        .map(|home| home.join(".config").join("neurostrata"))
        .unwrap_or_else(|| PathBuf::from(".neurostrata"))
}

fn get_acceptable_embedders() -> Result<Vec<AcceptableEmbedder>> {
    get_acceptable_embedders_in(&config_dir())
}

pub(crate) fn get_acceptable_embedders_in(config_dir: &Path) -> Result<Vec<AcceptableEmbedder>> {
    if !config_dir.exists() {
        fs::create_dir_all(config_dir).context("Failed to create config directory")?;
    }

    let config_path = config_dir.join("embedders.json");

    // Default list if file doesn't exist or is empty
    let default_models = default_acceptable_embedders();

    if config_path.exists() {
        let content = fs::read_to_string(&config_path).context("Failed to read embedders.json")?;
        match serde_json::from_str::<Vec<AcceptableEmbedder>>(&content) {
            Ok(mut models) => {
                if !models.is_empty() {
                    // Backfill known pins for configs written before supply-chain
                    // verification existed; unknown models remain unpinned and fail
                    // closed at load time.
                    for model in &mut models {
                        if model.model_sha256.is_none()
                            && model.model_name.eq_ignore_ascii_case("NomicEmbedTextV15")
                        {
                            model.model_sha256 = Some(
                                "147d5aa88c2101237358e17796cf3a227cead1ec304ec34b465bb08e9d952965"
                                    .to_string(),
                            );
                        }
                    }
                    return Ok(models);
                }
            }
            Err(e) => {
                // A corrupt user file must never be silently overwritten: the
                // user's edits stay on disk while we run on the in-memory default.
                tracing::warn!(
                    "Failed to parse {}: {e}; using default embedder list in memory without overwriting existing file",
                    config_path.display()
                );
                return Ok(default_models);
            }
        }
    }

    // Write default to file so user can edit it (only when the file is absent
    // or was an empty, valid list)
    if let Ok(json_content) = serde_json::to_string_pretty(&default_models) {
        if let Err(e) = fs::write(&config_path, json_content) {
            tracing::warn!(
                "Failed to write default embedder config to {}: {e}",
                config_path.display()
            );
        } else {
            // The default file was just materialized, so a first-run model
            // choice may still be offered: record it as pending for the
            // next interactive command. This path runs inside daemon and
            // serving processes, so it never prompts here.
            crate::model_choice::record_pending_choice(config_dir);
        }
    }

    Ok(default_models)
}

fn find_existing_cache_dir() -> Result<PathBuf> {
    // Same reason as above: $HOME is absent on Windows, and an empty home turns
    // every candidate below into a relative path, which is how a 523 MB model
    // cache ends up inside the caller's working directory.
    let home_path = dirs::home_dir()
        .context("Could not find home directory ($HOME / USERPROFILE unset)")?;

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
            return Ok(dir); // Return the first valid existing cache directory
        }
    }

    Ok(primary_neuro_cache)
}

/// The model this process is configured to use: the one NEUROSTRATA_MODEL names
/// when it is acceptable, otherwise the first acceptable entry.
fn configured_model() -> Result<AcceptableEmbedder> {
    pick_configured_model(get_acceptable_embedders()?)
}

/// [`configured_model`] against an explicit config directory.
pub(crate) fn configured_model_in(config_dir: &Path) -> Result<AcceptableEmbedder> {
    pick_configured_model(get_acceptable_embedders_in(config_dir)?)
}

fn pick_configured_model(acceptable_models: Vec<AcceptableEmbedder>) -> Result<AcceptableEmbedder> {
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

/// Token ceiling for one embedding. Roughly covers MAX_SYMBOL_CHARS of source
const MAX_EMBED_TOKENS: usize = 2048;

/// Arena allocation is disabled by default; `NEUROSTRATA_EMBED_ARENA` with
/// one of the house truthy spellings restores it for operators who measured a
/// benefit. Mirrors the HYBRID_SEARCH convention (1/true/yes/on).
fn arena_disabled_for(value: Option<&str>) -> bool {
    let enabled = matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    );
    !enabled
}

/// The pin gate `FastEmbedder::new` applies to the configured model: a
/// local entry without a pinned SHA-256 fails closed, so a hand-authored
/// or setup-written entry can never load unverified.
fn pinned_sha_for(target_model: &AcceptableEmbedder) -> Result<&str> {
    target_model.model_sha256.as_deref().ok_or_else(|| {
        anyhow!(
            "Embedder supply-chain error: model '{}' has no pinned SHA-256",
            target_model.model_name
        )
    })
}

/// Verify the SHA-256 of the exact model file fastembed loads for
/// `model` against a pinned value. The file is located through
/// fastembed's own registry entry (`model_code` / `model_file`), in the
/// HuggingFace cache layout — never by filename guesswork: quantized
/// and optimized variants load files a `model.onnx`-only search cannot
/// see, and a sibling model's file must never satisfy this model's pin.
/// Returns `Err` on mismatch or when the file cannot be located
/// unambiguously (fail-closed).
pub fn verify_model_checksum(
    cache_dir: &Path,
    model: &EmbeddingModel,
    pinned_sha: &str,
) -> Result<()> {
    let model_name = format!("{model:?}");
    let info = TextEmbedding::get_model_info(model).map_err(|e| {
        anyhow!("Embedder supply-chain error: no fastembed model info for '{model_name}': {e}")
    })?;
    let model_dir = cache_dir.join(format!("models--{}", info.model_code.replace('/', "--")));
    let snapshot = resolve_snapshot_dir(&model_dir, &model_name, pinned_sha)?;
    let file = snapshot.join(&info.model_file);
    if !file.is_file() {
        return Err(anyhow!(
            "Embedder supply-chain error: model '{}' model file not found in cache for verification (pinned SHA: {}).",
            model_name, pinned_sha
        ));
    }
    let actual = hash_file_sha256(&file)?;
    if actual != pinned_sha {
        return Err(anyhow!(
            "Embedder supply-chain error: model '{}' checksum mismatch!\n  File: {}\n  Expected: {}\n  Actual: {}",
            model_name,
            file.display(),
            pinned_sha,
            actual
        ));
    }
    Ok(())
}

/// Select a cached model's snapshot directory: the one `refs/main`
/// names when it exists, else the only snapshot present. Several
/// snapshots with no ref to disambiguate fail closed rather than guess
/// which bytes the pin should be checked against.
fn resolve_snapshot_dir(model_dir: &Path, model_name: &str, pinned_sha: &str) -> Result<PathBuf> {
    let snapshots_dir = model_dir.join("snapshots");
    if let Ok(rev) = fs::read_to_string(model_dir.join("refs").join("main")) {
        let named = snapshots_dir.join(rev.trim());
        if named.is_dir() {
            return Ok(named);
        }
        // A refs/main that reads successfully but names a missing
        // snapshot is fail-closed — the ref is present and
        // authoritative, so falling through to enumeration
        // would verify a snapshot the ref does not name. Only a
        // genuinely unreadable refs/main (absent file) falls through
        // to the exactly-one-snapshot rule below.
        return Err(anyhow!(
            "Embedder supply-chain error: model '{}' refs/main names snapshot '{}', which is not in the cache; refusing to verify a different snapshot (pinned SHA: {}).",
            model_name,
            rev.trim(),
            pinned_sha
        ));
    }
    let mut snapshots: Vec<PathBuf> = match fs::read_dir(&snapshots_dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    };
    match snapshots.len() {
        1 => Ok(snapshots.pop().expect("exactly one snapshot")),
        0 => Err(anyhow!(
            "Embedder supply-chain error: model '{}' model file not found in cache for verification (pinned SHA: {}).",
            model_name, pinned_sha
        )),
        count => Err(anyhow!(
            "Embedder supply-chain error: model '{}' has {} cached snapshots and no refs/main to select one; refusing to guess which to verify (pinned SHA: {}).",
            model_name, count, pinned_sha
        )),
    }
}

/// Streamed SHA-256 of a file (model files run to hundreds of MiB; the
/// digest must not require holding one in memory).
fn hash_file_sha256(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = fs::File::open(path).with_context(|| {
        format!(
            "Failed to read model file {} for checksum verification",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).with_context(|| {
            format!(
                "Failed to read model file {} for checksum verification",
                path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Local fastembed-backed embedding provider.
pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
    model_name: String,
    dimensions: usize,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        let target_model = configured_model()?;

        // The descriptor must describe the model actually loaded: an unknown
        // configured name silently falls back to NomicEmbedTextV15 (768 dims).
        let (model_enum, loaded_model_name, loaded_dims) = match EmbeddingModel::from_str(
            &target_model.model_name,
        ) {
            Ok(m) => (m, target_model.model_name.clone(), target_model.dimensions),
            Err(_) => {
                tracing::warn!(
                    "Unknown embedding model '{}', falling back to NomicEmbedTextV15 (768 dims)",
                    target_model.model_name
                );
                (
                    EmbeddingModel::NomicEmbedTextV15,
                    "NomicEmbedTextV15".to_string(),
                    768,
                )
            }
        };

        let cache_dir = find_existing_cache_dir()?;

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
        // The CPU arena keeps the largest buffer any input ever needed.
        // Texts here are embedded one at a time at their own length, so the
        // arena ratchets up and never gives it back. This applies to every
        // local embedding (ingest, daemon queries, MCP calls), not only
        // ingest. Set NEUROSTRATA_EMBED_ARENA=1 to keep the previous
        // default; any other value (or unset) disables the arena.
        let mut init = TextInitOptions::new(model_enum.clone())
            .with_cache_dir(cache_dir.clone())
            .with_max_length(MAX_EMBED_TOKENS);
        // Deliberately pin the CPU execution provider: arena allocation is a
        // CPU-provider setting, and this build's fastembed/ort path is CPU-only.
        // A future non-CPU provider needs its own policy rather than inheriting
        // this one silently.
        let disable_arena =
            arena_disabled_for(std::env::var("NEUROSTRATA_EMBED_ARENA").ok().as_deref());
        static SAID_ARENA: std::sync::Once = std::sync::Once::new();
        SAID_ARENA.call_once(|| {
            eprintln!(
                "Embedding CPU arena: {}",
                if disable_arena { "disabled" } else { "enabled (NEUROSTRATA_EMBED_ARENA)" }
            );
        });
        if disable_arena {
            init = init.with_execution_providers(vec![ort::ep::CPU::default()
                .with_arena_allocator(false)
                .build()]);
        }
        let model = TextEmbedding::try_new(init.with_show_download_progress(true))?;

        // Supply-chain verification: the configured entry must carry a
        // pinned SHA-256, and the file fastembed actually loaded (the
        // resolved enum above, fallback included) must hash to it. This
        // protects against cache poisoning attacks where an attacker
        // could replace a downloaded model.
        let pinned_sha = pinned_sha_for(&target_model)?;
        verify_model_checksum(&cache_dir, &model_enum, pinned_sha)?;

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            model_name: loaded_model_name,
            dimensions: loaded_dims,
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
        embeddings
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Embedding model returned an empty vector batch"))
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embedder_id(&self) -> &str {
        &self.model_name
    }

    fn embedding_descriptor(&self) -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "fastembed".into(),
            model: self.model_name.clone(),
            revision: "onnx".into(),
            dims: self.dimensions,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
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

/// A typed carrier for the response status so the retry loop can downcast to
/// it from the anyhow chain (reqwest::StatusCode does not implement
/// std::error::Error itself).
#[derive(Debug)]
struct StatusError(reqwest::StatusCode);

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StatusError {}

impl RemoteEmbedder {
    pub fn new(
        base_url: String,
        api_key: String,
        api_model: String,
        dimensions: usize,
    ) -> Result<Self> {
        if dimensions == 0 {
            // Dimension validation becomes disabled (try_embed only checks len
            // when dimensions>0); surface that loudly at construction.
            tracing::warn!(
                "RemoteEmbedder configured with dimensions=0; dimension validation is disabled"
            );
        }
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
                    if let Some(s) = e.downcast_ref::<StatusError>() {
                        if s.0 == reqwest::StatusCode::TOO_MANY_REQUESTS
                            || s.0.is_server_error()
                        {
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
            // The status travels in the error chain so the retry loop in
            // `embed_with_retry` can downcast and tell retryable server
            // errors from fatal client ones.
            return Err(anyhow::Error::new(StatusError(status))
                .context(format!("API error {}: {}", status, body)));
        }

        let json: serde_json::Value = response.json().await.context("Failed to parse response")?;
        // Embedding-space identity persistence: a malformed remote vector must
        // be rejected outright. Substituting 0.0 for non-numeric or non-finite
        // elements would silently poison cosine distances, and a length that
        // disagrees with self.dimensions would be unmixable with every stored
        // vector in the space. Fail-closed (Composite Recommendation #11).
        let arr = json["data"][0]["embedding"]
            .as_array()
            .context("Missing embedding in response")?;
        if arr.is_empty() {
            anyhow::bail!("remote embedding was empty");
        }
        let mut embedding = Vec::with_capacity(arr.len());
        for v in arr {
            let val = v
                .as_f64()
                .ok_or_else(|| anyhow!("remote embedding contained non-finite or non-numeric value"))?;
            if !val.is_finite() {
                anyhow::bail!("remote embedding contained non-finite or non-numeric value");
            }
            embedding.push(val as f32);
        }
        if self.dimensions > 0 && embedding.len() != self.dimensions {
            anyhow::bail!(
                "remote embedding dimension mismatch: expected {}, got {}",
                self.dimensions,
                embedding.len()
            );
        }
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

    fn embedding_descriptor(&self) -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "openai-compatible".into(),
            model: self.model_name.clone(),
            revision: "unknown".into(),
            dims: self.dimensions,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
    }
}

/// The remote API model a remote entry is served under when it names
/// no `api_model` of its own. Shared by `build_embedder` (which
/// constructs the `RemoteEmbedder` with it) and the store's
/// configured-descriptor synthesis (which must predict that same
/// descriptor), so the default cannot drift between the two sites.
pub(crate) const DEFAULT_REMOTE_API_MODEL: &str = "text-embedding-3-small";

pub fn build_embedder() -> Result<Arc<dyn Embedder>> {
    let target_model = configured_model()?;

    let emb: Arc<dyn Embedder> = if let (Some(base_url), Some(api_key_env)) =
        (&target_model.base_url, &target_model.api_key_env)
    {
        let api_key = std::env::var(api_key_env)
            .with_context(|| format!("API key env var {} not set", api_key_env))?;
        let api_model = target_model
            .api_model
            .clone()
            .unwrap_or_else(|| DEFAULT_REMOTE_API_MODEL.to_string());

        eprintln!(
            "Initializing RemoteEmbedder with model: {} at {}",
            api_model, base_url
        );
        Arc::new(RemoteEmbedder::new(
            base_url.clone(),
            api_key,
            api_model,
            target_model.dimensions,
        )?) as Arc<dyn Embedder>
    } else {
        Arc::new(FastEmbedder::new()?) as Arc<dyn Embedder>
    };

    // Embedding-space identity must resolve at startup: descriptors get
    // persisted on namespace metadata and compared with `compatible`, so an
    // embedder that reports the "unknown" descriptor would defeat that check.
    let descriptor = emb.embedding_descriptor();
    if descriptor.compatible(&crate::traits::EmbeddingDescriptor::unknown()) {
        anyhow::bail!(
            "embedder '{}' reports the unknown embedding descriptor",
            emb.embedder_id()
        );
    }
    eprintln!(
        "Embedding space: provider={} model={} dims={}",
        descriptor.provider, descriptor.model, descriptor.dims
    );
    Ok(emb)
}

#[cfg(test)]
mod tests {
    #[test]
    fn arena_default_is_disabled_with_an_explicit_opt_in() {
        for off in [None, Some("0"), Some(""), Some("no"), Some("false")] {
            assert!(super::arena_disabled_for(off), "{off:?} is not opt-in");
        }
        for on in ["1", "true", "TRUE", "yes", "on"] {
            assert!(!super::arena_disabled_for(Some(on)), "{on:?} opts in");
        }
    }

    /// Source assertion: the env-parse test alone cannot catch the arena
    /// ratchet returning. Both flipping this to `true` and deleting the call
    /// fail here.
    #[test]
    fn the_constructor_disables_the_cpu_arena() {
        let source = include_str!("embed.rs");
        // Build the needle so this assertion's own text cannot satisfy it.
        let needle = format!(".with_arena_allocator({})", "false");
        assert!(
            source.contains(&needle),
            "FastEmbedder must disable the CPU arena allocator"
        );
    }

    use super::*;

    /// Read one full HTTP request (headers + declared body) off the socket,
    /// so responding cannot race the client still writing.
    async fn read_http_request(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&acc).to_string();
            if let Some(idx) = text.find("\r\n\r\n") {
                let mut content_length = 0usize;
                for line in text[..idx].lines() {
                    if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = rest.trim().parse().unwrap_or(0);
                    }
                }
                if acc.len() >= idx + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&acc).to_string()
    }

    #[test]
    fn the_configured_width_is_known_without_loading_a_model() {
        assert!(configured_dimensions().expect("embedders resolve") > 0);
    }

    #[tokio::test]
    async fn remote_embedder_happy_path() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let req = read_http_request(&mut sock).await;
            assert!(
                req.contains("POST /embeddings"),
                "request: {}",
                req
            );
            assert!(
                req.to_ascii_lowercase()
                    .contains("authorization: bearer test-key"),
                "request: {}",
                req
            );
            let body = r#"{"data":[{"embedding":[0.25,0.5,0.75,1.0]}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            use tokio::io::AsyncWriteExt;
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        });
        let embedder = RemoteEmbedder::new(
            format!("http://{}", addr),
            "test-key".to_string(),
            "test-model".to_string(),
            4,
        )
        .unwrap();
        let v = embedder.embed("hello").await.unwrap();
        assert_eq!(v, vec![0.25f32, 0.5, 0.75, 1.0]);
        server.await.unwrap();
    }

    /// Composite Recommendation #11: a remote response whose embedding is
    /// non-numeric, non-finite, empty, or dimension-mismatched must fail the
    /// embed call instead of silently surviving as 0.0 fills.
    #[tokio::test]
    async fn remote_embedder_rejects_non_numeric_and_mismatched_embeddings() {
        let bad_embeddings: Vec<(&str, &str)> = vec![
            ("non-numeric element", r#"[1.0, "not-a-number", 3.0, 4.0]"#),
            ("dimension mismatch (expected 4)", r#"[1.0, 2.0, 3.0]"#),
            ("empty", r#"[]"#),
        ];
        for (case, body_vec) in bad_embeddings {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.expect("accept");
                let _ = read_http_request(&mut sock).await;
                let body =
                    format!(r#"{{"data":[{{"embedding":{}}}]}}"#, body_vec);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                use tokio::io::AsyncWriteExt;
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.flush().await.unwrap();
            });
            let embedder = RemoteEmbedder::new(
                format!("http://{}", addr),
                "test-key".to_string(),
                "test-model".to_string(),
                4,
            )
            .unwrap();
            let err = embedder.embed("hello").await.expect_err(case);
            assert!(
                err.to_string().contains("remote embedding"),
                "case '{}' produced an unexpected error: {}",
                case,
                err
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn remote_embedder_retries_on_500() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut handled = 0usize;
            for attempt in 0..2 {
                let (mut sock, _) = listener.accept().await.expect("accept");
                let req = read_http_request(&mut sock).await;
                assert!(req.contains("POST /embeddings"), "request: {}", req);
                let (status_line, body) = if attempt == 0 {
                    ("HTTP/1.1 500 Internal Server Error", "server fail")
                } else {
                    (
                        "HTTP/1.1 200 OK",
                        r#"{"data":[{"embedding":[1.0,2.0,3.0,4.0]}]}"#,
                    )
                };
                let resp = format!(
                    "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                );
                use tokio::io::AsyncWriteExt;
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.flush().await.unwrap();
                handled += 1;
            }
            handled
        });
        let embedder = RemoteEmbedder::new(
            format!("http://{}", addr),
            "test-key".to_string(),
            "test-model".to_string(),
            4,
        )
        .unwrap();
        let v = embedder.embed("retry me").await.unwrap();
        assert_eq!(v, vec![1.0f32, 2.0, 3.0, 4.0]);
        let handled = server.await.unwrap();
        assert_eq!(handled, 2, "server must see both the failed and the retried attempt");
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
    fn remote_embedder_descriptor_is_resolved_not_unknown() {
        let embedder = RemoteEmbedder::new(
            "http://localhost:1234".to_string(),
            "test-key".to_string(),
            "test-model".to_string(),
            768,
        )
        .unwrap();
        let d = embedder.embedding_descriptor();
        assert_eq!(d.provider, "openai-compatible");
        assert_eq!(d.model, "test-model");
        assert_eq!(d.dims, 768);
        assert!(!d.compatible(&crate::traits::EmbeddingDescriptor::unknown()));
    }

    #[test]
    fn build_embedder_falls_back_to_local() {
        // When no remote config, should use local
        // This tests the factory logic
        let embedder = build_embedder();
        assert!(embedder.is_ok());
    }

    /// A fresh temp cache root for the verifier fixtures.
    fn fresh_cache_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ns-checksum-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `bytes` as `model_file` inside a fixture snapshot in the real
    /// fastembed/HuggingFace cache layout:
    /// `models--{model_code}/snapshots/testrev/{model_file}`.
    fn fixture_snapshot_file(
        cache: &Path,
        model_code: &str,
        model_file: &str,
        bytes: &[u8],
    ) -> PathBuf {
        let file = cache
            .join(format!("models--{}", model_code.replace('/', "--")))
            .join("snapshots")
            .join("testrev")
            .join(model_file);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, bytes).unwrap();
        file
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn verify_model_checksum_fails_on_mismatch() {
        let dir = fresh_cache_dir("mismatch");
        fixture_snapshot_file(
            &dir,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model.onnx",
            b"not-the-model",
        );
        let err = verify_model_checksum(&dir, &EmbeddingModel::NomicEmbedTextV15, "deadbeef")
            .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_model_checksum_fails_when_no_file() {
        let dir = fresh_cache_dir("missing");
        let err = verify_model_checksum(&dir, &EmbeddingModel::NomicEmbedTextV15, "deadbeef")
            .unwrap_err();
        assert!(err.to_string().contains("model file not found"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_model_checksum_accepts_the_default_model_in_the_real_layout() {
        // The default flow must not regress: FP32 Nomic in the snapshot
        // layout verifies against the hash of the file fastembed loads.
        let dir = fresh_cache_dir("default-ok");
        let bytes = b"fp32-weights";
        fixture_snapshot_file(
            &dir,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model.onnx",
            bytes,
        );
        verify_model_checksum(&dir, &EmbeddingModel::NomicEmbedTextV15, &sha256_hex(bytes))
            .expect("the default model verifies against the file fastembed loads");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_hashes_the_file_fastembed_loads() {
        // NomicEmbedTextV15Q shares its snapshot with the FP32 file (the
        // pre-seeded cache shape): fastembed loads onnx/model_quantized.onnx,
        // so the quantized bytes' pin must pass and the FP32 sibling's
        // pin must fail — the old collector never saw the quantized file.
        let dir = fresh_cache_dir("quantized");
        let fp32 = b"fp32-sibling-bytes";
        let quantized = b"quantized-bytes";
        fixture_snapshot_file(
            &dir,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model.onnx",
            fp32,
        );
        fixture_snapshot_file(
            &dir,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model_quantized.onnx",
            quantized,
        );
        verify_model_checksum(
            &dir,
            &EmbeddingModel::NomicEmbedTextV15Q,
            &sha256_hex(quantized),
        )
        .expect("the quantized file's pin verifies");
        let err =
            verify_model_checksum(&dir, &EmbeddingModel::NomicEmbedTextV15Q, &sha256_hex(fp32))
                .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);

        // BGESmallENV15Q loads a root-level model_optimized.onnx; a decoy
        // onnx/model.onnx in the same snapshot must be ignored.
        let dir = fresh_cache_dir("optimized");
        let optimized = b"optimized-bytes";
        fixture_snapshot_file(
            &dir,
            "Qdrant/bge-small-en-v1.5-onnx-Q",
            "model_optimized.onnx",
            optimized,
        );
        fixture_snapshot_file(
            &dir,
            "Qdrant/bge-small-en-v1.5-onnx-Q",
            "onnx/model.onnx",
            b"decoy",
        );
        verify_model_checksum(
            &dir,
            &EmbeddingModel::BGESmallENV15Q,
            &sha256_hex(optimized),
        )
        .expect("the optimized file's pin verifies");
        let err =
            verify_model_checksum(&dir, &EmbeddingModel::BGESmallENV15Q, &sha256_hex(b"decoy"))
                .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_model_checksum_refuses_dangling_refs_main() {
        // A refs/main that reads successfully but names a snapshot
        // directory that does not exist is a fail-closed case.
        // Falling through to snapshot enumeration
        // would verify whichever snapshot happens to be present
        // instead of the one the ref names.
        let dir = fresh_cache_dir("dangling-ref");
        fixture_snapshot_file(
            &dir,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model.onnx",
            b"fp32-weights",
        );
        let model_dir = dir.join("models--nomic-ai--nomic-embed-text-v1.5");
        std::fs::create_dir_all(model_dir.join("refs")).unwrap();
        std::fs::write(model_dir.join("refs").join("main"), "missingrev").unwrap();
        let err = verify_model_checksum(
            &dir,
            &EmbeddingModel::NomicEmbedTextV15,
            &sha256_hex(b"fp32-weights"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("refs/main"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_model_checksum_follows_a_valid_refs_main() {
        // Control for the dangling-ref refusal: a refs/main naming an
        // existing snapshot selects exactly that snapshot.
        let dir = fresh_cache_dir("valid-ref");
        let bytes = b"fp32-weights";
        fixture_snapshot_file(
            &dir,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model.onnx",
            bytes,
        );
        let model_dir = dir.join("models--nomic-ai--nomic-embed-text-v1.5");
        std::fs::create_dir_all(model_dir.join("refs")).unwrap();
        std::fs::write(model_dir.join("refs").join("main"), "testrev").unwrap();
        verify_model_checksum(&dir, &EmbeddingModel::NomicEmbedTextV15, &sha256_hex(bytes))
            .expect("a valid refs/main selects its snapshot");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pin_gate_refuses_a_pinless_shortlist_entry() {
        // The loader backfills only the FP32 default's pin, so a
        // hand-authored shortlist entry without one reaches this gate
        // pinless and is refused with the actionable error. Extending
        // the backfill is a deliberate future change to this test and to
        // `hand_authored_shortlist_entry_without_pin_is_not_backfilled`.
        let entry = AcceptableEmbedder {
            model_name: "NomicEmbedTextV15Q".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            model_sha256: None,
        };
        let err = pinned_sha_for(&entry).unwrap_err();
        assert!(err.to_string().contains("no pinned SHA-256"), "{err}");
    }

    #[test]
    fn non_default_pick_pin_flows_from_writer_through_resolution_to_verification() {
        // The closest faithful FastEmbedder::new proxy that stays
        // hermetic: new() reads the global config dir and performs a real
        // model load, so this test runs the exact post-load sequence —
        // writer output -> configured_model_in resolution -> the pin gate
        // (pinned_sha_for) -> model-aware verify_model_checksum — for a
        // non-default pick. Which file gets hashed is proven against
        // fixture bytes by verify_hashes_the_file_fastembed_loads; the
        // emitted pin's correctness is the table constant asserted here.
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::set_var("NEUROSTRATA_MODEL", "");

        let root = std::env::temp_dir().join(format!("ns-pin-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let db_path = root.join("ladybug.db");
        let choice = crate::model_choice::candidates()
            .iter()
            .find(|c| c.model_name == "NomicEmbedTextV15Q")
            .expect("shortlist contains NomicEmbedTextV15Q");
        crate::model_choice::write_initial_embedders_file(&config_dir, choice, &db_path)
            .expect("write the pick");

        let resolved = configured_model_in(&config_dir).expect("resolve the pick");
        assert_eq!(resolved.model_name, "NomicEmbedTextV15Q");
        let pin = pinned_sha_for(&resolved).expect("the written pick carries a pin");
        assert_eq!(pin, choice.model_sha256);

        let model_enum = EmbeddingModel::from_str(&resolved.model_name).expect("enum parses");
        let cache = root.join("cache");
        let quantized = b"proxy-quantized-bytes";
        fixture_snapshot_file(
            &cache,
            "nomic-ai/nomic-embed-text-v1.5",
            "onnx/model_quantized.onnx",
            quantized,
        );
        verify_model_checksum(&cache, &model_enum, &sha256_hex(quantized))
            .expect("the resolved model verifies the file fastembed loads");

        // Negative control: the same entry with the pin stripped hits the
        // exact gate FastEmbedder::new applies — "no pinned SHA-256".
        let stripped =
            serde_json::json!([{ "model_name": "NomicEmbedTextV15Q", "dimensions": 768 }]);
        std::fs::write(config_dir.join("embedders.json"), stripped.to_string()).unwrap();
        let resolved = configured_model_in(&config_dir).expect("resolve the stripped file");
        let err = pinned_sha_for(&resolved).unwrap_err();
        assert!(err.to_string().contains("no pinned SHA-256"), "{err}");

        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
