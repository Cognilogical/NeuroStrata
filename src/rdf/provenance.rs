use chrono::DateTime;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ALLOWED_ORIGINS: [&str; 4] = ["user", "agent", "system", "mesh"];
pub const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
pub const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
pub const PROV_WAS_INVALIDATED_BY: &str = "http://www.w3.org/ns/prov#wasInvalidatedBy";

/// Resolve one metadata field with nested-provenance fallback. The first
/// non-null top-level value wins; otherwise the value under
/// `metadata.provenance` is used. JSON null counts as absent at both levels,
/// so a null top-level field cannot shadow a populated nested one.
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

/// Canonical string form of a source value, shared by provenance projection
/// and epistemic evidence checks. Structured `{kind, ref, captured_at}`
/// objects serialize as `kind:ref:captured_at`; any other non-empty object
/// serializes as compact JSON. Null and empty objects yield no source.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeProvenance {
    pub source: Option<String>,
    pub extractor: String,
    pub origin: String,
    pub confidence: f64,
    pub valid_from: i64,
    pub valid_to: i64,
    pub generation: i64,
    /// Top-level metadata carried verbatim (untyped JSON).
    #[serde(default)]
    pub generated_by: Option<String>,
    #[serde(default)]
    pub verified_by: Option<String>,
    #[serde(default)]
    pub epistemic_tier: Option<String>,
    #[serde(default)]
    pub stale_after: Option<String>,
    #[serde(default)]
    pub not_definition: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceWarning {
    pub target: String,
    pub field: String,
    pub message: String,
}

pub fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => {
                out.push(ch);
            }
            _ => {
                for b in ch.encode_utf8(&mut [0; 4]).as_bytes() {
                    out.push('%');
                    out.push_str(&format!("{:02X}", b));
                }
            }
        }
    }
    out
}

pub fn memory_assertion_iri(
    namespace: &str,
    memory_id: &str,
    predicate_slug: &str,
    object_discriminator: usize,
    assertion_version: i64,
) -> String {
    format!(
        "urn:neurostrata:assertion:{}:memory:{}:{}:{}:{}",
        percent_encode_segment(namespace),
        percent_encode_segment(memory_id),
        percent_encode_segment(predicate_slug),
        object_discriminator,
        assertion_version
    )
}

pub fn edge_assertion_iri(
    namespace: &str,
    relation: &str,
    from_id: &str,
    to_id: &str,
    generation: i64,
    valid_from: i64,
    ordinal: Option<usize>,
) -> String {
    let base = format!(
        "urn:neurostrata:assertion:{}:edge:{}:{}:{}:g{}:v{}",
        percent_encode_segment(namespace),
        percent_encode_segment(relation),
        percent_encode_segment(from_id),
        percent_encode_segment(to_id),
        generation,
        valid_from
    );
    match ordinal {
        Some(ord) => format!("{}:{}", base, ord),
        None => base,
    }
}

pub fn extract_node_provenance(
    metadata: &serde_json::Value,
    default_generation: i64,
) -> NodeProvenance {
    let prov_obj = metadata.get("provenance").and_then(|v| v.as_object());
    // A single resolution rule for every provenance field: top-level
    // non-null first, then the nested provenance object.
    let source_final = resolved_source(metadata);
    let extractor = prov_obj
        .and_then(|p| p.get("extractor"))
        .and_then(|v| v.as_str())
        .unwrap_or("manual")
        .to_string();
    let origin = prov_obj
        .and_then(|p| p.get("origin"))
        .and_then(|v| v.as_str())
        .unwrap_or("system")
        .to_string();
    let confidence = prov_obj
        .and_then(|p| p.get("confidence"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let valid_from = metadata
        .get("valid_from")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let valid_to = metadata
        .get("valid_to")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let generation = prov_obj
        .and_then(|p| p.get("generation"))
        .and_then(|v| v.as_i64())
        .or_else(|| metadata.get("generation").and_then(|v| v.as_i64()))
        .unwrap_or(default_generation);
    // Nested provenance compatibility: Capability C fields may live under
    // `metadata.provenance` (the structured node-provenance block) or at the
    // top level of `metadata`. Top-level wins when present, otherwise fall
    // back to the nested object so existing rows keep working.
    let generated_by = resolve_str(metadata, "generated_by").map(str::to_string);
    let verified_by = resolve_str(metadata, "verified_by").map(str::to_string);
    let epistemic_tier = resolve_str(metadata, "epistemic_tier").map(str::to_string);
    let stale_after = resolve_str(metadata, "stale_after").map(str::to_string);
    let not_definition = resolve_field(metadata, "not_definition").cloned();
    NodeProvenance {
        source: source_final,
        extractor,
        origin,
        confidence,
        valid_from,
        valid_to,
        generation,
        generated_by,
        verified_by,
        epistemic_tier,
        stale_after,
        not_definition,
    }
}

pub fn validate_node_provenance(target: &str, prov: &NodeProvenance) -> Vec<ProvenanceWarning> {
    let mut warnings = Vec::new();
    if !prov.confidence.is_finite() || prov.confidence < 0.0 || prov.confidence > 1.0 {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "confidence".to_string(),
            message: format!("confidence {} not in [0,1]", prov.confidence),
        });
    }
    if !ALLOWED_ORIGINS.contains(&prov.origin.as_str()) {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "origin".to_string(),
            message: format!("origin '{}' not allowed", prov.origin),
        });
    }
    if prov.valid_to != 0 && prov.valid_to < prov.valid_from {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "valid_to".to_string(),
            message: "valid_to < valid_from".to_string(),
        });
    }
    if prov.source.as_ref().map_or(false, |s| s.trim().is_empty()) {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "source".to_string(),
            message: "source is empty".to_string(),
        });
    }
    if prov.extractor.trim().is_empty() {
        warnings.push(ProvenanceWarning {
            target: target.to_string(),
            field: "extractor".to_string(),
            message: "extractor is empty".to_string(),
        });
    }
    warnings
}

pub fn validate_assertion_iri(iri: &str) -> bool {
    // Full shape: urn : neurostrata : assertion : ns : (memory|edge) : ...
    // Anchor by position so a namespace or ID named "memory" or "edge"
    // elsewhere in the IRI cannot confuse the validator.
    let parts: Vec<&str> = iri.split(':').collect();
    if parts.get(0) != Some(&"urn")
        || parts.get(1) != Some(&"neurostrata")
        || parts.get(2) != Some(&"assertion")
    {
        return false;
    }
    let Some(tail) = parts.get(3..) else {
        return false;
    };
    if tail.is_empty() {
        return false;
    }
    let has_g = tail.iter().any(|p| p.starts_with('g') && p.len() > 1);
    let has_v = tail.iter().any(|p| p.starts_with('v') && p.len() > 1);
    let filled = |idx: usize| tail.get(idx).map(|p| !p.is_empty()).unwrap_or(false);
    if tail.get(1) == Some(&"memory") {
        // Expected: ns, "memory", id, slug, disc, version => 6 after prefix.
        // Namespace, memory id and predicate slug must all be present.
        return tail.len() >= 6 && filled(0) && filled(2) && filled(3);
    }
    if tail.get(1) == Some(&"edge") {
        // Expected: ns, "edge", rel, from, to, g..., v... => 6+ after prefix
        return tail.len() >= 6
            && has_g
            && has_v
            && filled(0)
            && filled(2)
            && filled(3)
            && filled(4);
    }
    false
}

