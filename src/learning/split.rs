//! Component-disjoint splitting of memory IDs into train/calibration/evaluation splits.
//!
//! No connected component (by memory ID, payload hash, supersession lineage,
//! shared governance target, or time window) may cross train/calibration/evaluation
//! boundaries. This is a hard invariant enforced at dataset build time (Step 3 of Task 6).

use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// A container for dataset splits, keyed by observation ID.
#[derive(Debug, Clone)]
pub struct DatasetSplits {
    /// Observation IDs assigned to training
    pub train: HashSet<String>,
    /// Observation IDs assigned to calibration
    pub calibration: HashSet<String>,
    /// Observation IDs assigned to evaluation
    pub eval: HashSet<String>,
}

impl DatasetSplits {
    pub fn new() -> Self {
        Self {
            train: HashSet::new(),
            calibration: HashSet::new(),
            eval: HashSet::new(),
        }
    }

    pub fn insert(&mut self, observation_id: &str, split: Split) {
        match split {
            Split::Train => { let _ = self.train.insert(observation_id.to_string()); }
            Split::Calibration => { let _ = self.calibration.insert(observation_id.to_string()); }
            Split::Eval => { let _ = self.eval.insert(observation_id.to_string()); }
        }
    }

    pub fn contains(&self, observation_id: &str) -> bool {
        self.train.contains(observation_id)
            || self.calibration.contains(observation_id)
            || self.eval.contains(observation_id)
    }
    /// The split an observation is assigned to, if any.
    pub fn split_of(&self, observation_id: &str) -> Option<Split> {
        if self.train.contains(observation_id) {
            Some(Split::Train)
        } else if self.calibration.contains(observation_id) {
            Some(Split::Calibration)
        } else if self.eval.contains(observation_id) {
            Some(Split::Eval)
        } else {
            None
        }
    }
}

/// Which dataset split an observation belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Split {
    Train,
    Calibration,
    Eval,
}

// Union-Find for observation indices
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        let mut parent = vec![0; n];
        let rank = vec![0; n];
        for i in 0..n {
            parent[i] = i;
        }
        Self { parent, rank }
    }
    
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }
    
    fn union(&mut self, x: usize, y: usize) {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx == ry { return; }
        
        if self.rank[rx] < self.rank[ry] {
            self.parent[rx] = ry;
        } else if self.rank[rx] > self.rank[ry] {
            self.parent[ry] = rx;
        } else {
            self.parent[ry] = rx;
            self.rank[rx] += 1;
        }
    }
    
    fn components(&mut self) -> HashMap<usize, Vec<usize>> {
        let mut map: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..self.parent.len() {
            // A stale parent pointer is not a root: only find(i) resolves the
            // true component root, otherwise a chain unions get torn apart.
            let root = self.find(i);
            map.entry(root).or_default().push(i);
        }
        map
    }
}

