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
pub fn append_event(doc: &mut Value, event: Value) {
    ensure_events_section(doc);
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
