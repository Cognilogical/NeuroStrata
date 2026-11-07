//! `MeshReactor` — per-stratum inbound/outbound frame coordinator, conflict
//! preservation, Lock-2 task completion enforcement, and anti-entropy catch-up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use openssl::pkey::{PKey, Private};

use crate::synaptic_mesh::attention::AttentionFilter;
use crate::synaptic_mesh::conflict::{self, ConflictKind};
use crate::synaptic_mesh::frame::{
    strip_procedure_runtime_counters, validate_egress_candidate, validate_ingress_frame,
    MeshFrame, MeshPayloadKind, SovereigntyGateError, SyncHello, TaskMeshAction,
};
use crate::synaptic_mesh::outbox::Outbox;
use crate::synaptic_mesh::record::MeshRecord;
use crate::synaptic_mesh::transport::{LeaseVerdict, MeshTransport};

/// Outcome of applying a verified inbound [`MeshFrame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactorApplyOutcome {
    Applied,
    NoOpDuplicate,
    BufferedOutOfOrder { expected_seq: u64, got_seq: u64 },
    ConflictPreserved { id: String },
    LeaseArbitrated(LeaseVerdict),
}

/// Errors returned when publishing or applying a [`MeshFrame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactorApplyError {
    Sovereignty(SovereigntyGateError),
    PrivateCollision(String),
    Lock2Violation(String),
    Storage(String),
}

impl std::fmt::Display for ReactorApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sovereignty(e) => write!(f, "sovereignty gate violation: {e}"),
            Self::PrivateCollision(id) => {
                write!(f, "refusing mesh import over sovereign private row {id}")
            }
            Self::Lock2Violation(task_id) => {
                write!(
                    f,
                    "Lock 2 violation: remote task `{task_id}` completed with zero extracted memories"
                )
            }
            Self::Storage(e) => write!(f, "reactor storage error: {e}"),
        }
    }
}

impl std::error::Error for ReactorApplyError {}

#[derive(Debug, Clone)]
struct TwinVarCell {
    value: f64,
    lamport_ts: u64,
    origin_node_id: String,
}

#[derive(Debug, Clone, Default)]
struct TwinMirrorState {
    version: u64,
    vars: BTreeMap<String, TwinVarCell>,
}

const MAX_REORDER_BUFFER_PER_PEER: usize = 64;

/// Coordinates outbound frame signing/publishing and inbound per-stratum state merges.
pub struct MeshReactor {
    mesh_id: String,
    epoch: u64,
    local_did: String,
    privkey: PKey<Private>,
    outbox: Arc<Outbox>,
    transport: Arc<dyn MeshTransport>,
    attention: Arc<AttentionFilter>,
    conflicts_dir: PathBuf,
    next_seq: AtomicU64,
    lamport_clock: AtomicU64,
    seen_frames: Mutex<BTreeSet<(String, u64)>>,
    last_contiguous_seq: Mutex<BTreeMap<String, u64>>,
    reorder_buffer: Mutex<BTreeMap<String, BTreeMap<u64, MeshFrame>>>,
    local_private_ids: Mutex<BTreeSet<String>>,
    cognitive_records: Mutex<BTreeMap<String, MeshRecord>>,
    tasks: Mutex<BTreeMap<String, serde_json::Value>>,
    twins: Mutex<BTreeMap<String, TwinMirrorState>>,
}

