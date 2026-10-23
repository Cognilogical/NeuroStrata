//! Post-hoc Gold temperature fitting from gold calibration pairs.
use anyhow::Result;
use crate::judgment::CalibrationTier;

/// Post-hoc temperature fitting on gold calibration data.
/// Computes the temperature scaling that maximizes log-probability agreement
/// between candidate and gold soft targets, and writes calibration metadata
/// into the exported bundle.
pub struct TemperatureFitter {
    /// Fit temperature from gold calibration log-probs.
    pub fn fit(&self, gold_log_probs: &[f64], cand_log_probs: &[f64]) -> Result<f64> {
        // Phase-0: no-op; actual optimization deferred to Task 3.
        Ok(1.0)
    }
}

impl TemperatureFitter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TemperatureFitter {
    fn default() -> Self {
        Self::new()
    }
}
