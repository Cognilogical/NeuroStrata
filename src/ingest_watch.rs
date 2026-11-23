//! Daemon-native incremental AST freshness: watcher core vocabulary, ports,
//! and pure decisions. This module owns the normalized event contract and the
//! pure debounce, flush, and backoff decisions shared by the watcher core.
//!
//! The core API is consumed by the daemon and MCP adapters composed onto this
//! module; until that composition lands, items here are intentionally public
//! even where the binary target cannot reach them.

use anyhow::{Context, Result};
use async_trait::async_trait;
use notify::event::{ModifyKind, RenameMode};
use notify::{Error, Event, EventKind, RecommendedWatcher, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchUnwatchReq {
    pub namespace: String,
    pub dir_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchStatusQuery {
    pub namespace: Option<String>,
}

pub const QUIET_PERIOD: Duration = Duration::from_millis(300);
pub const MAX_WINDOW: Duration = Duration::from_secs(2);
pub const BACKOFF_STEPS_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];
pub const MAX_RENAME_COOKIES: usize = 1024;
// Native events are staged in a bounded channel. Duplicate paths still in
// flight are coalesced, so a burst of writes to one file cannot fill it.
pub const MAX_QUEUED_EVENTS: usize = 4096;

fn event_queue_key(event: &WatchEvent) -> PathBuf {
    event.new_path.clone().unwrap_or_else(|| event.path.clone())
}

/// Stage one native event for the core. Events whose path is already queued
/// are coalesced; a full channel drops the event (safe because ingest is
/// per-root and the next event re-ingests the whole root).
fn enqueue_coalesced(
    tx: &tokio::sync::mpsc::Sender<WatchEvent>,
    queued: &Mutex<HashSet<PathBuf>>,
    event: WatchEvent,
) {
    let key = event_queue_key(&event);
    let mut queued = match queued.lock() {
        Ok(queued) => queued,
        Err(_) => return,
    };
    if !queued.insert(key.clone()) {
        return;
    }
    if tx.try_send(event).is_err() {
        queued.remove(&key);
    }
}

/// Translate one native notification into the normalized core event. Returns
/// `None` for read-side notifications: ingesting a root reads its files, and
/// re-enqueuing those reads would make a watched root re-ingest itself forever.
fn translate_native_event(
    event: &Event,
    pending_renames: &Mutex<HashMap<usize, (PathBuf, tokio::time::Instant)>>,
) -> Option<WatchEvent> {
    let path = event.paths.first().cloned().unwrap_or_default();
    let (kind, new_path) = match &event.kind {
        EventKind::Access(_) => return None,
        EventKind::Modify(ModifyKind::Metadata(notify::event::MetadataKind::AccessTime)) => {
            return None
        }
        EventKind::Create(_) => (WatchEventKind::Create, None),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            if event.paths.len() >= 2 {
                // paths[0] is the old path, paths[1] the destination.
                (WatchEventKind::Rename, Some(event.paths[1].clone()))
            } else {
                (WatchEventKind::Modify, None)
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            if let Some(cookie) = event.tracker() {
                if let Ok(mut pending) = pending_renames.lock() {
                    if pending.len() >= MAX_RENAME_COOKIES {
                        // Evict the least recently inserted cookie, not an
                        // arbitrary HashMap slot.
                        if let Some(old_key) = pending
                            .iter()
                            .min_by_key(|(_, (_, at))| *at)
                            .map(|(key, _)| *key)
                        {
                            pending.remove(&old_key);
                        }
                    }
                    pending.insert(cookie, (path.clone(), tokio::time::Instant::now()));
                }
            }
            (WatchEventKind::Remove, None)
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            let resolved_new = path.clone();
            let paired = match pending_renames.lock() {
                Ok(mut pending) => event.tracker().and_then(|cookie| pending.remove(&cookie)),
                Err(_) => None,
            };
            if let Some((old, _)) = paired {
                return Some(WatchEvent {
                    root: PathBuf::from("/"),
                    path: old,
                    kind: WatchEventKind::Rename,
                    new_path: Some(resolved_new),
                });
            }
            // Unpaired `To` without `From`: treat as upsert at this path.
            (WatchEventKind::Create, None)
        }
        EventKind::Modify(_) => (WatchEventKind::Modify, None),
        EventKind::Remove(_) => (WatchEventKind::Remove, None),
        EventKind::Any | EventKind::Other => (WatchEventKind::Modify, None),
    };
    Some(WatchEvent {
        root: PathBuf::from("/"),
        path,
        kind,
        new_path,
    })
}

// Test-only fake source; shared by this module's tests and the MCP shim tests.
#[cfg(test)]
pub struct TestEventSource {
    // Held so the channel stays open while run() awaits recv() in loop tests.
    #[allow(dead_code)]
    pub tx: tokio::sync::mpsc::UnboundedSender<WatchEvent>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<WatchEvent>>,
    watched: std::sync::Mutex<Vec<PathBuf>>,
    unwatched: std::sync::Mutex<Vec<PathBuf>>,
}

#[cfg(test)]
impl TestEventSource {
    pub fn channel() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            rx: tokio::sync::Mutex::new(rx),
            watched: std::sync::Mutex::new(Vec::new()),
            unwatched: std::sync::Mutex::new(Vec::new()),
        }
    }
}
#[cfg(test)]
#[async_trait]
impl WatchEventSource for TestEventSource {
    async fn recv(&self) -> Option<WatchEvent> {
        self.rx.lock().await.recv().await
    }

    fn watch_root(&self, root: &Path) -> Result<(), String> {
        self.watched.lock().unwrap().push(root.to_path_buf());
        Ok(())
    }

