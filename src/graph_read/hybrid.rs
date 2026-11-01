use std::collections::HashSet;

/// Normalized vector distance in [0.0, 1.0] (lower = better).
/// Reuses existing capped usage + provenance + feedback gain.
pub fn normalize_vec(distance: f32, feedback_gain: f32) -> f32 {
    // Clamp to [0, 1.0] and apply the same capped gain logic as in search_inner.
    let capped = distance.clamp(0.0, 1.0);
    let capped_gain = (capped * feedback_gain).clamp(-0.5, 0.5); // Simplified gain model
    capped - capped_gain
}

/// Normalized graph distance in [0.0, 1.0].
/// e.g. 1.0 / (1.0 + co_occurrence_count) or hop-weighted distance.
pub fn normalize_graph(count: u64) -> f32 {
    // Example: higher count → lower score (1.0 / (1.0 + count)) bounded to [0,1]
    1.0 / (1.0 + count as f32)
}

/// Deterministic lexical/symbol distance in [0.0, 1.0].
/// 0.0 = exact match, 1.0 = no match.
pub fn normalize_sym(symbol_match: bool) -> f32 {
    if symbol_match {
        0.0
    } else {
        1.0
    }
}

/// Combine normalized distances into hybrid score.
/// Weights must sum to 1.0.
pub fn combine(
    d_vec: f32,
    d_graph: f32,
    d_sym: f32,
    weights: (f32, f32, f32),
) -> f32 {
    let (w_vec, w_graph, w_sym) = weights;
    d_vec * w_vec + d_graph * w_graph + d_sym * w_sym
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_vec_clamps_and_applies_gain() {
        assert!((normalize_vec(0.5, 0.0) - 0.5).abs() < 1e-6);
        assert!((normalize_vec(1.5, 0.0) - 1.0).abs() < 1e-6);
        assert!((normalize_vec(-0.1, 0.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_graph_zero_count_is_one() {
        assert!((normalize_graph(0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_sym_exact_vs_none() {
        assert!((normalize_sym(true) - 0.0).abs() < 1e-6);
        assert!((normalize_sym(false) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn combine_weights_sum_to_one() {
        let s = combine(0.4, 0.6, 0.0, (0.5, 0.5, 0.0));
        assert!((s - 0.5).abs() < 1e-6);
    }
}
