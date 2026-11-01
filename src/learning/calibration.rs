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
        // Validate input lengths
        if gold_log_probs.len() != cand_log_probs.len() {
            anyhow::bail!("gold_log_probs and cand_log_probs must have the same length");
        }
        
        // Handle empty case
        if gold_log_probs.is_empty() {
            return Ok(1.0);
        }

        // Objective function: MSE between temperature-scaled candidate and gold log-probs
        fn mse(t: f64, gold: &[f64], cand: &[f64]) -> f64 {
            gold.iter().zip(cand.iter()).map(|(g, c)| {
                let scaled = c / t;
                (scaled - g).powi(2)
            }).sum::<f64>() / (gold.len() as f64)
        }

        // Coarse grid search: 0.1 to 10.0 with step 0.05
        let mut best_t = 1.0;
        let mut best_mse = mse(1.0, gold_log_probs, cand_log_probs);
        
        // Ensure float division
        let steps = (9.9 / 0.05 + 1.0) as usize;
        for i in 0..steps {
            let t = 0.1 + i as f64 * 0.05;
            let current_mse = mse(t, gold_log_probs, cand_log_probs);
            if current_mse < best_mse {
                best_mse = current_mse;
                best_t = t;
            }
        }

        // Refine search around best_t with smaller step
        let mut refined_best_t = best_t;
        let mut refined_best_mse = best_mse;
        
        for _j in 0..200 as usize {
            let refined_t_low = (best_t - 0.2).max(0.1);
            let refined_t_high = (best_t + 0.2).min(10.0);
            let refined_range = refined_t_high - refined_t_low;
            
            for i in 0..(200) as usize {
                let t = refined_t_low + i as f64 * refined_range / 200.0;
                let current_mse = mse(t, gold_log_probs, cand_log_probs);
                if current_mse < refined_best_mse {
                    refined_best_mse = current_mse;
                    refined_best_t = t;
                }
            }
        }

        Ok(refined_best_t.clamp(0.1, 10.0))
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
    fn test_cand_double_gold() {
        let fitter = TemperatureFitter::new();
        let gold = vec![-1.0, -2.0, -3.0];
        let cand = gold.iter().map(|&x| x * 2.0).collect::<Vec<_>>();
        let t = fitter.fit(&gold, &cand).expect("fit failed");
        assert!((t - 2.0).abs() < 0.15, "t={:.2} should be close to 2.0", t);
    }

    #[test]
    fn test_identical_inputs() {
        let fitter = TemperatureFitter::new();
        let gold = vec![-1.0, -2.0, -3.0];
        let cand = gold.clone();
        let t = fitter.fit(&gold, &cand).expect("fit failed");
        assert!((t - 1.0).abs() < 0.15, "t={:.2} should be close to 1.0", t);
    }

    #[test]
    fn test_length_mismatch() {
        let fitter = TemperatureFitter::new();
        let gold = vec![-1.0, -2.0];
        let cand = vec![-1.0];
        let result = fitter.fit(&gold, &cand);
        assert!(result.is_err(), "should error on mismatched lengths");
    }

    #[test]
    fn test_empty_input() {
        let fitter = TemperatureFitter::new();
        let gold: Vec<f64> = vec![];
        let cand: Vec<f64> = vec![];
        let t = fitter.fit(&gold, &cand).expect("fit failed");
        assert_eq!(t, 1.0, "empty inputs should return 1.0");
    }
}
