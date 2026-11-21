//! Upgraded search filters and compact renderer helpers.
//!
//! Ranking here works on raw L2 distances (lower = better). Shape scoring
//! SUBTRACTS a fixed gain from that distance, so a boosted fact simply lands
//! closer to its query: `score = (distance - decision_shape_boost(...)).max(0.0)`.

use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub memory_types: Vec<String>,
    pub fact_shape: Option<String>, // decision/topology/attempt/any
    pub path_prefix: Option<String>,
    pub path_prefix_forms: Vec<String>,
    pub exclude_stale: bool,
    pub include_code_ast: bool,
}

impl SearchFilters {
    /// Test-only constructor (production filters carry explicit intent).
    #[cfg(test)]
    pub fn default_filters() -> Self {
        Self {
            include_code_ast: false,
            ..Default::default()
        }
    }
}

/// Whether a memory type is included by default. `code_ast` excluded unless requested.
pub fn type_allowed(memory_type: &str, filters: &SearchFilters) -> bool {
    if !filters.memory_types.is_empty() && !filters.memory_types.iter().any(|t| t == memory_type) {
        return false;
    }
    if memory_type == "code_ast" && !filters.include_code_ast {
        return false;
    }
    true
}

/// Whether a fact shape matches the requested shape.
pub fn fact_shape_matches(metadata: &Value, shape: &str) -> bool {
    match shape {
        "decision" => metadata
            .get("decision")
            .map(|v| !v.is_null())
            .unwrap_or(false),
        "topology" => metadata
            .get("topology")
            .map(|v| !v.is_null())
            .unwrap_or(false),
        "attempt" => metadata
            .get("attempt")
            .map(|v| !v.is_null())
            .unwrap_or(false),
        "any" | "" => true,
        _ => false,
    }
}

/// The distance subtraction a `decision`-shaped fact earns: it is taken off
/// the raw L2 distance (lower distance = better rank), so a decision-shaped
/// fact surfaces above an equivalently-similar unshaped fact.
pub const DECISION_SHAPE_GAIN: f32 = 0.03;

/// Distance-reducing boost for decision-shaped facts (spec: decision shape
/// gain). The caller subtracts the returned value from the raw L2 distance
/// and clamps at zero; lower distance = better rank.
pub fn decision_shape_boost(metadata: &Value) -> f32 {
    if fact_shape_matches(metadata, "decision") {
        DECISION_SHAPE_GAIN
    } else {
        0.0
    }
}

/// Path prefix filter on location.
pub fn path_prefix_matches(location: &str, prefix: Option<&str>) -> bool {
    match prefix {
        None => true,
        Some(p) if p.trim().is_empty() => true,
        Some(p) => single_prefix_matches(location, p),
    }
}

/// Canonicalize a value for boundary-aware prefix comparison: host-separator
/// folding, duplicate-separator collapsing, `./` stripping, and edge `/` trim.
pub fn normalize_prefix_comparison_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        let mapped = if ch == '\\' { '/' } else { ch };
        if mapped == '/' && out.ends_with('/') {
            continue;
        }
        out.push(mapped);
    }
    while let Some(rest) = out.strip_prefix("./") {
        out = rest.to_string();
    }
    let trimmed = out.trim_matches('/');
    if trimmed == "." {
        return String::new();
    }
    let mut normalized = trimmed.to_string();
    if normalized.len() >= 2
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
    {
        normalized.as_mut_str()[..1].make_ascii_uppercase();
    }
    normalized
}

fn is_absolute_any_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with('\\')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\'))
}

