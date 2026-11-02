//! AC4 doctor classification: Materialized / Declared / Orphaned.
//! - Materialized: the declared target names a node the store holds exactly.
//! - Declared: the target does not name a node exactly, but `KnownIds::resolve`
//!   maps it to one (legacy suffix / qualified-id normalization). A resolvable
//!   declared target is *fixable* and therefore blocking for AC4.
//! - Orphaned: resolvable by nothing — warning only, non-blocking.

use crate::store::ladybug::{edge_specs, KnownIds};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetClass {
    Materialized,
    Declared(String),
    Orphaned,
}

pub fn classify_target(known: &KnownIds<'_>, target: &str) -> TargetClass {
    if known.contains(target) {
        TargetClass::Materialized
    } else if let Some(resolved) = known.resolve(target) {
        TargetClass::Declared(resolved)
    } else {
        TargetClass::Orphaned
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetClassificationSummary {
    pub materialized: usize,
    pub declared: usize,
    pub orphaned: usize,
    pub declared_targets: Vec<String>,
    pub orphaned_targets: Vec<String>,
}

pub fn classify_namespace_targets(memories: &[crate::traits::SearchResult]) -> TargetClassificationSummary {
    let known = KnownIds::new(memories.iter().map(|m| m.id.as_str()));
    let mut summary = TargetClassificationSummary::default();

    for m in memories {
        for edge in edge_specs(&m.payload.metadata) {
            match classify_target(&known, &edge.target_id) {
                TargetClass::Materialized => summary.materialized += 1,
                TargetClass::Declared(resolved_id) => {
                    summary.declared += 1;
                    if !resolved_id.is_empty() && resolved_id != edge.target_id {
                        tracing::debug!(
                            "edge '{}' -> '{}' resolved to declared target '{}'",
                            m.id,
                            edge.target_id,
                            resolved_id
                        );
                    }
                    // The summary reports the id a normalization pass would
                    // write — the resolved one when resolution produced it,
                    // the raw target otherwise — so downstream consumers
                    // (the AC4 precheck) name the canonical fixable id.
                    if !resolved_id.is_empty() {
                        summary.declared_targets.push(resolved_id);
                    } else {
                        summary.declared_targets.push(edge.target_id);
                    }
                }
                TargetClass::Orphaned => {
                    summary.orphaned += 1;
                    summary.orphaned_targets.push(edge.target_id);
                }
            }
        }
    }

    summary
}

/// The AC4 gate over one classification. A resolvable declared target can still
/// be fixed in place, so it blocks; an orphan cannot be fixed by normalization,
/// so it warns and passes.
pub fn classify_unmatched(
    materialized: usize,
    declared: usize,
    orphaned: usize,
) -> Result<Option<String>, String> {
    if declared > 0 {
        return Err(format!(
            "AC4 blocked: {} resolvable declared targets remain unmaterialized (materialized={}, orphaned={})",
            declared, materialized, orphaned
        ));
    }
    if orphaned > 0 {
        return Ok(Some(format!(
            "AC4 warning: {} orphaned targets (non-blocking; materialized={})",
            orphaned, materialized
        )));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_zero_targets_passes() {
        assert_eq!(classify_unmatched(5, 0, 0), Ok(None));
    }

    #[test]
    fn unmatched_orphans_warn_non_blocking() {
        let expected = Ok(Some(
            "AC4 warning: 2 orphaned targets (non-blocking; materialized=5)".to_string(),
        ));
        let got = classify_unmatched(5, 0, 2);
        assert_eq!(got, expected);
    }

    #[test]
    fn unmatched_resolvable_declared_blocks() {
        let got = classify_unmatched(5, 1, 0);
        assert!(got.is_err());
        let msg = got.unwrap_err();
        assert!(msg.starts_with("AC4 blocked:"), "got: {}", msg);
        assert!(msg.contains("resolvable declared targets"), "got: {}", msg);
    }

    #[test]
    fn classify_target_materialized_when_exact() {
        let known = KnownIds::new(["NeuroStrata::src/lib.rs"]);
        assert_eq!(
            classify_target(&known, "NeuroStrata::src/lib.rs"),
            TargetClass::Materialized
        );
    }

    #[test]
    fn classify_target_declared_when_resolvable() {
        // Bare logical path resolves to the qualified id the store holds.
        let known = KnownIds::new(["NeuroStrata::src/lib.rs"]);
        match classify_target(&known, "src/lib.rs") {
            TargetClass::Declared(resolved) => assert_eq!(resolved, "NeuroStrata::src/lib.rs"),
            other => panic!("expected Declared, got {:?}", other),
        }
    }

    #[test]
    fn classify_target_orphaned_when_unresolvable() {
        let known = KnownIds::new(["NeuroStrata::src/lib.rs"]);
        assert_eq!(classify_target(&known, "docs/absent.md"), TargetClass::Orphaned);
    }

    #[test]
    fn classify_namespace_targets_counts_classes() {
        let memories = [
            crate::traits::SearchResult {
                id: "NeuroStrata::src/lib.rs".to_string(),
                score: 0.0,
                payload: crate::traits::MemoryPayload {
                    content: String::new(),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "file".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
                evidence: None,
            },
            crate::traits::SearchResult {
                id: "NeuroStrata::src/main.rs".to_string(),
                score: 0.0,
                payload: crate::traits::MemoryPayload {
                    content: String::new(),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "file".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "related_to": ["src/lib.rs", "docs/absent.md", "NeuroStrata::src/lib.rs"]
                    }),
                },
                evidence: None,
            },
        ];
        let s = classify_namespace_targets(&memories);
        assert_eq!(s.materialized, 1);
        assert_eq!(s.declared, 1);
        assert_eq!(
            s.declared_targets,
            vec!["NeuroStrata::src/lib.rs".to_string()]
        );
        assert_eq!(s.orphaned, 1);
        assert_eq!(s.orphaned_targets, vec!["docs/absent.md".to_string()]);
    }
}