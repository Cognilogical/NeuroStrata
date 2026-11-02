use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub const DIGITAL_TWIN_MEMORY_TYPES: [&str; 2] = ["digital_twin", "digital_twin_schema"];

#[derive(Debug, Clone, PartialEq)]
pub enum TwinError {
    NonFinite { name: String, value: f64 },
    InvalidPayload(String),
    NotFound(String),
    VersionNotFound { twin_id: String, version: u64 },
}

impl fmt::Display for TwinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TwinError::NonFinite { name, value } => {
                write!(f, "NonFinite variable '{}': {}", name, value)
            }
            TwinError::InvalidPayload(s) => write!(f, "InvalidPayload: {}", s),
            TwinError::NotFound(s) => write!(f, "NotFound: {}", s),
            TwinError::VersionNotFound { twin_id, version } => {
                write!(
                    f,
                    "VersionNotFound twin_id='{}' version={}",
                    twin_id, version
                )
            }
        }
    }
}

impl std::error::Error for TwinError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TwinWarning {
    OutOfBounds {
        name: String,
        value: f64,
        min: f64,
        max: f64,
    },
    UnknownVariable { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TwinDelta {
    pub name: String,
    pub old: Option<f64>,
    pub new: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TwinSummary {
    pub id: String,
    pub twin_id: String,
    pub twin_version: u64,
    pub variable_count: usize,
    pub last_updated: i64,
    pub source: Option<String>,
    pub schema_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TwinVarBounds {
    pub min: f64,
    pub max: f64,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TwinSchema {
    pub variables: BTreeMap<String, TwinVarBounds>,
}

impl TwinSchema {
    pub fn from_metadata(metadata: &serde_json::Value) -> Option<Self> {
        let twin_schema = metadata.get("twin_schema")?;
        let variables_obj = twin_schema.get("variables")?;
        let variables = variables_obj
            .as_object()?
            .iter()
            .filter_map(|(k, v)| {
                let bounds_obj = v.as_object()?;
                let min = bounds_obj.get("min")?.as_f64()?;
                let max = bounds_obj.get("max")?.as_f64()?;
                let unit = bounds_obj.get("unit")?.as_str()?.to_string();
                Some((
                    k.clone(),
                    TwinVarBounds { min, max, unit },
                ))
            })
            .collect();
        Some(TwinSchema { variables })
    }
}

pub fn sanitize_var_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

pub fn get_state(payload: &crate::traits::MemoryPayload) -> BTreeMap<String, f64> {
    let mut result = BTreeMap::new();
    if let Some(vars) = payload.metadata.get("twin").and_then(|t| t.get("variables")) {
        if let Some(obj) = vars.as_object() {
            for (k, v) in obj {
                if let Some(val) = v.as_f64() {
                    if val.is_finite() {
                        result.insert(k.clone(), val);
                    }
                }
            }
        }
    }
    result
}

pub fn set_state(
    payload: &mut crate::traits::MemoryPayload,
    vars: &BTreeMap<String, f64>,
    merge: bool,
) -> Result<(), TwinError> {
    for (name, value) in vars {
        if sanitize_var_name(name).is_empty() {
            return Err(TwinError::InvalidPayload(format!(
                "invalid variable name: {}",
                name
            )));
        }
        if value.is_nan() || value.is_infinite() {
            return Err(TwinError::NonFinite {
                name: name.clone(),
                value: *value,
            });
        }
    }

    payload.memory_type = "digital_twin".to_string();

    if payload.metadata.is_null() || !payload.metadata.is_object() {
        payload.metadata = serde_json::json!({});
    }
    let meta = payload
        .metadata
        .as_object_mut()
        .ok_or_else(|| TwinError::InvalidPayload("metadata is not an object".to_string()))?;
    if !meta.contains_key("twin") || meta.get("twin").map_or(true, |v| !v.is_object()) {
        meta.insert("twin".to_string(), serde_json::json!({}));
    }
    let twin = meta
        .get_mut("twin")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| TwinError::InvalidPayload("twin is not an object".to_string()))?;

    if merge {
        if !twin.contains_key("variables") || twin.get("variables").map_or(true, |v| !v.is_object()) {
            twin.insert("variables".to_string(), serde_json::json!({}));
        }
        let vars_obj = twin
            .get_mut("variables")
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| TwinError::InvalidPayload("variables is not an object".to_string()))?;
        for (k, v) in vars {
            let clean_k = sanitize_var_name(k);
            vars_obj.insert(clean_k, serde_json::json!(*v));
        }
    } else {
        let new_vars: serde_json::Map<String, serde_json::Value> = vars
            .iter()
            .map(|(k, v)| (sanitize_var_name(k), serde_json::json!(*v)))
            .collect();
        twin.insert("variables".to_string(), serde_json::Value::Object(new_vars));
    }

    let current_version = twin
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    twin.insert("version".to_string(), serde_json::json!(current_version + 1));

    Ok(())
}

pub fn diff(old: &crate::traits::MemoryPayload, new: &crate::traits::MemoryPayload) -> Vec<TwinDelta> {
    let old_state = get_state(old);
    let new_state = get_state(new);
    let mut all_keys: Vec<&String> = old_state.keys().chain(new_state.keys()).collect();
    all_keys.sort();
    all_keys.dedup();
    let mut result = Vec::new();
    for k in all_keys {
        let old_v = old_state.get(k);
        let new_v = new_state.get(k);
        match (old_v, new_v) {
            (Some(o), Some(n)) => {
                if (o - n).abs() > f64::EPSILON * o.abs().max(n.abs()).max(1.0) {
                    result.push(TwinDelta {
                        name: k.clone(),
                        old: Some(*o),
                        new: Some(*n),
                    });
                }
            }
            (Some(o), None) => {
                result.push(TwinDelta {
                    name: k.clone(),
                    old: Some(*o),
                    new: None,
                });
            }
            (None, Some(n)) => {
                result.push(TwinDelta {
                    name: k.clone(),
                    old: None,
                    new: Some(*n),
                });
            }
            (None, None) => {}
        }
    }
    result
}

pub fn validate_state(vars: &BTreeMap<String, f64>, schema: Option<&TwinSchema>) -> Vec<TwinWarning> {
    let mut warnings = Vec::new();
    if let Some(s) = schema {
        for (name, val) in vars {
            if let Some(bounds) = s.variables.get(name) {
                if *val < bounds.min || *val > bounds.max {
                    warnings.push(TwinWarning::OutOfBounds {
                        name: name.clone(),
                        value: *val,
                        min: bounds.min,
                        max: bounds.max,
                    });
                }
            } else {
                warnings.push(TwinWarning::UnknownVariable { name: name.clone() });
            }
        }
    }
    warnings
}

pub fn list_twins_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
) -> Vec<TwinSummary> {
    let mut best: BTreeMap<String, (String, u64, usize, i64, Option<String>, Option<String>)> = BTreeMap::new();
    let now = chrono::Utc::now().timestamp();
    for (id, payload) in rows {
        let is_twin_row = DIGITAL_TWIN_MEMORY_TYPES.contains(&payload.memory_type.as_str())
            || payload.metadata.get("twin").is_some();
        if !is_twin_row {
            continue;
        }
        let valid_to = payload.metadata.get("valid_to");
        let not_retired = match valid_to {
            None => true,
            Some(v) if v.is_null() => true,
            Some(v) => v.as_i64().map_or(false, |ts| ts == 0 || ts > now),
        };
        if !not_retired {
            continue;
        }
        let twin_id = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let Some(twin_id) = twin_id else { continue };
        let version = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let state = get_state(payload);
        let variable_count = state.len();
        let last_updated = payload
            .metadata
            .get("valid_from")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let source = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("source"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let schema_id = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("schema_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let entry = best.entry(twin_id.clone()).or_insert((
            id.clone(),
            version,
            variable_count,
            last_updated,
            source.clone(),
            schema_id.clone(),
        ));
        if version > entry.1 {
            *entry = (id.clone(), version, variable_count, last_updated, source, schema_id);
        }
    }
    let mut summaries: Vec<TwinSummary> = best
        .into_iter()
        .map(|(twin_id, (id, twin_version, variable_count, last_updated, source, schema_id))| {
            TwinSummary {
                id,
                twin_id: twin_id.clone(),
                twin_version,
                variable_count,
                last_updated,
                source,
                schema_id,
            }
        })
        .collect();
    summaries.sort_by(|a, b| a.twin_id.cmp(&b.twin_id));
    summaries
}

pub fn get_twin_state_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
    twin_id: &str,
) -> Option<BTreeMap<String, f64>> {
    let mut best_version: Option<u64> = None;
    let mut best_payload: Option<&crate::traits::MemoryPayload> = None;
    for (_id, payload) in rows {
        let is_twin_row = DIGITAL_TWIN_MEMORY_TYPES.contains(&payload.memory_type.as_str())
            || payload.metadata.get("twin").is_some();
        if !is_twin_row {
            continue;
        }
        let payload_twin_id = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str());
        if payload_twin_id != Some(twin_id) {
            continue;
        }
        let version = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if best_version.map_or(true, |bv| version >= bv) {
            best_version = Some(version);
            best_payload = Some(payload);
        }
    }
    best_payload.map(get_state)
}