/// Exact Capability C epistemic tier vocabulary.
///
/// Absence defaults: an agent-generated claim (no `generated_by` absent but
/// agent_name present) reads as `unverified_agent`; otherwise the legacy
/// unverified semantics are preserved (`EpistemicTier::Unverified`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicTier {
    UnverifiedAgent,
    EmpiricalVerified,
    FormalVerified,
    HumanGoverned,
    /// Backward-compatible bucket for metadata that predates this enum or
    /// carries no tier information at all.
    Unverified,
}

impl EpistemicTier {
    /// Canonical lowercase identifier for this tier.
    pub fn as_str(&self) -> &'static str {
        match self {
            EpistemicTier::UnverifiedAgent => "unverified_agent",
            EpistemicTier::EmpiricalVerified => "empirical_verified",
            EpistemicTier::FormalVerified => "formal_verified",
            EpistemicTier::HumanGoverned => "human_governed",
            EpistemicTier::Unverified => "unverified",
        }
    }

    /// Classify a memory's tier from metadata (top-level or nested
    /// provenance), using agent_name as the fallback signal when no recognised
    /// tier is present. An unknown explicit string must not strip attribution
    /// from an agent-generated claim: it takes the same path as absence.
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

/// Validates epistemic metadata invariants, fail-closed: every rule break
/// produces a warning the caller (validate_memory) must surface.
///
/// Rules (Capability C):
///   * verified tiers (empirical_verified / formal_verified / human_governed)
///     require nonempty `verified_by`;
///   * autonomous agent claims (unverified_agent generated by an agent) require
///     `generated_by != verified_by` when both are present;
///   * formal_verified requires formal-kernel/certificate evidence (source that
///     names a certificate/kernel artifact);
///   * human_governed requires authenticated human owner/admin evidence
///     (source naming an owner/admin), not an arbitrary string;
///   * empirical_verified requires test/command/action receipt evidence
///     (source naming a receipt/test/command).
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
                message: "stale_after must be an RFC3339 timestamp or a non-negative Unix epoch"
                    .to_string(),
            });
        }
    }

    warnings
}

/// Parse `stale_after` (ISO-8601 / RFC3339 string or Unix epoch seconds) and
/// return whether the memory is stale at the supplied `now`. Malformed input
/// returns `None` (caller decides fail-closed vs fail-open).
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

/// Exact `[DEPRECATED — SUPERSEDED BY <id>: <reason>]` banner for a
/// `metadata.deprecation` object (spec §4.2.3). Returns `None` unless `status`
/// is exactly `"deprecated"`. A missing or malformed reason renders as an
/// empty slot — never a fabricated explanation — and never panics.
pub fn deprecation_banner(deprecation: &Value) -> Option<String> {
    let obj = deprecation.as_object()?;
    if obj.get("status").and_then(Value::as_str) != Some("deprecated") {
        return None;
    }
    let id = obj
        .get("superseded_by")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    Some(format!("[DEPRECATED — SUPERSEDED BY {id}: {reason}]"))
}

/// Structured Capability C presentation for one memory or concept.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryPresentation {
    /// Structured negative boundary (string, array, or bool `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_definition: Option<Value>,
    /// Exact banner, present only when the row declares `status: "deprecated"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecation_banner: Option<String>,
}

/// Resolve a Capability C field from top-level metadata, a nested concept
/// object, or the provenance block, in that order.
fn presentation_field<'a>(metadata: &'a Value, key: &str) -> Option<&'a Value> {
    resolve_field(metadata, key).or_else(|| {
        metadata
            .get("concept")
            .and_then(|c| c.get(key))
            .filter(|v| !v.is_null())
    })
}

/// Compute the structured presentation fields shared by every recall surface.
pub fn memory_presentation(metadata: &Value) -> MemoryPresentation {
    let not_definition = presentation_field(metadata, "not_definition")
        .filter(|v| !v.is_null())
        .cloned();
    let deprecation_banner =
        presentation_field(metadata, "deprecation").and_then(deprecation_banner);
    MemoryPresentation {
        not_definition,
        deprecation_banner,
    }
}

/// Deterministic prefix: deprecation banner first, then the negative boundary.
pub fn presentation_prefix(metadata: &Value) -> String {
    let presentation = memory_presentation(metadata);
    let mut prefix = String::new();
    if let Some(banner) = &presentation.deprecation_banner {
        prefix.push_str(banner);
        prefix.push('\n');
    }
    if let Some(boundary) = &presentation.not_definition {
        prefix.push_str(&not_definition_banner(boundary));
        prefix.push('\n');
    }
    prefix
}

/// Prepend the deterministic presentation prefix to rendered content.
pub fn decorate_content(content: &str, metadata: &Value) -> String {
    let prefix = presentation_prefix(metadata);
    if prefix.is_empty() {
        content.to_string()
    } else {
        format!("{prefix}{content}")
    }
}

/// Attach structured presentation fields to a handler's JSON row and decorate
/// its rendered content field in place. All recall surfaces call this so a
/// deprecated or bounded row reads identically however the agent reached it.
pub fn attach_presentation(value: &mut Value, metadata: &Value, content_key: &str) {
    let presentation = memory_presentation(metadata);
    if let Some(obj) = value.as_object_mut() {
        if let Some(banner) = presentation.deprecation_banner {
            obj.insert("deprecation_banner".to_string(), Value::String(banner));
        }
        if let Some(boundary) = presentation.not_definition {
            obj.insert("not_definition".to_string(), boundary);
        }
        if let Some(content) = obj.get(content_key).and_then(Value::as_str) {
            let decorated = decorate_content(content, metadata);
            obj.insert(content_key.to_string(), Value::String(decorated));
        }
    }
}

/// Render a `not_definition` explanatory banner: structured negative boundary
/// string/list, or bool `false` semantics.
pub fn not_definition_banner(not_definition: &Value) -> String {
    match not_definition {
        Value::String(s) if !s.trim().is_empty() => format!("NOT DEFINITION: {s}"),
        Value::Array(arr) => {
            let items: Vec<&str> = arr
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if items.is_empty() {
                // An empty or all-nonstring boundary has no text to show;
                // never emit a dangling "NOT DEFINITION (boundary): " prefix.
                "NOT DEFINITION: unknown".to_string()
            } else {
                format!("NOT DEFINITION (boundary): {}", items.join(", "))
            }
        }
        Value::Bool(false) => "NOT DEFINITION: false (unbounded)".to_string(),
        _ => "NOT DEFINITION: unknown".to_string(),
    }
}

