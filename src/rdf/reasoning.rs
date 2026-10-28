use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Deterministic transitive closure over a directed adjacency map.
/// Used for RDFS subClassOf*/subPropertyOf* and bounded transitive GOVERNS.
pub fn transitive_closure(adj: &BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for key in adj.keys() {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(key.clone());
        while let Some(cur) = queue.pop_front() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(nexts) = adj.get(&cur) {
                for n in nexts {
                    if !seen.contains(n) {
                        queue.push_back(n.clone());
                    }
                }
            }
        }
        seen.remove(key);
        out.insert(key.clone(), seen);
    }
    out
}

/// Bounded BFS expansion from a seed over adjacency, respecting a hop budget.
pub fn bounded_expand(
    seed: &str,
    adj: &BTreeMap<String, BTreeSet<String>>,
    max_hops: usize,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((seed.to_string(), 0));
    while let Some((cur, depth)) = queue.pop_front() {
        if depth > max_hops {
            continue;
        }
        if !out.insert(cur.clone()) {
            continue;
        }
        if depth == max_hops {
            continue;
        }
        if let Some(nexts) = adj.get(&cur) {
            for n in nexts {
                if !out.contains(n) {
                    queue.push_back((n.clone(), depth + 1));
                }
            }
        }
    }
    out.remove(seed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitive_closure_computes_reachability() {
        let mut adj = BTreeMap::new();
        adj.insert("a".into(), BTreeSet::from(["b".into()]));
        adj.insert("b".into(), BTreeSet::from(["c".into()]));
        adj.insert("c".into(), BTreeSet::new());
        let cl = transitive_closure(&adj);
        assert!(cl.get("a").unwrap().contains("b"));
        assert!(cl.get("a").unwrap().contains("c"));
        assert!(cl.get("b").unwrap().contains("c"));
    }

    #[test]
    fn bounded_expand_respects_hop_limit() {
        let mut adj = BTreeMap::new();
        adj.insert("a".into(), BTreeSet::from(["b".into()]));
        adj.insert("b".into(), BTreeSet::from(["c".into()]));
        adj.insert("c".into(), BTreeSet::from(["d".into()]));
        let r = bounded_expand("a", &adj, 1);
        assert!(r.contains("b"));
        assert!(!r.contains("c"));
    }
}

/// Query budget for bounded reasoning operations (OWL/RDFS spec).
#[derive(Debug, Clone)]
pub struct QueryBudget {
    pub max_ms: u64,
    pub max_nodes: usize,
}

impl Default for QueryBudget {
    fn default() -> Self {
        Self { max_ms: 50, max_nodes: 256 }
    }
}

impl QueryBudget {
    pub fn check_nodes(&self, n: usize) -> bool {
        n <= self.max_nodes
    }
}
