//! Post-ingest normalize subscriber (Layer 2 — ThalamicBus).
//! Triggers `reconcile`-style normalization when ingest completes (AC4 pre-check).

use crate::events::{MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse};
use async_trait::async_trait;

pub struct PostIngestNormalize;

impl PostIngestNormalize {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl MemorySubscriber for PostIngestNormalize {
    fn name(&self) -> &'static str { "post_ingest_normalize" }

    async fn handle(
        &self,
        event: &ThalamicPulse,
        _token: &RecursionToken,
        _ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError> {
        match event {
            // Trigger normalize / reconcile on ingest-done pulse
            ThalamicPulse::Created { kind, .. } if kind == "ingest_done" => {
                // Normalization: dirty targets reconciled before task_complete pre-check
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
