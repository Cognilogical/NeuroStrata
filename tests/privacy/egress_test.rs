#![allow(dead_code)]

#[path = "../../src/context/model.rs"]
mod model;
#[path = "../../src/context/source.rs"]
mod source;
#[path = "../../src/task/tool_receipts.rs"]
mod tool_receipts;
// The REAL production shareable-type predicate (crate-free module also used by
// src/synaptic_mesh/outbox.rs) — not a drifting copy of the allowlist.
#[path = "../../src/synaptic_mesh/shareable.rs"]
mod shareable;

use model::{EgressPolicy, LookupStatus};

#[test]
fn egress_denial_distinct() {
    let p = EgressPolicy::deny("no egress");
    assert!(p.enforce().is_err());
    let a = EgressPolicy::allow();
    assert!(a.enforce().is_ok());
}

#[test]
fn cross_scope_default_same_namespace() {
    assert!(source::same_namespace("ns", "ns"));
    assert!(source::cross_scope_authorized("ns", "ns", &[]));
    assert!(!source::cross_scope_authorized("ns", "other", &[]));
    assert!(source::cross_scope_authorized(
        "ns",
        "other",
        &["other".to_string()]
    ));
}

#[test]
fn lookup_status_variants() {
    assert_ne!(LookupStatus::Denied, LookupStatus::NotFound);
}

/// Spec §6 R6.4: receipts are local JSON records, never memory rows. The mesh
/// export filter selects only shareable memory types, so it cannot emit a
/// receipt — even if one is handed to it as a candidate row.
#[test]
fn receipt_events_are_local_not_mesh_exportable() {
    use tool_receipts::{ReceiptFields, ReceiptStatus};

    let receipt = ReceiptFields::new(
        "evt-egress".to_string(),
        1_700_000_001,
        "ses".to_string(),
        "t1".to_string(),
        "NeuroStrata".to_string(),
        "neurostrata_related".to_string(),
        "related".to_string(),
        "T_HIGH_BLAST".to_string(),
        "graph-first".to_string(),
        true,
        ReceiptStatus::Success,
        7,
        1,
        vec!["m1".to_string()],
        Vec::new(),
        Vec::new(),
        "raw query text must not escape",
        "raw content text must not escape",
    )
    .to_value();
    assert!(receipt.get("memory_type").is_none());
    assert!(receipt.get("eligibility").is_none());

    let candidates = vec![
        serde_json::json!({
            "memory_type": "fact",
            "eligibility": "shareable",
            "content": "ordinary shared fact"
        }),
        receipt,
    ];
    // Apply the exact production predicate the outbox worker uses.
    let exported: Vec<_> = candidates
        .iter()
        .filter(|row| {
            row.get("memory_type")
                .and_then(serde_json::Value::as_str)
                .map(shareable::is_shareable_memory_type)
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    assert_eq!(exported.len(), 1, "only the real fact row may be exported");
    assert!(
        exported.iter().all(|row| row.get("event_id").is_none()),
        "mesh export must contain zero receipt rows"
    );
}

/// Digests only: raw query/content text must never appear in a receipt, so
/// there is nothing sensitive for a mesh export to carry in the first place.
#[test]
fn receipts_never_leak_raw_query_or_content() {
    use tool_receipts::{digest, ReceiptFields, ReceiptStatus};

    let raw_query = "secret query body";
    let raw_content = "secret memory body";
    let receipt = ReceiptFields::new(
        "evt-leak".to_string(),
        1_700_000_002,
        "ses".to_string(),
        "t1".to_string(),
        "NeuroStrata".to_string(),
        "neurostrata_sparql_query".to_string(),
        "sparql_query".to_string(),
        "T_INVARIANT".to_string(),
        "ontology".to_string(),
        true,
        ReceiptStatus::Success,
        3,
        0,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        raw_query,
        raw_content,
    )
    .to_value();
    let serialized = receipt.to_string();
    assert!(!serialized.contains(raw_query));
    assert!(!serialized.contains(raw_content));
    assert_eq!(
        receipt["query_digest"].as_str(),
        Some(digest(raw_query).as_str())
    );
    assert_eq!(
        receipt["content_digest"].as_str(),
        Some(digest(raw_content).as_str())
    );
}
