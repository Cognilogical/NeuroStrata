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
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

pub const QUIET_PERIOD: Duration = Duration::from_millis(300);
pub const MAX_WINDOW: Duration = Duration::from_secs(2);
pub const BACKOFF_STEPS_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];

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

/// Next backoff delay for a failed trigger attempt, progressing through the
/// backoff ladder and capping at the last step.
pub fn next_backoff(attempt: usize) -> Duration {
    let idx = attempt.min(BACKOFF_STEPS_SECS.len() - 1);
    Duration::from_secs(BACKOFF_STEPS_SECS[idx])
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
                        reg.canonical_path = canonical.clone();
                        tracing::warn!(
                            "watch registry entry J6-canonicalized at load: {} -> {}",
                            reg.canonical_path.display(),
                            canonical.display()
                        );
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
                .get(&(entry_namespace.clone(), entry_path.clone()))
                .expect("runtime present");
            let finished = runtime
                .last_outcome
                .as_ref()
                .map(|outcome| outcome.state == TriggerState::Finished)
                .unwrap_or(false);
            roots.push(RootStatus {
                namespace: entry_namespace.clone(),
                canonical_path: entry_path.clone(),
                dirty: runtime.dirty,
                pending_paths_count: runtime.pending.len(),
                last_event_unix: runtime.last_event_unix,
                last_trigger_unix: runtime.last_trigger_unix,
                last_ingest_state: runtime.last_outcome.as_ref().map(|o| o.state),
                last_ingest_finished_unix: if finished {
                    runtime.last_trigger_unix
                } else {
                    None
                },
                backoff_attempt: runtime.backoff_attempt,
                next_retry_unix: runtime.next_retry_unix,
                watch_active: runtime.watch_active,
            });
        }
        WatchStatusReport { roots }
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

    struct TestEventSource {
        tx: tokio::sync::mpsc::UnboundedSender<WatchEvent>,
        rx: tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
        watched: std::sync::Mutex<Vec<PathBuf>>,
        unwatched: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl TestEventSource {
        fn channel() -> Self {
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
            self.watched
                .lock()
                .map_err(|_| "watch mutex poisoned".to_string())?
                .push(root.to_path_buf());
            Ok(())
        }
        fn unwatch_root(&mut self, root: &Path) -> Result<(), String> {
            self.unwatched
                .lock()
                .map_err(|_| "watch mutex poisoned".to_string())?
                .push(root.to_path_buf());
            Ok(())
        }
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
    async fn start_canonicalizes_persisted_registration_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch_registrations.json");
        let in_root = dir.path().join("src");
        std::fs::create_dir_all(&in_root).unwrap();
        let resolved = in_root.canonicalize().unwrap();
        let raw = format!("{}/./src", dir.path().display());
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
        let loaded = service.store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].canonical_path, resolved);
        assert_eq!(service.store.load().unwrap()[0].canonical_path, resolved);
    }
}
