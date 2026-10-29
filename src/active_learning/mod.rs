//! Active learning helpers (in-house).
pub fn pick_uncertain(scores: &[(String, f64)], n: usize) -> Vec<String> {
    let mut v: Vec<(String, f64)> = scores.to_vec();
    v.sort_by(|a,b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    v.into_iter().take(n).map(|x| x.0).collect()
}
pub fn uncertainty_entropy(p: f64) -> f64 {
    let p = p.clamp(1e-9, 1.0-1e-9);
    -(p*p.log2() + (1.0-p)*(1.0-p).log2())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picks_lowest() {
        let s = vec![("a".into(),0.9),("b".into(),0.1)];
        assert_eq!(pick_uncertain(&s,1), vec!["b".to_string()]);
    }
    #[test]
    fn entropy_max_at_half() {
        assert!(uncertainty_entropy(0.5) > uncertainty_entropy(0.9));
    }
}
