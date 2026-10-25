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
    Lifecycle, LifecycleState, MeshRecord, Relationship, MESH_RECORD_SCHEMA_VERSION,
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
        let record = match validate_record(&candidate, served, &raw) {
            Ok(r) => r,
            Err(reason) => {
                report.ignored.push(format!("{rel}: {reason}"));
                continue;
            }
        };
        if let Err(reason) = epoch_gate(record.origin_mesh_epoch, manifest_epoch) {
            report.ignored.push(format!("{rel}: {reason}"));
            continue;
        }
        match apply_one(
            root,
            &mesh_id,
            &candidate,
            record,
            store.clone(),
            emb.clone(),
            concurrent,
            import_trust,
            head_signature,
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
    let text = std::fs::read_to_string(cursor_file(mesh_root)).ok()?;
    let doc: Value = serde_json::from_str(&text).ok()?;
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

/// Parse + validate one record against the pinned contract: known schema
/// version, served namespace parity with its path, shareable memory type,
/// id/filename agreement, no runtime counter keys. `Ok(record)` is apt for
/// apply; `Err(reason)` is an ignore entry.
pub fn validate_record(
    candidate: &Candidate,
    served: &[String],
    raw: &str,
) -> Result<MeshRecord, String> {
    if raw.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "record is {} bytes, over the {}-byte per-record cap (MAX_RECORD_BYTES)",
            raw.len(),
            MAX_RECORD_BYTES
        ));
    }
    let record: MeshRecord =
        serde_json::from_str(raw).map_err(|e| format!("unparseable MeshRecord ({e})"))?;
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
    if record.namespace != candidate.namespace {
        return Err(format!(
            "record namespace {} does not match its path namespace {}",
            record.namespace, candidate.namespace
        ));
    }
    if !SHAREABLE_MEMORY_TYPES.contains(&record.memory_type.as_str()) {
        return Err(format!(
            "memory_type {} is not shareable (shareable: {})",
            record.memory_type,
            SHAREABLE_MEMORY_TYPES.join(", ")
        ));
    }
    if record.id.to_string() != candidate.file_stem {
        return Err(format!(
            "record id {} does not match its filename {}",
            record.id, candidate.file_stem
        ));
    }
    let whole =
        serde_json::to_value(&record).map_err(|e| format!("unparseable MeshRecord ({e})"))?;
    if contains_counter_key(&whole) {
        return Err(
            "runtime procedure counters are strictly local; a shared record must not carry them"
                .to_string(),
        );
    }
    // Secret scan (J15, import leg): the same scanner every local write
    // path answers to, run over the record's content and metadata before
    // anything is applied. The refusal names the scan category and
    // which half of the record matched, never the secret itself — and
    // never the scanner's raw location either: locations are built
    // from the record's own metadata key names, which are
    // sender-controlled and could carry secret-shaped text of their
    // own into the operator-visible report (fix round, M-1).
    let content_scan_text = serde_json::to_string(&record.content)
        .map_err(|e| format!("unparseable MeshRecord content ({e})"))?;
    if let Some(rejection) =
        crate::secrets::scan_entry_point(&content_scan_text, &record.metadata, "mesh_import")
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
    Ok(record)
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
    use std::cmp::Ordering::{Equal, Greater, Less};
    match candidate_epoch.cmp(&manifest_epoch) {
        Equal => Ok(()),
        Less => Err(format!(
            "old-epoch record (origin_mesh_epoch {candidate_epoch} < manifest epoch {manifest_epoch}); quarantined, not replayed"
        )),
        Greater => Err(format!(
            "future-epoch record (origin_mesh_epoch {candidate_epoch} > manifest epoch {manifest_epoch}); refused, not understood"
        )),
    }
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

    // Tombstone is terminal: deletion wins over every stale update, and the
    // deleted content is never re-served nor retained in a conflict artifact.
    if local_state.is_terminal() {
        return match cand_state {
            Tombstoned => MergeDecision::Skip,
            _ => {
                MergeDecision::Refuse("tombstone wins; deleted content is never served".to_string())
            }
        };
    }

    // A retired (superseded) id never reactivates — can_transition_to refuses
    // every superseded -> live hop. The merge table's supersede rows branch
    // from this one gate.
    if local_state == Superseded {
        return match cand_state {
            // Tombstone wins even over a retired id.
            Tombstoned => MergeDecision::Apply,
            Superseded => {
                if cand_sup != local_superseded_by {
                    MergeDecision::Conflict("two different replacements")
                } else {
                    MergeDecision::Skip
                }
            }
            // supersede + content edit: replacement authoritative; the losing
            // edit is preserved as a new record (handled by the caller).
            Active if content_changed => MergeDecision::SupersedeWinsPreserveEdit,
            // restore (or archive) of a retired id is refused, never stomped.
            _ => MergeDecision::Refuse("superseded; rejected stale restore".to_string()),
        };
    }

    // Incoming tombstone always wins over a live row.
    if cand_state == Tombstoned {
        return MergeDecision::Apply;
    }

    // Live -> superseded is always legal.
    if cand_state == Superseded {
        // supersede + content edit (reverse direction): the incoming
        // supersession is authoritative, the local edit is preserved as a new
        // record.
        if local_state == Active && content_changed {
            return MergeDecision::SupersedeWinsPreserveEdit;
        }
        return MergeDecision::Apply;
    }

    // Both live (active/archived): gate the state change through the spec
    // transition table, then overlay the merge-table combinations.
    if cand_state != local_state {
        if content_changed {
            return MergeDecision::ApplyCombination(
                "archive + edit; edited content stays archived",
            );
        }
        // Same content, one side archived and the other restored: the spec's
        // "restore + archive" pair — unresolved only when concurrent.
        if concurrent {
            return MergeDecision::Conflict("restore + archive");
        }
        // The single gate every applied lifecycle change passes through.
        return if local_state.can_transition_to(cand_state) {
            MergeDecision::Apply
        } else {
            MergeDecision::Refuse(format!("illegal transition {local_state} -> {cand_state}"))
        };
    }

    // Same live state: a content change is a forward edit (apply) or, when
    // the two versions diverged, an incompatible-edit conflict.
    if content_changed {
        return if concurrent {
            MergeDecision::Conflict("incompatible content edits")
        } else {
            MergeDecision::Apply
        };
    }
    MergeDecision::Skip
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

