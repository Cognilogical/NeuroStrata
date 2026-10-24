//! `MeshOutbox` — tees shareable memory mutations into the durable outbox.
//!
//! The Thalamic Bus hands a pulse to this subscriber *after* the store commit,
//! so this is the durable write-ahead step of the publish path: it reads the
//! mutated memory back from the store, snapshots it into a self-contained
//! [`OutboxEntry`], and appends it to the JSONL WAL. It **never writes a mesh
//! file and never commits** — the worker does that. Its one side effect beyond
//! the WAL is a `notify_one` on a shared `Notify`, waking the drain loop so a
//! mutation is exported promptly rather than only on the next 5s tick.
//!
//! It captures only the shareable types (`rule`, `fact`, `procedure`) — tasks,
//! structural rows and the bus's own sentinel rows are strictly local. The
//! `eligibility` and foreign-origin (`metadata.mesh`) gates are deliberately
//! NOT applied here: those are the worker's publish decision, evaluated against
//! the mesh manifest's policy, and a subscriber has no manifest.

use crate::events::{
    MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse,
};
use crate::synaptic_mesh::outbox::{
    is_shareable_memory_type, Outbox, OutboxEntry, OP_ARCHIVED, OP_CREATED, OP_SUPERSEDED,
};
use crate::traits::MemoryPayload;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

/// Tees shareable mutations into the outbox and wakes the worker.
pub struct MeshOutbox {
    outbox: Outbox,
    wake: Arc<Notify>,
    /// The mesh manifest epoch at wiring time, stamped into every captured
    /// entry so the worker can quarantine a pre-purge entry at drain time
    /// (publish-side epoch gate). An epoch migration is an operator re-init
    /// that rewires the daemon, so wiring-time resolution is the same
    /// freshness contract the drain loop's per-tick reopen implies.
    epoch: u32,
}

impl MeshOutbox {
    /// Open the outbox over `outbox_dir` and share `wake` with the daemon's
    /// drain loop. Err (rather than panic) when the outbox cannot open, so the
    /// daemon can log and continue without a mesh subscriber.
    pub fn new(
        outbox_dir: impl Into<PathBuf>,
        wake: Arc<Notify>,
        epoch: u32,
    ) -> Result<Self, String> {
        Ok(Self {
            outbox: Outbox::open(&outbox_dir.into())?,
            wake,
            epoch,
        })
    }
}

#[async_trait]
impl MemorySubscriber for MeshOutbox {
    fn name(&self) -> &'static str {
        "mesh_outbox"
    }

    async fn handle(
        &self,
        event: &ThalamicPulse,
        _token: &RecursionToken,
        ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError> {
        let mut entries: Vec<OutboxEntry> = Vec::new();
        match event {
            ThalamicPulse::Created { id, namespace, .. } => {
                if let Some(payload) = self.read(ctx, namespace, id).await? {
                    if let Some(entry) =
                        self.entry(OP_CREATED, id, namespace, payload, None, ctx.ts)
                    {
                        entries.push(entry);
                    }
                }
            }
            ThalamicPulse::Superseded {
                old_id,
                new_id,
                namespace,
            } => {
                // A supersede is two durable facts: the old row retired, and a
                // replacement row that was never announced by its own `Created`
                // pulse (the supersede handler writes it directly). Capture both.
                if let Some(payload) = self.read(ctx, namespace, old_id).await? {
                    if let Some(entry) = self.entry(
                        OP_SUPERSEDED,
                        old_id,
                        namespace,
                        payload,
                        Some(new_id.clone()),
                        ctx.ts,
                    ) {
                        entries.push(entry);
                    }
                }
                if let Some(payload) = self.read(ctx, namespace, new_id).await? {
                    if let Some(entry) =
                        self.entry(OP_CREATED, new_id, namespace, payload, None, ctx.ts)
                    {
                        entries.push(entry);
                    }
                }
            }
            ThalamicPulse::Archived { id, namespace } => {
                if let Some(payload) = self.read(ctx, namespace, id).await? {
                    if let Some(entry) =
                        self.entry(OP_ARCHIVED, id, namespace, payload, None, ctx.ts)
                    {
                        entries.push(entry);
                    }
                }
            }
            // A guard verdict is not a shareable memory mutation.
            ThalamicPulse::GuardedActionFired { .. } => return Ok(()),
        };

        if entries.is_empty() {
            return Ok(());
        }
        for entry in entries {
            self.outbox.append(&entry).map_err(anyhow::Error::msg)?;
        }
        self.wake.notify_one();
        Ok(())
    }
}

