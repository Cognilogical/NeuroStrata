//! Semantic evaluator using LadyBugDB for vector search
//!
//! Validates actions against behavioral rules stored as memories in the 'guard' namespace.
//! Each rule is embedded and stored with memory_type="guard_rule".

use crate::guard::models::BehavioralRule;
use crate::traits::{Embedder, MemoryPayload, VectorStore};
use anyhow::{Context, Result};
use std::sync::Arc;

/// Composite Rec #4: the similarity floor a rule row must clear before the
/// semantic guard may act on it. Vector search returns nearest neighbours —
/// not matches — so a loosely related rule below this cutoff must never turn a
/// validate call into a false-positive rejection.
pub const MIN_CONSTRAINT_SIMILARITY: f32 = 0.25;

/// The memory-type families the guard treats as rules. `guard_rule` is what
/// `add_constraint` writes; `rule` is the vocabulary's generic rule type, so
/// rules seeded through the normal memory surface stay visible to the guard.
const RULE_MEMORY_TYPES: [&str; 2] = ["rule", "guard_rule"];

pub struct SemanticEvaluator {
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn Embedder>,
    namespace: String,
}

impl SemanticEvaluator {
    pub fn new(store: Arc<dyn VectorStore>, embedder: Arc<dyn Embedder>, namespace: &str) -> Self {
        Self {
            store,
            embedder,
            namespace: namespace.to_string(),
        }
    }

    /// Add a behavioral rule to the guard namespace
    pub async fn add_constraint(&self, rule: BehavioralRule) -> Result<()> {
        let text_to_embed = format!("{} {}", rule.trigger_pattern, rule.constraint_text);
        let vector = self.embedder.embed(&text_to_embed).await?;

        let payload = MemoryPayload {
            content: rule.constraint_text.clone(),
            user_id: "guard".to_string(),
            memory_type: "guard_rule".to_string(),
            agent_name: Some("neurostrata-guard".to_string()),
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "id": rule.id,
                "version": rule.version,
                "rule_class": rule.rule_class,
                "trigger_pattern": rule.trigger_pattern,
                "hit_count": rule.hit_count,
                "status": rule.status,
            }),
        };

        self.store
            .upsert(&self.namespace, &rule.id, vector, payload)
            .await
            .context("Failed to store guard rule")?;

        Ok(())
    }

    /// Find matching rules for a given payload using vector similarity
    pub async fn match_constraints(
        &self,
        payload: &str,
        limit: usize,
    ) -> Result<Vec<BehavioralRule>> {
        let vector = self.embedder.embed(payload).await?;

        // The store's row-class flags belong to the general search path's
        // contract. The guard asks for everything in its own namespace and
        // applies its own rule filter below (the `guard_rule` metadata
        // fields), so a change to the store's contract cannot silently change
        // which rules the guard is able to see.
        let search_results = self
            .store
            .search(&self.namespace, vector, limit, true, true)
            .await?;

        let rules = rules_from_rows(search_results);

        Ok(rules)
    }

    /// Evaluate whether a payload violates any constraints
    pub async fn evaluate(&self, payload: &str) -> Result<Vec<BehavioralRule>> {
        // Return top 3 matching rules
        self.match_constraints(payload, 3).await
    }

    /// Record one firing of a rule: bump its persisted `hit_count` by one.
    ///
    /// `hit_count` is the only record of how often a rule actually fired, and
    /// it was read as a live signal while nothing ever wrote it -- so every
    /// consumer of the field was reading a constant zero.
    ///
    /// The bump reads the row, changes this one field, and writes the row
    /// back with the vector it already had: the rule keeps its embedding and
    /// its identity, and only the counter moves.
    pub async fn increment_hit_count(&self, rule_id: &str) -> Result<()> {
        let Some((vector, mut payload)) = self.store.get(&self.namespace, rule_id).await? else {
            // The rule is gone. There is nothing left to count, and inventing
            // a row for a rule that no longer exists would resurrect it.
            return Ok(());
        };

        if !payload.metadata.is_object() {
            anyhow::bail!("rule {rule_id} carries metadata that is not an object");
        }

        let hits = payload
            .metadata
            .get("hit_count")
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
            .unwrap_or(0);
        payload.metadata["hit_count"] = serde_json::json!(hits + 1);

        self.store
            .upsert(&self.namespace, rule_id, vector, payload)
            .await
            .context("Failed to record the rule hit")?;

        Ok(())
    }
}

