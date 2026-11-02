//! `community` — deterministic Leiden community detection.
//! Spec: docs/superpowers/specs/2026-10-27-community-detection-design.md (APPROVED 2026-10-28).

pub mod community_inner {

    use leiden_rs::{GraphDataBuilder, Leiden, LeidenConfig, QualityType};
    use std::collections::{BTreeMap, HashMap};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Community {
        pub id: u32,
        pub member_ids: Vec<String>,
        pub hub_id: String,
        pub size: usize,
    }

    /// Spec §5.2: `hub_id` is the member with the highest `access_count`,
    /// breaking ties by the lexicographically smallest id. Members absent from
    /// the map count as 0; empty member lists yield an empty hub.
    pub fn select_hub_id(member_ids: &[String], access_counts: &HashMap<String, u64>) -> String {
        member_ids
            .iter()
            .min_by(|a, b| {
                let ca = access_counts.get(a.as_str()).copied().unwrap_or(0);
                let cb = access_counts.get(b.as_str()).copied().unwrap_or(0);
                cb.cmp(&ca).then_with(|| a.cmp(b))
            })
            .cloned()
            .unwrap_or_default()
    }

    /// Recompute `hub_id` for every community from the supplied access counts.
    pub fn assign_hubs_by_access_count(
        communities: &mut [Community],
        access_counts: &HashMap<String, u64>,
    ) {
        for c in communities.iter_mut() {
            c.hub_id = select_hub_id(&c.member_ids, access_counts);
        }
    }

    /// Partition + converged quality, bundled for the resolution-parameterized
    /// entry point.
    #[derive(Debug, Clone)]
    pub struct CommunityResult {
        pub communities: Vec<Community>,
        pub quality: f64,
    }

    /// Resolution-parameterized community detection over an edge list only.
    /// The node set is derived from the edge endpoints, so isolated nodes are
    /// never silently folded into a phantom community.
    pub fn detect_communities_with_resolution(
        edges: &[(String, String, f64)],
        resolution: f64,
    ) -> anyhow::Result<CommunityResult> {
        let mut node_ids: Vec<String> = edges
            .iter()
            .flat_map(|(src, tgt, _)| [src.clone(), tgt.clone()])
            .collect();
        node_ids.sort();
        node_ids.dedup();
        let edge_f32: Vec<(String, String, f32)> = edges
            .iter()
            .map(|(s, t, w)| (s.clone(), t.clone(), *w as f32))
            .collect();
        let (communities, quality) =
            detect_communities_with_quality(&node_ids, &edge_f32, resolution)?;
        Ok(CommunityResult { communities, quality })
    }

    /// Detect communities AND return the partition quality (modularity) the
    /// Leiden run converged to (spec §5.2). `0.0` for empty input.
    /// The canonical detection entry point: the caller supplies the full node
    /// set (isolated nodes keep their slot in the partition) and the
    /// LeidenConfig resolution. `detect_communities_with_resolution` derives
    /// the node set from the edge endpoints and delegates here.
    pub fn detect_communities_with_quality(
        node_ids: &[String],
        edges: &[(String, String, f32)],
        resolution: f64,
    ) -> anyhow::Result<(Vec<Community>, f64)> {
        detect_inner(node_ids.to_vec(), edges.to_vec(), resolution)
    }

    fn detect_inner(
        node_ids: Vec<String>,
        edges: Vec<(String, String, f32)>,
        resolution: f64,
    ) -> anyhow::Result<(Vec<Community>, f64)> {
        // Sort deterministically and dedup (spec §5.7: sorted node order)
        let mut sorted: Vec<String> = node_ids;
        sorted.sort();
        sorted.dedup();

        if sorted.is_empty() {
            return Ok((Vec::new(), 0.0));
        }

        // Node id -> index (sorted order = deterministic CSR adjacency)
        let idx: HashMap<&str, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();

        let n = sorted.len();
        let mut builder = GraphDataBuilder::new(n);

        // Build weighted undirected graph: sum multi-edges, add edges in BTreeMap
        // key order so GraphDataBuilder adjacency insertion order is deterministic
        // (spec §5.7: floating point summation order fixed by sorted iteration).
        let mut wmap: BTreeMap<(usize, usize), f32> = BTreeMap::new();
        for (src, tgt, w) in &edges {
            if let (Some(&si), Some(&ti)) = (idx.get(src.as_str()), idx.get(tgt.as_str())) {
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
            resolution,
            quality: QualityType::Modularity,
            epsilon: 0.001,
            skip_refinement: false,
            min_iterations: 1,
            ..LeidenConfig::default()
        };
        let leiden = Leiden::new(config);
        let result = leiden.run(&graph).map_err(|e| anyhow::anyhow!("leiden run: {e}"))?;
        let quality = result.quality;
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
            // Empty access-count map → all counts 0 → smallest id wins,
            // identical to the previous `member_ids.iter().min()` behavior.
            c.hub_id = select_hub_id(&c.member_ids, &HashMap::new());
        }

        Ok((communities, quality))
    }