/// Match one prefix against a location using normalized, separator- and
/// segment-boundary-aware comparison. Relative prefix forms match only relative
/// locations; absolute prefix forms match only absolute locations.
pub fn single_prefix_matches(location: &str, prefix: &str) -> bool {
    if prefix.trim().is_empty() {
        return true;
    }
    if location.trim().is_empty() {
        return false;
    }
    if is_absolute_any_path(location) != is_absolute_any_path(prefix) {
        return false;
    }
    let loc = normalize_prefix_comparison_value(location);
    if loc.is_empty() {
        return false;
    }
    let pref = normalize_prefix_comparison_value(prefix);
    if pref.is_empty() {
        return loc != ".." && !loc.starts_with("../");
    }
    loc == pref
        || loc
            .strip_prefix(&pref)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A location matches when any resolved spelling of the requested prefix matches.
pub fn path_prefix_forms_match(location: &str, forms: &[String]) -> bool {
    if forms.is_empty() {
        return true;
    }
    forms
        .iter()
        .any(|form| path_prefix_matches(location, Some(form.as_str())))
}

/// Match a row against resolved prefix spellings across its location and the
/// path-shaped metadata fields used by structural and governance rows.
pub fn row_matches_prefix_forms(location: &str, metadata: &Value, forms: &[String]) -> bool {
    if forms.is_empty() {
        return true;
    }
    if !location.trim().is_empty() && path_prefix_forms_match(location, forms) {
        return true;
    }
    for key in ["relative_path", "path"] {
        if let Some(p) = metadata.get(key).and_then(|v| v.as_str()) {
            if path_prefix_forms_match(p, forms) {
                return true;
            }
        }
    }
    if let Some(locs) = metadata.get("locations").and_then(|v| v.as_array()) {
        for item in locs {
            if let Some(p) = item.get("path").and_then(|v| v.as_str()) {
                if path_prefix_forms_match(p, forms) {
                    return true;
                }
            }
        }
    }
    if let Some(gov) = metadata.get("governs") {
        match gov {
            Value::String(p) => {
                if path_prefix_forms_match(p, forms) {
                    return true;
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(p) = item.as_str() {
                        if path_prefix_forms_match(p, forms) {
                            return true;
                        }
                    } else if let Some(p) = item.get("path").and_then(|v| v.as_str()) {
                        if path_prefix_forms_match(p, forms) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// exclude_stale: exclude memories whose source currency is changed AND semantic validation != verified.
pub fn is_stale_for_filter(metadata: &Value) -> bool {
    let source = metadata
        .get("source_currency")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let semantic = metadata
        .get("verification")
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    source == "changed" && semantic != "verified"
}

pub fn passes_filters(
    memory_type: &str,
    metadata: &Value,
    location: &str,
    filters: &SearchFilters,
) -> bool {
    if !type_allowed(memory_type, filters) {
        return false;
    }
    if memory_type == "fact" {
        if let Some(shape) = &filters.fact_shape {
            if !shape.is_empty() && shape != "any" {
                if !fact_shape_matches(metadata, shape) {
                    return false;
                }
            }
        }
    }
    if !filters.path_prefix_forms.is_empty() {
        if !row_matches_prefix_forms(location, metadata, &filters.path_prefix_forms) {
            return false;
        }
    } else if let Some(p) = filters
        .path_prefix
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        if !row_matches_prefix_forms(location, metadata, &[p.to_string()]) {
            return false;
        }
    }
    if filters.exclude_stale && is_stale_for_filter(metadata) {
        return false;
    }
    true
}

/// Exact warning emitted when the removed `include_related` alias is used.
/// `"include_references"` is provenance-only; the alias used to be a silent
/// no-op, so the correction has to be loud and byte-stable.
pub const INCLUDE_RELATED_DEPRECATION_WARNING: &str =
    "'include_related' is deprecated; use 'include_references'";

/// Pure resolution of the `search_memory` argument contract.
///
/// Kept independent of the store and embedder so the alias/default rules can
/// be pinned by a focused test. `graph_expanded_ids` is supplied by the caller:
/// the IDs come from hybrid graph expansion, never from ranking changes here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchMemoryContract {
    pub mode: String,
    pub include_references: bool,
    pub warnings: Vec<String>,
    pub graph_expanded_ids: Vec<String>,
}

impl SearchMemoryContract {
    pub fn from_arguments(arguments: &Value, graph_expanded_ids: Vec<String>) -> Self {
        let mode = arguments
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("hybrid")
            .to_string();
        let include_related = arguments.get("include_related").and_then(|v| v.as_bool());
        let include_references = arguments
            .get("include_references")
            .and_then(|v| v.as_bool());
        let mut warnings = Vec::new();
        if arguments.get("include_related").is_some() {
            warnings.push(INCLUDE_RELATED_DEPRECATION_WARNING.to_string());
        }
        // Explicit `include_references` wins when both keys are present;
        // the deprecated alias only fills in an otherwise absent value.
        let include_references = include_references.or(include_related).unwrap_or(false);
        Self {
            mode,
            include_references,
            warnings,
            graph_expanded_ids,
        }
    }
}

/// Append the additive `search_contract` line to any report, including the
/// plain strings returned on hard-error paths (missing params, bad mode, init
/// or embed failure, search failure). Error paths pass no graph-expanded ids,
/// but the rule set is identical to the success path: `hybrid` default,
/// `include_related` deprecation warning, and an always-present id array.
/// Kept crate-free so `upgraded_search_test` can pin the error rendering.
pub fn append_search_contract_line(
    mut report: String,
    arguments: &Value,
    graph_expanded_ids: Vec<String>,
) -> String {
    let contract = SearchMemoryContract::from_arguments(arguments, graph_expanded_ids);
    report.push_str(&format!(
        "\n\nsearch_contract: {}",
        serde_json::to_string(&contract).unwrap_or_default()
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_filters() {
        let f = SearchFilters::default_filters();
        assert!(f.memory_types.is_empty());
        assert!(f.fact_shape.is_none());
        assert!(f.path_prefix.is_none());
        assert!(!f.exclude_stale);
        assert!(!f.include_code_ast);
    }

    #[test]
    fn type_allowed_default() {
        let f = SearchFilters::default_filters();
        // code_ast excluded by default
        assert!(!type_allowed("code_ast", &f));
        assert!(type_allowed("rule", &f));
        // memory_types filter
        let mut f2 = SearchFilters::default();
        f2.memory_types = vec!["fact".to_string()];
        f2.include_code_ast = true;
        assert!(type_allowed("fact", &f2));
        assert!(!type_allowed("rule", &f2));
    }

    #[test]
    fn decision_shape_boost_variants() {
        let m = json!({"decision": "ship podman only"});
        assert_eq!(decision_shape_boost(&m), DECISION_SHAPE_GAIN);
        assert_eq!(decision_shape_boost(&json!({"topology": {"a": 1}})), 0.0);
        assert_eq!(decision_shape_boost(&json!({})), 0.0);
        assert_eq!(decision_shape_boost(&json!(null)), 0.0);
    }

    /// Scores are retrieval DISTANCES (lower = better). The decision-shape
    /// gain must reduce the distance of a decision-shaped fact relative to an
    /// identical unshaped fact; adding it would push decisions DOWN the
    /// ascending ranking (Slice AD-5).
    #[test]
    fn decision_shape_gain_lowers_distance() {
        let shaped = json!({"decision": "ship podman only"});
        let unshaped = json!({});
        let d: f32 = 0.42;
        let shaped_score = (d - decision_shape_boost(&shaped)).max(0.0);
        let unshaped_score = (d - decision_shape_boost(&unshaped)).max(0.0);
        assert!(
            shaped_score < unshaped_score,
            "decision-shaped fact must score a lower distance than an identical unshaped fact (got {shaped_score} vs {unshaped_score})"
        );
        assert_eq!(unshaped_score, d);
        assert_eq!(shaped_score, d - DECISION_SHAPE_GAIN);
        // Near-zero distances clamp at 0.0 without ever going negative or
        // flipping the ordering against a fully-boosted distance.
        let near_zero_shaped = (0.01_f32 - decision_shape_boost(&shaped)).max(0.0);
        assert_eq!(near_zero_shaped, 0.0);
        assert!(near_zero_shaped <= (0.01_f32 - decision_shape_boost(&unshaped)).max(0.0));
    }

    #[test]
    fn fact_shape_matches_variants() {
        let m = json!({"decision": "x"});
        assert!(fact_shape_matches(&m, "decision"));
        assert!(!fact_shape_matches(&m, "topology"));
        let m2 = json!({"topology": {"a":1}});
        assert!(fact_shape_matches(&m2, "topology"));
        let m3 = json!({"attempt": {"outcome":"failed"}});
        assert!(fact_shape_matches(&m3, "attempt"));
        assert!(fact_shape_matches(&json!({}), "any"));
        assert!(fact_shape_matches(&json!({}), ""));
        assert!(!fact_shape_matches(&json!({}), "bogus"));
    }

    #[test]
    fn path_prefix_matches_variants() {
        assert!(path_prefix_matches("src/a.rs", None));
        assert!(path_prefix_matches("src/a.rs", Some("")));
        assert!(path_prefix_matches("src/a.rs", Some("src/")));
        assert!(!path_prefix_matches("src/a.rs", Some("lib/")));
    }

    #[test]
    fn root_prefix_forms_match_in_root_relative_and_absolute_locations_only() {
        let forms = vec![".".to_string(), "/repo".to_string()];
        assert!(path_prefix_forms_match("src/lib.rs", &forms));
        assert!(path_prefix_forms_match("Cargo.toml", &forms));
        assert!(path_prefix_forms_match("./src/lib.rs", &forms));
        assert!(path_prefix_forms_match("/repo/src/lib.rs", &forms));
        assert!(!path_prefix_forms_match("/repo-other/src/lib.rs", &forms));
        assert!(!path_prefix_forms_match("/other/src/lib.rs", &forms));
        assert!(!path_prefix_forms_match("../other/src/lib.rs", &forms));
        assert!(!path_prefix_forms_match("", &forms));
    }

    #[test]
    fn is_stale_for_filter_variants() {
        let stale = json!({"source_currency":"changed","verification":{"status":"unverified"}});
        assert!(is_stale_for_filter(&stale));
        let verified = json!({"source_currency":"changed","verification":{"status":"verified"}});
        assert!(!is_stale_for_filter(&verified));
        let fresh = json!({"source_currency":"current"});
        assert!(!is_stale_for_filter(&fresh));
    }

    #[test]
    fn passes_filters_combines() {
        let mut f = SearchFilters::default();
        f.memory_types = vec!["fact".to_string()];
        f.path_prefix = Some("src/".to_string());
        f.exclude_stale = true;
        f.include_code_ast = true;
        let meta = json!({"source_currency":"current"});
        assert!(passes_filters("fact", &meta, "src/x.rs", &f));
        // wrong type
        assert!(!passes_filters("rule", &meta, "src/x.rs", &f));
        // wrong path
        assert!(!passes_filters("fact", &meta, "lib/x.rs", &f));
        // stale excluded
        let stale_meta =
            json!({"source_currency":"changed","verification":{"status":"unverified"}});
        assert!(!passes_filters("fact", &stale_meta, "src/x.rs", &f));
        // fact_shape filter
        let mut f2 = f.clone();
        f2.fact_shape = Some("decision".to_string());
        let decision_meta = json!({"decision":"x","source_currency":"current"});
        assert!(passes_filters("fact", &decision_meta, "src/x.rs", &f2));
        let no_decision = json!({"source_currency":"current"});
        assert!(!passes_filters("fact", &no_decision, "src/x.rs", &f2));
        // code_ast type_allowed respects include_code_ast
        let mut f3 = SearchFilters::default();
        f3.include_code_ast = false;
        assert!(!passes_filters("code_ast", &json!({}), "src/x.rs", &f3));
    }
}
