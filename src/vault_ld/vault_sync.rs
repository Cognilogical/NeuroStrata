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
use crate::vault_ld::{
    CanvasDocument, CanvasNode, VaultNote, FINGERPRINT_VERSION, VAULT_LD_SHAPES_V1, YAML_LD_CONTEXT,
};

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

/// Metadata keys the import owns on the replacement head. Everything else on
/// a prior head (notably `task`) is preserved across a reimport: #335 showed
/// that copying a fixed field list drops the next structured field silently.
const IMPORT_OWNED_METADATA_KEYS: [&str; 18] = [
    // Import-owned values, overlaid after the prior head is copied.
    "vault_source",
    "stub",
    "canonical_sha256",
    "fingerprint_version",
    "related",
    "valid_from",
    "supersedes",
    // Edge renderings. The vault note's parsed link set is authoritative;
    // keeping alternate keys would resurrect links deleted from the vault,
    // because VaultNote::from_payload unions all four fields on export.
    "related_to",
    "relates_to",
    "governs",
    "depends_on",
    // Store-derived and retirement bookkeeping must not be re-headered.
    "facets",
    "facets_bitemporal",
    "concept_iri",
    "topology_original",
    "valid_to",
    "superseded_by",
    "archived_by_vault_import",
];

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

/// Open an export destination for a truncating write.
///
/// `ensure_safe_export_destination` inspects the path first, but that check
/// and the write are separate syscalls: a symlink swapped in between them
/// would otherwise be followed. On Unix the open itself carries
/// `O_NOFOLLOW`, so the kernel refuses the final-component symlink (ELOOP)
/// and closes that race.
#[cfg(unix)]
fn open_export_destination(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Non-Unix: there is no `O_NOFOLLOW` equivalent for reparse points, so the
/// `symlink_metadata` precheck remains the only guard. A reparse point
/// swapped in after that check can still be followed; this is the documented
/// residual gap.
#[cfg(not(unix))]
fn open_export_destination(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Write one export artifact, refusing symlink and non-regular destinations.
///
/// The `ensure_safe_export_destination` precheck gives the caller a clear
/// refusal before any byte is written; `open_export_destination` narrows the
/// final-component race between that check and the write.
fn write_export_destination(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;

    ensure_safe_export_destination(path)?;
    let mut file = open_export_destination(path).map_err(|e| {
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ELOOP) {
            return format!(
                "Refusing to write through symlink destination {}",
                path.display()
            );
        }
        format!("Failed to write {}: {}", path.display(), e)
    })?;
    file.write_all(contents)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))
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

    // Filter by ReBAC can_read_memory, then drop retired rows: an export is
    // the current-knowledge surface, and `list` is generation-scoped only.
    let principal = config.principal.as_deref().unwrap_or("default");
    let now = chrono::Utc::now().timestamp();
    let filtered: Vec<SearchResult> = memories
        .into_iter()
        .filter(|m| rebac::can_read_memory(principal, &m.payload.user_id, &m.payload.metadata))
        .filter(|m| {
            let metadata = &m.payload.metadata;
            !crate::store::ladybug::is_retired_at(metadata, now)
                && metadata
                    .get("superseded_by")
                    .map_or(true, serde_json::Value::is_null)
        })
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

    // Plan every note write and check every destination before the first
    // write: a refused note or canvas must not leave a partial export behind.
    // Flattened ids can collide (a/b, a:b, a\b -> a_b.md); never overwrite an
    // already-planned note and never inflate the exported count with a
    // silently lost note.
    let mut errors = Vec::new();
    let mut written_filenames: BTreeMap<String, String> = BTreeMap::new();
    let mut planned_writes: Vec<(PathBuf, String)> = Vec::new();
    let mut unsafe_destination = false;

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

        if let Err(e) = ensure_safe_export_destination(&filepath) {
            errors.push(format!(
                "Export destination unsafe for {} failed: {}",
                filepath.display(),
                e
            ));
            unsafe_destination = true;
            continue;
        }
        planned_writes.push((filepath, note.to_markdown()));
    }

    // Generate the Canvas and check its destination too, still before any
    // note is written. A refused canvas is a typed error with zero writes.
    let canvas = generate_canvas(&notes, &config.namespace);
    let canvas_path = canonical_output.join("topology.canvas");

    let canvas_json = serde_json::to_string_pretty(&canvas)
        .map_err(|e| format!("Failed to serialize canvas: {}", e))?;

    ensure_safe_export_destination(&canvas_path)?;

    if unsafe_destination {
        return Ok(VaultExportResult {
            success: false,
            message: "Exported 0 notes".to_string(),
            notes_exported: 0,
            topology_exported: false,
            canvas_path: None,
            errors,
        });
    }

    let mut exported = 0;
    for (filepath, markdown) in &planned_writes {
        if let Err(e) = write_export_destination(filepath, markdown.as_bytes()) {
            errors.push(format!(
                "Export destination unsafe for {} failed: {}",
                filepath.display(),
                e
            ));
        } else {
            exported += 1;
        }
    }

    write_export_destination(&canvas_path, canvas_json.as_bytes())?;

    Ok(VaultExportResult {
        success: errors.is_empty(),
        message: format!("Exported {} notes", exported),
        notes_exported: exported,
        topology_exported: true,
        canvas_path: Some(canvas_path.to_string_lossy().to_string()),
        errors,
    })
}

/// Exact-mode target resolution against the ids in `known`. The declared
/// spelling wins so an exported id that already starts with the namespace
/// (`<ns>::7::path`) keeps its own row; only then may the namespace prefix act
/// as an alias for a bare id (`<ns>::z` → `z`).
fn exact_note_target(
    namespace: &str,
    declared: &str,
    known: &std::collections::BTreeSet<String>,
) -> Option<String> {
    crate::parser::ingest::exact_target_match(namespace, declared, |candidate| {
        known.contains(candidate)
    })
    .map(str::to_string)
}

