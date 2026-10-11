//! Conflict overlay — where unresolvable concurrent merges land (slice 6).
//!
//! See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`
//! §"Concurrent merge policy" and §"Status surfaces".
//!
//! Two responsibilities, one module:
//!
//! 1. **Durable overlay** — an on-disk registration of every id whose
//!    incoming candidate collided with the locally accepted projection in a
//!    way the merge table does not auto-resolve. The overlay lives in a
//!    sibling dir of the mesh checkout (mirroring `outbox.rs`'s
//!    `<mesh_dir>.outbox` pattern) so `git add -A` can never stage it:
//!
//!    ```text
//!    ~/.NeuroStrata/synaptic-mesh/<mesh_id>/           # the git repo
//!    ~/.NeuroStrata/synaptic-mesh/<mesh_id>.conflicts/  # this module's dir
//!        overlay.json                                   # the registration
//!        <namespace>/<id>.json                          # preserved candidate variants
//!    ```
//!
//!    The incoming candidate (which is NOT applied to the store) is written
//!    byte-for-byte into `<namespace>/<id>.json`, and the id is registered in
//!    `overlay.json` with a reason. Both the variant and the registration are
//!    idempotent (re-saving overwrites identically), which is what makes the
//!    "re-present next pull" cursor strategy safe.
//!
//! 2. **Live suppression signal** — the store read paths (`search_memory`,
//!    `get_snapshot`, `export_graph`) do not carry `project_root`, so they
//!    cannot resolve the mesh checkout to read the overlay file. Instead, at
//!    conflict time the import path annotates the *existing* row's
//!    `metadata.mesh.conflict = true` (an additive marker inside the
//!    mesh-owned provenance sub-object — the row's content, lifecycle and
//!    user metadata are untouched, matching "row untouched"). [`is_conflicting`]
//!    is the one shared question the read paths ask, mirroring
//!    `archive_memory::is_archived`.
//!
//! The overlay file remains the durable source of truth for [`status_lines`]
//! and for [`Overlay`]-driven re-presentation; the metadata marker is its
//! live reflection in the store.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::record::MeshRecord;
use crate::traits::MemoryPayload;

/// The overlay file's name inside the conflicts dir.
pub const OVERLAY_FILE_NAME: &str = "overlay.json";
/// On-disk schema version of the overlay file. Bumping is a breaking change.
pub const OVERLAY_SCHEMA_VERSION: &str = "synaptic-mesh-conflict/v1";

/// The conflicts dir for a mesh checkout: `<mesh_dir>.conflicts`, a sibling of
/// the repo so `git add -A` never stages it (same rationale as the outbox).
pub fn conflicts_dir_for(mesh_dir: &Path) -> PathBuf {
    let mut os = mesh_dir.as_os_str().to_os_string();
    os.push(".conflicts");
    PathBuf::from(os)
}

/// What kind of conflict entry this is. `Unresolved` pauses integration at
/// the id and suppresses it from the effective view; `Preserved` keeps the
/// record authoritative (a replacement won) but retains the losing candidate
/// for audit, without suppressing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictKind {
    Unresolved,
    Preserved,
}

/// One registered conflict: the colliding id, why it collided, and where the
/// preserved incoming candidate lives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictEntry {
    pub namespace: String,
    pub id: String,
    /// The incoming candidate's `update_id` — re-presentation compares it to
    /// the still-unresolved candidate so a team-side fix (a new revision) is
    /// picked up while a stale re-presentation stays a no-op.
    pub update_id: String,
    pub reason: String,
    /// Path of the preserved variant, relative to the conflicts dir.
    pub variant_file: String,
    pub kind: ConflictKind,
    pub recorded_at: String,
}

/// The overlay file: a versioned list of conflict entries. Empty on a fresh
/// mesh; only ever appended-to or pruned via [`clear_conflict`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overlay {
    pub schema_version: String,
    pub conflicts: Vec<ConflictEntry>,
}

impl Overlay {
    fn empty() -> Self {
        Self {
            schema_version: OVERLAY_SCHEMA_VERSION.to_string(),
            conflicts: Vec::new(),
        }
    }

    /// The ids in `namespace` registered as *unresolved* (suppressible).
    ///
    /// The live suppression signal reads the row's `metadata.mesh.conflict`
    /// marker (via [`is_conflicting_metadata`]) rather than this set, because
    /// the store read paths carry no `mesh_dir`. This remains the overlay's
    /// own query for `status_lines`/future read sites that do resolve the dir.
    #[allow(dead_code)]
    pub fn unresolved_ids(&self, namespace: &str) -> HashSet<String> {
        self.conflicts
            .iter()
            .filter(|e| e.namespace == namespace && e.kind == ConflictKind::Unresolved)
            .map(|e| e.id.clone())
            .collect()
    }

