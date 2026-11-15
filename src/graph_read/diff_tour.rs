use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use petgraph::algo::{tarjan_scc, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unidiff::PatchSet;

use crate::formal_kernel::{classify_tour_stop, TourStopCategory};
use crate::graph_read::locate::{LocateFailedAttempt, LocateRuleSummary, RULE_SUMMARY_MAX_CHARS};
use crate::rebac::{can_read_memory, resolve_principal};
use crate::traits::{GraphRead, SearchResult, VectorStore};

pub const DEFAULT_MAX_BLAST_RADIUS_CALLERS: usize = 15;
pub const MAX_DIFF_HUNKS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffTourRequest {
    pub namespace: String,
    /// Optional project root (verified via check_caller_path on /diff-tour).
    #[serde(default)]
    pub project_root: Option<String>,
    /// Raw unified diff text. Preferred & 100% VCS-agnostic.
    #[serde(default)]
    pub diff_text: Option<String>,
    /// Optional explicit file/line spans when caller already parsed the diff.
    #[serde(default)]
    pub changed_spans: Option<Vec<ChangedFileSpan>>,
    #[serde(default)]
    pub max_blast_radius_callers: Option<usize>,
    #[serde(default)]
    pub principal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFileSpan {
    pub path: String,
    #[serde(default)]
    pub old_lines: Option<(u32, u32)>,
    #[serde(default)]
    pub new_lines: Option<(u32, u32)>,
    pub change_kind: String, // "added" | "modified" | "deleted" | "renamed"
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TourItem {
    pub path: String,
    pub lines: String,
    pub symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub change_kind: String,
    pub rationale: String,
    pub modified_callees_in_diff: Vec<String>,
    pub modified_callers_in_diff: Vec<String>,
    pub unchanged_callers: Vec<String>,
    pub governing_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TourStop {
    pub stop_number: u8,  // 1..=4
    pub category: String, // "foundations" | "core_logic" | "surface_and_callers" | "tests_and_docs"
    pub title: String,
    pub items: Vec<TourItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffTourResponse {
    pub namespace: String,
    pub total_files_changed: usize,
    pub total_symbols_touched: usize,
    pub stops: Vec<TourStop>,
    pub governing_rules: Vec<LocateRuleSummary>,
    pub failed_attempts: Vec<LocateFailedAttempt>,
    pub untested_modified_symbols: Vec<String>,
    pub uningested_files: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
}

fn clean_diff_path(raw: &str) -> String {
    let trimmed = raw.trim().split('\t').next().unwrap_or(raw).trim();
    let stripped = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    if stripped == "/dev/null" {
        return String::new();
    }
    crate::parser::ingest::normalize_node_path(stripped)
}

fn parse_hunk_range_token(token: &str) -> Option<(u32, u32)> {
    let digits = token.trim_start_matches(|c: char| c == '-' || c == '+');
    if let Some((start_s, len_s)) = digits.split_once(',') {
        let start = start_s.parse::<u32>().ok()?;
        let len = len_s.parse::<u32>().ok()?;
        if start == 0 && len == 0 {
            return None;
        }
        let end = start.saturating_add(len.saturating_sub(1)).max(start);
        Some((start, end))
    } else {
        let start = digits.parse::<u32>().ok()?;
        if start == 0 {
            return None;
        }
        Some((start, start))
    }
}

/// Parse unified diff text into `ChangedFileSpan` items using `unidiff::PatchSet`,
/// with a header-tolerant fallback for compact/header-only unified diff fixtures.
pub fn parse_unified_diff_spans(diff_text: &str) -> (Vec<ChangedFileSpan>, bool) {
    let mut spans = Vec::new();
    let mut truncated = false;

    let mut patch = PatchSet::new();
    if patch.parse(diff_text).is_ok() && !patch.files().is_empty() {
        for file in patch.files() {
            let target = clean_diff_path(&file.target_file);
            let source = clean_diff_path(&file.source_file);
            let is_renamed = !source.is_empty() && !target.is_empty() && source != target;
            let path = if !target.is_empty() { target } else { source };
            if path.is_empty() {
                continue;
            }
            let change_kind = if file.is_added_file() {
                "added"
            } else if file.is_removed_file() {
                "deleted"
            } else if is_renamed {
                "renamed"
            } else {
                "modified"
            };
            if file.hunks().is_empty() {
                if spans.len() >= MAX_DIFF_HUNKS {
                    truncated = true;
                    break;
                }
                spans.push(ChangedFileSpan {
                    path,
                    old_lines: None,
                    new_lines: Some((1, 1)),
                    change_kind: change_kind.to_string(),
                });
                continue;
            }
            for hunk in file.hunks() {
                if spans.len() >= MAX_DIFF_HUNKS {
                    truncated = true;
                    break;
                }
                let old_lines = if hunk.source_start == 0 && hunk.source_length == 0 {
                    None
                } else {
                    let s = hunk.source_start as u32;
                    let e = (hunk.source_start + hunk.source_length.saturating_sub(1))
                        .max(hunk.source_start) as u32;
                    Some((s, e))
                };
                let new_lines = if hunk.target_start == 0 && hunk.target_length == 0 {
                    None
                } else {
                    let s = hunk.target_start as u32;
                    let e = (hunk.target_start + hunk.target_length.saturating_sub(1))
                        .max(hunk.target_start) as u32;
                    Some((s, e))
                };
                spans.push(ChangedFileSpan {
                    path: path.clone(),
                    old_lines,
                    new_lines,
                    change_kind: change_kind.to_string(),
                });
            }
            if truncated {
                break;
            }
        }
    }

    // Also scan standard `--- ` / `+++ ` / `@@ ` headers so multi-file diffs that
    // omit `diff --git` separators or have compact hunk bodies never drop files.
    let mut seen_file_hunks: BTreeSet<(String, Option<(u32, u32)>, Option<(u32, u32)>)> = spans
        .iter()
        .map(|s| (s.path.clone(), s.old_lines, s.new_lines))
        .collect();
    let mut current_source = String::new();
    let mut current_target = String::new();
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            current_source = clean_diff_path(rest);
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            current_target = clean_diff_path(rest);
        } else if line.starts_with("@@ ") {
            let path = if !current_target.is_empty() {
                current_target.clone()
            } else {
                current_source.clone()
            };
            if path.is_empty() {
                continue;
            }
            let change_kind = if current_source.is_empty() {
                "added"
            } else if current_target.is_empty() {
                "deleted"
            } else if current_source != current_target {
                "renamed"
            } else {
                "modified"
            };
            let parts: Vec<&str> = line.split_whitespace().collect();
            let old_lines = parts.get(1).and_then(|t| parse_hunk_range_token(t));
            let new_lines = parts.get(2).and_then(|t| parse_hunk_range_token(t));
            let key = (path.clone(), old_lines, new_lines);
            if seen_file_hunks.insert(key) {
                if spans.len() >= MAX_DIFF_HUNKS {
                    truncated = true;
                    break;
                }
                spans.push(ChangedFileSpan {
                    path,
                    old_lines,
                    new_lines,
                    change_kind: change_kind.to_string(),
                });
            }
        }
    }

    (spans, truncated)
}

fn is_superseded_or_archived(meta: &Value) -> bool {
    if meta
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if meta
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(vt) = meta.get("valid_to") {
        if !vt.is_null() {
            if let Some(s) = vt.as_str() {
                if !s.is_empty() {
                    return true;
                }
            } else if vt.as_i64().is_some() {
                return true;
            }
        }
    }
    false
}

fn is_structural_type(memory_type: &str) -> bool {
    matches!(
        memory_type,
        "code_ast" | "symbol" | "file" | "doc_section" | "directory" | "markdown"
    )
}

fn is_normative_type(memory_type: &str) -> bool {
    matches!(memory_type, "rule" | "fact" | "procedure" | "context")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{prefix}...")
}

fn extract_span_path(row: &SearchResult) -> Option<String> {
    let loc = row.payload.location.trim();
    if !loc.is_empty() && loc != "global" && loc != "unknown" {
        let base = loc.split('#').next().unwrap_or(loc);
        let norm = crate::parser::ingest::normalize_node_path(base);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    if let Some(path) = row
        .payload
        .metadata
        .get("relative_path")
        .or_else(|| row.payload.metadata.get("path"))
        .and_then(|v| v.as_str())
    {
        let norm = crate::parser::ingest::normalize_node_path(path);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    None
}

fn extract_symbol_name(row: &SearchResult) -> Option<String> {
    if let Some(s) = row
        .payload
        .metadata
        .get("symbol")
        .or_else(|| row.payload.metadata.get("symbol_name"))
        .and_then(|v| v.as_str())
    {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let loc = row.payload.location.trim();
    if let Some((_, frag)) = loc.split_once('#') {
        let after_colon = frag.split_once(':').map(|(_, n)| n).unwrap_or(frag);
        let before_at = after_colon
            .split_once('@')
            .map(|(n, _)| n)
            .unwrap_or(after_colon)
            .trim();
        if !before_at.is_empty() {
            return Some(before_at.to_string());
        }
    }
    None
}

fn extract_symbol_kind(row: &SearchResult) -> Option<String> {
    row.payload
        .metadata
        .get("symbol_kind")
        .or_else(|| row.payload.metadata.get("ast_kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_line_range_str(lines: &str) -> Option<(u32, u32)> {
    let trimmed = lines.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((s, e)) = trimmed.split_once('-') {
        let start = s.trim().parse::<u32>().ok()?;
        let end = e.trim().parse::<u32>().ok()?;
        Some((start, end.max(start)))
    } else {
        let line = trimmed.parse::<u32>().ok()?;
        Some((line, line))
    }
}

fn ranges_overlap(a: (u32, u32), b: (u32, u32)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

fn span_intersects_symbol(span: &ChangedFileSpan, sym_range: Option<(u32, u32)>) -> bool {
    let Some(sr) = sym_range else {
        return true;
    };
    if let Some(nr) = span.new_lines {
        if ranges_overlap(nr, sr) {
            return true;
        }
    }
    if let Some(or) = span.old_lines {
        if ranges_overlap(or, sr) {
            return true;
        }
    }
    span.new_lines.is_none() && span.old_lines.is_none()
}

fn format_span_lines(span: &ChangedFileSpan) -> String {
    if let Some((s, e)) = span.new_lines {
        format!("{s}-{e}")
    } else if let Some((s, e)) = span.old_lines {
        format!("{s}-{e}")
    } else {
        "1-1".to_string()
    }
}

fn is_test_or_doc_item(
    path: &str,
    memory_type: Option<&str>,
    symbol: Option<&str>,
    content: Option<&str>,
) -> bool {
    if matches!(memory_type, Some("doc_section") | Some("markdown")) {
        return true;
    }
    let p = path.to_ascii_lowercase();
    if p.starts_with("docs/")
        || p.starts_with("tests/")
        || p.contains("/tests/")
        || p.ends_with(".md")
        || p.ends_with("_test.rs")
        || p.ends_with(".test.ts")
        || p.ends_with(".spec.ts")
    {
        return true;
    }
    if let Some(sym) = symbol {
        let s = sym.to_ascii_lowercase();
        if s.starts_with("test_") || s.ends_with("_test") || s == "tests" {
            return true;
        }
    }
    if let Some(body) = content {
        if body.contains("#[test]") || body.contains("#[tokio::test") {
            return true;
        }
    }
    false
}

fn is_foundation_kind_or_path_item(path: &str, symbol_kind: Option<&str>) -> bool {
    let p = path.to_ascii_lowercase();
    if p == "src/traits.rs"
        || p == "src/schema.json"
        || p.ends_with("/traits.rs")
        || p.ends_with("/schema.json")
        || p.ends_with("/types.rs")
    {
        return true;
    }
    if let Some(kind) = symbol_kind {
        let k = kind.to_ascii_lowercase();
        if matches!(
            k.as_str(),
            "structs"
                | "struct"
                | "enums"
                | "enum"
                | "traits"
                | "trait"
                | "interfaces"
                | "interface"
                | "types"
                | "type"
                | "classes"
                | "class"
        ) {
            return true;
        }
    }
    false
}

fn is_entrypoint_path_item(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    matches!(
        p.as_str(),
        "src/main.rs" | "src/server.rs" | "src/daemon.rs" | "plugins/core/index.ts"
    ) || p.ends_with("/main.rs")
        || p.ends_with("/server.rs")
        || p.ends_with("/daemon.rs")
}

fn extract_dependency_ids(meta: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for key in [
        "depends_on",
        "calls_resolved",
        "cross_file_callees",
        "scip_callees",
    ] {
        if let Some(arr) = meta.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        out.insert(s.to_string());
                    }
                } else if let Some(id) = item
                    .get("target_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                {
                    if !id.is_empty() {
                        out.insert(id.to_string());
                    }
                }
            }
        }
    }
    out
}

fn extract_caller_ids(meta: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for key in ["cross_file_callers", "scip_callers"] {
        if let Some(arr) = meta.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        out.insert(s.to_string());
                    }
                } else if let Some(id) = item
                    .get("caller_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                {
                    if !id.is_empty() {
                        out.insert(id.to_string());
                    }
                }
            }
        }
    }
    out
}

fn extract_governed_targets(row: &SearchResult) -> Vec<(String, Option<String>, Option<String>)> {
    let mut out: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    let loc = row.payload.location.trim();
    if !loc.is_empty() && loc != "global" && loc != "unknown" {
        let base = crate::parser::ingest::normalize_node_path(loc.split('#').next().unwrap_or(loc));
        if !base.is_empty() {
            let lines = (!row.payload.location_lines.trim().is_empty())
                .then(|| row.payload.location_lines.trim().to_string());
            let sym = extract_symbol_name(row);
            out.push((base, sym, lines));
        }
    }
    if let Some(locs) = row
        .payload
        .metadata
        .get("locations")
        .and_then(|v| v.as_array())
    {
        for entry in locs {
            if let Some(p) = entry.get("path").and_then(|v| v.as_str()) {
                let norm = crate::parser::ingest::normalize_node_path(p);
                if !norm.is_empty() {
                    let sym = entry
                        .get("symbol")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let lines = entry
                        .get("lines")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    out.push((norm, sym, lines));
                }
            }
        }
    }
    if let Some(gov) = row.payload.metadata.get("governs") {
        match gov {
            Value::String(s) => {
                let norm = crate::parser::ingest::normalize_node_path(s);
                if !norm.is_empty() {
                    out.push((norm, None, None));
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let norm = crate::parser::ingest::normalize_node_path(s);
                        if !norm.is_empty() {
                            out.push((norm, None, None));
                        }
                    } else if let Some(p) = item.get("path").and_then(|v| v.as_str()) {
                        let norm = crate::parser::ingest::normalize_node_path(p);
                        if !norm.is_empty() {
                            let sym = item
                                .get("symbol")
                                .and_then(|v| v.as_str())
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty());
                            let lines = item
                                .get("lines")
                                .and_then(|v| v.as_str())
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty());
                            out.push((norm, sym, lines));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn path_matches_governance_target(file_path: &str, target_path: &str) -> bool {
    let f = crate::parser::ingest::normalize_node_path(file_path);
    let t = crate::parser::ingest::normalize_node_path(target_path);
    if f.is_empty() || t.is_empty() {
        return false;
    }
    if f == t {
        return true;
    }
    let prefix = format!("{}/", t.trim_end_matches('/'));
    f.starts_with(&prefix)
}

fn extract_failed_attempt(row: &SearchResult) -> Option<LocateFailedAttempt> {
    let attempt = row.payload.metadata.get("attempt")?.as_object()?;
    let outcome = attempt
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !outcome.eq_ignore_ascii_case("failed") {
        return None;
    }
    let approach = attempt
        .get("approach")
        .and_then(|v| v.as_str())
        .map(|s| truncate_chars(s, 200))
        .unwrap_or_else(|| truncate_chars(&row.payload.content, 200));
    let failure_mode = attempt
        .get("failure_mode")
        .and_then(|v| v.as_str())
        .map(|s| truncate_chars(s, 200))
        .unwrap_or_else(|| "unspecified failure mode".to_string());
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for (p, _, _) in extract_governed_targets(row) {
        paths.insert(p);
    }
    Some(LocateFailedAttempt {
        id: row.id.clone(),
        approach,
        failure_mode,
        paths: paths.into_iter().collect(),
    })
}

#[derive(Debug, Clone)]
struct StagedTourNode {
    row_id: Option<String>,
    path: String,
    lines: String,
    start_line: u32,
    symbol: Option<String>,
    symbol_kind: Option<String>,
    memory_type: Option<String>,
    content: Option<String>,
    change_kind: String,
}

fn format_symbol_label(path: &str, symbol: Option<&str>) -> String {
    match symbol {
        Some(sym) if !sym.is_empty() => format!("{path}::{sym}"),
        _ => path.to_string(),
    }
}

/// Order nodes within a stop so callees precede callers (reverse topological order of
/// `DEPENDS_ON` edges `caller -> callee`), collapsing strongly connected components via
/// `petgraph::algo::tarjan_scc` and `petgraph::algo::toposort`.
fn order_nodes_callee_first(
    mut indices: Vec<usize>,
    nodes: &[StagedTourNode],
    callees_in_diff_idx: &HashMap<usize, BTreeSet<usize>>,
) -> Vec<usize> {
    if indices.len() <= 1 {
        return indices;
    }
    indices.sort_by(|&a, &b| {
        nodes[a]
            .path
            .cmp(&nodes[b].path)
            .then_with(|| nodes[a].start_line.cmp(&nodes[b].start_line))
            .then_with(|| nodes[a].symbol.cmp(&nodes[b].symbol))
    });

    let pos_of: HashMap<usize, usize> = indices
        .iter()
        .enumerate()
        .map(|(pos, &orig)| (orig, pos))
        .collect();

    // Build dependency graph where edge `callee -> caller` means `callee` must come first.
    let mut graph: DiGraph<usize, ()> = DiGraph::new();
    let graph_nodes: Vec<NodeIndex> = indices.iter().map(|&orig| graph.add_node(orig)).collect();
    for &caller_orig in &indices {
        let caller_pos = pos_of[&caller_orig];
        if let Some(callees) = callees_in_diff_idx.get(&caller_orig) {
            for &callee_orig in callees {
                if let Some(&callee_pos) = pos_of.get(&callee_orig) {
                    if callee_pos != caller_pos {
                        graph.add_edge(graph_nodes[callee_pos], graph_nodes[caller_pos], ());
                    }
                }
            }
        }
    }

    if let Ok(sorted) = toposort(&graph, None) {
        return sorted.into_iter().map(|nx| graph[nx]).collect();
    }

    // Cyclic dependencies exist: condense SCCs via Tarjan's algorithm.
    // `tarjan_scc` returns SCCs in reverse topological order (sinks first);
    // since our edges are `callee -> caller`, reversing `sccs` puts callees first.
    let mut sccs = tarjan_scc(&graph);
    sccs.reverse();
    let mut ordered = Vec::with_capacity(indices.len());
    for scc in sccs {
        let mut members: Vec<usize> = scc.into_iter().map(|nx| graph[nx]).collect();
        members.sort_by(|&a, &b| {
            nodes[a]
                .path
                .cmp(&nodes[b].path)
                .then_with(|| nodes[a].start_line.cmp(&nodes[b].start_line))
                .then_with(|| nodes[a].symbol.cmp(&nodes[b].symbol))
        });
        ordered.extend(members);
    }
    ordered
}

pub async fn execute_diff_tour(
    store: &Arc<dyn VectorStore>,
    request: &DiffTourRequest,
    trusted_principal: Option<&str>,
) -> anyhow::Result<DiffTourResponse> {
    let namespace = crate::server::resolve_namespace(store, &request.namespace).await;
    let max_blast = request
        .max_blast_radius_callers
        .unwrap_or(DEFAULT_MAX_BLAST_RADIUS_CALLERS)
        .clamp(1, 100);
    let principal = resolve_principal(
        trusted_principal.unwrap_or("admin"),
        request.principal.as_deref(),
    );

    let (mut spans, mut truncated) = if let Some(explicit) = &request.changed_spans {
        let mut c = explicit.clone();
        let is_trunc = c.len() > MAX_DIFF_HUNKS;
        if is_trunc {
            c.truncate(MAX_DIFF_HUNKS);
        }
        for s in &mut c {
            s.path = crate::parser::ingest::normalize_node_path(&s.path);
        }
        (c, is_trunc)
    } else {
        (Vec::new(), false)
    };

    if let Some(diff_text) = request
        .diff_text
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let (parsed, parsed_trunc) = parse_unified_diff_spans(diff_text);
        truncated = truncated || parsed_trunc;
        for p in parsed {
            if spans.len() >= MAX_DIFF_HUNKS {
                truncated = true;
                break;
            }
            spans.push(p);
        }
    }

    let all_rows = store.list(&namespace, None, None).await.unwrap_or_default();
    let visible_rows: Vec<SearchResult> = all_rows
        .into_iter()
        .filter(|r| !crate::registry::is_registry_row(&r.payload.metadata))
        .filter(|r| r.payload.memory_type != "action_receipt" && r.payload.memory_type != "task")
        .filter(|r| !is_superseded_or_archived(&r.payload.metadata))
        .filter(|r| can_read_memory(&principal, &r.payload.user_id, &r.payload.metadata))
        .collect();

    let rows_by_id: HashMap<String, SearchResult> = visible_rows
        .iter()
        .map(|r| (r.id.clone(), r.clone()))
        .collect();

    // Index structural rows by file path and build workspace call graph.
    let mut structural_by_path: HashMap<String, Vec<SearchResult>> = HashMap::new();
    let mut symbols_by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut symbol_callees: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut symbol_callers: HashMap<String, BTreeSet<String>> = HashMap::new();

    for row in &visible_rows {
        if is_structural_type(&row.payload.memory_type) {
            if let Some(p) = extract_span_path(row) {
                structural_by_path.entry(p).or_default().push(row.clone());
            }
            if let Some(sym) = extract_symbol_name(row) {
                symbols_by_name
                    .entry(sym.to_ascii_lowercase())
                    .or_default()
                    .push(row.id.clone());
            }
        }
    }

    for row in &visible_rows {
        if !is_structural_type(&row.payload.memory_type) {
            continue;
        }
        let mut callees = extract_dependency_ids(&row.payload.metadata);
        if let Some(raw_calls) = row.payload.metadata.get("calls").and_then(|v| v.as_array()) {
            for c in raw_calls.iter().filter_map(|v| v.as_str()) {
                let terminal = c
                    .rsplit(|ch: char| ch == ':' || ch == '.')
                    .next()
                    .unwrap_or(c)
                    .to_ascii_lowercase();
                if let Some(target_ids) = symbols_by_name.get(&terminal) {
                    for tid in target_ids {
                        if tid != &row.id {
                            callees.insert(tid.clone());
                        }
                    }
                }
            }
        }
        callees.retain(|tid| tid != &row.id && rows_by_id.contains_key(tid));
        for tid in &callees {
            symbol_callers
                .entry(tid.clone())
                .or_default()
                .insert(row.id.clone());
        }
        if !callees.is_empty() {
            symbol_callees
                .entry(row.id.clone())
                .or_default()
                .extend(callees);
        }
        for caller_id in extract_caller_ids(&row.payload.metadata) {
            if caller_id != row.id && rows_by_id.contains_key(&caller_id) {
                symbol_callers
                    .entry(row.id.clone())
                    .or_default()
                    .insert(caller_id.clone());
                symbol_callees
                    .entry(caller_id)
                    .or_default()
                    .insert(row.id.clone());
            }
        }
    }

    // Map diff spans onto ingested symbols (or uningested file hunks).
    let mut touched_files: BTreeSet<String> = BTreeSet::new();
    let mut uningested_files: BTreeSet<String> = BTreeSet::new();
    let mut staged_nodes_map: BTreeMap<String, StagedTourNode> = BTreeMap::new();

    for span in &spans {
        if span.path.is_empty() {
            continue;
        }
        touched_files.insert(span.path.clone());
        let Some(file_rows) = structural_by_path.get(&span.path) else {
            uningested_files.insert(span.path.clone());
            let lines_str = format_span_lines(span);
            let start_line = span
                .new_lines
                .map(|(s, _)| s)
                .or_else(|| span.old_lines.map(|(s, _)| s))
                .unwrap_or(1);
            let key = format!("file:{}#{}", span.path, lines_str);
            staged_nodes_map.entry(key).or_insert(StagedTourNode {
                row_id: None,
                path: span.path.clone(),
                lines: lines_str,
                start_line,
                symbol: None,
                symbol_kind: None,
                memory_type: None,
                content: None,
                change_kind: span.change_kind.clone(),
            });
            continue;
        };

        let mut matched_symbol_row = false;
        for row in file_rows {
            let sym = extract_symbol_name(row);
            if sym.is_none() && row.payload.memory_type == "file" {
                continue;
            }
            let sym_range = parse_line_range_str(&row.payload.location_lines);
            if span_intersects_symbol(span, sym_range) {
                matched_symbol_row = true;
                let lines_str = if !row.payload.location_lines.trim().is_empty() {
                    row.payload.location_lines.trim().to_string()
                } else {
                    format_span_lines(span)
                };
                let start_line = sym_range
                    .map(|(s, _)| s)
                    .or_else(|| span.new_lines.map(|(s, _)| s))
                    .unwrap_or(1);
                let key = format!("row:{}", row.id);
                staged_nodes_map.entry(key).or_insert(StagedTourNode {
                    row_id: Some(row.id.clone()),
                    path: span.path.clone(),
                    lines: lines_str,
                    start_line,
                    symbol: sym,
                    symbol_kind: extract_symbol_kind(row),
                    memory_type: Some(row.payload.memory_type.clone()),
                    content: Some(row.payload.content.clone()),
                    change_kind: span.change_kind.clone(),
                });
            }
        }

        if !matched_symbol_row {
            let lines_str = format_span_lines(span);
            let start_line = span
                .new_lines
                .map(|(s, _)| s)
                .or_else(|| span.old_lines.map(|(s, _)| s))
                .unwrap_or(1);
            let key = format!("file:{}#{}", span.path, lines_str);
            staged_nodes_map.entry(key).or_insert(StagedTourNode {
                row_id: None,
                path: span.path.clone(),
                lines: lines_str,
                start_line,
                symbol: None,
                symbol_kind: None,
                memory_type: None,
                content: None,
                change_kind: span.change_kind.clone(),
            });
        }
    }

    let staged_nodes: Vec<StagedTourNode> = staged_nodes_map.into_values().collect();
    let total_symbols_touched = staged_nodes.iter().filter(|n| n.symbol.is_some()).count();

    let row_id_to_staged_idx: HashMap<String, usize> = staged_nodes
        .iter()
        .enumerate()
        .filter_map(|(idx, n)| n.row_id.as_ref().map(|id| (id.clone(), idx)))
        .collect();

    // Induced subgraph edges across staged nodes.
    let mut callees_in_diff_idx: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let mut callers_in_diff_idx: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let mut unchanged_caller_ids: HashMap<usize, BTreeSet<String>> = HashMap::new();

    for (idx, node) in staged_nodes.iter().enumerate() {
        let Some(rid) = &node.row_id else {
            continue;
        };
        if let Some(callees) = symbol_callees.get(rid) {
            for callee_id in callees {
                if let Some(&target_idx) = row_id_to_staged_idx.get(callee_id) {
                    if target_idx != idx {
                        callees_in_diff_idx
                            .entry(idx)
                            .or_default()
                            .insert(target_idx);
                        callers_in_diff_idx
                            .entry(target_idx)
                            .or_default()
                            .insert(idx);
                    }
                }
            }
        }
        if let Some(callers) = symbol_callers.get(rid) {
            for caller_id in callers {
                if let Some(&caller_idx) = row_id_to_staged_idx.get(caller_id) {
                    if caller_idx != idx {
                        callers_in_diff_idx
                            .entry(idx)
                            .or_default()
                            .insert(caller_idx);
                        callees_in_diff_idx
                            .entry(caller_idx)
                            .or_default()
                            .insert(idx);
                    }
                } else {
                    unchanged_caller_ids
                        .entry(idx)
                        .or_default()
                        .insert(caller_id.clone());
                }
            }
        }
    }

    // Hydrate GOVERNS rules and failed attempts across all touched files.
    let mut rule_table: BTreeMap<String, LocateRuleSummary> = BTreeMap::new();
    let mut failed_attempts_map: BTreeMap<String, LocateFailedAttempt> = BTreeMap::new();
    let mut rules_by_path: HashMap<String, BTreeSet<String>> = HashMap::new();

    if let Some(ls) = crate::traits::VectorStore::as_any(store.as_ref())
        .downcast_ref::<crate::store::LadybugStore>()
    {
        for path in &touched_files {
            if let Ok(hits) = ls
                .memories_for_file(&namespace, None, path, None, true, &[], &[], 25)
                .await
            {
                for hit in hits {
                    if let Some(row) = rows_by_id.get(&hit.id) {
                        if extract_failed_attempt(row).is_some() {
                            continue;
                        }
                        let entry = rule_table.entry(row.id.clone()).or_insert_with(|| {
                            let gov_paths: BTreeSet<String> = extract_governed_targets(row)
                                .into_iter()
                                .map(|(p, _, _)| p)
                                .collect();
                            LocateRuleSummary {
                                id: row.id.clone(),
                                memory_type: row.payload.memory_type.clone(),
                                summary: truncate_chars(
                                    &row.payload.content,
                                    RULE_SUMMARY_MAX_CHARS,
                                ),
                                governs_paths: gov_paths.into_iter().collect(),
                            }
                        });
                        if !entry.governs_paths.contains(path) {
                            entry.governs_paths.push(path.clone());
                            entry.governs_paths.sort();
                        }
                        rules_by_path
                            .entry(path.clone())
                            .or_default()
                            .insert(row.id.clone());
                    }
                }
            }
        }
    }

    for row in &visible_rows {
        let targets = extract_governed_targets(row);
        if let Some(mut attempt) = extract_failed_attempt(row) {
            let matches_touched = targets.iter().any(|(tp, _, _)| {
                touched_files
                    .iter()
                    .any(|tf| path_matches_governance_target(tf, tp))
            });
            if matches_touched {
                for tf in &touched_files {
                    if targets
                        .iter()
                        .any(|(tp, _, _)| path_matches_governance_target(tf, tp))
                        && !attempt.paths.contains(tf)
                    {
                        attempt.paths.push(tf.clone());
                    }
                }
                attempt.paths.sort();
                failed_attempts_map.insert(attempt.id.clone(), attempt);
            }
            continue;
        }
        if !is_normative_type(&row.payload.memory_type) {
            continue;
        }
        for tf in &touched_files {
            if targets
                .iter()
                .any(|(tp, _, _)| path_matches_governance_target(tf, tp))
            {
                let entry = rule_table.entry(row.id.clone()).or_insert_with(|| {
                    let gov_paths: BTreeSet<String> =
                        targets.iter().map(|(p, _, _)| p.clone()).collect();
                    LocateRuleSummary {
                        id: row.id.clone(),
                        memory_type: row.payload.memory_type.clone(),
                        summary: truncate_chars(&row.payload.content, RULE_SUMMARY_MAX_CHARS),
                        governs_paths: gov_paths.into_iter().collect(),
                    }
                });
                if !entry.governs_paths.contains(tf) {
                    entry.governs_paths.push(tf.clone());
                    entry.governs_paths.sort();
                }
                rules_by_path
                    .entry(tf.clone())
                    .or_default()
                    .insert(row.id.clone());
            }
        }
    }

    // Classify every staged node into one of the 4 stops via `classify_tour_stop`.
    let mut stop_buckets: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
    let mut node_categories: Vec<TourStopCategory> = Vec::with_capacity(staged_nodes.len());

    for (idx, node) in staged_nodes.iter().enumerate() {
        let is_test_or_doc = is_test_or_doc_item(
            &node.path,
            node.memory_type.as_deref(),
            node.symbol.as_deref(),
            node.content.as_deref(),
        );
        let is_foundation =
            is_foundation_kind_or_path_item(&node.path, node.symbol_kind.as_deref());
        let is_entrypoint = is_entrypoint_path_item(&node.path);
        let in_deg = callers_in_diff_idx
            .get(&idx)
            .map(|s| s.len() as u32)
            .unwrap_or(0);
        let out_deg = callees_in_diff_idx
            .get(&idx)
            .map(|s| s.len() as u32)
            .unwrap_or(0);

        let cat = classify_tour_stop(
            is_test_or_doc,
            is_foundation,
            is_entrypoint,
            in_deg,
            out_deg,
        );
        node_categories.push(cat);
        stop_buckets.entry(cat.stop_number()).or_default().push(idx);
    }

    // Identify untested_modified_symbols in Stops 1..=3.
    let mut untested_modified_symbols: BTreeSet<String> = BTreeSet::new();
    for (idx, node) in staged_nodes.iter().enumerate() {
        if node_categories[idx] == TourStopCategory::TestsAndDocs {
            continue;
        }
        let Some(sym) = node.symbol.as_deref() else {
            continue;
        };
        let has_modified_test_caller = callers_in_diff_idx
            .get(&idx)
            .map(|callers| {
                callers
                    .iter()
                    .any(|&c_idx| node_categories[c_idx] == TourStopCategory::TestsAndDocs)
            })
            .unwrap_or(false);
        let has_unchanged_test_caller = unchanged_caller_ids
            .get(&idx)
            .map(|callers| {
                callers.iter().any(|cid| {
                    rows_by_id
                        .get(cid)
                        .map(|r| {
                            let p = extract_span_path(r).unwrap_or_default();
                            let s = extract_symbol_name(r);
                            is_test_or_doc_item(
                                &p,
                                Some(&r.payload.memory_type),
                                s.as_deref(),
                                Some(&r.payload.content),
                            )
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        if !has_modified_test_caller && !has_unchanged_test_caller {
            untested_modified_symbols.insert(format!("{}::{sym}", node.path));
        }
    }

    // Construct ordered TourStop list (1..=4).
    let stop_specs = [
        (1u8, "foundations", "Schema, Traits & Foundations"),
        (2u8, "core_logic", "Core Implementation"),
        (
            3u8,
            "surface_and_callers",
            "Entrypoints, Surface & Blast Radius",
        ),
        (4u8, "tests_and_docs", "Tests & Documentation"),
    ];

    let mut stops: Vec<TourStop> = Vec::new();
    for (stop_num, category_slug, title) in stop_specs {
        let Some(bucket) = stop_buckets.remove(&stop_num) else {
            continue;
        };
        if bucket.is_empty() {
            continue;
        }
        let ordered_indices = order_nodes_callee_first(bucket, &staged_nodes, &callees_in_diff_idx);
        let mut items: Vec<TourItem> = Vec::with_capacity(ordered_indices.len());
        for idx in ordered_indices {
            let node = &staged_nodes[idx];
            let modified_callees_in_diff: Vec<String> = callees_in_diff_idx
                .get(&idx)
                .map(|set| {
                    set.iter()
                        .map(|&c_idx| {
                            format_symbol_label(
                                &staged_nodes[c_idx].path,
                                staged_nodes[c_idx].symbol.as_deref(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let modified_callers_in_diff: Vec<String> = callers_in_diff_idx
                .get(&idx)
                .map(|set| {
                    set.iter()
                        .map(|&c_idx| {
                            format_symbol_label(
                                &staged_nodes[c_idx].path,
                                staged_nodes[c_idx].symbol.as_deref(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let unchanged_callers: Vec<String> = unchanged_caller_ids
                .get(&idx)
                .map(|set| {
                    let mut labels: Vec<String> = set
                        .iter()
                        .filter_map(|cid| {
                            let r = rows_by_id.get(cid)?;
                            let p = extract_span_path(r)?;
                            let s = extract_symbol_name(r);
                            Some(format_symbol_label(&p, s.as_deref()))
                        })
                        .collect();
                    labels.sort();
                    labels.dedup();
                    labels.truncate(max_blast);
                    labels
                })
                .unwrap_or_default();

            let governing_rule_ids: Vec<String> = rules_by_path
                .get(&node.path)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();

            let rationale = match stop_num {
                1 => format!(
                    "Foundational definition or dependency sink in {} (depended on by {} modified symbol(s), {} unchanged caller(s), {} governing rule(s))",
                    node.path,
                    modified_callers_in_diff.len(),
                    unchanged_callers.len(),
                    governing_rule_ids.len()
                ),
                2 => format!(
                    "Core implementation in {} (calls {} modified callee(s), called by {} modified caller(s) and {} unchanged caller(s))",
                    node.path,
                    modified_callees_in_diff.len(),
                    modified_callers_in_diff.len(),
                    unchanged_callers.len()
                ),
                3 => format!(
                    "Surface/entrypoint or top-level caller in {} (calls {} modified callee(s), {} unchanged caller(s) in blast radius)",
                    node.path,
                    modified_callees_in_diff.len(),
                    unchanged_callers.len()
                ),
                _ => format!(
                    "Verification test or documentation in {} (exercises {} modified symbol(s))",
                    node.path,
                    modified_callees_in_diff.len()
                ),
            };

            items.push(TourItem {
                path: node.path.clone(),
                lines: node.lines.clone(),
                symbol: node.symbol.clone(),
                symbol_kind: node.symbol_kind.clone(),
                change_kind: node.change_kind.clone(),
                rationale,
                modified_callees_in_diff,
                modified_callers_in_diff,
                unchanged_callers,
                governing_rule_ids,
            });
        }

        stops.push(TourStop {
            stop_number: stop_num,
            category: category_slug.to_string(),
            title: title.to_string(),
            items,
        });
    }

    // Record touched paths in Graph-First ledger.
    for path in &touched_files {
        crate::task::record_graph_query(store, &namespace, "neurostrata_diff_tour", path).await;
    }

    Ok(DiffTourResponse {
        namespace,
        total_files_changed: touched_files.len(),
        total_symbols_touched,
        stops,
        governing_rules: rule_table.into_values().collect(),
        failed_attempts: failed_attempts_map.into_values().collect(),
        untested_modified_symbols: untested_modified_symbols.into_iter().collect(),
        uningested_files: uningested_files.into_iter().collect(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LadybugStore;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    #[tokio::test]
    async fn diff_tour_orders_four_stops_reports_unchanged_callers_and_governs_rules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn VectorStore> =
            Arc::new(LadybugStore::new(dir.path().join("db"), 384).expect("store init"));
        let ns = "DiffTourSpecNs";
        store.init(ns).await.unwrap();

        // 1. Foundation symbol in src/traits.rs (Stop 1)
        store
            .upsert(
                ns,
                "sym-trait-vector-store",
                vec![0.1; 384],
                MemoryPayload {
                    content: "traits VectorStore in src/traits.rs\nlines 10-40\n\npub trait VectorStore {}".to_string(),
                    location: "src/traits.rs".to_string(),
                    location_lines: "10-40".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "VectorStore",
                        "symbol_kind": "traits",
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 2. Core logic symbol in src/store/ladybug.rs (Stop 2): calls VectorStore, called by handle_locate and unchanged caller
        store
            .upsert(
                ns,
                "sym-callee-core",
                vec![0.1; 384],
                MemoryPayload {
                    content: "functions execute_store_query in src/store/ladybug.rs\nlines 100-150\n\npub fn execute_store_query() {}".to_string(),
                    location: "src/store/ladybug.rs".to_string(),
                    location_lines: "100-150".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "execute_store_query",
                        "symbol_kind": "functions",
                        "depends_on": ["sym-trait-vector-store"],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 3. Unchanged caller in src/a.rs (NOT in the diff, calls execute_store_query)
        store
            .upsert(
                ns,
                "sym-unchanged-caller",
                vec![0.1; 384],
                MemoryPayload {
                    content: "functions caller in src/a.rs\nlines 10-30\n\npub fn caller() { execute_store_query(); }".to_string(),
                    location: "src/a.rs".to_string(),
                    location_lines: "10-30".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "caller",
                        "symbol_kind": "functions",
                        "calls": ["execute_store_query"],
                        "depends_on": ["sym-callee-core"],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 4. Entrypoint symbol in src/server.rs (Stop 3): calls execute_store_query
        store
            .upsert(
                ns,
                "sym-server-handler",
                vec![0.1; 384],
                MemoryPayload {
                    content: "functions handle_request in src/server.rs\nlines 200-260\n\npub async fn handle_request() { execute_store_query(); }".to_string(),
                    location: "src/server.rs".to_string(),
                    location_lines: "200-260".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "handle_request",
                        "symbol_kind": "functions",
                        "depends_on": ["sym-callee-core"],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 5. Test symbol in tests/integration_test.rs (Stop 4): calls handle_request
        store
            .upsert(
                ns,
                "sym-test-handler",
                vec![0.1; 384],
                MemoryPayload {
                    content: "functions test_handle_request in tests/integration_test.rs\nlines 1-30\n\n#[test]\nfn test_handle_request() { handle_request(); }".to_string(),
                    location: "tests/integration_test.rs".to_string(),
                    location_lines: "1-30".to_string(),
                    memory_type: "code_ast".to_string(),
                    metadata: json!({
                        "domain": "code_ast",
                        "symbol": "test_handle_request",
                        "symbol_kind": "functions",
                        "depends_on": ["sym-server-handler"],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 6. Governing rule on src/store/
        store
            .upsert(
                ns,
                "rule-store-safety",
                vec![0.1; 384],
                MemoryPayload {
                    content: "All store queries must preserve bitemporal validity.".to_string(),
                    location: "src/store".to_string(),
                    location_lines: String::new(),
                    memory_type: "rule".to_string(),
                    metadata: json!({
                        "governs": ["src/store"],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        // 7. Failed attempt on src/store/ladybug.rs
        store
            .upsert(
                ns,
                "attempt-store-lock",
                vec![0.1; 384],
                MemoryPayload {
                    content: "Holding write lock across await deadlocked.".to_string(),
                    location: "src/store/ladybug.rs".to_string(),
                    location_lines: String::new(),
                    memory_type: "fact".to_string(),
                    metadata: json!({
                        "attempt": {
                            "approach": "Hold MutexGuard across async boundary",
                            "outcome": "failed",
                            "failure_mode": "Deadlock under concurrent readers"
                        },
                        "locations": [{ "path": "src/store/ladybug.rs" }],
                    }),
                    user_id: "test".to_string(),
                    agent_name: None,
                },
            )
            .await
            .unwrap();

        let unified_diff = "\
--- a/tests/integration_test.rs
+++ b/tests/integration_test.rs
@@ -5,3 +5,4 @@
 fn test_handle_request() {
+    handle_request();
 }
--- a/src/server.rs
+++ b/src/server.rs
@@ -210,3 +210,4 @@
 pub async fn handle_request() {
+    execute_store_query();
 }
--- a/src/store/ladybug.rs
+++ b/src/store/ladybug.rs
@@ -110,3 +110,4 @@
 pub fn execute_store_query() {
+    // updated query
 }
--- a/src/traits.rs
+++ b/src/traits.rs
@@ -15,3 +15,4 @@
 pub trait VectorStore {
+    fn new_method(&self);
 }
";

        let req = DiffTourRequest {
            namespace: ns.to_string(),
            project_root: None,
            diff_text: Some(unified_diff.to_string()),
            changed_spans: None,
            max_blast_radius_callers: Some(15),
            principal: Some("admin".to_string()),
        };

        let resp = execute_diff_tour(&store, &req, Some("admin"))
            .await
            .expect("execute_diff_tour");

        assert_eq!(resp.total_files_changed, 4);
        assert_eq!(resp.total_symbols_touched, 4);
        assert_eq!(resp.stops.len(), 4);
        assert_eq!(resp.stops[0].stop_number, 1);
        assert_eq!(resp.stops[0].category, "foundations");
        assert_eq!(resp.stops[0].items[0].path, "src/traits.rs");

        assert_eq!(resp.stops[1].stop_number, 2);
        assert_eq!(resp.stops[1].category, "core_logic");
        let core_item = &resp.stops[1].items[0];
        assert_eq!(core_item.path, "src/store/ladybug.rs");
        assert_eq!(core_item.symbol.as_deref(), Some("execute_store_query"));
        assert!(core_item
            .unchanged_callers
            .contains(&"src/a.rs::caller".to_string()));
        assert!(core_item
            .governing_rule_ids
            .contains(&"rule-store-safety".to_string()));

        assert_eq!(resp.stops[2].stop_number, 3);
        assert_eq!(resp.stops[2].category, "surface_and_callers");
        assert_eq!(resp.stops[2].items[0].path, "src/server.rs");

        assert_eq!(resp.stops[3].stop_number, 4);
        assert_eq!(resp.stops[3].category, "tests_and_docs");
        assert_eq!(resp.stops[3].items[0].path, "tests/integration_test.rs");

        assert!(resp
            .governing_rules
            .iter()
            .any(|r| r.id == "rule-store-safety"));
        assert_eq!(resp.failed_attempts.len(), 1);
        assert_eq!(resp.failed_attempts[0].id, "attempt-store-lock");

        // handle_request is tested by test_handle_request in Stop 4, whereas execute_store_query
        // has no direct test caller in Stop 4.
        assert!(resp
            .untested_modified_symbols
            .contains(&"src/store/ladybug.rs::execute_store_query".to_string()));
        assert!(!resp
            .untested_modified_symbols
            .contains(&"src/server.rs::handle_request".to_string()));
    }
}