/// Validate-and-apply one candidate against the store. `root` is the mesh
/// checkout (its sibling `.conflicts` dir receives preserved variants);
/// `concurrent` is true only on the full-tree reconciliation path.
/// `import_trust` is the manifest's imported-procedures policy (the
/// stamp source for applied procedures); `head_signature` is the HEAD
/// commit's signature state, recorded in provenance only.
async fn apply_one(
    root: &Path,
    mesh_id: &str,
    _candidate: &Candidate,
    record: MeshRecord,
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
    concurrent: bool,
    import_trust: Trust,
    head_signature: &str,
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
            Some(_) => {
                let Some((local_state, local_superseded_by)) = local_mesh_state(&payload.metadata)
                else {
                    return Disposition::Ignored(format!(
                        "mesh row {id} has unreadable lifecycle provenance"
                    ));
                };
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
) -> Disposition {
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
    metadata.insert(
        "mesh".to_string(),
        mesh_provenance(
            mesh_id,
            &record.update_id,
            &record.lifecycle.created_by,
            record.origin_mesh_epoch,
            state,
            superseded_by,
            head_signature,
        ),
    );
    apply_lifecycle_markers(
        &mut metadata,
        state,
        superseded_by,
        archived_at,
        tombstoned_at,
    );

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
    let _ = conflict::clear_conflict(root, namespace, id);
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
        cand.origin_mesh_epoch,
    );
    let new_id = new_record.id.to_string();
    // No trust_stamp: `preserved_edit_record` is funnel code and has
    // already set the preserved record's stamp ("untrusted") itself —
    // the embryo this task generalizes. Stamping the manifest policy
    // here would overwrite the funnel's own decision.
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
/// §"Concurrent merge policy"), surfaced in metadata too so the linkage is
/// visible on the store row. Only `eligibility`, `trust` and `references` are
/// (re)set on the metadata — every other key the edit carried survives.
fn preserved_edit_record(
    namespace: &str,
    original_id: &str,
    content: Value,
    memory_type: String,
    created_by: String,
    source_rels: Vec<Relationship>,
    source_metadata: Value,
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
    metadata.insert("trust".to_string(), json!("untrusted"));
    metadata.insert("references".to_string(), json!([original_id]));
    MeshRecord {
        schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
        id: Uuid::new_v4(),
        namespace: namespace.to_string(),
        memory_type,
        lifecycle: Lifecycle {
            state: LifecycleState::Active,
            created_at: chrono::Utc::now(),
            created_by,
            superseded_by: None,
            archived_at: None,
            tombstoned_at: None,
        },
        content,
        metadata: Value::Object(metadata),
        relationships: rels,
        update_id: Uuid::new_v4(),
        origin_mesh_epoch,
        trust: Trust::Untrusted,
    }
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
            let Ok(Some((vector, mut payload))) = store.get(namespace, &row.id).await else {
                continue;
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
            if store.upsert(namespace, &row.id, vector, payload).await.is_ok() {
                if imported {
                    stamped_untrusted += 1;
                } else {
                    stamped_local += 1;
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
    use std::os::unix::fs::symlink;
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
        let secret = _base.join("outside.json");
        std::fs::write(&secret, r#"{"id":"stolen"}"#).unwrap();

        let evil = Uuid::new_v4();
        let d = mesh.root().join("probe").join("memories");
        std::fs::create_dir_all(&d).unwrap();
        symlink(&secret, d.join(format!("{evil}.json"))).unwrap();

        let exec_id = Uuid::new_v4();
        let bad_id = Uuid::new_v4();
        commit_record_json(&mesh, "probe", "memories", &bad_id.to_string(), "{not json");
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
        mesh.commit_all("peer bits", "node-peer-1").unwrap();

        let store = fresh_store("hostile").await;
        store.init("probe").await.unwrap();
        let report = apply_changed(&mesh, &served.to_vec(), store.clone(), emb())
            .await
            .unwrap();
        assert_eq!(report.applied, 0, "hostile files never apply: {report:?}");
        let joined = report.ignored.join(" | ");
        assert!(
            report.ignored.iter().any(|i| i.contains("symlink")),
            "symlink ignored+named: {joined}"
        );
        assert!(
            report.ignored.iter().any(|i| i.contains("executable")),
            "exec-bit ignored+named: {joined}"
        );
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
        let rel = commit_record_json(&mesh, "probe", "procedures", &id.to_string(), &body);
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
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
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
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
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

        // Original tags survive; the references link lands in metadata.
        assert_eq!(z.payload.metadata["tags"], json!(["mesh", "procedure"]));
        assert_eq!(z.payload.metadata["references"], json!([id.to_string()]));

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
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
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

    /// Program Task 3 (fix round, M-2): under a Trusted manifest the
    /// supersession candidate takes the manifest stamp, but the
    /// preserved edit keeps the funnel's own "untrusted" — its apply
    /// passes no stamp. This pins that threading: handing the preserved
    /// record the manifest stamp turns this test red.
    #[tokio::test]
    async fn preserved_edit_stays_untrusted_under_a_trusted_manifest() {
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
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Untrusted,
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
            json!("untrusted"),
            "the preserved edit keeps the funnel's own stamp: {:?}",
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
            "component": "api-gateway",
            "target_system": "ledger-service",
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
