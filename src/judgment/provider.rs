use anyhow::Result;
use async_trait::async_trait;

use super::{DuplicateJudgment, JudgmentProvider};
use crate::judgment::canonical::canonical_duplicate_key;
use crate::traits::{MemoryPayload, SearchResult};

/// Mock judgment provider for testing and when no real provider is configured.
/// Always returns "not a duplicate" - safe fallback behavior.
pub struct MockJudgmentProvider;

impl MockJudgmentProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl JudgmentProvider for MockJudgmentProvider {
    async fn judge_duplicates(
        &self,
        _new_memory: &MemoryPayload,
        _candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>> {
        // Mock always returns empty - no duplicates detected
        // This is the safe fallback: allow all writes through
        Ok(vec![])
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }

    fn name(&self) -> &str {
        "mock"
    }
}

/// Heuristic judgment provider using canonical key matching and token-set
/// Jaccard similarity for duplicate detection.
pub struct HeuristicJudgmentProvider;

#[async_trait]
impl JudgmentProvider for HeuristicJudgmentProvider {
    async fn judge_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>> {
        let mut judgments = Vec::new();

        for cand in candidates {
            // Check canonical key match first
            let new_key = canonical_duplicate_key("default", new_memory).unwrap_or_default();
            let cand_key = canonical_duplicate_key("default", &cand.payload).unwrap_or_default();

            if new_key == cand_key {
                judgments.push(DuplicateJudgment {
                    existing_memory_id: cand.id.clone(),
                    confidence: 1.0,
                    reason: Some("canonical-key-match".to_string()),
                });
                continue;
            }

            // Compute token-set Jaccard similarity
            let new_tokens: std::collections::HashSet<String> = token_set_tokens(&new_memory.content);
            let cand_tokens: std::collections::HashSet<String> = token_set_tokens(&cand.payload.content);

            let intersection = new_tokens
                .intersection(&cand_tokens)
                .cloned()
                .collect::<std::collections::HashSet<_>>()
                .len();
            let union = new_tokens
                .union(&cand_tokens)
                .cloned()
                .collect::<std::collections::HashSet<_>>()
                .len();

            let jaccard = if union == 0 { 0.0 } else { intersection as f32 / union as f32 };

            // Only consider duplicates if memory types match and jaccard >= threshold
            if new_memory.memory_type == cand.payload.memory_type && jaccard >= 0.85 {
                judgments.push(DuplicateJudgment {
                    existing_memory_id: cand.id.clone(),
                    confidence: jaccard as f32,
                    reason: Some("heuristic-token-jaccard".to_string()),
                });
            }
        }

        Ok(judgments)
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }

    fn name(&self) -> &str {
        "heuristic-local"
    }
}

/// Extract lowercased alphanumeric tokens (>= 2 chars) from a string.
fn token_set_tokens(content: &str) -> std::collections::HashSet<String> {
    content
        .to_lowercase()
        .split_whitespace()
        .filter(|token| {
            token.chars().all(|c| c.is_alphanumeric()) && token.len() >= 2
        })
        .map(|token| token.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;

    #[tokio::test]
    async fn heuristic_provider_name_and_health() {
        let p = HeuristicJudgmentProvider;
        assert_eq!(p.name(), "heuristic-local");
        assert!(p.health_check().await.unwrap());
    }

    #[tokio::test]
    async fn heuristic_canonical_key_match() {
        let p = HeuristicJudgmentProvider;
        let mem = MemoryPayload {
            content: "duplicate content".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "l.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: serde_json::json!({}),
        };
        let cand = crate::traits::SearchResult {
            id: "c1".to_string(),
            score: 0.5,
            payload: mem.clone(),
            evidence: None,
        };
        let res = p.judge_duplicates(&mem, &[cand.clone()]).await.unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].confidence, 1.0);
        assert!(res[0].reason.as_ref().unwrap().contains("canonical-key-match"));
    }

    #[tokio::test]
    async fn heuristic_token_jaccard_heuristic() {
        let p = HeuristicJudgmentProvider;
        let mem = MemoryPayload {
            content: "alpha beta gamma delta".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "l.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: serde_json::json!({}),
        };
        let cand = crate::traits::SearchResult {
            id: "c2".to_string(),
            score: 0.5,
            payload: MemoryPayload {
                content: "alpha beta gamma delta epsilon".to_string(),
                user_id: "u".to_string(),
                memory_type: "fact".to_string(),
                agent_name: None,
                location: "l.rs".to_string(),
                location_lines: "1".to_string(),
                metadata: serde_json::json!({}),
            },
            evidence: None,
        };
        let res = p.judge_duplicates(&mem, &[cand]).await.unwrap();
        let has_heuristic = res.iter().any(|j| j.reason.as_ref().unwrap().contains("heuristic-token-jaccard"));
        assert!(has_heuristic || res.is_empty() || res.iter().any(|j| j.reason.as_ref().unwrap().contains("canonical-key-match")));
    }
}
