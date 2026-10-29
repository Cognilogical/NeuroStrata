//! SPARQL 1.1 subset (in-house): SELECT over N-Quad lines with basic WHERE.
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Pattern {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

#[derive(Debug, Clone)]
pub struct SelectQuery {
    pub vars: Vec<String>,
    pub patterns: Vec<Pattern>,
}

/// Parse a very small SPARQL SELECT: "SELECT ?s ?p ?o WHERE { ?s ?p ?o }"
pub fn parse_select(q: &str) -> Option<SelectQuery> {
    let q = q.trim();
    let upper = q.to_uppercase();
    if !upper.starts_with("SELECT") { return None; }
    let where_idx = upper.find("WHERE")?;
    let vars_part = &q[6..where_idx];
    let vars: Vec<String> = vars_part.split_whitespace().map(|s| s.trim_start_matches('?').to_string()).collect();
    let rest = &q[where_idx..];
    let start = rest.find('{')?;
    let end = rest.rfind('}')?;
    let body = &rest[start+1..end];
    let mut patterns = Vec::new();
    for triple in body.split('.') {
        let toks: Vec<&str> = triple.split_whitespace().collect();
        if toks.len() < 3 { continue; }
        patterns.push(Pattern { subject: toks[0].to_string(), predicate: toks[1].to_string(), object: toks[2].to_string() });
    }
    Some(SelectQuery { vars, patterns })
}

/// Execute SELECT over N-Quad lines. Returns rows.
pub fn execute(q: &SelectQuery, nquads: &[String]) -> Vec<HashMap<String, String>> {
    let mut rows = Vec::new();
    for line in nquads {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 { continue; }
        let s = parts[0]; let p = parts[1]; let o = parts[2];
        // match first pattern only (subset)
        if let Some(pat) = q.patterns.first() {
            if !match_term(&pat.subject, s) { continue; }
            if !match_term(&pat.predicate, p) { continue; }
            if !match_term(&pat.object, o) { continue; }
        }
        let mut row = HashMap::new();
        for v in &q.vars {
            let val = match v.as_str() {
                "s" => s, "p" => p, "o" => o, _ => "",
            };
            row.insert(v.clone(), val.to_string());
        }
        rows.push(row);
    }
    rows
}

fn match_term(pat: &str, val: &str) -> bool {
    if pat.starts_with('?') { return true; }
    pat == val
}

pub fn select_all(nquads: &[String]) -> Vec<HashMap<String, String>> {
    let q = SelectQuery { vars: vec!["s".into(),"p".into(),"o".into()], patterns: vec![Pattern{subject:"?s".into(),predicate:"?p".into(),object:"?o".into()}] };
    execute(&q, nquads)
}

pub fn ns_salience(id: &str, neural_gain: f64, tombstoned: bool) -> f64 {
    if tombstoned { return 0.0; }
    let _ = id;
    1.0 * neural_gain.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_and_execute() {
        let q = parse_select("SELECT ?s ?p ?o WHERE { ?s ?p ?o }").unwrap();
        let rows = execute(&q, &["<s> <p> <o> .".to_string()]);
        assert_eq!(rows.len(), 1);
    }
    #[test]
    fn filter_object() {
        let q = parse_select("SELECT ?s WHERE { ?s <p> <o> }").unwrap();
        let rows = execute(&q, &["<s> <p> <o> .".to_string(), "<x> <p> <y> .".to_string()]);
        assert_eq!(rows.len(), 1);
    }
    #[test]
    fn salience() {
        assert_eq!(ns_salience("x", 2.0, true), 0.0);
        assert_eq!(ns_salience("x", 2.0, false), 2.0);
    }
}
