//! Post-ingest normalize subscriber (Layer 2 — ThalamicBus).
//!
//! On an `ingest_done` pulse it runs one AC4 normalization pass over the
//! pulse's namespace: every declared relation target in `related_to`,
//! `contained_by`, `governs`, and `extracted_from` that can be resolved to an
//! id the store holds is rewritten to that exact id in place, so graph edge
//! canonicalization converges without an agent remembering to do it. The
//! pass itself is [`normalize_namespace_targets`].
//!
//! The same pulse then clears the ingested namespace's export-freshness
//! `dirty` flag via
//! [`super::export_freshness_dirty::clear_export_freshness_for_namespace`]:
//! a normalization pass unions the affected rows as they already stand,
//! commits them, so the committed export is not behind the store an ingest
//! just touched. Only that namespace's flag is cleared — other namespaces
//! may still hold un-exported mutations and must not pass the gate.
//!
//! Both passes propagate their errors to the dispatcher — a failed
//! normalization or flag clear is recorded as a subscriber failure on the
//! bus metrics, never silently swallowed.

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
    let known = crate::store::ladybug::KnownIds::new(all.iter().map(|m| m.id.as_str()));
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
    use crate::events::{ThalamicPulse, RecursionToken, SubscriberContext, ExportFreshnessDirty};
    use crate::traits::MemoryPayload;
    use std::sync::Arc;

    fn payload(user_id: &str, metadata: serde_json::Value) -> MemoryPayload {
        MemoryPayload {
            content: "test row".into(),
            user_id: user_id.into(),
            memory_type: "task".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        }
    }

    /// The flag row the freshness subscriber writes, found by type so the
    /// assertion does not have to name the sentinel's private id format.
    async fn freshness_flag(store_ref: &dyn crate::traits::VectorStore) -> Option<MemoryPayload> {
        store_ref
            .list("global", None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.payload.memory_type == "freshness_flag")
            .map(|r| r.payload)
    }

    #[tokio::test]
    async fn new_handles_ingest_done_pulse() {
        let svc = PostIngestNormalize::new();
        let pulse = ThalamicPulse::Created {
            id: "i".into(), kind: "ingest_done".into(), namespace: "global".into(),
        };
        let mock: Arc<crate::guard::testkit::MockStore> = Arc::new(crate::guard::testkit::MockStore::new());
        let store_ref: &dyn crate::traits::VectorStore = mock.as_ref();
        let ctx = SubscriberContext { store: store_ref, namespace: "global", ts: 0 };
        let token = RecursionToken::new();

        // The ingest left one target row (auto-ingestor) and one task whose
        // declared relation target is the unqualified form of it.
        store_ref
            .upsert(
                "global",
                "NeuroStrata::src/lib.rs",
                vec![0.0; 4],
                payload("auto-ingestor", serde_json::json!({})),
            )
            .await
            .unwrap();
        store_ref
            .upsert(
                "global",
                "task-doc",
                vec![0.0; 4],
                payload(
                    "agent",
                    serde_json::json!({ "related_to": ["src/lib.rs"] }),
                ),
            )
            .await
            .unwrap();

        // And a genuinely dirty export-freshness flag, written the way the
        // production writer writes it.
        ExportFreshnessDirty::new(4)
            .handle(
                &ThalamicPulse::Created {
                    id: "task-doc".into(),
                    kind: "task".into(),
                    namespace: "global".into(),
                },
                &token,
                &ctx,
            )
            .await
            .unwrap();
        let before = freshness_flag(store_ref).await.expect("the flag was written");
        assert_eq!(before.metadata["dirty"], serde_json::json!(true),
            "precondition: the flag is dirty before the ingest-done pulse");

        svc.handle(&pulse, &token, &ctx).await.unwrap();

        // The declared target was rewritten to the exact id the store holds.
        let meta = mock.metadata_of("global", "task-doc").expect("row kept");
        assert_eq!(
            meta["related_to"][0], "NeuroStrata::src/lib.rs",
            "the ingest_done pulse normalized the declared target: {meta}"
        );

        // The freshness flag was cleared, so export-graph reports fresh.
        let after = freshness_flag(store_ref).await.expect("the flag row survives");
        assert_eq!(
            after.metadata["dirty"],
            serde_json::json!(false),
            "the ingest_done pulse cleared the export-freshness flag"
        );

        let other = ThalamicPulse::Created { id: "i2".into(), kind: "task_created".into(), namespace: "global".into() };
        svc.handle(&other, &RecursionToken::new(), &ctx).await.unwrap();
        // A non-ingest pulse is a no-op: the already-normalized row did not
        // need (and did not get) a second pass.
        let meta = mock.metadata_of("global", "task-doc").unwrap();
        assert_eq!(
            meta["related_to"][0], "NeuroStrata::src/lib.rs",
            "no pulse but ingest_done normalizes: {meta}"
        );
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
        ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError> {
        match event {
            // Trigger normalize / reconcile on ingest-done pulse. Errors are
            // propagated, not discarded: a normalize or flag-clear failure is
            // a subscriber error the bus counts, and callers see it. Only the
            // ingested namespace's flag is cleared — other namespaces may
            // still hold un-exported mutations and must not pass the gate.
            ThalamicPulse::Created { kind, namespace, .. } if kind == "ingest_done" => {
                normalize_namespace_targets(ctx.store, namespace)
                    .await
                    .map_err(|e| anyhow::anyhow!("post_ingest_normalize failed: {e}"))?;
                super::export_freshness_dirty::clear_export_freshness_for_namespace(
                    ctx.store,
                    namespace,
                )
                .await
                .map_err(|e| anyhow::anyhow!("clear_export_freshness failed: {e:#}"))?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}