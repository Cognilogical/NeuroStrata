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

/// The store reports each row's distance (lower is closer), so the similarity
/// floor is applied as a maximum distance, the same mapping the normative
/// search slots use (`similarity = 1 - distance`).
const MAX_CONSTRAINT_DISTANCE: f32 = 1.0 - MIN_CONSTRAINT_SIMILARITY;

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
        // The embedding only picks candidates; the trigger match below runs on
        // the full payload. A whole-file edit does not need to be embedded
        // whole.
        let vector = self.embedder.embed(embed_input(payload)).await?;

        // Only rule rows can become guard rules, so the scan is limited to
        // those types instead of ranking the whole namespace (every AST
        // symbol and doc section) and discarding all but a few rows.
        let rule_types: Vec<String> = RULE_MEMORY_TYPES.iter().map(|t| t.to_string()).collect();
        let mut combined = self
            .store
            .search_by_types(&self.namespace, vector.clone(), limit, &rule_types)
            .await?;

        let mut seen_ids: std::collections::HashSet<String> =
            combined.iter().map(|r| r.id.clone()).collect();

        if let Ok(listed) = self
            .store
            .list(&self.namespace, None, Some("guard_rule"))
            .await
        {
            merge_listed_rules(&mut combined, &mut seen_ids, listed);
        }

        if self.namespace != "global" {
            if let Ok(global_listed) = self
                .store
                .list("global", None, Some("guard_rule"))
                .await
            {
                merge_listed_rules(&mut combined, &mut seen_ids, global_listed);
            }
            if let Ok(global_search) = self
                .store
                .search_by_types("global", vector, limit, &rule_types)
                .await
            {
                for row in global_search {
                    if seen_ids.insert(row.id.clone()) {
                        combined.push(row);
                    }
                }
            }
        }

        let rules = rules_from_rows(combined);

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
        let (target_ns, vector, mut payload) =
            if let Some((v, p)) = self.store.get(&self.namespace, rule_id).await? {
                (self.namespace.as_str(), v, p)
            } else if self.namespace != "global" {
                let Some((v, p)) = self.store.get("global", rule_id).await? else {
                    return Ok(());
                };
                ("global", v, p)
            } else {
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
            .upsert(target_ns, rule_id, vector, payload)
            .await
            .context("Failed to record the rule hit")?;

        Ok(())
    }
}

/// The most of a payload the guard embeds to pick rule candidates.
const MAX_EMBED_CHARS: usize = 2000;

/// The leading part of `payload` the guard embeds, cut on a char boundary.
fn embed_input(payload: &str) -> &str {
    match payload.char_indices().nth(MAX_EMBED_CHARS) {
        Some((end, _)) => &payload[..end],
        None => payload,
    }
}

