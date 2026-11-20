//! Crate-free ranking metrics and gate evaluation for the semantic evaluation
//! release gate. Kept free of `crate::` imports so the integration test target
//! can include this file directly via `#[path]`.
//!
//! All serialized values are required to be finite; callers must run
//! [`validate_arm_metrics`] (or [`evaluate_thresholds`]) before serializing a
//! receipt.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelevanceLabel {
    pub id: String,
    pub gain: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryCategory {
    Alias,
    ExactIdentifier,
    Hierarchy,
    Adversarial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalArm {
    Baseline,
    Exact,
    FlatAliases,
    Ontology,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryMetrics {
    pub query_id: String,
    pub category: QueryCategory,
    pub ndcg_at_10: f64,
    pub recall_at_5: f64,
    pub precision_at_5: f64,
    pub forbidden_hits: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmMetrics {
    pub arm: RetrievalArm,
    pub ndcg_at_10: f64,
    pub recall_at_5: f64,
    pub precision_at_5: f64,
    pub ontology_recall_at_5: f64,
    pub p95_ms: f64,
    pub queries: Vec<QueryMetrics>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    Pass,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateViolation {
    pub code: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateReport {
    pub verdict: GateVerdict,
    pub violations: Vec<GateViolation>,
}

fn unique_ranked<'a>(ranked: &'a [String], k: usize) -> Vec<&'a str> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(ranked.len().min(k));
    for id in ranked {
        if out.len() >= k {
            break;
        }
        if seen.insert(id.as_str()) {
            out.push(id.as_str());
        }
    }
    out
}

fn relevant_ids(relevant: &[RelevanceLabel]) -> BTreeSet<&str> {
    relevant.iter().map(|l| l.id.as_str()).collect()
}

/// DCG with graded gains: sum of `(2^gain - 1) / log2(rank + 2)` over the
/// first `k` deduplicated ranked ids. Returns 0 for `k == 0`.
pub fn ndcg_at(ranked: &[String], relevant: &[RelevanceLabel], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    // Duplicate labels for one id must not inflate the ideal ranking:
    // keep the maximum gain per id for both ranked lookup and ideal DCG.
    let mut gains: BTreeMap<&str, f64> = BTreeMap::new();
    for label in relevant {
        let entry = gains.entry(label.id.as_str()).or_insert(0.0);
        let gain = label.gain as f64;
        if gain > *entry {
            *entry = gain;
        }
    }
    let gain_of = |id: &str| gains.get(id).copied().unwrap_or(0.0);
    let mut dcg = 0.0;
    for (rank, id) in unique_ranked(ranked, k).iter().enumerate() {
        let gain = gain_of(id);
        if gain > 0.0 {
            dcg += (2.0_f64.powf(gain) - 1.0) / (rank as f64 + 2.0).log2();
        }
    }
    let mut ideal: Vec<f64> = gains.into_values().collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut idcg = 0.0;
    for (rank, gain) in ideal.into_iter().take(k).enumerate() {
        if gain > 0.0 {
            idcg += (2.0_f64.powf(gain) - 1.0) / (rank as f64 + 2.0).log2();
        }
    }
    if idcg == 0.0 {
        return 0.0;
    }
    dcg / idcg
}

/// Fraction of unique relevant ids found in the first `k` deduplicated
/// ranked ids. Empty relevant set yields 0.0 (undefined; callers requiring a
/// defined value must enforce >= 1 relevant id per query).
pub fn recall_at(ranked: &[String], relevant: &[RelevanceLabel], k: usize) -> f64 {
    let rel = relevant_ids(relevant);
    if rel.is_empty() || k == 0 {
        return 0.0;
    }
    let hits = unique_ranked(ranked, k)
        .into_iter()
        .filter(|id| rel.contains(id))
        .count();
    hits as f64 / rel.len() as f64
}

/// Unique relevant ids in the first `k` deduplicated ranked ids, divided by
/// the fixed cutoff `k`.
pub fn precision_at(ranked: &[String], relevant: &[RelevanceLabel], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let rel = relevant_ids(relevant);
    let hits = unique_ranked(ranked, k)
        .into_iter()
        .filter(|id| rel.contains(id))
        .count();
    hits as f64 / k as f64
}

/// Sorted-sample percentile by nearest rank: `ceil(p / 100 * N)` with a
/// clamped 1-based rank. `p` is clamped to [0, 100]; empty input yields 0.0.
/// Non-finite samples are discarded rather than silently poisoning the
/// estimate.
pub fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut values: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p = if p.is_finite() {
        p.clamp(0.0, 100.0)
    } else {
        100.0
    };
    if values.len() == 1 {
        return values[0];
    }
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    let rank = rank.clamp(1, values.len());
    values[rank - 1]
}

/// Relative gain `(candidate - baseline) / baseline`. Returns 0.0 whenever
/// the baseline is zero or either operand is non-finite, so a positive
/// absolute gain from a zero baseline is never represented as infinity or
/// NaN and must be carried by the Recall@5 alternative gate instead.
pub fn relative_gain(baseline: f64, candidate: f64) -> f64 {
    if !baseline.is_finite() || !candidate.is_finite() || baseline == 0.0 {
        return 0.0;
    }
    (candidate - baseline) / baseline
}

/// Mean per-query metrics for an arm. `ontology_recall_at_5` is supplied by
/// the caller (the ablation lane owns the ontology-targeted query subset) and
/// is required to be finite. Empty query sets yield 0.0 aggregates.
pub fn summarize_arm(
    arm: RetrievalArm,
    queries: Vec<QueryMetrics>,
    ontology_recall_at_5: f64,
    p95_ms: f64,
) -> ArmMetrics {
    let n = queries.len() as f64;
    let mean = |f: fn(&QueryMetrics) -> f64| {
        if queries.is_empty() {
            0.0
        } else {
            queries.iter().map(f).sum::<f64>() / n
        }
    };
    ArmMetrics {
        arm,
        ndcg_at_10: mean(|q| q.ndcg_at_10),
        recall_at_5: mean(|q| q.recall_at_5),
        precision_at_5: mean(|q| q.precision_at_5),
        ontology_recall_at_5,
        p95_ms,
        queries,
    }
}

/// Fail-closed finite check for arm metrics. Returns the first offending
/// field path in the error string.
pub fn validate_arm_metrics(metrics: &ArmMetrics) -> Result<(), String> {
    let fields = [
        ("ndcg_at_10", metrics.ndcg_at_10),
        ("recall_at_5", metrics.recall_at_5),
        ("precision_at_5", metrics.precision_at_5),
        ("ontology_recall_at_5", metrics.ontology_recall_at_5),
        ("p95_ms", metrics.p95_ms),
    ];
    for (name, value) in fields {
        if !value.is_finite() {
            return Err(format!(
                "arm {:?} field {} is not finite",
                metrics.arm, name
            ));
        }
    }
    for query in &metrics.queries {
        for (name, value) in [
            ("ndcg_at_10", query.ndcg_at_10),
            ("recall_at_5", query.recall_at_5),
            ("precision_at_5", query.precision_at_5),
        ] {
            if !value.is_finite() {
                return Err(format!(
                    "query {} field {} is not finite",
                    query.query_id, name
                ));
            }
        }
    }
    Ok(())
}

fn mean_subset_recall(metrics: &ArmMetrics) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for query in &metrics.queries {
        if matches!(
            query.category,
            QueryCategory::Alias | QueryCategory::Hierarchy
        ) {
            sum += query.recall_at_5;
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

fn violation(code: &str, detail: String) -> GateViolation {
    GateViolation {
        code: code.to_string(),
        detail,
    }
}

/// Evaluate the accepted acceptance gate: arm 4 (ontology) vs arm 3 (flat
/// aliases). `derived_storage_bytes` must be the serialized arm-4
/// OntologyIndex/registry-row byte count and `fixture_payload_bytes` the
/// fixture JSON byte size; both are measured by the ablation lane. A zero
/// fixture payload byte count is rejected as `invalid_byte_counts` rather
/// than inferred. All checks are independent and violations accumulate.
pub fn evaluate_thresholds(
    arm3: &ArmMetrics,
    arm4: &ArmMetrics,
    derived_storage_bytes: u64,
    fixture_payload_bytes: u64,
    deterministic: bool,
    failed_query_ids: &[String],
) -> GateReport {
    let mut violations = Vec::new();

    for (label, arm) in [("arm3", arm3), ("arm4", arm4)] {
        if let Err(detail) = validate_arm_metrics(arm) {
            violations.push(violation("non_finite_metric", format!("{label}: {detail}")));
        }
    }

    let ndcg_rel = relative_gain(arm3.ndcg_at_10, arm4.ndcg_at_10);
    let recall_gain = arm4.recall_at_5 - arm3.recall_at_5;
    if ndcg_rel < 0.05 && recall_gain < 0.05 {
        violations.push(violation(
            "insufficient_lift",
            format!("relative nDCG@10 gain {ndcg_rel:.6} < 0.05 and Recall@5 gain {recall_gain:.6} < 0.05"),
        ));
    }

    match (mean_subset_recall(arm3), mean_subset_recall(arm4)) {
        (Some(b), Some(c)) => {
            let lift = c - b;
            if lift < 0.10 {
                violations.push(violation(
                    "insufficient_ontology_subset_lift",
                    format!("alias+hierarchy Recall@5 subset lift {lift:.6} < 0.10 (baseline {b:.6}, candidate {c:.6})"),
                ));
            }
        }
        _ => violations.push(violation(
            "insufficient_ontology_subset_lift",
            "arm3 or arm4 has no alias/hierarchy queries; subset lift is undefined and fails closed".to_string(),
        )),
    }

    let ontology_lift = arm4.ontology_recall_at_5 - arm3.ontology_recall_at_5;
    if ontology_lift < 0.10 {
        violations.push(violation(
            "insufficient_ontology_targeted_lift",
            format!("ontology-targeted Recall@5 lift {ontology_lift:.6} < 0.10"),
        ));
    }

    let precision_drop = arm3.precision_at_5 - arm4.precision_at_5;
    if precision_drop > 0.02 + 1e-9 {
        violations.push(violation(
            "precision_drop_exceeded",
            format!("Precision@5 drop {precision_drop:.6} exceeds 0.02"),
        ));
    }

    let forbidden: Vec<&str> = arm4
        .queries
        .iter()
        .flat_map(|q| q.forbidden_hits.iter().map(|h| h.as_str()))
        .collect();
    if !forbidden.is_empty() {
        violations.push(violation(
            "forbidden_identity_hit",
            format!("arm4 forbidden identity hits: {}", forbidden.join(", ")),
        ));
    }

    let overhead = arm4.p95_ms - arm3.p95_ms;
    if arm3.p95_ms <= 0.0 {
        // Zero baseline: relative overhead is undefined; fail only when the
        // absolute overhead exceeds 25 ms rather than emitting NaN/inf.
        if overhead > 25.0 {
            violations.push(violation(
                "p95_overhead_exceeded",
                format!("p95 overhead {overhead:.3} ms exceeds 25 ms with a zero baseline"),
            ));
        }
    } else {
        let relative = overhead / arm3.p95_ms;
        if overhead > 25.0 || relative > 0.10 {
            violations.push(violation(
                "p95_overhead_exceeded",
                format!(
                    "p95 overhead {overhead:.3} ms exceeds 25 ms or 10% relative ({relative:.6})"
                ),
            ));
        }
    }

    if fixture_payload_bytes == 0 {
        violations.push(violation(
            "invalid_byte_counts",
            "fixture_payload_bytes must be the measured fixture JSON byte size; zero is invalid, not inferable".to_string(),
        ));
    } else if (derived_storage_bytes as u128) * 100 >= (fixture_payload_bytes as u128) * 5 {
        violations.push(violation(
            "derived_storage_exceeded",
            format!("derived storage {derived_storage_bytes} B is >= 5% of fixture payload {fixture_payload_bytes} B"),
        ));
    }

    if !deterministic {
        violations.push(violation(
            "nondeterminism",
            "two runs produced different ranked ids or non-finite metrics".to_string(),
        ));
    }

    if !failed_query_ids.is_empty() {
        violations.push(violation(
            "failed_query",
            format!("failed queries: {}", failed_query_ids.join(", ")),
        ));
    }

    let verdict = if violations.is_empty() {
        GateVerdict::Pass
    } else {
        GateVerdict::Fail
    };
    GateReport {
        verdict,
        violations,
    }
}

/// Median per-query p95 latency across measured iterations. Input is
/// `samples[iteration][query]`; at least three iterations are required and
/// every iteration must report the same query count. Fail-closed on any
/// malformed shape.
pub fn per_query_p95_medians(samples: &[Vec<f64>]) -> Result<Vec<f64>, String> {
    if samples.len() < 3 {
        return Err(format!(
            "p95 requires at least 3 measured iterations, got {}",
            samples.len()
        ));
    }
    let query_count = samples[0].len();
    if query_count == 0 {
        return Err("p95 requires at least one query sample".to_string());
    }
    if let Some((i, row)) = samples
        .iter()
        .enumerate()
        .find(|(_, row)| row.len() != query_count)
    {
        return Err(format!(
            "iteration {i} has {} query samples, expected {query_count}",
            row.len()
        ));
    }
    Ok((0..query_count)
        .map(|q| {
            percentile(
                &samples.iter().map(|row| row[q]).collect::<Vec<f64>>(),
                50.0,
            )
        })
        .collect())
}

/// Arm-level p95 derived from per-query median latencies.
pub fn arm_p95_from_query_medians(medians: &[f64]) -> f64 {
    percentile(medians, 95.0)
}
