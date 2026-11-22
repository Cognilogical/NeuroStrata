//! Post-dispatch tool receipts (spec §6 R6).
//!
//! Crate-free by design: integration tests compile this file directly from its
//! real path, the same way `snapshot_readiness.rs` is shared. Receipts are
//! LOCAL JSON records in `.NeuroStrata/session_queries.json` — never memory
//! rows — so the SynapticMesh outbox (which exports only shareable *memory*
//! types) cannot carry them. Raw query/content text is never retained: only
//! SHA-256 digests are written.

use fs2::FileExt as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Process-wide counter for collision-free ledger temp names: a PID alone
/// cannot separate two writers inside one process.
static LEDGER_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    // A bare legacy `queries[]` array is wrapped losslessly: the rows survive
    // verbatim and the object gains the additive events section below. This is
    // the same normalization the ledger writer applies before it mutates.
    if doc.is_array() {
        let rows = std::mem::take(doc);
        // Match mutate_session_ledger's wrap: it always carries a namespace.
        // A bare legacy array records none, so the neutral "global" default
        // applies (same missing-namespace policy as MCP receipts).
        *doc = json!({ "namespace": "global", "queries": rows });
    }
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

/// The ONE ledger mutation path shared by every writer. It quarantines a
/// corrupt file instead of clobbering it, normalizes a bare legacy array
/// losslessly, and commits through a sibling temp file + atomic rename so a
/// crashed process can never leave a half-written ledger behind.
///
/// `mutate` runs only on a freshly parsed (or normalized) document. If the
/// original file is non-empty but not JSON, nothing is written at all: the
/// caller's data is preserved at the original path and copied to
/// `<ledger>.corrupt` for an operator to inspect.
pub fn mutate_session_ledger<F>(path: &std::path::Path, namespace: &str, mutate: F)
where
    F: FnOnce(&mut Value),
{
    // The pre-commit hook and this daemon both read-modify-write this ledger.
    // Hold one cross-process advisory lock for the whole operation so neither
    // writer's rename discards the other's receipts or recorded queries.
    let lock_path = path.with_file_name(format!(
        "{}.lock",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session_queries.json".to_string())
    ));
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok();
    if let Some(file) = lock_file.as_ref() {
        let _ = file.lock_exclusive();
    }
    mutate_session_ledger_inner(path, namespace, mutate)
    // `lock_file` is dropped here, releasing the advisory lock.
}

fn mutate_session_ledger_inner<F>(path: &std::path::Path, namespace: &str, mutate: F)
where
    F: FnOnce(&mut Value),
{
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let mut doc: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(err) => {
                let corrupt = path.with_extension("json.corrupt");
                if let Err(copy_err) = std::fs::copy(path, &corrupt) {
                    tracing::warn!(
                        "session ledger {} is corrupt ({err}); quarantine copy failed: {copy_err}",
                        path.display()
                    );
                } else {
                    tracing::warn!(
                        "session ledger {} is corrupt ({err}); refusing to overwrite; quarantined to {}",
                        path.display(),
                        corrupt.display()
                    );
                }
                return;
            }
        },
        // Absent or empty file: start from a fresh, valid ledger document.
        _ => json!({ "namespace": namespace, "queries": [] }),
    };
    if doc.is_array() {
        let rows = std::mem::take(&mut doc);
        doc = json!({ "namespace": namespace, "queries": rows });
    } else if !doc.is_object() {
        doc = json!({ "namespace": namespace, "queries": [] });
    }
    mutate(&mut doc);
    let Ok(serialized) = serde_json::to_string_pretty(&doc) else {
        return;
    };
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session_queries.json".to_string());
    let tmp = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        LEDGER_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&tmp)?;
        std::io::Write::write_all(&mut file, serialized.as_bytes())?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(
            "atomic rename of session ledger {} failed: {err}",
            path.display()
        );
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
    let count = counters.get(tool).and_then(Value::as_u64).unwrap_or(0);
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
    if lower.starts_with("missing ")
        || lower.starts_with("error")
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

/// JSON array keys that carry real result rows across graph/ontology handlers.
const RESULT_KEYS: &[&str] = &[
    "memories",
    "results",
    "symbols",
    "concepts",
    "related_memories",
    "hits",
    "rows",
    "locations",
    "recommendations",
    "gaps",
];

