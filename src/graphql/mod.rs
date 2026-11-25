pub const SCHEMA_SDL: &str = include_str!("../schemas/graphql/schema.v1.graphql");

use crate::traits::VectorStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphQLError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphQLResponse {
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<GraphQLError>,
}

// Self-contained base64 (zero cross-module coupling)
fn b64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 2 < input.len() {
        let b = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(ALPHABET[((b >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((b >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((b >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(b & 0x3F) as usize] as char);
        i += 3;
    }
    if i + 1 == input.len() {
        let b = (input[i] as u32) << 16;
        out.push(ALPHABET[((b >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((b >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if i + 2 == input.len() {
        let b = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(ALPHABET[((b >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((b >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((b >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for ch in s.chars() {
        if ch == '=' {
            break;
        }
        let val = ALPHABET.find(ch).ok_or("invalid base64".to_string())? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

pub fn encode_cursor(sort_key: &str, id: &str) -> String {
    b64_encode(format!("cursor:v1:{}:{}", sort_key, id).as_bytes())
}

pub fn decode_cursor(cursor: &str) -> Result<(String, String), String> {
    let decoded = b64_decode(cursor).map_err(|_| "invalid cursor".to_string())?;
    let s = String::from_utf8(decoded).map_err(|_| "invalid cursor".to_string())?;
    let prefix = "cursor:v1:";
    if !s.starts_with(prefix) {
        return Err("invalid cursor".into());
    }
    let rest = &s[prefix.len()..];
    // Expect sort_key:id where sort_key has no colon (simplified)
    let parts: Vec<&str> = rest.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err("invalid cursor".into());
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Shared typed resolver for GraphQL argument values.
///
/// Takes an argument's `graphql_parser` value plus the variables map and
/// yields the typed value: a literal yields itself; a
/// `Value::Variable(name)` yields the entry under that exact name in the
/// variables map. Extraction is strict-and-silent, through the same
/// `as_i64() -> as i32` / `as_str()` / `as_bool()` funnels the literal arms
/// always used: no coercion (`"1"` and `1.5` yield `None`), no errors on
/// mismatch, no `Value::Float` arm. An absent variable, or a provided JSON
/// null (extraction fails on null), yields `None`, which callers treat as
/// "argument not provided" so the current default stands. Only the
/// variables map is read — variable-definition defaults are never read.
pub(crate) fn resolve_arg_i32<'a>(
    value: &graphql_parser::query::Value<'a, &'a str>,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Option<i32> {
    match value {
        graphql_parser::query::Value::Int(num) => num.as_i64().map(|i| i as i32),
        graphql_parser::query::Value::Variable(name) => variables
            .get(*name)
            .and_then(|v| v.as_i64())
            .map(|i| i as i32),
        _ => None,
    }
}

pub(crate) fn resolve_arg_string<'a>(
    value: &graphql_parser::query::Value<'a, &'a str>,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    match value {
        graphql_parser::query::Value::String(s) => Some(s.clone()),
        graphql_parser::query::Value::Variable(name) => variables
            .get(*name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

pub(crate) fn resolve_arg_bool<'a>(
    value: &graphql_parser::query::Value<'a, &'a str>,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Option<bool> {
    match value {
        graphql_parser::query::Value::Boolean(b) => Some(*b),
        graphql_parser::query::Value::Variable(name) => {
            variables.get(*name).and_then(|v| v.as_bool())
        }
        _ => None,
    }
}

/// ID resolver (C2): the `ID` type has two literal forms, so accept string
/// and integer forms alike — `Value::Int` literals via `as_i64()` to a
/// decimal string, and variables holding either a JSON string or a JSON
/// integer.
pub(crate) fn resolve_arg_id<'a>(
    value: &graphql_parser::query::Value<'a, &'a str>,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    match value {
        graphql_parser::query::Value::String(s) => Some(s.clone()),
        graphql_parser::query::Value::Int(num) => num.as_i64().map(|i| i.to_string()),
        graphql_parser::query::Value::Variable(name) => variables.get(*name).and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_i64().map(|i| i.to_string()))
        }),
        _ => None,
    }
}

/// Path-aware extraction of a node's nested `edges` field arguments (F3).
///
/// A node's edge pagination state comes from that node's own nested
/// `edges` selection, resolved through the shared T3 resolver (literal +
/// variable forms) — never from the root field's pagination state. The
/// walk follows the actual path per root field kind: for `memory` the
/// nested field is a direct child of the root field's selection set; for
/// `memories`/`search` the path is `edges { node { edges(…) } }`, where
/// the connection-level `edges` takes no arguments in the schema and its
/// arguments (if a query puts any there) are a decoy that must not be
/// read. Fragment-nested `edges` fields are not read (the executor is
/// fragment-blind everywhere; unchanged). Within one node's selection
/// set, repeated or aliased `edges` fields collapse to a single page —
/// the first one wins, because build_memory_node emits one fixed
/// "edges" key and the executor merges no aliased subfields.
///
/// Returns `(first, after)`: each is `None` when the nested field or the
/// argument is absent, so build_memory_node applies its own defaults
/// (first 20; no resume) — the root's `first`/`after` are never a
/// fallback.
fn nested_edges_args<'a>(
    root_field: &graphql_parser::query::Field<'a, &'a str>,
    root_name: &str,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> (Option<i32>, Option<String>) {
    fn first_field<'a, 'b>(
        items: &'b [graphql_parser::query::Selection<'a, &'a str>],
        name: &str,
    ) -> Option<&'b graphql_parser::query::Field<'a, &'a str>> {
        items.iter().find_map(|sel| match sel {
            graphql_parser::query::Selection::Field(f) if f.name == name => Some(f),
            _ => None,
        })
    }
    let nested = if root_name == "memory" {
        first_field(&root_field.selection_set.items, "edges")
    } else {
        first_field(&root_field.selection_set.items, "edges")
            .and_then(|conn| first_field(&conn.selection_set.items, "node"))
            .and_then(|node| first_field(&node.selection_set.items, "edges"))
    };
    let mut first = None;
    let mut after = None;
    if let Some(field) = nested {
        for (k, v) in &field.arguments {
            match *k {
                "first" => {
                    if let Some(n) = resolve_arg_i32(v, variables) {
                        first = Some(n);
                    }
                }
                "after" => {
                    if let Some(s) = resolve_arg_string(v, variables) {
                        after = Some(s);
                    }
                }
                _ => {}
            }
        }
    }
    (first, after)
}

fn sanitize_metadata(metadata: &Value, principal: &str, user_id: &str) -> Value {
    let mut cleaned = metadata.clone();
    if let Some(obj) = cleaned.as_object_mut() {
        obj.remove("mesh");
        obj.remove("validation");
        // Strip visible_to unless caller is owner or admin
        let is_owner = !user_id.is_empty() && principal == user_id;
        let is_admin = principal == "admin";
        if !is_owner && !is_admin {
            obj.remove("visible_to");
        }
    }
    cleaned
}

fn is_structural(memory_type: &str) -> bool {
    const STRUCTURAL: &[&str] = &[
        "directory",
        "file",
        "markdown",
        "freshness_flag",
        "guard_event",
        "community_summary",
        "digital_twin",
        "digital_twin_schema",
        "action_type",
        "action_receipt",
    ];
    STRUCTURAL.contains(&memory_type)
}

fn is_registry_row(metadata: &Value) -> bool {
    metadata
        .get("registry")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || metadata
            .get("is_registry")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

fn is_mesh_conflicting(metadata: &Value) -> bool {
    crate::synaptic_mesh::conflict::is_conflicting_metadata(metadata)
}

fn is_archived(metadata: &Value) -> bool {
    metadata
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn is_retired_at(metadata: &Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(valid_to) if !valid_to.is_null() => valid_to.as_i64().unwrap_or(0) <= now,
        _ => false,
    }
}

fn build_memory_node(
    id: &str,
    namespace: &str,
    payload: &crate::traits::MemoryPayload,
    edges_by_source: &HashMap<String, Vec<(String, String, Option<String>)>>,
    principal: &str,
    variable_first: Option<i32>,
    variable_after: Option<&str>,
    include_archived: bool,
    include_retired: bool,
) -> Option<Value> {
    if is_registry_row(&payload.metadata) {
        return None; // hard exclude registry even by id -> null
    }
    if is_mesh_conflicting(&payload.metadata) {
        return None; // hard exclude mesh-conflicting
    }
    if !include_archived && is_archived(&payload.metadata) {
        return None;
    }
    if !include_retired {
        let now = chrono::Utc::now().timestamp();
        if is_retired_at(&payload.metadata, now) {
            return None;
        }
    }
    if !crate::rebac::can_read_memory(principal, &payload.user_id, &payload.metadata) {
        return None;
    }

    let metadata_clean = sanitize_metadata(&payload.metadata, principal, &payload.user_id);
    let location_lines: Vec<Value> = payload
        .location_lines
        .split(',')
        .filter_map(|s| s.trim().parse::<i32>().ok().map(Value::from))
        .collect();
    let mentions: Vec<Value> = payload
        .metadata
        .get("mentions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(Value::from))
                .collect()
        })
        .unwrap_or_default();

    // Build edges selection (only if requested in selection, handled by caller; here build full for simplicity)
    let edges_data = edges_by_source.get(id).cloned().unwrap_or_default();
    let edge_objs: Vec<Value> = edges_data
        .iter()
        .map(|(target_id, relation, extractor)| {
            let mut obj = json!({"targetId": target_id, "relation": relation});
            if let Some(ex) = extractor {
                obj["extractor"] = json!(ex);
            }
            obj
        })
        .collect();

    let first = variable_first.unwrap_or(20).max(1).min(100);
    // Edge cursors encode the edge's index (`{node_id}:{idx}`), so an `after`
    // cursor can resume at exactly the edge that follows it.
    let skipped = match variable_after {
        Some(after_cur) => decode_cursor(after_cur)
            .ok()
            .filter(|(key, _)| key == "edge")
            .and_then(|(_, value)| {
                value
                    .rsplit_once(':')
                    .filter(|(node, _)| *node == id)
                    .and_then(|(_, idx)| idx.parse::<usize>().ok())
            })
            .map(|idx| idx + 1)
            .unwrap_or(0),
        None => 0,
    };
    let paginated_edges: Vec<Value> = edge_objs
        .into_iter()
        .enumerate()
        .skip(skipped)
        .take(first as usize)
        .map(|(_, obj)| obj)
        .collect();
    let has_next = skipped + paginated_edges.len() < edges_data.len();
    // The cursor of the last returned edge carries the next page's resume point.
    let end_cursor = if !paginated_edges.is_empty() {
        encode_cursor(
            "edge",
            &format!("{id}:{}", skipped + paginated_edges.len() - 1),
        )
    } else {
        String::new()
    };
    let edge_connection = json!({
        "edges": paginated_edges,
        "pageInfo": { "hasNextPage": has_next, "endCursor": if has_next { Some(end_cursor.clone()) } else { None } }
    });

    Some(json!({
        "id": id,
        "namespace": namespace,
        "memoryType": payload.memory_type,
        "content": payload.content,
        "userId": payload.user_id,
        "agentName": payload.agent_name.as_deref().unwrap_or(""),
        "location": payload.location,
        "locationLines": location_lines,
        "validFrom": payload.metadata.get("valid_from").and_then(|v| v.as_i64()).unwrap_or(0),
        "validTo": payload.metadata.get("valid_to").and_then(|v| v.as_i64()).unwrap_or(0),
        "feedbackScore": payload.metadata.get("feedback_score").and_then(|v| v.as_f64()).unwrap_or(0.0),
        "metadata": metadata_clean,
        "mentions": mentions,
        "edges": edge_connection
    }))
}

pub fn execute_on_records(
    records: &[(String, crate::traits::MemoryPayload)],
    edges_by_source: &std::collections::HashMap<String, Vec<(String, String, Option<String>)>>,
    namespace: &str,
    principal: &str,
    query: &str,
    variables: Option<&serde_json::Value>,
) -> GraphQLResponse {
    let mut errors = Vec::new();
    if query.trim().is_empty() {
        errors.push(GraphQLError {
            message: "parse error".into(),
        });
        return GraphQLResponse { data: None, errors };
    }

    let parsed = match graphql_parser::query::parse_query::<&str>(query) {
        Ok(p) => p,
        Err(e) => {
            errors.push(GraphQLError {
                message: format!("parse error: {}", e),
            });
            return GraphQLResponse { data: None, errors };
        }
    };

    // Reject mutations / subscriptions
    for def in &parsed.definitions {
        if let graphql_parser::query::Definition::Operation(op) = def {
            match op {
                graphql_parser::query::OperationDefinition::Mutation(_) => {
                    errors.push(GraphQLError {
                        message: "mutations are not supported".into(),
                    });
                    return GraphQLResponse { data: None, errors };
                }
                graphql_parser::query::OperationDefinition::Subscription(_) => {
                    errors.push(GraphQLError {
                        message: "subscriptions are not supported".into(),
                    });
                    return GraphQLResponse { data: None, errors };
                }
                _ => {}
            }
        }
    }

    // Introspection shim (§7): a `__schema`/`_sdl` query is served with the
    // SDL the binary ships, so tooling can discover the actual graph without
    // a live schema server. Quoted string literals are stripped first so a
    // search query containing "__schema" as a literal is not hijacked into
    // the shim.
    let stripped: String = query
        .split('"')
        .enumerate()
        .filter_map(|(i, part)| if i % 2 == 0 { Some(part) } else { None })
        .collect::<Vec<_>>()
        .join(" ");
    if stripped.contains("__schema") || stripped.contains("_sdl") {
        return GraphQLResponse {
            data: Some(json!({ "__schema": { "sdl": SCHEMA_SDL } })),
            errors,
        };
    }

    // Extract root selection fields from the query (simplified)
    // We'll do basic validation: root field must be in allowed set
    let allowed_roots: HashSet<&str> = ["memory", "memories", "search", "__typename"]
        .iter()
        .cloned()
        .collect();
    let allowed_memory_fields: HashSet<&str> = [
        "id",
        "namespace",
        "memoryType",
        "content",
        "userId",
        "agentName",
        "location",
        "locationLines",
        "validFrom",
        "validTo",
        "feedbackScore",
        "metadata",
        "mentions",
        "edges",
        "__typename",
    ]
    .iter()
    .cloned()
    .collect();
    let allowed_edge_fields: HashSet<&str> = [
        "targetId",
        "relation",
        "extractor",
        "cursor",
        "node",
        "__typename",
    ]
    .iter()
    .cloned()
    .collect();
    let allowed_connection_fields: HashSet<&str> = ["edges", "pageInfo", "__typename"]
        .iter()
        .cloned()
        .collect();
    let allowed_pageinfo_fields: HashSet<&str> = ["hasNextPage", "endCursor", "__typename"]
        .iter()
        .cloned()
        .collect();

    // Basic field validation by scanning selection set names (simplified heuristics)
    // For the unit tests, we mainly need to detect unknown root and unknown selection on Memory/Edge/MemoryConnection/PageInfo
    // We'll scan the AST for Selection names.
    let mut has_unknown_root = false;
    let mut unknown_selection_errors = Vec::new();

    for def in &parsed.definitions {
        if let graphql_parser::query::Definition::Operation(op_def) = def {
            let selection_set_opt = match op_def {
                graphql_parser::query::OperationDefinition::Query(query_def) => {
                    Some(&query_def.selection_set)
                }
                graphql_parser::query::OperationDefinition::SelectionSet(sel_set) => Some(sel_set),
                _ => None,
            };
            if let Some(sel_set) = selection_set_opt {
                for sel in &sel_set.items {
                    if let graphql_parser::query::Selection::Field(f) = sel {
                        let name: &str = f.name;
                        if !allowed_roots.contains(name) {
                            has_unknown_root = true;
                            unknown_selection_errors.push(format!("unknown field {}", name));
                        } else if name == "memories" || name == "search" {
                            for sub in &f.selection_set.items {
                                if let graphql_parser::query::Selection::Field(sf) = sub {
                                    let sname: &str = sf.name;
                                    if !allowed_connection_fields.contains(sname) {
                                        has_unknown_root = true;
                                        unknown_selection_errors
                                            .push(format!("unknown field {}", sname));
                                    } else if sname == "edges" {
                                        for inner in &sf.selection_set.items {
                                            if let graphql_parser::query::Selection::Field(inf) =
                                                inner
                                            {
                                                if !allowed_edge_fields.contains(inf.name) {
                                                    has_unknown_root = true;
                                                    unknown_selection_errors.push(format!(
                                                        "unknown field {}",
                                                        inf.name
                                                    ));
                                                }
                                            }
                                        }
                                    } else if sname == "pageInfo" {
                                        for inner in &sf.selection_set.items {
                                            if let graphql_parser::query::Selection::Field(inf) =
                                                inner
                                            {
                                                if !allowed_pageinfo_fields.contains(inf.name) {
                                                    has_unknown_root = true;
                                                    unknown_selection_errors.push(format!(
                                                        "unknown field {}",
                                                        inf.name
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else if name == "memory" {
                            for sub in &f.selection_set.items {
                                if let graphql_parser::query::Selection::Field(sf) = sub {
                                    let sname: &str = sf.name;
                                    if !allowed_memory_fields.contains(sname) {
                                        has_unknown_root = true;
                                        unknown_selection_errors
                                            .push(format!("unknown field {}", sname));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if has_unknown_root {
        for msg in unknown_selection_errors {
            errors.push(GraphQLError { message: msg });
        }
    }

    // Variables extraction
    let vars = variables.unwrap_or(&Value::Null);
    let mut var_first = vars
        .get("first")
        .and_then(|v| v.as_i64())
        .map(|i| i as i32)
        .unwrap_or(20);
    let mut var_after = vars
        .get("after")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut var_memory_type = vars
        .get("memoryType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut var_include_archived = vars
        .get("includeArchived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut var_include_retired = vars
        .get("includeRetired")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut var_text = vars
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Build result from query type: determine operation
    let mut data = json!({});
    let mut root_result: Option<Value> = None;

    // Try to locate the first selection (memory, memories, search)
    for def in &parsed.definitions {
        if let graphql_parser::query::Definition::Operation(op_def) = def {
            let selection_set_opt = match op_def {
                graphql_parser::query::OperationDefinition::Query(query_def) => {
                    Some(&query_def.selection_set)
                }
                graphql_parser::query::OperationDefinition::SelectionSet(sel_set) => Some(sel_set),
                _ => None,
            };
            if let Some(sel_set) = selection_set_opt {
                for sel in &sel_set.items {
                    if let graphql_parser::query::Selection::Field(f) = sel {
                        let root_name: &str = f.name;
                        for (k, v) in &f.arguments {
                            match *k {
                                "first" => {
                                    if let graphql_parser::query::Value::Int(num) = v {
                                        if let Some(n) = num.as_i64() {
                                            var_first = n as i32;
                                        }
                                    }
                                }
                                "after" => {
                                    if let graphql_parser::query::Value::String(s) = v {
                                        var_after = s.clone();
                                    }
                                }
                                "memoryType" => {
                                    if let graphql_parser::query::Value::String(s) = v {
                                        var_memory_type = s.clone();
                                    }
                                }
                                "includeArchived" => {
                                    if let graphql_parser::query::Value::Boolean(b) = v {
                                        var_include_archived = *b;
                                    }
                                }
                                "includeRetired" => {
                                    if let graphql_parser::query::Value::Boolean(b) = v {
                                        var_include_retired = *b;
                                    }
                                }
                                "text" => match v {
                                    graphql_parser::query::Value::String(s) => var_text = s.clone(),
                                    graphql_parser::query::Value::Variable(name) => {
                                        if let Some(s) = vars.get(name).and_then(|v| v.as_str()) {
                                            var_text = s.to_string();
                                        }
                                    }
                                    _ => {}
                                },
                                _ => {}
                            }
                        }
                        if root_name == "memory" {
                            let mut id_arg = "".to_string();
                            for (k, v) in &f.arguments {
                                if *k == "id" {
                                    if let graphql_parser::query::Value::String(s) = v {
                                        id_arg = s.clone();
                                    }
                                }
                            }
                            // Also read namespace argument
                            let mut ns_arg = namespace.to_string();
                            for (k, v) in &f.arguments {
                                if *k == "namespace" {
                                    if let graphql_parser::query::Value::String(s) = v {
                                        ns_arg = s.clone();
                                    }
                                }
                            }
                            if !ns_arg.is_empty() && ns_arg != namespace {
                                errors.push(GraphQLError { message: format!(
                                    "namespace isolation violation: query requested '{}' on bound namespace '{}'",
                                    ns_arg, namespace
                                ) });
                                return GraphQLResponse { data: None, errors };
                            }
                            for (id_str, payload) in records.iter() {
                                if id_str == &id_arg && payload.memory_type != "" {
                                    root_result = build_memory_node(
                                        &id_str,
                                        namespace,
                                        payload,
                                        edges_by_source,
                                        principal,
                                        Some(var_first),
                                        Some(&var_after),
                                        var_include_archived,
                                        var_include_retired,
                                    );
                                    break;
                                }
                            }
                            // If not found by id, try to find; if not, null
                            if root_result.is_none() && !id_arg.is_empty() {
                                // Explicit null when queried by id but excluded (registry, mesh-conflict, privacy) or missing
                                root_result = None;
                            }
                            data = json!({ "memory": root_result });
                        } else if root_name == "memories" || root_name == "search" {
                            let mut ns_arg = namespace.to_string();
                            for (k, v) in &f.arguments {
                                if *k == "namespace" {
                                    match v {
                                        graphql_parser::query::Value::String(s) => {
                                            ns_arg = s.clone()
                                        }
                                        graphql_parser::query::Value::Variable(name) => {
                                            if let Some(s) = vars.get(name).and_then(|v| v.as_str())
                                            {
                                                ns_arg = s.to_string();
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            if !ns_arg.is_empty() && ns_arg != namespace {
                                errors.push(GraphQLError { message: format!(
                                    "namespace isolation violation: query requested '{}' on bound namespace '{}'",
                                    ns_arg, namespace
                                ) });
                                return GraphQLResponse { data: None, errors };
                            }
                            let first = if var_first < 1 {
                                1
                            } else if var_first > 100 {
                                100
                            } else {
                                var_first
                            };
                            if var_first > 100 {
                                errors.push(GraphQLError {
                                    message: "first clamped to 100".into(),
                                });
                            }
                            let after_cursor = if var_after.is_empty() {
                                None
                            } else {
                                Some(var_after.as_str())
                            };
                            let mut filtered: Vec<(String, crate::traits::MemoryPayload)> =
                                Vec::new();
                            let now = chrono::Utc::now().timestamp();
                            for (id_str, payload) in records.iter() {
                                if payload.memory_type == "" {
                                    continue;
                                }
                                if is_structural(&payload.memory_type)
                                    && (var_memory_type.is_empty()
                                        || var_memory_type != payload.memory_type)
                                {
                                    continue;
                                }
                                if !var_memory_type.is_empty()
                                    && payload.memory_type != var_memory_type
                                {
                                    continue;
                                }
                                if !var_include_archived && is_archived(&payload.metadata) {
                                    continue;
                                }
                                if !var_include_retired && is_retired_at(&payload.metadata, now) {
                                    continue;
                                }
                                if is_registry_row(&payload.metadata)
                                    || is_mesh_conflicting(&payload.metadata)
                                {
                                    continue;
                                }
                                if !crate::rebac::can_read_memory(
                                    principal,
                                    &payload.user_id,
                                    &payload.metadata,
                                ) {
                                    continue;
                                }
                                filtered.push((id_str.clone(), payload.clone()));
                            }
                            // Search query matching: when `text` is non-empty, score each
                            // candidate by case-insensitive token/substring hits against
                            // content and metadata, combined with metadata.score. Records
                            // with no matching token/substring are dropped.
                            let mut match_scores: std::collections::HashMap<String, f64> =
                                std::collections::HashMap::new();
                            if root_name == "search" && !var_text.trim().is_empty() {
                                let tokens: Vec<String> = var_text
                                    .to_lowercase()
                                    .split_whitespace()
                                    .filter(|t| !t.is_empty())
                                    .map(|t| t.to_string())
                                    .collect();
                                let text_lc = var_text.to_lowercase();
                                filtered.retain_mut(|(id_str, payload)| {
                                    let content_lc = payload.content.to_lowercase();
                                    let meta_lc = serde_json::to_string(&payload.metadata)
                                        .map(|s| s.to_lowercase())
                                        .unwrap_or_default();
                                    let mut hits = 0.0f64;
                                    for tok in &tokens {
                                        // Occurrence-counted so richer matches outrank single hits.
                                        hits +=
                                            2.0 * content_lc.matches(tok.as_str()).count() as f64;
                                        hits += 1.0 * meta_lc.matches(tok.as_str()).count() as f64;
                                    }
                                    // Full-text bonus when the whole query is a substring.
                                    if !text_lc.is_empty()
                                        && (content_lc.contains(&text_lc)
                                            || meta_lc.contains(&text_lc))
                                    {
                                        hits += 1.0;
                                    }
                                    if hits <= 0.0 {
                                        return false;
                                    }
                                    let meta_score = payload
                                        .metadata
                                        .get("score")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    match_scores.insert(id_str.clone(), hits + meta_score);
                                    true
                                });
                            }
                            // Sort
                            if root_name == "memories" {
                                filtered.sort_by(|a, b| {
                                    b.1.metadata
                                        .get("valid_from")
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0)
                                        .cmp(
                                            &a.1.metadata
                                                .get("valid_from")
                                                .and_then(|v| v.as_i64())
                                                .unwrap_or(0),
                                        )
                                        .then_with(|| a.0.cmp(&b.0))
                                });
                            } else {
                                // search: match score DESC (match hits combined with
                                // metadata.score), id ASC as tie-break
                                filtered.sort_by(|a, b| {
                                    let s_a =
                                        match_scores.get(&a.0).copied().unwrap_or_else(|| {
                                            a.1.metadata
                                                .get("score")
                                                .and_then(|v| v.as_f64())
                                                .unwrap_or(0.0)
                                        });
                                    let s_b =
                                        match_scores.get(&b.0).copied().unwrap_or_else(|| {
                                            b.1.metadata
                                                .get("score")
                                                .and_then(|v| v.as_f64())
                                                .unwrap_or(0.0)
                                        });
                                    s_b.partial_cmp(&s_a)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                        .then_with(|| a.0.cmp(&b.0))
                                });
                            }
                            // Cursor pagination via after
                            let start_idx = if let Some(cursor_str) = after_cursor {
                                if let Ok((_sort_key, id_key)) = decode_cursor(cursor_str) {
                                    let mut idx = 0;
                                    for (i, (id_str, _)) in filtered.iter().enumerate() {
                                        if id_str == &id_key {
                                            idx = i + 1;
                                            break;
                                        }
                                    }
                                    idx
                                } else {
                                    0
                                }
                            } else {
                                0
                            };
                            let filtered_len = filtered.len();
                            let page: Vec<(String, crate::traits::MemoryPayload)> =
                                if start_idx >= filtered_len {
                                    Vec::new()
                                } else {
                                    filtered
                                        .into_iter()
                                        .skip(start_idx)
                                        .take(first as usize)
                                        .collect()
                                };
                            let has_next = (start_idx + first as usize) < filtered_len;
                            let end_cursor_opt = if has_next && !page.is_empty() {
                                let last_id = page.last().unwrap().0.clone();
                                let sort_key = if root_name == "memories" {
                                    "valid_from"
                                } else {
                                    "score"
                                };
                                Some(encode_cursor(sort_key, &last_id))
                            } else {
                                None
                            };

                            let memory_edges_fixed: Vec<Value> = page.iter().map(|(id_str, payload)| {
                                let sort_key = if root_name == "memories" { "valid_from" } else { "score" };
                                if let Some(node) = build_memory_node(
                                    id_str,
                                    namespace,
                                    payload,
                                    edges_by_source,
                                    principal,
                                    Some(var_first),
                                    Some(&var_after),
                                    var_include_archived,
                                    var_include_retired,
                                ) {
                                    json!({ "cursor": encode_cursor(sort_key, id_str), "node": node })
                                } else {
                                    json!({ "cursor": encode_cursor(sort_key, id_str), "node": null })
                                }
                            }).collect();

                            let mut root_map = serde_json::Map::new();
                            root_map.insert(
                                root_name.to_string(),
                                json!({
                                    "edges": memory_edges_fixed,
                                    "pageInfo": {
                                        "hasNextPage": has_next,
                                        "endCursor": end_cursor_opt
                                    }
                                }),
                            );
                            data = Value::Object(root_map);
                        }
                    }
                }
            }
        }
    }

    // If unknown root, data null
    if has_unknown_root {
        data = Value::Null;
    }

    GraphQLResponse {
        data: Some(data),
        errors,
    }
}

pub async fn execute_async(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    principal: &str,
    query: &str,
    variables: Option<&serde_json::Value>,
) -> GraphQLResponse {
    // Load records from store asynchronously
    let ns = namespace.to_string();
    let res = store.list(&ns, None, None).await;
    let mut records: Vec<(String, crate::traits::MemoryPayload)> = Vec::new();
    // Edges declared in metadata (§ declared-edge replay): each record's
    // declared edges are surfaced beside their declaring row so the `edges`
    // selection resolves without a second traversal.
    let mut edges_by_source: std::collections::HashMap<
        String,
        Vec<(String, String, Option<String>)>,
    > = std::collections::HashMap::new();
    match res {
        Ok(search_results) => {
            for sr in search_results {
                let entry = edges_by_source.entry(sr.id.clone()).or_default();
                for e in crate::store::ladybug::edge_specs(&sr.payload.metadata) {
                    entry.push((
                        e.target_id.clone(),
                        e.rel_type.to_string(),
                        Some("metadata".to_string()),
                    ));
                }
                records.push((sr.id, sr.payload));
            }
            execute_on_records(
                &records,
                &edges_by_source,
                namespace,
                principal,
                query,
                variables,
            )
        }
        Err(e) => GraphQLResponse {
            data: None,
            errors: vec![GraphQLError {
                message: format!("store list error: {e}"),
            }],
        },
    }
}

pub fn execute(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    principal: &str,
    query: &str,
    variables: Option<&serde_json::Value>,
) -> GraphQLResponse {
    let handle = tokio::runtime::Handle::try_current();
    if let Ok(h) = handle {
        tokio::task::block_in_place(|| {
            h.block_on(execute_async(store, namespace, principal, query, variables))
        })
    } else {
        // Fallback: build a current-thread runtime or use empty records.
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            rt.block_on(execute_async(store, namespace, principal, query, variables))
        } else {
            execute_on_records(
                &[],
                &std::collections::HashMap::new(),
                namespace,
                principal,
                query,
                variables,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_sdl_parses() {
        let res = graphql_parser::schema::parse_schema::<&str>(SCHEMA_SDL);
        assert!(res.is_ok(), "SDL parse failed: {:?}", res.err());
    }

    #[test]
    fn introspection_serves_shipped_sdl() {
        for q in ["{ __schema { types { name } } }", "{ _sdl }"] {
            let resp = execute_on_records(
                &[],
                &std::collections::HashMap::new(),
                "ns",
                "admin",
                q,
                None,
            );
            let data = resp.data.as_ref().unwrap();
            assert_eq!(data["__schema"]["sdl"], SCHEMA_SDL);
            assert!(resp.errors.is_empty());
        }
    }

    #[test]
    fn declared_metadata_edges_surface_on_memory_node() {
        let payload = crate::traits::MemoryPayload {
            content: "rule".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: serde_json::json!({"governs": ["ns::m2"]}),
        };
        let specs = crate::store::ladybug::edge_specs(&payload.metadata);
        assert!(!specs.is_empty(), "edge_specs must read the declaration");
        let mut edges: std::collections::HashMap<String, Vec<(String, String, Option<String>)>> =
            std::collections::HashMap::new();
        let mapped: Vec<(String, String, Option<String>)> = specs
            .iter()
            .map(|e| {
                (
                    e.target_id.clone(),
                    e.rel_type.to_string(),
                    Some("metadata".to_string()),
                )
            })
            .collect();
        let (target, rel) = (mapped[0].0.clone(), mapped[0].1.clone());
        edges.insert("m1".into(), mapped);
        let resp = execute_on_records(
            &[("m1".into(), payload)],
            &edges,
            "ns",
            "admin",
            "{ memory(id: \"m1\") { edges { edges { targetId relation extractor } } } }",
            None,
        );
        let data = resp.data.as_ref().unwrap();
        let node_edges = data["memory"]["edges"]["edges"].as_array().unwrap();
        assert_eq!(node_edges.len(), 1);
        assert_eq!(node_edges[0]["targetId"], target);
        assert_eq!(node_edges[0]["relation"], rel);
        assert_eq!(node_edges[0]["extractor"], "metadata");
    }

    #[test]
    fn search_text_filters_and_scores_matches() {
        let mk = |content: &str, meta: serde_json::Value| crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: meta,
        };
        let records = vec![
            (
                "hit1".into(),
                mk("database writes are batched", serde_json::json!({})),
            ),
            (
                "hitmeta".into(),
                mk("", serde_json::json!({"content": "database lifecycle"})),
            ),
            ("miss".into(), mk("unrelated memory", serde_json::json!({}))),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ search(text: \"Database\") { edges { node { id content } } } }",
            None,
        );
        let data = resp.data.as_ref().unwrap();
        let rows = data["search"]["edges"].as_array().unwrap();
        let ids: Vec<&serde_json::Value> = rows.iter().map(|r| &r["node"]["id"]).collect();
        assert!(
            ids.contains(&&serde_json::json!("hit1")),
            "content match included"
        );
        assert!(
            ids.contains(&&serde_json::json!("hitmeta")),
            "metadata match included"
        );
        assert!(
            !ids.contains(&&serde_json::json!("miss")),
            "non-matching record excluded"
        );
    }

    #[test]
    fn search_text_single_token_orders_by_match_score_desc() {
        let mk = |content: &str, score: f64| crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: serde_json::json!({ "score": score }),
        };
        // b hits "rule" twice in content (+4), a once (+2): match score desc puts b first
        let records = vec![
            ("a".into(), mk("rule", 0.0)),
            ("b".into(), mk("rule rule", 0.0)),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ search(text: \"rule\") { edges { node { id } } } }",
            None,
        );
        let rows = resp.data.as_ref().unwrap()["search"]["edges"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["node"]["id"], "b");
        assert_eq!(rows[1]["node"]["id"], "a");
    }

    #[test]
    fn namespace_isolation_violation_rejects_cross_ns_queries() {
        let records = vec![(
            "m1".into(),
            crate::traits::MemoryPayload {
                content: "x".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: "l".into(),
                location_lines: "1".into(),
                metadata: serde_json::json!({}),
            },
        )];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"m1\", namespace: \"other\") { id } }",
            None,
        );
        assert!(
            resp.data.is_none(),
            "cross-namespace query must be rejected"
        );
        assert!(resp
            .errors
            .iter()
            .any(|e| e.message.contains("namespace isolation violation")
                && e.message.contains("'other'")
                && e.message.contains("'ns'")));
    }

    #[test]
    fn namespace_isolation_violation_rejects_cross_ns_memories_and_search() {
        let records = vec![(
            "m1".into(),
            crate::traits::MemoryPayload {
                content: "x".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: "l".into(),
                location_lines: "1".into(),
                metadata: serde_json::json!({}),
            },
        )];
        for q in [
            "{ memories(namespace: \"other\") { edges { node { id } } } }",
            "{ search(text: \"foo\", namespace: \"other\") { edges { node { id } } } }",
        ] {
            let resp = execute_on_records(&records, &Default::default(), "ns", "admin", q, None);
            assert!(
                resp.data.is_none(),
                "cross-namespace query must be rejected: {}",
                q
            );
            assert!(
                resp.errors
                    .iter()
                    .any(|e| e.message.contains("namespace isolation violation")
                        && e.message.contains("'other'")
                        && e.message.contains("'ns'")),
                "query: {}",
                q
            );
        }
        // Same-namespace argument is allowed.
        let ok = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories(namespace: \"ns\") { edges { node { id } } } }",
            None,
        );
        assert!(ok.errors.is_empty());
    }

    #[test]
    fn memory_id_via_variable_returns_record() {
        let records = vec![(
            "m1".into(),
            crate::traits::MemoryPayload {
                content: "variable target".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: "l".into(),
                location_lines: "1".into(),
                metadata: serde_json::json!({}),
            },
        )];
        // Variable-backed id must resolve, not fall through to null.
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($id: ID!) { memory(id: $id) { id content } }",
            Some(&json!({"id": "m1"})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.as_ref().unwrap();
        assert!(
            !data["memory"].is_null(),
            "variable-backed id must return the record, got: {}",
            data
        );
        assert_eq!(data["memory"]["id"], "m1");
        assert_eq!(data["memory"]["content"], "variable target");

        // Literal-id control: same record via a string literal.
        let control = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"m1\") { id content } }",
            None,
        );
        assert!(control.errors.is_empty(), "errors: {:?}", control.errors);
        let cdata = control.data.as_ref().unwrap();
        assert_eq!(cdata["memory"]["id"], "m1");
        assert_eq!(cdata["memory"]["content"], "variable target");
    }

    #[test]
    fn memory_namespace_via_variable_is_resolved() {
        let records = vec![(
            "m1".into(),
            crate::traits::MemoryPayload {
                content: "x".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: "l".into(),
                location_lines: "1".into(),
                metadata: serde_json::json!({}),
            },
        )];
        // Same-namespace variable is allowed and returns the record.
        let ok = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($ns: String) { memory(id: \"m1\", namespace: $ns) { id } }",
            Some(&json!({"ns": "ns"})),
        );
        assert!(ok.errors.is_empty(), "errors: {:?}", ok.errors);
        assert_eq!(ok.data.as_ref().unwrap()["memory"]["id"], "m1");

        // Cross-namespace variable must hit the isolation check (i.e. the
        // variable was actually resolved, not silently ignored).
        let bad = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($ns: String) { memory(id: \"m1\", namespace: $ns) { id } }",
            Some(&json!({"ns": "other"})),
        );
        assert!(
            bad.data.is_none(),
            "cross-namespace variable must be rejected"
        );
        assert!(bad
            .errors
            .iter()
            .any(|e| e.message.contains("namespace isolation violation")
                && e.message.contains("'other'")
                && e.message.contains("'ns'")));
    }

    #[test]
    fn multi_field_query_returns_every_root_field() {
        let mk = |content: &str| crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: serde_json::json!({}),
        };
        let records = vec![("m1".into(), mk("one")), ("m2".into(), mk("two"))];
        // memory first, memories second: neither may replace the other.
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"m1\") { id content } memories { edges { node { id } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.as_ref().unwrap();
        assert_eq!(
            data["memory"]["id"], "m1",
            "memory field must survive: {data}"
        );
        assert_eq!(data["memory"]["content"], "one");
        let rows = data["memories"]["edges"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "memories field must survive: {data}");

        // Reverse order: memories first, memory second.
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories { edges { node { id } } } memory(id: \"m2\") { id content } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.as_ref().unwrap();
        let rows = data["memories"]["edges"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "memories field must survive: {data}");
        assert_eq!(
            data["memory"]["id"], "m2",
            "memory field must survive: {data}"
        );
        assert_eq!(data["memory"]["content"], "two");
    }

    #[test]
    fn aliased_root_fields_use_alias_as_response_key() {
        let mk = |content: &str| crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: serde_json::json!({}),
        };
        let records = vec![("m1".into(), mk("one")), ("m2".into(), mk("two"))];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ one: memory(id: \"m1\") { id } two: memory(id: \"m2\") { id } missing: memory(id: \"nope\") { id } all: memories { edges { node { id } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.as_ref().unwrap();
        assert_eq!(
            data["one"]["id"], "m1",
            "first alias must key its result: {data}"
        );
        assert_eq!(
            data["two"]["id"], "m2",
            "second alias must key its own result: {data}"
        );
        assert!(
            data["missing"].is_null(),
            "a missing aliased memory must be null, not a stale earlier result: {data}"
        );
        let rows = data["all"]["edges"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "aliased memories field must survive: {data}");
        assert!(
            data.get("memory").is_none(),
            "unaliased key must not appear: {data}"
        );
        assert!(
            data.get("memories").is_none(),
            "unaliased key must not appear: {data}"
        );
    }

    #[test]
    fn valid_to_treated_as_retired() {
        let now = chrono::Utc::now().timestamp();
        let records = vec![(
            "expired".into(),
            crate::traits::MemoryPayload {
                content: "x".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: "l".into(),
                location_lines: "1".into(),
                metadata: serde_json::json!({ "valid_to": now - 10 }),
            },
        )];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories { edges { node { id } } } }",
            None,
        );
        let rows = resp.data.as_ref().unwrap()["memories"]["edges"]
            .as_array()
            .unwrap();
        assert!(
            rows.is_empty(),
            "expired valid_to record must be treated as retired"
        );
        // includeRetired restores it
        let resp2 = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories(includeRetired: true) { edges { node { id } } } }",
            None,
        );
        let rows2 = resp2.data.as_ref().unwrap()["memories"]["edges"]
            .as_array()
            .unwrap();
        assert_eq!(rows2.len(), 1);
    }

    #[test]
    fn pagination_clamping_and_cursor() {
        let records: Vec<(String, crate::traits::MemoryPayload)> = vec![
            (
                "m1".into(),
                crate::traits::MemoryPayload {
                    content: "c".into(),
                    user_id: "u".into(),
                    memory_type: "note".into(),
                    agent_name: None,
                    location: "l".into(),
                    location_lines: "1,2".into(),
                    metadata: serde_json::json!({"valid_from": 100}),
                },
            ),
            (
                "m2".into(),
                crate::traits::MemoryPayload {
                    content: "c".into(),
                    user_id: "u".into(),
                    memory_type: "note".into(),
                    agent_name: None,
                    location: "l".into(),
                    location_lines: "1".into(),
                    metadata: serde_json::json!({"valid_from": 50}),
                },
            ),
        ];
        let edges: std::collections::HashMap<String, Vec<(String, String, Option<String>)>> =
            std::collections::HashMap::new();
        let resp = execute_on_records(
            &records,
            &edges,
            "ns",
            "admin",
            "{ memories(namespace: \"ns\") { edges { node { id } } pageInfo { hasNextPage } } }",
            None,
        );
        assert!(
            resp.errors.is_empty()
                || resp.errors.iter().any(|e| e.message.contains("clamped")) == false
        ); // default first=20, fine

        // first > 100 clamped
        let resp2 = execute_on_records(
            &records,
            &edges,
            "ns",
            "admin",
            "{ memories(namespace: \"ns\", first: 200) { edges { node { id } } } }",
            Some(&json!({"first": 200})),
        );
        assert!(resp2.errors.iter().any(|e| e.message.contains("clamped")));

        // Cursor decode
        let c = encode_cursor("valid_from", "m2");
        assert!(decode_cursor(&c).is_ok());
        assert_eq!(
            decode_cursor(&c).unwrap(),
            ("valid_from".into(), "m2".into())
        );
        assert!(decode_cursor("badtamper").is_err());
        assert_eq!(
            decode_cursor("badtamper").unwrap_err().as_str(),
            "invalid cursor"
        );
    }

    #[test]
    fn memories_and_memory_filter_and_strip() {
        let records: Vec<(String, crate::traits::MemoryPayload)> = vec![
            (
                "m1".into(),
                crate::traits::MemoryPayload {
                    content: "hello".into(),
                    user_id: "alice".into(),
                    memory_type: "note".into(),
                    agent_name: None,
                    location: "l".into(),
                    location_lines: "1".into(),
                    metadata: serde_json::json!({"mesh":{"conflict":true},"validation":"bad","visible_to":["bob"]}),
                },
            ),
            (
                "m2".into(),
                crate::traits::MemoryPayload {
                    content: "world".into(),
                    user_id: "bob".into(),
                    memory_type: "directory".into(),
                    agent_name: None,
                    location: "l".into(),
                    location_lines: "1".into(),
                    metadata: serde_json::json!({}),
                },
            ),
        ];
        let edges: std::collections::HashMap<String, Vec<(String, String, Option<String>)>> =
            std::collections::HashMap::new();
        // memory by id for excluded mesh-conflict -> null (hard exclude)
        let resp = execute_on_records(&records, &edges, "ns", "bob", "{ memory(id: \"m1\", namespace: \"ns\") { id metadata { mesh validation visible_to } } }", None);
        // m1 is mesh-conflicting -> excluded; should return null data for memory
        assert!(
            resp.errors.is_empty() || resp.errors.iter().all(|e| !e.message.contains("unknown"))
        );

        // structural type excluded from memories unless requested
        let resp_mem = execute_on_records(
            &records,
            &edges,
            "ns",
            "bob",
            "{ memories(namespace: \"ns\") { edges { node { id memoryType } } } }",
            None,
        );
        // m2 (directory) excluded; only m1 might appear if not filtered by privacy (bob != alice, but visible_to bob allows it; mesh conflict excludes)
        // m1 excluded by mesh_conflict. So empty edges.
        let data = resp_mem.data.as_ref().unwrap();
        let edges_arr = data
            .get("memories")
            .and_then(|m| m.get("edges"))
            .and_then(|e| e.as_array())
            .unwrap();
        assert!(edges_arr.is_empty());
    }

    #[test]
    fn mutation_rejected() {
        let resp = execute_on_records(
            &[],
            &std::collections::HashMap::new(),
            "ns",
            "admin",
            "mutation { createMemory { id } }",
            None,
        );
        assert!(resp.data.is_none() || resp.data.as_ref().unwrap().is_null());
        assert!(resp
            .errors
            .iter()
            .any(|e| e.message == "mutations are not supported"));
    }

    #[test]
    fn unknown_field_rejected() {
        let resp = execute_on_records(
            &[],
            &std::collections::HashMap::new(),
            "ns",
            "admin",
            "{ memories(namespace: \"ns\") { badField } }",
            None,
        );
        assert!(resp.errors.iter().any(|e| e.message.contains("unknown")));
    }

    #[test]
    fn f2_domain_registry_row_absent_from_memories_search_and_memory_by_id() {
        let mk = |content: &str, meta: serde_json::Value| crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: meta,
        };
        let records = vec![
            (
                "reg1".into(),
                mk(
                    "alpha registry row",
                    serde_json::json!({"domain_registry": {"domains": ["auth"]}}),
                ),
            ),
            ("real1".into(), mk("alpha real row", serde_json::json!({}))),
        ];
        // memories surface
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories { edges { node { id } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let rows = resp.data.as_ref().unwrap()["memories"]["edges"]
            .as_array()
            .unwrap();
        let ids: Vec<&serde_json::Value> = rows.iter().map(|r| &r["node"]["id"]).collect();
        assert!(
            ids.contains(&&serde_json::json!("real1")),
            "real row must be served in memories: {ids:?}"
        );
        assert!(
            !ids.contains(&&serde_json::json!("reg1")),
            "domain_registry row must be absent from memories: {ids:?}"
        );
        // search surface
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ search(text: \"alpha\") { edges { node { id } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let rows = resp.data.as_ref().unwrap()["search"]["edges"]
            .as_array()
            .unwrap();
        let ids: Vec<&serde_json::Value> = rows.iter().map(|r| &r["node"]["id"]).collect();
        assert!(
            ids.contains(&&serde_json::json!("real1")),
            "real row must be served in search: {ids:?}"
        );
        assert!(
            !ids.contains(&&serde_json::json!("reg1")),
            "domain_registry row must be absent from search: {ids:?}"
        );
        // memory(id:) surface
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"reg1\") { id } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert!(
            resp.data.as_ref().unwrap()["memory"].is_null(),
            "domain_registry row must be null from memory(id:): {}",
            resp.data.as_ref().unwrap()
        );
        // by-id control: the real row is served by id
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"real1\") { id } }",
            None,
        );
        assert_eq!(resp.data.as_ref().unwrap()["memory"]["id"], "real1");
    }

    #[test]
    fn f2_domain_registry_row_consumes_no_page_slot_and_no_null_edge() {
        let mk = |content: &str, valid_from: i64, meta: serde_json::Value| {
            crate::traits::MemoryPayload {
                content: content.into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: "l".into(),
                location_lines: "1".into(),
                metadata: {
                    let mut m = meta;
                    m["valid_from"] = serde_json::json!(valid_from);
                    m
                },
            }
        };
        // Registry row sorts first in memories (highest valid_from), so if it
        // is not filtered before pagination it consumes the first page slot.
        let records = vec![
            (
                "reg1".into(),
                mk(
                    "registry row",
                    300,
                    serde_json::json!({"domain_registry": {"domains": ["auth"]}}),
                ),
            ),
            ("real1".into(), mk("real one", 200, serde_json::json!({}))),
            ("real2".into(), mk("real two", 100, serde_json::json!({}))),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories(first: 2) { edges { node { id } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let rows = resp.data.as_ref().unwrap()["memories"]["edges"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 2, "both real rows must fill the page: {rows:?}");
        assert!(
            rows.iter().all(|r| !r["node"].is_null()),
            "no edge may carry node: null: {rows:?}"
        );
        let ids: Vec<&serde_json::Value> = rows.iter().map(|r| &r["node"]["id"]).collect();
        assert_eq!(
            ids,
            vec![&serde_json::json!("real1"), &serde_json::json!("real2")],
            "page must contain exactly the real rows in order: {ids:?}"
        );
    }

    // ---- T3 oracle helpers ----
    fn t3_payload(
        content: &str,
        memory_type: &str,
        metadata: serde_json::Value,
    ) -> crate::traits::MemoryPayload {
        crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: memory_type.into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata,
        }
    }

    fn t3_numbered_records(n: usize) -> Vec<(String, crate::traits::MemoryPayload)> {
        (1..=n)
            .map(|i| {
                (
                    format!("m{i}"),
                    t3_payload(
                        "c",
                        "fact",
                        serde_json::json!({"valid_from": 1000 - (i as i64) * 10}),
                    ),
                )
            })
            .collect()
    }

    fn t3_edge_ids(resp: &GraphQLResponse, key: &str) -> Vec<String> {
        let data = resp.data.as_ref().expect("data must be present");
        data[key]["edges"]
            .as_array()
            .expect("edges must be an array")
            .iter()
            .map(|e| {
                e["node"]["id"]
                    .as_str()
                    .expect("node id must be a string")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn t3_o1_renamed_variable_first() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($n: Int) { memories(first: $n) { edges { node { id } } } }",
            Some(&json!({"n": 1})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(t3_edge_ids(&resp, "memories"), vec!["m1".to_string()]);
    }

    #[test]
    fn t3_o1_renamed_variable_after() {
        let records = t3_numbered_records(3);
        let cursor = encode_cursor("valid_from", "m1");
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($c: String) { memories(after: $c) { edges { node { id } } } }",
            Some(&json!({"c": cursor})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories"),
            vec!["m2".to_string(), "m3".to_string()]
        );
    }

    #[test]
    fn t3_o1_renamed_variable_memory_type() {
        let records = vec![
            (
                "f1".into(),
                t3_payload("fact row", "fact", serde_json::json!({"valid_from": 200})),
            ),
            (
                "n1".into(),
                t3_payload("note row", "note", serde_json::json!({"valid_from": 100})),
            ),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($t: String) { memories(memoryType: $t) { edges { node { id } } } }",
            Some(&json!({"t": "note"})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(t3_edge_ids(&resp, "memories"), vec!["n1".to_string()]);
    }

    #[test]
    fn t3_o1_renamed_variable_include_archived() {
        let records = vec![
            (
                "live".into(),
                t3_payload("live", "fact", serde_json::json!({"valid_from": 200})),
            ),
            (
                "arch".into(),
                t3_payload(
                    "archived",
                    "fact",
                    serde_json::json!({"valid_from": 100, "archived": true}),
                ),
            ),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($a: Boolean) { memories(includeArchived: $a) { edges { node { id } } } }",
            Some(&json!({"a": true})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let ids = t3_edge_ids(&resp, "memories");
        assert_eq!(
            ids.len(),
            2,
            "archived row must be included via renamed variable: {ids:?}"
        );
        assert!(
            ids.contains(&"arch".to_string()),
            "archived row id missing: {ids:?}"
        );
    }

    #[test]
    fn t3_o1_renamed_variable_include_retired() {
        let now = chrono::Utc::now().timestamp();
        let records = vec![
            (
                "live".into(),
                t3_payload("live", "fact", serde_json::json!({"valid_from": 200})),
            ),
            (
                "old".into(),
                t3_payload(
                    "retired",
                    "fact",
                    serde_json::json!({"valid_from": 100, "valid_to": now - 10}),
                ),
            ),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($r: Boolean) { memories(includeRetired: $r) { edges { node { id } } } }",
            Some(&json!({"r": true})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let ids = t3_edge_ids(&resp, "memories");
        assert_eq!(
            ids.len(),
            2,
            "retired row must be included via renamed variable: {ids:?}"
        );
        assert!(
            ids.contains(&"old".to_string()),
            "retired row id missing: {ids:?}"
        );
    }

    #[test]
    fn t3_o2_unreferenced_variable_first_does_not_paginate() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories { edges { node { id } } } }",
            Some(&json!({"first": 1})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories").len(),
            3,
            "unreferenced first=1 must not paginate: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o2_unreferenced_variable_include_archived_does_not_include() {
        let records = vec![
            (
                "live".into(),
                t3_payload("live", "fact", serde_json::json!({"valid_from": 200})),
            ),
            (
                "arch".into(),
                t3_payload(
                    "archived",
                    "fact",
                    serde_json::json!({"valid_from": 100, "archived": true}),
                ),
            ),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories { edges { node { id } } } }",
            Some(&json!({"includeArchived": true})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories"),
            vec!["live".to_string()],
            "unreferenced includeArchived=true must not include archived rows: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o3_absent_referenced_variable_ignores_stray_seed() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($n: Int) { memories(first: $n) { edges { node { id } } } }",
            Some(&json!({"first": 1})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories").len(),
            3,
            "absent $n must page by default 20, not stray first=1: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o3_referenced_variable_wins_over_stray_seed() {
        let records = t3_numbered_records(8);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($n: Int) { memories(first: $n) { edges { node { id } } } }",
            Some(&json!({"n": 7, "first": 1})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories").len(),
            7,
            "referenced $n=7 must win over stray first=1: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o4_definition_default_is_not_honoured() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($n: Int = 1) { memories(first: $n) { edges { node { id } } } }",
            Some(&json!({})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories").len(),
            3,
            "definition default must NOT be honoured; default-20 page expected: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o5_string_first_variable_yields_default() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($n: Int) { memories(first: $n) { edges { node { id } } } }",
            Some(&json!({"n": "1"})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories").len(),
            3,
            "string \"1\" must not coerce; default 20 expected: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o5_float_first_variable_yields_default() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($n: Int) { memories(first: $n) { edges { node { id } } } }",
            Some(&json!({"n": 1.5})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories").len(),
            3,
            "float 1.5 must not coerce; default 20 expected: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o5_string_bool_variable_yields_default() {
        let records = vec![
            (
                "live".into(),
                t3_payload("live", "fact", serde_json::json!({"valid_from": 200})),
            ),
            (
                "arch".into(),
                t3_payload(
                    "archived",
                    "fact",
                    serde_json::json!({"valid_from": 100, "archived": true}),
                ),
            ),
        ];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($b: Boolean) { memories(includeArchived: $b) { edges { node { id } } } }",
            Some(&json!({"b": "true"})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t3_edge_ids(&resp, "memories"),
            vec!["live".to_string()],
            "string \"true\" must not coerce; archived stays excluded: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o6_per_field_isolation_literal_arguments() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(&records, &Default::default(), "ns", "admin",
            "{ a: memories(first: 1) { edges { node { id } } } b: memories(first: 2) { edges { node { id } } } }",
            None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(t3_edge_ids(&resp, "a"), vec!["m1".to_string()]);
        assert_eq!(
            t3_edge_ids(&resp, "b"),
            vec!["m1".to_string(), "m2".to_string()]
        );
    }

    #[test]
    fn t3_o6_per_field_isolation_variable_arguments() {
        let records = t3_numbered_records(3);
        let resp = execute_on_records(&records, &Default::default(), "ns", "admin",
            "query($x: Int, $y: Int) { a: memories(first: $x) { edges { node { id } } } b: memories(first: $y) { edges { node { id } } } }",
            Some(&json!({"x": 1, "y": 2})));
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(t3_edge_ids(&resp, "a"), vec!["m1".to_string()]);
        assert_eq!(
            t3_edge_ids(&resp, "b"),
            vec!["m1".to_string(), "m2".to_string()]
        );
    }

    #[test]
    fn t3_o7_int_literal_id_returns_row_42() {
        let records = vec![(
            "42".into(),
            t3_payload("answer", "fact", serde_json::json!({})),
        )];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: 42) { id } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data.as_ref().unwrap()["memory"]["id"],
            "42",
            "Int literal id must return row 42: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o7_int_variable_id_returns_row_42() {
        let records = vec![(
            "42".into(),
            t3_payload("answer", "fact", serde_json::json!({})),
        )];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "query($id: ID) { memory(id: $id) { id } }",
            Some(&json!({"id": 42})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data.as_ref().unwrap()["memory"]["id"],
            "42",
            "integer variable id must return row 42: {:?}",
            resp.data
        );
    }

    #[test]
    fn t3_o7_string_literal_id_control() {
        let records = vec![(
            "42".into(),
            t3_payload("answer", "fact", serde_json::json!({})),
        )];
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"42\") { id } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(resp.data.as_ref().unwrap()["memory"]["id"], "42");
    }

    #[test]
    fn f2_registry_true_only_row_is_served() {
        let mk = |content: &str, meta: serde_json::Value| crate::traits::MemoryPayload {
            content: content.into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "l".into(),
            location_lines: "1".into(),
            metadata: meta,
        };
        let records = vec![
            (
                "boolreg".into(),
                mk(
                    "boolean registry row",
                    serde_json::json!({"registry": true}),
                ),
            ),
            ("real1".into(), mk("real row", serde_json::json!({}))),
        ];
        // memories surface: a registry:true-only row carries no
        // domain_registry mark, so the canonical predicate serves it.
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memories { edges { node { id } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let rows = resp.data.as_ref().unwrap()["memories"]["edges"]
            .as_array()
            .unwrap();
        let ids: Vec<&serde_json::Value> = rows.iter().map(|r| &r["node"]["id"]).collect();
        assert!(
            ids.contains(&&serde_json::json!("boolreg")),
            "registry:true-only row must be served in memories: {ids:?}"
        );
        assert!(
            ids.contains(&&serde_json::json!("real1")),
            "real row must be served in memories: {ids:?}"
        );
        // memory(id:) surface
        let resp = execute_on_records(
            &records,
            &Default::default(),
            "ns",
            "admin",
            "{ memory(id: \"boolreg\") { id } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data.as_ref().unwrap()["memory"]["id"],
            "boolreg",
            "registry:true-only row must be served by id: {}",
            resp.data.as_ref().unwrap()
        );
    }

    // ---- T4 (F3) nested edges oracles ----
    // A node's edges live in `edges_by_source`; the node's JSON carries one
    // fixed "edges" connection object ({ edges, pageInfo }) built by
    // build_memory_node. Nested pagination state must come from the node's
    // own nested `edges` field arguments, never from the root field's.

    fn t4_payload(valid_from: i64) -> crate::traits::MemoryPayload {
        t3_payload("c", "fact", serde_json::json!({"valid_from": valid_from}))
    }

    fn t4_edges_map(
        n: usize,
    ) -> std::collections::HashMap<String, Vec<(String, String, Option<String>)>> {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "m1".to_string(),
            (0..n)
                .map(|i| (format!("t{i}"), "rel".to_string(), None))
                .collect::<Vec<_>>(),
        );
        m
    }

    fn t4_targets(node: &serde_json::Value) -> Vec<String> {
        node["edges"]["edges"]
            .as_array()
            .expect("node edges must be an array")
            .iter()
            .map(|e| {
                e["targetId"]
                    .as_str()
                    .expect("targetId must be a string")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn t4_o1_memory_nested_first_and_after_resume() {
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(&records, &edges, "ns", "admin",
            "{ memory(id: \"m1\") { edges(first: 1) { edges { targetId } pageInfo { hasNextPage endCursor } } } }", None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let node = &resp.data.as_ref().unwrap()["memory"];
        assert_eq!(t4_targets(node), vec!["t0".to_string()]);
        assert_eq!(node["edges"]["pageInfo"]["hasNextPage"], json!(true));
        let end_cursor = node["edges"]["pageInfo"]["endCursor"]
            .as_str()
            .expect("endCursor must be present when hasNextPage")
            .to_string();
        assert_eq!(end_cursor, encode_cursor("edge", "m1:0"));
        let q2 = format!("{{ memory(id: \"m1\") {{ edges(first: 1, after: \"{end_cursor}\") {{ edges {{ targetId }} }} }} }}");
        let resp2 = execute_on_records(&records, &edges, "ns", "admin", &q2, None);
        assert!(resp2.errors.is_empty(), "errors: {:?}", resp2.errors);
        assert_eq!(
            t4_targets(&resp2.data.as_ref().unwrap()["memory"]),
            vec!["t1".to_string()]
        );
    }

    #[test]
    fn t4_o2_connection_depth_nested_first() {
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(
            &records,
            &edges,
            "ns",
            "admin",
            "{ memories { edges { node { id edges(first: 1) { edges { targetId } } } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let node = &resp.data.as_ref().unwrap()["memories"]["edges"][0]["node"];
        assert_eq!(t4_targets(node), vec!["t0".to_string()]);
    }

    #[test]
    fn t4_o3_connection_level_edges_args_are_a_decoy() {
        // edges(first: 99) sits on the CONNECTION-level field, which takes no
        // arguments in the schema; the node's own edges(first: 2) governs.
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(&records, &edges, "ns", "admin",
            "{ memories { edges(first: 99) { node { id edges(first: 2) { edges { targetId } } } } } }", None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let node = &resp.data.as_ref().unwrap()["memories"]["edges"][0]["node"];
        assert_eq!(t4_targets(node), vec!["t0".to_string(), "t1".to_string()]);
    }

    #[test]
    fn t4_o3b_connection_decoy_without_nested_args_keeps_default_page() {
        // PIN: the plan's literal oracle-3 configuration — connection-level
        // edges(first: 99) present with nested edges arguments ABSENT.
        // Green pre- and post-fix by construction; guards against a future
        // "nested-else-connection-fallback" regression, which would read
        // the connection-level first: 99 and return 25 instead of the
        // schema-default page of 20.
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(
            &records,
            &edges,
            "ns",
            "admin",
            "{ memories { edges(first: 99) { node { id edges { edges { targetId } } } } } }",
            None,
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let node = &resp.data.as_ref().unwrap()["memories"]["edges"][0]["node"];
        let targets = t4_targets(node);
        assert_eq!(
            targets.len(),
            20,
            "expected the default page of 20, got {}: {targets:?}",
            targets.len()
        );
        assert_eq!(targets.first().map(String::as_str), Some("t0"));
        assert_eq!(targets.last().map(String::as_str), Some("t19"));
    }

    #[test]
    fn t4_o4_aliased_memory_roots_extract_independently() {
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(&records, &edges, "ns", "admin",
            "{ a: memory(id: \"m1\") { edges(first: 1) { edges { targetId } } } b: memory(id: \"m1\") { edges(first: 3) { edges { targetId } } } }", None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.as_ref().unwrap();
        assert_eq!(t4_targets(&data["a"]), vec!["t0".to_string()]);
        assert_eq!(
            t4_targets(&data["b"]),
            vec!["t0".to_string(), "t1".to_string(), "t2".to_string()]
        );
    }

    #[test]
    fn t4_o5_root_state_isolation_and_memory_root_args_inert() {
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        // Root first: 1 must not cap the node's nested edges(first: 2).
        let resp = execute_on_records(&records, &edges, "ns", "admin",
            "{ memories(first: 1) { edges { node { id edges(first: 2) { edges { targetId } pageInfo { hasNextPage endCursor } } } } } }", None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let node = &resp.data.as_ref().unwrap()["memories"]["edges"][0]["node"];
        assert_eq!(t4_targets(node), vec!["t0".to_string(), "t1".to_string()]);
        let end_cursor = node["edges"]["pageInfo"]["endCursor"]
            .as_str()
            .expect("endCursor must be present when hasNextPage")
            .to_string();
        // Nested after carrying that genuine edge cursor resumes at t2.
        let q2 = format!("{{ memories(first: 1) {{ edges {{ node {{ id edges(first: 2, after: \"{end_cursor}\") {{ edges {{ targetId }} }} }} }} }} }}");
        let resp2 = execute_on_records(&records, &edges, "ns", "admin", &q2, None);
        assert!(resp2.errors.is_empty(), "errors: {:?}", resp2.errors);
        let node2 = &resp2.data.as_ref().unwrap()["memories"]["edges"][0]["node"];
        assert_eq!(t4_targets(node2), vec!["t2".to_string(), "t3".to_string()]);
        // Inertness leg: schema-invalid first: 2 ON the memory root itself no
        // longer caps the node's edges — with no nested arguments the node
        // serves its edges under the default 20 (all 5 here).
        let edges5 = t4_edges_map(5);
        let resp3 = execute_on_records(
            &records,
            &edges5,
            "ns",
            "admin",
            "{ memory(id: \"m1\", first: 2) { edges { edges { targetId } } } }",
            None,
        );
        assert!(resp3.errors.is_empty(), "errors: {:?}", resp3.errors);
        assert_eq!(
            t4_targets(&resp3.data.as_ref().unwrap()["memory"]),
            vec![
                "t0".to_string(),
                "t1".to_string(),
                "t2".to_string(),
                "t3".to_string(),
                "t4".to_string()
            ]
        );
    }

    #[test]
    fn t4_o6_cursor_provenance_edge_resumes_root_restarts() {
        let records = vec![
            ("m1".into(), t4_payload(100)),
            ("m2".into(), t4_payload(50)),
        ];
        let edges = t4_edges_map(25);
        // Page 1 via the nested selection.
        let resp = execute_on_records(&records, &edges, "ns", "admin",
            "{ memory(id: \"m1\") { edges(first: 2) { edges { targetId } pageInfo { endCursor } } } }", None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let node = &resp.data.as_ref().unwrap()["memory"];
        assert_eq!(t4_targets(node), vec!["t0".to_string(), "t1".to_string()]);
        let edge_cursor = node["edges"]["pageInfo"]["endCursor"]
            .as_str()
            .expect("endCursor must be present when hasNextPage")
            .to_string();
        assert_eq!(edge_cursor, encode_cursor("edge", "m1:1"));
        // Page 2 via page 1's EDGE endCursor resumes correctly.
        let q2 = format!("{{ memory(id: \"m1\") {{ edges(first: 2, after: \"{edge_cursor}\") {{ edges {{ targetId }} }} }} }}");
        let resp2 = execute_on_records(&records, &edges, "ns", "admin", &q2, None);
        assert!(resp2.errors.is_empty(), "errors: {:?}", resp2.errors);
        assert_eq!(
            t4_targets(&resp2.data.as_ref().unwrap()["memory"]),
            vec!["t2".to_string(), "t3".to_string()]
        );
        // The ROOT connection's endCursor lives in a different cursor domain.
        let resp_root = execute_on_records(
            &records,
            &edges,
            "ns",
            "admin",
            "{ memories(first: 1) { pageInfo { hasNextPage endCursor } } }",
            None,
        );
        assert!(
            resp_root.errors.is_empty(),
            "errors: {:?}",
            resp_root.errors
        );
        let root_cursor = resp_root.data.as_ref().unwrap()["memories"]["pageInfo"]["endCursor"]
            .as_str()
            .expect("root endCursor must be present when hasNextPage")
            .to_string();
        assert_ne!(root_cursor, edge_cursor);
        // Feeding it as nested after restarts silently at the first edge.
        let q3 = format!("{{ memory(id: \"m1\") {{ edges(first: 2, after: \"{root_cursor}\") {{ edges {{ targetId }} }} }} }}");
        let resp3 = execute_on_records(&records, &edges, "ns", "admin", &q3, None);
        assert!(resp3.errors.is_empty(), "errors: {:?}", resp3.errors);
        assert_eq!(
            t4_targets(&resp3.data.as_ref().unwrap()["memory"]),
            vec!["t0".to_string(), "t1".to_string()]
        );
    }

    #[test]
    fn t4_o7_variable_form_nested_first() {
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(
            &records,
            &edges,
            "ns",
            "admin",
            "query($n: Int) { memory(id: \"m1\") { edges(first: $n) { edges { targetId } } } }",
            Some(&json!({"n": 1})),
        );
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t4_targets(&resp.data.as_ref().unwrap()["memory"]),
            vec!["t0".to_string()]
        );
    }

    #[test]
    fn t4_o8_within_node_aliased_edges_collapse_first_wins() {
        // One node selection carrying edges(first: 1) and an aliased
        // edges(first: 3): build_memory_node emits a single "edges" page, so
        // the two fields collapse to one page — first wins.
        let records = vec![("m1".into(), t4_payload(100))];
        let edges = t4_edges_map(25);
        let resp = execute_on_records(&records, &edges, "ns", "admin",
            "{ memory(id: \"m1\") { edges(first: 1) { edges { targetId } } e2: edges(first: 3) { edges { targetId } } } }", None);
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            t4_targets(&resp.data.as_ref().unwrap()["memory"]),
            vec!["t0".to_string()]
        );
    }
}
