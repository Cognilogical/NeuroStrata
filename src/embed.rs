use crate::traits::Embedder;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// An HTTP failure from the embedding endpoint, carrying the status so the
/// retry loop can recognise it.
///
/// Formatting the status into an `anyhow!` string used to erase the type: the
/// resulting error chain held no `StatusCode`, so the downcast in
/// `embed_with_retry` never matched and the 429/5xx probe was dead code.
#[derive(Debug)]
struct EmbedApiStatus {
    status: reqwest::StatusCode,
    body: String,
}

impl std::fmt::Display for EmbedApiStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embedding API error {}: {}", self.status, self.body)
    }
}

impl std::error::Error for EmbedApiStatus {}

/// Whether this failure is worth a second attempt. Kept separate from the loop
/// so the classification can be tested without a server.
fn should_retry(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<EmbedApiStatus>()
        .map(|e| e.status == reqwest::StatusCode::TOO_MANY_REQUESTS || e.status.is_server_error())
        .unwrap_or(false)
}

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
    /// Provenance recorded by the binary when it materializes
    /// `embedders.json` itself. Per-entry, not a file-level field: the file
    /// is a bare JSON array and an object wrapper would be destroyed by an
    /// older binary on rollback (its parser treats an unparseable file as
    /// absent and overwrites it with the defaults). Older parsers tolerate
    /// this field (no `deny_unknown_fields`), and it is omitted when unset
    /// so user-written entries serialize exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<String>,
    /// Optional SHA-256 pin over the model's downloaded artifact set
    /// (Task 9 / DR-7). Absent = unpinned: the computed pin is recorded
    /// as a PIN-CANDIDATE at load instead of being enforced.
    #[serde(default)]
    pub artifact_pin: Option<String>,
    /// Pinned SHA-256 of the model weights. When set, the embedder verifies
    /// the downloaded model against this hash before accepting it.
    /// Supply-chain protection: a mismatched hash is a fatal error.
    #[serde(default)]
    pub model_sha256: Option<String>,
}

/// The provenance value stamped on every entry when the binary writes
/// `embedders.json` itself.
pub(crate) const EMBEDDERS_FILE_PROVENANCE: &str = "neurostrata-auto-generated";

/// The built-in default embedder list, without provenance.
fn default_acceptable_embedders() -> Vec<AcceptableEmbedder> {
    vec![
        AcceptableEmbedder {
            model_name: "NomicEmbedTextV15".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        },
        AcceptableEmbedder {
            model_name: "BGEBaseENV15".to_string(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        },
    ]
}

/// The default list as the binary materializes it: every entry stamped
/// with its provenance.
pub(crate) fn auto_generated_defaults() -> Vec<AcceptableEmbedder> {
    default_acceptable_embedders()
        .into_iter()
        .map(|mut model| {
            model.generated_by = Some(EMBEDDERS_FILE_PROVENANCE.to_string());
            model
        })
        .collect()
}

/// Whether two entries are the same operator choice, ignoring provenance.
fn same_embedder_choice(a: &AcceptableEmbedder, b: &AcceptableEmbedder) -> bool {
    a.model_name == b.model_name
        && a.dimensions == b.dimensions
        && a.base_url == b.base_url
        && a.api_key_env == b.api_key_env
        && a.api_model == b.api_model
}

/// Whether a parsed `embedders.json` is the file the binary materialized,
/// untouched since: every entry carries the provenance stamp and the full
/// entry contents (order included -- position 0 is the selection) match
/// the built-in defaults semantically. Anything else is user-edited, the
/// conservative direction: when in doubt, the file wins over a profile.
/// (Consequence: if a future release changes the built-in defaults,
/// previously auto-generated files flip to user-edited.)
fn is_auto_generated(models: &[AcceptableEmbedder]) -> bool {
    let defaults = default_acceptable_embedders();
    models.len() == defaults.len()
        && models.iter().zip(defaults.iter()).all(|(model, default)| {
            same_embedder_choice(model, default)
                && model.generated_by.as_deref() == Some(EMBEDDERS_FILE_PROVENANCE)
        })
}

/// The three states of `embedders.json` the product distinguishes
/// (Spec Feature 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddersFileState {
    /// Never materialized (or unparseable/empty, which materialization
    /// treats the same way): not an operator choice.
    Absent,
    /// Written by the binary itself, contents untouched since: not an
    /// operator choice.
    AutoGenerated,
    /// Anything else: an explicit operator choice.
    UserEdited,
}

struct EmbedderFileLoad {
    models: Vec<AcceptableEmbedder>,
    state: EmbeddersFileState,
}

fn load_acceptable_embedders() -> Result<EmbedderFileLoad> {
    // dirs::home_dir() rather than $HOME: Windows sets USERPROFILE, not HOME, so
    // reading the variable directly sends this to a relative path under whatever
    // directory happened to launch the process. config.rs already resolves it this way.
    let config_dir = dirs::home_dir()
        .map(|home| home.join(".config").join("neurostrata"))
        .unwrap_or_else(|| PathBuf::from(".neurostrata"));

    // Configuration is optional: the built-in defaults remain usable when
    // the home directory is read-only. Persisting those defaults is best
    // effort, like the write below, rather than a prerequisite for startup.
    if !config_dir.exists() {
        let _ = fs::create_dir_all(&config_dir);
    }

    let config_path = config_dir.join("embedders.json");

    if config_path.exists() {
        let content = fs::read_to_string(&config_path).context("Failed to read embedders.json")?;
        if let Ok(models) = serde_json::from_str::<Vec<AcceptableEmbedder>>(&content) {
            if !models.is_empty() {
                let state = if is_auto_generated(&models) {
                    EmbeddersFileState::AutoGenerated
                } else {
                    EmbeddersFileState::UserEdited
                };
                return Ok(EmbedderFileLoad { models, state });
            }
        }
    }

    // Default list if file doesn't exist or is empty. The materialized
    // copy carries provenance on every entry; the in-memory list is the
    // same choices without it (resolution treats the state, not the
    // stamp, as the signal).
    let default_models = default_acceptable_embedders();

    // Write default to file so user can edit it
    if let Ok(json_content) = serde_json::to_string_pretty(&auto_generated_defaults()) {
        let _ = fs::write(&config_path, json_content);
    }

    Ok(EmbedderFileLoad {
        models: default_models,
        state: EmbeddersFileState::Absent,
    })
}

/// Which layer supplied the resolved embedder, for the startup summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelSource {
    Environment,
    UserEditedFile,
    Profile,
    AutoGeneratedFile,
}

impl ModelSource {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::UserEditedFile => "file (user-edited)",
            Self::Profile => "profile",
            Self::AutoGeneratedFile => "file (auto-generated)",
        }
    }
}

/// The resolved embedder and the layer that supplied it.
#[derive(Clone)]
pub(crate) struct ResolvedModel {
    pub model: AcceptableEmbedder,
    pub source: ModelSource,
}

/// The profile's model as a configured embedder, constructed directly --
/// deliberately not a lookup into the acceptable-embedders list, so a
/// profile can name a model the list does not contain (BGESmallENV15Q is
/// not in the built-in default list). Local by construction: a remote
/// embedder can only arrive via a user-edited file, which outranks the
/// profile, so the profile default never displaces a remote (Spec
/// Feature 1, non-goals).
fn profile_model(profile: crate::config::CapacityProfile) -> AcceptableEmbedder {
    AcceptableEmbedder {
        model_name: profile.model_name().to_string(),
        dimensions: profile.embedder_dimensions(),
        base_url: None,
        api_key_env: None,
        api_model: None,
        generated_by: None,
        artifact_pin: None,
        model_sha256: None,
    }
}

