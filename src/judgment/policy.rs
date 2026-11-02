use crate::traits::UpsertReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DedupDecision {
    Allowed,
    AllowedWithHint,
    BlockedExact,
    BlockedModel,
    LinkedExisting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupHint {
    pub candidate_id: String,
    pub raw_distance: f32,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct MemoryWriteReport {
    pub upsert: Option<UpsertReport>,
    pub decision: DedupDecision,
    pub dedup_hint: Option<DedupHint>,
    pub linked_existing_id: Option<String>,
}

// `EmbedderDistancePolicy`/`DedupDecisionPolicy` were removed: the policy in
// force is encoded in `DeduplicationConfig` + `model_may_block` (production +
// test-verified: only a Gold-calibrated, currently-active model may block a
// bare standalone add; inline task extraction never model-blocks).

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationTier {
    ProxyInternal,
    Gold,
}

/// Whether a model with this identity may gate (block) a memory write.
/// Compiled and used in production: `LayaOnnxProvider::may_block` calls this.
pub fn model_may_block(
    calibration_tier: CalibrationTier,
    model_version: &str,
    active_version: Option<&str>,
    is_bare_standalone_add: bool,
) -> bool {
    if !is_bare_standalone_add {
        return false;
    }
    if !matches!(calibration_tier, CalibrationTier::Gold) {
        return false;
    }
    match active_version {
        Some(av) if av == model_version => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_internal_cannot_block() {
        assert!(!model_may_block(
            CalibrationTier::ProxyInternal,
            "v1",
            Some("v1"),
            true
        ));
    }

    #[test]
    fn shadow_model_cannot_block() {
        // Gold tier but not active => shadow, cannot block
        assert!(!model_may_block(
            CalibrationTier::Gold,
            "v2",
            Some("v1"),
            true
        ));
    }

    #[test]
    fn gold_model_must_be_active_to_block() {
        assert!(model_may_block(
            CalibrationTier::Gold,
            "v1",
            Some("v1"),
            true
        ));
    }

    #[test]
    fn inline_task_extraction_never_model_blocks() {
        assert!(!model_may_block(
            CalibrationTier::Gold,
            "v1",
            Some("v1"),
            false
        ));
    }
}
