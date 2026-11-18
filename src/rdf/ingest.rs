//! Ontology-scoped bounded RDF/Turtle classifier + ingestion path (Spec §6.5,
//! v1.19.0 Capability F).
//!
//! Validates every candidate against ONLY the governed vocabulary of the
//! requested domain (closure seeded from that domain's SHACL shapes and
//! expanded under a finite `QueryBudget`), emits deterministic RDF Turtle with
//! `rdf:type` triples, runs SHACL fail-closed validation on the emitted graph
//! and on the `MemoryPayload` BEFORE any write, and commits exactly one batch
//! memory via the real `VectorStore`. Any violation anywhere aborts with a
//! typed error and zero writes.

use crate::rdf::canonical::nquads_digest;
use crate::rdf::ontology::{OntologyIndex, QueryBudget, VocabularySelectionError};
use crate::rdf::shacl::ShaclViolation;
use crate::task::intent::detect_task_intents;
use crate::traits::{Embedder, MemoryPayload, VectorStore};
use oxrdf::vocab::rdf;
use oxrdf::{Literal, NamedNode, Term, Triple};
use serde_json::Value;
use std::fmt;

/// A candidate object term: either an absolute IRI or a validated literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateObject {
    /// Absolute IRI (validated before use).
    Iri(String),
    /// Literal with optional datatype IRI (allowlisted) or BCP47 language tag.
    Literal(CandidateLiteral),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateLiteral {
    pub value: String,
    /// Absolute datatype IRI; must be in the governed XSD allowlist.
    pub datatype: Option<String>,
    /// BCP47 language tag; mutually exclusive with `datatype`.
    pub language: Option<String>,
}

/// One explicitly governed classification: subject/class/predicate/object,
/// all pre-validated absolute IRIs or validated literals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationCandidate {
    pub subject: String,
    pub class: String,
    pub predicate: String,
    pub object: CandidateObject,
}

/// Batch classification + ingestion request. A non-empty candidate list is
/// mandatory; the domain and user_id are carried explicitly.
#[derive(Debug, Clone)]
pub struct BoundedClassificationRequest {
    pub domain: String,
    pub user_id: String,
    pub candidates: Vec<ClassificationCandidate>,
    pub reason: Option<String>,
}

/// Typed, distinct failures. Fail-closed by construction: every variant is
/// produced BEFORE any store write happens.
#[derive(Debug)]
pub enum IngestError {
    /// The candidate list was empty; an empty batch is never ingested.
    EmptyBatch,
    /// Pre-allocation cap on the number of candidates in one batch.
    TooManyCandidates { count: usize, max: usize },
    /// Pre-allocation cap on a single IRI field's byte length.
    IriTooLong {
        field: &'static str,
        length: usize,
        max: usize,
    },
    /// Pre-allocation cap on a literal value's byte length.
    LiteralTooLong { length: usize, max: usize },
    UnknownDomain(String),
    BudgetExhausted { visited: usize, max_nodes: usize },
    /// The domain governs no classes or no predicates; fail closed.
    EmptyVocabulary(String),
    /// A candidate class term is not governed by the domain vocabulary.
    InventedClass(String),
    /// A candidate predicate term is not governed by the domain vocabulary.
    InventedPredicate(String),
    MalformedIri { field: &'static str, value: String },
    /// A datatype IRI outside the governed XSD allowlist.
    UnknownDatatype(String),
    InvalidLiteral { field: &'static str, why: String },
    /// SHACL validation of the emitted RDF graph reported violations.
    ShaclRejected(Vec<ShaclViolation>),
    /// SHACL graph validation itself failed (fail closed).
    GraphValidationFailed(String),
    /// SHACL validation of the memory payload reported violations.
    PayloadRejected(Vec<ShaclViolation>),
    RdfSerialization(String),
    Embedding(String),
    Store(anyhow::Error),
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestError::EmptyBatch => write!(f, "candidate batch must be non-empty"),
            IngestError::TooManyCandidates { count, max } => {
                write!(f, "candidate batch has {count} entries (cap {max})")
            }
            IngestError::IriTooLong {
                field,
                length,
                max,
            } => write!(f, "{field} IRI is {length} bytes (cap {max})"),
            IngestError::LiteralTooLong { length, max } => {
                write!(f, "literal value is {length} bytes (cap {max})")
            }
            IngestError::UnknownDomain(d) => write!(f, "unknown domain: {d}"),
            IngestError::BudgetExhausted { visited, max_nodes } => write!(
                f,
                "query budget exhausted after {visited} visited nodes (max {max_nodes})"
            ),
            IngestError::EmptyVocabulary(d) => {
                write!(f, "domain {d:?} governs an empty vocabulary; failing closed")
            }
            IngestError::InventedClass(t) => {
                write!(f, "class term is not governed by the domain vocabulary: {t}")
            }
            IngestError::InventedPredicate(t) => {
                write!(f, "predicate term is not governed by the domain vocabulary: {t}")
            }
            IngestError::MalformedIri { field, value } => {
                write!(f, "malformed absolute IRI in {field}: {value}")
            }
            IngestError::UnknownDatatype(dt) => {
                write!(f, "datatype IRI is not in the governed XSD allowlist: {dt}")
            }
            IngestError::InvalidLiteral { field, why } => {
                write!(f, "invalid literal ({field}): {why}")
            }
            IngestError::ShaclRejected(v) => {
                write!(f, "SHACL validation rejected the emitted graph: {} violation(s)", v.len())
            }
            IngestError::GraphValidationFailed(why) => {
                write!(f, "SHACL graph validation failed: {why}")
            }
            IngestError::PayloadRejected(v) => {
                write!(f, "SHACL validation rejected the memory payload: {} violation(s)", v.len())
            }
            IngestError::RdfSerialization(why) => write!(f, "RDF serialization failed: {why}"),
            IngestError::Embedding(why) => write!(f, "embedding failed: {why}"),
            IngestError::Store(err) => write!(f, "store error: {err}"),
        }
    }
}

impl std::error::Error for IngestError {}

