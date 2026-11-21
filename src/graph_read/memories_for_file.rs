//! Graph-read primitives for `memories_for_file`, shared by the `GraphRead`
//! capability and its integration test.
//!
//! This module is deliberately free of `crate::` dependencies (serde +
//! serde_json + std only) so `tests/graph_read/memories_for_file_test.rs` can
//! compile it from its real path and pin the ranking contract without a
//! library target -- the same pattern `src/parser/generation.rs` uses (see
//! `tests/ingest/staged_publication_test.rs`).
//!
//! Path normalization is NOT redefined here: it lives once in
//! `crate::parser::ingest::normalize_node_path`, which both ingestion and the
//! `GraphRead` implementation call, so the two cannot drift.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The default `memory_types` a `memories_for_file` query asks for.
pub const DEFAULT_MEMORY_TYPES: [&str; 3] = ["rule", "fact", "procedure"];

/// The default result limit.
pub const DEFAULT_LIMIT: usize = 5;

/// The ceiling on a caller's `limit` (spec §4.1: default 5, max 20).
pub const MAX_LIMIT: usize = 20;

/// How a memory matched the queried file/symbol.
///
/// The variant order IS the primary ranking order (best first): an exact
/// symbol outranks the file it lives in, which outranks a symbol reached
/// through the file, which outranks an ancestor directory, which outranks a
/// `RELATES_TO` edge, which outranks a metadata declaration that never
/// materialised an edge. `priority()` spells that order out so the sort key
/// does not depend on `Ord`'s declaration order alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphMatchKind {
    /// The memory governs the exact symbol named in the query.
    ExactSymbol,
    /// The memory governs the exact file named in the query.
    ExactFile,
    /// The memory governs a symbol contained by the queried file (2-hop).
    SymbolTwoHop,
    /// The memory governs an ancestor directory of the queried file.
    AncestorDirectory,
    /// The memory is linked to the queried target by a `RELATES_TO` edge.
    Related,
    /// The memory declares the path in `metadata.governs` but no edge formed.
    MetadataFallback,
}

impl GraphMatchKind {
    /// Lower is better, matching the variant order above.
    pub fn priority(self) -> u8 {
        match self {
            GraphMatchKind::ExactSymbol => 0,
            GraphMatchKind::ExactFile => 1,
            GraphMatchKind::SymbolTwoHop => 2,
            GraphMatchKind::AncestorDirectory => 3,
            GraphMatchKind::Related => 4,
            GraphMatchKind::MetadataFallback => 5,
        }
    }
}

/// One memory surfaced by `memories_for_file`, with enough context to rank it
/// and to tell the caller *why* it matched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub id: String,
    pub memory_type: String,
    pub content: String,
    pub match_kind: GraphMatchKind,
    pub access_count: i64,
    /// One human-readable sentence naming the target that produced the match.
    pub explanation: String,
    /// The row's metadata, carried so ranking (type/freshness) and callers can
    /// inspect the memory without a second fetch.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    /// Up to 2 cross-domain conceptual bridges where this hit is the seed.
    /// Populated from 1-hop neighbors whose domain is disjoint from `source_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conceptual_bridges: Vec<ConceptualBridge>,
    /// How many files the memory is linked to (see `fan_out_bucket`). Used
    /// for ranking only, so it is not part of the answer.
    #[serde(skip)]
    pub linked_files: usize,
}

/// A cross-domain conceptual bridge connecting a seed memory to a normative target
/// via a 2-hop path (seed -> via -> target). Used to surface hidden architectural
/// relationships that are not directly visible through the standard graph edges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptualBridge {
    /// The seed memory (directly matched to the queried file/symbol).
    pub source_id: String,
    /// The intermediate node on the 2-hop path (seed -> via -> target).
    pub via_id: String,
    /// The normative target memory in a disjoint domain from `source_id`.
    pub target_id: String,
    /// Normalized domain slug of the target (e.g., "rule", "fact", "procedure").
    pub target_domain: String,
    /// Human-readable one-sentence summary of the target (max 160 chars).
    pub target_summary: String,
    /// Human-readable path description, e.g. "src/server.rs -> via-id -> rule-id".
    pub path: String,
    /// Memory type of the target (rule | fact | decision | procedure).
    pub target_type: String,
}

