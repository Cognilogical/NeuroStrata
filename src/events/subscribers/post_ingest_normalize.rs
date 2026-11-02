//! Post-ingest normalize subscriber (Layer 2 — ThalamicBus).
//! Triggers `reconcile`-style normalization when ingest completes (AC4 pre-check).

// The subscriber is registered by the daemon's thalamic bus at startup: an
// `ingest_done` pulse runs one normalization pass over the namespace.
use async_trait::async_trait;
use crate::events::{MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse};

pub struct PostIngestNormalize;

impl PostIngestNormalize {
    pub fn new() -> Self { Self }
}

/// The relation keys whose string targets are normalized in place.
const NORMALIZABLE_RELATION_KEYS: [&str; 4] =
    ["related_to", "contained_by", "governs", "extracted_from"];

/// Rewrite every string element of the four relation arrays in one memory's
/// metadata. Returns true when anything changed; the metadata object is
/// mutated in place so the caller only needs to persist it.
fn normalize_payload_metadata(
    known: &crate::store::ladybug::KnownIds<'_>,
    metadata: &mut serde_json::Value,
) -> bool {
    use serde_json::Value;

    let mut changed = false;
    if !metadata.is_object() {
        return false;
    }
    for key in NORMALIZABLE_RELATION_KEYS {
        let Some(Value::Array(arr)) = metadata.get_mut(key) else { continue };
        for item in arr.iter_mut() {
            let Value::String(s) = item else { continue };
            if known.contains(s.as_str()) {
                continue;
            }
            if let Some(resolved) = known.resolve(s.as_str()) {
                if resolved != *s {
                    *item = Value::String(resolved);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// AC4 normalization pass over one namespace: every resolvable declared target
/// is rewritten to the exact id the store holds (KnownIds::resolve), so the
/// graph's edge-canonicalization converges without an agent remembering to do
/// it. Returns (memories scanned, memories rewritten).
pub async fn normalize_namespace_targets(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
) -> Result<(usize, usize), String> {
    let all = store
        .list(namespace, None, None)
        .await
        .map_err(|e| e.to_string())?;
    let known = crate::store::ladybug::KnownIds::new(
        all.iter()
            .filter(|m| m.payload.user_id == "auto-ingestor")
            .map(|m| m.id.as_str()),
    );
    let mut updated = 0usize;

    for mem in &all {
        // Ingest-derived and namespace-qualified rows are the target side of
        // the graph (they are what resolution points AT, not declarations).
        if mem.payload.user_id == "auto-ingestor"
            || mem.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
        {
            continue;
        }
        let mut payload = mem.payload.clone();
        let changed = normalize_payload_metadata(&known, &mut payload.metadata);
        if !changed {
            continue;
        }
        let (vector, mut fresh) = store
            .get(namespace, &mem.id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("normalize: row {} vanished mid-pass", mem.id))?;
        fresh.metadata = payload.metadata;
        store
            .upsert(namespace, &mem.id, vector, fresh)
            .await
            .map_err(|e| e.to_string())?;
        updated += 1;
    }

    Ok((all.len(), updated))
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

    #[test]
    fn normalize_payload_metadata_rewrites_and_flags_change() {
        let known = crate::store::ladybug::KnownIds::new(["NeuroStrata::src/lib.rs"]);
        let mut meta = serde_json::json!({
            "related_to": ["src/lib.rs", "NeuroStrata::src/lib.rs", "docs/absent.md"],
            "governs": [],
            "topology": {"component": "x"}
        });
        assert!(normalize_payload_metadata(&known, &mut meta));
        assert_eq!(meta["related_to"][0], "NeuroStrata::src/lib.rs");
        assert_eq!(meta["related_to"][1], "NeuroStrata::src/lib.rs");
        assert_eq!(meta["related_to"][2], "docs/absent.md");
        // Second pass: already normalized, so nothing changes.
        assert!(!normalize_payload_metadata(&known, &mut meta));
        assert_eq!(meta, serde_json::json!({
            "related_to": ["NeuroStrata::src/lib.rs", "NeuroStrata::src/lib.rs", "docs/absent.md"],
            "governs": [],
            "topology": {"component": "x"}
        }));
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
                let _ = normalize_namespace_targets(_ctx.store, namespace).await;
                let _ = super::clear_export_freshness(_ctx.store).await;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}