    /// Number of unresolved conflicts (integration-pausing).
    pub fn unresolved_count(&self) -> usize {
        self.conflicts
            .iter()
            .filter(|e| e.kind == ConflictKind::Unresolved)
            .count()
    }
}

/// Load the overlay for a mesh checkout. **Fail-soft**: a missing or corrupt
/// file reads as empty — a network/disk absence never fails a query or a
/// pull; it just means "nothing registered yet".
pub fn load_overlay(mesh_dir: &Path) -> Overlay {
    let file = conflicts_dir_for(mesh_dir).join(OVERLAY_FILE_NAME);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(_) => return Overlay::empty(),
    };
    let overlay: Overlay = match serde_json::from_str(&text) {
        Ok(o) => o,
        Err(_) => return Overlay::empty(),
    };
    if overlay.schema_version != OVERLAY_SCHEMA_VERSION {
        return Overlay::empty();
    }
    overlay
}

/// Atomically persist the overlay (tmp → rename), after any registration or
/// clear.
pub fn save_overlay(mesh_dir: &Path, overlay: &Overlay) -> anyhow::Result<()> {
    let dir = conflicts_dir_for(mesh_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("conflicts dir could not be created at {}", dir.display()))?;
    let file = dir.join(OVERLAY_FILE_NAME);
    let tmp = file.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        format!(
            "{}\n",
            serde_json::to_string(overlay).context("overlay serialization")?
        ),
    )
    .with_context(|| format!("overlay tmp could not be written at {}", tmp.display()))?;
    std::fs::rename(&tmp, &file).with_context(|| {
        format!(
            "overlay could not be moved into place at {}",
            file.display()
        )
    })?;
    Ok(())
}

/// Persist the incoming candidate variant into `<namespace>/<id>.json` under
/// the conflicts dir (idempotent — re-saving overwrites identically), then
/// register (or refresh) the id in the overlay. Returns the variant's
/// relative path.
pub fn register_conflict(
    mesh_dir: &Path,
    namespace: &str,
    id: &str,
    update_id: &str,
    reason: &str,
    kind: ConflictKind,
    candidate: &MeshRecord,
) -> anyhow::Result<()> {
    let dir = conflicts_dir_for(mesh_dir);
    let variant_rel = format!("{namespace}/{id}.json");
    let variant_abs = dir.join(&variant_rel);
    if let Some(parent) = variant_abs.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("variant dir could not be created at {}", parent.display()))?;
    }
    std::fs::write(
        &variant_abs,
        format!(
            "{}\n",
            serde_json::to_string(candidate).context("candidate variant serialization")?
        ),
    )
    .with_context(|| {
        format!(
            "candidate variant could not be written to {}",
            variant_abs.display()
        )
    })?;

    let mut overlay = load_overlay(mesh_dir);
    overlay
        .conflicts
        .retain(|e| !(e.namespace == namespace && e.id == id));
    overlay.conflicts.push(ConflictEntry {
        namespace: namespace.to_string(),
        id: id.to_string(),
        update_id: update_id.to_string(),
        reason: reason.to_string(),
        variant_file: variant_rel,
        kind,
        recorded_at: chrono::Utc::now().to_rfc3339(),
    });
    save_overlay(mesh_dir, &overlay)
}

/// Remove a conflict once a later pull resolves it. Idempotent: an id that
/// was never registered is already clear.
pub fn clear_conflict(mesh_dir: &Path, namespace: &str, id: &str) -> anyhow::Result<()> {
    let mut overlay = load_overlay(mesh_dir);
    let before = overlay.conflicts.len();
    overlay
        .conflicts
        .retain(|e| !(e.namespace == namespace && e.id == id));
    if overlay.conflicts.len() == before {
        return Ok(());
    }
    save_overlay(mesh_dir, &overlay)
}

