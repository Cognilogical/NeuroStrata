//! Evaluation suite for candidate models.
//!
//! Applies temperature scaling to `(probability, is_duplicate)` pairs and
//! computes real classification metrics: accuracy, precision, recall, F1,
//! Brier score, and negative log-likelihood. Consumed by the trainer's builtin
//! path (`train_candidate`) to emit a measurable `eval_report.json`, and fail
//! closed when a candidate cannot be scored. `evaluate_pairs` returns a fully
//! zeroed `EvalReport` on empty inputs — metrics that are mathematically
//! undefined read as 0.0 — and clamps the temperature into `[0.1, 10.0]`
//! (1.0 when non-finite) so no temperature can produce NaN/±inf scores.

use serde::{Deserialize, Serialize};

/// Metrics for a temperature-scaled evaluation of duplicate-judgment pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub pair_count: usize,
    pub accuracy: f64,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub brier_score: f64,
    pub nll: f64,
    pub temperature: f32,
}

/// Logit of a probability clamped to `[1e-6, 1 - 1e-6]`.
fn logit_of(p: f32) -> f64 {
    let pc = (p as f64).clamp(1e-6, 1.0 - 1e-6);
    (pc / (1.0 - pc)).ln()
}

/// Evaluate `(prob, is_duplicate)` pairs under temperature scaling
/// `sigmoid(logit(p) / temperature)`.
///
/// `p` is the model's probability that the candidate is a duplicate; the
/// hard decision threshold is 0.5. Metrics are 0.0 when undefined
/// (e.g. precision with no predicted positives).
pub fn evaluate_pairs(pairs: &[(f32, bool)], temperature: f32) -> EvalReport {
    // Clamp before division: temperature <= 0 or NaN would otherwise turn
    // logit/temperature into NaN or ±inf and poison every downstream metric.
    let temp = if temperature.is_finite() {
        (temperature as f64).clamp(0.1, 10.0)
    } else {
        1.0
    };
    let mut report = EvalReport {
        pair_count: pairs.len(),
        accuracy: 0.0,
        precision: 0.0,
        recall: 0.0,
        f1: 0.0,
        brier_score: 0.0,
        nll: 0.0,
        temperature: temp as f32,
    };
    if pairs.is_empty() {
        return report;
    }
    let mut tp: f64 = 0.0;
    let mut fp: f64 = 0.0;
    let mut tn: f64 = 0.0;
    let mut fn_: f64 = 0.0;
    let mut sum_brier: f64 = 0.0;
    let mut sum_nll: f64 = 0.0;
    for (p, is_dup) in pairs {
        let z = logit_of(*p) / temp;
        let pred = (1.0 / (1.0 + (-z).exp())).clamp(1e-12, 1.0 - 1e-12);
        let y = if *is_dup { 1.0 } else { 0.0 };
        match (pred >= 0.5, *is_dup) {
            (true, true) => tp += 1.0,
            (true, false) => fp += 1.0,
            (false, true) => fn_ += 1.0,
            (false, false) => tn += 1.0,
        }
        sum_brier += (pred - y) * (pred - y);
        sum_nll += -(y * pred.ln() + (1.0 - y) * (1.0 - pred).ln());
    }
    let n = pairs.len() as f64;
    report.accuracy = (tp + tn) / n;
    report.precision = if tp + fp > 0.0 { tp / (tp + fp) } else { 0.0 };
    report.recall = if tp + fn_ > 0.0 { tp / (tp + fn_) } else { 0.0 };
    report.f1 = if report.precision + report.recall > 0.0 {
        2.0 * report.precision * report.recall / (report.precision + report.recall)
    } else {
        0.0
    };
    report.brier_score = sum_brier / n;
    report.nll = sum_nll / n;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pairs_yield_zeroed_report() {
        let r = evaluate_pairs(&[], 1.0);
        assert_eq!(r.pair_count, 0);
        assert_eq!(r.accuracy, 0.0);
        assert_eq!(r.f1, 0.0);
    }

    #[test]
    fn perfect_separation_scores_full_marks() {
        let r = evaluate_pairs(&[(0.9, true), (0.1, false)], 1.0);
        assert_eq!(r.pair_count, 2);
        assert!((r.accuracy - 1.0).abs() < 1e-9);
        assert!((r.precision - 1.0).abs() < 1e-9);
        assert!((r.recall - 1.0).abs() < 1e-9);
        assert!((r.f1 - 1.0).abs() < 1e-9);
        assert!(r.brier_score < 0.05, "brier={}", r.brier_score);
        assert!(r.nll < 0.15, "nll={}", r.nll);
    }

    #[test]
    fn inverted_predictions_score_zero_accuracy() {
        let r = evaluate_pairs(&[(0.9, false), (0.1, true)], 1.0);
        assert!((r.accuracy - 0.0).abs() < 1e-9);
        assert!((r.precision - 0.0).abs() < 1e-9);
        assert!((r.recall - 0.0).abs() < 1e-9);
        assert!((r.f1 - 0.0).abs() < 1e-9);
        assert!(r.nll > 1.0, "high NLL for inverted predictions, got {}", r.nll);
    }

    #[test]
    fn temperature_changes_predictions_consistently() {
        // Same pairs, large temperature pushes overconfident predictions
        // toward 0.5 => higher Brier against extreme pseudo-labels.
        let cold = evaluate_pairs(&[(0.99, false)], 0.1);
        let warm = evaluate_pairs(&[(0.99, false)], 10.0);
        assert!(cold.nll > warm.nll, "cold nll={} warm nll={}", cold.nll, warm.nll);
        assert_eq!(cold.temperature, 0.1);
        assert_eq!(warm.temperature, 10.0);
    }

    #[test]
    fn brier_matches_manual_computation() {
        // p=0.5 => pred 0.5 regardless of T; y=1 => brier (0.5-1)^2 = 0.25.
        let r = evaluate_pairs(&[(0.5, true)], 1.0);
        assert!((r.brier_score - 0.25).abs() < 1e-9, "brier={}", r.brier_score);
    }
}
