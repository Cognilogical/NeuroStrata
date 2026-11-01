//! Phase 0 contract test — the context read-planning DTOs.
//!
//! The crate ships a binary target only (`src/main.rs`), so an integration
//! test cannot import the module tree. The DTO module is therefore compiled
//! here from its real path rather than copied: the contract under test is the
//! file the server ships, not a transcription that can drift from it.

#![allow(dead_code)]

#[path = "../../src/context/model.rs"]
mod model;

use model::{
    ContextItem, ContextPlan, ContextRequest, Coverage, DecisionLifecycle, Degradation,
    EgressPolicy, Freshness, SemanticValidation, SourceCurrency, SourceIdentity,
    CONTEXT_PLAN_SCHEMA_VERSION,
};
use serde_json::{json, Value};

fn sample_source() -> SourceIdentity {
    SourceIdentity {
        repo_id: "repo-1".into(),
        checkout_id: "checkout-1".into(),
        worktree_id: "worktree-1".into(),
        revision: "0bd6914".into(),
        dirty: true,
        relative_path: "src/context/model.rs".into(),
    }
}

fn sample_request() -> ContextRequest {
    ContextRequest {
        session_id: "ses_abc".into(),
        context_epoch: 7,
        event_id: "evt_1".into(),
        trigger: "file_touch".into(),
        source: sample_source(),
        policy: EgressPolicy {
            allowed: true,
            reason: None,
        },
    }
}

#[test]
fn context_request_round_trips_through_json() {
    let request = sample_request();

    let encoded = serde_json::to_value(&request).expect("request serializes");
    let decoded: ContextRequest = serde_json::from_value(encoded).expect("request deserializes");

    assert_eq!(decoded, request);
}

#[test]
fn context_request_uses_the_contract_field_names() {
    let encoded = serde_json::to_value(sample_request()).expect("request serializes");

    let object = encoded.as_object().expect("request is an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "context_epoch",
            "event_id",
            "policy",
            "session_id",
            "source",
            "trigger"
        ],
        "the wire field names are the contract; renaming one orphans every client"
    );

    let source = encoded["source"].as_object().expect("source is an object");
    let mut source_keys: Vec<&str> = source.keys().map(String::as_str).collect();
    source_keys.sort_unstable();
    assert_eq!(
        source_keys,
        vec![
            "checkout_id",
            "dirty",
            "relative_path",
            "repo_id",
            "revision",
            "worktree_id"
        ],
        "source identity must carry every field that separates concurrent worktrees"
    );
}

#[test]
fn context_request_preserves_a_denied_egress_policy_with_its_reason() {
    let request = ContextRequest {
        policy: EgressPolicy {
            allowed: false,
            reason: Some("remote embedder egress denied for namespace 'other'".into()),
        },
        ..sample_request()
    };

    let encoded = serde_json::to_value(&request).expect("request serializes");
    assert_eq!(encoded["policy"]["allowed"], json!(false));

    let decoded: ContextRequest = serde_json::from_value(encoded).expect("request deserializes");
    assert_eq!(decoded.policy.allowed, false);
    assert_eq!(
        decoded.policy.reason.as_deref(),
        Some("remote embedder egress denied for namespace 'other'"),
        "a denial without its reason is indistinguishable from 'not found'"
    );
}

#[test]
fn degradation_variants_have_canonical_tokens() {
    let expected = [
        (Degradation::Supported, "supported"),
        (Degradation::Unsupported, "unsupported"),
        (Degradation::Unavailable, "unavailable"),
        (Degradation::Partial, "partial"),
        (Degradation::Degraded, "degraded"),
    ];

    assert_eq!(
        Degradation::ALL.len(),
        expected.len(),
        "every degradation variant must be covered by this contract test"
    );

    for (variant, token) in expected {
        assert_eq!(
            serde_json::to_value(variant).expect("degradation serializes"),
            json!(token)
        );
        assert_eq!(Degradation::parse(token), Some(variant));
        assert_eq!(variant.as_str(), token);
    }
}

#[test]
fn degradation_never_resolves_to_the_same_state_for_different_tokens() {
    assert_ne!(Degradation::Unsupported, Degradation::Unavailable);
    assert_ne!(Degradation::Partial, Degradation::Degraded);
    assert_eq!(Degradation::parse("chaged"), None);
    assert_eq!(Degradation::parse(""), None);
}

