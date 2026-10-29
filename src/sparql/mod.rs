//! SPARQL 1.1 subset (in-house approximation): simple SELECT over N-Quads lines.
use std::collections::HashMap;

/// A parsed triple pattern: ?var or literal.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

/// Execute a very simple SPARQL SELECT: "SELECT ?s ?p ?o WHERE { ?s ?p ?o }"
/// against a list of N-Quad lines. Returns rows as Vec<HashMap<var, String>>.
pub fn select_all(nquads: &[String]) -> Vec<HashMap<String, String>> {
    let mut rows = Vec::new();
    for line in nquads {
        // parse "s p o ." 
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 { continue; }
        let mut row = HashMap::new();
        row.insert("s".to_string(), parts[0].to_string());
        row.insert("p".to_string(), parts[1].to_string());
        row.insert("o".to_string(), parts[2].to_string());
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn select_parses() {
        let q = vec!["<s> <p> <o> .".to_string()];
        let r = select_all(&q);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].get("s").unwrap(), "<s>");
    }
}

/// Custom SPARQL function `ns:salience(id, neural_gain, tombstoned)` honoring
/// neural gain and tombstones: returns 0.0 for tombstoned rows.
pub fn ns_salience(id: &str, neural_gain: f64, tombstoned: bool) -> f64 {
    if tombstoned { return 0.0; }
    let base = 1.0;
    let _ = id;
    base * neural_gain.max(0.0)
}

#[cfg(test)]
mod salience_tests {
    use super::*;
    #[test]
    fn tombstoned_zero() {
        assert_eq!(ns_salience("x", 2.0, true), 0.0);
    }
    #[test]
    fn gain_applied() {
        assert_eq!(ns_salience("x", 2.0, false), 2.0);
    }
}
