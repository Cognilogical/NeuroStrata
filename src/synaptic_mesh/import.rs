//! Mesh import — the CONSUME path of the SynapticMesh: mesh files →
//! validated staging → applied store rows.
//!
//! See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`
//! §"Import validation" and §"Record format". Slice 4 owns this path;
//! slice 3 owns the publish side (outbox/worker) and slice 6 owns the full
//! lifecycle table — this module is deliberately LIFECYCLE-LIGHT.
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

use anyhow::Context as _;
use serde_json::{json, Value};

use super::record::{LifecycleState, MeshRecord, MESH_RECORD_SCHEMA_VERSION};
use super::storage::MeshStorage;
use crate::traits::{Embedder, MemoryPayload, VectorStore};

/// The on-disk schema version of the accepted-head cursor file.
pub const IMPORT_CURSOR_SCHEMA_VERSION: &str = "synaptic-mesh-import-cursor/v1";

/// Memory types a mesh record may carry for it to be applied (the v1
/// shareable set, spec §"Sharing boundary"). `structural` records are
/// project-local AST nodes and never travel; anything unknown (future
/// vocabulary) is ignored and reported, never applied, never crash.
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
    let project_root = arguments
        .get("project_root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

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
        "pulled mesh {mesh_id}: applied {}, skipped {}, ignored {}",
        report.applied,
        report.skipped_update_id,
        report.ignored.len()
    );
    if !report.applied_ids.is_empty() {
        out.push_str(&format!(" ids {}", report.applied_ids.join(", ")));
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
    };

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
        match apply_one(&mesh_id, &candidate, record, store.clone(), emb.clone()).await {
            Disposition::Applied => {
                report.applied += 1;
                report.applied_ids.push(candidate.file_stem.clone());
            }
            Disposition::Skipped => report.skipped_update_id += 1,
            Disposition::Ignored(reason) => {
                report.ignored.push(format!("{rel}: {reason}"));
            }
        }
    }

    // Accept HEAD only after the whole batch was walked. A crash before this
    // line leaves the old cursor: the next pass replays the same files and
    // the update_id skip (plus the private-row guard) makes that a no-op.
    report.head = current_head(root)?.unwrap_or_default();
    if !report.head.is_empty() {
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
    Ok(record)
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
    Skipped,
    Ignored(String),
}

/// Validate-and-apply one candidate against the store.
async fn apply_one(
    mesh_id: &str,
    _candidate: &Candidate,
    record: MeshRecord,
    store: std::sync::Arc<dyn VectorStore>,
    emb: std::sync::Arc<dyn Embedder>,
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
    if let Some((_, payload)) = existing {
        match mesh_update_id_of(&payload.metadata) {
            Some(accepted) if accepted == record.update_id.to_string() => {
                return Disposition::Skipped;
            }
            None => {
                return Disposition::Ignored(format!(
                    "private memory owns id {id} — never overwritten, never deleted"
                ));
            }
            // Mesh-owned row at another revision: the mesh is that row's
            // projection, so the candidate takes over.
            Some(_) => {}
        }
    }

    let text = content_text(&record);
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
    metadata.insert(
        "mesh".to_string(),
        json!({
            "mesh_id": mesh_id,
            "update_id": record.update_id.to_string(),
            "origin": record.lifecycle.created_by,
            "origin_mesh_epoch": record.origin_mesh_epoch,
            "recorded_at": chrono::Utc::now().to_rfc3339()
        }),
    );
    // Lifecycle-light (slice 6 owns the full table): archived merges the
    // existing 1.8.0 {"archived": true} tombstone convention from
    // handlers/archive_memory.rs; superseded upserts the replacement and
    // records the pointer in metadata.
    match record.lifecycle.state {
        LifecycleState::Archived => {
            metadata.insert("archived".to_string(), json!(true));
            metadata.insert(
                "archived_at".to_string(),
                json!(record
                    .lifecycle
                    .archived_at
                    .map(|t| t.timestamp())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp())),
            );
        }
        // TODO(SM-slice-6): the full lifecycle table owns supersession
        // (replacement upsert + old-row retirement). Until then the
        // replacement pointer is recorded on the row itself.
        LifecycleState::Superseded => {
            metadata.insert(
                "superseded_by".to_string(),
                json!(record.lifecycle.superseded_by.map(|u| u.to_string())),
            );
        }
        _ => {}
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
    if let Err(e) = store.upsert(&namespace, &id, vector, payload).await {
        return Disposition::Ignored(format!("store upsert failed for {id}: {e}"));
    }
    Disposition::Applied
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
        store.list(ns, None).await.unwrap().len()
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
        let hits = store.search("probe", query, 5).await.unwrap();
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
}
