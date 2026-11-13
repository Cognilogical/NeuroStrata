//! Mesh import — the CONSUME path of the SynapticMesh: mesh files →
//! validated staging → applied store rows.
//!
//! See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`
//! §"Import validation", §"Record format", §"Lifecycle state machine" and
//! §"Concurrent merge policy". Slice 4 owns the fetch/replay plumbing;
//! slice 3 owns the publish side (outbox/worker); slice 6 (this module, at
//! apply time) owns the lifecycle gate and the concurrent-merge table.
//!
//! Shape of the path:
//!
//! 1. the joined mesh is resolved through the project pointer
//!    (`MeshStorage::open`, read-only use of slice 2a's storage API), the
//!    remote is fetched with `storage.pull()` (ff-only),
//! 2. replay operates on COMMITTED content only:
//!    `git diff --name-only <accepted-cursor>..HEAD` after the fast-forward,
//!    with a full-tree fallback (`git ls-files`, still validated file by
//!    file) when the cursor is missing or names a commit the checkout no
//!    longer has,
//! 3. every candidate file is validated (path shape, traversal, symlinks,
//!    executable bits, parseable record, known schema version, served
//!    namespace, shareable memory type, no runtime counters) BEFORE anything
//!    is written,
//! 4. valid rows are applied through the [`VectorStore`] trait: content is
//!    embedded with the [`Embedder`] and upserted under
//!    `(namespace, record.id)`, carrying `metadata.mesh` provenance, so the
//!    row is an ordinary store row that search-shaped reads see without any
//!    special casing,
//! 5. idempotence by `(id, update_id)`: an existing mesh-owned row whose
//!    recorded `update_id` equals the candidate's is skipped. A row WITHOUT
//!    mesh provenance is a private local memory — it is never overwritten,
//!    never deleted; a same-id collision is reported into the ignore list.
//! 6. the accepted-head cursor lives in a scratch JSON file NEXT TO the mesh
//!    checkout (outside it: the mesh repo must stay a clean projection),
//!    written tmp→rename AFTER the whole batch applied, so a crash before
//!    the advance simply replays the same files and the `update_id` skip
//!    makes it a no-op.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context as _;
use serde_json::{json, Value};
use uuid::Uuid;

use super::conflict::{self, ConflictKind};
use super::manifest::Trust;
use super::record::{
    Lifecycle, LifecycleState, MeshRecord, Relationship, RdfBlock, MESH_RECORD_SCHEMA_VERSION,
};
use super::storage::MeshStorage;
use crate::traits::{Embedder, MemoryPayload, VectorStore};

/// The on-disk schema version of the accepted-head cursor file.
pub const IMPORT_CURSOR_SCHEMA_VERSION: &str = "synaptic-mesh-import-cursor/v1";

/// Memory types a mesh record may carry for it to be applied (the v1
/// shareable set, spec §"Sharing boundary"). `structural` records are
/// project-local AST nodes and never travel; anything unknown (future
/// vocabulary) is rejected at import (hard error), never applied, never crash.
const SHAREABLE_MEMORY_TYPES: [&str; 3] = ["rule", "fact", "procedure"];

/// Runtime procedure counters are strictly local (spec §"Record format":
/// "they never leave the node"). A shared record carrying ANY of these keys
/// anywhere in its JSON is ignored: import cannot reset local counters.
const RUNTIME_COUNTER_KEYS: [&str; 4] = [
    "remaining_fires",
    "performance_count",
    "last_performed_at",
    "last_episodic_pointer",
];

/// Conservative per-record byte cap, enforced BEFORE parse (spec §"Import
/// validation": "bounds"). A candidate larger than this is ignored — never
/// parsed, never applied — so an oversized JSON blob cannot force the daemon
/// to materialize an arbitrarily large record.
pub const MAX_RECORD_BYTES: usize = 1024 * 1024; // 1 MiB
/// Per spec §5.5: maximum canonical N-Quads payload size and quad count.
pub const MAX_RDF_QUADS_BYTES: usize = 512 * 1024; // 512 KiB
pub const MAX_RDF_QUADS_COUNT: usize = 1000;

/// RDF validation diagnostic carried out-of-band from validation to the
/// pre-apply gate. This preserves signature verification over the record
/// EXACTLY as it arrived (no metadata mutation before verify_record_signature),
/// while still attaching the diagnostic to the applied record for audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdfDiagnostic {
    pub violations: Vec<String>,
    pub phase: String,
}

/// What one replay pass did.
#[derive(Debug, Clone)]
pub struct ApplyReport {
    /// Records that were upserted into the store this pass.
    pub applied: usize,
    /// Records skipped because an existing mesh-owned row already carries
    /// the same `update_id` (idempotent replay, incl. crash-before-cursor).
    pub skipped_update_id: usize,
    /// `"<rel path>: <reason>"` for every rejected candidate. A rejected
    /// candidate is intentionally not applied and never crashes the pass.
    pub ignored: Vec<String>,
    /// True when the pass worked from the full tracked tree instead of a
    /// diff (no cursor yet, unusable cursor, or divergent/rewritten history).
    pub full_tree: bool,
    /// The commit accepted when the pass finished (empty when the checkout
    /// had no commits at all).
    pub head: String,
    /// The ids of the rows written this pass.
    pub applied_ids: Vec<String>,
    /// Records whose incoming candidate was an UNRESOLVED concurrent merge
    /// conflict (per §"Concurrent merge policy"): the candidate is preserved
    /// in the conflict overlay, the id is suppressed from the effective
    /// view, and the accepted-head cursor is NOT advanced (integration is
    /// paused at those ids until a team-side resolution lands).
    pub conflicted: usize,
    /// The ids registered as unresolved conflicts this pass.
    pub conflicted_ids: Vec<String>,
    /// `archive + content edit` combinations applied this pass — the edited
    /// content was kept but the record stayed archived. Reported (spec
    /// §"Concurrent merge policy": "report combination"), never resolved by
    /// timestamp.
    pub combinations: Vec<String>,
    /// Records whose Ed25519 signature verified cleanly.
    pub verified_signed: usize,
    /// Records whose Ed25519 signature verified ONLY over the RDF
    /// payload (the legacy signing scheme): the RDF block is a lossy
    /// projection of the record, so these records applied without
    /// full-record verification and are counted apart from
    /// `verified_signed`.
    pub rdf_only_verified: usize,
    /// Records accepted without a signature (warn-first Phase 1).
    pub unsigned_accepted: usize,
    /// Records rejected due to an invalid Ed25519 signature or DID/pubkey mismatch.
    pub signature_rejected: usize,
    /// Records rejected because `signer_did` is not in a non-empty `manifest.signers`.
    pub unauthorized_signer: usize,
}

/// Where the accepted-head cursor lives for a mesh checkout: a sibling
/// scratch JSON OUTSIDE the git checkout. The mesh repo stays a clean
/// projection; the cursor is daemon state, not team state.
pub fn cursor_file(mesh_root: &Path) -> PathBuf {
    let name = mesh_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("mesh");
    mesh_root
        .parent()
        .unwrap_or(mesh_root)
        .join(format!(".{name}.import-cursor.json"))
}

/// The mesh wire surface for `neurostrata_synaptic_mesh_pull`: resolve the
/// joined mesh through the project pointer, fetch (ff-only), validate+replay
/// the changed committed files into the store, advance the cursor.
pub async fn handle_mesh_pull(
    arguments: &Value,
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
) -> String {
    // The same per-request resolution the other mesh tools apply (Task 2 /
    // J6): canonicalize the caller's root; never consume the verbatim string.
    let project_root = match super::mesh_service::resolve_mesh_project_root(arguments) {
        Ok(root) => root,
        Err(e) => return e,
    };

    let pointer = match MeshStorage::read_pointer(&project_root) {
        Ok(p) => p,
        Err(e) => return format!("error: pull failed — {:#}", e),
    };
    let Some((pointer_mesh_id, mesh_dir, _remote)) = pointer else {
        return format!(
            "no mesh joined — no pointer at {}",
            project_root
                .join(super::storage::POINTER_DIR_NAME)
                .join(super::storage::POINTER_FILE_NAME)
                .display()
        );
    };
    let mesh = match MeshStorage::open(&mesh_dir) {
        Ok(m) => m,
        Err(e) => return format!("error: pull failed — {:#}", e),
    };
    let mesh_id = mesh.manifest().mesh_id.to_string();
    if mesh.manifest().mesh_id != pointer_mesh_id {
        // Same parity MeshService::joined() enforces for the mesh surface.
        return format!(
            "no mesh joined — pointer names mesh {} but {} holds {mesh_id}",
            pointer_mesh_id,
            mesh_dir.display()
        );
    }

    if mesh
        .manifest()
        .remote
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .is_none()
    {
        return format!(
            "error: pull failed — mesh {mesh_id} has no remote configured; join a mesh with a remote before pull/push"
        );
    }
    if matches!(remote_refs_present(mesh.root()), Some(false)) {
        return format!(
            "pulled mesh {mesh_id}: nothing on the remote yet — records land with the mesh-worker (slice 3)"
        );
    }
    if let Err(e) = mesh.pull() {
        return format!("error: pull failed — {:#}", e);
    }

    let served = served_namespaces(arguments, &mesh, &store).await;
    if served.is_empty() {
        return format!(
            "error: pull failed — mesh {mesh_id} has no joined namespace to serve records into"
        );
    }

    match apply_changed(&mesh, &served, store, emb).await {
        Ok(report) => format_apply_report(&mesh_id, &report),
        Err(e) => format!("error: replay failed — {:#}", e),
    }
}

/// Which namespaces this pull serves records into: the explicit
/// `namespace` argument (resolved through the same case-folding the rest of
/// the tool surface uses), or every namespace the mesh manifest joined.
async fn served_namespaces(
    arguments: &Value,
    mesh: &MeshStorage,
    store: &std::sync::Arc<dyn VectorStore>,
) -> Vec<String> {
    let mut served: Vec<String> = vec![];
    match arguments.get("namespace") {
        Some(Value::String(s)) if !s.trim().is_empty() => {
            served.push(crate::server::resolve_namespace(store, s).await);
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    if !s.trim().is_empty() {
                        served.push(crate::server::resolve_namespace(store, s).await);
                    }
                }
            }
        }
        _ => served.extend(mesh.manifest().namespaces.iter().cloned()),
    }
    served.retain(|s| !s.is_empty());
    served.sort();
    served.dedup();
    served
}

/// One pull/apply summary in the mesh surface's prose.
fn format_apply_report(mesh_id: &str, report: &ApplyReport) -> String {
    let mut out = format!(
        "pulled mesh {mesh_id}: applied {}, skipped {}, ignored {}{}",
        report.applied,
        report.skipped_update_id,
        report.ignored.len(),
        if report.conflicted > 0 {
            format!(", {} conflict(s)", report.conflicted)
        } else {
            String::new()
        }
    );
    if !report.applied_ids.is_empty() {
        out.push_str(&format!(" ids {}", report.applied_ids.join(", ")));
    }
    if !report.conflicted_ids.is_empty() {
        out.push_str(&format!(" conflicted {}", report.conflicted_ids.join(", ")));
    }
    if report.full_tree {
        out.push_str(" (full-tree reconciliation — no usable accepted-head cursor)");
    }
    if report.verified_signed > 0
        || report.rdf_only_verified > 0
        || report.signature_rejected > 0
        || report.unauthorized_signer > 0
    {
        out.push_str(&format!(
            ", signed {} / signature-rejected {} / unauthorized {}",
            report.verified_signed, report.signature_rejected, report.unauthorized_signer
        ));
        if report.rdf_only_verified > 0 {
            out.push_str(&format!(", rdf-only-signed {}", report.rdf_only_verified));
        }
    }
    let head_len = report.head.len().min(7);
    if head_len > 0 {
        out.push_str(&format!(", head {}", &report.head[..head_len]));
    }
    for line in report.ignored.iter().take(5) {
        out.push_str(&format!("\n  ignored: {line}"));
    }
    if report.ignored.len() > 5 {
        out.push_str(&format!("\n  … {} more ignored", report.ignored.len() - 5));
    }
    for line in report.combinations.iter() {
        out.push_str(&format!("\n  combined: {line}"));
    }
    if report.conflicted > 0 {
        out.push_str(
            "\n  (integration paused at conflicting ids — resolve in the mesh, then pull again)",
        );
    }
    out
}

/// Does the configured remote have refs yet? `Some(false)` = reachable but
/// EMPTY (a normal "team inbox not yet filled" state, answered with a
/// courtesy instead of a merge error); `Some(true)`/`None` = pull will do
/// real work or will fail loudly. Mirrors mesh_service's private probe; the
/// behavior cannot drift because both feeds `storage.pull()`.
fn remote_refs_present(root: &Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .args(["ls-remote", "--refs", "origin", "HEAD"])
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.is_empty())
}

/// The reusable innards: diff the committed content against the accepted
/// cursor, validate every candidate, apply the survivors through the store
/// traits, then advance the cursor. The caller must have completed its fetch
/// already; this function works on whatever HEAD currently is. Callable from
/// tests without any wire involvement.
pub async fn apply_changed(
    mesh: &MeshStorage,
    served: &[String],
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
) -> anyhow::Result<ApplyReport> {
    let root = mesh.root();
    let mesh_id = mesh.manifest().mesh_id.to_string();
    let manifest_epoch = mesh.manifest().epoch;

    // ONCE per pass, before ANY candidate read: the whole tree is refused
    // when the checkout is dirty (spec §Recovery: "pause sync, do not touch
    // local store"). Dirty tracked files would serve UNCOMMITTED content —
    // the cursor-diff path is protected in practice (ff-only pull never
    // touches dirty files) but the full-tree fallback is not, and identity
    // would not self-heal until a later commit bumps update_id.
    let dirty = dirty_checkout_paths(root)?;
    if !dirty.is_empty() {
        anyhow::bail!(
            "dirty mesh checkout — sync paused, nothing read or written; dirty: {}. \
             To recover: commit or restore the mesh checkout, then pull again",
            dirty.join("; ")
        );
    }

    let (paths, full_tree) = {
        let cursor = load_cursor(root, &mesh_id);
        changed_paths(root, cursor.as_deref())?
    };

    let mut report = ApplyReport {
        applied: 0,
        skipped_update_id: 0,
        ignored: Vec::new(),
        full_tree,
        head: String::new(),
        applied_ids: Vec::new(),
        conflicted: 0,
        conflicted_ids: Vec::new(),
        combinations: Vec::new(),
        verified_signed: 0,
        rdf_only_verified: 0,
        unsigned_accepted: 0,
        signature_rejected: 0,
        unauthorized_signer: 0,
    };

    // Divergence detection (documented cursor strategy, §"Concurrent merge
    // policy"): the diff path (ff-only pull, a known linear ancestor) can
    // never be concurrent, so `concurrent` is false there. The full-tree
    // path is exactly the spec's "divergent ancestry / rewritten history"
    // trigger, where a local row's `update_id` may not be an ancestor of
    // the candidate — so same-state divergent changes are treated as
    // concurrent there. Crash-before-cursor replay (full-tree, identical
    // update_id) is unaffected: the update_id skip fires before the merge
    // table is consulted.
    let concurrent = full_tree;

    // Program Task 3: the trust stamp for imported procedures comes
    // from THIS mesh's manifest policy — read here, once per pass, and
    // threaded down to the apply. The sender's record is never the
    // stamp source. The HEAD signature state rides along for
    // provenance recording (DR-9: metadata only, never a gate).
    let import_trust = mesh.manifest().policy.imported_procedures_trust;
    let head_signature = head_commit_signature_state(root);

    // The `(namespace, id)` pairs already presented as per-record `.json`
    // candidates this pass. A record that arrived BOTH as a `.json` file
    // and inside a `.bundle.zst` is applied exactly once — the `.json`
    // candidate path is authoritative; the bundle copy is the duplicate.
    // (`validate_record` enforces id == file stem, so the pair key aligns
    // with the bundle comparison below.)
    let json_candidate_ids: std::collections::HashSet<(String, String)> = paths
        .iter()
        .filter_map(|rel| classify_path(rel, served).ok().flatten())
        .map(|c| (c.namespace, c.file_stem))
        .collect();

    // Bundle/RDF-patch artifacts are scanned before the candidate loop so a
    // compressed drain bundle or an rdfpatch sidecar is accounted for on
    // every replay pass (fail-soft; see `scan_bundles_and_patches`). The
    // decoded bundle records are RETAINED: a record carried only inside a
    // `.bundle.zst` is a replay candidate like any other, verified and
    // applied below.
    let bundled_records = scan_bundles_and_patches(root);

    for rel in paths {
        let candidate = match classify_path(&rel, served) {
            Ok(Some(c)) => c,
            // Not a record path at all (manifest.json et al.) — not a
            // candidate, silently tracked content.
            Ok(None) => continue,
            Err(reason) => {
                report.ignored.push(format!("{rel}: {reason}"));
                continue;
            }
        };
        let raw = match validated_read(root, &rel) {
            Ok(r) => r,
            Err(reason) => {
                report.ignored.push(format!("{rel}: {reason}"));
                continue;
            }
        };
        let (mut record, signature_disposition) = match pre_apply_gate(
            validate_record(&candidate, served, &raw),
            manifest_epoch,
            &mesh.manifest().signers,
            &rel,
            &mut report,
        ) {
            Some(r) => r,
            None => continue,
        };
        // RDF patch sidecar attachment: a sibling `<id>.rdfpatch` next to
        // the `.json` record is parsed (fail-soft) and attached AFTER
        // signature verification, so canonical signing bytes over the
        // `.json` record (patch is `skip_serializing_if = "Option::is_none"`)
        // are unaffected.
        if let Some(stream) = load_record_patch(&root.join(&rel)) {
            record.patch = Some(stream);
        }
        match apply_one(
            root,
            &mesh_id,
            record,
            store.clone(),
            emb.clone(),
            concurrent,
            import_trust,
            head_signature,
            &signature_disposition,
            false,
        )
        .await
        {
            Disposition::Applied => {
                report.applied += 1;
                report.applied_ids.push(candidate.file_stem.clone());
            }
            Disposition::AppliedCombination(reason) => {
                report.applied += 1;
                report.applied_ids.push(candidate.file_stem.clone());
                report.combinations.push(format!("{rel}: {reason}"));
            }
            Disposition::Skipped => report.skipped_update_id += 1,
            Disposition::Ignored(reason) => {
                report.ignored.push(format!("{rel}: {reason}"));
            }
            Disposition::Conflicted(id) => {
                report.conflicted += 1;
                report.conflicted_ids.push(id);
            }
        }
    }

    // Bundle-only records: everything `scan_bundles_and_patches` decoded
    // whose `(namespace, id)` did NOT arrive as a `.json` candidate this
    // pass. Identical gates as the `.json` path, by construction: both
    // routes pass their validation result through `pre_apply_gate`
    // (epoch gate, then signature, then disposition) before
    // `apply_one` — each route evaluates its own validator first
    // (`validate_record` on the `.json` route, `validate_bundle_record`
    // here, both over `validate_parsed_record`) and passes the `Result`
    // in — and the bundle validation is the portable subset of
    // `validate_record` (see `validate_bundle_record` — a bundle record
    // has no path and no file stem, so the two path-dependent checks —
    // path-namespace parity and id == file-stem — do not transfer).
    // The dedup skip here is keyed on the path-derived `(namespace,
    // file-stem)` pair collected before validation, and content is NOT
    // compared: a bundle record under a key whose `.json` candidate
    // reached any disposition in that route — applied, ignored,
    // skipped, or conflicted — inherits that disposition for the pass
    // and is not separately evaluated or reported. Rejections of
    // decoded records — including an unserved namespace, which the
    // validation step now refuses — land in `report.ignored` like any
    // other candidate, and the report counts the Disposition like any
    // other candidate; a bundle that fails to decode at all (corrupt
    // stream, malformed element, aggregate decompression cap) is
    // logged and skipped wholesale by the scan — pre-existing
    // behaviour, no report entry.
    for (bundle_path, record) in bundled_records {
        let id = record.id.to_string();
        if json_candidate_ids.contains(&(record.namespace.clone(), id.clone())) {
            continue;
        }
        let rel = format!("{} ({})", bundle_path.display(), record.id);
        let (record, signature_disposition) = match pre_apply_gate(
            validate_bundle_record(record, served),
            manifest_epoch,
            &mesh.manifest().signers,
            &rel,
            &mut report,
        ) {
            Some(r) => r,
            None => continue,
        };
        match apply_one(
            root,
            &mesh_id,
            record,
            store.clone(),
            emb.clone(),
            concurrent,
            import_trust,
            head_signature,
            &signature_disposition,
            true,
        )
        .await
        {
            Disposition::Applied => {
                report.applied += 1;
                report.applied_ids.push(id);
            }
            Disposition::AppliedCombination(reason) => {
                report.applied += 1;
                report.applied_ids.push(id);
                report.combinations.push(format!("{rel}: {reason}"));
            }
            Disposition::Skipped => report.skipped_update_id += 1,
            Disposition::Ignored(reason) => {
                report.ignored.push(format!("{rel}: {reason}"));
            }
            Disposition::Conflicted(conflict_id) => {
                report.conflicted += 1;
                report.conflicted_ids.push(conflict_id);
            }
        }
    }

    // Accept HEAD only after the whole batch was walked AND no unresolved
    // conflict paused integration. A crash before this line leaves the old
    // cursor: the next pass replays the same files and the update_id skip
    // (plus the private-row guard) makes that a no-op. A batch with an
    // unresolved conflict deliberately does NOT advance the cursor — the
    // conflicted ids are re-presented (and re-preserved idempotently) on the
    // next pull, so a team-side resolution is picked up while a still-open
    // conflict is never silently dropped.
    report.head = current_head(root)?.unwrap_or_default();
    if !report.head.is_empty() && report.conflicted == 0 {
        write_cursor(root, &mesh_id, &report.head)?;
    }
    Ok(report)
}

/// Read the accepted-head cursor for this checkout, iff it is intact, names
/// this mesh's schema, and belongs to this mesh.
pub fn load_cursor(mesh_root: &Path, mesh_id: &str) -> Option<String> {
    let path = cursor_file(mesh_root);
    if !path.exists() {
        return None;
    }
    // A cursor that exists but cannot be READ or PARSED is corrupt state,
    // not absence: warn loudly (an operator must see it) and fall through
    // to the full-tree reconciliation the None caller applies.
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(
                "corrupt or unreadable mesh cursor at {}: {} — falling back to full-tree reconciliation",
                path.display(),
                e
            );
            return None;
        }
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!(
                "corrupt or unreadable mesh cursor at {}: {} — falling back to full-tree reconciliation",
                path.display(),
                e
            );
            return None;
        }
    };
    if doc["schema_version"].as_str() != Some(IMPORT_CURSOR_SCHEMA_VERSION) {
        return None;
    }
    if doc["mesh_id"].as_str() != Some(mesh_id) {
        // A repointed mesh dir must not inherit another mesh's cursor; the
        // full-tree fallback below is validated like any other pass.
        return None;
    }
    let head = doc["accepted_head"].as_str()?;
    if head.trim().is_empty() {
        return None;
    }
    Some(head.trim().to_string())
}

/// Atomically persist the cursor: write tmp → rename, after the batch.
pub fn write_cursor(mesh_root: &Path, mesh_id: &str, head: &str) -> anyhow::Result<()> {
    let file = cursor_file(mesh_root);
    let doc = json!({
        "schema_version": IMPORT_CURSOR_SCHEMA_VERSION,
        "mesh_id": mesh_id,
        "accepted_head": head,
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    let tmp = file.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        format!(
            "{}\n",
            serde_json::to_string(&doc).context("cursor serialization")?
        ),
    )
    .with_context(|| format!("cursor tmp could not be written to {}", tmp.display()))?;
    std::fs::rename(&tmp, &file)
        .with_context(|| format!("cursor could not be moved into place at {}", file.display()))?;
    Ok(())
}

/// HEAD of the checkout, `Ok(None)` while the branch is unborn.
pub fn current_head(mesh_root: &Path) -> anyhow::Result<Option<String>> {
    let out = git(mesh_root, &["rev-parse", "--verify", "-q", "HEAD"])?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Which committed files a replay pass must examine, and whether it had to
/// fall back to the full tree.
pub fn changed_paths(
    mesh_root: &Path,
    cursor: Option<&str>,
) -> anyhow::Result<(Vec<String>, bool)> {
    let head = current_head(mesh_root)?;
    let Some(head) = head else {
        // Unborn branch: nothing committed yet; an empty full-tree pass.
        return Ok((tracked_paths(mesh_root)?, true));
    };
    if let Some(cursor) = cursor {
        // The cursor is usable only while the checkout still contains that
        // commit (rewritten or divergent history falls through to the
        // validated full-tree reconciliation; nothing is blindly applied).
        let approach = format!("{cursor}^{{commit}}");
        if run_ok(mesh_root, &["cat-file", "-e", &approach]).is_ok() {
            if let Ok(out) = run_ok(
                mesh_root,
                &["diff", "--name-only", "-z", &format!("{cursor}..{head}")],
            ) {
                return Ok((split_nul(&out.stdout), false));
            }
        }
    }
    Ok((tracked_paths(mesh_root)?, true))
}

/// Committed content via the index. apply_changed always runs after a
/// ff-merge (index == HEAD) or against a clean checkout, so the index never
/// outlives what is committed here.
fn tracked_paths(mesh_root: &Path) -> anyhow::Result<Vec<String>> {
    let out = run_ok(mesh_root, &["ls-files", "-z"])?;
    Ok(split_nul(&out.stdout))
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// One tracked path, classified: `Ok(None)` = not a record path at all
/// (`manifest.json`, tooling files) and silently skipped; `Ok(Some)` = a
/// well-shaped record path inside a served namespace; `Err(reason)` = a
/// record-shaped path that must be reported and ignored.
pub fn classify_path(rel: &str, served: &[String]) -> Result<Option<Candidate>, String> {
    if rel.starts_with('/') {
        return Err("path traversal (absolute path)".to_string());
    }
    let components: Vec<&str> = rel.split('/').collect();
    if components.iter().any(|c| *c == ".." || *c == ".") {
        return Err("path traversal (..)".to_string());
    }
    if components.len() != 3 {
        // manifest.json, helper files etc. — tracked but not a record.
        return Ok(None);
    }
    let (ns, dir, file) = (components[0], components[1], components[2]);
    if components.iter().any(|c| c.is_empty()) {
        return Err("malformed record path (empty component)".to_string());
    }
    match dir {
        "memories" => {}
        "procedures" => {}
        // Tracked but not a record shape.
        _ => return Ok(None),
    }
    let Some(file_stem) = file.strip_suffix(".json") else {
        return Ok(None);
    };
    if file_stem.is_empty() {
        return Err("empty record file name".to_string());
    }
    if !served.iter().any(|s| s == ns) {
        return Err(format!(
            "foreign-namespace {ns} (served: {})",
            served.join(", ")
        ));
    }
    Ok(Some(Candidate {
        namespace: ns.to_string(),
        file_stem: file_stem.to_string(),
    }))
}

/// One well-shaped record path ready for validation + apply.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub namespace: String,
    pub file_stem: String,
}

/// Read one mesh file SAFELY: no symlinks, no path escaping the mesh root,
/// no executable payload bits. Returns the file text or an ignore reason
/// (never an anyhow error — a hostile file is an ignored candidate, not a
/// failed pass).
pub fn validated_read(mesh_root: &Path, rel: &str) -> Result<String, String> {
    let path = mesh_root.join(rel);
    let sym = std::fs::symlink_metadata(&path).map_err(|e| format!("unreadable ({e})"))?;
    if sym.file_type().is_symlink() {
        return Err("symlink (a mesh record must be plain committed content)".to_string());
    }
    // A symlinked PARENT directory would smuggle the file out of the mesh
    // root without the file itself being a symlink, so canonicalize both.
    let canon_root =
        std::fs::canonicalize(mesh_root).map_err(|e| format!("mesh root unreadable ({e})"))?;
    let canon = std::fs::canonicalize(&path).map_err(|e| format!("unreadable ({e})"))?;
    if !canon.starts_with(&canon_root) {
        return Err("path escapes the mesh directory".to_string());
    }
    let plain = std::fs::metadata(&path).map_err(|e| format!("unreadable ({e})"))?;
    if has_exec_bit(&plain) {
        return Err("executable payload bits (a mesh record is data, not code)".to_string());
    }
    std::fs::read_to_string(&path).map_err(|e| format!("unreadable ({e})"))
}

#[cfg(unix)]
fn has_exec_bit(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn has_exec_bit(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Parse + validate one `.json` record against the pinned contract: known
/// schema version, served namespace parity with its path, shareable memory
/// type, id/filename agreement, no runtime counter keys. `Ok((record, diagnostic))`
/// is apt for apply; `Err(reason)` is an ignore entry.
pub fn validate_record(
    candidate: &Candidate,
    served: &[String],
    raw: &str,
) -> Result<(MeshRecord, Option<RdfDiagnostic>), String> {
    if raw.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "record is {} bytes, over the {}-byte per-record cap (MAX_RECORD_BYTES)",
            raw.len(),
            MAX_RECORD_BYTES
        ));
    }
    let record: MeshRecord =
        serde_json::from_str(raw).map_err(|e| format!("unparseable MeshRecord ({e})"))?;
    validate_parsed_record(record, served, Some(candidate))
}

/// Validate one bundle-carried record: the portable subset of
/// [`validate_record`] — every check that does not need a file. A bundle
/// record arrives as a deserialized [`MeshRecord`] with no path and no
/// file stem, so the id == file-stem check (and the path-namespace parity
/// check it rides with) cannot transfer; synthesising a stem from the
/// record id would make that check tautological, so it is excluded, not
/// faked. The per-record byte cap is enforced over the record's
/// serialized JSON form, the bundle carrier's analogue of the raw
/// `.json` bytes. Everything else — schema version, served-namespace
/// parity, shareable memory type, runtime-counter-key rejection, the
/// warn-and-apply canonical-RDF diagnostic, and the secret scan over
/// content and metadata — is the shared [`validate_parsed_record`], so
/// the two routes cannot drift apart.
fn validate_bundle_record(
    record: MeshRecord,
    served: &[String],
) -> Result<(MeshRecord, Option<RdfDiagnostic>), String> {
    let serialized =
        serde_json::to_string(&record).map_err(|e| format!("unparseable MeshRecord ({e})"))?;
    if serialized.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "record is {} bytes, over the {}-byte per-record cap (MAX_RECORD_BYTES)",
            serialized.len(),
            MAX_RECORD_BYTES
        ));
    }
    validate_parsed_record(record, served, None)
}

