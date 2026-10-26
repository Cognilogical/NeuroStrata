//! Upgraded search filters and compact renderer helpers.

use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub memory_types: Vec<String>,
    pub fact_shape: Option<String>, // decision/topology/attempt/any
    pub path_prefix: Option<String>,
    pub exclude_stale: bool,
    pub include_code_ast: bool,
}

impl SearchFilters {
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
        "decision" => metadata.get("decision").map(|v| !v.is_null()).unwrap_or(false),
        "topology" => metadata.get("topology").map(|v| !v.is_null()).unwrap_or(false),
        "attempt" => metadata.get("attempt").map(|v| !v.is_null()).unwrap_or(false),
        "any" | "" => true,
        _ => false,
    }
}

/// Path prefix filter on location.
pub fn path_prefix_matches(location: &str, prefix: Option<&str>) -> bool {
    match prefix {
        None => true,
        Some(p) if p.is_empty() => true,
        Some(p) => location.starts_with(p),
    }
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

pub fn passes_filters(memory_type: &str, metadata: &Value, location: &str, filters: &SearchFilters) -> bool {
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
    if !path_prefix_matches(location, filters.path_prefix.as_deref()) {
        return false;
    }
    if filters.exclude_stale && is_stale_for_filter(metadata) {
        return false;
    }
    true
}
