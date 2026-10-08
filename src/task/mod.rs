//! The task subsystem: handlers for the eight MCP tools, the beads importer,
//! and the snapshot header that puts every session inside the task system.
//!
//! Tasks are memories (`memory_type: "task"`) whose state lives in
//! `metadata.task`. `done` has exactly one entrance -- `task_complete`, and it
//! is guarded by an extraction (Lock 2). See docs/design-task-subsystem.md
//! sections 1 through 3 and 6 through 8.

pub mod gate;
pub mod machine;

use crate::judgment::DeduplicationChecker;
use crate::traits::{Embedder, MemoryPayload, SearchResult, VectorStore};
use machine::{apply, annotate, Ctx, Status, Task};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
        .list(namespace, None)
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
    memories
        .iter()
        .filter(|m| gate::is_task(m))
        .map(|m| (m.id.clone(), gate::task_status(m)))
        .collect()
}

/// `ready` (section 3.2): open AND every blocker done. A blocker that is
/// missing cannot be done, so the task stays unready -- conservative by
/// default rather than optimistically actionable.
fn is_ready(payload: &MemoryPayload, statuses: &HashMap<String, Status>) -> bool {
    blocked_by_of(payload)
        .iter()
        .all(|dep| statuses.get(dep) == Some(&Status::Done))
}

/// The base record every tool returns alongside its own fields.
pub fn task_summary(id: &str, payload: &MemoryPayload) -> Value {
    let task = payload.metadata.get("task");
    let field = |key: &str| task.and_then(|t| t.get(key)).cloned().unwrap_or(Value::Null);
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
        .map_err(|e| format!("Failed to store task '{}': {}", id, e))
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
    let title = match required_str(&args, "title") {
        Ok(t) => t,
        Err(e) => return e,
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
            return format!(
                "ERROR: unknown task_type '{}'. Valid types: task, bug, feature, epic.",
                t
            )
        }
    };
    let priority = match args.get("priority") {
        None => 2,
        Some(Value::Number(n))
            if n.as_i64().map(|p| (0..=4).contains(&p)).unwrap_or(false) =>
        {
            n.as_i64().unwrap_or(2)
        }
        Some(_) => return "ERROR: 'priority' must be an integer from 0 to 4 (0 is highest).".to_string(),
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
        title,
        &user_id,
        agent_name.as_deref(),
        task_meta,
        top_level,
    )
    .await
    {
        Ok((id, payload)) => encode(&json!({
            "id": id,
            "namespace": namespace,
            "task": task_summary(&id, &payload),
        })),
        Err(e) => e,
    }
}

// ── tool: neurostrata_task_claim ───────────────────────────────────────────