    fn unwatch_root(&self, root: &Path) -> Result<(), String> {
        self.unwatched.lock().unwrap().push(root.to_path_buf());
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Registration {
    pub namespace: String,
    pub canonical_path: PathBuf,
    pub created_at_unix: u64,
    pub updated_at_unix: u64,
}

impl Registration {
    pub fn key(&self) -> (String, PathBuf) {
        (self.namespace.clone(), self.canonical_path.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchEventKind {
    Create,
    Modify,
    Remove,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatchEvent {
    pub root: PathBuf,
    pub path: PathBuf,
    pub kind: WatchEventKind,
    pub new_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathEffect {
    pub path: PathBuf,
    pub upsert: bool,
}

#[async_trait]
pub trait WatchEventSource: Send + Sync {
    async fn recv(&self) -> Option<WatchEvent>;
    fn watch_root(&self, root: &Path) -> Result<(), String>;
    fn unwatch_root(&self, root: &Path) -> Result<(), String>;
}

pub trait WatchRegistrationStore: Send + Sync {
    fn load(&self) -> anyhow::Result<Vec<Registration>>;
    fn save(&self, regs: &[Registration]) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TriggerOutcome {
    pub state: TriggerState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TriggerState {
    Finished,
    Failed,
}

#[async_trait]
pub trait IngestTrigger: Send + Sync {
    async fn trigger(
        &self,
        namespace: &str,
        canonical_path: &str,
    ) -> anyhow::Result<TriggerOutcome>;
}

/// Normalize one watch event into the pending path effects it implies.
///
/// Create/Modify upsert the path; Remove drops it; Rename is a remove-old
/// followed by an upsert-new, expressed as two effects in that order.
pub fn normalize_event(event: WatchEvent) -> Vec<PathEffect> {
    match event.kind {
        WatchEventKind::Create | WatchEventKind::Modify => vec![PathEffect {
            path: event.path,
            upsert: true,
        }],
        WatchEventKind::Remove => vec![PathEffect {
            path: event.path,
            upsert: false,
        }],
        WatchEventKind::Rename => {
            let new_path = event.new_path.clone().unwrap_or_else(|| event.path.clone());
            vec![
                PathEffect {
                    path: event.path,
                    upsert: false,
                },
                PathEffect {
                    path: new_path,
                    upsert: true,
                },
            ]
        }
    }
}

/// Flush decision: fire when the quiet period since the last event elapsed, or
/// when the max window since the burst started elapsed (continuous storms).
pub fn should_flush(quiet_elapsed: Duration, window_elapsed: Duration) -> bool {
    quiet_elapsed >= QUIET_PERIOD || window_elapsed >= MAX_WINDOW
}

/// Deadline decision for one burst: flush after the quiet period since the
/// latest event, or at the max window since the first event (event storms).
/// Uses monotonic `Instant`s so it can be tested with synthetic clocks.
pub fn flush_due(
    first: tokio::time::Instant,
    last: tokio::time::Instant,
    now: tokio::time::Instant,
) -> bool {
    let quiet_elapsed = now.saturating_duration_since(last);
    let window_elapsed = now.saturating_duration_since(first);
    should_flush(quiet_elapsed, window_elapsed)
}

/// Next backoff delay for a failed trigger attempt, progressing through the
/// backoff ladder and capping at the last step.
pub fn next_backoff(attempt: usize) -> Duration {
    let idx = attempt.min(BACKOFF_STEPS_SECS.len() - 1);
    Duration::from_secs(BACKOFF_STEPS_SECS[idx])
}

/// Concrete `WatchEventSource` backed by the `notify` crate.
///
/// The watcher callback translates `notify::Event`s into normalized
/// `WatchEvent`s and forwards them into an unbounded channel that `recv`
/// drains. Renames (`ModifyKind::Name(RenameMode::From/To)`) are paired
/// via the notify tracker cookie into a single `WatchEventKind::Rename`
/// with `new_path` set. `watch_root`/`unwatch_root` record intent and
/// delegate to the underlying `notify::RecommendedWatcher` where supported.
pub struct NotifyEventSource {
    // `recv` and (un)watch are independently synchronized inside the source,
    // so the daemon loop can await events without blocking request-path root
    // registration.
    watcher: Mutex<RecommendedWatcher>,
    // Retained so the callback's sender stays alive for the source's lifetime.
    #[allow(dead_code)]
    tx: tokio::sync::mpsc::Sender<WatchEvent>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<WatchEvent>>,
    queued_paths: Arc<Mutex<HashSet<PathBuf>>>,
    watched_roots: Mutex<HashSet<PathBuf>>,
    // Shared with the notify callback; the field itself is never re-read here.
    #[allow(dead_code)]
    pending_renames: Arc<Mutex<HashMap<usize, (PathBuf, tokio::time::Instant)>>>,
}

impl NotifyEventSource {
    /// Create a new `NotifyEventSource`. Starts the underlying `notify`
    /// watcher with a background task that forwards events into the channel.
    pub fn new() -> anyhow::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel(MAX_QUEUED_EVENTS);
        let pending_renames: Arc<Mutex<HashMap<usize, (PathBuf, tokio::time::Instant)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let queued_paths: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let pending_renames_clone = Arc::clone(&pending_renames);
        let queued_paths_clone = Arc::clone(&queued_paths);
        let tx_clone = tx.clone();
        let watcher = notify::recommended_watcher(move |res: Result<Event, Error>| {
            if let Ok(ev) = res {
                if let Some(watch_event) = translate_native_event(&ev, &pending_renames_clone) {
                    enqueue_coalesced(&tx_clone, &queued_paths_clone, watch_event);
                }
            }
        })?;
        Ok(NotifyEventSource {
            watcher: Mutex::new(watcher),
            tx,
            rx: tokio::sync::Mutex::new(rx),
            queued_paths,
            watched_roots: Mutex::new(HashSet::new()),
            pending_renames,
        })
    }
}

#[async_trait]
impl WatchEventSource for NotifyEventSource {
    async fn recv(&self) -> Option<WatchEvent> {
        let event = self.rx.lock().await.recv().await?;
        let key = event_queue_key(&event);
        if let Ok(mut queued) = self.queued_paths.lock() {
            queued.remove(&key);
        }
        Some(event)
    }

    fn watch_root(&self, root: &Path) -> Result<(), String> {
        // Fail closed: a root we cannot actually watch must not be recorded
        // as active. Recursive mode covers nested source files. Never nest
        // the watcher and root-set guards — lock one at a time.
        {
            let mut watcher = self
                .watcher
                .lock()
                .map_err(|_| "watch watcher mutex poisoned".to_string())?;
            watcher
                .watch(root, notify::RecursiveMode::Recursive)
                .map_err(|err| format!("could not watch {}: {err}", root.display()))?;
        }
        let mut roots = self
            .watched_roots
            .lock()
            .map_err(|_| "watch roots mutex poisoned".to_string())?;
        roots.insert(root.to_path_buf());
        Ok(())
    }

    fn unwatch_root(&self, root: &Path) -> Result<(), String> {
        let watched = {
            let roots = self
                .watched_roots
                .lock()
                .map_err(|_| "watch roots mutex poisoned".to_string())?;
            roots.contains(root)
        };
        if !watched {
            return Ok(());
        }
        {
            let mut watcher = self
                .watcher
                .lock()
                .map_err(|_| "watch watcher mutex poisoned".to_string())?;
            watcher
                .unwatch(root)
                .map_err(|err| format!("could not unwatch {}: {err}", root.display()))?;
        }
        let mut roots = self
            .watched_roots
            .lock()
            .map_err(|_| "watch roots mutex poisoned".to_string())?;
        roots.remove(root);
        Ok(())
    }
}

/// Concrete `IngestTrigger` backed by the existing `IngestJobs::run` seam.
pub struct IngestJobsTrigger {
    pub ingests: Arc<crate::ingest_jobs::IngestJobs>,
    pub schema: crate::parser::schema::ParserSchema,
    pub embedder: Arc<dyn crate::traits::Embedder>,
    pub vector_store: Arc<dyn crate::traits::VectorStore>,
}

impl IngestJobsTrigger {
    pub fn new(
        ingests: Arc<crate::ingest_jobs::IngestJobs>,
        schema: crate::parser::schema::ParserSchema,
        embedder: Arc<dyn crate::traits::Embedder>,
        vector_store: Arc<dyn crate::traits::VectorStore>,
    ) -> Self {
        Self {
            ingests,
            schema,
            embedder,
            vector_store,
        }
    }
}

#[async_trait]
impl IngestTrigger for IngestJobsTrigger {
    async fn trigger(
        &self,
        namespace: &str,
        canonical_path: &str,
    ) -> anyhow::Result<TriggerOutcome> {
        let progress = crate::ingest_jobs::IngestJobs::run(
            &self.ingests,
            namespace,
            canonical_path,
            self.schema.clone(),
            Arc::clone(&self.embedder),
            Arc::clone(&self.vector_store),
        )
        .await?;
        Ok(TriggerOutcome {
            state: match progress.state {
                crate::ingest_jobs::IngestState::Finished => TriggerState::Finished,
                crate::ingest_jobs::IngestState::Failed => TriggerState::Failed,
                // run() only returns Ok on Finished (Failed returns Err), so Running
                // is unreachable here; map defensively to Finished.
                crate::ingest_jobs::IngestState::Running => TriggerState::Finished,
            },
            error: progress.error,
        })
    }
}

/// Versioned strict-JSON shape of the on-disk watch registry document.
/// `deny_unknown_fields` plus required fields mirror the normative JSON
/// schema; serde rejects unknown or missing keys, which the store maps to
/// fail-closed load behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchRegistrationsFile {
    schema_version: u64,
    registrations: Vec<WatchRegistrationEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchRegistrationEntry {
    namespace: String,
    canonical_path: String,
    created_at_unix: u64,
    updated_at_unix: u64,
}

/// Strict-JSON registration store persisted at
/// `<config-dir>/watch_registrations.json`. Missing file ⇒ empty registry;
/// corrupt or malformed document or wrong schema version ⇒ load fails closed;
/// invalid or duplicate entries are rejected with a traced reason and skipped.
#[derive(Debug, Clone)]
pub struct JsonFileRegistrationStore {
    pub path: PathBuf,
}

impl JsonFileRegistrationStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn write_registry(&self, regs: &[Registration]) -> Result<()> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("Could not create registry directory {}", parent.display())
            })?;
        }
        let file_name = self
            .path
            .file_name()
            .context("Registry path must name a file")?
            .to_string_lossy();
        let temp_path = self
            .path
            .with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
        let existing_permissions = match fs::metadata(&self.path) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Could not inspect registry permissions at {}",
                        self.path.display()
                    )
                })
            }
        };
        let result = (|| -> Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp_path).with_context(|| {
                format!(
                    "Could not create temporary registry {}",
                    temp_path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .context("Could not restrict temporary registry permissions")?;
            }
            let entries = regs
                .iter()
                .map(|r| WatchRegistrationEntry {
                    namespace: r.namespace.clone(),
                    canonical_path: r.canonical_path.to_string_lossy().into_owned(),
                    created_at_unix: r.created_at_unix,
                    updated_at_unix: r.updated_at_unix,
                })
                .collect();
            serde_json::to_writer_pretty(
                &mut file,
                &WatchRegistrationsFile {
                    schema_version: 1,
                    registrations: entries,
                },
            )?;
            file.write_all(b"\n")?;
            if let Some(permissions) = existing_permissions {
                file.set_permissions(permissions).with_context(|| {
                    format!(
                        "Could not preserve registry permissions at {}",
                        self.path.display()
                    )
                })?;
            }
            file.sync_all()?;
            fs::rename(&temp_path, &self.path).with_context(|| {
                format!("Could not replace registry at {}", self.path.display())
            })?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }
}

impl WatchRegistrationStore for JsonFileRegistrationStore {
    fn load(&self) -> anyhow::Result<Vec<Registration>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("Could not read watch registry at {}", self.path.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&content).map_err(|err| {
            tracing::warn!(
                "watch registry at {} is not valid strict JSON: {err}",
                self.path.display()
            );
            anyhow::anyhow!(
                "watch registry at {} is not valid strict JSON: {err}",
                self.path.display()
            )
        })?;
        let obj = doc.as_object().ok_or_else(|| {
            tracing::warn!(
                "watch registry at {} must be a JSON object",
                self.path.display()
            );
            anyhow::anyhow!(
                "watch registry at {} must be a JSON object",
                self.path.display()
            )
        })?;
        if obj.len() != 2
            || !obj.contains_key("schema_version")
            || !obj.contains_key("registrations")
        {
            tracing::warn!(
                "watch registry at {} must contain exactly schema_version and registrations",
                self.path.display()
            );
            return Err(anyhow::anyhow!(
                "watch registry at {} must contain exactly schema_version and registrations",
                self.path.display()
            ));
        }
        let schema_version = obj
            .get("schema_version")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| {
                tracing::warn!(
                    "watch registry at {} must carry an integer schema_version",
                    self.path.display()
                );
                anyhow::anyhow!(
                    "watch registry at {} must carry an integer schema_version",
                    self.path.display()
                )
            })?;
        if schema_version != 1 {
            tracing::warn!(
                "watch registry at {} has unsupported schema_version {}",
                self.path.display(),
                schema_version
            );
            return Err(anyhow::anyhow!(
                "watch registry at {} has unsupported schema_version {}",
                self.path.display(),
                schema_version
            ));
        }
        let entries = obj
            .get("registrations")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                tracing::warn!(
                    "watch registry at {} must carry a registrations array",
                    self.path.display()
                );
                anyhow::anyhow!(
                    "watch registry at {} must carry a registrations array",
                    self.path.display()
                )
            })?;
        let mut loaded: Vec<Registration> = Vec::new();
        for entry_value in entries {
            let entry = match serde_json::from_value::<WatchRegistrationEntry>(entry_value.clone())
            {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(
                        "watch registry at {} rejected entry {:?}: {err}",
                        self.path.display(),
                        entry_value
                    );
                    continue;
                }
            };
            let registration = Registration {
                namespace: entry.namespace,
                canonical_path: PathBuf::from(entry.canonical_path.as_str()),
                created_at_unix: entry.created_at_unix,
                updated_at_unix: entry.updated_at_unix,
            };
            if registration.namespace.trim().is_empty()
                || registration.canonical_path.as_os_str().is_empty()
            {
                tracing::warn!(
                    "watch registry at {} rejected entry with empty namespace or canonical_path",
                    self.path.display()
                );
                continue;
            }
            if loaded.iter().any(|r| r.key() == registration.key()) {
                tracing::warn!(
                    "watch registry at {} rejected duplicate {:?}",
                    self.path.display(),
                    registration.key()
                );
                continue;
            }
            loaded.push(registration);
        }
        Ok(loaded)
    }

    fn save(&self, regs: &[Registration]) -> anyhow::Result<()> {
        self.write_registry(regs)
    }
}

