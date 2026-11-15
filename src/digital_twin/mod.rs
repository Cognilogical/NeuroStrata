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
    UnknownVariable {
        name: String,
    },
    MissingInterfaceVariable {
        name: String,
        interface_slug: Option<String>,
    },
    InvalidAttribute {
        name: String,
        reason: String,
    },
    LinkCardinalityViolation {
        relation: String,
        direction: String,
        actual: u32,
        min_count: u32,
        max_count: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TwinAttrValue {
    Bool(bool),
    I64(i64),
    String(String),
}

fn default_attr_type() -> String {
    "string".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TwinAttrSpec {
    #[serde(default = "default_attr_type")]
    pub attr_type: String,
    #[serde(default)]
    pub in_values: Option<Vec<String>>,
    #[serde(default)]
    pub required: bool,
}

fn default_link_direction() -> String {
    "outgoing".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TwinLinkConstraint {
    pub relation: String,
    #[serde(default = "default_link_direction")]
    pub direction: String,
    #[serde(default)]
    pub target_interface_or_schema: Option<String>,
    #[serde(default)]
    pub target_memory_type: Option<String>,
    #[serde(default)]
    pub min_count: u32,
    #[serde(default)]
    pub max_count: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReducerAgg {
    #[serde(alias = "Sum", alias = "SUM")]
    Sum,
    #[serde(alias = "Avg", alias = "AVG")]
    Avg,
    #[serde(alias = "Min", alias = "MIN")]
    Min,
    #[serde(alias = "Max", alias = "MAX")]
    Max,
    #[serde(alias = "Count", alias = "COUNT")]
    Count,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyReducerSpec {
    pub agg: ReducerAgg,
    pub relation: String,
    #[serde(default = "default_link_direction")]
    pub direction: String,
    #[serde(default)]
    pub target_interface_or_schema: Option<String>,
    #[serde(default)]
    pub source_variable: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TwinStateWithReducers {
    pub variables: BTreeMap<String, f64>,
    pub derived_variables: BTreeMap<String, f64>,
    pub attributes: BTreeMap<String, TwinAttrValue>,
    pub implements_interfaces: Vec<String>,
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

fn default_schema_kind() -> String {
    "object_schema".to_string()
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TwinSchema {
    #[serde(default = "default_schema_kind")]
    pub kind: String,
    #[serde(default)]
    pub schema_id: Option<String>,
    #[serde(default)]
    pub interface_slug: Option<String>,
    #[serde(default)]
    pub implements_interfaces: Vec<String>,
    #[serde(default)]
    pub variables: BTreeMap<String, TwinVarBounds>,
    #[serde(default)]
    pub attributes: BTreeMap<String, TwinAttrSpec>,
    #[serde(default)]
    pub link_constraints: Vec<TwinLinkConstraint>,
    #[serde(default)]
    pub reducers: BTreeMap<String, PropertyReducerSpec>,
    #[serde(default)]
    pub resolved_interfaces: Vec<String>,
    #[serde(default)]
    pub interface_variable_sources: BTreeMap<String, Option<String>>,
}

impl TwinSchema {
    pub fn from_metadata(metadata: &serde_json::Value) -> Option<Self> {
        let twin_schema = metadata.get("twin_schema")?;
        let obj = twin_schema.as_object()?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(default_schema_kind);
        let schema_id = obj
            .get("schema_id")
            .or_else(|| obj.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let interface_slug = obj
            .get("interface_slug")
            .or_else(|| obj.get("slug"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let implements_interfaces = obj
            .get("implements_interfaces")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let variables = obj
            .get("variables")
            .and_then(|v| v.as_object())
            .map(|vars_obj| {
                vars_obj
                    .iter()
                    .filter_map(|(k, v)| {
                        let bounds_obj = v.as_object()?;
                        let min = bounds_obj.get("min")?.as_f64()?;
                        let max = bounds_obj.get("max")?.as_f64()?;
                        let unit = bounds_obj
                            .get("unit")
                            .and_then(|u| u.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some((k.clone(), TwinVarBounds { min, max, unit }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let attributes = obj
            .get("attributes")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let link_constraints = obj
            .get("link_constraints")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let reducers = obj
            .get("reducers")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        Some(TwinSchema {
            kind,
            schema_id,
            interface_slug,
            implements_interfaces,
            variables,
            attributes,
            link_constraints,
            reducers,
            resolved_interfaces: Vec::new(),
            interface_variable_sources: BTreeMap::new(),
        })
    }
}

pub fn collect_twin_schemas_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
) -> BTreeMap<String, TwinSchema> {
    let mut schemas = BTreeMap::new();
    let now = chrono::Utc::now().timestamp();
    for (id, payload) in rows {
        let is_schema_row = payload.memory_type == "digital_twin_schema"
            || payload.metadata.get("twin_schema").is_some();
        if !is_schema_row {
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
        if let Some(mut schema) = TwinSchema::from_metadata(&payload.metadata) {
            if schema.schema_id.is_none() {
                schema.schema_id = Some(id.clone());
            }
            if schema.kind == "interface" && schema.interface_slug.is_none() {
                schema.interface_slug = schema.schema_id.clone();
            }
            schemas.insert(id.clone(), schema.clone());
            if let Some(ref sid) = schema.schema_id {
                schemas.insert(sid.clone(), schema.clone());
            }
            if let Some(ref slug) = schema.interface_slug {
                schemas.insert(slug.clone(), schema);
            }
        }
    }
    schemas
}

pub fn resolve_effective_twin_schema(
    schema: &TwinSchema,
    schemas: &BTreeMap<String, TwinSchema>,
) -> TwinSchema {
    let mut effective = schema.clone();
    let mut visited = std::collections::BTreeSet::new();
    if let Some(ref sid) = schema.schema_id {
        visited.insert(sid.clone());
    }
    if let Some(ref slug) = schema.interface_slug {
        visited.insert(slug.clone());
    }
    let mut queue: std::collections::VecDeque<(String, usize)> = schema
        .implements_interfaces
        .iter()
        .map(|i| (i.clone(), 1usize))
        .collect();
    let mut resolved_ifaces = Vec::new();
    if schema.kind == "interface" {
        if let Some(ref slug) = schema.interface_slug {
            resolved_ifaces.push(slug.clone());
        }
    }

    while let Some((iface_key, depth)) = queue.pop_front() {
        if depth > 8 || !visited.insert(iface_key.clone()) {
            continue;
        }
        if let Some(iface) = schemas.get(&iface_key) {
            let canonical_slug = iface
                .interface_slug
                .clone()
                .or_else(|| iface.schema_id.clone())
                .unwrap_or_else(|| iface_key.clone());
            visited.insert(canonical_slug.clone());
            if !resolved_ifaces.contains(&canonical_slug) {
                resolved_ifaces.push(canonical_slug.clone());
            }
            for (var_name, iface_bounds) in &iface.variables {
                effective
                    .interface_variable_sources
                    .entry(var_name.clone())
                    .or_insert_with(|| Some(canonical_slug.clone()));
                if let Some(child_bounds) = effective.variables.get_mut(var_name) {
                    let eff_min = child_bounds.min.max(iface_bounds.min);
                    let eff_max = child_bounds.max.min(iface_bounds.max);
                    if eff_min <= eff_max {
                        child_bounds.min = eff_min;
                        child_bounds.max = eff_max;
                    }
                    if child_bounds.unit.is_empty() && !iface_bounds.unit.is_empty() {
                        child_bounds.unit = iface_bounds.unit.clone();
                    }
                } else {
                    effective
                        .variables
                        .insert(var_name.clone(), iface_bounds.clone());
                }
            }
            for (attr_name, attr_spec) in &iface.attributes {
                effective
                    .attributes
                    .entry(attr_name.clone())
                    .or_insert_with(|| attr_spec.clone());
            }
            for lc in &iface.link_constraints {
                if !effective.link_constraints.contains(lc) {
                    effective.link_constraints.push(lc.clone());
                }
            }
            for (red_name, red_spec) in &iface.reducers {
                effective
                    .reducers
                    .entry(red_name.clone())
                    .or_insert_with(|| red_spec.clone());
            }
            for parent_iface in &iface.implements_interfaces {
                queue.push_back((parent_iface.clone(), depth + 1));
            }
        } else if !resolved_ifaces.contains(&iface_key) {
            resolved_ifaces.push(iface_key);
        }
    }

    effective.resolved_interfaces = resolved_ifaces;
    effective
}

pub fn sanitize_var_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn get_state(payload: &crate::traits::MemoryPayload) -> BTreeMap<String, f64> {
    let mut result = BTreeMap::new();
    if let Some(vars) = payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("variables"))
    {
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
        if !twin.contains_key("variables") || twin.get("variables").map_or(true, |v| !v.is_object())
        {
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

    let current_version = twin.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    twin.insert(
        "version".to_string(),
        serde_json::json!(current_version + 1),
    );

    Ok(())
}

pub fn diff(
    old: &crate::traits::MemoryPayload,
    new: &crate::traits::MemoryPayload,
) -> Vec<TwinDelta> {
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

pub fn get_attributes(payload: &crate::traits::MemoryPayload) -> BTreeMap<String, TwinAttrValue> {
    let mut result = BTreeMap::new();
    if let Some(attrs) = payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("attributes"))
    {
        if let Some(obj) = attrs.as_object() {
            for (k, v) in obj {
                if let Some(b) = v.as_bool() {
                    result.insert(k.clone(), TwinAttrValue::Bool(b));
                } else if let Some(i) = v.as_i64() {
                    result.insert(k.clone(), TwinAttrValue::I64(i));
                } else if let Some(s) = v.as_str() {
                    result.insert(k.clone(), TwinAttrValue::String(s.to_string()));
                }
            }
        }
    }
    result
}

pub fn set_attributes(
    payload: &mut crate::traits::MemoryPayload,
    attrs: &BTreeMap<String, TwinAttrValue>,
    merge: bool,
) -> Result<(), TwinError> {
    for name in attrs.keys() {
        if sanitize_var_name(name).is_empty() {
            return Err(TwinError::InvalidPayload(format!(
                "invalid attribute name: {}",
                name
            )));
        }
    }
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

    let to_json = |val: &TwinAttrValue| match val {
        TwinAttrValue::Bool(b) => serde_json::json!(*b),
        TwinAttrValue::I64(i) => serde_json::json!(*i),
        TwinAttrValue::String(s) => serde_json::json!(s),
    };

    if merge {
        if !twin.contains_key("attributes")
            || twin.get("attributes").map_or(true, |v| !v.is_object())
        {
            twin.insert("attributes".to_string(), serde_json::json!({}));
        }
        let attrs_obj = twin
            .get_mut("attributes")
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| TwinError::InvalidPayload("attributes is not an object".to_string()))?;
        for (k, v) in attrs {
            attrs_obj.insert(sanitize_var_name(k), to_json(v));
        }
    } else {
        let new_attrs: serde_json::Map<String, serde_json::Value> = attrs
            .iter()
            .map(|(k, v)| (sanitize_var_name(k), to_json(v)))
            .collect();
        twin.insert(
            "attributes".to_string(),
            serde_json::Value::Object(new_attrs),
        );
    }
    Ok(())
}

pub fn validate_attributes(
    attrs: &BTreeMap<String, TwinAttrValue>,
    schema: Option<&TwinSchema>,
) -> Vec<TwinWarning> {
    let mut warnings = Vec::new();
    let Some(s) = schema else {
        return warnings;
    };
    if s.attributes.is_empty() {
        return warnings;
    }
    for (spec_name, spec) in &s.attributes {
        if spec.required && !attrs.contains_key(spec_name) {
            warnings.push(TwinWarning::InvalidAttribute {
                name: spec_name.clone(),
                reason: "required attribute is missing".to_string(),
            });
        }
    }
    for (name, val) in attrs {
        match s.attributes.get(name) {
            None => {
                warnings.push(TwinWarning::InvalidAttribute {
                    name: name.clone(),
                    reason: "unknown attribute".to_string(),
                });
            }
            Some(spec) => match (spec.attr_type.as_str(), val) {
                ("bool", TwinAttrValue::Bool(_)) => {}
                ("i64", TwinAttrValue::I64(_)) => {}
                ("string", TwinAttrValue::String(_)) => {}
                ("enum", TwinAttrValue::String(st)) => {
                    if let Some(ref allowed) = spec.in_values {
                        if !allowed.iter().any(|a| a == st) {
                            warnings.push(TwinWarning::InvalidAttribute {
                                name: name.clone(),
                                reason: format!("value '{}' not in allowed enum {:?}", st, allowed),
                            });
                        }
                    }
                }
                (expected, actual) => {
                    warnings.push(TwinWarning::InvalidAttribute {
                        name: name.clone(),
                        reason: format!("expected type '{}', got {:?}", expected, actual),
                    });
                }
            },
        }
    }
    warnings
}

pub fn validate_state(
    vars: &BTreeMap<String, f64>,
    schema: Option<&TwinSchema>,
) -> Vec<TwinWarning> {
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
        for (req_var, iface_slug) in &s.interface_variable_sources {
            if !vars.contains_key(req_var) {
                warnings.push(TwinWarning::MissingInterfaceVariable {
                    name: req_var.clone(),
                    interface_slug: iface_slug.clone(),
                });
            }
        }
    }
    warnings
}

pub fn list_twins_from_rows(rows: &[(String, crate::traits::MemoryPayload)]) -> Vec<TwinSummary> {
    let mut best: BTreeMap<String, (String, u64, usize, i64, Option<String>, Option<String>)> =
        BTreeMap::new();
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
            *entry = (
                id.clone(),
                version,
                variable_count,
                last_updated,
                source,
                schema_id,
            );
        }
    }
    let mut summaries: Vec<TwinSummary> = best
        .into_iter()
        .map(
            |(twin_id, (id, twin_version, variable_count, last_updated, source, schema_id))| {
                TwinSummary {
                    id,
                    twin_id: twin_id.clone(),
                    twin_version,
                    variable_count,
                    last_updated,
                    source,
                    schema_id,
                }
            },
        )
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
    get_twin_state_from_rows(rows, twin_id).ok_or_else(|| TwinError::NotFound(twin_id.to_string()))
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

pub fn stage_twin_mutation(
    prev_row_id: &str,
    prev_payload: &crate::traits::MemoryPayload,
    new_vars: &BTreeMap<String, f64>,
    merge: bool,
    source: Option<&str>,
    now_ts: i64,
) -> (
    String,
    u64,
    u64,
    crate::traits::MemoryPayload,
    Vec<TwinDelta>,
) {
    let twin_id = prev_payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("id"))
        .or_else(|| {
            prev_payload
                .metadata
                .get("digital_twin")
                .and_then(|t| t.get("twin_id").or_else(|| t.get("id")))
        })
        .and_then(|v| v.as_str())
        .unwrap_or(prev_row_id)
        .to_string();
    let prev_version = prev_payload
        .metadata
        .get("twin")
        .and_then(|t| t.get("version"))
        .or_else(|| {
            prev_payload
                .metadata
                .get("digital_twin")
                .and_then(|t| t.get("version"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    let next_version = prev_version + 1;

    let mut next_payload = prev_payload.clone();
    next_payload.memory_type = "digital_twin".to_string();
    let _ = set_state(&mut next_payload, new_vars, merge);
    let state_after = get_state(&next_payload);

    if !next_payload.metadata.is_object() {
        next_payload.metadata = serde_json::json!({});
    }
    if let Some(meta) = next_payload.metadata.as_object_mut() {
        meta.remove("valid_to");
        meta.remove("superseded_by");
        meta.insert("valid_from".to_string(), serde_json::json!(now_ts));
        meta.insert("supersedes".to_string(), serde_json::json!(prev_row_id));
        if let Some(twin_obj) = meta.get_mut("twin").and_then(|t| t.as_object_mut()) {
            twin_obj.insert("id".to_string(), serde_json::json!(twin_id));
            twin_obj.insert("version".to_string(), serde_json::json!(next_version));
            if let Some(src) = source {
                twin_obj.insert("source".to_string(), serde_json::json!(src));
            }
        }
    }
    next_payload.content = format!(
        "Digital twin '{}' v{}: {:?}",
        twin_id, next_version, state_after
    );
    let deltas = diff(prev_payload, &next_payload);
    (twin_id, prev_version, next_version, next_payload, deltas)
}

pub async fn list_twins(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
) -> Vec<TwinSummary> {
    match store.list(namespace, None, None).await {
        Ok(results) => {
            let rows: Vec<(String, crate::traits::MemoryPayload)> =
                results.into_iter().map(|r| (r.id, r.payload)).collect();
            list_twins_from_rows(&rows)
        }
        Err(e) => {
            tracing::warn!(
                "digital_twin store.list error in namespace '{}': {e}",
                namespace
            );
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
            let rows: Vec<(String, crate::traits::MemoryPayload)> =
                results.into_iter().map(|r| (r.id, r.payload)).collect();
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
            tracing::warn!(
                "digital_twin store.list error in namespace '{}': {e}",
                namespace
            );
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
            let rows: Vec<(String, crate::traits::MemoryPayload)> =
                results.into_iter().map(|r| (r.id, r.payload)).collect();
            twin_history_from_rows(&rows, twin_id, limit)
        }
        Err(e) => {
            tracing::warn!(
                "digital_twin store.list error in namespace '{}': {e}",
                namespace
            );
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
            let rows: Vec<(String, crate::traits::MemoryPayload)> =
                results.into_iter().map(|r| (r.id, r.payload)).collect();
            twin_diff_from_rows(&rows, twin_id, from_version, to_version)
        }
        Err(e) => {
            tracing::warn!(
                "digital_twin store.list error in namespace '{}': {e}",
                namespace
            );
            Err(TwinError::NotFound(format!(
                "store error for twin {}",
                twin_id
            )))
        }
    }
}

fn collect_neighbor_row_ids(
    parent_row_id: &str,
    parent_twin_id: &str,
    parent_payload: &crate::traits::MemoryPayload,
    relation: &str,
    direction: &str,
    rows: &[(String, crate::traits::MemoryPayload)],
    edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)],
) -> Vec<String> {
    let rel_upper = relation.trim().to_ascii_uppercase();
    let anchor_key = match rel_upper.as_str() {
        "CONTAINS" => Some("contains"),
        "DEPENDS_ON" => Some("depends_on"),
        "RELATES_TO" => Some("relates_to"),
        "GOVERNS" => Some("governs"),
        _ => None,
    };
    let is_incoming = direction.eq_ignore_ascii_case("incoming");

    // Map any row_id of a twin -> its twin_id, and twin_id -> active latest row_id.
    let summaries = list_twins_from_rows(rows);
    let mut active_twin_row_by_twin_id: BTreeMap<String, String> = BTreeMap::new();
    for s in &summaries {
        active_twin_row_by_twin_id.insert(s.twin_id.clone(), s.id.clone());
    }
    let mut twin_id_by_any_row_id: BTreeMap<String, String> = BTreeMap::new();
    for (rid, pl) in rows {
        if let Some(tid) = pl
            .metadata
            .get("twin")
            .and_then(|t| t.get("id").or_else(|| t.get("twin_id")))
            .and_then(|v| v.as_str())
        {
            twin_id_by_any_row_id.insert(rid.clone(), tid.to_string());
        }
    }

    let is_parent_ref = |r: &str| -> bool {
        r == parent_row_id
            || r == parent_twin_id
            || twin_id_by_any_row_id
                .get(r)
                .is_some_and(|tid| tid == parent_twin_id)
    };

    let canonicalize_target = |raw_target: &str| -> String {
        if let Some(active_rid) = active_twin_row_by_twin_id.get(raw_target) {
            return active_rid.clone();
        }
        if let Some(tid) = twin_id_by_any_row_id.get(raw_target) {
            if let Some(active_rid) = active_twin_row_by_twin_id.get(tid) {
                return active_rid.clone();
            }
        }
        raw_target.to_string()
    };

    let mut targets = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for (from, rel, to, _) in edges {
        if !rel.eq_ignore_ascii_case(&rel_upper) {
            continue;
        }
        if !is_incoming && is_parent_ref(from) {
            let c = canonicalize_target(to);
            if seen.insert(c.clone()) {
                targets.push(c);
            }
        } else if is_incoming && is_parent_ref(to) {
            let c = canonicalize_target(from);
            if seen.insert(c.clone()) {
                targets.push(c);
            }
        }
    }

    if let Some(ak) = anchor_key {
        if !is_incoming {
            if let Some(arr) = parent_payload
                .metadata
                .get("bi_directional_anchors")
                .and_then(|b| b.get(ak))
                .and_then(|v| v.as_array())
            {
                for item in arr.iter().filter_map(|v| v.as_str()) {
                    let c = canonicalize_target(item);
                    if seen.insert(c.clone()) {
                        targets.push(c);
                    }
                }
            }
        } else {
            for (rid, pl) in rows {
                if let Some(arr) = pl
                    .metadata
                    .get("bi_directional_anchors")
                    .and_then(|b| b.get(ak))
                    .and_then(|v| v.as_array())
                {
                    if arr.iter().filter_map(|v| v.as_str()).any(&is_parent_ref) {
                        let c = canonicalize_target(rid);
                        if seen.insert(c.clone()) {
                            targets.push(c);
                        }
                    }
                }
            }
        }
    }

    targets
}

fn neighbor_matches_target_schema_or_interface(
    neighbor_payload: &crate::traits::MemoryPayload,
    target_filter: Option<&str>,
    schemas: &BTreeMap<String, TwinSchema>,
) -> bool {
    let Some(target) = target_filter else {
        return true;
    };
    let Some(twin_obj) = neighbor_payload.metadata.get("twin") else {
        return false;
    };
    if let Some(ifaces) = twin_obj
        .get("implements_interfaces")
        .and_then(|v| v.as_array())
    {
        if ifaces
            .iter()
            .filter_map(|v| v.as_str())
            .any(|i| i == target)
        {
            return true;
        }
    }
    let Some(sid) = twin_obj.get("schema_id").and_then(|v| v.as_str()) else {
        return false;
    };
    if sid == target {
        return true;
    }
    if let Some(schema) = schemas.get(sid) {
        let eff = resolve_effective_twin_schema(schema, schemas);
        if eff.schema_id.as_deref() == Some(target)
            || eff.interface_slug.as_deref() == Some(target)
            || eff.resolved_interfaces.iter().any(|i| i == target)
        {
            return true;
        }
    }
    false
}

pub fn validate_link_constraints(
    parent_row_id: &str,
    parent_twin_id: &str,
    parent_payload: &crate::traits::MemoryPayload,
    effective_schema: &TwinSchema,
    rows: &[(String, crate::traits::MemoryPayload)],
    edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)],
    schemas: &BTreeMap<String, TwinSchema>,
) -> Vec<TwinWarning> {
    let mut warnings = Vec::new();
    if effective_schema.link_constraints.is_empty() {
        return warnings;
    }
    let row_by_id: BTreeMap<&str, &crate::traits::MemoryPayload> =
        rows.iter().map(|(id, p)| (id.as_str(), p)).collect();

    for lc in &effective_schema.link_constraints {
        let candidate_ids = collect_neighbor_row_ids(
            parent_row_id,
            parent_twin_id,
            parent_payload,
            &lc.relation,
            &lc.direction,
            rows,
            edges,
        );
        let mut count = 0u32;
        for cid in candidate_ids {
            if let Some(pl) = row_by_id.get(cid.as_str()) {
                if let Some(ref mt) = lc.target_memory_type {
                    if &pl.memory_type != mt {
                        continue;
                    }
                }
                if !neighbor_matches_target_schema_or_interface(
                    pl,
                    lc.target_interface_or_schema.as_deref(),
                    schemas,
                ) {
                    continue;
                }
                count += 1;
            }
        }
        let below_min = count < lc.min_count;
        let above_max = lc.max_count.is_some_and(|m| count > m);
        if below_min || above_max {
            warnings.push(TwinWarning::LinkCardinalityViolation {
                relation: lc.relation.clone(),
                direction: lc.direction.clone(),
                actual: count,
                min_count: lc.min_count,
                max_count: lc.max_count,
            });
        }
    }
    warnings
}

pub fn compute_twin_reducers(
    parent_row_id: &str,
    parent_twin_id: &str,
    parent_payload: &crate::traits::MemoryPayload,
    effective_schema: &TwinSchema,
    rows: &[(String, crate::traits::MemoryPayload)],
    edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)],
    schemas: &BTreeMap<String, TwinSchema>,
    budget: Option<crate::rdf::QueryBudget>,
) -> BTreeMap<String, f64> {
    let mut derived = BTreeMap::new();
    if effective_schema.reducers.is_empty() {
        return derived;
    }
    let budget = budget.unwrap_or(crate::rdf::QueryBudget {
        max_ms: 50,
        max_nodes: 40,
    });
    let max_nodes = if budget.max_nodes == 0 {
        40
    } else {
        budget.max_nodes
    };
    let max_ms = if budget.max_ms == 0 {
        50
    } else {
        budget.max_ms
    };
    let started = std::time::Instant::now();

    let row_by_id: BTreeMap<&str, &crate::traits::MemoryPayload> =
        rows.iter().map(|(id, p)| (id.as_str(), p)).collect();

    for (reducer_name, spec) in &effective_schema.reducers {
        if started.elapsed().as_millis() as u64 > max_ms {
            break;
        }
        let neighbor_ids = collect_neighbor_row_ids(
            parent_row_id,
            parent_twin_id,
            parent_payload,
            &spec.relation,
            &spec.direction,
            rows,
            edges,
        );
        let mut values = Vec::new();
        let mut matched_neighbors = 0usize;
        for nid in neighbor_ids {
            if matched_neighbors >= max_nodes || (started.elapsed().as_millis() as u64) > max_ms {
                break;
            }
            let Some(n_payload) = row_by_id.get(nid.as_str()) else {
                continue;
            };
            if !neighbor_matches_target_schema_or_interface(
                n_payload,
                spec.target_interface_or_schema.as_deref(),
                schemas,
            ) {
                continue;
            }
            matched_neighbors += 1;
            if let Some(ref src_var) = spec.source_variable {
                let n_vars = get_state(n_payload);
                if let Some(&v) = n_vars.get(src_var) {
                    if v.is_finite() {
                        values.push(v);
                    }
                }
            }
        }

        match spec.agg {
            ReducerAgg::Count => {
                derived.insert(reducer_name.clone(), matched_neighbors as f64);
            }
            ReducerAgg::Sum => {
                let sum: f64 = values.iter().copied().sum();
                if sum.is_finite() {
                    derived.insert(reducer_name.clone(), sum);
                }
            }
            ReducerAgg::Avg => {
                let avg = if values.is_empty() {
                    0.0
                } else {
                    let sum: f64 = values.iter().copied().sum();
                    let a = sum / (values.len() as f64);
                    if a.is_finite() {
                        a
                    } else {
                        0.0
                    }
                };
                derived.insert(reducer_name.clone(), avg);
            }
            ReducerAgg::Min => {
                if let Some(min_v) = values.iter().copied().reduce(f64::min) {
                    if min_v.is_finite() {
                        derived.insert(reducer_name.clone(), min_v);
                    }
                }
            }
            ReducerAgg::Max => {
                if let Some(max_v) = values.iter().copied().reduce(f64::max) {
                    if max_v.is_finite() {
                        derived.insert(reducer_name.clone(), max_v);
                    }
                }
            }
        }
    }

    derived
}

pub fn get_twin_state_with_reducers_from_rows(
    rows: &[(String, crate::traits::MemoryPayload)],
    edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)],
    twin_id: &str,
) -> Option<TwinStateWithReducers> {
    let summaries = list_twins_from_rows(rows);
    let summary = summaries.into_iter().find(|s| s.twin_id == twin_id)?;
    let (_, payload) = rows.iter().find(|(id, _)| id == &summary.id)?;
    let variables = get_state(payload);
    let attributes = get_attributes(payload);
    let schemas = collect_twin_schemas_from_rows(rows);

    let (derived_variables, implements_interfaces) = if let Some(ref sid) = summary.schema_id {
        if let Some(raw_schema) = schemas.get(sid) {
            let eff = resolve_effective_twin_schema(raw_schema, &schemas);
            let derived = compute_twin_reducers(
                &summary.id,
                twin_id,
                payload,
                &eff,
                rows,
                edges,
                &schemas,
                None,
            );
            (derived, eff.resolved_interfaces)
        } else {
            (BTreeMap::new(), Vec::new())
        }
    } else {
        (BTreeMap::new(), Vec::new())
    };

    Some(TwinStateWithReducers {
        variables,
        derived_variables,
        attributes,
        implements_interfaces,
    })
}

pub async fn get_twin_state_with_reducers(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
    twin_id: &str,
) -> Option<TwinStateWithReducers> {
    let results = store.list(namespace, None, None).await.ok()?;
    let rows: Vec<(String, crate::traits::MemoryPayload)> =
        results.into_iter().map(|r| (r.id, r.payload)).collect();
    let edges = store.list_edges(namespace).await.unwrap_or_default();
    get_twin_state_with_reducers_from_rows(&rows, &edges, twin_id)
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
        let version = p
            .metadata
            .get("twin")
            .unwrap()
            .get("version")
            .unwrap()
            .as_u64()
            .unwrap();
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
            make_payload(
                json!({ "twin": { "id": "t1", "version": 1, "variables": { "a": 1.0 } } }),
            ),
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
                m.insert(
                    "t".into(),
                    TwinVarBounds {
                        min: 0.0,
                        max: 10.0,
                        unit: "c".into(),
                    },
                );
                m
            },
            ..Default::default()
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
            (
                "id1".into(),
                make_payload(json!({
                    "twin": { "id": "t1", "version": 1, "variables": { "a": 1.0 } },
                    "valid_from": 100
                })),
            ),
            (
                "id2".into(),
                make_payload(json!({
                    "twin": { "id": "t1", "version": 2, "variables": { "a": 2.0 } },
                    "valid_from": 200
                })),
            ),
            (
                "id3".into(),
                make_payload(json!({
                    "twin": { "id": "t2", "version": 1, "variables": { "b": 1.0 } },
                    "valid_from": 150
                })),
            ),
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
            (
                "id1".into(),
                make_payload(json!({ "twin": { "id": "t1", "version": 2 }, "valid_from": 200 })),
            ),
            (
                "id2".into(),
                make_payload(json!({ "twin": { "id": "t1", "version": 1 }, "valid_from": 100 })),
            ),
        ];
        let hist = twin_history_from_rows(&rows, "t1", 0);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].0, 1);
        assert_eq!(hist[1].0, 2);
    }

    #[test]
    fn twin_diff_from_rows_success_and_missing_version() {
        let rows = vec![
            (
                "id1".into(),
                make_payload(
                    json!({ "twin": { "id": "t1", "version": 1, "variables": { "a": 1.0 } }, "valid_from": 100 }),
                ),
            ),
            (
                "id2".into(),
                make_payload(
                    json!({ "twin": { "id": "t1", "version": 2, "variables": { "a": 2.0 } }, "valid_from": 200 }),
                ),
            ),
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
            (
                "id1".into(),
                make_payload(json!({
                    "twin": { "id": "tfuture", "version": 1, "variables": { "a": 1.0 } },
                    "valid_to": now + 3600
                })),
            ),
            (
                "id2".into(),
                make_payload(json!({
                    "twin": { "id": "texpired", "version": 1, "variables": { "b": 1.0 } },
                    "valid_to": now - 60
                })),
            ),
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

    #[test]
    fn polymorphic_interface_inheritance_and_validate_state() {
        let mut thermal_payload = make_payload(json!({
            "twin_schema": {
                "kind": "interface",
                "interface_slug": "thermal-asset",
                "variables": {
                    "temperature": { "min": -50.0, "max": 200.0, "unit": "C" }
                }
            }
        }));
        thermal_payload.memory_type = "digital_twin_schema".into();

        let mut pump_payload = make_payload(json!({
            "twin_schema": {
                "kind": "object_schema",
                "schema_id": "cooling-pump",
                "implements_interfaces": ["thermal-asset"],
                "variables": {
                    "power_kw": { "min": 0.0, "max": 500.0, "unit": "kW" }
                }
            }
        }));
        pump_payload.memory_type = "digital_twin_schema".into();

        let mut chiller_payload = make_payload(json!({
            "twin_schema": {
                "kind": "object_schema",
                "schema_id": "chiller-unit",
                "implements_interfaces": ["thermal-asset"],
                "variables": {
                    "temperature": { "min": -10.0, "max": 80.0, "unit": "C" }
                }
            }
        }));
        chiller_payload.memory_type = "digital_twin_schema".into();

        let rows = vec![
            ("iface-thermal".into(), thermal_payload),
            ("schema-pump".into(), pump_payload),
            ("schema-chiller".into(), chiller_payload),
        ];
        let schemas = collect_twin_schemas_from_rows(&rows);
        let pump_eff =
            resolve_effective_twin_schema(schemas.get("cooling-pump").unwrap(), &schemas);
        let chiller_eff =
            resolve_effective_twin_schema(schemas.get("chiller-unit").unwrap(), &schemas);

        assert_eq!(
            pump_eff.resolved_interfaces,
            vec!["thermal-asset".to_string()]
        );
        assert!(pump_eff.variables.contains_key("temperature"));

        // Chiller tightens temperature bounds to [-10.0, 80.0].
        let chiller_temp = chiller_eff.variables.get("temperature").unwrap();
        assert_eq!(chiller_temp.min, -10.0);
        assert_eq!(chiller_temp.max, 80.0);

        // Out-of-bounds temperature on cooling-pump and chiller-unit triggers OutOfBounds.
        let mut pump_vars = BTreeMap::new();
        pump_vars.insert("temperature".into(), 250.0);
        pump_vars.insert("power_kw".into(), 15.0);
        let pump_warnings = validate_state(&pump_vars, Some(&pump_eff));
        assert!(pump_warnings
            .iter()
            .any(|w| matches!(w, TwinWarning::OutOfBounds { name, .. } if name == "temperature")));

        let mut chiller_vars = BTreeMap::new();
        chiller_vars.insert("temperature".into(), 100.0);
        let chiller_warnings = validate_state(&chiller_vars, Some(&chiller_eff));
        assert!(chiller_warnings
            .iter()
            .any(|w| matches!(w, TwinWarning::OutOfBounds { name, .. } if name == "temperature")));

        // Missing required interface variable triggers MissingInterfaceVariable.
        let mut missing_temp = BTreeMap::new();
        missing_temp.insert("power_kw".into(), 15.0);
        let missing_warnings = validate_state(&missing_temp, Some(&pump_eff));
        assert!(missing_warnings.iter().any(|w| matches!(
            w,
            TwinWarning::MissingInterfaceVariable { name, interface_slug }
            if name == "temperature" && interface_slug.as_deref() == Some("thermal-asset")
        )));
    }

    #[test]
    fn property_reducer_sum_avg_and_empty_zero_guard() {
        let mut loop_schema = make_payload(json!({
            "twin_schema": {
                "kind": "object_schema",
                "schema_id": "cooling-loop",
                "reducers": {
                    "total_power_kw": {
                        "agg": "Sum",
                        "relation": "CONTAINS",
                        "direction": "outgoing",
                        "target_interface_or_schema": "cooling-pump",
                        "source_variable": "power_kw",
                        "unit": "kW"
                    },
                    "avg_power_kw": {
                        "agg": "Avg",
                        "relation": "CONTAINS",
                        "direction": "outgoing",
                        "target_interface_or_schema": "cooling-pump",
                        "source_variable": "power_kw",
                        "unit": "kW"
                    },
                    "pump_count": {
                        "agg": "Count",
                        "relation": "CONTAINS",
                        "direction": "outgoing",
                        "target_interface_or_schema": "cooling-pump"
                    }
                }
            }
        }));
        loop_schema.memory_type = "digital_twin_schema".into();

        let mut pump_schema = make_payload(json!({
            "twin_schema": {
                "kind": "object_schema",
                "schema_id": "cooling-pump",
                "variables": {
                    "power_kw": { "min": 0.0, "max": 500.0, "unit": "kW" }
                }
            }
        }));
        pump_schema.memory_type = "digital_twin_schema".into();

        let loop_twin = make_payload(json!({
            "twin": {
                "id": "loop-1",
                "version": 1,
                "schema_id": "cooling-loop",
                "variables": { "flow_rate": 120.0 }
            }
        }));
        let pump1 = make_payload(json!({
            "twin": {
                "id": "pump-1",
                "version": 1,
                "schema_id": "cooling-pump",
                "variables": { "power_kw": 10.0 }
            }
        }));
        let pump2 = make_payload(json!({
            "twin": {
                "id": "pump-2",
                "version": 1,
                "schema_id": "cooling-pump",
                "variables": { "power_kw": 15.5 }
            }
        }));
        let pump3 = make_payload(json!({
            "twin": {
                "id": "pump-3",
                "version": 1,
                "schema_id": "cooling-pump",
                "variables": { "power_kw": 20.0 }
            }
        }));

        let rows = vec![
            ("s-loop".into(), loop_schema),
            ("s-pump".into(), pump_schema),
            ("r-loop".into(), loop_twin),
            ("r-p1".into(), pump1),
            ("r-p2".into(), pump2),
            ("r-p3".into(), pump3),
        ];
        let edges = vec![
            ("r-loop".into(), "CONTAINS".into(), "r-p1".into(), None),
            ("r-loop".into(), "CONTAINS".into(), "r-p2".into(), None),
            ("r-loop".into(), "CONTAINS".into(), "r-p3".into(), None),
        ];

        let state = get_twin_state_with_reducers_from_rows(&rows, &edges, "loop-1").unwrap();
        assert_eq!(state.derived_variables.get("total_power_kw"), Some(&45.5));
        assert_eq!(state.derived_variables.get("pump_count"), Some(&3.0));
        assert!((state.derived_variables["avg_power_kw"] - (45.5 / 3.0)).abs() < 1e-9);

        // Empty edges -> Avg returns 0.0 (never NaN).
        let empty_state = get_twin_state_with_reducers_from_rows(&rows, &[], "loop-1").unwrap();
        assert_eq!(
            empty_state.derived_variables.get("total_power_kw"),
            Some(&0.0)
        );
        assert_eq!(
            empty_state.derived_variables.get("avg_power_kw"),
            Some(&0.0)
        );
        assert_eq!(empty_state.derived_variables.get("pump_count"), Some(&0.0));
    }
}
