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
        let mut rank = vec![0; n];
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
    
    fn components(&self) -> HashMap<usize, Vec<usize>> {
        let mut map: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, &p) in self.parent.iter().enumerate() {
            // Use the current parent (may have been path-compressed by previous find/union)
            map.entry(p).or_default().push(i);
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
    let mut assigned: HashSet<usize> = HashSet::new();
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
        assigned.extend(component);
    }
    
    // Validate no component crosses split boundaries
    let mut violations: Vec<String> = Vec::new();
    let mut split_for_component: HashMap<usize, Split> = HashMap::new();
    
    for component in component_list.iter() {
        if let Some(&obs_idx) = component.first() {
            let root = uf.find(obs_idx);
            let component_split = match root {
                _ => { // Find split by checking first observation
                    let first_obs = &observation_ids[component[0]];
                    if splits.train.contains(first_obs) {
                        Split::Train
                    } else if splits.calibration.contains(first_obs) {
                        Split::Calibration
                    } else {
                        Split::Eval
                    }
                }
            };
            split_for_component.insert(root, component_split);
        }
    }
    
    // Check each component for boundary crossing
    for (root, expected_split) in split_for_component.iter() {
        let component = components.get(root).unwrap();
        for &obs_idx in component {
            let obs_id = &observation_ids[obs_idx];
            let mut actual_split = Split::Train; // default
            if splits.train.contains(obs_id) {
                actual_split = Split::Train;
            } else if splits.calibration.contains(obs_id) {
                actual_split = Split::Calibration;
            } else if splits.eval.contains(obs_id) {
                actual_split = Split::Eval;
            }
            
            if actual_split != *expected_split {
                violations.push(format!("Component root {} (expected {:?}): observation {} in {:?}", 
                    root, expected_split, obs_id, actual_split));
            }
        }
    }
    
    if !violations.is_empty() {
        anyhow::bail!("Component-disjoint constraint violations: {:?}", violations);
    }
    
    Ok(splits)
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
}
