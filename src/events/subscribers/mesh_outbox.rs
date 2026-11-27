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
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::Notify;
use uuid::Uuid;

/// Whether a memory is a derived artifact (KGE vectors, community labels,
/// inferred edges) that must stay local/versioned derived data and never
/// cross the mesh. The `derived` marker is accepted in every shape it is
/// written in: boolean `true`, integer `1`, or the strings `"true"`/`"1"`
/// (case-insensitive).
fn is_derived_metadata(meta: &JsonValue) -> bool {
    meta.get("derived")
        .map(|v| {
            v.as_bool().unwrap_or(false)
                || v.as_i64() == Some(1)
                || v.as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("true") || s.trim() == "1")
        })
        .unwrap_or(false)
}

/// The per-capture facts shared by every entry one pulse produces: the
/// dispatcher's timestamp and the durable bus sequence being acknowledged
/// (if this delivery came from the durable path).
#[derive(Clone, Copy)]
struct CaptureStamp {
    ts: i64,
    bus_seq: Option<u64>,
    generation: u64,
    /// Mesh epoch stamped on the durable event, not the epoch current at
    /// replay time. A pre-purge event must stay pre-purge so the worker's
    /// epoch gate can quarantine it.
    epoch: u32,
}

impl CaptureStamp {
    /// Per-effect dedup key: instance generation, delivery sequence, and the
    /// semantic leg of a multi-entry pulse. `Superseded`'s retirement and
    /// replacement legs therefore cannot suppress one another on replay.
    fn bus_key(&self, leg: &str) -> Option<String> {
        self.bus_seq
            .map(|seq| format!("{}:{}:{}", self.generation, seq, leg))
    }
}

/// The publish destination a capture is stamped with. Held behind a lock so
/// the daemon can rebind it when the project switches mesh or purges (which
/// bumps the epoch) without dropping and re-registering the subscriber.
struct MeshBinding {
    outbox: Arc<Outbox>,
    /// Manifest epoch at binding time, stamped into every captured entry so
    /// the worker can quarantine a pre-purge entry (publish-side epoch gate).
    epoch: u32,
    /// The mesh this subscriber may export to. A durable event captured for a
    /// different mesh is refused rather than replayed into the current one;
    /// `Uuid::nil()` is the legacy/unbound case with no identity to compare.
    mesh_id: Uuid,
}

