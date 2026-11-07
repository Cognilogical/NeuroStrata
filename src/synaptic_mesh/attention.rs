//! Session working-set `AttentionFilter` for targeted `[NeuroMesh Δ]` agent interrupts.
//!
//! Keeps agents mesh-ignorant by default while queueing a 1-line
//! `[NeuroMesh Δ peer=<short_did> lamport=<ts>] ...` notice only when an inbound
//! peer delta intersects the local session's active tasks, files, concepts, or twins.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use crate::synaptic_mesh::frame::{MeshFrame, MeshPayloadKind, TaskMeshAction};

/// Active working set for one local MCP session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionWorkingSet {
    pub session_id: String,
    pub namespace: String,
    pub active_task_ids: BTreeSet<String>,
    pub file_targets: BTreeSet<String>,
    pub concept_iris: BTreeSet<String>,
    pub twin_ids: BTreeSet<String>,
}

/// Evaluates inbound [`MeshFrame`]s against active session working sets and buffers
/// 1-line `[NeuroMesh Δ]` notices for the next MCP tool response.
#[derive(Debug, Default)]
pub struct AttentionFilter {
    sessions: Mutex<BTreeMap<String, SessionWorkingSet>>,
    pending_notices: Mutex<BTreeMap<String, Vec<String>>>,
}

impl AttentionFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace the working set for `ws.session_id`.
    pub fn register_or_update_session(&self, ws: SessionWorkingSet) {
        if let Ok(mut map) = self.sessions.lock() {
            map.insert(ws.session_id.clone(), ws);
        }
    }

    /// Evaluate an inbound [`MeshFrame`] against all registered sessions.
    /// Returns `(session_id, formatted_notice)` pairs for every intersecting session
    /// and queues each notice for [`drain_session_notices`](Self::drain_session_notices).
    pub fn evaluate_inbound_frame(&self, frame: &MeshFrame) -> Vec<(String, String)> {
        let sessions = match self.sessions.lock() {
            Ok(s) => s.clone(),
            Err(_) => return Vec::new(),
        };
        let short_peer = short_did(&frame.origin_node_id);
        let mut matches = Vec::new();

        for (session_id, ws) in &sessions {
            if !ws.namespace.is_empty() && ws.namespace != frame.namespace {
                continue;
            }
            let summary = match &frame.kind {
                MeshPayloadKind::CognitiveDelta {
                    record,
                    concept_iris,
                    file_targets,
                    ..
                } => {
                    let concept_hit = concept_iris.iter().any(|c| ws.concept_iris.contains(c));
                    let file_hit = file_targets.iter().any(|f| ws.file_targets.contains(f));
                    if concept_hit || file_hit {
                        Some(format!(
                            "{} `{}` ({})",
                            record.memory_type, record.id, record.lifecycle.state
                        ))
                    } else {
                        None
                    }
                }
                MeshPayloadKind::TaskDelta {
                    task_id, action, ..
                } => {
                    if ws.active_task_ids.contains(task_id) {
                        let verb = match action {
                            TaskMeshAction::Created => "created",
                            TaskMeshAction::Updated => "updated",
                            TaskMeshAction::Completed => "completed",
                        };
                        Some(format!("Task `{task_id}` {verb} by peer"))
                    } else {
                        None
                    }
                }
                MeshPayloadKind::TaskLeaseDelta { lease } => {
                    if ws.active_task_ids.contains(&lease.task_id) {
                        Some(format!(
                            "Task `{}` claimed by peer `{}`",
                            lease.task_id,
                            short_did(&lease.holder_node_id)
                        ))
                    } else {
                        None
                    }
                }
                MeshPayloadKind::TwinDelta {
                    twin_id,
                    changed_variables,
                    ..
                } => {
                    if ws.twin_ids.contains(twin_id) {
                        let vars: Vec<String> = changed_variables
                            .iter()
                            .map(|(k, v)| format!("{twin_id}.{k} → {v}"))
                            .collect();
                        Some(format!("Twin {}", vars.join(", ")))
                    } else {
                        None
                    }
                }
                MeshPayloadKind::SyncHello(_) => None,
            };

            if let Some(summary) = summary {
                let line = format!(
                    "[NeuroMesh Δ peer={short_peer} lamport={}] {summary}.",
                    frame.lamport_ts
                );
                matches.push((session_id.clone(), line.clone()));
                if let Ok(mut pending) = self.pending_notices.lock() {
                    pending.entry(session_id.clone()).or_default().push(line);
                }
            }
        }

        matches
    }

    /// Drain all queued `[NeuroMesh Δ]` notices for `session_id`.
    pub fn drain_session_notices(&self, session_id: &str) -> Vec<String> {
        if let Ok(mut pending) = self.pending_notices.lock() {
            pending.remove(session_id).unwrap_or_default()
        } else {
            Vec::new()
        }
    }
}

fn short_did(did: &str) -> String {
    let stripped = did.strip_prefix("did:key:").unwrap_or(did);
    if stripped.len() <= 12 {
        stripped.to_string()
    } else {
        format!(
            "{}...{}",
            &stripped[..6],
            &stripped[stripped.len().saturating_sub(4)..]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synaptic_mesh::frame::{MeshFrame, MeshPayloadKind};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn emits_neuromesh_delta_only_when_inbound_frame_intersects_session_working_set() {
        let filter = AttentionFilter::new();
        filter.register_or_update_session(SessionWorkingSet {
            session_id: "ses-active".into(),
            namespace: "ops".into(),
            active_task_ids: BTreeSet::from(["task-100".to_string()]),
            file_targets: BTreeSet::from(["src/auth.rs".to_string()]),
            concept_iris: BTreeSet::from(["urn:ns:concept:jwt".to_string()]),
            twin_ids: BTreeSet::from(["cooling-loop-1".to_string()]),
        });

        // 1. Unrelated twin update -> 0 notices
        let unrelated = MeshFrame {
            mesh_id: "m1".into(),
            epoch: 1,
            origin_node_id: "did:key:z6Mkp99998f2a".into(),
            seq: 1,
            lamport_ts: 411,
            timestamp_ms: 1000,
            namespace: "ops".into(),
            visible_to: vec![],
            kind: MeshPayloadKind::TwinDelta {
                twin_id: "other-twin".into(),
                version: 2,
                source: "sensor".into(),
                rdf_patch: String::new(),
                changed_variables: BTreeMap::from([("pressure".to_string(), 2.0)]),
            },
            signature: "sig".into(),
        };
        assert!(filter.evaluate_inbound_frame(&unrelated).is_empty());
        assert!(filter.drain_session_notices("ses-active").is_empty());

        // 2. Intersecting twin + task update -> 1-line [NeuroMesh Δ ...] notice queued
        let matching_twin = MeshFrame {
            lamport_ts: 412,
            kind: MeshPayloadKind::TwinDelta {
                twin_id: "cooling-loop-1".into(),
                version: 3,
                source: "sensor".into(),
                rdf_patch: String::new(),
                changed_variables: BTreeMap::from([("pressure".to_string(), 4.8)]),
            },
            ..unrelated.clone()
        };
        let hits = filter.evaluate_inbound_frame(&matching_twin);
        assert_eq!(hits.len(), 1);
        let drained = filter.drain_session_notices("ses-active");
        assert_eq!(drained.len(), 1);
        assert!(drained[0].starts_with("[NeuroMesh Δ peer="));
        assert!(drained[0].contains("lamport=412]"));
        assert!(drained[0].contains("cooling-loop-1"));
        // Draining clears the queue
        assert!(filter.drain_session_notices("ses-active").is_empty());
    }
}
