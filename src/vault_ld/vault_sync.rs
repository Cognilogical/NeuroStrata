//! Vault-LD sync module — W3C YAML-LD 1.0 / Vault-LD bidirectional Markdown vault sync.
//!
//! Contains the shared MCP + CLI handler plus entry-point export/import logic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::rebac;
use crate::secrets::SecretScanner;
use crate::traits::{Embedder, SearchResult, VectorStore};
use crate::vault_ld::{CanvasDocument, CanvasNode, VAULT_LD_SHAPES_V1, VaultNote, YAML_LD_CONTEXT};

/// Configuration for a vault sync operation
#[derive(Debug, Clone)]
pub struct VaultSyncConfig {
    pub namespace: String,
    pub vault_path: PathBuf,
    pub output_dir: PathBuf,
    pub principal: Option<String>,
    /// Project root for the inner J6 caller-path check. Absent → the
    /// process cwd (the CLI default). The HTTP route injects its
    /// daemon startup root instead.
    pub project_root: Option<PathBuf>,
    /// Extra allowlist entries for the inner J6 check, in addition to
    /// the project root. Absent → none (CLI behavior).
    pub allowed_external_dirs: Option<Vec<PathBuf>>,
}

/// Result of vault export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultExportResult {
    pub success: bool,
    pub message: String,
    pub notes_exported: usize,
    pub topology_exported: bool,
    pub canvas_path: Option<String>,
    pub errors: Vec<String>,
}

/// Result of vault import
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultImportResult {
    pub success: bool,
    pub message: String,
    pub notes_imported: usize,
    pub topology_imported: bool,
    pub errors: Vec<String>,
}

/// Default resource caps for a single vault sync operation.
pub const MAX_VAULT_FILES: usize = 10_000;
pub const MAX_VAULT_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_VAULT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Resource caps for a vault import/export. The default entry points use the
/// published constants; tests tighten them to exercise fail-closed paths.
#[derive(Debug, Clone)]
pub struct VaultSyncLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for VaultSyncLimits {
    fn default() -> Self {
        Self {
            max_files: MAX_VAULT_FILES,
            max_file_bytes: MAX_VAULT_FILE_BYTES,
            max_total_bytes: MAX_VAULT_TOTAL_BYTES,
        }
    }
}

/// Canonicalize a path using J6 caller-path authority
pub fn canonicalize_vault_path(
    raw: &Path,
    project_root: &Path,
    allowed_external_dirs: &[PathBuf],
) -> Result<PathBuf, String> {
    // Single source of truth for J6: the same resolver and containment rule
    // the daemon uses. No uncanonicalized fallback for a non-existent target.
    crate::daemon::check_caller_path(raw, project_root, allowed_external_dirs)
}

/// Scan content for secrets
pub fn scan_secrets(content: &str, location: &str) -> Result<(), String> {
    if let Some(rejection) = SecretScanner::scan_str(content, location) {
        return Err(format!("Secret detected at {}: {}", location, rejection));
    }
    Ok(())
}

/// Validate Turtle against SHACL shapes using ontobug
pub fn validate_shacl(data_ttl: &str) -> Result<(), String> {
    match ontobug::validate::shacl_turtle(
        data_ttl,
        VAULT_LD_SHAPES_V1,
        &ontobug::validate::Mode::Native,
    ) {
        Ok(report) if report.conforms => Ok(()),
        Ok(report) => Err(format!(
            "SHACL validation error: {} violation(s): {}",
            report.violations(),
            report
                .results
                .iter()
                .filter(|r| r.severity == "Violation")
                .filter_map(|r| r.message.clone())
                .collect::<Vec<_>>()
                .join("; ")
        )),
        Err(e) => Err(format!("SHACL validation error: {}", e)),
    }
}

/// Escape a value for a double-quoted Turtle literal. Backslashes must be
/// replaced first so the escapes introduced below are not doubled.
pub fn escape_turtle_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Characters that cannot appear inside an IRIREF or a prefixed name without
/// letting untrusted frontmatter break out of the generated Turtle.
fn is_iri_unsafe_char(c: char) -> bool {
    matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\')
        || (c as u32) <= 0x20
        || c == '\u{7f}'
}

/// Strict Turtle `PNAME_LN`:
/// - `PN_PREFIX`: `[A-Za-z_][A-Za-z0-9_.-]*`, never ending in `.`
/// - `PN_LOCAL`: `[A-Za-z0-9_:][A-Za-z0-9_.:-]*`, never ending in `.`
fn is_strict_prefixed_name(s: &str) -> bool {
    fn valid_prefix(prefix: &str) -> bool {
        if prefix.ends_with('.') {
            return false;
        }
        let mut chars = prefix.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    }
    fn valid_local(local: &str) -> bool {
        if local.ends_with('.') {
            return false;
        }
        let mut chars = local.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == ':' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    }
    match s.split_once(':') {
        Some((prefix, local)) => valid_prefix(prefix) && valid_local(local),
        None => false,
    }
}

/// A fully-bracketed `<IRI>` whose interior carries no IRI-unsafe character.
fn is_safe_iri_literal(s: &str) -> bool {
    if s.len() < 3 || !s.starts_with('<') || !s.ends_with('>') {
        return false;
    }
    let inner = &s[1..s.len() - 1];
    !inner.is_empty() && !inner.chars().any(is_iri_unsafe_char)
}

/// Convert VaultNote to Turtle (RDF). Untrusted frontmatter can reach the
/// subject IRI and the class position verbatim, so both are validated here
/// and every literal value is escaped before interpolation.
pub fn note_to_turtle(note: &VaultNote) -> Result<String, String> {
    if note.id.is_empty() || note.id.chars().any(is_iri_unsafe_char) {
        return Err(format!("invalid id for Turtle subject IRI: {:?}", note.id));
    }
    // rdf_type is interpolated into `a yaml:VaultNote, {rdf_type}` — a raw
    // value there could mint arbitrary additional triples.
    if !is_strict_prefixed_name(&note.rdf_type) && !is_safe_iri_literal(&note.rdf_type) {
        return Err(format!(
            "invalid rdf_type for Turtle class position: {:?}",
            note.rdf_type
        ));
    }

    let mut turtle = String::new();

    turtle.push_str("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n");
    turtle.push_str("@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n");
    turtle.push_str("@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n");
    turtle.push_str(&format!("@prefix yaml: <{}> .\n", YAML_LD_CONTEXT));
    turtle.push_str("\n");

    // Memory ids are UUIDs/bare slugs, not absolute IRIs; wrap them in a
    // stable urn: so the Turtle parses. Preserve ids that are already IRIs.
    let subject = if note.id.contains(':') {
        note.id.clone()
    } else {
        format!("urn:vault:{}", note.id)
    };
    // Target the embedded VaultNote shape: an rdf_type alone (yaml:rule,
    // yaml:fact, ...) would not be targeted, making SHACL a no-op.
    turtle.push_str(&format!(
        "<{}> a yaml:VaultNote, {} ;\n",
        subject, note.rdf_type
    ));
    turtle.push_str(&format!(
        "  yaml:rdfType \"{}\" ;\n",
        escape_turtle_literal(&note.rdf_type)
    ));
    turtle.push_str(&format!(
        "  yaml:epistemicTier \"{}\" ;\n",
        escape_turtle_literal(&note.epistemic_tier)
    ));
    turtle.push_str(&format!(
        "  yaml:canonicalSha256 \"{}\" ;\n",
        escape_turtle_literal(&note.canonical_sha256)
    ));

    if !note.related.is_empty() {
        turtle.push_str("  yaml:related [\n");
        let last = note.related.len().saturating_sub(1);
        for (i, target) in note.related.iter().enumerate() {
            turtle.push_str("    a yaml:RelatedTarget ;\n");
            turtle.push_str(&format!(
                "    yaml:targetId \"{}\"",
                escape_turtle_literal(target)
            ));
            if i < last {
                turtle.push_str(" ;\n");
            } else {
                turtle.push_str("\n");
            }
        }
        turtle.push_str("  ] ;\n");
    }

    turtle.push_str(&format!(
        "  yaml:content \"{}\" .\n\n",
        escape_turtle_literal(&note.content)
    ));

    Ok(turtle)
}

