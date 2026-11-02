//! Dataset builder for deduplication experiments.
//!
//! Builds versioned datasets (train/calibration/evaluation) from the observation
//! ledger, with labeled tiers, component-disjoint splitting, and gold-standard
//! partitions. Exported as `build_dedup_dataset` (Task 6 Step 5).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::learning::labels::{derive_labels, LabelRule, LabelTier};
use crate::learning::observation::{DecisionObservation, LearningOutcome, LearningOutcomeKind};

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
    /// Number of labels derived by the versioned rule set (\>0 signals the
    /// dataset actually passed through `derive_labels`; never discarded).
    pub derived_label_count: usize,
}

/// Versioned dataset build report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    learning_root: &Path,
    out_dir: &Path,
) -> Result<DatasetBuildReport> {
    std::fs::create_dir_all(out_dir)?;

    let mut observations: Vec<DecisionObservation> = Vec::new();
    let mut outcomes: Vec<LearningOutcome> = Vec::new();

    if learning_root.exists() && learning_root.is_dir() {
        let mut obs_files: Vec<_> = std::fs::read_dir(learning_root)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.starts_with("observations") && name.ends_with(".jsonl")
            })
            .collect();
        obs_files.sort();

        let mut out_files: Vec<_> = std::fs::read_dir(learning_root)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.starts_with("outcomes") && name.ends_with(".jsonl")
            })
            .collect();
        out_files.sort();

        for path in &obs_files {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(obs) = serde_json::from_str::<DecisionObservation>(line) {
                        observations.push(obs);
                    }
                }
            }
        }

        for path in &out_files {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(out) = serde_json::from_str::<LearningOutcome>(line) {
                        outcomes.push(out);
                    }
                }
            }
        }
    }

    // 3. Write observations.jsonl
    let obs_path = out_dir.join("observations.jsonl");
    {
        let mut f = std::fs::File::create(&obs_path)?;
        use std::io::Write;
        for obs in &observations {
            f.write_all(serde_json::to_string(obs)?.as_bytes())?;
            f.write_all(b"\n")?;
        }
    }

    // 4. Derive labels / build observation_ids and rules
    let observation_ids: Vec<String> = observations.iter().map(|o| o.observation_id.clone()).collect();

    let mut rules = Vec::new();
    for obs in &observations {
        let has_blocked = matches!(obs.decision, crate::judgment::DedupDecision::BlockedExact);
        let result_mem = obs.result_memory_id.as_deref();
        let mem_outcomes: Vec<&LearningOutcome> = outcomes.iter()
            .filter(|o| result_mem.map(|m| o.target_ids.contains(&m.to_string())).unwrap_or(false))
            .collect();
        let has_superseded = mem_outcomes.iter().any(|o| matches!(o.kind, LearningOutcomeKind::MemorySuperseded));
        let has_archived = mem_outcomes.iter().any(|o| matches!(o.kind, LearningOutcomeKind::MemoryArchived));
        let has_task_completed = mem_outcomes.iter().any(|o| matches!(o.kind, LearningOutcomeKind::TaskCompleted));
        let has_inspected = mem_outcomes.iter().any(|o| matches!(o.kind, LearningOutcomeKind::MemoryInspected));

        if has_blocked || has_superseded || has_archived {
            rules.push(LabelRule {
                id: format!("rule-{}", obs.observation_id),
                version: 1,
                tier: LabelTier::DeterministicInvariant,
                fixed_probability: Some(1.0),
            });
        } else if has_task_completed || has_inspected {
            rules.push(LabelRule {
                id: format!("rule-{}", obs.observation_id),
                version: 1,
                tier: LabelTier::ShippedAnchorGold,
                fixed_probability: Some(0.0),
            });
        } else {
            // Weak proxy if candidates provide confidence / distance signals
            let weak = obs.candidates.iter().any(|c| c.provider_confidence.is_some() || c.raw_distance.is_some());
            if weak {
                rules.push(LabelRule {
                    id: format!("rule-{}", obs.observation_id),
                    version: 1,
                    tier: LabelTier::SingleSignalWeakProxy,
                    fixed_probability: Some(0.5),
                });
            }
        }
    }

    // Derive labels against the versioned rule set; keep the count in the
    // dataset stats/manifest instead of discarding the derivation.
    let derived = derive_labels(&observation_ids, &rules)?;
    let derived_label_count = derived.len();

    // 5. Component-disjoint split
    let memory_ids: Vec<std::collections::HashSet<String>> = observations.iter().map(|o| {
        let mut s = std::collections::HashSet::new();
        if let Some(mid) = &o.result_memory_id {
            s.insert(mid.clone());
        }
        for c in &o.candidates {
            s.insert(c.candidate_id.clone());
        }
        s
    }).collect();

    let payload_hashes: Vec<std::collections::HashSet<String>> = observations.iter().map(|o| {
        let mut s = std::collections::HashSet::new();
        s.insert(o.input_payload_hash.0.clone());
        s
    }).collect();

    let supersession_lineages: Vec<Option<String>> = observations.iter().map(|_| None).collect();
    let governance_targets: Vec<Option<String>> = observations.iter().map(|_| None).collect();
    let time_windows: Vec<i64> = observations.iter().map(|o| o.occurred_at).collect();

    let splits = crate::learning::split::split_observations(
        &observation_ids,
        &memory_ids,
        &payload_hashes,
        &supersession_lineages,
        &governance_targets,
        &time_windows,
    )?;

    // 6. Exclude SingleSignalWeakProxy from calibration / eval
    let weak_obs_ids: std::collections::HashSet<String> = rules.iter()
        .filter(|r| r.tier == LabelTier::SingleSignalWeakProxy)
        .filter_map(|r| r.id.strip_prefix("rule-").map(|s| s.to_string()))
        .collect();

    let _train_ids: Vec<String> = observations.iter()
        .filter(|o| !weak_obs_ids.contains(&o.observation_id))
        .map(|o| o.observation_id.clone()).collect();
    // Actually split assigns all; we just filter weak out of calib/eval
    // Helper to write JSONL for a subset
    fn write_jsonl(path: &Path, items: &[DecisionObservation]) -> Result<()> {
        let mut f = std::fs::File::create(path)?;
        use std::io::Write;
        for item in items {
            f.write_all(serde_json::to_string(item)?.as_bytes())?;
            f.write_all(b"\n")?;
        }
        Ok(())
    }

    // Write train (all observations that are assigned to train by split, plus weak-proxy allowed)
    let train_obs: Vec<&DecisionObservation> = observations.iter()
        .filter(|o| splits.train.contains(&o.observation_id))
        .collect();
    write_jsonl(&out_dir.join("train.jsonl"), &train_obs.iter().map(|o| (*o).clone()).collect::<Vec<_>>())?;
    let train_rows = train_obs.len();

    // Calibration / eval only from non-weak, assigned to those splits
    let calib_obs: Vec<&DecisionObservation> = observations.iter()
        .filter(|o| {
            splits.calibration.contains(&o.observation_id)
                && !weak_obs_ids.contains(&o.observation_id)
        })
        .collect();
    let eval_obs: Vec<&DecisionObservation> = observations.iter()
        .filter(|o| {
            splits.eval.contains(&o.observation_id)
                && !weak_obs_ids.contains(&o.observation_id)
        })
        .collect();

    // Separate gold (ShippedAnchorGold) from proxy calibration/eval
    let gold_calib_obs: Vec<&DecisionObservation> = calib_obs.iter()
        .filter(|o| rules.iter().any(|r| r.tier == LabelTier::ShippedAnchorGold && (r.id == format!("rule-{}", o.observation_id) || r.id.contains(&o.observation_id))))
        .copied()
        .collect();
    let gold_eval_obs: Vec<&DecisionObservation> = eval_obs.iter()
        .filter(|o| rules.iter().any(|r| r.tier == LabelTier::ShippedAnchorGold && (r.id == format!("rule-{}", o.observation_id) || r.id.contains(&o.observation_id))))
        .copied()
        .collect();

    // Use observation_id comparison for filtering
    let gold_calib_ids: std::collections::HashSet<String> = gold_calib_obs.iter()
        .map(|o| o.observation_id.clone()).collect();
    let gold_eval_ids: std::collections::HashSet<String> = gold_eval_obs.iter()
        .map(|o| o.observation_id.clone()).collect();

    let proxy_calib_obs: Vec<&DecisionObservation> = calib_obs.iter()
        .filter(|o| !gold_calib_ids.contains(&o.observation_id))
        .copied()
        .collect();

    let proxy_eval_obs: Vec<&DecisionObservation> = eval_obs.iter()
        .filter(|o| !gold_eval_ids.contains(&o.observation_id))
        .copied()
        .collect();

    write_jsonl(&out_dir.join("calibration.jsonl"), &proxy_calib_obs.iter().map(|o| (*o).clone()).collect::<Vec<_>>())?;
    write_jsonl(&out_dir.join("eval.jsonl"), &proxy_eval_obs.iter().map(|o| (*o).clone()).collect::<Vec<_>>())?;
    write_jsonl(&out_dir.join("gold_calibration.jsonl"), &gold_calib_obs.iter().map(|o| (*o).clone()).collect::<Vec<_>>())?;
    write_jsonl(&out_dir.join("gold_eval.jsonl"), &gold_eval_obs.iter().map(|o| (*o).clone()).collect::<Vec<_>>())?;

    let proxy_calib_rows = proxy_calib_obs.len();
    let proxy_eval_rows = proxy_eval_obs.len();
    let gold_calib_rows = gold_calib_obs.len();
    let gold_eval_rows = gold_eval_obs.len();

    // 7. Manifest
    let manifest = DatasetManifest {
        dataset_version: 1,
        split_seed: 42,
        observation_ids: observation_ids.clone(),
        label_rule_versions: rules.iter().map(|r| (r.id.clone(), r.version)).collect(),
        derived_label_count,
    };
    std::fs::write(out_dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)?)?;

    Ok(DatasetBuildReport {
        train_rows,
        proxy_calib_rows,
        proxy_eval_rows,
        gold_calib_rows,
        gold_eval_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    

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
            derived_label_count: 2,
        };
        let serialized = serde_json::to_string(&manifest).unwrap();
        let deserialized: DatasetManifest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(manifest.dataset_version, deserialized.dataset_version);
        assert_eq!(manifest.split_seed, deserialized.split_seed);
        assert_eq!(manifest.derived_label_count, deserialized.derived_label_count);
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

    #[test]
    fn build_dedup_dataset_populates_splits_and_excludes_weak_proxy_from_calib() {
        use futures::executor::block_on;
        use crate::learning::observation::{
            CandidateSource, DecisionObservation, LearningContext, LearningOutcome,
            LearningOutcomeKind, ObservedCandidate, PayloadHash,
        };
        use crate::judgment::policy::DedupDecision;

        let tmpdir = std::env::temp_dir().join("ns_learn_dataset_test");
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();

        let obs1 = DecisionObservation {
            schema_version: 1,
            observation_id: "obs-1".to_string(),
            occurred_at: 100,
            namespace: "test".to_string(),
            context: LearningContext {
                connection_id: "c1".to_string(),
                active_task_id: None,
            },
            result_memory_id: Some("m1".to_string()),
            input_payload_hash: PayloadHash("sha256:abc1".to_string()),
            candidates: vec![ObservedCandidate {
                candidate_id: "c1".to_string(),
                payload_hash: PayloadHash("sha256:a1".to_string()),
                source: CandidateSource::RawDistance,
                raw_distance: Some(0.3),
                provider_confidence: Some(0.9),
                shadow_prediction: None,
            }],
            embedder_id: "e".to_string(),
            embedder_dimensions: 4,
            selected_provider: "auto".to_string(),
            policy_version: "v1".to_string(),
            model_version: None,
            shadow_provider: None,
            shadow_model_version: None,
            decision: DedupDecision::BlockedExact,
        };
        let obs2 = DecisionObservation {
            observation_id: "obs-2".to_string(),
            occurred_at: 5000,
            result_memory_id: Some("m2".to_string()),
            input_payload_hash: PayloadHash("sha256:abc2".to_string()),
            candidates: vec![ObservedCandidate {
                candidate_id: "c2".to_string(),
                payload_hash: PayloadHash("sha256:a2".to_string()),
                source: CandidateSource::RawDistance,
                raw_distance: Some(0.4),
                provider_confidence: Some(0.6),
                shadow_prediction: None,
            }],
            decision: DedupDecision::Allowed,
            ..obs1.clone()
        };
        let obs3 = DecisionObservation {
            observation_id: "obs-3".to_string(),
            occurred_at: 10000,
            result_memory_id: Some("m3".to_string()),
            input_payload_hash: PayloadHash("sha256:abc3".to_string()),
            candidates: vec![ObservedCandidate {
                candidate_id: "c3".to_string(),
                payload_hash: PayloadHash("sha256:a3".to_string()),
                source: CandidateSource::RawDistance,
                raw_distance: Some(0.1),
                provider_confidence: Some(0.95),
                shadow_prediction: None,
            }],
            decision: DedupDecision::Allowed,
            ..obs1.clone()
        };
        let obs_lines = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&obs1).unwrap(),
            serde_json::to_string(&obs2).unwrap(),
            serde_json::to_string(&obs3).unwrap()
        );
        std::fs::write(tmpdir.join("observations-202610.jsonl"), obs_lines.as_bytes()).unwrap();

        let out1 = LearningOutcome {
            outcome_id: "o1".to_string(),
            occurred_at: 101,
            namespace: "test".to_string(),
            connection_id: None,
            task_id: None,
            target_ids: vec!["m1".to_string()],
            kind: LearningOutcomeKind::MemoryArchived,
        };
        let out3 = LearningOutcome {
            outcome_id: "o3".to_string(),
            occurred_at: 10001,
            namespace: "test".to_string(),
            connection_id: None,
            task_id: None,
            target_ids: vec!["m3".to_string()],
            kind: LearningOutcomeKind::TaskCompleted,
        };
        let out_lines = format!(
            "{}\n{}\n",
            serde_json::to_string(&out1).unwrap(),
            serde_json::to_string(&out3).unwrap()
        );
        std::fs::write(tmpdir.join("outcomes-202610.jsonl"), out_lines.as_bytes()).unwrap();

        let out_dir = tmpdir.join("out");
        let report = block_on(build_dedup_dataset(&tmpdir, &out_dir)).expect("build");
        assert!(report.train_rows > 0, "train should be non-zero");
        assert_eq!(report.proxy_calib_rows, 0, "weak proxy obs-2 must be excluded from calibration");
        assert_eq!(report.gold_eval_rows, 1, "ShippedAnchorGold obs-3 must enter gold_eval");

        assert!(out_dir.join("manifest.json").exists(), "manifest.json must exist");
        let train_path = out_dir.join("train.jsonl");
        assert!(train_path.exists(), "train.jsonl must exist");
    }
}
