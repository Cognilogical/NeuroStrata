//! Post-dispatch tool receipts (spec §6 R6).
//!
//! Crate-free by design: integration tests compile this file directly from its
//! real path, the same way `snapshot_readiness.rs` is shared. Receipts are
//! LOCAL JSON records in `.NeuroStrata/session_queries.json` — never memory
//! rows — so the SynapticMesh outbox (which exports only shareable *memory*
//! types) cannot carry them. Raw query/content text is never retained: only
//! SHA-256 digests are written.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Ledger schema version for the additive `events[]` section. Legacy
/// `queries[]` stays untouched for compatibility but is excluded from every
/// quality metric.
pub const EVENTS_VERSION: u32 = 1;
/// Bound on retained receipts so a long session cannot grow the ledger forever.
pub const MAX_EVENTS: usize = 500;
/// Bound on `returned_ids` in a single receipt.
pub const MAX_RETURNED_IDS: usize = 32;

/// Exact R6 receipt field set. Tests pin this so an accidental raw field or a
/// missing attribution field cannot drift in silently.
pub const RECEIPT_FIELD_NAMES: &[&str] = &[
    "event_id",
    "ts",
    "session_id",
    "task_id",
    "namespace",
    "tool",
    "operation",
    "trigger_id",
    "route_class",
    "schema_valid",
    "status",
    "latency_ms",
    "result_count",
    "returned_ids",
    "prescribed_action_ids",
    "followed_action_ids",
    "query_digest",
    "content_digest",
];

/// Honest outcome of a tool dispatch. `failed` receipts are still recorded —
/// they are telemetry, not a gate. The snapshot readiness consumer derives its
/// own tool allowlist and never counts failed rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptStatus {
    Success,
    Empty,
    Unsupported,
    Failed,
}

impl ReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReceiptStatus::Success => "success",
            ReceiptStatus::Empty => "empty",
            ReceiptStatus::Unsupported => "unsupported",
            ReceiptStatus::Failed => "failed",
        }
    }
}

/// All R6 fields. There is deliberately no `query`/`content` field: the only
/// way to construct one is via [`ReceiptFields::new`], which hashes raw text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptFields {
    pub event_id: String,
    pub ts: i64,
    pub session_id: String,
    pub task_id: String,
    pub namespace: String,
    pub tool: String,
    pub operation: String,
    pub trigger_id: String,
    pub route_class: String,
    pub schema_valid: bool,
    pub status: ReceiptStatus,
    pub latency_ms: u64,
    pub result_count: usize,
    pub returned_ids: Vec<String>,
    pub prescribed_action_ids: Vec<String>,
    pub followed_action_ids: Vec<String>,
    pub query_digest: String,
    pub content_digest: String,
}

impl ReceiptFields {
    /// Build a receipt from raw query/content. Only digests survive; returned
    /// ids are bounded so a pathological response cannot bloat the ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: String,
        ts: i64,
        session_id: String,
        task_id: String,
        namespace: String,
        tool: String,
        operation: String,
        trigger_id: String,
        route_class: String,
        schema_valid: bool,
        status: ReceiptStatus,
        latency_ms: u64,
        result_count: usize,
        returned_ids: Vec<String>,
        prescribed_action_ids: Vec<String>,
        followed_action_ids: Vec<String>,
        query_raw: &str,
        content_raw: &str,
    ) -> Self {
        let mut returned_ids = returned_ids;
        returned_ids.truncate(MAX_RETURNED_IDS);
        Self {
            event_id,
            ts,
            session_id,
            task_id,
            namespace,
            tool,
            operation,
            trigger_id,
            route_class,
            schema_valid,
            status,
            latency_ms,
            result_count,
            returned_ids,
            prescribed_action_ids,
            followed_action_ids,
            query_digest: digest(query_raw),
            content_digest: digest(content_raw),
        }
    }

    /// Serialize exactly the R6 field set — no raw text, no memory-row markers.
    pub fn to_value(&self) -> Value {
        json!({
            "event_id": self.event_id,
            "ts": self.ts,
            "session_id": self.session_id,
            "task_id": self.task_id,
            "namespace": self.namespace,
            "tool": self.tool,
            "operation": self.operation,
            "trigger_id": self.trigger_id,
            "route_class": self.route_class,
            "schema_valid": self.schema_valid,
            "status": self.status.as_str(),
            "latency_ms": self.latency_ms,
            "result_count": self.result_count,
            "returned_ids": self.returned_ids,
            "prescribed_action_ids": self.prescribed_action_ids,
            "followed_action_ids": self.followed_action_ids,
            "query_digest": self.query_digest,
            "content_digest": self.content_digest,
        })
    }
}

