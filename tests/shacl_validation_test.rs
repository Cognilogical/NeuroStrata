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
pub mod domain {
    pub fn normalize_domain_slug(raw: &str) -> Option<String> {
        let mut slug = String::with_capacity(raw.len());
        let mut separator_pending = false;
        for ch in raw.to_lowercase().chars() {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
                if separator_pending && !slug.is_empty() {
                    slug.push('-');
                }
                separator_pending = false;
                slug.push(ch);
            } else {
                separator_pending = true;
            }
        }
        if slug.is_empty() {
            None
        } else {
            Some(slug)
        }
    }
}

pub mod store {
    /// Dummy LadybugStore type for the test. The function `can_write_namespace` in
    /// `src/rebac.rs` references this type; it is unused in the test anyway.
    pub struct LadybugStore;

    impl LadybugStore {
        /// Stand-in for `store::ladybug::LadybugStore::new` so the rebac unit tests
        /// copied into this crate can construct the type. The namespace write gate
        /// never touches the store.
        pub fn new(
            _local_path: impl Into<std::path::PathBuf>,
            _dimensions: usize,
        ) -> Result<Self, String> {
            Ok(Self)
        }
    }
}

pub mod rdf {
    pub mod action {
        use crate::shacl::ShaclViolation;
        use crate::traits::MemoryPayload;

        pub struct ActionTypeDefinition;
        impl ActionTypeDefinition {
            pub fn from_payload(payload: &MemoryPayload) -> Result<Self, String> {
                match payload.metadata.get("action_type") {
                    Some(v) if v.is_object() => Ok(Self),
                    _ => Err("action_type object is required".to_string()),
                }
            }
            pub fn validate_definition(&self) -> Vec<ShaclViolation> {
                Vec::new()
            }
        }

        pub struct ActionReceiptPayload;
        impl ActionReceiptPayload {
            pub fn from_payload(payload: &MemoryPayload) -> Result<Self, String> {
                match payload.metadata.get("action_receipt") {
                    Some(v) if v.is_object() => Ok(Self),
                    _ => Err("action_receipt object is required".to_string()),
                }
            }
            pub fn validate_receipt(&self, _payload: &MemoryPayload) -> Vec<ShaclViolation> {
                Vec::new()
            }
        }
    }

    // Self-contained port of src/rdf/provenance.rs's Capability C epistemic
    // surface. shacl.rs is pulled into this crate via #[path], so its own inline
    // #[cfg(test)] tests run here too; those tests assert real tier behavior,
    // making a permissive empty stand-in invalid. This port must return the same
    // warnings as production for every input. The production provenance unit
    // suite remains authoritative -- keep these helpers in lockstep with it.
    pub mod provenance {
        use chrono::DateTime;
        use serde_json::Value;

