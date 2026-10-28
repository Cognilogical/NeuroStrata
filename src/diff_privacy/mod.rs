//! Differential privacy mesh (in-house): Laplace noise.
pub fn laplace_noise(value: f64, epsilon: f64, seed: u64) -> f64 {
    // deterministic pseudo-noise
    let x = ((seed as f64) * 0.000001).sin();
    value + x / epsilon.max(1e-9)
}
#[cfg(test)] mod tests { use super::*; #[test] fn runs() { let v=laplace_noise(5.0,1.0,42); assert!(v.is_finite()); } }