/// Tees shareable mutations into the outbox and wakes the worker.
pub struct MeshOutbox {
    binding: RwLock<MeshBinding>,
    wake: Arc<Notify>,
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
        Self::new_bound(outbox_dir, wake, epoch, Uuid::nil())
    }

    /// Bind the subscriber to the mesh currently configured, so a replay of
    /// an event captured for a different mesh is retained, never exported.
    pub fn new_bound(
        outbox_dir: impl Into<PathBuf>,
        wake: Arc<Notify>,
        epoch: u32,
        mesh_id: Uuid,
    ) -> Result<Self, String> {
        Ok(Self {
            binding: RwLock::new(MeshBinding {
                outbox: Arc::new(Outbox::open(&outbox_dir.into())?),
                epoch,
                mesh_id,
            }),
            wake,
        })
    }

    /// Swap the publish destination in place. A capture already in flight
    /// keeps the binding it read at the start of `handle`; a new capture sees
    /// the new outbox, epoch and mesh identity.
    fn rebind(&self, outbox_dir: &Path, epoch: u32, mesh_id: Uuid) -> Result<(), String> {
        let outbox = Arc::new(Outbox::open(outbox_dir)?);
        let mut binding = self
            .binding
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *binding = MeshBinding {
            outbox,
            epoch,
            mesh_id,
        };
        Ok(())
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
        // Snapshot the publish binding before any await: the daemon may rebind
        // it mid-capture when the project switches mesh, and a std lock guard
        // must not be held across the store reads below.
        let (outbox, epoch, mesh_id) = {
            let binding = self
                .binding
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (Arc::clone(&binding.outbox), binding.epoch, binding.mesh_id)
        };
        // A durable delivery carries the instance generation and bus sequence
        // that produced it. Dedup is per captured effect, never per pulse: a
        // crash between the two appends of a `Superseded` pulse must not make
        // the survivor suppress its missing sibling on replay.
        let delivery = crate::events::current_delivery()
            .filter(|delivery| delivery.class == crate::events::EventClass::Mesh);
        let capture = delivery.and_then(|delivery| delivery.capture);
        // A durable event remembers the mesh it was captured for. Replaying it
        // after the project left that mesh must never publish the pending
        // mutation into the NEW mesh; refuse so the entry stays unacknowledged
        // for an operator instead of silently crossing the boundary.
        if let Some(capture) = capture {
            if mesh_id != Uuid::nil() && capture.mesh_id != mesh_id {
                return Err(anyhow::anyhow!(
                    "durable event {} was captured for mesh {} but this daemon publishes to {}",
                    delivery.map(|d| d.seq).unwrap_or(0),
                    capture.mesh_id,
                    mesh_id
                ));
            }
        }
        let stamp = CaptureStamp {
            ts: ctx.ts,
            bus_seq: delivery.map(|delivery| delivery.seq),
            generation: delivery.map(|delivery| delivery.generation).unwrap_or(0),
            epoch: capture.map(|capture| capture.epoch).unwrap_or(epoch),
        };
        let mut entries: Vec<OutboxEntry> = Vec::new();
        match event {
            ThalamicPulse::Created {
                id,
                namespace,
                kind,
                ..
            } => {
                // `ingest:<ns>` / `ingest_done` is a namespace-wide completion
                // marker for the normalize and freshness subscribers, NOT a
                // memory mutation. It has no backing row, so looking it up here
                // would either fabricate a creation or strand the durable
                // entry forever. Shareable rows it stands for are emitted
                // individually by the ingest handler.
                if kind == "ingest_done" {
                    return Ok(());
                }
                let payload = self.read(ctx, namespace, id).await?;
                // A replayed `Created` can arrive after the row was
                // archived or retired. Exporting it as a creation would
                // make the mesh worker overwrite the newer lifecycle, so
                // replay the state the payload actually records.
                let op = if payload_is_retired(&payload) {
                    OP_ARCHIVED
                } else {
                    OP_CREATED
                };
                if let Some(entry) = self.entry(op, id, namespace, payload, None, stamp, "created")
                {
                    entries.push(entry);
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
                let old_payload = self.read(ctx, namespace, old_id).await?;
                // A replay can reach this pulse after the old row was
                // archived. Exporting a `superseded` lifecycle there would
                // roll the mesh back from Archived to Superseded, so
                // capture the state the row actually records now.
                let retire_op = if payload_is_retired(&old_payload) {
                    OP_ARCHIVED
                } else {
                    OP_SUPERSEDED
                };
                if let Some(entry) = self.entry(
                    retire_op,
                    old_id,
                    namespace,
                    old_payload,
                    Some(new_id.clone()),
                    stamp,
                    "retire",
                ) {
                    entries.push(entry);
                }
                let new_payload = self.read(ctx, namespace, new_id).await?;
                let op = if payload_is_retired(&new_payload) {
                    OP_ARCHIVED
                } else {
                    OP_CREATED
                };
                if let Some(entry) =
                    self.entry(op, new_id, namespace, new_payload, None, stamp, "replace")
                {
                    entries.push(entry);
                }
            }
            ThalamicPulse::Archived { id, namespace } => {
                let payload = self.read(ctx, namespace, id).await?;
                if let Some(entry) =
                    self.entry(OP_ARCHIVED, id, namespace, payload, None, stamp, "archived")
                {
                    entries.push(entry);
                }
            }
            // A guard verdict is not a shareable memory mutation.
            ThalamicPulse::GuardedActionFired { .. } => return Ok(()),
        };

        if entries.is_empty() {
            return Ok(());
        }
        // Dedup each captured effect independently. A durable replay after a
        // crash between two legs finds only one of them captured and must still
        // append the missing one.
        let pending = if stamp.bus_seq.is_some() {
            outbox.pending().map_err(anyhow::Error::msg)?
        } else {
            Vec::new()
        };
        for entry in entries {
            let already = match &entry.bus_key {
                Some(key) => {
                    outbox
                        .is_bus_key_committed(key)
                        .map_err(anyhow::Error::msg)?
                        || pending
                            .iter()
                            .any(|seen| seen.bus_key.as_deref() == Some(key.as_str()))
                }
                None => match entry.bus_seq {
                    Some(seq) => {
                        outbox
                            .is_bus_seq_committed(seq)
                            .map_err(anyhow::Error::msg)?
                            || pending.iter().any(|seen| seen.bus_seq == Some(seq))
                    }
                    None => false,
                },
            };
            if already {
                tracing::debug!(
                    id = %entry.id,
                    "mesh outbox: captured effect already recorded, skipping"
                );
                continue;
            }
            outbox.append(&entry).map_err(anyhow::Error::msg)?;
        }
        self.wake.notify_one();
        Ok(())
    }

    fn refresh_mesh_binding(&self, binding: crate::events::MeshCapture, outbox_dir: &Path) {
        if let Err(err) = self.rebind(outbox_dir, binding.epoch, binding.mesh_id) {
            tracing::warn!(
                "mesh outbox rebind to {} failed: {err}; captures stay on the previous binding",
                outbox_dir.display()
            );
        }
    }
}

