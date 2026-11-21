//! Outcome-verified belief reflection for Capability E (`neurostrata-mpau`).
//!
//! Provides deterministic bounded reliability updates for governing rules and
//! fired procedures based on task completion (positive) and action reverts or
//! failed attempts (negative). Negative outcomes also set a `stale_after`
//! review flag and return an explicit contradiction-check request compatible
//! with the existing Laya queue/hook.
//!
//! CONCURRENCY CONTRACT: the `apply_*_outcome` helpers perform a read-modify-
//! write cycle over `metadata`. They are NOT atomic and do NOT claim a compare-
//! and-set themselves. Every caller must serialize the read-modify-write via
//! the store's lock or CAS. Production hook integration will wrap these calls
//! in the store lock/CAS; until then callers must not assume an atomic claim.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

/// Bounded outcome statistics for a rule/procedure.
/// Counters are monotonic; reliability_score is clamped to [0, 1] and computed
/// from a documented Beta posterior with a (2, 2) prior:
///   reliability = (successes + 2) / (successes + reverts + failures + 4)
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OutcomeStats {
    pub successes: u64,
    pub reverts: u64,
    pub failures: u64,
    pub reliability_score: f64,
}

/// Type of negative outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegativeKind {
    /// An action revert was executed (neurostrata_action(revert)).
    Revert,
    /// A negative exemplar was recorded (metadata.attempt.outcome == "failed").
    FailedAttempt,
}

/// Request for a contradiction check, compatible with Laya queue/hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContradictionCheckRequest {
    pub target_memory_id: String,
    pub action_receipt_id: Option<String>,
    pub reason: String,
}

/// Result of applying an outcome update. `OutcomeStats` carries an f64 score,
/// so this type is `PartialEq` only — deriving `Eq` would be invalid.
#[derive(Debug, Clone, PartialEq)]
pub struct OutcomeUpdate {
    pub stats: OutcomeStats,
    pub contradiction_check: Option<ContradictionCheckRequest>,
    pub stale_after: Option<DateTime<Utc>>,
}

/// Rejection reason for a malformed outcome update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeError {
    /// Outcome updates require a nonempty receipt/evidence id for idempotency.
    EmptyReceiptId,
    /// A negative outcome requires a nonempty target memory id.
    EmptyTargetMemoryId,
    /// `metadata` was not a JSON object; a read-modify-write is impossible.
    NonObjectMetadata,
}

/// Internal state to track processed receipts for idempotency.
/// `processed_receipts` is a `BTreeSet` so persistence is deterministic.
#[derive(Debug, Clone, Default)]
pub struct OutcomeTracker {
    stats: OutcomeStats,
    processed_receipts: BTreeSet<String>,
}

impl OutcomeTracker {
    /// Create a new tracker from existing stats and already-processed receipt ids.
    pub fn new(initial_stats: OutcomeStats, processed: BTreeSet<String>) -> Self {
        Self {
            stats: initial_stats,
            processed_receipts: processed,
        }
    }

    /// Returns the current stats (cloned).
    pub fn stats(&self) -> OutcomeStats {
        self.stats.clone()
    }

    /// Records a positive outcome (task completion or un-reverted apply).
    /// `receipt_id` must be a nonempty id; an empty id is rejected so positive
    /// outcomes are idempotent too. An already-seen id is a no-op.
    pub fn on_positive(&mut self, receipt_id: Option<&str>) -> Result<OutcomeStats, OutcomeError> {
        let Some(id) = receipt_id else {
            return Err(OutcomeError::EmptyReceiptId);
        };
        if id.trim().is_empty() {
            return Err(OutcomeError::EmptyReceiptId);
        }
        if !self.processed_receipts.insert(id.to_string()) {
            return Ok(self.stats.clone()); // idempotent
        }
        self.stats.successes = self.stats.successes.saturating_add(1);
        self.stats.reliability_score = compute_reliability(&self.stats);
        Ok(self.stats.clone())
    }