fn map_vocab_error(err: VocabularySelectionError) -> IngestError {
    match err {
        VocabularySelectionError::UnknownDomain(d) => IngestError::UnknownDomain(d),
        VocabularySelectionError::BudgetExhausted { visited, max_nodes } => {
            IngestError::BudgetExhausted { visited, max_nodes }
        }
        VocabularySelectionError::EmptyVocabulary(d) => IngestError::EmptyVocabulary(d),
        VocabularySelectionError::MalformedIri(m) => IngestError::MalformedIri {
            field: "iri",
            value: m,
        },
        VocabularySelectionError::UnknownDatatype(d) => IngestError::UnknownDatatype(d),
    }
}

/// Result of whole-batch classification: deterministic Turtle, the exact
/// `MemoryPayload` to persist, and the post-dedup triple count.
#[derive(Debug, Clone)]
pub struct ClassifiedBatch {
    pub turtle: String,
    pub memory_payload: MemoryPayload,
    pub triple_count: usize,
}

/// Result of a successful ingestion: the persisted store id plus the same
/// artifacts, for callers/tests that need to audit what was committed.
#[derive(Debug, Clone)]
pub struct IngestOutcome {
    pub id: String,
    pub turtle: String,
    pub triple_count: usize,
    pub memory_payload: MemoryPayload,
}

/// A scheme (`^[A-Za-z][A-Za-z0-9+.-]*:`) with no path/query/fragment
/// delimiter before the colon. Absolute IRIs only; relative references
/// fail closed.
fn is_absolute_iri(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    if scheme.is_empty() {
        return false;
    }
    let mut chars = scheme.chars();
    let first = chars.next().expect("non-empty scheme");
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const GOVERNED_DATATYPES: &[&str] = &[
    "http://www.w3.org/2001/XMLSchema#string",
    "http://www.w3.org/2001/XMLSchema#integer",
    "http://www.w3.org/2001/XMLSchema#decimal",
    "http://www.w3.org/2001/XMLSchema#double",
    "http://www.w3.org/2001/XMLSchema#boolean",
    "http://www.w3.org/2001/XMLSchema#dateTime",
    "http://www.w3.org/2001/XMLSchema#date",
    "http://www.w3.org/2001/XMLSchema#anyURI",
];

/// Finite pre-allocation caps. Every buffer built from a request is bounded
/// by these constants so a hostile batch cannot drive unbounded allocation.
const MAX_CANDIDATES: usize = 256;
const MAX_IRI_LENGTH: usize = 2048;
const MAX_LITERAL_LENGTH: usize = 65_536;

fn is_governed_datatype(iri: &str) -> bool {
    GOVERNED_DATATYPES.contains(&iri)
}

fn check_iri_length(field: &'static str, value: &str) -> Result<(), IngestError> {
    let length = value.len();
    if length > MAX_IRI_LENGTH {
        Err(IngestError::IriTooLong {
            field,
            length,
            max: MAX_IRI_LENGTH,
        })
    } else {
        Ok(())
    }
}

fn strip_sign(s: &str) -> &str {
    match s.as_bytes() {
        [b'+' | b'-', ..] => &s[1..],
        _ => s,
    }
}

fn valid_integer(s: &str) -> bool {
    let body = strip_sign(s);
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())
}

fn valid_decimal(s: &str) -> bool {
    let body = strip_sign(s);
    if body.is_empty() {
        return false;
    }
    match body.split_once('.') {
        Some((int, frac)) => {
            let int_ok = int.bytes().all(|b| b.is_ascii_digit());
            let frac_ok = frac.bytes().all(|b| b.is_ascii_digit());
            // At least one digit overall; "123." and ".5" are both legal XSD decimals.
            let any_digit = !int.is_empty() || !frac.is_empty();
            int_ok && frac_ok && any_digit
        }
        None => body.bytes().all(|b| b.is_ascii_digit()),
    }
}

