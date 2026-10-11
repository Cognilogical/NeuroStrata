//! Mesh manifest — the file at the root of every SynapticMesh git repo
//! (`<mesh_dir>/manifest.json`) that names the mesh, declares the schema,
//! records its origin, and carries the team's sharing policy.
//!
//! On-disk schema is `synaptic-mesh/v1`. The struct is the Rust mirror of
//! that schema; the round-trip test (`tests::round_trips_through_serde_json`)
//! is the contract that proves the on-disk wire format and the in-process
//! struct agree.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The schema version baked into every `manifest.json`. Bumping this is a
/// breaking change for older daemons; an unknown version fails import.
pub const MESH_MANIFEST_SCHEMA_VERSION: &str = "synaptic-mesh/v1";

/// The root of a SynapticMesh repo. Lives at `<mesh_dir>/manifest.json`,
/// written once at `init` time and rewritten only on policy change or epoch
/// migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshManifest {
    pub schema_version: String,
    pub mesh_id: Uuid,
    pub mesh_name: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    /// Coordinated-rewrite counter (spec §"Deletion & erasure": a purge is an
    /// epoch migration). Increments on every coordinated history rewrite; at
    /// import, each record's `origin_mesh_epoch` is compared against this and
    /// a record from an older epoch is quarantined rather than replayed into
    /// the new epoch (`super::import::epoch_gate`).
    pub epoch: u32,
    pub namespaces: Vec<String>,
    pub policy: MeshPolicy,
    pub remote: Option<String>,
}

/// The team's sharing policy. Every field is enforced by the mesh-worker at
/// publish time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshPolicy {
    /// If true, records without an explicit `eligibility: shareable` are
    /// refused at publish time. Defaults to true (opt-in sharing).
    pub require_record_eligibility: bool,
    /// Trust level applied to imported procedures on first import. The spec
    /// locks this at `Untrusted`; promoted procedures must come from local
    /// approval.
    pub imported_procedures_trust: Trust,
    /// Whether the team has agreed to coordinated history rewrites
    /// (`filter-repo` purge) at all. False = tombstones only, no history
    /// rewriting permitted.
    pub allow_history_purge: bool,
    /// Whether the team has agreed to unattended mesh-repo compaction.
    /// Defaults to false — compaction is an explicit operator action.
    pub auto_compact: bool,
    /// Consented cadence, in seconds, for the daemon's *periodic* mesh pull
    /// (spec §"Triggers": "optionally consented periodic fetch at turn
    /// boundary"). `None` (the default) means NO scheduled pulls: the daemon
    /// only pulls on session start (best-effort, 5s timeout) and on explicit
    /// `mesh pull` / pull-before-push. The field is serde-default so older
    /// manifests without it stay readable (slice 7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull_interval_secs: Option<u64>,
}

/// Trust level for a record, especially a procedure. The trust spec mirrors
/// the spec: `Trusted` for local records, `Untrusted` for imports, `Promoted`
/// after local promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    Trusted,
    Untrusted,
    Promoted,
}

impl std::fmt::Display for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Trust::Trusted => f.write_str("trusted"),
            Trust::Untrusted => f.write_str("untrusted"),
            Trust::Promoted => f.write_str("promoted"),
        }
    }
}

/// Default policy: opt-in sharing, untrusted imports, no purge, no compaction.
impl Default for MeshPolicy {
    fn default() -> Self {
        Self {
            require_record_eligibility: true,
            imported_procedures_trust: Trust::Untrusted,
            allow_history_purge: false,
            auto_compact: false,
            auto_pull_interval_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_serde_json() {
        let manifest = MeshManifest {
            schema_version: MESH_MANIFEST_SCHEMA_VERSION.to_string(),
            mesh_id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            mesh_name: "stripe-api-team".to_string(),
            created_at: DateTime::parse_from_rfc3339("2026-10-10T12:00:00+00:00")
                .unwrap()
                .with_timezone(&Utc),
            created_by: "node-abc123".to_string(),
            epoch: 1,
            namespaces: vec!["stripe-api".to_string()],
            policy: MeshPolicy::default(),
            remote: Some("https://github.com/team-cognilogical/mesh-stripe-api.git".to_string()),
        };

        let json = serde_json::to_string(&manifest).expect("serialize manifest");
        let restored: MeshManifest = serde_json::from_str(&json).expect("deserialize manifest");

        assert_eq!(
            restored, manifest,
            "manifest must round-trip through JSON byte-identical"
        );
    }

    #[test]
    fn schema_version_is_pinned() {
        // Bumping MESH_MANIFEST_SCHEMA_VERSION is a breaking change for older
        // daemons. Pin it here so any drift fails the test loudly.
        assert_eq!(MESH_MANIFEST_SCHEMA_VERSION, "synaptic-mesh/v1");
    }

    #[test]
    fn default_policy_is_opt_in_only() {
        // Per spec §"Sharing boundary": namespace opt-in is necessary but not
        // sufficient; records default to private. The default policy must
        // require explicit eligibility, default procedures to untrusted, and
        // refuse history rewriting and auto-compaction by default.
        let p = MeshPolicy::default();
        assert!(
            p.require_record_eligibility,
            "records must default to private"
        );
        assert_eq!(p.imported_procedures_trust, Trust::Untrusted);
        assert!(!p.allow_history_purge);
        assert!(!p.auto_compact);
        // Periodic FETCH is a consented trigger (spec §"Triggers"): absent
        // consent the daemon must not schedule pulls on its own.
        assert_eq!(p.auto_pull_interval_secs, None);
    }

    #[test]
    fn auto_pull_interval_is_serde_default_and_optional() {
        // An OLD manifest (no field) deserializes fine, and a manifest WITH
        // the field round-trips it — slice 8 daemons reading slice 7
        // manifests and vice versa must both hold.
        let old = r#"{
            "require_record_eligibility": true,
            "imported_procedures_trust": "untrusted",
            "allow_history_purge": false,
            "auto_compact": false
        }"#;
        let policy: MeshPolicy = serde_json::from_str(old).expect("old policy parses");
        assert_eq!(policy, MeshPolicy::default(), "missing field == default");

        let consented = MeshPolicy {
            auto_pull_interval_secs: Some(600),
            ..MeshPolicy::default()
        };
        let json = serde_json::to_string(&consented).expect("serializes");
        let back: MeshPolicy = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back.auto_pull_interval_secs, Some(600));
    }

    #[test]
    fn trust_serializes_lowercase() {
        // The on-disk schema uses lowercase ("trusted", "untrusted",
        // "promoted"). Rust's default rename would emit "Trusted" etc.; the
        // `rename_all = "lowercase"` annotation is what keeps the wire format
        // stable.
        for (t, expected) in [
            (Trust::Trusted, r#""trusted""#),
            (Trust::Untrusted, r#""untrusted""#),
            (Trust::Promoted, r#""promoted""#),
        ] {
            assert_eq!(serde_json::to_string(&t).unwrap(), expected);
        }
    }
}
