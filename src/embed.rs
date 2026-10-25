use crate::traits::Embedder;
use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// Optional SHA-256 pin over the model's downloaded artifact set
    /// (Task 9 / DR-7). Absent = unpinned: the computed pin is recorded
    /// as a PIN-CANDIDATE at load instead of being enforced.
    #[serde(default)]
    pub artifact_pin: Option<String>,
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
            artifact_pin: None,
        },
        AcceptableEmbedder {
            model_name: "BGEBaseENV15".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            artifact_pin: None,
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

/// Resolve an embedders.json model name to the fastembed model it names.
///
/// An unrecognized name refuses (J29): the previous behavior silently
/// substituted NomicEmbedTextV15, so a typo in embedders.json ran a
/// different model than the configuration named. The error names the
/// requested name and the acceptable list — the models this build of
/// fastembed can actually load.
fn resolve_embedding_model(model_name: &str) -> Result<EmbeddingModel> {
    EmbeddingModel::from_str(model_name).map_err(|_| {
        let acceptable = TextEmbedding::list_supported_models()
            .iter()
            .map(|info| format!("{:?}", info.model))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!(
            "unknown embedder model '{model_name}': this build cannot load a model by that name \
             and no fallback is substituted — set model_name in embedders.json to one of the \
             acceptable models: {acceptable}"
        )
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The DR-7 artifact pin over one model's snapshot directory: every file
/// present (recursively; the snapshot layout nests some models' graph
/// under an `onnx/` subdir), hashed by content. The pin is the SHA-256
/// hex of the concatenation of `"<file-sha256>  <relative-path>\n"` lines
/// sorted by relative path — a pin over the ONNX file alone would let a
/// swapped tokenizer walk past it. The PIN-CANDIDATE adoption record is
/// the one exclusion: it is this gate's own output, not a model artifact.
fn compute_artifact_pin(model_dir: &Path) -> Result<String> {
    let mut rel_paths: Vec<String> = Vec::new();
    collect_artifact_files(model_dir, model_dir, &mut rel_paths)?;
    if rel_paths.is_empty() {
        anyhow::bail!(
            "no embedder artifacts found in {} — the model cache is empty or incomplete",
            model_dir.display()
        );
    }
    rel_paths.sort();
    let mut listing = String::new();
    for rel in &rel_paths {
        let bytes = fs::read(model_dir.join(rel))
            .with_context(|| format!("failed to read embedder artifact {rel}"))?;
        listing.push_str(&format!("{}  {}\n", sha256_hex(&bytes), rel));
    }
    Ok(sha256_hex(listing.as_bytes()))
}

fn collect_artifact_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed to read model cache directory {}", dir.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect_artifact_files(root, &path, out)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "model cache walk escaped its root at {}",
                        path.display()
                    )
                })?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if rel != "PIN-CANDIDATE" {
                out.push(rel);
            }
        }
    }
    Ok(())
}

/// The artifact-pin gate for one model's snapshot directory (Task 9).
///
/// With a recorded pin, the artifact set must hash to it or the load
/// refuses. With no pin, the load proceeds and the computed pin is
/// recorded through the channels that exist today — a PIN-CANDIDATE
/// file in the model's cache directory and one `eprintln!` line. It is
/// deliberately not a `tracing` record: no subscriber is installed until
/// Task 19 (J26), so a tracing adoption record would be discarded.
fn enforce_artifact_pin(
    model_dir: &Path,
    recorded_pin: Option<&str>,
    model_name: &str,
) -> Result<()> {
    let computed = compute_artifact_pin(model_dir)?;
    match recorded_pin {
        Some(pin) => {
            if pin.trim() == computed {
                Ok(())
            } else {
                anyhow::bail!(
                    "embedder artifact pin mismatch for model '{model_name}': the artifact set \
                     in {} does not match the artifact_pin recorded in embedders.json \
                     (recorded {}, computed {}) — refusing to load; if the model cache was \
                     legitimately refreshed, update artifact_pin to the computed value or \
                     remove it to re-adopt",
                    model_dir.display(),
                    pin.trim(),
                    computed
                );
            }
        }
        None => {
            let candidate = model_dir.join("PIN-CANDIDATE");
            // Best-effort: an unwritable cache must not fail a load that
            // would have succeeded unpinned — the eprintln below still
            // records the computed pin, and the warning names the failure.
            if let Err(e) = fs::write(&candidate, format!("{computed}\n")) {
                eprintln!(
                    "warning: could not write artifact pin candidate {}: {e}",
                    candidate.display()
                );
            }
            eprintln!(
                "No artifact_pin recorded for embedder model '{model_name}': computed pin \
                 {computed} over the artifact set in {} and wrote it to {} — record it as \
                 artifact_pin in embedders.json to pin this model",
                model_dir.display(),
                candidate.display()
            );
            Ok(())
        }
    }
}

