//! Personalized PageRank (in-house approximation).
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub fn personalized_pagerank(
    nodes: &[String],
    edges: &[(String, String, f64)],
    seed: &str,
    damping: f64,
    iters: usize,
) -> HashMap<String, f64> {
    let n = nodes.len();
    if n == 0 { return HashMap::new(); }
    let idx: HashMap<String, usize> = nodes.iter().enumerate().map(|(i, x)| (x.clone(), i)).collect();
    let mut adj: Vec<Vec<(usize, f64)>> = vec![vec![]; n];
    for (u, v, w) in edges {
        if let (Some(&i), Some(&j)) = (idx.get(u), idx.get(v)) {
            adj[i].push((j, *w));
            adj[j].push((i, *w));
        }
    }
    // out-degree sums
    let out_sum: Vec<f64> = (0..n).map(|i| adj[i].iter().map(|x| x.1).sum::<f64>().max(1e-9)).collect();
    let seed_idx = idx.get(seed).copied();
    let mut pr = vec![0.0; n];
    if let Some(si) = seed_idx { pr[si] = 1.0; } else if n > 0 { pr[0] = 1.0; }
    for _ in 0..iters {
        let mut next = vec![0.0; n];
        for i in 0..n {
            for &(j, w) in &adj[i] {
                next[j] += damping * pr[i] * (w / out_sum[i]);
            }
        }
        // teleport to seed
        if let Some(si) = seed_idx {
            next[si] += 1.0 - damping;
        }
        pr = next;
    }
    nodes.iter().enumerate().map(|(i, x)| (x.clone(), pr[i])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seed_ranks_highest() {
        let nodes = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let edges = vec![("a".into(), "b".into(), 1.0), ("b".into(), "c".into(), 1.0)];
        let pr = personalized_pagerank(&nodes, &edges, "a", 0.85, 20);
        assert!(pr["a"] > pr["c"]);
    }
}
