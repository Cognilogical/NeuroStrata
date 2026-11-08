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
            max_tokens: 8192,
            note: "Same model as the standard default, int8-quantized. Dimensions and context unchanged.",
        },
        ModelChoice {
            model_name: "BGESmallENV15Q",
            dimensions: 384,
            file_bytes: 66465124,
            max_tokens: 512,
            note: "Smallest download. 384 dimensions; 512-token context. Already the low profile's model.",
        },
        ModelChoice {
            model_name: "SnowflakeArcticEmbedMQ",
            dimensions: 768,
            file_bytes: 110084023,
            max_tokens: 512,
            note: "Keeps 768 dimensions; 512-token context.",
        },
        ModelChoice {
            model_name: "BGESmallENV15",
            dimensions: 384,
            file_bytes: 133093490,
            max_tokens: 512,
            note: "The non-quantized small model. 384 dimensions; 512-token context.",
        },
        ModelChoice {
            model_name: "BGEBaseENV15Q",
            dimensions: 768,
            file_bytes: 217824172,
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
/// Fail-closed, in order: an existing database at `db_path` refuses (a model
/// was already chosen by use); an existing `embedders.json` that parses to
/// anything other than exactly the built-in default refuses, byte-identical;
/// an unparseable existing file refuses (the writer never repairs a file it
/// cannot classify, even though the loader tolerates one by ignoring it).
/// The one replaceable existing file is the exact built-in default a first
/// daemon start materializes before any database exists.
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
    if db_path.exists() {
        anyhow::bail!(
            "refusing to write the initial embedders file: a database already exists at {}",
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
        let default = default_acceptable_embedders();
        if serde_json::to_value(&existing)? != serde_json::to_value(&default)? {
            anyhow::bail!(
                "refusing to overwrite {}: its embedder list is not the built-in default",
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
        model_sha256: None,
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
    let tmp = dir.join(format!(".embedders.json.tmp-{}", std::process::id()));
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

    // --- Task A3: the fail-closed initial-file writer ---

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

    #[test]
    fn writer_refuses_when_db_exists() {
        let (tmp, config_dir) = fresh_config_dir("db-exists");
        let db_path = tmp.path().join("neurostrata.db");
        fs::write(&db_path, b"").expect("create empty db file");
        let result = write_initial_embedders_file(&config_dir, bge_small_q(), &db_path);
        assert!(
            result.is_err(),
            "an existing database must refuse the write"
        );
        assert!(
            !config_dir.join("embedders.json").exists(),
            "no embedders.json may be created when the write is refused"
        );
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
}
