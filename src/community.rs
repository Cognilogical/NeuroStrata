//! `community` — deterministic Leiden community detection.
//! Spec: docs/superpowers/specs/2026-10-27-community-detection-design.md (APPROVED 2026-10-28).

mod inner {
    
    use leiden_rs::{GraphDataBuilder, Leiden, LeidenConfig, QualityType};
    use std::collections::{BTreeMap, HashMap};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Community {
        pub id: u32,
        pub member_ids: Vec<String>,
        pub hub_id: String,
        pub size: usize,
    }

    pub fn detect_communities(
        node_ids: &[String],
        edges: &[(String, String, f32)],
    ) -> anyhow::Result<Vec<Community>> {
        // Sort deterministically and dedup (spec §5.7: sorted node order)
        let mut sorted: Vec<String> = node_ids.to_vec();
        sorted.sort();
        sorted.dedup();

        if sorted.is_empty() {
            return Ok(Vec::new());
        }

        // Node id -> index (sorted order = deterministic CSR adjacency)
        let idx: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();

        let n = sorted.len();
        let mut builder = GraphDataBuilder::new(n);

        // Build weighted undirected graph: sum multi-edges, add edges in BTreeMap
        // key order so GraphDataBuilder adjacency insertion order is deterministic
        // (spec §5.7: floating point summation order fixed by sorted iteration).
        let mut wmap: BTreeMap<(usize, usize), f32> = BTreeMap::new();
        for (src, tgt, w) in edges {
            if let (Some(&si), Some(&ti)) = (idx.get(src), idx.get(tgt)) {
                if si == ti {
                    continue; // ignore self-loops
                }
                let (a, b) = if si < ti { (si, ti) } else { (ti, si) };
                *wmap.entry((a, b)).or_insert(0.0) += w;
            }
        }
        for ((a, b), weight) in wmap {
            builder.add_edge(a, b, weight as f64).map_err(|e| {
                anyhow::anyhow!("leiden edge add failed ({a}->{b}): {e}")
            })?;
        }
        let graph = builder.build().map_err(|e| anyhow::anyhow!("leiden graph build: {e}"))?;

        let config = LeidenConfig {
            seed: Some(42),
            max_iterations: 10,
            resolution: 1.0,
            quality: QualityType::Modularity,
            epsilon: 0.001,
            skip_refinement: false,
            min_iterations: 1,
            ..LeidenConfig::default()
        };
        let leiden = Leiden::new(config);
        let result = leiden.run(&graph).map_err(|e| anyhow::anyhow!("leiden run: {e}"))?;
        let partition = result.partition;

        // Collect communities from nodes_in_community (sorted node indices).
        let mut groups: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for comm_id in 0..partition.num_communities() {
            let nodes = partition.nodes_in_community(comm_id);
            for node_idx in nodes {
                groups
                    .entry(comm_id as u32)
                    .or_default()
                    .push(sorted[node_idx].clone());
            }
        }

        let mut communities: Vec<Community> = groups
            .into_values()
            .map(|mut members| {
                members.sort();
                Community {
                    id: 0,
                    member_ids: members,
                    hub_id: String::new(),
                    size: 0,
                }
            })
            .filter(|c| c.member_ids.len() >= 2)
            .collect();

        // Sort by size DESC, min_member_id ASC (spec §5.2a canonical numbering).
        communities.sort_by(|a, b| {
            b.member_ids
                .len()
                .cmp(&a.member_ids.len())
                .then_with(|| {
                    a.member_ids
                        .iter()
                        .min()
                        .unwrap()
                        .cmp(b.member_ids.iter().min().unwrap())
                })
        });

        // Assign deterministic ids and size.
        for (i, c) in communities.iter_mut().enumerate() {
            c.id = i as u32;
            c.size = c.member_ids.len();
            c.hub_id = c.member_ids.iter().min().cloned().unwrap_or_default();
        }

        Ok(communities)
    }

    /// Filter out structural memory types from community members.
    /// Structural types do not count toward size / hub / min_member_id (§5.2a).
    pub fn filter_structural(
        members: &[String],
        types: &HashMap<String, String>,
        structural_types: &[&str],
    ) -> Vec<String> {
        members
            .iter()
            .filter(|id| {
                types
                    .get(*id)
                    .map(|t| !structural_types.contains(&t.as_str()))
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }
}

/// Structural memory types excluded from community size/hub computation (§5.2a).
pub const COMMUNITY_STRUCTURAL_TYPES: [&str; 6] = [
    "directory",
    "file",
    "markdown",
    "symbol",
    "code_ast",
    "community_summary",
];

pub use inner::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_cliques_yield_two_communities() -> anyhow::Result<()> {
        // Fixture: clique A (n1,n2,n3), clique B (n4,n5,n6), single edge n3-n4.
        let nodes = vec![
            "c1".to_string(), "c2".to_string(), "c3".to_string(),
            "c4".to_string(), "c5".to_string(), "c6".to_string(),
        ];
        let edges = vec![
            ("c1".to_string(), "c2".to_string(), 1.0),
            ("c2".to_string(), "c3".to_string(), 1.0),
            ("c1".to_string(), "c3".to_string(), 1.0),
            ("c4".to_string(), "c5".to_string(), 1.0),
            ("c5".to_string(), "c6".to_string(), 1.0),
            ("c4".to_string(), "c6".to_string(), 1.0),
            ("c3".to_string(), "c4".to_string(), 0.5),
        ];

        let communities = detect_communities(&nodes, &edges)?;
        assert_eq!(
            communities.len(),
            2,
            "two cliques connected by one edge yields 2 communities"
        );


        // Determinism across runs with same input.
        let second = detect_communities(&nodes, &edges)?;
        assert_eq!(communities, second, "partition must be deterministic");

        // Determinism across shuffled input order (nodes + edges).
        let mut shuffled_nodes = nodes.clone();
        shuffled_nodes.reverse();
        let mut shuffled_edges: Vec<(String, String, f32)> = edges.clone();
        shuffled_edges.reverse();
        let third = detect_communities(&shuffled_nodes, &shuffled_edges)?;
        assert_eq!(communities, third, "partition must be order-independent");

        Ok(())
    }

    #[test]
    fn singleton_nodes_are_skipped() -> anyhow::Result<()> {
        let nodes = vec!["a".to_string(), "b".to_string()];
        let edges = vec![];
        let communities = detect_communities(&nodes, &edges)?;
        assert!(communities.is_empty(), "singletons produce no communities");
        Ok(())
    }

    #[test]
    fn empty_input_returns_empty() {
        let communities = detect_communities(&[], &[]).expect("empty input must not error");
        assert!(communities.is_empty());
    }
}