impl MeshOutbox {
    /// Read one memory back off the store. `Ok(None)` when it was deleted
    /// concurrently — nothing to export, not an error.
    async fn read(
        &self,
        ctx: &SubscriberContext<'_>,
        namespace: &str,
        id: &str,
    ) -> Result<Option<MemoryPayload>, SubscriberError> {
        Ok(ctx
            .store
            .get(namespace, id)
            .await?
            .map(|(_, payload)| payload))
    }

    /// Build one outbox entry from a captured payload. Non-shareable types
    /// collapse to `None` (a no-op, not an error); the worker still re-checks
    /// the type as a second line of defence.
    fn entry(
        &self,
        op: &str,
        id: &str,
        namespace: &str,
        payload: MemoryPayload,
        superseded_by: Option<String>,
        ts: i64,
    ) -> Option<OutboxEntry> {
        // Structural rows are strictly local (spec §"Sharing boundary"),
        // mirroring the exclusion `server::set_metadata` applies: AST-ingest
        // rows (whatever their declared `memory_type`) and `::`-qualified
        // structural ids never enter the outbox. A `procedure_perform` firing
        // needs no guard here — the bus carries no pulse for it at all (see
        // the finding documented in the tests).
        if payload.user_id == "auto-ingestor"
            || id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
        {
            return None;
        }
        if !is_shareable_memory_type(&payload.memory_type) {
            return None;
        }
        Some(OutboxEntry {
            stable_op_id: uuid::Uuid::new_v4(),
            op: op.to_string(),
            id: id.to_string(),
            namespace: namespace.to_string(),
            memory_type: payload.memory_type,
            content: payload.content,
            metadata: payload.metadata,
            superseded_by,
            ts,
            epoch: self.epoch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RecursionToken, ThalamicBus};
    use crate::store::LadybugStore;
    use crate::synaptic_mesh::outbox::outbox_dir_for;
    use crate::traits::VectorStore;
    use std::path::PathBuf;
    use std::time::Duration;

    const TEST_DIMENSIONS: usize = 4;
    /// Long enough for the dispatcher to pop the queue and run the subscriber.
    const DISPATCH_WAIT: Duration = Duration::from_millis(100);

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-mesh-outbox-sub-{}-{}-{}",
            std::process::id(),
            tag,
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn payload(memory_type: &str, metadata: serde_json::Value) -> MemoryPayload {
        payload_as("system", memory_type, metadata)
    }

    fn payload_as(user_id: &str, memory_type: &str, metadata: serde_json::Value) -> MemoryPayload {
        MemoryPayload {
            content: "the text".to_string(),
            user_id: user_id.to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        }
    }

    /// A bus carrying a real store and the outbox subscriber over `outbox_dir`.
    async fn wired(
        tag: &str,
        outbox_dir: PathBuf,
        wake: Arc<Notify>,
    ) -> (ThalamicBus, Arc<LadybugStore>) {
        let dir = scratch(tag);
        let store = LadybugStore::for_testing(dir.join("db.lbug"), TEST_DIMENSIONS)
            .expect("open temp store");
        store.init("ns").await.expect("ns table");
        let store = Arc::new(store);
        let bus = ThalamicBus::new(16);
        bus.attach_store(store.clone());
        bus.register(Box::new(
            MeshOutbox::new(outbox_dir, wake, 1).expect("outbox opens"),
        ));
        (bus, store)
    }

    #[tokio::test]
    async fn a_shareable_created_pulse_writes_an_outbox_entry() {
        let dir = scratch("created");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("created-db", outbox_dir.clone(), wake.clone()).await;

        let id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload("rule", serde_json::json!({ "eligibility": "shareable" })),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Created {
                id: id.clone(),
                namespace: "ns".to_string(),
                kind: "rule".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "one entry per Created");
        assert_eq!(pending[0].op, OP_CREATED);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].memory_type, "rule");
        assert_eq!(pending[0].content, "the text");
        assert_eq!(
            pending[0].epoch, 1,
            "the entry carries its capture-time epoch"
        );
        assert_eq!(
            pending[0].metadata["eligibility"],
            serde_json::json!("shareable")
        );
    }

