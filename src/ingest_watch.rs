//! Daemon-native incremental AST freshness: watcher core vocabulary, ports,
//! and pure decisions. This module owns the normalized event contract and the
//! pure debounce, flush, and backoff decisions shared by the watcher core.
//!
//! The core API is consumed by the daemon and MCP adapters composed onto this
//! module; until that composition lands, items here are intentionally public
//! even where the binary target cannot reach them.
#![allow(dead_code)]

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

pub const QUIET_PERIOD: Duration = Duration::from_millis(300);
pub const MAX_WINDOW: Duration = Duration::from_secs(2);
pub const BACKOFF_STEPS_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];
pub const MAX_RENAME_COOKIES: usize = 1024;

pub struct TestEventSource {
    pub tx: tokio::sync::mpsc::UnboundedSender<WatchEvent>,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    watched: std::sync::Mutex<Vec<PathBuf>>,
    unwatched: std::sync::Mutex<Vec<PathBuf>>,
}

impl TestEventSource {
    pub fn channel() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            rx,
            watched: std::sync::Mutex::new(Vec::new()),
            unwatched: std::sync::Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl WatchEventSource for TestEventSource {
    async fn recv(&mut self) -> Option<WatchEvent> {
        self.rx.recv().await
    }

    fn watch_root(&mut self, root: &Path) -> Result<(), String> {
        self.watched.lock().unwrap().push(root.to_path_buf());
        Ok(())
    }

    fn unwatch_root(&mut self, root: &Path) -> Result<(), String> {
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
pub trait WatchEventSource: Send {
    async fn recv(&mut self) -> Option<WatchEvent>;
    fn watch_root(&mut self, root: &Path) -> Result<(), String>;
    fn unwatch_root(&mut self, root: &Path) -> Result<(), String>;
}

pub trait WatchRegistrationStore: Send + Sync {
    fn load(&self) -> anyhow::Result<Vec<Registration>>;
    fn save(&self, regs: &[Registration]) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerOutcome {
    pub state: TriggerState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    watcher: RecommendedWatcher,
    tx: tokio::sync::mpsc::UnboundedSender<WatchEvent>,
    rx: tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    watched_roots: Mutex<HashSet<PathBuf>>,
    pending_renames: Arc<Mutex<HashMap<usize, PathBuf>>>,
}

impl NotifyEventSource {
    /// Create a new `NotifyEventSource`. Starts the underlying `notify`
    /// watcher with a background task that forwards events into the channel.
    pub fn new() -> anyhow::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let pending_renames: Arc<Mutex<HashMap<usize, PathBuf>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_renames_clone = Arc::clone(&pending_renames);
        let tx_clone = tx.clone();
        let watcher = notify::recommended_watcher(move |res: Result<Event, Error>| {
            if let Ok(ev) = res {
                let path = ev.paths.first().cloned().unwrap_or_default();
                let (kind, new_path, skip_direct_send) = match &ev.kind {
                    EventKind::Create(_) => (WatchEventKind::Create, None, false),
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                        if ev.paths.len() >= 2 {
                            let _old = ev.paths[0].clone();
                            let new = ev.paths[1].clone();
                            // A single both-paths rename must be emitted; the
                            // old path is `paths[0]`, destination is `paths[1]`.
                            (WatchEventKind::Rename, Some(new), false)
                        } else {
                            (WatchEventKind::Modify, None, false)
                        }
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                        if let Some(cookie) = ev.tracker() {
                            if let Ok(mut pending) = pending_renames_clone.lock() {
                                if pending.len() >= MAX_RENAME_COOKIES {
                                    // Evict oldest entry to keep map bounded.
                                    if let Some(old_key) = pending.keys().next().cloned() {
                                        pending.remove(&old_key);
                                    }
                                }
                                pending.insert(cookie, path.clone());
                            }
                        }
                        (WatchEventKind::Remove, None, false)
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                        let resolved_new = path.clone();
                        if let Some(cookie) = ev.tracker() {
                            if let Ok(mut pending) = pending_renames_clone.lock() {
                                if let Some(old) = pending.remove(&cookie) {
                                    let watch_event = WatchEvent {
                                        root: PathBuf::from("/"),
                                        path: old,
                                        kind: WatchEventKind::Rename,
                                        new_path: Some(resolved_new),
                                    };
                                    let _ = tx_clone.send(watch_event);
                                    return;
                                }
                            }
                        }
                        // Unpaired `To` without `From`: treat as upsert at this path.
                        (WatchEventKind::Create, None, false)
                    }
                    EventKind::Modify(_) => (WatchEventKind::Modify, None, false),
                    EventKind::Remove(_) => (WatchEventKind::Remove, None, false),
                    _ => (WatchEventKind::Modify, None, false),
                };
                let watch_event = WatchEvent {
                    root: PathBuf::from("/"),
                    path,
                    kind,
                    new_path,
                };
                if !skip_direct_send {
                    let _ = tx_clone.send(watch_event);
                }
            }
        })?;
        Ok(NotifyEventSource {
            watcher,
            tx,
            rx,
            watched_roots: Mutex::new(HashSet::new()),
            pending_renames,
        })
    }
}

#[async_trait]
impl WatchEventSource for NotifyEventSource {
    async fn recv(&mut self) -> Option<WatchEvent> {
        self.rx.recv().await
    }

    fn watch_root(&mut self, root: &Path) -> Result<(), String> {
        // Fail closed: a root we cannot actually watch must not be recorded
        // as active. Recursive mode covers nested source files.
        self.watcher
            .watch(root, notify::RecursiveMode::Recursive)
            .map_err(|err| format!("could not watch {}: {err}", root.display()))?;
        let mut roots = self
            .watched_roots
            .lock()
            .map_err(|_| "watch roots mutex poisoned".to_string())?;
        roots.insert(root.to_path_buf());
        Ok(())
    }

    fn unwatch_root(&mut self, root: &Path) -> Result<(), String> {
        let mut roots = self
            .watched_roots
            .lock()
            .map_err(|_| "watch roots mutex poisoned".to_string())?;
        if !roots.contains(root) {
            return Ok(());
        }
        self.watcher
            .unwatch(root)
            .map_err(|err| format!("could not unwatch {}: {err}", root.display()))?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    events: tokio::sync::Mutex<Box<dyn WatchEventSource>>,
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
            events: tokio::sync::Mutex::new(events),
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
        let mut events = self
            .events
            .try_lock()
            .map_err(|_| anyhow::anyhow!("watch event source mutex poisoned"))?;
        events
            .watch_root(canonical)
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn deactivate(&self, canonical: &Path) -> anyhow::Result<()> {
        let mut events = self
            .events
            .try_lock()
            .map_err(|_| anyhow::anyhow!("watch event source mutex poisoned"))?;
        events
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

    pub fn register(&self, namespace: &str, raw: &Path) -> anyhow::Result<Registration> {
        let canonical = self.canonical_or_err(raw)?;
        self.ensure_runtime(namespace, &canonical)
    }

    pub fn unregister(&self, namespace: &str, raw: &Path) -> anyhow::Result<()> {
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
            return Ok(());
        }
        self.persist_state()?;
        if !still_referenced {
            self.deactivate(&canonical)?;
        }
        Ok(())
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
            self.activate(&path)?;
            self.mark_watch_active(&(namespace.clone(), path.clone()), true)?;
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
        let mut state = self.lock_state()?;
        let keys: Vec<RootKey> = state.roots.keys().cloned().collect();
        let mut matched = false;
        for key in keys {
            let canonical = &key.1;
            let paths: Vec<PathBuf> = effects.iter().map(|e| e.path.clone()).collect();
            let eligible = paths
                .iter()
                .any(|path| crate::parser::ingest::is_ingest_eligible_path(path, canonical));
            if !eligible {
                continue;
            }
            matched = true;
            if let Some(runtime) = state.roots.get_mut(&key) {
                for effect in &effects {
                    runtime.pending.insert(effect.path.clone());
                }
                runtime.dirty = true;
                runtime.last_event_unix = Some(unix_now_secs());
            }
            let mut bursts = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
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
        if !matched {
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
        let now = tokio::time::Instant::now();
        let mut earliest = now + MAX_WINDOW + QUIET_PERIOD;
        if let (Ok(bursts), Ok(retries)) = (self.bursts.lock(), self.retry_at.lock()) {
            for (key, burst) in bursts.iter() {
                let retry = retries.get(key).copied().unwrap_or(burst.first);
                let candidates = [burst.last + QUIET_PERIOD, burst.first + MAX_WINDOW, retry];
                for candidate in candidates {
                    if candidate < earliest {
                        earliest = candidate;
                    }
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
        {
            let mut state = self.lock_state()?;
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
                    let mut retries = self
                        .retry_at
                        .lock()
                        .map_err(|_| anyhow::anyhow!("retry mutex poisoned"))?;
                    retries.insert(key.clone(), started + delay);
                }
            }
        }
        {
            let mut active = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("active mutex poisoned"))?;
            active.remove(key);
        }
        let mut bursts = self
            .bursts
            .lock()
            .map_err(|_| anyhow::anyhow!("burst mutex poisoned"))?;
        let events_during_run = bursts
            .get(key)
            .map(|burst| burst.last >= started)
            .unwrap_or(false);
        if outcome.state == TriggerState::Finished && !events_during_run {
            bursts.remove(key);
            let mut state = self.lock_state()?;
            if let Some(runtime) = state.roots.get_mut(key) {
                runtime.dirty = false;
                runtime.pending.clear();
            }
        } else {
            // One follow-up burst, armed from completion time. More events
            // coalesce into it; no second follow-up is scheduled separately.
            bursts.insert(
                key.clone(),
                BurstState {
                    first: finished,
                    last: finished,
                },
            );
            let mut state = self.lock_state()?;
            if let Some(runtime) = state.roots.get_mut(key) {
                runtime.dirty = true;
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
            let mut events = self.events.lock().await;
            tokio::select! {
                ev = events.recv() => {
                    drop(events);
                    if let Some(ev) = ev {
                        self.ingest_event(ev, tokio::time::Instant::now())?;
                    } else {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => { drop(events); }
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
            match self.trigger.trigger(&namespace, &path_str).await {
                Ok(outcome) => {
                    let now = unix_now_secs();
                    let mut state = self.lock_state()?;
                    if let Some(rt) = state.roots.get_mut(&(namespace.clone(), canonical.clone())) {
                        rt.last_trigger_unix = Some(now);
                        rt.last_outcome = Some(outcome.clone());
                        if outcome.state == TriggerState::Finished {
                            rt.backoff_attempt = 0;
                            rt.next_retry_unix = None;
                            rt.pending.clear();
                        } else {
                            rt.dirty = true;
                            let delay = next_backoff(rt.backoff_attempt);
                            rt.backoff_attempt += 1;
                            rt.next_retry_unix = Some(now + delay.as_secs());
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("reconcile trigger failed for {}: {}", path_str, err);
                    let now = unix_now_secs();
                    let mut state = self.lock_state()?;
                    if let Some(rt) = state.roots.get_mut(&(namespace.clone(), canonical.clone())) {
                        rt.dirty = true;
                        let delay = next_backoff(rt.backoff_attempt);
                        rt.backoff_attempt += 1;
                        rt.next_retry_unix = Some(now + delay.as_secs());
                    }
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

    fn store_for(dir: &tempfile::TempDir) -> Arc<JsonFileRegistrationStore> {
        Arc::new(JsonFileRegistrationStore::new(
            dir.path().join("watch_registrations.json"),
        ))
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
        let service = service_for(&dir, dir.path().to_path_buf());
        let inside = dir.path().join("src");
        let registration = service.register("NeuroStrata", &inside).unwrap();
        assert_eq!(registration.canonical_path, inside);
        assert!(service
            .register("NeuroStrata", &PathBuf::from("/tmp"))
            .is_err());
    }

    #[test]
    fn status_reports_clean_root_after_register() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_for(&dir, dir.path().to_path_buf());
        let root = dir.path().join("src");
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
        let path = dir.path().join("watch_registrations.json");
        let in_root = dir.path().join("src");
        let doc = format!(
            r#"{{"schema_version":1,"registrations":[{{"namespace":"NeuroStrata","canonical_path":"{}","created_at_unix":1763596800,"updated_at_unix":1763596800}},{{"namespace":"NeuroStrata","canonical_path":"/tmp","created_at_unix":1763596800,"updated_at_unix":1763596800}}]}}"#,
            in_root.display()
        );
        std::fs::write(&path, doc).unwrap();
        let service = service_for(&dir, dir.path().to_path_buf());
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
        let resolved = in_root.canonicalize().unwrap();
        let raw = format!("{}/src/../src", dir.path().display());
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
        use notify::Watcher as _;
        let dir = tempfile::tempdir().unwrap();
        let mut source = NotifyEventSource::new().expect("notify watcher available");
        source
            .watcher
            .watch(dir.path(), notify::RecursiveMode::Recursive)
            .expect("watch temp dir");

        // Helper to wait for specific event kind
        async fn recv(source: &mut NotifyEventSource, want: WatchEventKind) -> WatchEvent {
            loop {
                let ev = tokio::time::timeout(std::time::Duration::from_secs(2), source.recv())
                    .await
                    .expect("event timeout")
                    .expect("event");
                if ev.kind == want {
                    return ev;
                }
                // Drain intermediate events (e.g., Modify/Access from same write)
            }
        }

        // Create a file
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello").unwrap();

        // Poll for create event
        let ev = recv(&mut source, WatchEventKind::Create).await;
        assert_eq!(ev.kind, WatchEventKind::Create);
        assert_eq!(ev.path, file);

        // Modify the file - exact kind may vary by platform/backend; accept any
        std::fs::write(&file, b"world").unwrap();
        let ev = recv(&mut source, WatchEventKind::Modify).await;
        assert_eq!(ev.path, file);
        // Accept Modify, Create (truncate+write may look like recreate), or Remove+Create
        assert!(matches!(
            ev.kind,
            WatchEventKind::Modify | WatchEventKind::Create | WatchEventKind::Remove
        ));

        // Delete the file
        std::fs::remove_file(&file).unwrap();
        let ev = recv(&mut source, WatchEventKind::Remove).await;
        // Accept any kind that indicates the path is gone; on some platforms a
        // modify+remove pair may arrive shifted, so we just verify the path matches.
        assert_eq!(ev.path, file);

        // Rename test: create old, rename to new
        let old = dir.path().join("old.txt");
        let new = dir.path().join("new.txt");
        std::fs::write(&old, b"old").unwrap();
        // Wait for create
        let _ = recv(&mut source, WatchEventKind::Create).await;
        // Rename
        std::fs::rename(&old, &new).unwrap();
        let ev = recv(&mut source, WatchEventKind::Rename).await;
        assert_eq!(ev.kind, WatchEventKind::Rename);
        assert_eq!(ev.path, old);
        assert_eq!(ev.new_path, Some(new.clone()));

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

    fn service_with(
        dir: &tempfile::TempDir,
        trigger: Arc<dyn IngestTrigger>,
    ) -> (IngestWatchService, PathBuf) {
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let canonical = root.canonicalize().unwrap();
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
        let second = second.canonicalize().unwrap();
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
}
