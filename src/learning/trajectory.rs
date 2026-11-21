//! Deterministic trajectory-to-procedure miner for Capability E (`neurostrata-mpau`).
//!
//! Inspects completed tasks with clean (un-reverted) action receipt chains and
//! extracts recurring ordered n-grams of
//! `(trigger_context, tool_or_action_sequence, verification_gate)`.
//! When a pattern appears across >= 3 distinct completed tasks with a 100%
//! non-reverted completion rate, it synthesizes candidate `procedure` payloads
//! (and candidate `action_type` definitions) with SHACL-valid metadata.
//!
//! Evidence is mandatory: a task contributes only if it has both a nonempty,
//! un-reverted receipt chain and at least one extracted memory id. Dedup is
//! fail-closed: the judgment provider must be present, return without error,
//! and report only finite confidences below the finite threshold. Mined action
//! definitions are validated by the caller through the definition gate and are
//! NEVER auto-applied.

use crate::judgment::JudgmentProvider;
use crate::rdf::action::{ActionEffectTemplate, ActionReceiptPayload, ActionTypeDefinition};
use crate::task::machine::Status;
use crate::traits::{MemoryPayload, SearchResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Simple trigger enum values that a procedure may carry verbatim.
const SIMPLE_TRIGGERS: [&str; 3] = ["session-start", "before-edit", "after-mutation"];

/// Upper bound on an `every-n-*` interval. A governed trigger with an
/// unbounded suffix is not persisted; it is rejected instead.
const MAX_EVERY_N: u64 = 1_000_000;

/// One step in a task trajectory: a trigger context (e.g. "session-start"),
/// an ordered sequence of tool/action calls, and an optional verification gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryStep {
    pub trigger_context: String,
    pub tool_or_action_sequence: Vec<String>,
    pub verification_gate: Option<String>,
}

/// Evidence for a single task. Includes the task's status, extracted memory
/// ids, the action receipts associated with it, and the ordered trajectory
/// steps recorded during execution.
#[derive(Debug, Clone)]
pub struct TaskEvidence {
    pub task_id: String,
    pub status: Status,
    pub extracted_memory_ids: Vec<String>,
    pub receipts: Vec<ActionReceiptPayload>,
    pub steps: Vec<TrajectoryStep>,
}

/// A proposed procedure pattern identified by the miner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureCandidate {
    pub trigger: String,
    pub task_ids: Vec<String>,
    /// Actual (un-reverted) receipt ids backing the candidate. Used as source
    /// evidence; no verification is claimed without a real receipt id.
    pub receipt_ids: Vec<String>,
    pub pattern: Vec<TrajectoryStep>,
}

/// A candidate action type derived from a recurring pattern. Carries a COMPLETE
/// `ActionTypeDefinition` (valid role, nonempty valid effects) so callers can
/// validate the definition itself through the existing definition gate.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionTypeCandidate {
    pub definition: ActionTypeDefinition,
}

/// Returns true if the receipt indicates a revert has occurred.
pub fn is_receipt_reverted(receipt: &ActionReceiptPayload) -> bool {
    receipt.reverted_at.is_some() || receipt.reverted_by_receipt_id.is_some()
}

/// Returns true if all receipts in the task are un-reverted (clean).
/// NOTE: vacuously true for an empty chain; callers must also require the
/// chain to be nonempty.
pub fn has_only_clean_receipts(task: &TaskEvidence) -> bool {
    task.receipts.iter().all(|r| !is_receipt_reverted(r))
}

/// Maps a free-form trigger context onto the governed procedure trigger
/// vocabulary. Unknown or malformed contexts are REJECTED (`None`) rather than
/// silently rewritten to `session-start`, so the miner skips the pattern.
/// `every-n-*` suffixes must be a positive, bounded, nonzero integer.
fn normalize_trigger(context: &str) -> Option<String> {
    let ctx = context.trim();
    if SIMPLE_TRIGGERS.contains(&ctx) {
        return Some(ctx.to_string());
    }
    for prefix in ["every-n-sessions:", "every-n-days:"] {
        if let Some(rest) = ctx.strip_prefix(prefix) {
            if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            match rest.parse::<u64>() {
                Ok(n) if n > 0 && n <= MAX_EVERY_N => return Some(ctx.to_string()),
                _ => return None,
            }
        }
    }
    None
}