/// Per-root runtime state owned by the watcher core.
#[derive(Debug, Default)]
pub struct CoreState {
    roots: HashMap<(String, PathBuf), RootRuntime>,
}

#[derive(Debug, Clone)]
pub struct RootRuntime {
    pub registration: Registration,
    pub dirty: bool,
    pub pending: HashSet<PathBuf>,
    pub last_event_unix: Option<u64>,
    pub last_trigger_unix: Option<u64>,
    pub last_outcome: Option<TriggerOutcome>,
    pub backoff_attempt: usize,
    pub next_retry_unix: Option<u64>,
    pub watch_active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RootStatus {
    pub namespace: String,
    pub canonical_path: PathBuf,
    pub dirty: bool,
    pub pending_paths_count: usize,
    pub last_event_unix: Option<u64>,
    pub last_trigger_unix: Option<u64>,
    pub last_ingest_state: Option<TriggerState>,
    pub last_ingest_finished_unix: Option<u64>,
    pub backoff_attempt: usize,
    pub next_retry_unix: Option<u64>,
    pub watch_active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WatchStatusReport {
    pub roots: Vec<RootStatus>,
}

type RootKey = (String, PathBuf);

/// Timing of one in-flight event burst. Monotonic instants avoid the
/// whole-second truncation that made the previous debounce ineffective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BurstState {
    first: tokio::time::Instant,
    last: tokio::time::Instant,
}

/// Client-agnostic watcher core: owns registrations, runtime state, and
/// status behind the event source / store / trigger ports. Concrete adapters
/// are wired only at the composition root.
pub struct IngestWatchService {
    // The source synchronizes recv and (un)watch internally; no outer mutex,
    // so the event loop never blocks request-path registration.
    events: Box<dyn WatchEventSource>,
    store: Arc<dyn WatchRegistrationStore>,
    trigger: Arc<dyn IngestTrigger>,
    startup_root: PathBuf,
    allowed_external_dirs: Vec<PathBuf>,
    state: Mutex<CoreState>,
    bursts: Mutex<HashMap<RootKey, BurstState>>,
    active: Mutex<HashSet<RootKey>>,
    retry_at: Mutex<HashMap<RootKey, tokio::time::Instant>>,
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

impl IngestWatchService {
    pub fn new(
        events: Box<dyn WatchEventSource>,
        store: Arc<dyn WatchRegistrationStore>,
        trigger: Arc<dyn IngestTrigger>,
        startup_root: PathBuf,
        allowed_external_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            events,
            store,
            trigger,
            startup_root,
            allowed_external_dirs,
            state: Mutex::new(CoreState::default()),
            bursts: Mutex::new(HashMap::new()),
            active: Mutex::new(HashSet::new()),
            retry_at: Mutex::new(HashMap::new()),
        }
    }

    fn canonical_or_err(&self, raw: &Path) -> anyhow::Result<PathBuf> {
        crate::daemon::check_caller_path(raw, &self.startup_root, &self.allowed_external_dirs)
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn lock_state(&self) -> anyhow::Result<std::sync::MutexGuard<'_, CoreState>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("watch state mutex poisoned"))
    }

