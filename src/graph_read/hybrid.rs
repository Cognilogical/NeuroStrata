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

/// Standard Reciprocal Rank Fusion smoothing constant (`k = 60.0`).
pub const RRF_K: f32 = 60.0;

/// Multi-retriever candidate input for Reciprocal Rank Fusion (`neurostrata-5xjo`).
#[derive(Clone, Debug)]
pub struct RankedCandidate {
    pub id: String,
    pub vec_rank: Option<usize>,
    pub vec_distance: Option<f32>,
    pub lex_rank: Option<usize>,
    pub lex_score: Option<f32>,
    pub sym_grade: SymbolMatchGrade,
    pub graph_co_occurrence: u64,
    pub parent_best_rank: Option<usize>,
    pub edge_priority_weight: f32,
    pub feedback_gain: f32,
}

/// Canonical edge priority weight for graph-expanded candidates (`neurostrata-5xjo` §4.2).
pub fn canonical_edge_priority_weight(relation: &str) -> f32 {
    match relation.trim().to_ascii_uppercase().as_str() {
        "GOVERNS" => 1.0,
        "SUPERSEDES" => 0.9,
        "EXTRACTED_FROM" => 0.8,
        "RELATES_TO" | "CONTAINS" => 0.6,
        "DEPENDS_ON" => 0.5,
        _ => 0.5,
    }
}

/// Compute normalized Reciprocal Rank Fusion distance `d_hybrid in [0.0, 1.0]`
/// (lower = better, preserving NeuroStrata's distance sort invariant).
pub fn compute_rrf_distance(
    candidate: &RankedCandidate,
    weights: (f32, f32, f32),
    decision_boost: f32,
    hop_decay: f32,
) -> f32 {
    let (w_vec, w_graph, w_sym) = weights;
    let w_sum = w_vec + w_graph + w_sym;
    let (_nw_vec, nw_graph, nw_sym) = if w_sum > 0.0 {
        (w_vec / w_sum, w_graph / w_sum, w_sym / w_sum)
    } else {
        (0.5, 0.3, 0.2)
    };

    let sym_gain_raw = 1.0 - normalize_sym(candidate.sym_grade);
    let sym_weight = nw_sym.clamp(0.15, 0.30);

    let total_gain = if candidate.vec_rank.is_some() || candidate.lex_rank.is_some() {
        let vec_rrf = candidate
            .vec_rank
            .map(|r| (RRF_K + 1.0) / (RRF_K + r.max(1) as f32))
            .unwrap_or(0.0);
        let lex_rrf = candidate
            .lex_rank
            .map(|r| (RRF_K + 1.0) / (RRF_K + r.max(1) as f32))
            .unwrap_or(0.0);
        let best_track = vec_rrf.max(lex_rrf);
        let second_track = vec_rrf.min(lex_rrf);
        let track_gain = 0.45 * best_track + 0.25 * second_track;

        let bm25_bonus = candidate
            .lex_score
            .filter(|s| *s > 0.0)
            .map(|s| 0.06 * (s / (s + 1.0)))
            .unwrap_or(0.0);
        let vec_micro = candidate
            .vec_distance
            .map(|d| 0.04 * (1.0 - normalize_vec(d, 0.0)))
            .unwrap_or(0.0);
        let graph_micro = 0.04 * (1.0 - normalize_graph(candidate.graph_co_occurrence));
        let sym_gain = sym_weight * sym_gain_raw;

        track_gain + bm25_bonus + vec_micro + graph_micro + sym_gain
    } else {
        let p_rank = candidate.parent_best_rank.unwrap_or(10).max(1);
        let parent_rrf = (RRF_K + 1.0) / (RRF_K + p_rank as f32);
        let co_occ_factor = 1.0 + 0.1 * (1.0 + candidate.graph_co_occurrence as f32).ln();
        let edge_w = candidate.edge_priority_weight.clamp(0.1, 1.0);
        let base_exp_weight = nw_graph.clamp(0.25, 0.40);
        let exp_gain =
            (base_exp_weight * hop_decay.clamp(0.1, 1.0) * edge_w * parent_rrf * co_occ_factor)
                .min(0.42);
        let sym_gain = 0.10 * sym_gain_raw;
        exp_gain + sym_gain
    };

    let feedback_adj = 0.05 * candidate.feedback_gain.clamp(-1.0, 1.0);
    (1.0 - total_gain - decision_boost.max(0.0) - feedback_adj).clamp(0.0, 1.0)
}

