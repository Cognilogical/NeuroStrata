//! Task-validation graph/ontology advisories (design spec §6 R7).
//!
//! Crate-free by design: the integration test compiles this module from its
//! real path, the same way `snapshot_readiness.rs` and `routing.rs` are shared.
//! Pure input only: no store, clock, or filesystem access here. Every value is
//! additive to `task_validate`; nothing computed here is ever copied into a
//! gate `violations` array.

use serde_json::{json, Value};
use std::collections::BTreeSet;

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
const HIGH_BLAST_TOOL: &str = "neurostrata_related";
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
        .filter(|event| match event.get("event_id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => seen.insert(id),
            _ => true,
        })
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

/// Compute all R7 advisories. A genuine receipt is an attributed `events[]`
/// entry with an allowlisted tool and a `success`/`empty` status. Legacy
/// `queries[]` rows and `tool:"pre-commit"` are never read: only `events[]`
/// reaches this function. An escalated trigger is resolved by its OWN tool
/// receipt: `T_HIGH_BLAST` needs a genuine `neurostrata_related` receipt whose
/// `followed_action_ids` exactly matches the prescribed action id emitted by
/// the `T_HIGH_BLAST`-triggering receipt. A default one-hop lookup, a different
/// seed or a different hop count cannot stand in for it. With no expected
/// proof the advisory stays unresolved (honest abstention).
pub fn compute_advisories(input: &AdvisoryInput<'_>) -> AdvisoryReport {
    let mut applicable: BTreeSet<String> = input
        .applicable_trigger_ids
        .iter()
        .filter(|id| is_trigger_id(id))
        .cloned()
        .collect();
    for id in applicable_triggers_from_ledger(input.ledger, input.active_task_ids) {
        applicable.insert(id);
    }

    // Exact proof of the prescribed two-hop expansion: the action ids emitted
    // by an attributed T_HIGH_BLAST-triggering receipt. The ledger is
    // digest-only, so depth/seed cannot be parsed back out of a receipt; only
    // the action-id digest can prove the prescribed call actually ran.
    let mut expected_action_ids: BTreeSet<&str> = BTreeSet::new();
    for event in unique_events(input.ledger) {
        if !attributed(event, input.active_task_ids) {
            continue;
        }
        // Prescriptions live in `prescribed_action_ids` as `<TRIGGER>:<hash>`;
        // a prescribing receipt never claims the trigger in its own
        // `trigger_id` (that slot identifies the tool that actually ran).
        if let Some(ids) = event.get("prescribed_action_ids").and_then(Value::as_array) {
            for value in ids {
                if let Some(id) = value
                    .as_str()
                    .filter(|s| s.starts_with("T_HIGH_BLAST:"))
                {
                    expected_action_ids.insert(id);
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
                let proved = !expected_action_ids.is_empty()
                    && event
                        .get("followed_action_ids")
                        .and_then(Value::as_array)
                        .map(|ids| {
                            ids.iter()
                                .filter_map(Value::as_str)
                                .any(|id| expected_action_ids.contains(id))
                        })
                        .unwrap_or(false);
                if proved {
                    high_blast_resolved = true;
                    resolved.insert(T_HIGH_BLAST);
                }
            }
            DIFF_TOOL => {
                diff_resolved = true;
                resolved.insert(T_DIFF);
            }
            TAXONOMY_TOOL => match event.get("operation").and_then(Value::as_str) {
                Some("advise") => {
                    strategic_resolved = true;
                    resolved.insert(T_STRATEGIC);
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
            alone.unresolved_trigger_ids.iter().any(|t| t == T_AMBIGUOUS),
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
            resolved.unresolved_trigger_ids.iter().all(|t| t != T_AMBIGUOUS),
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
}
