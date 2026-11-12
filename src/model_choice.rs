//! Embedding-model shortlist for setup-time model choice.
//!
//! The candidates a first-run setup may offer, in presentation order,
//! plus the fail-closed writer that persists a pick as the initial
//! `embedders.json`. The setup surface slices a prefix of
//! [`candidates()`] at the shortlist breadth fixed outside this module
//! and derives each candidate's context class from `max_tokens`
//! (8,192 = full-context class; 512 = limited-context class).

/// One selectable embedding model in the setup shortlist.
pub struct ModelChoice {
    /// fastembed `EmbeddingModel` variant name; parses via
    /// `EmbeddingModel::from_str`.
    pub model_name: &'static str,
    /// Embedding width; matches the resolver's model info for
    /// `model_name`.
    pub dimensions: usize,
    /// Byte size of the model's ONNX file on HuggingFace, byte-exact as
    /// re-verified against the live `resolve` URLs when this table was
    /// written.
    pub file_bytes: u64,
    /// Lowercase-hex SHA-256 of the exact ONNX file fastembed loads for
    /// this model (its registry `model_file`), computed from the
    /// downloaded file after its byte count matched `file_bytes`. The
    /// writer emits it as the entry's `model_sha256` pin: a local entry
    /// without a pin fails closed at embedder load, so a pinless pick
    /// would brick the daemon's first start after the choice.
    pub model_sha256: &'static str,
    /// Maximum number of input tokens the model embeds before
    /// truncation.
    pub max_tokens: usize,
    /// Structural facts only — no benchmark figures.
    pub note: &'static str,
}

/// The shortlist, in presentation order: the ranked top three first,
/// then the non-quantized small model, then the base quantized model.
pub fn candidates() -> &'static [ModelChoice] {
    &[
        ModelChoice {
            model_name: "NomicEmbedTextV15Q",
            dimensions: 768,
            file_bytes: 137296292,
            model_sha256: "b4342336debaea79de872370664b0aaeb67dea4605513d00ee236ea871a81f27",
            max_tokens: 8192,
            note: "Same model as the standard default, int8-quantized. Dimensions and context unchanged.",
        },
        ModelChoice {
            model_name: "BGESmallENV15Q",
            dimensions: 384,
            file_bytes: 66465124,
            model_sha256: "51f1bd0addd6e859e42c2c8021a5e5461385bb676a649f4b269aa445449f2431",
            max_tokens: 512,
            note: "Smallest download. 384 dimensions; 512-token context. Already the low profile's model.",
        },
        ModelChoice {
            model_name: "SnowflakeArcticEmbedMQ",
            dimensions: 768,
            file_bytes: 110084023,
            model_sha256: "e46017484d369593d6bb830936fde2175ce21e1ba1ddf002f23e3212d87c824a",
            max_tokens: 512,
            note: "Keeps 768 dimensions; 512-token context.",
        },
        ModelChoice {
            model_name: "BGESmallENV15",
            dimensions: 384,
            file_bytes: 133093490,
            model_sha256: "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35",
            max_tokens: 512,
            note: "The non-quantized small model. 384 dimensions; 512-token context.",
        },
        ModelChoice {
            model_name: "BGEBaseENV15Q",
            dimensions: 768,
            file_bytes: 217824172,
            model_sha256: "4e556722bc4f65716c544c8a931f1e90fb3f866e5741fd93a96f051d673339c7",
            max_tokens: 512,
            note: "768 dimensions; least memory saved of this list; 512-token context.",
        },
    ]
}

use crate::embed::{default_acceptable_embedders, AcceptableEmbedder};
use anyhow::Context;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Persist a setup-time model choice as the initial `embedders.json` in
/// `config_dir`, returning the written path.
///
/// The file is a bare JSON array, choice first, followed by the built-in
/// default entries (minus the choice itself if it is among them) — so it is
/// never semantically equal to the built-in default, and the loader, which
/// only ever overwrites a file whose content is the default, leaves it in
/// place; with `NEUROSTRATA_MODEL` unset the first entry wins, which is what
/// makes the choice durable.
///
/// Fail-closed, in order: a database at `db_path` that has recorded data
/// refuses (a model was already chosen by use — see [`store_has_data`]; a
/// pristine store, whose file a bare daemon start creates before any data
/// exists, does not refuse); an existing `embedders.json` that parses to
/// anything other than the empty list or exactly the built-in default
/// refuses, byte-identical; an unparseable existing file refuses (the
/// writer never repairs a file it cannot classify, even though the loader
/// tolerates one by ignoring it). The replaceable existing files are the
/// exact built-in default a first daemon start materializes before any
/// data is recorded, and an empty list — which the loader itself treats
/// as the default and the offer boundary already qualifies; the writer
/// applies the same allowable-file rule as the offer boundary, so a state
/// the system offers the choice on can always accept the pick.
pub fn write_initial_embedders_file(
    config_dir: &Path,
    choice: &ModelChoice,
    db_path: &Path,
) -> anyhow::Result<PathBuf> {
    write_initial_embedders_file_inner(config_dir, choice, db_path, false)
}

/// `fail_before_rename` is the failure seam for the interruption test: when
/// set, the write aborts after the temp file is synced, before the rename.
fn write_initial_embedders_file_inner(
    config_dir: &Path,
    choice: &ModelChoice,
    db_path: &Path,
    fail_before_rename: bool,
) -> anyhow::Result<PathBuf> {
    if store_has_data(db_path) {
        anyhow::bail!(
            "refusing to write the initial embedders file: the database at {} has already recorded data (its embedding model was chosen by use)",
            db_path.display()
        );
    }

    let target = config_dir.join("embedders.json");
    if target.exists() {
        let content = fs::read_to_string(&target)
            .with_context(|| format!("failed to read existing {}", target.display()))?;
        let existing: Vec<AcceptableEmbedder> = serde_json::from_str(&content).with_context(|| {
            format!(
                "existing {} cannot be parsed; refusing to overwrite a file that cannot be classified",
                target.display()
            )
        })?;
        if !embedders_entries_qualify(&existing) {
            anyhow::bail!(
                "refusing to overwrite {}: its embedder list is neither empty nor the built-in default",
                target.display()
            );
        }
    }

    let mut entries: Vec<AcceptableEmbedder> = vec![AcceptableEmbedder {
        model_name: choice.model_name.to_string(),
        dimensions: choice.dimensions,
        base_url: None,
        api_key_env: None,
        api_model: None,
        // The candidate's pin: a local entry without one fails closed at
        // embedder load ("no pinned SHA-256"), which would brick the
        // first daemon start after the pick.
        model_sha256: Some(choice.model_sha256.to_string()),
    }];
    entries.extend(
        default_acceptable_embedders()
            .into_iter()
            .filter(|entry| !entry.model_name.eq_ignore_ascii_case(choice.model_name)),
    );

    let serialized = serde_json::to_string_pretty(&entries)?;
    fs::create_dir_all(config_dir)
        .with_context(|| format!("failed to create config directory {}", config_dir.display()))?;
    write_file_atomically(&target, &serialized, fail_before_rename)?;
    Ok(target)
}

/// Write `contents` to `path` atomically: serialize fully to a temp file in
/// the same directory, fsync it, rename over the target, fsync the
/// directory. A crash at any point leaves the target absent or holding its
/// complete previous content, never a truncation.
fn write_file_atomically(
    path: &Path,
    contents: &str,
    fail_before_rename: bool,
) -> anyhow::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        anyhow::anyhow!("embedders path {} has no parent directory", path.display())
    })?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "embedders.json".to_string());
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let staged = (|| -> anyhow::Result<()> {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("failed to create temp file {}", tmp.display()))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if fail_before_rename {
            anyhow::bail!("injected failure before rename (test seam)");
        }
        fs::rename(&tmp, path).with_context(|| {
            format!("failed to rename {} over {}", tmp.display(), path.display())
        })?;
        Ok(())
    })();
    if staged.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    staged?;
    // The directory fsync is what makes the rename itself durable; some
    // platforms cannot open a directory as a file, so it is best-effort.
    if let Ok(dir_file) = fs::File::open(dir) {
        let _ = dir_file.sync_all();
    }
    Ok(())
}

// --- The store-state sidecar and the pristine predicate ---
//
// The model-choice lock used to key on the database file's existence, but
// a bare daemon start creates the store (ladybug.db) before any user data
// exists, so existence alone misclassifies every daemon-first install as
// locked. The lock now keys on recorded data, answered by a store-authored
// sidecar: `ladybug.db.store-state.json` beside the db file, maintained by
// the store itself (see `LadybugStore`). This module owns the file format
// and the one predicate both the offer condition and the writer guard
// evaluate — pure filesystem reads, because the processes that evaluate
// it cannot open the (single-writer) store.

