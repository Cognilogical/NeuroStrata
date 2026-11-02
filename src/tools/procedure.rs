//! Procedural Memory — the third leg of the memory triad.
//!
//! Semantic memory says what is true (rule/fact), episodic memory says what
//! happened (the Episodic Buffer), procedural memory says what to *do*. A
//! `procedure` is a knowing-how row: it does not expire on its own, it decays
//! with disuse (`last_performed_at`), it is strengthened by rehearsal
//! (`performance_count`), and it may carry a bounded iteration budget
//! (`remaining_fires`).
//!
//! Two surfaces:
//!   - `neurostrata_procedure_perform` — acknowledge one firing: stamp, count,
//!     decrement the budget, and write the Episodic Buffer entry the row
//!     points back to.
//!   - the `procedures_due` strap on `get_snapshot` — what is still live.
//!
//! No new infrastructure: `valid_to` is the bi-temporal TTL the server already
//! filters on everywhere else, and the episodic entry is
//! `crate::buffer::append_entry` consumed as-is. The trigger enum lives in
//! memory metadata, deliberately outside wiring.rs's closed `fires_on`
//! registry, because a procedure is data a user writes, not code.
//!
//! v1 keeps due-logic thin: every non-lapsed procedure is due, and only the
//! day-cadence trigger gets a real `next_due`.

use crate::traits::{SearchResult, VectorStore};
use serde_json::{json, Value};
use std::sync::Arc;

/// Mirrors `crate::buffer::ROLLOVER_BYTES`, which is private to that module.
/// buffer.rs is consumed as-is here, so the number is repeated rather than
/// exported; if one moves, the other has to move with it.
const ROLLOVER_BYTES: u64 = 500 * 1024;
/// Seconds in a day, for the `every-n-days:N` cadence.
const DAY_SECONDS: i64 = 86_400;
/// Estimated wall-clock length of one session, for the
/// `every-n-sessions:N` cadence (a session carries no timestamp of its own).
const SESSION_ESTIMATE_SECONDS: i64 = 3600;

/// Composite Rec #20: process-wide per-procedure firing locks, keyed by
/// `{namespace}::{id}`. Two concurrent perform calls on the same row would
/// otherwise both read the row before either upserts and both spend one of
/// the same `remaining_fires` — double-spending the budget against a single
/// counter. The async mutex is held from the row's authoritative re-read
/// through `store.upsert`, so concurrent firings serialize.
static PROCEDURE_FIRE_LOCKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> = std::sync::OnceLock::new();

fn fire_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    let map = PROCEDURE_FIRE_LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    // A poisoned std lock must not brick firings for the rest of the
    // process: resume, like every other lock site in this crate does.
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

// ── reading procedure metadata ──────────────────────────────────────────────

/// Has this row passed its bi-temporal expiry?
///
/// Deliberately the same predicate the snapshot's own `valid_to` filter uses:
/// absent or explicitly null never lapses, and a value that is not an integer
/// reads as 0 (long expired) rather than as "expires in the far future".
pub(crate) fn is_lapsed(metadata: &Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(v) => !v.is_null() && v.as_i64().unwrap_or(0) <= now,
        None => false,
    }
}

/// The iteration budget. `None` is unbounded — and so is a value that cannot
/// be parsed as an integer, because an unreadable budget must not refuse
/// every firing. A JSON string holding an integer (`"3"`) counts: metadata
/// round-trips through textual surfaces, so string-encoded integers are
/// honored rather than silently read as unbounded.
fn remaining_fires(metadata: &Value) -> Option<i64> {
    let v = metadata.get("remaining_fires")?;
    if v.is_null() {
        return None;
    }
    match v
        .as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    {
        Some(n) => Some(n),
        None => {
            tracing::warn!("procedure has malformed remaining_fires metadata: {}", v);
            None
        }
    }
}

