//! Active learning uncertainty (in-house): pick lowest-confidence items.
pub fn pick_uncertain(scores: &[(String, f64)], n: usize) -> Vec<String> {
    let mut v: Vec<(String, f64)> = scores.to_vec();
    v.sort_by(|a,b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    v.into_iter().take(n).map(|x| x.0).collect()
}
#[cfg(test)] mod tests { use super::*; #[test] fn picks_lowest() { let s=vec![("a".into(),0.9),("b".into(),0.1)]; assert_eq!(pick_uncertain(&s,1),vec!["b"]); } }