    /// Filter out structural node ids from community members.
    /// Structural types do not count toward size / hub / min_member_id (§5.2a).
    /// The filter runs first, then `hub_id` is RECOMPUTED from the surviving
    /// members, so the hub of a filtered community is never an excluded
    /// structural node. Hub selection delegates to `assign_hubs_by_access_count`
    /// — the single canonical hub-assignment implementation.
    pub fn filter_structural_with_access_counts(
        mut communities: Vec<Community>,
        structural_ids: &std::collections::HashSet<String>,
        access_counts: &HashMap<String, u64>,
    ) -> Vec<Community> {
        for c in communities.iter_mut() {
            c.member_ids.retain(|id| !structural_ids.contains(id));
            c.size = c.member_ids.len();
        }
        assign_hubs_by_access_count(&mut communities, access_counts);
        communities
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

pub use community_inner::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

        let (communities, _quality) = detect_communities_with_quality(&nodes, &edges, 1.0)?;
        assert_eq!(
            communities.len(),
            2,
            "two cliques connected by one edge yields 2 communities"
        );


        // Determinism across runs with same input.
        let second = detect_communities_with_quality(&nodes, &edges, 1.0).map(|(c, _)| c)?;
        assert_eq!(communities, second, "partition must be deterministic");

        // Determinism across shuffled input order (nodes + edges).
        let mut shuffled_nodes = nodes.clone();
        shuffled_nodes.reverse();
        let mut shuffled_edges: Vec<(String, String, f32)> = edges.clone();
        shuffled_edges.reverse();
        let third = detect_communities_with_quality(&shuffled_nodes, &shuffled_edges, 1.0).map(|(c, _)| c)?;
        assert_eq!(communities, third, "partition must be order-independent");

        Ok(())
    }

    #[test]
    fn singleton_nodes_are_skipped() -> anyhow::Result<()> {
        let nodes = vec!["a".to_string(), "b".to_string()];
        let edges = vec![];
        let (communities, _quality) = detect_communities_with_quality(&nodes, &edges, 1.0)?;
        assert!(communities.is_empty(), "singletons produce no communities");
        Ok(())
    }

    #[test]
    fn empty_input_returns_empty() {
        let (communities, quality) =
            detect_communities_with_quality(&[], &[], 1.0).expect("empty input must not error");
        assert!(communities.is_empty());
        assert_eq!(quality, 0.0);
    }

    #[test]
    fn resolution_flows_into_leiden_detection() -> anyhow::Result<()> {
        // Two cliques (a,b,c) and (d,e,f) joined by one weak bridge. γ=0.1
        // keeps the cliques; γ=5.0 shatters every node into a singleton, so
        // the resolution really reaches LeidenConfig (was discarded before).
        let mut edges: Vec<(String, String, f64)> = vec![
            ("a", "b"), ("b", "c"), ("a", "c"), ("d", "e"), ("e", "f"), ("d", "f"),
        ].into_iter().map(|(s, t)| (s.into(), t.into(), 1.0)).collect();
        edges.push(("c".into(), "d".into(), 0.3));

        let low = detect_communities_with_resolution(&edges, 0.1)?;
        assert_eq!(
            low.communities.len(),
            2,
            "γ=0.1 keeps both cliques as communities"
        );
        assert!(low.quality > 0.0, "the clique split has positive modularity");

        let high = detect_communities_with_resolution(&edges, 5.0)?;
        assert!(
            high.communities.is_empty(),
            "γ=5.0 shatters the graph into singletons, which can never form a ≥2 community"
        );
        assert!(
            high.quality < 0.0,
            "a shattered singleton partition has negative modularity"
        );

        // Determinism: identical inputs give the identical partition.
        let again = detect_communities_with_resolution(&edges, 0.1)?;
        assert_eq!(low.communities, again.communities);

        // Empty edge list: no derived nodes, no community, zero quality.
        let none = detect_communities_with_resolution(&[], 1.0)?;
        assert!(none.communities.is_empty());
        assert_eq!(none.quality, 0.0);
        Ok(())
    }

