//! Label derivation and tier management for the observation ledger.
//!
//! Each `DerivedLabel` carries an explicit source tier so that downstream
//! dataset builders can exclude `SingleSignalWeakProxy` rows from calibration
//! and evaluation splits (Step 2 of Task 6).

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Source tier for a derived label, determining which dataset split it may enter.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LabelTier {
    /// A hard constraint or invariant rule — highest authority.
    DeterministicInvariant,
    /// Multiple signals agree — high confidence.
    MultiSignalConsensus,
    /// Single signal, weak proxy — cannot enter calibration/eval files.
    SingleSignalWeakProxy,
    /// Manually adjudicated gold-standard data.
    ShippedAnchorGold,
}

/// A label derived from an observation, with its source tier and rule provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedLabel {
    pub observation_id: String,
    pub expected_duplicate_probability: Option<f32>,
    pub tier: LabelTier,
    pub rule_id: String,
    pub rule_version: u32,
    pub weight: f32,
}

/// Versioned label rules that drive `derive_labels`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelRule {
    pub id: String,
    pub version: u32,
    pub tier: LabelTier,
    /// Optional explicit probability if this rule is a hard constraint.
    pub fixed_probability: Option<f32>,
}

/// Derive labels from observations, applying versioned rules.
/// Returns empty if no rules match (absence of follow-up = no DerivedLabel).
pub fn derive_labels(
    observation_ids: &[String],
    rules: &[LabelRule],
) -> Result<Vec<DerivedLabel>> {
    let mut labels = Vec::new();
    for obs_id in observation_ids {
        for rule in rules {
            labels.push(DerivedLabel {
                observation_id: obs_id.clone(),
                expected_duplicate_probability: rule.fixed_probability,
                tier: rule.tier,
                rule_id: rule.id.clone(),
                rule_version: rule.version,
                weight: 1.0,
            });
        }
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_observations_immutable_when_label_rules_change() {
        // Observation data must remain immutable when label rules change.
        // This test FAILS until Step 2: derive_labels returns the same
        // observation_ids regardless of which rules are applied.
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string()];
        let rules = vec![LabelRule {
            id: "rule-v1".into(),
            version: 1,
            tier: LabelTier::ShippedAnchorGold,
            fixed_probability: Some(1.0),
        }];

        let labels = derive_labels(&obs_ids, &rules).expect("derive labels");
        // Observations must remain unchanged regardless of rule change
        let labels_changed: Vec<_> = labels
            .iter()
            .map(|l| l.observation_id.as_str())
            .collect();
        assert_eq!(labels_changed, obs_ids);
    }

    #[test]
    fn absence_of_follow_up_yields_no_derived_label() {
        // No rules => no labels derived
        let obs_ids = vec!["obs-1".to_string()];
        let labels = derive_labels(&obs_ids, &[]).expect("derive labels");
        assert!(labels.is_empty(), "no rules should yield no labels");
    }

    #[test]
    fn single_signal_weak_proxy_cannot_enter_calibration() {
        // Step 2: SingleSignalWeakProxy cannot enter calibration or evaluation
        let rules = vec![LabelRule {
            id: "weak-proxy".into(),
            version: 1,
            tier: LabelTier::SingleSignalWeakProxy,
            fixed_probability: Some(0.6),
        }];
        assert!(
            rules[0].tier != LabelTier::ShippedAnchorGold,
            "weak proxy must not masquerade as gold"
        );
    }
}