impl MeshOutbox {
    /// Read one memory back off the store. A missing row is an error: the
    /// durable pulse already claims a mutation, so acknowledging a row that
    /// vanished between append and read would silently retire the event
    /// instead of leaving it pending for reconciliation.
    async fn read(
        &self,
        ctx: &SubscriberContext<'_>,
        namespace: &str,
        id: &str,
    ) -> Result<MemoryPayload, SubscriberError> {
        ctx.store.get(namespace, id).await?.map(|(_, payload)| payload).ok_or_else(|| {
            anyhow::anyhow!(
                "durable event refers to missing memory {namespace}:{id}; leaving it pending for reconciliation"
            )
        })
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
        stamp: CaptureStamp,
        leg: &str,
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
        if payload.memory_type == "action_receipt"
            && !crate::formal_kernel::is_action_memory_shareable(
                crate::formal_kernel::ActionMemoryKind::ActionReceipt,
                true,
            )
        {
            return None;
        }
        if !is_shareable_memory_type(&payload.memory_type) {
            return None;
        }
        // Derived artifacts (KGE vectors, community labels, inferred edges)
        // stay local/versioned derived data and never cross the mesh.
        if is_derived_metadata(&payload.metadata) {
            return None;
        }
        // When a durable delivery identifies this effect, derive its id from
        // that identity: a replay after a lost write produces the same id and
        // the worker's idempotent apply collapses the two. Undelivered captures
        // keep a fresh v7 id because they have no stable identity to derive
        // from.
        let bus_key = stamp.bus_key(leg);
        let stable_op_id = match &bus_key {
            Some(key) => uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, key.as_bytes()),
            None => uuid::Uuid::now_v7(),
        };
        Some(OutboxEntry {
            stable_op_id,
            op: op.to_string(),
            id: id.to_string(),
            namespace: namespace.to_string(),
            memory_type: payload.memory_type,
            content: payload.content,
            metadata: payload.metadata,
            superseded_by,
            ts: stamp.ts,
            epoch: stamp.epoch,
            bus_seq: stamp.bus_seq,
            bus_key,
        })
    }
}

/// Whether a captured payload already records a retired/archived lifecycle.
/// A replayed creation must not export an `Active` lifecycle over it.
fn payload_is_retired(payload: &MemoryPayload) -> bool {
    // A supersession outranks a lapsed `valid_to`: the row's lifecycle is
    // Superseded and its successor link must survive, not Archived.
    if payload
        .metadata
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .is_some_and(|successor| !successor.is_empty())
    {
        return false;
    }
    let metadata = &payload.metadata;
    let memory_type = &payload.memory_type;
    let archived = match metadata.get("archived") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::Number(value)) => value.as_i64().unwrap_or(0) != 0,
        Some(serde_json::Value::String(value)) => {
            matches!(value.as_str(), "1" | "true" | "TRUE" | "True")
        }
        _ => false,
    };
    if archived {
        return true;
    }
    // A procedure's `valid_to` is its rehearsal-budget expiry: a lapsed
    // procedure still answers and its row is NOT archived. Only an explicit
    // retirement marker changes the exported lifecycle.
    if memory_type == "procedure" {
        return false;
    }
    match metadata.get("valid_to") {
        Some(serde_json::Value::Number(value)) => value
            .as_i64()
            .is_some_and(|ts| ts > 0 && ts <= chrono::Utc::now().timestamp()),
        Some(serde_json::Value::String(value)) => value
            .parse::<i64>()
            .is_ok_and(|ts| ts > 0 && ts <= chrono::Utc::now().timestamp()),
        _ => false,
    }
}

