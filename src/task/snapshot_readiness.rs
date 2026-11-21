//! Snapshot graph/ontology readiness section (design spec §6 R5).
//!
//! Pure data plus deterministic rendering: no store access, no I/O, no clock,
//! no randomness. The snapshot handler gathers ReBAC-filtered taxonomy rows
//! and active-task tool usage, then calls `render_readiness_section` so the
//! section cannot drift from its contract test. Crate-free by design: the
//! integration test includes this file directly via `#[path]`.

use serde_json::Value;
use std::collections::BTreeSet;

/// Exact heading required by R5.1.
pub const SECTION_HEADING: &str = "### Graph & Ontology Readiness";

/// Exact zero-concept hint required by R5.2 (emitted as its own line).
pub const ZERO_CONCEPT_HINT: &str = "Taxonomy (0 concepts): run neurostrata_taxonomy(operation:'set_thesis') to anchor domain concepts.";

/// R5.1: at most three recommended reads.
pub const MAX_RECOMMENDED_READS: usize = 3;

/// Task statuses that still represent work in flight. `done` is deliberately
/// absent: finished tasks must not attract tool-receipt attribution.
pub const ACTIVE_TASK_STATUSES: &[&str] = &["open", "in_progress", "blocked"];

/// Graph/ontology MCP tools eligible for diversity counting.
pub const GRAPH_TOOLS: &[&str] = &[
    "neurostrata_memories_for_file",
    "neurostrata_related",
    "neurostrata_locate",
    "neurostrata_sparql_query",
    "neurostrata_object_view",
    "neurostrata_taxonomy",
    "neurostrata_diff_tour",
    "neurostrata_search_memory",
];

/// IDs of tasks still in flight from `(id, status)` pairs. Unknown/done
/// statuses are excluded. Caller maps its own status enum to these strings.
pub fn active_task_ids(rows: &[(String, String)]) -> BTreeSet<String> {
    rows.iter()
        .filter(|(_, status)| ACTIVE_TASK_STATUSES.contains(&status.as_str()))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Genuine outcomes: a real tool ran and either found something or honestly
/// found nothing. `failed`/`unsupported` are not coverage and never count
/// toward tool diversity.
const GENUINE_STATUSES: &[&str] = &["success", "empty"];

/// Distinct allowlisted graph tools from attributed post-dispatch events only.
/// Legacy `queries[]` rows are ignored by design (spec R6 excludes them from
/// quality metrics), so a ledger containing only legacy rows yields an empty
/// set even when it names allowlisted tools or `tool:"pre-commit"`.
pub fn graph_tools_from_ledger(ledger: &Value, active_ids: &BTreeSet<String>) -> Vec<String> {
    let Some(events) = ledger.get("events").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut tools: BTreeSet<String> = BTreeSet::new();
    let mut seen_event_ids: BTreeSet<&str> = BTreeSet::new();
    for event in events {
        let attributed = event
            .get("task_id")
            .and_then(Value::as_str)
            .map(|id| active_ids.contains(id))
            .unwrap_or(false);
        if !attributed {
            continue;
        }
        let status = event.get("status").and_then(Value::as_str).unwrap_or("");
        if !GENUINE_STATUSES.contains(&status) {
            continue;
        }
        // A replayed receipt (same non-empty event_id) is one lookup, not two.
        if let Some(event_id) = event
            .get("event_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            if !seen_event_ids.insert(event_id) {
                continue;
            }
        }
        if let Some(tool) = event.get("tool").and_then(Value::as_str) {
            if GRAPH_TOOLS.contains(&tool) {
                tools.insert(tool.to_string());
            }
        }
    }
    tools.into_iter().collect()
}

/// One taxonomy concept as seen by a caller after ReBAC filtering.
///
/// `restricted` is defense-in-depth: production fills it from the same
/// `is_public_access_policy` gate that filters the snapshot taxonomy view, so
/// it is always false there. The field exists so the pure function itself can
/// never count or name a restricted row if a caller forwards one by mistake.
#[derive(Debug, Clone)]
pub struct ReadinessConcept {
    pub slug: String,
    pub restricted: bool,
    /// Read-time epistemic status; `"seed"`/`"defined"` count as ungrounded.
    pub epistemic_status: String,
}

