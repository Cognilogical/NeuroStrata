//! Task-validation graph/ontology advisories (design spec §6 R7).
//!
//! Crate-free by design: the integration test compiles this module from its
//! real path, the same way `snapshot_readiness.rs` and `routing.rs` are shared.
//! Pure input only: no store, clock, or filesystem access here. Every value is
//! additive to `task_validate`; nothing computed here is ever copied into a
//! gate `violations` array.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const W_GRAPH_NO_EXPANSION: &str = "W_GRAPH_NO_EXPANSION";
pub const W_DIFF_TOUR_RECOMMENDED: &str = "W_DIFF_TOUR_RECOMMENDED";
pub const W_ONTOLOGY_STRATEGIC_MISS: &str = "W_ONTOLOGY_STRATEGIC_MISS";

pub const T_HIGH_BLAST: &str = "T_HIGH_BLAST";
pub const T_DIFF: &str = "T_DIFF";
pub const T_STRATEGIC: &str = "T_STRATEGIC";
pub const T_PATH: &str = "T_PATH";
pub const T_AMBIGUOUS: &str = "T_AMBIGUOUS";
pub const T_SEED_EDGE: &str = "T_SEED_EDGE";
pub const T_SEED_EXPAND: &str = "T_SEED_EXPAND";
pub const T_DOMAIN_NEW: &str = "T_DOMAIN_NEW";
pub const T_INVARIANT: &str = "T_INVARIANT";

const PATH_TOOL: &str = "neurostrata_memories_for_file";
const AMBIGUOUS_TOOL: &str = "neurostrata_locate";
const SEED_EDGE_TOOL: &str = "neurostrata_object_view";
pub const HIGH_BLAST_TOOL: &str = "neurostrata_related";
const DIFF_TOOL: &str = "neurostrata_diff_tour";
const TAXONOMY_TOOL: &str = "neurostrata_taxonomy";
const INVARIANT_TOOL: &str = "neurostrata_sparql_query";
/// `empty` is a valid lookup outcome: a real tool ran and honestly found
/// nothing. `unsupported`/`failed` are not genuine coverage.
const GENUINE_STATUSES: &[&str] = &["success", "empty"];

/// Caller-supplied signals. Absent signals abstain; nothing is invented.
/// `ledger` is the local `.NeuroStrata/session_queries.json` document; a
/// missing/unreadable ledger is passed as `{}` by the caller and simply yields
/// zero coverage (fail-soft, no error, no violation).
pub struct AdvisoryInput<'a> {
    pub ledger: &'a Value,
    pub active_task_ids: &'a BTreeSet<String>,
    /// The canonical graph-tool allowlist (`snapshot_readiness::GRAPH_TOOLS`),
    /// passed in so this crate-free module needs no duplicate constant.
    pub allowed_graph_tools: &'a [&'a str],
    /// Triggers judged applicable by the caller (validate args or routing
    /// signals). Ledger-recorded triggers are added on top of these.
    pub applicable_trigger_ids: &'a [String],
    pub taxonomy_available: bool,
    pub concept_grounding_linked: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdvisoryReport {
    pub genuine_lookup_coverage: bool,
    pub distinct_graph_tools: usize,
    pub high_blast_radius_unexplored: bool,
    pub diff_tour_recommended: bool,
    pub taxonomy_available: bool,
    pub strategic_context_detected: bool,
    pub concept_grounding_linked: bool,
    pub recommendations: Vec<String>,
    pub unresolved_trigger_ids: Vec<String>,
}

impl AdvisoryReport {
    /// The exact R7.1 field set. No `violations` key, no strict-mode switch.
    pub fn to_json(&self) -> Value {
        json!({
            "genuine_lookup_coverage": self.genuine_lookup_coverage,
            "distinct_graph_tools": self.distinct_graph_tools,
            "high_blast_radius_unexplored": self.high_blast_radius_unexplored,
            "diff_tour_recommended": self.diff_tour_recommended,
            "taxonomy_available": self.taxonomy_available,
            "strategic_context_detected": self.strategic_context_detected,
            "concept_grounding_linked": self.concept_grounding_linked,
            "recommendations": self.recommendations,
            "unresolved_trigger_ids": self.unresolved_trigger_ids,
        })
    }
}

fn events(ledger: &Value) -> &[Value] {
    ledger
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Receipts carrying the same non-empty `event_id` are one lookup, even when
/// the ledger replays them. A missing/empty id is counted once per row.
fn unique_events(ledger: &Value) -> Vec<&Value> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    events(ledger)
        .iter()
        .filter(
            |event| match event.get("event_id").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => seen.insert(id),
                _ => true,
            },
        )
        .collect()
}

