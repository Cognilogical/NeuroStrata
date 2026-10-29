//! Differential privacy helpers (in-house).
pub fn laplace_noise(value: f64, epsilon: f64, seed: u64) -> f64 {
    // Deterministic pseudo-noise (not cryptographically secure).
    let mut x = seed as f64;
    let noise = ((x * 9301.0 + 49297.0) % 233280.0) / 233280.0 - 0.5;
    value + noise / epsilon.max(1e-9)
}
pub fn clip(value: f64, min: f64, max: f64) -> f64 {
    value.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clip_bounds() { assert_eq!(clip(5.0,0.0,1.0),1.0); }
    #[test]
    fn noise_finite() { assert!(laplace_noise(1.0,1.0,42).is_finite()); }
}