        /// Resolve one metadata field with nested-provenance fallback. The first
        /// non-null top-level value wins; JSON null counts as absent at both
        /// levels so a null top-level field cannot shadow a nested one.
        fn resolve_field<'a>(metadata: &'a Value, key: &str) -> Option<&'a Value> {
            metadata.get(key).filter(|v| !v.is_null()).or_else(|| {
                metadata
                    .get("provenance")
                    .and_then(|p| p.get(key))
                    .filter(|v| !v.is_null())
            })
        }

        fn resolve_str<'a>(metadata: &'a Value, key: &str) -> Option<&'a str> {
            resolve_field(metadata, key).and_then(Value::as_str)
        }

        /// Canonical string form of a source value: structured
        /// {kind, ref, captured_at} becomes `kind:ref:captured_at`; any other
        /// non-empty object becomes compact JSON. Null/empty objects yield none.
        fn source_string(value: Option<&Value>) -> Option<String> {
            match value.filter(|v| !v.is_null())? {
                Value::String(s) => Some(s.clone()),
                Value::Object(obj) if obj.is_empty() => None,
                Value::Object(obj) => {
                    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
                    let reference = obj.get("ref").and_then(Value::as_str).unwrap_or("");
                    let captured = obj.get("captured_at").and_then(Value::as_str).unwrap_or("");
                    if kind.is_empty() && reference.is_empty() && captured.is_empty() {
                        serde_json::to_string(obj).ok()
                    } else {
                        Some(format!("{}:{}:{}", kind, reference, captured))
                    }
                }
                other => Some(other.to_string()),
            }
        }

        fn resolved_source(metadata: &Value) -> Option<String> {
            source_string(resolve_field(metadata, "source"))
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct ProvenanceWarning {
            pub target: String,
            pub field: String,
            pub message: String,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum EpistemicTier {
            UnverifiedAgent,
            EmpiricalVerified,
            FormalVerified,
            HumanGoverned,
            Unverified,
        }

        impl EpistemicTier {
            pub fn as_str(&self) -> &'static str {
                match self {
                    EpistemicTier::UnverifiedAgent => "unverified_agent",
                    EpistemicTier::EmpiricalVerified => "empirical_verified",
                    EpistemicTier::FormalVerified => "formal_verified",
                    EpistemicTier::HumanGoverned => "human_governed",
                    EpistemicTier::Unverified => "unverified",
                }
            }

            pub fn classify(metadata: &Value, agent_name: Option<&str>) -> EpistemicTier {
                let explicit = resolve_str(metadata, "epistemic_tier").map(|s| s.to_lowercase());
                let from_agent = || {
                    if agent_name.is_some() {
                        EpistemicTier::UnverifiedAgent
                    } else {
                        EpistemicTier::Unverified
                    }
                };
                match explicit.as_deref() {
                    Some("unverified_agent") => EpistemicTier::UnverifiedAgent,
                    Some("empirical_verified") => EpistemicTier::EmpiricalVerified,
                    Some("formal_verified") => EpistemicTier::FormalVerified,
                    Some("human_governed") => EpistemicTier::HumanGoverned,
                    Some("unverified") => EpistemicTier::Unverified,
                    Some(other) => {
                        tracing::warn!(tier = %other, "unknown epistemic_tier string falls back to agent attribution");
                        from_agent()
                    }
                    None => from_agent(),
                }
            }
        }

        /// Fail-closed epistemic validation: every rule break is a warning.
        pub fn validate_epistemic_metadata(
            metadata: &Value,
            agent_name: Option<&str>,
        ) -> Vec<ProvenanceWarning> {
            let mut warnings = Vec::new();
            let tier = EpistemicTier::classify(metadata, agent_name);
            // Every field below resolves through the same top-level-first rule. A
            // nested provenance block can satisfy a check, but cannot split a pair
            // (generated_by/verified_by) across levels to dodge self-verification.
            let verified_by = resolve_str(metadata, "verified_by").filter(|s| !s.trim().is_empty());
            let source = resolved_source(metadata);

            match tier {
                EpistemicTier::EmpiricalVerified
                | EpistemicTier::FormalVerified
                | EpistemicTier::HumanGoverned => {
                    if verified_by.is_none() {
                        warnings.push(ProvenanceWarning {
                            target: "metadata".to_string(),
                            field: "verified_by".to_string(),
                            message: format!(
                                "verified tier '{}' requires nonempty verified_by",
                                tier.as_str()
                            ),
                        });
                    }
                }
                EpistemicTier::UnverifiedAgent => {
                    if let (Some(gb), Some(vb)) = (
                        resolve_str(metadata, "generated_by"),
                        resolve_str(metadata, "verified_by"),
                    ) {
                        if !gb.trim().is_empty() && !vb.trim().is_empty() && gb == vb {
                            warnings.push(ProvenanceWarning {
                                target: "metadata".to_string(),
                                field: "verified_by".to_string(),
                                message: "autonomous agent claim requires generated_by != verified_by (self-verification rejected)".to_string(),
                            });
                        }
                    }
                }
                EpistemicTier::Unverified => {}
            }

            // Tier-specific evidence checks fail closed on an explicit tier string.
            // Structured {kind, ref, captured_at} sources are serialized through the
            // same canonical form used by provenance projection.
            let explicit = resolve_str(metadata, "epistemic_tier").map(|s| s.to_lowercase());
            match explicit.as_deref() {
                Some(tier @ ("formal_verified" | "human_governed" | "empirical_verified")) => {
                    let has_evidence = |needles: &[&str]| {
                        source.as_deref().map_or(false, |s| {
                            let lower = s.to_lowercase();
                            needles.iter().any(|needle| lower.contains(needle))
                        })
                    };
                    let (ok, message) = match tier {
                        "formal_verified" => (
                            has_evidence(&["certificate", "formal-kernel", "kernel"]),
                            "formal_verified requires formal-kernel/certificate evidence in metadata.source",
                        ),
                        "human_governed" => (
                            has_evidence(&["owner", "admin"]),
                            "human_governed requires authenticated human owner/admin evidence in metadata.source",
                        ),
                        _ => (
                            has_evidence(&["test", "command", "receipt"]),
                            "empirical_verified requires test/command/action receipt evidence in metadata.source",
                        ),
                    };
                    if !ok {
                        warnings.push(ProvenanceWarning {
                            target: "metadata".to_string(),
                            field: "source".to_string(),
                            message: message.to_string(),
                        });
                    }
                }
                // The two non-elevated tiers are valid vocabulary but impose no
                // evidence burden; only strings outside the exact vocabulary fail.
                Some("unverified_agent") | Some("unverified") => {}
                // An explicit string outside the exact tier vocabulary fails closed.
                Some(other) => warnings.push(ProvenanceWarning {
                    target: "metadata".to_string(),
                    field: "epistemic_tier".to_string(),
                    message: format!(
                        "unknown epistemic_tier '{}'; expected one of unverified/unverified_agent/empirical_verified/formal_verified/human_governed",
                        other
                    ),
                }),
                None => {}
            }

            // Freshness SLA: a malformed stale_after is a validation warning (the
            // gate separately skips it as advisory rather than coercing epoch 0).
            if let Some(value) = resolve_field(metadata, "stale_after") {
                if stale_after_parse(value, 0).is_none() {
                    warnings.push(ProvenanceWarning {
                        target: "metadata".to_string(),
                        field: "stale_after".to_string(),
                        message:
                            "stale_after must be an RFC3339 timestamp or a non-negative Unix epoch"
                                .to_string(),
                    });
                }
            }

            warnings
        }

        /// RFC3339 timestamp or non-negative Unix epoch seconds -> stale at `now`.
        pub fn stale_after_parse(value: &Value, now: i64) -> Option<bool> {
            // A negative epoch is malformed, not "stale since 1970" — coercing it to
            // zero would make every corrupt row look expired.
            let epoch = |secs: i64| (secs >= 0).then(|| secs < now);
            match value {
                Value::String(s) => {
                    // Try RFC3339 first, then bare epoch-seconds string.
                    if let Ok(stamp) = DateTime::parse_from_rfc3339(s) {
                        Some(stamp.timestamp() < now)
                    } else if let Ok(secs) = s.trim().parse::<i64>() {
                        epoch(secs)
                    } else {
                        None
                    }
                }
                Value::Number(n) => n.as_i64().and_then(epoch),
                _ => None,
            }
        }
    }
}

