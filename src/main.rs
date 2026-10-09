mod buffer;
mod config;
mod daemon;
mod embed;
mod guard;
mod ingest_jobs;
mod judgment;
mod parser;
mod secrets;
mod server;
mod store;
mod task;
mod traits;

use config::Config;
use std::sync::Arc;
use crate::traits::SearchResult;
use store::LadybugStore;
use crate::traits::{Embedder, VectorStore};
use embed::build_embedder;
use clap::{Parser, Subcommand};

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

        /// Leave superseded (retired) memories out of the export entirely
        #[arg(long)]
        exclude_superseded: bool,
    },

    /// Delete a memory from a namespace by ID
    Delete {
        /// The target namespace
        namespace: String,

        /// The memory ID to delete
        id: String,
    },

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

    /// Task subsystem: the gate behind the pre-push hook (section 5)
    Task {
        #[command(subcommand)]
        action: TaskCommands,
    },

    /// Git hook management: installs the one hook that exists
    Hooks {
        #[command(subcommand)]
        action: HooksCommands,
    },
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
    /// Write .git/hooks/pre-push: the only hook the task system keeps
    Install {
        /// Replace a pre-push hook NeuroStrata did not write
        #[arg(long)]
        force: bool,
    },
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
        None => {
            return Skew::Stale(
                "the daemon predates build verification (no /info), so it is older than this binary".to_string(),
            )
        }
    };
    let hash = info.get("exe_hash").and_then(|v| v.as_u64());
    let len = info.get("exe_len").and_then(|v| v.as_u64());
    let version = info.get("version").and_then(|v| v.as_str()).unwrap_or("unknown");
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

async fn fetch_daemon_info() -> Option<serde_json::Value> {
    let res = reqwest::Client::new()
        .get("http://127.0.0.1:34343/info")
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    res.json::<serde_json::Value>().await.ok()
}