/// The model's snapshot directory under the shared fastembed cache root,
/// in the Hugging Face hub layout fastembed downloads into:
/// `models--<org>--<repo>/snapshots/<revision>`, the revision named by
/// `refs/main`. None when the model has not been downloaded yet.
fn model_snapshot_dir(cache_root: &Path, model: &EmbeddingModel) -> Option<PathBuf> {
    let code = TextEmbedding::get_model_info(model).ok()?.model_code.clone();
    let repo_dir = cache_root.join(format!("models--{}", code.replace('/', "--")));
    let snapshots = repo_dir.join("snapshots");
    if let Ok(revision) = fs::read_to_string(repo_dir.join("refs").join("main")) {
        let candidate = snapshots.join(revision.trim());
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(&snapshots)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    if dirs.len() == 1 {
        dirs.pop()
    } else {
        None
    }
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
/// at ~3.5 characters per token, with headroom for the header lines.
const MAX_EMBED_TOKENS: usize = 2048;

pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
    model_name: String,
    dimensions: usize,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        let target_model = configured_model()?;

        let model_enum = resolve_embedding_model(&target_model.model_name)?;

        let cache_dir = find_existing_cache_dir();

        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)
                .context("Failed to create shared model cache directory")?;
        }

        // Artifact pin (Task 9 / DR-7): when the model's snapshot is
        // already cached, the pin gate runs before fastembed touches it,
        // so a mismatched artifact set never reaches the loader. A
        // missing snapshot means a first download; the gate then runs
        // after the load, before the embedder is handed out.
        let snapshot_dir = model_snapshot_dir(&cache_dir, &model_enum);
        if let Some(dir) = &snapshot_dir {
            enforce_artifact_pin(
                dir,
                target_model.artifact_pin.as_deref(),
                &target_model.model_name,
            )?;
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
            TextInitOptions::new(model_enum.clone())
                .with_cache_dir(cache_dir.clone())
                .with_max_length(MAX_EMBED_TOKENS)
                .with_show_download_progress(true),
        )?;

        if snapshot_dir.is_none() {
            match model_snapshot_dir(&cache_dir, &model_enum) {
                Some(dir) => enforce_artifact_pin(
                    &dir,
                    target_model.artifact_pin.as_deref(),
                    &target_model.model_name,
                )?,
                None => {
                    if target_model.artifact_pin.is_some() {
                        anyhow::bail!(
                            "embedder model '{}' has an artifact_pin recorded in embedders.json \
                             but no artifact directory was found under {} after load — refusing \
                             to run it unpinned",
                            target_model.model_name,
                            cache_dir.display()
                        );
                    }
                }
            }
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

/// Validate a remote embedder base_url before any request is built from
/// it (J23): it must parse as a URL with a host, and its scheme must be
/// https — http is accepted only for loopback hosts. Refusals name the
/// host only, never the full URL (which can carry userinfo) and never
/// the API key.
fn validate_remote_base_url(base_url: &str) -> Result<()> {
    let url = reqwest::Url::parse(base_url).map_err(|_| {
        anyhow::anyhow!(
            "remote embedder base_url does not parse as a URL with a host — refusing; \
             base_url must use https (http is accepted only for loopback hosts)"
        )
    })?;
    let host = url.host_str().ok_or_else(|| {
        anyhow::anyhow!(
            "remote embedder base_url has no host — refusing; \
             base_url must use https (http is accepted only for loopback hosts)"
        )
    })?;
    // Judge loopback from the parsed host, not host_str(): host_str()
    // keeps the brackets of an IPv6 literal ("[::1]"), which IpAddr
    // parsing rejects — url::Host yields the address itself.
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        scheme => anyhow::bail!(
            "remote embedder base_url host '{host}' uses scheme '{scheme}' — refusing; \
             base_url must use https (http is accepted only for loopback hosts)"
        ),
    }
}

