//! Closure budget + anomaly detection (in-house approximation).
pub use crate::rdf::reasoning::QueryBudget;

/// Simple z-score based anomaly detector.
pub fn zscore_anomalies(values: &[f64], threshold: f64) -> Vec<usize> {
    if values.is_empty() { return vec![]; }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let std = var.sqrt();
    if std == 0.0 { return vec![]; }
    values.iter().enumerate().filter(|(_, v)| ((*v - mean) / std).abs() > threshold).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_outlier() {
        let v = vec![1.0, 1.1, 0.9, 100.0];
        let a = zscore_anomalies(&v, 1.5);
        assert!(a.contains(&3));
    }
}