/// `get_twin_state_from_rows` for callers that need an error, not an absence:
/// a twin id the rows do not carry is a lookup failure the caller must see
/// (`TwinError::NotFound`), never an empty state that reads as "no variables".
pub fn require_twin_state_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
    twin_id: &str,
) -> Result<BTreeMap<String, f64>, TwinError> {
    get_twin_state_from_rows(rows, twin_id)
        .ok_or_else(|| TwinError::NotFound(twin_id.to_string()))
}

pub fn twin_history_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
    twin_id: &str,
    limit: usize,
) -> Vec<(u64, i64, BTreeMap<String, f64>)> {
    let mut matched: Vec<(u64, i64, BTreeMap<String, f64>)> = Vec::new();
    for (_id, payload) in rows {
        let is_twin_row = DIGITAL_TWIN_MEMORY_TYPES.contains(&payload.memory_type.as_str())
            || payload.metadata.get("twin").is_some();
        if !is_twin_row {
            continue;
        }
        let payload_twin_id = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str());
        if payload_twin_id != Some(twin_id) {
            continue;
        }
        let version = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let valid_from = payload
            .metadata
            .get("valid_from")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let state = get_state(payload);
        matched.push((version, valid_from, state));
    }
    matched.sort_by(|a, b| a.0.cmp(&b.0));
    if limit > 0 {
        matched.truncate(limit);
    }
    matched
}

