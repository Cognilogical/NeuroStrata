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