/// Combine normalized distances into one hybrid score in `[0.0, 1.0]`.
/// Weights that do not sum to ~1.0 (within `1e-4`) are re-normalized by their
/// sum so caller-supplied weights are honoured without drifting off scale.
/// Degenerate weights (`w_sum <= 0.0`) carry no signal of their own, so the
/// vector distance (clamped to range) is passed through as the score rather
/// than collapsing into a false "perfect" `0.0`.
pub fn combine(d_vec: f32, d_graph: f32, d_sym: f32, weights: (f32, f32, f32)) -> f32 {
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
    let query_lower = query.trim().to_lowercase();
    // An empty query matches everything as a prefix, which would grade every
    // candidate as `Prefix`; treat it as "no query" instead.
    if query_lower.is_empty() {
        return SymbolMatchGrade::None;
    }
    for sym in candidate_symbols {
        let sym_lower = sym.trim().to_lowercase();
        if sym_lower == query_lower {
            return SymbolMatchGrade::Exact;
        }
        if sym_lower
            .split(|c: char| matches!(c, ':' | '/' | '\\' | '.'))
            .any(|seg| !seg.is_empty() && seg == query_lower)
        {
            return SymbolMatchGrade::Exact;
        }
    }
    for sym in candidate_symbols {
        let sym_lower = sym.trim().to_lowercase();
        if sym_lower.starts_with(&query_lower) {
            return SymbolMatchGrade::Prefix;
        }
        if sym_lower
            .split(|c: char| matches!(c, ':' | '/' | '\\' | '.'))
            .any(|seg| !seg.is_empty() && seg.starts_with(&query_lower))
        {
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

    #[test]
    fn rrf_dual_track_ranks_strictly_better_than_single_track_at_same_rank() {
        let weights = (0.5, 0.3, 0.2);
        let dual = RankedCandidate {
            id: "dual".to_string(),
            vec_rank: Some(2),
            vec_distance: Some(0.25),
            lex_rank: Some(2),
            lex_score: Some(3.5),
            sym_grade: SymbolMatchGrade::None,
            graph_co_occurrence: 0,
            parent_best_rank: None,
            edge_priority_weight: 0.0,
            feedback_gain: 0.0,
        };
        let vec_only = RankedCandidate {
            id: "vec-only".to_string(),
            vec_rank: Some(2),
            vec_distance: Some(0.25),
            lex_rank: None,
            lex_score: None,
            sym_grade: SymbolMatchGrade::None,
            graph_co_occurrence: 0,
            parent_best_rank: None,
            edge_priority_weight: 0.0,
            feedback_gain: 0.0,
        };
        let lex_only = RankedCandidate {
            id: "lex-only".to_string(),
            vec_rank: None,
            vec_distance: None,
            lex_rank: Some(2),
            lex_score: Some(3.5),
            sym_grade: SymbolMatchGrade::None,
            graph_co_occurrence: 0,
            parent_best_rank: None,
            edge_priority_weight: 0.0,
            feedback_gain: 0.0,
        };

        let d_dual = compute_rrf_distance(&dual, weights, 0.0, 1.0);
        let d_vec = compute_rrf_distance(&vec_only, weights, 0.0, 1.0);
        let d_lex = compute_rrf_distance(&lex_only, weights, 0.0, 1.0);

        assert!(
            d_dual < d_vec,
            "dual-matched RRF distance ({d_dual}) must be strictly lower than vector-only ({d_vec})"
        );
        assert!(
            d_dual < d_lex,
            "dual-matched RRF distance ({d_dual}) must be strictly lower than lexical-only ({d_lex})"
        );
    }

    #[test]
    fn rrf_exact_symbol_and_lexical_match_beats_distant_vector_only() {
        let weights = (0.5, 0.3, 0.2);
        let symbol_hit = RankedCandidate {
            id: "code-ast-sym".to_string(),
            vec_rank: Some(15),
            vec_distance: Some(0.85),
            lex_rank: Some(1),
            lex_score: Some(6.0),
            sym_grade: SymbolMatchGrade::Exact,
            graph_co_occurrence: 0,
            parent_best_rank: None,
            edge_priority_weight: 0.0,
            feedback_gain: 0.0,
        };
        let vec_top = RankedCandidate {
            id: "vec-1".to_string(),
            vec_rank: Some(1),
            vec_distance: Some(0.20),
            lex_rank: None,
            lex_score: None,
            sym_grade: SymbolMatchGrade::None,
            graph_co_occurrence: 0,
            parent_best_rank: None,
            edge_priority_weight: 0.0,
            feedback_gain: 0.0,
        };

        let d_sym = compute_rrf_distance(&symbol_hit, weights, 0.0, 1.0);
        let d_vec = compute_rrf_distance(&vec_top, weights, 0.0, 1.0);
        assert!(
            d_sym < d_vec,
            "exact symbol + lexical hit ({d_sym}) must rank ahead of vector-only hit ({d_vec})"
        );
    }

    #[test]
    fn rrf_expansion_respects_edge_priority_and_parent_rank() {
        let weights = (0.5, 0.3, 0.2);
        let governs_from_rank1 = RankedCandidate {
            id: "exp-governs".to_string(),
            vec_rank: None,
            vec_distance: None,
            lex_rank: None,
            lex_score: None,
            sym_grade: SymbolMatchGrade::None,
            graph_co_occurrence: 2,
            parent_best_rank: Some(1),
            edge_priority_weight: canonical_edge_priority_weight("GOVERNS"),
            feedback_gain: 0.0,
        };
        let depends_from_rank5 = RankedCandidate {
            id: "exp-depends".to_string(),
            vec_rank: None,
            vec_distance: None,
            lex_rank: None,
            lex_score: None,
            sym_grade: SymbolMatchGrade::None,
            graph_co_occurrence: 1,
            parent_best_rank: Some(5),
            edge_priority_weight: canonical_edge_priority_weight("DEPENDS_ON"),
            feedback_gain: 0.0,
        };

        let d_gov = compute_rrf_distance(&governs_from_rank1, weights, 0.0, 0.85);
        let d_dep = compute_rrf_distance(&depends_from_rank5, weights, 0.0, 0.85);
        assert!(
            d_gov < d_dep,
            "GOVERNS expansion from rank-1 parent ({d_gov}) must rank ahead of DEPENDS_ON from rank-5 ({d_dep})"
        );
    }
}