pub fn twin_diff_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
    twin_id: &str,
    from_version: u64,
    to_version: u64,
) -> Result<Vec<TwinDelta>, TwinError> {
    let mut from_payload: Option<&crate::traits::MemoryPayload> = None;
    let mut to_payload: Option<&crate::traits::MemoryPayload> = None;
    for (_id, payload) in rows {
        let is_twin_row = DIGITAL_TWIN_MEMORY_TYPES.contains(&payload.memory_type.as_str())
            || payload.metadata.get("twin").is_some();
        if !is_twin_row {
            continue;
        }
        let payload_twin_id = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str());
        if payload_twin_id != Some(twin_id) {
            continue;
        }
        let version = payload
            .metadata
            .get("twin")
            .and_then(|t| t.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if version == from_version {
            from_payload = Some(payload);
        }
        if version == to_version {
            to_payload = Some(payload);
        }
    }
    if from_payload.is_none() {
        return Err(TwinError::VersionNotFound {
            twin_id: twin_id.to_string(),
            version: from_version,
        });
    }
    if to_payload.is_none() {
        return Err(TwinError::VersionNotFound {
            twin_id: twin_id.to_string(),
            version: to_version,
        });
    }
    Ok(diff(from_payload.unwrap(), to_payload.unwrap()))
}

pub async fn list_twins(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
) -> Vec<TwinSummary> {
    match store.list(namespace, None, None).await {
        Ok(results) => {
            let rows: Vec<(String, crate::traits::MemoryPayload)> = results
                .into_iter()
                .map(|r| (r.id, r.payload))
                .collect();
            list_twins_from_rows(&rows)
        }
        Err(e) => {
            tracing::warn!("digital_twin store.list error in namespace '{}': {e}", namespace);
            Vec::new()
        }
    }
}

pub async fn get_twin_state(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
    twin_id: &str,
) -> Option<BTreeMap<String, f64>> {
    match store.list(namespace, None, None).await {
        Ok(results) => {
            let rows: Vec<(String, crate::traits::MemoryPayload)> = results
                .into_iter()
                .map(|r| (r.id, r.payload))
                .collect();
            let state = get_twin_state_from_rows(&rows, twin_id);
            if state.is_none() {
                tracing::debug!(
                    "[TWIN] state lookup missed for twin '{}' in namespace '{}'",
                    twin_id,
                    namespace
                );
            }
            state
        }
        Err(e) => {
            tracing::warn!("digital_twin store.list error in namespace '{}': {e}", namespace);
            None
        }
    }
}