/// Lifetime rehearsal count.
fn performance_count(metadata: &Value) -> i64 {
    metadata
        .get("performance_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// Epoch of the last acknowledged firing.
fn last_performed_at(metadata: &Value) -> Option<i64> {
    metadata.get("last_performed_at").and_then(|v| v.as_i64())
}

/// Back-reference to the newest Episodic Buffer entry this procedure produced.
fn last_episodic_pointer(metadata: &Value) -> Option<String> {
    metadata
        .get("last_episodic_pointer")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Whether this row arrived through the mesh import funnel. The funnel
/// writes a `mesh` provenance object onto every row it applies (see
/// `synaptic_mesh::import`), and no local write path writes one, so the
/// object's presence is the provenance signal the gate discriminates on.
fn has_import_provenance(metadata: &Value) -> bool {
    metadata
        .get("mesh")
        .map(|m| m.is_object())
        .unwrap_or(false)
}

/// Whether the fire gate blocks this row. The trust stamp is
/// authoritative, and what an absent or unknown stamp means depends on
/// provenance (Program Task 3):
///
/// - `untrusted` never fires, wherever the row came from.
/// - `trusted`, `promoted`, and `local` fire: each is an affirmative
///   decision (manifest policy, local promotion, local authorship).
/// - Absent or unknown stamp on a LOCAL row (no import provenance)
///   fires: absent stamp + no provenance = local authorship, the
///   doctrine this gate has always kept — an unreadable field must not
///   brick a live local procedure.
/// - Absent or unknown stamp on a row WITH import provenance does not
///   fire: for a mesh-sourced row, a missing stamp is missing evidence,
///   not authorship.
fn is_untrusted(metadata: &Value) -> bool {
    match metadata.get("trust").and_then(|v| v.as_str()) {
        Some("untrusted") => true,
        Some("trusted") | Some("promoted") | Some("local") => false,
        _ => has_import_provenance(metadata),
    }
}

/// Stamp a locally created procedure row as locally authored. Every
/// creation path calls this when it mints a procedure — add_memory,
/// task_complete's inline extraction, and the CLI `Add` command (whose
/// memory_type is a free-form argument): authorship is a fact about
/// the write path, so the stamp overwrites any caller-supplied trust
/// value. The mesh import funnel never calls this — imported rows take
/// the manifest's stamp instead (Program Task 3).
pub(crate) fn stamp_local_trust(metadata: &mut Value) {
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("trust".to_string(), json!("local"));
    }
}

/// When this procedure is next owed.
///
/// v1 only gives the day-cadence trigger a real answer: everything else fires
/// on the snapshot it rides on, so it is owed at `now`. A `every-n-days:N` or
/// `every-n-sessions:N` procedure that has never been performed is owed at
/// `now` too — otherwise a brand-new procedure would sit silent until its
/// first rehearsal.
///
/// `every-n-sessions:N` (Fired on snapshots just like `every-n-days:N`, but
/// measured in sessions rather than days): a session has no wall-clock
/// length, so one session is estimated as [`SESSION_ESTIMATE_SECONDS`] of
/// elapsed time between rehearsals. When the row carries a
/// `sessions_since_last_fire` counter it is authoritative instead of the
/// estimate.
fn next_due(metadata: &Value, now: i64) -> i64 {
    let trigger_str = metadata
        .get("trigger")
        .and_then(|t| t.as_str());
    let day_cadence = trigger_str
        .and_then(|t| t.strip_prefix("every-n-days:"))
        .and_then(|n| n.parse::<i64>().ok());
    if let (Some(days), Some(last)) = (day_cadence, last_performed_at(metadata)) {
        return last + days * DAY_SECONDS;
    }
    let session_cadence = trigger_str
        .and_then(|t| t.strip_prefix("every-n-sessions:"))
        .and_then(|n| n.parse::<i64>().ok());
    if let (Some(n_sessions), Some(last)) = (session_cadence, last_performed_at(metadata)) {
        if n_sessions > 1 {
            let sessions_elapsed = metadata
                .get("sessions_since_last_fire")
                .and_then(|v| v.as_i64())
                .unwrap_or(n_sessions - 1);
            return last + sessions_elapsed * SESSION_ESTIMATE_SECONDS;
        }
    }
    now
}

// ── strap: procedures_due ───────────────────────────────────────────────────

/// Every non-lapsed procedure in the namespace, ordered by `next_due`.
pub(crate) fn procedures_due(memories: &[SearchResult], now: i64) -> Vec<Value> {
    let mut rows: Vec<(i64, String, Value)> = Vec::new();
    for memory in memories {
        if memory.payload.memory_type != "procedure" {
            continue;
        }
        let metadata = &memory.payload.metadata;
        if is_lapsed(metadata, now) {
            continue;
        }
        let due = next_due(metadata, now);
        let row = json!({
            "id": memory.id,
            "content": memory.payload.content,
            "trigger": metadata.get("trigger").cloned().unwrap_or(Value::Null),
            "next_due": due,
            "remaining_fires": remaining_fires(metadata),
            "last_performed_at": last_performed_at(metadata),
            "performance_count": performance_count(metadata),
        });
        rows.push((due, memory.id.clone(), row));
    }
    // Ties break on id so two procedures owed at the same instant keep a
    // stable order between two snapshots.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

/// The `procedures_due` block of `get_snapshot`.
pub(crate) fn procedures_due_strap(memories: &[SearchResult], now: i64) -> String {
    let rows = procedures_due(memories, now);
    if rows.is_empty() {
        return "No procedures are due in this namespace.\n".to_string();
    }
    match serde_json::to_string_pretty(&rows) {
        Ok(body) => format!("Procedures due ({}):\n{}", rows.len(), body),
        Err(e) => format!("Procedures due: unreadable ({}).", e),
    }
}

/// Filtered variant: only procedures whose trigger or content matches the
/// query/task_id (substring, case-insensitive). When no due procedure matches
/// the filter, returns `"No procedures match the given filter.\n"` — it does
/// not fall back to the unfiltered list. An empty filter matches all due
/// procedures (the unfiltered strap).
pub(crate) fn procedures_due_strap_filtered(
    memories: &[SearchResult],
    now: i64,
    query: Option<&str>,
    task_id: Option<&str>,
) -> String {
    let all = procedures_due(memories, now);
    let needle = query
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| task_id.map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_lowercase();
    if needle.is_empty() {
        return procedures_due_strap(memories, now);
    }
    let filtered: Vec<Value> = all
        .into_iter()
        .filter(|row| {
            let trigger = row.get("trigger").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            let mem = memories
                .iter()
                .find(|m| m.id == row.get("id").and_then(|v| v.as_str()).unwrap_or(""));
            let content_ok = mem
                .map(|m| m.payload.content.to_lowercase().contains(&needle))
                .unwrap_or(false);
            // Domain-aware matching (F19): also match when the needle is a
            // domain slug that the procedure's metadata.domain expands to.
            let domain_ok = mem.map(|m| {
                let domains: Vec<String> = match m.payload.metadata.get("domain") {
                    Some(serde_json::Value::String(s)) => vec![s.clone()],
                    Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
                    _ => vec![],
                };
                domains.iter().any(|d| {
                    let slug = crate::domain::normalize_domain_slug(d).unwrap_or_else(|| d.to_lowercase());
                    slug == needle || needle == slug || d.to_lowercase() == needle
                })
            }).unwrap_or(false);
            trigger.contains(&needle) || content_ok || domain_ok
        })
        .collect();
    if filtered.is_empty() {
        return "No procedures match the given filter.\n".to_string();
    }
    match serde_json::to_string_pretty(&filtered) {
        Ok(body) => format!("Procedures due ({}):\n{}", filtered.len(), body),
        Err(e) => format!("Procedures due: unreadable ({}).", e),
    }
}

// ── tool: neurostrata_procedure_perform ─────────────────────────────────────

/// Every outcome answers with the same seven keys, so a client can read
/// `performed` without first learning which refusal shape it got.
fn reply(
    performed: bool,
    lapsed: bool,
    spent: bool,
    budget: Option<i64>,
    count: i64,
    pointer: Option<String>,
    reason: &str,
) -> String {
    json!({
        "performed": performed,
        "lapsed": lapsed,
        "spent": spent,
        "remaining_fires": budget,
        "performance_count": count,
        "episodic_pointer": pointer,
        "reason": reason,
    })
    .to_string()
}

/// Where the Episodic Buffer lives. Same vocabulary as
/// `neurostrata_append_log`, which requires the root explicitly; the working
/// directory is the fallback so `{id}` alone is a complete call.
fn project_root(arguments: &Value) -> Option<std::path::PathBuf> {
    arguments
        .get("project_root")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// Acknowledge one firing of a procedure.
///
/// Checks run in the order a caller can act on: the row has to exist, it has
/// to be a procedure, it has to be inside its `valid_to` window, and it has to
/// have fires left. A refused firing mutates nothing — a lapse and a spent
/// budget are statements about the row, not events that change it.
pub async fn handle_procedure_perform(arguments: Value, store: Arc<dyn VectorStore>) -> String {
    let id = match arguments.get("id").and_then(|v| v.as_str()) {
        Some(i) if !i.trim().is_empty() => i.trim().to_string(),
        _ => {
            return reply(
                false,
                false,
                false,
                None,
                0,
                None,
                "missing 'id': procedure_perform needs the procedure's memory id",
            )
        }
    };

    // Optional explicit namespace: when given, address the row directly before
    // paying for the UUID scan across every namespace.
    let explicit_ns = arguments
        .get("namespace")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string);

    // Store errors must not masquerade as `not found`: every Err is kept here
    // and, when the row fails to locate, answered truthfully as a store error.
    let mut last_err: Option<anyhow::Error> = None;

    let mut holder: Option<String> = None;
    if let Some(ns) = explicit_ns {
        holder = match store.get(&ns, &id).await {
            Ok(Some(_)) => Some(ns.clone()),
            Ok(None) => None,
            Err(e) => {
                last_err.get_or_insert(e);
                None
            }
        };
    }
    if holder.is_none() {
        // No namespace parameter (or the explicit one missed): the row is
        // found by scanning the namespace list. Ids are UUIDs, so the scan
        // lands on one row; sorting first keeps the answer independent of
        // database row order.
        let mut namespaces = match store.list_namespaces().await {
            Ok(ns) => ns,
            Err(e) => {
                return reply(
                    false,
                    false,
                    false,
                    None,
                    0,
                    None,
                    &format!("store error: {e}"),
                )
            }
        };
        namespaces.sort();
        // Composite Rec #20, first pass: locate the row's namespace only, so the
        // per-procedure fire lock can be keyed by `{namespace}::{id}` before the
        // mutation path reads anything. Ids are UUIDs, so the scan lands on one
        // row; sorting first keeps the answer independent of database row order.
        for namespace in namespaces {
            match store.get(&namespace, &id).await {
                Ok(Some(_)) => {
                    holder = Some(namespace);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    last_err.get_or_insert(e);
                }
            }
        }
    }
    let namespace = match holder {
        Some(ns) => ns,
        None => {
            return match last_err {
                Some(e) => reply(
                    false,
                    false,
                    false,
                    None,
                    0,
                    None,
                    &format!("store error: {e}"),
                ),
                None => reply(false, false, false, None, 0, None, "not found"),
            };
        }
    };

    // Hold the per-procedure fire lock from before the authoritative read to
    // past the upsert below: a concurrent second perform of the same row
    // parks here instead of spending the same `remaining_fires` twice.
    let lock = fire_lock(&format!("{namespace}::{id}"));
    let _fire_guard = lock.lock().await;

    // Authoritative re-read UNDER the fire lock: the locating pass above can
    // be stale by the time the lock is granted — a competing perform may
    // have already spent a fire between location and lock.
    let (vector, mut payload) = match store.get(&namespace, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return reply(false, false, false, None, 0, None, "not found"),
        Err(e) => {
            let reason = format!("store error: {e}");
            return reply(false, false, false, None, 0, None, &reason);
        }
    };

    if payload.memory_type != "procedure" {
        return reply(
            false,
            false,
            false,
            None,
            0,
            None,
            &format!("wrong memory_type: {}", payload.memory_type),
        );
    }
    if !payload.metadata.is_object() {
        return reply(
            false,
            false,
            false,
            None,
            0,
            None,
            "metadata is not a JSON object, so procedure_perform cannot stamp it",
        );
    }

    let now = chrono::Utc::now().timestamp();
    let metadata = payload.metadata.clone();
    let budget = remaining_fires(&metadata);
    let count = performance_count(&metadata);
    // Composite Rec #20: the fired-from count, so each firing carries an
    // auditable monotonic transition (count-1 → count) even against a row
    // whose current count is later rewritten by hand.
    let prev_count = count;
    let pointer_before = last_episodic_pointer(&metadata);

    // The trust gate runs before lapse and before every mutation: an
    // imported procedure is visible in search, but this node does not
    // execute it until a local operator has explicitly approved it
    // (design §Trust model). Like a lapse, the refusal is a statement
    // about the row, not an event that changes it — so the reply still
    // reports the row's own counters, and nothing is written.
    if is_untrusted(&metadata) {
        return reply(
            false,
            false,
            false,
            budget,
            count,
            pointer_before,
            "untrusted: this procedure was imported from a peer and cannot fire \
             on this node until it is locally promoted — rewrite the row's FULL \
             metadata via set-metadata with trust set to \"promoted\" (set-metadata \
             REPLACES the metadata object rather than merging, so pass the complete \
             object including trigger, remaining_fires, and every other key you \
             want to keep, with trust: \"promoted\")",
        );
    }

    if is_lapsed(&metadata, now) {
        return reply(false, true, false, budget, count, pointer_before, "lapsed");
    }
    if budget == Some(0) {
        return reply(false, false, true, budget, count, pointer_before, "spent");
    }

    let after_budget = budget.map(|b| b.saturating_sub(1));
    let after_count = count + 1;
    let note = arguments
        .get("note")
        .and_then(|n| n.as_str())
        .map(str::trim)
        .filter(|n| !n.is_empty());

    // The entry id is minted here and written into the entry text, so
    // last_episodic_pointer names something an operator can grep the buffer
    // for rather than an id nothing holds.
    let entry_id = uuid::Uuid::new_v4().to_string();
    let line = format!(
        "procedure_perform {entry_id} [{id}] performed (count {after_count}){}",
        note.map(|n| format!(": {n}")).unwrap_or_default()
    );

    let buffer = crate::buffer::load_config();
    let (pointer, buffer_note): (Option<String>, String) = if !buffer.enabled {
        (
            None,
            "Episodic Buffer disabled (config: \"episodic_buffer\": false); no entry written."
                .to_string(),
        )
    } else if let Some(rejection) = crate::buffer::scan_for_append(&line, &[]) {
        // The buffer scans every entry it is handed. Consuming append_entry
        // directly must not become the way around that.
        (None, rejection)
    } else {
        match project_root(&arguments) {
            Some(root) => match crate::buffer::append_entry(
                &root,
                &line,
                &[],
                ROLLOVER_BYTES,
                buffer.retention_days,
            ) {
                Ok(_) => (Some(entry_id.clone()), String::new()),
                Err(e) => (None, format!("Episodic Buffer append failed: {e}")),
            },
            None => (
                None,
                "no project_root available, so the Episodic Buffer entry was not written."
                    .to_string(),
            ),
        }
    };

    if let Some(obj) = payload.metadata.as_object_mut() {
        obj.insert("last_performed_at".to_string(), json!(now));
        obj.insert("performance_count".to_string(), json!(after_count));
        obj.insert("previous_performance_count".to_string(), json!(prev_count));
        obj.insert(
            "last_episodic_pointer".to_string(),
            pointer.clone().map(Value::String).unwrap_or(Value::Null),
        );
        // An unbounded budget stays exactly as it was: absent stays absent and
        // an explicit null stays null, rather than being rewritten to a number.
        if let Some(after) = after_budget {
            obj.insert("remaining_fires".to_string(), json!(after));
        }
    }

    if let Err(e) = store.upsert(&namespace, &id, vector, payload).await {
        return reply(
            false,
            false,
            false,
            after_budget,
            after_count,
            pointer,
            &format!("write failed: {e}"),
        );
    }

    // A missing episodic entry does not un-perform the firing: the
    // acknowledgement is the rehearsal, and the reply says plainly that the
    // log line is missing rather than pretending the whole thing failed.
    let reason = if buffer_note.is_empty() {
        "performed".to_string()
    } else {
        format!("performed; {buffer_note}")
    };
    reply(
        true,
        false,
        false,
        after_budget,
        after_count,
        pointer,
        &reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;

    async fn empty_store(namespace: &str) -> Arc<dyn VectorStore> {
        let dir = std::env::temp_dir().join(format!("ns-procedure-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> = Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&dir, 4).expect("open temp database"),
        );
        store.init(namespace).await.expect("create the schema");
        store
    }

    async fn seed(
        store: &Arc<dyn VectorStore>,
        namespace: &str,
        id: &str,
        memory_type: &str,
        metadata: Value,
    ) {
        store
            .upsert(
                namespace,
                id,
                vec![0.1, 0.2, 0.3, 0.4],
                MemoryPayload {
                    content: "run the gate before every push".to_string(),
                    user_id: "tester".to_string(),
                    memory_type: memory_type.to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata,
                },
            )
            .await
            .expect("seed the row");
    }

    /// Scratch project root, so the Episodic Buffer write lands somewhere
    /// disposable instead of in the working directory.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-procedure-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn procedure_metadata(budget: Value) -> Value {
        json!({ "trigger": "session-start", "remaining_fires": budget })
    }

    async fn stored(store: &Arc<dyn VectorStore>, namespace: &str, id: &str) -> MemoryPayload {
        store
            .get(namespace, id)
            .await
            .expect("read")
            .expect("the row is still there")
            .1
    }

    /// The happy path is the whole contract in one assertion set: the stamp
    /// lands, the count moves, the budget shrinks by exactly one, and the row
    /// points at the episodic entry the firing wrote.
    #[tokio::test]
    async fn performing_a_procedure_stamps_counts_and_spends_one_fire() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-1",
            "procedure",
            procedure_metadata(json!(3)),
        )
        .await;
        let root = scratch("perform");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-1", "note": "gate is green", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("the tool answers with json");

        assert_eq!(reply["performed"], json!(true), "{}", reply);
        assert_eq!(reply["lapsed"], json!(false));
        assert_eq!(reply["spent"], json!(false));
        assert_eq!(reply["remaining_fires"], json!(2));
        assert_eq!(reply["performance_count"], json!(1));

        let payload = stored(&store, "probe", "proc-1").await;
        let meta = &payload.metadata;
        let stamped = meta["last_performed_at"].as_i64().expect("a stamp");
        assert!(
            (chrono::Utc::now().timestamp() - stamped).abs() < 60,
            "stamped now, got {}",
            stamped
        );
        assert_eq!(meta["performance_count"], json!(1));
        assert_eq!(meta["remaining_fires"], json!(2));
        assert_eq!(meta["trigger"], json!("session-start"), "untouched");

        // The buffer write follows the machine's own configuration, exactly as
        // neurostrata_append_log does; both outcomes are contract.
        let pointer = meta["last_episodic_pointer"].as_str();
        if crate::buffer::load_config().enabled {
            let pointer = pointer.expect("an enabled buffer leaves a pointer");
            assert_eq!(reply["episodic_pointer"], json!(pointer));
            let text =
                std::fs::read_to_string(root.join(".NeuroStrata/sessions/current.md")).unwrap();
            assert!(
                text.contains(pointer),
                "the entry is findable by its id: {}",
                text
            );
            assert!(text.contains("gate is green"), "and carries the note");
        } else {
            assert!(pointer.is_none(), "a disabled buffer writes no pointer");
        }
    }

    /// The iteration budget is the whole reason `remaining_fires` exists: three
    /// firings spend it, the fourth is refused, and the refusal changes
    /// nothing. Deleting the decrement breaks this.
    #[tokio::test]
    async fn a_budget_of_three_is_spent_after_three_firings_and_then_refused() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-budget",
            "procedure",
            procedure_metadata(json!(3)),
        )
        .await;
        let root = scratch("budget");

        let mut budgets = Vec::new();
        for expected in [2, 1, 0] {
            let reply: Value = serde_json::from_str(
                &handle_procedure_perform(
                    json!({ "id": "proc-budget", "project_root": root.display().to_string() }),
                    store.clone(),
                )
                .await,
            )
            .expect("json");
            assert_eq!(reply["performed"], json!(true), "{}", reply);
            budgets.push(reply["remaining_fires"].as_i64().unwrap());
            assert_eq!(reply["remaining_fires"], json!(expected));
        }
        assert_eq!(budgets, vec![2, 1, 0], "one fire per performance");

        let spent: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-budget", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(spent["performed"], json!(false));
        assert_eq!(spent["spent"], json!(true));
        assert_eq!(spent["reason"], json!("spent"));

        let meta = stored(&store, "probe", "proc-budget").await.metadata;
        assert_eq!(meta["remaining_fires"], json!(0));
        assert_eq!(
            meta["performance_count"],
            json!(3),
            "the refusal is not a fourth performance"
        );
    }

    /// An unbounded budget stays unbounded: no key is invented and an explicit
    /// null is not rewritten into a number.
    #[tokio::test]
    async fn an_unbounded_budget_never_runs_out() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-null",
            "procedure",
            procedure_metadata(Value::Null),
        )
        .await;
        let root = scratch("unbounded");

        for _ in 0..3 {
            let reply: Value = serde_json::from_str(
                &handle_procedure_perform(
                    json!({ "id": "proc-null", "project_root": root.display().to_string() }),
                    store.clone(),
                )
                .await,
            )
            .expect("json");
            assert_eq!(reply["performed"], json!(true));
            assert_eq!(reply["remaining_fires"], Value::Null);
        }
        let meta = stored(&store, "probe", "proc-null").await.metadata;
        assert_eq!(meta["remaining_fires"], Value::Null);
        assert_eq!(meta["performance_count"], json!(3));
    }

    /// Lapse is a statement about the row, not an event that changes it: the
    /// refusal must leave every field exactly as it found them.
    #[tokio::test]
    async fn a_lapsed_procedure_is_refused_and_untouched() {
        let store = empty_store("probe").await;
        let past = chrono::Utc::now().timestamp() - 60;
        seed(
            &store,
            "probe",
            "proc-lapsed",
            "procedure",
            json!({ "trigger": "session-start", "remaining_fires": 2, "valid_to": past, "performance_count": 7 }),
        )
        .await;
        let before = stored(&store, "probe", "proc-lapsed").await;

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(json!({ "id": "proc-lapsed" }), store.clone()).await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(false), "{}", reply);
        assert_eq!(reply["lapsed"], json!(true));
        assert_eq!(reply["spent"], json!(false));
        assert_eq!(reply["reason"], json!("lapsed"));
        // The reply still reports the row's own numbers rather than zeros.
        assert_eq!(reply["performance_count"], json!(7));
        assert_eq!(reply["remaining_fires"], json!(2));

        assert_eq!(
            stored(&store, "probe", "proc-lapsed").await.metadata,
            before.metadata,
            "a lapsed procedure is not stamped"
        );
    }

    // ── trust: imported procedures must be promoted before they fire ────────

    /// An imported row (`trust: "untrusted"`) is visible but not executable:
    /// the fire is refused before any mutation, the counters keep the values
    /// the row was found with, and the row itself is untouched.
    #[tokio::test]
    async fn untrusted_imported_procedure_cannot_fire() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-import",
            "procedure",
            json!({ "trust": "untrusted", "trigger": "session-start", "remaining_fires": 3 }),
        )
        .await;
        let before = stored(&store, "probe", "proc-import").await;

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-import", "project_root": scratch("import").display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(false), "{}", reply);
        assert_eq!(reply["lapsed"], json!(false));
        assert_eq!(reply["spent"], json!(false));
        // The reply reports the row's own numbers rather than zeros.
        assert_eq!(reply["remaining_fires"], json!(3));
        assert_eq!(reply["performance_count"], json!(0));

        let after = stored(&store, "probe", "proc-import").await;
        assert_eq!(after.metadata, before.metadata, "the row is untouched");
        assert!(
            after.metadata.get("last_performed_at").is_none(),
            "no stamp: refusal is not a rehearsal"
        );
    }

    /// Promotion is the explicit local approval: once `trust` reads
    /// "promoted", the procedure fires exactly like a local one.
    #[tokio::test]
    async fn promoted_procedure_fires() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-promoted",
            "procedure",
            json!({ "trust": "promoted", "trigger": "session-start", "remaining_fires": 2 }),
        )
        .await;
        let root = scratch("promoted");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-promoted", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(true), "{}", reply);
        assert_eq!(reply["remaining_fires"], json!(1));
        assert_eq!(reply["performance_count"], json!(1));

        let meta = stored(&store, "probe", "proc-promoted").await.metadata;
        assert_eq!(meta["remaining_fires"], json!(1));
        assert!(meta["last_performed_at"].is_i64(), "stamped like any fire");
    }

    /// `trusted` is also a fireable trust level (the design's third name for
    /// a locally-approved row).
    #[tokio::test]
    async fn trusted_procedure_fires() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-trusted",
            "procedure",
            json!({ "trust": "trusted", "trigger": "session-start", "remaining_fires": 2 }),
        )
        .await;
        let root = scratch("trusted");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-trusted", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(true), "{}", reply);
        assert_eq!(reply["remaining_fires"], json!(1));
    }

    /// Regression lock: a local procedure with no trust key at all fires
    /// exactly as it did before the gate existed.
    #[tokio::test]
    async fn absent_trust_fires_unchanged() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-local",
            "procedure",
            procedure_metadata(json!(3)),
        )
        .await;
        let root = scratch("local");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-local", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(true), "{}", reply);
        assert_eq!(reply["remaining_fires"], json!(2));

        let meta = stored(&store, "probe", "proc-local").await.metadata;
        assert_eq!(meta["remaining_fires"], json!(2));
        assert_eq!(meta["performance_count"], json!(1));
        assert!(
            meta.get("trust").is_none(),
            "firing must not invent a trust key on a local row"
        );
    }

    /// Program Task 3 (d): the fail-closed half of the gate applies ONLY
    /// to rows carrying import provenance. An imported row whose stamp
    /// is absent — or holds a value outside the known set — must not
    /// fire: for a mesh-sourced row, a missing stamp is not local
    /// authorship, it is missing evidence.
    #[tokio::test]
    async fn unstamped_imported_row_does_not_fire() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-imported-plain",
            "procedure",
            json!({
                "trigger": "session-start",
                "remaining_fires": 3,
                "mesh": {"mesh_id": "mesh-alpha", "update_id": "u-1", "origin": "mesh-import"}
            }),
        )
        .await;
        seed(
            &store,
            "probe",
            "proc-imported-unknown",
            "procedure",
            json!({
                "trust": "sender-invented-value",
                "trigger": "session-start",
                "remaining_fires": 3,
                "mesh": {"mesh_id": "mesh-alpha", "update_id": "u-2", "origin": "mesh-import"}
            }),
        )
        .await;
        let root = scratch("imported-unstamped");

        for id in ["proc-imported-plain", "proc-imported-unknown"] {
            let reply: Value = serde_json::from_str(
                &handle_procedure_perform(
                    json!({ "id": id, "project_root": root.display().to_string() }),
                    store.clone(),
                )
                .await,
            )
            .expect("json");
            assert_eq!(
                reply["performed"],
                json!(false),
                "{id} carries import provenance without a known stamp and must not fire: {reply}"
            );
            assert!(
                reply["reason"]
                    .as_str()
                    .expect("reason text")
                    .starts_with("untrusted"),
                "{id}: the refusal names the trust state: {reply}"
            );
            let meta = stored(&store, "probe", id).await.metadata;
            assert!(
                meta.get("last_performed_at").is_none(),
                "{id}: refusal is not a rehearsal"
            );
        }
    }

    /// Program Task 3: a procedure seeded in the exact shape the repo's
    /// own AGENTS.md §3a documents — add_memory fields, NO trust field —
    /// still fires. Stamping happens at creation, but rows in the
    /// documented local shape keep firing exactly as before.
    #[tokio::test]
    async fn section_3a_shaped_procedure_fires() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-3a",
            "procedure",
            json!({
                "trigger": "session-start",
                "remaining_fires": 3,
                "valid_to": Value::Null,
                "performance_count": 0,
                "last_performed_at": Value::Null,
                "last_episodic_pointer": Value::Null
            }),
        )
        .await;
        let root = scratch("section-3a");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-3a", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(
            reply["performed"],
            json!(true),
            "the documented local procedure shape must fire: {reply}"
        );
        assert_eq!(reply["remaining_fires"], json!(2));
    }

    /// The refusal has to teach the fix: it names the trust state, the
    /// promotion verb, and the target value — and says plainly that
    /// set-metadata replaces the metadata object, so the full object has to
    /// be written back with trust set to "promoted".
    #[tokio::test]
    async fn refusal_message_names_promotion_path() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-msg",
            "procedure",
            json!({ "trust": "untrusted", "trigger": "session-start", "remaining_fires": 3 }),
        )
        .await;

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(json!({ "id": "proc-msg" }), store.clone()).await,
        )
        .expect("json");
        let reason = reply["reason"].as_str().unwrap();
        assert!(reason.contains("untrusted"), "{}", reason);
        assert!(reason.contains("set-metadata"), "{}", reason);
        assert!(reason.contains("promoted"), "{}", reason);
        assert!(
            reason.to_lowercase().contains("replac"),
            "the message must warn that set-metadata replaces the metadata object: {}",
            reason
        );
    }

    /// A future `valid_to` is a live deadline, not a lapse -- the same
    /// distinction supersede makes when it refuses to retire a row early.
    #[tokio::test]
    async fn a_future_expiry_is_still_performable() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-live",
            "procedure",
            json!({
                "trigger": "every-n-days:7",
                "remaining_fires": null,
                "valid_to": chrono::Utc::now().timestamp() + 3600
            }),
        )
        .await;
        let root = scratch("future");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-live", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(true), "{}", reply);
        assert_eq!(reply["lapsed"], json!(false));
    }

    #[tokio::test]
    async fn a_non_procedure_row_is_refused_by_its_actual_type() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "rule-1",
            "rule",
            json!({ "enforcement": "ENFORCED" }),
        )
        .await;
        let before = stored(&store, "probe", "rule-1").await;

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(json!({ "id": "rule-1" }), store.clone()).await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(false));
        assert_eq!(reply["lapsed"], json!(false));
        assert_eq!(reply["spent"], json!(false));
        assert_eq!(reply["reason"], json!("wrong memory_type: rule"));
        assert_eq!(
            stored(&store, "probe", "rule-1").await.metadata,
            before.metadata,
            "a rule is not stamped as if it were a procedure"
        );
    }

    #[tokio::test]
    async fn an_unknown_id_is_not_found() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-1",
            "procedure",
            procedure_metadata(json!(1)),
        )
        .await;

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(json!({ "id": "nope" }), store.clone()).await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(false));
        assert_eq!(reply["reason"], json!("not found"));

        let no_id: Value =
            serde_json::from_str(&handle_procedure_perform(json!({}), store.clone()).await)
                .expect("json");
        assert_eq!(no_id["performed"], json!(false));
        assert!(
            no_id["reason"].as_str().unwrap().contains("missing 'id'"),
            "{}",
            no_id
        );
    }

    /// A procedure in another namespace is still reachable: the tool is
    /// addressed by id alone, so it finds the row rather than guessing.
    #[tokio::test]
    async fn a_procedure_is_found_outside_the_namespace_the_caller_thought_of() {
        let store = empty_store("probe").await;
        store.init("other").await.expect("schema");
        seed(
            &store,
            "other",
            "proc-far",
            "procedure",
            procedure_metadata(json!(1)),
        )
        .await;
        let root = scratch("far");

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(
                json!({ "id": "proc-far", "project_root": root.display().to_string() }),
                store.clone(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(true), "{}", reply);
        assert_eq!(
            stored(&store, "other", "proc-far").await.metadata["performance_count"],
            json!(1)
        );
    }

    // ── valid_to, the TTL the rest of the server already shares ─────────────

    #[test]
    fn lapse_reads_valid_to_the_way_the_snapshot_filter_does() {
        let now = 1_000_000;
        assert!(!is_lapsed(&json!({}), now), "absent never lapses");
        assert!(
            !is_lapsed(&json!({ "valid_to": null }), now),
            "null never lapses"
        );
        assert!(
            !is_lapsed(&json!({ "valid_to": now + 1 }), now),
            "future is live"
        );
        assert!(
            is_lapsed(&json!({ "valid_to": now }), now),
            "the instant itself lapses"
        );
        assert!(is_lapsed(&json!({ "valid_to": now - 1 }), now));
        // An unreadable expiry reads as long expired rather than as never,
        // matching the snapshot's `unwrap_or(0) > now` filter.
        assert!(is_lapsed(&json!({ "valid_to": "soon" }), now));
    }

    #[test]
    fn next_due_is_now_unless_a_day_cadence_has_a_rehearsal_to_count_from() {
        let now = 1_000_000;
        let day = 86_400;
        assert_eq!(next_due(&json!({ "trigger": "session-start" }), now), now);
        assert_eq!(
            next_due(&json!({}), now),
            now,
            "an absent trigger still fires"
        );
        assert_eq!(
            next_due(&json!({ "trigger": "every-n-days:7" }), now),
            now,
            "never performed: owed now, not in seven days"
        );
        assert_eq!(
            next_due(
                &json!({ "trigger": "every-n-days:7", "last_performed_at": now - day }),
                now
            ),
            now + 6 * day
        );
        assert_eq!(
            next_due(
                &json!({ "trigger": "every-n-days:x", "last_performed_at": now - day }),
                now
            ),
            now,
            "an unparseable cadence falls back to v1's always-due"
        );
    }

    // ── the procedures_due strap ───────────────────────────────────────────

    fn row(id: &str, memory_type: &str, metadata: Value) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score: 0.0,
            payload: MemoryPayload {
                content: format!("do {}", id),
                user_id: "tester".to_string(),
                memory_type: memory_type.to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata,
            },
            evidence: None,
        }
    }

    /// Three procedures, one of them lapsed: the strap shows the two live ones
    /// and nothing else, in `next_due` order.
    #[test]
    fn procedures_due_carries_the_live_rows_ordered_by_next_due() {
        let now = 1_000_000;
        let memories = vec![
            row(
                "proc-lapsed",
                "procedure",
                json!({ "trigger": "session-start", "valid_to": now - 1 }),
            ),
            row(
                "proc-later",
                "procedure",
                json!({
                    "trigger": "every-n-days:7",
                    "last_performed_at": now - 86_400,
                    "performance_count": 3,
                    "remaining_fires": null
                }),
            ),
            row(
                "proc-now",
                "procedure",
                json!({ "trigger": "before-edit", "remaining_fires": 1, "performance_count": 0 }),
            ),
            row("rule-1", "rule", json!({})),
        ];

        let due = procedures_due(&memories, now);
        let ids: Vec<&str> = due.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            vec!["proc-now", "proc-later"],
            "lapsed and non-procedures drop out"
        );

        let first = &due[0];
        assert_eq!(first["content"], json!("do proc-now"));
        assert_eq!(first["trigger"], json!("before-edit"));
        assert_eq!(first["next_due"], json!(now));
        assert_eq!(first["remaining_fires"], json!(1));
        assert_eq!(first["last_performed_at"], Value::Null);
        assert_eq!(first["performance_count"], json!(0));

        assert_eq!(due[1]["next_due"], json!(now + 6 * 86_400));
        assert_eq!(due[1]["performance_count"], json!(3));
    }

    #[test]
    fn the_strap_says_so_when_nothing_is_due() {
        let now = 1_000_000;
        assert_eq!(
            procedures_due_strap(&[row("rule-1", "rule", json!({}))], now),
            "No procedures are due in this namespace.\n"
        );
    }

    /// Composite Rec #20: the per-procedure fire lock's whole point. Many
    /// concurrent performs of the same budgeted row must serialize exactly
    /// `remaining_fires` times: none may spend a fire against a count the
    /// other also read, none may run after the budget is exhausted, and the
    /// final row must carry the roll call of every accepted firing through
    /// `previous_performance_count`. Break the lock (or read outside it) and
    /// two racers read the same counters before either upserts — the budget
    /// under-spends and the count lags the lost updates.
    #[tokio::test]
    async fn concurrent_performs_serialize_and_never_double_spend_the_budget() {
        let store = empty_store("probe").await;
        seed(
            &store,
            "probe",
            "proc-race",
            "procedure",
            procedure_metadata(json!(3)),
        )
        .await;
        let root = scratch("race");

        let racers = 6usize;
        let mut responded: Vec<Value> = Vec::new();
        for _ in 0..racers {
            let (store, root) = (store.clone(), root.clone());
            responded.push(
                tokio::spawn(async move {
                    serde_json::from_str(
                        &handle_procedure_perform(
                            json!({
                                "id": "proc-race",
                                "project_root": root.display().to_string()
                            }),
                            store,
                        )
                        .await,
                    )
                    .expect("json reply")
                })
                .await
                .expect("join"),
            );
        }

        let performed = responded
            .iter()
            .filter(|r| r["performed"] == json!(true))
            .count();
        assert_eq!(
            performed, 3,
            "exactly one firing per remaining fire, all others spent-refused: {:?}",
            responded
        );

        let meta = stored(&store, "probe", "proc-race").await.metadata;
        assert_eq!(meta["remaining_fires"], json!(0), "{}", meta);
        assert_eq!(meta["performance_count"], json!(3), "{}", meta);
        // The monotonic audit trail: after a wake-up, the very next
        // serialized firing records 2 → 3.
        assert_eq!(meta["previous_performance_count"], json!(2), "{}", meta);
    }
}