/// Generate Canvas document from vault notes
pub fn generate_canvas(notes: &[VaultNote], _namespace: &str) -> CanvasDocument {
    // Deterministic by id: identical input sets always produce identical bytes.
    let mut ordered: Vec<&VaultNote> = notes.iter().collect();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));

    let known = ["rule", "fact", "procedure", "structural", "task", "symbol"];
    let mut groups: Vec<(&str, Vec<&VaultNote>)> = Vec::new();
    for tier in known {
        let group: Vec<&VaultNote> = ordered
            .iter()
            .copied()
            .filter(|n| n.epistemic_tier == tier)
            .collect();
        if !group.is_empty() {
            groups.push((tier, group));
        }
    }
    // Finding 8: custom tiers get a fallback column instead of vanishing.
    let custom: Vec<&VaultNote> = ordered
        .iter()
        .copied()
        .filter(|n| !known.contains(&n.epistemic_tier.as_str()))
        .collect();
    if !custom.is_empty() {
        groups.push(("custom", custom));
    }

    let mut nodes = Vec::new();
    let mut node_positions: std::collections::HashMap<String, (f64, f64)> =
        std::collections::HashMap::new();

    for (tier_index, (tier, group)) in groups.iter().enumerate() {
        let x = (tier_index as f64) * 400.0 + 100.0;
        for (i, note) in group.iter().enumerate() {
            let y = (i as f64) * 200.0 + 100.0;
            node_positions.insert(note.id.clone(), (x, y));
            let color = match *tier {
                "rule" => "#e74c3c",
                "fact" => "#3498db",
                "procedure" => "#9b59b6",
                "structural" => "#2ecc71",
                "task" => "#f39c12",
                _ => "#95a5a6",
            };
            nodes.push(CanvasNode {
                id: note.id.clone(),
                node_type: "text".to_string(),
                x,
                y,
                width: 200.0,
                height: 100.0,
                label: Some(note.id.clone()),
                color: Some(color.to_string()),
                file: None,
            });
        }
    }

    let mut edge_pairs: Vec<(String, String)> = Vec::new();
    for note in &ordered {
        if !node_positions.contains_key(&note.id) {
            continue;
        }
        for target in &note.related {
            if node_positions.contains_key(target) {
                edge_pairs.push((note.id.clone(), target.clone()));
            }
        }
    }
    edge_pairs.sort();
    edge_pairs.dedup();
    let edges = edge_pairs
        .into_iter()
        .enumerate()
        .map(|(i, (from, to))| crate::vault_ld::CanvasEdge {
            id: format!("edge_{}", i + 1),
            edge_type: "curved".to_string(),
            from_node: from,
            to_node: to,
            from_side: Some("right".to_string()),
            to_side: Some("left".to_string()),
        })
        .collect();

    CanvasDocument { nodes, edges }
}

/// Map an untrusted memory id to one flat Markdown filename.
///
/// Imported ids are attacker-controlled: the pre-existing `/` and `:` rewrite
/// leaves the Windows separator `\` intact, so `..\..\outside\victim` escapes
/// the J6-validated vault root when `join`ed on Windows (and UNC / rooted ids
/// replace the base outright). Neutralising every separator form keeps the
/// established flattened-naming behaviour on all platforms.
fn export_filename_for_id(id: &str) -> String {
    let mut stem = String::with_capacity(id.len() + 3);
    for ch in id.chars() {
        stem.push(match ch {
            '/' | '\\' | ':' => '_',
            _ => ch,
        });
    }
    stem.push_str(".md");
    stem
}

/// Issue #297: refuse to write through an existing symlink or non-regular
/// destination file inside the vault/output directory.
fn ensure_safe_export_destination(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_symlink() {
                Err(format!(
                    "Refusing to write through symlink destination {}",
                    path.display()
                ))
            } else if !ft.is_file() {
                Err(format!(
                    "Refusing to overwrite non-regular destination {}",
                    path.display()
                ))
            } else {
                Ok(())
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "Failed to inspect export destination {}: {}",
            path.display(),
            e
        )),
    }
}

/// Export vault contents to the configured path using the default caps.
pub async fn export_vault(
    config: VaultSyncConfig,
    store: Arc<dyn VectorStore>,
) -> Result<VaultExportResult, String> {
    export_vault_with_limits(config, store, VaultSyncLimits::default()).await
}

/// Export vault contents with explicit resource caps.
pub async fn export_vault_with_limits(
    config: VaultSyncConfig,
    store: Arc<dyn VectorStore>,
    limits: VaultSyncLimits,
) -> Result<VaultExportResult, String> {
    // Validate both paths with J6 and use the canonical results for ALL I/O.
    let project_root = config
        .project_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let allowed = config.allowed_external_dirs.clone().unwrap_or_default();
    let canonical_vault = canonicalize_vault_path(&config.vault_path, &project_root, &allowed)?;
    let canonical_output = canonicalize_vault_path(&config.output_dir, &project_root, &allowed)?;
    std::fs::create_dir_all(&canonical_vault)
        .map_err(|e| format!("Failed to create vault directory: {}", e))?;
    std::fs::create_dir_all(&canonical_output)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Query active memories from store
    let memories = store
        .list(&config.namespace, None, None)
        .await
        .map_err(|e| format!("Failed to list memories: {}", e))?;

    // Filter by ReBAC can_read_memory
    let principal = config.principal.as_deref().unwrap_or("default");
    let filtered: Vec<SearchResult> = memories
        .into_iter()
        .filter(|m| rebac::can_read_memory(principal, &m.payload.user_id, &m.payload.metadata))
        .collect();

    // Convert to VaultNotes
    let mut notes = Vec::new();
    for mem in filtered {
        match VaultNote::from_search_result(&mem) {
            Ok(note) => notes.push(note),
            Err(e) => {
                warn!("Failed to convert memory {}: {}", mem.id, e);
            }
        }
    }

    notes.sort_by(|a, b| a.id.cmp(&b.id));

    // Enforce caps on candidates before any file is written: a capped export
    // must leave the vault directory untouched.
    if notes.len() > limits.max_files {
        return Err(format!(
            "vault export file count exceeds maximum of {} files",
            limits.max_files
        ));
    }
    let mut export_total_bytes: u64 = 0;
    for note in &notes {
        let size = note.to_markdown().len() as u64;
        if size > limits.max_file_bytes {
            return Err(format!(
                "vault export file for id '{}' exceeds maximum of {} bytes",
                note.id, limits.max_file_bytes
            ));
        }
        export_total_bytes = export_total_bytes.saturating_add(size);
        if export_total_bytes > limits.max_total_bytes {
            return Err(format!(
                "vault export total bytes exceed maximum of {} bytes",
                limits.max_total_bytes
            ));
        }
    }

    // Write each note as markdown file. Flattened ids can collide (a/b,
    // a:b, a\b -> a_b.md); never overwrite an already-written note and
    // never inflate the exported count with a silently lost note.
    let mut exported = 0;
    let mut errors = Vec::new();
    let mut written_filenames: BTreeMap<String, String> = BTreeMap::new();

    for note in &notes {
        let filename = export_filename_for_id(&note.id);
        let filepath = canonical_vault.join(&filename);
        // Fail closed: the J6-validated vault root is the only legal parent.
        // A separator that somehow survived sanitisation must not be written.
        if filepath.parent() != Some(canonical_vault.as_path()) {
            errors.push(format!(
                "Refusing to export note {} outside vault root",
                note.id
            ));
            continue;
        }

        if let Some(existing_id) = written_filenames.get(&filename) {
            if existing_id != &note.id {
                errors.push(format!(
                    "export filename collision for id '{}': '{}' already written by id '{}'",
                    note.id, filename, existing_id
                ));
            }
            continue;
        }
        written_filenames.insert(filename, note.id.clone());

        let markdown = note.to_markdown();
        if let Err(e) = ensure_safe_export_destination(&filepath) {
            errors.push(format!("Export destination unsafe for {} failed: {}", filepath.display(), e));
            continue;
        }
        if let Err(e) = std::fs::write(&filepath, markdown) {
            errors.push(format!("Failed to write {}: {}", filepath.display(), e));
        } else {
            exported += 1;
        }
    }

    // Generate and write Canvas topology
    let canvas = generate_canvas(&notes, &config.namespace);
    let canvas_path = canonical_output.join("topology.canvas");

    let canvas_json = serde_json::to_string_pretty(&canvas)
        .map_err(|e| format!("Failed to serialize canvas: {}", e))?;

    ensure_safe_export_destination(&canvas_path)?;
    std::fs::write(&canvas_path, canvas_json)
        .map_err(|e| format!("Failed to write canvas: {}", e))?;

    Ok(VaultExportResult {
        success: errors.is_empty(),
        message: format!("Exported {} notes", exported),
        notes_exported: exported,
        topology_exported: true,
        canvas_path: Some(canvas_path.to_string_lossy().to_string()),
        errors,
    })
}

/// WikiLink targets with no corresponding note in the vault. BTreeSet gives
/// deterministic order for queryable stub minting.
pub fn unresolved_wiki_targets(notes: &[VaultNote]) -> Vec<String> {
    let known: std::collections::BTreeSet<&str> = notes.iter().map(|n| n.id.as_str()).collect();
    let mut missing = std::collections::BTreeSet::new();
    for note in notes {
        for target in &note.related {
            if !known.contains(target.as_str()) {
                missing.insert(target.clone());
            }
        }
    }
    missing.into_iter().collect()
}

/// Internal placeholder note for an unresolved WikiLink.
fn stub_note(target: &str) -> VaultNote {
    let content = format!("Stub node for unresolved WikiLink [[{target}]]");
    let mut note = VaultNote {
        id: target.to_string(),
        rdf_type: "yaml:structural".to_string(),
        context: YAML_LD_CONTEXT.to_string(),
        epistemic_tier: "structural".to_string(),
        canonical_sha256: String::new(),
        related: Vec::new(),
        content,
    };
    note.canonical_sha256 = note.fingerprint();
    note
}