pub async fn twin_history(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
    twin_id: &str,
    limit: usize,
) -> Vec<(u64, i64, BTreeMap<String, f64>)> {
    match store.list(namespace, None, None).await {
        Ok(results) => {
            let rows: Vec<(String, crate::traits::MemoryPayload)> = results
                .into_iter()
                .map(|r| (r.id, r.payload))
                .collect();
            twin_history_from_rows(&rows, twin_id, limit)
        }
        Err(e) => {
            tracing::warn!("digital_twin store.list error in namespace '{}': {e}", namespace);
            Vec::new()
        }
    }
}

pub async fn twin_diff(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
    twin_id: &str,
    from_version: u64,
    to_version: u64,
) -> Result<Vec<TwinDelta>, TwinError> {
    match store.list(namespace, None, None).await {
        Ok(results) => {
            let rows: Vec<(String, crate::traits::MemoryPayload)> = results
                .into_iter()
                .map(|r| (r.id, r.payload))
                .collect();
            twin_diff_from_rows(&rows, twin_id, from_version, to_version)
        }
        Err(e) => {
            tracing::warn!("digital_twin store.list error in namespace '{}': {e}", namespace);
            Err(TwinError::NotFound(format!(
                "store error for twin {}",
                twin_id
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_payload(metadata: serde_json::Value) -> crate::traits::MemoryPayload {
        crate::traits::MemoryPayload {
            content: "twin".into(),
            user_id: "u".into(),
            memory_type: "digital_twin".into(),
            agent_name: None,
            location: "".into(),
            location_lines: "".into(),
            metadata,
        }
    }

    #[test]
    fn get_state_extracts_finite_numbers() {
        let p = make_payload(json!({
            "twin": { "variables": { "a": 1.5, "b": 2.0, "c": "bad", "d": f64::NAN } }
        }));
        let state = get_state(&p);
        assert_eq!(state.len(), 2);
        assert!(state.contains_key("a"));
        assert!(state.contains_key("b"));
        assert!(!state.contains_key("c"));
        assert!(!state.contains_key("d"));
    }

    #[test]
    fn set_state_replace_and_merge() {
        let mut p = make_payload(json!({ "twin": { "variables": { "x": 1.0 } } }));
        let mut vars = BTreeMap::new();
        vars.insert("y".into(), 2.5);
        set_state(&mut p, &vars, false).unwrap();
        assert_eq!(p.memory_type, "digital_twin");
        let state = get_state(&p);
        assert!(!state.contains_key("x"));
        assert_eq!(state.get("y"), Some(&2.5));
        let version = p.metadata.get("twin").unwrap().get("version").unwrap().as_u64().unwrap();
        assert_eq!(version, 1);

        vars.clear();
        vars.insert("z".into(), 3.0);
        set_state(&mut p, &vars, true).unwrap();
        let state2 = get_state(&p);
        assert!(state2.contains_key("y"));
        assert!(state2.contains_key("z"));
    }

    #[test]
    fn set_state_rejects_non_finite() {
        let mut p = make_payload(json!({ "twin": {} }));
        let mut vars = BTreeMap::new();
        vars.insert("bad".into(), f64::INFINITY);
        assert!(matches!(
            set_state(&mut p, &vars, false),
            Err(TwinError::NonFinite { .. })
        ));
    }

    #[test]
    fn set_state_rejects_name_that_sanitizes_to_empty() {
        let mut p = make_payload(json!({ "twin": {} }));
        let mut vars = BTreeMap::new();
        vars.insert(String::new(), 1.0);
        assert!(matches!(
            set_state(&mut p, &vars, false),
            Err(TwinError::InvalidPayload(s)) if s.contains("invalid variable name")
        ));
    }

    #[test]
    fn require_twin_state_from_rows_maps_miss_to_not_found() {
        let rows = vec![(
            "id1".into(),
            make_payload(json!({ "twin": { "id": "t1", "version": 1, "variables": { "a": 1.0 } } })),
        )];
        let state = require_twin_state_from_rows(&rows, "t1").unwrap();
        assert_eq!(state.get("a"), Some(&1.0));
        assert!(matches!(
            require_twin_state_from_rows(&rows, "absent"),
            Err(TwinError::NotFound(id)) if id == "absent"
        ));
    }

    #[test]
    fn diff_detects_changes_adds_removes() {
        let old_p = make_payload(json!({ "twin": { "variables": { "a": 1.0, "b": 2.0 } } }));
        let new_p = make_payload(json!({ "twin": { "variables": { "a": 1.0, "c": 3.0 } } }));
        let d = diff(&old_p, &new_p);
        assert_eq!(d.len(), 2);
        let names: std::collections::BTreeSet<String> = d.iter().map(|x| x.name.clone()).collect();
        assert!(names.contains("b"));
        assert!(names.contains("c"));
    }

    #[test]
    fn validate_state_warns_bounds_and_unknown() {
        let schema = TwinSchema {
            variables: {
                let mut m = BTreeMap::new();
                m.insert("t".into(), TwinVarBounds { min: 0.0, max: 10.0, unit: "c".into() });
                m
            },
        };
        let mut vars = BTreeMap::new();
        vars.insert("t".into(), 5.0);
        vars.insert("unknown".into(), 1.0);
        vars.insert("t_bad".into(), 15.0);
        // since schema only has "t", "unknown" is unknown, "t_bad" is unknown, "t" is ok
        let warnings = validate_state(&vars, Some(&schema));
        assert!(!warnings.is_empty());
    }

    #[test]
    fn list_twins_from_rows_groups_and_picks_latest() {
        let rows = vec![
            ("id1".into(), make_payload(json!({
                "twin": { "id": "t1", "version": 1, "variables": { "a": 1.0 } },
                "valid_from": 100
            }))),
            ("id2".into(), make_payload(json!({
                "twin": { "id": "t1", "version": 2, "variables": { "a": 2.0 } },
                "valid_from": 200
            }))),
            ("id3".into(), make_payload(json!({
                "twin": { "id": "t2", "version": 1, "variables": { "b": 1.0 } },
                "valid_from": 150
            }))),
        ];
        let summaries = list_twins_from_rows(&rows);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].twin_id, "t1");
        assert_eq!(summaries[0].twin_version, 2);
        assert_eq!(summaries[1].twin_id, "t2");
    }

    #[test]
    fn twin_history_from_rows_sorted_and_truncated() {
        let rows = vec![
            ("id1".into(), make_payload(json!({ "twin": { "id": "t1", "version": 2 }, "valid_from": 200 }))),
            ("id2".into(), make_payload(json!({ "twin": { "id": "t1", "version": 1 }, "valid_from": 100 }))),
        ];
        let hist = twin_history_from_rows(&rows, "t1", 0);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].0, 1);
        assert_eq!(hist[1].0, 2);
    }

    #[test]
    fn twin_diff_from_rows_success_and_missing_version() {
        let rows = vec![
            ("id1".into(), make_payload(json!({ "twin": { "id": "t1", "version": 1, "variables": { "a": 1.0 } }, "valid_from": 100 }))),
            ("id2".into(), make_payload(json!({ "twin": { "id": "t1", "version": 2, "variables": { "a": 2.0 } }, "valid_from": 200 }))),
        ];
        let d = twin_diff_from_rows(&rows, "t1", 1, 2).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "a");

        assert!(twin_diff_from_rows(&rows, "t1", 99, 2).is_err());
    }

    #[test]
    fn list_twins_from_rows_future_and_expired_valid_to() {
        let now = chrono::Utc::now().timestamp();
        let rows = vec![
            ("id1".into(), make_payload(json!({
                "twin": { "id": "tfuture", "version": 1, "variables": { "a": 1.0 } },
                "valid_to": now + 3600
            }))),
            ("id2".into(), make_payload(json!({
                "twin": { "id": "texpired", "version": 1, "variables": { "b": 1.0 } },
                "valid_to": now - 60
            }))),
        ];
        let summaries = list_twins_from_rows(&rows);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].twin_id, "tfuture");
    }

    #[test]
    fn get_twin_state_from_rows_reaches_version_zero() {
        let rows = vec![(
            "id1".into(),
            make_payload(json!({
                "twin": { "id": "t0", "version": 0, "variables": { "a": 1.0 } }
            })),
        )];
        let state = get_twin_state_from_rows(&rows, "t0");
        assert!(state.is_some());
        assert_eq!(state.unwrap().get("a"), Some(&1.0));
    }

    #[test]
    fn sanitize_var_name_replaces_invalid() {
        assert_eq!(sanitize_var_name("hello-world!"), "hello_world_");
        assert_eq!(sanitize_var_name("a_b"), "a_b");
    }
}
