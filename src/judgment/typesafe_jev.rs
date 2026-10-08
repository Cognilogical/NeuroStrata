use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{DuplicateJudgment, JudgmentProvider};
use crate::traits::{MemoryPayload, SearchResult};

/// TypeSafe Jev provider for semantic duplicate detection.
/// Uses the TypeSafe HTTP API to make judgment calls.
pub struct TypeSafeJevProvider {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
}

impl TypeSafeJevProvider {
    pub fn new(api_key: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            api_key,
            client,
            base_url: "https://api.typesafe.ai/v1".to_string(),
        })
    }

    /// Build the request payload for TypeSafe API.
    fn build_request(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> serde_json::Value {
        let mut questions = serde_json::Map::new();

        for (i, candidate) in candidates.iter().enumerate() {
            questions.insert(
                format!("is_duplicate_{}", i),
                serde_json::json!({
                    "type": "noul",
                    "instructions": "Is the new memory a duplicate of this existing memory?",
                    "state": {
                        "new_memory": {
                            "content": new_memory.content,
                            "type": new_memory.memory_type,
                            "metadata": new_memory.metadata
                        },
                        "existing_memory": {
                            "id": candidate.id,
                            "content": candidate.payload.content,
                            "type": candidate.payload.memory_type,
                            "metadata": candidate.payload.metadata
                        }
                    },
                    "criteria": {
                        "positive": "Both memories express the same fact, rule, or architectural decision, even if worded differently",
                        "negative": "The memories are related but express different facts, rules, or decisions"
                    }
                }),
            );
        }

        serde_json::json!({
            "model": "jev-1.13",
            "questions": questions
        })
    }

    /// Parse the TypeSafe API response into duplicate judgments.
    fn parse_response(
        &self,
        response: &serde_json::Value,
        candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>> {
        let answers = response
            .get("answers")
            .and_then(|a| a.as_object())
            .context("Missing 'answers' in response")?;

        let mut judgments = Vec::new();

        for (i, candidate) in candidates.iter().enumerate() {
            let question_key = format!("is_duplicate_{}", i);
            if let Some(answer) = answers.get(&question_key) {
                let confidence = answer
                    .get("probability")
                    .and_then(|p| p.as_f64())
                    .unwrap_or(0.0) as f32;

                let reason = answer
                    .get("reasoning")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());

                if confidence > 0.5 {
                    // Only report if confidence suggests it might be a duplicate
                    judgments.push(DuplicateJudgment {
                        existing_memory_id: candidate.id.clone(),
                        confidence,
                        reason,
                    });
                }
            }
        }

        Ok(judgments)
    }
}

#[async_trait]
impl JudgmentProvider for TypeSafeJevProvider {
    async fn judge_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>> {
        if candidates.is_empty() {
            return Ok(vec![]);
        }

        let request = self.build_request(new_memory, candidates);

        let response = self
            .client
            .post(&format!("{}/evaluate", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to call TypeSafe API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("TypeSafe API error: {} - {}", status, body);
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse TypeSafe response")?;

        self.parse_response(&response_json, candidates)
    }

    async fn health_check(&self) -> Result<bool> {
        let response = self
            .client
            .get(&format!("{}/health", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await;

        Ok(response.map(|r| r.status().is_success()).unwrap_or(false))
    }

    fn name(&self) -> &str {
        "typesafe_jev"
    }
}
