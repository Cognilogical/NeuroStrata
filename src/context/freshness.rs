//! Lazy freshness computation: pure comparisons plus a bounded `AnchorIo`.

use super::model::{Coverage, DecisionLifecycle, Freshness, SemanticValidation, SourceCurrency};

/// Inputs to a freshness computation. Every field is optional so a caller that
/// only knows part of the picture still gets an honest answer (the missing
/// parts read as `unknown`/`unavailable`/`proposed`, never as `verified`).
#[derive(Debug, Clone, Default)]
pub struct FreshnessInputs {
    /// Hash captured when the memory was written/verified.
    pub stored_file_sha256: Option<String>,
    /// Hash of the file as it is now.
    pub current_file_sha256: Option<String>,
    /// Hash of the governed span captured at write/verify time.
    pub stored_span_sha256: Option<String>,
    /// Hash of the governed span as it is now.
    pub current_span_sha256: Option<String>,
    /// `verification.status` from the memory's metadata.
    pub verification_status: Option<String>,
    /// Index coverage reported by the store.
    pub index_coverage: Option<Coverage>,
    /// `metadata.decision.status` when the memory is a decision.
    pub decision_status: Option<String>,
    /// Whether the memory governs any path at all. `false` makes source
    /// currency `not_applicable` rather than `unknown`.
    pub has_governed_path: bool,
}

impl FreshnessInputs {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Compute the four freshness dimensions from `inputs`.
pub fn compute(inputs: &FreshnessInputs) -> Freshness {
    Freshness {
        source_currency: source_currency(inputs),
        semantic_validation: semantic_validation(inputs.verification_status.as_deref()),
        index_coverage: inputs.index_coverage.unwrap_or(Coverage::Unavailable),
        decision_lifecycle: decision_lifecycle(inputs.decision_status.as_deref()),
    }
}

/// Alias for callers that prefer a verb-first name.
pub fn compute_freshness(inputs: &FreshnessInputs) -> Freshness {
    compute(inputs)
}

/// Positional variant kept for callers that do not want to build the struct.
#[allow(clippy::too_many_arguments)]
pub fn compute_all(
    stored_file_sha256: Option<&str>,
    current_file_sha256: Option<&str>,
    stored_span_sha256: Option<&str>,
    current_span_sha256: Option<&str>,
    verification_status: Option<&str>,
    index_coverage: Option<Coverage>,
    decision_status: Option<&str>,
    has_governed_path: bool,
) -> Freshness {
    compute(&FreshnessInputs {
        stored_file_sha256: stored_file_sha256.map(str::to_string),
        current_file_sha256: current_file_sha256.map(str::to_string),
        stored_span_sha256: stored_span_sha256.map(str::to_string),
        current_span_sha256: current_span_sha256.map(str::to_string),
        verification_status: verification_status.map(str::to_string),
        index_coverage,
        decision_status: decision_status.map(str::to_string),
        has_governed_path,
    })
}

/// Compute only the source-currency dimension from two hashes.
pub fn source_currency_from_hashes(stored: Option<&str>, current: Option<&str>) -> SourceCurrency {
    match (stored, current) {
        (None, _) => SourceCurrency::Unknown,
        (Some(_), None) => SourceCurrency::Missing,
        (Some(s), Some(c)) if s == c => SourceCurrency::Unchanged,
        (Some(_), Some(_)) => SourceCurrency::Changed,
    }
}

fn source_currency(inputs: &FreshnessInputs) -> SourceCurrency {
    if !inputs.has_governed_path {
        return SourceCurrency::NotApplicable;
    }
    // Span hash is authoritative when both sides carry it.
    if let (Some(stored), Some(current)) =
        (inputs.stored_span_sha256.as_deref(), inputs.current_span_sha256.as_deref())
    {
        if stored == current {
            return SourceCurrency::Unchanged;
        }
        return SourceCurrency::Changed;
    }
    source_currency_from_hashes(
        inputs.stored_file_sha256.as_deref(),
        inputs.current_file_sha256.as_deref(),
    )
}

fn semantic_validation(status: Option<&str>) -> SemanticValidation {
    match status.map(str::trim).map(str::to_lowercase).as_deref() {
        Some("verified") => SemanticValidation::Verified,
        Some("contradicted") | Some("drifted") => SemanticValidation::Contradicted,
        _ => SemanticValidation::Unreviewed,
    }
}

fn decision_lifecycle(status: Option<&str>) -> DecisionLifecycle {
    match status.map(str::trim).map(str::to_lowercase).as_deref() {
        Some("accepted") => DecisionLifecycle::Accepted,
        Some("superseded") => DecisionLifecycle::Superseded,
        Some("withdrawn") => DecisionLifecycle::Withdrawn,
        _ => DecisionLifecycle::Proposed,
    }
}

/// `exclude_stale` per spec: exclude memories whose source currency is
/// `changed` AND semantic validation is not `verified`.
pub fn is_stale(f: &Freshness) -> bool {
    f.source_currency == "changed" && f.semantic_validation != "verified"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_applicable_distinct_from_unknown() {
        let na = compute(&FreshnessInputs {
            has_governed_path: false,
            ..Default::default()
        });
        let un = compute(&FreshnessInputs {
            has_governed_path: true,
            ..Default::default()
        });
        assert_eq!(na.source_currency, SourceCurrency::NotApplicable);
        assert_eq!(un.source_currency, SourceCurrency::Unknown);
        assert_ne!(na.source_currency, un.source_currency);
    }

    #[test]
    fn changed_hash_means_changed() {
        let f = compute(&FreshnessInputs {
            has_governed_path: true,
            stored_file_sha256: Some("aaa".into()),
            current_file_sha256: Some("bbb".into()),
            ..Default::default()
        });
        assert_eq!(f.source_currency, SourceCurrency::Changed);
    }

    #[test]
    fn unchanged_when_hashes_match() {
        let f = compute(&FreshnessInputs {
            has_governed_path: true,
            stored_file_sha256: Some("aaa".into()),
            current_file_sha256: Some("aaa".into()),
            ..Default::default()
        });
        assert_eq!(f.source_currency, SourceCurrency::Unchanged);
    }

    #[test]
    fn decision_lifecycle_independent_of_anchor_drift() {
        let f = compute(&FreshnessInputs {
            has_governed_path: true,
            stored_file_sha256: Some("aaa".into()),
            current_file_sha256: Some("bbb".into()),
            decision_status: Some("accepted".into()),
            ..Default::default()
        });
        assert_eq!(f.source_currency, SourceCurrency::Changed);
        assert_eq!(f.decision_lifecycle, DecisionLifecycle::Accepted);
    }
}