/// Emit the five Capability C epistemic fields as typed literals on one
/// RDF-star assertion node. Shared by the memory and edge projections so the
/// two shapes can never drift; values are rendered deterministically.
fn push_epistemic_assertion_quads(
    quads: &mut Vec<Quad>,
    assertion: &NamedOrBlankNode,
    graph: &GraphName,
    prov: &NodeProvenance,
) {
    use crate::rdf::namespace::ns_predicate;
    let xsd_str = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#string".to_string());
    let push = |quads: &mut Vec<Quad>, predicate: &str, value: String| {
        quads.push(Quad {
            subject: assertion.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate(predicate)),
            object: Term::Literal(Literal::new_typed_literal(value, xsd_str.clone())),
            graph_name: graph.clone(),
        });
    };
    if let Some(value) = &prov.generated_by {
        push(quads, "generatedBy", value.clone());
    }
    if let Some(value) = &prov.verified_by {
        push(quads, "verifiedBy", value.clone());
    }
    if let Some(value) = &prov.epistemic_tier {
        push(quads, "epistemicTier", value.clone());
    }
    if let Some(value) = &prov.stale_after {
        push(quads, "staleAfter", value.clone());
    }
    if let Some(value) = &prov.not_definition {
        let literal = match value {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_else(|_| "unknown".to_string()),
        };
        push(quads, "notDefinition", literal);
    }
}

pub fn project_memory_rdf_star(
    namespace: &str,
    id: &str,
    payload: &crate::traits::MemoryPayload,
    tier: crate::rdf::types::Tier,
    generation: u64,
) -> (Vec<oxrdf::Quad>, Vec<ProvenanceWarning>) {
    use crate::rdf::namespace::{field_predicate, graph_iri, memory_iri, ns_predicate};
    let s = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(memory_iri(id, None)));
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(graph_iri(
        namespace, tier, generation,
    )));
    let xsd_str = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#string".to_string());
    let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer".to_string());
    let xsd_dbl = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#double".to_string());
    let rdf_type =
        NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string());
    let rdf_reifies = NamedNode::new_unchecked(RDF_REIFIES.to_string());

    let mut quads = Vec::new();
    let mut base = Vec::new();

    let push_str_quad = |base: &mut Vec<Quad>, pred: &str, val: String| {
        base.push(Quad {
            subject: s.clone(),
            predicate: NamedNode::new_unchecked(pred.to_string()),
            object: Term::Literal(Literal::new_typed_literal(val, xsd_str.clone())),
            graph_name: graph.clone(),
        });
    };

    push_str_quad(
        &mut base,
        &field_predicate("content"),
        payload.content.clone(),
    );
    push_str_quad(
        &mut base,
        &field_predicate("user_id"),
        payload.user_id.clone(),
    );
    push_str_quad(
        &mut base,
        &field_predicate("memory_type"),
        payload.memory_type.clone(),
    );
    if let Some(agent) = &payload.agent_name {
        push_str_quad(&mut base, &field_predicate("agent_name"), agent.clone());
    }

    // domain
    if let Some(domain) = payload.metadata.get("domain") {
        match domain {
            serde_json::Value::String(sval) => {
                push_str_quad(&mut base, &ns_predicate("domain"), sval.clone());
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    if let Some(sval) = v.as_str() {
                        push_str_quad(&mut base, &ns_predicate("domain"), sval.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    // conceptIri
    if let Some(concept) = payload.metadata.get("concept_iri") {
        let iris: Vec<String> = match concept {
            serde_json::Value::String(sval) => vec![sval.clone()],
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => vec![],
        };
        for iri in iris {
            if !iri.trim().is_empty() {
                base.push(Quad {
                    subject: s.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("conceptIri")),
                    object: Term::NamedNode(NamedNode::new_unchecked(iri)),
                    graph_name: graph.clone(),
                });
            }
        }
    }
    // facet
    if let Some(facets) = payload.metadata.get("facets").and_then(|v| v.as_array()) {
        for f in facets {
            if let Some(sval) = f.as_str() {
                push_str_quad(&mut base, &ns_predicate("facet"), sval.to_string());
            }
        }
    }
    // supersedes / superseded_by provenance relations (not opaque JSON)
    if let Some(sup) = payload.metadata.get("supersedes") {
        if let Some(sval) = sup.as_str() {
            base.push(Quad {
                subject: s.clone(),
                predicate: NamedNode::new_unchecked(PROV_WAS_DERIVED_FROM.to_string()),
                object: Term::NamedNode(NamedNode::new_unchecked(sval.to_string())),
                graph_name: graph.clone(),
            });
        }
    }
    if let Some(sup_by) = payload.metadata.get("superseded_by") {
        if let Some(sval) = sup_by.as_str() {
            base.push(Quad {
                subject: s.clone(),
                predicate: NamedNode::new_unchecked(PROV_WAS_INVALIDATED_BY.to_string()),
                object: Term::NamedNode(NamedNode::new_unchecked(sval.to_string())),
                graph_name: graph.clone(),
            });
        }
    }

    // rdf:type Memory
    base.push(Quad {
        subject: s.clone(),
        predicate: rdf_type.clone(),
        object: Term::NamedNode(NamedNode::new_unchecked(
            "urn:neurostrata:Memory".to_string(),
        )),
        graph_name: graph.clone(),
    });

    let prov = extract_node_provenance(&payload.metadata, generation as i64);
    let mut warnings = validate_node_provenance(&format!("urn:neurostrata:memory:{}", id), &prov);
    warnings.extend(validate_epistemic_metadata(
        &payload.metadata,
        payload.agent_name.as_deref(),
    ));

    let assertion_iri = memory_assertion_iri(namespace, id, "memory", 0, prov.generation);
    let assertion_subj =
        NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(assertion_iri.clone()));

    for b in &base {
        // emit base
        quads.push(b.clone());
        // assertion block for this base quad
        let triple_term = Term::Triple(Box::new(Triple::new(
            b.subject.clone(),
            b.predicate.clone(),
            b.object.clone(),
        )));
        // rdf:type Assertion
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: rdf_type.clone(),
            object: Term::NamedNode(NamedNode::new_unchecked(
                "urn:neurostrata:ns#Assertion".to_string(),
            )),
            graph_name: b.graph_name.clone(),
        });
        // rdf:reifies << ( s p o ) >>
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: rdf_reifies.clone(),
            object: triple_term,
            graph_name: b.graph_name.clone(),
        });
        // provenance fields
        if let Some(src) = &prov.source {
            if !src.trim().is_empty() {
                quads.push(Quad {
                    subject: assertion_subj.clone(),
                    predicate: NamedNode::new_unchecked(ns_predicate("source")),
                    object: Term::Literal(Literal::new_typed_literal(src.clone(), xsd_str.clone())),
                    graph_name: b.graph_name.clone(),
                });
            }
        }
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("extractor")),
            object: Term::Literal(Literal::new_typed_literal(
                prov.extractor.clone(),
                xsd_str.clone(),
            )),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("origin")),
            object: Term::Literal(Literal::new_typed_literal(
                prov.origin.clone(),
                xsd_str.clone(),
            )),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("confidence")),
            object: Term::Literal(Literal::new_typed_literal(
                format!("{}", prov.confidence),
                xsd_dbl.clone(),
            )),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("validFrom")),
            object: Term::Literal(Literal::new_typed_literal(
                format!("{}", prov.valid_from),
                xsd_int.clone(),
            )),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("validTo")),
            object: Term::Literal(Literal::new_typed_literal(
                format!("{}", prov.valid_to),
                xsd_int.clone(),
            )),
            graph_name: b.graph_name.clone(),
        });
        quads.push(Quad {
            subject: assertion_subj.clone(),
            predicate: NamedNode::new_unchecked(ns_predicate("generation")),
            object: Term::Literal(Literal::new_typed_literal(
                format!("{}", prov.generation),
                xsd_int.clone(),
            )),
            graph_name: b.graph_name.clone(),
        });
    }
    // Epistemic metadata describes the assertion itself, so it is emitted
    // once per assertion node rather than repeated for every reified triple.
    push_epistemic_assertion_quads(&mut quads, &assertion_subj, &graph, &prov);
    (quads, warnings)
}

