//! Knowledge-graph embeddings (in-house): TransE, RotatE, ComplEx, GraphSAGE.
pub fn transe_score(head: &[f32], relation: &[f32], tail: &[f32]) -> f32 {
    let n = head.len().min(relation.len()).min(tail.len());
    let mut sum = 0.0f32;
    for i in 0..n { let v = head[i] + relation[i] - tail[i]; sum += v*v; }
    -sum.sqrt()
}
pub fn rotate_score(head: &[f32], relation: &[f32], tail: &[f32]) -> f32 {
    let n = head.len().min(relation.len()).min(tail.len());
    let mut sum = 0.0f32;
    for i in 0..n { let v = head[i] * relation[i] - tail[i]; sum += v*v; }
    -sum.sqrt()
}
pub fn complex_score(head: &[f32], relation: &[f32], tail: &[f32]) -> f32 {
    let n = head.len().min(relation.len()).min(tail.len());
    let mut sum = 0.0f32;
    for i in 0..n { sum += head[i] * relation[i] * tail[i]; }
    sum
}
/// GraphSAGE-style mean aggregation of neighbor embeddings.
pub fn graphsage_aggregate(neighbors: &[Vec<f32>]) -> Vec<f32> {
    if neighbors.is_empty() { return vec![]; }
    let dim = neighbors[0].len();
    let mut out = vec![0.0f32; dim];
    for n in neighbors {
        for i in 0..dim.min(n.len()) { out[i] += n[i]; }
    }
    let k = neighbors.len() as f32;
    for v in &mut out { *v /= k; }
    out
}
/// Serialize embeddings to JSON string.
pub fn to_json(embeddings: &std::collections::BTreeMap<String, Vec<f32>>) -> String {
    serde_json::to_string(embeddings).unwrap_or_default()
}
/// Deserialize embeddings from JSON string.
pub fn from_json(s: &str) -> std::collections::BTreeMap<String, Vec<f32>> {
    serde_json::from_str(s).unwrap_or_default()
}


/// Decide whether retraining should trigger based on new edge count threshold.
pub fn should_retrain(new_edges: usize, threshold: usize) -> bool {
    new_edges >= threshold
}

/// Offline experiment mode: score a batch of triples and return mean score.
pub fn batch_score(triples: &[(Vec<f32>,Vec<f32>,Vec<f32>)]) -> f32 {
    if triples.is_empty() { return 0.0; }
    let sum: f32 = triples.iter().map(|(h,r,t)| transe_score(h,r,t)).sum();
    sum / triples.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transe() { assert!(transe_score(&[1.0,2.0],&[0.0,0.0],&[1.0,2.0]) > transe_score(&[1.0,2.0],&[0.0,0.0],&[5.0,5.0])); }
    #[test]
    fn graphsage() {
        let n = vec![vec![1.0,2.0], vec![3.0,4.0]];
        let a = graphsage_aggregate(&n);
        assert_eq!(a, vec![2.0,3.0]);
    }
    #[test]
    fn retrain_trigger() { assert!(should_retrain(10,5)); assert!(!should_retrain(2,5)); }
    #[test]
    fn batch() {
        let t = vec![(vec![1.0,2.0],vec![0.0,0.0],vec![1.0,2.0])];
        assert!(batch_score(&t).is_finite());
    }
    #[test]
    fn roundtrip() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("a".into(), vec![1.0,2.0]);
        let j = to_json(&m);
        let m2 = from_json(&j);
        assert_eq!(m, m2);
    }
}