/// Mines recurring procedure patterns from completed tasks.
///
/// * `tasks` - task evidence; non-`done` tasks, tasks without a nonempty clean
///   receipt chain, and tasks with no extracted memory are filtered out before
///   pattern counting.
/// * `n` - n-gram length (number of contiguous steps per pattern).
/// * `existing_procedures` - current procedure memories to dedup against.
/// * `judge` - judgment provider (Laya ONNX in production, a fake in tests).
///   `None` means the judge is unavailable, which fails closed: no candidate.
/// * `confidence_threshold` - a duplicate at or above this finite confidence
///   blocks the candidate; a non-finite threshold fails closed.
///
/// Recurring patterns are emitted in deterministic (key-sorted) order, and each
/// candidate's `task_ids` and `receipt_ids` are sorted.
pub async fn mine_procedures(
    tasks: &[TaskEvidence],
    n: usize,
    existing_procedures: &[MemoryPayload],
    judge: Option<Arc<dyn JudgmentProvider>>,
    confidence_threshold: f32,
) -> Vec<ProcedureCandidate> {
    if n == 0 {
        return Vec::new();
    }

    // pattern key -> (window steps, distinct task ids, distinct receipt ids)
    let mut patterns: BTreeMap<String, (Vec<TrajectoryStep>, BTreeSet<String>, BTreeSet<String>)> =
        BTreeMap::new();

    for task in tasks {
        if task.status != Status::Done {
            continue;
        }
        // Empty receipts are vacuously "clean" but are NOT empirical evidence.
        if task.receipts.is_empty() || !has_only_clean_receipts(task) {
            continue;
        }
        if task.extracted_memory_ids.is_empty() {
            continue;
        }
        if task.steps.len() < n {
            continue;
        }
        for window in task.steps.windows(n) {
            let key = match serde_json::to_string(window) {
                Ok(key) => key,
                Err(_) => continue,
            };
            let entry = patterns
                .entry(key)
                .or_insert_with(|| (window.to_vec(), BTreeSet::new(), BTreeSet::new()));
            entry.1.insert(task.task_id.clone());
            entry
                .2
                .extend(task.receipts.iter().map(|r| r.receipt_id.clone()));
        }
    }

    let mut candidates = Vec::new();
    for (_key, (pattern, ids, receipt_ids)) in patterns {
        if ids.len() < 3 {
            continue;
        }
        let Some(trigger) = normalize_trigger(
            pattern
                .first()
                .map(|s| s.trigger_context.as_str())
                .unwrap_or(""),
        ) else {
            // Unknown/malformed trigger: skip the pattern, never remap it.
            continue;
        };
        let candidate = ProcedureCandidate {
            trigger,
            task_ids: ids.into_iter().collect(),
            receipt_ids: receipt_ids.into_iter().collect(),
            pattern,
        };
        if dedup_safe(
            &candidate,
            existing_procedures,
            judge.as_deref(),
            confidence_threshold,
        )
        .await
        {
            candidates.push(candidate);
        }
    }
    candidates
}

/// Fail-closed dedup: returns true only when a judge is present, the threshold
/// is finite, the judge answers without error, and no judgment has a non-finite
/// confidence or reports a duplicate at or above `confidence_threshold`.
async fn dedup_safe(
    candidate: &ProcedureCandidate,
    existing_procedures: &[MemoryPayload],
    judge: Option<&dyn JudgmentProvider>,
    confidence_threshold: f32,
) -> bool {
    let Some(judge) = judge else {
        return false;
    };
    // A NaN/infinite threshold cannot define a meaningful duplicate boundary.
    if !confidence_threshold.is_finite() {
        return false;
    }
    let new_memory = candidate_to_memory(candidate);
    let existing: Vec<SearchResult> = existing_procedures
        .iter()
        .enumerate()
        .map(|(i, payload)| SearchResult {
            id: format!("existing-procedure-{i}"),
            score: 1.0,
            payload: payload.clone(),
            evidence: None,
        })
        .collect();
    match judge.judge_duplicates(&new_memory, &existing).await {
        Err(_) => false,
        Ok(judgments) => {
            // `DuplicateJudgment` carries only a confidence score (there is no
            // decision field to consult), so the governed policy is: any
            // judgment at/above the finite threshold blocks the candidate, and
            // any non-finite confidence fails closed.
            for judgment in judgments {
                if !judgment.confidence.is_finite() {
                    return false;
                }
                if judgment.confidence >= confidence_threshold {
                    return false;
                }
            }
            true
        }
    }
}