pub fn project_edge_rdf_star(
    namespace: &str,
    from: &str,
    relation: &str,
    to: &str,
    prov: &NodeProvenance,
    ordinal: Option<usize>,
    graph_name: oxrdf::GraphName,
) -> (Vec<oxrdf::Quad>, Vec<ProvenanceWarning>) {
    use crate::rdf::namespace::relation_predicate;
    let s = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
        "urn:neurostrata:memory:{}",
        from
    )));
    let o_named = NamedNode::new_unchecked(format!("urn:neurostrata:memory:{}", to));
    let o_term = Term::NamedNode(o_named.clone());
    let xsd_str = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#string".to_string());
    let xsd_int = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer".to_string());
    let xsd_dbl = NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#double".to_string());
    let rdf_type =
        NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string());
    let rdf_reifies = NamedNode::new_unchecked(RDF_REIFIES.to_string());
    let ns_predicate = |n: &str| NamedNode::new_unchecked(format!("urn:neurostrata:ns#{}", n));

    let base = Quad {
        subject: s.clone(),
        predicate: NamedNode::new_unchecked(relation_predicate(relation)),
        object: o_term.clone(),
        graph_name: graph_name.clone(),
    };

    let assertions = edge_assertion_iri(
        namespace,
        relation,
        from,
        to,
        prov.generation,
        prov.valid_from,
        ordinal,
    );
    let assertion_subj = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(assertions));

    let mut quads = Vec::new();
    quads.push(base.clone());

    // assertion block
    let triple_term = Term::Triple(Box::new(Triple::new(
        s.clone(),
        base.predicate.clone(),
        o_term.clone(),
    )));
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: rdf_type.clone(),
        object: Term::NamedNode(NamedNode::new_unchecked(
            "urn:neurostrata:ns#Assertion".to_string(),
        )),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: rdf_reifies.clone(),
        object: triple_term,
        graph_name: graph_name.clone(),
    });
    if let Some(src) = &prov.source {
        if !src.trim().is_empty() {
            quads.push(Quad {
                subject: assertion_subj.clone(),
                predicate: ns_predicate("source"),
                object: Term::Literal(Literal::new_typed_literal(src.clone(), xsd_str.clone())),
                graph_name: graph_name.clone(),
            });
        }
    }
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("extractor"),
        object: Term::Literal(Literal::new_typed_literal(
            prov.extractor.clone(),
            xsd_str.clone(),
        )),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("origin"),
        object: Term::Literal(Literal::new_typed_literal(
            prov.origin.clone(),
            xsd_str.clone(),
        )),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("confidence"),
        object: Term::Literal(Literal::new_typed_literal(
            format!("{}", prov.confidence),
            xsd_dbl.clone(),
        )),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("validFrom"),
        object: Term::Literal(Literal::new_typed_literal(
            format!("{}", prov.valid_from),
            xsd_int.clone(),
        )),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("validTo"),
        object: Term::Literal(Literal::new_typed_literal(
            format!("{}", prov.valid_to),
            xsd_int.clone(),
        )),
        graph_name: graph_name.clone(),
    });
    quads.push(Quad {
        subject: assertion_subj.clone(),
        predicate: ns_predicate("generation"),
        object: Term::Literal(Literal::new_typed_literal(
            format!("{}", prov.generation),
            xsd_int.clone(),
        )),
        graph_name: graph_name.clone(),
    });
    push_epistemic_assertion_quads(&mut quads, &assertion_subj, &graph_name, prov);

    let mut warnings = validate_node_provenance(
        &format!("urn:neurostrata:edge:{}:{}:{}", namespace, relation, from),
        prov,
    );
    // Edge assertions carry the same epistemic bundle as memory assertions,
    // so they must be judged by the same fail-closed invariants.
    let edge_metadata = serde_json::json!({
        "source": prov.source,
        "generated_by": prov.generated_by,
        "verified_by": prov.verified_by,
        "epistemic_tier": prov.epistemic_tier,
        "stale_after": prov.stale_after,
    });
    warnings.extend(validate_epistemic_metadata(&edge_metadata, None));
    (quads, warnings)
}