    #[test]
    fn filter_structural_recomputes_hub_from_survivors() {
        use std::collections::HashSet;
        let members = vec![
            "aaa_struct".to_string(),
            "bbb_rule".to_string(),
            "ccc_rule".to_string(),
        ];
        let types: HashMap<String, String> = [
            ("aaa_struct".to_string(), "symbol".to_string()),
            ("bbb_rule".to_string(), "rule".to_string()),
            ("ccc_rule".to_string(), "rule".to_string()),
        ]
        .into_iter()
        .collect();

        // Structural ids derived from the type map, mirroring server.rs.
        let structural_ids: HashSet<String> = types
            .iter()
            .filter(|(_, t)| COMMUNITY_STRUCTURAL_TYPES.contains(&t.as_str()))
            .map(|(id, _)| id.clone())
            .collect();

        let community = Community {
            id: 0,
            member_ids: members,
            hub_id: "aaa_struct".to_string(),
            size: 3,
        };

        // Empty access-count map → tie breaks to the smallest surviving id.
        let out = filter_structural_with_access_counts(
            vec![community],
            &structural_ids,
            &HashMap::new(),
        );

        assert_eq!(out[0].member_ids, vec!["bbb_rule", "ccc_rule"]);
        assert_eq!(
            out[0].hub_id, "bbb_rule",
            "hub must be recomputed after the structural filter, never a structural node"
        );
        assert_eq!(out[0].size, 2);
    }

    #[test]
    fn hub_selection_prefers_highest_access_count_then_smallest_id() {
        let members = vec!["b".to_string(), "c".to_string(), "a".to_string()];

        // Empty map → all counts 0 → lexicographically smallest id wins.
        let empty = HashMap::new();
        assert_eq!(select_hub_id(&members, &empty), "a");
        assert_eq!(select_hub_id(&[], &empty), "");

        // Highest access_count wins regardless of id order.
        let mut counts = HashMap::new();
        counts.insert("c".to_string(), 7u64);
        counts.insert("a".to_string(), 3u64);
        counts.insert("b".to_string(), 1u64);
        assert_eq!(select_hub_id(&members, &counts), "c");

        // Ties break to the smallest id, not to input order.
        let mut tied = HashMap::new();
        tied.insert("c".to_string(), 5u64);
        tied.insert("a".to_string(), 5u64);
        tied.insert("b".to_string(), 5u64);
        assert_eq!(select_hub_id(&members, &tied), "a");

        // assign_hubs_by_access_count applies the same rule per community.
        let mut communities = vec![
            Community {
                id: 0,
                member_ids: members.clone(),
                hub_id: "b".to_string(),
                size: 3,
            },
            Community {
                id: 1,
                member_ids: vec!["x".to_string(), "y".to_string()],
                hub_id: "x".to_string(),
                size: 2,
            },
        ];
        assign_hubs_by_access_count(&mut communities, &counts);
        assert_eq!(communities[0].hub_id, "c");
        // Missing counts default to 0 → tie-break to "x".
        assert_eq!(communities[1].hub_id, "x");
    }

    #[test]
    fn filter_structural_with_access_counts_retains_and_recomputes_hubs() {
        use std::collections::HashSet;
        let communities = vec![
            Community {
                id: 0,
                member_ids: vec!["s1".to_string(), "b".to_string(), "a".to_string()],
                hub_id: "s1".to_string(),
                size: 3,
            },
        ];
        let structural: HashSet<String> = ["s1".to_string()].into_iter().collect();
        let mut counts = HashMap::new();
        counts.insert("a".to_string(), 2u64);
        counts.insert("b".to_string(), 9u64);

        let out = filter_structural_with_access_counts(communities, &structural, &counts);
        assert_eq!(out[0].member_ids, vec!["b", "a"]);
        assert_eq!(out[0].hub_id, "b", "highest access_count survivor wins");
        assert_eq!(out[0].size, 2);
    }
}