/// Retire a prior row as a bitemporal history version. `upsert` is a
/// MERGE ... ON MATCH SET (in-place), so the old payload must be copied to
/// a distinct id with an expiry stamp BEFORE the new head is written.
fn retire_metadata(metadata: &mut serde_json::Value, new_id: &str, now: i64) {
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("valid_to".to_string(), serde_json::json!(now));
        obj.insert("superseded_by".to_string(), serde_json::json!(new_id));
        obj.insert(
            "archived_by_vault_import".to_string(),
            serde_json::json!(true),
        );
    }
}

/// A failed history rollback must be surfaced, never treated as success.
fn cleanup_orphan_error(history_id: &str, cleanup_err: &str) -> String {
    format!(
        "ORPHAN HISTORY: could not roll back history row {history_id} after a head write failure: {cleanup_err}"
    )
}

/// Import turn. The store's write lock guards a single upsert statement, not
/// a multi-note import; holding it across upserts would deadlock. A
/// process-level turn gives the whole read-validate-embed-write pipeline
/// single-writer discipline without nesting store locks.
static IMPORT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// §5 mesh sovereignty / privacy: decide whether an existing row may be
/// overwritten by a vault import. `can_write_memory(principal, principal)` is
/// insufficient — it is about the caller's right to write *new* memory, not
/// about ownership of an existing row. Mesh-provenance rows are sovereign
/// even against admin, and private/foreign rows are owner-only.
fn writable_prior(principal: &str, prior: &crate::traits::MemoryPayload) -> Result<(), String> {
    let owner = prior.user_id.as_str();

    if let Some(mesh) = prior.metadata.get("mesh").and_then(Value::as_object) {
        // A mesh row's author is an unverified sender assertion; record and
        // gate on it regardless. Only rows this principal wrote are ours.
        let author = mesh
            .get("created_by")
            .and_then(Value::as_str)
            .or_else(|| mesh.get("claimed_created_by").and_then(Value::as_str))
            .unwrap_or(owner);
        if author != principal {
            return Err(format!(
                "mesh-owned row authored by '{author}' is sovereign; \
                 principal '{principal}' may not overwrite it"
            ));
        }
    }

    if !owner.is_empty() && owner != principal {
        return Err(format!(
            "foreign-owned row (owner '{owner}') may not be overwritten by '{principal}'"
        ));
    }

    let eligibility = prior
        .metadata
        .get("eligibility")
        .and_then(Value::as_str)
        .unwrap_or("private");
    if eligibility == "private" && owner != principal {
        return Err(format!(
            "private row not owned by '{principal}' may not be overwritten"
        ));
    }

    Ok(())
}

/// Import vault contents into the database using the default caps.
pub async fn import_vault(
    config: VaultSyncConfig,
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn Embedder>,
) -> Result<VaultImportResult, String> {
    import_vault_with_limits(config, store, embedder, VaultSyncLimits::default()).await
}

/// Import vault contents with explicit resource caps.
pub async fn import_vault_with_limits(
    config: VaultSyncConfig,
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn Embedder>,
    limits: VaultSyncLimits,
) -> Result<VaultImportResult, String> {
    // Serialize the whole pipeline. The store's write lock guards one upsert
    // statement, not a multi-note import; holding it across upserts would
    // deadlock, so a process-level import turn gives the read-validate-embed-
    // write pipeline single-writer discipline without nesting store locks.
    let _turn = IMPORT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let project_root = config
        .project_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let allowed = config.allowed_external_dirs.clone().unwrap_or_default();
    let canonical_vault = canonicalize_vault_path(&config.vault_path, &project_root, &allowed)?;

    // ---- Phase 1: collect + pre-read (validation, no writes) ----
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut notes = Vec::new();
    let mut errors = Vec::new();

    // Count `.md` entries as they are collected. A cap violation is a typed
    // error returned before any note is read or upserted.
    for entry in std::fs::read_dir(&canonical_vault)
        .map_err(|e| format!("Failed to read vault directory: {}", e))?
    {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                errors.push(format!("Failed to read vault entry: {}", e));
                continue;
            }
        };
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if paths.len() >= limits.max_files {
            return Err(format!(
                "vault file count exceeds maximum of {} files",
                limits.max_files
            ));
        }
        paths.push(path);
    }
    paths.sort();

    // Read every candidate in deterministic order and enforce byte caps
    // before the upsert loop. Per-file failures are collected and skipped;
    // a global total-byte violation is a typed cap error with zero writes.
    let mut preloaded: Vec<(PathBuf, String)> = Vec::new();
    let mut total_bytes: u64 = 0;
    for path in &paths {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) => {
                errors.push(format!(
                    "Skipping unreadable vault file {}: {}",
                    path.display(),
                    e
                ));
                continue;
            }
        };
        if metadata.len() > limits.max_file_bytes {
            errors.push(format!(
                "Skipping oversized vault file {}: {} bytes exceeds maximum of {} bytes",
                path.display(),
                metadata.len(),
                limits.max_file_bytes
            ));
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => {
                errors.push(format!(
                    "Skipping unreadable vault file {}: {}",
                    path.display(),
                    e
                ));
                continue;
            }
        };
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > limits.max_total_bytes {
            return Err(format!(
                "vault total bytes exceed maximum of {} bytes",
                limits.max_total_bytes
            ));
        }
        preloaded.push((path.clone(), content));
    }

    for (path, content) in preloaded {
        if let Err(e) = scan_secrets(&content, &path.to_string_lossy()) {
            errors.push(e);
            continue;
        }

        let note = match VaultNote::from_markdown(&content) {
            Ok(mut note) => {
                // Frontmatter hash is informational and may be stale. Derive
                // bitemporal identity from content, tier and edges (finding 3).
                note.canonical_sha256 = note.fingerprint();
                note
            }
            Err(e) => {
                errors.push(format!("Failed to parse {}: {}", path.display(), e));
                continue;
            }
        };

        let turtle = match note_to_turtle(&note) {
            Ok(turtle) => turtle,
            Err(e) => {
                errors.push(format!("Invalid frontmatter in {}: {}", path.display(), e));
                continue;
            }
        };
        if let Err(e) = validate_shacl(&turtle) {
            errors.push(format!(
                "SHACL validation failed for {}: {}",
                path.display(),
                e
            ));
            continue;
        }

        notes.push(note);
    }

    // Mint queryable stubs for unresolved WikiLinks; each is gated again.
    let mut stub_ids = std::collections::BTreeSet::new();
    for target in unresolved_wiki_targets(&notes) {
        let stub = stub_note(&target);
        let turtle = match note_to_turtle(&stub) {
            Ok(turtle) => turtle,
            Err(e) => {
                errors.push(format!("Invalid stub id {}: {}", target, e));
                continue;
            }
        };
        if let Err(e) = validate_shacl(&turtle) {
            errors.push(format!(
                "SHACL validation failed for stub {}: {}",
                target, e
            ));
            continue;
        }
        stub_ids.insert(target);
        notes.push(stub);
    }
    notes.sort_by(|a, b| a.id.cmp(&b.id));

    let principal = config.principal.as_deref().unwrap_or("default");
    let now = chrono::Utc::now().timestamp();

    struct Planned {
        id: String,
        vector: Vec<f32>,
        metadata: serde_json::Value,
        content: String,
        memory_type: String,
        history: Option<(String, Vec<f32>, crate::traits::MemoryPayload)>,
    }

    let mut planned: Vec<Planned> = Vec::new();
    for note in notes {
        let is_stub = stub_ids.contains(&note.id);
        let gate_metadata = serde_json::json!({
            "user_id": principal,
            "epistemic_tier": note.epistemic_tier,
            "stub": is_stub,
        });
        if !rebac::can_write_memory(principal, principal, &gate_metadata) {
            errors.push(format!("ReBAC write denied for note {}", note.id));
            continue;
        }

        let (prior_vector, prior_payload) = match store.get(&config.namespace, &note.id).await {
            Ok(Some(pair)) => (Some(pair.0), Some(pair.1)),
            Ok(None) => (None, None),
            Err(e) => {
                errors.push(format!("Failed to read prior head {}: {}", note.id, e));
                continue;
            }
        };

        if let Some(ref prior) = prior_payload {
            // Byte-identical re-import is a no-op before any sovereignty
            // question: a protected row we do not touch is not overwritten.
            // The hash always comes from parsed content/edges, never frontmatter.
            let prior_hash = prior
                .metadata
                .get("canonical_sha256")
                .and_then(|v| v.as_str());
            if prior_hash == Some(note.canonical_sha256.as_str()) {
                continue;
            }
            if let Err(e) = writable_prior(principal, prior) {
                errors.push(format!("Refusing to overwrite {}: {}", note.id, e));
                continue;
            }
        }

        // Embed before staging any write: an embed failure must never leave
        // a retired history row behind.
        let vector = match embedder.embed(&note.content).await {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("Failed to embed {}: {}", note.id, e));
                continue;
            }
        };

        let mut metadata = serde_json::json!({
            "vault_source": true,
            "stub": is_stub,
            "canonical_sha256": note.canonical_sha256,
            "related": note.related,
            "valid_from": now,
        });

        let mut history = None;
        if let (Some(hv), Some(mut prior)) = (prior_vector, prior_payload) {
            if !prior.metadata.is_object() {
                errors.push(format!(
                    "Prior head {} has non-object metadata; refusing bitemporal import",
                    note.id
                ));
                continue;
            }
            if let Some(prior_obj) = prior.metadata.as_object() {
                if let Some(visible_to) = prior_obj.get("visible_to") {
                    metadata["visible_to"] = visible_to.clone();
                }
                if let Some(eligibility) = prior_obj.get("eligibility") {
                    metadata["eligibility"] = eligibility.clone();
                }
            }
            let history_id = format!("{}.vault-history.{}", note.id, uuid::Uuid::new_v4());
            retire_metadata(&mut prior.metadata, &note.id, now);
            metadata["supersedes"] = serde_json::json!(history_id);
            history = Some((history_id, hv, prior));
        }

        planned.push(Planned {
            id: note.id,
            vector,
            metadata,
            content: note.content,
            memory_type: note
                .rdf_type
                .strip_prefix("yaml:")
                .unwrap_or("symbol")
                .to_string(),
            history,
        });
    }

    // ---- Phase 2: writes ----
    let mut imported = 0;
    for item in planned {
        let cleanup_id = item.history.as_ref().map(|(id, _, _)| id.clone());
        if let Some((history_id, history_vector, history_payload)) = item.history {
            if let Err(e) = store
                .upsert(
                    &config.namespace,
                    &history_id,
                    history_vector,
                    history_payload,
                )
                .await
            {
                errors.push(format!("Failed to preserve history for {}: {}", item.id, e));
                continue;
            }
        }

        let payload = crate::traits::MemoryPayload {
            content: item.content,
            memory_type: item.memory_type,
            location: canonical_vault.to_string_lossy().to_string(),
            user_id: principal.to_string(),
            agent_name: Some("NeuroStrata-VaultLD".to_string()),
            location_lines: "".to_string(),
            metadata: item.metadata,
        };

        match store
            .upsert(&config.namespace, &item.id, item.vector, payload)
            .await
        {
            Ok(_) => imported += 1,
            Err(e) => {
                // Never leave a head-without-history outcome: roll back the
                // history row staged for this note.
                if let Some(history_id) = cleanup_id {
                    if let Err(cleanup_err) = store.delete(&config.namespace, &history_id).await {
                        let msg = cleanup_orphan_error(&history_id, &cleanup_err.to_string());
                        warn!("{}", msg);
                        errors.push(msg);
                    }
                }
                errors.push(format!("Failed to upsert {}: {}", item.id, e));
            }
        }
    }

    Ok(VaultImportResult {
        success: errors.is_empty(),
        message: format!("Imported {} notes", imported),
        notes_imported: imported,
        topology_imported: false,
        errors,
    })
}