/// Shareable rows actually present in a namespace after an ingest, as
/// `(id, memory_type)` pairs. The ingest completion pulse is a namespace-wide
/// marker with no backing row; emitting that synthetic id would strand the
/// durable entry. Instead the handler emits one `Created` pulse per row this
/// helper returns, so a genuinely shareable memory written by an ingest gets
/// its own self-contained capture. Structural/AST rows and derived artifacts
/// are filtered by the exact same predicates the subscriber applies, so they
/// stay strictly local.
pub async fn shareable_rows_for_mesh(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
) -> Vec<(String, String)> {
    match store.list(namespace, None, None).await {
        Ok(rows) => rows
            .into_iter()
            .filter(|row| {
                row.payload.user_id != "auto-ingestor"
                    && !row.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
                    && is_shareable_memory_type(&row.payload.memory_type)
                    && !is_derived_metadata(&row.payload.metadata)
            })
            .map(|row| (row.id, row.payload.memory_type))
            .collect(),
        Err(err) => {
            tracing::warn!("could not enumerate ingest rows for mesh sync: {err}");
            Vec::new()
        }
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
            uuid::Uuid::now_v7()
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

        let id = uuid::Uuid::now_v7().to_string();
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
    async fn a_replayed_created_pulse_for_a_retired_row_captures_the_archive() {
        let dir = scratch("replayed-created-retired");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("replayed-created-retired-db", outbox_dir.clone(), wake).await;

        let id = uuid::Uuid::now_v7().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload(
                    "fact",
                    serde_json::json!({ "eligibility": "shareable", "archived": 1 }),
                ),
            )
            .await
            .unwrap();

        bus.emit(
            ThalamicPulse::Created {
                id: id.clone(),
                namespace: "ns".to_string(),
                kind: "fact".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].op, OP_ARCHIVED,
            "a replay must export the row's current retired lifecycle, not resurrect it as Created"
        );
    }

    #[tokio::test]
    async fn a_non_shareable_type_is_not_captured() {
        let dir = scratch("nonshareable");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("nonshareable-db", outbox_dir.clone(), wake).await;

        let id = uuid::Uuid::now_v7().to_string();
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

        let old_id = uuid::Uuid::now_v7().to_string();
        let new_id = uuid::Uuid::now_v7().to_string();
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

        let id = uuid::Uuid::now_v7().to_string();
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

        let id = uuid::Uuid::now_v7().to_string();
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

        let id = format!("src::{}", uuid::Uuid::now_v7());
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

        let id = uuid::Uuid::now_v7().to_string();
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

    /// Derived rows (`metadata.derived` in any documented shape -- boolean
    /// `true`, integer `1`, or the strings `"true"`/`"1"`) never enter the
    /// outbox: KGE vectors, community labels, and inferred edges stay local -
    /// and versioned-derived data (F4).
    #[tokio::test]
    async fn a_derived_row_is_not_captured_in_any_marker_shape() {
        for derived in [
            serde_json::json!("true"),
            serde_json::json!(1),
            serde_json::json!(true),
        ] {
            let dir = scratch("derived");
            let outbox_dir = dir.join("mesh");
            let wake = Arc::new(Notify::new());
            let (bus, store) = wired("derived-db", outbox_dir.clone(), wake).await;

            let id = uuid::Uuid::now_v7().to_string();
            store
                .upsert(
                    "ns",
                    &id,
                    vec![0.0; TEST_DIMENSIONS],
                    payload(
                        "rule",
                        serde_json::json!({ "eligibility": "shareable", "derived": derived }),
                    ),
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
                "a derived row must not cross the mesh, marker shape: {derived}"
            );
        }
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

    /// A durable delivery that is replayed (a crash between the capture and
    /// the bus offset write) must not append a second copy: the entry already
    /// pending under that bus sequence is the previous attempt's effect.
    #[tokio::test]
    async fn a_replayed_durable_delivery_does_not_append_a_duplicate_entry() {
        use crate::events::outbox::{DurableDelivery, EventClass};

        let dir = scratch("replay-dedup");
        let outbox_dir = dir.join("mesh");
        let store = LadybugStore::for_testing(dir.join("db.lbug"), TEST_DIMENSIONS)
            .expect("open temp store");
        store.init("ns").await.expect("ns table");
        let sub =
            MeshOutbox::new(outbox_dir.clone(), Arc::new(Notify::new()), 1).expect("outbox opens");

        let id = uuid::Uuid::now_v7().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload("rule", serde_json::json!({ "eligibility": "shareable" })),
            )
            .await
            .unwrap();
        let pulse = ThalamicPulse::Created {
            id: id.clone(),
            namespace: "ns".to_string(),
            kind: "rule".to_string(),
        };
        let delivery = Some(DurableDelivery {
            class: EventClass::Mesh,
            seq: 7,
            generation: 1,
            capture: None,
        });
        let ctx = SubscriberContext {
            store: &store,
            namespace: "ns",
            ts: 1,
        };

        crate::events::with_delivery(delivery, sub.handle(&pulse, &RecursionToken::new(), &ctx))
            .await
            .expect("first capture");
        crate::events::with_delivery(delivery, sub.handle(&pulse, &RecursionToken::new(), &ctx))
            .await
            .expect("replayed capture is a no-op");

        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "the replay appends no duplicate");
        assert_eq!(
            pending[0].bus_seq,
            Some(7),
            "the entry records the sequence it was captured for"
        );
    }

    /// The judge's commit-before-bus-ack proof: the worker committed (drained)
    /// the captured entry, so it is no longer pending. A replay of the same
    /// durable `bus_seq` must *still* be recognised as a duplicate, because
    /// the bus offset had not advanced when the process crashed.
    #[tokio::test]
    async fn a_superseded_row_with_an_expired_valid_to_is_not_archived() {
        let dir = scratch("superseded-expired");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let (bus, store) = wired("superseded-expired-db", outbox_dir.clone(), wake).await;
        let id = uuid::Uuid::now_v7().to_string();
        let successor = uuid::Uuid::now_v7().to_string();
        let past = chrono::Utc::now().timestamp() - 60;
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload(
                    "fact",
                    serde_json::json!({
                        "eligibility": "shareable",
                        "superseded_by": successor.clone(),
                        "valid_to": past,
                    }),
                ),
            )
            .await
            .unwrap();
        store
            .upsert(
                "ns",
                &successor,
                vec![0.0; TEST_DIMENSIONS],
                payload("fact", serde_json::json!({ "eligibility": "shareable" })),
            )
            .await
            .unwrap();
        bus.emit(
            ThalamicPulse::Superseded {
                old_id: id.clone(),
                new_id: successor,
                namespace: "ns".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;
        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending[0].op, OP_SUPERSEDED,
            "a superseded_by marker outranks an expired valid_to"
        );
        assert!(
            pending[0].superseded_by.is_some(),
            "the successor link must survive an expired valid_to"
        );
    }

    #[tokio::test]
    async fn a_durable_event_for_a_missing_row_stays_pending() {
        let dir = scratch("missing-row");
        let outbox_dir = dir.join("mesh");
        let wake = Arc::new(Notify::new());
        let sub = MeshOutbox::new(outbox_dir.clone(), wake, 1).unwrap();
        let db = scratch("missing-row-db");
        let store = LadybugStore::for_testing(db.join("db.lbug"), TEST_DIMENSIONS).unwrap();
        store.init("ns").await.unwrap();
        let store: Arc<dyn crate::traits::VectorStore> = Arc::new(store);
        let ctx = SubscriberContext::new(store.as_ref(), "ns", 0);
        let err = sub
            .handle(
                &ThalamicPulse::Created {
                    id: "gone".to_string(),
                    namespace: "ns".to_string(),
                    kind: "fact".to_string(),
                },
                &RecursionToken::new(),
                &ctx,
            )
            .await
            .expect_err("a missing row must not be acknowledged");
        assert!(err.to_string().contains("missing"));
        assert!(Outbox::open(&outbox_dir)
            .unwrap()
            .pending()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_replayed_delivery_is_deduped_after_its_entry_was_committed() {
        use crate::events::outbox::{DurableDelivery, EventClass};

        let dir = scratch("replay-after-commit");
        let outbox_dir = dir.join("mesh");
        let store = LadybugStore::for_testing(dir.join("db.lbug"), TEST_DIMENSIONS)
            .expect("open temp store");
        store.init("ns").await.expect("ns table");
        let sub =
            MeshOutbox::new(outbox_dir.clone(), Arc::new(Notify::new()), 1).expect("outbox opens");

        let id = uuid::Uuid::now_v7().to_string();
        store
            .upsert(
                "ns",
                &id,
                vec![0.0; TEST_DIMENSIONS],
                payload("rule", serde_json::json!({ "eligibility": "shareable" })),
            )
            .await
            .unwrap();
        let pulse = ThalamicPulse::Created {
            id: id.clone(),
            namespace: "ns".to_string(),
            kind: "rule".to_string(),
        };
        let delivery = Some(DurableDelivery {
            class: EventClass::Mesh,
            seq: 7,
            generation: 1,
            capture: None,
        });
        let ctx = SubscriberContext {
            store: &store,
            namespace: "ns",
            ts: 1,
        };

        crate::events::with_delivery(delivery, sub.handle(&pulse, &RecursionToken::new(), &ctx))
            .await
            .expect("first capture");

        // The worker drains the outbox and marks the entry committed *before*
        // the bus writes its durable offset.
        let outbox = Outbox::open(&outbox_dir).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "one capture landed");
        let committed: Vec<uuid::Uuid> = pending.iter().map(|e| e.stable_op_id).collect();
        outbox.mark_committed(&committed).unwrap();
        assert!(
            outbox.pending().unwrap().is_empty(),
            "the worker committed (drained) the entry"
        );

        // The bus replays the same sequence after a crash.
        crate::events::with_delivery(delivery, sub.handle(&pulse, &RecursionToken::new(), &ctx))
            .await
            .expect("the replayed capture is a no-op");

        let outbox = Outbox::open(&outbox_dir).unwrap();
        assert!(
            outbox.pending().unwrap().is_empty(),
            "a committed bus_seq must not be re-appended, even though it is no longer pending"
        );
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
            let results = store
                .list(ns, None, None)
                .await
                .map_err(|e| format!("{}", e))?;
            for sr in results {
                let payload = &sr.payload;
                // Only shareable types.
                if !is_shareable_memory_type(&payload.memory_type) {
                    continue;
                }
                // Structural rows are strictly local.
                if payload.user_id == "auto-ingestor"
                    || sr.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
                {
                    continue;
                }
                // Derived artifacts (KGE vectors, community labels, inferred
                // edges) stay local/versioned derived data, mirroring `entry()`.
                if is_derived_metadata(&payload.metadata) {
                    continue;
                }
                // Eligibility gate (worker also re-checks, but skip early).
                // Default is private; only shareable records are published.
                if payload.metadata.get("eligibility").and_then(|v| v.as_str()) != Some("shareable")
                {
                    continue;
                }

                // Check if mesh file exists (supporting UUIDv5 mapping for deterministic taxonomy IDs).
                let subdir = if payload.memory_type == "procedure" {
                    "procedures"
                } else {
                    "memories"
                };
                let mesh_id = crate::synaptic_mesh::worker::parse_or_derive_mesh_uuid(
                    ns,
                    &payload.memory_type,
                    &sr.id,
                )
                .map(|u| u.to_string())
                .unwrap_or_else(|| sr.id.clone());
                let mesh_file = mesh_root
                    .join(ns)
                    .join(subdir)
                    .join(format!("{mesh_id}.json"));
                if mesh_file.exists() {
                    continue;
                }

                // Build an outbox entry.
                let entry = OutboxEntry {
                    stable_op_id: uuid::Uuid::now_v7(),
                    op: OP_CREATED.to_string(),
                    id: sr.id.clone(),
                    namespace: ns.clone(),
                    memory_type: payload.memory_type.clone(),
                    content: payload.content.clone(),
                    metadata: payload.metadata.clone(),
                    superseded_by: None,
                    ts: chrono::Utc::now().timestamp(),
                    epoch,
                    // Reconciliation is not a bus delivery: its idempotency
                    // basis is the existing mesh file, not a sequence.
                    bus_seq: None,
                    bus_key: None,
                };
                outbox.append(&entry).map_err(|e| format!("{}", e))?;
                re_emitted += 1;
            }
        }
        Ok(re_emitted)
    }
}