pub async fn handle_task_claim(args: Value, store: Arc<dyn VectorStore>) -> String {
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

    let assignee = arg_str(&args, "assignee")
        .unwrap_or("unknown")
        .to_string();
    let session = arg_str(&args, "session_id").unwrap_or("").to_string();

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

    match from {
        // Re-claiming what this session already holds is idempotent.
        Status::InProgress if holder == session => {
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
        Status::InProgress => {
            let now = chrono::Utc::now().timestamp();
            let age = gate::age_secs(
                task.payload.metadata.get("task"),
                &["updated_at", "created_at"],
                now,
            );
            // Live = updated within the staleness window. Stale = the holder
            // probably went away, so the claim is up for grabs (section 5.3).
            let live = age.map(|a| a <= gate::STALE_AFTER_SECS).unwrap_or(false);
            if live {
                return format!(
                    "ERROR: task {} is claimed by {} (session '{}') and that claim is live (last update {}s ago). Two live sessions on one task duplicate work: ask them to release it (neurostrata_task_update with status 'open'), or claim it yourself after {} minutes without an update.",
                    id,
                    holder_assignee,
                    holder,
                    age.unwrap_or(0),
                    gate::STALE_AFTER_SECS / 60
                );
            }
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
            note = format!(
                "claim (took over a stale claim by {})",
                holder_assignee
            );
        }
        Status::Open => {}
        Status::Blocked | Status::Done => {
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
    {
        let obj = task_obj_mut(&mut task.payload);
        obj.insert("assignee".to_string(), json!(assignee));
        obj.insert("session_id".to_string(), json!(session));
    }

    if let Err(e) = save_task(&store, &namespace, &id, vector, task.payload.clone()).await {
        return e;
    }
    encode(&json!({
        "id": id,
        "namespace": namespace,
        "status": task.status().as_str(),
        "claimed": true,
        "transition": { "from": from.as_str(), "to": "in_progress" },
        "task": task_summary(&id, &task.payload),
    }))
}

// ── tool: neurostrata_task_update ──────────────────────────────────────────

pub async fn handle_task_update(
    args: Value,
    _emb: Arc<dyn Embedder>,
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
    let priority = match args.get("priority") {
        None => None,
        Some(Value::Number(n))
            if n.as_i64().map(|p| (0..=4).contains(&p)).unwrap_or(false) => n.as_i64(),
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
        && priority.is_none()
        && assignee.is_none()
        && add_labels.is_empty()
        && !has_blocked_by
    {
        return "ERROR: nothing to update: pass at least one of status, note, priority, assignee, add_labels, blocked_by.".to_string();
    }

    let (mut task, vector) = match load_task(&store, &namespace, &id).await {
        Ok(found) => found,
        Err(e) => return e,
    };
    let from = task.status();
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
        _ if note.is_some() => task = annotate(&task, &ctx),
        _ => {}
    }

    // Field updates ride on whatever the transition produced.
    {
        let obj = task_obj_mut(&mut task.payload);
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
    encode(&json!({
        "id": id,
        "namespace": namespace,
        "status": task.status().as_str(),
        "transition": transition,
        "task": task_summary(&id, &task.payload),
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
    let include_done = args.get("include_done").and_then(|v| v.as_bool()).unwrap_or(false);
    let ready_only = args.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);

    let memories = match store.list(&namespace, None).await {
        Ok(m) => m,
        Err(e) => {
            return format!(
                "ERROR: could not list tasks in '{}': {}. Check neurostrata_list_namespaces.",
                namespace, e
            )
        }
    };
    let statuses = task_statuses(&memories);

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
    if ready_only {
        tasks.retain(|m| {
            gate::task_status(m) == Status::Open && is_ready(&m.payload, &statuses)
        });
    }

    // Highest priority first; ties keep the list deterministic by id.
    tasks.sort_by(|a, b| priority_of(&a.payload).cmp(&priority_of(&b.payload)).then_with(|| a.id.cmp(&b.id)));

    let summaries: Vec<Value> = tasks
        .iter()
        .map(|m| {
            let ready = gate::task_status(m) == Status::Open && is_ready(&m.payload, &statuses);
            let mut summary = task_summary(&m.id, &m.payload);
            if let Some(obj) = summary.as_object_mut() {
                obj.insert("ready".to_string(), json!(ready));
            }
            summary
        })
        .collect();

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
/// `memory` through the add_memory pipeline. Failure is `Err`, which the
/// dispatcher turns into a JSON-RPC -32603 -- never a success payload.
pub async fn handle_task_complete(
    args: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    dedup: Option<Arc<DeduplicationChecker>>,
) -> Result<String, String> {
    let id = required_str(&args, "id")?.to_string();
    let namespace_arg = required_str(&args, "namespace")?;
    let namespace = crate::server::resolve_namespace(&store, namespace_arg).await;
    check_namespace(&namespace)?;

    let (task, vector) = load_task(&store, &namespace, &id).await?;
    if task.status() == Status::Done {
        return Err(format!(
            "ERROR: task {} is already done. Reopen it first (neurostrata_task_update with status 'open') if there is more work.",
            id
        ));
    }

    let reason = arg_str(&args, "reason").unwrap_or("").to_string();
    let link_memory_id = arg_str(&args, "link_memory_id").map(|s| s.to_string());
    let memory = args.get("memory").cloned();
    let actor = task
        .task_field("assignee")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let list_now = || async {
        store
            .list(&namespace, None)
            .await
            .map_err(|e| format!("Failed to list memories in '{}': {}", namespace, e))
    };

    // Path 1: the edge already exists (a memory written earlier with
    // `extracted_from: [id]`).
    let mut memories = list_now().await?;
    let mut edge = gate::extraction_exists(&memories, &id);

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
            let (existing_vector, mut payload) =
                match store.get(&namespace, link_id).await {
                    Ok(Some(found)) => found,
                    Ok(None) => {
                        return Err(format!(
                            "ERROR: no memory with id '{}' in namespace '{}' to link.",
                            link_id, namespace
                        ))
                    }
                    Err(e) => {
                        return Err(format!("ERROR: could not read memory '{}': {}", link_id, e))
                    }
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
                    format!("Failed to link memory '{}' to task '{}': {}", link_id, id, e)
                })?;
            // Verified after the write: the rule is an edge *after the call*.
            memories = list_now().await?;
            edge = gate::extraction_exists(&memories, &id);
        }
    }

    // Path 3: write the extraction inline, through the same add_memory
    // pipeline (embed, secret scan, dedup check, upsert) with the edge stamped
    // on the way in.
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
            let mut meta = json!({ "extracted_from": [id.as_str()] });
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
            // An extraction is an insertion: it meets the same scanner.
            if let Some(rejection) =
                crate::secrets::scan_entry_point(&content, &meta, "task_complete")
            {
                return Err(rejection.to_string());
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
            crate::server::embed_dedup_and_upsert(
                &store,
                &emb,
                &namespace,
                &payload,
                dedup.as_ref(),
            )
            .await?;
            memories = list_now().await?;
            edge = gate::extraction_exists(&memories, &id);
        }
    }

    if !edge {
        return Err(blocked_message(&id));
    }

    // With the edge in place the ExtractionRequired guard is satisfied.
    let ctx = Ctx {
        reason,
        extraction_edge_exists: true,
        actor,
        session: String::new(),
    };
    let completed = apply(&task, Status::Done, &ctx).map_err(|e| format!("ERROR: {}", e))?;
    save_task(&store, &namespace, &id, vector, completed.payload.clone()).await?;

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

    serde_json::to_string_pretty(&json!({
        "task": task_summary(&id, &completed.payload),
        "extracted_memory_ids": extracted,
        "transition": { "from": task.status().as_str(), "to": "done" },
    }))
    .map_err(|e| format!("Internal serialization error: {}", e))
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
    match gate::run(&store, &namespace).await {
        Ok(report) => encode(&report.validate_json()),
        Err(e) => format!("ERROR: could not evaluate the task gate: {}", e),
    }
}

// ── snapshot stickiness (section 6.1) ──────────────────────────────────────

/// The header `get_snapshot` prepends: the Zero-Action Start line plus the
/// claimed and ready work. Every session starts inside the task system
/// because the mandatory pre-flight tool carries it.
pub fn snapshot_prefix(memories: &[SearchResult]) -> String {
    let statuses = task_statuses(memories);
    let mut claimed: Vec<&SearchResult> = Vec::new();
    let mut ready: Vec<&SearchResult> = Vec::new();
    for m in memories {
        if !gate::is_task(m) {
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

## 1. Zero-Action Start (mandatory)
No code or file edits until a task exists AND is claimed by this session:
1. `neurostrata_task_list` with `ready: true` to pick up existing work, or
2. `neurostrata_task_create` (namespace, title) then `neurostrata_task_claim`
   (id, namespace, assignee, session_id).
Never use TodoWrite, TaskCreate, or markdown TODO lists: tasks live in
NeuroStrata, not in the repo.

## 2. Session completion (all three, in order)
1. `neurostrata_task_complete` on every claimed task. It is the only entrance
   to `done`, and it forces extraction: pass `memory.content` (what did this
   task teach? what rule does it prove?) or `link_memory_id`.
2. `neurostrata-mcp task gate {ns}` must exit 0.
3. `git pull --rebase` then `git push` (the pre-push hook runs the gate).

## 3. Mid-task state
Progress notes go on the task: `neurostrata_task_update` with `note` appends
to history. The task record IS Tier-3 task memory -- recover a session with
`neurostrata_task_list` + `neurostrata_get_memory`, not log archaeology.

## 4. Memory
Architectural rules, decisions, and fixes -> `neurostrata_add_memory`.
Pre-flight every new task with `neurostrata_get_snapshot` first.
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

    let instructions = json!([
        { "step": 1, "action": "write_file", "path": "AGENTS.md" },
        { "step": 2, "action": "run", "cmd": "neurostrata-mcp hooks install" },
        { "step": 3, "action": "call_tool", "tool": "neurostrata_task_claim",
          "params": { "id": task_id.as_str(), "namespace": namespace.as_str(),
                      "assignee": "this-session", "session_id": "this-session" } },
        { "step": 4, "action": "call_tool", "tool": "neurostrata_ingest_directory",
          "params": { "dir_path": project_root, "namespace": namespace.as_str() } },
    ]);

    encode(&json!({
        "namespace": namespace,
        "files": [
            { "path": "AGENTS.md", "overwrite": false, "content": agents_template(&namespace) },
            { "path": ".NeuroStrata/docs/.gitkeep", "overwrite": false, "content": "" }
        ],
        "first_task": { "id": task_id, "title": title },
        "hooks": { "install_command": "neurostrata-mcp hooks install" },
        "instructions": instructions,
        "rule": "Zero-Action Start is now active: no file edits without a claimed task.",
    }))
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
    let config = std::fs::read_to_string(root.join(".git").join("config")).ok().or_else(|| {
        let gitfile = std::fs::read_to_string(root.join(".git")).ok()?;
        let gitdir_line = gitfile.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
        let gitdir = std::path::PathBuf::from(gitdir_line.trim());
        let gitdir = if gitdir.is_absolute() { gitdir } else { root.join(gitdir) };
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
    let mut counts: std::collections::HashMap<&'static str, usize> = std::collections::HashMap::new();
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
    scan.language_counts.sort_by(|a, b| b.1.cmp(&a.1));
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

    if let Ok(entries) = std::fs::read_dir(root.join(".git/hooks")) {
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
        store, emb, namespace, title, "unknown", None, task_meta, Map::new(),
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

    let scan = detect_project(root);
    let mut conflicts: Vec<Value> = Vec::new();
    let mut tasks_created: Vec<Value> = Vec::new();
    let mut instructions: Vec<Value> = Vec::new();
    let mut step = 1;

    // One of the two hooks install runs, always first (section 8): the gate
    // is what makes the system live on the first push.
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
    instructions.push(json!({ "step": step, "action": "run", "cmd": hook_cmd }));
    step += 1;

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
            .unwrap_or(0);
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
                instructions.push(json!({ "step": step, "action": "run", "cmd": import_cmd }));
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
        if let Ok(vec) = emb.embed(&content).await {
            if let Ok(hits) = store.search(&namespace, vec, 3).await {
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
        .list(namespace, None)
        .await
        .map_err(|e| format!("ERROR: could not list '{}': {}", namespace, e))?;
    let mut seen_bead_ids: HashSet<String> = rows
        .iter()
        .filter_map(|r| {
            r.payload
                .metadata
                .get("task")
                .and_then(|t| t.get("bead_id"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
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
            close_reason: value.get("close_reason").and_then(|v| v.as_str()).map(String::from),
            created_at: value.get("created_at").and_then(|v| v.as_str()).map(String::from),
            updated_at: value.get("updated_at").and_then(|v| v.as_str()).map(String::from),
            created_by: value.get("created_by").and_then(|v| v.as_str()).map(String::from),
        });
    }

    // First pass: allocate ids for what is new, skip what bead_id already
    // covers (the idempotency key from section 7).
    let key = ns_key(namespace);
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut allocations: Vec<Option<String>> = Vec::with_capacity(pending.len());
    for p in &pending {
        if seen_bead_ids.contains(&p.bead_id) {
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
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::ladybug::LadybugStore::new(&dir, 4).expect("open temp database"));
        store.init("global").await.expect("create the schema");
        store
    }

    fn emb() -> Arc<dyn Embedder> {
        Arc::new(StubEmbedder)
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
        let parsed: Value =
            serde_json::from_str(&reply).unwrap_or_else(|e| panic!("create failed: {} -- {}", reply, e));
        parsed["id"].as_str().expect("id").to_string()
    }

    async fn status_of(store: &Arc<dyn VectorStore>, namespace: &str, id: &str) -> Status {
        let (_, payload) = store.get(namespace, id).await.unwrap().expect("task exists");
        Task { id: id.to_string(), payload }.status()
    }

    #[test]
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
        assert!(rules.iter().all(|r| r["heuristic"] == true && r["verified"] == false));
        let _ = std::fs::remove_dir_all(&root);
    }

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
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()), "{}", suffix);
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
        assert!(reply.starts_with("ERROR: namespace 'NewProj' does not exist"), "{}", reply);
        assert!(reply.contains("neurostrata_bootstrap"), "{}", reply);
        assert!(
            store.list_namespaces().await.unwrap().is_empty(),
            "a refused create must not invent the namespace"
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
        assert!(payload.metadata["task"]["created_at"].as_str().unwrap().ends_with('Z'));
        assert!(payload.metadata["task"]["history"].as_array().unwrap().is_empty());
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
        assert!(reply.starts_with("ERROR: Illegal transition blocked -> in_progress"), "{}", reply);
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
        assert!(reply.contains("done only via neurostrata_task_complete"), "{}", reply);
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

    #[tokio::test]
    async fn re_claiming_from_the_same_session_is_idempotent() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        let args = json!({ "namespace": "MyProj", "id": id, "assignee": "agent-a", "session_id": "ses_a" });

        handle_task_claim(args.clone(), store.clone()).await;
        let again = handle_task_claim(args, store.clone()).await;
        assert!(again.contains("already claimed by this session"), "{}", again);

        let (_, payload) = store.get("MyProj", &id).await.unwrap().unwrap();
        assert_eq!(payload.metadata["task"]["history"].as_array().unwrap().len(), 1);
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
        assert!(takeover.contains("\"status\": \"in_progress\""), "{}", takeover);

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
    async fn claiming_a_done_task_names_the_legal_transition_instead() {
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
        )
        .await
        .expect_err("Lock 2 must refuse a completion with nothing extracted");

        let expected = format!(
            "BLOCKED (Lock 2): task {} cannot complete without an extracted memory. Either call neurostrata_add_memory with metadata extracted_from: [\"{}\"], or retry neurostrata_task_complete with `memory.content` (what did you learn? what rule does this task prove?) or `link_memory_id`.",
            id, id
        );
        assert_eq!(err, expected, "the -32603 body must match section 3.2 verbatim");
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::InProgress,
            "a blocked completion changes nothing"
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
        assert_eq!(kept.metadata["access_count"], json!(3), "everything else is untouched");
    }

    #[tokio::test]
    async fn path_3_inline_memory_goes_through_the_add_memory_pipeline() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;

        let before = store.list("MyProj", None).await.unwrap().len();
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
        )
        .await
        .expect("the inline extraction writes the edge");

        let parsed: Value = serde_json::from_str(&reply).expect("json reply");
        assert_eq!(parsed["task"]["status"], json!("done"));
        let extracted = parsed["extracted_memory_ids"].as_array().unwrap();
        assert_eq!(extracted.len(), 1);

        let after = store.list("MyProj", None).await.unwrap();
        assert_eq!(after.len(), before + 1, "one memory was written");
        let written = after.iter().find(|r| Some(r.id.as_str()) == extracted[0].as_str()).expect("the new memory");
        assert_eq!(written.payload.memory_type, "fact", "the schema default");
        assert_eq!(written.payload.metadata["extracted_from"], json!([id.as_str()]));
        assert_eq!(
            status_of(&store, "MyProj", &id).await,
            Status::Done,
            "the transition happened after the extraction landed"
        );
        // The inline write also carries the pipeline's own stamps.
        assert!(written.payload.metadata.get("valid_from").is_some(), "valid_from comes from the pipeline");
        assert!(written.payload.metadata.get("access_count").is_some());
    }

    #[tokio::test]
    async fn a_secret_in_the_inline_extraction_is_refused_before_any_write() {
        let store = store_with_namespace("MyProj").await;
        let id = create_task(&store, "MyProj", "the work", 2).await;
        claim(&store, &id).await;
        let before = store.list("MyProj", None).await.unwrap().len();

        let err = handle_task_complete(
            json!({
                "namespace": "MyProj",
                "id": id,
                "memory": { "content": "use ghp_aaaabbbbccccdddd to push" }
            }),
            emb(),
            store.clone(),
            None,
        )
        .await
        .expect_err("a secret must not be stored as an extraction");
        assert!(err.contains("ERROR [SECURITY]"), "{}", err);
        assert_eq!(store.list("MyProj", None).await.unwrap().len(), before);
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
        )
        .await
        .unwrap();

        let err = handle_task_complete(
            json!({ "namespace": "MyProj", "id": id, "memory": { "content": "again" } }),
            emb(),
            store.clone(),
            None,
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
        )
        .await
        .expect("extraction-backed completion");
        assert!(completion.contains("\"status\": \"done\""), "{}", completion);
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
        assert!(!ids.contains(&done_id.as_str()), "done is hidden by default");
        // Priority order: actionable (P1) before waiting (P2).
        assert_eq!(ids[0], open_id);

        let reply =
            handle_task_list(json!({ "namespace": "MyProj", "ready": true }), store.clone()).await;
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        let ids: Vec<&str> = parsed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![open_id.as_str()], "ready = open with no live blockers");

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
            task_row("myproj-aaaa", json!({ "status": "in_progress", "assignee": "agent-a" })),
            task_row("myproj-bbbb", json!({ "status": "open", "priority": 1 })),
            task_row("myproj-cccc", json!({ "status": "open", "priority": 3, "blocked_by": ["myproj-bbbb"] })),
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
        assert!(!prefix.contains("myproj-dddd"), "done work is not listed: {}", prefix);
        assert!(!prefix.contains("always use podman"), "memories stay in the snapshot body");
    }

    #[test]
    fn snapshot_prefix_says_so_when_there_is_nothing_to_pick_up() {
        let prefix = snapshot_prefix(&[]);
        assert!(
            prefix.starts_with("Zero-Action Start: claim or create a task before editing."),
            "{}",
            prefix
        );
        assert!(prefix.contains("No tasks are claimed or ready"), "{}", prefix);
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
        let (_, payload) = store.get("MyProj", &first_id).await.unwrap().expect("created");
        assert_eq!(payload.memory_type, "task");

        let files = parsed["files"].as_array().unwrap();
        assert_eq!(files[0]["path"], json!("AGENTS.md"));
        assert_eq!(files[0]["overwrite"], json!(false));
        assert!(files[0]["content"].as_str().unwrap().contains("Zero-Action Start"));
        assert_eq!(files[1]["path"], json!(".NeuroStrata/docs/.gitkeep"));

        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(instructions.len(), 4);
        assert_eq!(instructions[0]["action"], json!("write_file"));
        assert_eq!(instructions[1]["cmd"], json!("neurostrata-mcp hooks install"));
        assert_eq!(instructions[2]["tool"], json!("neurostrata_task_claim"));
        assert_eq!(instructions[2]["params"]["id"], json!(first_id));
        assert_eq!(instructions[3]["tool"], json!("neurostrata_ingest_directory"));
        assert_eq!(parsed["hooks"]["install_command"], json!("neurostrata-mcp hooks install"));
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

        assert_eq!(parsed["detected"]["git_remote"], json!("git@github.com:me/proj.git"));
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
        assert_eq!(created.len(), 2, "hook replacement + beads migration: {:?}", created);
        assert!(created[1]["title"].as_str().unwrap().contains("Migrate 2 beads issues"));
        assert!(created[1]["metadata_hint"]
            .as_str()
            .unwrap()
            .contains("neurostrata-mcp task import proj --from-beads .beads/issues.jsonl"));

        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(instructions[0]["cmd"], json!("neurostrata-mcp hooks install --force"));
        assert!(instructions.iter().any(|i| {
            i["cmd"].as_str()
                .map(|c| c.starts_with("neurostrata-mcp task import"))
                .unwrap_or(false)
        }), "the import step is instructed: {:?}", instructions);
        assert!(instructions.iter().any(|i| i["tool"] == json!("neurostrata_add_memory")));
        assert!(instructions.iter().any(|i| i["tool"] == json!("neurostrata_task_complete")),
            "completing the setup tasks forces the first extractions: {:?}", instructions);

        // The tasks really exist, in creation order.
        let rows = store.list("proj", None).await.unwrap();
        let tasks: Vec<_> = rows.iter().filter(|r| r.payload.memory_type == "task").collect();
        assert_eq!(tasks.len(), 2);
        for created in created {
            let id = created["id"].as_str().unwrap();
            assert!(tasks.iter().any(|t| t.id == id), "{} was announced but not created", id);
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn setup_on_a_clean_project_suggests_the_plain_hook_and_no_conflicts() {
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

        assert_eq!(parsed["conflicts"], json!([]));
        assert_eq!(parsed["tasks_created"], json!([]));
        assert_eq!(parsed["instructions"][0]["cmd"], json!("neurostrata-mcp hooks install"));
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
        assert_eq!(summary.malformed_lines, 1, "the broken line is counted, not fatal");

        let rows = store.list("proj", None).await.unwrap();
        let tasks: Vec<_> = rows.iter().filter(|r| r.payload.memory_type == "task").collect();
        assert_eq!(tasks.len(), 2);

        let closed = tasks
            .iter()
            .find(|t| t.payload.metadata["task"]["bead_id"] == json!("strata-1"))
            .expect("the closed issue was imported");
        assert_eq!(closed.payload.content, "Fix the gate");
        assert_eq!(closed.payload.metadata["task"]["status"], json!("done"));
        assert_eq!(closed.payload.metadata["task"]["grandfathered"], json!(true));
        assert_eq!(closed.payload.metadata["task"]["close_reason"], json!("Fixed"));
        assert_eq!(closed.payload.metadata["task"]["priority"], json!(0));
        assert_eq!(closed.payload.metadata["task"]["task_type"], json!("bug"));
        assert_eq!(closed.payload.metadata["task"]["description"], json!("the gate wedged on push"));
        assert_eq!(
            closed.payload.metadata["task"]["created_at"],
            json!("2026-05-25T16:54:42Z"),
            "timestamps are preserved verbatim"
        );
        assert_eq!(closed.payload.user_id, "neo", "created_by becomes the record's user_id");
        // blocked_by is translated into this system's ids (see below).
        let blocked = closed.payload.metadata["task"]["blocked_by"].as_array().unwrap();
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

        let rows = store.list("proj", None).await.unwrap();
        let tasks: Vec<_> = rows.iter().filter(|r| r.payload.memory_type == "task").collect();
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
}
