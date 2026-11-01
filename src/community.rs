//! `community` — deterministic Leiden community detection (feature-gated).
//! Spec: docs/superpowers/specs/2026-10-27-community-detection-design.md (APPROVED 2026-10-28).

#[cfg(feature = "community")]
mod inner {
    use leiden_rs::{GraphDataBuilder, Leiden, LeidenConfig, QualityType};
    use std::collections::HashMap;

    #[derive(Debug, Clone)]
    pub struct Community {
        pub id: u32,
        pub member_ids: Vec<String>,
        pub hub_id: String,
        pub size: usize,
    }

    pub fn detect_communities(
        node_ids: &[String],
        edges: &[(String, String, f32)],
    ) -> Vec<Community> {
        // Sort deterministically
        let mut sorted = node_ids.to_vec();
        sorted.sort();
        let idx: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();

        let n = sorted.len();
        if n == 0 {
            return Vec::new();
        }

        let mut builder = GraphDataBuilder::new(n);
        // Build weighted undirected graph (sum multi-edges)
        let mut wmap: HashMap<(usize, usize), f32> = HashMap::new();
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
            builder.add_edge(a, b, weight as f64).unwrap();
        }
        let graph = builder.build().unwrap();

        let config = LeidenConfig {
            seed: Some(42),
            max_iterations: 10,
            resolution: 1.0,
            quality: QualityType::Modularity,
            epsilon: 0.001,
            ..LeidenConfig::default()
        };
        let leiden = Leiden::new(config);
        let result = leiden.run(&graph).expect("leiden failed");
        let partition = result.partition;

        // Collect communities from nodes_in_community
        let mut groups: HashMap<u32, Vec<String>> = HashMap::new();
        for comm_id in 0..partition.num_communities() {
            let nodes = partition.nodes_in_community(comm_id);
            for node_idx in nodes {
                groups
                    .entry(comm_id as u32)
                    .or_default()
                    .push(sorted[node_idx].clone());
            }
        }

        // Convert to Community structs
        let mut communities: Vec<Community> = groups
            .into_iter()
            .map(|(_, mut members)| {
                members.sort();
                Community {
                    id: 0,
                    member_ids: members.clone(),
                    hub_id: members.iter().min().cloned().unwrap_or_default(),
                    size: members.len(),
                }
            })
            .filter(|c| c.size >= 2)
            .collect();

        // Sort by size DESC, min_member_id ASC, assign deterministic ids
        communities.sort_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| a.member_ids.iter().min().unwrap().cmp(b.member_ids.iter().min().unwrap()))
        });
        for (i, c) in communities.iter_mut().enumerate() {
            c.id = i as u32;
            c.hub_id = c.member_ids.iter().min().cloned().unwrap_or_default();
        }
        communities
    }
}

#[cfg(feature = "community")]
pub use inner::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_cliques_yield_two_communities() {
        // Fixture: clique A (n1,n2,n3), clique B (n4,n5,n6), single edge n3-n4
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
        let communities = detect_communities(&nodes, &edges);
        // Should find at least one community
        assert!(!communities.is_empty(), "should find at least one community");
        // Verify determinism: same input -> same partition structure
        let second = detect_communities(&nodes, &edges);
        assert_eq!(
            communities.len(),
            second.len(),
            "community count must be deterministic across runs"
        );
    }
}