    /// Records a negative outcome (revert or failed attempt).
    /// `receipt_id` and `target_memory_id` must both be nonempty; malformed
    /// input is rejected (fail closed). An already-seen receipt is a no-op.
    /// Returns the updated stats and a contradiction check request + stale_after flag.
    pub fn on_negative(
        &mut self,
        kind: NegativeKind,
        receipt_id: &str,
        target_memory_id: &str,
        now: DateTime<Utc>,
    ) -> Result<OutcomeUpdate, OutcomeError> {
        if receipt_id.trim().is_empty() {
            return Err(OutcomeError::EmptyReceiptId);
        }
        if target_memory_id.trim().is_empty() {
            return Err(OutcomeError::EmptyTargetMemoryId);
        }
        if !self.processed_receipts.insert(receipt_id.to_string()) {
            return Ok(OutcomeUpdate {
                stats: self.stats.clone(),
                contradiction_check: None,
                stale_after: None,
            });
        }
        match kind {
            NegativeKind::Revert => {
                self.stats.reverts = self.stats.reverts.saturating_add(1);
            }
            NegativeKind::FailedAttempt => {
                self.stats.failures = self.stats.failures.saturating_add(1);
            }
        }
        self.stats.reliability_score = compute_reliability(&self.stats);
        let stale_after = now + chrono::Duration::days(7);
        let contradiction = ContradictionCheckRequest {
            target_memory_id: target_memory_id.to_string(),
            action_receipt_id: Some(receipt_id.to_string()),
            reason: format!("Negative outcome {:?} on receipt {}", kind, receipt_id),
        };
        Ok(OutcomeUpdate {
            stats: self.stats.clone(),
            contradiction_check: Some(contradiction),
            stale_after: Some(stale_after),
        })
    }
}

