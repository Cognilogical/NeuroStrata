//! `neurostrata-mcp root audit`: the stored paths of a namespace's active
//! memories that a root marker would spell differently, and the exact
//! `neurostrata_supersede_memory` call that corrects each memory.
//!
//! Read-only by construction: it lists rows and never writes. It reads a whole
//! namespace, so it stays on the CLI (AGENTS.md section 5) and is never an MCP
//! tool.

use crate::paths::{relative_to_on_disk, to_project_relative, RootSource};
use crate::traits::{SearchResult, VectorStore};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The `reason` every generated supersede call records.
pub const SUPERSEDE_REASON: &str = "path made project-relative";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuditReport {
    pub fixable: Vec<FixableRow>,
    pub needs_decision: Vec<UndecidedRow>,
}

/// One memory and the ready-to-send `neurostrata_supersede_memory` arguments
/// that correct it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FixableRow {
    pub id: String,
    pub supersede_args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndecidedRow {
    pub id: String,
    pub path: String,
    pub why: UndecidedWhy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UndecidedWhy {
    /// A relative path that names no file or folder under the root: a moved or
    /// deleted file, or a key relative to another root.
    NoSuchFile,
    /// A path whose `source_context.source_id` is bound to a project context
    /// whose root is not this one.
    OtherSourceRoot,
    /// A `location` that is not the first ref's path once both are spelled
    /// relative to the root: supersede sets `location` from the first of
    /// `locations`, so the call would replace it.
    LocationDiffersFromFirstRef,
    /// `--for-root`: a relative path that names a file under the current root
    /// that lies outside the planned root, so no key under the planned root
    /// names it.
    OutsidePlannedRoot,
}

impl UndecidedWhy {
    pub fn describe(self) -> &'static str {
        match self {
            UndecidedWhy::NoSuchFile => {
                "names no file under the root (moved, deleted, or relative to another root)"
            }
            UndecidedWhy::OtherSourceRoot => "its source_id is bound to a different root",
            UndecidedWhy::LocationDiffersFromFirstRef => {
                "location differs from the first ref; supersede would replace it with the first ref's path"
            }
            UndecidedWhy::OutsidePlannedRoot => {
                "names a file under the current root that is outside the planned root"
            }
        }
    }
}

impl AuditReport {
    /// 0 when nothing is fixable, else 1; the other group never counts.
    pub fn exit_code(&self) -> i32 {
        if self.fixable.is_empty() {
            0
        } else {
            1
        }
    }

    /// The human-readable report: one JSON line per fixable memory, then one
    /// line per path that needs a decision.
    pub fn to_text(&self) -> String {
        let mut out = vec![format!(
            "Fixable ({}): apply each line as a neurostrata_supersede_memory call",
            self.fixable.len()
        )];
        out.push(
            "  note: supersede has no location argument, so a call for a memory with only a \
             location also stores that path as its first ref"
                .to_string(),
        );
        if self.fixable.is_empty() {
            out.push("  none".to_string());
        }
        for row in &self.fixable {
            out.push(row.supersede_args.to_string());
        }
        out.push(String::new());
        out.push(format!(
            "Needs your decision ({}): no command is generated",
            self.needs_decision.len()
        ));
        if self.needs_decision.is_empty() {
            out.push("  none".to_string());
        }
        for row in &self.needs_decision {
            out.push(format!(
                "  {}  {}  {}",
                row.id,
                row.path,
                row.why.describe()
            ));
        }
        out.join("\n")
    }
}

/// Rows the audit reads: live memories. Ingested code and document nodes are
/// corrected by re-ingesting, tasks and receipts are not memories to
/// supersede, and retired, superseded or archived rows are history.
fn is_auditable(row: &SearchResult, now: i64) -> bool {
    let payload = &row.payload;
    if matches!(
        payload.memory_type.as_str(),
        "task"
            | "action_receipt"
            | "code_ast"
            | "symbol"
            | "file"
            | "doc_section"
            | "directory"
            | "markdown"
    ) {
        return false;
    }
    let meta = &payload.metadata;
    if crate::lexical::archived_flag(meta) {
        return false;
    }
    if meta
        .get("superseded_by")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return false;
    }
    if let Some(valid_to) = meta.get("valid_to").and_then(Value::as_i64) {
        if valid_to > 0 && valid_to <= now {
            return false;
        }
    }
    true
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A `location` that names a path, not a placeholder.
fn location_is_path(location: &str) -> bool {
    let trimmed = location.trim();
    !trimmed.is_empty() && trimmed != "global" && trimmed != "unknown"
}

/// A `governs` entry that names a path rather than a memory id, a symbol id or
/// a domain (the rule `related` applies to its seeds). A bare name without a
/// separator (`Cargo.toml`) is a path when a file of that name exists under
/// one of `roots`.
fn governs_is_path(entry: &str, roots: &[&Path]) -> bool {
    if crate::paths::is_absolute_any(entry) {
        return true;
    }
    if entry.contains("::") || entry.starts_with("file:") {
        return false;
    }
    if entry.contains('/') || entry.contains('\\') {
        return true;
    }
    let name = entry.trim();
    !name.is_empty() && roots.iter().any(|root| root.join(name).is_file())
}

fn ends_with_separator(raw: &str) -> bool {
    raw.ends_with('/') || raw.ends_with('\\')
}

