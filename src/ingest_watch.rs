//! Daemon-native incremental AST freshness: watcher core vocabulary, ports,
//! and pure decisions. Task 1 lands the contract only; the event loop,
//! persistence store, and service methods follow in later tasks.

use std::path::{Path, PathBuf};
use std::time::Duration;
use async_trait::async_trait;
use anyhow::Result;

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
        WatchEventKind::Rename => vec![
            PathEffect {
                path: event.path.clone(),
                upsert: false,
            },
            PathEffect {
                path: event.path,
                upsert: true,
            },
        ],
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

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_event(kind: WatchEventKind) -> WatchEvent {
        WatchEvent {
            root: PathBuf::from("/tmp/fresh-root"),
            path: PathBuf::from("/tmp/fresh-root/src/lib.rs"),
            kind,
        }
    }

    #[test]
    fn normalize_event_create_and_modify_upsert_the_path() {
        for kind in [WatchEventKind::Create, WatchEventKind::Modify] {
            let effects = normalize_event(watch_event(kind));
            assert_eq!(effects.len(), 1);
            assert_eq!(
                effects[0].path,
                PathBuf::from("/tmp/fresh-root/src/lib.rs")
            );
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
        let effects = normalize_event(watch_event(WatchEventKind::Rename));
        assert_eq!(effects.len(), 2);
        assert!(!effects[0].upsert);
        assert!(effects[1].upsert);
    }

    #[test]
    fn should_flush_stays_quiet_below_thresholds() {
        assert!(!should_flush(Duration::from_millis(200), Duration::from_secs(1)));
        assert!(!should_flush(Duration::from_millis(200), Duration::from_millis(1500)));
    }

    #[test]
    fn should_flush_fires_on_quiet_period_or_max_window() {
        assert!(should_flush(Duration::from_millis(300), Duration::from_secs(2)));
        assert!(should_flush(Duration::from_millis(50), Duration::from_secs(2)));
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
}
