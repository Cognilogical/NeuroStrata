pub mod circuit_breaker;
pub mod provider;
pub mod typesafe_jev;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::traits::{MemoryPayload, SearchResult};

/// Result of a duplicate judgment from a judgment model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateJudgment {
    pub existing_memory_id: String,
    pub confidence: f32,
    pub reason: Option<String>,
}

/// A provider that can make semantic duplicate judgments.
/// This trait is model-agnostic - any judgment model (TypeSafe Jev, custom LLM, etc.)
/// can implement this interface.
#[async_trait]
pub trait JudgmentProvider: Send + Sync {
    /// Judge whether a new memory is a duplicate of an existing one.
    /// Returns a list of duplicates with confidence scores.
    async fn judge_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Result<Vec<DuplicateJudgment>>;

    /// Health check - verify the provider is operational.
    async fn health_check(&self) -> Result<bool>;

    /// Provider name for logging/metrics.
    fn name(&self) -> &str;
}

use crate::config::DeduplicationConfig;

/// Circuit breaker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening circuit
    pub failure_threshold: u32,
    
    /// Initial backoff duration (milliseconds)
    pub initial_backoff_ms: u64,
    
    /// Maximum backoff duration (milliseconds)
    pub max_backoff_ms: u64,
    
    /// Backoff multiplier
    pub backoff_multiplier: f64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            initial_backoff_ms: 1000,
            max_backoff_ms: 60000, // 1 minute max
            backoff_multiplier: 2.0,
        }
    }
}

/// The main deduplication checker that orchestrates the process.
pub struct DeduplicationChecker {
    config: DeduplicationConfig,
    provider: Box<dyn JudgmentProvider>,
    circuit_breaker: circuit_breaker::CircuitBreaker,
}

impl DeduplicationChecker {
    pub fn new(config: DeduplicationConfig) -> Result<Self> {
        // Determine provider based on API key presence - the key itself is the enable signal
        let provider: Box<dyn JudgmentProvider> = if config.judgment_api_key.is_some() {
            Box::new(typesafe_jev::TypeSafeJevProvider::new(config.judgment_api_key.clone().unwrap())?)
        } else {
            Box::new(provider::MockJudgmentProvider::new())
        };

        // Construct circuit breaker config from flattened config fields
        let cb_config = CircuitBreakerConfig {
            failure_threshold: config.cb_failure_threshold,
            initial_backoff_ms: config.cb_backoff_initial_seconds * 1000,
            max_backoff_ms: config.cb_backoff_max_seconds * 1000,
            backoff_multiplier: 2.0,
        };

        Ok(Self {
            config: config.clone(),
            provider,
            circuit_breaker: circuit_breaker::CircuitBreaker::new(cb_config),
        })
    }

    /// Check if a new memory is a duplicate of existing memories.
    /// Returns None if deduplication is disabled (no API key) or circuit is open.
    pub async fn check_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Option<Vec<DuplicateJudgment>> {
        // The presence of an API key is the enable signal - no redundant config
        if self.config.judgment_api_key.is_none() {
            return None;
        }

        // Check circuit breaker
        if !self.circuit_breaker.allow_request() {
            tracing::debug!(
                provider = self.provider.name(),
                "Circuit breaker open, skipping deduplication"
            );
            return None;
        }

        // Apply timeout to judgment call
        let timeout = Duration::from_secs(self.config.timeout_seconds);
        let result = tokio::time::timeout(timeout, async {
            self.provider
                .judge_duplicates(new_memory, candidates)
                .await
        })
        .await;

        match result {
            Ok(Ok(judgments)) => {
                // Success - reset circuit breaker
                self.circuit_breaker.record_success();
                
                // Filter by confidence threshold
                let duplicates: Vec<_> = judgments
                    .into_iter()
                    .filter(|j| j.confidence >= self.config.confidence_threshold)
                    .collect();

                if duplicates.is_empty() {
                    None
                } else {
                    Some(duplicates)
                }
            }
            Ok(Err(e)) => {
                // Provider error - record failure
                tracing::warn!(
                    provider = self.provider.name(),
                    error = %e,
                    "Judgment provider error"
                );
                self.circuit_breaker.record_failure();
                None
            }
            Err(_) => {
                // Timeout - record failure
                tracing::warn!(
                    provider = self.provider.name(),
                    timeout_seconds = self.config.timeout_seconds,
                    "Judgment call timed out"
                );
                self.circuit_breaker.record_failure();
                None
            }
        }
    }

    /// Health check for the judgment provider.
    pub async fn health_check(&self) -> bool {
        self.provider.health_check().await.unwrap_or(false)
    }
}
