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

const HIGH_BLAST_TOOL: &str = "neurostrata_related";
const DIFF_TOOL: &str = "neurostrata_diff_tour";
const TAXONOMY_TOOL: &str = "neurostrata_taxonomy";
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
    for event in events(ledger) {
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
/// receipt; a genuine one-hop lookup cannot stand in for it.
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

    let mut distinct_tools = BTreeSet::new();
    let mut resolved: BTreeSet<&str> = BTreeSet::new();
    let mut high_blast_resolved = false;
    let mut diff_resolved = false;
    let mut strategic_resolved = false;

    for event in events(input.ledger) {
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
        if let Some(id) = event.get("trigger_id").and_then(Value::as_str) {
            resolved.insert(id);
        }
        match tool {
            HIGH_BLAST_TOOL => {
                high_blast_resolved = true;
                resolved.insert(T_HIGH_BLAST);
            }
            DIFF_TOOL => {
                diff_resolved = true;
                resolved.insert(T_DIFF);
            }
            TAXONOMY_TOOL if event.get("operation").and_then(Value::as_str) == Some("advise") => {
                strategic_resolved = true;
                resolved.insert(T_STRATEGIC);
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
