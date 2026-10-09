use crate::config::DeduplicationConfig;
use crate::events::{
    EpisodicPointerEcho, ExportFreshnessDirty, GuardEventLog, ThalamicBus,
};
use crate::judgment::DeduplicationChecker;
use crate::traits::{Embedder, VectorStore};
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// How often the daemon flushes to durable storage. Anything written between
/// checkpoints is lost if the process is killed, so this bounds the damage.
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Who this daemon is: the build it was STARTED from. The fingerprint is
/// captured once at startup because the file on disk can be replaced under a
/// running process -- the guinea pig re-verified a shipped fix through a
/// pre-fix daemon and filed a false "still open" (round 3, BUG-10 follow-up).
#[derive(Clone, serde::Serialize)]
pub(crate) struct BuildInfo {
    version: &'static str,
    exe_hash: u64,
    exe_len: u64,
    pid: u32,
    started_at: i64,
}

/// FNV-1a over the current executable, plus its length. Hand-rolled arithmetic
/// stays stable across builds and toolchains where std's hasher does not, and
/// the comparison is daemon-startup-time versus the binary on disk now.
pub(crate) fn exe_fingerprint() -> (u64, u64) {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut len = 0u64;
    if let Ok(path) = std::env::current_exe() {
        if let Ok(bytes) = std::fs::read(&path) {
            len = bytes.len() as u64;
            for b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    (hash, len)
}

#[derive(Clone)]
struct AppState {
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    /// The thalamic bus. In the state so every request handler can emit the
    /// pulses its storage mutations produce; `POST /bus/metrics` reports it.
    /// Constructed before the HTTP server binds, so no request can arrive
    /// while it is still being wired.
    bus: Arc<ThalamicBus>,
    /// The walks in flight, which outlive the requests that started them.
    ingests: Arc<crate::ingest_jobs::IngestJobs>,
    /// Fires once, when something asks the daemon to stop. Taken by whoever
    /// gets there first so a second /shutdown call is harmless.
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// Memory deduplication checker (None if disabled or misconfigured)
    deduplication_checker: Option<Arc<DeduplicationChecker>>,
    /// Guard validator for behavioral constraint checking
    guard_validator: Arc<crate::guard::GuardValidator>,
    /// The build this process was started from, for /info.
    build: BuildInfo,
}

#[derive(Deserialize)]
struct IngestReq {
    dir: String,
    namespace: String,
}

#[derive(Deserialize)]
struct BackupReq {
    dir: String,
}

#[derive(Deserialize)]
struct DeleteReq {
    namespace: String,
    id: String,
}

#[derive(Deserialize)]
struct ArchiveReq {
    namespace: String,
    id: String,
}

#[derive(Deserialize)]
struct EditReq {
    old_namespace: String,
    id: String,
    new_namespace: String,
    content: String,
    location: String,
}

#[derive(Deserialize)]
struct GraphQuery {
    namespace: Option<String>,
    /// Guinea-pig BUG-7: superseded rows are excluded by default here and
    /// carried marked when explicitly asked for.
    include_superseded: Option<bool>,
    /// Skip the namespace filter and return the whole store's graph: the CLI
    /// `export-graph` writes every namespace and must mean the same thing
    /// with or without a daemon.
    all: Option<bool>,
}

pub async fn start_daemon(
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    deduplication_config: Option<DeduplicationConfig>,
) -> anyhow::Result<()> {
    // The port the MCP proxy and CLI hardcode; changing it changes the wire
    // contract, so production never does. Tests pass an ephemeral address to
    // `start_daemon_on` directly.
    start_daemon_on("127.0.0.1:34343".to_string(), embedder, vector_store, deduplication_config).await
}

/// The pulse queue's bound. Ten thousand mutations is minutes of headroom at
/// the daemon's write rate; past it the bus drops oldest and says so in
/// `POST /bus/metrics` rather than growing without limit.
///
/// `pub(crate)` because `neurostrata-mcp bus-metrics` builds the same kind of
/// bus when no daemon is running to ask.
pub(crate) const BUS_CAPACITY: usize = 10_000;

/// Build the daemon's thalamic bus: the bounded queue and the three v1
/// subscribers, wired to the store the daemon opened.
///
/// The store is attached *before* any subscriber registers: a registered
/// subscriber can be handed a pulse the moment one is emitted, and its
/// `SubscriberContext` must find the daemon's store, not the dispatcher's
/// inert stand-in. `dimensions` is the embedder's width because every
/// subscriber's row is a row like any other -- LadybugDB's `Memory.embedding`
/// is a fixed-size `FLOAT[N]`, and the wrong width is a rejected write.
///
/// `project_root` anchors `EpisodicPointerEcho`'s buffer. The production
/// caller is [`daemon_bus`], which resolves it from the environment rather
/// than guessing from the process's directory; tests pass a scratch root.
pub(crate) fn build_thalamic_bus(
    vector_store: Arc<dyn VectorStore>,
    dimensions: usize,
    project_root: impl Into<PathBuf>,
) -> Arc<ThalamicBus> {
    let bus = Arc::new(ThalamicBus::new(BUS_CAPACITY));
    bus.attach_store(vector_store);
    bus.register(Box::new(EpisodicPointerEcho::new(project_root)));
    bus.register(Box::new(ExportFreshnessDirty::new(dimensions)));
    bus.register(Box::new(GuardEventLog::new(dimensions)));
    bus
}

/// Env var naming the project the daemon serves.
const PROJECT_ROOT_ENV: &str = "NEUROSTRATA_PROJECT_ROOT";

/// The project the daemon serves, named by the operator. Under
/// `systemd --user` the daemon's cwd is `$HOME`, so the process directory is
/// not the tree any pulse belongs to; an echo written there lands in the
/// wrong project's buffer entirely (task-8 review, C2). The env var is what
/// a unit file or wrapper shell sets; the cwd stays as the fallback so a
/// plain `neurostrata-mcp daemon` on a developer's machine still serves the
/// tree it was started from.
pub(crate) fn resolve_project_root() -> PathBuf {
    match std::env::var_os(PROJECT_ROOT_ENV) {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => match std::env::current_dir() {
            Ok(dir) => dir,
            // An empty root would make the echo path relative to the process
            // directory -- the very `$HOME` symptom the env anchor exists to
            // avoid, so say so instead of failing silently (task-8 review,
            // fix round 2, minor 1).
            Err(e) => {
                tracing::warn!(
                    "{PROJECT_ROOT_ENV} is unset and the process directory is unreadable ({e}); \
                     the pointer echo has no project root to anchor to"
                );
                PathBuf::new()
            }
        },
    }
}

/// The daemon's own bus. The only production path into [`build_thalamic_bus`]:
/// startup and the tests construct through here, so the anchor the
/// subscribers get is the one the daemon resolved, not a second guess made
/// at each call site.
pub(crate) fn daemon_bus(
    vector_store: Arc<dyn VectorStore>,
    dimensions: usize,
) -> Arc<ThalamicBus> {
    build_thalamic_bus(vector_store, dimensions, resolve_project_root())
}

pub(crate) async fn start_daemon_on(
    bind_addr: String,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    deduplication_config: Option<DeduplicationConfig>,
) -> anyhow::Result<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    // Initialize deduplication checker if config provided
    // The presence of a judgment API key is the enable signal - no redundant config
    let deduplication_checker = if let Some(config) = deduplication_config {
        if config.judgment_api_key.is_some() {
            match DeduplicationChecker::new(config) {
                Ok(checker) => {
                    tracing::info!("Memory deduplication enabled with judgment model");
                    Some(Arc::new(checker))
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize deduplication checker: {}", e);
                    None
                }
            }
        } else {
            tracing::debug!("Deduplication config present but no judgment API key - feature disabled");
            None
        }
    } else {
        None
    };

    // Initialize guard validator for behavioral constraint checking
    let guard_validator = Arc::new(crate::guard::GuardValidator::new(
        vector_store.clone(),
        embedder.clone(),
        "guard",
    ));

    // The thalamic bus, built before anything can serve -- a request that
    // lands on the first accepted connection emits into a wired bus or
    // reports its metrics, never into a bus under construction. The
    // embedder's width is read here because `embedder` moves into the
    // state below.
    let bus = daemon_bus(vector_store.clone(), embedder.dimensions());

    // Kept here as well as in the state, because stopping has to wait for it.
    let ingests = Arc::new(crate::ingest_jobs::IngestJobs::new());
    let fingerprint = exe_fingerprint();
    let state = AppState {
        embedder,
        vector_store: vector_store.clone(),
        bus,
        ingests: ingests.clone(),
        shutdown: Arc::new(Mutex::new(Some(shutdown_tx))),
        deduplication_checker,
        guard_validator,
        build: BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            exe_hash: fingerprint.0,
            exe_len: fingerprint.1,
            pid: std::process::id(),
            started_at: chrono::Utc::now().timestamp(),
        },
    };

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/info", get(handle_info))
        .route("/graph", get(handle_get_graph))
        .route("/ingest", post(handle_ingest))
        .route("/delete", post(handle_delete))
        .route("/memory/archive", post(handle_memory_archive))
        .route("/edit", post(handle_edit))
        .route("/set-metadata", post(handle_set_metadata))
        .route("/validate", post(handle_validate))
        .route("/mcp", post(handle_mcp))
        .route("/backup", post(handle_backup))
        .route("/tasks/import", post(handle_tasks_import))
        .route("/cli/read", post(handle_cli_read))
        .route("/tasks/gate", post(handle_tasks_gate))
        .route("/bus/metrics", post(handle_bus_metrics))
        .route("/shutdown", post(handle_shutdown))
        .with_state(state);

    // Bound the loss window for anything that kills us without warning.
    //
    // This is the ONLY place a checkpoint happens while the daemon is serving.
    // It used to run inside each write handler, which looked safer and was far
    // worse: a checkpoint waits for every active transaction to drain, and
    // under a steady stream of queries that window never opens, so the engine
    // blocked for its own timeout -- around two and a half minutes -- with the
    // caller still waiting on the response (bead neurostrata-3fi.6.4). Out here
    // a failure costs a retry instead of a request, and the write is already in
    // the log either way.
    let periodic_store = vector_store.clone();
    // Stopped before the final checkpoint, not aborted: a checkpoint it has
    // already started runs on a blocking thread that abort cannot reach, and the
    // final checkpoint would then contend with it.
    let (stop_periodic, mut periodic_stopped) = tokio::sync::watch::channel(false);
    let periodic = tokio::spawn(async move {
        let mut wait = CHECKPOINT_INTERVAL;
        let mut failures: u32 = 0;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = periodic_stopped.changed() => break,
            }

            if !periodic_store.is_dirty() {
                wait = CHECKPOINT_INTERVAL;
                continue;
            }

            match periodic_store.checkpoint().await {
                Ok(()) => {
                    if failures > 0 {
                        eprintln!(
                            "Checkpoint succeeded after {} failed attempts; those writes are on disk now.",
                            failures
                        );
                    }
                    failures = 0;
                    wait = CHECKPOINT_INTERVAL;
                }
                Err(e) => {
                    failures += 1;
                    // Say it once, then only occasionally: a busy database can
                    // refuse the quiet moment for a while, and a warning per
                    // attempt would bury the log without adding anything.
                    if failures == 1 {
                        eprintln!("WARNING: checkpoint failed, so recent writes stay in the log until one succeeds. Retrying: {}", e);
                    } else if failures % 10 == 0 {
                        eprintln!("WARNING: {} checkpoints in a row have failed -- everything written since the last success would be lost to a hard kill: {}", failures, e);
                    }
                    wait = checkpoint_backoff(failures);
                }
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    eprintln!("NeuroStrata Daemon listening on {bind_addr}");
    let cause = Arc::new(Mutex::new(ShutdownCause::Requested));
    let recorded_cause = cause.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let why = shutdown_signal(shutdown_rx).await;
            if let Ok(mut slot) = recorded_cause.lock() {
                *slot = why;
            }
        })
        .await?;

    let cause = cause.lock().map(|c| *c).unwrap_or(ShutdownCause::Requested);

    // An ingest runs detached from the request that started it, so the graceful
    // shutdown above did not wait for it. Stopping without waiting left the
    // runtime to drop the walk after it had cleared the namespace but before it
    // had rebuilt or relinked it. The periodic checkpoint keeps running
    // meanwhile, and is stopped only once the walk is done.
    if !ingests.wait_idle(ingest_wait_budget(cause)).await {
        eprintln!("WARNING: an ingest was still running when the daemon had to stop, so its namespace may be only partly rebuilt. Ingest it again.");
    }

    let _ = stop_periodic.send(true);
    let _ = periodic.await;

    // The whole point of stopping gracefully: get everything on disk before exit.
    // A checkpoint needs every transaction to drain, and detached work such as
    // access counting can still be finishing, so a refusal is retried rather
    // than accepted. Exiting after one that never succeeded is an error.
    let deadline = tokio::time::Instant::now() + final_checkpoint_budget(cause);
    let mut failures: u32 = 0;
    loop {
        match vector_store.checkpoint().await {
            Ok(()) => {
                eprintln!("Checkpoint complete. NeuroStrata Daemon stopped.");
                return Ok(());
            }
            Err(e) => {
                failures += 1;
                let wait = checkpoint_backoff(failures);
                if tokio::time::Instant::now() + wait > deadline {
                    eprintln!(
                        "ERROR: final checkpoint failed {} time(s), recent writes may be lost: {}",
                        failures, e
                    );
                    return Err(anyhow::anyhow!(
                        "final checkpoint failed after {} attempt(s): {}",
                        failures,
                        e
                    ));
                }
                eprintln!(
                    "Final checkpoint refused (attempt {}), retrying in {}s: {}",
                    failures,
                    wait.as_secs(),
                    e
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

async fn handle_get_graph(
    State(state): State<AppState>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let requested = query.namespace.unwrap_or_else(|| "global".to_string());
    let ns = crate::server::resolve_namespace(&state.vector_store, &requested).await;
    
    // Using export_graph here temporarily or implement native LadybugDB querying here
    // For now, let's just use export_graph (which gets everything) and filter by namespace
    // In a real refactor, we would add get_graph_by_namespace to VectorStore.
    // Wait! VectorStore has export_graph() returning the whole graph!
    let data = state
        .vector_store
        .export_graph(query.include_superseded.unwrap_or(false))
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if query.all.unwrap_or(false) {
        return Ok(Json(data));
    }
    
    // We can just return it all and let the client filter, or we can filter it here.
    // The Tauri backend did: "MATCH (n:Memory) WHERE n.namespace = 'global' OR n.namespace = '{ns}'"
    // Let's filter it.
    let mut filtered_nodes = Vec::new();
    let mut filtered_links = Vec::new();
    let mut allowed_ids = std::collections::HashSet::new();

    if let Some(nodes) = data.get("nodes").and_then(|n| n.as_array()) {
        for node in nodes {
            if let Some(n_ns) = node.get("namespace").and_then(|ns| ns.as_str()) {
                if n_ns == "global" || n_ns == ns {
                    filtered_nodes.push(node.clone());
                    if let Some(id) = node.get("id").and_then(|i| i.as_str()) {
                        allowed_ids.insert(id.to_string());
                    }
                }
            }
        }
    }

    // export_graph emits "links"; reading "edges" here served an edgeless graph.
    if let Some(links) = data.get("links").and_then(|l| l.as_array()) {
        for link in links {
            let source = link.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let target = link.get("target").and_then(|s| s.as_str()).unwrap_or("");
            if allowed_ids.contains(source) && allowed_ids.contains(target) {
                filtered_links.push(link.clone());
            }
        }
    }

    Ok(Json(serde_json::json!({
        "nodes": filtered_nodes,
        "links": filtered_links
    })))
}

async fn handle_ingest(
    State(state): State<AppState>,
    Json(req): Json<IngestReq>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    // The shipped schema, the same one the CLI and the MCP tool use. This route
    // carried its own inline copy declaring rust and nothing else, so the GUI --
    // which is this route's main caller -- built a graph with no Python, Go,
    // TypeScript or Java symbols in it, and no structs or impls even in Rust.
    // Two ingests of one repository produced different graphs depending on which
    // surface asked (bead neurostrata-tad).
    let schema_str = include_str!("schema.json");
    let schema = crate::parser::schema::ParserSchema::load(schema_str)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // The GUI derives this from the folder name, so it arrives in whatever
    // case the checkout happens to use (bead neurostrata-fld).
    let namespace = crate::server::resolve_namespace(&state.vector_store, &req.namespace).await;

    // The walk belongs to the registry, not to this request: a client that
    // disconnects no longer takes it down half-finished (bead neurostrata-7ej).
    let progress = state
        .ingests
        .run(
            &namespace,
            &req.dir,
            schema,
            state.embedder.clone(),
            state.vector_store.clone(),
        )
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::to_value(progress).unwrap_or_else(
        |_| serde_json::json!({ "state": "finished" }),
    )))
}

/// Backup and restore run here rather than in the CLI so they work against a
/// live daemon: the database is single-writer, and the daemon holds that writer.
async fn handle_backup(
    State(state): State<AppState>,
    Json(req): Json<BackupReq>,
) -> Result<String, (axum::http::StatusCode, String)> {
    state
        .vector_store
        .export_database(&req.dir)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(format!("Backed up to {}", req.dir))
}

#[derive(serde::Deserialize)]
struct ImportReq {
    namespace: String,
    from_beads: String,
}
/// The beads migration runs here for the same reason (guinea-pig BUG-5): the
/// import is a store write, and the daemon is the store's single writer. The
/// CLI used to demand a shutdown first -- tearing down the shared daemon that
/// every console was using, which is exactly what pushed an agent toward
/// forcing its own instances.
async fn handle_tasks_import(
    State(state): State<AppState>,
    Json(req): Json<ImportReq>,
) -> Result<String, (axum::http::StatusCode, String)> {
    let summary = crate::task::import_beads(
        state.vector_store.clone(),
        state.embedder.clone(),
        &req.namespace,
        &req.from_beads,
    )
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(serde_json::to_string(&summary).unwrap_or_default())
}

#[derive(serde::Deserialize)]
struct ReadReq {
    op: String,
    namespace: Option<String>,
}

/// Read-only CLI proxy (guinea-pig BUG-8): `doctor`, `list` and `namespaces`
/// run while the daemon holds the store, through this route, without a second
/// engine ever opening the database.
async fn handle_cli_read(
    State(state): State<AppState>,
    Json(req): Json<ReadReq>,
) -> Result<String, (axum::http::StatusCode, String)> {
    let value = match req.op.as_str() {
        "namespaces" => {
            let namespaces = state
                .vector_store
                .list_namespaces()
                .await
                .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            serde_json::json!({ "namespaces": namespaces })
        }
        "list" => {
            let ns = req.namespace.unwrap_or_default();
            let rows = state
                .vector_store
                .list(&ns, None)
                .await
                .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            serde_json::json!({ "rows": rows })
        }
        other => {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                format!("unknown read op '{}': expected 'namespaces' or 'list'", other),
            ))
        }
    };
    Ok(value.to_string())
}

/// How long to wait after a checkpoint that could not get its quiet moment.
/// Short at first, because the window can open as soon as one query finishes,
/// and capped so a lull is never missed by much.
fn checkpoint_backoff(failures: u32) -> std::time::Duration {
    let secs = 1u64 << failures.min(5);
    std::time::Duration::from_secs(secs.min(30))
}

/// Why the daemon is stopping, which decides how long its final checkpoint may
/// keep retrying.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ShutdownCause {
    /// Windows kills the process roughly five seconds after this, whatever it
    /// is doing.
    ConsoleClosing,
    /// Everything else: /shutdown, Ctrl-C, SIGTERM. Nothing outside is counting
    /// down, so it can wait out a busy engine.
    Requested,
}