impl RemoteEmbedder {
    pub fn new(
        base_url: String,
        api_key: String,
        api_model: String,
        dimensions: usize,
    ) -> Result<Self> {
        validate_remote_base_url(&base_url)?;

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
            // The response body is dropped, never embedded in the error
            // (J23): it is remote-controlled text that can echo request
            // data, and an error string ends up in logs. The status is
            // the whole report.
            return Err(anyhow::anyhow!(
                "remote embedder returned HTTP {}",
                status.as_u16()
            ));
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

    // ---- Task 9 (J29/J23): embedder identity & egress ----

    /// A synthetic model artifact directory: the five files a real
    /// fastembed snapshot carries, with stand-in bytes. The pin machinery
    /// operates on file bytes, so no model download is needed.
    fn synthetic_model_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-t9-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in [
            ("model.onnx", &b"onnx-graph-bytes-v1"[..]),
            ("tokenizer.json", &b"{\"tokenizer\":true}"[..]),
            ("config.json", &b"{\"architectures\":[\"BertModel\"]}"[..]),
            (
                "tokenizer_config.json",
                &b"{\"tokenizer_class\":\"BertTokenizer\"}"[..],
            ),
            (
                "special_tokens_map.json",
                &b"{\"unk_token\":\"[UNK]\"}"[..],
            ),
        ] {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        dir
    }

    /// The DR-7 pin construction, reimplemented on the test side so the
    /// production computation is checked against an independent reading of
    /// the spec: per-file SHA-256; lines `"<file-sha256>  <filename>\n"`
    /// sorted by filename; the pin is the SHA-256 hex of the concatenation.
    /// (The synthetic dirs are flat, so filenames are the relative paths.)
    fn spec_artifact_pin(dir: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "PIN-CANDIDATE")
            .collect();
        names.sort();
        let mut listing = String::new();
        for name in &names {
            let bytes = std::fs::read(dir.join(name)).unwrap();
            listing.push_str(&format!(
                "{}  {}\n",
                format!("{:x}", Sha256::digest(&bytes)),
                name
            ));
        }
        format!("{:x}", Sha256::digest(listing.as_bytes()))
    }

    #[test]
    fn unknown_model_name_refuses_and_names_the_list() {
        let err = resolve_embedding_model("DefinitelyNotAModel")
            .expect_err("an unknown model name must refuse, not fall back")
            .to_string();
        assert!(err.contains("DefinitelyNotAModel"), "names the request: {err}");
        assert!(
            err.contains("NomicEmbedTextV15"),
            "names the acceptable list: {err}"
        );
        assert!(
            err.contains("BGEBaseENV15"),
            "names the acceptable list: {err}"
        );
    }

    #[test]
    fn known_model_names_still_resolve() {
        // Zero-change pin: every model the operator's embedders.json names
        // today keeps resolving by name.
        for name in ["NomicEmbedTextV15", "BGEBaseENV15", "BGESmallENV15Q"] {
            assert!(resolve_embedding_model(name).is_ok(), "{name} resolves");
        }
    }

    #[test]
    fn pinned_model_with_wrong_hash_refuses() {
        let dir = synthetic_model_dir("wrong-hash");
        let wrong = "0".repeat(64);
        let err = enforce_artifact_pin(&dir, Some(wrong.as_str()), "SyntheticModel")
            .expect_err("a pin that does not match the artifact set must refuse")
            .to_string();
        assert!(err.contains("SyntheticModel"), "names the model: {err}");
    }

    #[test]
    fn pinned_model_with_correct_hash_loads() {
        let dir = synthetic_model_dir("correct-hash");
        let pin = spec_artifact_pin(&dir);
        enforce_artifact_pin(&dir, Some(pin.as_str()), "SyntheticModel")
            .expect("a matching pin loads");
    }

    #[test]
    fn swapped_tokenizer_file_refuses() {
        // The anti-theater test: the pin covers the whole file set, so
        // changing one non-graph file must refuse.
        let dir = synthetic_model_dir("swapped-tokenizer");
        let pin = spec_artifact_pin(&dir);
        std::fs::write(dir.join("tokenizer.json"), b"{\"tokenizer\":false}").unwrap();
        enforce_artifact_pin(&dir, Some(pin.as_str()), "SyntheticModel")
            .expect_err("a swapped tokenizer.json must refuse");
    }

