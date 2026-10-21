//! Mesh record — one file per memory or procedure in the SynapticMesh repo.
//!
//! See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md` §"Record format".
//!
//! The wire format is `synaptic-mesh-record/v1`. The on-disk JSON file holds
//! exactly one record; the file path is `<namespace>/memories/<id>.json` or
//! `<namespace>/procedures/<id>.json`.
//!
//! The `LifecycleState` enum is the state machine for the spec's allowed
//! transitions (`tests::transitions_match_spec_table` enforces the table from
//! §"Lifecycle state machine" and §"Concurrent merge policy").

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// On-disk schema version. Bumping is a breaking change for older daemons.
pub const MESH_RECORD_SCHEMA_VERSION: &str = "synaptic-mesh-record/v1";

/// A single mesh record (memory or procedure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshRecord {
    pub schema_version: String,
    pub id: Uuid,
    pub namespace: String,
    /// Memory type from the NeuroStrata vocabulary: `rule`, `fact`, or
    /// `procedure`. The mesh is closed over this set (`SHAREABLE_MEMORY_TYPES`
    /// in `synaptic_mesh/import.rs`); an unknown value — including
    /// `structural` — is rejected at import time.
    pub memory_type: String,
    pub lifecycle: Lifecycle,
    /// Type-specific content. For `procedure`, this carries the definition
    /// (trigger, initial budget, valid_to, rehearsal_script). Embeddings are
    /// NOT shared.
    pub content: serde_json::Value,
    /// Per-record metadata. `eligibility` and `trust` are the gate fields
    /// the mesh-worker consults at publish time.
    pub metadata: serde_json::Value,
    pub relationships: Vec<Relationship>,
    /// Monotonic per-record id. Bumps on every accepted change. The pull
    /// diff uses `(id, update_id)` as the identity tuple; a record at the
    /// same `update_id` is a no-op.
    pub update_id: Uuid,
    /// The mesh epoch when this record was first published. Two gates compare
    /// it against the mesh manifest's current epoch: `import::epoch_gate`
    /// refuses to *replay* a record born in an older epoch, and the
    /// mesh-worker's `drain_once` refuses to *publish* an outbox entry born in
    /// an older epoch (quarantined, not replayed into the new epoch).
    pub origin_mesh_epoch: u32,
    /// Local trust flag. Mirrors the spec's trust model.
    pub trust: super::manifest::Trust,
}

/// Lifecycle state and provenance for a mesh record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub state: LifecycleState,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub superseded_by: Option<Uuid>,
    pub archived_at: Option<DateTime<Utc>>,
    pub tombstoned_at: Option<DateTime<Utc>>,
}

/// Allowed transitions per spec. The state machine is closed; an unknown
/// transition is refused at apply time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleState {
    Active,
    Archived,
    Superseded,
    Tombstoned,
}

impl std::fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleState::Active => f.write_str("active"),
            LifecycleState::Archived => f.write_str("archived"),
            LifecycleState::Superseded => f.write_str("superseded"),
            LifecycleState::Tombstoned => f.write_str("tombstoned"),
        }
    }
}

impl std::str::FromStr for LifecycleState {
    type Err = String;

    /// The wire form is lowercase (see [`Display`]); an unknown token is an
    /// error so a future state never silently reads as "active". Import uses
    /// this to turn a stored `metadata.mesh.lifecycle_state` back into the
    /// enum before gating a candidate transition.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(LifecycleState::Active),
            "archived" => Ok(LifecycleState::Archived),
            "superseded" => Ok(LifecycleState::Superseded),
            "tombstoned" => Ok(LifecycleState::Tombstoned),
            other => Err(format!("unknown lifecycle state {other:?}")),
        }
    }
}

/// A typed edge between two records. Today: `supersedes` (terminal) and
/// `references` (non-terminal link).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Relationship {
    pub kind: String,
    pub target: Uuid,
}

impl LifecycleState {
    /// Returns true iff `self -> next` is an allowed transition per the spec
    /// §"Lifecycle state machine" rules:
    ///
    /// - active -> archived (reversible)
    /// - archived -> active (observing the archived revision)
    /// - active/archived -> superseded (terminal)
    /// - any -> tombstoned (terminal)
    /// - retired IDs never reactivate (the spec rule: `superseded -> active`
    ///   and `tombstoned -> active` are forbidden)
    pub fn can_transition_to(self, next: LifecycleState) -> bool {
        use LifecycleState::*;
        match (self, next) {
            (Active, Archived) => true,
            (Archived, Active) => true,
            (Active, Superseded) => true,
            (Archived, Superseded) => true,
            (Active, Tombstoned) => true,
            (Archived, Tombstoned) => true,
            (Superseded, Tombstoned) => true,
            // All other transitions are forbidden.
            _ => false,
        }
    }

