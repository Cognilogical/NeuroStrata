//! Closure budget and anomaly detection (in-house).
pub fn zscore_anomalies(values: &[f64], threshold: f64) -> Vec<usize> {
    if values.is_empty() { return vec![]; }
    let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
    let var: f64 = values.iter().map(|v| (v-mean).powi(2)).sum::<f64>() / values.len() as f64;
    let sd = var.sqrt().max(1e-9);
    values.iter().enumerate().filter(|(_,v)| ((*v-mean)/sd).abs() > threshold).map(|(i,_)| i).collect()
}
pub fn within_budget(used: usize, budget: usize) -> bool { used <= budget }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anomalies() {
        let v = vec![1.0,1.0,1.0,10.0];
        assert!(!zscore_anomalies(&v,1.5).is_empty());
    }
    #[test]
    fn budget() { assert!(within_budget(5,10)); assert!(!within_budget(11,10)); }
}