/// Split a qualified key into the prefix to preserve and the logical path the
/// audit judges. Only the audited namespace is stripped, plus one numeric
/// generation: a namespace may itself contain `::`, so splitting on any `::`
/// would mistake part of the namespace for a separator and the key would be
/// neither counted nor rewritten.
fn split_qualified<'a>(entry: &'a str, namespace: &str) -> (&'a str, &'a str) {
    let sep = crate::parser::ingest::NAMESPACE_SEPARATOR;
    let mut prefix = "";
    let mut rest = entry;
    if let Some(stripped) = entry
        .strip_prefix(namespace)
        .and_then(|r| r.strip_prefix(sep))
        .filter(|r| !r.is_empty())
    {
        prefix = &entry[..entry.len() - stripped.len()];
        rest = stripped;
    }
    if let Some((head, tail)) = rest.split_once(sep) {
        if head.parse::<u64>().is_ok() {
            prefix = &entry[..entry.len() - tail.len()];
            rest = tail;
        }
    }
    (prefix, rest)
}

/// The roots an audit judges paths against: the current root, whose files
/// the stored relative keys name today, and the root the corrections are
/// spelled relative to (the same one unless `--for-root` names a planned
/// root).
struct Roots<'a> {
    current: &'a Path,
    target: &'a Path,
    target_str: String,
    planned: bool,
}

impl<'a> Roots<'a> {
    fn new(current: &'a Path, planned: Option<&'a Path>) -> Self {
        let target = planned.unwrap_or(current);
        Roots {
            current,
            target,
            target_str: target.to_string_lossy().to_string(),
            planned: planned.is_some_and(|p| !crate::root_marker::same_folder(p, current)),
        }
    }

    fn both(&self) -> [&Path; 2] {
        [self.current, self.target]
    }
}

/// What one stored path needs.
enum Verdict {
    /// This spelling, relative to the target root.
    Rewrite(String),
    /// Relative and names nothing under the root.
    Missing,
    /// Relative, names a file under the current root, but that file is
    /// outside the planned root.
    OutsideTarget,
    /// Already fine, or not this audit's business (absolute outside the root).
    Keep,
}

fn judge(raw: &str, roots: &Roots) -> Verdict {
    if crate::paths::is_absolute_any(raw) {
        let lookup = to_project_relative(raw, Some(&roots.target_str), None);
        if lookup.source != RootSource::ProjectRoot || lookup.path.is_empty() {
            return Verdict::Keep;
        }
        let mut rel = lookup.path;
        if ends_with_separator(raw) && !rel.ends_with('/') {
            rel.push('/');
        }
        return Verdict::Rewrite(rel);
    }
    let bare = raw.split('#').next().unwrap_or(raw);
    let key = crate::parser::ingest::normalize_node_path(bare);
    if key.is_empty() {
        return Verdict::Keep;
    }
    let under_current = roots.current.join(&key);
    if !roots.planned {
        return if under_current.exists() {
            Verdict::Keep
        } else {
            Verdict::Missing
        };
    }
    // A planned root: a key that names a file under the current root is
    // spelled relative to the planned root; one that does not, but names a
    // file under the planned root, already fits it.
    if under_current.exists() {
        return match relative_to_on_disk(&under_current.to_string_lossy(), &roots.target_str) {
            Some(rel) if rel == key => Verdict::Keep,
            Some(mut rel) => {
                if ends_with_separator(bare) && !rel.ends_with('/') {
                    rel.push('/');
                }
                rel.push_str(&raw[bare.len()..]);
                Verdict::Rewrite(rel)
            }
            None => Verdict::OutsideTarget,
        };
    }
    if roots.target.join(&key).exists() {
        Verdict::Keep
    } else {
        Verdict::Missing
    }
}

/// `raw` as the call would store it: the rewritten spelling, else the stored
/// one, normalized for comparison.
fn spelled_key(raw: &str, roots: &Roots) -> String {
    match judge(raw, roots) {
        Verdict::Rewrite(rel) => crate::parser::ingest::normalize_node_path(&rel),
        _ => crate::parser::ingest::normalize_node_path(raw),
    }
}

