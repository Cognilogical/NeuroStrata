//! RAG fusion helpers (in-house).
pub fn rrf(lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    use std::collections::BTreeMap;
    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    for list in lists {
        for (i, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + i as f64 + 1.0);
        }
    }
    let mut v: Vec<(String, f64)> = scores.into_iter().collect();
    v.sort_by(|a,b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}
pub fn fuse(lists: &[Vec<String>], weights: &[f64]) -> Vec<(String, f64)> {
    use std::collections::BTreeMap;
    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    for (li, list) in lists.iter().enumerate() {
        let w = weights.get(li).copied().unwrap_or(1.0);
        for (i, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += w / (i as f64 + 1.0);
        }
    }
    let mut v: Vec<(String, f64)> = scores.into_iter().collect();
    v.sort_by(|a,b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rrf_basic() {
        let lists = vec![vec!["a".into(),"b".into()], vec!["b".into(),"c".into()]];
        let r = rrf(&lists, 60.0);
        assert!(!r.is_empty());
    }
    #[test]
    fn fuse_weights() {
        let lists = vec![vec!["a".into()], vec!["b".into()]];
        let r = fuse(&lists, &[2.0,1.0]);
        assert_eq!(r[0].0, "a");
    }
}
