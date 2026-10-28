//! Community detection (Louvain) over the memory graph — in-house approximation.
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A simple undirected graph for community detection.
#[derive(Debug, Default, Clone)]
pub struct Graph {
    pub nodes: BTreeSet<String>,
    pub edges: Vec<(String, String, f64)>, // (u, v, weight)
}

impl Graph {
    pub fn new() -> Self { Self::default() }
    pub fn add_node(&mut self, n: String) { self.nodes.insert(n); }
    pub fn add_edge(&mut self, u: String, v: String, w: f64) {
        self.nodes.insert(u.clone());
        self.nodes.insert(v.clone());
        self.edges.push((u, v, w));
    }

    /// Total edge weight (sum of all weights, counting each edge once).
    pub fn total_weight(&self) -> f64 {
        self.edges.iter().map(|e| e.2).sum()
    }
}

/// Run one pass of Louvain community detection. Returns a map node -> community id.
pub fn louvain(graph: &Graph) -> HashMap<String, usize> {
    let nodes: Vec<String> = graph.nodes.iter().cloned().collect();
    let n = nodes.len();
    if n == 0 { return HashMap::new(); }
    let idx: HashMap<String, usize> = nodes.iter().enumerate().map(|(i, x)| (x.clone(), i)).collect();

    let mut w: Vec<Vec<f64>> = vec![vec![0.0; n]; n];
    for (u, v, wt) in &graph.edges {
        if let (Some(&i), Some(&j)) = (idx.get(u), idx.get(v)) {
            w[i][j] += wt; w[j][i] += wt;
        }
    }
    let k: Vec<f64> = (0..n).map(|i| (0..n).map(|j| w[i][j]).sum()).collect();
    let m2: f64 = k.iter().sum();
    if m2 == 0.0 {
        return nodes.iter().enumerate().map(|(i, x)| (x.clone(), i)).collect();
    }

    let mut comm: Vec<usize> = (0..n).collect();
    let mut improved = true;
    let mut passes = 0;
    while improved && passes < 50 {
        improved = false;
        passes += 1;
        for i in 0..n {
            let ci = comm[i];
            // sum of weights from i to community c
            let mut best = ci;
            let mut best_gain = 0.0f64;
            // candidate communities
            let mut cands: BTreeSet<usize> = BTreeSet::new();
            for j in 0..n {
                if w[i][j] > 0.0 { cands.insert(comm[j]); }
            }
            cands.insert(ci);
            for &c in &cands {
                if c == ci { continue; }
                // compute gain
                let mut sigma_in: f64 = 0.0; // sum weights i->c
                let mut sigma_tot: f64 = 0.0; // sum k for c
                let mut sigma_in_ci: f64 = 0.0;
                let mut sigma_tot_ci: f64 = 0.0;
                for j in 0..n {
                    if comm[j] == c { sigma_in += w[i][j]; sigma_tot += k[j]; }
                    if comm[j] == ci { sigma_in_ci += w[i][j]; sigma_tot_ci += k[j]; }
                }
                // modularity gain (Newman)
                let gain = (sigma_in - sigma_in_ci) / m2
                    - ( (sigma_tot + k[i]) * (sigma_tot + k[i])
                      - sigma_tot * sigma_tot
                      - k[i] * k[i] ) / (m2 * m2);
                if gain > best_gain + 1e-12 {
                    best_gain = gain;
                    best = c;
                }
            }
            if best != ci {
                comm[i] = best;
                improved = true;
            }
        }
    }
    let mut map: HashMap<usize, usize> = HashMap::new();
    let mut next = 0;
    for c in &comm {
        if !map.contains_key(c) { map.insert(*c, next); next += 1; }
    }
    nodes.iter().enumerate().map(|(i, x)| (x.clone(), map[&comm[i]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn louvain_two_cliques() {
        let mut g = Graph::new();
        // two cliques connected by one edge
        for a in ["a","b","c"] {
            for b in ["a","b","c"] {
                if a < b { g.add_edge(a.to_string(), b.to_string(), 1.0); }
            }
        }
        for a in ["d","e","f"] {
            for b in ["d","e","f"] {
                if a < b { g.add_edge(a.to_string(), b.to_string(), 1.0); }
            }
        }
        g.add_edge("c".to_string(), "d".to_string(), 0.1);
        let com = louvain(&g);
        // a,b,c should share a community; d,e,f another
        assert_eq!(com["a"], com["b"]);
        assert_eq!(com["a"], com["c"]);
        assert_eq!(com["d"], com["e"]);
        assert_ne!(com["a"], com["d"]);
    }
    #[test]
    fn empty_graph() {
        let g = Graph::new();
        assert!(louvain(&g).is_empty());
    }
}