/// Renders a human-readable description of a mined pattern.
fn describe(pattern: &[TrajectoryStep]) -> String {
    let parts: Vec<String> = pattern
        .iter()
        .map(|step| {
            let tools = step.tool_or_action_sequence.join(" -> ");
            match &step.verification_gate {
                Some(gate) => format!("[{}] {} (gate: {})", step.trigger_context, tools, gate),
                None => format!("[{}] {}", step.trigger_context, tools),
            }
        })
        .collect();
    format!("Trajectory-mined procedure: {}", parts.join(" ; "))
}

/// Miner identity for generated trust metadata.
const MINER_ID: &str = "trajectory-miner";

/// Trust vocabulary shared by procedure payloads and mined action effects.
/// Returns `(verified_by, source, epistemic_tier)`. `empirical_verified` is
/// emitted only when real receipt ids exist, and always alongside a `source`
/// string citing those same ids; no-receipt candidates use the canonical
/// `unverified_agent` tier with task-provenance ids instead.
fn trust_evidence(candidate: &ProcedureCandidate) -> (String, String, &'static str) {
    if candidate.receipt_ids.is_empty() {
        let provenance = format!("tasks:{}", candidate.task_ids.join(","));
        (provenance.clone(), provenance, "unverified_agent")
    } else {
        let receipts = format!("receipts:{}", candidate.receipt_ids.join(","));
        (receipts.clone(), receipts, "empirical_verified")
    }
}

/// Converts a `ProcedureCandidate` into a `MemoryPayload` carrying the metadata
/// required by the Capability E specification. Source evidence uses only real
/// receipt/task ids carried by the candidate; verification is claimed only when
/// an actual receipt id is available, and then `source` cites the same ids.
pub fn candidate_to_memory(candidate: &ProcedureCandidate) -> MemoryPayload {
    let (verified_by, source, epistemic_tier) = trust_evidence(candidate);
    let metadata = json!({
        "trigger": candidate.trigger,
        "remaining_fires": 10,
        "performance_count": 0,
        "generated_by": MINER_ID,
        "verified_by": verified_by,
        "source": source,
        "epistemic_tier": epistemic_tier,
        "source_evidence": {
            "receipt_ids": candidate.receipt_ids,
            "task_ids": candidate.task_ids,
        },
        "pattern": candidate.pattern,
    });
    MemoryPayload {
        content: describe(&candidate.pattern),
        user_id: MINER_ID.to_string(),
        memory_type: "procedure".to_string(),
        agent_name: Some(MINER_ID.to_string()),
        location: String::new(),
        location_lines: String::new(),
        metadata,
    }
}

