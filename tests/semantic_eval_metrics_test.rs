#[path = "../src/semantic_eval/metrics.rs"]
mod metrics;

use metrics::*;

fn label(id: &str, gain: u8) -> RelevanceLabel {
    RelevanceLabel { id: id.to_string(), gain }
}

fn query(id: &str, category: QueryCategory, ndcg: f64, recall: f64, precision: f64) -> QueryMetrics {
    QueryMetrics {
        query_id: id.to_string(),
        category,
        ndcg_at_10: ndcg,
        recall_at_5: recall,
        precision_at_5: precision,
        forbidden_hits: Vec::new(),
    }
}

fn qualifying_arms() -> (ArmMetrics, ArmMetrics) {
    let arm3 = ArmMetrics {
        arm: RetrievalArm::FlatAliases,
        ndcg_at_10: 0.60,
        recall_at_5: 0.467,
        precision_at_5: 0.62,
        ontology_recall_at_5: 0.45,
        p95_ms: 100.0,
        queries: vec![
            query("q-alias", QueryCategory::Alias, 0.55, 0.40, 0.60),
            query("q-hier", QueryCategory::Hierarchy, 0.55, 0.40, 0.60),
            query("q-adv", QueryCategory::Adversarial, 0.70, 0.60, 0.66),
        ],
    };
    let arm4 = ArmMetrics {
        arm: RetrievalArm::Ontology,
        ndcg_at_10: 0.80,
        recall_at_5: 0.633,
        precision_at_5: 0.61,
        ontology_recall_at_5: 0.65,
        p95_ms: 110.0,
        queries: vec![
            query("q-alias", QueryCategory::Alias, 0.80, 0.60, 0.60),
            query("q-hier", QueryCategory::Hierarchy, 0.80, 0.60, 0.60),
            query("q-adv", QueryCategory::Adversarial, 0.80, 0.70, 0.63),
        ],
    };
    (arm3, arm4)
}

fn gate_ok(arm3: &ArmMetrics, arm4: &ArmMetrics) -> GateReport {
    evaluate_thresholds(arm3, arm4, 4, 100, true, &[])
}

#[test]
fn ndcg_at_graded_gains_ideal_ranking_is_one() {
    let relevant = vec![label("a", 3), label("b", 2), label("c", 1)];
    let ranked = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let score = ndcg_at(&ranked, &relevant, 10);
    assert!((score - 1.0).abs() < 1e-12);
}

#[test]
fn ndcg_at_empty_ranking_is_zero() {
    let relevant = vec![label("a", 3)];
    assert_eq!(ndcg_at(&[], &relevant, 10), 0.0);
}

#[test]
fn ndcg_at_applies_rank_discount() {
    let relevant = vec![label("a", 3), label("b", 2)];
    let ranked = vec!["b".to_string(), "a".to_string()];
    let score = ndcg_at(&ranked, &relevant, 2);
    let dcg = 3.0 / 2.0_f64.log2() + 7.0 / 3.0_f64.log2();
    let idcg = 7.0 / 1.0 + 3.0 / 3.0_f64.log2();
    assert!((score - dcg / idcg).abs() < 1e-12);
    assert!(score < 1.0);
}

#[test]
fn ndcg_at_counts_duplicate_ranked_ids_once() {
    let relevant = vec![label("a", 2)];
    let ranked = vec!["a".to_string(), "a".to_string(), "a".to_string()];
    let score = ndcg_at(&ranked, &relevant, 10);
    assert!((score - 1.0).abs() < 1e-12);
}

#[test]
fn recall_at_5_with_fewer_than_five_returned_and_duplicates() {
    let relevant = vec![label("x", 1), label("y", 1), label("z", 1)];
    let ranked = vec!["x".to_string(), "x".to_string(), "y".to_string()];
    let score = recall_at(&ranked, &relevant, 5);
    assert!((score - 2.0 / 3.0).abs() < 1e-12);
}