    /// Returns true iff this state is terminal (no further forward
    /// transitions allowed except tombstoning, which is the only escape).
    pub fn is_terminal(self) -> bool {
        // Tombstoned is terminal. Superseded is terminal but allows one
        // last hop to tombstoned. So "terminal" here means "no forward
        // progress possible without tombstoning."
        matches!(self, LifecycleState::Tombstoned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_at(state: LifecycleState) -> MeshRecord {
        MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
            id: Uuid::nil(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            lifecycle: Lifecycle {
                state,
                created_at: DateTime::parse_from_rfc3339("2026-10-10T12:00:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc),
                created_by: "node-abc".to_string(),
                superseded_by: None,
                archived_at: None,
                tombstoned_at: None,
            },
            content: serde_json::json!({"text": "fixture"}),
            metadata: serde_json::json!({"eligibility": "shareable"}),
            relationships: vec![],
            update_id: Uuid::nil(),
            origin_mesh_epoch: 1,
            trust: super::super::manifest::Trust::Trusted,
        }
    }

    #[test]
    fn transitions_match_spec_table() {
        use LifecycleState::*;
        // Allowed transitions from §"Lifecycle state machine".
        let allowed: &[(LifecycleState, LifecycleState)] = &[
            (Active, Archived),
            (Archived, Active),
            (Active, Superseded),
            (Archived, Superseded),
            (Active, Tombstoned),
            (Archived, Tombstoned),
            (Superseded, Tombstoned),
        ];
        for (from, to) in allowed {
            assert!(
                from.can_transition_to(*to),
                "spec-allowed transition {:?} -> {:?} refused",
                from,
                to
            );
        }

        // All other pairs must be refused. Exhaust the cross-product of
        // states excluding the allowed set.
        let all = [Active, Archived, Superseded, Tombstoned];
        let mut refused_count = 0;
        for from in all {
            for to in all {
                if allowed.contains(&(from, to)) {
                    continue;
                }
                assert!(
                    !from.can_transition_to(to),
                    "forbidden transition {:?} -> {:?} was accepted",
                    from,
                    to
                );
                refused_count += 1;
            }
        }
        // Sanity check: there should be 16 - 7 = 9 forbidden pairs in the
        // 4x4 cross product. If the allowed set ever changes this number
        // shifts and the test fails loudly.
        assert_eq!(refused_count, 9, "transition matrix changed shape");
    }

    #[test]
    fn retired_ids_never_reactivate() {
        // Spec rule: superseded -> active and tombstoned -> active are
        // forbidden. A retired ID must get a new linked ID, not be revived.
        assert!(!LifecycleState::Superseded.can_transition_to(LifecycleState::Active));
        assert!(!LifecycleState::Tombstoned.can_transition_to(LifecycleState::Active));
        // Self-transitions are also forbidden (would be a no-op anyway).
        for s in [
            LifecycleState::Active,
            LifecycleState::Archived,
            LifecycleState::Superseded,
            LifecycleState::Tombstoned,
        ] {
            assert!(
                !s.can_transition_to(s),
                "self-transition {:?} -> {:?} was accepted",
                s,
                s
            );
        }
    }

    #[test]
    fn tombstoned_is_terminal() {
        // The only forward escape from tombstoned is to... nothing. A
        // tombstoned record is permanent. (Filter-repo purge is an
        // epoch-migration operator action, not a transition.)
        assert!(LifecycleState::Tombstoned.is_terminal());
        // Superseded is "soft terminal" — tombstoning is its only forward
        // hop. is_terminal returns false because there's still one hop.
        assert!(!LifecycleState::Superseded.is_terminal());
        assert!(!LifecycleState::Active.is_terminal());
        assert!(!LifecycleState::Archived.is_terminal());
    }

    #[test]
    fn record_round_trips_through_serde_json() {
        let r = record_at(LifecycleState::Active);
        let json = serde_json::to_string(&r).expect("serialize record");
        let restored: MeshRecord = serde_json::from_str(&json).expect("deserialize record");
        assert_eq!(restored, r, "record must round-trip byte-identical");
    }

    #[test]
    fn lifecycle_state_serializes_lowercase() {
        // Wire format is lowercase ("active", "archived", "superseded",
        // "tombstoned").
        for (s, expected) in [
            (LifecycleState::Active, r#""active""#),
            (LifecycleState::Archived, r#""archived""#),
            (LifecycleState::Superseded, r#""superseded""#),
            (LifecycleState::Tombstoned, r#""tombstoned""#),
        ] {
            assert_eq!(serde_json::to_string(&s).unwrap(), expected);
        }
    }

    #[test]
    fn record_schema_version_is_pinned() {
        assert_eq!(MESH_RECORD_SCHEMA_VERSION, "synaptic-mesh-record/v1");
    }
}