fn final_checkpoint_budget(cause: ShutdownCause) -> std::time::Duration {
    match cause {
        ShutdownCause::ConsoleClosing => std::time::Duration::from_secs(4),
        ShutdownCause::Requested => std::time::Duration::from_secs(60),
    }
}

/// How long a stopping daemon waits for a running ingest to finish. A closing
/// console leaves no time for one. Anything else waits, because stopping
/// mid-walk leaves the namespace cleared and only partly rebuilt.
fn ingest_wait_budget(cause: ShutdownCause) -> std::time::Duration {
    match cause {
        ShutdownCause::ConsoleClosing => std::time::Duration::ZERO,
        ShutdownCause::Requested => std::time::Duration::from_secs(300),
    }
}

async fn handle_delete(
    State(state): State<AppState>,
    Json(req): Json<DeleteReq>,
) -> Result<&'static str, (axum::http::StatusCode, String)> {
    // Answer with what the engine said. A bare 500 cost an afternoon here: a
    // caller could not tell a write conflict from a missing id, and neither
    // could the log (bead neurostrata-3fi.6.5).
    let namespace = crate::server::resolve_namespace(&state.vector_store, &req.namespace).await;
    state.vector_store.delete(&namespace, &req.id)
        .await
        .map_err(|e| {
            eprintln!("delete of {} in {} failed: {}", req.id, req.namespace, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
    Ok("OK")
}

/// POST /memory/archive -- the HTTP face of `neurostrata_archive_memory`.
///
/// Deliberately a delegation rather than a second implementation: the handler
/// merges the tombstone into the row's metadata and emits `Archived`, and a
/// copy of that here would be free to drift from the tool surface on exactly
/// the fields an audit trail depends on.
///
/// The verdict is the status code, and the sentence is still the body: the
/// handler returns an `ArchiveOutcome`, this is the one place that reads a
/// variant and picks the code, so there is no second, driftable reading of the
/// same sentence in the route.
///
/// It was always a 200 once, and that made a refused archive indistinguishable
/// from a performed one to `curl -f` and `reqwest::error_for_status()` alike --
/// a mutating endpoint reporting success for a row it did not touch.
async fn handle_memory_archive(
    State(state): State<AppState>,
    Json(req): Json<ArchiveReq>,
) -> (axum::http::StatusCode, String) {
    let outcome = crate::handlers::archive_memory::archive_memory(
        serde_json::json!({ "namespace": req.namespace, "id": req.id }),
        state.vector_store.clone(),
        state.bus.clone(),
    )
    .await;
    let status = match &outcome {
        crate::handlers::archive_memory::ArchiveOutcome::Archived { .. } => axum::http::StatusCode::OK,
        crate::handlers::archive_memory::ArchiveOutcome::NotFound { .. } => {
            axum::http::StatusCode::NOT_FOUND
        }
        crate::handlers::archive_memory::ArchiveOutcome::Invalid(_) => {
            axum::http::StatusCode::BAD_REQUEST
        }
        crate::handlers::archive_memory::ArchiveOutcome::Failed(_) => {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, outcome.to_string())
}

async fn handle_edit(
    State(state): State<AppState>,
    Json(req): Json<EditReq>,
) -> Result<&'static str, axum::http::StatusCode> {
    // The operator's repair path: an in-place rewrite that keeps the id and
    // keeps no history. Agents get neurostrata_supersede_memory instead, which
    // is additive. Editing stays here, behind a human, because it destroys.
    let old_namespace = crate::server::resolve_namespace(&state.vector_store, &req.old_namespace).await;
    let new_namespace = crate::server::resolve_namespace(&state.vector_store, &req.new_namespace).await;
    match crate::server::edit_memory(
        &*state.vector_store,
        &*state.embedder,
        &old_namespace,
        &req.id,
        &new_namespace,
        &req.content,
        &req.location,
    )
    .await
    {
        Ok(crate::server::EditOutcome::Edited) | Ok(crate::server::EditOutcome::NotFound) => Ok("OK"),
        // The next ingest of that namespace would overwrite the edit.
        Ok(crate::server::EditOutcome::Ingested) => Err(axum::http::StatusCode::CONFLICT),
        Err(e) => {
            eprintln!("edit of {} in {} failed: {}", req.id, old_namespace, e);
            Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(serde::Deserialize)]
struct SetMetadataReq {
    namespace: String,
    id: String,
    metadata: serde_json::Value,
}

async fn handle_set_metadata(
    State(state): State<AppState>,
    Json(req): Json<SetMetadataReq>,
) -> Result<&'static str, axum::http::StatusCode> {
    let namespace = crate::server::resolve_namespace(&state.vector_store, &req.namespace).await;
    match crate::server::set_metadata(
        &*state.vector_store,
        &namespace,
        &req.id,
        req.metadata,
    )
    .await
    {
        Ok(crate::server::MetadataOutcome::Updated) | Ok(crate::server::MetadataOutcome::NotFound) => Ok("OK"),
        Ok(crate::server::MetadataOutcome::Ingested) => Err(axum::http::StatusCode::CONFLICT),
        Err(e) => {
            eprintln!("set-metadata of {} in {} failed: {}", req.id, namespace, e);
            Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// Handle a single MCP JSON-RPC line
async fn handle_mcp(
    State(state): State<AppState>,
    Json(request): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    if let Ok(rpc_req) = serde_json::from_value::<crate::server::JsonRpcRequest>(request) {
        let response = crate::server::process_mcp_request(
            rpc_req,
            state.embedder.clone(),
            state.vector_store.clone(),
            state.ingests.clone(),
            state.deduplication_checker.clone(),
            state.bus.clone(),
        )
        .await;
        Json(response)
    } else {
        Json(serde_json::json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "Invalid Request"}}))
    }
}

/// Resolves when the daemon should stop: an explicit POST /shutdown, Ctrl-C, or
/// the OS asking us to go away. On Windows a console close gives roughly five
/// seconds before the process is killed regardless, so the checkpoint that
/// follows has to be quick.
async fn shutdown_signal(rx: oneshot::Receiver<()>) -> ShutdownCause {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("Received Ctrl-C, shutting down.");
    };

    #[cfg(windows)]
    let os_signal = async {
        let mut close = match tokio::signal::windows::ctrl_close() {
            Ok(s) => s,
            Err(_) => return std::future::pending::<ShutdownCause>().await,
        };
        let mut shutdown = match tokio::signal::windows::ctrl_shutdown() {
            Ok(s) => s,
            Err(_) => return std::future::pending::<ShutdownCause>().await,
        };
        tokio::select! {
            _ = close.recv() => eprintln!("Console is closing, shutting down."),
            _ = shutdown.recv() => eprintln!("System is shutting down."),
        }
        ShutdownCause::ConsoleClosing
    };

    #[cfg(unix)]
    let os_signal = async {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return std::future::pending::<ShutdownCause>().await,
        };
        term.recv().await;
        eprintln!("Received SIGTERM, shutting down.");
        ShutdownCause::Requested
    };

    tokio::select! {
        _ = ctrl_c => ShutdownCause::Requested,
        cause = os_signal => cause,
        _ = rx => {
            eprintln!("Shutdown requested over HTTP.");
            ShutdownCause::Requested
        }
    }
}

/// Build identity of the daemon answering. `neurostrata-mcp status` compares
/// it against the installed binary and refuses to call a mismatch "healthy" --
/// a daemon older than this route cannot be the build on disk, and its tools
/// and schemas belong to whatever it was started from.
async fn handle_info(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": state.build.version,
        "exe_hash": state.build.exe_hash,
        "exe_len": state.build.exe_len,
        "pid": state.build.pid,
        "started_at": state.build.started_at,
    }))
}

