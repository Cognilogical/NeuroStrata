pub mod canonical;
pub mod circuit_breaker;
pub mod policy;
pub mod provider;
pub mod typesafe_jev;
pub mod laya_onnx;

pub use policy::{CalibrationTier, DedupDecision};

pub use laya_onnx::{ModelVersion, RuntimeState};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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

use crate::config::{DeduplicationConfig, DedupProviderMode};

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

/// A set of judgment providers selected for dedup checking.
/// Each mode (Auto, Heuristic, LayaLocal, Jev, None) selects one or two
/// providers — the "active" one and an optional "shadow" provider.
#[derive(Clone)]
pub struct ProviderSet {
    pub active: Option<Arc<dyn JudgmentProvider>>,
    pub shadow: Option<Arc<dyn JudgmentProvider>>,
}

/// Select judgment providers based on config mode and provided Laya/Jev candidates.
/// When config.provider is None or explicit Jev without a key, no provider is selected
/// and the baseline write path is allowed (per Step 6: no automatic Jev failover).
pub fn select_dedup_providers(
    config: &DeduplicationConfig,
    laya: Option<Arc<dyn JudgmentProvider>>,
    jev: Option<Arc<dyn JudgmentProvider>>,
) -> Result<ProviderSet> {
    let (active, shadow): (Option<Arc<dyn JudgmentProvider>>, Option<Arc<dyn JudgmentProvider>>) = match config.provider {
        DedupProviderMode::Auto => {
            // Auto: local heuristic as active, Laya local as shadow if available
            let active = Some(Arc::new(crate::judgment::provider::MockJudgmentProvider) as Arc<dyn JudgmentProvider>);
            let shadow = laya;
            (active, shadow)
        }
        DedupProviderMode::Heuristic => {
            // Heuristic: local heuristic only, no shadow
            let active = Some(Arc::new(crate::judgment::provider::MockJudgmentProvider) as Arc<dyn JudgmentProvider>);
            (active, None)
        }
        DedupProviderMode::LayaLocal => {
            // LayaLocal: use provided Laya model as active, no shadow
            let active = laya.ok_or_else(|| anyhow::anyhow!("LayaLocal requires a Laya provider"))?;
            (Some(active), None)
        }
        DedupProviderMode::Jev => {
            // Jev: use the Jev provider
            let active = jev.ok_or_else(|| anyhow::anyhow!("Jev provider is required"))?;
            (Some(active), None)
        }
        DedupProviderMode::None => {
            // None: disable dedup
            (None, None)
        }
    };
    Ok(ProviderSet { active, shadow })
}

/// The main deduplication checker that orchestrates the process.
pub struct DeduplicationChecker {
    pub config: DeduplicationConfig,
    provider_set: ProviderSet,
    circuit_breaker: circuit_breaker::CircuitBreaker,
}

impl DeduplicationChecker {
    pub fn new(config: DeduplicationConfig) -> Result<Self> {
        // Determine the provider set based on explicit provider mode.
        // `none` alone disables dedup learning (baseline write path is allowed).
        let provider_set = select_dedup_providers(
            &config,
            Some(Arc::new(crate::judgment::provider::MockJudgmentProvider)),
            Some(Arc::new(typesafe_jev::TypeSafeJevProvider::new(
                config.judgment_api_key.clone().unwrap_or_default(),
            )?)),
        )?;

        let cb_config = CircuitBreakerConfig {
            failure_threshold: config.cb_failure_threshold,
            initial_backoff_ms: config.cb_backoff_initial_seconds * 1000,
            max_backoff_ms: config.cb_backoff_max_seconds * 1000,
            backoff_multiplier: 2.0,
        };

        Ok(Self {
            config,
            provider_set,
            circuit_breaker: circuit_breaker::CircuitBreaker::new(cb_config),
        })
    }

    /// Check if a new memory is a duplicate of existing memories.
    /// Returns None if deduplication is disabled (provider mode is None) or
    /// circuit is open. Provider failures allow the baseline write path.
    pub async fn check_duplicates(
        &self,
        new_memory: &MemoryPayload,
        candidates: &[SearchResult],
    ) -> Option<Vec<DuplicateJudgment>> {
        let active = self.provider_set.active.clone()?;
        if let Some(provider) = &self.provider_set.active {
            if !self.circuit_breaker.allow_request() {
                tracing::debug!(
                    provider = provider.name(),
                    "Circuit breaker open, skipping deduplication"
                );
                return None;
            }
        }

        // Apply timeout to judgment call
        let timeout = Duration::from_secs(self.config.timeout_seconds);
        let result = tokio::time::timeout(timeout, async {
            self.provider_set
                .active
                .as_ref()
                .unwrap()
                .judge_duplicates(new_memory, candidates)
                .await
        })
        .await;

        match result {
            Ok(Ok(judgments)) => {
                if let Some(provider) = &self.provider_set.active {
                    self.circuit_breaker.record_success();
                }
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
                tracing::warn!(
                    provider = "judgment",
                    error = %e,
                    "Judgment provider error"
                );
                if let Some(provider) = &self.provider_set.active {
                    self.circuit_breaker.record_failure();
                }
                None
            }
            Err(_) => {
                tracing::warn!(
                    provider = "judgment",
                    timeout_seconds = self.config.timeout_seconds,
                    "Judgment call timed out"
                );
                if let Some(provider) = &self.provider_set.active {
                    self.circuit_breaker.record_failure();
                }
                None
            }
        }
    }

    /// Health check for the judgment provider.
    pub async fn health_check(&self) -> bool {
        if let Some(provider) = &self.provider_set.active {
            provider.health_check().await.unwrap_or(false)
        } else {
            false
        }
    }

    /// Test-only constructor allowing injection of a custom provider.
    #[cfg(test)]
    pub fn new_with_provider(
        config: DeduplicationConfig,
        provider: Arc<dyn JudgmentProvider>,
    ) -> Self {
        let provider_set = ProviderSet {
            active: Some(provider),
            shadow: None,
        };
        let cb_config = CircuitBreakerConfig {
            failure_threshold: config.cb_failure_threshold,
            initial_backoff_ms: config.cb_backoff_initial_seconds * 1000,
            max_backoff_ms: config.cb_backoff_max_seconds * 1000,
            backoff_multiplier: 2.0,
        };
        Self {
            config,
            provider_set,
            circuit_breaker: circuit_breaker::CircuitBreaker::new(cb_config),
        }
    }
}
