//! RAG fusion (in-house): reciprocal rank fusion of multiple ranked lists.
pub fn rrf(lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    use std::collections::HashMap;
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            let s = 1.0 / (k + (rank as f64) + 1.0);
            *scores.entry(id.clone()).or_insert(0.0) += s;
        }
    }
    let mut v: Vec<(String, f64)> = scores.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuses() {
        let l1 = vec!["a".into(), "b".into()];
        let l2 = vec!["b".into(), "c".into()];
        let r = rrf(&[l1, l2], 60.0);
        assert!(r.iter().any(|(id, _)| id == "b"));
    }
}