/// The shared validation core for an already-parsed record. `candidate`
/// is `Some` only on the `.json` route, where the record arrived as a
/// file: those callers additionally get the path-namespace parity check
/// and the id == file-stem check. `None` (the bundle route) runs every
/// remaining check unchanged.
fn validate_parsed_record(
    record: MeshRecord,
    served: &[String],
    candidate: Option<&Candidate>,
) -> Result<(MeshRecord, Option<RdfDiagnostic>), String> {
    if record.schema_version != MESH_RECORD_SCHEMA_VERSION {
        return Err(format!(
            "unknown schema_version {} (supported: {})",
            record.schema_version, MESH_RECORD_SCHEMA_VERSION
        ));
    }
    if !served.iter().any(|s| s == &record.namespace) {
        return Err(format!(
            "record namespace {} is not served (served: {})",
            record.namespace,
            served.join(", ")
        ));
    }
    if let Some(candidate) = candidate {
        if record.namespace != candidate.namespace {
            return Err(format!(
                "record namespace {} does not match its path namespace {}",
                record.namespace, candidate.namespace
            ));
        }
    }
    if !SHAREABLE_MEMORY_TYPES.contains(&record.memory_type.as_str()) {
        return Err(format!(
            "memory_type {} is not shareable (shareable: {})",
            record.memory_type,
            SHAREABLE_MEMORY_TYPES.join(", ")
        ));
    }
    if let Some(candidate) = candidate {
        if record.id.to_string() != candidate.file_stem {
            return Err(format!(
                "record id {} does not match its filename {}",
                record.id, candidate.file_stem
            ));
        }
    }
    let whole =
        serde_json::to_value(&record).map_err(|e| format!("unparseable MeshRecord ({e})"))?;
    if contains_counter_key(&whole) {
        return Err(
            "runtime procedure counters are strictly local; a shared record must not carry them"
                .to_string(),
        );
    }
    // Canonical RDF validation (§5.2f, Phase 1: warn-and-apply-fallback).
    // If the record carries an rdf block, verify its canonical_hash,
    // predicate allowlist, and subject agreement. On violation, report it
    // as a diagnostic but do NOT refuse the record — Phase 1 accepts the
    // valid JSON envelope and emits the violation for audit. The diagnostic
    // is carried OUT OF BAND (not attached to `record.metadata`) so that
    // signature verification over the canonical bytes runs over the record
    // exactly as it arrived — no metadata mutation before verify_record_signature.
    let mut diagnostic = None;
    if let Some(ref rdf_block) = record.rdf {
        if let Err(violations) = crate::rdf::dataset_canonical::validate_canonical_rdf(&record, rdf_block) {
            // Phase 1: emit diagnostic (spec §5.3 warn-first); record is still
            // applied. The violation is carried out-of-band for later attachment.
            tracing::warn!(
                target: "neurostrata::import::rdf",
                "canonical RDF violation on {} (ns={}) — Phase 1 apply-with-diagnostic: {}",
                record.id, record.namespace,
                violations.join("; ")
            );
            diagnostic = Some(RdfDiagnostic {
                violations,
                phase: "1".to_string(),
            });
        }
    }
    // Secret scan (J15, import leg): the same scanner every local write
    // path answers to, run over the record's content and metadata before
    // anything is applied. The refusal names the scan category and
    // which half of the record matched, never the secret itself — and
    // never the scanner's raw location either: locations are built
    // from the record's own metadata key names, which are
    // sender-controlled and could carry secret-shaped text of their
    // own into the operator-visible report (fix round, M-1).
    // Scan metadata AS IT WOULD BE after diagnostic attachment, so secret
    // scan coverage of diagnostic text is preserved.
    let mut metadata_for_scan = record.metadata.clone();
    if let Some(ref diag) = diagnostic {
        if let Some(obj) = metadata_for_scan.as_object_mut() {
            obj.insert(
                "rdf_diagnostic_violations".to_string(),
                json!(diag.violations),
            );
            obj.insert("rdf_diagnostic_phase".to_string(), json!(diag.phase));
        }
    }
    let content_scan_text = serde_json::to_string(&record.content)
        .map_err(|e| format!("unparseable MeshRecord content ({e})"))?;
    if let Some(rejection) =
        crate::secrets::scan_entry_point(&content_scan_text, &metadata_for_scan, "mesh_import")
    {
        let surface = if rejection.location.starts_with("mesh_import.metadata") {
            "record metadata"
        } else {
            "record content"
        };
        return Err(format!(
            "secret scan rejected the record: {} detected in {surface}",
            rejection.category
        ));
    }
    Ok((record, diagnostic))
}

/// The epoch gate (spec §"Deletion & erasure" / §"Import validation"): a
/// record's `origin_mesh_epoch` is compared against the mesh manifest's
/// current `epoch` at apply time.
///
/// - older → quarantine: the record was born before a coordinated history
///   rewrite (a `filter-repo` purge) and must not be replayed into the new
///   epoch. Refused, named in the ignore report — NOT a conflict.
/// - equal → apply (today's behavior).
/// - newer → refused: a single-epoch model cannot produce a record from the
///   future, so a validator refuses what it does not understand rather than
///   silently accepting it.
pub fn epoch_gate(candidate_epoch: u32, manifest_epoch: u32) -> Result<(), String> {
    use crate::formal_kernel::epoch_gate_decision;
    use crate::formal_kernel::KernelEpochDecision;
    match epoch_gate_decision(candidate_epoch, manifest_epoch) {
        KernelEpochDecision::Apply => Ok(()),
        KernelEpochDecision::QuarantineOld => Err(format!(
            "old-epoch record (origin_mesh_epoch {candidate_epoch} < manifest epoch {manifest_epoch}); quarantined, not replayed"
        )),
        KernelEpochDecision::RefuseFuture => Err(format!(
            "future-epoch record (origin_mesh_epoch {candidate_epoch} > manifest epoch {manifest_epoch}); refused, not understood"
        )),
    }
}

/// The ONE pre-apply gate both record-entry routes pass through, in the
/// `.json` path's original order: validation (the caller's
/// `validate_record` / `validate_bundle_record` result), then
/// [`epoch_gate`], then `verify_record_signature`. No record reaches
/// `apply_one` without returning `Some` from here.
///
/// `Some((record, disposition))` = every gate passed; the signature
/// counters for the record have been recorded on `report`, and the
/// [`SignatureDisposition`] is returned alongside the record so
/// `apply_one` can stamp the stored provenance with the verification
/// outcome that actually occurred. `None` = rejected: the rejection has
/// already been recorded on `report` (an `ignored` entry under `rel`,
/// plus the signature counters when the signature stage was reached),
/// the record is NOT applied, and the caller must not count it as
/// applied.
fn pre_apply_gate(
    validated: Result<(MeshRecord, Option<RdfDiagnostic>), String>,
    manifest_epoch: u32,
    signers: &[super::manifest::SignerEntry],
    rel: &str,
    report: &mut ApplyReport,
) -> Option<(MeshRecord, SignatureDisposition)> {
    let (mut record, rdf_diagnostic) = match validated {
        Ok((r, d)) => (r, d),
        Err(reason) => {
            report.ignored.push(format!("{rel}: {reason}"));
            return None;
        }
    };

    if let Err(reason) = epoch_gate(record.origin_mesh_epoch, manifest_epoch) {
        report.ignored.push(format!("{rel}: {reason}"));
        return None;
    }
    let disposition = match verify_record_signature(&record, signers) {
        SignatureDisposition::Verified => {
            report.verified_signed += 1;
            SignatureDisposition::Verified
        }
        SignatureDisposition::VerifiedRdfOnly => {
            report.rdf_only_verified += 1;
            SignatureDisposition::VerifiedRdfOnly
        }
        SignatureDisposition::Unsigned => {
            report.unsigned_accepted += 1;
            SignatureDisposition::Unsigned
        }
        SignatureDisposition::Invalid(reason) => {
            report.signature_rejected += 1;
            report.ignored.push(format!("{rel}: {reason}"));
            return None;
        }
        SignatureDisposition::Unauthorized(reason) => {
            report.unauthorized_signer += 1;
            report.ignored.push(format!("{rel}: {reason}"));
            return None;
        }
    };
    // Attach the RDF diagnostic AFTER signature verification, so the
    // canonical signing bytes used for verification are over the record
    // EXACTLY as it arrived — no metadata mutation beforehand.
    if let Some(diag) = rdf_diagnostic {
        if let Some(obj) = record.metadata.as_object_mut() {
            obj.insert(
                "rdf_diagnostic_violations".to_string(),
                json!(diag.violations),
            );
            obj.insert("rdf_diagnostic_phase".to_string(), json!(diag.phase));
        }
    }
    Some((record, disposition))
}

/// Does this JSON anywhere (objects recursively) carry a runtime counter
/// key? `remaining_fires_initial` is an initial budget, not a counter, and
/// matches none of the keys exactly.
pub fn contains_counter_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys()
                .any(|k| RUNTIME_COUNTER_KEYS.contains(&k.as_str()))
                || map.values().any(contains_counter_key)
        }
        Value::Array(items) => items.iter().any(contains_counter_key),
        _ => false,
    }
}

/// The embeddable text of a record's content: its `text` field when the
/// content carries one, else the serialized content itself.
pub fn content_text(record: &MeshRecord) -> String {
    match record.content.get("text").and_then(Value::as_str) {
        Some(text) => text.to_string(),
        None => record.content.to_string(),
    }
}

/// The `update_id` a store row's metadata.mesh provenance carries, when it
/// is provenance at all (absent = private local memory).
pub fn mesh_update_id_of(metadata: &Value) -> Option<String> {
    metadata
        .get("mesh")
        .and_then(Value::as_object)
        .and_then(|m| m.get("update_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The unix-millisecond timestamp encoded in a UUIDv7 id's timestamp
/// prefix, or `None` for any other UUID version. This is the revision
/// ordering signal for the tracked-stale-bundle guard (issue #44, Gate
/// G2 option (a)): v7 ids are minted per accepted change, so their
/// prefixes order revisions — but only v7 ids carry a timestamp at all,
/// so every non-v7 id (on either side) leaves the guard inert.
fn update_id_v7_millis(id: &Uuid) -> Option<u64> {
    if id.get_version() != Some(uuid::Version::SortRand) {
        return None;
    }
    let (secs, subsec_nanos) = id.get_timestamp()?.to_unix();
    secs.checked_mul(1_000)?
        .checked_add(u64::from(subsec_nanos / 1_000_000))
}

/// What one candidate's apply did.
#[derive(Debug, Clone, PartialEq)]
enum Disposition {
    Applied,
    /// `archive + edit` combination applied (edited content kept, record
    /// archived). Carries the reported combination reason.
    AppliedCombination(String),
    Skipped,
    Ignored(String),
    /// Unresolved concurrent conflict: the candidate was preserved in the
    /// overlay, the id was suppressed, integration is paused. Carries the
    /// conflicting id.
    Conflicted(String),
}

/// What the merge table decided for one candidate against the current row.
///
/// The table is `docs/…/2026-10-10-synaptic-mesh-design.md`
/// §"Concurrent merge policy". The lifecycle gate [`LifecycleState::can_transition_to`]
/// supplies the *retirement* rules (tombstone wins, superseded wins, no
/// reactivation) deterministically in every replay; this enum adds the
/// *concurrent combination* rules that only make sense when two versions of
/// one record diverge (detected via the `concurrent` flag).
#[derive(Debug, Clone, PartialEq)]
enum MergeDecision {
    /// Clean forward apply.
    Apply,
    /// `archive + edit`: apply the edited content but keep the record
    /// archived; the combination is reported, never resolved by timestamp.
    ApplyCombination(&'static str),
    /// Idempotent no-op (same revision already accepted).
    Skip,
    /// Refused by the lifecycle gate (tombstone/superseded win, retired id
    /// reactivation). The candidate is NOT applied and NOT preserved — a
    /// tombstone never retains the deleted content in a conflict artifact.
    Refuse(String),
    /// Unresolved concurrent merge: preserve the candidate, suppress the id.
    Conflict(&'static str),
    /// `supersede + content edit`: the replacement is authoritative, but the
    /// losing edit is preserved as a NEW record referencing the retired id —
    /// never dropped (spec §"Concurrent merge policy").
    SupersedeWinsPreserveEdit,
}

/// Classify one candidate against the current (mesh-owned) row.
///
/// `local_state`/`local_superseded_by`/`local_content` describe the row the
/// replay already accepted; `cand` is the incoming committed record.
/// `concurrent` is true only when the two versions are genuinely divergent
/// (the spec's "divergent ancestry" trigger) — see the caller in
/// [`apply_changed`] for why the diff path is never concurrent.
///
/// The retirement rules (tombstone wins, superseded wins, no reactivation)
/// key off [`LifecycleState::is_terminal`] and [`LifecycleState::can_transition_to`],
/// which are the single gate every applied lifecycle change passes through.
fn classify_merge(
    local_state: LifecycleState,
    local_superseded_by: Option<Uuid>,
    local_content: &str,
    cand: &MeshRecord,
    concurrent: bool,
) -> MergeDecision {
    use LifecycleState::*;
    let cand_state = cand.lifecycle.state;
    let cand_sup = cand.lifecycle.superseded_by;
    let content_changed = local_content != content_text(cand);
    // Derive the boolean the kernel expects: same superseded-by target.
    let same_superseded_by = local_superseded_by == cand_sup;

    // Pure kernel decision (zero external dependencies).
    let kernel = crate::formal_kernel::classify_merge_decision(
        local_state.to_kernel(),
        cand_state.to_kernel(),
        same_superseded_by,
        content_changed,
        concurrent,
    );

    // Map the 10 KernelMergeDecision variants back to the 6 MergeDecision
    // variants with their exact existing reason strings.
    match kernel {
        crate::formal_kernel::KernelMergeDecision::Apply => MergeDecision::Apply,
        crate::formal_kernel::KernelMergeDecision::ApplyCombinationArchiveEdit => {
            MergeDecision::ApplyCombination(
                "archive + edit; edited content stays archived",
            )
        }
        crate::formal_kernel::KernelMergeDecision::Skip => MergeDecision::Skip,
        crate::formal_kernel::KernelMergeDecision::RefuseTombstoneWins => {
            MergeDecision::Refuse("tombstone wins; deleted content is never served".to_string())
        }
        crate::formal_kernel::KernelMergeDecision::RefuseStaleRestore => {
            MergeDecision::Refuse("superseded; rejected stale restore".to_string())
        }
        crate::formal_kernel::KernelMergeDecision::RefuseIllegalTransition => {
            MergeDecision::Refuse(format!(
                "illegal transition {local_state} -> {cand_state}"
            ))
        }
        crate::formal_kernel::KernelMergeDecision::ConflictTwoReplacements => {
            MergeDecision::Conflict("two different replacements")
        }
        crate::formal_kernel::KernelMergeDecision::ConflictRestoreArchive => {
            MergeDecision::Conflict("restore + archive")
        }
        crate::formal_kernel::KernelMergeDecision::ConflictIncompatibleEdits => {
            MergeDecision::Conflict("incompatible content edits")
        }
        crate::formal_kernel::KernelMergeDecision::SupersedeWinsPreserveEdit => {
            MergeDecision::SupersedeWinsPreserveEdit
        }
    }
}

/// Canonical merge-decision tag names mirroring the Lean 4 truth table
/// (`proofs/NeuroStrata`): the single canonical string each [`MergeDecision`]
/// outcome maps to, consumed by the Rust↔Lean parity suite (task
/// `neurostrata-ngd9`).
const PARITY_REPLACED_BY_A: Uuid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_0001);
const PARITY_REPLACED_BY_B: Uuid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_0002);

/// Classify one synthetic `(local, candidate)` merge pair over the parity
/// inputs and return the canonical Lean-parity tag for the decision.
///
/// `same_superseded_by` / `content_changed` / `concurrent` are the finite
/// parity dimensions (replacement identity, content divergence, genuine
/// divergence); the pair is evaluated through the real [`classify_merge`] so
/// the tag can never drift from the production table.
pub fn classify_merge_parity_tag(
    local_state: LifecycleState,
    cand_state: LifecycleState,
    same_superseded_by: bool,
    content_changed: bool,
    concurrent: bool,
) -> &'static str {
    let cand_text = if content_changed { "edited" } else { "base" };
    let cand = MeshRecord {
        schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
        id: Uuid::new_v4(),
        namespace: "probe".to_string(),
        memory_type: "rule".to_string(),
        lifecycle: Lifecycle {
            state: cand_state,
            created_at: chrono::Utc::now(),
            created_by: "node-peer-1".to_string(),
            superseded_by: if cand_state == LifecycleState::Superseded {
                Some(PARITY_REPLACED_BY_A)
            } else {
                None
            },
            archived_at: None,
            tombstoned_at: None,
        },
        content: json!({ "text": cand_text }),
        metadata: json!({ "eligibility": "shareable", "trust": "untrusted" }),
        rdf: None,
        relationships: vec![],
        update_id: Uuid::new_v4(),
        origin_mesh_epoch: 1,
        trust: Trust::Untrusted,
        signature: None,
        signer_did: None,
        public_key: None,
        patch: None,
    };
    let local_superseded_by = if local_state == LifecycleState::Superseded {
        Some(if same_superseded_by { PARITY_REPLACED_BY_A } else { PARITY_REPLACED_BY_B })
    } else {
        None
    };
    let decision = classify_merge(local_state, local_superseded_by, "base", &cand, concurrent);
    match decision {
        MergeDecision::Apply => "apply",
        MergeDecision::ApplyCombination(_) => "apply_combination_archive_edit",
        MergeDecision::Skip => "skip",
        MergeDecision::Refuse(reason) => {
            if reason.contains("tombstone") {
                "refuse_tombstone_wins"
            } else if reason.contains("superseded") {
                "refuse_stale_restore"
            } else {
                "refuse_illegal_transition"
            }
        }
        MergeDecision::Conflict(reason) => {
            if reason == "two different replacements" {
                "conflict_two_replacements"
            } else if reason == "restore + archive" {
                "conflict_restore_archive"
            } else {
                "conflict_incompatible_edits"
            }
        }
        MergeDecision::SupersedeWinsPreserveEdit => "supersede_wins_preserve_edit",
    }
}

/// The current lifecycle state of a mesh-owned row, read from its
/// `metadata.mesh.lifecycle_state` (written by slice 6), falling back to the
/// 1.8.0 top-level markers for rows applied before this slice. Returns
/// `None` when the row carries no mesh provenance (private local memory) —
/// callers must not call this on a private row.
fn local_mesh_state(metadata: &Value) -> Option<(LifecycleState, Option<Uuid>)> {
    let mesh = metadata.get("mesh").and_then(Value::as_object)?;
    if let Some(s) = mesh.get("lifecycle_state").and_then(Value::as_str) {
        let state = LifecycleState::from_str(s).ok()?;
        let superseded_by = mesh
            .get("superseded_by")
            .and_then(Value::as_str)
            .and_then(|u| Uuid::parse_str(u).ok());
        return Some((state, superseded_by));
    }
    // Backward-compatible inference (rows applied before slice 6).
    let superseded_by = metadata
        .get("superseded_by")
        .and_then(Value::as_str)
        .and_then(|u| Uuid::parse_str(u).ok());
    if metadata
        .get("tombstoned")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some((LifecycleState::Tombstoned, superseded_by));
    }
    if metadata
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some((LifecycleState::Archived, superseded_by));
    }
    if superseded_by.is_some() {
        return Some((LifecycleState::Superseded, superseded_by));
    }
    Some((LifecycleState::Active, None))
}

/// The `metadata.mesh` provenance object for a freshly applied row. Carries
/// the effective lifecycle state (not necessarily the candidate's, in the
/// archive+edit combination) so the next apply can gate against it.
///
/// Trust provenance (Program Task 3): `origin` names the funnel
/// ("mesh-import"), NOT the sender. The sender's `created_by` is kept
/// as `claimed_created_by` with `created_by_verified: false` — a mesh
/// record's author field is an unverified assertion by the sender, and
/// the provenance says so instead of laundering it into a fact.
/// `head_commit_signature` records the signature state of the mesh
/// HEAD commit the record arrived in (DR-9: recorded as metadata only,
/// never gated on — mesh commits are unsigned by construction).
fn mesh_provenance(
    mesh_id: &str,
    update_id: &Uuid,
    claimed_created_by: &str,
    origin_mesh_epoch: u32,
    state: LifecycleState,
    superseded_by: Option<Uuid>,
    head_commit_signature: &str,
) -> Value {
    json!({
        "mesh_id": mesh_id,
        "update_id": update_id.to_string(),
        "origin": "mesh-import",
        "claimed_created_by": claimed_created_by,
        "created_by_verified": false,
        "head_commit_signature": head_commit_signature,
        "origin_mesh_epoch": origin_mesh_epoch,
        "recorded_at": chrono::Utc::now().to_rfc3339(),
        "lifecycle_state": state.to_string(),
        "superseded_by": superseded_by.map(|u| u.to_string()),
        "conflict": false,
    })
}

/// The signature state of the mesh checkout's HEAD commit, for
/// provenance recording only (DR-9). `git log --format=%G?` answers
/// "G" for a good signature and "N" for none; anything else — bad or
/// unknown validity, a git failure, no HEAD — reads as "unverifiable"
/// rather than a guess.
fn head_commit_signature_state(root: &Path) -> &'static str {
    match run_ok(root, &["log", "-1", "--format=%G?", "HEAD"]) {
        Ok(out) => match String::from_utf8_lossy(&out.stdout).trim() {
            "G" => "signed",
            "N" => "unsigned",
            _ => "unverifiable",
        },
        Err(_) => "unverifiable",
    }
}

/// Merge the 1.8.0 read-side conventions (`archived: true`,
/// `superseded_by`) and the mesh terminal marker (`tombstoned: true`) onto a
/// row's top-level metadata so the existing read paths (`is_archived`) and
/// the lifecycle gate agree on one shape. The `metadata.mesh` provenance is
/// written separately by [`mesh_provenance`].
fn apply_lifecycle_markers(
    metadata: &mut serde_json::Map<String, Value>,
    state: LifecycleState,
    superseded_by: Option<Uuid>,
    archived_at: Option<chrono::DateTime<chrono::Utc>>,
    tombstoned_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    match state {
        LifecycleState::Archived => {
            metadata.insert("archived".to_string(), json!(true));
            metadata.insert(
                "archived_at".to_string(),
                json!(archived_at
                    .map(|t| t.timestamp())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp())),
            );
        }
        LifecycleState::Tombstoned => {
            // Tombstone suppresses from search via the same `archived` read
            // path AND carries the stronger terminal marker so the lifecycle
            // gate can never reactivate it.
            metadata.insert("archived".to_string(), json!(true));
            metadata.insert("tombstoned".to_string(), json!(true));
            metadata.insert(
                "tombstoned_at".to_string(),
                json!(tombstoned_at
                    .map(|t| t.timestamp())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp())),
            );
        }
        LifecycleState::Superseded => {
            metadata.insert(
                "superseded_by".to_string(),
                json!(superseded_by.map(|u| u.to_string())),
            );
        }
        _ => {}
    }
}

/// Annotate an EXISTING row with the suppression marker
/// `metadata.mesh.conflict = true` (+ the reason), leaving its content,
/// lifecycle and user metadata untouched. The row keeps its own vector and
/// its accepted `update_id`; only the additive marker changes.
async fn annotate_conflicting(
    store: &std::sync::Arc<dyn VectorStore>,
    namespace: &str,
    id: &str,
    vector: &[f32],
    payload: &MemoryPayload,
    reason: &str,
) -> anyhow::Result<()> {
    let mut metadata = match payload.metadata.as_object() {
        Some(m) => m.clone(),
        None => {
            return Err(anyhow::anyhow!(
                "row {id} metadata is not a JSON object; cannot annotate the conflict"
            ))
        }
    };
    let mut mesh = metadata
        .get("mesh")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    mesh.insert("conflict".to_string(), json!(true));
    mesh.insert("conflict_reason".to_string(), json!(reason));
    metadata.insert("mesh".to_string(), Value::Object(mesh));
    let new_payload = MemoryPayload {
        content: payload.content.clone(),
        user_id: payload.user_id.clone(),
        memory_type: payload.memory_type.clone(),
        agent_name: payload.agent_name.clone(),
        location: payload.location.clone(),
        location_lines: payload.location_lines.clone(),
        metadata: Value::Object(metadata),
    };
    store
        .upsert(namespace, id, vector.to_vec(), new_payload)
        .await
        .map(|_| ())
}

/// How a candidate should be materialised into a row.
enum ApplyShape {
    /// Apply the candidate exactly as committed.
    AsCandidate,
    /// `archive + edit` combination: keep the *edited* content (the active
    /// side's text) but force the record archived.
    ArchivedCombination { edited: String },
}

/// Parse the sibling `<id>.rdfpatch` sidecar of a candidate record
/// (`record_path` is the `.json` path inside the mesh checkout), if any.
/// Fail-soft: an unreadable or invalid sidecar is a `tracing::warn!` and
/// returns `None`; the record is still gated by ordinary replay validation.
/// The caller attaches the stream AFTER `verify_record_signature`, so
/// canonical signing bytes over the `.json` record are unaffected.
fn load_record_patch(record_path: &Path) -> Option<crate::streaming::Stream> {
    let patch_path = record_path.with_extension("rdfpatch");
    if !patch_path.exists() {
        return None;
    }
    match std::fs::read_to_string(&patch_path) {
        Ok(text) => match crate::streaming::Stream::parse_patch(&text) {
            Ok(deltas) => Some(crate::streaming::Stream { events: deltas }),
            Err(e) => {
                tracing::warn!("mesh rdf patch {} invalid: {e}", patch_path.display());
                None
            }
        },
        Err(e) => {
            tracing::warn!("mesh rdf patch {} unreadable: {e}", patch_path.display());
            None
        }
    }
}