/// The memory-type segment of the ranking key (spec §4.1):
/// `rule`(ENFORCED) > `rule`(other) > `fact`(decision/topology/attempt) >
/// `procedure` > general `fact` > anything else. Lower is better.
pub fn memory_type_priority(memory_type: &str, metadata: &Value) -> u8 {
    match memory_type {
        "rule" => {
            if metadata.get("enforcement").and_then(|v| v.as_str()) == Some("ENFORCED") {
                0
            } else {
                1
            }
        }
        "fact" => {
            let shaped = ["decision", "topology", "attempt"]
                .iter()
                .any(|k| metadata.get(k).map(|v| !v.is_null()).unwrap_or(false));
            if shaped {
                2
            } else {
                4
            }
        }
        "procedure" => 3,
        _ => 5,
    }
}

/// The freshness segment of the ranking key: verified ranks first, then
/// unknown/absent, then a verification that found something wrong. Lower is
/// better.
pub fn freshness_priority(metadata: &Value) -> u8 {
    match metadata
        .get("verification")
        .and_then(|v| v.get("status"))
        .and_then(|s| s.as_str())
    {
        Some("verified") => 0,
        Some("contradicted") | Some("drifted") => 2,
        _ => 1,
    }
}

/// The fan-out segment of the ranking key: a memory linked to 1-2 files says
/// more about each of them than one linked to 3-9, or to 10 or more. Lower
/// is better. It is a *tiebreaker* inside one enforcement class, never a
/// reason to rank a broad ENFORCED rule below a narrow advisory one (#208).
pub fn fan_out_bucket(linked_files: usize) -> u8 {
    match linked_files {
        0..=2 => 0,
        3..=9 => 1,
        _ => 2,
    }
}

/// Whether the memory is linked to the queried file itself (or to a symbol
/// in it), rather than to a directory above it or through `RELATES_TO`.
fn is_direct(kind: GraphMatchKind) -> bool {
    matches!(
        kind,
        GraphMatchKind::ExactSymbol | GraphMatchKind::ExactFile | GraphMatchKind::SymbolTwoHop
    )
}