/// The one shared read-path question, core: is this metadata marked under an
/// unresolved conflict overlay? Strict, like `is_archived`: only the literal
/// JSON `metadata.mesh.conflict == true` reads as conflicting; anything else —
/// absent, `false`, wrong type — is not. `is_conflicting` (the row-shaped
/// wrapper) and the store's `export_graph` both call this so the three read
/// surfaces can never disagree.
pub fn is_conflicting_metadata(metadata: &serde_json::Value) -> bool {
    metadata
        .get("mesh")
        .and_then(|m| m.get("conflict"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Row-shaped convenience for the server read sites (`search_memory`,
/// `get_snapshot`).
pub fn is_conflicting(payload: &MemoryPayload) -> bool {
    is_conflicting_metadata(&payload.metadata)
}

/// Status lines for the `mesh status` surface (wired by the orchestrator at
/// merge — slice 6 exports it, slice 7 owns `mesh_service::status_report`).
/// Never fails: a missing/corrupt overlay yields an empty report.
#[allow(dead_code)] // slice-7 seam: mesh_service::status_report wires it
pub fn status_lines(mesh_dir: &Path) -> Vec<String> {
    let overlay = load_overlay(mesh_dir);
    let unresolved: Vec<&ConflictEntry> = overlay
        .conflicts
        .iter()
        .filter(|e| e.kind == ConflictKind::Unresolved)
        .collect();
    let preserved: Vec<&ConflictEntry> = overlay
        .conflicts
        .iter()
        .filter(|e| e.kind == ConflictKind::Preserved)
        .collect();
    let mut lines = Vec::new();
    if unresolved.is_empty() && preserved.is_empty() {
        return lines;
    }
    lines.push(format!(
        "conflicts: {} unresolved{}",
        overlay.unresolved_count(),
        if preserved.is_empty() {
            String::new()
        } else {
            format!(", {} preserved variant(s)", preserved.len())
        }
    ));
    for e in unresolved {
        lines.push(format!(
            "  {}/{}: {} (variant {})",
            e.namespace, e.id, e.reason, e.variant_file
        ));
    }
    for e in preserved {
        lines.push(format!(
            "  {}/{}: {} (preserved variant {})",
            e.namespace, e.id, e.reason, e.variant_file
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ns-mesh-conflict-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn candidate(id: Uuid, text: &str) -> MeshRecord {
        serde_json::from_value(record_value(id, text)).unwrap()
    }

    fn record_value(id: Uuid, text: &str) -> serde_json::Value {
        json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": id.to_string(),
            "namespace": "probe",
            "memory_type": "rule",
            "lifecycle": {
                "state": "active",
                "created_at": "2026-10-11T00:00:00Z",
                "created_by": "node-peer-1",
                "superseded_by": null,
                "archived_at": null,
                "tombstoned_at": null
            },
            "content": {"text": text},
            "metadata": {"eligibility": "shareable", "trust": "untrusted"},
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": "untrusted"
        })
    }

    /// The conflicts dir is a sibling of the checkout, never inside it, so a
    /// git commit can never stage a preserved variant or the overlay.
    #[test]
    fn conflicts_dir_is_a_sibling_of_the_checkout() {
        let mesh_dir = Path::new("/tmp/neurostrata/synaptic-mesh/abc-123");
        let dir = conflicts_dir_for(mesh_dir);
        assert_eq!(
            dir,
            Path::new("/tmp/neurostrata/synaptic-mesh/abc-123.conflicts")
        );
        assert!(
            !dir.starts_with(mesh_dir),
            "must not nest inside the checkout"
        );
    }

    /// Registering a conflict durably preserves the variant, records the id,
    /// and survives a reload; status_lines surfaces it.
    #[test]
    fn register_persists_variant_and_overlay_and_status_surfaces_it() {
        let mesh_dir = scratch("register");
        let id = Uuid::new_v4();
        let cand = candidate(id, "the losing variant");
        let update_id = cand.update_id.to_string();

        register_conflict(
            &mesh_dir,
            "probe",
            &id.to_string(),
            &update_id,
            "incompatible content edits",
            ConflictKind::Unresolved,
            &cand,
        )
        .unwrap();

        // Variant file exists, byte-parseable as the candidate.
        let variant_path = conflicts_dir_for(&mesh_dir)
            .join("probe")
            .join(format!("{id}.json"));
        let saved: MeshRecord =
            serde_json::from_str(&std::fs::read_to_string(&variant_path).unwrap()).unwrap();
        assert_eq!(saved, cand, "variant preserved byte-for-byte");

        // Overlay survives reload.
        let overlay = load_overlay(&mesh_dir);
        assert_eq!(overlay.unresolved_count(), 1);
        assert!(overlay.unresolved_ids("probe").contains(&id.to_string()));

        // status_lines surfaces it.
        let lines = status_lines(&mesh_dir);
        assert!(
            lines.iter().any(|l| l.contains("conflicts: 1 unresolved")),
            "lines: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&id.to_string()) && l.contains("incompatible")),
            "lines: {lines:?}"
        );
    }

    /// Re-saving the same conflict is idempotent: one entry, one variant, no
    /// growth. This is the property the "re-present next pull" cursor
    /// strategy leans on.
    #[test]
    fn re_registering_is_idempotent() {
        let mesh_dir = scratch("idempotent");
        let id = Uuid::new_v4();
        let cand = candidate(id, "still losing");
        let update_id = cand.update_id.to_string();
        for _ in 0..3 {
            register_conflict(
                &mesh_dir,
                "probe",
                &id.to_string(),
                &update_id,
                "two different replacements",
                ConflictKind::Unresolved,
                &cand,
            )
            .unwrap();
        }
        let overlay = load_overlay(&mesh_dir);
        assert_eq!(
            overlay.conflicts.len(),
            1,
            "one entry after three registers"
        );
        assert_eq!(overlay.unresolved_count(), 1);
    }

    /// clear_conflict removes the entry; an id never registered is already
    /// clear (no error).
    #[test]
    fn clear_removes_and_is_idempotent() {
        let mesh_dir = scratch("clear");
        let id = Uuid::new_v4();
        let cand = candidate(id, "resolved later");
        let update_id = cand.update_id.to_string();
        register_conflict(
            &mesh_dir,
            "probe",
            &id.to_string(),
            &update_id,
            "restore + archive",
            ConflictKind::Unresolved,
            &cand,
        )
        .unwrap();
        clear_conflict(&mesh_dir, "probe", &id.to_string()).unwrap();
        assert_eq!(load_overlay(&mesh_dir).unresolved_count(), 0);
        clear_conflict(&mesh_dir, "probe", "never-was").unwrap();
    }

    /// A missing or corrupt overlay reads as empty and fails nothing.
    #[test]
    fn missing_or_corrupt_overlay_reads_empty() {
        let mesh_dir = scratch("missing");
        assert_eq!(load_overlay(&mesh_dir).unresolved_count(), 0);
        assert!(status_lines(&mesh_dir).is_empty());

        std::fs::create_dir_all(conflicts_dir_for(&mesh_dir)).unwrap();
        std::fs::write(
            conflicts_dir_for(&mesh_dir).join(OVERLAY_FILE_NAME),
            "{ not json",
        )
        .unwrap();
        assert_eq!(load_overlay(&mesh_dir).unresolved_count(), 0);
    }

    /// is_conflicting is true only for the typed `metadata.mesh.conflict:
    /// true` — the strict shape that keeps a typo from hiding a row.
    #[test]
    fn is_conflicting_is_true_only_for_the_typed_true() {
        for (metadata, expected) in [
            (json!({"mesh": {"conflict": true}}), true),
            (json!({"mesh": {"conflict": false}}), false),
            (json!({"mesh": {}}), false),
            (json!({}), false),
            (json!({"mesh": {"conflict": "true"}}), false),
            (json!({"mesh": {"conflict": 1}}), false),
            (json!({"mesh": {"conflict": null}}), false),
        ] {
            let p = MemoryPayload {
                content: "x".into(),
                user_id: "u".into(),
                memory_type: "rule".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata,
            };
            assert_eq!(is_conflicting(&p), expected);
        }
    }

    /// Preserved variants (a replacement won) are surfaced in status but do
    /// NOT count toward the unresolved (integration-pausing) total, and do
    /// NOT appear in the suppressible id set.
    #[test]
    fn preserved_kind_surfaces_without_suppressing() {
        let mesh_dir = scratch("preserved");
        let id = Uuid::new_v4();
        let cand = candidate(id, "stale edit");
        let update_id = cand.update_id.to_string();
        register_conflict(
            &mesh_dir,
            "probe",
            &id.to_string(),
            &update_id,
            "supersede + edit; replacement authoritative",
            ConflictKind::Preserved,
            &cand,
        )
        .unwrap();
        let overlay = load_overlay(&mesh_dir);
        assert_eq!(overlay.unresolved_count(), 0);
        assert!(!overlay.unresolved_ids("probe").contains(&id.to_string()));
        let lines = status_lines(&mesh_dir);
        assert!(
            lines.iter().any(|l| l.contains("preserved variant(s)")),
            "lines: {lines:?}"
        );
    }
}
