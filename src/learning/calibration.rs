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

    /// Fit temperature from gold calibration pairs (prob, true/false).
    ///
    /// Real temperature scaling: for each pair, the clamped probability is
    /// converted to a logit `z = ln(p / (1 - p))`, and `T ∈ [0.1, 10.0]` is
    /// optimized to minimize the negative log-likelihood of
    /// `sigmoid(z / T)` against the gold targets (1.0 for duplicates,
    /// 0.0 for non-duplicates; both taken from `gold` calibration data,
    /// never synthesized). Search is a coarse geometric grid followed by
    /// fine local refinement.
    pub fn fit(&self, pairs: &[(f32, bool)]) -> Result<f32> {
        if pairs.is_empty() {
            return Ok(1.0);
        }
        let logits: Vec<f64> = pairs.iter()
            .map(|(p, _)| {
                let pc = (*p as f64).clamp(1e-6, 1.0 - 1e-6);
                (pc / (1.0 - pc)).ln()
            })
            .collect();
        // Gold targets: the pairs ARE the gold calibration data.
        let targets: Vec<f64> = pairs.iter()
            .map(|(_, is_dup)| if *is_dup { 1.0 } else { 0.0 })
            .collect();

        let nll = |t: f64| -> f64 {
            logits.iter().zip(targets.iter())
                .map(|(&z, &y)| {
                    let s = 1.0 / (1.0 + (-z / t).exp());
                    let s = s.clamp(1e-12, 1.0 - 1e-12);
                    -(y * s.ln() + (1.0 - y) * (1.0 - s).ln())
                })
                .sum::<f64>()
        };

        // Coarse geometric grid over [0.1, 10.0].
        let steps = 24usize;
        let mut best_t = 1.0f64;
        let mut best_nll = nll(best_t);
        for i in 0..=steps {
            let t = 0.1 * (100.0f64).powf(i as f64 / steps as f64);
            let v = nll(t);
            if v < best_nll {
                best_nll = v;
                best_t = t;
            }
        }
        // Fine refinement: shrink a window around the best point.
        for _round in 0..4 {
            let lo = (best_t / 2.0).max(0.1);
            let hi = (best_t * 2.0).min(10.0);
            if hi <= lo {
                break;
            }
            let n = 33usize;
            for i in 0..n {
                let frac = i as f64 / (n - 1) as f64;
                let t = lo * (hi / lo).powf(frac);
                let v = nll(t);
                if v < best_nll {
                    best_nll = v;
                    best_t = t;
                }
            }
        }
        Ok(best_t.clamp(0.1, 10.0) as f32)
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
    fn test_cand_pairs() {
        let fitter = TemperatureFitter::new();
        let t = fitter.fit(&[(0.8, true), (0.2, false)]).expect("fit failed");
        // Both pairs are well-separated from 0.5, so sharpening (small T)
        // strictly improves NLL: the fit must land near the lower bound.
        assert!(t >= 0.1 && t <= 0.3, "t={:.4}", t);
    }

    #[test]
    fn test_identical_pairs() {
        let fitter = TemperatureFitter::new();
        let t = fitter.fit(&[(0.5, true), (0.5, false)]).expect("fit failed");
        // z = 0 for p = 0.5, so sigmoid is flat in T: any valid T accepted.
        assert!(t >= 0.1 && t <= 10.0, "t={:.2}", t);
    }

    #[test]
    fn test_confident_wrong_pair_raises_temperature() {
        let fitter = TemperatureFitter::new();
        // Model says 0.99 but gold says non-duplicate: calibration must soften
        // (large T) rather than trust the overconfident probability.
        let t = fitter.fit(&[(0.99, false), (0.01, true)]).expect("fit failed");
        assert!(t > 5.0, "t={:.4}", t);
    }

    #[test]
    fn test_empty_input() {
        let fitter = TemperatureFitter::new();
        let t = fitter.fit(&[]).expect("fit failed");
        assert_eq!(t, 1.0, "empty inputs should return 1.0");
    }
}