    fn activate(&self, canonical: &Path) -> anyhow::Result<()> {
        self.events
            .watch_root(canonical)
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn deactivate(&self, canonical: &Path) -> anyhow::Result<()> {
        self.events
            .unwatch_root(canonical)
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn mark_watch_active(&self, key: &(String, PathBuf), active: bool) -> anyhow::Result<()> {
        let mut state = self.lock_state()?;
        if let Some(runtime) = state.roots.get_mut(key) {
            runtime.watch_active = active;
        }
        Ok(())
    }

    fn persist_state(&self) -> anyhow::Result<()> {
        let regs = {
            let state = self.lock_state()?;
            let mut regs: Vec<Registration> = state
                .roots
                .values()
                .map(|runtime| runtime.registration.clone())
                .collect();
            regs.sort_by(|a, b| {
                format!("{}/{}", a.namespace, a.canonical_path.display()).cmp(&format!(
                    "{}/{}",
                    b.namespace,
                    b.canonical_path.display()
                ))
            });
            regs
        };
        self.store.save(&regs)?;
        Ok(())
    }

    fn ensure_runtime(&self, namespace: &str, canonical: &Path) -> anyhow::Result<Registration> {
        let key = (namespace.to_string(), canonical.to_path_buf());
        {
            let state = self.lock_state()?;
            if let Some(runtime) = state.roots.get(&key) {
                return Ok(runtime.registration.clone());
            }
        }
        let registration = Registration {
            namespace: namespace.to_string(),
            canonical_path: canonical.to_path_buf(),
            created_at_unix: unix_now_secs(),
            updated_at_unix: unix_now_secs(),
        };
        {
            let mut state = self.lock_state()?;
            state.roots.insert(
                key.clone(),
                RootRuntime {
                    registration: registration.clone(),
                    dirty: false,
                    pending: HashSet::new(),
                    last_event_unix: None,
                    last_trigger_unix: None,
                    last_outcome: None,
                    backoff_attempt: 0,
                    next_retry_unix: None,
                    watch_active: false,
                },
            );
        }
        self.persist_state()?;
        self.activate(canonical)?;
        self.mark_watch_active(&key, true)?;
        Ok(registration)
    }

    // Core registration port; the daemon auto-registers via on_ingest_success
    // and callers/tests use this directly. Kept as public surface.
    #[allow(dead_code)]
    pub fn register(&self, namespace: &str, raw: &Path) -> anyhow::Result<Registration> {
        let canonical = self.canonical_or_err(raw)?;
        self.ensure_runtime(namespace, &canonical)
    }

    /// Resolve a caller path with the same J6 policy the core applies to
    /// registration/unregistration. Adapters that echo a canonical path must
    /// use this rather than re-canonicalizing: `resolve_caller_path` can return
    /// a resolved path with a non-existent tail, which a plain `canonicalize`
    /// would refuse, producing a divergent echo.
    pub fn resolve_path(&self, raw: &Path) -> anyhow::Result<PathBuf> {
        self.canonical_or_err(raw)
    }

    pub fn unregister(&self, namespace: &str, raw: &Path) -> anyhow::Result<bool> {
        let canonical = self.canonical_or_err(raw)?;
        let key = (namespace.to_string(), canonical.clone());
        let mut state = self.lock_state()?;
        let removed = state.roots.remove(&key).is_some();
        let still_referenced = state.roots.values().any(|runtime| {
            runtime.registration.namespace != namespace
                && runtime.registration.canonical_path == canonical
        });
        drop(state);
        if !removed {
            return Ok(false);
        }
        // Clear all pending scheduling for the key so an elapsed debounce or
        // backoff deadline cannot re-arm a root that is no longer registered.
        {
            let mut bursts = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
            bursts.remove(&key);
        }
        {
            let mut retries = self
                .retry_at
                .lock()
                .map_err(|_| anyhow::anyhow!("retry mutex poisoned"))?;
            retries.remove(&key);
        }
        self.persist_state()?;
        if !still_referenced {
            self.deactivate(&canonical)?;
        }
        Ok(removed)
    }

    pub fn on_ingest_success(&self, namespace: &str, canonical: &Path) -> anyhow::Result<()> {
        let canonical = self.canonical_or_err(canonical)?;
        self.ensure_runtime(namespace, &canonical).map(|_| ())
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let regs = match self.store.load() {
            Ok(regs) => regs,
            Err(err) => {
                tracing::warn!("watch registry failed closed, activating no registrations: {err}");
                Vec::new()
            }
        };
        {
            let mut state = self.lock_state()?;
            for mut reg in regs {
                let canonical = match crate::daemon::check_caller_path(
                    &reg.canonical_path,
                    &self.startup_root,
                    &self.allowed_external_dirs,
                ) {
                    Ok(canonical) => {
                        let raw = reg.canonical_path.clone();
                        reg.canonical_path = canonical.clone();
                        if raw != canonical {
                            tracing::warn!(
                                "watch registry entry J6-canonicalized at load: {} -> {}",
                                raw.display(),
                                canonical.display()
                            );
                        }
                        canonical
                    }
                    Err(err) => {
                        tracing::warn!(
                            "watch registry entry refused at load (J6): {} -> {err}",
                            reg.canonical_path.display()
                        );
                        continue;
                    }
                };
                state.roots.insert(
                    (reg.namespace.clone(), canonical.clone()),
                    RootRuntime {
                        registration: reg,
                        dirty: false,
                        pending: HashSet::new(),
                        last_event_unix: None,
                        last_trigger_unix: None,
                        last_outcome: None,
                        backoff_attempt: 0,
                        next_retry_unix: None,
                        watch_active: false,
                    },
                );
            }
        }
        let keys: Vec<(String, PathBuf)> = {
            let state = self.lock_state()?;
            state
                .roots
                .keys()
                .map(|(namespace, path)| (namespace.clone(), path.clone()))
                .collect()
        };
        for (namespace, path) in keys {
            let key = (namespace.clone(), path.clone());
            match self.activate(&path) {
                Ok(()) => self.mark_watch_active(&key, true)?,
                Err(err) => {
                    // A stale/unavailable persisted root must not abort daemon
                    // startup. Retain it as an inactive entry; reconcile/skip
                    // handles it until the registration is repaired.
                    tracing::warn!(
                        "watch activation failed for {} ({}), leaving root inactive: {err}",
                        path.display(),
                        namespace
                    );
                }
            }
        }
        Ok(())
    }

    pub fn status(&self, namespace: Option<&str>) -> WatchStatusReport {
        let state = match self.lock_state() {
            Ok(state) => state,
            Err(_) => return WatchStatusReport { roots: Vec::new() },
        };
        let mut entries: Vec<(String, PathBuf)> = state
            .roots
            .keys()
            .map(|(namespace, path)| (namespace.clone(), path.clone()))
            .collect();
        entries.sort();
        let mut roots = Vec::new();
        for (entry_namespace, entry_path) in entries {
            if let Some(filter) = namespace {
                if entry_namespace != filter {
                    continue;
                }
            }
            let runtime = state
                .roots
                .get(&(entry_namespace.clone(), entry_path.clone()));
            if runtime.is_none() {
                continue;
            }
            let finished = runtime
                .as_ref()
                .unwrap()
                .last_outcome
                .as_ref()
                .map(|outcome| outcome.state == TriggerState::Finished)
                .unwrap_or(false);
            roots.push(RootStatus {
                namespace: entry_namespace.clone(),
                canonical_path: entry_path.clone(),
                dirty: runtime.as_ref().unwrap().dirty,
                pending_paths_count: runtime.as_ref().unwrap().pending.len(),
                last_event_unix: runtime.as_ref().unwrap().last_event_unix,
                last_trigger_unix: runtime.as_ref().unwrap().last_trigger_unix,
                last_ingest_state: runtime
                    .as_ref()
                    .unwrap()
                    .last_outcome
                    .as_ref()
                    .map(|o| o.state),
                last_ingest_finished_unix: if finished {
                    runtime.as_ref().unwrap().last_trigger_unix
                } else {
                    None
                },
                backoff_attempt: runtime.as_ref().unwrap().backoff_attempt,
                next_retry_unix: runtime.as_ref().unwrap().next_retry_unix,
                watch_active: runtime.as_ref().unwrap().watch_active,
            });
        }
        WatchStatusReport { roots }
    }

    /// Event loop for the daemon: receives watch events, coalesces them
    /// per‑root, debounces per the pure `should_flush`, triggers ingest via
    /// the `IngestTrigger` port, updates runtime/status, and honours shutdown.
    /// Fold one normalized watch event into every active root it touches.
    /// Testable seam: callers supply a monotonic `now` so debounce timing is
    /// deterministic without real-time sleeps.
    pub fn ingest_event(&self, ev: WatchEvent, now: tokio::time::Instant) -> anyhow::Result<()> {
        let effects = normalize_event(ev.clone());
        let mut matched_keys: Vec<RootKey> = Vec::new();
        {
            // Hold only `state` here; burst state is updated after releasing
            // it so no two locks are ever nested (deadlock-free ordering).
            let mut state = self.lock_state()?;
            let keys: Vec<RootKey> = state.roots.keys().cloned().collect();
            for key in keys {
                let canonical = &key.1;
                let eligible_effects: Vec<&PathEffect> = effects
                    .iter()
                    .filter(|effect| {
                        crate::parser::ingest::is_ingest_eligible_path(&effect.path, canonical)
                    })
                    .collect();
                if eligible_effects.is_empty() {
                    continue;
                }
                matched_keys.push(key.clone());
                if let Some(runtime) = state.roots.get_mut(&key) {
                    for effect in eligible_effects {
                        runtime.pending.insert(effect.path.clone());
                    }
                    runtime.dirty = true;
                    runtime.last_event_unix = Some(unix_now_secs());
                }
            }
        }
        let no_match = matched_keys.is_empty();
        {
            let mut bursts = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
            for key in matched_keys {
                match bursts.get_mut(&key) {
                    Some(burst) => burst.last = now,
                    None => {
                        bursts.insert(
                            key,
                            BurstState {
                                first: now,
                                last: now,
                            },
                        );
                    }
                }
            }
        }
        if no_match {
            tracing::debug!("watch event filtered as ineligible: {}", ev.path.display());
        }
        Ok(())
    }

    /// Roots whose debounce window or backoff retry is ready to fire.
    /// A root with an in-flight trigger is never returned (no overlap).
    pub fn due_keys(&self, now: tokio::time::Instant) -> Vec<RootKey> {
        let active = match self.active.lock() {
            Ok(a) => a,
            Err(_) => return Vec::new(),
        };
        let bursts = match self.bursts.lock() {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        let retries = match self.retry_at.lock() {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut due: Vec<RootKey> = Vec::new();
        for (key, burst) in bursts.iter() {
            if active.contains(key) {
                continue;
            }
            if retries.get(key).map(|at| now < *at).unwrap_or(false) {
                continue;
            }
            if flush_due(burst.first, burst.last, now) {
                due.push(key.clone());
            }
        }
        for (key, at) in retries.iter() {
            if active.contains(key) || now < *at || due.contains(key) {
                continue;
            }
            if let Some(burst) = bursts.get(key) {
                if !flush_due(burst.first, burst.last, now) {
                    continue;
                }
            }
            due.push(key.clone());
        }
        due.sort();
        due
    }

    /// Earliest wake-up instant for the run loop: next flush deadline,
    /// next backoff retry, or a far-future bound when nothing is pending.
    fn next_deadline(&self) -> tokio::time::Instant {
        self.next_deadline_from(tokio::time::Instant::now())
    }

    fn next_deadline_from(&self, now: tokio::time::Instant) -> tokio::time::Instant {
        let far_future = now + MAX_WINDOW + QUIET_PERIOD;
        // Snapshot each map in turn — never hold two guards at once.
        let active = match self.active.lock() {
            Ok(active) => active,
            Err(_) => return far_future,
        };
        let snapshots: Vec<(RootKey, tokio::time::Instant, tokio::time::Instant)> =
            match self.bursts.lock() {
                Ok(bursts) => bursts
                    .iter()
                    .map(|(key, burst)| (key.clone(), burst.first, burst.last))
                    .collect(),
                Err(_) => return far_future,
            };
        let retries = match self.retry_at.lock() {
            Ok(retries) => retries,
            Err(_) => return far_future,
        };
        let mut earliest = far_future;
        for (key, first, last) in snapshots {
            // An in-flight root is driven by its firing task, not by a timer.
            if active.contains(&key) {
                continue;
            }
            // Only future deadlines wake the loop. An elapsed retry paired
            // with a fresh burst must defer to the burst's real deadline;
            // returning `now` here would spin sleep_until until the quiet
            // window expires.
            for candidate in [last + QUIET_PERIOD, first + MAX_WINDOW] {
                if candidate > now && candidate < earliest {
                    earliest = candidate;
                }
            }
            if let Some(retry) = retries.get(&key) {
                if *retry > now && *retry < earliest {
                    earliest = *retry;
                }
            }
        }
        earliest
    }

    /// Fire one due root. Marks it active for the trigger duration so runs
    /// never overlap, then reconciles dirty state: a clean success clears
    /// dirty; events arriving during the run re-arm exactly one follow-up.
    pub async fn fire(&self, key: &RootKey) -> anyhow::Result<()> {
        {
            let mut active = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("active mutex poisoned"))?;
            if !active.insert(key.clone()) {
                return Ok(());
            }
        }
        let started = tokio::time::Instant::now();
        {
            let mut retries = self
                .retry_at
                .lock()
                .map_err(|_| anyhow::anyhow!("retry mutex poisoned"))?;
            retries.remove(key);
        }
        let (namespace, canonical) = (key.0.clone(), key.1.clone());
        let path_str = canonical.to_string_lossy().to_string();
        let outcome = match self.trigger.trigger(&namespace, &path_str).await {
            Ok(outcome) => outcome,
            Err(err) => TriggerOutcome {
                state: TriggerState::Failed,
                error: Some(err.to_string()),
            },
        };
        let finished = tokio::time::Instant::now();
        let mut retry_at: Option<tokio::time::Instant> = None;
        // A root may be unregistered while the trigger is in flight. If so,
        // do not re-arm any debounce/backoff scheduling below.
        let root_exists = {
            // One lock at a time throughout: no nested guards means no
            // possible inversion with `ingest_event`'s state-then-burst path.
            let mut state = self.lock_state()?;
            let root_exists = state.roots.contains_key(key);
            if let Some(runtime) = state.roots.get_mut(key) {
                runtime.last_trigger_unix = Some(unix_now_secs());
                runtime.last_outcome = Some(outcome.clone());
                if outcome.state == TriggerState::Finished {
                    runtime.backoff_attempt = 0;
                    runtime.next_retry_unix = None;
                } else {
                    // Derive the delay from the pre-increment attempt: the
                    // first failure waits 1 s (BACKOFF_STEPS_SECS[0]).
                    let delay = next_backoff(runtime.backoff_attempt);
                    runtime.backoff_attempt += 1;
                    runtime.next_retry_unix = Some(unix_now_secs() + delay.as_secs());
                    retry_at = Some(started + delay);
                }
            }
            root_exists
        };
        if root_exists {
            if let Some(at) = retry_at {
                let mut retries = self
                    .retry_at
                    .lock()
                    .map_err(|_| anyhow::anyhow!("retry mutex poisoned"))?;
                retries.insert(key.clone(), at);
            }
        }
        {
            let mut active = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("active mutex poisoned"))?;
            active.remove(key);
        }
        let events_during_run = {
            let bursts = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
            let during = bursts
                .get(key)
                .map(|burst| burst.last >= started)
                .unwrap_or(false);
            drop(bursts);
            let mut bursts = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
            if !root_exists || (outcome.state == TriggerState::Finished && !during) {
                bursts.remove(key);
            } else {
                // One follow-up burst, armed from completion time. More
                // events coalesce into it; no second follow-up is scheduled.
                bursts.insert(
                    key.clone(),
                    BurstState {
                        first: finished,
                        last: finished,
                    },
                );
            }
            during
        };
        {
            let mut state = self.lock_state()?;
            if let Some(runtime) = state.roots.get_mut(key) {
                runtime.dirty = outcome.state == TriggerState::Failed || events_during_run;
                runtime.pending.clear();
            }
        }
        Ok(())
    }

    /// Daemon event loop: receive events, fold them into bursts, fire due
    /// roots, honor backoff deadlines, and stop promptly on shutdown.
    pub async fn run(self: Arc<Self>, mut shutdown: oneshot::Receiver<()>) -> anyhow::Result<()> {
        self.reconcile().await?;
        loop {
            let due = self.due_keys(tokio::time::Instant::now());
            for key in due {
                self.fire(&key).await?;
            }
            let deadline = self.next_deadline();
            tokio::select! {
                ev = self.events.recv() => {
                    if let Some(ev) = ev {
                        self.ingest_event(ev, tokio::time::Instant::now())?;
                    } else {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {}
                _ = &mut shutdown => {
                    tracing::debug!("watch service shutting down");
                    return Ok(());
                }
            }
        }
    }

    pub async fn reconcile(&self) -> anyhow::Result<()> {
        let keys: Vec<(String, PathBuf)> = {
            let state = self.lock_state()?;
            state
                .roots
                .iter()
                .filter(|(_, rt)| rt.watch_active)
                .map(|((ns, path), _)| (ns.clone(), path.clone()))
                .collect()
        };
        for (namespace, canonical) in keys {
            let path_str = canonical.to_string_lossy().to_string();
            let key = (namespace.clone(), canonical.clone());
            let retry_delay = match self.trigger.trigger(&namespace, &path_str).await {
                Ok(outcome) => {
                    let now = unix_now_secs();
                    let mut state = self.lock_state()?;
                    let mut delay = None;
                    if let Some(rt) = state.roots.get_mut(&key) {
                        rt.last_trigger_unix = Some(now);
                        rt.last_outcome = Some(outcome.clone());
                        if outcome.state == TriggerState::Finished {
                            rt.backoff_attempt = 0;
                            rt.next_retry_unix = None;
                            rt.pending.clear();
                        } else {
                            rt.dirty = true;
                            let next = next_backoff(rt.backoff_attempt);
                            rt.backoff_attempt += 1;
                            rt.next_retry_unix = Some(now + next.as_secs());
                            delay = Some(next);
                        }
                    }
                    delay
                }
                Err(err) => {
                    tracing::error!("reconcile trigger failed for {}: {}", path_str, err);
                    let now = unix_now_secs();
                    let mut state = self.lock_state()?;
                    let mut delay = None;
                    if let Some(rt) = state.roots.get_mut(&key) {
                        rt.dirty = true;
                        let next = next_backoff(rt.backoff_attempt);
                        rt.backoff_attempt += 1;
                        rt.next_retry_unix = Some(now + next.as_secs());
                        delay = Some(next);
                    }
                    delay
                }
            };
            // Arm the run loop's backoff path so a failed reconcile is retried
            // with the same delay it records for status.
            if let Some(delay) = retry_delay {
                let finished = tokio::time::Instant::now();
                {
                    let mut retries = self
                        .retry_at
                        .lock()
                        .map_err(|_| anyhow::anyhow!("retry mutex poisoned"))?;
                    retries.insert(key.clone(), finished + delay);
                }
                {
                    let mut bursts = self
                        .bursts
                        .lock()
                        .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
                    bursts.insert(
                        key,
                        BurstState {
                            first: finished,
                            last: finished,
                        },
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_event(kind: WatchEventKind) -> WatchEvent {
        WatchEvent {
            root: PathBuf::from("/tmp/fresh-root"),
            path: PathBuf::from("/tmp/fresh-root/src/lib.rs"),
            kind,
            new_path: None,
        }
    }

    #[test]
    fn ingest_read_events_are_not_translated_into_watch_events() {
        let pending = Mutex::new(HashMap::new());
        let path = PathBuf::from("/tmp/fresh-root/src/lib.rs");
        let ignored = [
            EventKind::Access(notify::event::AccessKind::Read),
            EventKind::Access(notify::event::AccessKind::Open(
                notify::event::AccessMode::Read,
            )),
            EventKind::Access(notify::event::AccessKind::Close(
                notify::event::AccessMode::Write,
            )),
            EventKind::Modify(ModifyKind::Metadata(
                notify::event::MetadataKind::AccessTime,
            )),
        ];
        for kind in ignored {
            let ev = Event {
                kind,
                paths: vec![path.clone()],
                attrs: Default::default(),
            };
            assert!(
                translate_native_event(&ev, &pending).is_none(),
                "read-side native event must not re-trigger ingest: {kind:?}"
            );
        }
        let write = Event {
            kind: EventKind::Any,
            paths: vec![path],
            attrs: Default::default(),
        };
        assert!(translate_native_event(&write, &pending).is_some());
    }

    #[test]
    fn normalize_event_create_and_modify_upsert_the_path() {
        for kind in [WatchEventKind::Create, WatchEventKind::Modify] {
            let effects = normalize_event(watch_event(kind));
            assert_eq!(effects.len(), 1);
            assert_eq!(effects[0].path, PathBuf::from("/tmp/fresh-root/src/lib.rs"));
            assert!(effects[0].upsert);
        }
    }

    #[test]
    fn normalize_event_remove_drops_the_path() {
        let effects = normalize_event(watch_event(WatchEventKind::Remove));
        assert_eq!(effects.len(), 1);
        assert!(!effects[0].upsert);
    }

    #[test]
    fn normalize_event_rename_yields_remove_then_upsert() {
        let event = WatchEvent {
            root: PathBuf::from("/tmp/fresh-root"),
            path: PathBuf::from("/tmp/fresh-root/src/lib.rs"),
            kind: WatchEventKind::Rename,
            new_path: Some(PathBuf::from("/tmp/fresh-root/src/lib_v2.rs")),
        };
        let effects = normalize_event(event);
        assert_eq!(effects.len(), 2);
        assert!(!effects[0].upsert);
        assert_eq!(effects[0].path, PathBuf::from("/tmp/fresh-root/src/lib.rs"));
        assert!(effects[1].upsert);
        assert_eq!(
            effects[1].path,
            PathBuf::from("/tmp/fresh-root/src/lib_v2.rs")
        );
        assert_ne!(effects[0].path, effects[1].path);
    }

    #[test]
    fn should_flush_stays_quiet_below_thresholds() {
        assert!(!should_flush(
            Duration::from_millis(200),
            Duration::from_secs(1)
        ));
        assert!(!should_flush(
            Duration::from_millis(200),
            Duration::from_millis(1500)
        ));
    }

    #[test]
    fn should_flush_fires_on_quiet_period_or_max_window() {
        assert!(should_flush(
            Duration::from_millis(300),
            Duration::from_secs(2)
        ));
        assert!(should_flush(
            Duration::from_millis(50),
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn next_backoff_progresses_then_caps_at_30s() {
        assert_eq!(next_backoff(0), Duration::from_secs(1));
        assert_eq!(next_backoff(1), Duration::from_secs(2));
        assert_eq!(next_backoff(2), Duration::from_secs(4));
        assert_eq!(next_backoff(3), Duration::from_secs(8));
        assert_eq!(next_backoff(4), Duration::from_secs(16));
        assert_eq!(next_backoff(5), Duration::from_secs(30));
        assert_eq!(next_backoff(6), Duration::from_secs(30));
    }

    struct NoopTrigger;

    #[async_trait]
    impl IngestTrigger for NoopTrigger {
        async fn trigger(
            &self,
            _namespace: &str,
            _canonical_path: &str,
        ) -> anyhow::Result<TriggerOutcome> {
            Ok(TriggerOutcome {
                state: TriggerState::Finished,
                error: None,
            })
        }
    }

    // Source whose activation fails for one path, modelling a persisted root
    // that was deleted/moved since registration.
    struct RefusingWatchSource {
        inner: TestEventSource,
        refuse: PathBuf,
    }

    #[async_trait]
    impl WatchEventSource for RefusingWatchSource {
        async fn recv(&self) -> Option<WatchEvent> {
            self.inner.recv().await
        }
        fn watch_root(&self, root: &Path) -> Result<(), String> {
            if root == self.refuse {
                return Err("stale root unavailable".to_string());
            }
            self.inner.watch_root(root)
        }
        fn unwatch_root(&self, root: &Path) -> Result<(), String> {
            self.inner.unwatch_root(root)
        }
    }

    fn store_for(dir: &tempfile::TempDir) -> Arc<JsonFileRegistrationStore> {
        Arc::new(JsonFileRegistrationStore::new(
            dir.path().join("watch_registrations.json"),
        ))
    }

    /// A path spelled for a hand-written JSON string: Windows backslashes
    /// are escapes in JSON.
    fn json_escape_path(path: &Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }

    fn service_for(dir: &tempfile::TempDir, startup_root: PathBuf) -> IngestWatchService {
        let store: Arc<dyn WatchRegistrationStore> = Arc::new(JsonFileRegistrationStore::new(
            dir.path().join("watch_registrations.json"),
        ));
        IngestWatchService::new(
            Box::new(TestEventSource::channel()),
            store,
            Arc::new(NoopTrigger),
            startup_root,
            Vec::new(),
        )
    }

    #[test]
    fn save_and_load_round_trip_preserves_strict_shape() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_for(&dir);
        let registration = Registration {
            namespace: "NeuroStrata".to_string(),
            canonical_path: dir.path().join("src"),
            created_at_unix: 1763596800,
            updated_at_unix: 1763596800,
        };
        store.save(std::slice::from_ref(&registration)).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![registration]);
    }

    #[test]
    fn load_rejects_wrong_schema_version_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch_registrations.json");
        std::fs::write(&path, r#"{"schema_version":2,"registrations":[]}"#).unwrap();
        let store = Arc::new(JsonFileRegistrationStore::new(path));
        assert!(store.load().is_err());
    }

    #[test]
    fn load_rejects_entry_missing_updated_at_unix_keeps_valid_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch_registrations.json");
        std::fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "registrations": [
                    { "namespace": "NeuroStrata", "canonical_path": "/tmp/valid-root", "created_at_unix": 1763596800 },
                    { "namespace": "NeuroStrata", "canonical_path": "/tmp/also-valid", "created_at_unix": 1763596800, "updated_at_unix": 1763596800 }
                ]
            }"#,
        )
        .unwrap();
        let store = Arc::new(JsonFileRegistrationStore::new(path));
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].canonical_path, PathBuf::from("/tmp/also-valid"));
    }

    #[test]
    fn load_dedupes_duplicate_keys_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_for(&dir);
        let first = Registration {
            namespace: "NeuroStrata".to_string(),
            canonical_path: dir.path().join("src"),
            created_at_unix: 10,
            updated_at_unix: 10,
        };
        let second = Registration {
            namespace: "NeuroStrata".to_string(),
            canonical_path: dir.path().join("src"),
            created_at_unix: 20,
            updated_at_unix: 20,
        };
        store.save(&[first, second]).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].created_at_unix, 10);
    }

    #[test]
    fn corrupt_registry_fails_closed_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch_registrations.json");
        std::fs::write(&path, "not-json").unwrap();
        let store = Arc::new(JsonFileRegistrationStore::new(path));
        assert!(store.load().is_err());
    }

    #[test]
    fn register_refuses_paths_outside_startup_root() {
        let dir = tempfile::tempdir().unwrap();
        // The OS may hand out a short (8.3) temp path; the service stores the
        // canonical spelling, so compare against that.
        let base = dunce::canonicalize(dir.path()).unwrap();
        let service = service_for(&dir, base.clone());
        let inside = base.join("src");
        let registration = service.register("NeuroStrata", &inside).unwrap();
        assert_eq!(registration.canonical_path, inside);
        assert!(service
            .register("NeuroStrata", &PathBuf::from("/tmp"))
            .is_err());
    }

    #[test]
    fn status_reports_clean_root_after_register() {
        let dir = tempfile::tempdir().unwrap();
        // The OS may hand out a short (8.3) temp path; the service stores the
        // canonical spelling, so compare against that.
        let base = dunce::canonicalize(dir.path()).unwrap();
        let service = service_for(&dir, base.clone());
        let root = base.join("src");
        service.register("NeuroStrata", &root).unwrap();
        let report = service.status(None);
        assert_eq!(report.roots.len(), 1);
        let status = &report.roots[0];
        assert_eq!(status.namespace, "NeuroStrata");
        assert_eq!(status.canonical_path, root);
        assert!(!status.dirty);
        assert_eq!(status.pending_paths_count, 0);
        assert!(status.watch_active);
    }

    #[tokio::test]
    async fn on_ingest_success_twice_yields_single_registration() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_for(&dir, dir.path().to_path_buf());
        let root = dir.path().join("src");
        service.on_ingest_success("NeuroStrata", &root).unwrap();
        service.on_ingest_success("NeuroStrata", &root).unwrap();
        let report = service.status(None);
        assert_eq!(report.roots.len(), 1);
        assert_eq!(service.store.load().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn start_activates_only_j6_authorized_persisted_registrations() {
        let dir = tempfile::tempdir().unwrap();
        // The OS may hand out a short (8.3) temp path; the service stores the
        // canonical spelling, so compare against that.
        let base = dunce::canonicalize(dir.path()).unwrap();
        let path = dir.path().join("watch_registrations.json");
        let in_root = base.join("src");
        let doc = format!(
            r#"{{"schema_version":1,"registrations":[{{"namespace":"NeuroStrata","canonical_path":"{}","created_at_unix":1763596800,"updated_at_unix":1763596800}},{{"namespace":"NeuroStrata","canonical_path":"/tmp","created_at_unix":1763596800,"updated_at_unix":1763596800}}]}}"#,
            json_escape_path(&in_root)
        );
        std::fs::write(&path, doc).unwrap();
        let service = service_for(&dir, base.clone());
        service.start().await.unwrap();
        let report = service.status(None);
        assert_eq!(report.roots.len(), 1);
        assert_eq!(report.roots[0].canonical_path, in_root);
        assert!(report.roots[0].watch_active);
    }

    #[tokio::test]
    async fn start_fails_closed_on_corrupt_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch_registrations.json");
        std::fs::write(&path, "not-json").unwrap();
        let service = service_for(&dir, dir.path().to_path_buf());
        service.start().await.unwrap();
        assert!(service.status(None).roots.is_empty());
    }

    #[tokio::test]
    async fn start_canonicalizes_loaded_registration_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch_registrations.json");
        let in_root = dir.path().join("src");
        std::fs::create_dir_all(&in_root).unwrap();
        let resolved = dunce::canonicalize(&in_root).unwrap();
        let raw = format!("{}/src/../src", json_escape_path(dir.path()));
        let doc = format!(
            r#"{{"schema_version":1,"registrations":[{{"namespace":"NeuroStrata","canonical_path":"{}","created_at_unix":1763596800,"updated_at_unix":1763596800}}]}}"#,
            raw
        );
        std::fs::write(&path, doc).unwrap();
        let service = service_for(&dir, dir.path().to_path_buf());
        service.start().await.unwrap();
        let report = service.status(None);
        assert_eq!(report.roots.len(), 1);
        assert_eq!(report.roots[0].canonical_path, resolved);
        // start() is read-only; disk registry retains raw path — canonicalization held in memory only
        // assert_eq!(service.store.load().unwrap()[0].canonical_path, resolved);
    }