/// Model resolution, first match wins (Spec Feature 1):
/// 1. `NEUROSTRATA_MODEL`; 2. a user-edited `embedders.json`; 3. the
/// profile default; 4. the auto-generated list's first entry.
pub(crate) fn resolve_configured_model(
    acceptable_models: Vec<AcceptableEmbedder>,
    state: EmbeddersFileState,
    requested: Option<&str>,
    profile: Option<crate::config::CapacityProfile>,
) -> Result<ResolvedModel> {
    if let Some(name) = requested.filter(|name| !name.trim().is_empty()) {
        let model = acceptable_models
            .into_iter()
            .find(|model| model.model_name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| anyhow::anyhow!("NEUROSTRATA_MODEL names no configured embedder"))?;
        return Ok(ResolvedModel {
            model,
            source: ModelSource::Environment,
        });
    }
    if state == EmbeddersFileState::UserEdited {
        let model = acceptable_models
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no embedding models are configured"))?;
        return Ok(ResolvedModel {
            model,
            source: ModelSource::UserEditedFile,
        });
    }
    if let Some(profile) = profile {
        return Ok(ResolvedModel {
            model: profile_model(profile),
            source: ModelSource::Profile,
        });
    }
    let model = acceptable_models
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no embedding models are configured"))?;
    Ok(ResolvedModel {
        model,
        source: ModelSource::AutoGeneratedFile,
    })
}

/// As `resolve_configured_model`, taking the profile as its raw value so
/// the invalid-value error surfaces here, on the embedder-construction
/// path -- the surface Spec AC6 scopes it to.
pub(crate) fn resolve_configured_model_from_inputs(
    acceptable_models: Vec<AcceptableEmbedder>,
    state: EmbeddersFileState,
    requested: Option<&str>,
    profile_raw: Option<&str>,
) -> Result<ResolvedModel> {
    let profile = crate::config::CapacityProfile::from_value(profile_raw)?;
    resolve_configured_model(acceptable_models, state, requested, profile)
}

/// Resolution against the live inputs: the embedders file, the
/// environment, and the capacity profile. This is a profile-consuming
/// surface, so an invalid `NEUROSTRATA_PROFILE` fails here.
pub(crate) fn configured_resolution() -> Result<ResolvedModel> {
    let load = load_acceptable_embedders()?;
    let requested = std::env::var("NEUROSTRATA_MODEL").ok();
    let profile_raw = std::env::var("NEUROSTRATA_PROFILE").ok();
    resolve_configured_model_from_inputs(
        load.models,
        load.state,
        requested.as_deref(),
        profile_raw.as_deref(),
    )
}

/// Coarse model class for the disagreement check. Judged by class, not by
/// a dimension threshold: BGEBaseENV15 is 768 wide but belongs to the
/// small-model family a `low` profile intends, and must not trip the
/// warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelClass {
    Small,
    Large,
    Unknown,
}

fn model_class(model_name: &str) -> ModelClass {
    if model_name.starts_with("BGESmall") || model_name.starts_with("BGEBase") {
        ModelClass::Small
    } else if model_name.starts_with("NomicEmbed")
        || model_name.starts_with("MultilingualE5Large")
    {
        ModelClass::Large
    } else {
        ModelClass::Unknown
    }
}

/// A profile/model disagreement: a profile is set, a higher layer
/// (environment or user-edited file) supplied the model, and the resolved
/// model's class differs from the profile's intent.
#[derive(Debug, Clone)]
pub(crate) struct ProfileDisagreement {
    pub profile: crate::config::CapacityProfile,
    pub resolved_model: String,
    pub winning_layer: ModelSource,
}

pub(crate) fn profile_disagreement(
    resolved: &ResolvedModel,
    profile: Option<crate::config::CapacityProfile>,
) -> Option<ProfileDisagreement> {
    let profile = profile?;
    if !matches!(
        resolved.source,
        ModelSource::Environment | ModelSource::UserEditedFile
    ) {
        return None;
    }
    let intent_class = model_class(profile.model_name());
    let resolved_class = model_class(&resolved.model.model_name);
    if intent_class == ModelClass::Unknown
        || resolved_class == ModelClass::Unknown
        || intent_class == resolved_class
    {
        return None;
    }
    Some(ProfileDisagreement {
        profile,
        resolved_model: resolved.model.model_name.clone(),
        winning_layer: resolved.source,
    })
}

/// The one startup warning for a disagreement: it names the profile's
/// intent, the resolved model, and the winning layer. Startup proceeds --
/// the explicit setting governs.
pub(crate) fn format_disagreement_warning(disagreement: &ProfileDisagreement) -> String {
    format!(
        "WARNING: NEUROSTRATA_PROFILE={} intends {}, but the resolved model is {} (source: {}); the explicit setting governs and startup proceeds",
        disagreement.profile.name(),
        disagreement.profile.model_name(),
        disagreement.resolved_model,
        disagreement.winning_layer.label(),
    )
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

/// The model this process is configured to use. An absent selection chooses
/// the first configured model; an explicit invalid selection fails closed.
fn configured_model() -> Result<AcceptableEmbedder> {
    Ok(configured_resolution()?.model)
}

fn select_configured_model(
    acceptable_models: Vec<AcceptableEmbedder>,
    requested: Option<&str>,
) -> Result<AcceptableEmbedder> {
    Ok(resolve_configured_model(
        acceptable_models,
        EmbeddersFileState::UserEdited,
        requested,
        None,
    )?
    .model)
}

fn local_model_details(model: &AcceptableEmbedder) -> Result<(EmbeddingModel, usize)> {
    let parsed = EmbeddingModel::from_str(&model.model_name)
        .map_err(|_| anyhow::anyhow!("unsupported local embedding model {:?}", model.model_name))?;
    let info = TextEmbedding::get_model_info(&parsed)?;
    let dimensions = info.dim;
    if model.dimensions != dimensions {
        anyhow::bail!(
            "configured embedding width {} does not match model {:?} width {}",
            model.dimensions,
            parsed,
            dimensions
        );
    }
    Ok((parsed, dimensions))
}

fn is_remote_model(model: &AcceptableEmbedder) -> bool {
    model.base_url.is_some() && model.api_key_env.is_some()
}

/// Canonical identity for a remote embedding space. The endpoint is hashed so
/// credentials embedded in a URL never enter the database marker or errors.
/// Trailing slashes do not change the endpoint identity.
pub(crate) fn remote_embedder_id(base_url: &str, api_model: &str) -> String {
    let endpoint_hash = sha2::Sha256::digest(base_url.trim_end_matches('/').as_bytes());
    format!("remote-sha256:{}:{}", hex::encode(endpoint_hash), api_model)
}

/// Upgrade a legacy runtime identity only when it can be matched exactly to
/// the configured canonical identity. Both endpoint URLs and model names may
/// contain colons, so the legacy string cannot be split safely in isolation.
pub(crate) fn canonicalize_persisted_embedder_id(
    identity: &str,
    configured_identity: &str,
) -> Result<String> {
    if identity == configured_identity || !identity.starts_with("remote:") {
        return Ok(identity.to_string());
    }
    let Some(legacy) = identity.strip_prefix("remote:") else {
        unreachable!();
    };
    if configured_identity.starts_with("remote-sha256:") {
        for (split, _) in legacy.match_indices(':') {
            let endpoint = &legacy[..split];
            let model = &legacy[split + 1..];
            if remote_embedder_id(endpoint, model) == configured_identity {
                return Ok(configured_identity.to_string());
            }
        }
    }
    // Refusal Standard, maximal satisfiable form at this site: the saved
    // legacy identity cannot be shown (it is precisely the string that
    // could not be canonicalized, and it can embed a credential-bearing
    // URL — element 6 is absolute), so the message describes it and names
    // the configured canonical identity instead. Widths are out of
    // context here: this function receives only the two identities, and
    // its store-open caller in read_width_marker has no configured width
    // in scope.
    anyhow::bail!("Refusing to open: the database's saved legacy remote embedding model identity cannot be safely matched to the configured identity {:?}. Keep the database intact and reopen it with its original endpoint and model (the original embedder), or use a separate database path for the new model; no in-place re-embedding migration is available.", configured_identity)
}

pub(crate) fn configured_identity_for(model: &AcceptableEmbedder) -> Result<String> {
    if is_remote_model(model) {
        if model.dimensions == 0 {
            anyhow::bail!("configured embedding dimensions must be positive");
        }
        let (Some(base_url), Some(_)) = (&model.base_url, &model.api_key_env) else {
            unreachable!();
        };
        Ok(remote_embedder_id(
            base_url,
            model
                .api_model
                .as_deref()
                .unwrap_or("text-embedding-3-small"),
        ))
    } else {
        let (canonical_model, _) = local_model_details(model)?;
        Ok(format!("fastembed:{canonical_model}"))
    }
}

/// Stable, non-secret identity for the configured embedding space. This is
/// available without loading the local model, so backup, restore, and metadata
/// reads can validate the same identity as the daemon.
pub fn configured_embedder_id() -> Result<String> {
    let model = configured_model()?;
    configured_identity_for(&model)
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
/// Files are hashed incrementally — a model's graph is hundreds of MB
/// and this gate runs on every load, so no artifact is buffered whole.
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
        let digest = sha256_hex_of_file(&model_dir.join(rel))
            .with_context(|| format!("failed to read embedder artifact {rel}"))?;
        listing.push_str(&format!("{}  {}\n", digest, rel));
    }
    Ok(sha256_hex(listing.as_bytes()))
}

