//! Community detection (Louvain) over the memory graph — in-house.
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Default, Clone)]
pub struct Graph { pub nodes: BTreeSet<String>, pub edges: Vec<(String, String, f64)> }

impl Graph {
    pub fn new() -> Self { Self::default() }
    pub fn add_node(&mut self, n: String) { self.nodes.insert(n); }
    pub fn add_edge(&mut self, u: String, v: String, w: f64) {
        self.nodes.insert(u.clone()); self.nodes.insert(v.clone()); self.edges.push((u,v,w));
    }
    pub fn total_weight(&self) -> f64 { self.edges.iter().map(|e| e.2).sum() }
}

pub fn louvain(graph: &Graph) -> HashMap<String, usize> {
    let nodes: Vec<String> = graph.nodes.iter().cloned().collect();
    let n = nodes.len();
    if n == 0 { return HashMap::new(); }
    let idx: HashMap<String, usize> = nodes.iter().enumerate().map(|(i,x)|(x.clone(),i)).collect();
    let mut w = vec![vec![0.0;n]; n];
    for (u,v,wt) in &graph.edges {
        if let (Some(&i),Some(&j)) = (idx.get(u), idx.get(v)) { w[i][j]+=wt; w[j][i]+=wt; }
    }
    let k: Vec<f64> = (0..n).map(|i|(0..n).map(|j|w[i][j]).sum()).collect();
    let m2: f64 = k.iter().sum();
    if m2 == 0.0 { return nodes.iter().enumerate().map(|(i,x)|(x.clone(),i)).collect(); }
    let mut comm: Vec<usize> = (0..n).collect();
    let mut improved = true; let mut passes = 0;
    while improved && passes < 50 {
        improved = false; passes += 1;
        for i in 0..n {
            let ci = comm[i];
            let mut best = ci; let mut best_gain = 0.0f64;
            let mut cands: BTreeSet<usize> = BTreeSet::new();
            for j in 0..n { if w[i][j] > 0.0 { cands.insert(comm[j]); } }
            for &c in &cands {
                if c == ci { continue; }
                // modularity gain approximation
                let mut gain = 0.0f64;
                for j in 0..n {
                    if comm[j] == c { gain += w[i][j]; }
                    if comm[j] == ci { gain -= w[i][j]; }
                }
                if gain > best_gain { best_gain = gain; best = c; }
            }
            if best != ci { comm[i] = best; improved = true; }
        }
    }
    nodes.iter().enumerate().map(|(i,x)|(x.clone(), comm[i])).collect()
}

/// Hierarchical summary: community id -> member nodes.
pub fn community_members(graph: &Graph, comm: &HashMap<String, usize>) -> BTreeMap<usize, Vec<String>> {
    let mut out: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (n, c) in comm { out.entry(*c).or_default().push(n.clone()); }
    for v in out.values_mut() { v.sort(); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic() {
        let mut g = Graph::new();
        g.add_edge("a".into(),"b".into(),1.0);
        g.add_edge("c".into(),"d".into(),1.0);
        let c = louvain(&g);
        assert!(!c.is_empty());
    }
    #[test]
    fn members() {
        let mut g = Graph::new();
        g.add_edge("a".into(),"b".into(),1.0);
        let c = louvain(&g);
        let m = community_members(&g, &c);
        assert!(!m.is_empty());
    }
}
