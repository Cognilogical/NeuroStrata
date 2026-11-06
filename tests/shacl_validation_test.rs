//! SHACL Core validation integration test
//!
//! Verifies that the sgj4 SHACL shape validation implementation:
//! - Loads versioned Turtle shape files from src/schemas/shacl/
//! - Validates Memory rows at write time with warn-first approach
//! - Returns ShaclViolation objects without rejecting writes
//! - Implements all constraints from the approved design
//!
//! Note: Integration test uses #[path = "../src/rdf/shacl.rs"] since this
//! is a binary crate and test crates can't import internal modules directly.

#![allow(dead_code)]

// Minimal stand-in for the crate's MemoryPayload (src/traits.rs can't compile
// standalone: it references crate::graph_read, crate::source_context and
// crate::task::intake). shacl.rs only needs this struct via crate::traits::.
// Its field surface is pinned against the real shipped struct by
// `local_memory_payload_matches_the_shipped_struct` below.
pub mod traits {
    use serde_json::Value;

    #[derive(Debug, Clone)]
    pub struct MemoryPayload {
        pub content: String,
        pub user_id: String,
        pub memory_type: String,
        pub agent_name: Option<String>,
        pub location: String,
        pub location_lines: String,
        pub metadata: Value,
    }

    /// Stand-in for src/traits.rs::has_source. Pinned against the shipped
    /// semantics so a future edit to the local copy fails the drift guard.
    pub fn has_source(metadata: &Value) -> bool {
        metadata
            .get("source")
            .map(|s| {
                s.as_str().is_some_and(|x| !x.trim().is_empty())
                    || s.as_object().is_some_and(|o| !o.is_empty())
            })
            .unwrap_or(false)
    }
}

// Import SHACL validation module directly from source (binary crate can't import crate::)
#[path = "../src/rebac.rs"]
pub mod rebac;
#[path = "../src/multimodal/mod.rs"]
pub mod multimodal;
#[path = "../src/rdf/shacl.rs"]
mod shacl;

use shacl::validate_memory;
use traits::MemoryPayload;

/// The payload surface this stand-in pins against the shipped struct.
const MEMORY_PAYLOAD_FIELDS: [&str; 7] = [
    "content",
    "user_id",
    "memory_type",
    "agent_name",
    "location",
    "location_lines",
    "metadata",
];

// Create minimal payload (missing required fields → should produce violations)
fn create_minimal_payload() -> MemoryPayload {
    MemoryPayload {
        content: "test content".to_string(),
        user_id: "test_user".to_string(),
        memory_type: "fact".to_string(),
        agent_name: None,
        location: "test location".to_string(),
        location_lines: "1-10".to_string(),
        metadata: serde_json::json!({}),
    }
}

// Create valid payload (all fields OK → should have no violations)
fn create_valid_payload() -> MemoryPayload {
    MemoryPayload {
        content: "valid content".to_string(),
        user_id: "valid_user".to_string(),
        memory_type: "fact".to_string(),
        agent_name: None,
        location: "valid location".to_string(),
        location_lines: "1-20".to_string(),
        metadata: serde_json::json!({}),
    }
}

// Create rule payload without source → should produce violation
fn create_rule_payload_no_source() -> MemoryPayload {
    MemoryPayload {
        content: "rule content".to_string(),
        user_id: "rule_user".to_string(),
        memory_type: "rule".to_string(),
        agent_name: None,
        location: "rule location".to_string(),
        location_lines: "1-5".to_string(),
        metadata: serde_json::json!({"enforcement": "ENFORCED"}),
    }
}

/// The stand-in struct is not allowed to drift from the shipped one: every
/// field the test's `traits::MemoryPayload` declares must exist, by name, in
/// `src/traits.rs`'s real `pub struct MemoryPayload` (the struct literals in
/// the helpers above keep the local side from dropping a field; this test
/// keeps the real side's field names from drifting under it).
#[test]
fn local_memory_payload_matches_the_shipped_struct() {
    let src =
        std::fs::read_to_string("src/traits.rs").expect("src/traits.rs readable");
    let start = src
        .find("pub struct MemoryPayload {")
        .expect("src/traits.rs declares pub struct MemoryPayload");
    let rest = &src[start..];
    let body_len = rest
        .find("\n}")
        .expect("the MemoryPayload struct closes");
    let body = &rest[..body_len];

    for field in MEMORY_PAYLOAD_FIELDS {
        assert!(
            body.contains(&format!("pub {field}:")),
            "tests/shacl_validation_test.rs's MemoryPayload field `{field}` \
             must exist in src/traits.rs's pub struct MemoryPayload"
        );
    }
}

#[test]
fn test_memory_shacl_content_validation() {
    let mut payload = create_minimal_payload();
    payload.content = "".to_string();
    let mp = payload;
    
    let violations = validate_memory(&mp);
    assert!(!violations.is_empty(), "Empty content should produce violation");
    
    let content_violation = violations
        .iter()
        .find(|v| v.field == "content")
        .expect("Should have content violation");
    assert!(content_violation.why.contains("content must be non-empty"));
}

#[test]
fn test_memory_shacl_user_id_validation() {
    let mut payload = create_minimal_payload();
    payload.user_id = "".to_string();
    let mp = payload;
    
    let violations = validate_memory(&mp);
    assert!(!violations.is_empty(), "Empty user_id should produce violation");
    
    let user_id_violation = violations
        .iter()
        .find(|v| v.field == "user_id")
        .expect("Should have user_id violation");
    assert!(user_id_violation.why.contains("user_id must be non-empty"));
}

