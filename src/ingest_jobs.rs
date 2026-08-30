//! Keeps a directory ingest running when the caller that asked for it leaves.
//!
//! Ingestion used to run inside the request handler. axum drops that future the
//! moment the client disconnects, so a caller that timed out, or a window that
//! was closed, took the walk down with it -- part-built, before `relink_edges`,
//! and therefore with every GOVERNS edge missing. Nothing recorded that it had
//! happened: the daemon stayed healthy and idle, and the only symptom was a
//! graph that looked wrong (bead neurostrata-7ej).
//!
//! The walk now belongs to a task of its own. Requests wait on it and report
//! what it did, but leaving no longer stops it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::watch;

use crate::parser::ingest::IngestObserver;
use crate::parser::schema::ParserSchema;
use crate::traits::{Embedder, VectorStore};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestState {
    Running,
    Finished,
    Failed,
}

/// What a walk has done so far. Reported while it runs, and kept afterwards so
/// asking about a namespace tells you how its last ingest ended.
#[derive(Clone, Debug, Serialize)]
pub struct IngestProgress {
    pub namespace: String,
    pub dir: String,
    pub state: IngestState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub files_ingested: usize,
    pub symbols_ingested: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relinked_edges: Option<usize>,
    pub started_unix: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_unix: Option<u64>,
}

impl IngestProgress {
    fn started(namespace: &str, dir: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            dir: dir.to_string(),
            state: IngestState::Running,
            error: None,
            files_ingested: 0,
            symbols_ingested: 0,
            last_file: None,
            relinked_edges: None,
            started_unix: now_unix(),
            finished_unix: None,
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Turns the walk's callbacks into something a waiting request can read.
struct ProgressReporter {
    tx: watch::Sender<IngestProgress>,
}

impl IngestObserver for ProgressReporter {
    fn file_ingested(&self, path: &str, symbols: usize) {
        self.tx.send_modify(|progress| {
            progress.files_ingested += 1;
            progress.symbols_ingested += symbols;
            progress.last_file = Some(path.to_string());
        });
    }

    fn relinked(&self, edges: usize) {
        self.tx.send_modify(|progress| progress.relinked_edges = Some(edges));
    }
}

/// One walk per namespace, owned here rather than by whoever asked for it.
#[derive(Default)]
pub struct IngestJobs {
    /// Keyed by namespace. An entry outlives its walk, which costs one receiver
    /// per namespace and keeps the check below a single lookup.
    jobs: Mutex<HashMap<String, watch::Receiver<IngestProgress>>>,
}

impl IngestJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs the ingest and waits for it, or waits for the one already running.
    ///
    /// Two walks over one namespace must never overlap: ingestion begins by
    /// deleting every row it is about to rebuild, so the second would clear
    /// what the first had done. A caller that gave up and retried used to
    /// produce exactly that pair.
    pub async fn run(
        self: &Arc<Self>,
        namespace: &str,
        dir: &str,
        schema: ParserSchema,
        embedder: Arc<dyn Embedder>,
        vector_store: Arc<dyn VectorStore>,
    ) -> anyhow::Result<IngestProgress> {
        let mut rx = {
            let mut jobs = self.jobs.lock().unwrap();

            let already_running = jobs
                .get(namespace)
                .is_some_and(|rx| rx.borrow().state == IngestState::Running);

            if already_running {
                jobs.get(namespace).expect("checked just above").clone()
            } else {
                let (tx, rx) = watch::channel(IngestProgress::started(namespace, dir));
                jobs.insert(namespace.to_string(), rx.clone());

                let namespace = namespace.to_string();
                let dir = dir.to_string();

                // Detached deliberately. tokio::spawn keeps running when its
                // JoinHandle is dropped, which is what makes a disconnect stop
                // mattering: the request goes, the walk does not.
                tokio::spawn(async move {
                    let reporter: Arc<dyn IngestObserver> =
                        Arc::new(ProgressReporter { tx: tx.clone() });

                    let outcome = crate::parser::ingest::ingest_directory(
                        std::path::Path::new(&dir),
                        &schema,
                        embedder,
                        vector_store,
                        &namespace,
                        Some(reporter),
                    )
                    .await;

                    tx.send_modify(|progress| {
                        progress.finished_unix = Some(now_unix());
                        match &outcome {
                            Ok(()) => progress.state = IngestState::Finished,
                            Err(e) => {
                                progress.state = IngestState::Failed;
                                progress.error = Some(e.to_string());
                            }
                        }
                    });
                });

                rx
            }
        };

        loop {
            // Cloned out rather than held: the borrow guard must not be alive
            // across the await below.
            let progress = rx.borrow_and_update().clone();
            match progress.state {
                IngestState::Finished => return Ok(progress),
                IngestState::Failed => {
                    return Err(anyhow::anyhow!(progress
                        .error
                        .unwrap_or_else(|| "ingestion failed".to_string())))
                }
                IngestState::Running => {}
            }

            if rx.changed().await.is_err() {
                // The task went away without reporting, which a panic inside it
                // would do. Say so rather than report success.
                return Err(anyhow::anyhow!(
                    "the ingest of namespace '{}' ended without reporting a result",
                    namespace
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(state: IngestState) -> IngestProgress {
        let mut progress = IngestProgress::started("ns", "dir");
        progress.state = state;
        progress
    }

    /// A walk that is still running is what a second caller attaches to, so the
    /// state a live entry reports has to be readable while it runs.
    #[test]
    fn a_live_walk_reports_that_it_is_running() {
        let jobs = IngestJobs::new();
        let (tx, rx) = watch::channel(progress(IngestState::Running));
        jobs.jobs.lock().unwrap().insert("ns".to_string(), rx);

        let running = |jobs: &IngestJobs| {
            jobs.jobs
                .lock()
                .unwrap()
                .get("ns")
                .is_some_and(|rx| rx.borrow().state == IngestState::Running)
        };

        assert!(running(&jobs));

        tx.send_modify(|progress| {
            progress.state = IngestState::Failed;
            progress.error = Some("no such directory".to_string());
        });

        // Finished, so the next caller starts a walk rather than attaching to
        // one that has already ended.
        assert!(!running(&jobs));
    }

    /// Progress is what a caller reads instead of waiting, so the counts have
    /// to accumulate across files rather than report only the last one.
    #[test]
    fn progress_accumulates_over_the_walk() {
        let (tx, rx) = watch::channel(progress(IngestState::Running));
        let reporter = ProgressReporter { tx };

        reporter.file_ingested("src/a.rs", 3);
        reporter.file_ingested("src/b.rs", 4);
        reporter.relinked(20);

        let progress = rx.borrow();
        assert_eq!(progress.files_ingested, 2);
        assert_eq!(progress.symbols_ingested, 7);
        assert_eq!(progress.last_file.as_deref(), Some("src/b.rs"));
        assert_eq!(progress.relinked_edges, Some(20));
    }
}