fn valid_from(metadata: &Value) -> i64 {
    metadata
        .get("valid_from")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// Sort hits by the spec's ranking: match kind, then memory-type priority,
/// then fan-out bucket, then freshness, then (direct matches) newest
/// `valid_from` first or (weaker matches) `access_count` descending, then id
/// for determinism. Type outranks fan-out so a broad ENFORCED rule is never
/// pushed off the page by narrow advisory rules; fan-out only separates hits
/// that are equal on kind and type. Newest first for direct matches because an
/// older memory on the same file has had more time to collect reads, and it is
/// the newer one that describes the file as it is.
pub fn rank_hits(hits: &mut [MemoryHit]) {
    hits.sort_by(|a, b| {
        a.match_kind
            .priority()
            .cmp(&b.match_kind.priority())
            .then_with(|| {
                memory_type_priority(&a.memory_type, &a.metadata)
                    .cmp(&memory_type_priority(&b.memory_type, &b.metadata))
            })
            .then_with(|| fan_out_bucket(a.linked_files).cmp(&fan_out_bucket(b.linked_files)))
            .then_with(|| freshness_priority(&a.metadata).cmp(&freshness_priority(&b.metadata)))
            .then_with(|| {
                if is_direct(a.match_kind) {
                    valid_from(&b.metadata).cmp(&valid_from(&a.metadata))
                } else {
                    b.access_count.cmp(&a.access_count)
                }
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

// --- Conditional escalation (Task 4) ---------------------------------------
//
// These thresholds and expected-decision strings mirror `crate::task::routing`
// on purpose. This module stays crate-free (serde_json + std only) so the
// integration test can compile it from its real path; hence the duplication.

/// R4: an AST blast radius at or above this value needs a two-hop expansion.
/// `pub` so `tests/agent_adoption_test.rs` can pin it against `routing`.
pub const HIGH_BLAST_THRESHOLD: usize = 15;
/// R4: at least this many staged files (or a PR review scope) needs a diff tour.
pub const DIFF_STAGED_FILE_THRESHOLD: usize = 3;
/// §5: no response may recommend more than three follow-up actions.
pub const MAX_NEXT_ACTIONS: usize = 3;

pub const EXPECTED_AMBIGUOUS: &str = "Pick a localized symbol/path, then re-run the path lookup.";
pub const EXPECTED_HIGH_BLAST: &str =
    "Expand two hops; re-plan edits around unchanged callers before proceeding.";
pub const EXPECTED_SEED_EDGE: &str =
    "Use the result to decide the next step; abstain when it adds no evidence.";
pub const EXPECTED_DIFF: &str =
    "Review by dependency stop; audit unchanged callers and governing rules.";

/// FNV-1a over the emitted args plus relevant ids. Stable across runs so an
/// `action_id` can be correlated with the call it produced (mirrors routing).
fn stable_digest(args: &Value, ids: &[String]) -> String {
    let mut payload = args.to_string();
    for id in ids {
        payload.push('\u{1f}');
        payload.push_str(id);
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in payload.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Materialize one additive §5 `next_action`:
/// `{action_id, trigger_id, tool, args, reason, expected_decision}`.
fn next_action(
    trigger_id: &str,
    tool: &str,
    args: &Value,
    digest_ids: &[String],
    reason: &str,
    expected_decision: &str,
) -> Value {
    json!({
        "action_id": format!("{}:{}", trigger_id, stable_digest(args, digest_ids)),
        "trigger_id": trigger_id,
        "tool": tool,
        "args": args,
        "reason": reason,
        "expected_decision": expected_decision,
    })
}

/// Pure escalation decision for `memories_for_file`, shared with the HTTP
/// handler in `src/server.rs`. Returns `{status, next_actions,
/// suppressed_reason?}` with only additive fields.
///
/// - `empty` -> `status:"empty"` + `T_AMBIGUOUS` to `neurostrata_locate`.
///   An empty lookup is NOT "no governance" and is never described as such.
/// - `blast_radius_count >= 15` **and a usable seed id** -> `T_HIGH_BLAST` to
///   `neurostrata_related` with `max_hops:2`. An empty/whitespace seed is never
///   emitted as `args.id`; with no seed the trigger is skipped.
/// - a non-empty conceptual bridge -> `T_SEED_EDGE` to `neurostrata_object_view`.
/// - `staged_file_count >= 3` -> `T_DIFF` to `neurostrata_diff_tour` with
///   only the schema-valid `{namespace, diff_text}` args (`git_ref`, if any,
///   belongs in `reason` only -- `diff_tour` declares no such argument).
///
/// At most three actions are emitted. An ordinary single-file edit (no
/// triggers) emits an empty `next_actions` plus a `suppressed_reason`.
pub fn recommended_next_actions(
    blast_radius_count: usize,
    bridge_ids: &[String],
    staged_file_count: usize,
    empty: bool,
    namespace: &str,
    seed_id: Option<&str>,
) -> Value {
    let status = if empty { "empty" } else { "success" };
    let mut actions: Vec<Value> = Vec::new();

    if empty {
        // An empty lookup prescribes T_AMBIGUOUS only when there is a usable
        // seed to narrow. With no seed there is no schema-valid `query` arg,
        // so abstain (suppressed_reason is set when next_actions is empty).
        if let Some(seed) = seed_id.filter(|id| !id.trim().is_empty()) {
            let args = json!({ "namespace": namespace, "query": seed });
            actions.push(next_action(
                "T_AMBIGUOUS",
                "neurostrata_locate",
                &args,
                &[],
                "Empty lookup: narrow to a concrete symbol/path and re-run the path lookup.",
                EXPECTED_AMBIGUOUS,
            ));
        }
    } else {
        if blast_radius_count >= HIGH_BLAST_THRESHOLD {
            // `routing::next_action` likewise refuses an empty seed id: a
            // related call with args.id == "" is schema-invalid, so abstain.
            if let Some(id) = seed_id.filter(|id| !id.trim().is_empty()) {
                let args = json!({ "namespace": namespace, "id": id, "max_hops": 2 });
                actions.push(next_action(
                    "T_HIGH_BLAST",
                    "neurostrata_related",
                    &args,
                    &[id.to_string()],
                    &format!(
                        "Blast radius {blast_radius_count} >= 15: expand two hops before editing."
                    ),
                    EXPECTED_HIGH_BLAST,
                ));
            }
        }
        if let Some(bridge_id) = bridge_ids.iter().find(|id| !id.trim().is_empty()) {
            let args = json!({ "namespace": namespace, "id": bridge_id });
            actions.push(next_action(
                "T_SEED_EDGE",
                "neurostrata_object_view",
                &args,
                &[bridge_id.clone()],
                "Cross-domain conceptual bridge: inspect the target before editing.",
                EXPECTED_SEED_EDGE,
            ));
        }
    }

    if staged_file_count >= DIFF_STAGED_FILE_THRESHOLD {
        // `diff_tour` declares no `git_ref` arg; the caller fills `diff_text`.
        let args = json!({ "namespace": namespace, "diff_text": "" });
        actions.push(next_action(
            "T_DIFF",
            "neurostrata_diff_tour",
            &args,
            &[],
            "Scope has >=3 staged files; supply diff_text (e.g. from HEAD) and review by dependency stop.",
            EXPECTED_DIFF,
        ));
    }

    // §5's cap is structural: one high-blast + one seed-edge + one diff is the
    // maximum this decision can emit. A runtime truncate can only discard a
    // legitimate action, so assert the bound instead of silently cutting.
    debug_assert!(
        actions.len() <= MAX_NEXT_ACTIONS,
        "escalation must emit at most {MAX_NEXT_ACTIONS} actions"
    );
    let mut response = json!({ "status": status, "next_actions": actions });
    if response["next_actions"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        // State the *conditions* for escalation, not the observed values: a
        // high-radius lookup with no usable seed, or an empty lookup with no
        // seed to narrow, also lands here.
        let reason = if empty {
            "Empty lookup with no usable symbol/path seed: no schema-valid graph action to prescribe; narrow the query and re-run the path lookup."
        } else {
            "Single-file lookup already carries 1-hop related memories; no trigger fired (high-blast needs radius >= 15 and a usable seed, no conceptual bridge, staged scope < 3)."
        };
        response["suppressed_reason"] = Value::String(reason.to_string());
    }
    response
}

/// Additive fail-soft shape for a `memories_for_file` query failure.
///
/// The legacy fields (`memories`, `unsupported`, `path`, `error`) keep their
/// exact values; the §5 receipt fields are added so a failed lookup is never
/// misread as "no governing memory". `next_actions` is empty and a
/// `suppressed_reason` is always present, matching the abstention rule in
/// `recommended_next_actions` (empty actions must say why none fired).
pub fn query_failure_response(path: &str, error: &str) -> Value {
    json!({
        "memories": [],
        "unsupported": false,
        "path": path,
        "error": error,
        "status": "failed",
        "next_actions": [],
        "suppressed_reason": "Query failed; no escalation actions were emitted because the lookup did not complete. A failed lookup is not evidence of no governing memory.",
    })
}