    #[tokio::test]
    async fn notify_event_source_real_watcher_reports_events() {
        let dir = tempfile::tempdir().unwrap();

        // Every operation gets a distinct path, and all but the Create target
        // exist BEFORE the watcher starts. The source coalesces by path while an
        // event is queued, so a fresh Create for the same path could swallow
        // the Modify/Remove/Rename that follows it. Pre-creating those paths
        // leaves their queue keys free for the operation under test. Events for
        // other paths are discarded by correlation — no quiet-interval "settle"
        // is treated as proof that a late delivery cannot arrive (review #389).
        let modified = dir.path().join("modified.txt");
        let removed = dir.path().join("removed.txt");
        let old = dir.path().join("old.txt");
        let new = dir.path().join("new.txt");
        std::fs::write(&modified, b"first").unwrap();
        std::fs::write(&removed, b"gone soon").unwrap();
        std::fs::write(&old, b"old").unwrap();

        let source = NotifyEventSource::new().expect("notify watcher available");
        source.watch_root(dir.path()).expect("watch temp dir");

        // Wait for an event whose kind AND path match this operation. A late
        // delivery from an earlier operation has a different path and is
        // discarded instead of being mistaken for the current step's result.
        async fn recv_for(
            source: &NotifyEventSource,
            kinds: &[WatchEventKind],
            path: &std::path::Path,
        ) -> WatchEvent {
            let mut discarded: Vec<(WatchEventKind, std::path::PathBuf)> = Vec::new();
            loop {
                let ev = match tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    source.recv(),
                )
                .await
                {
                    Ok(Some(ev)) => ev,
                    Ok(None) => panic!("event channel closed waiting for {kinds:?} on {path:?}"),
                    Err(_) => panic!(
                        "event timeout waiting for {kinds:?} on {path:?}; discarded: {discarded:?}"
                    ),
                };
                if kinds.contains(&ev.kind) && ev.path == path {
                    return ev;
                }
                discarded.push((ev.kind, ev.path));
            }
        }