/// Caller-supplied inputs. Every field is input-only; this module never reads
/// the store or infers data it was not given.
#[derive(Debug, Clone)]
pub struct ReadinessInput {
    pub concepts: Vec<ReadinessConcept>,
    /// Graph/ontology MCP tool names recorded on active tasks.
    pub graph_tool_names: Vec<String>,
    /// Whether an unrestricted thesis row is visible.
    pub has_thesis: bool,
}

/// Computed readiness metrics. All fields are additive to the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessReport {
    pub taxonomy_concepts_count: usize,
    pub ungrounded_concepts_count: usize,
    pub active_task_graph_tool_diversity: usize,
    pub recommended_reads: Vec<String>,
}

/// Count visible concepts (deduped by slug), ungrounded ones, and distinct
/// active-task graph tools. Restricted concepts are excluded from every total.
pub fn compute_readiness(input: &ReadinessInput) -> ReadinessReport {
    let mut visible_slugs: BTreeSet<&str> = BTreeSet::new();
    let mut ungrounded_slugs: BTreeSet<&str> = BTreeSet::new();
    for concept in &input.concepts {
        if concept.restricted {
            continue;
        }
        let slug = concept.slug.trim();
        if slug.is_empty() || !visible_slugs.insert(slug) {
            continue;
        }
        if matches!(concept.epistemic_status.as_str(), "seed" | "defined" | "") {
            ungrounded_slugs.insert(slug);
        }
    }

    let mut distinct_tools: BTreeSet<&str> = BTreeSet::new();
    for tool in &input.graph_tool_names {
        let tool = tool.trim();
        if !tool.is_empty() {
            distinct_tools.insert(tool);
        }
    }

    let mut report = ReadinessReport {
        taxonomy_concepts_count: visible_slugs.len(),
        ungrounded_concepts_count: ungrounded_slugs.len(),
        active_task_graph_tool_diversity: distinct_tools.len(),
        recommended_reads: Vec::new(),
    };
    report.recommended_reads = recommended_reads(&report);
    report
}

/// Deterministic, read-only follow-ups. Empty ontology needs no read (the
/// zero-concept hint already names `set_thesis`).
fn recommended_reads(report: &ReadinessReport) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if report.taxonomy_concepts_count > 0 {
        out.push("neurostrata_taxonomy(operation:'briefing')".to_string());
        if report.ungrounded_concepts_count > 0 {
            out.push("neurostrata_taxonomy(operation:'gaps')".to_string());
        }
        if report.active_task_graph_tool_diversity < 2 {
            out.push("neurostrata_memories_for_file".to_string());
        }
    }
    out.truncate(MAX_RECOMMENDED_READS);
    out
}

/// Render the complete readiness section, including the R5.2 hint when there
/// is neither a thesis nor any visible concept.
pub fn render_readiness_section(input: &ReadinessInput) -> String {
    let report = compute_readiness(input);
    let mut lines: Vec<String> = vec![
        SECTION_HEADING.to_string(),
        format!(
            "  - taxonomy_concepts_count: {}",
            report.taxonomy_concepts_count
        ),
        format!(
            "  - ungrounded_concepts_count: {}",
            report.ungrounded_concepts_count
        ),
        format!(
            "  - active_task_graph_tool_diversity: {}",
            report.active_task_graph_tool_diversity
        ),
    ];
    let reads = if report.recommended_reads.is_empty() {
        "none".to_string()
    } else {
        report.recommended_reads.join(", ")
    };
    lines.push(format!("  - recommended_reads: {reads}"));
    if report.taxonomy_concepts_count == 0 && !input.has_thesis {
        lines.push(ZERO_CONCEPT_HINT.to_string());
    }
    lines.join("\n")
}
