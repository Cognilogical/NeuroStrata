use anyhow::Result;
use async_trait::async_trait;

use super::{DuplicateJudgment, JudgmentProvider};
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