/// Computes reliability from Beta(2,2) posterior mean:
///   reliability = (successes + 2) / (successes + reverts + failures + 4)
///
/// Counters come from stored `u64` metadata and may sit at extreme values.
/// Widen to `u128` and use saturating adds so the sum can never overflow or
/// wrap (in debug or release), preserving the exact formula for every value a
/// `u64` counter can hold. The final score is finite and clamped to [0, 1].
fn compute_reliability(stats: &OutcomeStats) -> f64 {
    let s = stats.successes as u128;
    let r = stats.reverts as u128;
    let f = stats.failures as u128;
    let num = s.saturating_add(2);
    let den = s.saturating_add(r).saturating_add(f).saturating_add(4);
    let score = num as f64 / den as f64;
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Convenience: apply a positive outcome directly to a mutable `metadata` object.
/// Read-modify-write is NOT atomic; the caller must serialize (store lock/CAS).
pub fn apply_positive_outcome(
    metadata: &mut serde_json::Value,
    receipt_id: Option<&str>,
) -> Result<OutcomeStats, OutcomeError> {
    let mut tracker = extract_tracker(metadata);
    let stats = tracker.on_positive(receipt_id)?;
    write_tracker(metadata, &tracker)?;
    Ok(stats)
}

/// Convenience: apply a negative outcome directly to a mutable `metadata` object.
/// Read-modify-write is NOT atomic; the caller must serialize (store lock/CAS).
pub fn apply_negative_outcome(
    metadata: &mut serde_json::Value,
    kind: NegativeKind,
    receipt_id: &str,
    target_memory_id: &str,
    now: DateTime<Utc>,
) -> Result<OutcomeUpdate, OutcomeError> {
    let mut tracker = extract_tracker(metadata);
    let update = tracker.on_negative(kind, receipt_id, target_memory_id, now)?;
    write_tracker(metadata, &tracker)?;
    if let Some(stale) = update.stale_after {
        metadata["stale_after"] = json!(stale.to_rfc3339());
    }
    // Borrow the contradiction request instead of partially moving it out of
    // `update`, which is returned to the caller below.
    if let Some(cc) = update.contradiction_check.as_ref() {
        metadata["contradiction_check"] = json!(cc);
    }
    Ok(update)
}

/// Extracts an `OutcomeTracker` from `metadata.outcome_stats` and `metadata.processed_receipts`.
fn extract_tracker(metadata: &serde_json::Value) -> OutcomeTracker {
    let mut stats: OutcomeStats = metadata
        .get("outcome_stats")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    // Never trust a persisted score: recompute it from the counters so a stale
    // or manipulated stored score cannot survive a read.
    stats.reliability_score = compute_reliability(&stats);
    let processed = metadata
        .get("processed_receipts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    OutcomeTracker::new(stats, processed)
}

/// Writes the tracker back into `metadata`. Non-object metadata is an explicit
/// error (no panic, no silent data loss). Processed receipt ids are persisted
/// in sorted order via `BTreeSet` iteration.
fn write_tracker(
    metadata: &mut serde_json::Value,
    tracker: &OutcomeTracker,
) -> Result<(), OutcomeError> {
    let obj = metadata
        .as_object_mut()
        .ok_or(OutcomeError::NonObjectMetadata)?;
    obj.insert("outcome_stats".to_string(), json!(tracker.stats));
    obj.insert(
        "processed_receipts".to_string(),
        json!(tracker.processed_receipts.iter().collect::<Vec<_>>()),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn outcome_stats_serde_roundtrip() {
        let stats = OutcomeStats {
            successes: 3,
            reverts: 2,
            failures: 1,
            reliability_score: 0.625,
        };
        let value = serde_json::to_value(&stats).expect("OutcomeStats must serialize");
        let back: OutcomeStats =
            serde_json::from_value(value).expect("OutcomeStats must deserialize");
        assert_eq!(stats, back);
    }

    #[test]
    fn contradiction_request_serde_roundtrip() {
        let request = ContradictionCheckRequest {
            target_memory_id: "rule-42".to_string(),
            action_receipt_id: Some("receipt-7".to_string()),
            reason: "negative outcome".to_string(),
        };
        let value = serde_json::to_value(&request).expect("request must serialize");
        let back: ContradictionCheckRequest =
            serde_json::from_value(value).expect("request must deserialize");
        assert_eq!(request, back);
    }

    #[test]
    fn positive_reinforcement_increments_success_and_reliability() {
        let mut meta = json!({});
        let s1 = apply_positive_outcome(&mut meta, Some("rec1")).unwrap();
        assert_eq!(s1.successes, 1);
        assert_eq!(s1.reverts, 0);
        assert_eq!(s1.failures, 0);
        // Beta(2,2): (1+2)/(1+0+0+4) = 3/5 = 0.6
        assert!((s1.reliability_score - 0.6).abs() < 1e-12);

        let s2 = apply_positive_outcome(&mut meta, Some("rec2")).unwrap();
        assert_eq!(s2.successes, 2);
        // (2+2)/(2+0+0+4) = 4/6 = 2/3
        assert!((s2.reliability_score - 2.0 / 3.0).abs() < 1e-12);

        let s3 = apply_positive_outcome(&mut meta, Some("rec1")).unwrap();
        assert_eq!(s3.successes, 2, "duplicate receipt must be idempotent");
        assert!((s3.reliability_score - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn negative_revert_decrements_reliability_and_flags_stale() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap();
        let mut meta = json!({
            "outcome_stats": { "successes": 3, "reverts": 0, "failures": 0, "reliability_score": 0.8333333333333333 },
        });
        let update =
            apply_negative_outcome(&mut meta, NegativeKind::Revert, "rec-rev1", "rule-42", now)
                .unwrap();
        assert_eq!(update.stats.successes, 3);
        assert_eq!(update.stats.reverts, 1);
        assert_eq!(update.stats.failures, 0);
        // (3+2)/(3+1+0+4) = 5/8 = 0.625
        assert!((update.stats.reliability_score - 0.625).abs() < 1e-12);
        assert!(update.contradiction_check.is_some());
        assert_eq!(
            update
                .contradiction_check
                .as_ref()
                .unwrap()
                .target_memory_id,
            "rule-42"
        );
        assert_eq!(update.stale_after.unwrap(), now + chrono::Duration::days(7));

        let update2 =
            apply_negative_outcome(&mut meta, NegativeKind::Revert, "rec-rev1", "rule-42", now)
                .unwrap();
        assert_eq!(update2.stats.successes, 3);
        assert_eq!(update2.stats.reverts, 1);
        assert!(update2.contradiction_check.is_none());
        assert!(update2.stale_after.is_none());
    }

    #[test]
    fn negative_failed_attempt_decrements_reliability() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap();
        let mut meta = json!({
            "outcome_stats": { "successes": 2, "reverts": 0, "failures": 0, "reliability_score": 0.6666666666666666 },
        });
        let update = apply_negative_outcome(
            &mut meta,
            NegativeKind::FailedAttempt,
            "rec-fail1",
            "proc-7",
            now,
        )
        .unwrap();
        assert_eq!(update.stats.successes, 2);
        assert_eq!(update.stats.reverts, 0);
        assert_eq!(update.stats.failures, 1);
        // (2+2)/(2+0+1+4) = 4/7
        assert!((update.stats.reliability_score - 4.0 / 7.0).abs() < 1e-12);
        assert!(update.contradiction_check.is_some());
    }

    #[test]
    fn reliability_bounded_finite() {
        let all_fail = OutcomeStats {
            successes: 0,
            reverts: 0,
            failures: 10,
            reliability_score: 0.0,
        };
        let rel = compute_reliability(&all_fail);
        assert!(rel.is_finite() && (0.0..=1.0).contains(&rel));

        let all_success = OutcomeStats {
            successes: 10,
            reverts: 0,
            failures: 0,
            reliability_score: 0.0,
        };
        let rel2 = compute_reliability(&all_success);
        assert!(rel2.is_finite() && (0.0..=1.0).contains(&rel2));
    }

    #[test]
    fn extreme_counters_never_overflow_wrap_or_panic() {
        let max = u64::MAX;
        let all_success = OutcomeStats {
            successes: max,
            reverts: 0,
            failures: 0,
            reliability_score: 0.0,
        };
        // Exact Beta(2,2) with u64::MAX successes: (MAX+2)/(MAX+4) rounds to 1.0
        // in f64; must be finite and exactly 1.0 — never a wrapped 0.
        assert_eq!(compute_reliability(&all_success), 1.0);

        let extreme = OutcomeStats {
            successes: max,
            reverts: max,
            failures: max,
            reliability_score: 0.0,
        };
        let rel = compute_reliability(&extreme);
        assert!(rel.is_finite());
        assert!((0.0..=1.0).contains(&rel));
        // (MAX+2)/(3*MAX+4) ~= 1/3
        assert!((rel - 1.0 / 3.0).abs() < 1e-9);

        let all_negative = OutcomeStats {
            successes: 0,
            reverts: max,
            failures: max,
            reliability_score: 0.0,
        };
        let rel2 = compute_reliability(&all_negative);
        assert!(rel2.is_finite() && rel2 > 0.0 && rel2 < 1.0);
    }

    #[test]
    fn counters_monotonic_no_overflow() {
        let mut meta = json!({});
        for i in 0..1000 {
            apply_positive_outcome(&mut meta, Some(&format!("rec{}", i))).unwrap();
        }
        let stats = extract_tracker(&meta).stats();
        assert_eq!(stats.successes, 1000);
        assert!(stats.reliability_score > 0.99);
    }

    #[test]
    fn empty_ids_rejected_fail_closed() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap();
        let mut meta = json!({});
        let before = meta.clone();

        assert_eq!(
            apply_positive_outcome(&mut meta, None),
            Err(OutcomeError::EmptyReceiptId)
        );
        assert_eq!(
            apply_positive_outcome(&mut meta, Some("")),
            Err(OutcomeError::EmptyReceiptId)
        );
        assert_eq!(
            apply_positive_outcome(&mut meta, Some("   ")),
            Err(OutcomeError::EmptyReceiptId)
        );
        assert_eq!(meta, before, "rejected outcomes must not mutate metadata");

        assert_eq!(
            apply_negative_outcome(&mut meta, NegativeKind::Revert, "", "rule", now),
            Err(OutcomeError::EmptyReceiptId)
        );
        assert_eq!(
            apply_negative_outcome(&mut meta, NegativeKind::Revert, "r", "", now),
            Err(OutcomeError::EmptyTargetMemoryId)
        );
        assert_eq!(meta, before);
    }

    #[test]
    fn non_object_metadata_is_error_not_panic() {
        let mut meta = json!(42);
        assert_eq!(
            apply_positive_outcome(&mut meta, Some("r")),
            Err(OutcomeError::NonObjectMetadata)
        );
        assert_eq!(meta, json!(42));

        let mut meta = json!("not-an-object");
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap();
        assert_eq!(
            apply_negative_outcome(&mut meta, NegativeKind::Revert, "r", "m", now),
            Err(OutcomeError::NonObjectMetadata)
        );
        assert_eq!(meta, json!("not-an-object"));
    }

    #[test]
    fn processed_receipts_persist_deterministically() {
        let mut meta = json!({});
        for id in ["zeta", "alpha", "middle", "beta"] {
            apply_positive_outcome(&mut meta, Some(id)).unwrap();
        }
        let persisted = meta["processed_receipts"].as_array().unwrap();
        let ids: Vec<&str> = persisted.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(ids, vec!["alpha", "beta", "middle", "zeta"]);
    }

    #[test]
    fn extract_tracker_recomputes_stale_stored_score() {
        let meta = json!({
            "outcome_stats": { "successes": 0, "reverts": 0, "failures": 10, "reliability_score": 0.999 },
        });
        let tracker = extract_tracker(&meta);
        let expected = compute_reliability(&OutcomeStats {
            successes: 0,
            reverts: 0,
            failures: 10,
            reliability_score: 0.0,
        });
        assert!((tracker.stats().reliability_score - expected).abs() < 1e-12);
        assert!((tracker.stats().reliability_score - 0.999).abs() > 1e-6);
    }

    #[test]
    fn duplicate_receipt_idempotent_positive() {
        let mut meta = json!({});
        let s1 = apply_positive_outcome(&mut meta, Some("rec-dup")).unwrap();
        let s2 = apply_positive_outcome(&mut meta, Some("rec-dup")).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn duplicate_receipt_idempotent_negative() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap();
        let mut meta = json!({});
        let u1 = apply_negative_outcome(&mut meta, NegativeKind::Revert, "rec-neg", "rule-x", now)
            .unwrap();
        let u2 = apply_negative_outcome(&mut meta, NegativeKind::Revert, "rec-neg", "rule-x", now)
            .unwrap();
        assert_eq!(u1.stats, u2.stats);
        assert!(u2.contradiction_check.is_none());
        assert!(u2.stale_after.is_none());
    }

    #[test]
    fn stale_after_written_to_metadata() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap();
        let mut meta = json!({});
        apply_negative_outcome(&mut meta, NegativeKind::Revert, "rec-stale", "proc-1", now)
            .unwrap();
        let stale = meta.get("stale_after").and_then(|v| v.as_str());
        assert!(stale.is_some());
        let parsed: DateTime<Utc> = stale.unwrap().parse().unwrap();
        assert_eq!(parsed, now + chrono::Duration::days(7));
    }
}
