//! Knowledge-graph embeddings (in-house approximation): TransE scoring.
pub fn transe_score(head: &[f32], relation: &[f32], tail: &[f32]) -> f32 {
    // score = -||h + r - t|| (higher is better). Return negative L2 norm.
    let mut sum = 0.0f32;
    let n = head.len().min(relation.len()).min(tail.len());
    for i in 0..n {
        let v = head[i] + relation[i] - tail[i];
        sum += v * v;
    }
    -sum.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn perfect_triple_scores_zero() {
        let h = vec![1.0, 2.0];
        let r = vec![0.5, -0.5];
        let t = vec![1.5, 1.5];
        let s = transe_score(&h, &r, &t);
        assert!(s > -1e-5);
    }
}
