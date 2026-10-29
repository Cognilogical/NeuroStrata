//! Federation helpers (in-house): cross-instance query projection.
use serde_json::Value;

pub fn project(rows: &[Value], fields: &[&str]) -> Vec<Value> {
    rows.iter().map(|r| {
        let mut out = serde_json::Map::new();
        for f in fields {
            if let Some(v) = r.get(f) { out.insert(f.to_string(), v.clone()); }
        }
        Value::Object(out)
    }).collect()
}

pub fn merge(results: &[Vec<Value>]) -> Vec<Value> {
    results.iter().flatten().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_fields() {
        let rows = vec![serde_json::json!({"a":1,"b":2})];
        let p = project(&rows, &["a"]);
        assert_eq!(p[0].get("a").unwrap(), 1);
        assert!(p[0].get("b").is_none());
    }
    #[test]
    fn merge_results() {
        let r = merge(&[vec![serde_json::json!(1)], vec![serde_json::json!(2)]]);
        assert_eq!(r.len(), 2);
    }
}