/// Validate-and-apply one candidate against the store. `root` is the mesh
/// checkout (its sibling `.conflicts` dir receives preserved variants);
/// `concurrent` is true only on the full-tree reconciliation path.
/// `import_trust` is the manifest's imported-procedures policy (the
/// stamp source for applied procedures); `head_signature` is the HEAD
/// commit's signature state, recorded in provenance only.
/// `signature_disposition` is the outcome `pre_apply_gate` recorded for
/// this record's signature; it keys the provenance stamp in
/// `apply_effective`, so a signature that verified only over the RDF
/// projection is never stamped as full-record verified.
async fn apply_one(
    root: &Path,
    mesh_id: &str,
    record: MeshRecord,
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
    concurrent: bool,
    import_trust: Trust,
    head_signature: &str,
    signature_disposition: &SignatureDisposition,
    // Route flag for the tracked-stale-bundle guard (issue #44): `true`
    // only for records that arrived through the bundle-only loop, never
    // for `.json` candidates.
    from_bundle: bool,
) -> Disposition {
    let id = record.id.to_string();
    let namespace = record.namespace.clone();
    if let Err(e) = store.init(&namespace).await {
        return Disposition::Ignored(format!("store init failed for namespace {namespace}: {e}"));
    }

    // Idempotence and the private-memory guard happen BEFORE any write. A
    // row without mesh provenance is a private local memory: never
    // overwritten, never deleted.
    let existing = match store.get(&namespace, &id).await {
        Ok(found) => found,
        Err(e) => return Disposition::Ignored(format!("store read failed for {id}: {e}")),
    };

    if let Some((vector, payload)) = existing {
        match mesh_update_id_of(&payload.metadata) {
            Some(accepted) if accepted == record.update_id.to_string() => {
                return Disposition::Skipped;
            }
            None => {
                return Disposition::Ignored(format!(
                    "private memory owns id {id} — never overwritten, never deleted"
                ));
            }
            // Mesh-owned row at another revision: gate the transition.
            Some(accepted) => {
                let Some((local_state, local_superseded_by)) = local_mesh_state(&payload.metadata)
                else {
                    return Disposition::Ignored(format!(
                        "mesh row {id} has unreadable lifecycle provenance"
                    ));
                };
                // Tracked-stale-bundle guard (issue #44, Gate G2 option
                // (a)): a bundle-only candidate on a DIFF pass whose
                // UUIDv7 update_id predates the accepted revision would
                // otherwise pass `classify_merge` as a plain forward edit
                // and silently roll the live row back. The guard is
                // deliberately narrow — bundle route only, diff pass
                // only (full-tree passes stay concurrent/conflicted),
                // both sides live and in the SAME state (transitions and
                // combinations are untouched), content actually
                // differing, and both update_ids UUIDv7 with the
                // candidate STRICTLY older (millisecond comparison;
                // ties and any non-v7 id leave it inert).
                if from_bundle && !concurrent {
                    let both_live_same_state = matches!(
                        local_state,
                        LifecycleState::Active | LifecycleState::Archived
                    ) && record.lifecycle.state == local_state;
                    if both_live_same_state && payload.content != content_text(&record) {
                        let accepted_v7 = Uuid::parse_str(&accepted)
                            .ok()
                            .and_then(|u| update_id_v7_millis(&u));
                        let candidate_v7 = update_id_v7_millis(&record.update_id);
                        if let (Some(accepted_ts), Some(candidate_ts)) = (accepted_v7, candidate_v7)
                        {
                            if candidate_ts < accepted_ts {
                                return Disposition::Ignored(format!(
                                    "{id}: stale bundle revision refused — candidate update_id {} predates accepted {accepted}",
                                    record.update_id
                                ));
                            }
                        }
                    }
                }
                match classify_merge(
                    local_state,
                    local_superseded_by,
                    &payload.content,
                    &record,
                    concurrent,
                ) {
                    MergeDecision::Skip => return Disposition::Skipped,
                    MergeDecision::Refuse(reason) => {
                        return Disposition::Ignored(format!("{id}: {reason}"));
                    }
                    MergeDecision::Conflict(reason) => {
                        // Preserve the candidate, register the overlay, and
                        // annotate the EXISTING row (content untouched) with
                        // the suppression marker.
                        if let Err(e) = conflict::register_conflict(
                            root,
                            &namespace,
                            &id,
                            &record.update_id.to_string(),
                            reason,
                            ConflictKind::Unresolved,
                            &record,
                        ) {
                            return Disposition::Ignored(format!(
                                "conflict overlay write failed for {id}: {e}"
                            ));
                        }
                        if let Err(e) =
                            annotate_conflicting(&store, &namespace, &id, &vector, &payload, reason)
                                .await
                        {
                            return Disposition::Ignored(format!(
                                "conflict marker write failed for {id}: {e}"
                            ));
                        }
                        return Disposition::Conflicted(id);
                    }
                    MergeDecision::SupersedeWinsPreserveEdit => {
                        return preserve_superseded_edit(
                            root,
                            mesh_id,
                            &id,
                            &namespace,
                            local_state,
                            &payload,
                            &record,
                            store,
                            emb,
                            import_trust,
                            head_signature,
                            signature_disposition,
                        )
                        .await;
                    }
                    MergeDecision::ApplyCombination(reason) => {
                        // Edited content stays archived. The edited content
                        // is whichever side is ACTIVE (they edited; we
                        // archived, or vice-versa).
                        let edited = if local_state == LifecycleState::Archived {
                            content_text(&record)
                        } else {
                            payload.content.clone()
                        };
                        return match apply_effective(
                            root,
                            mesh_id,
                            &id,
                            &namespace,
                            &record,
                            store,
                            emb,
                            ApplyShape::ArchivedCombination { edited },
                            Some(import_trust),
                            head_signature,
                            signature_disposition,
                        )
                        .await
                        {
                            Disposition::Applied => {
                                Disposition::AppliedCombination(reason.to_string())
                            }
                            other => other,
                        };
                    }
                    MergeDecision::Apply => {}
                }
            }
        }
    }

    apply_effective(
        root,
        mesh_id,
        &id,
        &namespace,
        &record,
        store,
        emb,
        ApplyShape::AsCandidate,
        Some(import_trust),
        head_signature,
        signature_disposition,
    )
    .await
}

/// Materialise one candidate (or combination) into a store row: embed the
/// effective content, stamp the mesh provenance and lifecycle markers, and
/// upsert. On success, any prior conflict registration for this id is
/// cleared (the row is no longer suppressed).
///
/// `trust_stamp` is the manifest policy to stamp onto a procedure row
/// (`Some` for records arriving through the mesh funnel). `None` leaves
/// the record's own trust key as the record builder set it — the
/// preserved-edit path, whose builder is itself funnel code that has
/// already decided the stamp. Non-procedure records are never stamped.
async fn apply_effective(
    root: &Path,
    mesh_id: &str,
    id: &str,
    namespace: &str,
    record: &MeshRecord,
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
    shape: ApplyShape,
    trust_stamp: Option<Trust>,
    head_signature: &str,
    signature_disposition: &SignatureDisposition,
) -> Disposition {
    // 54-B: only an as-candidate apply stores the record the candidate's
    // signature was verified over. An archived combination stores a
    // (content, state) pair that was never the subject of that
    // verification — the local row's content forced to Archived, or the
    // candidate's content with its signed Active state forced to
    // Archived — so combination rows earn no verified stamps below.
    let stored_is_signed_candidate = matches!(shape, ApplyShape::AsCandidate);
    let (text, state, superseded_by, archived_at, tombstoned_at) = match shape {
        ApplyShape::AsCandidate => (
            content_text(record),
            record.lifecycle.state,
            record.lifecycle.superseded_by,
            record.lifecycle.archived_at,
            record.lifecycle.tombstoned_at,
        ),
        ApplyShape::ArchivedCombination { edited } => (
            edited,
            LifecycleState::Archived,
            None,
            Some(chrono::Utc::now()),
            None,
        ),
    };

    let vector = match emb.embed(&text).await {
        Ok(v) => v,
        Err(e) => return Disposition::Ignored(format!("embedding failed for {id}: {e}")),
    };

    let mut metadata = match record.metadata.as_object() {
        Some(map) => map.clone(),
        None => {
            return Disposition::Ignored(format!("record {id} metadata is not a JSON object"));
        }
    };
    // Program Task 3 (b): the procedure trust stamp comes from the
    // manifest policy threaded in by the caller — whatever the sender's
    // record claims in its metadata is overwritten here. This is the
    // preserved-edit embryo (which force-stamps "untrusted" in
    // `preserved_edit_record`) generalized to the main apply path: the
    // funnel decides, the sender doesn't.
    if record.memory_type == "procedure" {
        if let Some(policy) = trust_stamp {
            metadata.insert("trust".to_string(), json!(policy.to_string()));
        }
    }

    // F7: an attached `.rdfpatch` sidecar (loaded after signature
    // verification in the batch apply) is genuinely applied to the stored
    // row here: `references` / `related_to` deltas land on the metadata
    // arrays the edge tables read, fail-soft per delta, and the delta count
    // is recorded so a no-op sidecar is distinguishable from an absent one.
    if let Some(ref stream) = record.patch {
        if !stream.is_empty() {
            metadata.insert("rdf_patch_quad_count".to_string(), json!(stream.len()));
            for delta in &stream.events {
                let predicate = delta
                    .predicate
                    .trim_start_matches('<')
                    .trim_end_matches('>');
                let key = if predicate.ends_with("references") {
                    "references"
                } else if predicate.ends_with("related_to") {
                    "related_to"
                } else {
                    continue;
                };
                let object = delta
                    .object
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string();
                let mut targets: Vec<String> = metadata
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                match delta.op {
                    crate::streaming::PatchOp::Add => {
                        if !object.is_empty() && !targets.iter().any(|t| t == &object) {
                            targets.push(object);
                        }
                    }
                    crate::streaming::PatchOp::Delete => {
                        targets.retain(|t| *t != object);
                    }
                }
                metadata.insert(key.to_string(), json!(targets));
            }
        }
    }
    let mut mesh_prov = mesh_provenance(
        mesh_id,
        &record.update_id,
        &record.lifecycle.created_by,
        record.origin_mesh_epoch,
        state,
        superseded_by,
        head_signature,
    );
    // The provenance stamp is keyed on the disposition the gate actually
    // recorded, not on the mere presence of a signature: a signature that
    // verified over the canonical-JSON payload covers the whole record,
    // while one that verified only over the RDF payload covers just that
    // lossy projection — the two earn different stamps. The stamp also
    // requires the stored row to BE the signed candidate
    // (`stored_is_signed_candidate`): an archived-combination row stores
    // a (content, state) pair the signature never covered, so it earns
    // no verified stamp in either branch — neither
    // `signature_verified`/`verified_signer_did` nor
    // `rdf_signature_verified`/`rdf_verified_signer_did`.
    if stored_is_signed_candidate && record.signature.is_some() {
        if let Some(ref did) = record.signer_did {
            if let Some(obj) = mesh_prov.as_object_mut() {
                match signature_disposition {
                    SignatureDisposition::Verified => {
                        obj.insert("verified_signer_did".to_string(), json!(did));
                        obj.insert("signature_verified".to_string(), json!(true));
                    }
                    SignatureDisposition::VerifiedRdfOnly => {
                        // 54-A: NOT `verified_signer_did` — rebac derives
                        // the resource owner from that key and the owner
                        // is always allowed, so it is owner-grade signer
                        // attribution. An RDF-only signature attests only
                        // to the lossy RDF payload, not the full record;
                        // its signer provenance is RDF-branch only.
                        obj.insert("rdf_verified_signer_did".to_string(), json!(did));
                        obj.insert("rdf_signature_verified".to_string(), json!(true));
                    }
                    _ => {}
                }
            }
        }
    }
    metadata.insert("mesh".to_string(), mesh_prov);
    apply_lifecycle_markers(
        &mut metadata,
        state,
        superseded_by,
        archived_at,
        tombstoned_at,
    );

    // F20 concept_iri mesh mapping (C5): preserve concept_iri (already in metadata
    // via allowlist) and map to receiver's local domain slug(s) when registry has
    // matching concept_iri. Idempotent on replay.
    let current_update_id = record.update_id.to_string();
    let already_imported = match store.get(namespace, id).await {
        Ok(found) => found
            .and_then(|(_, p)| mesh_update_id_of(&p.metadata))
            .as_deref()
            == Some(current_update_id.as_str()),
        Err(e) => {
            tracing::warn!("[neurostrata:mesh] store read failed for {id}: {e}");
            false
        }
    };

    if !already_imported {
        if let Some(concept_val) = metadata.get("concept_iri").cloned() {
            let iris: Vec<String> = match &concept_val {
                Value::String(s) => vec![s.clone()],
                Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
                _ => vec![],
            };
            let rid = crate::registry::registry_row_id(namespace);
            let entries = match store.get(namespace, &rid).await {
                Ok(Some((_, rp))) => crate::registry::registry_domain_entries(&rp.metadata),
                Ok(None) => vec![],
                Err(e) => {
                    tracing::warn!(
                        "[neurostrata:mesh] registry row {rid} unreadable: {e}"
                    );
                    vec![]
                }
            };
            let mut matched_slugs: Vec<String> = Vec::new();
            for iri in &iris {
                let matches: Vec<String> = entries
                    .iter()
                    .filter_map(|e| {
                        if e.get("concept_iri").and_then(|v| v.as_str()) == Some(iri.as_str()) {
                            e.get("slug").and_then(|v| v.as_str()).map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                match matches.len() {
                    0 => {}
                    1 => {
                        if !matched_slugs.contains(&matches[0]) {
                            matched_slugs.push(matches[0].clone());
                        }
                    }
                    n => {
                        // ambiguous
                        tracing::warn!(
                            "[neurostrata:mesh] ambiguous concept_iri {} matches {} local domains",
                            iri, n
                        );
                    }
                }
            }
            if !matched_slugs.is_empty() {
                let mut existing: Vec<String> = match metadata.get("domain") {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
                    _ => vec![],
                };
                for s in &matched_slugs {
                    if !existing.contains(s) {
                        existing.push(s.clone());
                    }
                }
                existing.sort();
                existing.dedup();
                let val = if existing.len() == 1 {
                    json!(existing[0].clone())
                } else {
                    json!(existing)
                };
                metadata.insert("domain".to_string(), val);
            }
        }
    }

    let payload = MemoryPayload {
        content: text,
        user_id: record.lifecycle.created_by.clone(),
        memory_type: record.memory_type.clone(),
        agent_name: record
            .metadata
            .get("agent_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        location: String::new(),
        location_lines: String::new(),
        metadata: Value::Object(metadata),
    };
    let imported_domain = crate::domain::domain_key(&payload.metadata);
    if let Err(e) = store.upsert(namespace, id, vector, payload).await {
        return Disposition::Ignored(format!("store upsert failed for {id}: {e}"));
    }
    // A successful apply supersedes any prior conflict registration (the
    // row is no longer suppressed).
    if let Err(e) = conflict::clear_conflict(root, namespace, id) {
        tracing::warn!("Failed to clear mesh conflict overlay for {namespace}::{id}: {e}");
    }
    // Union the imported record's domain into the local registry (a cache).
    // A failure here is a warning, never a refusal: the row is already stored.
    if let Some(domain) = imported_domain {
        if let Some(warning) = crate::registry::write_registry(
            &store,
            &emb,
            namespace,
            std::slice::from_ref(&domain),
        )
        .await
        {
            tracing::warn!(
                "registry union failed for imported record {id} in {namespace}: {warning}"
            );
        }
    }
    Disposition::Applied
}

/// `supersede + content edit` (spec §"Concurrent merge policy"): the
/// replacement is authoritative — X ends (or stays) superseded — and the
/// losing edit becomes a NEW record referencing X, never dropped.
///
/// Direction A (`local_state == Superseded`, candidate is the edit): X stays
/// superseded as-is and the candidate becomes the new record. Direction B
/// (local is the edit, candidate supersedes): the supersession applies to X
/// and the local edit becomes the new record.
///
/// Direction B's preserved-edit record used to rebuild content as a
/// `{"text": <string>}` wrapper and write a fixed metadata literal, which
/// string-in-stringed a procedure definition and dropped the row's own
/// metadata and relationships. Fixed 2026-10-11 (slice-6b): the preserved
/// record now carries the row's structured content, merged metadata and
/// reconstructed relationships — see [`preserved_edit_record`].
async fn preserve_superseded_edit(
    root: &Path,
    mesh_id: &str,
    id: &str,
    namespace: &str,
    local_state: LifecycleState,
    local_payload: &MemoryPayload,
    cand: &MeshRecord,
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
    import_trust: Trust,
    head_signature: &str,
    signature_disposition: &SignatureDisposition,
) -> Disposition {
    // Which side holds the edit, and must the incoming supersession apply?
    // `source_metadata` is the metadata the new record inherits from the
    // losing edit's side (candidate metadata for direction A, the existing
    // row's for direction B), merged with the gate fields rather than
    // replaced by a fixed literal (slice-6b, 2026-10-11).
    let (content, memory_type, created_by, source_rels, source_metadata, apply_supersession) =
        if local_state == LifecycleState::Superseded {
            // The incoming candidate is the edit; X is already superseded.
            (
                cand.content.clone(),
                cand.memory_type.clone(),
                cand.lifecycle.created_by.clone(),
                cand.relationships.clone(),
                cand.metadata.clone(),
                false,
            )
        } else {
            // The local row is the edit; apply the incoming supersession.
            // Carry the row's own structured content, metadata and
            // relationships rather than a lossy `{"text": ...}` wrapper and
            // an empty relationship list.
            (
                local_content_value(local_payload),
                local_payload.memory_type.clone(),
                local_origin(local_payload),
                local_relationships(local_payload),
                local_payload.metadata.clone(),
                true,
            )
        };

    if apply_supersession {
        match apply_effective(
            root,
            mesh_id,
            id,
            namespace,
            cand,
            store.clone(),
            emb.clone(),
            ApplyShape::AsCandidate,
            Some(import_trust),
            head_signature,
            signature_disposition,
        )
        .await
        {
            Disposition::Applied => {}
            other => return other,
        }
    }

    let new_record = preserved_edit_record(
        namespace,
        id,
        content,
        memory_type,
        created_by,
        source_rels,
        source_metadata,
        import_trust,
        cand.origin_mesh_epoch,
    );
    let new_id = new_record.id.to_string();
    // No trust_stamp: `preserved_edit_record` is funnel code and has
    // already set the preserved record's stamp from `import_trust`
    // (untrusted by default, the manifest policy when overridden) —
    // stamping again here would only repeat the funnel's own decision.
    // The preserved record is built unsigned, so its signature
    // disposition is `Unsigned` and it earns no verification stamp.
    apply_effective(
        root,
        mesh_id,
        &new_id,
        namespace,
        &new_record,
        store,
        emb,
        ApplyShape::AsCandidate,
        None,
        head_signature,
        &SignatureDisposition::Unsigned,
    )
    .await
}

/// The origin node of an already-applied row, used as the `created_by`
/// of a preserved-edit record. Rows applied since Program Task 3 carry
/// the claim as `metadata.mesh.claimed_created_by` (with `origin`
/// naming the funnel instead); rows applied before that carry it in
/// `metadata.mesh.origin` itself, which stays readable here so
/// pre-change rows keep working.
fn local_origin(payload: &MemoryPayload) -> String {
    payload
        .metadata
        .get("mesh")
        .and_then(|m| {
            m.get("claimed_created_by")
                .or_else(|| m.get("origin"))
        })
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// The local row's content, preserved as its structured `Value` rather than a
/// `{"text": ...}` wrapper. The store flattens content to text at apply time
/// ([`content_text`]: the `text` field, else the serialized content), so a
/// structured definition (a procedure's trigger/valid_to/rehearsal_script)
/// lives in `payload.content` as its JSON serialization. Re-parsing it keeps
/// the structure intact (slice-6b, 2026-10-11); plain text stays wrapped.
fn local_content_value(payload: &MemoryPayload) -> Value {
    match serde_json::from_str::<Value>(&payload.content) {
        Ok(value) if value.is_object() => value,
        _ => json!({ "text": payload.content.clone() }),
    }
}

/// The existing row's mesh relationships, reconstructed from its metadata.
/// Mesh `relationships` are not stored in the graph store; only these metadata
/// fields survive the apply — `references` (array) mirrors `references` links
/// and `supersedes` (string) mirrors a `supersedes` link, matching
/// [`super::worker::build_record`]'s read of `metadata.supersedes`.
fn local_relationships(payload: &MemoryPayload) -> Vec<Relationship> {
    let mut rels = Vec::new();
    if let Some(arr) = payload.metadata.get("references").and_then(Value::as_array) {
        for item in arr {
            if let Some(target) = item.as_str().and_then(|s| Uuid::parse_str(s).ok()) {
                rels.push(Relationship {
                    kind: "references".to_string(),
                    target,
                });
            }
        }
    }
    if let Some(target) = payload
        .metadata
        .get("supersedes")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        rels.push(Relationship {
            kind: "supersedes".to_string(),
            target,
        });
    }
    rels
}

/// Build the preserved-edit record for `supersede + content edit` (pure).
/// The losing edit's own content/type/creator/relationships/metadata are
/// carried forward; the record references the retired `original_id`
/// (`relationships: [{kind: "references", target: original_id}]`, spec
/// §"Concurrent merge policy"), surfaced in metadata as `references`
/// alongside the edit's own reference links so the linkage is visible on
/// the store row and no carried link is discarded. Only `eligibility`,
/// `trust` and `references` are (re)set on the metadata — every other key
/// the edit carried survives. `import_trust` is the trust the stored row
/// carries (defaulting to untrusted, honoring the manifest's
/// `imported_procedures_trust` when it is configured differently).
fn preserved_edit_record(
    namespace: &str,
    original_id: &str,
    content: Value,
    memory_type: String,
    created_by: String,
    source_rels: Vec<Relationship>,
    source_metadata: Value,
    import_trust: Trust,
    origin_mesh_epoch: u32,
) -> MeshRecord {
    let mut rels = source_rels;
    if let Ok(original_id) = Uuid::parse_str(original_id) {
        rels.push(Relationship {
            kind: "references".to_string(),
            target: original_id,
        });
    }
    let mut metadata = match source_metadata.as_object() {
        Some(map) => map.clone(),
        None => serde_json::Map::new(),
    };
    metadata.insert("eligibility".to_string(), json!("shareable"));
    metadata.insert("trust".to_string(), json!(import_trust.to_string()));
    // The edit's own reference links survive alongside the spec-mandated
    // link to the retired original: dropping them would lose the losing
    // edit's graph edges on the preserved row.
    let mut refs: Vec<String> = vec![original_id.to_string()];
    for r in &rels {
        if !refs.contains(&r.target.to_string()) {
            refs.push(r.target.to_string());
        }
    }
    metadata.insert("references".to_string(), json!(refs));

    // Build canonical RDF block for the preserved-edit record (spec §5.2c).
    // Always emit: Phase 1 dual-write.
    let created_at = chrono::Utc::now();
    let content_str = match &content {
        Value::String(s) => s.clone(),
        other => match serde_json::to_string(other) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "preserved edit original_id={original_id}: non-serialisable content defaulted to empty: {e}"
                );
                String::new()
            }
        },
    };
    // Curated metadata for RDF projection
    let eligibility = metadata.get("eligibility").and_then(|v| v.as_str());
    let source = metadata.get("source").and_then(|v| v.as_str());
    let tags: Vec<String> = metadata.get("tags").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let concept_iri = metadata.get("concept_iri").and_then(|v| v.as_str());
    let rdf_relationships: Vec<(String, String)> = rels.iter()
        .map(|r| (r.kind.clone(), r.target.to_string()))
        .collect();
    let state_str = "active";
    let id = Uuid::new_v4();
    let update_id = Uuid::new_v4();
    let (nquads_text, digest_str) = crate::rdf::dataset_canonical::build_mesh_canonical_nquads(
        &id.to_string(),
        namespace,
        &memory_type,
        state_str,
        &created_at.to_rfc3339(),
        &created_by,
        None,
        None,
        None,
        &content_str,
        eligibility,
        source,
        &tags,
        concept_iri,
        &rdf_relationships,
        &update_id.to_string(),
        origin_mesh_epoch,
        &import_trust.to_string(),
        None,
        None,
        None,
        None,
    );
    MeshRecord {
        schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
        id,
        namespace: namespace.to_string(),
        memory_type,
        lifecycle: Lifecycle {
            state: LifecycleState::Active,
            created_at,
            created_by,
            superseded_by: None,
            archived_at: None,
            tombstoned_at: None,
        },
        content,
        metadata: Value::Object(metadata),
        rdf: Some(RdfBlock {
            schema: "neurostrata-rdf-record/v1".to_string(),
            format: "n-quads".to_string(),
            canonical_hash: digest_str,
            quads: nquads_text,
        }),
        relationships: rels,
        update_id,
        origin_mesh_epoch,
        trust: import_trust,
        signature: None,
        signer_did: None,
        public_key: None,
        patch: None,
    }
}

/// Outcome of Ed25519 signature verification on a [`MeshRecord`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureDisposition {
    /// The signature verified over the canonical-JSON payload, which
    /// covers the whole record.
    Verified,
    /// The signature verified ONLY over the RDF payload (the legacy
    /// signing scheme): it covers the stored quads plus `update_id` and
    /// epoch, and nothing else — the RDF block is a lossy projection of
    /// the record, so the JSON fields outside the projection were never
    /// signed. The record still applies, but it must not be counted or
    /// stamped as full-record verified.
    VerifiedRdfOnly,
    Unsigned,
    Invalid(String),
    Unauthorized(String),
}

/// Verify optional Ed25519 signature + DID/pubkey consistency + manifest allowlist.
pub fn verify_record_signature(
    record: &MeshRecord,
    signers: &[super::manifest::SignerEntry],
) -> SignatureDisposition {
    let has_sig = record.signature.is_some();
    let has_did = record.signer_did.is_some();
    let has_pub = record.public_key.is_some();
    if !has_sig && !has_did && !has_pub {
        return SignatureDisposition::Unsigned;
    }
    let (Some(sig_b64), Some(did), Some(pub_hex)) =
        (&record.signature, &record.signer_did, &record.public_key)
    else {
        return SignatureDisposition::Invalid(
            "partial signature fields: signature, signer_did, and public_key must all be present"
                .to_string(),
        );
    };
    let Ok(pub_bytes) = hex::decode(pub_hex) else {
        return SignatureDisposition::Invalid("invalid hex in public_key".to_string());
    };
    if pub_bytes.len() != 32 {
        return SignatureDisposition::Invalid("public_key must be 32 bytes".to_string());
    }
    // The signer DID must resolve to exactly the record's public key: a
    // DID that cannot be parsed (or names a different key) is a mismatch.
    match crate::did::public_key_from_did(did) {
        Ok(did_pub) if did_pub == pub_bytes => {}
        _ => {
            return SignatureDisposition::Invalid(
                "signer_did does not match public_key".to_string(),
            );
        }
    }
    let Ok(sig_bytes) = crate::did::decode_base64(sig_b64) else {
        return SignatureDisposition::Invalid("invalid base64 in signature".to_string());
    };
    let payload_rdf = record.rdf.as_ref().map(|rdf_block| {
        crate::did::signing_payload(
            &rdf_block.quads,
            &record.update_id.to_string(),
            record.origin_mesh_epoch,
        )
    });
    let payload_json = match serde_json::to_value(record) {
        Ok(v) => match crate::did::canonical_signing_bytes(&v) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                tracing::warn!(
                    "[neurostrata:mesh] canonical signing bytes unavailable for {}: {e}",
                    record.id
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                "[neurostrata:mesh] record {} not canonicalisable: {e}",
                record.id
            );
            None
        }
    };

    // The two payload branches are tracked separately: they do not cover
    // the same bytes. The canonical-JSON payload covers the whole record
    // (the RDF block included, as one of its fields); the RDF payload
    // covers only the stored quads plus `update_id` and epoch — a lossy
    // projection. OR-ing them into one outcome would let an RDF-only
    // signature earn full-record trust for JSON fields it never covered.
    let verified_rdf = payload_rdf
        .as_ref()
        .map(|p| crate::did::verify(p, &pub_bytes, &sig_bytes))
        .unwrap_or(false);
    let verified_json = payload_json
        .as_ref()
        .map(|p| crate::did::verify(p, &pub_bytes, &sig_bytes))
        .unwrap_or(false);

    if !verified_rdf && !verified_json {
        return SignatureDisposition::Invalid("Ed25519 signature verification failed".to_string());
    }

    if !signers.is_empty()
        && !signers
            .iter()
            .any(|s| &s.did == did && s.public_key.eq_ignore_ascii_case(pub_hex))
    {
        return SignatureDisposition::Unauthorized(format!(
            "signer {did} is not in manifest.signers allowlist"
        ));
    }

    // If both branches somehow verify, the JSON branch wins: it is the
    // one that covers the full record.
    if verified_json {
        SignatureDisposition::Verified
    } else {
        SignatureDisposition::VerifiedRdfOnly
    }
}

/// Decompress a zstd-compressed mesh record bundle with bounded output size.
/// Used by [`read_compressed_bundle`]; additionally available for direct
/// bundle decodes.
pub fn decompress_record_bundle(bytes: &[u8]) -> Result<Vec<MeshRecord>, String> {
    let raw = crate::compression::zstd::decompress_mesh_bundle(
        bytes,
        crate::compression::zstd::DEFAULT_MESH_MAX_DECOMPRESSED_BYTES,
    )
    .map_err(|e| e.to_string())?;
    serde_json::from_slice(&raw).map_err(|e| e.to_string())
}

/// Read a compressed bundle from disk and decompress it.
pub fn read_compressed_bundle(bundle_path: &Path) -> Result<Vec<MeshRecord>, String> {
    let bytes = std::fs::read(bundle_path).map_err(|e| e.to_string())?;
    decompress_record_bundle(&bytes)
}