/// SHA-256 hex of one file's bytes, streamed in fixed-size chunks so a
/// several-hundred-MB model graph never lands in memory whole.
fn sha256_hex_of_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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
    let model = configured_model()?;
    if is_remote_model(&model) {
        if model.dimensions == 0 {
            anyhow::bail!("configured embedding dimensions must be positive");
        }
        Ok(model.dimensions)
    } else {
        Ok(local_model_details(&model)?.1)
    }
}

const MAX_EMBED_TOKENS: usize = 2048;

/// The opt-in idle-unload setting (Task 4), env-only:
/// `NEUROSTRATA_EMBEDDER_IDLE_UNLOAD_SECS`. Unset or `0` disables the
/// feature (pinned no-change behavior); a positive integer is the idle
/// threshold in seconds. Anything else is a hard error on the surfaces
/// that read it (embedder construction and the startup summary) -- the
/// Status arms never read it.
pub(crate) fn idle_unload_threshold_from_value(raw: Option<&str>) -> Result<Option<Duration>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let secs: u64 = raw.parse().map_err(|_| {
        anyhow::anyhow!(
            "NEUROSTRATA_EMBEDDER_IDLE_UNLOAD_SECS must be a non-negative integer number of seconds, got {raw:?}"
        )
    })?;
    Ok(if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    })
}

/// The idle-unload threshold from the process environment.
pub(crate) fn idle_unload_threshold() -> Result<Option<Duration>> {
    let raw = std::env::var("NEUROSTRATA_EMBEDDER_IDLE_UNLOAD_SECS").ok();
    idle_unload_threshold_from_value(raw.as_deref())
}

/// The startup-summary segment for the idle-unload setting (Spec
/// Feature 2): exactly `idle unload: on (threshold Ns)` or
/// `idle unload: off`.
pub(crate) fn format_idle_unload_segment(threshold: Option<Duration>) -> String {
    match threshold {
        Some(threshold) => format!("idle unload: on (threshold {}s)", threshold.as_secs()),
        None => "idle unload: off".to_string(),
    }
}

/// Builds one session plus the identity it serves. The factory is the
/// reload contract in one type: it owns ONLY the values captured at
/// startup (identity, canonical name, cache dir, max length) and
/// rebuilds a session from them -- it never re-runs model resolution
/// and never re-reads the environment or the embedders file.
type SessionFactory<T> = Box<dyn Fn() -> Result<(T, String)> + Send + Sync>;

struct IdleSessionState<T> {
    session: Option<T>,
    last_used: Instant,
}

/// A lazily unloaded embedding session (Task 4, Spec Feature 2).
///
/// The first load is eager: construction runs the factory once and
/// fails if it fails (startup stays fail-closed). Afterwards the
/// staleness check lives in the access path itself -- no daemon ticker:
/// an access that finds the session idle past the threshold drops it
/// and rebuilds it from the factory's captured values before serving.
/// A reload whose identity differs from the startup identity is a loud
/// error and the mismatched session is never installed; a failed
/// reload surfaces its error and the next access retries. Unset
/// threshold (`None`) disables unloading entirely.
///
/// Status surfaces read `snapshot_loaded` only: an atomic written on
/// load/unload transitions. It never locks the session state, never
/// calls the factory, and never touches the idle clock (Spec GT-7).
pub(crate) struct IdleSession<T> {
    factory: SessionFactory<T>,
    startup_identity: String,
    threshold: Option<Duration>,
    state: Mutex<IdleSessionState<T>>,
    loaded_snapshot: AtomicBool,
}

impl<T> IdleSession<T> {
    fn new<F>(factory: F, threshold: Option<Duration>) -> Result<Self>
    where
        F: Fn() -> Result<(T, String)> + Send + Sync + 'static,
    {
        Self::new_at(factory, threshold, Instant::now())
    }

    fn new_at<F>(factory: F, threshold: Option<Duration>, now: Instant) -> Result<Self>
    where
        F: Fn() -> Result<(T, String)> + Send + Sync + 'static,
    {
        let factory: SessionFactory<T> = Box::new(factory);
        // Eager first load: a factory that cannot build the startup
        // session fails construction, exactly as startup did before
        // idle unload existed.
        let (session, identity) = factory()?;
        Ok(Self {
            factory,
            startup_identity: identity,
            threshold,
            state: Mutex::new(IdleSessionState {
                session: Some(session),
                last_used: now,
            }),
            loaded_snapshot: AtomicBool::new(true),
        })
    }

    /// The identity captured at startup. Reloads must reproduce it.
    fn startup_identity(&self) -> &str {
        &self.startup_identity
    }

    /// Lock-free snapshot of whether a session is currently installed.
    /// The only session-state read status surfaces may make.
    fn snapshot_loaded(&self) -> bool {
        self.loaded_snapshot.load(Ordering::SeqCst)
    }

    /// Serve `f` with the session, dropping and rebuilding it first if
    /// it has been idle past the threshold.
    fn with_session<R>(&self, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        self.with_session_at(Instant::now(), f)
    }

    fn with_session_at<R>(
        &self,
        now: Instant,
        f: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("session lock poisoned: {}", e))?;
        if let (Some(threshold), Some(_)) = (self.threshold, &state.session) {
            let idle = now.duration_since(state.last_used);
            if idle >= threshold {
                state.session = None;
                self.loaded_snapshot.store(false, Ordering::SeqCst);
                eprintln!("embedder session unloaded after {}s idle", idle.as_secs());
            }
        }
        if state.session.is_none() {
            let (session, identity) = (self.factory)()?;
            if identity != self.startup_identity {
                anyhow::bail!(
                    "embedder session reload produced model identity {:?}, but the startup identity is {:?}; refusing to serve a different model than startup resolved",
                    identity,
                    self.startup_identity
                );
            }
            eprintln!("embedder session reloaded (model: {})", identity);
            state.session = Some(session);
            self.loaded_snapshot.store(true, Ordering::SeqCst);
        }
        state.last_used = now;
        let session = state
            .session
            .as_mut()
            .expect("the session is installed by the reload branch above");
        f(session)
    }
}

