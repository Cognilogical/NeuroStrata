//! Version-gated re-parse contract.
//!
//! The crate ships a binary target only (`src/main.rs`), so an integration
//! test cannot import the module tree. The schema fingerprint and the ingest
//! version helpers therefore live in modules deliberately free of `crate::`
//! dependencies, and are compiled here from their real paths.

#![allow(dead_code)]

#[path = "../../src/parser/generation.rs"]
mod generation;
#[path = "../../src/parser/schema.rs"]
mod schema;

use generation::{ingest_version, version_gate_tripped, PARSER_LOGIC_VERSION};
use schema::ParserSchema;

fn schema_json(a_first: bool) -> String {
    // Same semantic schema; key insertion order differs.
    if a_first {
        r#"{"languages":{"rust":{"extensions":[".rs","rs"],"queries":{"functions":"(function_item) @fn"}},"python":{"extensions":["py"],"queries":{"functions":"(function_definition) @fn"}}}}"#
    } else {
        r#"{"languages":{"python":{"extensions":["py"],"queries":{"functions":"(function_definition) @fn"}},"rust":{"extensions":["rs",".rs"],"queries":{"functions":"(function_item) @fn"}}}}"#
    }
    .to_string()
}

#[test]
fn fingerprint_is_stable_across_key_and_extension_order() {
    let a = ParserSchema::load(&schema_json(true)).expect("parses");
    let b = ParserSchema::load(&schema_json(false)).expect("parses");
    assert_eq!(a.fingerprint(), b.fingerprint());
}

#[test]
fn fingerprint_changes_when_a_query_changes() {
    let a = ParserSchema::load(&schema_json(true)).expect("parses");
    let changed = schema_json(true).replace("(function_item) @fn", "(function_item) @fn2");
    let b = ParserSchema::load(&changed).expect("parses");
    assert_ne!(a.fingerprint(), b.fingerprint());
}

#[test]
fn ingest_version_changes_with_logic_version_or_fingerprint() {
    let fp = "abc123";
    let v = ingest_version(fp);
    assert_eq!(v, ingest_version("abc123"));
    assert_ne!(v, ingest_version("def456"));
    assert!(v.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(PARSER_LOGIC_VERSION, 2);
}

#[test]
fn fingerprint_is_stable_for_empty_and_one_language_schemas() {
    let empty = ParserSchema::load(r#"{"languages":{}}"#).expect("parses");
    assert_eq!(empty.fingerprint(), empty.fingerprint());
    let one = ParserSchema::load(r#"{"languages":{"rust":{"extensions":["rs"],"queries":{"functions":"(function_item) @fn"}}}}"#).expect("parses");
    assert_eq!(one.fingerprint(), one.fingerprint());
    assert_ne!(empty.fingerprint(), one.fingerprint());
}

#[test]
fn version_gate_trips_only_when_stamp_is_missing_or_different() {
    assert!(!version_gate_tripped(Some("v1"), "v1"));
    assert!(version_gate_tripped(Some("v1"), "v2"));
    assert!(
        version_gate_tripped(None, "v1"),
        "pre-migration rows must re-parse"
    );
}