/// The root a ref's `source_context.source_id` is bound to, when it is bound
/// to one that is not `root`.
fn bound_elsewhere(reference: &Value, root: &Path, bindings: &HashMap<String, PathBuf>) -> bool {
    let Some(source_id) = reference
        .get("source_context")
        .and_then(|c| c.get("source_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let Some(context_dir) = bindings.get(source_id) else {
        return false;
    };
    // A binding names the `.NeuroStrata` folder; its parent is the root.
    let bound_root = context_dir.parent().unwrap_or(context_dir);
    !crate::root_marker::same_folder(bound_root, root)
}

/// Audit rows already read from a namespace. Rows are taken as given apart
/// from the active filter, so a caller that read them through the daemon gets
/// the same report as [`audit_namespace`].
pub fn audit_rows(
    rows: &[SearchResult],
    namespace: &str,
    root: &Path,
    bindings: &HashMap<String, PathBuf>,
) -> AuditReport {
    audit_rows_for(rows, namespace, root, None, bindings)
}

/// [`audit_rows`] with corrections spelled relative to `planned` (`root
/// audit --for-root`) when given: keys that name files under `root` today
/// are rewritten to the keys the planned root produces for them, so applying
/// the calls and then moving the root there loses nothing.
pub fn audit_rows_for(
    rows: &[SearchResult],
    namespace: &str,
    root: &Path,
    planned: Option<&Path>,
    bindings: &HashMap<String, PathBuf>,
) -> AuditReport {
    let roots = Roots::new(root, planned);
    let now = now_secs();
    let mut fixable = Vec::new();
    let mut needs_decision: Vec<UndecidedRow> = Vec::new();
    let mut sorted: Vec<&SearchResult> = rows.iter().filter(|r| is_auditable(r, now)).collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    for row in sorted {
        let payload = &row.payload;
        let meta = &payload.metadata;
        let mut undecided: Vec<UndecidedRow> = Vec::new();
        let note = |path: &str, why: UndecidedWhy, list: &mut Vec<UndecidedRow>| {
            let entry = UndecidedRow {
                id: row.id.clone(),
                path: path.to_string(),
                why,
            };
            if !list.contains(&entry) {
                list.push(entry);
            }
        };
        let mut rewrote = false;
        let mut other_root = false;
        let mut other_root_paths: Vec<String> = Vec::new();
        let mut location_conflict = false;

        // `locations`: the refs, each kept whole with only its path rewritten,
        // or the bare location when the memory has no refs.
        let refs = meta
            .get("refs")
            .and_then(Value::as_array)
            .filter(|r| !r.is_empty());
        let mut locations: Option<Vec<Value>> = None;
        if let Some(refs) = refs {
            let mut out = Vec::with_capacity(refs.len());
            let mut first_raw: Option<Option<String>> = None;
            for reference in refs {
                let mut obj: Map<String, Value> =
                    reference.as_object().cloned().unwrap_or_default();
                // The stored anchor is `file`; supersede reads `path`.
                let raw = obj
                    .remove("file")
                    .or_else(|| obj.remove("path"))
                    .and_then(|v| v.as_str().map(str::to_string));
                if first_raw.is_none() {
                    first_raw = Some(raw.clone());
                }
                if let Some(raw) = raw {
                    let mut spelled = raw.clone();
                    if bound_elsewhere(reference, root, bindings) {
                        other_root = true;
                        other_root_paths.push(crate::parser::ingest::normalize_node_path(&raw));
                        note(&raw, UndecidedWhy::OtherSourceRoot, &mut undecided);
                    } else {
                        match judge(&raw, &roots) {
                            Verdict::Rewrite(rel) => {
                                rewrote = true;
                                spelled = rel;
                            }
                            Verdict::Missing => {
                                note(&raw, UndecidedWhy::NoSuchFile, &mut undecided)
                            }
                            Verdict::OutsideTarget => {
                                note(&raw, UndecidedWhy::OutsidePlannedRoot, &mut undecided)
                            }
                            Verdict::Keep => {}
                        }
                    }
                    obj.insert("path".to_string(), Value::String(spelled));
                }
                out.push(Value::Object(obj));
            }
            locations = Some(out);
            // Supersede sets `location` from the first of `locations`. A
            // location that is a path and differs from the first ref's path
            // (both spelled relative to the root) would be replaced, so a
            // memory that would get a call is left for the user to decide.
            let location = payload.location.as_str();
            if location_is_path(location) {
                let same = first_raw.flatten().is_some_and(|first| {
                    spelled_key(&first, &roots) == spelled_key(location, &roots)
                });
                location_conflict = !same;
            }
        } else if location_is_path(&payload.location) {
            let raw = payload.location.clone();
            let mut spelled = raw.clone();
            match judge(&raw, &roots) {
                Verdict::Rewrite(rel) => {
                    rewrote = true;
                    spelled = rel;
                }
                Verdict::Missing => note(&raw, UndecidedWhy::NoSuchFile, &mut undecided),
                Verdict::OutsideTarget => {
                    note(&raw, UndecidedWhy::OutsidePlannedRoot, &mut undecided)
                }
                Verdict::Keep => {}
            }
            let mut obj = Map::new();
            obj.insert("path".to_string(), Value::String(spelled));
            if !payload.location_lines.trim().is_empty() {
                obj.insert(
                    "lines".to_string(),
                    Value::String(payload.location_lines.clone()),
                );
            }
            locations = Some(vec![Value::Object(obj)]);
        }

        // `governs`: every entry kept, path entries inside the root rewritten.
        let mut governs: Option<Value> = None;
        if let Some(entries) = meta.get("governs").and_then(Value::as_array) {
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries {
                let Some(raw) = entry.as_str() else {
                    out.push(entry.clone());
                    continue;
                };
                let (qualifier, logical) = split_qualified(raw, namespace);
                if !governs_is_path(logical, &roots.both())
                    || other_root_paths
                        .contains(&crate::parser::ingest::normalize_node_path(logical))
                {
                    out.push(entry.clone());
                    continue;
                }
                match judge(logical, &roots) {
                    Verdict::Rewrite(rel) => {
                        rewrote = true;
                        out.push(Value::String(format!("{qualifier}{rel}")));
                    }
                    Verdict::Missing => {
                        note(raw, UndecidedWhy::NoSuchFile, &mut undecided);
                        out.push(entry.clone());
                    }
                    Verdict::OutsideTarget => {
                        note(raw, UndecidedWhy::OutsidePlannedRoot, &mut undecided);
                        out.push(entry.clone());
                    }
                    Verdict::Keep => out.push(entry.clone()),
                }
            }
            governs = Some(Value::Array(out));
        }
        // Supersede derives governs from `locations` unless `governs` is
        // passed, so a call with `locations` always carries the stored value
        // (`[]` when there is none) and adds nothing.
        if locations.is_some() && governs.is_none() {
            governs = Some(match meta.get("governs") {
                None | Some(Value::Null) => Value::Array(Vec::new()),
                Some(other) => other.clone(),
            });
        }

        // The conflict matters only for a memory that would get a call.
        let location_conflict = location_conflict && rewrote && !other_root;
        if location_conflict {
            note(
                &payload.location,
                UndecidedWhy::LocationDiffersFromFirstRef,
                &mut undecided,
            );
        }
        needs_decision.extend(undecided);
        // One memory is one supersede call: a memory with a path bound to
        // another root is left whole for the user to decide.
        if rewrote && !other_root && !location_conflict {
            let mut args = Map::new();
            args.insert("id".to_string(), json!(row.id));
            args.insert("namespace".to_string(), json!(namespace));
            args.insert("content".to_string(), json!(payload.content));
            args.insert("reason".to_string(), json!(SUPERSEDE_REASON));
            if let Some(locations) = locations {
                args.insert("locations".to_string(), Value::Array(locations));
            }
            if let Some(governs) = governs {
                args.insert("governs".to_string(), governs);
            }
            fixable.push(FixableRow {
                id: row.id.clone(),
                supersede_args: Value::Object(args),
            });
        }
    }
    AuditReport {
        fixable,
        needs_decision,
    }
}

/// Audit a namespace's active memories against `root` (the marker root),
/// spelling corrections relative to `planned` when given. `bindings` are the
/// configured project contexts (source_id to its `.NeuroStrata` folder). Only
/// lists rows; never writes.
pub async fn audit_namespace(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    root: &Path,
    planned: Option<&Path>,
    bindings: &HashMap<String, PathBuf>,
) -> anyhow::Result<AuditReport> {
    let rows = store.list(namespace, None, None).await?;
    Ok(audit_rows_for(&rows, namespace, root, planned, bindings))
}

/// Every path key a row stores, relative or absolute: `location`, `refs`
/// files and the `governs` entries that name paths (never memory ids or
/// domains; a bare name only when a file of that name exists under `root`).
fn path_keys(row: &SearchResult, root: &Path) -> Vec<String> {
    let payload = &row.payload;
    let mut keys = Vec::new();
    if location_is_path(&payload.location) {
        keys.push(payload.location.clone());
    }
    if let Some(refs) = payload.metadata.get("refs").and_then(Value::as_array) {
        for reference in refs {
            if let Some(file) = reference
                .get("file")
                .or_else(|| reference.get("path"))
                .and_then(Value::as_str)
            {
                keys.push(file.to_string());
            }
        }
    }
    if let Some(governs) = payload.metadata.get("governs").and_then(Value::as_array) {
        keys.extend(
            governs
                .iter()
                .filter_map(Value::as_str)
                .filter(|g| governs_is_path(g, &[root]))
                .map(str::to_string),
        );
    }
    keys
}

/// Every relative path key a row stores.
fn relative_keys(row: &SearchResult, root: &Path) -> Vec<String> {
    path_keys(row, root)
        .into_iter()
        .filter(|k| !crate::paths::is_absolute_any(k))
        .collect()
}

/// [`rows_lost_in`] over the namespace's rows read from a store, for tests.
#[cfg(test)]
pub async fn rows_lost_by_root_change(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    current: &Path,
    new: &Path,
) -> anyhow::Result<Vec<String>> {
    let rows = store.list(namespace, None, None).await?;
    Ok(rows_lost_in(&rows, current, new))
}

/// Ids of active rows with a relative stored key that names an existing file
/// or folder under `current` but would be spelled differently relative to
/// `new`, so moving the root from `current` to `new` would stop them matching.
/// Absolute keys never matched by a relative key, so they are not lost. The
/// rows are already listed (the CLI reads them through the daemon's
/// `/cli/read`).
pub fn rows_lost_in(rows: &[SearchResult], current: &Path, new: &Path) -> Vec<String> {
    let new_str = new.to_string_lossy().to_string();
    rows_losing(rows, current, |full, key| {
        relative_to_on_disk(full, &new_str).as_deref() != Some(key)
    })
}

/// For a root change between unrelated folders (a sibling, another clone):
/// ids of active rows with a relative stored key that names an existing file
/// or folder under `current` but nothing under `new`.
pub fn rows_missing_under(rows: &[SearchResult], current: &Path, new: &Path) -> Vec<String> {
    let mut lost = rows_losing(rows, current, |_, key| !new.join(key).exists());
    // An absolute anchor under `current` is also lost: lookups join relative
    // requests to the cached root, so the old spelling stops matching once
    // the root moves to `new`.
    let current_canon = dunce::canonicalize(current).unwrap_or_else(|_| current.to_path_buf());
    let now = now_secs();
    for row in rows.iter().filter(|r| is_auditable(r, now)) {
        if lost.contains(&row.id) {
            continue;
        }
        let anchored_under_current = path_keys(row, current).into_iter().any(|key| {
            if !crate::paths::is_absolute_any(&key) {
                return false;
            }
            let path = PathBuf::from(&key);
            path.exists()
                && dunce::canonicalize(&path)
                    .is_ok_and(|p| p.starts_with(&current_canon) && p != current_canon)
        });
        if anchored_under_current {
            lost.push(row.id.clone());
        }
    }
    lost.sort();
    lost
}

/// Ids of active rows with a relative key that names something under
/// `current` (`full`) and that `lost(full, key)` says would stop matching.
fn rows_losing(
    rows: &[SearchResult],
    current: &Path,
    lost_key: impl Fn(&str, &str) -> bool,
) -> Vec<String> {
    let now = now_secs();
    let current_str = current
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string();
    let mut lost: Vec<String> = Vec::new();
    for row in rows.iter().filter(|r| is_auditable(r, now)) {
        let moves = relative_keys(row, current).iter().any(|raw| {
            let bare = raw.split('#').next().unwrap_or(raw);
            let key = crate::parser::ingest::normalize_node_path(bare);
            if key.is_empty() {
                return false;
            }
            let full = format!("{current_str}/{key}");
            if !Path::new(&full).exists() {
                return false;
            }
            lost_key(&full, &key)
        });
        if moves && !lost.contains(&row.id) {
            lost.push(row.id.clone());
        }
    }
    lost.sort();
    lost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::testkit::MockStore;
    use crate::traits::MemoryPayload;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn payload(content: &str, location: &str, metadata: Value) -> MemoryPayload {
        MemoryPayload {
            content: content.to_string(),
            user_id: "agent".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: location.to_string(),
            location_lines: String::new(),
            metadata,
        }
    }

    fn tmp_root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        (tmp, root)
    }

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    fn abs(root: &Path, rel: &str) -> String {
        root.join(rel).to_string_lossy().to_string()
    }

    /// A store that counts every write it is asked for.
    struct CountingStore {
        inner: MockStore,
        writes: AtomicUsize,
    }
    impl CountingStore {
        fn bump(&self) {
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl crate::traits::AsAny for CountingStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    #[async_trait::async_trait]
    impl VectorStore for CountingStore {
        async fn init(&self, ns: &str) -> anyhow::Result<()> {
            self.bump();
            self.inner.init(ns).await
        }
        async fn upsert(
            &self,
            ns: &str,
            id: &str,
            v: Vec<f32>,
            p: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            self.bump();
            self.inner.upsert(ns, id, v, p).await
        }
        async fn increment_metadata_counter(
            &self,
            ns: &str,
            id: &str,
            key: &str,
        ) -> anyhow::Result<()> {
            self.bump();
            self.inner.increment_metadata_counter(ns, id, key).await
        }
        async fn search(
            &self,
            ns: &str,
            v: Vec<f32>,
            l: usize,
            it: bool,
            ia: bool,
        ) -> anyhow::Result<Vec<SearchResult>> {
            self.inner.search(ns, v, l, it, ia).await
        }
        async fn delete(&self, ns: &str, id: &str) -> anyhow::Result<()> {
            self.bump();
            self.inner.delete(ns, id).await
        }
        async fn clear_ingested(&self, ns: &str) -> anyhow::Result<()> {
            self.bump();
            self.inner.clear_ingested(ns).await
        }
        async fn relink_edges(&self, ns: &str, sk: &str) -> anyhow::Result<usize> {
            self.bump();
            self.inner.relink_edges(ns, sk).await
        }
        async fn list(
            &self,
            ns: &str,
            u: Option<&str>,
            mt: Option<&str>,
        ) -> anyhow::Result<Vec<SearchResult>> {
            self.inner.list(ns, u, mt).await
        }
        async fn get(
            &self,
            ns: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            self.inner.get(ns, id).await
        }
        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.bump();
            self.inner.relocate(id, from, to).await
        }
        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }
        async fn export_graph(
            &self,
            ir: bool,
            ia: bool,
            pc: &HashMap<String, PathBuf>,
            ao: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<Value> {
            self.inner.export_graph(ir, ia, pc, ao).await
        }
        async fn increment_access_count(&self, ns: &str, id: &str) -> anyhow::Result<()> {
            self.bump();
            self.inner.increment_access_count(ns, id).await
        }
        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }
        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.bump();
            self.inner.checkpoint().await
        }
    }

    async fn store_with(rows: Vec<(&str, MemoryPayload)>) -> Arc<dyn VectorStore> {
        let store = MockStore::new();
        for (id, p) in rows {
            store
                .upsert("example-project", id, vec![0.0], p)
                .await
                .unwrap();
        }
        Arc::new(store)
    }

    #[tokio::test]
    async fn absolute_in_root_is_fixable_with_full_lists() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        touch(&root, "README.md");
        let a = abs(&root, "src/a.rs");
        let store = store_with(vec![(
            "m1",
            payload(
                "Keep the parser pure.",
                &a,
                json!({
                    "refs": [
                        { "file": a, "lines": "1-5", "symbol": "parse" },
                        { "file": "README.md" }
                    ],
                    "governs": ["src/"]
                }),
            ),
        )])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert!(
            report.needs_decision.is_empty(),
            "{:?}",
            report.needs_decision
        );
        assert_eq!(report.fixable.len(), 1);
        assert_eq!(report.fixable[0].id, "m1");
        assert_eq!(
            report.fixable[0].supersede_args,
            json!({
                "id": "m1",
                "namespace": "example-project",
                "content": "Keep the parser pure.",
                "reason": "path made project-relative",
                "locations": [
                    { "path": "src/a.rs", "lines": "1-5", "symbol": "parse" },
                    { "path": "README.md" }
                ],
                "governs": ["src/"]
            })
        );
    }

    #[tokio::test]
    async fn absolute_directory_governs_keeps_its_slash() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let dir = format!("{}/", abs(&root, "src"));
        let store = store_with(vec![(
            "m1",
            payload("Rule.", "", json!({ "governs": [dir, "auth"] })),
        )])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(report.fixable.len(), 1);
        assert_eq!(
            report.fixable[0].supersede_args["governs"],
            json!(["src/", "auth"])
        );
    }

    #[tokio::test]
    async fn governs_only_row_gets_no_locations_key() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let a = abs(&root, "src/a.rs");
        let store = store_with(vec![(
            "m1",
            payload("Rule.", "", json!({ "governs": [a] })),
        )])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(report.fixable.len(), 1);
        let args = &report.fixable[0].supersede_args;
        assert!(args.get("locations").is_none(), "{args}");
        assert_eq!(args["governs"], json!(["src/a.rs"]));
    }

    #[tokio::test]
    async fn missing_relative_file_needs_decision() {
        let (_tmp, root) = tmp_root();
        let store = store_with(vec![(
            "m1",
            payload(
                "Rule.",
                "src/gone.rs",
                json!({ "refs": [{ "file": "src/gone.rs" }] }),
            ),
        )])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert!(report.fixable.is_empty());
        assert_eq!(
            report.needs_decision,
            vec![UndecidedRow {
                id: "m1".to_string(),
                path: "src/gone.rs".to_string(),
                why: UndecidedWhy::NoSuchFile,
            }]
        );
    }

    #[tokio::test]
    async fn ref_bound_to_other_source_root_needs_decision() {
        let (_tmp, root) = tmp_root();
        let (_other_tmp, other) = tmp_root();
        touch(&root, "src/a.rs");
        touch(&root, "src/b.rs");
        let a = abs(&root, "src/a.rs");
        let b = abs(&root, "src/b.rs");
        let mut bindings = HashMap::new();
        bindings.insert("other-src".to_string(), other.join(".NeuroStrata"));
        bindings.insert("this-src".to_string(), root.join(".NeuroStrata"));
        let store = store_with(vec![(
            "m1",
            payload(
                "Rule.",
                &a,
                json!({ "refs": [
                    { "file": a, "source_context": { "source_id": "this-src" } },
                    { "file": b, "source_context": { "source_id": "other-src" } }
                ] }),
            ),
        )])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &bindings)
            .await
            .unwrap();
        assert!(report.fixable.is_empty(), "{:?}", report.fixable);
        assert_eq!(
            report.needs_decision,
            vec![UndecidedRow {
                id: "m1".to_string(),
                path: b,
                why: UndecidedWhy::OtherSourceRoot,
            }]
        );
    }

    #[tokio::test]
    async fn exit_code_counts_only_fixable() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let only_undecided =
            store_with(vec![("m1", payload("Rule.", "src/gone.rs", json!({})))]).await;
        let report = audit_namespace(
            &only_undecided,
            "example-project",
            &root,
            None,
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.needs_decision.len(), 1);
        assert_eq!(report.exit_code(), 0);
        let with_fixable = store_with(vec![(
            "m2",
            payload("Rule.", &abs(&root, "src/a.rs"), json!({})),
        )])
        .await;
        let report = audit_namespace(
            &with_fixable,
            "example-project",
            &root,
            None,
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.fixable.len(), 1);
        assert_eq!(
            report.fixable[0].supersede_args["locations"],
            json!([{ "path": "src/a.rs" }])
        );
        assert_eq!(report.exit_code(), 1);
        assert!(report
            .to_text()
            .contains("also stores that path as its first ref"));
        assert!(report
            .to_text()
            .contains("\"reason\":\"path made project-relative\""));
    }

    #[tokio::test]
    async fn location_only_row_passes_empty_governs() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let a = abs(&root, "src/a.rs");
        let store = store_with(vec![
            ("m1", payload("Rule.", &a, json!({}))),
            ("m2", payload("Rule.", &a, json!({ "governs": null }))),
            // Refs without governs: the call must not let supersede derive any.
            (
                "m3",
                payload("Rule.", &a, json!({ "refs": [{ "file": a }] })),
            ),
        ])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(report.fixable.len(), 3, "{:?}", report.needs_decision);
        for row in &report.fixable {
            assert_eq!(row.supersede_args["governs"], json!([]), "{}", row.id);
            assert_eq!(
                row.supersede_args["locations"][0]["path"],
                json!("src/a.rs"),
                "{}",
                row.id
            );
        }
    }

    #[tokio::test]
    async fn location_differing_from_first_ref_needs_decision() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        touch(&root, "src/b.rs");
        let a = abs(&root, "src/a.rs");
        let b = abs(&root, "src/b.rs");
        let store = store_with(vec![
            (
                "m1",
                payload(
                    "Rule.",
                    &b,
                    json!({ "refs": [{ "file": a }], "governs": [a] }),
                ),
            ),
            (
                "m2",
                payload(
                    "Rule.",
                    &a,
                    json!({ "refs": [{ "file": a }], "governs": [a] }),
                ),
            ),
        ])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(
            report.needs_decision,
            vec![UndecidedRow {
                id: "m1".to_string(),
                path: b,
                why: UndecidedWhy::LocationDiffersFromFirstRef,
            }]
        );
        assert_eq!(report.fixable.len(), 1);
        assert_eq!(report.fixable[0].id, "m2");
        assert_eq!(
            report.fixable[0].supersede_args["governs"],
            json!(["src/a.rs"])
        );
    }

    #[tokio::test]
    async fn retired_rows_and_tasks_are_skipped() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let a = abs(&root, "src/a.rs");
        let mut task = payload("Task.", &a, json!({}));
        task.memory_type = "task".to_string();
        let store = store_with(vec![
            (
                "old",
                payload("Old.", &a, json!({ "superseded_by": "new" })),
            ),
            ("gone", payload("Gone.", &a, json!({ "valid_to": 1 }))),
            ("t1", task),
        ])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert!(report.fixable.is_empty(), "{:?}", report.fixable);
    }

    #[tokio::test]
    async fn audit_writes_nothing() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let a = abs(&root, "src/a.rs");
        let counting = Arc::new(CountingStore {
            inner: MockStore::new(),
            writes: AtomicUsize::new(0),
        });
        counting
            .inner
            .upsert(
                "example-project",
                "m1",
                vec![0.0],
                payload("Rule.", &a, json!({ "governs": [a] })),
            )
            .await
            .unwrap();
        let store: Arc<dyn VectorStore> = counting.clone();
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(report.fixable.len(), 1);
        rows_lost_by_root_change(&store, "example-project", &root, root.parent().unwrap())
            .await
            .unwrap();
        assert_eq!(counting.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rows_lost_moving_root_up() {
        let (_tmp, top) = tmp_root();
        let sub = top.join("sub");
        touch(&sub, "a.rs");
        std::fs::create_dir_all(sub.join("auth")).unwrap();
        let store = store_with(vec![
            (
                "m1",
                payload("Rule.", "a.rs", json!({ "refs": [{ "file": "a.rs" }] })),
            ),
            // An absolute key never matched by a relative key: not lost.
            ("m2", payload("Rule.", &abs(&sub, "a.rs"), json!({}))),
            // Names nothing under the current root: not lost by the move.
            ("m3", payload("Rule.", "nothing.rs", json!({}))),
            // A governs id or domain that happens to name a folder is not a key.
            ("m4", payload("Rule.", "", json!({ "governs": ["auth"] }))),
        ])
        .await;
        let lost = rows_lost_by_root_change(&store, "example-project", &sub, &top)
            .await
            .unwrap();
        assert_eq!(lost, vec!["m1".to_string()]);
        let same = rows_lost_by_root_change(&store, "example-project", &sub, &sub)
            .await
            .unwrap();
        assert!(same.is_empty(), "{same:?}");
    }

    /// `--for-root`: keys that name files under the current root are spelled
    /// relative to the planned root, so applying the calls and then moving
    /// the root up loses nothing.
    #[tokio::test]
    async fn for_root_spells_corrections_relative_to_the_planned_root() {
        let (_tmp, top) = tmp_root();
        let sub = top.join("sub");
        touch(&sub, "a.rs");
        touch(&sub, "b.rs");
        touch(&top, "top.rs");
        let b = abs(&sub, "b.rs");
        let rows = vec![
            (
                "m1",
                payload(
                    "Rule a.",
                    "a.rs",
                    json!({ "refs": [{ "file": "a.rs", "lines": "2" }] }),
                ),
            ),
            ("m2", payload("Rule b.", &b, json!({ "governs": [b] }))),
            // Already a key of the planned root.
            ("m3", payload("Rule top.", "top.rs", json!({}))),
            ("m4", payload("Rule gone.", "gone.rs", json!({}))),
        ];
        let store = store_with(rows).await;
        let report = audit_namespace(&store, "example-project", &sub, Some(&top), &HashMap::new())
            .await
            .unwrap();
        let fixed: HashMap<&str, &Value> = report
            .fixable
            .iter()
            .map(|r| (r.id.as_str(), &r.supersede_args))
            .collect();
        assert_eq!(fixed.len(), 2, "{:?}", report);
        assert_eq!(
            fixed["m1"]["locations"],
            json!([{ "path": "sub/a.rs", "lines": "2" }])
        );
        assert_eq!(fixed["m2"]["locations"], json!([{ "path": "sub/b.rs" }]));
        assert_eq!(fixed["m2"]["governs"], json!(["sub/b.rs"]));
        assert_eq!(
            report.needs_decision,
            vec![UndecidedRow {
                id: "m4".to_string(),
                path: "gone.rs".to_string(),
                why: UndecidedWhy::NoSuchFile,
            }]
        );

        // The corrected rows lose nothing when the root moves up.
        let corrected = vec![
            (
                "m1",
                payload(
                    "Rule a.",
                    "sub/a.rs",
                    json!({ "refs": [{ "file": "sub/a.rs" }] }),
                ),
            ),
            ("m3", payload("Rule top.", "top.rs", json!({}))),
        ];
        let rows: Vec<SearchResult> = store_with(corrected)
            .await
            .list("example-project", None, None)
            .await
            .unwrap();
        assert!(rows_lost_in(&rows, &sub, &top).is_empty());
        // Without the correction the move loses m1.
        let rows: Vec<SearchResult> = store.list("example-project", None, None).await.unwrap();
        assert_eq!(rows_lost_in(&rows, &sub, &top), vec!["m1".to_string()]);

        // A planned root that does not contain the file: the key cannot be
        // spelled there, so it is left for a decision.
        let (_other_tmp, other) = tmp_root();
        let report = audit_namespace(
            &store,
            "example-project",
            &sub,
            Some(&other),
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert!(report.needs_decision.contains(&UndecidedRow {
            id: "m1".to_string(),
            path: "a.rs".to_string(),
            why: UndecidedWhy::OutsidePlannedRoot,
        }));
    }

    /// A location that names another file than the first ref, both relative
    /// to the root, would be replaced by supersede: the memory is left for a
    /// decision even when the location is relative.
    #[tokio::test]
    async fn relative_location_differing_from_rewritten_first_ref_needs_decision() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        touch(&root, "README.md");
        let a = abs(&root, "src/a.rs");
        let store = store_with(vec![
            (
                "m1",
                payload("Rule.", "README.md", json!({ "refs": [{ "file": a }] })),
            ),
            (
                "m2",
                payload("Rule.", "src/a.rs", json!({ "refs": [{ "file": a }] })),
            ),
            // Nothing to rewrite: no call, so no decision either.
            (
                "m3",
                payload(
                    "Rule.",
                    "README.md",
                    json!({ "refs": [{ "file": "src/a.rs" }] }),
                ),
            ),
        ])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(
            report.needs_decision,
            vec![UndecidedRow {
                id: "m1".to_string(),
                path: "README.md".to_string(),
                why: UndecidedWhy::LocationDiffersFromFirstRef,
            }]
        );
        assert_eq!(report.fixable.len(), 1);
        assert_eq!(report.fixable[0].id, "m2");
    }

    /// A bare file name in `governs` is a key when that file exists under the
    /// root: the root-change check counts it, and an absolute spelling of it
    /// is still rewritten.
    #[tokio::test]
    async fn bare_file_name_governs_counts_as_a_path() {
        let (_tmp, top) = tmp_root();
        let sub = top.join("sub");
        touch(&sub, "Cargo.toml");
        let store = store_with(vec![
            (
                "m1",
                payload("Rule.", "", json!({ "governs": ["Cargo.toml"] })),
            ),
            // No such file: a domain or an id, not a key.
            ("m2", payload("Rule.", "", json!({ "governs": ["auth"] }))),
        ])
        .await;
        let lost = rows_lost_by_root_change(&store, "example-project", &sub, &top)
            .await
            .unwrap();
        assert_eq!(lost, vec!["m1".to_string()]);
        let report = audit_namespace(&store, "example-project", &sub, Some(&top), &HashMap::new())
            .await
            .unwrap();
        assert_eq!(report.fixable.len(), 1, "{report:?}");
        assert_eq!(
            report.fixable[0].supersede_args["governs"],
            json!(["sub/Cargo.toml"])
        );
    }

    /// Between unrelated roots a key is lost only when its file is missing
    /// under the new root.
    #[tokio::test]
    async fn rows_missing_under_an_unrelated_root() {
        let (_tmp, base) = tmp_root();
        let (old, new) = (base.join("old"), base.join("new"));
        touch(&old, "a.rs");
        touch(&old, "both.rs");
        touch(&new, "both.rs");
        let rows: Vec<SearchResult> = store_with(vec![
            ("m1", payload("Rule.", "a.rs", json!({}))),
            ("m2", payload("Rule.", "both.rs", json!({}))),
        ])
        .await
        .list("example-project", None, None)
        .await
        .unwrap();
        assert_eq!(
            rows_missing_under(&rows, &old, &new),
            vec!["m1".to_string()]
        );
    }

    /// An absolute anchor under the old checkout is a loss too: after the
    /// root moves, lookups join relative requests to the new root, so the
    /// old absolute spelling no longer matches.
    #[tokio::test]
    async fn rows_missing_under_flags_absolute_anchors_under_the_old_root() {
        let (_tmp, base) = tmp_root();
        let (old, new) = (base.join("old"), base.join("new"));
        touch(&old, "a.rs");
        touch(&new, "a.rs");
        let old_abs = abs(&old, "a.rs");
        let new_abs = abs(&new, "a.rs");
        let rows: Vec<SearchResult> = store_with(vec![
            ("m1", payload("Rule.", &old_abs, json!({}))),
            ("m2", payload("Rule.", &new_abs, json!({}))),
            (
                "m3",
                payload("Rule.", "", json!({ "refs": [{ "file": old_abs }] })),
            ),
        ])
        .await
        .list("example-project", None, None)
        .await
        .unwrap();
        assert_eq!(
            rows_missing_under(&rows, &old, &new),
            vec!["m1".to_string(), "m3".to_string()]
        );
    }

    /// A binding spelled differently for the same folder is this root, not
    /// another one.
    #[cfg(windows)]
    #[tokio::test]
    async fn binding_in_another_spelling_of_the_root_is_this_root() {
        let (_tmp, root) = tmp_root();
        touch(&root, "src/a.rs");
        let a = abs(&root, "src/a.rs");
        let raw = root.to_string_lossy().into_owned();
        let respelled = format!(
            "{}{}",
            raw[..1].to_ascii_lowercase(),
            raw[1..].replace('\\', "/")
        );
        let mut bindings = HashMap::new();
        bindings.insert(
            "this-src".to_string(),
            PathBuf::from(respelled).join(".NeuroStrata"),
        );
        let store = store_with(vec![(
            "m1",
            payload(
                "Rule.",
                &a,
                json!({ "refs": [{ "file": a, "source_context": { "source_id": "this-src" } }] }),
            ),
        )])
        .await;
        let report = audit_namespace(&store, "example-project", &root, None, &bindings)
            .await
            .unwrap();
        assert!(
            report.needs_decision.is_empty(),
            "{:?}",
            report.needs_decision
        );
        assert_eq!(report.fixable.len(), 1);
    }
}