#[test]
fn context_plan_round_trips_and_reports_truncation_and_degradation() {
    let plan = ContextPlan {
        schema_version: CONTEXT_PLAN_SCHEMA_VERSION.into(),
        event_id: "evt_1".into(),
        index_generation: 42,
        applied_scope: "namespace:NeuroStrata".into(),
        items: vec![ContextItem {
            id: "mem-1".into(),
            memory_type: "rule".into(),
            content: "Always use podman".into(),
            freshness: Some(Freshness {
                source_currency: SourceCurrency::Unchanged,
                semantic_validation: SemanticValidation::Verified,
                index_coverage: Coverage::Complete,
                decision_lifecycle: DecisionLifecycle::Accepted,
            }),
        }],
        truncation: true,
        degradation: Degradation::Partial,
    };

    let encoded = serde_json::to_value(&plan).expect("plan serializes");
    assert_eq!(
        encoded["schema_version"],
        json!(CONTEXT_PLAN_SCHEMA_VERSION)
    );
    assert_eq!(encoded["index_generation"], json!(42));
    assert_eq!(encoded["truncation"], json!(true));
    assert_eq!(encoded["degradation"], json!("partial"));
    assert_eq!(encoded["items"][0]["id"], json!("mem-1"));

    let decoded: ContextPlan = serde_json::from_value(encoded).expect("plan deserializes");
    assert_eq!(decoded, plan);
}

#[test]
fn context_plan_reports_an_unavailable_backend_as_unavailable_not_empty() {
    let plan = ContextPlan::new("evt_2", Degradation::Unavailable);

    assert_eq!(plan.schema_version, CONTEXT_PLAN_SCHEMA_VERSION);
    assert_eq!(plan.degradation, Degradation::Unavailable);
    assert!(plan.items.is_empty());
    assert!(
        !plan.truncation,
        "an unavailable backend truncated nothing; truncation must stay false"
    );

    let encoded = serde_json::to_value(&plan).expect("plan serializes");
    let decoded: ContextPlan = serde_json::from_value(encoded).expect("plan deserializes");
    assert_eq!(decoded.degradation, Degradation::Unavailable);
}

#[test]
fn freshness_states_stay_distinguishable() {
    assert_ne!(
        SourceCurrency::NotApplicable,
        SourceCurrency::Unknown,
        "'no governed path' is not the same claim as 'we could not tell'"
    );
    assert_ne!(SemanticValidation::Verified, SemanticValidation::Unreviewed);
    assert_ne!(
        SemanticValidation::Contradicted,
        SemanticValidation::Unreviewed
    );
    assert_ne!(Coverage::Complete, Coverage::Partial);
    assert_ne!(Coverage::Unavailable, Coverage::Partial);
    assert_ne!(DecisionLifecycle::Proposed, DecisionLifecycle::Accepted);
    assert_ne!(DecisionLifecycle::Superseded, DecisionLifecycle::Withdrawn);

    assert_eq!(
        SourceCurrency::parse("not_applicable"),
        Some(SourceCurrency::NotApplicable)
    );
    assert_eq!(
        SourceCurrency::parse("unknown"),
        Some(SourceCurrency::Unknown)
    );
    assert_eq!(
        SemanticValidation::parse("verified"),
        Some(SemanticValidation::Verified)
    );
    assert_eq!(Coverage::parse("unavailable"), Some(Coverage::Unavailable));
    assert_eq!(
        DecisionLifecycle::parse("superseded"),
        Some(DecisionLifecycle::Superseded)
    );
}

#[test]
fn freshness_compares_against_its_canonical_token() {
    let freshness = Freshness {
        source_currency: SourceCurrency::Changed,
        semantic_validation: SemanticValidation::Unreviewed,
        index_coverage: Coverage::Partial,
        decision_lifecycle: DecisionLifecycle::Proposed,
    };

    assert_eq!(freshness.source_currency, "changed");
    assert_eq!(freshness.semantic_validation, "unreviewed");
    assert_eq!(freshness.index_coverage, "partial");
    assert_eq!(freshness.decision_lifecycle, "proposed");

    let encoded = serde_json::to_value(freshness).expect("freshness serializes");
    assert_eq!(encoded["source_currency"], json!("changed"));
    assert_eq!(encoded["index_coverage"], json!("partial"));

    let decoded: Freshness = serde_json::from_value(encoded).expect("freshness deserializes");
    assert_eq!(decoded, freshness);
}

#[test]
fn context_item_without_freshness_omits_the_key() {
    let item = ContextItem {
        id: "mem-2".into(),
        memory_type: "fact".into(),
        content: "The daemon listens on 34343".into(),
        freshness: None,
    };

    let encoded = serde_json::to_value(&item).expect("item serializes");
    let object = encoded.as_object().expect("item is an object");
    assert!(
        !object.contains_key("freshness"),
        "an item with no computed freshness must not claim one"
    );

    let decoded: ContextItem = serde_json::from_value(encoded).expect("item deserializes");
    assert_eq!(decoded, item);
    assert!(decoded.freshness.is_none());
}

#[test]
fn every_context_item_carries_the_three_mandatory_fields() {
    let item = ContextItem {
        id: "mem-3".into(),
        memory_type: "procedure".into(),
        content: "Run the gate before pushing".into(),
        freshness: None,
    };

    let encoded = serde_json::to_value(&item).expect("item serializes");
    for field in ["id", "memory_type", "content"] {
        assert!(
            encoded.get(field).is_some(),
            "context item must carry `{field}`"
        );
    }
    let value: Value = encoded;
    assert!(value["id"].is_string());
}