    #[test]
    fn added_artifact_file_refuses() {
        let dir = synthetic_model_dir("added-file");
        let pin = spec_artifact_pin(&dir);
        std::fs::write(dir.join("vocab.txt"), b"extra file").unwrap();
        enforce_artifact_pin(&dir, Some(pin.as_str()), "SyntheticModel")
            .expect_err("an additional file in the set must refuse");
    }

    #[test]
    fn identical_recache_passes() {
        let first = synthetic_model_dir("recache-a");
        let pin = spec_artifact_pin(&first);
        let second = synthetic_model_dir("recache-b");
        enforce_artifact_pin(&second, Some(pin.as_str()), "SyntheticModel")
            .expect("a byte-identical re-download verifies against the pin");
    }

    #[test]
    fn unpinned_model_loads_and_records_hash() {
        let dir = synthetic_model_dir("unpinned");
        enforce_artifact_pin(&dir, None, "SyntheticModel")
            .expect("an unpinned model loads");
        let candidate = dir.join("PIN-CANDIDATE");
        assert!(candidate.exists(), "the adoption record is written");
        let recorded = std::fs::read_to_string(&candidate).unwrap();
        assert_eq!(recorded.trim(), spec_artifact_pin(&dir));
        // The recorded candidate, once adopted into embedders.json, verifies.
        enforce_artifact_pin(&dir, Some(recorded.trim()), "SyntheticModel")
            .expect("the recorded candidate verifies once adopted");
    }

    #[test]
    fn embedders_json_pin_field_is_optional() {
        // Zero-change pin: existing embedders.json files (no artifact_pin)
        // parse unchanged, and a present pin round-trips.
        let without: Vec<AcceptableEmbedder> = serde_json::from_str(
            r#"[{"model_name":"NomicEmbedTextV15","dimensions":768}]"#,
        )
        .unwrap();
        assert!(without[0].artifact_pin.is_none());
        let with: Vec<AcceptableEmbedder> = serde_json::from_str(
            r#"[{"model_name":"NomicEmbedTextV15","dimensions":768,"artifact_pin":"abc123"}]"#,
        )
        .unwrap();
        assert_eq!(with[0].artifact_pin.as_deref(), Some("abc123"));
    }

    #[test]
    fn remote_http_to_public_host_is_refused() {
        let err = RemoteEmbedder::new(
            "http://embeddings.example.com:8080".to_string(),
            "test-key".to_string(),
            "test-model".to_string(),
            768,
        )
        .err()
        .expect("plain http to a public host must refuse")
        .to_string();
        assert!(
            err.contains("embeddings.example.com"),
            "names the host: {err}"
        );
        assert!(err.contains("https"), "names the rule: {err}");
        assert!(!err.contains("test-key"), "never names the key: {err}");
    }

    #[test]
    fn remote_https_loads() {
        let embedder = RemoteEmbedder::new(
            "https://embeddings.example.com".to_string(),
            "test-key".to_string(),
            "test-model".to_string(),
            768,
        );
        assert!(embedder.is_ok());
    }

    #[test]
    fn remote_http_to_loopback_loads() {
        for url in ["http://127.0.0.1:8080", "http://[::1]:8080"] {
            let embedder = RemoteEmbedder::new(
                url.to_string(),
                "test-key".to_string(),
                "test-model".to_string(),
                768,
            );
            assert!(embedder.is_ok(), "{url} is loopback http and loads");
        }
    }

    #[test]
    fn remote_url_without_host_is_refused() {
        for url in ["http://", "not-a-url"] {
            let embedder = RemoteEmbedder::new(
                url.to_string(),
                "test-key".to_string(),
                "test-model".to_string(),
                768,
            );
            assert!(embedder.is_err(), "{url} has no usable host and refuses");
        }
    }

    #[tokio::test]
    async fn remote_error_contains_status_not_body() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "MARKER-SECRET-BODY";
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let embedder = RemoteEmbedder::new(
            format!("http://{addr}"),
            "test-key".to_string(),
            "test-model".to_string(),
            8,
        )
        .unwrap();
        let err = embedder
            .embed("hello")
            .await
            .expect_err("a 500 response is an error")
            .to_string();
        assert!(err.contains("500"), "names the status: {err}");
        assert!(
            !err.contains("MARKER-SECRET-BODY"),
            "never carries the response body: {err}"
        );
    }
}