/// Shared handler for MCP tool and CLI (CLI + server share same impl)
pub async fn handle_vault_sync(
    args: &Value,
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn Embedder>,
    project_root: Option<PathBuf>,
    allowed_external_dirs: Option<Vec<PathBuf>>,
) -> String {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("export");

    let namespace = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("NeuroStrata");

    let vault_path = args
        .get("vault_path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let output_dir = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let principal = args.get("principal").and_then(|v| v.as_str());

    let config = VaultSyncConfig {
        namespace: namespace.to_string(),
        vault_path: PathBuf::from(vault_path),
        output_dir: PathBuf::from(output_dir),
        principal: principal.map(|s| s.to_string()),
        project_root,
        allowed_external_dirs,
    };

    let result =
        match action {
            "export" => match export_vault(config, store).await {
                Ok(r) => serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!("Serialization error: {}", e)),
                Err(e) => serde_json::to_string(&VaultExportResult {
                    success: false,
                    message: e,
                    notes_exported: 0,
                    topology_exported: false,
                    canvas_path: None,
                    errors: Vec::new(),
                })
                .unwrap(),
            },
            "import" => match import_vault(config, store, embedder).await {
                Ok(r) => serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!("Serialization error: {}", e)),
                Err(e) => serde_json::to_string(&VaultImportResult {
                    success: false,
                    message: e,
                    notes_imported: 0,
                    topology_imported: false,
                    errors: Vec::new(),
                })
                .unwrap(),
            },
            _ => serde_json::to_string(&VaultExportResult {
                success: false,
                message: format!("Unknown action: {}", action),
                notes_exported: 0,
                topology_exported: false,
                canvas_path: None,
                errors: Vec::new(),
            })
            .unwrap(),
        };

    result
}
#[cfg(test)]
mod tests {
    use super::*;

    struct TestEmbedder;

