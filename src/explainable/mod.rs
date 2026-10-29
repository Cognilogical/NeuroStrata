//! Explainable search results (in-house).
#[derive(Debug, Clone)]
pub struct Explanation { pub memory_id: String, pub score: f32, pub matched_terms: Vec<String>, pub reason: String }
pub fn explain(memory_id: &str, score: f32, matched_terms: &[&str]) -> Explanation {
    Explanation {
        memory_id: memory_id.to_string(),
        score,
        matched_terms: matched_terms.iter().map(|s| s.to_string()).collect(),
        reason: format!("matched {} terms, score {}", matched_terms.len(), score),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explain_basic() {
        let e = explain("m1", 1.5, &["foo"]);
        assert!(e.reason.contains("1 terms"));
    }
}