/// SHA-256 of raw text, hex-encoded. Callers pass raw text here and store only
/// the returned digest.
pub fn digest(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Ensure a ledger object carries the versioned additive `events[]` section.
/// Missing or malformed ledgers are normalized in place; legacy `queries[]`
/// is never touched.
pub fn ensure_events_section(doc: &mut Value) {
    if !doc.is_object() {
        *doc = json!({});
    }
    let obj = doc.as_object_mut().expect("ledger is an object");
    if !obj.contains_key("events_version") {
        obj.insert("events_version".to_string(), json!(EVENTS_VERSION));
    }
    if !obj.get("events").map(Value::is_array).unwrap_or(false) {
        obj.insert("events".to_string(), json!([]));
    }
}

/// Append one receipt event, dropping the oldest entries past [`MAX_EVENTS`].
pub fn append_event(doc: &mut Value, mut event: Value) {
    ensure_events_section(doc);
    // Defense in depth: bound `returned_ids` even on events built by hand, not
    // only through `ReceiptFields::new`. The ledger can never grow without bound
    // from a single oversized response.
    if let Some(ids) = event.get_mut("returned_ids").and_then(Value::as_array_mut) {
        if ids.len() > MAX_RETURNED_IDS {
            ids.truncate(MAX_RETURNED_IDS);
        }
    }
    let events = doc
        .get_mut("events")
        .and_then(Value::as_array_mut)
        .expect("events section is an array");
    events.push(event);
    if events.len() > MAX_EVENTS {
        let drop_n = events.len() - MAX_EVENTS;
        events.drain(0..drop_n);
    }
}

/// Additive `tool_invocations: {tool: count}` for advanced subsystems. No
/// gates, quotas or nudges are derived from these counters.
pub fn bump_tool_invocation(doc: &mut Value, tool: &str) {
    if !doc.is_object() {
        *doc = json!({});
    }
    let obj = doc.as_object_mut().expect("ledger is an object");
    let counters = obj
        .entry("tool_invocations".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !counters.is_object() {
        *counters = Value::Object(Map::new());
    }
    let count = counters
        .get(tool)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    counters
        .as_object_mut()
        .expect("tool_invocations is an object")
        .insert(tool.to_string(), json!(count + 1));
}

/// Model the SynapticMesh export filter over candidate rows. Only rows whose
/// `memory_type` is in the shareable allowlist can leave the machine; receipts
/// carry no `memory_type`, so zero receipt rows are ever exported.
pub fn mesh_exportable_rows(rows: &[Value], shareable_types: &[&str]) -> Vec<Value> {
    rows.iter()
        .filter(|row| {
            row.get("memory_type")
                .and_then(Value::as_str)
                .map(|t| shareable_types.contains(&t))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Derive the honest receipt status from a tool's REAL returned text. This is
/// only ever called AFTER the handler returns — never at dispatch time — so an
/// unsupported/empty/failed outcome cannot be recorded as `success`.
pub fn derive_status(result_text: &str, is_error: bool) -> ReceiptStatus {
    if is_error {
        return ReceiptStatus::Failed;
    }
    let lower = result_text.to_ascii_lowercase();
    if lower.contains("\"unsupported\":true")
        || lower.contains("\"status\":\"unsupported\"")
        || lower.contains("\"status\": \"unsupported\"")
    {
        return ReceiptStatus::Unsupported;
    }
    if let Ok(v) = serde_json::from_str::<Value>(result_text) {
        if v.get("unsupported").and_then(Value::as_bool) == Some(true)
            || v.get("status").and_then(Value::as_str) == Some("unsupported")
        {
            return ReceiptStatus::Unsupported;
        }
        // A failed lookup still carries an empty result array (`query_failure_response`
        // emits `{status:"failed", memories:[]}`). Failure outranks emptiness:
        // counting it as genuine coverage would inflate coverage/diversity.
        if v.get("status").and_then(Value::as_str) == Some("failed")
            || v.get("error")
                .and_then(Value::as_str)
                .map(|e| !e.trim().is_empty())
                .unwrap_or(false)
        {
            return ReceiptStatus::Failed;
        }
        if is_empty_result(&v) {
            return ReceiptStatus::Empty;
        }
    }
    if lower.contains("no relevant memories found.")
        || lower.contains("no results")
        || lower.contains("not found")
    {
        return ReceiptStatus::Empty;
    }
    // Text guards for non-JSON responses. Placed after the existing Empty
    // patterns so a genuine "no results" text is not reclassified as failed.
    if lower.starts_with("error")
        || lower.contains("error [")
        || lower.contains("error:")
        || lower.contains("\"status\":\"failed\"")
        || lower.contains("\"status\": \"failed\"")
        || lower.contains("\"error\":")
    {
        return ReceiptStatus::Failed;
    }
    ReceiptStatus::Success
}

/// True when a JSON response carries only empty result collections.
fn is_empty_result(v: &Value) -> bool {
    const RESULT_KEYS: &[&str] = &[
        "memories",
        "results",
        "symbols",
        "concepts",
        "related_memories",
        "hits",
        "rows",
        "nodes",
    ];
    let arrays: Vec<&Vec<Value>> = RESULT_KEYS
        .iter()
        .filter_map(|k| v.get(k).and_then(Value::as_array))
        .collect();
    !arrays.is_empty() && arrays.iter().all(|a| a.is_empty())
}

/// Marker emitted by `format_memory` for every rendered search result.
const MEMORY_ID_PREFIX: &str = "--- Memory ID: ";

/// Bounded ids from the human-readable `render_search_results` report. Only
/// the ids survive — surrounding rendered text is never retained.
fn text_memory_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(MEMORY_ID_PREFIX) {
        let after = &rest[pos + MEMORY_ID_PREFIX.len()..];
        let line_end = after.find('\n').unwrap_or(after.len());
        let line = &after[..line_end];
        let id = line.split(" ---").next().unwrap_or(line).trim();
        if !id.is_empty() {
            ids.push(id.to_string());
            if ids.len() >= MAX_RETURNED_IDS {
                break;
            }
        }
        rest = &after[line_end..];
    }
    ids
}

/// Bounded result ids extracted from a REAL response (JSON or search text).
pub fn extract_returned_ids(result_text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(result_text) else {
        return text_memory_ids(result_text);
    };
    let mut ids = Vec::new();
    const RESULT_KEYS: &[&str] = &[
        "memories",
        "results",
        "symbols",
        "concepts",
        "related_memories",
        "hits",
        "rows",
    ];
    for key in RESULT_KEYS {
        if let Some(arr) = v.get(*key).and_then(Value::as_array) {
            for item in arr {
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        ids.push(id.to_string());
                        if ids.len() >= MAX_RETURNED_IDS {
                            return ids;
                        }
                    }
                }
            }
        }
    }
    ids
}

/// Result count from a REAL response: an explicit JSON count field, the sum of
/// known JSON result arrays, or the bounded `--- Memory ID:` markers of a text
/// report. Never invented.
pub fn result_count(result_text: &str) -> usize {
    let Ok(v) = serde_json::from_str::<Value>(result_text) else {
        return text_memory_ids(result_text).len();
    };
    if let Some(n) = v
        .get("result_count")
        .and_then(Value::as_u64)
        .or_else(|| v.get("count").and_then(Value::as_u64))
        .or_else(|| v.get("total").and_then(Value::as_u64))
    {
        return n as usize;
    }
    const RESULT_KEYS: &[&str] = &[
        "memories",
        "results",
        "symbols",
        "concepts",
        "related_memories",
        "hits",
        "rows",
    ];
    RESULT_KEYS
        .iter()
        .filter_map(|k| v.get(k).and_then(Value::as_array))
        .map(|a| a.len())
        .sum()
}

/// Coarse route class for a tool name, derived locally so receipt builders
/// stay crate-free.
pub fn route_class(tool: &str) -> &'static str {
    if matches!(
        tool,
        "neurostrata_taxonomy" | "neurostrata_object_view" | "neurostrata_sparql_query"
    ) {
        "ontology"
    } else {
        "graph_read"
    }
}

/// Build a fully-attributed receipt from the REAL outcome of a dispatch. All
/// outcome fields (status, count, ids, digests, latency) come from the actual
/// returned text — nothing here is a hardcoded success placeholder.
#[allow(clippy::too_many_arguments)]
pub fn build_dispatch_receipt(
    event_id: String,
    ts: i64,
    session_id: String,
    namespace: String,
    tool: String,
    query_raw: &str,
    result_text: &str,
    is_error: bool,
    latency_ms: u64,
) -> ReceiptFields {
    let status = derive_status(result_text, is_error);
    let returned_ids = extract_returned_ids(result_text);
    let count = result_count(result_text);
    let operation = tool.trim_start_matches("neurostrata_").to_string();
    let schema_valid = !is_error && serde_json::from_str::<Value>(result_text).is_ok();
    ReceiptFields::new(
        event_id,
        ts,
        session_id,
        String::new(), // task_id filled by the recorder that stamps the active task
        namespace,
        tool.clone(),
        operation,
        String::new(),
        route_class(&tool).to_string(),
        schema_valid,
        status,
        latency_ms,
        count,
        returned_ids,
        Vec::new(),
        Vec::new(),
        query_raw,
        result_text,
    )
}

/// Read §5 action objects from the REAL handler response text. `next_actions`
/// (used by `memories_for_file`) and `next_hop_calls` (used by task context)
/// both carry `trigger_id` and `action_id`; neither is synthesized here.
pub fn trigger_context_from_result(result_text: &str) -> (String, Vec<String>) {
    let Ok(value) = serde_json::from_str::<Value>(result_text) else {
        return (String::new(), Vec::new());
    };
    let mut triggers: BTreeSet<String> = BTreeSet::new();
    let mut action_ids: Vec<String> = Vec::new();
    for key in ["next_actions", "next_hop_calls"] {
        let Some(actions) = value.get(key).and_then(Value::as_array) else {
            continue;
        };
        for action in actions {
            if let Some(id) = action.get("trigger_id").and_then(Value::as_str) {
                if !id.trim().is_empty() {
                    triggers.insert(id.to_string());
                }
            }
            if let Some(id) = action.get("action_id").and_then(Value::as_str) {
                if !id.trim().is_empty() {
                    action_ids.push(id.to_string());
                }
            }
        }
    }
    (
        triggers.into_iter().next().unwrap_or_default(),
        action_ids,
    )
}

/// Fill the two trigger-attribution fields from the REAL response, leaving a
/// post-dispatch receipt otherwise untouched. No response actions means no
/// trigger context — the advisory layer then honestly reports no applicable
/// escalation instead of inventing one.
pub fn attach_trigger_context(mut fields: ReceiptFields, result_text: &str) -> ReceiptFields {
    let (trigger_id, action_ids) = trigger_context_from_result(result_text);
    if !trigger_id.is_empty() {
        fields.trigger_id = trigger_id;
    }
    if !action_ids.is_empty() {
        fields.prescribed_action_ids = action_ids;
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_status_explicit_failed_precedes_empty() {
        // A failed lookup carries an empty result array, but failure is the
        // honest status and must win over empty-array detection.
        assert_eq!(
            derive_status(r#"{"status":"failed","memories":[]}"#, false),
            ReceiptStatus::Failed
        );
        assert_eq!(
            derive_status(r#"{"error":"query backend down","memories":[]}"#, false),
            ReceiptStatus::Failed
        );
        // Malformed (non-JSON) responses still carry an explicit failed status.
        assert_eq!(
            derive_status(r#"{"status": "failed", "memories": []"#, false),
            ReceiptStatus::Failed
        );
    }

    #[test]
    fn receipt_status_genuine_empty_unchanged() {
        assert_eq!(
            derive_status(r#"{"status":"empty","memories":[]}"#, false),
            ReceiptStatus::Empty
        );
        assert_eq!(
            derive_status("no relevant memories found.", false),
            ReceiptStatus::Empty
        );
    }

    #[test]
    fn receipt_text_report_ids_and_count_bounded() {
        // `handle_search_memory` returns render_search_results text, not JSON.
        let report = concat!(
            "--- Memory ID: mem-alpha ---\nType: rule\nContent: a\n\n",
            "--- Memory ID: mem-beta --- (score 0.91)\nType: fact\nContent: b\n\n",
            "--- Memory ID: mem-gamma ---\nType: fact\nContent: c\n"
        );
        assert_eq!(
            extract_returned_ids(report),
            vec!["mem-alpha", "mem-beta", "mem-gamma"]
        );
        assert_eq!(result_count(report), 3);

        assert!(extract_returned_ids("no relevant memories found.").is_empty());
        assert_eq!(result_count("no relevant memories found."), 0);

        // JSON responses keep their existing behaviour.
        let json_text = r#"{"memories":[{"id":"mem-json"}]}"#;
        assert_eq!(extract_returned_ids(json_text), vec!["mem-json"]);
        assert_eq!(result_count(json_text), 1);

        // Both ids and count respect the same bound.
        let many = (0..(MAX_RETURNED_IDS + 5))
            .map(|i| format!("--- Memory ID: mem-{i} ---\n"))
            .collect::<String>();
        assert_eq!(extract_returned_ids(&many).len(), MAX_RETURNED_IDS);
        assert_eq!(result_count(&many), MAX_RETURNED_IDS);
    }
}