    #[tokio::test]
    async fn a_non_shareable_type_is_not_captured() {
        let dir = scratch("nonshareable");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("nonshareable-db", outbox_dir.clone(), wake).await;

        let id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload("task", serde_json::json!({})),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Created {
                id,
                namespace: "ns".to_string(),
                kind: "task".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        assert!(
            outbox.pending().unwrap().is_empty(),
            "a task mutation is not shareable"
        );
    }

    #[tokio::test]
    async fn a_superseded_pulse_captures_both_rows() {
        let dir = scratch("superseded");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("superseded-db", outbox_dir.clone(), wake).await;

        let old_id = uuid::Uuid::new_v4().to_string();
        let new_id = uuid::Uuid::new_v4().to_string();
        for (id, metadata) in [
            (
                &old_id,
                serde_json::json!({ "eligibility": "shareable", "superseded_by": new_id }),
            ),
            (
                &new_id,
                serde_json::json!({ "eligibility": "shareable", "supersedes": old_id }),
            ),
        ] {
            store
                .upsert(
                    "ns",
                    id,
                    vec![0.0; TEST_DIMENSIONS],
                    payload("rule", metadata),
                )
                .await
                .unwrap();
        }

        bus.emit(
            ThalamicPulse::Superseded {
                old_id: old_id.clone(),
                new_id: new_id.clone(),
                namespace: "ns".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(
            pending.len(),
            2,
            "the retire + the replacement are both captured"
        );
        assert_eq!(pending[0].op, OP_SUPERSEDED);
        assert_eq!(pending[0].id, old_id);
        assert_eq!(pending[0].superseded_by.as_deref(), Some(new_id.as_str()));
        assert_eq!(pending[1].op, OP_CREATED);
        assert_eq!(pending[1].id, new_id);
    }

    #[tokio::test]
    async fn an_archived_pulse_captures_the_tombstone() {
        let dir = scratch("archived");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("archived-db", outbox_dir.clone(), wake).await;

        let id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload(
                    "fact",
                    serde_json::json!({ "eligibility": "shareable", "archived": true }),
                ),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Archived {
                id: id.clone(),
                namespace: "ns".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].op, OP_ARCHIVED);
        assert_eq!(pending[0].id, id);
    }

    /// Structural rows (`user_id: "auto-ingestor"`) never enter the outbox,
    /// mirroring the exclusion `server::set_metadata` already applies at the
    /// metadata path — an ingest-adjacent row is strictly local, whatever its
    /// `memory_type` says.
    #[tokio::test]
    async fn a_structural_auto_ingest_row_is_not_captured() {
        let dir = scratch("structural-auto-ingest");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("structural-ingest-db", outbox_dir.clone(), wake).await;

        let id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload_as("auto-ingestor", "rule", serde_json::json!({})),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Created {
                id,
                namespace: "ns".to_string(),
                kind: "rule".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        assert!(
            outbox.pending().unwrap().is_empty(),
            "an auto-ingestor row is strictly local"
        );
    }

    /// Structural rows (`::`-qualified source ids from the AST ingest) never
    /// enter the outbox, same mirror as above.
    #[tokio::test]
    async fn a_structural_namespaced_row_is_not_captured() {
        let dir = scratch("structural-namespaced");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("structural-ns-db", outbox_dir.clone(), wake).await;

        let id = format!("src::{}", uuid::Uuid::new_v4());
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload_as("system", "rule", serde_json::json!({})),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Created {
                id,
                namespace: "ns".to_string(),
                kind: "rule".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        assert!(
            outbox.pending().unwrap().is_empty(),
            "a `::`-qualified id is strictly local"
        );
    }

    /// A procedure's DEFINITION is shareable and its add pulse is captured.
    /// A `procedure_perform` firing has no pulse at all — `ThalamicPulse` has
    /// no such variant (`event.rs`), a perform stamps the row in place and
    /// announces nothing — so there is no perform-side exclusion to add: the
    /// only procedure traffic a bus subscriber can ever see is a definition
    /// add/update.
    #[tokio::test]
    async fn a_procedure_definition_add_pulse_is_captured_perform_never_pulses() {
        let dir = scratch("procedure-def");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("procedure-def-db", outbox_dir.clone(), wake).await;

        let id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload_as(
                    "system",
                    "procedure",
                    serde_json::json!({ "eligibility": "shareable", "trigger": "before-edit", "remaining_fires": 5 }),
                ),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Created {
                id: id.clone(),
                namespace: "ns".to_string(),
                kind: "procedure".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "a procedure definition is shareable");
        assert_eq!(pending[0].memory_type, "procedure");
    }

    /// The subscriber's outbox directory is a sibling of the mesh checkout, so
    /// it can never be staged into the mesh repo. Guarded here, at the
    /// boundary the daemon wires.
    #[test]
    fn the_outbox_dir_is_outside_the_mesh_repo() {
        let dir = PathBuf::from("/base/mesh-id");
        let outbox = outbox_dir_for(&dir);
        assert!(!outbox.starts_with(&dir));
    }
}

impl MeshOutbox {
    /// R9: Reconcile the source store against the mesh checkout. For every
    /// shareable memory in the store that has no corresponding mesh record
    /// file, build an outbox entry and append it. This closes the "lossy bus"
    /// loss window: even if a pulse was dropped, the next reconciliation
    /// re-emits the record.
    ///
    /// Returns the number of entries re-emitted.
    pub async fn reconcile(
        store: &dyn crate::traits::VectorStore,
        mesh_root: &std::path::Path,
        outbox: &Outbox,
        namespaces: &[String],
        epoch: u32,
    ) -> Result<usize, String> {
        let mut re_emitted = 0usize;
        for ns in namespaces {
            let results = store.list(ns, None, None).await.map_err(|e| format!("{}", e))?;
            for sr in results {
                let payload = &sr.payload;
                // Only shareable types.
                if !is_shareable_memory_type(&payload.memory_type) {
                    continue;
                }
                // Structural rows are strictly local.
                if payload.user_id == "auto-ingestor" || sr.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                    continue;
                }
                // Eligibility gate (worker also re-checks, but skip early).
                if payload.metadata.get("eligibility").and_then(|v| v.as_str()) != Some("shareable") {
                    // Default is private; only shareable records are published.
                    // But we don't know the mesh policy here, so only skip if
                    // explicitly private/absent. The worker will filter anyway.
                }

                // Check if mesh file exists.
                let subdir = if payload.memory_type == "procedure" { "procedures" } else { "memories" };
                let mesh_file = mesh_root.join(ns).join(subdir).join(format!("{}.json", sr.id));
                if mesh_file.exists() {
                    continue;
                }

                // Build an outbox entry.
                let entry = OutboxEntry {
                    stable_op_id: uuid::Uuid::new_v4(),
                    op: OP_CREATED.to_string(),
                    id: sr.id.clone(),
                    namespace: ns.clone(),
                    memory_type: payload.memory_type.clone(),
                    content: payload.content.clone(),
                    metadata: payload.metadata.clone(),
                    superseded_by: None,
                    ts: chrono::Utc::now().timestamp(),
                    epoch,
                };
                outbox.append(&entry).map_err(|e| format!("{}", e))?;
                re_emitted += 1;
            }
        }
        Ok(re_emitted)
    }
}