#[test]
fn test_memory_shacl_memory_type_validation() {
    let mut payload = create_minimal_payload();
    payload.memory_type = "".to_string();
    let mp = payload;
    
    let violations = validate_memory(&mp);
    assert!(!violations.is_empty(), "Empty memory_type should produce violation");
    
    let memory_type_violation = violations
        .iter()
        .find(|v| v.field == "memory_type")
        .expect("Should have memory_type violation");
    assert!(memory_type_violation.why.contains("memory_type must be non-empty"));
}

#[test]
fn test_memory_shacl_metadata_object_validation() {
    let payload = create_minimal_payload();
    let mp = MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: serde_json::json!("invalid string"),
    };
    
    let violations = validate_memory(&mp);
    assert!(!violations.is_empty(), "Invalid metadata should produce violation");
    
    let metadata_violation = violations
        .iter()
        .find(|v| v.field == "metadata")
        .expect("Should have metadata violation");
    assert!(metadata_violation.why.contains("metadata must be an object or null"));
}

#[test]
fn test_memory_shacl_rule_source_validation() {
    let payload = create_rule_payload_no_source();
    let mp = payload;
    
    let violations = validate_memory(&mp);
    assert!(!violations.is_empty(), "Rule without source should produce violation");
    
    let source_violation = violations
        .iter()
        .find(|v| v.field == "metadata.source")
        .expect("Should have source violation");
    assert!(source_violation.why.contains("source is required for a rule"));
}

#[test]
fn test_memory_shacl_topology_validation() {
    let mut payload = create_minimal_payload();
    payload.memory_type = "fact".to_string();
    payload.metadata = serde_json::json!({
        "topology": {
            "component": "comp1",
            "target_system": "sys1"
            // missing channel, credential_boundary, reason - required
        }
    });
    let mp = payload;
    
    let violations = validate_memory(&mp);
    assert!(!violations.is_empty(), "Missing topology keys should produce violations");
    
    // Check for missing required topology keys
    let missing_keys = violations.iter()
        .filter(|v| v.field.starts_with("metadata.topology."))
        .count();
    assert!(missing_keys >= 3, "Should have violations for missing topology keys, got {}: {:?}", missing_keys, violations);
}

#[test]
fn test_memory_shacl_valid_payload_no_violations() {
    let payload = create_valid_payload();
    let mp = payload;
    
    let violations = validate_memory(&mp);
    assert!(violations.is_empty(), "Valid payload should have no violations, got: {:?}", violations);
}

/// Extracts the body of the function whose signature starts with `sig`.
///
/// Brace matching, not `rfind` over the remainder of the file: guessing the
/// closing brace would swallow every later function, and any anchor checked
/// against that region could be satisfied by unrelated code -- which is
/// exactly the drift this guard exists to catch.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("signature {sig:?} not found"));
    let rest = &src[start..];
    let brace_open = rest
        .find('{')
        .unwrap_or_else(|| panic!("{sig:?} opens with '{{'"));
    let mut depth = 0usize;
    for (offset, ch) in rest[brace_open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[brace_open + 1..brace_open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("{sig:?} body closes with '}}'");
}

/// Whitespace-insensitive form of a function body, so two implementations
/// that differ only in formatting compare equal.
fn squashed(body: &str) -> String {
    body.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Drift guard for the shipped `has_source` predicate in `src/traits.rs`.
///
/// Two things must hold, and both are checked against the real files on disk
/// (same pattern as `local_memory_payload_matches_the_shipped_struct`):
///
/// 1. The shipped `has_source` still expresses the string-or-object
///    provenance rule. The anchors are behavior-bearing expressions, not
///    comments: a rewrite that drops the non-empty-object branch, drops the
///    trim on a blank string, or flips the absent-key default breaks one of
///    them and fails this test.
/// 2. The stand-in this integration test exercises is the same code as the
///    shipped predicate. Without this, the test would validate a copy that
///    had already drifted away from the one `src/rdf/shacl.rs` delegates to
///    in production.
#[test]
fn shipped_has_source_exists_and_preserves_provenance_semantics() {
    let src = std::fs::read_to_string("src/traits.rs").expect("src/traits.rs readable");
    let body = fn_body(&src, "pub fn has_source");

    for anchor in [
        "as_str().is_some_and",
        "!x.trim().is_empty()",
        "as_object().is_some_and",
        "!o.is_empty()",
        "unwrap_or(false)",
    ] {
        assert!(
            body.contains(anchor),
            "shipped has_source drifted: missing behavior-bearing token \"{anchor}\"\nactual body:\n{body}",
        );
    }

    // The stand-in must be the shipped predicate, not a lookalike.
    let test_src = std::fs::read_to_string("tests/shacl_validation_test.rs")
        .expect("tests/shacl_validation_test.rs readable");
    let stand_in = fn_body(&test_src, "pub fn has_source");
    assert_eq!(
        squashed(stand_in),
        squashed(body),
        "the has_source stand-in in this test drifted from the shipped predicate in src/traits.rs",
    );
}

#[test]
fn test_shacl_violation_structure() {
    let mut payload = create_minimal_payload();
    payload.content = "".to_string();
    let mp = payload;
    
    let violations = validate_memory(&mp);
    
    // Verify violation structure
    for violation in &violations {
        assert!(!violation.field.is_empty());
        assert!(!violation.why.is_empty());
    }
}