//! Personalized PageRank (in-house).
use std::collections::{BTreeMap, BTreeSet};

pub fn personalized_pagerank(
    adj: &BTreeMap<String, BTreeSet<String>>,
    seed: &str,
    damping: f64,
    iters: usize,
) -> BTreeMap<String, f64> {
    let nodes: Vec<String> = adj.keys().cloned().collect();
    let n = nodes.len().max(1) as f64;
    let mut rank: BTreeMap<String, f64> = nodes.iter().map(|x| (x.clone(), 1.0/n)).collect();
    for _ in 0..iters {
        let mut next: BTreeMap<String, f64> = nodes.iter().map(|x| (x.clone(), (1.0-damping)/n)).collect();
        for u in &nodes {
            if let Some(outs) = adj.get(u) {
                let deg = outs.len().max(1) as f64;
                for v in outs {
                    *next.entry(v.clone()).or_insert(0.0) += damping * rank.get(u).copied().unwrap_or(0.0) / deg;
                }
            }
        }
        // seed boost
        *next.entry(seed.to_string()).or_insert(0.0) += 0.1;
        rank = next;
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks() {
        let mut adj = BTreeMap::new();
        adj.insert("a".into(), BTreeSet::from(["b".into()]));
        adj.insert("b".into(), BTreeSet::from(["a".into()]));
        let r = personalized_pagerank(&adj, "a", 0.85, 10);
        assert!(r.contains_key("a"));
    }
}