/// Wire-up for the artifacts a mesh-worker may leave behind on the source
/// node (spec: `NEUROSTRATA_MESH_BUNDLE_ZSTD` and
/// `NEUROSTRATA_MESH_RDF_PATCH`). Called once per pulled checkout, before
/// the replay pass. Fail-soft everywhere: a missing `bundles/` dir, an
/// unreadable bundle, or an invalid patch is logged, never fatal.
///
/// Returns every `MeshRecord` decoded from a `bundles/*.bundle.zst`
/// artifact, paired with its bundle path so the replay pass can report the
/// source. The caller deduplicates against the `.json` candidate tree — a
/// bundle is a replay candidate, not a debug log.
///
/// Trackedness barrier (bug #44, untracked/ignored limb): only a bundle
/// that is TRACKED in the mesh checkout (`git ls-files` under `bundles/`)
/// is decoded. A git-ignored or untracked bundle is local-only — drain
/// bundles are exactly that (storage installs `/bundles/*.bundle.zst`) —
/// and is invisible to the import dirty gate, so replaying it would let a
/// stale local file silently roll a live row back on a quiet pass. If the
/// `git ls-files` probe itself fails, NO bundle is decoded: the dirty
/// gate and `tracked_paths` both refuse to trust the disk when git
/// errors, and this fail-soft scan's analogue of that refusal is to
/// decode nothing from `bundles/` (the `.json` replay is unaffected).
fn scan_bundles_and_patches(mesh_root: &Path) -> Vec<(PathBuf, MeshRecord)> {
    let mut bundled_records: Vec<(PathBuf, MeshRecord)> = Vec::new();
    let bundles_dir = mesh_root.join("bundles");
    if bundles_dir.is_dir() {
        // One `git ls-files` for the whole `bundles/` tree, `-z` +
        // exact relative-path comparison (mirrors `tracked_paths`).
        let tracked_bundles: std::collections::HashSet<PathBuf> =
            match run_ok(mesh_root, &["ls-files", "-z", "--", "bundles/"]) {
                Ok(out) => split_nul(&out.stdout)
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
                Err(e) => {
                    tracing::warn!(
                        "mesh bundle scan at {}: git ls-files failed ({e:#}) — \
                         decoding no bundles this pass (fail-closed)",
                        bundles_dir.display()
                    );
                    std::collections::HashSet::new()
                }
            };
        if let Ok(entries) = std::fs::read_dir(&bundles_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("zst") {
                    if path.to_string_lossy().ends_with(".bundle.zst") {
                        // Local-only (git-ignored or untracked) bundles
                        // never replay; tracked bundles are the transport.
                        let rel = path.strip_prefix(mesh_root).unwrap_or(&path);
                        if !tracked_bundles.contains(rel) {
                            tracing::debug!(
                                "mesh bundle {} is local-only (not tracked) — skipped",
                                path.display()
                            );
                            continue;
                        }
                        match read_compressed_bundle(&path) {
                            Ok(records) => {
                                tracing::debug!(
                                    "mesh bundle {} decoded: {} record(s)",
                                    path.display(),
                                    records.len()
                                );
                                for record in records {
                                    bundled_records.push((path.clone(), record));
                                }
                            }
                            Err(e) => {
                                tracing::warn!("mesh bundle {} undecodable: {e}", path.display());
                            }
                        }
                    }
                }
            }
        }
    }

    // `.rdfpatch` sidecars written next to a `<id>.json` record file are
    // validated with the streaming patch parser. A parse failure is a
    // warning; the record itself is still gated by the ordinary replay
    // validation.
    if let Ok(nses) = std::fs::read_dir(mesh_root) {
        for ns in nses.flatten() {
            let ns_path = ns.path();
            if !ns_path.is_dir() {
                continue;
            }
            for subdir in ["memories", "procedures"] {
                let dir = ns_path.join(subdir);
                if !dir.is_dir() {
                    continue;
                }
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("rdfpatch") {
                            continue;
                        }
                        // "Alongside a record": the sibling `.json` must exist.
                        if !path.with_extension("json").exists() {
                            continue;
                        }
                        match std::fs::read_to_string(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|text| {
                                crate::streaming::Stream::parse_patch(&text)
                                    .map(|deltas| deltas.len())
                                    .map_err(|e| e.to_string())
                            }) {
                            Ok(n) => {
                                tracing::debug!(
                                    "mesh rdf patch {} valid: {} delta(s)",
                                    path.display(),
                                    n
                                );
                            }
                            Err(e) => {
                                tracing::warn!("mesh rdf patch {} invalid: {e}", path.display());
                            }
                        }
                    }
                }
            }
        }
    }

    bundled_records
}

// --- git plumbing (fail-soft, non-interactive; mirrors storage.rs) --------

/// The checkout's dirty porcelain paths (`git status --porcelain`, run like
/// storage.rs runs git: non-interactive, std::process::Command). An EMPTY
/// result is the only green light for a replay pass.
fn dirty_checkout_paths(mesh_root: &Path) -> anyhow::Result<Vec<String>> {
    let out = run_ok(mesh_root, &["status", "--porcelain"])?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("git {} could not be executed", args.join(" ")))
}

