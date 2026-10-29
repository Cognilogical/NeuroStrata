//! Assertion identity + provenance (K3 addition).
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Assertion {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub transaction_time: i64,
}

impl Assertion {
    pub fn new(subject: &str, predicate: &str, object: &str) -> Self {
        Self {
            id: format!("assertion:{}", Uuid::now_v7()),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            source: None,
            valid_from: None,
            valid_to: None,
            transaction_time: chrono::Utc::now().timestamp(),
        }
    }
    pub fn with_source(mut self, source: &str) -> Self { self.source = Some(source.to_string()); self }
    pub fn with_validity(mut self, from: i64, to: i64) -> Self { self.valid_from = Some(from); self.valid_to = Some(to); self }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creates_id() {
        let a = Assertion::new("s","p","o").with_source("src");
        assert!(a.id.starts_with("assertion:"));
        assert_eq!(a.source.as_deref(), Some("src"));
    }
}