/// The sidecar format version written by this build.
pub(crate) const STORE_STATE_VERSION: u32 = 1;

/// The content of `ladybug.db.store-state.json`: whether the store has
/// recorded user data, and the embedding-space descriptor of the model
/// that created / last legitimately opened it while pristine.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreState {
    pub version: u32,
    pub has_data: bool,
    pub descriptor: crate::traits::EmbeddingDescriptor,
}

/// The sidecar's path: beside the database file, in the directory the
/// daemon lock already lives in.
pub(crate) fn store_state_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(".store-state.json");
    PathBuf::from(name)
}

/// How the store-state sidecar reads: a recorded state, no sidecar at
/// all, or a sidecar that is present but cannot be trusted. Absence and
/// unparseability must stay distinct — absence is legitimate (fresh and
/// pre-feature stores), while a present-but-unparseable sidecar means a
/// recorded identity exists but cannot be verified, so every open path
/// that consults recorded identity refuses on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StoreStateRead {
    Recorded(StoreState),
    Absent,
    Unparseable(String),
}

fn read_store_state_file(db_path: &Path) -> StoreStateRead {
    let path = store_state_path(db_path);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if path.exists() => {
            // A read failure on a file that exists (permissions, a
            // directory) is not absence: fail closed.
            return StoreStateRead::Unparseable(format!("it cannot be read: {e}"));
        }
        Err(_) => return StoreStateRead::Absent,
    };
    match serde_json::from_str::<StoreState>(&content) {
        Ok(state) if state.version != STORE_STATE_VERSION => StoreStateRead::Unparseable(format!(
            "unsupported store-state version {} (this build expects version {})",
            state.version, STORE_STATE_VERSION
        )),
        Ok(state) => StoreStateRead::Recorded(state),
        Err(e) => {
            StoreStateRead::Unparseable(format!("it cannot be parsed as store-state JSON: {e}"))
        }
    }
}

/// The store-state sidecar's classification: recorded, absent, or
/// present-but-unparseable.
pub(crate) fn read_store_state(db_path: &Path) -> StoreStateRead {
    read_store_state_file(db_path)
}

/// The one refusal for a present-but-unparseable sidecar, shared by
/// every open path that consults recorded identity so the wording
/// cannot drift between them. The refusal never writes: the file is
/// the operator's evidence and may be repairable.
pub(crate) fn store_state_unparseable_error(db_path: &Path, reason: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "refusing to open the store at {}: its store-state sidecar at {} is present but unparseable ({}); its recorded embedding identity cannot be verified. Repair or restore the sidecar, or start from a new store. The file was left untouched.",
        db_path.display(),
        store_state_path(db_path).display(),
        reason
    )
}

/// Write the sidecar atomically (the same temp + fsync + rename pattern
/// as the embedders file).
pub(crate) fn write_store_state(db_path: &Path, state: &StoreState) -> anyhow::Result<()> {
    let serialized = serde_json::to_string_pretty(state)?;
    write_file_atomically(&store_state_path(db_path), &serialized, false)
}

/// Whether the store at `db_path` has recorded user data — the one
/// definition of "locked" for the setup-time model choice. Fail-closed:
/// true on any doubt. A parseable sidecar's flag governs; an unparseable
/// sidecar means data; a database with no sidecar (created by a pre-fix
/// binary, or its sidecar lost) means data, because its state is
/// unknowable from outside; only a store with neither file has
/// certainly never recorded anything.
pub(crate) fn store_has_data(db_path: &Path) -> bool {
    match read_store_state_file(db_path) {
        StoreStateRead::Recorded(state) => state.has_data,
        StoreStateRead::Unparseable(_) => true,
        StoreStateRead::Absent => db_path.exists(),
    }
}

/// Whether the store at `db_path` may still receive a setup-time model
/// choice: exactly the negation of [`store_has_data`].
pub(crate) fn store_is_pristine(db_path: &Path) -> bool {
    !store_has_data(db_path)
}

/// The daemon's pre-flight identity check: a pure-filesystem
/// refusal, run before the daemon lock is taken
/// and before any embedder is built or the engine opens, so a refused
/// daemon start leaves the store byte-untouched (no lock truncation,
/// no WAL preservation copy, no model download for a doomed start).
///
/// Refuses only when the comparison is *commensurate*: the sidecar
/// records data, and the configured entry is a local fastembed entry
/// whose name parses — under those two conditions the descriptor
/// synthesized from the configuration is exactly what the daemon will
/// serve, so a mismatch with the recorded descriptor is exact. A
/// remote entry's serving descriptor cannot be synthesized from
/// config, and an unparseable name legitimately falls back to a
/// different model at load, so both pass here; the store-level check
/// keyed on the true serving descriptor still refuses before
/// any vector lands. An unresolvable configuration also passes: the
/// embedder build that follows fails on its own with the loader's
/// error, and this pre-flight must not invent a second failure mode
/// for broken configs.
pub(crate) fn check_daemon_open_identity(db_path: &Path, config_dir: &Path) -> anyhow::Result<()> {
    use std::str::FromStr;

    // An unparseable sidecar refuses first, before the has_data early
    // return and before config resolution: its recorded identity
    // cannot be verified, so no configuration — commensurate or not —
    // may open this store.
    let state = match read_store_state(db_path) {
        StoreStateRead::Recorded(state) => state,
        StoreStateRead::Absent => return Ok(()),
        StoreStateRead::Unparseable(reason) => {
            return Err(store_state_unparseable_error(db_path, &reason));
        }
    };
    if !state.has_data {
        return Ok(());
    }
    let Ok(entry) = crate::embed::configured_model_in(config_dir) else {
        return Ok(());
    };
    if entry.base_url.is_some() || entry.api_key_env.is_some() {
        return Ok(());
    }
    if fastembed::EmbeddingModel::from_str(&entry.model_name).is_err() {
        return Ok(());
    }
    // The daemon synthesizes its serving descriptor from the entry it
    // resolved, in `configured_embedding_descriptor`'s shape with the
    // entry's own width (the width the daemon opens the store with).
    let configured = crate::traits::EmbeddingDescriptor {
        provider: "fastembed".into(),
        model: entry.model_name.clone(),
        revision: "onnx".into(),
        dims: entry.dimensions,
        normalization: "unknown".into(),
        preprocessing: "unknown".into(),
        distance_metric: "cosine".into(),
    };
    if configured != state.descriptor {
        return Err(crate::store::ladybug::EmbeddingIdentityRefusal::between(
            &state.descriptor,
            &configured,
        )
        .into());
    }
    Ok(())
}

// --- Deferred setup-time choice ---
//
// Materialization runs inside daemon/serving processes whose stdin is not
// a terminal and whose stdout is a log sink or the MCP wire, so it never
// prompts: it only records a pending choice. The next interactive command
// surfaces it. The offer itself is a pure rendering of the candidate
// slice plus the measured coverage, so it is testable without a terminal.

/// The pending-choice marker file, written beside `embedders.json` in the
/// config directory at materialization time.
const PENDING_CHOICE_MARKER: &str = "model-choice-pending.json";

fn pending_choice_path(config_dir: &Path) -> PathBuf {
    config_dir.join(PENDING_CHOICE_MARKER)
}

/// Record a pending model choice in `config_dir`, unless one is already
/// recorded. Called from the embedder loader's materialization branch —
/// the one place the built-in default file is written — so a declined
/// choice (marker removed, default file now in place) is never recorded
/// again by later loads. Best-effort: a failed marker write must not fail
/// the load it rides on.
pub(crate) fn record_pending_choice(config_dir: &Path) {
    let marker = pending_choice_path(config_dir);
    if marker.exists() {
        return;
    }
    let _ = fs::write(&marker, "{\"pending\": \"embedding-model-choice\"}\n");
}

fn clear_pending_choice(config_dir: &Path) {
    let _ = fs::remove_file(pending_choice_path(config_dir));
}

/// The one allowable-file rule for an existing `embedders.json`, shared
/// by the offer boundary and the choice writer: a parsed embedder list
/// qualifies when it is empty or exactly the built-in default.
fn embedders_entries_qualify(entries: &[AcceptableEmbedder]) -> bool {
    entries.is_empty()
        || serde_json::to_value(entries).ok()
            == serde_json::to_value(default_acceptable_embedders()).ok()
}

/// Whether the `embedders.json` state in `config_dir` still qualifies for
/// a first-run offer: the file is absent, an empty list, or exactly the
/// built-in default. An unparseable file is operator-present — it never
/// qualifies and is never modified.
fn embedders_file_qualifies_for_offer(config_dir: &Path) -> bool {
    let path = config_dir.join("embedders.json");
    if !path.exists() {
        return true;
    }
    let Ok(content) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(entries) = serde_json::from_str::<Vec<AcceptableEmbedder>>(&content) else {
        return false;
    };
    embedders_entries_qualify(&entries)
}

