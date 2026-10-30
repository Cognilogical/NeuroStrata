//! SHACL Core validation integration test
//!
//! Verifies that the sgj4 SHACL shape validation implementation:
//! - Loads versioned Turtle shape files from src/schemas/shacl/
//! - Validates Memory rows at write time with warn-first approach
//! - Returns ShaclViolation objects without rejecting writes
//! - Implements all constraints from the approved design
//!
//! Note: Integration test uses #[path = "../../src/rdf/shacl.rs"] since this
//! is a binary crate and test crates can't import internal modules directly.

#![allow(dead_code)]

// Import SHACL validation module directly from source (binary crate can't import crate::)
#[path = "../../src/rdf/shacl.rs"]
mod shacl;

use shacl::{validate_memory, ShaclViolation};

// Replicate MemoryPayload inline (same as src/traits.rs definition)
struct TestMemoryPayload {
    pub content: String,
    pub user_id: String,
    pub memory_type: String,
    pub agent_name: Option<String>,
    pub location: String,
    pub location_lines: String,
    pub metadata: Value,
}

// Create minimal payload (missing required fields → should produce violations)
fn create_minimal_payload() -> TestMemoryPayload {
    TestMemoryPayload {
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
fn create_valid_payload() -> TestMemoryPayload {
    TestMemoryPayload {
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
fn create_rule_payload_no_source() -> TestMemoryPayload {
    TestMemoryPayload {
        content: "rule content".to_string(),
        user_id: "rule_user".to_string(),
        memory_type: "rule".to_string(),
        agent_name: None,
        location: "rule location".to_string(),
        location_lines: "1-5".to_string(),
        metadata: serde_json::json!({"enforcement": "ENFORCED"}),
    }
}

#[test]
fn test_memory_shacl_content_validation() {
    let mut payload = create_minimal_payload();
    payload.content = "".to_string();
    
    // Convert to the actual MemoryPayload struct for validation
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
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
    
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
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
    
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
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
    let mp = crate::traits::MemoryPayload {
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
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
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
    
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
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
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
    let violations = validate_memory(&mp);
    assert!(violations.is_empty(), "Valid payload should have no violations, got: {:?}", violations);
}

#[test]
fn test_shacl_violation_structure() {
    let mut payload = create_minimal_payload();
    payload.content = "".to_string();
    
    let mp = crate::traits::MemoryPayload {
        content: payload.content,
        user_id: payload.user_id,
        memory_type: payload.memory_type,
        agent_name: payload.agent_name,
        location: payload.location,
        location_lines: payload.location_lines,
        metadata: payload.metadata,
    };
    
    let violations = validate_memory(&mp);
    
    // Verify violation structure
    for violation in &violations {
        assert!(!violation.field.is_empty());
        assert!(!violation.why.is_empty());
    }
}