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

use crate::traits::{MemoryPayload, SearchResult, VectorStore};
use serde_json::{json, Value};
use std::sync::Arc;

/// Mirrors `crate::buffer::ROLLOVER_BYTES`, which is private to that module.
/// buffer.rs is consumed as-is here, so the number is repeated rather than
/// exported; if one moves, the other has to move with it.
const ROLLOVER_BYTES: u64 = 500 * 1024;
/// Seconds in a day, for the `every-n-days:N` cadence.
const DAY_SECONDS: i64 = 86_400;

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

/// The iteration budget. `None` is unbounded — and so is a value that is not
/// an integer, because an unreadable budget must not refuse every firing.
fn remaining_fires(metadata: &Value) -> Option<i64> {
    metadata
        .get("remaining_fires")
        .and_then(|v| v.as_i64())
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
    metadata
        .get("last_performed_at")
        .and_then(|v| v.as_i64())
}

/// Back-reference to the newest Episodic Buffer entry this procedure produced.
fn last_episodic_pointer(metadata: &Value) -> Option<String> {
    metadata
        .get("last_episodic_pointer")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// When this procedure is next owed.
///
/// v1 only gives the day-cadence trigger a real answer: everything else fires
/// on the snapshot it rides on, so it is owed at `now`. A `every-n-days:N`
/// procedure that has never been performed is owed at `now` too — otherwise a
/// brand-new procedure would sit silent until its first rehearsal.
fn next_due(metadata: &Value, now: i64) -> i64 {
    let cadence = metadata
        .get("trigger")
        .and_then(|t| t.as_str())
        .and_then(|t| t.strip_prefix("every-n-days:"))
        .and_then(|n| n.parse::<i64>().ok());
    match (cadence, last_performed_at(metadata)) {
        (Some(days), Some(last)) => last + days * DAY_SECONDS,
        _ => now,
    }
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

    // No namespace parameter: the tool is addressed by memory id alone, so the
    // row is found by scanning the namespace list. Ids are UUIDs, so the scan
    // lands on one row; sorting first keeps the answer independent of database
    // row order.
    let mut namespaces = store.list_namespaces().await.unwrap_or_default();
    namespaces.sort();
    let mut found: Option<(String, Vec<f32>, MemoryPayload)> = None;
    for namespace in namespaces {
        match store.get(&namespace, &id).await {
            Ok(Some(row)) => {
                found = Some((namespace, row.0, row.1));
                break;
            }
            Ok(None) => {}
            Err(_) => continue,
        }
    }
    let (namespace, vector, mut payload) = match found {
        Some(row) => row,
        None => return reply(false, false, false, None, 0, None, "not found"),
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
    let pointer_before = last_episodic_pointer(&metadata);

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
        seed(&store, "probe", "proc-1", "procedure", procedure_metadata(json!(3))).await;
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
            let text = std::fs::read_to_string(root.join(".NeuroStrata/sessions/current.md")).unwrap();
            assert!(text.contains(pointer), "the entry is findable by its id: {}", text);
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
        seed(&store, "probe", "proc-budget", "procedure", procedure_metadata(json!(3))).await;
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
        assert_eq!(meta["performance_count"], json!(3), "the refusal is not a fourth performance");
    }

    /// An unbounded budget stays unbounded: no key is invented and an explicit
    /// null is not rewritten into a number.
    #[tokio::test]
    async fn an_unbounded_budget_never_runs_out() {
        let store = empty_store("probe").await;
        seed(&store, "probe", "proc-null", "procedure", procedure_metadata(Value::Null)).await;
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
        seed(&store, "probe", "rule-1", "rule", json!({ "enforcement": "ENFORCED" })).await;
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
        seed(&store, "probe", "proc-1", "procedure", procedure_metadata(json!(1))).await;

        let reply: Value = serde_json::from_str(
            &handle_procedure_perform(json!({ "id": "nope" }), store.clone()).await,
        )
        .expect("json");
        assert_eq!(reply["performed"], json!(false));
        assert_eq!(reply["reason"], json!("not found"));

        let no_id: Value = serde_json::from_str(
            &handle_procedure_perform(json!({}), store.clone()).await,
        )
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
        seed(&store, "other", "proc-far", "procedure", procedure_metadata(json!(1))).await;
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
        assert!(!is_lapsed(&json!({ "valid_to": null }), now), "null never lapses");
        assert!(!is_lapsed(&json!({ "valid_to": now + 1 }), now), "future is live");
        assert!(is_lapsed(&json!({ "valid_to": now }), now), "the instant itself lapses");
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
        assert_eq!(next_due(&json!({}), now), now, "an absent trigger still fires");
        assert_eq!(
            next_due(&json!({ "trigger": "every-n-days:7" }), now),
            now,
            "never performed: owed now, not in seven days"
        );
        assert_eq!(
            next_due(&json!({ "trigger": "every-n-days:7", "last_performed_at": now - day }), now),
            now + 6 * day
        );
        assert_eq!(
            next_due(&json!({ "trigger": "every-n-days:x", "last_performed_at": now - day }), now),
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
        assert_eq!(ids, vec!["proc-now", "proc-later"], "lapsed and non-procedures drop out");

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
}