/// The offer condition: the store has recorded no data yet (the same
/// predicate the writer guard uses, so an offer and a pick can never
/// disagree about locked-ness), and the embedders file state qualifies.
/// Re-verified at surfacing time — a store that recorded data since the
/// marker was recorded voids the pending choice.
fn offer_condition_holds(config_dir: &Path, db_path: &Path) -> bool {
    store_is_pristine(db_path) && embedders_file_qualifies_for_offer(config_dir)
}

const PERMANENCE_WARNING: &str = "This choice is permanent for this database: the store refuses any other model once data exists. Changing later requires a new database.";
const NOT_MEASURED_MEMORY_LINE: &str = "Not measured — resident-memory delta unverified.";
const NOT_MEASURED_QUALITY_LINE: &str = "Not measured — quality delta unverified.";
/// Measured on a live-store corpus: 6.08% (18/296) of items exceed 512
/// tokens; the plain-language line rounds that to ~6%.
const TRUNCATION_PCT_OVER_512: u32 = 6;

/// The resident-memory line for one candidate: the measured figure
/// where the candidate was measured end-to-end, the not-measured line
/// otherwise. A candidate never displays another candidate's numbers.
fn memory_line(choice: &ModelChoice) -> String {
    match choice.model_name {
        // Measured: Q after-batch runs 330/332 MiB; FP32 947/938 MiB;
        // reported saving 606 MiB.
        "NomicEmbedTextV15Q" => "Measured: resident memory after a batch was 330/332 MiB, a measured saving of 606 MiB against the FP32 default's 947/938 MiB.".to_string(),
        _ => NOT_MEASURED_MEMORY_LINE.to_string(),
    }
}

/// The quality line for one candidate, same coverage rule as
/// [`memory_line`].
fn quality_line(choice: &ModelChoice) -> String {
    match choice.model_name {
        // Measured: top-5 overlap 0.873333, mean cosine 0.948251;
        // blind recall 23/30 against FP32's 22/30.
        "NomicEmbedTextV15Q" => "Measured: top-5 overlap with the FP32 default was 0.873 (mean cosine 0.948); blind recall was 23/30 against the FP32 default's 22/30.".to_string(),
        // Measured: top-5 overlap 0.666667; no resident-memory measurement.
        "BGESmallENV15Q" => {
            "Measured: top-5 overlap with the FP32 default was 0.667."
                .to_string()
        }
        _ => NOT_MEASURED_QUALITY_LINE.to_string(),
    }
}

/// Render the offer for `offered` (the top-three candidate slice of
/// [`candidates()`]). Pure: the same slice always renders the same text.
pub(crate) fn render_offer(offered: &[ModelChoice]) -> String {
    let mut out = String::new();
    out.push_str(
        "This install has not stored any data yet, so its embedding model can still be chosen.\n",
    );
    for (index, choice) in offered.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n", index + 1, choice.model_name));
        out.push_str(&format!(
            "   File size: {:.1} MiB\n",
            choice.file_bytes as f64 / 1_048_576.0
        ));
        out.push_str(&format!("   {}\n", choice.note));
        out.push_str(&format!("   Memory: {}\n", memory_line(choice)));
        out.push_str(&format!("   Quality: {}\n", quality_line(choice)));
        out.push_str(&format!("   {PERMANENCE_WARNING}\n"));
        if choice.max_tokens <= 512 {
            out.push_str(&format!(
                "   This model's context is 512 tokens. About {TRUNCATION_PCT_OVER_512}% of the items in a measured reference store exceed that and would be embedded truncated — silently, and permanently for those items.\n"
            ));
        }
    }
    out
}

/// The user's answer to a surfaced model-choice offer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChoiceResponse {
    /// Pick the offered candidate at this index. An out-of-range index is
    /// not a decline — it is treated as [`ChoiceResponse::Invalid`].
    Pick(usize),
    /// Explicitly decline (the documented decline input). Final: the
    /// marker is removed and the install is not asked again.
    Decline,
    /// Anything that is neither a pick nor an explicit decline —
    /// unparseable or empty input at the terminal. Not a decline: the
    /// marker stays in place and nothing is written, so the next
    /// interactive command offers the choice again.
    Invalid,
}

/// What a surfacing attempt did with a pending choice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PendingChoiceOutcome {
    /// No marker: nothing was pending.
    NoMarker,
    /// A marker exists but this invocation is not interactive; the marker
    /// is left for a later interactive command.
    NotInteractive,
    /// The offer condition no longer holds (the store has recorded data,
    /// or the file state is locked); the marker is removed, nothing is
    /// offered.
    Voided,
    /// The user declined; the marker is removed and the install is not
    /// asked again.
    Declined,
    /// The answer was invalid (unparseable, empty, or an out-of-range
    /// pick); the marker is left in place, nothing is written, and the
    /// choice is offered again at the next interactive command.
    InvalidInput,
    /// The user picked a model; it was persisted via
    /// [`write_initial_embedders_file`] and the marker removed.
    Chosen(&'static str),
}