pub fn serialize_rdf_star(
    quads: &[oxrdf::Quad],
    format: &str,
    flatten_graphs: bool,
) -> Result<String, String> {
    if !matches!(
        format,
        "turtle-star" | "trig-star" | "n-triples-star" | "n-quads-star"
    ) {
        return Err(format!("unsupported RDF-star format: {format}"));
    }

    let mut sorted: Vec<&oxrdf::Quad> = quads.iter().collect();
    sorted.sort_by(|a, b| {
        let a_g = a.graph_name.to_string();
        let b_g = b.graph_name.to_string();
        let a_s = a.subject.to_string();
        let b_s = b.subject.to_string();
        let a_p = a.predicate.to_string();
        let b_p = b.predicate.to_string();
        let a_o = a.object.to_string();
        let b_o = b.object.to_string();
        (a_g, a_s, a_p, a_o).cmp(&(b_g, b_s, b_p, b_o))
    });

    // Use TurtleSerializer for triple formats, TriG/NQuads for quad formats.
    let is_triple_format = format == "turtle-star"
        || (flatten_graphs
            && format != "trig-star"
            && format != "n-triples-star"
            && format != "n-quads-star");
    let is_trig = format == "trig-star"
        || (!flatten_graphs
            && format != "n-quads-star"
            && format != "n-triples-star"
            && format != "turtle-star");

    if format == "n-triples-star" || (flatten_graphs && is_triple_format && format != "turtle-star")
    {
        // NTriplesSerializer over triples
        let mut serializer = oxttl::NTriplesSerializer::new().for_writer(Vec::new());
        for q in sorted {
            let t = Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone());
            serializer
                .serialize_triple(t.as_ref())
                .map_err(|e| format!("serialize: {}", e))?;
        }
        let bytes = serializer.finish();
        return String::from_utf8(bytes).map_err(|_| "utf8".to_string());
    }

    if format == "turtle-star" || (flatten_graphs && is_triple_format) {
        let mut serializer = oxttl::TurtleSerializer::new().for_writer(Vec::new());
        for q in sorted {
            let t = Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone());
            serializer
                .serialize_triple(t.as_ref())
                .map_err(|e| format!("serialize: {}", e))?;
        }
        let bytes = serializer.finish().map_err(|e| format!("finish: {}", e))?;
        return String::from_utf8(bytes).map_err(|_| "utf8".to_string());
    }

    if format == "trig-star" || is_trig {
        let mut serializer = oxttl::TriGSerializer::new().for_writer(Vec::new());
        for q in sorted {
            serializer
                .serialize_quad(q.as_ref())
                .map_err(|e| format!("serialize: {}", e))?;
        }
        let bytes = serializer.finish().map_err(|e| format!("finish: {}", e))?;
        return String::from_utf8(bytes).map_err(|_| "utf8".to_string());
    }

    // Any remaining format must be `n-quads-star`: every other combination is
    // handled by the branches above (n-triples-star/turtle-star via
    // `is_triple_format` when flattening, every non-quad format via `is_trig`
    // when graphs are preserved).
    let mut serializer = oxttl::NQuadsSerializer::new().for_writer(Vec::new());
    for q in sorted {
        // `flatten_graphs` also applies to n-quads-star: strip the graph name
        // so every quad lands in the default graph.
        let graph_name = if flatten_graphs {
            GraphName::DefaultGraph
        } else {
            q.graph_name.clone()
        };
        let flattened = Quad {
            subject: q.subject.clone(),
            predicate: q.predicate.clone(),
            object: q.object.clone(),
            graph_name,
        };
        serializer
            .serialize_quad(flattened.as_ref())
            .map_err(|e| format!("serialize: {}", e))?;
    }
    let bytes = serializer.finish();
    String::from_utf8(bytes).map_err(|_| "utf8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use oxrdf::{GraphName, NamedNode};
    use serde_json::json;

    fn sample_payload() -> MemoryPayload {
        MemoryPayload {
            content: "test content".to_string(),
            user_id: "u1".to_string(),
            memory_type: "fact".to_string(),
            agent_name: Some("agent".to_string()),
            location: "a.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: json!({"source":"manual","domain":"d","facets":["f"]}),
        }
    }

    #[test]
    fn memory_assertion_iri_format() {
        let iri = memory_assertion_iri("ns", "m1", "memory", 0, 3);
        assert!(iri.starts_with("urn:neurostrata:assertion:ns:memory:m1:memory:0:3"));
    }

    #[test]
    fn edge_assertion_iri_format() {
        let iri = edge_assertion_iri("ns", "GOVERNS", "a", "b", 2, 10, Some(1));
        assert!(iri.contains(":edge:GOVERNS:a:b:g2:v10:1"));
        let iri2 = edge_assertion_iri("ns", "RELATES_TO", "x", "y", 1, 5, None);
        assert!(iri2.contains(":edge:RELATES_TO:x:y:g1:v5") && !iri2.contains(":g1:v5:1"));
    }

    #[test]
    fn percent_encode_segment_escapes() {
        assert_eq!(percent_encode_segment("a/b"), "a%2Fb");
        assert_eq!(percent_encode_segment("hello world"), "hello%20world");
        assert_eq!(percent_encode_segment("%"), "%25");
        assert_eq!(percent_encode_segment(":"), "%3A");
    }

    #[test]
    fn validate_node_provenance_warn_first() {
        let bad = NodeProvenance {
            source: Some("".to_string()),
            extractor: "".to_string(),
            origin: "bad".to_string(),
            confidence: 1.5,
            valid_from: 10,
            valid_to: 5,
            generation: 1,
            generated_by: None,
            verified_by: None,
            epistemic_tier: None,
            stale_after: None,
            not_definition: None,
        };
        let w = validate_node_provenance("t", &bad);
        assert!(!w.is_empty());
    }

    #[test]
    fn validate_assertion_iri_memory_and_edge() {
        assert!(validate_assertion_iri(
            "urn:neurostrata:assertion:ns:memory:m:sl:0:1"
        ));
        assert!(validate_assertion_iri(
            "urn:neurostrata:assertion:ns:edge:r:a:b:g1:v0"
        ));
        assert!(!validate_assertion_iri("nope"));
    }

    #[test]
    fn validate_assertion_iri_rejects_short_inputs_without_panicking() {
        assert!(!validate_assertion_iri("urn:neurostrata:assertion"));
        assert!(!validate_assertion_iri(""));
        assert!(!validate_assertion_iri("urn:neurostrata"));
    }

    #[test]
    fn project_memory_rdf_star_emits_base_and_assertion() {
        let (qs, _w) = project_memory_rdf_star(
            "ns",
            "m1",
            &sample_payload(),
            crate::rdf::types::Tier::Domain,
            1,
        );
        assert!(!qs.is_empty());
        assert!(qs.iter().any(|q| q.predicate.as_str().contains("content")));
        assert!(qs.iter().any(|q| q.object.to_string().contains("Assertion")
            || q.predicate.as_str().contains("type")
                && q.object.to_string().contains("Assertion")));
    }

    #[test]
    fn project_edge_rdf_star_emits_base_and_assertion() {
        let prov = NodeProvenance {
            source: Some("src".to_string()),
            extractor: "e".to_string(),
            origin: "system".to_string(),
            confidence: 0.9,
            valid_from: 1,
            valid_to: 0,
            generation: 1,
            generated_by: None,
            verified_by: None,
            epistemic_tier: None,
            stale_after: None,
            not_definition: None,
        };
        let (qs, _w) = project_edge_rdf_star(
            "ns",
            "a",
            "GOVERNS",
            "b",
            &prov,
            None,
            GraphName::DefaultGraph,
        );
        assert!(!qs.is_empty());
    }

    #[test]
    fn serialize_rdf_star_formats() {
        let q = oxrdf::Quad {
            subject: NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
                "urn:test:s".to_string(),
            )),
            predicate: NamedNode::new_unchecked("urn:test:p".to_string()),
            object: Term::Literal(Literal::new_simple_literal("hello")),
            graph_name: GraphName::DefaultGraph,
        };
        let res_t = serialize_rdf_star(&[q.clone()], "turtle-star", true);
        assert!(res_t.is_ok());
        let res_n = serialize_rdf_star(&[q.clone()], "n-triples-star", true);
        assert!(res_n.is_ok());
        let res_trig = serialize_rdf_star(&[q.clone()], "trig-star", false);
        assert!(res_trig.is_ok());
        let res_nq = serialize_rdf_star(&[q.clone()], "n-quads-star", false);
        assert!(res_nq.is_ok());
    }

    // ==== Capability C: EpistemicTier classification ====

    #[test]
    fn epistemic_tier_classify_explicit_tiers() {
        let meta = json!({"epistemic_tier": "unverified_agent"});
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent")),
            EpistemicTier::UnverifiedAgent
        );

        let meta = json!({"epistemic_tier": "empirical_verified"});
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent")),
            EpistemicTier::EmpiricalVerified
        );

        let meta = json!({"epistemic_tier": "formal_verified"});
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent")),
            EpistemicTier::FormalVerified
        );

        let meta = json!({"epistemic_tier": "human_governed"});
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent")),
            EpistemicTier::HumanGoverned
        );
    }

    #[test]
    fn epistemic_tier_classify_absence_defaults() {
        // Agent-generated without explicit tier => unverified_agent
        let meta = json!({});
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent")),
            EpistemicTier::UnverifiedAgent
        );

        // Non-agent without explicit tier => unverified (legacy)
        let meta = json!({});
        assert_eq!(
            EpistemicTier::classify(&meta, None),
            EpistemicTier::Unverified
        );
    }

    #[test]
    fn epistemic_tier_classify_unknown_string() {
        let meta = json!({"epistemic_tier": "foobar"});
        // An unrecognised string keeps agent attribution instead of silently
        // degrading an agent-generated claim to the legacy unverified bucket.
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent")),
            EpistemicTier::UnverifiedAgent
        );
        assert_eq!(
            EpistemicTier::classify(&meta, None),
            EpistemicTier::Unverified
        );
    }

    #[test]
    fn epistemic_tier_as_str() {
        assert_eq!(EpistemicTier::UnverifiedAgent.as_str(), "unverified_agent");
        assert_eq!(
            EpistemicTier::EmpiricalVerified.as_str(),
            "empirical_verified"
        );
        assert_eq!(EpistemicTier::FormalVerified.as_str(), "formal_verified");
        assert_eq!(EpistemicTier::HumanGoverned.as_str(), "human_governed");
        assert_eq!(EpistemicTier::Unverified.as_str(), "unverified");
    }

    // ==== Capability C: validate_epistemic_metadata ====

    #[test]
    fn validate_epistemic_metadata_verified_tier_requires_verified_by() {
        let meta = json!({"epistemic_tier": "empirical_verified", "verified_by": ""});
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(!w.is_empty(), "empty verified_by should warn");

        let meta = json!({"epistemic_tier": "formal_verified"});
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(!w.is_empty(), "missing verified_by should warn");

        let meta = json!({
            "epistemic_tier": "human_governed",
            "verified_by": "human:owner",
            "source": "owner: kenton approved this rule"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(w.is_empty(), "genuine owner evidence should pass: {w:?}");
    }

    #[test]
    fn validate_epistemic_metadata_self_verification_rejected() {
        let meta = json!({
            "epistemic_tier": "unverified_agent",
            "generated_by": "agent-1",
            "verified_by": "agent-1"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(!w.is_empty(), "generated_by == verified_by should warn");
        assert!(w
            .iter()
            .any(|v| v.field == "verified_by" && v.message.contains("self-verification")));

        // Different values should pass
        let meta = json!({
            "epistemic_tier": "unverified_agent",
            "generated_by": "agent-1",
            "verified_by": "agent-2"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(
            w.is_empty(),
            "different generated_by/verified_by should pass"
        );
    }

    #[test]
    fn validate_epistemic_metadata_formal_verified_requires_certificate_source() {
        let meta = json!({
            "epistemic_tier": "formal_verified",
            "verified_by": "cert-1",
            "source": "some random text"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(!w.is_empty(), "missing certificate evidence should warn");
        assert!(w
            .iter()
            .any(|v| v.field == "source" && v.message.contains("formal-kernel")));

        let meta = json!({
            "epistemic_tier": "formal_verified",
            "verified_by": "cert-1",
            "source": "formal-kernel certificate"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(w.is_empty(), "certificate source should pass");
    }

    #[test]
    fn validate_epistemic_metadata_human_governed_requires_owner_admin_source() {
        let meta = json!({
            "epistemic_tier": "human_governed",
            "verified_by": "human-1",
            "source": "just a string"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(!w.is_empty(), "missing owner/admin evidence should warn");
        assert!(w
            .iter()
            .any(|v| v.field == "source" && v.message.contains("owner/admin")));

        let meta = json!({
            "epistemic_tier": "human_governed",
            "verified_by": "human-1",
            "source": "owner: kenton"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(w.is_empty(), "owner source should pass");
    }

    #[test]
    fn validate_epistemic_metadata_empirical_verified_requires_test_receipt_source() {
        let meta = json!({
            "epistemic_tier": "empirical_verified",
            "verified_by": "test-1",
            "source": "some documentation"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(!w.is_empty(), "missing test/receipt evidence should warn");
        assert!(w
            .iter()
            .any(|v| v.field == "source" && v.message.contains("test/command/action")));

        let meta = json!({
            "epistemic_tier": "empirical_verified",
            "verified_by": "test-1",
            "source": "test receipt: 12345"
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(w.is_empty(), "test receipt source should pass");
    }

    #[test]
    fn validate_epistemic_metadata_unverified_tier_no_warnings() {
        let meta = json!({"epistemic_tier": "unverified"});
        let w = validate_epistemic_metadata(&meta, None);
        assert!(w.is_empty(), "unverified tier should not warn");
    }

    // ==== Capability C: stale_after_parse ====

    #[test]
    fn stale_after_parse_rfc3339() {
        let now = 1_760_000_000; // fixed epoch for determinism
        let meta = json!("2025-10-01T00:00:00Z"); // well before now
        assert_eq!(stale_after_parse(&meta, now), Some(true));

        let meta = json!("2030-01-01T00:00:00Z"); // well after now
        assert_eq!(stale_after_parse(&meta, now), Some(false));
    }

    #[test]
    fn stale_after_parse_epoch_seconds_string() {
        let now = 1_760_000_000;
        let meta = json!("1700000000"); // before now
        assert_eq!(stale_after_parse(&meta, now), Some(true));

        let meta = json!("1800000000"); // after now
        assert_eq!(stale_after_parse(&meta, now), Some(false));
    }

    #[test]
    fn stale_after_parse_epoch_seconds_number() {
        let now = 1_760_000_000;
        let meta = json!(1700000000);
        assert_eq!(stale_after_parse(&meta, now), Some(true));

        let meta = json!(1800000000);
        assert_eq!(stale_after_parse(&meta, now), Some(false));
    }

    #[test]
    fn stale_after_parse_malformed_returns_none() {
        let now = 1_760_000_000;
        let meta = json!("not-a-date");
        assert_eq!(stale_after_parse(&meta, now), None);

        let meta = json!(["array"]);
        assert_eq!(stale_after_parse(&meta, now), None);
    }

    // ==== Capability C: deprecation_banner ====

    #[test]
    fn deprecation_banner_format() {
        let meta = json!({
            "status": "deprecated",
            "superseded_by": "mem-123",
            "reason": "replaced",
            "effective_at": 1
        });
        let banner = deprecation_banner(&meta);
        assert_eq!(
            banner.as_deref(),
            Some("[DEPRECATED — SUPERSEDED BY mem-123: replaced]")
        );
    }

    // ==== Capability C remediation: exact banner + presentation helper ====
    #[test]
    fn deprecation_banner_emits_exact_id_and_reason_and_never_fabricates_one() {
        let full = json!({
            "status": "deprecated",
            "superseded_by": "mem-9",
            "reason": "older API removed",
            "effective_at": 1700000000
        });
        assert_eq!(
            deprecation_banner(&full).as_deref(),
            Some("[DEPRECATED — SUPERSEDED BY mem-9: older API removed]")
        );
        let missing = json!({"status": "deprecated", "superseded_by": "mem-9"});
        let rendered = deprecation_banner(&missing).expect("deprecated row still gets a banner");
        assert!(
            rendered.starts_with("[DEPRECATED — SUPERSEDED BY mem-9:"),
            "deterministic when reason absent: {rendered}"
        );
        assert!(
            !rendered.contains("fabricated") && !rendered.contains("unknown"),
            "no invented reason: {rendered}"
        );
        let malformed =
            json!({"status": "deprecated", "superseded_by": "mem-9", "reason": {"nested": true}});
        let rendered = deprecation_banner(&malformed).expect("malformed reason never panics");
        assert!(
            rendered.starts_with("[DEPRECATED — SUPERSEDED BY mem-9:"),
            "deterministic when reason malformed: {rendered}"
        );
        assert_eq!(deprecation_banner(&json!({"status": "active"})), None);
    }

    #[test]
    fn memory_presentation_carries_structured_fields_and_prepends_deterministically() {
        let metadata = json!({
            "deprecation": {
                "status": "deprecated",
                "superseded_by": "mem-11",
                "reason": "retired",
                "effective_at": 1
            },
            "not_definition": "not for production",
        });
        let presentation = memory_presentation(&metadata);
        assert_eq!(
            presentation.deprecation_banner.as_deref(),
            Some("[DEPRECATED — SUPERSEDED BY mem-11: retired]")
        );
        assert_eq!(
            presentation.not_definition,
            Some(json!("not for production"))
        );
        assert_eq!(
            decorate_content("legacy body", &metadata),
            "[DEPRECATED — SUPERSEDED BY mem-11: retired]\nNOT DEFINITION: not for production\nlegacy body"
        );
    }

    // ==== Capability C: not_definition_banner ====

    #[test]
    fn not_definition_banner_string() {
        let banner = not_definition_banner(&json!("this is a boundary"));
        assert_eq!(banner, "NOT DEFINITION: this is a boundary");
    }

    #[test]
    fn not_definition_banner_array() {
        let banner = not_definition_banner(&json!(["a", "b", "c"]));
        assert_eq!(banner, "NOT DEFINITION (boundary): a, b, c");
    }

    #[test]
    fn not_definition_banner_bool_false() {
        let banner = not_definition_banner(&json!(false));
        assert_eq!(banner, "NOT DEFINITION: false (unbounded)");
    }

    #[test]
    fn not_definition_banner_unknown() {
        let banner = not_definition_banner(&json!(123));
        assert_eq!(banner, "NOT DEFINITION: unknown");
    }

    // ==== Capability C: extract_node_provenance nested provenance compatibility ====

    #[test]
    fn extract_node_provenance_top_level_wins_over_nested() {
        let meta = json!({
            "provenance": {
                "generated_by": "nested-agent",
                "verified_by": "nested-verifier",
                "epistemic_tier": "formal_verified",
                "stale_after": "2099-01-01T00:00:00Z",
                "not_definition": "nested boundary"
            },
            "generated_by": "top-agent",
            "verified_by": "top-verifier",
            "epistemic_tier": "empirical_verified",
            "stale_after": "2030-01-01T00:00:00Z",
            "not_definition": "top boundary"
        });
        let prov = extract_node_provenance(&meta, 1);
        assert_eq!(prov.generated_by, Some("top-agent".to_string()));
        assert_eq!(prov.verified_by, Some("top-verifier".to_string()));
        assert_eq!(prov.epistemic_tier, Some("empirical_verified".to_string()));
        assert_eq!(prov.stale_after, Some("2030-01-01T00:00:00Z".to_string()));
        assert_eq!(prov.not_definition, Some(json!("top boundary")));
    }

    #[test]
    fn extract_node_provenance_falls_back_to_nested_when_top_level_absent() {
        let meta = json!({
            "provenance": {
                "generated_by": "nested-agent",
                "verified_by": "nested-verifier",
                "epistemic_tier": "formal_verified",
                "stale_after": "2099-01-01T00:00:00Z",
                "not_definition": "nested boundary"
            }
        });
        let prov = extract_node_provenance(&meta, 1);
        assert_eq!(prov.generated_by, Some("nested-agent".to_string()));
        assert_eq!(prov.verified_by, Some("nested-verifier".to_string()));
        assert_eq!(prov.epistemic_tier, Some("formal_verified".to_string()));
        assert_eq!(prov.stale_after, Some("2099-01-01T00:00:00Z".to_string()));
        assert_eq!(prov.not_definition, Some(json!("nested boundary")));
    }

    #[test]
    fn extract_node_provenance_empty_nested_is_none() {
        let meta = json!({
            "provenance": {}
        });
        let prov = extract_node_provenance(&meta, 1);
        assert_eq!(prov.generated_by, None);
        assert_eq!(prov.verified_by, None);
        assert_eq!(prov.epistemic_tier, None);
        assert_eq!(prov.stale_after, None);
        assert_eq!(prov.not_definition, None);
    }

    // ==== Review findings: unified nested provenance / fail-closed tiers ====

    #[test]
    fn classify_unknown_tier_keeps_agent_attribution() {
        let meta = json!({"epistemic_tier": "totally-unknown"});
        assert_eq!(
            EpistemicTier::classify(&meta, Some("agent-1")),
            EpistemicTier::UnverifiedAgent
        );
        assert_eq!(
            EpistemicTier::classify(&meta, None),
            EpistemicTier::Unverified
        );
    }

    #[test]
    fn classify_reads_nested_provenance_tier_top_level_wins() {
        let nested = json!({"provenance": {"epistemic_tier": "formal_verified"}});
        assert_eq!(
            EpistemicTier::classify(&nested, None),
            EpistemicTier::FormalVerified
        );
        let both = json!({
            "provenance": {"epistemic_tier": "formal_verified"},
            "epistemic_tier": "empirical_verified"
        });
        assert_eq!(
            EpistemicTier::classify(&both, None),
            EpistemicTier::EmpiricalVerified
        );
    }

    #[test]
    fn epistemic_unknown_tier_string_fails_closed() {
        let meta = json!({"epistemic_tier": "verified-ish", "verified_by": "v"});
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(w.iter().any(|x| x.field == "epistemic_tier"), "{w:?}");
    }

    #[test]
    fn nested_self_verification_cannot_bypass_the_check() {
        let meta = json!({
            "epistemic_tier": "unverified_agent",
            "generated_by": "agent-1",
            "provenance": {"verified_by": "agent-1"}
        });
        let w = validate_epistemic_metadata(&meta, Some("agent"));
        assert!(
            w.iter()
                .any(|x| x.field == "verified_by" && x.message.contains("self-verification")),
            "{w:?}"
        );
    }

    #[test]
    fn nested_verified_fields_are_honored() {
        let meta = json!({
            "provenance": {
                "epistemic_tier": "empirical_verified",
                "verified_by": "test-suite@abc123",
                "source": "test receipt: cargo test"
            }
        });
        assert!(
            validate_epistemic_metadata(&meta, None).is_empty(),
            "nested provenance must satisfy the same validation"
        );
    }

    #[test]
    fn structured_source_evidence_resolves_for_all_tiers() {
        let valid = [
            json!({
                "epistemic_tier": "empirical_verified",
                "verified_by": "tester",
                "source": {"kind": "test", "ref": "cargo-test", "captured_at": "t"}
            }),
            json!({
                "epistemic_tier": "formal_verified",
                "verified_by": "cert",
                "source": {"kind": "certificate", "ref": "cert-sha256", "captured_at": "t"}
            }),
            json!({
                "epistemic_tier": "human_governed",
                "verified_by": "owner",
                "source": {"kind": "owner", "ref": "kenton", "captured_at": "t"}
            }),
            json!({
                "provenance": {
                    "epistemic_tier": "formal_verified",
                    "verified_by": "cert",
                    "source": {"kind": "certificate", "ref": "cert-sha256", "captured_at": "t"}
                }
            }),
        ];
        for meta in &valid {
            assert!(
                validate_epistemic_metadata(meta, None).is_empty(),
                "structured/nested evidence must be recognized: {meta} => {:?}",
                validate_epistemic_metadata(meta, None)
            );
        }
        let bad = json!({
            "epistemic_tier": "human_governed",
            "verified_by": "someone",
            "source": {"kind": "random", "ref": "x", "captured_at": "t"}
        });
        assert!(
            validate_epistemic_metadata(&bad, None)
                .iter()
                .any(|x| x.field == "source"),
            "structured source without owner/admin evidence must warn"
        );
    }

    #[test]
    fn malformed_stale_after_warns_in_epistemic_validation() {
        assert!(
            validate_epistemic_metadata(&json!({"stale_after": "not-a-date"}), None)
                .iter()
                .any(|x| x.field == "stale_after")
        );
        assert!(
            validate_epistemic_metadata(&json!({"stale_after": "2026-01-01T00:00:00Z"}), None)
                .is_empty()
        );
        assert!(validate_epistemic_metadata(&json!({"stale_after": 1700000000}), None).is_empty());
    }

    #[test]
    fn stale_after_parse_rejects_negative_and_overflow_epochs() {
        assert_eq!(stale_after_parse(&json!(-1), 100), None);
        assert_eq!(stale_after_parse(&json!("-1"), 100), None);
        assert_eq!(
            stale_after_parse(&json!("99999999999999999999999"), 100),
            None
        );
        assert_eq!(stale_after_parse(&json!("0"), 100), Some(true));
        assert_eq!(stale_after_parse(&json!(0), 100), Some(true));
    }

    #[test]
    fn not_definition_banner_arrays_have_no_trailing_empty_text() {
        assert_eq!(not_definition_banner(&json!([])), "NOT DEFINITION: unknown");
        assert_eq!(
            not_definition_banner(&json!([1, null, {}])),
            "NOT DEFINITION: unknown"
        );
        assert_eq!(
            not_definition_banner(&json!(["a", "", null, "  "])),
            "NOT DEFINITION (boundary): a"
        );
    }

    #[test]
    fn null_provenance_fields_are_absent_not_present() {
        let meta = json!({
            "generated_by": null,
            "provenance": {"generated_by": "nested-agent", "not_definition": null}
        });
        let prov = extract_node_provenance(&meta, 1);
        assert_eq!(prov.generated_by, Some("nested-agent".to_string()));
        assert_eq!(prov.not_definition, None);
    }

    #[test]
    fn top_level_source_wins_over_nested_provenance_source() {
        let meta = json!({
            "source": "top source",
            "provenance": {"source": "nested source"}
        });
        assert_eq!(
            extract_node_provenance(&meta, 1).source.as_deref(),
            Some("top source")
        );
    }

    #[test]
    fn assertion_iri_rejects_empty_namespace_and_id_segments() {
        assert!(!validate_assertion_iri(
            "urn:neurostrata:assertion::memory:m:sl:0:1"
        ));
        assert!(!validate_assertion_iri(
            "urn:neurostrata:assertion:ns:memory::sl:0:1"
        ));
        assert!(!validate_assertion_iri(
            "urn:neurostrata:assertion:ns:edge:r::b:g1:v0"
        ));
        assert!(validate_assertion_iri(
            "urn:neurostrata:assertion:ns:memory:m:sl:0:1"
        ));
    }

    #[test]
    fn project_memory_emits_epistemic_assertion_quads() {
        use crate::rdf::namespace::ns_predicate;
        let mut payload = sample_payload();
        payload.metadata = json!({
            "generated_by": "agent-1",
            "verified_by": "test@abc",
            "epistemic_tier": "empirical_verified",
            "stale_after": "2026-01-01T00:00:00Z",
            "not_definition": ["not a rule about docker"],
            "source": "test receipt"
        });
        let (quads, warnings) =
            project_memory_rdf_star("ns", "m1", &payload, crate::rdf::types::Tier::Domain, 1);
        assert!(warnings.is_empty(), "{warnings:?}");
        let assertion = memory_assertion_iri("ns", "m1", "memory", 0, 1);
        let on_assertion = |q: &Quad| matches!(&q.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == assertion);
        for (pred, expected) in [
            ("generatedBy", "agent-1"),
            ("verifiedBy", "test@abc"),
            ("epistemicTier", "empirical_verified"),
            ("staleAfter", "2026-01-01T00:00:00Z"),
        ] {
            let q = quads
                .iter()
                .find(|q| on_assertion(q) && q.predicate.as_str() == ns_predicate(pred))
                .unwrap_or_else(|| panic!("missing assertion quad for {pred}"));
            assert!(
                matches!(&q.object, Term::Literal(l) if l.value() == expected),
                "{pred} literal mismatch: {:?}",
                q.object
            );
        }
        assert!(quads
            .iter()
            .any(|q| { on_assertion(q) && q.predicate.as_str() == ns_predicate("notDefinition") }));
        let first = serialize_rdf_star(&quads, "trig-star", false).expect("valid trig-star");
        let second = serialize_rdf_star(&quads, "trig-star", false).expect("valid trig-star");
        assert_eq!(first, second, "projection must be deterministic");
        assert!(first.contains("generatedBy") && first.contains("epistemicTier"));
    }

    #[test]
    fn project_edge_emits_epistemic_assertion_quads_and_warnings() {
        let prov = NodeProvenance {
            source: Some("test receipt".to_string()),
            extractor: "e".to_string(),
            origin: "system".to_string(),
            confidence: 0.9,
            valid_from: 1,
            valid_to: 0,
            generation: 1,
            generated_by: Some("agent-1".to_string()),
            verified_by: Some("agent-1".to_string()),
            epistemic_tier: Some("unverified_agent".to_string()),
            stale_after: Some("2026-01-01T00:00:00Z".to_string()),
            not_definition: Some(json!(["not a rule"])),
        };
        let (quads, warnings) = project_edge_rdf_star(
            "ns",
            "a",
            "GOVERNS",
            "b",
            &prov,
            None,
            GraphName::DefaultGraph,
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.field == "verified_by" && w.message.contains("self-verification")),
            "edge epistemic validation must run: {warnings:?}"
        );
        let assertion = edge_assertion_iri("ns", "GOVERNS", "a", "b", 1, 1, None);
        let on_assertion = |q: &Quad| matches!(&q.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == assertion);
        for pred in [
            "generatedBy",
            "verifiedBy",
            "epistemicTier",
            "staleAfter",
            "notDefinition",
        ] {
            assert!(
                quads.iter().any(|q| {
                    on_assertion(q) && q.predicate.as_str() == format!("urn:neurostrata:ns#{pred}")
                }),
                "edge assertion missing quad for {pred}"
            );
        }
    }
}