    #[async_trait::async_trait]
    impl Embedder for TestEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 4];
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += b as f32;
            }
            Ok(v)
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    fn note(id: &str, related: &[&str]) -> VaultNote {
        VaultNote {
            id: id.to_string(),
            rdf_type: "yaml:fact".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "fact".to_string(),
            canonical_sha256: hex::encode(Sha256::digest(id.as_bytes())),
            related: related.iter().map(|s| s.to_string()).collect(),
            content: String::new(),
        }
    }

    #[test]
    fn unresolved_wiki_targets_are_deduplicated_and_deterministic() {
        let notes = vec![
            note("a", &["b", "missing-1", "missing-2"]),
            note("b", &["missing-1", "a"]),
        ];
        assert_eq!(
            unresolved_wiki_targets(&notes),
            vec!["missing-1", "missing-2"]
        );
    }

    #[test]
    fn stub_note_carries_unresolved_target_and_structural_tier() {
        let stub = stub_note("missing-1");
        assert_eq!(stub.id, "missing-1");
        assert_eq!(stub.epistemic_tier, "structural");
        assert!(stub.content.contains("missing-1"));
        assert!(stub.related.is_empty());
        assert_eq!(stub.canonical_sha256.len(), 64);
    }

    #[test]
    fn retired_metadata_marks_prior_head_as_history() {
        let mut meta = serde_json::json!({"vault_source": true, "valid_to": 0});
        retire_metadata(&mut meta, "replacement-id", 1234);
        assert_eq!(meta["valid_to"], serde_json::json!(1234));
        assert_eq!(meta["superseded_by"], serde_json::json!("replacement-id"));
        assert_eq!(meta["archived_by_vault_import"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn import_mints_stubs_and_preserves_prior_head() {
        let root = std::env::temp_dir().join(format!("ns-vault-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_test";
        store.init(ns).await.expect("init namespace");
        let embedder: std::sync::Arc<dyn Embedder> = std::sync::Arc::new(TestEmbedder);

        std::fs::write(
            vault.join("note-1.md"),
            note("note-1", &["missing-target"]).to_markdown(),
        )
        .expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };

        let first = import_vault(config.clone(), store.clone(), embedder.clone())
            .await
            .expect("first import");
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.notes_imported, 2, "note + stub");
        let (_, stub) = store.get(ns, "missing-target").await.unwrap().unwrap();
        assert_eq!(stub.metadata["stub"], serde_json::json!(true));
        assert_eq!(stub.memory_type, "structural");

        let mut changed = note("note-1", &["missing-target"]);
        changed.content = "Changed body".to_string();
        changed.canonical_sha256 = hex::encode(Sha256::digest(changed.content.as_bytes()));
        std::fs::write(vault.join("note-1.md"), changed.to_markdown()).expect("write changed note");
        let second = import_vault(config.clone(), store.clone(), embedder.clone())
            .await
            .expect("second import");
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        let (_, head) = store.get(ns, "note-1").await.unwrap().unwrap();
        let history_id = head.metadata["supersedes"].as_str().expect("supersedes");
        let (_, history) = store.get(ns, history_id).await.unwrap().unwrap();
        assert_eq!(
            history.metadata["superseded_by"],
            serde_json::json!("note-1")
        );
        assert!(history.metadata["valid_to"].is_number());

        let third = import_vault(config, store.clone(), embedder)
            .await
            .expect("third import");
        assert_eq!(third.notes_imported, 0, "unchanged re-import is a no-op");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding 6: no uncanonicalized fallback for a non-existent target whose
    /// remainder spells `..`. Mirror daemon::check_caller_path exactly.
    #[test]
    fn canonicalize_rejects_dotdot_in_nonexistent_remainder() {
        let root = std::env::temp_dir().join(format!("ns-j6-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("vault")).expect("create root");
        // The spelling climbs out of the root: Windows resolves `missing\..`
        // lexically, so there the containment check refuses it instead.
        let escaped = root.join("vault/missing/../../../escape");
        assert!(
            canonicalize_vault_path(&escaped, &root, &[]).is_err(),
            "`..` in a non-existent remainder must be refused, not joined"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding 6: a symlink that escapes the authority root must be refused.
    #[cfg(unix)]
    #[test]
    fn canonicalize_rejects_symlink_escape() {
        let root = std::env::temp_dir().join(format!("ns-j6-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("ns-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::os::unix::fs::symlink(&outside, root.join("escape")).expect("symlink");
        assert!(
            canonicalize_vault_path(&root.join("escape"), &root, &[]).is_err(),
            "symlink escape must be refused"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Finding 5: shuffled source order must not change exported bytes.
    #[test]
    fn generate_canvas_is_byte_identical_under_shuffled_input() {
        let a = note("a", &["b"]);
        let b = note("b", &[]);
        let c = note("c", &["a", "b"]);
        let first =
            serde_json::to_string(&generate_canvas(&[a.clone(), b.clone(), c.clone()], "ns"))
                .unwrap();
        let second = serde_json::to_string(&generate_canvas(&[c, a, b], "ns")).unwrap();
        assert_eq!(first, second);
    }

    /// Finding 8: custom epistemic tiers must get a fallback column, not vanish.
    #[test]
    fn generate_canvas_retains_unknown_tier() {
        let mut custom = note("custom", &[]);
        custom.epistemic_tier = "heuristic".to_string();
        let canvas = generate_canvas(&[custom], "ns");
        assert!(canvas.nodes.iter().any(|n| n.id == "custom"));
    }

    /// Finding 3: a related-only edit is a new version, not a silent no-op.
    #[tokio::test]
    async fn related_only_edit_creates_history_and_updates_edges() {
        let root = std::env::temp_dir().join(format!("ns-rel-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_rel";
        store.init(ns).await.expect("init");
        let embedder: std::sync::Arc<dyn Embedder> = std::sync::Arc::new(TestEmbedder);
        let mut original = note("edge-note", &["first"]);
        original.content = "Same body".to_string();
        std::fs::write(vault.join("edge-note.md"), original.to_markdown()).unwrap();
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        import_vault(config.clone(), store.clone(), embedder.clone())
            .await
            .unwrap();

        let mut edited = original.clone();
        edited.related = vec!["second".to_string()];
        std::fs::write(vault.join("edge-note.md"), edited.to_markdown()).unwrap();
        let result = import_vault(config, store.clone(), embedder).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let (_, head) = store.get(ns, "edge-note").await.unwrap().unwrap();
        assert_eq!(head.metadata["related"], serde_json::json!(["second"]));
        let history_id = head.metadata["supersedes"].as_str().expect("supersedes");
        let (_, history) = store.get(ns, history_id).await.unwrap().unwrap();
        assert_eq!(history.metadata["related"], serde_json::json!(["first"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn seed_payload(user_id: &str, metadata: serde_json::Value) -> crate::traits::MemoryPayload {
        crate::traits::MemoryPayload {
            content: "seeded".to_string(),
            user_id: user_id.to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        }
    }

    /// Finding 1: a mesh-provenance row authored by another peer is
    /// sovereign — even an admin principal may not overwrite it via import.
    #[tokio::test]
    async fn import_refuses_mesh_owned_row_not_written_by_principal() {
        let root = std::env::temp_dir().join(format!("ns-mesh-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_mesh";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                "mesh-note",
                vec![0.0; 4],
                seed_payload(
                    "peer-a",
                    serde_json::json!({
                        "mesh": {
                            "origin": "mesh-import",
                            "claimed_created_by": "peer-a",
                            "created_by_verified": false
                        }
                    }),
                ),
            )
            .await
            .expect("seed mesh row");

        std::fs::write(
            vault.join("mesh-note.md"),
            note("mesh-note", &[]).to_markdown(),
        )
        .expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: Some("me".to_string()),
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(
            result.errors.iter().any(|e| e.contains("mesh")),
            "mesh sovereignty must refuse overwrite: {:?}",
            result.errors
        );
        let (_, after) = store.get(ns, "mesh-note").await.unwrap().unwrap();
        assert_eq!(after.user_id, "peer-a", "mesh row must be unchanged");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding 1: a private row owned by someone else is foreign and must
    /// not be overwritten by an unrelated principal.
    #[tokio::test]
    async fn import_refuses_private_row_not_owned_by_principal() {
        let root = std::env::temp_dir().join(format!("ns-priv-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_priv";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                "priv-note",
                vec![0.0; 4],
                seed_payload("other", serde_json::json!({"eligibility": "private"})),
            )
            .await
            .expect("seed private row");
        std::fs::write(
            vault.join("priv-note.md"),
            note("priv-note", &[]).to_markdown(),
        )
        .expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: Some("me".to_string()),
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("private") || e.contains("foreign")),
            "foreign private row must be refused: {:?}",
            result.errors
        );
        let (_, after) = store.get(ns, "priv-note").await.unwrap().unwrap();
        assert_eq!(after.user_id, "other", "private row must be unchanged");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding 4/2: the shared handler must validate the output_dir against
    /// the same caller-path authority and create a safe nested path.
    #[tokio::test]
    async fn handler_creates_nested_output_dir_and_writes_canvas() {
        let root = std::env::temp_dir().join(format!("ns-out-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        store.init("vault_out").await.expect("init");
        let out = root.join("nested/deep/out");
        let args = serde_json::json!({
            "action": "export",
            "namespace": "vault_out",
            "vault_path": vault.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
        });
        let res = handle_vault_sync(
            &args,
            store,
            std::sync::Arc::new(TestEmbedder),
            Some(root.clone()),
            None,
        )
        .await;
        let result: VaultExportResult = serde_json::from_str(&res).expect("valid result");
        assert!(
            result.success,
            "nested output must be created: {:?}",
            result
        );
        assert!(out.join("topology.canvas").exists(), "canvas written");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding 4/2: an output_dir outside the authority root is refused by
    /// the shared handler even when the directory already exists on disk.
    #[tokio::test]
    async fn handler_rejects_output_dir_escape() {
        let root = std::env::temp_dir().join(format!("ns-out-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let outside = std::env::temp_dir().join(format!("ns-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).expect("create outside");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        store.init("vault_escape").await.expect("init");
        let args = serde_json::json!({
            "action": "export",
            "namespace": "vault_escape",
            "vault_path": vault.to_string_lossy(),
            "output_dir": outside.to_string_lossy(),
        });
        let res = handle_vault_sync(
            &args,
            store,
            std::sync::Arc::new(TestEmbedder),
            Some(root.clone()),
            None,
        )
        .await;
        let result: VaultExportResult = serde_json::from_str(&res).expect("valid result");
        assert!(!result.success, "escape must be refused: {:?}", result);
        assert!(
            !outside.join("topology.canvas").exists(),
            "no canvas outside authority"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    struct FailingEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("injected embed failure")
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    /// Finding 7: preflight all embeddings before any write. An embed
    /// failure must leave no head and no orphan history row.
    #[tokio::test]
    async fn embed_failure_writes_nothing() {
        let root = std::env::temp_dir().join(format!("ns-emb-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_emb";
        store.init(ns).await.expect("init");
        // Seed a prior head so a naive implementation would retire it into a
        // history row BEFORE discovering the embed failure.
        store
            .upsert(
                ns,
                "a",
                vec![0.0; 4],
                seed_payload(
                    "default",
                    serde_json::json!({"vault_source": true, "canonical_sha256": "old-hash"}),
                ),
            )
            .await
            .expect("seed head");
        let mut changed = note("a", &[]);
        changed.content = "changed body".to_string();
        std::fs::write(vault.join("a.md"), changed.to_markdown()).expect("write a");
        std::fs::write(vault.join("b.md"), note("b", &[]).to_markdown()).expect("write b");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = import_vault(config, store.clone(), std::sync::Arc::new(FailingEmbedder))
            .await
            .expect("import");
        assert_eq!(result.notes_imported, 0);
        assert!(store.get(ns, "b").await.unwrap().is_none());
        let all = store.list(ns, None, None).await.expect("list");
        assert_eq!(
            all.len(),
            1,
            "only the seeded head remains; no orphan history row"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding 7: concurrent imports of one note serialize on the import
    /// lock; the head always carries a real history row, never orphaned.
    #[tokio::test]
    async fn concurrent_same_note_import_has_no_head_without_history() {
        let root = std::env::temp_dir().join(format!("ns-conc-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_conc";
        store.init(ns).await.expect("init");
        // Two vaults holding the same note id with different content: the
        // first import writes a head, the second must retire it to history.
        let vault_b = root.join("vault-b");
        std::fs::create_dir_all(&vault_b).expect("create vault-b");
        let mut note_a = note("conc-note", &[]);
        note_a.content = "content A".to_string();
        let mut note_b = note("conc-note", &[]);
        note_b.content = "content B".to_string();
        std::fs::write(vault.join("c.md"), note_a.to_markdown()).expect("write a");
        std::fs::write(vault_b.join("c.md"), note_b.to_markdown()).expect("write b");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let (s1, s2, e1, e2) = (
            store.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
            std::sync::Arc::new(TestEmbedder),
        );
        let mut c1 = config.clone();
        c1.vault_path = vault;
        let mut c2 = config;
        c2.vault_path = vault_b;
        let t1 = tokio::spawn(async move { import_vault(c1, s1, e1).await });
        let t2 = tokio::spawn(async move { import_vault(c2, s2, e2).await });
        let (r1, r2) = (t1.await.expect("join"), t2.await.expect("join"));
        assert!(r1.is_ok() && r2.is_ok(), "both imports succeed");
        let (_, head) = store.get(ns, "conc-note").await.unwrap().unwrap();
        let history_id = head.metadata["supersedes"]
            .as_str()
            .expect("head must supersede a real history row");
        assert!(
            store.get(ns, history_id).await.unwrap().is_some(),
            "history row exists"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Closure finding 1: an export-derived note whose content carries
    /// surrounding whitespace must re-import as a byte-identical no-op.
    #[tokio::test]
    async fn surrounding_whitespace_does_not_create_new_version() {
        let root = std::env::temp_dir().join(format!("ns-ws-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_ws";
        store.init(ns).await.expect("init");
        let payload = crate::traits::MemoryPayload {
            content: "  body with spaces  ".to_string(),
            user_id: "default".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        let result = crate::traits::SearchResult {
            id: "ws-note".to_string(),
            score: 0.0,
            payload,
            evidence: None,
        };
        let note = VaultNote::from_search_result(&result).expect("note");
        store
            .upsert(
                ns,
                "ws-note",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: note.content.clone(),
                    user_id: "default".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "vault_source": true,
                        "related": note.related,
                        "canonical_sha256": note.fingerprint()
                    }),
                },
            )
            .await
            .expect("seed head");
        std::fs::write(vault.join("ws-note.md"), note.to_markdown()).expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let out = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert_eq!(
            out.notes_imported, 0,
            "whitespace-only difference is a no-op"
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(store.list(ns, None, None).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Closure finding 2: a byte-identical protected row is a no-op before
    /// any sovereignty gate, so it skips without error or write.
    #[tokio::test]
    async fn protected_row_identical_content_is_noop_not_error() {
        let root = std::env::temp_dir().join(format!("ns-prot-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_prot";
        store.init(ns).await.expect("init");
        let mut note = note("protected", &[]);
        note.content = "same body".to_string();
        store
            .upsert(
                ns,
                "protected",
                vec![0.0; 4],
                seed_payload(
                    "peer-a",
                    serde_json::json!({
                        "mesh": {"origin": "mesh-import", "claimed_created_by": "peer-a"},
                        "canonical_sha256": note.fingerprint()
                    }),
                ),
            )
            .await
            .expect("seed protected row");
        std::fs::write(vault.join("protected.md"), note.to_markdown()).expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: Some("me".to_string()),
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let out = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(
            out.errors.is_empty(),
            "identical protected row must skip silently: {:?}",
            out.errors
        );
        assert_eq!(out.notes_imported, 0);
        assert_eq!(store.list(ns, None, None).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Closure finding 2: changed protected content is still refused with
    /// zero writes.
    #[tokio::test]
    async fn protected_row_changed_content_still_refused_with_zero_writes() {
        let root = std::env::temp_dir().join(format!("ns-prot2-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_prot2";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                "protected",
                vec![0.0; 4],
                seed_payload(
                    "peer-a",
                    serde_json::json!({
                        "mesh": {"origin": "mesh-import", "claimed_created_by": "peer-a"},
                        "canonical_sha256": "old-hash"
                    }),
                ),
            )
            .await
            .expect("seed protected row");
        let mut changed = note("protected", &[]);
        changed.content = "different body".to_string();
        std::fs::write(vault.join("protected.md"), changed.to_markdown()).expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: Some("me".to_string()),
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let out = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(out.errors.iter().any(|e| e.contains("mesh")));
        assert_eq!(out.notes_imported, 0);
        assert_eq!(store.list(ns, None, None).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Closure finding 4: handler-level secret rejection, with zero writes.
    #[tokio::test]
    async fn handler_import_rejects_secret_with_zero_writes() {
        let root = std::env::temp_dir().join(format!("ns-sec-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_secret";
        store.init(ns).await.expect("init");
        let markdown = format!(
            "---\n@context: \"{}\"\n@id: \"secret-note\"\n@type: \"yaml:fact\"\n\
             epistemic_tier: fact\ncanonical_sha256: \"{}\"\nrelated: []\n---\n\n\
             api_key = \"sk-ant-api03-abcdefghijklmnopqrstuvwxyz123456\"\n",
            YAML_LD_CONTEXT,
            "a".repeat(64)
        );
        std::fs::write(vault.join("secret.md"), markdown).expect("write note");
        let args = serde_json::json!({
            "action": "import",
            "namespace": ns,
            "vault_path": vault.to_string_lossy(),
            "output_dir": vault.to_string_lossy(),
            "principal": "me",
        });
        let res = handle_vault_sync(
            &args,
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
            Some(root.clone()),
            None,
        )
        .await;
        let out: VaultImportResult = serde_json::from_str(&res).expect("valid result");
        assert!(!out.success);
        assert!(
            out.errors
                .iter()
                .any(|e| e.to_lowercase().contains("secret"))
        );
        assert!(store.list(ns, None, None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #288: every Turtle string literal escapes the five characters
    /// that can terminate or distort a quoted literal.
    #[test]
    fn escape_turtle_literal_escapes_backslash_quote_and_controls() {
        assert_eq!(
            escape_turtle_literal("a\\b\"c\nd\re\tf"),
            "a\\\\b\\\"c\\nd\\re\\tf"
        );
        assert_eq!(escape_turtle_literal("plain"), "plain");
    }

    /// Issue #288: a quote in epistemic_tier is escaped into a single valid
    /// Turtle literal; rejection comes from the SHACL value constraint, not
    /// from a broken Turtle document or an injected extra triple.
    #[test]
    fn note_to_turtle_escapes_epistemic_tier_and_shacl_rejects_bad_tier() {
        let mut n = note("tier-note", &[]);
        n.epistemic_tier = "bad\" tier".to_string();
        let turtle = note_to_turtle(&n).expect("quote must be escaped, not fatal to rendering");
        assert!(
            turtle.contains("yaml:epistemicTier \"bad\\\" tier\""),
            "{}",
            turtle
        );
        assert_eq!(turtle.matches("yaml:epistemicTier").count(), 1);
        let err = validate_shacl(&turtle).expect_err("SHACL value constraint must reject tier");
        assert!(err.contains("SHACL"), "{}", err);
        assert!(err.contains("epistemicTier"), "{}", err);
    }

    /// Issue #288: hostile ids are rejected before they can form a Turtle IRI.
    #[test]
    fn note_to_turtle_rejects_iri_unsafe_and_empty_id() {
        for bad_id in ["evil>id", "", "bad\nid"] {
            let mut n = note(bad_id, &[]);
            n.canonical_sha256 = hex::encode(Sha256::digest(bad_id.as_bytes()));
            let err = note_to_turtle(&n).expect_err("unsafe id must fail closed");
            assert!(err.to_lowercase().contains("id"), "{}", err);
        }
    }

    /// Issue #288: a newline/quote in canonical_sha256 is escaped into one
    /// literal; the SHACL pattern constraint rejects it cleanly.
    #[test]
    fn note_to_turtle_escapes_canonical_sha256_and_shacl_rejects_bad_hash() {
        let mut n = note("hash-note", &[]);
        n.canonical_sha256 = "bad\"\nsha".to_string();
        let turtle = note_to_turtle(&n).expect("turtle renders");
        assert!(
            turtle.contains("yaml:canonicalSha256 \"bad\\\"\\nsha\""),
            "{}",
            turtle
        );
        assert_eq!(turtle.matches("yaml:canonicalSha256").count(), 1);
        let err = validate_shacl(&turtle).expect_err("SHACL pattern must reject hash");
        assert!(err.contains("canonicalSha256"), "{}", err);
    }

    /// Issue #288: a hostile related target is escaped inside its literal so
    /// no extra triples are injected and the graph matches the stored note.
    #[test]
    fn note_to_turtle_escapes_hostile_related_target_without_injection() {
        let target = "evil\" wikilink\n<evil> a yaml:VaultNote";
        let mut n = note("rel-note", &[target]);
        n.canonical_sha256 = hex::encode(Sha256::digest(n.id.as_bytes()));
        let turtle = note_to_turtle(&n).expect("turtle renders");
        assert!(
            turtle.contains("yaml:targetId \"evil\\\" wikilink\\n<evil> a yaml:VaultNote\""),
            "{}",
            turtle
        );
        assert_eq!(
            turtle.matches("yaml:targetId").count(),
            1,
            "no injected triples"
        );
        assert_eq!(turtle.matches(" a yaml:VaultNote,").count(), 1);
        assert!(validate_shacl(&turtle).is_ok(), "{}", turtle);
    }

    /// Issue #288: minted link stubs carry an untrusted target as their id;
    /// an unsafe target must fail closed at Turtle generation.
    #[test]
    fn stub_note_with_unsafe_target_id_fails_closed() {
        for bad_target in ["evil\"target", "target\nwith-newline"] {
            let stub = stub_note(bad_target);
            let err = note_to_turtle(&stub).expect_err("unsafe stub id must fail closed");
            assert!(err.to_lowercase().contains("id"), "{}", err);
        }
    }

    /// Issue #288: a hostile rdf_type cannot inject arbitrary Turtle syntax
    /// through the `a yaml:VaultNote, ...` position.
    #[test]
    fn note_to_turtle_rejects_unsafe_rdf_type() {
        let mut n = note("type-note", &[]);
        n.rdf_type = "yaml:fact> <evil".to_string();
        let err = note_to_turtle(&n).expect_err("unsafe rdf_type must fail closed");
        assert!(err.to_lowercase().contains("rdf_type"), "{}", err);
        let mut bracket = note("bracket-note", &[]);
        bracket.rdf_type = "yaml:fact] a <evil".to_string();
        assert!(note_to_turtle(&bracket).is_err());
    }

    /// Closure finding 4: handler-level SHACL rejection, with zero writes.
    #[tokio::test]
    async fn handler_import_rejects_shacl_invalid_note_with_zero_writes() {
        let root = std::env::temp_dir().join(format!("ns-shacl-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_shacl";
        store.init(ns).await.expect("init");
        // Issue #288: a double quote inside epistemic_tier is escaped into a
        // valid Turtle literal. Rejection must come from the SHACL value
        // constraint on the tier, never from Turtle injection or a parse error.
        let markdown = format!(
            "---\n@context: \"{}\"\n@id: \"shacl-note\"\n@type: \"yaml:fact\"\n\
             epistemic_tier: 'bad\" tier'\ncanonical_sha256: \"{}\"\nrelated: []\n---\n\n\
             body\n",
            YAML_LD_CONTEXT,
            "a".repeat(64)
        );
        std::fs::write(vault.join("shacl.md"), markdown).expect("write note");
        let args = serde_json::json!({
            "action": "import",
            "namespace": ns,
            "vault_path": vault.to_string_lossy(),
            "output_dir": vault.to_string_lossy(),
            "principal": "me",
        });
        let res = handle_vault_sync(
            &args,
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
            Some(root.clone()),
            None,
        )
        .await;
        let out: VaultImportResult = serde_json::from_str(&res).expect("valid result");
        assert!(!out.success);
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains("SHACL") && e.contains("epistemicTier")),
            "SHACL tier value constraint must fire: {:?}",
            out.errors
        );
        assert!(store.list(ns, None, None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Closure finding 6: a failed history rollback must be reported, never
    /// silently discarded as "rolled back".
    #[test]
    fn cleanup_failure_surfaces_orphan_history_error() {
        let msg = cleanup_orphan_error("history-1", "disk read-only");
        assert!(msg.contains("ORPHAN HISTORY"));
        assert!(msg.contains("history-1"));
        assert!(msg.contains("disk read-only"));
    }

    /// Codex 4610878281 P1: separator-bearing ids flatten to one safe name.
    /// Covers both slash styles, nested traversal, rooted/prefix-like ids and
    /// legitimate nested ids (which stay flattened, as before).
    #[test]
    fn export_filename_is_flat_for_every_separator_and_prefix_form() {
        let cases = [
            ("../../outside/victim", ".._.._outside_victim.md"),
            (r"..\..\outside\victim", ".._.._outside_victim.md"),
            ("a/b/../../outside/victim", "a_b_.._.._outside_victim.md"),
            (r"a\b\..\..\outside\victim", "a_b_.._.._outside_victim.md"),
            ("/etc/passwd", "_etc_passwd.md"),
            (r"\root\victim", "_root_victim.md"),
            (r"\\server\share\victim", "__server_share_victim.md"),
            (r"C:\Windows\Temp\victim", "C__Windows_Temp_victim.md"),
            (r"\\?\C:\victim", "__?_C__victim.md"),
            ("folder/note", "folder_note.md"),
            (r"folder\note", "folder_note.md"),
            ("plain-id", "plain-id.md"),
        ];
        for (id, expected) in cases {
            let name = export_filename_for_id(id);
            assert_eq!(name, expected, "id {id:?}");
            assert!(
                !name.contains('/') && !name.contains('\\') && !name.contains(':'),
                "no separator survived for {id:?} -> {name:?}"
            );
            assert!(
                Path::new(&name).components().count() == 1,
                "must be a single path component: {name:?}"
            );
        }
    }

    /// Issue #284: an unreadable per-file entry is reported and skipped; the
    /// remaining valid notes still import.
    #[cfg(unix)]
    #[tokio::test]
    async fn import_continues_past_unreadable_file() {
        let root = std::env::temp_dir().join(format!("ns-cap-broken-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_cap_broken";
        store.init(ns).await.expect("init");
        std::fs::write(vault.join("good-1.md"), note("good-1", &[]).to_markdown())
            .expect("write good-1");
        std::fs::write(vault.join("good-2.md"), note("good-2", &[]).to_markdown())
            .expect("write good-2");
        std::os::unix::fs::symlink("nonexistent_target", vault.join("broken.md"))
            .expect("create broken symlink");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert_eq!(result.notes_imported, 2, "{:?}", result.errors);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("broken.md"),
            "{:?}",
            result.errors
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #284: file-count cap is enforced while collecting paths, before
    /// any note is read or upserted.
    #[tokio::test]
    async fn import_file_count_cap_fails_before_any_write() {
        let root = std::env::temp_dir().join(format!("ns-cap-files-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_cap_files";
        store.init(ns).await.expect("init");
        for id in ["one", "two"] {
            std::fs::write(vault.join(format!("{id}.md")), note(id, &[]).to_markdown())
                .expect("write note");
        }
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let limits = VaultSyncLimits {
            max_files: 1,
            max_file_bytes: MAX_VAULT_FILE_BYTES,
            max_total_bytes: MAX_VAULT_TOTAL_BYTES,
        };
        let err = import_vault_with_limits(
            config,
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
            limits,
        )
        .await
        .expect_err("file count cap");
        assert!(
            err.contains("file count exceeds maximum of 1 files"),
            "{}",
            err
        );
        assert!(store.list(ns, None, None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #284: total-byte cap is enforced in the pre-read pass, before the
    /// upsert loop starts.
    #[tokio::test]
    async fn import_total_bytes_cap_fails_before_any_write() {
        let root = std::env::temp_dir().join(format!("ns-cap-total-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_cap_total";
        store.init(ns).await.expect("init");
        for id in ["one", "two"] {
            std::fs::write(vault.join(format!("{id}.md")), note(id, &[]).to_markdown())
                .expect("write note");
        }
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let limits = VaultSyncLimits {
            max_files: MAX_VAULT_FILES,
            max_file_bytes: MAX_VAULT_FILE_BYTES,
            max_total_bytes: 1,
        };
        let err = import_vault_with_limits(
            config,
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
            limits,
        )
        .await
        .expect_err("total bytes cap");
        assert!(
            err.contains("total bytes exceed maximum of 1 bytes"),
            "{}",
            err
        );
        assert!(store.list(ns, None, None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #284: an oversized file is a per-file error, not a whole-import
    /// abort.
    #[tokio::test]
    async fn import_oversized_file_is_reported_and_skipped() {
        let root = std::env::temp_dir().join(format!("ns-cap-size-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_cap_size";
        store.init(ns).await.expect("init");
        std::fs::write(vault.join("big.md"), note("big", &[]).to_markdown()).expect("write big");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let limits = VaultSyncLimits {
            max_files: MAX_VAULT_FILES,
            max_file_bytes: 10,
            max_total_bytes: MAX_VAULT_TOTAL_BYTES,
        };
        let result = import_vault_with_limits(
            config,
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
            limits,
        )
        .await
        .expect("import");
        assert_eq!(result.notes_imported, 0);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("exceeds maximum of 10 bytes"),
            "{:?}",
            result.errors
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #284: export honors the same caps before touching the filesystem.
    #[tokio::test]
    async fn export_file_count_cap_fails_without_writing() {
        let root = std::env::temp_dir().join(format!("ns-cap-exp-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_cap_export";
        store.init(ns).await.expect("init");
        for id in ["one", "two"] {
            store
                .upsert(
                    ns,
                    id,
                    vec![0.0; 4],
                    crate::traits::MemoryPayload {
                        content: id.to_string(),
                        user_id: "default".to_string(),
                        memory_type: "fact".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: serde_json::json!({}),
                    },
                )
                .await
                .expect("seed note");
        }
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let limits = VaultSyncLimits {
            max_files: 1,
            max_file_bytes: MAX_VAULT_FILE_BYTES,
            max_total_bytes: MAX_VAULT_TOTAL_BYTES,
        };
        let err = export_vault_with_limits(config, store, limits)
            .await
            .expect_err("export file count cap");
        assert!(
            err.contains("file count exceeds maximum of 1 files"),
            "{}",
            err
        );
        assert_eq!(
            std::fs::read_dir(&vault)
                .expect("read vault")
                .filter_map(|e| e.ok())
                .count(),
            0,
            "no files written when the cap is exceeded"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #291: `/`, `\` and `:` all flatten to `_`, so distinct ids can
    /// map to one filename. The collision must be reported and counted once,
    /// never silently overwritten while inflating `notes_exported`.
    #[tokio::test]
    async fn export_vault_reports_filename_collision_without_overwrite() {
        let root = std::env::temp_dir().join(format!("ns-col-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_collision";
        store.init(ns).await.expect("init");
        for id in ["a/b", "a:b", r"a\b"] {
            store
                .upsert(
                    ns,
                    id,
                    vec![0.0; 4],
                    crate::traits::MemoryPayload {
                        content: format!("body of {id}"),
                        user_id: "default".to_string(),
                        memory_type: "fact".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: serde_json::json!({}),
                    },
                )
                .await
                .expect("seed note");
        }
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = export_vault(config, store.clone()).await.expect("export");
        assert_eq!(result.notes_exported, 1, "honest count: {:?}", result);
        assert_eq!(result.errors.len(), 2, "{:?}", result.errors);
        assert!(
            result
                .errors
                .iter()
                .all(|e| e.contains("filename collision")),
            "{:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| e.contains("a_b.md")),
            "{:?}",
            result.errors
        );
        let md_count = std::fs::read_dir(&vault)
            .expect("read vault")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
            .count();
        assert_eq!(md_count, 1, "only one file may be written");

        // Re-import into a fresh store + namespace: count must match export.
        let fresh_ns = "vault_collision_reimport";
        let fresh_store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db2"), 4)
                .expect("fresh temp store"),
        );
        fresh_store
            .init(fresh_ns)
            .await
            .expect("init fresh namespace");
        let import_config = VaultSyncConfig {
            namespace: fresh_ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let imported = import_vault(
            import_config,
            fresh_store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("reimport");
        assert!(imported.errors.is_empty(), "{:?}", imported.errors);
        assert_eq!(imported.notes_imported, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Codex 4610878281 P1: an imported id carrying Windows separators must
    /// never survive into an export filename. On Windows `canonical_vault.join`
    /// would follow `..\` out of the J6-validated root; on POSIX the backslash
    /// is a literal character and lands in the filename. Either way a path
    /// separator reaching the filesystem is the bug.
    #[tokio::test]
    async fn export_flattens_windows_separator_id() {
        let root = std::env::temp_dir().join(format!("ns-exp-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_exp";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                r"..\..\outside\victim",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "hostile".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed hostile id");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = export_vault(config, store).await.expect("export");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notes_exported, 1);
        let names: Vec<String> = std::fs::read_dir(&vault)
            .expect("read vault")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            names.iter().all(|n| !n.contains('/') && !n.contains('\\')),
            "no written filename may carry a separator: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == ".._.._outside_victim.md"),
            "hostile id must flatten to one safe name: {:?}",
            names
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Codex PR #295 P2: valid custom Turtle local-name characters (dots,
    /// leading digits, colons) in `rdf_type` must be accepted so exported
    /// memories with custom `memory_type` values such as `custom.kind`
    /// round-trip through `export_vault` -> `import_vault`, while trailing
    /// dots (which terminate a Turtle statement) remain rejected.
    #[tokio::test]
    async fn custom_dotted_memory_type_roundtrips_through_export_and_import() {
        for valid_type in ["yaml:custom.kind", "yaml:123_custom", "yaml:ns:sub.kind"] {
            let mut n = note("dotted-note", &[]);
            n.rdf_type = valid_type.to_string();
            let turtle = note_to_turtle(&n).unwrap_or_else(|e| {
                panic!("expected valid Turtle type {valid_type:?} to render, got {e}")
            });
            validate_shacl(&turtle).unwrap_or_else(|e| {
                panic!("expected valid Turtle type {valid_type:?} to pass SHACL, got {e}")
            });
        }

        for invalid_type in ["yaml:custom.", "yaml.:custom", "yaml:"] {
            let mut n = note("trailing-dot-note", &[]);
            n.rdf_type = invalid_type.to_string();
            assert!(
                note_to_turtle(&n).is_err(),
                "invalid prefixed name {invalid_type:?} must fail closed"
            );
        }

        let root = std::env::temp_dir().join(format!("ns-custom-type-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_custom_type";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                "custom-1",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "custom kind note".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "custom.kind".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed custom.kind memory");

        let export_cfg = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let exported = export_vault(export_cfg, store.clone())
            .await
            .expect("export");
        assert_eq!(exported.notes_exported, 1, "{:?}", exported.errors);

        let import_ns = "vault_custom_type_imported";
        let fresh_store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db2"), 4)
                .expect("fresh temp store"),
        );
        fresh_store.init(import_ns).await.expect("init import ns");
        let import_cfg = VaultSyncConfig {
            namespace: import_ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let imported = import_vault(
            import_cfg,
            fresh_store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(imported.errors.is_empty(), "{:?}", imported.errors);
        assert_eq!(imported.notes_imported, 1);
        let (_, payload) = fresh_store
            .get(import_ns, "custom-1")
            .await
            .expect("get")
            .expect("imported row exists");
        assert_eq!(payload.memory_type, "custom.kind");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #296: re-importing a note preserves the prior head's
    /// visible_to and eligibility metadata; the new head must not widen access
    /// to a wider audience than the prior head granted.
    #[tokio::test]
    async fn import_preserves_prior_head_visible_to_and_eligibility() {
        let root = std::env::temp_dir().join(format!("ns-visible-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_visible";
        store.init(ns).await.expect("init");
        let seeded_metadata = serde_json::json!({
            "visible_to": ["default"],
            "eligibility": "shareable",
            "canonical_sha256": "old-hash"
        });
        store
            .upsert(
                ns,
                "vis-note",
                vec![0.0; 4],
                seed_payload("default", seeded_metadata.clone()),
            )
            .await
            .expect("seed prior head");
        assert!(
            !rebac::can_read_memory("stranger", "default", &seeded_metadata),
            "stranger must not read seeded head"
        );
        let mut changed = note("vis-note", &[]);
        changed.content = "updated body".to_string();
        changed.canonical_sha256 = hex::encode(Sha256::digest(changed.content.as_bytes()));
        std::fs::write(vault.join("vis-note.md"), changed.to_markdown()).expect("write note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notes_imported, 1);
        let (_, head) = store.get(ns, "vis-note").await.unwrap().unwrap();
        assert_eq!(head.content, "updated body");
        assert!(head.metadata["supersedes"].is_string());
        assert_eq!(head.metadata["visible_to"], serde_json::json!(["default"]));
        assert_eq!(head.metadata["eligibility"], serde_json::json!("shareable"));
        assert!(
            !rebac::can_read_memory("stranger", &head.user_id, &head.metadata),
            "stranger must not read tightened head"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #297: export refuses to write through a symlinked note or canvas destination.
    #[cfg(unix)]
    #[tokio::test]
    async fn export_refuses_symlinked_note_and_canvas_destinations() {
        let root = std::env::temp_dir().join(format!("ns-expsym-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let outside = std::env::temp_dir().join(format!("ns-outsym-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).expect("create outside");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_export_sym";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                "link-note",
                vec![0.0; 4],
                seed_payload("default", serde_json::json!({"canonical_sha256": "test"})),
            )
            .await
            .expect("seed note");

        let victim_note = outside.join("victim_note.md");
        let victim_canvas = outside.join("victim_canvas.json");
        std::fs::write(&victim_note, "untouched note").expect("write victim note");
        std::fs::write(&victim_canvas, "untouched canvas").expect("write victim canvas");

        // Case (a): note destination is a symlink.
        std::os::unix::fs::symlink(&victim_note, vault.join("link-note.md")).expect("symlink note");
        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = export_vault(config, store.clone()).await.expect("export");
        assert!(
            result.errors.iter().any(|e| e.contains("symlink")),
            "{:?}",
            result.errors
        );
        assert_eq!(result.notes_exported, 0);
        assert!(!result.success);
        assert_eq!(
            std::fs::read_to_string(&victim_note).unwrap(),
            "untouched note",
            "symlink target must be untouched"
        );

        // Case (b): canvas destination is a symlink.
        std::fs::remove_file(vault.join("link-note.md")).expect("remove note symlink");
        let _ = std::fs::remove_file(vault.join("topology.canvas"));
        std::os::unix::fs::symlink(&victim_canvas, vault.join("topology.canvas")).expect("symlink canvas");
        let config2 = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let err = export_vault(config2, store.clone()).await.unwrap_err();
        assert!(
            err.contains("symlink"),
            "{}",
            err
        );
        assert_eq!(
            std::fs::read_to_string(&victim_canvas).unwrap(),
            "untouched canvas",
            "symlink target must be untouched"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