/// Surface a pending model choice at an interactive command, before the
/// command executes (and therefore before any store creation on its
/// path). `responder` receives the rendered offer and answers with a
/// [`ChoiceResponse`]: only an explicit decline or a valid pick consumes
/// the marker; invalid input leaves it in place. A failed persist
/// propagates and leaves the marker in place, so the choice is not lost.
pub(crate) fn surface_pending_choice(
    config_dir: &Path,
    db_path: &Path,
    stdin_is_terminal: bool,
    responder: &mut dyn FnMut(&str) -> ChoiceResponse,
) -> anyhow::Result<PendingChoiceOutcome> {
    if !pending_choice_path(config_dir).exists() {
        return Ok(PendingChoiceOutcome::NoMarker);
    }
    if !stdin_is_terminal {
        return Ok(PendingChoiceOutcome::NotInteractive);
    }
    if !offer_condition_holds(config_dir, db_path) {
        clear_pending_choice(config_dir);
        return Ok(PendingChoiceOutcome::Voided);
    }
    let offered = &candidates()[..3];
    let rendered = render_offer(offered);
    match responder(&rendered) {
        ChoiceResponse::Pick(index) if index < offered.len() => {
            let choice = &offered[index];
            write_initial_embedders_file(config_dir, choice, db_path)?;
            clear_pending_choice(config_dir);
            Ok(PendingChoiceOutcome::Chosen(choice.model_name))
        }
        ChoiceResponse::Decline => {
            clear_pending_choice(config_dir);
            Ok(PendingChoiceOutcome::Declined)
        }
        ChoiceResponse::Pick(_) | ChoiceResponse::Invalid => Ok(PendingChoiceOutcome::InvalidInput),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{
        configured_model_in, default_acceptable_embedders, get_acceptable_embedders_in,
        AcceptableEmbedder,
    };
    use fastembed::{EmbeddingModel, TextEmbedding};
    use std::fs;
    use std::path::PathBuf;
    use std::str::FromStr;

    #[test]
    fn candidates_are_complete_in_presentation_order() {
        let names: Vec<&str> = candidates().iter().map(|c| c.model_name).collect();
        assert_eq!(
            names,
            vec![
                "NomicEmbedTextV15Q",
                "BGESmallENV15Q",
                "SnowflakeArcticEmbedMQ",
                "BGESmallENV15",
                "BGEBaseENV15Q",
            ]
        );
    }

    #[test]
    fn candidate_dimensions_match_the_resolver() {
        for c in candidates() {
            let parsed = EmbeddingModel::from_str(c.model_name)
                .unwrap_or_else(|_| panic!("{} does not parse as an EmbeddingModel", c.model_name));
            let info = TextEmbedding::get_model_info(&parsed)
                .unwrap_or_else(|_| panic!("no model info for {}", c.model_name));
            assert_eq!(
                info.dim, c.dimensions,
                "dimension drift for {}",
                c.model_name
            );
        }
    }

    #[test]
    fn candidate_file_bytes_are_the_validated_values() {
        let expected: [(&str, u64); 5] = [
            ("NomicEmbedTextV15Q", 137296292),
            ("BGESmallENV15Q", 66465124),
            ("SnowflakeArcticEmbedMQ", 110084023),
            ("BGESmallENV15", 133093490),
            ("BGEBaseENV15Q", 217824172),
        ];
        assert_eq!(candidates().len(), expected.len());
        for (c, (name, bytes)) in candidates().iter().zip(expected.iter()) {
            assert_eq!(c.model_name, *name);
            assert_eq!(c.file_bytes, *bytes, "file_bytes drift for {}", name);
        }
    }

    #[test]
    fn candidate_pins_are_well_formed() {
        // The built-in FP32 default's pin (embed.rs): no candidate may
        // carry it — each candidate's pin is the hash of the exact file
        // fastembed loads for that candidate, and all five files differ.
        const FP32_DEFAULT_PIN: &str =
            "147d5aa88c2101237358e17796cf3a227cead1ec304ec34b465bb08e9d952965";
        let mut seen = std::collections::HashSet::new();
        for c in candidates() {
            assert_eq!(c.model_sha256.len(), 64, "pin length for {}", c.model_name);
            assert!(
                c.model_sha256
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
                "pin for {} must be lowercase hex",
                c.model_name
            );
            assert_ne!(
                c.model_sha256, FP32_DEFAULT_PIN,
                "{} carries the FP32 default's pin",
                c.model_name
            );
            assert!(
                seen.insert(c.model_sha256),
                "duplicate pin at {}",
                c.model_name
            );
        }
    }

    #[test]
    fn writer_emits_the_candidates_pin() {
        // Every pick the writer writes must carry that candidate's pin:
        // FastEmbedder::new fails closed on a missing pin, so a `None`
        // here bricks the daemon's first start after the pick.
        for choice in candidates() {
            let (tmp, config_dir) = fresh_config_dir("writer-pin");
            let db_path = tmp.path().join("neurostrata.db");
            write_initial_embedders_file(&config_dir, choice, &db_path).expect("write choice file");
            let content =
                fs::read_to_string(config_dir.join("embedders.json")).expect("read written file");
            let parsed: Vec<AcceptableEmbedder> =
                serde_json::from_str(&content).expect("written file parses");
            assert_eq!(
                parsed[0].model_sha256.as_deref(),
                Some(choice.model_sha256),
                "the written pick for {} must carry its pin",
                choice.model_name
            );
            let entries = get_acceptable_embedders_in(&config_dir).expect("loader round-trip");
            assert_eq!(
                entries[0].model_sha256.as_deref(),
                Some(choice.model_sha256),
                "the loader must preserve the pin for {}",
                choice.model_name
            );
        }
    }

    // --- The fail-closed initial-file writer ---

    fn bge_small_q() -> &'static ModelChoice {
        candidates()
            .iter()
            .find(|c| c.model_name == "BGESmallENV15Q")
            .expect("shortlist contains BGESmallENV15Q")
    }

    /// A fresh temp config dir; the returned TempDir guards its lifetime.
    fn fresh_config_dir(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let config_dir = tmp.path().join(format!("config-{tag}"));
        fs::create_dir_all(&config_dir).expect("create config dir");
        (tmp, config_dir)
    }

    #[test]
    fn setup_choice_is_not_the_default_list() {
        let (tmp, config_dir) = fresh_config_dir("not-default");
        let db_path = tmp.path().join("neurostrata.db");
        let written = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("write choice file");
        let content = fs::read_to_string(&written).expect("read written file");
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("written file is a JSON array");
        let default_value =
            serde_json::to_value(default_acceptable_embedders()).expect("serialize default list");
        assert_ne!(
            parsed, default_value,
            "a setup choice file must not be semantically equal to the built-in default"
        );
        assert_eq!(parsed[0]["model_name"], "BGESmallENV15Q");
    }

    #[test]
    fn setup_choice_survives_loader_round_trip() {
        let (tmp, config_dir) = fresh_config_dir("round-trip");
        let db_path = tmp.path().join("neurostrata.db");
        let written = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("write choice file");
        let on_disk: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&written).expect("read written file"))
                .expect("written file is a JSON array");
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the file");
        assert_eq!(
            serde_json::to_value(&entries).expect("serialize loader entries"),
            on_disk,
            "the loader must return the written entries unchanged"
        );
        assert_eq!(entries[0].model_name, "BGESmallENV15Q");
        assert_eq!(entries[0].dimensions, 384);
    }

    #[test]
    fn writer_env_beats_file_and_unacceptable_env_falls_back() {
        let (tmp, config_dir) = fresh_config_dir("env");
        let db_path = tmp.path().join("neurostrata.db");
        write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("write choice file");

        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        // An env-named model that is in the file wins over the first entry.
        std::env::set_var("NEUROSTRATA_MODEL", "NomicEmbedTextV15");
        let selected = configured_model_in(&config_dir).expect("resolve env-named model");
        assert_eq!(selected.model_name, "NomicEmbedTextV15");
        // An env-named model that is not acceptable silently falls back to
        // the file's first entry — upstream resolution never errors here.
        std::env::set_var("NEUROSTRATA_MODEL", "NoSuchAcceptableModel");
        let fallback = configured_model_in(&config_dir).expect("resolve fallback");
        assert_eq!(fallback.model_name, "BGESmallENV15Q");
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    // --- The lock keys on recorded store data, not on the
    // db file's existence. A bare daemon start creates ladybug.db before
    // any user data exists, so existence alone must never refuse. The
    // store-authored sidecar (ladybug.db.store-state.json) is the record:
    // these helpers write it as raw JSON, pinning the on-disk contract.

    fn sidecar_path_for(db_path: &Path) -> PathBuf {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(".store-state.json");
        PathBuf::from(name)
    }

    fn write_sidecar(db_path: &Path, has_data: bool) {
        let content = format!(
            "{{\"version\": 1, \"has_data\": {has_data}, \"descriptor\": {{\"provider\": \"fastembed\", \"model\": \"NomicEmbedTextV15\", \"revision\": \"onnx\", \"dims\": 768, \"normalization\": \"unknown\", \"preprocessing\": \"unknown\", \"distance_metric\": \"cosine\"}}}}"
        );
        fs::write(sidecar_path_for(db_path), content).expect("write sidecar");
    }

    /// A db file the way a bare daemon start leaves it: present, with a
    /// 4096-byte first page, and no user data behind it.
    fn write_bare_db(db_path: &Path) {
        fs::write(db_path, vec![0u8; 4096]).expect("create bare db file");
    }

    #[test]
    fn writer_succeeds_when_db_exists_but_store_is_pristine() {
        let (tmp, config_dir) = fresh_config_dir("writer-pristine-db");
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        write_sidecar(&db_path, false);
        write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("a pristine store (db file present, no recorded data) must not refuse");
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the pick");
        assert_eq!(entries[0].model_name, "BGESmallENV15Q");
    }

    #[test]
    fn writer_refuses_when_store_has_data() {
        let (tmp, config_dir) = fresh_config_dir("writer-data-db");
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        write_sidecar(&db_path, true);
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(
            result.is_err(),
            "a store that has recorded data must refuse the write"
        );
        assert!(
            !config_dir.join("embedders.json").exists(),
            "no embedders.json may be created when the write is refused"
        );
    }

    #[test]
    fn writer_and_offer_fail_closed_without_sidecar() {
        // A db file with no sidecar was created by a pre-fix binary (or
        // lost its sidecar): its data state is unknowable from outside,
        // so it is locked.
        let (tmp, config_dir) = fresh_config_dir("fail-closed-no-sidecar");
        write_default_file(&config_dir);
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(result.is_err(), "db present + sidecar absent must refuse");
        let called = std::cell::Cell::new(false);
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            called.set(true);
            ChoiceResponse::Pick(0)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Voided);
        assert!(!called.get(), "a fail-closed store is never offered");
    }

    #[test]
    fn writer_and_offer_fail_closed_with_unparseable_sidecar() {
        let (tmp, config_dir) = fresh_config_dir("fail-closed-bad-sidecar");
        write_default_file(&config_dir);
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        fs::write(sidecar_path_for(&db_path), "{not json").expect("write unparseable sidecar");
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(result.is_err(), "an unparseable sidecar must refuse");
        let called = std::cell::Cell::new(false);
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            called.set(true);
            ChoiceResponse::Pick(0)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Voided);
        assert!(!called.get(), "an unparseable sidecar is never offered");
    }

    #[test]
    fn writer_refuses_when_sidecar_reports_data_without_db() {
        // Inconsistent state (sidecar says data, db file gone): the
        // sidecar's word governs — fail closed.
        let (tmp, config_dir) = fresh_config_dir("sidecar-data-no-db");
        let db_path = tmp.path().join("ladybug.db");
        write_sidecar(&db_path, true);
        assert!(!db_path.exists());
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(
            result.is_err(),
            "a sidecar reporting data must refuse even when the db file is absent"
        );
    }

    #[test]
    fn store_predicate_fresh_install_has_no_data() {
        let (tmp, config_dir) = fresh_config_dir("predicate-fresh");
        let db_path = tmp.path().join("ladybug.db");
        assert!(!store_has_data(&db_path), "nothing on disk: no data");
        assert!(store_is_pristine(&db_path));
        // A pristine sidecar alone (db not yet created) is still pristine.
        write_sidecar(&db_path, false);
        assert!(!store_has_data(&db_path));
        assert!(store_is_pristine(&db_path));
        // And the writer agrees: a fresh install accepts the pick.
        write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("fresh install writer succeeds");
    }

    #[test]
    fn write_initial_embedders_file_refuses_non_default_file() {
        let (tmp, config_dir) = fresh_config_dir("non-default-file");
        let target = config_dir.join("embedders.json");
        fs::write(
            &target,
            "[{\"model_name\":\"SnowflakeArcticEmbedMQ\",\"dimensions\":768}]\n",
        )
        .expect("pre-write hand-authored file");
        let before = fs::read(&target).expect("read pre-written file");
        let db_path = tmp.path().join("neurostrata.db");
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(
            result.is_err(),
            "a non-default operator file must refuse the write"
        );
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "a refused write must leave the file byte-identical"
        );
    }

    #[test]
    fn empty_embedder_list_is_replaceable_by_the_choice_writer() {
        // The offer boundary qualifies an empty embedder list (the
        // loader treats it as the built-in default), so the writer must
        // accept the same state — otherwise the pick made at the offered
        // prompt could never be written.
        let (tmp, config_dir) = fresh_config_dir("empty-list-writer");
        let target = config_dir.join("embedders.json");
        fs::write(&target, "[]\n").expect("pre-write empty-list file");
        let db_path = tmp.path().join("neurostrata.db");
        write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("an empty embedder list is replaceable by the choice writer");
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the file");
        assert_eq!(entries[0].model_name, "BGESmallENV15Q");
        assert_eq!(entries[0].dimensions, 384);
    }

    #[test]
    fn empty_embedder_list_pick_surfaces_and_clears_the_marker() {
        // The surfacing half: with an empty embedder list in place, a
        // pending choice surfaces, the pick persists, and the marker
        // clears.
        let (tmp, config_dir) = fresh_config_dir("empty-list-surface");
        fs::write(config_dir.join("embedders.json"), "[]\n").expect("pre-write empty-list file");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_offer| {
            ChoiceResponse::Pick(0)
        })
        .expect("the pick surfaces without a writer refusal");
        let picked = candidates()[0].model_name;
        assert_eq!(outcome, PendingChoiceOutcome::Chosen(picked));
        assert!(
            !pending_choice_path(&config_dir).exists(),
            "a persisted pick clears the pending marker"
        );
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the file");
        assert_eq!(entries[0].model_name, picked);
    }

    #[test]
    fn offer_and_writer_agree_on_embedders_file_states() {
        // One shared rule decides which existing embedders files may be
        // replaced: absent, empty, or exactly the built-in default
        // qualify; a non-default list and an unparseable file do not.
        // For every state, a file the offer qualifies must accept the
        // pick, and a file it does not qualify must refuse it.
        let default_json =
            serde_json::to_string_pretty(&default_acceptable_embedders()).expect("serialize");
        let cases: [(&str, Option<String>, bool); 5] = [
            ("absent", None, true),
            ("empty", Some("[]\n".to_string()), true),
            ("exact-default", Some(default_json), true),
            (
                "non-default",
                Some(
                    "[{\"model_name\":\"SnowflakeArcticEmbedMQ\",\"dimensions\":768}]\n"
                        .to_string(),
                ),
                false,
            ),
            ("unparseable", Some("{not json".to_string()), false),
        ];
        for (tag, contents, expected) in cases {
            let (tmp, config_dir) = fresh_config_dir(&format!("agree-{tag}"));
            if let Some(contents) = contents {
                fs::write(config_dir.join("embedders.json"), contents)
                    .expect("pre-write embedders file");
            }
            let offered = embedders_file_qualifies_for_offer(&config_dir);
            assert_eq!(
                offered, expected,
                "offer qualification is wrong for the {tag} embedders file state"
            );
            let db_path = tmp.path().join("neurostrata.db");
            let writer_ok =
                write_initial_embedders_file(&config_dir, bge_small_q(), &db_path).is_ok();
            assert_eq!(
                writer_ok, expected,
                "writer acceptance is wrong for the {tag} embedders file state"
            );
            assert_eq!(
                offered, writer_ok,
                "offer and writer disagree on the {tag} embedders file state"
            );
        }
    }

    #[test]
    fn writer_replaces_exact_default_when_no_db() {
        let (tmp, config_dir) = fresh_config_dir("exact-default");
        let target = config_dir.join("embedders.json");
        let default_json =
            serde_json::to_string_pretty(&default_acceptable_embedders()).expect("serialize");
        fs::write(&target, &default_json).expect("pre-write default file");
        let db_path = tmp.path().join("neurostrata.db");
        write_initial_embedders_file(&config_dir, bge_small_q(), &db_path)
            .expect("an exact-default file with no database is replaceable");
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the file");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].model_name, "BGESmallENV15Q");
        assert_eq!(entries[0].dimensions, 384);
        assert_eq!(entries[1].model_name, "NomicEmbedTextV15");
    }

    fn assert_no_temp_litter(dir: &std::path::Path) {
        for entry in fs::read_dir(dir).expect("list config dir") {
            let name = entry.expect("dir entry").file_name();
            assert!(
                !name.to_string_lossy().contains("tmp"),
                "temp file left behind: {name:?}"
            );
        }
    }

    #[test]
    fn writer_leaves_no_partial_file_on_interruption() {
        // Fresh directory: an interrupted write leaves no target at all.
        let (tmp, config_dir) = fresh_config_dir("interrupt-fresh");
        let db_path = tmp.path().join("neurostrata.db");
        let result = write_initial_embedders_file_inner(&config_dir, bge_small_q(), &db_path, true);
        assert!(result.is_err(), "the injected failure must surface");
        assert!(
            !config_dir.join("embedders.json").exists(),
            "an interrupted write must not leave a partial target"
        );
        assert_no_temp_litter(&config_dir);

        // Existing default file: an interrupted replace leaves the complete
        // previous content, never a truncation.
        let (tmp, config_dir) = fresh_config_dir("interrupt-replace");
        let target = config_dir.join("embedders.json");
        let default_json =
            serde_json::to_string_pretty(&default_acceptable_embedders()).expect("serialize");
        fs::write(&target, &default_json).expect("pre-write default file");
        let before = fs::read(&target).expect("read pre-written file");
        let db_path = tmp.path().join("neurostrata.db");
        let result = write_initial_embedders_file_inner(&config_dir, bge_small_q(), &db_path, true);
        assert!(result.is_err(), "the injected failure must surface");
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "an interrupted replace must keep the complete previous content"
        );
        assert_no_temp_litter(&config_dir);
    }

    #[test]
    fn acceptable_embedder_shape_matches_loader_type() {
        // Guards the test module's use of the loader's entry type: a minimal
        // hand-authored entry parses with the optional fields defaulted.
        let parsed: Vec<AcceptableEmbedder> =
            serde_json::from_str("[{\"model_name\":\"X\",\"dimensions\":1}]").expect("parse");
        assert_eq!(parsed[0].model_name, "X");
        assert!(parsed[0].model_sha256.is_none());
    }

    // --- Deferred setup-time choice ---

    fn write_default_file(config_dir: &Path) {
        let default_json =
            serde_json::to_string_pretty(&default_acceptable_embedders()).expect("serialize");
        fs::write(config_dir.join("embedders.json"), &default_json).expect("write default file");
    }

    #[test]
    fn setup_offers_choice_when_db_exists_but_store_is_pristine() {
        // Regression at unit level: a bare daemon start creates
        // ladybug.db before any user data exists, and the
        // pending choice recorded at that start must still surface and
        // persist at the next interactive command.
        let (tmp, config_dir) = fresh_config_dir("a4fix-pristine-db");
        write_default_file(&config_dir);
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        write_sidecar(&db_path, false);
        let mut seen_offer = String::new();
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |offer| {
            seen_offer = offer.to_string();
            ChoiceResponse::Pick(1)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Chosen("BGESmallENV15Q"));
        assert!(seen_offer.contains("BGESmallENV15Q"));
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the pick");
        assert_eq!(entries[0].model_name, "BGESmallENV15Q");
        assert!(
            !pending_choice_path(&config_dir).exists(),
            "a completed pick clears the marker"
        );
    }

    #[test]
    fn setup_never_offers_choice_when_store_has_data() {
        let (tmp, config_dir) = fresh_config_dir("a4fix-data-db");
        write_default_file(&config_dir);
        let target = config_dir.join("embedders.json");
        let before = fs::read(&target).expect("read default file");
        record_pending_choice(&config_dir);
        assert!(pending_choice_path(&config_dir).exists());
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        write_sidecar(&db_path, true);
        let called = std::cell::Cell::new(false);
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            called.set(true);
            ChoiceResponse::Pick(0)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Voided);
        assert!(
            !called.get(),
            "no offer may be presented once the store has recorded data"
        );
        assert!(
            !pending_choice_path(&config_dir).exists(),
            "a voided pending choice removes its marker"
        );
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "a voided choice leaves the embedders file untouched"
        );
    }

    #[test]
    fn setup_offers_choice_when_only_default_file_exists() {
        let (tmp, config_dir) = fresh_config_dir("a4-default-only");
        write_default_file(&config_dir);
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        let mut seen_offer = String::new();
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |offer| {
            seen_offer = offer.to_string();
            ChoiceResponse::Pick(1)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Chosen("BGESmallENV15Q"));
        assert!(seen_offer.contains("BGESmallENV15Q"));
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the pick");
        assert_eq!(entries[0].model_name, "BGESmallENV15Q");
        assert!(
            !pending_choice_path(&config_dir).exists(),
            "a completed pick clears the marker"
        );
    }

    #[test]
    fn setup_never_offers_choice_when_file_unparseable() {
        let (tmp, config_dir) = fresh_config_dir("a4-unparseable");
        let target = config_dir.join("embedders.json");
        fs::write(&target, "{not json").expect("write unparseable file");
        let before = fs::read(&target).expect("read file");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        let called = std::cell::Cell::new(false);
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            called.set(true);
            ChoiceResponse::Pick(0)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Voided);
        assert!(
            !called.get(),
            "an unparseable file is operator-present: never offered"
        );
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "an unparseable file is never modified"
        );
        assert!(!pending_choice_path(&config_dir).exists());
    }

    #[test]
    fn setup_rerun_preserves_chosen_model() {
        let (tmp, config_dir) = fresh_config_dir("a4-rerun");
        write_default_file(&config_dir);
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Pick(2)
        })
        .expect("first surface");
        assert_eq!(
            outcome,
            PendingChoiceOutcome::Chosen("SnowflakeArcticEmbedMQ")
        );
        let target = config_dir.join("embedders.json");
        let after_pick = fs::read(&target).expect("read picked file");
        // A later interactive command finds no marker and changes nothing;
        // the chosen file is non-default, so the install is locked.
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Pick(0)
        })
        .expect("rerun surface");
        assert_eq!(outcome, PendingChoiceOutcome::NoMarker);
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            after_pick,
            "a rerun must preserve the chosen model file byte-identically"
        );
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the pick");
        assert_eq!(entries[0].model_name, "SnowflakeArcticEmbedMQ");
    }

    #[test]
    fn setup_failure_leaves_no_partial_config() {
        let (tmp, config_dir) = fresh_config_dir("a4-failure");
        write_default_file(&config_dir);
        let target = config_dir.join("embedders.json");
        let before = fs::read(&target).expect("read default file");
        record_pending_choice(&config_dir);
        // Sabotage the writer's temp path (the writer's documented temp name) so
        // the persist fails after staging begins.
        let tmp_blocker = config_dir.join(format!(".embedders.json.tmp-{}", std::process::id()));
        fs::create_dir(&tmp_blocker).expect("create temp blocker dir");
        let db_path = tmp.path().join("neurostrata.db");
        let result = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Pick(0)
        });
        assert!(result.is_err(), "a failed persist must surface as an error");
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "a failed persist leaves the complete previous file, never a partial one"
        );
        assert!(
            pending_choice_path(&config_dir).exists(),
            "a failed persist leaves the marker in place, so the choice is not lost"
        );
    }

    #[test]
    fn setup_presents_not_measured_lines_for_unmeasured_candidates() {
        let top3 = &candidates()[..3];
        let arctic = render_offer(&top3[2..3]);
        assert!(
            arctic.contains("Not measured — resident-memory delta unverified."),
            "an unmeasured candidate carries the memory line verbatim"
        );
        assert!(
            arctic.contains("Not measured — quality delta unverified."),
            "an unmeasured candidate carries the quality line verbatim"
        );
        let bge = render_offer(&top3[1..2]);
        assert!(bge.contains("Not measured — resident-memory delta unverified."));
        assert!(
            !bge.contains("Not measured — quality delta unverified."),
            "BGESmallENV15Q has a measured quality line, so it must not carry the quality disclaimer"
        );
        assert!(
            bge.contains("0.667"),
            "BGESmallENV15Q shows its measured overlap"
        );
        assert!(
            bge.contains(
                "Measured: top-5 overlap with the FP32 default was 0.667."
            ),
            "BGESmallENV15Q shows its measured quality line verbatim"
        );
        let nomic = render_offer(&top3[0..1]);
        assert!(
            !nomic.contains("Not measured —"),
            "the end-to-end measured candidate carries neither disclaimer"
        );
        assert!(
            nomic.contains("606"),
            "NomicEmbedTextV15Q shows its measured saving"
        );
        assert!(
            nomic.contains("0.873"),
            "NomicEmbedTextV15Q shows its measured overlap"
        );
        assert!(
            nomic.contains(
                "Measured: resident memory after a batch was 330/332 MiB, a measured saving of 606 MiB against the FP32 default's 947/938 MiB."
            ),
            "NomicEmbedTextV15Q shows its measured memory line verbatim"
        );
        assert!(
            nomic.contains(
                "Measured: top-5 overlap with the FP32 default was 0.873 (mean cosine 0.948); blind recall was 23/30 against the FP32 default's 22/30."
            ),
            "NomicEmbedTextV15Q shows its measured quality line verbatim"
        );
        // Warning copy, split by context class, with the measured figure.
        assert!(bge.contains("This model's context is 512 tokens. About 6% of the items"));
        assert!(arctic.contains("This model's context is 512 tokens. About 6% of the items"));
        assert!(
            !nomic.contains("This model's context is 512 tokens"),
            "a full-context candidate shows no truncation warning"
        );
        for candidate in top3 {
            assert!(
                render_offer(std::slice::from_ref(candidate)).contains(
                    "This choice is permanent for this database: the store refuses any other model once data exists. Changing later requires a new database."
                ),
                "every candidate shows the permanence warning verbatim"
            );
        }
    }

    #[test]
    fn materialization_without_terminal_records_pending_choice() {
        let (_tmp, config_dir) = fresh_config_dir("a4-materialize");
        // Drive the loader the way any daemon/serving path does: no terminal,
        // no responder exists on this path, and no prompt can be attempted.
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader materializes");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model_name, "NomicEmbedTextV15");
        let on_disk: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(config_dir.join("embedders.json")).expect("read"),
        )
        .expect("written file is a JSON array");
        assert_eq!(
            on_disk,
            serde_json::to_value(default_acceptable_embedders()).expect("serialize default"),
            "materialization writes today's default file exactly; no choice is written"
        );
        assert!(
            pending_choice_path(&config_dir).exists(),
            "materialization records the pending choice for the next interactive command"
        );
        let _ = get_acceptable_embedders_in(&config_dir).expect("second load");
        assert!(pending_choice_path(&config_dir).exists());
    }

    #[test]
    fn pending_choice_surfaces_at_next_interactive_command() {
        let (tmp, config_dir) = fresh_config_dir("a4-surface");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        // A non-interactive invocation leaves the marker for a later one.
        let outcome = surface_pending_choice(&config_dir, &db_path, false, &mut |_| {
            ChoiceResponse::Pick(0)
        })
        .expect("non-interactive surface");
        assert_eq!(outcome, PendingChoiceOutcome::NotInteractive);
        assert!(pending_choice_path(&config_dir).exists());
        // The next interactive command presents the choice; a pick persists
        // via the writer and clears the marker.
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Pick(0)
        })
        .expect("interactive surface");
        assert_eq!(outcome, PendingChoiceOutcome::Chosen("NomicEmbedTextV15Q"));
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the pick");
        assert_eq!(entries[0].model_name, "NomicEmbedTextV15Q");
        assert!(!pending_choice_path(&config_dir).exists());

        // Variant: a database created in the interim voids the pending choice.
        let (tmp, config_dir) = fresh_config_dir("a4-surface-void");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        fs::write(&db_path, b"").expect("create db in the interim");
        let called = std::cell::Cell::new(false);
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            called.set(true);
            ChoiceResponse::Pick(0)
        })
        .expect("voided surface");
        assert_eq!(outcome, PendingChoiceOutcome::Voided);
        assert!(!called.get(), "a voided choice is never presented");
        assert!(
            !config_dir.join("embedders.json").exists(),
            "a voided choice writes nothing"
        );
        assert!(!pending_choice_path(&config_dir).exists());
    }

    #[test]
    fn setup_out_of_range_pick_keeps_pending_choice() {
        let (tmp, config_dir) = fresh_config_dir("a4fix-out-of-range");
        write_default_file(&config_dir);
        let target = config_dir.join("embedders.json");
        let before = fs::read(&target).expect("read default file");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        // "9" at the terminal maps to an out-of-range pick index: a
        // fat-finger, not a decline. The marker stays and nothing is
        // written, so the choice is offered again.
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Pick(9)
        })
        .expect("out-of-range surface");
        assert_eq!(outcome, PendingChoiceOutcome::InvalidInput);
        assert!(
            pending_choice_path(&config_dir).exists(),
            "an out-of-range pick leaves the marker in place"
        );
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "an out-of-range pick writes nothing"
        );
        // The next interactive command offers the choice again, and a
        // valid pick then persists and clears the marker.
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Pick(0)
        })
        .expect("re-offer surface");
        assert_eq!(outcome, PendingChoiceOutcome::Chosen("NomicEmbedTextV15Q"));
        assert!(!pending_choice_path(&config_dir).exists());
    }

    #[test]
    fn setup_invalid_response_keeps_pending_choice() {
        let (tmp, config_dir) = fresh_config_dir("a4fix-invalid");
        write_default_file(&config_dir);
        let target = config_dir.join("embedders.json");
        let before = fs::read(&target).expect("read default file");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        // Unparseable input ("abc", blank) is invalid, not a decline:
        // the marker stays and nothing is written.
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Invalid
        })
        .expect("invalid surface");
        assert_eq!(outcome, PendingChoiceOutcome::InvalidInput);
        assert!(
            pending_choice_path(&config_dir).exists(),
            "invalid input leaves the marker in place"
        );
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "invalid input writes nothing"
        );
    }

    #[test]
    fn setup_decline_clears_pending_choice() {
        let (tmp, config_dir) = fresh_config_dir("a4fix-decline");
        write_default_file(&config_dir);
        let target = config_dir.join("embedders.json");
        let before = fs::read(&target).expect("read default file");
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("neurostrata.db");
        // Only an explicit decline consumes the marker (without writing).
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            ChoiceResponse::Decline
        })
        .expect("decline surface");
        assert_eq!(outcome, PendingChoiceOutcome::Declined);
        assert!(
            !pending_choice_path(&config_dir).exists(),
            "an explicit decline clears the marker"
        );
        assert_eq!(
            fs::read(&target).expect("re-read file"),
            before,
            "a decline writes nothing"
        );
    }

    // --- The daemon pre-flight identity check and the pin-backfill
    // posture ---

    /// The FP32 default's descriptor, as a serving process records it.
    fn fp32_recorded_descriptor() -> crate::traits::EmbeddingDescriptor {
        crate::traits::EmbeddingDescriptor {
            provider: "fastembed".into(),
            model: "NomicEmbedTextV15".into(),
            revision: "onnx".into(),
            dims: 768,
            normalization: "unknown".into(),
            preprocessing: "unknown".into(),
            distance_metric: "cosine".into(),
        }
    }

    /// Every file under `root`, as relative path -> bytes: the
    /// byte-untouched comparison for the pre-flight (contents and the
    /// file set itself — no new files may appear either).
    fn dir_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.insert(
                        path.strip_prefix(root).expect("under root").to_path_buf(),
                        fs::read(&path).expect("read file"),
                    );
                }
            }
        }
        out
    }

    #[test]
    fn daemon_refuses_setup_chosen_model_against_existing_db() {
        // The pre-flight is filesystem-only, so the fixture store is
        // a bare db file plus its sidecar — no engine fixture is
        // needed, and the refusal must leave every byte (and the file
        // set) untouched.
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let tmp = tempfile::tempdir().expect("temp dir");
        let store_dir = tmp.path().join("store");
        fs::create_dir_all(&store_dir).expect("create store dir");
        let db_path = store_dir.join("ladybug.db");
        write_bare_db(&db_path);
        write_store_state(
            &db_path,
            &StoreState {
                version: STORE_STATE_VERSION,
                has_data: true,
                descriptor: fp32_recorded_descriptor(),
            },
        )
        .expect("write the data-bearing sidecar");

        // The Q configuration, written the setup way — against a
        // different, empty db path, as the plan's construction does.
        let (cfg_tmp, config_dir) = fresh_config_dir("a5-preflight-q");
        let other_db = cfg_tmp.path().join("ladybug.db");
        let q = candidates()
            .iter()
            .find(|c| c.model_name == "NomicEmbedTextV15Q")
            .expect("shortlist contains NomicEmbedTextV15Q");
        write_initial_embedders_file(&config_dir, q, &other_db).expect("write the Q pick");

        let before = dir_snapshot(&store_dir);
        let err = check_daemon_open_identity(&db_path, &config_dir)
            .expect_err("the pre-flight must refuse a data-bearing FP32 store under a Q config");
        assert!(
            crate::store::ladybug::embedding_identity_refusal_message(&err).is_some(),
            "the pre-flight refusal is the typed identity refusal: {err}"
        );
        assert!(
            err.to_string().contains("'NomicEmbedTextV15'")
                && err.to_string().contains("'NomicEmbedTextV15Q'"),
            "the refusal names the recorded and configured models: {err}"
        );
        assert_eq!(
            dir_snapshot(&store_dir),
            before,
            "a refused pre-flight leaves the store directory byte-identical, with no new files"
        );

        // Negative control: the configuration restored to the recorded
        // model passes.
        fs::write(
            config_dir.join("embedders.json"),
            "[{\"model_name\":\"NomicEmbedTextV15\",\"dimensions\":768}]",
        )
        .expect("restore the FP32 config");
        check_daemon_open_identity(&db_path, &config_dir)
            .expect("the recorded model's configuration passes");

        // Negative control: a pristine sidecar passes under the Q
        // config (nothing recorded yet — the choice is still open).
        fs::write(
            config_dir.join("embedders.json"),
            "[{\"model_name\":\"NomicEmbedTextV15Q\",\"dimensions\":768}]",
        )
        .expect("restore the Q config");
        write_store_state(
            &db_path,
            &StoreState {
                version: STORE_STATE_VERSION,
                has_data: false,
                descriptor: fp32_recorded_descriptor(),
            },
        )
        .expect("rewrite the sidecar as pristine");
        check_daemon_open_identity(&db_path, &config_dir)
            .expect("a pristine store passes under any configuration");

        // Negative control: no sidecar at all (a pre-sidecar store)
        // passes — there is no recorded identity to protect.
        fs::remove_file(store_state_path(&db_path)).expect("remove the sidecar");
        check_daemon_open_identity(&db_path, &config_dir)
            .expect("a pre-sidecar store passes the pre-flight");

        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    #[test]
    fn daemon_preflight_passes_incommensurate_configurations() {
        // The pre-flight refuses only when the configured descriptor
        // is exactly what the daemon will serve. A remote entry's
        // serving descriptor cannot be synthesized from config, and an
        // unparseable local name falls back to a different model at
        // load — both pass here and remain gated by the store-level
        // check on the true serving descriptor.
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let tmp = tempfile::tempdir().expect("temp dir");
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        write_store_state(
            &db_path,
            &StoreState {
                version: STORE_STATE_VERSION,
                has_data: true,
                descriptor: fp32_recorded_descriptor(),
            },
        )
        .expect("write the data-bearing sidecar");
        let (_cfg_tmp, config_dir) = fresh_config_dir("a5-preflight-incommensurate");

        fs::write(
            config_dir.join("embedders.json"),
            "[{\"model_name\":\"SomeRemoteModel\",\"dimensions\":768,\"base_url\":\"http://127.0.0.1:9/v1\",\"api_model\":\"SomeRemoteModel\"}]",
        )
        .expect("write a remote entry");
        check_daemon_open_identity(&db_path, &config_dir)
            .expect("a remote configuration is incommensurate for the pre-flight");

        fs::write(
            config_dir.join("embedders.json"),
            "[{\"model_name\":\"NotARealModel\",\"dimensions\":768}]",
        )
        .expect("write an unparseable local entry");
        check_daemon_open_identity(&db_path, &config_dir)
            .expect("an unparseable name is incommensurate for the pre-flight");

        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    #[test]
    fn hand_authored_shortlist_entry_without_pin_is_not_backfilled() {
        // The loader backfills only the FP32 default's pin. A
        // hand-authored shortlist entry stays pinless through the
        // loader and fails
        // closed at embedder load (the embed-side pin gate is pinned
        // by `pin_gate_refuses_a_pinless_shortlist_entry`); extending
        // the backfill is a deliberate future change to this test.
        let (_tmp, config_dir) = fresh_config_dir("a5-pin-no-backfill");
        fs::write(
            config_dir.join("embedders.json"),
            "[{\"model_name\":\"NomicEmbedTextV15Q\",\"dimensions\":768}]",
        )
        .expect("write the hand-authored entry");
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the file");
        assert_eq!(entries[0].model_name, "NomicEmbedTextV15Q");
        assert!(
            entries[0].model_sha256.is_none(),
            "the loader must not backfill a shortlist entry's pin"
        );
    }

    #[test]
    fn unparseable_sidecar_refuses_daemon_preflight() {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let tmp = tempfile::tempdir().expect("temp dir");
        let store_dir = tmp.path().join("store");
        fs::create_dir_all(&store_dir).expect("create store dir");
        let db_path = store_dir.join("ladybug.db");
        write_bare_db(&db_path);
        fs::write(sidecar_path_for(&db_path), "{not json").expect("write unparseable sidecar");
        let (_cfg_tmp, config_dir) = fresh_config_dir("unparseable-preflight");
        let before = dir_snapshot(&store_dir);
        let err = check_daemon_open_identity(&db_path, &config_dir)
            .expect_err("an unparseable sidecar must refuse the daemon pre-flight");
        let message = err.to_string();
        assert!(
            message.contains(&sidecar_path_for(&db_path).display().to_string()),
            "the refusal names the sidecar path: {message}"
        );
        assert!(
            message.contains("unparseable"),
            "the refusal says the sidecar is unparseable: {message}"
        );
        assert!(
            message.contains("cannot be parsed"),
            "the refusal gives the parse reason: {message}"
        );
        assert_eq!(
            dir_snapshot(&store_dir),
            before,
            "a refused pre-flight leaves the store directory byte-identical"
        );
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    #[test]
    fn wrong_version_sidecar_refuses_daemon_preflight() {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let tmp = tempfile::tempdir().expect("temp dir");
        let store_dir = tmp.path().join("store");
        fs::create_dir_all(&store_dir).expect("create store dir");
        let db_path = store_dir.join("ladybug.db");
        write_bare_db(&db_path);
        // Pristine on purpose: the refusal must not depend on has_data.
        let wrong_version = STORE_STATE_VERSION + 1;
        let content = format!(
            "{{\"version\": {wrong_version}, \"has_data\": false, \"descriptor\": {{\"provider\": \"fastembed\", \"model\": \"NomicEmbedTextV15\", \"revision\": \"onnx\", \"dims\": 768, \"normalization\": \"unknown\", \"preprocessing\": \"unknown\", \"distance_metric\": \"cosine\"}}}}"
        );
        fs::write(sidecar_path_for(&db_path), content).expect("write wrong-version sidecar");
        let (_cfg_tmp, config_dir) = fresh_config_dir("wrong-version-preflight");
        let before = dir_snapshot(&store_dir);
        let err = check_daemon_open_identity(&db_path, &config_dir)
            .expect_err("a wrong-version sidecar must refuse the daemon pre-flight");
        let message = err.to_string();
        assert!(
            message.contains(&format!(
                "unsupported store-state version {}",
                STORE_STATE_VERSION + 1
            )),
            "the refusal names the recorded version: {message}"
        );
        assert!(
            message.contains(&format!("expects version {STORE_STATE_VERSION}")),
            "the refusal names the expected version: {message}"
        );
        assert_eq!(
            dir_snapshot(&store_dir),
            before,
            "a refused pre-flight leaves the store directory byte-identical"
        );
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    fn write_wrong_version_sidecar(db_path: &Path, has_data: bool) {
        let wrong_version = STORE_STATE_VERSION + 1;
        let content = format!(
            "{{\"version\": {wrong_version}, \"has_data\": {has_data}, \"descriptor\": {{\"provider\": \"fastembed\", \"model\": \"NomicEmbedTextV15\", \"revision\": \"onnx\", \"dims\": 768, \"normalization\": \"unknown\", \"preprocessing\": \"unknown\", \"distance_metric\": \"cosine\"}}}}"
        );
        fs::write(sidecar_path_for(db_path), content).expect("write wrong-version sidecar");
    }

    #[test]
    fn read_store_state_distinguishes_absent_recorded_and_unparseable() {
        let (tmp, _config_dir) = fresh_config_dir("read-state-distinguishes");
        let db_path = tmp.path().join("ladybug.db");
        assert_eq!(
            read_store_state(&db_path),
            StoreStateRead::Absent,
            "no files at all reads as absent"
        );
        write_sidecar(&db_path, false);
        match read_store_state(&db_path) {
            StoreStateRead::Recorded(state) => {
                assert!(!state.has_data, "the recorded sidecar is pristine");
            }
            other => panic!("a valid sidecar reads as recorded, got {other:?}"),
        }
        fs::write(sidecar_path_for(&db_path), "{not json").expect("write corrupt sidecar");
        assert!(
            matches!(read_store_state(&db_path), StoreStateRead::Unparseable(_)),
            "a corrupt sidecar reads as unparseable, not absent"
        );
        write_wrong_version_sidecar(&db_path, false);
        match read_store_state(&db_path) {
            StoreStateRead::Unparseable(reason) => {
                assert!(
                    reason.contains("version"),
                    "the wrong-version reason names the version: {reason}"
                );
            }
            other => panic!("a wrong-version sidecar reads as unparseable, got {other:?}"),
        }
        // A missing `version` field fails the StoreState parse, so it
        // lands in the parse arm — not the version arm.
        let descriptor = "\"descriptor\": {\"provider\": \"fastembed\", \"model\": \"NomicEmbedTextV15\", \"revision\": \"onnx\", \"dims\": 768, \"normalization\": \"unknown\", \"preprocessing\": \"unknown\", \"distance_metric\": \"cosine\"}";
        fs::write(
            sidecar_path_for(&db_path),
            format!("{{\"has_data\": false, {descriptor}}}"),
        )
        .expect("write sidecar with no version field");
        match read_store_state(&db_path) {
            StoreStateRead::Unparseable(reason) => {
                assert!(
                    reason.contains("cannot be parsed"),
                    "a missing version field gives the parse reason: {reason}"
                );
                assert!(
                    !reason.contains("unsupported store-state version"),
                    "a missing version field must not give the version reason: {reason}"
                );
            }
            other => panic!("a versionless sidecar reads as unparseable, got {other:?}"),
        }
        // A `version` of the wrong JSON type (a string) likewise
        // fails the parse and lands in the parse arm.
        fs::write(
            sidecar_path_for(&db_path),
            format!("{{\"version\": \"1\", \"has_data\": false, {descriptor}}}"),
        )
        .expect("write sidecar with a string version field");
        match read_store_state(&db_path) {
            StoreStateRead::Unparseable(reason) => {
                assert!(
                    reason.contains("cannot be parsed"),
                    "a string version field gives the parse reason: {reason}"
                );
                assert!(
                    !reason.contains("unsupported store-state version"),
                    "a string version field must not give the version reason: {reason}"
                );
            }
            other => panic!("a string-version sidecar reads as unparseable, got {other:?}"),
        }
    }

    #[test]
    fn store_has_data_stays_fail_closed_for_unparseable_and_wrong_version() {
        let (tmp, _config_dir) = fresh_config_dir("has-data-fail-closed");
        let db_path = tmp.path().join("ladybug.db");
        assert!(!store_has_data(&db_path), "nothing on disk: no data");
        assert!(store_is_pristine(&db_path));
        write_sidecar(&db_path, false);
        assert!(
            !store_has_data(&db_path),
            "a valid pristine sidecar: no data"
        );
        assert!(store_is_pristine(&db_path));
        write_sidecar(&db_path, true);
        assert!(store_has_data(&db_path), "a valid data sidecar: data");
        assert!(!store_is_pristine(&db_path));
        fs::write(sidecar_path_for(&db_path), "{not json").expect("write corrupt sidecar");
        assert!(
            store_has_data(&db_path),
            "an unparseable sidecar means data"
        );
        assert!(!store_is_pristine(&db_path));
        write_wrong_version_sidecar(&db_path, false);
        assert!(
            store_has_data(&db_path),
            "a wrong-version sidecar means data, whatever its flag says"
        );
        assert!(!store_is_pristine(&db_path));
    }

    #[test]
    fn writer_and_offer_fail_closed_with_wrong_version_sidecar() {
        let (tmp, config_dir) = fresh_config_dir("fail-closed-wrong-version");
        write_default_file(&config_dir);
        record_pending_choice(&config_dir);
        let db_path = tmp.path().join("ladybug.db");
        write_bare_db(&db_path);
        write_wrong_version_sidecar(&db_path, false);
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(result.is_err(), "a wrong-version sidecar must refuse");
        let entries = get_acceptable_embedders_in(&config_dir).expect("loader reads the file");
        assert_eq!(
            serde_json::to_value(&entries).expect("serialize entries"),
            serde_json::to_value(default_acceptable_embedders()).expect("serialize default"),
            "a refused write leaves the default embedders file, with no pick created"
        );
        let called = std::cell::Cell::new(false);
        let outcome = surface_pending_choice(&config_dir, &db_path, true, &mut |_| {
            called.set(true);
            ChoiceResponse::Pick(0)
        })
        .expect("surface");
        assert_eq!(outcome, PendingChoiceOutcome::Voided);
        assert!(!called.get(), "a wrong-version sidecar is never offered");
    }
}
