//! Semantic evaluator using LadyBugDB for vector search
//! 
//! Validates actions against behavioral rules stored as memories in the 'guard' namespace.
//! Each rule is embedded and stored with memory_type="guard_rule".

use crate::guard::models::BehavioralRule;
use crate::traits::{Embedder, VectorStore, MemoryPayload};
use anyhow::{Context, Result};
use std::sync::Arc;

pub struct SemanticEvaluator {
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn Embedder>,
    namespace: String,
}

impl SemanticEvaluator {
    pub fn new(
        store: Arc<dyn VectorStore>,
        embedder: Arc<dyn Embedder>,
        namespace: &str,
    ) -> Self {
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
            agent_name: Some("neurocortex".to_string()),
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
    pub async fn match_constraints(&self, payload: &str, limit: usize) -> Result<Vec<BehavioralRule>> {
        let vector = self.embedder.embed(payload).await?;
        
        let search_results = self.store.search(&self.namespace, vector, limit).await?;

        let rules = search_results
            .into_iter()
            .filter_map(|result| {
                let metadata = &result.payload.metadata;
                
                // Extract rule fields from metadata
                let id = metadata.get("id")?.as_str()?.to_string();
                let version = metadata.get("version")?.as_u64()? as u32;
                let rule_class = metadata.get("rule_class")?.as_str()?.to_string();
                let trigger_pattern = metadata.get("trigger_pattern")?.as_str()?.to_string();
                let hit_count = metadata.get("hit_count")?.as_u64()? as u32;
                let status = metadata.get("status")?.as_str()?.to_string();
                let constraint_text = result.payload.content;

                Some(BehavioralRule {
                    id,
                    version,
                    rule_class,
                    trigger_pattern,
                    constraint_text,
                    hit_count,
                    status,
                })
            })
            .collect();

        Ok(rules)
    }

    /// Evaluate whether a payload violates any constraints
    pub async fn evaluate(&self, payload: &str) -> Result<Vec<BehavioralRule>> {
        // Return top 3 matching rules
        self.match_constraints(payload, 3).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_guard_rule_storage() {
        // Integration test would go here
        // Requires mock VectorStore and Embedder
    }
}