/// Turn raw search rows into guard rules, dropping every row that either
/// cannot prove it is a rule (rule types only) or sits below the similarity
/// cutoff (`MIN_CONSTRAINT_SIMILARITY`). `SearchResult::score` is a distance
/// (lower is closer), so a row passes when its distance is at most
/// `MAX_CONSTRAINT_DISTANCE`. A pure pass over the search results so the
/// cutoff behaviour is testable without a vector store.
pub(crate) fn rules_from_rows(
    search_results: Vec<crate::traits::SearchResult>,
) -> Vec<BehavioralRule> {
    search_results
        .into_iter()
        .filter(|result| (result.score as f32) <= MAX_CONSTRAINT_DISTANCE)
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

/// Adds the listed `guard_rule` rows to `combined`. A listed guard rule is
/// always eligible, whatever its distance to the payload: the trigger match
/// that follows decides whether it fires. Search scores are distances, so a
/// listed rule is given distance 0.0 (the closest possible), which clears
/// `rules_from_rows`. A rule the typed search already returned keeps its row
/// but takes that distance too.
fn merge_listed_rules(
    combined: &mut Vec<crate::traits::SearchResult>,
    seen_ids: &mut std::collections::HashSet<String>,
    listed: Vec<crate::traits::SearchResult>,
) {
    for mut row in listed {
        if seen_ids.insert(row.id.clone()) {
            row.score = 0.0;
            combined.push(row);
        } else if let Some(existing) = combined.iter_mut().find(|r| r.id == row.id) {
            existing.score = 0.0;
        }
    }
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

    /// A generic `rule` row is not a `guard_rule`, so the guard listing never
    /// returns it: it can only reach the guard through typed search. With the
    /// mock reporting distances like the real store, that path is exercised.
    #[tokio::test]
    async fn a_rule_row_reaches_the_guard_only_through_typed_search() {
        use crate::guard::testkit::{MockStore, StubEmbedder};
        use crate::traits::{Embedder, VectorStore};

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        let payload = rule_row("seeded-rule", 0.0, "rule").payload;
        store
            .upsert("guard", "seeded-rule", vec![0.0; 4], payload)
            .await
            .expect("seeding a rule row must succeed");

        let matcher = SemanticEvaluator::new(store, embedder, "guard");
        let rules = matcher
            .match_constraints("docker run attempted", 5)
            .await
            .expect("matching constraints must succeed");

        assert!(
            rules.iter().any(|r| r.id == "seeded-rule"),
            "a rule row found by typed search must survive the distance floor: {rules:?}"
        );
    }

    /// Composite Rec #4: row conversion applies the similarity cutoff and the
    /// rule-type filter. A rule row below `MIN_CONSTRAINT_SIMILARITY` must not
    /// become a match, or loosely related memories would fire the guard as
    /// false positives.
    #[test]
    fn embed_input_is_bounded_and_cuts_on_a_char_boundary() {
        assert_eq!(embed_input("short"), "short");
        let long = "é".repeat(MAX_EMBED_CHARS + 50);
        let cut = embed_input(&long);
        assert_eq!(cut.chars().count(), MAX_EMBED_CHARS);
        assert!(long.starts_with(cut));
        let exact = "a".repeat(MAX_EMBED_CHARS);
        assert_eq!(embed_input(&exact), exact);
    }

    /// Typed search returns the closest rules with their distance as the
    /// score. A guard rule that is also in the listing must stay eligible even
    /// when it is the closest match (distance below the cutoff).
    #[test]
    fn a_listed_guard_rule_stays_eligible_when_typed_search_already_returned_it() {
        let mut combined = vec![
            // The nearest rule: distance 0.05, so below the cutoff.
            rule_row("rule-near", 0.05, "guard_rule"),
        ];
        let mut seen: std::collections::HashSet<String> =
            combined.iter().map(|r| r.id.clone()).collect();
        // The listing returns every guard rule, including the near one, with
        // the store's own score of 0.
        let listed = vec![rule_row("rule-near", 0.0, "guard_rule"), rule_row("rule-other", 0.0, "guard_rule")];

        merge_listed_rules(&mut combined, &mut seen, listed);

        let rules = rules_from_rows(combined);
        let mut ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec!["rule-near", "rule-other"],
            "the nearest guard rule must not be dropped by the distance cutoff: {ids:?}"
        );
    }

    /// The real store's typed search scores rows by L2 distance (lower is
    /// closer). A rule whose embedding equals the query is distance 0 and must
    /// survive the floor; a rule far from the query must not.
    #[tokio::test]
    async fn typed_search_distances_are_read_as_distances_by_the_floor() {
        use crate::store::ladybug::LadybugStore;
        use crate::traits::VectorStore;

        let dir = std::env::temp_dir().join(format!("ns-guard-floor-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let payload = |id: &str| rule_row(id, 0.0, "rule").payload;
        store
            .upsert("probe", "rule-near", vec![0.9, 0.1, 0.1, 0.1], payload("rule-near"))
            .await
            .unwrap();
        store
            .upsert("probe", "rule-far", vec![-0.9, -0.1, -0.1, -0.1], payload("rule-far"))
            .await
            .unwrap();

        let types = vec!["rule".to_string(), "guard_rule".to_string()];
        let found = store
            .search_by_types("probe", vec![0.9, 0.1, 0.1, 0.1], 5, &types)
            .await
            .unwrap();
        let near = found.iter().find(|r| r.id == "rule-near").expect("near row");
        assert_eq!(near.score, 0.0, "the store reports a distance, not a similarity");

        let ids: Vec<String> = rules_from_rows(found).into_iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec!["rule-near".to_string()],
            "the identical rule must survive the floor and the opposite one must not: {ids:?}"
        );
    }

    #[test]
    fn low_similarity_rule_rows_do_not_match() {
        // Scores are distances: lower is closer, and a row passes while its
        // distance is at most MAX_CONSTRAINT_DISTANCE.
        let rows = vec![
            // A close rule that cleared the cutoff.
            rule_row("rule-high", 0.1, "guard_rule"),
            // Same rule content, but the embedding is barely related: the
            // vector search still returned it, the cutoff must drop it.
            rule_row("rule-low", MAX_CONSTRAINT_DISTANCE + 0.01, "guard_rule"),
            // Exactly at the floor: the cutoff is inclusive.
            rule_row("rule-at-floor", MAX_CONSTRAINT_DISTANCE, "guard_rule"),
            // A very close row that is not a rule at all (a fact, a task
            // result, anything the namespace picked up) must never fire.
            rule_row("not-a-rule", 0.01, "fact"),
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