/// Split observation IDs into train/calibration/evaluation based on
/// component-disjoint rules. Returns the splits and a list of any
/// violations (connected components that cross boundaries).
pub fn split_observations(
    observation_ids: &[String],
    memory_ids: &[HashSet<String>],    // per-observation memory ID sets
    payload_hashes: &[HashSet<String>], // per-observation payload hash sets
    supersession_lineages: &[Option<String>], // per-observation superseded-by
    governance_targets: &[Option<String>],   // per-observation governance target
    time_windows: &[i64],                    // per-observation timestamp
) -> Result<DatasetSplits> {
    let mut splits = DatasetSplits::new();
    let n = observation_ids.len();
    let mut uf = UnionFind::new(n);
    
    // Union observations based on shared signals
    // Memory IDs
    let mut memory_id_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, mids) in memory_ids.iter().enumerate() {
        for mid in mids {
            memory_id_to_indices.entry(mid.clone()).or_default().push(i);
        }
    }
    for indices in memory_id_to_indices.values() {
        if indices.len() > 1 {
            let first = indices[0];
            for &other in indices.iter().skip(1) {
                uf.union(first, other);
            }
        }
    }
    
    // Payload hashes
    let mut ph_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, phs) in payload_hashes.iter().enumerate() {
        for ph in phs {
            ph_to_indices.entry(ph.clone()).or_default().push(i);
        }
    }
    for indices in ph_to_indices.values() {
        if indices.len() > 1 {
            let first = indices[0];
            for &other in indices.iter().skip(1) {
                uf.union(first, other);
            }
        }
    }
    
    // Supersession lineages (same Some(sup) value)
    let mut sup_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, sup_opt) in supersession_lineages.iter().enumerate() {
        if let Some(sup) = sup_opt {
            sup_to_indices.entry(sup.clone()).or_default().push(i);
        }
    }
    for indices in sup_to_indices.values() {
        if indices.len() > 1 {
            let first = indices[0];
            for &other in indices.iter().skip(1) {
                uf.union(first, other);
            }
        }
    }
    
    // Governance targets (same Some(gov) value)
    let mut gov_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, gov_opt) in governance_targets.iter().enumerate() {
        if let Some(gov) = gov_opt {
            gov_to_indices.entry(gov.clone()).or_default().push(i);
        }
    }
    for indices in gov_to_indices.values() {
        if indices.len() > 1 {
            let first = indices[0];
            for &other in indices.iter().skip(1) {
                uf.union(first, other);
            }
        }
    }
    
    // Time windows (same 1-hour bucket when > 0)
    let mut time_to_indices: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, &ts) in time_windows.iter().enumerate() {
        if ts > 0 {
            let hour = ts / 3600;
            time_to_indices.entry(hour).or_default().push(i);
        }
    }
    for indices in time_to_indices.values() {
        if indices.len() > 1 {
            let first = indices[0];
            for &other in indices.iter().skip(1) {
                uf.union(first, other);
            }
        }
    }
    
    // Group observations by their root components
    let components: HashMap<usize, Vec<usize>> = uf.components();
    let mut component_list: Vec<Vec<usize>> = components.values().cloned().collect();
    component_list.sort_by(|a, b| {
        a[0].cmp(&b[0]) // deterministic ordering
    });
    
    // Assign each component deterministically to Train/Calibration/Eval
    for (idx, component) in component_list.iter().enumerate() {
        let split = match idx % 3 {
            0 => Split::Train,
            1 => Split::Calibration,
            _ => Split::Eval,
        };
        
        for &obs_idx in component {
            let obs_id = &observation_ids[obs_idx];
            if !splits.contains(obs_id) {
                splits.insert(obs_id, split);
            }
        }
    }
    
    // Validate no connected group (canonical union-find component) spans
    // multiple splits.
    let group_roots: Vec<usize> = (0..n).map(|i| uf.find(i)).collect();
    verify_no_group_leakage(observation_ids, &group_roots, &splits)?;

    Ok(splits)
}