#[test]
fn precision_at_5_uses_fixed_k_denominator_and_dedupes() {
    let relevant = vec![label("x", 1), label("y", 1), label("z", 1)];
    let ranked = vec!["x".to_string(), "x".to_string(), "y".to_string()];
    let score = precision_at(&ranked, &relevant, 5);
    assert!((score - 2.0 / 5.0).abs() < 1e-12);
}

#[test]
fn recall_at_empty_relevant_set_is_zero() {
    let ranked = vec!["x".to_string()];
    assert_eq!(recall_at(&ranked, &[], 5), 0.0);
}

#[test]
fn relative_gain_from_zero_baseline_is_never_nan_or_inf() {
    let from_zero = relative_gain(0.0, 0.5);
    assert!(from_zero.is_finite());
    assert_eq!(from_zero, 0.0);
    assert!(relative_gain(0.0, 0.0).is_finite());
    assert!((relative_gain(0.5, 0.75) - 0.5).abs() < 1e-12);
}

#[test]
fn percentile_interpolates_and_handles_edge_cases() {
    let samples = vec![10.0, 20.0, 30.0, 40.0];
    assert!((percentile(&samples, 50.0) - 20.0).abs() < 1e-12);
    assert!((percentile(&samples, 0.0) - 10.0).abs() < 1e-12);
    assert!((percentile(&samples, 100.0) - 40.0).abs() < 1e-12);
    assert_eq!(percentile(&[], 95.0), 0.0);
    assert_eq!(percentile(&[42.0], 95.0), 42.0);
}

#[test]
fn summarize_arm_computes_means_and_preserves_queries() {
    let queries = vec![
        query("q1", QueryCategory::Alias, 0.5, 0.5, 0.5),
        query("q2", QueryCategory::Hierarchy, 1.0, 1.0, 1.0),
    ];
    let arm = summarize_arm(RetrievalArm::Ontology, queries, 0.75, 123.0);
    assert!((arm.ndcg_at_10 - 0.75).abs() < 1e-12);
    assert!((arm.recall_at_5 - 0.75).abs() < 1e-12);
    assert!((arm.precision_at_5 - 0.75).abs() < 1e-12);
    assert!((arm.ontology_recall_at_5 - 0.75).abs() < 1e-12);
    assert!((arm.p95_ms - 123.0).abs() < 1e-12);
    assert_eq!(arm.queries.len(), 2);
    let empty = summarize_arm(RetrievalArm::Baseline, vec![], 0.0, 0.0);
    assert_eq!(empty.ndcg_at_10, 0.0);
    assert_eq!(empty.recall_at_5, 0.0);
}

#[test]
fn gate_passes_qualifying_arm4_result() {
    let (arm3, arm4) = qualifying_arms();
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Pass, "violations: {:?}", report.violations);
    assert!(report.violations.is_empty());
}

#[test]
fn gate_passes_via_recall_alternative_when_ndcg_flat() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.ndcg_at_10 = arm3.ndcg_at_10 * 1.01; // below 5% relative gain
    arm4.recall_at_5 = arm3.recall_at_5 + 0.10; // 10pp recall gain
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Pass, "violations: {:?}", report.violations);
}

#[test]
fn gate_rejects_insufficient_ndcg_and_recall_lift() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.ndcg_at_10 = arm3.ndcg_at_10 * 1.01;
    arm4.recall_at_5 = arm3.recall_at_5 + 0.02;
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "insufficient_lift"));
}

#[test]
fn gate_rejects_insufficient_ontology_subset_lift() {
    let (arm3, mut arm4) = qualifying_arms();
    // Flatten ontology-targeted categories below the 10pp subset floor.
    arm4.queries[0].recall_at_5 = 0.45;
    arm4.queries[1].recall_at_5 = 0.45;
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "insufficient_ontology_subset_lift"));
}

#[test]
fn gate_rejects_insufficient_ontology_targeted_lift() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.ontology_recall_at_5 = arm3.ontology_recall_at_5 + 0.05;
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "insufficient_ontology_targeted_lift"));
}

#[test]
fn gate_rejects_precision_drop_over_two_points() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.precision_at_5 = arm3.precision_at_5 - 0.03;
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "precision_drop_exceeded"));
}