#[path = "../src/multimodal/mod.rs"]
pub mod multimodal;
#[path = "../src/rebac.rs"]
pub mod rebac;
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
    let src = std::fs::read_to_string("src/traits.rs").expect("src/traits.rs readable");
    let start = src
        .find("pub struct MemoryPayload {")
        .expect("src/traits.rs declares pub struct MemoryPayload");
    let rest = &src[start..];
    let body_len = rest.find("\n}").expect("the MemoryPayload struct closes");
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
    assert!(
        !violations.is_empty(),
        "Empty content should produce violation"
    );

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
    assert!(
        !violations.is_empty(),
        "Empty user_id should produce violation"
    );

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
    assert!(
        !violations.is_empty(),
        "Empty memory_type should produce violation"
    );

    let memory_type_violation = violations
        .iter()
        .find(|v| v.field == "memory_type")
        .expect("Should have memory_type violation");
    assert!(memory_type_violation
        .why
        .contains("memory_type must be non-empty"));
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
    assert!(
        !violations.is_empty(),
        "Invalid metadata should produce violation"
    );

    let metadata_violation = violations
        .iter()
        .find(|v| v.field == "metadata")
        .expect("Should have metadata violation");
    assert!(metadata_violation
        .why
        .contains("metadata must be an object or null"));
}

#[test]
fn test_memory_shacl_rule_source_validation() {
    let payload = create_rule_payload_no_source();
    let mp = payload;

    let violations = validate_memory(&mp);
    assert!(
        !violations.is_empty(),
        "Rule without source should produce violation"
    );

    let source_violation = violations
        .iter()
        .find(|v| v.field == "metadata.source")
        .expect("Should have source violation");
    assert!(source_violation
        .why
        .contains("source is required for a rule"));
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
    assert!(
        !violations.is_empty(),
        "Missing topology keys should produce violations"
    );

    // Check for missing required topology keys
    let missing_keys = violations
        .iter()
        .filter(|v| v.field.starts_with("metadata.topology."))
        .count();
    assert!(
        missing_keys >= 3,
        "Should have violations for missing topology keys, got {}: {:?}",
        missing_keys,
        violations
    );
}

#[test]
fn test_memory_shacl_valid_payload_no_violations() {
    let payload = create_valid_payload();
    let mp = payload;

    let violations = validate_memory(&mp);
    assert!(
        violations.is_empty(),
        "Valid payload should have no violations, got: {:?}",
        violations
    );
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

/// Drift guard for the self-contained provenance port in this harness.
///
/// shacl.rs is pulled in via #[path], so its inline Capability C unit tests
/// execute against the local stand-in rather than the shipped module. A silent
/// divergence would make those tests validate code that production never runs,
/// so every ported helper must stay identical (modulo whitespace) to its
/// counterpart in src/rdf/provenance.rs.
#[test]
fn provenance_stand_in_matches_shipped_epistemic_semantics() {
    let shipped =
        std::fs::read_to_string("src/rdf/provenance.rs").expect("src/rdf/provenance.rs readable");
    let test_src = std::fs::read_to_string("tests/shacl_validation_test.rs")
        .expect("tests/shacl_validation_test.rs readable");

    for sig in [
        "fn resolve_field",
        "fn resolve_str",
        "fn source_string",
        "fn resolved_source",
        "pub fn classify",
        "pub fn validate_epistemic_metadata",
        "pub fn stale_after_parse",
    ] {
        assert_eq!(
            squashed(fn_body(&test_src, sig)),
            squashed(fn_body(&shipped, sig)),
            "stand-in `{sig}` drifted from src/rdf/provenance.rs: the \
             shacl_validation_test provenance stand-in must match production \
             exactly so inline shacl.rs tests validate the shipped semantics",
        );
    }
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
