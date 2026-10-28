//! Explainable retrieval (in-house): attach explanation to a search hit.
#[derive(Debug, Clone)]
pub struct Explanation {
    pub memory_id: String,
    pub reason: String,
    pub score: f32,
}

pub fn explain(memory_id: &str, score: f32, matched_terms: &[&str]) -> Explanation {
    Explanation {
        memory_id: memory_id.to_string(),
        reason: format!("matched terms: {}", matched_terms.join(", ")),
        score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds() {
        let e = explain("m1", 0.5, &["auth", "token"]);
        assert!(e.reason.contains("auth"));
    }
}