/// Prefer using `run_ok` from storage.rs / mesh_service.rs where they exist.
fn run_ok(root: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    let out = git(root, args)?;
    if out.status.success() {
        return Ok(out);
    }
    anyhow::bail!(
        "git {} failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// Program Task 3 (c): classify procedure rows written before stamping
/// existed, discriminating on mesh provenance. An unstamped procedure
/// with no import provenance is local authorship → stamped `local`. An
/// unstamped procedure WITH import provenance is stamped `untrusted`,
/// so it stays unfired until local promotion — the backfill never
/// launders an import into a local by silence. Already-stamped rows
/// and non-procedure rows are untouched, so the pass is idempotent.
/// Returns `(stamped_local, stamped_untrusted)` counts; the daemon
/// triggers this once at store open and logs them.
pub async fn backfill_imported_procedure_trust(
    store: &std::sync::Arc<dyn VectorStore>,
) -> (usize, usize) {
    let mut stamped_local = 0usize;
    let mut stamped_untrusted = 0usize;
    let namespaces = match store.list_namespaces().await {
        Ok(ns) => ns,
        Err(e) => {
            tracing::warn!("procedure trust backfill: cannot list namespaces: {e}");
            return (0, 0);
        }
    };
    for namespace in &namespaces {
        let rows = match store.list(namespace, None, None).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("procedure trust backfill: cannot list {namespace}: {e}");
                continue;
            }
        };
        for row in rows {
            if row.payload.memory_type != "procedure" {
                continue;
            }
            // A readable string stamp already classifies the row —
            // never rewritten (promotion and prior stamps stand).
            if row
                .payload
                .metadata
                .get("trust")
                .and_then(|v| v.as_str())
                .is_some()
            {
                continue;
            }
            let imported = row
                .payload
                .metadata
                .get("mesh")
                .map(|m| m.is_object())
                .unwrap_or(false);
            // List rows carry no vector; re-read the row to upsert it.
            let (vector, mut payload) = match store.get(namespace, &row.id).await {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        "procedure trust backfill: re-read of {} in {namespace} failed: {e}",
                        row.id
                    );
                    continue;
                }
            };
            // The row may have gained a stamp between list and get.
            if payload
                .metadata
                .get("trust")
                .and_then(|v| v.as_str())
                .is_some()
            {
                continue;
            }
            let Some(obj) = payload.metadata.as_object_mut() else {
                continue;
            };
            obj.insert(
                "trust".to_string(),
                json!(if imported { "untrusted" } else { "local" }),
            );
            match store.upsert(namespace, &row.id, vector, payload).await {
                Ok(_) => {
                    if imported {
                        stamped_untrusted += 1;
                    } else {
                        stamped_local += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "procedure trust backfill: upsert of {} in {namespace} failed: {e}",
                        row.id
                    );
                }
            }
        }
    }
    if stamped_local + stamped_untrusted > 0 {
        tracing::info!(
            "procedure trust backfill: stamped {stamped_local} local, {stamped_untrusted} imported-untrusted"
        );
    }
    (stamped_local, stamped_untrusted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use uuid::Uuid;

    /// Mirrors server.rs's established StubEmbedder (1.8.0 lesson): the
    /// per-element `(i as f32 + 1.0) * 0.001` offset keeps the vector a
    /// non-integer FLOAT[] so LadybugDB's ARRAY_DISTANCE binder accepts it,
    /// and the same offset on write and read keeps equality assertions.
    struct StubEmbedder;

    #[async_trait::async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 4];
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += b as f32;
            }
            for (i, x) in v.iter_mut().enumerate() {
                *x += (i as f32 + 1.0) * 0.001;
            }
            Ok(v)
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    // ---- fixtures ---------------------------------------------------------

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ns-mesh-import-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A mesh joined on the `probe` namespace, committed so HEAD exists.
    fn mesh_with_commit(tag: &str) -> (PathBuf, MeshStorage) {
        let base = scratch(tag);
        let mesh = MeshStorage::init(
            &base,
            "node-import-test",
            "import-tests",
            &["probe".to_string()],
            None,
        )
        .unwrap();
        (base, mesh)
    }

    fn mesh_id_str(mesh: &MeshStorage) -> String {
        mesh.manifest().mesh_id.to_string()
    }

    /// Rewrite the on-disk manifest to a new `epoch`, commit it (the local
    /// half of an epoch migration), and reopen the mesh so its cached
    /// manifest reflects the new epoch.
    fn reopen_with_epoch(mesh: &MeshStorage, epoch: u32) -> MeshStorage {
        let mut manifest = mesh.manifest().clone();
        manifest.epoch = epoch;
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(
            mesh.root().join(super::super::storage::MANIFEST_FILE_NAME),
            format!("{json}\n"),
        )
        .unwrap();
        mesh.commit_all("epoch migration", "node-import-test")
            .unwrap();
        MeshStorage::open(mesh.root()).unwrap()
    }

    /// Rewrite the on-disk manifest to carry `signers` as its signer
    /// allowlist, commit it, and reopen the mesh so its cached manifest
    /// reflects the allowlist — the signer analogue of
    /// `reopen_with_epoch`.
    fn reopen_with_signers(
        mesh: &MeshStorage,
        signers: Vec<super::super::manifest::SignerEntry>,
    ) -> MeshStorage {
        let mut manifest = mesh.manifest().clone();
        manifest.signers = signers;
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(
            mesh.root().join(super::super::storage::MANIFEST_FILE_NAME),
            format!("{json}\n"),
        )
        .unwrap();
        mesh.commit_all("signer allowlist", "node-import-test")
            .unwrap();
        MeshStorage::open(mesh.root()).unwrap()
    }

    fn lifecycle_json(state: &str, archived_at: Option<chrono::DateTime<chrono::Utc>>) -> Value {
        json!({
            "state": state,
            "created_at": "2026-10-11T00:00:00Z",
            "created_by": "node-peer-1",
            "superseded_by": Value::Null,
            "archived_at": archived_at.map(|t| serde_json::to_value(t).unwrap()).unwrap_or(Value::Null),
            "tombstoned_at": Value::Null
        })
    }

    fn record_value(
        schema_version: &str,
        id: Uuid,
        namespace: &str,
        memory_type: &str,
        state: &str,
        archived_at: Option<chrono::DateTime<chrono::Utc>>,
        content: Value,
    ) -> Value {
        json!({
            "schema_version": schema_version,
            "id": id.to_string(),
            "namespace": namespace,
            "memory_type": memory_type,
            "lifecycle": lifecycle_json(state, archived_at),
            "content": content,
            "metadata": {"eligibility": "shareable", "trust": "untrusted"},
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": "untrusted"
        })
    }

    fn simple_rule(id: Uuid, text: &str) -> Value {
        record_value(
            "synaptic-mesh-record/v1",
            id,
            "probe",
            "rule",
            "active",
            None,
            json!({"text": text}),
        )
    }

    /// Write `<ns>/{memories,procedures}/<stem>.json` into the mesh and
    /// commit as the peer node. Returns the rel path.
    fn commit_record_json(
        mesh: &MeshStorage,
        ns: &str,
        dir: &str,
        stem: &str,
        body: &str,
    ) -> String {
        let d = mesh.root().join(ns).join(dir);
        std::fs::create_dir_all(&d).unwrap();
        let rel = format!("{ns}/{dir}/{stem}.json");
        std::fs::write(mesh.root().join(&rel), body).unwrap();
        mesh.commit_all("peer record", "node-peer-1").unwrap();
        rel
    }

    async fn fresh_store(tag: &str) -> std::sync::Arc<dyn VectorStore> {
        let dir = std::env::temp_dir().join(format!(
            "ns-mesh-import-db-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::LadybugStore::for_testing(&dir, 4).expect("open temp store"),
        );
        store
    }

    fn emb() -> std::sync::Arc<dyn Embedder> {
        std::sync::Arc::new(StubEmbedder)
    }

    async fn row_count(store: &std::sync::Arc<dyn VectorStore>, ns: &str) -> usize {
        store.init(ns).await.unwrap();
        store.list(ns, None, None).await.unwrap().len()
    }

    /// Slice T fix: a record that exists ONLY inside a `.bundle.zst`
    /// artifact is a replay candidate — the bundle decode is no longer
    /// dropped after a debug log. This record has no `.json` sibling in the
    /// record tree; the bundle is its only carrier, and it must land in the
    /// store exactly like a committed `.json` record would.
    #[tokio::test]
    async fn bundle_only_record_is_imported_and_applied_into_the_store() {
        let (_base, mesh) = mesh_with_commit("bundle-only");
        let id = Uuid::new_v4();
        let value = simple_rule(id, "shares via bundle only");
        let record: MeshRecord = serde_json::from_value(value).unwrap();
        let written = crate::synaptic_mesh::worker::write_compressed_bundle(
            &mesh.root().join("bundles"),
            "test",
            &[record],
        )
        .expect("bundle writes");
        assert!(
            written.to_string_lossy().ends_with("test.bundle.zst"),
            "bundle artifact shape: {}",
            written.display()
        );
        assert!(
            !mesh
                .root()
                .join("probe")
                .join("memories")
                .join(format!("{id}.json"))
                .exists(),
            "fixture sanity: the record has NO `.json` sibling"
        );
        // The bundle artifact must be TRACKED mesh content for replay:
        // a plain `commit_all` leaves it untracked because storage's
        // `/bundles/*.bundle.zst` rule git-ignores it (local-only), so
        // the fixture commits the bootstrap files, then force-tracks
        // the bundle — the legitimate transport route.
        mesh.commit_all("bundle artifact", "node-peer-1").unwrap();
        force_track_bundle(&mesh, "bundles/test.bundle.zst");
        let store = fresh_store("bundle-only").await;

        let report =
            apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
                .await
                .expect("full-tree replay applies the bundle record");

        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert!(report.ignored.is_empty(), "report: {report:?}");
        assert_eq!(
            row_count(&store, "probe").await,
            1,
            "the bundle-only record landed in the store"
        );
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied from bundle");
        assert_eq!(payload.content, "shares via bundle only");
    }

    // ---- bug #44 (untracked/ignored limb): local-only bundles ------------
    //
    // A bundle file that is git-ignored or untracked in the mesh checkout
    // is local-only (drain bundles are exactly that: storage installs
    // `/bundles/*.bundle.zst`). It must never be replayed: it is invisible
    // to the dirty gate (ignored files do not show in porcelain) and no
    // revision order is compared, so a stale local bundle would silently
    // roll a live row back on a quiet import pass. A TRACKED bundle is
    // the legitimate transport route and must keep replaying.

    /// Run one git subcommand in a test-fixture checkout; return stdout.
    fn git_fixture(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs in fixture");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Force-track a bundle artifact (`git add -f` + commit): the
    /// `/bundles/*.bundle.zst` ignore rule installed by storage makes a
    /// plain `commit_all` / `git add -A` leave every bundle untracked, so
    /// a fixture that means "this bundle travelled through git" must say
    /// so explicitly.
    fn force_track_bundle(mesh: &MeshStorage, rel: &str) {
        git_fixture(mesh.root(), &["add", "-f", "--", rel]);
        git_fixture(
            mesh.root(),
            &[
                "-c",
                "user.name=node-peer-1",
                "-c",
                "user.email=node-peer-1@synaptic-mesh.local",
                "commit",
                "-m",
                "tracked bundle artifact",
                "--",
                rel,
            ],
        );
        let tracked = git_fixture(mesh.root(), &["ls-files", "--", "bundles/"]);
        assert!(
            tracked.lines().any(|l| l == rel),
            "fixture sanity: {rel} must be tracked, ls-files: {tracked}"
        );
    }

    /// RED for #44's ignored limb: a record advances to a newer revision
    /// through the committed `.json` route; a stale bundle holding the
    /// older revision then sits in `bundles/` as a git-ignored local-only
    /// file. A quiet import pass (cursor already at HEAD, no committed
    /// change) must keep the newer live row — on the unfixed tree the
    /// bundle scan replays the stale bundle and the row rolls back.
    #[tokio::test]
    async fn ignored_stale_bundle_does_not_roll_back_newer_live_row() {
        let (_base, mesh) = mesh_with_commit("ignored-stale-bundle");
        let id = Uuid::new_v4();
        // Newer revision, applied/live via the committed `.json` route.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "newer revision")).unwrap(),
        );
        let store = fresh_store("ignored-stale-bundle").await;
        let served = ["probe".to_string()];
        let first = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .expect("first pass applies the newer revision");
        assert_eq!(first.applied, 1, "report: {first:?}");

        // Stale older revision, carried ONLY by a local-only bundle:
        // written the way a drain writes it, never force-tracked.
        let stale: MeshRecord =
            serde_json::from_value(simple_rule(id, "older stale revision")).unwrap();
        crate::synaptic_mesh::worker::write_compressed_bundle(
            &mesh.root().join("bundles"),
            "stale",
            &[stale],
        )
        .expect("bundle writes");
        // Fixture sanity: the bundle is git-ignored (the real
        // `/bundles/*.bundle.zst` rule storage installs) and untracked.
        let check = std::process::Command::new("git")
            .args(["check-ignore", "--quiet", "bundles/stale.bundle.zst"])
            .current_dir(mesh.root())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git check-ignore runs");
        assert!(
            check.status.success(),
            "fixture sanity: bundles/stale.bundle.zst must be git-ignored"
        );
        let tracked = git_fixture(mesh.root(), &["ls-files", "--", "bundles/"]);
        assert!(
            !tracked.contains("stale.bundle.zst"),
            "fixture sanity: stale bundle must be untracked, ls-files: {tracked}"
        );

        // Quiet pass: no new commits, cursor already at HEAD.
        let second = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .expect("quiet pass completes");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("live row still present");
        assert_eq!(
            payload.content, "newer revision",
            "local-only stale bundle must not roll back the live row; report: {second:?}"
        );
        assert_eq!(second.applied, 0, "report: {second:?}");
    }

    /// Control: the same bundle shape, properly TRACKED (force-added and
    /// committed), carrying a new record, must still replay. This passes
    /// both before and after the local-only barrier lands.
    #[tokio::test]
    async fn tracked_bundle_carrying_new_record_still_replays() {
        let (_base, mesh) = mesh_with_commit("tracked-bundle-control");
        let id = Uuid::new_v4();
        let record: MeshRecord =
            serde_json::from_value(simple_rule(id, "tracked bundle record")).unwrap();
        crate::synaptic_mesh::worker::write_compressed_bundle(
            &mesh.root().join("bundles"),
            "tracked",
            &[record],
        )
        .expect("bundle writes");
        // Commit the mesh bootstrap files first (a plain `commit_all`
        // leaves the ignored bundle itself untracked), then force-track
        // the bundle on top so the checkout is clean and the bundle is
        // genuinely tracked content.
        mesh.commit_all("bootstrap", "node-peer-1").unwrap();
        force_track_bundle(&mesh, "bundles/tracked.bundle.zst");
        let store = fresh_store("tracked-bundle-control").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("tracked bundle replay completes");
        assert_eq!(report.applied, 1, "report: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied from tracked bundle");
        assert_eq!(payload.content, "tracked bundle record");
    }

    // ---- bug #44 (tracked limb): stale tracked bundle revision guard ----
    //
    // A TRACKED bundle is legitimate transport (see the barrier above),
    // so it replays — but on a diff pass a bundle-only record whose
    // UUIDv7 `update_id` predates the accepted revision of a live row in
    // the same lifecycle state must not roll that row back. The ordering
    // signal is the v7 timestamp prefix (Gate G2 option (a)).

    /// Millisecond timestamps for deterministic v7 update_ids.
    const REV_OLD_MILLIS: u64 = 1_700_000_000_000;
    const REV_NEW_MILLIS: u64 = 1_700_000_120_000;

    /// A deterministic UUIDv7 whose timestamp prefix is `millis`; `fill`
    /// varies the counter/random tail so distinct ids can share a millis
    /// value (the tie case).
    fn v7_update_id(millis: u64, fill: u8) -> Uuid {
        uuid::Builder::from_unix_timestamp_millis(millis, &[fill; 10]).into_uuid()
    }

    /// A record JSON with an explicit lifecycle state and `update_id`
    /// (the `record_value` analogue with the revision under test control).
    fn revision_value(id: Uuid, state: &str, text: &str, update_id: Uuid) -> Value {
        let archived_at = if state == "archived" {
            Some(chrono::Utc::now())
        } else {
            None
        };
        let mut value = record_value(
            "synaptic-mesh-record/v1",
            id,
            "probe",
            "rule",
            state,
            archived_at,
            json!({"text": text}),
        );
        value["update_id"] = json!(update_id.to_string());
        value
    }

    /// Pass 1 applies `local` through the committed `.json` route; the
    /// candidate then arrives ONLY inside a force-tracked bundle and is
    /// presented on a second pass. `full_tree` drops the cursor (and
    /// withdraws the `.json` carrier, so the bundle copy is not deduped
    /// against its `.json` sibling) to make that pass a full-tree
    /// reconciliation; otherwise it is a diff pass.
    async fn tracked_bundle_second_pass(
        tag: &str,
        local: &Value,
        candidate: MeshRecord,
        full_tree: bool,
    ) -> (ApplyReport, std::sync::Arc<dyn VectorStore>) {
        let (_base, mesh) = mesh_with_commit(tag);
        let id = local["id"].as_str().expect("local record id").to_string();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id,
            &serde_json::to_string(local).unwrap(),
        );
        let store = fresh_store(tag).await;
        let served = ["probe".to_string()];
        let first = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("first pass applies the local revision");
        assert_eq!(first.applied, 1, "first report: {first:?}");
        crate::synaptic_mesh::worker::write_compressed_bundle(
            &mesh.root().join("bundles"),
            "rev",
            &[candidate],
        )
        .expect("bundle writes");
        mesh.commit_all("bundle artifact", "node-peer-1").unwrap();
        force_track_bundle(&mesh, "bundles/rev.bundle.zst");
        if full_tree {
            std::fs::remove_file(
                mesh.root()
                    .join("probe")
                    .join("memories")
                    .join(format!("{id}.json")),
            )
            .unwrap();
            mesh.commit_all("withdraw json carrier", "node-peer-1")
                .unwrap();
            std::fs::remove_file(cursor_file(mesh.root())).unwrap();
        }
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("second pass completes");
        assert_eq!(second.full_tree, full_tree, "pass shape: {second:?}");
        (second, store)
    }

    /// The v7-millis helper: deterministic v7 ids report
    /// `Version::SortRand` and round-trip their millis; a v4 id yields
    /// `None`; two v7 ids at the same millis extract EQUAL timestamps
    /// (the tie the guard treats as inert).
    #[test]
    fn update_id_v7_millis_extracts_v7_timestamps_only() {
        let old = v7_update_id(REV_OLD_MILLIS, 0x11);
        assert_eq!(
            old.get_version(),
            Some(uuid::Version::SortRand),
            "fixture sanity: the constructed id is UUIDv7"
        );
        assert_eq!(update_id_v7_millis(&old), Some(REV_OLD_MILLIS));
        assert_eq!(
            update_id_v7_millis(&v7_update_id(REV_NEW_MILLIS, 0x12)),
            Some(REV_NEW_MILLIS)
        );
        assert_eq!(
            update_id_v7_millis(&Uuid::new_v4()),
            None,
            "v4 has no timestamp prefix"
        );
        assert_eq!(
            update_id_v7_millis(&v7_update_id(REV_NEW_MILLIS, 0x13)),
            update_id_v7_millis(&v7_update_id(REV_NEW_MILLIS, 0x14)),
            "same-millis v7 ids tie"
        );
    }

    async fn stored_content(store: &std::sync::Arc<dyn VectorStore>, id: Uuid) -> String {
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row present");
        payload.content
    }

    /// RED repro (tracked twin of
    /// `ignored_stale_bundle_does_not_roll_back_newer_live_row`): the
    /// bundle is force-tracked/committed in the mesh history, the local
    /// row sits at a NEWER v7 revision, and the bundle carries the same
    /// record at an OLDER v7 revision, same live state, older content.
    /// On the unguarded tree the diff pass applies the bundle record and
    /// the row rolls back.
    #[tokio::test]
    async fn tracked_stale_bundle_does_not_roll_back_newer_live_row() {
        let id = Uuid::new_v4();
        let local = revision_value(
            id,
            "active",
            "newer revision",
            v7_update_id(REV_NEW_MILLIS, 0xA1),
        );
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "active",
            "older stale revision",
            v7_update_id(REV_OLD_MILLIS, 0xA2),
        ))
        .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("tracked-stale-bundle", &local, candidate, false).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(
            !report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert!(
            report
                .ignored
                .iter()
                .any(|e| e.contains("stale bundle revision")),
            "the stale-revision refusal is reported: {report:?}"
        );
        assert_eq!(
            stored_content(&store, id).await,
            "newer revision",
            "tracked stale bundle must not roll back the live row; report: {report:?}"
        );
    }

    /// Control: the same shape with a NEWER bundle revision applies.
    #[tokio::test]
    async fn tracked_bundle_with_newer_revision_still_applies() {
        let id = Uuid::new_v4();
        let local = revision_value(
            id,
            "active",
            "older revision",
            v7_update_id(REV_OLD_MILLIS, 0xB1),
        );
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "active",
            "newer revision",
            v7_update_id(REV_NEW_MILLIS, 0xB2),
        ))
        .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("tracked-newer-bundle", &local, candidate, false).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert_eq!(stored_content(&store, id).await, "newer revision");
    }

    /// Control: the stale-bundle input on a FULL-TREE pass is a
    /// concurrent conflict, not the stale-revision refusal (the guard is
    /// inert when `concurrent`).
    #[tokio::test]
    async fn tracked_stale_bundle_on_full_tree_pass_conflicts_not_stale_refused() {
        let id = Uuid::new_v4();
        let local = revision_value(
            id,
            "active",
            "newer revision",
            v7_update_id(REV_NEW_MILLIS, 0xC1),
        );
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "active",
            "older stale revision",
            v7_update_id(REV_OLD_MILLIS, 0xC2),
        ))
        .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("tracked-stale-full-tree", &local, candidate, true).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert_eq!(report.conflicted, 1, "report: {report:?}");
        assert!(
            report.conflicted_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert!(
            !report
                .ignored
                .iter()
                .any(|e| e.contains("stale bundle revision")),
            "no stale refusal on a full-tree pass: {report:?}"
        );
        assert_eq!(stored_content(&store, id).await, "newer revision");
    }

    /// Control: a bundle candidate whose `update_id` EQUALS the accepted
    /// revision is Skipped (the pre-existing equal-revision skip), never
    /// the stale refusal.
    #[tokio::test]
    async fn tracked_bundle_with_equal_update_id_is_skipped() {
        let id = Uuid::new_v4();
        let accepted = v7_update_id(REV_NEW_MILLIS, 0xD1);
        let local = revision_value(id, "active", "accepted revision", accepted);
        let candidate: MeshRecord =
            serde_json::from_value(revision_value(id, "active", "other content", accepted))
                .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("tracked-equal-bundle", &local, candidate, false).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert_eq!(report.skipped_update_id, 1, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        assert_eq!(stored_content(&store, id).await, "accepted revision");
    }

    /// Control (Gate G2 Test C negative): a NEWER archive transition
    /// arriving bundle-only on a diff pass still applies — state
    /// transitions are out of the guard's scope.
    #[tokio::test]
    async fn tracked_bundle_newer_archive_transition_still_applies() {
        let id = Uuid::new_v4();
        let local = revision_value(
            id,
            "active",
            "shared content",
            v7_update_id(REV_OLD_MILLIS, 0xE1),
        );
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "archived",
            "shared content",
            v7_update_id(REV_NEW_MILLIS, 0xE2),
        ))
        .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("tracked-archive-bundle", &local, candidate, false).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            !report
                .ignored
                .iter()
                .any(|e| e.contains("stale bundle revision")),
            "report: {report:?}"
        );
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row present");
        assert_eq!(
            payload.metadata["mesh"]["lifecycle_state"],
            json!("archived"),
            "the archive transition landed: {:?}",
            payload.metadata
        );
    }

    /// Boundary: v7 timestamp TIES are inert — a bundle candidate at the
    /// same millis as the accepted revision (different tail) applies
    /// exactly as it does today.
    #[tokio::test]
    async fn tracked_bundle_with_tied_v7_timestamp_still_applies() {
        let id = Uuid::new_v4();
        let accepted = v7_update_id(REV_NEW_MILLIS, 0xF1);
        let candidate_id = v7_update_id(REV_NEW_MILLIS, 0xF2);
        assert_ne!(accepted, candidate_id, "tie needs distinct update_ids");
        let local = revision_value(id, "active", "local tied revision", accepted);
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "active",
            "candidate tied revision",
            candidate_id,
        ))
        .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("tracked-tied-bundle", &local, candidate, false).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert_eq!(stored_content(&store, id).await, "candidate tied revision");
    }

    /// Boundary (the disclosed v7-only scope): a stale-content candidate
    /// whose `update_id` is NOT v7 — or whose accepted revision is not
    /// v7 — is not refused; it behaves exactly as today (applies).
    #[tokio::test]
    async fn stale_bundle_with_non_v7_update_id_is_not_refused() {
        // Candidate side non-v7.
        let id = Uuid::new_v4();
        let local = revision_value(
            id,
            "active",
            "newer revision",
            v7_update_id(REV_NEW_MILLIS, 0x91),
        );
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "active",
            "v4 candidate revision",
            Uuid::new_v4(),
        ))
        .unwrap();
        let (report, store) =
            tracked_bundle_second_pass("nonv7-candidate-bundle", &local, candidate, false).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            !report
                .ignored
                .iter()
                .any(|e| e.contains("stale bundle revision")),
            "report: {report:?}"
        );
        assert_eq!(stored_content(&store, id).await, "v4 candidate revision");

        // Accepted side non-v7.
        let id2 = Uuid::new_v4();
        let local2 = revision_value(id2, "active", "local v4 revision", Uuid::new_v4());
        let candidate2: MeshRecord = serde_json::from_value(revision_value(
            id2,
            "active",
            "v7 candidate revision",
            v7_update_id(REV_OLD_MILLIS, 0x92),
        ))
        .unwrap();
        let (report2, store2) =
            tracked_bundle_second_pass("nonv7-accepted-bundle", &local2, candidate2, false).await;
        assert_eq!(report2.applied, 1, "report: {report2:?}");
        assert_eq!(stored_content(&store2, id2).await, "v7 candidate revision");
    }

    /// Scope negative (cross-state): the guard requires BOTH sides live
    /// in the SAME state, so an OLDER v7 bundle candidate in a DIFFERENT
    /// live state than the accepted row must NOT be stale-refused — it
    /// falls through to `classify_merge`, where Active (local) vs
    /// Archived (candidate) with changed content is the `archive + edit`
    /// combination. The combination's semantics apply: the edited
    /// content — here the LOCAL row's, since the local side is the
    /// Active one (see the `ApplyCombination` arm in `apply_one`) — is
    /// kept, and the stored row is Archived.
    #[tokio::test]
    async fn tracked_stale_cross_state_bundle_still_combines() {
        let id = Uuid::new_v4();
        let local = revision_value(
            id,
            "active",
            "newer revision",
            v7_update_id(REV_NEW_MILLIS, 0x17),
        );
        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "archived",
            "candidate archived edit",
            v7_update_id(REV_OLD_MILLIS, 0x18),
        ))
        .unwrap();
        let (report, store) = tracked_bundle_second_pass(
            "tracked-stale-cross-state",
            &local,
            candidate,
            false,
        )
        .await;
        assert!(
            !report
                .ignored
                .iter()
                .any(|e| e.contains("stale bundle revision")),
            "cross-state candidate must not be stale-refused: {report:?}"
        );
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert!(
            report
                .combinations
                .iter()
                .any(|e| e.contains("archive + edit")),
            "the candidate reached the combination machinery: {report:?}"
        );
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row present");
        assert_eq!(
            payload.metadata["mesh"]["lifecycle_state"],
            json!("archived"),
            "the combination stored the row archived: {:?}",
            payload.metadata
        );
        assert_eq!(
            payload.content, "newer revision",
            "combination semantics: the Active (local) side's edited \
             content is kept, not the candidate's; report: {report:?}"
        );
    }

    /// Scope negative (private-row precedence): the private-memory guard
    /// in `apply_one` answers BEFORE the stale-bundle guard is ever
    /// consulted (a private row has no mesh `update_id`, so the stale
    /// guard's `Some(accepted)` branch is unreachable for it). The
    /// candidate below is shaped exactly as a stale candidate would be
    /// against a mesh-owned Active row at the newer revision — live
    /// Active state, changed content, an OLDER v7 `update_id` — and it
    /// arrives via the bundle route on a diff pass, the only route/pass
    /// where the stale guard can fire. The report must carry the
    /// private-memory refusal and NOT the stale refusal, and the private
    /// row must be untouched.
    #[tokio::test]
    async fn tracked_stale_bundle_against_private_row_reports_private_not_stale() {
        let (_base, mesh) = mesh_with_commit("tracked-stale-private");
        // Commit the mesh bootstrap files so the checkout is clean for
        // the passes below (a plain `commit_all` leaves no record yet).
        mesh.commit_all("bootstrap", "node-peer-1").unwrap();
        let id = Uuid::new_v4();
        let store = fresh_store("tracked-stale-private").await;
        // Seed the private row: no mesh provenance at all (mirrors
        // `private_row_with_same_id_is_never_stomped`).
        store.init("probe").await.unwrap();
        store
            .upsert(
                "probe",
                &id.to_string(),
                emb().embed("my private note").await.unwrap(),
                MemoryPayload {
                    content: "my private note".to_string(),
                    user_id: "me".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"source": "hand-written"}),
                },
            )
            .await
            .unwrap();
        // Prime the cursor with a pass over the still record-less mesh,
        // so the bundle presentation below is a DIFF pass (the stale
        // guard is inert on full-tree passes, which would make the
        // precedence claim vacuous).
        let served = ["probe".to_string()];
        let priming = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("priming pass completes");
        assert_eq!(priming.applied, 0, "priming report: {priming:?}");

        let candidate: MeshRecord = serde_json::from_value(revision_value(
            id,
            "active",
            "older stale revision",
            v7_update_id(REV_OLD_MILLIS, 0x19),
        ))
        .unwrap();
        crate::synaptic_mesh::worker::write_compressed_bundle(
            &mesh.root().join("bundles"),
            "priv",
            &[candidate],
        )
        .expect("bundle writes");
        mesh.commit_all("bundle artifact", "node-peer-1").unwrap();
        force_track_bundle(&mesh, "bundles/priv.bundle.zst");

        let report = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("bundle presentation pass completes");
        assert!(
            !report.full_tree,
            "the presentation must be a diff pass: {report:?}"
        );
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(
            report
                .ignored
                .iter()
                .any(|e| e.contains("private memory owns id")),
            "the private-memory refusal is reported: {report:?}"
        );
        assert!(
            !report
                .ignored
                .iter()
                .any(|e| e.contains("stale bundle revision")),
            "the stale refusal must not pre-empt the private guard: {report:?}"
        );
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("private row still present");
        assert_eq!(payload.content, "my private note", "content preserved");
        assert!(
            payload.metadata.get("mesh").is_none(),
            "no provenance was injected into a private row"
        );
    }

    // ---- issue #43: bundle gate parity -----------------------------------
    //
    // A bundle-only record must pass the same pre-apply gate as a `.json`
    // candidate (validation, then epoch, then signature) before it is
    // applied. The tests below exercise behaviour on both routes, never
    // the id == file-stem check: bundle records have no file stem.

    fn bundle_record(value: Value) -> MeshRecord {
        serde_json::from_value(value).unwrap()
    }

    /// Replay one record carried ONLY inside a `.bundle.zst`
    /// (force-tracked mesh content — see `force_track_bundle`: a plain
    /// commit leaves an ignored bundle untracked and local-only, and
    /// local-only bundles never replay) into a fresh store.
    /// `manifest_epoch`
    /// migrates the mesh manifest to that epoch before the bundle lands.
    async fn run_bundle_route(
        tag: &str,
        record: MeshRecord,
        manifest_epoch: Option<u32>,
    ) -> (ApplyReport, std::sync::Arc<dyn VectorStore>) {
        run_bundle_route_with_signers(tag, record, manifest_epoch, Vec::new()).await
    }

    /// `run_bundle_route` with the mesh manifest's signer allowlist
    /// populated: the replay then verifies signatures against a
    /// NON-empty `manifest.signers`, so a `Verified` disposition is
    /// trusted verification, not just self-consistent crypto.
    async fn run_bundle_route_with_signers(
        tag: &str,
        record: MeshRecord,
        manifest_epoch: Option<u32>,
        signers: Vec<super::super::manifest::SignerEntry>,
    ) -> (ApplyReport, std::sync::Arc<dyn VectorStore>) {
        let (_base, mesh) = mesh_with_commit(tag);
        let mesh = match manifest_epoch {
            Some(epoch) => reopen_with_epoch(&mesh, epoch),
            None => mesh,
        };
        let mesh = if signers.is_empty() {
            mesh
        } else {
            reopen_with_signers(&mesh, signers)
        };
        crate::synaptic_mesh::worker::write_compressed_bundle(
            &mesh.root().join("bundles"),
            "test",
            &[record],
        )
        .expect("bundle writes");
        mesh.commit_all("bundle artifact", "node-peer-1").unwrap();
        force_track_bundle(&mesh, "bundles/test.bundle.zst");
        let store = fresh_store(tag).await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("bundle-route replay completes");
        (report, store)
    }

    /// Replay the same record as a committed `.json` candidate into a
    /// fresh store — the other half of every parity assertion.
    async fn run_json_route(
        tag: &str,
        value: &Value,
        manifest_epoch: Option<u32>,
    ) -> (ApplyReport, std::sync::Arc<dyn VectorStore>) {
        let (_base, mesh) = mesh_with_commit(tag);
        let mesh = match manifest_epoch {
            Some(epoch) => reopen_with_epoch(&mesh, epoch),
            None => mesh,
        };
        let id = value["id"].as_str().expect("record value carries an id");
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            id,
            &serde_json::to_string(value).unwrap(),
        );
        let store = fresh_store(tag).await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("json-route replay completes");
        (report, store)
    }

    /// The ignore reasons of a report, stripped of the route-specific
    /// source prefix (`<rel path>: ` on the `.json` route,
    /// `<bundle path> (<record id>): ` on the bundle route) so reasons
    /// can be compared across routes.
    fn ignore_reasons(report: &ApplyReport) -> Vec<String> {
        report
            .ignored
            .iter()
            .map(|entry| {
                entry
                    .split_once(": ")
                    .map(|(_, reason)| reason.to_string())
                    .unwrap_or_else(|| entry.clone())
            })
            .collect()
    }

    /// A bundle-only record with an unknown schema version is rejected
    /// by validation and never applied — the `.json` route's refusal,
    /// now on the bundle route too. Validation precedes signature
    /// accounting, so neither signature counter moves.
    #[tokio::test]
    async fn bundle_only_record_with_unknown_schema_version_is_rejected_and_not_applied() {
        let id = Uuid::new_v4();
        let value = record_value(
            "synaptic-mesh-record/v99",
            id,
            "probe",
            "rule",
            "active",
            None,
            json!({"text": "from the future"}),
        );
        let (report, store) = run_bundle_route("bundle-schema", bundle_record(value), None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.unsigned_accepted, 0, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("synaptic-mesh-record/v99"),
            "the unknown version is named: {joined}"
        );
        assert!(
            joined.contains("synaptic-mesh-record/v1"),
            "the supported version is named: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// A bundle-only record carrying a detectable secret is refused by
    /// the secret scan inside validation, named in the report, and
    /// never applied.
    #[tokio::test]
    async fn bundle_only_record_with_secret_is_rejected_and_not_applied() {
        let id = Uuid::new_v4();
        let pat = format!("ghp_{}", "A".repeat(36));
        let value = simple_rule(id, &format!("use this token: {pat}"));
        let (report, store) = run_bundle_route("bundle-secret", bundle_record(value), None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.unsigned_accepted, 0, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("secret scan"),
            "the refusal names the scan: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// A bundle-only record carrying a runtime procedure counter is
    /// refused by validation and never applied: counters are strictly
    /// local and never travel, on either route. The refusal's report
    /// entry names the record (the bundle route's `rel` carries the
    /// record id).
    #[tokio::test]
    async fn bundle_only_record_with_runtime_counter_is_rejected_and_not_applied() {
        let id = Uuid::new_v4();
        let mut value = simple_rule(id, "carries a local counter");
        value["metadata"]["remaining_fires"] = json!(3);
        let (report, store) = run_bundle_route("bundle-counter", bundle_record(value), None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.unsigned_accepted, 0, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("runtime procedure counters"),
            "the refusal names the counters: {joined}"
        );
        assert!(
            joined.contains(&id.to_string()),
            "the entry identifies the record: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// The per-record byte cap is part of the portable validation
    /// subset: a bundle record whose serialized form exceeds
    /// `MAX_RECORD_BYTES` is refused by the bundle validator with the
    /// same reason the `.json` route gives an oversized file. (The
    /// route-level consequence of an over-cap payload — wholesale
    /// decode refusal, nothing applied — is covered end-to-end by
    /// `bundle_with_over_cap_record_is_not_applied_end_to_end` below.)
    #[test]
    fn bundle_validation_rejects_record_over_per_record_cap() {
        let id = Uuid::new_v4();
        let big_text = "x".repeat(MAX_RECORD_BYTES + 1);
        let record = bundle_record(simple_rule(id, &big_text));
        let err = validate_bundle_record(record, &["probe".to_string()])
            .expect_err("an over-cap bundle record must be refused");
        assert!(
            err.contains("per-record cap"),
            "the reason names the cap: {err}"
        );
        assert!(
            err.contains(&MAX_RECORD_BYTES.to_string()),
            "the cap value is named: {err}"
        );
    }

    /// Over-cap, end-to-end: the real bundle writer carries a record
    /// whose serialized form exceeds `MAX_RECORD_BYTES` without
    /// refusing (`write_compressed_bundle` serializes and compresses
    /// with no size check), so the bundle CAN be constructed — but its
    /// decompressed payload (the one-element record array) exceeds
    /// the aggregate decode cap (`DEFAULT_MESH_MAX_DECOMPRESSED_BYTES`,
    /// also 1 MiB), so the scan cannot decode it at all: it is logged
    /// and skipped wholesale, with no report entry and nothing
    /// applied. Enforcement on this route is therefore the decode
    /// aggregate cap; the validator-level per-record check on the
    /// serialized form (the test above) pins the measure at the
    /// validation layer, for any path that reaches it.
    #[tokio::test]
    async fn bundle_with_over_cap_record_is_not_applied_end_to_end() {
        let id = Uuid::new_v4();
        let big_text = "x".repeat(MAX_RECORD_BYTES + 1);
        let record = bundle_record(simple_rule(id, &big_text));
        let (report, store) = run_bundle_route("bundle-over-cap", record, None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert!(
            report.ignored.is_empty(),
            "an undecodable bundle leaves no report entry: {report:?}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// A bundle-only record born in an older epoch is rejected by the
    /// epoch gate specifically: the refusal names both epochs, no
    /// signature counter moves (the epoch gate precedes signature
    /// verification), and the record is not quietly counted as a
    /// replay skip either.
    #[tokio::test]
    async fn bundle_only_record_with_old_epoch_is_rejected_by_epoch_gate() {
        let id = Uuid::new_v4();
        // Born in epoch 1; the mesh manifest migrates to epoch 2.
        let value = simple_rule(id, "published before the purge");
        let (report, store) =
            run_bundle_route("bundle-epoch-old", bundle_record(value), Some(2)).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(
            report.skipped_update_id, 0,
            "an epoch refusal is not a replay skip: {report:?}"
        );
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.unsigned_accepted, 0, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("old-epoch"),
            "the refusal names the old epoch: {joined}"
        );
        assert!(
            joined.contains("origin_mesh_epoch 1") && joined.contains("manifest epoch 2"),
            "both epochs are named: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// A bundle-only record in a namespace this node does not serve is
    /// reported in `ignored`, not silently dropped: the validation
    /// step's served-namespace refusal replaces the bundle loop's old
    /// silent `continue`.
    #[tokio::test]
    async fn bundle_only_record_in_unserved_namespace_is_reported_not_silently_dropped() {
        let id = Uuid::new_v4();
        let value = record_value(
            "synaptic-mesh-record/v1",
            id,
            "elsewhere",
            "rule",
            "active",
            None,
            json!({"text": "belongs to another team"}),
        );
        let (report, store) = run_bundle_route("bundle-unserved", bundle_record(value), None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("elsewhere") && joined.contains("is not served"),
            "the unserved refusal is named: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// The legitimate replay path stays open: a valid, signed
    /// bundle-only record passes the unified gate, counts as verified,
    /// and lands in the store. The fixture manifest's signer allowlist
    /// is populated with the test key's entry (DID + public key, as
    /// `SignerEntry` requires), so verification runs against a
    /// NON-empty `manifest.signers`: `verified_signed == 1` here is
    /// trusted verification — a signer absent from the allowlist
    /// would be refused as unauthorized, not verified.
    #[tokio::test]
    async fn valid_signed_bundle_only_record_still_applies() {
        let id = Uuid::new_v4();
        let mut record = bundle_record(simple_rule(id, "signed bundle record"));
        let key_dir = scratch("bundle-signed-key");
        let key_path = key_dir.join("signing.key");
        crate::synaptic_mesh::worker::sign_record(&mut record, &key_path).expect("record signs");
        let signers = vec![super::super::manifest::SignerEntry {
            did: record
                .signer_did
                .clone()
                .expect("a signed record carries a signer DID"),
            public_key: record
                .public_key
                .clone()
                .expect("a signed record carries a public key"),
            role: "signer".to_string(),
        }];
        let (report, store) =
            run_bundle_route_with_signers("bundle-signed", record, None, signers).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert_eq!(report.verified_signed, 1, "report: {report:?}");
        assert_eq!(report.unsigned_accepted, 0, "report: {report:?}");
        assert_eq!(report.unauthorized_signer, 0, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied from signed bundle");
        assert_eq!(payload.content, "signed bundle record");
    }

    /// A bundle-only record that is SIGNED over the canonical-JSON
    /// payload and carries an RDF block with a Phase-1 violation (empty
    /// `rdf.quads`) is still APPLIED: the RDF diagnostic is attached
    /// AFTER signature verification, so the signature is checked over the
    /// record exactly as it arrived.
    ///
    /// Before the deferral the diagnostic was inserted into
    /// `record.metadata` before `verify_record_signature`. The canonical
    /// JSON signing bytes cover `metadata`, so the valid signature no
    /// longer matched and the whole record was ignored — a signed record
    /// that arrived with a warn-worthy RDF block could never be applied.
    ///
    /// The record is signed over the canonical-JSON payload on purpose:
    /// that is the payload whose bytes include `metadata`, and therefore
    /// the one the diagnostic mutation could break. (A record signed over
    /// the RDF payload — `{quads}\n{update_id}\n{origin_mesh_epoch}` —
    /// would verify either way, because that payload carries no
    /// metadata.)
    #[tokio::test]
    async fn bundle_only_signed_record_with_rdf_diagnostic_is_still_applied() {
        let id = Uuid::new_v4();
        let mut record = bundle_record(simple_rule(id, "signed bundle record with rdf diagnostic"));
        // An RDF block that trips a Phase-1 canonical-RDF violation.
        record.rdf = Some(RdfBlock {
            schema: "neurostrata-rdf-record/v1".to_string(),
            format: "n-quads".to_string(),
            canonical_hash: String::new(),
            quads: String::new(),
        });
        let key_dir = scratch("bundle-rdf-diagnostic-key");
        let key_path = key_dir.join("signing.key");
        let (pkey, did, pubkey) =
            crate::did::load_or_generate_keypair(&key_path).expect("the scratch signing key loads");
        // Sign over the canonical-JSON bytes of the record AS IT ARRIVED:
        // the signer DID and public key are part of those bytes, the
        // signature field is not (it is stripped before canonicalizing),
        // and the Phase-1 diagnostic is not in them yet.
        record.signer_did = Some(did.clone());
        record.public_key = Some(hex::encode(&pubkey));
        let payload = crate::did::canonical_signing_bytes(
            &serde_json::to_value(&record).expect("the record canonicalizes"),
        )
        .expect("the canonical signing bytes are computable");
        let sig = crate::did::sign(&payload, &pkey).expect("the record signs");
        record.signature = Some(crate::did::encode_base64(&sig));
        let signers = vec![super::super::manifest::SignerEntry {
            did: record
                .signer_did
                .clone()
                .expect("a signed record carries a signer DID"),
            public_key: record
                .public_key
                .clone()
                .expect("a signed record carries a public key"),
            role: "signer".to_string(),
        }];
        let (report, store) =
            run_bundle_route_with_signers("bundle-rdf-diagnostic", record, None, signers).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert_eq!(report.verified_signed, 1, "report: {report:?}");
        assert_eq!(report.signature_rejected, 0, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied from signed bundle");
        assert_eq!(
            payload
                .metadata
                .get("rdf_diagnostic_phase")
                .and_then(|v| v.as_str()),
            Some("1"),
            "the applied record still carries the Phase-1 diagnostic: {:?}",
            payload.metadata
        );
        let violations = payload
            .metadata
            .get("rdf_diagnostic_violations")
            .and_then(|v| v.as_array())
            .expect("the applied record still carries the diagnostic violations");
        assert!(
            !violations.is_empty(),
            "the diagnostic names at least one violation"
        );
    }

    /// A bundle record signed with a scratch key and then tampered
    /// with (a signed field mutated after signing) is rejected on the
    /// bundle route by signature verification: not applied,
    /// `signature_rejected` moves, and an ignored entry exists. This
    /// is the route-level guard against a verification stub that
    /// always answers Verified.
    #[tokio::test]
    async fn bundle_only_record_with_tampered_signature_is_rejected_and_not_applied() {
        let id = Uuid::new_v4();
        let mut record = bundle_record(simple_rule(id, "signed bundle record"));
        let key_dir = scratch("bundle-tampered-key");
        let key_path = key_dir.join("signing.key");
        crate::synaptic_mesh::worker::sign_record(&mut record, &key_path).expect("record signs");
        record.content = json!({"text": "tampered after signing"});
        let (report, store) = run_bundle_route("bundle-tampered", record, None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.unsigned_accepted, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 1, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("signature verification failed"),
            "the refusal names the failed verification: {joined}"
        );
        assert!(
            joined.contains(&id.to_string()),
            "the entry identifies the record: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// Behavioural equivalence: the same record is accepted or rejected
    /// identically — same applied count, same applied ids, same ignore
    /// reasons, same signature/disposition counters, same store
    /// outcome — on the `.json` route and the bundle-only route, across
    /// a valid record and four portable validation failures (including
    /// a secret on the METADATA surface, not just in content), plus
    /// the epoch gate under a migrated manifest. Every case also
    /// carries an ABSOLUTE expectation, so a change that breaks BOTH
    /// routes the same way still fails this test. A future change to
    /// one route's gate that diverges from the other fails it too.
    #[tokio::test]
    async fn bundle_and_json_routes_accept_and_reject_identically() {
        let valid_id = Uuid::new_v4();
        let schema_id = Uuid::new_v4();
        let secret_id = Uuid::new_v4();
        let metadata_secret_id = Uuid::new_v4();
        let structural_id = Uuid::new_v4();
        let pat = format!("ghp_{}", "B".repeat(36));
        let pat_meta = format!("ghp_{}", "C".repeat(36));
        let mut metadata_secret =
            simple_rule(metadata_secret_id, "clean content, secret in metadata");
        metadata_secret["metadata"]["note"] = json!(format!("use this token: {pat_meta}"));
        let cases: Vec<(&str, Value, usize)> = vec![
            ("valid", simple_rule(valid_id, "applies on both routes"), 1),
            (
                "unknown-schema",
                record_value(
                    "synaptic-mesh-record/v99",
                    schema_id,
                    "probe",
                    "rule",
                    "active",
                    None,
                    json!({"text": "from the future"}),
                ),
                0,
            ),
            (
                "secret",
                simple_rule(secret_id, &format!("use this token: {pat}")),
                0,
            ),
            ("secret-in-metadata", metadata_secret, 0),
            (
                "non-shareable-type",
                record_value(
                    "synaptic-mesh-record/v1",
                    structural_id,
                    "probe",
                    "structural",
                    "active",
                    None,
                    json!({"text": "project-local, never travels"}),
                ),
                0,
            ),
        ];
        for (name, value, expected_applied) in cases {
            let (json_report, json_store) =
                run_json_route(&format!("equiv-json-{name}"), &value, None).await;
            let (bundle_report, bundle_store) = run_bundle_route(
                &format!("equiv-bundle-{name}"),
                bundle_record(value.clone()),
                None,
            )
            .await;
            assert_eq!(
                json_report.applied, expected_applied,
                "{name}: json applied is not the absolute expectation: {json_report:?}"
            );
            assert_eq!(
                bundle_report.applied, expected_applied,
                "{name}: bundle applied is not the absolute expectation: {bundle_report:?}"
            );
            if expected_applied == 0 {
                assert!(
                    !json_report.ignored.is_empty(),
                    "{name}: json route must report the rejection: {json_report:?}"
                );
                assert!(
                    !bundle_report.ignored.is_empty(),
                    "{name}: bundle route must report the rejection: {bundle_report:?}"
                );
            }
            assert_eq!(
                json_report.applied, bundle_report.applied,
                "{name}: applied counts diverge (json {json_report:?} vs bundle {bundle_report:?})"
            );
            assert_eq!(
                json_report.applied_ids, bundle_report.applied_ids,
                "{name}: applied ids diverge"
            );
            assert_eq!(
                json_report.verified_signed, bundle_report.verified_signed,
                "{name}: verified_signed diverges"
            );
            assert_eq!(
                json_report.unsigned_accepted, bundle_report.unsigned_accepted,
                "{name}: unsigned_accepted diverges"
            );
            assert_eq!(
                json_report.signature_rejected, bundle_report.signature_rejected,
                "{name}: signature_rejected diverges"
            );
            assert_eq!(
                json_report.skipped_update_id, bundle_report.skipped_update_id,
                "{name}: skipped_update_id diverges"
            );
            assert_eq!(
                ignore_reasons(&json_report),
                ignore_reasons(&bundle_report),
                "{name}: ignore reasons diverge (json {:?} vs bundle {:?})",
                json_report.ignored,
                bundle_report.ignored
            );
            assert_eq!(
                row_count(&json_store, "probe").await,
                row_count(&bundle_store, "probe").await,
                "{name}: store outcomes diverge"
            );
        }

        // Epoch parity under a migrated manifest (epoch 2, records born
        // in epoch 1): both routes refuse with the epoch gate's reason.
        let epoch_id = Uuid::new_v4();
        let epoch_value = simple_rule(epoch_id, "born before the migration");
        let (json_report, _) = run_json_route("equiv-json-epoch", &epoch_value, Some(2)).await;
        let (bundle_report, _) =
            run_bundle_route("equiv-bundle-epoch", bundle_record(epoch_value), Some(2)).await;
        assert_eq!(json_report.applied, 0, "json report: {json_report:?}");
        assert_eq!(bundle_report.applied, 0, "bundle report: {bundle_report:?}");
        assert!(
            !json_report.ignored.is_empty(),
            "json route must report the epoch refusal: {json_report:?}"
        );
        assert!(
            !bundle_report.ignored.is_empty(),
            "bundle route must report the epoch refusal: {bundle_report:?}"
        );
        assert_eq!(
            json_report.verified_signed, bundle_report.verified_signed,
            "epoch: verified_signed diverges"
        );
        assert_eq!(
            json_report.unsigned_accepted, bundle_report.unsigned_accepted,
            "epoch: unsigned_accepted diverges"
        );
        assert_eq!(
            json_report.signature_rejected, bundle_report.signature_rejected,
            "epoch: signature_rejected diverges"
        );
        assert_eq!(
            json_report.skipped_update_id, bundle_report.skipped_update_id,
            "epoch: skipped_update_id diverges"
        );
        assert_eq!(
            ignore_reasons(&json_report),
            ignore_reasons(&bundle_report),
            "epoch refusals diverge (json {:?} vs bundle {:?})",
            json_report.ignored,
            bundle_report.ignored
        );
        let bundle_reasons = ignore_reasons(&bundle_report).join(" | ");
        assert!(
            bundle_reasons.contains("old-epoch"),
            "the bundle refusal is the epoch gate's: {bundle_reasons}"
        );
    }

    // ---- issue #45: RDF-only signatures are not full-record verified -----
    //
    // The RDF block is a lossy projection of the record. A signature over
    // the RDF payload (`{quads}\n{update_id}\n{epoch}` — the legacy
    // scheme production `sign_record` used for RDF-bearing records)
    // therefore covers none of the JSON fields outside the projection,
    // and must not earn the full-record `Verified` disposition, its
    // report counter, or its `signature_verified` provenance stamp. The
    // record still applies (legacy imports keep working); it is counted
    // and stamped as what it is. A signature over the canonical-JSON
    // payload covers the whole record and keeps the full outcome.

    /// Minimal stand-in quads for the fixtures below — NOT a faithful
    /// builder projection. The verifier consumes the stored quads
    /// string verbatim, and canonical reconstruction is the RDF
    /// diagnostic's concern (warn-only, attached after verification),
    /// so a single minimal quad suffices for disposition/stamp
    /// assertions.
    fn rdf_quads_for(id: Uuid, text: &str) -> String {
        format!("<urn:neurostrata:memory:{id}> <urn:neurostrata:content> \"{text}\" .\n")
    }

    /// A bundle-carried record with an RDF block attached.
    fn rdf_bearing_record(id: Uuid, text: &str) -> MeshRecord {
        let mut record = bundle_record(simple_rule(id, text));
        record.rdf = Some(RdfBlock {
            schema: "neurostrata-rdf-record/v1".to_string(),
            format: "n-quads".to_string(),
            canonical_hash: String::new(),
            quads: rdf_quads_for(id, text),
        });
        record
    }

    /// Sign a record the LEGACY way: directly over the RDF payload with
    /// the keypair at `key_path`. Production `sign_record` no longer
    /// produces these signatures — that is precisely the legacy
    /// simulation: a record signed by an older peer.
    fn sign_record_legacy_rdf(record: &mut MeshRecord, key_path: &Path) {
        let (pkey, did, pubkey) =
            crate::did::load_or_generate_keypair(key_path).expect("the scratch signing key loads");
        record.signer_did = Some(did);
        record.public_key = Some(hex::encode(&pubkey));
        let rdf_block = record
            .rdf
            .as_ref()
            .expect("legacy RDF signing needs an RDF block");
        let payload = crate::did::signing_payload(
            &rdf_block.quads,
            &record.update_id.to_string(),
            record.origin_mesh_epoch,
        );
        let sig = crate::did::sign(&payload, &pkey).expect("the record signs");
        record.signature = Some(crate::did::encode_base64(&sig));
    }

    /// The manifest allowlist entry for a signed record's key, as the
    /// #43 fixtures build it.
    fn signer_entry_for(record: &MeshRecord) -> super::super::manifest::SignerEntry {
        super::super::manifest::SignerEntry {
            did: record
                .signer_did
                .clone()
                .expect("a signed record carries a signer DID"),
            public_key: record
                .public_key
                .clone()
                .expect("a signed record carries a public key"),
            role: "signer".to_string(),
        }
    }

    /// Replay one record as a committed `.json` candidate with the
    /// mesh manifest's signer allowlist populated — the `.json`-route
    /// analogue of `run_bundle_route_with_signers`, mirroring
    /// `run_json_route`'s harness (a committed record file whose stem
    /// is the record id).
    async fn run_json_route_with_signers(
        tag: &str,
        record: MeshRecord,
        signers: Vec<super::super::manifest::SignerEntry>,
    ) -> (ApplyReport, std::sync::Arc<dyn VectorStore>) {
        let (_base, mesh) = mesh_with_commit(tag);
        let mesh = if signers.is_empty() {
            mesh
        } else {
            reopen_with_signers(&mesh, signers)
        };
        let id = record.id.to_string();
        let value = serde_json::to_value(&record).expect("the record serializes");
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id,
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store(tag).await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("json-route replay completes");
        (report, store)
    }

    /// The stored `metadata.mesh` provenance object of an applied row.
    async fn stored_mesh_provenance(
        store: &std::sync::Arc<dyn VectorStore>,
        id: &Uuid,
    ) -> Value {
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("the row was applied");
        payload
            .metadata
            .get("mesh")
            .cloned()
            .expect("the applied row carries mesh provenance")
    }

    /// T1: a legacy RDF-signed record whose JSON surface is tampered
    /// AFTER signing — a metadata key outside the RDF projection is
    /// added — still applies (the RDF branch verifies), but it is
    /// counted as RDF-only verified, NOT as fully verified, and its
    /// stored provenance carries `rdf_signature_verified` without the
    /// `signature_verified` key. Before the fix the two payload branches
    /// were OR-ed into one `Verified`, so this record counted as
    /// `verified_signed` and was stamped `signature_verified: true`
    /// even though the added key was never signed. Signer attribution
    /// is likewise branch-aware (54-A): the signer is recorded under
    /// the RDF-specific `rdf_verified_signer_did` key only, never
    /// under the owner-grade `verified_signer_did` key that rebac
    /// derives the resource owner from.
    #[tokio::test]
    async fn rdf_only_signature_with_json_tampered_outside_projection_is_not_fully_verified() {
        let id = Uuid::new_v4();
        let mut record = rdf_bearing_record(id, "legacy signed rule");
        let key_dir = scratch("rdf-only-tampered-key");
        let key_path = key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut record, &key_path);
        let signer_did = record.signer_did.clone().expect("signed record has a DID");
        let signers = vec![signer_entry_for(&record)];
        // Tamper OUTSIDE the projection: a metadata key the RDF builder
        // never projects. The quads, update_id and epoch are untouched,
        // so the RDF payload still verifies; the canonical-JSON payload
        // covers `metadata`, so the JSON branch fails.
        if let Some(obj) = record.metadata.as_object_mut() {
            obj.insert(
                "unprojected_note".to_string(),
                json!("added after signing"),
            );
        }
        let (report, store) =
            run_bundle_route_with_signers("rdf-only-tampered", record, None, signers).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert_eq!(report.rdf_only_verified, 1, "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 0, "report: {report:?}");
        assert_eq!(report.unauthorized_signer, 0, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        let mesh_prov = stored_mesh_provenance(&store, &id).await;
        assert_eq!(
            mesh_prov.get("rdf_signature_verified"),
            Some(&json!(true)),
            "the stamp names the RDF-only verification: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("signature_verified").is_none(),
            "no full-record stamp for an RDF-only signature: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("verified_signer_did").is_none(),
            "no owner-grade signer stamp for an RDF-only signature: {mesh_prov}"
        );
        assert_eq!(
            mesh_prov.get("rdf_verified_signer_did"),
            Some(&json!(signer_did)),
            "the signer is recorded under the RDF-specific key only: {mesh_prov}"
        );
    }

    /// T2 (control): a legacy RDF-signed record whose content is altered
    /// after signing — with its RDF projection altered to match, as a
    /// tampered record on the wire would carry — fails BOTH payload
    /// branches and is rejected: not applied, `signature_rejected`
    /// moves. (Altering the JSON content alone, leaving the quads as
    /// signed, is T1's case, not this one: the RDF payload is computed
    /// from the stored quads, so a projection-covered change must touch
    /// the quads to reach it.)
    #[tokio::test]
    async fn rdf_signature_with_projection_covered_field_tampered_is_rejected() {
        let id = Uuid::new_v4();
        let mut record = rdf_bearing_record(id, "signed content");
        let key_dir = scratch("rdf-tampered-key");
        let key_path = key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut record, &key_path);
        record.content = json!({"text": "tampered content"});
        if let Some(rdf_block) = record.rdf.as_mut() {
            rdf_block.quads = rdf_quads_for(id, "tampered content");
        }
        let (report, store) = run_bundle_route("rdf-tampered", record, None).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.rdf_only_verified, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 1, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("signature verification failed"),
            "the refusal names the failed verification: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// T3: an RDF-bearing record signed by production `sign_record`
    /// verifies over the canonical-JSON payload — the whole record,
    /// RDF block included — so it counts as fully verified and earns
    /// the full stamp, with no RDF-only marker.
    #[tokio::test]
    async fn rdf_bearing_record_with_full_record_signature_is_fully_verified() {
        let id = Uuid::new_v4();
        let mut record = rdf_bearing_record(id, "full record signed rule");
        let key_dir = scratch("rdf-full-json-key");
        let key_path = key_dir.join("signing.key");
        crate::synaptic_mesh::worker::sign_record(&mut record, &key_path).expect("record signs");
        let signer_did = record.signer_did.clone().expect("signed record has a DID");
        let signers = vec![signer_entry_for(&record)];
        let (report, store) =
            run_bundle_route_with_signers("rdf-full-json", record, None, signers).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert_eq!(report.verified_signed, 1, "report: {report:?}");
        assert_eq!(report.rdf_only_verified, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 0, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        let mesh_prov = stored_mesh_provenance(&store, &id).await;
        assert_eq!(
            mesh_prov.get("signature_verified"),
            Some(&json!(true)),
            "the full-record stamp: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("rdf_signature_verified").is_none(),
            "no RDF-only marker on a full-record signature: {mesh_prov}"
        );
        assert_eq!(
            mesh_prov.get("verified_signer_did"),
            Some(&json!(signer_did)),
            "the signer is recorded: {mesh_prov}"
        );
    }

    /// T4: an untampered legacy RDF-signed record still applies — the
    /// legacy scheme keeps working — but it is counted and stamped as
    /// RDF-only verified, never as fully verified.
    #[tokio::test]
    async fn legacy_rdf_only_signature_applies_as_rdf_verified_only() {
        let id = Uuid::new_v4();
        let mut record = rdf_bearing_record(id, "untampered legacy rule");
        let key_dir = scratch("rdf-only-untampered-key");
        let key_path = key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut record, &key_path);
        let signer_did = record.signer_did.clone().expect("signed record has a DID");
        let signers = vec![signer_entry_for(&record)];
        let (report, store) =
            run_bundle_route_with_signers("rdf-only-untampered", record, None, signers).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert_eq!(report.rdf_only_verified, 1, "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 0, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        let mesh_prov = stored_mesh_provenance(&store, &id).await;
        assert_eq!(
            mesh_prov.get("rdf_signature_verified"),
            Some(&json!(true)),
            "the stamp names the RDF-only verification: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("signature_verified").is_none(),
            "no full-record stamp for an RDF-only signature: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("verified_signer_did").is_none(),
            "no owner-grade signer stamp for an RDF-only signature: {mesh_prov}"
        );
        assert_eq!(
            mesh_prov.get("rdf_verified_signer_did"),
            Some(&json!(signer_did)),
            "the signer is recorded under the RDF-specific key only: {mesh_prov}"
        );
    }

    /// T5: the same legacy RDF-only outcome on the `.json` route. Every
    /// other #45 test rides the bundle route, so the `.json` call site
    /// in `apply_changed` could drop or mis-thread its disposition —
    /// counting the record without passing the disposition to
    /// `apply_one`, or passing full-record `Verified` instead — and no
    /// test would fail. Here the record arrives as a committed `.json`
    /// file: it applies, counts as RDF-only verified (never as fully
    /// verified), and its stored provenance carries
    /// `rdf_signature_verified` with NO `signature_verified` key and
    /// NO owner-grade `verified_signer_did` key — the signer is
    /// recorded under `rdf_verified_signer_did` only (54-A).
    #[tokio::test]
    async fn legacy_rdf_only_signature_applies_as_rdf_verified_only_on_json_route() {
        let id = Uuid::new_v4();
        let mut record = rdf_bearing_record(id, "untampered legacy rule via json");
        let key_dir = scratch("rdf-only-json-key");
        let key_path = key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut record, &key_path);
        let signer_did = record.signer_did.clone().expect("signed record has a DID");
        let signers = vec![signer_entry_for(&record)];
        let (report, store) =
            run_json_route_with_signers("rdf-only-json", record, signers).await;
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(
            report.applied_ids.contains(&id.to_string()),
            "report: {report:?}"
        );
        assert_eq!(report.rdf_only_verified, 1, "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 0, "report: {report:?}");
        assert_eq!(report.unauthorized_signer, 0, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");
        let mesh_prov = stored_mesh_provenance(&store, &id).await;
        assert_eq!(
            mesh_prov.get("rdf_signature_verified"),
            Some(&json!(true)),
            "the stamp names the RDF-only verification: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("signature_verified").is_none(),
            "no full-record stamp for an RDF-only signature: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("verified_signer_did").is_none(),
            "no owner-grade signer stamp for an RDF-only signature: {mesh_prov}"
        );
        assert_eq!(
            mesh_prov.get("rdf_verified_signer_did"),
            Some(&json!(signer_did)),
            "the signer is recorded under the RDF-specific key only: {mesh_prov}"
        );
    }

    /// T6: a legacy RDF-only-signed record whose signer is NOT in a
    /// non-empty manifest allowlist is refused as unauthorized — the
    /// allowlist holds a DIFFERENT signer, built the same way as the
    /// applying fixtures' allowlists. The record is not applied and
    /// neither verification counter moves: the allowlist check runs
    /// before the verified branches are split, so an RDF-only
    /// verification cannot bypass it (a `VerifiedRdfOnly` returned
    /// ahead of the allowlist check would apply this record and count
    /// it as RDF-only verified instead).
    #[tokio::test]
    async fn rdf_only_signature_from_unauthorized_signer_is_rejected() {
        let id = Uuid::new_v4();
        let mut record = rdf_bearing_record(id, "legacy rule from an unlisted signer");
        let key_dir = scratch("rdf-only-unauth-key");
        let key_path = key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut record, &key_path);
        // The allowlist names a different signer: a second legacy-signed
        // record signed with a second key, as the applying fixtures
        // build their entries — its entry is well-formed, it is simply
        // not this record's signer.
        let mut other = rdf_bearing_record(Uuid::new_v4(), "the listed signer");
        let other_key_dir = scratch("rdf-only-unauth-other-key");
        let other_key_path = other_key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut other, &other_key_path);
        let signers = vec![signer_entry_for(&other)];
        let (report, store) =
            run_bundle_route_with_signers("rdf-only-unauth", record, None, signers).await;
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert!(report.applied_ids.is_empty(), "report: {report:?}");
        assert_eq!(report.unauthorized_signer, 1, "report: {report:?}");
        assert_eq!(report.rdf_only_verified, 0, "report: {report:?}");
        assert_eq!(report.verified_signed, 0, "report: {report:?}");
        assert_eq!(report.signature_rejected, 0, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("not in manifest.signers allowlist"),
            "the refusal names the allowlist: {joined}"
        );
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "nothing reached the store"
        );
    }

    /// 54-A: an RDF-only verification must NOT stamp the owner-grade
    /// `verified_signer_did` key — `rebac::extract_resource_owner`
    /// derives the resource owner from `metadata.mesh.verified_signer_did`
    /// and the owner is always allowed, so that key confers owner-grade
    /// signer attribution. An RDF signature attests only to the lossy
    /// RDF payload, not the full record, so its signer provenance is
    /// carried under the RDF-specific `rdf_verified_signer_did` key
    /// instead. A full-record verification (control) keeps stamping
    /// `verified_signer_did` exactly as before.
    #[tokio::test]
    async fn rdf_only_verification_does_not_stamp_owner_grade_signer_did() {
        // RDF-only branch: legacy RDF-signed, untampered.
        let rdf_id = Uuid::new_v4();
        let mut rdf_record = rdf_bearing_record(rdf_id, "rdf only signer attribution rule");
        let rdf_key_dir = scratch("rdf-only-attribution-key");
        let rdf_key_path = rdf_key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut rdf_record, &rdf_key_path);
        let rdf_signer_did = rdf_record
            .signer_did
            .clone()
            .expect("signed record has a DID");
        let rdf_signers = vec![signer_entry_for(&rdf_record)];
        let (rdf_report, rdf_store) = run_bundle_route_with_signers(
            "rdf-only-attribution",
            rdf_record,
            None,
            rdf_signers,
        )
        .await;
        assert_eq!(rdf_report.applied, 1, "report: {rdf_report:?}");
        assert_eq!(rdf_report.rdf_only_verified, 1, "report: {rdf_report:?}");
        assert_eq!(rdf_report.verified_signed, 0, "report: {rdf_report:?}");
        let rdf_prov = stored_mesh_provenance(&rdf_store, &rdf_id).await;
        assert!(
            rdf_prov.get("verified_signer_did").is_none(),
            "RDF-only verification must not stamp owner-grade verified_signer_did: {rdf_prov}"
        );
        assert_eq!(
            rdf_prov.get("rdf_verified_signer_did"),
            Some(&json!(rdf_signer_did)),
            "the RDF-branch signer is recorded under the RDF-specific key only: {rdf_prov}"
        );
        assert_eq!(
            rdf_prov.get("rdf_signature_verified"),
            Some(&json!(true)),
            "the RDF-branch verification stamp is retained: {rdf_prov}"
        );

        // Control: full-record verification stamps verified_signer_did.
        let full_id = Uuid::new_v4();
        let mut full_record = rdf_bearing_record(full_id, "full record signer attribution rule");
        let full_key_dir = scratch("full-attribution-key");
        let full_key_path = full_key_dir.join("signing.key");
        crate::synaptic_mesh::worker::sign_record(&mut full_record, &full_key_path)
            .expect("record signs");
        let full_signer_did = full_record
            .signer_did
            .clone()
            .expect("signed record has a DID");
        let full_signers = vec![signer_entry_for(&full_record)];
        let (full_report, full_store) = run_bundle_route_with_signers(
            "full-attribution",
            full_record,
            None,
            full_signers,
        )
        .await;
        assert_eq!(full_report.applied, 1, "report: {full_report:?}");
        assert_eq!(full_report.verified_signed, 1, "report: {full_report:?}");
        assert_eq!(full_report.rdf_only_verified, 0, "report: {full_report:?}");
        let full_prov = stored_mesh_provenance(&full_store, &full_id).await;
        assert_eq!(
            full_prov.get("verified_signer_did"),
            Some(&json!(full_signer_did)),
            "full-record verification keeps the owner-grade signer stamp: {full_prov}"
        );
        assert!(
            full_prov.get("rdf_verified_signer_did").is_none(),
            "no RDF-specific signer key on a full-record signature: {full_prov}"
        );
    }

    // ---- finding 54-B: combination rows earn no verified stamps --------
    //
    // `MergeDecision::ApplyCombination` materialises a row that is NOT
    // the signed candidate: the stored (content, state) pair was never
    // the subject of the candidate's signature verification, in either
    // sub-shape. The candidate's `signature_disposition` therefore must
    // not stamp the combination row verified — for either signature
    // branch. The gate counters still record that the CANDIDATE
    // verified; only the stored row's provenance stamps are withheld.

    /// 54-B, local-Active sub-shape: pass 1 applies an Active record
    /// (content C_local); pass 2's candidate is Archived with DIFFERENT
    /// content C_cand, signed with the production full-record signer
    /// (canonical-JSON payload, disposition `Verified`). The
    /// combination stores the LOCAL row's content forced to Archived —
    /// text present in neither of the candidate's signed payloads — so
    /// the stored provenance must carry NO `signature_verified` and NO
    /// `verified_signer_did`, even though the gate counted the
    /// candidate as verified_signed.
    #[tokio::test]
    async fn archived_combination_from_local_active_row_earns_no_verified_stamp() {
        let id = Uuid::new_v4();
        // Build and sign the pass-2 candidate first, so its signer can
        // populate the manifest allowlist before either pass runs.
        let mut candidate = rdf_bearing_record(id, "candidate archived edit");
        candidate.lifecycle.state = LifecycleState::Archived;
        candidate.lifecycle.archived_at = Some(chrono::Utc::now());
        let key_dir = scratch("combo-local-active-key");
        let key_path = key_dir.join("signing.key");
        crate::synaptic_mesh::worker::sign_record(&mut candidate, &key_path)
            .expect("record signs");
        let signers = vec![signer_entry_for(&candidate)];

        let (_base, mesh) = mesh_with_commit("combo-local-active");
        let mesh = reopen_with_signers(&mesh, signers);
        let store = fresh_store("combo-local-active").await;
        let served = ["probe".to_string()];

        // Pass 1: the local row lands Active with the local content.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "local active content")).unwrap(),
        );
        let first = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("pass 1 applies");
        assert_eq!(first.applied, 1, "pass 1 report: {first:?}");
        let (_, local) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("the local row was applied");
        assert_eq!(local.content, "local active content");

        // Pass 2: the signed Archived candidate with different content
        // replays against the SAME store and lands as a combination.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&serde_json::to_value(&candidate).unwrap()).unwrap(),
        );
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("pass 2 applies");
        assert_eq!(second.applied, 1, "pass 2 report: {second:?}");
        assert!(
            !second.combinations.is_empty(),
            "the combination is reported: {second:?}"
        );
        assert_eq!(
            second.verified_signed, 1,
            "the candidate itself verified at the gate: {second:?}"
        );
        assert!(second.ignored.is_empty(), "pass 2 report: {second:?}");

        // The stored row is the combination: LOCAL content, Archived.
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("the combination row was applied");
        assert_eq!(payload.content, "local active content");
        let mesh_prov = stored_mesh_provenance(&store, &id).await;
        assert_eq!(
            mesh_prov.get("lifecycle_state"),
            Some(&json!("archived")),
            "the combination row is stored archived: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("signature_verified").is_none(),
            "no full-record stamp on a combination row: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("verified_signer_did").is_none(),
            "no owner-grade signer stamp on a combination row: {mesh_prov}"
        );
    }

    /// 54-B, local-Archived sub-shape: pass 1 applies an Archived
    /// record; pass 2's candidate is Active with edited content, signed
    /// with the legacy RDF signer (disposition `VerifiedRdfOnly`). The
    /// combination stores the CANDIDATE's content but forced to
    /// Archived, while the candidate signed Active — the signed
    /// (content, state) pair was never stored — so the stored
    /// provenance must carry NO `rdf_signature_verified` and NO
    /// `rdf_verified_signer_did`, even though the gate counted the
    /// candidate as rdf_only_verified.
    #[tokio::test]
    async fn archived_combination_from_local_archived_row_earns_no_rdf_verified_stamp() {
        let id = Uuid::new_v4();
        // Build and sign the pass-2 candidate first, so its signer can
        // populate the manifest allowlist before either pass runs.
        let mut candidate = rdf_bearing_record(id, "candidate active edit");
        let key_dir = scratch("combo-local-archived-key");
        let key_path = key_dir.join("signing.key");
        sign_record_legacy_rdf(&mut candidate, &key_path);
        let signers = vec![signer_entry_for(&candidate)];

        let (_base, mesh) = mesh_with_commit("combo-local-archived");
        let mesh = reopen_with_signers(&mesh, signers);
        let store = fresh_store("combo-local-archived").await;
        let served = ["probe".to_string()];

        // Pass 1: the local row lands Archived.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&record_with_state(
                id,
                "archived",
                None,
                "local archived content",
            ))
            .unwrap(),
        );
        let first = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("pass 1 applies");
        assert_eq!(first.applied, 1, "pass 1 report: {first:?}");
        let (_, local) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("the local row was applied");
        assert_eq!(local.content, "local archived content");

        // Pass 2: the legacy RDF-signed Active candidate with edited
        // content replays against the SAME store and lands as a
        // combination.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&serde_json::to_value(&candidate).unwrap()).unwrap(),
        );
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .expect("pass 2 applies");
        assert_eq!(second.applied, 1, "pass 2 report: {second:?}");
        assert!(
            !second.combinations.is_empty(),
            "the combination is reported: {second:?}"
        );
        assert_eq!(
            second.rdf_only_verified, 1,
            "the candidate itself verified RDF-only at the gate: {second:?}"
        );
        assert_eq!(second.verified_signed, 0, "pass 2 report: {second:?}");
        assert!(second.ignored.is_empty(), "pass 2 report: {second:?}");

        // The stored row is the combination: the CANDIDATE's content,
        // but Archived — not the Active state the candidate signed.
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("the combination row was applied");
        assert_eq!(payload.content, "candidate active edit");
        let mesh_prov = stored_mesh_provenance(&store, &id).await;
        assert_eq!(
            mesh_prov.get("lifecycle_state"),
            Some(&json!("archived")),
            "the combination row is stored archived: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("rdf_signature_verified").is_none(),
            "no RDF-only stamp on a combination row: {mesh_prov}"
        );
        assert!(
            mesh_prov.get("rdf_verified_signer_did").is_none(),
            "no RDF-branch signer stamp on a combination row: {mesh_prov}"
        );
    }

    /// T7: `format_apply_report` renders the RDF-only segment only
    /// when the counter is non-zero, and renders byte-identically to
    /// the pre-change format when it is zero. The zero-case expectation
    /// is the full string the base implementation (0564d108) produces
    /// for this same minimal report: with every signature counter at
    /// zero it appends no signed segment at all, and head, ids,
    /// conflicts and combinations are all empty here, so nothing else
    /// is appended either.
    #[test]
    fn format_apply_report_rdf_only_segment_is_additive() {
        fn minimal_report(rdf_only_verified: usize) -> ApplyReport {
            ApplyReport {
                applied: 1,
                skipped_update_id: 0,
                ignored: Vec::new(),
                full_tree: false,
                head: String::new(),
                applied_ids: Vec::new(),
                conflicted: 0,
                conflicted_ids: Vec::new(),
                combinations: Vec::new(),
                verified_signed: 0,
                rdf_only_verified,
                unsigned_accepted: 0,
                signature_rejected: 0,
                unauthorized_signer: 0,
            }
        }
        let with_rdf = format_apply_report("format-test", &minimal_report(1));
        assert!(
            with_rdf.contains("rdf-only-signed 1"),
            "the RDF-only segment is rendered: {with_rdf}"
        );
        assert_eq!(
            with_rdf,
            "pulled mesh format-test: applied 1, skipped 0, ignored 0, \
             signed 0 / signature-rejected 0 / unauthorized 0, rdf-only-signed 1"
        );
        let without_rdf = format_apply_report("format-test", &minimal_report(0));
        assert_eq!(
            without_rdf,
            "pulled mesh format-test: applied 1, skipped 0, ignored 0"
        );
    }

    // ---- contract tests ----------------------------------------------------

    /// Happy path: one committed record file replays into an ordinary store
    /// row — retrievable by id, carrying mesh provenance, and visible to a
    /// search-shaped read without any special casing.
    #[tokio::test]
    async fn pull_replays_a_committed_record_into_the_store_with_provenance() {
        let (_base, mesh) = mesh_with_commit("happy");
        let id = Uuid::new_v4();
        let value = simple_rule(id, "always use podman");
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("happy").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("full-tree replay applies");

        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(report.applied_ids.contains(&id.to_string()));
        assert!(report.ignored.is_empty(), "report: {report:?}");
        assert!(report.full_tree, "no cursor yet: first pass is full-tree");

        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(payload.content, "always use podman");
        assert_eq!(payload.memory_type, "rule");
        let mesh_meta = payload.metadata.get("mesh").expect("mesh provenance");
        assert_eq!(mesh_meta["mesh_id"], json!(mesh_id_str(&mesh)));
        let update_id = mesh_meta["update_id"]
            .as_str()
            .expect("update_id recorded")
            .to_string();
        let record: MeshRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            update_id,
            record.update_id.to_string(),
            "the row's provenance carries the committed record's update_id"
        );

        // Ordinary row: a search-shaped read finds it without special casing.
        let query = emb().embed("always use podman").await.unwrap();
        let hits = store.search("probe", query, 5, false, false).await.unwrap();
        assert!(!hits.is_empty(), "search must see applied mesh rows");
        assert_eq!(hits[0].id, id.to_string());

        // Cursor advanced to HEAD.
        let head = current_head(mesh.root()).unwrap().unwrap();
        assert_eq!(load_cursor(mesh.root(), &mesh_id_str(&mesh)).unwrap(), head);
    }

    /// Idempotence: re-running against an already-accepted head applies
    /// nothing (the diff is empty), and a crash-before-cursor-advance replay
    /// applies nothing AND duplicates nothing (the update_id skip).
    #[tokio::test]
    async fn reapply_advances_zero_and_crash_replay_never_duplicates() {
        let (_base, mesh) = mesh_with_commit("reapply");
        let id = Uuid::new_v4();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "deterministic dedup")).unwrap(),
        );
        let store = fresh_store("reapply").await;
        let served = ["probe".to_string()];

        let first = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(first.applied, 1);
        assert_eq!(row_count(&store, "probe").await, 1);

        // Cursor advanced: replay of the same head is a 0-applied pass.
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(second.applied, 0, "no changed files: nothing applies");

        // Crash before the cursor advance: the same files diff again, but
        // the accepted rows are skipped by update_id — no duplicates.
        std::fs::remove_file(cursor_file(mesh.root())).unwrap();
        let third = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert!(third.full_tree, "lost cursor → full-tree reconciliation");
        assert_eq!(third.applied, 0, "already-accepted update_id skips");
        assert_eq!(
            third.skipped_update_id, 1,
            "the accepted record is skipped, not re-applied"
        );
        assert_eq!(row_count(&store, "probe").await, 1, "no duplicate row");
    }

    /// Hostile checkout contents — path traversal, symlinks, executable
    /// payload bits, garbage JSON — are all ignored, and the store stays
    /// untouched.
    #[tokio::test]
    async fn traversal_symlink_executable_and_garbage_are_ignored_and_store_untouched() {
        // Traversal never becomes a candidate: the classifier refuses any
        // path that leaves the mesh root.
        let served = ["probe".to_string()];
        let err = classify_path("../escape/memories/a.json", &served)
            .expect_err("traversal must be refused");
        assert!(err.contains("traversal"), "got: {err}");
        let err = classify_path("probe/memories/../../escape.json", &served)
            .expect_err("traversal must be refused");
        assert!(err.contains("traversal"), "got: {err}");

        let (_base, mesh) = mesh_with_commit("hostile");
        let bad_id = Uuid::new_v4();
        commit_record_json(&mesh, "probe", "memories", &bad_id.to_string(), "{not json");

        // The symlink and executable-bit setups are Unix-only: the link is
        // created through the Unix `symlink` API, and the exec-bit rejection
        // is itself a Unix-mode check (`has_exec_bit` is always false on
        // non-Unix, where the plain record file it chmods would be
        // legitimate data and would apply). The traversal checks above and the
        // garbage-JSON assertions below run on every platform.
        #[cfg(unix)]
        {
            let secret = _base.join("outside.json");
            std::fs::write(&secret, r#"{"id":"stolen"}"#).unwrap();

            let evil = Uuid::new_v4();
            let d = mesh.root().join("probe").join("memories");
            std::fs::create_dir_all(&d).unwrap();
            symlink(&secret, d.join(format!("{evil}.json"))).unwrap();

            let exec_id = Uuid::new_v4();
            commit_record_json(
                &mesh,
                "probe",
                "memories",
                &exec_id.to_string(),
                &serde_json::to_string(&simple_rule(exec_id, "executable payload")).unwrap(),
            );
            let exec_path = mesh
                .root()
                .join("probe")
                .join("memories")
                .join(format!("{exec_id}.json"));
            let mut mode = std::fs::metadata(&exec_path).unwrap().permissions();
            mode.set_mode(0o755);
            std::fs::set_permissions(&exec_path, mode).unwrap();
        }
        mesh.commit_all("peer bits", "node-peer-1").unwrap();

        let store = fresh_store("hostile").await;
        store.init("probe").await.unwrap();
        let report = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0, "hostile files never apply: {report:?}");
        let joined = report.ignored.join(" | ");
        // The symlink and exec-bit entries only exist when the Unix-only
        // setups above ran.
        #[cfg(unix)]
        {
            assert!(
                report.ignored.iter().any(|i| i.contains("symlink")),
                "symlink ignored+named: {joined}"
            );
            assert!(
                report.ignored.iter().any(|i| i.contains("executable")),
                "exec-bit ignored+named: {joined}"
            );
        }
        assert!(
            report.ignored.iter().any(|i| i.contains("unparseable")
                || i.contains("parsererror")
                || i.contains("expected")),
            "garbage JSON ignored+named: {joined}"
        );
        assert_eq!(row_count(&store, "probe").await, 0, "store untouched");
    }

    /// A record whose schema_version this daemon does not know is passed
    /// over, NAMED in the report, and never crashes the pass.
    #[tokio::test]
    async fn unknown_schema_version_is_ignored_and_named() {
        let (_base, mesh) = mesh_with_commit("schema");
        let id = Uuid::new_v4();
        let future = record_value(
            "synaptic-mesh-record/v99",
            id,
            "probe",
            "rule",
            "active",
            None,
            json!({"text": "from the future"}),
        );
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&future).unwrap(),
        );
        let store = fresh_store("schema").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(
            row_count(&store, "probe").await,
            0,
            "unknown-schema rows must not land"
        );
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("synaptic-mesh-record/v99"),
            "the unknown version is named: {joined}"
        );
        assert!(
            joined.contains("synaptic-mesh-record/v1"),
            "the supported version is named: {joined}"
        );
    }

    // ---- slice F3: old-epoch quarantine + oversized-JSON cap ---------------
    //
    // Spec §"Deletion & erasure" / §"Import validation": the manifest epoch is
    // the purge mechanism's only teeth. A record whose `origin_mesh_epoch`
    // predates the manifest's current epoch is from a pre-purge peer and is
    // quarantined (a named ignore), never replayed. Equal epochs apply as
    // today; a future epoch is refused explicitly (a validator rejects what it
    // does not understand). Oversized records are capped before parse.

    /// An old-epoch record (origin_mesh_epoch 1, manifest epoch 2 after a
    /// migration) is refused and NAMED in the ignore report — quarantined,
    /// not replayed into the new epoch.
    #[tokio::test]
    async fn epoch_older_refused_named() {
        let (_base, mesh) = mesh_with_commit("epoch-older");
        let mesh = reopen_with_epoch(&mesh, 2);
        let id = Uuid::new_v4();
        // A record born in the pre-migration epoch (origin_mesh_epoch 1).
        let value = simple_rule(id, "published before the purge");
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("epoch-older").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(
            report.applied, 0,
            "old-epoch record never applies: {report:?}"
        );
        assert_eq!(row_count(&store, "probe").await, 0, "store untouched");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("old-epoch"),
            "the refusal names the old epoch: {joined}"
        );
        assert!(
            joined.contains("origin_mesh_epoch 1") && joined.contains("manifest epoch 2"),
            "both epochs are named: {joined}"
        );
    }

    /// An equal-epoch record applies as it always has — byte-identical
    /// behavior to the pre-gate flow.
    #[tokio::test]
    async fn epoch_equal_applies() {
        let (_base, mesh) = mesh_with_commit("epoch-equal");
        let id = Uuid::new_v4();
        let value = simple_rule(id, "same epoch as the manifest");
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("epoch-equal").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 1, "equal epoch applies: {report:?}");
        assert!(report.ignored.is_empty(), "nothing ignored: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(payload.content, "same epoch as the manifest");
    }

    /// A future-epoch record (origin_mesh_epoch 2, manifest epoch 1) is
    /// refused with an explicit "future epoch" reason — never silently
    /// accepted.
    #[tokio::test]
    async fn epoch_future_refused() {
        let (_base, mesh) = mesh_with_commit("epoch-future");
        let id = Uuid::new_v4();
        let mut value = simple_rule(id, "from a future epoch");
        value["origin_mesh_epoch"] = json!(2);
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("epoch-future").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(
            report.applied, 0,
            "future-epoch record never applies: {report:?}"
        );
        assert_eq!(row_count(&store, "probe").await, 0, "store untouched");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("future-epoch"),
            "the refusal names the future epoch: {joined}"
        );
        assert!(
            joined.contains("origin_mesh_epoch 2") && joined.contains("manifest epoch 1"),
            "both epochs are named: {joined}"
        );
    }

    /// A record larger than `MAX_RECORD_BYTES` is ignored BEFORE parse, with
    /// a reason naming the cap — an oversized JSON blob never applies.
    #[tokio::test]
    async fn oversized_record_ignored_named() {
        let (_base, mesh) = mesh_with_commit("oversized");
        let id = Uuid::new_v4();
        let big_text = "x".repeat(MAX_RECORD_BYTES + 1);
        let value = simple_rule(id, &big_text);
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("oversized").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(
            report.applied, 0,
            "oversized record never applies: {report:?}"
        );
        assert_eq!(row_count(&store, "probe").await, 0, "store untouched");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("per-record cap"),
            "the reason names the cap: {joined}"
        );
        assert!(
            joined.contains(&MAX_RECORD_BYTES.to_string()),
            "the cap value is named: {joined}"
        );
    }

    /// A record belonging to a namespace this project does not serve is
    /// ignored with a reason — other teams' files are not applied here.
    #[tokio::test]
    async fn foreign_namespace_file_is_ignored() {
        let (_base, mesh) = mesh_with_commit("foreign");
        let id = Uuid::new_v4();
        let other = simple_rule(id, "belongs to another team");
        commit_record_json(
            &mesh,
            "elsewhere",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&other).unwrap(),
        );
        let store = fresh_store("foreign").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0);
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("foreign-namespace"),
            "foreign origin named: {joined}"
        );
        assert!(
            joined.contains("elsewhere"),
            "the other namespace named: {joined}"
        );
        store.init("probe").await.unwrap();
        assert_eq!(row_count(&store, "probe").await, 0);
        store.init("elsewhere").await.unwrap();
        assert_eq!(row_count(&store, "elsewhere").await, 0);
    }

    /// A row that already exists WITHOUT mesh provenance is a private local
    /// memory: never overwritten, never deleted; the same-id collision is
    /// reported into the ignore list.
    #[tokio::test]
    async fn private_row_with_same_id_is_never_stomped() {
        let (_base, mesh) = mesh_with_commit("private");
        let id = Uuid::new_v4();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "the mesh's version")).unwrap(),
        );
        let store = fresh_store("private").await;
        store.init("probe").await.unwrap();
        store
            .upsert(
                "probe",
                &id.to_string(),
                emb().embed("my private note").await.unwrap(),
                MemoryPayload {
                    content: "my private note".to_string(),
                    user_id: "me".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"source": "hand-written"}),
                },
            )
            .await
            .unwrap();

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0, "the private row was never stomped");
        assert!(
            report.ignored.iter().any(|i| i.contains("private")),
            "the collision is reported: {:?}",
            report.ignored
        );
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("still there");
        assert_eq!(payload.content, "my private note", "content preserved");
        assert!(
            payload.metadata.get("mesh").is_none(),
            "no provenance was injected into a private row"
        );
    }

    /// Lifecycle-light (slice 6 owns the table): a candidate whose lifecycle
    /// state is `archived` applies and merges the existing 1.8.0
    /// `{"archived": true}` metadata convention onto the row.
    #[tokio::test]
    async fn archived_candidate_merges_the_archived_tombstone_convention() {
        let (_base, mesh) = mesh_with_commit("archived");
        let id = Uuid::new_v4();
        let archived_at = chrono::Utc::now();
        let value = record_value(
            "synaptic-mesh-record/v1",
            id,
            "probe",
            "fact",
            "archived",
            Some(archived_at),
            json!({"text": "the old way is gone"}),
        );
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("archived").await;

        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(
            report.applied, 1,
            "archived candidates still apply: {report:?}"
        );
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(payload.content, "the old way is gone");
        assert_eq!(
            payload.metadata["archived"],
            json!(true),
            "1.8.0 tombstone convention merged"
        );
        assert!(
            payload.metadata["archived_at"].is_i64(),
            "archived_at timestamp written: {:?}",
            payload.metadata
        );
        assert!(payload.metadata.get("mesh").is_some(), "provenance kept");
    }

    /// A procedure-definition candidate applies as an untrusted import and
    /// carries NO runtime counters — the fixture file itself has none, and
    /// the applied row's metadata gains none.
    #[tokio::test]
    async fn procedure_definition_applies_and_carries_no_counters() {
        let (_base, mesh) = mesh_with_commit("procedure");
        let id = Uuid::new_v4();
        let proc_json = json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": id.to_string(),
            "namespace": "probe",
            "memory_type": "procedure",
            "lifecycle": lifecycle_json("active", None),
            "content": {
                "trigger": "session-start",
                "remaining_fires_initial": 10,
                "valid_to": Value::Null,
                "rehearsal_script": "Read the mesh design spec once."
            },
            "metadata": {"eligibility": "shareable", "trust": "untrusted"},
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": "untrusted"
        });
        let body = serde_json::to_string(&proc_json).unwrap();
        let _rel = commit_record_json(&mesh, "probe", "procedures", &id.to_string(), &body);
        // The ASSERT: the shared file itself carries no counters.
        for counter in RUNTIME_COUNTER_KEYS {
            assert!(
                !body.contains(&format!("\"{counter}\"")),
                "the shared file must not carry runtime counter {counter}"
            );
        }

        let store = fresh_store("procedure").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(report.ignored.is_empty());
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("procedure applied");
        assert_eq!(payload.memory_type, "procedure");
        assert!(
            payload.content.contains("rehearsal_script"),
            "the definition rides in content"
        );
        // And the applied row gains no counters either — import cannot reset
        // local procedure budgets.
        for counter in RUNTIME_COUNTER_KEYS {
            assert!(
                payload.metadata.get(counter).is_none(),
                "applied procedure row must not carry counter {counter}: {:?}",
                payload.metadata
            );
        }
        assert!(payload.metadata.get("mesh").is_some(), "provenance present");
        assert_eq!(
            payload.metadata["trust"],
            json!("untrusted"),
            "imports are untrusted"
        );
    }

    /// The cursor works: with the accepted head recorded, an ff pull and a
    /// second commit apply ONLY the changed file.
    #[tokio::test]
    async fn cursor_second_commit_applies_only_the_changed_file() {
        let (_base, mesh) = mesh_with_commit("cursor");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &a.to_string(),
            &serde_json::to_string(&simple_rule(a, "first commit")).unwrap(),
        );
        let store = fresh_store("cursor").await;
        let served = ["probe".to_string()];

        let first = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(first.applied, 1);

        let id_b = commit_record_json(
            &mesh,
            "probe",
            "memories",
            &b.to_string(),
            &serde_json::to_string(&simple_rule(b, "second commit")).unwrap(),
        );

        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert!(
            !second.full_tree,
            "the cursor exists: replay must be diff-based"
        );
        assert_eq!(
            second.applied,
            1,
            "only the changed file applies: {report:?}",
            report = second
        );
        assert!(second.applied_ids.contains(&b.to_string()));
        assert!(!second.applied_ids.contains(&a.to_string()));
        assert_eq!(
            second.ignored.iter().any(|i| i.contains(&id_b)),
            false,
            "the applied file must not also be ignored"
        );

        let (_, row_a) = store
            .get("probe", &a.to_string())
            .await
            .unwrap()
            .expect("row a");
        let (_, row_b) = store
            .get("probe", &b.to_string())
            .await
            .unwrap()
            .expect("row b");
        assert_eq!(row_a.content, "first commit");
        assert_eq!(row_b.content, "second commit");
    }

    /// A dirty checkout pauses the whole pass (spec §Recovery: "Dirty
    /// working tree → pause sync, do not touch local store"): a full-tree
    /// pass reads candidate CONTENT off the disk, and disk ≠ committed when
    /// the tree is dirty. The refusal names the dirty paths and the
    /// recovery hint, writes NO cursor, and leaves the store untouched;
    /// once the checkout is clean again the pass applies normally.
    #[tokio::test]
    async fn dirty_checkout_refuses_full_tree_pass() {
        let (_base, mesh) = mesh_with_commit("dirty-full");
        let id = Uuid::new_v4();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "the committed content")).unwrap(),
        );
        // Dirty BOTH ways: a modified tracked record (uncommitted content is
        // exactly what a full-tree pass would wrongly read off disk) and an
        // untracked extra file.
        let record_path = mesh
            .root()
            .join("probe")
            .join("memories")
            .join(format!("{id}.json"));
        std::fs::write(
            &record_path,
            &serde_json::to_string(&simple_rule(id, "uncommitted local edit")).unwrap(),
        )
        .unwrap();
        let junk_name = format!("{}.txt", Uuid::new_v4());
        let junk_rel = format!("probe/memories/{junk_name}");
        std::fs::write(mesh.root().join(&junk_rel), "untracked junk").unwrap();

        let store = fresh_store("dirty-full").await;
        let served = ["probe".to_string()];
        let err = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .expect_err("a dirty checkout must refuse the entire apply pass");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("dirty mesh checkout"),
            "the failure names the dirty checkout: {msg}"
        );
        assert!(msg.contains(&id.to_string()), "dirty path named: {msg}");
        assert!(msg.contains(&junk_name), "dirty path named: {msg}");
        assert!(
            msg.contains("commit or restore the mesh checkout, then pull again"),
            "the recovery hint travels with the failure: {msg}"
        );
        assert_eq!(row_count(&store, "probe").await, 0, "store untouched");
        assert!(
            load_cursor(mesh.root(), &mesh_id_str(&mesh)).is_none(),
            "cursor NOT written on a refused pass"
        );

        // Committed (clean again): the same pass applies normally.
        mesh.commit_all("clean local edits", "node-peer-1").unwrap();
        let report = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .expect("clean checkout applies");
        assert_eq!(report.applied, 1, "the record applies normally");
        assert_eq!(row_count(&store, "probe").await, 1);
        let head = current_head(mesh.root()).unwrap().unwrap();
        assert_eq!(load_cursor(mesh.root(), &mesh_id_str(&mesh)).unwrap(), head);
    }

    /// The guard sits at pass ENTRY, so the cursor-diff path is covered
    /// too: a valid cursor behind HEAD plus dirty tree refuses without
    /// advancing the cursor; cleaned up, the diff pass applies only the
    /// committed change.
    #[tokio::test]
    async fn dirty_guard_covers_diff_path_too() {
        let (_base, mesh) = mesh_with_commit("dirty-diff");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &a.to_string(),
            &serde_json::to_string(&simple_rule(a, "first commit")).unwrap(),
        );
        let store = fresh_store("dirty-diff").await;
        let served = ["probe".to_string()];
        let first = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(first.applied, 1);
        let accepted = load_cursor(mesh.root(), &mesh_id_str(&mesh)).unwrap();

        // Second commit; the accepted cursor deliberately stays one commit
        // behind — the next pass would be a diff pass.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &b.to_string(),
            &serde_json::to_string(&simple_rule(b, "second commit")).unwrap(),
        );
        let junk_name = format!("{}.txt", Uuid::new_v4());
        let junk_rel = format!("probe/memories/{junk_name}");
        std::fs::write(mesh.root().join(&junk_rel), "untracked junk").unwrap();

        let err = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .expect_err("a dirty checkout must refuse even a diff pass");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("dirty mesh checkout") && msg.contains(&junk_name),
            "the refusal names the checkout and its dirty paths: {msg}"
        );
        assert!(
            msg.contains("commit or restore the mesh checkout, then pull again"),
            "recovery hint present: {msg}"
        );
        assert_eq!(
            load_cursor(mesh.root(), &mesh_id_str(&mesh)).unwrap(),
            accepted,
            "cursor NOT advanced on a refused pass"
        );

        // Clean again: the diff pass applies only the second commit.
        std::fs::remove_file(mesh.root().join(&junk_rel)).unwrap();
        let second = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .unwrap();
        assert!(
            !second.full_tree,
            "the cursor exists: replay must be diff-based"
        );
        assert_eq!(second.applied, 1, "only the changed file applies");
        assert!(second.applied_ids.contains(&b.to_string()));
    }

    /// The wire arm routes to this module; a pull with no joined mesh
    /// answers the canonical prefix, never crashes.
    #[tokio::test]
    async fn wire_pull_without_mesh_answers_no_mesh_joined() {
        let project = scratch("wire-nomesh");
        let out = handle_mesh_pull(
            &json!({"project_root": project.display().to_string()}),
            fresh_store("wire-nomesh").await,
            emb(),
        )
        .await;
        assert!(
            out.starts_with("no mesh joined"),
            "canonical no-mesh answer required, got: {out}"
        );
    }

    // ---- slice 6: lifecycle gate + concurrent merge table ----------------
    //
    // The merge table is `docs/…/2026-10-10-synaptic-mesh-design.md`
    // §"Concurrent merge policy". Every row gets a test; classify_merge is a
    // pure function so the table is exercised directly, and the lifecycle
    // gate (tombstone wins / retired ids never reactivate / supersede
    // authoritative) is additionally proven end-to-end through apply_changed.

    /// Build a [`MeshRecord`] directly for the pure-function table tests.
    fn record_at(
        id: Uuid,
        state: LifecycleState,
        superseded_by: Option<Uuid>,
        text: &str,
    ) -> MeshRecord {
        MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
            id,
            namespace: "probe".to_string(),
            memory_type: "rule".to_string(),
            lifecycle: Lifecycle {
                state,
                created_at: chrono::Utc::now(),
                created_by: "node-peer-1".to_string(),
                superseded_by,
                archived_at: None,
                tombstoned_at: None,
            },
            content: json!({ "text": text }),
            metadata: json!({ "eligibility": "shareable", "trust": "untrusted" }),
            rdf: None,

            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
            signature: None,
            signer_did: None,
            public_key: None,
            patch: None,
        }
    }

    /// Build a record JSON with an explicit lifecycle state + `superseded_by`,
    /// for committing through the mesh in the end-to-end tests.
    fn record_with_state(id: Uuid, state: &str, superseded_by: Option<Uuid>, text: &str) -> Value {
        json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": id.to_string(),
            "namespace": "probe",
            "memory_type": "rule",
            "lifecycle": {
                "state": state,
                "created_at": "2026-10-11T00:00:00Z",
                "created_by": "node-peer-1",
                "superseded_by": superseded_by.map(|u| json!(u.to_string())).unwrap_or(Value::Null),
                "archived_at": Value::Null,
                "tombstoned_at": Value::Null
            },
            "content": {"text": text},
            "metadata": {"eligibility": "shareable", "trust": "untrusted"},
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": "untrusted"
        })
    }

    /// Row 1 — `archive + content edit`: edited content stays archived; the
    /// combination is reported, never resolved by timestamp.
    #[test]
    fn merge_table_row_archive_plus_edit_keeps_edited_content_archived() {
        let id = Uuid::new_v4();
        let cand = record_at(id, LifecycleState::Archived, None, "edited content");
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "original", &cand, true),
            MergeDecision::ApplyCombination("archive + edit; edited content stays archived")
        );
        // The reverse direction (we archived, they edited) resolves the same.
        let cand2 = record_at(id, LifecycleState::Active, None, "edited content");
        assert_eq!(
            classify_merge(LifecycleState::Archived, None, "original", &cand2, true),
            MergeDecision::ApplyCombination("archive + edit; edited content stays archived")
        );
    }

    /// Row 2 — `restore + archive`: unresolved; keep inactive pending explicit
    /// resolution.
    #[test]
    fn merge_table_row_restore_plus_archive_is_unresolved() {
        let id = Uuid::new_v4();
        let cand = record_at(id, LifecycleState::Archived, None, "same content");
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "same content", &cand, true),
            MergeDecision::Conflict("restore + archive")
        );
        let cand2 = record_at(id, LifecycleState::Active, None, "same content");
        assert_eq!(
            classify_merge(LifecycleState::Archived, None, "same content", &cand2, true),
            MergeDecision::Conflict("restore + archive")
        );
        // Non-concurrent (linear) restore/archive is a plain forward apply.
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "same content", &cand, false),
            MergeDecision::Apply
        );
    }

    /// Row 3 — `supersede + stale restore`: superseded wins; the restore is
    /// refused and reported, never stomped.
    #[test]
    fn merge_table_row_supersede_beats_stale_restore() {
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        let cand = record_at(id, LifecycleState::Active, None, "original");
        assert_eq!(
            classify_merge(
                LifecycleState::Superseded,
                Some(replacement),
                "original",
                &cand,
                true
            ),
            MergeDecision::Refuse("superseded; rejected stale restore".to_string())
        );
    }

    /// Row 4 — `supersede + content edit`: replacement authoritative; the edit
    /// is preserved as a new record referencing the retired id.
    #[test]
    fn merge_table_row_supersede_plus_edit_preserves_edit_as_new_record() {
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        // Direction A: we superseded, the candidate is the edit.
        let cand = record_at(id, LifecycleState::Active, None, "edited content");
        assert_eq!(
            classify_merge(
                LifecycleState::Superseded,
                Some(replacement),
                "original",
                &cand,
                true
            ),
            MergeDecision::SupersedeWinsPreserveEdit
        );
        // Direction B: we edited, the candidate is the supersession.
        let cand2 = record_at(
            id,
            LifecycleState::Superseded,
            Some(replacement),
            "original",
        );
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "edited content", &cand2, true),
            MergeDecision::SupersedeWinsPreserveEdit
        );
    }

    /// Row 5 — `two different replacements` (X→Y vs X→Z): unresolved; never
    /// pick by timestamp; preserve both candidates.
    #[test]
    fn merge_table_row_two_different_replacements_is_unresolved() {
        let id = Uuid::new_v4();
        let y = Uuid::new_v4();
        let z = Uuid::new_v4();
        let cand = record_at(id, LifecycleState::Superseded, Some(z), "original");
        assert_eq!(
            classify_merge(LifecycleState::Superseded, Some(y), "original", &cand, true),
            MergeDecision::Conflict("two different replacements")
        );
        // The same replacement is idempotent, not a conflict.
        let same = record_at(id, LifecycleState::Superseded, Some(y), "original");
        assert_eq!(
            classify_merge(LifecycleState::Superseded, Some(y), "original", &same, true),
            MergeDecision::Skip
        );
    }

    /// Row 6 — `incompatible content edits`: unresolved when concurrent;
    /// preserve both versions.
    #[test]
    fn merge_table_row_incompatible_content_edits_is_unresolved() {
        let id = Uuid::new_v4();
        let cand = record_at(id, LifecycleState::Active, None, "their edit");
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "our edit", &cand, true),
            MergeDecision::Conflict("incompatible content edits")
        );
        // A non-concurrent (linear) edit is a plain forward apply.
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "our edit", &cand, false),
            MergeDecision::Apply
        );
    }

    /// Row 7 — `tombstone + edit/restore`: tombstone wins; never serve
    /// deleted content.
    #[test]
    fn merge_table_row_tombstone_always_wins() {
        let id = Uuid::new_v4();
        let edit = record_at(id, LifecycleState::Active, None, "edited after delete");
        assert_eq!(
            classify_merge(LifecycleState::Tombstoned, None, "", &edit, true),
            MergeDecision::Refuse("tombstone wins; deleted content is never served".to_string())
        );
        let restore = record_at(id, LifecycleState::Archived, None, "original");
        assert_eq!(
            classify_merge(LifecycleState::Tombstoned, None, "", &restore, false),
            MergeDecision::Refuse("tombstone wins; deleted content is never served".to_string())
        );
        // An incoming tombstone over a live row applies.
        let tomb = record_at(id, LifecycleState::Tombstoned, None, "original");
        assert_eq!(
            classify_merge(LifecycleState::Active, None, "original", &tomb, true),
            MergeDecision::Apply
        );
        assert_eq!(
            classify_merge(LifecycleState::Archived, None, "original", &tomb, false),
            MergeDecision::Apply
        );
    }

    /// Retired ids (superseded/tombstoned) never reactivate — the gate refuses
    /// any superseded/tombstoned -> live hop.
    #[test]
    fn retired_id_reactivation_is_refused() {
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        let active = record_at(id, LifecycleState::Active, None, "original");
        assert!(
            matches!(
                classify_merge(
                    LifecycleState::Superseded,
                    Some(replacement),
                    "original",
                    &active,
                    false
                ),
                MergeDecision::Refuse(_)
            ),
            "superseded -> active refused"
        );
        let archived = record_at(id, LifecycleState::Archived, None, "original");
        assert!(
            matches!(
                classify_merge(
                    LifecycleState::Superseded,
                    Some(replacement),
                    "original",
                    &archived,
                    false
                ),
                MergeDecision::Refuse(_)
            ),
            "superseded -> archived refused"
        );
        assert!(
            matches!(
                classify_merge(LifecycleState::Tombstoned, None, "original", &active, false),
                MergeDecision::Refuse(_)
            ),
            "tombstoned -> active refused"
        );
    }

    /// Task 1 of the Lean4 formal-verification series (`neurostrata-ngd9`): the
    /// `Superseded` + `Archived` concurrent-edit pairs must preserve the losing
    /// edit (`SupersedeWinsPreserveEdit`), and `classify_merge_parity_tag` must
    /// expose the canonical Lean truth-table tag for each decision.
    #[test]
    fn supersede_and_archived_preserve_edit_and_parity_tags() {
        let u1 = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_0001);

        // Superseded local + archived candidate + content edit → preserve edit.
        let archived_cand = record_at(Uuid::new_v4(), LifecycleState::Archived, None, "edited");
        assert_eq!(
            classify_merge(LifecycleState::Superseded, Some(u1), "orig", &archived_cand, true),
            MergeDecision::SupersedeWinsPreserveEdit
        );
        // Archived local + superseded candidate + content edit → preserve edit.
        let superseded_cand = record_at(Uuid::new_v4(), LifecycleState::Superseded, Some(u1), "orig");
        assert_eq!(
            classify_merge(LifecycleState::Archived, None, "edited", &superseded_cand, true),
            MergeDecision::SupersedeWinsPreserveEdit
        );
        // Same content on a retired id is a stale restore: refused, never applied.
        assert_eq!(
            classify_merge(
                LifecycleState::Superseded,
                Some(u1),
                "same",
                &record_at(Uuid::new_v4(), LifecycleState::Archived, None, "same"),
                true
            ),
            MergeDecision::Refuse("superseded; rejected stale restore".to_string())
        );
        // Same content on an archived row with a superseding candidate: apply.
        assert_eq!(
            classify_merge(
                LifecycleState::Archived,
                None,
                "same",
                &record_at(Uuid::new_v4(), LifecycleState::Superseded, Some(u1), "same"),
                true
            ),
            MergeDecision::Apply
        );

        // Parity tags: one canonical string per merge decision.
        assert_eq!(
            classify_merge_parity_tag(
                LifecycleState::Superseded,
                LifecycleState::Archived,
                true,
                true,
                true
            ),
            "supersede_wins_preserve_edit"
        );
        assert_eq!(
            classify_merge_parity_tag(
                LifecycleState::Archived,
                LifecycleState::Superseded,
                true,
                true,
                true
            ),
            "supersede_wins_preserve_edit"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Active, LifecycleState::Tombstoned, true, false, false),
            "apply"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Active, LifecycleState::Archived, true, true, true),
            "apply_combination_archive_edit"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Superseded, LifecycleState::Superseded, true, false, true),
            "skip"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Tombstoned, LifecycleState::Active, true, false, true),
            "refuse_tombstone_wins"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Superseded, LifecycleState::Archived, true, false, true),
            "refuse_stale_restore"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Superseded, LifecycleState::Superseded, false, false, true),
            "conflict_two_replacements"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Active, LifecycleState::Archived, true, false, true),
            "conflict_restore_archive"
        );
        assert_eq!(
            classify_merge_parity_tag(LifecycleState::Active, LifecycleState::Active, true, true, true),
            "conflict_incompatible_edits"
        );
    }

    /// End-to-end: a tombstone always wins and a retired id never resurrects —
    /// the resurrection candidate is refused and reported, never stomped.
    #[tokio::test]
    async fn tombstone_never_resurrects_end_to_end() {
        let (_base, mesh) = mesh_with_commit("tombstone-e2e");
        let id = Uuid::new_v4();
        let served = ["probe".to_string()];
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&record_with_state(id, "active", None, "original")).unwrap(),
        );
        let store = fresh_store("tombstone-e2e").await;
        assert_eq!(
            apply_changed(&mesh, &served, store.clone(), emb())
                .await
                .unwrap()
                .applied,
            1
        );

        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&record_with_state(id, "tombstoned", None, "original")).unwrap(),
        );
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(second.applied, 1, "tombstone applies: {second:?}");
        let (_, row) = store.get("probe", &id.to_string()).await.unwrap().unwrap();
        assert_eq!(row.metadata["tombstoned"], json!(true));

        // Resurrection attempt.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&record_with_state(id, "active", None, "zombie")).unwrap(),
        );
        let third = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(third.applied, 0, "resurrection never applies: {third:?}");
        assert!(
            third.ignored.iter().any(|i| i.contains("tombstone")),
            "refused and reported: {:?}",
            third.ignored
        );
        let (_, row) = store.get("probe", &id.to_string()).await.unwrap().unwrap();
        assert_eq!(row.metadata["tombstoned"], json!(true), "still tombstoned");
        assert_eq!(row.content, "original", "deleted content never re-served");
    }

    /// End-to-end: `supersede + content edit` keeps the replacement
    /// authoritative AND preserves the losing edit as a NEW record referencing
    /// the retired id — never dropped.
    #[tokio::test]
    async fn supersede_plus_edit_preserves_edit_as_new_record_end_to_end() {
        let (_base, mesh) = mesh_with_commit("supersede-edit-e2e");
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        let served = ["probe".to_string()];
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&record_with_state(
                id,
                "superseded",
                Some(replacement),
                "original",
            ))
            .unwrap(),
        );
        let store = fresh_store("supersede-edit-e2e").await;
        assert_eq!(
            apply_changed(&mesh, &served, store.clone(), emb())
                .await
                .unwrap()
                .applied,
            1
        );

        // The peer's concurrent edit arrives.
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&record_with_state(id, "active", None, "edited content"))
                .unwrap(),
        );
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(
            second.applied, 1,
            "the edit is preserved as a new record: {second:?}"
        );

        // X stays superseded.
        let (_, x) = store.get("probe", &id.to_string()).await.unwrap().unwrap();
        assert_eq!(x.metadata["mesh"]["lifecycle_state"], json!("superseded"));

        // A new record Z exists with the edited content, referencing X.
        let all = store.list("probe", None, None).await.unwrap();
        assert_eq!(
            all.len(),
            2,
            "X superseded + preserved edit Z: {:?}",
            all.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
        );
        let z = all
            .iter()
            .find(|r| r.id != id.to_string())
            .expect("the preserved edit record");
        assert_eq!(z.payload.content, "edited content");
        assert_eq!(z.payload.metadata["references"], json!([id.to_string()]));
    }

    /// Direction B (`local is the edit, candidate supersedes`) preserves the
    /// local procedure row's structured definition: content stays an object
    /// with its trigger/valid_to/rehearsal_script keys intact, not a
    /// `{"text": ...}` string wrapper, and the row's own metadata tags and a
    /// references link survive (slice-6b fix).
    #[tokio::test]
    async fn direction_b_preserves_procedure_definition_structure() {
        let root = scratch("dirb-proc");
        let mesh_id = "mesh-dirb".to_string();
        let namespace = "probe".to_string();
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();

        // The local row: a procedure whose definition rides in content as its
        // JSON serialization (the apply path stores `content_text`), plus tags
        // and a references relationship entry in metadata.
        let definition = json!({
            "trigger": "session-start",
            "remaining_fires_initial": 5,
            "valid_to": "2027-01-01T00:00:00Z",
            "rehearsal_script": { "steps": ["read spec", "act"] },
        });
        let referenced = Uuid::new_v4();
        let local_payload = MemoryPayload {
            content: definition.to_string(),
            user_id: "node-local".to_string(),
            memory_type: "procedure".to_string(),
            agent_name: Some("node-local".to_string()),
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "tags": ["mesh", "procedure"],
                "references": [referenced.to_string()],
                "mesh": {
                    "update_id": Uuid::new_v4().to_string(),
                    "origin": "node-local",
                    "lifecycle_state": "active",
                },
            }),
        };

        // The incoming supersession: the authoritative replacement.
        let cand = MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
            id,
            namespace: namespace.clone(),
            memory_type: "procedure".to_string(),
            lifecycle: Lifecycle {
                state: LifecycleState::Superseded,
                created_at: chrono::Utc::now(),
                created_by: "node-peer-1".to_string(),
                superseded_by: Some(replacement),
                archived_at: None,
                tombstoned_at: None,
            },
            content: json!({
                "trigger": "every-n-days:7",
                "remaining_fires_initial": 1,
                "valid_to": Value::Null,
                "rehearsal_script": "the replacement",
            }),
            metadata: json!({ "eligibility": "shareable", "trust": "untrusted" }),
            rdf: None,

            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
            signature: None,
            signer_did: None,
            public_key: None,
            patch: None,
        };

        let store = fresh_store("dirb-proc").await;
        store.init(&namespace).await.unwrap();
        let disposition = preserve_superseded_edit(
            &root,
            &mesh_id,
            &id.to_string(),
            &namespace,
            LifecycleState::Active,
            &local_payload,
            &cand,
            store.clone(),
            emb(),
            Trust::Untrusted,
            "unsigned",
            &SignatureDisposition::Unsigned,
        )
        .await;
        assert!(
            matches!(disposition, Disposition::Applied),
            "{disposition:?}"
        );

        // The preserved edit is a NEW row whose content is the structured
        // definition, not a string wrapper.
        let all = store.list(&namespace, None, None).await.unwrap();
        assert_eq!(all.len(), 2, "superseded X + preserved edit Z");
        let z = all
            .iter()
            .find(|r| r.id != id.to_string())
            .expect("the preserved edit record");
        let content_value: Value = serde_json::from_str(&z.payload.content).unwrap();
        assert!(
            content_value.is_object(),
            "definition not string-wrapped: {content_value}"
        );
        assert_eq!(content_value["trigger"], json!("session-start"));
        assert_eq!(content_value["valid_to"], json!("2027-01-01T00:00:00Z"));
        assert_eq!(
            content_value["rehearsal_script"]["steps"],
            json!(["read spec", "act"])
        );

        // Original tags survive; the references metadata carries the ORIGINAL
        // id link first, then the edit's own carried reference (F5: the
        // preserved edit no longer discards its reference links).
        assert_eq!(z.payload.metadata["tags"], json!(["mesh", "procedure"]));
        assert_eq!(
            z.payload.metadata["references"],
            json!([id.to_string(), referenced.to_string()])
        );

        // The typed relationships carry the row's own references link forward
        // AND append the spec's references -> original_id link.
        let rebuilt = preserved_edit_record(
            &namespace,
            &id.to_string(),
            local_content_value(&local_payload),
            local_payload.memory_type.clone(),
            local_origin(&local_payload),
            local_relationships(&local_payload),
            local_payload.metadata.clone(),
            Trust::Untrusted,
            cand.origin_mesh_epoch,
        );
        assert!(
            rebuilt.relationships.contains(&Relationship {
                kind: "references".to_string(),
                target: id,
            }),
            "references original_id link: {:?}",
            rebuilt.relationships
        );
        assert!(
            rebuilt.relationships.contains(&Relationship {
                kind: "references".to_string(),
                target: referenced,
            }),
            "carried references link: {:?}",
            rebuilt.relationships
        );
    }

    /// Direction B merges the local row's metadata with the gate fields
    /// instead of a fixed literal: a pre-existing key (`custom_key`) survives
    /// alongside `references`, `trust` and `eligibility`.
    #[tokio::test]
    async fn direction_b_metadata_merge_does_not_clobber_row_keys() {
        let root = scratch("dirb-meta");
        let mesh_id = "mesh-dirb".to_string();
        let namespace = "probe".to_string();
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();

        let local_payload = MemoryPayload {
            content: "the local edit".to_string(),
            user_id: "node-local".to_string(),
            memory_type: "rule".to_string(),
            agent_name: Some("node-local".to_string()),
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "custom_key": "custom_value",
                "mesh": {
                    "update_id": Uuid::new_v4().to_string(),
                    "origin": "node-local",
                    "lifecycle_state": "active",
                },
            }),
        };

        let cand = MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
            id,
            namespace: namespace.clone(),
            memory_type: "rule".to_string(),
            lifecycle: Lifecycle {
                state: LifecycleState::Superseded,
                created_at: chrono::Utc::now(),
                created_by: "node-peer-1".to_string(),
                superseded_by: Some(replacement),
                archived_at: None,
                tombstoned_at: None,
            },
            content: json!({ "text": "the replacement" }),
            metadata: json!({ "eligibility": "shareable", "trust": "untrusted" }),
            rdf: None,

            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
            signature: None,
            signer_did: None,
            public_key: None,
            patch: None,
        };

        let store = fresh_store("dirb-meta").await;
        store.init(&namespace).await.unwrap();
        let disposition = preserve_superseded_edit(
            &root,
            &mesh_id,
            &id.to_string(),
            &namespace,
            LifecycleState::Active,
            &local_payload,
            &cand,
            store.clone(),
            emb(),
            Trust::Untrusted,
            "unsigned",
            &SignatureDisposition::Unsigned,
        )
        .await;
        assert!(
            matches!(disposition, Disposition::Applied),
            "{disposition:?}"
        );

        let all = store.list(&namespace, None, None).await.unwrap();
        let z = all
            .iter()
            .find(|r| r.id != id.to_string())
            .expect("the preserved edit record");

        // The pre-existing key survives; the gate fields are present.
        assert_eq!(z.payload.metadata["custom_key"], json!("custom_value"));
        assert_eq!(z.payload.metadata["references"], json!([id.to_string()]));
        assert_eq!(z.payload.metadata["trust"], json!("untrusted"));
        assert_eq!(z.payload.metadata["eligibility"], json!("shareable"));
    }

    /// Program Task 3 (fix round, M-2), superseded 2026-11-02 (Slice X, F6):
    /// under a Trusted manifest the supersession candidate AND the
    /// preserved edit both take the manifest's `imported_procedures_trust`
    /// stamp — `preserved_edit_record` now threads `import_trust` into its
    /// metadata stamp (untrusted by default, the manifest policy when it
    /// is configured differently). This pins that threading.
    #[tokio::test]
    async fn preserved_edit_honors_the_manifest_policy_under_a_trusted_manifest() {
        let root = scratch("dirb-trusted");
        let mesh_id = "mesh-dirb".to_string();
        let namespace = "probe".to_string();
        let id = Uuid::new_v4();
        let replacement = Uuid::new_v4();

        let definition = json!({
            "trigger": "session-start",
            "remaining_fires_initial": 5,
            "valid_to": "2027-01-01T00:00:00Z",
            "rehearsal_script": { "steps": ["read spec", "act"] },
        });
        let local_payload = MemoryPayload {
            content: definition.to_string(),
            user_id: "node-local".to_string(),
            memory_type: "procedure".to_string(),
            agent_name: Some("node-local".to_string()),
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "mesh": {
                    "update_id": Uuid::new_v4().to_string(),
                    "origin": "node-local",
                    "lifecycle_state": "active",
                },
            }),
        };

        let cand = MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
            id,
            namespace: namespace.clone(),
            memory_type: "procedure".to_string(),
            lifecycle: Lifecycle {
                state: LifecycleState::Superseded,
                created_at: chrono::Utc::now(),
                created_by: "node-peer-1".to_string(),
                superseded_by: Some(replacement),
                archived_at: None,
                tombstoned_at: None,
            },
            content: json!({
                "trigger": "every-n-days:7",
                "remaining_fires_initial": 1,
                "valid_to": Value::Null,
                "rehearsal_script": "the replacement",
            }),
            metadata: json!({ "eligibility": "shareable", "trust": "untrusted" }),
            rdf: None,

            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
            signature: None,
            signer_did: None,
            public_key: None,
            patch: None,
        };

        let store = fresh_store("dirb-trusted").await;
        store.init(&namespace).await.unwrap();
        let disposition = preserve_superseded_edit(
            &root,
            &mesh_id,
            &id.to_string(),
            &namespace,
            LifecycleState::Active,
            &local_payload,
            &cand,
            store.clone(),
            emb(),
            Trust::Trusted,
            "unsigned",
            &SignatureDisposition::Unsigned,
        )
        .await;
        assert!(
            matches!(disposition, Disposition::Applied),
            "{disposition:?}"
        );

        let all = store.list(&namespace, None, None).await.unwrap();
        assert_eq!(all.len(), 2, "superseded X + preserved edit Z");
        let x = all
            .iter()
            .find(|r| r.id == id.to_string())
            .expect("the supersession row");
        assert_eq!(
            x.payload.metadata["trust"],
            json!("trusted"),
            "the candidate takes the manifest stamp: {:?}",
            x.payload.metadata
        );
        let z = all
            .iter()
            .find(|r| r.id != id.to_string())
            .expect("the preserved edit record");
        assert_eq!(
            z.payload.metadata["trust"],
            json!("trusted"),
            "the preserved edit honors the manifest policy stamp: {:?}",
            z.payload.metadata
        );
    }

    /// No silent drop: an unresolved conflict keeps the accepted projection,
    /// preserves the candidate durably outside the checkout, does NOT advance
    /// the cursor (re-presented next pull), and never applies the losing
    /// candidate.
    #[tokio::test]
    async fn conflict_is_never_silently_dropped() {
        let (_base, mesh) = mesh_with_commit("conflict-nodrop");
        let id = Uuid::new_v4();
        let served = ["probe".to_string()];
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "original")).unwrap(),
        );
        let store = fresh_store("conflict-nodrop").await;
        assert_eq!(
            apply_changed(&mesh, &served, store.clone(), emb())
                .await
                .unwrap()
                .applied,
            1
        );

        // Divergent edit: lose the cursor → full-tree → concurrent.
        std::fs::remove_file(cursor_file(mesh.root())).unwrap();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "their edit")).unwrap(),
        );
        let second = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(second.conflicted, 1, "conflict detected: {second:?}");
        assert!(second.conflicted_ids.contains(&id.to_string()));

        // Cursor NOT advanced past an unresolved conflict.
        let head = current_head(mesh.root()).unwrap().unwrap();
        assert_ne!(
            load_cursor(mesh.root(), &mesh_id_str(&mesh)),
            Some(head),
            "cursor must not advance past an unresolved conflict"
        );

        // Re-presentation is idempotent: still conflicted, never applied.
        let third = apply_changed(&mesh, &served, store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(third.conflicted, 1, "still conflicted: {third:?}");
        assert_eq!(third.applied, 0, "the conflict is never silently applied");

        // The accepted projection is preserved, marked, and the candidate is
        // durably preserved outside the checkout.
        let (_, row) = store.get("probe", &id.to_string()).await.unwrap().unwrap();
        assert_eq!(row.content, "original", "accepted projection preserved");
        assert!(
            crate::synaptic_mesh::conflict::is_conflicting(&row),
            "still suppressed"
        );
        let variant = crate::synaptic_mesh::conflict::conflicts_dir_for(mesh.root())
            .join("probe")
            .join(format!("{id}.json"));
        assert!(
            variant.exists(),
            "variant preserved durably outside the checkout"
        );
    }

    /// The acceptance sweep (AC 8): a fact carrying an access topology crosses
    /// the mesh's record path and comes back whole.
    ///
    /// The mesh is closed over `SHAREABLE_MEMORY_TYPES`
    /// (`SHAREABLE_MEMORY_TYPES`, above), so a `fact` is inside the closure
    /// and is applied; and the import carries the record's metadata onto the
    /// row verbatim, so a field the vocabulary only learned in v5 --
    /// `metadata.topology` -- travels unchanged rather than being flattened
    /// away.
    #[tokio::test]
    async fn a_topology_fact_survives_the_mesh_round_trip() {
        let (_base, mesh) = mesh_with_commit("topology");
        let id = Uuid::new_v4();
        let topology = json!({
            "component": "apigateway",
            "target_system": "ledgerservice",
            "channel": "mTLS",
            "credential_boundary": "eu-west-1 VPC",
            "reason": "the ledger accepts traffic only from inside the VPC",
        });
        let mut value = record_value(
            "synaptic-mesh-record/v1",
            id,
            "probe",
            "fact",
            "active",
            None,
            json!({"text": "the ledger is reachable only through the gateway"}),
        );
        value["metadata"]["topology"] = topology.clone();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );

        let store = fresh_store("topology").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("the full-tree replay applies");
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");

        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("the fact crossed the mesh");
        assert_eq!(
            payload.memory_type, "fact",
            "the shareable closure holds: a fact is a fact on the far side"
        );
        assert_eq!(
            payload.metadata["topology"], topology,
            "the import preserves the record's metadata verbatim"
        );
    }

    #[tokio::test]
    async fn an_imported_records_domain_is_unioned_into_the_local_registry() {
        let (_base, mesh) = mesh_with_commit("domainunion");
        let id = Uuid::new_v4();
        let mut value = record_value(
            "synaptic-mesh-record/v1",
            id,
            "probe",
            "fact",
            "active",
            None,
            json!({"text": "auth fact"}),
        );
        value["metadata"]["domain"] = json!("auth");
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );

        let store = fresh_store("domainunion").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .expect("the full-tree replay applies");
        assert_eq!(report.applied, 1, "report: {report:?}");
        assert!(report.ignored.is_empty(), "report: {report:?}");

        store.init("probe").await.unwrap();
        let reg_id = crate::registry::registry_row_id("probe");
        let (_, payload) = store
            .get("probe", &reg_id)
            .await
            .unwrap()
            .expect("registry row present after import");
        let domains = crate::registry::registry_domains(&payload.metadata);
        assert!(
            domains.contains(&"auth".to_string()),
            "domains: {domains:?}"
        );
    }

    // ---- Program Task 3: mesh import trust -------------------------------
    //
    // Survivor rows J2/J3 (binding): the trust stamp on an imported
    // procedure comes from THIS mesh's manifest policy, never from the
    // sender's record — sender metadata is evidence, not authority. The
    // fixtures below deliberately keep encoding sender claims; the funnel
    // must override them in BOTH directions.

    /// Rewrite the on-disk manifest's imported-procedures policy, commit,
    /// and reopen so the cached manifest reflects it (mirrors
    /// `reopen_with_epoch`).
    fn reopen_with_policy(mesh: &MeshStorage, trust: super::super::manifest::Trust) -> MeshStorage {
        let mut manifest = mesh.manifest().clone();
        manifest.policy.imported_procedures_trust = trust;
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(
            mesh.root().join(super::super::storage::MANIFEST_FILE_NAME),
            format!("{json}\n"),
        )
        .unwrap();
        mesh.commit_all("policy change", "node-import-test").unwrap();
        MeshStorage::open(mesh.root()).unwrap()
    }

    /// A procedure record whose sender-side trust claims are parameters:
    /// `metadata_trust` is the metadata claim (None = the record carries no
    /// trust key at all, the shape real exports arrive in), `top_trust` the
    /// top-level record field.
    fn procedure_value(
        id: Uuid,
        metadata_trust: Option<&str>,
        top_trust: &str,
        script: &str,
    ) -> Value {
        let mut metadata = json!({"eligibility": "shareable"});
        if let Some(t) = metadata_trust {
            metadata["trust"] = json!(t);
        }
        json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": id.to_string(),
            "namespace": "probe",
            "memory_type": "procedure",
            "lifecycle": lifecycle_json("active", None),
            "content": {
                "trigger": "session-start",
                "remaining_fires_initial": 10,
                "valid_to": Value::Null,
                "rehearsal_script": script
            },
            "metadata": metadata,
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": top_trust
        })
    }

    /// Fire (or attempt to fire) an applied row through the real gate.
    async fn perform_outcome(store: &std::sync::Arc<dyn VectorStore>, id: &str) -> Value {
        let root = std::env::temp_dir().join(format!("ns-task3-perform-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let text = crate::tools::procedure::handle_procedure_perform(
            json!({"id": id, "project_root": root.display().to_string()}),
            store.clone(),
        )
        .await;
        serde_json::from_str(&text).expect("perform returns JSON")
    }

    /// J2/J3: under the default (untrusted) manifest policy, a sender
    /// claiming "trusted" in BOTH the metadata and the top-level field is
    /// overridden by the manifest — the row is stamped untrusted and the
    /// gate refuses to fire it.
    #[tokio::test]
    async fn imported_procedure_with_forged_trusted_metadata_does_not_fire() {
        let (_base, mesh) = mesh_with_commit("forged");
        let id = Uuid::new_v4();
        let value = procedure_value(id, Some("trusted"), "trusted", "Read the forged spec once.");
        commit_record_json(
            &mesh,
            "probe",
            "procedures",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("forged").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 1, "report: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(
            payload.metadata["trust"],
            json!("untrusted"),
            "the manifest stamps the row, not the sender's claim: {:?}",
            payload.metadata
        );
        let outcome = perform_outcome(&store, &id.to_string()).await;
        assert_eq!(outcome["performed"], json!(false), "{outcome}");
        assert!(
            outcome["reason"]
                .as_str()
                .expect("reason text")
                .starts_with("untrusted"),
            "the gate's refusal names the trust state: {outcome}"
        );
    }

    /// The stamp source is the manifest in both directions: a trusted
    /// manifest overrides a sender's "untrusted" claim UP (and the row
    /// fires), and a record carrying NO trust claim at all — the shape
    /// real exports arrive in — is stamped from the manifest too.
    #[tokio::test]
    async fn manifest_policy_is_the_stamp_source() {
        let (_base, mesh) = mesh_with_commit("pol-trusted");
        let mesh = reopen_with_policy(&mesh, super::super::manifest::Trust::Trusted);
        let id = Uuid::new_v4();
        let value = procedure_value(id, Some("untrusted"), "untrusted", "Read the spec once.");
        commit_record_json(
            &mesh,
            "probe",
            "procedures",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("pol-trusted").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 1, "report: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(
            payload.metadata["trust"],
            json!("trusted"),
            "manifest Trusted wins over the sender's untrusted claim: {:?}",
            payload.metadata
        );
        let outcome = perform_outcome(&store, &id.to_string()).await;
        assert_eq!(outcome["performed"], json!(true), "{outcome}");

        // No sender claim at all, default manifest: stamped untrusted.
        let (_base2, mesh2) = mesh_with_commit("pol-none");
        let id2 = Uuid::new_v4();
        let value2 = procedure_value(id2, None, "untrusted", "Read the unstamped spec once.");
        commit_record_json(
            &mesh2,
            "probe",
            "procedures",
            &id2.to_string(),
            &serde_json::to_string(&value2).unwrap(),
        );
        let store2 = fresh_store("pol-none").await;
        let report2 = apply_changed(&mesh2, &["probe".to_string()], store2.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report2.applied, 1, "report: {report2:?}");
        let (_, payload2) = store2
            .get("probe", &id2.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(
            payload2.metadata["trust"],
            json!("untrusted"),
            "an unstamped sender record takes the manifest stamp: {:?}",
            payload2.metadata
        );
    }

    /// J15 (import leg): a record carrying a detectable secret never
    /// reaches the store — validation refuses it and the refusal lands in
    /// the report's ignore list naming the scan.
    #[tokio::test]
    async fn record_with_secret_is_rejected() {
        let (_base, mesh) = mesh_with_commit("secret");
        let id = Uuid::new_v4();
        let pat = format!("ghp_{}", "A".repeat(36));
        let value = simple_rule(id, &format!("use this token: {pat}"));
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("secret").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        assert!(
            report.ignored[0].contains("secret scan"),
            "the refusal names the scan: {:?}",
            report.ignored
        );
        assert_eq!(row_count(&store, "probe").await, 0, "nothing reached the store");
    }

    /// Program Task 3 (fix round, M-1): the scanner builds its location
    /// from the record's own metadata key names, and those keys are
    /// sender-controlled — a crafted key could ride the ignore report
    /// verbatim. The report names the scan category and which half of
    /// the record matched, never the raw location and never the value.
    #[tokio::test]
    async fn secret_scan_report_never_echoes_sender_key_names() {
        let (_base, mesh) = mesh_with_commit("secret-key");
        let id = Uuid::new_v4();
        let evil_key = format!("ghp_{}", "K".repeat(36));
        let secret_value = format!("ghp_{}", "V".repeat(36));
        let mut value = simple_rule(id, "a perfectly ordinary rule");
        let mut metadata = serde_json::Map::new();
        metadata.insert("eligibility".to_string(), json!("shareable"));
        metadata.insert("trust".to_string(), json!("untrusted"));
        metadata.insert(
            evil_key.clone(),
            json!({ "nested": secret_value.clone() }),
        );
        value["metadata"] = Value::Object(metadata);
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&value).unwrap(),
        );
        let store = fresh_store("secret-key").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0, "report: {report:?}");
        assert_eq!(report.ignored.len(), 1, "report: {report:?}");
        let entry = &report.ignored[0];
        assert!(
            entry.contains("secret scan"),
            "the refusal still names the scan: {entry}"
        );
        assert!(
            entry.contains("github_pat"),
            "the refusal names the scan category: {entry}"
        );
        assert!(
            !entry.contains(&evil_key),
            "the report must not echo a sender-controlled key name: {entry}"
        );
        assert!(
            !entry.contains(&secret_value),
            "the report must not echo the secret value: {entry}"
        );
        assert_eq!(row_count(&store, "probe").await, 0, "nothing reached the store");
    }

    /// J3 (provenance leg): the row keeps the sender's claimed author for
    /// display, but the provenance marks the claim as exactly that — an
    /// unverified assertion — and records the HEAD commit's signature
    /// state as metadata only (DR-9: recorded, never gated on).
    #[tokio::test]
    async fn imported_created_by_is_marked_unverified() {
        let (_base, mesh) = mesh_with_commit("createdby");
        let id = Uuid::new_v4();
        commit_record_json(
            &mesh,
            "probe",
            "memories",
            &id.to_string(),
            &serde_json::to_string(&simple_rule(id, "always use podman")).unwrap(),
        );
        let store = fresh_store("createdby").await;
        let report = apply_changed(&mesh, &["probe".to_string()], store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 1, "report: {report:?}");
        let (_, payload) = store
            .get("probe", &id.to_string())
            .await
            .unwrap()
            .expect("row applied");
        assert_eq!(payload.user_id, "node-peer-1", "the claim is preserved");
        let mesh_meta = payload.metadata.get("mesh").expect("mesh provenance");
        assert_eq!(mesh_meta["origin"], json!("mesh-import"));
        assert_eq!(mesh_meta["claimed_created_by"], json!("node-peer-1"));
        assert_eq!(mesh_meta["created_by_verified"], json!(false));
        assert_eq!(
            mesh_meta["head_commit_signature"],
            json!("unsigned"),
            "test commits are unsigned; the state is recorded, not gated"
        );
    }

    async fn seed_row(store: &std::sync::Arc<dyn VectorStore>, id: &str, memory_type: &str, metadata: Value) {
        store
            .upsert(
                "probe",
                id,
                emb().embed(id).await.unwrap(),
                MemoryPayload {
                    content: format!("row {id}"),
                    user_id: "me".to_string(),
                    memory_type: memory_type.to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata,
                },
            )
            .await
            .unwrap();
    }

    async fn trust_stamp_of(store: &std::sync::Arc<dyn VectorStore>, id: &str) -> Value {
        let (_, payload) = store
            .get("probe", id)
            .await
            .unwrap()
            .expect("row present");
        payload
            .metadata
            .get("trust")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// (c) Backfill: unstamped rows are classified by provenance —
    /// no mesh provenance → local; mesh provenance → untrusted (stays
    /// unfired). Stamped rows and non-procedures are untouched, and a
    /// second pass changes nothing.
    #[tokio::test]
    async fn backfill_stamps_by_provenance() {
        let store = fresh_store("backfill").await;
        store.init("probe").await.unwrap();
        seed_row(&store, "proc-local", "procedure", json!({"trigger": "session-start"})).await;
        seed_row(
            &store,
            "proc-imported",
            "procedure",
            json!({
                "trigger": "session-start",
                "mesh": {"mesh_id": "mesh-alpha", "update_id": "u-1", "origin": "mesh-import"}
            }),
        )
        .await;
        seed_row(&store, "proc-promoted", "procedure", json!({"trust": "promoted"})).await;
        seed_row(&store, "rule-plain", "rule", json!({})).await;

        let (local, imported) = backfill_imported_procedure_trust(&store).await;
        assert_eq!((local, imported), (1, 1), "one of each class stamped");
        assert_eq!(trust_stamp_of(&store, "proc-local").await, json!("local"));
        assert_eq!(trust_stamp_of(&store, "proc-imported").await, json!("untrusted"));
        assert_eq!(
            trust_stamp_of(&store, "proc-promoted").await,
            json!("promoted"),
            "an existing stamp is never rewritten"
        );
        assert_eq!(
            trust_stamp_of(&store, "rule-plain").await,
            Value::Null,
            "non-procedure rows are not stamped"
        );

        let (local2, imported2) = backfill_imported_procedure_trust(&store).await;
        assert_eq!((local2, imported2), (0, 0), "idempotent: nothing left to classify");
    }
}
