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

    /// R13: fail closed on non-loopback egress. A remote base_url throws the
    /// provider's traffic off-host, so it is only allowed when the operator has
    /// explicitly opted in (`NEUROSTRATA_ALLOW_REMOTE_EGRESS=1`) AND supplied a
    /// non-empty api_key. Loopback targets are always allowed.
    fn assert_egress_authorized(&self) -> Result<()> {
        let host = self
            .base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or("");
        let loopback = matches!(host, "localhost" | "[::1]" | "::1")
            || host.starts_with("127.")
            || host == "[::ffff:127.0.0.1]";
        if loopback {
            return Ok(());
        }
        let permitted =
            std::env::var("NEUROSTRATA_ALLOW_REMOTE_EGRESS").ok().as_deref() == Some("1");
        let has_key = !self.api_key.trim().is_empty();
        if !permitted || !has_key {
            anyhow::bail!(
                "Remote judgment egress refused: non-loopback base_url requires NEUROSTRATA_ALLOW_REMOTE_EGRESS=1 and non-empty api_key (R13)"
            );
        }
        Ok(())
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
                let confidence = match answer.get("probability") {
                    Some(prob) if prob.is_f64() => prob.as_f64(),
                    Some(prob) if prob.is_i64() => Some((prob.as_i64().unwrap_or(0) as f64) / 100.0),
                    Some(prob) if prob.is_string() => {
                        let s = prob.as_str().unwrap_or("");
                        match s.to_ascii_lowercase().as_str() {
                            "yes" | "true" | "duplicate" => Some(0.9),
                            "no" | "false" => Some(0.1),
                            _ => None,
                        }
                    }
                    // Fallback: check for SystemOne noul in alternative fields
                    _ => answer.get("answer").or_else(|| answer.get("verdict")).or_else(|| answer.get("noul")).and_then(|v| v.as_str()).map(|s| Some(match s.to_ascii_lowercase().as_str() {
                        "yes" | "true" | "duplicate" => 0.9,
                        "no" | "false" => 0.1,
                        _ => 0.0,
                    })).flatten(),
                };
                let confidence = confidence.unwrap_or(0.0) as f32;

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

        // R13: fail closed BEFORE any network I/O leaves the process.
        self.assert_egress_authorized()?;

        let request = self.build_request(new_memory, candidates);

        let post_url = if self.base_url.ends_with("/systemone") || self.base_url.ends_with("/evaluate") {
            self.base_url.clone()
        } else {
            format!("{}/systemone", self.base_url.trim_end_matches('/'))
        };
        let response = self
            .client
            .post(&post_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to call TypeSafe API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = match response.text().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("failed to read TypeSafe Jev error body: {e}");
                    String::new()
                }
            };
            anyhow::bail!("TypeSafe API error: {} - {}", status, body);
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse TypeSafe response")?;

        self.parse_response(&response_json, candidates)
    }

    async fn health_check(&self) -> Result<bool> {
        // R13: the health probe is also real network I/O; same fail-closed rule.
        if self.assert_egress_authorized().is_err() {
            return Ok(false);
        }
        let response = self
            .client
            .get(&format!("{}/health", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await;

        match response {
            Ok(r) => Ok(r.status().is_success()),
            Err(e) => {
                tracing::debug!("TypeSafe Jev health probe transport error: {e}");
                Ok(false)
            }
        }
    }

    fn name(&self) -> &str {
        "typesafe_jev"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_provider() -> TypeSafeJevProvider {
        let provider = TypeSafeJevProvider::new(String::new())
            .expect("provider constructs even without a key");
        provider
    }

    /// R13: a non-loopback base_url must be refused before any HTTP request is
    /// sent when the egress opt-in and api_key requirements are unmet.
    #[tokio::test]
    async fn remote_base_url_fails_closed_without_egress_authorization() {
        std::env::set_var("NEUROSTRATA_ALLOW_REMOTE_EGRESS", "0");
        let provider = remote_provider();

        let err = provider
            .assert_egress_authorized()
            .expect_err("remote base_url without authorization is refused");
        assert!(
            err.to_string().contains("(R13)"),
            "unexpected error: {err}"
        );

        // Fail closed through the actual judgment path (network call never
        // attempts to reach the remote host).
        let new_memory = MemoryPayload {
            content: "new fact".to_string(),
            user_id: "agent".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        let candidates = vec![SearchResult {
            id: "existing-1".to_string(),
            score: 0.9,
            payload: MemoryPayload {
                content: "existing fact".to_string(),
                user_id: "agent".to_string(),
                memory_type: "fact".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::json!({}),
            },
            evidence: None,
        }];

        let err = provider
            .judge_duplicates(&new_memory, &candidates)
            .await
            .expect_err("egress refused before any network call");
        assert!(
            err.to_string().contains("Remote judgment egress refused"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn loopback_base_url_is_exempt_from_egress_gate() {
        let mut provider = TypeSafeJevProvider::new("test-key".to_string())
            .expect("provider constructs");
        provider.base_url = "http://127.0.0.1:8080/v1".to_string();
        provider
            .assert_egress_authorized()
            .expect("loopback target never trips the egress gate");
    }

    #[tokio::test]
    async fn parse_response_handles_systemone_noui_answers() {
        // Test that parse_response accepts both numeric probability and SystemOne noul fields
        let provider = TypeSafeJevProvider::new(String::new())
            .expect("provider constructs even without a key");

        // Yes answer via "noui" field (SystemOne style)
        let resp_yes = serde_json::json!({
            "answers": {
                "is_duplicate_0": {
                    "answer": "yes",
                    "verdict": null,
                    "noui": "yes"
                }
            }
        });
        let mut cands = vec![];
        let new_mem = MemoryPayload {
            content: "test".to_string(),
            user_id: String::new(),
            memory_type: String::new(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        // dummy candidate
        cands.push(crate::traits::SearchResult {
            id: "c1".to_string(),
            score: 0.9,
            payload: new_mem.clone(),
            evidence: None,
        });
        let judgments = provider
            .parse_response(&resp_yes, &cands)
            .unwrap();
        assert!(!judgments.is_empty());
        assert!(
            judgments[0].confidence > 0.5,
            "noui 'yes' should yield confidence > 0.5"
        );

        // No answer via "noui" field
        let resp_no = serde_json::json!({
            "answers": {
                "is_duplicate_0": {
                    "answer": "no",
                    "verdict": null,
                    "noui": "no"
                }
            }
        });
        let judgments2 = provider.parse_response(&resp_no, &cands).unwrap();
        assert!(judgments2.is_empty(), "noui 'no' should yield confidence <= 0.5");

        // Probability answer still works
        let resp_prob = serde_json::json!({
            "answers": {
                "is_duplicate_0": {
                    "probability": 0.8,
                    "reasoning": "duplicate"
                }
            }
        });
        let judgments3 = provider.parse_response(&resp_prob, &cands).unwrap();
        assert!(!judgments3.is_empty());
        assert_eq!(judgments3[0].confidence, 0.8);
    }
}
