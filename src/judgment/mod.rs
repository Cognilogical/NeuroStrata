pub mod canonical;
pub mod circuit_breaker;
pub mod policy;
pub mod provider;
pub mod typesafe_jev;
pub mod laya_onnx;

pub use policy::{CalibrationTier, DedupDecision};

pub use laya_onnx::RuntimeState;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

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

/// The verdict of a shadow (non-gating) provider: the full judged list.
pub type DuplicateVerdict = Vec<DuplicateJudgment>;

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
            let active = Some(Arc::new(crate::judgment::provider::HeuristicJudgmentProvider) as Arc<dyn JudgmentProvider>);
            let shadow = laya;
            (active, shadow)
        }
        DedupProviderMode::Heuristic => {
            // Heuristic: local heuristic only, no shadow
            let active = Some(Arc::new(crate::judgment::provider::HeuristicJudgmentProvider) as Arc<dyn JudgmentProvider>);
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
    /// Last shadow provider verdict: (provider name, judgments). Kept, never
    /// discarded — the shadow records alongside without gating the write.
    last_shadow: std::sync::Arc<std::sync::Mutex<Option<(String, DuplicateVerdict)>>>,
}

impl DeduplicationChecker {
    pub fn new(config: DeduplicationConfig) -> Result<Self> {
        // Load a real Laya ONNX provider from the bundle when it exists and
        // parses; otherwise log WHY it is absent (at warn, so misconfiguration
        // is visible) and continue without a laya provider.
        let laya_bundle_path = if config.laya_bundle_path.trim().is_empty() { std::path::PathBuf::from("artifacts/laya-bundle") } else { std::path::PathBuf::from(&config.laya_bundle_path) };
        let laya_provider = match crate::judgment::laya_onnx::LayaOnnxProvider::load(
            &laya_bundle_path,
            Arc::new(Semaphore::new(4)),
        ) {
            Ok(p) => Some(Arc::new(p) as Arc<dyn JudgmentProvider>),
            Err(e) => {
                tracing::warn!(
                    "Failed to load Laya ONNX bundle at {}: {e}",
                    laya_bundle_path.display()
                );
                None
            }
        };

        // Determine the provider set based on explicit provider mode.
        // `none` alone disables dedup learning (baseline write path is allowed).
        // The Jev API key resolves from the explicit config value first and,
        // when absent/blank, from the env var named by `judgment_api_key_env`
        // so the env-var indirection in the config is genuinely wired.
        let jev_api_key = config
            .judgment_api_key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                config
                    .judgment_api_key_env
                    .as_ref()
                    .and_then(|env| std::env::var(env).ok())
                    .filter(|k| !k.trim().is_empty())
            })
            .unwrap_or_default();
        let provider_set = select_dedup_providers(
            &config,
            laya_provider,
            Some(Arc::new(typesafe_jev::TypeSafeJevProvider::new(
                jev_api_key,
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
            last_shadow: std::sync::Arc::new(std::sync::Mutex::new(None)),
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
        let Some(active) = self.provider_set.active.as_ref() else {
            return None;
        };
        if !self.circuit_breaker.allow_request() {
            tracing::debug!(
                provider = active.name(),
                cb_state = %self.circuit_breaker.state(),
                failures = self.circuit_breaker.failure_count(),
                "Circuit breaker open, skipping deduplication"
            );
            return None;
        }
        // The shadow provider records its verdict alongside without gating
        // the write: store (provider name, verdict) for inspection, and
        // never fabricate one when it errors, times out, or is skipped.
        let timeout = Duration::from_secs(self.config.timeout_seconds);
        if let Some(shadow) = &self.provider_set.shadow {
            // Clear any verdict from a prior call FIRST: a stale shadow
            // verdict from an earlier request must never leak into a later
            // one when the shadow fails, times out, or is not invoked.
            *self.last_shadow.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let shadow_result = tokio::time::timeout(
                timeout,
                shadow.judge_duplicates(new_memory, candidates),
            )
            .await;
            match shadow_result {
                Ok(Ok(verdict)) => {
                    *self
                        .last_shadow
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) =
                        Some((shadow.name().to_string(), verdict));
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        provider = shadow.name(),
                        error = %e,
                        "Shadow judgment failed; verdict not recorded"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        provider = shadow.name(),
                        timeout_secs = timeout.as_secs(),
                        "Shadow judgment timed out; verdict not recorded"
                    );
                }
            }
        }

        // Apply timeout to judgment call
        let result = tokio::time::timeout(timeout, async {
            active.judge_duplicates(new_memory, candidates).await
        })
        .await;

        match result {
            Ok(Ok(judgments)) => {
                self.circuit_breaker.record_success();
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
                    cb_state = %self.circuit_breaker.state(),
                    failures = self.circuit_breaker.failure_count(),
                    "Judgment provider error"
                );
                self.circuit_breaker.record_failure();
                None
            }
            Err(_) => {
                tracing::warn!(
                    provider = "judgment",
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
        if let Some(provider) = &self.provider_set.active {
            match provider.health_check().await {
                Ok(ok) => ok,
                Err(e) => {
                    tracing::warn!("judgment provider health_check failed: {e}");
                    false
                }
            }
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
            last_shadow: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// The last recorded shadow provider verdict, if any: (provider name, judgments).
    pub fn last_shadow_verdict(&self) -> Option<(String, DuplicateVerdict)> {
        self.last_shadow.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Test-only constructor injecting both an active and a shadow provider.
    #[cfg(test)]
    pub fn new_with_active_and_shadow(
        config: DeduplicationConfig,
        active: Arc<dyn JudgmentProvider>,
        shadow: Arc<dyn JudgmentProvider>,
    ) -> Self {
        let provider_set = ProviderSet {
            active: Some(active),
            shadow: Some(shadow),
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
            last_shadow: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{MemoryPayload, SearchResult};

    struct FakeJudgmentProvider {
        name: &'static str,
        confidence: f32,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl JudgmentProvider for FakeJudgmentProvider {
        async fn judge_duplicates(
            &self,
            _new_memory: &MemoryPayload,
            candidates: &[SearchResult],
        ) -> Result<Vec<DuplicateJudgment>> {
            if self.fail {
                anyhow::bail!("fake shadow failure");
            }
            Ok(candidates
                .iter()
                .map(|c| DuplicateJudgment {
                    existing_memory_id: c.id.clone(),
                    confidence: self.confidence,
                    reason: Some(self.name.to_string()),
                })
                .collect())
        }

        async fn health_check(&self) -> Result<bool> {
            Ok(!self.fail)
        }

        fn name(&self) -> &str {
            self.name
        }
    }

    fn dummy_memory() -> MemoryPayload {
        MemoryPayload {
            content: "hello".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "loc".to_string(),
            location_lines: "1".to_string(),
            metadata: serde_json::Value::Null,
        }
    }

    fn dummy_candidate(id: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score: 0.9,
            payload: dummy_memory(),
            evidence: None,
        }
    }

    #[tokio::test]
    async fn shadow_verdict_is_recorded_not_discarded() {
        let config = DeduplicationConfig::default();
        let checker = DeduplicationChecker::new_with_active_and_shadow(
            config,
            Arc::new(FakeJudgmentProvider { name: "active", confidence: 0.9, fail: false }),
            Arc::new(FakeJudgmentProvider { name: "shadow-test", confidence: 0.95, fail: false }),
        );
        let mem = dummy_memory();
        let cands = vec![dummy_candidate("c1")];
        checker.check_duplicates(&mem, &cands).await;
        let verdict = checker.last_shadow_verdict().expect("shadow verdict must be stored");
        assert_eq!(verdict.0, "shadow-test");
        assert_eq!(verdict.1.len(), 1);
        assert_eq!(verdict.1[0].existing_memory_id, "c1");
        assert!((verdict.1[0].confidence - 0.95).abs() < 1e-6);
    }

    #[tokio::test]
    async fn shadow_error_records_no_verdict() {
        let config = DeduplicationConfig::default();
        let checker = DeduplicationChecker::new_with_active_and_shadow(
            config,
            Arc::new(FakeJudgmentProvider { name: "active", confidence: 0.9, fail: false }),
            Arc::new(FakeJudgmentProvider { name: "shadow-test", confidence: 0.95, fail: true }),
        );
        let mem = dummy_memory();
        let cands = vec![dummy_candidate("c1")];
        checker.check_duplicates(&mem, &cands).await;
        assert!(checker.last_shadow_verdict().is_none(), "failed shadow must not fabricate a verdict");
    }
}