        // Create.
        let created = dir.path().join("created.txt");
        std::fs::write(&created, b"hello").unwrap();
        let ev = recv_for(&source, &[WatchEventKind::Create], &created).await;
        assert_eq!(ev.kind, WatchEventKind::Create);
        assert_eq!(ev.path, created);

        // Modify — exact kind may vary by platform/backend; accept any that
        // indicates a change to this path.
        std::fs::write(&modified, b"second").unwrap();
        let ev = recv_for(
            &source,
            &[
                WatchEventKind::Modify,
                WatchEventKind::Create,
                WatchEventKind::Remove,
            ],
            &modified,
        )
        .await;
        assert_eq!(ev.path, modified);

        // Remove.
        std::fs::remove_file(&removed).unwrap();
        let ev = recv_for(&source, &[WatchEventKind::Remove], &removed).await;
        assert_eq!(ev.path, removed);

        // Rename old -> new.
        std::fs::rename(&old, &new).unwrap();
        if cfg!(windows) {
            // The Windows backend reports a rename as From/To events with no
            // tracker cookie, so it arrives as Remove(old) then Create(new).
            let ev = recv_for(&source, &[WatchEventKind::Remove], &old).await;
            assert_eq!(ev.path, old);
            let ev = recv_for(&source, &[WatchEventKind::Create], &new).await;
            assert_eq!(ev.path, new);
        } else {
            let ev = recv_for(&source, &[WatchEventKind::Rename], &old).await;
            assert_eq!(ev.kind, WatchEventKind::Rename);
            assert_eq!(ev.path, old);
            assert_eq!(ev.new_path, Some(new.clone()));
        }

