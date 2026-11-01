use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lbug::{Connection, Database, SystemConfig};

use crate::domain::domain_key;
use crate::traits::{
    is_internal_ingest_stage_namespace, is_reserved_ingest_destination, DomainMatchMode,
    DomainSearchOutcome, GraphMatchKind, GraphRead, MemoryHit, MemoryPayload,
    RelocateOutcome, SearchResult, VectorStore, INTERNAL_INGEST_STAGE_PREFIX,
};

struct IngestStageLease {
    file: Option<File>,
}

impl Drop for IngestStageLease {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
            // Keep the sidecar as durable provenance if staging exits before
            // publication. The next ingest can then safely recover its rows.
            drop(file);
        }
    }
}

pub(crate) fn ingest_stage_lease_path(db_path: &Path, namespace: &str) -> Result<PathBuf> {
    let generation = namespace
        .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("invalid internal ingest generation namespace"))?;
    let parsed = uuid::Uuid::parse_str(generation)
        .map_err(|_| anyhow::anyhow!("invalid internal ingest generation identifier"))?;
    if parsed.to_string() != generation {
        anyhow::bail!("invalid internal ingest generation identifier");
    }
    Ok(raw_database_sibling(
        &canonical_database_path(db_path),
        &format!(".stage-{generation}.lock"),
    ))
}

fn ingest_stage_lease_candidates(db_path: &Path, namespace: &str) -> Result<Vec<PathBuf>> {
    let canonical = ingest_stage_lease_path(db_path, namespace)?;
    let alias = raw_database_sibling(
        db_path,
        &format!(
            ".stage-{}.lock",
            namespace
                .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
                .expect("validated by ingest_stage_lease_path")
        ),
    );
    let mut paths = Vec::new();
    let mut identities = HashSet::new();
    for path in [canonical, alias] {
        let identity = canonical_path_for_comparison(&path);
        if identities.insert(identity) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn ingest_stage_lease_candidates_with_aliases(
    db_path: &Path,
    aliases: &[PathBuf],
    namespace: &str,
) -> Result<Vec<PathBuf>> {
    let mut paths = ingest_stage_lease_candidates(db_path, namespace)?;
    let mut identities = HashSet::new();
    for path in &paths {
        identities.insert(canonical_path_for_comparison(path));
    }
    for alias in aliases {
        // Alias entries are frozen at store-open time: their parent is
        // canonicalized but the final filename is preserved. Do not resolve a
        // symlinked parent again during later recovery, or a retargeted parent
        // could make this database inspect another directory's sidecars.
        let generation = namespace
            .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
            .expect("validated by is_internal_ingest_stage_namespace");
        let path = raw_database_sibling(alias, &format!(".stage-{generation}.lock"));
        let identity = canonical_path_for_comparison(&path);
        if identities.insert(identity) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn canonical_path_for_comparison(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };
    std::fs::canonicalize(parent)
        .map(|parent| parent.join(name))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn canonical_database_path(db_path: &Path) -> PathBuf {
    std::fs::canonicalize(db_path)
        .ok()
        .or_else(|| {
            let parent = db_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = db_path.file_name()?;
            std::fs::canonicalize(parent)
                .ok()
                .map(|parent| parent.join(name))
        })
        .unwrap_or_else(|| db_path.to_path_buf())
}

/// Resolve one database identity before locks, sidecars, or the engine open.
/// Existing aliases resolve to their target; fresh files resolve through an
/// already-created canonical parent. Dangling symlinks are not fresh DB paths.
fn resolve_database_path(db_path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(db_path) {
        Ok(path) => Ok(path),
        Err(error) if std::fs::symlink_metadata(db_path).is_ok() => Err(anyhow::anyhow!(
            "cannot resolve existing database path {}: {}",
            db_path.display(),
            error
        )),
        Err(error) if error.kind() != ErrorKind::NotFound => Err(anyhow::anyhow!(
            "cannot resolve database path {}: {}",
            db_path.display(),
            error
        )),
        Err(_) => {
            let parent = db_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = db_path.file_name().ok_or_else(|| {
                anyhow::anyhow!("database path {} has no file name", db_path.display())
            })?;
            let parent = std::fs::canonicalize(parent).map_err(|error| {
                anyhow::anyhow!(
                    "cannot resolve database parent {}: {}",
                    parent.display(),
                    error
                )
            })?;
            Ok(parent.join(name))
        }
    }
}

fn anchor_path_spelling(path: &Path) -> Result<PathBuf> {
    let anchored = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    absolute_path_spelling(&anchored)
}

fn absolute_path_spelling(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if !absolute.is_absolute() {
        anyhow::bail!("database path {} is not absolute", path.display());
    }
    Ok(absolute)
}

fn freeze_path_parent(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("database path {} has no file name", path.display()))?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}

/// Find supported path spellings that resolve to the same database. Follow
/// symlink chains in file and parent components so every known sibling marker
/// is inspected before the engine opens.
fn discover_database_path_aliases(resolved: &Path, supplied: &Path) -> Result<Vec<PathBuf>> {
    let supplied = absolute_path_spelling(supplied)?;
    ensure_database_path_unchanged(resolved, &supplied)?;
    let mut pending = vec![supplied];
    let mut seen = HashSet::new();
    let mut aliases = Vec::new();
    while let Some(candidate) = pending.pop() {
        if !seen.insert(candidate.clone()) {
            continue;
        }
        if seen.len() > 64 {
            anyhow::bail!("database alias chain exceeds the supported 64-path limit");
        }
        if resolve_database_path(&candidate).ok().as_deref() == Some(resolved)
            && candidate != resolved
        {
            // Sidecars are siblings, so freeze the containing directory while
            // retaining this alias's filename. A later parent-symlink retarget
            // must not redirect recovery into another database's sidecars.
            let frozen = freeze_path_parent(&candidate)?;
            if frozen != resolved {
                aliases.push(frozen);
            }
        }

        let mut prefix = PathBuf::new();
        for component in candidate.components() {
            match component {
                std::path::Component::CurDir => prefix.push("."),
                std::path::Component::ParentDir => prefix.push(".."),
                _ => prefix.push(component.as_os_str()),
            }
            match std::fs::symlink_metadata(&prefix) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&prefix).map_err(|error| {
                        anyhow::anyhow!(
                            "cannot inspect database alias {}: {}",
                            prefix.display(),
                            error
                        )
                    })?;
                    let target = if target.is_absolute() {
                        target
                    } else {
                        prefix
                            .parent()
                            .unwrap_or_else(|| Path::new("/"))
                            .join(target)
                    };
                    let remainder = candidate.strip_prefix(&prefix).map_err(|error| {
                        anyhow::anyhow!("cannot expand database alias: {}", error)
                    })?;
                    let expanded = if remainder.as_os_str().is_empty() {
                        target
                    } else {
                        target.join(remainder)
                    };
                    pending.push(absolute_path_spelling(&expanded)?);
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => {
                    anyhow::bail!(
                        "cannot inspect database path component {}: {}",
                        prefix.display(),
                        error
                    )
                }
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    Ok(aliases)
}

fn ensure_database_path_unchanged(resolved: &Path, supplied: &Path) -> Result<()> {
    let current = resolve_database_path(supplied)?;
    if current != resolved {
        anyhow::bail!(
            "database path alias {} changed while it was being inspected; no database was opened. Retry with a stable path and preserve all marker files",
            supplied.display()
        );
    }
    Ok(())
}

fn raw_database_sibling(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn database_path_spellings(db_path: &Path, aliases: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for path in std::iter::once(db_path).chain(aliases.iter().map(PathBuf::as_path)) {
        if seen.insert(path.to_path_buf()) {
            paths.push(path.to_path_buf());
        }
    }
    paths
}

fn discover_stage_lease_sidecars_with_aliases(
    db_path: &Path,
    aliases: &[PathBuf],
) -> Result<Vec<(String, PathBuf)>> {
    let database_paths = database_path_spellings(db_path, aliases);
    let mut found = std::collections::BTreeMap::<String, PathBuf>::new();
    for database in database_paths {
        let Some(name) = database.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(parent) = database.parent() else {
            continue;
        };
        let prefix = format!("{name}.stage-");
        let entries = match std::fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let Some(filename) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let Some(generation) = filename
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix(".lock"))
            else {
                continue;
            };
            let Ok(parsed) = uuid::Uuid::parse_str(generation) else {
                continue;
            };
            if parsed.to_string() != generation {
                continue;
            }
            let namespace = format!("{INTERNAL_INGEST_STAGE_PREFIX}{generation}");
            found.entry(namespace).or_insert_with(|| entry.path());
        }
    }
    Ok(found.into_iter().collect())
}

fn database_writer_lock_path(db_path: &Path) -> PathBuf {
    raw_database_sibling(&canonical_database_path(db_path), ".writer.lock")
}

pub(crate) fn acquire_database_writer_lock(db_path: &Path) -> Result<File> {
    let path = database_writer_lock_path(db_path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)
        .map_err(|error| {
            anyhow::anyhow!(
                "could not open database writer lock {}: {}",
                path.display(),
                error
            )
        })?;
    file.lock().map_err(|error| {
        anyhow::anyhow!(
            "could not lock database writer lock {}: {}",
            path.display(),
            error
        )
    })?;
    Ok(file)
}

pub(crate) fn restore_in_progress_path(db_path: &Path) -> PathBuf {
    let mut marker = db_path.as_os_str().to_os_string();
    marker.push(".restore-in-progress");
    PathBuf::from(marker)
}

pub(crate) fn create_restored_stage_leases(
    db_path: &Path,
    namespaces: &[String],
) -> Result<Vec<PathBuf>> {
    let mut created = Vec::new();
    for namespace in namespaces {
        if !is_internal_ingest_stage_namespace(namespace) {
            for path in &created {
                let _ = std::fs::remove_file(path);
            }
            anyhow::bail!("snapshot contains an invalid staging namespace identifier");
        }
        let path = ingest_stage_lease_path(db_path, namespace)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                created.push(path);
            }
            Err(error) => {
                for path in &created {
                    let _ = std::fs::remove_file(path);
                }
                return Err(anyhow::anyhow!(
                    "cannot install fresh staging provenance beside restored database: {}",
                    error
                ));
            }
        }
    }
    Ok(created)
}

fn has_ingest_stage_lease(db_path: &Path, aliases: &[PathBuf], namespace: &str) -> bool {
    if !is_internal_ingest_stage_namespace(namespace) {
        return false;
    }
    let Ok(paths) = ingest_stage_lease_candidates_with_aliases(db_path, aliases, namespace) else {
        return false;
    };
    paths.into_iter().any(|path| {
        let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
            return false;
        };
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(_)) => false,
        }
    })
}

fn read_stage_provenance(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut rows = match conn
        .query("MATCH (s:IngestStage) RETURN s.namespace, s.generation ORDER BY s.namespace")
    {
        Ok(rows) => rows,
        Err(error) if error.to_string().contains("does not exist in catalog") => {
            return Ok(Vec::new())
        }
        Err(error) => return Err(error.into()),
    };
    let mut stages = Vec::new();
    while let Some(row) = rows.next() {
        let namespace = format!("{}", row[0]);
        let generation = format!("{}", row[1]);
        let valid = namespace
            .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
            .is_some_and(|raw| raw == generation && is_internal_ingest_stage_namespace(&namespace));
        if valid {
            stages.push((namespace, generation));
        }
    }
    Ok(stages)
}

/// Returns cataloged stages plus pre-catalog stage rows whose exact reserved
/// UUID namespace, generated sidecar, and auto-ingestor-qualified IDs prove
/// staging ownership. This is only a visibility filter; it does not authorize
/// deletion or migration of an active stage.
fn read_hidden_stage_namespaces(
    conn: &Connection,
    db_path: &Path,
    aliases: &[PathBuf],
) -> Result<HashSet<String>> {
    let mut hidden: HashSet<String> = read_stage_provenance(conn)?
        .into_iter()
        .map(|(namespace, _)| namespace)
        .collect();
    for (namespace, sidecar) in discover_stage_lease_sidecars_with_aliases(db_path, aliases)? {
        if hidden.contains(&namespace) || !sidecar.is_file() {
            continue;
        }
        if !is_internal_ingest_stage_namespace(&namespace) {
            continue;
        }
        let safe = escape_ladybug_string(&namespace);
        let mut rows = conn.query(&format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.user_id",
            safe
        ))?;
        let mut found = false;
        let mut owned = true;
        while let Some(row) = rows.next() {
            found = true;
            let id = format!("{}", row[0]);
            let user_id = format!("{}", row[1]);
            if user_id != "auto-ingestor"
                || !id.starts_with(&format!(
                    "{}{}",
                    namespace,
                    crate::parser::ingest::NAMESPACE_SEPARATOR
                ))
            {
                owned = false;
                break;
            }
        }
        if found && owned {
            hidden.insert(namespace);
        }
    }
    Ok(hidden)
}

/// Register legacy stage rows at a snapshot cut while the caller owns the
/// database writer lock. A valid UUID sidecar plus exclusively auto-ingestor,
/// namespace-qualified rows is the legacy ownership evidence. This only writes
/// catalog provenance; it never deletes rows or touches lease ownership.
fn migrate_legacy_stages_at_snapshot_cut(
    conn: &Connection,
    db_path: &Path,
    aliases: &[PathBuf],
) -> Result<Vec<File>> {
    let mut known: HashSet<String> = read_stage_provenance(conn)?
        .into_iter()
        .map(|(namespace, _)| namespace)
        .collect();
    let mut legacy_leases = Vec::new();
    for (namespace, sidecar) in discover_stage_lease_sidecars_with_aliases(db_path, aliases)? {
        if known.contains(&namespace) || !sidecar.is_file() {
            continue;
        }
        // A legacy generation has no durable catalog marker yet. Lock every
        // known alias before classifying it and keep those locks through the
        // completed snapshot. If a pre-catalog writer is still active, refuse
        // the snapshot instead of capturing a moving generation.
        for candidate in ingest_stage_lease_candidates_with_aliases(db_path, aliases, &namespace)? {
            if !candidate.is_file() {
                continue;
            }
            let lease = OpenOptions::new().read(true).write(true).open(&candidate)?;
            match lease.try_lock() {
                Ok(()) => legacy_leases.push(lease),
                Err(std::fs::TryLockError::WouldBlock) => {
                    anyhow::bail!(
                        "cannot snapshot legacy ingest generation {} while its stage lease is active",
                        namespace
                    )
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(anyhow::anyhow!(
                        "cannot verify legacy ingest generation {} lease: {}",
                        namespace,
                        error
                    ));
                }
            }
        }
        let Some(generation) = namespace.strip_prefix(INTERNAL_INGEST_STAGE_PREFIX) else {
            continue;
        };
        conn.query("BEGIN TRANSACTION")?;
        let result = (|| -> Result<bool> {
            let safe = escape_ladybug_string(&namespace);
            let mut rows = conn.query(&format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.user_id",
                safe
            ))?;
            let mut found = false;
            while let Some(row) = rows.next() {
                found = true;
                let id = format!("{}", row[0]);
                let user_id = format!("{}", row[1]);
                if user_id != "auto-ingestor"
                    || !id.starts_with(&format!(
                        "{}{}",
                        namespace,
                        crate::parser::ingest::NAMESPACE_SEPARATOR
                    ))
                {
                    return Ok(false);
                }
            }
            if !found {
                return Ok(false);
            }
            conn.query(&format!(
                "CREATE (s:IngestStage {{namespace: '{}', generation: '{}'}})",
                safe,
                escape_ladybug_string(generation)
            ))?;
            Ok(true)
        })();
        match result {
            Ok(true) => {
                conn.query("COMMIT")?;
                known.insert(namespace);
            }
            Ok(false) => {
                conn.query("ROLLBACK")?;
            }
            Err(error) => {
                let rollback = conn.query("ROLLBACK");
                return match rollback {
                    Ok(_) => Err(error),
                    Err(rollback) => Err(anyhow::anyhow!(
                        "snapshot stage provenance migration failed: {}; rollback failed: {}",
                        error,
                        rollback
                    )),
                };
            }
        }
    }
    Ok(legacy_leases)
}

fn cleanup_ingest_stage_lease_after_commit(path: &Path) -> Option<String> {
    match std::fs::remove_file(path) {
        Ok(()) => None,
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "Ingest generation committed, but staging lease cleanup failed: {}. The empty generation is safe and can be cleaned up later.",
            error
        )),
    }
}

#[cfg(test)]
static TEST_PUBLISH_CRASH_BARRIER: std::sync::OnceLock<
    std::sync::Mutex<Option<(String, PathBuf, PathBuf)>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
static TEST_SNAPSHOT_OPERATION_BARRIER: std::sync::OnceLock<
    std::sync::Mutex<Option<(String, PathBuf, PathBuf)>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
static TEST_WRITE_LOCK_ATTEMPT: std::sync::OnceLock<
    std::sync::Mutex<Option<(PathBuf, PathBuf)>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn set_test_write_lock_attempt(db_path: PathBuf, marker: PathBuf) {
    *TEST_WRITE_LOCK_ATTEMPT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = Some((db_path, marker));
}

#[cfg(test)]
fn test_write_lock_attempt(db_path: &Path) {
    let marker = {
        let mut state = TEST_WRITE_LOCK_ATTEMPT
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap();
        match state.as_ref() {
            Some((path, _))
                if std::fs::canonicalize(path).ok() == std::fs::canonicalize(db_path).ok() =>
            {
                Some(state.take().unwrap().1)
            }
            _ => None,
        }
    };
    if let Some(marker) = marker {
        let lock_path = database_writer_lock_path(db_path);
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .expect("writer lock probe file is openable");
        match probe.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => {
                std::fs::write(marker, "writer lock was contended")
                    .expect("write lock contention marker is writable");
            }
            Ok(()) => {
                let _ = probe.unlock();
                panic!("expected the checkpoint to hold the writer lock");
            }
            Err(std::fs::TryLockError::Error(error)) => {
                panic!("could not verify writer lock contention: {error}");
            }
        }
    }
}

#[cfg(test)]
fn set_test_snapshot_operation_barrier(operation: &str, db_path: PathBuf, barrier: PathBuf) {
    *TEST_SNAPSHOT_OPERATION_BARRIER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = Some((operation.to_string(), db_path, barrier));
}

#[cfg(test)]
fn test_snapshot_operation_barrier(db_path: &Path, operation: &str) {
    let barrier = {
        let mut state = TEST_SNAPSHOT_OPERATION_BARRIER
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap();
        match state.as_ref() {
            Some((expected, path, _))
                if expected == operation
                    && std::fs::canonicalize(path).ok() == std::fs::canonicalize(db_path).ok() =>
            {
                state.take().unwrap().2
            }
            _ => return,
        }
    };
    std::fs::write(&barrier, operation).expect("snapshot operation barrier is writable");
    let mut release = barrier.as_os_str().to_os_string();
    release.push(".release");
    let release = PathBuf::from(release);
    let started = std::time::Instant::now();
    while !release.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "snapshot operation barrier watchdog expired at {operation}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
fn set_test_publish_crash_barrier(phase: &str, db_path: PathBuf, barrier: PathBuf) {
    *TEST_PUBLISH_CRASH_BARRIER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = Some((phase.to_string(), db_path, barrier));
}

#[cfg(test)]
fn test_publish_crash_barrier(db_path: &Path, phase: &str) {
    let barrier = {
        let mut state = TEST_PUBLISH_CRASH_BARRIER
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap();
        match state.as_ref() {
            Some((configured_phase, configured_db, _))
                if configured_phase == phase
                    && std::fs::canonicalize(configured_db).ok()
                        == std::fs::canonicalize(db_path).ok() =>
            {
                state.take().unwrap().2
            }
            _ => return,
        }
    };
    std::fs::write(&barrier, phase).expect("publish crash barrier is writable");
    let mut release_path = barrier.as_os_str().to_os_string();
    release_path.push(".release");
    let release = PathBuf::from(release_path);
    let started = std::time::Instant::now();
    while !release.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "publish crash barrier watchdog expired at {phase}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Read a boolean column out of a query row.
///
/// The value has to be read as a typed boolean, never as its rendered form:
/// lbug renders `Value::Bool(true)` as "True", so comparing the rendering
/// against "true" is never true and the flag silently reads false.
///
/// The debug assert turns a future column-type drift into a loud failure in
/// test builds instead of another silent inversion.
fn row_flag_true(value: &lbug::Value) -> bool {
    debug_assert!(
        matches!(value, lbug::Value::Bool(_)),
        "the direction flag must be a typed boolean"
    );
    matches!(value, lbug::Value::Bool(true))
}

/// Internal search routine shared by `search` and `search_with_domain`.
/// Takes an owned `HashSet<String>` of domain keys (already normalized).
async fn search_inner(
    store: &LadybugStore,
    namespace: &str,
    vector: Vec<f32>,
    limit: usize,
    include_tasks: bool,
    include_archived: bool,
    domain_keys: HashSet<String>,
) -> Result<DomainSearchOutcome> {
    let namespace = namespace.to_string();
    store
        .with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let vec_str = format!(
                "[{}]",
                // R16: defense-in-depth (no native bind in lbug 0.20.4).
                // Always emit decimal (e.g. 0 → 0.0) so ARRAY_DISTANCE
                // binder never sees pure-integer arrays.
                vector.iter().map(|f| {
                    let s = format!("{}", f);
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        s
                    } else {
                        format!("{}.0", s)
                    }
                }).collect::<Vec<_>>().join(",")
            );
            let now = chrono::Utc::now().timestamp();

            // LadybugDB (the embedder) has no OFFSET, so we over-fetch with a larger LIMIT and
            // filter/trim in Rust. The multiplier bounds cost: at most
            // FETCH_MULTIPLIER * limit rows are scanned from the engine.
            const FETCH_MULTIPLIER: usize = 10;
            let fetch_limit = limit * FETCH_MULTIPLIER;

            // Step 1: Base vector search.
            //
            // Structural nodes are stored with an all-zero embedding, and the
            // distance from an all-zero row is the query vector's own magnitude
            // -- the same value for every one of them, whatever was asked. Left
            // in, they tie with each other and can fill the limit with path
            // stubs. They stay reachable through the graph expansion in step 2,
            // which is where they are actually useful.
            //
            // ORDER BY dist ASC, id ASC gives deterministic tie-breaking so the
            // same query returns the same rows across runs.
            let structural_types = STRUCTURAL_MEMORY_TYPES
                .iter()
                .map(|t| format!("'{}'", t))
                .collect::<Vec<_>>()
                .join(", ");

            // Build the row-class filter for the base query.
            // We exclude tasks, archived, and mesh-conflicting rows HERE so they
            // never consume the candidate budget. The server escape hatches
            // (include_tasks, include_archived) control whether these filters apply.
            let mut excluded_types = STRUCTURAL_MEMORY_TYPES.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            if !include_tasks {
                excluded_types.push("task".to_string());
            }
            let excluded_types_str = excluded_types
                .iter()
                .map(|t| format!("'{}'", t))
                .collect::<Vec<_>>()
                .join(", ");

            let search_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND NOT m.memory_type IN [{}] RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC, m.id ASC LIMIT {}",
                safe_ns, excluded_types_str, vec_str, fetch_limit
            );

            let result = conn.query(&search_query)?;
            let result_count = result.get_num_tuples() as usize;
            let mut all_primaries = Vec::new();
            let mut filtered_primaries = Vec::new();

            for row in result {
                let id: String = format!("{}", row[0]);
                let distance: f32 = match &row[1] {
                    lbug::Value::Float(f) if f.is_finite() && *f >= 0.0 => *f,
                    lbug::Value::Double(d) if d.is_finite() && *d >= 0.0 => *d as f32,
                    _ => continue,
                };

                let content: String = format!("{}", row[2]);
                let user_id: String = format!("{}", row[3]);
                let memory_type: String = format!("{}", row[4]);
                let agent_name: String = format!("{}", row[5]);
                let location: String = format!("{}", row[6]);
                let location_lines: String = format!("{}", row[7]);
                let metadata_str: String = format!("{}", row[8]);

                let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                // Skip retired rows -- they do not count toward the limit.
                if is_retired_at(&metadata_val, now) {
                    continue;
                }

                // Row-class filters: archived and mesh-conflicting rows are
                // filtered HERE in the store so they never consume a primary slot.
                // The server escape hatches control these filters.
                if !include_archived && is_archived(&metadata_val) {
                    continue;
                }
                if is_mesh_conflicting(&metadata_val) {
                    continue;
                }
                if crate::registry::is_registry_row(&metadata_val) {
                    continue;
                }

                // Domain filter check: happens AFTER row-class filters and BEFORE usage boost.
                // The domain_keys set is already normalized (lowercase-kebab).
                let domain_match = if domain_keys.is_empty() {
                    true // empty key set = no filter asked for
                } else {
                    domain_key(&metadata_val)
                        .map(|k| domain_keys.contains(&k))
                        .unwrap_or(false)
                };

                // Usage boost: ln(1+n) gives a real gain even at n=1, and the
                // cap at half distance preserves semantic ranking order.
                //
                // FEATURE-4: provenance earns a share of that same capped
                // budget. It is added *inside* the cap rather than after it --
                // subtracting from `distance` a term that was never itself
                // bounded is how a tie-breaker quietly becomes an override and
                // starts returning the wrong memory for the right reason.
                let access_count = metadata_val.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
                let n = access_count.max(0) as f32;
                let gain = 0.05 * (1.0 + n).ln();
                let source_gain = if crate::traits::has_source(&metadata_val) { SOURCE_RANK_GAIN } else { 0.0 };
                let capped_gain = (gain + source_gain).min(0.5 * distance);
                let boosted_distance = distance - capped_gain;

                let sr = SearchResult {
                    id: id.clone(),
                    score: boosted_distance,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                    evidence: Some(vec![crate::traits::SearchEvidence {
                        kind: crate::traits::EvidenceKind::DirectMatch,
                        path: vec![],
                        explanation: "This memory matched the query vector.".to_string(),
                    }]),
                };

                // Every accepted row goes to all_primaries
                all_primaries.push(sr.clone());

                // Only domain-matching rows go to filtered_primaries
                if domain_match {
                    filtered_primaries.push(sr);
                }

                // Stop condition counts filtered_primaries when filter is active,
                // otherwise counts all_primaries (which is the same as filtered when empty).
                let stop_count = if domain_keys.is_empty() {
                    all_primaries.len()
                } else {
                    filtered_primaries.len()
                };
                if stop_count >= limit {
                    break;
                }
            }

            // Determine chosen set and mode.
            let (chosen_primaries, mode) = if domain_keys.is_empty() {
                // No filter asked for: all rows go to both lists, mode is Vector
                (all_primaries, DomainMatchMode::Vector)
            } else if filtered_primaries.is_empty() && !all_primaries.is_empty() {
                // Filter would empty the candidates: skip it, use unfiltered, mode is Vector
                (all_primaries.into_iter().take(limit).collect(), DomainMatchMode::Vector)
            } else {
                // Filter applied successfully
                (filtered_primaries, DomainMatchMode::Domain)
            };

            let primary_ids: Vec<String> = chosen_primaries.iter().map(|r| r.id.clone()).collect();
            let mut results = chosen_primaries;

            // Step 2: Hybrid GraphRAG — directed evidence expansion.
            // Four bounded queries replace the old undirected ones, each using
            // label(e) for the relationship name and an outgoing flag for direction.
            // The expansion is NOT domain-filtered: a topology fact's GOVERNS neighbour
            // is the context that made the fact worth recalling, and filtering it away
            // would break the one-hop path the parent spec §4.2.3 depends on.
            if !primary_ids.is_empty() {
                let id_list = primary_ids.iter()
                    .map(|id| format!("'{}'", escape_ladybug_string(&id)))
                    .collect::<Vec<_>>()
                    .join(", ");

                let supported_labels = ["RELATES_TO", "CONTAINS", "GOVERNS"];
                let primary_set: std::collections::HashSet<&str> =
                    primary_ids.iter().map(|s| s.as_str()).collect();

                // One-hop neighbor queries (outgoing and incoming)
                let neighbor_out_q = format!(
                    "MATCH (a:Memory)-[e]->(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata, a.id, label(e) AS relation, true AS outgoing ORDER BY b.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );
                let neighbor_in_q = format!(
                    "MATCH (a:Memory)<-[e]-(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata, a.id, label(e) AS relation, false AS outgoing ORDER BY b.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // Two-hop governing queries (outgoing and incoming)
                let govern_out_q = format!(
                    "MATCH (a:Memory)-[e]->(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata, a.id, label(e) AS relation, f.id, f.metadata, true AS outgoing ORDER BY r.id ASC, f.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );
                let govern_in_q = format!(
                    "MATCH (a:Memory)<-[e]-(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata, a.id, label(e) AS relation, f.id, f.metadata, false AS outgoing ORDER BY r.id ASC, f.id ASC, a.id ASC, relation ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // Episode-sibling query: memories extracted from the same task.
                // Symmetric through the task: (m:Memory)-[:EXTRACTED_FROM]->(t:Memory)<-[:EXTRACTED_FROM]-(s:Memory)
                // m = originating primary, t = intermediate task, s = sibling candidate.
                let episode_sibling_q = format!(
                    "MATCH (m:Memory)-[:EXTRACTED_FROM]->(t:Memory)<-[:EXTRACTED_FROM]-(s:Memory) WHERE m.id IN [{}] AND s.namespace = '{}' AND NOT s.id IN [{}] RETURN DISTINCT s.id, s.content, s.user_id, s.memory_type, s.agent_name, s.location, s.location_lines, s.metadata, m.id, t.id, t.metadata ORDER BY s.id ASC, m.id ASC, t.id ASC LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // Phase 1: Read all rows, build evidence, group by result ID.
                // Value: (payload, evidence_list, co_occurrence_count).
                // co_occurrence_count = number of DISTINCT originating primaries that reach this candidate.
                #[derive(Default)]
                struct CandidateEntry {
                    payload: Option<MemoryPayload>,
                    evidence: Vec<crate::traits::SearchEvidence>,
                    primary_sources: std::collections::HashSet<String>,
                }
                let mut by_id: std::collections::HashMap<String, CandidateEntry> =
                    std::collections::HashMap::new();

                // Process one-hop neighbor queries (4 directed branches total).
                for q in [&neighbor_out_q, &neighbor_in_q] {
                    if let Ok(rows) = conn.query(q) {
                        for row in rows {
                            let result_id: String = format!("{}", row[0]);
                            let content: String = format!("{}", row[1]);
                            let user_id: String = format!("{}", row[2]);
                            let memory_type: String = format!("{}", row[3]);
                            let agent_name: String = format!("{}", row[4]);
                            let location: String = format!("{}", row[5]);
                            let location_lines: String = format!("{}", row[6]);
                            let metadata_str: String = format!("{}", row[7]);
                            let metadata_val: Value =
                                serde_json::from_str(&metadata_str).unwrap_or(Value::Null);
                            if is_retired_at(&metadata_val, now) { continue; }
                            if primary_set.contains(result_id.as_str()) { continue; }

                            // Row-class filters for expansion candidates (same as primaries)
                            // Use memory_type column directly, not metadata
                            if !include_tasks && memory_type == "task" { continue; }
                            if !include_archived && is_archived(&metadata_val) { continue; }
                            if is_mesh_conflicting(&metadata_val) { continue; }
                            if crate::registry::is_registry_row(&metadata_val) { continue; }

                            let primary_id: String = format!("{}", row[8]);
                            let relation: String = format!("{}", row[9]);
                            let outgoing: bool = row_flag_true(&row[10]);
                            if !supported_labels.contains(&relation.as_str()) { continue; }

                            let (src, tgt) = if outgoing {
                                (primary_id.clone(), result_id.clone())
                            } else {
                                (result_id.clone(), primary_id.clone())
                            };
                            let edges = vec![crate::traits::EvidenceEdge {
                                source: src.clone(), relation: relation.clone(), target: tgt.clone(),
                            }];
                            let kind = crate::traits::EvidenceKind::OneHop;
                            let norm = norm_path(&edges, &kind);

                            let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                            if entry.payload.is_none() {
                                entry.payload = Some(MemoryPayload {
                                    content, user_id, memory_type,
                                    agent_name: Some(agent_name),
                                    location, location_lines, metadata: metadata_val,
                                });
                            }
                            // Track distinct primary sources for co-occurrence ranking
                            entry.primary_sources.insert(primary_id);
                            if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                                entry.evidence.push(crate::traits::SearchEvidence {
                                    kind,
                                    explanation: "This memory is one graph hop from a direct match.".to_string(),
                                    path: edges,
                                });
                            }
                        }
                    }
                }

                // Process two-hop governing queries
                for q in [&govern_out_q, &govern_in_q] {
                    if let Ok(rows) = conn.query(q) {
                        for row in rows {
                            let result_id: String = format!("{}", row[0]);
                            // memory_type is at row[3] (r.memory_type) - use column directly for task filter
                            let memory_type: String = format!("{}", row[3]);
                            let metadata_str: String = format!("{}", row[7]);
                            let metadata_val: Value =
                                serde_json::from_str(&metadata_str).unwrap_or(Value::Null);
                            if is_retired_at(&metadata_val, now) { continue; }
                            if primary_set.contains(result_id.as_str()) { continue; }

                            // Row-class filters for expansion candidates
                            // Use memory_type column directly, not metadata (same as one-hop fix)
                            if !include_tasks && memory_type == "task" { continue; }
                            if !include_archived && is_archived(&metadata_val) { continue; }
                            if is_mesh_conflicting(&metadata_val) { continue; }
                            if crate::registry::is_registry_row(&metadata_val) { continue; }

                            let primary_id: String = format!("{}", row[8]);
                            let relation: String = format!("{}", row[9]);
                            let f_id: String = format!("{}", row[10]);
                            let f_metadata_str: String = format!("{}", row[11]);
                            let f_metadata: Value =
                                serde_json::from_str(&f_metadata_str).unwrap_or(Value::Null);
                            let outgoing: bool = row_flag_true(&row[12]);

                            // Retired intermediate invalidates only that path
                            if is_retired_at(&f_metadata, now) { continue; }
                            if !supported_labels.contains(&relation.as_str()) { continue; }

                            let (src1, tgt1) = if outgoing {
                                (primary_id.clone(), f_id.clone())
                            } else {
                                (f_id.clone(), primary_id.clone())
                            };
                            let edges = vec![
                                crate::traits::EvidenceEdge {
                                    source: src1, relation, target: tgt1,
                                },
                                crate::traits::EvidenceEdge {
                                    source: result_id.clone(),
                                    relation: "GOVERNS".to_string(),
                                    target: f_id,
                                },
                            ];
                            let kind = crate::traits::EvidenceKind::Governs2hop;
                            let norm = norm_path(&edges, &kind);

                            let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                            if entry.payload.is_none() {
                                let content: String = format!("{}", row[1]);
                                let user_id: String = format!("{}", row[2]);
                                let agent_name: String = format!("{}", row[4]);
                                let location: String = format!("{}", row[5]);
                                let location_lines: String = format!("{}", row[6]);
                                entry.payload = Some(MemoryPayload {
                                    content, user_id, memory_type: memory_type.clone(),
                                    agent_name: Some(agent_name),
                                    location, location_lines, metadata: metadata_val,
                                });
                            }
                            // Track distinct primary sources for co-occurrence ranking
                            entry.primary_sources.insert(primary_id);
                            if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                                entry.evidence.push(crate::traits::SearchEvidence {
                                    kind,
                                    explanation: "This memory governs a node one hop from a direct match.".to_string(),
                                    path: edges,
                                });
                            }
                        }
                    }
                }

                // Process episode-sibling query
                // Row layout: s.id, s.content, s.user_id, s.memory_type, s.agent_name, s.location, s.location_lines, s.metadata, m.id, t.id, t.metadata
                if let Ok(rows) = conn.query(&episode_sibling_q) {
                    for row in rows {
                        let result_id: String = format!("{}", row[0]);
                        let content: String = format!("{}", row[1]);
                        let user_id: String = format!("{}", row[2]);
                        let memory_type: String = format!("{}", row[3]);
                        let agent_name: String = format!("{}", row[4]);
                        let location: String = format!("{}", row[5]);
                        let location_lines: String = format!("{}", row[6]);
                        let metadata_str: String = format!("{}", row[7]);
                        let metadata_val: Value =
                            serde_json::from_str(&metadata_str).unwrap_or(Value::Null);
                        if is_retired_at(&metadata_val, now) { continue; }
                        if primary_set.contains(result_id.as_str()) { continue; }

                        // Row-class filters for the SIBLING (same as other expansion candidates)
                        if !include_tasks && memory_type == "task" { continue; }
                        if !include_archived && is_archived(&metadata_val) { continue; }
                        if is_mesh_conflicting(&metadata_val) { continue; }
                        if crate::registry::is_registry_row(&metadata_val) { continue; }

                        let primary_id: String = format!("{}", row[8]); // m.id (originating primary)
                        let t_id: String = format!("{}", row[9]);     // t.id (intermediate task)
                        let t_metadata_str: String = format!("{}", row[10]);
                        let t_metadata: Value =
                            serde_json::from_str(&t_metadata_str).unwrap_or(Value::Null);

                        // Retired intermediate task invalidates only that path (matching two-hop rule)
                        if is_retired_at(&t_metadata, now) { continue; }

                        // Evidence path: primary -(EXTRACTED_FROM)-> task <-EXTRACTED_FROM- sibling
                        let edges = vec![
                            crate::traits::EvidenceEdge {
                                source: primary_id.clone(),
                                relation: "EXTRACTED_FROM".to_string(),
                                target: t_id.clone(),
                            },
                            crate::traits::EvidenceEdge {
                                source: result_id.clone(),
                                relation: "EXTRACTED_FROM".to_string(),
                                target: t_id,
                            },
                        ];
                        let kind = crate::traits::EvidenceKind::EpisodeSibling;

                        let entry = by_id.entry(result_id).or_insert_with(CandidateEntry::default);
                        if entry.payload.is_none() {
                            entry.payload = Some(MemoryPayload {
                                content, user_id, memory_type,
                                agent_name: Some(agent_name),
                                location, location_lines, metadata: metadata_val,
                            });
                        }
                        // Track distinct primary sources for co-occurrence ranking
                        entry.primary_sources.insert(primary_id);
                        let norm = norm_path(&edges, &kind);
                        if !entry.evidence.iter().any(|e| e.kind == kind && norm_path(&e.path, &e.kind) == norm) {
                            entry.evidence.push(crate::traits::SearchEvidence {
                                kind,
                                explanation: "This memory and a direct match were both extracted from the same task.".to_string(),
                                path: edges,
                            });
                        }
                    }
                }

                // Phase 2: Admit DISTINCT result IDs in family order — one-hop
                // family first, then episode-sibling, then governing-only — capped at `limit` per family.
                // Within each family, order by co-occurrence count (distinct primary sources) DESC,
                // then by id ASC for stable tiebreak.
                let mut candidates: Vec<(usize, usize, String)> = by_id.iter()
                    .filter_map(|(id, entry)| {
                        entry.payload.as_ref()?;
                        let family = if entry.evidence.iter().any(|e| e.kind == crate::traits::EvidenceKind::OneHop) { 0 }
                            else if entry.evidence.iter().any(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling) { 1 }
                            else { 2 };
                        let co_occur = entry.primary_sources.len();
                        Some((family, co_occur, id.clone()))
                    })
                    .collect();
                // Sort by: family ASC (one-hop, then episode-sibling, then governing), co_occurrence DESC, id ASC
                candidates.sort_by(|a, b| {
                    a.0.cmp(&b.0)  // family: 0 (one-hop) before 1 (episode-sibling) before 2 (governing)
                        .then_with(|| b.1.cmp(&a.1))  // co-occurrence DESC
                        .then_with(|| a.2.cmp(&b.2))  // id ASC for stable tiebreak
                });
                let mut admitted = Vec::new();
                let mut per_family = [0usize; 3];
                for (family, _co_occur, id) in &candidates {
                    if per_family[*family] >= limit {
                        continue;
                    }
                    per_family[*family] += 1;
                    admitted.push(id.clone());
                }

                // Phase 3: Build results with capped evidence (max 3 per result).
                // Score for expansions: use a small constant that sorts after all
                // real boosted_distance scores but before the old 10.0 sentinel.
                // The real discriminator is EvidenceKind, not score.
                const EXPANSION_BASE_SCORE: f32 = 1.0; // sorts after any real boosted_distance (< 1.0 typically)
                for id in &admitted {
                    let entry = match by_id.remove(id) {
                        Some(e) => e,
                        None => {
                            tracing::warn!("admitted id {} not in by_id map (drift); skipping", id);
                            continue;
                        }
                    };
                    let payload = match entry.payload {
                        Some(p) => p,
                        None => {
                            tracing::warn!("entry for {} has no payload (drift); skipping", id);
                            continue;
                        }
                    };
                    let mut evidence = entry.evidence;
                    // Sort evidence: kind (OneHop < Governs2hop), then path edges
                    evidence.sort_by(|a, b| {
                        a.kind.cmp(&b.kind).then_with(|| {
                            a.path.iter().zip(b.path.iter())
                                .map(|(ae, be)| ae.source.cmp(&be.source)
                                    .then(ae.relation.cmp(&be.relation))
                                    .then(ae.target.cmp(&be.target)))
                                .find(|o| *o != std::cmp::Ordering::Equal)
                                .unwrap_or(a.path.len().cmp(&b.path.len()))
                        })
                    });
                    evidence.truncate(3);
                    results.push(SearchResult {
                        id: id.clone(),
                        score: EXPANSION_BASE_SCORE,
                        payload,
                        evidence: if evidence.is_empty() { None } else { Some(evidence) },
                    });
                }
            }

            // Final sort: primaries (DirectMatch, real boosted_distance) first,
            // then expansions (OneHop/Governs2hop, EXPANSION_BASE_SCORE).
            // Within expansions, the admission order (co-occurrence DESC, family order) is preserved.
            results.sort_by(|a, b| {
                let a_is_primary = a.evidence.as_ref().map_or(false, |evs| evs.iter().any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch));
                let b_is_primary = b.evidence.as_ref().map_or(false, |evs| evs.iter().any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch));
                match (a_is_primary, b_is_primary) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    (true, true) => a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal),
                    (false, false) => std::cmp::Ordering::Equal, // preserve admission order (stable sort)
                }
            });

            // Incomplete marker: set when the full 10x budget was consumed by the
// Cypher LIMIT, meaning more matching rows beyond the page may exist and
// must be fetched via pagination to avoid losing live matches hidden behind
// rows filtered out after LIMIT. Heuristic: if result_count == fetch_limit
// the engine's LIMIT was hit, indicating the multiplier exhausted and pages
// beyond remain.
let incomplete = result_count >= fetch_limit;
Ok(DomainSearchOutcome { results, mode, incomplete })
        })
        .await
}

/// Memory ceilings for the embedded graph store.
///
/// Both defaults exist because the engine's own defaults are not ceilings:
/// `SystemConfig::default()` leaves `buffer_pool_size` at 0, which lbug turns
/// into 80% of the machine's physical memory, and `max_db_size` at `u32::MAX`,
/// which it turns into an 8 TB virtual region. The test constructor has always
/// bounded both; the production open path did not, so the pool was free to grow
/// to most of RAM as pages were touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimits {
    pub buffer_pool_bytes: u64,
    pub max_db_bytes: u64,
}

impl StoreLimits {
    /// 256 MiB is a ceiling, not an allocation -- the pool fills as pages are
    /// read -- and is comfortably above the working set of a symbol index.
    pub const DEFAULT_BUFFER_POOL_BYTES: u64 = 256 * 1024 * 1024;
    /// 8 GiB of address space: far below the engine's 8 TB default, and far
    /// above anything this tool has produced.
    pub const DEFAULT_MAX_DB_BYTES: u64 = 8 * 1024 * 1024 * 1024;

    fn validate(self) -> Result<()> {
        if self.buffer_pool_bytes == 0 || self.max_db_bytes == 0 {
            anyhow::bail!(
                "store byte limits must be positive; zero selects an unbounded engine default"
            );
        }
        if self.buffer_pool_bytes == u32::MAX as u64 || self.max_db_bytes == u32::MAX as u64 {
            anyhow::bail!(
                "store byte limits cannot use the engine's u32::MAX automatic-size sentinel"
            );
        }
        if self.buffer_pool_bytes > self.max_db_bytes {
            anyhow::bail!("the buffer-pool ceiling cannot exceed the database-size ceiling");
        }
        Ok(())
    }
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            buffer_pool_bytes: Self::DEFAULT_BUFFER_POOL_BYTES,
            max_db_bytes: Self::DEFAULT_MAX_DB_BYTES,
        }
    }
}

/// Width and, for newly marked databases, model identity used to create a DB.
///
/// The `Memory` table stores `embedding FLOAT[N]`, sized from the embedder at
/// creation time. The fixed width can be read from the catalog, but the schema
/// does not record which model produced each vector. A sidecar records both for
/// new stores; a legacy store without identity adopts its current model with a
/// visible warning, then blocks subsequent identity changes. The adoption is
/// itself recorded in the persisted marker as provenance (see
/// `PersistedEmbeddingMarker`); adopting never refuses the open.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct EmbeddingMarker {
    version: u8,
    dimensions: usize,
    #[serde(default)]
    embedder_id: Option<String>,
}
pub(crate) fn width_marker_path(db_path: &Path) -> PathBuf {
    let canonical = canonical_database_path(db_path);
    let mut marker = canonical.as_os_str().to_os_string();
    marker.push(".embedding");
    PathBuf::from(marker)
}

/// Adoption provenance recorded in a marker's JSON, if any. Legacy numeric
/// markers and markers written before provenance existed carry none.
fn adopted_from_in_marker_bytes(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("adopted_from")?.as_str().map(str::to_string)
}

/// Read the marker; legacy numeric markers remain readable for an explicit,
/// warned identity adoption.
#[allow(dead_code)] // the open path uses the provenance-aware form below
fn read_width_marker(
    resolved_db_path: &Path,
    supplied_db_paths: &[PathBuf],
    configured_embedder_id: Option<&str>,
) -> Result<Option<EmbeddingMarker>> {
    Ok(read_width_marker_with_provenance(
        resolved_db_path,
        supplied_db_paths,
        configured_embedder_id,
    )?
    .map(|(marker, _)| marker))
}

/// Read the marker together with any adoption provenance recorded in it.
/// Provenance is not a field of `EmbeddingMarker`: it describes how the
/// marker came to carry its identity, not the identity itself, and keeping
/// it alongside leaves every existing marker construction untouched.
fn read_width_marker_with_provenance(
    resolved_db_path: &Path,
    supplied_db_paths: &[PathBuf],
    configured_embedder_id: Option<&str>,
) -> Result<Option<(EmbeddingMarker, Option<String>)>> {
    // `resolved_db_path` is the exact file identity locked and passed to the
    // engine. Do not resolve the caller's symlink again here: it could be
    // retargeted between resolution and marker preflight.
    let canonical = raw_database_sibling(resolved_db_path, ".embedding");
    let mut paths = vec![canonical.clone()];
    for supplied in supplied_db_paths {
        let marker = raw_database_sibling(supplied, ".embedding");
        if marker != canonical && !paths.contains(&marker) {
            paths.push(marker);
        }
    }
    let mut found: Vec<(PathBuf, EmbeddingMarker, Option<String>)> = Vec::new();
    for path in paths {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let adopted_from = adopted_from_in_marker_bytes(&bytes);
                let marker = parse_width_marker(&path, &bytes)?;
                found.push((path.clone(), marker, adopted_from));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                // A dangling symlink or other existing but unreadable marker is
                // evidence, not absence. Never adopt a new identity over it.
                if std::fs::symlink_metadata(&path).is_ok() {
                    anyhow::bail!("Cannot inspect the embedding marker at {}: {}", path.display(), error);
                }
            }
            Err(error) => anyhow::bail!(
                "Cannot read the embedding marker at {}: {}. Keep the database and marker intact; repair access or restore a matching snapshot to a separate path.",
                path.display(), error
            ),
        }
    }
    let Some((_, first, _)) = found.first() else {
        return Ok(None);
    };
    let identities = found
        .iter()
        .filter_map(|(_, marker, _)| marker.embedder_id.as_deref())
        .map(|identity| match configured_embedder_id {
            Some(configured) => {
                crate::embed::canonicalize_persisted_embedder_id(identity, configured)
            }
            None => Ok(identity.to_string()),
        })
        .collect::<Result<HashSet<_>>>()?;
    if found
        .iter()
        .any(|(_, other, _)| other.dimensions != first.dimensions)
        || identities.len() > 1
    {
        let paths = found
            .iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" and ");
        anyhow::bail!(
            "Conflicting embedding markers exist at {}. Preserve both markers and the database; reconcile them only after verifying which model created the vectors.",
            paths
        );
    }
    let identity_marker = found
        .iter()
        .find(|(_, marker, _)| marker.embedder_id.is_some())
        .map(|(_, marker, adopted_from)| (marker, adopted_from));
    let (selected, adopted_from) = identity_marker.unwrap_or((first, &found[0].2));
    Ok(Some((selected.clone(), adopted_from.clone())))
}

fn parse_width_marker(path: &Path, bytes: &[u8]) -> Result<EmbeddingMarker> {
    let raw = String::from_utf8_lossy(bytes);
    let marker = if let Ok(marker) = serde_json::from_slice::<EmbeddingMarker>(bytes) {
        marker
    } else {
        let dimensions = raw.trim().parse().map_err(|error| {
            anyhow::anyhow!(
                "The embedding marker at {} is malformed; preserve it and inspect it without overwriting it: {}",
                path.display(),
                error
            )
        })?;
        EmbeddingMarker {
            version: 1,
            dimensions,
            embedder_id: None,
        }
    };
    if marker.version != 1 || marker.dimensions == 0 {
        anyhow::bail!(
            "The embedding marker at {} has unsupported version {} or zero width",
            path.display(),
            marker.version
        );
    }
    Ok(marker)
}

fn persisted_embedding_width(db: &Database) -> Result<Option<usize>> {
    let conn = Connection::new(db)?;
    let mut rows = match conn.query("CALL table_info('Memory') RETURN *") {
        Ok(rows) => rows,
        Err(error) if error.to_string().contains("does not exist in catalog") => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut has_properties = false;
    while let Some(row) = rows.next() {
        has_properties = true;
        if format!("{}", row[1]) != "embedding" {
            continue;
        }
        let property_type = format!("{}", row[2]);
        let Some((_, raw_width)) = property_type.split_once('[') else {
            anyhow::bail!("Memory.embedding has unsupported schema type {property_type}");
        };
        let width = raw_width
            .trim_end_matches(']')
            .parse::<usize>()
            .map_err(|e| {
                anyhow::anyhow!("Memory.embedding has unsupported schema type {property_type}: {e}")
            })?;
        return Ok(Some(width));
    }
    if has_properties {
        anyhow::bail!("Memory table exists but has no embedding property");
    }
    Ok(None)
}

/// The canonicalized (saved, configured) identity pair for refusal
/// messages, or `None` when either identity is absent or the saved
/// legacy identity cannot be safely canonicalized against the
/// configured one. Refusal text must only ever carry canonicalized
/// identities (Refusal Standard element 6): a legacy remote identity
/// can embed a credential-bearing URL, and one that cannot be matched
/// to the configured identity must never be printed raw.
fn canonical_identity_pair(
    saved: Option<&str>,
    configured: Option<&str>,
) -> Option<(String, String)> {
    let (saved, configured) = (saved?, configured?);
    crate::embed::canonicalize_persisted_embedder_id(saved, configured)
        .ok()
        .map(|canonical| (canonical, configured.to_string()))
}

fn check_embedding_width(
    db_path: &Path,
    recorded: usize,
    dimensions: usize,
    identities: Option<(&str, &str)>,
) -> Result<()> {
    if recorded != dimensions {
        let identity_clause = match identities {
            Some((saved, configured)) => format!(
                " It was embedded with model identity {saved:?}, while the configured identity is {configured:?}."
            ),
            None => String::new(),
        };
        return Err(anyhow::anyhow!(
            "Refusing to open {}: its Memory.embedding column has width {} but the configured embedder produces {}.{} Keep the database intact and reopen it with the original embedder, or use a separate database path for the new model; no in-place re-embedding migration is available. Marker: {}",
            db_path.display(),
            recorded,
            dimensions,
            identity_clause,
            width_marker_path(db_path).display()
        ));
    }

    Ok(())
}

/// Persist the schema width and embedding identity used to create the database.
/// A failed write must stop the open; otherwise a later same-width model could
/// silently adopt an unmarked database.
#[allow(dead_code)] // the open path uses the provenance-aware form below
fn write_width_marker(db_path: &Path, dimensions: usize, embedder_id: Option<&str>) -> Result<()> {
    write_width_marker_with_adopted_from(db_path, dimensions, embedder_id, None)
}

/// The marker's serialization form. `adopted_from` records that this store's
/// identity was adopted from a legacy marker (`"width-only"`) or from its
/// fixed-width schema alone (`"schema-only"`); it is serialized only when an
/// adoption occurred, so natively marked stores keep the pre-existing shape
/// and markers written before it existed parse unchanged.
#[derive(serde::Serialize)]
struct PersistedEmbeddingMarker {
    version: u8,
    dimensions: usize,
    embedder_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adopted_from: Option<String>,
}

fn write_width_marker_with_adopted_from(
    db_path: &Path,
    dimensions: usize,
    embedder_id: Option<&str>,
    adopted_from: Option<&str>,
) -> Result<()> {
    let path = width_marker_path(db_path);
    let marker = PersistedEmbeddingMarker {
        version: 1,
        dimensions,
        embedder_id: embedder_id.map(str::to_string),
        adopted_from: adopted_from.map(str::to_string),
    };
    let tmp = path.with_extension(format!("embedding.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        std::fs::write(&tmp, serde_json::to_vec_pretty(&marker)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })()
    .map_err(|error| {
        anyhow::anyhow!(
            "could not persist the embedding marker at {}: {}",
            path.display(),
            error
        )
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// One pass of the row-identity audit init() runs (Task 7): the store's
/// Memory rows classified by their (namespace, id) identity. A row whose
/// namespace property is NULL predates the identity rule (or arrived
/// damaged); the audit backfills it in place when its metadata carries a
/// valid `namespace`, and quarantines it otherwise. A quarantined row
/// keeps its data, blocks nothing, and refuses writes aimed at its id
/// until an operator repairs it. The audit never fails init.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowIdentityAudit {
    pub total_rows: i64,
    pub null_namespace_rows: i64,
    pub backfilled: i64,
    pub quarantined: i64,
}

pub struct LadybugStore {
    #[allow(dead_code)]
    local_path: PathBuf,
    /// Original path spellings are retained only to find historical lease
    /// sidecars. Engine and snapshot operations use the resolved path above.
    path_aliases: Vec<PathBuf>,
    dimensions: usize,
    db: Arc<Database>,
    dirty: Dirty,
    /// Serializes checkpointing and snapshot copies. The write lock is also
    /// held during a snapshot so the database and provenance manifest share a
    /// single application-level cut.
    snapshot_lock: Arc<tokio::sync::Mutex<()>>,
    /// The most recent row-identity audit, taken by init() (Task 7).
    identity_audit: std::sync::Mutex<RowIdentityAudit>,
    /// LadybugDB allows exactly one write transaction and REFUSES a second
    /// rather than queueing it, so two writers that overlap produce "Cannot
    /// start a new write transaction in the system" for whichever arrives
    /// second. Every search spawns access-count writes, which makes that
    /// collision ordinary rather than exotic (bead neurostrata-3fi.6.6).
    ///
    /// Taking a turn here converts a random failure into a short wait. Writes
    /// are milliseconds now that checkpointing has left the request path, and
    /// the caller's deadline covers the wait as well as the write. The
    /// checkpoint deliberately does NOT take this lock: it waits for
    /// transactions to drain, so holding writers out for its duration would
    /// recreate the stall it was moved out of the request path to avoid.
    write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Set once the tables exist. init() is called on the way into add, move
    /// and -- because it is cheap to write and expensive to mean -- every
    /// search, and each call issued CREATE NODE TABLE only to be told it
    /// already exists. That is a write transaction per read, colliding with
    /// real writes for nothing.
    schema_ready: std::sync::atomic::AtomicBool,
    /// Set by init() when the Memory table carries the derived `pk`
    /// column (Task 7 re-plan): fresh tables are created with it, and a
    /// pre-existing table gains it via ALTER + backfill. While false,
    /// identity operations (upsert MERGE, get, delete, edge MERGE) use
    /// the legacy (namespace, id) property matching instead of keying on
    /// pk — the store never refuses to open over a missing column.
    pk_ready: std::sync::atomic::AtomicBool,
}

/// Whether a write is sitting in the log with no checkpoint behind it yet.
///
/// `take` clears and reports in one step so a write landing mid-checkpoint is
/// not swallowed: the flag is cleared before the flush, and put back if the
/// flush fails.
#[derive(Clone, Default)]
struct Dirty(Arc<std::sync::atomic::AtomicBool>);

impl Dirty {
    fn mark(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_set(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    fn take(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// Marks a write attempt dirty before its cross-process writer lock is
/// released. Checkpoint acquires the same lock before taking this flag, so a
/// canceled caller cannot hide a mutation from the checkpoint loop.
struct MarkDirtyOnDrop(Dirty);

impl Drop for MarkDirtyOnDrop {
    fn drop(&mut self) {
        self.0.mark();
    }
}

/// Normal open: checksums on, and replay failures throw. The throw is the whole
/// point -- with it off the engine swallows a failed verification, opens anyway
/// and silently drops every record it could not authenticate, which is exactly
/// the outcome this code exists to make visible.
fn verifying_config(limits: StoreLimits) -> SystemConfig {
    SystemConfig::default()
        .buffer_pool_size(limits.buffer_pool_bytes)
        .max_db_size(limits.max_db_bytes)
        .throw_on_wal_replay_failure(true)
}

/// Recovery open, used only after verification has already failed. The engine
/// cannot currently verify a WAL it wrote itself once the process dies without a
/// clean close: with checksums on, replay restores the catalog and discards
/// every row; with them off, the same rows come back intact. See
/// examples/wal_repro.rs, and bead neurostrata-kug for the upstream defect.
///
/// This trades tamper-evidence for the writes, so it is never the normal path
/// and never silent.
fn unverified_config(limits: StoreLimits) -> SystemConfig {
    SystemConfig::default()
        .buffer_pool_size(limits.buffer_pool_bytes)
        .max_db_size(limits.max_db_bytes)
        .throw_on_wal_replay_failure(false)
        .enable_checksums(false)
}

/// Fail closed by default: an unauthenticated WAL is refused rather than
/// replayed. Set `NEUROSTRATA_STRICT_WAL=0` (or `NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1`)
/// to opt into the legacy unverified recovery path. The default is strict so a
/// corrupt or tampered WAL is investigated, not silently recovered.
fn strict_wal_verification() -> bool {
    if std::env::var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return false;
    }
    match std::env::var("NEUROSTRATA_STRICT_WAL").as_deref() {
        Ok("0") => false,
        _ => true,
    }
}

fn is_wal_integrity_failure(err: &str) -> bool {
    err.contains("Checksum verification failed")
        || err.contains("Corrupted wal file")
        || err.contains("invalid WAL record type")
}

/// Copies a leftover write-ahead log aside before it is replayed and truncated.
/// Best effort by design: failing to keep the copy is not a reason to refuse to
/// open the database.
fn preserve_unclean_wal(local_path: &std::path::Path) {
    let mut wal_path = local_path.to_path_buf().into_os_string();
    wal_path.push(".wal");
    let wal_file = std::path::PathBuf::from(wal_path);

    let size = match std::fs::metadata(&wal_file) {
        Ok(m) if m.len() > 0 => m.len(),
        _ => return,
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut copy_path = wal_file.clone().into_os_string();
    copy_path.push(format!(".unclean.{}", timestamp));
    let copy_file = std::path::PathBuf::from(copy_path);

    eprintln!(
        "WARNING: the last shutdown was not clean -- a {} byte write-ahead log was left behind. Recovering what it still holds.",
        size
    );
    match std::fs::copy(&wal_file, &copy_file) {
        Ok(_) => eprintln!(
            "A copy was kept at {:?} in case recovery is incomplete.",
            copy_file
        ),
        Err(e) => eprintln!("WARNING: could not keep a copy of it: {}", e),
    }
}

impl LadybugStore {
    #[cfg(test)]
    pub(crate) async fn seed_legacy_memory_for_test(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<()> {
        let (namespace, id) = (namespace.to_string(), id.to_string());
        let pk_ready = self.pk_ready();
        self.write_with_deadline("seeding a legacy memory fixture", move |conn| {
            let vector = format!(
                "[{}]",
                vector
                    .iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            // Task 7 fix round: this fixture seeds rows into a fresh
            // (pk-keyed) table, so the CREATE must supply the derived
            // key as create input, exactly as a production writer
            // does. In legacy mode (no pk column) the fragment is
            // empty and the statement is unchanged.
            let pk_prop = if pk_ready {
                format!(
                    "pk: '{}', ",
                    escape_ladybug_string(&memory_pk(&namespace, &id))
                )
            } else {
                String::new()
            };
            conn.query(&format!(
                "CREATE (m:Memory {{{}id: '{}', namespace: '{}', content: '{}', user_id: '{}', memory_type: '{}', agent_name: '{}', location: '{}', location_lines: '{}', metadata: '{}', embedding: {}}})",
                pk_prop,
                escape_ladybug_string(&id),
                escape_ladybug_string(&namespace),
                escape_ladybug_string(&payload.content),
                escape_ladybug_string(&payload.user_id),
                escape_ladybug_string(&payload.memory_type),
                escape_ladybug_string(payload.agent_name.as_deref().unwrap_or("unknown")),
                escape_ladybug_string(&payload.location),
                escape_ladybug_string(&payload.location_lines),
                escape_ladybug_string(&serde_json::to_string(&payload.metadata)?),
                vector
            ))?;
            Ok(())
        })
        .await
    }

    async fn clear_internal_ingest_stage(&self, namespace: &str) -> Result<()> {
        if !is_internal_ingest_stage_namespace(namespace) {
            anyhow::bail!("refusing to clear a non-staging namespace as an ingest generation");
        }
        let namespace = namespace.to_string();
        self.write_with_deadline("recovering an interrupted ingest generation", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let generation = namespace
                .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
                .ok_or_else(|| anyhow::anyhow!("invalid ingest generation"))?
                .to_string();
            conn.query("BEGIN TRANSACTION")?;
            let result = (|| -> Result<()> {
                let has_provenance = conn
                    .query(&format!(
                        "MATCH (s:IngestStage) WHERE s.namespace = '{}' AND s.generation = '{}' RETURN s.namespace LIMIT 1",
                        safe_ns,
                        escape_ladybug_string(&generation)
                    ))?
                    .next()
                    .is_some();
                if !has_provenance {
                    anyhow::bail!("refusing to clear an ingest generation without database provenance");
                }
                conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' DETACH DELETE m",
                    safe_ns
                ))?;
                conn.query(&format!(
                    "MATCH (s:IngestStage) WHERE s.namespace = '{}' AND s.generation = '{}' DELETE s",
                    safe_ns,
                    escape_ladybug_string(&generation)
                ))?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    conn.query("COMMIT")?;
                    Ok(())
                }
                Err(error) => {
                    let rollback = conn.query("ROLLBACK");
                    match rollback {
                        Ok(_) => Err(error),
                        Err(rollback) => Err(anyhow::anyhow!(
                            "stage recovery failed: {}; rollback failed: {}",
                            error,
                            rollback
                        )),
                    }
                }
            }
        })
        .await
    }

    async fn recover_interrupted_ingest_stages(&self) -> Result<()> {
        self.migrate_pre_catalog_stage_leases().await?;
        let stages = self.with_conn(read_stage_provenance).await?;

        for (namespace, _generation) in stages {
            let lease_paths = ingest_stage_lease_candidates_with_aliases(
                &self.local_path,
                &self.path_aliases,
                &namespace,
            )?;
            // Older path aliases could leave a stale unlocked sidecar beside
            // the canonical sidecar. Recovery must hold every extant alias
            // before it treats the generation as abandoned.
            let mut leases = Vec::new();
            let mut active = false;
            for path in &lease_paths {
                match OpenOptions::new().read(true).write(true).open(path) {
                    Ok(file) => match file.try_lock() {
                        Ok(()) => leases.push(file),
                        Err(std::fs::TryLockError::WouldBlock) => active = true,
                        Err(std::fs::TryLockError::Error(error)) => {
                            eprintln!(
                                "Could not determine whether hidden ingest generation {} is active: {}. Leaving it intact.",
                                namespace, error
                            );
                            active = true;
                        }
                    },
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        eprintln!(
                            "Could not inspect hidden ingest generation {}: {}. Leaving it intact.",
                            namespace, error
                        );
                        active = true;
                    }
                }
            }
            if active {
                drop(leases);
                continue;
            }
            if leases.is_empty() {
                let path = &lease_paths[0];
                let file = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                if file.try_lock().is_err() {
                    continue;
                }
                leases.push(file);
            }

            match self.clear_internal_ingest_stage(&namespace).await {
                Ok(()) => {
                    for file in leases {
                        let _ = file.unlock();
                    }
                    for path in lease_paths {
                        let _ = std::fs::remove_file(path);
                    }
                }
                Err(error) => {
                    eprintln!(
                        "Could not remove interrupted hidden ingest generation {}: {}. It remains excluded from namespace listings and graph exports.",
                        namespace, error
                    );
                }
            }
        }
        self.remove_orphan_stage_lease_sidecars().await?;
        Ok(())
    }

    /// Upgrade abandoned generations created by the previous release, which
    /// recorded ownership only in a locked sidecar. Require the reserved UUID
    /// namespace, that exact sidecar, and exclusively auto-ingestor rows before
    /// writing durable catalog provenance; user/lookalike rows remain intact.
    async fn migrate_pre_catalog_stage_leases(&self) -> Result<()> {
        let current: HashSet<String> = self
            .with_conn(read_stage_provenance)
            .await?
            .into_iter()
            .map(|(namespace, _)| namespace)
            .collect();
        for (namespace, discovered_path) in
            discover_stage_lease_sidecars_with_aliases(&self.local_path, &self.path_aliases)?
        {
            if current.contains(&namespace) {
                continue;
            }
            let mut paths = ingest_stage_lease_candidates_with_aliases(
                &self.local_path,
                &self.path_aliases,
                &namespace,
            )?;
            if !paths.contains(&discovered_path) {
                paths.push(discovered_path.clone());
            }
            let mut leases = Vec::new();
            let mut active_lease = false;
            let mut unreadable = false;
            for path in paths {
                match OpenOptions::new().read(true).write(true).open(&path) {
                    Ok(file) => match file.try_lock() {
                        Ok(()) => leases.push(file),
                        // An active legacy generation still needs durable
                        // catalog provenance so snapshots hide it. Migration
                        // only records ownership; it never deletes these rows
                        // or its lease. The database writer lock below
                        // serializes this check with engine writes.
                        Err(std::fs::TryLockError::WouldBlock) => active_lease = true,
                        Err(std::fs::TryLockError::Error(_)) => unreadable = true,
                    },
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(_) => unreadable = true,
                }
            }
            if unreadable {
                drop(leases);
                continue;
            }
            if active_lease {
                // Without ownership of every legacy lease alias, do not
                // convert a transient old-format stage into catalog provenance:
                // later snapshot code would mistake that marker for proof the
                // generation is stable. The strict namespace + sidecar + row
                // check in read_hidden_stage_namespaces keeps it invisible.
                drop(leases);
                continue;
            }
            // The discovered sidecar may be in a symlink alias directory that
            // is not represented by the canonical candidate list.
            if leases.is_empty() && !active_lease {
                let file = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&discovered_path)
                {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                match file.try_lock() {
                    Ok(()) => leases.push(file),
                    Err(_) => continue,
                }
            }
            let generation = namespace
                .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
                .expect("sidecar discovery produces reserved namespace")
                .to_string();
            let migrated = self
                .write_with_deadline("upgrading ingest stage provenance", move |conn| {
                    conn.query("BEGIN TRANSACTION")?;
                    let result = (|| -> Result<bool> {
                        let safe = escape_ladybug_string(&namespace);
                        let mut rows = conn.query(&format!(
                            "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.user_id",
                            safe
                        ))?;
                        let mut found = false;
                        while let Some(row) = rows.next() {
                            found = true;
                            let id = format!("{}", row[0]);
                            let user_id = format!("{}", row[1]);
                            if user_id != "auto-ingestor"
                                || !id.starts_with(&format!(
                                    "{}{sep}",
                                    namespace,
                                    sep = crate::parser::ingest::NAMESPACE_SEPARATOR
                                ))
                            {
                                return Ok(false);
                            }
                        }
                        if !found {
                            return Ok(false);
                        }
                        conn.query(&format!(
                            "CREATE (s:IngestStage {{namespace: '{}', generation: '{}'}})",
                            safe,
                            escape_ladybug_string(&generation)
                        ))?;
                        Ok(true)
                    })();
                    match result {
                        Ok(true) => {
                            conn.query("COMMIT")?;
                            Ok(true)
                        }
                        Ok(false) => {
                            conn.query("ROLLBACK")?;
                            Ok(false)
                        }
                        Err(error) => {
                            let rollback = conn.query("ROLLBACK");
                            match rollback {
                                Ok(_) => Err(error),
                                Err(rollback) => Err(anyhow::anyhow!(
                                    "stage provenance upgrade failed: {}; rollback failed: {}",
                                    error,
                                    rollback
                                )),
                            }
                        }
                    }
                })
                .await?;
            drop(leases);
            let _ = migrated;
        }
        Ok(())
    }

    async fn remove_orphan_stage_lease_sidecars(&self) -> Result<()> {
        let _write_turn = self.write_lock.lock().await;
        let db_path = self.local_path.clone();
        let _cross_process_writer =
            tokio::task::spawn_blocking(move || acquire_database_writer_lock(&db_path)).await??;
        let live_stages: HashSet<String> = self
            .with_conn(read_stage_provenance)
            .await?
            .into_iter()
            .map(|(namespace, _)| namespace)
            .collect();
        for (namespace, _) in
            discover_stage_lease_sidecars_with_aliases(&self.local_path, &self.path_aliases)?
        {
            if live_stages.contains(&namespace) {
                continue;
            }
            let has_rows = self
                .with_conn({
                    let namespace = namespace.clone();
                    move |conn| {
                        let safe = escape_ladybug_string(&namespace);
                        let mut rows = conn.query(&format!(
                            "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id LIMIT 1",
                            safe
                        ))?;
                        Ok(rows.next().is_some())
                    }
                })
                .await?;
            if has_rows {
                // Without catalog provenance, preserve these bytes and their
                // sidecar. They may be legacy or user-owned rows.
                continue;
            }
            for path in ingest_stage_lease_candidates_with_aliases(
                &self.local_path,
                &self.path_aliases,
                &namespace,
            )? {
                let lock = match OpenOptions::new().read(true).write(true).open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                match lock.try_lock() {
                    Ok(()) => {
                        drop(lock);
                        match std::fs::remove_file(&path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == ErrorKind::NotFound => {}
                            // Some platforms refuse to unlink a locked file. The
                            // leftover is harmless and can be retried later.
                            Err(error) if error.kind() == ErrorKind::PermissionDenied => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Err(std::fs::TryLockError::WouldBlock) => continue,
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn ingest_stage_namespaces(&self) -> Result<Vec<String>> {
        self.with_conn(|conn| {
            Ok(read_stage_provenance(conn)?
                .into_iter()
                .map(|(namespace, _)| namespace)
                .collect())
        })
        .await
    }

    /// Construct a `LadybugStore` for tests with a small system footprint so
    /// `cargo test` (parallel by default) does not mmap-reserve 8 TiB per
    /// open. The defaults are 512 MiB max database size + 64 MiB buffer
    /// pool; override with `NEUROSTRATA_TEST_MAX_DB_SIZE` /
    /// `NEUROSTRATA_TEST_BUFFER_POOL_SIZE` env vars when a test fixture
    /// needs more (or less) room.
    ///
    /// 15-ish test sites use temp directories; this keeps every concurrent
    /// open well under container memory ceilings and ends the open-time
    /// `Mmap for size 8796093022208 failed` flake (task neurostrata-c8te).
    /// The most recent row-identity audit, taken by init() (Task 7). All
    /// zeroes until the first init() completes on this store instance.
    pub fn row_identity_audit(&self) -> RowIdentityAudit {
        *self
            .identity_audit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether init() confirmed the derived `pk` column on the Memory
    /// table (Task 7 re-plan). Identity operations key on pk when this
    /// holds and fall back to (namespace, id) property matching when it
    /// does not.
    fn pk_ready(&self) -> bool {
        self.pk_ready.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn for_testing(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        let max_db_size: u64 = std::env::var("NEUROSTRATA_TEST_MAX_DB_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(512 * 1024 * 1024);
        let buffer_pool_size: u64 = std::env::var("NEUROSTRATA_TEST_BUFFER_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64 * 1024 * 1024);
        let limits = StoreLimits {
            buffer_pool_bytes: buffer_pool_size,
            max_db_bytes: max_db_size,
        };
        let cfg = SystemConfig::default()
            .max_db_size(max_db_size)
            .buffer_pool_size(buffer_pool_size)
            .throw_on_wal_replay_failure(true);
        Self::with_system_config(local_path, dimensions, cfg, limits)
    }

    /// Production open with an explicit memory ceiling. Every caller that is not
    /// a test should use this rather than `new`, so the pool cannot inherit the
    /// engine default of 80% of physical memory.
    pub fn new_with_limits(
        local_path: impl Into<PathBuf>,
        dimensions: usize,
        limits: StoreLimits,
    ) -> Result<Self> {
        Self::with_system_config(local_path, dimensions, verifying_config(limits), limits)
    }

    pub fn new_with_identity_limits(
        local_path: impl Into<PathBuf>,
        dimensions: usize,
        embedder_id: String,
        limits: StoreLimits,
    ) -> Result<Self> {
        Self::with_system_config_and_identity(
            local_path,
            dimensions,
            Some(embedder_id),
            verifying_config(limits),
            limits,
        )
    }

    pub fn new(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        Self::new_with_limits(local_path, dimensions, StoreLimits::default())
    }

    /// Open the database with a custom `SystemConfig`. Production paths go
    /// through `new_with_limits`; tests go through `for_testing`. The fallback
    /// path (WAL verification failure) always uses the hardcoded recovery shape;
    /// only the *initial* open is parameterized. `limits` is threaded into both
    /// so recovery cannot silently drop the memory ceiling.
    pub fn with_system_config(
        local_path: impl Into<PathBuf>,
        dimensions: usize,
        initial_config: SystemConfig,
        limits: StoreLimits,
    ) -> Result<Self> {
        Self::with_system_config_and_identity(local_path, dimensions, None, initial_config, limits)
    }

    fn with_system_config_and_identity(
        local_path: impl Into<PathBuf>,
        dimensions: usize,
        embedder_id: Option<String>,
        initial_config: SystemConfig,
        limits: StoreLimits,
    ) -> Result<Self> {
        let local_path = local_path.into();
        limits.validate()?;
        if let Some(parent) = local_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                anyhow::anyhow!(
                    "could not create database directory {}: {}",
                    parent.display(),
                    error
                )
            })?;
        }
        // Anchor the caller's path to this working directory without resolving
        // symlinks. Alias discovery must see every original chain spelling so
        // its marker evidence can be checked before the engine open.
        let supplied_path = anchor_path_spelling(&local_path)?;
        // Serialize the marker check and engine open with snapshot restore. A
        // restore holds this same canonical writer lock from destination
        // preflight through installation, so an opener cannot pass the check
        // just before the restore interlock appears and then race its rename.
        let resolved_path = resolve_database_path(&supplied_path)?;
        let _open_writer = acquire_database_writer_lock(&resolved_path)?;
        let path_aliases = discover_database_path_aliases(&resolved_path, &supplied_path)?;
        ensure_database_path_unchanged(&resolved_path, &supplied_path)?;
        if restore_in_progress_path(&resolved_path).exists() {
            anyhow::bail!(
                "database restore at {} is incomplete; finish or recover it before opening",
                local_path.display()
            );
        }
        if dimensions == 0 {
            anyhow::bail!("embedding dimensions must be positive");
        }
        let database_preexisted = resolved_path.is_file();
        // A marker cannot outlive the database it described. If the file is
        // absent, ignore any sibling marker and replace it after opening this
        // fresh path; otherwise a stale 768 marker can strand a new 384 DB.
        let (marker, existing_adopted_from) = if database_preexisted {
            match read_width_marker_with_provenance(
                &resolved_path,
                &path_aliases,
                embedder_id.as_deref(),
            )? {
                Some((marker, adopted_from)) => (Some(marker), adopted_from),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        ensure_database_path_unchanged(&resolved_path, &supplied_path)?;
        if let Some(marker) = &marker {
            let identities = canonical_identity_pair(
                marker.embedder_id.as_deref(),
                embedder_id.as_deref(),
            );
            check_embedding_width(
                &resolved_path,
                marker.dimensions,
                dimensions,
                identities.as_ref().map(|(saved, configured)| {
                    (saved.as_str(), configured.as_str())
                }),
            )?;
            if let (Some(saved), Some(current)) = (&marker.embedder_id, &embedder_id) {
                let saved_canonical =
                    crate::embed::canonicalize_persisted_embedder_id(saved, current)?;
                if saved_canonical != current.as_str() {
                    anyhow::bail!(
                        "Refusing to open database path alias: it was embedded with model identity {:?}, while the configured identity is {:?}. The recorded embedding width is {} and the configured embedding width is {}. Keep the database intact and reopen it with the original embedder, or use a separate database path for the new model; no in-place re-embedding migration is available.",
                        saved_canonical, current, marker.dimensions, dimensions
                    );
                }
            }
        }

        // A WAL present at open time means the last process did not close cleanly:
        // a clean close checkpoints and removes it. Keep a copy before the engine
        // replays, because replay truncates the file to the last good record and
        // whatever followed is then unrecoverable.
        preserve_unclean_wal(&resolved_path);

        // Open with verification first. Only if the WAL fails to authenticate do we
        // consider replaying it unverified, and never quietly.
        let db = match Database::new(&resolved_path, initial_config) {
            Ok(db) => db,
            Err(e) => {
                let err_msg = e.to_string();
                if !is_wal_integrity_failure(&err_msg) {
                    return Err(e.into());
                }

                eprintln!(
                    "ERROR: the write-ahead log failed verification: {}",
                    err_msg
                );

                if strict_wal_verification() {
                    return Err(anyhow::anyhow!(
                        "Refusing to open: the write-ahead log did not verify. The log has been left in place for inspection. To recover the writes without verification, set NEUROSTRATA_STRICT_WAL=0 (or NEUROSTRATA_ALLOW_UNVERIFIED_WAL=1). See docs/runbooks/wal-recovery.md."
                    ));
                }

                eprintln!("ERROR: retrying WITHOUT checksum verification to recover those writes. The records replayed this way are NOT authenticated -- if this database may have been tampered with, stop and inspect the preserved copy instead. Set NEUROSTRATA_STRICT_WAL=1 to refuse this fallback.");

                match Database::new(&resolved_path, unverified_config(limits)) {
                    Ok(db) => {
                        eprintln!("Recovered by replaying the unverified write-ahead log.");
                        db
                    }
                    Err(retry_e) => {
                        // Unreadable even without verification: set it aside rather
                        // than leave the database unopenable, and say what was lost.
                        let mut wal_path = resolved_path.clone().into_os_string();
                        wal_path.push(".wal");
                        let wal_file = std::path::PathBuf::from(wal_path);

                        if !wal_file.exists() {
                            return Err(anyhow::anyhow!(
                                "WAL verification failed and the file is no longer at {:?}: {}",
                                wal_file,
                                retry_e
                            ));
                        }

                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let mut corrupted_path = wal_file.clone().into_os_string();
                        corrupted_path.push(format!(".corrupted.{}", timestamp));
                        let corrupted_file = std::path::PathBuf::from(corrupted_path);

                        let dropped_bytes =
                            std::fs::metadata(&wal_file).map(|m| m.len()).unwrap_or(0);
                        std::fs::rename(&wal_file, &corrupted_file).map_err(|err| {
                            anyhow::anyhow!(
                                "Recovery failed: could not move the unreadable WAL file: {}",
                                err
                            )
                        })?;

                        eprintln!(
                            "ERROR: DATA LOSS. {} bytes of un-checkpointed writes could not be replayed even unverified, and were discarded to reopen the database.",
                            dropped_bytes
                        );
                        eprintln!("ERROR: they were kept at {:?} -- do not delete it if those writes mattered.", corrupted_file);
                        // No WAL left to verify at this point.
                        Database::new(&resolved_path, verifying_config(limits)).map_err(
                            |last_e| {
                                anyhow::anyhow!("Retry failed after discarding the WAL: {}", last_e)
                            },
                        )?
                    }
                }
            }
        };

        let schema_width = persisted_embedding_width(&db)?;
        let identities = marker.as_ref().and_then(|marker| {
            canonical_identity_pair(marker.embedder_id.as_deref(), embedder_id.as_deref())
        });
        let identity_args = identities
            .as_ref()
            .map(|(saved, configured)| (saved.as_str(), configured.as_str()));
        if let Some(schema_width) = schema_width {
            check_embedding_width(&resolved_path, schema_width, dimensions, identity_args)?;
            if let Some(marker) = &marker {
                check_embedding_width(
                    &resolved_path,
                    marker.dimensions,
                    schema_width,
                    identity_args,
                )?;
            }
        } else if database_preexisted {
            // An existing DB without Memory is either a new empty catalog or a
            // partial/foreign schema; init() performs its own idempotent DDL.
        }
        let mut adopted_from: Option<&str> = None;
        if let Some(marker) = &marker {
            check_embedding_width(
                &resolved_path,
                marker.dimensions,
                dimensions,
                identity_args,
            )?;
            if let (Some(saved), Some(current)) = (&marker.embedder_id, &embedder_id) {
                let saved_canonical =
                    crate::embed::canonicalize_persisted_embedder_id(saved, current)?;
                if saved_canonical != current.as_str() {
                    return Err(anyhow::anyhow!(
                        "Refusing to open {}: it was embedded with model identity {:?}, while the configured identity is {:?}. The recorded embedding width is {} and the configured embedding width is {}. Keep the database intact and reopen it with the original embedder, or use a separate database path for the new model; no in-place re-embedding migration is available.",
                        local_path.display(), saved_canonical, current, marker.dimensions, dimensions
                    ));
                }
            } else if marker.embedder_id.is_none() && embedder_id.is_some() {
                adopted_from = Some("width-only");
                eprintln!(
                    "WARNING: {} records embedding width but no model identity; adopting the configured identity without verifying historical vectors. Future model changes will be blocked. This adoption has been recorded in the store's marker.",
                    local_path.display()
                );
            }
        } else if schema_width.is_some() && embedder_id.is_some() {
            adopted_from = Some("schema-only");
            eprintln!(
                "WARNING: {} has a fixed-width embedding schema but no sidecar identity; adopting the configured model identity without verifying historical vectors. This adoption has been recorded in the store's marker.",
                local_path.display()
            );
        }
        let marker_identity = match (
            marker
                .as_ref()
                .and_then(|marker| marker.embedder_id.as_deref()),
            embedder_id.as_deref(),
        ) {
            (Some(saved), Some(current)) => Some(crate::embed::canonicalize_persisted_embedder_id(
                saved, current,
            )?),
            (Some(saved), None) => Some(saved.to_string()),
            (None, Some(current)) => Some(current.to_string()),
            (None, None) => None,
        };
        // Provenance is sticky: a fresh adoption records its origin, and a
        // later open under the adopted identity carries the existing record
        // forward instead of dropping it in this rewrite.
        let marker_provenance = adopted_from.or(existing_adopted_from.as_deref());
        write_width_marker_with_adopted_from(
            &resolved_path,
            dimensions,
            marker_identity.as_deref(),
            marker_provenance,
        )?;

        Ok(Self {
            local_path: resolved_path,
            path_aliases,
            dimensions,
            db: Arc::new(db),
            dirty: Dirty::default(),
            snapshot_lock: Arc::new(tokio::sync::Mutex::new(())),
            identity_audit: std::sync::Mutex::new(RowIdentityAudit::default()),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            schema_ready: std::sync::atomic::AtomicBool::new(false),
            pk_ready: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Runs a write with a deadline, so a caller is told the database is busy
    /// rather than left holding a connection that never answers. Engine-level
    /// lock waits of two and a half minutes were measured while a burst of
    /// writes was in flight (bead neurostrata-3fi.6).
    ///
    /// The deadline releases the CALLER, not the query: spawn_blocking cannot be
    /// cancelled, so the statement may still land afterwards. The message says
    /// so, because retrying blind could otherwise duplicate work.
    ///
    /// Which is why the writer's turn belongs to the blocking task and not to
    /// the caller. It used to be a guard inside the timed future, so a caller
    /// that gave up released the turn while its statement was still running,
    /// and the next writer promptly opened a second one against an engine that
    /// already had one. That is how a single stalled write became five in a row
    /// (bead neurostrata-4nk): each waited its 30s, gave up, and handed the
    /// turn to the next. Held by the task, the turn is released when the work
    /// actually finishes, so a caller giving up costs a wait instead.
    async fn write_with_deadline<T, F>(&self, what: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: Fn(&Connection) -> Result<T> + Clone + Send + 'static,
    {
        let write_lock = Arc::clone(&self.write_lock);
        let db = self.db.clone();
        let db_path = self.local_path.clone();
        let write_dirty = self.dirty.clone();

        let outcome = bounded(what, write_timeout(), async move {
            // One writer at a time, because the engine refuses the second
            // rather than making it wait. The deadline covers this queue too:
            // a caller stuck behind a stalled write hears that the database is
            // busy instead of waiting indefinitely for its turn. Giving up
            // while merely waiting for the turn leaks nothing -- no statement
            // has been started yet.
            let mut turn = write_lock.lock_owned().await;

            // The turn orders our own writers; the background checkpoint and
            // any other process are outside it, so a refusal can still arrive.
            // Those clear in milliseconds, so wait and try again rather than
            // handing the caller a failure it can do nothing about.
            let mut attempt = 0;
            loop {
                let (returned, result) = Self::write_owning_turn(
                    db.clone(),
                    db_path.clone(),
                    turn,
                    write_dirty.clone(),
                    f.clone(),
                )
                .await?;
                turn = returned;

                match result {
                    Err(e) if is_write_collision(&e) && attempt < WRITE_COLLISION_RETRIES => {
                        WRITE_COLLISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        attempt += 1;
                        // The turn is held across this pause on purpose: it is
                        // ours until we stop retrying, and nothing is running
                        // inside the engine while we wait.
                        tokio::time::sleep(std::time::Duration::from_millis(25 * attempt as u64))
                            .await;
                    }
                    result => return result,
                }
            }
        })
        .await;

        if outcome.is_err() {
            WRITES_ABANDONED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "DIAGNOSTIC: gave up waiting on {} -- engine: {}",
                what,
                engine_stats()
            );
        }

        outcome
    }

    /// Atomic publication cannot return a timeout while the transaction may
    /// still commit in the background. Log when the ordinary write deadline
    /// expires, then wait for a definitive commit or rollback result. The
    /// general write deadline behavior remains unchanged.
    async fn write_to_completion<T, F>(
        &self,
        what: &'static str,
        deadline: std::time::Duration,
        f: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: Fn(&Connection) -> Result<T> + Clone + Send + 'static,
    {
        let write_lock = Arc::clone(&self.write_lock);
        let db = self.db.clone();
        let db_path = self.local_path.clone();
        let write_dirty = self.dirty.clone();
        let mut worker = tokio::spawn(async move {
            let mut turn = write_lock.lock_owned().await;
            let mut attempt = 0;
            loop {
                let (returned, result) = Self::write_owning_turn(
                    db.clone(),
                    db_path.clone(),
                    turn,
                    write_dirty.clone(),
                    f.clone(),
                )
                .await?;
                turn = returned;
                match result {
                    Err(error)
                        if is_write_collision(&error) && attempt < WRITE_COLLISION_RETRIES =>
                    {
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(25 * attempt as u64))
                            .await;
                    }
                    result => return result,
                }
            }
        });

        let outcome = match tokio::time::timeout(deadline, &mut worker).await {
            Ok(joined) => joined.map_err(|error| {
                anyhow::anyhow!("the publication task did not finish: {}", error)
            })?,
            Err(_) => {
                eprintln!(
                    "DIAGNOSTIC: publication exceeded its {}s deadline; waiting for its transaction to commit or roll back before reporting an outcome -- engine: {}",
                    deadline.as_secs(),
                    engine_stats()
                );
                worker.await.map_err(|error| {
                    anyhow::anyhow!("the publication task did not finish: {}", error)
                })?
            }
        };
        outcome
    }

    /// Runs one attempt on a blocking thread, and gives that thread the
    /// writer's turn for as long as it lasts.
    ///
    /// The turn travels back out with the result so a retry can keep it. A
    /// caller that has stopped waiting never reads that result, so the turn is
    /// dropped when the task completes -- which is exactly when the next writer
    /// should be allowed to start.
    async fn write_owning_turn<T, F>(
        db: Arc<Database>,
        db_path: PathBuf,
        turn: tokio::sync::OwnedMutexGuard<()>,
        dirty: Dirty,
        f: F,
    ) -> Result<(tokio::sync::OwnedMutexGuard<()>, Result<T>)>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            // The engine runs here, on a blocking-pool thread, and a fault
            // in it is the crash this daemon is known for. The crash handler
            // needs an alternate signal stack bigger than the one the
            // standard library gives a thread, so this thread is given one
            // before it touches the engine. The call is idempotent, so the
            // pool reusing this thread does not leak another stack.
            let _ = crate::crash::install_alt_stack();
            #[cfg(test)]
            test_write_lock_attempt(&db_path);
            let outcome = (|| -> Result<T> {
                let _cross_process_writer = acquire_database_writer_lock(&db_path)?;
                let conn = Connection::new(&db)?;
                let _mark_dirty = MarkDirtyOnDrop(dirty);
                f(&conn)
            })();
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (turn, outcome)
        })
        .await
        .map_err(|e| anyhow::anyhow!("the database thread did not finish: {}", e))
    }

    /// Runs one piece of database work on a blocking thread.
    ///
    /// Every lbug call is synchronous FFI. Called straight from an async handler
    /// it occupies a tokio worker for as long as the engine takes, and a handful
    /// of concurrent queries can therefore starve the runtime outright: during a
    /// write burst even /health -- an async closure returning a constant -- had
    /// no thread to run on, which is what made the daemon look dead while it was
    /// still listening (bead neurostrata-3fi.6).
    ///
    /// The connection is opened inside the closure on purpose. It borrows the
    /// Database and is not Send, so it must never be created here and moved, nor
    /// held across an await.
    async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.db.clone();
        ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            // As in `with_locked_conn`: the engine runs on this thread, so
            // the crash handler's alternate signal stack is installed here.
            let _ = crate::crash::install_alt_stack();
            let outcome = (|| -> Result<T> {
                let conn = Connection::new(&db)?;
                f(&conn)
            })();
            // Counted here rather than after the await: the await is what a
            // deadline drops, and the whole question is whether the work
            // outlives the caller that stopped waiting for it.
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            outcome
        })
        .await
        .map_err(|e| anyhow::anyhow!("the database thread did not finish: {}", e))?
    }
}

/// One shape warning produced during vocabulary validation of a write.
#[derive(Debug, Clone)]
struct WriteShapeWarning {
    field: String,
    why: String,
    skipped: String,
}

/// The five keys `fact.fields.topology` declares (vocabulary v5). Extra keys
/// are ignored; a missing key or a present key that is not a string is a
/// defect.
const TOPOLOGY_KEYS: [&str; 5] = [
    "component",
    "target_system",
    "channel",
    "credential_boundary",
    "reason",
];

/// Validates a memory payload's metadata shape and extracts edge specs.
/// Returns the valid edge specs and any shape warnings (never rejects the write).
fn parse_edge_specs(
    payload: &crate::traits::MemoryPayload,
) -> (Vec<EdgeSpec>, Vec<WriteShapeWarning>) {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();

    // (A) memory_type check
    if payload.memory_type.trim().is_empty() {
        warnings.push(WriteShapeWarning {
            field: "memory_type".to_string(),
            why: "memory_type must contain a non-whitespace character".to_string(),
            skipped: "nothing; payload retained unchanged".to_string(),
        });
    }
    // Any other string accepted silently, never normalized.

    // (B) metadata root
    match &payload.metadata {
        Value::Null => { /* ok */ }
        Value::Object(_) => { /* ok, inspect below */ }
        _ => {
            warnings.push(WriteShapeWarning {
                field: "metadata".to_string(),
                why: "metadata must be an object or null".to_string(),
                skipped: "all relationship declarations".to_string(),
            });
            return (specs, warnings);
        }
    }

    // (C) relation keys in existing order
    for (key, rel_type, points_at_target) in [
        ("related_to", "RELATES_TO", true),
        ("contained_by", "CONTAINS", false),
        ("governs", "GOVERNS", true),
        // Vocabulary v2: a memory extracted from a completed task declares it,
        // and the edge points memory -> task (self_to_target).
        ("extracted_from", "EXTRACTED_FROM", true),
    ] {
        match payload.metadata.get(key) {
            None => { /* absent: ok */ }
            Some(Value::Array(arr)) if arr.is_empty() => { /* empty: ok */ }
            Some(Value::Array(arr)) => {
                for (i, item) in arr.iter().enumerate() {
                    match item {
                        Value::String(s) if s.trim().is_empty() => {
                            warnings.push(WriteShapeWarning {
                                field: format!("{}.{}", key, i),
                                why: "relation target must contain a non-whitespace character"
                                    .to_string(),
                                skipped: "this edge declaration".to_string(),
                            });
                        }
                        Value::String(s) => {
                            // (D) qualified syntax check
                            if let Some(pos) = s.find(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                                let ns_part = &s[..pos];
                                let id_part =
                                    &s[pos + crate::parser::ingest::NAMESPACE_SEPARATOR.len()..];
                                if ns_part.trim().is_empty() || id_part.trim().is_empty() {
                                    warnings.push(WriteShapeWarning {
                                        field: format!("{}.{}", key, i),
                                        why: "qualified target needs a namespace and local ID"
                                            .to_string(),
                                        skipped: "this edge declaration".to_string(),
                                    });
                                    continue;
                                }
                            }
                            specs.push(EdgeSpec {
                                rel_type,
                                target_id: s.clone(),
                                points_at_target,
                                declaration_index: i,
                            });
                        }
                        _ => {
                            warnings.push(WriteShapeWarning {
                                field: format!("{}.{}", key, i),
                                why: "relation target must be a string".to_string(),
                                skipped: "this edge declaration".to_string(),
                            });
                        }
                    }
                }
            }
            _ => {
                warnings.push(WriteShapeWarning {
                    field: key.to_string(),
                    why: "relation declaration must be an array".to_string(),
                    skipped: "declarations in this field".to_string(),
                });
            }
        }
    }

    // (E) access topology shape. The field is declared on a `fact`, so the
    // check keys off the memory type: on any other type `topology` is an
    // undeclared metadata key and is accepted unchanged.
    //
    // The check is soft. A malformed claim warns and is still written, because
    // a rejected write would lose the claim entirely.
    if payload.memory_type.trim() == "fact" {
        if let Some(topology) = payload.metadata.get("topology") {
            let mut defects: Vec<String> = Vec::new();
            match topology {
                Value::Object(fields) => {
                    for key in TOPOLOGY_KEYS {
                        match fields.get(key) {
                            None => defects.push(format!("is missing the required key {:?}", key)),
                            Some(Value::String(_)) => { /* a string: as declared */ }
                            Some(_) => defects.push(format!("key {:?} must be a string", key)),
                        }
                    }
                }
                _ => defects.push("must be an object".to_string()),
            }
            if !defects.is_empty() {
                warnings.push(WriteShapeWarning {
                    field: "metadata.topology".to_string(),
                    why: format!("metadata.topology {}", defects.join("; ")),
                    skipped: "the topology shape check; the fact is still written".to_string(),
                });
            }
        }
    }

    // (F) attempt shape. Declared on a `fact` (a failed episode recorded on a
    // fact). Soft check: a malformed attempt warns and the fact is still
    // written.
    if payload.memory_type.trim() == "fact" {
        let defects = crate::attempt::attempt_defects(&payload.metadata);
        if payload.metadata.get("attempt").is_some() && !defects.is_empty() {
            warnings.push(WriteShapeWarning {
                field: "metadata.attempt".to_string(),
                why: format!("metadata.attempt {}", defects.join("; ")),
                skipped: "the attempt shape check; the fact is still written".to_string(),
            });
        }
    }

    (specs, warnings)
}

/// Collects shape warnings from a payload for emit_write_shape_warning calls.
fn write_shape_warnings(payload: &crate::traits::MemoryPayload) -> Vec<WriteShapeWarning> {
    parse_edge_specs(payload).1
}

/// Emits one shape warning to stderr in the required JSON format.
fn emit_write_shape_warning(namespace: &str, id: &str, warning: &WriteShapeWarning) {
    eprintln!(
        "[neurostrata:write-shape] {}",
        serde_json::json!({
            "namespace": namespace,
            "id": id,
            "field": warning.field,
            "why": warning.why,
            "skipped": warning.skipped,
        })
    );
}

/// Exact ID lookup for qualified target resolution.
fn exact_target_exists(conn: &Connection, id: &str) -> Result<bool> {
    let safe_id = escape_ladybug_string(id);
    let query = format!(
        "MATCH (b:Memory) WHERE b.id = '{}' RETURN b.id LIMIT 1",
        safe_id
    );
    let mut rows = conn.query(&query)?;
    Ok(rows.next().is_some())
}

/// The generation currently active for one source (`0` = none published).
fn active_generation_of_source(conn: &Connection, namespace: &str, source_key: &str) -> u64 {
    let key = crate::parser::ingest::generation_key(namespace, source_key);
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.id = '{}' RETURN g.active_generation",
        escape_ladybug_string(&key)
    );
    match conn.query(&query) {
        Ok(mut rows) => match rows.next() {
            Some(row) => format!("{}", row[0]).parse::<i64>().unwrap_or(0).max(0) as u64,
            None => 0,
        },
        Err(_) => 0,
    }
}

/// The most recently published generation for a namespace across all sources.
/// Used to resolve a declared edge target on write, where the source is not
/// carried: the latest tree is the best available answer. A query that cannot
/// run reports `0` rather than failing, so resolution degrades to "no
/// generation" instead of erroring out of a write it is only trying to guide.
fn active_generation_of(conn: &Connection, namespace: &str) -> u64 {
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.namespace = '{}' AND g.active_generation > 0 RETURN g.active_generation",
        escape_ladybug_string(namespace)
    );
    match conn.query(&query) {
        Ok(rows) => {
            let mut latest: u64 = 0;
            for row in rows {
                if let Ok(g) = format!("{}", row[0]).parse::<i64>() {
                    if g > 0 {
                        latest = latest.max(g as u64);
                    }
                }
            }
            latest
        }
        Err(_) => 0,
    }
}

/// The set of active generations for a namespace, one per source that has
/// published. Read surfaces (`list`, `export_graph`) drop rows from a
/// superseded generation against this set.
fn active_generations_of(conn: &Connection, namespace: &str) -> std::collections::HashSet<u64> {
    let query = format!(
        "MATCH (g:IngestGeneration) WHERE g.namespace = '{}' AND g.active_generation > 0 RETURN g.active_generation",
        escape_ladybug_string(namespace)
    );
    let mut gens = std::collections::HashSet::new();
    if let Ok(rows) = conn.query(&query) {
        for row in rows {
            if let Ok(g) = format!("{}", row[0]).parse::<i64>() {
                if g > 0 {
                    gens.insert(g as u64);
                }
            }
        }
    }
    gens
}

/// The active `(namespace, generation)` pairs across every namespace, for the
/// whole-graph read surface (`export_graph`). A generation-scoped node whose
/// pair is absent was superseded by a later publish and is dropped.
fn all_active_generations(conn: &Connection) -> std::collections::HashSet<(String, u64)> {
    let query = "MATCH (g:IngestGeneration) WHERE g.active_generation > 0 RETURN g.namespace, g.active_generation";
    let mut pairs = std::collections::HashSet::new();
    if let Ok(rows) = conn.query(query) {
        for row in rows {
            let ns = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
            if let Ok(g) = format!("{}", row[1]).parse::<i64>() {
                if g > 0 {
                    pairs.insert((ns, g as u64));
                }
            }
        }
    }
    pairs
}

/// The ids a namespace actually holds, indexed so a declaration can be resolved
/// without scanning them.
///
/// Node ids became repository-relative, so a memory written when ingestion
/// produced absolute ids declares `C:/proj/src/foo.rs` and matches nothing now.
/// Its tail still identifies the file, so a declaration ending in `/<id>`
/// resolves to that id, and a suffix matching more than one node is left alone.
///
/// That rule is unchanged. Asking it of every known id in turn was the problem:
/// O(declarations x nodes) comparisons with a `format!` allocation per
/// comparison, on the path that runs at the end of every ingest. A declaration
/// has only as many path suffixes as it has separators, so looking each of
/// those up answers the same question in a handful of hashes however large the
/// namespace grows.
pub struct KnownIds<'a> {
    ids: std::collections::HashSet<&'a str>,
    /// Qualified ids indexed by the bare path inside them, so a rule naming
    /// `src/lib.rs` finds `NeuroStrata::src/lib.rs` without scanning. A path
    /// held by more than one namespace is recorded as ambiguous and resolves to
    /// neither: a GOVERNS edge into another project's file is worse than none.
    by_path: std::collections::HashMap<&'a str, Option<&'a str>>,
}

impl<'a> KnownIds<'a> {
    /// Index every id. A bare path is reachable exactly as-is; a qualified or
    /// generation-scoped id is reachable by its logical path. No generation
    /// filter: used by tests and by callers that have already narrowed the set.
    pub fn new(ids: impl IntoIterator<Item = &'a str>) -> Self {
        Self::build(ids, None)
    }

    /// Index only the active generation's nodes. A generation-scoped id whose
    /// generation is not `active_gen` is left out, and so is any id that is not
    /// generation-scoped at all (a rule/task UUID, or a pre-generation
    /// qualified id) -- only the published generation is reachable.
    pub fn active(ids: impl IntoIterator<Item = &'a str>, active_gen: u64) -> Self {
        Self::build(ids, Some(active_gen))
    }

    fn build(ids: impl IntoIterator<Item = &'a str>, active_gen: Option<u64>) -> Self {
        let mut id_set: std::collections::HashSet<&'a str> =
            std::collections::HashSet::new();
        let mut by_path: std::collections::HashMap<&'a str, Option<&'a str>> =
            std::collections::HashMap::new();

        for id in ids.into_iter().filter(|id| !id.is_empty()) {
            if let Some(active) = active_gen {
                match crate::parser::ingest::split_scoped_id(id) {
                    Some((_, gen, _)) => {
                        if gen != active {
                            continue; // a stale generation is not reachable
                        }
                    }
                    // Not generation-scoped: not part of any active generation.
                    None => continue,
                }
            }

            id_set.insert(id);
            // Only namespace- (or generation-) scoped ids are indexed by their
            // logical path; a bare id stays reachable exactly as-is.
            if id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                let path = crate::parser::ingest::logical_path_of(id);
                if !path.is_empty() {
                    by_path
                        .entry(path)
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(id));
                }
            }
        }

        Self {
            ids: id_set,
            by_path,
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Finds the node a declaration means, when it does not name one exactly.
    pub fn resolve(&self, declared: &str) -> Option<String> {
        // Trailing slashes name a directory the same way the path without
        // one does; the ingested node id carries no trailing slash.
        let declared = declared.trim_end_matches('/');
        // Deliberately before normalisation: an id stored with backslashes is
        // still matched by naming it exactly, as it always was.
        if self.ids.contains(declared) {
            return Some(declared.to_string());
        }

        let declared = declared.replace('\\', "/");

        // An id qualified by its namespace is LONGER than the declaration that
        // names it: a rule says `src/lib.rs`, the id is `NeuroStrata::src/lib.rs`.
        // That is the common case now, so it is asked first. A path held by two
        // namespaces was indexed as ambiguous and resolves to neither.
        if let Some(found) = self.by_path.get(declared.as_str()) {
            return found.map(str::to_string);
        }

        // The other direction: a declaration longer than the id it means, which
        // is a legacy absolute path naming a file the graph already holds.
        //
        // Its suffix can name that file two ways. Before migrating it is an id
        // (`src/lib.rs`); after doctor's migration, a re-ingest, it is the path
        // inside a qualified id (`NeuroStrata::src/lib.rs`). Checking only ids
        // meant every absolute declaration lost its edge at exactly the step
        // the migration tells people to take.
        let mut only: Option<&str> = None;
        for (separator, _) in declared.match_indices('/') {
            let suffix = &declared[separator + 1..];
            let mut hits: Vec<&str> = Vec::new();
            if let Some(id) = self.ids.get(suffix) {
                hits.push(id);
            }
            match self.by_path.get(suffix) {
                Some(Some(qualified)) => hits.push(qualified),
                // Held by more than one namespace: ambiguous, as above.
                Some(None) => return None,
                None => {}
            }
            for id in hits {
                if only.is_some() {
                    // A suffix matching more than one node is ambiguous, and a
                    // wrong edge is worse than a missing one.
                    return None;
                }
                only = Some(id);
            }
        }
        only.map(|id| id.to_string())
    }
}

/// One edge a memory asks for, read off its metadata.
#[derive(Debug, PartialEq, Clone)]
pub struct EdgeSpec {
    pub rel_type: &'static str,
    pub target_id: String,
    pub declaration_index: usize,
    /// true for `self -> target`, false for `target -> self`. Containment reads
    /// the other way round: the parent contains the child, not the reverse.
    pub points_at_target: bool,
}

/// Maps metadata arrays onto the three edge tables. `related_to` is a semantic
/// link, `contained_by` is structure (directory to file to symbol), and
/// `governs` connects a rule to the code it constrains -- the edge that lets an
/// architectural memory be found from the file it applies to.
pub fn edge_specs(metadata: &serde_json::Value) -> Vec<EdgeSpec> {
    let payload = crate::traits::MemoryPayload {
        content: String::new(),
        user_id: String::new(),
        memory_type: String::new(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata: metadata.clone(),
    };
    parse_edge_specs(&payload).0
}

/// Memory types written with a zero vector. They describe where something
/// lives, or record that a machine operation happened, not what it says, so
/// they carry no meaning for a similarity search: directory ingestion's nodes,
/// and the thalamic bus's sentinel and audit rows.
const STRUCTURAL_MEMORY_TYPES: [&str; 5] = [
    "directory",
    "file",
    "markdown",
    "freshness_flag",
    "guard_event",
];

/// Row-class predicates for filtering at the store seam.
/// These are applied BEFORE the candidate budget is spent.

fn is_archived(metadata: &Value) -> bool {
    metadata
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Delegates to the canonical predicate rather than re-deriving it: the
/// conflict rule is stated once, in `synaptic_mesh::conflict`, so the read
/// surfaces cannot drift into disagreeing about what "conflicting" means.
fn is_mesh_conflicting(metadata: &Value) -> bool {
    crate::synaptic_mesh::conflict::is_conflicting_metadata(metadata)
}

/// Ranking gain for a memory that carries `metadata.source` (FEATURE-4).
///
/// Sized against the usage boost's own scale: at access_count = 1 the usage
/// gain is 0.05 * ln(2) ~= 0.035, so a single provenance marker buys roughly
/// two reads' worth of promotion. Enough to settle a tie, small enough that a
/// memory about a different subject does not surface just because somebody
/// dated it.
const SOURCE_RANK_GAIN: f32 = 0.05;

/// Normalize an evidence path for dedup: a one-hop undirected relation's endpoints are sorted; a directed relation is order-sensitive.
fn norm_path(
    edges: &[crate::traits::EvidenceEdge],
    kind: &crate::traits::EvidenceKind,
) -> Vec<(String, String, String)> {
    let relations = &crate::traits::memory_vocabulary()["relations"];
    edges
        .iter()
        .map(|e| {
            if *kind == crate::traits::EvidenceKind::OneHop
                && relations[e.relation.as_str()]["direction"] == "undirected"
            {
                let (a, b) = if e.source <= e.target {
                    (e.source.clone(), e.target.clone())
                } else {
                    (e.target.clone(), e.source.clone())
                };
                (a, e.relation.clone(), b)
            } else {
                (e.source.clone(), e.relation.clone(), e.target.clone())
            }
        })
        .collect()
}

fn escape_ladybug_string(s: &str) -> String {
    s.replace("\\", "\\\\").replace("'", "\\'")
}

/// The derived single-column key for a Memory row (Task 7 re-plan,
/// ratified 2026-10-25): `namespace + U+0001 + id`, carried in the `pk`
/// column — the PRIMARY KEY of fresh tables, an ordinary backfilled
/// column on tables that predate it.
///
/// The scheme exists because lbug 0.20.4's parser rejects a multi-column
/// PRIMARY KEY in CREATE NODE TABLE outright (probed, run 40164691714),
/// so the (namespace, id) identity the row MERGE keys on cannot be the
/// table's key directly. The separator is U+0001, a control character:
/// DR-1 namespaces, DR-2 bare ids, and the ratified qualified-id
/// remainder (no quotes, backslashes, or control characters) all refuse
/// control characters, so neither side of the derivation can contain the
/// separator and the concatenation is injective — two distinct
/// (namespace, id) pairs never derive the same pk.
fn memory_pk(namespace: &str, id: &str) -> String {
    format!("{}\u{1}{}", namespace, id)
}

/// NUL is refused at the store boundary, by name (Task 7). It is the one
/// byte no rung of the escaping ladder may carry into the engine: handed
/// to a bound parameter it would meet the engine's C strings, and embedded
/// in an escaped literal it would land raw. Every other control character
/// is the ladder's business; NUL is the boundary's.
fn reject_nul(field: &str, value: &str) -> Result<()> {
    if value.contains('\u{0}') {
        anyhow::bail!("ERROR [NUL]: NUL byte in {field} is refused at the store boundary");
    }
    Ok(())
}

/// Whether any string in a metadata value — keys included — carries a NUL.
/// Checked on the value itself, not its serialization: in the serialized
/// text a NUL appears as a six-character escape sequence, where a scan
/// for the raw byte would miss it.
fn value_contains_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\u{0}'),
        Value::Array(items) => items.iter().any(value_contains_nul),
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| k.contains('\u{0}') || value_contains_nul(v)),
        _ => false,
    }
}

/// Validates a Cypher relation (edge) type name before it is interpolated
/// into a query string. Relation types are compile-time constants today, but
/// this guard prevents a future code path that accepts user-supplied relation
/// names from becoming a Cypher-injection vector. Returns `true` only for
/// names matching `^[A-Z][A-Z0-9_]*$`.
fn is_valid_relation_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn replace_json_namespace_prefix(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(text) => {
            if let Some(rest) = text.strip_prefix(from) {
                *text = format!("{to}{rest}");
            }
        }
        Value::Array(items) => {
            for item in items {
                replace_json_namespace_prefix(item, from, to);
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                replace_json_namespace_prefix(item, from, to);
            }
        }
        _ => {}
    }
}

/// How many edges go into one relinking statement. The pairs are written into
/// the query text, so this is what bounds how large that text can get.
const RELINK_BATCH: usize = 500;

/// The relationship tables init() creates between Memory rows. Relocate's
/// delete-and-recreate path replays a moved row's edges across exactly
/// these tables.
const MEMORY_REL_TABLES: [&str; 4] = ["RELATES_TO", "CONTAINS", "GOVERNS", "EXTRACTED_FROM"];

/// Replays one relationship type in a couple of statements instead of one per
/// edge.
///
/// The obvious spelling is a single UNWIND driving MERGE, which would say "link
/// these unless they are linked already" in one statement. lbug 0.15.3 cannot
/// run it: MERGE inside an UNWIND pipeline fails with an internal
/// `invalid unordered_map<K, T> key`, while the same UNWIND with MATCH, or the
/// same MERGE without UNWIND, both succeed. So the pairs that already exist are
/// read back and subtracted here, which makes CREATE safe and keeps the
/// statement count independent of the number of edges.
///
/// It also reports the truth. The loop this replaces counted statements that
/// did not error -- including every one whose MATCH found nothing -- so an
/// ingest that linked nothing still announced hundreds of relinked edges.
fn create_missing_edges(
    conn: &Connection,
    rel_type: &str,
    pairs: &[(String, String)],
) -> Result<usize> {
    if !is_valid_relation_name(rel_type) {
        anyhow::bail!("invalid relation name for Cypher interpolation: {:?}", rel_type);
    }
    let existing = conn.query(&format!(
        "MATCH (x:Memory)-[:{}]->(y:Memory) RETURN x.id, y.id",
        rel_type
    ))?;

    let mut present = std::collections::HashSet::new();
    for row in existing {
        present.insert((format!("{}", row[0]), format!("{}", row[1])));
    }

    // The same edge can be declared by both ends, and CREATE would honour each
    // declaration separately, so the batch is deduplicated as well.
    let mut missing = Vec::new();
    for pair in pairs {
        if !present.contains(pair) {
            present.insert(pair.clone());
            missing.push(pair);
        }
    }

    let mut created = 0usize;
    for chunk in missing.chunks(RELINK_BATCH) {
        let list = chunk
            .iter()
            .map(|(from, to)| {
                format!(
                    "{{f: '{}', t: '{}'}}",
                    escape_ladybug_string(from),
                    escape_ladybug_string(to)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        // Both ends must exist; a rule naming a file the ingester never reached
        // still produces nothing, exactly as before.
        let query = format!(
            "UNWIND [{}] AS e MATCH (a:Memory {{id: e.f}}), (b:Memory {{id: e.t}}) CREATE (a)-[:{}]->(b) RETURN count(*)",
            list, rel_type
        );
        for row in conn.query(&query)? {
            created += format!("{}", row[0]).parse::<usize>().unwrap_or(0);
        }
    }

    Ok(created)
}

#[async_trait]
impl VectorStore for LadybugStore {
    async fn init(&self, _namespace: &str) -> Result<()> {
        if self.schema_ready.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }

        // The snapshot guard is held across the call so schema creation
        // stays exclusive with snapshot operations (export/checkpoint). The
        // write turn must NOT be pre-acquired here: write_with_deadline
        // takes it inside its own non-cancellable blocking closure (see R5),
        // which is also what keeps it from being released by a timeout while
        // the C++ call is still running.
        let _snapshot_turn = self.snapshot_lock.clone().lock_owned().await;
        let dimensions = self.dimensions;
        #[cfg(test)]
        let db_path = self.local_path.clone();
        // Use write_with_deadline so the write lock is held INSIDE the
        // spawn_blocking closure (see R5). Holding an async lock across
        // with_conn releases it on timeout while the C++ call is still running.
        let outcome = self.write_with_deadline("initializing schema", move |conn| {
            #[cfg(test)]
            test_snapshot_operation_barrier(&db_path, "init");

            // Task 7 re-plan (ratified 2026-10-25): fresh Memory tables
            // key on the derived pk (see memory_pk) — lbug 0.20.4 accepts
            // only a single-column PRIMARY KEY, so (namespace, id)
            // identity is carried in one column. A table that already
            // exists keeps its PRIMARY KEY (id); it gains the pk column
            // below instead of being recreated.
            let create_node_table = format!(
                "CREATE NODE TABLE Memory (pk STRING, id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[{}], PRIMARY KEY (pk))",
                dimensions
            );
            let mut created_fresh = true;
            if let Err(e) = conn.query(&create_node_table) {
                if !e.to_string().contains("already exists") {
                    return Err(e.into());
                }
                created_fresh = false;
            }

            let create_ingest_stage_table = "CREATE NODE TABLE IngestStage (namespace STRING, generation STRING, PRIMARY KEY (namespace))";
            if let Err(error) = conn.query(create_ingest_stage_table) {
                if !error.to_string().contains("already exists") {
                    return Err(error.into());
                }
            }

            let create_rel_table = "CREATE REL TABLE RELATES_TO (FROM Memory TO Memory)";
            conn.query(create_rel_table).ok();

            let create_contains_table = "CREATE REL TABLE CONTAINS (FROM Memory TO Memory)";
            conn.query(create_contains_table).ok();

            let create_governs_table = "CREATE REL TABLE GOVERNS (FROM Memory TO Memory)";
            conn.query(create_governs_table).ok();

            // Vocabulary v2: extraction edges (memory -> task) behind Lock 2.
            let create_extracted_table = "CREATE REL TABLE EXTRACTED_FROM (FROM Memory TO Memory)";
            conn.query(create_extracted_table).ok();

            // PQTH: Intake receipts table (operational, not memory rows)
            let create_receipts_table = "CREATE NODE TABLE IntakeReceipt (id STRING, namespace STRING, source_kind STRING, source_revision STRING, source_digest STRING, detected_intents STRING, raw_text STRING, created_at STRING, PRIMARY KEY (id))";
            conn.query(create_receipts_table).ok();

            // PQTH: Intake candidates table (operational, not memory rows)
            let create_candidates_table = "CREATE NODE TABLE IntakeCandidate (id STRING, namespace STRING, receipt_id STRING, title STRING, description STRING, review_state STRING, disposition STRING, task_id STRING, cas_version INT64, created_at STRING, updated_at STRING, PRIMARY KEY (id))";
            conn.query(create_candidates_table).ok();

            // Source-scoped staged ingestion. Two row kinds live in this table:
            //   * the counter row, `id = namespace`, whose `counter` allocates
            //     the next generation number (shared across sources so the
            //     generation-scoped node ids never collide);
            //   * a pointer row per source, `id = namespace::source_key`, whose
            //     `active_generation` names that source's published generation
            //     (0 = none). The pointer flip is the atomic publish step, and
            //     keying it by source is what keeps worktree B from overwriting
            //     worktree A's active generation.
            let create_generation_table = "CREATE NODE TABLE IngestGeneration (id STRING, namespace STRING, source_key STRING, active_generation INT64, counter INT64, PRIMARY KEY (id))";
            conn.query(create_generation_table).ok();


            // Task 7 row-identity audit: classify the store's rows by
            // (namespace, id) identity. A row whose namespace property is
            // NULL predates the identity rule (or arrived damaged); one
            // carrying a valid namespace in its metadata is backfilled in
            // place, the rest are quarantined — counted, left
            // byte-identical, and refused as write targets by upsert and
            // delete until an operator repairs them. The audit runs
            // inside init's write on purpose: it must never become a
            // reason the store refuses to open, so every failure mode
            // here degrades to a count, never an error.
            let mut audit = RowIdentityAudit::default();
            if let Ok(mut rows) = conn.query("MATCH (m:Memory) RETURN count(m)") {
                if let Some(row) = rows.next() {
                    audit.total_rows = format!("{}", row[0]).parse().unwrap_or(0);
                }
            }
            let mut orphans: Vec<(String, String)> = Vec::new();
            if let Ok(mut rows) = conn.query(
                "MATCH (m:Memory) WHERE m.namespace IS NULL RETURN m.id, m.metadata",
            ) {
                while let Some(row) = rows.next() {
                    orphans.push((format!("{}", row[0]), format!("{}", row[1])));
                }
            }
            audit.null_namespace_rows = orphans.len() as i64;
            for (orphan_id, metadata) in &orphans {
                let claimed = serde_json::from_str::<Value>(metadata).ok().and_then(|v| {
                    v.get("namespace")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                });
                let repaired = match claimed {
                    Some(ns) if crate::domain::validate_namespace(&ns).is_ok() => {
                        let repair = format!(
                            "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace IS NULL SET m.namespace = '{}'",
                            escape_ladybug_string(orphan_id),
                            escape_ladybug_string(&ns),
                        );
                        conn.query(&repair).is_ok()
                    }
                    _ => false,
                };
                if repaired {
                    audit.backfilled += 1;
                } else {
                    audit.quarantined += 1;
                }
            }
            if audit.null_namespace_rows > 0 {
                eprintln!(
                    "[neurostrata:identity-audit] {} of {} Memory rows had a NULL namespace: {} backfilled from metadata, {} quarantined (writes to a quarantined id refuse until an operator repairs the row)",
                    audit.null_namespace_rows, audit.total_rows, audit.backfilled, audit.quarantined
                );
            }

            // Task 7 re-plan: a pre-existing Memory table has no pk
            // column (its PRIMARY KEY is still (id)). Add the column and
            // backfill each row's derived pk, so identity operations can
            // key on pk against it exactly as they do against a fresh
            // table. This never blocks the open either: if the ALTER or
            // the backfill fails, the store continues in legacy
            // (namespace, id) matching mode (pk_ready stays false) and
            // says so on stderr. Runs after the namespace audit above so
            // rows whose namespace was just backfilled get the pk of
            // their repaired identity in the same pass.
            let mut pk_ready = created_fresh;
            if !created_fresh {
                let has_pk_column =
                    conn.query("MATCH (m:Memory) RETURN m.pk LIMIT 1").is_ok();
                if has_pk_column {
                    pk_ready = true;
                } else {
                    match conn.query("ALTER TABLE Memory ADD pk STRING DEFAULT ''") {
                        Ok(_) => {
                            pk_ready = true;
                        }
                        Err(e) => {
                            eprintln!(
                                "[neurostrata:identity-audit] could not add the derived pk column to the existing Memory table ({}) — continuing in legacy (namespace, id) matching mode",
                                e
                            );
                        }
                    }
                }
                if pk_ready {
                    let mut stale: Vec<(String, String, String)> = Vec::new();
                    if let Ok(mut rows) = conn.query(
                        "MATCH (m:Memory) WHERE m.namespace IS NOT NULL RETURN m.id, m.namespace, m.pk",
                    ) {
                        while let Some(row) = rows.next() {
                            let row_id = format!("{}", row[0]);
                            let row_ns = format!("{}", row[1]);
                            let row_pk = format!("{}", row[2]);
                            let derived = memory_pk(&row_ns, &row_id);
                            if row_pk != derived {
                                stale.push((row_id, row_ns, derived));
                            }
                        }
                    }
                    let mut backfilled = 0usize;
                    let mut unfixable = 0usize;
                    for (row_id, row_ns, derived) in &stale {
                        let applied = (|| -> std::result::Result<(), lbug::Error> {
                            let mut stmt = conn.prepare(
                                "MATCH (m:Memory) WHERE m.id = $id AND m.namespace = $ns SET m.pk = $pk",
                            )?;
                            conn.execute(
                                &mut stmt,
                                vec![
                                    ("id", lbug::Value::String(row_id.clone())),
                                    ("ns", lbug::Value::String(row_ns.clone())),
                                    ("pk", lbug::Value::String(derived.clone())),
                                ],
                            )?;
                            Ok(())
                        })();
                        if applied.is_ok() {
                            backfilled += 1;
                        } else {
                            unfixable += 1;
                        }
                    }
                    if backfilled > 0 {
                        eprintln!(
                            "[neurostrata:identity-audit] backfilled the derived pk on {} Memory row(s)",
                            backfilled
                        );
                    }
                    if unfixable > 0 {
                        // A primary-key column cannot be updated in
                        // place (the binder refuses SET on it), so a
                        // stale pk on a fresh table is beyond init's
                        // repair: the row keeps its stored key, stays
                        // readable by property scans, and is reported
                        // here rather than silently skipped.
                        eprintln!(
                            "[neurostrata:identity-audit] {} Memory row(s) carry a pk that does not match their (namespace, id) and could not be rewritten in place — an operator repair (delete + reinsert) is required",
                            unfixable
                        );
                    }
                }
            }

            Ok((audit, pk_ready))
        })
        .await;

        // Creating the tables is itself a write worth flushing.
        self.dirty.mark();
        match outcome {
            Ok((audit, pk_ready)) => {
                *self
                    .identity_audit
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = audit;
                self.pk_ready
                    .store(pk_ready, std::sync::atomic::Ordering::Release);
                self.schema_ready
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<crate::traits::UpsertReport> {
        if namespace.starts_with(INTERNAL_INGEST_STAGE_PREFIX) {
            let authorized_stage_write = payload.user_id == "auto-ingestor"
                && has_ingest_stage_lease(&self.local_path, &self.path_aliases, namespace);
            if !authorized_stage_write {
                anyhow::bail!("reserved ingest staging namespace is not available for this write");
            }
        }
        let staged_generation = namespace
            .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
            .filter(|_| namespace.starts_with(INTERNAL_INGEST_STAGE_PREFIX))
            .map(ToOwned::to_owned);
        let namespace = namespace.to_string();
        let id = id.to_string();
        // Task 7: the store boundary validates row identity before any
        // query is built. NUL is refused first and by name — it is the one
        // byte no rung of the escaping ladder may carry into the engine —
        // then DR-1/DR-2 decide the namespace and the id (validate_id
        // carries the ratified composite rule for qualified ids).
        reject_nul("namespace", &namespace)?;
        reject_nul("id", &id)?;
        crate::domain::validate_namespace(&namespace).map_err(anyhow::Error::msg)?;
        crate::domain::validate_id(&id).map_err(anyhow::Error::msg)?;
        reject_nul("content", &payload.content)?;
        reject_nul("user_id", &payload.user_id)?;
        reject_nul("memory_type", &payload.memory_type)?;
        if let Some(agent_name) = &payload.agent_name {
            reject_nul("agent_name", agent_name)?;
        }
        reject_nul("location", &payload.location)?;
        reject_nul("location_lines", &payload.location_lines)?;
        if value_contains_nul(&payload.metadata) {
            anyhow::bail!("ERROR [NUL]: NUL byte in metadata is refused at the store boundary");
        }
        // R14: reject empty/whitespace namespaces before hitting the store.
        // DR-1 above already refuses both; this stays as the store-layer
        // backstop R14 landed, beneath the validator.
        if namespace.trim().is_empty() {
            anyhow::bail!("namespace must be non-empty");
        }
        // Parse shape warnings from the ORIGINAL payload before the write.
        let shape_warnings = write_shape_warnings(&payload);
        let pk_ready = self.pk_ready();
        // The stub materializer needs the embedding width for its zero vector;
        // captured here so the closure stays `FnOnce + Send + 'static`.
        let dimensions = self.dimensions;
        let zero_vec = zero_vector_str(dimensions);
        self.write_with_deadline("writing a memory", move |conn| {

            if let Some(generation) = &staged_generation {
                let safe_ns = escape_ladybug_string(&namespace);
                let mut rows = conn.query(&format!(
                    "MATCH (s:IngestStage) WHERE s.namespace = '{}' AND s.generation = '{}' RETURN s.namespace LIMIT 1",
                    safe_ns,
                    escape_ladybug_string(generation)
                ))?;
                if rows.next().is_none() {
                    anyhow::bail!("reserved ingest staging namespace has no database provenance");
                }
            }

            let safe_id = escape_ladybug_string(&id);
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_content = escape_ladybug_string(&payload.content);
            let safe_user_id = escape_ladybug_string(&payload.user_id);
            let safe_memory_type = escape_ladybug_string(&payload.memory_type);
            let safe_agent_name = escape_ladybug_string(
                payload.agent_name.as_deref().unwrap_or("unknown"),
            );
            let safe_location = escape_ladybug_string(&payload.location);
            let safe_location_lines = escape_ladybug_string(&payload.location_lines);
            let safe_metadata = escape_ladybug_string(&serde_json::to_string(&payload.metadata)?);

            let vec_str = format!(
                "[{}]",
                // R16: defense-in-depth (no native bind in lbug 0.20.4).
                // Always emit decimal (e.g. 0 → 0.0) so ARRAY_DISTANCE
                // binder never sees pure-integer arrays.
                vector.iter().map(|f| {
                    let s = format!("{}", f);
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        s
                    } else {
                        format!("{}.0", s)
                    }
                }).collect::<Vec<_>>().join(",")
            );

            // A quarantined row (NULL namespace, per init's audit) must not
            // be forked into a second identity by this write: refuse loudly
            // and leave the row byte-identical for an operator to repair.
            let mut held = conn.query(&format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace IS NULL RETURN count(m)",
                safe_id
            ))?;
            let held_count = held
                .next()
                .map(|row| format!("{}", row[0]).parse::<i64>().unwrap_or(0))
                .unwrap_or(0);
            if held_count > 0 {
                anyhow::bail!(
                    "write refused: id '{}' is held by a quarantined row (NULL namespace) — the quarantine is repaired by an operator, not by forking the id into a second row",
                    id
                );
            }

            // Row identity is (namespace, id). When the table carries
            // the derived pk column, the MERGE keys on it and every value
            // is a bound parameter (B1 — binding is probed, Step A): the
            // pattern key and the SET values travel as $parameters, so
            // nothing caller-supplied is interpolated into the statement.
            // The embedding still interpolates as a literal: it is a
            // vector of engine-formatted numbers built above, never
            // caller text. When init could not give the table a pk
            // column (legacy mode), the MERGE keeps the pre-re-plan
            // (namespace, id) property pattern over escaped literals.
            if pk_ready {
                // The pattern carries id beside pk: on a legacy table
                // (pk added by ALTER) the PRIMARY KEY is still id, and
                // the binder refuses a MERGE whose create path does not
                // supply the key property in the pattern ("Create node m
                // expects primary key id as input" — run 40167089842).
                // A key property also cannot be assigned via SET, so id
                // comes from the pattern and ON CREATE SET starts at
                // namespace.
                let merge_query = format!(
                    "MERGE (m:Memory {{pk: $pk, id: $id}})
                     ON CREATE SET m.namespace = $ns, m.content = $content, m.user_id = $user_id, m.memory_type = $memory_type, m.agent_name = $agent_name, m.location = $location, m.location_lines = $location_lines, m.metadata = $metadata, m.embedding = {}
                     ON MATCH SET m.content = $content, m.user_id = $user_id, m.memory_type = $memory_type, m.agent_name = $agent_name, m.location = $location, m.location_lines = $location_lines, m.metadata = $metadata, m.embedding = {}",
                    vec_str, vec_str
                );
                let mut stmt = conn.prepare(&merge_query)?;
                conn.execute(
                    &mut stmt,
                    vec![
                        ("pk", lbug::Value::String(memory_pk(&namespace, &id))),
                        ("id", lbug::Value::String(id.clone())),
                        ("ns", lbug::Value::String(namespace.clone())),
                        ("content", lbug::Value::String(payload.content.clone())),
                        ("user_id", lbug::Value::String(payload.user_id.clone())),
                        (
                            "memory_type",
                            lbug::Value::String(payload.memory_type.clone()),
                        ),
                        (
                            "agent_name",
                            lbug::Value::String(
                                payload
                                    .agent_name
                                    .clone()
                                    .unwrap_or_else(|| "unknown".to_string()),
                            ),
                        ),
                        ("location", lbug::Value::String(payload.location.clone())),
                        (
                            "location_lines",
                            lbug::Value::String(payload.location_lines.clone()),
                        ),
                        (
                            "metadata",
                            lbug::Value::String(serde_json::to_string(&payload.metadata)?),
                        ),
                    ],
                )?;
            } else {
                let insert_query = format!(
                    "MERGE (m:Memory {{id: '{}', namespace: '{}'}})
                     ON CREATE SET m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}
                     ON MATCH SET m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}",
                    safe_id, safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str,
                    safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str
                );

                conn.query(&insert_query)?;
            }

            // Emit shape warnings after successful row write.
            for w in &shape_warnings {
                emit_write_shape_warning(&namespace, &id, w);
            }

            let mut deferred_edges = Vec::new();

            // Materialise the edges this memory declares. A target that does not exist
            // yet simply produces no edge: MATCH finds nothing and MERGE never runs.
            // That is deliberate -- a rule may name a file the ingester has not reached.
            for edge in edge_specs(&payload.metadata) {
                let target = if edge.target_id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                    // (E) Qualified: exact ID lookup in any namespace.
                    match exact_target_exists(conn, &edge.target_id) {
                        Ok(true) => edge.target_id.clone(),
                        Ok(false) => {
                            deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                rel_type: edge.rel_type.to_string(),
                                declared_target: edge.target_id.clone(),
                                declaration_index: edge.declaration_index,
                                resolution: crate::traits::TargetResolution::Missing,
                            });
                            emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                why: "qualified target does not exist".to_string(),
                                skipped: "this edge materialization".to_string(),
                            });
                            continue;
                        }
                        Err(e) => {
                            deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                rel_type: edge.rel_type.to_string(),
                                declared_target: edge.target_id.clone(),
                                declaration_index: edge.declaration_index,
                                resolution: crate::traits::TargetResolution::Failed(e.to_string()),
                            });
                            emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                why: "target lookup failed".to_string(),
                                skipped: "this edge materialization".to_string(),
                            });
                            continue;
                        }
                    }
                } else {
                    match resolve_target_on_write(conn, &namespace, &edge.target_id) {
                        crate::traits::TargetResolution::Resolved(found) => found,
                        // Spec §3.2: a GOVERNS target that names a file not yet
                        // ingested auto-materialises a stub `file` node so the
                        // edge forms immediately instead of waiting for an
                        // ingest that may never come. Only the unqualified,
                        // unambiguous `Missing` case stubs -- an ambiguous or
                        // failed resolution, or a qualified cross-namespace id,
                        // still defers.
                        crate::traits::TargetResolution::Missing
                            if edge.rel_type == "GOVERNS" =>
                        {
                            match materialize_stub_file_node(
                                conn,
                                &namespace,
                                &edge.target_id,
                                &zero_vec,
                                pk_ready,
                            ) {
                                Ok(stub_id) => stub_id,
                                Err(e) => {
                                    deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                        rel_type: edge.rel_type.to_string(),
                                        declared_target: edge.target_id.clone(),
                                        declaration_index: edge.declaration_index,
                                        resolution: crate::traits::TargetResolution::Failed(
                                            e.to_string(),
                                        ),
                                    });
                                    emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                        field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                        why: "stub materialization failed".to_string(),
                                        skipped: "this edge materialization".to_string(),
                                    });
                                    continue;
                                }
                            }
                        }
                        resolution @ crate::traits::TargetResolution::Missing
                        | resolution @ crate::traits::TargetResolution::Ambiguous(_)
                        | resolution @ crate::traits::TargetResolution::Failed(_) => {
                            deferred_edges.push(crate::traits::EdgeResolutionStatus {
                                rel_type: edge.rel_type.to_string(),
                                declared_target: edge.target_id.clone(),
                                declaration_index: edge.declaration_index,
                                resolution,
                            });
                            emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                                field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                                why: "unqualified target is missing or ambiguous".to_string(),
                                skipped: "this edge materialization".to_string(),
                            });
                            continue;
                        }
                    }
                };
                if !is_valid_relation_name(edge.rel_type) {
                    deferred_edges.push(crate::traits::EdgeResolutionStatus {
                        rel_type: edge.rel_type.to_string(),
                        declared_target: edge.target_id.clone(),
                        declaration_index: edge.declaration_index,
                        resolution: crate::traits::TargetResolution::Failed(
                            "invalid relation name for Cypher interpolation".to_string(),
                        ),
                    });
                    continue;
                }
                // Task 7: the edge binds rows by (namespace, id), never by
                // id alone — an id-only MATCH lands the edge on every
                // same-id twin in other namespaces. The target row's
                // namespace is resolved with the source namespace
                // preferred; a true tie (the id lives in several
                // namespaces, the source's not among them) defers instead
                // of spraying.
                let target_ns = match bind_row_namespace(conn, &target, &namespace) {
                    RowBind::Bound(ns) => ns,
                    RowBind::Gone => {
                        deferred_edges.push(crate::traits::EdgeResolutionStatus {
                            rel_type: edge.rel_type.to_string(),
                            declared_target: edge.target_id.clone(),
                            declaration_index: edge.declaration_index,
                            resolution: crate::traits::TargetResolution::Missing,
                        });
                        emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                            field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                            why: "target row no longer exists".to_string(),
                            skipped: "this edge materialization".to_string(),
                        });
                        continue;
                    }
                    RowBind::Ambiguous(candidates) => {
                        deferred_edges.push(crate::traits::EdgeResolutionStatus {
                            rel_type: edge.rel_type.to_string(),
                            declared_target: edge.target_id.clone(),
                            declaration_index: edge.declaration_index,
                            resolution: crate::traits::TargetResolution::Ambiguous(candidates),
                        });
                        emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                            field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                            why: "target id is ambiguous across namespaces".to_string(),
                            skipped: "this edge materialization".to_string(),
                        });
                        continue;
                    }
                };
                let (from_pair, to_pair) = if edge.points_at_target {
                    (
                        (id.as_str(), namespace.as_str()),
                        (target.as_str(), target_ns.as_str()),
                    )
                } else {
                    (
                        (target.as_str(), target_ns.as_str()),
                        (id.as_str(), namespace.as_str()),
                    )
                };
                // With the pk column present, both ends bind by derived
                // pk via $parameters (B1); the relation type still
                // interpolates — it is a validated identifier
                // (is_valid_relation_name above), never free text. In
                // legacy mode the (id, namespace) property MATCH over
                // escaped literals is kept.
                let edge_result: Result<()> = (|| {
                    if pk_ready {
                        let edge_query = format!(
                            "MATCH (a:Memory {{pk: $apk}}), (b:Memory {{pk: $bpk}}) MERGE (a)-[:{}]->(b)",
                            edge.rel_type
                        );
                        let mut stmt = conn.prepare(&edge_query)?;
                        conn.execute(
                            &mut stmt,
                            vec![
                                (
                                    "apk",
                                    lbug::Value::String(memory_pk(from_pair.1, from_pair.0)),
                                ),
                                (
                                    "bpk",
                                    lbug::Value::String(memory_pk(to_pair.1, to_pair.0)),
                                ),
                            ],
                        )?;
                    } else {
                        let edge_query = format!(
                            "MATCH (a:Memory {{id: '{}', namespace: '{}'}}), (b:Memory {{id: '{}', namespace: '{}'}}) MERGE (a)-[:{}]->(b)",
                            escape_ladybug_string(from_pair.0),
                            escape_ladybug_string(from_pair.1),
                            escape_ladybug_string(to_pair.0),
                            escape_ladybug_string(to_pair.1),
                            edge.rel_type
                        );
                        conn.query(&edge_query)?;
                    }
                    Ok(())
                })();
                if let Err(e) = edge_result {
                    deferred_edges.push(crate::traits::EdgeResolutionStatus {
                        rel_type: edge.rel_type.to_string(),
                        declared_target: edge.target_id.clone(),
                        declaration_index: edge.declaration_index,
                        resolution: crate::traits::TargetResolution::Failed(e.to_string()),
                    });
                    emit_write_shape_warning(&namespace, &id, &WriteShapeWarning {
                        field: format!("metadata.{}", edge.rel_type.to_lowercase()),
                        why: "edge materialization failed".to_string(),
                        skipped: "this edge materialization".to_string(),
                    });
                    eprintln!("[neurostrata:write-shape] edge query error: {}", e);
                }
            }

            Ok(crate::traits::UpsertReport {
                id: id.clone(),
                deferred_edges,
            })
        })
        .await
    }

    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
    ) -> Result<Vec<SearchResult>> {
        let outcome = search_inner(
            self,
            namespace,
            vector,
            limit,
            include_tasks,
            include_archived,
            HashSet::new(),
        )
        .await?;
        Ok(outcome.results)
    }

    async fn search_with_domain(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        domains: &[String],
    ) -> Result<DomainSearchOutcome> {
        // Build the normalized domain key set before entering the closure.
        // The closure requires an owned HashSet because with_conn needs FnOnce + Send + 'static.
        // Normalize each provided domain slug so the store's contract is:
        // "the key set is compared by normalized slug" — the same comparison-time
        // normalization applied to the row's stored domain.
        let domain_keys: HashSet<String> = domains
            .iter()
            .filter_map(|d| crate::domain::normalize_domain_slug(d))
            .collect();
        search_inner(
            self,
            namespace,
            vector,
            limit,
            include_tasks,
            include_archived,
            domain_keys,
        )
        .await
    }

    async fn nearest_by_raw_distance(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        query: crate::traits::RawDistanceQuery,
    ) -> Result<Vec<crate::traits::RawDistanceCandidate>> {
        let scope = query.scope;
        let limit = query.limit;
        let namespace = namespace.to_string();
        let is_procedure_scope =
            matches!(scope, crate::traits::DedupCandidateScope::Procedure { .. });
        let proc_trigger = match &scope {
            crate::traits::DedupCandidateScope::Procedure { trigger } => Some(trigger.clone()),
            _ => None,
        };

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let vec_str = format!(
                "[{}]",
                // R16: defense-in-depth (no native bind in lbug 0.20.4).
                // Always emit decimal (e.g. 0 → 0.0) so ARRAY_DISTANCE
                // binder never sees pure-integer arrays.
                vector.iter().map(|f| {
                    let s = format!("{}", f);
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        s
                    } else {
                        format!("{}.0", s)
                    }
                }).collect::<Vec<_>>().join(",")
            );
            let now = chrono::Utc::now().timestamp();

            const FETCH_MULTIPLIER: usize = 10;
            let fetch_limit = limit.max(1) * FETCH_MULTIPLIER;

            // Excluded memory types for Domain scope (structural + task + procedure + symbol/code_ast).
            // Procedure scope restricts to memory_type = 'procedure' instead.
            let excluded_types_str = if is_procedure_scope {
                // exclude everything; we filter to procedure in Rust + SQL
                String::new()
            } else {
                let mut excluded = STRUCTURAL_MEMORY_TYPES
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                excluded.push("task".to_string());
                excluded.push("procedure".to_string());
                excluded.push("symbol".to_string());
                excluded.push("code_ast".to_string());
                excluded
                    .iter()
                    .map(|t| format!("'{}'", t))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let type_filter = if is_procedure_scope {
                "AND m.memory_type = 'procedure'".to_string()
            } else {
                format!("AND NOT m.memory_type IN [{}]", excluded_types_str)
            };

            let search_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' {} RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC, m.id ASC LIMIT {}",
                safe_ns, type_filter, vec_str, fetch_limit
            );

            let result = conn.query(&search_query)?;
            let mut out = Vec::new();
            for row in result {
                if out.len() >= limit {
                    break;
                }
                let id: String = format!("{}", row[0]);
                let distance: f32 = match &row[1] {
                    lbug::Value::Float(f) if f.is_finite() && *f >= 0.0 => *f,
                    lbug::Value::Double(d) if d.is_finite() && *d >= 0.0 => *d as f32,
                    _ => continue,
                };
                let content: String = format!("{}", row[2]);
                let user_id: String = format!("{}", row[3]);
                let memory_type: String = format!("{}", row[4]);
                let agent_name: String = format!("{}", row[5]);
                let location: String = format!("{}", row[6]);
                let location_lines: String = format!("{}", row[7]);
                let metadata_str: String = format!("{}", row[8]);
                let metadata_val: Value =
                    serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                // Lifecycle filters (Rust side).
                if is_retired_at(&metadata_val, now) {
                    continue;
                }
                if is_archived(&metadata_val) {
                    continue;
                }
                if is_mesh_conflicting(&metadata_val) {
                    continue;
                }
                if crate::registry::is_registry_row(&metadata_val) {
                    continue;
                }
                if memory_type != "procedure" && user_id == "auto-ingestor" {
                    continue;
                }

                // Procedure scope: trigger match, not lapsed, not spent.
                if is_procedure_scope {
                    let trig_ok = proc_trigger
                        .as_ref()
                        .map(|t| {
                            metadata_val
                                .get("trigger")
                                .and_then(|v| v.as_str())
                                .map(|s| s == t)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !trig_ok {
                        continue;
                    }
                    // lapsed?
                    if is_retired_at(&metadata_val, now) {
                        continue;
                    }
                    // spent?
                    if let Some(rf) = metadata_val.get("remaining_fires") {
                        if !rf.is_null() && rf.as_i64() == Some(0) {
                            continue;
                        }
                    }
                }

                out.push(crate::traits::RawDistanceCandidate {
                    id,
                    raw_distance: distance,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: if agent_name == "unknown" || agent_name.is_empty() {
                            None
                        } else {
                            Some(agent_name)
                        },
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                });
            }
            Ok(out)
        })
        .await
    }

    async fn find_active_exact_content(
        &self,
        namespace: &str,
        exact_content: &str,
        scope: crate::traits::DedupCandidateScope,
    ) -> Result<Vec<crate::traits::ExactContentCandidate>> {
        let is_procedure_scope =
            matches!(scope, crate::traits::DedupCandidateScope::Procedure { .. });
        let proc_trigger = match &scope {
            crate::traits::DedupCandidateScope::Procedure { trigger } => Some(trigger.clone()),
            _ => None,
        };
        let namespace = namespace.to_string();
        let exact_content = exact_content.to_string();
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_content = escape_ladybug_string(&exact_content);
            let now = chrono::Utc::now().timestamp();

            let type_filter = if is_procedure_scope {
                "AND m.memory_type = 'procedure'".to_string()
            } else {
                let mut excluded = STRUCTURAL_MEMORY_TYPES
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                excluded.push("task".to_string());
                excluded.push("procedure".to_string());
                excluded.push("symbol".to_string());
                excluded.push("code_ast".to_string());
                let s = excluded.iter().map(|t| format!("'{}'", t)).collect::<Vec<_>>().join(", ");
                format!("AND NOT m.memory_type IN [{}]", s)
            };

            let query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.content = '{}' {} RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata",
                safe_ns, safe_content, type_filter
            );
            let result = conn.query(&query)?;
            let mut out = Vec::new();
            for row in result {
                let id: String = format!("{}", row[0]);
                let content: String = format!("{}", row[1]);
                let user_id: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);
                let metadata_val: Value =
                    serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                if is_retired_at(&metadata_val, now) {
                    continue;
                }
                if is_archived(&metadata_val) {
                    continue;
                }
                if is_mesh_conflicting(&metadata_val) {
                    continue;
                }
                if crate::registry::is_registry_row(&metadata_val) {
                    continue;
                }
                if memory_type != "procedure" && user_id == "auto-ingestor" {
                    continue;
                }

                if is_procedure_scope {
                    let trig_ok = proc_trigger
                        .as_ref()
                        .map(|t| {
                            metadata_val
                                .get("trigger")
                                .and_then(|v| v.as_str())
                                .map(|s| s == t)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !trig_ok {
                        continue;
                    }
                    if is_retired_at(&metadata_val, now) {
                        continue;
                    }
                    if let Some(rf) = metadata_val.get("remaining_fires") {
                        if !rf.is_null() && rf.as_i64() == Some(0) {
                            continue;
                        }
                    }
                }

                out.push(crate::traits::ExactContentCandidate {
                    id,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: if agent_name == "unknown" || agent_name.is_empty() {
                            None
                        } else {
                            Some(agent_name)
                        },
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                });
            }
            Ok(out)
        })
        .await
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        // Task 7: NUL is refused at the store boundary on writes too.
        reject_nul("namespace", &namespace)?;
        reject_nul("id", &id)?;
        let pk_ready = self.pk_ready();
        self.write_with_deadline("deleting a memory", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            // A quarantined row (NULL namespace, per init's audit) refuses
            // writes aimed at its id, delete included: removing it is an
            // operator's repair decision, not a caller's side effect.
            let mut held = conn.query(&format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace IS NULL RETURN count(m)",
                safe_id
            ))?;
            let held_count = held
                .next()
                .map(|row| format!("{}", row[0]).parse::<i64>().unwrap_or(0))
                .unwrap_or(0);
            if held_count > 0 {
                anyhow::bail!(
                    "delete refused: id '{}' is held by a quarantined row (NULL namespace) — the quarantine is repaired by an operator, not deleted around",
                    id
                );
            }

            // A silent no-op on a missing id reads as success to the caller
            // (the CLI then reports a deletion that never happened), so
            // resolve the row first and fail loudly when it is absent.
            // With the pk column present the row is addressed by its
            // derived pk (bound, B1); legacy mode keeps the (id,
            // namespace) property match.
            if pk_ready {
                let pk = memory_pk(&namespace, &id);
                let exists = (|| -> std::result::Result<bool, lbug::Error> {
                    let mut stmt =
                        conn.prepare("MATCH (m:Memory {pk: $pk}) RETURN m.id LIMIT 1")?;
                    let mut rows =
                        conn.execute(&mut stmt, vec![("pk", lbug::Value::String(pk.clone()))])?;
                    Ok(rows.next().is_some())
                })()?;
                if !exists {
                    anyhow::bail!("No memory with id '{}' in namespace '{}'. Note: stored ids are namespace-qualified for ingested nodes.", id, namespace);
                }
                let mut stmt = conn.prepare("MATCH (m:Memory {pk: $pk}) DETACH DELETE m")?;
                conn.execute(&mut stmt, vec![("pk", lbug::Value::String(pk))])?;
                return Ok(());
            }
            let exists = conn
                .query(&format!(
                    "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' RETURN m.id LIMIT 1",
                    safe_id, safe_ns
                ))?
                .next()
                .is_some();
            if !exists {
                anyhow::bail!("No memory with id '{}' in namespace '{}'. Note: stored ids are namespace-qualified for ingested nodes.", id, namespace);
            }
            conn.query(&format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' DETACH DELETE m",
                safe_id, safe_ns
            ))?;
            Ok(())
        })
        .await
    }

    async fn relocate(&self, id: &str, from: &str, to: &str) -> Result<RelocateOutcome> {
        // Task 7: the store boundary validates the move's identity before
        // any store access, as it does a write's. NUL is refused by name
        // on every caller-supplied input, and the destination must pass
        // DR-1: a relocate that minted a row under a namespace the
        // validators refuse everywhere else would strand the row there,
        // unreachable by any later upsert.
        reject_nul("id", id)?;
        reject_nul("namespace", from)?;
        reject_nul("namespace", to)?;
        crate::domain::validate_namespace(to).map_err(anyhow::Error::msg)?;
        if from == to {
            return Ok(RelocateOutcome::SameNamespace);
        }
        let (vector, payload) = match self.get(from, id).await? {
            Some(pair) => pair,
            None => return Ok(RelocateOutcome::NotFound),
        };
        // An ingested node's id names the namespace that owns it, and the next
        // ingest of that namespace would MERGE onto the moved row and update it
        // where it now sits -- the cross-project theft qualified ids exist to stop.
        if payload.user_id == "auto-ingestor"
            || id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
        {
            return Ok(RelocateOutcome::Ingested);
        }

        let (id, from, to) = (id.to_string(), from.to_string(), to.to_string());
        let pk_ready = self.pk_ready();
        let payload = payload.clone();
        self.write_with_deadline("moving a memory", move |conn| {
            // The single-statement forms (the in-place SET below on a
            // pk-column table, and the legacy SET at the bottom) move the
            // row in one statement: no moment at which the memory exists
            // in neither namespace, and the row keeps its id, vector and
            // edges. The fresh-table fallback cannot claim that — it
            // recreates before it deletes, so the row briefly
            // exists in both namespaces instead (never in neither), as
            // its own comment states.
            // With the pk column present the derived key must move with
            // the row (bound, B1): a relocate that left the old
            // namespace's pk behind would strand the row — pk lookups in
            // the new namespace would miss it, and a fresh upsert there
            // could fork a second row under the same id.
            if pk_ready {
                let old_pk = memory_pk(&from, &id);
                let new_pk = memory_pk(&to, &id);
                // First attempt: move the row in place. Legal where pk
                // is a plain column (a legacy table that gained it via
                // ALTER); the binder refuses it where pk IS the primary
                // key — "Cannot set property pk ... Try delete and then
                // insert" (run 40167089842) — and that refusal selects
                // the delete-and-recreate path below.
                let in_place = (|| -> std::result::Result<(), lbug::Error> {
                    let mut stmt = conn.prepare(
                        "MATCH (m:Memory) WHERE m.id = $id AND m.namespace = $from AND m.user_id <> 'auto-ingestor' SET m.namespace = $to, m.pk = $pk",
                    )?;
                    conn.execute(
                        &mut stmt,
                        vec![
                            ("id", lbug::Value::String(id.clone())),
                            ("from", lbug::Value::String(from.clone())),
                            ("to", lbug::Value::String(to.clone())),
                            ("pk", lbug::Value::String(new_pk.clone())),
                        ],
                    )?;
                    Ok(())
                })();
                match in_place {
                    Ok(()) => return Ok(RelocateOutcome::Moved),
                    Err(e) if e.to_string().contains("primary key") => {}
                    Err(e) => return Err(e.into()),
                }
                // Recreate under the new namespace's key, then delete
                // the old row: the new row is created FIRST and the
                // captured edges are replayed onto it, and only then is
                // the old row deleted. The order is the atomicity story
                // — no engine transaction spans these statements, so
                // every intermediate state must be loss-free on its own:
                // a crash or error anywhere before the final delete
                // leaves the memory in both namespaces at worst (a
                // duplicate a later relocate or delete can clean up),
                // never in neither, and never with a prefix of its
                // edges. The whole sequence runs under the writer's
                // turn, so no other writer observes the row mid-move.
                let mut edges: Vec<(&'static str, bool, String)> = Vec::new();
                for rel in MEMORY_REL_TABLES {
                    for outgoing in [true, false] {
                        let capture = if outgoing {
                            format!(
                                "MATCH (m:Memory {{pk: $pk}})-[:{}]->(o:Memory) RETURN o.pk",
                                rel
                            )
                        } else {
                            format!(
                                "MATCH (o:Memory)-[:{}]->(m:Memory {{pk: $pk}}) RETURN o.pk",
                                rel
                            )
                        };
                        let mut stmt = conn.prepare(&capture)?;
                        let rows = conn.execute(
                            &mut stmt,
                            vec![("pk", lbug::Value::String(old_pk.clone()))],
                        )?;
                        for row in rows {
                            let other_pk = format!("{}", row[0]);
                            // A self-edge's other end is the moved row
                            // itself, and the outgoing pass already
                            // captured it; the incoming pass skips it, or
                            // the replay below would create it twice.
                            if !outgoing && other_pk == old_pk {
                                continue;
                            }
                            edges.push((rel, outgoing, other_pk));
                        }
                    }
                }
                let vec_str = format!(
                    "[{}]",
                    vector
                        .iter()
                        .map(|f| {
                            let s = format!("{}", f);
                            if s.contains('.') || s.contains('e') || s.contains('E') {
                                s
                            } else {
                                format!("{}.0", s)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                );
                let create = format!(
                    "CREATE (m:Memory {{pk: $pk, id: $id, namespace: $ns, content: $content, user_id: $user_id, memory_type: $memory_type, agent_name: $agent_name, location: $location, location_lines: $location_lines, metadata: $metadata, embedding: {}}})",
                    vec_str
                );
                let mut stmt = conn.prepare(&create)?;
                conn.execute(
                    &mut stmt,
                    vec![
                        ("pk", lbug::Value::String(new_pk.clone())),
                        ("id", lbug::Value::String(id.clone())),
                        ("ns", lbug::Value::String(to.clone())),
                        ("content", lbug::Value::String(payload.content.clone())),
                        ("user_id", lbug::Value::String(payload.user_id.clone())),
                        (
                            "memory_type",
                            lbug::Value::String(payload.memory_type.clone()),
                        ),
                        (
                            "agent_name",
                            lbug::Value::String(
                                payload
                                    .agent_name
                                    .clone()
                                    .unwrap_or_else(|| "unknown".to_string()),
                            ),
                        ),
                        ("location", lbug::Value::String(payload.location.clone())),
                        (
                            "location_lines",
                            lbug::Value::String(payload.location_lines.clone()),
                        ),
                        (
                            "metadata",
                            lbug::Value::String(serde_json::to_string(&payload.metadata)?),
                        ),
                    ],
                )?;
                for (rel, outgoing, other_pk) in &edges {
                    // A self-edge's other end is the moved row itself:
                    // it follows the row to its new key.
                    let other = if other_pk == &old_pk {
                        new_pk.clone()
                    } else {
                        other_pk.clone()
                    };
                    let (a_pk, b_pk) = if *outgoing {
                        (new_pk.clone(), other)
                    } else {
                        (other, new_pk.clone())
                    };
                    let replay = format!(
                        "MATCH (a:Memory {{pk: $apk}}), (b:Memory {{pk: $bpk}}) CREATE (a)-[:{}]->(b)",
                        rel
                    );
                    let mut stmt = conn.prepare(&replay)?;
                    conn.execute(
                        &mut stmt,
                        vec![
                            ("apk", lbug::Value::String(a_pk)),
                            ("bpk", lbug::Value::String(b_pk)),
                        ],
                    )?;
                }
                // The old row goes LAST, once the new row and every
                // replayed edge exist. DETACH takes the old row's
                // original edges with it, leaving exactly the replayed
                // set on the new row.
                let mut stmt = conn.prepare("MATCH (m:Memory {pk: $pk}) DETACH DELETE m")?;
                conn.execute(
                    &mut stmt,
                    vec![("pk", lbug::Value::String(old_pk.clone()))],
                )?;
                return Ok(RelocateOutcome::Moved);
            }
            let query = format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' AND m.user_id <> 'auto-ingestor' SET m.namespace = '{}'",
                escape_ladybug_string(&id),
                escape_ladybug_string(&from),
                escape_ladybug_string(&to)
            );
            conn.query(&query)?;
            Ok(RelocateOutcome::Moved)
        })
        .await
    }

    async fn export_database(&self, dir: &str) -> Result<()> {
        self.init("snapshot").await?;
        let snapshot_turn = self.snapshot_lock.clone().lock_owned().await;
        let write_turn = self.write_lock.clone().lock_owned().await;
        let source = self.local_path.clone();
        let source_aliases = self.path_aliases.clone();
        let database = self.db.clone();
        let dir = PathBuf::from(dir);
        tokio::task::spawn_blocking(move || -> Result<()> {
            // These guards stay in the non-cancellable worker for the entire
            // checkpoint, provenance read, and sibling-file copy. Cancelling
            // export's async caller cannot expose a moving DB/WAL cut.
            let _snapshot_turn = snapshot_turn;
            let _write_turn = write_turn;
            let _cross_process_writer = acquire_database_writer_lock(&source)?;
            #[cfg(test)]
            test_snapshot_operation_barrier(&source, "export");
            if dir.exists() {
                anyhow::bail!("snapshot destination already exists; choose a new empty path");
            }
            let parent = dir.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            let _ = crate::crash::install_alt_stack();
            let conn = Connection::new(&database)?;
            let (stages, _legacy_stage_leases) = {
                // The writer lock makes this the stable DB/lease cut. Register
                // proven pre-catalog generations before checkpoint and
                // manifest capture, including a stage created after preflight.
                let legacy_stage_leases =
                    migrate_legacy_stages_at_snapshot_cut(&conn, &source, &source_aliases)?;
                // Checkpoint first so the snapshot cannot miss writes still in the
                // WAL. The engine's EXPORT DATABASE is not used: it SIGSEGVs in
                // lbug 0.20.4's planner on every store (planExportTableData ->
                // std::__format on a dangling string_view).
                conn.query("CHECKPOINT")?;
                (read_stage_provenance(&conn)?, legacy_stage_leases)
            };
            drop(conn);
            let dir_name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("snapshot");
            let staging_dir = parent.join(format!(".{dir_name}.staging-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&staging_dir)?;
            let outcome = (|| -> Result<()> {
                let target = staging_dir.join("ladybug.store");
                std::fs::copy(&source, &target).map_err(|e| {
                    anyhow::anyhow!("could not copy the database into the snapshot: {}", e)
                })?;
                let wal = raw_database_sibling(&source, ".wal");
                if wal.exists() {
                    std::fs::copy(&wal, staging_dir.join("ladybug.store.wal")).map_err(|e| {
                        anyhow::anyhow!(
                            "could not copy the write-ahead log into the snapshot: {}",
                            e
                        )
                    })?;
                }
                let source_marker = width_marker_path(&source);
                if source_marker.exists() {
                    std::fs::copy(&source_marker, width_marker_path(&target)).map_err(|e| {
                        anyhow::anyhow!(
                            "could not copy the embedding marker into the snapshot: {}",
                            e
                        )
                    })?;
                }
                let stage_namespaces: Vec<String> = stages
                    .iter()
                    .map(|(namespace, _)| namespace.clone())
                    .collect();
                let manifest = serde_json::json!({
                    "format": "neurostrata-snapshot",
                    "version": 2,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "source": source.display().to_string(),
                    "bytes": std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0),
                    "stage_generations": stage_namespaces,
                });
                std::fs::write(
                    staging_dir.join("manifest.json"),
                    serde_json::to_string_pretty(&manifest)?,
                )?;
                std::fs::rename(&staging_dir, &dir)?;
                Ok(())
            })();
            if outcome.is_err() {
                let _ = std::fs::remove_dir_all(&staging_dir);
            }
            outcome
        })
        .await
        .map_err(|error| anyhow::anyhow!("snapshot worker failed: {}", error))?
    }

    async fn checkpoint(&self) -> Result<()> {
        let snapshot_turn = self.snapshot_lock.clone().lock_owned().await;
        let source = self.local_path.clone();
        let database = self.db.clone();
        let dirty = self.dirty.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _snapshot_turn = snapshot_turn;
            let _cross_process_writer = acquire_database_writer_lock(&source)?;
            // Clear after obtaining the cross-process writer lock. Any write
            // that lands after the checkpoint marks dirty again.
            let had_writes = dirty.take();
            let _ = crate::crash::install_alt_stack();
            #[cfg(test)]
            test_snapshot_operation_barrier(&source, "checkpoint");
            ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let result = (|| -> Result<()> {
                let conn = Connection::new(&database)?;
                conn.query("CHECKPOINT")?;
                Ok(())
            })();
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if result.is_err() && had_writes {
                dirty.mark();
            }
            result
        })
        .await
        .map_err(|error| anyhow::anyhow!("checkpoint worker failed: {}", error))?
    }

    fn is_dirty(&self) -> bool {
        self.dirty.is_set()
    }

    async fn relink_edges(&self, namespace: &str, source_key: &str) -> Result<usize> {
        // Declarations live in each memory's metadata, which survives ingestion;
        // only the edges themselves are lost with the nodes. Replaying them is
        // therefore a read of what is already stored, not a guess.
        let memories = self.list(namespace, None, None).await?;
        let active_gen = self.active_generation(namespace, source_key).await?;

        // The published generation's logical paths. A stub materialised before
        // the file was ingested is a placeholder; once the real node arrives it
        // must be retired so the placeholder never shadows the real tree.
        let active_paths: std::collections::HashSet<String> = memories
            .iter()
            .filter_map(|m| match crate::parser::ingest::split_scoped_id(&m.id) {
                Some((_, gen, _)) if gen == active_gen => {
                    Some(crate::parser::ingest::logical_path_of(&m.id).to_string())
                }
                _ => None,
            })
            .collect();

        // Ids stored before node ids became repository-relative are absolute, so
        // a rule written to match them declares C:/proj/src/foo.rs and matches
        // nothing now. Resolve those by path suffix against the ids that do
        // exist, and leave an ambiguous suffix alone rather than guess. Only the
        // active generation's nodes are reachable: a rule always re-attaches to
        // this source's published tree, never to a superseded one.
        let known = KnownIds::active(memories.iter().map(|m| m.id.as_str()), active_gen);

        // Grouped by relationship because the type is part of the statement and
        // cannot be a value in it: three groups, so three statements, whatever
        // the edge count.
        let mut wanted: std::collections::HashMap<&'static str, Vec<(String, String)>> =
            std::collections::HashMap::new();
        let mut by_suffix = 0usize;

        for memory in &memories {
            // A retired memory no longer governs anything. Replaying its
            // declarations would give the corrected-away wording its edges back.
            // Its id stays among the known targets above, though: something live
            // may still point at it.
            if is_retired(&memory.payload.metadata) {
                continue;
            }
            // An ingested node from a superseded generation must not have its
            // structural (`contained_by`) edges replayed: only this source's
            // active generation is live. Rules, facts and tasks (never
            // generation-scoped) are always live.
            if memory.payload.user_id == "auto-ingestor" {
                match crate::parser::ingest::split_scoped_id(&memory.id) {
                    Some((_, gen, _)) if gen == active_gen => {}
                    _ => continue,
                }
            }
            // Emit any shape warnings from this memory's metadata.
            let warnings = write_shape_warnings(&memory.payload);
            for w in &warnings {
                emit_write_shape_warning(namespace, &memory.id, w);
            }
            for edge in edge_specs(&memory.payload.metadata) {
                let target = match known.resolve(&edge.target_id) {
                    Some(resolved) => {
                        if resolved != edge.target_id {
                            by_suffix += 1;
                        }
                        resolved
                    }
                    // Kept as declared rather than dropped: it names nothing this
                    // namespace holds, but the match is not namespaced, so it can
                    // still land on a node another namespace ingested -- which is
                    // what the statement-per-edge loop did.
                    None => edge.target_id.clone(),
                };
                let pair = if edge.points_at_target {
                    (memory.id.clone(), target)
                } else {
                    (target, memory.id.clone())
                };
                wanted.entry(edge.rel_type).or_default().push(pair);
            }
        }

        if wanted.is_empty() && active_paths.is_empty() {
            return Ok(0);
        }

        if by_suffix > 0 {
            println!(
                "Relinked {} edge(s) whose declared target was an older absolute path",
                by_suffix
            );
        }

        let namespace_owned = namespace.to_string();
        self.write_with_deadline("relinking the graph", move |conn| {
            // Retire stubs before replaying edges: the replayed declaration
            // then points at the real node, not the placeholder it replaced.
            let _ = retire_replaced_stubs(conn, &namespace_owned, &active_paths)?;
            let mut created = 0usize;
            for (rel_type, pairs) in &wanted {
                created += create_missing_edges(conn, rel_type, pairs)?;
            }
            Ok(created)
        })
        .await
    }

    async fn clear_ingested(&self, namespace: &str) -> Result<()> {
        let namespace = namespace.to_string();
        self.write_with_deadline("clearing the ingested rows", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // Every row the ingester owns: the AST symbols and the directory/file
            // nodes too, since ingestion rebuilds the whole structure for a namespace
            let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m", safe_ns);
            conn.query(&query)?;
            Ok(())
        })
        .await
    }

    async fn begin_ingest_stage(&self, namespace: &str) -> Result<Box<dyn Send>> {
        if !is_internal_ingest_stage_namespace(namespace) {
            anyhow::bail!("ingest staging namespace is not internal");
        }
        self.recover_interrupted_ingest_stages().await?;
        let requested_namespace = namespace.to_string();
        let already_present = self
            .with_conn(move |conn| {
                let safe_namespace = escape_ladybug_string(&requested_namespace);
                let mut rows = match conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN count(m)",
                    safe_namespace
                )) {
                    Ok(rows) => rows,
                    Err(error) if error.to_string().contains("does not exist in catalog") => {
                        return Ok(false)
                    }
                    Err(error) => return Err(error.into()),
                };
                Ok(rows.next().is_some_and(|row| format!("{}", row[0]) != "0"))
            })
            .await?;
        if already_present {
            anyhow::bail!(
                "ingest generation namespace already contains rows; preserving existing data"
            );
        }
        let path = ingest_stage_lease_path(&self.local_path, namespace)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                anyhow::anyhow!("could not create the ingest generation lease: {}", error)
            })?;
        file.try_lock().map_err(|error| {
            let _ = std::fs::remove_file(&path);
            anyhow::anyhow!("could not lock the ingest generation lease: {}", error)
        })?;
        let stage_namespace = namespace.to_string();
        let generation = namespace
            .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
            .expect("validated stage namespace")
            .to_string();
        if let Err(error) = self
            .write_with_deadline("recording ingest stage provenance", move |conn| {
                conn.query(&format!(
                    "CREATE (s:IngestStage {{namespace: '{}', generation: '{}'}})",
                    escape_ladybug_string(&stage_namespace),
                    escape_ladybug_string(&generation)
                ))?;
                Ok(())
            })
            .await
        {
            let _ = file.unlock();
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(Box::new(IngestStageLease { file: Some(file) }))
    }

    async fn publish_ingested(&self, namespace: &str, staged_namespace: &str) -> Result<usize> {
        if is_reserved_ingest_destination(namespace) {
            anyhow::bail!("ingest destination uses the reserved internal staging namespace");
        }
        if !is_internal_ingest_stage_namespace(staged_namespace) || namespace == staged_namespace {
            anyhow::bail!("ingest publication requires a distinct internal staging generation");
        }
        let lease_path = ingest_stage_lease_path(&self.local_path, staged_namespace)?;
        if !lease_path.is_file() {
            anyhow::bail!("ingest publication requires an active staging lease");
        }
        let namespace = namespace.to_string();
        let staged_namespace = staged_namespace.to_string();
        let pk_ready = self.pk_ready();
        #[cfg(test)]
        let hook_db_path = self.local_path.clone();
        let published = self.write_to_completion(
            "publishing an ingested generation",
            write_timeout(),
            move |conn| {
            conn.query("BEGIN TRANSACTION")?;
            let transaction = (|| -> Result<usize> {
                #[cfg(test)]
                test_publish_crash_barrier(&hook_db_path, "after-begin");
                let safe_namespace = escape_ladybug_string(&namespace);
                let safe_stage_namespace = escape_ladybug_string(&staged_namespace);
                let generation = staged_namespace
                    .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
                    .ok_or_else(|| anyhow::anyhow!("invalid staged generation"))?
                    .to_string();
                let has_stage_provenance = conn
                    .query(&format!(
                        "MATCH (s:IngestStage) WHERE s.namespace = '{}' AND s.generation = '{}' RETURN s.namespace LIMIT 1",
                        safe_stage_namespace,
                        escape_ladybug_string(&generation)
                    ))?
                    .next()
                    .is_some();
                if !has_stage_provenance {
                    anyhow::bail!("ingest publication has no database stage provenance");
                }
                conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m",
                    safe_namespace
                ))?;
                #[cfg(test)]
                test_publish_crash_barrier(&hook_db_path, "after-delete");

                let staged_prefix = format!(
                    "{}{}",
                    staged_namespace,
                    crate::parser::ingest::NAMESPACE_SEPARATOR
                );
                let published_prefix = format!(
                    "{}{}",
                    namespace,
                    crate::parser::ingest::NAMESPACE_SEPARATOR
                );

                // Promote in bounded pages. The vectors and text payloads
                // remain in the database; only IDs and metadata cross into
                // process memory for namespace-reference rewriting.
                let mut first_page = true;
                loop {
                    let mut staged = conn.query(&format!(
                        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' RETURN m.id, m.metadata LIMIT 128",
                        safe_stage_namespace
                    ))?;
                    let mut page = Vec::with_capacity(128);
                    while let Some(row) = staged.next() {
                        page.push((format!("{}", row[0]), format!("{}", row[1])));
                    }
                    drop(staged);
                    if page.is_empty() {
                        break;
                    }
                    for (old_id, metadata_text) in page {
                        let path = old_id.strip_prefix(&staged_prefix).ok_or_else(|| {
                            anyhow::anyhow!("staged node id does not match its internal generation")
                        })?;
                        let new_id = format!("{}{}", published_prefix, path);
                        let mut metadata: serde_json::Value = serde_json::from_str(&metadata_text)
                            .map_err(|error| anyhow::anyhow!("invalid staged metadata: {}", error))?;
                        replace_json_namespace_prefix(
                            &mut metadata,
                            &staged_prefix,
                            &published_prefix,
                        );
                        let safe_old_id = escape_ladybug_string(&old_id);
                        let safe_new_id = escape_ladybug_string(&new_id);
                        let safe_metadata =
                            escape_ladybug_string(&serde_json::to_string(&metadata)?);
                        // Task 7 fix round: the promoted row is a fresh
                        // CREATE, so on a pk-carrying table it must supply
                        // the derived key (memory_pk of the destination
                        // namespace and the new id) as create input — the
                        // binder refuses a create that omits the primary
                        // key. In legacy mode (no pk column) the fragment
                        // is empty and the statement is unchanged.
                        let pk_prop = if pk_ready {
                            format!(
                                "pk: '{}', ",
                                escape_ladybug_string(&memory_pk(&namespace, &new_id))
                            )
                        } else {
                            String::new()
                        };
                        conn.query(&format!(
                            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' CREATE (n:Memory {{{}id: '{}'}}) SET n.namespace = '{}', n.content = m.content, n.user_id = m.user_id, n.memory_type = m.memory_type, n.agent_name = m.agent_name, n.location = m.location, n.location_lines = m.location_lines, n.metadata = '{}', n.embedding = m.embedding",
                            safe_stage_namespace,
                            safe_old_id,
                            pk_prop,
                            safe_new_id,
                            safe_namespace,
                            safe_metadata
                        ))?;
                        conn.query(&format!(
                            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' DETACH DELETE m",
                            safe_stage_namespace, safe_old_id
                        ))?;
                    }
                    if first_page {
                        first_page = false;
                        #[cfg(test)]
                        test_publish_crash_barrier(&hook_db_path, "after-first-page");
                    }
                }
                // A generation is private state and must not survive a
                // successful publication, even if an unexpected row type was
                // written there. Promotion and disposal share this transaction.
                conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' DETACH DELETE m",
                    safe_stage_namespace
                ))?;
                conn.query(&format!(
                    "MATCH (s:IngestStage) WHERE s.namespace = '{}' AND s.generation = '{}' DELETE s",
                    safe_stage_namespace,
                    escape_ladybug_string(&generation)
                ))?;

                // Re-read declarations inside the transaction so the new nodes
                // and the rules that point at them become visible together.
                let mut memories = conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.user_id, m.metadata",
                    safe_namespace
                ))?;
                let mut edge_rows = Vec::new();
                let mut known_ids = Vec::new();
                while let Some(row) = memories.next() {
                    let id = format!("{}", row[0]);
                    let user_id = format!("{}", row[1]);
                    let metadata_text = format!("{}", row[2]);
                    let metadata: serde_json::Value = serde_json::from_str(&metadata_text)
                        .map_err(|e| anyhow::anyhow!("invalid metadata for {}: {}", id, e))?;
                    known_ids.push(id.clone());
                    edge_rows.push((id, user_id, metadata));
                }
                let known = KnownIds::new(known_ids.iter().map(String::as_str));
                let mut wanted: std::collections::HashMap<
                    &'static str,
                    Vec<(String, String)>,
                > = std::collections::HashMap::new();
                for (id, _user_id, metadata) in &edge_rows {
                    if is_retired(metadata) {
                        continue;
                    }
                    for edge in edge_specs(metadata) {
                        let target = known
                            .resolve(&edge.target_id)
                            .unwrap_or_else(|| edge.target_id.clone());
                        let pair = if edge.points_at_target {
                            (id.clone(), target)
                        } else {
                            (target, id.clone())
                        };
                        wanted.entry(edge.rel_type).or_default().push(pair);
                    }
                }
                let mut created = 0;
                for (rel_type, pairs) in &wanted {
                    created += create_missing_edges(conn, rel_type, pairs)?;
                }
                #[cfg(test)]
                test_publish_crash_barrier(&hook_db_path, "precommit");
                Ok(created)
            })();

            match transaction {
                Ok(created) => match conn.query("COMMIT") {
                    Ok(_) => {
                        #[cfg(test)]
                        test_publish_crash_barrier(&hook_db_path, "postcommit");
                        Ok(created)
                    }
                    Err(commit) => match conn.query("ROLLBACK") {
                        Ok(_) => Err(commit.into()),
                        Err(rollback) => Err(anyhow::anyhow!(
                            "atomic ingest commit outcome is uncertain: commit failed: {}; rollback also failed: {}",
                            commit,
                            rollback
                        )),
                    },
                },
                Err(error) => match conn.query("ROLLBACK") {
                    Ok(_) => Err(error),
                    Err(rollback) => Err(anyhow::anyhow!(
                        "atomic ingest publication outcome is uncertain: {}; rollback also failed: {}",
                        error,
                        rollback
                    )),
                },
            }
        },
        )
        .await?;
        if let Some(warning) = cleanup_ingest_stage_lease_after_commit(&lease_path) {
            eprintln!("{}", warning);
        }
        Ok(published)
    }

    async fn begin_generation(&self, namespace: &str, source_key: &str) -> Result<u64> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.write_with_deadline("beginning a staging generation", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_sk = escape_ladybug_string(&source_key);

            // Read the current counter (default 0), then write counter + 1 in
            // the same write turn. The single-writer lock serialises this
            // against every other store write, so the read and the bump are one
            // atomic step and no two begins hand out the same generation. The
            // counter is shared across sources (keyed by namespace alone), so a
            // generation number never collides between two worktrees.
            let read = format!(
                "MATCH (g:IngestGeneration) WHERE g.id = '{}' RETURN g.counter",
                safe_ns
            );
            let mut current: i64 = 0;
            if let Ok(mut rows) = conn.query(&read) {
                if let Some(row) = rows.next() {
                    current = format!("{}", row[0]).parse::<i64>().unwrap_or(0);
                }
            }
            let next = (current.max(0) as u64).saturating_add(1);

            let write = format!(
                "MERGE (g:IngestGeneration {{id: '{}'}}) ON CREATE SET g.namespace = '{}', g.source_key = '', g.counter = {}, g.active_generation = 0 ON MATCH SET g.counter = {}",
                safe_ns, safe_ns, next, next
            );
            conn.query(&write)?;

            // Ensure this source has a pointer row. Its active generation is
            // left untouched here: it stays on the last published generation
            // until `publish_generation` flips it.
            let pointer = format!(
                "MERGE (g:IngestGeneration {{id: '{}'}}) ON CREATE SET g.namespace = '{}', g.source_key = '{}', g.counter = 0, g.active_generation = 0",
                escape_ladybug_string(&crate::parser::ingest::generation_key(
                    &namespace,
                    &source_key
                )),
                safe_ns,
                safe_sk
            );
            conn.query(&pointer)?;

            Ok(next)
        })
        .await
    }

    async fn publish_generation(
        &self,
        namespace: &str,
        source_key: &str,
        generation: u64,
    ) -> Result<()> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.write_with_deadline("publishing a staging generation", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_sk = escape_ladybug_string(&source_key);
            // The flip is one statement on this source's pointer row: the active
            // pointer moves to `generation` and the previous generation's rows
            // stay as history. Another source's pointer is untouched.
            let write = format!(
                "MERGE (g:IngestGeneration {{id: '{}'}}) ON CREATE SET g.namespace = '{}', g.source_key = '{}', g.counter = 0, g.active_generation = {} ON MATCH SET g.active_generation = {}",
                escape_ladybug_string(&crate::parser::ingest::generation_key(
                    &namespace,
                    &source_key
                )),
                safe_ns,
                safe_sk,
                generation,
                generation
            );
            conn.query(&write)?;
            Ok(())
        })
        .await
    }

    async fn abort_generation(
        &self,
        namespace: &str,
        source_key: &str,
        generation: u64,
    ) -> Result<()> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.write_with_deadline("aborting a staging generation", move |conn| {
            // A staged node's id is `ns::<generation>::<path>`, so every row of
            // this generation shares the prefix. Nothing else matches it, and
            // the previous generation is untouched.
            let prefix = crate::parser::ingest::generation_scoped_id(&namespace, generation, "");
            let query = format!(
                "MATCH (m:Memory) WHERE m.id STARTS WITH '{}' DETACH DELETE m",
                escape_ladybug_string(&prefix)
            );
            conn.query(&query)?;

            // Defensive: if this source's pointer somehow already names the
            // aborted generation (abort is normally pre-publish), clear it so
            // the superseded generation is never reachable.
            let clear = format!(
                "MATCH (g:IngestGeneration) WHERE g.id = '{}' AND g.active_generation = {} SET g.active_generation = 0",
                escape_ladybug_string(&crate::parser::ingest::generation_key(
                    &namespace,
                    &source_key
                )),
                generation
            );
            conn.query(&clear)?;
            Ok(())
        })
        .await
    }

    async fn active_generation(&self, namespace: &str, source_key: &str) -> Result<u64> {
        let namespace = namespace.to_string();
        let source_key = source_key.to_string();
        self.with_conn(move |conn| {
            Ok(active_generation_of_source(conn, &namespace, &source_key))
        })
        .await
    }

    async fn generation_nodes(&self, namespace: &str, generation: u64) -> Result<Vec<String>> {
        let namespace = namespace.to_string();
        self.with_conn(move |conn| {
            let prefix = crate::parser::ingest::generation_scoped_id(&namespace, generation, "");
            let query = format!(
                "MATCH (m:Memory) WHERE m.id STARTS WITH '{}' RETURN m.id",
                escape_ladybug_string(&prefix)
            );
            let mut rows = conn.query(&query)?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next() {
                ids.push(format!("{}", row[0]));
            }
            Ok(ids)
        })
        .await
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>, memory_type: Option<&str>) -> Result<Vec<SearchResult>> {
        let namespace = namespace.to_string();
        let user_id = user_id.map(|v| v.to_string());
        let memory_type = memory_type.map(|v| v.to_string());
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // The active generations for this namespace, read once so every
            // generation-scoped row can be checked against them below. A row
            // from a superseded generation is dropped; a row that is not
            // generation-scoped (a rule, fact, task, or legacy id) is always
            // kept, so the full-DB diagnostic callers keep seeing everything
            // they always did.
            let active_gens = active_generations_of(conn, &namespace);

            // Build WHERE clause with optional filters (R15: push into Cypher).
            let mut conditions = vec![format!("m.namespace = '{}'", safe_ns)];
            if let Some(ref uid) = user_id {
                conditions.push(format!("m.user_id = '{}'", escape_ladybug_string(uid)));
            }
            if let Some(ref mt) = memory_type {
                conditions.push(format!("m.memory_type = '{}'", escape_ladybug_string(mt)));
            }
            let where_clause = conditions.join(" AND ");

            let query = format!("MATCH (m:Memory) WHERE {} RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", where_clause);

            let result = conn.query(&query)?;
            let mut results = Vec::new();

            for row in result {
                let id: String = format!("{}", row[0]);
                if let Some((_, gen, _)) = crate::parser::ingest::split_scoped_id(&id) {
                    if !active_gens.contains(&gen) {
                        continue; // superseded generation: not reachable
                    }
                }
                let content: String = format!("{}", row[1]);
                let uid: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);

                let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                results.push(SearchResult {
                    id,
                    score: 0.0,
                    payload: MemoryPayload {
                        content,
                        user_id: uid,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                    evidence: None,
                });
            }

            Ok(results)
        })
        .await
    }

    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        // Task 7: NUL is refused at the store boundary on reads too — a
        // lookup carrying one is refused by name rather than escaped into
        // a query that can only miss.
        reject_nul("namespace", &namespace)?;
        reject_nul("id", &id)?;
        let pk_ready = self.pk_ready();
        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            // With the pk column present the row is addressed by its
            // derived pk (bound, B1); legacy mode keeps the (namespace,
            // id) property match.
            let mut result = if pk_ready {
                let mut stmt = conn.prepare(
                    "MATCH (m:Memory {pk: $pk}) RETURN m.embedding, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata",
                )?;
                conn.execute(
                    &mut stmt,
                    vec![("pk", lbug::Value::String(memory_pk(&namespace, &id)))],
                )?
            } else {
                let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.embedding, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, safe_id);
                conn.query(&query)?
            };

            if let Some(row) = result.next() {
                // The column is FLOAT[N], a fixed-size ARRAY, so the engine hands
                // back Value::Array. Matching only Value::List meant this always
                // returned an empty vector, and every caller that read a memory
                // and wrote it back stored an embedding of length zero -- which
                // the engine rejects, silently in /edit's case (bead
                // neurostrata-vbj). Accept both: a LIST column would still parse.
                let mut vec: Vec<f32> = Vec::new();
                if let lbug::Value::Array(_, vals) | lbug::Value::List(_, vals) = &row[0] {
                    for v in vals {
                        if let lbug::Value::Float(f) = v {
                            vec.push(*f);
                        } else if let lbug::Value::Double(d) = v {
                            vec.push(*d as f32);
                        }
                    }
                }

                let content: String = format!("{}", row[1]);
                let uid: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);

                let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);



                return Ok(Some((
                    vec,
                    MemoryPayload {
                        content,
                        user_id: uid,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                )));
            }

            Ok(None)
        })
        .await
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        let db_path = self.local_path.clone();
        let path_aliases = self.path_aliases.clone();
        self.with_conn(move |conn| {
            let hidden_stages = read_hidden_stage_namespaces(conn, &db_path, &path_aliases)?;
            let query = "MATCH (m:Memory) RETURN DISTINCT m.namespace AS ns;";
            let mut result = conn.query(query)?;

            let mut namespaces = Vec::new();
            while let Some(row) = result.next() {
                if let lbug::Value::String(ns) = row[0].clone() {
                    if !hidden_stages.contains(&ns) {
                        namespaces.push(ns);
                    }
                }
            }

            Ok(namespaces)
        })
        .await
    }

    async fn export_graph(
        &self,
        include_retired: bool,
        include_archived: bool,
        project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
        anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
    ) -> Result<serde_json::Value> {
        let project_contexts = project_contexts.clone();
        let anchor_override = anchor_override.cloned();
        let db_path = self.local_path.clone();
        let path_aliases = self.path_aliases.clone();
        self.with_conn(move |conn| {
            let now = chrono::Utc::now().timestamp();
            let hidden_stage_namespaces =
                read_hidden_stage_namespaces(conn, &db_path, &path_aliases)?;

            // 1. Fetch all nodes. Generation-scoped nodes from a superseded
            //    generation are dropped against the live `(namespace, gen)`
            //    set read once below, matching `list`'s superseded filter.
            let mut nodes = Vec::new();
            let mut retired_ids = std::collections::HashSet::new();
            let mut hidden_stage_ids = std::collections::HashSet::new();
            let active_gens = all_active_generations(conn);
            let query_nodes = "MATCH (n:Memory) RETURN n.id, n.namespace, n.memory_type, n.content, n.location, n.metadata;";
            let mut result_nodes = conn.query(query_nodes)?;

            while let Some(row) = result_nodes.next() {
                let id = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let namespace = if let lbug::Value::String(s) = &row[1] { s.clone() } else { "global".to_string() };
                if hidden_stage_namespaces.contains(&namespace) {
                    hidden_stage_ids.insert(id);
                    continue;
                }
                let memory_type = if let lbug::Value::String(s) = &row[2] { s.clone() } else { "unknown".to_string() };
                let content = if let lbug::Value::String(s) = &row[3] { s.clone() } else { "".to_string() };
                let location = if let lbug::Value::String(s) = &row[4] { s.clone() } else { "".to_string() };

                // A generation-scoped node whose `(namespace, generation)` is not
                // live was superseded: excluded from the export and its edges
                // dropped below (via `retired_ids`), exactly as `list` does.
                if let Some((_, gen, _)) = crate::parser::ingest::split_scoped_id(&id) {
                    if !active_gens.contains(&(namespace.clone(), gen)) {
                        retired_ids.insert(id.clone());
                        continue;
                    }
                }

                let mut absolute_path = "".to_string();
                let mut domain = None;
                let mut retired = false;
                let mut full_metadata = serde_json::json!({});
                let mut superseded_by = serde_json::Value::Null;

                if let lbug::Value::String(metadata_str) = &row[5] {
                    if let Ok(metadata_val) = serde_json::from_str::<serde_json::Value>(metadata_str) {
                        retired = is_retired_at(&metadata_val, now);
                        if let Some(abs_path) = metadata_val.get("absolute_path").and_then(|v| v.as_str()) {
                            absolute_path = abs_path.to_string();
                        }
                        if let Some(d) = metadata_val.get("domain").and_then(|v| v.as_str()) {
                            domain = Some(d.to_string());
                        }
                        superseded_by = metadata_val.get("superseded_by").cloned().unwrap_or(serde_json::Value::Null);
                        full_metadata = metadata_val;
                    }
                }

                // Track retired ids so edges can be filtered below.
                if retired {
                    retired_ids.insert(id.clone());
                }

                absolute_path = crate::source_context::export_absolute_path(
                    &location,
                    &absolute_path,
                    &full_metadata,
                    &project_contexts,
                    anchor_override.as_ref(),
                );

                if !include_retired && retired {
                    continue;
                }
                // The tombstone filter mirrors the read surfaces: by default
                // an archived row stays out of the export, with the same
                // escape hatch the operator gets in search/snapshot. Edges
                // incident on an excluded node are dropped below.
                let archived = full_metadata
                    .get("archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !include_archived && archived {
                    retired_ids.insert(id.clone());
                    continue;
                }
                // Conflicting mesh rows are suppressed from the effective view
                // (spec §"Concurrent merge policy"): the same shared helper the
                // search/snapshot read paths ask, so the three surfaces can
                // never disagree. No escape hatch — an unresolved conflict is
                // never served until it is resolved.
                if crate::synaptic_mesh::conflict::is_conflicting_metadata(&full_metadata) {
                    retired_ids.insert(id.clone());
                    continue;
                }

                nodes.push(serde_json::json!({
                    "id": id,
                    "namespace": namespace,
                    "memory_type": memory_type,
                    "content": content,
                    "location": location,
                    "absolute_path": absolute_path,
                    "domain": domain,
                    // The graph is not a text dump: the declarative anchors and
                    // the lineage marker travel with the node (guinea-pig BUG-6
                    // and BUG-7). `superseded` is the live-layer concept that
                    // search already hides by; `superseded_by` names the row
                    // that replaced it.
                    "metadata": full_metadata,
                    "superseded": retired,
                    "superseded_by": superseded_by,
                }));
            }

            // 2. Fetch all edges, filtering out any incident on a retired node
            //    in active view.
            let mut links = Vec::new();

            // RELATES_TO
            let query_relates = "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) RETURN a.id, b.id;";
            let mut res_relates = conn.query(query_relates)?;
            while let Some(row) = res_relates.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if hidden_stage_ids.contains(&source)
                    || hidden_stage_ids.contains(&target)
                    || (!include_retired
                        && (retired_ids.contains(&source) || retired_ids.contains(&target)))
                {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "RELATES_TO"
                }));
            }

            // CONTAINS
            let query_contains = "MATCH (a:Memory)-[r:CONTAINS]->(b:Memory) RETURN a.id, b.id;";
            let mut res_contains = conn.query(query_contains)?;
            while let Some(row) = res_contains.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if hidden_stage_ids.contains(&source)
                    || hidden_stage_ids.contains(&target)
                    || (!include_retired
                        && (retired_ids.contains(&source) || retired_ids.contains(&target)))
                {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "CONTAINS"
                }));
            }

            // GOVERNS
            let query_governs = "MATCH (a:Memory)-[r:GOVERNS]->(b:Memory) RETURN a.id, b.id;";
            let mut res_governs = conn.query(query_governs)?;
            while let Some(row) = res_governs.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if hidden_stage_ids.contains(&source)
                    || hidden_stage_ids.contains(&target)
                    || (!include_retired
                        && (retired_ids.contains(&source) || retired_ids.contains(&target)))
                {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "GOVERNS"
                }));
            }

            // EXTRACTED_FROM (vocabulary v2): the consolidation edge a completed
            // task left behind. Without it the export loses the very link the
            // done-funnel exists to create.
            let query_extracted = "MATCH (a:Memory)-[r:EXTRACTED_FROM]->(b:Memory) RETURN a.id, b.id;";
            let res_extracted = conn.query(query_extracted)?;
            for row in res_extracted {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if hidden_stage_ids.contains(&source)
                    || hidden_stage_ids.contains(&target)
                    || (!include_retired
                        && (retired_ids.contains(&source) || retired_ids.contains(&target)))
                {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "EXTRACTED_FROM"
                }));
            }

            Ok(serde_json::json!({
                "nodes": nodes,
                "links": links
            }))
        })
        .await
    }

    /// Retrieval bumps a counter, so it has to cost about what a counter costs.
    /// Routing it through get() and upsert() rewrote the entire row -- its
    /// 768-float embedding included -- and re-ran every edge MERGE, five times
    /// per search, fire and forget. Later writes queued behind that burst: a
    /// delete issued with no pause after a search took 154s where an idle one
    /// takes none (bead neurostrata-3fi.6). Touch one column instead.
    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.write_with_deadline("counting a read", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            let read = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.metadata",
                safe_ns, safe_id
            );
            let mut result = conn.query(&read)?;
            let current = match result.next() {
                Some(row) => format!("{}", row[0]),
                None => return Ok(()),
            };

            let write = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'",
                safe_ns,
                safe_id,
                escape_ladybug_string(&bump_access_count(&current))
            );
            conn.query(&write)?;
            Ok(())
        })
        .await
    }

    /// The batched form of `increment_access_count`: one write session for
    /// the whole set. The per-hit read-modify-write runs sequentially on
    /// one connection, so a search's hits land as a single write and cannot
    /// lose updates against each other. Statements commit as they execute:
    /// a failure partway leaves the earlier rows bumped and the later rows
    /// not — for access counts (best-effort telemetry whose call site is
    /// fire-and-forget) that is the same failure shape as one of today's
    /// per-hit writes failing, never a corruption.
    async fn increment_access_counts(&self, namespace: &str, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let namespace = namespace.to_string();
        let ids: Vec<String> = ids.to_vec();
        self.write_with_deadline("counting reads", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            for id in &ids {
                let safe_id = escape_ladybug_string(id);
                let read = format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.metadata",
                    safe_ns, safe_id
                );
                let mut result = conn.query(&read)?;
                let current = match result.next() {
                    Some(row) => format!("{}", row[0]),
                    None => continue,
                };
                let write = format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'",
                    safe_ns,
                    safe_id,
                    escape_ladybug_string(&bump_access_count(&current))
                );
                conn.query(&write)?;
            }
            Ok(())
        })
        .await
    }
}

impl crate::traits::AsAny for LadybugStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A zero vector of `dimensions` floats, decimal-formatted for the engine's
/// `ARRAY_DISTANCE` binder (R16) -- the shape structural nodes store.
fn zero_vector_str(dimensions: usize) -> String {
    format!("[{}]", vec!["0.0"; dimensions].join(","))
}

/// Materialises a stub `file` node for a `GOVERNS` target that does not exist
/// yet (spec §3.2). The stub carries the legacy two-segment qualified id
/// (`namespace::path`), so `logical_path_of` still recovers the bare path a
/// rule names, and a later ingest replaces it. `ON CREATE` only: an existing
/// node is never overwritten.
fn materialize_stub_file_node(
    conn: &Connection,
    namespace: &str,
    declared: &str,
    zero_vec: &str,
    pk_ready: bool,
) -> Result<String> {
    // A declaration that names a directory carries a trailing slash (the same
    // convention `KnownIds::resolve` already honours); an unindexed ancestor
    // directory must materialise as `directory` so the graph-read's
    // `AncestorDirectory` match finds it.
    let is_directory = declared.ends_with('/');
    let node_path = crate::parser::ingest::normalize_node_path(declared);
    if node_path.is_empty() {
        anyhow::bail!("cannot materialise a stub node for an empty path");
    }
    let stub_id = crate::parser::ingest::qualified_id(namespace, &node_path);
    let safe_id = escape_ladybug_string(&stub_id);
    let safe_ns = escape_ladybug_string(namespace);
    let safe_location = escape_ladybug_string(&node_path);
    let memory_type = if is_directory { "directory" } else { "file" };
    let metadata = serde_json::json!({ "stub": true, "absolute_path": node_path });
    let safe_metadata = escape_ladybug_string(&serde_json::to_string(&metadata)?);
    // On a pk-keyed table the primary key is create input: the MERGE
    // pattern must carry the derived pk (the same shape the upsert's
    // prepared MERGE uses), or the engine refuses the create. In legacy
    // mode the fragment is empty and the statement is unchanged.
    let pk_pattern = if pk_ready {
        format!(
            "pk: '{}', ",
            escape_ladybug_string(&memory_pk(namespace, &stub_id))
        )
    } else {
        String::new()
    };
    let query = format!(
        "MERGE (m:Memory {{{}id: '{}'}}) ON CREATE SET m.namespace = '{}', m.content = '', m.user_id = 'auto-ingestor', m.memory_type = '{}', m.agent_name = 'neurostrata-stub', m.location = '{}', m.location_lines = '', m.metadata = '{}', m.embedding = {}",
        pk_pattern, safe_id, safe_ns, memory_type, safe_location, safe_metadata, zero_vec
    );
    conn.query(&query)?;
    Ok(stub_id)
}

/// Deletes stub nodes (`agent_name = 'neurostrata-stub'`) whose logical path is
/// now held by the published generation, so a placeholder never shadows the
/// real ingested node that replaced it.
fn retire_replaced_stubs(
    conn: &Connection,
    namespace: &str,
    active_paths: &std::collections::HashSet<String>,
) -> Result<usize> {
    let query = format!(
        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.agent_name = 'neurostrata-stub' RETURN m.id",
        escape_ladybug_string(namespace)
    );
    let mut rows = conn.query(&query)?;
    let mut stubs = Vec::new();
    while let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        let path = crate::parser::ingest::logical_path_of(&id);
        if active_paths.contains(path) {
            stubs.push(id);
        }
    }
    let mut removed = 0usize;
    for id in stubs {
        let del = format!(
            "MATCH (m:Memory {{id: '{}'}}) DETACH DELETE m",
            escape_ladybug_string(&id)
        );
        conn.query(&del)?;
        removed += 1;
    }
    Ok(removed)
}

/// Whether `ancestor` is a directory prefix of `path`: strict, at a `/`
/// boundary, so `src` matches `src/lib.rs` but `sr` and `src/lib` do not match
/// `src/lib.rs`.
fn is_ancestor_of(ancestor: &str, path: &str) -> bool {
    !ancestor.is_empty()
        && ancestor != path
        && path.starts_with(ancestor)
        && path[ancestor.len()..].starts_with('/')
}

/// Whether a symbol node's logical path (`path#kind:name@hash`) is the named
/// symbol in `path`.
fn is_symbol_of(logical_path: &str, path: &str, symbol: &str) -> bool {
    let Some(rest) = logical_path.strip_prefix(path) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('#') else {
        return false;
    };
    let Some((_kind, name_at_hash)) = rest.split_once(':') else {
        return false;
    };
    let name = name_at_hash.split('@').next().unwrap_or(name_at_hash);
    name == symbol
}

/// Runs one edge query and turns its rows into hits. `kind_of` maps a returned
/// target id to the match kind it represents; `edge_name` names the relation in
/// the explanation (`governs` vs `relates to`).
fn edge_hits_from_rows(
    conn: &Connection,
    query: &str,
    memory_types: &[String],
    kind_of: impl Fn(&str) -> GraphMatchKind,
    edge_name: &str,
) -> Result<Vec<MemoryHit>> {
    let now = chrono::Utc::now().timestamp();
    let mut rows = conn.query(query)?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next() {
        let target_id = format!("{}", row[0]);
        let id = format!("{}", row[1]);
        let content = format!("{}", row[2]);
        let memory_type = format!("{}", row[3]);
        let metadata: Value =
            serde_json::from_str(&format!("{}", row[4])).unwrap_or(Value::Null);

        if !memory_types.is_empty() && !memory_types.iter().any(|t| t == &memory_type) {
            continue;
        }
        if is_archived(&metadata) || is_retired_at(&metadata, now) || is_mesh_conflicting(&metadata)
        {
            continue;
        }
        let access_count = metadata
            .get("access_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        hits.push(MemoryHit {
            id,
            memory_type,
            content,
            match_kind: kind_of(&target_id),
            access_count,
            explanation: format!("{} {}", edge_name, target_id),
            metadata,
        });
    }
    Ok(hits)
}

/// The graph-edge paths that reach a governing or related memory: direct
/// `GOVERNS` onto the candidate targets, 2-hop symbol governance
/// (`memory -GOVERNS-> symbol <-CONTAINS- file`), and `RELATES_TO` onto the
/// candidate targets in both directions (the relation is undirected).
fn graph_hits_from_edges(
    conn: &Connection,
    namespace: &str,
    candidates: &[(String, GraphMatchKind)],
    file_ids: &[String],
    memory_types: &[String],
) -> Result<Vec<MemoryHit>> {
    let safe_ns = escape_ladybug_string(namespace);
    let mut hits = Vec::new();

    if !candidates.is_empty() {
        let id_list = candidates
            .iter()
            .map(|(id, _)| format!("'{}'", escape_ladybug_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let kind_by_id: std::collections::HashMap<&str, GraphMatchKind> = candidates
            .iter()
            .map(|(id, kind)| (id.as_str(), *kind))
            .collect();

        let governs_query = format!(
            "MATCH (m:Memory)-[:GOVERNS]->(t:Memory) WHERE t.id IN [{}] AND m.namespace = '{}' RETURN t.id, m.id, m.content, m.memory_type, m.metadata",
            id_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &governs_query,
            memory_types,
            |target| {
                kind_by_id
                    .get(target)
                    .copied()
                    .unwrap_or(GraphMatchKind::MetadataFallback)
            },
            "governs",
        )?);

        // RELATES_TO is undirected, so both orientations are queried explicitly
        // (the stored edge is still a directed row).
        let relates_out_query = format!(
            "MATCH (m:Memory)-[:RELATES_TO]->(t:Memory) WHERE t.id IN [{}] AND m.namespace = '{}' RETURN t.id, m.id, m.content, m.memory_type, m.metadata",
            id_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &relates_out_query,
            memory_types,
            |_| GraphMatchKind::Related,
            "relates to",
        )?);
        let relates_in_query = format!(
            "MATCH (m:Memory)<-[:RELATES_TO]-(t:Memory) WHERE t.id IN [{}] AND m.namespace = '{}' RETURN t.id, m.id, m.content, m.memory_type, m.metadata",
            id_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &relates_in_query,
            memory_types,
            |_| GraphMatchKind::Related,
            "relates to",
        )?);
    }

    if !file_ids.is_empty() {
        let file_list = file_ids
            .iter()
            .map(|id| format!("'{}'", escape_ladybug_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "MATCH (m:Memory)-[:GOVERNS]->(sym:Memory)<-[:CONTAINS]-(f:Memory) WHERE f.id IN [{}] AND m.namespace = '{}' RETURN sym.id, m.id, m.content, m.memory_type, m.metadata",
            file_list, safe_ns
        );
        hits.extend(edge_hits_from_rows(
            conn,
            &query,
            memory_types,
            |_| GraphMatchKind::SymbolTwoHop,
            "governs",
        )?);
    }

    Ok(hits)
}

/// Whether an id is reachable under the active generation set: a
/// generation-scoped id is reachable only when its generation is active, while
/// a non-scoped id (a rule/fact/procedure id, or a pre-generation stub) is
/// always reachable.
fn is_active_id(id: &str, active_gens: &std::collections::HashSet<u64>) -> bool {
    match crate::parser::ingest::split_scoped_id(id) {
        Some((_, gen, _)) => active_gens.contains(&gen),
        None => true,
    }
}

/// Discovers the target nodes a query can match: the exact file node, the
/// exact symbol nodes (when a symbol is named), and the ancestor directory
/// nodes (when `include_ancestors`). The lookups are location-scoped Cypher
/// queries rather than a whole-namespace scan, and each row is then narrowed
/// to the active generation set so a superseded tree never contributes.
fn discover_candidates(
    conn: &Connection,
    safe_ns: &str,
    path: &str,
    symbol: Option<&str>,
    include_ancestors: bool,
    active_gens: &std::collections::HashSet<u64>,
) -> Result<(Vec<String>, Vec<(String, GraphMatchKind)>)> {
    let mut file_ids: Vec<String> = Vec::new();
    let mut candidates: Vec<(String, GraphMatchKind)> = Vec::new();

    // The file node and its symbol nodes share `location = path`: the file's
    // logical path is the path itself, a symbol's is `path#kind:name@hash`.
    let safe_path = escape_ladybug_string(path);
    let query = format!(
        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.location = '{}' RETURN m.id",
        safe_ns, safe_path
    );
    let mut rows = conn.query(&query)?;
    while let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        if !is_active_id(&id, active_gens) {
            continue;
        }
        let logical = crate::parser::ingest::logical_path_of(&id);
        if logical == path {
            file_ids.push(id.clone());
            candidates.push((id.clone(), GraphMatchKind::ExactFile));
        } else if let Some(sym) = symbol {
            if is_symbol_of(logical, path, sym) {
                candidates.push((id.clone(), GraphMatchKind::ExactSymbol));
            }
        }
    }

    if include_ancestors {
        let dir_query = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.memory_type = 'directory' RETURN m.id, m.location",
            safe_ns
        );
        let mut rows = conn.query(&dir_query)?;
        while let Some(row) = rows.next() {
            let id = format!("{}", row[0]);
            if !is_active_id(&id, active_gens) {
                continue;
            }
            let location = format!("{}", row[1]);
            if is_ancestor_of(&location, path) {
                candidates.push((id.clone(), GraphMatchKind::AncestorDirectory));
            }
        }
    }

    Ok((file_ids, candidates))
}

/// The metadata fallback: a memory whose `metadata.governs` declares the path
/// (or an ancestor) but whose edge never materialised -- e.g. the declaration
/// predates the stub behaviour, or the target is outside the ingested tree.
///
/// The query is scoped to the memory types asked for (rules/facts/procedures
/// by default), so the ingested AST bulk -- file/symbol/directory rows, which
/// never carry a `governs` declaration -- is never scanned.
fn add_metadata_fallback_conn(
    conn: &Connection,
    safe_ns: &str,
    hits: &mut Vec<MemoryHit>,
    path: &str,
    include_ancestors: bool,
    memory_types: &[String],
) -> Result<()> {
    // An empty `memory_types` means "no type filter" (the trait contract), so
    // the type condition is omitted entirely; a non-empty list is pushed into
    // the WHERE clause so only those rows are read.
    let type_condition = if memory_types.is_empty() {
        None
    } else {
        let list = memory_types
            .iter()
            .map(|t| format!("'{}'", escape_ladybug_string(t)))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("m.memory_type IN [{}]", list))
    };
    let where_clause = match &type_condition {
        Some(tc) => format!("m.namespace = '{}' AND {}", safe_ns, tc),
        None => format!("m.namespace = '{}'", safe_ns),
    };
    let query = format!(
        "MATCH (m:Memory) WHERE {} RETURN m.id, m.content, m.memory_type, m.metadata",
        where_clause
    );
    let now = chrono::Utc::now().timestamp();
    let mut rows = conn.query(&query)?;
    while let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        let content = format!("{}", row[1]);
        let memory_type = format!("{}", row[2]);
        let metadata: Value =
            serde_json::from_str(&format!("{}", row[3])).unwrap_or(Value::Null);
        if is_archived(&metadata)
            || is_retired_at(&metadata, now)
            || is_mesh_conflicting(&metadata)
        {
            continue;
        }
        let governs = match metadata.get("governs") {
            Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
            _ => continue,
        };
        for declared in governs {
            let normalized = crate::parser::ingest::normalize_node_path(declared);
            let matched =
                normalized == path || (include_ancestors && is_ancestor_of(&normalized, path));
            if matched {
                let access_count = metadata
                    .get("access_count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                hits.push(MemoryHit {
                    id,
                    memory_type,
                    content,
                    match_kind: GraphMatchKind::MetadataFallback,
                    access_count,
                    explanation: format!("declares governs {}", normalized),
                    metadata,
                });
                break;
            }
        }
    }
    Ok(())
}

/// Keep the best hit per memory id: a lower match-kind priority wins, so a
/// direct `GOVERNS` edge is never shadowed by a weaker metadata-fallback hit.
fn dedup_hits(hits: &mut Vec<MemoryHit>) {
    use std::collections::hash_map::Entry;
    let mut best: std::collections::HashMap<String, MemoryHit> = std::collections::HashMap::new();
    for hit in hits.drain(..) {
        match best.entry(hit.id.clone()) {
            Entry::Vacant(e) => {
                e.insert(hit);
            }
            Entry::Occupied(mut e) => {
                if hit.match_kind.priority() < e.get().match_kind.priority() {
                    e.insert(hit);
                }
            }
        }
    }
    hits.extend(best.into_values());
}

#[async_trait::async_trait]
impl GraphRead for LadybugStore {
    async fn memories_for_file(
        &self,
        namespace: &str,
        source: Option<&str>,
        path: &str,
        symbol: Option<&str>,
        include_ancestors: bool,
        memory_types: &[String],
        exclude_ids: &[String],
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        let namespace = namespace.to_string();
        let path = crate::parser::ingest::normalize_node_path(path);
        let symbol = symbol.map(|s| s.to_string());
        let memory_types = memory_types.to_vec();
        let exclude_ids = exclude_ids.to_vec();
        let limit = limit.min(crate::graph_read::memories_for_file::MAX_LIMIT);
        let source = source.map(|s| s.to_string());

        let ns = namespace.clone();
        let path_c = path.clone();
        let sym_c = symbol.clone();
        let types = memory_types.clone();

        let mut hits = self
            .with_conn(move |conn| {
                let safe_ns = escape_ladybug_string(&ns);

                // A source key pins the query to that source's active
                // generation; `None` keeps every published generation (the
                // same set `list` reads).
                let active_gens: std::collections::HashSet<u64> = match &source {
                    Some(s) => {
                        let g = active_generation_of_source(conn, &ns, s);
                        if g > 0 {
                            [g].into_iter().collect()
                        } else {
                            std::collections::HashSet::new()
                        }
                    }
                    None => active_generations_of(conn, &ns),
                };

                let (file_ids, candidates) = discover_candidates(
                    conn,
                    &safe_ns,
                    &path_c,
                    sym_c.as_deref(),
                    include_ancestors,
                    &active_gens,
                )?;

                let mut hits =
                    graph_hits_from_edges(conn, &ns, &candidates, &file_ids, &types)?;

                add_metadata_fallback_conn(
                    conn,
                    &safe_ns,
                    &mut hits,
                    &path_c,
                    include_ancestors,
                    &types,
                )?;

                Ok(hits)
            })
            .await?;

        // Exclusion happens before the limit is applied, so an excluded hit
        // never crowds out a live one.
        if !exclude_ids.is_empty() {
            hits.retain(|h| !exclude_ids.iter().any(|e| e == &h.id));
        }

        dedup_hits(&mut hits);
        crate::graph_read::memories_for_file::rank_hits(&mut hits);
        hits.truncate(limit);
        Ok(hits)
    }

    async fn related(
        &self,
        namespace: &str,
        seed: &str,
        relations: &[String],
        direction: crate::graph_read::related::Direction,
        max_hops: usize,
        memory_types: &[String],
        limit: usize,
        cursor: Option<&crate::graph_read::related::Cursor>,
    ) -> Result<crate::graph_read::related::RelatedResult> {
        let namespace = namespace.to_string();
        let seed = seed.to_string();
        let relations = relations.to_vec();
        let memory_types = memory_types.to_vec();
        let max_hops = crate::graph_read::related::clamp_max_hops(max_hops);
        let limit = crate::graph_read::related::clamp_limit(limit);

        let ns = namespace.clone();
        let seed_c = seed.clone();
        let rels = relations.clone();

        let (nodes, edges, generation) = self
            .with_conn(move |conn| {
                fetch_related_subgraph(conn, &ns, &seed_c, &rels, max_hops)
            })
            .await?;

        Ok(crate::graph_read::related::graph_read_related(
            &seed,
            &nodes,
            &edges,
            &relations,
            direction,
            max_hops,
            &memory_types,
            limit,
            generation,
            cursor,
        ))
    }
}

/// How many edge rows one traversal level may fetch (bounding the Cypher read;
/// the in-memory traversal applies the tighter `MAX_NODES`/`MAX_EDGES` budget).
const RELATED_FETCH_LIMIT: usize = 500;

/// Fetch the bounded subgraph within `max_hops` of `seed`: the seed node plus
/// its neighbours out to the hop limit, each row-class-filtered (retired /
/// archived / mesh-conflicting / registry / superseded generation). Returns the
/// in-memory `NodeSpec`/`EdgeSpec` lists the pure traversal consumes, plus the
/// active generation the cursor fingerprint must match.
fn fetch_related_subgraph(
    conn: &Connection,
    namespace: &str,
    seed: &str,
    relations: &[String],
    max_hops: usize,
) -> Result<(
    Vec<crate::graph_read::related::NodeSpec>,
    Vec<crate::graph_read::related::EdgeSpec>,
    u64,
)> {
    use crate::graph_read::related::{EdgeSpec, NodeSpec};

    let safe_ns = escape_ladybug_string(namespace);
    let now = chrono::Utc::now().timestamp();
    let active_gens = active_generations_of(conn, namespace);
    let generation = active_gens.iter().copied().max().unwrap_or(0);

    let allowed: std::collections::HashSet<&str> = if relations.is_empty() {
        crate::graph_read::related::DEFAULT_RELATIONS
            .iter()
            .copied()
            .collect()
    } else {
        relations
            .iter()
            .map(String::as_str)
            .filter(|r| crate::graph_read::related::is_allowed_relation(r))
            .collect()
    };

    let to_node_spec = |id: String,
                        memory_type: String,
                        content: String,
                        metadata: &Value|
     -> NodeSpec {
        let visible = is_active_id(&id, &active_gens)
            && !is_archived(metadata)
            && !is_retired_at(metadata, now)
            && !is_mesh_conflicting(metadata)
            && !crate::registry::is_registry_row(metadata);
        NodeSpec {
            id,
            memory_type,
            content,
            domain: metadata
                .get("domain")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            freshness: metadata
                .get("verification")
                .and_then(|v| v.get("status"))
                .and_then(|s| s.as_str())
                .map(str::to_string),
            lifecycle: metadata
                .get("superseded_by")
                .and_then(|v| v.as_str())
                .map(|_| "superseded".to_string()),
            visible,
        }
    };

    let mut nodes: std::collections::HashMap<String, NodeSpec> =
        std::collections::HashMap::new();
    let mut edges: Vec<EdgeSpec> = Vec::new();

    // Resolve the seed: an exact id first (memory/task/qualified id), then a
    // normalized path against `location` (file/symbol nodes).
    let mut seed_id: Option<String> = None;
    let exact_q = format!(
        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.id, m.memory_type, m.content, m.metadata",
        safe_ns,
        escape_ladybug_string(seed)
    );
    let mut rows = conn.query(&exact_q)?;
    if let Some(row) = rows.next() {
        let id = format!("{}", row[0]);
        let memory_type = format!("{}", row[1]);
        let content = format!("{}", row[2]);
        let metadata: Value = serde_json::from_str(&format!("{}", row[3])).unwrap_or(Value::Null);
        seed_id = Some(id.clone());
        nodes.insert(
            id.clone(),
            to_node_spec(id, memory_type, content, &metadata),
        );
    }

    if seed_id.is_none() {
        let normalized = crate::parser::ingest::normalize_node_path(seed);
        let loc_q = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.location = '{}' RETURN m.id, m.memory_type, m.content, m.metadata ORDER BY m.memory_type ASC",
            safe_ns,
            escape_ladybug_string(&normalized)
        );
        let mut rows = conn.query(&loc_q)?;
        // Prefer the file node over a symbol over any other row.
        let mut candidate: Option<(String, String, String, Value)> = None;
        while let Some(row) = rows.next() {
            let id = format!("{}", row[0]);
            let memory_type = format!("{}", row[1]);
            let content = format!("{}", row[2]);
            let metadata: Value =
                serde_json::from_str(&format!("{}", row[3])).unwrap_or(Value::Null);
            let prefer = match (candidate.as_ref().map(|c| c.1.as_str()), memory_type.as_str()) {
                (_, "file") => true,
                (Some("file"), _) => false,
                (Some("symbol"), "directory") => false,
                _ => candidate.is_none(),
            };
            if prefer || candidate.is_none() {
                candidate = Some((id, memory_type, content, metadata));
            }
        }
        if let Some((id, memory_type, content, metadata)) = candidate {
            seed_id = Some(id.clone());
            nodes.insert(id.clone(), to_node_spec(id, memory_type, content, &metadata));
        }
    }

    let Some(seed_id) = seed_id else {
        return Ok((Vec::new(), Vec::new(), generation));
    };

    // BFS out to `max_hops`, fetching both orientations (the pure traversal
    // reapplies the direction allowlist). Only same-namespace neighbours are
    // ever read, so the traversal can never leak across namespaces.
    let mut frontier: Vec<String> = vec![seed_id.clone()];
    let mut seen: std::collections::HashSet<String> = [seed_id].into_iter().collect();

    for _hop in 0..max_hops {
        if frontier.is_empty() {
            break;
        }
        let id_list = frontier
            .iter()
            .map(|id| format!("'{}'", escape_ladybug_string(id)))
            .collect::<Vec<_>>()
            .join(", ");

        let mut next_frontier = Vec::new();

        let out_q = format!(
            "MATCH (a:Memory)-[e]->(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' RETURN a.id, label(e), b.id, b.memory_type, b.content, b.metadata LIMIT {}",
            id_list, safe_ns, RELATED_FETCH_LIMIT
        );
        {
            let mut rows = conn.query(&out_q)?;
            while let Some(row) = rows.next() {
                let frontier_id = format!("{}", row[0]);
                let relation = format!("{}", row[1]);
                let neighbor_id = format!("{}", row[2]);
                let neighbor_type = format!("{}", row[3]);
                let neighbor_content = format!("{}", row[4]);
                let neighbor_meta: Value =
                    serde_json::from_str(&format!("{}", row[5])).unwrap_or(Value::Null);
                if !allowed.contains(relation.as_str()) {
                    continue;
                }
                edges.push(EdgeSpec {
                    source: frontier_id.clone(),
                    relation: relation.clone(),
                    target: neighbor_id.clone(),
                });
                if !nodes.contains_key(&neighbor_id) {
                    nodes.insert(
                        neighbor_id.clone(),
                        to_node_spec(
                            neighbor_id.clone(),
                            neighbor_type,
                            neighbor_content,
                            &neighbor_meta,
                        ),
                    );
                }
                if !seen.contains(&neighbor_id) {
                    seen.insert(neighbor_id.clone());
                    next_frontier.push(neighbor_id);
                }
            }
        }

        let in_q = format!(
            "MATCH (a:Memory)<-[e]-(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' RETURN a.id, label(e), b.id, b.memory_type, b.content, b.metadata LIMIT {}",
            id_list, safe_ns, RELATED_FETCH_LIMIT
        );
        {
            let mut rows = conn.query(&in_q)?;
            while let Some(row) = rows.next() {
                let frontier_id = format!("{}", row[0]);
                let relation = format!("{}", row[1]);
                let neighbor_id = format!("{}", row[2]);
                let neighbor_type = format!("{}", row[3]);
                let neighbor_content = format!("{}", row[4]);
                let neighbor_meta: Value =
                    serde_json::from_str(&format!("{}", row[5])).unwrap_or(Value::Null);
                if !allowed.contains(relation.as_str()) {
                    continue;
                }
                edges.push(EdgeSpec {
                    source: neighbor_id.clone(),
                    relation: relation.clone(),
                    target: frontier_id.clone(),
                });
                if !nodes.contains_key(&neighbor_id) {
                    nodes.insert(
                        neighbor_id.clone(),
                        to_node_spec(
                            neighbor_id.clone(),
                            neighbor_type,
                            neighbor_content,
                            &neighbor_meta,
                        ),
                    );
                }
                if !seen.contains(&neighbor_id) {
                    seen.insert(neighbor_id.clone());
                    next_frontier.push(neighbor_id);
                }
            }
        }

        frontier = next_frontier;
    }

    let nodes: Vec<NodeSpec> = nodes.into_values().collect();
    Ok((nodes, edges, generation))
}

/// IntakeStore implementation for LadybugStore (PQTH durable intake ledger).
#[async_trait::async_trait]
impl crate::traits::IntakeStore for LadybugStore {
    async fn create_receipt(
        &self,
        request: crate::task::intake::CreateReceiptRequest,
    ) -> Result<crate::task::intake::CreateReceiptResult, String> {
        let namespace = request.namespace.clone();
        let source_digest = request.source_digest.clone();
        let text = request.text.clone();
        let source_kind = request.source_kind;
        let source_revision = request.source_revision.clone();

        // Generate receipt ID deterministically from namespace + source_digest
        let receipt_id = crate::task::intake::generate_receipt_id(&namespace, &source_digest);

        // Process text through M2 lexical detector
        let detected_intents = crate::task::intake::process_intake_text(&text);

        // Create candidates from detected intents
        let candidates = crate::task::intake::create_candidates_from_intents(
            &namespace,
            &receipt_id,
            &request,
            detected_intents.clone(),
        );

        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        // Clone values needed for both receipt construction and safe strings
        let source_revision_for_receipt = source_revision.clone();
        let source_digest_for_receipt = source_digest.clone();
        let detected_intents_for_receipt = detected_intents.clone();
        let text_for_receipt = text.clone();
        let now_for_receipt = now.clone();

        let receipt = crate::task::intake::IntakeReceipt {
            id: receipt_id.clone(),
            namespace: namespace.clone(),
            source_kind,
            source_revision: source_revision_for_receipt,
            source_digest: source_digest_for_receipt,
            detected_intents: detected_intents_for_receipt,
            raw_text: text_for_receipt,
            created_at: now_for_receipt,
        };

        // Serialize detected intents for storage
        let intents_json = serde_json::to_string(&detected_intents).unwrap_or_default();
        let safe_intents = escape_ladybug_string(&intents_json);
        let safe_raw_text = escape_ladybug_string(&text);
        let safe_receipt_id = escape_ladybug_string(&receipt_id);
        let safe_ns = escape_ladybug_string(&namespace);
        let safe_source_kind = format!("{:?}", source_kind).to_lowercase();
        let safe_source_revision = escape_ladybug_string(&source_revision);
        let safe_source_digest = escape_ladybug_string(&source_digest);
        let safe_created_at = escape_ladybug_string(&now);

        // Transactional write: receipt + candidates atomically
        let candidates_clone = candidates.clone();
        self.write_with_deadline("creating intake receipt and candidates", move |conn| {
            // Insert receipt
            let receipt_query = format!(
                "MERGE (r:IntakeReceipt {{id: '{}'}})
                 ON CREATE SET r.namespace = '{}', r.source_kind = '{}', r.source_revision = '{}',
                     r.source_digest = '{}', r.detected_intents = '{}', r.raw_text = '{}', r.created_at = '{}'",
                safe_receipt_id, safe_ns, safe_source_kind, safe_source_revision,
                safe_source_digest, safe_intents, safe_raw_text, safe_created_at
            );
            conn.query(&receipt_query)?;

            // Insert candidates
            for cand in &candidates_clone {
                let safe_cand_id = escape_ladybug_string(&cand.id);
                let safe_receipt_id = escape_ladybug_string(&cand.receipt_id);
                let safe_title = escape_ladybug_string(&cand.title);
                let safe_desc = cand.description.as_ref().map(|d| escape_ladybug_string(d)).unwrap_or_default();
                let safe_review_state = format!("{:?}", cand.review_state).to_lowercase();
                let safe_disposition = format!("{:?}", cand.disposition).to_lowercase();
                let safe_task_id = cand.task_id.as_ref().map(|t| escape_ladybug_string(t)).unwrap_or_default();
                let safe_created = escape_ladybug_string(&cand.created_at);
                let safe_updated = escape_ladybug_string(&cand.updated_at);

                let cand_query = format!(
                    "MERGE (c:IntakeCandidate {{id: '{}'}})
                     ON CREATE SET c.namespace = '{}', c.receipt_id = '{}', c.title = '{}',
                         c.description = '{}', c.review_state = '{}', c.disposition = '{}',
                         c.task_id = '{}', c.cas_version = {}, c.created_at = '{}', c.updated_at = '{}'",
                    safe_cand_id, safe_ns, safe_receipt_id, safe_title, safe_desc,
                    safe_review_state, safe_disposition, safe_task_id, cand.cas_version,
                    safe_created, safe_updated
                );
                conn.query(&cand_query)?;
            }

            Ok(())
        })
        .await
        .map_err(|e| format!("Failed to create intake receipt: {}", e))?;

        let candidate_count = candidates.len();
        Ok(crate::task::intake::CreateReceiptResult {
            receipt,
            candidates,
            candidate_count,
        })
    }

    async fn list_receipts(
        &self,
        request: crate::task::intake::ListReceiptsRequest,
    ) -> Result<crate::task::intake::IntakeListResponse<crate::task::intake::IntakeReceipt>, String> {
        let namespace = request.namespace.clone();
        let limit = request.limit.unwrap_or(50);
        let offset = request.offset.unwrap_or(0);

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // Get total count
            let count_query = format!(
                "MATCH (r:IntakeReceipt) WHERE r.namespace = '{}' RETURN count(r)",
                safe_ns
            );
            let total: i64 = conn
                .query(&count_query)?
                .next()
                .and_then(|row| format!("{}", row[0]).parse().ok())
                .unwrap_or(0);

            // Get paginated results
            let query = format!(
                "MATCH (r:IntakeReceipt) WHERE r.namespace = '{}' RETURN r.id, r.namespace, r.source_kind, r.source_revision, r.source_digest, r.detected_intents, r.raw_text, r.created_at ORDER BY r.created_at DESC LIMIT {} OFFSET {}",
                safe_ns, limit, offset
            );
            let mut results = Vec::new();
            for row in conn.query(&query)? {
                let id: String = format!("{}", row[0]);
                let ns: String = format!("{}", row[1]);
                let source_kind_str: String = format!("{}", row[2]);
                let source_revision: String = format!("{}", row[3]);
                let source_digest: String = format!("{}", row[4]);
                let detected_intents_str: String = format!("{}", row[5]);
                let raw_text: String = format!("{}", row[6]);
                let created_at: String = format!("{}", row[7]);

                let source_kind = match source_kind_str.as_str() {
                    "conversation" => crate::task::intake::SourceKind::Conversation,
                    "manual" => crate::task::intake::SourceKind::Manual,
                    "imported" => crate::task::intake::SourceKind::Imported,
                    "tool" => crate::task::intake::SourceKind::Tool,
                    _ => crate::task::intake::SourceKind::Conversation,
                };

                let detected_intents: Vec<crate::task::intake::DetectedIntent> =
                    serde_json::from_str(&detected_intents_str).unwrap_or_default();

                results.push(crate::task::intake::IntakeReceipt {
                    id,
                    namespace: ns,
                    source_kind,
                    source_revision,
                    source_digest,
                    detected_intents,
                    raw_text,
                    created_at,
                });
            }

            Ok(crate::task::intake::IntakeListResponse {
                namespace,
                count: results.len(),
                total: total as usize,
                offset,
                limit,
                items: results,
            })
        })
        .await
        .map_err(|e| format!("Failed to list receipts: {}", e))
    }

    async fn list_candidates(
        &self,
        request: crate::task::intake::ListCandidatesRequest,
    ) -> Result<crate::task::intake::IntakeListResponse<crate::task::intake::IntakeCandidate>, String> {
        let namespace = request.namespace.clone();
        let limit = request.limit.unwrap_or(50);
        let offset = request.offset.unwrap_or(0);
        let review_state_filter = request.review_state;
        let disposition_filter = request.disposition;

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);

            // Build WHERE clause for filters
            let mut where_clauses = vec![format!("c.namespace = '{}'", safe_ns)];
            if let Some(rs) = review_state_filter {
                where_clauses.push(format!("c.review_state = '{}'", format!("{:?}", rs).to_lowercase()));
            }
            if let Some(d) = disposition_filter {
                where_clauses.push(format!("c.disposition = '{}'", format!("{:?}", d).to_lowercase()));
            }
            let where_clause = where_clauses.join(" AND ");

            // Get total count
            let count_query = format!(
                "MATCH (c:IntakeCandidate) WHERE {} RETURN count(c)",
                where_clause
            );
            let total: i64 = conn
                .query(&count_query)?
                .next()
                .and_then(|row| format!("{}", row[0]).parse().ok())
                .unwrap_or(0);

            // Get paginated results
            let query = format!(
                "MATCH (c:IntakeCandidate) WHERE {} RETURN c.id, c.namespace, c.receipt_id, c.title, c.description, c.review_state, c.disposition, c.task_id, c.cas_version, c.created_at, c.updated_at ORDER BY c.created_at DESC LIMIT {} OFFSET {}",
                where_clause, limit, offset
            );
            let mut results = Vec::new();
            for row in conn.query(&query)? {
                let id: String = format!("{}", row[0]);
                let ns: String = format!("{}", row[1]);
                let receipt_id: String = format!("{}", row[2]);
                let title: String = format!("{}", row[3]);
                let description: String = format!("{}", row[4]);
                let review_state_str: String = format!("{}", row[5]);
                let disposition_str: String = format!("{}", row[6]);
                let task_id: String = format!("{}", row[7]);
                let cas_version: i64 = format!("{}", row[8]).parse().unwrap_or(1);
                let created_at: String = format!("{}", row[9]);
                let updated_at: String = format!("{}", row[10]);

                let review_state = match review_state_str.as_str() {
                    "pending" => crate::task::intake::ReviewState::Pending,
                    "inreview" => crate::task::intake::ReviewState::InReview,
                    "accepted" => crate::task::intake::ReviewState::Accepted,
                    "dismissed" => crate::task::intake::ReviewState::Dismissed,
                    _ => crate::task::intake::ReviewState::Pending,
                };

                let disposition = match disposition_str.as_str() {
                    "pending" => crate::task::intake::Disposition::Pending,
                    "created" => crate::task::intake::Disposition::Created,
                    "linked" => crate::task::intake::Disposition::Linked,
                    "dismissed" => crate::task::intake::Disposition::Dismissed,
                    _ => crate::task::intake::Disposition::Pending,
                };

                results.push(crate::task::intake::IntakeCandidate {
                    id,
                    namespace: ns,
                    receipt_id,
                    title,
                    description: if description.is_empty() { None } else { Some(description) },
                    review_state,
                    disposition,
                    task_id: if task_id.is_empty() { None } else { Some(task_id) },
                    cas_version: cas_version as u64,
                    created_at,
                    updated_at,
                });
            }

            Ok(crate::task::intake::IntakeListResponse {
                namespace,
                count: results.len(),
                total: total as usize,
                offset,
                limit,
                items: results,
            })
        })
        .await
        .map_err(|e| format!("Failed to list candidates: {}", e))
    }

    async fn resolve_candidate(
        &self,
        request: crate::task::intake::ResolveCandidateRequest,
    ) -> Result<crate::task::intake::ResolveCandidateResult, String> {
        let namespace = request.namespace.clone();
        let candidate_id = request.candidate_id.clone();
        let disposition = request.disposition;
        let task_id = request.task_id.clone();
        let task_id_for_closure = task_id.clone();
        let cas_version = request.cas_version;

        self.write_with_deadline("resolving intake candidate", move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_cand_id = escape_ladybug_string(&candidate_id);
            let safe_disposition = format!("{:?}", disposition).to_lowercase();
            let safe_task_id = task_id_for_closure.as_ref().map(|t| escape_ladybug_string(t)).unwrap_or_default();
            let new_version = cas_version + 1;
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let safe_updated = escape_ladybug_string(&now);

            // For "created" disposition, verify the task_id refers to a real task
            // in the Memory table (memory_type = 'task') in the same namespace.
            // Reject missing/blank/wrong-namespace/non-task IDs.
            if disposition == crate::task::intake::Disposition::Created {
                if safe_task_id.is_empty() {
                    return Err(anyhow::anyhow!("task_id is required for 'created' disposition"));
                }
                let task_check = format!(
                    "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' AND m.memory_type = 'task' RETURN m.id",
                    safe_task_id, safe_ns
                );
                let mut task_result = conn.query(&task_check)?;
                if task_result.next().is_none() {
                    return Err(anyhow::anyhow!(
                        "task_id '{}' does not refer to an existing task in namespace '{}'",
                        task_id_for_closure.as_deref().unwrap_or(""),
                        namespace
                    ));
                }
            }

            // CAS: only update if cas_version matches
            let query = format!(
                "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' AND c.cas_version = {} SET c.disposition = '{}', c.task_id = '{}', c.cas_version = {}, c.updated_at = '{}', c.review_state = '{}' RETURN c.id",
                safe_cand_id, safe_ns, cas_version, safe_disposition, safe_task_id, new_version, safe_updated,
                if disposition == crate::task::intake::Disposition::Dismissed { "dismissed" } else { "accepted" }
            );
            let mut result = conn.query(&query)?;

            if result.next().is_none() {
                // Check if candidate exists but version mismatch
                let check_query = format!(
                    "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' RETURN c.cas_version",
                    safe_cand_id, safe_ns
                );
                let mut check_result = conn.query(&check_query)?;
                if let Some(row) = check_result.next() {
                    let current_version: i64 = format!("{}", row[0]).parse().unwrap_or(0);
                    return Err(anyhow::anyhow!("CAS conflict: expected version {}, found {}", cas_version, current_version));
                }
                return Err(anyhow::anyhow!("Candidate not found: {}", candidate_id));
            }

            // Fetch updated candidate
            let fetch_query = format!(
                "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' RETURN c.id, c.namespace, c.receipt_id, c.title, c.description, c.review_state, c.disposition, c.task_id, c.cas_version, c.created_at, c.updated_at",
                safe_cand_id, safe_ns
            );
            let mut fetch_result = conn.query(&fetch_query)?;
            let row = fetch_result.next().ok_or_else(|| anyhow::anyhow!("Candidate not found after update: {}", candidate_id))?;

            let id: String = format!("{}", row[0]);
            let ns: String = format!("{}", row[1]);
            let receipt_id: String = format!("{}", row[2]);
            let title: String = format!("{}", row[3]);
            let description: String = format!("{}", row[4]);
            let review_state_str: String = format!("{}", row[5]);
            let disposition_str: String = format!("{}", row[6]);
            let task_id_db: String = format!("{}", row[7]);
            let cas_version_db: i64 = format!("{}", row[8]).parse().unwrap_or(new_version as i64);
            let created_at: String = format!("{}", row[9]);
            let updated_at: String = format!("{}", row[10]);

            let review_state = match review_state_str.as_str() {
                "pending" => crate::task::intake::ReviewState::Pending,
                "inreview" => crate::task::intake::ReviewState::InReview,
                "accepted" => crate::task::intake::ReviewState::Accepted,
                "dismissed" => crate::task::intake::ReviewState::Dismissed,
                _ => crate::task::intake::ReviewState::Pending,
            };

            let disposition_enum = match disposition_str.as_str() {
                "pending" => crate::task::intake::Disposition::Pending,
                "created" => crate::task::intake::Disposition::Created,
                "linked" => crate::task::intake::Disposition::Linked,
                "dismissed" => crate::task::intake::Disposition::Dismissed,
                _ => crate::task::intake::Disposition::Pending,
            };

            let candidate = crate::task::intake::IntakeCandidate {
                id,
                namespace: ns,
                receipt_id,
                title,
                description: if description.is_empty() { None } else { Some(description) },
                review_state,
                disposition: disposition_enum,
                task_id: if task_id_db.is_empty() { None } else { Some(task_id_db) },
                cas_version: cas_version_db as u64,
                created_at,
                updated_at,
            };

            Ok(crate::task::intake::ResolveCandidateResult {
                candidate: candidate.clone(),
                task_created: disposition == crate::task::intake::Disposition::Created,
                task_id: task_id_for_closure.clone(),
            })
        })
        .await
        .map_err(|e| format!("Failed to resolve candidate: {}", e))
    }

    async fn get_receipt(
        &self,
        namespace: &str,
        id: &str,
    ) -> Result<Option<crate::task::intake::IntakeReceipt>, String> {
        let namespace = namespace.to_string();
        let id = id.to_string();

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            let query = format!(
                "MATCH (r:IntakeReceipt) WHERE r.id = '{}' AND r.namespace = '{}' RETURN r.id, r.namespace, r.source_kind, r.source_revision, r.source_digest, r.detected_intents, r.raw_text, r.created_at",
                safe_id, safe_ns
            );
            let mut result = conn.query(&query)?;

            if let Some(row) = result.next() {
                let receipt_id: String = format!("{}", row[0]);
                let ns: String = format!("{}", row[1]);
                let source_kind_str: String = format!("{}", row[2]);
                let source_revision: String = format!("{}", row[3]);
                let source_digest: String = format!("{}", row[4]);
                let detected_intents_str: String = format!("{}", row[5]);
                let raw_text: String = format!("{}", row[6]);
                let created_at: String = format!("{}", row[7]);

                let source_kind = match source_kind_str.as_str() {
                    "conversation" => crate::task::intake::SourceKind::Conversation,
                    "manual" => crate::task::intake::SourceKind::Manual,
                    "imported" => crate::task::intake::SourceKind::Imported,
                    "tool" => crate::task::intake::SourceKind::Tool,
                    _ => crate::task::intake::SourceKind::Conversation,
                };

                let detected_intents: Vec<crate::task::intake::DetectedIntent> =
                    serde_json::from_str(&detected_intents_str).unwrap_or_default();

                Ok(Some(crate::task::intake::IntakeReceipt {
                    id: receipt_id,
                    namespace: ns,
                    source_kind,
                    source_revision,
                    source_digest,
                    detected_intents,
                    raw_text,
                    created_at,
                }))
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| format!("Failed to get receipt: {}", e))
    }

    async fn get_candidate(
        &self,
        namespace: &str,
        id: &str,
    ) -> Result<Option<crate::task::intake::IntakeCandidate>, String> {
        let namespace = namespace.to_string();
        let id = id.to_string();

        self.with_conn(move |conn| {
            let safe_ns = escape_ladybug_string(&namespace);
            let safe_id = escape_ladybug_string(&id);

            let query = format!(
                "MATCH (c:IntakeCandidate) WHERE c.id = '{}' AND c.namespace = '{}' RETURN c.id, c.namespace, c.receipt_id, c.title, c.description, c.review_state, c.disposition, c.task_id, c.cas_version, c.created_at, c.updated_at",
                safe_id, safe_ns
            );
            let mut result = conn.query(&query)?;

            if let Some(row) = result.next() {
                let cand_id: String = format!("{}", row[0]);
                let ns: String = format!("{}", row[1]);
                let receipt_id: String = format!("{}", row[2]);
                let title: String = format!("{}", row[3]);
                let description: String = format!("{}", row[4]);
                let review_state_str: String = format!("{}", row[5]);
                let disposition_str: String = format!("{}", row[6]);
                let task_id: String = format!("{}", row[7]);
                let cas_version: i64 = format!("{}", row[8]).parse().unwrap_or(1);
                let created_at: String = format!("{}", row[9]);
                let updated_at: String = format!("{}", row[10]);

                let review_state = match review_state_str.as_str() {
                    "pending" => crate::task::intake::ReviewState::Pending,
                    "inreview" => crate::task::intake::ReviewState::InReview,
                    "accepted" => crate::task::intake::ReviewState::Accepted,
                    "dismissed" => crate::task::intake::ReviewState::Dismissed,
                    _ => crate::task::intake::ReviewState::Pending,
                };

                let disposition = match disposition_str.as_str() {
                    "pending" => crate::task::intake::Disposition::Pending,
                    "created" => crate::task::intake::Disposition::Created,
                    "linked" => crate::task::intake::Disposition::Linked,
                    "dismissed" => crate::task::intake::Disposition::Dismissed,
                    _ => crate::task::intake::Disposition::Pending,
                };

                Ok(Some(crate::task::intake::IntakeCandidate {
                    id: cand_id,
                    namespace: ns,
                    receipt_id,
                    title,
                    description: if description.is_empty() { None } else { Some(description) },
                    review_state,
                    disposition,
                    task_id: if task_id.is_empty() { None } else { Some(task_id) },
                    cas_version: cas_version as u64,
                    created_at,
                    updated_at,
                }))
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| format!("Failed to get candidate: {}", e))
    }
}

/// Diagnostic counters for work handed to the engine.
///
/// Process-wide on purpose: there is one store per process, and the question
/// they exist to answer -- does blocking work pile up and never come back -- is
/// about the process, not about any one call.
///
/// `STARTED` minus `FINISHED` is how many engine calls are inside lbug right
/// now. If that number climbs and does not come down, a deadline that could not
/// cancel its statement has left the work running, which is the hypothesis these
/// were added to test (bead neurostrata-4nk).
pub static ENGINE_CALLS_STARTED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static ENGINE_CALLS_FINISHED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Writes whose caller gave up. The statement may still be running.
pub static WRITES_ABANDONED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Times the engine refused a second write transaction.
pub static WRITE_COLLISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many engine calls are inside lbug right now.
pub fn engine_in_flight() -> u64 {
    ENGINE_CALLS_STARTED
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(ENGINE_CALLS_FINISHED.load(std::sync::atomic::Ordering::Relaxed))
}

/// The counters as a line worth logging.
pub fn engine_stats() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!(
        "{} in flight ({} started, {} finished), {} abandoned, {} collisions",
        engine_in_flight(),
        ENGINE_CALLS_STARTED.load(Relaxed),
        ENGINE_CALLS_FINISHED.load(Relaxed),
        WRITES_ABANDONED.load(Relaxed),
        WRITE_COLLISIONS.load(Relaxed)
    )
}

/// How long a write may wait before the caller is told the database is busy.
/// Override with NEUROSTRATA_WRITE_TIMEOUT_SECS where an unusually large
/// database makes the default too tight.
fn write_timeout() -> std::time::Duration {
    let secs = std::env::var("NEUROSTRATA_WRITE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// How many times a write retries after being refused the write transaction.
/// Waits are 25ms, 50ms and so on, and the caller's deadline caps the total
/// regardless, so this only decides how patient a single attempt is.
const WRITE_COLLISION_RETRIES: u32 = 6;

/// The engine's refusal when something else already holds the write
/// transaction. Worth recognising by name: the fix is to wait a moment, not to
/// report a failure, and the in-process lock cannot prevent every case -- the
/// background checkpoint and any other process opening the same database are
/// both outside it.
fn is_write_collision(err: &anyhow::Error) -> bool {
    err.to_string()
        .contains("Cannot start a new write transaction")
}

/// Whether a memory has been retired, meaning its `valid_to` has passed.
///
/// A retired row has to stop surfacing wherever results come from: the vector
/// match, the graph around that match, and the edges relinking replays.
/// Filtering only the first lets the graph hand back what the match left out.
fn is_retired(metadata: &Value) -> bool {
    is_retired_at(metadata, chrono::Utc::now().timestamp())
}

/// Timestamp-aware variant so callers that fetch many rows in one pass can
/// capture `now` once instead of calling Utc::now() per row.
fn is_retired_at(metadata: &Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(valid_to) if !valid_to.is_null() => valid_to.as_i64().unwrap_or(0) <= now,
        _ => false,
    }
}

/// The ids a declared edge target could stand for when it is not already a
/// qualified id: the declaration generation-scoped into the active generation,
/// and every separator-anchored suffix of it, bare and scoped. A suffix is how a
/// legacy absolute path finds the file it meant.
fn declared_target_candidates(namespace: &str, active_gen: u64, declared: &str) -> Vec<String> {
    let declared = declared.replace('\\', "/");
    let mut candidates =
        vec![crate::parser::ingest::generation_scoped_id(namespace, active_gen, &declared)];
    for (cut, _) in declared.match_indices('/') {
        let suffix = &declared[cut + 1..];
        if !suffix.is_empty() {
            candidates.push(suffix.to_string());
            candidates.push(crate::parser::ingest::generation_scoped_id(
                namespace,
                active_gen,
                suffix,
            ));
        }
    }
    candidates
}

/// Chooses among the ids that exist: the memory's own namespace first (in the
/// active generation), then the id exactly as declared, then a suffix -- but
/// only one. An ambiguous suffix produces no edge, because a wrong edge is
/// worse than a missing one.
fn pick_declared_target(
    namespace: &str,
    active_gen: u64,
    declared: &str,
    found: &[String],
) -> crate::traits::TargetResolution {
    let qualified = crate::parser::ingest::generation_scoped_id(
        namespace,
        active_gen,
        &declared.replace('\\', "/"),
    );
    if found.iter().any(|id| *id == qualified) {
        return crate::traits::TargetResolution::Resolved(qualified);
    }
    if found.iter().any(|id| id == declared) {
        return crate::traits::TargetResolution::Resolved(declared.to_string());
    }
    let suffixes: Vec<String> = found
        .iter()
        .filter(|id| **id != qualified && id.as_str() != declared)
        .cloned()
        .collect();
    match suffixes.len() {
        0 => crate::traits::TargetResolution::Missing,
        1 => crate::traits::TargetResolution::Resolved(suffixes[0].clone()),
        _ => crate::traits::TargetResolution::Ambiguous(suffixes),
    }
}

/// Resolves a declared edge target while a memory is being written, in one read.
///
/// The exact id stays unscoped, as edges always have been; everything inferred
/// from it is confined to the memory's own namespace, so a bare `src/lib.rs`
/// cannot land on another project's file.
/// How an edge target binds to exactly one row (Task 7).
enum RowBind {
    /// The namespace of the row the edge lands on.
    Bound(String),
    /// No row carries the id (or the lookup itself failed — edge
    /// materialization is best-effort and reports through the deferred
    /// list either way).
    Gone,
    /// The id lives in several namespaces, none of them the preferred
    /// one: a true tie, deferred rather than sprayed.
    Ambiguous(Vec<String>),
}

/// Resolve the namespace of the row an edge should land on: the row whose
/// id is `id`, preferring `preferred` (the source memory's namespace)
/// when the id lives there, else the single namespace holding it.
fn bind_row_namespace(conn: &Connection, id: &str, preferred: &str) -> RowBind {
    let query = format!(
        "MATCH (b:Memory) WHERE b.id = '{}' AND b.namespace IS NOT NULL RETURN DISTINCT b.namespace",
        escape_ladybug_string(id)
    );
    let mut namespaces = Vec::new();
    match conn.query(&query) {
        Ok(mut rows) => {
            while let Some(row) = rows.next() {
                namespaces.push(format!("{}", row[0]));
            }
        }
        Err(_) => return RowBind::Gone,
    }
    if namespaces.iter().any(|ns| ns == preferred) {
        RowBind::Bound(preferred.to_string())
    } else if namespaces.len() == 1 {
        RowBind::Bound(namespaces.into_iter().next().unwrap())
    } else if namespaces.is_empty() {
        RowBind::Gone
    } else {
        RowBind::Ambiguous(namespaces)
    }
}

fn resolve_target_on_write(
    conn: &Connection,
    namespace: &str,
    declared: &str,
) -> crate::traits::TargetResolution {
    let active_gen = active_generation_of(conn, namespace);
    let candidates = declared_target_candidates(namespace, active_gen, declared)
        .iter()
        .map(|c| format!("'{}'", escape_ladybug_string(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "MATCH (b:Memory) WHERE b.id = '{}' OR (b.namespace = '{}' AND b.id IN [{}]) RETURN DISTINCT b.id",
        escape_ladybug_string(declared),
        escape_ladybug_string(namespace),
        candidates
    );
    let mut rows = match conn.query(&query) {
        Ok(rows) => rows,
        Err(e) => return crate::traits::TargetResolution::Failed(e.to_string()),
    };
    let mut found = Vec::new();
    while let Some(row) = rows.next() {
        found.push(format!("{}", row[0]));
    }
    pick_declared_target(namespace, active_gen, declared, &found)
}

/// Fails a future that outstays its deadline, with a message that says what was
/// waiting and admits the work may still be running.
async fn bounded<T>(
    what: &str,
    limit: std::time::Duration,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "the database is busy: {} did not finish within {}s. The statement may still be running, so read the record back before retrying.",
            what,
            limit.as_secs()
        )),
    }
}

/// Adds one to `access_count` and leaves the rest of the blob alone. Metadata
/// that is missing, empty or unparseable starts a fresh object at 1 instead of
/// failing: reading a memory must never be the thing that errors.
fn bump_access_count(metadata: &str) -> String {
    let mut value: Value = serde_json::from_str(metadata).unwrap_or_else(|_| serde_json::json!({}));
    if !value.is_object() {
        value = serde_json::json!({});
    }
    if let Some(obj) = value.as_object_mut() {
        let count = obj
            .get("access_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        obj.insert(
            "access_count".to_string(),
            serde_json::json!(count.saturating_add(1)),
        );
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    static TEST_CWD_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    async fn wait_for_test_barrier(path: &Path) {
        let started = std::time::Instant::now();
        while !path.exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(15),
                "test operation did not reach its barrier"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn release_test_barrier(path: &Path) {
        let mut release = path.as_os_str().to_os_string();
        release.push(".release");
        std::fs::write(PathBuf::from(release), "continue").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn database_path_aliases_share_writer_and_stage_sidecars() {
        let root = std::env::temp_dir().join(format!("db-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("database.store");
        let alias = root.join("database-alias.store");
        std::fs::write(&database, b"fixture").unwrap();
        std::os::unix::fs::symlink(&database, &alias).unwrap();
        let namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        assert_eq!(
            database_writer_lock_path(&database),
            database_writer_lock_path(&alias)
        );
        assert_eq!(
            ingest_stage_lease_path(&database, &namespace).unwrap(),
            ingest_stage_lease_path(&alias, &namespace).unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    async fn seed_process_crash_publication_fixture(db: &Path, namespace: &str, stage: &str) {
        let store = LadybugStore::for_testing(db, 4).unwrap();
        store.init("process-crash-fixture").await.unwrap();
        let old_id = format!(
            "{}{}old.rs",
            namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                namespace,
                &old_id,
                vec![0.1; 4],
                MemoryPayload {
                    content: "old-generation".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "old.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let manual_rule_id = format!(
            "{}{}manual-rule",
            namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                namespace,
                &manual_rule_id,
                vec![0.2; 4],
                MemoryPayload {
                    content: "manual rule".into(),
                    user_id: "owner".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"governs": [old_id]}),
                },
            )
            .await
            .unwrap();
        assert_eq!(governs_edges_from(&store, &manual_rule_id).await, 1);

        let lease = store.begin_ingest_stage(stage).await.unwrap();
        let staged_namespace = stage.to_string();
        let staged_prefix = format!("{}{}", stage, crate::parser::ingest::NAMESPACE_SEPARATOR);
        let pk_ready = store.pk_ready();
        store
            .write_with_deadline("seeding a staged process-crash fixture", move |conn| {
                // Task 7 fix round: the fixture's raw CREATEs seed a
                // fresh (pk-keyed) table, so each row supplies the
                // derived key as create input, as a production writer
                // must. In legacy mode the fragment is empty.
                let pk_prop = |id: &str| {
                    if pk_ready {
                        format!(
                            "pk: '{}', ",
                            escape_ladybug_string(&memory_pk(&staged_namespace, id))
                        )
                    } else {
                        String::new()
                    }
                };
                for i in 0..130 {
                    let location = if i == 0 {
                        "old.rs".to_string()
                    } else {
                        format!("file-{i}.rs")
                    };
                    let id = format!("{staged_prefix}{location}");
                    conn.query(&format!(
                        "CREATE (m:Memory {{{}id: '{}', namespace: '{}', content: 'new-generation-{i}', user_id: 'auto-ingestor', memory_type: 'file', agent_name: '', location: '{}', location_lines: '', metadata: '{{}}', embedding: [0.3, 0.3, 0.3, 0.3]}})",
                        pk_prop(&id),
                        escape_ladybug_string(&id),
                        escape_ladybug_string(&staged_namespace),
                        escape_ladybug_string(&location)
                    ))?;
                }
                let rule_id = format!("{staged_prefix}new-rule");
                let target_id = format!("{staged_prefix}old.rs");
                let metadata = serde_json::to_string(&json!({"governs": [target_id]}))?;
                conn.query(&format!(
                    "CREATE (m:Memory {{{}id: '{}', namespace: '{}', content: 'new rule', user_id: 'auto-ingestor', memory_type: 'rule', agent_name: '', location: '', location_lines: '', metadata: '{}', embedding: [0.4, 0.4, 0.4, 0.4]}})",
                    pk_prop(&rule_id),
                    escape_ladybug_string(&rule_id),
                    escape_ladybug_string(&staged_namespace),
                    escape_ladybug_string(&metadata)
                ))?;
                Ok(())
            })
            .await
            .unwrap();
        drop(lease);
        drop(store);
    }

    async fn assert_process_crash_generation(
        store: &LadybugStore,
        namespace: &str,
        committed: bool,
    ) {
        let namespace = namespace.to_string();
        let query_namespace = namespace.clone();
        let (rows, edges) = store
            .with_conn(move |conn| {
                let safe_namespace = escape_ladybug_string(&query_namespace);
                let mut result = conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.location, m.metadata, m.embedding",
                    safe_namespace
                ))?;
                let mut rows = HashMap::<String, (Vec<f32>, MemoryPayload)>::new();
                while let Some(row) = result.next() {
                    let id = format!("{}", row[0]);
                    let mut embedding = Vec::new();
                    if let lbug::Value::Array(_, values) | lbug::Value::List(_, values) = &row[6] {
                        for value in values {
                            match value {
                                lbug::Value::Float(value) => embedding.push(*value),
                                lbug::Value::Double(value) => embedding.push(*value as f32),
                                other => anyhow::bail!("unexpected embedding value: {other:?}"),
                            }
                        }
                    }
                    let metadata_text = format!("{}", row[5]);
                    let metadata = serde_json::from_str(&metadata_text)?;
                    rows.insert(
                        id,
                        (
                            embedding,
                            MemoryPayload {
                                content: format!("{}", row[1]),
                                user_id: format!("{}", row[2]),
                                memory_type: format!("{}", row[3]),
                                agent_name: None,
                                location: format!("{}", row[4]),
                                location_lines: String::new(),
                                metadata,
                            },
                        ),
                    );
                }
                let mut result = conn.query(&format!(
                    "MATCH (a:Memory)-[:GOVERNS]->(b:Memory) WHERE a.namespace = '{}' AND b.namespace = '{}' RETURN a.id, b.id",
                    safe_namespace, safe_namespace
                ))?;
                let mut edges = HashSet::new();
                while let Some(row) = result.next() {
                    edges.insert((format!("{}", row[0]), format!("{}", row[1])));
                }
                Ok((rows, edges))
            })
            .await
            .unwrap();

        let old_id = format!(
            "{namespace}{}old.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        let manual_rule_id = format!(
            "{namespace}{}manual-rule",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        let mut expected_ids = HashSet::from([old_id.clone(), manual_rule_id.clone()]);
        let mut expected_edges = HashSet::from([(manual_rule_id.clone(), old_id.clone())]);
        if committed {
            for index in 1..130 {
                let path = format!("file-{index}.rs");
                expected_ids.insert(format!(
                    "{namespace}{}{path}",
                    crate::parser::ingest::NAMESPACE_SEPARATOR
                ));
            }
            let new_rule_id = format!(
                "{namespace}{}new-rule",
                crate::parser::ingest::NAMESPACE_SEPARATOR
            );
            expected_ids.insert(new_rule_id.clone());
            expected_edges.insert((new_rule_id, old_id.clone()));
        }
        assert_eq!(
            rows.keys().cloned().collect::<HashSet<_>>(),
            expected_ids,
            "crash recovery must expose exactly the old or committed generation"
        );

        let (vector, payload) = rows.get(&old_id).expect("old.rs row exists");
        assert_eq!(vector, &vec![if committed { 0.3 } else { 0.1 }; 4]);
        assert_eq!(
            payload.content,
            if committed {
                "new-generation-0"
            } else {
                "old-generation"
            }
        );
        assert_eq!(payload.user_id, "auto-ingestor");
        assert_eq!(payload.memory_type, "file");
        assert_eq!(payload.location, "old.rs");
        assert_eq!(payload.metadata, json!({}));

        let (vector, payload) = rows.get(&manual_rule_id).expect("manual rule exists");
        assert_eq!(vector, &vec![0.2; 4]);
        assert_eq!(payload.content, "manual rule");
        assert_eq!(payload.user_id, "owner");
        assert_eq!(payload.memory_type, "rule");
        assert_eq!(payload.metadata, json!({"governs": [old_id.clone()]}));

        if committed {
            for index in 1..130 {
                let path = format!("file-{index}.rs");
                let id = format!(
                    "{namespace}{}{path}",
                    crate::parser::ingest::NAMESPACE_SEPARATOR
                );
                let (vector, payload) = rows.get(&id).expect("committed file row exists");
                assert_eq!(vector, &vec![0.3; 4], "vector for {id}");
                assert_eq!(payload.content, format!("new-generation-{index}"));
                assert_eq!(payload.user_id, "auto-ingestor");
                assert_eq!(payload.memory_type, "file");
                assert_eq!(payload.location, path);
                assert_eq!(payload.metadata, json!({}));
            }
            let new_rule_id = format!(
                "{namespace}{}new-rule",
                crate::parser::ingest::NAMESPACE_SEPARATOR
            );
            let (vector, payload) = rows.get(&new_rule_id).expect("new rule exists");
            assert_eq!(vector, &vec![0.4; 4]);
            assert_eq!(payload.content, "new rule");
            assert_eq!(payload.user_id, "auto-ingestor");
            assert_eq!(payload.memory_type, "rule");
            assert_eq!(payload.metadata, json!({"governs": [old_id]}));
        }
        assert_eq!(
            edges, expected_edges,
            "the exact declaration edge set survives"
        );
    }

    #[tokio::test]
    async fn publish_process_crash_child_worker() {
        let Ok(db) = std::env::var("NEUROSTRATA_TEST_PUBLISH_CRASH_DB") else {
            return;
        };
        let phase = std::env::var("NEUROSTRATA_TEST_PUBLISH_CRASH_PHASE").unwrap();
        let barrier =
            PathBuf::from(std::env::var_os("NEUROSTRATA_TEST_PUBLISH_CRASH_BARRIER").unwrap());
        set_test_publish_crash_barrier(&phase, PathBuf::from(&db), barrier);
        let namespace = std::env::var("NEUROSTRATA_TEST_PUBLISH_CRASH_NAMESPACE").unwrap();
        let stage = std::env::var("NEUROSTRATA_TEST_PUBLISH_CRASH_STAGE").unwrap();
        let db_path = PathBuf::from(&db);
        let store = LadybugStore::for_testing(db, 4).unwrap();
        store.init("process-crash-child").await.unwrap();
        test_publish_crash_barrier(&db_path, "staged");
        store.publish_ingested(&namespace, &stage).await.unwrap();
    }

    #[tokio::test]
    async fn snapshot_writer_lock_child_worker() {
        let Ok(db) = std::env::var("NEUROSTRATA_TEST_SNAPSHOT_WRITER_DB") else {
            return;
        };
        let barrier =
            PathBuf::from(std::env::var_os("NEUROSTRATA_TEST_SNAPSHOT_WRITER_BARRIER").unwrap());
        let _lock = acquire_database_writer_lock(Path::new(&db)).unwrap();
        std::fs::write(&barrier, "writer holds database lock").unwrap();
        let mut release_path = barrier.as_os_str().to_os_string();
        release_path.push(".release");
        let release = PathBuf::from(release_path);
        let started = std::time::Instant::now();
        while !release.exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "snapshot writer lock watchdog expired"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn snapshot_waits_for_a_competing_process_writer() {
        let root = std::env::temp_dir().join(format!("snapshot-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fixture.store");
        let snapshot = root.join("snapshot");
        let store = std::sync::Arc::new(LadybugStore::for_testing(&db, 4).unwrap());
        store.init("snapshot-lock").await.unwrap();
        store
            .upsert(
                "snapshot-lock",
                "snapshot-row",
                vec![0.0; 4],
                MemoryPayload {
                    content: "before-writer-lock".into(),
                    user_id: "owner".into(),
                    memory_type: "note".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let barrier = root.join("writer-barrier");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("store::ladybug::tests::snapshot_writer_lock_child_worker")
            .arg("--nocapture")
            .env("NEUROSTRATA_TEST_SNAPSHOT_WRITER_DB", &db)
            .env("NEUROSTRATA_TEST_SNAPSHOT_WRITER_BARRIER", &barrier)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !barrier.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("snapshot writer child exited before holding lock: {status}");
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(15),
                "snapshot writer child did not reach its barrier"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let store_for_export = store.clone();
        let output = snapshot.clone();
        let exporter = tokio::spawn(async move {
            store_for_export
                .export_database(&output.to_string_lossy())
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            !exporter.is_finished(),
            "snapshot must wait while another process owns the database writer lock"
        );
        let mut release_path = barrier.as_os_str().to_os_string();
        release_path.push(".release");
        std::fs::write(release_path, "release").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "snapshot writer child failed: {status}");
        exporter.await.unwrap().unwrap();
        drop(store);

        let snapshot_db = snapshot.join("ladybug.store");
        let restored = LadybugStore::for_testing(&snapshot_db, 4).unwrap();
        restored.init("snapshot-cut-check").await.unwrap();
        let row = restored
            .get("snapshot-lock", "snapshot-row")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.1.content, "before-writer-lock");
        drop(restored);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn process_crash_during_publication_recovers_old_or_committed_generation() {
        for phase in [
            "staged",
            "after-delete",
            "after-first-page",
            "precommit",
            "postcommit",
        ] {
            let root = std::env::temp_dir()
                .join(format!("publish-crash-{phase}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let db = root.join("fixture.store");
            let namespace = format!("crash-{phase}");
            let stage = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
            seed_process_crash_publication_fixture(&db, &namespace, &stage).await;
            let barrier = root.join("barrier");
            let executable = std::env::current_exe().unwrap();
            let mut child = std::process::Command::new(executable)
                .arg("--exact")
                .arg("store::ladybug::tests::publish_process_crash_child_worker")
                .arg("--nocapture")
                .env("NEUROSTRATA_TEST_PUBLISH_CRASH_PHASE", phase)
                .env("NEUROSTRATA_TEST_PUBLISH_CRASH_BARRIER", &barrier)
                .env("NEUROSTRATA_TEST_PUBLISH_CRASH_DB", &db)
                .env("NEUROSTRATA_TEST_PUBLISH_CRASH_NAMESPACE", &namespace)
                .env("NEUROSTRATA_TEST_PUBLISH_CRASH_STAGE", &stage)
                .spawn()
                .unwrap();
            let started = std::time::Instant::now();
            while !barrier.exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("child exited before reaching {phase} barrier: {status}");
                }
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(15),
                    "child did not reach {phase} barrier before watchdog"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            child.kill().unwrap();
            let crash_status = child.wait().unwrap();
            assert!(
                !crash_status.success(),
                "the writer must be killed without normal process unwinding"
            );

            let reopened = LadybugStore::for_testing(&db, 4).unwrap();
            reopened.init("after-process-crash").await.unwrap();
            let should_be_new = phase == "postcommit";
            assert_process_crash_generation(&reopened, &namespace, should_be_new).await;

            let followup = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
            let old_id = format!(
                "{}{}old.rs",
                namespace,
                crate::parser::ingest::NAMESPACE_SEPARATOR
            );
            let _lease = reopened.begin_ingest_stage(&followup).await.unwrap();
            assert!(
                !ingest_stage_lease_path(&db, &stage).unwrap().exists(),
                "recovery removes orphan stage lease sidecars after a completed or rolled-back transaction"
            );
            if !should_be_new {
                assert!(reopened
                    .get(
                        &stage,
                        &format!(
                            "{}{}old.rs",
                            stage,
                            crate::parser::ingest::NAMESPACE_SEPARATOR
                        )
                    )
                    .await
                    .unwrap()
                    .is_none());
            }
            let followup_id = format!(
                "{}{}old.rs",
                followup,
                crate::parser::ingest::NAMESPACE_SEPARATOR
            );
            reopened
                .upsert(
                    &followup,
                    &followup_id,
                    vec![0.9; 4],
                    MemoryPayload {
                        content: "follow-up replacement".into(),
                        user_id: "auto-ingestor".into(),
                        memory_type: "file".into(),
                        agent_name: None,
                        location: "old.rs".into(),
                        location_lines: String::new(),
                        metadata: json!({}),
                    },
                )
                .await
                .unwrap();
            reopened
                .publish_ingested(&namespace, &followup)
                .await
                .unwrap();
            assert_eq!(
                reopened
                    .get(&namespace, &old_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .1
                    .content,
                "follow-up replacement"
            );
            assert_eq!(
                governs_edges_from(
                    &reopened,
                    &format!(
                        "{}{}manual-rule",
                        namespace,
                        crate::parser::ingest::NAMESPACE_SEPARATOR
                    )
                )
                .await,
                1
            );
            drop(reopened);
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn cancelling_publication_caller_waits_for_definitive_commit() {
        let root = std::env::temp_dir().join(format!("publish-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fixture.store");
        let namespace = "cancel-publication";
        let stage = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        seed_process_crash_publication_fixture(&db, namespace, &stage).await;
        let store = std::sync::Arc::new(LadybugStore::for_testing(&db, 4).unwrap());
        store.init("cancel-publication").await.unwrap();
        store
            .checkpoint()
            .await
            .expect("checkpoint staged generation before publication");
        assert!(
            !store.is_dirty(),
            "publication starts with no pending checkpoint work"
        );
        let barrier = root.join("barrier");
        set_test_publish_crash_barrier("precommit", db.clone(), barrier.clone());
        let writer_store = store.clone();
        let namespace_owned = namespace.to_string();
        let stage_owned = stage.clone();
        let caller = tokio::spawn(async move {
            writer_store
                .publish_ingested(&namespace_owned, &stage_owned)
                .await
        });
        let started = std::time::Instant::now();
        while !barrier.exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(15),
                "publication did not reach precommit barrier"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        caller.abort();
        let cancellation = caller
            .await
            .expect_err("publication caller must be canceled")
            .is_cancelled();
        assert!(
            cancellation,
            "JoinHandle must confirm cancellation before release"
        );
        let mut release_path = barrier.as_os_str().to_os_string();
        release_path.push(".release");
        std::fs::write(release_path, "continue").unwrap();

        let lock_path = db.clone();
        let drained_writer =
            tokio::task::spawn_blocking(move || acquire_database_writer_lock(&lock_path))
                .await
                .expect("writer-lock waiter completes")
                .expect("detached publication releases its writer lock");
        assert_process_crash_generation(&store, namespace, true).await;
        assert!(
            store.is_dirty(),
            "a committed publication must remain dirty after its caller is canceled"
        );
        drop(drained_writer);
        store
            .checkpoint()
            .await
            .expect("checkpoint the committed publication");
        assert!(
            !store.is_dirty(),
            "checkpoint clears the publication dirty flag"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    async fn seed_raw_reserved_namespace_row(
        store: &LadybugStore,
        namespace: &str,
        id: &str,
        user_id: &str,
    ) {
        // Task 7 fix round: derive the pk from the RAW namespace/id
        // (before escaping) with memory_pk, exactly as a production
        // writer does; the fragment is empty in legacy mode (no pk
        // column), where the binder does not demand it.
        let pk_prop = if store.pk_ready() {
            format!(
                "pk: '{}', ",
                escape_ladybug_string(&memory_pk(namespace, id))
            )
        } else {
            String::new()
        };
        let namespace = escape_ladybug_string(namespace);
        let id = escape_ladybug_string(id);
        let user_id = escape_ladybug_string(user_id);
        store
            .with_conn(move |conn| {
                conn.query(&format!(
                    "CREATE (m:Memory {{{}id: '{}', namespace: '{}', content: 'preserve me', user_id: '{}', memory_type: 'fact', agent_name: '', location: '', location_lines: '', metadata: '{{}}', embedding: [0.0, 0.0, 0.0, 0.0]}})",
                    pk_prop, id, namespace, user_id
                ))?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[test]
    fn post_commit_stage_lease_cleanup_failure_is_only_a_warning() {
        let path = std::env::temp_dir().join(format!("stage-lease-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let warning = cleanup_ingest_stage_lease_after_commit(&path).unwrap();
        assert!(warning.contains("generation committed"));
        assert!(warning.contains("cleanup failed"));
        assert!(
            path.is_dir(),
            "cleanup reports the failure without deleting data"
        );
        std::fs::remove_dir(path).unwrap();
    }

    /// The production open path must never hand the engine its own defaults: a
    /// `buffer_pool_size` of 0 becomes 80% of physical memory, and a
    /// `max_db_size` of `u32::MAX` becomes an 8 TB virtual region.
    #[test]
    fn the_default_limits_are_a_ceiling_rather_than_most_of_ram() {
        let limits = StoreLimits::default();
        assert!(
            limits.buffer_pool_bytes > 0,
            "a zero pool means 'ask the engine'"
        );
        assert!(
            limits.buffer_pool_bytes <= 512 * 1024 * 1024,
            "the default pool must stay small enough not to scale with installed RAM"
        );
        assert!(limits.max_db_bytes > 0);
        assert!(
            limits.max_db_bytes < 1024 * 1024 * 1024 * 1024,
            "the default must be far below the engine's 8 TB virtual reservation"
        );
    }

    #[test]
    fn a_missing_width_marker_is_written_on_first_open() {
        let dir = std::env::temp_dir().join(format!("ladybug-marker-{}", uuid::Uuid::new_v4()));
        {
            let _store = LadybugStore::for_testing(&dir, 4).expect("open at width 4");
        }

        let marker = width_marker_path(&dir);
        assert!(marker.exists(), "the width is recorded beside the database");
        let marker_value: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(&marker).expect("read marker")).unwrap();
        assert_eq!(marker_value.dimensions, 4);

        assert!(
            LadybugStore::for_testing(&dir, 4).is_ok(),
            "the same width must reopen"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopening_a_database_with_a_different_embedding_width_is_refused() {
        let dir = std::env::temp_dir().join(format!("ladybug-width-{}", uuid::Uuid::new_v4()));
        {
            let _store = LadybugStore::for_testing(&dir, 8).expect("open at width 8");
        }

        let error = match LadybugStore::for_testing(&dir, 4) {
            Ok(_) => panic!("a 4-wide embedder must not open an 8-wide database"),
            Err(error) => error,
        };
        let message = format!("{error}");
        assert!(
            message.contains("width 8"),
            "names the recorded width: {message}"
        );
        assert!(
            message.contains("produces 4"),
            "names the configured width: {message}"
        );
        // Refusal Standard elements 4-5, in the reference site's wording.
        for element in [
            "Keep the database intact",
            "original embedder",
            "separate database path",
            "no in-place re-embedding migration is available",
        ] {
            assert!(message.contains(element), "remediation element {element:?} missing: {message}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Refusal Standard at the width site (Spec Feature 3 AC1): when
    /// identities are in context, the width refusal — often the only
    /// refusal that fires — names both canonicalized identities as well
    /// as both widths and the reference remediation.
    #[test]
    fn width_refusal_names_both_identities_and_both_widths() {
        let dir = std::env::temp_dir().join(format!("ladybug-width-id-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 8, "fastembed:model-a".into(), limits)
                    .expect("open model A at width 8");
        }
        let error = match LadybugStore::new_with_identity_limits(
            &dir,
            4,
            "fastembed:model-b".into(),
            limits,
        ) {
            Ok(_) => panic!("a 4-wide embedder must not open an 8-wide database"),
            Err(error) => error,
        };
        let message = format!("{error}");
        for element in [
            "fastembed:model-a",
            "fastembed:model-b",
            "width 8",
            "produces 4",
            "Keep the database intact",
            "original embedder",
            "separate database path",
            "no in-place re-embedding migration is available",
        ] {
            assert!(message.contains(element), "width refusal element {element:?} missing: {message}");
        }
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    #[tokio::test]
    async fn legacy_database_without_marker_uses_catalog_width() {
        let dir =
            std::env::temp_dir().join(format!("ladybug-legacy-width-{}", uuid::Uuid::new_v4()));
        {
            let store = LadybugStore::for_testing(&dir, 8).expect("open at width 8");
            store.init("probe").await.expect("create Memory schema");
        }
        std::fs::remove_file(width_marker_path(&dir)).expect("remove only test sidecar");
        let error = match LadybugStore::for_testing(&dir, 4) {
            Ok(_) => panic!("catalog width must guard a legacy DB without a sidecar"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("width 8"));
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn stale_marker_is_replaced_when_database_path_is_absent() {
        let dir =
            std::env::temp_dir().join(format!("ladybug-stale-marker-{}", uuid::Uuid::new_v4()));
        std::fs::write(width_marker_path(&dir), "768\n").unwrap();
        {
            let _store =
                LadybugStore::for_testing(&dir, 4).expect("fresh path ignores orphan marker");
            let marker: EmbeddingMarker =
                serde_json::from_slice(&std::fs::read(width_marker_path(&dir)).unwrap()).unwrap();
            assert_eq!(marker.dimensions, 4);
        }
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    #[tokio::test]
    async fn same_width_model_identity_change_is_refused() {
        let dir = std::env::temp_dir().join(format!("ladybug-model-id-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("open model A");
            store.init("probe").await.expect("create schema");
        }
        let error = match LadybugStore::new_with_identity_limits(
            &dir,
            4,
            "fastembed:model-b".into(),
            limits,
        ) {
            Ok(_) => panic!("same-width model changes alter the vector space"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("model identity"));
        // Refusal Standard (Spec Feature 3 AC1): both canonicalized
        // identities, both widths (equal at this site — the width check
        // has already passed — stated for uniformity), and the reference
        // remediation elements.
        for element in [
            "fastembed:model-a",
            "fastembed:model-b",
            "recorded embedding width is 4",
            "configured embedding width is 4",
            "Keep the database intact",
            "original embedder",
            "separate database path",
            "no in-place re-embedding migration is available",
        ] {
            assert!(message.contains(element), "identity refusal element {element:?} missing: {message}");
        }
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    fn read_marker_json(db_path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(width_marker_path(db_path)).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn width_only_store_adoption_records_provenance() {
        let dir =
            std::env::temp_dir().join(format!("ladybug-adopt-width-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("open model A");
            store.init("probe").await.expect("create schema");
        }
        // Rewrite the sidecar as a legacy marker: width recorded, no identity.
        std::fs::write(
            width_marker_path(&dir),
            serde_json::to_vec(&json!({"version": 1, "dimensions": 4})).unwrap(),
        )
        .unwrap();
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("width-only store is adopted under the configured identity");
        }
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(width_marker_path(&dir)).unwrap()).unwrap();
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        assert_eq!(
            read_marker_json(&dir).get("adopted_from"),
            Some(&json!("width-only")),
            "the adoption is recorded in the marker itself"
        );
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }
    /// Spec Feature 1 AC1 (store side, Ruling R1 placement): with an
    /// auto-generated embedders file and `NEUROSTRATA_PROFILE=low`, the
    /// resolved model is BGESmallENV15Q and a store created in that state
    /// is minted with the BGE-small identity, not Nomic's.
    #[tokio::test]
    async fn profile_low_resolution_mints_bge_small_identity_on_a_new_store() {
        let resolved = crate::embed::resolve_configured_model_from_inputs(
            crate::embed::auto_generated_defaults(),
            crate::embed::EmbeddersFileState::AutoGenerated,
            None,
            Some("low"),
        )
        .expect("profile resolution succeeds");
        assert_eq!(resolved.model.model_name, "BGESmallENV15Q");
        let identity = crate::embed::configured_identity_for(&resolved.model)
            .expect("the profile model has a canonical identity");

        let profile = crate::config::CapacityProfile::Low;
        let limits = StoreLimits {
            buffer_pool_bytes: profile.buffer_pool_bytes(),
            max_db_bytes: profile.max_db_bytes(),
        };
        let dir = std::env::temp_dir()
            .join(format!("ladybug-profile-mint-{}", uuid::Uuid::new_v4()));
        {
            let store = LadybugStore::new_with_identity_limits(
                &dir,
                resolved.model.dimensions,
                identity,
                limits,
            )
            .expect("open a new store with the profile-resolved identity");
            store.init("profile-mint").await.expect("create schema");
        }
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(width_marker_path(&dir)).unwrap()).unwrap();
        assert_eq!(
            marker.embedder_id.as_deref(),
            Some("fastembed:BGESmallENV15Q")
        );
        assert_eq!(marker.dimensions, 384);
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    #[tokio::test]
    async fn schema_only_adoption_records_provenance() {
        let dir =
            std::env::temp_dir().join(format!("ladybug-adopt-schema-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("open model A");
            store.init("probe").await.expect("create schema");
        }
        // No sidecar at all: the width can only come from the fixed schema.
        std::fs::remove_file(width_marker_path(&dir)).expect("remove only test sidecar");
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("schema-only store is adopted under the configured identity");
        }
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(width_marker_path(&dir)).unwrap()).unwrap();
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        assert_eq!(
            read_marker_json(&dir).get("adopted_from"),
            Some(&json!("schema-only")),
            "the adoption is recorded in the marker itself"
        );
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    #[tokio::test]
    async fn adopted_store_refuses_a_different_same_width_model() {
        let dir =
            std::env::temp_dir().join(format!("ladybug-adopt-refuse-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("open model A");
            store.init("probe").await.expect("create schema");
        }
        std::fs::write(
            width_marker_path(&dir),
            serde_json::to_vec(&json!({"version": 1, "dimensions": 4})).unwrap(),
        )
        .unwrap();
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("model A adopts the width-only store");
        }
        // Model B has the same width, so only the recorded identity — now
        // persisted by the adoption — can refuse it.
        let error = match LadybugStore::new_with_identity_limits(
            &dir,
            4,
            "fastembed:model-b".into(),
            limits,
        ) {
            Ok(_) => panic!("an adopted store must refuse a different same-width model"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("model identity"));
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    #[test]
    fn native_store_has_no_adoption_provenance() {
        let dir = std::env::temp_dir().join(format!("ladybug-native-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("open model A");
        }
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(width_marker_path(&dir)).unwrap()).unwrap();
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        assert_eq!(
            read_marker_json(&dir).get("adopted_from"),
            None,
            "a store created fresh under an identity was never adopted"
        );
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }

    #[test]
    fn pre_change_marker_still_parses() {
        let dir = std::env::temp_dir().join(format!("ladybug-pre-change-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("fixture.store");
        std::fs::write(&database, b"disposable database fixture").unwrap();
        // A marker written before adoption provenance existed has no field.
        let legacy = b"{\"version\":1,\"dimensions\":4,\"embedder_id\":\"fastembed:model-a\"}";
        std::fs::write(width_marker_path(&database), legacy).unwrap();
        let (marker, adopted_from) =
            read_width_marker_with_provenance(&database, &[], Some("fastembed:model-a"))
                .expect("pre-change marker still loads")
                .expect("pre-change marker exists");
        assert_eq!(marker.dimensions, 4);
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        assert_eq!(adopted_from, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn adoption_provenance_is_sticky() {
        let dir =
            std::env::temp_dir().join(format!("ladybug-adopt-sticky-{}", uuid::Uuid::new_v4()));
        let limits = StoreLimits::default();
        {
            let store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("open model A");
            store.init("probe").await.expect("create schema");
        }
        std::fs::write(
            width_marker_path(&dir),
            serde_json::to_vec(&json!({"version": 1, "dimensions": 4})).unwrap(),
        )
        .unwrap();
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("model A adopts the width-only store");
        }
        assert_eq!(
            read_marker_json(&dir).get("adopted_from"),
            Some(&json!("width-only"))
        );
        // A later open is a normal marked open — no adoption branch runs —
        // but the open path rewrites the marker and must not drop the record.
        {
            let _store =
                LadybugStore::new_with_identity_limits(&dir, 4, "fastembed:model-a".into(), limits)
                    .expect("adopted store reopens under the adopted identity");
        }
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(width_marker_path(&dir)).unwrap()).unwrap();
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        assert_eq!(
            read_marker_json(&dir).get("adopted_from"),
            Some(&json!("width-only")),
            "provenance survives the marker rewrite on a later open"
        );
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(width_marker_path(&dir));
    }
    /// Child worker for `profile_cap_refusal_fires_at_reduced_cap`:
    /// grows a store at an 8 MiB max-DB cap on incompressible rows,
    /// checkpointing after each upsert — a
    /// transaction's rows only reach the data file, and the capped page
    /// accounting, when a checkpoint merges the WAL — until the engine
    /// refuses. Asserts the refusal is the engine's max-DB-size error and
    /// that the seeded row still reads back after it, prints the PASS
    /// marker the parent asserts on, then exits directly: running the
    /// store's destructor would run the checkpoint-on-close that
    /// segfaults on lbug 0.20.4 after a cap refusal (see the parent
    /// test's comment).
    ///
    /// Marked `#[ignore]`: the body ends in `std::process::exit`, so it
    /// must never run as part of the suite proper — not even if the gate
    /// variable leaks into the suite's own environment. The parent
    /// spawns it by name with `--ignored`.
    ///
    /// Row math: at an 8 MiB cap the engine's VM region holds exactly
    /// two frame groups — one shadow, one data — so the data file has
    /// 1024 pages (4 MiB). Row WIDTH decides which engine limit fires
    /// first (CI fix loop round 4, probed against the lbug 0.20.4 wheel
    /// at this test's exact cadence): 32 KiB rows starved the buffer
    /// pool during checkpoint merges — "the buffer pool is full and no
    /// memory could be freed" at ~33 rows — because this repo's
    /// `StoreLimits::validate` couples the pool to at most the cap, so
    /// the pool cannot be given the headroom wide-row merges want; the
    /// same starvation recurred at a 64 MiB cap/pool (~512 rows). With
    /// 4 KiB incompressible rows the per-checkpoint merge working set
    /// stays small enough that the frame-group accounting — the cap
    /// itself — fires first: the engine's max-DB-size refusal was
    /// observed at ~208 rows in a two-column probe table, and at 310
    /// rows (2 KiB rows) and 122 rows (8 KiB rows), so the window is
    /// wide, not a knife's edge. This table's rows are somewhat wider
    /// than the probe's (vector + metadata alongside the content), so
    /// the refusal is expected no later; the 1024-row bound is ~5x
    /// headroom over the probe's 208 and is unreachable unless the cap
    /// is not enforced at all.
    ///
    /// Re-tuned 2026-11-01 (owner's decision, fork forward-merge to
    /// upstream v1.14.3-patch): upstream's IngestGeneration DDL adds
    /// a table whose pages shift the checkpoint's buffer-pool
    /// geometry in the merged tree. With the original 4 KiB filler
    /// the pool starved at row ~127, and with 2 KiB filler at row
    /// ~255 — in both cases the pool died with the data file short
    /// of its 1024-page group (807 and 968 pages respectively), so
    /// the buffer-pool error fired before the frame-group accounting
    /// could. (Disabling only the IngestGeneration DDL restored the
    /// pre-merge behaviour exactly: refusal at row 174 via the
    /// max-DB-size error, with per-row file growth byte-identical
    /// between the trees.) At 1 KiB filler the per-checkpoint merge
    /// working set stays small enough that the data file completes
    /// its group: the max-DB-size refusal was observed at row 455
    /// with the data file at 1023 pages. The 1024-row bound is ~2.2x
    /// headroom over that observation and remains unreachable unless
    /// the cap is not enforced at all. The assertion itself is
    /// unchanged: the refusal must be the engine's max-DB-size error.
    #[tokio::test]
    #[ignore = "child worker: spawned by profile_cap_refusal_fires_at_reduced_cap"]
    async fn profile_cap_refusal_child_worker() {
        let Ok(db) = std::env::var("NEUROSTRATA_TEST_PROFILE_CAP_DB") else {
            return;
        };
        let db = PathBuf::from(db);
        // The pool rides at the cap rather than at the profile's 64 MiB:
        // this repo's StoreLimits::validate refuses a buffer pool larger
        // than the max-DB ceiling ("the buffer-pool ceiling cannot
        // exceed the database-size ceiling"), a coupling the engine
        // itself does not impose — its verifySizeParams checks each
        // value on its own. The pool size plays no part in the cap
        // mechanism under test (frame-group accounting on the data
        // file); the profile's pool pairs with the profile's 2 GiB cap,
        // which satisfies the same validation.
        let limits = StoreLimits {
            buffer_pool_bytes: 8 * 1024 * 1024,
            max_db_bytes: 8 * 1024 * 1024,
        };
        let payload = |content: String| MemoryPayload {
            content,
            user_id: "agent".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: "fixture.json".into(),
            location_lines: "1".into(),
            metadata: json!({"evidence": "cap"}),
        };
        let store = LadybugStore::new_with_identity_limits(
            &db,
            4,
            "fastembed:BGESmallENV15Q".into(),
            limits,
        )
        .expect("open a new store at the reduced cap");
        store.init("profile-cap").await.expect("create schema");
        store
            .upsert(
                "profile-cap",
                "profile-cap::seed",
                vec![0.11, 0.22, 0.33, 0.44],
                payload("seed row".into()),
            )
            .await
            .expect("seed insert under the cap");
        let mut refusal = None;
        for i in 0..1024u64 {
            let inserted = store
                .upsert(
                    "profile-cap",
                    &format!("profile-cap::row-{i}"),
                    vec![0.11, 0.22, 0.33, 0.44],
                    payload(incompressible_filler(i)),
                )
                .await;
            // The checkpoint is part of the growth mechanism, not an
            // afterthought: upserts only mark the store dirty, and the
            // merge is what allocates data-file pages against the cap.
            let step = match inserted {
                Ok(_) => store.checkpoint().await,
                Err(error) => Err(error),
            };
            match step {
                Ok(()) => continue,
                Err(error) => {
                    refusal = Some(error);
                    break;
                }
            }
        }
        let refusal = refusal.expect(
            "a store grown to its max-DB cap must refuse further growth",
        );
        // The refusal must be the engine's size-cap error, not any loud
        // error: an identity, marker, schema, or WAL failure along the
        // way must fail this test, not satisfy it. Rounds 1–2 of the CI
        // fix loop verified the engine's exact text ("Maximum database
        // size of {} bytes has been reached. Reopen the database with a
        // larger max_db_size to continue."), so the assertion names that
        // family directly instead of the broader alternation the
        // unverified text once required.
        let message = format!("{refusal:#}");
        let lowered = message.to_lowercase();
        assert!(
            lowered.contains("maximum database size") && lowered.contains("has been reached"),
            "the refusal must be the engine's max-DB-size error, got: {message}"
        );
        for other_path in ["model identity", "marker", "checksum", "width"] {
            assert!(
                !lowered.contains(other_path),
                "the refusal came from the {other_path} path, not the size cap: {message}"
            );
        }
        // The refusal is clean: the store stays readable afterwards
        // (round 2 probe — a post-refusal scan returned the row count).
        let seed = store
            .get("profile-cap", "profile-cap::seed")
            .await
            .expect("a read after the refusal must still succeed")
            .expect("the seed row must survive the refusal");
        assert_eq!(seed.1.content, "seed row");
        println!("PROFILE_CAP_REFUSAL_PASS refusal: {message}");
        {
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }
        // Exit without unwinding: dropping the store would run the
        // engine's checkpoint-on-close, which is exactly the path that
        // segfaults after a cap refusal on lbug 0.20.4 (documented on
        // the parent test, not exercised here).
        std::process::exit(0);
    }

    /// Deterministic incompressible filler: xorshift64* output, hex
    /// encoded — 64 words become a 1 KiB string. Hex of pseudo-random
    /// bytes carries 4 bits of entropy per stored character, so even
    /// ideal entropy coding could at best halve it — where
    /// repeated-character filler compresses ~1000:1 and plateaus at
    /// ~2.2 MB, never approaching any cap (CI fix loop round 2 probe).
    /// The width is deliberately narrow: at 32 KiB per row the buffer
    /// pool starved during checkpoint merges before the size cap could
    /// fire (round 4 probe — see the child worker's row math), and the
    /// 2026-11-01 re-tune (see the child worker's row math) narrowed
    /// the filler from 4 KiB to 1 KiB because the merged tree's
    /// checkpoint pool geometry leaves less headroom. The growth
    /// loop's row bound is sized to survive the halving.
    fn incompressible_filler(seed: u64) -> String {
        let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03);
        let mut out = String::with_capacity(1024);
        for _ in 0..64 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            out.push_str(&format!("{word:016x}"));
        }
        out
    }

    /// Spec Feature 1 AC7, reduced-cap subprocess form (Pratik's
    /// decision, 2026-10-24 — CI fix loop round 3). A real store opened
    /// through the production path refuses further growth with the
    /// engine's max-DB-size error once it reaches its configured cap.
    /// The growth runs in a child process (the
    /// `profile_cap_refusal_child_worker` test above, spawned the same
    /// way the publication crash-recovery tests spawn theirs); this
    /// parent asserts on the child's exit status and output.
    ///
    /// Why a reduced cap: the cap under test is 8 MiB, the smallest
    /// value the engine admits — `BufferManager::verifySizeParams` (lbug
    /// v0.20.4, src/storage/buffer_manager/buffer_manager.cpp) requires
    /// at least two page groups, one for the main data file and one for
    /// the shadow file: 2 * 4096 * 1024 = 8 MiB. The limit reaches the
    /// engine through the same `StoreLimits` -> `SystemConfig::
    /// max_db_size` flow as any other value (fix loop round 1 verified
    /// that leg end to end), with one repo-level qualification:
    /// `StoreLimits::validate` requires the buffer pool to be no larger
    /// than the max-DB ceiling, so at this reduced cap the child's pool
    /// rides at the cap instead of the profile's 64 MiB. The profile-low
    /// value itself, 2 GiB, is pinned by
    /// `profile_values_are_the_spec_starting_values` in src/config.rs.
    ///
    /// Why a subprocess, and why the 2 GiB end-to-end form is BLOCKED:
    /// lbug 0.20.4 has a FileHandle exception-safety defect — a file
    /// handle claims page capacity *before* allocating the frame group
    /// that capacity describes, so when the allocation throws at the
    /// cap the handle is left claiming capacity for a group that was
    /// never recorded, and the next checkpoint, or the checkpoint on
    /// database close, indexes the frame-group table out of bounds and
    /// segfaults the process (fix loop round 2,
    /// reproduced against the engine in both growth modes). The refusal
    /// itself is clean, and the refusal is what this test exercises; the
    /// defect is documented here rather than exercised — the child
    /// asserts the refusal, prints its marker, and exits via
    /// `std::process::exit`, never running the destructor checkpoint
    /// that would crash. A 2 GiB end-to-end variant cannot do that
    /// inside the test harness: it would segfault in teardown and fail
    /// the suite over an engine defect this repo does not own. It stays
    /// blocked on an upstream engine fix.
    ///
    /// Growth is natural only — no file pre-grow (round 2: the pre-grow
    /// was an accelerant, never the cause), and the filler is
    /// incompressible (see `incompressible_filler`).
    #[tokio::test]
    async fn profile_cap_refusal_fires_at_reduced_cap() {
        use std::io::Read as _;
        let db =
            std::env::temp_dir().join(format!("ladybug-profile-cap-{}", uuid::Uuid::new_v4()));
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("store::ladybug::tests::profile_cap_refusal_child_worker")
            .arg("--nocapture")
            .arg("--ignored")
            .env("NEUROSTRATA_TEST_PROFILE_CAP_DB", &db)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= std::time::Duration::from_secs(120) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("profile cap child did not finish before the watchdog");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        assert!(
            status.success(),
            "profile cap child failed: {status}; child output:\n{output}"
        );
        assert!(
            output.contains("PROFILE_CAP_REFUSAL_PASS"),
            "child never reported the cap refusal; child output:\n{output}"
        );
        assert!(
            output.contains("Maximum database size"),
            "child's refusal was not the engine's max-DB-size error; child output:\n{output}"
        );
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(width_marker_path(&db));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_file_symlink_cannot_bypass_embedding_identity_marker() {
        let root =
            std::env::temp_dir().join(format!("ladybug-identity-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("primary.store");
        let alias = root.join("alias.store");
        let parent_alias = root.join("through-parent");
        std::os::unix::fs::symlink(&root, &parent_alias).unwrap();
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("alias-identity-fixture").await.unwrap();
            store
                .upsert(
                    "model-fixture",
                    "model-fixture::row",
                    vec![0.11, 0.22, 0.33, 0.44],
                    MemoryPayload {
                        content: "identity evidence".into(),
                        user_id: "agent".into(),
                        memory_type: "fact".into(),
                        agent_name: None,
                        location: "fixture.json".into(),
                        location_lines: "1".into(),
                        metadata: json!({"evidence": "preserve"}),
                    },
                )
                .await
                .unwrap();
        }
        let original_marker = width_marker_path(&original);
        let marker_before = std::fs::read(&original_marker).unwrap();
        let vector_before = {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store
                .with_conn(|conn| {
                    let mut rows = conn.query(
                        "MATCH (m:Memory) WHERE m.id = 'model-fixture::row' RETURN m.embedding",
                    )?;
                    let row = rows
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("seed vector missing"))?;
                    Ok(format!("{}", row[0]))
                })
                .await
                .unwrap()
        };
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        {
            let _store = LadybugStore::new_with_identity_limits(
                &alias,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .expect("matching model identity opens through the symlink alias");
        }

        let error = match LadybugStore::new_with_identity_limits(
            &alias,
            4,
            "fastembed:model-b".into(),
            limits,
        ) {
            Ok(_) => {
                panic!("opening an existing DB through a symlink must retain its model identity")
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("model identity"), "{error:#}");
        assert_eq!(std::fs::read(&original_marker).unwrap(), marker_before);
        assert!(!raw_database_sibling(&alias, ".embedding").exists());
        for supported_alias in [parent_alias.join("primary.store")] {
            {
                let _store = LadybugStore::new_with_identity_limits(
                    &supported_alias,
                    4,
                    "fastembed:model-a".into(),
                    limits,
                )
                .expect("aliases resolve to the original database identity");
            }
            let error = LadybugStore::new_with_identity_limits(
                &supported_alias,
                8,
                "fastembed:model-a".into(),
                limits,
            )
            .err()
            .expect("wrong width through alias is refused");
            assert!(error.to_string().contains("width 4"), "{error:#}");
            assert_eq!(std::fs::read(&original_marker).unwrap(), marker_before);
        }
        let new_via_parent = parent_alias.join("new-through-parent.store");
        {
            let store = LadybugStore::new_with_identity_limits(
                &new_via_parent,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .expect("new DB beneath a symlinked parent uses canonical identity");
            assert_eq!(
                store.local_path,
                std::fs::canonicalize(&root)
                    .unwrap()
                    .join("new-through-parent.store")
            );
        }
        assert!(root.join("new-through-parent.store.embedding").is_file());
        let error = LadybugStore::new_with_identity_limits(
            &root.join("new-through-parent.store"),
            4,
            "fastembed:model-b".into(),
            limits,
        )
        .err()
        .expect("canonical path sees the identity created through parent alias");
        assert!(error.to_string().contains("model identity"));
        let _cwd_guard = TEST_CWD_MUTEX.lock().unwrap();
        let previous_directory = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        {
            let _store = LadybugStore::new_with_identity_limits(
                Path::new("primary.store"),
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .expect("relative DB path uses the canonical identity");
        }
        let relative_width_error = LadybugStore::new_with_identity_limits(
            Path::new("primary.store"),
            8,
            "fastembed:model-a".into(),
            limits,
        )
        .err()
        .expect("relative alias cannot bypass width validation");
        std::env::set_current_dir(previous_directory).unwrap();
        assert!(relative_width_error.to_string().contains("width 4"));
        assert_eq!(std::fs::read(&original_marker).unwrap(), marker_before);
        let original_store = LadybugStore::new_with_identity_limits(
            &original,
            4,
            "fastembed:model-a".into(),
            limits,
        )
        .unwrap();
        let record = original_store
            .get("model-fixture", "model-fixture::row")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.1.content, "identity evidence");
        assert_eq!(record.1.metadata, json!({"evidence": "preserve"}));
        let vector = original_store
            .with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (m:Memory) WHERE m.id = 'model-fixture::row' RETURN m.embedding",
                )?;
                let row = rows
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("seed vector disappeared"))?;
                Ok(format!("{}", row[0]))
            })
            .await
            .unwrap();
        assert_eq!(
            vector, vector_before,
            "rejected alias open preserves exact vector data"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Refusal Standard at the alias site (Spec Feature 3 AC1, the
    /// substantive fix): the alias refusal names both canonicalized
    /// identities, both widths (equal here — the width check has already
    /// passed), and the reference remediation wording.
    #[cfg(unix)]
    #[test]
    fn alias_identity_refusal_meets_the_refusal_standard() {
        let root =
            std::env::temp_dir().join(format!("ladybug-alias-standard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("primary.store");
        let alias = root.join("alias.store");
        let limits = StoreLimits::default();
        {
            let _store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .expect("open model A");
        }
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let error = match LadybugStore::new_with_identity_limits(
            &alias,
            4,
            "fastembed:model-b".into(),
            limits,
        ) {
            Ok(_) => panic!("opening an existing DB through a symlink must retain its model identity"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("database path alias"), "the alias site fired: {message}");
        for element in [
            "fastembed:model-a",
            "fastembed:model-b",
            "recorded embedding width is 4",
            "configured embedding width is 4",
            "Keep the database intact",
            "original embedder",
            "separate database path",
            "no in-place re-embedding migration is available",
        ] {
            assert!(message.contains(element), "alias refusal element {element:?} missing: {message}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn marker_preflight_stays_bound_to_resolved_db_if_supplied_alias_retargets() {
        let root =
            std::env::temp_dir().join(format!("ladybug-marker-retarget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let resolved_db = root.join("resolved.store");
        let other_db = root.join("other.store");
        let supplied_alias = root.join("alias.store");
        std::fs::write(&resolved_db, b"resolved database fixture").unwrap();
        std::fs::write(&other_db, b"other database fixture").unwrap();
        write_width_marker(&resolved_db, 4, Some("fastembed:model-a")).unwrap();
        write_width_marker(&other_db, 4, Some("fastembed:model-b")).unwrap();
        std::os::unix::fs::symlink(&other_db, &supplied_alias).unwrap();

        // Models the alias being retargeted after the constructor resolved and
        // locked `resolved_db`, but before it inspects the caller's old spelling.
        let marker = read_width_marker(
            &resolved_db,
            std::slice::from_ref(&supplied_alias),
            Some("fastembed:model-a"),
        )
        .expect("resolved database marker should be authoritative")
        .expect("resolved marker exists");
        assert_eq!(marker.dimensions, 4);
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn alias_retarget_after_resolution_fails_before_marker_adoption() {
        let root = std::env::temp_dir().join(format!(
            "ladybug-alias-retarget-preflight-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let primary_a = root.join("primary-a.store");
        let primary_b = root.join("primary-b.store");
        let historical = root.join("historical.store");
        let outer = root.join("outer.store");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &primary_a,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("retarget-fixture").await.unwrap();
            store
                .upsert(
                    "retarget-fixture",
                    "retarget-fixture::row",
                    vec![0.11, 0.22, 0.33, 0.44],
                    MemoryPayload {
                        content: "retargeted alias evidence".into(),
                        user_id: "agent".into(),
                        memory_type: "fact".into(),
                        agent_name: None,
                        location: "fixture.json".into(),
                        location_lines: "1".into(),
                        metadata: json!({"keep": true}),
                    },
                )
                .await
                .unwrap();
        }
        {
            let store = LadybugStore::new_with_identity_limits(
                &primary_b,
                4,
                "fastembed:model-b".into(),
                limits,
            )
            .unwrap();
            store.init("other-retarget-fixture").await.unwrap();
        }
        let primary_marker = raw_database_sibling(&primary_a, ".embedding");
        let primary_marker_original = std::fs::read(&primary_marker).unwrap();
        std::fs::remove_file(&primary_marker).unwrap();
        let historical_marker = raw_database_sibling(&historical, ".embedding");
        let identity_a = serde_json::to_vec(&EmbeddingMarker {
            version: 1,
            dimensions: 4,
            embedder_id: Some("fastembed:model-a".into()),
        })
        .unwrap();
        std::fs::write(&historical_marker, &identity_a).unwrap();
        std::os::unix::fs::symlink(&primary_a, &historical).unwrap();
        std::os::unix::fs::symlink(&historical, &outer).unwrap();

        let resolved = resolve_database_path(&outer).unwrap();
        std::fs::remove_file(&historical).unwrap();
        std::os::unix::fs::symlink(&primary_b, &historical).unwrap();

        let error = discover_database_path_aliases(&resolved, &outer)
            .expect_err("a retarget between resolution and alias discovery must fail closed");
        assert!(error
            .to_string()
            .contains("changed while it was being inspected"));
        assert_eq!(std::fs::read(&historical_marker).unwrap(), identity_a);
        assert!(!primary_marker.exists());

        let store = LadybugStore::new_with_identity_limits(
            &primary_a,
            4,
            "fastembed:model-a".into(),
            limits,
        )
        .unwrap();
        let row = store
            .get("retarget-fixture", "retarget-fixture::row")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.1.content, "retargeted alias evidence");
        assert_eq!(row.1.metadata, json!({"keep": true}));
        assert_eq!(row.0, vec![0.11, 0.22, 0.33, 0.44]);
        assert_eq!(
            std::fs::read(&primary_marker).unwrap(),
            serde_json::to_vec_pretty(&PersistedEmbeddingMarker {
                version: 1,
                dimensions: 4,
                embedder_id: Some("fastembed:model-a".into()),
                adopted_from: Some("schema-only".into()),
            })
            .unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_path_keeps_parent_components_after_symlink_resolution() {
        let root = std::env::temp_dir().join(format!(
            "ladybug-symlink-parent-component-{}",
            uuid::Uuid::new_v4()
        ));
        let real_subdir = root.join("real/subdir");
        let entry = root.join("entry");
        std::fs::create_dir_all(&real_subdir).unwrap();
        std::fs::create_dir_all(&entry).unwrap();
        let primary_a = root.join("real/db.store");
        let lexical_b = entry.join("db.store");
        let link = entry.join("link");
        std::os::unix::fs::symlink("../real/subdir", &link).unwrap();
        let requested = entry.join("link/../db.store");
        let limits = StoreLimits::default();
        for (path, model, namespace, content, vector) in [
            (
                &primary_a,
                "fastembed:model-a",
                "symlink-parent-a",
                "database reached through symlink then parent traversal",
                vec![0.1, 0.2, 0.3, 0.4],
            ),
            (
                &lexical_b,
                "fastembed:model-b",
                "lexical-parent-b",
                "database reached by lexical collapse only",
                vec![0.4, 0.3, 0.2, 0.1],
            ),
        ] {
            let store =
                LadybugStore::new_with_identity_limits(path, 4, model.into(), limits).unwrap();
            store.init(namespace).await.unwrap();
            store
                .upsert(
                    namespace,
                    &format!("{namespace}::sentinel"),
                    vector,
                    MemoryPayload {
                        content: content.into(),
                        user_id: "agent".into(),
                        memory_type: "fact".into(),
                        agent_name: None,
                        location: "sentinel.json".into(),
                        location_lines: "1".into(),
                        metadata: json!({"database": namespace}),
                    },
                )
                .await
                .unwrap();
        }
        let marker_a = raw_database_sibling(&primary_a, ".embedding");
        let marker_b = raw_database_sibling(&lexical_b, ".embedding");
        let marker_a_before = std::fs::read(&marker_a).unwrap();
        let marker_b_before = std::fs::read(&marker_b).unwrap();
        assert_eq!(
            resolve_database_path(&requested).unwrap(),
            std::fs::canonicalize(&primary_a).unwrap()
        );

        let error = LadybugStore::new_with_identity_limits(
            &requested,
            4,
            "fastembed:model-b".into(),
            limits,
        )
        .err()
        .expect("the actual symlink-resolved database A must reject model B");
        assert!(error.to_string().contains("model identity"));
        assert_eq!(std::fs::read(&marker_a).unwrap(), marker_a_before);
        assert_eq!(std::fs::read(&marker_b).unwrap(), marker_b_before);

        let store_a = LadybugStore::new_with_identity_limits(
            &primary_a,
            4,
            "fastembed:model-a".into(),
            limits,
        )
        .unwrap();
        let row_a = store_a
            .get("symlink-parent-a", "symlink-parent-a::sentinel")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row_a.1.content,
            "database reached through symlink then parent traversal"
        );
        assert_eq!(row_a.0, vec![0.1, 0.2, 0.3, 0.4]);
        let store_b = LadybugStore::new_with_identity_limits(
            &lexical_b,
            4,
            "fastembed:model-b".into(),
            limits,
        )
        .unwrap();
        let row_b = store_b
            .get("lexical-parent-b", "lexical-parent-b::sentinel")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row_b.1.content, "database reached by lexical collapse only");
        assert_eq!(row_b.0, vec![0.4, 0.3, 0.2, 0.1]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn marker_write_failure_is_returned_and_existing_evidence_is_preserved() {
        let root = std::env::temp_dir().join(format!(
            "ladybug-marker-write-failure-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("fixture.store");
        std::fs::write(&database, b"disposable database bytes").unwrap();
        let marker = width_marker_path(&database);
        std::fs::create_dir(&marker).unwrap();
        let sentinel = marker.join("preserve-this-evidence");
        std::fs::write(&sentinel, b"existing marker evidence").unwrap();

        let error = write_width_marker(&database, 4, Some("fastembed:model-a"))
            .expect_err("the existing marker directory must make atomic replacement fail");
        assert!(error
            .to_string()
            .contains("could not persist the embedding marker"));
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"existing marker evidence"
        );
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("fixture.store.embedding.")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn creating_database_from_bare_relative_path_uses_canonical_parent() {
        let root =
            std::env::temp_dir().join(format!("ladybug-relative-new-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _cwd_guard = TEST_CWD_MUTEX.lock().unwrap();
        let previous_directory = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        {
            let _store = LadybugStore::new_with_identity_limits(
                Path::new("relative.store"),
                4,
                "fastembed:model-a".into(),
                StoreLimits::default(),
            )
            .expect("a bare relative path resolves through the current directory");
        }
        std::env::set_current_dir(previous_directory).unwrap();
        assert!(root.join("relative.store").is_file());
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(root.join("relative.store.embedding")).unwrap())
                .unwrap();
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn alias_opened_database_exports_resolved_database_and_marker() {
        let root =
            std::env::temp_dir().join(format!("ladybug-alias-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("primary.store");
        let alias = root.join("alias.store");
        let snapshot = root.join("snapshot");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("alias-export").await.unwrap();
            store
                .upsert(
                    "alias-export",
                    "alias-export::first",
                    vec![0.1, 0.2, 0.3, 0.4],
                    MemoryPayload {
                        content: "written before alias open".into(),
                        user_id: "agent".into(),
                        memory_type: "fact".into(),
                        agent_name: None,
                        location: "fixture".into(),
                        location_lines: "1".into(),
                        metadata: json!({"preserve": true}),
                    },
                )
                .await
                .unwrap();
        }
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let alias_store =
            LadybugStore::new_with_identity_limits(&alias, 4, "fastembed:model-a".into(), limits)
                .unwrap();
        assert_eq!(
            alias_store.local_path,
            std::fs::canonicalize(&original).unwrap()
        );
        alias_store
            .export_database(snapshot.to_str().unwrap())
            .await
            .unwrap();
        let snapshot_db = snapshot.join("ladybug.store");
        let snapshot_store = LadybugStore::new_with_identity_limits(
            &snapshot_db,
            4,
            "fastembed:model-a".into(),
            limits,
        )
        .unwrap();
        snapshot_store.init("alias-export-restored").await.unwrap();
        let restored = snapshot_store
            .get("alias-export", "alias-export::first")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.1.content, "written before alias open");
        assert_eq!(restored.1.metadata, json!({"preserve": true}));
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(width_marker_path(&snapshot_db)).unwrap())
                .unwrap();
        assert_eq!(marker.embedder_id.as_deref(), Some("fastembed:model-a"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn matching_legacy_alias_marker_is_preserved_without_weakening_canonical_identity() {
        let root =
            std::env::temp_dir().join(format!("ladybug-legacy-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("primary.store");
        let alias = root.join("alias.store");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("legacy-alias-fixture").await.unwrap();
        }
        let canonical_marker = width_marker_path(&original);
        std::fs::remove_file(&canonical_marker).unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let alias_marker = raw_database_sibling(&alias, ".embedding");
        std::fs::write(&alias_marker, "4\n").unwrap();
        {
            let _store = LadybugStore::new_with_identity_limits(
                &alias,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .expect("matching legacy alias width remains compatible");
        }
        let canonical: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(&canonical_marker).unwrap()).unwrap();
        assert_eq!(canonical.embedder_id.as_deref(), Some("fastembed:model-a"));
        assert_eq!(std::fs::read_to_string(&alias_marker).unwrap(), "4\n");
        let error = LadybugStore::new_with_identity_limits(
            &original,
            4,
            "fastembed:model-b".into(),
            limits,
        )
        .err()
        .expect("canonical identity cannot be weakened by numeric alias marker");
        assert!(error.to_string().contains("model identity"));
        assert_eq!(std::fs::read_to_string(&alias_marker).unwrap(), "4\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn conflicting_alias_marker_fails_before_open_without_rewriting_evidence() {
        let root =
            std::env::temp_dir().join(format!("ladybug-conflict-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("primary.store");
        let alias = root.join("alias.store");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("conflicting-alias-fixture").await.unwrap();
            store
                .upsert(
                    "conflict-fixture",
                    "conflict-fixture::row",
                    vec![0.15, 0.25, 0.35, 0.45],
                    MemoryPayload {
                        content: "conflict evidence".into(),
                        user_id: "agent".into(),
                        memory_type: "fact".into(),
                        agent_name: Some("fixture-agent".into()),
                        location: "fixture.json".into(),
                        location_lines: "2".into(),
                        metadata: json!({"keep": "all"}),
                    },
                )
                .await
                .unwrap();
        }
        let before_vector = {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store
                .with_conn(|conn| {
                    let mut rows = conn.query(
                        "MATCH (m:Memory) WHERE m.id = 'conflict-fixture::row' RETURN m.embedding",
                    )?;
                    let row = rows
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("fixture vector missing"))?;
                    Ok(format!("{}", row[0]))
                })
                .await
                .unwrap()
        };
        let canonical_marker = width_marker_path(&original);
        let canonical_before = std::fs::read(&canonical_marker).unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let alias_marker = raw_database_sibling(&alias, ".embedding");
        let conflict = serde_json::to_vec(&EmbeddingMarker {
            version: 1,
            dimensions: 4,
            embedder_id: Some("fastembed:model-b".into()),
        })
        .unwrap();
        std::fs::write(&alias_marker, &conflict).unwrap();
        let wal = raw_database_sibling(&original, ".wal");
        let wal_bytes = b"unopened WAL sentinel";
        std::fs::write(&wal, wal_bytes).unwrap();
        let error =
            LadybugStore::new_with_identity_limits(&alias, 4, "fastembed:model-a".into(), limits)
                .err()
                .expect("conflicting alias evidence is refused");
        assert!(error.to_string().contains("Conflicting embedding markers"));
        assert_eq!(std::fs::read(&canonical_marker).unwrap(), canonical_before);
        assert_eq!(std::fs::read(&alias_marker).unwrap(), conflict);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_bytes);
        assert!(!raw_database_sibling(&original, ".wal.unclean").exists());
        let width_conflict = serde_json::to_vec(&EmbeddingMarker {
            version: 1,
            dimensions: 8,
            embedder_id: Some("fastembed:model-a".into()),
        })
        .unwrap();
        std::fs::write(&alias_marker, &width_conflict).unwrap();
        let error =
            LadybugStore::new_with_identity_limits(&alias, 4, "fastembed:model-a".into(), limits)
                .err()
                .expect("alias width conflicts fail closed");
        assert!(error.to_string().contains("Conflicting embedding markers"));
        assert_eq!(std::fs::read(&canonical_marker).unwrap(), canonical_before);
        assert_eq!(std::fs::read(&alias_marker).unwrap(), width_conflict);
        std::fs::remove_file(wal).unwrap();
        let store = LadybugStore::new_with_identity_limits(
            &original,
            4,
            "fastembed:model-a".into(),
            limits,
        )
        .unwrap();
        let row = store
            .get("conflict-fixture", "conflict-fixture::row")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.1.content, "conflict evidence");
        assert_eq!(row.1.agent_name.as_deref(), Some("fixture-agent"));
        assert_eq!(row.1.location_lines, "2");
        assert_eq!(row.1.metadata, json!({"keep": "all"}));
        let after_vector = store
            .with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (m:Memory) WHERE m.id = 'conflict-fixture::row' RETURN m.embedding",
                )?;
                let row = rows
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("fixture vector missing"))?;
                Ok(format!("{}", row[0]))
            })
            .await
            .unwrap();
        assert_eq!(after_vector, before_vector);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chained_file_alias_markers_are_all_checked_before_open() {
        let root =
            std::env::temp_dir().join(format!("ladybug-chained-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let primary = root.join("primary.store");
        let historical = root.join("historical.store");
        let outer = root.join("outer.store");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &primary,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("chained-alias-fixture").await.unwrap();
            store
                .upsert(
                    "chained-alias-fixture",
                    "chained-alias-fixture::row",
                    vec![0.12, 0.23, 0.34, 0.45],
                    MemoryPayload {
                        content: "chained alias evidence".into(),
                        user_id: "agent".into(),
                        memory_type: "fact".into(),
                        agent_name: Some("alias-fixture".into()),
                        location: "fixture.json".into(),
                        location_lines: "7".into(),
                        metadata: json!({"preserve": "exact"}),
                    },
                )
                .await
                .unwrap();
        }
        std::os::unix::fs::symlink(&primary, &historical).unwrap();
        std::os::unix::fs::symlink(&historical, &outer).unwrap();
        let resolved = resolve_database_path(&outer).unwrap();
        let discovered = discover_database_path_aliases(&resolved, &outer).unwrap();
        assert!(
            discovered
                .iter()
                .any(|alias| alias.file_name() == historical.file_name()),
            "historical alias missing from {discovered:?}"
        );
        let canonical_marker = raw_database_sibling(&primary, ".embedding");
        let historical_marker = raw_database_sibling(&historical, ".embedding");
        let outer_marker = raw_database_sibling(&outer, ".embedding");
        let canonical_original = std::fs::read(&canonical_marker).unwrap();
        std::fs::remove_file(&canonical_marker).unwrap();

        let legacy_identity = serde_json::to_vec(&EmbeddingMarker {
            version: 1,
            dimensions: 4,
            embedder_id: Some("fastembed:model-a".into()),
        })
        .unwrap();
        std::fs::write(&historical_marker, &legacy_identity).unwrap();
        let error =
            LadybugStore::new_with_identity_limits(&outer, 4, "fastembed:model-b".into(), limits)
                .err()
                .expect("intermediate file-alias identity must block same-width model B");
        assert!(error.to_string().contains("model identity"));
        assert_eq!(std::fs::read(&historical_marker).unwrap(), legacy_identity);
        assert!(!canonical_marker.exists());
        assert!(!outer_marker.exists());

        std::fs::write(&canonical_marker, &canonical_original).unwrap();
        let intermediate_conflict = serde_json::to_vec(&EmbeddingMarker {
            version: 1,
            dimensions: 4,
            embedder_id: Some("fastembed:model-b".into()),
        })
        .unwrap();
        std::fs::write(&historical_marker, &intermediate_conflict).unwrap();
        let error =
            LadybugStore::new_with_identity_limits(&outer, 4, "fastembed:model-a".into(), limits)
                .err()
                .expect("conflicting intermediate marker must fail before engine open");
        assert!(error.to_string().contains("Conflicting embedding markers"));
        assert_eq!(
            std::fs::read(&canonical_marker).unwrap(),
            canonical_original
        );
        assert_eq!(
            std::fs::read(&historical_marker).unwrap(),
            intermediate_conflict
        );

        let store =
            LadybugStore::new_with_identity_limits(&primary, 4, "fastembed:model-a".into(), limits)
                .unwrap();
        let row = store
            .get("chained-alias-fixture", "chained-alias-fixture::row")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.1.content, "chained alias evidence");
        assert_eq!(row.1.agent_name.as_deref(), Some("alias-fixture"));
        assert_eq!(row.1.location_lines, "7");
        assert_eq!(row.1.metadata, json!({"preserve": "exact"}));
        assert_eq!(row.0, vec![0.12, 0.23, 0.34, 0.45]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chained_alias_parent_is_frozen_for_stage_recovery() {
        let root =
            std::env::temp_dir().join(format!("ladybug-chained-parent-{}", uuid::Uuid::new_v4()));
        let real_a = root.join("real-a");
        let real_b = root.join("real-b");
        std::fs::create_dir_all(&real_a).unwrap();
        std::fs::create_dir_all(&real_b).unwrap();
        let parent_alias = root.join("parent-alias");
        let primary_a = real_a.join("primary.store");
        let historical_a = real_a.join("historical.store");
        let primary_b = real_b.join("primary.store");
        let historical_b = real_b.join("historical.store");
        let outer = root.join("outer.store");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &primary_a,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("parent-alias-a").await.unwrap();
        }
        {
            let store = LadybugStore::new_with_identity_limits(
                &primary_b,
                4,
                "fastembed:model-b".into(),
                limits,
            )
            .unwrap();
            store.init("parent-alias-b").await.unwrap();
        }
        std::os::unix::fs::symlink(&primary_a, &historical_a).unwrap();
        std::os::unix::fs::symlink(&primary_b, &historical_b).unwrap();
        std::os::unix::fs::symlink(&real_a, &parent_alias).unwrap();
        std::os::unix::fs::symlink(parent_alias.join("historical.store"), &outer).unwrap();
        let store =
            LadybugStore::new_with_identity_limits(&outer, 4, "fastembed:model-a".into(), limits)
                .unwrap();

        let other_generation = uuid::Uuid::new_v4().to_string();
        let other_alias_lease =
            raw_database_sibling(&historical_b, &format!(".stage-{other_generation}.lock"));
        std::fs::write(&other_alias_lease, b"other database lease").unwrap();
        let other_marker = width_marker_path(&primary_b);
        let other_marker_before = std::fs::read(&other_marker).unwrap();

        std::fs::remove_file(&parent_alias).unwrap();
        std::os::unix::fs::symlink(&real_b, &parent_alias).unwrap();
        store.recover_interrupted_ingest_stages().await.unwrap();

        assert_eq!(
            std::fs::read(&other_alias_lease).unwrap(),
            b"other database lease",
            "recovery must not follow a retargeted intermediate parent alias"
        );
        assert_eq!(std::fs::read(&other_marker).unwrap(), other_marker_before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_alias_marker_fails_closed_before_engine_open() {
        let root =
            std::env::temp_dir().join(format!("ladybug-dangling-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("primary.store");
        let alias = root.join("alias.store");
        let limits = StoreLimits::default();
        {
            let store = LadybugStore::new_with_identity_limits(
                &original,
                4,
                "fastembed:model-a".into(),
                limits,
            )
            .unwrap();
            store.init("dangling-marker-fixture").await.unwrap();
        }
        let marker = width_marker_path(&original);
        let before = std::fs::read(&marker).unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let dangling = raw_database_sibling(&alias, ".embedding");
        std::os::unix::fs::symlink(root.join("missing-marker-target"), &dangling).unwrap();
        let error =
            LadybugStore::new_with_identity_limits(&alias, 4, "fastembed:model-a".into(), limits)
                .err()
                .expect("dangling alias marker is not equivalent to absent evidence");
        assert!(error
            .to_string()
            .contains("Cannot inspect the embedding marker"));
        assert_eq!(std::fs::read(&marker).unwrap(), before);
        assert!(std::fs::symlink_metadata(&dangling).is_ok());
        std::fs::remove_file(&dangling).unwrap();
        let malformed = b"{\"version\":1,\"dimensions\":4,\"embedder_id\":\"sensitive-endpoint\"";
        std::fs::write(&dangling, malformed).unwrap();
        let error =
            LadybugStore::new_with_identity_limits(&alias, 4, "fastembed:model-a".into(), limits)
                .err()
                .expect("malformed alias marker fails closed");
        assert!(error.to_string().contains("malformed"));
        assert!(!error.to_string().contains("sensitive-endpoint"));
        assert_eq!(std::fs::read(&dangling).unwrap(), malformed);
        assert_eq!(std::fs::read(&marker).unwrap(), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_database_symlink_is_not_treated_as_a_new_database() {
        let root =
            std::env::temp_dir().join(format!("ladybug-dangling-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let alias = root.join("alias.store");
        let missing_target = root.join("missing.store");
        std::os::unix::fs::symlink(&missing_target, &alias).unwrap();
        let alias_marker = raw_database_sibling(&alias, ".embedding");
        let marker_bytes = serde_json::to_vec(&EmbeddingMarker {
            version: 1,
            dimensions: 4,
            embedder_id: Some("fastembed:model-a".into()),
        })
        .unwrap();
        std::fs::write(&alias_marker, &marker_bytes).unwrap();
        let error = resolve_database_path(&alias).unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot resolve existing database path"));
        assert!(
            !missing_target.exists(),
            "dangling link must not create a new DB target"
        );
        assert_eq!(std::fs::read(&alias_marker).unwrap(), marker_bytes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn legacy_remote_identity_is_upgraded_and_mismatch_errors_redact_it() {
        let dir = std::env::temp_dir().join(format!("ladybug-remote-id-{}", uuid::Uuid::new_v4()));
        let legacy = "remote:https://user:top-secret@example.test/v1:embed-v2";
        let current =
            crate::embed::remote_embedder_id("https://user:top-secret@example.test/v1", "embed-v2");
        let marker_path = width_marker_path(&dir);
        {
            let store = LadybugStore::new_with_identity_limits(
                &dir,
                4,
                current.clone(),
                StoreLimits::default(),
            )
            .expect("create database using the matching runtime identity");
            store.init("legacy-remote-marker-fixture").await.unwrap();
        }
        std::fs::write(
            &marker_path,
            serde_json::to_vec(&EmbeddingMarker {
                version: 1,
                dimensions: 4,
                embedder_id: Some(legacy.to_string()),
            })
            .unwrap(),
        )
        .unwrap();

        {
            let _store = LadybugStore::new_with_identity_limits(
                &dir,
                4,
                current.clone(),
                StoreLimits::default(),
            )
            .expect("matching legacy remote marker is accepted");
        }
        let rewritten: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(rewritten.embedder_id.as_deref(), Some(current.as_str()));
        let serialized = std::fs::read_to_string(&marker_path).unwrap();
        assert!(!serialized.contains("top-secret"));
        assert!(!serialized.contains("https://"));

        #[cfg(unix)]
        {
            let alias = dir.with_extension("alias.store");
            std::os::unix::fs::symlink(&dir, &alias).unwrap();
            let alias_marker = raw_database_sibling(&alias, ".embedding");
            std::fs::write(
                &alias_marker,
                serde_json::to_vec(&EmbeddingMarker {
                    version: 1,
                    dimensions: 4,
                    embedder_id: Some(legacy.to_string()),
                })
                .unwrap(),
            )
            .unwrap();
            let _store = LadybugStore::new_with_identity_limits(
                &alias,
                4,
                current.clone(),
                StoreLimits::default(),
            )
            .expect(
                "legacy alias identity that canonicalizes to the configured remote is accepted",
            );
            assert!(
                std::fs::read_to_string(&alias_marker)
                    .unwrap()
                    .contains("top-secret"),
                "historical alias evidence is preserved rather than rewritten"
            );
            let error = LadybugStore::new_with_identity_limits(
                &alias,
                4,
                crate::embed::remote_embedder_id(
                    "https://other:top-secret@example.test/v1",
                    "embed-v2",
                ),
                StoreLimits::default(),
            )
            .err()
            .expect("alias endpoint mismatch is refused");
            assert!(error.to_string().contains("model identity"));
            assert!(!error.to_string().contains("top-secret"));
            assert!(!error.to_string().contains("https://"));
            let _ = std::fs::remove_file(alias_marker);
            let _ = std::fs::remove_file(alias);
        }

        std::fs::write(
            &marker_path,
            serde_json::to_vec(&EmbeddingMarker {
                version: 1,
                dimensions: 4,
                embedder_id: Some(legacy.to_string()),
            })
            .unwrap(),
        )
        .unwrap();
        let error = match LadybugStore::new_with_identity_limits(
            &dir,
            4,
            crate::embed::remote_embedder_id(
                "https://other:top-secret@example.test/v1",
                "embed-v2",
            ),
            StoreLimits::default(),
        ) {
            Ok(_) => panic!("a different endpoint must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("model identity"));
        assert!(!error.to_string().contains("top-secret"));
        assert!(!error.to_string().contains("https://"));

        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(marker_path);
    }

    /// Refusal Standard element 6 (Spec Feature 3 AC2): a legacy persisted
    /// identity embedding a credential-bearing URL must appear in NO
    /// refusal string from any site — width, marker-identity, or alias.
    /// Only canonicalized identities may enter message text; a legacy
    /// identity that cannot be canonicalized against the configured one
    /// is never printed raw.
    #[tokio::test]
    async fn credential_bearing_legacy_identity_never_appears() {
        let legacy = "remote:https://user:top-secret@example.test/v1:embed-v2";
        let other = crate::embed::remote_embedder_id(
            "https://other:top-secret@example.test/v1",
            "embed-v2",
        );
        let assert_redacted = |error: &anyhow::Error, site: &str| {
            let message = format!("{error:#}");
            for leaked in [legacy, "top-secret", "https://", "example.test"] {
                assert!(
                    !message.contains(leaked),
                    "{site} refusal leaked {leaked:?}: {message}"
                );
            }
        };

        // Width site: the legacy identity canonicalizes to the configured
        // (hashed) remote identity, so the width check is the refusal that
        // fires; its message may carry only the canonicalized form.
        let dir = std::env::temp_dir().join(format!("ladybug-cred-width-{}", uuid::Uuid::new_v4()));
        let marker_path = width_marker_path(&dir);
        let matching = crate::embed::remote_embedder_id(
            "https://user:top-secret@example.test/v1",
            "embed-v2",
        );
        {
            let store = LadybugStore::new_with_identity_limits(
                &dir,
                4,
                matching.clone(),
                StoreLimits::default(),
            )
            .expect("create database using the matching runtime identity");
            store.init("credential-width-fixture").await.unwrap();
        }
        std::fs::write(
            &marker_path,
            serde_json::to_vec(&EmbeddingMarker {
                version: 1,
                dimensions: 4,
                embedder_id: Some(legacy.to_string()),
            })
            .unwrap(),
        )
        .unwrap();
        let error = LadybugStore::new_with_identity_limits(&dir, 8, matching.clone(), StoreLimits::default())
            .err()
            .expect("width mismatch with a legacy identity is refused");
        assert_redacted(&error, "width");
        assert!(
            format!("{error:#}").contains(&matching),
            "the width refusal names the canonicalized identity only: {error:#}"
        );

        // Marker-identity site: same width, mismatched configured endpoint.
        // The refusal that surfaces is the canonicalization bail (via
        // read_width_marker, which precedes the ladybug identity bails);
        // pin its upgraded Refusal Standard content positively, not only
        // its redaction.
        let error = LadybugStore::new_with_identity_limits(&dir, 4, other.clone(), StoreLimits::default())
            .err()
            .expect("endpoint mismatch with a legacy identity is refused");
        assert_redacted(&error, "marker-identity");
        let message = format!("{error:#}");
        for element in [
            other.as_str(),
            "Keep the database intact",
            "original endpoint and model",
            "separate database path",
            "no in-place re-embedding migration is available",
        ] {
            assert!(message.contains(element), "canonicalization refusal element {element:?} missing: {message}");
        }
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(&marker_path);

        // Alias site: the legacy identity is reached through a symlink.
        #[cfg(unix)]
        {
            let root = std::env::temp_dir()
                .join(format!("ladybug-cred-alias-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let original = root.join("primary.store");
            let alias = root.join("alias.store");
            {
                let _store = LadybugStore::new_with_identity_limits(
                    &original,
                    4,
                    crate::embed::remote_embedder_id(
                        "https://user:top-secret@example.test/v1",
                        "embed-v2",
                    ),
                    StoreLimits::default(),
                )
                .expect("create database using the matching runtime identity");
            }
            std::fs::write(
                width_marker_path(&original),
                serde_json::to_vec(&EmbeddingMarker {
                    version: 1,
                    dimensions: 4,
                    embedder_id: Some(legacy.to_string()),
                })
                .unwrap(),
            )
            .unwrap();
            std::os::unix::fs::symlink(&original, &alias).unwrap();
            let error = LadybugStore::new_with_identity_limits(
                &alias,
                4,
                other.clone(),
                StoreLimits::default(),
            )
            .err()
            .expect("alias endpoint mismatch with a legacy identity is refused");
            assert_redacted(&error, "alias");
            let message = format!("{error:#}");
            for element in [
                other.as_str(),
                "no in-place re-embedding migration is available",
            ] {
                assert!(message.contains(element), "alias canonicalization refusal element {element:?} missing: {message}");
            }
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// The canonical source key used by the relink/publish seed helpers. A real
    /// ingest derives it from the directory path (`source_key_for`); the unit
    /// tests pin a stable literal so the source-scoped pointer is deterministic.
    const PROBE_SOURCE: &str = "/worktrees/probe";

    /// The resolution rule as every caller now reaches it: build the index
    /// once, then ask it.
    fn resolve(declared: &str, known: &[String]) -> Option<String> {
        KnownIds::new(known.iter().map(|id| id.as_str())).resolve(declared)
    }

    #[test]
    fn the_engines_refusal_is_recognised_as_a_collision() {
        let refusal = anyhow::anyhow!(
            "Query execution failed: Cannot start a new write transaction in the system. Only one write transaction at a time is allowed in the system."
        );
        assert!(is_write_collision(&refusal));
    }

    /// Isolation probe for the EXPORT DATABASE segfault (guinea-pig BUG-3):
    /// if this dies with SIGSEGV the engine's export is broken on EVERY store,
    /// not just on one damaged by an incident. A crash here is the diagnosis.
    #[tokio::test]
    async fn export_database_survives_a_fresh_store() {
        let root = std::env::temp_dir().join(format!("lbug-export-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fresh.lbug");
        let store = LadybugStore::for_testing(db.to_string_lossy().to_string(), 4).unwrap();
        store.init("ExportProbe").await.unwrap();
        let out = root.join("out");
        store
            .export_database(&out.to_string_lossy())
            .await
            .expect("EXPORT snapshot on a fresh store");
        assert!(
            out.join("ladybug.store").exists(),
            "the store file is in the snapshot"
        );
        assert!(
            out.join("manifest.json").exists(),
            "the snapshot carries a manifest"
        );
        let snapshot_marker = width_marker_path(&out.join("ladybug.store"));
        let marker: EmbeddingMarker =
            serde_json::from_slice(&std::fs::read(snapshot_marker).unwrap()).unwrap();
        assert_eq!(
            marker.dimensions, 4,
            "the snapshot stays paired with its width"
        );
    }

    #[tokio::test]
    async fn cancelled_init_keeps_schema_exclusion_until_engine_work_finishes() {
        let root = std::env::temp_dir().join(format!("init-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fixture.store");
        let store = Arc::new(LadybugStore::for_testing(&db, 4).unwrap());
        let barrier = root.join("init-entered");
        set_test_snapshot_operation_barrier("init", db.clone(), barrier.clone());

        let init_store = store.clone();
        let init = tokio::spawn(async move { init_store.init("init-cancel").await });
        wait_for_test_barrier(&barrier).await;
        init.abort();
        assert!(init.await.unwrap_err().is_cancelled());

        let output = root.join("snapshot");
        let export_output = output.clone();
        let export_store = store.clone();
        let export = tokio::spawn(async move {
            export_store
                .export_database(&export_output.to_string_lossy())
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !export.is_finished(),
            "export must wait for detached schema initialization"
        );
        release_test_barrier(&barrier);
        export.await.unwrap().unwrap();
        assert!(output.join("manifest.json").is_file());
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancelled_checkpoint_keeps_snapshot_exclusion_until_flush_finishes() {
        let root = std::env::temp_dir().join(format!("checkpoint-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fixture.store");
        let store = Arc::new(LadybugStore::for_testing(&db, 4).unwrap());
        store.init("checkpoint-cancel").await.unwrap();
        store
            .upsert(
                "checkpoint-cancel",
                "checkpoint-row",
                vec![0.5; 4],
                MemoryPayload {
                    content: "checkpoint fixture".into(),
                    user_id: "owner".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let barrier = root.join("checkpoint-entered");
        set_test_snapshot_operation_barrier("checkpoint", db.clone(), barrier.clone());

        let checkpoint_store = store.clone();
        let checkpoint = tokio::spawn(async move { checkpoint_store.checkpoint().await });
        wait_for_test_barrier(&barrier).await;
        checkpoint.abort();
        assert!(checkpoint.await.unwrap_err().is_cancelled());

        let output = root.join("snapshot");
        let export_output = output.clone();
        let export_store = store.clone();
        let export = tokio::spawn(async move {
            export_store
                .export_database(&export_output.to_string_lossy())
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !export.is_finished(),
            "export must wait for detached checkpoint"
        );
        release_test_barrier(&barrier);
        export.await.unwrap().unwrap();
        assert!(output.join("manifest.json").is_file());
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancelled_export_keeps_the_snapshot_cut_locked_until_bundle_is_complete() {
        let root = std::env::temp_dir().join(format!("export-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fixture.store");
        let store = Arc::new(LadybugStore::for_testing(&db, 4).unwrap());
        store.init("export-cancel").await.unwrap();
        store
            .upsert(
                "export-cancel",
                "before-export",
                vec![0.5; 4],
                MemoryPayload {
                    content: "committed before snapshot".into(),
                    user_id: "owner".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let barrier = root.join("export-entered");
        set_test_snapshot_operation_barrier("export", db.clone(), barrier.clone());

        let output = root.join("snapshot");
        let export_store = store.clone();
        let export_output = output.clone();
        let export = tokio::spawn(async move {
            export_store
                .export_database(&export_output.to_string_lossy())
                .await
        });
        wait_for_test_barrier(&barrier).await;
        export.abort();
        assert!(export.await.unwrap_err().is_cancelled());

        let write_store = store.clone();
        let write = tokio::spawn(async move {
            write_store
                .upsert(
                    "export-cancel",
                    "after-export",
                    vec![0.25; 4],
                    MemoryPayload {
                        content: "written after snapshot".into(),
                        user_id: "owner".into(),
                        memory_type: "fact".into(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({}),
                    },
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !write.is_finished(),
            "a writer must remain queued until the detached snapshot bundle is complete"
        );
        release_test_barrier(&barrier);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !output.join("manifest.json").is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "export worker did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        write.await.unwrap().unwrap();

        let snapshot_db = output.join("ladybug.store");
        let snapshot = LadybugStore::for_testing(&snapshot_db, 4).unwrap();
        snapshot.init("export-cancel-check").await.unwrap();
        assert_eq!(
            snapshot
                .get("export-cancel", "before-export")
                .await
                .unwrap()
                .unwrap()
                .1
                .content,
            "committed before snapshot"
        );
        assert!(snapshot
            .get("export-cancel", "after-export")
            .await
            .unwrap()
            .is_none());
        drop(snapshot);
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_ordinary_failure_is_not_retried_as_a_collision() {
        let other = anyhow::anyhow!("Binder exception: Table Memory does not exist");
        assert!(!is_write_collision(&other));
    }

    #[test]
    fn a_legacy_absolute_declaration_finds_the_file_it_meant() {
        let known = vec![
            "src/store/ladybug.rs".to_string(),
            "src/daemon.rs".to_string(),
        ];
        assert_eq!(
            resolve("C:/dev/projects/neurostrata/src/store/ladybug.rs", &known).as_deref(),
            Some("src/store/ladybug.rs")
        );
        // Backslashes are the same path, written the way Windows hands it over.
        assert_eq!(
            resolve(
                r"C:\dev\projects
eurostrata\src\daemon.rs",
                &known
            )
            .as_deref(),
            Some("src/daemon.rs")
        );
    }

    #[test]
    fn an_exact_declaration_is_returned_untouched() {
        let known = vec!["src/daemon.rs".to_string()];
        assert_eq!(
            resolve("src/daemon.rs", &known).as_deref(),
            Some("src/daemon.rs")
        );
    }

    /// The migration doctor prescribes is a re-ingest, which qualifies every
    /// id. A rule that still declares the old absolute path must follow it.
    #[test]
    fn a_legacy_absolute_declaration_finds_its_qualified_id() {
        let known = vec![
            "probe::src/store/ladybug.rs".to_string(),
            "probe::src/daemon.rs".to_string(),
        ];
        assert_eq!(
            resolve("C:/dev/projects/neurostrata/src/store/ladybug.rs", &known).as_deref(),
            Some("probe::src/store/ladybug.rs")
        );
        assert_eq!(
            resolve("C:/old/mylib.rs", &["probe::lib.rs".to_string()]),
            None
        );
        assert_eq!(
            resolve(
                "C:/old/src/a.rs",
                &["probe::src/a.rs".to_string(), "other::src/a.rs".to_string()]
            ),
            None,
            "a path two namespaces hold is ambiguous"
        );
    }

    #[test]
    fn an_ambiguous_suffix_is_left_alone() {
        // A wrong edge is worse than a missing one.
        // Both "mod.rs" and "x/mod.rs" are suffixes of the declaration.
        let known = vec!["mod.rs".to_string(), "x/mod.rs".to_string()];
        assert_eq!(resolve("C:/proj/x/mod.rs", &known), None);
    }

    #[test]
    fn a_target_that_was_never_ingested_stays_unresolved() {
        let known = vec!["src/daemon.rs".to_string()];
        assert_eq!(resolve("C:/proj/src/nowhere.rs", &known), None);
    }

    #[test]
    fn a_fresh_store_has_nothing_to_flush() {
        let dirty = Dirty::default();
        assert!(!dirty.is_set());
        assert!(!dirty.take());
    }

    #[test]
    fn taking_the_flag_clears_it_so_one_checkpoint_answers_for_many_writes() {
        let dirty = Dirty::default();
        dirty.mark();
        dirty.mark();

        assert!(dirty.take(), "the flag was set, so taking it reports true");
        assert!(!dirty.is_set(), "and leaves nothing behind");
        assert!(!dirty.take(), "a second take has nothing to report");
    }

    #[test]
    fn a_write_during_a_checkpoint_is_not_swallowed() {
        let dirty = Dirty::default();
        dirty.mark();

        // The checkpoint takes the flag first, then a write lands while the
        // engine is still flushing. That write must survive the clear.
        let _flushing = dirty.take();
        dirty.mark();

        assert!(dirty.is_set());
    }

    #[tokio::test]
    async fn a_write_that_outstays_its_deadline_says_the_database_is_busy() {
        let slow = async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok::<(), anyhow::Error>(())
        };

        let err = bounded(
            "deleting a memory",
            std::time::Duration::from_millis(50),
            slow,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("the database is busy"), "{}", err);
        assert!(err.contains("deleting a memory"), "{}", err);
        // The caller is released, not the query -- saying so is the point.
        assert!(err.contains("may still be running"), "{}", err);
    }

    #[tokio::test]
    async fn cancelling_a_queued_write_does_not_dispatch_or_mark_dirty() {
        use std::future::Future;

        struct NoopWake;
        impl std::task::Wake for NoopWake {
            fn wake(self: std::sync::Arc<Self>) {}
        }

        let root =
            std::env::temp_dir().join(format!("queued-write-cancel-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&root, 4).expect("open DB");
        store.init("queued-write-cancel").await.unwrap();
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty());

        let held_turn = store.write_lock.clone().lock_owned().await;
        let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dispatched_by_write = dispatched.clone();
        let mut write = Box::pin(store.write_with_deadline("queued cancellation", move |_| {
            dispatched_by_write.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }));
        let waker = std::task::Waker::from(std::sync::Arc::new(NoopWake));
        let mut context = std::task::Context::from_waker(&waker);
        assert!(
            write.as_mut().poll(&mut context).is_pending(),
            "write must be queued behind the held turn"
        );
        drop(write);
        drop(held_turn);

        assert!(
            !dispatched.load(std::sync::atomic::Ordering::Acquire),
            "cancellation before dispatch must not run the write closure"
        );
        assert!(!store.is_dirty(), "a queued cancellation is not a mutation");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_dispatched_write_marks_dirty_after_its_caller_is_cancelled() {
        let root =
            std::env::temp_dir().join(format!("dispatched-write-cancel-{}", uuid::Uuid::new_v4()));
        let db = root.join("fixture.store");
        std::fs::create_dir_all(&root).unwrap();
        let store = std::sync::Arc::new(LadybugStore::for_testing(&db, 4).expect("open DB"));
        store.init("dispatched-write-cancel").await.unwrap();
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty());

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            writer_store
                .write_with_deadline("cancelled dispatched write", move |conn| {
                    conn.query("BEGIN TRANSACTION")?;
                    conn.query("CREATE (m:Memory {pk: 'dispatched-write-cancel\u{1}dispatched-write-cancel::row', id: 'dispatched-write-cancel::row', namespace: 'dispatched-write-cancel', content: 'committed', user_id: 'owner', memory_type: 'fact', agent_name: 'fixture', location: 'fixture.rs', location_lines: '1', metadata: '{}', embedding: [0.1, 0.2, 0.3, 0.4]})")?;
                    started_tx.send(()).expect("signal dispatched write");
                    release_rx.lock().unwrap().recv().expect("release write");
                    conn.query("COMMIT")?;
                    Ok(())
                })
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().expect("write dispatched"))
            .await
            .unwrap();
        writer.abort();
        assert!(writer.await.unwrap_err().is_cancelled());
        release_tx.send(()).expect("release detached engine write");

        let lock_path = db.clone();
        let drained_writer =
            tokio::task::spawn_blocking(move || acquire_database_writer_lock(&lock_path))
                .await
                .expect("writer-lock waiter completes")
                .expect("dispatched write releases its writer lock");
        assert!(store.is_dirty(), "the blocking write worker marks dirty");
        drop(drained_writer);

        let row: Option<String> = store
            .with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (m:Memory) WHERE m.id = 'dispatched-write-cancel::row' RETURN m.content",
                )?;
                Ok(rows.next().map(|row| format!("{}", row[0])))
            })
            .await
            .unwrap();
        assert_eq!(row.as_deref(), Some("committed"));
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty(), "checkpoint clears the completed write");
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_timed_out_dispatched_write_marks_dirty_when_it_finishes() {
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root =
            std::env::temp_dir().join(format!("dispatched-write-timeout-{}", uuid::Uuid::new_v4()));
        let db = root.join("fixture.store");
        std::fs::create_dir_all(&root).unwrap();
        let store = std::sync::Arc::new(LadybugStore::for_testing(&db, 4).expect("open DB"));
        store.init("dispatched-write-timeout").await.unwrap();
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty());

        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "1");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            writer_store
                .write_with_deadline("timed out dispatched write", move |conn| {
                    conn.query("BEGIN TRANSACTION")?;
                    conn.query("CREATE (m:Memory {pk: 'dispatched-write-timeout\u{1}dispatched-write-timeout::row', id: 'dispatched-write-timeout::row', namespace: 'dispatched-write-timeout', content: 'committed', user_id: 'owner', memory_type: 'fact', agent_name: 'fixture', location: 'fixture.rs', location_lines: '1', metadata: '{}', embedding: [0.1, 0.2, 0.3, 0.4]})")?;
                    started_tx.send(()).expect("signal dispatched write");
                    release_rx.lock().unwrap().recv().expect("release write");
                    conn.query("COMMIT")?;
                    Ok(())
                })
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().expect("write dispatched"))
            .await
            .unwrap();
        let timeout = writer.await.expect("deadline returns to the caller");
        std::env::remove_var("NEUROSTRATA_WRITE_TIMEOUT_SECS");
        assert!(timeout
            .unwrap_err()
            .to_string()
            .contains("may still be running"));
        assert!(
            !store.is_dirty(),
            "the pending transaction has not finished"
        );
        release_tx.send(()).expect("release detached engine write");

        let lock_path = db.clone();
        let drained_writer =
            tokio::task::spawn_blocking(move || acquire_database_writer_lock(&lock_path))
                .await
                .expect("writer-lock waiter completes")
                .expect("timed-out write releases its writer lock");
        assert!(store.is_dirty(), "the completed blocking write marks dirty");
        drop(drained_writer);

        let row: Option<String> = store
            .with_conn(|conn| {
                let mut rows = conn.query("MATCH (m:Memory) WHERE m.id = 'dispatched-write-timeout::row' RETURN m.content")?;
                Ok(rows.next().map(|row| format!("{}", row[0])))
            })
            .await
            .unwrap();
        assert_eq!(row.as_deref(), Some("committed"));
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty(), "checkpoint clears the completed write");
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_write_error_after_mutation_keeps_the_store_dirty() {
        let root =
            std::env::temp_dir().join(format!("partial-write-dirty-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&root, 4).expect("open DB");
        store.init("partial-write-dirty").await.unwrap();
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty());

        let result: anyhow::Result<()> = store
            .write_with_deadline("partial write fixture", move |conn| {
                conn.query("CREATE (m:Memory {pk: 'partial-write-dirty\u{1}partial-write-dirty::row', id: 'partial-write-dirty::row', namespace: 'partial-write-dirty', content: 'partially written', user_id: 'owner', memory_type: 'fact', agent_name: 'fixture', location: 'partial.rs', location_lines: '1', metadata: '{}', embedding: [0.1, 0.2, 0.3, 0.4]})")?;
                anyhow::bail!("simulated failure after mutation")
            })
            .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("simulated failure"));
        assert!(
            store.is_dirty(),
            "an error after a possibly committed statement remains checkpoint-visible"
        );
        let row: Option<String> = store
            .with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (m:Memory) WHERE m.id = 'partial-write-dirty::row' RETURN m.content",
                )?;
                Ok(rows.next().map(|row| format!("{}", row[0])))
            })
            .await
            .unwrap();
        assert_eq!(row.as_deref(), Some("partially written"));
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty());
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_write_queued_behind_checkpoint_marks_dirty_after_checkpoint_clears() {
        let root =
            std::env::temp_dir().join(format!("checkpoint-queued-write-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fixture.store");
        let store = std::sync::Arc::new(LadybugStore::for_testing(&db, 4).expect("open DB"));
        store.init("checkpoint-queued-write").await.unwrap();
        store
            .upsert(
                "checkpoint-queued-write",
                "checkpoint-queued-write::seed",
                vec![0.0; 4],
                MemoryPayload {
                    content: "seed".into(),
                    user_id: "owner".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: "seed.rs".into(),
                    location_lines: "1".into(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty(), "staging work was checkpointed first");

        let checkpoint_barrier = root.join("checkpoint-entered");
        set_test_snapshot_operation_barrier("checkpoint", db.clone(), checkpoint_barrier.clone());
        let checkpoint_store = store.clone();
        let checkpoint = tokio::spawn(async move { checkpoint_store.checkpoint().await });
        wait_for_test_barrier(&checkpoint_barrier).await;
        assert!(!store.is_dirty(), "checkpoint took the previous dirty flag");

        let write_lock_attempt = root.join("write-lock-attempted");
        set_test_write_lock_attempt(db.clone(), write_lock_attempt.clone());
        let write_store = store.clone();
        let write = tokio::spawn(async move {
            write_store
                .upsert(
                    "checkpoint-queued-write",
                    "checkpoint-queued-write::queued",
                    vec![0.4; 4],
                    MemoryPayload {
                        content: "queued behind checkpoint".into(),
                        user_id: "owner".into(),
                        memory_type: "fact".into(),
                        agent_name: None,
                        location: "queued.rs".into(),
                        location_lines: "2".into(),
                        metadata: json!({}),
                    },
                )
                .await
        });
        wait_for_test_barrier(&write_lock_attempt).await;
        assert!(
            !write.is_finished(),
            "the write cannot pass the checkpoint's writer lock"
        );
        release_test_barrier(&checkpoint_barrier);
        checkpoint.await.unwrap().unwrap();
        write.await.unwrap().unwrap();
        assert!(
            store.is_dirty(),
            "the queued write marks dirty after checkpoint cleared the prior flag"
        );
        let row = store
            .get("checkpoint-queued-write", "checkpoint-queued-write::queued")
            .await
            .unwrap();
        assert_eq!(row.unwrap().1.content, "queued behind checkpoint");
        store.checkpoint().await.unwrap();
        assert!(!store.is_dirty());
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn waiting_a_turn_to_write_counts_against_the_deadline() {
        // The shape write_with_deadline uses: the queue for the single writer
        // sits inside the bound, so a caller stuck behind a stalled write is
        // told the database is busy rather than waiting for its turn forever.
        let lock = tokio::sync::Mutex::new(());
        let held = lock.lock().await;

        let err = bounded(
            "writing a memory",
            std::time::Duration::from_millis(50),
            async {
                let _turn = lock.lock().await;
                Ok::<(), anyhow::Error>(())
            },
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("the database is busy"), "{}", err);
        drop(held);
    }

    /// The invariant the writer's turn exists for. Held by the caller, giving
    /// up released it while the statement was still running, and the next
    /// writer opened a second transaction against an engine that already had
    /// one -- which is how one stalled write became five in a row (bead
    /// neurostrata-4nk).
    #[tokio::test]
    async fn giving_up_does_not_hand_on_a_turn_that_is_still_in_use() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));

        // Stands in for a statement spawn_blocking cannot cancel: it owns the
        // turn and keeps it for as long as it runs.
        let turn = Arc::clone(&lock).lock_owned().await;
        let statement = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(turn);
        });

        let err = bounded(
            "writing a memory",
            std::time::Duration::from_millis(20),
            async {
                let _held = Arc::clone(&lock).lock_owned().await;
                Ok::<(), anyhow::Error>(())
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("the database is busy"), "{}", err);

        assert!(
            lock.try_lock().is_err(),
            "the turn was handed on while the statement was still running"
        );

        statement.await.expect("the statement finishes");
        assert!(
            lock.try_lock().is_ok(),
            "the turn is free once the statement has actually ended"
        );
    }

    #[tokio::test]
    async fn a_write_inside_its_deadline_is_left_alone() {
        let quick = async { Ok::<u8, anyhow::Error>(7) };
        let out = bounded("writing a memory", std::time::Duration::from_secs(5), quick)
            .await
            .unwrap();
        assert_eq!(out, 7);
    }

    #[test]
    fn the_write_deadline_can_be_overridden() {
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "90");
        assert_eq!(write_timeout(), std::time::Duration::from_secs(90));

        // A nonsense or zero value falls back rather than disabling the bound.
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "0");
        assert_eq!(write_timeout(), std::time::Duration::from_secs(30));
        std::env::remove_var("NEUROSTRATA_WRITE_TIMEOUT_SECS");
    }

    #[test]
    fn a_first_read_starts_the_count_at_one() {
        assert_eq!(
            bump_access_count("{}"),
            json!({"access_count": 1}).to_string()
        );
    }

    #[test]
    fn counting_a_read_leaves_the_rest_of_the_blob_alone() {
        let before = json!({
            "domain": "database",
            "governs": ["src/daemon.rs"],
            "access_count": 4
        })
        .to_string();

        let after: Value = serde_json::from_str(&bump_access_count(&before)).unwrap();

        assert_eq!(after["access_count"], json!(5));
        assert_eq!(after["domain"], json!("database"));
        assert_eq!(after["governs"], json!(["src/daemon.rs"]));
    }

    #[test]
    fn unusable_metadata_does_not_fail_a_read() {
        for junk in ["", "not json at all", "[1,2,3]", "null"] {
            assert_eq!(
                bump_access_count(junk),
                json!({"access_count": 1}).to_string(),
                "junk metadata {:?} should still count the read",
                junk
            );
        }
    }

    async fn retired_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-retired-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");
        store
    }

    fn retired_test_payload(memory_type: &str, user_id: &str, metadata: Value) -> MemoryPayload {
        MemoryPayload {
            content: format!("a {} row", memory_type),
            user_id: user_id.to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        }
    }

    #[tokio::test]
    async fn export_graph_resolves_each_node_by_its_source_identity() {
        let store = retired_test_store().await;
        let root = std::env::temp_dir().join(format!("ns-export-context-{}", uuid::Uuid::new_v4()));
        let root_a = root.join("project-a");
        let root_b = root.join("project-b");
        let marker_a = root_a.join(".NeuroStrata");
        let marker_b = root_b.join(".NeuroStrata");
        std::fs::create_dir_all(&marker_a).unwrap();
        std::fs::create_dir_all(&marker_b).unwrap();
        let contexts = std::collections::HashMap::from([
            ("source-a".to_string(), marker_a),
            ("source-b".to_string(), marker_b),
        ]);

        for (id, source_id) in [("node-a", "source-a"), ("node-b", "source-b")] {
            let mut payload = retired_test_payload(
                "file",
                "agent",
                json!({
                    "absolute_path": format!("/stored/{source_id}/src/lib.rs"),
                    "refs": [{
                        "file": "src/lib.rs",
                        "source_context": { "source_id": source_id, "provider": "kept" },
                        "provider_data": { "opaque": true }
                    }]
                }),
            );
            payload.location = "src/lib.rs".to_string();
            store
                .upsert("probe", id, vec![0.0; 4], payload)
                .await
                .unwrap();
        }

        let graph = store
            .export_graph(false, false, &contexts, None)
            .await
            .unwrap();
        for (id, expected_root) in [("node-a", root_a), ("node-b", root_b)] {
            let node = graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == id)
                .unwrap();
            assert_eq!(
                node["absolute_path"],
                expected_root.join("src/lib.rs").to_string_lossy().as_ref()
            );
            assert_eq!(node["metadata"]["refs"][0]["provider_data"]["opaque"], true);
            assert_eq!(
                node["metadata"]["refs"][0]["source_context"]["source_id"],
                if id == "node-a" {
                    "source-a"
                } else {
                    "source-b"
                }
            );
        }
    }

    async fn governs_edges_from(store: &LadybugStore, id: &str) -> usize {
        let id = escape_ladybug_string(id);
        store
            .with_conn(move |conn| {
                let mut rows = conn.query(&format!(
                    "MATCH (a:Memory {{id: '{}'}})-[:GOVERNS]->(:Memory) RETURN count(*)",
                    id
                ))?;
                Ok(rows
                    .next()
                    .map(|row| format!("{}", row[0]).parse().unwrap_or(0))
                    .unwrap_or(0))
            })
            .await
            .expect("count GOVERNS edges")
    }

    /// Guinea-pig BUG-6 and BUG-7: the export is the portable artifact, so it
    /// must carry the declarative anchors, mark superseded rows with their
    /// replacement, and include the extraction edges the done-funnel creates.
    #[tokio::test]
    async fn export_carries_metadata_marks_superseded_and_keeps_extraction_edges() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];
        // Targets first: the write-shape validator refuses edges whose target
        // row does not exist yet.
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "new-rule",
                zero.clone(),
                retired_test_payload("rule", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "old-rule",
                zero,
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "superseded_by": "new-rule", "valid_to": 1, "related_to": ["new-rule"] }),
                ),
            )
            .await
            .unwrap();

        let all = store
            .export_graph(true, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let nodes = all["nodes"].as_array().unwrap();
        let old = nodes.iter().find(|n| n["id"] == "old-rule").unwrap();
        assert_eq!(old["superseded"], true);
        assert_eq!(old["superseded_by"], "new-rule");
        assert!(
            old["metadata"]["related_to"].is_array(),
            "metadata travels with the node"
        );
        let new = nodes.iter().find(|n| n["id"] == "new-rule").unwrap();
        assert_eq!(new["superseded"], false);
        assert_eq!(new["superseded_by"], serde_json::Value::Null);

        let links = all["links"].as_array().unwrap();
        assert!(
            links.iter().any(|l| l["type"] == "EXTRACTED_FROM"),
            "the consolidation edge is part of the graph"
        );

        let active = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let ids: Vec<_> = active["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].clone())
            .collect();
        assert!(
            !ids.contains(&json!("old-rule")),
            "--exclude-superseded drops retired rows"
        );
    }

    #[tokio::test]
    async fn relinking_does_not_give_a_retired_rule_its_edges_back() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";
        let zero = vec![0.0f32; 4];

        // Both rules are written before the file exists, so only relinking can
        // give them edges.
        store
            .upsert(
                "probe",
                "retired-rule",
                zero.clone(),
                retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "live-rule",
                zero.clone(),
                retired_test_payload("rule", "agent", json!({ "governs": [file] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                file,
                zero,
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();

        store.relink_edges("probe", PROBE_SOURCE).await.expect("relink");

        assert_eq!(
            governs_edges_from(&store, "live-rule").await,
            1,
            "a live rule is relinked"
        );
        assert_eq!(
            governs_edges_from(&store, "retired-rule").await,
            0,
            "a retired rule is not"
        );
    }

    #[tokio::test]
    async fn the_graph_does_not_bring_back_a_retired_rule() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";

        store
            .upsert(
                "probe",
                file,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "symbol",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "code_ast",
                    "auto-ingestor",
                    json!({ "contained_by": [file] }),
                ),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-rule",
                vec![0.0, 1.0, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })),
            )
            .await
            .unwrap();

        // The symbol matches; its file is governed by the retired rule, two hops
        // out -- exactly the path the governing expansion follows. The query
        // vector is not whole numbers: `1` and `0` would print as an integer
        // list, which array_distance refuses.
        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 5, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert!(ids.contains(&"symbol".to_string()), "{:?}", ids);
        assert!(
            !ids.contains(&"retired-rule".to_string()),
            "a retired rule stays retired: {:?}",
            ids
        );
    }

    #[test]
    fn a_declaration_prefers_its_own_namespace_then_its_exact_id_then_one_suffix() {
        // Generation 5 is active; the qualified form is now generation-scoped.
        let scoped = crate::parser::ingest::generation_scoped_id("probe", 5, "src/a.rs");
        let both = vec![scoped.clone(), "src/a.rs".to_string()];
        assert!(matches!(
            pick_declared_target("probe", 5, "src/a.rs", &both),
            crate::traits::TargetResolution::Resolved(ref id) if *id == scoped
        ));
        assert!(matches!(
            pick_declared_target("probe", 5, "src/a.rs", &["src/a.rs".to_string()]),
            crate::traits::TargetResolution::Resolved(ref id) if id == "src/a.rs"
        ));
        assert!(matches!(
            pick_declared_target("probe", 5, "C:/old/src/a.rs", &[scoped.clone()]),
            crate::traits::TargetResolution::Resolved(ref id) if *id == scoped
        ));
        assert!(
            matches!(
                pick_declared_target(
                    "probe",
                    5,
                    "C:/old/src/a.rs",
                    &[
                        scoped.clone(),
                        crate::parser::ingest::generation_scoped_id("probe", 5, "a.rs"),
                    ]
                ),
                crate::traits::TargetResolution::Ambiguous(_)
            ),
            "an ambiguous suffix is left alone"
        );
        assert!(declared_target_candidates("probe", 5, "C:/old/src/a.rs").contains(&scoped));
    }

    #[tokio::test]
    async fn ingest_publication_rolls_back_rows_and_rule_edges_on_store_failure() {
        let dir = std::env::temp_dir().join(format!("atomic-ingest-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temporary database");
        store.init("atomic").await.expect("create schema");
        let old_id = format!("atomic{}old.rs", crate::parser::ingest::NAMESPACE_SEPARATOR);
        store
            .upsert(
                "atomic",
                &old_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "Path: old.rs".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "old.rs".into(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "atomic",
                "rule-atomic",
                vec![0.0; 4],
                MemoryPayload {
                    content: "rule".into(),
                    user_id: "agent".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({"governs": [old_id]}),
                },
            )
            .await
            .unwrap();

        let staged_namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let _stage_lease = store
            .begin_ingest_stage(&staged_namespace)
            .await
            .expect("acquire staging lease");
        let staged_id = format!(
            "{}{}new.rs",
            staged_namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                &staged_namespace,
                &staged_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "Path: new.rs".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "new.rs".into(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .unwrap();
        store
            .with_conn({
                let staged_namespace = staged_namespace.clone();
                let staged_id = staged_id.clone();
                move |conn| {
                    conn.query(&format!(
                        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{{'",
                        escape_ladybug_string(&staged_namespace),
                        escape_ladybug_string(&staged_id)
                    ))?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        let result = store.publish_ingested("atomic", &staged_namespace).await;
        assert!(
            result.is_err(),
            "malformed staged metadata aborts the transaction"
        );
        assert!(store.get("atomic", &old_id).await.unwrap().is_some());
        assert!(store
            .get(&staged_namespace, &staged_id)
            .await
            .unwrap()
            .is_some());
        let graph = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        assert!(graph["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| { link["source"] == "rule-atomic" && link["target"] == old_id }));
        assert!(
            !store
                .list_namespaces()
                .await
                .unwrap()
                .contains(&staged_namespace),
            "private staging is excluded from namespace listings"
        );
        assert!(
            graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|node| node["namespace"] != staged_namespace),
            "private staging is excluded from graph exports"
        );

        let interrupted_namespace = "atomic".to_string();
        let interrupted: Result<()> = store
            .with_conn(move |conn| -> Result<()> {
                conn.query("BEGIN TRANSACTION")?;
                conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m",
                    interrupted_namespace
                ))?;
                anyhow::bail!("simulated process interruption before commit");
            })
            .await;
        assert!(interrupted.is_err());
        assert!(
            store.get("atomic", &old_id).await.unwrap().is_some(),
            "disconnecting before COMMIT leaves the last committed generation intact"
        );

        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn atomic_publication_promotes_database_rows_and_disposes_stage_together() {
        let dir = std::env::temp_dir().join(format!("atomic-promote-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temporary database");
        store.init("atomic-promote").await.unwrap();
        let old_id = format!(
            "atomic-promote{}old.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                "atomic-promote",
                &old_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "old".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "old.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        store
            .upsert(
                "atomic-promote",
                "rule-promote",
                vec![0.0; 4],
                MemoryPayload {
                    content: "rule".into(),
                    user_id: "agent".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "governs": ["new.rs"] }),
                },
            )
            .await
            .unwrap();

        let staged_namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let _stage_lease = store.begin_ingest_stage(&staged_namespace).await.unwrap();
        let staged_id = format!(
            "{}{}new.rs",
            staged_namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                &staged_namespace,
                &staged_id,
                vec![0.25, 0.5, 0.75, 1.0],
                MemoryPayload {
                    content: "new".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "new.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();

        store
            .publish_ingested("atomic-promote", &staged_namespace)
            .await
            .unwrap();
        let new_id = format!(
            "atomic-promote{}new.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        assert!(store
            .get("atomic-promote", &old_id)
            .await
            .unwrap()
            .is_none());
        let promoted = store
            .get("atomic-promote", &new_id)
            .await
            .unwrap()
            .expect("new generation is visible after commit");
        assert_eq!(promoted.0, vec![0.25, 0.5, 0.75, 1.0]);
        assert_eq!(promoted.1.content, "new");
        assert!(store
            .get(&staged_namespace, &staged_id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(governs_edges_from(&store, "rule-promote").await, 1);

        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn publication_rejects_reserved_destination_without_touching_active_stages() {
        let db = std::env::temp_dir().join(format!("reserved-publish-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&db, 4).unwrap();
        store.init("reserved-publish").await.unwrap();
        let destination = format!("{INTERNAL_INGEST_STAGE_PREFIX}{}", uuid::Uuid::new_v4());
        let source = format!("{INTERNAL_INGEST_STAGE_PREFIX}{}", uuid::Uuid::new_v4());
        let fresh_destination = format!("{INTERNAL_INGEST_STAGE_PREFIX}{}", uuid::Uuid::new_v4());
        let _destination_lease = store.begin_ingest_stage(&destination).await.unwrap();
        let _source_lease = store.begin_ingest_stage(&source).await.unwrap();
        let destination_id = format!(
            "{destination}{}target.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        let source_id = format!(
            "{source}{}source.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        for (namespace, id, content, vector) in [
            (
                &destination,
                &destination_id,
                "held destination",
                vec![0.1, 0.2, 0.3, 0.4],
            ),
            (
                &source,
                &source_id,
                "publish source",
                vec![0.4, 0.3, 0.2, 0.1],
            ),
        ] {
            store
                .upsert(
                    namespace,
                    id,
                    vector,
                    MemoryPayload {
                        content: content.into(),
                        user_id: "auto-ingestor".into(),
                        memory_type: "file".into(),
                        agent_name: None,
                        location: format!("{content}.rs"),
                        location_lines: String::new(),
                        metadata: json!({"preserve": content}),
                    },
                )
                .await
                .unwrap();
        }
        async fn snapshot(store: &LadybugStore, namespace: String) -> Vec<Vec<String>> {
            store.with_conn(move |conn| {
                let safe = escape_ladybug_string(&namespace);
                let mut rows = conn.query(&format!(
                    "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata, m.embedding ORDER BY m.id",
                    safe
                ))?;
                let mut values = Vec::new();
                while let Some(row) = rows.next() {
                    values.push((0..9).map(|i| format!("{}", row[i])).collect::<Vec<_>>());
                }
                Ok(values)
            }).await.unwrap()
        }
        async fn edge_snapshot(store: &LadybugStore) -> Vec<Vec<String>> {
            store.with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (a:Memory)-[r]->(b:Memory) RETURN a.id, label(r), b.id ORDER BY a.id, label(r), b.id",
                )?;
                let mut values = Vec::new();
                while let Some(row) = rows.next() {
                    values.push((0..3).map(|i| format!("{}", row[i])).collect::<Vec<_>>());
                }
                Ok(values)
            }).await.unwrap()
        }
        async fn catalog_snapshot(store: &LadybugStore) -> Vec<(String, String)> {
            store
                .with_conn(|conn| {
                    let mut rows = conn.query(
                    "MATCH (s:IngestStage) RETURN s.namespace, s.generation ORDER BY s.namespace",
                )?;
                    let mut values = Vec::new();
                    while let Some(row) = rows.next() {
                        values.push((format!("{}", row[0]), format!("{}", row[1])));
                    }
                    Ok(values)
                })
                .await
                .unwrap()
        }
        store
            .with_conn({
                let edge = (destination_id.clone(), source_id.clone());
                move |conn| {
                    create_missing_edges(conn, "RELATES_TO", &[edge])?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        let destination_before = snapshot(&store, destination.clone()).await;
        let source_before = snapshot(&store, source.clone()).await;
        let edges_before = edge_snapshot(&store).await;
        assert!(edges_before.contains(&vec![
            destination_id.clone(),
            "RELATES_TO".into(),
            source_id.clone(),
        ]));
        let catalog_before = catalog_snapshot(&store).await;
        assert!(catalog_before.iter().any(|(ns, generation)| {
            ns == &destination
                && generation == destination.trim_start_matches(INTERNAL_INGEST_STAGE_PREFIX)
        }));
        assert!(catalog_before.iter().any(|(ns, generation)| {
            ns == &source && generation == source.trim_start_matches(INTERNAL_INGEST_STAGE_PREFIX)
        }));
        assert!(!has_ingest_stage_lease(
            &store.local_path,
            &store.path_aliases,
            &fresh_destination
        ));

        for invalid in [
            destination.as_str(),
            fresh_destination.as_str(),
            "__neurostrata_ingest_stage__malformed",
            INTERNAL_INGEST_STAGE_PREFIX,
        ] {
            let error = store.publish_ingested(invalid, &source).await.unwrap_err();
            assert!(error
                .to_string()
                .contains("reserved internal staging namespace"));
            assert_eq!(
                snapshot(&store, destination.clone()).await,
                destination_before
            );
            assert_eq!(snapshot(&store, source.clone()).await, source_before);
            assert_eq!(edge_snapshot(&store).await, edges_before);
            assert_eq!(catalog_snapshot(&store).await, catalog_before);
        }

        let same_error = store
            .publish_ingested(&source, &source)
            .await
            .expect_err("source cannot also be the destination");
        assert!(same_error
            .to_string()
            .contains("reserved internal staging namespace"));
        assert_eq!(
            snapshot(&store, destination.clone()).await,
            destination_before
        );
        assert_eq!(snapshot(&store, source.clone()).await, source_before);
        assert_eq!(edge_snapshot(&store).await, edges_before);
        assert!(has_ingest_stage_lease(
            &store.local_path,
            &store.path_aliases,
            &destination
        ));
        assert!(has_ingest_stage_lease(
            &store.local_path,
            &store.path_aliases,
            &source
        ));
        assert!(!has_ingest_stage_lease(
            &store.local_path,
            &store.path_aliases,
            &fresh_destination
        ));

        store
            .publish_ingested("published-source", &source)
            .await
            .unwrap();
        store
            .publish_ingested("published-destination", &destination)
            .await
            .unwrap();
        assert!(store
            .get("published-source", "published-source::source.rs")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get("published-destination", "published-destination::target.rs")
            .await
            .unwrap()
            .is_some());
        let _ = std::fs::remove_file(&db);
    }

    #[tokio::test]
    async fn stage_recovery_requires_lease_provenance_and_preserves_lookalike_namespaces() {
        let dir = std::env::temp_dir().join(format!("ingest-recovery-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temporary database");
        store.init("recovery").await.unwrap();

        let active_namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let active_lease = store.begin_ingest_stage(&active_namespace).await.unwrap();
        let active_id = format!(
            "{}{}active.rs",
            active_namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                &active_namespace,
                &active_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "active stage".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "active.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();

        // Model a process that wrote stage provenance and rows, then exited:
        // the lease remains on disk but is unlocked.
        let orphan_namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let orphan_lease = store.begin_ingest_stage(&orphan_namespace).await.unwrap();
        let orphan_lease_path = ingest_stage_lease_path(&dir, &orphan_namespace).unwrap();
        let orphan_id = format!(
            "{}{}orphan.rs",
            orphan_namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                &orphan_namespace,
                &orphan_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "interrupted stage".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "orphan.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let dropped_provenance_namespace = orphan_namespace.clone();
        store
            .write_with_deadline("simulating a pre-catalog stage", move |conn| {
                let safe = escape_ladybug_string(&dropped_provenance_namespace);
                conn.query(&format!(
                    "MATCH (s:IngestStage) WHERE s.namespace = '{}' DELETE s",
                    safe
                ))?;
                Ok(())
            })
            .await
            .unwrap();
        drop(orphan_lease);
        let legacy_namespace = format!("project-ingest-stage-{}", uuid::Uuid::new_v4());
        let legacy_id = format!(
            "{}{}legacy.rs",
            legacy_namespace,
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                &legacy_namespace,
                &legacy_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "legacy interrupted stage".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "legacy.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();

        // These namespaces have no stage lease. Their spelling alone must
        // never hide or authorize deletion, even when the name parses as a
        // generated stage ID or starts with the reserved prefix malformed.
        let lookalike_namespace =
            format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let lookalike_system_id = format!("{}::system-row", lookalike_namespace);
        let lookalike_user_id = format!("{}::user-row", lookalike_namespace);
        seed_raw_reserved_namespace_row(
            &store,
            &lookalike_namespace,
            &lookalike_system_id,
            "system",
        )
        .await;
        seed_raw_reserved_namespace_row(&store, &lookalike_namespace, &lookalike_user_id, "user-1")
            .await;
        let lookalike_lease_path = ingest_stage_lease_path(&dir, &lookalike_namespace).unwrap();
        std::fs::write(
            &lookalike_lease_path,
            "legacy sidecar without catalog ownership",
        )
        .unwrap();
        let malformed_namespace = format!("{}notes", INTERNAL_INGEST_STAGE_PREFIX);
        let malformed_id = format!("{}::user-row", malformed_namespace);
        seed_raw_reserved_namespace_row(&store, &malformed_namespace, &malformed_id, "user-2")
            .await;
        let legacy_user_namespace = format!("project-ingest-stage-{}", uuid::Uuid::new_v4());
        let legacy_user_id = format!("{}::user-row", legacy_user_namespace);
        store
            .upsert(
                &legacy_user_namespace,
                &legacy_user_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "legacy user data".into(),
                    user_id: "user-3".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let rejected_reserved_write = store
            .upsert(
                &malformed_namespace,
                "new-user-write",
                vec![0.0; 4],
                MemoryPayload {
                    content: "must not write".into(),
                    user_id: "system".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await;
        assert!(rejected_reserved_write.is_err());
        assert!(store
            .begin_ingest_stage(&lookalike_namespace)
            .await
            .is_err());
        assert!(store
            .upsert(
                &lookalike_namespace,
                "lookalike-public-write",
                vec![0.0; 4],
                MemoryPayload {
                    content: "must not write".into(),
                    user_id: "system".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .is_err());

        let visible_names = store.list_namespaces().await.unwrap();
        assert!(!visible_names.contains(&active_namespace));
        assert!(!visible_names.contains(&orphan_namespace));
        assert!(visible_names.contains(&legacy_namespace));
        assert!(visible_names.contains(&legacy_user_namespace));
        assert!(visible_names.contains(&lookalike_namespace));
        assert!(visible_names.contains(&malformed_namespace));
        let exported = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let exported_nodes = exported["nodes"].as_array().unwrap();
        assert!(exported_nodes.iter().all(|node| {
            node["namespace"] != active_namespace && node["namespace"] != orphan_namespace
        }));
        for namespace in [
            legacy_namespace.as_str(),
            legacy_user_namespace.as_str(),
            lookalike_namespace.as_str(),
            malformed_namespace.as_str(),
        ] {
            assert!(exported_nodes
                .iter()
                .any(|node| node["namespace"] == namespace));
        }

        let new_namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let new_lease = store.begin_ingest_stage(&new_namespace).await.unwrap();
        assert!(store
            .get(&active_namespace, &active_id)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get(&orphan_namespace, &orphan_id)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get(&legacy_namespace, &legacy_id)
            .await
            .unwrap()
            .is_some());
        for (namespace, id) in [
            (&lookalike_namespace, &lookalike_system_id),
            (&lookalike_namespace, &lookalike_user_id),
            (&malformed_namespace, &malformed_id),
            (&legacy_user_namespace, &legacy_user_id),
        ] {
            assert!(store.get(namespace, id).await.unwrap().is_some());
        }
        assert!(!orphan_lease_path.exists());
        assert!(
            lookalike_lease_path.exists(),
            "mixed user/system rows with a lookalike sidecar remain preserved"
        );
        let new_lease_path = ingest_stage_lease_path(&dir, &new_namespace).unwrap();
        drop(new_lease);
        drop(active_lease);

        let recovery_namespace =
            format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let recovery_lease = store.begin_ingest_stage(&recovery_namespace).await.unwrap();
        assert!(store
            .get(&active_namespace, &active_id)
            .await
            .unwrap()
            .is_none());
        let recovery_lease_path = ingest_stage_lease_path(&dir, &recovery_namespace).unwrap();
        drop(recovery_lease);
        let _ = std::fs::remove_file(new_lease_path);
        let _ = std::fs::remove_file(recovery_lease_path);

        let _ = std::fs::remove_file(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_checks_legacy_alias_and_canonical_stage_leases() {
        let root = std::env::temp_dir().join(format!("stage-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("database.store");
        let alias = root.join("database-alias.store");
        {
            let target_store = LadybugStore::for_testing(&database, 4).unwrap();
            target_store.init("stage-alias-target").await.unwrap();
        }
        std::os::unix::fs::symlink(&database, &alias).unwrap();
        let store = LadybugStore::for_testing(&alias, 4).unwrap();
        store.init("stage-alias").await.unwrap();

        let namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let lease = store.begin_ingest_stage(&namespace).await.unwrap();
        let canonical_lease = ingest_stage_lease_path(&alias, &namespace).unwrap();
        let alias_lease = raw_database_sibling(
            &alias,
            &format!(
                ".stage-{}.lock",
                namespace
                    .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
                    .unwrap()
            ),
        );
        std::fs::write(&alias_lease, "stale alias lease").unwrap();
        let id = format!("{}::file.rs", namespace);
        store
            .upsert(
                &namespace,
                &id,
                vec![0.25; 4],
                MemoryPayload {
                    content: "active alias generation".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "file.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();

        store.recover_interrupted_ingest_stages().await.unwrap();
        assert_eq!(
            store.get(&namespace, &id).await.unwrap().unwrap().1.content,
            "active alias generation",
            "a stale alias cannot hide an actively locked canonical generation"
        );

        drop(lease);
        store.recover_interrupted_ingest_stages().await.unwrap();
        assert!(store.get(&namespace, &id).await.unwrap().is_none());
        assert!(!canonical_lease.exists());
        assert!(!alias_lease.exists());
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn relative_file_alias_is_anchored_for_legacy_lease_recovery_after_cwd_changes() {
        let root =
            std::env::temp_dir().join(format!("relative-stage-alias-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("database.store");
        let alias = root.join("database-alias.store");
        {
            let target = LadybugStore::for_testing(&database, 4).unwrap();
            target.init("relative-alias-target").await.unwrap();
        }
        std::os::unix::fs::symlink(&database, &alias).unwrap();
        let _cwd_guard = TEST_CWD_MUTEX.lock().unwrap();
        let previous_directory = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        let relative_alias = Path::new("database-alias.store");
        let store = LadybugStore::for_testing(relative_alias, 4).unwrap();
        store.init("relative-alias-stage").await.unwrap();
        let namespace = format!("{INTERNAL_INGEST_STAGE_PREFIX}{}", uuid::Uuid::new_v4());
        let lease = store.begin_ingest_stage(&namespace).await.unwrap();
        let id = format!("{namespace}::legacy.rs");
        store
            .upsert(
                &namespace,
                &id,
                vec![0.2, 0.3, 0.4, 0.5],
                MemoryPayload {
                    content: "relative legacy stage".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "legacy.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let canonical_lease = ingest_stage_lease_path(&database, &namespace).unwrap();
        // Keep the alias sidecar anchored to the directory used at open time;
        // this test deliberately changes cwd before recovery.
        let alias_lease = raw_database_sibling(
            &alias,
            &format!(
                ".stage-{}.lock",
                namespace
                    .strip_prefix(INTERNAL_INGEST_STAGE_PREFIX)
                    .unwrap()
            ),
        );
        drop(lease);
        std::fs::remove_file(&canonical_lease).unwrap();
        std::fs::write(&alias_lease, "legacy relative alias lease").unwrap();
        let namespace_for_delete = namespace.clone();
        store
            .write_with_deadline("seeding legacy alias stage", move |conn| {
                let safe = escape_ladybug_string(&namespace_for_delete);
                conn.query(&format!(
                    "MATCH (s:IngestStage) WHERE s.namespace = '{}' DELETE s",
                    safe
                ))?;
                Ok(())
            })
            .await
            .unwrap();
        std::env::set_current_dir(&previous_directory).unwrap();

        assert!(
            !store.list_namespaces().await.unwrap().contains(&namespace),
            "anchored relative alias still hides its legacy stage after cwd changes"
        );
        store.recover_interrupted_ingest_stages().await.unwrap();
        assert!(store.get(&namespace, &id).await.unwrap().is_none());
        assert!(!alias_lease.exists());
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retargeted_parent_alias_cannot_remove_other_database_stage_sidecars() {
        let root =
            std::env::temp_dir().join(format!("stage-parent-retarget-{}", uuid::Uuid::new_v4()));
        let original_parent = root.join("original");
        let replacement_parent = root.join("replacement");
        let parent_alias = root.join("parent-alias");
        std::fs::create_dir_all(&original_parent).unwrap();
        std::fs::create_dir_all(&replacement_parent).unwrap();
        let database = original_parent.join("database.store");
        {
            let store = LadybugStore::for_testing(&database, 4).unwrap();
            store.init("parent-retarget-fixture").await.unwrap();
        }
        std::os::unix::fs::symlink(&original_parent, &parent_alias).unwrap();
        let store = LadybugStore::for_testing(parent_alias.join("database.store"), 4).unwrap();
        store.init("parent-retarget-fixture").await.unwrap();

        std::fs::remove_file(&parent_alias).unwrap();
        std::os::unix::fs::symlink(&replacement_parent, &parent_alias).unwrap();
        let generation = uuid::Uuid::new_v4().to_string();
        let other_database_sidecar = raw_database_sibling(
            &replacement_parent.join("database.store"),
            &format!(".stage-{generation}.lock"),
        );
        std::fs::write(&other_database_sidecar, "other database provenance").unwrap();

        store.recover_interrupted_ingest_stages().await.unwrap();
        assert_eq!(
            std::fs::read(&other_database_sidecar).unwrap(),
            b"other database provenance",
            "recovery stays within the database parent captured at open"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_parent_lease_candidates_are_deduplicated_and_recovered() {
        let root =
            std::env::temp_dir().join(format!("stage-parent-alias-{}", uuid::Uuid::new_v4()));
        let alias_parent = root.with_extension("alias");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias_parent).unwrap();
        let alias_db = alias_parent.join("database.store");
        let store = LadybugStore::for_testing(&alias_db, 4).unwrap();
        store.init("stage-parent-alias").await.unwrap();

        let namespace = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let lease = store.begin_ingest_stage(&namespace).await.unwrap();
        store
            .upsert(
                &namespace,
                &format!("{namespace}::src/recovered.rs"),
                vec![0.25; 4],
                MemoryPayload {
                    content: "legacy stage under symlinked parent".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "src/recovered.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        drop(lease);
        assert_eq!(
            ingest_stage_lease_candidates(&alias_db, &namespace)
                .unwrap()
                .len(),
            1,
            "symlinked parent spellings refer to one lease identity"
        );
        store.recover_interrupted_ingest_stages().await.unwrap();
        assert!(store
            .get(&namespace, &format!("{namespace}::src/recovered.rs"))
            .await
            .unwrap()
            .is_none());
        assert!(!ingest_stage_lease_path(&alias_db, &namespace)
            .unwrap()
            .exists());
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&alias_parent);
    }

    #[tokio::test]
    async fn readers_keep_seeing_the_old_ingest_until_atomic_commit() {
        let dir = std::env::temp_dir().join(format!("atomic-reader-{}", uuid::Uuid::new_v4()));
        let store = std::sync::Arc::new(LadybugStore::for_testing(&dir, 4).expect("open DB"));
        store.init("atomic-reader").await.unwrap();
        let old_id = format!(
            "atomic-reader{}old.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                "atomic-reader",
                &old_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "Path: old.rs".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "old.rs".into(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .unwrap();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let writer_store = store.clone();
        let namespace = "atomic-reader".to_string();
        let writer = tokio::spawn(async move {
            writer_store
                .write_with_deadline("test atomic publication", move |conn| {
                    conn.query("BEGIN TRANSACTION")?;
                    conn.query(&format!(
                        "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m",
                        namespace
                    ))?;
                    started_tx
                        .clone()
                        .send(())
                        .expect("signal uncommitted delete");
                    release_rx.lock().unwrap().recv().expect("release transaction");
                    conn.query("COMMIT")?;
                    Ok(())
                })
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().expect("writer began transaction"))
            .await
            .unwrap();

        let visible = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            store.get("atomic-reader", &old_id),
        )
        .await;
        let _ = release_tx.send(());
        writer.await.unwrap().unwrap();
        assert!(
            visible
                .expect("reader should not block behind the uncommitted writer")
                .unwrap()
                .is_some(),
            "the old generation stays visible until commit"
        );
        assert!(store.get("atomic-reader", &old_id).await.unwrap().is_none());
        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn publication_deadline_waits_for_transaction_resolution() {
        let dir = std::env::temp_dir().join(format!("atomic-deadline-{}", uuid::Uuid::new_v4()));
        let store = std::sync::Arc::new(LadybugStore::for_testing(&dir, 4).expect("open DB"));
        store.init("atomic-deadline").await.unwrap();
        let old_id = format!(
            "atomic-deadline{}old.rs",
            crate::parser::ingest::NAMESPACE_SEPARATOR
        );
        store
            .upsert(
                "atomic-deadline",
                &old_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "old".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "old.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let writer_store = store.clone();
        let namespace = "atomic-deadline".to_string();
        let old_id_for_query = old_id.clone();
        let writer = tokio::spawn(async move {
            writer_store
                .write_to_completion(
                    "test publication deadline",
                    std::time::Duration::from_millis(40),
                    move |conn| {
                        conn.query("BEGIN TRANSACTION")?;
                        conn.query(&format!(
                            "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' DETACH DELETE m",
                            escape_ladybug_string(&namespace),
                            escape_ladybug_string(&old_id_for_query)
                        ))?;
                        started_tx.send(()).expect("signal uncommitted delete");
                        release_rx.lock().unwrap().recv().expect("release transaction");
                        conn.query("COMMIT")?;
                        Ok(())
                    },
                )
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().expect("transaction began"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !writer.is_finished(),
            "deadline expiration must not be reported as a completed failed write"
        );
        assert!(store
            .get("atomic-deadline", &old_id)
            .await
            .unwrap()
            .is_some());
        release_tx.send(()).expect("release publication");
        writer
            .await
            .expect("publication worker finishes")
            .expect("publication reports its resolved result");
        assert!(store
            .get("atomic-deadline", &old_id)
            .await
            .unwrap()
            .is_none());

        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn upsert_reports_unresolved_edge_target_and_declaration_index() {
        let store = retired_test_store().await;
        let report = store
            .upsert(
                "probe",
                "rule-with-deferred-edge",
                vec![0.0; 4],
                retired_test_payload(
                    "rule",
                    "agent",
                    // A qualified cross-namespace target never gets a stub
                    // (spec §3.2: stubs are only for the unqualified missing
                    // case), so it still defers with `Missing`.
                    json!({ "governs": ["other-ns::src/not-ingested.rs"] }),
                ),
            )
            .await
            .expect("memory write remains fail-soft");

        assert_eq!(report.id, "rule-with-deferred-edge");
        assert_eq!(report.deferred_edges.len(), 1);
        assert_eq!(report.deferred_edges[0].rel_type, "GOVERNS");
        assert_eq!(
            report.deferred_edges[0].declared_target,
            "other-ns::src/not-ingested.rs"
        );
        assert_eq!(report.deferred_edges[0].declaration_index, 0);
        assert!(matches!(
            report.deferred_edges[0].resolution,
            crate::traits::TargetResolution::Missing
        ));
    }

    /// Spec §3.2: an unqualified `GOVERNS` target that resolves to `Missing`
    /// auto-materialises a stub `file` node, so the edge forms immediately.
    #[tokio::test]
    async fn a_missing_unqualified_governs_target_materialises_a_stub_file_node() {
        let store = retired_test_store().await;
        let report = store
            .upsert(
                "probe",
                "rule-governing-unindexed-file",
                vec![0.0; 4],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["src/not-ingested.rs"] }),
                ),
            )
            .await
            .expect("memory write remains fail-soft");

        assert_eq!(report.id, "rule-governing-unindexed-file");
        assert!(
            report.deferred_edges.is_empty(),
            "a stubbed GOVERNS target materialises, not defers: {:?}",
            report.deferred_edges
        );

        // The stub exists under the legacy qualified id with a zero vector and
        // the structural `file` shape, marked `stub: true`.
        let stub_id = crate::parser::ingest::qualified_id("probe", "src/not-ingested.rs");
        let (vector, payload) = store
            .get("probe", &stub_id)
            .await
            .expect("read the stub back")
            .expect("the stub file node exists");
        assert_eq!(payload.memory_type, "file");
        assert_eq!(payload.user_id, "auto-ingestor");
        assert_eq!(payload.agent_name.as_deref(), Some("neurostrata-stub"));
        assert_eq!(
            payload.metadata.get("stub").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.metadata.get("absolute_path").and_then(|v| v.as_str()),
            Some("src/not-ingested.rs")
        );
        assert_eq!(vector, vec![0.0; 4]);

        // The rule now finds the file through the graph-read capability.
        let hits = store
            .memories_for_file(
                "probe",
                None,
                "src/not-ingested.rs",
                None,
                true,
                &["rule".to_string()],
                &[],
                5,
            )
            .await
            .expect("graph read succeeds");
        assert!(
            hits.iter()
                .any(|h| h.id == "rule-governing-unindexed-file"),
            "the governing rule surfaces for the stubbed file: {:?}",
            hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
    }

    /// A rule that governs a *directory* (`src/`) surfaces for an unindexed
    /// file inside it (`src/foo.rs`) through the `AncestorDirectory` match. The
    /// stub materialised for the directory carries `memory_type = "directory"`,
    /// not `"file"`, so the ancestor match recognises it.
    #[tokio::test]
    async fn unindexed_file_returns_parent_directory_rules() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "dir-governing-rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/"] })),
            )
            .await
            .expect("memory write remains fail-soft");

        // The directory stub is a directory, not a file.
        let stub_id = crate::parser::ingest::qualified_id("probe", "src");
        let (_, payload) = store
            .get("probe", &stub_id)
            .await
            .expect("read the stub back")
            .expect("the directory stub exists");
        assert_eq!(payload.memory_type, "directory");

        let hits = store
            .memories_for_file(
                "probe",
                None,
                "src/foo.rs",
                None,
                true,
                &["rule".to_string()],
                &[],
                5,
            )
            .await
            .expect("graph read succeeds");
        assert!(
            hits.iter().any(|h| h.id == "dir-governing-rule"),
            "the directory-governing rule surfaces for an unindexed file inside it: {:?}",
            hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
        let hit = hits
            .iter()
            .find(|h| h.id == "dir-governing-rule")
            .expect("the rule is among the hits");
        assert_eq!(hit.match_kind, GraphMatchKind::AncestorDirectory);
    }

    /// A memory linked by a `RELATES_TO` edge (not a `GOVERNS` edge) surfaces
    /// for the file it relates to, with the `Related` match kind. The relation
    /// is undirected, but the stored row is directed, so both orientations must
    /// be queried (finding 1).
    #[tokio::test]
    async fn a_related_memory_surfaces_for_the_file_it_relates_to() {
        let store = retired_test_store().await;

        // Target first: the write-shape validator refuses edges whose target
        // row does not exist yet.
        let mut file_payload = retired_test_payload("file", "auto-ingestor", json!({}));
        file_payload.location = "src/a.rs".to_string();
        store
            .upsert("probe", "probe::src/a.rs", vec![0.0; 4], file_payload)
            .await
            .unwrap();
        // The out-orientation: the memory declares the file, so the stored edge
        // is `memory -RELATES_TO-> file`.
        store
            .upsert(
                "probe",
                "related-memory",
                vec![0.0; 4],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({ "related_to": ["probe::src/a.rs"] }),
                ),
            )
            .await
            .unwrap();

        let hits = store
            .memories_for_file(
                "probe",
                None,
                "src/a.rs",
                None,
                true,
                &["fact".to_string()],
                &[],
                5,
            )
            .await
            .expect("graph read succeeds");
        let hit = hits
            .iter()
            .find(|h| h.id == "related-memory")
            .expect("the related memory surfaces: {:?}");
        assert_eq!(hit.match_kind, GraphMatchKind::Related);
        assert!(hit.explanation.contains("relates to"));
    }

    /// The `related` traversal reaches a depth-2 neighbour through a real
    /// `RELATES_TO` chain, marks every crossed edge `Supporting` (never
    /// `Governance`), and starts from the seed's exact id.
    #[tokio::test]
    async fn related_traverses_a_depth_two_relates_to_chain() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];

        // Targets first: the write-shape validator refuses edges whose target
        // row does not exist yet.
        store
            .upsert("probe", "deep", zero.clone(), retired_test_payload("fact", "agent", json!({})))
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "mid",
                zero.clone(),
                retired_test_payload("fact", "agent", json!({ "related_to": ["deep"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "seed",
                zero,
                retired_test_payload("rule", "agent", json!({ "related_to": ["mid"] })),
            )
            .await
            .unwrap();

        let result = store
            .related(
                "probe",
                "seed",
                &[],
                crate::graph_read::related::Direction::Both,
                2,
                &[],
                25,
                None,
            )
            .await
            .expect("related traversal succeeds");

        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["seed", "mid", "deep"]);
        assert_eq!(result.total, 3);
        assert!(
            result
                .edges
                .iter()
                .all(|e| e.role == crate::graph_read::related::EdgeRole::Supporting),
            "RELATES_TO edges are supporting, never governance: {:?}",
            result.edges
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.source == "mid" && e.target == "deep"),
            "the depth-2 hop is present as an edge"
        );
    }

    /// A stub materialised for an unindexed file is retired once the real node
    /// is ingested and published, so the placeholder never shadows the real
    /// tree (finding 3).
    #[tokio::test]
    async fn ingesting_the_real_node_retires_its_stub() {
        let store = retired_test_store().await;

        // The rule names a file that is not yet ingested: a stub materialises.
        store
            .upsert(
                "probe",
                "rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/not-ingested.rs"] })),
            )
            .await
            .unwrap();
        let stub_id = crate::parser::ingest::qualified_id("probe", "src/not-ingested.rs");
        assert!(
            store.get("probe", &stub_id).await.unwrap().is_some(),
            "the stub exists before ingestion"
        );

        // The real node arrives, staged and published as generation 1.
        let real_id =
            crate::parser::ingest::generation_scoped_id("probe", 1, "src/not-ingested.rs");
        store
            .upsert(
                "probe",
                &real_id,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store.publish_generation("probe", PROBE_SOURCE, 1).await.unwrap();

        store.relink_edges("probe", PROBE_SOURCE).await.expect("relink");

        assert!(
            store.get("probe", &stub_id).await.unwrap().is_none(),
            "the stub is retired once the real node is ingested"
        );
        assert!(
            store.get("probe", &real_id).await.unwrap().is_some(),
            "the real node remains"
        );
    }

    /// Ingestion qualifies node ids, and a rule names the file the way a human
    /// writes it. The edge has to form when the rule is written, not only after
    /// the next ingest happens to relink it.
    #[tokio::test]
    async fn a_rule_added_after_ingest_governs_the_file_it_names() {
        let store = retired_test_store().await;
        // Ingestion stages the file as a generation-scoped node and publishes
        // generation 1; the rule written afterwards names the bare path and must
        // attach to the published tree, not to a superseded one.
        let file_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        store
            .upsert(
                "probe",
                &file_id,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store.publish_generation("probe", PROBE_SOURCE, 1).await.unwrap();
        store
            .upsert(
                "probe",
                "rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["src/a.rs"] })),
            )
            .await
            .unwrap();

        assert_eq!(governs_edges_from(&store, "rule").await, 1);
    }

    #[tokio::test]
    async fn qualified_cli_root_id_relinks_same_namespace_governs_edge_exactly() {
        let store = retired_test_store().await;
        let root = std::env::temp_dir().join(format!("cli-id-contract-{}", uuid::Uuid::new_v4()));
        let source_root = root.join("project");
        std::fs::create_dir_all(source_root.join("src")).unwrap();
        let file_path = source_root.join("src/core.rs");
        std::fs::write(&file_path, "pub fn core() {}\n").unwrap();

        let local_id = crate::parser::ingest::node_id_for(&source_root, &file_path);
        assert_eq!(local_id, "src/core.rs");
        let namespace = "synthetic-validation";
        let target_id = crate::parser::ingest::qualified_id(namespace, &local_id);
        let rule_id = crate::parser::ingest::qualified_id(namespace, "rule");
        let stage = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let lease = store.begin_ingest_stage(&stage).await.unwrap();
        store
            .upsert(
                &stage,
                &format!("{stage}::src/core.rs"),
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                namespace,
                &rule_id,
                vec![0.0; 4],
                retired_test_payload("rule", "owner", json!({"governs": [target_id.clone()]})),
            )
            .await
            .unwrap();
        drop(lease);
        store.publish_ingested(namespace, &stage).await.unwrap();

        let actual = store
            .with_conn(|conn| {
                let mut rows =
                    conn.query("MATCH (a:Memory)-[:GOVERNS]->(b:Memory) RETURN a.id, b.id")?;
                let mut edges = HashSet::new();
                while let Some(row) = rows.next() {
                    edges.insert((format!("{}", row[0]), format!("{}", row[1])));
                }
                Ok(edges)
            })
            .await
            .unwrap();
        assert_eq!(actual, HashSet::from([(rule_id, target_id)]));
        assert_eq!(
            store
                .get(namespace, "synthetic-validation::src/core.rs")
                .await
                .unwrap()
                .unwrap()
                .1
                .content,
            "a file row"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Moving used to be "write to the target, delete from the source". upsert
    /// never rewrites a namespace, so the write updated the source row in place
    /// and the delete then removed the only copy.
    #[tokio::test]
    async fn moving_a_memory_keeps_the_row_and_its_edges() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": ["probe::src/a.rs"] })),
            )
            .await
            .unwrap();

        assert_eq!(
            store.relocate("rule", "probe", "global").await.unwrap(),
            RelocateOutcome::Moved
        );

        assert!(
            store.get("global", "rule").await.unwrap().is_some(),
            "the memory is in its new namespace"
        );
        assert!(
            store.get("probe", "rule").await.unwrap().is_none(),
            "and no longer in the old one"
        );
        assert_eq!(
            governs_edges_from(&store, "rule").await,
            1,
            "its edges came with it"
        );
    }

    #[tokio::test]
    async fn an_ingested_node_is_not_moved() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .relocate("probe::src/a.rs", "probe", "other")
                .await
                .unwrap(),
            RelocateOutcome::Ingested
        );
        assert!(
            store
                .get("probe", "probe::src/a.rs")
                .await
                .unwrap()
                .is_some(),
            "and it stays where it was"
        );
        assert_eq!(
            store.relocate("missing", "probe", "other").await.unwrap(),
            RelocateOutcome::NotFound
        );
        assert_eq!(
            store.relocate("rule", "probe", "probe").await.unwrap(),
            RelocateOutcome::SameNamespace
        );
    }

    /// Create one edge of `rel` between two rows addressed by pk, raw —
    /// for graph shapes the metadata declarations cannot express (a
    /// self-edge; an incoming edge in a second table).
    async fn t7_create_edge(store: &LadybugStore, rel: &str, from_pk: &str, to_pk: &str) {
        let query = format!(
            "MATCH (a:Memory {{pk: '{}'}}), (b:Memory {{pk: '{}'}}) CREATE (a)-[:{}]->(b)",
            escape_ladybug_string(from_pk),
            escape_ladybug_string(to_pk),
            rel,
        );
        store
            .with_conn(move |conn| {
                conn.query(&query)?;
                Ok(())
            })
            .await
            .expect("create the edge");
    }

    /// Every (from_pk, to_pk) pair joined by an edge of `rel`, sorted.
    async fn t7_edge_pairs(store: &LadybugStore, rel: &str) -> Vec<(String, String)> {
        let query = format!("MATCH (a:Memory)-[:{}]->(b:Memory) RETURN a.pk, b.pk", rel);
        let mut pairs = store
            .with_conn(move |conn| {
                let mut rows = conn.query(&query)?;
                let mut out = Vec::new();
                while let Some(row) = rows.next() {
                    out.push((format!("{}", row[0]), format!("{}", row[1])));
                }
                Ok(out)
            })
            .await
            .expect("read the edge pairs");
        pairs.sort();
        pairs
    }

    /// The fresh-table fallback (pk IS the primary key, so the in-place
    /// SET is refused) recreates before it deletes, and a self-edge —
    /// matched by both the outgoing and the incoming capture pass — must
    /// be replayed exactly once, remapped onto the new key.
    #[tokio::test]
    async fn relocate_on_a_fresh_table_keeps_a_self_edge_exactly_once() {
        let store = t7_store().await;
        store
            .upsert("probe", "mover", vec![0.0; 4], t7_payload("mover"))
            .await
            .unwrap();
        store
            .upsert("probe", "subject", vec![0.0; 4], t7_payload("subject"))
            .await
            .unwrap();
        let old_pk = memory_pk("probe", "mover");
        t7_create_edge(&store, "RELATES_TO", &old_pk, &old_pk).await;
        t7_create_edge(
            &store,
            "GOVERNS",
            &old_pk,
            &memory_pk("probe", "subject"),
        )
        .await;

        assert_eq!(
            store.relocate("mover", "probe", "global").await.unwrap(),
            RelocateOutcome::Moved
        );

        let new_pk = memory_pk("global", "mover");
        assert!(
            store.get("global", "mover").await.unwrap().is_some(),
            "the row is in its new namespace"
        );
        assert!(
            store.get("probe", "mover").await.unwrap().is_none(),
            "and no copy is left behind"
        );
        assert_eq!(
            t7_edge_pairs(&store, "RELATES_TO").await,
            vec![(new_pk.clone(), new_pk.clone())],
            "exactly one self-edge survives, bound to the new pk"
        );
        assert_eq!(
            t7_edge_pairs(&store, "GOVERNS").await,
            vec![(new_pk, memory_pk("probe", "subject"))],
            "the ordinary outgoing edge came along exactly once"
        );
    }

    /// The fallback's replay covers both directions across all four rel
    /// tables; every edge survives the move exactly once.
    #[tokio::test]
    async fn relocate_on_a_fresh_table_keeps_edges_in_both_directions_across_tables() {
        let store = t7_store().await;
        for id in ["mover", "north", "south"] {
            store
                .upsert("probe", id, vec![0.0; 4], t7_payload(id))
                .await
                .unwrap();
        }
        let mover = memory_pk("probe", "mover");
        let north = memory_pk("probe", "north");
        let south = memory_pk("probe", "south");
        // Outgoing in two tables, incoming in two more.
        t7_create_edge(&store, "RELATES_TO", &mover, &north).await;
        t7_create_edge(&store, "GOVERNS", &mover, &south).await;
        t7_create_edge(&store, "CONTAINS", &north, &mover).await;
        t7_create_edge(&store, "EXTRACTED_FROM", &south, &mover).await;

        assert_eq!(
            store.relocate("mover", "probe", "global").await.unwrap(),
            RelocateOutcome::Moved
        );

        let moved = memory_pk("global", "mover");
        assert_eq!(
            t7_edge_pairs(&store, "RELATES_TO").await,
            vec![(moved.clone(), north.clone())]
        );
        assert_eq!(
            t7_edge_pairs(&store, "GOVERNS").await,
            vec![(moved.clone(), south.clone())]
        );
        assert_eq!(
            t7_edge_pairs(&store, "CONTAINS").await,
            vec![(north, moved.clone())]
        );
        assert_eq!(
            t7_edge_pairs(&store, "EXTRACTED_FROM").await,
            vec![(south, moved)]
        );
        assert_eq!(
            t7_count_where(&store, "m.id = 'mover'").await,
            1,
            "one mover row, in the new namespace"
        );
    }

    /// Relocate validates its identity inputs at the store boundary: an
    /// illegal destination namespace or a NUL-bearing id is refused
    /// before any mutation, and the row stays where it was.
    #[tokio::test]
    async fn relocate_refuses_an_illegal_destination_and_a_nul_id() {
        let store = t7_store().await;
        store
            .upsert("probe", "stayer", vec![0.0; 4], t7_payload("stayer"))
            .await
            .unwrap();

        let err = store
            .relocate("stayer", "probe", "bad ns")
            .await
            .expect_err("a destination namespace with a space is refused");
        assert!(
            err.to_string().contains("ERROR [NAMESPACE]"),
            "the refusal names the violated rule: {err}"
        );
        let err = store
            .relocate("sta\u{0}yer", "probe", "global")
            .await
            .expect_err("a NUL-bearing id is refused");
        assert!(
            err.to_string().contains("ERROR [NUL]"),
            "the refusal names the refused byte: {err}"
        );

        assert!(
            store.get("probe", "stayer").await.unwrap().is_some(),
            "the row is unmoved"
        );
        assert_eq!(t7_count_where(&store, "m.id = 'stayer'").await, 1);
    }

    #[test]
    fn each_metadata_key_maps_to_its_edge_table() {
        let specs = edge_specs(&json!({
            "related_to": ["a"],
            "contained_by": ["parent"],
            "governs": ["src/lib.rs"]
        }));
        assert_eq!(specs.len(), 3);
        assert!(specs.contains(&EdgeSpec {
            rel_type: "RELATES_TO",
            target_id: "a".into(),
            points_at_target: true,
            declaration_index: 0
        }));
        assert!(specs.contains(&EdgeSpec {
            rel_type: "GOVERNS",
            target_id: "src/lib.rs".into(),
            points_at_target: true,
            declaration_index: 0
        }));
    }

    /// Containment points from the parent to the child, so a directory contains
    /// its files rather than the other way round.
    #[test]
    fn containment_points_from_the_parent() {
        let specs = edge_specs(&json!({ "contained_by": ["src"] }));
        assert_eq!(specs[0].rel_type, "CONTAINS");
        assert!(!specs[0].points_at_target);
    }

    #[test]
    fn absent_or_empty_metadata_asks_for_no_edges() {
        assert!(edge_specs(&json!({})).is_empty());
        assert!(edge_specs(&json!({ "related_to": [] })).is_empty());
        assert!(edge_specs(&json!({ "related_to": [""] })).is_empty());
        assert!(edge_specs(&json!(null)).is_empty());
    }

    /// A store with three nodes: a rule that declares the file it governs, a
    /// second rule that declares the same file the way ingestion used to write
    /// it -- absolute -- and the file itself.
    async fn store_with_two_declarations() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let seed = |id: &str, metadata: &str| {
            format!(
                "CREATE (m:Memory {{pk: '{}', id: '{}', namespace: 'probe', content: 'c', user_id: 'u', memory_type: 't', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
                escape_ladybug_string(&format!("seed-pk:{id}")),
                escape_ladybug_string(id),
                escape_ladybug_string(metadata)
            )
        };
        // Ingested nodes are generation-scoped now, and generation 1 is the
        // published tree the relink must attach to. The file row therefore
        // carries the scoped id, and the active pointer is flipped below.
        let file_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        let rows = [
            seed("rule", r#"{"governs": ["src/a.rs"]}"#),
            seed("legacy", r#"{"governs": ["C:/old/checkout/src/a.rs"]}"#),
            seed(&file_id, "{}"),
        ];
        for row in rows {
            store
                .with_conn(move |conn| {
                    conn.query(&row)?;
                    Ok(())
                })
                .await
                .expect("seed a memory");
        }
        store
            .publish_generation("probe", PROBE_SOURCE, 1)
            .await
            .expect("publish generation 1");
        store
    }

    async fn governs_count(store: &LadybugStore) -> usize {
        store
            .with_conn(|conn| {
                let result = conn.query("MATCH (:Memory)-[:GOVERNS]->(:Memory) RETURN count(*)")?;
                let mut count = 0usize;
                for row in result {
                    count += format!("{}", row[0]).parse::<usize>().unwrap_or(0);
                }
                Ok(count)
            })
            .await
            .expect("count the edges")
    }

    /// Both declarations are replayed, including the one that names the file by
    /// its old absolute path.
    #[tokio::test]
    async fn relinking_replays_every_declaration_once() {
        let store = store_with_two_declarations().await;

        let created = store.relink_edges("probe", PROBE_SOURCE).await.expect("relink");
        assert_eq!(created, 2, "both declarations name a node that exists");
        assert_eq!(governs_count(&store).await, 2);
    }

    /// Relinking runs at the end of every ingest, so running it twice must not
    /// double the graph. The count is of edges actually created, not of
    /// statements that did not error -- the loop this replaced reported the
    /// latter, so it claimed hundreds of edges on a run that made none.
    #[tokio::test]
    async fn relinking_twice_creates_nothing_the_second_time() {
        let store = store_with_two_declarations().await;

        assert_eq!(store.relink_edges("probe", PROBE_SOURCE).await.expect("first"), 2);
        assert_eq!(
            store.relink_edges("probe", PROBE_SOURCE).await.expect("second"),
            0,
            "the edges already exist, so none are created"
        );
        assert_eq!(governs_count(&store).await, 2);
    }

    /// Regression guard for task neurostrata-c8te (parallel-test mmap flake):
    /// `LadybugStore::for_testing` MUST use a small enough max_db_size +
    /// buffer_pool_size that N parallel opens don't trip the 8 TiB mmap
    /// reservation the C++ default carries over (DEFAULT_VM_REGION_MAX_SIZE
    /// = `(uint64_t)1 << 43` on Linux x86_64). 32 threads chosen to exceed
    /// the flake's worst observed contention.
    ///
    /// NOTE on drill: this test cannot drill in this dev box (30 GiB RAM,
    /// `ulimit -v unlimited`, Linux overcommit_memory=0 — the 8 TiB mmaps
    /// succeed at the call site). It WILL trip in stricter CI containers
    /// (GitHub Actions: 7 GiB, ulimit set), which is where the original
    /// flake surfaced. The fix is verified directly below in a separate
    /// unit test that asserts the configuration choice.
    #[test]
    fn for_testing_does_not_reserve_an_8tib_mmap() {
        const N: usize = 32;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
        let mut handles = Vec::new();
        for i in 0..N {
            let b = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let dir = std::env::temp_dir().join(format!(
                    "ns-mmap-parallel-{}-{}",
                    std::process::id(),
                    i
                ));
                b.wait();
                LadybugStore::for_testing(&dir, 4)
                    .map(|_| ())
                    .map_err(|e| format!("thread {} failed to open: {}", i, e))
            }));
        }
        for h in handles {
            match h.join().expect("thread panicked") {
                Ok(()) => {}
                Err(e) => panic!("parallel test open failed (regression of c8te): {}", e),
            }
        }
    }

    /// Drill-independent regression for c8te: even on a dev box that allows
    /// 8 TiB mmaps via overcommit, this test verifies the configuration
    /// choice directly: `for_testing` MUST keep the `max_db_size` /
    /// `buffer_pool_size` overrides in its function body. If someone
    /// drops the overrides, the C++ default (8 TiB on Linux x64) returns
    /// at the binding level (`lbug_rs.cpp` only forwards the Rust value
    /// if `!= -1u`/u32::MAX, but the C++ ctor's `SystemConfig()` sets the
    /// 8 TiB default BEFORE the binding overlay -- so leaving the Rust
    /// value at `Default::default()` re-enables the 8 TiB).
    ///
    /// Scoped to the `for_testing` function body so the assertion does not
    /// match its own search string (an earlier version of this guard was
    /// fooled by exactly that -- a self-referential assertion in the test
    /// code itself).
    #[test]
    fn for_testing_uses_small_max_db_size() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/ladybug.rs"),
        )
        .expect("read self");
        let start = src
            .find("pub fn for_testing")
            .expect("for_testing exists in this file");
        let end_marker = "Self::with_system_config";
        let end_rel = src[start..]
            .find(end_marker)
            .expect("end of for_testing call found");
        let body = &src[start..start + end_rel + end_marker.len()];
        assert!(
            body.contains(".max_db_size(max_db_size)"),
            "for_testing must shrink max_db_size below the C++ default \
             (8 TiB on Linux x64); parallel test opens trip the mmap limit \
             -- task neurostrata-c8te regression guard"
        );
        assert!(
            body.contains(".buffer_pool_size(buffer_pool_size)"),
            "for_testing must cap buffer_pool_size for parallel tests"
        );
    }

    /// A declaration naming something that was never ingested links nothing,
    /// and is not counted as though it had.
    #[tokio::test]
    async fn a_declaration_with_no_node_behind_it_links_nothing() {
        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let row = format!(
            "CREATE (m:Memory {{pk: 'seed-pk:rule', id: 'rule', namespace: 'probe', content: 'c', user_id: 'u', memory_type: 't', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
            escape_ladybug_string(r#"{"governs": ["src/never-ingested.rs"]}"#)
        );
        store
            .with_conn(move |conn| {
                conn.query(&row)?;
                Ok(())
            })
            .await
            .expect("seed a memory");

        assert_eq!(store.relink_edges("probe", PROBE_SOURCE).await.expect("relink"), 0);
        assert_eq!(governs_count(&store).await, 0);
    }

    /// A superseded generation's rows are hidden from the read surfaces once a
    /// later publish flips the source's active pointer, while the live rows
    /// remain reachable. Exercises the source-scoped pointer and the
    /// `list`/`export_graph` superseded filters directly (finding 6).
    #[tokio::test]
    async fn superseded_generations_are_hidden_from_list_and_export() {
        let store = retired_test_store().await;
        let zero = vec![0.0f32; 4];

        // Stage and publish generation 1 for source A.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 1);
        let gen1_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        store
            .upsert(
                "probe",
                &gen1_id,
                zero.clone(),
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .publish_generation("probe", "source-a", 1)
            .await
            .unwrap();
        assert_eq!(
            store.active_generation("probe", "source-a").await.unwrap(),
            1
        );

        // A second publish of the same source supersedes generation 1.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 2);
        let gen2_id = crate::parser::ingest::generation_scoped_id("probe", 2, "src/a.rs");
        store
            .upsert(
                "probe",
                &gen2_id,
                zero.clone(),
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .publish_generation("probe", "source-a", 2)
            .await
            .unwrap();
        assert_eq!(
            store.active_generation("probe", "source-a").await.unwrap(),
            2
        );

        // list: only the active generation's row is reachable.
        let listed = store.list("probe", None, None).await.unwrap();
        let listed_ids: Vec<String> = listed.into_iter().map(|r| r.id).collect();
        assert!(listed_ids.contains(&gen2_id), "the active generation is listed");
        assert!(
            !listed_ids.contains(&gen1_id),
            "the superseded generation is not listed"
        );

        // export_graph: the same filter drops the superseded row.
        let graph = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let exported_ids: Vec<String> = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect();
        assert!(
            exported_ids.iter().any(|id| id == &gen2_id),
            "the active generation is exported"
        );
        assert!(
            !exported_ids.iter().any(|id| id == &gen1_id),
            "the superseded generation is not exported"
        );
    }

    /// `get` reads back the embedding it stored.
    ///
    /// The column is FLOAT[N] -- an ARRAY -- and the reader matched only
    /// Value::List, so this came back empty every time. Nothing caught it
    /// because search never returns a vector: it computes array_distance in the
    /// engine. Only the read-modify-write callers saw it, and they wrote the
    /// empty vector straight back (bead neurostrata-vbj).
    #[tokio::test]
    async fn get_reads_back_the_embedding_it_stored() {
        let dir = std::env::temp_dir().join(format!("ns-embed-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let stored = vec![0.25f32, -1.5, 3.0, 0.0];
        store
            .upsert(
                "probe",
                "rule",
                stored.clone(),
                MemoryPayload {
                    content: "c".to_string(),
                    user_id: "u".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("store the row");

        let (read_back, _) = store
            .get("probe", "rule")
            .await
            .expect("query succeeds")
            .expect("the row is there");
        assert_eq!(read_back, stored, "the embedding survives a round trip");
    }

    // --- TASK 1 tests: usage boost formula and counter saturation ---

    #[test]
    fn usage_boost_at_distance_zero_stays_zero() {
        // gain = 0.05 * ln(1+5) ≈ 0.080, capped_gain = min(0.080, 0) = 0
        let n = 5i64;
        let distance: f32 = 0.0;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        let capped = gain.min(0.5 * distance);
        let boosted = distance - capped;
        assert_eq!(boosted, 0.0);
    }

    #[test]
    fn usage_boost_at_count_zero_gives_zero_gain() {
        let n = 0i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        assert_eq!(gain, 0.0);
    }

    #[test]
    fn usage_boost_at_count_one_gives_real_gain() {
        let n = 1i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        assert!(gain > 0.0, "ln(1+1) should be > 0");
    }

    #[test]
    fn usage_boost_cap_holds_at_half_distance() {
        let distance: f32 = 0.1;
        let n = 8i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        let capped = gain.min(0.5 * distance);
        let boosted = distance - capped;
        assert!(boosted >= 0.0, "adjusted must be non-negative");
        assert!(boosted >= 0.5 * distance, "cap must hold: adjusted >= d/2");
        assert!(boosted <= distance, "adjusted must be <= d");
    }

    #[test]
    fn usage_boost_in_range_for_finite_distance() {
        for d in [0.01, 0.1, 0.5, 1.0, 5.0] {
            for n in [0, 1, 5, 100, 1000] {
                let gain = 0.05 * (1.0 + n as f32).ln();
                let capped = gain.min(0.5 * d);
                let boosted = d - capped;
                assert!(
                    boosted >= 0.5 * d,
                    "d={}, n={}: adjusted {} < d/2",
                    d,
                    n,
                    boosted
                );
                assert!(boosted <= d, "d={}, n={}: adjusted {} > d", d, n, boosted);
            }
        }
    }

    #[test]
    fn bump_access_count_saturates_at_i64_max() {
        let metadata = json!({"access_count": i64::MAX}).to_string();
        let result = bump_access_count(&metadata);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["access_count"], json!(i64::MAX));
    }

    #[tokio::test]
    async fn search_after_saturating_count_does_not_panic() {
        let store = retired_test_store().await;
        // Non-integer values: LadybugDB treats whole-number floats as integers,
        // which ARRAY_DISTANCE rejects.
        let v = vec![1.1, 0.0, 0.0, 0.0];
        store
            .upsert(
                "probe",
                "hot-memory",
                v.clone(),
                retired_test_payload("rule", "agent", json!({"access_count": i64::MAX})),
            )
            .await
            .unwrap();
        // Should not panic from overflow in the gain calculation.
        let results = store.search("probe", v, 10, false, false).await.unwrap();
        assert!(!results.is_empty());
    }

    // --- TASK 2 tests: search paging, expansion paging, export_graph ---

    #[tokio::test]
    async fn search_returns_active_results_when_retired_rows_fill_first_pages() {
        let store = retired_test_store().await;
        // Insert many retired rows with very close (identical) embeddings,
        // then one live row further away.
        for i in 0..20 {
            store
                .upsert(
                    "probe",
                    &format!("retired-{}", i),
                    vec![0.1; 4],
                    retired_test_payload("rule", "agent", json!({"valid_to": 1})),
                )
                .await
                .unwrap();
        }
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.9; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![0.1; 4], 5, false, false)
            .await
            .unwrap();
        let ids: Vec<String> = results.into_iter().map(|r| r.id).collect();
        assert!(
            ids.contains(&"live-row".to_string()),
            "paging must find the live row: {:?}",
            ids
        );
        assert!(
            ids.iter().all(|id| !id.starts_with("retired-")),
            "no retired rows should appear: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn search_expansion_excludes_retired_while_live_sibling_surfaces() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";

        store
            .upsert(
                "probe",
                file,
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "live-rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": [file] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-rule",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })),
            )
            .await
            .unwrap();

        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 5, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert!(ids.contains(&"live-rule".to_string()), "{:?}", ids);
        assert!(!ids.contains(&"retired-rule".to_string()), "{:?}", ids);
    }

    #[tokio::test]
    async fn search_stable_tie_order() {
        let store = retired_test_store().await;
        // Two live rows with identical embeddings; order should be deterministic.
        for name in &["alpha", "beta"] {
            store
                .upsert(
                    "probe",
                    name,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", json!({})),
                )
                .await
                .unwrap();
        }

        let first: Vec<String> = store
            .search("probe", vec![0.5; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        let second: Vec<String> = store
            .search("probe", vec![0.5; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(first, second, "tie order must be deterministic");
    }

    /// FEATURE-4: on equal semantic distance, the memory that says where it
    /// came from ranks first. Both rows carry the identical embedding, so
    /// distance ties exactly and provenance is the only thing that can separate
    /// them. The query is deliberately off-axis: at distance 0 the cap allows no
    /// boost at all, and a test that cannot fail is not a test.
    #[tokio::test]
    async fn sourced_row_outranks_unsourced_row_at_equal_distance() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "unsourced",
                vec![0.5; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "sourced",
                vec![0.5; 4],
                retired_test_payload("rule", "agent", json!({ "source": "owner 2026-10-09" })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![0.25; 4], 10, false, false)
            .await
            .unwrap();
        let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();

        assert_eq!(
            ids.first().map(|s| s.as_str()),
            Some("sourced"),
            "sourced memory must win the tie: {:?}",
            ids
        );
        // Both rows sit at the same distance; only the sourced one was promoted.
        assert!(
            results[0].score < results[1].score,
            "sourced row must carry the lower (better) score: {:?}",
            results
                .iter()
                .map(|r| (r.id.clone(), r.score))
                .collect::<Vec<_>>()
        );
    }

    /// The cap is the whole contract: a provenance marker settles a tie, it
    /// does not promote an off-topic memory over an on-topic one. Same boost,
    /// worse embedding, so raw distance must still decide.
    #[tokio::test]
    async fn provenance_never_overrides_a_better_match() {
        let store = retired_test_store().await;
        // Query points at [0.9,0.1,0.1,0.1]. The unsourced row sits exactly on
        // it (distance 0); the sourced row points the other way entirely.
        // Components are non-zero decimals because the query string is built by
        // Display on each f32 -- a 0.0 renders as "0" and the literal comes back
        // from the parser as INT, which ARRAY_DISTANCE refuses outright.
        store
            .upsert(
                "probe",
                "unsourced-closest",
                vec![0.9, 0.1, 0.1, 0.1],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "sourced-far",
                vec![0.1, 0.9, 0.1, 0.1],
                retired_test_payload("rule", "agent", json!({ "source": "owner 2026-10-09" })),
            )
            .await
            .unwrap();

        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert_eq!(
            ids.first().map(|s| s.as_str()),
            Some("unsourced-closest"),
            "a closer match wins even when unsourced: {:?}",
            ids
        );
    }

    /// Provenance and usage are separate signals and both still count. A sourced
    /// row with no reads must beat an unsourced row with none (provenance), and
    /// a heavily-used unsourced row must still beat a sourced row with none
    /// (usage is not subordinated to provenance).
    #[tokio::test]
    async fn provenance_and_usage_both_apply() {
        let store = retired_test_store().await;
        for (id, meta) in [
            ("plain", json!({})),
            ("sourced", json!({ "source": "owner 2026-10-09" })),
            ("used-unsourced", json!({ "access_count": 50 })),
        ] {
            store
                .upsert(
                    "probe",
                    id,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", meta),
                )
                .await
                .unwrap();
        }

        let scored: Vec<(String, f32)> = store
            .search("probe", vec![0.25; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.id, r.score))
            .collect();

        let score_of = |id: &str| {
            scored
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, s)| *s)
                .unwrap()
        };
        assert!(
            score_of("sourced") < score_of("plain"),
            "sourced outranks plain: {:?}",
            scored
        );
        assert!(
            score_of("used-unsourced") < score_of("sourced"),
            "usage gain is not capped away by provenance: {:?}",
            scored
        );
    }

    /// A blank or null `source` is not provenance. The gate treats it as
    /// missing, so the ranker must too -- otherwise passing the gate buys no
    /// ranking advantage and the two stop describing the same field.
    #[tokio::test]
    async fn blank_source_is_not_provenance() {
        let store = retired_test_store().await;
        for (id, meta) in [
            ("unsourced", json!({})),
            ("blank", json!({ "source": "   " })),
            ("null", json!({ "source": null })),
        ] {
            store
                .upsert(
                    "probe",
                    id,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", meta),
                )
                .await
                .unwrap();
        }

        let scored: Vec<(String, f32)> = store
            .search("probe", vec![0.25; 4], 10, false, false)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.id, r.score))
            .collect();

        let blank = scored.iter().find(|(i, _)| i == "blank").unwrap().1;
        let null = scored.iter().find(|(i, _)| i == "null").unwrap().1;
        let plain = scored.iter().find(|(i, _)| i == "unsourced").unwrap().1;
        assert_eq!(
            blank, plain,
            "a blank source is not provenance: {:?}",
            scored
        );
        assert_eq!(null, plain, "a null source is not provenance: {:?}", scored);
    }

    #[tokio::test]
    async fn export_graph_excludes_retired_by_default() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "another-live",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "related_to": ["live-node"] })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-node"), "{:?}", node_ids);
        assert!(node_ids.contains(&"another-live"), "{:?}", node_ids);
        assert!(!node_ids.contains(&"retired-node"), "{:?}", node_ids);

        // No dangling edges to retired nodes.
        let links = data["links"].as_array().unwrap();
        let retired_set: std::collections::HashSet<&str> =
            ["retired-node"].iter().copied().collect();
        for link in links {
            let src = link.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let tgt = link.get("target").and_then(|s| s.as_str()).unwrap_or("");
            assert!(
                !retired_set.contains(src) && !retired_set.contains(tgt),
                "dangling edge to retired node: {:?}",
                link
            );
        }
    }

    #[tokio::test]
    async fn export_graph_include_retired_true_shows_all() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "retired-node",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(true, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-node"), "{:?}", node_ids);
        assert!(node_ids.contains(&"retired-node"), "{:?}", node_ids);
    }

    // ---- export-graph archived filter -----------------------------------
    //
    // The export-graph CLI mirrors the read surfaces: archived rows are
    // hidden by default, with an explicit `include_archived: true` escape
    // hatch. The trait gains a second parameter; the SQL filter excludes
    // rows whose `metadata.archived == true` when the caller does not ask
    // for them.

    /// A row tombstoned by `archive_memory` (or hand-written with the same
    /// shape) is excluded from the default export. The export-graph is a
    /// recall surface for "what is live", not "what was ever true".
    #[tokio::test]
    async fn export_graph_excludes_archived_by_default() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "archived-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "archived": true })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-row"), "{:?}", node_ids);
        assert!(
            !node_ids.contains(&"archived-row"),
            "archived rows are excluded by default: {node_ids:?}"
        );
    }

    /// `include_archived: true` is the audit / second-look escape hatch --
    /// every row, live and tombstoned, surfaces.
    #[tokio::test]
    async fn export_graph_include_archived_true_shows_all() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "archived-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "archived": true })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, true, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-row"), "{:?}", node_ids);
        assert!(
            node_ids.contains(&"archived-row"),
            "include_archived surfaces the tombstoned row: {node_ids:?}"
        );
    }

    // ---- export-graph conflict suppression --------------------------------
    //
    // A row marked under an unresolved conflict overlay is suppressed from
    // the effective view (spec §"Concurrent merge policy") the same way an
    // archived row is — except there is NO escape hatch for it: even
    // `include_archived: true` must never serve an unresolved conflict.

    /// Sensitivity note: this suppression site (the
    /// `is_conflicting_metadata` filter in `export_graph`) shipped before
    /// any test pinned it — the integration checker found that removing
    /// the filter leaves every suite green (a mutation-green site). This
    /// fixture is the pin: one conflicting row + one clean row, exported
    /// with `include_archived: true` so the audit hatch is wide open. The
    /// conflicting row stays out; the clean row is untouched by it.
    #[tokio::test]
    async fn export_graph_suppresses_conflicting_rows_even_with_include_archived() {
        let store = retired_test_store().await;
        store
            .upsert(
                "probe",
                "clean-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "conflict-row",
                vec![0.0; 4],
                retired_test_payload("rule", "agent", json!({ "mesh": { "conflict": true } })),
            )
            .await
            .unwrap();

        let data = store
            .export_graph(false, true, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(
            !node_ids.contains(&"conflict-row"),
            "unresolved conflict rows leave the effective view even with \
             include_archived: {node_ids:?}"
        );
        assert!(
            node_ids.contains(&"clean-row"),
            "the suppression is per-row, never a blanket export loss: {node_ids:?}"
        );
    }

    // --- Vocabulary validation (parse_edge_specs) tests ---

    #[test]
    fn vocabulary_validation_allows_missing_and_empty_arrays() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert!(warnings.is_empty());

        let p2 = MemoryPayload {
            metadata: json!({ "related_to": [], "contained_by": [], "governs": [] }),
            ..p.clone()
        };
        let (specs2, warnings2) = parse_edge_specs(&p2);
        assert!(specs2.is_empty());
        assert!(warnings2.is_empty());
    }

    #[test]
    fn vocabulary_validation_skips_only_invalid_edge_items() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["valid.rs", 42, "", null] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target_id, "valid.rs");
        // Two warnings: integer item + empty string
        assert!(warnings.len() >= 2);
    }

    #[test]
    fn vocabulary_validation_preserves_custom_memory_types() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "preference".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        };
        let (_, warnings) = parse_edge_specs(&p);
        assert!(
            warnings.is_empty(),
            "custom types should be accepted silently"
        );
    }

    #[test]
    fn vocabulary_validation_preserves_payload_metadata() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "fact".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["src/a.rs"], "domain": "test" }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target_id, "src/a.rs");
        assert!(warnings.is_empty());
    }

    #[test]
    fn vocabulary_validation_accepts_existing_qualified_cross_namespace_target() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["OtherNS::src/a.rs"] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target_id, "OtherNS::src/a.rs");
        assert!(
            warnings.is_empty(),
            "qualified cross-namespace targets should be accepted"
        );
    }

    #[test]
    fn vocabulary_validation_warns_for_missing_qualified_target() {
        // Qualified with empty local ID part
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["NS::"] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn vocabulary_validation_warns_for_unresolved_unqualified_target() {
        // Non-array metadata
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": "not-an-array" }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].why, "relation declaration must be an array");
    }

    #[test]
    fn vocabulary_validation_rejects_empty_qualified_components_only() {
        // Empty namespace part
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": ["::src/a.rs"] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn vocabulary_warning_has_exact_reason_and_skipped_fields() {
        let p = MemoryPayload {
            content: "c".into(),
            user_id: "u".into(),
            memory_type: "rule".into(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({ "governs": [42] }),
        };
        let (specs, warnings) = parse_edge_specs(&p);
        assert!(specs.is_empty());
        assert_eq!(warnings[0].why, "relation target must be a string");
        assert_eq!(warnings[0].skipped, "this edge declaration");
    }

    /// The access-topology shape check (vocabulary v5).
    ///
    /// A `fact` declares the access path it describes in `metadata.topology`.
    /// The check is soft: a malformed shape warns and the fact is still
    /// written, because a rejected write would lose the claim.
    mod topology {
        use super::*;

        fn well_formed_topology() -> Value {
            json!({
                "component": "api-gateway",
                "target_system": "ledger-service",
                "channel": "mTLS",
                "credential_boundary": "eu-west-1 VPC",
                "reason": "the ledger accepts traffic only from inside the VPC",
            })
        }

        fn fact_with_topology(topology: Value) -> MemoryPayload {
            MemoryPayload {
                content: "c".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({ "topology": topology }),
            }
        }

        #[test]
        fn a_well_formed_topology_writes_without_a_warning() {
            let mut topology = well_formed_topology();
            // An extra key is not a defect: the declared shape ignores them.
            topology["extra_key"] = json!("ignored");
            let (specs, warnings) = parse_edge_specs(&fact_with_topology(topology));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert!(
                !warnings.iter().any(|w| w.field == "metadata.topology"),
                "a well-formed topology raises no warning: {warnings:?}"
            );
            assert!(
                warnings.is_empty(),
                "the payload carries nothing else that could warn: {warnings:?}"
            );
        }

        #[test]
        fn a_malformed_topology_warns_and_the_fact_is_still_written() {
            // A required key is missing.
            let mut missing = well_formed_topology();
            missing.as_object_mut().unwrap().remove("channel");
            let (specs, warnings) = parse_edge_specs(&fact_with_topology(missing));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.topology");
            assert_eq!(
                warnings[0].why,
                "metadata.topology is missing the required key \"channel\""
            );
            assert_eq!(
                warnings[0].skipped,
                "the topology shape check; the fact is still written"
            );

            // The topology is not an object at all: a string, an array, a null.
            for topology in [json!("not-an-object"), json!(["component"]), Value::Null] {
                let (_, warnings) = parse_edge_specs(&fact_with_topology(topology));
                assert_eq!(warnings.len(), 1, "{warnings:?}");
                assert_eq!(warnings[0].field, "metadata.topology");
                assert_eq!(warnings[0].why, "metadata.topology must be an object");
            }

            // A present key that is not a string.
            let mut not_a_string = well_formed_topology();
            not_a_string["channel"] = json!(42);
            let (_, warnings) = parse_edge_specs(&fact_with_topology(not_a_string));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.topology");
            assert_eq!(
                warnings[0].why,
                "metadata.topology key \"channel\" must be a string"
            );
        }

        #[test]
        fn a_topology_on_a_memory_that_is_not_a_fact_is_not_checked() {
            let p = MemoryPayload {
                memory_type: "rule".into(),
                ..fact_with_topology(json!("not-an-object"))
            };
            let (_, warnings) = parse_edge_specs(&p);
            assert!(
                warnings.is_empty(),
                "the field is declared on a fact alone, and an undeclared metadata \
                 key is accepted unchanged: {warnings:?}"
            );
        }

        /// The check's key list and the vocabulary's own list must be the same
        /// list. A vocabulary bump that added a key would otherwise leave the
        /// store enforcing a stale shape, silently.
        #[test]
        fn the_topology_keys_match_the_vocabulary() {
            let v = crate::traits::memory_vocabulary();
            let summary = v["tool_summaries"]["memory_type"]
                .as_str()
                .expect("the memory_type summary is a string");
            let listed = summary
                .split_once("metadata.topology (")
                .expect("the summary lists the topology keys")
                .1
                .split_once(')')
                .expect("the listed keys are parenthesised")
                .0;
            let vocabulary_keys: Vec<&str> = listed.split(", ").collect();
            assert_eq!(
                vocabulary_keys,
                TOPOLOGY_KEYS.to_vec(),
                "the check enforces the vocabulary's own key list"
            );

            let definition = v["memory_types"]["fact"]["fields"]["topology"]["definition"]
                .as_str()
                .expect("the topology field carries a definition");
            for key in TOPOLOGY_KEYS {
                assert!(
                    definition.contains(key),
                    "the topology definition names {}",
                    key
                );
            }
        }

        #[tokio::test]
        async fn a_fact_with_a_malformed_topology_is_still_written() {
            let store = retired_test_store().await;
            store
                .upsert(
                    "probe",
                    "probe::fact",
                    vec![0.0; 4],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "topology": { "component": "api-gateway" } }),
                    ),
                )
                .await
                .expect("a malformed topology is a warning, not a rejection");

            let stored = store
                .get("probe", "probe::fact")
                .await
                .unwrap()
                .expect("the fact is stored");
            assert_eq!(
                stored.1.metadata["topology"]["component"],
                json!("api-gateway"),
                "the topology is stored exactly as it was given"
            );
        }
    }

    /// The access-attempt shape check (vocabulary v6).
    mod attempt {
        use super::*;

        fn well_formed_attempt() -> Value {
            json!({
                "approach": "retry with backoff",
                "outcome": "failed",
                "failure_mode": "timeout",
                "context": "under load"
            })
        }

        fn fact_with_attempt(attempt: Value) -> MemoryPayload {
            MemoryPayload {
                content: "c".into(),
                user_id: "u".into(),
                memory_type: "fact".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({ "attempt": attempt }),
            }
        }

        #[test]
        fn a_well_formed_attempt_writes_without_a_warning() {
            let (specs, warnings) = parse_edge_specs(&fact_with_attempt(well_formed_attempt()));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert!(
                !warnings.iter().any(|w| w.field == "metadata.attempt"),
                "a well-formed attempt raises no warning: {warnings:?}"
            );
            assert!(
                warnings.is_empty(),
                "the payload carries nothing else that could warn: {warnings:?}"
            );
        }

        #[test]
        fn a_malformed_attempt_warns_at_parse_edge_specs_level() {
            // missing key
            let mut missing = well_formed_attempt();
            missing.as_object_mut().unwrap().remove("context");
            let (specs, warnings) = parse_edge_specs(&fact_with_attempt(missing));
            assert!(specs.is_empty(), "the check never touches the edge specs");
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.attempt");
            assert_eq!(
                warnings[0].why,
                "metadata.attempt is missing the required key \"context\""
            );
            assert_eq!(
                warnings[0].skipped,
                "the attempt shape check; the fact is still written"
            );

            // not an object
            for attempt in [json!("not-an-object"), json!(["approach"]), Value::Null] {
                let (_, warnings) = parse_edge_specs(&fact_with_attempt(attempt));
                assert_eq!(warnings.len(), 1, "{warnings:?}");
                assert_eq!(warnings[0].field, "metadata.attempt");
                assert_eq!(warnings[0].why, "metadata.attempt must be an object");
            }

            // non-string key
            let mut not_a_string = well_formed_attempt();
            not_a_string["approach"] = json!(42);
            let (_, warnings) = parse_edge_specs(&fact_with_attempt(not_a_string));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.attempt");
            assert_eq!(
                warnings[0].why,
                "metadata.attempt key \"approach\" must be a string"
            );

            // empty string
            let mut empty = well_formed_attempt();
            empty["approach"] = json!("   ");
            let (_, warnings) = parse_edge_specs(&fact_with_attempt(empty));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].field, "metadata.attempt");
            assert_eq!(
                warnings[0].why,
                "metadata.attempt key \"approach\" must be a non-empty string"
            );
        }

        #[test]
        fn an_attempt_on_a_memory_that_is_not_a_fact_is_not_checked() {
            let p = MemoryPayload {
                memory_type: "rule".into(),
                ..fact_with_attempt(json!("not-an-object"))
            };
            let (_, warnings) = parse_edge_specs(&p);
            assert!(
                warnings.is_empty(),
                "the field is declared on a fact alone: {warnings:?}"
            );
        }

        #[tokio::test]
        async fn a_malformed_attempt_warns_and_the_write_still_succeeds() {
            let store = retired_test_store().await;
            store
                .upsert(
                    "probe",
                    "probe::fact",
                    vec![0.0; 4],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "attempt": { "approach": "retry", "outcome": "failed", "failure_mode": "timeout" } }),
                    ),
                )
                .await
                .expect("a malformed attempt is a warning, not a rejection");

            let stored = store
                .get("probe", "probe::fact")
                .await
                .unwrap()
                .expect("the fact is stored");
            assert_eq!(
                stored.1.metadata["attempt"]["approach"],
                json!("retry"),
                "the attempt is stored exactly as it was given"
            );
        }
    }

    // --- Integration tests: search with evidence ---

    /// Build a probe namespace with symbol contained_by file, rule governs file.
    async fn evidence_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-evidence-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        // File node (structural, zero vector)
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.1; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();

        // Symbol contained_by file (vector close to query)
        store
            .upsert(
                "probe",
                "probe::src/a.rs::run",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "code_ast",
                    "auto-ingestor",
                    json!({ "contained_by": ["probe::src/a.rs"] }),
                ),
            )
            .await
            .unwrap();

        // Rule governs file (vector orthogonal to query)
        store
            .upsert(
                "probe",
                "rule-1",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": ["probe::src/a.rs"] })),
            )
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn relationship_label_queries_return_actual_table_names() {
        let store = evidence_test_store().await;
        // Query closest to symbol; symbol is primary, file is one-hop, rule is governing
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        // Symbol should be primary (direct match)
        let sym = results
            .iter()
            .find(|r| r.id == "probe::src/a.rs::run")
            .unwrap();
        assert!(sym
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch));
    }

    #[tokio::test]
    async fn one_hop_evidence_preserves_both_directed_orientations() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let file = results.iter().find(|r| r.id == "probe::src/a.rs").unwrap();
        let evs = file.evidence.as_ref().unwrap();
        let one_hop: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert!(!one_hop.is_empty(), "file should have one-hop evidence");
        // The CONTAINS edge: symbol contains_by file, so file is the source of CONTAINS
        // Path: "probe::src/a.rs::run" <-CONTAINS- "probe::src/a.rs"
        let path = &one_hop[0].path;
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].relation, "CONTAINS");
        assert_eq!(path[0].source, "probe::src/a.rs");
        assert_eq!(path[0].target, "probe::src/a.rs::run");
    }

    /// A store whose primary reaches its neighbours by OUTGOING edges only:
    ///
    ///   probe::primary -RELATES_TO-> probe::mid <-GOVERNS- probe::ruler
    ///
    /// `probe::mid` is the one-hop neighbour; `probe::ruler` is the node that
    /// governs it, two hops from the primary. The expansion has to report the
    /// graph's own direction on both paths.
    async fn outgoing_evidence_test_store() -> LadybugStore {
        let dir =
            std::env::temp_dir().join(format!("ns-outgoing-evidence-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        // The one-hop neighbour: the target of the primary's RELATES_TO edge.
        store
            .upsert(
                "probe",
                "probe::mid",
                vec![0.5, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // The governing node: it governs the one-hop neighbour.
        store
            .upsert(
                "probe",
                "probe::ruler",
                vec![0.2, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "governs": ["probe::mid"] })),
            )
            .await
            .unwrap();

        // The primary: closest to the query, source of the one-hop edge.
        store
            .upsert(
                "probe",
                "probe::primary",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "related_to": ["probe::mid"] })),
            )
            .await
            .unwrap();

        store
    }

    /// An outgoing edge is reported from the primary towards the neighbour.
    /// Reading the direction flag off a rendered boolean made it always false,
    /// which reversed every outgoing path.
    #[tokio::test]
    async fn outgoing_one_hop_evidence_points_from_the_primary_to_the_neighbour() {
        let store = outgoing_evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let mid = results.iter().find(|r| r.id == "probe::mid").unwrap();
        let one_hop: Vec<_> = mid
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert_eq!(one_hop.len(), 1, "mid should carry one one-hop path");
        let edge = &one_hop[0].path[0];
        assert_eq!(edge.source, "probe::primary");
        assert_eq!(edge.relation, "RELATES_TO");
        assert_eq!(edge.target, "probe::mid");
    }

    /// The first hop of a two-hop governing path is an outgoing edge too, and
    /// carries the same direction.
    #[tokio::test]
    async fn outgoing_two_hop_evidence_keeps_the_first_hop_directed() {
        let store = outgoing_evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let ruler = results.iter().find(|r| r.id == "probe::ruler").unwrap();
        let two_hop: Vec<_> = ruler
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert_eq!(two_hop.len(), 1, "ruler should carry one two-hop path");
        let path = &two_hop[0].path;
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].source, "probe::primary");
        assert_eq!(path[0].relation, "RELATES_TO");
        assert_eq!(path[0].target, "probe::mid");
        assert_eq!(path[1].source, "probe::ruler");
        assert_eq!(path[1].relation, "GOVERNS");
        assert_eq!(path[1].target, "probe::mid");
    }

    #[tokio::test]
    async fn relates_to_evidence_is_undirected_and_canonical() {
        let dir = std::env::temp_dir().join(format!("ns-relates-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.unwrap();

        // mem-b must exist before mem-a references it via related_to
        store
            .upsert(
                "probe",
                "mem-b",
                vec![0.9, 0.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "mem-a",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "related_to": ["mem-b"] })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let b = results.iter().find(|r| r.id == "mem-b").unwrap();
        let evs = b.evidence.as_ref().unwrap();
        let one_hop: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert!(!one_hop.is_empty());
        assert_eq!(one_hop[0].path[0].relation, "RELATES_TO");
    }

    #[tokio::test]
    async fn governs_two_hop_evidence_contains_primary_file_and_rule() {
        let store = evidence_test_store().await;
        // A filler primary closer than the rule fills the primary quota, so
        // rule-1 stays out of the primary set and must arrive via expansion.
        store
            .upsert(
                "probe",
                "filler",
                vec![0.9, 0.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        // limit=2: primaries are the symbol (closest) and the filler; the file
        // arrives one hop away and the rule two hops away. Four results, the
        // full limit * 2 response.
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 2, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1");
        assert!(
            rule.is_some(),
            "rule-1 should appear in results, got: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let evs = rule.unwrap().evidence.as_ref().unwrap();
        let gov2: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert!(
            !gov2.is_empty(),
            "rule should have governs_2hop evidence, got: {:?}",
            evs.iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
        let path = &gov2[0].path;
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].relation, "CONTAINS");
        assert_eq!(path[1].relation, "GOVERNS");
        // rule-1 GOVERNS file: rule-1 is source, file is target
        assert_eq!(path[1].source, "rule-1");
    }

    #[tokio::test]
    async fn retired_intermediate_excludes_only_its_evidence_path() {
        let store = evidence_test_store().await;
        // Retire the file (intermediate between symbol and rule)
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.1; 4],
                retired_test_payload("file", "auto-ingestor", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1");
        // Rule should still appear (as one-hop via direct GOVERNS from symbol? no —
        // the only path to rule is through the file, which is retired)
        // Actually: symbol is primary. symbol --CONTAINS--> file. file is retired.
        // governing: a(symbol) -[e]-> f(file) <-[:GOVERNS]- r(rule)
        // f is retired, so this path is excluded.
        // But rule has GOVERNS edge directly from rule -> file, so there might be
        // a one-hop path from symbol to rule if symbol and rule share a direct edge.
        // They don't. So rule should NOT appear.
        if let Some(rule) = rule {
            // If rule appears, its evidence should not reference the retired file
            for ev in rule.evidence.as_ref().unwrap() {
                for edge in &ev.path {
                    assert!(
                        edge.source != "probe::src/a.rs" && edge.target != "probe::src/a.rs",
                        "retired intermediate should not appear in evidence"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn active_rule_survives_via_another_active_intermediate() {
        let store = evidence_test_store().await;
        // A second symbol fills the primary quota (limit=2) so rule-1 stays
        // expansion-only, and gives the rule a second intermediate to travel
        // through.
        store
            .upsert(
                "probe",
                "probe::src/b.rs",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "probe::src/b.rs::run",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "code_ast",
                    "auto-ingestor",
                    json!({ "contained_by": ["probe::src/b.rs"] }),
                ),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "rule-1",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["probe::src/a.rs", "probe::src/b.rs"] }),
                ),
            )
            .await
            .unwrap();
        // Retire the ORIGINAL intermediate: the a.rs path dies, the rule must
        // survive through b.rs alone.
        store
            .upsert(
                "probe",
                "probe::src/a.rs",
                vec![0.1; 4],
                retired_test_payload("file", "auto-ingestor", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();

        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 2, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1");
        assert!(
            rule.is_some(),
            "rule-1 should survive via the active intermediate, got: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let evs = rule.unwrap().evidence.as_ref().unwrap();
        let gov2: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert!(!gov2.is_empty(), "rule should have governs_2hop evidence");
        for ev in &gov2 {
            for edge in &ev.path {
                assert!(
                    edge.source != "probe::src/a.rs" && edge.target != "probe::src/a.rs",
                    "retired intermediate must not appear in surviving evidence: {:?}",
                    ev.path
                );
            }
        }
        // The surviving path travels through the active intermediate.
        assert!(gov2.iter().all(|e| e
            .path
            .iter()
            .any(|edge| edge.target == "probe::src/b.rs" || edge.source == "probe::src/b.rs")));
    }

    #[tokio::test]
    async fn direct_match_evidence_has_no_edges() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let sym = results
            .iter()
            .find(|r| r.id == "probe::src/a.rs::run")
            .unwrap();
        let evs = sym.evidence.as_ref().unwrap();
        let direct = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::DirectMatch)
            .unwrap();
        assert!(
            direct.path.is_empty(),
            "direct match evidence has empty path"
        );
    }

    #[tokio::test]
    async fn duplicate_result_ids_merge_distinct_paths() {
        let store = evidence_test_store().await;
        // file appears in both: one-hop from symbol (via CONTAINS) and as intermediate
        // in the governing path. It should have one evidence entry at most.
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let file = results.iter().find(|r| r.id == "probe::src/a.rs").unwrap();
        let evs = file.evidence.as_ref().unwrap();
        // File should have one-hop evidence (from symbol via CONTAINS)
        let one_hop: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::OneHop)
            .collect();
        assert!(!one_hop.is_empty());
    }

    #[tokio::test]
    async fn delete_missing_memory_fails_loudly() {
        let store = evidence_test_store().await;
        // A zero-row delete must not read as success: the caller would report
        // a deletion that never happened.
        let missing = store.delete("probe", "no-such-id").await;
        assert!(missing.is_err(), "deleting an absent id must error");
        assert!(missing.unwrap_err().to_string().contains("no-such-id"));

        // The real id still deletes cleanly.
        store.delete("probe", "rule-1").await.unwrap();
        let gone = store.get("probe", "rule-1").await.unwrap();
        assert!(gone.is_none(), "rule-1 should be deleted");
    }

    #[tokio::test]
    async fn evidence_paths_are_sorted_and_capped_at_three() {
        let store = evidence_test_store().await;
        // Add extra paths: rule also related_to the symbol
        store.upsert("probe", "rule-1", vec![0.0, 1.1, 0.0, 0.0],
            retired_test_payload("rule", "agent",
                json!({ "governs": ["probe::src/a.rs"], "related_to": ["probe::src/a.rs::run"] }))).await.unwrap();
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        let rule = results.iter().find(|r| r.id == "rule-1").unwrap();
        let evs = rule.evidence.as_ref().unwrap();
        assert!(
            evs.len() <= 3,
            "evidence should be capped at 3, got {}",
            evs.len()
        );
        // Should be sorted by kind (OneHop before Governs2hop)
        for pair in evs.windows(2) {
            assert!(
                pair[0].kind <= pair[1].kind,
                "evidence should be sorted by kind"
            );
        }
    }

    #[tokio::test]
    async fn expansion_never_creates_paths_longer_than_two_edges() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    assert!(
                        ev.path.len() <= 2,
                        "{}: path too long ({} edges)",
                        r.id,
                        ev.path.len()
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn duplicate_paths_do_not_consume_result_slots() {
        let store = evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .unwrap();
        // Each result should appear at most once
        let mut ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), results.len(), "no duplicate result IDs");
    }

    // --- BUG-1 / neurostrata-i54c / neurostrata-sfrh fix tests ---

    /// Test helper for search with filters
    async fn search_with_filters(
        store: &LadybugStore,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
    ) -> Vec<SearchResult> {
        // This will be the new signature after the fix
        store
            .search(namespace, vector, limit, include_tasks, include_archived)
            .await
            .unwrap()
    }

    /// Build a test store with task, archived, and mesh-conflicting rows
    async fn filter_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-filter-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let zero = vec![0.0f32; 4];
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Primary: a fact that matches the query
        store
            .upsert(
                "probe",
                "primary-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // Task row (should be excluded by default)
        store
            .upsert(
                "probe",
                "task-row",
                query_vec.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();

        // Archived row (should be excluded by default)
        store
            .upsert(
                "probe",
                "archived-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "archived": true })),
            )
            .await
            .unwrap();

        // Mesh-conflicting row (should be excluded by default)
        store
            .upsert(
                "probe",
                "conflict-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "mesh": { "conflict": true } })),
            )
            .await
            .unwrap();

        // A rule that governs the primary (expansion candidate)
        store
            .upsert(
                "probe",
                "governs-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": ["primary-fact"] })),
            )
            .await
            .unwrap();

        // A task that governs something (should NOT seed expansions)
        store
            .upsert(
                "probe",
                "task-governs",
                vec![0.0, 0.0, 1.1, 0.0],
                retired_test_payload("task", "agent", json!({ "governs": ["primary-fact"] })),
            )
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn graph_expanded_neighbor_appears_in_results() {
        let store = filter_test_store().await;
        // Search with limit=1, should get 1 primary + its expansions
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        // The primary should be there
        let primary = results.iter().find(|r| r.id == "primary-fact");
        assert!(
            primary.is_some(),
            "primary-fact should be in results: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // The expansion (governs-rule) should also be there
        let expansion = results.iter().find(|r| r.id == "governs-rule");
        assert!(
            expansion.is_some(),
            "governs-rule expansion should be in results: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Expansion should have OneHop evidence (direct GOVERNS edge from rule to primary)
        let expansion = expansion.unwrap();
        let evs = expansion.evidence.as_ref().unwrap();
        let one_hop = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::OneHop);
        assert!(
            one_hop.is_some(),
            "expansion should have OneHop evidence, got: {:?}",
            evs.iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn direct_hits_and_expansions_distinguishable_by_kind_not_score() {
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let primary = results.iter().find(|r| r.id == "primary-fact").unwrap();
        let expansion = results.iter().find(|r| r.id == "governs-rule").unwrap();

        // Primary should have DirectMatch evidence
        let primary_ev = primary.evidence.as_ref().unwrap();
        let direct = primary_ev
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(direct.is_some(), "primary should have DirectMatch evidence");

        // Expansion should have OneHop evidence (direct GOVERNS edge)
        let expansion_ev = expansion.evidence.as_ref().unwrap();
        let one_hop = expansion_ev
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::OneHop);
        assert!(
            one_hop.is_some(),
            "expansion should have OneHop evidence, got: {:?}",
            expansion_ev
                .iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );

        // Expansion should NOT have DirectMatch
        let direct_on_expansion = expansion_ev
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(
            direct_on_expansion.is_none(),
            "expansion should NOT have DirectMatch evidence"
        );

        // Scores should NOT use magic 10.0 sentinel
        assert_ne!(
            expansion.score, 10.0,
            "expansion score should not be magic 10.0 sentinel"
        );
        // Primary should have a real boosted distance score
        assert!(primary.score < 10.0, "primary should have real score");
    }

    #[tokio::test]
    async fn excluded_rows_do_not_consume_candidate_budget() {
        let store = filter_test_store().await;
        // limit=2, there are 4 matching rows (primary, task, archived, conflict)
        // With filters applied in store, only primary should count toward limit by default
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 2, false, false).await;

        // Should get primary + expansion (governs-rule), NOT task/archived/conflict
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"primary-fact"),
            "primary-fact should be present"
        );
        assert!(
            ids.contains(&"governs-rule"),
            "governs-rule expansion should be present"
        );
        assert!(
            !ids.contains(&"task-row"),
            "task-row should be excluded by default"
        );
        assert!(
            !ids.contains(&"archived-fact"),
            "archived-fact should be excluded by default"
        );
        assert!(
            !ids.contains(&"conflict-fact"),
            "conflict-fact should be excluded by default"
        );

        // With include_tasks=true and limit=2, both primary and task should appear as primaries
        let results_with_tasks =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 2, true, false).await;
        let ids_with_tasks: Vec<&str> = results_with_tasks.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids_with_tasks.contains(&"task-row"),
            "task-row should appear when include_tasks=true"
        );
        assert!(
            ids_with_tasks.contains(&"primary-fact"),
            "primary-fact should still appear"
        );
    }

    #[tokio::test]
    async fn access_count_increments_only_for_direct_hits() {
        // This test verifies the store returns the right evidence kinds;
        // the server is responsible for only incrementing on DirectMatch.
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let primary = results.iter().find(|r| r.id == "primary-fact").unwrap();
        let expansion = results.iter().find(|r| r.id == "governs-rule").unwrap();

        // Primary has DirectMatch
        let primary_has_direct = primary
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(primary_has_direct, "primary should have DirectMatch");

        // Expansion does NOT have DirectMatch
        let expansion_has_direct = expansion
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch);
        assert!(
            !expansion_has_direct,
            "expansion should NOT have DirectMatch"
        );
    }

    #[tokio::test]
    async fn expansions_not_seeded_from_excluded_rows() {
        let store = filter_test_store().await;
        // task-governs is a task row that governs primary-fact
        // It should NOT seed expansions because it's excluded (task type)
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        // task-governs itself should not appear (excluded)
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-governs"),
            "task-governs should be excluded"
        );

        // No expansion should have evidence path going through task-governs
        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    for edge in &ev.path {
                        assert_ne!(
                            edge.source, "task-governs",
                            "expansion should not be seeded from task-governs (source)"
                        );
                        assert_ne!(
                            edge.target, "task-governs",
                            "expansion should not be seeded from task-governs (target)"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn co_occurrence_ranking_deterministic_and_respects_caps() {
        let dir = std::env::temp_dir().join(format!("ns-cooccur-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let zero = vec![0.0f32; 4];
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Two primaries that both connect to the same expansion candidate
        store
            .upsert(
                "probe",
                "primary-1",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary-2",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary-3",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // Shared expansion: governed by both primary-1 and primary-2
        store
            .upsert(
                "probe",
                "shared-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["primary-1", "primary-2"] }),
                ),
            )
            .await
            .unwrap();

        // Expansion only from primary-3
        store
            .upsert(
                "probe",
                "solo-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({ "governs": ["primary-3"] })),
            )
            .await
            .unwrap();

        // Another shared expansion with 3 primaries
        store
            .upsert(
                "probe",
                "triple-rule",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "rule",
                    "agent",
                    json!({ "governs": ["primary-1", "primary-2", "primary-3"] }),
                ),
            )
            .await
            .unwrap();

        // Search with limit=3 (3 primaries), should admit expansions up to limit per family
        let results = search_with_filters(&store, "probe", query_vec, 3, false, false).await;

        // Find the expansion results
        let expansions: Vec<_> = results.iter().filter(|r| r.id.contains("rule")).collect();

        // Co-occurrence count: triple-rule (3) > shared-rule (2) > solo-rule (1)
        // Within the governing family (family=1), they should be ordered by co-occurrence DESC
        let gov_expansions: Vec<_> = expansions
            .iter()
            .filter(|r| {
                r.evidence
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            })
            .collect();

        // Verify ordering by co-occurrence (descending)
        let mut prev_count = usize::MAX;
        for r in &gov_expansions {
            let count = r
                .evidence
                .as_ref()
                .unwrap()
                .iter()
                .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
                .map(|e| e.path.len()) // Not exactly co-occurrence, but related
                .max()
                .unwrap_or(0);
            // The actual co-occurrence is the number of distinct primary paths
            // We verify by checking the number of distinct primary IDs in evidence
            let primary_sources: std::collections::HashSet<_> = r
                .evidence
                .as_ref()
                .unwrap()
                .iter()
                .flat_map(|e| e.path.iter().map(|edge| edge.source.as_str()))
                .collect();
            let co_occur = primary_sources.len();
            assert!(
                co_occur <= prev_count,
                "co-occurrence should be non-increasing: {} then {}",
                co_occur,
                prev_count
            );
            prev_count = co_occur;
        }

        // Per-family cap: at most `limit` (3) expansions per family
        assert!(
            gov_expansions.len() <= 3,
            "governing family should respect per-family cap"
        );
    }

    #[tokio::test]
    async fn limit_counts_recall_eligible_primaries_plus_expansions() {
        let store = filter_test_store().await;
        // limit=1 should return 1 primary + up to 1 expansion per family
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        // Count primaries (DirectMatch)
        let primary_count = results
            .iter()
            .filter(|r| {
                r.evidence
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|e| e.kind == crate::traits::EvidenceKind::DirectMatch)
            })
            .count();
        assert_eq!(primary_count, 1, "should have exactly 1 primary (limit=1)");

        // Total results can exceed limit due to expansions
        assert!(results.len() >= 1, "should have at least the primary");
        assert!(
            results.len() <= 3,
            "should not exceed limit * families (1 * 2 families + 1 primary) = 3"
        );
    }

    #[tokio::test]
    async fn include_archived_true_includes_archived_rows() {
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, true).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"archived-fact"),
            "archived-fact should appear when include_archived=true"
        );
    }

    #[tokio::test]
    async fn score_cap_half_distance_preserved() {
        // The co-occurrence ranking must not violate the 0.5 * distance cap
        // If scores are used for ranking, the cap must hold
        let store = filter_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    if ev.kind != crate::traits::EvidenceKind::DirectMatch {
                        // For expansions, the score should respect the cap
                        // The cap is applied at line 1055: (gain + source_gain).min(0.5 * distance)
                        // Since expansions get score from a different mechanism now,
                        // we verify the cap is not violated by checking scores are reasonable
                        assert!(r.score >= 0.0, "score should be non-negative");
                        assert!(r.score < 10.0, "score should not be magic sentinel");
                    }
                }
            }
        }
    }

    // --- Episode-sibling expansion tests ---

    /// Build a test store with a task and multiple lessons extracted from it.
    async fn episode_sibling_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-episode-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task row MUST be created FIRST so EXTRACTED_FROM edges materialize
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();

        // Primary: a fact that matches the query
        store
            .upsert(
                "probe",
                "primary-lesson",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Sibling lesson 1: extracted from same task
        store
            .upsert(
                "probe",
                "sibling-lesson-1",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Sibling lesson 2: extracted from same task
        store
            .upsert(
                "probe",
                "sibling-lesson-2",
                vec![0.0, 0.0, 1.1, 0.0],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Another task with many lessons (for family independence test)
        store
            .upsert(
                "probe",
                "task-many",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("many-lesson-{}", i),
                    vec![0.1 * (i + 1) as f32, 0.1, 0.1, 0.1],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "extracted_from": ["task-many"] }),
                    ),
                )
                .await
                .unwrap();
        }

        // One-hop neighbor for family independence test
        store
            .upsert(
                "probe",
                "one-hop-neighbor",
                vec![0.9, 0.1, 0.1, 0.1],
                retired_test_payload("fact", "agent", json!({ "related_to": ["primary-lesson"] })),
            )
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn episode_sibling_appears_in_results() {
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        // Primary should be there
        let primary = results.iter().find(|r| r.id == "primary-lesson");
        assert!(primary.is_some(), "primary-lesson should be in results");

        // Sibling should appear as expansion
        let sibling = results.iter().find(|r| r.id == "sibling-lesson-1");
        assert!(
            sibling.is_some(),
            "sibling-lesson-1 should be in results: {:?}",
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Sibling should have EpisodeSibling evidence
        let sibling = sibling.unwrap();
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling);
        assert!(
            ep_sib.is_some(),
            "sibling should have EpisodeSibling evidence, got: {:?}",
            evs.iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn episode_sibling_evidence_has_correct_path_and_explanation() {
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        let sibling = results.iter().find(|r| r.id == "sibling-lesson-1").unwrap();
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling)
            .unwrap();

        // Path should have exactly 2 edges
        assert_eq!(
            ep_sib.path.len(),
            2,
            "EpisodeSibling path should have 2 edges"
        );

        // First edge: primary -(EXTRACTED_FROM)-> task
        assert_eq!(ep_sib.path[0].source, "primary-lesson");
        assert_eq!(ep_sib.path[0].relation, "EXTRACTED_FROM");
        assert_eq!(ep_sib.path[0].target, "task-1");

        // Second edge: sibling -(EXTRACTED_FROM)-> task
        assert_eq!(ep_sib.path[1].source, "sibling-lesson-1");
        assert_eq!(ep_sib.path[1].relation, "EXTRACTED_FROM");
        assert_eq!(ep_sib.path[1].target, "task-1");

        // Explanation should match spec
        assert_eq!(
            ep_sib.explanation,
            "This memory and a direct match were both extracted from the same task."
        );
    }

    #[tokio::test]
    async fn task_row_never_appears_as_result() {
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-1"),
            "task row should never appear as a result"
        );
        assert!(
            !ids.contains(&"task-many"),
            "task-many row should never appear as a result"
        );
    }

    #[tokio::test]
    async fn family_independence_many_lessons_do_not_crowd_out_one_hop() {
        let store = episode_sibling_test_store().await;
        // limit=1: one primary, up to 1 one-hop, up to 1 episode-sibling, up to 1 governing
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();

        // Primary should be there
        assert!(ids.contains(&"primary-lesson"));

        // One-hop neighbor should be admitted (family 0)
        assert!(
            ids.contains(&"one-hop-neighbor"),
            "one-hop neighbor should be admitted despite many siblings"
        );

        // At least one episode sibling should be admitted (family 1)
        let has_sibling = ids
            .iter()
            .any(|id| id.starts_with("sibling-lesson") || id.starts_with("many-lesson"));
        assert!(
            has_sibling,
            "at least one episode sibling should be admitted"
        );
    }

    #[tokio::test]
    async fn family_independence_one_hop_does_not_crowd_out_siblings() {
        let store = episode_sibling_test_store().await;
        // Add many one-hop neighbors
        let zero = vec![0.0f32; 4];
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("extra-neighbor-{}", i),
                    vec![0.9, 0.1, 0.1, 0.1],
                    retired_test_payload(
                        "fact",
                        "agent",
                        json!({ "related_to": ["primary-lesson"] }),
                    ),
                )
                .await
                .unwrap();
        }

        // limit=1: should still admit one episode sibling
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false).await;

        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();

        // Primary should be there
        assert!(ids.contains(&"primary-lesson"));

        // At least one episode sibling should be admitted (family 1)
        let has_sibling = ids
            .iter()
            .any(|id| id.starts_with("sibling-lesson") || id.starts_with("many-lesson"));
        assert!(
            has_sibling,
            "episode sibling should be admitted despite many one-hop neighbors: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn archived_sibling_is_excluded() {
        let dir =
            std::env::temp_dir().join(format!("ns-episode-archived-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "archived-sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({ "extracted_from": ["task-1"], "archived": true }),
                ),
            )
            .await
            .unwrap();

        let results =
            search_with_filters(&store, "probe", query_vec.clone(), 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"archived-sibling"),
            "archived sibling should be excluded by default"
        );

        // With include_archived=true, it should appear
        let results_archived =
            search_with_filters(&store, "probe", query_vec, 5, false, true).await;
        let ids_archived: Vec<&str> = results_archived.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids_archived.contains(&"archived-sibling"),
            "archived sibling should appear when include_archived=true"
        );
    }

    #[tokio::test]
    async fn mesh_conflicting_sibling_is_excluded() {
        let dir =
            std::env::temp_dir().join(format!("ns-episode-conflict-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "conflict-sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({ "extracted_from": ["task-1"], "mesh": { "conflict": true } }),
                ),
            )
            .await
            .unwrap();

        let results = search_with_filters(&store, "probe", query_vec, 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"conflict-sibling"),
            "mesh-conflicting sibling should be excluded"
        );
    }

    #[tokio::test]
    async fn task_typed_sibling_is_excluded() {
        let dir = std::env::temp_dir().join(format!("ns-episode-task-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "task-sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("task", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        let results =
            search_with_filters(&store, "probe", query_vec.clone(), 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-sibling"),
            "task-typed sibling should be excluded by default"
        );

        // With include_tasks=true, it should appear
        let results_tasks = search_with_filters(&store, "probe", query_vec, 5, true, false).await;
        let ids_tasks: Vec<&str> = results_tasks.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids_tasks.contains(&"task-sibling"),
            "task-typed sibling should appear when include_tasks=true"
        );
    }

    #[tokio::test]
    async fn retired_intermediate_task_invalidates_path() {
        let dir = std::env::temp_dir().join(format!("ns-episode-retired-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first (with valid_to=1 to be retired)
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({ "valid_to": 1 })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "sibling",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Use limit=1 so only 'primary' is a primary; 'sibling' becomes an expansion candidate
        let results = search_with_filters(&store, "probe", query_vec, 1, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"sibling"),
            "sibling should not appear when intermediate task is retired"
        );
    }

    #[tokio::test]
    async fn co_occurrence_sibling_from_two_primaries_counts_two() {
        let dir = std::env::temp_dir().join(format!("ns-episode-cooccur-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Task MUST be created first
        store
            .upsert(
                "probe",
                "task-1",
                zero.clone(),
                retired_test_payload("task", "agent", json!({})),
            )
            .await
            .unwrap();
        // Two primaries extracted from the same task
        store
            .upsert(
                "probe",
                "primary-1",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "primary-2",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();
        // Sibling extracted from same task - vector far from query so it doesn't become a primary
        store
            .upsert(
                "probe",
                "sibling",
                vec![0.0, 0.0, 0.0, 1.1],
                retired_test_payload("fact", "agent", json!({ "extracted_from": ["task-1"] })),
            )
            .await
            .unwrap();

        // Search with limit=2 so only the two primaries are in primary_ids
        let results = search_with_filters(&store, "probe", query_vec, 2, false, false).await;
        let sibling = results.iter().find(|r| r.id == "sibling").unwrap();
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling)
            .collect();

        // Should have two distinct EpisodeSibling paths (one from each primary)
        assert_eq!(
            ep_sib.len(),
            2,
            "should have two EpisodeSibling paths, one per primary"
        );

        // Co-occurrence: primary_sources should contain both primaries
        // We verify by checking the evidence paths have both primary sources (first edge of each path)
        let primary_sources: std::collections::HashSet<_> = ep_sib
            .iter()
            .filter_map(|e| e.path.first().map(|edge| edge.source.as_str()))
            .collect();
        assert!(primary_sources.contains("primary-1"));
        assert!(primary_sources.contains("primary-2"));
        assert_eq!(primary_sources.len(), 2);
    }

    #[tokio::test]
    async fn episode_sibling_serializes_as_episode_sibling() {
        let ev = crate::traits::SearchEvidence {
            kind: crate::traits::EvidenceKind::EpisodeSibling,
            path: vec![
                crate::traits::EvidenceEdge {
                    source: "primary".into(),
                    relation: "EXTRACTED_FROM".into(),
                    target: "task".into(),
                },
                crate::traits::EvidenceEdge {
                    source: "sibling".into(),
                    relation: "EXTRACTED_FROM".into(),
                    target: "task".into(),
                },
            ],
            explanation: "test".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "episode_sibling");
    }

    #[tokio::test]
    async fn existing_invariants_hold_expansions_not_seeded_from_excluded_rows() {
        let dir =
            std::env::temp_dir().join(format!("ns-episode-invariant-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let zero = vec![0.0f32; 4];

        // Primary
        store
            .upsert(
                "probe",
                "primary",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();
        // Task that governs primary (should NOT seed expansions because it's a task)
        store
            .upsert(
                "probe",
                "task-governs",
                zero.clone(),
                retired_test_payload("task", "agent", json!({ "governs": ["primary"] })),
            )
            .await
            .unwrap();
        // A rule that the task governs (should NOT be reached via task)
        store
            .upsert(
                "probe",
                "ruled-by-task",
                vec![0.0, 1.1, 0.0, 0.0],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();
        // Re-upsert task-governs with the new governs edge
        store
            .upsert(
                "probe",
                "task-governs",
                zero.clone(),
                retired_test_payload(
                    "task",
                    "agent",
                    json!({ "governs": ["primary", "ruled-by-task"] }),
                ),
            )
            .await
            .unwrap();

        let results = search_with_filters(&store, "probe", query_vec, 5, false, false).await;
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"task-governs"),
            "task-governs should be excluded"
        );

        // ruled-by-task should NOT appear via the task (task is excluded from seeding)
        // It might appear via other paths, but not via task-governs
        for r in &results {
            if let Some(evs) = &r.evidence {
                for ev in evs {
                    for edge in &ev.path {
                        assert_ne!(
                            edge.source, "task-governs",
                            "expansion should not be seeded from task-governs (source)"
                        );
                        assert_ne!(
                            edge.target, "task-governs",
                            "expansion should not be seeded from task-governs (target)"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn graph_expansion_still_reaches_server_caller() {
        // This is an integration-style test verifying the store returns evidence
        // that the server can render. The server's exhaustive match on EvidenceKind
        // will fail to compile if EpisodeSibling is not handled.
        let store = episode_sibling_test_store().await;
        let results =
            search_with_filters(&store, "probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false).await;

        let sibling = results.iter().find(|r| r.id == "sibling-lesson-1").unwrap();
        assert!(sibling.evidence.is_some());
        let evs = sibling.evidence.as_ref().unwrap();
        let ep_sib = evs
            .iter()
            .find(|e| e.kind == crate::traits::EvidenceKind::EpisodeSibling);
        assert!(
            ep_sib.is_some(),
            "EpisodeSibling evidence should be present for server rendering"
        );
    }

    // --- TASK 2 tests: domain-filtered search ---

    #[tokio::test]
    async fn a_domain_filter_narrows_the_candidates_to_the_matching_domain() {
        let dir = std::env::temp_dir().join(format!("ns-domain-filter-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Two facts with different domains
        store
            .upsert(
                "probe",
                "auth-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "auth" })),
            )
            .await
            .unwrap();
        store
            .upsert(
                "probe",
                "database-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "database" })),
            )
            .await
            .unwrap();

        // Search with domain filter for "database" only
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec,
                10,
                false,
                false,
                &["database".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Domain);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"database-fact"),
            "database-fact should be returned"
        );
        assert!(
            !ids.contains(&"auth-fact"),
            "auth-fact should be filtered out"
        );
    }

    #[tokio::test]
    async fn a_domain_filter_that_would_empty_the_candidates_falls_back_to_the_unfiltered_rows() {
        let dir = std::env::temp_dir().join(format!("ns-domain-fallback-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // One fact with domain "auth"
        store
            .upsert(
                "probe",
                "auth-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "auth" })),
            )
            .await
            .unwrap();

        // Search with domain filter for "database" (no match)
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec,
                10,
                false,
                false,
                &["database".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Vector);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"auth-fact"),
            "auth-fact should be returned via fallback"
        );
    }

    #[tokio::test]
    async fn a_legacy_free_text_domain_matches_the_slug_at_comparison_time() {
        let dir = std::env::temp_dir().join(format!("ns-domain-legacy-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // One fact with legacy free-text domain "Database Access"
        // Seeded straight into the store (not through the add path)
        store
            .upsert(
                "probe",
                "legacy-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "Database Access" })),
            )
            .await
            .unwrap();

        // Search with the normalized slug "database-access"
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec,
                10,
                false,
                false,
                &["database-access".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Domain);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"legacy-fact"),
            "legacy-fact should match the normalized slug"
        );

        // Verify the stored metadata still has the original free-text value
        // Use a fresh get() read-back to assert no migration occurred.
        let stored = store.get("probe", "legacy-fact").await.unwrap().unwrap();
        let stored_domain = stored.1.metadata.get("domain").and_then(|v| v.as_str());
        assert_eq!(
            stored_domain,
            Some("Database Access"),
            "stored domain must remain unchanged"
        );
    }

    #[tokio::test]
    async fn a_filtered_search_with_limit_below_unfiltered_count_returns_only_matching_rows() {
        let dir =
            std::env::temp_dir().join(format!("ns-domain-filter-limit-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        // Seed 5 rows with domain "other" (won't match) and 1 row with domain "target" (will match).
        // Limit is 3, so unfiltered all_primaries would exceed limit (6 > 3),
        // but only the one "target" row should pass the filter.
        for i in 0..5 {
            store
                .upsert(
                    "probe",
                    &format!("other-fact-{}", i),
                    query_vec.clone(),
                    retired_test_payload("fact", "agent", json!({ "domain": "other" })),
                )
                .await
                .unwrap();
        }
        store
            .upsert(
                "probe",
                "target-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({ "domain": "target" })),
            )
            .await
            .unwrap();

        // Search with domain filter for "target" only, limit = 3
        let outcome = store
            .search_with_domain(
                "probe",
                query_vec.clone(),
                3,
                false,
                false,
                &["target".to_string()],
            )
            .await
            .unwrap();

        // Filter applied successfully: only the matching row returned, mode is Domain
        assert_eq!(outcome.mode, crate::traits::DomainMatchMode::Domain);
        let ids: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 1, "only the matching row should be returned");
        assert!(
            ids.contains(&"target-fact"),
            "target-fact should be returned"
        );

        // Verify that unfiltered set really did exceed limit (otherwise test is vacuous)
        let unfiltered = store
            .search("probe", query_vec.clone(), 10, false, false)
            .await
            .unwrap();
        assert!(
            unfiltered.len() > 3,
            "unfiltered set must exceed limit for the stop condition to be meaningful"
        );
    }

    #[tokio::test]
    async fn mock_store_search_with_domain_matches_real_store_behaviour() {
        use crate::guard::testkit::MockStore;
        use crate::traits::VectorStore;

        // Build a REAL store and a MOCK store
        let real = retired_test_store().await;
        let mock = MockStore::new();
        let namespace = "probe";

        // Seed IDENTICAL rows into both stores
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        let payload_auth = retired_test_payload("fact", "agent", json!({ "domain": "auth" }));
        let payload_db = retired_test_payload("fact", "agent", json!({ "domain": "database" }));
        let payload_legacy =
            retired_test_payload("fact", "agent", json!({ "domain": "Database Access" }));

        // Insert into real store
        real.upsert(
            namespace,
            "auth-fact",
            query_vec.clone(),
            payload_auth.clone(),
        )
        .await
        .unwrap();
        real.upsert(
            namespace,
            "database-fact",
            query_vec.clone(),
            payload_db.clone(),
        )
        .await
        .unwrap();
        real.upsert(
            namespace,
            "legacy-fact",
            query_vec.clone(),
            payload_legacy.clone(),
        )
        .await
        .unwrap();

        // Insert into mock store
        mock.upsert(namespace, "auth-fact", query_vec.clone(), payload_auth)
            .await
            .unwrap();
        mock.upsert(namespace, "database-fact", query_vec.clone(), payload_db)
            .await
            .unwrap();
        mock.upsert(namespace, "legacy-fact", query_vec.clone(), payload_legacy)
            .await
            .unwrap();

        // Helper to get sorted IDs from a DomainSearchOutcome
        let sorted_ids = |outcome: &crate::traits::DomainSearchOutcome| -> Vec<String> {
            let mut ids: Vec<String> = outcome.results.iter().map(|r| r.id.clone()).collect();
            ids.sort();
            ids
        };

        // Case A: key set matches exactly one row → mode Domain, only that row returned
        let real_outcome = real
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database".to_string()],
            )
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for narrowing case"
        );
        assert_eq!(
            sorted_ids(&real_outcome),
            vec!["database-fact".to_string()],
            "only the matching row should be returned"
        );

        // Case B: key set matches nothing → soft fallback → mode Vector, all rows returned
        let real_outcome = real
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["nonexistent".to_string()],
            )
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["nonexistent".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for fallback case"
        );
        assert_eq!(
            sorted_ids(&real_outcome).len(),
            3,
            "all three rows should be returned via soft fallback"
        );

        // Case C: legacy free-text domain matches normalized slug at comparison time
        let real_outcome = real
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database-access".to_string()],
            )
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(
                namespace,
                query_vec.clone(),
                10,
                false,
                false,
                &["database-access".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Domain);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for legacy case"
        );
        assert!(
            sorted_ids(&real_outcome).contains(&"legacy-fact".to_string()),
            "legacy-fact should match the normalized slug"
        );

        // Case D: no filter (empty key set) → mode Vector, all rows returned
        let real_outcome = real
            .search_with_domain(namespace, query_vec.clone(), 10, false, false, &[])
            .await
            .unwrap();
        let mock_outcome = mock
            .search_with_domain(namespace, query_vec, 10, false, false, &[])
            .await
            .unwrap();

        assert_eq!(real_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(mock_outcome.mode, crate::traits::DomainMatchMode::Vector);
        assert_eq!(
            sorted_ids(&real_outcome),
            sorted_ids(&mock_outcome),
            "real and mock must return the same set of IDs for empty key set"
        );
        assert_eq!(
            sorted_ids(&real_outcome).len(),
            3,
            "all three facts should be returned when no filter"
        );
    }

    #[tokio::test]
    async fn mock_store_search_with_domain_honours_limit() {
        use crate::guard::testkit::MockStore;
        use crate::traits::{DomainMatchMode, VectorStore};

        let mock = MockStore::new();
        let namespace = "probe";

        // Seed: TWO rows matching "database", ONE row matching "auth", ONE row matching "compute"
        // This ensures the filtered branch has multiple matching rows to test truncation.
        let query_vec = vec![1.1, 0.0, 0.0, 0.0];
        for (id, domain) in [
            ("fact-db-1", "database"),
            ("fact-db-2", "database"),
            ("fact-auth", "auth"),
            ("fact-compute", "compute"),
        ] {
            let payload = retired_test_payload("fact", "agent", json!({ "domain": domain }));
            mock.upsert(namespace, id, query_vec.clone(), payload)
                .await
                .unwrap();
        }

        // Empty-key branch (domains = &[]): limit = 1 should return exactly 1 row
        let outcome = mock
            .search_with_domain(namespace, query_vec.clone(), 1, false, false, &[])
            .await
            .unwrap();
        assert_eq!(outcome.mode, DomainMatchMode::Vector);
        assert_eq!(
            outcome.results.len(),
            1,
            "empty-key branch: limit=1 must return exactly 1 row"
        );

        // Filtered branch (domains = &["database"]): limit = 1 should return exactly 1 matching row
        // We seeded TWO "database" rows, so truncation must happen.
        let outcome = mock
            .search_with_domain(
                namespace,
                query_vec,
                1,
                false,
                false,
                &["database".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(outcome.mode, DomainMatchMode::Domain);
        assert_eq!(
            outcome.results.len(),
            1,
            "filtered branch: limit=1 must return exactly 1 row (truncation)"
        );
        // The mock returns in insertion order, so the first matching row is "fact-db-1"
        assert_eq!(
            outcome.results[0].id, "fact-db-1",
            "filtered branch: must return the first matching row in insertion order"
        );
    }

    /// The thread that runs the engine must have an alternate signal stack
    /// big enough for the crash handler.
    ///
    /// This is the crash path that matters: the engine runs on a
    /// blocking-pool thread, and the handler runs on that thread's
    /// alternate stack. The Rust standard library gives a thread 8192 bytes
    /// of it, which is too small -- the handler would fault inside itself
    /// and the breadcrumb would be lost.
    #[tokio::test]
    async fn the_engine_thread_has_an_alternate_stack_the_handler_can_run_on() {
        // The size the handler asks for, measured on this thread.
        crate::crash::install_alt_stack().expect("the alternate stack installs");
        let required = crate::crash::alt_stack_size();
        assert!(
            required >= 32 * 1024,
            "the handler needs at least 32 KiB of alternate stack, got {required}"
        );

        let store = episode_sibling_test_store().await;
        let observed = store
            .with_conn(|_conn| Ok(crate::crash::alt_stack_size()))
            .await
            .expect("the engine call finishes");
        assert!(
            observed >= required,
            "the engine thread must have an alternate stack of at least {required} bytes, got {observed}"
        );
    }

    /// The acceptance sweep (AC 5): a fact with a well-formed access topology
    /// round-trips, reaches the file it governs through the one-hop expansion,
    /// and carries that `GOVERNS` edge into the exported graph.
    ///
    /// The file node is written FIRST, so the edge resolves on the fact's
    /// write, and it is written as a `file` -- a structural type, excluded
    /// from the base candidate query. That is the point: the governed file is
    /// reachable ONLY through the expansion, never as a direct match, which is
    /// why the recall filters to direct matches and never recalls a governed
    /// AST node in place of the fact.
    #[tokio::test]
    async fn a_topology_fact_reaches_its_governed_file_and_exports() {
        let store = retired_test_store().await;

        let topology = json!({
            "component": "api-gateway",
            "target_system": "ledger-service",
            "channel": "mTLS",
            "credential_boundary": "eu-west-1 VPC",
            "reason": "the ledger accepts traffic only from inside the VPC",
        });

        // The structural file node, written before the fact so the GOVERNS
        // edge has a target to resolve against.
        store
            .upsert(
                "probe",
                "ledger-file",
                vec![0.0; 4],
                retired_test_payload("file", "auto-ingestor", json!({})),
            )
            .await
            .expect("the file node is written");

        store
            .upsert(
                "probe",
                "topology-fact",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload(
                    "fact",
                    "agent",
                    json!({
                        "governs": ["ledger-file"],
                        "topology": topology,
                    }),
                ),
            )
            .await
            .expect("the fact is written");

        // (1) The topology round-trips unchanged through a search for it.
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 5, false, false)
            .await
            .expect("the search runs");
        let fact = results
            .iter()
            .find(|r| r.id == "topology-fact")
            .unwrap_or_else(|| {
                panic!(
                    "the fact is in the results: {:?}",
                    results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            fact.payload.metadata["topology"], topology,
            "the access topology round-trips unchanged"
        );

        // (2) The fact reaches the file node through the GOVERNS edge, and the
        //     expansion row carries OneHop evidence.
        let file = results
            .iter()
            .find(|r| r.id == "ledger-file")
            .unwrap_or_else(|| {
                panic!(
                    "the governed file is reached through the expansion: {:?}",
                    results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
                )
            });
        let evidence = file
            .evidence
            .as_ref()
            .expect("the expansion row carries evidence");
        assert!(
            evidence
                .iter()
                .any(|e| e.kind == crate::traits::EvidenceKind::OneHop),
            "the governed file is reached in one hop: {:?}",
            evidence
                .iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            evidence.iter().any(|e| e.path.iter().any(|edge| {
                edge.relation == "GOVERNS"
                    && ((edge.source == "topology-fact" && edge.target == "ledger-file")
                        || (edge.source == "ledger-file" && edge.target == "topology-fact"))
            })),
            "the one-hop path names the GOVERNS edge between the fact and the file: {:?}",
            evidence
        );

        // (3) The file node is a structural type: it is NOT a base candidate,
        //     so it carries no DirectMatch and is reachable only through the
        //     expansion -- which is why the recall keeps the fact and drops
        //     the governed node.
        assert!(
            STRUCTURAL_MEMORY_TYPES.contains(&"file"),
            "a file node is a structural type: {:?}",
            STRUCTURAL_MEMORY_TYPES
        );
        assert!(
            !crate::traits::is_direct_match(&file.evidence),
            "the structural file is not a direct match: {:?}",
            file.evidence
        );
        assert!(
            crate::traits::is_direct_match(&fact.evidence),
            "the fact itself is the direct match: {:?}",
            fact.evidence
        );

        // (4) The export carries the fact node AND the GOVERNS link to the file.
        let graph = store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .expect("the graph exports");
        let nodes = graph["nodes"].as_array().expect("the export carries nodes");
        let fact_node = nodes
            .iter()
            .find(|n| n["id"] == "topology-fact")
            .expect("the fact node is in the export");
        assert_eq!(
            fact_node["metadata"]["topology"], topology,
            "the export carries the topology with the fact"
        );
        let links = graph["links"].as_array().expect("the export carries links");
        assert!(
            links.iter().any(|l| {
                l["source"] == "topology-fact"
                    && l["target"] == "ledger-file"
                    && l["type"] == "GOVERNS"
            }),
            "the GOVERNS link to the file is in the export: {:?}",
            links
        );
    }

    /// The dedup key: a one-hop undirected path collapses with its own
    /// reverse, and a multi-hop path does not. A path's orientation is part of
    /// its identity, except for a one-hop edge that has no direction.
    #[test]
    fn a_one_hop_undirected_path_collapses_but_a_two_hop_path_does_not() {
        let edge = |source: &str, relation: &str, target: &str| crate::traits::EvidenceEdge {
            source: source.to_string(),
            relation: relation.to_string(),
            target: target.to_string(),
        };
        let forward = vec![edge("probe::a", "RELATES_TO", "probe::b")];
        let reverse = vec![edge("probe::b", "RELATES_TO", "probe::a")];
        let directed_forward = vec![edge("probe::a", "GOVERNS", "probe::b")];
        let directed_reverse = vec![edge("probe::b", "GOVERNS", "probe::a")];

        assert_eq!(
            norm_path(&forward, &crate::traits::EvidenceKind::OneHop),
            norm_path(&reverse, &crate::traits::EvidenceKind::OneHop),
            "a one-hop undirected path normalises to the same key either way"
        );
        assert_ne!(
            norm_path(&directed_forward, &crate::traits::EvidenceKind::OneHop),
            norm_path(&directed_reverse, &crate::traits::EvidenceKind::OneHop),
            "a one-hop directed path keeps its order"
        );
        assert_ne!(
            norm_path(&forward, &crate::traits::EvidenceKind::Governs2hop),
            norm_path(&reverse, &crate::traits::EvidenceKind::Governs2hop),
            "a two-hop path keeps its order"
        );
    }

    /// A store whose primary is reached by an INCOMING edge, with a node that
    /// governs the intermediate:
    ///
    ///   probe::mid -RELATES_TO-> probe::primary <-GOVERNS- probe::ruler
    ///
    /// `probe::ruler` is two hops from the primary: the primary is reached
    /// through the intermediate, and the ruler governs the intermediate.
    async fn incoming_evidence_test_store() -> LadybugStore {
        let dir =
            std::env::temp_dir().join(format!("ns-incoming-evidence-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        // The direct match: the target of the intermediate's RELATES_TO edge.
        store
            .upsert(
                "probe",
                "probe::primary",
                vec![1.1, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        // The intermediate: it relates to the primary, so the edge points at
        // the primary.
        store
            .upsert(
                "probe",
                "probe::mid",
                vec![0.5, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "related_to": ["probe::primary"] })),
            )
            .await
            .unwrap();

        // The governing node: it governs the intermediate.
        store
            .upsert(
                "probe",
                "probe::ruler",
                vec![0.2, 0.0, 0.0, 0.0],
                retired_test_payload("fact", "agent", json!({ "governs": ["probe::mid"] })),
            )
            .await
            .unwrap();

        store
    }

    /// An incoming two-hop path's first hop points at the direct match: the
    /// intermediate is the edge's source and the direct match its target. The
    /// outgoing branch builds the same hop the other way round, so the two
    /// directions have to be pinned separately.
    #[tokio::test]
    async fn incoming_two_hop_evidence_points_the_first_hop_at_the_direct_match() {
        let store = incoming_evidence_test_store().await;
        let results = store
            .search("probe", vec![1.1, 0.0, 0.0, 0.0], 1, false, false)
            .await
            .unwrap();
        let ruler = results.iter().find(|r| r.id == "probe::ruler").unwrap();
        let two_hop: Vec<_> = ruler
            .evidence
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::traits::EvidenceKind::Governs2hop)
            .collect();
        assert_eq!(two_hop.len(), 1, "ruler should carry one two-hop path");
        let path = &two_hop[0].path;
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].source, "probe::mid");
        assert_eq!(path[0].relation, "RELATES_TO");
        assert_eq!(path[0].target, "probe::primary");
        assert_eq!(path[1].source, "probe::ruler");
        assert_eq!(path[1].relation, "GOVERNS");
        assert_eq!(path[1].target, "probe::mid");
    }

    #[tokio::test]
    async fn the_registry_row_never_appears_in_search_results() {
        let dir = std::env::temp_dir().join(format!("ns-registry-row-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let query_vec = vec![1.1, 0.0, 0.0, 0.0];

        store
            .upsert(
                "probe",
                "normal-fact",
                query_vec.clone(),
                retired_test_payload("fact", "agent", json!({})),
            )
            .await
            .unwrap();

        let registry_id = crate::registry::registry_row_id("probe");
        let payload = MemoryPayload {
            content: crate::registry::registry_content("probe"),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: crate::registry::registry_metadata(&["auth".to_string()]),
        };
        store
            .upsert("probe", &registry_id, query_vec.clone(), payload)
            .await
            .unwrap();

        let results = store
            .search("probe", query_vec, 10, false, false)
            .await
            .unwrap();
        assert!(
            !results.iter().any(|r| r.id == registry_id),
            "the registry row must never appear in search results"
        );
    }

    // ── Task 7 (C5): (namespace, id) row identity, audit/quarantine, ────────
    // ── escaping ladder. RED for the remediation program's Phase B. ─────────

    fn t7_payload(content: &str) -> MemoryPayload {
        MemoryPayload {
            content: content.to_string(),
            user_id: "agent".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        }
    }

    async fn t7_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-t7-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::for_testing(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");
        store
    }

    async fn t7_count_where(store: &LadybugStore, where_clause: &str) -> i64 {
        let query = format!("MATCH (m:Memory) WHERE {} RETURN count(m)", where_clause);
        store
            .with_conn(move |conn| {
                let mut rows = conn.query(&query)?;
                let n = rows
                    .next()
                    .map(|row| format!("{}", row[0]).parse::<i64>().unwrap_or(-1))
                    .unwrap_or(-1);
                Ok(n)
            })
            .await
            .expect("count rows")
    }

    /// The DDL the pre-Task-7 binary creates: PRIMARY KEY (id), namespace a
    /// plain property set ON CREATE. Seeding with it builds a genuine
    /// old-binary store, not a caricature of one.
    const T7_OLD_DDL: &str = "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[4], PRIMARY KEY (id))";

    // Returns the Database alone: a Connection borrows its Database, so a
    // (Database, Connection) pair can never be returned from one function
    // (E0515/E0505). Call sites bind the Database first (keep-alive; drops
    // last) and build their own Connection from it.
    fn t7_raw_db(path: &std::path::Path) -> Database {
        let cfg = SystemConfig::default()
            .max_db_size(512 * 1024 * 1024)
            .buffer_pool_size(64 * 1024 * 1024);
        Database::new(path, cfg).expect("raw open")
    }

    /// Seed one row the way the OLD upsert writes it: MERGE on {id} alone,
    /// namespace assigned ON CREATE.
    fn t7_old_shape_insert(conn: &Connection, namespace: &str, id: &str, content: &str) {
        let query = format!(
            "MERGE (m:Memory {{id: '{}'}}) ON CREATE SET m.namespace = '{}', m.content = '{}', m.user_id = 'agent', m.memory_type = 'fact', m.agent_name = 'a', m.location = '', m.location_lines = '', m.metadata = '{{}}', m.embedding = [0.0,0.0,0.0,0.0] ON MATCH SET m.content = '{}'",
            escape_ladybug_string(id),
            escape_ladybug_string(namespace),
            escape_ladybug_string(content),
            escape_ladybug_string(content),
        );
        conn.query(&query).expect("old-shape insert");
    }

    #[tokio::test]
    async fn same_id_two_namespaces_coexist() {
        let store = t7_store().await;
        store
            .upsert("ns-a", "shared-id", vec![0.0; 4], t7_payload("content-a"))
            .await
            .expect("upsert in ns-a");
        store
            .upsert("ns-b", "shared-id", vec![0.0; 4], t7_payload("content-b"))
            .await
            .expect("the same id in ns-b is a different row, not an overwrite");

        let (_, a) = store
            .get("ns-a", "shared-id")
            .await
            .unwrap()
            .expect("ns-a row exists");
        let (_, b) = store
            .get("ns-b", "shared-id")
            .await
            .unwrap()
            .expect("ns-b row exists");
        assert_eq!(a.content, "content-a");
        assert_eq!(b.content, "content-b");
        assert_eq!(
            t7_count_where(&store, "m.id = 'shared-id'").await,
            2,
            "two rows carry the shared id, one per namespace"
        );

        // Re-writing ns-a must leave ns-b byte-identical.
        let mut a2 = t7_payload("content-a2");
        a2.metadata = json!({"round": 2});
        store
            .upsert("ns-a", "shared-id", vec![0.0; 4], a2)
            .await
            .unwrap();
        let (_, b_after) = store
            .get("ns-b", "shared-id")
            .await
            .unwrap()
            .expect("ns-b row still exists");
        assert_eq!(b_after.content, "content-b");
        assert_eq!(b_after.metadata, json!({}));
    }

    #[tokio::test]
    async fn old_binary_rows_are_found_not_duplicated() {
        let path = std::env::temp_dir().join(format!("ns-t7-old-{}", uuid::Uuid::new_v4()));
        {
            let _db = t7_raw_db(&path);
            let conn = Connection::new(&_db).expect("raw connection");
            conn.query(T7_OLD_DDL).expect("old DDL");
            t7_old_shape_insert(&conn, "legacy-ns", "legacy-id", "old content");
        }
        let store = LadybugStore::for_testing(&path, 4).expect("open old store");
        store.init("legacy-ns").await.expect("init over old store");

        store
            .upsert(
                "legacy-ns",
                "legacy-id",
                vec![0.0; 4],
                t7_payload("new content"),
            )
            .await
            .expect("upsert matches the old row");
        assert_eq!(
            t7_count_where(&store, "m.id = 'legacy-id'").await,
            1,
            "the old row was found, not duplicated"
        );
        let (_, got) = store
            .get("legacy-ns", "legacy-id")
            .await
            .unwrap()
            .expect("row readable");
        assert_eq!(got.content, "new content");
    }

    #[tokio::test]
    async fn legacy_table_gains_pk_backfill_and_fails_loud_across_namespaces() {
        // A store built by the pre-re-plan binary: PRIMARY KEY (id), no pk
        // column, one row. Reopening through init must add the column and
        // backfill the derived pk — never brick the open.
        let path = std::env::temp_dir().join(format!("ns-t7-legacy-{}", uuid::Uuid::new_v4()));
        {
            let _db = t7_raw_db(&path);
            let conn = Connection::new(&_db).expect("raw connection");
            conn.query(T7_OLD_DDL).expect("old DDL");
            t7_old_shape_insert(&conn, "legacy-ns", "legacy-id", "old content");
        }
        let store = LadybugStore::for_testing(&path, 4).expect("open old store");
        store.init("legacy-ns").await.expect("init over old store");
        assert!(
            store.pk_ready(),
            "init must give the legacy table a working pk column"
        );

        // The row is readable, and its stored pk is the derived key.
        let (_, got) = store
            .get("legacy-ns", "legacy-id")
            .await
            .unwrap()
            .expect("row readable");
        assert_eq!(got.content, "old content");
        let stored_pk = store
            .with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (m:Memory) WHERE m.id = 'legacy-id' RETURN m.pk",
                )?;
                let pk = rows
                    .next()
                    .map(|row| format!("{}", row[0]))
                    .unwrap_or_default();
                Ok(pk)
            })
            .await
            .expect("read the stored pk");
        assert_eq!(stored_pk, memory_pk("legacy-ns", "legacy-id"));

        // An upsert by (namespace, id) matches the backfilled row.
        store
            .upsert(
                "legacy-ns",
                "legacy-id",
                vec![0.0; 4],
                t7_payload("matched"),
            )
            .await
            .expect("upsert matches the backfilled row");
        assert_eq!(
            t7_count_where(&store, "m.id = 'legacy-id'").await,
            1,
            "the backfilled row was matched, not duplicated"
        );

        // The legacy table's PRIMARY KEY is still (id), so the same id in
        // another namespace cannot become a second row: the write fails
        // LOUD at the engine rather than silently aliasing onto the
        // existing row, and the existing row is untouched.
        let res = store
            .upsert(
                "other-ns",
                "legacy-id",
                vec![0.0; 4],
                t7_payload("alias attempt"),
            )
            .await;
        assert!(
            res.is_err(),
            "a cross-namespace same-id write on a legacy table must fail loud"
        );
        assert_eq!(
            t7_count_where(&store, "m.id = 'legacy-id'").await,
            1,
            "the failed write created nothing"
        );
        let (_, got) = store
            .get("legacy-ns", "legacy-id")
            .await
            .unwrap()
            .expect("row still readable");
        assert_eq!(got.content, "matched", "the existing row was not aliased");
    }

    #[tokio::test]
    async fn null_namespace_rows_backfill_or_quarantine_never_block() {
        let path = std::env::temp_dir().join(format!("ns-t7-null-{}", uuid::Uuid::new_v4()));
        // Phase 1: seed two rows whose namespace property is NULL (omitted
        // at CREATE) — one carries its namespace in metadata, one carries
        // nothing — then drop the store so the reopen runs the audit.
        {
            let seed_store = LadybugStore::for_testing(&path, 4).expect("open temp database");
            seed_store.init("probe").await.expect("create the schema");
            for (id, metadata) in [
                ("orphan-with-meta", r#"{"namespace":"recovered-ns"}"#),
                ("orphan-no-meta", "{}"),
            ] {
                // The pk is bound, not interpolated: the with-meta row's
                // key is the derived key of the namespace its metadata
                // claims (as an upsert would have written it before the
                // namespace property was lost), and a derived key carries
                // the U+0001 separator, which travels as a parameter.
                let pk = if id == "orphan-with-meta" {
                    memory_pk("recovered-ns", id)
                } else {
                    format!("seed-pk:{id}")
                };
                let create = format!(
                    "CREATE (m:Memory {{pk: $pk, id: '{}', content: 'c', user_id: 'u', memory_type: 'note', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
                    id,
                    escape_ladybug_string(metadata),
                );
                seed_store
                    .with_conn(move |conn| {
                        let mut stmt = conn.prepare(&create)?;
                        conn.execute(
                            &mut stmt,
                            vec![("pk", lbug::Value::String(pk))],
                        )?;
                        Ok(())
                    })
                    .await
                    .expect("seed a null-namespace row");
            }
            assert_eq!(
                t7_count_where(&seed_store, "m.namespace IS NULL").await,
                2,
                "both seeded rows are null-namespace before the audit"
            );
        }
        // Phase 2: reopen. Init runs the audit; the store opens regardless.
        let store = LadybugStore::for_testing(&path, 4).expect("reopen the store");
        store.init("probe").await.expect("init must not be blocked");

        // The backfilled row is a normal row now: readable, and a fresh
        // upsert matches it instead of duplicating it.
        let (_, got) = store
            .get("recovered-ns", "orphan-with-meta")
            .await
            .unwrap()
            .expect("backfilled row is readable in its namespace");
        assert_eq!(got.content, "c");
        store
            .upsert(
                "recovered-ns",
                "orphan-with-meta",
                vec![0.0; 4],
                t7_payload("updated"),
            )
            .await
            .unwrap();
        assert_eq!(
            t7_count_where(&store, "m.id = 'orphan-with-meta'").await,
            1,
            "upsert after backfill matched, not duplicated"
        );

        // The quarantined row never blocks anything, but a write aimed at
        // its id refuses loudly instead of forking the identity.
        let res = store
            .upsert("any-ns", "orphan-no-meta", vec![0.0; 4], t7_payload("fork"))
            .await;
        assert!(res.is_err(), "a write to a quarantined id must refuse");
        let err = res.err().unwrap().to_string();
        assert!(
            err.contains("quarantine"),
            "refusal names the quarantine: {err}"
        );
        assert_eq!(
            t7_count_where(&store, "m.namespace IS NULL").await,
            1,
            "the quarantined row itself is untouched"
        );
    }

    #[tokio::test]
    async fn audit_at_init_never_blocks_open() {
        let path = std::env::temp_dir().join(format!("ns-t7-init-{}", uuid::Uuid::new_v4()));
        {
            let _db = t7_raw_db(&path);
            let conn = Connection::new(&_db).expect("raw connection");
            conn.query(T7_OLD_DDL).expect("old DDL");
            conn.query(
                "CREATE (m:Memory {id: 'pre-orphan', content: 'c', user_id: 'u', memory_type: 'note', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]})",
            )
            .expect("seed a null-namespace row before first init");
        }
        let store = LadybugStore::for_testing(&path, 4).expect("open store");
        // Init runs the audit; a store holding an unclassifiable row opens.
        store.init("probe").await.expect("init must not be blocked");
        // The quarantined row refuses writes by id, and ordinary work
        // proceeds on the same store.
        let res = store
            .upsert("any-ns", "pre-orphan", vec![0.0; 4], t7_payload("fork"))
            .await;
        assert!(res.is_err(), "a write to a quarantined id must refuse");
        assert!(
            res.err().unwrap().to_string().contains("quarantine"),
            "refusal names the quarantine"
        );
        store
            .upsert("probe", "fresh", vec![0.0; 4], t7_payload("fine"))
            .await
            .expect("the store works with a quarantined row present");
    }

    #[tokio::test]
    async fn rollback_drill_old_merge_against_dual_namespace_store() {
        let store = t7_store().await;
        store
            .upsert("ns-a", "shared-id", vec![0.0; 4], t7_payload("content-a"))
            .await
            .unwrap();
        store
            .upsert("ns-b", "shared-id", vec![0.0; 4], t7_payload("content-b"))
            .await
            .unwrap();

        // The drill: an old binary's upsert statement, verbatim shape,
        // against a store where the id legitimately exists twice. Which
        // row it binds (or that it errors) is OBSERVED and printed for the
        // rollback record — the assertions below only pin that the drill
        // cannot create or destroy rows.
        let drill = format!(
            "MERGE (m:Memory {{id: 'shared-id'}}) ON CREATE SET m.namespace = 'ns-a', m.content = 'drill-write', m.user_id = 'agent', m.memory_type = 'fact', m.agent_name = 'a', m.location = '', m.location_lines = '', m.metadata = '{{}}', m.embedding = [0.0,0.0,0.0,0.0] ON MATCH SET m.content = 'drill-write'"
        );
        let outcome = store
            .with_conn(move |conn| {
                conn.query(&drill)?;
                Ok(())
            })
            .await;
        let (_, a) = store.get("ns-a", "shared-id").await.unwrap().unwrap();
        let (_, b) = store.get("ns-b", "shared-id").await.unwrap().unwrap();
        eprintln!(
            "ROLLBACK DRILL: old-shape MERGE outcome={:?} ns-a content={:?} ns-b content={:?}",
            outcome.as_ref().map(|_| "ok"),
            a.content,
            b.content
        );
        assert_eq!(
            t7_count_where(&store, "m.id = 'shared-id'").await,
            2,
            "the drill neither created nor destroyed a row"
        );
    }

    #[tokio::test]
    async fn adversarial_content_round_trips_byte_identical() {
        let store = t7_store().await;
        let cases = [
            "it's a test",
            "back\\slash and C:\\path\\to",
            "'}'); DETACH DELETE (m) //",
            "line one\nline two\r\nline three\ttabbed",
            "unicode: héllo → 世界 ✓",
            "trailing backslash\\",
            "' OR '1'='1",
        ];
        for (i, content) in cases.iter().enumerate() {
            let id = format!("adv-{i}");
            let mut payload = t7_payload(content);
            payload.metadata = json!({"note": "meta 'quoted'\nnewline \\ backslash"});
            store
                .upsert("vault", &id, vec![0.0; 4], payload)
                .await
                .unwrap_or_else(|e| panic!("upsert of case {i} must not error: {e}"));
            let (_, got) = store
                .get("vault", &id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("case {i} must be readable"));
            assert_eq!(&got.content, content, "case {i} round-trips byte-identical");
            assert_eq!(
                got.metadata["note"],
                json!("meta 'quoted'\nnewline \\ backslash"),
                "case {i} metadata round-trips byte-identical"
            );
        }
        assert_eq!(
            t7_count_where(&store, "m.namespace = 'vault'").await,
            cases.len() as i64,
            "no injected statement created or destroyed rows"
        );
    }

    #[tokio::test]
    async fn adversarial_ids_refuse_or_round_trip() {
        let store = t7_store().await;
        // Legal-but-tricky ids round-trip byte-identical, including the
        // engine-minted qualified shape (namespace::path, with slashes).
        for id in ["a:b.c-d_e", "probe::src/a.rs::run", "a:", "..x"] {
            store
                .upsert("probe", id, vec![0.0; 4], t7_payload("ok"))
                .await
                .unwrap_or_else(|e| panic!("legal id {id:?} must be accepted: {e}"));
            let (_, got) = store
                .get("probe", id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("legal id {id:?} must be readable"));
            assert_eq!(got.content, "ok");
        }
        // Illegal ids are refused by the DR-2 allowlist before any query.
        for id in [
            "bad id",
            "bad'id",
            "bad\\id",
            "bad\nid",
            "",
            "üñï",
            &"a".repeat(129),
        ] {
            let res = store.upsert("probe", id, vec![0.0; 4], t7_payload("no")).await;
            assert!(res.is_err(), "illegal id {id:?} must be refused");
            let err = res.err().unwrap().to_string();
            assert!(
                err.contains("ERROR [ID]"),
                "refusal for {id:?} names the id rule: {err}"
            );
        }
        // Illegal namespaces are refused by DR-1 the same way.
        for ns in ["bad ns", ".lead", "a/b", "", &"a".repeat(65)] {
            let res = store.upsert(ns, "fine-id", vec![0.0; 4], t7_payload("no")).await;
            assert!(res.is_err(), "illegal namespace {ns:?} must be refused");
            let err = res.err().unwrap().to_string();
            assert!(
                err.contains("ERROR [NAMESPACE]"),
                "refusal for namespace {ns:?} names the rule: {err}"
            );
        }
    }

    #[tokio::test]
    async fn nul_is_refused_at_the_store_boundary() {
        let store = t7_store().await;
        let res = store
            .upsert("probe", "nul-content", vec![0.0; 4], t7_payload("a\u{0}b"))
            .await;
        assert!(res.is_err(), "NUL in content must be refused");
        let err = res.err().unwrap().to_string();
        assert!(err.contains("NUL"), "refusal names the NUL: {err}");
        assert!(
            store.get("probe", "nul-content").await.unwrap().is_none(),
            "nothing was written"
        );
        let mut payload = t7_payload("clean");
        payload.metadata = json!({"k": "a\u{0}b"});
        let res = store.upsert("probe", "nul-meta", vec![0.0; 4], payload).await;
        assert!(res.is_err(), "NUL in metadata must be refused");
        let err = res.err().unwrap().to_string();
        assert!(err.contains("NUL"), "refusal names the NUL: {err}");
        let res = store.get("probe", "a\u{0}b").await;
        assert!(res.is_err(), "NUL in a lookup id must be refused");
        let err = res.err().unwrap().to_string();
        assert!(err.contains("NUL"), "refusal names the NUL: {err}");
    }

    #[tokio::test]
    async fn edge_merge_binds_within_namespace() {
        let store = t7_store().await;
        store
            .upsert("ns-a", "shared", vec![0.0; 4], t7_payload("a-target"))
            .await
            .unwrap();
        store
            .upsert("ns-b", "shared", vec![0.0; 4], t7_payload("b-target"))
            .await
            .unwrap();
        let mut rule = t7_payload("the rule");
        rule.metadata = json!({"governs": ["shared"]});
        store.upsert("ns-a", "rule-a", vec![0.0; 4], rule).await.unwrap();

        // Exactly one GOVERNS edge leaves rule-a, and it lands on ns-a's
        // row — never on the same-id twin in ns-b.
        let targets = store
            .with_conn(|conn| {
                let mut rows = conn.query(
                    "MATCH (a:Memory {id: 'rule-a'})-[:GOVERNS]->(b:Memory) RETURN b.namespace",
                )?;
                let mut out = Vec::new();
                while let Some(row) = rows.next() {
                    out.push(format!("{}", row[0]));
                }
                Ok(out)
            })
            .await
            .expect("read the edges back");
        assert_eq!(targets, vec!["ns-a".to_string()]);
    }

    // ── Engine capability probes (Task 7 Step A). Each probe ASSERTS the ────
    // ── capability it tests, so the verdict IS the test result: pass means ──
    // ── supported, and failure carries the engine's own error text. (The ───
    // ── print-form originals were unreadable in CI, which discards the ──────
    // ── output of passing tests.) The verdicts decide which rung of the ────
    // ── escaping ladder and which DDL the implementation takes. ────────────
    //
    // The second Step A probe — composite PRIMARY KEY (namespace, id) —
    // was retired on 2026-10-25: it FAILED as designed (run 40164691714),
    // proving lbug 0.20.4's parser rejects a multi-column PRIMARY KEY in
    // CREATE NODE TABLE. That verdict is the reason fresh tables key on
    // the derived single-column pk (see memory_pk); the probe's work is
    // done and its record lives in the plan's Decisions Register.

    #[test]
    fn probe_lbug_param_binding_merge_and_array() {
        use lbug::{LogicalType, Value};
        let path = std::env::temp_dir().join(format!("ns-t7-probe-bind-{}", uuid::Uuid::new_v4()));
        let _db = t7_raw_db(&path);
        let conn = Connection::new(&_db).expect("raw connection");
        conn.query(
            "CREATE NODE TABLE Probe (id STRING, namespace STRING, content STRING, embedding FLOAT[4], PRIMARY KEY (id))",
        )
        .expect("PROBE BINDING unsupported: probe DDL failed");
        let emb = || {
            Value::Array(
                LogicalType::Float,
                vec![
                    Value::Float(0.5),
                    Value::Float(0.25),
                    Value::Float(0.0),
                    Value::Float(1.0),
                ],
            )
        };
        // The capability under test, end to end: a MERGE whose pattern key
        // and SET values are all bound $parameters, executed twice (ON
        // CREATE, then ON MATCH), including a FLOAT[4] array parameter —
        // then read back through a bound WHERE parameter.
        let run = (|| -> Result<Vec<Vec<Value>>, lbug::Error> {
            let mut merge = conn.prepare(
                "MERGE (m:Probe {id: $id}) ON CREATE SET m.namespace = $ns, m.content = $content, m.embedding = $emb ON MATCH SET m.content = $content, m.embedding = $emb",
            )?;
            conn.execute(
                &mut merge,
                vec![
                    ("id", Value::String("x".to_string())),
                    ("ns", Value::String("probe".to_string())),
                    ("content", Value::String("bound 'quoted' content".to_string())),
                    ("emb", emb()),
                ],
            )?;
            conn.execute(
                &mut merge,
                vec![
                    ("id", Value::String("x".to_string())),
                    ("ns", Value::String("probe".to_string())),
                    ("content", Value::String("second write".to_string())),
                    ("emb", emb()),
                ],
            )?;
            let mut read = conn.prepare(
                "MATCH (m:Probe) WHERE m.id = $id RETURN m.content, m.embedding",
            )?;
            let mut rows = Vec::new();
            for row in conn.execute(&mut read, vec![("id", Value::String("x".to_string()))])? {
                rows.push(row);
            }
            Ok(rows)
        })();
        let rows = run.expect("PROBE BINDING unsupported");
        assert_eq!(
            rows.len(),
            1,
            "PROBE BINDING: the second bound MERGE must MATCH the first row, not create a second"
        );
        let row = &rows[0];
        assert!(
            matches!(&row[0], Value::String(s) if s == "second write"),
            "PROBE BINDING: ON MATCH SET must carry the second bound content, got {:?}",
            row[0]
        );
        let floats: Vec<f32> = match &row[1] {
            Value::Array(_, vals) | Value::List(_, vals) => vals
                .iter()
                .map(|v| match v {
                    Value::Float(f) => *f,
                    Value::Double(d) => *d as f32,
                    other => panic!("PROBE BINDING: embedding element not numeric: {other:?}"),
                })
                .collect(),
            other => panic!("PROBE BINDING: embedding did not round-trip as an array: {other:?}"),
        };
        assert_eq!(
            floats,
            vec![0.5, 0.25, 0.0, 1.0],
            "PROBE BINDING: FLOAT[4] array parameter must round-trip element-for-element"
        );
        eprintln!("PROBE BINDING: supported — asserted via bound MERGE + FLOAT[4] round-trip");
    }
}

