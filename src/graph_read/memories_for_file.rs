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
use serde_json::Value;

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
/// metadata declaration that never materialised an edge. `priority()` spells
/// that order out so the sort key does not depend on `Ord`'s declaration
/// order alone.
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
            GraphMatchKind::MetadataFallback => 4,
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
}

/// The memory-type segment of the ranking key (spec §4.1):
/// `rule`(ENFORCED) > `rule`(other) > `fact`(decision/topology/attempt) >
/// `procedure` > general `fact` > anything else. Lower is better.
pub fn memory_type_priority(memory_type: &str, metadata: &Value) -> u8 {
    match memory_type {
        "rule" => {
            if metadata
                .get("enforcement")
                .and_then(|v| v.as_str())
                == Some("ENFORCED")
            {
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

/// Sort hits by the spec's ranking: match kind, then memory-type priority,
/// then freshness, then `access_count` descending, then id for determinism.
pub fn rank_hits(hits: &mut [MemoryHit]) {
    hits.sort_by(|a, b| {
        a.match_kind
            .priority()
            .cmp(&b.match_kind.priority())
            .then_with(|| {
                memory_type_priority(&a.memory_type, &a.metadata)
                    .cmp(&memory_type_priority(&b.memory_type, &b.metadata))
            })
            .then_with(|| freshness_priority(&a.metadata).cmp(&freshness_priority(&b.metadata)))
            .then_with(|| b.access_count.cmp(&a.access_count))
            .then_with(|| a.id.cmp(&b.id))
    });
}
