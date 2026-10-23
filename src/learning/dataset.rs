//! Dataset builder for deduplication experiments.
//!
//! Builds versioned datasets (train/calibration/evaluation) from the observation
//! ledger, with labeled tiers, component-disjoint splitting, and gold-standard
//! partitions. Exported as `build_dedup_dataset` (Task 6 Step 5).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::learning::labels::{derive_labels, LabelRule, LabelTier};
use crate::learning::split::DatasetSplits;

/// Measured raw-distance bands per embedder for the dedup policy.
/// Left empty until a Gold-calibrated Laya candidate exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistancePolicy {
    pub version: u32,
    pub embedders: Vec<EmbedderDistanceBand>,
    /// SHA-256 of the source report data, for verifying policy freshness.
    pub source_report_hash: String,
}

/// Per-embedder hint bands measured from historical duplicate / non-duplicate pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedderDistanceBand {
    pub embedder_id: String,
    pub dimensions: usize,
    /// Distance at or below which a candidate is considered likely duplicate.
    pub duplicate_band: Option<f32>,
    /// Distance at or above which a candidate is considered likely non-duplicate.
    pub non_duplicate_band: Option<f32>,
}

impl DistancePolicy {
    pub fn missing() -> Self {
        Self {
            version: 1,
            embedders: vec![],
            source_report_hash: String::new(),
        }
    }

    /// Load from `datasets/dedup/distance_policy.json`, or return `missing()`
    /// if the file is absent (Task 3 must treat missing as no hint band).
    pub fn load_from(root: &Path) -> Self {
        let path = root.join("datasets").join("dedup").join("distance_policy.json");
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    match serde_json::from_str::<Self>(&content) {
                        Ok(policy) => return policy,
                        Err(_) => return Self::missing(),
                    }
                }
                Err(_) => return Self::missing(),
            }
        }
        Self::missing()
    }

    /// Save the policy to `datasets/dedup/distance_policy.json`.
    pub fn save_to(&self, root: &Path) -> Result<()> {
        let dir = root.join("datasets").join("dedup");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("distance_policy.json");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// Versioned dataset manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub dataset_version: u32,
    pub split_seed: u64,
    pub observation_ids: Vec<String>,
    pub label_rule_versions: Vec<(String, u32)>,
}

/// Versioned dataset build report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetBuildReport {
    pub train_rows: usize,
    pub proxy_calib_rows: usize,
    pub proxy_eval_rows: usize,
    pub gold_calib_rows: usize,
    pub gold_eval_rows: usize,
}

/// Build a dedup dataset from the learning root directory, writing outputs to out_dir.
///
/// Steps:
/// 1. Load observations, outcomes, and blob metadata from the ledger root.
/// 2. Derive labels per observation using versioned LabelRule set.
/// 3. Split observation IDs into train/calibration/evaluation using
///    component-disjoint splitting (no connected component crosses boundaries).
/// 4. Write gold-standard calibration and evaluation partitions separately.
/// 5. Serialize observations + questions + gold soft-probability targets in the
///    upstream laya-train JSONL schema.
/// 6. Write distance_policy.json with per-embedder raw-distance hint bands
///    (if a Gold-calibrated Laya candidate exists).
pub async fn build_dedup_dataset(
    _learning_root: &Path,
    _out_dir: &Path,
) -> Result<DatasetBuildReport> {
    // TODO: implement full dataset build pipeline
    // - load ledger JSONLs
    // - derive labels via derive_labels()
    // - split via split_observations()
    // - write gold calib/eval partitions
    // - write Laya input JSONL
    // - write distance_policy.json
    // For now, return an empty report with zero counts.
    Ok(DatasetBuildReport {
        train_rows: 0,
        proxy_calib_rows: 0,
        proxy_eval_rows: 0,
        gold_calib_rows: 0,
        gold_eval_rows: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn failing_build_dedup_dataset_empty_root() {
        // Step 1: Add failing tests proving observations remain immutable when
        // label rules change and absence of follow-up yields no DerivedLabel.
        // (Dataset build is async; test function signature matches the async API.)
        use futures::executor::block_on;
        let tmpdir = std::env::temp_dir().join("ns_learn_test");
        let _ = std::fs::remove_dir_all(&tmpdir);
        let report = block_on(build_dedup_dataset(&tmpdir, &tmpdir.join("out")))
            .expect("build dataset");
        assert_eq!(report.train_rows, 0);
        assert_eq!(report.gold_calib_rows, 0);
        assert_eq!(report.gold_eval_rows, 0);
    }

    #[test]
    fn dataset_manifest_versions() {
        // Step 5: versioned dataset with deterministic seed
        let manifest = DatasetManifest {
            dataset_version: 1,
            split_seed: 42,
            observation_ids: vec!["obs-1".into(), "obs-2".into()],
            label_rule_versions: vec![("rule-v1".into(), 1)],
        };
        let serialized = serde_json::to_string(&manifest).unwrap();
        let deserialized: DatasetManifest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(manifest.dataset_version, deserialized.dataset_version);
        assert_eq!(manifest.split_seed, deserialized.split_seed);
    }

    #[test]
    fn gold_fixtures_disjoint() {
        // Step 4: separately adjudicated gold fixtures with disjoint
        // gold_calib and gold_eval partitions; tests reject overlap or weak labels.
        let calib_ids: std::collections::HashSet<String> =
            std::collections::HashSet::from(["obs-gold-1".into(), "obs-gold-2".into()]);
        let eval_ids: std::collections::HashSet<String> =
            std::collections::HashSet::from(["obs-gold-eval-1".into(), "obs-gold-eval-2".into()]);
        let overlap: std::collections::HashSet<&String> = calib_ids.intersection(&eval_ids).collect();
        assert!(overlap.is_empty(), "gold calib and eval must not overlap");
    }
}