/// Store-backed half of the same rule: try each spelling in order and return
/// the first row that exists. `None` means every spelling is absent and the
/// caller must defer or stub the declared spelling.
async fn exact_stored_target(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    declared: &str,
    cache: &mut std::collections::HashMap<String, Option<(String, crate::traits::MemoryPayload)>>,
) -> Result<Option<(String, crate::traits::MemoryPayload)>, String> {
    if let Some(cached) = cache.get(declared) {
        return Ok(cached.clone());
    }
    let mut found = None;
    for candidate in crate::parser::ingest::exact_target_spellings(namespace, declared) {
        match store.get(namespace, candidate).await {
            Ok(Some((_, payload))) => {
                found = Some((candidate.to_string(), payload));
                break;
            }
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    cache.insert(declared.to_string(), found.clone());
    Ok(found)
}

/// The keep set for one head's declarations, plus whether it depends on a
/// post-write replay.
struct KeepTargets {
    ids: Vec<String>,
    /// The keep-set chose a vault-known id that was not stored when the head
    /// was written. The write may have bound a stored alias fallback and
    /// reported no deferred edge, so reconciliation would retire that edge
    /// without a post-write replay once the preferred id exists.
    pending_preferred: bool,
}

/// The edge ids a head's declarations will actually bind, matching the store's
/// exact-mode lookup. Both id sources are consulted per spelling so the keep
/// set cannot pick an alias the upsert would not have bound; an unresolved
/// declaration keeps its declared spelling so a later replay binds the same
/// target the vault named.
async fn exact_keep_targets(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    declared: &[String],
    known: &std::collections::BTreeSet<String>,
) -> KeepTargets {
    let mut ids = Vec::with_capacity(declared.len());
    let mut pending_preferred = false;
    for target in declared {
        // Find the first spelling present in either source. Vault membership is
        // free; the store is queried only until one spelling resolves.
        let mut store_hit = None;
        let mut known_hit = None;
        let mut failure = None;
        for candidate in crate::parser::ingest::exact_target_spellings(namespace, target) {
            if known.contains(candidate) {
                known_hit = Some(candidate.to_string());
                break;
            }
            match store.get(namespace, candidate).await {
                Ok(Some(_)) => {
                    store_hit = Some(candidate.to_string());
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    failure = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(e) = failure {
            warn!("Could not resolve exact edge target {}: {}", target, e);
            ids.push(target.clone());
            continue;
        }
        // A vault-known id may be written later in this import. It is still
        // the preferred binding, so its absence now makes the head depend on
        // the post-write replay even when an alias kept the first upsert happy.
        let chosen = match known_hit {
            Some(candidate) => match store.get(namespace, &candidate).await {
                Ok(Some(_)) => candidate,
                Ok(None) => {
                    pending_preferred = true;
                    candidate
                }
                Err(e) => {
                    // Resolution failed, not the target: keep the preferred id
                    // and retry after the writes rather than dropping it.
                    warn!("Could not resolve exact edge target {}: {}", target, e);
                    pending_preferred = true;
                    candidate
                }
            },
            None => store_hit.unwrap_or_else(|| target.clone()),
        };
        ids.push(chosen);
    }
    KeepTargets {
        ids,
        pending_preferred,
    }
}

/// WikiLink targets with no corresponding note in the vault. BTreeSet gives
/// deterministic order for queryable stub minting. An own-namespace prefix is
/// an alias only when the bare note exists, so unresolved ids keep the spelling
/// the vault declared.
pub fn unresolved_wiki_targets(notes: &[VaultNote], namespace: &str) -> Vec<String> {
    let known: std::collections::BTreeSet<String> = notes.iter().map(|n| n.id.clone()).collect();
    let mut missing = std::collections::BTreeSet::new();
    for note in notes {
        for target in &note.related {
            if exact_note_target(namespace, target, &known).is_none() {
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
        // #337: authorize each candidate's RESOLVED path, then read that same
        // path. Checking only the vault root let a `.md` symlink import a file
        // from outside the project root / allowed_external_dirs.
        let resolved = match crate::daemon::check_caller_path(path, &project_root, &allowed) {
            Ok(resolved) => resolved,
            Err(e) => {
                errors.push(format!("Refusing vault file {}: {}", path.display(), e));
                continue;
            }
        };
        let metadata = match std::fs::metadata(&resolved) {
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
        let content = match std::fs::read_to_string(&resolved) {
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
            Ok(note) if note.id.contains(".vault-history.") => {
                // History copies are importer projections, never vault-authored
                // knowledge. Old exports may still carry them; ignoring the id
                // keeps a stale vault from resurrecting one.
                continue;
            }
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
    // #336: a target is unresolved only when the vault AND the store lack it.
    // In a partial vault a linked memory's file is absent by definition; a
    // placeholder must not replace the live head that still exists.
    let mut mintable = Vec::new();
    let mut exact_cache = std::collections::HashMap::new();
    for target in unresolved_wiki_targets(&notes, &config.namespace) {
        match exact_stored_target(&store, &config.namespace, &target, &mut exact_cache).await {
            Ok(Some((_, payload))) => {
                // An existing stub is exactly the state this loop would
                // produce, so re-import is a silent no-op. A non-stub head
                // is live knowledge whose file is simply absent from a
                // partial vault — that is the #336 refusal.
                let prior_is_stub = payload
                    .metadata
                    .get("stub")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !prior_is_stub {
                    errors.push(format!(
                        "Skipping WikiLink stub for '{}': the memory exists in the store \
                         and its file is absent from the vault",
                        target
                    ));
                }
            }
            Ok(None) => mintable.push(target),
            Err(e) => errors.push(format!(
                "Skipping WikiLink stub for '{}': could not check the store: {}",
                target, e
            )),
        }
    }
    let mut stub_ids = std::collections::BTreeSet::new();
    for target in mintable {
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
    let known_notes: std::collections::BTreeSet<String> =
        notes.iter().map(|n| n.id.clone()).collect();

    let principal = config.principal.as_deref().unwrap_or("default");
    let now = chrono::Utc::now().timestamp();

    struct Planned {
        id: String,
        vector: Vec<f32>,
        metadata: serde_json::Value,
        content: String,
        memory_type: String,
        /// The vault's parsed link set, kept for edge reconciliation.
        related: Vec<String>,
        history: Option<(String, Vec<f32>, crate::traits::MemoryPayload)>,
    }

    let mut planned: Vec<Planned> = Vec::new();
    // An unchanged declarer skips planning, so its edge can never observe a
    // preferred target that only arrives in this turn. Record the declaration
    // and rebind edges after every head is written, without touching the head.
    let mut unchanged_declarers: Vec<(String, Vec<String>)> = Vec::new();
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

        // #336 backstop: a stub is additive only. If a prior head exists for
        // a stub id (a write landed between resolution and planning), refuse
        // rather than replace live content with placeholder text.
        if is_stub && prior_payload.is_some() {
            errors.push(format!(
                "Refusing to replace existing memory '{}' with a WikiLink stub",
                note.id
            ));
            continue;
        }

        let declared_type = note.rdf_type.strip_prefix("yaml:").unwrap_or("symbol");
        if let Some(ref prior) = prior_payload {
            // Byte-identical re-import is a no-op before any sovereignty
            // question: a protected row we do not touch is not overwritten.
            // The hash always comes from parsed content/edges/type, never
            // frontmatter. Task rows have no stored hash, so derive the same
            // identity the export path would render for the prior payload
            // (#335).
            let prior_hash = prior
                .metadata
                .get("canonical_sha256")
                .and_then(|v| v.as_str());
            let prior_version = prior
                .metadata
                .get("fingerprint_version")
                .and_then(Value::as_u64);
            let unchanged = if prior_version.is_some_and(|v| v >= FINGERPRINT_VERSION) {
                match prior_hash {
                    Some(hash) => hash == note.canonical_sha256,
                    None => {
                        VaultNote::from_payload(&note.id, prior).canonical_sha256
                            == note.canonical_sha256
                    }
                }
            } else {
                // v1 head: keep its no-op round-trip, but require the declared
                // type to match so a type-only edit still versions.
                prior.memory_type == declared_type
                    && match prior_hash {
                        Some(hash) => {
                            hash == note.fingerprint_legacy() || hash == note.canonical_sha256
                        }
                        None => {
                            VaultNote::from_payload(&note.id, prior).fingerprint_legacy()
                                == note.fingerprint_legacy()
                        }
                    }
            };
            if unchanged {
                // Rebinding mutates edges, so an untouched head still needs
                // the same sovereignty gate as a changed one. A protected head
                // stays a silent no-op, exactly as the unchanged path was before.
                if writable_prior(principal, prior).is_ok() {
                    unchanged_declarers.push((note.id.clone(), note.related.clone()));
                }
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

        // #335: start from the prior head's structured metadata so a reimport
        // cannot silently drop fields the import does not own (`task`, and any
        // future structured state). Import-owned and store-derived keys are
        // removed here and overlaid below.
        let mut metadata = match prior_payload.as_ref().and_then(|p| p.metadata.as_object()) {
            Some(prior_obj) => {
                let mut preserved = prior_obj.clone();
                for key in IMPORT_OWNED_METADATA_KEYS {
                    preserved.remove(key);
                }
                serde_json::Value::Object(preserved)
            }
            None => serde_json::json!({}),
        };
        metadata["vault_source"] = serde_json::json!(true);
        // A WikiLink names an exact vault-note id. Store-side path resolution
        // must not relink it to an ingested symbol that happens to share its
        // logical path (PR #363).
        metadata["exact_edge_targets"] = serde_json::json!(true);
        metadata["stub"] = serde_json::json!(is_stub);
        metadata["canonical_sha256"] = serde_json::json!(note.canonical_sha256);
        metadata["fingerprint_version"] = serde_json::json!(FINGERPRINT_VERSION);
        // `related` is the vault render; `related_to` is the parser-recognised
        // relation key that materialises the RELATES_TO edge (#343).
        metadata["related"] = serde_json::json!(note.related);
        metadata["related_to"] = serde_json::json!(note.related);
        metadata["valid_from"] = serde_json::json!(now);

        let mut history = None;
        if let (Some(hv), Some(mut prior)) = (prior_vector, prior_payload) {
            if !prior.metadata.is_object() {
                errors.push(format!(
                    "Prior head {} has non-object metadata; refusing bitemporal import",
                    note.id
                ));
                continue;
            }
            // `visible_to` / `eligibility` (and any future structured field)
            // already came across in the wholesale copy above.
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
            memory_type: declared_type.to_string(),
            related: note.related,
            history,
        });
    }

    // ---- Phase 2: writes ----
    let mut imported = 0;
    let mut imported_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut deferred_retry: Vec<(String, Vec<f32>, crate::traits::MemoryPayload)> = Vec::new();
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

        let retry = payload
            .metadata
            .get("related_to")
            .and_then(Value::as_array)
            .is_some_and(|targets| !targets.is_empty())
            .then(|| (payload.clone(), item.vector.clone()));
        let keep = exact_keep_targets(&store, &config.namespace, &item.related, &known_notes).await;

        // Materialise the new head before retiring the old head's edges.
        // Reconciliation deletes edges the current head still relies on, so
        // doing it first would leave a surviving old head broken when the
        // upsert fails (#363). Vault targets are exact note ids, so the
        // declared set matches the ids the upsert writes.
        match store
            .upsert(&config.namespace, &item.id, item.vector, payload)
            .await
        {
            Ok(report) => {
                imported += 1;
                imported_ids.insert(item.id.clone());
                if let Err(e) = store
                    .reconcile_declared_edges(&config.namespace, &item.id, "RELATES_TO", &keep.ids)
                    .await
                {
                    errors.push(format!(
                        "Failed to retire stale edges for {}: {}",
                        item.id, e
                    ));
                }
                // A declarer may precede its target in id order, and an alias
                // fallback can hide that from the deferred report: retry after
                // every head exists whenever the preferred binding was not yet
                // stored.
                if !report.deferred_edges.is_empty() || keep.pending_preferred {
                    if let Some((payload, vector)) = retry {
                        deferred_retry.push((item.id.clone(), vector, payload));
                    }
                }
            }
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

    // Every row (and stub) now exists, so a forward reference resolves. A
    // target absent from both the vault and the store stays deferred.
    for (id, vector, payload) in deferred_retry {
        let declared: Vec<String> = payload
            .metadata
            .get("related_to")
            .and_then(Value::as_array)
            .map(|targets| {
                targets
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let keep = exact_keep_targets(&store, &config.namespace, &declared, &known_notes).await;
        if let Err(e) = store.upsert(&config.namespace, &id, vector, payload).await {
            errors.push(format!("Failed to relink edges for {}: {}", id, e));
        } else if let Err(e) = store
            .reconcile_declared_edges(&config.namespace, &id, "RELATES_TO", &keep.ids)
            .await
        {
            errors.push(format!("Failed to retire stale edges for {}: {}", id, e));
        }
    }

    // A declaration that did not change can still point at the wrong row: the
    // preferred exact target it names may only have been imported now. Rebind
    // those edges after every head exists; the head row itself is never
    // rewritten, so no content version or history row appears.
    for (id, declared) in unchanged_declarers {
        let keep = exact_keep_targets(&store, &config.namespace, &declared, &known_notes).await;
        if !keep.ids.iter().any(|target| imported_ids.contains(target)) {
            continue;
        }
        if let Err(e) = store
            .rebind_declared_edges(&config.namespace, &id, "RELATES_TO", &keep.ids)
            .await
        {
            errors.push(format!("Failed to rebind edges for {}: {}", id, e));
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
            unresolved_wiki_targets(&notes, "ns"),
            vec!["missing-1", "missing-2"]
        );

        // A qualified spelling is an alias only when the bare note exists.
        let alias = vec![note("a", &["ns::z"]), note("z", &[])];
        assert!(unresolved_wiki_targets(&alias, "ns").is_empty());
        // With no bare note the declaration stays unresolved in its own spelling.
        let missing = vec![note("a", &["ns::missing"])];
        assert_eq!(unresolved_wiki_targets(&missing, "ns"), vec!["ns::missing"]);
        // A verbatim scoped note id resolves before the prefix is read as an alias.
        let verbatim = vec![note("a", &["ns::7::path"]), note("ns::7::path", &[])];
        assert!(unresolved_wiki_targets(&verbatim, "ns").is_empty());
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
        assert!(out
            .errors
            .iter()
            .any(|e| e.to_lowercase().contains("secret")));
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

    /// Issue #297: the open that performs the write must itself refuse a
    /// symlink final component. The metadata precheck is deliberately
    /// bypassed here to model the TOCTOU window between that check and the
    /// write; only `O_NOFOLLOW` closes it.
    #[cfg(unix)]
    #[test]
    fn open_export_destination_refuses_symlink_without_precheck() {
        let root = std::env::temp_dir().join(format!("ns-exp-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create root");
        let victim = root.join("victim.md");
        std::fs::write(&victim, "untouched").expect("write victim");
        let dest = root.join("dest.md");
        std::os::unix::fs::symlink(&victim, &dest).expect("symlink destination");

        let err = open_export_destination(&dest).expect_err("symlink open must be refused");
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "untouched",
            "the symlink target must not be truncated"
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
        std::os::unix::fs::symlink(&victim_canvas, vault.join("topology.canvas"))
            .expect("symlink canvas");
        let config2 = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let err = export_vault(config2, store.clone()).await.unwrap_err();
        assert!(err.contains("symlink"), "{}", err);
        assert_eq!(
            std::fs::read_to_string(&victim_canvas).unwrap(),
            "untouched canvas",
            "symlink target must be untouched"
        );
        assert!(
            !vault.join("link-note.md").exists(),
            "a refused canvas must not leave notes written"
        );
        let md_count = std::fs::read_dir(&vault)
            .expect("read vault")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
            .count();
        assert_eq!(md_count, 0, "canvas refusal must write no note files");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Issue #297: one refused note destination must not leave the other
    /// notes already written; all destinations are checked before the first
    /// write.
    #[cfg(unix)]
    #[tokio::test]
    async fn export_refuses_all_notes_when_one_destination_is_symlinked() {
        let root = std::env::temp_dir().join(format!("ns-expsym-partial-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let outside = std::env::temp_dir().join(format!("ns-outsym-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).expect("create outside");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_export_sym_partial";
        store.init(ns).await.expect("init");
        for id in ["link-note", "safe-note"] {
            store
                .upsert(
                    ns,
                    id,
                    vec![0.0; 4],
                    seed_payload("default", serde_json::json!({"canonical_sha256": "test"})),
                )
                .await
                .expect("seed note");
        }
        let victim = outside.join("victim_note.md");
        std::fs::write(&victim, "untouched note").expect("write victim note");
        std::os::unix::fs::symlink(&victim, vault.join("link-note.md")).expect("symlink note");

        let config = VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.clone(),
            output_dir: vault.clone(),
            principal: None,
            project_root: Some(root.clone()),
            allowed_external_dirs: None,
        };
        let result = export_vault(config, store.clone()).await.expect("export");
        assert!(!result.success);
        assert_eq!(
            result.notes_exported, 0,
            "no note may be written when any destination is refused: {:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| e.contains("symlink")),
            "{:?}",
            result.errors
        );
        assert!(
            !vault.join("safe-note.md").exists(),
            "a refused note must not leave safe notes already written"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "untouched note",
            "symlink target must be untouched"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// #335: task rows carry no `canonical_sha256`, so the unchanged check
    /// must derive identity from the prior payload. An unchanged round-trip
    /// must neither replace the head nor drop `metadata.task`.
    #[tokio::test]
    async fn import_unchanged_task_roundtrip_is_a_noop_and_keeps_task_state() {
        let root = std::env::temp_dir().join(format!("ns-task-noop-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_task_noop";
        store.init(ns).await.expect("init");
        let task_meta = serde_json::json!({
            "status": "done",
            "session_id": "sess-1",
            "assignee": "agent-a",
            "blocked_by": ["other-task"],
            "history": [{"at": 1, "note": "claimed"}],
            "created_at": 1,
            "updated_at": 2
        });
        store
            .upsert(
                ns,
                "task-1",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "Ship the vault fix".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: Some("agent-a".to_string()),
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({ "task": task_meta.clone() }),
                },
            )
            .await
            .expect("seed task head");

        let mut exported = note("task-1", &[]);
        exported.rdf_type = "yaml:task".to_string();
        exported.epistemic_tier = "task".to_string();
        exported.content = "Ship the vault fix".to_string();
        exported.canonical_sha256 = exported.fingerprint();
        std::fs::write(vault.join("task-1.md"), exported.to_markdown()).expect("write task note");

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
        assert_eq!(
            result.notes_imported, 0,
            "unchanged task round-trip must be a no-op: {:?}",
            result.errors
        );
        let (_, head) = store.get(ns, "task-1").await.unwrap().unwrap();
        assert_eq!(head.content, "Ship the vault fix");
        assert_eq!(head.memory_type, "task");
        assert_eq!(head.metadata["task"], task_meta);
        assert!(
            head.metadata.get("supersedes").is_none(),
            "unchanged import must not retire the head"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #335: an edited task reimport must keep the prior head's structured
    /// task state (status, claim, blockers, history) on the new live head.
    #[tokio::test]
    async fn import_task_edit_preserves_task_state() {
        let root = std::env::temp_dir().join(format!("ns-task-edit-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_task_edit";
        store.init(ns).await.expect("init");
        let task_meta = serde_json::json!({
            "status": "in_progress",
            "session_id": "sess-1",
            "assignee": "agent-a",
            "blocked_by": ["other-task"],
            "history": [{"at": 1, "note": "claimed"}]
        });
        store
            .upsert(
                ns,
                "task-2",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "Original task body".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "task".to_string(),
                    agent_name: Some("agent-a".to_string()),
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({ "task": task_meta.clone() }),
                },
            )
            .await
            .expect("seed task head");

        let mut edited = note("task-2", &[]);
        edited.rdf_type = "yaml:task".to_string();
        edited.epistemic_tier = "task".to_string();
        edited.content = "Edited task body".to_string();
        edited.canonical_sha256 = edited.fingerprint();
        std::fs::write(vault.join("task-2.md"), edited.to_markdown()).expect("write task note");

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
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let (_, head) = store.get(ns, "task-2").await.unwrap().unwrap();
        assert_eq!(head.content, "Edited task body");
        assert_eq!(head.memory_type, "task");
        assert_eq!(
            head.metadata["task"], task_meta,
            "task state must survive an edited reimport"
        );
        let history_id = head.metadata["supersedes"].as_str().expect("supersedes");
        let (_, history) = store.get(ns, history_id).await.unwrap().unwrap();
        assert_eq!(history.content, "Original task body");
        assert_eq!(history.metadata["task"], task_meta);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Codex review 7b8f8fb4#r4649890196: preserving the prior head's
    /// alternate edge keys (`related_to`/`governs`/`depends_on`) must not
    /// resurrect links the edited vault note removed — `from_payload` unions
    /// all four fields, so the next export would restore them.
    #[tokio::test]
    async fn import_reconciles_preserved_edge_fields_with_edited_links() {
        let root = std::env::temp_dir().join(format!("ns-edge-reconcile-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_edge_reconcile";
        store.init(ns).await.expect("init");

        // The prior head carries typed edge renderings plus unrelated state.
        store
            .upsert(
                ns,
                "edge-reconcile",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "Original body".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "canonical_sha256": "seed-hash",
                        "related": ["kept-edge", "deleted-edge"],
                        "related_to": ["deleted-related-to"],
                        "governs": ["deleted-governs"],
                        "depends_on": ["deleted-depends"],
                        "custom_state": {"stage": "kept"}
                    }),
                },
            )
            .await
            .expect("seed edge head");
        // The kept target is in the vault unchanged, so it is a no-op and
        // the import has nothing to stub.
        let mut kept = note("kept-edge", &[]);
        kept.content = "kept target".to_string();
        kept.canonical_sha256 = kept.fingerprint();
        store
            .upsert(
                ns,
                "kept-edge",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: kept.content.clone(),
                    user_id: "default".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "canonical_sha256": kept.canonical_sha256
                    }),
                },
            )
            .await
            .expect("seed kept edge target");
        std::fs::write(vault.join("kept-edge.md"), kept.to_markdown())
            .expect("write kept target note");

        // The edited vault note keeps only one link.
        let mut edited = note("edge-reconcile", &["kept-edge"]);
        edited.content = "Edited body".to_string();
        edited.canonical_sha256 = edited.fingerprint();
        std::fs::write(vault.join("edge-reconcile.md"), edited.to_markdown())
            .expect("write edited note");

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
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let (_, head) = store.get(ns, "edge-reconcile").await.unwrap().unwrap();
        assert_eq!(head.metadata["related"], serde_json::json!(["kept-edge"]));
        // #343: `related_to` is now the parser-recognised head key and must
        // carry exactly the parsed set, not the stale preserved one.
        assert_eq!(
            head.metadata["related_to"],
            serde_json::json!(["kept-edge"]),
            "the head must declare the parsed link set for edge materialisation"
        );
        for key in ["governs", "depends_on"] {
            assert!(
                head.metadata.get(key).is_none(),
                "stale edge field {key} must be reconciled away: {:?}",
                head.metadata
            );
        }
        assert_eq!(
            head.metadata["custom_state"]["stage"],
            serde_json::json!("kept"),
            "unrelated structured state must survive"
        );
        let exported = VaultNote::from_payload("edge-reconcile", &head);
        assert_eq!(
            exported.related,
            vec!["kept-edge".to_string()],
            "the next export must not resurrect deleted links"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #336: a WikiLink target that exists in the store but whose file is
    /// absent from the vault must not be replaced by a stub.
    #[tokio::test]
    async fn import_does_not_stub_over_existing_head_absent_from_vault() {
        let root = std::env::temp_dir().join(format!("ns-stub-over-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_stub_over";
        store.init(ns).await.expect("init");
        store
            .upsert(
                ns,
                "existing-target",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "Real knowledge".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "canonical_sha256": "seed-hash",
                        "visible_to": ["default"]
                    }),
                },
            )
            .await
            .expect("seed existing target");

        // Only the linker is in the vault; the target's own file is absent.
        std::fs::write(
            vault.join("linker.md"),
            note("linker", &["existing-target"]).to_markdown(),
        )
        .expect("write linker note");

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
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("existing-target") && e.to_lowercase().contains("stub")),
            "the unresolved-but-present target must be reported: {:?}",
            result.errors
        );
        let (_, head) = store
            .get(ns, "existing-target")
            .await
            .unwrap()
            .expect("existing target must remain");
        assert_eq!(head.content, "Real knowledge");
        assert_eq!(head.memory_type, "fact");
        assert!(
            head.metadata.get("stub").is_none()
                || head.metadata["stub"] == serde_json::json!(false),
            "existing head must not be marked as a stub: {:?}",
            head.metadata
        );
        assert!(
            head.metadata.get("supersedes").is_none(),
            "existing head must not be retired by a stub"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #337: a `.md` symlink inside an authorized vault that resolves outside
    /// the project root (and allowlist) must be refused, not imported.
    #[cfg(unix)]
    #[tokio::test]
    async fn import_refuses_md_symlink_outside_authorized_roots() {
        let root = std::env::temp_dir().join(format!("ns-symlink-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("ns-symlink-out-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        std::fs::create_dir_all(&outside).expect("create outside");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_symlink";
        store.init(ns).await.expect("init");

        std::fs::write(vault.join("good.md"), note("good", &[]).to_markdown())
            .expect("write good note");
        let escaped = outside.join("escape-note.md");
        std::fs::write(&escaped, note("escape-note", &[]).to_markdown())
            .expect("write outside note");
        std::os::unix::fs::symlink(&escaped, vault.join("escape.md")).expect("symlink escape");

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
        assert_eq!(
            result.notes_imported, 1,
            "only the in-bound note may import: {:?}",
            result.errors
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("escape.md") && e.to_lowercase().contains("refus")),
            "the escaping symlink must be refused by name: {:?}",
            result.errors
        );
        assert!(
            store.get(ns, "escape-note").await.unwrap().is_none(),
            "content outside the authority boundary must not enter the store"
        );
        assert!(store.get(ns, "good").await.unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// v1 identity, reproduced here so the legacy seeds do not depend on the
    /// code under test gaining a legacy method before it exists.
    fn legacy_fingerprint(note: &VaultNote) -> String {
        let mut related = note.related.clone();
        related.sort();
        related.dedup();
        hex::encode(Sha256::digest(
            format!(
                "{}\u{1f}{}\u{1f}{}",
                note.content.trim(),
                note.epistemic_tier,
                related.join("\u{1e}")
            )
            .as_bytes(),
        ))
    }

    fn vault_config(root: &Path, vault: &Path, ns: &str) -> VaultSyncConfig {
        VaultSyncConfig {
            namespace: ns.to_string(),
            vault_path: vault.to_path_buf(),
            output_dir: vault.to_path_buf(),
            principal: None,
            project_root: Some(root.to_path_buf()),
            allowed_external_dirs: None,
        }
    }

    /// #343: a WikiLink must reach the graph, not just metadata.
    #[tokio::test]
    async fn import_materializes_wikilink_edges_and_exports_without_duplicates() {
        let root = std::env::temp_dir().join(format!("ns-vault-edges-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_edges";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        let mut a = note("note-a", &["note-b"]);
        a.content = "A links to B".to_string();
        a.canonical_sha256 = a.fingerprint();
        let mut b = note("note-b", &[]);
        b.content = "B is a target".to_string();
        b.canonical_sha256 = b.fingerprint();
        std::fs::write(vault.join("note-a.md"), a.to_markdown()).expect("write a");
        std::fs::write(vault.join("note-b.md"), b.to_markdown()).expect("write b");

        let imported = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(imported.errors.is_empty(), "{:?}", imported.errors);
        assert_eq!(imported.notes_imported, 2, "{:?}", imported.errors);

        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            edges.iter().any(|(from, rel, to, _)| from == "note-a"
                && rel == "RELATES_TO"
                && to == "note-b"),
            "WikiLink must be a stored RELATES_TO edge, got {:?}",
            edges
        );

        let exported = export_vault(config.clone(), store.clone())
            .await
            .expect("export");
        assert!(exported.success, "{:?}", exported.errors);
        let markdown = std::fs::read_to_string(vault.join("note-a.md")).expect("read export");
        assert_eq!(
            markdown.matches("[[note-b]]").count(),
            1,
            "export must render the link once, not once per metadata key"
        );

        let again = import_vault(config, store, std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert_eq!(again.notes_imported, 0, "round-trip must be a no-op");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #346: a WikiLink removed from the vault must retire the stored edge,
    /// not only its metadata. Reconcile is scoped to upsert-declared
    /// RELATES_TO edges, so mention-detector links and GOVERNS survive.
    #[tokio::test]
    async fn reimport_retires_stored_edges_for_removed_wikilinks() {
        let root =
            std::env::temp_dir().join(format!("ns-vault-retire-edges-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_retire_edges";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        let mut linker = note("linker", &["stale-target", "kept-target"]);
        linker.content = "links both".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        for plain in [note("stale-target", &[]), note("kept-target", &[])] {
            let mut plain = plain;
            plain.content = format!("body of {}", plain.id);
            plain.canonical_sha256 = plain.fingerprint();
            std::fs::write(vault.join(format!("{}.md", plain.id)), plain.to_markdown())
                .expect("write note");
        }
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("write linker");

        let first = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        let edges = store.list_edges(ns).await.expect("list edges");
        for target in ["stale-target", "kept-target"] {
            assert!(
                edges.iter().any(|(from, rel, to, _)| from == "linker"
                    && rel == "RELATES_TO"
                    && to == target),
                "import must materialize {target}: {:?}",
                edges
            );
        }

        // Control: an unrelated upsert-declared edge is never reconciled.
        store
            .upsert(
                ns,
                "governing-rule",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "a rule".to_string(),
                    user_id: "default".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({"governs": ["kept-target"]}),
                },
            )
            .await
            .expect("seed governs edge");

        linker.related = vec!["kept-target".to_string()];
        linker.content = "links one".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("rewrite linker");

        let second = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("reimport");
        assert_eq!(second.notes_imported, 1, "{:?}", second.errors);
        assert!(second.errors.is_empty(), "{:?}", second.errors);

        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            !edges.iter().any(|(from, rel, to, _)| from == "linker"
                && rel == "RELATES_TO"
                && to == "stale-target"),
            "a removed WikiLink must retire its RELATES_TO edge: {:?}",
            edges
        );
        assert!(
            edges.iter().any(|(from, rel, to, _)| from == "linker"
                && rel == "RELATES_TO"
                && to == "kept-target"),
            "a kept WikiLink must keep its edge: {:?}",
            edges
        );
        assert!(
            edges
                .iter()
                .any(|(from, rel, to, _)| from == "governing-rule"
                    && rel == "GOVERNS"
                    && to == "kept-target"),
            "an unrelated upsert edge must survive reconciliation: {:?}",
            edges
        );

        let exported = export_vault(config.clone(), store.clone())
            .await
            .expect("export");
        assert!(exported.success, "{:?}", exported.errors);
        let markdown = std::fs::read_to_string(vault.join("linker.md")).expect("read export");
        assert!(
            !markdown.contains("[[stale-target]]"),
            "export must not resurrect the removed link: {markdown}"
        );
        assert_eq!(markdown.matches("[[kept-target]]").count(), 1, "{markdown}");

        // Empty transition: removing the last link retires the last edge.
        linker.related.clear();
        linker.content = "links none".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown())
            .expect("rewrite empty linker");
        let third = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import empty links");
        assert!(third.errors.is_empty(), "{:?}", third.errors);
        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            !edges
                .iter()
                .any(|(from, rel, _, _)| from == "linker" && rel == "RELATES_TO"),
            "an empty link set must retire every declared RELATES_TO edge: {:?}",
            edges
        );
        assert!(
            edges
                .iter()
                .any(|(from, rel, to, _)| from == "governing-rule"
                    && rel == "GOVERNS"
                    && to == "kept-target"),
            "the unrelated edge must still survive: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PR #363: a vault target whose logical path collides with an ingested
    /// symbol must remain the exact vault-note id — in the declared metadata
    /// and in the materialised edge — so export renders one link and the
    /// reimport fingerprint is stable.
    #[tokio::test]
    async fn vault_targets_survive_symbol_path_collisions() {
        let root =
            std::env::temp_dir().join(format!("ns-vault-exact-targets-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_exact_targets";
        store.init(ns).await.expect("init");

        let generation = store.begin_generation(ns, "code").await.expect("begin");
        let symbol_id =
            crate::parser::ingest::generation_scoped_id(ns, generation, "shared-target");
        store
            .upsert(
                ns,
                &symbol_id,
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "an ingested symbol".to_string(),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "symbol".to_string(),
                    agent_name: Some("auto-ingestor".to_string()),
                    location: "shared-target".to_string(),
                    location_lines: "".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed colliding symbol");
        store
            .publish_generation(ns, "code", generation)
            .await
            .expect("publish");

        let config = vault_config(&root, &vault, ns);
        let mut linker = note("linker", &["shared-target"]);
        linker.content = "links the vault note".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        let mut target = note("shared-target", &[]);
        target.content = "vault note body".to_string();
        target.canonical_sha256 = target.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("shared-target.md"), target.to_markdown()).expect("write target");

        let result = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let (_, head) = store
            .get(ns, "linker")
            .await
            .expect("get head")
            .expect("head exists");
        assert_eq!(
            head.metadata["related_to"],
            serde_json::json!(["shared-target"]),
            "a vault target must stay an exact note id"
        );
        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            edges.iter().any(|(from, rel, to, _)| from == "linker"
                && rel == "RELATES_TO"
                && to == "shared-target"),
            "the edge must target the exact vault note: {:?}",
            edges
        );
        assert!(
            !edges.iter().any(|(_, _, to, _)| to == &symbol_id),
            "the ingested symbol must not be linked: {:?}",
            edges
        );

        export_vault(config.clone(), store.clone())
            .await
            .expect("export");
        let markdown = std::fs::read_to_string(vault.join("linker.md")).expect("read export");
        assert_eq!(
            markdown.matches("[[shared-target]]").count(),
            1,
            "{markdown}"
        );
        assert!(
            !markdown.contains(&format!("[[{symbol_id}]]")),
            "export must not render the symbol as a second link: {markdown}"
        );

        let again = import_vault(config, store, std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert_eq!(
            again.notes_imported, 0,
            "the exact-id round-trip must keep the fingerprint stable: {:?}",
            again.errors
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PR #363: a same-namespace qualified WikiLink (`<ns>::z`) names the
    /// vault note `z`, which is stored bare. A same-path ingested symbol must
    /// not capture it just because the note has not been written yet.
    #[tokio::test]
    async fn qualified_wikilink_binds_exact_note_before_symbol_resolution() {
        let root = std::env::temp_dir().join(format!(
            "ns-vault-qualified-targets-{}",
            uuid::Uuid::new_v4()
        ));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_qualified_targets";
        store.init(ns).await.expect("init");

        let generation = store.begin_generation(ns, "code").await.expect("begin");
        let symbol_id = crate::parser::ingest::generation_scoped_id(ns, generation, "z");
        store
            .upsert(
                ns,
                &symbol_id,
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "an ingested symbol".to_string(),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "symbol".to_string(),
                    agent_name: Some("auto-ingestor".to_string()),
                    location: "z".to_string(),
                    location_lines: "".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed colliding symbol");
        store
            .publish_generation(ns, "code", generation)
            .await
            .expect("publish");

        let qualified_target = format!("{ns}::z");
        let config = vault_config(&root, &vault, ns);
        let mut linker = note("linker", &[qualified_target.as_str()]);
        linker.content = "links the qualified vault note".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        // The link spells the note with its namespace; the note id stays bare.
        let mut target = note("z", &[]);
        target.content = "qualified vault note body".to_string();
        target.canonical_sha256 = target.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("qualified.md"), target.to_markdown()).expect("write target");

        let result = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let (_, head) = store
            .get(ns, "linker")
            .await
            .expect("get head")
            .expect("head exists");
        assert_eq!(
            head.metadata["related_to"],
            serde_json::json!([qualified_target.clone()]),
            "the declared spelling must survive for export"
        );
        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            edges
                .iter()
                .any(|(from, rel, to, _)| from == "linker" && rel == "RELATES_TO" && to == "z"),
            "the edge must target the exact vault note `z`: {:?}",
            edges
        );
        assert!(
            !edges.iter().any(|(_, _, to, _)| to == &symbol_id),
            "the same-path ingested symbol must not be linked: {:?}",
            edges
        );

        let mut export_config = config.clone();
        export_config.vault_path = root.join("exported");
        export_config.output_dir = root.join("exported");
        export_vault(export_config, store.clone())
            .await
            .expect("export");
        let markdown =
            std::fs::read_to_string(root.join("exported").join("linker.md")).expect("read export");
        assert_eq!(
            markdown.matches(&format!("[[{qualified_target}]]")).count(),
            1,
            "{markdown}"
        );
        assert!(
            !markdown.contains(&format!("[[{symbol_id}]]")),
            "export must not render the symbol as a second link: {markdown}"
        );

        let prior_sha = head.metadata["canonical_sha256"].clone();
        let again = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(
            again.notes_imported, 0,
            "the exact-id round-trip must keep the fingerprint stable"
        );
        let (_, reimported) = store
            .get(ns, "linker")
            .await
            .expect("get reimported head")
            .expect("head exists");
        assert_eq!(
            reimported.metadata["canonical_sha256"], prior_sha,
            "the linker fingerprint must not churn"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PR #363: ingestion replays every memory's declarations through
    /// `relink_edges`. A vault head carrying `exact_edge_targets` must keep its
    /// bare target when a same-path symbol is published later; replay may not
    /// infer the symbol and leave the imported edge set changed.
    #[tokio::test]
    async fn relink_preserves_exact_vault_targets_across_ingestion() {
        let root =
            std::env::temp_dir().join(format!("ns-vault-replay-targets-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_replay_targets";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        // `a` links the note `z`; the body avoids the symbol name so the
        // mention detector has nothing to infer. `z` is a real vault note.
        let mut a = note("a", &["z"]);
        a.content = "links the target note".to_string();
        a.canonical_sha256 = a.fingerprint();
        let mut z = note("z", &[]);
        z.content = "vault note body".to_string();
        z.canonical_sha256 = z.fingerprint();
        std::fs::write(vault.join("a.md"), a.to_markdown()).expect("write a");
        std::fs::write(vault.join("z.md"), z.to_markdown()).expect("write z");

        let result = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let linked = |edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)]| -> Vec<String> {
            edges
                .iter()
                .filter(|(from, rel, _, _)| from == "a" && rel == "RELATES_TO")
                .map(|(_, _, to, _)| to.clone())
                .collect()
        };
        assert_eq!(
            linked(&store.list_edges(ns).await.expect("initial edges")),
            vec!["z".to_string()],
            "the import binds the bare vault note"
        );

        // Simulate the next ingestion: a code symbol whose logical path is `z`
        // is staged, published, and the declarations are replayed.
        let generation = store.begin_generation(ns, "code").await.expect("begin");
        let symbol_id = crate::parser::ingest::generation_scoped_id(ns, generation, "z");
        store
            .upsert(
                ns,
                &symbol_id,
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "an ingested symbol".to_string(),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "symbol".to_string(),
                    agent_name: Some("auto-ingestor".to_string()),
                    location: "z".to_string(),
                    location_lines: "".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed same-path symbol");
        store
            .publish_generation(ns, "code", generation)
            .await
            .expect("publish");
        store
            .relink_edges(ns, "code")
            .await
            .expect("replay declarations");

        let edges = store.list_edges(ns).await.expect("edges after replay");
        assert_eq!(
            linked(&edges),
            vec!["z".to_string()],
            "replay must not add a second target to the exact declaration: {edges:?}"
        );
        assert!(
            !edges.iter().any(|(_, _, to, _)| to == &symbol_id),
            "replay must not infer the same-path symbol: {edges:?}"
        );

        let mut export_config = config.clone();
        export_config.vault_path = root.join("exported");
        export_config.output_dir = root.join("exported");
        export_vault(export_config, store.clone())
            .await
            .expect("export");
        let markdown =
            std::fs::read_to_string(root.join("exported").join("a.md")).expect("read export");
        assert_eq!(
            markdown.matches("[[z]]").count(),
            1,
            "export must render the one declared link: {markdown}"
        );
        assert!(
            !markdown.contains(&format!("[[{symbol_id}]]")),
            "export must not render the symbol as a link: {markdown}"
        );

        let prior_sha = store
            .get(ns, "a")
            .await
            .expect("get head")
            .expect("head exists")
            .1
            .metadata["canonical_sha256"]
            .clone();
        let again = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(
            again.notes_imported, 0,
            "the exact replay must keep the fingerprint stable"
        );
        assert_eq!(
            store
                .get(ns, "a")
                .await
                .expect("get reimported head")
                .expect("head exists")
                .1
                .metadata["canonical_sha256"],
            prior_sha,
            "the head fingerprint must not churn"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PR #363: an exported note's own id can carry the namespace and a
    /// numeric middle segment (`<ns>::7::path`). A WikiLink naming it must
    /// bind that verbatim row, not treat the prefix as an alias and mint a
    /// `7::path` stub.
    #[tokio::test]
    async fn qualified_note_id_binds_verbatim_before_alias_stripping() {
        let root =
            std::env::temp_dir().join(format!("ns-vault-verbatim-id-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        // `ns` is a valid IRI scheme, so the scoped id itself stays a legal
        // Turtle subject; a longer namespace with `_` would not.
        let ns = "ns";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        let scoped_id = format!("{ns}::7::path");
        let alias_id = "7::path";
        let mut linker = note("linker", &[scoped_id.as_str()]);
        linker.content = "links the scoped note".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        let mut target = note(&scoped_id, &[]);
        target.content = "body of the scoped note".to_string();
        target.canonical_sha256 = target.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("scoped.md"), target.to_markdown()).expect("write target");

        let result = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            store
                .get(ns, &scoped_id)
                .await
                .expect("get scoped")
                .is_some(),
            "the verbatim note must be imported"
        );
        assert!(
            store.get(ns, alias_id).await.expect("get alias").is_none(),
            "the namespace prefix must not be read as an alias: no `{alias_id}` stub"
        );
        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            edges.iter().any(|(from, rel, to, _)| from == "linker"
                && rel == "RELATES_TO"
                && to == &scoped_id),
            "the edge must bind the verbatim note id: {:?}",
            edges
        );
        assert!(
            !edges.iter().any(|(_, _, to, _)| to == alias_id),
            "no edge may bind the stripped alias: {:?}",
            edges
        );

        let mut export_config = config.clone();
        export_config.vault_path = root.join("exported");
        export_config.output_dir = root.join("exported");
        export_vault(export_config, store.clone())
            .await
            .expect("export");
        let markdown =
            std::fs::read_to_string(root.join("exported").join("linker.md")).expect("read export");
        assert_eq!(
            markdown.matches(&format!("[[{scoped_id}]]")).count(),
            1,
            "{markdown}"
        );
        assert!(
            !markdown.contains(&format!("[[{alias_id}]]")),
            "export must keep the verbatim spelling: {markdown}"
        );

        let prior_sha = store
            .get(ns, "linker")
            .await
            .expect("get head")
            .expect("head exists")
            .1
            .metadata["canonical_sha256"]
            .clone();
        let again = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(
            again.notes_imported, 0,
            "the verbatim round-trip must keep the fingerprint stable"
        );
        assert_eq!(
            store
                .get(ns, "linker")
                .await
                .expect("get reimported head")
                .expect("head exists")
                .1
                .metadata["canonical_sha256"],
            prior_sha,
            "the linker fingerprint must not churn"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PR #363: keep resolution must consult stored ids and vault ids per
    /// spelling. A stored verbatim `<ns>::z` outranks the vault-known alias
    /// `z`; otherwise reconciliation deletes the edge the upsert just wrote
    /// and an unchanged reimport can never repair it.
    #[tokio::test]
    async fn stored_verbatim_target_outranks_vault_alias_during_reconciliation() {
        let root =
            std::env::temp_dir().join(format!("ns-vault-dual-set-keep-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "ns";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        let stored_id = format!("{ns}::z");
        store
            .upsert(
                ns,
                &stored_id,
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "memory stored under its verbatim id".to_string(),
                    memory_type: "note".to_string(),
                    location: "".to_string(),
                    user_id: "default".to_string(),
                    agent_name: Some("auto-ingestor".to_string()),
                    location_lines: "".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed stored verbatim target");

        // Partial vault: only the bare `z` file and a linker naming `ns::z`.
        let mut linker = note("linker", &[stored_id.as_str()]);
        linker.content = "links the stored verbatim memory".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        let mut target = note("z", &[]);
        target.content = "partial vault body for z".to_string();
        target.canonical_sha256 = target.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("z.md"), target.to_markdown()).expect("write z");

        let result = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.success, "{result:?}");

        let linked = |edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)]| {
            edges
                .iter()
                .filter(|(from, rel, _, _)| from == "linker" && rel == "RELATES_TO")
                .map(|(_, _, to, _)| to.clone())
                .collect::<Vec<_>>()
        };
        let edges = store.list_edges(ns).await.expect("list edges");
        assert_eq!(
            linked(&edges),
            vec![stored_id.clone()],
            "reconciliation must keep the id materialization bound: {:?}",
            edges
        );
        assert!(
            !edges.iter().any(|(_, _, to, _)| to == "z"),
            "no edge may bind the alias: {:?}",
            edges
        );
        let (_, bare) = store
            .get(ns, "z")
            .await
            .expect("get bare note")
            .expect("the partial vault note exists");
        assert_eq!(
            bare.metadata.get("stub").and_then(Value::as_bool),
            Some(false),
            "the vault note itself must not become an alias stub"
        );

        let mut export_config = config.clone();
        export_config.vault_path = root.join("exported");
        export_config.output_dir = root.join("exported");
        export_vault(export_config, store.clone())
            .await
            .expect("export");
        let markdown =
            std::fs::read_to_string(root.join("exported").join("linker.md")).expect("read export");
        assert_eq!(
            markdown.matches(&format!("[[{stored_id}]]")).count(),
            1,
            "{markdown}"
        );
        assert!(!markdown.contains("[[z]]"), "{markdown}");

        let again = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(
            again.notes_imported, 0,
            "an unchanged reimport must be a no-op: {:?}",
            again.errors
        );
        let edges = store
            .list_edges(ns)
            .await
            .expect("list edges after reimport");
        assert_eq!(
            linked(&edges),
            vec![stored_id],
            "an unchanged reimport must not churn the reconciled edge set: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PR #363: a preferred vault target may be absent when its declarer is
    /// written, while an alias is already stored. The upsert binds the alias
    /// and reports no deferred edge; reconciliation then retires it. The
    /// preferred id must be retried after the whole vault exists, even though
    /// the initial lookup succeeded.
    #[tokio::test]
    async fn forward_reference_rebinds_preferred_target_after_alias_fallback() {
        let root = std::env::temp_dir().join(format!(
            "ns-vault-pending-preferred-{}",
            uuid::Uuid::new_v4()
        ));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "ns";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        // The alias is live in the store before the preferred id exists.
        store
            .upsert(
                ns,
                "z",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "memory stored under the bare alias".to_string(),
                    memory_type: "note".to_string(),
                    location: "".to_string(),
                    user_id: "default".to_string(),
                    agent_name: Some("auto-ingestor".to_string()),
                    location_lines: "".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed stored alias");

        let preferred = format!("{ns}::z");
        // `a` sorts before the target, so it is written while `ns::z` is absent.
        let mut linker = note("a", &[preferred.as_str()]);
        linker.content = "links the future verbatim note".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        let mut target = note(&preferred, &[]);
        target.content = "body of the future verbatim note".to_string();
        target.canonical_sha256 = target.fingerprint();
        std::fs::write(vault.join("a.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("target.md"), target.to_markdown()).expect("write target");

        let result = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.success, "{result:?}");

        let linked = |edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)]| {
            edges
                .iter()
                .filter(|(from, rel, _, _)| from == "a" && rel == "RELATES_TO")
                .map(|(_, _, to, _)| to.clone())
                .collect::<Vec<_>>()
        };
        let edges = store.list_edges(ns).await.expect("list edges");
        assert_eq!(
            linked(&edges),
            vec![preferred.clone()],
            "the replay must rebind the preferred id once it exists: {:?}",
            edges
        );
        assert!(
            !edges
                .iter()
                .any(|(from, _, to, _)| from == "a" && to == "z"),
            "reconciliation must not leave the alias edge: {:?}",
            edges
        );

        let mut export_config = config.clone();
        export_config.vault_path = root.join("exported");
        export_config.output_dir = root.join("exported");
        export_vault(export_config, store.clone())
            .await
            .expect("export");
        let markdown =
            std::fs::read_to_string(root.join("exported").join("a.md")).expect("read export");
        assert_eq!(
            markdown.matches(&format!("[[{preferred}]]")).count(),
            1,
            "{markdown}"
        );
        assert!(!markdown.contains("[[z]]"), "{markdown}");

        let again = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("reimport");
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(
            again.notes_imported, 0,
            "an unchanged reimport must be a no-op: {:?}",
            again.errors
        );
        let edges = store
            .list_edges(ns)
            .await
            .expect("list edges after reimport");
        assert_eq!(
            linked(&edges),
            vec![preferred],
            "an unchanged reimport must not churn the reconciled edge set: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A declarer whose fingerprint is unchanged skips planning entirely. A
    /// preferred target that only arrives in a later turn therefore never
    /// reaches it, and its edge stays on the alias fallback forever. Rebinding
    /// must not mint a second content version of the declarer.
    #[tokio::test]
    async fn unchanged_declarer_rebinds_when_preferred_target_arrives() {
        let root = std::env::temp_dir().join(format!(
            "ns-vault-unchanged-rebind-{}",
            uuid::Uuid::new_v4()
        ));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "ns";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        // The bare alias exists before the declarer's preferred id does.
        store
            .upsert(
                ns,
                "z",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: "memory stored under the bare alias".to_string(),
                    memory_type: "note".to_string(),
                    location: "".to_string(),
                    user_id: "default".to_string(),
                    agent_name: Some("auto-ingestor".to_string()),
                    location_lines: "".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed stored alias");

        let preferred = format!("{ns}::z");
        let mut linker = note("a", &[preferred.as_str()]);
        linker.content = "links the alias until the verbatim note exists".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        let mut alias = note("z", &[]);
        alias.content = "body of the bare alias".to_string();
        alias.canonical_sha256 = alias.fingerprint();
        std::fs::write(vault.join("a.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("z.md"), alias.to_markdown()).expect("write alias");

        let first = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(first.success, "{first:?}");

        let linked = |edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)]| {
            edges
                .iter()
                .filter(|(from, rel, _, _)| from == "a" && rel == "RELATES_TO")
                .map(|(_, _, to, _)| to.clone())
                .collect::<Vec<_>>()
        };
        let edges = store.list_edges(ns).await.expect("list edges");
        assert_eq!(
            linked(&edges),
            vec!["z".to_string()],
            "the first import binds the stored alias: {:?}",
            edges
        );

        let (_, before) = store
            .get(ns, "a")
            .await
            .expect("get declarer")
            .expect("declarer exists");

        // Second turn: the literal preferred note arrives; `a` is untouched.
        let mut literal = note(&preferred, &[]);
        literal.content = "body of the verbatim note".to_string();
        literal.canonical_sha256 = literal.fingerprint();
        std::fs::write(vault.join("preferred.md"), literal.to_markdown()).expect("write preferred");

        let second = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("reimport");
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert!(second.success, "{second:?}");
        assert_eq!(
            second.notes_imported, 1,
            "only the preferred note is new: {:?}",
            second.errors
        );

        let (_, after) = store
            .get(ns, "a")
            .await
            .expect("get declarer after rebind")
            .expect("declarer survives");
        assert_eq!(
            after.content, before.content,
            "head content must not change"
        );
        assert_eq!(
            after.memory_type, before.memory_type,
            "head type must not change"
        );
        assert_eq!(
            after.metadata.get("canonical_sha256"),
            before.metadata.get("canonical_sha256"),
            "head identity must not change"
        );
        assert_eq!(
            after.metadata.get("supersedes"),
            None,
            "an edge rebind must not supersede the head: {:?}",
            after.metadata
        );
        let history_rows = store
            .list(ns, None, None)
            .await
            .expect("list rows")
            .into_iter()
            .filter(|row| row.id.starts_with("a.vault-history."))
            .count();
        assert_eq!(history_rows, 0, "no history version may be created");

        let edges = store.list_edges(ns).await.expect("list edges after rebind");
        assert_eq!(
            linked(&edges),
            vec![preferred.clone()],
            "the unchanged declarer must rebind its preferred target: {:?}",
            edges
        );

        let mut export_config = config.clone();
        export_config.vault_path = root.join("exported");
        export_config.output_dir = root.join("exported");
        export_vault(export_config, store.clone())
            .await
            .expect("export");
        let markdown =
            std::fs::read_to_string(root.join("exported").join("a.md")).expect("read export");
        assert_eq!(
            markdown.matches(&format!("[[{preferred}]]")).count(),
            1,
            "{markdown}"
        );
        assert!(!markdown.contains("[[z]]"), "{markdown}");

        let third = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("third import");
        assert!(third.errors.is_empty(), "{:?}", third.errors);
        assert_eq!(
            third.notes_imported, 0,
            "a stable rebind must not churn the head: {:?}",
            third.errors
        );
        let edges = store
            .list_edges(ns)
            .await
            .expect("list edges after third import");
        assert_eq!(
            linked(&edges),
            vec![preferred],
            "a third unchanged import must be a stable no-op: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn relates_to_targets(
        edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)],
        from: &str,
    ) -> Vec<String> {
        edges
            .iter()
            .filter(|(source, rel, _, _)| source == from && rel == "RELATES_TO")
            .map(|(_, _, target, _)| target.clone())
            .collect()
    }

    /// Seed an unchanged declarer whose head is already bound to the bare `z`
    /// alias while its note declares the preferred `ns::z`. The red turn adds
    /// the literal preferred note to a vault that also holds `a` byte-identical.
    async fn seed_unchanged_alias_declarer(
        label: &str,
        ns: &str,
        owner: &str,
        mesh: Option<serde_json::Value>,
    ) -> (
        std::sync::Arc<dyn VectorStore>,
        VaultSyncConfig,
        String,
        crate::traits::MemoryPayload,
    ) {
        let root = std::env::temp_dir().join(format!("ns-vault-{label}-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        store.init(ns).await.expect("init");
        let mut config = vault_config(&root, &vault, ns);
        config.principal = Some("me".to_string());

        let preferred = format!("{ns}::z");
        let mut linker = note("a", &[preferred.as_str()]);
        linker.content = "links the alias until the verbatim note exists".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        let mut alias = note("z", &[]);
        alias.content = "body of the bare alias".to_string();
        alias.canonical_sha256 = alias.fingerprint();
        std::fs::write(vault.join("a.md"), linker.to_markdown()).expect("write linker");
        std::fs::write(vault.join("z.md"), alias.to_markdown()).expect("write alias");

        store
            .upsert(
                ns,
                "z",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: alias.content.clone(),
                    memory_type: "fact".to_string(),
                    location: String::new(),
                    user_id: "me".to_string(),
                    agent_name: None,
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "canonical_sha256": alias.canonical_sha256,
                        "fingerprint_version": FINGERPRINT_VERSION,
                        "exact_edge_targets": true,
                    }),
                },
            )
            .await
            .expect("seed alias");

        let mut metadata = serde_json::json!({
            "canonical_sha256": linker.canonical_sha256,
            "fingerprint_version": FINGERPRINT_VERSION,
            "related_to": ["z"],
            "exact_edge_targets": true,
        });
        if let Some(mesh) = mesh {
            metadata["mesh"] = mesh;
        }
        store
            .upsert(
                ns,
                "a",
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: linker.content.clone(),
                    memory_type: "fact".to_string(),
                    location: String::new(),
                    user_id: owner.to_string(),
                    agent_name: None,
                    location_lines: String::new(),
                    metadata,
                },
            )
            .await
            .expect("seed declarer");
        let before = store
            .get(ns, "a")
            .await
            .expect("get declarer")
            .expect("declarer exists")
            .1;

        let mut literal = note(&preferred, &[]);
        literal.content = "body of the verbatim note".to_string();
        literal.canonical_sha256 = literal.fingerprint();
        std::fs::write(vault.join("preferred.md"), literal.to_markdown()).expect("write preferred");

        (store, config, preferred, before)
    }

    async fn assert_declarer_head_unchanged(
        store: &std::sync::Arc<dyn VectorStore>,
        ns: &str,
        before: &crate::traits::MemoryPayload,
    ) {
        let after = store
            .get(ns, "a")
            .await
            .expect("get declarer after import")
            .expect("declarer survives")
            .1;
        assert_eq!(
            after.content, before.content,
            "head content must not change"
        );
        assert_eq!(
            after.memory_type, before.memory_type,
            "head type must not change"
        );
        assert_eq!(after.user_id, before.user_id, "head owner must not change");
        assert_eq!(
            after.metadata, before.metadata,
            "an unchanged declarer's head must not be rewritten"
        );
        assert_eq!(
            after.metadata.get("supersedes"),
            None,
            "an edge rebind must not supersede the head: {:?}",
            after.metadata
        );
        let history_rows = store
            .list(ns, None, None)
            .await
            .expect("list rows")
            .into_iter()
            .filter(|row| row.id.starts_with("a.vault-history."))
            .count();
        assert_eq!(history_rows, 0, "no history version may be created");
    }

    /// A foreign unchanged declarer is protected ground: the import may not
    /// rebind its edges merely because its preferred target arrived.
    #[tokio::test]
    async fn unchanged_foreign_declarer_does_not_rebind_protected_edges() {
        let ns = "vaultforeign";
        let (store, config, preferred, before) =
            seed_unchanged_alias_declarer("foreign-unchanged", ns, "peer-a", None).await;
        let root = config.project_root.clone().expect("root");
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);

        assert_declarer_head_unchanged(&store, ns, &before).await;
        let edges = store.list_edges(ns).await.expect("list edges");
        assert_eq!(
            relates_to_targets(&edges, "a"),
            vec!["z".to_string()],
            "a protected declarer must keep its alias edge: {:?}",
            edges
        );
        assert!(
            !relates_to_targets(&edges, "a").contains(&preferred),
            "no preferred edge may be created for a protected declarer: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Mesh-provenance sovereignty applies even when the head's owner matches
    /// the importing principal: the mesh author, not the local owner, decides.
    #[tokio::test]
    async fn unchanged_mesh_owned_declarer_does_not_rebind_protected_edges() {
        let ns = "vaultmesh";
        let (store, config, preferred, before) = seed_unchanged_alias_declarer(
            "mesh-unchanged",
            ns,
            "me",
            Some(serde_json::json!({
                "origin": "mesh-import",
                "claimed_created_by": "peer-a",
                "created_by_verified": false
            })),
        )
        .await;
        let root = config.project_root.clone().expect("root");
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);

        assert_declarer_head_unchanged(&store, ns, &before).await;
        let edges = store.list_edges(ns).await.expect("list edges");
        assert_eq!(
            relates_to_targets(&edges, "a"),
            vec!["z".to_string()],
            "a mesh-owned declarer must keep its alias edge: {:?}",
            edges
        );
        assert!(
            !relates_to_targets(&edges, "a").contains(&preferred),
            "no preferred edge may be created for a mesh-owned declarer: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Control: the ownership gate must not stop an owned unchanged declarer
    /// from following its newly imported preferred target.
    #[tokio::test]
    async fn owned_unchanged_declarer_still_rebinds_preferred_target() {
        let ns = "vaultowned";
        let (store, config, preferred, before) =
            seed_unchanged_alias_declarer("owned-unchanged", ns, "me", None).await;
        let root = config.project_root.clone().expect("root");
        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);

        assert_declarer_head_unchanged(&store, ns, &before).await;
        let edges = store.list_edges(ns).await.expect("list edges");
        assert_eq!(
            relates_to_targets(&edges, "a"),
            vec![preferred],
            "an owned unchanged declarer must rebind its preferred target: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Delegating store that fails one head upsert, so the caller can prove a
    /// failed reimport never leaves edges deleted under a surviving old head.
    struct FailingHeadUpsertStore {
        inner: std::sync::Arc<dyn VectorStore>,
        fail_id: &'static str,
    }

    impl crate::traits::AsAny for FailingHeadUpsertStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[async_trait::async_trait]
    impl VectorStore for FailingHeadUpsertStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }

        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: crate::traits::MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            if id == self.fail_id {
                anyhow::bail!("injected head upsert failure for {id}");
            }
            self.inner.upsert(namespace, id, vector, payload).await
        }

        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }

        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }

        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }

        async fn reconcile_declared_edges(
            &self,
            namespace: &str,
            id: &str,
            rel_type: &str,
            keep: &[String],
        ) -> anyhow::Result<usize> {
            self.inner
                .reconcile_declared_edges(namespace, id, rel_type, keep)
                .await
        }

        async fn relink_edges(&self, namespace: &str, source_key: &str) -> anyhow::Result<usize> {
            self.inner.relink_edges(namespace, source_key).await
        }

        async fn begin_generation(&self, namespace: &str, source_key: &str) -> anyhow::Result<u64> {
            self.inner.begin_generation(namespace, source_key).await
        }

        async fn publish_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .publish_generation(namespace, source_key, generation)
                .await
        }

        async fn abort_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .abort_generation(namespace, source_key, generation)
                .await
        }

        async fn active_generation(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<u64> {
            self.inner.active_generation(namespace, source_key).await
        }

        async fn generation_nodes(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<String>> {
            self.inner.generation_nodes(namespace, generation).await
        }

        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner.list(namespace, user_id, memory_type).await
        }

        async fn generation_summary(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<crate::traits::GenerationRow>> {
            self.inner.generation_summary(namespace, generation).await
        }

        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, crate::traits::MemoryPayload)>> {
            self.inner.get(namespace, id).await
        }

        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }

        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }

        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }

        async fn increment_access_count(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }

        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }

        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }
    }

    /// PR #363: when the head upsert fails, an edit that removes a WikiLink
    /// must not leave the old head with its previously stored edge deleted.
    #[tokio::test]
    async fn failed_reimport_keeps_existing_edges_with_the_old_head() {
        let root = std::env::temp_dir().join(format!("ns-vault-atomic-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_atomic_edges";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        let mut linker = note("linker", &["stale-target", "kept-target"]);
        linker.content = "links both".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        for plain in [note("stale-target", &[]), note("kept-target", &[])] {
            let mut plain = plain;
            plain.content = format!("body of {}", plain.id);
            plain.canonical_sha256 = plain.fingerprint();
            std::fs::write(vault.join(format!("{}.md", plain.id)), plain.to_markdown())
                .expect("write note");
        }
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("write linker");

        let first = import_vault(
            config.clone(),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("first import");
        assert!(first.errors.is_empty(), "{:?}", first.errors);

        linker.related = vec!["kept-target".to_string()];
        linker.content = "links one".to_string();
        linker.canonical_sha256 = linker.fingerprint();
        std::fs::write(vault.join("linker.md"), linker.to_markdown()).expect("rewrite linker");

        let failing: std::sync::Arc<dyn VectorStore> =
            std::sync::Arc::new(FailingHeadUpsertStore {
                inner: store.clone(),
                fail_id: "linker",
            });
        let second = import_vault(config, failing, std::sync::Arc::new(TestEmbedder))
            .await
            .expect("failing reimport");
        assert!(!second.errors.is_empty(), "head upsert must fail");

        let (_, head) = store
            .get(ns, "linker")
            .await
            .expect("get head")
            .expect("old head survives");
        let mut related: Vec<String> = head.metadata["related_to"]
            .as_array()
            .expect("related_to array")
            .iter()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();
        related.sort();
        assert_eq!(
            related,
            vec!["kept-target".to_string(), "stale-target".to_string()],
            "the old head must still declare its original links"
        );
        let edges = store.list_edges(ns).await.expect("list edges");
        assert!(
            edges.iter().any(|(from, rel, to, _)| from == "linker"
                && rel == "RELATES_TO"
                && to == "stale-target"),
            "a failed head upsert must not retire an edge under the old head: {:?}",
            edges
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #344: retired rows must not be presented as current knowledge.
    #[tokio::test]
    async fn export_excludes_retired_rows_and_keeps_canvas_coherent() {
        let root = std::env::temp_dir().join(format!("ns-vault-retired-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_retired";
        store.init(ns).await.expect("init");
        let namespace = ns.to_string();
        let seed = |id: &'static str, metadata: serde_json::Value, content: &'static str| {
            let store = store.clone();
            let namespace = namespace.clone();
            async move {
                store
                    .upsert(
                        &namespace,
                        id,
                        vec![0.0; 4],
                        crate::traits::MemoryPayload {
                            content: content.to_string(),
                            user_id: "default".to_string(),
                            memory_type: "rule".to_string(),
                            agent_name: None,
                            location: String::new(),
                            location_lines: String::new(),
                            metadata,
                        },
                    )
                    .await
                    .expect("seed row");
            }
        };
        let past = chrono::Utc::now().timestamp() - 60;
        let future = chrono::Utc::now().timestamp() + 3600;
        seed("live-rule", serde_json::json!({}), "live body").await;
        seed(
            "expired-rule",
            serde_json::json!({"valid_to": past, "superseded_by": "live-rule"}),
            "expired body",
        )
        .await;
        seed(
            "superseded-rule",
            serde_json::json!({"superseded_by": "live-rule"}),
            "superseded body",
        )
        .await;
        seed(
            "future-rule",
            serde_json::json!({"valid_to": future}),
            "future body",
        )
        .await;
        seed(
            "live-rule.vault-history.0000",
            serde_json::json!({"valid_to": past, "superseded_by": "live-rule"}),
            "history body",
        )
        .await;

        let config = vault_config(&root, &vault, ns);
        let exported = export_vault(config, store).await.expect("export");
        assert!(exported.success, "{:?}", exported.errors);
        assert_eq!(exported.notes_exported, 2, "{:?}", exported.errors);
        assert!(vault.join("live-rule.md").exists());
        assert!(vault.join("future-rule.md").exists());
        for id in [
            "expired-rule",
            "superseded-rule",
            "live-rule.vault-history.0000",
        ] {
            assert!(
                !vault.join(format!("{id}.md")).exists(),
                "retired row {id} must not be exported"
            );
        }
        let canvas: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(vault.join("topology.canvas")).unwrap())
                .expect("canvas json");
        assert_eq!(
            canvas["nodes"].as_array().map(Vec::len),
            Some(2),
            "canvas must describe the exported set: {canvas}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #344: a stale export's history file must not become live knowledge.
    #[tokio::test]
    async fn import_ignores_stale_vault_history_files() {
        let root = std::env::temp_dir().join(format!("ns-vault-stale-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_stale_history";
        store.init(ns).await.expect("init");
        let config = vault_config(&root, &vault, ns);

        let history_id = "stale-note.vault-history.6f0f0f0f-0000-4000-8000-000000000000";
        let mut stale = note(history_id, &[]);
        stale.content = "obsolete body".to_string();
        stale.canonical_sha256 = stale.fingerprint();
        std::fs::write(vault.join(format!("{history_id}.md")), stale.to_markdown())
            .expect("write stale history");
        let mut live = note("stale-note", &[]);
        live.content = "current body".to_string();
        live.canonical_sha256 = live.fingerprint();
        std::fs::write(vault.join("stale-note.md"), live.to_markdown()).expect("write live");

        let result = import_vault(config, store.clone(), std::sync::Arc::new(TestEmbedder))
            .await
            .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notes_imported, 1, "{:?}", result.errors);
        assert!(
            store.get(ns, history_id).await.unwrap().is_none(),
            "history ids are importer projections, never vault-authored memories"
        );
        assert!(store.get(ns, "stale-note").await.unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #345: seed the exact shape a pre-v2 import leaves behind.
    async fn seed_legacy_head(
        store: &std::sync::Arc<dyn VectorStore>,
        ns: &str,
        id: &str,
        body: &str,
        memory_type: &str,
        legacy_hash: &str,
    ) {
        store
            .upsert(
                ns,
                id,
                vec![0.0; 4],
                crate::traits::MemoryPayload {
                    content: body.to_string(),
                    user_id: "default".to_string(),
                    memory_type: memory_type.to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({
                        "vault_source": true,
                        "stub": false,
                        "canonical_sha256": legacy_hash,
                        "related": [],
                        "valid_from": chrono::Utc::now().timestamp() - 60
                    }),
                },
            )
            .await
            .expect("seed legacy head");
    }

    /// #345: a type-only edit must not be swallowed as "unchanged".
    #[tokio::test]
    async fn type_only_edit_creates_new_head_version() {
        let root = std::env::temp_dir().join(format!("ns-vault-type-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_type_edit";
        store.init(ns).await.expect("init");

        let mut before = note("type-note", &[]);
        before.content = "same body".to_string();
        before.canonical_sha256 = legacy_fingerprint(&before);
        seed_legacy_head(
            &store,
            ns,
            "type-note",
            "same body",
            "fact",
            &before.canonical_sha256,
        )
        .await;

        let mut after = before.clone();
        after.rdf_type = "yaml:rule".to_string();
        after.canonical_sha256 = after.fingerprint();
        std::fs::write(vault.join("type-note.md"), after.to_markdown()).expect("write note");

        let result = import_vault(
            vault_config(&root, &vault, ns),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.notes_imported, 1,
            "a type-only edit must be imported: {:?}",
            result.errors
        );
        let (_, head) = store.get(ns, "type-note").await.unwrap().unwrap();
        assert_eq!(head.memory_type, "rule");
        assert_eq!(head.metadata["fingerprint_version"], serde_json::json!(2));
        assert!(
            head.metadata.get("supersedes").is_some(),
            "the edit must create a history version"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #345: upgrading identity must not re-version unchanged legacy heads.
    #[tokio::test]
    async fn legacy_v1_head_reimport_is_not_mass_versioned() {
        let root = std::env::temp_dir().join(format!("ns-vault-legacy-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_legacy_noop";
        store.init(ns).await.expect("init");

        let mut unchanged = note("legacy-note", &[]);
        unchanged.content = "legacy body".to_string();
        unchanged.canonical_sha256 = legacy_fingerprint(&unchanged);
        seed_legacy_head(
            &store,
            ns,
            "legacy-note",
            "legacy body",
            "fact",
            &unchanged.canonical_sha256,
        )
        .await;
        std::fs::write(vault.join("legacy-note.md"), unchanged.to_markdown()).expect("write note");

        let result = import_vault(
            vault_config(&root, &vault, ns),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("import");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.notes_imported, 0,
            "v1 -> v2 must not mass-version: {:?}",
            result.errors
        );
        let (_, head) = store.get(ns, "legacy-note").await.unwrap().unwrap();
        assert!(head.metadata.get("supersedes").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #345: once stamped v2, a byte-identical reimport remains a no-op.
    #[tokio::test]
    async fn unchanged_v2_reimport_is_a_noop() {
        let root = std::env::temp_dir().join(format!("ns-vault-v2-{}", uuid::Uuid::new_v4()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).expect("create vault");
        let store: std::sync::Arc<dyn VectorStore> = std::sync::Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(root.join("db"), 4)
                .expect("temp store"),
        );
        let ns = "vault_v2_noop";
        store.init(ns).await.expect("init");
        let mut body = note("v2-note", &[]);
        body.content = "v2 body".to_string();
        body.canonical_sha256 = body.fingerprint();
        std::fs::write(vault.join("v2-note.md"), body.to_markdown()).expect("write note");

        let first = import_vault(
            vault_config(&root, &vault, ns),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("first import");
        assert_eq!(first.notes_imported, 1, "{:?}", first.errors);
        let second = import_vault(
            vault_config(&root, &vault, ns),
            store.clone(),
            std::sync::Arc::new(TestEmbedder),
        )
        .await
        .expect("second import");
        assert_eq!(second.notes_imported, 0, "{:?}", second.errors);
        let (_, head) = store.get(ns, "v2-note").await.unwrap().unwrap();
        assert_eq!(head.metadata["fingerprint_version"], serde_json::json!(2));
        assert!(head.metadata.get("supersedes").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