/// POST /bus/metrics -- the thalamic bus as it stands at this instant.
///
/// The body is `BusMetrics` itself; the bus counts every pulse it accepted,
/// dropped, or lost a subscriber to, so an operator watching a write storm can
/// tell "the bus is keeping up" from "the queue is full" without reading the
/// daemon's log. Deliberately inert: it reads atomics and takes two short
/// locks, and never touches the store, so a daemon wedged in the database can
/// still answer it.
///
/// Serialized by the handler `neurostrata-mcp bus-metrics` calls, so the route
/// and the CLI cannot report different buses; the content type is set by hand
/// because the shared handler returns a `String`.
async fn handle_bus_metrics(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        crate::handlers::bus_metrics::handle_bus_metrics(state.bus.clone()).await,
    )
}

async fn handle_shutdown(State(state): State<AppState>) -> &'static str {
    // A second caller finds None here; stopping twice is not an error.
    let sender = state.shutdown.lock().ok().and_then(|mut guard| guard.take());
    match sender {
        Some(tx) => {
            let _ = tx.send(());
            "Shutting down"
        }
        None => "Already shutting down",
    }
}

async fn handle_validate(
    State(state): State<AppState>,
    Json(request): Json<crate::guard::ValidateRequest>,
) -> Result<Json<crate::guard::ValidateResponse>, (axum::http::StatusCode, String)> {
    // Use sandbox if podman is available
    let use_sandbox = true;
    
    let (verdict, rule_ids) = state
        .guard_validator
        .validate(&request.action_type, &request.payload, &request.cwd, use_sandbox)
        .await;
    
    Ok(Json(crate::guard::ValidateResponse {
        trace_id: request.trace_id,
        verdict,
        rule_ids_triggered: rule_ids,
    }))
}