impl MeshReactor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mesh_id: String,
        epoch: u64,
        local_did: String,
        privkey: PKey<Private>,
        outbox: Arc<Outbox>,
        transport: Arc<dyn MeshTransport>,
        attention: Arc<AttentionFilter>,
        conflicts_dir: PathBuf,
    ) -> Self {
        let initial_seq = outbox.max_seq_for(&local_did).unwrap_or(0);
        let initial_cursors = outbox.all_peer_cursors().unwrap_or_default();
        let mut initial_seen = BTreeSet::new();
        for (peer, &max_s) in &initial_cursors {
            for s in 1..=max_s {
                initial_seen.insert((peer.clone(), s));
            }
        }
        Self {
            mesh_id,
            epoch,
            local_did,
            privkey,
            outbox,
            transport,
            attention,
            conflicts_dir,
            next_seq: AtomicU64::new(initial_seq),
            lamport_clock: AtomicU64::new(initial_seq),
            seen_frames: Mutex::new(initial_seen),
            last_contiguous_seq: Mutex::new(initial_cursors),
            reorder_buffer: Mutex::new(BTreeMap::new()),
            local_private_ids: Mutex::new(BTreeSet::new()),
            cognitive_records: Mutex::new(BTreeMap::new()),
            tasks: Mutex::new(BTreeMap::new()),
            twins: Mutex::new(BTreeMap::new()),
        }
    }

    /// Mark `id` as a sovereign local private row that can never be overwritten by mesh peers.
    pub fn register_local_private_id(&self, id: &str) {
        if let Ok(mut set) = self.local_private_ids.lock() {
            set.insert(id.to_string());
        }
    }

    /// Sign, durably log to `frames.wal`, and publish a payload fail-soft across `MeshTransport`.
    pub async fn publish_payload(
        &self,
        namespace: &str,
        visible_to: Vec<String>,
        mut kind: MeshPayloadKind,
    ) -> Result<MeshFrame, ReactorApplyError> {
        match &mut kind {
            MeshPayloadKind::CognitiveDelta { record, .. } => {
                strip_procedure_runtime_counters(&mut record.metadata);
                strip_procedure_runtime_counters(&mut record.content);
                validate_egress_candidate(&record.memory_type, &record.metadata)
                    .map_err(ReactorApplyError::Sovereignty)?;
            }
            MeshPayloadKind::TwinDelta {
                changed_variables, ..
            } => {
                for (k, v) in changed_variables.iter() {
                    if !v.is_finite() {
                        return Err(ReactorApplyError::Sovereignty(
                            SovereigntyGateError::NonFiniteTwinValue(k.clone()),
                        ));
                    }
                }
            }
            MeshPayloadKind::TaskDelta { task_json, .. } => {
                if let Some(elig) = task_json
                    .get("metadata")
                    .and_then(|m| m.get("eligibility"))
                    .and_then(|v| v.as_str())
                {
                    if elig != "shareable" {
                        return Err(ReactorApplyError::Sovereignty(
                            SovereigntyGateError::NotShareable(format!(
                                "task eligibility is '{elig}', required 'shareable'"
                            )),
                        ));
                    }
                }
            }
            MeshPayloadKind::TaskLeaseDelta { .. } | MeshPayloadKind::SyncHello(_) => {}
        }

        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let lamport_ts = self.lamport_clock.fetch_add(1, Ordering::SeqCst) + 1;
        let mut frame = MeshFrame {
            mesh_id: self.mesh_id.clone(),
            epoch: self.epoch,
            origin_node_id: self.local_did.clone(),
            seq,
            lamport_ts,
            timestamp_ms: Utc::now().timestamp_millis(),
            namespace: namespace.to_string(),
            visible_to,
            kind,
            signature: String::new(),
        };
        frame
            .sign_frame(&self.privkey)
            .map_err(|e| ReactorApplyError::Sovereignty(SovereigntyGateError::InvalidSignature(e)))?;

        self.outbox
            .append_frame(&frame)
            .map_err(|e| ReactorApplyError::Storage(e.to_string()))?;

        // Fail-soft transport publish: frame is already durable in frames.wal
        let _ = self.transport.publish(&frame).await;
        Ok(frame)
    }

    /// Validate and apply an inbound [`MeshFrame`] according to Tri-Strata + World State rules.
    pub async fn apply_inbound_frame(
        &self,
        frame: MeshFrame,
        now_ms: i64,
    ) -> Result<ReactorApplyOutcome, ReactorApplyError> {
        validate_ingress_frame(&frame, &self.mesh_id, self.epoch, &self.local_did)
            .map_err(ReactorApplyError::Sovereignty)?;

        // Check duplicate by (origin_node_id, seq) before mutating state
        let key = (frame.origin_node_id.clone(), frame.seq);
        if self
            .seen_frames
            .lock()
            .map_err(|_| ReactorApplyError::Storage("seen_frames lock poisoned".into()))?
            .contains(&key)
        {
            return Ok(ReactorApplyOutcome::NoOpDuplicate);
        }

        // Check per-origin sequence continuity; buffer out-of-order frames
        let expected_seq = self
            .last_contiguous_seq
            .lock()
            .map_err(|_| ReactorApplyError::Storage("last_contiguous_seq lock".into()))?
            .get(&frame.origin_node_id)
            .copied()
            .unwrap_or(0)
            + 1;
        if frame.seq > expected_seq {
            let mut buf = self
                .reorder_buffer
                .lock()
                .map_err(|_| ReactorApplyError::Storage("reorder_buffer lock".into()))?;
            let peer_buf = buf.entry(frame.origin_node_id.clone()).or_default();
            if peer_buf.len() < MAX_REORDER_BUFFER_PER_PEER {
                peer_buf.insert(frame.seq, frame.clone());
            }
            return Ok(ReactorApplyOutcome::BufferedOutOfOrder {
                expected_seq,
                got_seq: frame.seq,
            });
        }

        let origin = frame.origin_node_id.clone();
        let seq = frame.seq;
        let primary_res = self.apply_single_verified_frame(frame, now_ms).await;
        self.advance_contiguous_seq(&origin, seq)?;
        let outcome = primary_res?;

        // Drain any buffered contiguous follow-up frames for this origin
        loop {
            let next_expected = self
                .last_contiguous_seq
                .lock()
                .map_err(|_| ReactorApplyError::Storage("last_contiguous_seq lock".into()))?
                .get(&origin)
                .copied()
                .unwrap_or(0)
                + 1;
            let next_frame = self
                .reorder_buffer
                .lock()
                .map_err(|_| ReactorApplyError::Storage("reorder_buffer lock".into()))?
                .get_mut(&origin)
                .and_then(|m| m.remove(&next_expected));
            let Some(buffered) = next_frame else {
                break;
            };
            let b_seq = buffered.seq;
            let _ = self.apply_single_verified_frame(buffered, now_ms).await;
            self.advance_contiguous_seq(&origin, b_seq)?;
        }

        Ok(outcome)
    }

    fn advance_contiguous_seq(&self, origin: &str, seq: u64) -> Result<(), ReactorApplyError> {
        let mut map = self
            .last_contiguous_seq
            .lock()
            .map_err(|_| ReactorApplyError::Storage("last_contiguous_seq lock".into()))?;
        let entry = map.entry(origin.to_string()).or_insert(0);
        if seq > *entry {
            *entry = seq;
        }
        Ok(())
    }

    async fn apply_single_verified_frame(
        &self,
        frame: MeshFrame,
        now_ms: i64,
    ) -> Result<ReactorApplyOutcome, ReactorApplyError> {
        let key = (frame.origin_node_id.clone(), frame.seq);
        let outcome = match &frame.kind {
            MeshPayloadKind::CognitiveDelta { record, .. } => {
                let rec_id = record.id.to_string();
                let is_private = self
                    .local_private_ids
                    .lock()
                    .map_err(|_| ReactorApplyError::Storage("local_private_ids lock".into()))?
                    .contains(&rec_id);
                if is_private {
                    return Err(ReactorApplyError::PrivateCollision(rec_id));
                }

                let mut store = self
                    .cognitive_records
                    .lock()
                    .map_err(|_| ReactorApplyError::Storage("cognitive_records lock".into()))?;
                if let Some(existing) = store.get(&rec_id) {
                    if existing.update_id == record.update_id {
                        return Ok(ReactorApplyOutcome::NoOpDuplicate);
                    }
                    let incoming_supersedes = record.lifecycle.superseded_by == Some(existing.id)
                        || existing.lifecycle.superseded_by == Some(record.id);
                    if !incoming_supersedes {
                        conflict::register_conflict(
                            &self.conflicts_dir,
                            &frame.namespace,
                            &rec_id,
                            &record.update_id.to_string(),
                            "concurrent mesh update_id divergence",
                            ConflictKind::Unresolved,
                            record,
                        )
                        .map_err(|e| ReactorApplyError::Storage(e.to_string()))?;
                        self.commit_accepted_frame(&frame, key)?;
                        return Ok(ReactorApplyOutcome::ConflictPreserved { id: rec_id });
                    }
                }
                store.insert(rec_id, record.clone());
                ReactorApplyOutcome::Applied
            }
            MeshPayloadKind::TaskDelta {
                task_id,
                action,
                task_json,
                extracted_memory_ids,
            } => {
                if *action == TaskMeshAction::Completed && extracted_memory_ids.is_empty() {
                    return Err(ReactorApplyError::Lock2Violation(task_id.clone()));
                }
                let mut tasks = self
                    .tasks
                    .lock()
                    .map_err(|_| ReactorApplyError::Storage("tasks lock".into()))?;
                match tasks.get(task_id) {
                    None => {
                        tasks.insert(task_id.clone(), task_json.clone());
                    }
                    Some(existing) => {
                        let merged = merge_task_json(existing, task_json);
                        tasks.insert(task_id.clone(), merged);
                    }
                }
                ReactorApplyOutcome::Applied
            }
            MeshPayloadKind::TaskLeaseDelta { lease } => {
                let verdict = self
                    .transport
                    .acquire_or_renew_lease(lease, now_ms)
                    .await
                    .map_err(|e| ReactorApplyError::Storage(e.to_string()))?;
                ReactorApplyOutcome::LeaseArbitrated(verdict)
            }
            MeshPayloadKind::TwinDelta {
                twin_id,
                version,
                changed_variables,
                ..
            } => {
                let mut twins = self
                    .twins
                    .lock()
                    .map_err(|_| ReactorApplyError::Storage("twins lock".into()))?;
                let entry = twins.entry(twin_id.clone()).or_default();
                if *version > entry.version {
                    entry.version = *version;
                }
                for (var_name, &val) in changed_variables {
                    let should_update = match entry.vars.get(var_name) {
                        None => true,
                        Some(cell) => {
                            (frame.lamport_ts, &frame.origin_node_id)
                                > (cell.lamport_ts, &cell.origin_node_id)
                        }
                    };
                    if should_update {
                        entry.vars.insert(
                            var_name.clone(),
                            TwinVarCell {
                                value: val,
                                lamport_ts: frame.lamport_ts,
                                origin_node_id: frame.origin_node_id.clone(),
                            },
                        );
                    }
                }
                ReactorApplyOutcome::Applied
            }
            MeshPayloadKind::SyncHello(_) => ReactorApplyOutcome::Applied,
        };

        self.commit_accepted_frame(&frame, key)?;
        Ok(outcome)
    }

    fn commit_accepted_frame(
        &self,
        frame: &MeshFrame,
        key: (String, u64),
    ) -> Result<(), ReactorApplyError> {
        let _ = self
            .lamport_clock
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |curr| {
                Some(curr.max(frame.lamport_ts) + 1)
            });
        self.seen_frames
            .lock()
            .map_err(|_| ReactorApplyError::Storage("seen_frames lock poisoned".into()))?
            .insert(key);
        self.outbox
            .append_frame(frame)
            .map_err(|e| ReactorApplyError::Storage(e.to_string()))?;
        let _ = self.attention.evaluate_inbound_frame(frame);
        Ok(())
    }

    /// Snapshot the current merged variable map and version for a Digital Twin.
    pub fn get_twin_state(&self, twin_id: &str) -> Option<(u64, BTreeMap<String, f64>)> {
        let twins = self.twins.lock().ok()?;
        let state = twins.get(twin_id)?;
        let vars = state
            .vars
            .iter()
            .map(|(k, cell)| (k.clone(), cell.value))
            .collect();
        Some((state.version, vars))
    }

    /// Snapshot the current merged JSON state for a distributed Central Executive task.
    pub fn get_task_state(&self, task_id: &str) -> Option<serde_json::Value> {
        self.tasks.lock().ok()?.get(task_id).cloned()
    }

    /// Compute missing frames from `frames.wal` to satisfy a peer's [`SyncHello`] cursor map.
    pub fn handle_sync_hello(&self, hello: &SyncHello) -> anyhow::Result<Vec<MeshFrame>> {
        if hello.mesh_id != self.mesh_id {
            anyhow::bail!(
                "SyncHello mesh_id mismatch: expected {}, got {}",
                self.mesh_id,
                hello.mesh_id
            );
        }
        if hello.epoch != self.epoch {
            anyhow::bail!(
                "SyncHello epoch mismatch: expected {}, got {}",
                self.epoch,
                hello.epoch
            );
        }
        let local_cursors = self.outbox.all_peer_cursors()?;
        let mut replay = Vec::new();
        for (origin, &max_seq) in &local_cursors {
            let peer_seq = hello.peer_cursors.get(origin).copied().unwrap_or(0);
            if max_seq > peer_seq {
                let mut slice = self
                    .outbox
                    .replay_frames_range(origin, peer_seq + 1, max_seq)?;
                replay.append(&mut slice);
            }
        }
        replay.sort_by_key(|f| (f.lamport_ts, f.seq));
        Ok(replay)
    }
}