#[test]
fn gate_allows_exactly_two_point_precision_drop() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.precision_at_5 = arm3.precision_at_5 - 0.02;
    let report = gate_ok(&arm3, &arm4);
    assert!(!report.violations.iter().any(|v| v.code == "precision_drop_exceeded"));
}

#[test]
fn gate_rejects_forbidden_identity_hit_in_arm4() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.queries[0].forbidden_hits = vec!["foreign-project-doc".to_string()];
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "forbidden_identity_hit"));
}

#[test]
fn gate_rejects_p95_overhead_above_25ms_even_with_low_relative() {
    let (mut arm3, mut arm4) = qualifying_arms();
    arm3.p95_ms = 400.0;
    arm4.p95_ms = 430.0; // 30ms absolute overhead, 7.5% relative
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "p95_overhead_exceeded"));
}

#[test]
fn gate_rejects_p95_overhead_above_10_percent() {
    let (mut arm3, mut arm4) = qualifying_arms();
    arm3.p95_ms = 100.0;
    arm4.p95_ms = 112.0; // 12ms absolute, 12% relative
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "p95_overhead_exceeded"));
}

#[test]
fn gate_zero_p95_baseline_fails_only_above_25ms_absolute() {
    let (mut arm3, mut arm4) = qualifying_arms();
    arm3.p95_ms = 0.0;
    arm4.p95_ms = 20.0;
    let pass = gate_ok(&arm3, &arm4);
    assert!(!pass.violations.iter().any(|v| v.code == "p95_overhead_exceeded"));
    arm4.p95_ms = 30.0;
    let fail = gate_ok(&arm3, &arm4);
    assert!(fail.violations.iter().any(|v| v.code == "p95_overhead_exceeded"));
}

#[test]
fn gate_rejects_excessive_derived_storage() {
    let (arm3, arm4) = qualifying_arms();
    let report = evaluate_thresholds(&arm3, &arm4, 5, 100, true, &[]); // exactly 5%, not below
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "derived_storage_exceeded"));
    let ok = evaluate_thresholds(&arm3, &arm4, 4, 100, true, &[]);
    assert!(!ok.violations.iter().any(|v| v.code == "derived_storage_exceeded"));
}

#[test]
fn gate_rejects_nondeterminism_and_failed_queries() {
    let (arm3, arm4) = qualifying_arms();
    let nondet = evaluate_thresholds(&arm3, &arm4, 4, 100, false, &[]);
    assert!(nondet.violations.iter().any(|v| v.code == "nondeterminism"));
    let failed = evaluate_thresholds(&arm3, &arm4, 4, 100, true, &["q-adv".to_string()]);
    assert!(failed.violations.iter().any(|v| v.code == "failed_query"));
}

#[test]
fn gate_reports_non_finite_metrics_as_violations() {
    let (arm3, mut arm4) = qualifying_arms();
    arm4.ndcg_at_10 = f64::NAN;
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "non_finite_metric"));
}

#[test]
fn serde_rejects_unknown_category_and_arm_variants() {
    let bad_category = r#"{"query_id":"q","category":"unknown_cat","ndcg_at_10":0.5,"recall_at_5":0.5,"precision_at_5":0.5,"forbidden_hits":[]}"#;
    assert!(serde_json::from_str::<QueryMetrics>(bad_category).is_err());
    let bad_arm = r#"{"arm":"nonsense","ndcg_at_10":0.5,"recall_at_5":0.5,"precision_at_5":0.5,"ontology_recall_at_5":0.5,"p95_ms":10.0,"queries":[]}"#;
    assert!(serde_json::from_str::<ArmMetrics>(bad_arm).is_err());
}

#[test]
fn serde_rejects_unknown_fields_on_strict_structs() {
    let extra = r#"{"id":"a","gain":2,"surprise":1}"#;
    assert!(serde_json::from_str::<RelevanceLabel>(extra).is_err());
    let (arm3, _) = qualifying_arms();
    let mut value = serde_json::to_value(&arm3).unwrap();
    value.as_object_mut().unwrap().insert("bogus".into(), serde_json::json!(1));
    assert!(serde_json::from_value::<ArmMetrics>(value).is_err());
}