#[derive(Deserialize)]
struct TaskGateReq {
    namespace: String,
    /// Strict only marks the run in the log; the verdict never changes with
    /// it, because one gate serves MCP, CLI, and the hook (section 4).
    #[serde(default)]
    strict: bool,
}

/// POST /tasks/gate -- what the pre-push hook sees (section 4).
///
/// Metadata-only by construction: the same `gate::run` the MCP validate tool
/// and the CLI use, one `list` scan, no embedder -- so it answers inside a
/// `git push`. Returns `{ok, violations}`; if the tasks cannot be listed at
/// all that is a 500, because a gate that cannot see the work must never
/// report it clean.
async fn handle_tasks_gate(
    State(state): State<AppState>,
    Json(req): Json<TaskGateReq>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let namespace = crate::server::resolve_namespace(&state.vector_store, &req.namespace).await;
    let report = crate::task::gate::run(&state.vector_store, &namespace)
        .await
        .map_err(|e| {
            eprintln!("[neurostrata:gate] gate for '{}' failed: {}", namespace, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;
    if req.strict && !report.ok() {
        eprintln!(
            "[neurostrata:gate] strict: {} violation(s) block the push in '{}'",
            report.violations.len(),
            namespace
        );
    }
    Ok(Json(report.gate_json()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RecursionToken, ThalamicPulse};
    use crate::traits::{MemoryPayload, RelocateOutcome, SearchResult};
    use async_trait::async_trait;
    use std::time::{Duration, Instant};

    /// `std::env` is process-global, so tests that mutate `PROJECT_ROOT_ENV`
    /// must not run concurrently: one test's `remove_var` strands another's
    /// `expect`. Held for the full body of every env-mutating test; poisoning
    /// (a panic while holding it) is tolerated so one failure cannot cascade.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_first_retry_comes_quickly() {
        // The quiet moment a checkpoint needs can open as soon as one query
        // finishes, so the first wait is seconds, not the full interval.
        assert!(checkpoint_backoff(1) < std::time::Duration::from_secs(5));
    }

    #[test]
    fn repeated_failures_back_off_but_never_give_up_for_long() {
        let waits: Vec<u64> = (1..=8).map(|n| checkpoint_backoff(n).as_secs()).collect();

        assert!(waits.windows(2).all(|w| w[1] >= w[0]), "{:?} should not shrink", waits);
        assert!(
            waits.iter().all(|w| *w <= 30),
            "a lull must never be missed by more than half a minute: {:?}",
            waits
        );
        assert_eq!(*waits.last().unwrap(), 30, "and it settles at the cap");
    }

    #[test]
    fn a_closing_console_gets_less_time_than_windows_allows() {
        assert!(final_checkpoint_budget(ShutdownCause::ConsoleClosing) < std::time::Duration::from_secs(5));
    }

    #[test]
    fn an_explicit_stop_waits_out_a_busy_engine() {
        assert!(final_checkpoint_budget(ShutdownCause::Requested) >= std::time::Duration::from_secs(30));
    }

    #[test]
    fn only_a_closing_console_stops_without_waiting_for_an_ingest() {
        assert_eq!(ingest_wait_budget(ShutdownCause::ConsoleClosing), std::time::Duration::ZERO);
        assert!(ingest_wait_budget(ShutdownCause::Requested) >= std::time::Duration::from_secs(60));
    }

    #[test]
    fn the_exe_fingerprint_is_stable_and_covers_the_file() {
        let (h1, l1) = super::exe_fingerprint();
        let (h2, l2) = super::exe_fingerprint();
        assert_eq!((h1, l1), (h2, l2));
        assert!(l1 > 0);
    }

    /// An embedder with no model behind it: the daemon's startup path never
    /// embeds, and `dimensions()` is the value threaded into the bus's
    /// subscribers, so the stub's width is what their writes must carry.
    struct StubEmbedder {
        dimensions: usize,
    }

    #[async_trait]
    impl crate::traits::Embedder for StubEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; self.dimensions])
        }
        fn dimensions(&self) -> usize {
            self.dimensions
        }
    }

    /// A store that remembers every upsert -- `(namespace, id, vector width,
    /// memory_type)` -- and answers everything else with a trivial result.
    /// The bus rows are the point of the assertion; nothing in these tests
    /// reads them back through a query path. `seeded` rows are what `get`
    /// serves, so a test can start with a row already in the store.
    #[derive(Default)]
    struct RecordingStore {
        upserts: Mutex<Vec<(String, String, usize, String)>>,
        seeded: Mutex<Vec<(String, String, Vec<f32>, MemoryPayload)>>,
    }

    impl RecordingStore {
        fn upserts(&self) -> Vec<(String, String, usize, String)> {
            self.upserts.lock().expect("recording store poisoned").clone()
        }

        fn holding(namespace: &str, id: &str, memory_type: &str) -> Self {
            let store = RecordingStore::default();
            store.seeded.lock().expect("recording store poisoned").push((
                namespace.to_string(),
                id.to_string(),
                vec![0.0; 8],
                MemoryPayload {
                    content: "a row worth tombstoning".to_string(),
                    memory_type: memory_type.to_string(),
                    location: String::new(),
                    user_id: "test".to_string(),
                    agent_name: None,
                    location_lines: String::new(),
                    metadata: serde_json::json!({ "origin": "seed" }),
                },
            ));
            store
        }
    }

    #[async_trait]
    impl crate::traits::VectorStore for RecordingStore {
        async fn init(&self, _namespace: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<()> {
            self.upserts
                .lock()
                .expect("recording store poisoned")
                .push((namespace.to_string(), id.to_string(), vector.len(), payload.memory_type));
            Ok(())
        }
        async fn search(
            &self,
            _namespace: &str,
            _vector: Vec<f32>,
            _limit: usize,
        ) -> anyhow::Result<Vec<SearchResult>> {
            Ok(Vec::new())
        }
        async fn delete(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn clear_ingested(&self, _namespace: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn relink_edges(&self, _namespace: &str) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn list(
            &self,
            _namespace: &str,
            _user_id: Option<&str>,
        ) -> anyhow::Result<Vec<SearchResult>> {
            Ok(Vec::new())
        }
        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            Ok(self
                .seeded
                .lock()
                .expect("recording store poisoned")
                .iter()
                .find(|(ns, seeded_id, _, _)| ns == namespace && seeded_id == id)
                .map(|(_, _, vector, payload)| (vector.clone(), payload.clone())))
        }
        async fn relocate(
            &self,
            _id: &str,
            _from: &str,
            _to: &str,
        ) -> anyhow::Result<RelocateOutcome> {
            Ok(RelocateOutcome::Moved)
        }
        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn export_graph(&self, _include_retired: bool) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"nodes": [], "links": []}))
        }
        async fn increment_access_count(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn export_database(&self, _dir: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn checkpoint(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_dirty(&self) -> bool {
            false
        }
    }

    /// A throwaway project root, named like the subscriber tests' scratch
    /// directories so an earlier run's leftover can never be read as this
    /// run's output.
    fn scratch_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-daemon-bus-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// What the daemon registers and what its store then sees, one level below
    /// the router: the pulse goes through the real dispatcher into the real
    /// v1 subscribers. `Created` must flip the freshness sentinel through the
    /// attached store, and `GuardedActionFired` must write exactly one audit
    /// row -- both at the embedder's width. If `attach_store` had never run,
    /// the subscribers would dispatch against the bus's inert stand-in and
    /// this store would record nothing at all.
    #[tokio::test]
    async fn daemon_builds_a_bus_of_three_subscribers_wired_to_its_store() {
        const DIMENSIONS: usize = 42;
        let store = Arc::new(RecordingStore::default());
        let bus = build_thalamic_bus(store.clone(), DIMENSIONS, scratch_root("wiring"));

        // Step 1's subject: exactly the three v1 subscribers are registered.
        let m = bus.metrics();
        assert_eq!(m.subscribers, 3, "the daemon bus must carry the three v1 subscribers: {m:?}");

        bus.emit(
            ThalamicPulse::Created {
                id: "m1".into(),
                namespace: "bus-test".into(),
                kind: "fact".into(),
            },
            &RecursionToken::root(),
        );
        bus.emit(
            ThalamicPulse::GuardedActionFired {
                trace_id: "t1".into(),
                action_type: "shell".into(),
                payload_hash: 0x1234_5678,
                verdict: "allow".into(),
                rule_ids_triggered: vec![],
                namespace: "bus-test".into(),
            },
            &RecursionToken::root(),
        );

        // Poll the dispatcher's own count instead of guessing a sleep: every
        // subscriber has run only once `dispatcher_handled` reaches the two
        // pulses emitted.
        let deadline = Instant::now() + Duration::from_secs(5);
        while bus.metrics().dispatcher_handled < 2 {
            assert!(Instant::now() < deadline, "the dispatcher stalled: {:?}", bus.metrics());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let rows = store.upserts();
        // Created flips the sentinel once; the guard pulse flips it again
        // (every pulse is a mutation of what the store holds, including the
        // audit row itself) and logs its own row. The order is the
        // registration order.
        let kinds: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["freshness_flag", "freshness_flag", "guard_event"],
            "the daemon bus's subscribers write exactly the sentinel and audit rows: {rows:?}"
        );
        for (namespace, _id, width, _kind) in &rows {
            assert_eq!(namespace, "bus-test", "rows land in the pulse's namespace");
            assert_eq!(
                *width, DIMENSIONS,
                "subscribers constructed from the embedder's width, or the engine rejects the row"
            );
        }
    }

    /// C2: the anchor must come from configuration, not the process's
    /// directory -- `systemd --user` starts the daemon in `$HOME`, and a
    /// pointer echo written there belongs to no project. Both resolution
    /// branches are contract: env wins, and only an unset env falls back.
    #[test]
    fn project_root_comes_from_the_env_and_falls_back_to_the_cwd() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let configured = scratch_root("root-env");
        std::env::set_var(PROJECT_ROOT_ENV, &configured);
        assert_eq!(
            resolve_project_root(),
            configured,
            "the env var names the served project even though the process sits elsewhere"
        );

        std::env::remove_var(PROJECT_ROOT_ENV);
        assert_eq!(
            resolve_project_root(),
            std::env::current_dir().unwrap(),
            "with no env var the daemon serves the tree it was started from"
        );
        assert!(
            std::env::var_os(PROJECT_ROOT_ENV).is_none(),
            "the var is restored to unset so no later test observes this root"
        );
    }

    /// The same assertion one level up: through `daemon_bus`, the only
    /// production construction path, the registered `EpisodicPointerEcho`
    /// writes under the configured root. Mutation-proof -- re-point the env
    /// var and the echo follows to the new root and abandons the old one.
    #[tokio::test]
    async fn the_daemon_anchors_its_pointer_echo_at_the_env_project_root() {
        const DIMENSIONS: usize = 8;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fn echoed(root: &std::path::Path) -> Option<String> {
            std::fs::read_to_string(
                root.join(".NeuroStrata").join("sessions").join("current.md"),
            )
            .ok()
        }
        async fn dispatched(bus: &Arc<ThalamicBus>, pulses: u64) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while bus.metrics().dispatcher_handled < pulses {
                assert!(Instant::now() < deadline, "the dispatcher stalled: {:?}", bus.metrics());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        let store = Arc::new(RecordingStore::default());
        let root_a = scratch_root("anchor-a");
        std::env::set_var(PROJECT_ROOT_ENV, &root_a);

        let bus = daemon_bus(store.clone(), DIMENSIONS);
        bus.emit(
            ThalamicPulse::Created {
                id: "c2-a".into(),
                namespace: "bus-test".into(),
                kind: "fact".into(),
            },
            &RecursionToken::root(),
        );
        dispatched(&bus, 1).await;

        let text_a = echoed(&root_a).expect("the pointer lands under the configured root");
        assert!(text_a.contains("c2-a"), "the echo carries the pulse id: {text_a}");
        if let Some(cwd_text) = echoed(&std::env::current_dir().unwrap()) {
            assert!(
                !cwd_text.contains("c2-a"),
                "the configured root served the echo, not the daemon's cwd"
            );
        }

        // Re-point the env var: a bus built afterwards follows, and the
        // root that stopped being configured sees nothing new.
        let root_b = scratch_root("anchor-b");
        std::env::set_var(PROJECT_ROOT_ENV, &root_b);

        let bus = daemon_bus(store.clone(), DIMENSIONS);
        bus.emit(
            ThalamicPulse::Created {
                id: "c2-b".into(),
                namespace: "bus-test".into(),
                kind: "fact".into(),
            },
            &RecursionToken::root(),
        );
        dispatched(&bus, 1).await;

        let text_b = echoed(&root_b).expect("re-pointing the env var re-anchors the echo");
        assert!(text_b.contains("c2-b"), "the echo carries the pulse id: {text_b}");
        assert!(
            !echoed(&root_a).unwrap().contains("c2-b"),
            "the old root never sees pulses emitted after it stopped being configured"
        );
        std::env::remove_var(PROJECT_ROOT_ENV);
        assert!(
            std::env::var_os(PROJECT_ROOT_ENV).is_none(),
            "the var is restored to unset so no later test observes this root"
        );
    }

    /// The cheap proof the two env tests above are serialised, not merely
    /// well-behaved by luck: mutate `PROJECT_ROOT_ENV` twice under
    /// `ENV_LOCK`, and each pass must see exactly what it set -- a stale
    /// value from a concurrent test would trip the first assertion, and a
    /// leaked value would trip the last.
    #[test]
    fn the_env_lock_makes_repeated_mutation_deterministic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for tag in ["pass-one", "pass-two"] {
            let root = scratch_root(tag);
            std::env::set_var(PROJECT_ROOT_ENV, &root);
            assert_eq!(
                resolve_project_root(),
                root,
                "{tag}: the root resolved is the one this test just set, with no interference"
            );
            std::env::remove_var(PROJECT_ROOT_ENV);
            assert!(
                std::env::var_os(PROJECT_ROOT_ENV).is_none(),
                "{tag}: the var is cleared at the end of every pass"
            );
        }
    }

    /// The init-order drill (Step 6): a request that lands immediately after
    /// the daemon starts serving must find the bus already registered -- the
    /// bus is built before the HTTP server binds, so the first
    /// `POST /bus/metrics` of the process reports all three subscribers. If
    /// startup served before finishing the bus, this is the request that
    /// would catch it, at the same endpoint Step 7 curls against the live
    /// daemon.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_bus_is_live_before_the_first_request() {
        // An ephemeral port, and the full production startup sequence: the
        // machine's live daemon owns 34343, and a test must never contend
        // with it.
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let daemon = tokio::spawn(start_daemon_on(
            addr.clone(),
            Arc::new(StubEmbedder { dimensions: 8 }),
            Arc::new(RecordingStore::default()),
            None,
        ));
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Wait for the listener to answer at all; a daemon that could not
        // bind fails its own join below rather than hanging here.
        let mut serving = false;
        for _ in 0..500 {
            if client
                .get(format!("{base}/health"))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                serving = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(serving, "daemon never started serving on {addr}");

        let response = client.post(format!("{base}/bus/metrics")).send().await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.text().await.unwrap();
        assert!(
            body.contains("\"subscribers\":3"),
            "the first request after start must see the fully wired bus, got: {body}"
        );

        // Stop it over the same path the CLI uses, and require the shutdown
        // to finish cleanly rather than leaving the task running to test end.
        client.post(format!("{base}/shutdown")).send().await.unwrap();
        daemon.await.unwrap().unwrap();
    }

    /// The daemon on an ephemeral port, and a client that has waited until it
    /// answers. An ephemeral port because the machine's live daemon owns
    /// 34343, and the full production startup sequence so the routes under test
    /// are the ones production registers.
    async fn spawn_test_daemon_with(
        store: Arc<RecordingStore>,
    ) -> (String, reqwest::Client, tokio::task::JoinHandle<anyhow::Result<()>>) {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let daemon = tokio::spawn(start_daemon_on(
            addr.clone(),
            Arc::new(StubEmbedder { dimensions: 8 }),
            store,
            None,
        ));
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Wait for the listener to answer at all; a daemon that could not bind
        // fails its own join below rather than hanging here.
        let mut serving = false;
        for _ in 0..500 {
            if client
                .get(format!("{base}/health"))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                serving = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(serving, "daemon never started serving on {addr}");
        (base, client, daemon)
    }

    async fn spawn_test_daemon() -> (String, reqwest::Client, tokio::task::JoinHandle<anyhow::Result<()>>) {
        spawn_test_daemon_with(Arc::new(RecordingStore::default())).await
    }

    /// `POST /memory/archive` must reach the one handler the MCP tool surface
    /// and the CLI call -- twice, on both sides of the branch that matters --
    /// and it must *report* each side as its own status.
    ///
    /// With no such row the answer must be the handler's own "No memory with
    /// id ...", which an unregistered path could never produce (it would be a
    /// bare 404 saying nothing about memories), and it must be a 404: this is
    /// a mutating endpoint, so a refusal read as 200 is a row reported
    /// archived that is still live. With a real row the route has to write the
    /// tombstone and emit `Archived`, and answer 200: the write shows up as an
    /// upsert on the store the daemon was handed, and the pulse as a nonzero
    /// count on the daemon's own bus.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_archive_route_delegates_to_the_handler_the_tool_also_calls() {
        let store = Arc::new(RecordingStore::holding("bus-test", "m1", "fact"));
        let (base, client, daemon) = spawn_test_daemon_with(store.clone()).await;

        let missing = client
            .post(format!("{base}/memory/archive"))
            .json(&serde_json::json!({ "namespace": "bus-test", "id": "no-such-row" }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            missing.status(),
            axum::http::StatusCode::NOT_FOUND,
            "a refused archive must not be a 200: curl -f and error_for_status() read that as a row archived"
        );
        let body = missing.text().await.unwrap();
        assert!(
            body.contains("No memory with id no-such-row"),
            "a missing row is refused in the handler's own words, not a route's: {body}"
        );
        assert!(
            store.upserts().is_empty(),
            "a refused archive writes nothing: {:?}",
            store.upserts()
        );

        let archived = client
            .post(format!("{base}/memory/archive"))
            .json(&serde_json::json!({ "namespace": "bus-test", "id": "m1" }))
            .send()
            .await
            .unwrap();
        assert_eq!(
                            archived.status(),
                            axum::http::StatusCode::OK,
                            "the one status the route may answer for a row it really tombstoned"
                        );
        let body = archived.text().await.unwrap();
        assert!(
            body.contains("Archived m1 in namespace bus-test"),
            "the route must answer with the handler's success sentence, got: {body}"
        );
        assert!(
            store.upserts().iter().any(|(ns, id, _, kind)| {
                ns == "bus-test" && id == "m1" && kind == "fact"
            }),
            "the tombstone must be written back over the row itself: {:?}",
            store.upserts()
        );

        // The pulse is the half that only the handler does: the route has to
        // have gone through it, or the daemon's bus would still be at zero.
        let metrics: serde_json::Value = serde_json::from_str(
            &client.post(format!("{base}/bus/metrics")).send().await.unwrap().text().await.unwrap(),
        )
        .expect("bus metrics body is JSON");
        assert!(
            metrics["events_emitted"].as_u64().unwrap_or(0) >= 1,
            "the Archived pulse must have been emitted by the handler the route calls: {metrics}"
        );

        client.post(format!("{base}/shutdown")).send().await.unwrap();
        daemon.await.unwrap().unwrap();
    }

    /// `POST /bus/metrics` and `neurostrata-mcp bus-metrics` must be the same
    /// answer to the same question, so the route is held to the handler's
    /// field set. The values differ on purpose -- the route reads the daemon's
    /// wired bus, the handler here a bare one -- and comparing them would only
    /// assert that two different buses report different subscriber counts.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_bus_metrics_route_answers_in_the_shared_handlers_shape() {
        let (base, client, daemon) = spawn_test_daemon().await;

        let response = client.post(format!("{base}/bus/metrics")).send().await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "the route must still answer as JSON after delegating to a String-returning handler"
        );
        let body = response.text().await.unwrap();
        let from_route: serde_json::Value =
            serde_json::from_str(&body).expect("the route body is the metrics object");
        let from_handler: serde_json::Value = serde_json::from_str(
            &crate::handlers::bus_metrics::handle_bus_metrics(Arc::new(ThalamicBus::new(8))).await,
        )
        .expect("the handler body is the metrics object");

        let fields = |v: &serde_json::Value| {
            v.as_object()
                .expect("metrics is an object")
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            fields(&from_route),
            fields(&from_handler),
            "one shape, one source: the route and the CLI must report the same fields"
        );
        assert_eq!(
            from_route["subscribers"], 3,
            "and the route still reads the daemon's own wired bus: {body}"
        );

        client.post(format!("{base}/shutdown")).send().await.unwrap();
        daemon.await.unwrap().unwrap();
    }
}