fn attributed(event: &Value, active_ids: &BTreeSet<String>) -> bool {
    event
        .get("task_id")
        .and_then(Value::as_str)
        .map(|id| active_ids.contains(id))
        .unwrap_or(false)
}

fn is_trigger_id(value: &str) -> bool {
    value.len() > 2
        && value.starts_with("T_")
        && value.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

/// Triggers witnessed by real receipts: a `trigger_id` on any attributed
/// event, or a `T_*:` action id in `prescribed_action_ids`. Failed receipts
/// still prove applicability; they simply cannot resolve the trigger.
pub fn applicable_triggers_from_ledger(
    ledger: &Value,
    active_ids: &BTreeSet<String>,
) -> Vec<String> {
    let mut found = BTreeSet::new();
    for event in unique_events(ledger) {
        if !attributed(event, active_ids) {
            continue;
        }
        if let Some(id) = event.get("trigger_id").and_then(Value::as_str) {
            if is_trigger_id(id) {
                found.insert(id.to_string());
            }
        }
        if let Some(ids) = event.get("prescribed_action_ids").and_then(Value::as_array) {
            for value in ids {
                if let Some(action_id) = value.as_str() {
                    let prefix = action_id.split(':').next().unwrap_or("");
                    if is_trigger_id(prefix) {
                        found.insert(prefix.to_string());
                    }
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Dispatch-time proof plan: the trigger a real tool call follows plus the
/// canonical digest-only args/ids used to mint a stable action id. The caller
/// copies the resulting action id into `followed_action_ids`; no raw text is
/// ever written to the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchProof {
    pub trigger: String,
    pub args: Value,
    pub ids: Vec<String>,
}

/// Which trigger a tool call actually follows. An explicit hint (the
/// `next_action` the caller is dispatching) wins. `related` serves both
/// `T_HIGH_BLAST` and `T_SEED_EXPAND` with byte-identical args, so with no
/// hint it MUST abstain — a default mint would falsely resolve the other
/// trigger (C32). `diff_tour` and taxonomy `advise` map to a single trigger
/// and keep a safe default. A hint incompatible with the tool is ignored.
pub fn proof_trigger_for_tool(
    tool: &str,
    operation: Option<&str>,
    hint: Option<&str>,
) -> Option<String> {
    if let Some(h) = hint.map(str::trim).filter(|s| is_trigger_id(s)) {
        let compatible = match h {
            T_HIGH_BLAST | T_SEED_EXPAND => tool == HIGH_BLAST_TOOL,
            T_DIFF => tool == DIFF_TOOL,
            T_STRATEGIC => tool == TAXONOMY_TOOL && operation == Some("advise"),
            _ => false,
        };
        if compatible {
            return Some(h.to_string());
        }
    }
    // Deliberately no `HIGH_BLAST_TOOL` default: only a hint can de-alias it.
    match (tool, operation) {
        (DIFF_TOOL, _) => Some(T_DIFF.to_string()),
        (TAXONOMY_TOOL, Some("advise")) => Some(T_STRATEGIC.to_string()),
        _ => None,
    }
}

/// Pure proof plan for a real tool call. Abstains (`None`) when the call cannot
/// honestly prove a prescription: no usable seed, fewer than the prescribed
/// two hops, empty diff text, a non-`advise` taxonomy operation, or — for
/// `related` — no trigger hint (C32). `related` with hops >= 2 is normalized to
/// the canonical two-hop args so a superset (e.g. three hops) reproduces the
/// prescribed digest (C10).
pub fn dispatch_proof(
    tool: &str,
    operation: Option<&str>,
    args: Option<&Value>,
    hint: Option<&str>,
    namespace: &str,
) -> Option<DispatchProof> {
    let trigger = proof_trigger_for_tool(tool, operation, hint)?;
    let args = args?;
    match (tool, operation) {
        (HIGH_BLAST_TOOL, _) => {
            let raw_seed = args.get("id").and_then(Value::as_str)?;
            let seed = raw_seed.trim();
            if seed.is_empty() {
                return None;
            }
            if args.get("max_hops").and_then(Value::as_u64).unwrap_or(0) < 2 {
                return None;
            }
            Some(DispatchProof {
                trigger: trigger.clone(),
                // The hint is part of the prescribed args, so include it in
                // the canonical digest: a hinted prescription and its real
                // dispatch must reproduce the exact same action id (C32).
                args: json!({
                    "namespace": namespace,
                    "id": seed,
                    "max_hops": 2,
                    "followed_trigger_id": &trigger,
                }),
                ids: vec![seed.to_string()],
            })
        }
        (DIFF_TOOL, _) => {
            let diff = args.get("diff_text").and_then(Value::as_str)?;
            if diff.trim().is_empty() {
                return None;
            }
            // Keep the raw text for the digest so it matches the prescription
            // verbatim; only the emptiness check is trimmed.
            Some(DispatchProof {
                trigger,
                args: json!({ "namespace": namespace, "diff_text": diff }),
                ids: vec![],
            })
        }
        (TAXONOMY_TOOL, Some("advise")) => {
            let top_k = args.get("top_k").and_then(Value::as_u64)?;
            Some(DispatchProof {
                trigger,
                args: json!({ "namespace": namespace, "operation": "advise", "top_k": top_k }),
                ids: vec![],
            })
        }
        _ => None,
    }
}

fn receipt_ts(event: &Value) -> Option<i64> {
    event.get("ts").and_then(Value::as_i64)
}

/// Merge an expected proof. A ledger row supplies a timestamp; a caller
/// signal (a full `<TRIGGER>:<digest>` action id that survived eviction) does
/// not. Prefer a known timestamp over an unknown one, then the latest known.
fn merge_expected(map: &mut BTreeMap<String, Option<i64>>, id: &str, ts: Option<i64>) {
    match map.get_mut(id) {
        Some(slot) => {
            if slot.is_none() {
                *slot = ts;
            } else if let (Some(cur), Some(new)) = (*slot, ts) {
                if new > cur {
                    *slot = Some(new);
                }
            }
        }
        None => {
            map.insert(id.to_string(), ts);
        }
    }
}

/// A genuine follow-through must show a real, non-empty result — not merely a
/// receipt that ran. Diff tours with no stops and empty advise responses are
/// reported as `status:"empty"` by the receipt builder.
fn nonempty_outcome(event: &Value) -> bool {
    if event.get("status").and_then(Value::as_str) == Some("empty") {
        return false;
    }
    if event
        .get("result_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        return true;
    }
    event
        .get("returned_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| !ids.is_empty())
}

/// A resolving receipt proves a prescription only when it carries a followed
/// action id with the SAME trigger prefix (no cross-trigger aliasing), that id
/// is an expected prescription, the receipt is not earlier than the
/// prescription, and — for diff/strategic — the outcome is non-empty.
fn proof_proves(
    event: &Value,
    expected: &BTreeMap<String, Option<i64>>,
    trigger: &str,
    require_outcome: bool,
) -> bool {
    let Some(followed) = event.get("followed_action_ids").and_then(Value::as_array) else {
        return false;
    };
    if require_outcome && !nonempty_outcome(event) {
        return false;
    }
    let prefix = format!("{trigger}:");
    let event_ts = receipt_ts(event);
    for value in followed.iter().filter_map(Value::as_str) {
        if !value.starts_with(&prefix) {
            continue;
        }
        if let Some(prescribed_ts) = expected.get(value) {
            if let (Some(real), Some(prescribed)) = (event_ts, *prescribed_ts) {
                if real < prescribed {
                    continue;
                }
            }
            return true;
        }
    }
    false
}

/// Compute all R7 advisories. A genuine receipt is an attributed `events[]`
/// entry with an allowlisted tool and a `success`/`empty` status. Legacy
/// `queries[]` rows and `tool:"pre-commit"` are never read: only `events[]`
/// reaches this function. An escalated trigger is resolved by its OWN tool
/// receipt: a followed action id must match a stable digest of the prescribed
/// `<TRIGGER>:<args>` action, carry the same trigger prefix, and be at least as
/// new as the prescription. A prescription passed as a full action id in
/// `applicable_trigger_ids` still resolves after its ledger row is evicted.
/// With no expected proof the advisory stays unresolved (honest abstention).
pub fn compute_advisories(input: &AdvisoryInput<'_>) -> AdvisoryReport {
    let mut applicable: BTreeSet<String> = input
        .applicable_trigger_ids
        .iter()
        .map(|id| id.split_once(':').map_or(id.as_str(), |(prefix, _)| prefix))
        .filter(|id| is_trigger_id(id))
        .map(str::to_string)
        .collect();
    for id in applicable_triggers_from_ledger(input.ledger, input.active_task_ids) {
        applicable.insert(id);
    }

    // Expected proof for every escalated trigger, not just T_HIGH_BLAST. The
    // value is the prescribing receipt's `ts` (None when the prescription was
    // passed as a bare signal and its ledger row is gone). A full
    // `<TRIGGER>:<digest>` signal derives the expected proof from the stable
    // next_action digest, so eviction of the prescribing row cannot silently
    // un-resolve a still-applicable prescription.
    let mut expected: BTreeMap<String, Option<i64>> = BTreeMap::new();
    for signal in input.applicable_trigger_ids {
        if let Some((prefix, _)) = signal.split_once(':') {
            if is_trigger_id(prefix) {
                merge_expected(&mut expected, signal, None);
            }
        }
    }
    for event in unique_events(input.ledger) {
        if !attributed(event, input.active_task_ids) {
            continue;
        }
        let ts = receipt_ts(event);
        if let Some(ids) = event.get("prescribed_action_ids").and_then(Value::as_array) {
            for value in ids.iter().filter_map(Value::as_str) {
                if let Some((prefix, _)) = value.split_once(':') {
                    if is_trigger_id(prefix) {
                        merge_expected(&mut expected, value, ts);
                    }
                }
            }
        }
    }

    let mut distinct_tools = BTreeSet::new();
    let mut resolved: BTreeSet<&str> = BTreeSet::new();
    let mut high_blast_resolved = false;
    let mut diff_resolved = false;
    let mut strategic_resolved = false;

    for event in unique_events(input.ledger) {
        if !attributed(event, input.active_task_ids) {
            continue;
        }
        let Some(tool) = event.get("tool").and_then(Value::as_str) else {
            continue;
        };
        if !input.allowed_graph_tools.contains(&tool) {
            continue;
        }
        let status = event.get("status").and_then(Value::as_str).unwrap_or("");
        if !GENUINE_STATUSES.contains(&status) {
            continue;
        }
        distinct_tools.insert(tool.to_string());
        // Resolve each trigger ONLY by the receipt of the tool that actually
        // executes it. A prescribing receipt's `trigger_id` and
        // `prescribed_action_ids` prove applicability, never follow-through.
        match tool {
            PATH_TOOL => {
                resolved.insert(T_PATH);
            }
            AMBIGUOUS_TOOL => {
                resolved.insert(T_AMBIGUOUS);
            }
            SEED_EDGE_TOOL => {
                resolved.insert(T_SEED_EDGE);
            }
            HIGH_BLAST_TOOL => {
                resolved.insert(T_SEED_EXPAND);
                if proof_proves(event, &expected, T_HIGH_BLAST, false) {
                    high_blast_resolved = true;
                    resolved.insert(T_HIGH_BLAST);
                }
            }
            DIFF_TOOL => {
                // A bound prescription must be proved; with no prescription to
                // bind to, an empty diff tour still does not claim follow-through.
                let bound = expected
                    .keys()
                    .any(|id| id.starts_with(&format!("{T_DIFF}:")));
                if (bound && proof_proves(event, &expected, T_DIFF, true))
                    || (!bound && nonempty_outcome(event))
                {
                    diff_resolved = true;
                    resolved.insert(T_DIFF);
                }
            }
            TAXONOMY_TOOL => match event.get("operation").and_then(Value::as_str) {
                Some("advise") => {
                    // Preserve the legacy unbound behavior (a real advise call
                    // resolves the generic strategic signal). Once a concrete
                    // prescription exists, require its proof and a real result.
                    let bound = expected
                        .keys()
                        .any(|id| id.starts_with(&format!("{T_STRATEGIC}:")));
                    if !bound || proof_proves(event, &expected, T_STRATEGIC, true) {
                        strategic_resolved = true;
                        resolved.insert(T_STRATEGIC);
                    }
                }
                Some("briefing") | Some("gaps") => {
                    resolved.insert(T_DOMAIN_NEW);
                }
                _ => {}
            },
            INVARIANT_TOOL => {
                resolved.insert(T_INVARIANT);
            }
            _ => {}
        }
    }

    let high_blast_unexplored = applicable.contains(T_HIGH_BLAST) && !high_blast_resolved;
    let diff_tour_recommended = applicable.contains(T_DIFF) && !diff_resolved;
    let strategic_context_detected = applicable.contains(T_STRATEGIC);
    let strategic_miss = strategic_context_detected && !strategic_resolved;
    let unresolved_trigger_ids: Vec<String> = applicable
        .iter()
        .filter(|id| !resolved.contains(id.as_str()))
        .cloned()
        .collect();

    let mut recommendations = Vec::new();
    if high_blast_unexplored || diff_tour_recommended {
        recommendations.push(W_GRAPH_NO_EXPANSION.to_string());
    }
    if diff_tour_recommended {
        recommendations.push(W_DIFF_TOUR_RECOMMENDED.to_string());
    }
    if strategic_miss {
        recommendations.push(W_ONTOLOGY_STRATEGIC_MISS.to_string());
    }

    AdvisoryReport {
        genuine_lookup_coverage: !distinct_tools.is_empty(),
        distinct_graph_tools: distinct_tools.len(),
        high_blast_radius_unexplored: high_blast_unexplored,
        diff_tour_recommended: diff_tour_recommended,
        taxonomy_available: input.taxonomy_available,
        strategic_context_detected: strategic_context_detected,
        concept_grounding_linked: input.concept_grounding_linked,
        recommendations,
        unresolved_trigger_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn active() -> BTreeSet<String> {
        ["t-active".to_string()].into_iter().collect()
    }

    fn allowed() -> Vec<&'static str> {
        vec!["neurostrata_memories_for_file", HIGH_BLAST_TOOL]
    }

    /// A prescribing `memories_for_file` receipt plus one `related` receipt
    /// carrying the given followed-proof ids (empty = a default one-hop call).
    fn ledger(proof: Vec<&str>) -> Value {
        json!({
            "events": [
                {
                    "task_id": "t-active",
                    "tool": "neurostrata_memories_for_file",
                    "operation": "memories_for_file",
                    "status": "success",
                    "trigger_id": T_HIGH_BLAST,
                    "prescribed_action_ids": ["T_HIGH_BLAST:proof-hops2-seed"]
                },
                {
                    "task_id": "t-active",
                    "tool": HIGH_BLAST_TOOL,
                    "operation": "related",
                    "status": "success",
                    "followed_action_ids": proof
                }
            ]
        })
    }

    fn report(ledger: &Value) -> AdvisoryReport {
        let active = active();
        let applicable = vec![T_HIGH_BLAST.to_string()];
        compute_advisories(&AdvisoryInput {
            ledger,
            active_task_ids: &active,
            allowed_graph_tools: &allowed(),
            applicable_trigger_ids: &applicable,
            taxonomy_available: false,
            concept_grounding_linked: false,
        })
    }

    #[test]
    fn advisory_high_blast_one_hop_without_proof_unresolved() {
        let r = report(&ledger(vec![]));
        assert!(r.high_blast_radius_unexplored);
        assert!(r.recommendations.iter().any(|w| w == W_GRAPH_NO_EXPANSION));
        assert!(r.unresolved_trigger_ids.iter().any(|t| t == T_HIGH_BLAST));
    }

    #[test]
    fn advisory_high_blast_exact_proof_resolves() {
        let r = report(&ledger(vec!["T_HIGH_BLAST:proof-hops2-seed"]));
        assert!(!r.high_blast_radius_unexplored);
        assert!(!r.recommendations.iter().any(|w| w == W_GRAPH_NO_EXPANSION));
        assert!(r.unresolved_trigger_ids.iter().all(|t| t != T_HIGH_BLAST));
    }

    #[test]
    fn advisory_high_blast_wrong_seed_or_hops_stays_unresolved() {
        let r = report(&ledger(vec!["T_HIGH_BLAST:wrong-seed-hops1"]));
        assert!(r.high_blast_radius_unexplored);
        assert!(r.recommendations.iter().any(|w| w == W_GRAPH_NO_EXPANSION));
        assert!(r.unresolved_trigger_ids.iter().any(|t| t == T_HIGH_BLAST));
    }

    #[test]
    fn advisory_high_blast_absent_expected_proof_abstains() {
        let mut l = ledger(vec!["T_HIGH_BLAST:proof-hops2-seed"]);
        l["events"][0]
            .as_object_mut()
            .unwrap()
            .remove("prescribed_action_ids");
        let r = report(&l);
        assert!(r.high_blast_radius_unexplored);
    }

    // Task 11 Fix 2d: a genuine taxonomy `advise` receipt clears the strategic
    // miss; a `tree`/`briefing` receipt must NOT (predicate stays strict).
    #[test]
    fn advisory_strategic_taxonomy_advise_resolves_tree_does_not() {
        let active = active();
        let applicable = vec![T_STRATEGIC.to_string()];
        let allowed = vec![TAXONOMY_TOOL];
        let mk = |op: &str| {
            json!({
                "events": [{
                    "task_id": "t-active",
                    "tool": TAXONOMY_TOOL,
                    "operation": op,
                    "status": "success"
                }]
            })
        };
        let report_for = |le: &Value| {
            compute_advisories(&AdvisoryInput {
                ledger: le,
                active_task_ids: &active,
                allowed_graph_tools: &allowed,
                applicable_trigger_ids: &applicable,
                taxonomy_available: true,
                concept_grounding_linked: false,
            })
            .to_json()
        };

        let advise = report_for(&mk("advise"));
        assert!(advise["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w != W_ONTOLOGY_STRATEGIC_MISS));
        assert!(advise["unresolved_trigger_ids"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t != T_STRATEGIC));

        let tree = report_for(&mk("tree"));
        assert!(tree["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == W_ONTOLOGY_STRATEGIC_MISS));
        assert!(tree["unresolved_trigger_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == T_STRATEGIC));
    }

    // Task 13 P1-2: a prescribing receipt never resolves its own prescription.
    #[test]
    fn advisory_prescription_never_self_resolves_until_executing_tool() {
        let active = active();
        let applicable = vec![T_AMBIGUOUS.to_string()];
        let allowed = vec![PATH_TOOL, AMBIGUOUS_TOOL];
        let prescribing = json!({
            "events": [{
                "event_id": "prescription",
                "task_id": "t-active",
                "tool": PATH_TOOL,
                "trigger_id": T_PATH,
                "status": "empty",
                "prescribed_action_ids": ["T_AMBIGUOUS:abc"]
            }]
        });
        let report_for = |ledger: &Value| {
            compute_advisories(&AdvisoryInput {
                ledger,
                active_task_ids: &active,
                allowed_graph_tools: &allowed,
                applicable_trigger_ids: &applicable,
                taxonomy_available: false,
                concept_grounding_linked: false,
            })
        };
        let alone = report_for(&prescribing);
        assert!(
            alone
                .unresolved_trigger_ids
                .iter()
                .any(|t| t == T_AMBIGUOUS),
            "prescription alone must stay unresolved"
        );
        let mut with_locate = prescribing.clone();
        with_locate["events"].as_array_mut().unwrap().push(json!({
            "event_id": "locate",
            "task_id": "t-active",
            "tool": AMBIGUOUS_TOOL,
            "trigger_id": T_AMBIGUOUS,
            "status": "success"
        }));
        let resolved = report_for(&with_locate);
        assert!(
            resolved
                .unresolved_trigger_ids
                .iter()
                .all(|t| t != T_AMBIGUOUS),
            "a genuine locate receipt resolves T_AMBIGUOUS"
        );
    }

    // Task 13 P1-8: replayed event_id counted once, not twice.
    #[test]
    fn advisory_replayed_event_id_deduped() {
        let active = active();
        let applicable = vec![T_HIGH_BLAST.to_string()];
        let allowed = vec![PATH_TOOL, HIGH_BLAST_TOOL];
        let ledger = json!({
            "events": [
                {
                    "event_id": "same",
                    "task_id": "t-active",
                    "tool": PATH_TOOL,
                    "status": "success",
                    "trigger_id": T_PATH,
                    "prescribed_action_ids": ["T_HIGH_BLAST:proof"]
                },
                {
                    "event_id": "same",
                    "task_id": "t-active",
                    "tool": PATH_TOOL,
                    "status": "success",
                    "trigger_id": T_PATH,
                    "prescribed_action_ids": ["T_HIGH_BLAST:proof"]
                }
            ]
        });
        let report = compute_advisories(&AdvisoryInput {
            ledger: &ledger,
            active_task_ids: &active,
            allowed_graph_tools: &allowed,
            applicable_trigger_ids: &applicable,
            taxonomy_available: false,
            concept_grounding_linked: false,
        });
        assert_eq!(
            report.distinct_graph_tools, 1,
            "a replayed event_id must not raise tool diversity"
        );
    }

    // oy5c C32: a followed call must mint its proof under the trigger it
    // actually followed; `related` can follow T_HIGH_BLAST or T_SEED_EXPAND.
    #[test]
    fn advisory_proof_trigger_for_tool_hint_and_default() {
        assert_eq!(
            proof_trigger_for_tool(HIGH_BLAST_TOOL, None, None).as_deref(),
            None,
            "related without a hint cannot de-alias its two triggers"
        );
        assert_eq!(
            proof_trigger_for_tool(HIGH_BLAST_TOOL, None, Some(T_SEED_EXPAND)).as_deref(),
            Some(T_SEED_EXPAND),
            "an explicit hint must de-alias seed expansion from high blast"
        );
        assert_eq!(
            proof_trigger_for_tool(DIFF_TOOL, Some("diff_tour"), None).as_deref(),
            Some(T_DIFF)
        );
        assert_eq!(
            proof_trigger_for_tool(TAXONOMY_TOOL, Some("advise"), None).as_deref(),
            Some(T_STRATEGIC)
        );
        assert_eq!(
            proof_trigger_for_tool(TAXONOMY_TOOL, Some("tree"), None).as_deref(),
            None,
            "a non-advise taxonomy operation proves no listed trigger"
        );
        // A hint incompatible with the tool is ignored, never trusted.
        assert_eq!(
            proof_trigger_for_tool(DIFF_TOOL, Some("diff_tour"), Some(T_HIGH_BLAST)).as_deref(),
            Some(T_DIFF)
        );
    }

    // oy5c C32 RED: `related` serves two triggers, so without the hint it
    // cannot mint an honest proof — defaulting to T_HIGH_BLAST falsely
    // resolves a T_HIGH_BLAST advisory when the agent followed seed expansion.
    #[test]
    fn advisory_related_without_hint_abstains() {
        assert_eq!(
            proof_trigger_for_tool(HIGH_BLAST_TOOL, None, None).as_deref(),
            None,
            "no hint => no honest trigger for related"
        );
        assert!(
            dispatch_proof(
                HIGH_BLAST_TOOL,
                Some("related"),
                Some(&json!({ "namespace": "NS", "id": "seed", "max_hops": 2 })),
                None,
                "NS"
            )
            .is_none(),
            "a hint-less related call must abstain"
        );
        // A compatibility-checked hint de-aliases seed expansion.
        assert_eq!(
            dispatch_proof(
                HIGH_BLAST_TOOL,
                Some("related"),
                Some(&json!({ "namespace": "NS", "id": "seed", "max_hops": 2 })),
                Some(T_SEED_EXPAND),
                "NS"
            )
            .map(|p| p.trigger)
            .as_deref(),
            Some(T_SEED_EXPAND)
        );
    }

    // oy5c C10/C12/C16: proof minting normalizes hops and abstains on an empty
    // diff. The digest is recomputed from canonical args only.
    #[test]
    fn advisory_dispatch_proof_normalizes_and_abstains() {
        let related = dispatch_proof(
            HIGH_BLAST_TOOL,
            Some("related"),
            Some(&json!({ "namespace": "NS", "id": " seed ", "max_hops": 3 })),
            Some(T_HIGH_BLAST),
            "NS",
        )
        .expect("a genuine related call has a proof plan");
        assert_eq!(related.trigger, T_HIGH_BLAST);
        assert_eq!(
            related.args,
            json!({
                "namespace": "NS", "id": "seed", "max_hops": 2,
                "followed_trigger_id": T_HIGH_BLAST
            }),
            "hops >= 2 must normalize to the prescribed two-hop args"
        );
        assert_eq!(related.ids, vec!["seed".to_string()]);
        assert!(dispatch_proof(
            HIGH_BLAST_TOOL,
            Some("related"),
            Some(&json!({ "namespace": "NS", "id": "seed", "max_hops": 1 })),
            Some(T_HIGH_BLAST),
            "NS"
        )
        .is_none());
        assert!(
            dispatch_proof(
                DIFF_TOOL,
                Some("diff_tour"),
                Some(&json!({ "namespace": "NS", "diff_text": "  " })),
                None,
                "NS"
            )
            .is_none(),
            "an empty diff_text cannot be honestly dispatched"
        );
        let diff = dispatch_proof(
            DIFF_TOOL,
            Some("diff_tour"),
            Some(&json!({ "namespace": "NS", "diff_text": " real diff " })),
            None,
            "NS",
        )
        .expect("a real diff has a proof plan");
        assert_eq!(diff.trigger, T_DIFF);
        assert_eq!(
            diff.args,
            json!({ "namespace": "NS", "diff_text": " real diff " })
        );
    }

    // oy5c C12/C13: resolution receipt must be at or after the prescription.
    #[test]
    fn advisory_temporal_ordering_rejects_earlier_receipt() {
        let ledger = |ts: i64| {
            json!({
                "events": [
                    {
                        "task_id": "t-active",
                        "tool": PATH_TOOL,
                        "status": "success",
                        "ts": 100,
                        "prescribed_action_ids": ["T_HIGH_BLAST:p"]
                    },
                    {
                        "task_id": "t-active",
                        "tool": HIGH_BLAST_TOOL,
                        "status": "success",
                        "ts": ts,
                        "followed_action_ids": ["T_HIGH_BLAST:p"]
                    }
                ]
            })
        };
        assert!(report(&ledger(99)).high_blast_radius_unexplored);
        assert!(!report(&ledger(101)).high_blast_radius_unexplored);
    }

    // oy5c item 7: an applicable prescription passed as a full action id
    // survives eviction of the prescribing ledger row.
    #[test]
    fn advisory_eviction_signal_action_id_still_resolves() {
        let active = active();
        let allowed = vec![HIGH_BLAST_TOOL];
        let ledger = json!({
            "events": [{
                "task_id": "t-active",
                "tool": HIGH_BLAST_TOOL,
                "status": "success",
                "followed_action_ids": ["T_HIGH_BLAST:evicted-proof"]
            }]
        });
        let applicable = vec![
            T_HIGH_BLAST.to_string(),
            "T_HIGH_BLAST:evicted-proof".to_string(),
        ];
        let r = compute_advisories(&AdvisoryInput {
            ledger: &ledger,
            active_task_ids: &active,
            allowed_graph_tools: &allowed,
            applicable_trigger_ids: &applicable,
            taxonomy_available: false,
            concept_grounding_linked: false,
        });
        assert!(!r.high_blast_radius_unexplored);
    }

    // oy5c C32: a seed-expansion proof cannot resolve a high-blast
    // prescription even when the digest half is identical.
    #[test]
    fn advisory_cross_trigger_proof_cannot_alias() {
        let l = ledger(vec!["T_SEED_EXPAND:proof-hops2-seed"]);
        let applicable = vec![T_HIGH_BLAST.to_string()];
        let active = active();
        let r = compute_advisories(&AdvisoryInput {
            ledger: &l,
            active_task_ids: &active,
            allowed_graph_tools: &allowed(),
            applicable_trigger_ids: &applicable,
            taxonomy_available: false,
            concept_grounding_linked: false,
        });
        assert!(r.high_blast_radius_unexplored);
    }

    // oy5c C12/C13/C16: T_DIFF binds to a proof tied to the prescribed action
    // and never resolves on an empty outcome.
    #[test]
    fn advisory_diff_requires_bound_proof_and_outcome() {
        let active = active();
        let allowed = vec![PATH_TOOL, DIFF_TOOL];
        let applicable = vec![T_DIFF.to_string()];
        let mk = |proof: Vec<&str>, count: u64, status: &str| {
            json!({
                "events": [
                    {
                        "task_id": "t-active",
                        "tool": PATH_TOOL,
                        "status": "success",
                        "ts": 10,
                        "prescribed_action_ids": ["T_DIFF:diff-proof"]
                    },
                    {
                        "task_id": "t-active",
                        "tool": DIFF_TOOL,
                        "status": status,
                        "ts": 11,
                        "result_count": count,
                        "followed_action_ids": proof
                    }
                ]
            })
        };
        let check = |ledger: &Value| {
            compute_advisories(&AdvisoryInput {
                ledger,
                active_task_ids: &active,
                allowed_graph_tools: &allowed,
                applicable_trigger_ids: &applicable,
                taxonomy_available: false,
                concept_grounding_linked: false,
            })
            .diff_tour_recommended
        };
        assert!(
            check(&mk(vec![], 2, "success")),
            "no proof stays unresolved"
        );
        assert!(
            check(&mk(vec!["T_DIFF:diff-proof"], 0, "empty")),
            "empty outcome stays unresolved"
        );
        assert!(!check(&mk(vec!["T_DIFF:diff-proof"], 2, "success")));
    }

    // oy5c C18: a bound T_STRATEGIC claim needs proof plus a non-empty result.
    #[test]
    fn advisory_strategic_bound_proof_requires_outcome() {
        let active = active();
        let applicable = vec![T_STRATEGIC.to_string()];
        let mk = |proof: Vec<&str>, count: u64| {
            json!({
                "events": [
                    {
                        "task_id": "t-active",
                        "tool": PATH_TOOL,
                        "status": "success",
                        "ts": 10,
                        "prescribed_action_ids": ["T_STRATEGIC:advice-proof"]
                    },
                    {
                        "task_id": "t-active",
                        "tool": TAXONOMY_TOOL,
                        "operation": "advise",
                        "status": "success",
                        "ts": 11,
                        "result_count": count,
                        "followed_action_ids": proof
                    }
                ]
            })
        };
        let check = |ledger: &Value| {
            compute_advisories(&AdvisoryInput {
                ledger,
                active_task_ids: &active,
                allowed_graph_tools: &vec![PATH_TOOL, TAXONOMY_TOOL],
                applicable_trigger_ids: &applicable,
                taxonomy_available: true,
                concept_grounding_linked: false,
            })
            .recommendations
            .iter()
            .any(|w| w == W_ONTOLOGY_STRATEGIC_MISS)
        };
        assert!(check(&mk(vec![], 3)), "no proof stays a strategic miss");
        assert!(
            check(&mk(vec!["T_STRATEGIC:advice-proof"], 0)),
            "empty result stays a miss"
        );
        assert!(!check(&mk(vec!["T_STRATEGIC:advice-proof"], 3)));
    }
}