#[test]
fn serialized_metric_values_are_finite_and_round_trip() {
    let (_, arm4) = qualifying_arms();
    let text = serde_json::to_string(&arm4).unwrap();
    assert!(!text.contains("NaN"));
    assert!(!text.contains("inf"));
    let parsed: ArmMetrics = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.queries.len(), 3);
    validate_arm_metrics(&parsed).unwrap();
}

#[test]
fn validate_arm_metrics_rejects_non_finite_per_query_values() {
    let (_, mut arm4) = qualifying_arms();
    arm4.queries[0].recall_at_5 = f64::INFINITY;
    let err = validate_arm_metrics(&arm4).unwrap_err();
    assert!(err.contains("recall_at_5"));
}

#[test]
fn gate_report_serializes_with_verdict_and_violations() {
    let (arm3, arm4) = qualifying_arms();
    let report = gate_ok(&arm3, &arm4);
    let text = serde_json::to_string(&report).unwrap();
    assert!(text.contains("\"verdict\":\"pass\""));
    let failing = evaluate_thresholds(&arm3, &arm4, 500, 100, false, &["q-adv".to_string()]);
    let text = serde_json::to_string(&failing).unwrap();
    assert!(text.contains("\"verdict\":\"fail\""));
}

#[test]
fn gate_zero_ndcg_baseline_passes_only_via_recall_alternative() {
    let (mut arm3, mut arm4) = qualifying_arms();
    arm3.ndcg_at_10 = 0.0;
    arm4.ndcg_at_10 = 0.75; // positive absolute gain from zero => relative_gain must be 0.0, not inf/NaN
    assert_eq!(relative_gain(0.0, 0.75), 0.0);
    arm4.recall_at_5 = arm3.recall_at_5 + 0.06; // >= 5pp recall alternative
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Pass, "violations: {:?}", report.violations);
}

#[test]
fn gate_rejects_zero_fixture_payload_byte_count_as_invalid() {
    let (arm3, arm4) = qualifying_arms();
    let report = evaluate_thresholds(&arm3, &arm4, 0, 0, true, &[]);
    assert_eq!(report.verdict, GateVerdict::Fail);
    assert!(report.violations.iter().any(|v| v.code == "invalid_byte_counts"));
}

#[test]
fn per_query_p95_medians_require_three_consistent_iterations() {
    let samples = vec![
        vec![10.0, 20.0],
        vec![12.0, 18.0],
        vec![14.0, 22.0],
    ];
    let medians = per_query_p95_medians(&samples).unwrap();
    assert!((medians[0] - 12.0).abs() < 1e-12);
    assert!((medians[1] - 20.0).abs() < 1e-12);
    assert!(per_query_p95_medians(&samples[..2]).is_err());
    let ragged = vec![vec![10.0], vec![11.0, 12.0], vec![13.0]];
    assert!(per_query_p95_medians(&ragged).is_err());
    assert!(per_query_p95_medians(&[vec![], vec![], vec![]]).is_err());
}

#[test]
fn arm_p95_from_query_medians_uses_percentile() {
    let medians = vec![10.0, 20.0, 30.0, 40.0];
    let p95 = arm_p95_from_query_medians(&medians);
    assert!((p95 - 40.0).abs() < 1e-12);
    assert_eq!(arm_p95_from_query_medians(&[]), 0.0);
}

#[test]
fn percentile_uses_nearest_rank_boundaries() {
    let samples = vec![10.0, 20.0, 30.0, 40.0];
    assert_eq!(percentile(&samples, 0.0), 10.0);
    assert_eq!(percentile(&samples, 25.0), 10.0); // ceil(1.00) = rank 1
    assert_eq!(percentile(&samples, 26.0), 20.0); // ceil(1.04) = rank 2
    assert_eq!(percentile(&samples, 50.0), 20.0);
    assert_eq!(percentile(&samples, 51.0), 30.0); // ceil(2.04) = rank 3
    assert_eq!(percentile(&samples, 95.0), 40.0); // ceil(3.80) = rank 4
    assert_eq!(percentile(&samples, 100.0), 40.0);
    assert_eq!(percentile(&samples, 101.0), 40.0); // clamped
    assert_eq!(percentile(&samples, -1.0), 10.0); // clamped
}

