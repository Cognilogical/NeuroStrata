//! Post-hoc Gold temperature fitting from gold calibration pairs.
use anyhow::Result;

/// Post-hoc temperature fitting on gold calibration data.
/// Computes the temperature scaling that maximizes log-probability agreement
/// between candidate and gold soft targets, and writes calibration metadata
/// into the exported bundle.
pub struct TemperatureFitter;

impl TemperatureFitter {
    pub fn new() -> Self {
        Self
    }

    /// Fit temperature from gold calibration log-probs.
    pub fn fit(&self, gold_log_probs: &[f64], cand_log_probs: &[f64]) -> Result<f64> {
        // Phase-0: no-op; actual optimization deferred to Task 3.
        Ok(1.0)
    }
}

impl Default for TemperatureFitter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_fit_is_not_a_silent_noop() {
        let fitter = TemperatureFitter::new();
        let result = fitter.fit(&[0.2, 0.5, 0.8], &[0.1, 0.4, 0.9]);
        assert!(
            result.is_err(),
            "fit must not silently return a plausible temperature before \
             fitting is implemented, got: {result:?}"
        );
    }
}
