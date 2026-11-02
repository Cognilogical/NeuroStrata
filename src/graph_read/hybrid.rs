/// Normalize a vector-search distance into `[0.0, 1.0]` (lower = better).
/// The distance is clamped to that range first, then a signed feedback gain
/// weighted by the capped distance — `capped * feedback_gain`, itself clamped
/// to `±0.5` — is subtracted, and the result is re-clamped so the output
/// always stays inside `[0.0, 1.0]`.
pub fn normalize_vec(distance: f32, feedback_gain: f32) -> f32 {
    // Clamp to [0, 1.0] and apply the same capped gain logic as in search_inner.
    let capped = distance.clamp(0.0, 1.0);
    let capped_gain = (capped * feedback_gain).clamp(-0.5, 0.5);
    (capped - capped_gain).clamp(0.0, 1.0)
}

/// Normalize a graph co-occurrence count into `[0.0, 1.0]`: higher counts mean
/// stronger evidence (a smaller distance). `count == 0` scores `1.0` (no
/// signal), and the score decays as `1.0 / (1.0 + count)`.
pub fn normalize_graph(count: u64) -> f32 {
    1.0 / (1.0 + count as f32)
}

/// Deterministic lexical/symbol distance in [0.0, 1.0] (spec §6.2).
/// `0.0` = exact match; `0.25` = prefix match; `0.5` = token/substring match;
/// `1.0` = no match.
pub fn normalize_sym(grade: SymbolMatchGrade) -> f32 {
    match grade {
        SymbolMatchGrade::Exact => 0.0,
        SymbolMatchGrade::Prefix => 0.25,
        SymbolMatchGrade::TokenSubstring => 0.5,
        SymbolMatchGrade::None => 1.0,
    }
}

/// Spec §6.2 symbol match grades (lower = better).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolMatchGrade {
    Exact,
    Prefix,
    TokenSubstring,
    None,
}

/// Combine normalized distances into one hybrid score in `[0.0, 1.0]`.
/// Weights that do not sum to ~1.0 (within `1e-4`) are re-normalized by their
/// sum so caller-supplied weights are honoured without drifting off scale.
/// Degenerate weights (`w_sum <= 0.0`) carry no signal of their own, so the
/// vector distance (clamped to range) is passed through as the score rather
/// than collapsing into a false "perfect" `0.0`.
pub fn combine(
    d_vec: f32,
    d_graph: f32,
    d_sym: f32,
    weights: (f32, f32, f32),
) -> f32 {
    let (w_vec, w_graph, w_sym) = weights;
    let w_sum = w_vec + w_graph + w_sym;
    if w_sum <= 0.0 {
        return d_vec.clamp(0.0, 1.0);
    }
    let raw = if (w_sum - 1.0).abs() > 1e-4 {
        (d_vec * w_vec + d_graph * w_graph + d_sym * w_sym) / w_sum
    } else {
        d_vec * w_vec + d_graph * w_graph + d_sym * w_sym
    };
    raw.clamp(0.0, 1.0)
}

/// Compute symbol match grade between a query text and a list of candidate symbols.
/// Lower grade = better match.
pub fn compute_symbol_match(query: &str, candidate_symbols: &[String]) -> SymbolMatchGrade {
    // Simple implementation: exact match first, prefix next, token substring last.
    let query_lower = query.to_lowercase();
    // An empty query matches everything as a prefix, which would grade every
    // candidate as `Prefix`; treat it as "no query" instead.
    if query_lower.is_empty() {
        return SymbolMatchGrade::None;
    }
    for sym in candidate_symbols {
        let sym_lower = sym.to_lowercase();
        if sym_lower == query_lower {
            return SymbolMatchGrade::Exact;
        }
    }
    for sym in candidate_symbols {
        let sym_lower = sym.to_lowercase();
        if sym_lower.starts_with(&query_lower) {
            return SymbolMatchGrade::Prefix;
        }
    }
    for sym in candidate_symbols {
        let sym_lower = sym.to_lowercase();
        if sym_lower.contains(&query_lower) {
            return SymbolMatchGrade::TokenSubstring;
        }
    }
    SymbolMatchGrade::None
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
        assert!((normalize_sym(SymbolMatchGrade::Exact) - 0.0).abs() < 1e-6);
        assert!((normalize_sym(SymbolMatchGrade::None) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_sym_prefix_and_substring() {
        assert!((normalize_sym(SymbolMatchGrade::Prefix) - 0.25).abs() < 1e-6);
        assert!((normalize_sym(SymbolMatchGrade::TokenSubstring) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn combine_weights_sum_to_one() {
        let s = combine(0.4, 0.6, 0.0, (0.5, 0.5, 0.0));
        assert!((s - 0.5).abs() < 1e-6);
    }

    /// Degenerate weights must not read as a perfect `0.0` distance: the
    /// vector distance passes through (clamped) as the only available score.
    #[test]
    fn combine_all_zero_weights_falls_back_to_vec_distance() {
        assert!((combine(0.3, 0.9, 0.5, (0.0, 0.0, 0.0)) - 0.3).abs() < 1e-6);
        // Out-of-range vector distances are still clamped to [0.0, 1.0].
        assert!(combine(1.5, 0.9, 0.5, (0.0, 0.0, 0.0)) == 1.0);
        assert!(combine(-0.2, 0.9, 0.5, (0.0, 0.0, 0.0)) == 0.0);
    }
}