async fn probe_daemon() -> DaemonProbe {
    match reqwest::Client::new()
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

/// The pre-push hook, section 5.2 verbatim. Installed by `hooks install`.
///
/// One hook is the whole enforcement surface: tasks live in LadybugDB, never
/// in the repo, so there is nothing to commit, sync, or rebuild. Blocks only
/// on exit 1 -- an unavailable database (exit 2) passes here; `task gate
/// --strict` maps its own infra failure to exit 1 instead of 2 (section 5.1).
const PRE_PUSH_HOOK: &str = r#"#!/bin/bash
# NeuroStrata task gate. One hook is the whole enforcement surface:
# tasks live in LadybugDB, never in the repo, so there is nothing to
# commit, sync, or rebuild.
command -v neurostrata-mcp >/dev/null || exit 0
[ -n "$NEUROSTRATA_SKIP_GATE" ] && exit 0

NAMESPACE=$(basename "$(git rev-parse --show-toplevel)")
neurostrata-mcp task gate "$NAMESPACE" --strict
code=$?
if [ $code -eq 1 ]; then
    echo "Push blocked: resolve the tasks above, or NEUROSTRATA_SKIP_GATE=1 git push" >&2
    exit 1
fi
exit 0   # 0 and 2 both pass here unless --strict flips 2 to blocking
"#;

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
    let dimensions = embed::configured_dimensions()
        .map_err(|e| format!("the configured embedding dimensions could not be read: {}", e))?;
    let store: Arc<dyn VectorStore> = Arc::new(
        LadybugStore::new(config.db_path.to_string_lossy().to_string(), dimensions)
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
                let res = reqwest::Client::new()
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
            ReadSource::Direct(s) => s.list(namespace, None).await,
            ReadSource::Daemon => {
                let res = reqwest::Client::new()
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
                                want, namespaces
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
                        let known = crate::store::ladybug::KnownIds::new(
                            memories.iter().map(|m| m.id.as_str()),
                        );

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

                            if memory.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
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
                        println!("  [{}] declared targets that match nothing ingested: {}", ns, missing.len());
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
                    println!("Found {} memories in namespace '{}':\n", results.len(), namespace);
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

    #[test]
    fn a_mistyped_subcommand_is_an_error_not_an_execution() {
        let parsed = Cli::try_parse_from(["neurostrata-mcp", "lsit"]);
        assert!(parsed.is_err(), "a typo must never reach std::process::Command");
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
        assert!(daemon_holds_lock(&db), "a live holder is visible to shutdown");
        assert!(take_daemon_lock(&db).is_err(), "a second daemon is refused");

        drop(held);
        assert!(!daemon_holds_lock(&db), "the lock is free once its holder is gone");
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
        assert!(daemon_busy(DaemonProbe::Absent, true), "listener closed, still checkpointing");
        assert!(!daemon_busy(DaemonProbe::Silent, false), "silence with no holder is no daemon");
        assert!(!daemon_busy(DaemonProbe::Responsive, true), "an answering daemon is asked, not refused");
    }

    /// Section 5.1: a verdict maps to 0/1; no verdict blocks only under
    /// --strict and passes (2) otherwise -- and 2 is what the hook tolerates.
    #[test]
    fn the_gate_exit_code_flips_only_under_strict() {
        assert_eq!(gate_exit_code(Some(true), false), 0);
        assert_eq!(gate_exit_code(Some(true), true), 0);
        assert_eq!(gate_exit_code(Some(false), false), 1);
        assert_eq!(gate_exit_code(Some(false), true), 1, "a verdict never changes with the flag");
        assert_eq!(gate_exit_code(None, false), 2, "infra warns and lets the push through");
        assert_eq!(
            gate_exit_code(None, true),
            1,
            "strict maps the infra failure to the one code the hook blocks on"
        );
    }

    /// Section 5.2 verbatim: blocks on violations only, skips on the escape
    /// hatch, and lets 0 and 2 pass.
    #[test]
    fn the_pre_push_hook_is_the_documented_script() {
        assert!(PRE_PUSH_HOOK.starts_with("#!/bin/bash\n"));
        assert!(PRE_PUSH_HOOK.contains("command -v neurostrata-mcp >/dev/null || exit 0"));
        assert!(PRE_PUSH_HOOK.contains("[ -n \"$NEUROSTRATA_SKIP_GATE\" ] && exit 0"));
        assert!(PRE_PUSH_HOOK
            .contains("NAMESPACE=$(basename \"$(git rev-parse --show-toplevel)\")"));
        assert!(PRE_PUSH_HOOK.contains("neurostrata-mcp task gate \"$NAMESPACE\" --strict"));
        assert!(PRE_PUSH_HOOK.contains("code=$?"));
        assert!(PRE_PUSH_HOOK.contains("if [ $code -eq 1 ]; then"));
        assert!(PRE_PUSH_HOOK.contains("NEUROSTRATA_SKIP_GATE=1 git push"));
        assert!(PRE_PUSH_HOOK.contains("exit 0   # 0 and 2 both pass here unless --strict flips 2 to blocking"));
        assert!(!PRE_PUSH_HOOK.contains("$code -eq 2"), "2 never blocks in the hook itself");
    }

    #[test]
    fn the_gate_and_hook_commands_parse_with_their_flags() {
        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "task",
            "gate",
            "NeuroStrata",
            "--strict",
        ])
        .expect("task gate parses");
        match cli.command {
            Some(Commands::Task {
                action: TaskCommands::Gate { namespace, strict },
            }) => {
                assert_eq!(namespace, "NeuroStrata");
                assert!(strict);
            }
            other => panic!("expected Task/Gate, got {:?}", other),
        }

        let cli = Cli::try_parse_from(["neurostrata-mcp", "task", "validate", "MyProj"])
            .expect("task validate parses");
        match cli.command {
            Some(Commands::Task {
                action: TaskCommands::Validate { namespace },
            }) => assert_eq!(namespace, "MyProj"),
            other => panic!("expected Task/Validate, got {:?}", other),
        }

        let cli =
            Cli::try_parse_from(["neurostrata-mcp", "task", "import", "MyProj", "--from-beads", ".beads/issues.jsonl"])
                .expect("task import parses");
        match cli.command {
            Some(Commands::Task {
                action: TaskCommands::Import { namespace, from_beads },
            }) => {
                assert_eq!(namespace, "MyProj");
                assert_eq!(from_beads, ".beads/issues.jsonl");
            }
            other => panic!("expected Task/Import, got {:?}", other),
        }

        let cli =
            Cli::try_parse_from(["neurostrata-mcp", "doctor", "--namespace", "MyProj"]).expect("doctor parses");
        match cli.command {
            Some(Commands::Doctor { namespace }) => {
                assert_eq!(namespace.as_deref(), Some("MyProj"))
            }
            other => panic!("expected Doctor, got {:?}", other),
        }
        let cli = Cli::try_parse_from(["neurostrata-mcp", "doctor"]).expect("doctor parses unscoped");
        match cli.command {
            Some(Commands::Doctor { namespace }) => assert!(namespace.is_none()),
            other => panic!("expected Doctor, got {:?}", other),
        }

        let cli =
            Cli::try_parse_from(["neurostrata-mcp", "hooks", "install", "--force"]).expect("hooks install parses");
        match cli.command {
            Some(Commands::Hooks {
                action: HooksCommands::Install { force },
            }) => assert!(force),
            other => panic!("expected Hooks/Install, got {:?}", other),
        }

        // The flag stays optional: plain `hooks install` is the common case.
        let cli =
            Cli::try_parse_from(["neurostrata-mcp", "hooks", "install"]).expect("hooks install parses bare");
        match cli.command {
            Some(Commands::Hooks {
                action: HooksCommands::Install { force },
            }) => assert!(!force),
            other => panic!("expected Hooks/Install, got {:?}", other),
        }
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

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
            eprintln!("NeuroStrata MCP Server initializing...");
            
            // Spawn the daemon as a detached process
            let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("neurostrata-mcp"));
            std::process::Command::new(exe)
                .arg("daemon")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            
            eprintln!("Waiting for daemon to become ready (this may take a moment while models load)...");
            
            // Wait for daemon to become ready. Bounded per attempt as well as
            // overall: an unbound port on this machine hangs rather than
            // refusing -- the behaviour DaemonProbe exists to describe -- so an
            // untimed send here could spend the whole "30 seconds max" inside
            // one call.
            let client = reqwest::Client::new();
            for _ in 0..300 { // 30 seconds max
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
    let cli = Cli::parse();

    if let Some(command) = cli.command {
        match command {
            Commands::Daemon => {
                println!("NeuroStrata MCP Server initializing in DAEMON-ONLY mode...");
                let config = Config::from_default_path()?;
                // Taken before the database opens. `shutdown` waits for this lock
                // to be released rather than for the port to close.
                let _ = DAEMON_LOCK.set(take_daemon_lock(&config.db_path)?);
                let embedder = build_embedder()?;
                let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embedder.dimensions(),
                )?);
                vector_store.init("global").await?;
                let outcome = daemon::start_daemon(
                    embedder,
                    vector_store,
                    Some(config.deduplication.clone()),
                ).await;
                record_final_checkpoint(&outcome);
                outcome?;
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

                let client = reqwest::Client::new();
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
                let attempts = if probe == DaemonProbe::Responsive { 900 } else { 2400 };
                for _ in 0..attempts {
                    if daemon_holds_lock(&db_path) {
                        saw_lock = true;
                    } else if saw_lock {
                        let report = std::fs::read_to_string(daemon_lock_path(&db_path)).unwrap_or_default();
                        if report == FINAL_CHECKPOINT_OK {
                            println!("Daemon stopped and checkpointed.");
                            return Ok(());
                        }
                        if report.is_empty() {
                            eprintln!("The daemon exited without reporting its final checkpoint, so writes since the last one may not be on disk.");
                        } else {
                            eprintln!("The daemon exited, but its final checkpoint did not complete: {}", report);
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
                                println!("build: in sync with this binary ({}, hash {:016x})", v, own.0);
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
                        println!("status: down -- safe to start exactly one: neurostrata-mcp daemon");
                        std::process::exit(1);
                    }
                }
            }
            Commands::Backup { dir } => {
                // The database is single-writer. When a daemon holds it, ask the
                // daemon to do the work rather than fighting it for the lock.
                if daemon_running {
                    let res = reqwest::Client::new()
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
                let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embed::configured_dimensions()?,
                )?);
                vector_store.export_database(&dir).await?;
                println!("Backed up to {}", dir);
                return Ok(());
            }
            Commands::Restore { dir, into } => {
                // A snapshot is a file copy, so restoring means copying it into
                // place BEFORE any engine opens it -- an open engine maps the
                // file it was created with, and copying under a live handle is
                // exactly the memory-unsafety class the backup bug came from.
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
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                let snapshot = std::path::Path::new(&dir).join("ladybug.store");
                if !snapshot.exists() {
                    eprintln!("{:?} does not look like a snapshot: no ladybug.store inside it.", dir);
                    std::process::exit(1);
                }
                std::fs::copy(&snapshot, &target)?;
                let mut wal = snapshot.clone().into_os_string();
                wal.push(".wal");
                let wal = std::path::PathBuf::from(wal);
                if wal.exists() {
                    let mut target_wal = target.clone().into_os_string();
                    target_wal.push(".wal");
                    std::fs::copy(&wal, std::path::PathBuf::from(target_wal))?;
                }

                // Opened only after the copy, to verify the restored store
                // reads back and to report what it holds.
                let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                    target.to_string_lossy().to_string(),
                    embed::configured_dimensions()?,
                )?);

                let namespaces = vector_store.list_namespaces().await.unwrap_or_default();
                println!("Restored {} into {:?}", dir, target);
                if !namespaces.is_empty() {
                    println!("It holds {} namespace(s): {}", namespaces.len(), namespaces.join(", "));
                }
                if target != config.db_path {
                    println!("To use it, set db_path in ~/.config/neurostrata/config.json to {:?}", target);
                }
                return Ok(());
            }
            Commands::Task { action } => match action {
                // Read-only, so it never needs the mutating-DB refusal: ask a
                // live daemon, else open the database metadata-only (5.1).
                TaskCommands::Gate { namespace, strict } => {
                    // Both policies print the escape hatch (section 5.1).
                    if std::env::var("NEUROSTRATA_SKIP_GATE")
                        .map(|v| !v.is_empty())
                        .unwrap_or(false)
                    {
                        eprintln!(
                            "[neurostrata:gate] NEUROSTRATA_SKIP_GATE is set; skipping the gate for '{}'.",
                            namespace
                        );
                        return Ok(());
                    }

                    let outcome: Result<serde_json::Value, String> =
                        if probe == DaemonProbe::Responsive {
                            match reqwest::Client::new()
                                .post("http://127.0.0.1:34343/tasks/gate")
                                .json(&serde_json::json!({
                                    "namespace": namespace,
                                    "strict": strict,
                                }))
                                .timeout(std::time::Duration::from_secs(10))
                                .send()
                                .await
                            {
                                Ok(resp) if resp.status().is_success() => match resp.text().await {
                                    Ok(body) => serde_json::from_str::<serde_json::Value>(&body)
                                        .map_err(|e| {
                                            format!(
                                                "the daemon's gate reply could not be read: {}",
                                                e
                                            )
                                        }),
                                    Err(e) => Err(format!(
                                        "the daemon's gate reply could not be read: {}",
                                        e
                                    )),
                                },
                                Ok(resp) => Err(format!(
                                    "the daemon answered {} on /tasks/gate",
                                    resp.status()
                                )),
                                Err(e) => Err(format!("the request to the daemon failed: {}", e)),
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
                                    violation
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("?"),
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
                        match reqwest::Client::new()
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
                            Ok(resp) if resp.status().is_success() => resp
                                .text()
                                .await
                                .ok()
                                .and_then(|body| {
                                    serde_json::from_str::<serde_json::Value>(&body)
                                        .ok()
                                        .and_then(|v| {
                                            v.pointer("/result/content/0/text")
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                }),
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
                        let res = reqwest::Client::new()
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
                        let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                            config.db_path.to_string_lossy().to_string(),
                            embedder.dimensions(),
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
                    let hooks_dir = root.join(".git/hooks");
                    if !hooks_dir.is_dir() {
                        std::fs::create_dir_all(&hooks_dir)?;
                    }
                    let target = hooks_dir.join("pre-push");
                    // Overwriting our own hook needs no flag: --force is for
                    // a pre-push somebody else wrote.
                    let ours = std::fs::read_to_string(&target)
                        .map(|content| content.contains("NeuroStrata task gate"))
                        .unwrap_or(false);
                    if target.exists() && !ours && !force {
                        eprintln!(
                            "{} already exists and was not written by NeuroStrata.",
                            target.display()
                        );
                        eprintln!("Re-run with --force to replace it with the NeuroStrata task gate:");
                        eprintln!("  neurostrata-mcp hooks install --force");
                        std::process::exit(1);
                    }
                    std::fs::write(&target, PRE_PUSH_HOOK)?;
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
                    }
                    println!("Installed the NeuroStrata task gate at {}.", target.display());
                    return Ok(());
                }
            },
            other => {
                if daemon_running {
                    // Read-only graph export is not a store mutation: it rides
                    // the daemon like backup does, instead of demanding a
                    // shutdown (the same trap as guinea-pig BUG-5).
                    if let Commands::ExportGraph {
                        out_path,
                        exclude_superseded,
                    } = &other
                    {
                        let default_path = ".NeuroStrata/graph/graph.json".to_string();
                        let target_path = out_path.as_ref().unwrap_or(&default_path);
                        let res = reqwest::Client::new()
                            .get(format!(
                                "http://127.0.0.1:34343/graph?include_superseded={}&all=true",
                                !exclude_superseded
                            ))
                            .send()
                            .await?;
                        let status = res.status();
                        let body = res.text().await.unwrap_or_default();
                        if !status.is_success() {
                            eprintln!("Graph export failed: {}", body);
                            std::process::exit(1);
                        }
                        println!("Exporting Memory Graph to {}", target_path);
                        if let Some(parent) = std::path::Path::new(target_path).parent() {
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
                let embedder = build_embedder()?;
                let vector_store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embedder.dimensions(),
                )?);
                let read = ReadSource::Direct(vector_store.clone());

                match other {
                    Commands::Doctor { namespace } => {
                        run_doctor(&read, namespace.as_deref()).await?;
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
                            if daemon_holds_lock(&config.db_path) { "held" } else { "free" }
                        );
                        println!("status: down -- safe to start exactly one: neurostrata-mcp daemon");
                        std::process::exit(1);
                    }
                    Commands::Ingest { dir, namespace, schema_path } => {
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
                            println!("Ingesting AST from {:?} into namespace '{}'", dir_path, namespace);
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
                    Commands::ExportGraph { out_path, exclude_superseded } => {
                        let default_path = ".NeuroStrata/graph/graph.json".to_string();
                        let target_path = out_path.as_ref().unwrap_or(&default_path);
                        println!("Exporting Memory Graph to {}", target_path);
                        if let Some(parent) = std::path::Path::new(target_path).parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        vector_store.init("global").await?;
                        // Superseded rows are exported marked (`superseded`,
                        // `superseded_by`) unless --exclude-superseded drops them.
                        let graph_data = vector_store.export_graph(!exclude_superseded).await?;
                        std::fs::write(target_path, serde_json::to_string_pretty(&graph_data)?)?;
                        println!("Graph exported successfully.");
                    }
                    Commands::Delete { namespace, id } => {
                        vector_store.delete(&namespace, &id).await?;
                        println!("Memory deleted successfully.");
                    }
                    Commands::Move { source_namespace, id, target_namespace } => {
                        // One statement that changes the namespace. Copying to the
                        // target and deleting from the source removed the only copy,
                        // because upsert never rewrites a row's namespace.
                        match vector_store.relocate(&id, &source_namespace, &target_namespace).await? {
                            crate::traits::RelocateOutcome::Moved => {
                                println!("Moved {} from '{}' to '{}'.", id, source_namespace, target_namespace);
                            }
                            crate::traits::RelocateOutcome::SameNamespace => {
                                println!("{} is already in '{}'.", id, source_namespace);
                            }
                            crate::traits::RelocateOutcome::NotFound => {
                                eprintln!("No memory with id {} in namespace {}.", id, source_namespace);
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
                    Commands::Add { namespace, memory_type, content, location } => {
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
                        vector_store.upsert(&namespace, &id, vector, payload).await?;
                        println!("Memory added successfully with ID: {}", id);
                    }
                    Commands::Edit { namespace, id, new_namespace, content, location } => {
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
                            crate::server::EditOutcome::Edited => println!("Successfully edited memory {}", id),
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
                    // Handled above, before the database is ever opened.
                    Commands::Daemon
                    | Commands::Shutdown
                    | Commands::Run { .. }
                    | Commands::Backup { .. }
                    | Commands::Restore { .. }
                    | Commands::Task { .. }
                    | Commands::Hooks { .. } => unreachable!(),
                }
            }
        }
    }

    Ok(())
}
