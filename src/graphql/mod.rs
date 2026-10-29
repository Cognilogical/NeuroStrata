//! Curated read-only GraphQL-ish query shim (in-house).
use serde_json::{json, Value};

pub fn execute(query: &str) -> String {
    // Minimal: echo query as a JSON object. Real schema generation is future work.
    json!({"query": query, "data": null}).to_string()
}

pub fn schema_from_vocabulary(vocab: &[&str]) -> Value {
    let fields: Vec<Value> = vocab.iter().map(|v| json!({"name": v, "type": "String"})).collect();
    json!({"type": "Memory", "fields": fields})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_builds() {
        let s = schema_from_vocabulary(&["content","memory_type"]);
        assert!(s["fields"].as_array().unwrap().len() == 2);
    }
    #[test]
    fn execute_returns_json() {
        let r = execute("{ memory { content } }");
        assert!(r.contains("query"));
    }
}