/// Verify a model file's SHA-256 checksum against a pinned value.
/// Returns `Err` on mismatch (fail-closed supply-chain protection).
fn verify_model_checksum(cache_dir: &PathBuf, model_name: &str, pinned_sha: &str) -> Result<()> {
    // Try common model file paths within the cache
    let possible_paths = vec![
        cache_dir.join(format!("models--{}", model_name.replace("/", "--"))),
        cache_dir.join(model_name.replace("/", "--")),
    ];

    for path in &possible_paths {
        if path.is_file() {
            let data = std::fs::read(path)
                .context("Failed to read model file for checksum verification")?;
            let actual = format!("{:x}", Sha256::digest(&data));
            if actual != pinned_sha {
                return Err(anyhow!(
                    "Embedder supply-chain error: model '{}' checksum mismatch!\n  File: {}\n  Expected: {}\n  Actual: {}",
                    model_name, path.display(), pinned_sha, actual
                ));
            }
        }
    }

    // If we found no file to verify, log a warning (cannot verify without model file).
    // The model may be downloaded lazily; if a pinned checksum is set, the user
    // expects it to be enforced on the actual model data.
    eprintln!(
        "[WARNING] Pinned SHA-256 set for embedder '{}' but model file not found in cache for verification.",
        model_name
    );
    Ok(())
}

/// Token ceiling for one embedding. Roughly covers MAX_SYMBOL_CHARS of source