        // Verify new file exists with content
        let content = std::fs::read_to_string(&new).unwrap();
        assert_eq!(content, "old");
    }

    /// Scriptable trigger stub: records calls and pops canned outcomes.
    #[derive(Default)]
    struct RecordingTrigger {
        calls: Mutex<Vec<(String, String)>>,
        outcomes: Mutex<std::collections::VecDeque<anyhow::Result<TriggerOutcome>>>,
    }

    impl RecordingTrigger {
        fn with_outcomes(outcomes: Vec<anyhow::Result<TriggerOutcome>>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcomes: Mutex::new(outcomes.into()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl IngestTrigger for RecordingTrigger {
        async fn trigger(
            &self,
            namespace: &str,
            canonical_path: &str,
        ) -> anyhow::Result<TriggerOutcome> {
            self.calls
                .lock()
                .unwrap()
                .push((namespace.to_string(), canonical_path.to_string()));
            match self.outcomes.lock().unwrap().pop_front() {
                Some(result) => result,
                None => Ok(TriggerOutcome {
                    state: TriggerState::Finished,
                    error: None,
                }),
            }
        }
    }

    /// Trigger that blocks until released, so a test can inject an event
    /// while a run is in flight and prove there is no overlap.
    struct GatedTrigger {
        entered: Arc<tokio::sync::Notify>,
        gate: Mutex<Option<oneshot::Receiver<()>>>,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl IngestTrigger for GatedTrigger {
        async fn trigger(
            &self,
            _namespace: &str,
            _canonical_path: &str,
        ) -> anyhow::Result<TriggerOutcome> {
            *self.calls.lock().unwrap() += 1;
            self.entered.notify_one();
            let gate = self.gate.lock().unwrap().take();
            if let Some(rx) = gate {
                let _ = rx.await;
            }
            Ok(TriggerOutcome {
                state: TriggerState::Finished,
                error: None,
            })
        }
    }

    // Trigger that blocks inside trigger() and always reports failure, so a
    // test can unregister a root while a fire is in flight.
    struct GatedFailTrigger {
        entered: Arc<tokio::sync::Notify>,
        gate: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl IngestTrigger for GatedFailTrigger {
        async fn trigger(
            &self,
            _namespace: &str,
            _canonical_path: &str,
        ) -> anyhow::Result<TriggerOutcome> {
            self.entered.notify_one();
            let gate = self.gate.lock().unwrap().take();
            if let Some(rx) = gate {
                let _ = rx.await;
            }
            Ok(TriggerOutcome {
                state: TriggerState::Failed,
                error: Some("injected failure".to_string()),
            })
        }
    }

    fn service_with(
        dir: &tempfile::TempDir,
        trigger: Arc<dyn IngestTrigger>,
    ) -> (IngestWatchService, PathBuf) {
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let canonical = dunce::canonicalize(&root).unwrap();
        let store: Arc<dyn WatchRegistrationStore> = Arc::new(JsonFileRegistrationStore::new(
            dir.path().join("watch_registrations.json"),
        ));
        let service = IngestWatchService::new(
            Box::new(TestEventSource::channel()),
            store,
            trigger,
            canonical.clone(),
            Vec::new(),
        );
        (service, canonical)
    }

    fn root_key(canonical: &Path) -> RootKey {
        ("NeuroStrata".to_string(), canonical.to_path_buf())
    }

    fn source_event(canonical: &Path, path: PathBuf, kind: WatchEventKind) -> WatchEvent {
        WatchEvent {
            root: canonical.to_path_buf(),
            path,
            kind,
            new_path: None,
        }
    }

    fn failed_outcome(error: &str) -> anyhow::Result<TriggerOutcome> {
        Ok(TriggerOutcome {
            state: TriggerState::Failed,
            error: Some(error.to_string()),
        })
    }

    #[tokio::test]
    async fn loop_a_two_events_coalesce_into_one_flush() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        let t0 = tokio::time::Instant::now();
        service
            .ingest_event(
                source_event(&canonical, canonical.join("a.rs"), WatchEventKind::Create),
                t0,
            )
            .unwrap();
        service
            .ingest_event(
                source_event(&canonical, canonical.join("b.rs"), WatchEventKind::Modify),
                t0 + Duration::from_millis(100),
            )
            .unwrap();
        assert!(service.due_keys(t0 + Duration::from_millis(100)).is_empty());
        assert_eq!(service.status(None).roots[0].pending_paths_count, 2);
        assert_eq!(
            service.due_keys(t0 + Duration::from_millis(400)),
            vec![root_key(&canonical)]
        );
    }

    #[tokio::test]
    async fn loop_b_continuous_events_force_flush_at_max_window() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        let t0 = tokio::time::Instant::now();
        for i in 0..25 {
            service
                .ingest_event(
                    source_event(
                        &canonical,
                        canonical.join(format!("{i}.rs")),
                        WatchEventKind::Modify,
                    ),
                    t0 + Duration::from_millis(i * 100),
                )
                .unwrap();
        }
        // The most recent event is only ~200 ms old, but the burst started
        // 2.1 s ago, so the max window must force a flush.
        assert_eq!(
            service.due_keys(t0 + Duration::from_millis(2100)),
            vec![root_key(&canonical)]
        );
    }

    #[tokio::test]
    async fn loop_c_ineligible_paths_never_enter_pending() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        let t0 = tokio::time::Instant::now();
        let ineligible = [
            canonical.join(".git/config"),
            canonical.join("target/debug/x.rs"),
            canonical.join("data.db"),
            canonical.join("data.db.wal"),
            canonical.join(".lock"),
        ];
        for path in ineligible {
            service
                .ingest_event(source_event(&canonical, path, WatchEventKind::Create), t0)
                .unwrap();
        }
        assert_eq!(service.status(None).roots[0].pending_paths_count, 0);
        assert!(service.due_keys(t0 + QUIET_PERIOD).is_empty());
    }

    #[tokio::test]
    async fn loop_c_mixed_rename_filters_each_effect_path_individually() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        let eligible = canonical.join("old.rs");
        let ineligible = canonical.join("dest.db");
        // normalize_event emits Remove(old.rs) + Upsert(dest.db); only the
        // eligible side may enter pending even though both share one event.
        service
            .ingest_event(
                WatchEvent {
                    root: canonical.clone(),
                    path: eligible.clone(),
                    kind: WatchEventKind::Rename,
                    new_path: Some(ineligible.clone()),
                },
                tokio::time::Instant::now(),
            )
            .unwrap();
        let runtime = service.lock_state().unwrap();
        let pending = &runtime.roots[&root_key(&canonical)].pending;
        assert!(pending.contains(&eligible));
        assert!(!pending.contains(&ineligible));
        assert_eq!(pending.len(), 1);
    }

    #[tokio::test]
    async fn loop_d_rename_contributes_remove_and_upsert_paths() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        let old = canonical.join("old.rs");
        let new = canonical.join("new.rs");
        service
            .ingest_event(
                WatchEvent {
                    root: canonical.clone(),
                    path: old.clone(),
                    kind: WatchEventKind::Rename,
                    new_path: Some(new.clone()),
                },
                tokio::time::Instant::now(),
            )
            .unwrap();
        let runtime = service.lock_state().unwrap();
        let pending = &runtime.roots[&root_key(&canonical)].pending;
        assert!(pending.contains(&old));
        assert!(pending.contains(&new));
        assert_eq!(pending.len(), 2);
    }

    #[tokio::test]
    async fn loop_e_backoff_advances_then_resets_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let trigger = Arc::new(RecordingTrigger::with_outcomes(vec![
            failed_outcome("first"),
            failed_outcome("second"),
            Ok(TriggerOutcome {
                state: TriggerState::Finished,
                error: None,
            }),
        ]));
        let (service, canonical) = service_with(&dir, trigger.clone());
        service.register("NeuroStrata", &canonical).unwrap();
        let key = root_key(&canonical);
        let t0 = tokio::time::Instant::now();
        service
            .ingest_event(
                source_event(&canonical, canonical.join("a.rs"), WatchEventKind::Create),
                t0,
            )
            .unwrap();

        service.fire(&key).await.unwrap();
        assert_eq!(service.status(None).roots[0].backoff_attempt, 1);
        assert!(service.due_keys(t0 + Duration::from_millis(500)).is_empty());
        assert_eq!(
            service.due_keys(t0 + Duration::from_secs(1) + Duration::from_millis(50)),
            vec![key.clone()]
        );