/// Turn raw search rows into guard rules, dropping every row that either
/// cannot prove it is a rule (rule types only) or sits below the similarity
/// cutoff (`MIN_CONSTRAINT_SIMILARITY`). A pure pass over the search results so
/// the cutoff behaviour is testable without a vector store.
pub(crate) fn rules_from_rows(
    search_results: Vec<crate::traits::SearchResult>,
) -> Vec<BehavioralRule> {
    search_results
        .into_iter()
        .filter(|result| (result.score as f32) >= MIN_CONSTRAINT_SIMILARITY)
        .filter(|result| RULE_MEMORY_TYPES.contains(&result.payload.memory_type.as_str()))
        .filter_map(|result| {
            // A rule row whose metadata is incomplete is not a rule: it is
            // dropped, and the drop is logged so a corrupt row is visible
            // instead of silently ignored.
            try_parse_rule_row(&result).or_else(|| {
                tracing::warn!(
                    "guard rule row {} missing required metadata field",
                    result.id
                );
                None
            })
        })
        .collect()
}

/// Extract the rule fields from one search row's metadata. `None` means the
/// row is missing at least one required field (`id`, `version`, `rule_class`,
/// `trigger_pattern`, `hit_count`, `status`).
fn try_parse_rule_row(result: &crate::traits::SearchResult) -> Option<BehavioralRule> {
    let metadata = &result.payload.metadata;
    Some(BehavioralRule {
        id: metadata.get("id")?.as_str()?.to_string(),
        version: metadata.get("version")?.as_u64()? as u32,
        rule_class: metadata.get("rule_class")?.as_str()?.to_string(),
        trigger_pattern: metadata.get("trigger_pattern")?.as_str()?.to_string(),
        hit_count: metadata.get("hit_count")?.as_u64()? as u32,
        status: metadata.get("status")?.as_str()?.to_string(),
        constraint_text: result.payload.content.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{MemoryPayload, SearchResult};
    use serde_json::json;

    fn rule_row(id: &str, score: f32, memory_type: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score,
            payload: MemoryPayload {
                content: "always use podman, never docker".to_string(),
                user_id: "guard".to_string(),
                memory_type: memory_type.to_string(),
                agent_name: Some("neurostrata-guard".to_string()),
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({
                    "id": id,
                    "version": 1,
                    "rule_class": "blocking",
                    "trigger_pattern": "docker",
                    "hit_count": 0,
                    "status": "active",
                }),
            },
            evidence: None,
        }
    }

    #[tokio::test]
    async fn test_guard_rule_storage() {
        use crate::guard::models::BehavioralRule;
        use crate::guard::testkit::{MockStore, StubEmbedder};
        use crate::traits::{Embedder, VectorStore};

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        let matcher = SemanticEvaluator::new(store, embedder, "guard");

        let rule = BehavioralRule {
            id: "rule-podman".to_string(),
            version: 1,
            rule_class: "blocking".to_string(),
            trigger_pattern: "docker".to_string(),
            constraint_text: "always use podman, never docker".to_string(),
            hit_count: 0,
            status: "active".to_string(),
        };
        matcher
            .add_constraint(rule)
            .await
            .expect("storing a guard rule must succeed");

        let rules = matcher
            .match_constraints("docker run attempted", 5)
            .await
            .expect("matching constraints must succeed");

        assert!(
            rules
                .iter()
                .any(|r| r.id == "rule-podman"
                    && r.constraint_text == "always use podman, never docker"),
            "stored guard rule must be retrievable via match_constraints, got: {rules:?}"
        );
    }

    /// Composite Rec #4: row conversion applies the similarity cutoff and the
    /// rule-type filter. A rule row below `MIN_CONSTRAINT_SIMILARITY` must not
    /// become a match, or loosely related memories would fire the guard as
    /// false positives.
    #[test]
    fn low_similarity_rule_rows_do_not_match() {
        let rows = vec![
            // A live rule that cleared the cutoff: the only row that survives.
            rule_row("rule-high", 0.9, "guard_rule"),
            // Same rule content, but the embedding is barely related — the
            // vector search still returned it, the cutoff must drop it.
            rule_row("rule-low", MIN_CONSTRAINT_SIMILARITY - 0.01, "guard_rule"),
            // Exactly at the floor: the cutoff is inclusive.
            rule_row("rule-at-floor", MIN_CONSTRAINT_SIMILARITY, "guard_rule"),
            // A high-scoring row that is not a rule at all (a fact, a task
            // result, anything the namespace picked up) must never fire.
            rule_row("not-a-rule", 0.99, "fact"),
        ];

        let rules = rules_from_rows(rows);

        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["rule-high", "rule-at-floor"],
            "only rules that clear the cutoff survive: {ids:?}"
        );
        // The surviving rows still carry their parsed rule fields.
        assert_eq!(rules[0].trigger_pattern, "docker");
        assert_eq!(rules[0].status, "active");
    }
}