fn task_status_rank(status: &str) -> u8 {
    match status {
        "done" => 3,
        "in_progress" => 2,
        "blocked" => 1,
        _ => 0,
    }
}

fn merge_task_json(existing: &serde_json::Value, incoming: &serde_json::Value) -> serde_json::Value {
    let mut merged = incoming.clone();
    let old_status = existing
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("open");
    let new_status = incoming
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("open");
    if task_status_rank(old_status) > task_status_rank(new_status) {
        if let Some(obj) = merged.as_object_mut() {
            obj.insert(
                "status".to_string(),
                serde_json::Value::String(old_status.to_string()),
            );
        }
    }
    if let (Some(old_notes), Some(new_notes)) = (
        existing.get("notes").and_then(|v| v.as_array()),
        incoming.get("notes").and_then(|v| v.as_array()),
    ) {
        let mut union_notes = old_notes.clone();
        for n in new_notes {
            if !union_notes.contains(n) {
                union_notes.push(n.clone());
            }
        }
        if let Some(obj) = merged.as_object_mut() {
            obj.insert("notes".to_string(), serde_json::Value::Array(union_notes));
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::generate_keypair;
    use crate::synaptic_mesh::attention::AttentionFilter;
    use crate::synaptic_mesh::frame::{MeshPayloadKind, TaskMeshAction};
    use crate::synaptic_mesh::manifest::Trust;
    use crate::synaptic_mesh::outbox::Outbox;
    use crate::synaptic_mesh::p2p::P2pMeshTransport;
    use crate::synaptic_mesh::record::{
        Lifecycle, LifecycleState, MeshRecord, MESH_RECORD_SCHEMA_VERSION,
    };
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn reactor_enforces_private_sovereignty_lock2_conflict_preservation_and_twin_lww() {
        let psk = [0x55u8; 32];
        let tmp = tempfile::tempdir().unwrap();
        let outbox_a = Arc::new(Outbox::open(&tmp.path().join("a.outbox")).unwrap());
        let outbox_b = Arc::new(Outbox::open(&tmp.path().join("b.outbox")).unwrap());
        let trans_a = P2pMeshTransport::bind("127.0.0.1:0", "m1".into(), 1, psk, outbox_a.clone())
            .await
            .unwrap();
        let trans_b = P2pMeshTransport::bind("127.0.0.1:0", "m1".into(), 1, psk, outbox_b.clone())
            .await
            .unwrap();

        let (pkey_a, did_a, _) = generate_keypair().unwrap();
        let (pkey_b, did_b, _) = generate_keypair().unwrap();

        let reactor_a = MeshReactor::new(
            "m1".into(),
            1,
            did_a.clone(),
            pkey_a,
            outbox_a,
            trans_a,
            Arc::new(AttentionFilter::new()),
            tmp.path().join("a.conflicts"),
        );
        let reactor_b = MeshReactor::new(
            "m1".into(),
            1,
            did_b.clone(),
            pkey_b,
            outbox_b,
            trans_b,
            Arc::new(AttentionFilter::new()),
            tmp.path().join("b.conflicts"),
        );

        // 1. Private row sovereignty: Node B owns private id -> refuses Node A's frame
        let priv_uuid = Uuid::new_v4();
        reactor_b.register_local_private_id(&priv_uuid.to_string());
        let rec = MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.into(),
            id: priv_uuid,
            namespace: "ns".into(),
            memory_type: "rule".into(),
            lifecycle: Lifecycle {
                state: LifecycleState::Active,
                created_at: Utc::now(),
                created_by: did_a.clone(),
                superseded_by: None,
                archived_at: None,
                tombstoned_at: None,
            },
            content: serde_json::json!("shared rule"),
            metadata: serde_json::json!({ "eligibility": "shareable" }),
            rdf: None,
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Trusted,
            signature: None,
            signer_did: Some(did_a.clone()),
            public_key: None,
            patch: None,
        };
        let frame_priv = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::CognitiveDelta {
                    record: rec.clone(),
                    rdf_patch: String::new(),
                    concept_iris: vec![],
                    file_targets: vec![],
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            reactor_b.apply_inbound_frame(frame_priv, 1000).await,
            Err(ReactorApplyError::PrivateCollision(_))
        ));

        // 2. Remote TaskDelta Completed without Lock-2 extracted memory -> rejected
        let frame_bad_done = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TaskDelta {
                    task_id: "t-1".into(),
                    action: TaskMeshAction::Completed,
                    task_json: serde_json::json!({ "id": "t-1", "status": "done" }),
                    extracted_memory_ids: vec![],
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            reactor_b.apply_inbound_frame(frame_bad_done, 1000).await,
            Err(ReactorApplyError::Lock2Violation(_))
        ));

        // 3. Concurrent TwinDelta updates merge per-variable by (lamport_ts, origin_node_id)
        let f_twin_a = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TwinDelta {
                    twin_id: "reactor-1".into(),
                    version: 1,
                    source: "sensor".into(),
                    rdf_patch: String::new(),
                    changed_variables: BTreeMap::from([
                        ("temp".into(), 100.0),
                        ("pressure".into(), 2.0),
                    ]),
                },
            )
            .await
            .unwrap();
        let f_twin_b = reactor_b
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TwinDelta {
                    twin_id: "reactor-1".into(),
                    version: 2,
                    source: "sensor".into(),
                    rdf_patch: String::new(),
                    changed_variables: BTreeMap::from([
                        ("pressure".into(), 5.5),
                        ("flow".into(), 12.0),
                    ]),
                },
            )
            .await
            .unwrap();
        reactor_b
            .apply_inbound_frame(f_twin_b.clone(), 1000)
            .await
            .unwrap();
        reactor_b
            .apply_inbound_frame(f_twin_a.clone(), 1000)
            .await
            .unwrap();
        let (_, merged) = reactor_b.get_twin_state("reactor-1").unwrap();
        assert_eq!(merged.get("temp"), Some(&100.0));
        assert_eq!(merged.get("flow"), Some(&12.0));
        // f_twin_a had higher lamport_ts (3 vs 1), so pressure = 2.0 wins deterministically
        assert_eq!(merged.get("pressure"), Some(&2.0));

        // 4. Concurrent shareable rule divergence preserves both variants in conflict overlay
        let shared_id = Uuid::new_v4();
        let mut rec_v1 = rec.clone();
        rec_v1.id = shared_id;
        rec_v1.update_id = Uuid::new_v4();
        let mut rec_v2 = rec.clone();
        rec_v2.id = shared_id;
        rec_v2.update_id = Uuid::new_v4();
        rec_v2.content = serde_json::json!("divergent rule content");

        let frame_v1 = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::CognitiveDelta {
                    record: rec_v1,
                    rdf_patch: String::new(),
                    concept_iris: vec![],
                    file_targets: vec![],
                },
            )
            .await
            .unwrap();
        let frame_v2 = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::CognitiveDelta {
                    record: rec_v2,
                    rdf_patch: String::new(),
                    concept_iris: vec![],
                    file_targets: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(
            reactor_b.apply_inbound_frame(frame_v1, 1000).await.unwrap(),
            ReactorApplyOutcome::Applied
        );
        assert_eq!(
            reactor_b.apply_inbound_frame(frame_v2, 1000).await.unwrap(),
            ReactorApplyOutcome::ConflictPreserved {
                id: shared_id.to_string()
            }
        );

        // 5. Valid TaskDelta with Lock-2 memory applies and SyncHello replays missing frames
        let frame_ok_done = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TaskDelta {
                    task_id: "t-2".into(),
                    action: TaskMeshAction::Completed,
                    task_json: serde_json::json!({ "id": "t-2", "status": "done" }),
                    extracted_memory_ids: vec!["mem-1".into()],
                },
            )
            .await
            .unwrap();
        assert_eq!(
            reactor_b.apply_inbound_frame(frame_ok_done, 1000).await.unwrap(),
            ReactorApplyOutcome::Applied
        );
        assert_eq!(
            reactor_b.get_task_state("t-2").unwrap()["status"],
            "done"
        );
        let catchup = reactor_a
            .handle_sync_hello(&SyncHello {
                node_id: did_b,
                mesh_id: "m1".into(),
                epoch: 1,
                peer_cursors: BTreeMap::from([(did_a, 4)]),
            })
            .unwrap();
        assert_eq!(catchup.len(), 2);
    }

    #[tokio::test]
    async fn reactor_fixes_egress_validation_wal_dedup_gap_buffering_notes_and_hello_check() {
        let psk = [0x66u8; 32];
        let tmp = tempfile::tempdir().unwrap();
        let outbox_a = Arc::new(Outbox::open(&tmp.path().join("a2.outbox")).unwrap());
        let outbox_b = Arc::new(Outbox::open(&tmp.path().join("b2.outbox")).unwrap());
        let trans_a = P2pMeshTransport::bind("127.0.0.1:0", "m1".into(), 1, psk, outbox_a.clone())
            .await
            .unwrap();
        let trans_b = P2pMeshTransport::bind("127.0.0.1:0", "m1".into(), 1, psk, outbox_b.clone())
            .await
            .unwrap();

        let (pkey_a, did_a, _) = generate_keypair().unwrap();
        let (pkey_b, did_b, _) = generate_keypair().unwrap();

        let reactor_a = MeshReactor::new(
            "m1".into(),
            1,
            did_a.clone(),
            pkey_a,
            outbox_a.clone(),
            trans_a,
            Arc::new(AttentionFilter::new()),
            tmp.path().join("a2.conflicts"),
        );
        let reactor_b = MeshReactor::new(
            "m1".into(),
            1,
            did_b.clone(),
            pkey_b.clone(),
            outbox_b.clone(),
            trans_b.clone(),
            Arc::new(AttentionFilter::new()),
            tmp.path().join("b2.conflicts"),
        );

        // 1. Egress TwinDelta with NaN is rejected before sequencing
        let nan_res = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TwinDelta {
                    twin_id: "t1".into(),
                    version: 1,
                    source: "sensor".into(),
                    rdf_patch: String::new(),
                    changed_variables: BTreeMap::from([("temp".into(), f64::NAN)]),
                },
            )
            .await;
        assert!(matches!(
            nan_res,
            Err(ReactorApplyError::Sovereignty(
                SovereigntyGateError::NonFiniteTwinValue(_)
            ))
        ));

        // 2. Egress TaskDelta with private eligibility is rejected before sequencing
        let priv_task_res = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TaskDelta {
                    task_id: "t-priv".into(),
                    action: TaskMeshAction::Created,
                    task_json: serde_json::json!({
                        "id": "t-priv",
                        "status": "open",
                        "metadata": { "eligibility": "private" }
                    }),
                    extracted_memory_ids: vec![],
                },
            )
            .await;
        assert!(matches!(
            priv_task_res,
            Err(ReactorApplyError::Sovereignty(
                SovereigntyGateError::NotShareable(_)
            ))
        ));

        // 3. Out-of-order gap buffering: seq 1 and seq 2 published by A; B receives seq 2 first
        let f1 = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TaskDelta {
                    task_id: "t-gap".into(),
                    action: TaskMeshAction::Created,
                    task_json: serde_json::json!({
                        "id": "t-gap",
                        "status": "open",
                        "notes": ["note-a"]
                    }),
                    extracted_memory_ids: vec![],
                },
            )
            .await
            .unwrap();
        let f2 = reactor_a
            .publish_payload(
                "ns",
                vec![],
                MeshPayloadKind::TaskDelta {
                    task_id: "t-gap".into(),
                    action: TaskMeshAction::Updated,
                    task_json: serde_json::json!({
                        "id": "t-gap",
                        "status": "in_progress",
                        "notes": ["note-b"]
                    }),
                    extracted_memory_ids: vec![],
                },
            )
            .await
            .unwrap();

        assert_eq!(
            reactor_b.apply_inbound_frame(f2, 1000).await.unwrap(),
            ReactorApplyOutcome::BufferedOutOfOrder {
                expected_seq: 1,
                got_seq: 2
            }
        );
        // Applying f1 applies f1 AND drains buffered f2, unioning notes
        assert_eq!(
            reactor_b.apply_inbound_frame(f1.clone(), 1000).await.unwrap(),
            ReactorApplyOutcome::Applied
        );
        let merged_task = reactor_b.get_task_state("t-gap").unwrap();
        assert_eq!(merged_task["status"], "in_progress");
        let notes = merged_task["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 2);

        // 4. Restarted reactor seeds seen_frames from outbox and deduplicates f1
        let reactor_b_restarted = MeshReactor::new(
            "m1".into(),
            1,
            did_b.clone(),
            pkey_b,
            outbox_b,
            trans_b,
            Arc::new(AttentionFilter::new()),
            tmp.path().join("b2.conflicts"),
        );
        assert_eq!(
            reactor_b_restarted
                .apply_inbound_frame(f1, 1000)
                .await
                .unwrap(),
            ReactorApplyOutcome::NoOpDuplicate
        );

        // 5. handle_sync_hello rejects wrong mesh_id or epoch
        assert!(reactor_a
            .handle_sync_hello(&SyncHello {
                node_id: did_b,
                mesh_id: "wrong-mesh".into(),
                epoch: 1,
                peer_cursors: BTreeMap::new(),
            })
            .is_err());
    }
}
