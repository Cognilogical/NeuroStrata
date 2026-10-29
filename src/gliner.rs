//! GLiNER-style entity candidate emitter (review-only).
//!
//! In-house deterministic fallback: extracts capitalized phrase candidates from
//! prose without ONNX. Full ONNX sidecar is feature-gated (`gliner` feature)
//! and remains a future enhancement; this module never affects search/dedup/guard.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlinerCandidate {
    pub text: String,
    pub label: String,
    pub score: f32,
    pub review_only: bool,
}

/// Extract capitalized multi-word phrases as entity candidates (review-only).
pub fn extract_candidates(text: &str) -> Vec<GlinerCandidate> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| !c.is_alphanumeric());
        if trimmed.len() >= 3 && trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            out.push(GlinerCandidate {
                text: trimmed.to_string(),
                label: "ENTITY".to_string(),
                score: 0.5,
                review_only: true,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_capitalized() {
        let c = extract_candidates("Hello World foo");
        assert!(!c.is_empty());
        assert!(c.iter().all(|x| x.review_only));
    }
    #[test]
    fn empty_for_lowercase() {
        let c = extract_candidates("hello world");
        assert!(c.is_empty());
    }
}