/// Legacy key recognized only by empty-detection (kept for behavior parity).
const EMPTY_ONLY_RESULT_KEYS: &[&str] = &["nodes"];

/// An ASK query returns one boolean verdict even though `solution_count` is 0.
fn is_ask_verdict(v: &Value) -> bool {
    v.get("query_form").and_then(Value::as_str) == Some("ask")
        && v.get("results")
            .and_then(Value::as_object)
            .and_then(|o| o.get("boolean"))
            .map(Value::is_boolean)
            .unwrap_or(false)
}

/// Append a non-empty id, bounded to [`MAX_RETURNED_IDS`]. No raw text is kept.
fn push_bounded(ids: &mut Vec<String>, id: Option<&str>) {
    if let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) {
        if ids.len() < MAX_RETURNED_IDS {
            ids.push(id.to_string());
        }
    }
}

/// Bounded governing-rule ids from a top-level `governing_rules` array.
fn push_rule_ids(v: &Value, ids: &mut Vec<String>) {
    if let Some(rules) = v.get("governing_rules").and_then(Value::as_array) {
        for rule in rules {
            push_bounded(ids, rule.get("id").and_then(Value::as_str));
        }
    }
}

/// Total items across diff-tour stops.
fn diff_tour_item_count(v: &Value) -> usize {
    v.get("stops")
        .and_then(Value::as_array)
        .map(|stops| {
            stops
                .iter()
                .map(|stop| {
                    stop.get("items")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

/// True when a JSON response carries only empty result collections.
fn is_empty_result(v: &Value) -> bool {
    // A real verdict (ASK boolean) or an object view is always one result.
    if is_ask_verdict(v) || v.get("object_view").map(Value::is_object).unwrap_or(false) {
        return false;
    }
    if v.get("solution_count").and_then(Value::as_u64) == Some(0) {
        return true;
    }
    if let Some(stops) = v.get("stops").and_then(Value::as_array) {
        return stops.is_empty() || diff_tour_item_count(v) == 0;
    }
    let arrays: Vec<&Vec<Value>> = RESULT_KEYS
        .iter()
        .chain(EMPTY_ONLY_RESULT_KEYS.iter())
        .filter_map(|k| v.get(k).and_then(Value::as_array))
        .collect();
    let has_single_id = ["concept_id", "thesis_id"].iter().any(|k| {
        v.get(k)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    });
    !arrays.is_empty() && arrays.iter().all(|a| a.is_empty()) && !has_single_id
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
    for key in RESULT_KEYS {
        if let Some(arr) = v.get(*key).and_then(Value::as_array) {
            for item in arr {
                if *key == "locations" {
                    // locate items carry scip_symbol/symbol/path, not always id.
                    let id = ["id", "scip_symbol", "symbol", "path"]
                        .iter()
                        .find_map(|k| item.get(k).and_then(Value::as_str));
                    push_bounded(&mut ids, id);
                    if let Some(rules) = item.get("governing_rules").and_then(Value::as_array) {
                        for rule in rules {
                            push_bounded(&mut ids, rule.get("id").and_then(Value::as_str));
                        }
                    }
                } else {
                    push_bounded(&mut ids, item.get("id").and_then(Value::as_str));
                }
                if ids.len() >= MAX_RETURNED_IDS {
                    return ids;
                }
            }
        }
    }
    // Single-object view: the viewed id is the one bounded id.
    if let Some(view) = v.get("object_view").filter(|view| view.is_object()) {
        push_bounded(&mut ids, view.get("id").and_then(Value::as_str));
    }
    // Diff tour: one bounded id per stop item (symbol, else path, else id).
    if let Some(stops) = v.get("stops").and_then(Value::as_array) {
        for stop in stops {
            if let Some(items) = stop.get("items").and_then(Value::as_array) {
                for item in items {
                    let id = ["symbol", "path", "id"]
                        .iter()
                        .find_map(|k| item.get(k).and_then(Value::as_str));
                    push_bounded(&mut ids, id);
                }
            }
            if let Some(rules) = stop.get("governing_rules").and_then(Value::as_array) {
                for rule in rules {
                    push_bounded(&mut ids, rule.get("id").and_then(Value::as_str));
                }
            }
        }
    }
    // Top-level governing rules (locate / diff_tour emit these at the root).
    push_rule_ids(&v, &mut ids);
    // Single-id fallbacks (taxonomy upsert_concept / set_thesis).
    push_bounded(&mut ids, v.get("concept_id").and_then(Value::as_str));
    push_bounded(&mut ids, v.get("thesis_id").and_then(Value::as_str));
    ids
}

/// Result count from a REAL response: an explicit JSON count field, the sum of
/// known JSON result arrays, or the bounded `--- Memory ID:` markers of a text
/// report. Never invented.
pub fn result_count(result_text: &str) -> usize {
    let Ok(v) = serde_json::from_str::<Value>(result_text) else {
        return text_memory_ids(result_text).len();
    };
    // ASK is one boolean verdict regardless of its zero solution_count.
    if is_ask_verdict(&v) {
        return 1;
    }
    if let Some(n) = v
        .get("result_count")
        .and_then(Value::as_u64)
        .or_else(|| v.get("count").and_then(Value::as_u64))
        .or_else(|| v.get("total").and_then(Value::as_u64))
        .or_else(|| v.get("solution_count").and_then(Value::as_u64))
    {
        return n as usize;
    }
    // A single-object view is one result.
    if v.get("object_view").map(Value::is_object).unwrap_or(false) {
        return 1;
    }
    // Diff tour: sum stop items, or the declared total when stops is absent.
    if v.get("stops").and_then(Value::as_array).is_some() {
        return diff_tour_item_count(&v);
    }
    if let Some(n) = v.get("total_symbols_touched").and_then(Value::as_u64) {
        return n as usize;
    }
    let mut count: usize = RESULT_KEYS
        .iter()
        .filter_map(|k| v.get(k).and_then(Value::as_array))
        .map(Vec::len)
        .sum();
    for key in ["concept_id", "thesis_id"] {
        if v.get(key)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            count += 1;
        }
    }
    count
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
    // The handler's real `operation` (e.g. taxonomy `advise`) outranks the
    // tool-derived name; a response without one falls back to the tool name.
    let operation = serde_json::from_str::<Value>(result_text)
        .ok()
        .and_then(|v| {
            v.get("operation")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| tool.trim_start_matches("neurostrata_").to_string());
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

/// For `neurostrata_taxonomy` dispatch only: recover the requested operation
/// from call args when the handler response carried no top-level `operation`
/// (e.g. an error path). Never stores raw args — only the bounded op string.
pub fn taxonomy_operation_fallback(
    operation: &str,
    is_taxonomy: bool,
    args: Option<&Value>,
) -> String {
    if is_taxonomy && operation == "taxonomy" {
        if let Some(op) = args
            .and_then(|a| a.get("operation"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return op.to_string();
        }
    }
    operation.to_string()
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
    (triggers.into_iter().next().unwrap_or_default(), action_ids)
}

/// Fill `prescribed_action_ids` from the REAL response actions, leaving the
/// receipt's own `trigger_id` alone. A response's `next_actions` describe work
/// the CALLER should do next; stamping one of those triggers onto the
/// prescribing receipt would make an unresolved prescription look fulfilled.
/// The only self-description we add is `T_PATH` for a `memories_for_file`
/// receipt whose trigger slot is still empty — that receipt IS the genuine
/// path lookup, not a prescription of one.
pub fn attach_trigger_context(mut fields: ReceiptFields, result_text: &str) -> ReceiptFields {
    let (_prescribed_trigger, action_ids) = trigger_context_from_result(result_text);
    if !action_ids.is_empty() {
        fields.prescribed_action_ids = action_ids;
    }
    if fields.trigger_id.trim().is_empty() && fields.tool == "neurostrata_memories_for_file" {
        fields.trigger_id = "T_PATH".to_string();
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

    // ---- Task 11 Fix 1: missing-query search classified success (c2) ----
    #[test]
    fn receipt_status_missing_query_search_is_failed() {
        let text = "Missing 'query' parameter.\n\n[search_contract] query=* top_k=5";
        assert_eq!(derive_status(text, false), ReceiptStatus::Failed);
        // Genuine empty search outcomes still classify as Empty, not Failed.
        assert_eq!(
            derive_status(r#"{"memories":[]}"#, false),
            ReceiptStatus::Empty
        );
        assert_eq!(
            derive_status("no relevant memories found.", false),
            ReceiptStatus::Empty
        );
    }

    // ---- Task 11 Fix 2a/2c: taxonomy receipt operation = handler op ----
    #[test]
    fn receipt_operation_comes_from_handler_result() {
        let mk = |tool: &str, text: &str| {
            build_dispatch_receipt(
                "evt-op".to_string(),
                1,
                "ses".to_string(),
                "ns".to_string(),
                tool.to_string(),
                "q",
                text,
                false,
                1,
            )
        };
        assert_eq!(
            mk(
                "neurostrata_taxonomy",
                r#"{"operation":"advise","recommendations":[]}"#
            )
            .operation,
            "advise"
        );
        assert_eq!(
            mk("neurostrata_taxonomy", r#"{"operation":"briefing"}"#).operation,
            "briefing"
        );
        // Non-taxonomy tool with no top-level operation is unchanged.
        assert_eq!(
            mk("neurostrata_related", r#"{"memories":[]}"#).operation,
            "related"
        );
    }

    // ---- Task 11 Fix 2b: failed taxonomy call operation from args ----
    #[test]
    fn taxonomy_failed_call_operation_from_args() {
        let args = serde_json::json!({"operation":"advise"});
        assert_eq!(
            taxonomy_operation_fallback("taxonomy", true, Some(&args)),
            "advise"
        );
        assert_eq!(
            taxonomy_operation_fallback("related", false, Some(&args)),
            "related"
        );
        let no_op = serde_json::json!({"namespace":"NeuroStrata"});
        assert_eq!(
            taxonomy_operation_fallback("taxonomy", true, Some(&no_op)),
            "taxonomy"
        );
        assert_eq!(
            taxonomy_operation_fallback("advise", true, Some(&args)),
            "advise"
        );
        assert_eq!(
            taxonomy_operation_fallback("taxonomy", true, None),
            "taxonomy"
        );
    }

    // ---- Task 11 Fix 3: honest count/ids for remaining graph tools ----
    #[test]
    fn receipt_count_and_ids_sparql() {
        let select = r#"{"query_form":"select","solution_count":3,"results":[]}"#;
        assert_eq!(result_count(select), 3);
        assert!(extract_returned_ids(select).is_empty());

        // ASK returns one boolean verdict even though solution_count is 0.
        let ask = r#"{"query_form":"ask","solution_count":0,"results":{"boolean":true}}"#;
        assert_eq!(result_count(ask), 1);
        assert_eq!(derive_status(ask, false), ReceiptStatus::Success);

        let empty = r#"{"query_form":"select","solution_count":0,"results":[]}"#;
        assert_eq!(result_count(empty), 0);
        assert_eq!(derive_status(empty, false), ReceiptStatus::Empty);
    }

    #[test]
    fn receipt_count_and_ids_locate() {
        let text = r#"{"locations":[{"path":"src/a.rs","scip_symbol":"foo"},{"id":"mem-2","symbol":"bar"}],"governing_rules":[{"id":"rule-1"}]}"#;
        assert_eq!(result_count(text), 2);
        assert_eq!(extract_returned_ids(text), vec!["foo", "mem-2", "rule-1"]);
        let empty = r#"{"locations":[],"governing_rules":[]}"#;
        assert_eq!(result_count(empty), 0);
        assert_eq!(derive_status(empty, false), ReceiptStatus::Empty);
    }

    #[test]
    fn receipt_count_and_ids_object_view() {
        let text = r#"{"object_view":{"id":"mem-x","location":"src/lib.rs"}}"#;
        assert_eq!(result_count(text), 1);
        assert_eq!(extract_returned_ids(text), vec!["mem-x"]);
        assert_eq!(derive_status(text, false), ReceiptStatus::Success);
    }

    #[test]
    fn receipt_count_and_ids_diff_tour() {
        let text = r#"{"total_symbols_touched":2,"stops":[{"items":[{"symbol":"s1","path":"src/a.rs"},{"path":"src/b.rs"}]}],"governing_rules":[{"id":"rule-2"}]}"#;
        assert_eq!(result_count(text), 2);
        assert_eq!(extract_returned_ids(text), vec!["s1", "src/b.rs", "rule-2"]);
        // Fallback when stops is absent.
        assert_eq!(result_count(r#"{"total_symbols_touched":5}"#), 5);
        let empty = r#"{"stops":[]}"#;
        assert_eq!(result_count(empty), 0);
        assert_eq!(derive_status(empty, false), ReceiptStatus::Empty);
    }

    #[test]
    fn receipt_count_and_ids_taxonomy() {
        let text =
            r#"{"operation":"advise","recommendations":[{"id":"rec-1"}],"gaps":[{"id":"gap-1"}]}"#;
        assert_eq!(result_count(text), 2);
        assert_eq!(extract_returned_ids(text), vec!["rec-1", "gap-1"]);
        let single = r#"{"operation":"briefing","concept_id":"concept-1"}"#;
        assert_eq!(result_count(single), 1);
        assert_eq!(extract_returned_ids(single), vec!["concept-1"]);
        let empty = r#"{"recommendations":[],"gaps":[]}"#;
        assert_eq!(result_count(empty), 0);
        assert_eq!(derive_status(empty, false), ReceiptStatus::Empty);
    }

    // ---- Task 13 P1-2: a prescription never stamps its own trigger id ----
    #[test]
    fn attach_trigger_context_does_not_self_resolve() {
        let base = build_dispatch_receipt(
            "e".to_string(),
            1,
            "s".to_string(),
            "ns".to_string(),
            "neurostrata_memories_for_file".to_string(),
            "q",
            r#"{"memories":[]}"#,
            false,
            1,
        );
        // The response prescribes T_AMBIGUOUS; the prescribing receipt must
        // still identify itself as the genuine T_PATH lookup.
        let prescribing = r#"{"status":"empty","memories":[],"next_actions":[{"trigger_id":"T_AMBIGUOUS","action_id":"T_AMBIGUOUS:abc","tool":"neurostrata_locate"}]}"#;
        let receipt = attach_trigger_context(base, prescribing);
        assert_eq!(receipt.trigger_id, "T_PATH");
        assert_eq!(receipt.prescribed_action_ids, vec!["T_AMBIGUOUS:abc"]);

        // A receipt that already carries a genuine trigger keeps it.
        let mut other = build_dispatch_receipt(
            "e2".to_string(),
            1,
            "s".to_string(),
            "ns".to_string(),
            "neurostrata_related".to_string(),
            "q",
            r#"{"memories":[]}"#,
            false,
            1,
        );
        other.trigger_id = "T_HIGH_BLAST".to_string();
        let other = attach_trigger_context(other, prescribing);
        assert_eq!(other.trigger_id, "T_HIGH_BLAST");
    }

    // ---- Task 13 P1-5: corrupt-ledger quarantine + bare-array wrap ----
    #[test]
    fn mutate_session_ledger_quarantines_corrupt_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(".NeuroStrata/session_queries.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{ this is not json ";
        std::fs::write(&path, original).unwrap();
        mutate_session_ledger(&path, "ns", |doc| {
            doc["events"] = json!([{"event_id": "should-never-land"}]);
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "a corrupt ledger must never be clobbered"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.corrupt")).unwrap(),
            original,
            "quarantine copy must hold the untouched original"
        );
    }

    #[test]
    fn mutate_session_ledger_wraps_bare_array_losslessly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session_queries.json");
        std::fs::write(&path, r#"[{"tool":"neurostrata_related","query":"x.rs"}]"#).unwrap();
        mutate_session_ledger(&path, "ns", |doc| {
            append_event(doc, json!({"event_id": "e1"}));
        });
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["queries"][0]["tool"],
            json!("neurostrata_related"),
            "legacy rows survive verbatim"
        );
        assert_eq!(doc["namespace"], json!("ns"));
        assert_eq!(doc["events"][0]["event_id"], json!("e1"));
        // No sibling temp file is left behind after an atomic commit.
        assert_eq!(
            std::fs::read_dir(tmp.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains("tmp-"))
                .count(),
            0
        );
    }

    #[test]
    fn ensure_events_section_wraps_bare_array() {
        let mut doc = json!([{"tool":"neurostrata_locate"}]);
        ensure_events_section(&mut doc);
        assert_eq!(doc["queries"][0]["tool"], json!("neurostrata_locate"));
        assert_eq!(doc["events_version"], json!(EVENTS_VERSION));
        assert!(doc["events"].as_array().unwrap().is_empty());
        // Review MINOR: the bare-array wrap must carry a namespace just as the
        // mutate_session_ledger wrap does. A bare legacy array has no recorded
        // namespace, so the neutral "global" default applies.
        assert_eq!(
            doc["namespace"],
            json!("global"),
            "bare-array wrap must include a namespace"
        );
    }
}