#[test]
fn ndcg_at_deduplicates_relevant_labels_using_maximum_gain() {
    let relevant = vec![label("a", 1), label("a", 3), label("b", 2)];
    let ranked = vec!["a".to_string(), "b".to_string()];
    let score = ndcg_at(&ranked, &relevant, 10);
    assert!((score - 1.0).abs() < 1e-12, "duplicate labels inflated ideal DCG: {score}");
}

#[test]
fn per_query_p95_medians_still_use_three_iteration_medians() {
    let samples = vec![
        vec![10.0, 20.0],
        vec![12.0, 18.0],
        vec![14.0, 22.0],
    ];
    let medians = per_query_p95_medians(&samples).unwrap();
    assert!((medians[0] - 12.0).abs() < 1e-12);
    assert!((medians[1] - 20.0).abs() < 1e-12);
}

/// C37: read the mixed-subset figures from the checked-in receipt instead of
/// hand-mirroring them. One mechanical chain: receipt -> this test -> gate.
fn recorded_subset_lift_numbers() -> (f64, f64, f64) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("eval/semantic/retrieval-receipt.json");
    let text = std::fs::read_to_string(&path).expect("read semantic receipt");
    let receipt: serde_json::Value = serde_json::from_str(&text).expect("strict JSON receipt");
    let detail = receipt["gate"]["violations"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["code"] == "insufficient_ontology_subset_lift")
        })
        .and_then(|row| row["detail"].as_str())
        .expect("receipt must carry the subset-lift violation detail");
    let number_after = |marker: &str| -> f64 {
        detail
            .split(marker)
            .nth(1)
            .and_then(|tail| tail.split_whitespace().next())
            .map(|token| {
                token.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
            })
            .and_then(|token| token.parse().ok())
            .unwrap_or_else(|| panic!("detail has no number after '{marker}': {detail}"))
    };
    (
        number_after("subset lift "),
        number_after("baseline "),
        number_after("candidate "),
    )
}

/// R8.1 honesty guard (additive): the recorded mixed alias+hierarchy subset
/// lift is below the 0.10 floor, so the accepted gate must still fail closed
/// with the subset-lift violation. Never weakens the existing gate tests.
#[test]
fn recorded_mixed_subset_lift_gate_fails_honestly() {
    let (lift, baseline, candidate) = recorded_subset_lift_numbers();
    let arm3 = ArmMetrics {
        arm: RetrievalArm::FlatAliases,
        ndcg_at_10: 0.55,
        recall_at_5: baseline,
        precision_at_5: 0.62,
        ontology_recall_at_5: 0.45,
        p95_ms: 100.0,
        queries: vec![
            query("q-alias", QueryCategory::Alias, 0.55, baseline, 0.60),
            query("q-hier", QueryCategory::Hierarchy, 0.55, baseline, 0.60),
        ],
    };
    let arm4 = ArmMetrics {
        arm: RetrievalArm::Ontology,
        ndcg_at_10: 0.56,
        recall_at_5: candidate,
        precision_at_5: 0.61,
        ontology_recall_at_5: 0.46,
        p95_ms: 105.0,
        queries: vec![
            query("q-alias", QueryCategory::Alias, 0.56, candidate, 0.60),
            query("q-hier", QueryCategory::Hierarchy, 0.56, candidate, 0.60),
        ],
    };
    let report = gate_ok(&arm3, &arm4);
    assert_eq!(report.verdict, GateVerdict::Fail);
    let violation = report
        .violations
        .iter()
        .find(|v| v.code == "insufficient_ontology_subset_lift")
        .expect("subset-lift violation must remain enforced");
    assert!(
        violation.detail.contains(&format!("{lift:.6}")),
        "unexpected measured lift: {}",
        violation.detail
    );
    assert!(violation.detail.contains("< 0.10"));
}