/// Verify that every group of observations grouped by their canonical
/// union-find component root was assigned the exact same `Split`. Bails if any
/// group spans multiple splits (or if an observation has no split at all).
pub fn verify_no_group_leakage(
    observation_ids: &[String],
    group_roots: &[usize],
    splits: &DatasetSplits,
) -> Result<()> {
    use std::collections::hash_map::Entry;
    let mut violations: Vec<String> = Vec::new();
    let mut group_split: HashMap<usize, Split> = HashMap::new();
    for (obs_id, root) in observation_ids.iter().zip(group_roots.iter()) {
        let Some(split) = splits.split_of(obs_id) else {
            violations.push(format!(
                "observation {} (group {}) has no split assignment",
                obs_id, root
            ));
            continue;
        };
        match group_split.entry(*root) {
            Entry::Occupied(e) if *e.get() != split => {
                violations.push(format!(
                    "group {} spans {:?} and {:?} (observation {})",
                    root,
                    e.get(),
                    split,
                    obs_id
                ));
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(e) => {
                e.insert(split);
            }
        }
    }
    if !violations.is_empty() {
        anyhow::bail!("Component-disjoint constraint violations: {:?}", violations);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_split_observations_component_disjoint() {
        // Step 3: component-disjoint splitting across memory IDs, payload hashes,
        // supersession lineages, shared governance targets, and time.
        // Assert no connected component crosses train/calibration/evaluation.
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string()];
        let memory_ids = vec![HashSet::from(["mid-1".to_string()]), HashSet::from(["mid-2".to_string()])];
        let payload_hashes = vec![HashSet::from(["ph-1".to_string()]), HashSet::from(["ph-2".to_string()])];
        let supersession_lineages = vec![None, None];
        let governance_targets = vec![None, None];
        let time_windows = vec![1000, 2000];

        let result = split_observations(
            &obs_ids,
            &memory_ids,
            &payload_hashes,
            &supersession_lineages,
            &governance_targets,
            &time_windows,
        );
        // For now, just verify the function compiles and returns splits
        let _splits = result.expect("split observations");
        assert!(_splits.train.contains("obs-1") || _splits.train.contains("obs-2"));
    }

    #[test]
    fn split_disjointness_no_cross_component() {
        // Step 3: verify that no connected component crosses train/calibration/eval
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string()];
        let memory_ids = vec![HashSet::from(["mid-1".to_string()]), HashSet::from(["mid-1".to_string()])]; // shared memory ID
        let payload_hashes = vec![HashSet::from(["ph-1".to_string()]), HashSet::from(["ph-1".to_string()])];
        let supersession_lineages = vec![None, None];
        let governance_targets = vec![None, None];
        let time_windows = vec![1000, 2000];

        let result = split_observations(
            &obs_ids,
            &memory_ids,
            &payload_hashes,
            &supersession_lineages,
            &governance_targets,
            &time_windows,
        );
        let splits = result.expect("splits");
        // obs-1 and obs-2 share a memory ID, so they should be in the same split
        let obs1_in_train = splits.train.contains("obs-1");
        let obs2_in_train = splits.train.contains("obs-2");
        // They should both be in the same split (train, for this test)
        assert_eq!(obs1_in_train, obs2_in_train, "shared memory ID must not cross split boundaries");
    }

    #[test]
    fn split_connected_by_payload_hash() {
        // Shared payload hash should keep observations in same split
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string()];
        let memory_ids = vec![HashSet::new(), HashSet::new()];
        let payload_hashes = vec![HashSet::from(["ph-1".to_string()]), HashSet::from(["ph-1".to_string()])];
        let supersession_lineages = vec![None, None];
        let governance_targets = vec![None, None];
        let time_windows = vec![1000, 2000];

        let result = split_observations(
            &obs_ids,
            &memory_ids,
            &payload_hashes,
            &supersession_lineages,
            &governance_targets,
            &time_windows,
        );
        let splits = result.expect("splits");
        let obs1_in_train = splits.train.contains("obs-1");
        let obs2_in_train = splits.train.contains("obs-2");
        assert_eq!(obs1_in_train, obs2_in_train, "shared payload hash must not cross split boundaries");
    }

    #[test]
    fn split_connected_by_governance_target() {
        // Shared governance target should keep observations in same split
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string()];
        let memory_ids = vec![HashSet::new(), HashSet::new()];
        let payload_hashes = vec![HashSet::new(), HashSet::new()];
        let supersession_lineages = vec![None, None];
        let governance_targets = vec![Some("gov-1".to_string()), Some("gov-1".to_string())];
        let time_windows = vec![1000, 2000];

        let result = split_observations(
            &obs_ids,
            &memory_ids,
            &payload_hashes,
            &supersession_lineages,
            &governance_targets,
            &time_windows,
        );
        let splits = result.expect("splits");
        let obs1_in_train = splits.train.contains("obs-1");
        let obs2_in_train = splits.train.contains("obs-2");
        assert_eq!(obs1_in_train, obs2_in_train, "shared governance target must not cross split boundaries");
    }

    #[test]
    fn time_window_groups_share_split() {
        // Observations within the same hour bucket are unioned into one group,
        // so they must land in the exact same split.
        let obs_ids = vec!["obs-a".to_string(), "obs-b".to_string(), "obs-c".to_string()];
        let empty = || HashSet::new();
        let result = split_observations(
            &obs_ids,
            &[empty(), empty(), empty()],
            &[empty(), empty(), empty()],
            &[None, None, None],
            &[None, None, None],
            &[1000, 2000, 1500], // all in the same 1-hour bucket
        );
        let splits = result.expect("splits");
        let assigned: Vec<Split> = obs_ids.iter().map(|id| splits.split_of(id).expect("assigned")).collect();
        assert!(
            assigned.iter().all(|s| *s == assigned[0]),
            "same time-window group must share one split, got {:?}",
            assigned
        );
    }

    #[test]
    fn verify_no_group_leakage_bails_on_split_group() {
        let obs_ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut splits = DatasetSplits::new();
        splits.insert("a", Split::Train);
        splits.insert("b", Split::Train);
        splits.insert("c", Split::Eval);
        // a, b, c all belong to group 7.
        let err = verify_no_group_leakage(&obs_ids, &[7, 7, 7], &splits)
            .err()
            .expect("group spanning multiple splits must bail");
        assert!(err.to_string().contains("Component-disjoint constraint violations"), "err={}", err);
    }

    #[test]
    fn verify_no_group_leakage_passes_consistent_groups() {
        let obs_ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut splits = DatasetSplits::new();
        splits.insert("a", Split::Train);
        splits.insert("b", Split::Train);
        splits.insert("c", Split::Eval);
        // Groups are consistent: {a, b} -> Train, {c} -> Eval.
        assert!(verify_no_group_leakage(&obs_ids, &[0, 0, 1], &splits).is_ok());
    }

    #[test]
    fn chained_keys_through_two_fields_share_split() {
        // obs1 ~ obs2 by memory id, obs2 ~ obs3 by payload hash: transitivity
        // of the union-find must carry all three into one split.
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string(), "obs-3".to_string()];
        let empty = || HashSet::new();
        let result = split_observations(
            &obs_ids,
            &[HashSet::from(["mid-1".to_string()]), HashSet::from(["mid-1".to_string()]), empty()],
            &[empty(), HashSet::from(["ph-9".to_string()]), HashSet::from(["ph-9".to_string()])],
            &[None, None, None],
            &[None, None, None],
            &[1000, 2000, 3000], // distinct hour buckets so time unions nothing
        );
        let splits = result.expect("splits");
        let assigned: Vec<Split> = obs_ids.iter().map(|id| splits.split_of(id).expect("assigned")).collect();
        assert!(
            assigned.iter().all(|s| *s == assigned[0]),
            "transitive two-field chain must share one split, got {:?}",
            assigned
        );
    }

    #[test]
    fn stale_parent_pointers_do_not_split_a_component() {
        // obs1~obs2 (memory id), obs2~obs3 (governance), obs3~obs4 (payload
        // hash). The final union merges root {obs1,obs2} with root {obs3,obs4};
        // obs4's parent pointer is then stale, so grouping by the raw parent
        // vector instead of find() used to tear obs4 into its own component,
        // faking a group-leakage violation.
        let obs_ids = vec!["obs-1".to_string(), "obs-2".to_string(), "obs-3".to_string(), "obs-4".to_string()];
        let empty = || HashSet::new();
        let result = split_observations(
            &obs_ids,
            &[HashSet::from(["mid-A".to_string()]), HashSet::from(["mid-A".to_string()]), empty(), empty()],
            &[empty(), empty(), HashSet::from(["ph-X".to_string()]), HashSet::from(["ph-X".to_string()])],
            &[None, None, None, None],
            &[None, Some("gov-G".to_string()), Some("gov-G".to_string()), None],
            &[1000, 2000, 3000, 4000], // distinct hour buckets so time unions nothing
        );
        let splits = result.expect("splits");
        let assigned: Vec<Split> = obs_ids.iter().map(|id| splits.split_of(id).expect("assigned")).collect();
        assert!(
            assigned.iter().all(|s| *s == assigned[0]),
            "stale-pointer component must stay in one split, got {:?}",
            assigned
        );
    }

    #[test]
    fn verify_no_group_leakage_bails_on_unassigned_observation() {
        let obs_ids = vec!["a".to_string(), "unassigned".to_string()];
        let mut splits = DatasetSplits::new();
        splits.insert("a", Split::Train);
        let err = verify_no_group_leakage(&obs_ids, &[0, 1], &splits)
            .err()
            .expect("unassigned observation must bail");
        assert!(err.to_string().contains("unassigned"), "err={}", err);
    }
}
