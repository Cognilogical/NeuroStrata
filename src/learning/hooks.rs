//! Capability E production hooks — wire the deterministic miner and the
//! outcome-verified reflection core into the two committed lifecycle points.
//!
//! This is the ONLY place that persists learned state. It never completes a
//! task and never applies/reverts an action, so hook persistence cannot recurse
//! into the lifecycle that invoked it. Every write goes through
//! `LadybugStore::commit_action_batch` (one single-writer turn + expected-head
//! CAS + privacy/ReBAC-aware staged writes). Failures are collected into a
//! report and logged; they never roll back an already-committed task/action and
//! are never reported as reflection success.

use crate::judgment::laya_onnx::LayaOnnxProvider;
use crate::judgment::{DeduplicationChecker, JudgmentProvider};
use crate::learning::reflection::{
    apply_negative_outcome, apply_positive_outcome, ContradictionCheckRequest, NegativeKind,
};
use crate::learning::trajectory::{
    candidate_to_action_type, candidate_to_memory, is_receipt_reverted, mine_procedures,
    TaskEvidence, TrajectoryStep,
};
use crate::rdf::action::{
    current_action_timestamp, is_active_payload, ActionApplyOutcome, ActionError,
    ActionReceiptPayload, ActionRevertOutcome, ActionTypeDefinition, ActionWriteSet, ExpectedHead,
    StagedMemoryWrite,
};
use crate::rdf::scenario::{fingerprint_live_row, sha256_fingerprint};
use crate::store::ladybug::LadybugStore;
use crate::task::machine::Status;
use crate::traits::{Embedder, MemoryPayload, SearchResult, VectorStore};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;

const MAX_CAS_RETRIES: usize = 5;
const MINING_NGRAM: usize = 1;
const FALLBACK_CONFIDENCE_THRESHOLD: f32 = 0.8;

/// Result of the post-completion hook. Surfaced so a silent failure is
/// impossible to mistake for success.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TaskCompletionHookReport {
    pub task_id: String,
    pub evidence_tasks: usize,
    pub candidates_persisted: usize,
    pub action_candidates_surfaced: usize,
    pub positive_reflections: usize,
    pub negative_reflections: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ActionApplyHookReport {
    pub receipt_id: String,
    pub linked_completed_task: bool,
    pub positive_reflections: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ActionRevertHookReport {
    pub revert_receipt_id: String,
    pub negative_reflections: usize,
    pub contradictions_queued: usize,
    pub errors: Vec<String>,
}

fn err(errors: &mut Vec<String>, context: &str, e: impl std::fmt::Display) {
    let msg = format!("{context}: {e}");
    tracing::warn!("{msg}");
    errors.push(msg);
}

// ── Real-evidence collection (pure, deterministic) ───────────────────────────

fn string_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn receipt_mentions_task(receipt: &ActionReceiptPayload, task_id: &str) -> bool {
    receipt.task_deltas.iter().any(|d| d.task_id == task_id)
        || receipt.params.get("task_id").and_then(Value::as_str) == Some(task_id)
}

/// Recorded trigger token. Only an explicitly recorded, governed value is used;
/// an absent or malformed value stays empty so `normalize_trigger` rejects the
/// pattern (fail closed, never fabricated).
fn recorded_trigger(task: &MemoryPayload, receipts: &[ActionReceiptPayload]) -> String {
    let task_obj = task.metadata.get("task");
    for v in [
        task.metadata.get("trigger"),
        task_obj.and_then(|t| t.get("trigger")),
    ] {
        if let Some(s) = v.and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return s.trim().to_string();
            }
        }
    }
    for r in receipts {
        if let Some(s) = r.params.get("trigger").and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return s.trim().to_string();
            }
        }
    }
    String::new()
}