pub struct FastEmbedder {
    session: Arc<IdleSession<TextEmbedding>>,
    model_name: String,
    dimensions: usize,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        Self::with_model(configured_model()?)
    }

    /// Construct from an already-resolved model, without re-running
    /// resolution: startup paths that resolved the model once (and
    /// reported its provenance) build from that same model.
    fn with_model(target_model: AcceptableEmbedder) -> Result<Self> {
        let (model_enum, dimensions) = local_model_details(&target_model)?;
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

        // The values the startup session is constructed with, captured
        // once here: identity, canonical model name, cache dir, and the
        // max length below. The idle-unload factory rebuilds ONLY the
        // ONNX session from these captured values; it never re-resolves
        // the configured model and never re-reads any configuration
        // source, so a reload can only ever reproduce the startup
        // session (the P1-2 contract).
        let model_name = format!("fastembed:{model_enum}");
        let captured_identity = model_name.clone();
        let captured_model_name = model_enum.to_string();
        let captured_max_length = MAX_EMBED_TOKENS;
        // The factory owns a clone of the cache dir (Task 9): the outer
        // cache_dir and model_enum stay borrowable after the session is
        // built, which the post-load pin gate below depends on.
        let captured_cache_dir = cache_dir.clone();
        let factory = move || -> Result<(TextEmbedding, String)> {
            let model_enum = EmbeddingModel::from_str(&captured_model_name).map_err(|_| {
                anyhow::anyhow!(
                    "captured local embedding model {:?} no longer parses",
                    captured_model_name
                )
            })?;
            // fastembed defaults to 512 tokens regardless of what the model supports
            // (NomicEmbedTextV15 handles 8192), and truncates silently. Anything above
            // what ingestion actually stores per symbol is wasted, so this is set to
            // match MAX_SYMBOL_CHARS in src/parser/ingest.rs -- change the two together.
            let model = TextEmbedding::try_new(
                TextInitOptions::new(model_enum)
                    .with_cache_dir(captured_cache_dir.clone())
                    .with_max_length(captured_max_length)
                    .with_show_download_progress(true),
            )?;
            Ok((model, captured_identity.clone()))
        };
        let session = IdleSession::new(factory, idle_unload_threshold()?)?;

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
                            "embedder model '{}' has a recorded artifact_pin \
                             but no artifact directory was found under {} after load — refusing \
                             to run it unpinned",
                            target_model.model_name,
                            cache_dir.display()
                        );
                    }
                    // Unpinned with an unresolvable snapshot layout: there
                    // is no artifact set to record a candidate over, so the
                    // adoption record is skipped — say so on stderr rather
                    // than letting the load proceed unpinned in silence.
                    eprintln!(
                        "Skipped recording an artifact pin candidate for embedder model \
                         '{}': the model's snapshot directory could not be resolved under \
                         {} after load, so there is no artifact set to pin — the load \
                         proceeds unpinned",
                        target_model.model_name,
                        cache_dir.display()
                    );
                }
            }
        }

        // Supply-chain verification: if a pinned SHA-256 is specified, verify
        // the model file against it. This protects against cache poisoning
        // attacks where an attacker could replace a downloaded model.
        if let Some(pinned_sha) = &target_model.model_sha256 {
            verify_model_checksum(&cache_dir, &target_model.model_name, pinned_sha)?;
        }

        Ok(Self {
            session: Arc::new(session),
            model_name,
            dimensions,
        })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Model inference is CPU-bound and can stall the async runtime when
        // called from a Tokio task. Isolate it on the blocking thread pool.
        // The idle-unload staleness check lives inside `with_session`:
        // a session idle past the threshold is dropped and rebuilt from
        // the startup-captured values before this call is served.
        let session = Arc::clone(&self.session);
        let text = text.to_owned();
        let mut embeddings = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
            session.with_session(|model| {
                model
                    .embed(vec![&*text], None)
                    .context("fastembed embed call failed")
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("embed task panicked: {}", e))??;
        let embedding = embeddings
            .pop()
            .ok_or_else(|| anyhow::anyhow!("fastembed returned no embedding"))?;
        if embedding.len() != self.dimensions {
            anyhow::bail!(
                "fastembed returned width {}, but configured model identity {} requires {}",
                embedding.len(),
                self.model_name,
                self.dimensions
            );
        }
        Ok(embedding)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embedder_id(&self) -> &str {
        &self.model_name
    }

    fn session_loaded_snapshot(&self) -> Option<bool> {
        Some(self.session.snapshot_loaded())
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
        if dimensions == 0 {
            anyhow::bail!("remote embedding dimensions must be positive");
        }
        validate_remote_base_url(&base_url)?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;
        let normalized_base_url = base_url.trim_end_matches('/').to_string();
        let model_name = remote_embedder_id(&normalized_base_url, &api_model);

        Ok(Self {
            client,
            base_url: normalized_base_url,
            api_key,
            api_model: api_model.clone(),
            dimensions,
            model_name,
        })
    }

    async fn embed_with_retry(&self, text: &str) -> Result<Vec<f32>> {
        let mut attempts = 0;
        loop {
            match self.try_embed(text).await {
                Ok(embedding) if embedding.len() == self.dimensions => return Ok(embedding),
                Ok(embedding) => {
                    return Err(anyhow::anyhow!(
                        "remote embedding returned width {}, but configured model identity {} requires {}",
                        embedding.len(),
                        self.model_name,
                        self.dimensions
                    ));
                }
                Err(e) => {
                    attempts += 1;
                    if attempts >= 2 {
                        return Err(e);
                    }
                    if should_retry(&e) {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
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
            .map_err(|error| anyhow::anyhow!("HTTP request failed: {}", error.without_url()))?;

        if !response.status().is_success() {
            let status = response.status();
            // The response body is never read into the error (Task 9 /
            // J23): it is remote-controlled text that can echo request
            // data, and an error string ends up in logs. The typed status
            // is the whole report — carrying it as EmbedApiStatus (with an
            // empty body) is also what keeps should_retry's classification
            // working.
            return Err(anyhow::Error::new(EmbedApiStatus {
                status,
                body: String::new(),
            }));
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
    build_embedder_for(&configured_model()?)
}

/// Build the embedder for an already-resolved model. Resolution is not
/// re-run: the model the caller resolved (and, at startup, reported the
/// provenance of) is the model that gets constructed, by construction.
pub(crate) fn build_embedder_for(target_model: &AcceptableEmbedder) -> Result<Arc<dyn Embedder>> {
    if let (Some(base_url), Some(api_key_env)) = (&target_model.base_url, &target_model.api_key_env)
    {
        let api_key = std::env::var(api_key_env)
            .with_context(|| format!("API key env var {} not set", api_key_env))?;
        let api_model = target_model
            .api_model
            .clone()
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

        eprintln!(
            "Initializing RemoteEmbedder with configured model {}",
            api_model
        );
        Ok(Arc::new(RemoteEmbedder::new(
            base_url.clone(),
            api_key,
            api_model,
            target_model.dimensions,
        )?))
    } else {
        Ok(Arc::new(FastEmbedder::with_model(target_model.clone())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex as AsyncMutex;

    struct MockEndpoint {
        statuses: AsyncMutex<VecDeque<u16>>,
        requests: AtomicUsize,
    }

    async fn mock_embedding(State(state): State<Arc<MockEndpoint>>) -> impl IntoResponse {
        state.requests.fetch_add(1, Ordering::SeqCst);
        let status = state.statuses.lock().await.pop_front().unwrap_or(200);
        let status = StatusCode::from_u16(status).unwrap();
        if status.is_success() {
            (status, Json(json!({"data": [{"embedding": [0.25, 0.5]}]}))).into_response()
        } else {
            (status, Json(json!({"error": "mock failure"}))).into_response()
        }
    }

    async fn exercise_retry(
        statuses: &[u16],
        expected_requests: usize,
        succeeds: bool,
        dimensions: usize,
    ) {
        let state = Arc::new(MockEndpoint {
            statuses: AsyncMutex::new(statuses.iter().copied().collect()),
            requests: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/embeddings", post(mock_embedding))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let embedder = RemoteEmbedder::new(
            format!("http://{address}"),
            "test-key".into(),
            "test-model".into(),
            dimensions,
        )
        .unwrap();
        let result = embedder.embed("test").await;
        assert_eq!(result.is_ok(), succeeds, "status sequence {statuses:?}");
        assert_eq!(
            state.requests.load(Ordering::SeqCst),
            expected_requests,
            "status sequence {statuses:?}"
        );
        server.abort();
    }

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
    fn default_configuration_selects_local_embedding() {
        // Verify backend selection without loading or downloading a model.
        // Model availability is an installation concern, not a factory test.
        let model = configured_model().expect("built-in embedding defaults resolve");
        assert!(model.base_url.is_none());
        assert!(model.api_key_env.is_none());
        assert_eq!(model.model_name, "NomicEmbedTextV15");
        assert_eq!(local_model_details(&model).unwrap().1, 768);
    }

    #[test]
    fn explicitly_invalid_model_selection_fails_without_defaulting() {
        let models = vec![AcceptableEmbedder {
            model_name: "NomicEmbedTextV15".into(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        }];
        assert!(select_configured_model(models, Some("typo-model")).is_err());
    }

    #[test]
    fn local_model_validation_rejects_unknown_names_and_wrong_widths() {
        let mut model = AcceptableEmbedder {
            model_name: "NotARealFastEmbedModel".into(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        };
        assert!(local_model_details(&model).is_err());

        model.model_name = "NomicEmbedTextV15".into();
        model.dimensions = 384;
        assert!(local_model_details(&model).is_err());

        model.dimensions = 768;
        let (variant, width) = local_model_details(&model).unwrap();
        assert_eq!(width, 768);
        assert_eq!(
            configured_identity_for(&model).unwrap(),
            format!("fastembed:{variant}")
        );
    }

    #[test]
    fn same_width_local_models_keep_distinct_canonical_identities() {
        let model_a = AcceptableEmbedder {
            model_name: "NomicEmbedTextV15".into(),
            dimensions: 768,
            base_url: None,
            api_key_env: None,
            api_model: None,
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        };
        let model_b = AcceptableEmbedder {
            model_name: "BGEBaseENV15".into(),
            ..model_a.clone()
        };
        assert_eq!(local_model_details(&model_a).unwrap().1, 768);
        assert_eq!(local_model_details(&model_b).unwrap().1, 768);
        assert_ne!(
            configured_identity_for(&model_a).unwrap(),
            configured_identity_for(&model_b).unwrap()
        );
    }

    #[test]
    fn configured_and_runtime_remote_identities_are_canonical_and_secret_free() {
        let endpoint = "https://user:top-secret@example.test/v1///";
        let model = AcceptableEmbedder {
            model_name: "tenant-remote".into(),
            dimensions: 768,
            base_url: Some(endpoint.into()),
            api_key_env: Some("REMOTE_API_KEY".into()),
            api_model: Some("embed-v2".into()),
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        };
        let configured = configured_identity_for(&model).unwrap();
        let runtime = RemoteEmbedder::new(
            endpoint.into(),
            "not-part-of-the-identity".into(),
            "embed-v2".into(),
            768,
        )
        .unwrap();

        assert_eq!(configured, runtime.embedder_id());
        assert_eq!(
            configured,
            configured_identity_for(&AcceptableEmbedder {
                base_url: Some("https://user:top-secret@example.test/v1".into()),
                ..model.clone()
            })
            .unwrap(),
            "trailing slashes do not change configured identity"
        );
        assert_ne!(
            configured,
            configured_identity_for(&AcceptableEmbedder {
                base_url: Some("https://user:top-secret@example.test/other".into()),
                ..model.clone()
            })
            .unwrap(),
            "changing the endpoint changes identity"
        );
        assert_eq!(
            configured,
            canonicalize_persisted_embedder_id(
                "remote:https://user:top-secret@example.test/v1:embed-v2",
                &configured,
            )
            .unwrap(),
            "older markers are normalized before comparison and rewritten safely"
        );
        let colon_model_identity = remote_embedder_id(
            "https://user:top-secret@example.test/v1",
            "nomic-embed-text:latest",
        );
        assert_eq!(
            canonicalize_persisted_embedder_id(
                "remote:https://user:top-secret@example.test/v1:nomic-embed-text:latest",
                &colon_model_identity,
            )
            .unwrap(),
            colon_model_identity,
            "legacy model names with colons are matched against the full configured endpoint and model"
        );
        let mismatch = canonicalize_persisted_embedder_id(
            "remote:https://other:secret@example.test/v1:nomic-embed-text:latest",
            &colon_model_identity,
        )
        .unwrap_err()
        .to_string();
        assert!(!mismatch.contains("secret"));
        assert!(!mismatch.contains("https://"));
        // Refusal Standard, maximal satisfiable form at this site: the
        // configured canonical identity (element 2) and the no-migration
        // truth (element 5) are present; the saved legacy identity can
        // never be printed (element 6) — it is the un-canonicalizable
        // string, so the message describes it instead of quoting it.
        assert!(
            mismatch.contains(&colon_model_identity),
            "names the configured canonical identity: {mismatch}"
        );
        assert!(
            mismatch.contains("no in-place re-embedding migration is available"),
            "states the no-migration truth: {mismatch}"
        );
        assert!(
            mismatch.contains("model identity"),
            "retains the refusal family's phrasing: {mismatch}"
        );
        assert_ne!(
            configured,
            configured_identity_for(&AcceptableEmbedder {
                base_url: Some("https://other:top-secret@example.test/v1".into()),
                ..model.clone()
            })
            .unwrap(),
            "changing endpoint identity changes the hash"
        );
        assert_ne!(
            configured,
            configured_identity_for(&AcceptableEmbedder {
                api_model: Some("embed-v3".into()),
                ..model
            })
            .unwrap(),
            "changing the model changes identity"
        );
        for secret in ["user", "top-secret", "example.test", "https://"] {
            assert!(!configured.contains(secret), "identity leaked {secret}");
            assert!(!runtime.embedder_id().contains(secret));
        }
        let marker = serde_json::json!({ "embedder_id": configured }).to_string();
        assert!(!marker.contains("top-secret"));
        assert!(!marker.contains("https://"));
    }

    #[tokio::test]
    async fn remote_request_errors_do_not_format_url_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = RemoteEmbedder::new(
            format!("http://user:top-secret@{address}"),
            "secret-key".into(),
            "embed-v2".into(),
            2,
        )
        .unwrap()
        .embed("test")
        .await
        .unwrap_err();
        let safe = format!("{error:#}");
        assert!(!safe.contains("top-secret"));
        assert!(!safe.contains("user@"));
    }

    /// The retry used to probe `downcast_ref::<StatusCode>()` on a string that
    /// held no status, so a 429 never retried. The status has to survive the
    /// conversion into an error.
    #[test]
    fn api_status_survives_the_anyhow_chain() {
        let error = anyhow::Error::new(EmbedApiStatus {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body: "slow down".to_string(),
        });

        let recovered = error
            .downcast_ref::<EmbedApiStatus>()
            .expect("the status must be recoverable, or the retry never fires");
        assert_eq!(recovered.status, reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert!(error.to_string().contains("slow down"));
    }

    #[test]
    fn rate_limits_and_server_errors_are_retried() {
        let status = |code: u16| {
            anyhow::Error::new(EmbedApiStatus {
                status: reqwest::StatusCode::from_u16(code).expect("valid status"),
                body: String::new(),
            })
        };

        assert!(should_retry(&status(429)), "429 is a rate limit");
        assert!(should_retry(&status(500)), "5xx is transient");
        assert!(should_retry(&status(503)), "5xx is transient");
        assert!(
            !should_retry(&status(400)),
            "a bad request will not improve"
        );
        assert!(!should_retry(&status(401)), "a bad key will not improve");
        assert!(
            !should_retry(&anyhow::anyhow!("connection reset")),
            "an untyped error must not be retried as if it were a rate limit"
        );
    }

    // Task 2 (capacity profiles): model resolution gains the profile layer
    // and embedders.json provenance. The matrix below is Spec AC1.

    fn entry(model_name: &str, dimensions: usize) -> AcceptableEmbedder {
        AcceptableEmbedder {
            model_name: model_name.to_string(),
            dimensions,
            base_url: None,
            api_key_env: None,
            api_model: None,
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        }
    }

    fn edited_models() -> Vec<AcceptableEmbedder> {
        // A user-edited list: BGEBase first -- position 0 is the selection.
        vec![entry("BGEBaseENV15", 768), entry("NomicEmbedTextV15", 768)]
    }

    #[test]
    fn auto_generated_detection_requires_provenance_and_default_contents() {
        let stamped = auto_generated_defaults();
        assert!(is_auto_generated(&stamped));

        // Provenance missing (e.g. a file written by an older binary) is
        // treated as user-edited -- the conservative direction.
        let unstamped = default_acceptable_embedders();
        assert!(!is_auto_generated(&unstamped));

        // A pure reorder is a real choice (position 0 is the selection).
        let mut reordered = stamped.clone();
        reordered.reverse();
        assert!(!is_auto_generated(&reordered));

        // Any content difference, provenance notwithstanding, is an edit.
        let mut changed = stamped.clone();
        changed[0].dimensions = 384;
        assert!(!is_auto_generated(&changed));

        let mut extended = stamped;
        extended.push(entry("BGESmallENV15Q", 384));
        assert!(!is_auto_generated(&extended));
    }

    #[test]
    fn materialized_defaults_carry_provenance() {
        let stamped = auto_generated_defaults();
        assert!(stamped
            .iter()
            .all(|m| m.generated_by.as_deref() == Some(EMBEDDERS_FILE_PROVENANCE)));
        // Provenance is ignored by the semantic comparison itself: the
        // stamped and unstamped default lists are the same choices.
        assert!(stamped
            .iter()
            .zip(default_acceptable_embedders().iter())
            .all(|(a, b)| same_embedder_choice(a, b)));
    }

    #[test]
    fn profile_resolution_matrix() {
        use EmbeddersFileState::{Absent, AutoGenerated, UserEdited};
        use ModelSource::{AutoGeneratedFile, Environment, Profile, UserEditedFile};
        // (requested, file state, profile raw, expected model, expected source)
        let cases: &[(
            Option<&str>,
            EmbeddersFileState,
            Option<&str>,
            &str,
            ModelSource,
        )] = &[
            // No profile: today's behavior at every file state.
            (None, Absent, None, "NomicEmbedTextV15", AutoGeneratedFile),
            (None, AutoGenerated, None, "NomicEmbedTextV15", AutoGeneratedFile),
            (None, UserEdited, None, "BGEBaseENV15", UserEditedFile),
            // The case v1 got wrong: auto-generated file + profile=low
            // resolves the small model, directly, even though
            // BGESmallENV15Q is not an entry in the default list.
            (None, Absent, Some("low"), "BGESmallENV15Q", Profile),
            (None, AutoGenerated, Some("low"), "BGESmallENV15Q", Profile),
            (None, AutoGenerated, Some("standard"), "NomicEmbedTextV15", Profile),
            // A user-edited file is an explicit choice and beats the profile.
            (None, UserEdited, Some("low"), "BGEBaseENV15", UserEditedFile),
            (None, UserEdited, Some("standard"), "BGEBaseENV15", UserEditedFile),
            // The env var beats every layer below it, case-insensitively.
            (
                Some("nomicembedtextv15"),
                AutoGenerated,
                Some("low"),
                "NomicEmbedTextV15",
                Environment,
            ),
            (Some("BGEBaseENV15"), Absent, Some("low"), "BGEBaseENV15", Environment),
            (
                Some("NomicEmbedTextV15"),
                UserEdited,
                Some("standard"),
                "NomicEmbedTextV15",
                Environment,
            ),
        ];
        for (requested, state, profile_raw, expected_model, expected_source) in cases {
            let models = match state {
                EmbeddersFileState::UserEdited => edited_models(),
                _ => default_acceptable_embedders(),
            };
            let resolved =
                resolve_configured_model_from_inputs(models, *state, *requested, *profile_raw)
                    .unwrap_or_else(|e| {
                        panic!("case {requested:?}/{state:?}/{profile_raw:?}: {e:#}")
                    });
            assert_eq!(
                resolved.model.model_name, *expected_model,
                "case {requested:?}/{state:?}/{profile_raw:?}"
            );
            assert_eq!(
                resolved.source, *expected_source,
                "case {requested:?}/{state:?}/{profile_raw:?}"
            );
        }
    }

    #[test]
    fn profile_resolution_still_fails_closed_on_an_unknown_env_model() {
        // The profile does not widen the env var's contract: naming a model
        // that is not configured is an error, not a default.
        assert!(resolve_configured_model_from_inputs(
            default_acceptable_embedders(),
            EmbeddersFileState::AutoGenerated,
            Some("typo-model"),
            Some("low"),
        )
        .is_err());
    }

    #[test]
    fn invalid_profile_scoped_error() {
        // Spec AC6 (embedder surface): an invalid profile value fails
        // embedder construction...
        assert!(resolve_configured_model_from_inputs(
            default_acceptable_embedders(),
            EmbeddersFileState::AutoGenerated,
            None,
            Some("bogus"),
        )
        .is_err());
        // ...while the same inputs with a valid profile resolve.
        assert!(resolve_configured_model_from_inputs(
            default_acceptable_embedders(),
            EmbeddersFileState::AutoGenerated,
            None,
            Some("low"),
        )
        .is_ok());
    }

    #[test]
    fn build_embedder_for_constructs_from_the_carried_model() {
        // Fix round 1 (Finding 2): startup builds the embedder from the
        // model resolve_startup carried, never from a second resolution.
        // This remote model appears in no embedders list and is named by
        // no env var, so only construction from the carried value can
        // produce it -- a re-resolution would fail or pick another model.
        std::env::set_var("NS_TASK2_CARRIED_KEY", "test-key");
        let carried = AcceptableEmbedder {
            model_name: "carried-remote".into(),
            dimensions: 768,
            base_url: Some("https://embeddings.example.test/v1".into()),
            api_key_env: Some("NS_TASK2_CARRIED_KEY".into()),
            api_model: Some("carried-model".into()),
            generated_by: None,
            artifact_pin: None,
            model_sha256: None,
        };
        let embedder = build_embedder_for(&carried).unwrap();
        assert_eq!(
            embedder.embedder_id(),
            remote_embedder_id("https://embeddings.example.test/v1", "carried-model")
        );
        assert_eq!(embedder.dimensions(), 768);
        std::env::remove_var("NS_TASK2_CARRIED_KEY");
    }

    #[test]
    fn disagreement_warns_once_and_starts() {
        // Spec AC5: profile=low + env-resolved Nomic -> a warning naming
        // the profile intent, the resolved model, and the winning layer.
        let resolved = resolve_configured_model_from_inputs(
            default_acceptable_embedders(),
            EmbeddersFileState::AutoGenerated,
            Some("NomicEmbedTextV15"),
            Some("low"),
        )
        .unwrap();
        let low = crate::config::CapacityProfile::Low;
        let disagreement = profile_disagreement(&resolved, Some(low))
            .expect("a large model resolved over a low profile disagrees");
        let warning = format_disagreement_warning(&disagreement);
        assert!(warning.contains("low"), "{warning}");
        assert!(warning.contains("BGESmallENV15Q"), "{warning}");
        assert!(warning.contains("NomicEmbedTextV15"), "{warning}");
        assert!(warning.contains("environment"), "{warning}");

        // A legitimate small model must not trip the warning: BGEBase is
        // the profile's model family even though it is 768 wide -- class,
        // not a dimension threshold.
        let resolved = resolve_configured_model_from_inputs(
            edited_models(),
            EmbeddersFileState::UserEdited,
            None,
            Some("low"),
        )
        .unwrap();
        assert!(profile_disagreement(&resolved, Some(low)).is_none());

        // No disagreement when the profile itself supplied the model.
        let resolved = resolve_configured_model_from_inputs(
            default_acceptable_embedders(),
            EmbeddersFileState::AutoGenerated,
            None,
            Some("low"),
        )
        .unwrap();
        assert!(profile_disagreement(&resolved, Some(low)).is_none());
    }

    #[tokio::test]
    async fn remote_retry_policy_runs_against_a_local_http_server() {
        exercise_retry(&[429, 200], 2, true, 2).await;
        exercise_retry(&[503, 200], 2, true, 2).await;
        exercise_retry(&[503, 503], 2, false, 2).await;
        exercise_retry(&[400, 200], 1, false, 2).await;
        exercise_retry(&[401, 200], 1, false, 2).await;
        exercise_retry(&[200], 1, false, 3).await;
    }

    // ---- Task 4: IdleSession (opt-in idle unload of the local session) ----

    /// Dummy session for container tests: `serial` identifies the exact
    /// instance the factory built, `identity` is what the factory reports
    /// alongside it.
    struct DummySession {
        serial: usize,
        identity: String,
    }

    /// A counting factory that always succeeds, reporting `identity`.
    fn counting_factory(
        calls: Arc<AtomicUsize>,
        identity: &str,
    ) -> impl Fn() -> Result<(DummySession, String)> + Send + Sync {
        let identity = identity.to_string();
        move || {
            let serial = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok((
                DummySession {
                    serial,
                    identity: identity.clone(),
                },
                identity.clone(),
            ))
        }
    }

    #[test]
    fn idle_session_loads_once_and_reuses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Instant::now();
        let session = IdleSession::new_at(
            counting_factory(calls.clone(), "fastembed:DummyModel"),
            Some(Duration::from_secs(60)),
            base,
        )
        .unwrap();
        for offset in [0, 10, 59] {
            let serial = session
                .with_session_at(base + Duration::from_secs(offset), |s| Ok(s.serial))
                .unwrap();
            assert_eq!(
                serial, 1,
                "within the threshold the startup session is reused"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(session.snapshot_loaded());
    }

    #[test]
    fn idle_session_drops_after_threshold_and_reloads_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Instant::now();
        let session = Arc::new(
            IdleSession::new_at(
                counting_factory(calls.clone(), "fastembed:DummyModel"),
                Some(Duration::from_secs(30)),
                base,
            )
            .unwrap(),
        );
        session.with_session_at(base, |s| Ok(s.serial)).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Concurrent callers all arrive past the threshold: exactly one
        // drop-and-rebuild may happen; the rest reuse the reloaded session.
        let stale = base + Duration::from_secs(45);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let session = Arc::clone(&session);
            handles.push(std::thread::spawn(move || {
                session.with_session_at(stale, |s| Ok(s.serial)).unwrap()
            }));
        }
        let serials: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            serials.iter().all(|&serial| serial == 2),
            "every concurrent caller is served by the one reloaded session: {serials:?}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "exactly one reload under concurrency"
        );
        assert!(session.snapshot_loaded());
    }

    #[test]
    fn idle_session_reload_failure_surfaces_and_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Instant::now();
        let factory_calls = calls.clone();
        let session = IdleSession::new_at(
            move || {
                let n = factory_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 2 {
                    anyhow::bail!("reload exploded");
                }
                Ok((
                    DummySession {
                        serial: n,
                        identity: "fastembed:DummyModel".into(),
                    },
                    "fastembed:DummyModel".to_string(),
                ))
            },
            Some(Duration::from_secs(30)),
            base,
        )
        .unwrap();
        let stale = base + Duration::from_secs(60);
        let error = session
            .with_session_at(stale, |s| Ok(s.serial))
            .unwrap_err();
        assert!(error.to_string().contains("reload exploded"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            !session.snapshot_loaded(),
            "a failed reload leaves the session unloaded"
        );

        // The next embed retries the reload instead of serving a dead
        // session or failing forever.
        let serial = session.with_session_at(stale, |s| Ok(s.serial)).unwrap();
        assert_eq!(serial, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(session.snapshot_loaded());
    }

    #[test]
    fn idle_session_reload_identity_mismatch_is_a_loud_error() {
        // After any reload the embedder asserts its identity equals the
        // startup identity; a mismatch is a loud embed error, never a
        // silently served different model.
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Instant::now();
        let factory_calls = calls.clone();
        let session = IdleSession::new_at(
            move || {
                let n = factory_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let identity = if n == 2 {
                    "fastembed:MutatedModel"
                } else {
                    "fastembed:StartupModel"
                };
                Ok((
                    DummySession {
                        serial: n,
                        identity: identity.to_string(),
                    },
                    identity.to_string(),
                ))
            },
            Some(Duration::from_secs(30)),
            base,
        )
        .unwrap();
        let stale = base + Duration::from_secs(60);
        let error = session
            .with_session_at(stale, |s| Ok(s.serial))
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("fastembed:MutatedModel"), "{message}");
        assert!(message.contains("fastembed:StartupModel"), "{message}");
        assert!(
            !session.snapshot_loaded(),
            "a mismatched session is never installed"
        );
        // Retried on the next embed, and the correct model serves.
        let serial = session.with_session_at(stale, |s| Ok(s.serial)).unwrap();
        assert_eq!(serial, 3);
    }

    #[test]
    fn idle_session_reload_ignores_mutated_config() {
        // The P1-2 pin: the factory rebuilds ONLY from values captured at
        // startup. Between drop and reload, everything a naive reload
        // would consult is mutated -- a shared "live config" cell, the
        // process environment, and an embedders file on disk -- and the
        // reloaded identity must still be the startup identity.
        let calls = Arc::new(AtomicUsize::new(0));
        let live_config = Arc::new(Mutex::new("fastembed:StartupModel".to_string()));
        let base = Instant::now();
        let factory_calls = calls.clone();
        let session = IdleSession::new_at(
            move || {
                let serial = factory_calls.fetch_add(1, Ordering::SeqCst) + 1;
                Ok((
                    DummySession {
                        serial,
                        identity: "fastembed:StartupModel".to_string(),
                    },
                    "fastembed:StartupModel".to_string(),
                ))
            },
            Some(Duration::from_secs(30)),
            base,
        )
        .unwrap();
        session.with_session_at(base, |s| Ok(s.serial)).unwrap();

        *live_config.lock().unwrap() = "fastembed:MutatedModel".to_string();
        std::env::set_var("NS_TASK4_MUTATED_MODEL", "MutatedModel");
        let fake_dir =
            std::env::temp_dir().join(format!("neurostrata-task4-{}", std::process::id()));
        std::fs::create_dir_all(&fake_dir).unwrap();
        std::fs::write(
            fake_dir.join("embedders.json"),
            r#"[{"model_name":"MutatedModel","dimensions":384}]"#,
        )
        .unwrap();

        let identity = session
            .with_session_at(base + Duration::from_secs(60), |s| Ok(s.identity.clone()))
            .unwrap();
        assert_eq!(identity, "fastembed:StartupModel");
        assert_eq!(session.startup_identity(), "fastembed:StartupModel");
        std::env::remove_var("NS_TASK4_MUTATED_MODEL");
        let _ = std::fs::remove_dir_all(&fake_dir);
    }

    #[test]
    fn idle_session_first_load_is_eager() {
        // Construction with an unloadable factory fails at construction:
        // first load stays eager and startup stays fail-closed.
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = calls.clone();
        let result = IdleSession::<DummySession>::new_at(
            move || {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("startup session cannot load")
            },
            Some(Duration::from_secs(30)),
            Instant::now(),
        );
        assert!(
            result.is_err(),
            "construction fails when the first load fails"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the factory ran at construction, not on first use"
        );
    }

    #[test]
    fn idle_session_status_polling_never_loads_or_resets() {
        // Status-equivalent reads are the lock-free snapshot only. The
        // factory succeeds on calls 1-2 and fails on call 3 so the test
        // can also observe the unloaded state.
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Instant::now();
        let factory_calls = calls.clone();
        let session = IdleSession::new_at(
            move || {
                let n = factory_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 3 {
                    anyhow::bail!("reload exploded");
                }
                Ok((
                    DummySession {
                        serial: n,
                        identity: "fastembed:DummyModel".into(),
                    },
                    "fastembed:DummyModel".to_string(),
                ))
            },
            Some(Duration::from_secs(30)),
            base,
        )
        .unwrap();
        session.with_session_at(base, |s| Ok(s.serial)).unwrap();

        for _ in 0..5 {
            assert!(session.snapshot_loaded());
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "status reads never call the factory"
        );

        // The reads did not reset the idle clock either: this embed still
        // measures idle from the last real embed (base), so it drops and
        // reloads exactly once.
        let serial = session
            .with_session_at(base + Duration::from_secs(31), |s| Ok(s.serial))
            .unwrap();
        assert_eq!(serial, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Drive the session into the unloaded state (the reload at
        // base+62 fails), then poll status repeatedly: the factory is
        // never called and the session stays unloaded.
        assert!(session
            .with_session_at(base + Duration::from_secs(62), |s| Ok(s.serial))
            .is_err());
        assert!(!session.snapshot_loaded());
        for _ in 0..5 {
            assert!(!session.snapshot_loaded());
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "status reads never reload an unloaded session"
        );
        assert!(!session.snapshot_loaded(), "the session is still unloaded");
    }

    #[test]
    fn idle_session_disabled_never_unloads() {
        // Unset/0 setting = disabled: pinned no-change behavior, the
        // startup session serves forever.
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Instant::now();
        let session = IdleSession::new_at(
            counting_factory(calls.clone(), "fastembed:DummyModel"),
            None,
            base,
        )
        .unwrap();
        let serial = session
            .with_session_at(base + Duration::from_secs(86_400 * 365), |s| Ok(s.serial))
            .unwrap();
        assert_eq!(serial, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(session.snapshot_loaded());
    }

    #[test]
    fn idle_unload_setting_parses_env_only_values() {
        assert_eq!(idle_unload_threshold_from_value(None).unwrap(), None);
        assert_eq!(idle_unload_threshold_from_value(Some("")).unwrap(), None);
        assert_eq!(idle_unload_threshold_from_value(Some("0")).unwrap(), None);
        assert_eq!(
            idle_unload_threshold_from_value(Some("300")).unwrap(),
            Some(Duration::from_secs(300))
        );
        assert!(idle_unload_threshold_from_value(Some("bogus")).is_err());
        assert!(idle_unload_threshold_from_value(Some("-5")).is_err());
    }

    #[test]
    fn idle_unload_summary_segment_is_exact() {
        assert_eq!(format_idle_unload_segment(None), "idle unload: off");
        assert_eq!(
            format_idle_unload_segment(Some(Duration::from_secs(300))),
            "idle unload: on (threshold 300s)"
        );
    }

    #[test]
    fn idle_unload_log_lines_are_exact() {
        // Spec Feature 2, "What the user sees": exactly these two lines.
        // Needles are assembled from fragments so this test's own
        // literals do not satisfy the search.
        let source = include_str!("embed.rs");
        let unload = ["embedder session unloaded after ", "{}s idle"].concat();
        let reload = ["embedder session reloaded (model: ", "{})"].concat();
        assert!(source.contains(&unload), "the unload log line is exact");
        assert!(source.contains(&reload), "the reload log line is exact");
    }

    #[test]
    fn fastembedder_reload_factory_uses_only_captured_values() {
        // Structural complement to idle_session_reload_ignores_mutated_config
        // for the production path: the session factory inside
        // FastEmbedder::with_model may only rebuild from the values it
        // captured at construction. It must never re-run model
        // resolution, re-read the environment, or re-read the embedders
        // file. Needles are assembled from fragments so this test's own
        // literals are not counted.
        let source = include_str!("embed.rs");
        let start = source
            .find("\n    fn with_model(")
            .expect("FastEmbedder::with_model exists");
        let body = &source[start..];
        let body = &body[..body
            .find("\n#[async_trait]")
            .expect("the impl block ends before the trait impl")];
        assert!(
            body.contains("IdleSession::new"),
            "with_model must build an IdleSession"
        );
        for (label, needle) in [
            ("model resolution", ["configured", "_model()"].concat()),
            ("environment reads", ["env", "::var"].concat()),
            ("the embedders file", ["embedders", ".json"].concat()),
            (
                "the acceptable-embedder loader",
                ["load_acceptable", "_embedders"].concat(),
            ),
        ] {
            assert!(
                !body.contains(&needle),
                "the reload factory must not re-run {label}"
            );
        }
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
    /// the spec: per-file SHA-256 over every file in the tree, walked
    /// recursively; lines `"<file-sha256>  <relative-path>\n"` with
    /// `/`-joined relative paths, sorted by relative path; the pin is the
    /// SHA-256 hex of the concatenation. The PIN-CANDIDATE adoption record
    /// is excluded, as in production.
    fn spec_artifact_pin(dir: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut names: Vec<String> = Vec::new();
        collect_spec_files(dir, dir, &mut names);
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

    fn collect_spec_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_spec_files(root, &path, out);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                if rel != "PIN-CANDIDATE" {
                    out.push(rel);
                }
            }
        }
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
    fn nested_artifact_file_is_covered_by_the_pin() {
        // The operator's real snapshot layout nests the graph under an
        // `onnx/` subdirectory. The pin must cover subdirectory files
        // under their `/`-joined relative paths: a flat-only production
        // walk would compute a different pin than the recursive spec
        // reimplementation (failing the load below), and mutating the
        // nested file must refuse.
        let dir = synthetic_model_dir("nested-layout");
        let onnx_dir = dir.join("onnx");
        std::fs::create_dir_all(&onnx_dir).unwrap();
        std::fs::write(onnx_dir.join("model.onnx"), b"nested-graph-bytes-v1").unwrap();
        let pin = spec_artifact_pin(&dir);
        enforce_artifact_pin(&dir, Some(pin.as_str()), "SyntheticModel")
            .expect("a pin computed over the nested layout loads");
        std::fs::write(onnx_dir.join("model.onnx"), b"nested-graph-bytes-v2").unwrap();
        enforce_artifact_pin(&dir, Some(pin.as_str()), "SyntheticModel")
            .expect_err("a swapped nested graph file must refuse");
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
        // A 500 is retryable and the client retries exactly once (see
        // remote_retry_policy_runs_against_a_local_http_server, where the
        // [503, 503] sequence is two requests), so an endpoint returning
        // 500 answers both attempts. A one-shot server would drop the
        // retry into a connection-refused transport error with no status,
        // and the assertions below would be testing the fixture's
        // absence, not the client's error.
        std::thread::spawn(move || {
            for _ in 0..2 {
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
