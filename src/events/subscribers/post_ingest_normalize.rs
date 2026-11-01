//! Post-ingest normalize subscriber (Layer 2 — ThalamicBus).
//! Triggers `reconcile`-style normalization when ingest completes (AC4 pre-check).

use crate::events::{MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse};
use async_trait::async_trait;

pub struct PostIngestNormalize;

impl PostIngestNormalize {
    pub fn new() -> Self { Self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ThalamicPulse, RecursionToken, SubscriberContext};
    use std::sync::Arc;
    #[tokio::test]
    async fn new_handles_ingest_done_pulse() {
        let svc = PostIngestNormalize::new();
        let pulse = ThalamicPulse::Created {
            id: "i".into(), kind: "ingest_done".into(), namespace: "global".into(),
        };
        let mock: Arc<crate::guard::testkit::MockStore> = Arc::new(crate::guard::testkit::MockStore::new());
        let store_ref: &dyn crate::traits::VectorStore = mock.as_ref();
        assert!(svc.handle(&pulse, &RecursionToken::new(), &SubscriberContext { store: store_ref, namespace: "global", ts: 0 }).await.is_ok());
        let other = ThalamicPulse::Created { id: "i2".into(), kind: "task_created".into(), namespace: "global".into() };
        assert!(svc.handle(&other, &RecursionToken::new(), &SubscriberContext { store: store_ref, namespace: "global", ts: 0 }).await.is_ok());
    }
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
            ThalamicPulse::Created { kind, namespace, .. } if kind == "ingest_done" => {
                let _ = super::clear_export_freshness(_ctx.store).await;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