/// Converts a `ProcedureCandidate` into a COMPLETE `ActionTypeCandidate`.
/// Returns `None` when the pattern carries no tool/action names.
///
/// The returned definition is ready for the caller to run through
/// `from_payload` + `validate_definition` + `validate_memory`. This function
/// never applies the action; auto-apply is forbidden.
pub fn candidate_to_action_type(candidate: &ProcedureCandidate) -> Option<ActionTypeCandidate> {
    let mut tokens: Vec<String> = Vec::new();
    for step in &candidate.pattern {
        for tool in &step.tool_or_action_sequence {
            let token = tool
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            for part in token.split('-') {
                if !part.is_empty() {
                    tokens.push(part.to_string());
                }
            }
        }
    }
    if tokens.is_empty() {
        return None;
    }
    let description = describe(&candidate.pattern);
    // Mirror candidate_to_memory exactly: an empirical_verified effect is
    // created only with matching verified_by + source citing real receipt ids.
    let (verified_by, source, epistemic_tier) = trust_evidence(candidate);
    let definition = ActionTypeDefinition {
        slug: format!("mined-{}", tokens.join("-")),
        title: format!("Mined procedure ({})", candidate.trigger),
        description: description.clone(),
        required_role: "editor".to_string(),
        parameters: Default::default(),
        submission_criteria: vec![],
        effects: vec![ActionEffectTemplate::UpsertMemory {
            memory_type: "procedure".to_string(),
            content_template: description,
            metadata_template: json!({
                "generated_by": MINER_ID,
                "verified_by": verified_by,
                "source": source,
                "epistemic_tier": epistemic_tier,
            }),
        }],
    };
    Some(ActionTypeCandidate { definition })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judgment::DuplicateJudgment;
    use async_trait::async_trait;

    fn step(trigger: &str, seq: Vec<&str>, gate: Option<&str>) -> TrajectoryStep {
        TrajectoryStep {
            trigger_context: trigger.to_string(),
            tool_or_action_sequence: seq.iter().map(|s| s.to_string()).collect(),
            verification_gate: gate.map(|s| s.to_string()),
        }
    }

    fn receipt(tag: &str, reverted: bool) -> ActionReceiptPayload {
        ActionReceiptPayload {
            receipt_id: tag.to_string(),
            action_slug: "test-action".to_string(),
            principal: "agent".to_string(),
            executed_at: 1_700_000_000,
            params: Default::default(),
            target_pointer_id: None,
            target_pool_id: None,
            superseded_pairs: vec![],
            created_ids: vec![],
            twin_deltas: vec![],
            edge_deltas: vec![],
            row_deltas: vec![],
            task_deltas: vec![],
            reverted_at: if reverted { Some(1_700_001_000) } else { None },
            reverted_by_receipt_id: if reverted {
                Some(format!("rev-{tag}"))
            } else {
                None
            },
            reverts_receipt_id: None,
            forced_by: None,
            forced_at: None,
        }
    }

    /// Standard qualifying task: has a receipt and an extracted memory.
    fn task(
        task_id: &str,
        status: Status,
        receipts: Vec<ActionReceiptPayload>,
        steps: Vec<TrajectoryStep>,
    ) -> TaskEvidence {
        TaskEvidence {
            task_id: task_id.to_string(),
            status,
            extracted_memory_ids: vec![format!("{task_id}-extraction")],
            receipts,
            steps,
        }
    }

    fn procedure_memory(id: &str) -> MemoryPayload {
        MemoryPayload {
            content: format!("existing procedure {id}"),
            user_id: "system".to_string(),
            memory_type: "procedure".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "trigger": "session-start" }),
        }
    }

    struct NoDupProvider;

    #[async_trait]
    impl JudgmentProvider for NoDupProvider {
        async fn judge_duplicates(
            &self,
            _new_memory: &MemoryPayload,
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

    struct AlwaysDupProvider;

    #[async_trait]
    impl JudgmentProvider for AlwaysDupProvider {
        async fn judge_duplicates(
            &self,
            _new_memory: &MemoryPayload,
            candidates: &[SearchResult],
        ) -> anyhow::Result<Vec<DuplicateJudgment>> {
            Ok(candidates
                .iter()
                .map(|c| DuplicateJudgment {
                    existing_memory_id: c.id.clone(),
                    confidence: 0.99,
                    reason: Some("always-dup".to_string()),
                })
                .collect())
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn name(&self) -> &str {
            "always-dup"
        }
    }

    struct ErrorProvider;

    #[async_trait]
    impl JudgmentProvider for ErrorProvider {
        async fn judge_duplicates(
            &self,
            _new_memory: &MemoryPayload,
            _candidates: &[SearchResult],
        ) -> anyhow::Result<Vec<DuplicateJudgment>> {
            anyhow::bail!("judge unavailable")
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn name(&self) -> &str {
            "error"
        }
    }

    /// Emits one judgment per candidate with a fixed (possibly malformed) confidence.
    struct FixedConfidenceProvider(f32);

    #[async_trait]
    impl JudgmentProvider for FixedConfidenceProvider {
        async fn judge_duplicates(
            &self,
            _new_memory: &MemoryPayload,
            candidates: &[SearchResult],
        ) -> anyhow::Result<Vec<DuplicateJudgment>> {
            Ok(candidates
                .iter()
                .map(|c| DuplicateJudgment {
                    existing_memory_id: c.id.clone(),
                    confidence: self.0,
                    reason: None,
                })
                .collect())
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn name(&self) -> &str {
            "fixed-confidence"
        }
    }

    fn no_dup() -> Arc<dyn JudgmentProvider> {
        Arc::new(NoDupProvider)
    }

    /// Three distinct qualifying tasks sharing `s`.
    fn three_tasks(s: &TrajectoryStep) -> Vec<TaskEvidence> {
        vec![
            task(
                "t1",
                Status::Done,
                vec![receipt("r1", false)],
                vec![s.clone()],
            ),
            task(
                "t2",
                Status::Done,
                vec![receipt("r2", false)],
                vec![s.clone()],
            ),
            task(
                "t3",
                Status::Done,
                vec![receipt("r3", false)],
                vec![s.clone()],
            ),
        ]
    }

    #[tokio::test]
    async fn two_occurrences_rejected_three_accepted() {
        let s = step("session-start", vec!["tool_a", "tool_b"], Some("gate_ok"));
        let tasks_2 = vec![
            task(
                "t1",
                Status::Done,
                vec![receipt("r1", false)],
                vec![s.clone()],
            ),
            task(
                "t2",
                Status::Done,
                vec![receipt("r2", false)],
                vec![s.clone()],
            ),
        ];
        let cands_2 = mine_procedures(&tasks_2, 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(cands_2.len(), 0, "two occurrences must be rejected");

        let cands_3 = mine_procedures(&three_tasks(&s), 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(
            cands_3.len(),
            1,
            "three occurrences must yield one candidate"
        );
        assert_eq!(cands_3[0].task_ids, vec!["t1", "t2", "t3"]);
        assert_eq!(cands_3[0].receipt_ids, vec!["r1", "r2", "r3"]);
    }

    #[tokio::test]
    async fn distinct_tasks_required_not_repeated_in_one_task() {
        let s = step("session-start", vec!["a", "b"], None);
        let tasks = vec![task(
            "t1",
            Status::Done,
            vec![receipt("r1", false)],
            vec![s.clone(), s.clone(), s.clone()],
        )];
        let cands = mine_procedures(&tasks, 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(
            cands.len(),
            0,
            "one task repeated is not three distinct tasks"
        );
    }

    #[tokio::test]
    async fn order_matters() {
        let s_ab = step("session-start", vec!["A", "B"], None);
        let s_ba = step("session-start", vec!["B", "A"], None);
        let tasks = vec![
            task(
                "t1",
                Status::Done,
                vec![receipt("r1", false)],
                vec![s_ab.clone()],
            ),
            task(
                "t2",
                Status::Done,
                vec![receipt("r2", false)],
                vec![s_ba.clone()],
            ),
            task(
                "t3",
                Status::Done,
                vec![receipt("r3", false)],
                vec![s_ab.clone()],
            ),
        ];
        let cands = mine_procedures(&tasks, 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(
            cands.len(),
            0,
            "distinct order patterns must not be conflated"
        );
    }

    #[tokio::test]
    async fn any_revert_blocks_candidate() {
        let s = step("session-start", vec!["a", "b"], None);
        let tasks = vec![
            task(
                "t1",
                Status::Done,
                vec![receipt("r1", false)],
                vec![s.clone()],
            ),
            task(
                "t2",
                Status::Done,
                vec![receipt("r2", true)],
                vec![s.clone()],
            ),
            task(
                "t3",
                Status::Done,
                vec![receipt("r3", false)],
                vec![s.clone()],
            ),
        ];
        let cands = mine_procedures(&tasks, 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(cands.len(), 0, "reverted receipt must block candidate");
    }

    #[tokio::test]
    async fn zero_receipts_is_not_empirical_evidence() {
        let s = step("session-start", vec!["a", "b"], None);
        let no_receipt = TaskEvidence {
            task_id: "t1".to_string(),
            status: Status::Done,
            extracted_memory_ids: vec!["m1".to_string()],
            receipts: vec![], // clean is vacuously true, but no evidence
            steps: vec![s.clone()],
        };
        let tasks = vec![
            no_receipt,
            task(
                "t2",
                Status::Done,
                vec![receipt("r2", false)],
                vec![s.clone()],
            ),
            task(
                "t3",
                Status::Done,
                vec![receipt("r3", false)],
                vec![s.clone()],
            ),
        ];
        let cands = mine_procedures(&tasks, 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(
            cands.len(),
            0,
            "empty receipt chain must not count as evidence"
        );
    }

    #[tokio::test]
    async fn zero_extractions_is_not_empirical_evidence() {
        let s = step("session-start", vec!["a", "b"], None);
        let no_extraction = TaskEvidence {
            task_id: "t1".to_string(),
            status: Status::Done,
            extracted_memory_ids: vec![],
            receipts: vec![receipt("r1", false)],
            steps: vec![s.clone()],
        };
        let tasks = vec![
            no_extraction,
            task(
                "t2",
                Status::Done,
                vec![receipt("r2", false)],
                vec![s.clone()],
            ),
            task(
                "t3",
                Status::Done,
                vec![receipt("r3", false)],
                vec![s.clone()],
            ),
        ];
        let cands = mine_procedures(&tasks, 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(
            cands.len(),
            0,
            "no extracted memory must not count as evidence"
        );
    }

    #[tokio::test]
    async fn unknown_trigger_is_rejected_not_rewritten() {
        let s = step("totally-unknown-context", vec!["a", "b"], None);
        let cands = mine_procedures(&three_tasks(&s), 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(
            cands.len(),
            0,
            "unknown trigger must be skipped, not silently remapped to session-start"
        );
    }

    #[tokio::test]
    async fn every_n_triggers_validated() {
        let valid = step("every-n-sessions:3", vec!["a", "b"], None);
        let cands = mine_procedures(&three_tasks(&valid), 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].trigger, "every-n-sessions:3");

        let valid_days = step("every-n-days:1", vec!["a", "b"], None);
        let cands = mine_procedures(&three_tasks(&valid_days), 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].trigger, "every-n-days:1");

        for bad in [
            "every-n-sessions:0",
            "every-n-days:0",
            "every-n-sessions:abc",
            "every-n-sessions:-3",
            "every-n-sessions:18446744073709551616", // > u64::MAX
            "every-n-days:1000001",                  // above bounded max
        ] {
            let s = step(bad, vec!["a", "b"], None);
            let cands = mine_procedures(&three_tasks(&s), 1, &[], Some(no_dup()), 0.8).await;
            assert_eq!(cands.len(), 0, "malformed trigger '{bad}' must be rejected");
        }
    }

    #[tokio::test]
    async fn malformed_confidence_and_threshold_fail_closed() {
        let s = step("session-start", vec!["a"], None);
        let tasks = three_tasks(&s);
        let existing = vec![procedure_memory("p-1")];

        for bad_confidence in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let judge: Arc<dyn JudgmentProvider> =
                Arc::new(FixedConfidenceProvider(bad_confidence));
            let cands = mine_procedures(&tasks, 1, &existing, Some(judge), 0.8).await;
            assert_eq!(cands.len(), 0, "non-finite confidence must fail closed");
        }

        for bad_threshold in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let cands = mine_procedures(&tasks, 1, &existing, Some(no_dup()), bad_threshold).await;
            assert_eq!(cands.len(), 0, "non-finite threshold must fail closed");
        }

        // A finite judgment below threshold still allows the candidate.
        let low: Arc<dyn JudgmentProvider> = Arc::new(FixedConfidenceProvider(0.1));
        let cands = mine_procedures(&tasks, 1, &existing, Some(low), 0.8).await;
        assert_eq!(cands.len(), 1, "finite below-threshold judgment must allow");
    }

    #[tokio::test]
    async fn candidate_payload_is_shacl_valid() {
        let s = step("before-edit", vec!["edit", "validate"], Some("ok"));
        let cands = mine_procedures(&three_tasks(&s), 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(cands.len(), 1);
        let mem = candidate_to_memory(&cands[0]);
        let violations = crate::rdf::shacl::validate_memory(&mem);
        assert!(
            violations.is_empty(),
            "candidate memory must pass SHACL: {:?}",
            violations
        );
        assert_eq!(mem.memory_type, "procedure");
        let md = mem.metadata;
        assert_eq!(
            md.get("generated_by").and_then(|v| v.as_str()),
            Some("trajectory-miner")
        );
        assert_eq!(
            md.get("epistemic_tier").and_then(|v| v.as_str()),
            Some("empirical_verified")
        );
        assert_eq!(md.get("remaining_fires").and_then(|v| v.as_i64()), Some(10));
        assert_eq!(
            md.get("performance_count").and_then(|v| v.as_i64()),
            Some(0)
        );
        let vb = md.get("verified_by").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(
            vb, "receipts:r1,r2,r3",
            "verified_by must cite exact receipt ids"
        );
        let src = md.get("source").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(
            src, "receipts:r1,r2,r3",
            "source must cite exact receipt ids"
        );
        assert_eq!(src, vb, "empirical_verified requires source == verified_by");
        let evidence = md.get("source_evidence").unwrap();
        let receipt_ids = evidence.get("receipt_ids").unwrap().as_array().unwrap();
        assert_eq!(receipt_ids.len(), 3);
        assert_eq!(receipt_ids[0].as_str(), Some("r1"));
    }

    #[tokio::test]
    async fn candidate_without_receipt_claims_no_verification() {
        // A hand-built candidate with no receipts must not fabricate verification.
        let candidate = ProcedureCandidate {
            trigger: "session-start".to_string(),
            task_ids: vec!["t1".to_string()],
            receipt_ids: vec![],
            pattern: vec![step("session-start", vec!["a"], None)],
        };
        let md = candidate_to_memory(&candidate).metadata;
        let vb = md.get("verified_by").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(vb, "tasks:t1", "no receipt => task provenance only");
        let src = md.get("source").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(src, "tasks:t1");
        assert_eq!(src, vb);
        assert_eq!(
            md.get("epistemic_tier").and_then(|v| v.as_str()),
            Some("unverified_agent"),
            "no receipt => canonical unverified_agent tier"
        );
        assert!(!md.to_string().contains("empirical_verified"));
        assert!(md
            .get("source_evidence")
            .unwrap()
            .get("receipt_ids")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn laya_dedup_fail_closed() {
        let s = step("session-start", vec!["x"], None);
        let tasks = three_tasks(&s);
        let existing = vec![procedure_memory("p-1")];

        let dup_judge: Arc<dyn JudgmentProvider> = Arc::new(AlwaysDupProvider);
        let cands_dup = mine_procedures(&tasks, 1, &existing, Some(dup_judge), 0.8).await;
        assert_eq!(
            cands_dup.len(),
            0,
            "duplicate found by judge must block candidate"
        );

        let cands_none = mine_procedures(&tasks, 1, &existing, None, 0.8).await;
        assert_eq!(
            cands_none.len(),
            0,
            "absent judge must be treated as unsafe"
        );

        let err_judge: Arc<dyn JudgmentProvider> = Arc::new(ErrorProvider);
        let cands_err = mine_procedures(&tasks, 1, &existing, Some(err_judge), 0.8).await;
        assert_eq!(cands_err.len(), 0, "judge error must be treated as unsafe");
    }

    #[tokio::test]
    async fn candidate_action_type_definition_validates_end_to_end() {
        let s = step("session-start", vec!["a", "b", "c"], Some("gate"));
        let cands = mine_procedures(&three_tasks(&s), 1, &[], Some(no_dup()), 0.8).await;
        assert_eq!(cands.len(), 1);
        // Validate the definition returned by the miner itself — do not
        // hand-assemble missing fields in the test.
        let def = candidate_to_action_type(&cands[0])
            .expect("must produce an action candidate")
            .definition;
        assert_eq!(def.required_role, "editor");
        assert!(!def.effects.is_empty());

        // Action-effect metadata must mirror candidate_to_memory exactly and
        // use the canonical verified vocabulary.
        let template = match &def.effects[0] {
            ActionEffectTemplate::UpsertMemory {
                metadata_template, ..
            } => metadata_template.clone(),
            other @ ActionEffectTemplate::SetTwinState { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
            other @ ActionEffectTemplate::SupersedeMemory { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
            other @ ActionEffectTemplate::LinkEdge { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
            other @ ActionEffectTemplate::TransitionTask { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
        };
        let proc_md = candidate_to_memory(&cands[0]).metadata;
        for field in ["generated_by", "verified_by", "source", "epistemic_tier"] {
            assert_eq!(
                template.get(field),
                proc_md.get(field),
                "effect metadata '{field}' must mirror procedure metadata"
            );
        }
        assert_eq!(
            template.get("generated_by").and_then(|v| v.as_str()),
            Some("trajectory-miner")
        );
        assert_eq!(
            template.get("verified_by").and_then(|v| v.as_str()),
            Some("receipts:r1,r2,r3")
        );
        assert_eq!(
            template.get("source").and_then(|v| v.as_str()),
            Some("receipts:r1,r2,r3")
        );
        assert_eq!(
            template.get("epistemic_tier").and_then(|v| v.as_str()),
            Some("empirical_verified")
        );

        let def_payload = MemoryPayload {
            content: def.title.clone(),
            user_id: "system".to_string(),
            memory_type: "action_type".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "action_type": def.clone() }),
        };
        assert!(
            def.validate_definition().is_empty(),
            "mined definition must pass definition gate: {:?}",
            def.validate_definition()
        );
        let parsed = ActionTypeDefinition::from_payload(&def_payload).unwrap();
        assert_eq!(parsed, def);
        let mem_violations = crate::rdf::shacl::validate_memory(&def_payload);
        assert!(
            mem_violations.is_empty(),
            "mined definition payload must pass validate_memory: {:?}",
            mem_violations
        );
    }

    #[tokio::test]
    async fn action_effect_never_claims_empirical_verified_without_receipts() {
        let candidate = ProcedureCandidate {
            trigger: "session-start".to_string(),
            task_ids: vec!["t1".to_string()],
            receipt_ids: vec![],
            pattern: vec![step("session-start", vec!["a"], None)],
        };
        let def = candidate_to_action_type(&candidate).unwrap().definition;
        let template = match &def.effects[0] {
            ActionEffectTemplate::UpsertMemory {
                metadata_template, ..
            } => metadata_template.clone(),
            other @ ActionEffectTemplate::SetTwinState { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
            other @ ActionEffectTemplate::SupersedeMemory { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
            other @ ActionEffectTemplate::LinkEdge { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
            other @ ActionEffectTemplate::TransitionTask { .. } => {
                panic!("candidate_to_action_type must produce UpsertMemory, got {other:?}")
            }
        };
        assert_eq!(
            template.get("epistemic_tier").and_then(|v| v.as_str()),
            Some("unverified_agent")
        );
        assert_eq!(
            template.get("verified_by").and_then(|v| v.as_str()),
            Some("tasks:t1")
        );
        assert_eq!(
            template.get("source").and_then(|v| v.as_str()),
            Some("tasks:t1")
        );
        assert!(!template.to_string().contains("empirical_verified"));
        // Mirrors the procedure payload for the same evidence-free candidate.
        let proc_md = candidate_to_memory(&candidate).metadata;
        for field in ["generated_by", "verified_by", "source", "epistemic_tier"] {
            assert_eq!(template.get(field), proc_md.get(field));
        }
    }

    #[tokio::test]
    async fn action_candidate_with_no_tools_is_none() {
        let candidate = ProcedureCandidate {
            trigger: "session-start".to_string(),
            task_ids: vec!["t1".to_string()],
            receipt_ids: vec!["r1".to_string()],
            pattern: vec![step("session-start", vec![], None)],
        };
        assert!(candidate_to_action_type(&candidate).is_none());
    }
}
