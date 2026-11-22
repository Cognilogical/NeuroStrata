//! The task subsystem: handlers for the eight MCP tools, the beads importer,
//! and the snapshot header that puts every session inside the task system.
//!
//! Tasks are memories (`memory_type: "task"`) whose state lives in
//! `metadata.task`. `done` has exactly one entrance -- `task_complete`, and it
//! is guarded by an extraction (Lock 2). See docs/design-task-subsystem.md
//! sections 1 through 3 and 6 through 8.

pub mod gate;
pub mod graph_advisories;
pub mod harness_plugins;
pub mod intake;
pub mod intent;
pub mod machine;
pub mod precheck;
pub mod receipt;
pub mod routing;
pub mod snapshot_readiness;
pub mod tool_receipts;
pub mod wiring;

use crate::formal_kernel::{evaluate_claim_decision, KernelClaimDecision};
use crate::judgment::DeduplicationChecker;
use crate::store::LadybugStore;
use crate::traits::{Embedder, GraphRead, MemoryPayload, SearchResult, VectorStore};
use intake::undisposed_high_confidence_obligations;
use machine::{annotate, apply, Ctx, Status, Task};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

static TASK_RECEIPTS: std::sync::LazyLock<std::sync::Mutex<crate::receipts::store::ReceiptStore>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(crate::receipts::store::ReceiptStore::new()));

// ── small helpers shared by every handler ──────────────────────────────────

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    match arg_str(args, key) {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(format!("ERROR: '{}' is required.", key)),
    }
}

fn string_list(args: &Value, key: &str) -> Result<Vec<String>, String> {
    match args.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => return Err(format!("ERROR: '{}' must be an array of strings.", key)),
                }
            }
            Ok(out)
        }
        Some(_) => Err(format!("ERROR: '{}' must be an array of strings.", key)),
    }
}

/// The exact funnel message (section 2): `done` cannot be reached from
/// `task_update`, and saying so points at the one tool that can.
pub const DONE_FUNNEL_MESSAGE: &str =
    "ERROR: done is reachable only via neurostrata_task_complete, which requires memory extraction (Lock 2).";

fn check_namespace(namespace: &str) -> Result<(), String> {
    if namespace.contains('/') || namespace.contains('\\') {
        return Err(
            "ERROR [NAMESPACE]: The namespace cannot be a file path. It must be the exact project name (e.g., 'NeuroStrata'). Do not use slashes."
                .to_string(),
        );
    }
    Ok(())
}

fn encode(body: &Value) -> String {
    serde_json::to_string_pretty(body)
        .unwrap_or_else(|e| format!("ERROR: could not encode the response: {}", e))
}

/// The id key for a namespace: `MyProj` -> `myproj`, so ids read as
/// `<ns-key>-<4 base36>` (section 1.1).
fn ns_key(namespace: &str) -> String {
    let mut key = String::with_capacity(namespace.len());
    for c in namespace.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            key.push(c);
        } else if c.is_ascii_uppercase() {
            key.push(c.to_ascii_lowercase());
        } else if !key.is_empty() && !key.ends_with('-') {
            key.push('-');
        }
    }
    let key = key.trim_matches('-').to_string();
    if key.is_empty() {
        "ns".to_string()
    } else {
        key
    }
}

/// Upper bound on a task title. Long enough for a sentence, short enough that
/// the task stays a one-line label in list output and snapshot headers.
const MAX_TITLE: usize = 200;

/// Validates a caller-supplied title: present, not blank, not absurdly long.
fn check_title(title: &str) -> Result<String, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("ERROR: 'title' cannot be empty or whitespace-only.".to_string());
    }
    if trimmed.chars().count() > MAX_TITLE {
        return Err(format!(
            "ERROR: 'title' is {} characters; the maximum is {}.",
            trimmed.chars().count(),
            MAX_TITLE
        ));
    }
    Ok(trimmed.to_string())
}

const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// How many fresh suffixes to try before declaring a collision problem.
const ID_ATTEMPTS: usize = 64;

fn random_suffix() -> String {
    let id = uuid::Uuid::new_v4();
    let bytes = id.as_bytes();
    let mut out = String::with_capacity(4);
    for b in bytes.iter().take(4) {
        out.push(BASE36[*b as usize % 36] as char);
    }
    out
}

fn allocate_task_id(key: &str, existing: &HashSet<String>) -> Option<String> {
    for _ in 0..ID_ATTEMPTS {
        let id = format!("{}-{}", key, random_suffix());
        if !existing.contains(&id) {
            return Some(id);
        }
    }
    None
}

async fn existing_ids(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
) -> Result<HashSet<String>, String> {
    store
        .list(namespace, None, None)
        .await
        .map(|rows| rows.into_iter().map(|r| r.id).collect())
        .map_err(|e| format!("Failed to list memories for id allocation: {}", e))
}

/// Mutable access to `metadata.task`, creating/coercing the object if a
/// hand-edited row lost it.
fn task_obj_mut(payload: &mut MemoryPayload) -> &mut Map<String, Value> {
    if !payload.metadata.is_object() {
        payload.metadata = json!({});
    }
    let meta = payload
        .metadata
        .as_object_mut()
        .expect("metadata was just coerced to an object");
    if !meta.get("task").map(|t| t.is_object()).unwrap_or(false) {
        meta.insert("task".to_string(), json!({}));
    }
    meta.get_mut("task")
        .and_then(Value::as_object_mut)
        .expect("task was just coerced to an object")
}

fn priority_of(payload: &MemoryPayload) -> i64 {
    payload
        .metadata
        .get("task")
        .and_then(|t| t.get("priority"))
        .and_then(|p| p.as_i64())
        // Missing priority reads as the default, same as the gate does.
        .unwrap_or(2)
}

fn blocked_by_of(payload: &MemoryPayload) -> Vec<String> {
    payload
        .metadata
        .get("task")
        .and_then(|t| t.get("blocked_by"))
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn task_statuses(memories: &[SearchResult]) -> HashMap<String, Status> {
    let now = chrono::Utc::now().timestamp();
    let retired: HashSet<&str> = memories
        .iter()
        .filter(|m| gate::is_task(m) && gate::is_retired(&m.payload.metadata, now))
        .map(|m| m.id.as_str())
        .collect();
    let mut statuses: HashMap<String, Status> = memories
        .iter()
        .filter(|m| gate::is_task(m))
        .map(|m| (m.id.clone(), gate::task_status(m)))
        .collect();
    // A retired blocker is history, but the work it stood for lives on in the
    // row that replaced it: a live unfinished successor still holds its
    // dependents. The successor is the row at the end of the supersession
    // chain, not merely the next link -- a work that was replaced twice is
    // still in flight under the row that replaced it twice. A retired blocker
    // with no live successor is void -- the dependency named a record that no
    // longer exists.
    for m in memories.iter().filter(|m| retired.contains(m.id.as_str())) {
        let successor = gate::live_successor(&m.payload.metadata, memories, now)
            .and_then(|s| statuses.get(&s.id).copied());
        statuses.insert(m.id.clone(), successor.unwrap_or(Status::Done));
    }
    statuses
}

/// `ready` (section 3.2): open AND every blocker done. A blocker that is
/// missing cannot be done, so the task stays unready -- conservative by
/// default rather than optimistically actionable. A retired blocker counts
/// as done: it is history, and history resolves nothing further.
fn is_ready(payload: &MemoryPayload, statuses: &HashMap<String, Status>) -> bool {
    blocked_by_of(payload)
        .iter()
        .all(|dep| statuses.get(dep) == Some(&Status::Done))
}

/// The base record every tool returns alongside its own fields.
pub fn task_summary(id: &str, payload: &MemoryPayload) -> Value {
    let task = payload.metadata.get("task");
    let field = |key: &str| {
        task.and_then(|t| t.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let status = task
        .and_then(|t| t.get("status"))
        .and_then(|s| s.as_str())
        .and_then(Status::parse)
        .unwrap_or(Status::Open);
    json!({
        "id": id,
        "title": payload.content,
        "status": status.as_str(),
        "priority": field("priority"),
        "task_type": field("task_type"),
        "assignee": field("assignee"),
        "session_id": field("session_id"),
        "branch": field("branch"),
        "worktree": field("worktree"),
        "labels": field("labels"),
        "blocked_by": field("blocked_by"),
        "description": field("description"),
        "close_reason": field("close_reason"),
        "created_at": field("created_at"),
        "updated_at": field("updated_at"),
        "history": field("history"),
    })
}

async fn load_task(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    id: &str,
) -> Result<(Task, Vec<f32>), String> {
    match store.get(namespace, id).await {
        Ok(Some((vector, payload))) => {
            if payload.memory_type != "task" {
                return Err(format!(
                    "ERROR: '{}' is a {} memory, not a task. Task tools only act on memory_type 'task' rows.",
                    id, payload.memory_type
                ));
            }
            Ok((Task { id: id.to_string(), payload }, vector))
        }
        Ok(None) => Err(format!(
            "ERROR: no task with id '{}' in namespace '{}'. List the work with neurostrata_task_list; ids look like '{}-xxxx'.",
            id,
            namespace,
            ns_key(namespace)
        )),
        Err(e) => Err(format!("Failed to read task '{}': {}", id, e)),
    }
}

async fn save_task(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    id: &str,
    vector: Vec<f32>,
    payload: MemoryPayload,
) -> Result<(), String> {
    store
        .init(namespace)
        .await
        .map_err(|_| "Failed to initialize table.".to_string())?;
    store
        .upsert(namespace, id, vector, payload)
        .await
        .map(|_| ())
        .map_err(|e| format!("Failed to store task '{}': {}", id, e))
}

/// Pure staging helper for kinetic `TransitionTask` effects.
/// Never writes to storage and unconditionally forbids transitioning to `done` (Lock 2).
pub fn stage_task_transition_at(
    id: &str,
    payload: &MemoryPayload,
    new_status: &str,
    note: &str,
    actor: &str,
    session: &str,
    now_ts: i64,
) -> Result<(String, String, MemoryPayload), String> {
    let target_status = machine::Status::parse(new_status)
        .ok_or_else(|| format!("invalid task status '{new_status}'"))?;
    let task = machine::Task {
        id: id.to_string(),
        payload: payload.clone(),
    };
    let from_status = task.status();
    let kernel_req = match target_status {
        machine::Status::Open => crate::formal_kernel::ActionTaskTargetStatus::Open,
        machine::Status::InProgress => crate::formal_kernel::ActionTaskTargetStatus::InProgress,
        machine::Status::Blocked => crate::formal_kernel::ActionTaskTargetStatus::Blocked,
        machine::Status::Done => crate::formal_kernel::ActionTaskTargetStatus::Done,
    };
    match crate::formal_kernel::evaluate_action_task_effect(from_status.to_kernel(), kernel_req) {
        Ok(_) => {}
        Err(crate::formal_kernel::ActionTaskEffectError::DoneRequiresComplete) => {
            return Err(
                "TransitionTask cannot transition to 'done' (Lock 2 requires neurostrata_task_complete)"
                    .to_string(),
            );
        }
        Err(crate::formal_kernel::ActionTaskEffectError::IllegalTransition) => {
            return Err(format!(
                "illegal task transition: '{}' -> '{}'",
                from_status.as_str(),
                target_status.as_str()
            ));
        }
    }
    let ctx = machine::Ctx {
        reason: note.to_string(),
        extraction_edge_exists: false,
        actor: actor.to_string(),
        session: session.to_string(),
    };
    let mut next_task = if from_status == target_status {
        if note.trim().is_empty() {
            return Err("same-status task transition requires a non-empty note".to_string());
        }
        machine::annotate(&task, &ctx)
    } else {
        machine::apply(&task, target_status, &ctx).map_err(|e| e.to_string())?
    };

    if let Some(task_obj) = next_task
        .payload
        .metadata
        .get_mut("task")
        .and_then(|t| t.as_object_mut())
    {
        task_obj.insert("updated_at".to_string(), serde_json::json!(now_ts));
        if let Some(history) = task_obj.get_mut("history").and_then(|h| h.as_array_mut()) {
            if let Some(last) = history.last_mut().and_then(|e| e.as_object_mut()) {
                last.insert("at".to_string(), serde_json::json!(now_ts));
            }
        }
    }

    Ok((
        from_status.as_str().to_string(),
        target_status.as_str().to_string(),
        next_task.payload,
    ))
}

/// Writes a task row under an already-allocated id. Every creation path ends
/// here so ids, timestamps, and shape look the same however a task was born.
#[allow(clippy::too_many_arguments)]
async fn write_task_row(
    store: &Arc<dyn VectorStore>,
    emb: &Arc<dyn Embedder>,
    namespace: &str,
    id: &str,
    content: &str,
    user_id: &str,
    agent_name: Option<&str>,
    mut task_meta: Map<String, Value>,
    top_level: Map<String, Value>,
) -> Result<MemoryPayload, String> {
    store
        .init(namespace)
        .await
        .map_err(|_| "Failed to initialize table.".to_string())?;
    let vector = emb
        .embed(content)
        .await
        .map_err(|e| format!("Failed to embed the task title: {}", e))?;

    let now = machine::timestamp();
    task_meta.entry("status").or_insert_with(|| json!("open"));
    task_meta.entry("history").or_insert_with(|| json!([]));
    task_meta.entry("created_at").or_insert_with(|| json!(now));
    task_meta.entry("updated_at").or_insert_with(|| json!(now));

    let mut metadata = Map::new();
    metadata.insert("task".to_string(), Value::Object(task_meta));
    for (key, value) in top_level {
        metadata.insert(key, value);
    }

    let payload = MemoryPayload {
        content: content.to_string(),
        user_id: user_id.to_string(),
        memory_type: "task".to_string(),
        agent_name: agent_name.map(|s| s.to_string()),
        location: String::new(),
        location_lines: String::new(),
        metadata: Value::Object(metadata),
    };
    store
        .upsert(namespace, id, vector, payload.clone())
        .await
        .map_err(|e| format!("Failed to store task '{}': {}", id, e))?;
    Ok(payload)
}

/// Allocates a collision-free id and writes the task.
#[allow(clippy::too_many_arguments)]
async fn store_new_task(
    store: &Arc<dyn VectorStore>,
    emb: &Arc<dyn Embedder>,
    namespace: &str,
    content: &str,
    user_id: &str,
    agent_name: Option<&str>,
    task_meta: Map<String, Value>,
    top_level: Map<String, Value>,
) -> Result<(String, MemoryPayload), String> {
    // Listing is what finds id collisions, and listing needs the schema.
    store
        .init(namespace)
        .await
        .map_err(|_| "Failed to initialize table.".to_string())?;
    let existing = existing_ids(store, namespace).await?;
    let key = ns_key(namespace);
    let id = allocate_task_id(&key, &existing).ok_or_else(|| {
        format!(
            "ERROR: could not allocate a task id under '{}' after {} attempts.",
            key, ID_ATTEMPTS
        )
    })?;
    let payload = write_task_row(
        store, emb, namespace, &id, content, user_id, agent_name, task_meta, top_level,
    )
    .await?;
    Ok((id, payload))
}

/// Memory types treated as normative (governing constraints) for reserved slots.
const NORMATIVE_MEMORY_TYPES: &[&str] =
    &["rule", "fact", "decision", "procedure", "thesis", "concept"];
/// Memory types treated as structural evidence anchors.
const STRUCTURAL_MEMORY_TYPES: &[&str] = &["code_ast", "doc_section"];
/// Max normative memories reserved in `auto_retrieved_context`.
const MAX_NORMATIVE_MEMORIES: usize = 4;
/// Max structural anchors in `auto_retrieved_context`.
const MAX_STRUCTURAL_ANCHORS: usize = 3;
/// Max cross-domain conceptual bridges emitted per task context.
const MAX_CONCEPTUAL_BRIDGES: usize = 2;
const MAX_RELATED_SEEDS: usize = 5;
const MAX_RELATED_EDGES: usize = 15;
/// Relations traversed when synthesising 2-hop conceptual bridges.
const BRIDGE_RELATIONS: &[&str] = &[
    "GOVERNS",
    "CONTAINS",
    "RELATES_TO",
    "EXTRACTED_FROM",
    "DEPENDS_ON",
];

fn is_normative_memory(memory_type: &str) -> bool {
    NORMATIVE_MEMORY_TYPES.contains(&memory_type)
}

fn is_structural_memory(memory_type: &str) -> bool {
    STRUCTURAL_MEMORY_TYPES.contains(&memory_type)
}

/// True when a memory row has `metadata.pinned == true`.
pub fn is_pinned_memory(metadata: &Value) -> bool {
    metadata
        .get("pinned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// True when content opens with a canonical rule prefix (covering legacy global
/// rules that were stored with `memory_type: "fact"`).
pub fn is_rule_prefixed_content(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("RULE:")
        || trimmed.starts_with("RULE (")
        || trimmed.starts_with("Project Rule:")
}

fn is_active_golden_candidate(r: &SearchResult) -> bool {
    if r.payload.memory_type == "task"
        || r.payload.memory_type == "community_summary"
        || crate::registry::is_registry_row(&r.payload.metadata)
        || crate::handlers::archive_memory::is_archived(&r.payload)
        || r.payload.metadata.get("superseded_by").is_some()
        || r.payload
            .metadata
            .get("valid_to")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    {
        return false;
    }
    if r.payload.memory_type == "guard_rule" {
        return r
            .payload
            .metadata
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active")
            != "deprecated";
    }
    true
}

/// Collect Golden Rules across the `"global"` namespace and the target
/// `namespace`. In `"global"`, any active rule, `guard_rule`, `pinned: true`
/// memory, or rule-prefixed (`RULE:` / `RULE (` / `Project Rule:`) memory
/// qualifies. In a project namespace, any active `pinned: true` memory
/// qualifies.
pub async fn collect_golden_rules(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    local_memories: Option<&[SearchResult]>,
) -> Vec<SearchResult> {
    let mut out: Vec<SearchResult> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    let global_rows: Vec<SearchResult> = if namespace == "global" {
        match local_memories {
            Some(slice) => slice.to_vec(),
            None => store.list("global", None, None).await.unwrap_or_default(),
        }
    } else {
        store.list("global", None, None).await.unwrap_or_default()
    };

    for r in global_rows {
        if !is_active_golden_candidate(&r) {
            continue;
        }
        let qualifies = is_pinned_memory(&r.payload.metadata)
            || r.payload.memory_type == "rule"
            || r.payload.memory_type == "guard_rule"
            || is_rule_prefixed_content(&r.payload.content);
        if qualifies && seen_ids.insert(r.id.clone()) {
            out.push(r);
        }
    }

    if namespace != "global" {
        let proj_rows: Vec<SearchResult> = match local_memories {
            Some(slice) => slice.to_vec(),
            // Only pinned rows qualify here, so ask the store for those.
            None => store.list_pinned(namespace).await.unwrap_or_default(),
        };
        for r in proj_rows {
            if !is_active_golden_candidate(&r) {
                continue;
            }
            if is_pinned_memory(&r.payload.metadata) && seen_ids.insert(r.id.clone()) {
                out.push(r);
            }
        }
    }

    out.sort_by(|a, b| {
        let a_pin = is_pinned_memory(&a.payload.metadata);
        let b_pin = is_pinned_memory(&b.payload.metadata);
        let a_not_guard = a.payload.memory_type != "guard_rule";
        let b_not_guard = b.payload.memory_type != "guard_rule";
        let a_cnt = a
            .payload
            .metadata
            .get("access_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let b_cnt = b
            .payload
            .metadata
            .get("access_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        b_pin
            .cmp(&a_pin)
            .then_with(|| b_not_guard.cmp(&a_not_guard))
            .then_with(|| b_cnt.cmp(&a_cnt))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen_content: HashSet<String> = HashSet::new();
    out.retain(|r| seen_content.insert(r.payload.content.trim().to_string()));
    out
}

/// Render the Golden Rules block for `neurostrata_get_snapshot`.
pub fn format_golden_rules_section(golden_rules: &[SearchResult]) -> String {
    if golden_rules.is_empty() {
        return String::new();
    }
    let mut out = String::from("### Golden Rules (Global & Pinned — ALWAYS ENFORCED):");
    for r in golden_rules {
        let snippet: String = r.payload.content.chars().take(280).collect();
        out.push_str(&format!(
            "\n  - [{}] `{}`: {}",
            r.payload.memory_type,
            r.id,
            snippet.replace('\n', " ")
        ));
    }
    out
}

/// Best-effort persistence of `.NeuroStrata/active_session.json` so harness
/// plugins can inject Golden Rules and active task context on session startup
/// (`sessionContextHandler`) even before the first tool call.
pub fn persist_active_session_file(
    project_root: &std::path::Path,
    namespace: &str,
    golden_rules: &[SearchResult],
    active_task: Option<(&str, &MemoryPayload)>,
) {
    persist_active_session_file_with_home(
        project_root,
        namespace,
        golden_rules,
        active_task,
        &session_home_dirs(),
    );
}

fn persist_active_session_file_with_home(
    project_root: &std::path::Path,
    namespace: &str,
    golden_rules: &[SearchResult],
    active_task: Option<(&str, &MemoryPayload)>,
    homes: &[std::path::PathBuf],
) {
    if is_home_without_git_with(project_root, homes) {
        return;
    }
    let ns_dir = project_root.join(".NeuroStrata");
    let session_path = ns_dir.join("active_session.json");
    let mut active: Map<String, Value> = std::fs::read_to_string(&session_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    active.insert("namespace".to_string(), json!(namespace));
    let golden_strings: Vec<String> = golden_rules
        .iter()
        .map(|r| {
            let snippet: String = r.payload.content.chars().take(280).collect();
            format!(
                "[{}] {}: {}",
                r.payload.memory_type,
                r.id,
                snippet.replace('\n', " ")
            )
        })
        .collect();
    active.insert("golden_rules".to_string(), json!(golden_strings));

    if let Some((tid, payload)) = active_task {
        active.insert("task_id".to_string(), json!(tid));
        let task_obj = payload.metadata.get("task");
        let status = task_obj
            .and_then(|t| t.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("in_progress");
        active.insert("status".to_string(), json!(status));
        if let Some(sid) = task_obj
            .and_then(|t| t.get("session_id"))
            .and_then(|v| v.as_str())
        {
            active.insert("session_id".to_string(), json!(sid));
        }
        if let Some(inj) = task_obj
            .and_then(|t| t.get("subagent_prompt_injection"))
            .and_then(|v| v.as_str())
        {
            active.insert("subagent_prompt_injection".to_string(), json!(inj));
        }
        active.insert("updated_at".to_string(), json!(machine::timestamp()));
    }

    if std::fs::create_dir_all(&ns_dir).is_ok() {
        if let Ok(serialized) = serde_json::to_string_pretty(&Value::Object(active)) {
            let _ = std::fs::write(&session_path, serialized);
        }
    }
}

/// The single rendered prompt used for BOTH the budget measurement and the
/// final output, so a prompt that measures under 3,500 chars cannot grow past
/// it in the copy that is actually emitted. Golden Rule ids/never dropped:
/// shedding only tightens their snippet after hits/bridges/taxonomy are gone.
fn render_subagent_prompt(
    namespace: &str,
    task_id: &str,
    goal: &str,
    golden_lines: &[String],
    kept_hits: &[String],
    bridges_prompt: &str,
    taxonomy_prompt: &str,
) -> String {
    let mut combined: Vec<String> = golden_lines.to_vec();
    combined.extend(kept_hits.iter().cloned());
    let hits_block = if combined.is_empty() {
        "  - (no pre-existing memories matched; run neurostrata_search_memory / neurostrata_memories_for_file as you explore)".to_string()
    } else {
        combined.join("\n")
    };
    format!(
        "### NeuroStrata Task & Graph Context (PASTE INTO ALL SUBAGENT PROMPTS)\n\
         - Namespace: `{}` | Task ID: `{}` | Goal: {}\n\
         - Auto-Retrieved Governing Rules & Graph Nodes:\n{}{}{}\n\
         - Mandatory Graph-First Lookup Rule: Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed under `### NeuroStrata Task & Graph Context` in your prompt. For any file you touch that is NOT listed in that block, call `neurostrata_memories_for_file` first (or `neurostrata_related` / `neurostrata_search_memory` with `mode`: `hybrid` (default) + `include_references`: true / `neurostrata_sparql_query`).\n\
         - Formal Verification Reflex: When adding or modifying a state machine, gate, sovereignty/ACL check, CRDT/merge rule, lease/claim rule, or conservation invariant, evaluate whether to extract the pure decision into `src/formal_kernel.rs`, prove its theorems in `proofs/NeuroStrata/*.lean` (zero `sorry`/`axiom`), and extend `tests/lean_parity_test.rs`.",
        namespace, task_id, goal, hits_block, bridges_prompt, taxonomy_prompt
    )
}

/// Rebuild the Golden Rule prompt lines at a shorter snippet length. Every
/// rule keeps its id and its line — only the snippet shrinks (240 -> 60 min).
fn golden_prompt_lines(golden_rules: &[SearchResult], snippet_len: usize) -> Vec<String> {
    golden_rules
        .iter()
        .map(|r| {
            let snippet: String = r.payload.content.chars().take(snippet_len).collect();
            let presented = crate::rdf::provenance::decorate_content(&snippet, &r.payload.metadata);
            format!(
                "  - [GOLDEN {}] `{}`: {}",
                r.payload.memory_type,
                r.id,
                presented.replace('\n', " ")
            )
        })
        .collect()
}

/// Map task metadata into the canonical R4 `TaskContext` so
/// `routing::evaluate_triggers` has a production caller at task-context build
/// time. Absent/neutral metadata fires nothing (honest abstention).
fn task_routing_triggers(
    namespace: &str,
    metadata: &Value,
) -> Vec<crate::task::routing::RouteTrigger> {
    let flag = |key: &str| metadata.get(key).and_then(Value::as_bool).unwrap_or(false);
    crate::task::routing::evaluate_triggers(crate::task::routing::TaskContext {
        namespace: namespace.to_string(),
        known_path: None,
        ambiguous_query: None,
        blast_radius_count: 0,
        blast_id: None,
        staged_file_count: 0,
        diff_text: None,
        git_ref: None,
        task_type: metadata
            .get("task_type")
            .and_then(Value::as_str)
            .unwrap_or("task")
            .to_string(),
        design_review: flag("design_review"),
        strategic: flag("strategic"),
        cross_row_invariant: flag("cross_row_invariant"),
        invariant_query: None,
    })
}

/// Pure R4 seed-expansion leg. Only a seed with at least one real related
/// edge earns a `T_SEED_EXPAND` next hop; a seed with no edges abstains
/// honestly. The id is deliberately distinct from `T_SEED_EDGE`, which is the
/// `object_view` prescription emitted by `memories_for_file` for conceptual
/// bridges — the two response-derived seed triggers must never cross-resolve.
fn seed_expand_next_hops(namespace: &str, seeds: &[String], edges: &[Value]) -> Vec<Value> {
    let mut actions = Vec::new();
    for seed in seeds.iter().take(3) {
        if seed.trim().is_empty() {
            continue;
        }
        let has_edge = edges.iter().any(|edge| {
            edge.get("source").and_then(Value::as_str) == Some(seed.as_str())
                || edge.get("target").and_then(Value::as_str) == Some(seed.as_str())
        });
        if !has_edge {
            continue;
        }
        let trigger = crate::task::routing::RouteTrigger {
            id: "T_SEED_EXPAND",
            tool: "neurostrata_related",
            args: json!({
                "namespace": namespace,
                "id": seed,
                "max_hops": 2,
                "followed_trigger_id": "T_SEED_EXPAND"
            }),
            reason: "Seed has a real related edge; expand two hops before editing.".to_string(),
        };
        actions.push(crate::task::routing::next_action(&trigger, &[seed.clone()]));
        if actions.len() >= 3 {
            break;
        }
    }
    actions
}

/// Auto-retrieve relevant governing rules, memories, code symbols, and 1-hop
/// graph edges for a task, and format a ready-to-paste subagent prompt block.
pub async fn build_task_graph_context(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    task_id: &str,
    vector: &[f32],
    payload: &MemoryPayload,
) -> (Value, String) {
    let golden_rules = collect_golden_rules(store, namespace, None).await;
    let mut golden_json = Vec::new();
    let mut golden_lines = golden_prompt_lines(&golden_rules, 240);
    let mut golden_snippet_len = 240usize;
    let mut seen_ids: HashSet<String> = HashSet::new();

    for r in &golden_rules {
        seen_ids.insert(r.id.clone());
        let snippet: String = r.payload.content.chars().take(240).collect();
        let presented = crate::rdf::provenance::decorate_content(&snippet, &r.payload.metadata);
        let mut entry = json!({
            "id": r.id,
            "memory_type": r.payload.memory_type,
            "location": r.payload.location,
            "snippet": presented,
            "metadata": r.payload.metadata,
        });
        crate::rdf::provenance::attach_presentation(&mut entry, &r.payload.metadata, "snippet");
        golden_json.push(entry);
    }

    // Hybrid seed selection (R2.1): use the hybrid search path rather than raw
    // vector `store.search`, so graph-expanded IDs are available as seeds.
    let raw_hits = if let Some(lb) = VectorStore::as_any(&**store).downcast_ref::<LadybugStore>() {
        lb.search_hybrid_with_domain(
            namespace,
            vector.to_vec(),
            12,
            false,
            false,
            true,
            &[],
            Some(&payload.content),
            false,
            crate::traits::SearchMode::Hybrid,
        )
        .await
        .map(|o| o.results)
        .unwrap_or_default()
    } else {
        store
            .search(namespace, vector.to_vec(), 12, false, false)
            .await
            .unwrap_or_default()
    };
    let mut hits_json = Vec::new();
    let mut hit_lines: Vec<String> = Vec::new();
    for r in raw_hits {
        if r.id == task_id
            || seen_ids.contains(&r.id)
            || r.payload.memory_type == "task"
            || crate::registry::is_registry_row(&r.payload.metadata)
            || crate::handlers::archive_memory::is_archived(&r.payload)
        {
            continue;
        }
        let snippet: String = r.payload.content.chars().take(140).collect();
        let presented = crate::rdf::provenance::decorate_content(&snippet, &r.payload.metadata);
        let calls = r
            .payload
            .metadata
            .get("calls")
            .cloned()
            .unwrap_or(Value::Null);
        let imports = r
            .payload
            .metadata
            .get("imports")
            .cloned()
            .unwrap_or(Value::Null);
        let governs = r
            .payload
            .metadata
            .get("governs")
            .cloned()
            .unwrap_or(Value::Null);
        let depends_on = r
            .payload
            .metadata
            .get("depends_on")
            .cloned()
            .unwrap_or(Value::Null);
        hit_lines.push(format!(
            "  - [{}] `{}` ({}): {}",
            r.payload.memory_type,
            r.id,
            if r.payload.location.is_empty() {
                "memory"
            } else {
                &r.payload.location
            },
            presented.replace('\n', " ")
        ));
        let mut entry = json!({
            "id": r.id,
            "memory_type": r.payload.memory_type,
            "location": r.payload.location,
            "snippet": presented,
            "metadata": r.payload.metadata,
            "calls": calls,
            "imports": imports,
            "governs": governs,
            "depends_on": depends_on,
            "evidence": r.evidence,
        });
        crate::rdf::provenance::attach_presentation(&mut entry, &r.payload.metadata, "snippet");
        hits_json.push(entry);
        if hits_json.len() >= 5 {
            break;
        }
    }
    let mut kept_hits = hit_lines.clone();
    // Partition the retrieved items into reserved normative memories and structural anchors.
    let normative_memories: Vec<Value> = hits_json
        .iter()
        .filter(|h| {
            is_normative_memory(h.get("memory_type").and_then(|v| v.as_str()).unwrap_or(""))
        })
        .take(MAX_NORMATIVE_MEMORIES)
        .cloned()
        .collect();
    let structural_anchors: Vec<Value> = hits_json
        .iter()
        .filter(|h| {
            is_structural_memory(h.get("memory_type").and_then(|v| v.as_str()).unwrap_or(""))
        })
        .take(MAX_STRUCTURAL_ANCHORS)
        .cloned()
        .collect();

    // Synthesise up to MAX_CONCEPTUAL_BRIDGES cross-domain 2-hop bridges from the
    // retrieved seed memories to normative targets in a disjoint domain.
    let conceptual_bridges = synthesize_conceptual_bridges(store, namespace, &hits_json).await;

    // Context-budgeted (<= 1200 chars) unrestricted Project Thesis & matched SKOS concepts (`neurostrata-f0bn`).
    let (project_thesis, domain_concepts, mut taxonomy_prompt) =
        if let Some(lb) = VectorStore::as_any(&**store).downcast_ref::<LadybugStore>() {
            crate::rdf::taxonomy::build_task_taxonomy_context(lb, namespace, &payload.content).await
        } else {
            (Value::Null, Value::Array(Vec::new()), String::new())
        };

    // R2.2/R2.3: collect at most MAX_RELATED_EDGES one-hop edges from at most
    // MAX_RELATED_SEEDS seed items, plus metadata-declared AST calls/imports.
    let mut related_edges: Vec<Value> = Vec::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
    let mut expansion_error: Option<String> = None;

    // Metadata-declared edges (GOVERNS / DEPENDS_ON / calls / imports).
    for h in &hits_json {
        let sid = h.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if sid.is_empty() {
            continue;
        }
        let meta = h.get("metadata").cloned().unwrap_or(Value::Null);
        let pairs: &[(&str, &str)] = &[
            ("governs", "GOVERNS"),
            ("depends_on", "DEPENDS_ON"),
            ("calls", "calls"),
            ("imports", "imports"),
        ];
        for (key, rel) in pairs {
            if let Some(arr) = meta.get(*key).and_then(|v| v.as_array()) {
                for t in arr {
                    if let Some(tid) = t.as_str() {
                        let edge_key = (sid.to_string(), rel.to_string(), tid.to_string());
                        if seen_edges.insert(edge_key) && related_edges.len() < MAX_RELATED_EDGES {
                            related_edges.push(json!({
                                "source": sid,
                                "relation": rel,
                                "target": tid,
                            }));
                        }
                    }
                }
            }
        }
    }

    // Preserve the count of metadata-declared edges so a Ladybug traversal
    // failure can drop only Ladybug-sourced edges, not provenance.
    let metadata_edge_count = related_edges.len();

    // 1-hop graph edges via LadybugStore::related (bounded).
    let seeds: Vec<String> = hits_json
        .iter()
        .filter_map(|h| h.get("id").and_then(|v| v.as_str()).map(String::from))
        .take(MAX_RELATED_SEEDS)
        .collect();
    if let Some(lb) = VectorStore::as_any(&**store).downcast_ref::<LadybugStore>() {
        let relations: Vec<String> = vec![
            "GOVERNS".to_string(),
            "DEPENDS_ON".to_string(),
            "CONTAINS".to_string(),
            "RELATES_TO".to_string(),
        ];
        let policy = crate::graph_read::traversal::CrossNamespacePolicy::default();
        for seed in &seeds {
            if related_edges.len() >= MAX_RELATED_EDGES {
                break;
            }
            match lb
                .related(
                    namespace,
                    seed,
                    &relations,
                    crate::graph_read::related::Direction::Both,
                    1,
                    &[],
                    MAX_RELATED_EDGES,
                    None,
                    &policy,
                )
                .await
            {
                Ok(res) => {
                    for e in res.edges {
                        if related_edges.len() >= MAX_RELATED_EDGES {
                            break;
                        }
                        let edge_key = (e.source.clone(), e.relation.clone(), e.target.clone());
                        if seen_edges.insert(edge_key) {
                            related_edges.push(json!({
                                "source": e.source,
                                "relation": e.relation,
                                "target": e.target,
                            }));
                        }
                    }
                }
                Err(err) => {
                    related_edges.truncate(metadata_edge_count);
                    expansion_error = Some(err.to_string());
                    break;
                }
            }
        }
    }

    // next_hop_calls: <=3 copy-pasteable related calls derived from returned
    // IDs. `T_SEED_EXPAND` is the task-context leg; it fires only for seeds
    // with a real related edge and never claims a blast-radius signal.
    let mut next_hop_calls = seed_expand_next_hops(namespace, &seeds, &related_edges);
    // Task-level R4 triggers give `routing::evaluate_triggers` a production
    // caller. Default task metadata fires nothing, so existing context tests
    // are unchanged; the overall cap stays at three actions (§5).
    let task_triggers = task_routing_triggers(namespace, &payload.metadata);
    let lookup_ids: Vec<String> = hits_json
        .iter()
        .filter_map(|h| h.get("id").and_then(Value::as_str).map(String::from))
        .collect();
    for trigger in task_triggers {
        if next_hop_calls.len() >= 3 {
            break;
        }
        next_hop_calls.push(crate::task::routing::next_action(&trigger, &lookup_ids));
    }

    let expansion_error_value = expansion_error.map(Value::String).unwrap_or(Value::Null);

    let auto_retrieved_context = json!({
        "query": payload.content,
        "golden_rules": golden_json,
        "items": hits_json,
        "related_edges": related_edges,
        "next_hop_calls": next_hop_calls,
        "expansion_error": expansion_error_value,
        "normative_memories": normative_memories,
        "structural_anchors": structural_anchors,
        "conceptual_bridges": conceptual_bridges,
        "project_thesis": project_thesis,
        "domain_concepts": domain_concepts,
    });

    let mut bridges_prompt = conceptual_bridges
        .as_array()
        .map(|arr| {
            if arr.is_empty() {
                String::new()
            } else {
                let lines: Vec<String> = arr
                    .iter()
                    .filter_map(|b| {
                        b.get("why_connected")
                            .and_then(|v| v.as_str())
                            .map(|s| format!("  - {s}"))
                    })
                    .collect();
                format!("\n### Cross-Domain Bridges\n{}", lines.join("\n"))
            }
        })
        .unwrap_or_default();

    // Full-prompt budget enforcement (Task 13 P1-9): one renderer for both
    // measuring and output. Cap the Goal line, then shed in R2.4 order.
    let goal: String = payload.content.chars().take(240).collect();
    loop {
        let candidate = render_subagent_prompt(
            namespace,
            task_id,
            &goal,
            &golden_lines,
            &kept_hits,
            &bridges_prompt,
            &taxonomy_prompt,
        );
        if candidate.chars().count() <= 3500 {
            break;
        }
        if !kept_hits.is_empty() {
            kept_hits.pop();
        } else if !bridges_prompt.is_empty() {
            bridges_prompt = String::new();
        } else if !taxonomy_prompt.is_empty() {
            taxonomy_prompt = String::new();
        } else if golden_snippet_len > 60 {
            golden_snippet_len = std::cmp::max(60, golden_snippet_len.saturating_sub(30));
            golden_lines = golden_prompt_lines(&golden_rules, golden_snippet_len);
        } else {
            break;
        }
    }
    let mut subagent_prompt = render_subagent_prompt(
        namespace,
        task_id,
        &goal,
        &golden_lines,
        &kept_hits,
        &bridges_prompt,
        &taxonomy_prompt,
    );
    // Review IMPORTANT-3: an extreme Golden Rule load can still exceed 3,500
    // after every shed stage. Golden Rule ids/lines are emitted first in the
    // hits block, so a char-boundary-safe prefix cap keeps as many as fit and
    // never splits a UTF-8 scalar. The assert below stays as a postcondition.
    if subagent_prompt.chars().count() > 3500 {
        subagent_prompt = subagent_prompt.chars().take(3500).collect();
    }
    assert!(
        subagent_prompt.chars().count() <= 3500,
        "rendered subagent prompt exceeded the 3,500-char bound"
    );
    (auto_retrieved_context, subagent_prompt)
}

/// Synthesise cross-domain conceptual bridges from retrieved seed memories.
///
/// For each seed item, traverses 2 hops (`seed -> via -> target`) through the
/// graph and keeps up to `MAX_CONCEPTUAL_BRIDGES` targets whose memory type is
/// normative and whose domain is disjoint from the seed's domain. Every bridge
/// carries a concrete `why_connected` path explanation. Fail-soft: a traversal
/// error yields no bridges for that seed rather than failing the whole context.
async fn synthesize_conceptual_bridges(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    items: &[Value],
) -> Value {
    let mut bridges: Vec<Value> = Vec::new();
    let mut seen_targets: HashSet<String> = HashSet::new();
    // `related` lives on `GraphRead`, which only `LadybugStore` implements among
    // the `VectorStore` implementors; other store types yield no bridges.
    let Some(ladybug) = VectorStore::as_any(&**store).downcast_ref::<LadybugStore>() else {
        return Value::Array(Vec::new());
    };
    let relations: Vec<String> = BRIDGE_RELATIONS.iter().map(|s| s.to_string()).collect();
    let policy = crate::graph_read::traversal::CrossNamespacePolicy::default();
    for item in items {
        if bridges.len() >= MAX_CONCEPTUAL_BRIDGES {
            break;
        }
        let seed_id = match item.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let seed_metadata = item.get("metadata").unwrap_or(&Value::Null);
        let seed_location = item.get("location").and_then(|v| v.as_str()).unwrap_or("");
        let seed_tokens = feature_tokens(seed_metadata, seed_location);
        if let Ok(result) = ladybug
            .related(
                namespace,
                &seed_id,
                &relations,
                crate::graph_read::related::Direction::Both,
                2,
                &[],
                50,
                None,
                &policy,
            )
            .await
        {
            // hop1_ids: the via nodes reached from the seed (frontier depth 0).
            let mut hop1_ids: HashSet<String> = HashSet::new();
            for edge in &result.edges {
                if edge.depth == 0 {
                    if edge.source != seed_id {
                        hop1_ids.insert(edge.source.clone());
                    }
                    if edge.target != seed_id {
                        hop1_ids.insert(edge.target.clone());
                    }
                }
            }
            for edge in &result.edges {
                if bridges.len() >= MAX_CONCEPTUAL_BRIDGES {
                    break;
                }
                // frontier depth 1 edge: via -> target (seed -> via has frontier depth 0).
                if edge.depth != 1 {
                    continue;
                }
                let a = edge.source.as_str();
                let b = edge.target.as_str();
                // The via is whichever endpoint sits at hop 1; the target is the
                // other endpoint (the edge may be stored in either orientation
                // under Direction::Both, so do not assume edge.source is the via).
                let (via_id, target_id) =
                    if hop1_ids.contains(a) && b != seed_id && !hop1_ids.contains(b) {
                        (a, b)
                    } else if hop1_ids.contains(b) && a != seed_id && !hop1_ids.contains(a) {
                        (b, a)
                    } else {
                        continue;
                    };
                if seen_targets.contains(target_id) {
                    continue;
                }
                let target_node = match result.nodes.iter().find(|n| n.id == target_id) {
                    Some(n) => n,
                    None => continue,
                };
                if !is_normative_memory(&target_node.memory_type) {
                    continue;
                }
                // Fetch the target row so its full metadata (domain, tags,
                // governs, location) is available for token extraction.
                let target_metadata =
                    if let Ok(Some((_, payload))) = ladybug.get(namespace, target_id).await {
                        payload.metadata
                    } else {
                        Value::Null
                    };
                let target_location = target_metadata
                    .get("location")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut target_tokens: HashSet<String> = HashSet::new();
                if let Some(domain) = &target_node.domain {
                    target_tokens.insert(domain.clone());
                }
                for t in crate::domain::domain_keys(&target_metadata) {
                    target_tokens.insert(t);
                }
                target_tokens.extend(feature_tokens(&target_metadata, target_location));
                // Cross-domain filter: require jaccard < 0.35 when the target
                // carries any tokens; an empty target token set is not a filter.
                if !target_tokens.is_empty() {
                    let sim = jaccard_similarity(&seed_tokens, &target_tokens);
                    if sim >= 0.35 {
                        continue;
                    }
                }
                seen_targets.insert(target_id.to_string());
                let why_connected = format!("{} -> {} -> {}", seed_id, via_id, target_id);
                bridges.push(json!({
                    "id": target_node.id,
                    "memory_type": target_node.memory_type,
                    "content": target_node.snippet,
                    "why_connected": why_connected,
                }));
            }
        }
    }
    Value::Array(bridges)
}

/// Build the feature token set for a memory from its domain slugs, tags, and
/// top-level path prefixes of its `location` and `governs` entries.
pub(crate) fn feature_tokens(metadata: &Value, location: &str) -> HashSet<String> {
    let mut tokens: HashSet<String> = HashSet::new();
    for d in crate::domain::domain_keys(metadata) {
        tokens.insert(d);
    }
    if let Some(tags) = metadata.get("tags").and_then(|v| v.as_array()) {
        for tag in tags {
            if let Some(s) = tag.as_str() {
                tokens.insert(s.to_string());
            }
        }
    }
    let add_prefix = |tokens: &mut HashSet<String>, path: &str| {
        let prefix: String = path.split('/').take(2).collect::<Vec<_>>().join("/");
        if !prefix.is_empty() {
            tokens.insert(prefix);
        }
    };
    add_prefix(&mut tokens, location);
    if let Some(gov) = metadata.get("governs").and_then(|v| v.as_array()) {
        for g in gov {
            if let Some(s) = g.as_str() {
                add_prefix(&mut tokens, s);
            }
        }
    }
    tokens
}

/// Jaccard similarity between two token sets: `|a ∩ b| / |a ∪ b|`. Returns
/// `0.0` when either set is empty.
pub(crate) fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

#[cfg(test)]
static RECORDED_GRAPH_QUERIES: std::sync::OnceLock<
    std::sync::Mutex<Vec<(String, String, String)>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub fn clear_recorded_graph_queries_for_tests() {
    let mutex = RECORDED_GRAPH_QUERIES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut guard) = mutex.lock() {
        guard.clear();
    }
}

#[cfg(test)]
pub fn recorded_graph_queries_for_tests() -> Vec<(String, String, String)> {
    let mutex = RECORDED_GRAPH_QUERIES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    mutex.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Resolve a caller directory (which may be a subdirectory of a Git worktree)
/// to its enclosing worktree/project root so `.NeuroStrata/active_session.json`
/// and `.NeuroStrata/session_queries.json` are always read and written at the
/// worktree root rather than a nested subdirectory.
///
/// A project root is marked by `.git` (directory or worktree file). A bare
/// `.NeuroStrata` in an *ancestor* is not authority: anything can create one
/// in a shared parent, including this resolver's own earlier fallback writes,
/// and adopting it silently redirects session state for every caller below
/// (issue #322). The given directory itself still counts when it carries
/// `.NeuroStrata`, so non-git projects whose cwd is the root are unaffected.
pub fn resolve_session_root_dir(dir: &std::path::Path) -> std::path::PathBuf {
    if let Some(wt) = gate::GateScope::capture(dir).worktree {
        return std::path::PathBuf::from(wt);
    }
    let resolved = crate::formal_kernel::resolve_session_root(None, dir);
    if is_home_without_git(&resolved) {
        // A markerless descendant of a non-git home is scratch, not a
        // project. Hand back the home folder, which every writer rejects.
        let check = dunce::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
        for home in session_home_dirs() {
            if let Ok(canonical) = dunce::canonicalize(&home) {
                if check.starts_with(&canonical) {
                    return canonical;
                }
            }
        }
    }
    resolved
}

// Process-wide, not per thread: a test that points NEUROSTRATA_PROJECT_ROOT
// into its fake home must have every concurrent test see that folder as the
// home folder too, or another test's lookup could write a ledger there.
// The fake home is added to the real one, never put in its place: a test
// running at the same time still stops at the real home folder.
#[cfg(test)]
static TEST_HOME_DIR: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Also treat `home` as a home folder until the guard drops. Hold
/// `daemon::project_root_env_test_lock()` while the guard lives.
#[cfg(test)]
pub(crate) fn override_home_dir_for_test(home: &std::path::Path) -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *TEST_HOME_DIR.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
    *TEST_HOME_DIR.lock().unwrap_or_else(|p| p.into_inner()) = Some(home.to_path_buf());
    Reset
}

/// The user's home folder. Tests can add a fake one next to it.
fn session_home_dirs() -> Vec<std::path::PathBuf> {
    let mut homes = Vec::new();
    #[cfg(test)]
    homes.extend(
        TEST_HOME_DIR
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone(),
    );
    homes.extend(dirs::home_dir());
    homes
}

fn is_home_folder(dir: &std::path::Path, home: Option<&std::path::Path>) -> bool {
    let Some(home) = home.and_then(|h| dunce::canonicalize(h).ok()) else {
        return false;
    };
    dunce::canonicalize(dir).is_ok_and(|d| d == home)
}

fn is_any_home_folder(dir: &std::path::Path, homes: &[std::path::PathBuf]) -> bool {
    homes.iter().any(|h| is_home_folder(dir, Some(h)))
}

/// True when `dir` is the user's home folder and holds no `.git`. Such a
/// folder is never a project, so no session file (`active_session.json`,
/// `session_queries.json`) is written there: `~/.NeuroStrata` belongs to
/// SynapticMesh, not to a project.
pub(crate) fn is_home_without_git(dir: &std::path::Path) -> bool {
    is_home_without_git_with(dir, &session_home_dirs())
}

fn is_home_without_git_with(dir: &std::path::Path, homes: &[std::path::PathBuf]) -> bool {
    // A markerless descendant of a non-git home is just as much scratch as
    // the home folder itself. `.NeuroStrata` or `.git` on the way up marks a
    // real project and stops the walk before home.
    matches!(
        walk_to_session_marker_with(dir, homes),
        SessionMarkerWalk::StoppedAtHome
    )
}

/// Where a walk for a session-file root ended.
#[derive(Debug, PartialEq, Eq)]
enum SessionMarkerWalk {
    /// The nearest folder holding `.git` or `.NeuroStrata`.
    Root(std::path::PathBuf),
    /// The walk reached the home folder without finding `.git`.
    StoppedAtHome,
    /// No folder from the start up to the filesystem root holds a marker.
    NotFound,
}

/// Find the session-file root for `start` (a folder, or a file whose folder
/// is used): the nearest folder holding `.git` or `.NeuroStrata`. The home
/// folder without `.git` is never a root, even with `.NeuroStrata`
/// (SynapticMesh keeps its node id there), and a walk that reaches it stops
/// there rather than continuing above it.
fn walk_to_session_marker(start: &std::path::Path) -> SessionMarkerWalk {
    walk_to_session_marker_with(start, &session_home_dirs())
}

fn walk_to_session_marker_with(
    start: &std::path::Path,
    homes: &[std::path::PathBuf],
) -> SessionMarkerWalk {
    let start = if start.is_dir() {
        start
    } else {
        match start.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => return SessionMarkerWalk::NotFound,
        }
    };
    let is_home = |dir: &std::path::Path| is_any_home_folder(dir, &homes);
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return SessionMarkerWalk::Root(dir.to_path_buf());
        }
        if is_home(dir) {
            return SessionMarkerWalk::StoppedAtHome;
        }
        if dir.join(".NeuroStrata").exists() {
            return SessionMarkerWalk::Root(dir.to_path_buf());
        }
        cur = dir.parent().filter(|p| !p.as_os_str().is_empty());
    }
    SessionMarkerWalk::NotFound
}

/// Walk a candidate root to its session-file root; `None` when there is none.
fn walk_to_worktree_marker(root: &std::path::Path) -> Option<std::path::PathBuf> {
    if root.as_os_str().is_empty() {
        return None;
    }
    match walk_to_session_marker(root) {
        SessionMarkerWalk::Root(r) => Some(r),
        SessionMarkerWalk::StoppedAtHome | SessionMarkerWalk::NotFound => None,
    }
}

/// The single ledger-root policy shared by the receipt recorder and by
/// `task_validate`: an explicit caller worktree hint wins; otherwise prefer
/// the namespace registry's `project_root` then registered directory rows, and
/// only fall back to the daemon CWD last. Each candidate is walked up to the
/// nearest folder holding `.git` or `.NeuroStrata`; the home folder without
/// `.git` never qualifies. `None` when no candidate qualifies: the
/// recorder then writes nothing, rather than creating `.NeuroStrata` in the
/// home folder or at a drive root.
pub(crate) async fn receipt_ledger_root(
    caller_root: Option<&str>,
    store: &Arc<dyn VectorStore>,
    namespace: &str,
) -> Option<std::path::PathBuf> {
    if let Some(hint) = caller_root.map(str::trim).filter(|s| !s.is_empty()) {
        let hint = std::path::Path::new(hint);
        // A hint at or under a non-git home names no project at all. Stop
        // here: falling through to the registry/directory rows or daemon CWD
        // would write this home session's receipt into another project.
        if is_home_without_git(hint) {
            return None;
        }
        // A caller hint is only trusted when it actually resolves inside a
        // worktree. A scratch dir with no marker falls through to the
        // registry/directory rows and daemon CWD, never becomes the root.
        if let Some(root) = walk_to_worktree_marker(&resolve_session_root_dir(hint)) {
            return Some(root);
        }
    }
    let ns = crate::server::resolve_namespace(store, namespace).await;
    if let Ok(Some((_, registry))) = store.get(&ns, &crate::registry::registry_row_id(&ns)).await {
        if crate::registry::is_registry_row(&registry.metadata) {
            if let Some(p) = registry
                .metadata
                .get("project_root")
                .and_then(Value::as_str)
                .filter(|p| !p.trim().is_empty())
            {
                if let Some(root) = walk_to_worktree_marker(std::path::Path::new(p)) {
                    return Some(root);
                }
            }
        }
    }
    if let Ok(dirs) = store.list(&ns, None, Some("directory")).await {
        for d in &dirs {
            if let Some(p) = d
                .payload
                .metadata
                .get("absolute_path")
                .and_then(Value::as_str)
            {
                if let Some(root) = walk_to_worktree_marker(std::path::Path::new(p)) {
                    return Some(root);
                }
            }
        }
    }
    walk_to_worktree_marker(&crate::daemon::resolve_project_root())
}

/// Record a graph or memory lookup against any active `in_progress` task in
/// `namespace` and append to `.NeuroStrata/session_queries.json` if a project
/// root is known.
pub async fn record_graph_query(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    tool_name: &str,
    query_hint: &str,
) {
    record_graph_query_with_root(store, namespace, tool_name, query_hint, None).await;
}

/// Like [`record_graph_query`], but prefers an explicit caller root
/// (`project_root` / `worktree` / `cwd`) or an absolute `query_hint`'s enclosing
/// git worktree over the shared namespace registry row's `project_root`.
pub async fn record_graph_query_with_root(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    tool_name: &str,
    query_hint: &str,
    caller_root: Option<&str>,
) {
    record_graph_query_inner(store, namespace, tool_name, query_hint, caller_root, true).await;
}

/// Append one legacy `queries[]` history row for a path returned by a single
/// dispatched lookup without touching task counters (panel C14). The dispatch
/// itself is counted once via [`record_graph_query`]; these rows exist only for
/// readers that still consult the legacy history ledger.
pub async fn record_legacy_query_path(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    tool_name: &str,
    query_hint: &str,
    caller_root: Option<&str>,
) {
    record_graph_query_inner(store, namespace, tool_name, query_hint, caller_root, false).await;
}

async fn record_graph_query_inner(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    tool_name: &str,
    query_hint: &str,
    caller_root: Option<&str>,
    count_task: bool,
) {
    let ns = crate::server::resolve_namespace(store, namespace).await;
    #[cfg(test)]
    {
        let mutex = RECORDED_GRAPH_QUERIES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
        if let Ok(mut guard) = mutex.lock() {
            guard.push((ns.clone(), tool_name.to_string(), query_hint.to_string()));
        }
    }
    // This runs on every graph-first lookup. It used to list the WHOLE
    // namespace each time (+50 to +300 MB per call in the daemon log); name only the rows it needs: the task rows, the
    // registry row by its fixed id, and directory rows only when the registry
    // row has no project root.
    let Ok(rows) = store.list(&ns, None, Some("task")).await else {
        return;
    };
    let now = chrono::Utc::now().timestamp();
    let entry_str = format!(
        "{}:{}",
        tool_name,
        query_hint.chars().take(80).collect::<String>()
    );
    let mut project_root: Option<String> = caller_root
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            resolve_session_root_dir(std::path::Path::new(s))
                .to_string_lossy()
                .into_owned()
        });
    // A hint under the home folder (no `.git` on the way up) names no
    // project: the task is still stamped, but no ledger gets the entry.
    let mut hint_under_home = false;
    if project_root.is_none() {
        let hint_path = std::path::Path::new(query_hint);
        if hint_path.is_absolute() {
            match walk_to_session_marker(hint_path) {
                SessionMarkerWalk::Root(dir) => {
                    project_root = Some(dir.to_string_lossy().to_string());
                }
                SessionMarkerWalk::StoppedAtHome => hint_under_home = true,
                SessionMarkerWalk::NotFound => {}
            }
        }
    }
    if project_root.is_none() && !hint_under_home {
        if let Ok(Some((_, registry))) =
            store.get(&ns, &crate::registry::registry_row_id(&ns)).await
        {
            if crate::registry::is_registry_row(&registry.metadata) {
                if let Some(p) = registry
                    .metadata
                    .get("project_root")
                    .and_then(|v| v.as_str())
                {
                    project_root = Some(p.to_string());
                }
            }
        }
    }
    if project_root.is_none() && !hint_under_home {
        if let Ok(dirs) = store.list(&ns, None, Some("directory")).await {
            for d in &dirs {
                if let Some(p) = d
                    .payload
                    .metadata
                    .get("absolute_path")
                    .and_then(|v| v.as_str())
                {
                    if std::path::Path::new(p).join(".git").exists() {
                        project_root = Some(p.to_string());
                        break;
                    }
                }
            }
        }
    }
    // Panel C14/C15: one dispatch increments ONE counter — the dispatching
    // task — never every in-progress task. No explicit task id reaches this
    // legacy recorder, so it uses the same deterministic first-in-progress-by-id
    // fallback as the receipt recorder. The whole load -> decide -> upsert runs
    // under the per-task claim lock from the claim path so concurrent dispatches
    // cannot lose increments across their `.await` points.
    if count_task {
        if let Some(r) = rows
            .iter()
            .filter(|r| gate::is_task(r) && gate::task_status(r) == Status::InProgress)
            .min_by(|a, b| a.id.cmp(&b.id))
        {
            let lock = claim_lock(&r.id);
            let _guard = lock.lock().await;
            if let Ok(Some((vec, mut payload))) = store.get(&ns, &r.id).await {
                let obj = task_obj_mut(&mut payload);
                // Re-check under the lock: the task may have left in_progress.
                if obj.get("status").and_then(Value::as_str) == Some(Status::InProgress.as_str()) {
                    let count = obj
                        .get("graph_queries_count")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        + 1;
                    obj.insert("graph_queries_count".to_string(), json!(count));
                    obj.insert("graph_queried".to_string(), json!(true));
                    let mut recent: Vec<String> = obj
                        .get("recent_queries")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    recent.push(entry_str.clone());
                    if recent.len() > 10 {
                        let drop_n = recent.len() - 10;
                        recent.drain(0..drop_n);
                    }
                    obj.insert("recent_queries".to_string(), json!(recent));
                    let _ = store.upsert(&ns, &r.id, vec, payload).await;
                }
            }
        }
    }
    if project_root.is_none() && !hint_under_home {
        // Start from `resolve_project_root()` (which honors `NEUROSTRATA_PROJECT_ROOT`
        // before falling back to `std::env::current_dir()`) and walk up to the
        // nearest folder holding `.git` or `.NeuroStrata`.
        // No root found (home folder, drive root): nothing is written.
        project_root = walk_to_worktree_marker(&crate::daemon::resolve_project_root())
            .map(|root| root.to_string_lossy().into_owned());
    }
    let governed_prefix = if query_hint.ends_with('/')
        || (project_root.is_some()
            && std::path::Path::new(project_root.as_ref().unwrap())
                .join(query_hint)
                .is_dir())
    {
        // Explicit directory coverage: preserve trailing slash without
        // widening to the top-level directory.
        format!("{}/", query_hint.trim_end_matches('/'))
    } else if tool_name == "neurostrata_memories_for_file"
        || query_hint.contains('/')
        || query_hint.contains('.')
    {
        // Exact file coverage: preserve the queried path verbatim.
        query_hint.to_string()
    } else {
        // Bare module/domain keyword: widen to directory coverage.
        format!("{}/", query_hint)
    };
    if let Some(root) = project_root.filter(|r| !is_home_without_git(std::path::Path::new(r))) {
        let ledger_path = std::path::Path::new(&root).join(".NeuroStrata/session_queries.json");
        // NOTE (spec §6 R6.2): this is the PRE-dispatch legacy path. It writes
        // only the legacy `queries[]` row + the single task stamp above; it must
        // NOT write an `events[]` receipt, because no outcome exists yet. Receipts
        // are written exclusively by `record_tool_receipt` after the handler returns.
        tool_receipts::mutate_session_ledger(&ledger_path, &ns, |doc| {
            if let Some(arr) = doc.get_mut("queries").and_then(|v| v.as_array_mut()) {
                // Legacy rows stay for compatibility; every quality-metric
                // reader ignores them in favour of `events[]` receipts.
                arr.push(json!({
                    "ts": now,
                    "tool": tool_name,
                    "query": query_hint,
                    "governed_prefixes": [governed_prefix],
                }));
            }
        });
    }
}

/// Session identifier stamped onto receipts. Prefer the harness-provided id;
/// fall back to a process-local id so local runs still attribute an event.
pub(crate) fn current_session_id() -> String {
    static SESSION_ID: OnceLock<String> = OnceLock::new();
    SESSION_ID
        .get_or_init(|| {
            std::env::var("OPENCODE_SESSION_ID")
                .or_else(|_| std::env::var("NEUROSTRATA_SESSION_ID"))
                .unwrap_or_else(|_| format!("local-{}", uuid::Uuid::new_v4()))
        })
        .clone()
}

/// Record a fully-specified POST-DISPATCH receipt. This is the ONLY function
/// that writes `events[]` receipts; every outcome field comes from the real
/// handler response. It keeps a caller-supplied `task_id` for attribution and
/// otherwise falls back to the stable active in-progress task, then bumps the
/// additive-only `tool_invocations` counter. Legacy
/// `queries[]` rows and the `graph_queries_count`/`graph_queried` stamps belong
/// exclusively to the handler-internal recorders. Receipts stay in the local
/// JSON ledger — never memory rows, so no ReBAC or mesh surface sees them.
pub async fn record_tool_receipt(
    store: &Arc<dyn VectorStore>,
    mut fields: tool_receipts::ReceiptFields,
    caller_root: Option<&str>,
) {
    let ns = crate::server::resolve_namespace(store, &fields.namespace).await;

    // Review fix round 2: ATTRIBUTION ONLY. Never touch `graph_queries_count`,
    // `graph_queried` or `recent_queries`; those belong exclusively to the
    // handler-internal recorders, and bumping them here double-counts a dispatch.
    //
    // Panel C15: a non-empty `fields.task_id` is the caller's real task context
    // and always wins. The generic MCP dispatch forwards no task id, but it does
    // carry a session id and a caller root. When no id is supplied, attribute the
    // receipt to the task that actually made the call, mirroring the specificity
    // order of `handle_guard_validate` in src/server.rs:
    // session+worktree > session > worktree > lowest-id. Within each tier the
    // lowest id wins, so attribution stays deterministic regardless of the order
    // `store.list` returned. Receipt attribution is telemetry only (never a
    // gate/quota), so this sharpens accuracy without opening any work.
    if fields.task_id.is_empty() {
        let now = chrono::Utc::now().timestamp();
        if let Ok(rows) = store.list(&ns, None, Some("task")).await {
            let is_active = |r: &SearchResult| {
                gate::is_task(r)
                    && !gate::is_retired(&r.payload.metadata, now)
                    && gate::task_status(r) == Status::InProgress
            };
            fn task_meta<'a>(r: &'a SearchResult, key: &str) -> Option<&'a str> {
                r.payload
                    .metadata
                    .get("task")
                    .and_then(|t| t.get(key))
                    .and_then(|v| v.as_str())
            }
            let pick = |matches: &dyn Fn(&SearchResult) -> bool| {
                rows.iter()
                    .filter(|r| is_active(r) && matches(r))
                    .min_by(|a, b| a.id.cmp(&b.id))
                    .map(|r| r.id.clone())
            };
            let session = fields.session_id.trim();
            let session = (!session.is_empty()).then_some(session);
            let worktree = caller_root.map(str::trim).filter(|w| !w.is_empty());
            let attributed = match (session, worktree) {
                (Some(s), Some(w)) => pick(&|r| {
                    task_meta(r, "session_id") == Some(s) && task_meta(r, "worktree") == Some(w)
                })
                .or_else(|| pick(&|r| task_meta(r, "session_id") == Some(s)))
                .or_else(|| pick(&|r| task_meta(r, "worktree") == Some(w))),
                (Some(s), None) => pick(&|r| task_meta(r, "session_id") == Some(s)),
                (None, Some(w)) => pick(&|r| task_meta(r, "worktree") == Some(w)),
                (None, None) => None,
            }
            .or_else(|| pick(&|_| true));
            if let Some(id) = attributed {
                fields.task_id = id;
            }
        }
    }

    // Resolve the ledger root: explicit caller root, then the namespace
    // registry/directory roots, then cwd walked up to a worktree marker.
    // Review fix round 2: this recorder writes ONLY the
    // versioned `events[]` receipt and the additive `tool_invocations`
    // counter. Legacy `queries[]` rows belong exclusively to the handler-
    // internal ledger recorders, so none are written here.
    let Some(root) = receipt_ledger_root(caller_root, store, &ns).await else {
        return;
    };
    let ledger_path = root.join(".NeuroStrata/session_queries.json");
    let tool = fields.tool.clone();
    tool_receipts::mutate_session_ledger(&ledger_path, &ns, |doc| {
        tool_receipts::append_event(doc, fields.to_value());
        tool_receipts::bump_tool_invocation(doc, &tool);
    });
}

// ── tool: neurostrata_task_create ──────────────────────────────────────────

pub async fn handle_task_create(
    args: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    let namespace = match required_str(&args, "namespace") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let title = match arg_str(&args, "title") {
        Some(t) => match check_title(t) {
            Ok(v) => v,
            Err(e) => return e,
        },
        None => {
            return "ERROR: 'title' is required.".to_string();
        }
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }

    // A typo'd namespace would silently fork the stratum. New projects seed
    // their first task through neurostrata_bootstrap instead.
    match store.list_namespaces().await {
        Ok(existing) if existing.iter().any(|n| n == &namespace) => {}
        Ok(existing) => {
            return format!(
                "ERROR: namespace '{}' does not exist. Check neurostrata_list_namespaces with a spelling you have seen; for a brand new project call neurostrata_bootstrap first, which creates the first task. Existing namespaces: {:?}",
                namespace, existing
            );
        }
        Err(e) => return format!("ERROR: could not verify which namespaces exist: {}", e),
    }

    let task_type = match arg_str(&args, "task_type") {
        None => "task".to_string(),
        Some(t) if ["task", "bug", "feature", "epic"].contains(&t) => t.to_string(),
        Some(t) => {
            // An agent that asks for a `recurring` task (or any other value
            // outside the enum) is asking the wrong question: tasks must
            // reach `done`, so a never-closing type would break the lifecycle
            // invariant. Recurring / scheduled work is a *procedure* memory
            // — created via `neurostrata_add_memory` with `memory_type:
            // "procedure"`, stamped via `neurostrata_procedure_perform`.
            // Point the agent at the right path on the very first failure;
            // the other discovery surfaces (the schema enum, the lane doc)
            // are downstream of this signpost.
            return format!(
                "ERROR: unknown task_type '{t}'. Valid types: task, bug, feature, epic. (Recurring or scheduled patterns are not tasks: create a procedure via `neurostrata_add_memory` with `memory_type: \"procedure\"`, then stamp each firing with `neurostrata_procedure_perform`.)",
            );
        }
    };
    let priority = match args.get("priority") {
        None => 2,
        Some(Value::Number(n)) if n.as_i64().map(|p| (0..=4).contains(&p)).unwrap_or(false) => {
            n.as_i64().unwrap_or(2)
        }
        Some(_) => {
            return "ERROR: 'priority' must be an integer from 0 to 4 (0 is highest).".to_string()
        }
    };
    let labels = match string_list(&args, "labels") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let blocked_by = match string_list(&args, "blocked_by") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let description = arg_str(&args, "description")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let parent_id = arg_str(&args, "parent_id").map(|s| s.to_string());
    let user_id = arg_str(&args, "user_id").unwrap_or("unknown").to_string();
    let agent_name = arg_str(&args, "agent_name").map(|s| s.to_string());

    // A parent must exist and be a task, or the CONTAINS edge points at nothing.
    if let Some(parent) = &parent_id {
        match store.get(&namespace, parent).await {
            Ok(Some((_, p))) if p.memory_type == "task" => {}
            Ok(Some(_)) => return format!("ERROR: parent_id '{}' is not a task.", parent),
            Ok(None) => {
                return format!(
                    "ERROR: no task with id '{}' in namespace '{}'.",
                    parent, namespace
                )
            }
            Err(e) => return format!("ERROR: could not read parent_id '{}': {}", parent, e),
        }
    }

    let mut task_meta = Map::new();
    task_meta.insert("priority".to_string(), json!(priority));
    task_meta.insert("task_type".to_string(), json!(task_type));
    if !labels.is_empty() {
        task_meta.insert("labels".to_string(), json!(labels));
    }
    if !blocked_by.is_empty() {
        task_meta.insert("blocked_by".to_string(), json!(blocked_by));
    }
    if let Some(d) = &description {
        task_meta.insert("description".to_string(), json!(d));
    }
    let mut top_level = Map::new();
    if let Some(parent) = &parent_id {
        // The child declares its container (vocabulary: target_to_self), so
        // the materialized edge runs parent -> subtask.
        top_level.insert("contained_by".to_string(), json!([parent]));
    }

    match store_new_task(
        &store,
        &emb,
        &namespace,
        &title,
        &user_id,
        agent_name.as_deref(),
        task_meta,
        top_level,
    )
    .await
    {
        Ok((id, mut payload)) => {
            let vector = emb.embed(&payload.content).await.unwrap_or_default();
            let (hits_json, subagent_prompt) =
                build_task_graph_context(&store, &namespace, &id, &vector, &payload).await;
            // Re-read the freshest row under the task claim lock so a
            // concurrent create/update cannot be overwritten mid-context-build.
            let claim_mutex = claim_lock(&id);
            let _claim_guard = claim_mutex.lock().await;
            if let Ok((mut latest_task, latest_vec)) = load_task(&store, &namespace, &id).await {
                {
                    let obj = task_obj_mut(&mut latest_task.payload);
                    obj.insert(
                        "subagent_prompt_injection".to_string(),
                        json!(subagent_prompt),
                    );
                }
                let _ = save_task(&store, &namespace, &id, latest_vec, latest_task.payload).await;
            }
            {
                let obj = task_obj_mut(&mut payload);
                obj.insert(
                    "subagent_prompt_injection".to_string(),
                    json!(subagent_prompt),
                );
            }
            encode(&json!({
                "id": id,
                "namespace": namespace,
                "task": task_summary(&id, &payload),
                "auto_retrieved_context": hits_json,
                "subagent_prompt_injection": subagent_prompt,
            }))
        }
        Err(e) => e,
    }
}

// ── tool: neurostrata_task_claim ───────────────────────────────────────────

/// Per-task claim locks: task id -> the lock serializing that task's claim
/// sequence. Weak entries so a lock is dropped once no claim holds it; dead
/// entries are pruned on each call, so the map tracks live claims, not every
/// task ever claimed.
static CLAIM_LOCKS: OnceLock<StdMutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

/// The lock `handle_task_claim` holds across its whole load -> decide ->
/// upsert sequence. Claim's only production caller runs inside the daemon's
/// MCP dispatch (`process_mcp_request`); stdio processes are pure proxies
/// and never execute the handler, so this process-global registry serializes
/// all claim traffic in the shipped topology. The store's upsert API has no
/// conditional write to build a CAS on, so the process lock is the smallest
/// honest atomicity mechanism.
fn claim_lock(task_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let registry = CLAIM_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = map.get(task_id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    map.insert(task_id.to_string(), Arc::downgrade(&lock));
    lock
}

async fn resolve_task_scope(
    args: &Value,
    store: &Arc<dyn VectorStore>,
    namespace: &str,
) -> (std::path::PathBuf, gate::GateScope) {
    let explicit_root = arg_str(args, "worktree")
        .or_else(|| arg_str(args, "project_root"))
        .or_else(|| arg_str(args, "cwd"))
        .map(std::path::PathBuf::from);
    let rid = crate::registry::registry_row_id(namespace);
    let registry_root = if let Ok(Some((_, reg_p))) = store.get(namespace, &rid).await {
        reg_p
            .metadata
            .get("project_root")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from)
    } else {
        None
    };
    let daemon_root = crate::daemon::resolve_project_root();

    let mut candidates = Vec::new();
    if let Some(ref r) = explicit_root {
        candidates.push(r.clone());
    } else {
        if let Some(r) = registry_root {
            if !candidates.contains(&r) {
                candidates.push(r);
            }
        }
        if !candidates.contains(&daemon_root) {
            candidates.push(daemon_root.clone());
        }
    }

    let mut chosen_root = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| daemon_root.clone());
    let mut scope = gate::GateScope::default();
    for cand in &candidates {
        let captured = gate::GateScope::capture(cand);
        if captured.branch.is_some() || captured.worktree.is_some() {
            chosen_root = cand.clone();
            scope = captured;
            break;
        }
    }
    if let Some(explicit_branch) = arg_str(args, "branch") {
        scope.branch = Some(explicit_branch.to_string());
    }
    if scope.worktree.is_none() {
        if let Some(ref explicit_wt) = explicit_root {
            let root_dir = resolve_session_root_dir(explicit_wt);
            let canonical = root_dir
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| root_dir.to_string_lossy().into_owned());
            scope.worktree = Some(canonical);
        }
    }
    (chosen_root, scope)
}

pub async fn handle_task_claim(args: Value, store: Arc<dyn VectorStore>) -> String {
    let id = match required_str(&args, "id") {
        Ok(v) => v.to_string(),
        Err(e) => return e,
    };
    let namespace = match required_str(&args, "namespace") {
        Ok(v) => v,
        Err(e) => return e,
    };
    // A claim without a session has no holder identity to exclude others
    // with: refuse before any state is read or written, and never persist
    // an empty session_id.
    let session = match required_str(&args, "session_id") {
        Ok(v) => v.to_string(),
        Err(e) => return e,
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }

    let assignee = arg_str(&args, "assignee").unwrap_or("unknown").to_string();

    // Serialize the whole load -> decide -> upsert sequence per task: two
    // concurrent claims on one task can no longer both observe it unclaimed.
    let lock = claim_lock(&id);
    let _claim_guard = lock.lock().await;

    let (mut task, vector) = match load_task(&store, &namespace, &id).await {
        Ok(found) => found,
        Err(e) => return e,
    };
    let from = task.status();
    let holder = task
        .task_field("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let holder_assignee = task
        .task_field("assignee")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut note = "claim".to_string();

    let k_status = from.to_kernel();
    let same_session = holder == session;
    let mut age: Option<i64> = None;
    if from == Status::InProgress && !same_session {
        let now = chrono::Utc::now().timestamp();
        age = gate::age_secs(
            task.payload.metadata.get("task"),
            &["updated_at", "created_at"],
            now,
        );
        if age.is_none() {
            tracing::warn!(
                "task {} in_progress claim has missing/invalid updated_at timestamp; treating claim as stale",
                id
            );
        }
    }
    match evaluate_claim_decision(k_status, same_session, age, gate::STALE_AFTER_SECS) {
        KernelClaimDecision::IdempotentSameSession => {
            // Legacy rows written before attribution existed keep the holder
            // but not the branch/worktree: backfill what is missing so the
            // strict gate can judge them, persist it, and leave already
            // attributed rows exactly as they are.
            let (_, scope) = resolve_task_scope(&args, &store, &namespace).await;
            let obj = task_obj_mut(&mut task.payload);
            let branch_missing = obj
                .get("branch")
                .and_then(Value::as_str)
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            let worktree_missing = obj
                .get("worktree")
                .and_then(Value::as_str)
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            let mut backfilled = false;
            if branch_missing && scope.branch.is_some() {
                obj.insert("branch".to_string(), json!(scope.branch));
                backfilled = true;
            }
            if worktree_missing && scope.worktree.is_some() {
                obj.insert("worktree".to_string(), json!(scope.worktree));
                backfilled = true;
            }
            if backfilled {
                if let Err(e) = save_task(
                    &store,
                    &namespace,
                    &id,
                    vector.clone(),
                    task.payload.clone(),
                )
                .await
                {
                    return e;
                }
            }
            let body = json!({
                "id": id,
                "namespace": namespace,
                "status": from.as_str(),
                "claimed": true,
                "note": "already claimed by this session",
                "task": task_summary(&id, &task.payload),
            });
            return encode(&body);
        }
        KernelClaimDecision::RefuseLiveHolder => {
            let age_val = age.unwrap_or(0);
            return format!(
                "ERROR: task {} is claimed by {} (session '{}') and that claim is live (last update {}s ago). Two live sessions on one task duplicate work: ask them to release it (neurostrata_task_update with status 'open'), or claim it yourself after {} minutes without an update.",
                id,
                holder_assignee,
                holder,
                age_val,
                gate::STALE_AFTER_SECS / 60
            );
        }
        KernelClaimDecision::TakeoverStaleHolder => {
            let release_reason = if holder.is_empty() {
                "release: the holder never recorded a session".to_string()
            } else {
                format!(
                    "release: session '{}' went stale after {} minutes",
                    holder,
                    gate::STALE_AFTER_SECS / 60
                )
            };
            let ctx = Ctx {
                reason: release_reason,
                extraction_edge_exists: false,
                actor: assignee.clone(),
                session: session.clone(),
            };
            // InProgress -> Open is a legal edge; taking over releases first
            // rather than stealing the state, so both hops stay in history.
            match apply(&task, Status::Open, &ctx) {
                Ok(released) => task = released,
                Err(e) => return format!("ERROR: {}", e),
            }
            {
                let obj = task_obj_mut(&mut task.payload);
                obj.remove("assignee");
                obj.remove("session_id");
            }
            note = format!("claim (took over a stale claim by {})", holder_assignee);
        }
        KernelClaimDecision::ClaimFromOpen => {}
        KernelClaimDecision::DelegateToFsm => {
            // No special-casing: apply() below returns the legal set from
            // here, which teaches the machine instead of guessing intent.
        }
    }

    let ctx = Ctx {
        reason: note,
        extraction_edge_exists: false,
        actor: assignee.clone(),
        session: session.clone(),
    };
    let claimed = match apply(&task, Status::InProgress, &ctx) {
        Ok(moved) => moved,
        Err(e) => return format!("ERROR: {}", e),
    };
    task = claimed;
    let (scope_root, scope) = resolve_task_scope(&args, &store, &namespace).await;
    {
        let obj = task_obj_mut(&mut task.payload);
        obj.insert("assignee".to_string(), json!(assignee));
        obj.insert("session_id".to_string(), json!(session));
        obj.insert("branch".to_string(), json!(scope.branch));
        obj.insert("worktree".to_string(), json!(scope.worktree));
    }

    // No re-read guard here: the per-task claim lock above is held across
    // this save, so no claim-path writer can interleave between the load
    // and the upsert. (The R7 best-effort re-read this replaces could only
    // ever fire for an out-of-process claim writer, and none exists — every
    // claim enters through this handler in the daemon process.)
    //
    // Build the graph context and stamp the prompt injection onto the row
    // BEFORE the single save, while _claim_guard is still held, so a
    // concurrent claim cannot observe a row without the injection.

    let (hits_json, subagent_prompt) =
        build_task_graph_context(&store, &namespace, &id, &vector, &task.payload).await;
    {
        let obj = task_obj_mut(&mut task.payload);
        obj.insert(
            "subagent_prompt_injection".to_string(),
            json!(subagent_prompt),
        );
    }
    if let Err(e) = save_task(
        &store,
        &namespace,
        &id,
        vector.clone(),
        task.payload.clone(),
    )
    .await
    {
        return e;
    }
    // Write active_session.json into the caller's own worktree, not the global
    // registry root: explicit worktree/project_root/cwd wins; otherwise use the
    // registry project_root. Never fall back to daemon_root here -- a worktree
    // caller without a registry root must not write into the daemon checkout.
    let explicit_root = arg_str(&args, "worktree")
        .or_else(|| arg_str(&args, "project_root"))
        .or_else(|| arg_str(&args, "cwd"))
        .map(std::path::PathBuf::from);
    let session_root = if let Some(ref exp) = explicit_root {
        scope
            .worktree
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| Some(resolve_session_root_dir(exp)))
    } else {
        let rid = crate::registry::registry_row_id(&namespace);
        if let Ok(Some((_, reg_p))) = store.get(&namespace, &rid).await {
            reg_p
                .metadata
                .get("project_root")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from)
        } else {
            None
        }
    };
    if let Some(root) = session_root {
        let golden = collect_golden_rules(&store, &namespace, None).await;
        persist_active_session_file(&root, &namespace, &golden, Some((&id, &task.payload)));
    }
    let event_id = arg_str(&args, "event_id")
        .unwrap_or(&format!("claim-{}", id))
        .to_string();
    {
        let mut r = TASK_RECEIPTS.lock().unwrap_or_else(|e| e.into_inner());
        let binding = crate::task::receipt::Receipt::capture(&id, &scope_root);
        r.record(crate::receipts::store::Receipt {
            session_id: session.clone(),
            task_id: id.clone(),
            event_id,
            outcome: "claimed".to_string(),
            exposed_ids: vec![format!("{}:{}", namespace, id)],
            worktree: binding.worktree,
            head_sha: binding.head_sha,
            dirty_tree_hash: binding.dirty_tree_hash,
        });
    }
    encode(&json!({
        "id": id,
        "namespace": namespace,
        "status": task.status().as_str(),
        "claimed": true,
        "transition": { "from": from.as_str(), "to": "in_progress" },
        "task": task_summary(&id, &task.payload),
        "auto_retrieved_context": hits_json,
        "subagent_prompt_injection": subagent_prompt,
    }))
}

// ── tool: neurostrata_task_update ──────────────────────────────────────────

pub async fn handle_task_update(
    args: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    // The done funnel (section 2): done has exactly one entrance and it is
    // guarded. Checked before anything is read or written.
    if arg_str(&args, "status") == Some("done") {
        return DONE_FUNNEL_MESSAGE.to_string();
    }

    let id = match required_str(&args, "id") {
        Ok(v) => v.to_string(),
        Err(e) => return e,
    };
    let namespace = match required_str(&args, "namespace") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }

    let target = match arg_str(&args, "status") {
        None => None,
        Some(s) => match Status::parse(s) {
            Some(st) => Some(st),
            None => {
                return format!(
                    "ERROR: unknown status '{}'. Valid here: open, in_progress, blocked (done only via neurostrata_task_complete).",
                    s
                )
            }
        },
    };
    let note = arg_str(&args, "note").map(|s| s.to_string());
    // The title is the task's content (section 1.1), so renaming it means
    // rewriting content AND re-embedding: a title is what search_memory reads.
    let new_title = match args.get("title") {
        None => None,
        Some(_) => match check_title(arg_str(&args, "title").unwrap_or_default()) {
            Ok(t) => Some(t),
            Err(e) => return e,
        },
    };
    let priority = match args.get("priority") {
        None => None,
        Some(Value::Number(n)) if n.as_i64().map(|p| (0..=4).contains(&p)).unwrap_or(false) => {
            n.as_i64()
        }
        Some(_) => {
            return "ERROR: 'priority' must be an integer from 0 to 4 (0 is highest).".to_string()
        }
    };
    let assignee = arg_str(&args, "assignee").map(|s| s.to_string());
    let add_labels = match string_list(&args, "add_labels") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let blocked_by = match string_list(&args, "blocked_by") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let has_blocked_by = args.get("blocked_by").is_some();

    if target.is_none()
        && note.is_none()
        && new_title.is_none()
        && priority.is_none()
        && assignee.is_none()
        && add_labels.is_empty()
        && !has_blocked_by
    {
        return "ERROR: nothing to update: pass at least one of title, status, note, priority, assignee, add_labels, blocked_by.".to_string();
    }

    let (mut task, mut vector) = match load_task(&store, &namespace, &id).await {
        Ok(found) => found,
        Err(e) => return e,
    };
    let from = task.status();

    // A rename is history, not a transition: without a caller's note the
    // history entry says what the title became, so Working Memory keeps the
    // old wording recoverable after a vocabulary scrub.
    let rename_note = new_title
        .as_ref()
        .filter(|t| **t != task.payload.content)
        .map(|t| format!("renamed: '{}' -> '{}'", task.payload.content, t));

    let ctx = Ctx {
        reason: note.clone().unwrap_or_default(),
        extraction_edge_exists: false,
        actor: assignee
            .clone()
            .or_else(|| {
                task.task_field("assignee")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "unknown".to_string()),
        session: task
            .task_field("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    };

    let mut transition: Option<Value> = None;
    match target {
        Some(to) if to != from => match apply(&task, to, &ctx) {
            Ok(moved) => {
                // Unclaim releases the holder's marks with it (section 2).
                if from == Status::InProgress && to == Status::Open {
                    let mut moved = moved;
                    let obj = task_obj_mut(&mut moved.payload);
                    obj.remove("assignee");
                    obj.remove("session_id");
                    obj.remove("branch");
                    obj.remove("worktree");
                    task = moved;
                }
                // Claiming through task_update must not hollow the machine:
                // an InProgress task with no holder fails the Open/InProgress
                // invariant, and task_update may be the only claim action a
                // session makes. So Open -> InProgress stamps the caller's
                // claim while it moves.
                else if from == Status::Open && to == Status::InProgress {
                    let mut moved = moved;
                    let caller_session = arg_str(&args, "session_id")
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.to_string());
                    let obj = task_obj_mut(&mut moved.payload);
                    let unclaimed = obj
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(|s| s.trim().is_empty())
                        .unwrap_or(true);
                    // A scope is attributable only to a real new session
                    // claim: an Open row with no holder and a caller-provided
                    // session. A holder-less transition stamps nothing, and a
                    // row still claimed by another holder keeps that holder's
                    // attribution untouched.
                    let new_claim = unclaimed && caller_session.is_some();
                    if unclaimed {
                        if let Some(session) = &caller_session {
                            obj.insert("session_id".to_string(), json!(session));
                        }
                        if obj
                            .get("assignee")
                            .and_then(Value::as_str)
                            .map(|s| s.trim().is_empty())
                            .unwrap_or(true)
                        {
                            if let Some(a) = assignee.clone().or_else(|| caller_session.clone()) {
                                obj.insert("assignee".to_string(), json!(a));
                            }
                        }
                    }
                    if new_claim {
                        let (_, scope) = resolve_task_scope(&args, &store, &namespace).await;
                        obj.insert("branch".to_string(), json!(scope.branch));
                        obj.insert("worktree".to_string(), json!(scope.worktree));
                    }
                    task = moved;
                } else {
                    task = moved;
                }
                transition = Some(json!({ "from": from.as_str(), "to": to.as_str() }));
            }
            Err(e) => return format!("ERROR: {}", e),
        },
        // A same-status note (or a plain progress note) is history, not a
        // transition: this is where the Breath prompt lands (section 6.5).
        _ if note.is_some() || rename_note.is_some() => {
            let mut hist_ctx = ctx.clone();
            if note.is_none() {
                hist_ctx.reason = rename_note.clone().unwrap_or_default();
            }
            task = annotate(&task, &hist_ctx);
        }
        _ => {}
    }

    // content IS the title (section 1.1), and the vector is the embedded
    // content: a rename that skipped the re-embed would leave search_memory
    // matching the old words.
    if let Some(t) = &new_title {
        task.payload.content = t.clone();
        if rename_note.is_some() {
            match emb.embed(t).await {
                Ok(v) => vector = v,
                Err(e) => return format!("ERROR: Failed to embed the task title: {}", e),
            }
        }
    }

    let refreshed_prompt = if rename_note.is_some() && task.status() == Status::InProgress {
        let (_hits, prompt) =
            build_task_graph_context(&store, &namespace, &id, &vector, &task.payload).await;
        Some(prompt)
    } else {
        None
    };

    // Field updates ride on whatever the transition produced.
    {
        let obj = task_obj_mut(&mut task.payload);
        if rename_note.is_some() {
            if let Some(prompt) = refreshed_prompt {
                obj.insert("subagent_prompt_injection".to_string(), json!(prompt));
            } else {
                obj.remove("subagent_prompt_injection");
            }
        }
        if let Some(p) = priority {
            obj.insert("priority".to_string(), json!(p));
        }
        if let Some(a) = assignee {
            obj.insert("assignee".to_string(), json!(a));
        }
        if !add_labels.is_empty() {
            let mut labels: Vec<Value> = obj
                .get("labels")
                .and_then(|l| l.as_array())
                .cloned()
                .unwrap_or_default();
            for label in &add_labels {
                if !labels.iter().any(|v| v.as_str() == Some(label.as_str())) {
                    labels.push(json!(label));
                }
            }
            obj.insert("labels".to_string(), Value::Array(labels));
        }
        if has_blocked_by {
            obj.insert("blocked_by".to_string(), json!(blocked_by));
        }
        obj.insert("updated_at".to_string(), json!(machine::timestamp()));
    }

    if let Err(e) = save_task(&store, &namespace, &id, vector, task.payload.clone()).await {
        return e;
    }
    let event_id = arg_str(&args, "event_id")
        .unwrap_or(&format!("update-{}", id))
        .to_string();
    let session_for_receipt = arg_str(&args, "session_id")
        .map(String::from)
        .unwrap_or_else(|| {
            task.task_field("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        });
    {
        let mut r = TASK_RECEIPTS.lock().unwrap_or_else(|e| e.into_inner());
        let binding =
            crate::task::receipt::Receipt::capture(&id, &crate::daemon::resolve_project_root());
        r.record(crate::receipts::store::Receipt {
            session_id: session_for_receipt,
            task_id: id.clone(),
            event_id,
            outcome: "updated".to_string(),
            exposed_ids: vec![format!("{}:{}", namespace, id)],
            worktree: binding.worktree,
            head_sha: binding.head_sha,
            dirty_tree_hash: binding.dirty_tree_hash,
        });
    }
    // Byte-compat: both legacy keys stay on the wire. The nudge is retired
    // (always null) because a legacy `queries[]` stamp can no longer certify
    // graph lookup; receipt-based advisories supersede it in `task_validate`.
    let graph_queries_count = task
        .payload
        .metadata
        .get("task")
        .and_then(|t| t.get("graph_queries_count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    encode(&json!({
        "id": id,
        "namespace": namespace,
        "status": task.status().as_str(),
        "transition": transition,
        "renamed": rename_note,
        "task": task_summary(&id, &task.payload),
        "graph_queries_count": graph_queries_count,
        "graph_lookup_nudge": Value::Null,
    }))
}

// ── tool: neurostrata_task_list ────────────────────────────────────────────

pub async fn handle_task_list(args: Value, store: Arc<dyn VectorStore>) -> String {
    let namespace = match required_str(&args, "namespace") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }

    let status_filter = match arg_str(&args, "status") {
        None => None,
        Some(s) => match Status::parse(s) {
            Some(st) => Some(st),
            None => {
                return format!(
                    "ERROR: unknown status '{}'. Valid: open, in_progress, blocked, done.",
                    s
                )
            }
        },
    };
    let assignee_filter = arg_str(&args, "assignee").map(|s| s.to_string());
    let include_done = args
        .get("include_done")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ready_only = args.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
    // Retired rows are history, and history is not work: the gate does not
    // judge them and the snapshot does not offer them, so the listing does
    // not either. The opt-in shows them, labelled, for the audit.
    let include_retired = args
        .get("include_retired")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Task rows only: the listing and the blocker statuses both read tasks and
    // nothing else, and listing the whole namespace grew the daemon ~100 MB per
    // call.
    let memories = match store.list(&namespace, None, Some("task")).await {
        Ok(m) => m,
        Err(e) => {
            return format!(
                "ERROR: could not list tasks in '{}': {}. Check neurostrata_list_namespaces.",
                namespace, e
            )
        }
    };
    let statuses = task_statuses(&memories);
    let now = chrono::Utc::now().timestamp();

    let mut tasks: Vec<&SearchResult> = memories.iter().filter(|m| gate::is_task(m)).collect();
    // Done is hidden by default: finished work is history, not the backlog.
    // An explicit status filter is the caller asking for it directly.
    if let Some(filter) = status_filter {
        tasks.retain(|m| gate::task_status(m) == filter);
    } else if !include_done {
        tasks.retain(|m| gate::task_status(m) != Status::Done);
    }
    if let Some(who) = &assignee_filter {
        tasks.retain(|m| {
            m.payload
                .metadata
                .get("task")
                .and_then(|t| t.get("assignee"))
                .and_then(|a| a.as_str())
                == Some(who.as_str())
        });
    }
    if !include_retired {
        tasks.retain(|m| !gate::is_retired(&m.payload.metadata, now));
    }
    if ready_only {
        tasks.retain(|m| {
            gate::task_status(m) == Status::Open
                && !gate::is_retired(&m.payload.metadata, now)
                && is_ready(&m.payload, &statuses)
        });
    }

    // Highest priority first; ties keep the list deterministic by id.
    tasks.sort_by(|a, b| {
        priority_of(&a.payload)
            .cmp(&priority_of(&b.payload))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut summaries: Vec<Value> = tasks
        .iter()
        .map(|m| {
            let retired = gate::is_retired(&m.payload.metadata, now);
            let ready =
                gate::task_status(m) == Status::Open && !retired && is_ready(&m.payload, &statuses);
            let mut summary = task_summary(&m.id, &m.payload);
            if let Some(obj) = summary.as_object_mut() {
                obj.insert("ready".to_string(), json!(ready));
                if retired {
                    // A row shown as history must say so, so nobody reads it as
                    // live claimed work.
                    obj.insert("retired".to_string(), json!(true));
                }
            }
            summary
        })
        .collect();

    // Enhancement 1: stateless plugins (cli-hook.mjs, resumed sessions) resolve
    // the active task's id + subagent_prompt_injection in one task_list call.
    // For InProgress tasks: if cached subagent_prompt_injection starts with the
    // standard header "### NeuroStrata Task & Graph Context", ALWAYS recompute
    // via build_task_graph_context (so archived rules / new rules are reflected).
    // If cached does NOT start with that header (synthetic test fixture), keep it.
    for (i, m) in tasks.iter().enumerate() {
        if gate::task_status(m) == Status::InProgress {
            let expected_goal = format!("| Goal: {}\n", m.payload.content);
            let cached = m
                .payload
                .metadata
                .get("task")
                .and_then(|t| t.get("subagent_prompt_injection"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty() && s.contains(&expected_goal));
            let should_recompute = cached
                .map(|s| s.starts_with("### NeuroStrata Task & Graph Context"))
                .unwrap_or(true);
            if let Some(cached_str) = cached {
                if !should_recompute {
                    if let Some(obj) = summaries[i].as_object_mut() {
                        obj.insert("subagent_prompt_injection".to_string(), json!(cached_str));
                    }
                    continue;
                }
            }
            let vector = load_task(&store, &namespace, &m.id)
                .await
                .map(|(_task, vec)| vec)
                .unwrap_or_default();
            let (_hits, prompt) =
                build_task_graph_context(&store, &namespace, &m.id, &vector, &m.payload).await;
            if let Some(obj) = summaries[i].as_object_mut() {
                obj.insert("subagent_prompt_injection".to_string(), json!(prompt));
            }
        }
    }

    encode(&json!({
        "namespace": namespace,
        "count": summaries.len(),
        "tasks": summaries,
    }))
}

// ── tool: neurostrata_task_complete ────────────────────────────────────────

/// The exact -32603 body from section 3.2: it names both ways to satisfy
/// Lock 2, so the failure is also the instruction for fixing it.
pub(crate) fn blocked_message(id: &str) -> String {
    format!(
        "BLOCKED (Lock 2): task {} cannot complete without an extracted memory. Either call neurostrata_add_memory with metadata extracted_from: [\"{}\"], or retry neurostrata_task_complete with `memory.content` (what did you learn? what rule does this task prove?) or `link_memory_id`.",
        id, id
    )
}

/// Q2 -- the Lock 2 enforcement point. Success requires at least one inbound
/// EXTRACTED_FROM edge *after* the call, checked in the design's order:
/// pre-existing edge, `link_memory_id` re-upsert (no re-embed), inline
/// `memory` through the add_memory pipeline. The inline `memory` accepts an
/// optional `metadata` object and persists it exactly as add_memory would
/// (caller keys verbatim; `extracted_from` stamped with this task if absent;
/// non-object metadata rejected) -- a dropped `source` here is a
/// `rule_without_source` gate violation one call later. Failure is `Err`,
/// which the dispatcher turns into a JSON-RPC -32603 -- never a success payload.
pub async fn handle_task_complete(
    args: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    dedup: Option<Arc<DeduplicationChecker>>,
    bus: Arc<crate::events::ThalamicBus>,
) -> Result<String, String> {
    let id = required_str(&args, "id")?.to_string();
    let namespace_arg = required_str(&args, "namespace")?;
    let namespace = crate::server::resolve_namespace(&store, namespace_arg).await;
    check_namespace(&namespace)?;
    precheck::normalize_and_precheck_namespace(&*store, &namespace).await?;

    let (task, vector) = load_task(&store, &namespace, &id).await?;
    // The clock the gate reads with: a lesson row that has been retired is
    // history and discharges nothing, so the completion path asks the same
    // question the gate asks.
    let now = chrono::Utc::now().timestamp();
    if task.status() == Status::Done {
        return Err(format!(
            "ERROR: task {} is already done. Reopen it first (neurostrata_task_update with status 'open') if there is more work.",
            id
        ));
    }

    let reason = arg_str(&args, "reason")
        .or_else(|| arg_str(&args, "close_reason"))
        .unwrap_or("")
        .to_string();
    let link_memory_id = arg_str(&args, "link_memory_id").map(|s| s.to_string());
    let memory = args.get("memory").cloned();

    // M3: optional inline follow_up_tasks. Validate everything before any
    // mutation so a bad entry cannot leave the parent half-completed.
    type FollowUpSpec = (String, Option<String>, String, i64, Vec<String>);
    let mut follow_up_specs: Vec<FollowUpSpec> = Vec::new();

    // Task intent enforcement: reject if intent markers present without follow_up_tasks or task ID citation
    let mut full_text = String::new();
    if !reason.is_empty() {
        full_text.push_str(&reason);
    }
    if let Some(mem_content) = memory
        .as_ref()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        full_text.push_str(mem_content);
    }
    // A passed-but-empty list is defer, not compliance: `follow_up_tasks: []`
    // is not an explicit follow-up plan.
    let has_follow_up = args
        .get("follow_up_tasks")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !full_text.is_empty() {
        use crate::task::intent::detect_task_intents;
        let signals = detect_task_intents(&full_text);
        if !signals.is_empty() && !has_follow_up && link_memory_id.is_none() {
            return Err(format!(
                "BLOCKED (Lock 2): task {} cannot complete without an explicit follow_up_tasks or task ID citation. Task intent markers detected. Either populate `follow_up_tasks` with new tasks or cite an existing task ID via `link_memory_id`. Detected markers: {:?}.",
                id, signals
            ));
        }
    }

    if let Some(fu) = args.get("follow_up_tasks") {
        if !fu.is_null() {
            let arr = match fu.as_array() {
                Some(a) => a,
                None => {
                    return Err("ERROR: 'follow_up_tasks' must be an array.".to_string());
                }
            };
            for (i, item) in arr.iter().enumerate() {
                if !item.is_object() {
                    return Err(format!("ERROR: follow_up_tasks[{}] must be an object.", i));
                }
                let title = match item.get("title").and_then(|t| t.as_str()) {
                    Some(t) => match check_title(t) {
                        Ok(v) => v,
                        Err(e) => {
                            return Err(format!("ERROR: follow_up_tasks[{}].title: {}", i, e))
                        }
                    },
                    None => {
                        return Err(format!(
                            "ERROR: follow_up_tasks[{}] requires a non-empty 'title'.",
                            i
                        ))
                    }
                };
                let description = item
                    .get("description")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
                let task_type = match item.get("task_type").and_then(|t| t.as_str()) {
                    None => "task".to_string(),
                    Some(t) if ["task", "bug", "feature", "epic"].contains(&t) => t.to_string(),
                    Some(t) => {
                        return Err(format!(
                            "ERROR: follow_up_tasks[{}].task_type '{}' invalid.",
                            i, t
                        ))
                    }
                };
                let priority = match item.get("priority") {
                    None => 2i64,
                    Some(Value::Number(n))
                        if n.as_i64().map(|p| (0..=4).contains(&p)).unwrap_or(false) =>
                    {
                        n.as_i64().unwrap_or(2)
                    }
                    Some(_) => {
                        return Err(format!(
                            "ERROR: follow_up_tasks[{}].priority must be an integer 0-4.",
                            i
                        ))
                    }
                };
                let labels = match string_list(item, "labels") {
                    Ok(v) => v,
                    Err(e) => return Err(format!("ERROR: follow_up_tasks[{}].labels: {}", i, e)),
                };
                follow_up_specs.push((title, description, task_type, priority, labels));
            }
        }
    }
    // Lock 2 — Task Obligation: undisposed high-confidence obligations block
    // completion until they are promoted (neurostrata_task_create) or dismissed
    // (neurostrata_task_candidates action: "resolve"). Placed BEFORE any memory
    // extraction/upsert side effects (Path 1/2/3) so a refusal leaves no
    // orphan memory row behind.
    let intake_store = store.as_intake_store();
    if let Some(intake) = intake_store {
        let obligations =
            undisposed_high_confidence_obligations(intake, &namespace, None, Some(id.as_str()))
                .await
                .unwrap_or_default();
        if !obligations.is_empty() {
            let mut msg = format!(
                "BLOCKED (Lock 2 — Task Obligation): task {} cannot complete: {} undisposed high-confidence task obligation(s) must be resolved before completion.\n",
                id,
                obligations.len()
            );
            for o in &obligations {
                msg.push_str(&format!(
                    "  - {}: {}\n    promote via: neurostrata_task_create | dismiss via: neurostrata_task_candidates action: \"resolve\" namespace: \"{}\" candidate_id: \"{}\" disposition: \"dismissed\"\n",
                    o.id,
                    o.title,
                    namespace,
                    o.id
                ));
            }
            return Err(msg);
        }
    }

    let actor = task
        .task_field("assignee")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let list_now = || async {
        store
            .list(&namespace, None, None)
            .await
            .map_err(|e| format!("Failed to list memories in '{}': {}", namespace, e))
    };

    // Path 1: the edge already exists (a memory written earlier with
    // `extracted_from: [id]`).
    let mut memories = list_now().await?;
    let mut edge = gate::extraction_exists(&memories, &id, now);

    // Path 2: bind an already-written memory. Its existing vector is reused
    // verbatim -- linking must never change what the memory ranks for.
    if !edge {
        if let Some(link_id) = &link_memory_id {
            if link_id == &id {
                return Err(format!(
                    "ERROR: link_memory_id must be a memory, not the task itself ('{}').",
                    id
                ));
            }
            let (existing_vector, mut payload) = match store.get(&namespace, link_id).await {
                Ok(Some(found)) => found,
                Ok(None) => {
                    return Err(format!(
                        "ERROR: no memory with id '{}' in namespace '{}' to link.",
                        link_id, namespace
                    ))
                }
                Err(e) => return Err(format!("ERROR: could not read memory '{}': {}", link_id, e)),
            };
            if !payload.metadata.is_object() {
                return Err(format!(
                    "ERROR: memory '{}' has legacy-shaped metadata (not a JSON object) and cannot be linked. Re-add it with object metadata first.",
                    link_id
                ));
            }
            let declared = payload.metadata.get("extracted_from").cloned();
            let mut arr: Vec<Value> = match declared {
                None => Vec::new(),
                Some(Value::Array(a)) => a,
                Some(_) => {
                    return Err(format!(
                        "ERROR: memory '{}' has a malformed 'extracted_from' (it must be an array of task ids).",
                        link_id
                    ))
                }
            };
            if !arr.iter().any(|v| v.as_str() == Some(id.as_str())) {
                arr.push(json!(id.as_str()));
            }
            payload
                .metadata
                .as_object_mut()
                .expect("object checked above")
                .insert("extracted_from".to_string(), Value::Array(arr));
            store
                .upsert(&namespace, link_id, existing_vector, payload)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to link memory '{}' to task '{}': {}",
                        link_id, id, e
                    )
                })?;
            // Verified after the write: the rule is an edge *after the call*.
            memories = list_now().await?;
            edge = gate::extraction_exists(&memories, &id, now);
        }
    }

    // Path 3: write the extraction inline, through the same add_memory
    // pipeline (embed, secret scan, dedup check, upsert) with the edge stamped
    // on the way in.
    //
    // The lesson's id is held here rather than at the write: the pulse that
    // announces it fires below, after the gate, and a refusal must publish
    // nothing.
    let mut inline_lesson: Option<String> = None;
    let mut pending_registry: Option<Vec<String>> = None;
    let mut registry_warnings: Vec<String> = Vec::new();
    if !edge {
        if let Some(mem) = &memory {
            if !mem.is_object() {
                return Err(
                    "ERROR: 'memory' must be an object with at least a non-empty 'content'."
                        .to_string(),
                );
            }
            let content = match mem.get("content").and_then(|c| c.as_str()) {
                Some(c) if !c.trim().is_empty() => c.to_string(),
                _ => {
                    return Err(format!(
                        "The `memory` argument needs a non-empty `content`: {}",
                        blocked_message(&id)
                    ))
                }
            };
            let memory_type = mem
                .get("memory_type")
                .and_then(|m| m.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("fact")
                .to_string();
            let user_id = arg_str(&args, "user_id").unwrap_or("unknown").to_string();
            let agent_name = arg_str(&args, "agent_name").map(|s| s.to_string());
            // The caller's metadata is the base; add_memory persists the
            // metadata it is given, so the completion handoff does no less. A
            // dropped key here is a dropped source: the gate would reject the
            // rule one call later for provenance the caller declared.
            // `metadata: null` is treated as absent (lenient), DELIBERATELY:
            // on the completion path the whole `memory` itself is optional,
            // so a caller leaving it out and one passing null mean the same
            // thing. add_memory's strict null rejection guards only the
            // explicit-tool path there; a wrong-typed metadata (string,
            // array, scalars) still errors below, mirroring add_memory.
            // Do NOT "fix" this arm to reject null without moving the
            // memory-optional framing with it.
            let mut meta = match mem.get("metadata") {
                None | Some(Value::Null) => json!({}),
                Some(m) if m.is_object() => m.clone(),
                Some(_) => {
                    return Err(
                        "ERROR: 'memory.metadata' must be a JSON object (key-value map), not a string, array, or null.".to_string()
                    )
                }
            };
            if !meta
                .get("extracted_from")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| arr.iter().any(|t| t.as_str() == Some(id.as_str())))
            {
                match meta.get_mut("extracted_from") {
                    None => meta["extracted_from"] = json!([id.as_str()]),
                    Some(Value::Array(arr)) => arr.push(json!(id.as_str())),
                    Some(_) => {
                        return Err(
                            "ERROR: memory metadata has a malformed 'extracted_from' (it must be an array of task ids).".to_string()
                        )
                    }
                }
            }
            let mut location = String::new();
            let mut location_lines = String::new();
            if let Some(locations) = mem.get("locations").and_then(|l| l.as_array()) {
                let (first, lines) = crate::server::first_location(locations);
                location = first;
                location_lines = lines;
                crate::server::stamp_locations(&mut meta, locations);
            }
            // The same lineage stamps add_memory gives every new row.
            crate::server::stamp_new_memory(&mut meta);
            // A procedure minted by an extraction is locally authored,
            // exactly as if add_memory had written it (Program Task 3).
            if memory_type == "procedure" {
                crate::tools::procedure::stamp_local_trust(&mut meta);
            }
            // An extraction is an insertion: it meets the same scanner.
            if let Some(rejection) =
                crate::secrets::scan_entry_point(&content, &meta, "task_complete")
            {
                return Err(rejection.to_string());
            }
            // The completion's slug comes from `memory.metadata.domain`: there
            // is no top-level `domain` argument on this path, so
            // `register_domain` applies to that field and is a no-op when the
            // memory carries no domain. The decision precedes the write.
            // Mirror handle_add_memory: normalize the domain to its slug before
            // the registry check so canonical keys agree across the two paths.
            if let Some(slug) = crate::domain::domain_key(&meta) {
                if let Some(meta_obj) = meta.as_object_mut() {
                    meta_obj.insert("domain".to_string(), Value::String(slug));
                }
            }
            if let Some(raw) = meta.get("domain").cloned() {
                let register_domain = args
                    .get("register_domain")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match crate::registry::check_domain(&store, &namespace, &raw, register_domain).await
                {
                    Ok(check) => {
                        pending_registry = check.cache;
                        if let Some(warning) = check.warning {
                            registry_warnings.push(warning);
                        }
                    }
                    Err(rejection) => return Err(rejection),
                }
            }
            let payload = MemoryPayload {
                content,
                user_id,
                memory_type,
                agent_name,
                location,
                location_lines,
                metadata: meta,
            };
            let report = crate::server::embed_dedup_and_upsert(
                &store,
                &emb,
                &namespace,
                &payload,
                dedup.as_ref(),
            )
            .await?;
            // If the extraction was blocked by dedup rather than written, do
            // not fall through to the generic Lock 2 refusal: name the
            // candidate so the agent can retry with link_memory_id instead of
            // looping on the same content.
            if matches!(
                report.decision,
                crate::judgment::policy::DedupDecision::BlockedExact
                    | crate::judgment::policy::DedupDecision::BlockedModel
            ) {
                let candidate = report
                    .linked_existing_id
                    .clone()
                    .or_else(|| report.dedup_hint.as_ref().map(|h| h.candidate_id.clone()))
                    .unwrap_or_else(|| "<unknown>".to_string());
                return Err(format!(
                    "ERROR: extraction was blocked by dedup ({:?}). The lesson matches an existing memory '{}'. Retry neurostrata_task_complete with link_memory_id: \"{}\" instead of memory.content, or pass a materially different content.",
                    report.decision, candidate, candidate
                ));
            }
            // Only now, with the extraction durable: a cache failure is
            // reported and never undoes the acceptance.
            if let Some(domains) = pending_registry.take() {
                if let Some(warning) =
                    crate::registry::write_registry(&store, &emb, &namespace, &domains).await
                {
                    registry_warnings.push(warning);
                }
            }
            memories = list_now().await?;
            edge = gate::extraction_exists(&memories, &id, now);
            let lesson_id = report
                .upsert
                .as_ref()
                .map(|u| u.id.clone())
                .or(report.linked_existing_id.clone())
                .filter(|s| !s.is_empty());
            inline_lesson = lesson_id;
        }
    }

    if !edge {
        return Err(blocked_message(&id));
    }

    // Emit site 3. "The task gave way to the lesson" -- the same `Superseded`
    // a correction uses, because both are a row being replaced by a row that
    // carries forward what it established. Only this path emits: Path 1 and
    // Path 2 add an edge to a memory that already existed, and there is no
    // move to announce.
    //
    // After the gate, not before it: the pulse claims the transition landed,
    // so a Lock 2 refusal above must leave the bus silent rather than send
    // every subscriber down a chain that does not exist. The ordering is the
    // invariant -- nothing re-derives it from the adjacent check.
    if let Some(lesson_id) = inline_lesson {
        bus.emit(
            crate::events::ThalamicPulse::Superseded {
                old_id: id.clone(),
                new_id: lesson_id,
                namespace: namespace.clone(),
            },
            &crate::events::RecursionToken::root(),
        );
    }

    // The completing session travels into the "done" history entry: without
    // it the claim/exclusivity trail stops at the finish line.
    let caller_session = arg_str(&args, "session_id")
        .map(|s| s.to_string())
        .or_else(|| {
            task.task_field("session_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    // With the edge in place the ExtractionRequired guard is satisfied.
    let ctx = Ctx {
        reason: reason.clone(),
        extraction_edge_exists: true,
        actor,
        session: caller_session.unwrap_or_default(),
    };
    let mut completed = apply(&task, Status::Done, &ctx).map_err(|e| format!("ERROR: {}", e))?;
    if !reason.trim().is_empty() {
        if let Some(meta) = completed.payload.metadata.as_object_mut() {
            if let Some(t) = meta.get_mut("task").and_then(|v| v.as_object_mut()) {
                t.insert("close_reason".to_string(), json!(reason.trim()));
            }
        }
    }
    save_task(&store, &namespace, &id, vector, completed.payload.clone()).await?;
    let event_id = arg_str(&args, "event_id")
        .unwrap_or(&format!("complete-{}", id))
        .to_string();
    let session_for_receipt = arg_str(&args, "session_id")
        .map(String::from)
        .unwrap_or_else(|| {
            task.task_field("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        });
    {
        let mut r = TASK_RECEIPTS.lock().unwrap_or_else(|e| e.into_inner());
        // Bind the completion receipt to the worktree state captured right now
        // (AC5): retention verification later re-checks this binding.
        let binding =
            crate::task::receipt::Receipt::capture(&id, &crate::daemon::resolve_project_root());
        r.record(crate::receipts::store::Receipt {
            session_id: session_for_receipt.clone(),
            task_id: binding.task_id.clone(),
            event_id: event_id.clone(),
            outcome: "completed".to_string(),
            exposed_ids: vec![format!("{}:{}", namespace, id)],
            worktree: binding.worktree,
            head_sha: binding.head_sha,
            dirty_tree_hash: binding.dirty_tree_hash,
        });
        // AC5: the completion receipt must be retained and verifiable — ask
        // the retention gate right here so a lost receipt is caught on the spot.
        let retention_ok = crate::retention::gate::verify_task_receipt_retention(
            &r,
            &session_for_receipt,
            &id,
            &event_id,
        );
        if !retention_ok {
            tracing::warn!("AC5 receipt retention check failed for task {}", id);
        }
    }

    // M3: spawn follow-up tasks now that the parent is done. Each inherits
    // the caller's user/agent defaults and carries spawned_from + related_to.
    let follow_user_id = arg_str(&args, "user_id").unwrap_or("unknown").to_string();
    let follow_agent = arg_str(&args, "agent_name").map(|s| s.to_string());
    let mut follow_up_created_ids: Vec<String> = Vec::new();
    for (title, description, task_type, priority, labels) in &follow_up_specs {
        let mut task_meta = Map::new();
        task_meta.insert("priority".to_string(), json!(*priority));
        task_meta.insert("task_type".to_string(), json!(task_type));
        task_meta.insert("spawned_from".to_string(), json!(id.as_str()));
        if !labels.is_empty() {
            task_meta.insert("labels".to_string(), json!(labels));
        }
        if let Some(d) = description {
            task_meta.insert("description".to_string(), json!(d));
        }
        let mut top_level = Map::new();
        top_level.insert("related_to".to_string(), json!([id.as_str()]));
        match store_new_task(
            &store,
            &emb,
            &namespace,
            title,
            &follow_user_id,
            follow_agent.as_deref(),
            task_meta,
            top_level,
        )
        .await
        {
            Ok((new_id, _payload)) => follow_up_created_ids.push(new_id),
            Err(e) => return Err(e),
        }
    }

    let extracted: Vec<String> = memories
        .iter()
        .filter(|m| {
            m.id != id
                && m.payload
                    .metadata
                    .get("extracted_from")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().any(|t| t.as_str() == Some(id.as_str())))
                    .unwrap_or(false)
        })
        .map(|m| m.id.clone())
        .collect();

    // M3: task_intent_signals from reason + memory.content
    let mut scan_text = reason.clone();
    if let Some(mem) = &memory {
        if let Some(c) = mem.get("content").and_then(|v| v.as_str()) {
            scan_text.push(' ');
            scan_text.push_str(c);
        }
    }
    let signals: Vec<Value> = crate::task::intent::detect_task_intents(&scan_text)
        .into_iter()
        .map(|s| {
            json!({
                "marker": s.marker,
                "excerpt": s.excerpt,
            })
        })
        .collect();

    // Capability E: post-commit learning hook. Best-effort by design — a
    // mining/reflection failure is reported and logged, never allowed to roll
    // back the already-committed task or make completion look successful when
    // reflection did not happen. The hook never completes a task, so no
    // recursion back into this handler is possible.
    let learning_report = crate::learning::hooks::on_task_completed(
        &store,
        &emb,
        &namespace,
        &id,
        dedup.as_ref(),
        &crate::daemon::resolve_project_root(),
    )
    .await;

    let mut reply = json!({
        "task": task_summary(&id, &completed.payload),
        "extracted_memory_ids": extracted,
        "follow_up_tasks_created": follow_up_created_ids,
        "task_intent_signals": signals,
        "transition": { "from": task.status().as_str(), "to": "done" },
    });
    reply["learning_hook"] = serde_json::to_value(&learning_report)
        .unwrap_or_else(|_| json!({ "errors": ["serialize learning hook report"] }));
    if !registry_warnings.is_empty() {
        reply["registry_warning"] = json!(registry_warnings.join("; "));
    }

    serde_json::to_string_pretty(&reply).map_err(|e| format!("Internal serialization error: {}", e))
}

// ── tool: neurostrata_task_validate ────────────────────────────────────────

pub async fn handle_task_validate(args: Value, store: Arc<dyn VectorStore>) -> String {
    let namespace = match required_str(&args, "namespace") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }
    // One list scan serves both the gate and the R7 advisories: the gate
    // stays metadata-only and the advisory inputs come from the same rows,
    // so the two surfaces cannot disagree about which tasks are active.
    let memories = match store.list(&namespace, None, None).await {
        Ok(memories) => memories,
        Err(e) => return format!("ERROR: could not evaluate the task gate: {}", e),
    };
    let report = gate::evaluate_scoped(&memories, chrono::Utc::now().timestamp(), None);
    let mut json = report.validate_json();
    // F13: dangling-edge report (warn-only, never blocks gate).
    if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
        .downcast_ref::<crate::store::LadybugStore>()
    {
        match ls.dangling_edges(&namespace).await {
            Ok(edges) => {
                let count = edges.len();
                let samples: Vec<_> = edges.into_iter().take(5).collect();
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(
                        "dangling_edges".to_string(),
                        serde_json::json!({
                            "total": count,
                            "samples": samples,
                        }),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("dangling_edges query failed for '{namespace}': {e}");
            }
        }
        let sw = ls.shape_warnings(&namespace).await;
        if let Some(obj) = json.as_object_mut() {
            obj.insert("shape_warnings".to_string(), serde_json::Value::Array(sw));
        }
        let ow = ls.ontology_warnings(&namespace).await;
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "ontology_warnings".to_string(),
                serde_json::Value::Array(ow),
            );
        }
        // Enrichment state surface (F12/F14/F24), warn-first.
        let rid = crate::registry::registry_row_id(&namespace);
        match crate::traits::VectorStore::get(ls, &namespace, &rid).await {
            Ok(Some((_, reg_payload))) => {
                let es = crate::registry::enrichment_state(&reg_payload.metadata);
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("enrichment_state".to_string(), es.clone());
                    if es.get("state").and_then(|v| v.as_str()) == Some("failed") {
                        let warnings = obj
                            .entry("warnings".to_string())
                            .or_insert_with(|| serde_json::json!([]));
                        if let Some(arr) = warnings.as_array_mut() {
                            arr.push(serde_json::json!({"kind":"enrichment_failed","state":es}));
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("enrichment registry get failed for '{namespace}:{rid}': {e}");
            }
        }
    }
    // R7: enrich the gate's zero report with local post-dispatch receipts.
    // Ledger read is fail-soft: missing/unreadable means zero coverage, never
    // an error and never a gate violation.
    let active_ids = snapshot_readiness::active_task_ids(
        &memories
            .iter()
            .filter(|r| gate::is_task(r))
            .filter(|r| !gate::is_retired(&r.payload.metadata, chrono::Utc::now().timestamp()))
            .map(|r| (r.id.clone(), gate::task_status(r).as_str().to_string()))
            .collect::<Vec<_>>(),
    );
    // Same ledger-root policy as the receipt recorder: a caller worktree hint
    // wins, then registry roots, so a receipt written from a worktree is
    // visible to validation even when the daemon runs outside that checkout.
    let caller_hint = arg_str(&args, "worktree")
        .or_else(|| arg_str(&args, "project_root"))
        .or_else(|| arg_str(&args, "cwd"));
    let ledger: Value = receipt_ledger_root(caller_hint, &store, &namespace)
        .await
        .and_then(|root| {
            std::fs::read_to_string(root.join(".NeuroStrata").join("session_queries.json")).ok()
        })
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "events": [] }));
    let mut applicable: Vec<String> = args
        .get("applicable_triggers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if args.get("strategic").and_then(Value::as_bool) == Some(true) {
        applicable.push(graph_advisories::T_STRATEGIC.to_string());
    }
    applicable.extend(graph_advisories::applicable_triggers_from_ledger(
        &ledger,
        &active_ids,
    ));
    // ReBAC: restricted concept/thesis rows are neither counted nor exposed.
    // Reuse the same public-access policy the taxonomy view applies, so an
    // advisory cannot leak the existence of a private row through a boolean.
    let concept_rows: Vec<&SearchResult> = memories
        .iter()
        .filter(|r| {
            matches!(r.payload.memory_type.as_str(), "concept" | "thesis")
                && crate::rdf::taxonomy::is_public_access_policy(&r.payload.metadata)
        })
        .collect();
    let taxonomy_available = !concept_rows.is_empty();
    let concept_grounding_linked = concept_rows.iter().any(|r| {
        r.payload
            .metadata
            .get("extracted_from")
            .and_then(Value::as_array)
            .map(|ids| !ids.is_empty())
            .unwrap_or(false)
    });
    let advisories = graph_advisories::compute_advisories(&graph_advisories::AdvisoryInput {
        ledger: &ledger,
        active_task_ids: &active_ids,
        allowed_graph_tools: snapshot_readiness::GRAPH_TOOLS,
        applicable_trigger_ids: &applicable,
        taxonomy_available,
        concept_grounding_linked,
    });
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "advisories".to_string(),
            json!({ "graph_ontology": advisories.to_json() }),
        );
    }
    encode(&json)
}

// ── snapshot stickiness (section 6.1) ──────────────────────────────────────

/// The header `get_snapshot` prepends: the Zero-Action Start line plus the
/// claimed and ready work. Every session starts inside the task system
/// because the mandatory pre-flight tool carries it.
pub fn snapshot_prefix(memories: &[SearchResult]) -> String {
    let statuses = task_statuses(memories);
    let now = chrono::Utc::now().timestamp();
    let mut claimed: Vec<&SearchResult> = Vec::new();
    let mut ready: Vec<&SearchResult> = Vec::new();
    for m in memories {
        if !gate::is_task(m) {
            continue;
        }
        // Retired rows are history, not work in flight, and the gate does
        // not judge them. Offering one as claimed work would send an agent
        // to release a claim that no longer blocks anything.
        if gate::is_retired(&m.payload.metadata, now) {
            continue;
        }
        match gate::task_status(m) {
            Status::InProgress => claimed.push(m),
            Status::Open if is_ready(&m.payload, &statuses) => ready.push(m),
            _ => {}
        }
    }
    ready.sort_by(|a, b| {
        priority_of(&a.payload)
            .cmp(&priority_of(&b.payload))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut out = String::from("Zero-Action Start: claim or create a task before editing.\n");
    out.push_str("General-Purpose Project Memory: NeuroStrata is a general-purpose, project-oriented memory, knowledge-base, and ontology system — projects do not need to be coding or document-oriented. Store user-provided facts, domain knowledge, entities, digital twins, and rules directly via neurostrata_add_memory (`locations` are optional and only needed when anchoring to files/symbols).\n");
    out.push_str("Task recognition: a statement of future/deferred work ('TODO:', 'we should also', 'later we need to', 'remaining work:', ...) is a task, not a note — capture it with neurostrata_task_create.\n");
    out.push_str("Graph-First & Subagent Propagation: before blind grep/glob or editing files, query neurostrata_memories_for_file, neurostrata_related, neurostrata_search_memory (mode hybrid default, include_references true), or neurostrata_sparql_query for GOVERNS/DEPENDS_ON/calls/imports edges. Write this into your session plan and paste the `subagent_prompt_injection` block from neurostrata_task_claim into EVERY subagent prompt you spawn.\n");
    out.push_str("Formal Verification Reflex: when adding or modifying a state machine, gate, sovereignty/ACL rule, CRDT/merge rule, lease/claim rule, or conservation invariant, evaluate whether to factor the pure decision into src/formal_kernel.rs, prove its invariants in proofs/NeuroStrata/*.lean (zero sorry/axiom), and add parity rows in tests/lean_parity_test.rs.\n");
    if claimed.is_empty() && ready.is_empty() {
        out.push_str("No tasks are claimed or ready in this namespace.\n");
        return out;
    }
    if !claimed.is_empty() {
        out.push_str("Claimed (finish or release before pushing):\n");
        for m in claimed {
            let assignee = m
                .payload
                .metadata
                .get("task")
                .and_then(|t| t.get("assignee"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            out.push_str(&format!(
                "  - {} {} ({})\n",
                m.id, m.payload.content, assignee
            ));
        }
    }
    if !ready.is_empty() {
        out.push_str("Ready to claim:\n");
        for m in ready {
            out.push_str(&format!(
                "  - {} {} [P{}]\n",
                m.id,
                m.payload.content,
                priority_of(&m.payload)
            ));
        }
    }
    out
}

// ── tool: neurostrata_bootstrap ────────────────────────────────────────────

/// The AGENTS.md template bootstrap hands back (section 3.2): task rules
/// first, because Zero-Action Start has to be the first thing an agent reads.
fn agents_template(namespace: &str) -> String {
    format!(
        r#"# Agent Rules — {ns}

## 0. The Prime Directive: Never Defer Anything
No task, feature, or implementation may be left in a "deferred", "todo",
"stub", "phase-2", or "later" state. Every open item must be either completed
and committed in this session or filed as a tracked task via
`neurostrata_task_create` with assignee and description.

## 1. Zero-Action Start (mandatory)
Pre-flight every session with `neurostrata_get_snapshot` (`namespace: "{ns}"`).
No code or file edits until a task exists AND is claimed by this session:
1. `neurostrata_task_list` with `ready: true` to pick up existing work, or
2. `neurostrata_task_create` (namespace, title) then `neurostrata_task_claim`
   (id, namespace, assignee, session_id).
Never use TodoWrite, TaskCreate, `bd`, or markdown TODO lists: tasks live in
NeuroStrata, not in the repo.

## 2. Session completion (all three, in order)
1. `neurostrata_task_complete` on every claimed task. It is the only entrance
   to `done`, and it forces extraction (Lock 2): pass `memory.content` (what did
   this task teach? what rule does it prove?) or `link_memory_id`. An inline
   `memory.metadata` is persisted exactly as add_memory persists one -- pass
   `source` and `governs` there for a rule that must carry provenance.
2. `neurostrata-mcp task gate {ns} --strict` must exit 0 (Lock 1).
3. `git pull --rebase` then `git push` (the pre-push hook runs the gate).

## 3. Mid-task state & Episodic Buffer (Lock 3 — Breath prompt)
Every 3–5 mutation steps, pause and append a state summary on the task via
`neurostrata_task_update` (`note` appends to history). Maintain a running
conversational transcript via `neurostrata_append_log`. The task record IS
Tier-3 task memory -- recover a session with `neurostrata_task_list` +
`neurostrata_get_memory`, not log archaeology.

## 4. Memory, Knowledge Base & Automatic Graph-First Propagation
NeuroStrata is a general-purpose, project-oriented memory, knowledge-base, and
ontology system — projects do not need to be coding or document-oriented.
Domain facts, user-told knowledge, entities, digital twins, architectural rules,
decisions, and fixes -> `neurostrata_add_memory` (`locations` are optional and
used only when anchoring to specific files or code symbols; correct outdated
records via `neurostrata_supersede_memory`; never bulk-delete).
- **Graph-First Lookup:** Before blind `grep`/`glob` or editing files, query `neurostrata_memories_for_file`, `neurostrata_related`, `neurostrata_trace_call_chain`, `neurostrata_impact_analysis`, `neurostrata_search_memory` (`mode`: `"hybrid"` (default), `include_references`: true), or `neurostrata_sparql_query` for `GOVERNS`, `DEPENDS_ON`, and AST `calls`/`imports` edges.
- **Subagent Prompt Propagation:** Subagents start with a blank context window. Write the Graph-First rule into your session plan on init, and paste the `subagent_prompt_injection` block returned by `neurostrata_task_claim` into EVERY subagent prompt you spawn.
- **Zero-Touch Harness Plugin Auto-Install & Hooks:** On init (`neurostrata_bootstrap`, `neurostrata_task_setup`, and `neurostrata_get_snapshot`), NeuroStrata automatically detects the active CLI/terminal agent harness (`opencode`, `openclaw`, `claude`, `hermes`, `codex`, `github-cli`), installs/wires the zero-dependency plugin under `.NeuroStrata/plugins/<harness>/`, and self-heals both `.git/hooks/pre-push` and `.git/hooks/pre-commit`.

## 5. Task vs procedure (do not confuse)
A `task` is a unit of work that finishes (it must reach `done` via
`neurostrata_task_complete` -- Lock 2). A `procedure` is a knowing-how
routine with a finite rehearsal budget; it is fired (not finished) by
`neurostrata_procedure_perform`, which decrements `remaining_fires` and
writes one Episodic Buffer entry per firing.
- Use a TASK for: work that completes once (or a recurring pattern where
  each cycle is a new task). One cycle = one task, created fresh.
- Use a PROCEDURE for: a finite-budget rehearsal or a recurring prompt
  the agent should acknowledge on a trigger (`session-start`, `before-edit`,
  `after-mutation`, `every-n-sessions:N`, `every-n-days:N`).
- Procedure creation path (there is no `procedure_create` tool):
  `neurostrata_add_memory` with `memory_type: "procedure"` and metadata
  fields `trigger` / `remaining_fires` / `valid_to` / `performance_count` /
  `last_performed_at` / `last_episodic_pointer`. Then stamp each firing with
  `neurostrata_procedure_perform`.
- `task_create` rejects `task_type: "recurring"` -- the rejection error
  itself points at this section.

## 6. Task recognition (heuristic)
A statement of future/deferred work is a TASK, not a note:
- Explicit tags: `TODO:`, `FIXME:`, `follow-up:`, `next step:`, `action item:`,
  `remaining work:`.
- Modal deferral: `we should also`, `later we need to`, `still need to`,
  `must also fix`, `need to follow up`, `filed follow-up`, `defer`/`deferred`.
When you see one in user/agent speech, logs, or memory content, capture it
with `neurostrata_task_create` (namespace, title) instead of letting it
evaporate. The lexical detector surfaces these as `task_intent_signals` in
`neurostrata_append_log` and as a `TASK NUDGE` line in `neurostrata_add_memory`.

## 7. SynapticMesh, Security & Infrastructure Constraints
- **SynapticMesh Honesty:** Only `eligibility: "shareable"` rule/fact/procedure definitions cross the mesh (stripped of local runtime counters). Private rows are sovereign; preserve mesh conflicts visibly and never force-push the mesh repo.
- **Daemon Caller-Path Authority (`J6`):** Caller-supplied filesystem paths on `/ingest`, `/backup`, `/export-graph`, and `/tasks/import` must reside within the project root or `allowed_external_dirs` in `~/.config/neurostrata/config.json`.
- **Tooling & Safety:** Never delete the shared LadybugDB directory or tables. Always use `podman` (never `docker`), strict JSON/JSON Schema, and non-interactive CLI flags.
"#,
        ns = namespace
    )
}

pub async fn handle_bootstrap(
    args: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    let namespace = match required_str(&args, "namespace") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let project_root = match required_str(&args, "project_root") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }
    let project_description = arg_str(&args, "project_description").map(|s| s.to_string());
    let root = std::path::Path::new(project_root);
    let harness_plugins = harness_plugins::auto_install_detected_plugins(
        root,
        harness_plugins::ConfigNamespace::Set(&namespace),
        arg_str(&args, "harness"),
    );
    let (wiring_block, verified, wire_instruction) = wiring_panel(root, &namespace);
    let hook_installed = verified["hook_installed"].as_bool().unwrap_or(false);

    // The instructor leaves concrete work: first_task is created here, so the
    // very first action an agent can take is already inside the system.
    let title = "Ingest codebase AST and write 3 initial architectural rules";
    let mut task_meta = Map::new();
    task_meta.insert("priority".to_string(), json!(1));
    task_meta.insert("task_type".to_string(), json!("task"));
    if let Some(desc) = &project_description {
        task_meta.insert("description".to_string(), json!(desc));
    }
    let (task_id, _) = match store_new_task(
        &store,
        &emb,
        &namespace,
        title,
        "unknown",
        Some("neurostrata-bootstrap"),
        task_meta,
        Map::new(),
    )
    .await
    {
        Ok(found) => found,
        Err(e) => return e,
    };

    let mut instructions: Vec<Value> = Vec::new();
    let mut step = 1;
    instructions.push(
        json!({ "step": step, "kind": "action", "action": "write_file", "path": "AGENTS.md" }),
    );
    step += 1;
    if !hook_installed {
        instructions.push(json!({ "step": step, "kind": "action", "action": "run", "cmd": "neurostrata-mcp hooks install" }));
        step += 1;
    }
    instructions.push(json!({ "step": step, "kind": "action", "action": "call_tool", "tool": "neurostrata_task_claim",
      "params": { "id": task_id.as_str(), "namespace": namespace.as_str(),
                  "assignee": "this-session", "session_id": "this-session" } }));
    step += 1;
    instructions.push(json!({ "step": step, "kind": "action", "action": "call_tool", "tool": "neurostrata_ingest_directory",
      "params": { "dir_path": project_root, "namespace": namespace.as_str() } }));
    step += 1;

    // The wiring panel's step (Q2): gate the checks at the project's
    // chokepoint -- git's pre-push by default, the project's ship step
    // otherwise. One template, git named first.
    let mut wire_step = wire_instruction;
    wire_step["step"] = json!(step);
    instructions.push(wire_step);

    // Non-git projects have no default gate point: exactly one task, so the
    // decision lands in a Goal and its resolution lands in memory.
    let mut tasks_created: Vec<Value> = Vec::new();
    if !verified["gate_point_resolved"].as_bool().unwrap_or(false) {
        let gp_title = "Identify and wire the project gate point, record it as a rule memory";
        let mut gp_meta = Map::new();
        gp_meta.insert("priority".to_string(), json!(1));
        gp_meta.insert("task_type".to_string(), json!("task"));
        gp_meta.insert(
            "description".to_string(),
            json!("No git detected. Find the ONE step every unit of work must pass to leave this project (CMS publish, render submission, publish script, review approval), wire `neurostrata-mcp task gate <ns> --strict` into it, and record the choice with neurostrata_add_memory (memory_type: rule)."),
        );
        match store_new_task(
            &store,
            &emb,
            &namespace,
            gp_title,
            "unknown",
            Some("neurostrata-bootstrap"),
            gp_meta,
            Map::new(),
        )
        .await
        {
            Ok((id, _)) => tasks_created.push(json!({ "id": id, "title": gp_title })),
            Err(e) => return e,
        }
    }

    encode(&json!({
        "namespace": namespace,
        "files": [
            { "path": "AGENTS.md", "overwrite": false, "content": agents_template(&namespace) },
            { "path": ".NeuroStrata/docs/.gitkeep", "overwrite": false, "content": "" }
        ],
        "first_task": { "id": task_id, "title": title },
        "harness_plugins": harness_plugins,
        "tasks_created": tasks_created,
        "hooks": { "install_command": "neurostrata-mcp hooks install" },
        "instructions": instructions,
        "wiring": wiring_block,
        "verified": verified,
        "rule": "Zero-Action Start is now active: no file edits without a claimed task.",
    }))
}

// ── the wiring panel (docs/design-wiring-panel.md) ─────────────────────────

/// The scaffolded `scripts/hooks/pre-push` the bootstrap hands a git project.
///
/// FINDING-bootstrap-wiring-nudge: the path of least resistance was a CI
/// workflow because that is where the other guards already lived. Shipping a
/// runnable hook flips it — an agent extends this file instead of inventing a
/// workflow. Both invocations are host-agnostic (GitHub, GitLab, Gitea, a bare
/// repo over SSH), which is the whole reason the hook is the default.
pub(crate) const SCAFFOLDED_PRE_PUSH_HOOK: &str = r#"#!/bin/bash
# NeuroStrata task gate — scaffolded by neurostrata-bootstrap / task_setup.
#
# The hook IS the gate. It runs on every push on ANY git host, so it is the
# required default; CI is vendor-specific and only a defense-in-depth extra.
# Install once with:  bash scripts/install-hooks.sh
# (`neurostrata-mcp hooks install` writes these exact bytes into the
# directory git actually runs hooks from, core.hooksPath honored — one
# hook text for both install paths, so they cannot drift.)

command -v neurostrata-mcp >/dev/null 2>&1 || { echo "error: neurostrata-mcp not found — gate NOT run for this push" >&2; exit 1; }
if [ "$NEUROSTRATA_SKIP_GATE" = "1" ]; then
    echo "NeuroStrata task gate: skipped (NEUROSTRATA_SKIP_GATE=1) — the gate did NOT run for this push" >&2
    exit 0
fi

# The namespace configured for this project, resolved in one place
# (`neurostrata-mcp namespace resolve`); a failed lookup blocks the push.
# The folder name, the namespace this gate checked before, is gated too.
NAMESPACE=$(neurostrata-mcp namespace resolve) || { echo "error: neurostrata-mcp namespace resolve failed — gate NOT run for this push (an older neurostrata-mcp without the namespace resolve command is a likely cause: upgrade it)" >&2; exit 1; }
FOLDER_NAMESPACE=$(basename "$(git rev-parse --show-toplevel)")
GATE_NAMESPACES=("$NAMESPACE")
# Only a namespace-shaped basename is safe to pass to `task gate`: an
# option-like or invalid name would change how the CLI parses the call.
if [[ "$FOLDER_NAMESPACE" =~ ^[A-Za-z0-9][A-Za-z0-9_.:-]*$ ]] && [ "$FOLDER_NAMESPACE" != "$NAMESPACE" ]; then
    GATE_NAMESPACES+=("$FOLDER_NAMESPACE")
fi

# ── NeuroStrata gate (required) ────────────────────────────────────────────
# --strict maps an unavailable database to a blocking exit too, so a 1
# below is always a verdict: violations, or a database that could not be
# checked under --strict. The hook blocks on the verdict code only.
for GATE_NAMESPACE in "${GATE_NAMESPACES[@]}"; do
    neurostrata-mcp task gate "$GATE_NAMESPACE" --strict
    code=$?
    if [ $code -eq 1 ]; then
        echo "Push blocked: resolve the tasks above" >&2
        exit 1
    fi
    if [ $code -ne 0 ]; then
        echo "error: neurostrata-mcp task gate exited $code — the gate could NOT run for this push" >&2
        exit 1
    fi
done

# ── project guards (add yours here) ────────────────────────────────────────
# Add the path of each local guard; it runs before every push on every host.
# A guard that belongs here does NOT need a CI workflow to be real.
PROJECT_GATES=(
    # Task-coverage gate: blocks pushes when changed docs contain work-item
    # language without a NeuroStrata task ID citation.
    ./scripts/task-coverage-gate.sh
    # ./scripts/rules-gate.sh
)

if [ ${#PROJECT_GATES[@]} -gt 0 ]; then
    for g in "${PROJECT_GATES[@]}"; do
        [ -f "$g" ] || { echo "task-coverage-gate: guard $g missing — skipping" >&2; continue; }
        "$g" || { echo "Push blocked: $g failed" >&2; exit 1; }
    done
fi

exit 0   # the gate passed and all project guards passed
# neurostrata-hook-sha256: 5a3b7ea2ec3f8b14761ee1a4efe8dc985a16706e8a18c820064fb5c52ac49e9c
"#;

/// SHA-256 of the scaffolded pre-push hook body (excluding the trailing
/// hash-marker line). Used to detect stale installed hooks on upgrade.
pub(crate) fn hook_sha256() -> String {
    use sha2::{Digest, Sha256};
    let marker = "# neurostrata-hook-sha256:";
    let body = match SCAFFOLDED_PRE_PUSH_HOOK.rfind(marker) {
        Some(idx) => &SCAFFOLDED_PRE_PUSH_HOOK[..idx],
        None => SCAFFOLDED_PRE_PUSH_HOOK,
    };
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hex::encode(hasher.finalize())
}

/// True when `content` was written by NeuroStrata (contains the gate or pre-commit marker).
pub(crate) fn is_ours_hook(content: &str) -> bool {
    content.contains("NeuroStrata task gate") || content.contains("NeuroStrata pre-commit hook")
}

/// True when `content` is a NeuroStrata hook whose embedded hash matches the
/// current scaffold (i.e. not stale).
pub(crate) fn is_current_hook(content: &str) -> bool {
    if !is_ours_hook(content) {
        return false;
    }
    let marker = format!("# neurostrata-hook-sha256: {}", hook_sha256());
    content.contains(&marker)
}

/// The scaffolded `pre-commit` hook: records the module-query ledger and
/// stamps `graph_queried` on the active task before any commit lands.
pub(crate) const SCAFFOLDED_PRE_COMMIT_HOOK: &str = r#"#!/bin/bash
# NeuroStrata pre-commit hook — scaffolded by neurostrata-bootstrap.
#
# Warns when a staged file has no recorded NeuroStrata graph/memory lookup and
# keeps the local `.NeuroStrata/session_queries.json` ledger readable. Receipts
# are written post-dispatch by the graph/ontology tools; this hook never
# fabricates lookup rows. Works across any repository layout.
#
# Install with:  neurostrata-mcp hooks install

ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
LEDGER="$ROOT/.NeuroStrata/session_queries.json"
# The namespace configured for this project (harness settings; folder name
# last), resolved in one place: `neurostrata-mcp namespace resolve`. The
# ledger's own field and the folder name are fallbacks when the binary is
# not on PATH or the lookup fails.
NAMESPACE=""
RESOLVED=""
if command -v neurostrata-mcp >/dev/null 2>&1; then
    if NAMESPACE=$(neurostrata-mcp namespace resolve 2>/dev/null) && [ -n "$NAMESPACE" ]; then
        RESOLVED=1
    else
        NAMESPACE=""
    fi
fi
if [ -z "$NAMESPACE" ] && [ -f "$LEDGER" ]; then
    NAMESPACE=$(grep -o '"namespace"[[:space:]]*:[[:space:]]*"[^"]*"' "$LEDGER" | head -n1 | sed 's/.*"namespace"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
fi
if [ -z "$NAMESPACE" ]; then
    NAMESPACE=$(basename "$ROOT")
fi

# Windows roots (`C:/...`) compare case-insensitively, like the file system.
# Only ASCII letters are folded, the same way in bash, node and python, so a
# file name spelled identically on both sides always matches.
FOLD=0
case "$ROOT" in
    [A-Za-z]:*) FOLD=1 ;;
esac
fold_case() {
    if [ "$FOLD" = "1" ]; then
        printf '%s' "$1" | LC_ALL=C tr '[:upper:]' '[:lower:]'
    else
        printf '%s' "$1"
    fi
}

# ── Collect staged file paths (excluding .NeuroStrata/ internal files) ───────
StagedFiles=()
while IFS= read -r -d '' file; do
    case "$file" in
        .NeuroStrata/*|"") ;;
        *) StagedFiles+=("$file") ;;
    esac
done < <(git diff --name-only -z --cached 2>/dev/null)
if [ "${#StagedFiles[@]}" -eq 0 ]; then
    while IFS= read -r -d '' file; do
        case "$file" in
            .NeuroStrata/*|"") ;;
            *) StagedFiles+=("$file") ;;
        esac
    done < <(git diff --name-only -z 2>/dev/null)
fi

# ── Project quality gate (optional; only when the project ships a runner) ────
# When scripts/check-staged-quality.sh exists it runs the project's staged
# formatting/lint checks. Any nonzero exit aborts the hook before the ledger
# is written, so a failed quality gate never records a lookup.
if [ -f "$ROOT/scripts/check-staged-quality.sh" ]; then
    bash "$ROOT/scripts/check-staged-quality.sh" || exit $?
fi

# ── Path normalization for ledger matching ─────────────────────────────────
# $file from git diff is repo-relative (e.g. `pkg/auth/handler.go`). A ledger
# entry may carry `path` (already project-relative, written by newer daemons;
# preferred), or only the raw `query`: `"$ROOT/$file"` as an absolute path,
# with `/` or `\`, `\\?\` or Git Bash `/c/` prefixes and any
# drive-letter case on Windows, `"./$file"`, or the bare `"$file"`. All of them
# are reduced to the repo-relative form below.
normalize_rel_path() {
    local p="$1"
    p="${p#"$ROOT"/}"
    while [ "${p#./}" != "$p" ]; do
        p="${p#./}"
    done
    p="${p%/}"
    printf '%s' "$p"
}

# Dump decoded, normalized ledger paths (one per line) using node or python3.
# Empty if ledger missing or neither runtime is available.
dump_ledger_paths() {
    if [ ! -f "$LEDGER" ]; then
        return 0
    fi
    export LEDGER ROOT FOLD
    if command -v node >/dev/null 2>&1; then
        node -e '
          const fs = require("fs");
          const ledger = process.env.LEDGER;
          const win = process.env.FOLD === "1";
          const fold = (s) => (win ? s.replace(/[A-Z]/g, (c) => c.toLowerCase()) : s);
          // C:\x | C:/x | \\?\C:\x | /c/x (Windows only) -> C:/x ; no trailing /
          const normAbs = (p) => {
            p = p.replace(/\\/g, "/").replace(/^\/\/[?.]\//, "");
            const m = win ? /^\/([A-Za-z])(\/.*)?$/.exec(p) : null;
            if (m) p = m[1] + ":" + (m[2] || "/");
            return p.length > 1 ? p.replace(/\/+$/, "") : p;
          };
          const roots = [process.env.ROOT].filter(Boolean).map(normAbs);
          let parsed;
          try { parsed = JSON.parse(fs.readFileSync(ledger, "utf8")); } catch { process.exit(0); }
          const queries = Array.isArray(parsed) ? parsed : (parsed && Array.isArray(parsed.queries) ? parsed.queries : []);
          const out = new Set();
          const norm = (raw) => {
            if (typeof raw !== "string" || !raw) return;
            let p = normAbs(raw);
            for (const r of roots) {
              if (fold(p) === fold(r)) return;
              if (fold(p).startsWith(fold(r) + "/")) { p = p.slice(r.length + 1); break; }
            }
            if (/^[A-Za-z]:\//.test(p) || p.startsWith("/")) return; // another project
            while (p.startsWith("./")) p = p.slice(2);
            if (p) out.add(fold(p));
          };
          for (const q of queries) {
            if (!q || typeof q !== "object") continue;
            if (typeof q.path === "string" && q.path) norm(q.path); else norm(q.query);
            for (const k of ["domains", "governed_prefixes"]) {
              if (Array.isArray(q[k])) q[k].forEach(norm);
            }
          }
          for (const p of out) process.stdout.write(p + "\n");
        ' 2>/dev/null || true
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, os, re, sys
# Bash compares these lines byte for byte (grep -x): write UTF-8 and plain
# newlines even where a Windows Python defaults to the ANSI code page and CRLF.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
ledger = os.environ["LEDGER"]
win = os.environ.get("FOLD") == "1"
def fold(s):
    return re.sub(r"[A-Z]", lambda m: m.group(0).lower(), s) if win else s
def norm_abs(p):
    p = re.sub(r"^//[?.]/", "", p.replace("\\", "/"))
    m = re.match(r"^/([A-Za-z])(/.*)?$", p) if win else None
    if m:
        p = m.group(1) + ":" + (m.group(2) or "/")
    return p.rstrip("/") if len(p) > 1 else p
roots = [norm_abs(r) for r in (os.environ.get("ROOT"),) if r]
try:
    with open(ledger, encoding="utf-8") as f:
        parsed = json.load(f)
except Exception:
    sys.exit(0)
queries = parsed if isinstance(parsed, list) else (parsed.get("queries") if isinstance(parsed, dict) else [])
if not isinstance(queries, list):
    queries = []
out = set()
def norm(raw):
    if not isinstance(raw, str) or not raw:
        return
    p = norm_abs(raw)
    for r in roots:
        if fold(p) == fold(r):
            return
        if fold(p).startswith(fold(r) + "/"):
            p = p[len(r) + 1:]
            break
    if re.match(r"^[A-Za-z]:/", p) or p.startswith("/"):
        return
    while p.startswith("./"):
        p = p[2:]
    if p:
        out.add(fold(p))
for q in queries:
    if not isinstance(q, dict):
        continue
    if isinstance(q.get("path"), str) and q.get("path"):
        norm(q.get("path"))
    else:
        norm(q.get("query"))
    for k in ("domains", "governed_prefixes"):
        v = q.get(k)
        if isinstance(v, list):
            for item in v:
                norm(item)
for p in out:
    sys.stdout.write(p + "\n")
' 2>/dev/null || true
    fi
}

LEDGER_PATHS=$(dump_ledger_paths)

ledger_has_file() {
    local rel="$1"
    if [ -n "$LEDGER_PATHS" ]; then
        printf '%s\n' "$LEDGER_PATHS" | grep -qxF "$(fold_case "$rel")"
        return $?
    fi
    [ -f "$LEDGER" ] || return 1
    grep -qF "\"$rel\"" "$LEDGER" 2>/dev/null \
        || grep -qF "\"./$rel\"" "$LEDGER" 2>/dev/null \
        || grep -qF "\"$ROOT/$rel\"" "$LEDGER" 2>/dev/null
}

ledger_has_dir() {
    local rel_dir="${1%/}"
    if [ -n "$LEDGER_PATHS" ]; then
        printf '%s\n' "$LEDGER_PATHS" | grep -qxF "$(fold_case "$rel_dir")"
        return $?
    fi
    [ -f "$LEDGER" ] || return 1
    grep -qF "\"$rel_dir/\"" "$LEDGER" 2>/dev/null \
        || grep -qF "\"./$rel_dir/\"" "$LEDGER" 2>/dev/null \
        || grep -qF "\"$ROOT/$rel_dir/\"" "$LEDGER" 2>/dev/null \
        || grep -qF "\"$rel_dir\"" "$LEDGER" 2>/dev/null
}

# ── Check each staged file / ancestor dir-prefix against session_queries.json ──
for raw_file in "${StagedFiles[@]}"; do
    file=$(normalize_rel_path "$raw_file")
    [ -z "$file" ] && continue
    matched=0
    if [ -f "$LEDGER" ]; then
        if ledger_has_file "$file"; then
            matched=1
        else
            # Walk ancestor directories of $file; only match if the exact
            # ancestor path was recorded in $LEDGER (not widened to top-level).
            anc=$(dirname "$file")
            while [ "$anc" != "." ] && [ "$anc" != "/" ] && [ -n "$anc" ]; do
                if ledger_has_dir "$anc"; then
                    matched=1
                    break
                fi
                anc=$(dirname "$anc")
            done
        fi
    fi
    if [ "$matched" -eq 0 ]; then
        echo "warning: file '$file' has no recorded NeuroStrata graph/memory lookup in this session" >&2
        echo "  → call: neurostrata_memories_for_file --namespace $NAMESPACE --path \"$file\"" >&2
    fi
done

# ── Write the query ledger (non-blocking, always writes) ─────────────────────
mkdir -p "$(dirname "$LEDGER")"
if [ ! -f "$LEDGER" ]; then
    printf '{\n  "namespace": "%s",\n  "queries": [\n  ]\n}\n' "$NAMESPACE" > "$LEDGER"
elif [ -n "$RESOLVED" ]; then
    # The ledger's namespace follows the resolved one, so lookups recorded from
    # now on are filed under the namespace the hooks and the gate use. The
    # rewrite goes through a temporary file and a rename; an unparseable
    # ledger or a missing node/python3 leaves the file as it is.
    export LEDGER NAMESPACE
    if command -v node >/dev/null 2>&1; then
        node -e '
          const fs = require("fs");
          const ledger = process.env.LEDGER;
          const ns = process.env.NAMESPACE;
          let parsed;
          try { parsed = JSON.parse(fs.readFileSync(ledger, "utf8")); } catch { process.exit(0); }
          if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) process.exit(0);
          if (parsed.namespace === ns) process.exit(0);
          parsed.namespace = ns;
          const tmp = ledger + ".tmp-" + process.pid;
          fs.writeFileSync(tmp, JSON.stringify(parsed, null, 2) + "\n");
          fs.renameSync(tmp, ledger);
        ' 2>/dev/null || true
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, os, sys
ledger = os.environ["LEDGER"]
ns = os.environ["NAMESPACE"]
try:
    with open(ledger, encoding="utf-8") as f:
        parsed = json.load(f)
except Exception:
    sys.exit(0)
if not isinstance(parsed, dict) or parsed.get("namespace") == ns:
    sys.exit(0)
parsed["namespace"] = ns
tmp = ledger + ".tmp-" + str(os.getpid())
with open(tmp, "w", encoding="utf-8", newline="\n") as f:
    json.dump(parsed, f, indent=2, ensure_ascii=False)
    f.write("\n")
os.replace(tmp, ledger)
' 2>/dev/null || true
    fi
fi


# Spec §6 R6.1: this hook never fabricates `tool:"pre-commit"` ledger rows.
# A stale/unrecorded staged file only produces the honest warning above. Real
# receipts are written post-dispatch by the graph/ontology tools themselves.

exit 0
"#;

/// The scaffolded `scripts/install-hooks.sh`: tracks the hook in the repo
/// (so it is reviewable) and installs the executable copy git actually runs.
const SCAFFOLDED_INSTALL_HOOKS: &str = r#"#!/usr/bin/env bash
# Install scripts/hooks/pre-push as the hook git runs. Safe to re-run.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
src="$root/scripts/hooks/pre-push"

[ -f "$src" ] || { echo "missing $src — restore it before installing" >&2; exit 1; }

# Non-default hooks dir? Ask git; the config wins over .git/hooks.
hooks_path="$(git config --get core.hooksPath || true)"
dest_dir="${hooks_path:-$root/.git/hooks}"
mkdir -p "$dest_dir"

install -m 0755 "$src" "$dest_dir/pre-push"
echo "Installed $dest_dir/pre-push"

pre_commit_src="$root/scripts/hooks/pre-commit"
if [ -f "$pre_commit_src" ]; then
    install -m 0755 "$pre_commit_src" "$dest_dir/pre-commit"
    echo "Installed $dest_dir/pre-commit"
fi

# Read-back only: report presence, never execute. Running it here would run
# every project guard in PROJECT_GATES as a side effect of installing.
if [ -x "$dest_dir/pre-push" ]; then
    echo "Read-back: $dest_dir/pre-push present"
else
    echo "Read-back: $dest_dir/pre-push MISSING or not executable" >&2
    exit 1
fi
echo "Verify: neurostrata_task_setup should report verified.hook_installed: true"
"#;

/// Single source for the task-coverage guard: the scaffolded file and the
/// repository script are the same bytes, so the hook's live PROJECT_GATES
/// entry can never point at a guard the installer forgot to create.
pub(crate) const SCAFFOLDED_TASK_COVERAGE_GATE: &str =
    include_str!("../../scripts/task-coverage-gate.sh");

/// The directory git actually runs hooks from, resolved the way git resolves
/// it: `core.hooksPath` at the highest-priority config level that sets it,
/// else `$GIT_DIR/hooks`.
///
/// Same source of truth as the scaffolded installer (`git config --get
/// core.hooksPath`, SCAFFOLDED_INSTALL_HOOKS). Reading `.git/config` by hand
/// instead made the two disagree: a `core.hooksPath` in the global config made
/// the installer write to the global path while this reported unhooked
/// (FINDING-bootstrap-wiring-nudge fix round 1).
pub(crate) fn git_hooks_dir(root: &std::path::Path) -> std::path::PathBuf {
    let configured = std::process::Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty());
    match configured {
        // A relative hooksPath is relative to the repo's top-level working
        // tree, which is `root`; git runs the hook from there.
        Some(v) => {
            let p = std::path::Path::new(&v);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                root.join(p)
            }
        }
        None => root.join(".git").join("hooks"),
    }
}

/// Best-effort hook hygiene for a project root, run by the daemon. Returns
/// status lines describing what was done (or a warning), or an empty string
/// when nothing needed doing (or the root isn't a git repo). Never errors.
/// The daemon's own `NEUROSTRATA_NAMESPACE` is not read: one daemon serves
/// every project.
pub(crate) fn hook_hygiene(root: &std::path::Path) -> String {
    hook_hygiene_reading_env(root, false, None)
}

/// Hook hygiene for a caller that already resolved the project namespace
/// (for example `get_snapshot`). The supplied namespace is an explicit answer
/// for configs that name none; it never overrides a config's own namespace.
pub(crate) fn hook_hygiene_with_namespace(
    root: &std::path::Path,
    supplied_namespace: Option<&str>,
) -> String {
    hook_hygiene_reading_env(root, false, supplied_namespace)
}

/// [`hook_hygiene`] for a CLI run in the project, where
/// `NEUROSTRATA_NAMESPACE` is the user's, as the git hooks see it.
pub(crate) fn hook_hygiene_from_cli(root: &std::path::Path) -> String {
    hook_hygiene_reading_env(root, true, None)
}

fn hook_hygiene_reading_env(
    root: &std::path::Path,
    read_env: bool,
    supplied_namespace: Option<&str>,
) -> String {
    if !root.join(".git").exists() {
        return String::new();
    }
    // Harness configs get a namespace only from an explicit source (a harness
    // config, a caller that already resolved it, or the environment on the
    // CLI): a ledger or folder-name answer written there would outrank every
    // explicit source from then on. Each config keeps its own; with no
    // explicit answer only plugin files are refreshed.
    let (explicit, warning) = match supplied_namespace
        .and_then(harness_plugins::usable_namespace)
        .map(|ns| (Some(ns), None))
    {
        Some(answer) => answer,
        None => match harness_plugins::namespace_for_refresh(root, read_env) {
            Ok(ns) => (ns, None),
            Err(e) => (
                None,
                Some(format!("HOOKS: harness configs left as they are: {e}")),
            ),
        },
    };
    let _ = harness_plugins::auto_install_detected_plugins(
        root,
        harness_plugins::ConfigNamespace::Keep(explicit.as_deref()),
        None,
    );
    let status = hook_hygiene_files(root);
    match warning {
        Some(w) if status.is_empty() => w,
        Some(w) => format!("{w}\n{status}"),
        None => status,
    }
}

/// The git hook and tracked hook-script part of [`hook_hygiene`].
fn hook_hygiene_files(root: &std::path::Path) -> String {
    let hooks_dir = git_hooks_dir(root);
    if !hooks_dir.is_dir() {
        if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
            tracing::warn!("failed to create hooks dir {}: {e}", hooks_dir.display());
        }
    }
    // Auto-install / refresh pre-commit hook if missing or stale.
    let pre_commit_target = hooks_dir.join("pre-commit");
    if !pre_commit_target.exists() {
        if std::fs::write(&pre_commit_target, SCAFFOLDED_PRE_COMMIT_HOOK).is_ok() {
            set_executable(&pre_commit_target);
        }
    } else if let Ok(pc_content) = std::fs::read_to_string(&pre_commit_target) {
        if pc_content.contains("NeuroStrata pre-commit hook")
            && pc_content != SCAFFOLDED_PRE_COMMIT_HOOK
            && std::fs::copy(&pre_commit_target, hooks_dir.join("pre-commit.bak")).is_ok()
            && std::fs::write(&pre_commit_target, SCAFFOLDED_PRE_COMMIT_HOOK).is_ok()
        {
            set_executable(&pre_commit_target);
        }
    }
    // Tracked hook artifacts live in scripts/. Create the tree explicitly so
    // a fresh project without scripts/ gets the whole set a live hook needs.
    // Seed only when absent: an older binary must never clobber a newer
    // checked-in hook or local edits. Git-hook auto-heal above is untouched.
    let scripts_dir = root.join("scripts");
    if let Err(e) = std::fs::create_dir_all(scripts_dir.join("hooks")) {
        tracing::warn!("failed to create scripts/hooks dir: {e}");
    }
    for (rel, content) in [
        ("hooks/pre-push", SCAFFOLDED_PRE_PUSH_HOOK),
        ("hooks/pre-commit", SCAFFOLDED_PRE_COMMIT_HOOK),
        ("install-hooks.sh", SCAFFOLDED_INSTALL_HOOKS),
        ("task-coverage-gate.sh", SCAFFOLDED_TASK_COVERAGE_GATE),
    ] {
        let path = scripts_dir.join(rel);
        if !path.exists() && std::fs::write(&path, content).is_ok() {
            set_executable(&path);
        }
    }
    let target = hooks_dir.join("pre-push");
    let version = env!("CARGO_PKG_VERSION");
    if !target.exists() {
        if std::fs::write(&target, SCAFFOLDED_PRE_PUSH_HOOK).is_ok() {
            set_executable(&target);
            return format!("HOOKS: auto-installed pre-push (v{})", version);
        }
        return String::new();
    }
    let content = match std::fs::read_to_string(&target) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    if is_ours_hook(&content) {
        if is_current_hook(&content) {
            return String::new();
        }
        // stale: back up + rewrite. A failed backup must not destroy the
        // hook content -- rewriting without the .bak loses the only copy.
        if let Err(e) = std::fs::copy(&target, hooks_dir.join("pre-push.bak")) {
            tracing::warn!("failed to back up foreign pre-push hook: {e}");
            return String::new();
        }
        if std::fs::write(&target, SCAFFOLDED_PRE_PUSH_HOOK).is_ok() {
            set_executable(&target);
            return format!("HOOKS: updated pre-push (stale -> v{})", version);
        }
        return String::new();
    }
    format!(
        "HOOKS WARNING: foreign pre-push hook at {}; inspect or run 'neurostrata-mcp hooks install --force'",
        target.display()
    )
}

#[cfg(unix)]
fn set_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)) {
        tracing::warn!("failed to set executable bit on {}: {e}", path.display());
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &std::path::Path) {}

/// Whether a NeuroStrata pre-push hook is observable at the path git runs.
///
/// Exactly one directory is checked — the resolved hooks dir. When
/// `core.hooksPath` is set git ignores `$GIT_DIR/hooks` entirely, so a stale
/// gate left there is not a gate: falling through to it reported `true` for a
/// project whose hooks moved, i.e. a gate that silently stopped running on
/// every push (FINDING-bootstrap-wiring-nudge fix round 1).
fn hook_observable(root: &std::path::Path) -> bool {
    let hook = git_hooks_dir(root).join("pre-push");
    let content = if hook.exists() {
        match std::fs::read_to_string(&hook) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::debug!("hook_observable read failed for {}: {e}", hook.display());
                None
            }
        }
    } else {
        None
    };
    content
        .filter(|s| s.contains("NeuroStrata task gate"))
        .is_some()
        && hook_executable(&hook)
}

/// A hook git will not run is not a gate: git executing a pre-push hook
/// without the executable bit fails, so "installed" must require it too.
#[cfg(unix)]
fn hook_executable(hook: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(hook)
        .map(|m| (m.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn hook_executable(_hook: &std::path::Path) -> bool {
    true
}

/// The project guards an installed hook actually runs.
///
/// `PROJECT_GATES` ships with every entry commented, so a hook file existing
/// proves nothing about project-pipeline coverage — claiming `client_side:
/// true` for every pipeline guard on the strength of a hook file alone is
/// exactly the unverifiable claim this panel exists to eliminate. Count the
/// live entries instead.
fn hook_project_guards(hook: &std::path::Path) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(hook) else {
        return Vec::new();
    };
    let mut guards = Vec::new();
    let mut in_array = false;
    for line in body.lines() {
        let line = line.trim();
        if !in_array {
            if line.starts_with("PROJECT_GATES=(") {
                in_array = true;
            }
            continue;
        }
        if line.starts_with(')') {
            break;
        }
        // A commented entry is an example, not a wired guard.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        guards.push(line.trim_matches('"').trim().to_string());
    }
    guards
}

/// Read-only verification plus the wiring block, shared by bootstrap and
/// task_setup. **Git-first, never git-only**: git is named the default
/// instance, and a non-software project (CMS, media production, research) is
/// told in the same breath to identify its own chokepoint and record it as a
/// rule memory. `gate_point` is free text on purpose — we never enumerate
/// CMSes or render farms.
fn wiring_panel(root: &std::path::Path, namespace: &str) -> (Value, Value, Value) {
    let git = root.join(".git").exists();
    let hook_installed = git && hook_observable(root);
    let hook_guards: Vec<String> = if hook_installed {
        hook_project_guards(&git_hooks_dir(root).join("pre-push"))
    } else {
        Vec::new()
    };
    let export_fresh: Option<bool> = {
        let export = root.join(".NeuroStrata").join("graph").join("graph.json");
        let modified = match std::fs::metadata(&export) {
            Ok(meta) => match meta.modified() {
                Ok(mtime) => Some(mtime),
                Err(e) => {
                    tracing::warn!("failed to read export file mtime: {e}");
                    None
                }
            },
            // Not exported yet: metadata fails, no mtime to read.
            Err(_) => None,
        };
        match (git, modified) {
            (false, _) => None,
            (true, None) => Some(false),
            (true, Some(mtime)) => {
                let age = std::time::SystemTime::now()
                    .duration_since(mtime)
                    .unwrap_or_default();
                Some(age <= std::time::Duration::from_secs(7 * 24 * 60 * 60))
            }
        }
    };

    // Q1 gate-point resolution: git resolves the default instance; no git
    // leaves it null and the project's agent must name the chokepoint.
    let gate_point: Option<String> = if git {
        Some(if hook_installed {
            "git-pre-push (NeuroStrata task gate installed)".to_string()
        } else {
            "git-pre-push (via neurostrata-mcp hooks install)".to_string()
        })
    } else {
        None
    };

    let mut automatic = Vec::new();
    let mut uncovered: Vec<&str> = Vec::new();
    let mut pipeline_ids: Vec<&str> = Vec::new();
    for w in wiring::AUTOMATIC_WIRES {
        if w.runs_in == wiring::RunsIn::GitPrePush && !git {
            continue; // the default instance only exists where git exists
        }
        let resolved = match w.runs_in {
            wiring::RunsIn::Core => Some("neurostrata core (tool, error path, lock)".to_string()),
            wiring::RunsIn::GitPrePush => gate_point.clone(),
            wiring::RunsIn::ProjectPipeline => {
                pipeline_ids.push(w.id);
                if git {
                    Some("project pipeline (wire each gate into the project's CI job)".to_string())
                } else {
                    None
                }
            }
        };
        // Note: "no machine exists" is intentionally absent from RunsIn — a
        // wire category the catalog never ships cannot claim coverage.
        if w.runs_in == wiring::RunsIn::ProjectPipeline && resolved.is_none() {
            uncovered.push(w.id);
        }
        let entry = json!({
            "id": w.id,
            "runs_in": w.runs_in.as_str(),
            "blocks": w.blocks,
            "gate_point": resolved,
            "reason": w.reason,
            // Field contract of docs/design-wiring-panel.md Q1: the schema key
            // exists even when no reference implementation is shipped, so
            // consumers read one stable shape.
            "reference": None::<String>,
        });
        // Q5 honesty, the hard version: no per-guard `client_side` flag. Whether the
        // hook runs a given pipeline guard is read out of the hook body
        // (`verified.hook_project_guards`) — a flag derived from "a hook file
        // exists" asserts coverage nobody observed, and PROJECT_GATES ships
        // empty, so the flag would be true exactly when it is least true.
        automatic.push(entry);
    }

    // The closed `fires_on` enum guards what this panel may surface as a
    // reminder: a wire whose trigger is outside `VALID_FIRES_ON` is outside
    // the contract (the catalog test asserts the same invariant), so it is
    // filtered here rather than advertised as a reminder.
    let reminders: Vec<Value> = wiring::REMINDER_WIRES
        .iter()
        .filter(|r| wiring::is_valid_fires_on(r.fires_on))
        .map(|r| json!({ "id": r.id, "fires_on": r.fires_on, "text": r.text }))
        .collect();

    // A `git-pre-push` claim with no observable hook is a lie, so it stays
    // uncovered and the install instruction survives the subtraction.
    if git && !hook_installed {
        uncovered.push("task-gate");
    }

    let core_ids: Vec<&str> = wiring::AUTOMATIC_WIRES
        .iter()
        .filter(|w| w.runs_in == wiring::RunsIn::Core)
        .map(|w| w.id)
        .collect();
    let git_ids: Vec<&str> = wiring::AUTOMATIC_WIRES
        .iter()
        .filter(|w| w.runs_in == wiring::RunsIn::GitPrePush)
        .map(|w| w.id)
        .collect();
    let coverage = json!({
        "core": core_ids,
        "git-pre-push": if hook_installed { git_ids } else { Vec::<&str>::new() },
        "project-pipeline": pipeline_ids.clone(),
        "uncovered": uncovered,
    });
    let wiring_block = json!({
        "automatic": automatic,
        "reminders": reminders,
        "coverage": coverage,
    });

    // Q5: verification is read-only and re-runnable; instructions subtract
    // anything verified true (the guinea pig's existing_hooks:[] beside an
    // install instruction becomes structurally impossible).
    let verified = json!({
        "hook_installed": hook_installed,
        // Healthy means the gate actually runs on every push; a claim above
        // the absence of the hook is the same lie the panel exists to kill.
        "status_healthy": hook_installed,
        "export_fresh": export_fresh,
        "gate_point_resolved": gate_point.is_some(),
        // Read out of the installed hook's PROJECT_GATES, not assumed from
        // the hook's existence: the scaffold ships every entry commented, so
        // "a hook exists" and "the guard runs on every push" are different
        // claims and only the second one is worth making.
        "hook_project_guards": hook_guards,
    });

    // Q2: one instruction template, git first and dominant, non-git in the
    // same sentence shape with a discovery clause that ends by writing the
    // chosen gate point into memory.
    let mut gate_ids = pipeline_ids.clone();
    if git {
        gate_ids.insert(0, "task-gate");
    }
    let resolve_clause = if git {
        // FINDING-bootstrap-wiring-nudge: this used to end "then wire the
        // project-pipeline gates above into your CI" — which made CI the
        // default and silently locked non-GitHub projects out. The hook is
        // the broader chokepoint (every host, every push, works on a bare
        // repo); CI is the optional second layer on top of it.
        if hook_installed {
            "Git detected -- still the default and recommended instance, because a git hook is the one chokepoint that works on EVERY git project -- and the NeuroStrata pre-push hook is already installed at the path git runs -- so the task gate runs on every push; nothing to install, and the scaffolded files are omitted from this instruction rather than written over a working hook. What is left is the project-pipeline gates above: `verified.hook_project_guards` lists the ones the installed hook already runs, and the rest are not yet enforced on a bare `git push`. Extend the installed `pre-push` hook's `PROJECT_GATES` with them -- a guard in the hook runs on every host and needs no CI. CI is optional and vendor-specific: if this project already has GitHub Actions, GitLab CI or similar, re-run the project-pipeline gates there as a defense-in-depth second check; a git project with no CI needs nothing beyond the hook."
        } else {
            "Git detected -- the default and recommended instance, because a git hook is the one chokepoint that works on EVERY git project: run `neurostrata-mcp hooks install` to put the task gate on pre-push (the gate does not move until that hook is installed and runs `task gate --strict`). The `files` field of this instruction carries a runnable `scripts/hooks/pre-push`, `scripts/hooks/pre-commit`, `scripts/install-hooks.sh` and `scripts/task-coverage-gate.sh` (the guard the hook's PROJECT_GATES already names); write them, install the hook, then extend that hook with the project-pipeline gates above -- they belong in the hook, not in a workflow. CI is optional and vendor-specific: if this project already has GitHub Actions, GitLab CI or similar, re-run the project-pipeline gates there as a defense-in-depth second check; a git project with no CI needs nothing beyond the hook. Verify with the `verified` block in this payload."
        }
    } else {
        "No git detected. Identify the ONE step every unit of work must pass to leave this project -- a CMS publish action, a render-farm submission, a publish script, a review-approval step. Wire `neurostrata-mcp task gate NAMESPACE --strict` (exit 1 = blocked) and the project-pipeline gates above into that step so nothing ships past an unresolved gate. Record the chosen step: neurostrata_add_memory with memory_type: rule, content naming the gate point -- the next session must not have to re-derive it."
    };
    let point_display = gate_point
        .clone()
        .unwrap_or_else(|| "this project's chokepoint (to be named)".to_string());
    let wire_instruction = json!({
        "kind": "action",
        "action": "wire_gates",
        "gate_point": gate_point,
        "gate_ids": gate_ids,
        "text": format!(
            "Gate these checks at {}: {}. {}",
            point_display,
            gate_ids.join(", "),
            resolve_clause.replace("NAMESPACE", namespace)
        ),
        // Scaffolded for git projects that have no hook yet: every artifact a
        // live PROJECT_GATES entry needs, plus both hook copies and their
        // installer. A hooked project gets nothing — writing these files would
        // overwrite a working gate and tell it to install a second one.
        "files": if git && !hook_installed {
            json!([
                { "path": "scripts/hooks/pre-push",
                  "content": SCAFFOLDED_PRE_PUSH_HOOK,
                  "executable": true,
                  "note": "extend PROJECT_GATES with the project's own guards" },
                { "path": "scripts/hooks/pre-commit",
                  "content": SCAFFOLDED_PRE_COMMIT_HOOK,
                  "executable": true,
                  "note": "records the graph-query ledger before each commit" },
                { "path": "scripts/install-hooks.sh",
                  "content": SCAFFOLDED_INSTALL_HOOKS,
                  "executable": true,
                  "note": "run: bash scripts/install-hooks.sh" },
                { "path": "scripts/task-coverage-gate.sh",
                  "content": SCAFFOLDED_TASK_COVERAGE_GATE,
                  "executable": true,
                  "note": "project gate: blocks docs with untracked work items" },
            ])
        } else {
            json!([])
        },
    });

    (wiring_block, verified, wire_instruction)
}

// ── tool: neurostrata_task_setup ───────────────────────────────────────────

/// What an existing project already has, found by reading the tree only.
#[derive(Debug, Default)]
struct ProjectScan {
    git_remote: Option<String>,
    languages: Vec<String>,
    /// Source-file counts per language, strongest first (guinea-pig BUG-4:
    /// the presence of one manifest is not dominance -- 72 tooling JS files
    /// must not outvote 185 Go files).
    language_counts: Vec<(String, usize)>,
    ci: Vec<String>,
    agents_md: bool,
    beads_dir: bool,
    beads_path: Option<String>,
    existing_hooks: Vec<String>,
    legacy_hook: bool,
}

fn detect_project(root: &std::path::Path) -> ProjectScan {
    let mut scan = ProjectScan::default();

    // The git config, reached directly (normal checkout) or through .git's
    // gitdir pointer (worktree), where remotes live in the main config.
    let config = std::fs::read_to_string(root.join(".git").join("config"))
        .ok()
        .or_else(|| {
            let gitfile = std::fs::read_to_string(root.join(".git")).ok()?;
            let gitdir_line = gitfile.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
            let gitdir = std::path::PathBuf::from(gitdir_line.trim());
            let gitdir = if gitdir.is_absolute() {
                gitdir
            } else {
                root.join(gitdir)
            };
            // <main>/.git/worktrees/<name> -> <main>/.git/config
            let base = gitdir.parent()?.parent()?;
            std::fs::read_to_string(base.join("config")).ok()
        });
    if let Some(config) = config {
        let mut current_remote: Option<String> = None;
        let mut remotes: Vec<(String, String)> = Vec::new();
        for line in config.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                current_remote = line
                    .strip_prefix("[remote \"")
                    .and_then(|r| r.strip_suffix("\"]"))
                    .map(String::from);
            } else if let Some(url) = line.strip_prefix("url = ") {
                if let Some(name) = &current_remote {
                    remotes.push((name.clone(), url.trim().to_string()));
                }
            }
        }
        scan.git_remote = remotes
            .iter()
            .find(|(name, _)| name == "origin")
            .or_else(|| remotes.first())
            .map(|(_, url)| url.clone());
    }

    if root.join("Cargo.toml").exists() {
        scan.languages.push("rust".to_string());
    }
    if root.join("package.json").exists() {
        scan.languages.push("javascript".to_string());
    }
    if root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists()
    {
        scan.languages.push("python".to_string());
    }
    if root.join("go.mod").exists() {
        scan.languages.push("go".to_string());
    }
    if root.join("pom.xml").exists()
        || root.join("build.gradle").exists()
        || root.join("build.gradle.kts").exists()
    {
        scan.languages.push("java".to_string());
    }

    // Weigh the tree, not the manifests (guinea-pig BUG-4). The `ignore`
    // walker respects .gitignore, so vendored and generated trees stay out.
    let mut counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .max_depth(Some(6))
        .build()
        .flatten()
    {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let lang = match entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
        {
            "rs" => "rust",
            "go" => "go",
            "py" => "python",
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => "javascript",
            "java" | "kt" | "kts" => "java",
            _ => continue,
        };
        *counts.entry(lang).or_insert(0) += 1;
    }
    for (lang, n) in &counts {
        if !scan.languages.iter().any(|l| l == lang) {
            scan.languages.push(lang.to_string());
        }
        scan.language_counts.push((lang.to_string(), *n));
    }
    scan.language_counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    // Manifest-detected languages with no counted files sort last, keeping the
    // order meaningful for everything suggested_rules derives from it.
    scan.languages.sort_by_key(|l| {
        std::cmp::Reverse(
            scan.language_counts
                .iter()
                .find(|(name, _)| name == l)
                .map(|(_, n)| *n)
                .unwrap_or(0),
        )
    });

    if root.join(".github/workflows").is_dir() {
        scan.ci.push("github-actions".to_string());
    }
    if root.join(".gitlab-ci.yml").exists() {
        scan.ci.push("gitlab-ci".to_string());
    }
    if root.join("Jenkinsfile").exists() {
        scan.ci.push("jenkins".to_string());
    }
    if root.join(".circleci/config.yml").exists() {
        scan.ci.push("circleci".to_string());
    }

    scan.agents_md = root.join("AGENTS.md").exists();

    let beads = root.join(".beads");
    if beads.is_dir() {
        scan.beads_dir = true;
        for candidate in ["issues.jsonl", "beads.jsonl", "issues.json"] {
            if beads.join(candidate).exists() {
                scan.beads_path = Some(format!(".beads/{}", candidate));
                break;
            }
        }
    }

    let hooks_dir = git_hooks_dir(root);
    if let Ok(entries) = std::fs::read_dir(&hooks_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".sample") {
                continue;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if name == "pre-push" {
                let ours = std::fs::read_to_string(entry.path())
                    .map(|c| c.contains("NeuroStrata task gate"))
                    .unwrap_or(false);
                if !ours {
                    scan.legacy_hook = true;
                }
            }
            scan.existing_hooks.push(name);
        }
    }
    scan.existing_hooks.sort();
    scan
}

/// At most three rules, each with why the scan suggests it (section 3.2).
fn suggested_rules(scan: &ProjectScan) -> Vec<Value> {
    let mut rules = Vec::new();
    let total: usize = scan.language_counts.iter().map(|(_, n)| n).sum();
    // A language earns a suggestion by dominance, not by leaving one manifest
    // behind: at least a fifth of the counted source files (guinea-pig BUG-4).
    for (lang, count) in &scan.language_counts {
        if rules.len() == 3 {
            break;
        }
        if total > 0 && count * 5 < total {
            continue;
        }
        let rule = match lang.as_str() {
            "rust" => (
                "Build with `cargo build`; `cargo test` and `cargo clippy` must pass before pushing.",
                "detected cargo workspace",
            ),
            "javascript" => (
                "JavaScript/TypeScript project: install from the lockfile and keep the package scripts (test, lint) green before pushing.",
                "detected package.json",
            ),
            "python" => (
                "Python project: run the configured test suite (pytest) before pushing.",
                "detected python packaging files",
            ),
            "go" => (
                "Go project: `go build ./...` and `go test ./...` must pass before pushing.",
                "detected go.mod",
            ),
            "java" => (
                "JVM project: run the Maven/Gradle check task before pushing.",
                "detected a JVM build file",
            ),
            _ => continue,
        };
        rules.push(json!({
            "content": rule.0,
            "memory_type": "rule",
            "rationale": format!("{} ({} source files counted)", rule.1, count),
            // Generated content is never ground truth: the caller must check
            // these against the project's standing rules before accepting.
            "heuristic": true,
            "verified": false,
        }));
    }
    // Nothing counted (empty or exotic tree): fall back to manifest presence,
    // still marked heuristic.
    if rules.is_empty() {
        for lang in &scan.languages {
            let rule = match lang.as_str() {
                "rust" => (
                    "Build with `cargo build`; `cargo test` and `cargo clippy` must pass before pushing.",
                    "detected cargo workspace",
                ),
                "javascript" => (
                    "JavaScript/TypeScript project: install from the lockfile and keep the package scripts (test, lint) green before pushing.",
                    "detected package.json",
                ),
                "python" => (
                    "Python project: run the configured test suite (pytest) before pushing.",
                    "detected python packaging files",
                ),
                "go" => (
                    "Go project: `go build ./...` and `go test ./...` must pass before pushing.",
                    "detected go.mod",
                ),
                "java" => (
                    "JVM project: run the Maven/Gradle check task before pushing.",
                    "detected a JVM build file",
                ),
                _ => continue,
            };
            rules.push(json!({
                "content": rule.0,
                "memory_type": "rule",
                "rationale": rule.1,
                "heuristic": true,
                "verified": false,
            }));
            if rules.len() == 3 {
                break;
            }
        }
    }
    if rules.len() < 3 && !scan.ci.is_empty() {
        rules.push(json!({
            "content": format!(
                "CI runs on {}; reproduce its checks locally before pushing.",
                scan.ci.join(", ")
            ),
            "memory_type": "rule",
            "heuristic": true,
            "verified": false,
            "rationale": format!("detected CI: {}", scan.ci.join(", ")),
        }));
    }
    rules
}

async fn new_setup_task(
    store: &Arc<dyn VectorStore>,
    emb: &Arc<dyn Embedder>,
    namespace: &str,
    title: &str,
    description: Option<&str>,
    priority: i64,
) -> Result<(String, MemoryPayload), String> {
    let mut task_meta = Map::new();
    task_meta.insert("priority".to_string(), json!(priority));
    task_meta.insert("task_type".to_string(), json!("task"));
    if let Some(d) = description {
        task_meta.insert("description".to_string(), json!(d));
    }
    store_new_task(
        store,
        emb,
        namespace,
        title,
        "unknown",
        None,
        task_meta,
        Map::new(),
    )
    .await
}

pub async fn handle_task_setup(
    args: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    let namespace = match required_str(&args, "namespace") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let project_root = match required_str(&args, "project_root") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    if let Err(e) = check_namespace(&namespace) {
        return e;
    }
    let root = std::path::Path::new(project_root);
    if !root.is_dir() {
        return format!("ERROR: project_root '{}' is not a directory.", project_root);
    }
    let harness_plugins = harness_plugins::auto_install_detected_plugins(
        root,
        harness_plugins::ConfigNamespace::Set(&namespace),
        arg_str(&args, "harness"),
    );

    let scan = detect_project(root);
    let (wiring_block, verified, wire_instruction) = wiring_panel(root, &namespace);
    let hook_installed = verified["hook_installed"].as_bool().unwrap_or(false);
    let mut conflicts: Vec<Value> = Vec::new();
    let mut tasks_created: Vec<Value> = Vec::new();
    let mut instructions: Vec<Value> = Vec::new();
    let mut step = 1;

    // One of the two hooks install runs, always first (section 8): the gate
    // is what makes the system live on the first push. Subtracted when the
    // hook is verified present (A10: setup verifies, it does not re-instruct).
    let hook_cmd = if scan.legacy_hook {
        conflicts.push(json!({
            "kind": "legacy_hook",
            "path": ".git/hooks/pre-push",
            "resolution": "neurostrata-mcp hooks install --force replaces it",
        }));
        "neurostrata-mcp hooks install --force"
    } else {
        "neurostrata-mcp hooks install"
    };
    if scan.legacy_hook || !hook_installed {
        instructions
            .push(json!({ "step": step, "kind": "action", "action": "run", "cmd": hook_cmd }));
        step += 1;
    }

    if scan.legacy_hook {
        let title = "Replace the legacy pre-push hook with the NeuroStrata task gate";
        match new_setup_task(
            &store,
            &emb,
            &namespace,
            title,
            Some(
                "The installed hook still runs the old database-mtime heuristic. The task gate replaces it: extraction is checked per task, by graph edge.",
            ),
            1,
        )
        .await
        {
            Ok((id, _)) => {
                tasks_created.push(json!({
                    "id": id,
                    "title": title,
                    "metadata_hint": "run: neurostrata-mcp hooks install --force",
                }));
            }
            Err(e) => return e,
        }
    }

    if let Some(beads_rel) = &scan.beads_path {
        let count = std::fs::read_to_string(root.join(beads_rel))
            .map(|text| text.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "failed to read beads file {}: {e}",
                    root.join(beads_rel).display()
                );
                0
            });
        let title = format!("Migrate {} beads issues into NeuroStrata tasks", count);
        let import_cmd = format!(
            "neurostrata-mcp task import {} --from-beads {}",
            namespace, beads_rel
        );
        match new_setup_task(
            &store,
            &emb,
            &namespace,
            &title,
            Some("One-shot import, idempotent on bead_id: re-running it skips what is already imported."),
            1,
        )
        .await
        {
            Ok((id, _)) => {
                tasks_created.push(json!({
                    "id": id,
                    "title": title,
                    "metadata_hint": format!("run: {}", import_cmd),
                }));
                instructions.push(json!({ "step": step, "kind": "action", "action": "run", "cmd": import_cmd }));
                step += 1;
            }
            Err(e) => return e,
        }
    }

    let rules = suggested_rules(&scan);
    // Generated rules are a draft, never ground truth (guinea-pig BUG-4):
    // each suggestion carries its nearest existing memories so a potential
    // contradiction is visible BEFORE anything is accepted, and real overlap
    // lands in conflicts[] rather than in a flat suggestion list.
    let mut enriched_rules: Vec<Value> = Vec::new();
    for rule in &rules {
        let mut r = rule.clone();
        let content = rule
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let mut similar: Vec<Value> = Vec::new();
        match emb.embed(&content).await {
            Ok(vec) => match store.search(&namespace, vec, 3, false, false).await {
                Ok(hits) => {
                    for hit in hits {
                        let preview: String = hit.payload.content.chars().take(160).collect();
                        similar.push(json!({
                            "id": hit.id,
                            "memory_type": hit.payload.memory_type,
                            "content": preview,
                        }));
                        if hit.payload.memory_type == "rule" {
                            conflicts.push(json!({
                            "kind": "suggested_rule_overlap",
                            "suggestion": content,
                            "existing_id": hit.id,
                            "existing": preview,
                            "resolution": "review both; supersede the stale one instead of accepting a contradiction",
                        }));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("rule similarity search failed for '{namespace}': {e}");
                }
            },
            Err(e) => {
                tracing::warn!("rule similarity embed failed for '{namespace}': {e}");
            }
        }
        if let Some(obj) = r.as_object_mut() {
            obj.insert("similar_existing".to_string(), json!(similar));
        }
        enriched_rules.push(r);
    }
    let rules = enriched_rules;
    for rule in &rules {
        instructions.push(json!({
            "step": step,
            "kind": "action",
            "action": "call_tool",
            "tool": "neurostrata_add_memory",
            "review_first": true,
            "params": {
                "content": rule.get("content").cloned().unwrap_or(Value::Null),
                "memory_type": rule.get("memory_type").cloned().unwrap_or(Value::Null),
                "namespace": namespace.as_str(),
            },
        }));
        step += 1;
    }

    // The wiring panel's own step (Q2): gate the project-pipeline checks at
    // the project's chokepoint -- git's pre-push by default, whatever the
    // project's ship step is otherwise. Always one instruction, one shape.
    let mut wire_step = wire_instruction;
    wire_step["step"] = json!(step);
    instructions.push(wire_step);
    step += 1;

    // A non-git project has no default gate point: exactly one task, so the
    // decision lands in a Goal and its resolution lands in memory.
    if !verified["gate_point_resolved"].as_bool().unwrap_or(false) {
        let title = "Identify and wire the project gate point, record it as a rule memory";
        match new_setup_task(
            &store,
            &emb,
            &namespace,
            title,
            Some(
                "No git detected. Find the ONE step every unit of work must pass to leave this project (CMS publish, render submission, publish script, review approval), wire `neurostrata-mcp task gate <ns> --strict` into it, and record the choice with neurostrata_add_memory (memory_type: rule).",
            ),
            1,
        )
        .await
        {
            Ok((id, _)) => {
                tasks_created.push(json!({
                    "id": id,
                    "title": title,
                    "metadata_hint": "run: the wire_gates instruction in this payload",
                }));
            }
            Err(e) => return e,
        }
    }

    // Completing the setup tasks forces the first extractions (section 8).
    for created in &tasks_created {
        if let Some(id) = created.get("id").and_then(|v| v.as_str()) {
            instructions.push(json!({
                "step": step,
                "action": "call_tool",
                "tool": "neurostrata_task_complete",
                "params": { "id": id, "namespace": namespace.as_str() },
            }));
            step += 1;
        }
    }

    let rid = crate::registry::registry_row_id(&namespace);
    let mut prev_wires: Vec<String> = Vec::new();
    let mut prev_fires: Vec<String> = Vec::new();
    if let Ok(Some((reg_vec, mut reg_payload))) = store.get(&namespace, &rid).await {
        if let Some(ds) = reg_payload.metadata.get("doctor_state") {
            if let Some(arr) = ds.get("configured_wires").and_then(|v| v.as_array()) {
                prev_wires = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect();
            }
            if let Some(arr) = ds.get("configured_fires_on").and_then(|v| v.as_array()) {
                prev_fires = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect();
            }
        }
        if let Some(obj) = reg_payload.metadata.as_object_mut() {
            let mut ds = obj
                .get("doctor_state")
                .cloned()
                .unwrap_or_else(|| json!({}));
            ds["configured_wires"] = json!(crate::task::wiring::known_wire_ids());
            ds["configured_fires_on"] = json!(crate::task::wiring::VALID_FIRES_ON);
            obj.insert("doctor_state".to_string(), ds);
        }
        let _ = store.upsert(&namespace, &rid, reg_vec, reg_payload).await;
    }
    let (new_auto, new_rem, new_fires) =
        crate::task::wiring::compute_lifecycle_delta(&prev_wires, &prev_fires);
    let lifecycle_delta = json!({
        "new_automatic_wires": new_auto.iter().map(|w| w.id).collect::<Vec<_>>(),
        "new_reminder_wires": new_rem.iter().map(|r| r.id).collect::<Vec<_>>(),
        "new_fires_on_triggers": new_fires,
    });

    encode(&json!({
        "namespace": namespace,
        "detected": {
            "git_remote": scan.git_remote,
            "languages": scan.languages,
            "language_counts": scan.language_counts,
            "ci": scan.ci,
            "agents_md": scan.agents_md,
            "beads_dir": scan.beads_dir,
            "beads_path": scan.beads_path,
            "existing_hooks": scan.existing_hooks,
        },
        "suggested_rules": rules,
        "suggested_rules_are_heuristic": true,
        "conflicts": conflicts,
        "tasks_created": tasks_created,
        "instructions": instructions,
        "wiring": wiring_block,
        "verified": verified,
        "lifecycle_delta": lifecycle_delta,
        "harness_plugins": harness_plugins,
    }))
}

// ── beads migration (section 7): CLI-only, idempotent on bead_id ───────────

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportSummary {
    pub created: usize,
    pub skipped_existing: usize,
    pub non_issue_lines: usize,
    pub malformed_lines: usize,
}

pub async fn import_beads(
    store: Arc<dyn VectorStore>,
    emb: Arc<dyn Embedder>,
    namespace: &str,
    from_beads: &str,
) -> Result<ImportSummary, String> {
    let raw = std::fs::read_to_string(from_beads)
        .map_err(|e| format!("ERROR: could not read {}: {}", from_beads, e))?;

    let mut summary = ImportSummary::default();

    let rows = store
        .list(namespace, None, None)
        .await
        .map_err(|e| format!("ERROR: could not list '{}': {}", namespace, e))?;
    let mut seen_bead_ids: HashSet<String> = HashSet::new();
    let mut bead_to_task: HashMap<String, String> = HashMap::new();
    for r in &rows {
        if let Some(bead_id) = r
            .payload
            .metadata
            .get("task")
            .and_then(|t| t.get("bead_id"))
            .and_then(|v| v.as_str())
        {
            seen_bead_ids.insert(bead_id.to_string());
            // Already-imported beads map to their real NeuroStrata task id, so
            // downstream beads can still translate their deps (the idempotent
            // skip must not orphan these mappings).
            bead_to_task.insert(bead_id.to_string(), r.id.clone());
        }
    }
    let mut existing_task_ids: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();

    struct Pending {
        bead_id: String,
        title: String,
        description: Option<String>,
        issue_type: String,
        priority: i64,
        labels: Vec<String>,
        deps: Vec<String>,
        closed: bool,
        close_reason: Option<String>,
        created_at: Option<String>,
        updated_at: Option<String>,
        created_by: Option<String>,
    }

    // Parse everything first: dependencies become blocked_by in the target
    // system's ids, so the bead_id -> task id map must exist before any write.
    let mut pending: Vec<Pending> = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                summary.malformed_lines += 1;
                continue;
            }
        };
        if value
            .get("_type")
            .and_then(|t| t.as_str())
            .map(|t| t != "issue")
            .unwrap_or(false)
        {
            summary.non_issue_lines += 1;
            continue;
        }
        let bead_id = match value.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                summary.malformed_lines += 1;
                continue;
            }
        };
        let title = value
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&bead_id)
            .to_string();
        let description = value
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let issue_type = value
            .get("issue_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("task")
            .to_string();
        let priority = value
            .get("priority")
            .and_then(|v| v.as_i64())
            .filter(|p| (0..=4).contains(p))
            .unwrap_or(2);
        let labels: Vec<String> = value
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let deps: Vec<String> = value
            .get("dependencies")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| d.get("depends_on_id").and_then(|v| v.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        pending.push(Pending {
            bead_id,
            title,
            description,
            issue_type,
            priority,
            labels,
            deps,
            closed: value.get("status").and_then(|v| v.as_str()) == Some("closed"),
            close_reason: value
                .get("close_reason")
                .and_then(|v| v.as_str())
                .map(String::from),
            created_at: value
                .get("created_at")
                .and_then(|v| v.as_str())
                .map(String::from),
            updated_at: value
                .get("updated_at")
                .and_then(|v| v.as_str())
                .map(String::from),
            created_by: value
                .get("created_by")
                .and_then(|v| v.as_str())
                .map(String::from),
        });
    }

    // First pass: allocate ids for what is new, skip what bead_id already
    // covers (the idempotency key from section 7).
    let key = ns_key(namespace);
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut allocations: Vec<Option<String>> = Vec::with_capacity(pending.len());
    for p in &pending {
        if seen_bead_ids.contains(&p.bead_id) {
            if let Some(existing_task_id) = bead_to_task.get(&p.bead_id) {
                id_map.insert(p.bead_id.clone(), existing_task_id.clone());
            }
            allocations.push(None);
            summary.skipped_existing += 1;
            continue;
        }
        let id = allocate_task_id(&key, &existing_task_ids).ok_or_else(|| {
            format!(
                "ERROR: could not allocate a task id for '{}' in '{}'.",
                p.bead_id, namespace
            )
        })?;
        existing_task_ids.insert(id.clone());
        id_map.insert(p.bead_id.clone(), id.clone());
        allocations.push(Some(id));
    }

    store
        .init(namespace)
        .await
        .map_err(|e| format!("ERROR: could not initialize '{}': {}", namespace, e))?;

    for (p, allocation) in pending.iter().zip(allocations.iter()) {
        let id = match allocation {
            Some(id) => id,
            None => continue,
        };

        let mut task_meta = Map::new();
        task_meta.insert(
            "status".to_string(),
            json!(if p.closed { "done" } else { "open" }),
        );
        task_meta.insert("priority".to_string(), json!(p.priority));
        task_meta.insert("task_type".to_string(), json!(p.issue_type));
        task_meta.insert("bead_id".to_string(), json!(p.bead_id));
        if !p.labels.is_empty() {
            task_meta.insert("labels".to_string(), json!(p.labels));
        }
        // Deps point at beads ids; translate them into this system's task ids
        // so readiness joins work, keeping the raw id only when nothing maps.
        let blocked: Vec<String> = p
            .deps
            .iter()
            .map(|d| id_map.get(d).cloned().unwrap_or_else(|| d.clone()))
            .collect();
        if !blocked.is_empty() {
            task_meta.insert("blocked_by".to_string(), json!(blocked));
        }
        if let Some(d) = &p.description {
            task_meta.insert("description".to_string(), json!(d));
        }
        // Timestamps are preserved verbatim (section 7).
        if let Some(t) = &p.created_at {
            task_meta.insert("created_at".to_string(), json!(t));
        }
        if let Some(t) = &p.updated_at {
            task_meta.insert("updated_at".to_string(), json!(t));
        }
        if p.closed {
            // Closed beads history is exempt from the extraction check
            // (section 7): demanding retroactive extraction from 40 historical
            // issues is friction, not enforcement.
            task_meta.insert("grandfathered".to_string(), json!(true));
            let reason = p
                .close_reason
                .clone()
                .unwrap_or_else(|| "closed in beads".to_string());
            task_meta.insert("close_reason".to_string(), json!(reason));
        }

        write_task_row(
            &store,
            &emb,
            namespace,
            id,
            &p.title,
            p.created_by.as_deref().unwrap_or("unknown"),
            Some("beads-import"),
            task_meta,
            Map::new(),
        )
        .await?;
        seen_bead_ids.insert(p.bead_id.clone());
        summary.created += 1;
    }

    Ok(summary)
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RecursionToken, ThalamicBus, ThalamicPulse};
    use crate::store::LadybugStore;

    /// R5: an "installed" hook must carry the executable bit, so fixtures
    /// asserting `hook_installed: true` install it the way the installer does.
    fn write_exec_hook(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }
    }

    // hook hash marker tests (Task 1)
    #[test]
    fn hook_sha256_is_64_hex() {
        let h = hook_sha256();
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn scaffold_is_current_hook() {
        assert!(is_current_hook(SCAFFOLDED_PRE_PUSH_HOOK));
    }

    #[test]
    fn foreign_hook_not_ours() {
        assert!(!is_ours_hook("#!/bin/bash\necho hi\n"));
        assert!(!is_current_hook("#!/bin/bash\necho hi\n"));
    }

    /// Install-hook audit: every live PROJECT_GATES entry in the scaffolded
    /// pre-push hook must have a matching bootstrap file, so a fresh project
    /// never silently skips a guard the hook claims to run.
    #[test]
    fn bootstrap_scaffolds_every_prepush_project_gate() {
        let root = std::env::temp_dir().join(format!("ns-scaffold-gates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();

        let (_, _, wire_instruction) = wiring_panel(&root, "MyProj");
        let files = wire_instruction["files"]
            .as_array()
            .expect("fresh git project gets scaffolded files");
        let paths: Vec<&str> = files
            .iter()
            .map(|f| f["path"].as_str().expect("file path"))
            .collect();
        for expected in [
            "scripts/hooks/pre-push",
            "scripts/hooks/pre-commit",
            "scripts/install-hooks.sh",
            "scripts/task-coverage-gate.sh",
        ] {
            assert!(
                paths.contains(&expected),
                "bootstrap must scaffold {expected}; got {paths:?}"
            );
        }
        assert!(
            files
                .iter()
                .all(|f| f["executable"].as_bool().unwrap_or(false)),
            "every scaffolded hook artifact must be executable: {files:?}"
        );

        // Comments and blank lines are not gates; only live array entries are.
        let open = "PROJECT_GATES=(";
        let start = SCAFFOLDED_PRE_PUSH_HOOK
            .find(open)
            .expect("PROJECT_GATES block in the scaffolded hook")
            + open.len();
        let block = &SCAFFOLDED_PRE_PUSH_HOOK[start..];
        let end = block.find(')').expect("closing paren of PROJECT_GATES");
        let live_gates: Vec<&str> = block[..end]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        assert!(
            !live_gates.is_empty(),
            "the hook must declare at least one live project gate"
        );
        for gate in live_gates {
            let normalized = gate.trim_start_matches("./");
            assert!(
                paths.contains(&normalized),
                "live gate {gate} has no scaffolded file; paths={paths:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Install-hook audit: hook_hygiene seeds every tracked artifact on a
    /// fresh project and keeps an existing one byte-identical on re-run.
    #[cfg(unix)]
    #[test]
    fn hook_hygiene_seeds_all_tracked_hook_artifacts_on_fresh_project() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ns-hygiene-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();

        let _ = hook_hygiene(&root);
        for (rel, content) in [
            ("scripts/hooks/pre-push", SCAFFOLDED_PRE_PUSH_HOOK),
            ("scripts/hooks/pre-commit", SCAFFOLDED_PRE_COMMIT_HOOK),
            ("scripts/install-hooks.sh", SCAFFOLDED_INSTALL_HOOKS),
            (
                "scripts/task-coverage-gate.sh",
                SCAFFOLDED_TASK_COVERAGE_GATE,
            ),
        ] {
            let path = root.join(rel);
            assert!(path.is_file(), "missing scaffolded artifact {rel}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                content,
                "{rel} content drifted"
            );
            assert!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o111 != 0,
                "{rel} must be executable"
            );
        }

        // Seed-only: a pre-existing tracked artifact (even foreign) is kept.
        let sentinel = root.join("scripts/task-coverage-gate.sh");
        std::fs::write(&sentinel, "#!/bin/bash\n# local sentinel\nexit 0\n").unwrap();
        let _ = hook_hygiene(&root);
        assert!(
            std::fs::read_to_string(&sentinel)
                .unwrap()
                .contains("local sentinel"),
            "hook_hygiene must not overwrite an existing tracked artifact"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A caller that knows the namespace (get_snapshot) can wire a detected
    /// harness that has no config of its own, without reading daemon env.
    #[test]
    fn supplied_namespace_wires_a_detected_harness() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();

        let _ = hook_hygiene_with_namespace(&root, Some("from-caller"));

        let cfg = std::fs::read_to_string(root.join(".claude/settings.local.json"))
            .expect("Claude hooks must be wired with the supplied namespace");
        assert!(cfg.contains("from-caller"), "{cfg}");
    }

    /// The scaffolded guard must be the canonical script, not an empty stub.
    #[test]
    fn scaffolded_task_coverage_gate_is_valid_bash() {
        if which_bash().is_none() {
            return;
        }
        assert_eq!(
            SCAFFOLDED_TASK_COVERAGE_GATE,
            include_str!("../../scripts/task-coverage-gate.sh"),
            "the guard must be single-sourced from the canonical script"
        );
        assert!(
            SCAFFOLDED_TASK_COVERAGE_GATE.contains("untracked work items"),
            "the guard must carry the task-ID gate marker"
        );
        let path = std::env::temp_dir().join(format!("ns-gate-scaffold-{}.sh", std::process::id()));
        std::fs::write(&path, SCAFFOLDED_TASK_COVERAGE_GATE).unwrap();
        let syntax = std::process::Command::new("bash")
            .arg("-n")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            syntax.status.success(),
            "scaffolded guard must be valid bash: {}",
            String::from_utf8_lossy(&syntax.stderr)
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── the bus, as the completion path sees it ────────────────────────────────

    /// Records every pulse the dispatcher hands it, so a test can assert on
    /// what the emit site published rather than on what it returned.
    struct PulseRecorder {
        seen: Arc<std::sync::Mutex<Vec<ThalamicPulse>>>,
    }

    #[async_trait::async_trait]
    impl crate::events::MemorySubscriber for PulseRecorder {
        fn name(&self) -> &'static str {
            "pulse_recorder"
        }

        async fn handle(
            &self,
            event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &crate::events::SubscriberContext<'_>,
        ) -> Result<(), crate::events::SubscriberError> {
            self.seen.lock().expect("recorder lock").push(event.clone());
            Ok(())
        }
    }

    /// A bus for a test that only needs the emit site to be callable -- the paths
    /// that are refused before any write, where nothing should be published and
    /// nobody is asserting on it.
    fn quiet_bus() -> Arc<ThalamicBus> {
        recording_bus().0
    }

    /// A bus with one recorder on it, plus the recorder's log. No store is
    /// attached: the recorder never reads `ctx.store`.
    fn recording_bus() -> (Arc<ThalamicBus>, Arc<std::sync::Mutex<Vec<ThalamicPulse>>>) {
        let bus = Arc::new(ThalamicBus::new(64));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        bus.register(Box::new(PulseRecorder { seen: seen.clone() }));
        (bus, seen)
    }

    /// Wait for the dispatcher to deliver `at_least` pulses, then hand back
    /// everything it recorded.
    async fn pulses_seen(
        seen: &Arc<std::sync::Mutex<Vec<ThalamicPulse>>>,
        at_least: usize,
    ) -> Vec<ThalamicPulse> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if seen.lock().expect("recorder lock").len() >= at_least {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the dispatcher delivered the pulses");
        seen.lock().expect("recorder lock").clone()
    }

    /// Deterministic, 4-dimensional (the width temp stores are opened with).
    struct StubEmbedder;

    #[async_trait::async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 4];
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += b as f32;
            }
            Ok(v)
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    /// Any call to embed() is a bug in a path that promised not to re-embed.
    struct FailEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FailEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("FailEmbedder must not be called on this path")
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    /// A fresh store with the global schema in place but no rows: listing on
    /// a schema-less store is an error, not an empty result.
    async fn temp_store() -> Arc<dyn VectorStore> {
        let dir = std::env::temp_dir().join(format!("ns-task-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> = Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&dir, 4).expect("open temp database"),
        );
        store.init("global").await.expect("create the schema");
        store
    }

    fn emb() -> Arc<dyn Embedder> {
        Arc::new(StubEmbedder)
    }

    /// Finding 3: with no caller root, a receipt must land under the namespace
    /// registry's `project_root` (which may sit outside the daemon CWD), not in
    /// the daemon's own ledger. Validation reads that same root.
    #[tokio::test]
    async fn ledger_root_registry_fallback_writes_receipt_under_registry_root() {
        let store = temp_store().await;
        let ns = "RegRootNs";
        store.init(ns).await.expect("init namespace schema");
        let worktree =
            std::env::temp_dir().join(format!("ns-ledger-registry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(worktree.join(".git")).unwrap();
        crate::registry::cache_project_root(&store, Some(&emb()), ns, worktree.to_str().unwrap())
            .await;
        assert_eq!(
            crate::registry::cached_project_root(&store, ns)
                .await
                .unwrap()
                .as_deref(),
            Some(worktree.to_str().unwrap()),
            "registry must carry the temp worktree root"
        );

        let fields = tool_receipts::ReceiptFields::new(
            "evt-registry".to_string(),
            0,
            "sess-registry".to_string(),
            String::new(),
            ns.to_string(),
            "neurostrata_related".to_string(),
            "related".to_string(),
            String::new(),
            "graph_read".to_string(),
            true,
            tool_receipts::ReceiptStatus::Success,
            0,
            1,
            vec!["mem-1".to_string()],
            Vec::new(),
            Vec::new(),
            "q",
            "r",
        );
        record_tool_receipt(&store, fields, None).await;

        let ledger = worktree.join(".NeuroStrata").join("session_queries.json");
        assert!(
            ledger.exists(),
            "receipt must be written under the registry project_root, not daemon CWD"
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
        assert_eq!(doc["events"][0]["event_id"], "evt-registry");
        assert_eq!(
            receipt_ledger_root(None, &store, ns).await,
            Some(worktree),
            "validation reads the same root the recorder wrote"
        );
    }

    /// Task 13 P1-8: a caller root hint without a worktree marker is untrusted;
    /// resolution falls through to the registry root instead of using the hint.
    #[tokio::test]
    async fn ledger_root_ignores_caller_hint_without_worktree_marker() {
        let store = temp_store().await;
        let ns = "HintRootNs";
        store.init(ns).await.expect("init namespace schema");
        let scratch =
            std::env::temp_dir().join(format!("ns-scratch-hint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&scratch).unwrap();
        assert!(!scratch.join(".git").exists() && !scratch.join(".NeuroStrata").exists());
        let worktree =
            std::env::temp_dir().join(format!("ns-real-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(worktree.join(".git")).unwrap();
        crate::registry::cache_project_root(&store, Some(&emb()), ns, worktree.to_str().unwrap())
            .await;
        assert_eq!(
            receipt_ledger_root(Some(scratch.to_str().unwrap()), &store, ns).await,
            Some(worktree),
            "unmarked scratch hint must fall through to the registry root"
        );
    }

    /// Git's own bash: on Windows `bash` on PATH may be WSL's launcher, which
    /// cannot run a hook against a Windows checkout.
    fn hook_bash() -> Option<std::path::PathBuf> {
        if cfg!(windows) {
            let out = std::process::Command::new("git")
                .arg("--exec-path")
                .output()
                .ok()?;
            let exec = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            // <git>/mingw64/libexec/git-core -> <git>
            let git_root = exec.parent()?.parent()?.parent()?.to_path_buf();
            ["usr/bin/bash.exe", "bin/bash.exe"]
                .iter()
                .map(|rel| git_root.join(rel))
                .find(|p| p.is_file())
        } else {
            Some(std::path::PathBuf::from("bash"))
        }
    }

    fn git_stdout_in(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Runs the scaffolded pre-commit hook in `dir`; returns its stderr.
    fn run_pre_commit(
        bash: &std::path::Path,
        hook: &std::path::Path,
        dir: &std::path::Path,
    ) -> String {
        let mut cmd = std::process::Command::new(bash);
        cmd.arg(hook).current_dir(dir);
        // Git for Windows puts its usr/bin (grep, sed, tr, ...) on PATH when it
        // runs a hook; a bare bash.exe launch does not, so add it the same way.
        if cfg!(windows) {
            if let Some(bin) = bash.parent() {
                let mut paths = vec![bin.to_path_buf()];
                paths.extend(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                ));
                cmd.env("PATH", std::env::join_paths(paths).expect("join PATH"));
            }
        }
        let out = cmd.output().expect("the hook runs");
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    /// The pre-commit hook used to strip only `"$ROOT/"` with forward slashes,
    /// so on Windows every lookup the hooks recorded (`C:\...\x.rs`) read as
    /// "no recorded lookup". Every spelling of a recorded file now matches.
    #[test]
    fn the_pre_commit_hook_matches_every_spelling_of_a_recorded_path() {
        let Some(bash) = hook_bash() else {
            eprintln!("skipped: no bash to run the hook with");
            return;
        };
        let has = |tool: &str| {
            std::process::Command::new(&bash)
                .args(["-c", &format!("command -v {tool}")])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !has("node") && !has("python3") {
            eprintln!("skipped: the hook needs node or python3 to read the ledger");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("example-project");
        let hooks_off = base.join("no-hooks");
        std::fs::create_dir_all(repo.join("src").join("a")).unwrap();
        std::fs::create_dir_all(repo.join(".NeuroStrata")).unwrap();
        std::fs::create_dir_all(&hooks_off).unwrap();
        git_stdout_in(&repo, &["init", "-q"]);
        git_stdout_in(&repo, &["config", "user.email", "t@example.invalid"]);
        git_stdout_in(&repo, &["config", "user.name", "t"]);
        git_stdout_in(
            &repo,
            &["config", "core.hooksPath", &hooks_off.to_string_lossy()],
        );
        for f in ["a1", "a2", "a3", "a4", "a5", "a6", "Ä7"] {
            std::fs::write(repo.join("src").join("a").join(format!("{f}.rs")), "x").unwrap();
        }
        std::fs::write(repo.join("none.rs"), "x").unwrap();
        git_stdout_in(&repo, &["config", "core.quotePath", "false"]);

        let root = git_stdout_in(&repo, &["rev-parse", "--show-toplevel"]);
        let back = root.replace('/', "\\");
        let mut queries = vec![
            json!({"tool": "neurostrata_memories_for_file", "query": format!(r"{back}\src\a\a1.rs")}),
            json!({"tool": "t", "query": format!("{root}/src/a/a2.rs")}),
        ];
        if cfg!(windows) {
            let msys = format!("/{}{}", root[..1].to_lowercase(), &root[2..]);
            queries.push(json!({"tool": "t", "query": format!("{msys}/src/a/a3.rs")}));
            queries.push(json!({"tool": "t", "query": format!(r"\\?\{back}\src\a\a4.rs")}));
            queries.push(
                json!({"tool": "t", "query": format!("{}/SRC/A/A5.RS", root.to_uppercase())}),
            );
        } else {
            queries.push(json!({"tool": "t", "query": "./src/a/a3.rs"}));
            queries.push(json!({"tool": "t", "query": "src/a/a4.rs/"}));
            queries.push(json!({"tool": "t", "query": "src/a/a5.rs"}));
        }
        // `path` wins over a query naming another checkout's absolute path.
        queries
            .push(json!({"tool": "t", "query": "/elsewhere/src/a/a6.rs", "path": "src/a/a6.rs"}));
        queries.push(json!({"tool": "t", "query": "/elsewhere/none.rs"}));
        // A non-ASCII name spelled the same on both sides matches: every
        // side folds only ASCII letters.
        queries.push(json!({"tool": "t", "query": format!("{root}/src/a/Ä7.rs")}));
        let ledger = repo.join(".NeuroStrata").join("session_queries.json");
        std::fs::write(
            &ledger,
            serde_json::to_string_pretty(
                &json!({"namespace": "example-project", "queries": queries}),
            )
            .unwrap(),
        )
        .unwrap();
        let hook = base.join("pre-commit");
        std::fs::write(&hook, SCAFFOLDED_PRE_COMMIT_HOOK).unwrap();

        git_stdout_in(&repo, &["add", "src", "none.rs"]);
        let stderr = run_pre_commit(&bash, &hook, &repo);
        assert!(
            stderr.contains("file 'none.rs' has no recorded"),
            "{stderr}"
        );
        for f in ["a1", "a2", "a3", "a4", "a5", "a6", "Ä7"] {
            assert!(
                !stderr.contains(&format!("src/a/{f}.rs' has no recorded")),
                "{f}: {stderr}"
            );
        }
    }

    /// The pre-commit hook is authored twice: the checked-in file the installer
    /// copies into `.git/hooks`, and the copy embedded in the binary for fresh
    /// projects. If the two drift, an installed project runs a different gate
    /// than the one reviewed in the repository — so they must stay byte-identical,
    /// and both must delegate to the project's staged-quality runner when one
    /// exists.
    #[test]
    fn pre_commit_hook_sources_stay_byte_identical() {
        let checked_in = include_str!("../../scripts/hooks/pre-commit");
        if SCAFFOLDED_PRE_COMMIT_HOOK != checked_in {
            panic!(
                "embedded pre-commit hook ({} bytes) differs from scripts/hooks/pre-commit ({} bytes)",
                SCAFFOLDED_PRE_COMMIT_HOOK.len(),
                checked_in.len()
            );
        }
        assert!(
            checked_in.contains("scripts/check-staged-quality.sh"),
            "the pre-commit hook must delegate to the project's staged-quality runner"
        );
    }

    /// The pre-commit hook files the ledger under the namespace `namespace
    /// resolve` answered: an older ledger naming another one is updated, its
    /// recorded lookups kept; a failed lookup leaves the ledger as it is.
    #[test]
    fn pre_commit_hook_updates_the_ledger_namespace_to_the_resolved_one() {
        let Some(bash) = hook_bash() else {
            eprintln!("skipped: no bash to run the hook with");
            return;
        };
        let has = |prog: &str| {
            std::process::Command::new(prog)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !has("node") && !has("python3") {
            eprintln!("skipped: neither node nor python3 to rewrite the ledger with");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = dunce::canonicalize(tmp.path())
            .unwrap()
            .join("example-project");
        std::fs::create_dir_all(repo.join(".NeuroStrata")).unwrap();
        git_stdout_in(&repo, &["init", "-q"]);
        let ledger = repo.join(".NeuroStrata").join("session_queries.json");
        let seeded = serde_json::to_string_pretty(&json!({
            "namespace": "beta",
            "queries": [{"tool": "t", "query": "src/a.rs"}]
        }))
        .unwrap();
        std::fs::write(&ledger, &seeded).unwrap();
        let hook = tmp.path().join("pre-commit");
        std::fs::write(&hook, SCAFFOLDED_PRE_COMMIT_HOOK).unwrap();
        let stub_dir = tmp.path().join("stub-bin");
        std::fs::create_dir_all(&stub_dir).unwrap();
        write_exec_hook(
            &stub_dir.join("neurostrata-mcp"),
            "#!/bin/sh\nif [ \"$1\" = namespace ]; then\n    [ -n \"$STUB_RESOLVE_EXIT\" ] && exit \"$STUB_RESOLVE_EXIT\"\n    echo alpha\n    exit 0\nfi\nexit 0\n",
        );
        let run = |resolve_exit: Option<&str>| {
            let mut dirs = vec![stub_dir.clone()];
            if let Some(bin) = bash.parent() {
                dirs.push(bin.to_path_buf());
            }
            dirs.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            let mut cmd = std::process::Command::new(&bash);
            cmd.arg(&hook)
                .current_dir(&repo)
                .env("PATH", std::env::join_paths(dirs).expect("join PATH"))
                .env_remove("STUB_RESOLVE_EXIT");
            if let Some(code) = resolve_exit {
                cmd.env("STUB_RESOLVE_EXIT", code);
            }
            let out = cmd.output().expect("the hook runs");
            assert_eq!(
                out.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let read = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap()
        };
        run(Some("1"));
        assert_eq!(
            std::fs::read_to_string(&ledger).unwrap(),
            seeded,
            "a failed lookup changes nothing"
        );
        run(None);
        let doc = read();
        assert_eq!(doc["namespace"], json!("alpha"));
        assert_eq!(doc["queries"], json!([{"tool": "t", "query": "src/a.rs"}]));
    }

    /// The pre-push hook stays fail-closed around `namespace resolve`: with
    /// no `neurostrata-mcp` on PATH, with a binary whose `namespace resolve`
    /// exits 2, and with a gate that exits 2, the push is blocked (exit 1).
    #[test]
    fn pre_push_hook_blocks_when_cli_missing_and_on_exit_2() {
        let Some(bash) = hook_bash() else {
            eprintln!("skipped: no bash to run the hook with");
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(tmp.path()).unwrap();
        git_stdout_in(&root, &["init", "-q"]);
        let hook = root.join("pre-push");
        std::fs::write(&hook, SCAFFOLDED_PRE_PUSH_HOOK).unwrap();
        let stub_dir = root.join("stub-bin");
        std::fs::create_dir_all(&stub_dir).unwrap();
        write_exec_hook(
            &stub_dir.join("neurostrata-mcp"),
            "#!/bin/sh\nif [ \"$1\" = namespace ]; then\n    [ -n \"$STUB_RESOLVE_EXIT\" ] && exit \"$STUB_RESOLVE_EXIT\"\n    echo example-project\n    exit 0\nfi\nexit ${STUB_GATE_EXIT:-0}\n",
        );
        // The inherited PATH minus any installed neurostrata-mcp, so "missing"
        // really is missing on a developer machine.
        let inherited: Vec<std::path::PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|d| {
                    [
                        "neurostrata-mcp",
                        "neurostrata-mcp.exe",
                        "neurostrata-mcp.cmd",
                    ]
                    .iter()
                    .all(|name| !d.join(name).is_file())
                })
                .collect();
        let run = |with_stub: bool, envs: &[(&str, &str)]| -> (Option<i32>, String) {
            let mut dirs = Vec::new();
            if with_stub {
                dirs.push(stub_dir.clone());
            }
            // Git for Windows' usr/bin (sh, basename) as git puts it there.
            if let Some(bin) = bash.parent() {
                dirs.push(bin.to_path_buf());
            }
            dirs.extend(inherited.iter().cloned());
            let mut cmd = std::process::Command::new(&bash);
            cmd.arg(&hook)
                .current_dir(&root)
                .env("PATH", std::env::join_paths(dirs).expect("join PATH"))
                .env_remove("NEUROSTRATA_SKIP_GATE")
                .env_remove("STUB_RESOLVE_EXIT")
                .env_remove("STUB_GATE_EXIT");
            for (key, value) in envs {
                cmd.env(key, value);
            }
            let out = cmd.output().expect("the hook runs");
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).to_string(),
            )
        };
        let (code, stderr) = run(false, &[]);
        assert_eq!(code, Some(1), "no neurostrata-mcp on PATH blocks the push");
        assert!(
            stderr.contains("error: neurostrata-mcp not found"),
            "{stderr}"
        );
        let (code, stderr) = run(true, &[("STUB_RESOLVE_EXIT", "2")]);
        assert_eq!(
            code,
            Some(1),
            "a namespace resolve that exits 2 blocks the push"
        );
        assert!(
            stderr.contains(
                "error: neurostrata-mcp namespace resolve failed — gate NOT run for this push"
            ) && stderr.contains("upgrade it"),
            "a failed resolve is announced, never silent: {stderr}"
        );
        let (code, stderr) = run(true, &[("STUB_GATE_EXIT", "2")]);
        assert_eq!(code, Some(1), "a gate that exits 2 blocks the push");
        assert!(
            stderr.contains("the gate could NOT run for this push"),
            "{stderr}"
        );
        assert_eq!(
            run(true, &[]).0,
            Some(0),
            "a passing gate lets the push through"
        );
    }

    /// `neurostrata-mcp hooks install` and `scripts/install-hooks.sh` must
    /// install the same fail-closed gate; a stale checked-in copy would let
    /// one install path bypass the task gate (audit #317).
    #[test]
    fn pre_push_hook_sources_stay_byte_identical() {
        let checked_in = include_str!("../../scripts/hooks/pre-push");
        if SCAFFOLDED_PRE_PUSH_HOOK != checked_in {
            panic!(
                "embedded pre-push hook ({} bytes) differs from scripts/hooks/pre-push ({} bytes)",
                SCAFFOLDED_PRE_PUSH_HOOK.len(),
                checked_in.len()
            );
        }
        assert!(
            !checked_in.contains("NEUROSTRATA_SKIP_GATE=1 git push"),
            "the blocked message must not advertise the escape hatch"
        );
        assert!(
            checked_in.contains("error: neurostrata-mcp not found"),
            "a missing gate binary must block the push"
        );
        assert!(
            checked_in.contains("if [ $code -ne 0 ]"),
            "a non-zero gate exit must block the push"
        );
    }

    /// A store whose `namespace` already holds one rule, so the namespace
    /// exists for task_create's existence check.
    async fn store_with_namespace(namespace: &str) -> Arc<dyn VectorStore> {
        let store = temp_store().await;
        store.init(namespace).await.expect("create the schema");
        let id = uuid::Uuid::new_v4().to_string();
        let vector = StubEmbedder.embed("always use podman").await.unwrap();
        store
            .upsert(
                namespace,
                &id,
                vector,
                MemoryPayload {
                    content: "always use podman".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("seed the rule");
        store
    }

    async fn create_task(
        store: &Arc<dyn VectorStore>,
        namespace: &str,
        title: &str,
        priority: i64,
    ) -> String {
        let args = json!({ "namespace": namespace, "title": title, "priority": priority });
        let reply = handle_task_create(args, emb(), store.clone()).await;
        let parsed: Value = serde_json::from_str(&reply)
            .unwrap_or_else(|e| panic!("create failed: {} -- {}", reply, e));
        parsed["id"].as_str().expect("id").to_string()
    }

    async fn status_of(store: &Arc<dyn VectorStore>, namespace: &str, id: &str) -> Status {
        let (_, payload) = store
            .get(namespace, id)
            .await
            .unwrap()
            .expect("task exists");
        Task {
            id: id.to_string(),
            payload,
        }
        .status()
    }

    fn reply_title(reply: &str) -> String {
        let parsed: Value = serde_json::from_str(reply)
            .unwrap_or_else(|e| panic!("json reply: {} -- {}", reply, e));
        parsed["task"]["title"].as_str().expect("title").to_string()
    }

    /// Review fix round 2: one graph-tool dispatch must not double-count the
    /// task counters. `record_tool_receipt` attributes the receipt to the active
    /// in-progress task but must leave `graph_queries_count`, `graph_queried`,
    /// `recent_queries` and legacy `queries[]` untouched — those belong solely
    /// to the handler-internal ledger recorders.
    #[tokio::test]
    async fn tool_receipt_never_double_stamps_task_counters() {
        use crate::guard::testkit::MockStore;
        use crate::task::tool_receipts::build_dispatch_receipt;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "T6Fix2";
        let task_id = "task-in-progress";
        let vector = StubEmbedder.embed("seed").await.unwrap();
        store
            .upsert(
                ns,
                task_id,
                vector,
                MemoryPayload {
                    content: "active task".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "task": {
                            "status": "in_progress",
                            "title": "active task",
                            "task_type": "task",
                            "priority": 1,
                        }
                    }),
                },
            )
            .await
            .expect("seed active task");

        let before = store
            .get(ns, task_id)
            .await
            .unwrap()
            .expect("task exists")
            .1
            .metadata
            .clone();
        assert!(before["task"].get("graph_queries_count").is_none());
        assert!(before["task"].get("graph_queried").is_none());
        assert!(before["task"].get("recent_queries").is_none());

        let response = serde_json::json!({ "memories": [ { "id": "m1" } ] }).to_string();
        let fields = build_dispatch_receipt(
            "evt-fix2".to_string(),
            1,
            "ses".to_string(),
            ns.to_string(),
            "neurostrata_locate".to_string(),
            "q",
            &response,
            false,
            4,
        );
        let tmp = tempfile::tempdir().unwrap();
        // Marker so resolve_session_root_dir keeps the ledger inside the
        // tempdir instead of walking out to the enclosing worktree root.
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();
        record_tool_receipt(&store, fields, Some(&caller_root)).await;

        // Counters and task metadata must be byte-identical after recording.
        let after = store
            .get(ns, task_id)
            .await
            .unwrap()
            .expect("task exists")
            .1
            .metadata;
        assert_eq!(
            after, before,
            "record_tool_receipt must not mutate task counters"
        );

        // Attribution still lands on the receipt, and no legacy row is written.
        let ledger_path = tmp.path().join(".NeuroStrata/session_queries.json");
        let ledger: Value =
            serde_json::from_str(&std::fs::read_to_string(&ledger_path).expect("ledger written"))
                .expect("valid ledger");
        assert_eq!(ledger["events"][0]["task_id"].as_str(), Some(task_id));
        assert_eq!(
            ledger["queries"].as_array().map(|a| a.len()),
            Some(0),
            "no legacy queries[] row from the receipt recorder"
        );
    }

    async fn seeded_task_count(store: &Arc<dyn VectorStore>, ns: &str, id: &str) -> i64 {
        store.get(ns, id).await.unwrap().unwrap().1.metadata["task"]
            .get("graph_queries_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    /// Panel C14: one locate dispatch must increment exactly one task counter,
    /// never every in-progress task and never once per returned path.
    #[tokio::test]
    async fn graph_query_counts_one_dispatch_on_one_task_only() {
        use crate::guard::testkit::MockStore;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "C14OneDispatch";
        let vector = StubEmbedder.embed("seed").await.unwrap();
        for (id, status) in [
            ("task-z", "in_progress"),
            ("task-a", "in_progress"),
            ("task-open", "open"),
        ] {
            store
                .upsert(
                    ns,
                    id,
                    vector.clone(),
                    MemoryPayload {
                        content: "task".to_string(),
                        user_id: "kenton".to_string(),
                        memory_type: "task".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({
                            "task": { "status": status, "title": id }
                        }),
                    },
                )
                .await
                .expect("seed task");
        }
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();

        record_graph_query_with_root(&store, ns, "neurostrata_locate", "q", Some(&caller_root))
            .await;

        let counts = [
            seeded_task_count(&store, ns, "task-z").await,
            seeded_task_count(&store, ns, "task-a").await,
            seeded_task_count(&store, ns, "task-open").await,
        ];
        assert_eq!(
            counts.iter().sum::<i64>(),
            1,
            "one increment total, got {:?}",
            counts
        );
        assert_eq!(
            counts.iter().filter(|&&c| c > 0).count(),
            1,
            "only the dispatching task is stamped, got {:?}",
            counts
        );
    }

    /// Panel C15: when no caller task context reaches the generic dispatch,
    /// the first-match fallback must be stable (lowest task id), not whichever
    /// row `store.list` happened to return first.
    #[tokio::test]
    async fn receipt_fallback_uses_stable_first_in_progress_task() {
        use crate::guard::testkit::MockStore;
        use crate::task::tool_receipts::build_dispatch_receipt;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "C15Attribution";
        let vector = StubEmbedder.embed("seed").await.unwrap();
        // Insert in reverse-id order: a stable fallback picks by id, not insertion order.
        for id in ["z-task", "a-task"] {
            store
                .upsert(
                    ns,
                    id,
                    vector.clone(),
                    MemoryPayload {
                        content: "task".to_string(),
                        user_id: "kenton".to_string(),
                        memory_type: "task".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({ "task": { "status": "in_progress", "title": id } }),
                    },
                )
                .await
                .expect("seed task");
        }
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();
        let response = json!({"memories": [{"id": "m1"}]}).to_string();
        let fields = build_dispatch_receipt(
            "evt-c15".to_string(),
            1,
            "ses".to_string(),
            ns.to_string(),
            "neurostrata_locate".to_string(),
            "q",
            &response,
            false,
            1,
        );
        assert!(fields.task_id.is_empty());
        record_tool_receipt(&store, fields, Some(&caller_root)).await;

        let ledger: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".NeuroStrata/session_queries.json"))
                .expect("ledger written"),
        )
        .expect("valid ledger");
        assert_eq!(
            ledger["events"][0]["task_id"].as_str(),
            Some("a-task"),
            "fallback must be deterministic (lowest task id), not first insertion"
        );
    }

    /// Seed an in-progress task carrying the optional claim context that a
    /// real `neurostrata_task_claim` writes under `metadata["task"]`.
    async fn seed_attributed_task(
        store: &Arc<dyn VectorStore>,
        ns: &str,
        id: &str,
        session_id: Option<&str>,
        worktree: Option<&str>,
    ) {
        let vector = StubEmbedder.embed("seed").await.unwrap();
        let mut task = json!({ "status": "in_progress", "title": id });
        if let Some(s) = session_id {
            task["session_id"] = json!(s);
        }
        if let Some(w) = worktree {
            task["worktree"] = json!(w);
        }
        store
            .upsert(
                ns,
                id,
                vector,
                MemoryPayload {
                    content: "task".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "task": task }),
                },
            )
            .await
            .expect("seed task");
    }

    /// Read the task id attributed on the first `events[]` receipt.
    fn receipt_attributed_task(tmp: &tempfile::TempDir) -> Option<String> {
        let ledger: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".NeuroStrata/session_queries.json"))
                .expect("ledger written"),
        )
        .expect("valid ledger");
        ledger["events"][0]["task_id"].as_str().map(str::to_string)
    }

    /// C15 follow-up (Codex P2): two in-progress tasks in one namespace with
    /// different session ids — the receipt must go to the caller's session,
    /// not the lexicographically smaller task.
    #[tokio::test]
    async fn receipt_fallback_attributes_by_session() {
        use crate::guard::testkit::MockStore;
        use crate::task::tool_receipts::build_dispatch_receipt;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "C15SessionAttribution";
        seed_attributed_task(&store, ns, "a-task", Some("session-alpha"), None).await;
        seed_attributed_task(&store, ns, "b-task", Some("session-beta"), None).await;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();
        let fields = build_dispatch_receipt(
            "evt-c15-session".to_string(),
            1,
            "session-beta".to_string(),
            ns.to_string(),
            "neurostrata_locate".to_string(),
            "q",
            &json!({"memories": [{"id": "m1"}]}).to_string(),
            false,
            1,
        );
        assert!(fields.task_id.is_empty());
        record_tool_receipt(&store, fields, Some(&caller_root)).await;

        assert_eq!(
            receipt_attributed_task(&tmp).as_deref(),
            Some("b-task"),
            "receipt must follow fields.session_id, not the lowest id"
        );
    }

    /// A session match beats a lexicographically smaller id, and the combined
    /// session+worktree tier outranks either single-context tier.
    #[tokio::test]
    async fn receipt_session_match_beats_smaller_id_and_worktree() {
        use crate::guard::testkit::MockStore;
        use crate::task::tool_receipts::build_dispatch_receipt;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "C15SessionWorktreeAttribution";
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();
        // Smaller id: only the session matches.
        seed_attributed_task(
            &store,
            ns,
            "a-task",
            Some("session-beta"),
            Some("other-root"),
        )
        .await;
        // Larger id: both session and worktree match the caller.
        seed_attributed_task(
            &store,
            ns,
            "z-task",
            Some("session-beta"),
            Some(caller_root.as_str()),
        )
        .await;
        let fields = build_dispatch_receipt(
            "evt-c15-both".to_string(),
            1,
            "session-beta".to_string(),
            ns.to_string(),
            "neurostrata_locate".to_string(),
            "q",
            &json!({"memories": [{"id": "m1"}]}).to_string(),
            false,
            1,
        );
        record_tool_receipt(&store, fields, Some(&caller_root)).await;

        assert_eq!(
            receipt_attributed_task(&tmp).as_deref(),
            Some("z-task"),
            "session+worktree match must beat the smaller id and session-only match"
        );
    }

    /// With no session match, a worktree-only match still wins over the
    /// lowest-id fallback.
    #[tokio::test]
    async fn receipt_fallback_attributes_by_worktree() {
        use crate::guard::testkit::MockStore;
        use crate::task::tool_receipts::build_dispatch_receipt;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "C15WorktreeAttribution";
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();
        seed_attributed_task(
            &store,
            ns,
            "a-task",
            Some("session-alpha"),
            Some("other-root"),
        )
        .await;
        seed_attributed_task(
            &store,
            ns,
            "b-task",
            Some("session-beta"),
            Some(caller_root.as_str()),
        )
        .await;
        let fields = build_dispatch_receipt(
            "evt-c15-worktree".to_string(),
            1,
            "session-unmatched".to_string(),
            ns.to_string(),
            "neurostrata_locate".to_string(),
            "q",
            &json!({"memories": [{"id": "m1"}]}).to_string(),
            false,
            1,
        );
        record_tool_receipt(&store, fields, Some(&caller_root)).await;

        assert_eq!(
            receipt_attributed_task(&tmp).as_deref(),
            Some("b-task"),
            "worktree-only match must beat the lowest-id fallback"
        );
    }

    /// Caller truth always wins: an explicit non-empty task id is never
    /// overwritten by any session/worktree fallback.
    #[tokio::test]
    async fn receipt_explicit_task_id_wins_over_fallback() {
        use crate::guard::testkit::MockStore;
        use crate::task::tool_receipts::build_dispatch_receipt;

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let ns = "C15ExplicitTaskId";
        seed_attributed_task(&store, ns, "a-task", Some("session-beta"), None).await;
        seed_attributed_task(&store, ns, "explicit-task", None, None).await;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".NeuroStrata")).unwrap();
        let caller_root = tmp.path().to_str().unwrap().to_string();
        let mut fields = build_dispatch_receipt(
            "evt-c15-explicit".to_string(),
            1,
            "session-beta".to_string(),
            ns.to_string(),
            "neurostrata_locate".to_string(),
            "q",
            &json!({"memories": [{"id": "m1"}]}).to_string(),
            false,
            1,
        );
        fields.task_id = "explicit-task".to_string();
        record_tool_receipt(&store, fields, Some(&caller_root)).await;

        assert_eq!(
            receipt_attributed_task(&tmp).as_deref(),
            Some("explicit-task"),
            "explicit fields.task_id must outrank any fallback"
        );
    }

    /// The git branch of the wiring panel (Q2/Q5): the default instance is
    /// named first and dominant, and a verified hook subtracts its own
    /// instruction instead of re-instructing.
    #[test]
    fn git_projects_get_the_default_instance_and_verified_subtraction() {
        let root = std::env::temp_dir().join(format!("ns-setup-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        write_exec_hook(
            &root.join(".git").join("hooks").join("pre-push"),
            "#!/bin/bash\n# NeuroStrata task gate marker\n",
        );

        let (wiring_block, verified, wire_instruction) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(true));
        assert_eq!(verified["gate_point_resolved"], json!(true));
        let text = wire_instruction["text"].as_str().unwrap();
        assert!(text.contains("Git detected"), "{}", text);
        assert!(text.contains("default and recommended"), "{}", text);
        assert!(text.contains("task-gate"), "{}", text);
        assert!(!text.contains("No git detected"), "{}", text);
        let task_gate = wiring_block["automatic"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == "task-gate")
            .expect("the git default instance is emitted with git");
        assert_eq!(task_gate["runs_in"], json!("git-pre-push"));
        assert_eq!(wiring_block["coverage"]["uncovered"], json!([]));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// FINDING-bootstrap-wiring-nudge: the clause must lead with the hook
    /// (works on any git host) and demote CI to the optional second layer.
    /// It used to end "wire the project-pipeline gates into your CI", which
    /// made CI the default and locked out every non-GitHub project.
    #[test]
    fn git_clause_leads_with_hooks_and_demotes_ci_to_optional() {
        let root = std::env::temp_dir().join(format!("ns-clause-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();

        let (_, _, wire_instruction) = wiring_panel(&root, "MyProj");
        let text = wire_instruction["text"].as_str().unwrap();
        assert!(text.contains("Git detected"), "{}", text);
        assert!(text.contains("default and recommended"), "{}", text);
        // required step: the hook, and it must run the gate
        assert!(text.contains("hooks install"), "{}", text);
        assert!(text.contains("--strict"), "{}", text);
        // the broader-solution principle, stated
        assert!(text.contains("EVERY git project"), "{}", text);
        // CI is optional + vendor-specific, not the destination
        assert!(text.contains("optional and vendor-specific"), "{}", text);
        assert!(text.contains("defense-in-depth"), "{}", text);
        assert!(text.contains("needs nothing beyond the hook"), "{}", text);
        // the regression itself: CI is no longer the thing to wire into
        assert!(
            !text.contains("into your CI"),
            "CI is not the default: {}",
            text
        );
        // and the agent is handed a runnable hook, not a suggestion
        let files = wire_instruction["files"]
            .as_array()
            .expect("scaffolded files");
        let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            vec![
                "scripts/hooks/pre-push",
                "scripts/hooks/pre-commit",
                "scripts/install-hooks.sh",
                "scripts/task-coverage-gate.sh",
            ]
        );
        let hook = files[0]["content"].as_str().unwrap();
        assert!(
            hook.contains("task gate") && hook.contains("--strict"),
            "{}",
            hook
        );
        assert!(
            hook.contains("PROJECT_GATES"),
            "the hook is the extension point: {}",
            hook
        );
        // vendor-neutral: no CI vendor is named in the runnable files
        assert!(!hook.contains("github"), "{}", hook);
        assert!(!files[1]["content"].as_str().unwrap().contains("github"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Q5: a `git-pre-push` claim with no observable hook is a lie. It must
    /// stay in the must-wire list, never be subtracted as verified.
    #[test]
    fn git_pre_push_without_a_hook_stays_in_the_must_wire_list() {
        let root = std::env::temp_dir().join(format!("ns-nohook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();

        let (wiring_block, verified, _) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(false));
        // not covered, and not quietly reported as covered
        assert_eq!(wiring_block["coverage"]["git-pre-push"], json!([]));
        assert!(
            wiring_block["coverage"]["uncovered"]
                .as_array()
                .unwrap()
                .iter()
                .any(|u| u == "task-gate"),
            "an unwired pre-push gate is uncovered"
        );
        // the gate is still advertised at all (git exists)
        assert!(wiring_block["automatic"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == "task-gate" && w["runs_in"] == "git-pre-push"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The mirror: with the scaffolded hook installed the gate verifies. But a
    /// hook file existing is NOT a project-pipeline guard running — the
    /// scaffold ships PROJECT_GATES empty — so the panel must not claim the
    /// pipeline guards are enforced (fix round 1, Important #1).
    #[test]
    fn installed_scaffolded_hook_verifies_the_gate_without_faking_pipeline_coverage() {
        let root = std::env::temp_dir().join(format!("ns-hooked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        // install the scaffold exactly as the installer would
        write_exec_hook(
            &root.join(".git").join("hooks").join("pre-push"),
            SCAFFOLDED_PRE_PUSH_HOOK,
        );

        let (wiring_block, verified, _) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(true));
        assert_eq!(
            wiring_block["coverage"]["git-pre-push"],
            json!(["task-gate"])
        );
        assert_eq!(wiring_block["coverage"]["uncovered"], json!([]));
        // the task-coverage gate is wired by design; it is a real guard, not faked
        assert_eq!(
            verified["hook_project_guards"],
            json!(["./scripts/task-coverage-gate.sh"])
        );
        // and no per-guard claim of client-side coverage survives
        assert!(
            wiring_block["automatic"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w.get("client_side").is_none() && w.get("hook_runs_this").is_none()),
            "an unobservable coverage flag must not be emitted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A guard the project actually put in PROJECT_GATES is reported as such.
    #[test]
    fn guards_wired_into_project_gates_are_reported_from_the_hook_body() {
        let root = std::env::temp_dir().join(format!("ns-pgates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        let hook = SCAFFOLDED_PRE_PUSH_HOOK.replace(
            "    # ./scripts/rules-gate.sh",
            "    ./scripts/rules-gate.sh\n    ./scripts/typecheck.sh",
        );
        write_exec_hook(&root.join(".git").join("hooks").join("pre-push"), &hook);

        let (_, verified, _) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(true));
        assert_eq!(
            verified["hook_project_guards"],
            json!([
                "./scripts/task-coverage-gate.sh",
                "./scripts/rules-gate.sh",
                "./scripts/typecheck.sh"
            ])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Important #2: an installed hook subtracts the install instruction AND
    /// the scaffolded files. Both at once — a hooked project told to install
    /// a second gate over its working one is the same failure this panel
    /// exists to prevent.
    #[test]
    fn hooked_project_gets_no_scaffold_files_and_no_install_instruction() {
        let root = std::env::temp_dir().join(format!("ns-hooked-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        write_exec_hook(
            &root.join(".git").join("hooks").join("pre-push"),
            SCAFFOLDED_PRE_PUSH_HOOK,
        );

        let (_, _, wire_instruction) = wiring_panel(&root, "MyProj");
        assert_eq!(
            wire_instruction["files"],
            json!([]),
            "no scaffold over a live hook"
        );
        let text = wire_instruction["text"].as_str().unwrap();
        assert!(
            !text.contains("hooks install"),
            "no second install: {}",
            text
        );
        assert!(
            !text.contains("install-hooks.sh"),
            "no second install: {}",
            text
        );
        // still actionable: the remaining work is the project guards
        assert!(text.contains("PROJECT_GATES"), "{}", text);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A pipeline guard with no hook is CI-only: say so. The bootstrap still
    /// nudges toward the hook, because that is the broader defense.
    #[test]
    fn ci_only_pipeline_guards_report_hook_runs_this_false() {
        let root = std::env::temp_dir().join(format!("ns-cionly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        std::fs::create_dir_all(root.join(".github").join("workflows")).unwrap();
        std::fs::write(
            root.join(".github").join("workflows").join("ci.yml"),
            "on: push",
        )
        .unwrap();

        let (wiring_block, verified, wire_instruction) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(false));
        assert_eq!(verified["hook_project_guards"], json!([]));
        // no hook, so nothing runs on a bare `git push` yet
        assert!(wiring_block["automatic"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["runs_in"] == "project-pipeline")
            .all(|w| w.get("client_side").is_none() && w.get("hook_runs_this").is_none()));
        let text = wire_instruction["text"].as_str().unwrap();
        assert!(
            text.contains("hooks install"),
            "still nudged toward the hook: {}",
            text
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project that moved its hooks dir via core.hooksPath is hooked, not
    /// unhooked; reporting otherwise would send the agent to wire a second,
    /// redundant gate. The config is read with `git config`, the same source
    /// the installer uses, so global and system levels count too.
    #[cfg(unix)]
    #[test]
    fn core_hooks_path_counts_as_observable() {
        if which_git().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("ns-hookspath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_in(&root, &["init", "-q"]);
        std::fs::create_dir_all(root.join(".githooks")).unwrap();
        git_in(&root, &["config", "core.hooksPath", ".githooks"]);
        write_exec_hook(
            &root.join(".githooks").join("pre-push"),
            SCAFFOLDED_PRE_PUSH_HOOK,
        );

        let (_, verified, _) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Important #4: a stale gate left in `$GIT_DIR/hooks` after the project
    /// relocated its hooks dir is not a gate — git never runs it. Reporting
    /// it as installed means the push gate silently stopped running.
    #[cfg(unix)]
    #[test]
    fn stale_local_hook_is_not_observable_once_hooks_path_relocates() {
        if which_git().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("ns-stalehook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_in(&root, &["init", "-q"]);
        // the gate that used to run
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        write_exec_hook(
            &root.join(".git").join("hooks").join("pre-push"),
            SCAFFOLDED_PRE_PUSH_HOOK,
        );
        // hooks moved, and nothing was installed at the new location
        std::fs::create_dir_all(root.join(".githooks")).unwrap();
        git_in(&root, &["config", "core.hooksPath", ".githooks"]);

        let (wiring_block, verified, wire_instruction) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(false));
        assert!(
            wiring_block["coverage"]["uncovered"]
                .as_array()
                .unwrap()
                .iter()
                .any(|u| u == "task-gate"),
            "a gate that never runs is uncovered"
        );
        // so the project is told to install at the path git actually runs
        assert!(wire_instruction["text"]
            .as_str()
            .unwrap()
            .contains("hooks install"));
        assert!(!wire_instruction["files"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Important #3: the resolved hooks dir comes from `git config`, so a
    /// `core.hooksPath` set at a higher level than the local file is honoured
    /// exactly as the installer honours it.
    #[cfg(unix)]
    #[test]
    fn hooks_dir_uses_the_same_source_of_truth_as_the_installer() {
        if which_git().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("ns-hookspath-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_in(&root, &["init", "-q"]);
        // the assertions read through the ambient config too (git_hooks_dir
        // runs plain `git config`), so skip where a personal config would
        // decide the outcome rather than this repo's.
        let ambient = std::process::Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .current_dir(&root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ambient {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        // no hooksPath anywhere -> $GIT_DIR/hooks
        assert_eq!(git_hooks_dir(&root), root.join(".git").join("hooks"));

        // what `git config --get` reports is what we use, absolute or not
        git_in(&root, &["config", "core.hooksPath", ".githooks"]);
        assert_eq!(git_hooks_dir(&root), root.join(".githooks"));
        let abs = root.join("elsewhere").join("hooks");
        let abs_arg = abs.display().to_string();
        git_in(&root, &["config", "core.hooksPath", &abs_arg]);
        assert_eq!(git_hooks_dir(&root), abs);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Important #5: the installer must not run the hook it installs. A
    /// project that fills PROJECT_GATES would otherwise run every guard on
    /// every `install-hooks.sh`.
    #[test]
    fn installer_reports_presence_without_executing_the_hook() {
        assert!(
            !SCAFFOLDED_INSTALL_HOOKS.contains("$dest_dir/pre-push\" </dev/null"),
            "the installer must not execute the hook: {}",
            SCAFFOLDED_INSTALL_HOOKS
        );
        // the installed path appears only in the install, the existence test
        // and echoes -- never as a bare invocation
        for line in SCAFFOLDED_INSTALL_HOOKS.lines() {
            let line = line.trim();
            if !line.contains("$dest_dir/pre-push") {
                continue;
            }
            assert!(
                line.starts_with("#")
                    || line.starts_with("echo ")
                    || line.contains("install -m")
                    || line.contains("[ -x "),
                "unexpected execution of the installed hook: {}",
                line
            );
        }
    }

    /// An option-like or otherwise invalid checkout basename must never be
    /// handed to `task gate` as a namespace.
    #[cfg(unix)]
    #[test]
    fn pre_push_hook_skips_an_invalid_folder_namespace() {
        use std::os::unix::fs::PermissionsExt;
        if which_bash().is_none() || which_git().is_none() {
            return;
        }
        let base = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(base.path()).unwrap().join("--help");
        std::fs::create_dir_all(&root).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        let hook = root.join("pre-push");
        std::fs::write(&hook, SCAFFOLDED_PRE_PUSH_HOOK).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let bin = base.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("neurostrata-mcp");
        std::fs::write(
            &stub,
            "#!/bin/sh\nif [ \"$1\" = namespace ]; then echo valid-ns; exit 0; fi\nif [ \"$1\" = task ]; then echo \"$3\" >> \"$GATE_LOG\"; exit 0; fi\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = base.path().join("gated");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = std::process::Command::new("bash")
            .arg(&hook)
            .current_dir(&root)
            .env("PATH", &path)
            .env("GATE_LOG", &log)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let gated = std::fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = gated.lines().collect();
        assert!(lines.contains(&"valid-ns"), "{gated}");
        assert!(
            !lines
                .iter()
                .any(|l| !l.is_empty() && !l.starts_with(|c: char| c.is_ascii_alphanumeric())),
            "{gated}"
        );
    }

    /// Non-git: no hook to report, no hook to scaffold, and the gates that
    /// still need a chokepoint stay uncovered.
    #[test]
    fn non_git_projects_get_no_scaffold_and_no_hooks_path() {
        let root = std::env::temp_dir().join(format!("ns-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let (wiring_block, verified, wire_instruction) = wiring_panel(&root, "MyProj");
        assert_eq!(verified["hook_installed"], json!(false));
        assert_eq!(verified["gate_point_resolved"], json!(false));
        assert_eq!(wiring_block["coverage"]["git-pre-push"], json!([]));
        assert_eq!(wire_instruction["files"], json!([]));
        assert!(
            !wiring_block["automatic"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["runs_in"] == "git-pre-push"),
            "no git, no git-pre-push category"
        );
        assert!(wiring_block["coverage"]["uncovered"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u == "memory-to-repo-drift"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The scaffolded hook is a runnable program, not a snippet: a stubbed
    /// gate decides its exit code. This is the property that flips the path
    /// of least resistance toward the hook.
    #[cfg(unix)]
    #[test]
    fn scaffolded_pre_push_hook_runs_and_exits_with_the_gate_verdict() {
        use std::os::unix::fs::PermissionsExt;
        if which_bash().is_none() || which_git().is_none() {
            return; // nothing to smoke-test with
        }
        let root = std::env::temp_dir().join(format!("ns-hookrun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scripts").join("hooks")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        // a git project, so `git rev-parse --show-toplevel` resolves
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());

        let hook = root.join("scripts").join("hooks").join("pre-push");
        std::fs::write(&hook, SCAFFOLDED_PRE_PUSH_HOOK).unwrap();
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();

        // the installer must be a runnable script too
        std::fs::write(
            root.join("scripts").join("install-hooks.sh"),
            SCAFFOLDED_INSTALL_HOOKS,
        )
        .unwrap();
        let syntax = std::process::Command::new("bash")
            .arg("-n")
            .arg(root.join("scripts").join("install-hooks.sh"))
            .status()
            .unwrap();
        assert!(syntax.success(), "install-hooks.sh must be valid bash");

        // stub the gate: its exit code is the only variable
        let stub = root.join("bin").join("neurostrata-mcp");
        std::fs::write(
            &stub,
            "#!/bin/sh\nif [ \"$1\" = namespace ]; then echo example-project; exit 0; fi\nexit ${STUB_GATE_EXIT:-0}\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );

        // Create a stub project gate that always passes (test is about hook infrastructure)
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(
            root.join("scripts").join("task-coverage-gate.sh"),
            "#!/bin/bash\n# stub gate for test\ntask-coverage-gate: no untracked work items found\nexit 0\n",
        ).unwrap();
        let mut perms = std::fs::metadata(root.join("scripts").join("task-coverage-gate.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(root.join("scripts").join("task-coverage-gate.sh"), perms)
            .unwrap();

        let run_gate = |code: i32| {
            std::process::Command::new("bash")
                .arg("scripts/hooks/pre-push")
                .current_dir(&root)
                .env("PATH", &path)
                .env("STUB_GATE_EXIT", code.to_string())
                .output()
                .unwrap()
        };

        let clean = run_gate(0);
        assert!(
            clean.status.success(),
            "a passing gate must let the push through"
        );
        let blocked = run_gate(1);
        assert_eq!(
            blocked.status.code(),
            Some(1),
            "a failing gate must fail the push"
        );
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("Push blocked"));

        // and the round trip: installing it makes the panel believe it
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::copy(&hook, root.join(".git").join("hooks").join("pre-push")).unwrap();
        assert!(
            hook_observable(&root),
            "the installed scaffold must be observable"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Run git in `root`, isolated from the developer's own global/system
    /// config so a personal `core.hooksPath` cannot change what is asserted.
    #[cfg(unix)]
    fn git_in(root: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {:?} failed", args);
    }

    #[cfg(unix)]
    fn which_bash() -> Option<String> {
        std::process::Command::new("bash")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| "bash".to_string())
    }

    #[cfg(unix)]
    fn which_git() -> Option<String> {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| "git".to_string())
    }

    /// Guinea-pig BUG-4: suggestions follow the counted tree, not manifest
    /// presence. One tooling JS file must not outvote eight Go files.
    #[test]
    fn language_suggestions_follow_file_counts_not_manifests() {
        let root = std::env::temp_dir().join(format!("ns-setup-dom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("go.mod"), "module x").unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        for i in 0..8 {
            std::fs::write(root.join(format!("f{}.go", i)), "").unwrap();
        }
        std::fs::write(root.join("tool.js"), "").unwrap();

        let scan = detect_project(&root);
        assert_eq!(scan.language_counts.first().unwrap().0, "go");
        let rules = suggested_rules(&scan);
        let contents: Vec<String> = rules
            .iter()
            .filter_map(|r| r.get("content").and_then(|c| c.as_str()).map(String::from))
            .collect();
        assert!(contents.iter().any(|c| c.contains("Go project")));
        assert!(
            !contents.iter().any(|c| c.contains("JavaScript")),
            "1 tooling js file out of 9 must not earn a rule"
        );
        assert!(rules
            .iter()
            .all(|r| r["heuristic"] == true && r["verified"] == false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ns_key_lowercases_and_sanitizes() {
        assert_eq!(ns_key("MyProj"), "myproj");
        assert_eq!(ns_key("NeuroStrata"), "neurostrata");
        assert_eq!(ns_key("my project"), "my-project");
        assert_eq!(ns_key("..."), "ns", "a keyless namespace still gets an id");
    }

    #[test]
    fn ids_are_ns_key_plus_four_base36_chars() {
        let mut used = HashSet::new();
        let id = allocate_task_id("myproj", &used).expect("allocated");
        assert!(id.starts_with("myproj-"), "{}", id);
        let suffix = &id["myproj-".len()..];
        assert_eq!(suffix.len(), 4);
        assert!(
            suffix.chars().all(|c| c.is_ascii_alphanumeric()),
            "{}",
            suffix
        );
        used.insert(id.clone());
        let second = allocate_task_id("myproj", &used).expect("allocated");
        assert_ne!(id, second, "collisions are checked, not trusted");
    }

    #[tokio::test]
    async fn task_create_refuses_a_namespace_that_does_not_exist_yet() {
        let store = temp_store().await; // nothing written: no namespaces at all
        let reply = handle_task_create(
            json!({ "namespace": "NewProj", "title": "first" }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(
            reply.starts_with("ERROR: namespace 'NewProj' does not exist"),
            "{}",
            reply
        );
        assert!(reply.contains("neurostrata_bootstrap"), "{}", reply);
        assert!(
            store.list_namespaces().await.unwrap().is_empty(),
            "a refused create must not invent the namespace"
        );
    }

    /// An agent that asks for `task_type: "recurring"` (or any non-task/bug/
    /// feature/epic value) is asking the wrong question: tasks must reach
    /// `done`, a recurring pattern is not a single work item. The error is
    /// the cheapest discovery surface to fix — every other agent that hits
    /// this dead end learns nothing without it.
    #[tokio::test]
    async fn task_create_rejects_unknown_task_type_with_a_procedure_signpost() {
        let store = store_with_namespace("MyProj").await;
        let reply = handle_task_create(
            json!({
                "namespace": "MyProj",
                "title": "monthly GSC measurement",
                "task_type": "recurring"
            }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(
            reply.starts_with("ERROR: unknown task_type 'recurring'"),
            "{}",
            reply
        );
        // The signpost: this is where agents cross over to procedure memory.
        assert!(
            reply.contains("procedure") || reply.contains("neurostrata_procedure_perform"),
            "the rejection must point at the procedure path: {reply}"
        );
        assert!(
            reply.contains("neurostrata_add_memory"),
            "the rejection must name the creation tool: {reply}"
        );
    }

    #[tokio::test]
    async fn task_create_writes_a_task_with_the_design_record_shape() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "Migrate beads issues", 1).await;
        assert!(id.starts_with("myproj-"), "{}", id);

        let (_, payload) = store.get("MyProj", &id).await.unwrap().expect("stored");
        assert_eq!(payload.memory_type, "task");
        assert_eq!(payload.content, "Migrate beads issues");
        assert_eq!(payload.metadata["task"]["status"], json!("open"));
        assert_eq!(payload.metadata["task"]["priority"], json!(1));
        assert_eq!(payload.metadata["task"]["task_type"], json!("task"));
        assert!(payload.metadata["task"]["created_at"]
            .as_str()
            .unwrap()
            .ends_with('Z'));
        assert!(payload.metadata["task"]["history"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn task_create_writes_a_contain_edge_declaration_for_a_parent() {
        let store = store_with_namespace("MyProj").await;
        let parent = create_task(&store, "MyProj", "epic", 2).await;
        let reply = handle_task_create(
            json!({ "namespace": "MyProj", "title": "subtask", "parent_id": parent }),
            emb(),
            store.clone(),
        )
        .await;
        let child: Value = serde_json::from_str(&reply).expect("json reply");
        let child_id = child["id"].as_str().unwrap().to_string();
        let (_, payload) = store.get("MyProj", &child_id).await.unwrap().unwrap();
        assert_eq!(payload.metadata["contained_by"], json!([parent]));
    }

    #[tokio::test]
    async fn task_update_refuses_done_and_points_at_task_complete() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "done" }),
            emb(),
            store.clone(),
        )
        .await;
        assert_eq!(reply, DONE_FUNNEL_MESSAGE);
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::Open,
            "the refused update must not touch the row"
        );
    }

    #[tokio::test]
    async fn task_update_appends_a_note_without_moving_the_state() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "note": "3 of 5: parser done" }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["status"], json!("open"));
        assert!(parsed["transition"].is_null(), "a note is not a transition");

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        let history = payload.metadata["task"]["history"].as_array().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["from"], "open");
        assert_eq!(history[0]["to"], "open");
        assert_eq!(history[0]["note"], "3 of 5: parser done");
    }

    /// R7 fix round 1: the count-based nudge is retired but its two wire
    /// fields stay present, so existing `task_update` consumers keep parsing
    /// the same shape. The nudge itself is null, never a manufactured warning.
    #[tokio::test]
    async fn task_update_keeps_legacy_graph_fields_byte_compatible() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "note": "still here" }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed.get("graph_queries_count"), Some(&json!(0)));
        assert_eq!(parsed.get("graph_lookup_nudge"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn task_update_refuses_an_illegal_transition_with_the_legal_set() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        // open -> open is not an edge; the machine teaches instead.
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "open", "note": "nudge" }),
            emb(),
            store.clone(),
        )
        .await;
        // status equal to current falls back to annotation, which is legal.
        assert!(reply.contains("\"transition\": null"), "{}", reply);

        // blocked -> in_progress is not in the table.
        handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "blocked", "note": "waiting" }),
            emb(),
            store.clone(),
        )
        .await;
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "in_progress" }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(
            reply.starts_with("ERROR: Illegal transition blocked -> in_progress"),
            "{}",
            reply
        );
        assert!(reply.contains("legal transitions are: open"), "{}", reply);
    }

    #[tokio::test]
    async fn task_update_on_an_unknown_status_names_the_valid_ones() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "canceled" }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(reply.contains("unknown status 'canceled'"), "{}", reply);
        assert!(
            reply.contains("done only via neurostrata_task_complete"),
            "{}",
            reply
        );
    }

    #[tokio::test]
    async fn task_update_renames_the_title_and_keeps_the_old_wording_in_history() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "Fix Kuzu parallel-test mmap flake", 2).await;

        let reply = handle_task_update(
            json!({
                "namespace": "MyProj",
                "id": id,
                "title": "Fix LadybugDB parallel-test mmap flake (smaller test buffer pool)",
            }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert!(
            parsed["renamed"]
                .as_str()
                .unwrap()
                .starts_with("renamed: 'Fix Kuzu"),
            "the reply names what it renamed: {}",
            reply
        );
        assert!(
            parsed["transition"].is_null(),
            "a rename is not a transition"
        );

        // Persisted: content IS the title (section 1.1).
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(
            payload.content,
            "Fix LadybugDB parallel-test mmap flake (smaller test buffer pool)"
        );
        assert_eq!(
            parsed["task"]["title"],
            json!("Fix LadybugDB parallel-test mmap flake (smaller test buffer pool)"),
            "the summary carries the new wording"
        );

        // The scrub is recoverable: history names both wordings.
        let history = payload.metadata["task"]["history"].as_array().unwrap();
        assert_eq!(history.len(), 1, "one entry, for the rename");
        assert_eq!(history[0]["from"], "open");
        assert_eq!(history[0]["to"], "open", "no state moved");
        assert!(
            history[0]["note"].as_str().unwrap().contains("Fix Kuzu")
                && history[0]["note"]
                    .as_str()
                    .unwrap()
                    .contains("Fix LadybugDB"),
            "history keeps both titles: {:?}",
            history[0]["note"]
        );
    }

    #[tokio::test]
    async fn renaming_re_embeds_because_the_vector_is_the_embedded_title() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the old words", 2).await;

        // A rename that skipped the re-embed would leave search matching the
        // old title, so the embedder is on the critical path.
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "title": "the new words" }),
            Arc::new(FailEmbedder),
            store.clone(),
        )
        .await;
        assert!(
            reply.starts_with("ERROR: Failed to embed the task title"),
            "{}",
            reply
        );
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(
            payload.content, "the old words",
            "a failed re-embed must not leave a title nothing can search"
        );

        // The same call with a working embedder moves the vector too.
        handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "title": "the new words" }),
            emb(),
            store.clone(),
        )
        .await;
        let (vector, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(payload.content, "the new words");
        let expected = StubEmbedder.embed("the new words").await.unwrap();
        assert_eq!(vector, expected, "the stored vector is the new title's");
    }

    #[tokio::test]
    async fn task_update_refuses_an_empty_title() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        for blank in ["", "   ", "\n\t "] {
            let reply = handle_task_update(
                json!({ "namespace": "MyProj", "id": id, "title": blank }),
                emb(),
                store.clone(),
            )
            .await;
            assert!(
                reply.contains("'title' cannot be empty"),
                "blank title {:?} -> {}",
                blank,
                reply
            );
        }
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(
            payload.content, "the work",
            "a refused rename writes nothing"
        );
        assert!(payload.metadata["task"]["history"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn task_rename_invalidates_and_recomputes_subagent_prompt_injection() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "original title", 1).await;
        handle_task_claim(
            json!({
                "namespace": "MyProj",
                "id": id,
                "assignee": "agent",
                "session_id": "sess-rename"
            }),
            store.clone(),
        )
        .await;

        let list_before: Value = serde_json::from_str(
            &handle_task_list(
                json!({ "namespace": "MyProj", "status": "in_progress" }),
                store.clone(),
            )
            .await,
        )
        .unwrap();
        let prompt_before = list_before["tasks"][0]["subagent_prompt_injection"]
            .as_str()
            .unwrap();
        assert!(prompt_before.contains("Goal: original title"));

        handle_task_update(
            json!({
                "namespace": "MyProj",
                "id": id,
                "title": "updated renamed title",
                "session_id": "sess-rename"
            }),
            emb(),
            store.clone(),
        )
        .await;

        let list_after: Value = serde_json::from_str(
            &handle_task_list(
                json!({ "namespace": "MyProj", "status": "in_progress" }),
                store.clone(),
            )
            .await,
        )
        .unwrap();
        let prompt_after = list_after["tasks"][0]["subagent_prompt_injection"]
            .as_str()
            .unwrap();
        assert!(
            prompt_after.contains("Goal: updated renamed title"),
            "expected updated goal in prompt: {}",
            prompt_after
        );
        assert!(!prompt_after.contains("Goal: original title"));
    }

    #[tokio::test]
    async fn task_create_and_rename_agree_on_the_title_limit() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        let too_long = "x".repeat(MAX_TITLE + 1);

        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "title": too_long.clone() }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(
            reply.contains(&format!("the maximum is {}", MAX_TITLE)),
            "{}",
            reply
        );

        let reply = handle_task_create(
            json!({ "namespace": "MyProj", "title": too_long }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(reply.contains("the maximum is"), "{}", reply);

        // Exactly at the limit is legal on both paths.
        let at_limit = "y".repeat(MAX_TITLE);
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "title": at_limit.clone() }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(!reply.starts_with("ERROR"), "{}", reply);
        assert_eq!(reply_title(&reply), at_limit);
    }

    /// A rename is not a silent rewrite: an unchanged title writes no history.
    #[tokio::test]
    async fn renaming_to_the_same_title_is_a_no_op() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "title": "the work" }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(reply.contains("\"renamed\": null"), "{}", reply);
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert!(payload.metadata["task"]["history"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn renaming_a_task_that_does_not_exist_answers_like_the_rest_of_the_surface() {
        let store = store_with_namespace("MyProj").await;
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": "myproj-zzzz", "title": "anything" }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(
            reply.starts_with("ERROR: no task with id 'myproj-zzzz' in namespace 'MyProj'"),
            "{}",
            reply
        );
        assert!(
            reply.contains("neurostrata_task_list"),
            "the same pointer every other miss gives: {}",
            reply
        );
    }

    #[test]
    fn claim_stamps_the_current_branch_and_worktree() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let store = store_with_namespace("MyProj").await;
            let id = create_task(&store, "MyProj", "the work", 2).await;
            let expected = gate::GateScope::capture(&crate::daemon::resolve_project_root());

            let reply = handle_task_claim(
                json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" }),
                store.clone(),
            )
            .await;
            assert!(reply.contains("\"status\": \"in_progress\""), "{}", reply);

            let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
            assert_eq!(payload.metadata["task"]["branch"], json!(expected.branch));
            assert_eq!(
                payload.metadata["task"]["worktree"],
                json!(expected.worktree)
            );
        });
    }

    #[tokio::test]
    async fn claim_resolves_scope_from_registry_project_root_and_explicit_worktree() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let non_git = tempfile::tempdir().unwrap();
        let git_wt = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-b", "feature/mcp-claim"])
            .current_dir(git_wt.path())
            .status()
            .unwrap();
        assert!(status.success());

        // Point daemon project root at a non-git directory (simulating daemon startup in $HOME),
        // while registering the namespace's project_root at the actual git worktree.
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", non_git.path());
        let store = store_with_namespace("MyProj").await;
        let rid = crate::registry::registry_row_id("MyProj");
        let reg_vec = StubEmbedder.embed("registry").await.unwrap();
        store
            .upsert(
                "MyProj",
                &rid,
                reg_vec,
                MemoryPayload {
                    content: "registry".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "structural".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "project_root": git_wt.path().to_string_lossy().to_string()
                    }),
                },
            )
            .await
            .unwrap();

        let id = create_task(&store, "MyProj", "mcp claimed work", 2).await;
        let reply = handle_task_claim(
            json!({
                "namespace": "MyProj",
                "id": id,
                "assignee": "agent-a",
                "session_id": "ses_mcp"
            }),
            store.clone(),
        )
        .await;
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert!(reply.contains("\"status\": \"in_progress\""), "{}", reply);

        let expected = gate::GateScope::capture(git_wt.path());
        assert_eq!(expected.branch.as_deref(), Some("feature/mcp-claim"));
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(payload.metadata["task"]["branch"], json!(expected.branch));
        assert_eq!(
            payload.metadata["task"]["worktree"],
            json!(expected.worktree)
        );

        // Another worktree pushing its own branch is not blocked by this MCP-claimed task.
        let other_scope = gate::GateScope {
            branch: Some("feature/other-wt".to_string()),
            worktree: Some("/repo/other-wt".to_string()),
        };
        let other_report = gate::run_scoped(&store, "MyProj", Some(&other_scope))
            .await
            .unwrap();
        assert!(
            !other_report
                .violations
                .iter()
                .any(|v| v.kind == "in_progress"),
            "other worktree must not be blocked: {:?}",
            other_report.violations
        );
    }

    #[test]
    fn task_summary_exposes_branch_and_worktree() {
        let payload = MemoryPayload {
            content: "scoped work".to_string(),
            user_id: "kenton".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "task": {
                    "status": "in_progress",
                    "branch": "feature/scope",
                    "worktree": "/wt/scope",
                }
            }),
        };
        let summary = task_summary("neurostrata-test", &payload);
        assert_eq!(summary["branch"], json!("feature/scope"));
        assert_eq!(summary["worktree"], json!("/wt/scope"));
    }

    #[tokio::test]
    async fn task_list_includes_branch_and_worktree() {
        let store = store_with_namespace("MyProj").await;
        let id = "task-scope-fields";
        let vector = StubEmbedder.embed("scope fields").await.unwrap();
        store
            .upsert(
                "MyProj",
                id,
                vector,
                MemoryPayload {
                    content: "scope fields".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "task": {
                            "status": "in_progress",
                            "branch": "feature/scope",
                            "worktree": "/wt/scope",
                        }
                    }),
                },
            )
            .await
            .unwrap();

        let list: Value = serde_json::from_str(
            &handle_task_list(json!({ "namespace": "MyProj" }), store.clone()).await,
        )
        .unwrap();
        assert_eq!(list["tasks"][0]["branch"], json!("feature/scope"));
        assert_eq!(list["tasks"][0]["worktree"], json!("/wt/scope"));
    }

    #[test]
    fn home_folder_gets_no_session_file() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".NeuroStrata")).unwrap();
        persist_active_session_file_with_home(
            home.path(),
            "example-project",
            &[],
            None,
            &[home.path().to_path_buf()],
        );
        assert!(!home
            .path()
            .join(".NeuroStrata/active_session.json")
            .exists());
        let work = home.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        persist_active_session_file_with_home(
            &work,
            "example-project",
            &[],
            None,
            &[home.path().to_path_buf()],
        );
        assert!(
            !work.join(".NeuroStrata/active_session.json").exists(),
            "a markerless folder under home is scratch, not a project"
        );
        // A marked descendant is a real non-git project and still gets state.
        std::fs::create_dir_all(work.join(".NeuroStrata")).unwrap();
        persist_active_session_file_with_home(
            &work,
            "example-project",
            &[],
            None,
            &[home.path().to_path_buf()],
        );
        assert!(work.join(".NeuroStrata/active_session.json").exists());
    }

    /// A markerless scratch folder below a non-git home is not a project:
    /// neither root resolution nor a session writer may adopt it.
    #[test]
    fn markerless_scratch_under_home_gets_no_session_state() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, _top, _nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        let scratch = home.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();

        let resolved = resolve_session_root_dir(&scratch);
        assert_ne!(
            resolved, scratch,
            "resolution must not adopt a markerless home descendant"
        );
        persist_active_session_file_with_home(
            &scratch,
            "example-project",
            &[],
            None,
            &[home.clone()],
        );
        assert!(!scratch.join(".NeuroStrata/active_session.json").exists());
    }

    /// Folders for the home-folder tests: `home` (the injected home folder,
    /// holding `.NeuroStrata` as SynapticMesh leaves it), `top` (a non-git
    /// project under it, holding `.NeuroStrata`) and `nowhere` (no marker,
    /// for `NEUROSTRATA_PROJECT_ROOT`). Every walk ends at `home` at the
    /// latest, so folders above the temp folder never affect the result.
    fn home_test_layout() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let home = dunce::canonicalize(tmp.path()).unwrap();
        let top = home.join("top");
        let nowhere = home.join("nowhere");
        std::fs::create_dir_all(home.join(".NeuroStrata")).unwrap();
        std::fs::create_dir_all(top.join(".NeuroStrata")).unwrap();
        std::fs::create_dir_all(&nowhere).unwrap();
        (tmp, home, top, nowhere)
    }

    fn ledger_at(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join(".NeuroStrata").join("session_queries.json")
    }

    #[tokio::test]
    async fn ledger_hint_walk_keeps_ancestor_neurostrata_except_home() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, top, nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &nowhere);
        let repo = top.join("scratch");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let file = repo.join("src").join("a.rs");
        std::fs::write(&file, "x").unwrap();
        let store = store_with_namespace("example-project").await;
        let hint = file.to_string_lossy().to_string();

        // A deep file in a non-git project finds the project's `.NeuroStrata`.
        record_graph_query_with_root(&store, "example-project", "t", &hint, None).await;
        assert!(ledger_at(&top).exists());
        assert!(!ledger_at(&home).exists());
        assert!(!ledger_at(&nowhere).exists());

        // The nearest marker wins: a `.git` below it takes the entry.
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        record_graph_query_with_root(&store, "example-project", "t", &hint, None).await;
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert!(ledger_at(&repo).exists());
        assert!(!ledger_at(&home).exists());
    }

    #[tokio::test]
    async fn ledger_hint_under_home_writes_nothing() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, top, nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        // A registry root that would otherwise catch it. The daemon root is
        // a marker-free folder, so lookups from tests running alongside
        // (which read the same variable) cannot write into `project`.
        let project = top.join("example-project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &nowhere);
        let store = store_with_namespace("example-project").await;
        crate::registry::cache_project_root(
            &store,
            Some(&emb()),
            "example-project",
            project.to_str().unwrap(),
        )
        .await;
        let docs = home.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        for file in [docs.join("a.md"), home.join("b.md")] {
            std::fs::write(&file, "x").unwrap();
            let hint = file.to_string_lossy().to_string();
            record_graph_query_with_root(&store, "example-project", "t", &hint, None).await;
        }
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert!(!ledger_at(&home).exists());
        assert!(!ledger_at(&docs).exists());
        assert!(
            !ledger_at(&project).exists(),
            "a hint under home stops; no fallback write"
        );

        // A home folder that is a git checkout is a project.
        std::fs::create_dir_all(home.join(".git")).unwrap();
        let hint = docs.join("a.md").to_string_lossy().to_string();
        record_graph_query_with_root(&store, "example-project", "t", &hint, None).await;
        assert!(ledger_at(&home).exists());
    }

    #[tokio::test]
    async fn daemon_root_walk_skips_home_but_keeps_ancestor_neurostrata() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, top, nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        let store = store_with_namespace("example-project").await;
        for daemon_root in [&home, &nowhere] {
            std::env::set_var("NEUROSTRATA_PROJECT_ROOT", daemon_root);
            record_graph_query_with_root(&store, "example-project", "t", "related", None).await;
        }
        assert!(!ledger_at(&home).exists());
        assert!(!nowhere.join(".NeuroStrata").exists());

        let sub = top.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &sub);
        record_graph_query_with_root(&store, "example-project", "t", "related", None).await;
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert!(ledger_at(&top).exists());
        assert!(!sub.join(".NeuroStrata").exists());
    }

    #[test]
    fn session_marker_walk_without_marker_finds_nothing() {
        let rel = std::path::Path::new("ns-no-such-folder-7f3a").join("sub");
        assert_eq!(walk_to_session_marker(&rel), SessionMarkerWalk::NotFound);
        assert_eq!(walk_to_worktree_marker(&rel), None);
        assert_eq!(walk_to_worktree_marker(std::path::Path::new("")), None);
    }

    #[tokio::test]
    async fn receipt_ledger_root_never_answers_home() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, top, nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        let store = temp_store().await;
        let ns = "example-project";
        store.init(ns).await.expect("init namespace schema");
        let sub = top.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        // (a) home without .git: as hint and as daemon root.
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &home);
        let got = receipt_ledger_root(Some(home.to_str().unwrap()), &store, ns).await;
        assert_eq!(got, None);
        // (b) an ancestor `.NeuroStrata` that is not home is a root.
        let got = receipt_ledger_root(Some(sub.to_str().unwrap()), &store, ns).await;
        assert_eq!(got, Some(top.clone()));
        // (c) no qualifying marker: no root, and the recorder writes nothing.
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &nowhere);
        assert_eq!(receipt_ledger_root(None, &store, ns).await, None);
        let fields = tool_receipts::ReceiptFields::new(
            "evt-none".to_string(),
            0,
            "sess-none".to_string(),
            String::new(),
            ns.to_string(),
            "neurostrata_related".to_string(),
            "related".to_string(),
            String::new(),
            "graph_read".to_string(),
            true,
            tool_receipts::ReceiptStatus::Success,
            0,
            1,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "q",
            "r",
        );
        record_tool_receipt(&store, fields, Some(nowhere.to_str().unwrap())).await;
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert!(!nowhere.join(".NeuroStrata").exists());
        assert!(!ledger_at(&home).exists());
        assert!(!ledger_at(&top).exists());

        // Home that is a git checkout is a project.
        std::fs::create_dir_all(home.join(".git")).unwrap();
        let got = receipt_ledger_root(Some(home.to_str().unwrap()), &store, ns).await;
        assert_eq!(got, Some(home.clone()));
    }

    /// A caller hint under a non-git home names no project: receipt-root
    /// resolution must stop there instead of landing in another project's
    /// ledger through the registry or daemon-root fallbacks.
    #[tokio::test]
    async fn receipt_root_stops_at_a_non_git_home_hint() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, top, nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        let project = top.join("example-project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &nowhere);
        let store = store_with_namespace("example-project").await;
        crate::registry::cache_project_root(
            &store,
            Some(&emb()),
            "example-project",
            project.to_str().unwrap(),
        )
        .await;
        let docs = home.join("docs");
        std::fs::create_dir_all(&docs).unwrap();

        let got =
            receipt_ledger_root(Some(docs.to_str().unwrap()), &store, "example-project").await;
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert_eq!(got, None, "a home hint must not resolve to another project");
    }

    #[tokio::test]
    async fn claim_from_home_keeps_its_worktree_stamp() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let (_tmp, home, _top, nowhere) = home_test_layout();
        let _home = override_home_dir_for_test(&home);
        std::env::set_var("NEUROSTRATA_PROJECT_ROOT", &nowhere);
        let store = store_with_namespace("example-project").await;
        let id = create_task(&store, "example-project", "work started from home", 2).await;
        let reply = handle_task_claim(
            json!({
                "namespace": "example-project",
                "id": id,
                "assignee": "agent-a",
                "session_id": "ses_home",
                "worktree": home.to_string_lossy(),
            }),
            store.clone(),
        )
        .await;
        std::env::remove_var("NEUROSTRATA_PROJECT_ROOT");
        assert!(reply.contains("\"status\": \"in_progress\""), "{}", reply);
        let (_, payload) = store.get("example-project", &id).await.unwrap().unwrap();
        let worktree = payload.metadata["task"]["worktree"]
            .as_str()
            .map(|w| dunce::canonicalize(w).unwrap());
        assert_eq!(
            worktree,
            Some(home.clone()),
            "a claim from home must not be stored unattributed"
        );
        assert!(!home
            .join(".NeuroStrata")
            .join("active_session.json")
            .exists());
    }

    #[tokio::test]
    async fn claim_writes_active_session_to_explicit_cwd_not_registry_root() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let store = store_with_namespace("MyProj").await;
        let rid = crate::registry::registry_row_id("MyProj");
        let reg_vec = StubEmbedder.embed("registry").await.unwrap();
        store
            .upsert(
                "MyProj",
                &rid,
                reg_vec,
                MemoryPayload {
                    content: "registry".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "structural".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "project_root": dir_a.path().to_string_lossy().to_string()
                    }),
                },
            )
            .await
            .unwrap();

        let id = create_task(&store, "MyProj", "scope me", 1).await;
        handle_task_claim(
            json!({
                "namespace": "MyProj",
                "id": id,
                "assignee": "agent",
                "session_id": "ses_wt",
                "cwd": dir_b.path().to_string_lossy().to_string(),
            }),
            store.clone(),
        )
        .await;

        let b_session = dir_b
            .path()
            .join(".NeuroStrata")
            .join("active_session.json");
        let a_session = dir_a
            .path()
            .join(".NeuroStrata")
            .join("active_session.json");
        assert!(
            b_session.exists(),
            "active_session.json must be written in the explicit cwd dir_b"
        );
        assert!(
            !a_session.exists(),
            "must not write into the registry project_root dir_a"
        );
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(&b_session).unwrap()).unwrap();
        assert_eq!(body["task_id"], json!(id));

        let expected_wt = dir_b
            .path()
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| dir_b.path().to_string_lossy().into_owned());
        let (_, claimed_payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(
            claimed_payload.metadata["task"]["worktree"],
            json!(expected_wt),
            "non-git explicit cwd must bind task worktree to dir_b rather than falling through to registry_root or daemon_root"
        );
        assert_eq!(
            claimed_payload.metadata["task"]["branch"],
            Value::Null,
            "non-git explicit cwd without branch arg must not inherit daemon_root branch"
        );

        // When the proxy runs from a subdirectory of a Git worktree, `cwd` is
        // that subdirectory, while `active_session.json` must still be written
        // at the worktree root so plugin readers at the project root find it.
        let wt_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt_dir.path().join(".git")).unwrap();
        let nested_cwd = wt_dir.path().join("crates").join("sub");
        std::fs::create_dir_all(&nested_cwd).unwrap();
        let id2 = create_task(&store, "MyProj", "scope nested cwd", 1).await;
        handle_task_claim(
            json!({
                "namespace": "MyProj",
                "id": id2,
                "assignee": "agent",
                "session_id": "ses_nested",
                "cwd": nested_cwd.to_string_lossy().to_string(),
            }),
            store.clone(),
        )
        .await;
        let root_session = wt_dir
            .path()
            .join(".NeuroStrata")
            .join("active_session.json");
        let nested_session = nested_cwd.join(".NeuroStrata").join("active_session.json");
        assert!(
            root_session.exists(),
            "active_session.json must be written at the enclosing worktree root"
        );
        assert!(
            !nested_session.exists(),
            "active_session.json must not be stranded in a nested subdirectory"
        );
    }

    #[test]
    fn zero_action_exempt_shell_payload_allows_git_push_and_rejects_mutations() {
        assert!(crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git push -u origin fix/task-claim-push-scope"
        ));
        assert!(crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "CARGO_TARGET_DIR=/tmp/target git push -u origin main && git status -s"
        ));
        assert!(crate::server::is_zero_action_exempt_shell_payload(
            "shell",
            "git pull --rebase && git push && git status"
        ));
        assert!(crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "neurostrata-mcp task gate NeuroStrata --strict"
        ));
        for force_push in [
            "git push --force",
            "git push -f",
            "git push -fv origin main",
            "git push -vf",
            "git push -uf",
            "git push origin +main",
            "git push --mirror",
            "git push --delete origin main",
            "git push --prune",
            "git push --force-with-lease=origin",
        ] {
            assert!(
                !crate::server::is_zero_action_exempt_shell_payload("bash", force_push),
                "force-push must still require an active task: {force_push}"
            );
        }
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git commit -m 'mutate history'"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git push && rm -rf src"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git push > /tmp/pwn"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git diff --output=src/file.rs"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git show --output src/file.rs"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git log --output=src/file.rs"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "bash",
            "git -c core.sshCommand=evil push"
        ));
        assert!(!crate::server::is_zero_action_exempt_shell_payload(
            "edit", "git push"
        ));
    }

    #[tokio::test]
    async fn holderless_open_to_in_progress_update_stays_unattributed_and_fails_closed() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        // No session_id: the transition has no holder, so it is not a claim.
        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "in_progress" }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(reply.contains("\"status\": \"in_progress\""), "{}", reply);

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        let task_meta = &payload.metadata["task"];
        assert!(
            task_meta.get("session_id").is_none(),
            "holder-less transition must not stamp a session: {}",
            task_meta
        );
        assert!(
            task_meta.get("branch").is_none() && task_meta.get("worktree").is_none(),
            "holder-less transition must not stamp scope: {}",
            task_meta
        );
        // The strict gate cannot prove a holder-less claim independent, so it
        // keeps blocking no matter which scope is pushed.
        let independent = gate::GateScope {
            branch: Some("feature/independent".to_string()),
            worktree: Some("/repo/independent".to_string()),
        };
        let report = gate::run_scoped(&store, "MyProj", Some(&independent))
            .await
            .unwrap();
        assert!(report.violations.iter().any(|v| v.kind == "in_progress"));
    }

    #[tokio::test]
    async fn task_update_open_to_in_progress_never_clobbers_another_holders_attribution() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        // Seed an Open row that still carries a prior holder's attribution.
        let (vector, mut payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        payload.metadata["task"]["assignee"] = json!("agent-other");
        payload.metadata["task"]["session_id"] = json!("ses_other");
        payload.metadata["task"]["branch"] = json!("other/branch");
        payload.metadata["task"]["worktree"] = json!("/repo/other");
        store.upsert("MyProj", &id, vector, payload).await.unwrap();

        let reply = handle_task_update(
            json!({
                "namespace": "MyProj",
                "id": id,
                "status": "in_progress",
                "assignee": "agent-new",
                "session_id": "ses_new"
            }),
            emb(),
            store.clone(),
        )
        .await;
        assert!(!reply.starts_with("ERROR"), "{}", reply);

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        let task_meta = &payload.metadata["task"];
        assert_eq!(task_meta["branch"], json!("other/branch"));
        assert_eq!(task_meta["worktree"], json!("/repo/other"));
        assert_eq!(task_meta["session_id"], json!("ses_other"));
    }

    #[test]
    fn task_update_claim_stamps_scope_for_the_strict_gate() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let store = store_with_namespace("MyProj").await;
            let id = create_task(&store, "MyProj", "the work", 2).await;
            let expected = gate::GateScope::capture(&crate::daemon::resolve_project_root());

            let reply = handle_task_update(
                json!({
                    "namespace": "MyProj",
                    "id": id,
                    "status": "in_progress",
                    "assignee": "agent-a",
                    "session_id": "ses_a"
                }),
                emb(),
                store.clone(),
            )
            .await;
            assert!(reply.contains("\"status\": \"in_progress\""), "{}", reply);

            let same_scope = gate::run_scoped(&store, "MyProj", Some(&expected))
                .await
                .unwrap();
            assert!(same_scope
                .violations
                .iter()
                .any(|v| v.kind == "in_progress"));
            let independent = gate::GateScope {
                branch: Some("feature/independent".to_string()),
                worktree: Some("/repo/independent".to_string()),
            };
            let other_scope = gate::run_scoped(&store, "MyProj", Some(&independent))
                .await
                .unwrap();
            assert!(!other_scope
                .violations
                .iter()
                .any(|v| v.kind == "in_progress"));
        });
    }

    #[tokio::test]
    async fn claim_is_refused_while_another_live_session_holds_the_task() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        let first = handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" }),
            store.clone(),
        )
        .await;
        assert!(first.contains("\"status\": \"in_progress\""), "{}", first);

        let second = handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-b", "session_id": "ses_b" }),
            store.clone(),
        )
        .await;
        assert!(second.starts_with("ERROR: task"), "{}", second);
        assert!(second.contains("claimed by agent-a"), "{}", second);
        assert!(second.contains("duplicate work"), "{}", second);

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(payload.metadata["task"]["assignee"], json!("agent-a"));
        assert_eq!(payload.metadata["task"]["session_id"], json!("ses_a"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_claims_exactly_one_wins() {
        let real = store_with_namespace("MyProj").await;
        let id = create_task(&real, "MyProj", "the work", 2).await;
        // SlowGetStore widens the load -> decide -> upsert window so the two
        // claims genuinely overlap; without a per-task claim lock both win.
        let store: Arc<dyn VectorStore> = Arc::new(SlowGetStore {
            inner: real.clone(),
        });
        let a = tokio::spawn(handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" }),
            store.clone(),
        ));
        let b = tokio::spawn(handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-b", "session_id": "ses_b" }),
            store.clone(),
        ));
        let replies = [a.await.unwrap(), b.await.unwrap()];
        let wins = replies
            .iter()
            .filter(|r| r.contains("\"claimed\": true"))
            .count();
        assert_eq!(
            wins, 1,
            "exactly one concurrent claim may win: {:?}",
            replies
        );
        let loser = replies
            .iter()
            .find(|r| !r.contains("\"claimed\": true"))
            .expect("the loser replied");
        assert!(loser.starts_with("ERROR: task"), "{}", loser);
        assert!(
            loser.contains("claimed by agent-"),
            "the loser is told who holds the task: {}",
            loser
        );

        let (_, payload) = real.get("MyProj", &id).await.unwrap().unwrap();
        let holder = payload.metadata["task"]["session_id"].as_str().unwrap();
        assert!(
            holder == "ses_a" || holder == "ses_b",
            "the persisted holder is the winner: {}",
            holder
        );
        assert_eq!(
            payload.metadata["task"]["history"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "only the winning claim is persisted"
        );
    }

    #[tokio::test]
    async fn empty_session_claim_is_refused() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;

        for args in [
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a" }),
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "" }),
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "   " }),
        ] {
            let reply = handle_task_claim(args, store.clone()).await;
            assert!(reply.starts_with("ERROR:"), "{}", reply);
            assert!(
                reply.contains("session_id"),
                "the refusal names the requirement: {}",
                reply
            );
        }

        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::Open,
            "a refused claim changes nothing"
        );
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert!(
            payload.metadata["task"].get("session_id").is_none(),
            "no empty session is persisted: {:?}",
            payload.metadata["task"]
        );
        assert!(
            payload.metadata["task"]["history"]
                .as_array()
                .unwrap()
                .is_empty(),
            "no transition is persisted"
        );
    }

    #[tokio::test]
    async fn same_session_reclaim_is_idempotent() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        let args = json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" });

        handle_task_claim(args.clone(), store.clone()).await;
        let again = handle_task_claim(args, store.clone()).await;
        assert!(
            again.contains("already claimed by this session"),
            "{}",
            again
        );

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(
            payload.metadata["task"]["history"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn same_session_reclaim_backfills_missing_legacy_attribution_and_preserves_existing() {
        let _env_guard = crate::daemon::project_root_env_test_lock();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let store = store_with_namespace("MyProj").await;
            let id = create_task(&store, "MyProj", "the work", 2).await;
            let args = json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" });

            handle_task_claim(args.clone(), store.clone()).await;

            // Simulate a legacy row: attribution dropped, claim intact.
            let expected = gate::GateScope::capture(&crate::daemon::resolve_project_root());
            let (vector, mut payload) = store.get("MyProj", &id).await.unwrap().unwrap();
            payload.metadata["task"]
                .as_object_mut()
                .unwrap()
                .remove("branch");
            payload.metadata["task"]
                .as_object_mut()
                .unwrap()
                .remove("worktree");
            store.upsert("MyProj", &id, vector, payload).await.unwrap();

            let backfill = handle_task_claim(args.clone(), store.clone()).await;
            assert!(
                backfill.contains("already claimed by this session"),
                "{}",
                backfill
            );

            let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
            assert_eq!(
                payload.metadata["task"]["branch"],
                json!(expected.branch),
                "legacy branch attribution must be backfilled"
            );
            assert_eq!(
                payload.metadata["task"]["worktree"],
                json!(expected.worktree),
                "legacy worktree attribution must be backfilled"
            );

            // Idempotency: an already-attributed row keeps its exact attribution.
            let (vector, mut payload) = store.get("MyProj", &id).await.unwrap().unwrap();
            payload.metadata["task"]["branch"] = json!("legacy/branch");
            payload.metadata["task"]["worktree"] = json!("/legacy/worktree");
            store.upsert("MyProj", &id, vector, payload).await.unwrap();

            let again = handle_task_claim(args, store.clone()).await;
            assert!(
                again.contains("already claimed by this session"),
                "{}",
                again
            );

            let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
            assert_eq!(payload.metadata["task"]["branch"], json!("legacy/branch"));
            assert_eq!(
                payload.metadata["task"]["worktree"],
                json!("/legacy/worktree"),
                "existing attribution must not be overwritten"
            );
        });
    }

    #[tokio::test]
    async fn a_stale_holder_is_released_then_claimed() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" }),
            store.clone(),
        )
        .await;

        // Age the claim past the staleness window.
        let (vector, mut payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        payload.metadata["task"]["updated_at"] = json!("2020-01-01T00:00:00Z");
        store.upsert("MyProj", &id, vector, payload).await.unwrap();

        let takeover = handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-b", "session_id": "ses_b" }),
            store.clone(),
        )
        .await;
        assert!(
            takeover.contains("\"status\": \"in_progress\""),
            "{}",
            takeover
        );

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(payload.metadata["task"]["assignee"], json!("agent-b"));
        assert_eq!(payload.metadata["task"]["session_id"], json!("ses_b"));
        let history = payload.metadata["task"]["history"].as_array().unwrap();
        assert_eq!(
            history.len(),
            3,
            "original claim, release, takeover claim: {:?}",
            history
        );
        assert_eq!(history[0]["to"], "in_progress", "the original claim");
        assert_eq!(history[1]["to"], "open", "the stale holder is released");
        assert_eq!(history[2]["to"], "in_progress", "then taken over");
        assert!(
            history[1]["note"].as_str().unwrap().contains("stale"),
            "the release says why: {:?}",
            history[1]["note"]
        );
    }

    #[tokio::test]
    async fn claim_after_completion_still_refused() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        // Hand-edit the status: the machine still refuses done -> in_progress.
        let (vector, mut payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        payload.metadata["task"]["status"] = json!("done");
        store.upsert("MyProj", &id, vector, payload).await.unwrap();

        let reply = handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" }),
            store.clone(),
        )
        .await;
        assert!(reply.contains("done -> in_progress"), "{}", reply);
        assert!(reply.contains("legal transitions are: open"), "{}", reply);
    }

    // ── Q2: the extraction gate, three ordered paths ────────────────────

    async fn claim(store: &Arc<dyn VectorStore>, id: &str) {
        let reply = handle_task_claim(
            json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" }),
            store.clone(),
        )
        .await;
        assert!(reply.contains("in_progress"), "claim failed: {}", reply);
    }

    #[tokio::test]
    async fn task_complete_without_any_extraction_is_blocked_with_the_design_message() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let err = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("Lock 2 must refuse a completion with nothing extracted");

        let expected = format!(
            "BLOCKED (Lock 2): task {} cannot complete without an extracted memory. Either call neurostrata_add_memory with metadata extracted_from: [\"{}\"], or retry neurostrata_task_complete with `memory.content` (what did you learn? what rule does this task prove?) or `link_memory_id`.",
            id, id
        );
        assert_eq!(
            err, expected,
            "the -32603 body must match section 3.2 verbatim"
        );
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::InProgress,
            "a blocked completion changes nothing"
        );
    }

    #[tokio::test]
    async fn task_complete_blocked_by_undisposed_high_confidence_obligation() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let intake = store.as_intake_store().expect("intake store");
        let prov = serde_json::json!({
            "turn_id": "t1", "role": "user",
            "evidence_quote": "fix the login bug", "confidence": 0.9
        });
        intake
            .create_receipt(crate::task::intake::CreateReceiptRequest {
                namespace: "MyProj".to_string(),
                source_kind: crate::task::intake::SourceKind::Tool,
                source_revision: "r1".to_string(),
                source_digest: "d1".to_string(),
                text: "obligation".to_string(),
                title: Some("Fix login bug".to_string()),
                description: None,
                candidate_kind: None,
                kind: Some("task_obligation".to_string()),
                category: None,
                payload: None,
                confidence: Some(0.9),
                source_span: None,
                provenance: Some(
                    serde_json::from_value::<crate::task::intake::ProvenanceSpan>(prov).unwrap(),
                ),
                extractor_fingerprint: None,
            })
            .await
            .expect("create obligation receipt");

        let err = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("undisposed obligation must block completion");

        assert!(
            err.starts_with("BLOCKED (Lock 2 — Task Obligation)"),
            "wrong message: {err}"
        );
        assert!(
            err.contains("Fix login bug"),
            "names obligation title: {err}"
        );
        assert!(
            err.contains("neurostrata_task_candidates action: \"resolve\""),
            "names resolve action: {err}"
        );
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::InProgress,
            "a blocked completion changes nothing"
        );
    }

    #[tokio::test]
    async fn task_complete_not_blocked_once_obligation_is_disposed() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let intake = store.as_intake_store().expect("intake store");
        let prov = serde_json::json!({
            "turn_id": "t1", "role": "user",
            "evidence_quote": "fix it", "confidence": 0.9
        });
        let create = intake
            .create_receipt(crate::task::intake::CreateReceiptRequest {
                namespace: "MyProj".to_string(),
                source_kind: crate::task::intake::SourceKind::Tool,
                source_revision: "r1".to_string(),
                source_digest: "d1".to_string(),
                text: "obligation".to_string(),
                title: Some("Fix it".to_string()),
                description: None,
                candidate_kind: None,
                kind: Some("task_obligation".to_string()),
                category: None,
                payload: None,
                confidence: Some(0.9),
                source_span: None,
                provenance: Some(
                    serde_json::from_value::<crate::task::intake::ProvenanceSpan>(prov).unwrap(),
                ),
                extractor_fingerprint: None,
            })
            .await
            .expect("create obligation receipt");
        let cand_id = create.candidates[0].id.clone();

        intake
            .resolve_candidate(crate::task::intake::ResolveCandidateRequest {
                namespace: "MyProj".to_string(),
                candidate_id: cand_id,
                disposition: crate::task::intake::Disposition::Dismissed,
                task_id: None,
                cas_version: 1,
            })
            .await
            .expect("dismiss obligation");

        // No obligation remains, so completion is refused only by Lock 2
        // extraction (no memory extracted), not by the obligation gate.
        let err = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("completion still blocked by Lock 2 extraction");

        assert!(
            !err.starts_with("BLOCKED (Lock 2 — Task Obligation)"),
            "disposed obligation must not block: {err}"
        );
    }

    #[tokio::test]
    async fn path_1_a_pre_existing_extraction_edge_lets_the_task_complete() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        // A memory written earlier, declaring the edge in its metadata.
        let mem_id = uuid::Uuid::new_v4().to_string();
        let vector = StubEmbedder.embed("the lesson").await.unwrap();
        store
            .upsert(
                "MyProj",
                &mem_id,
                vector,
                MemoryPayload {
                    content: "the lesson".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "extracted_from": [id.as_str()] }),
                },
            )
            .await
            .unwrap();

        let reply = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id, "reason": "lesson extracted" }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("the edge already exists");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["task"]["status"], json!("done"));
        assert_eq!(parsed["transition"]["from"], json!("in_progress"));
        assert_eq!(parsed["transition"]["to"], json!("done"));
        let extracted = parsed["extracted_memory_ids"].as_array().unwrap();
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].as_str(), Some(mem_id.as_str()));
        assert_eq!(status_of(&store, "MyProj", &id).await, Status::Done);
    }

    #[tokio::test]
    async fn path_2_link_memory_id_re_upserts_without_reembedding() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let mem_id = uuid::Uuid::new_v4().to_string();
        let vector = StubEmbedder.embed("an existing memory").await.unwrap();
        store
            .upsert(
                "MyProj",
                &mem_id,
                vector.clone(),
                MemoryPayload {
                    content: "an existing memory".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "access_count": 3 }),
                },
            )
            .await
            .unwrap();

        // FailEmbedder would fail the test if anything re-embedded the task
        // title or the linked memory on this path.
        let reply = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id, "link_memory_id": mem_id }),
            Arc::new(FailEmbedder),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("linking must not embed anything");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["task"]["status"], json!("done"));
        let extracted = parsed["extracted_memory_ids"].as_array().unwrap();
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].as_str(), Some(mem_id.as_str()));

        let (kept_vector, kept) = store.get("MyProj", &mem_id).await.unwrap().unwrap();
        assert_eq!(kept_vector, vector, "the linked memory keeps its vector");
        assert_eq!(kept.content, "an existing memory");
        assert_eq!(kept.metadata["extracted_from"], json!([id.as_str()]));
        assert_eq!(
            kept.metadata["access_count"],
            json!(3),
            "everything else is untouched"
        );
    }

    #[tokio::test]
    async fn path_3_inline_memory_goes_through_the_add_memory_pipeline() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let before = store.list("MyProj", None, None).await.unwrap().len();
        let reply = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "reason": "shipped",
                "memory": { "content": "tasks are memories; done needs an extraction" }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("the inline extraction writes the edge");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["task"]["status"], json!("done"));
        let extracted = parsed["extracted_memory_ids"].as_array().unwrap();
        assert_eq!(extracted.len(), 1);

        let after = store.list("MyProj", None, None).await.unwrap();
        assert_eq!(after.len(), before + 1, "one memory was written");
        let written = after
            .iter()
            .find(|r| Some(r.id.as_str()) == extracted[0].as_str())
            .expect("the new memory");
        assert_eq!(written.payload.memory_type, "fact", "the schema default");
        assert_eq!(
            written.payload.metadata["extracted_from"],
            json!([id.as_str()])
        );
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::Done,
            "the transition happened after the extraction landed"
        );
        // The inline write also carries the pipeline's own stamps.
        assert!(
            written.payload.metadata.get("valid_from").is_some(),
            "valid_from comes from the pipeline"
        );
        assert!(written.payload.metadata.get("access_count").is_some());
    }

    /// Pinned contract: the inline `memory` handoff persists `memory.metadata`
    /// exactly as add_memory would -- the caller's keys land verbatim (on top
    /// of the pipeline stamps), with the EXTRACTED_FROM edge guaranteed. This
    /// is the Source-necessity argument: a rule the funnel writes must carry
    /// the source its caller declared, or the gate the funnel feeds rejects
    /// it one call later.
    #[tokio::test]
    async fn path_3_inline_memory_persists_caller_metadata_verbatim() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let requested_source = json!({
            "kind": "task",
            "ref": id.as_str(),
            "captured_at": "2026-10-11"
        });
        let reply = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "memory": {
                    "content": "the lesson the task taught",
                    "memory_type": "rule",
                    "metadata": {
                        "extracted_from": [id.as_str()],
                        "source": requested_source.clone(),
                    }
                }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("the inline extraction writes the edge");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        let lesson_id = parsed["extracted_memory_ids"][0]
            .as_str()
            .unwrap()
            .to_string();
        let (_, lesson) = store.get("MyProj", &lesson_id).await.unwrap().unwrap();
        assert_eq!(lesson.memory_type, "rule", "the caller's memory_type lands");
        assert_eq!(
            lesson.metadata["source"], requested_source,
            "the caller's metadata.source must persist exactly as add_memory would persist it"
        );
        assert_eq!(
            lesson.metadata["extracted_from"],
            json!([id.as_str()]),
            "the EXTRACTED_FROM edge is guaranteed and not re-created"
        );
    }

    /// Pinned contract: a wrong-typed `extracted_from` under the caller's
    /// inline metadata is REJECTED, mirroring add_memory's wording -- the
    /// call errors naming the malformed key, nothing is written (a refusal
    /// publishes nothing), and the task stays claimed rather than completing
    /// or falling through to the generic Lock-2 block after a write. Locks
    /// the `Some(_)` arm of the inline extracted_from match: without it, the
    /// string "x" would be persisted verbatim and the same call would fail
    /// only after the row landed, with the wrong message.
    #[tokio::test]
    async fn path_3_inline_memory_rejects_malformed_extracted_from() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let before = store.list("MyProj", None, None).await.unwrap().len();
        let err = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "reason": "shipped",
                "memory": {
                    "content": "a lesson whose edge is malformed",
                    "metadata": { "extracted_from": "x" }
                }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("a non-array extracted_from must be rejected");

        assert!(
            err.contains("malformed 'extracted_from'")
                && err.contains("must be an array of task ids"),
            "the rejection must name the malformed key, not the generic Lock-2 block: {err}"
        );
        let after = store.list("MyProj", None, None).await.unwrap();
        assert_eq!(after.len(), before, "the intended memory was NOT created");
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::InProgress,
            "the task did NOT complete to done"
        );
    }

    // ── Task 3: the completion's write-path registry check ─────────────

    /// Seed the registry cache directly, so the check runs against a cache
    /// that is already present and (deliberately) missing the slug under test.
    async fn seed_registry(store: &Arc<dyn VectorStore>, namespace: &str, domains: &[&str]) {
        let domains: Vec<String> = domains.iter().map(|d| d.to_string()).collect();
        let content = crate::registry::registry_content(namespace);
        let vector = StubEmbedder.embed(&content).await.unwrap();
        store
            .upsert(
                namespace,
                &crate::registry::registry_row_id(namespace),
                vector,
                MemoryPayload {
                    content,
                    user_id: "system".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: crate::registry::registry_metadata(&domains),
                },
            )
            .await
            .expect("seed the registry row");
    }

    async fn registry_of(store: &Arc<dyn VectorStore>, namespace: &str) -> Vec<String> {
        store
            .get(namespace, &crate::registry::registry_row_id(namespace))
            .await
            .expect("read the registry row")
            .map(|(_, p)| crate::registry::registry_domains(&p.metadata))
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_completion_with_an_unregistered_domain_is_rejected() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        seed_registry(&store, "MyProj", &["auth"]).await;

        let err = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "memory": {
                    "content": "the lesson the task taught",
                    "metadata": { "domain": "storage" }
                }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("an unregistered domain must be rejected");

        assert!(err.contains("ERROR [DOMAIN]"), "{}", err);
        assert!(
            err.contains(r#"Registered domains: ["auth"]"#),
            "the rejection names the registered list: {}",
            err
        );
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::InProgress,
            "a rejected completion does not reach done"
        );
        assert_eq!(
            registry_of(&store, "MyProj").await,
            vec!["auth".to_string()],
            "a rejection writes nothing to the cache"
        );
    }

    #[tokio::test]
    async fn a_completion_with_register_domain_true_registers_the_domain() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        seed_registry(&store, "MyProj", &["auth"]).await;

        let reply = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "register_domain": true,
                "memory": {
                    "content": "the lesson the task taught",
                    "metadata": { "domain": "storage" }
                }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("register_domain accepts the new slug");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["task"]["status"], json!("done"));
        assert_eq!(
            registry_of(&store, "MyProj").await,
            vec!["auth".to_string(), "storage".to_string()]
        );
    }

    /// Regression lock: without `memory.metadata`, the inline write today is
    /// exactly what it stays -- extracted_from only, plus the pipeline stamps,
    /// no invention.
    #[tokio::test]
    async fn path_3_inline_memory_without_metadata_behaves_as_today() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let reply = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "memory": { "content": "metadata-less lesson" }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("the inline extraction writes the edge");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        let lesson_id = parsed["extracted_memory_ids"][0]
            .as_str()
            .unwrap()
            .to_string();
        let (_, lesson) = store.get("MyProj", &lesson_id).await.unwrap().unwrap();
        assert_eq!(lesson.memory_type, "fact");
        assert_eq!(lesson.metadata["extracted_from"], json!([id.as_str()]));
        assert!(lesson.metadata.get("source").is_none());
        assert!(
            lesson.metadata.get("valid_from").is_some(),
            "pipeline stamp"
        );
        assert!(
            lesson.metadata.get("access_count").is_some(),
            "pipeline stamp"
        );
    }

    /// Emit site 3: closing a task with an inline lesson is "the task gave way
    /// to a lesson", so the pulse names both. Asserts the ends against the
    /// store rather than against the reply, because a `Superseded` whose
    /// `new_id` is the task and whose `old_id` is the lesson would still parse
    /// -- it would just send every subscriber down the wrong chain.
    #[tokio::test]
    async fn an_inline_extraction_emits_superseded_from_the_task_to_the_lesson() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        let (bus, seen) = recording_bus();

        let reply = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "reason": "shipped",
                "memory": { "content": "tasks are memories; done needs an extraction" }
            }),
            emb(),
            store.clone(),
            None,
            bus,
        )
        .await
        .expect("the inline extraction writes the edge");
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        let lesson_id = parsed["extracted_memory_ids"][0]
            .as_str()
            .expect("the extraction's id")
            .to_string();

        let pulses = pulses_seen(&seen, 1).await;
        match &pulses[0] {
            ThalamicPulse::Superseded {
                old_id,
                new_id,
                namespace,
            } => {
                assert_eq!(old_id, &id, "the task is the row that gave way");
                assert_eq!(new_id, &lesson_id, "the lesson is the row that replaced it");
                assert_eq!(namespace, "MyProj");
                // The lesson really is a stored row pointing back at the task.
                let (_, lesson) = store
                    .get("MyProj", new_id)
                    .await
                    .unwrap()
                    .expect("the lesson is stored");
                assert_eq!(lesson.metadata["extracted_from"], json!([old_id.as_str()]));
            }
            other => panic!("expected Superseded, got {:?}", other),
        }
    }

    /// Wraps a real store and delays every task-row `get` briefly, widening
    /// the claim handler's load -> decide -> upsert window so two spawned
    /// claims overlap deterministically instead of racing by scheduler luck.
    struct SlowGetStore {
        inner: Arc<dyn VectorStore>,
    }

    #[async_trait::async_trait]
    impl VectorStore for SlowGetStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }
        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            self.inner.upsert(namespace, id, vector, payload).await
        }
        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }
        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }
        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }
        async fn relink_edges(&self, namespace: &str, source_key: &str) -> anyhow::Result<usize> {
            self.inner.relink_edges(namespace, source_key).await
        }
        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<SearchResult>> {
            self.inner.list(namespace, user_id, memory_type).await
        }
        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            let found = self.inner.get(namespace, id).await?;
            if let Some((_, payload)) = &found {
                if payload.metadata.get("task").is_some() {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
            }
            Ok(found)
        }
        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }
        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }
        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }
        async fn increment_access_count(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }
        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }
        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl crate::traits::AsAny for SlowGetStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Wraps a real store and hides every extraction-declaring row from
    /// `list`. `upsert` still lands, so the inline lesson really is written --
    /// the gate simply cannot see the edge it declared, which is the one
    /// thing that makes `extraction_exists` disagree with a write that
    /// succeeded.
    struct ListHidingStore {
        inner: Arc<dyn VectorStore>,
    }

    #[async_trait::async_trait]
    impl VectorStore for ListHidingStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }
        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            self.inner.upsert(namespace, id, vector, payload).await
        }
        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }
        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }
        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }
        async fn relink_edges(&self, namespace: &str, source_key: &str) -> anyhow::Result<usize> {
            self.inner.relink_edges(namespace, source_key).await
        }
        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<SearchResult>> {
            let rows = self.inner.list(namespace, user_id, memory_type).await?;
            Ok(rows
                .into_iter()
                .filter(|r| r.payload.metadata.get("extracted_from").is_none())
                .collect())
        }
        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            self.inner.get(namespace, id).await
        }
        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }
        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }
        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }
        async fn increment_access_count(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }
        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }
        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl crate::traits::AsAny for ListHidingStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// The ordering invariant the emit site carries: a `Superseded` published
    /// on the inline path claims the task gave way to a lesson. If the Lock 2
    /// gate then refuses -- the edge the write declared turned out not to be
    /// there -- the task never reached `done`, and subscribers that tracked the
    /// pulse would be tracking a move that never landed. The refusal path is
    /// the only one that must publish nothing at all.
    ///
    /// Mutation-proof: restoring the emit above the gate makes the count 1.
    #[tokio::test]
    async fn a_lock_2_refusal_after_the_inline_write_emits_nothing() {
        let real = store_with_namespace("MyProj").await;
        let id = create_task(&real, "MyProj", "the work", 2).await;
        claim(&real, &id).await;
        let store: Arc<dyn VectorStore> = Arc::new(ListHidingStore {
            inner: real.clone(),
        });
        let (bus, seen) = recording_bus();

        let err = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "reason": "shipped",
                "memory": { "content": "tasks are memories; done needs an extraction" }
            }),
            emb(),
            store.clone(),
            None,
            bus.clone(),
        )
        .await
        .expect_err("Lock 2 must refuse when the declared edge is not visible");

        assert!(
            err.starts_with("BLOCKED (Lock 2)"),
            "the refusal is the gate's own message: {}",
            err
        );
        assert_eq!(
            status_of(&real, "MyProj", &id).await,
            Status::InProgress,
            "a refused completion changes nothing"
        );
        // The write itself did land -- this is the case the pulse would have
        // lied about, not a path that failed earlier.
        let lesson_count = real
            .list("MyProj", None, None)
            .await
            .unwrap()
            .iter()
            .filter(|r| r.payload.metadata.get("extracted_from").is_some())
            .count();
        assert_eq!(lesson_count, 1, "the inline lesson was written");

        assert_eq!(
            bus.metrics().events_emitted,
            0,
            "a refused completion publishes no pulse: {:?}",
            bus.metrics()
        );
        // The dispatcher is asynchronous, so a pulse in flight would show up
        // late. Give it the window this path must stay silent through.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            seen.lock().expect("recorder lock").is_empty(),
            "no subscriber is told about a move that never landed"
        );
    }

    /// Linking an already-written memory adds the edge but creates no row, so
    /// there is no "gave way to" to publish. The negative half of the
    /// contract: an emit here would announce a move for a memory that was
    /// already there.
    #[tokio::test]
    async fn linking_an_existing_memory_emits_nothing() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        let mem_id = uuid::Uuid::new_v4().to_string();
        let vector = StubEmbedder.embed("an existing memory").await.unwrap();
        store
            .upsert(
                "MyProj",
                &mem_id,
                vector,
                MemoryPayload {
                    content: "an existing memory".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "access_count": 3 }),
                },
            )
            .await
            .unwrap();
        let (bus, seen) = recording_bus();

        handle_task_complete(
            json!({ "namespace": "MyProj", "id": id, "link_memory_id": mem_id }),
            emb(),
            store.clone(),
            None,
            bus,
        )
        .await
        .expect("linking must not embed anything");

        // The dispatcher is asynchronous, so a pulse in flight would show up
        // late. Give it the window this path must stay silent through.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            seen.lock().expect("recorder lock").is_empty(),
            "binding an existing memory publishes no pulse"
        );
    }

    #[tokio::test]
    async fn a_secret_in_the_inline_extraction_is_refused_before_any_write() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        let before = store.list("MyProj", None, None).await.unwrap().len();

        let err = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "memory": { "content": "use ghp_aaaabbbbccccdddd to push" }
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("a secret must not be stored as an extraction");
        assert!(err.contains("ERROR [SECURITY]"), "{}", err);
        assert_eq!(
            store.list("MyProj", None, None).await.unwrap().len(),
            before
        );
        assert_eq!(status_of(&store, "MyProj", &id).await, Status::InProgress);
    }

    #[tokio::test]
    async fn completing_an_already_done_task_is_refused_clearly() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        handle_task_complete(
            json!({ "namespace": "MyProj", "id": id, "memory": { "content": "lesson" } }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .unwrap();

        let err = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id, "memory": { "content": "again" } }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect_err("done -> done is not in the table");
        assert!(err.contains("already done"), "{}", err);
        assert!(err.contains("Reopen it first"), "{}", err);
    }

    // ── task_list ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn task_list_filters_by_ready_include_done_and_assignee() {
        let store = store_with_namespace("MyProj").await;
        let done_id = create_task(&store, "MyProj", "finished work", 4).await;
        // Finish it the only legal way: extraction first, then done.
        let completion = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": done_id,
                "reason": "shipped",
                "memory": { "content": "what the finished work proved" },
            }),
            emb(),
            store.clone(),
            None,
            quiet_bus(),
        )
        .await
        .expect("extraction-backed completion");
        assert!(
            completion.contains("\"status\": \"done\""),
            "{}",
            completion
        );
        let open_id = create_task(&store, "MyProj", "actionable work", 1).await;
        let blocked_id = create_task(&store, "MyProj", "waiting work", 2).await;
        // The blocked one waits on the open one.
        handle_task_update(
            json!({ "namespace": "MyProj", "id": blocked_id, "blocked_by": [open_id.as_str()] }),
            emb(),
            store.clone(),
        )
        .await;

        let reply = handle_task_list(json!({ "namespace": "MyProj" }), store.clone()).await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        let ids: Vec<&str> = parsed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&open_id.as_str()));
        assert!(ids.contains(&blocked_id.as_str()));
        assert!(
            !ids.contains(&done_id.as_str()),
            "done is hidden by default"
        );
        // Priority order: actionable (P1) before waiting (P2).
        assert_eq!(ids[0], open_id);

        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "ready": true }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        let ids: Vec<&str> = parsed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![open_id.as_str()],
            "ready = open with no live blockers"
        );

        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "include_done": true }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(3));

        claim(&store, &open_id).await;
        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "assignee": "agent-a" }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(1));
        assert_eq!(parsed["tasks"][0]["id"], json!(open_id));

        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "status": "done" }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(1), "an explicit status filter wins");
        assert_eq!(parsed["tasks"][0]["id"], json!(done_id));
    }

    /// Writes a task row with hand-set top-level metadata, so a test can
    /// plant a retired row the task tools would never create.
    async fn upsert_task_row(
        store: &Arc<dyn VectorStore>,
        namespace: &str,
        id: &str,
        content: &str,
        task_meta: Value,
        extra: Value,
    ) {
        let vector = emb().embed(content).await.unwrap();
        let mut metadata = json!({ "task": task_meta });
        if let Value::Object(map) = extra {
            for (key, value) in map {
                metadata[key] = value;
            }
        }
        store
            .upsert(
                namespace,
                id,
                vector,
                MemoryPayload {
                    content: content.to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata,
                },
            )
            .await
            .expect("upsert task row");
    }

    /// A retired task row is history: it is not ready work, and it does not
    /// hold its dependents hostage -- unless a live successor still carries
    /// the work.
    #[tokio::test]
    async fn retired_rows_are_not_ready_work_and_do_not_hold_dependents() {
        let store = store_with_namespace("MyProj").await;
        let now = chrono::Utc::now().timestamp();

        // A retired blocker with no live successor: history, so its
        // dependent is free to run.
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-retired",
            "retired work",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now - 1 }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-dependent",
            "work that waited for the retired one",
            json!({ "status": "open", "priority": 1, "blocked_by": ["MyProj-retired"] }),
            json!({}),
        )
        .await;

        // A retired blocker whose successor still carries the work: the
        // dependent still waits.
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-retired-with-successor",
            "retired work with a live successor",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now - 1, "superseded_by": "MyProj-live-work" }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-live-work",
            "the work that carries on",
            json!({ "status": "in_progress", "assignee": "agent-a" }),
            json!({}),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-dependent-on-successor",
            "work that still waits for the live successor",
            json!({
                "status": "open",
                "priority": 1,
                "blocked_by": ["MyProj-retired-with-successor"]
            }),
            json!({}),
        )
        .await;

        // A live blocker, and one whose expiry has not passed, both hold
        // their dependents.
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-live-blocker",
            "unfinished work",
            json!({ "status": "open", "priority": 1 }),
            json!({}),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-dependent-on-live",
            "work that waits for the live blocker",
            json!({ "status": "open", "priority": 1, "blocked_by": ["MyProj-live-blocker"] }),
            json!({}),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-future-blocker",
            "work that expires later",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now + 3600 }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-dependent-on-future",
            "work that waits for the future blocker",
            json!({ "status": "open", "priority": 1, "blocked_by": ["MyProj-future-blocker"] }),
            json!({}),
        )
        .await;

        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "ready": true }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        let ids: Vec<&str> = parsed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![
                "MyProj-dependent",
                "MyProj-future-blocker",
                "MyProj-live-blocker"
            ],
            "a retired row is not ready work, and only a live successor keeps a dependent waiting"
        );

        // The unfiltered listing does not offer history as work; the opt-in
        // shows it, labelled as history.
        let reply = handle_task_list(json!({ "namespace": "MyProj" }), store.clone()).await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(7));
        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "include_retired": true }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(9));
    }

    /// Retired rows are history: the default listing does not offer them as
    /// work, and the opt-in shows them labelled as history.
    #[tokio::test]
    async fn task_list_hides_retired_rows_by_default_and_shows_them_on_request() {
        let store = store_with_namespace("MyProj").await;
        let now = chrono::Utc::now().timestamp();
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-live",
            "live work",
            json!({ "status": "open", "priority": 1 }),
            json!({}),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-retired",
            "retired work",
            json!({ "status": "in_progress", "assignee": "ghost" }),
            json!({ "valid_to": now - 1 }),
        )
        .await;

        let reply = handle_task_list(json!({ "namespace": "MyProj" }), store.clone()).await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(1), "history is not offered as work");

        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "include_retired": true }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["count"], json!(2));
        let retired = parsed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "MyProj-retired")
            .expect("the retired row is listed on request");
        assert_eq!(
            retired["retired"],
            json!(true),
            "a row shown as history must say so"
        );
    }

    /// Readiness follows the chain: a retired blocker whose successor is also
    /// retired still holds its dependent while the chain carries live work,
    /// and a chain that ends with no live work holds nothing.
    #[tokio::test]
    async fn readiness_follows_the_supersession_chain_to_the_live_work() {
        let store = store_with_namespace("MyProj").await;
        let now = chrono::Utc::now().timestamp();

        // retired blocker -> retired successor -> live work in flight
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-chain-retired",
            "retired work",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now - 1, "superseded_by": "MyProj-chain-mid" }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-chain-mid",
            "the same work, replaced again",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now - 1, "superseded_by": "MyProj-chain-live" }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-chain-live",
            "the work that carries on",
            json!({ "status": "in_progress", "assignee": "agent-a" }),
            json!({}),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-chain-dependent",
            "work that waits for the chain",
            json!({ "status": "open", "priority": 1, "blocked_by": ["MyProj-chain-retired"] }),
            json!({}),
        )
        .await;

        // A chain that ends with no live successor is void: the dependent runs.
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-void-retired",
            "retired work, chain ends",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now - 1, "superseded_by": "MyProj-void-mid" }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-void-mid",
            "retired again, with nothing behind it",
            json!({ "status": "open", "priority": 1 }),
            json!({ "valid_to": now - 1 }),
        )
        .await;
        upsert_task_row(
            &store,
            "MyProj",
            "MyProj-void-dependent",
            "work that waited for a void chain",
            json!({ "status": "open", "priority": 1, "blocked_by": ["MyProj-void-retired"] }),
            json!({}),
        )
        .await;

        let reply = handle_task_list(
            json!({ "namespace": "MyProj", "ready": true }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        let ids: Vec<&str> = parsed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert!(
            !ids.contains(&"MyProj-chain-dependent"),
            "the chain still carries live work, so the dependent still waits: {:?}",
            ids
        );
        assert!(
            !ids.contains(&"MyProj-chain-live"),
            "the live successor is claimed, not ready: {:?}",
            ids
        );
        assert!(
            ids.contains(&"MyProj-void-dependent"),
            "a chain with no live work behind it holds nothing: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn unclaiming_releases_the_holders_marks() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let reply = handle_task_update(
            json!({ "namespace": "MyProj", "id": id, "status": "open" }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["status"], json!("open"));
        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert!(
            payload.metadata["task"].get("assignee").is_none(),
            "unclaim clears the assignee: {:?}",
            payload.metadata["task"]
        );
        assert!(payload.metadata["task"].get("session_id").is_none());
        assert!(payload.metadata["task"].get("branch").is_none());
        assert!(payload.metadata["task"].get("worktree").is_none());
    }

    // ── task_validate (the advisory twin of the gate) ───────────────────

    #[tokio::test]
    async fn task_validate_reports_the_gate_body_with_counts() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let reply = handle_task_validate(json!({ "namespace": "MyProj" }), store.clone()).await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["counts"]["in_progress"], json!(1));
        assert_eq!(parsed["violations"][0]["kind"], json!("in_progress"));
        assert!(parsed.get("stale").is_some());
        assert!(parsed.get("unextracted_done").is_some());
    }

    /// R7 fix round 1: a REAL `memories_for_file` response carrying a
    /// `T_HIGH_BLAST` next_action is threaded into the receipt, and validation
    /// warns from that live receipt with NO caller-supplied trigger args.
    /// A genuine `related` receipt then resolves it.
    #[tokio::test]
    async fn task_validate_uses_live_receipt_trigger_context_without_manual_args() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        // A realistic handler response carrying the §5 escalation action.
        let result_text = serde_json::json!({
            "namespace": "MyProj",
            "root": "ok",
            "next_actions": [{
                "trigger_id": "T_HIGH_BLAST",
                "action_id": "T_HIGH_BLAST:abc123",
                "tool": "neurostrata_related",
                "reason": "high blast radius"
            }]
        })
        .to_string();

        let fields = tool_receipts::attach_trigger_context(
            tool_receipts::build_dispatch_receipt(
                "evt-1".to_string(),
                1,
                "sess-1".to_string(),
                "MyProj".to_string(),
                "neurostrata_memories_for_file".to_string(),
                "src/task/mod.rs",
                &result_text,
                false,
                12,
            ),
            &result_text,
        );
        // P1-2: a prescribing receipt identifies the tool that actually ran
        // (T_PATH for memories_for_file); it never stamps a downstream
        // prescribed trigger onto its own trigger_id. The prescription lives
        // only in prescribed_action_ids and is resolved by the executing tool.
        assert_eq!(fields.trigger_id, "T_PATH");
        assert!(fields
            .prescribed_action_ids
            .iter()
            .any(|a| a == "T_HIGH_BLAST:abc123"));

        // Isolated worktree ledger seeded with that attributed receipt.
        let tmp = std::env::temp_dir().join(format!("ns-advisory-e2e-{}", uuid::Uuid::new_v4()));
        let ns_dir = tmp.join(".NeuroStrata");
        std::fs::create_dir_all(&ns_dir).unwrap();
        let mut event = fields.to_value();
        event["task_id"] = serde_json::json!(id);
        let ledger = serde_json::json!({
            "events_version": 1,
            "events": [event.clone()]
        });
        let ledger_path = ns_dir.join("session_queries.json");
        std::fs::write(&ledger_path, ledger.to_string()).unwrap();
        let worktree = tmp.to_string_lossy().to_string();

        // No `applicable_triggers` / `strategic` arg at all.
        let reply = handle_task_validate(
            serde_json::json!({ "namespace": "MyProj", "worktree": worktree }),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        let codes: Vec<&str> = parsed["advisories"]["graph_ontology"]["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            codes.contains(&"W_GRAPH_NO_EXPANSION"),
            "live receipt must warn without manual args: {:?}",
            codes
        );
        assert_eq!(
            parsed["advisories"]["graph_ontology"]["high_blast_radius_unexplored"],
            json!(true)
        );

        // A genuine `related` receipt WITHOUT exact proof must NOT resolve:
        // a default one-hop lookup cannot stand in for the prescribed call.
        let related = serde_json::json!({
            "task_id": id,
            "tool": "neurostrata_related",
            "operation": "related",
            "trigger_id": "T_HIGH_BLAST",
            "status": "success",
            "returned_ids": ["mem-x"]
        });
        let unproven = serde_json::json!({
            "events_version": 1,
            "events": [event.clone(), related.clone()]
        });
        std::fs::write(&ledger_path, unproven.to_string()).unwrap();
        let reply2 = handle_task_validate(
            serde_json::json!({ "namespace": "MyProj", "worktree": worktree }),
            store.clone(),
        )
        .await;
        let parsed2: Value = serde_json::from_str(&reply2).expect("json reply");
        let codes2: Vec<&str> = parsed2["advisories"]["graph_ontology"]["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            codes2.contains(&"W_GRAPH_NO_EXPANSION"),
            "a related receipt without followed-proof must not resolve: {:?}",
            codes2
        );

        // Exact followed-proof of the prescribed action id resolves it.
        let mut proven_related = related;
        proven_related["followed_action_ids"] = serde_json::json!(["T_HIGH_BLAST:abc123"]);
        let resolved = serde_json::json!({
            "events_version": 1,
            "events": [event, proven_related]
        });
        std::fs::write(&ledger_path, resolved.to_string()).unwrap();
        let reply3 = handle_task_validate(
            serde_json::json!({ "namespace": "MyProj", "worktree": worktree }),
            store.clone(),
        )
        .await;
        let parsed3: Value = serde_json::from_str(&reply3).expect("json reply");
        let codes3: Vec<&str> = parsed3["advisories"]["graph_ontology"]["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            !codes3.contains(&"W_GRAPH_NO_EXPANSION"),
            "exact followed-proof must resolve: {:?}",
            codes3
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── snapshot prefix (section 6.1) ───────────────────────────────────

    fn task_row(id: &str, task_meta: Value) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score: 0.0,
            payload: MemoryPayload {
                content: format!("work for {}", id),
                user_id: "kenton".to_string(),
                memory_type: "task".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({ "task": task_meta }),
            },
            evidence: None,
        }
    }

    #[test]
    fn snapshot_prefix_names_claimed_and_ready_tasks_and_opens_with_the_rule() {
        let memories = vec![
            task_row(
                "myproj-aaaa",
                json!({ "status": "in_progress", "assignee": "agent-a" }),
            ),
            task_row("myproj-bbbb", json!({ "status": "open", "priority": 1 })),
            task_row(
                "myproj-cccc",
                json!({ "status": "open", "priority": 3, "blocked_by": ["myproj-bbbb"] }),
            ),
            task_row("myproj-dddd", json!({ "status": "done" })),
            SearchResult {
                id: "rule-1".to_string(),
                score: 0.0,
                payload: MemoryPayload {
                    content: "always use podman".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
                evidence: None,
            },
        ];

        let prefix = snapshot_prefix(&memories);
        assert!(
            prefix.starts_with("Zero-Action Start: claim or create a task before editing."),
            "{}",
            prefix
        );
        assert!(prefix.contains("Claimed"), "{}", prefix);
        assert!(prefix.contains("myproj-aaaa"), "{}", prefix);
        assert!(prefix.contains("agent-a"), "{}", prefix);
        assert!(prefix.contains("Ready to claim:"), "{}", prefix);
        assert!(prefix.contains("myproj-bbbb"), "{}", prefix);
        assert!(
            !prefix.contains("myproj-cccc"),
            "a task blocked by an unfinished one is not ready: {}",
            prefix
        );
        assert!(
            !prefix.contains("myproj-dddd"),
            "done work is not listed: {}",
            prefix
        );
        assert!(
            !prefix.contains("always use podman"),
            "memories stay in the snapshot body"
        );
    }

    #[test]
    fn snapshot_prefix_omits_retired_task_rows() {
        // A retired row is history. The gate does not judge it, so the
        // snapshot must not tell an agent to release a claim that no
        // longer blocks anything.
        let mut retired = task_row(
            "myproj-retired",
            json!({ "status": "in_progress", "assignee": "ghost" }),
        );
        retired.payload.metadata["valid_to"] = json!(chrono::Utc::now().timestamp() - 1);
        let mut retired_open = task_row(
            "myproj-retired-open",
            json!({ "status": "open", "priority": 0 }),
        );
        retired_open.payload.metadata["valid_to"] = json!(chrono::Utc::now().timestamp() - 1);

        let memories = vec![
            retired,
            retired_open,
            task_row(
                "myproj-live",
                json!({ "status": "in_progress", "assignee": "agent-a" }),
            ),
            // Live work whose only blocker was superseded is ready: a retired
            // blocker is history, and history must not hide live work.
            task_row(
                "myproj-blocked-by-retired",
                json!({ "status": "open", "priority": 1, "blocked_by": ["myproj-retired-open"] }),
            ),
        ];

        let prefix = snapshot_prefix(&memories);
        assert!(prefix.contains("myproj-live"), "{}", prefix);
        assert!(
            prefix.contains("myproj-blocked-by-retired"),
            "a retired blocker must not hold live work hostage: {}",
            prefix
        );
        assert!(
            !prefix.contains("myproj-retired"),
            "a retired row is not work in flight: {}",
            prefix
        );
    }

    #[test]
    fn snapshot_prefix_says_so_when_there_is_nothing_to_pick_up() {
        let prefix = snapshot_prefix(&[]);
        assert!(
            prefix.starts_with("Zero-Action Start: claim or create a task before editing."),
            "{}",
            prefix
        );
        assert!(
            prefix.contains("No tasks are claimed or ready"),
            "{}",
            prefix
        );
    }

    // ── bootstrap (section 3.2) ─────────────────────────────────────────

    #[tokio::test]
    async fn bootstrap_creates_the_first_task_and_returns_the_instructions() {
        let store = temp_store().await;
        let reply = handle_bootstrap(
            json!({ "namespace": "MyProj", "project_root": "/tmp" }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");

        assert_eq!(parsed["namespace"], json!("MyProj"));
        assert_eq!(
            parsed["rule"],
            json!("Zero-Action Start is now active: no file edits without a claimed task.")
        );
        let first_id = parsed["first_task"]["id"].as_str().unwrap().to_string();
        assert!(first_id.starts_with("myproj-"), "{}", first_id);
        assert_eq!(
            parsed["first_task"]["title"],
            json!("Ingest codebase AST and write 3 initial architectural rules")
        );

        // The instructor left real work: the task exists server-side.
        let (_, payload) = store
            .get("MyProj", &first_id)
            .await
            .unwrap()
            .expect("created");
        assert_eq!(payload.memory_type, "task");

        let files = parsed["files"].as_array().unwrap();
        assert_eq!(files[0]["path"], json!("AGENTS.md"));
        assert_eq!(files[0]["overwrite"], json!(false));
        let agents_md = files[0]["content"].as_str().unwrap();
        assert!(agents_md.contains("Zero-Action Start"));
        // The bootstrap-generated AGENTS.md must teach the agent the task vs
        // procedure distinction up front -- the rejection error is the
        // backstop, but every agent that reads AGENTS.md on a fresh project
        // gets the right path before ever needing it.
        assert!(
            agents_md.contains("procedure") && agents_md.contains("neurostrata_procedure_perform"),
            "AGENTS.md must point at the procedure path: {agents_md}"
        );
        assert!(
            agents_md.contains("neurostrata_add_memory") && agents_md.contains("memory_type"),
            "AGENTS.md must name the creation path: {agents_md}"
        );
        assert_eq!(files[1]["path"], json!(".NeuroStrata/docs/.gitkeep"));

        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(instructions.len(), 5);
        assert_eq!(instructions[0]["action"], json!("write_file"));
        assert_eq!(
            instructions[1]["cmd"],
            json!("neurostrata-mcp hooks install")
        );
        assert_eq!(instructions[2]["tool"], json!("neurostrata_task_claim"));
        assert_eq!(instructions[2]["params"]["id"], json!(first_id));
        assert_eq!(
            instructions[3]["tool"],
            json!("neurostrata_ingest_directory")
        );
        assert_eq!(
            parsed["hooks"]["install_command"],
            json!("neurostrata-mcp hooks install")
        );

        // The wiring panel (docs/design-wiring-panel.md): one instruction
        // template, git-first but never git-only. /tmp has no .git, so the
        // gate point is unresolved, the non-git clause carries the discovery
        // instruction, and exactly one task is created to resolve it.
        assert!(instructions.iter().all(|i| i["kind"] == json!("action")));
        let wire = &instructions[4];
        assert_eq!(wire["action"], json!("wire_gates"));
        assert_eq!(wire["gate_point"], serde_json::Value::Null);
        let text = wire["text"].as_str().unwrap();
        assert!(text.contains("No git detected"), "{}", text);
        assert!(text.contains("CMS publish"), "{}", text);
        assert!(text.contains("memory_type: rule"), "{}", text);
        assert!(
            !text.contains("hooks install"),
            "no git, no hook instruction: {}",
            text
        );

        let verified = &parsed["verified"];
        assert_eq!(verified["hook_installed"], json!(false));
        assert_eq!(verified["gate_point_resolved"], json!(false));
        let wiring = &parsed["wiring"];
        assert!(wiring["coverage"]["uncovered"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u == "memory-to-repo-drift"));
        assert!(
            wiring["automatic"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w["id"] != "task-gate"),
            "the git default instance is not emitted without git"
        );
        let gate_tasks: Vec<_> = parsed["tasks_created"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["title"].as_str().unwrap().contains("gate point"))
            .collect();
        assert_eq!(
            gate_tasks.len(),
            1,
            "exactly one gate-point task for a non-git project"
        );
    }

    #[tokio::test]
    async fn bootstrap_refuses_a_namespace_that_is_a_path() {
        let store = temp_store().await;
        let reply = handle_bootstrap(
            json!({ "namespace": "some/repo", "project_root": "/tmp" }),
            emb(),
            store,
        )
        .await;
        assert!(reply.starts_with("ERROR [NAMESPACE]"), "{}", reply);
    }

    // ── task_setup (sections 3.2 and 8) ─────────────────────────────────

    #[tokio::test]
    async fn setup_detects_a_legacy_hook_and_beads_and_creates_the_migration_tasks() {
        let root = std::env::temp_dir().join(format!("ns-setup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::create_dir_all(root.join(".beads")).unwrap();
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:me/proj.git\n\tfetch = +refs/*\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".git/hooks/pre-push"),
            "#!/bin/bash\n# legacy mtime check\nDB_MOD_TIME=$(stat -c %Y ~/.config/neurostrata)\nexit 0\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".beads/issues.jsonl"),
            "{\"_type\":\"issue\",\"id\":\"strata-1\",\"title\":\"One\"}\n{\"_type\":\"issue\",\"id\":\"strata-2\",\"title\":\"Two\"}\n",
        )
        .unwrap();
        std::fs::write(root.join(".github/workflows/ci.yml"), "name: ci\n").unwrap();

        let store = store_with_namespace("proj").await;
        let reply = handle_task_setup(
            json!({ "namespace": "proj", "project_root": root.to_string_lossy() }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");

        assert_eq!(
            parsed["detected"]["git_remote"],
            json!("git@github.com:me/proj.git")
        );
        assert_eq!(parsed["detected"]["languages"], json!(["rust"]));
        assert_eq!(parsed["detected"]["ci"], json!(["github-actions"]));
        assert_eq!(parsed["detected"]["beads_dir"], json!(true));
        assert_eq!(parsed["detected"]["agents_md"], json!(false));
        assert_eq!(parsed["detected"]["existing_hooks"], json!(["pre-push"]));

        assert_eq!(parsed["conflicts"][0]["kind"], json!("legacy_hook"));
        assert_eq!(
            parsed["conflicts"][0]["resolution"],
            json!("neurostrata-mcp hooks install --force replaces it")
        );
        assert_eq!(parsed["suggested_rules"].as_array().unwrap().len(), 2); // rust + ci

        let created = parsed["tasks_created"].as_array().unwrap();
        assert_eq!(
            created.len(),
            2,
            "hook replacement + beads migration: {:?}",
            created
        );
        assert!(created[1]["title"]
            .as_str()
            .unwrap()
            .contains("Migrate 2 beads issues"));
        assert!(created[1]["metadata_hint"]
            .as_str()
            .unwrap()
            .contains("neurostrata-mcp task import proj --from-beads .beads/issues.jsonl"));

        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(
            instructions[0]["cmd"],
            json!("neurostrata-mcp hooks install --force")
        );
        assert!(
            instructions.iter().any(|i| {
                i["cmd"]
                    .as_str()
                    .map(|c| c.starts_with("neurostrata-mcp task import"))
                    .unwrap_or(false)
            }),
            "the import step is instructed: {:?}",
            instructions
        );
        assert!(instructions
            .iter()
            .any(|i| i["tool"] == json!("neurostrata_add_memory")));
        assert!(
            instructions
                .iter()
                .any(|i| i["tool"] == json!("neurostrata_task_complete")),
            "completing the setup tasks forces the first extractions: {:?}",
            instructions
        );

        // The tasks really exist, in creation order.
        let rows = store.list("proj", None, None).await.unwrap();
        let tasks: Vec<_> = rows
            .iter()
            .filter(|r| r.payload.memory_type == "task")
            .collect();
        assert_eq!(tasks.len(), 2);
        for created in created {
            let id = created["id"].as_str().unwrap();
            assert!(
                tasks.iter().any(|t| t.id == id),
                "{} was announced but not created",
                id
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn setup_on_a_clean_project_suggests_the_plain_hook_and_flags_the_seeded_overlap() {
        let root = std::env::temp_dir().join(format!("ns-setup-clean-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();

        let store = store_with_namespace("webproj").await;
        let reply = handle_task_setup(
            json!({ "namespace": "webproj", "project_root": root.to_string_lossy() }),
            emb(),
            store.clone(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");

        // R16 made ARRAY_DISTANCE bind whole-number vectors (the stub
        // embedder's), so the setup search now finds the seeded rule and the
        // overlap is reported -- the binder working as intended, not a
        // conflict-free project. Pre-R16 the query errored and setup
        // swallowed it, so this expectation read as no conflicts.
        let conflicts = parsed["conflicts"].as_array().expect("conflicts list");
        assert_eq!(conflicts.len(), 1, "conflicts: {:?}", parsed["conflicts"]);
        assert_eq!(conflicts[0]["kind"], json!("suggested_rule_overlap"));
        assert_eq!(conflicts[0]["existing"], json!("always use podman"));
        assert_eq!(
            conflicts[0]["suggestion"],
            json!("JavaScript/TypeScript project: install from the lockfile and keep the package scripts (test, lint) green before pushing.")
        );
        assert_eq!(parsed["tasks_created"], json!([]));
        assert_eq!(
            parsed["instructions"][0]["cmd"],
            json!("neurostrata-mcp hooks install")
        );
        assert_eq!(parsed["suggested_rules"][0]["memory_type"], json!("rule"));

        std::fs::remove_dir_all(&root).ok();
    }

    // ── beads import (section 7) ────────────────────────────────────────

    const BEADS_FIXTURE: &str = r#"{"_type":"issue","id":"strata-1","title":"Fix the gate","description":"the gate wedged on push","status":"closed","priority":0,"issue_type":"bug","created_at":"2026-05-25T16:54:42Z","updated_at":"2026-05-25T16:58:41Z","closed_at":"2026-05-25T16:58:41Z","close_reason":"Fixed","labels":["P0","bug"],"created_by":"neo","dependencies":[{"issue_id":"strata-1","depends_on_id":"strata-2","type":"blocks"}]}
{"_type":"issue","id":"strata-2","title":"Open thing","status":"open","priority":2,"issue_type":"task","created_at":"2026-05-26T10:00:00Z","updated_at":"2026-05-26T10:00:00Z","labels":["P2"]}
{"_type":"milestone","id":"ms-1"}
{"_type":"issue","id":"not json"#;

    async fn import_fixture(store: &Arc<dyn VectorStore>, namespace: &str) -> ImportSummary {
        let path = std::env::temp_dir().join(format!("beads-{}.jsonl", uuid::Uuid::new_v4()));
        std::fs::write(&path, BEADS_FIXTURE).unwrap();
        let summary = import_beads(store.clone(), emb(), namespace, &path.to_string_lossy())
            .await
            .expect("import succeeds");
        std::fs::remove_file(&path).ok();
        summary
    }

    #[tokio::test]
    async fn import_maps_closed_issues_to_grandfathered_done_tasks() {
        let store = store_with_namespace("proj").await;
        let summary = import_fixture(&store, "proj").await;
        assert_eq!(summary.created, 2);
        assert_eq!(summary.skipped_existing, 0);
        assert_eq!(summary.non_issue_lines, 1, "the milestone line is skipped");
        assert_eq!(
            summary.malformed_lines, 1,
            "the broken line is counted, not fatal"
        );

        let rows = store.list("proj", None, None).await.unwrap();
        let tasks: Vec<_> = rows
            .iter()
            .filter(|r| r.payload.memory_type == "task")
            .collect();
        assert_eq!(tasks.len(), 2);

        let closed = tasks
            .iter()
            .find(|t| t.payload.metadata["task"]["bead_id"] == json!("strata-1"))
            .expect("the closed issue was imported");
        assert_eq!(closed.payload.content, "Fix the gate");
        assert_eq!(closed.payload.metadata["task"]["status"], json!("done"));
        assert_eq!(
            closed.payload.metadata["task"]["grandfathered"],
            json!(true)
        );
        assert_eq!(
            closed.payload.metadata["task"]["close_reason"],
            json!("Fixed")
        );
        assert_eq!(closed.payload.metadata["task"]["priority"], json!(0));
        assert_eq!(closed.payload.metadata["task"]["task_type"], json!("bug"));
        assert_eq!(
            closed.payload.metadata["task"]["description"],
            json!("the gate wedged on push")
        );
        assert_eq!(
            closed.payload.metadata["task"]["created_at"],
            json!("2026-05-25T16:54:42Z"),
            "timestamps are preserved verbatim"
        );
        assert_eq!(
            closed.payload.user_id, "neo",
            "created_by becomes the record's user_id"
        );
        // blocked_by is translated into this system's ids (see below).
        let blocked = closed.payload.metadata["task"]["blocked_by"]
            .as_array()
            .unwrap();
        assert_eq!(blocked.len(), 1);

        let open = tasks
            .iter()
            .find(|t| t.payload.metadata["task"]["bead_id"] == json!("strata-2"))
            .expect("the open issue was imported");
        assert_eq!(open.payload.metadata["task"]["status"], json!("open"));
        assert!(open.payload.metadata["task"].get("grandfathered").is_none());

        // The dep on strata-2 now names the task that was created for it, so
        // readiness joins work instead of pointing at a beads id.
        let strata_2_task_id = open.id.clone();
        assert_eq!(blocked[0].as_str(), Some(strata_2_task_id.as_str()));
    }

    #[tokio::test]
    async fn import_is_idempotent_on_bead_id() {
        let store = store_with_namespace("proj").await;
        let first = import_fixture(&store, "proj").await;
        assert_eq!(first.created, 2);

        let second = import_fixture(&store, "proj").await;
        assert_eq!(second.created, 0, "re-runs skip everything");
        assert_eq!(second.skipped_existing, 2);

        let rows = store.list("proj", None, None).await.unwrap();
        let tasks: Vec<_> = rows
            .iter()
            .filter(|r| r.payload.memory_type == "task")
            .collect();
        assert_eq!(tasks.len(), 2, "no duplicates after a re-run");
    }

    // ── done funnel: the message itself is the contract ─────────────────

    #[test]
    fn the_funnel_message_is_exactly_what_the_design_specifies() {
        assert_eq!(
            DONE_FUNNEL_MESSAGE,
            "ERROR: done is reachable only via neurostrata_task_complete, which requires memory extraction (Lock 2)."
        );
    }

    /// R6: `handle_task_list` must recompute `subagent_prompt_injection` for
    /// InProgress tasks whenever the cached prompt starts with the standard
    /// header, so an archived governing rule no longer appears in the prompt.
    #[tokio::test]
    async fn task_list_recomputes_prompt_when_governing_rule_is_archived() {
        let store = store_with_namespace("MyProj").await;

        // 1. Insert a live rule memory in the namespace.
        let rule_id = uuid::Uuid::new_v4().to_string();
        let rule_vector = StubEmbedder
            .embed("Never use unwrap in production")
            .await
            .unwrap();
        store
            .upsert(
                "MyProj",
                &rule_id,
                rule_vector,
                MemoryPayload {
                    content: "Never use unwrap in production".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("seed the rule");

        // 2. Create and claim a task in MyProj.
        let task_id = create_task(&store, "MyProj", "Fix production unwrap", 1).await;
        handle_task_claim(
            json!({
                "namespace": "MyProj",
                "id": task_id,
                "assignee": "agent-a",
                "session_id": "ses_rule"
            }),
            store.clone(),
        )
        .await;

        // 3. handle_task_list must include the rule id/snippet in the prompt.
        let list_before: Value = serde_json::from_str(
            &handle_task_list(
                json!({ "namespace": "MyProj", "status": "in_progress" }),
                store.clone(),
            )
            .await,
        )
        .unwrap();
        let prompt_before = list_before["tasks"][0]["subagent_prompt_injection"]
            .as_str()
            .expect("prompt present before archive");
        assert!(
            prompt_before.contains(&rule_id),
            "the live rule id must appear in the prompt: {}",
            prompt_before
        );
        assert!(
            prompt_before.contains("Never use unwrap in production"),
            "the live rule snippet must appear in the prompt: {}",
            prompt_before
        );

        // 4. Archive the rule.
        let bus = Arc::new(crate::events::ThalamicBus::new(16));
        let _ = crate::handlers::archive_memory::handle_archive_memory(
            json!({ "namespace": "MyProj", "id": rule_id }),
            store.clone(),
            bus.clone(),
        )
        .await;
        let (_, rule_payload) = store.get("MyProj", &rule_id).await.unwrap().unwrap();
        assert!(
            crate::handlers::archive_memory::is_archived(&rule_payload),
            "the rule must really be archived"
        );

        // 5. handle_task_list again: the archived rule must no longer appear.
        let list_after: Value = serde_json::from_str(
            &handle_task_list(
                json!({ "namespace": "MyProj", "status": "in_progress" }),
                store.clone(),
            )
            .await,
        )
        .unwrap();
        let prompt_after = list_after["tasks"][0]["subagent_prompt_injection"]
            .as_str()
            .expect("prompt present after archive");
        assert!(
            !prompt_after.contains(&rule_id),
            "the archived rule id must not appear in the recomputed prompt: {}",
            prompt_after
        );
        assert!(
            !prompt_after.contains("Never use unwrap in production"),
            "the archived rule snippet must not appear in the recomputed prompt: {}",
            prompt_after
        );
    }

    /// Wave 3 / Task 3.1: `build_task_graph_context` partitions retrieved items into
    /// reserved `normative_memories` and `structural_anchors`, synthesises 2-hop
    /// cross-domain `conceptual_bridges`, and emits a `Cross-Domain Bridges`
    /// section in the subagent prompt when bridges are present.
    #[tokio::test]
    async fn task_graph_context_partitions_normative_structural_and_conceptual_bridges() {
        let store = store_with_namespace("probe").await;

        // M1: normative rule governing src/server.rs (backend domain).
        let m1_id = "mem-rule-m1".to_string();
        store
            .upsert(
                "probe",
                &m1_id,
                vec![0.0f32; 4],
                MemoryPayload {
                    content: "Daemon caller path check required on ingest".to_string(),
                    user_id: "owner".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "src/server.rs".to_string(),
                    location_lines: String::new(),
                    metadata: json!({"domain": "backend", "governs": ["src/server.rs"], "source": "owner 2026-11-07"}),
                },
            )
            .await
            .expect("seed M1 rule");

        // File B: plugins/core/index.ts (frontend domain), structural anchor.
        let file_b_id = "mem-file-b".to_string();
        store
            .upsert(
                "probe",
                &file_b_id,
                vec![0.0f32; 4],
                MemoryPayload {
                    content: "bundled harness adapter".to_string(),
                    user_id: "owner".to_string(),
                    memory_type: "code_ast".to_string(),
                    agent_name: None,
                    location: "plugins/core/index.ts".to_string(),
                    location_lines: String::new(),
                    metadata: json!({"domain": "frontend"}),
                },
            )
            .await
            .expect("seed file B");

        // Intermediate via node (frontend domain).
        let via_id = "mem-via".to_string();
        store
            .upsert(
                "probe",
                &via_id,
                vec![0.0f32; 4],
                MemoryPayload {
                    content: "adapter entry point".to_string(),
                    user_id: "owner".to_string(),
                    memory_type: "code_ast".to_string(),
                    agent_name: None,
                    location: "plugins/core/index.ts#fn:main".to_string(),
                    location_lines: String::new(),
                    metadata: json!({"domain": "frontend"}),
                },
            )
            .await
            .expect("seed via node");

        // 2-hop path: file-b ->(DEPENDS_ON)-> via ->(RELATES_TO)-> M1.
        let edge = |rel: &str, a: &str, b: &str| {
            format!(
                "MERGE (a:Memory {{id: '{a}'}}), (b:Memory {{id: '{b}'}}) MERGE (a)-[r:{rel} {{source: 'test', valid_from: 0, valid_to: 0, generation: 0, extractor: 'test', evidence_id: 'e', origin: 'local'}}]->(b)"
            )
        };
        let ladybug = VectorStore::as_any(&*store)
            .downcast_ref::<LadybugStore>()
            .expect("test store is a LadybugStore");
        for (rel, a, b) in [
            ("DEPENDS_ON", "mem-file-b", "mem-via"),
            ("RELATES_TO", "mem-via", "mem-rule-m1"),
        ] {
            ladybug
                .with_conn(move |conn| {
                    conn.query(&edge(rel, a, b))?;
                    Ok(())
                })
                .await
                .expect("seed bridge edge");
        }

        let payload = MemoryPayload {
            content: "review the harness adapter and its governing rule".to_string(),
            user_id: "owner".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (ctx, prompt) =
            build_task_graph_context(&store, "probe", "task-1", &[0.0f32; 4], &payload).await;

        let obj = ctx
            .as_object()
            .expect("auto_retrieved_context is an object");
        for key in [
            "query",
            "items",
            "related_edges",
            "normative_memories",
            "structural_anchors",
            "conceptual_bridges",
        ] {
            assert!(
                obj.contains_key(key),
                "auto_retrieved_context missing `{key}`: {obj:?}"
            );
        }

        let nm = obj
            .get("normative_memories")
            .and_then(|v| v.as_array())
            .expect("normative_memories is an array");
        assert!(
            nm.iter()
                .any(|v| v.get("id").and_then(|v| v.as_str()) == Some("mem-rule-m1")),
            "M1 rule must be in normative_memories: {nm:?}"
        );
        assert!(
            nm.len() <= MAX_NORMATIVE_MEMORIES,
            "normative_memories within cap: {nm:?}"
        );

        let sa = obj
            .get("structural_anchors")
            .and_then(|v| v.as_array())
            .expect("structural_anchors is an array");
        assert!(
            sa.iter()
                .any(|v| v.get("id").and_then(|v| v.as_str()) == Some("mem-file-b")),
            "file B must be in structural_anchors: {sa:?}"
        );
        assert!(
            sa.len() <= MAX_STRUCTURAL_ANCHORS,
            "structural_anchors within cap: {sa:?}"
        );

        let cb = obj
            .get("conceptual_bridges")
            .and_then(|v| v.as_array())
            .expect("conceptual_bridges is an array");
        assert!(
            !cb.is_empty(),
            "conceptual_bridges should be non-empty: {cb:?}"
        );
        assert!(
            cb.len() <= MAX_CONCEPTUAL_BRIDGES,
            "conceptual_bridges within cap: {cb:?}"
        );
        let bridge = cb.first().expect("at least one bridge");
        assert_eq!(
            bridge.get("id").and_then(|v| v.as_str()),
            Some("mem-rule-m1"),
            "bridge target must be M1: {bridge:?}"
        );
        assert!(
            bridge
                .get("why_connected")
                .and_then(|v| v.as_str())
                .is_some(),
            "bridge carries why_connected: {bridge:?}"
        );

        assert!(
            prompt.contains("### Cross-Domain Bridges"),
            "prompt has Cross-Domain Bridges section: {prompt}"
        );
        assert!(
            prompt.contains("mem-rule-m1"),
            "prompt references M1: {prompt}"
        );
    }

    #[tokio::test]
    async fn build_task_graph_context_surfaces_global_and_pinned_golden_rules() {
        let store = temp_store().await;
        store.init("global").await.expect("init global");
        store.init("probe").await.expect("init probe");

        // Global rule (even stored as memory_type="fact" with RULE: prefix and access_count=0).
        store
            .upsert(
                "global",
                "bf710e0e-a4b5-41cc-a8d7-e25fb14b12f8",
                vec![0.9f32; 4],
                MemoryPayload {
                    content: "RULE: The user has a strong preference AGAINST using Python."
                        .to_string(),
                    user_id: "owner".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"access_count": 0}),
                },
            )
            .await
            .expect("seed global python rule");

        // Project pinned rule with access_count=0.
        store
            .upsert(
                "probe",
                "pinned-local-1",
                vec![0.8f32; 4],
                MemoryPayload {
                    content: "Never use docker, always use podman.".to_string(),
                    user_id: "owner".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"pinned": true, "access_count": 0}),
                },
            )
            .await
            .expect("seed local pinned rule");

        let payload = MemoryPayload {
            content: "implement feature x".to_string(),
            user_id: "owner".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (ctx, prompt) =
            build_task_graph_context(&store, "probe", "task-1", &[0.1f32; 4], &payload).await;

        assert!(
            prompt.contains("bf710e0e-a4b5-41cc-a8d7-e25fb14b12f8")
                && prompt.contains("AGAINST using Python"),
            "subagent prompt must surface global NO PYTHON rule: {prompt}"
        );
        assert!(
            prompt.contains("pinned-local-1") && prompt.contains("always use podman"),
            "subagent prompt must surface local pinned rule: {prompt}"
        );
        let golden = ctx["golden_rules"]
            .as_array()
            .expect("auto_retrieved_context.golden_rules array");
        assert!(
            golden
                .iter()
                .any(|g| g["id"] == "bf710e0e-a4b5-41cc-a8d7-e25fb14b12f8"),
            "golden_rules must include global rule: {golden:?}"
        );
        assert!(
            golden.iter().any(|g| g["id"] == "pinned-local-1"),
            "golden_rules must include local pinned rule: {golden:?}"
        );
    }

    #[tokio::test]
    async fn task_context_carries_real_related_edges() {
        let store = store_with_namespace("probe").await;
        // Two memories connected by a GOVERNS edge.
        let a = "mem-a".to_string();
        let b = "mem-b".to_string();
        for id in [&a, &b] {
            store
                .upsert(
                    "probe",
                    id,
                    vec![0.0f32; 4],
                    MemoryPayload {
                        content: format!("memory {}", id),
                        user_id: "owner".to_string(),
                        memory_type: "rule".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({}),
                    },
                )
                .await
                .expect("seed");
        }
        let ladybug = VectorStore::as_any(&*store)
            .downcast_ref::<LadybugStore>()
            .expect("ladybug");
        ladybug
            .with_conn(move |conn| {
                conn.query(&format!(
                    "MERGE (a:Memory {{id: 'mem-a'}}), (b:Memory {{id: 'mem-b'}}) MERGE (a)-[r:GOVERNS {{source: 'test', valid_from: 0, valid_to: 0, generation: 0, extractor: 'test', evidence_id: 'e', origin: 'local'}}]->(b)"
                ))?;
                Ok(())
            })
            .await
            .expect("edge");

        let payload = MemoryPayload {
            content: "memory mem-a".to_string(),
            user_id: "owner".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (ctx, _prompt) =
            build_task_graph_context(&store, "probe", "task-1", &[0.0f32; 4], &payload).await;
        let edges = ctx["related_edges"]
            .as_array()
            .expect("related_edges array");
        assert!(
            !edges.is_empty(),
            "related_edges must be non-empty: {edges:?}"
        );
        let calls = ctx["next_hop_calls"]
            .as_array()
            .expect("next_hop_calls array");
        assert!(!calls.is_empty(), "next_hop_calls must be non-empty");
        assert_eq!(calls[0]["tool"], "neurostrata_related");
        assert_eq!(calls[0]["trigger_id"], "T_SEED_EXPAND");
    }

    // Task 13 P1-4: no edges -> no action; with edge -> T_SEED_EXPAND; ids do
    // not cross-resolve. P1-6: task metadata fires real R4 triggers.
    #[test]
    fn seed_expand_only_for_seeds_with_edges() {
        let none = seed_expand_next_hops("ns", &["s1".to_string()], &[]);
        assert!(none.is_empty(), "a seed with no edges must abstain");
        let edges = vec![json!({"source":"s1","relation":"GOVERNS","target":"s2"})];
        let some = seed_expand_next_hops("ns", &["s1".to_string(), "s9".to_string()], &edges);
        assert_eq!(some.len(), 1, "only the seeded-with-edge id fires");
        assert_eq!(some[0]["trigger_id"], "T_SEED_EXPAND");
        assert_eq!(some[0]["tool"], "neurostrata_related");
        assert_eq!(some[0]["args"]["id"], "s1");
        assert!(some[0]["action_id"]
            .as_str()
            .unwrap()
            .starts_with("T_SEED_EXPAND:"));
    }

    #[test]
    fn task_routing_triggers_use_canonical_contract() {
        assert!(task_routing_triggers("ns", &json!({})).is_empty());
        let routes =
            task_routing_triggers("ns", &json!({"task_type": "feature", "strategic": true}));
        let ids: Vec<&str> = routes.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["T_DOMAIN_NEW", "T_STRATEGIC"]);
        assert_eq!(routes[1].tool, "neurostrata_taxonomy");
        assert_eq!(routes[1].args["operation"], "advise");
    }

    #[tokio::test]
    async fn golden_rules_survive_graph_expansion_budget() {
        let store = temp_store().await;
        store.init("probe").await.expect("init probe");
        // Golden rule.
        store
            .upsert(
                "probe",
                "golden-1",
                vec![0.5f32; 4],
                MemoryPayload {
                    content: "Never use unwrap in production code paths.".to_string(),
                    user_id: "owner".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"pinned": true, "access_count": 0}),
                },
            )
            .await
            .expect("seed golden");
        // Bloat structural memories.
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("struct-{}", i),
                    vec![0.5f32; 4],
                    MemoryPayload {
                        content: "x".repeat(2000),
                        user_id: "owner".to_string(),
                        memory_type: "code_ast".to_string(),
                        agent_name: None,
                        location: format!("src/file{}.rs", i),
                        location_lines: String::new(),
                        metadata: json!({}),
                    },
                )
                .await
                .expect("seed struct");
        }
        let payload = MemoryPayload {
            content: "implement feature x".to_string(),
            user_id: "owner".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (ctx, prompt) =
            build_task_graph_context(&store, "probe", "task-1", &[0.5f32; 4], &payload).await;
        assert!(
            prompt.chars().count() <= 3500,
            "prompt too long: {}",
            prompt.chars().count()
        );
        assert!(
            prompt.contains("golden-1"),
            "golden rule must survive truncation: {prompt}"
        );
        let golden = ctx["golden_rules"].as_array().expect("golden_rules");
        assert!(!golden.is_empty(), "golden_rules must remain present");
    }

    // Task 13 P1-9: a 2,500-char goal + many golden rules still renders under
    // 3,500 chars with every Golden Rule id preserved.
    #[tokio::test]
    async fn subagent_prompt_bounded_with_long_goal_and_many_golden_rules() {
        let store = temp_store().await;
        store.init("probe").await.expect("init probe");
        let mut ids = Vec::new();
        for i in 0..15 {
            let id = format!("golden-{i}");
            ids.push(id.clone());
            store
                .upsert(
                    "probe",
                    &id,
                    vec![0.5f32; 4],
                    MemoryPayload {
                        content: format!("rule {i}: {}", "g".repeat(200)),
                        user_id: "owner".to_string(),
                        memory_type: "rule".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({"pinned": true, "access_count": 0}),
                    },
                )
                .await
                .expect("seed golden");
        }
        let payload = MemoryPayload {
            content: "G".repeat(2500),
            user_id: "owner".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (_ctx, prompt) =
            build_task_graph_context(&store, "probe", "task-bound", &[0.5f32; 4], &payload).await;
        assert!(
            prompt.chars().count() <= 3500,
            "prompt too long: {}",
            prompt.chars().count()
        );
        for id in &ids {
            assert!(prompt.contains(id), "golden rule id {id} was dropped");
        }
    }

    // Review IMPORTANT-3: a pathological golden-rule load that still exceeds
    // 3,500 after every shed stage must hard-cap gracefully (no assert panic),
    // keeping the early Golden Rule ids and a valid UTF-8 boundary.
    #[tokio::test]
    async fn subagent_prompt_hard_caps_with_pathological_golden_rules() {
        let store = temp_store().await;
        store.init("probe").await.expect("init probe");
        let mut ids = Vec::new();
        for i in 0..28 {
            let id = format!("pathological-golden-{i}");
            ids.push(id.clone());
            store
                .upsert(
                    "probe",
                    &id,
                    vec![0.5f32; 4],
                    MemoryPayload {
                        content: format!("rule {i}: {}", "p".repeat(600)),
                        user_id: "owner".to_string(),
                        memory_type: "rule".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({"pinned": true, "access_count": 0}),
                    },
                )
                .await
                .expect("seed golden");
        }
        let payload = MemoryPayload {
            content: "G".repeat(300),
            user_id: "owner".to_string(),
            memory_type: "task".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (_ctx, prompt) =
            build_task_graph_context(&store, "probe", "task-pathological", &[0.5f32; 4], &payload)
                .await;
        assert!(
            prompt.chars().count() <= 3500,
            "hard cap failed: {}",
            prompt.chars().count()
        );
        for id in ids.iter().take(4) {
            assert!(prompt.contains(id), "early golden rule id {id} was dropped");
        }
        // A truncated tail must still be valid UTF-8 (String guarantees this),
        // and the bound must be exactly respected, not merely approximated.
        assert_eq!(prompt.chars().count(), prompt.chars().take(3500).count());
    }

    // ── Capability C: task views + task_validate production surfaces ──────
    #[tokio::test]
    async fn task_validate_exposes_advisory_stale_memories_from_non_task_rows() {
        let store = store_with_namespace("MyProj").await;
        let fact_id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "MyProj",
                &fact_id,
                StubEmbedder
                    .embed("decayed external API fact")
                    .await
                    .unwrap(),
                MemoryPayload {
                    content: "external API fact that decayed".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"stale_after": "2000-01-01T00:00:00Z"}),
                },
            )
            .await
            .expect("seed stale fact");

        let reply = handle_task_validate(json!({ "namespace": "MyProj" }), store).await;
        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        let alias = parsed
            .get("stale_memories")
            .expect("task_validate must expose stale_memories alias");
        let items = alias.as_array().expect("stale_memories is a list");
        assert!(
            items.iter().any(|v| v["id"] == json!(fact_id)),
            "non-task stale row must be listed: {reply}"
        );
        assert!(
            items.iter().any(|v| v.to_string().contains("advisory")),
            "stale_memories must be labelled advisory (nonblocking): {reply}"
        );
        assert!(parsed.get("stale").is_some(), "legacy `stale` key retained");
    }

    #[tokio::test]
    async fn task_view_governing_memories_emit_deprecation_presentation() {
        let store = store_with_namespace("MyProj").await;
        let rule_id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "MyProj",
                &rule_id,
                StubEmbedder.embed("legacy podman rule").await.unwrap(),
                MemoryPayload {
                    content: "legacy podman rule that was superseded".to_string(),
                    user_id: "kenton".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "deprecation": {
                            "status": "deprecated",
                            "superseded_by": "mem-replacement-42",
                            "reason": "replaced by the podman v2 rule",
                            "effective_at": 1700000000
                        },
                        "not_definition": "does not apply to rootless podman sockets",
                    }),
                },
            )
            .await
            .expect("seed deprecated rule");

        let task_id = create_task(&store, "MyProj", "Review podman guidance", 1).await;
        handle_task_claim(
            json!({
                "namespace": "MyProj",
                "id": task_id,
                "assignee": "agent-a",
                "session_id": "ses-capc-view"
            }),
            store.clone(),
        )
        .await;
        let list: Value = serde_json::from_str(
            &handle_task_list(
                json!({ "namespace": "MyProj", "status": "in_progress" }),
                store.clone(),
            )
            .await,
        )
        .unwrap();
        let prompt = list["tasks"][0]["subagent_prompt_injection"]
            .as_str()
            .expect("prompt present");
        assert!(
            prompt.contains(
                "[DEPRECATED — SUPERSEDED BY mem-replacement-42: replaced by the podman v2 rule]"
            ),
            "governing memory in task view must emit exact banner: {prompt}"
        );
        assert!(
            prompt.contains("NOT DEFINITION: does not apply to rootless podman sockets"),
            "governing memory in task view must emit boundary: {prompt}"
        );
    }
}