fn valid_double(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower == "inf" || lower == "-inf" || lower == "+inf" || lower == "nan" {
        return true;
    }
    let body = strip_sign(s);
    let exp_at = body.find(|c| c == 'e' || c == 'E');
    let (mantissa, exponent) = match exp_at {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    if let Some(e) = exponent {
        let ebody = strip_sign(e);
        if ebody.is_empty() || !ebody.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    valid_decimal(mantissa)
}

fn valid_boolean(s: &str) -> bool {
    matches!(s, "true" | "false" | "1" | "0")
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

fn valid_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let [y, m, d] = parts.as_slice() else {
        return false;
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return false;
    }
    if !y.bytes().all(|b| b.is_ascii_digit())
        || !m.bytes().all(|b| b.is_ascii_digit())
        || !d.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    let year: i64 = y.parse().unwrap_or(-1);
    let month: u32 = m.parse().unwrap_or(0);
    let day: u32 = d.parse().unwrap_or(0);
    month >= 1 && day >= 1 && day <= days_in_month(year, month)
}

fn valid_time(s: &str) -> Option<()> {
    let mut it = s.split(':');
    let h = it.next()?;
    let m = it.next()?;
    let sec = it.next()?;
    if it.next().is_some() {
        return None;
    }
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let sec = match sec.split_once('.') {
        Some((s0, f)) => {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            s0
        }
        None => sec,
    };
    if sec.len() != 2 {
        return None;
    }
    for p in [h, m, sec] {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let hour: u32 = h.parse().unwrap_or(24);
    let minute: u32 = m.parse().unwrap_or(60);
    let second: u32 = sec.parse().unwrap_or(60);
    if hour <= 23 && minute <= 59 && second <= 59 {
        Some(())
    } else {
        None
    }
}

fn valid_datetime(s: &str) -> bool {
    let (date_part, rest) = match s.split_once('T') {
        Some(pair) => pair,
        None => return false,
    };
    if !valid_date(date_part) {
        return false;
    }
    // Optional timezone suffix: 'Z' or (+|-)HH:MM.
    let time_part = if let Some(stripped) = rest.strip_suffix('Z') {
        stripped
    } else if rest.len() >= 6 && rest.is_char_boundary(rest.len() - 6) {
        let cand = &rest[rest.len() - 6..];
        if cand.is_ascii() && (cand.starts_with('+') || cand.starts_with('-'))
            && cand[1..3].bytes().all(|b| b.is_ascii_digit())
            && cand.as_bytes()[3] == b':'
            && cand[4..6].bytes().all(|b| b.is_ascii_digit())
        {
            let tz_hour: u32 = cand[1..3].parse().unwrap_or(25);
            let tz_min: u32 = cand[4..6].parse().unwrap_or(61);
            if tz_hour > 14 || tz_min > 59 {
                return false;
            }
            &rest[..rest.len() - 6]
        } else {
            rest
        }
    } else {
        rest
    };
    valid_time(time_part).is_some()
}

fn validate_literal_lexical(datatype: &str, value: &str) -> Result<(), IngestError> {
    let invalid = |why: &str| IngestError::InvalidLiteral {
        field: "value",
        why: why.to_string(),
    };
    let string_dt = format!("{XSD}string");
    let any_uri = format!("{XSD}anyURI");
    if datatype == string_dt || datatype == any_uri {
        return Ok(());
    }
    let integer_dt = format!("{XSD}integer");
    if datatype == integer_dt {
        return if valid_integer(value) {
            Ok(())
        } else {
            Err(invalid("not a valid xsd:integer lexical form"))
        };
    }
    let decimal_dt = format!("{XSD}decimal");
    if datatype == decimal_dt {
        return if valid_decimal(value) {
            Ok(())
        } else {
            Err(invalid("not a valid xsd:decimal lexical form"))
        };
    }
    let double_dt = format!("{XSD}double");
    if datatype == double_dt {
        return if valid_double(value) {
            Ok(())
        } else {
            Err(invalid("not a valid xsd:double lexical form"))
        };
    }
    let boolean_dt = format!("{XSD}boolean");
    if datatype == boolean_dt {
        return if valid_boolean(value) {
            Ok(())
        } else {
            Err(invalid("not a valid xsd:boolean lexical form"))
        };
    }
    let date_dt = format!("{XSD}date");
    if datatype == date_dt {
        return if valid_date(value) {
            Ok(())
        } else {
            Err(invalid("not a valid xsd:date lexical form"))
        };
    }
    let datetime_dt = format!("{XSD}dateTime");
    if datatype == datetime_dt {
        return if valid_datetime(value) {
            Ok(())
        } else {
            Err(invalid("not a valid xsd:dateTime lexical form"))
        };
    }
    // Unreachable: datatypes are allowlist-checked before lexical validation.
    Err(IngestError::UnknownDatatype(datatype.to_string()))
}

/// Build a validated RDF term for a literal object. Datatype IRIs must be
/// governed (allowlist) and absolute; language tags must be valid BCP47 and
/// are mutually exclusive with a datatype.
fn build_literal(lit: &CandidateLiteral) -> Result<Term, IngestError> {
    if lit.datatype.is_some() && lit.language.is_some() {
        return Err(IngestError::InvalidLiteral {
            field: "datatype",
            why: "a literal cannot carry both a datatype IRI and a language tag".to_string(),
        });
    }
    if let Some(language) = &lit.language {
        return Literal::new_language_tagged_literal(lit.value.clone(), language.clone())
            .map(Term::Literal)
            .map_err(|e| IngestError::InvalidLiteral {
                field: "language",
                why: e.to_string(),
            });
    }
    if let Some(datatype) = &lit.datatype {
        if !is_absolute_iri(datatype) {
            return Err(IngestError::MalformedIri {
                field: "datatype",
                value: datatype.clone(),
            });
        }
        if !is_governed_datatype(datatype) {
            return Err(IngestError::UnknownDatatype(datatype.clone()));
        }
        validate_literal_lexical(datatype, &lit.value)?;
        let datatype_node = NamedNode::new(datatype.clone()).map_err(|_| {
            IngestError::MalformedIri {
                field: "datatype",
                value: datatype.clone(),
            }
        })?;
        return Ok(Term::Literal(Literal::new_typed_literal(
            lit.value.clone(),
            datatype_node,
        )));
    }
    Ok(Term::Literal(Literal::new_simple_literal(lit.value.clone())))
}

/// Canonical serialization of the whole batch as Turtle. Deterministic:
/// triples are sorted by their Display form and duplicates are removed before
/// serialization. All subject/predicate/object IRIs were validated to be
/// absolute, so no base IRI is required.
fn serialize_turtle(triples: &[Triple]) -> Result<(String, usize), IngestError> {
    let mut sorted = triples.to_vec();
    sorted.sort_by(|a, b| {
        (
            a.subject.to_string(),
            a.predicate.to_string(),
            a.object.to_string(),
        )
            .cmp(&(
                b.subject.to_string(),
                b.predicate.to_string(),
                b.object.to_string(),
            ))
    });
    sorted.dedup_by(|a, b| {
        a.subject.to_string() == b.subject.to_string()
            && a.predicate.to_string() == b.predicate.to_string()
            && a.object.to_string() == b.object.to_string()
    });

    let mut writer = oxttl::TurtleSerializer::new()
        .with_prefix("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        .map_err(|e| IngestError::RdfSerialization(e.to_string()))?
        .for_writer(Vec::new());
    for triple in &sorted {
        writer
            .serialize_triple(triple.as_ref())
            .map_err(|e| IngestError::RdfSerialization(e.to_string()))?;
    }
    let bytes = writer
        .finish()
        .map_err(|e| IngestError::RdfSerialization(e.to_string()))?;
    let turtle = String::from_utf8(bytes)
        .map_err(|e| IngestError::RdfSerialization(e.to_string()))?;
    Ok((turtle, sorted.len()))
}

/// Build the canonical batch `MemoryPayload`. `fact` type, canonical Turtle as
/// content, `bounded-classifier` as generating agent, `unverified_agent`
/// epistemic tier, and `task_intent_signals`/`task_intent` derived from the
/// caller's reason string. `verified_by` is never set here: self verification
/// is not verification.
fn build_payload(
    request: &BoundedClassificationRequest,
    turtle: &str,
    normalized_domain: &str,
) -> MemoryPayload {
    let signals = request
        .reason
        .as_deref()
        .map(detect_task_intents)
        .unwrap_or_default();
    let task_intent = signals
        .first()
        .map(|s| Value::String(s.marker.to_string()))
        .unwrap_or(Value::Null);
    let signals_json = Value::Array(
        signals
            .iter()
            .map(|s| {
                serde_json::json!({
                    "marker": s.marker,
                    "excerpt": s.excerpt,
                })
            })
            .collect(),
    );
    MemoryPayload {
        content: turtle.to_string(),
        user_id: request.user_id.clone(),
        memory_type: "fact".to_string(),
        agent_name: Some("bounded-classifier".to_string()),
        location: String::new(),
        location_lines: String::new(),
        metadata: serde_json::json!({
            "generated_by": "bounded-classifier",
            "epistemic_tier": "unverified_agent",
            "domain": normalized_domain,
            "reason": request.reason,
            "task_intent_signals": signals_json,
            "task_intent": task_intent,
        }),
    }
}

/// Classify a batch of explicitly governed candidates against the governed
/// vocabulary of ONE domain, under a finite `QueryBudget`.
///
/// Fail-closed pipeline: non-empty batch and non-empty domain guard → domain
/// vocabulary selection (budget-checked) → per-candidate IRI syntax and
/// absoluteness → class governed → predicate governed → object/literal
/// validation → whole-batch SHACL validation of the emitted RDF graph →
/// SHACL validation of the payload. Nothing is written by this function; the
/// caller decides whether/when to commit, and any error here means zero
/// writes.
pub fn bounded_classify(
    index: &OntologyIndex,
    budget: &QueryBudget,
    request: &BoundedClassificationRequest,
) -> Result<ClassifiedBatch, IngestError> {
    if request.candidates.is_empty() {
        return Err(IngestError::EmptyBatch);
    }
    if request.candidates.len() > MAX_CANDIDATES {
        return Err(IngestError::TooManyCandidates {
            count: request.candidates.len(),
            max: MAX_CANDIDATES,
        });
    }
    // Bound every buffer built from the request before any allocation.
    for candidate in &request.candidates {
        check_iri_length("subject", &candidate.subject)?;
        check_iri_length("class", &candidate.class)?;
        check_iri_length("predicate", &candidate.predicate)?;
        match &candidate.object {
            CandidateObject::Iri(iri) => check_iri_length("object", iri)?,
            CandidateObject::Literal(literal) => {
                let length = literal.value.len();
                if length > MAX_LITERAL_LENGTH {
                    return Err(IngestError::LiteralTooLong {
                        length,
                        max: MAX_LITERAL_LENGTH,
                    });
                }
                if let Some(datatype) = &literal.datatype {
                    check_iri_length("datatype", datatype)?;
                }
            }
        }
    }
    let domain = crate::domain::normalize_domain_slug(&request.domain)
        .ok_or_else(|| IngestError::UnknownDomain(request.domain.clone()))?;

    let vocab = index
        .select_domain_vocabulary(&domain, budget)
        .map_err(map_vocab_error)?;
    if vocab.is_empty() {
        return Err(IngestError::EmptyVocabulary(domain.to_string()));
    }

    let rdf_type = rdf::TYPE.into_owned();
    let mut triples: Vec<Triple> = Vec::with_capacity(request.candidates.len() * 2);
    for candidate in &request.candidates {
        if !is_absolute_iri(&candidate.subject) {
            return Err(IngestError::MalformedIri {
                field: "subject",
                value: candidate.subject.clone(),
            });
        }
        if !is_absolute_iri(&candidate.class) {
            return Err(IngestError::MalformedIri {
                field: "class",
                value: candidate.class.clone(),
            });
        }
        if !is_absolute_iri(&candidate.predicate) {
            return Err(IngestError::MalformedIri {
                field: "predicate",
                value: candidate.predicate.clone(),
            });
        }
        let subject = NamedNode::new(candidate.subject.clone()).map_err(|_| {
            IngestError::MalformedIri {
                field: "subject",
                value: candidate.subject.clone(),
            }
        })?;
        let class = NamedNode::new(candidate.class.clone()).map_err(|_| {
            IngestError::MalformedIri {
                field: "class",
                value: candidate.class.clone(),
            }
        })?;
        let predicate = NamedNode::new(candidate.predicate.clone()).map_err(|_| {
            IngestError::MalformedIri {
                field: "predicate",
                value: candidate.predicate.clone(),
            }
        })?;
        if !vocab.governs_class(&candidate.class) {
            return Err(IngestError::InventedClass(candidate.class.clone()));
        }
        if !vocab.governs_predicate(&candidate.predicate) {
            return Err(IngestError::InventedPredicate(candidate.predicate.clone()));
        }
        let object = match &candidate.object {
            CandidateObject::Iri(iri) => {
                if !is_absolute_iri(iri) {
                    return Err(IngestError::MalformedIri {
                        field: "object",
                        value: iri.clone(),
                    });
                }
                Term::NamedNode(NamedNode::new(iri.clone()).map_err(|_| {
                    IngestError::MalformedIri {
                        field: "object",
                        value: iri.clone(),
                    }
                })?)
            }
            CandidateObject::Literal(literal) => build_literal(literal)?,
        };
        triples.push(Triple::new(
            subject.clone(),
            rdf_type.clone(),
            Term::NamedNode(class),
        ));
        triples.push(Triple::new(subject, predicate, object));
    }

    let (turtle, triple_count) = serialize_turtle(&triples)?;

    let graph_violations = index
        .validate_rdf_graph(&turtle)
        .map_err(|e| IngestError::GraphValidationFailed(e.to_string()))?;
    if !graph_violations.is_empty() {
        return Err(IngestError::ShaclRejected(graph_violations));
    }

    let memory_payload = build_payload(request, &turtle, &domain);
    let payload_violations = crate::rdf::shacl::validate_memory(&memory_payload);
    if !payload_violations.is_empty() {
        return Err(IngestError::PayloadRejected(payload_violations));
    }

    Ok(ClassifiedBatch {
        turtle,
        memory_payload,
        triple_count,
    })
}

/// Classify, then commit exactly one batch memory to the real `VectorStore`.
/// All validation happens inside `bounded_classify` before the single
/// `upsert`; any failure aborts with zero writes. The deterministic id is
/// derived from the canonical Turtle bytes, so re-ingesting the same batch is
/// idempotent at the id level.
pub async fn ingest_bounded_classification<S: VectorStore + ?Sized>(
    store: &S,
    namespace: &str,
    index: &OntologyIndex,
    budget: &QueryBudget,
    request: &BoundedClassificationRequest,
    embedder: &dyn Embedder,
) -> Result<IngestOutcome, IngestError> {
    let batch = bounded_classify(index, budget, request)?;
    let namespace = namespace.trim();
    if namespace.is_empty() {
        return Err(IngestError::Store(anyhow::anyhow!(
            "namespace must be non-empty"
        )));
    }
    let vector = embedder
        .embed(&batch.memory_payload.content)
        .await
        .map_err(|e| IngestError::Embedding(e.to_string()))?;
    let id = format!(
        "bounded-ingest-{}",
        nquads_digest(&batch.memory_payload.content)
    );
    let report = store
        .upsert(namespace, &id, vector, batch.memory_payload.clone())
        .await
        .map_err(IngestError::Store)?;
    Ok(IngestOutcome {
        id: report.id,
        turtle: batch.turtle,
        triple_count: batch.triple_count,
        memory_payload: batch.memory_payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdf::ontology::DomainEntry;
    use crate::store::ladybug::LadybugStore;
    use oxrdf::Term;
    use oxttl::TurtleParser;

    const NS: &str = "urn:neurostrata:ns#";

    // Expected public caps; the implementation mirrors these values.
    const MAX_CANDIDATES: usize = 256;
    const MAX_IRI_LENGTH: usize = 2048;
    const MAX_LITERAL_LENGTH: usize = 65_536;

    const SHAPES_TTL: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ns: <urn:neurostrata:ns#> .
<urn:neurostrata:shape:memory> a sh:NodeShape ;
    sh:targetClass ns:Memory ;
    sh:property [ sh:path ns:hasType ; sh:minCount 1 ] .
<urn:neurostrata:shape:other> a sh:NodeShape ;
    sh:targetClass ns:Other ;
    sh:property [ sh:path ns:otherProp ; sh:minCount 1 ] .
"#;

    const STRICT_SHAPES_TTL: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ns: <urn:neurostrata:ns#> .
<urn:neurostrata:shape:memory> a sh:NodeShape ;
    sh:targetClass ns:Memory ;
    sh:property [ sh:path ns:hasType ; sh:minCount 2 ] .
<urn:neurostrata:shape:other> a sh:NodeShape ;
    sh:targetClass ns:Other ;
    sh:property [ sh:path ns:otherProp ; sh:minCount 1 ] .
"#;

    fn budget() -> QueryBudget {
        QueryBudget {
            max_ms: 10_000,
            max_nodes: 10_000,
        }
    }

    fn make_index() -> OntologyIndex {
        let entries = vec![
            DomainEntry {
                slug: "memory".to_string(),
                parent: None,
                aliases: vec![],
                concept_iri: Some(format!("{NS}Memory")),
            },
            DomainEntry {
                slug: "other".to_string(),
                parent: None,
                aliases: vec![],
                concept_iri: Some(format!("{NS}Other")),
            },
        ];
        let mut idx = OntologyIndex::from_entries(entries);
        idx.add_sub_property(&format!("{NS}hasType"), &format!("{NS}GlobalParent"));
        idx.add_symmetric_property(&format!("{NS}GlobalSymmetric"));
        idx.add_transitive_property(&format!("{NS}GlobalTransitive"));
        idx.load_shacl_shapes(SHAPES_TTL).expect("shapes load");
        idx
    }

    fn strict_index() -> OntologyIndex {
        let mut idx = make_index();
        idx.shacl_shapes.clear();
        idx.shapes_ttl = None;
        idx.load_shacl_shapes(STRICT_SHAPES_TTL).expect("shapes load");
        idx
    }

    fn request(candidates: Vec<ClassificationCandidate>) -> BoundedClassificationRequest {
        BoundedClassificationRequest {
            domain: "memory".to_string(),
            user_id: "tester".to_string(),
            candidates,
            reason: None,
        }
    }

    fn valid_candidate() -> ClassificationCandidate {
        ClassificationCandidate {
            subject: format!("{NS}thing:1"),
            class: format!("{NS}Memory"),
            predicate: format!("{NS}hasType"),
            object: CandidateObject::Iri(format!("{NS}ValueA")),
        }
    }

    struct StubEmbedder;

    #[async_trait::async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 4];
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += b as f32;
            }
            Ok(v)
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    fn temp_store_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/opencode").join(format!("ns-ingest-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn classify_emits_deterministic_rdf_payload() {
        let index = make_index();
        let batch = bounded_classify(&index, &budget(), &request(vec![valid_candidate()]))
            .expect("governed candidate classifies");
        assert_eq!(batch.triple_count, 2, "rdf:type + predicate triple");
        assert!(batch.turtle.contains("rdf:type") || batch.turtle.contains(" a "));
        assert!(batch.turtle.contains("ns#Memory") || batch.turtle.contains("#Memory>"));

        let parsed: Vec<_> = TurtleParser::new()
            .for_slice(batch.turtle.as_bytes())
            .collect::<Result<_, _>>()
            .expect("emitted turtle parses");
        assert_eq!(parsed.len(), 2);
        let rdf_type = oxrdf::vocab::rdf::TYPE;
        assert!(parsed.iter().any(|t| t.predicate.as_str() == rdf_type.as_str()));
        assert!(parsed
            .iter()
            .any(|t| t.predicate.as_str() == format!("{NS}hasType")));

        let payload = &batch.memory_payload;
        assert_eq!(payload.memory_type, "fact");
        assert_eq!(payload.user_id, "tester");
        assert_eq!(payload.content, batch.turtle);
        assert_eq!(payload.agent_name.as_deref(), Some("bounded-classifier"));
        assert_eq!(
            payload.metadata.get("generated_by").and_then(Value::as_str),
            Some("bounded-classifier")
        );
        assert_eq!(
            payload.metadata.get("epistemic_tier").and_then(Value::as_str),
            Some("unverified_agent")
        );
        assert!(payload.metadata.get("verified_by").is_none());
        assert_eq!(
            payload.metadata.get("task_intent"),
            Some(&Value::Null),
            "no reason -> no task intent"
        );
        assert_eq!(
            payload
                .metadata
                .get("task_intent_signals")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn invented_class_fails_closed() {
        let index = make_index();
        let mut c = valid_candidate();
        c.class = format!("{NS}Widget");
        let err = bounded_classify(&index, &budget(), &request(vec![c])).unwrap_err();
        assert!(matches!(err, IngestError::InventedClass(t) if t == format!("{NS}Widget")));
    }

    #[test]
    fn invented_predicate_fails_closed() {
        let index = make_index();
        let mut c = valid_candidate();
        c.predicate = format!("{NS}bogus");
        let err = bounded_classify(&index, &budget(), &request(vec![c])).unwrap_err();
        assert!(matches!(err, IngestError::InventedPredicate(t) if t == format!("{NS}bogus")));
    }

    #[test]
    fn out_of_domain_governed_predicate_fails_closed() {
        let index = make_index();
        let mut c = valid_candidate();
        // otherProp is governed by the "other" domain's shape, not by "memory".
        c.predicate = format!("{NS}otherProp");
        let err = bounded_classify(&index, &budget(), &request(vec![c])).unwrap_err();
        assert!(matches!(err, IngestError::InventedPredicate(t) if t == format!("{NS}otherProp")));
    }

    #[test]
    fn global_predicate_maps_do_not_leak_into_domain() {
        let index = make_index();
        for leak in ["GlobalSymmetric", "GlobalTransitive"] {
            let mut c = valid_candidate();
            c.predicate = format!("{NS}{leak}");
            let err = bounded_classify(&index, &budget(), &request(vec![c])).unwrap_err();
            assert!(
                matches!(err, IngestError::InventedPredicate(_)),
                "{leak} must not leak into the domain vocabulary"
            );
        }
        // But GlobalParent, a registered superproperty of the governed hasType,
        // IS governed via the domain-scoped property hierarchy. Pair it with
        // hasType so the fixture's minCount(1) shape stays satisfied.
        let mut governed = valid_candidate();
        governed.predicate = format!("{NS}GlobalParent");
        let batch = bounded_classify(
            &index,
            &budget(),
            &request(vec![valid_candidate(), governed]),
        )
        .expect("superproperty governed");
        assert!(batch.turtle.contains("GlobalParent"));
    }

    #[test]
    fn empty_batch_fails_closed() {
        let index = make_index();
        let err = bounded_classify(&index, &budget(), &request(vec![])).unwrap_err();
        assert!(matches!(err, IngestError::EmptyBatch));
    }

    #[test]
    fn no_shapes_means_empty_vocabulary_fail_closed() {
        let idx = OntologyIndex::from_entries(vec![DomainEntry {
            slug: "memory".to_string(),
            parent: None,
            aliases: vec![],
            concept_iri: Some(format!("{NS}Memory")),
        }]);
        let err = bounded_classify(&idx, &budget(), &request(vec![valid_candidate()])).unwrap_err();
        assert!(matches!(err, IngestError::EmptyVocabulary(d) if d == "memory"));
    }

    #[test]
    fn zero_budget_fails_closed() {
        let index = make_index();
        let zero = QueryBudget {
            max_ms: 0,
            max_nodes: 0,
        };
        let err = bounded_classify(&index, &zero, &request(vec![valid_candidate()])).unwrap_err();
        assert!(matches!(err, IngestError::BudgetExhausted { .. }));
        let one = QueryBudget {
            max_ms: 10_000,
            max_nodes: 1,
        };
        let err = bounded_classify(&index, &one, &request(vec![valid_candidate()])).unwrap_err();
        assert!(matches!(err, IngestError::BudgetExhausted { .. }));
    }

    #[test]
    fn unknown_domain_fails_closed() {
        let index = make_index();
        let mut req = request(vec![valid_candidate()]);
        req.domain = "nope".to_string();
        let err = bounded_classify(&index, &budget(), &req).unwrap_err();
        assert!(matches!(err, IngestError::UnknownDomain(d) if d == "nope"));
        let mut blank = request(vec![valid_candidate()]);
        blank.domain = "   ".to_string();
        let err = bounded_classify(&index, &budget(), &blank).unwrap_err();
        assert!(matches!(err, IngestError::UnknownDomain(_)));
    }

    #[test]
    fn malformed_iris_fail_closed() {
        let index = make_index();
        let cases: Vec<(&str, ClassificationCandidate)> = vec![
            ("subject", ClassificationCandidate {
                subject: "urn:x#a b".to_string(),
                ..valid_candidate()
            }),
            ("subject", ClassificationCandidate {
                subject: "relative/thing".to_string(),
                ..valid_candidate()
            }),
            ("class", ClassificationCandidate {
                class: "relative/class".to_string(),
                ..valid_candidate()
            }),
            ("predicate", ClassificationCandidate {
                predicate: "relative/pred".to_string(),
                ..valid_candidate()
            }),
            ("object", ClassificationCandidate {
                object: CandidateObject::Iri("relative/obj".to_string()),
                ..valid_candidate()
            }),
            ("datatype", ClassificationCandidate {
                object: CandidateObject::Literal(CandidateLiteral {
                    value: "1".to_string(),
                    datatype: Some("relative/dt".to_string()),
                    language: None,
                }),
                ..valid_candidate()
            }),
        ];
        for (field, candidate) in cases {
            let err = bounded_classify(&index, &budget(), &request(vec![candidate])).unwrap_err();
            assert!(
                matches!(&err, IngestError::MalformedIri { field: f, .. } if *f == field),
                "expected MalformedIri for {field}, got {err:?}"
            );
        }
    }

    #[test]
    fn unknown_datatype_fails_closed() {
        let index = make_index();
        let candidate = ClassificationCandidate {
            object: CandidateObject::Literal(CandidateLiteral {
                value: "x".to_string(),
                datatype: Some(format!("{NS}customDT")),
                language: None,
            }),
            ..valid_candidate()
        };
        let err = bounded_classify(&index, &budget(), &request(vec![candidate])).unwrap_err();
        assert!(matches!(err, IngestError::UnknownDatatype(dt) if dt == format!("{NS}customDT")));
    }

    #[test]
    fn invalid_literal_lexicals_fail_closed() {
        let index = make_index();
        let xsd_int = "http://www.w3.org/2001/XMLSchema#integer";
        let xsd_bool = "http://www.w3.org/2001/XMLSchema#boolean";
        let xsd_dt = "http://www.w3.org/2001/XMLSchema#dateTime";
        let xsd_date = "http://www.w3.org/2001/XMLSchema#date";
        let cases = [
            (xsd_int, "forty-two"),
            (xsd_bool, "yes"),
            (xsd_dt, "2026-13-99T99:99:99Z"),
            (xsd_dt, "yesterday"),
            (xsd_date, "2026-02-30"),
        ];
        for (datatype, value) in cases {
            let candidate = ClassificationCandidate {
                object: CandidateObject::Literal(CandidateLiteral {
                    value: value.to_string(),
                    datatype: Some(datatype.to_string()),
                    language: None,
                }),
                ..valid_candidate()
            };
            let err = bounded_classify(&index, &budget(), &request(vec![candidate])).unwrap_err();
            assert!(
                matches!(&err, IngestError::InvalidLiteral { field: "value", .. }),
                "datatype {datatype} value {value:?} must fail closed: {err:?}"
            );
        }
        // And the valid twins are accepted.
        let good = [
            (xsd_int, "42"),
            (xsd_int, "-7"),
            (xsd_bool, "true"),
            (xsd_dt, "2026-11-17T01:02:03Z"),
            (xsd_dt, "2026-11-17T01:02:03+02:00"),
            (xsd_date, "2026-02-28"),
        ];
        for (datatype, value) in good {
            let candidate = ClassificationCandidate {
                object: CandidateObject::Literal(CandidateLiteral {
                    value: value.to_string(),
                    datatype: Some(datatype.to_string()),
                    language: None,
                }),
                ..valid_candidate()
            };
            bounded_classify(&index, &budget(), &request(vec![candidate]))
                .expect("valid lexical form must be accepted");
        }
    }

    #[test]
    fn language_tag_validated_and_datatype_exclusive() {
        let index = make_index();
        let both = ClassificationCandidate {
            object: CandidateObject::Literal(CandidateLiteral {
                value: "hello".to_string(),
                datatype: Some("http://www.w3.org/2001/XMLSchema#string".to_string()),
                language: Some("en".to_string()),
            }),
            ..valid_candidate()
        };
        let err = bounded_classify(&index, &budget(), &request(vec![both])).unwrap_err();
        assert!(matches!(err, IngestError::InvalidLiteral { .. }));

        let bad_tag = ClassificationCandidate {
            object: CandidateObject::Literal(CandidateLiteral {
                value: "hello".to_string(),
                datatype: None,
                language: Some("not a tag!".to_string()),
            }),
            ..valid_candidate()
        };
        let err = bounded_classify(&index, &budget(), &request(vec![bad_tag])).unwrap_err();
        assert!(matches!(&err, IngestError::InvalidLiteral { field: "language", .. }));

        let ok = ClassificationCandidate {
            object: CandidateObject::Literal(CandidateLiteral {
                value: "hello".to_string(),
                datatype: None,
                language: Some("en-US".to_string()),
            }),
            ..valid_candidate()
        };
        bounded_classify(&index, &budget(), &request(vec![ok])).expect("valid BCP47 tag accepted");
    }

    #[test]
    fn output_is_deterministic_and_deduped() {
        let index = make_index();
        let a = valid_candidate();
        let mut b = valid_candidate();
        b.subject = format!("{NS}thing:2");

        let forward = bounded_classify(
            &index,
            &budget(),
            &request(vec![a.clone(), b.clone()]),
        )
        .expect("forward batch classifies");
        let reversed = bounded_classify(&index, &budget(), &request(vec![b, a]))
            .expect("reversed batch classifies");
        assert_eq!(
            forward.turtle, reversed.turtle,
            "candidate order must not change the emitted turtle"
        );
        assert_eq!(forward.triple_count, 4);

        let dup = bounded_classify(
            &index,
            &budget(),
            &request(vec![valid_candidate(), valid_candidate()]),
        )
        .expect("duplicate candidates classify");
        assert_eq!(dup.triple_count, 2, "duplicates collapse before emit");
    }

    #[test]
    fn task_intent_signals_flow_to_payload() {
        let index = make_index();
        let mut req = request(vec![valid_candidate()]);
        req.reason = Some("TODO: wire this up".to_string());
        let batch = bounded_classify(&index, &budget(), &req).expect("classifies");
        let signals = batch
            .memory_payload
            .metadata
            .get("task_intent_signals")
            .and_then(Value::as_array)
            .expect("signals array")
            .clone();
        assert!(!signals.is_empty(), "reason with todo: marker yields signals");
        assert_eq!(
            signals[0].get("marker").and_then(Value::as_str),
            Some("todo:")
        );
        assert!(signals[0].get("excerpt").is_some());
        assert_eq!(
            batch.memory_payload.metadata.get("task_intent"),
            Some(&Value::String("todo:".to_string()))
        );
    }

    #[test]
    fn shacl_graph_violation_rejects_batch() {
        let index = strict_index();
        let err = bounded_classify(&index, &budget(), &request(vec![valid_candidate()]))
            .unwrap_err();
        assert!(
            matches!(err, IngestError::ShaclRejected(_)),
            "minCount 2 with one triple must fail SHACL: {err:?}"
        );
    }

    #[test]
    fn payload_rejected_when_user_id_empty() {
        let index = make_index();
        let mut req = request(vec![valid_candidate()]);
        req.user_id = String::new();
        let err = bounded_classify(&index, &budget(), &req).unwrap_err();
        assert!(matches!(err, IngestError::PayloadRejected(_)));
    }

    #[tokio::test]
    async fn mixed_batch_writes_nothing() {
        let index = make_index();
        let dir = temp_store_dir();
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp db");
        store.init("test-ns").await.expect("init schema");

        let mut bad = valid_candidate();
        bad.predicate = format!("{NS}bogus");
        let req = request(vec![valid_candidate(), bad]);
        let err = ingest_bounded_classification(
            &store,
            "test-ns",
            &index,
            &budget(),
            &req,
            &StubEmbedder,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IngestError::InventedPredicate(_)));

        let hits = store
            .search("test-ns", vec![0.0f32; 4], 16, false, false)
            .await
            .expect("search");
        assert_eq!(hits.len(), 0, "a rejected batch must not write anything");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn valid_batch_writes_exactly_once_to_real_store() {
        let index = make_index();
        let dir = temp_store_dir();
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp db");
        store.init("test-ns").await.expect("init schema");

        let outcome = ingest_bounded_classification(
            &store,
            "test-ns",
            &index,
            &budget(),
            &request(vec![valid_candidate()]),
            &StubEmbedder,
        )
        .await
        .expect("governed batch ingests");
        assert!(outcome.id.starts_with("bounded-ingest-"));

        let hits = store
            .search("test-ns", vec![0.0f32; 4], 16, false, false)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1, "exactly one batch memory is committed");

        let (_, payload) = store
            .get("test-ns", &outcome.id)
            .await
            .expect("get round trip")
            .expect("row exists");
        assert_eq!(payload.content, outcome.turtle);
        assert_eq!(payload.memory_type, "fact");
        assert_eq!(
            payload.metadata.get("generated_by").and_then(Value::as_str),
            Some("bounded-classifier")
        );
        assert!(payload.metadata.get("verified_by").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn shacl_rejected_batch_writes_nothing() {
        let index = strict_index();
        let dir = temp_store_dir();
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp db");
        store.init("test-ns").await.expect("init schema");

        let err = ingest_bounded_classification(
            &store,
            "test-ns",
            &index,
            &budget(),
            &request(vec![valid_candidate()]),
            &StubEmbedder,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IngestError::ShaclRejected(_)));

        let hits = store
            .search("test-ns", vec![0.0f32; 4], 16, false, false)
            .await
            .expect("search");
        assert_eq!(hits.len(), 0, "SHACL rejection must precede any write");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn namespace_must_be_non_empty() {
        let index = make_index();
        let store = LadybugStore::for_testing(&temp_store_dir(), 4).expect("open temp db");
        store.init("test-ns").await.expect("init schema");
        let err = ingest_bounded_classification(
            &store,
            "   ",
            &index,
            &budget(),
            &request(vec![valid_candidate()]),
            &StubEmbedder,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IngestError::Store(_)));
    }

    // --- Capability F review fixes: RED regressions -----------------------

    #[tokio::test]
    async fn invented_class_absolute_iri_is_rejected_with_zero_writes() {
        let dir = temp_store_dir();
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp db");
        store.init("test-ns").await.expect("init schema");
        let mut candidate = valid_candidate();
        // `memory:` normalizes to the governed slug `memory`, but it is an
        // invented absolute IRI and must never be accepted as a class.
        candidate.class = "memory:".to_string();
        let err = ingest_bounded_classification(
            &store,
            "test-ns",
            &make_index(),
            &budget(),
            &request(vec![candidate]),
            &StubEmbedder,
        )
        .await
        .expect_err("invented class IRI must fail closed");
        assert!(matches!(err, IngestError::InventedClass(_)), "got {err}");
        let hits = store
            .search("test-ns", vec![0.0f32; 4], 16, false, false)
            .await
            .expect("search");
        assert!(hits.is_empty(), "a rejected class must write nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn preallocation_caps_reject_oversized_batches_iris_and_literals() {
        let dir = temp_store_dir();
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp db");
        store.init("test-ns").await.expect("init schema");
        let index = make_index();

        let too_many = vec![valid_candidate(); MAX_CANDIDATES + 1];
        assert!(
            bounded_classify(&index, &budget(), &request(too_many)).is_err(),
            "candidate count above the pre-allocation cap must fail closed"
        );

        let mut long_iri = valid_candidate();
        long_iri.subject = format!("{NS}{}", "x".repeat(MAX_IRI_LENGTH));
        assert!(
            bounded_classify(&index, &budget(), &request(vec![long_iri])).is_err(),
            "an over-long IRI must fail closed"
        );

        let mut long_literal = valid_candidate();
        long_literal.object = CandidateObject::Literal(CandidateLiteral {
            value: "y".repeat(MAX_LITERAL_LENGTH + 1),
            datatype: None,
            language: None,
        });
        assert!(
            bounded_classify(&index, &budget(), &request(vec![long_literal])).is_err(),
            "an over-long literal must fail closed"
        );

        let hits = store
            .search("test-ns", vec![0.0f32; 4], 16, false, false)
            .await
            .expect("search");
        assert!(hits.is_empty(), "cap violations must write nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalized_domain_is_persisted_in_payload_metadata() {
        let mut req = request(vec![valid_candidate()]);
        req.domain = "  Memory  ".to_string();
        let batch = bounded_classify(&make_index(), &budget(), &req)
            .expect("a case/whitespace variant of a governed domain must resolve");
        assert_eq!(
            batch.memory_payload.metadata.get("domain").and_then(Value::as_str),
            Some("memory"),
            "persist the resolved domain slug, not the raw request"
        );
    }
}