fn recorded_verification(
    task: &MemoryPayload,
    receipts: &[ActionReceiptPayload],
) -> Option<String> {
    let task_obj = task.metadata.get("task");
    for v in [
        task.metadata.get("verification_gate"),
        task_obj.and_then(|t| t.get("verification_gate")),
    ] {
        if let Some(s) = v.and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            return Some(s.trim().to_string());
        }
    }
    for r in receipts {
        for key in ["verification", "verification_gate", "gate"] {
            if let Some(s) = r
                .params
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

/// Build TaskEvidence for every completed task from existing store rows only.
/// Reverted receipt versions are kept visible (their active row carries
/// `reverted_at`), so the miner's `has_only_clean_receipts` can exclude them.
pub fn collect_evidence(rows: &[SearchResult]) -> Vec<TaskEvidence> {
    let now = current_action_timestamp();
    let receipt_for: Vec<(&SearchResult, ActionReceiptPayload)> = rows
        .iter()
        .filter(|r| r.payload.memory_type == "action_receipt" && is_active_payload(&r.payload, now))
        .filter_map(|r| {
            ActionReceiptPayload::from_payload(&r.payload)
                .ok()
                .map(|p| (r, p))
        })
        .collect();

    let mut out = Vec::new();
    for row in rows.iter().filter(|r| r.payload.memory_type == "task") {
        let status = row
            .payload
            .metadata
            .get("task")
            .and_then(|t| t.get("status"))
            .and_then(Value::as_str)
            .and_then(Status::parse);
        if status != Some(Status::Done) || !is_active_payload(&row.payload, now) {
            continue;
        }
        let task_id = &row.id;
        let extracted_memory_ids: BTreeSet<String> = rows
            .iter()
            .filter(|m| {
                string_array(m.payload.metadata.get("extracted_from"))
                    .iter()
                    .any(|t| t == task_id)
            })
            .map(|m| m.id.clone())
            .collect();

        let mut related: Vec<(i64, String, ActionReceiptPayload)> = receipt_for
            .iter()
            .filter(|(_, r)| receipt_mentions_task(r, task_id))
            .map(|(_, r)| (r.executed_at, r.receipt_id.clone(), r.clone()))
            .collect();
        related.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let receipts: Vec<ActionReceiptPayload> = related.into_iter().map(|(_, _, r)| r).collect();
        let trigger = recorded_trigger(&row.payload, &receipts);
        let verification = recorded_verification(&row.payload, &receipts);
        let steps: Vec<TrajectoryStep> = receipts
            .iter()
            .map(|r| TrajectoryStep {
                trigger_context: trigger.clone(),
                tool_or_action_sequence: vec![r.action_slug.clone()],
                verification_gate: verification.clone(),
            })
            .collect();
        out.push(TaskEvidence {
            task_id: task_id.clone(),
            status: Status::Done,
            extracted_memory_ids: extracted_memory_ids.into_iter().collect(),
            receipts,
            steps,
        });
    }
    out.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    out
}

/// The autonomous learner persists via CAS as `admin`, but must not mutate a
/// governance row that a foreign principal has explicitly privatized with an
/// owner ACL. ReBAC's admin bypass cannot express that, so check the ACL too.
fn learner_writable(payload: &MemoryPayload) -> bool {
    if !crate::rebac::can_write_memory("admin", &payload.user_id, &payload.metadata) {
        return false;
    }
    let owner = crate::rebac::extract_resource_owner(&payload.user_id, &payload.metadata);
    !crate::rebac::extract_acl_with_roles(&payload.metadata)
        .iter()
        .any(|(principal, role)| {
            role == "owner"
                && principal != "*"
                && principal != "admin"
                && principal != "system"
                && principal != &owner
        })
}

/// Genuine tool receipts are real outcomes (`success`/`empty`) from an
/// allowlisted graph tool, attributed to a task. The digest-only `events[]`
/// schema never stores rule/procedure ids verbatim, so reference evidence can
/// only come from the id-bearing fields of such receipts (panel C50). Legacy
/// `queries[]` rows and failed/unlisted/unattributed events are not evidence.
fn referenced_ids_from_ledger(
    session_queries_text: &str,
    task_id: Option<&str>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if session_queries_text.is_empty() {
        return out;
    }
    let Ok(ledger) = serde_json::from_str::<Value>(session_queries_text) else {
        return out;
    };
    let Some(events) = ledger.get("events").and_then(Value::as_array) else {
        return out;
    };
    for event in events {
        let status = event.get("status").and_then(Value::as_str).unwrap_or("");
        if status != "success" && status != "empty" {
            continue;
        }
        let tool = event.get("tool").and_then(Value::as_str).unwrap_or("");
        if !crate::task::snapshot_readiness::GRAPH_TOOLS.contains(&tool) {
            continue;
        }
        let attributed = event.get("task_id").and_then(Value::as_str).unwrap_or("");
        if attributed.is_empty() {
            continue;
        }
        if let Some(tid) = task_id {
            if attributed != tid {
                continue;
            }
        }
        for key in [
            "returned_ids",
            "prescribed_action_ids",
            "followed_action_ids",
        ] {
            if let Some(ids) = event.get(key).and_then(Value::as_array) {
                for id in ids
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    out.insert(id.to_string());
                }
            }
        }
    }
    out
}

/// Rule/procedure ids actually referenced by task evidence, the task's
/// subagent prompt injection, genuine `events[]` receipts for `task_id`, or an
/// action receipt. Only active, writable rows of the two governed memory types
/// are eligible.
pub fn collect_reflection_targets(
    rows: &[SearchResult],
    task_payload: Option<&MemoryPayload>,
    receipt_payloads: &[&MemoryPayload],
    session_queries_text: &str,
    task_id: Option<&str>,
) -> BTreeSet<String> {
    let now = current_action_timestamp();
    let eligible: Vec<&SearchResult> = rows
        .iter()
        .filter(|r| {
            matches!(r.payload.memory_type.as_str(), "rule" | "procedure")
                && is_active_payload(&r.payload, now)
                && learner_writable(&r.payload)
        })
        .collect();

    let mut mentioned: BTreeSet<String> = BTreeSet::new();
    if let Some(tp) = task_payload {
        for id in string_array(tp.metadata.get("extracted_from")) {
            mentioned.insert(id);
        }
        let injection = tp
            .metadata
            .get("task")
            .and_then(|t| t.get("subagent_prompt_injection"))
            .and_then(Value::as_str)
            .unwrap_or("");
        for r in &eligible {
            if !injection.is_empty() && injection.contains(&r.id) {
                mentioned.insert(r.id.clone());
            }
        }
    }
    mentioned.extend(referenced_ids_from_ledger(session_queries_text, task_id));
    for rp in receipt_payloads {
        // Governing links live inside `metadata.action_receipt` on real rows;
        // also read the top level so plain reference payloads still count.
        for source in std::iter::once(&rp.metadata).chain(rp.metadata.get("action_receipt")) {
            for key in [
                "governs",
                "related_to",
                "rule_ids",
                "procedure_ids",
                "motivated_by",
                "created_ids",
            ] {
                for id in string_array(source.get(key)) {
                    mentioned.insert(id);
                }
            }
        }
    }

    eligible
        .into_iter()
        .filter(|r| mentioned.contains(&r.id))
        .map(|r| r.id.clone())
        .collect()
}

fn session_queries_text(project_root: &Path) -> String {
    std::fs::read_to_string(
        project_root
            .join(".NeuroStrata")
            .join("session_queries.json"),
    )
    .unwrap_or_default()
}

/// Resolve a real receipt row by id, derive the governed rule/procedure ids it
/// references, and keep only those the completing task already reflected. The
/// exemplar row's own id is deliberately never used as a reflection target.
fn receipt_reflection_targets(
    rows: &[SearchResult],
    receipt_id: &str,
    task_targets: &BTreeSet<String>,
) -> BTreeSet<String> {
    let receipt_payloads: Vec<&MemoryPayload> = rows
        .iter()
        .filter(|r| {
            r.payload.memory_type == "action_receipt"
                && r.payload
                    .metadata
                    .get("action_receipt")
                    .and_then(|a| a.get("receipt_id"))
                    .and_then(Value::as_str)
                    == Some(receipt_id)
        })
        .map(|r| &r.payload)
        .collect();
    if receipt_payloads.is_empty() {
        return BTreeSet::new();
    }
    collect_reflection_targets(rows, None, &receipt_payloads, "", None)
        .into_iter()
        .filter(|id| task_targets.contains(id))
        .collect()
}

// ── CAS-backed persistence ───────────────────────────────────────────────────

/// Read-modify-write one row under the store's single-writer turn, guarded by a
/// RowFingerprint CAS. A retired/superseded row collapses to the absent
/// fingerprint and fails the head check, so history is never mutated in place.
async fn cas_update_memory(
    store: &LadybugStore,
    namespace: &str,
    row_id: &str,
    mutate: impl Fn(&mut MemoryPayload) -> std::result::Result<bool, String>,
) -> std::result::Result<bool, String> {
    for _ in 0..MAX_CAS_RETRIES {
        let (vector, mut payload) = store
            .get(namespace, row_id)
            .await
            .map_err(|e| format!("read '{row_id}': {e}"))?
            .ok_or_else(|| format!("row '{row_id}' vanished before CAS"))?;
        if !payload.metadata.is_object() {
            return Err(format!("row '{row_id}' carries non-object metadata"));
        }
        let now = current_action_timestamp();
        if !is_active_payload(&payload, now) {
            return Err(format!("row '{row_id}' is retired/superseded"));
        }
        let expected_fingerprint = fingerprint_live_row(Some(&payload), now);
        let before = payload.metadata.clone();
        if !mutate(&mut payload)? || payload.metadata == before {
            return Ok(false);
        }
        let batch = ActionWriteSet {
            expected_heads: vec![ExpectedHead::RowFingerprint {
                row_id: row_id.to_string(),
                expected_fingerprint,
            }],
            writes: vec![StagedMemoryWrite {
                id: row_id.to_string(),
                vector,
                payload,
            }],
            edge_deltas: Vec::new(),
        };
        match store.commit_action_batch(namespace, batch).await {
            Ok(()) => return Ok(true),
            Err(ActionError::ConflictWithNewerVersion { .. }) => continue,
            Err(e) => return Err(format!("CAS commit '{row_id}': {e}")),
        }
    }
    Err(format!(
        "CAS contention on '{row_id}' after {MAX_CAS_RETRIES} retries"
    ))
}

fn privacy_metadata(principal: &str, source: &str) -> Value {
    json!({
        "eligibility": "private",
        "visible_to": [{ "principal": principal, "role": "owner" }],
        "source_context": source,
    })
}

fn merge_privacy(payload: &mut MemoryPayload, principal: &str, source: &str) {
    if let (Some(obj), Some(extra)) = (
        payload.metadata.as_object_mut(),
        privacy_metadata(principal, source).as_object().cloned(),
    ) {
        for (k, v) in extra {
            obj.entry(k).or_insert(v);
        }
    }
}

/// Persist an accepted procedure candidate as a NEW row guarded by an
/// AbsentOrInactive head. A duplicate delivery sees the existing active row and
/// is a no-op; a foreign-owned id is never clobbered.
async fn persist_procedure_candidate(
    store: &LadybugStore,
    emb: &dyn Embedder,
    namespace: &str,
    mut payload: MemoryPayload,
) -> std::result::Result<bool, String> {
    let id = format!("learn-proc-{}", &sha256_fingerprint(&payload)[..24]);
    let principal = "admin".to_string();
    merge_privacy(&mut payload, &principal, "capability-e:task-completion");
    if let Some((_, existing)) = store
        .get(namespace, &id)
        .await
        .map_err(|e| format!("candidate pre-read: {e}"))?
    {
        if !learner_writable(&existing) {
            return Err(format!(
                "candidate id '{id}' is owned by a foreign principal"
            ));
        }
        if is_active_payload(&existing, current_action_timestamp()) {
            return Ok(false); // already persisted (idempotent)
        }
    }
    let vector = emb
        .embed(&payload.content)
        .await
        .map_err(|e| format!("candidate embed: {e}"))?;
    let batch = ActionWriteSet {
        expected_heads: vec![ExpectedHead::AbsentOrInactive { row_id: id.clone() }],
        writes: vec![StagedMemoryWrite {
            id,
            vector,
            payload,
        }],
        edge_deltas: Vec::new(),
    };
    match store.commit_action_batch(namespace, batch).await {
        Ok(()) => Ok(true),
        Err(ActionError::ConflictWithNewerVersion { .. }) => Ok(false),
        Err(e) => Err(format!("candidate commit: {e}")),
    }
}

/// Deterministic id for an inert action-candidate row. Kept as a helper so the
/// persistence path and its regressions address the exact same row.
fn action_candidate_id(definition: &ActionTypeDefinition, source_task_ids: &[String]) -> String {
    let probe = MemoryPayload {
        content: format!("{}|{}", definition.slug, source_task_ids.join(",")),
        user_id: "admin".to_string(),
        memory_type: "action_type_candidate".to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: json!({}),
    };
    format!(
        "learn-action-candidate-{}",
        &sha256_fingerprint(&probe)[..24]
    )
}

/// Surface a mined action definition only as an INERT candidate row. It is never
/// written as `action_type`, never defined, and never applied. The definition is
/// run through the same definition gate and strict SHACL post-image check used
/// for real action types, so an invalid candidate is rejected before any write.
/// The pre-read mirrors `persist_procedure_candidate`: an active row is an
/// idempotent no-op and a foreign-owned row is never clobbered.
async fn surface_action_candidate(
    store: &LadybugStore,
    namespace: &str,
    definition: &ActionTypeDefinition,
    source_task_ids: &[String],
    dimensions: usize,
) -> std::result::Result<bool, String> {
    let violations = definition.validate_definition();
    if !violations.is_empty() {
        return Err(format!(
            "action candidate definition invalid: {}",
            violations
                .iter()
                .map(|v| format!("{}: {}", v.field, v.why))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    let id = action_candidate_id(definition, source_task_ids);
    let principal = "admin".to_string();
    let mut payload = MemoryPayload {
        content: format!(
            "Inert candidate action definition '{}' (not defined)",
            definition.slug
        ),
        user_id: principal.clone(),
        memory_type: "action_type_candidate".to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: json!({
            "action_type_candidate": definition,
            "candidate_only": true,
            "auto_defined": false,
            "source_task_ids": source_task_ids,
        }),
    };
    merge_privacy(&mut payload, &principal, "capability-e:task-completion");
    let shacl_violations = store
        .get_ontology(namespace)
        .validate_strict_post_image(&payload);
    if !shacl_violations.is_empty() {
        return Err(format!(
            "action candidate SHACL invalid: {}",
            shacl_violations
                .iter()
                .map(|v| format!("{}: {}", v.field, v.why))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some((_, existing)) = store
        .get(namespace, &id)
        .await
        .map_err(|e| format!("action candidate pre-read: {e}"))?
    {
        if !learner_writable(&existing) {
            return Err(format!(
                "action candidate id '{id}' is owned by a foreign principal"
            ));
        }
        if is_active_payload(&existing, current_action_timestamp()) {
            return Ok(false);
        }
    }
    let batch = ActionWriteSet {
        expected_heads: vec![ExpectedHead::AbsentOrInactive { row_id: id.clone() }],
        writes: vec![StagedMemoryWrite {
            id,
            // Inert (never searched by meaning), but the row still lands in a
            // fixed FLOAT[N] column built at the embedder's configured width;
            // a zero vector of any other length is rejected by the engine.
            vector: vec![0.0f32; dimensions],
            payload,
        }],
        edge_deltas: Vec::new(),
    };
    match store.commit_action_batch(namespace, batch).await {
        Ok(()) => Ok(true),
        Err(ActionError::ConflictWithNewerVersion { .. }) => Ok(false),
        Err(e) => Err(format!("action candidate commit: {e}")),
    }
}

// ── Hook: task completion (after Lock 2 success and task commit) ─────────────

/// Testable entry point with an injected judgment provider.
pub async fn on_task_completed_with_judge(
    store: &Arc<dyn VectorStore>,
    emb: &Arc<dyn Embedder>,
    namespace: &str,
    task_id: &str,
    judge: Option<Arc<dyn JudgmentProvider>>,
    confidence_threshold: f32,
    project_root: &Path,
) -> TaskCompletionHookReport {
    let mut report = TaskCompletionHookReport {
        task_id: task_id.to_string(),
        ..Default::default()
    };
    let Some(ls) = VectorStore::as_any(store.as_ref()).downcast_ref::<LadybugStore>() else {
        err(
            &mut report.errors,
            "task hook",
            "store is not a LadybugStore",
        );
        return report;
    };
    let rows = match store.list(namespace, None, None).await {
        Ok(r) => r,
        Err(e) => {
            err(&mut report.errors, "task hook list", e);
            return report;
        }
    };
    let evidence = collect_evidence(&rows);
    report.evidence_tasks = evidence.len();
    let now_ts = current_action_timestamp();
    let existing_procedures: Vec<MemoryPayload> = rows
        .iter()
        .filter(|r| r.payload.memory_type == "procedure" && is_active_payload(&r.payload, now_ts))
        .map(|r| r.payload.clone())
        .collect();

    let candidates = mine_procedures(
        &evidence,
        MINING_NGRAM,
        &existing_procedures,
        judge,
        confidence_threshold,
    )
    .await;
    for candidate in &candidates {
        match persist_procedure_candidate(
            ls,
            emb.as_ref(),
            namespace,
            candidate_to_memory(candidate),
        )
        .await
        {
            Ok(true) => report.candidates_persisted += 1,
            Ok(false) => {}
            Err(e) => err(&mut report.errors, "persist procedure candidate", e),
        }
        if let Some(action_candidate) = candidate_to_action_type(candidate) {
            match surface_action_candidate(
                ls,
                namespace,
                &action_candidate.definition,
                &candidate.task_ids,
                emb.dimensions(),
            )
            .await
            {
                Ok(true) => report.action_candidates_surfaced += 1,
                Ok(false) => {}
                Err(e) => err(&mut report.errors, "surface action candidate", e),
            }
        }
    }

    let task_payload = rows
        .iter()
        .find(|r| r.id == task_id && r.payload.memory_type == "task")
        .map(|r| &r.payload);
    let receipt_payloads: Vec<&MemoryPayload> = rows
        .iter()
        .filter(|r| r.payload.memory_type == "action_receipt")
        .filter(|r| {
            ActionReceiptPayload::from_payload(&r.payload)
                .map(|p| receipt_mentions_task(&p, task_id))
                .unwrap_or(false)
        })
        .map(|r| &r.payload)
        .collect();
    let targets = collect_reflection_targets(
        &rows,
        task_payload,
        &receipt_payloads,
        &session_queries_text(project_root),
        Some(task_id),
    );
    // Mandatory task id for idempotency: a duplicate delivery cannot double-count.
    let completion_receipt = format!("task-complete:{task_id}");
    for target in &targets {
        match cas_update_memory(ls, namespace, target, |payload| {
            apply_positive_outcome(&mut payload.metadata, Some(&completion_receipt))
                .map(|_| true)
                .map_err(|e| format!("{e:?}"))
        })
        .await
        {
            Ok(true) => report.positive_reflections += 1,
            Ok(false) => {}
            Err(e) => err(&mut report.errors, "positive reflection", e),
        }
    }

    // Failed-attempt path: only when a REAL negative exemplar records a real
    // action receipt. The exemplar row id is evidence, never a reflection target:
    // resolve the receipt's governed rules/procedures and keep only those the
    // completing task actually referenced.
    let now = chrono::Utc::now();
    let failed_receipts: BTreeSet<String> = rows
        .iter()
        .filter(|r| {
            r.payload
                .metadata
                .get("attempt")
                .and_then(|a| a.get("outcome"))
                .and_then(Value::as_str)
                == Some("failed")
        })
        .filter_map(|r| {
            r.payload
                .metadata
                .get("attempt")
                .and_then(|a| a.get("action_receipt_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(String::from)
        })
        .collect();
    for receipt in failed_receipts {
        for target in receipt_reflection_targets(&rows, &receipt, &targets) {
            match cas_update_memory(ls, namespace, &target, |payload| {
                apply_negative_outcome(
                    &mut payload.metadata,
                    NegativeKind::FailedAttempt,
                    &receipt,
                    &target,
                    now,
                )
                .map(|_| true)
                .map_err(|e| format!("{e:?}"))
            })
            .await
            {
                Ok(true) => report.negative_reflections += 1,
                Ok(false) => {}
                Err(e) => err(&mut report.errors, "failed-attempt reflection", e),
            }
        }
    }
    report
}

/// Production entry point: builds the real Laya ONNX judgment provider from the
/// configured bundle (never a test double) and uses the configured threshold.
pub async fn on_task_completed(
    store: &Arc<dyn VectorStore>,
    emb: &Arc<dyn Embedder>,
    namespace: &str,
    task_id: &str,
    dedup: Option<&Arc<DeduplicationChecker>>,
    project_root: &Path,
) -> TaskCompletionHookReport {
    let threshold = dedup
        .map(|d| d.config.confidence_threshold)
        .unwrap_or(FALLBACK_CONFIDENCE_THRESHOLD);
    let judge = dedup.and_then(|d| {
        let path = PathBuf::from(&d.config.laya_bundle_path);
        if path.as_os_str().is_empty() {
            return None;
        }
        match LayaOnnxProvider::load(&path, Arc::new(Semaphore::new(4))) {
            Ok(p) => Some(Arc::new(p) as Arc<dyn JudgmentProvider>),
            Err(e) => {
                tracing::warn!(
                    "Capability E miner: cannot load Laya provider at {}: {e}",
                    path.display()
                );
                None
            }
        }
    });
    on_task_completed_with_judge(
        store,
        emb,
        namespace,
        task_id,
        judge,
        threshold,
        project_root,
    )
    .await
}

// ── Hook: kinetic apply (after commit, un-reverted, linked task completed) ────

pub async fn on_action_applied(
    store: &LadybugStore,
    namespace: &str,
    outcome: &ActionApplyOutcome,
    project_root: &Path,
) -> ActionApplyHookReport {
    let mut report = ActionApplyHookReport::default();
    let (receipt_id, receipt) = match outcome {
        ActionApplyOutcome::Applied {
            receipt_id,
            receipt,
        } => (receipt_id, receipt),
        ActionApplyOutcome::Rejected { .. } => return report, // no hook on rejected/dry runs
    };
    report.receipt_id = receipt_id.clone();
    if is_receipt_reverted(receipt) {
        return report; // positive only while un-reverted
    }
    report.linked_completed_task = receipt
        .task_deltas
        .iter()
        .any(|d| d.after_status == Status::Done.as_str());
    if !report.linked_completed_task {
        return report;
    }
    let rows = match store.list(namespace, None, None).await {
        Ok(r) => r,
        Err(e) => {
            err(&mut report.errors, "apply hook list", e);
            return report;
        }
    };
    let receipt_payloads: Vec<&MemoryPayload> = rows
        .iter()
        .filter(|r| {
            r.payload
                .metadata
                .get("action_receipt")
                .and_then(|a| a.get("receipt_id"))
                .and_then(Value::as_str)
                == Some(receipt_id.as_str())
        })
        .map(|r| &r.payload)
        .collect();
    let targets = collect_reflection_targets(
        &rows,
        None,
        &receipt_payloads,
        &session_queries_text(project_root),
        None,
    );
    for target in &targets {
        match cas_update_memory(store, namespace, target, |payload| {
            apply_positive_outcome(&mut payload.metadata, Some(receipt_id))
                .map(|_| true)
                .map_err(|e| format!("{e:?}"))
        })
        .await
        {
            Ok(true) => report.positive_reflections += 1,
            Ok(false) => {}
            Err(e) => err(&mut report.errors, "apply positive reflection", e),
        }
    }
    report
}

// ── Hook: kinetic revert (after a successful COMMIT only) ────────────────────

pub async fn on_action_reverted(
    store: &LadybugStore,
    namespace: &str,
    outcome: &ActionRevertOutcome,
) -> ActionRevertHookReport {
    on_action_reverted_with_root(
        store,
        namespace,
        outcome,
        &crate::daemon::resolve_project_root(),
    )
    .await
}

/// Testable variant with an explicit contradiction-queue root.
pub async fn on_action_reverted_with_root(
    store: &LadybugStore,
    namespace: &str,
    outcome: &ActionRevertOutcome,
    project_root: &Path,
) -> ActionRevertHookReport {
    let mut report = ActionRevertHookReport {
        revert_receipt_id: outcome.revert_receipt_id.clone(),
        ..Default::default()
    };
    let rows = match store.list(namespace, None, None).await {
        Ok(r) => r,
        Err(e) => {
            err(&mut report.errors, "revert hook list", e);
            return report;
        }
    };
    let reverted_payloads: Vec<&MemoryPayload> = rows
        .iter()
        .filter(|r| {
            r.payload
                .metadata
                .get("action_receipt")
                .and_then(|a| a.get("receipt_id"))
                .and_then(Value::as_str)
                == Some(outcome.reverted_receipt_id.as_str())
        })
        .map(|r| &r.payload)
        .collect();
    let targets = collect_reflection_targets(&rows, None, &reverted_payloads, "", None);
    let queue = crate::judgment::laya_onnx::ContradictionQueue::new(project_root);
    let now = chrono::Utc::now();
    let mut reflected = BTreeSet::new();
    for target in &targets {
        match cas_update_memory(store, namespace, target, |payload| {
            apply_negative_outcome(
                &mut payload.metadata,
                NegativeKind::Revert,
                &outcome.revert_receipt_id,
                target,
                now,
            )
            .map(|_| true)
            .map_err(|e| format!("{e:?}"))
        })
        .await
        {
            Ok(true) => {
                report.negative_reflections += 1;
                reflected.insert(target.clone());
            }
            Ok(false) => {}
            Err(e) => err(&mut report.errors, "negative reflection", e),
        }
    }
    // Only targets whose negative CAS update actually succeeded get a durable
    // contradiction request. A no-op/failed CAS must not enqueue one.
    for target in &reflected {
        let cc = ContradictionCheckRequest {
            target_memory_id: target.clone(),
            action_receipt_id: Some(outcome.revert_receipt_id.clone()),
            reason: format!("kinetic revert of receipt {}", outcome.reverted_receipt_id),
        };
        match queue.enqueue(
            &cc.target_memory_id,
            cc.action_receipt_id.as_deref(),
            &cc.reason,
            now.timestamp(),
        ) {
            Ok(true) => report.contradictions_queued += 1,
            Ok(false) => {}
            Err(e) => err(&mut report.errors, "enqueue contradiction", e),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judgment::DuplicateJudgment;
    use crate::learning::trajectory::has_only_clean_receipts;
    use crate::rdf::action::{ActionEffectTemplate, ActionTypeDefinition, TaskDelta};
    use async_trait::async_trait;
    use serde_json::json as j;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cap-e-hooks-{}-{}-{}",
            tag,
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    struct NoDup;
    #[async_trait]
    impl JudgmentProvider for NoDup {
        async fn judge_duplicates(
            &self,
            _new: &MemoryPayload,
            _candidates: &[SearchResult],
        ) -> anyhow::Result<Vec<DuplicateJudgment>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn name(&self) -> &str {
            "no-dup"
        }
    }

    fn memory(id: &str, mt: &str, metadata: Value) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score: 0.0,
            payload: MemoryPayload {
                content: format!("{mt} {id}"),
                user_id: "system".to_string(),
                memory_type: mt.to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata,
            },
            evidence: None,
        }
    }

    fn task_row(id: &str, extracted: &[&str]) -> SearchResult {
        memory(
            id,
            "task",
            j!({ "task": { "status": "done", "trigger": "before-edit" },
                 "extracted_from": extracted }),
        )
    }

    fn receipt_row(id: &str, task_id: &str, slug: &str, reverted: bool) -> SearchResult {
        let ar = ActionReceiptPayload {
            receipt_id: id.to_string(),
            action_slug: slug.to_string(),
            principal: "agent".to_string(),
            executed_at: 1_700_000_000,
            params: Default::default(),
            superseded_pairs: vec![],
            created_ids: vec![],
            twin_deltas: vec![],
            edge_deltas: vec![],
            row_deltas: vec![],
            task_deltas: vec![TaskDelta {
                task_id: task_id.to_string(),
                before_status: "in_progress".to_string(),
                after_status: "done".to_string(),
                before_payload: memory("b", "task", j!({})).payload,
                after_payload: memory("a", "task", j!({})).payload,
            }],
            reverted_at: if reverted { Some(1_700_000_001) } else { None },
            reverted_by_receipt_id: if reverted {
                Some(format!("rev-{id}"))
            } else {
                None
            },
            reverts_receipt_id: None,
            forced_by: None,
            forced_at: None,
        };
        memory(
            id,
            "action_receipt",
            j!({ "eligibility": "private", "action_receipt": ar }),
        )
    }

    async fn seed(store: &LadybugStore, namespace: &str, rows: Vec<SearchResult>) {
        let writes: Vec<StagedMemoryWrite> = rows
            .into_iter()
            .map(|r| StagedMemoryWrite {
                id: r.id,
                vector: vec![0.0f32; 4],
                payload: r.payload,
            })
            .collect();
        let heads = writes
            .iter()
            .map(|w| ExpectedHead::AbsentOrInactive {
                row_id: w.id.clone(),
            })
            .collect();
        store
            .commit_action_batch(
                namespace,
                ActionWriteSet {
                    expected_heads: heads,
                    writes,
                    edge_deltas: vec![],
                },
            )
            .await
            .expect("seed commit");
    }

    async fn embedder() -> Arc<dyn Embedder> {
        struct Dummy;
        #[async_trait]
        impl Embedder for Dummy {
            async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
                Ok(vec![0.0f32; 4])
            }
            fn dimensions(&self) -> usize {
                4
            }
        }
        Arc::new(Dummy)
    }

    #[test]
    fn evidence_is_built_from_real_rows_and_reverted_chains_stay_visible() {
        let rows = vec![
            task_row("t1", &[]),
            memory("m1", "fact", j!({ "extracted_from": ["t1"] })),
            receipt_row("r1", "t1", "do-thing", false),
            task_row("t2", &[]),
            memory("m2", "fact", j!({ "extracted_from": ["t2"] })),
            receipt_row("r2", "t2", "do-thing", true),
        ];
        let ev = collect_evidence(&rows);
        assert_eq!(ev.len(), 2);
        let t1 = ev.iter().find(|e| e.task_id == "t1").unwrap();
        assert_eq!(t1.extracted_memory_ids, vec!["m1"]);
        assert_eq!(t1.steps[0].tool_or_action_sequence, vec!["do-thing"]);
        assert_eq!(t1.steps[0].trigger_context, "before-edit");
        assert!(has_only_clean_receipts(t1));
        let t2 = ev.iter().find(|e| e.task_id == "t2").unwrap();
        assert!(!has_only_clean_receipts(t2));
    }

    #[test]
    fn reverted_chain_is_excluded_by_miner() {
        let s = TrajectoryStep {
            trigger_context: "before-edit".to_string(),
            tool_or_action_sequence: vec!["do-thing".to_string()],
            verification_gate: None,
        };
        let mk = |id: &str, reverted: bool| TaskEvidence {
            task_id: id.to_string(),
            status: Status::Done,
            extracted_memory_ids: vec![format!("{id}-m")],
            receipts: vec![ActionReceiptPayload::from_payload(
                &receipt_row(&format!("r{id}"), id, "do-thing", reverted).payload,
            )
            .unwrap()],
            steps: vec![s.clone()],
        };
        let evidence = vec![mk("1", false), mk("2", true), mk("3", false)];
        // Reverted task still appears in evidence, but miner filters it out.
        let candidates = futures::executor::block_on(mine_procedures(
            &evidence,
            1,
            &[],
            Some(Arc::new(NoDup)),
            0.8,
        ));
        assert!(
            candidates.is_empty(),
            "a reverted chain must block the pattern"
        );
    }

    #[test]
    fn reflection_targets_only_active_writable_referenced_rules() {
        let rows = vec![
            memory("rule-a", "rule", j!({})),
            memory("rule-b", "rule", j!({})),
            memory("fact-c", "fact", j!({})),
            memory(
                "private-rule",
                "rule",
                j!({ "visible_to": [{ "principal": "someone-else", "role": "owner" }] }),
            ),
        ];
        let task = memory(
            "t",
            "task",
            j!({ "extracted_from": ["rule-a", "fact-c", "private-rule"] }),
        );
        let targets = collect_reflection_targets(&rows, Some(&task.payload), &[], "", None);
        assert_eq!(targets, vec!["rule-a".to_string()].into_iter().collect());
    }

    /// Panel C50: a rule id is a legitimate reference only when a genuine
    /// `events[]` receipt for the dispatching task carries it in an id-bearing
    /// field. Raw ledger substrings (legacy `queries[]`, failed/unlisted/other
    /// task receipts) must not count.
    #[test]
    fn reflection_targets_parse_events_instead_of_ledger_substrings() {
        let ids = [
            "rule-legacy",
            "rule-failed",
            "rule-unlisted",
            "rule-other",
            "rule-good",
        ];
        let rows: Vec<SearchResult> = ids.iter().map(|id| memory(id, "rule", j!({}))).collect();
        let ledger = j!({
            "queries": [
                {"ts": 1, "tool": "neurostrata_locate", "query": "rule-legacy"}
            ],
            "events": [
                {"status": "failed", "tool": "neurostrata_locate", "task_id": "task-1", "returned_ids": ["rule-failed"]},
                {"status": "success", "tool": "bogus_tool", "task_id": "task-1", "returned_ids": ["rule-unlisted"]},
                {"status": "success", "tool": "neurostrata_memories_for_file", "task_id": "task-2", "returned_ids": ["rule-other"]},
                {"status": "success", "tool": "neurostrata_memories_for_file", "task_id": "task-1", "returned_ids": ["rule-good"]}
            ]
        })
        .to_string();

        let targets = collect_reflection_targets(&rows, None, &[], &ledger, Some("task-1"));
        assert_eq!(
            targets,
            vec!["rule-good".to_string()].into_iter().collect(),
            "only the genuine, attributed, allowlisted success receipt is a reference"
        );
    }

    #[tokio::test]
    async fn task_completion_is_idempotent_and_persists_candidate_and_positive_outcome() {
        let dir = tmp_dir("task-complete");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let emb = embedder().await;
        let store_dyn: Arc<dyn VectorStore> = Arc::new(store);

        let mut rows = Vec::new();
        for i in 1..=3 {
            let tid = format!("t{i}");
            // Task references the governed rule it actually used.
            rows.push(task_row(&tid, &["rule-gov"]));
            rows.push(receipt_row(&format!("r{i}"), &tid, "do-thing", false));
        }
        rows.push(memory(
            "rule-gov",
            "rule",
            j!({ "extracted_from": ["t1", "t2", "t3"] }),
        ));
        seed(
            VectorStore::as_any(store_dyn.as_ref())
                .downcast_ref::<LadybugStore>()
                .unwrap(),
            "ns-e",
            rows,
        )
        .await;

        let root = tmp_dir("root");
        std::fs::create_dir_all(&root).unwrap();
        let r1 = on_task_completed_with_judge(
            &store_dyn,
            &emb,
            "ns-e",
            "t1",
            Some(Arc::new(NoDup)),
            0.8,
            &root,
        )
        .await;
        assert!(r1.errors.is_empty(), "hook errors: {:?}", r1.errors);
        assert_eq!(r1.evidence_tasks, 3);
        assert_eq!(r1.candidates_persisted, 1);
        assert_eq!(r1.action_candidates_surfaced, 1);
        assert_eq!(r1.positive_reflections, 1);

        let ls = VectorStore::as_any(store_dyn.as_ref())
            .downcast_ref::<LadybugStore>()
            .unwrap();
        let (_, gov) = ls.get("ns-e", "rule-gov").await.unwrap().unwrap();
        assert_eq!(gov.metadata["outcome_stats"]["successes"], 1);

        let r2 = on_task_completed_with_judge(
            &store_dyn,
            &emb,
            "ns-e",
            "t1",
            Some(Arc::new(NoDup)),
            0.8,
            &root,
        )
        .await;
        assert_eq!(r2.candidates_persisted, 0, "candidate must be idempotent");
        assert_eq!(r2.positive_reflections, 0, "reflection must be idempotent");
        let (_, gov2) = ls.get("ns-e", "rule-gov").await.unwrap().unwrap();
        assert_eq!(gov2.metadata["outcome_stats"]["successes"], 1);
    }

    #[tokio::test]
    async fn apply_hook_positive_only_while_un_reverted_and_linked_to_completed_task() {
        let dir = tmp_dir("apply-hook");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let store_dyn: Arc<dyn VectorStore> = Arc::new(store);
        let ls = VectorStore::as_any(store_dyn.as_ref())
            .downcast_ref::<LadybugStore>()
            .unwrap();
        seed(
            ls,
            "ns-e",
            vec![memory(
                "rule-gov",
                "rule",
                j!({ "governs_link": ["rule-gov"] }),
            )],
        )
        .await;
        let root = tmp_dir("root");

        // Rejected: no hook at all.
        let rejected = ActionApplyOutcome::Rejected { failures: vec![] };
        let rep = on_action_applied(ls, "ns-e", &rejected, &root).await;
        assert_eq!(rep.positive_reflections, 0);
        assert_eq!(rep.receipt_id, "");

        // Non-completed task delta: no positive.
        let mut r = receipt_row("r1", "t1", "do-thing", false);
        if let Some(ar) = r.payload.metadata.get_mut("action_receipt") {
            if let Some(deltas) = ar.get_mut("task_deltas").and_then(|v| v.as_array_mut()) {
                if let Some(d) = deltas.first_mut() {
                    d["after_status"] = j!("in_progress");
                }
            }
        }
        let outcome = ActionApplyOutcome::Applied {
            receipt_id: "r1".to_string(),
            receipt: ActionReceiptPayload::from_payload(&r.payload).unwrap(),
        };
        let rep = on_action_applied(ls, "ns-e", &outcome, &root).await;
        assert!(!rep.linked_completed_task);
        assert_eq!(rep.positive_reflections, 0);

        // Reverted receipt: no positive even if linked to a completed task.
        let r = receipt_row("r2", "t1", "do-thing", true);
        let outcome = ActionApplyOutcome::Applied {
            receipt_id: "r2".to_string(),
            receipt: ActionReceiptPayload::from_payload(&r.payload).unwrap(),
        };
        let rep = on_action_applied(ls, "ns-e", &outcome, &root).await;
        assert_eq!(rep.positive_reflections, 0);
    }

    #[tokio::test]
    async fn revert_hook_negative_sets_stale_after_and_queues_contradiction() {
        let dir = tmp_dir("revert-hook");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let store_dyn: Arc<dyn VectorStore> = Arc::new(store);
        let ls = VectorStore::as_any(store_dyn.as_ref())
            .downcast_ref::<LadybugStore>()
            .unwrap();

        let mut reverted = receipt_row("apply-1", "t1", "do-thing", true);
        if let Some(ar) = reverted.payload.metadata.get_mut("action_receipt") {
            ar["governs"] = j!(["rule-gov"]);
        }
        seed(
            ls,
            "ns-e",
            vec![reverted, memory("rule-gov", "rule", j!({}))],
        )
        .await;

        let revert_receipt = ActionReceiptPayload {
            receipt_id: "revert-1".to_string(),
            action_slug: "revert-do-thing".to_string(),
            principal: "agent".to_string(),
            executed_at: 1_700_000_010,
            params: Default::default(),
            superseded_pairs: vec![],
            created_ids: vec![],
            twin_deltas: vec![],
            edge_deltas: vec![],
            row_deltas: vec![],
            task_deltas: vec![],
            reverted_at: None,
            reverted_by_receipt_id: None,
            reverts_receipt_id: Some("apply-1".to_string()),
            forced_by: None,
            forced_at: None,
        };
        let outcome = ActionRevertOutcome {
            reverted_receipt_id: "apply-1".to_string(),
            active_receipt_version_id: "apply-1-v2".to_string(),
            revert_receipt_id: "revert-1".to_string(),
            revert_receipt,
            forced: false,
        };
        let root = tmp_dir("revert-root");
        let rep = on_action_reverted_with_root(ls, "ns-e", &outcome, &root).await;
        assert!(
            rep.errors.is_empty(),
            "revert hook errors: {:?}",
            rep.errors
        );
        assert_eq!(rep.negative_reflections, 1);
        assert_eq!(rep.contradictions_queued, 1);

        let (_, gov) = ls.get("ns-e", "rule-gov").await.unwrap().unwrap();
        assert_eq!(gov.metadata["outcome_stats"]["reverts"], 1);
        assert!(gov.metadata.get("stale_after").is_some());
        // Idempotent under duplicate delivery of the same revert commit.
        let rep2 = on_action_reverted_with_root(ls, "ns-e", &outcome, &root).await;
        assert_eq!(rep2.negative_reflections, 0);
        assert_eq!(rep2.contradictions_queued, 0);
    }

    // ── Review-fix regressions (F1–F6) ──────────────────────────────────────

    /// Always judges every supplied candidate as a duplicate at confidence 1.0.
    struct DupJudge;
    #[async_trait]
    impl JudgmentProvider for DupJudge {
        async fn judge_duplicates(
            &self,
            _new: &MemoryPayload,
            candidates: &[SearchResult],
        ) -> anyhow::Result<Vec<DuplicateJudgment>> {
            Ok(candidates
                .iter()
                .map(|c| DuplicateJudgment {
                    existing_memory_id: c.id.clone(),
                    confidence: 1.0,
                    reason: None,
                })
                .collect())
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn name(&self) -> &str {
            "dup-judge"
        }
    }

    fn memory_with(id: &str, user_id: &str, mt: &str, metadata: Value) -> SearchResult {
        let mut r = memory(id, mt, metadata);
        r.payload.user_id = user_id.to_string();
        r
    }

    fn valid_action_definition(slug: &str) -> ActionTypeDefinition {
        ActionTypeDefinition {
            slug: slug.to_string(),
            title: "Learned action".to_string(),
            description: "candidate".to_string(),
            required_role: "editor".to_string(),
            parameters: Default::default(),
            submission_criteria: Vec::new(),
            effects: vec![ActionEffectTemplate::UpsertMemory {
                memory_type: "fact".to_string(),
                content_template: "learned".to_string(),
                metadata_template: j!({}),
            }],
        }
    }

    fn action_candidate_id_of(def: &ActionTypeDefinition, task_ids: &[String]) -> String {
        let probe = MemoryPayload {
            content: format!("{}|{}", def.slug, task_ids.join(",")),
            user_id: "admin".to_string(),
            memory_type: "action_type_candidate".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: j!({}),
        };
        format!(
            "learn-action-candidate-{}",
            &sha256_fingerprint(&probe)[..24]
        )
    }

    fn receipt_governing(id: &str, task_id: &str, slug: &str, governs: &[&str]) -> SearchResult {
        let mut r = receipt_row(id, task_id, slug, false);
        if let Some(ar) = r.payload.metadata.get_mut("action_receipt") {
            ar["governs"] = j!(governs);
        }
        r
    }

    // F1 — failed-attempt reflection must resolve the receipt's governed rules,
    // not treat the exemplar row id as the reflection target.
    #[tokio::test]
    async fn failed_attempt_reflects_governing_rule_via_receipt() {
        let dir = tmp_dir("f1-failed-attempt");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let store_dyn: Arc<dyn VectorStore> = Arc::new(store);
        let ls = VectorStore::as_any(store_dyn.as_ref())
            .downcast_ref::<LadybugStore>()
            .unwrap();
        let emb = embedder().await;

        let rows = vec![
            task_row("t1", &["rule-gov"]),
            receipt_governing("r1", "t1", "do-thing", &["rule-gov"]),
            memory("rule-gov", "rule", j!({})),
            memory_with(
                "exemplar-1",
                "system",
                "fact",
                j!({ "attempt": { "outcome": "failed", "action_receipt_id": "r-fail" } }),
            ),
            receipt_governing("r-fail", "t1", "do-fail", &["rule-gov"]),
        ];
        seed(ls, "ns-e", rows).await;

        let root = tmp_dir("f1-root");
        std::fs::create_dir_all(&root).unwrap();
        let rep = on_task_completed_with_judge(
            &store_dyn,
            &emb,
            "ns-e",
            "t1",
            Some(Arc::new(NoDup)),
            0.8,
            &root,
        )
        .await;
        assert!(rep.errors.is_empty(), "hook errors: {:?}", rep.errors);
        assert_eq!(
            rep.negative_reflections, 1,
            "failed attempt must apply a negative outcome"
        );

        let (_, gov) = ls.get("ns-e", "rule-gov").await.unwrap().unwrap();
        assert_eq!(gov.metadata["outcome_stats"]["failures"], 1);
        assert!(gov.metadata.get("stale_after").is_some());
        // The exemplar row is evidence, never a reflection target.
        let (_, ex) = ls.get("ns-e", "exemplar-1").await.unwrap().unwrap();
        assert!(ex.metadata.get("outcome_stats").is_none());

        // Idempotent: the same receipt id cannot double-count.
        let rep2 = on_task_completed_with_judge(
            &store_dyn,
            &emb,
            "ns-e",
            "t1",
            Some(Arc::new(NoDup)),
            0.8,
            &root,
        )
        .await;
        assert_eq!(rep2.negative_reflections, 0);
        let (_, gov2) = ls.get("ns-e", "rule-gov").await.unwrap().unwrap();
        assert_eq!(gov2.metadata["outcome_stats"]["failures"], 1);
    }

    // F2 — foreign-owned inactive action-candidate row must not be clobbered.
    #[tokio::test]
    async fn action_candidate_rejects_foreign_owned_inactive_row() {
        let dir = tmp_dir("f2-foreign-action-candidate");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let def = valid_action_definition("learned-foreign");
        let task_ids = vec!["t1".to_string()];
        let id = action_candidate_id_of(&def, &task_ids);
        seed(
            &store,
            "ns-e",
            vec![memory_with(
                &id,
                "system",
                "action_type_candidate",
                j!({
                    "valid_to": 1,
                    "visible_to": [{ "principal": "foreign-owner", "role": "owner" }]
                }),
            )],
        )
        .await;
        let result = surface_action_candidate(&store, "ns-e", &def, &task_ids, 4).await;
        assert!(
            result.is_err(),
            "foreign-owned inactive row must not be overwritten"
        );
    }

    // F2 — an existing active action-candidate row is an idempotent no-op.
    #[tokio::test]
    async fn action_candidate_active_row_is_noop() {
        let dir = tmp_dir("f2-active-action-candidate");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let def = valid_action_definition("learned-active");
        let task_ids = vec!["t1".to_string()];
        assert!(surface_action_candidate(&store, "ns-e", &def, &task_ids, 4)
            .await
            .unwrap());
        assert!(
            !surface_action_candidate(&store, "ns-e", &def, &task_ids, 4)
                .await
                .unwrap()
        );
    }

    // F2 — an invalid definition is rejected before any write.
    #[tokio::test]
    async fn action_candidate_rejects_invalid_definition_pre_write() {
        let dir = tmp_dir("f2-invalid-action-candidate");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let def = valid_action_definition("NOT A CANONICAL SLUG");
        let task_ids = vec!["t1".to_string()];
        let id = action_candidate_id_of(&def, &task_ids);
        let result = surface_action_candidate(&store, "ns-e", &def, &task_ids, 4).await;
        assert!(result.is_err(), "invalid definition must be rejected");
        assert!(
            store.get("ns-e", &id).await.unwrap().is_none(),
            "nothing may be written"
        );
    }

    // P2 (Codex thread 4610878291) — an inert action candidate must carry a
    // zero vector of the STORE's configured width, not a hardcoded 4. A
    // production store is FLOAT[768]; a 4-wide vector is rejected by the engine.
    #[tokio::test]
    async fn action_candidate_uses_store_embedding_width() {
        const STORE_WIDTH: usize = 8; // deliberately != 4
        let dir = tmp_dir("p2-action-candidate-width");
        let store = LadybugStore::for_testing(&dir, STORE_WIDTH).expect("store");
        store.init("ns-e").await.unwrap();
        let def = valid_action_definition("learned-width");
        let task_ids = vec!["t1".to_string()];
        let id = action_candidate_id_of(&def, &task_ids);

        let result = surface_action_candidate(&store, "ns-e", &def, &task_ids, STORE_WIDTH).await;
        assert!(
            result.is_ok(),
            "candidate write into FLOAT[{STORE_WIDTH}] must succeed: {:?}",
            result.err()
        );
        let (vector, _) = store.get("ns-e", &id).await.unwrap().unwrap();
        assert_eq!(
            vector.len(),
            STORE_WIDTH,
            "candidate vector width must match the store column"
        );
    }

    // F3 — only receipts linked to the completing task may reinforce targets.
    #[tokio::test]
    async fn task_completion_only_reflects_receipts_linked_to_task() {
        let dir = tmp_dir("f3-linked-receipts");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let store_dyn: Arc<dyn VectorStore> = Arc::new(store);
        let ls = VectorStore::as_any(store_dyn.as_ref())
            .downcast_ref::<LadybugStore>()
            .unwrap();
        let emb = embedder().await;

        let rows = vec![
            task_row("t1", &["rule-a"]),
            task_row("t2", &["rule-b"]),
            memory("rule-a", "rule", j!({})),
            memory("rule-b", "rule", j!({})),
            receipt_governing("r1", "t1", "do-thing", &["rule-a"]),
            receipt_governing("r2", "t2", "do-thing", &["rule-b"]),
        ];
        seed(ls, "ns-e", rows).await;

        let root = tmp_dir("f3-root");
        std::fs::create_dir_all(&root).unwrap();
        let rep = on_task_completed_with_judge(
            &store_dyn,
            &emb,
            "ns-e",
            "t1",
            Some(Arc::new(NoDup)),
            0.8,
            &root,
        )
        .await;
        assert!(rep.errors.is_empty(), "hook errors: {:?}", rep.errors);
        assert_eq!(rep.positive_reflections, 1, "only r1 is linked to t1");
        let (_, a) = ls.get("ns-e", "rule-a").await.unwrap().unwrap();
        let (_, b) = ls.get("ns-e", "rule-b").await.unwrap().unwrap();
        assert_eq!(a.metadata["outcome_stats"]["successes"], 1);
        assert!(
            b.metadata.get("outcome_stats").is_none(),
            "r2 (task t2) must not reinforce rule-b"
        );
    }

    // F5 — a no-op / failed CAS must not enqueue a contradiction request.
    #[tokio::test]
    async fn revert_queues_only_targets_with_successful_negative_cas() {
        let dir = tmp_dir("f5-cas-gated-queue");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let rows = vec![
            memory(
                "rule-gov",
                "rule",
                j!({
                    "outcome_stats": {
                        "successes": 0, "reverts": 0, "failures": 0, "reliability_score": 0.5
                    },
                    "processed_receipts": ["revert-1"]
                }),
            ),
            {
                let mut r = receipt_row("apply-1", "t1", "do-thing", true);
                if let Some(ar) = r.payload.metadata.get_mut("action_receipt") {
                    ar["governs"] = j!(["rule-gov"]);
                    ar["receipt_id"] = j!("apply-1");
                }
                r
            },
        ];
        seed(&store, "ns-e", rows).await;

        let outcome = ActionRevertOutcome {
            reverted_receipt_id: "apply-1".to_string(),
            active_receipt_version_id: "apply-1-v2".to_string(),
            revert_receipt_id: "revert-1".to_string(),
            revert_receipt: ActionReceiptPayload {
                receipt_id: "revert-1".to_string(),
                action_slug: "revert-do-thing".to_string(),
                principal: "agent".to_string(),
                executed_at: 1_700_000_010,
                params: Default::default(),
                superseded_pairs: vec![],
                created_ids: vec![],
                twin_deltas: vec![],
                edge_deltas: vec![],
                row_deltas: vec![],
                task_deltas: vec![],
                reverted_at: None,
                reverted_by_receipt_id: None,
                reverts_receipt_id: Some("apply-1".to_string()),
                forced_by: None,
                forced_at: None,
            },
            forced: false,
        };
        let root = tmp_dir("f5-root");
        let rep = on_action_reverted_with_root(&store, "ns-e", &outcome, &root).await;
        assert!(rep.errors.is_empty(), "hook errors: {:?}", rep.errors);
        assert_eq!(
            rep.negative_reflections, 0,
            "receipt already processed => CAS no-op"
        );
        assert_eq!(
            rep.contradictions_queued, 0,
            "no successful CAS => no queue entry"
        );
        let queue = crate::judgment::laya_onnx::ContradictionQueue::new(&root);
        assert!(
            queue.pending().unwrap().is_empty(),
            "no request may be appended"
        );
    }

    // F6 — a retired procedure must not suppress a mined candidate.
    #[tokio::test]
    async fn retired_procedure_does_not_suppress_mined_candidate() {
        let dir = tmp_dir("f6-retired-procedure");
        let store = LadybugStore::for_testing(&dir, 4).expect("store");
        store.init("ns-e").await.unwrap();
        let store_dyn: Arc<dyn VectorStore> = Arc::new(store);
        let ls = VectorStore::as_any(store_dyn.as_ref())
            .downcast_ref::<LadybugStore>()
            .unwrap();
        let emb = embedder().await;

        let mut rows = Vec::new();
        for i in 1..=3 {
            let tid = format!("t{i}");
            rows.push(task_row(&tid, &["mined-source"]));
            rows.push(receipt_row(&format!("r{i}"), &tid, "do-thing", false));
        }
        rows.push(memory(
            "mined-source",
            "fact",
            j!({ "extracted_from": ["t1", "t2", "t3"] }),
        ));
        rows.push(memory(
            "retired-proc",
            "procedure",
            j!({ "valid_to": 1, "visible_to": [{ "principal": "admin", "role": "owner" }] }),
        ));
        seed(ls, "ns-e", rows).await;

        let root = tmp_dir("f6-root");
        std::fs::create_dir_all(&root).unwrap();
        let rep = on_task_completed_with_judge(
            &store_dyn,
            &emb,
            "ns-e",
            "t1",
            Some(Arc::new(DupJudge)),
            0.8,
            &root,
        )
        .await;
        assert!(rep.errors.is_empty(), "hook errors: {:?}", rep.errors);
        assert_eq!(
            rep.candidates_persisted, 1,
            "a retired procedure must not be used as a dedup candidate"
        );
    }
}
