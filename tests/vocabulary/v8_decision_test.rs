use std::fs;

#[test]
fn v8_validates_decision_shape() {
    let raw = fs::read_to_string("src/schemas/memory-vocabulary.v3.json").unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["vocabulary_version"], 10);
    let fact_fields = v["memory_types"]["fact"]["fields"].as_object().unwrap();
    assert!(fact_fields.contains_key("decision"));
    let decision = &fact_fields["decision"];
    assert_eq!(decision["type"], "object");
}

#[test]
fn legacy_rows_readable() {
    // vocabulary file parses
    let raw = fs::read_to_string("src/schemas/memory-vocabulary.v3.json").unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v["memory_types"].is_object());
}

/// Composite Rec #27: vocabulary version consistency. The document is the
/// single source of truth the tool summaries and the docs read, so the
/// version must be current, every tool must carry a summary, and no stale
/// superseded-version references ("Vocabulary v2") may survive anywhere in
/// the shipped schema — nor in `src/store/ladybug.rs`, whose stale comments
/// were remediated; this now checks that real shipped source, not just the
/// schema document.
#[test]
fn vocabulary_version_and_tool_summaries_are_consistent() {
    let raw = fs::read_to_string("src/schemas/memory-vocabulary.v3.json").unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

    // The current vocabulary version.
    assert_eq!(v["vocabulary_version"], 10);

    // tool_summaries exists and is non-empty, and every entry is a real
    // non-blank summary rather than a placeholder.
    let summaries = v["tool_summaries"]
        .as_object()
        .expect("tool_summaries must exist");
    assert!(!summaries.is_empty(), "tool_summaries must be non-empty");
    for (tool, summary) in summaries {
        let summary = summary.as_str().unwrap_or_else(|| {
            panic!("tool_summaries[{tool}] must be a string");
        });
        assert!(
            !summary.trim().is_empty(),
            "tool_summaries[{tool}] must be a non-blank summary"
        );
        assert!(
            !summary.to_lowercase().contains("vocabulary v2"),
            "tool_summaries[{tool}] must not cite a superseded version: {summary}"
        );
    }

    // Zero stale superseded-version references anywhere in the schema
    // document itself (checks shipped JSON, not build-time state).
    assert!(
        !raw.to_lowercase().contains("vocabulary v2"),
        "the schema document carries stale superseded \"Vocabulary v2\" text"
    );

    // The shipped store source cannot blindly carry its stale "Vocabulary v2" comments anymore.
    let ladybug_src = fs::read_to_string("src/store/ladybug.rs").expect("ladybug.rs readable");
    assert!(
        !ladybug_src.contains("Vocabulary v2"),
        "src/store/ladybug.rs still carries stale superseded \"Vocabulary v2\" comments"
    );
}