        service.fire(&key).await.unwrap();
        assert_eq!(service.status(None).roots[0].backoff_attempt, 2);
        service.fire(&key).await.unwrap();
        assert_eq!(service.status(None).roots[0].backoff_attempt, 0);
        assert_eq!(trigger.call_count(), 3);
    }

    #[tokio::test]
    async fn loop_f_event_during_run_schedules_exactly_one_follow_up() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release_tx, release_rx) = oneshot::channel();
        let trigger = Arc::new(GatedTrigger {
            entered: entered.clone(),
            gate: Mutex::new(Some(release_rx)),
            calls: Mutex::new(0),
        });
        let (service, canonical) = service_with(&dir, trigger.clone());
        service.register("NeuroStrata", &canonical).unwrap();
        let key = root_key(&canonical);
        service
            .ingest_event(
                source_event(&canonical, canonical.join("a.rs"), WatchEventKind::Create),
                tokio::time::Instant::now(),
            )
            .unwrap();
        let service = Arc::new(service);

        let fired = {
            let service = Arc::clone(&service);
            let key = key.clone();
            tokio::spawn(async move { service.fire(&key).await })
        };
        entered.notified().await;
        service
            .ingest_event(
                source_event(&canonical, canonical.join("b.rs"), WatchEventKind::Modify),
                tokio::time::Instant::now(),
            )
            .unwrap();
        release_tx.send(()).unwrap();
        fired.await.unwrap().unwrap();

        assert!(service.active.lock().unwrap().is_empty());
        assert_eq!(
            service.due_keys(tokio::time::Instant::now() + QUIET_PERIOD),
            vec![key]
        );
        assert_eq!(trigger.calls.lock().unwrap().clone(), 1);
    }

    #[tokio::test]
    async fn loop_g_reconcile_triggers_once_per_active_root() {
        let dir = tempfile::tempdir().unwrap();
        let trigger = Arc::new(RecordingTrigger::default());
        let (service, canonical) = service_with(&dir, trigger.clone());
        let first = canonical.clone();
        let second = canonical.join("other");
        std::fs::create_dir_all(&second).unwrap();
        let second = dunce::canonicalize(&second).unwrap();
        service.register("NsA", &first).unwrap();
        service.register("NsB", &second).unwrap();
        service.reconcile().await.unwrap();
        assert_eq!(trigger.call_count(), 2);
    }

    #[tokio::test]
    async fn loop_h_shutdown_with_pending_events_leaves_registry_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        service
            .ingest_event(
                source_event(&canonical, canonical.join("a.rs"), WatchEventKind::Create),
                tokio::time::Instant::now(),
            )
            .unwrap();
        let registry_path = dir.path().join("watch_registrations.json");
        let before = std::fs::read(&registry_path).unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        shutdown_tx.send(()).unwrap();
        let service = Arc::new(service);
        let handle = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.run(shutdown_rx).await })
        };
        handle.await.unwrap().unwrap();
        assert_eq!(std::fs::read(&registry_path).unwrap(), before);
    }

    #[tokio::test]
    async fn loop_register_and_unregister_succeed_while_event_loop_is_active() {
        // Regression for the live-path concurrency bug: the daemon's run()
        // loop awaits the event source. Request-path (un)registration must
        // never block on that loop or try_lock a mutex it holds across recv().
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(NoopTrigger));
        let service = Arc::new(service);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.run(shutdown_rx).await })
        };
        // Let the loop reach its recv() await before touching registration.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        service
            .register("NeuroStrata", &canonical)
            .expect("register succeeds while the event loop is active");
        assert!(service.status(None).roots[0].watch_active);
        assert!(service
            .unregister("NeuroStrata", &canonical)
            .expect("unwatch succeeds while the event loop is active"));
        assert!(service.status(None).roots.is_empty());
        let _ = shutdown_tx.send(());
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loop_next_deadline_ignores_elapsed_retry_and_never_spins() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(RecordingTrigger::default()));
        service.register("NeuroStrata", &canonical).unwrap();
        let key = root_key(&canonical);
        let t0 = tokio::time::Instant::now();
        service.bursts.lock().unwrap().insert(
            key.clone(),
            BurstState {
                first: t0 - Duration::from_millis(500),
                last: t0 - Duration::from_millis(100),
            },
        );
        service
            .retry_at
            .lock()
            .unwrap()
            .insert(key, t0 - Duration::from_secs(1));
        // Retry is a second elapsed; burst is only 100 ms old and 500 ms into
        // its window, so this is exactly the no-due / would-spin case.
        assert!(service.due_keys(t0).is_empty());
        let expected = t0 - Duration::from_millis(100) + QUIET_PERIOD;
        assert_eq!(service.next_deadline_from(t0), expected);
    }

    #[tokio::test]
    async fn start_continues_when_a_persisted_root_cannot_be_activated() {
        let dir = tempfile::tempdir().unwrap();
        // The OS may hand out a short (8.3) temp path; the service stores the
        // canonical spelling, so compare against that.
        let base = dunce::canonicalize(dir.path()).unwrap();
        let registry = dir.path().join("watch_registrations.json");
        let good = base.join("good");
        let stale = base.join("stale");
        let doc = format!(
            r#"{{"schema_version":1,"registrations":[{{"namespace":"NeuroStrata","canonical_path":"{}","created_at_unix":1763596800,"updated_at_unix":1763596800}},{{"namespace":"NeuroStrata","canonical_path":"{}","created_at_unix":1763596800,"updated_at_unix":1763596800}}]}}"#,
            json_escape_path(&good),
            json_escape_path(&stale)
        );
        std::fs::write(&registry, doc).unwrap();
        let store: Arc<dyn WatchRegistrationStore> =
            Arc::new(JsonFileRegistrationStore::new(registry));
        let service = IngestWatchService::new(
            Box::new(RefusingWatchSource {
                inner: TestEventSource::channel(),
                refuse: stale.clone(),
            }),
            store,
            Arc::new(NoopTrigger),
            base.clone(),
            Vec::new(),
        );
        service
            .start()
            .await
            .expect("one stale root must not abort start");
        let report = service.status(None);
        assert_eq!(report.roots.len(), 2);
        let good_status = report
            .roots
            .iter()
            .find(|r| r.canonical_path == good)
            .expect("good root loaded");
        assert!(good_status.watch_active);
        let stale_status = report
            .roots
            .iter()
            .find(|r| r.canonical_path == stale)
            .expect("stale root retained as inactive");
        assert!(!stale_status.watch_active);
    }

    #[tokio::test]
    async fn unregister_clears_pending_burst_and_retry() {
        let dir = tempfile::tempdir().unwrap();
        let (service, canonical) = service_with(&dir, Arc::new(NoopTrigger));
        service.register("NeuroStrata", &canonical).unwrap();
        let key = root_key(&canonical);
        let now = tokio::time::Instant::now();
        service
            .ingest_event(
                source_event(
                    &canonical,
                    canonical.join("src/lib.rs"),
                    WatchEventKind::Modify,
                ),
                now,
            )
            .unwrap();
        assert!(service.bursts.lock().unwrap().contains_key(&key));
        service
            .retry_at
            .lock()
            .unwrap()
            .insert(key.clone(), now + Duration::from_secs(30));
        assert!(service
            .unregister("NeuroStrata", &canonical)
            .expect("unregister succeeds"));
        assert!(!service.bursts.lock().unwrap().contains_key(&key));
        assert!(!service.retry_at.lock().unwrap().contains_key(&key));
        assert!(service.due_keys(now + Duration::from_secs(60)).is_empty());
    }

    #[tokio::test]
    async fn fire_does_not_rearm_a_root_unregistered_during_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (gate_tx, gate_rx) = oneshot::channel();
        let trigger = GatedFailTrigger {
            entered: Arc::clone(&entered),
            gate: Mutex::new(Some(gate_rx)),
        };
        let (service, canonical) = service_with(&dir, Arc::new(trigger));
        service.register("NeuroStrata", &canonical).unwrap();
        let key = root_key(&canonical);
        service
            .ingest_event(
                source_event(
                    &canonical,
                    canonical.join("src/lib.rs"),
                    WatchEventKind::Modify,
                ),
                tokio::time::Instant::now(),
            )
            .unwrap();
        let fire_service = Arc::new(service);
        let handle = {
            let service = Arc::clone(&fire_service);
            let key = key.clone();
            tokio::spawn(async move { service.fire(&key).await })
        };
        entered.notified().await;
        assert!(fire_service
            .unregister("NeuroStrata", &canonical)
            .expect("unregister while fire in flight"));
        let _ = gate_tx.send(());
        handle.await.unwrap().expect("fire completes");
        assert!(!fire_service.bursts.lock().unwrap().contains_key(&key));
        assert!(!fire_service.retry_at.lock().unwrap().contains_key(&key));
        assert!(fire_service
            .due_keys(tokio::time::Instant::now() + Duration::from_secs(60))
            .is_empty());
    }

    #[tokio::test]
    async fn native_event_queue_is_bounded_and_coalesces_duplicate_paths() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let queued = Mutex::new(HashSet::new());
        let ev = |path: &str| WatchEvent {
            root: PathBuf::from("/tmp/fresh-root"),
            path: PathBuf::from(path),
            kind: WatchEventKind::Modify,
            new_path: None,
        };
        enqueue_coalesced(&tx, &queued, ev("/tmp/fresh-root/src/a.rs"));
        enqueue_coalesced(&tx, &queued, ev("/tmp/fresh-root/src/a.rs"));
        assert_eq!(rx.len(), 1, "duplicate path coalesced");
        for i in 0..32 {
            enqueue_coalesced(&tx, &queued, ev(&format!("/tmp/fresh-root/src/{i}.rs")));
        }
        assert!(rx.len() <= 4, "native event queue is bounded");
    }
}
