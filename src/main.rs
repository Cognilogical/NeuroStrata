// Increase recursion limit for large JSON literals in tools/list
#![recursion_limit = "256"]

mod attempt;
mod buffer;
mod config;
// The context DTOs are shipped ahead of the service that builds plans
// (plan Task 2 of the agent-adoption plan). Nothing constructs them yet,
// so the dead-code lint has no honest answer to give.
#[allow(dead_code)]
mod context;
mod crash;
mod daemon;
mod domain;
mod embed;
mod events;
mod graph_read;
mod guard;
mod handlers;
mod ingest_jobs;
mod judgment;
mod model_downloader;
mod learning;
mod parser;
mod recall_metrics;
mod receipts;
mod registry;
mod search;
mod secrets;
mod server;
mod setup;
mod source_context;
mod store;
mod synaptic_mesh;
mod task;
#[cfg(test)]
mod test_support;
mod tools;
mod traits;
mod util;

use crate::traits::SearchResult;
use crate::traits::{Embedder, VectorStore};
use anyhow::Context;
use clap::{Parser, Subcommand};
use config::Config;
use embed::build_embedder;
use std::collections::HashSet;
use std::sync::Arc;
use store::LadybugStore;

#[derive(Parser, Debug)]
#[command(name = "neurostrata-mcp")]
#[command(about = "🧠 NeuroStrata — MCP Server & CLI Engine", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start NeuroStrata in daemon-only mode
    Daemon,

    /// List all active namespaces
    Namespaces,

    /// List all memories in a given namespace
    List {
        /// The target namespace
        namespace: String,
    },

    /// Ingest a directory of code symbols into a namespace
    Ingest {
        /// The directory path to ingest
        dir: String,

        /// The target namespace
        namespace: String,

        /// Optional path to the parser schema JSON file
        schema_path: Option<String>,
    },

    /// Export the memory graph to a JSON file
    #[command(name = "export-graph")]
    ExportGraph {
        /// Output path for the JSON graph export
        out_path: Option<String>,

        /// Select the registered source marker for the default output path
        #[arg(long)]
        source_id: Option<String>,

        /// Leave superseded (retired) memories out of the export entirely
        #[arg(long)]
        exclude_superseded: bool,

        /// Include tombstoned (archived) memories in the export. Archived
        /// rows are hidden by default; this is the audit / second-look
        /// escape hatch, the same shape `include_archived` carries on
        /// search and snapshot.
        #[arg(long)]
        include_archived: bool,
    },

    /// Delete a memory from a namespace by ID
    Delete {
        /// The target namespace
        namespace: String,

        /// The memory ID to delete
        id: String,
    },

    /// Archive a memory by ID: tombstone it, keeping its content
    ///
    /// Not `delete`. Archiving merges `archived: true` into the row's own
    /// metadata, so the content, the id and the provenance survive the exact
    /// moment someone stops trusting it. It goes through the same handler as
    /// the `neurostrata_archive_memory` tool, so the two cannot disagree
    /// about what "archived" means.
    Archive {
        /// The namespace the memory is in
        namespace: String,

        /// The memory ID to archive
        id: String,
    },

    /// Report what the thalamic bus has done since the daemon started
    ///
    /// Reads the running daemon's bus: the pulses are emitted by the daemon
    /// that owns the store, so a bus built here would count nothing and
    /// answer with zeros meaning "no daemon" rather than "an idle bus".
    #[command(name = "bus-metrics")]
    BusMetrics,

    /// Move a memory into another namespace, by ID
    ///
    /// Destructive: it copies the row and then deletes the original, so it is
    /// a CLI command rather than an MCP tool. `doctor` prints one of these per
    /// id when two spellings of a project need merging.
    Move {
        /// The namespace the memory is in now
        source_namespace: String,

        /// The memory ID to move
        id: String,

        /// The namespace to move it into
        target_namespace: String,
    },

    /// Add a new memory to a namespace
    Add {
        /// The target namespace
        namespace: String,

        /// The memory type (e.g. symbol, text)
        memory_type: String,

        /// The memory content string
        content: String,

        /// Optional physical location metadata
        location: Option<String>,
    },

    /// Stop a running daemon so it checkpoints before exiting
    Shutdown,

    /// Fetch a pinned Laya ONNX model bundle (checksum-verified, supply-chain protected)
    Laya {
        /// Model identifier to fetch (e.g., v1.11.1)
        model: Option<String>,
        /// Target bundle directory (default: artifacts/laya-bundle)
        #[arg(long)]
        out_dir: Option<String>,
    },

    /// Report what an upgrade left inconsistent, changing nothing
    /// Report what an upgrade left inconsistent, changing nothing. Scoped and
    /// labeled: `--namespace <ns>` for one project, omitted = every namespace
    /// with each finding line carrying its own label (guinea-pig BUG-9).
    Doctor {
        /// Limit the report to one namespace (default: all namespaces)
        #[arg(long)]
        namespace: Option<String>,
    },

    /// Run an external plugin or helper, explicitly
    ///
    /// This used to happen implicitly for any unrecognised subcommand, which
    /// meant a typo executed whatever it named. Ask for it by name instead:
    ///   neurostrata-mcp run my-plugin --flag value
    Run {
        /// The program to execute
        program: String,

        /// Arguments passed to it unchanged
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Write a snapshot of the database to a directory (checkpointed file copy)
    Backup {
        /// Directory to write the backup into. Must not already exist
        dir: String,
    },

    /// Report daemon and store state without touching anything
    ///
    /// Exit 0: a healthy daemon is serving. Exit 1: no daemon and the lock is
    /// free -- safe to start one. Exit 2: the lock is held but nothing answers;
    /// a daemon is busy or finishing, so do not start another.
    Status,

    /// Rebuild a database from a backup, into a file that does not exist yet
    Restore {
        /// Directory a backup was written to
        dir: String,

        /// Where to build the restored database. Defaults to the configured
        /// db_path, which must not already exist
        #[arg(long)]
        into: Option<String>,
    },

    /// Edit an existing memory
    Edit {
        /// The target namespace
        namespace: String,

        /// The memory ID to edit
        id: String,

        /// The new namespace to move/save to
        new_namespace: String,

        /// The new content
        content: String,

        /// The new location
        location: String,
    },

    /// Update metadata on an existing memory without changing its ID
    #[command(name = "set-metadata")]
    SetMetadata {
        /// The target namespace
        namespace: String,

        /// The memory ID to update
        id: String,

        /// The new metadata as a JSON object string
        metadata: String,
    },

    /// Normalize declared edge targets to qualified ids where they resolve
    ///
    /// For every non-ingested memory in the namespace, rewrites each target in
    /// `related_to`, `contained_by`, `governs` and `extracted_from` to the
    /// qualified id it resolves to (when resolution is unambiguous). Uses the
    /// existing vector, so no re-embedding. Ingested nodes and code-node ids
    /// are never touched. Run with the daemon stopped.
    #[command(name = "normalize-metadata")]
    NormalizeMetadata {
        /// The exact project name
        namespace: String,
    },

    /// Task subsystem: the gate behind the pre-push hook (section 5)
    Task {
        #[command(subcommand)]
        action: TaskCommands,
    },

    /// Install NeuroStrata as a managed service on this system.
    /// Cross-platform: Linux uses systemd (user-level), macOS uses launchd,
    /// Windows uses NSSM/SCM. Installs the daemon + the backup timer.
    /// Idempotent — safe to re-run.
    Setup {
        /// Path to the NeuroStrata project root (where scripts/backup.sh lives).
        /// Defaults to current directory.
        #[arg(long)]
        repo_path: Option<std::path::PathBuf>,
    },

    /// Manage stable source IDs and their local NeuroStrata marker directories
    #[command(name = "project-context")]
    ProjectContext {
        #[command(subcommand)]
        action: ProjectContextCommands,
    },

    /// Git hook management: installs the one hook that exists
    Hooks {
        #[command(subcommand)]
        action: HooksCommands,
    },

    /// Share what your agents learn with the team's agents (SynapticMesh)
    ///
    /// The mesh is a standalone git repo the daemon owns; the project keeps
    /// only a pointer. Nothing here touches the LadybugDB, so the command
    /// runs without a daemon and without the store lock. `mesh` is the
    /// documented short alias.
    #[command(name = "synaptic-mesh", alias = "mesh")]
    SynapticMesh {
        #[command(subcommand)]
        action: crate::synaptic_mesh::SynapticMeshCommands,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
enum ProjectContextCommands {
    /// Bind a stable source ID to an existing .NeuroStrata marker directory
    Bind {
        source_id: String,
        marker_path: std::path::PathBuf,
    },
    /// List configured source IDs and their marker directories
    List,
    /// Remove a source ID binding
    Unbind { source_id: String },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum TaskCommands {
    /// Run the gate: 0 clean, 1 violations, 2 could-not-check (section 5.1)
    ///
    /// Mirrors the Backup dispatch: ask a live daemon, else open the database
    /// for a metadata-only read, else report the infrastructure failure.
    Gate {
        /// The exact project name, e.g. 'NeuroStrata'
        namespace: String,

        /// With violations an unavailable database blocks the push too
        #[arg(long)]
        strict: bool,

        /// Run the engine against an in-memory fixture of planted violations
        /// (Q4.3 of docs/design-wiring-panel.md) and assert the produced
        /// violation-kind set matches the canonical three. Metadata-only,
        /// bypasses the daemon and the lock; `namespace` is ignored.
        #[arg(long)]
        self_test: bool,
    },

    /// The gate as an advisory report: violations, staleness, counts. Always exits 0
    Validate {
        /// The exact project name
        namespace: String,
    },

    /// One-shot, idempotent beads import: .beads/issues.jsonl -> tasks (section 7)
    Import {
        /// The exact project name
        namespace: String,

        /// Path to the exported JSONL (bd export .beads/beads.jsonl)
        #[arg(long)]
        from_beads: String,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum HooksCommands {
    /// Write the pre-push hook into the directory git runs hooks from
    /// (core.hooksPath honored): the only hook the task system keeps
    Install {
        /// Replace a pre-push hook NeuroStrata did not write
        #[arg(long)]
        force: bool,
    },

    /// Write the pre-commit hook into the directory git runs hooks from
    /// (core.hooksPath honored): stamps session_queries.json and task.graph_queried
    /// when committing files in governed modules. Optional — only run on git repos.
    InstallPreCommit,
}

/// What a health probe actually found.
///
/// Silence is the case worth naming. A daemon busy inside the engine answers
/// nothing for minutes at a time, and reporting that as "no daemon is running"
/// sends people hunting a process that is very much alive -- or worse, killing
/// it and losing every write since the last checkpoint. Silence cannot be
/// resolved from here either: on this machine a connection to the port with
/// nothing behind it hangs instead of being refused, so a timeout genuinely
/// means "one of two things". Say that, rather than pick one.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DaemonProbe {
    Responsive,
    Silent,
    Absent,
}

/// Kept separate from the request so the distinction can be tested without a
/// socket. Only an explicit refusal proves absence.
fn classify_probe(reached: bool, refused: bool) -> DaemonProbe {
    if reached {
        DaemonProbe::Responsive
    } else if refused {
        DaemonProbe::Absent
    } else {
        DaemonProbe::Silent
    }
}

/// Human-readable byte count for the startup summary: whole GiB/MiB
/// render as such, anything else falls back to raw bytes.
fn format_bytes_human(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes % GIB == 0 {
        format!("{} GiB", bytes / GIB)
    } else if bytes % MIB == 0 {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// The startup summary line (Spec Feature 1): the resolved embedder and
/// both store limits, each with the layer that supplied it, so an
/// operator can always answer "why this model, why these limits?".
/// Task 4 extends this same surface with the idle-unload segment rather
/// than growing a second summary mechanism.
fn format_startup_summary(
    resolution: &embed::ResolvedModel,
    profile: Option<config::CapacityProfile>,
    limits: crate::store::StoreLimits,
    sources: &config::StoreLimitSources,
    idle_unload: Option<std::time::Duration>,
) -> String {
    let embedder_source = match resolution.source {
        embed::ModelSource::Profile => match profile {
            Some(profile) => format!("profile={}", profile.name()),
            None => embed::ModelSource::Profile.label().to_string(),
        },
        source => source.label().to_string(),
    };
    format!(
        "embedder: {} (source: {}) \u{b7} store: pool {} (source: {}), max DB {} (source: {}) \u{b7} {}",
        resolution.model.model_name,
        embedder_source,
        format_bytes_human(limits.buffer_pool_bytes),
        sources.pool.label(),
        format_bytes_human(limits.max_db_bytes),
        sources.max_db.label(),
        embed::format_idle_unload_segment(idle_unload),
    )
}

/// The outcome of startup resolution: everything a startup path needs to
/// open its store consistently with what the summary line reported. The
/// resolved model is carried, not discarded: the embedder is built from
/// it (`embed::build_embedder_for`), so the printed provenance describes
/// the constructed embedder by construction and resolution runs exactly
/// once at startup.
struct StartupResolution {
    resolution: embed::ResolvedModel,
    limits: crate::store::StoreLimits,
}

/// Resolve the capacity profile, the embedder, and the store limits once
/// at startup; print the summary line (always, profile or not) and, on a
/// profile/model disagreement, the single warning. This is a
/// profile-consuming surface: an invalid `NEUROSTRATA_PROFILE` fails
/// here, at embedder construction / store opening -- never on commands
/// that do neither (Spec AC6).
fn resolve_startup(config: &Config) -> anyhow::Result<StartupResolution> {
    let profile = crate::config::CapacityProfile::from_env()?;
    let resolution = embed::configured_resolution()?;
    let (limits, sources) = config.store_limits_resolved(profile)?;
    // Read here for the summary segment; embedder construction reads
    // the same env-only setting through the same helper, so the line
    // and the constructed embedder cannot disagree.
    let idle_unload = embed::idle_unload_threshold()?;
    println!(
        "{}",
        format_startup_summary(&resolution, profile, limits, &sources, idle_unload)
    );
    if let Some(disagreement) = embed::profile_disagreement(&resolution, profile) {
        eprintln!("{}", embed::format_disagreement_warning(&disagreement));
    }
    Ok(StartupResolution {
        resolution,
        limits,
    })
}

/// Store limits for a store-opening surface that is not a startup path:
/// the profile is parsed by this caller and passed in, so
/// `Config::store_limits()` itself stays profile-free.
fn profiled_store_limits(config: &Config) -> anyhow::Result<crate::store::StoreLimits> {
    let profile = crate::config::CapacityProfile::from_env()?;
    config.store_limits_with_profile(profile)
}

/// Whether the answering daemon runs the build the binary on disk holds.
enum Skew {
    InSync(String),
    Stale(String),
}

/// Kept separate from the request so the verdict is testable without a
/// socket. A daemon that cannot answer /info predates build verification,
/// so it is strictly older than any binary that asks -- stale by definition.
fn skew_verdict(info: Option<&serde_json::Value>, own: (u64, u64)) -> Skew {
    let info = match info {
        Some(i) => i,
        None => return Skew::Stale(
            "the daemon predates build verification (no /info), so it is older than this binary"
                .to_string(),
        ),
    };
    let hash = info.get("exe_hash").and_then(|v| v.as_u64());
    let len = info.get("exe_len").and_then(|v| v.as_u64());
    let version = info
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if hash == Some(own.0) && len == Some(own.1) {
        Skew::InSync(version.to_string())
    } else {
        Skew::Stale(format!(
            "daemon runs a different build (exe hash {:016x}, this binary {:016x}, started at unix {})",
            hash.unwrap_or(0),
            own.0,
            info.get("started_at").and_then(|v| v.as_i64()).unwrap_or(0)
        ))
    }
}

/// The gate's identity check (Task 1, J9): before the gate trusts a
/// /tasks/gate answer, the answering process's build fingerprint must
/// match the build this binary expects. /health answering proves only
/// that *something* is listening on the fixed port -- a squatter
/// answers it too. So the gate fetches /info and compares the build
/// fingerprint before any gate request is made. On mismatch the
/// refusal names the expected and reported fingerprints, because
/// "an old daemon" and "another process on the port" need different
/// operator actions.
///
/// What the fingerprint proves, and what it does not: the comparison
/// proves build identity only -- the fingerprint is computable from
/// the readable binary; it is not a secret. The /info probe itself
/// carries the bearer (fetch_daemon_info sends it), so the probe MAY
/// expose the bearer to a squatter before the comparison runs. That
/// exposure is accepted because the bearer alone unlocks nothing
/// destructive (DR-5 leak containment).
/// INVARIANT: the operator token is never sent to a daemon whose
/// fingerprint has not been verified; the bearer is expendable by
/// design (DR-5 leak containment).
fn gate_verify_daemon(info: Option<&serde_json::Value>, own: (u64, u64)) -> Result<(), String> {
    match skew_verdict(info, own) {
        Skew::InSync(_) => Ok(()),
        Skew::Stale(detail) => Err(format!(
            "refusing to trust the daemon on 127.0.0.1:34343: this binary's build \
             fingerprint is {:016x}/{}, and the answering process is a different \
             build ({}) -- it may be a stale daemon or another process squatting \
             on the port",
            own.0, own.1, detail
        )),
    }
}

async fn fetch_daemon_info() -> Option<serde_json::Value> {
    let res = daemon_http_client()
        .get("http://127.0.0.1:34343/info")
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    res.json::<serde_json::Value>().await.ok()
}

/// Read the configured db_path, or None if the config cannot be read.
/// Used by the proxy mode to locate the lock file before deciding whether
/// to spawn a competing daemon.
fn config_db_path() -> Option<String> {
    Config::from_default_path()
        .ok()
        .map(|c| c.db_path.to_string_lossy().to_string())
}

/// Where crash breadcrumbs go: a `crash` directory beside the database.
///
/// The database already lives under the user's own config directory, so the
/// breadcrumbs do too -- no privileged path, no setup, and nothing an
/// operator has to remove by hand. When the config cannot be read there is
/// no database path to follow, so the breadcrumbs go to the config
/// directory itself.
fn crash_dir() -> std::path::PathBuf {
    if let Ok(config) = Config::from_default_path() {
        if let Some(parent) = config.db_path.parent().and_then(|p| p.parent()) {
            return parent.join("crash");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config")
        .join("neurostrata")
        .join("crash")
}

fn dispatch_project_context(
    action: &ProjectContextCommands,
    config_path: &std::path::Path,
) -> anyhow::Result<String> {
    let mut config = Config::load_from(config_path)?;
    let (output, changed) = match action {
        ProjectContextCommands::Bind {
            source_id,
            marker_path,
        } => {
            config.bind_project_context(source_id, marker_path)?;
            let canonical = config
                .project_contexts
                .get(source_id)
                .expect("successful bind stores its source ID");
            (
                format!("Bound {source_id} to {}", canonical.display()),
                true,
            )
        }
        ProjectContextCommands::List => {
            let mut entries: Vec<_> = config.project_contexts.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let output = if entries.is_empty() {
                "No project contexts configured.".to_string()
            } else {
                entries
                    .into_iter()
                    .map(|(source_id, path)| format!("{source_id}\t{}", path.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            (output, false)
        }
        ProjectContextCommands::Unbind { source_id } => {
            config.unbind_project_context(source_id)?;
            (format!("Unbound {source_id}"), true)
        }
    };

    if changed {
        config.save_to(config_path)?;
    }
    Ok(output)
}

/// The daemon creates its lock by appending ".daemon.lock" to the db_path
/// (see daemon_lock_path in main.rs). with_extension would produce
/// "db.db.daemon.lock" for a file named "db", which is wrong.
fn lock_path_for(db_path: &str) -> std::path::PathBuf {
    let mut os = std::ffi::OsString::from(db_path);
    os.push(".daemon.lock");
    std::path::PathBuf::from(os)
}

async fn probe_daemon() -> DaemonProbe {
    match daemon_http_client()
        .get("http://127.0.0.1:34343/health")
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
    {
        Ok(_) => classify_probe(true, false),
        Err(e) => classify_probe(false, e.is_connect()),
    }
}

/// The file a daemon holds an exclusive lock on for its whole life.
///
/// The port cannot say when a daemon is finished: axum closes its listener
/// before the final checkpoint runs, so a refused connection arrives while the
/// process still owns the database. The OS releases this lock only when the
/// process exits -- cleanly, killed or crashed -- so a held lock proves the
/// daemon is still there, and a free one proves it is gone.
fn daemon_lock_path(db_path: &std::path::Path) -> std::path::PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push(".daemon.lock");
    std::path::PathBuf::from(path)
}

fn open_daemon_lock(db_path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let path = daemon_lock_path(db_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Held for the life of the daemon process and never dropped, so the lock goes
/// with the process rather than with anything that runs before the database
/// has closed.
static DAEMON_LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

/// What a stopping daemon leaves in its lock file for `shutdown` to read.
const FINAL_CHECKPOINT_OK: &str = "checkpointed";

/// Takes the daemon lock, or refuses: a second daemon would open the database
/// while the first one is writing to it.
fn take_daemon_lock(db_path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let file = open_daemon_lock(db_path)?;
    match file.try_lock() {
        Ok(()) => {
            // Whatever the last daemon reported is not about this one.
            file.set_len(0)?;
            Ok(file)
        }
        Err(std::fs::TryLockError::WouldBlock) => Err(anyhow::anyhow!(
            "Another NeuroStrata daemon is already running against {:?}.",
            db_path
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Whether some process holds the daemon lock right now.
fn daemon_holds_lock(db_path: &std::path::Path) -> bool {
    match open_daemon_lock(db_path) {
        Ok(file) => matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(_) => false,
    }
}

/// A daemon that holds the database but did not answer the probe is busy, not
/// gone, and opening the database from here would contend with its writer. An
/// answering daemon is not busy in this sense: it can be asked to do the work.
fn daemon_busy(probe: DaemonProbe, lock_held: bool) -> bool {
    lock_held && probe != DaemonProbe::Responsive
}

/// The gate's exit code: verdict known -> 0/1; verdict unavailable -> strict
/// blocks (1), otherwise 2 (section 5.1). The hook only blocks on 1, so the
/// non-strict infra path warns and lets the push through.
fn gate_exit_code(ok: Option<bool>, strict: bool) -> i32 {
    match ok {
        Some(true) => 0,
        Some(false) => 1,
        None => {
            if strict {
                1
            } else {
                2
            }
        }
    }
}

/// The gate CLI's half of the SKIP_GATE definition: the escape hatch is
/// honored only for the exact value "1" — "0", "false", "yes", and empty
/// all run the gate. The installed pre-push hook carries the same
/// definition in shell; the two homes must never drift (the Task 4 tests
/// pin both).
fn skip_gate_value_skips(value: Option<&str>) -> bool {
    value == Some("1")
}

/// The enclosing git work tree, or None outside one.
fn git_toplevel() -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

/// Section 5.1, path 2: no daemon to ask, so open the database for the
/// metadata-only read. Dimensions come from config -- loading the embedding
/// model would turn a `git push` into a model download.
async fn direct_report(
    config: &Config,
    namespace: &str,
) -> Result<crate::task::gate::GateReport, String> {
    let dimensions = embed::configured_dimensions().map_err(|e| {
        format!(
            "the configured embedding dimensions could not be read: {}",
            e
        )
    })?;
    let embedder_id = embed::configured_embedder_id()
        .map_err(|e| format!("the configured embedding identity could not be read: {}", e))?;
    let store: Arc<dyn VectorStore> = Arc::new(
        LadybugStore::new_with_identity_limits(
            config.db_path.to_string_lossy().to_string(),
            dimensions,
            embedder_id,
            profiled_store_limits(config)
                .map_err(|e| format!("store limits are invalid: {}", e))?,
        )
        .map_err(|e| format!("the database could not be opened: {}", e))?,
    );
    store
        .init(namespace)
        .await
        .map_err(|e| format!("the schema could not be opened: {}", e))?;
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    crate::task::gate::run(&store, &namespace)
        .await
        .map_err(|e| format!("the gate could not list the tasks: {}", e))
}

/// The CLI names ingested files relative to where it runs, as CLI-readme.md
/// documents: `ingest ./src` from a project yields `src/lib.rs`. An absolute
/// path to a subdirectory of the working directory is the same request written
/// out in full, so it is walked as that relative path. The working directory
/// itself stays absolute, so its root node is named the way the GUI names it.
fn cli_ingest_root(dir: &str) -> String {
    let path = std::path::Path::new(dir);
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rest) = path.strip_prefix(&cwd) {
                if !rest.as_os_str().is_empty() {
                    return rest.to_string_lossy().to_string();
                }
            }
        }
    }
    dir.to_string()
}

const DAEMON_BUSY_MESSAGE: &str = "A NeuroStrata daemon holds the database but did not answer within 500ms, so it is busy rather than gone, and opening the database from here would contend with it. Retry in a moment, or run `neurostrata-mcp shutdown` and let it finish.";

/// What `hooks install` writes, and where: the scaffolded hook text — the
/// same bytes the task-setup payload ships — into the directory git
/// actually runs hooks from (`git_hooks_dir`, which honors
/// `core.hooksPath`). One hook text, one resolution; there is no second
/// copy in this binary. Returns the installed hook's path; `Err` carries
/// the user-facing refusal (a pre-push hook NeuroStrata did not write,
/// and no `--force`).
///
/// Mesh piggyback (spec §"Hooks"), slice 7 decision: the mesh enqueue is
/// deliberately NOT installed into this hook. Hooks are unreliable (spec:
/// not inherited by clones, conflict with hook managers), rewriting the
/// hook would not retrofit already-installed hooks, and the gate stays
/// single-purpose. `neurostrata-mcp mesh sync-enqueue` (hidden CLI,
/// pull-then-push, one line, always exit 0) is the hook TARGET for sites
/// that opt in; manual wiring line, if wanted:
///   printf '\nneurostrata-mcp mesh sync-enqueue >/dev/null 2>&1 || true\n' >> .git/hooks/pre-push
/// Manual push/pull remains first-class either way.
fn install_pre_push_hook(
    root: &std::path::Path,
    force: bool,
) -> anyhow::Result<std::path::PathBuf> {
    let hooks_dir = crate::task::git_hooks_dir(root);
    if !hooks_dir.is_dir() {
        std::fs::create_dir_all(&hooks_dir)?;
    }
    let target = hooks_dir.join("pre-push");
    // Overwriting our own hook needs no flag: --force is for
    // a pre-push somebody else wrote.
    let ours = std::fs::read_to_string(&target)
        .map(|content| crate::task::is_ours_hook(&content))
        .unwrap_or(false);
    if target.exists() && !ours && !force {
        anyhow::bail!(
            "{} already exists and was not written by NeuroStrata.\nRe-run with --force to replace it with the NeuroStrata task gate:\n  neurostrata-mcp hooks install --force",
            target.display()
        );
    }
    // Back up any existing hook before overwriting (best-effort).
    if target.exists() {
        let backup = hooks_dir.join("pre-push.bak");
        let _ = std::fs::copy(&target, &backup);
    }
    std::fs::write(&target, crate::task::SCAFFOLDED_PRE_PUSH_HOOK)?;
    set_hook_executable(&target)?;
    Ok(target)
}

/// Mark a freshly-written hook file as executable. Git on POSIX runs a hook
/// only when the mode bit is set; on Windows it uses the filename
/// (`.git/hooks/<name>` without an extension, executed through Git Bash)
/// rather than POSIX permissions, so the helper is a no-op there. Gating
/// the Unix-only `PermissionsExt` import keeps the Windows build green.
#[cfg(unix)]
fn set_hook_executable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_hook_executable(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Install the pre-commit hook; non-git repos skip this (session ledger is primary mechanism).
fn install_pre_commit_hook(root: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let hooks_dir = crate::task::git_hooks_dir(root);
    if !hooks_dir.is_dir() { std::fs::create_dir_all(&hooks_dir)?; }
    let target = hooks_dir.join("pre-commit");
    std::fs::write(&target, crate::task::SCAFFOLDED_PRE_COMMIT_HOOK)?;
    set_hook_executable(&target)?;
    Ok(target)
}

/// Records how the daemon's final checkpoint went, for `shutdown` to report.
fn record_final_checkpoint(outcome: &anyhow::Result<()>) {
    use std::io::Write;
    if let Some(file) = DAEMON_LOCK.get() {
        let report = match outcome {
            Ok(()) => FINAL_CHECKPOINT_OK.to_string(),
            Err(e) => format!("error: {}", e),
        };
        let _ = file.set_len(0);
        let _ = (&*file).write_all(report.as_bytes());
        let _ = file.sync_all();
    }
}

/// Read-only queries against the store, transparently proxied to the daemon
/// when one is running (guinea-pig BUG-8): `doctor`, `list` and `namespaces`
/// are documented as changing nothing and must work while the system is up.
/// The proxy holds no engine handle -- that is the whole point.
enum ReadSource {
    Direct(Arc<dyn VectorStore>),
    Daemon,
}

impl ReadSource {
    async fn namespaces(&self) -> anyhow::Result<Vec<String>> {
        match self {
            ReadSource::Direct(s) => s.list_namespaces().await,
            ReadSource::Daemon => {
                let res = daemon_http_client()
                    .post("http://127.0.0.1:34343/cli/read")
                    .json(&serde_json::json!({ "op": "namespaces" }))
                    .send()
                    .await?;
                let v: serde_json::Value = res.json().await?;
                Ok(serde_json::from_value(v["namespaces"].clone())?)
            }
        }
    }

    async fn list(&self, namespace: &str) -> anyhow::Result<Vec<SearchResult>> {
        match self {
            ReadSource::Direct(s) => s.list(namespace, None, None).await,
            ReadSource::Daemon => {
                let res = daemon_http_client()
                    .post("http://127.0.0.1:34343/cli/read")
                    .json(&serde_json::json!({ "op": "list", "namespace": namespace }))
                    .send()
                    .await?;
                let v: serde_json::Value = res.json().await?;
                Ok(serde_json::from_value(v["rows"].clone())?)
            }
        }
    }
}

async fn run_doctor(read: &ReadSource, scope: Option<&str>) -> anyhow::Result<()> {
    // Read-only by design: it names what an upgrade left
    // behind and how to fix it, and touches nothing itself.
    // Guinea-pig BUG-9: every result is scoped and labeled.
    // Unscoped output is what produced a confident wrong
    // answer about a different project's graph.
    let mut namespaces = read.namespaces().await?;
    namespaces.sort();
    if let Some(want) = scope {
        if !namespaces.iter().any(|n| n.eq_ignore_ascii_case(want)) {
            anyhow::bail!(
                "no namespace '{}' here; known: {:?}. Refusing to print unscoped results.",
                want,
                namespaces
            );
        }
        namespaces.retain(|n| n.eq_ignore_ascii_case(want));
        println!("Namespace: {:?}\n", namespaces);
    } else {
        println!("Namespaces (all): {:?}\n", namespaces);
    }

    let mut collisions = 0;
    for (i, a) in namespaces.iter().enumerate() {
        for b in namespaces.iter().skip(i + 1) {
            if a.eq_ignore_ascii_case(b) {
                collisions += 1;
                let a_len = read.list(a).await.map(|m| m.len()).unwrap_or(0);
                let b_len = read.list(b).await.map(|m| m.len()).unwrap_or(0);
                println!("Two spellings of one project:");
                println!("  '{}' holds {} memories", a, a_len);
                println!("  '{}' holds {} memories", b, b_len);
                let (from, to) = if a_len < b_len { (a, b) } else { (b, a) };
                println!(
                    "  Merge with: neurostrata-mcp move '{}' <id> '{}'  (one per id)\n",
                    from, to
                );
            }
        }
    }
    if collisions == 0 {
        println!("No namespaces differ only by case.\n");
    }

    for ns in &namespaces {
        let memories = read.list(ns).await?;
        let known = crate::store::ladybug::KnownIds::new(memories.iter().map(|m| m.id.as_str()));

        let mut resolvable = Vec::new();
        let mut missing = Vec::new();
        let mut never_read = 0;
        let mut ingested = 0;
        let mut unqualified = Vec::new();

        for memory in &memories {
            // Ingested ids carry the namespace that owns them.
            // One written before that changed does not, and a
            // bare path is unique to a project rather than to
            // the database -- so two projects holding the same
            // path still collide until each is re-ingested.
            if memory.payload.user_id == "auto-ingestor" {
                ingested += 1;
                if !crate::parser::ingest::is_qualified(ns, &memory.id) {
                    unqualified.push(memory.id.clone());
                }
            }

            if memory
                .payload
                .metadata
                .get("access_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                == 0
            {
                never_read += 1;
            }
            for edge in crate::store::ladybug::edge_specs(&memory.payload.metadata) {
                if known.contains(&edge.target_id) {
                    continue;
                }
                match known.resolve(&edge.target_id) {
                    Some(_) => resolvable.push(edge.target_id.clone()),
                    None => missing.push(edge.target_id.clone()),
                }
            }
        }

        println!("[{}] {} memories", ns, memories.len());
        println!(
            "  [{}] ingested nodes carrying this namespace in their id: {} of {}",
            ns,
            ingested - unqualified.len(),
            ingested
        );
        if !unqualified.is_empty() {
            println!(
                "    {} predate namespace qualification. They resolve, but the next",
                unqualified.len()
            );
            println!("    project ingesting a shared path takes them.");
            for id in unqualified.iter().take(3) {
                println!("      {}", id);
            }
            println!("    Migrate: neurostrata-mcp ingest <dir> {}", ns);
            println!("    Backup first: re-ingest rewrites every id.");
        }
        println!(
            "  [{}] declared targets that need the older absolute form resolved: {}",
            ns,
            resolvable.len()
        );
        for target in resolvable.iter().take(3) {
            println!("    {}", target);
        }
        println!(
            "  [{}] declared targets that match nothing ingested: {}",
            ns,
            missing.len()
        );
        for target in missing.iter().take(3) {
            println!("    {}", target);
        }
        println!(
            "  [{}] memories never counted as read: {} of {}",
            ns,
            never_read,
            memories.len()
        );
        if never_read == memories.len() && !memories.is_empty() {
            println!(
                "    Every one. Before the embedding decode was fixed, each retrieval tried to"
            );
            println!(
                "    write an empty vector and was refused, so the Neural Gain Filter had nothing"
            );
            println!("    to rank by. Counts start rising again from the next search.");
        }
        println!();
    }

    Ok(())
}

async fn run_namespaces(read: &ReadSource) -> anyhow::Result<()> {
    let namespaces = read.namespaces().await?;
    println!("Namespaces:");
    for ns in namespaces {
        println!("  - {}", ns);
    }

    Ok(())
}

async fn run_list(read: &ReadSource, namespace: &str) -> anyhow::Result<()> {
    let results: Vec<SearchResult> = read.list(namespace).await?;
    println!(
        "Found {} memories in namespace '{}':\n",
        results.len(),
        namespace
    );
    for res in results {
        let location_str = if res.payload.location.is_empty() {
            "N/A".to_string()
        } else {
            res.payload.location.clone()
        };
        println!("--- ID: {} ---", res.id);
        println!("Type: {}", res.payload.memory_type);
        println!("Location: {}", location_str);
        println!("Content: {}\n", res.payload.content);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static STRICT_WAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct StrictWalTestEnvironment {
        strict: Option<std::ffi::OsString>,
        allow: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl StrictWalTestEnvironment {
        fn force_strict() -> Self {
            let lock = STRICT_WAL_TEST_LOCK.lock().unwrap();
            let strict = std::env::var_os("NEUROSTRATA_STRICT_WAL");
            let allow = std::env::var_os("NEUROSTRATA_ALLOW_UNVERIFIED_WAL");
            std::env::set_var("NEUROSTRATA_STRICT_WAL", "1");
            std::env::remove_var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL");
            Self {
                strict,
                allow,
                _lock: lock,
            }
        }
    }

    impl Drop for StrictWalTestEnvironment {
        fn drop(&mut self) {
            if let Some(value) = &self.strict {
                std::env::set_var("NEUROSTRATA_STRICT_WAL", value);
            } else {
                std::env::remove_var("NEUROSTRATA_STRICT_WAL");
            }
            if let Some(value) = &self.allow {
                std::env::set_var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL", value);
            } else {
                std::env::remove_var("NEUROSTRATA_ALLOW_UNVERIFIED_WAL");
            }
        }
    }

    // Task 2 (capacity profiles): the startup summary line is the
    // operator's answer to "why this model, why these limits" (Spec AC8).

    #[test]
    fn summary_line_reports_provenance() {
        let resolution = embed::ResolvedModel {
            model: embed::AcceptableEmbedder {
                model_name: "BGESmallENV15Q".to_string(),
                dimensions: 384,
                base_url: None,
                api_key_env: None,
                api_model: None,
                generated_by: None,
                artifact_pin: None,
                model_sha256: None,
            },
            source: embed::ModelSource::Profile,
        };
        let profile = Some(config::CapacityProfile::Low);
        let limits = crate::store::StoreLimits {
            buffer_pool_bytes: config::CapacityProfile::Low.buffer_pool_bytes(),
            max_db_bytes: config::CapacityProfile::Low.max_db_bytes(),
        };
        let sources = config::StoreLimitSources {
            pool: config::StoreLimitSource::Profile,
            max_db: config::StoreLimitSource::Profile,
        };
        let line = format_startup_summary(&resolution, profile, limits, &sources, None);
        assert!(line.contains("BGESmallENV15Q"), "{line}");
        assert!(line.contains("idle unload: off"), "{line}");
        assert!(line.contains("profile=low"), "{line}");
        assert!(line.contains("64 MiB"), "{line}");
        assert!(line.contains("2 GiB"), "{line}");
        assert!(line.contains("source: profile"), "{line}");

        // No profile, no explicit config: the sources name the
        // auto-generated file and the built-in defaults.
        let resolution = embed::ResolvedModel {
            model: embed::AcceptableEmbedder {
                model_name: "NomicEmbedTextV15".to_string(),
                dimensions: 768,
                base_url: None,
                api_key_env: None,
                api_model: None,
                generated_by: None,
                artifact_pin: None,
                model_sha256: None,
            },
            source: embed::ModelSource::AutoGeneratedFile,
        };
        let sources = config::StoreLimitSources {
            pool: config::StoreLimitSource::BuiltInDefault,
            max_db: config::StoreLimitSource::BuiltInDefault,
        };
        let line = format_startup_summary(
            &resolution,
            None,
            crate::store::StoreLimits::default(),
            &sources,
            None,
        );
        assert!(line.contains("file (auto-generated)"), "{line}");
        assert!(line.contains("built-in default"), "{line}");
        assert!(line.contains("idle unload: off"), "{line}");
    }

    /// Task 4 (idle unload): the summary line reports the opt-in idle
    /// unload setting in the same surface as the embedder provenance.
    #[test]
    fn summary_line_reports_idle_unload_threshold() {
        let resolution = embed::ResolvedModel {
            model: embed::AcceptableEmbedder {
                model_name: "BGESmallENV15Q".to_string(),
                dimensions: 384,
                base_url: None,
                api_key_env: None,
                api_model: None,
                generated_by: None,
                artifact_pin: None,
                model_sha256: None,
            },
            source: embed::ModelSource::Profile,
        };
        let limits = crate::store::StoreLimits {
            buffer_pool_bytes: config::CapacityProfile::Low.buffer_pool_bytes(),
            max_db_bytes: config::CapacityProfile::Low.max_db_bytes(),
        };
        let sources = config::StoreLimitSources {
            pool: config::StoreLimitSource::Profile,
            max_db: config::StoreLimitSource::Profile,
        };
        let line = format_startup_summary(
            &resolution,
            Some(config::CapacityProfile::Low),
            limits,
            &sources,
            Some(std::time::Duration::from_secs(300)),
        );
        assert!(
            line.contains("idle unload: on (threshold 300s)"),
            "{line}"
        );
    }

    /// T19: the auto-spawned daemon's stderr goes to daemon.log via the
    /// `daemon_stderr()` seam, while stdin/stdout stay null (Spec §5.2's
    /// contract -- the fix plus both non-defects, in one pin). The pin is
    /// structural, in the file's `include_str!("main.rs")` idiom: the
    /// spawn region runs from `let exe = std::env::current_exe()` through
    /// `.spawn()?`.
    #[test]
    fn daemon_spawn_stdio_contract_is_pinned() {
        let source = include_str!("main.rs");
        // The leading newline + spawn indentation keeps this search from
        // matching this test's own literals.
        let spawn_start = source
            .find("\n            let exe = std::env::current_exe()")
            .expect("the auto-spawn block exists");
        let spawn_end = source[spawn_start..]
            .find(".spawn()?")
            .map(|offset| spawn_start + offset + ".spawn()?".len())
            .expect("the auto-spawn block ends in .spawn()?");
        let spawn = &source[spawn_start..spawn_end];
        assert!(
            spawn.contains("daemon_stderr()"),
            "the spawn must route stderr through daemon_stderr():\n{spawn}"
        );
        assert!(
            spawn.contains(".stdin(std::process::Stdio::null())"),
            "the spawn's stdin must stay null:\n{spawn}"
        );
        assert!(
            spawn.contains(".stdout(std::process::Stdio::null())"),
            "the spawn's stdout must stay null:\n{spawn}"
        );
        assert!(
            !spawn.contains(".stderr(std::process::Stdio::null())"),
            "the spawn's stderr must not be discarded to null:\n{spawn}"
        );
    }

    /// Std-only tempdir for the seam tests (Cargo.toml carries no
    /// dev-dependencies): a pid/test-name-unique subdir of the system
    /// temp dir, removed by the caller afterwards.
    fn daemon_seam_tempdir(test_name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-daemon-seam-{}-{}",
            std::process::id(),
            test_name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the seam test tempdir can be created");
        dir
    }

    /// T19, Gate 0 Home β: the elected log home is the watchdog's
    /// data-home file -- the home resolves to
    /// `<home>/.local/share/neurostrata/daemon.log`, asserted
    /// component by component. The fixture home is absolute on the
    /// host platform: a bare `/home/op` is rooted but not absolute
    /// on Windows (no drive or UNC prefix), and the expected path
    /// is derived by the same joins the resolver performs, so the
    /// elected join is pinned on every platform (PR #27, Codex P2).
    #[test]
    fn daemon_log_path_matches_the_elected_home() {
        #[cfg(windows)]
        let home = std::path::Path::new(r"C:\Users\op");
        #[cfg(not(windows))]
        let home = std::path::Path::new("/home/op");
        let path = daemon_log_path_for(home);
        assert!(path.is_absolute(), "{path:?}");
        let normal_names = |path: &std::path::Path| -> Vec<String> {
            path.components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(name) => {
                        Some(name.to_string_lossy().into_owned())
                    }
                    _ => None,
                })
                .collect()
        };
        let mut expected_names = normal_names(home);
        expected_names.extend(
            [".local", "share", "neurostrata", "daemon.log"]
                .into_iter()
                .map(String::from),
        );
        assert_eq!(normal_names(&path), expected_names);
        assert_eq!(
            path,
            home.join(".local")
                .join("share")
                .join("neurostrata")
                .join("daemon.log")
        );
    }

    /// T19: under a fresh home the elected construction is genuinely
    /// nested with its parents absent; `daemon_stderr_for` creates
    /// them, opens the log, and returns the `Log` variant.
    #[test]
    fn daemon_stderr_opens_a_log_under_a_fresh_home() {
        let home = daemon_seam_tempdir("fresh-home");
        let path = daemon_log_path_for(&home);
        assert!(
            !path.parent().expect("the log path has a parent").exists(),
            "the log's parent directories must start absent"
        );
        let stderr = daemon_stderr_for(&path);
        assert!(
            matches!(stderr, DaemonStderr::Log(_)),
            "a fresh home must yield the Log variant"
        );
        assert!(path.exists(), "the log file must exist: {path:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// T19 (DD-3′): when the log cannot be opened -- here a regular
    /// file sits where a parent directory must be created --
    /// `daemon_stderr_for` returns `Null` and does not panic.
    #[test]
    fn daemon_stderr_falls_back_to_null_when_the_log_cannot_open() {
        let home = daemon_seam_tempdir("blocked-home");
        std::fs::write(home.join(".local"), b"not a directory")
            .expect("the blocking regular file can be placed");
        let path = daemon_log_path_for(&home);
        let stderr = daemon_stderr_for(&path);
        assert!(
            matches!(stderr, DaemonStderr::Null),
            "an unopenable log must yield the Null variant"
        );
        assert!(!path.exists(), "no log file can exist: {path:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Task 4 (idle unload), Spec GT-7: status never loads, reloads, or
    /// touches the embedding session and never resets the idle clock.
    /// The pin is structural, as with the profile pin above: neither
    /// Status arm (dedicated or fallback) may gain a call into the
    /// embedder, the session container, or the idle-unload setting.
    /// Session state is reported, if at all, only from a lock-free
    /// snapshot elsewhere -- never via the session.
    #[test]
    fn status_arms_never_touch_the_session() {
        let source = include_str!("main.rs");
        let dedicated_start = source
            .find("\n            Commands::Status => {")
            .expect("the dedicated Status arm exists");
        let dedicated_end = source[dedicated_start..]
            .find("Commands::Backup")
            .map(|offset| dedicated_start + offset)
            .expect("the Status arm is followed by the Backup arm");
        let fallback_start = source
            .find("\n                    Commands::Status => {")
            .expect("the fallback Status arm exists");
        let fallback_end = source[fallback_start..]
            .find("Commands::Ingest")
            .map(|offset| fallback_start + offset)
            .expect("the fallback Status arm is followed by the Ingest arm");
        for (label, arm) in [
            ("dedicated", &source[dedicated_start..dedicated_end]),
            ("fallback", &source[fallback_start..fallback_end]),
        ] {
            for needle in [
                "session_loaded_snapshot",
                "IdleSession",
                "idle_unload",
                "NEUROSTRATA_EMBEDDER_IDLE_UNLOAD_SECS",
                "build_embedder",
            ] {
                assert!(
                    !arm.contains(needle),
                    "the {label} Status arm must not touch the embedding session ({needle})"
                );
            }
        }
    }

    /// Spec AC6: the CLI Status path succeeds with an invalid
    /// `NEUROSTRATA_PROFILE` set because it never consults the profile.
    /// Status calls `std::process::exit`, so it cannot be exercised
    /// in-process; the pin is structural -- the dedicated Status arm's
    /// source must not reference the profile parser, the profile env var,
    /// or embedder resolution. (A behavioral complement lives in
    /// embed::tests::invalid_profile_scoped_error: the same invalid value
    /// DOES fail the embedder-construction surface.)
    #[test]
    fn status_path_never_consults_the_profile_parser() {
        let source = include_str!("main.rs");
        // The leading newline + arm indentation keeps this search from
        // matching this test's own literals.
        let status_start = source
            .find("\n            Commands::Status => {")
            .expect("the dedicated Status arm exists");
        let status_end = source[status_start..]
            .find("Commands::Backup")
            .map(|offset| status_start + offset)
            .expect("the Status arm is followed by the Backup arm");
        let status_arm = &source[status_start..status_end];
        assert!(
            !status_arm.contains("CapacityProfile"),
            "the Status arm must not parse the capacity profile"
        );
        assert!(
            !status_arm.contains("NEUROSTRATA_PROFILE"),
            "the Status arm must not read NEUROSTRATA_PROFILE"
        );
        assert!(
            !status_arm.contains("configured_resolution"),
            "the Status arm must not resolve the embedder"
        );
        assert!(
            !status_arm.contains("store_limits_with_profile"),
            "the Status arm must not apply the profile to store limits"
        );
    }

    /// Extract one top-level fn's body from this file's source, for the
    /// structural routing pins below.
    fn source_fn_body<'a>(source: &'a str, signature: &str) -> &'a str {
        // Anchored to a line start so a signature named inside another
        // test's string literals is not matched instead of the definition.
        let anchored = format!("\n{signature}");
        let start = source
            .find(&anchored)
            .unwrap_or_else(|| panic!("{signature} must exist"))
            + 1;
        let rest = &source[start + signature.len()..];
        let end = [rest.find("\nfn "), rest.find("\nasync fn ")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        &rest[..end]
    }

    /// Fix round 1 (Finding 1): the gate report and snapshot restore are
    /// store-opening surfaces whose model resolution already goes through
    /// the profile; their store limits must come from the profiled
    /// resolution too, not plain `store_limits()`.
    #[test]
    fn gate_and_restore_store_opens_route_through_profiled_limits() {
        let source = include_str!("main.rs");
        for signature in ["async fn direct_report(", "async fn restore_snapshot_bundle("] {
            let body = source_fn_body(source, signature);
            assert!(
                body.contains("profiled_store_limits(config)"),
                "{signature} must resolve store limits through the profile"
            );
            assert!(
                !body.contains(".store_limits()"),
                "{signature} must not use plain store_limits()"
            );
        }
    }

    /// Fix round 1 (Finding 2): resolution runs once at startup -- the
    /// `StartupResolution` carries the resolved model and both startup
    /// arms build the embedder from that carried model, so the printed
    /// summary describes the constructed embedder by construction.
    #[test]
    fn startup_paths_build_the_embedder_from_the_carried_resolution() {
        let source = include_str!("main.rs");
        assert!(
            source.contains("resolution: embed::ResolvedModel,"),
            "StartupResolution must carry the resolved model"
        );
        // Assembled from fragments so this test's own text is not counted.
        let needle = ["build_embedder_for(&startup", ".resolution.model)"].concat();
        assert_eq!(
            source.matches(&needle).count(),
            2,
            "both startup arms must build from the carried resolution"
        );
    }

    fn release_test_barrier(path: &std::path::Path) {
        let mut release = path.as_os_str().to_os_string();
        release.push(".release");
        std::fs::write(std::path::PathBuf::from(release), "continue").unwrap();
    }

    async fn verify_wal_snapshot_restore(
        snapshot: &std::path::Path,
        target: &std::path::Path,
        root: &std::path::Path,
        stage: &str,
        abandoned_stage: &str,
        legacy_stage: &str,
        lookalike: &str,
        malformed: &str,
    ) {
        use serde_json::json;

        let config: Config = serde_json::from_value(json!({})).unwrap();
        let visible = restore_snapshot_bundle(snapshot, target, &config)
            .await
            .unwrap();
        assert!(visible.contains(&"ordinary".to_string()));
        assert!(visible.contains(&lookalike.to_string()));
        assert!(visible.contains(&malformed.to_string()));
        assert!(!visible.contains(&stage.to_string()));
        assert!(!visible.contains(&abandoned_stage.to_string()));
        assert!(!visible.contains(&legacy_stage.to_string()));
        assert!(
            crate::store::ladybug::ingest_stage_lease_path(target, stage)
                .unwrap()
                .is_file()
        );
        assert!(
            crate::store::ladybug::ingest_stage_lease_path(target, abandoned_stage)
                .unwrap()
                .is_file()
        );
        assert!(
            crate::store::ladybug::ingest_stage_lease_path(target, legacy_stage)
                .unwrap()
                .is_file()
        );

        let dimensions = embed::configured_dimensions().unwrap();
        let restored = LadybugStore::new_with_identity_limits(
            target,
            dimensions,
            embed::configured_embedder_id().unwrap(),
            config.store_limits().unwrap(),
        )
        .unwrap();
        restored.init("restore-recovery-test").await.unwrap();
        for (namespace, id, expected) in [
            (lookalike, "legacy-row", "legacy user-owned lookalike"),
            (malformed, "malformed-row", "malformed prefix user content"),
            ("ordinary", "ordinary-row", "ordinary memory"),
            ("ordinary", "saved-fact", "preserved fact"),
            ("ordinary", "saved-task", "preserved task"),
            ("ordinary", "saved-procedure", "preserved procedure"),
            ("ordinary", "saved-history", "preserved history"),
        ] {
            assert_eq!(
                restored
                    .get(namespace, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .1
                    .content,
                expected
            );
        }
        let graph = restored
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        assert!(graph["links"].as_array().unwrap().iter().any(|link| {
            link["source"] == "ordinary-rule"
                && link["target"] == "ordinary-row"
                && link["type"] == "GOVERNS"
        }));

        let next_stage = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let _next_stage_lease = restored.begin_ingest_stage(&next_stage).await.unwrap();
        assert!(restored.get(stage, "stage-row").await.unwrap().is_none());
        assert!(restored
            .get(abandoned_stage, "abandoned-row")
            .await
            .unwrap()
            .is_none());
        assert!(restored
            .get(legacy_stage, &format!("{legacy_stage}::src/old.rs"))
            .await
            .unwrap()
            .is_none());
        assert!(restored
            .get(lookalike, "legacy-row")
            .await
            .unwrap()
            .is_some());
        assert!(restored
            .get(malformed, "malformed-row")
            .await
            .unwrap()
            .is_some());

        let corrupt_snapshot = root.join("corrupt-snapshot");
        std::fs::create_dir(&corrupt_snapshot).unwrap();
        std::fs::copy(
            snapshot.join("ladybug.store"),
            corrupt_snapshot.join("ladybug.store"),
        )
        .unwrap();
        std::fs::copy(
            snapshot.join("ladybug.store.embedding"),
            corrupt_snapshot.join("ladybug.store.embedding"),
        )
        .unwrap();
        let snapshot_wal = appended_path(&snapshot.join("ladybug.store"), ".wal");
        if snapshot_wal.exists() {
            std::fs::copy(
                snapshot_wal,
                appended_path(&corrupt_snapshot.join("ladybug.store"), ".wal"),
            )
            .unwrap();
        }
        let mut corrupt_manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(snapshot.join("manifest.json")).unwrap())
                .unwrap();
        corrupt_manifest["stage_generations"] = json!(["../escape"]);
        std::fs::write(
            corrupt_snapshot.join("manifest.json"),
            serde_json::to_vec(&corrupt_manifest).unwrap(),
        )
        .unwrap();
        let rejected_target = root.join("rejected.store");
        assert!(
            restore_snapshot_bundle(&corrupt_snapshot, &rejected_target, &config)
                .await
                .is_err()
        );
        assert!(!rejected_target.exists());
        assert!(!crate::store::ladybug::restore_in_progress_path(&rejected_target).exists());

        let stale_sidecar_target = root.join("stale-sidecar.store");
        std::fs::write(appended_path(&stale_sidecar_target, ".wal"), b"stale").unwrap();
        assert!(
            restore_snapshot_bundle(snapshot, &stale_sidecar_target, &config)
                .await
                .is_err()
        );
        assert!(!stale_sidecar_target.exists());
        std::fs::remove_file(appended_path(&stale_sidecar_target, ".wal")).unwrap();
        drop(restored);
    }

    #[tokio::test]
    async fn restore_holds_target_open_lock_until_database_and_provenance_are_installed() {
        use crate::traits::MemoryPayload;
        use serde_json::json;

        let root = std::env::temp_dir().join(format!("restore-open-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source_db = root.join("source.store");
        let dimensions = embed::configured_dimensions().unwrap();
        let embedder_id = embed::configured_embedder_id().unwrap();
        let test_config: Config = serde_json::from_value(json!({})).unwrap();
        let limits = test_config.store_limits().unwrap();
        let source = LadybugStore::new_with_identity_limits(
            &source_db,
            dimensions,
            embedder_id.clone(),
            limits,
        )
        .unwrap();
        source.init("restore-lock-source").await.unwrap();
        source
            .upsert(
                "preserved",
                "exact-row",
                vec![0.125; dimensions],
                MemoryPayload {
                    content: "complete restored content".into(),
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
        let snapshot = root.join("snapshot");
        source
            .export_database(&snapshot.to_string_lossy())
            .await
            .unwrap();
        drop(source);

        let target = root.join("restored.store");
        let barrier = root.join("restore-lock-entered");
        set_test_restore_target_lock_barrier(target.clone(), barrier.clone());
        let snapshot_for_thread = snapshot.clone();
        let target_for_thread = target.clone();
        let restore = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(restore_snapshot_bundle(
                    &snapshot_for_thread,
                    &target_for_thread,
                    &serde_json::from_value(json!({})).unwrap(),
                ))
        });
        let started = std::time::Instant::now();
        while !barrier.exists() {
            assert!(started.elapsed() < std::time::Duration::from_secs(15));
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let open_target = target.clone();
        let open_identity = embedder_id.clone();
        let opener = tokio::task::spawn_blocking(move || {
            LadybugStore::new_with_identity_limits(open_target, dimensions, open_identity, limits)
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !opener.is_finished(),
            "constructor must wait while restore owns the destination writer lock"
        );
        release_test_barrier(&barrier);
        let visible = restore.join().unwrap().unwrap();
        assert!(visible.contains(&"preserved".to_string()));
        let opened = opener.await.unwrap().unwrap();
        assert_eq!(
            opened
                .get("preserved", "exact-row")
                .await
                .unwrap()
                .unwrap()
                .1
                .content,
            "complete restored content"
        );
        drop(opened);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_mistyped_subcommand_is_an_error_not_an_execution() {
        let parsed = Cli::try_parse_from(["neurostrata-mcp", "lsit"]);
        assert!(
            parsed.is_err(),
            "a typo must never reach std::process::Command"
        );
    }

    #[tokio::test]
    async fn snapshot_restore_preserves_stage_provenance_and_legacy_lookalikes() {
        use crate::traits::MemoryPayload;
        use serde_json::json;
        let _strict_wal = StrictWalTestEnvironment::force_strict();

        let root = std::env::temp_dir().join(format!("snapshot-restore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.store");
        let snapshot = root.join("snapshot");
        let target = root.join("restored.store");
        let dimensions = embed::configured_dimensions().unwrap();
        let source_store = LadybugStore::new_with_identity_limits(
            &source,
            dimensions,
            embed::configured_embedder_id().unwrap(),
            crate::store::StoreLimits::default(),
        )
        .unwrap();
        source_store.init("snapshot-test").await.unwrap();

        let stage = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let _active_stage_lease = source_store.begin_ingest_stage(&stage).await.unwrap();
        let abandoned_stage = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let abandoned_lease = source_store
            .begin_ingest_stage(&abandoned_stage)
            .await
            .unwrap();
        let lookalike = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let malformed = format!("{}not-a-uuid", crate::traits::INTERNAL_INGEST_STAGE_PREFIX);
        let legacy_stage = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let ordinary = "ordinary".to_string();
        for (namespace, id, content, user_id) in [
            (
                &stage,
                "stage-row",
                "active staged content",
                "auto-ingestor",
            ),
            (
                &abandoned_stage,
                "abandoned-row",
                "abandoned stage content",
                "auto-ingestor",
            ),
            (&ordinary, "ordinary-row", "ordinary memory", "owner"),
        ] {
            source_store
                .upsert(
                    namespace,
                    id,
                    vec![0.25; dimensions],
                    MemoryPayload {
                        content: content.into(),
                        user_id: user_id.into(),
                        memory_type: "note".into(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({}),
                    },
                )
                .await
                .unwrap();
        }
        for (id, memory_type, content) in [
            ("saved-fact", "fact", "preserved fact"),
            ("saved-task", "task", "preserved task"),
            ("saved-procedure", "procedure", "preserved procedure"),
            ("saved-history", "history", "preserved history"),
        ] {
            source_store
                .upsert(
                    "ordinary",
                    id,
                    vec![0.25; dimensions],
                    MemoryPayload {
                        content: content.into(),
                        user_id: "owner".into(),
                        memory_type: memory_type.into(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: json!({}),
                    },
                )
                .await
                .unwrap();
        }
        source_store
            .upsert(
                "ordinary",
                "ordinary-rule",
                vec![0.25; dimensions],
                MemoryPayload {
                    content: "ordinary rule".into(),
                    user_id: "owner".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"governs": ["ordinary-row"]}),
                },
            )
            .await
            .unwrap();
        for (namespace, id, content) in [
            (&lookalike, "legacy-row", "legacy user-owned lookalike"),
            (&malformed, "malformed-row", "malformed prefix user content"),
        ] {
            source_store
                .seed_legacy_memory_for_test(
                    namespace,
                    id,
                    vec![0.5; dimensions],
                    MemoryPayload {
                        content: content.into(),
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
        }
        let legacy_lease_path =
            crate::store::ladybug::ingest_stage_lease_path(&source, &legacy_stage).unwrap();
        let legacy_lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&legacy_lease_path)
            .unwrap();
        legacy_lease.try_lock().unwrap();
        source_store
            .seed_legacy_memory_for_test(
                &legacy_stage,
                &format!("{legacy_stage}::src/old.rs"),
                vec![0.375; dimensions],
                MemoryPayload {
                    content: "pre-catalog active staged file".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "src/old.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        drop(source_store);
        let source_store = LadybugStore::new_with_identity_limits(
            &source,
            dimensions,
            embed::configured_embedder_id().unwrap(),
            crate::store::StoreLimits::default(),
        )
        .unwrap();
        assert!(!source_store
            .list_namespaces()
            .await
            .unwrap()
            .contains(&legacy_stage));
        let reopened_graph = source_store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        assert!(!reopened_graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["namespace"] == legacy_stage));
        assert_eq!(
            source_store
                .get(&legacy_stage, &format!("{legacy_stage}::src/old.rs"))
                .await
                .unwrap()
                .unwrap()
                .1
                .content,
            "pre-catalog active staged file",
            "visibility filtering must not delete legacy data"
        );
        let recovery_trigger = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let recovery_lease = source_store
            .begin_ingest_stage(&recovery_trigger)
            .await
            .unwrap();
        assert!(!source_store
            .ingest_stage_namespaces()
            .await
            .unwrap()
            .contains(&legacy_stage));
        assert!(!source_store
            .list_namespaces()
            .await
            .unwrap()
            .contains(&legacy_stage));
        let graph_before_snapshot = source_store
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        assert!(!graph_before_snapshot["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["namespace"] == legacy_stage));
        drop(recovery_lease);
        let active_legacy_snapshot_error = source_store
            .export_database(&snapshot.to_string_lossy())
            .await
            .expect_err("snapshot must refuse an active pre-catalog stage")
            .to_string();
        assert!(active_legacy_snapshot_error.contains("legacy ingest generation"));
        assert!(!snapshot.exists());
        assert_eq!(
            source_store
                .get(&legacy_stage, &format!("{legacy_stage}::src/old.rs"))
                .await
                .unwrap()
                .unwrap()
                .1
                .content,
            "pre-catalog active staged file"
        );
        drop(legacy_lease);
        drop(abandoned_lease);
        // Copy the open engine's database and active WAL outside the snapshot
        // API. This is deliberately an unverified, potentially inconsistent
        // crash-state pair, not a supported snapshot. Strict restore must fail
        // closed and leave both source and destination state unchanged.
        std::fs::create_dir(&snapshot).unwrap();
        let snapshot_db = snapshot.join("ladybug.store");
        std::fs::copy(&source, &snapshot_db).unwrap();
        let source_wal = appended_path(&source, ".wal");
        let snapshot_wal = appended_path(&snapshot_db, ".wal");
        assert!(
            source_wal.is_file() && std::fs::metadata(&source_wal).unwrap().len() > 0,
            "restore regression requires a nonempty source WAL"
        );
        let source_wal_copy = std::fs::read(&source_wal).unwrap();
        std::fs::copy(&source_wal, &snapshot_wal).unwrap();
        let mut corrupted_wal = std::fs::read(&snapshot_wal).unwrap();
        assert!(
            corrupted_wal.len() > 16,
            "WAL fixture must have payload and framing"
        );
        let corrupt_offset = corrupted_wal.len() / 2;
        corrupted_wal[corrupt_offset] ^= 0x80;
        std::fs::write(&snapshot_wal, &corrupted_wal).unwrap();
        assert_ne!(std::fs::read(&snapshot_wal).unwrap(), source_wal_copy);
        std::fs::copy(
            crate::store::ladybug::width_marker_path(&source),
            crate::store::ladybug::width_marker_path(&snapshot_db),
        )
        .unwrap();
        std::fs::write(
            snapshot.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "format": "neurostrata-snapshot",
                "version": 2,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "bytes": std::fs::metadata(&snapshot_db).unwrap().len(),
                "stage_generations": [stage, abandoned_stage],
            }))
            .unwrap(),
        )
        .unwrap();
        let source_db_before = std::fs::read(&source).unwrap();
        let source_wal_before = std::fs::read(&source_wal).unwrap();
        let source_marker_path = crate::store::ladybug::width_marker_path(&source);
        let source_marker_before = std::fs::read(&source_marker_path).unwrap();
        let active_lease_path =
            crate::store::ladybug::ingest_stage_lease_path(&source, &stage).unwrap();
        let abandoned_lease_path =
            crate::store::ladybug::ingest_stage_lease_path(&source, &abandoned_stage).unwrap();
        let legacy_lease_before = std::fs::read(&legacy_lease_path).unwrap();
        let active_lease_before = std::fs::read(&active_lease_path).unwrap();
        let abandoned_lease_before = std::fs::read(&abandoned_lease_path).unwrap();
        let rejected_wal_target = root.join("rejected-wal.store");
        let config: Config = serde_json::from_value(json!({})).unwrap();
        let rejected_wal_error = restore_snapshot_bundle(&snapshot, &rejected_wal_target, &config)
            .await
            .expect_err("the engine's checksum-rejected nonempty WAL must fail closed")
            .to_string();
        assert!(
            rejected_wal_error.contains("write-ahead log did not verify"),
            "expected strict checksum rejection, got: {rejected_wal_error}"
        );
        assert!(!rejected_wal_target.exists());
        assert!(!appended_path(&rejected_wal_target, ".wal").exists());
        assert!(!crate::store::ladybug::width_marker_path(&rejected_wal_target).exists());
        assert!(!crate::store::ladybug::restore_in_progress_path(&rejected_wal_target).exists());
        assert_eq!(std::fs::read(&source).unwrap(), source_db_before);
        assert_eq!(std::fs::read(&source_wal).unwrap(), source_wal_before);
        assert_eq!(
            std::fs::read(&source_marker_path).unwrap(),
            source_marker_before
        );
        assert_eq!(
            std::fs::read(&active_lease_path).unwrap(),
            active_lease_before
        );
        assert_eq!(
            std::fs::read(&abandoned_lease_path).unwrap(),
            abandoned_lease_before
        );
        assert_eq!(
            std::fs::read(&legacy_lease_path).unwrap(),
            legacy_lease_before
        );

        let api_snapshot = root.join("api-snapshot");
        source_store
            .export_database(&api_snapshot.to_string_lossy())
            .await
            .unwrap();
        verify_wal_snapshot_restore(
            &api_snapshot,
            &target,
            &root,
            &stage,
            &abandoned_stage,
            &legacy_stage,
            &lookalike,
            &malformed,
        )
        .await;
        drop(_active_stage_lease);
        drop(source_store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn strict_restore_rejects_or_exactly_replays_unverified_wal_capture() {
        use crate::traits::MemoryPayload;
        use serde_json::json;

        let _strict_wal = StrictWalTestEnvironment::force_strict();
        let root = std::env::temp_dir().join(format!("live-wal-capture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.store");
        let snapshot = root.join("snapshot");
        let target = root.join("restored.store");
        let config: Config = serde_json::from_value(json!({})).unwrap();
        let dimensions = embed::configured_dimensions().unwrap();
        let limits = config.store_limits().unwrap();
        let source_store = LadybugStore::new_with_identity_limits(
            &source,
            dimensions,
            embed::configured_embedder_id().unwrap(),
            limits,
        )
        .unwrap();
        source_store.init("live-wal-source").await.unwrap();

        let fact_id = "wal-data::stable-fact";
        source_store
            .upsert(
                "wal-data",
                fact_id,
                vec![0.125; dimensions],
                MemoryPayload {
                    content: "fact committed before unclean snapshot".into(),
                    user_id: "owner".into(),
                    memory_type: "fact".into(),
                    agent_name: None,
                    location: "facts/stable.json".into(),
                    location_lines: "1".into(),
                    metadata: json!({"stable": true}),
                },
            )
            .await
            .unwrap();
        let rule_id = "wal-data::rule";
        source_store
            .upsert(
                "wal-data",
                rule_id,
                vec![0.25; dimensions],
                MemoryPayload {
                    content: "rule declaration".into(),
                    user_id: "owner".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({"governs": [fact_id]}),
                },
            )
            .await
            .unwrap();
        source_store.relink_edges("wal-data", "wal-source").await.unwrap();

        let stage = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let _stage_lease = source_store.begin_ingest_stage(&stage).await.unwrap();
        let stage_row_id = format!("{stage}::src/pending.rs");
        source_store
            .upsert(
                &stage,
                &stage_row_id,
                vec![0.5; dimensions],
                MemoryPayload {
                    content: "staged row from unverified WAL capture".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: None,
                    location: "src/pending.rs".into(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();

        std::fs::create_dir(&snapshot).unwrap();
        let snapshot_db = snapshot.join("ladybug.store");
        std::fs::copy(&source, &snapshot_db).unwrap();
        let source_wal = appended_path(&source, ".wal");
        let snapshot_wal = appended_path(&snapshot_db, ".wal");
        assert!(source_wal.is_file());
        assert!(
            std::fs::metadata(&source_wal).unwrap().len() > 0,
            "live WAL capture must contain uncheckpointed writes"
        );
        std::fs::copy(&source_wal, &snapshot_wal).unwrap();
        let source_marker = crate::store::ladybug::width_marker_path(&source);
        let snapshot_marker = crate::store::ladybug::width_marker_path(&snapshot_db);
        std::fs::copy(&source_marker, &snapshot_marker).unwrap();
        std::fs::write(
            snapshot.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "format": "neurostrata-snapshot",
                "version": 2,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "bytes": std::fs::metadata(&snapshot_db).unwrap().len(),
                "stage_generations": [stage],
            }))
            .unwrap(),
        )
        .unwrap();

        let source_db_before = std::fs::read(&source).unwrap();
        let source_wal_before = std::fs::read(&source_wal).unwrap();
        let source_marker_before = std::fs::read(&source_marker).unwrap();
        let source_lease = crate::store::ladybug::ingest_stage_lease_path(&source, &stage).unwrap();
        let source_lease_before = std::fs::read(&source_lease).unwrap();
        let visible = match restore_snapshot_bundle(&snapshot, &target, &config).await {
            Ok(visible) => visible,
            Err(error) => {
                assert!(
                    error.to_string().contains("write-ahead log did not verify"),
                    "strict mode may reject an unverified active-copy WAL, got: {error:#}"
                );
                assert!(
                    !target.exists(),
                    "failed validation must not install a database"
                );
                assert!(!appended_path(&target, ".wal").exists());
                assert!(!crate::store::ladybug::width_marker_path(&target).exists());
                assert!(!crate::store::ladybug::restore_in_progress_path(&target).exists());
                assert_eq!(std::fs::read(&source).unwrap(), source_db_before);
                assert_eq!(std::fs::read(&source_wal).unwrap(), source_wal_before);
                assert_eq!(std::fs::read(&source_marker).unwrap(), source_marker_before);
                assert_eq!(std::fs::read(&source_lease).unwrap(), source_lease_before);
                drop(source_store);
                let _ = std::fs::remove_dir_all(root);
                return;
            }
        };
        assert_eq!(visible, vec!["wal-data".to_string()]);

        let restored = LadybugStore::new_with_identity_limits(
            &target,
            dimensions,
            embed::configured_embedder_id().unwrap(),
            limits,
        )
        .unwrap();
        restored.init("live-wal-restored").await.unwrap();
        let (fact_vector, fact) = restored.get("wal-data", fact_id).await.unwrap().unwrap();
        assert_eq!(fact_vector, vec![0.125; dimensions]);
        assert_eq!(fact.content, "fact committed before unclean snapshot");
        assert_eq!(fact.memory_type, "fact");
        assert_eq!(fact.metadata, json!({"stable": true}));
        let (rule_vector, rule) = restored.get("wal-data", rule_id).await.unwrap().unwrap();
        assert_eq!(rule_vector, vec![0.25; dimensions]);
        assert_eq!(rule.content, "rule declaration");
        assert_eq!(rule.metadata, json!({"governs": [fact_id]}));
        let graph = restored
            .export_graph(false, false, &std::collections::HashMap::new(), None)
            .await
            .unwrap();
        assert!(graph["links"].as_array().unwrap().iter().any(|edge| {
            edge["source"] == rule_id && edge["target"] == fact_id && edge["type"] == "GOVERNS"
        }));
        let (stage_vector, stage_payload) = restored
            .get(&stage, &stage_row_id)
            .await
            .unwrap()
            .expect("accepted WAL replay retains the staged file row");
        assert_eq!(stage_vector, vec![0.5; dimensions]);
        assert_eq!(
            stage_payload.content,
            "staged row from unverified WAL capture"
        );
        assert_eq!(stage_payload.user_id, "auto-ingestor");
        assert_eq!(stage_payload.memory_type, "file");
        assert_eq!(stage_payload.location, "src/pending.rs");
        assert_eq!(stage_payload.metadata, json!({}));
        assert!(!restored.list_namespaces().await.unwrap().contains(&stage));

        let next_stage = format!(
            "{}{}",
            crate::traits::INTERNAL_INGEST_STAGE_PREFIX,
            uuid::Uuid::new_v4()
        );
        let _next_lease = restored.begin_ingest_stage(&next_stage).await.unwrap();
        assert!(restored.get(&stage, &stage_row_id).await.unwrap().is_none());
        assert_eq!(
            restored
                .get("wal-data", fact_id)
                .await
                .unwrap()
                .unwrap()
                .1
                .content,
            "fact committed before unclean snapshot"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            source_db_before,
            "strict WAL handling must not mutate the source database"
        );
        assert_eq!(std::fs::read(&source_wal).unwrap(), source_wal_before);
        assert_eq!(std::fs::read(&source_marker).unwrap(), source_marker_before);
        assert_eq!(std::fs::read(&source_lease).unwrap(), source_lease_before);
        drop(restored);
        drop(source_store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn source_id_option_parses_with_existing_export_flags() {
        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "export-graph",
            "./graph.json",
            "--source-id",
            "source-a",
            "--exclude-superseded",
            "--include-archived",
        ])
        .expect("export-graph source selection parses");

        assert!(matches!(
            cli.command,
            Some(Commands::ExportGraph {
                out_path: Some(path),
                source_id: Some(source_id),
                exclude_superseded: true,
                include_archived: true,
            }) if path == "./graph.json" && source_id == "source-a"
        ));
    }

    #[test]
    fn project_context_bind_parses_source_id_and_path() {
        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "project-context",
            "bind",
            "source-a",
            "/tmp/project/.NeuroStrata",
        ])
        .expect("project-context bind parses");

        assert!(matches!(
            cli.command,
            Some(Commands::ProjectContext {
                action: ProjectContextCommands::Bind {
                    source_id,
                    marker_path
                }
            }) if source_id == "source-a"
                && marker_path == std::path::Path::new("/tmp/project/.NeuroStrata")
        ));
    }

    #[test]
    fn project_context_list_parses_without_arguments() {
        let cli = Cli::try_parse_from(["neurostrata-mcp", "project-context", "list"])
            .expect("project-context list parses");

        assert!(matches!(
            cli.command,
            Some(Commands::ProjectContext {
                action: ProjectContextCommands::List
            })
        ));
    }

    #[test]
    fn project_context_unbind_parses_source_id() {
        let cli = Cli::try_parse_from(["neurostrata-mcp", "project-context", "unbind", "source-a"])
            .expect("project-context unbind parses");

        assert!(matches!(
            cli.command,
            Some(Commands::ProjectContext {
                action: ProjectContextCommands::Unbind { source_id }
            }) if source_id == "source-a"
        ));
    }

    #[test]
    fn project_context_dispatch_persists_bind_lists_and_rejects_unknown_unbind() {
        let root = std::env::temp_dir().join(format!(
            "ns-project-context-dispatch-{}",
            uuid::Uuid::new_v4()
        ));
        let marker = root.join("project/.NeuroStrata");
        std::fs::create_dir_all(&marker).unwrap();
        let config_path = root.join("config.json");
        let db_path = root.join("db/ladybug.db");
        std::fs::write(
            &config_path,
            serde_json::json!({"db_path": db_path}).to_string(),
        )
        .unwrap();

        let bound = dispatch_project_context(
            &ProjectContextCommands::Bind {
                source_id: "source-a".to_string(),
                marker_path: marker.clone(),
            },
            &config_path,
        )
        .unwrap();
        let canonical = std::fs::canonicalize(&marker).unwrap();
        assert_eq!(bound, format!("Bound source-a to {}", canonical.display()));
        assert_eq!(
            Config::load_from(&config_path)
                .unwrap()
                .project_contexts
                .get("source-a"),
            Some(&canonical)
        );
        assert!(
            !db_path.exists(),
            "config dispatch never creates the database"
        );

        assert_eq!(
            dispatch_project_context(&ProjectContextCommands::List, &config_path).unwrap(),
            format!("source-a\t{}", canonical.display())
        );

        let original = std::fs::read(&config_path).unwrap();
        let error = dispatch_project_context(
            &ProjectContextCommands::Unbind {
                source_id: "unknown".to_string(),
            },
            &config_path,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("Unknown project context source ID"));
        assert_eq!(std::fs::read(&config_path).unwrap(), original);

        assert_eq!(
            dispatch_project_context(
                &ProjectContextCommands::Unbind {
                    source_id: "source-a".to_string(),
                },
                &config_path,
            )
            .unwrap(),
            "Unbound source-a"
        );
        assert!(Config::load_from(&config_path)
            .unwrap()
            .project_contexts
            .is_empty());
        assert!(!db_path.exists());
    }

    /// `set_hook_executable` is the only place the binary uses
    /// `std::os::unix::fs::PermissionsExt` -- the Windows build gates that
    /// branch out entirely, but the helper must compile and run on every
    /// target so a single call site replaces the inline block. On Unix the
    /// hook gets the executable bit; on Windows, git runs the hook via the
    /// filename (`.git/hooks/<name>` without an extension, executed through
    /// Git Bash) rather than POSIX permissions, so the helper is a no-op.
    #[test]
    fn set_hook_executable_marks_the_file_executable_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "ns-set-hook-exec-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pre-push");
        std::fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
        // Pre-condition: the file is not executable yet (umask-dependent, but
        // explicitly clear the bit so the assertion below is exact rather than
        // umask-dependent).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        set_hook_executable(&path).expect("the chmod lands");
        let perms = std::fs::metadata(&path).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o111,
            0o111,
            "the executable bits are set for owner+group+other: {:o}",
            perms.mode()
        );
    }

    #[test]
    fn running_a_plugin_has_to_be_asked_for_by_name() {
        let cli = Cli::try_parse_from(["neurostrata-mcp", "run", "my-plugin", "--flag", "value"])
            .expect("run takes a program and passes its arguments through");

        match cli.command {
            Some(Commands::Run { program, args }) => {
                assert_eq!(program, "my-plugin");
                assert_eq!(args, vec!["--flag".to_string(), "value".to_string()]);
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn a_held_daemon_lock_is_seen_and_goes_with_its_holder() {
        let db = std::env::temp_dir()
            .join(format!("ns-daemon-lock-{}", uuid::Uuid::new_v4()))
            .join("ladybug.db");
        assert!(!daemon_holds_lock(&db), "nothing holds a fresh lock");

        let held = take_daemon_lock(&db).expect("the first daemon takes the lock");
        assert!(
            daemon_holds_lock(&db),
            "a live holder is visible to shutdown"
        );
        assert!(take_daemon_lock(&db).is_err(), "a second daemon is refused");

        drop(held);
        assert!(
            !daemon_holds_lock(&db),
            "the lock is free once its holder is gone"
        );
    }

    #[test]
    fn only_a_refusal_proves_no_daemon() {
        assert_eq!(classify_probe(false, true), DaemonProbe::Absent);
    }

    #[test]
    fn silence_is_not_reported_as_an_absent_daemon() {
        assert_eq!(classify_probe(false, false), DaemonProbe::Silent);
    }

    #[test]
    fn an_answered_probe_is_a_live_daemon() {
        assert_eq!(classify_probe(true, false), DaemonProbe::Responsive);
    }

    #[test]
    fn an_absolute_cli_path_under_the_working_directory_is_walked_relative() {
        let cwd = std::env::current_dir().expect("working directory");
        assert_eq!(cli_ingest_root(&cwd.join("src").to_string_lossy()), "src");
        assert_eq!(
            cli_ingest_root(&cwd.to_string_lossy()),
            cwd.to_string_lossy(),
            "the working directory itself stays absolute"
        );
        assert_eq!(cli_ingest_root("./src"), "./src");
    }

    #[test]
    fn a_daemon_that_holds_the_lock_but_is_silent_is_busy_not_gone() {
        assert!(daemon_busy(DaemonProbe::Silent, true));
        assert!(
            daemon_busy(DaemonProbe::Absent, true),
            "listener closed, still checkpointing"
        );
        assert!(
            !daemon_busy(DaemonProbe::Silent, false),
            "silence with no holder is no daemon"
        );
        assert!(
            !daemon_busy(DaemonProbe::Responsive, true),
            "an answering daemon is asked, not refused"
        );
    }

    /// Section 5.1: a verdict maps to 0/1; no verdict blocks only under
    /// --strict and passes (2) otherwise -- and 2 is what the hook tolerates.
    #[test]
    fn the_gate_exit_code_flips_only_under_strict() {
        assert_eq!(gate_exit_code(Some(true), false), 0);
        assert_eq!(gate_exit_code(Some(true), true), 0);
        assert_eq!(gate_exit_code(Some(false), false), 1);
        assert_eq!(
            gate_exit_code(Some(false), true),
            1,
            "a verdict never changes with the flag"
        );
        assert_eq!(
            gate_exit_code(None, false),
            2,
            "infra warns and lets the push through"
        );
        assert_eq!(
            gate_exit_code(None, true),
            1,
            "strict maps the infra failure to the one code the hook blocks on"
        );
    }

    // ── Task 4: one hook installer, honest SKIP_GATE ─────────────────────
    //
    // The five tests below pin the unified gate/hook enforcement chain:
    // `hooks install` writes the scaffolded hook text (the one the
    // task-setup payload ships) into the directory git actually runs
    // hooks from, SKIP_GATE skips only for exactly "1" in both of its
    // homes, and a missing gate binary is announced, never silent.

    /// Run git in `root`, isolated from the developer's own global/system
    /// config so a personal `core.hooksPath` cannot change what is
    /// asserted. Returns the command's output.
    #[cfg(unix)]
    fn t4_git(root: &std::path::Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs")
    }

    #[cfg(unix)]
    fn t4_git_ok(root: &std::path::Path, args: &[&str]) {
        let out = t4_git(root, args);
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A fresh git repo in a temp dir, with a commit-ready identity.
    #[cfg(unix)]
    fn t4_repo(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("ns-t4-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        t4_git_ok(&root, &["init", "-q", "-b", "main"]);
        t4_git_ok(&root, &["config", "user.email", "t4@example.invalid"]);
        t4_git_ok(&root, &["config", "user.name", "Task 4 Test"]);
        root
    }

    /// `t4_repo` plus a bare remote and one seed commit, ready to push.
    #[cfg(unix)]
    fn t4_repo_with_remote(tag: &str) -> std::path::PathBuf {
        let root = t4_repo(tag);
        let remote = root.join("remote.git");
        t4_git_ok(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
        t4_git_ok(&root, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(root.join("file.txt"), "task 4\n").unwrap();
        t4_git_ok(&root, &["add", "file.txt"]);
        t4_git_ok(&root, &["commit", "-q", "-m", "task 4 seed"]);
        root
    }

    /// A stub `neurostrata-mcp` whose exit code is STUB_GATE_EXIT; every
    /// invocation appends a line to the marker file, so a test can prove
    /// the hook actually ran the gate.
    #[cfg(unix)]
    fn t4_stub_bin(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("stub-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let marker = root.join("gate-ran.marker");
        let stub = format!(
            "#!/bin/sh\necho ran >> \"{}\"\nexit ${{STUB_GATE_EXIT:-0}}\n",
            marker.display()
        );
        let stub_path = bin.join("neurostrata-mcp");
        std::fs::write(&stub_path, stub).unwrap();
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();
        (bin, marker)
    }

    /// Push `main` to origin with `path` as the push process's PATH.
    #[cfg(unix)]
    fn t4_push(
        root: &std::path::Path,
        path: &str,
        stub_gate_exit: Option<&str>,
    ) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["push", "origin", "main"])
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("PATH", path);
        if let Some(code) = stub_gate_exit {
            cmd.env("STUB_GATE_EXIT", code);
        }
        cmd.output().expect("git push runs")
    }

    /// `t4_push` with `NEUROSTRATA_SKIP_GATE` set on the push process
    /// (the hook inherits it), or explicitly removed when `None`.
    #[cfg(unix)]
    fn t4_push_with_skip(
        root: &std::path::Path,
        path: &str,
        stub_gate_exit: Option<&str>,
        skip_gate: Option<&str>,
    ) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["push", "origin", "main"])
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("PATH", path);
        if let Some(code) = stub_gate_exit {
            cmd.env("STUB_GATE_EXIT", code);
        }
        match skip_gate {
            Some(value) => {
                cmd.env("NEUROSTRATA_SKIP_GATE", value);
            }
            None => {
                cmd.env_remove("NEUROSTRATA_SKIP_GATE");
            }
        }
        cmd.output().expect("git push runs")
    }

    /// The CLI binary `cargo test` builds beside the test profile dir:
    /// unit tests run from `<profile>/deps/<test-binary>`, the CLI itself
    /// is at `<profile>/neurostrata-mcp`.
    #[cfg(unix)]
    fn t4_cli_binary() -> std::path::PathBuf {
        let exe = std::env::current_exe().expect("the test binary has a path");
        exe.parent()
            .and_then(|deps| deps.parent())
            .map(|profile| profile.join("neurostrata-mcp"))
            .expect("the CLI binary path derives from the test binary path")
    }

    /// Task 4, Step 1: with `core.hooksPath` set, `hooks install` writes
    /// the hook there — and git actually runs it from there on push; with
    /// it unset, the hook lands in `.git/hooks`.
    #[cfg(unix)]
    #[test]
    fn hooks_install_honors_hooks_path() {
        let root = t4_repo_with_remote("hookspath");
        t4_git_ok(&root, &["config", "core.hooksPath", ".githooks"]);
        let target = install_pre_push_hook(&root, false).expect("install succeeds");
        assert_eq!(target, root.join(".githooks").join("pre-push"));
        assert!(target.is_file(), "the hook lands where git looks for it");
        assert!(
            !root.join(".git").join("hooks").join("pre-push").exists(),
            "with core.hooksPath set, .git/hooks is not git's hooks dir"
        );

        // git runs the installed hook on push: a failing stub gate
        // blocks the push (and leaves its marker); a passing one lets
        // the same push through.
        let (stub_bin, marker) = t4_stub_bin(&root);
        let path = format!("{}:/usr/bin:/bin", stub_bin.display());
        let blocked = t4_push(&root, &path, Some("1"));
        assert!(
            !blocked.status.success(),
            "a failing gate must fail the push: {}",
            String::from_utf8_lossy(&blocked.stderr)
        );
        assert!(marker.exists(), "the hook ran the gate on push");
        assert!(
            String::from_utf8_lossy(&blocked.stderr).contains("Push blocked"),
            "the hook's verdict reaches the pusher: {}",
            String::from_utf8_lossy(&blocked.stderr)
        );
        let clean = t4_push(&root, &path, Some("0"));
        assert!(
            clean.status.success(),
            "a passing gate lets the push through: {}",
            String::from_utf8_lossy(&clean.stderr)
        );
        let _ = std::fs::remove_dir_all(&root);

        // Unset: the default .git/hooks. (Skipped where the ambient
        // config already sets a hooksPath — git_hooks_dir would be
        // right to honor it, exactly as the installer honors it.)
        let root = t4_repo("hooksdefault");
        let ambient = std::process::Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .current_dir(&root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ambient {
            let target = install_pre_push_hook(&root, false).expect("install succeeds");
            assert_eq!(target, root.join(".git").join("hooks").join("pre-push"));
            assert!(target.is_file());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Task 4, Step 1: the installed bytes ARE the scaffolded hook — the
    /// text the task-setup payload ships — in both install locations.
    #[cfg(unix)]
    #[test]
    fn installed_hook_is_the_scaffolded_one() {
        use std::os::unix::fs::PermissionsExt;
        for (tag, hooks_path) in [("bytes-default", None), ("bytes-custom", Some(".githooks"))] {
            let root = t4_repo(tag);
            if let Some(hp) = hooks_path {
                t4_git_ok(&root, &["config", "core.hooksPath", hp]);
            }
            let target = install_pre_push_hook(&root, false).expect("install succeeds");
            let bytes = std::fs::read(&target).unwrap();
            assert_eq!(
                bytes,
                crate::task::SCAFFOLDED_PRE_PUSH_HOOK.as_bytes(),
                "hooks install must write the scaffolded hook text byte for byte ({})",
                tag
            );
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "the installed hook is executable ({})",
                tag
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// Task 4, Step 1: SKIP_GATE has ONE definition in both homes — the
    /// gate CLI's predicate and the installed hook's shell test: only the
    /// exact value "1" skips; "0", "false", "yes", and empty all run the
    /// gate. (That the CLI *logs* the skip — stderr plus its stdout
    /// report line — is pinned behaviorally by
    /// `gate_cli_skip_is_logged_to_stderr_and_report`; the hook's legs
    /// through real pushes by `skip_gate_hook_legs_on_real_pushes`.)
    #[test]
    fn skip_gate_only_exactly_one() {
        assert!(!skip_gate_value_skips(None), "unset runs the gate");
        assert!(!skip_gate_value_skips(Some("")), "empty runs the gate");
        assert!(!skip_gate_value_skips(Some("0")), "\"0\" runs the gate");
        assert!(
            !skip_gate_value_skips(Some("false")),
            "\"false\" runs the gate"
        );
        assert!(!skip_gate_value_skips(Some("yes")), "\"yes\" runs the gate");
        assert!(!skip_gate_value_skips(Some("01")), "\"01\" is not \"1\"");
        assert!(
            !skip_gate_value_skips(Some(" 1")),
            "whitespace is not trimmed into a skip"
        );
        assert!(skip_gate_value_skips(Some("1")), "exactly \"1\" skips");

        let hook = crate::task::SCAFFOLDED_PRE_PUSH_HOOK;
        assert!(
            hook.contains("[ \"$NEUROSTRATA_SKIP_GATE\" = \"1\" ]"),
            "the hook's skip test is the exact-match form"
        );
        assert!(
            !hook.contains("[ -n \"$NEUROSTRATA_SKIP_GATE\" ]"),
            "the hook must not skip on any non-empty value"
        );
    }

    /// Task 4 fix round (I-1): the CLI half of the skip contract, pinned
    /// behaviorally against the real binary. With
    /// `NEUROSTRATA_SKIP_GATE=1` the gate arm logs the skip to stderr AND
    /// prints it in its stdout report, exiting before any store work;
    /// with "yes" it does neither — the gate actually runs (against the
    /// fresh default store under the isolated HOME it reports clean, or
    /// refuses *as a gate*, never as a skip).
    #[cfg(unix)]
    #[test]
    fn gate_cli_skip_is_logged_to_stderr_and_report() {
        let bin = t4_cli_binary();
        assert!(
            bin.is_file(),
            "cargo test builds the CLI binary at {}",
            bin.display()
        );
        let home = std::env::temp_dir().join(format!("ns-t4-clihome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        let run_gate = |skip: &str| {
            std::process::Command::new(&bin)
                .args(["task", "gate", "t4cli"])
                .current_dir(&home)
                .env("HOME", &home)
                .env("NEUROSTRATA_SKIP_GATE", skip)
                .output()
                .expect("the CLI runs")
        };

        let skipped = run_gate("1");
        assert!(
            skipped.status.success(),
            "a skipped gate exits cleanly: {}",
            String::from_utf8_lossy(&skipped.stderr)
        );
        assert!(
            String::from_utf8_lossy(&skipped.stderr).contains(
                "[neurostrata:gate] NEUROSTRATA_SKIP_GATE=1 is set; skipping the gate for 't4cli'."
            ),
            "the skip is logged to stderr: {}",
            String::from_utf8_lossy(&skipped.stderr)
        );
        assert!(
            String::from_utf8_lossy(&skipped.stdout).contains(
                "Task gate: skipped in 't4cli' (NEUROSTRATA_SKIP_GATE=1)."
            ),
            "the skip appears in the stdout gate report: {}",
            String::from_utf8_lossy(&skipped.stdout)
        );

        let ran = run_gate("yes");
        assert!(
            !String::from_utf8_lossy(&ran.stderr).contains("skipping the gate"),
            "SKIP_GATE=yes must not take the skip branch: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&ran.stdout).contains("Task gate: skipped"),
            "SKIP_GATE=yes must not report a skip: {}",
            String::from_utf8_lossy(&ran.stdout)
        );
        assert!(
            String::from_utf8_lossy(&ran.stdout).contains("Task gate: clean in 't4cli'.")
                || String::from_utf8_lossy(&ran.stderr)
                    .contains("[neurostrata:gate] the task gate could not run for 't4cli'"),
            "the gate actually ran (reported, or refused as a gate): stdout={} stderr={}",
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&ran.stderr)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Task 4 fix round (I-1): the hook half of the skip contract,
    /// driven through real pushes. Only exactly "1" skips — announced on
    /// stderr, gate not run; "yes" is not a skip: the gate runs and its
    /// verdict governs the push.
    #[cfg(unix)]
    #[test]
    fn skip_gate_hook_legs_on_real_pushes() {
        let root = t4_repo_with_remote("skiplegs");
        install_pre_push_hook(&root, false).expect("install succeeds");
        let (stub_bin, marker) = t4_stub_bin(&root);
        let path = format!("{}:/usr/bin:/bin", stub_bin.display());

        // "yes" is not a skip: the gate runs, and its failing verdict
        // blocks the push.
        let ran = t4_push_with_skip(&root, &path, Some("1"), Some("yes"));
        assert!(
            !ran.status.success(),
            "SKIP_GATE=yes must not skip: the failing gate blocks the push: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
        assert!(marker.exists(), "the gate ran under SKIP_GATE=yes");
        std::fs::remove_file(&marker).unwrap();

        // Exactly "1" skips: the push proceeds despite the failing gate,
        // the gate never runs, and the skip is announced on stderr.
        let skipped = t4_push_with_skip(&root, &path, Some("1"), Some("1"));
        assert!(
            skipped.status.success(),
            "SKIP_GATE=1 skips the gate, so the push proceeds: {}",
            String::from_utf8_lossy(&skipped.stderr)
        );
        assert!(!marker.exists(), "the gate did not run under SKIP_GATE=1");
        assert!(
            String::from_utf8_lossy(&skipped.stderr)
                .contains("NeuroStrata task gate: skipped (NEUROSTRATA_SKIP_GATE=1)"),
            "the hook announces its skip, never silent: {}",
            String::from_utf8_lossy(&skipped.stderr)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Task 4, Step 1: the installed hook, when `neurostrata-mcp` is not
    /// on PATH, announces that the gate did NOT run and lets the push
    /// proceed — never the old silent `exit 0`.
    #[cfg(unix)]
    #[test]
    fn missing_binary_warns_loudly() {
        let root = t4_repo_with_remote("nobinary");
        install_pre_push_hook(&root, false).expect("install succeeds");
        // A PATH with git but no neurostrata-mcp anywhere on it.
        let out = t4_push(&root, "/usr/bin:/bin", None);
        assert!(
            out.status.success(),
            "a missing gate binary must not block the push: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("warning: neurostrata-mcp not found — gate NOT run for this push"),
            "the skip-by-absence is announced, never silent: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Task 4 fix round (F5): a gate that exits 2 — could-not-check,
    /// never a verdict (under --strict the gate CLI itself exits only 0
    /// or 1, so a 2 means the binary that answered was stale, foreign,
    /// or died before reporting) — is announced on stderr, and the push
    /// still proceeds. Before the fix round this was the one silent
    /// pass left in the hook's own paths.
    #[cfg(unix)]
    #[test]
    fn gate_exit_two_is_announced_and_passes() {
        let root = t4_repo_with_remote("exittwo");
        install_pre_push_hook(&root, false).expect("install succeeds");
        let (stub_bin, marker) = t4_stub_bin(&root);
        let path = format!("{}:/usr/bin:/bin", stub_bin.display());
        let out = t4_push(&root, &path, Some("2"));
        assert!(
            out.status.success(),
            "an exit 2 never blocks the push: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(marker.exists(), "the gate was invoked");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("warning: neurostrata-mcp task gate exited 2"),
            "the could-not-run is announced, never silent: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Task 4, Step 1: the parallel PRE_PUSH_HOOK const is removed by
    /// this task; the scaffolded hook (the surviving text) must carry
    /// every behavior the const had, so the removal loses nothing. The
    /// list below is the const's complete behavior inventory at the Task
    /// 4 branch point. Two const behaviors are deliberately NOT in it —
    /// they are the defects this task removes, replaced and pinned by
    /// their own tests: silent exit when the binary is missing (now the
    /// loud warning, `missing_binary_warns_loudly`) and skip on any
    /// non-empty SKIP_GATE (now exactly "1", `skip_gate_only_exactly_one`).
    /// Everything else must be present in the scaffold verbatim.
    #[test]
    fn const_and_scaffold_diff_is_empty_or_ported() {
        let hook = crate::task::SCAFFOLDED_PRE_PUSH_HOOK;
        assert!(
            hook.starts_with("#!/bin/bash\n"),
            "the scaffold starts with the bash shebang, as the removed const did"
        );
        for behavior in [
            "command -v neurostrata-mcp",
            "NAMESPACE=$(basename \"$(git rev-parse --show-toplevel)\")",
            "neurostrata-mcp task gate \"$NAMESPACE\" --strict",
            "code=$?",
            "if [ $code -eq 1 ]; then",
            "echo \"Push blocked: resolve the tasks above, or NEUROSTRATA_SKIP_GATE=1 git push\" >&2",
            "exit 0   # 0 and 2 both pass here unless --strict flips 2 to blocking",
        ] {
            assert!(
                hook.contains(behavior),
                "behavior the removed const had is missing from the scaffold: {}",
                behavior
            );
        }
        // Fix round (F5): an exit 2 is announced, never silent — and
        // still never blocks. (Under --strict the gate CLI itself exits
        // only 0 or 1, so a 2 is never a verdict: a stale, foreign, or
        // dying binary.) The announcement branch must say so and must
        // not exit 1.
        let two_branch = hook
            .split("if [ $code -eq 2 ]; then")
            .nth(1)
            .expect("the exit-2 announcement branch exists")
            .split("fi")
            .next()
            .unwrap();
        assert!(
            two_branch.contains("gate could NOT run for this push"),
            "an exit 2 is announced as a could-not-run, never silent"
        );
        assert!(
            !two_branch.contains("exit 1"),
            "2 never blocks in the hook itself"
        );
    }

    #[test]
    fn the_gate_and_hook_commands_parse_with_their_flags() {
        let cli =
            Cli::try_parse_from(["neurostrata-mcp", "task", "gate", "NeuroStrata", "--strict"])
                .expect("task gate parses");
        match cli.command {
            Some(Commands::Task {
                action:
                    TaskCommands::Gate {
                        namespace,
                        strict,
                        self_test,
                    },
            }) => {
                assert_eq!(namespace, "NeuroStrata");
                assert!(strict);
                assert!(!self_test, "--self-test is not set in this invocation");
            }
            other => panic!("expected Task/Gate, got {:?}", other),
        }

        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "task",
            "gate",
            "NeuroStrata",
            "--self-test",
        ])
        .expect("task gate --self-test parses");
        match cli.command {
            Some(Commands::Task {
                action:
                    TaskCommands::Gate {
                        namespace,
                        strict,
                        self_test,
                    },
            }) => {
                assert_eq!(namespace, "NeuroStrata");
                assert!(!strict);
                assert!(self_test);
            }
            other => panic!("expected Task/Gate with --self-test, got {:?}", other),
        }

        let cli = Cli::try_parse_from(["neurostrata-mcp", "task", "validate", "MyProj"])
            .expect("task validate parses");
        match cli.command {
            Some(Commands::Task {
                action: TaskCommands::Validate { namespace },
            }) => assert_eq!(namespace, "MyProj"),
            other => panic!("expected Task/Validate, got {:?}", other),
        }

        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "task",
            "import",
            "MyProj",
            "--from-beads",
            ".beads/issues.jsonl",
        ])
        .expect("task import parses");
        match cli.command {
            Some(Commands::Task {
                action:
                    TaskCommands::Import {
                        namespace,
                        from_beads,
                    },
            }) => {
                assert_eq!(namespace, "MyProj");
                assert_eq!(from_beads, ".beads/issues.jsonl");
            }
            other => panic!("expected Task/Import, got {:?}", other),
        }

        let cli = Cli::try_parse_from(["neurostrata-mcp", "doctor", "--namespace", "MyProj"])
            .expect("doctor parses");
        match cli.command {
            Some(Commands::Doctor { namespace }) => {
                assert_eq!(namespace.as_deref(), Some("MyProj"))
            }
            other => panic!("expected Doctor, got {:?}", other),
        }
        let cli =
            Cli::try_parse_from(["neurostrata-mcp", "doctor"]).expect("doctor parses unscoped");
        match cli.command {
            Some(Commands::Doctor { namespace }) => assert!(namespace.is_none()),
            other => panic!("expected Doctor, got {:?}", other),
        }

        let cli = Cli::try_parse_from(["neurostrata-mcp", "hooks", "install", "--force"])
            .expect("hooks install parses");
        match cli.command {
            Some(Commands::Hooks {
                action: HooksCommands::Install { force },
            }) => assert!(force),
            other => panic!("expected Hooks/Install, got {:?}", other),
        }

        // The flag stays optional: plain `hooks install` is the common case.
        let cli = Cli::try_parse_from(["neurostrata-mcp", "hooks", "install"])
            .expect("hooks install parses bare");
        match cli.command {
            Some(Commands::Hooks {
                action: HooksCommands::Install { force },
            }) => assert!(!force),
            other => panic!("expected Hooks/Install, got {:?}", other),
        }
    }

    /// Task 7's operator surface: both commands exist as spelled here, and
    /// `archive` takes the namespace and the id in the order the tool surface
    /// names them -- a swapped pair would archive a row that does not exist
    /// and report only "no memory with that id".
    #[test]
    fn the_archive_and_bus_metrics_commands_parse() {
        let cli = Cli::try_parse_from(["neurostrata-mcp", "bus-metrics"])
            .expect("bus-metrics takes no arguments");
        assert!(
            matches!(cli.command, Some(Commands::BusMetrics)),
            "bus-metrics takes no arguments: {:?}",
            cli.command
        );

        let cli = Cli::try_parse_from(["neurostrata-mcp", "archive", "MyProj", "mem-1"])
            .expect("archive takes a namespace and an id");
        match cli.command {
            Some(Commands::Archive { namespace, id }) => {
                assert_eq!(namespace, "MyProj");
                assert_eq!(id, "mem-1");
            }
            other => panic!("expected Archive, got {:?}", other),
        }

        assert!(
            Cli::try_parse_from(["neurostrata-mcp", "archive", "MyProj"]).is_err(),
            "an archive with no id names nothing to archive"
        );
    }

    /// The SynapticMesh surface parses under its brand and its alias, and
    /// `init` carries every field the spec's onboarding needs. A rename that
    /// silently drops the alias orphans teammate docs the same way a tool
    /// rename orphans client wiring.
    #[test]
    fn the_synaptic_mesh_commands_parse_under_brand_and_alias() {
        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "synaptic-mesh",
            "init",
            "platform-team",
            "--namespace",
            "proj-a",
            "--namespace",
            "proj-b",
            "--remote",
            "git@example:mesh.git",
        ])
        .expect("synaptic-mesh init with flags");
        match cli.command {
            Some(Commands::SynapticMesh {
                action:
                    crate::synaptic_mesh::SynapticMeshCommands::Init {
                        name,
                        namespace,
                        remote,
                    },
            }) => {
                assert_eq!(name, "platform-team");
                assert_eq!(namespace, vec!["proj-a".to_string(), "proj-b".to_string()]);
                assert_eq!(remote.as_deref(), Some("git@example:mesh.git"));
            }
            other => panic!("expected SynapticMesh/Init, got {:?}", other),
        }

        // The documented short alias must resolve to the same command.
        let cli = Cli::try_parse_from(["neurostrata-mcp", "mesh", "status"])
            .expect("mesh status parses via the short alias");
        assert!(
            matches!(
                cli.command,
                Some(Commands::SynapticMesh {
                    action: crate::synaptic_mesh::SynapticMeshCommands::Status
                })
            ),
            "alias broken: {:?}",
            cli.command
        );
    }

    /// Step 4's contract, both halves: with a daemon answering, stdout is that
    /// daemon's `BusMetrics` JSON and nothing else -- it is what an operator
    /// pipes into `jq` -- and the command exits 0.
    ///
    /// The stub answers with the shared handler's own bytes, so this also holds
    /// the CLI and the route to one body rather than two shapes.
    #[tokio::test(flavor = "multi_thread")]
    async fn bus_metrics_prints_the_daemons_json_and_exits_zero() {
        let bus = Arc::new(crate::events::ThalamicBus::new(32));
        bus.emit(
            crate::events::ThalamicPulse::Archived {
                id: "m1".to_string(),
                namespace: "cli-bus-metrics".to_string(),
            },
            &crate::events::RecursionToken::root(),
        );
        let body = crate::handlers::handle_bus_metrics(bus).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let expected = body.clone();
        let stub = tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/bus/metrics",
                axum::routing::post(move || {
                    let body = expected.clone();
                    async move { body }
                }),
            );
            axum::serve(listener, app).await.unwrap();
        });

        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let code = run_bus_metrics(true, &addr, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            code,
            0,
            "a daemon answered: {:?}",
            String::from_utf8_lossy(&err)
        );

        let printed = String::from_utf8(out).expect("stdout is UTF-8");
        let snapshot: serde_json::Value =
            serde_json::from_str(printed.trim()).unwrap_or_else(|e| {
                panic!(
                    "stdout must be the metrics JSON itself, not prose about it: {printed} ({e})"
                )
            });
        assert_eq!(
            snapshot["events_emitted"], 1,
            "one pulse was emitted: {printed}"
        );
        for field in [
            "events_emitted",
            "dispatcher_handled",
            "subscribers",
            "queue_depth",
            "drops_oldest",
            "subscriber_panics",
            "subscriber_timeouts",
            "recursion_blocks",
        ] {
            assert!(
                snapshot.get(field).is_some(),
                "{field} must be in what the operator reads: {printed}"
            );
        }
        assert_eq!(
            printed.trim(),
            body.trim(),
            "stdout is the daemon's body verbatim"
        );

        stub.abort();
    }

    /// The other half of the same contract: no daemon is not an idle bus, and
    /// saying so in a stderr caveat while printing zeros to stdout is the
    /// hazard -- `bus-metrics | jq` cannot see the caveat. So: exit non-zero,
    /// nothing on stdout, and the refusal names the address it looked at.
    #[tokio::test]
    async fn bus_metrics_without_a_daemon_exits_nonzero_and_prints_no_json() {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let code = run_bus_metrics(false, DAEMON_ADDR, &mut out, &mut err)
            .await
            .unwrap();

        assert_eq!(
            code, BUS_METRICS_FAILURE_EXIT,
            "no daemon is a failure, not an idle bus"
        );
        assert!(
            out.is_empty(),
            "a fabricated zeroed snapshot must never reach stdout: {:?}",
            String::from_utf8_lossy(&out)
        );
        let said = String::from_utf8(err).expect("stderr is UTF-8");
        assert!(
            said.contains(DAEMON_ADDR) && said.contains("cannot read bus metrics"),
            "the refusal must name the address it could not reach: {said}"
        );
    }

    /// A daemon that answered the probe and then went away, or is wedged past
    /// the timeout, is the same absence to a script: exit 1, no JSON. The
    /// address is a dead local port, so the POST fails at once -- which is also
    /// why the request needs a timeout at all, since an unbound port on some
    /// machines hangs rather than refuses.
    #[tokio::test]
    async fn bus_metrics_exits_nonzero_when_the_daemon_never_answers() {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let code = run_bus_metrics(true, &addr, &mut out, &mut err)
            .await
            .unwrap();

        assert_eq!(code, BUS_METRICS_FAILURE_EXIT, "the daemon never answered");
        assert!(
            out.is_empty(),
            "no JSON is printed for an unreachable daemon: {:?}",
            String::from_utf8_lossy(&out)
        );
        let said = String::from_utf8(err).expect("stderr is UTF-8");
        assert!(
            said.contains(&addr) && said.contains("cannot read bus metrics"),
            "{said}"
        );
    }

    /// The POST is bounded. Untimed it could hang on a port that accepts and
    /// never replies, which is the behaviour this file documents at the
    /// readiness wait; there is no way to assert a hang from a test, so the
    /// bound itself is asserted instead.
    #[test]
    fn the_bus_metrics_post_is_bounded() {
        assert_eq!(
            BUS_METRICS_TIMEOUT,
            std::time::Duration::from_secs(10),
            "the other POSTs in this file use ten seconds; bus-metrics hangs the same way"
        );
    }

    /// Round 3: a daemon that cannot answer /info predates build verification,
    /// so it is older than any binary that asks -- stale by definition.
    #[test]
    fn a_daemon_without_build_info_is_stale_by_definition() {
        assert!(matches!(skew_verdict(None, (1, 2)), Skew::Stale(_)));
    }

    #[test]
    fn matching_fingerprints_are_in_sync_and_mismatches_are_stale() {
        let info = serde_json::json!({ "version": "1.7.0", "exe_hash": 7u64, "exe_len": 8u64, "started_at": 1 });
        assert!(matches!(skew_verdict(Some(&info), (7, 8)), Skew::InSync(_)));
        assert!(matches!(skew_verdict(Some(&info), (7, 9)), Skew::Stale(_)));
        assert!(matches!(skew_verdict(Some(&info), (6, 8)), Skew::Stale(_)));
    }

    /// Task 1 (J9): the gate trusts a daemon only when /info reports the
    /// build fingerprint of the binary on disk. A foreign fingerprint is
    /// refused with a message naming both, and a daemon that cannot
    /// identify itself at all is refused too -- /health answering is not
    /// identity, and a squatter answers /health.
    #[test]
    fn the_gate_verifies_daemon_identity_before_trusting_it() {
        let own = (7u64, 8u64);
        let good = serde_json::json!({ "version": "1.11.0", "exe_hash": 7u64, "exe_len": 8u64, "started_at": 1 });
        assert!(gate_verify_daemon(Some(&good), own).is_ok());

        let foreign = serde_json::json!({ "version": "9.9.9", "exe_hash": 0xdeadu64, "exe_len": 1u64, "started_at": 2 });
        let err = gate_verify_daemon(Some(&foreign), own).unwrap_err();
        assert!(
            err.contains("000000000000dead") && err.contains("0000000000000007"),
            "the refusal must name the reported and expected fingerprints: {err}"
        );

        assert!(gate_verify_daemon(None, own).is_err());
    }

    // ---- Task 19 (J26): the tracing subscriber these tests presume ----
    //
    // Until this task the crate declared `tracing` and emitted through
    // ~50 call sites with no subscriber installed anywhere: every event
    // went nowhere. The production seam is `init_tracing()` at the top
    // of `main()`: it builds the subscriber via
    // `tracing_fmt_subscriber(std::io::stderr, tracing_env_filter())`
    // and installs it as the global default. The writer slot is stderr
    // because stdout is the MCP stdio channel — a log line there would
    // corrupt the protocol. These tests drive the same builder with a
    // capture writer standing in the stderr slot, installed as a scoped
    // default so they never contend for the process-global one.

    /// An in-memory `Write` sink handed to the fmt layer through its
    /// `MakeWriter` closure; cloning shares the one buffer.
    #[derive(Clone, Default)]
    struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturedLog {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Serializes the tests that set `RUST_LOG` (env is process-global)
    /// and restores the caller's prior value on drop.
    struct RustLogRestore {
        prior: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl RustLogRestore {
        fn set(value: Option<&str>) -> Self {
            let lock = crate::test_support::ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prior = std::env::var_os("RUST_LOG");
            match value {
                Some(v) => std::env::set_var("RUST_LOG", v),
                None => std::env::remove_var("RUST_LOG"),
            }
            Self { prior, _lock: lock }
        }
    }

    impl Drop for RustLogRestore {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("RUST_LOG", v),
                None => std::env::remove_var("RUST_LOG"),
            }
        }
    }

    #[test]
    fn tracing_output_reaches_stderr() {
        // The production entry point itself runs, and running it twice
        // in one process is not an error (a re-exec'd helper or an
        // embedded caller must never panic on the one-shot global).
        init_tracing();
        init_tracing();

        let _env = RustLogRestore::set(None); // default filter: info
        let captured = CapturedLog::default();
        let sink = captured.clone();
        let subscriber = tracing_fmt_subscriber(move || sink.clone(), tracing_env_filter());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("tracing_output_reaches_stderr marker");
        });
        let text = captured.text();
        assert!(
            text.contains("tracing_output_reaches_stderr marker"),
            "the info event lands in the stderr slot: {text}"
        );
        assert!(
            text.contains("INFO"),
            "the default filter admits info, formatted with its level: {text}"
        );
    }

    #[test]
    fn stdout_carries_no_log_output() {
        let _env = RustLogRestore::set(None);
        let captured = CapturedLog::default();
        let sink = captured.clone();
        let subscriber = tracing_fmt_subscriber(move || sink.clone(), tracing_env_filter());
        // Stand-ins for the stdio mode's two channels: `captured` plays
        // the stderr role (the writer production hands the fmt layer);
        // a plain buffer plays stdout, written the way the stdio loop
        // writes a protocol frame.
        let mut stdout_role: Vec<u8> = Vec::new();
        tracing::subscriber::with_default(subscriber, || {
            stdout_role.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":1}\n");
            tracing::info!("stdout_carries_no_log_output marker");
        });
        assert_eq!(
            String::from_utf8(stdout_role).unwrap(),
            "{\"jsonrpc\":\"2.0\",\"id\":1}\n",
            "stdout carries protocol bytes only — no log output"
        );
        assert!(
            captured.text().contains("stdout_carries_no_log_output marker"),
            "the log line went to the stderr slot instead"
        );
    }

    #[test]
    fn rust_log_filter_is_honored() {
        let _env = RustLogRestore::set(Some("error"));
        let captured = CapturedLog::default();
        let sink = captured.clone();
        let subscriber = tracing_fmt_subscriber(move || sink.clone(), tracing_env_filter());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("rust_log_filter_is_honored info marker");
            tracing::error!("rust_log_filter_is_honored error marker");
        });
        let text = captured.text();
        assert!(
            text.contains("rust_log_filter_is_honored error marker"),
            "the error event passes the RUST_LOG=error filter: {text}"
        );
        assert!(
            !text.contains("rust_log_filter_is_honored info marker"),
            "the info event is suppressed by RUST_LOG=error: {text}"
        );
    }

    #[test]
    fn unparsable_rust_log_falls_back_to_info() {
        // `lolwut` is not a level, so the directive is invalid and
        // `try_from_env` errors: the fallback must yield the `info`
        // default — not a bricked startup, and not a permissive
        // filter that lets everything through.
        let _env = RustLogRestore::set(Some("neurostrata_mcp=lolwut"));
        let captured = CapturedLog::default();
        let sink = captured.clone();
        let subscriber = tracing_fmt_subscriber(move || sink.clone(), tracing_env_filter());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("unparsable_rust_log_falls_back_to_info info marker");
            tracing::debug!("unparsable_rust_log_falls_back_to_info debug marker");
        });
        let text = captured.text();
        assert!(
            text.contains("unparsable_rust_log_falls_back_to_info info marker"),
            "the info event passes the fallback filter: {text}"
        );
        assert!(
            !text.contains("unparsable_rust_log_falls_back_to_info debug marker"),
            "the debug event is suppressed — the fallback is info, not trace: {text}"
        );
    }
}

/// Where every CLI HTTP call goes. A constant because `bus-metrics` names it
/// in a refusal an operator has to be able to copy.
const DAEMON_ADDR: &str = "127.0.0.1:34343";

/// The auth headers every CLI HTTP call carries: the daemon bearer, and
/// -- for the destructive-class commands -- the operator token (DR-5).
fn daemon_auth_headers(with_operator: bool) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(token) = daemon::read_token_file("daemon.token") {
        if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
            headers.insert(reqwest::header::AUTHORIZATION, val);
        }
    }
    if with_operator {
        if let Some(operator) = daemon::read_token_file("operator.token") {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&operator) {
                headers.insert(
                    reqwest::header::HeaderName::from_static("x-neurostrata-operator"),
                    val,
                );
            }
        }
    }
    headers
}

/// Build an HTTP client that carries the daemon's bearer token (if available)
/// in its default headers, so CLI proxy calls authenticate against the daemon.
fn daemon_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(daemon_auth_headers(false))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// The daemon HTTP client plus the operator token, for the CLI commands
/// whose routes are destructive-class (/shutdown, /backup): the daemon
/// refuses those with the bearer alone.
fn daemon_operator_client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(daemon_auth_headers(true))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// How long `bus-metrics` waits for the daemon's answer. Same ten seconds the
/// other POSTs in this file use, and for the same reason an unbound port on
/// this machine hangs rather than refuses: untimed, this command can hang with
/// no output at all instead of reporting a daemon that is not there.
const BUS_METRICS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The exit code both refusal paths use. Non-zero so `bus-metrics | jq` and any
/// `set -e` script see the failure rather than reading absence of data as
/// "the bus is idle".
const BUS_METRICS_FAILURE_EXIT: i32 = 1;

/// What `bus-metrics` prints on the success path, and nothing else.
///
/// Split out from the command arm so the stdout an operator pipes into `jq`
/// is a thing with a test: it must be the daemon's JSON body, verbatim, with
/// no banner or caveat wrapped around it. The caveat for a missing daemon
/// goes to stderr on the other path instead.
fn print_bus_metrics_to(out: &mut impl std::io::Write, body: &str) -> std::io::Result<()> {
    writeln!(out, "{}", body)
}

/// `bus-metrics` with no daemon to ask, as the probe already found out.
///
/// No daemon is not "an idle bus": the pulses live in the process that owns the
/// store, so a bus built here to answer for it would print zeros that a script
/// cannot distinguish from a real, un-pulsed bus. So this refuses rather than
/// reporting, and says which address it looked at.
fn refuse_bus_metrics_to(err: &mut impl std::io::Write, addr: &str) -> std::io::Result<()> {
    writeln!(
        err,
        "no NeuroStrata daemon is running at {} -- cannot read bus metrics",
        addr
    )
}

/// `bus-metrics` when the daemon answered the probe but not the POST: it
/// stopped listening, or it is wedged past the timeout above. Reported like
/// any other absence, with the transport's own words kept.
fn refuse_bus_metrics_unreachable_to(
    err: &mut impl std::io::Write,
    addr: &str,
    cause: &dyn std::fmt::Display,
) -> std::io::Result<()> {
    writeln!(
        err,
        "no NeuroStrata daemon answered at {} -- cannot read bus metrics: {}",
        addr, cause
    )
}

/// `neurostrata-mcp bus-metrics`, as a function that returns an exit code
/// instead of taking the process with it.
///
/// Both halves of the contract live here so both can be tested: the success
/// path prints the daemon's `BusMetrics` JSON to `out` and returns 0, and
/// neither refusal path prints anything to `out`, says why on `err`, and
/// returns non-zero. `daemon_running` is the caller's earlier probe: it is
/// what keeps this command from touching the database at all when no daemon
/// owns it.
async fn run_bus_metrics(
    daemon_running: bool,
    addr: &str,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> anyhow::Result<i32> {
    if !daemon_running {
        refuse_bus_metrics_to(err, addr)?;
        return Ok(BUS_METRICS_FAILURE_EXIT);
    }

    let res = match daemon_http_client()
        .post(format!("http://{addr}/bus/metrics"))
        .timeout(BUS_METRICS_TIMEOUT)
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            refuse_bus_metrics_unreachable_to(err, addr, &e)?;
            return Ok(BUS_METRICS_FAILURE_EXIT);
        }
    };
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        // 404 is the expected answer from a daemon older than this route, and
        // saying so beats reporting an empty body as if the bus had said
        // nothing at all.
        writeln!(
            err,
            "POST /bus/metrics answered {}: {}",
            status.as_u16(),
            body
        )?;
        if status == reqwest::StatusCode::NOT_FOUND {
            writeln!(err, "That daemon predates the route. `neurostrata-mcp status` reports a stale daemon by comparing build fingerprints.")?;
        }
        return Ok(BUS_METRICS_FAILURE_EXIT);
    }

    print_bus_metrics_to(out, &body)?;
    Ok(0)
}

async fn restore_snapshot_bundle(
    snapshot_dir: &std::path::Path,
    target: &std::path::Path,
    config: &Config,
) -> anyhow::Result<Vec<String>> {
    let snapshot = snapshot_dir.join("ladybug.store");
    let manifest_path = snapshot_dir.join("manifest.json");
    if !snapshot.is_file() || !manifest_path.is_file() {
        anyhow::bail!("snapshot is incomplete: database or manifest is missing");
    }
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    if manifest.get("format").and_then(|v| v.as_str()) != Some("neurostrata-snapshot") {
        anyhow::bail!("snapshot manifest has an unsupported format");
    }
    let version = manifest
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("snapshot manifest has no valid version"))?;
    if version != 1 && version != 2 {
        anyhow::bail!("snapshot manifest version {version} is unsupported");
    }
    if let Some(bytes) = manifest.get("bytes").and_then(|v| v.as_u64()) {
        if std::fs::metadata(&snapshot)?.len() != bytes {
            anyhow::bail!("snapshot database size does not match its manifest");
        }
    }
    let manifest_stages = if version == 2 {
        let entries = manifest
            .get("stage_generations")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("snapshot stage provenance is missing"))?;
        let mut names = Vec::with_capacity(entries.len());
        let mut unique = HashSet::new();
        for entry in entries {
            let name = entry
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("snapshot stage identifier is not a string"))?;
            if !crate::traits::is_internal_ingest_stage_namespace(name)
                || !unique.insert(name.to_string())
            {
                anyhow::bail!("snapshot contains invalid or duplicate stage provenance");
            }
            names.push(name.to_string());
        }
        Some(names)
    } else {
        None
    };

    let dimensions = embed::configured_dimensions()?;
    let embedder_id = embed::configured_embedder_id()?;
    let limits = profiled_store_limits(config)?;
    let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let writer_target = target.to_path_buf();
    let _restore_writer = tokio::task::spawn_blocking(move || {
        crate::store::ladybug::acquire_database_writer_lock(&writer_target)
    })
    .await??;
    #[cfg(test)]
    test_restore_target_lock_barrier(target);
    let target_wal = appended_path(target, ".wal");
    let target_marker = crate::store::ladybug::width_marker_path(target);
    let restore_marker = crate::store::ladybug::restore_in_progress_path(target);
    if target.exists() || target_wal.exists() || target_marker.exists() || restore_marker.exists() {
        anyhow::bail!("restore destination or a sibling sidecar already exists");
    }
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database");
    let temp_dir = parent.join(format!(".{target_name}.restore-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;

    let marker_file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&restore_marker)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(error.into());
        }
    };
    drop(marker_file);
    let temp_db = temp_dir.join("ladybug.store");
    let temp_wal = appended_path(&temp_db, ".wal");
    let temp_marker = crate::store::ladybug::width_marker_path(&temp_db);
    let snapshot_wal = appended_path(&snapshot, ".wal");
    let snapshot_marker = crate::store::ladybug::width_marker_path(&snapshot);
    let mut restored_leases = Vec::new();
    let mut target_wal_installed = false;
    let mut target_marker_installed = false;
    let mut target_db_installed = false;

    let outcome = async {
        std::fs::copy(&snapshot, &temp_db)?;
        if snapshot_wal.exists() {
            std::fs::copy(&snapshot_wal, &temp_wal)?;
        }
        if snapshot_marker.exists() {
            std::fs::copy(&snapshot_marker, &temp_marker)?;
        }
        let candidate = LadybugStore::new_with_identity_limits(
            &temp_db,
            dimensions,
            embedder_id.clone(),
            limits,
        )?;
        candidate.init("restore-validation").await?;
        let mut db_stages = candidate.ingest_stage_namespaces().await?;
        db_stages.sort();
        if let Some(mut listed) = manifest_stages.clone() {
            listed.sort();
            if listed != db_stages {
                anyhow::bail!(
                    "snapshot manifest and database stage provenance do not match (manifest: {:?}; database: {:?})",
                    listed,
                    db_stages
                );
            }
        }
        drop(candidate);

        restored_leases = crate::store::ladybug::create_restored_stage_leases(target, &db_stages)?;
        // Opening the temporary bundle above may replay its WAL and the
        // validation init may update the catalog. Install the WAL that now
        // belongs to the validated temporary database; pairing that database
        // with the pre-validation snapshot WAL can replay changes twice or
        // apply offsets against a different DB image.
        if temp_wal.exists() {
            std::fs::copy(&temp_wal, &target_wal)?;
            target_wal_installed = true;
        }
        if temp_marker.exists() {
            std::fs::copy(&temp_marker, &target_marker)?;
            target_marker_installed = true;
        }
        std::fs::rename(&temp_db, target)?;
        target_db_installed = true;
        std::fs::remove_file(&restore_marker)?;
        // The complete DB, WAL, marker, and fresh lease provenance are now
        // visible. Release the installation lock before the normal constructor
        // below reacquires it for post-restore validation.
        drop(_restore_writer);

        // The database plus all sibling provenance is now complete. Verify the
        // installed copy after removing the interlock; a failure leaves a
        // complete, inspectable restore rather than deleting the snapshot data.
        let restored =
            LadybugStore::new_with_identity_limits(target, dimensions, embedder_id, limits)?;
        restored.init("restore-validation").await?;
        restored.list_namespaces().await
    }
    .await;

    let _ = std::fs::remove_dir_all(&temp_dir);
    if outcome.is_err() && !target_db_installed {
        for path in restored_leases {
            let _ = std::fs::remove_file(path);
        }
        if target_wal_installed {
            let _ = std::fs::remove_file(&target_wal);
        }
        if target_marker_installed {
            let _ = std::fs::remove_file(&target_marker);
        }
        let _ = std::fs::remove_file(&restore_marker);
    }
    outcome
}

#[cfg(test)]
static TEST_RESTORE_TARGET_LOCK_BARRIER: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::path::PathBuf, std::path::PathBuf)>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn set_test_restore_target_lock_barrier(target: std::path::PathBuf, barrier: std::path::PathBuf) {
    *TEST_RESTORE_TARGET_LOCK_BARRIER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = Some((target, barrier));
}

#[cfg(test)]
fn test_restore_target_lock_barrier(target: &std::path::Path) {
    let barrier = {
        let mut state = TEST_RESTORE_TARGET_LOCK_BARRIER
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap();
        match state.as_ref() {
            Some((expected, _)) if expected == target => state.take().unwrap().1,
            _ => return,
        }
    };
    std::fs::write(&barrier, "entered").unwrap();
    let mut release = barrier.as_os_str().to_os_string();
    release.push(".release");
    let release = std::path::PathBuf::from(release);
    let started = std::time::Instant::now();
    while !release.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "restore target-lock test barrier timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn appended_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    std::path::PathBuf::from(value)
}

/// Install the process-wide `tracing` subscriber — until this existed,
/// the crate's ~50 `tracing::` call sites emitted into the void (J26):
/// `tracing` was declared, call sites assumed a subscriber, and none
/// was ever installed. Called once at the top of `main()`, before the
/// runtime and every mode dispatch, so the daemon, the stdio proxy,
/// and every CLI command all log through the one subscriber.
///
/// The fmt layer writes to **stderr**: stdout is the MCP stdio channel,
/// and a log line there would corrupt the protocol.
fn init_tracing() {
    let subscriber = tracing_fmt_subscriber(std::io::stderr, tracing_env_filter());
    // The global default can be installed only once per process. A
    // second call — an embedded caller, a re-exec'd helper — must
    // never panic, so the already-installed error is dropped.
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// The filter for the startup subscriber: `RUST_LOG` when it is set and
/// parses, the `info` default otherwise. An unparsable `RUST_LOG`
/// falls back to the default rather than bricking startup on a typo.
fn tracing_env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

/// The fmt subscriber over an arbitrary writer: production passes
/// `std::io::stderr`; the tests pass a capture buffer in the same
/// slot. ANSI escapes are off — the readers are operators tailing a
/// terminal, a log file, or the daemon's captured stderr, and plain
/// text serves all three.
fn tracing_fmt_subscriber<W>(
    writer: W,
    filter: tracing_subscriber::EnvFilter,
) -> impl tracing::Subscriber + Send + Sync + 'static
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .finish()
}

/// The tokio runtime, built by hand rather than by `#[tokio::main]`.
///
/// The only reason to build it by hand is the crash handler. `SA_ONSTACK`
/// runs the handler on the thread's alternate signal stack, and the Rust
/// standard library gives each thread an 8192-byte one -- too small for the
/// handler's backtrace capture, which would then fault inside the handler
/// and lose the breadcrumb. `on_thread_start` is the only hook tokio offers
/// for a per-thread setup, and it covers the worker threads.
fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(|| {
            let _ = crash::install_alt_stack();
        })
        .build()?)
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let runtime = runtime()?;
    runtime.block_on(run())
}

/// The elected daemon log path for a given home (T19, Gate 0 Home β):
/// `<home>/.local/share/neurostrata/daemon.log`.
fn daemon_log_path_for(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".local")
        .join("share")
        .join("neurostrata")
        .join("daemon.log")
}

/// Where an auto-spawned daemon child's stderr is sent (T19).
///
/// The home's basis is `dirs::home_dir()`: on this system the resulting
/// path is byte-identical to the watchdog's `DAEMON_LOG`
/// (`$HOME/.local/share/neurostrata/daemon.log`,
/// `watchdog-backup.sh:47`), so watchdog-started and auto-spawned
/// daemon lifetimes log to one file. This deliberately does NOT follow
/// `XDG_DATA_HOME`, matching the watchdog's own ignoring of it.
///
/// There is no CWD fallback: when the home directory cannot be
/// resolved this returns `None`, and `daemon_stderr()` falls back to
/// discarding the child's stderr rather than inventing a path in the
/// process's working directory.
fn daemon_log_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| daemon_log_path_for(&home))
}

/// Where the auto-spawned daemon child's stderr goes: the daemon log
/// file, or nowhere. The decision is an enum (not a `Stdio`) because
/// `std::process::Stdio` offers no introspection for tests to match on.
enum DaemonStderr {
    Log(std::fs::File),
    Null,
}

/// Resolve the daemon child's stderr target (T19, DD-3′/DD-4′).
fn daemon_stderr() -> DaemonStderr {
    match daemon_log_path() {
        Some(path) => daemon_stderr_for(&path),
        None => {
            eprintln!(
                "cannot resolve a home directory for the daemon log; \
                 the daemon's stderr will be discarded"
            );
            DaemonStderr::Null
        }
    }
}

/// Open `path` (append + create, parent directories created) as the
/// daemon child's stderr target.
///
/// On any failure the path and error are printed to this process's own
/// stderr and the child gets `DaemonStderr::Null`. That is not a
/// mitigation: on open failure the child's whole-life stderr blindness
/// persists in full, and this note is best-effort -- it sits on the
/// proxy parent's stderr, a channel expected to be unread in
/// editor-launched deployments (DD-3′).
fn daemon_stderr_for(path: &std::path::Path) -> DaemonStderr {
    let created = match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent),
        None => Ok(()),
    };
    let opened = created.and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    });
    match opened {
        Ok(file) => DaemonStderr::Log(file),
        Err(e) => {
            eprintln!(
                "cannot open the daemon log {} ({}); the daemon's stderr will be discarded",
                path.display(),
                e
            );
            DaemonStderr::Null
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cli = Cli::parse();

    // This config-only command must not probe, start, or stop a daemon, and it
    // must not construct an embedder or open the database.
    if let Some(Commands::ProjectContext { action }) = cli.command.as_ref() {
        let config_path = Config::default_path()?;
        if !config_path.exists() {
            Config::from_default_path()?;
        }
        println!("{}", dispatch_project_context(action, &config_path)?);
        return Ok(());
    }

    // The crash breadcrumb, installed before anything long-running so the
    // daemon and the MCP proxy are both covered. A failure here costs a
    // diagnostic, not the run: the handler is never a prerequisite, so the
    // process carries on without it.
    let dir = crash_dir();
    if let Err(e) = crash::install(&dir) {
        eprintln!("crash breadcrumbs are off ({}): {}", dir.display(), e);
    }

    // Check if the daemon is already running on port 34343
    let probe = probe_daemon().await;
    // Only a daemon that answered gets to hold the database lock as far as the
    // CLI is concerned. Treating silence as "running" would refuse every local
    // command on a machine where an empty port times out rather than refuses.
    let daemon_running = probe == DaemonProbe::Responsive;

    // If no arguments, start standard MCP stdio mode
    if args.len() == 1 {
        if daemon_running {
            eprintln!("Daemon is already running. Starting MCP proxy...");
            server::start_mcp_proxy(server::DaemonOrigin::AlreadyRunning).await?;
            return Ok(());
        } else {
            // The probe can return Silent (timeout) for a daemon that is alive
            // but briefly not answering -- a restart window, model load, etc.
            // If the lock is held, a daemon exists: wait for it instead of
            // spawning a second one that will fail the lock check and leave
            // the proxy hung for 30s reporting "could not reach".
            let lock_held = config_db_path()
                .map(|p| lock_path_for(&p).exists())
                .unwrap_or(false);

            if lock_held {
                eprintln!(
                    "NeuroStrata daemon is starting (lock held); waiting for it to answer..."
                );
                let client = daemon_http_client();
                for _ in 0..1200 {
                    // up to 2 minutes
                    if client
                        .get("http://127.0.0.1:34343/health")
                        .timeout(std::time::Duration::from_millis(500))
                        .send()
                        .await
                        .is_ok()
                    {
                        eprintln!("Daemon is already running. Starting MCP proxy...");
                        server::start_mcp_proxy(server::DaemonOrigin::AlreadyRunning).await?;
                        return Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                eprintln!("A daemon holds the lock but did not answer within 2 minutes. Refusing to spawn a second instance.");
                std::process::exit(1);
            }

            eprintln!("NeuroStrata MCP Server initializing...");

            // Spawn the daemon as a detached process
            let exe = std::env::current_exe()
                .unwrap_or_else(|_| std::path::PathBuf::from("neurostrata-mcp"));
            let stderr = daemon_stderr();
            std::process::Command::new(exe)
                .arg("daemon")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(match stderr {
                    DaemonStderr::Log(file) => std::process::Stdio::from(file),
                    DaemonStderr::Null => std::process::Stdio::null(),
                })
                .spawn()?;

            eprintln!(
                "Waiting for daemon to become ready (this may take a moment while models load)..."
            );

            // Wait for daemon to become ready. Bounded per attempt as well as
            // overall: an unbound port on this machine hangs rather than
            // refusing -- the behaviour DaemonProbe exists to describe -- so an
            // untimed send here could spend the whole "30 seconds max" inside
            // one call.
            let client = daemon_http_client();
            for _ in 0..300 {
                // 30 seconds max
                if client
                    .get("http://127.0.0.1:34343/health")
                    .timeout(std::time::Duration::from_millis(500))
                    .send()
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            // Deliberately not conditional on that loop having succeeded. A
            // first run downloads the embedding model and takes far longer than
            // 30s, and the proxy is told where the daemon came from so it waits
            // rather than reporting a daemon it started itself as absent.
            server::start_mcp_proxy(server::DaemonOrigin::SpawnedByUs).await?;
            return Ok(());
        }
    }

    // An unrecognised first argument used to be run as an external program.
    // That made every typo an execution: `neurostrata-mcp lsit` ran whatever
    // `lsit` resolved to on PATH, and anything that could put a file on the
    // PATH could therefore have it invoked by a mistyped memory command. The
    // capability now needs asking for, by name, through `run` (bead
    // neurostrata-7s8). clap reports anything else as the unknown subcommand
    // it is.

    // Now parse using clap
    if let Some(command) = cli.command {
        match command {
            Commands::Daemon => {
                println!("NeuroStrata MCP Server initializing in DAEMON-ONLY mode...");
                let config = Config::from_default_path()?;
                // Taken before the database opens. `shutdown` waits for this lock
                // to be released rather than for the port to close.
                let _ = DAEMON_LOCK.set(take_daemon_lock(&config.db_path)?);
                let startup = resolve_startup(&config)?;
                let embedder = embed::build_embedder_for(&startup.resolution.model)?;
                let vector_store: Arc<dyn VectorStore> =
                    Arc::new(LadybugStore::new_with_identity_limits(
                        config.db_path.to_string_lossy().to_string(),
                        embedder.dimensions(),
                        embedder.embedder_id().to_string(),
                        startup.limits,
                    )?);
                vector_store.init("global").await?;
                let outcome = daemon::start_daemon(
                    embedder,
                    vector_store,
                    Some(config.deduplication.clone()),
                )
                .await;
                record_final_checkpoint(&outcome);
                outcome?;
            }
            Commands::Laya { model, out_dir } => {
                let model_id = model.unwrap_or_else(|| "v1.11.1".to_string());
                let target_dir = out_dir.unwrap_or_else(|| "artifacts/laya-bundle".to_string());
                crate::judgment::laya_onnx::LayaOnnxProvider::fetch_model(&model_id, &target_dir).await?;
                println!("Laya model '{}' fetched and verified to '{}'", model_id, target_dir);
            }
            Commands::Run { program, args } => {
                // Deliberate, named, and it never touches the database.
                match std::process::Command::new(&program).args(&args).status() {
                    Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                    Err(e) => {
                        eprintln!("Could not run '{}': {}", program, e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::SynapticMesh { action } => {
                // Also never touches the database: the mesh is a standalone
                // git repo the daemon owns. Call-site ambient resolution is
                // legal HERE (cwd is what a CLI means by "this project"),
                // which is exactly why it is resolved now and threaded,
                // never sniffed inside the handler.
                let project_root = std::env::current_dir()?;
                let ctx = crate::synaptic_mesh::MeshContext {
                    project_root: project_root.clone(),
                    base_root: crate::synaptic_mesh::conventional_base_root(),
                    node_id: crate::synaptic_mesh::resolve_node_id(),
                };
                let mut svc = crate::synaptic_mesh::MeshService::new(project_root);
                let reply =
                    crate::synaptic_mesh::handle_synaptic_mesh_command(&action, &ctx, &mut svc)
                        .await;
                println!("{}", reply);
                // Hook piggyback contract (spec §Hooks): the hook target
                // ALWAYS exits 0, even when its reply carries a failure —
                // a hook must never block the source push. The generic
                // fallthrough also exits 0, but the invariant is stated
                // (and enforced) explicitly here so a later refactor cannot
                // silently gate a hook on mesh state.
                if matches!(
                    action,
                    crate::synaptic_mesh::SynapticMeshCommands::SyncEnqueue
                ) {
                    return Ok(());
                }
            }
            Commands::Shutdown => {
                let config = Config::from_default_path()?;
                let db_path = config.db_path.clone();
                let held = daemon_holds_lock(&db_path);

                match probe {
                    DaemonProbe::Absent if !held => {
                        println!("No daemon is running on 127.0.0.1:34343.");
                        return Ok(());
                    }
                    DaemonProbe::Absent => {
                        eprintln!("A daemon has stopped listening but still holds the database, so it is finishing its final checkpoint. Waiting for it to exit.");
                    }
                    DaemonProbe::Silent => {
                        eprintln!("Nothing answered on 127.0.0.1:34343 within 500ms. Either no daemon is running, or one is busy in the database and cannot answer yet -- those look identical from here. Sending a stop request and waiting; if a daemon is there, this can take a couple of minutes. Do not kill it.");
                    }
                    DaemonProbe::Responsive => {}
                }

                // /shutdown is destructive-class: the operator client, or
                // the daemon refuses the request (DR-5).
                let client = daemon_operator_client();
                // A busy daemon may not answer this either. That is not a
                // failure: the request is queued, so fall through to the wait.
                if probe != DaemonProbe::Absent {
                    if let Err(e) = client
                        .post("http://127.0.0.1:34343/shutdown")
                        .timeout(std::time::Duration::from_secs(10))
                        .send()
                        .await
                    {
                        if e.is_connect() && !daemon_holds_lock(&db_path) {
                            println!("Nothing is listening on 127.0.0.1:34343 now -- either a daemon stopped as this ran, or there was never one to stop.");
                            return Ok(());
                        }
                        eprintln!("The stop request has not been acknowledged yet: {}. Waiting for the daemon to go anyway.", e);
                    }
                }

                // Wait for the process, not the port. The listener closes before
                // the final checkpoint runs, so a quiet port proves nothing; the
                // lock is released only once the process has exited. A daemon
                // that was already wedged gets longer, because engine waits of
                // two and a half minutes have been measured.
                let mut saw_lock = held;
                let attempts = if probe == DaemonProbe::Responsive {
                    900
                } else {
                    2400
                };
                for _ in 0..attempts {
                    if daemon_holds_lock(&db_path) {
                        saw_lock = true;
                    } else if saw_lock {
                        let report =
                            std::fs::read_to_string(daemon_lock_path(&db_path)).unwrap_or_default();
                        if report == FINAL_CHECKPOINT_OK {
                            println!("Daemon stopped and checkpointed.");
                            return Ok(());
                        }
                        if report.is_empty() {
                            eprintln!("The daemon exited without reporting its final checkpoint, so writes since the last one may not be on disk.");
                        } else {
                            eprintln!(
                                "The daemon exited, but its final checkpoint did not complete: {}",
                                report
                            );
                        }
                        std::process::exit(1);
                    } else if let Err(e) = client
                        .get("http://127.0.0.1:34343/health")
                        .timeout(std::time::Duration::from_millis(500))
                        .send()
                        .await
                    {
                        // No lock was ever seen: a daemon built before the lock
                        // existed. Only a refused connection counts as it having
                        // gone, and its checkpoint cannot be confirmed.
                        if e.is_connect() {
                            println!("Daemon stopped. It predates the shutdown lock, so its final checkpoint cannot be confirmed from here.");
                            return Ok(());
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                eprintln!(
                    "The daemon was still listening after {} seconds. It is probably still finishing a database operation -- leave it, and do not kill it: writes since the last checkpoint would be lost.",
                    attempts / 10
                );
                std::process::exit(1);
            }
            Commands::BusMetrics => {
                // Answered from the daemon, never from a bus built here: the
                // pulses are emitted by the daemon that owns the store, so a
                // local bus reports zeros that read as "an idle bus" rather
                // than "there is no bus". `probe` was taken before the
                // database was touched, so this never opens the store.
                let exit = run_bus_metrics(
                    daemon_running,
                    DAEMON_ADDR,
                    &mut std::io::stdout(),
                    &mut std::io::stderr(),
                )
                .await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Commands::Status => {
                let config = Config::from_default_path()?;
                let probe = probe_daemon().await;
                let held = daemon_holds_lock(&config.db_path);
                let size = std::fs::metadata(&config.db_path).map(|m| m.len()).ok();
                println!("store: {:?}", config.db_path);
                match size {
                    Some(bytes) => println!("store size: {} bytes", bytes),
                    None => println!("store size: (no file yet)"),
                }
                match probe {
                    DaemonProbe::Responsive => {
                        println!("daemon: listening on 127.0.0.1:34343, answering");
                        println!("lock: held");
                        // Runtime skew (round 3): the answering daemon may be
                        // running an OLDER build than the binary on disk -- the
                        // guinea pig re-verified a shipped fix through a
                        // pre-fix daemon and filed a false "still open".
                        let own = daemon::exe_fingerprint();
                        match skew_verdict(fetch_daemon_info().await.as_ref(), own) {
                            Skew::InSync(v) => {
                                println!(
                                    "build: in sync with this binary ({}, hash {:016x})",
                                    v, own.0
                                );
                                println!("status: healthy -- one daemon is serving every console");
                                std::process::exit(0);
                            }
                            Skew::Stale(detail) => {
                                println!("build: STALE -- {}", detail);
                                println!("skew: every tool and schema you see through the daemon belongs to that older build");
                                println!("skew: fix: neurostrata-mcp shutdown, then neurostrata-mcp daemon (restart any session holding an old stdio server)");
                                println!("status: stale -- answering, but not the build on disk");
                                std::process::exit(3);
                            }
                        }
                    }
                    _ if held => {
                        println!("daemon: NOT answering, but the store lock is held");
                        println!("status: busy -- a daemon is still finishing. Do NOT start another; wait, or run neurostrata-mcp shutdown");
                        std::process::exit(2);
                    }
                    _ => {
                        println!("daemon: not running");
                        println!("lock: free");
                        println!(
                            "status: down -- safe to start exactly one: neurostrata-mcp daemon"
                        );
                        std::process::exit(1);
                    }
                }
            }
            Commands::Backup { dir } => {
                // The database is single-writer. When a daemon holds it, ask the
                // daemon to do the work rather than fighting it for the lock.
                if daemon_running {
                    // /backup is destructive-class: bearer + operator (DR-5).
                    let res = daemon_operator_client()
                        .post("http://127.0.0.1:34343/backup")
                        .json(&serde_json::json!({ "dir": dir }))
                        .send()
                        .await?;
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    if !status.is_success() {
                        eprintln!("Backup failed: {}", body);
                        std::process::exit(1);
                    }
                    println!("{}", body);
                    return Ok(());
                }

                let config = Config::from_default_path()?;
                if daemon_busy(probe, daemon_holds_lock(&config.db_path)) {
                    eprintln!("{}", DAEMON_BUSY_MESSAGE);
                    std::process::exit(1);
                }
                // The width opens the store; exporting never embeds anything.
                let vector_store: Arc<dyn VectorStore> =
                    Arc::new(LadybugStore::new_with_identity_limits(
                        config.db_path.to_string_lossy().to_string(),
                        embed::configured_dimensions()?,
                        embed::configured_embedder_id()?,
                        profiled_store_limits(&config)?,
                    )?);
                vector_store.export_database(&dir).await?;
                println!("Backed up to {}", dir);
                return Ok(());
            }
            Commands::Restore { dir, into } => {
                let config = Config::from_default_path()?;
                let target = into
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| config.db_path.clone());

                if target.exists() {
                    eprintln!("{:?} already exists, and restoring into it would mean replacing what it holds.", target);
                    eprintln!("Restore into a new path instead: neurostrata-mcp restore <backup-dir> --into <new-db-path>");
                    eprintln!("Then point db_path in ~/.config/neurostrata/config.json at it once you have checked it.");
                    std::process::exit(1);
                }
                let namespaces =
                    restore_snapshot_bundle(std::path::Path::new(&dir), &target, &config).await?;
                println!("Restored {} into {:?}", dir, target);
                if !namespaces.is_empty() {
                    println!(
                        "It holds {} namespace(s): {}",
                        namespaces.len(),
                        namespaces.join(", ")
                    );
                }
                if target != config.db_path {
                    println!(
                        "To use it, set db_path in ~/.config/neurostrata/config.json to {:?}",
                        target
                    );
                }
                return Ok(());
            }
            Commands::Task { action } => match action {
                // Read-only, so it never needs the mutating-DB refusal: ask a
                // live daemon, else open the database metadata-only (5.1).
                TaskCommands::Gate {
                    namespace,
                    strict,
                    self_test,
                } => {
                    // Q4.3 of docs/design-wiring-panel.md: the gate self-tests
                    // itself against a synthetic in-memory fixture so it can
                    // prove it is still a gate. No daemon, no lock, no DB --
                    // short-circuit BEFORE any probe or lock check.
                    if self_test {
                        let report = crate::task::gate::self_test();
                        let json = report.to_json();
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&json).unwrap_or_default()
                        );
                        if report.passed {
                            println!(
                                "Task gate self-test PASS -- engine produces exactly {:?} ({} violation(s) on {} planted task(s)).",
                                report.expected_kinds,
                                report.total_violations,
                                report.counts.total
                            );
                            return Ok(());
                        } else {
                            eprintln!(
                                "Task gate self-test FAIL.\n  expected: {:?}\n  actual:   {:?}\n  missing:  {:?} (rules the gate silently dropped)\n  extra:    {:?} (rules the gate silently added)",
                                report.expected_kinds,
                                report.actual_kinds,
                                report.missing,
                                report.extra
                            );
                            std::process::exit(1);
                        }
                    }

                    // Both policies print the escape hatch (section 5.1).
                    if skip_gate_value_skips(
                        std::env::var("NEUROSTRATA_SKIP_GATE").ok().as_deref(),
                    ) {
                        eprintln!(
                            "[neurostrata:gate] NEUROSTRATA_SKIP_GATE=1 is set; skipping the gate for '{}'.",
                            namespace
                        );
                        println!(
                            "Task gate: skipped in '{}' (NEUROSTRATA_SKIP_GATE=1).",
                            namespace
                        );
                        return Ok(());
                    }

                    let outcome: Result<serde_json::Value, String> = if probe
                        == DaemonProbe::Responsive
                    {
                        // Identity before trust (Task 1, J9): verify the
                        // answering process is the expected daemon build
                        // via /info before sending the gate request or
                        // believing its answer -- a squatter on the port
                        // answers /health too, and would harvest the
                        // bearer token the gate request carries.
                        match gate_verify_daemon(
                            fetch_daemon_info().await.as_ref(),
                            daemon::exe_fingerprint(),
                        ) {
                            Err(e) => Err(e),
                            Ok(()) => match daemon_http_client()
                                .post("http://127.0.0.1:34343/tasks/gate")
                                .json(&serde_json::json!({
                                    "namespace": namespace,
                                    "strict": strict,
                                }))
                                .timeout(std::time::Duration::from_secs(10))
                                .send()
                                .await
                            {
                                Ok(resp) if resp.status().is_success() => {
                                    match resp.text().await {
                                        Ok(body) => {
                                            serde_json::from_str::<serde_json::Value>(&body)
                                                .map_err(|e| {
                                                    format!(
                                                        "the daemon's gate reply could not be read: {}",
                                                        e
                                                    )
                                                })
                                        }
                                        Err(e) => Err(format!(
                                            "the daemon's gate reply could not be read: {}",
                                            e
                                        )),
                                    }
                                }
                                Ok(resp) => Err(format!(
                                    "the daemon answered {} on /tasks/gate",
                                    resp.status()
                                )),
                                Err(e) => {
                                    Err(format!("the request to the daemon failed: {}", e))
                                }
                            },
                        }
                    } else {
                        match Config::from_default_path() {
                            Err(e) => Err(format!("the config could not be read: {}", e)),
                            Ok(config) => {
                                if daemon_busy(probe, daemon_holds_lock(&config.db_path)) {
                                    Err(DAEMON_BUSY_MESSAGE.to_string())
                                } else if probe == DaemonProbe::Silent {
                                    Err("nothing answered on 127.0.0.1:34343 within 500ms, so a daemon may be busy in the database; not opening it".to_string())
                                } else {
                                    let cwd = std::env::current_dir().unwrap_or_default();
                                    let line = crate::task::hook_hygiene(&cwd);
                                    if !line.is_empty() {
                                        println!("{}", line);
                                    }
                                    direct_report(&config, &namespace)
                                        .await
                                        .map(|report| report.gate_json())
                                }
                            }
                        }
                    };

                    match outcome {
                        Ok(body) => {
                            let violations = body
                                .get("violations")
                                .and_then(|v| v.as_array())
                                .cloned()
                                .unwrap_or_default();
                            for violation in &violations {
                                println!(
                                    "- [{}] {}: {}",
                                    violation
                                        .get("kind")
                                        .and_then(|k| k.as_str())
                                        .unwrap_or("?"),
                                    violation.get("id").and_then(|i| i.as_str()).unwrap_or("?"),
                                    violation
                                        .get("detail")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("")
                                );
                            }
                            if !violations.is_empty() {
                                println!();
                            }
                            let ok = violations.is_empty();
                            if ok {
                                println!("Task gate: clean in '{}'.", namespace);
                            } else {
                                println!(
                                    "Task gate: {} violation(s) in '{}'. Fix them (or claim and finish them), then push again.",
                                    violations.len(),
                                    namespace
                                );
                            }
                            std::process::exit(gate_exit_code(Some(ok), strict));
                        }
                        Err(e) => {
                            eprintln!(
                                "[neurostrata:gate] the task gate could not run for '{}': {}",
                                namespace, e
                            );
                            eprintln!(
                                "[neurostrata:gate] {}",
                                if strict {
                                    "--strict: an unavailable database blocks the push. Skip explicitly with NEUROSTRATA_SKIP_GATE=1 git push."
                                } else {
                                    "continuing anyway: an unavailable database is an ops problem, not agent misconduct. Set NEUROSTRATA_SKIP_GATE=1 to silence this, or pass --strict to block."
                                }
                            );
                            std::process::exit(gate_exit_code(None, strict));
                        }
                    }
                }

                // Advisory twin of the gate (section 4): always exits 0.
                TaskCommands::Validate { namespace } => {
                    let fetched = if probe == DaemonProbe::Responsive {
                        match daemon_http_client()
                            .post("http://127.0.0.1:34343/mcp")
                            .json(&serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "tools/call",
                                "params": {
                                    "name": "neurostrata_task_validate",
                                    "arguments": { "namespace": namespace },
                                },
                            }))
                            .timeout(std::time::Duration::from_secs(10))
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                resp.text().await.ok().and_then(|body| {
                                    serde_json::from_str::<serde_json::Value>(&body)
                                        .ok()
                                        .and_then(|v| {
                                            v.pointer("/result/content/0/text")
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                })
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    match fetched {
                        Some(report) => println!("{}", report),
                        None => match Config::from_default_path() {
                            Err(e) => {
                                eprintln!("[neurostrata:gate] the report for '{}' could not be fetched: {}", namespace, e);
                            }
                            Ok(config)
                                if daemon_busy(probe, daemon_holds_lock(&config.db_path))
                                    || probe == DaemonProbe::Silent =>
                            {
                                eprintln!(
                                    "[neurostrata:gate] the report for '{}' could not be fetched: {}",
                                    namespace, DAEMON_BUSY_MESSAGE
                                );
                            }
                            Ok(config) => match direct_report(&config, &namespace).await {
                                Ok(report) => {
                                    println!(
                                        "{}",
                                        serde_json::to_string_pretty(&report.validate_json())
                                            .unwrap_or_else(|_| "{}".to_string())
                                    );
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[neurostrata:gate] the report for '{}' could not be fetched: {}",
                                        namespace, e
                                    );
                                }
                            },
                        },
                    }
                    // Always exit 0: the report informs, it never blocks.
                    return Ok(());
                }

                // A store write the daemon can do on our behalf (guinea-pig
                // BUG-5): tearing down the shared daemon for a migration is
                // what pushed an agent toward forcing its own instances. The
                // direct path stays for when no daemon runs at all.
                TaskCommands::Import {
                    namespace,
                    from_beads,
                } => {
                    let summary_json: serde_json::Value = if daemon_running {
                        let res = daemon_http_client()
                            .post("http://127.0.0.1:34343/tasks/import")
                            .json(&serde_json::json!({ "namespace": namespace, "from_beads": from_beads }))
                            .send()
                            .await?;
                        let status = res.status();
                        let body = res.text().await.unwrap_or_default();
                        if !status.is_success() {
                            eprintln!("Import failed: {}", body);
                            std::process::exit(1);
                        }
                        serde_json::from_str(&body).unwrap_or_default()
                    } else {
                        let config = Config::from_default_path()?;
                        if daemon_busy(probe, daemon_holds_lock(&config.db_path)) {
                            eprintln!("{}", DAEMON_BUSY_MESSAGE);
                            std::process::exit(1);
                        }
                        let embedder = build_embedder()?;
                        let vector_store: Arc<dyn VectorStore> =
                            Arc::new(LadybugStore::new_with_identity_limits(
                                config.db_path.to_string_lossy().to_string(),
                                embedder.dimensions(),
                                embedder.embedder_id().to_string(),
                                profiled_store_limits(&config)?,
                            )?);
                        match crate::task::import_beads(
                            vector_store,
                            embedder.clone(),
                            &namespace,
                            &from_beads,
                        )
                        .await
                        {
                            Ok(summary) => serde_json::to_value(&summary).unwrap_or_default(),
                            Err(e) => {
                                eprintln!("{}", e);
                                std::process::exit(1);
                            }
                        }
                    };
                    let get = |k: &str| summary_json.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                    println!(
                        "Imported {} new task(s) into '{}'; skipped {} already imported; {} non-issue line(s), {} malformed line(s).",
                        get("created"),
                        namespace,
                        get("skipped_existing"),
                        get("non_issue_lines"),
                        get("malformed_lines")
                    );
                    if get("created") > 0 {
                        println!(
                            "Check the migration with: neurostrata-mcp task validate '{}'",
                            namespace
                        );
                    }
                    return Ok(());
                }
            },
            Commands::Setup { repo_path } => {
                let repo = match repo_path {
                    Some(p) => p,
                    None => {
                        std::env::current_dir().with_context(|| "cannot determine current_dir")?
                    }
                };
                match setup::run_setup(&repo) {
                    Ok(report) => {
                        if report.verified {
                            println!("Setup complete. Daemon healthy.");
                        } else if report.daemon_installed {
                            println!("Setup complete. Daemon installed but did not become healthy — see notes above.");
                        } else {
                            println!("Setup complete (manual steps required for this platform).");
                        }
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!("setup failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::Hooks { action } => match action {
                HooksCommands::Install { force } => {
                    let root = git_toplevel()
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap();
                    if !root.join(".git").exists() {
                        eprintln!(
                            "ERROR: {:?} is not a git repository; there is nowhere to install the pre-push hook.",
                            root
                        );
                        std::process::exit(1);
                    }
                    match install_pre_push_hook(&root, force) {
                        Ok(target) => {
                            println!(
                                "Installed the NeuroStrata task gate at {}.",
                                target.display()
                            );
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("{}", e);
                            std::process::exit(1);
                        }
                    }
                }
                HooksCommands::InstallPreCommit => {
                    let root = git_toplevel()
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap();
                    if !root.join(".git").exists() {
                        eprintln!(
                            "ERROR: {:?} is not a git repository; there is nowhere to install the pre-commit hook.",
                            root
                        );
                        std::process::exit(1);
                    }
                    match install_pre_commit_hook(&root) {
                        Ok(target) => {
                            println!(
                                "Installed the NeuroStrata pre-commit graph ledger at {}.",
                                target.display()
                            );
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("{}", e);
                            std::process::exit(1);
                        }
                    }
                }
            },
            other => {
                let graph_target = if let Commands::ExportGraph {
                    out_path,
                    source_id,
                    ..
                } = &other
                {
                    let source_config = source_id
                        .as_ref()
                        .map(|_| Config::from_default_path())
                        .transpose()?;
                    let empty_contexts = std::collections::HashMap::new();
                    let contexts = source_config
                        .as_ref()
                        .map(|config| &config.project_contexts)
                        .unwrap_or(&empty_contexts);
                    let project_root =
                        std::env::var_os("NEUROSTRATA_PROJECT_ROOT").map(std::path::PathBuf::from);
                    let invocation_dir = std::env::current_dir()?;
                    Some(crate::source_context::resolve_graph_output_path(
                        out_path.as_deref().map(std::path::Path::new),
                        source_id.as_deref(),
                        contexts,
                        project_root.as_deref(),
                        &invocation_dir,
                    )?)
                } else {
                    None
                };

                if daemon_running {
                    // Read-only graph export is not a store mutation: it rides
                    // the daemon like backup does, instead of demanding a
                    // shutdown (the same trap as guinea-pig BUG-5).
                    if let Commands::ExportGraph {
                        exclude_superseded,
                        include_archived,
                        ..
                    } = &other
                    {
                        let target_path = graph_target
                            .as_ref()
                            .expect("export graph target resolved before daemon dispatch");
                        let anchor_root = std::env::var_os("NEUROSTRATA_ANCHOR_ROOT")
                            .map(std::path::PathBuf::from);
                        let source_id = std::env::var("NEUROSTRATA_SOURCE_ID").ok();
                        let anchor_override = crate::source_context::scoped_anchor_override(
                            anchor_root.as_deref(),
                            source_id.as_deref(),
                        );
                        let mut url = reqwest::Url::parse("http://127.0.0.1:34343/graph")?;
                        {
                            let mut query = url.query_pairs_mut();
                            query
                                .append_pair(
                                    "include_superseded",
                                    &(!exclude_superseded).to_string(),
                                )
                                .append_pair("include_archived", &include_archived.to_string())
                                .append_pair("all", "true");
                            if let Some(anchor) = anchor_override.as_ref() {
                                query
                                    .append_pair(
                                        "anchor_root",
                                        &anchor.project_root.to_string_lossy(),
                                    )
                                    .append_pair("source_id", &anchor.source_id);
                            }
                        }
                        let res = daemon_http_client().get(url).send().await?;
                        let status = res.status();
                        let body = res.text().await.unwrap_or_default();
                        if !status.is_success() {
                            eprintln!("Graph export failed: {}", body);
                            std::process::exit(1);
                        }
                        println!("Exporting Memory Graph to {}", target_path.display());
                        if let Some(parent) = target_path
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                        {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(target_path, body)?;
                        println!("Graph exported successfully.");
                        return Ok(());
                    }
                    // Read-only tools run against the daemon's store without
                    // opening an engine here (guinea-pig BUG-8): doctor --help
                    // says "changing nothing", and the recovery moment is
                    // exactly when the daemon is up and least safe to stop.
                    let read = ReadSource::Daemon;
                    match &other {
                        Commands::Doctor { namespace } => {
                            run_doctor(&read, namespace.as_deref()).await?;
                            return Ok(());
                        }
                        Commands::Namespaces => {
                            run_namespaces(&read).await?;
                            return Ok(());
                        }
                        Commands::List { namespace } => {
                            run_list(&read, namespace).await?;
                            return Ok(());
                        }
                        _ => {}
                    }
                    eprintln!("CRITICAL ERROR: The NeuroStrata daemon is currently running (likely via OpenCode) and holds the database lock.");
                    eprintln!("You cannot run database-modifying CLI commands while the daemon is active.");
                    eprintln!("Run `neurostrata-mcp shutdown` to stop it safely -- killing the process discards any writes made since the last checkpoint.");
                    std::process::exit(1);
                }

                let config = Config::from_default_path()?;
                if daemon_busy(probe, daemon_holds_lock(&config.db_path)) {
                    eprintln!("{}", DAEMON_BUSY_MESSAGE);
                    std::process::exit(1);
                }
                let startup = resolve_startup(&config)?;
                let embedder = embed::build_embedder_for(&startup.resolution.model)?;
                let vector_store: Arc<dyn VectorStore> =
                    Arc::new(LadybugStore::new_with_identity_limits(
                        config.db_path.to_string_lossy().to_string(),
                        embedder.dimensions(),
                        embedder.embedder_id().to_string(),
                        startup.limits,
                    )?);
                let read = ReadSource::Direct(vector_store.clone());

                // CLI fallback: hook hygiene against the CWD git root (daemon down).
                let cwd = std::env::current_dir().unwrap_or_default();
                let line = crate::task::hook_hygiene(&cwd);
                if !line.is_empty() {
                    println!("{}", line);
                }

                match other {
                    Commands::Doctor { namespace } => {
                        run_doctor(&read, namespace.as_deref()).await?;
                    }
                    Commands::Laya { model, out_dir } => {
                        let model_id = model.unwrap_or_else(|| "v1.11.1".to_string());
                        let target_dir = out_dir.unwrap_or_else(|| "artifacts/laya-bundle".to_string());
                        let _ = crate::judgment::laya_onnx::LayaOnnxProvider::fetch_model(
                            &model_id,
                            &target_dir,
                        )
                        .await;
                    }
                    Commands::Namespaces => {
                        run_namespaces(&read).await?;
                    }
                    Commands::List { namespace } => {
                        run_list(&read, &namespace).await?;
                    }
                    // Reached only through the fallback; the dedicated Status
                    // arm exits before the store opens. Reported faithfully
                    // anyway: the daemon is provably not running in this arm.
                    Commands::Status => {
                        println!("store: {:?}", config.db_path);
                        println!("daemon: not running");
                        println!(
                            "lock: {}",
                            if daemon_holds_lock(&config.db_path) {
                                "held"
                            } else {
                                "free"
                            }
                        );
                        println!(
                            "status: down -- safe to start exactly one: neurostrata-mcp daemon"
                        );
                        std::process::exit(1);
                    }
                    Commands::Ingest {
                        dir,
                        namespace,
                        schema_path,
                    } => {
                        let dir = cli_ingest_root(&dir);
                        let dir_path = std::path::Path::new(&dir);
                        let schema_str = if let Some(path) = schema_path {
                            std::fs::read_to_string(&path).unwrap_or_else(|e| {
                                eprintln!("Failed to read schema from {}: {}", path, e);
                                std::process::exit(1);
                            })
                        } else {
                            include_str!("schema.json").to_string()
                        };

                        if let Ok(schema) = crate::parser::schema::ParserSchema::load(&schema_str) {
                            println!(
                                "Ingesting AST from {:?} into namespace '{}'",
                                dir_path, namespace
                            );
                            crate::parser::ingest::ingest_directory(
                                dir_path,
                                &schema,
                                embedder,
                                vector_store,
                                &namespace,
                                // The CLI has nowhere to report progress to: it
                                // is already printing each file as it goes.
                                None,
                            )
                            .await?;
                            println!("Ingestion complete.");
                        }
                    }
                    Commands::ExportGraph {
                        exclude_superseded,
                        include_archived,
                        ..
                    } => {
                        let target_path = graph_target
                            .as_ref()
                            .expect("export graph target resolved before direct dispatch");
                        println!("Exporting Memory Graph to {}", target_path.display());
                        if let Some(parent) = target_path
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                        {
                            std::fs::create_dir_all(parent)?;
                        }
                        vector_store.init("global").await?;
                        // A4 wire: refuse to export a stale graph. The
                        // `export_freshness_dirty` subscriber writes one
                        // sentinel row per namespace it sees mutate; this
                        // reads them and exits 1 if any is still dirty. An
                        // operator who has confirmed the export is current
                        // can flip the flag's `dirty` field to false; the
                        // gate trusts the flag, not its own clock.
                        if let Err(why) =
                            crate::events::check_export_freshness(&*vector_store).await
                        {
                            eprintln!("{why}");
                            std::process::exit(1);
                        }
                        // Superseded rows are exported marked (`superseded`,
                        // `superseded_by`) unless --exclude-superseded drops them.
                        // Tombstoned rows are excluded unless --include-archived
                        // asks for them; the same default-with-escape-hatch shape
                        // the search and snapshot read surfaces use.
                        let anchor_root = std::env::var_os("NEUROSTRATA_ANCHOR_ROOT")
                            .map(std::path::PathBuf::from);
                        let source_id = std::env::var("NEUROSTRATA_SOURCE_ID").ok();
                        let anchor_override = crate::source_context::scoped_anchor_override(
                            anchor_root.as_deref(),
                            source_id.as_deref(),
                        );
                        let graph_data = vector_store
                            .export_graph(
                                !exclude_superseded,
                                include_archived,
                                &config.project_contexts,
                                anchor_override.as_ref(),
                            )
                            .await?;
                        std::fs::write(target_path, serde_json::to_string_pretty(&graph_data)?)?;
                        println!("Graph exported successfully.");
                    }
                    Commands::NormalizeMetadata { namespace } => {
                        use crate::store::ladybug::KnownIds;
                        let all = vector_store.list(&namespace, None, None).await?;
                        let ingested_ids: Vec<String> = all
                            .iter()
                            .filter(|m| m.payload.user_id == "auto-ingestor")
                            .map(|m| m.id.clone())
                            .collect();
                        let known = KnownIds::new(ingested_ids.iter().map(|s| s.as_str()));
                        let mut scanned = 0usize;
                        let mut updated = 0usize;
                        let mut skipped = 0usize;
                        for mem in &all {
                            if mem.payload.user_id == "auto-ingestor"
                                || mem.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
                            {
                                skipped += 1;
                                continue;
                            }
                            scanned += 1;
                            let (vector, mut payload) = match vector_store.get(&namespace, &mem.id).await? {
                                Some(v) => v,
                                None => continue,
                            };
                            let mut metadata = payload.metadata.clone();
                            if !metadata.is_object() {
                                continue;
                            }
                            let mut changed = false;
                            for key in ["related_to", "contained_by", "governs", "extracted_from"] {
                                if let Some(arr) = metadata.get(key).and_then(|v| v.as_array()).cloned() {
                                    let mut new_arr = Vec::with_capacity(arr.len());
                                    for item in arr {
                                        if let Some(s) = item.as_str() {
                                            if let Some(resolved) = known.resolve(s) {
                                                if resolved != s {
                                                    new_arr.push(serde_json::Value::String(resolved));
                                                    changed = true;
                                                    continue;
                                                }
                                            }
                                        }
                                        new_arr.push(item);
                                    }
                                    if changed {
                                        metadata[key] = serde_json::Value::Array(new_arr);
                                    }
                                }
                            }
                            if !changed {
                                continue;
                            }
                            payload.metadata = metadata;
                            vector_store.upsert(&namespace, &mem.id, vector, payload).await?;
                            updated += 1;
                        }
                        println!(
                            "Scanned {} memories, updated {} ({} skipped as ingested/code nodes).",
                            scanned, updated, skipped
                        );
                    }
                    Commands::Delete { namespace, id } => {
                        vector_store.delete(&namespace, &id).await?;
                        println!("Memory deleted successfully.");
                    }
                    Commands::Archive { namespace, id } => {
                        // The same handler `neurostrata_archive_memory` calls,
                        // so the CLI and the tool cannot disagree about what
                        // "archived" means. It needs a bus to announce the
                        // tombstone on, and a CLI archive only runs with no
                        // daemon (a running one holds the database, above) --
                        // so this bus has no subscribers and the pulse is
                        // counted by nobody. The write is what was asked for.
                        let bus = Arc::new(crate::events::ThalamicBus::new(daemon::BUS_CAPACITY));
                        let arguments = serde_json::json!({ "namespace": namespace, "id": id });
                        let outcome = crate::handlers::archive_memory::archive_memory(
                            arguments,
                            vector_store.clone(),
                            bus,
                        )
                        .await;
                        // The exit code comes from the variant, not from
                        // reading the handler's sentence for a prefix: a
                        // refusal printed on stderr must not report success to
                        // a script.
                        if outcome.is_archived() {
                            println!("{}", outcome);
                        } else {
                            eprintln!("{}", outcome);
                            std::process::exit(1);
                        }
                    }
                    Commands::Move {
                        source_namespace,
                        id,
                        target_namespace,
                    } => {
                        // One statement that changes the namespace. Copying to the
                        // target and deleting from the source removed the only copy,
                        // because upsert never rewrites a row's namespace.
                        match vector_store
                            .relocate(&id, &source_namespace, &target_namespace)
                            .await?
                        {
                            crate::traits::RelocateOutcome::Moved => {
                                println!(
                                    "Moved {} from '{}' to '{}'.",
                                    id, source_namespace, target_namespace
                                );
                            }
                            crate::traits::RelocateOutcome::SameNamespace => {
                                println!("{} is already in '{}'.", id, source_namespace);
                            }
                            crate::traits::RelocateOutcome::NotFound => {
                                eprintln!(
                                    "No memory with id {} in namespace {}.",
                                    id, source_namespace
                                );
                                std::process::exit(1);
                            }
                            crate::traits::RelocateOutcome::Ingested => {
                                eprintln!(
                                    "{} was written by directory ingestion for '{}' and cannot be moved; ingest the directory into '{}' instead.",
                                    id, source_namespace, target_namespace
                                );
                                std::process::exit(1);
                            }
                        }
                    }
                    Commands::Add {
                        namespace,
                        memory_type,
                        content,
                        location,
                    } => {
                        let vector = embedder.embed(&content).await?;
                        let payload = crate::traits::MemoryPayload {
                            content: content.clone(),
                            memory_type: memory_type.clone(),
                            location: location.unwrap_or_default(),
                            user_id: "system".to_string(),
                            agent_name: Some("NeuroStrata".to_string()),
                            location_lines: "".to_string(),
                            metadata: serde_json::json!({}),
                        };
                        let id = uuid::Uuid::new_v4().to_string();
                        vector_store
                            .upsert(&namespace, &id, vector, payload)
                            .await?;
                        println!("Memory added successfully with ID: {}", id);
                    }
                    Commands::Edit {
                        namespace,
                        id,
                        new_namespace,
                        content,
                        location,
                    } => {
                        match crate::server::edit_memory(
                            &*vector_store,
                            &*embedder,
                            &namespace,
                            &id,
                            &new_namespace,
                            &content,
                            &location,
                        )
                        .await?
                        {
                            crate::server::EditOutcome::Edited => {
                                println!("Successfully edited memory {}", id)
                            }
                            crate::server::EditOutcome::NotFound => {
                                eprintln!("No memory with id {} in namespace {}.", id, namespace);
                                std::process::exit(1);
                            }
                            crate::server::EditOutcome::Ingested => {
                                eprintln!(
                                    "{} was written by directory ingestion, so the next ingest would overwrite an edit. Change the file and ingest again instead.",
                                    id
                                );
                                std::process::exit(1);
                            }
                        }
                    }
                    Commands::SetMetadata {
                        namespace,
                        id,
                        metadata,
                    } => {
                        let new_metadata: serde_json::Value = serde_json::from_str(&metadata)
                            .map_err(|e| anyhow::anyhow!("Invalid JSON metadata: {}", e))?;
                        match crate::server::set_metadata(
                            &*vector_store,
                            &namespace,
                            &id,
                            new_metadata,
                        )
                        .await?
                        {
                            crate::server::MetadataOutcome::Updated => {
                                println!("Successfully updated metadata for memory {}", id)
                            }
                            crate::server::MetadataOutcome::NotFound => {
                                eprintln!("No memory with id {} in namespace {}.", id, namespace);
                                std::process::exit(1);
                            }
                            crate::server::MetadataOutcome::Ingested => {
                                eprintln!(
                                    "{} was written by directory ingestion and cannot be updated.",
                                    id
                                );
                                std::process::exit(1);
                            }
                        }
                    }
                    // Handled above, before the database is ever opened.
                    Commands::Daemon
                    | Commands::Shutdown
                    | Commands::Run { .. }
                    | Commands::SynapticMesh { .. }
                    | Commands::Backup { .. }
                    | Commands::Restore { .. }
                    | Commands::Task { .. }
                    | Commands::Setup { .. }
                    | Commands::ProjectContext { .. }
                    | Commands::BusMetrics
                    | Commands::Hooks { .. } => unreachable!(),
                }
            }
        }
    }

    Ok(())
}
