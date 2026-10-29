//! RDF-star quoted triple provenance (in-house).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotedTriple { pub subject: String, pub predicate: String, pub object: String, pub asserted_by: Option<String>, pub valid_from: Option<i64> }

impl QuotedTriple {
    pub fn new(s: &str, p: &str, o: &str) -> Self {
        Self { subject: s.to_string(), predicate: p.to_string(), object: o.to_string(), asserted_by: None, valid_from: None }
    }
    pub fn to_nquads(&self) -> String {
        format!("<< {} {} {} >>", self.subject, self.predicate, self.object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted() {
        let mut q = QuotedTriple::new("<s>","<p>","<o>");
        q.asserted_by = Some("src".into());
        assert!(q.to_nquads().starts_with("<<"));
    }
}
