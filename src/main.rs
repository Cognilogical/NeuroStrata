// Increase recursion limit for large JSON literals in tools/list
#![recursion_limit = "512"]

mod attempt;
mod bench;
mod buffer;
mod community;
mod compression;
mod connectors;
mod config;
mod context;
mod crash;
mod daemon;
mod diagnostic;
mod did;
mod digital_twin;
mod doc_gate;
mod domain;
mod embed;
mod events;
mod formal_kernel;
mod kernel;
mod fts;
mod gliner;
mod graph_read;
mod graphql;
mod guard;
mod handlers;
mod ingest_jobs;
mod judgment;
mod kge;
mod learning;
mod lexical;
mod mention;
mod model_choice;
mod model_downloader;
mod multimodal;
mod ontology_grounding;
mod parser;
mod paths;
mod procmem;
mod rag_chat;
mod rdf;
mod rebac;
mod recall_metrics;
mod receipts;
mod registry;
mod retention;
mod search;
mod secrets;
mod server;
mod setup;
mod source_context;
mod storage;
mod store;
mod streaming;
mod synaptic_mesh;
mod task;
mod telemetry;
mod time_series;
mod tools;
mod traits;
mod workflow;

use crate::traits::SearchResult;
use crate::traits::VectorStore;
use anyhow::Context;
use clap::{Parser, Subcommand};
use config::Config;
use embed::build_embedder;
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
    Daemon {
        /// Record the memory growth log and sampler (`~/.config/neurostrata/memory-growth.log`).
        /// Off by default; `NEUROSTRATA_PROFILE=1` does the same for a daemon an IDE starts.
        #[arg(long)]
        profile: bool,
    },

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

        /// Preview ingestion without writing to the store or downloading models
        #[arg(long)]
        dry_run: bool,

        /// Dry-run mode: "estimate" or "presort"
        #[arg(long, default_value = "estimate")]
        dry_run_mode: String,
    },

    /// Ingest a compiler-produced SCIP protobuf index (.scip) into a namespace
    #[command(name = "ingest-scip")]
    IngestScip {
        /// Path to the .scip protobuf index file
        scip_path: String,

        /// The target namespace
        namespace: String,
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

        /// Clear export-freshness dirty flags before exporting
        #[arg(long)]
        clear: bool,
    },

    /// Repair namespace hygiene (source, dangling refs, endpoint mismatches)
    RepairHygiene {
        /// The namespace to repair (optional when --all is passed)
        namespace: Option<String>,

        /// Repair all namespaces in the database
        #[arg(long)]
        all: bool,
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

    /// Run the longitudinal benchmark suite and emit a JSON performance receipt
    Bench {
        /// Benchmark suite to run
        #[arg(long, default_value = "all")]
        suite: crate::bench::BenchSuite,
        /// Number of iterations per suite
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        /// Optional path to write the pretty JSON receipt
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Optional path to a previous receipt for regression comparison
        #[arg(long)]
        baseline: Option<std::path::PathBuf>,
        /// Maximum allowed regression percentage
        #[arg(long, default_value_t = 20.0)]
        max_regression_pct: f64,
        /// Exit non-zero when regression exceeds threshold
        #[arg(long)]
        fail_on_regression: bool,
        /// Optional namespace to record latency samples into TimeSeriesStore
        #[arg(long)]
        record_metrics: Option<String>,
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

        /// Copy the database and WAL files directly without opening or checkpointing the store
        #[arg(long)]
        no_open: bool,
    },

    /// Report daemon and store state without touching anything
    ///
    /// Exit 0: a healthy daemon is serving. Exit 1: no daemon and the lock is
    /// free -- safe to start one. Exit 2: the lock is held but nothing answers;
    /// a daemon is busy or finishing, so do not start another. Exit 3: the
    /// daemon is running an older/stale build (its probe handshake did not
    /// match), so it should be shut down and restarted.
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

    /// Knowledge-graph embeddings: train TransE structural channel over
    /// GOVERNS/CONTAINS/RELATES_TO/DEPENDS_ON/EXTRACTED_FROM 5 edge types.
    /// Embeddings feed a capped secondary term into retrieval (≤15%) and power
    /// review-only duplicate-candidate ranking (`neurostrata_find_duplicate_candidates`).
    #[command(name = "kge")]
    Kge {
        #[command(subcommand)]
        action: KgeCommands,
    },

    /// Storage hygiene, snapshot checksums, and WAL status
    #[command(name = "storage")]
    Storage {
        #[command(subcommand)]
        action: StorageCommands,
    },

    /// Local-first RAG chat over project memories
    #[command(name = "chat")]
    Chat {
        #[command(subcommand)]
        action: ChatCommands,
    },

    /// Time-series and episodic metrics query/list
    #[command(name = "metrics")]
    Metrics {
        #[command(subcommand)]
        action: MetricsCommands,
    },

    /// Local deduplication learning loop: status/pause/resume/rollback/reset/train
    #[command(name = "learn")]
    Learn {
        #[command(subcommand)]
        action: LearnCommands,
    },

    /// Curated read-only GraphQL query execution
    #[command(name = "graphql")]
    Graphql {
        /// GraphQL query document
        #[arg(short, long)]
        query: String,
        /// Target namespace
        #[arg(short, long, default_value = "NeuroStrata")]
        namespace: String,
    },

    /// Define, validate, apply, revert, or inspect kinetic Action Types
    #[command(name = "action")]
    Action {
        #[command(subcommand)]
        action: ActionCommands,
    },

    /// Create, stage, evaluate SPARQL, diff, commit, drop, or list counterfactual Ontology Scenarios
    #[command(name = "scenario")]
    Scenario {
        #[command(subcommand)]
        scenario: ScenarioCommands,
    },

    /// Hydrate a 360-degree Ontology Object View for a memory, symbol, file, task, or digital twin
    #[command(name = "object-view")]
    ObjectView {
        /// Target namespace
        namespace: String,
        /// Memory ID or digital twin_id to inspect
        id: String,
        /// Optional counterfactual scenario ID
        #[arg(long)]
        scenario_id: Option<String>,
        /// Max recent action receipts to include in lineage (default 5, max 25)
        #[arg(long, default_value_t = 5)]
        include_history_limit: usize,
        /// Evaluate ActionType preconditions (default true)
        #[arg(long, default_value_t = true)]
        evaluate_action_preconditions: bool,
        /// Maximum serialized response bytes before truncation (default 8192)
        #[arg(long, default_value_t = 8192)]
        max_bytes: usize,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },

    /// Single-call compound Code Finder, Blast-Radius & Governance Locator
    Locate {
        /// Target namespace
        namespace: String,
        /// Symbol name, file path, or natural-language localization query
        query: String,
        /// Optional directory or file path prefix filter
        #[arg(long)]
        path_prefix: Option<String>,
        /// Only return spans anchored to a specific AST symbol
        #[arg(long, default_value_t = false)]
        symbols_only: bool,
        /// Maximum ranked locations to return (default 8, max 20)
        #[arg(long, default_value_t = 8)]
        max_locations: usize,
        /// Maximum serialized response byte budget (default 6144)
        #[arg(long, default_value_t = 6144)]
        max_bytes: usize,
        /// Optional project root directory
        #[arg(long)]
        project_root: Option<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },

    /// Generate a 4-stop dependency-ordered Guided Diff Tour with blast-radius & GOVERNS audit
    #[command(name = "diff-tour")]
    DiffTour {
        /// Target namespace
        namespace: String,
        /// Optional path to a unified diff file (or '-' for stdin)
        #[arg(long)]
        diff_file: Option<String>,
        /// Inline unified diff text
        #[arg(long)]
        diff_text: Option<String>,
        /// Optional project root directory
        #[arg(long)]
        project_root: Option<String>,
        /// Maximum unchanged callers to list per item (default 15)
        #[arg(long, default_value_t = 15)]
        max_blast_radius_callers: usize,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },

    /// Evaluate and manage longitudinal Architectural & Governance Code Insights
    #[command(name = "insights")]
    Insights {
        /// Target namespace
        namespace: String,
        /// Action: snapshot | series | define | remove
        #[arg(default_value = "snapshot")]
        action: String,
        /// Insight slug (optional filter on snapshot/series; required on define/remove)
        #[arg(long)]
        name: Option<String>,
        /// Human-readable title when defining a custom insight
        #[arg(long)]
        title: Option<String>,
        /// Custom insight query kind (sparql | lexical | ast_calls)
        #[arg(long)]
        query_kind: Option<String>,
        /// Custom insight query string
        #[arg(long)]
        query: Option<String>,
        /// Optional file/directory path prefix filter
        #[arg(long)]
        path_prefix: Option<String>,
        /// Optional start unix timestamp (seconds)
        #[arg(long)]
        from_ts: Option<i64>,
        /// Optional end unix timestamp (seconds)
        #[arg(long)]
        to_ts: Option<i64>,
        /// Optional number of bitemporal as_of backfill steps across [from_ts, to_ts]
        #[arg(long)]
        backfill_points: Option<usize>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },

    /// Manage and query Subject-Matter Domain Expertise, SKOS Concept Taxonomy & SME Frontier Advisor
    #[command(name = "taxonomy")]
    Taxonomy {
        #[command(subcommand)]
        taxonomy: TaxonomyCommands,
    },

    /// Validate the 3-tier Documentation Manifest and surface-diff gate
    #[command(name = "doc-gate")]
    DocGate {
        /// Optional project root directory (defaults to git root or current directory)
        #[arg(long)]
        root: Option<std::path::PathBuf>,
        /// Output JSON report
        #[arg(long)]
        json: bool,
    },

    /// Inspect and drive the pluggable workflow lifecycle state machine and connector
    #[command(name = "workflow")]
    Workflow {
        #[command(subcommand)]
        action: WorkflowCommands,
    },
    /// Interact with external data pools via registered DataConnectors
    #[command(name = "data-connector")]
    DataConnector {
        #[command(subcommand)]
        action: DataConnectorCommands,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
enum WorkflowCommands {
    /// Show active workflow connector, captured scope, state digest, and changed paths
    #[command(alias = "connector")]
    Status {
        #[arg(long)]
        root: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Print the formal workflow stage transition table
    Transitions {
        #[arg(long)]
        json: bool,
    },
    /// Install or verify lifecycle triggers for the active workflow connector
    #[command(alias = "install-triggers")]
    Install {
        #[arg(long)]
        root: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
enum DataConnectorCommands {
    /// List all registered data connectors.
    List,
    /// Describe a registered pool on a connector.
    #[command(name = "describe")]
    Describe {
        /// Connector ID (from `list`)
        #[arg(long)]
        connector_id: String,
        /// Pool ID to describe
        #[arg(long)]
        pool_id: String,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Register a data pointer under the kernel's ReBAC + pre-gate enforcement.
    #[command(name = "register-pointer")]
    RegisterPointer {
        /// Target namespace
        namespace: String,
        /// Optional JSON payload string or @file path
        #[arg(long)]
        payload: Option<String>,
        /// Connector ID
        #[arg(long)]
        connector_id: Option<String>,
        /// Pointer ID
        #[arg(long)]
        pointer_id: Option<String>,
        /// Pool ID
        #[arg(long)]
        pool_id: Option<String>,
        /// Pointer URI
        #[arg(long)]
        pointer_uri: Option<String>,
        /// Entity type
        #[arg(long)]
        entity_type: Option<String>,
        /// Captured-at timestamp in epoch seconds
        #[arg(long)]
        captured_at_secs: Option<u64>,
        /// Maximum staleness in seconds
        #[arg(long)]
        max_staleness_secs: Option<u64>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Resolve a bounded slice from a trusted pointer.
    #[command(name = "resolve")]
    Resolve {
        /// Target namespace
        namespace: String,
        /// Pointer ID
        #[arg(long)]
        pointer_id: String,
        /// Offset into the slice (default 0)
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Maximum rows to resolve
        #[arg(long)]
        limit: u32,
        /// Maximum bytes to resolve
        #[arg(long)]
        max_bytes: u32,
        /// Optional captured-at timestamp (epoch seconds) for staleness override
        #[arg(long)]
        now_secs: Option<u64>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Apply an external write-back guarded by receipt + CAS + pre/post gates.
    #[command(name = "apply-effect")]
    ApplyEffect {
        /// Target namespace
        namespace: String,
        /// Pointer ID
        #[arg(long)]
        pointer_id: String,
        /// Operation name (e.g. "append", "replace")
        #[arg(long)]
        operation: String,
        /// Optional JSON payload string or @file path
        #[arg(long)]
        payload: Option<String>,
        /// Action receipt ID
        #[arg(long)]
        action_receipt_id: String,
        /// Idempotency key
        #[arg(long)]
        idempotency_key: String,
        /// Expected source SHA-256
        #[arg(long)]
        expected_source_sha256: Option<String>,
        /// Optional captured-at timestamp (epoch seconds) for staleness override
        #[arg(long)]
        now_secs: Option<u64>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Mediate a workflow transition + bounded data slice resolution.
    #[command(name = "mediate")]
    Mediate {
        /// Target namespace
        namespace: String,
        /// From workflow stage
        #[arg(long)]
        from_stage: String,
        /// To workflow stage
        #[arg(long)]
        to_stage: String,
        /// Whether the transition gate passed
        #[arg(long)]
        gate_passed: bool,
        /// Pointer ID
        #[arg(long)]
        pointer_id: String,
        /// Offset into the slice (default 0)
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Maximum rows to resolve
        #[arg(long)]
        limit: u32,
        /// Maximum bytes to resolve
        #[arg(long)]
        max_bytes: u32,
        /// Optional captured-at timestamp (epoch seconds) for staleness override
        #[arg(long)]
        now_secs: Option<u64>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
enum LearnCommands {
    /// Show local deduplication learning status
    Status,
    /// Pause background candidate training
    Pause,
    /// Resume background candidate training
    Resume,
    /// Rollback active model to previous known-good version
    Rollback,
    /// Reset derived candidate datasets/models
    Reset {
        #[arg(long)]
        purge_observations: bool,
        #[arg(long)]
        confirm_purge: bool,
    },
    /// Build deduplication dataset and train/evaluate a candidate version
    Train {
        #[arg(long, default_value = "v1.0.0-local")]
        version: String,
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

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum KgeCommands {
    /// Offline batch training of TransE structural embeddings over
    /// 5 typed graph edges (GOVERNS/CONTAINS/RELATES_TO/DEPENDS_ON/EXTRACTED_FROM).
    /// Writes artifact to `<db_path_parent>/kge/<scope>/` (model.bin + meta.json).
    Train {
        /// Train on a single namespace's subgraph (`kge/<ns>/`)
        #[arg(long)]
        namespace: Option<String>,
        /// Train on union of all namespaces' subgraphs (`kge/global/`)
        #[arg(long)]
        all: bool,
        /// SGD epochs (default: 50)
        #[arg(long, default_value_t = 50)]
        epochs: u32,
        /// Embedding dimension d (default: 64)
        #[arg(long, default_value_t = 64)]
        dim: u32,
        /// Structural weight capped [0, 0.3] (default: 0.15)
        #[arg(long, default_value_t = 0.15f32)]
        kge_weight: f32,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum StorageCommands {
    /// Report store size, WAL size, and snapshot hygiene status
    Status,
    /// Verify SHA-256 checksums of an exported snapshot directory
    VerifySnapshot {
        #[arg(short, long)]
        dir: String,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum ChatCommands {
    /// Ask a question over project memories using local-first RAG
    Ask {
        #[arg(short, long)]
        namespace: String,
        #[arg(short, long)]
        question: String,
        #[arg(long, default_value_t = 5)]
        top_k: usize,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum MetricsCommands {
    /// List active time-series metric series
    List {
        #[arg(short, long)]
        namespace: Option<String>,
    },
    /// Query time-series samples for a metric
    Query {
        #[arg(short, long)]
        namespace: String,
        #[arg(short, long)]
        kind: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub(crate) enum ActionCommands {
    /// Define a new kinetic ActionType in a namespace
    Define {
        /// Target namespace
        namespace: String,
        /// ActionTypeDefinition JSON object string
        definition: String,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Dry-run validate an ActionType invocation without writing
    Validate {
        /// Target namespace
        namespace: String,
        /// ActionType slug
        slug: String,
        /// JSON object of parameters
        #[arg(long, default_value = "{}")]
        params: String,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Execute a kinetic ActionType atomically and record a private receipt
    Apply {
        /// Target namespace
        namespace: String,
        /// ActionType slug
        slug: String,
        /// JSON object of parameters
        #[arg(long, default_value = "{}")]
        params: String,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Revert a previously applied action receipt
    Revert {
        /// Target namespace
        namespace: String,
        /// Receipt ID to revert
        receipt_id: String,
        /// Force revert across newer target versions (requires owner/admin)
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// List redacted active action receipts in a namespace
    Log {
        /// Target namespace
        namespace: String,
        /// Maximum entries to return (default 100, max 1000)
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
}

fn action_command_to_json(action: &ActionCommands) -> anyhow::Result<serde_json::Value> {
    match action {
        ActionCommands::Define {
            namespace,
            definition,
            principal,
        } => {
            let def_val: serde_json::Value = serde_json::from_str(definition)
                .map_err(|e| anyhow::anyhow!("Invalid ActionTypeDefinition JSON: {e}"))?;
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "define",
                "definition": def_val,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ActionCommands::Validate {
            namespace,
            slug,
            params,
            principal,
        } => {
            let params_val: serde_json::Value = serde_json::from_str(params)
                .map_err(|e| anyhow::anyhow!("Invalid params JSON: {e}"))?;
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "validate",
                "slug": slug,
                "params": params_val,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ActionCommands::Apply {
            namespace,
            slug,
            params,
            principal,
        } => {
            let params_val: serde_json::Value = serde_json::from_str(params)
                .map_err(|e| anyhow::anyhow!("Invalid params JSON: {e}"))?;
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "apply",
                "slug": slug,
                "params": params_val,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ActionCommands::Revert {
            namespace,
            receipt_id,
            force,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "revert",
                "receipt_id": receipt_id,
                "force": force,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ActionCommands::Log {
            namespace,
            limit,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "log",
                "limit": limit,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
    }
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
enum ScenarioCommands {
    /// Create a new counterfactual scenario overlay in a namespace
    Create {
        /// Target namespace
        namespace: String,
        /// Optional explicit scenario ID
        #[arg(long)]
        scenario_id: Option<String>,
        /// Optional human-readable scenario title
        #[arg(long)]
        title: Option<String>,
        /// Optional TTL in seconds (default 1800, max 86400)
        #[arg(long)]
        ttl_secs: Option<i64>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Stage a memory upsert/retirement inside a scenario overlay
    #[command(name = "stage-memory")]
    StageMemory {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
        /// Memory content
        content: String,
        /// Optional explicit memory ID
        #[arg(long)]
        memory_id: Option<String>,
        /// Memory type (default "fact")
        #[arg(long, default_value = "fact")]
        memory_type: String,
        /// Optional metadata JSON object
        #[arg(long, default_value = "{}")]
        metadata: String,
        /// Optional live/staged memory ID to retire/supersede
        #[arg(long)]
        retire_id: Option<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Stage a digital twin state mutation inside a scenario overlay
    #[command(name = "stage-twin")]
    StageTwin {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
        /// Digital twin ID
        twin_id: String,
        /// Variables JSON object (map of variable name -> finite f64)
        #[arg(long)]
        variables: String,
        /// Merge variables into existing state (default true)
        #[arg(long, default_value_t = true)]
        merge: bool,
        /// Optional TwinSchema ID
        #[arg(long)]
        schema_id: Option<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Dry-run apply a kinetic ActionType inside a scenario overlay
    #[command(name = "apply-action")]
    ApplyAction {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
        /// ActionType slug
        slug: String,
        /// Action parameters JSON object
        #[arg(long, default_value = "{}")]
        params: String,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Evaluate a SPARQL query over the merged live + scenario RDF dataset
    #[command(name = "eval")]
    Eval {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
        /// SPARQL query string
        query: String,
        /// Result format (json|xml|csv|tsv)
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Compute diff of a scenario against live state or another scenario
    Diff {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
        /// Optional second scenario ID for side-by-side diff
        #[arg(long)]
        compare_to: Option<String>,
    },
    /// Atomically commit a scenario overlay to the live store with CAS + SHACL + ReBAC gates
    Commit {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
        /// Enforce SHACL validation on commit (default true)
        #[arg(long, default_value_t = true)]
        enforce_shacl: bool,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Drop a scenario overlay without touching the live store
    Drop {
        /// Target namespace
        namespace: String,
        /// Scenario ID
        scenario_id: String,
    },
    /// List active scenario overlays in a namespace
    List {
        /// Target namespace
        namespace: String,
    },
}

fn scenario_command_to_json(scenario: &ScenarioCommands) -> anyhow::Result<serde_json::Value> {
    match scenario {
        ScenarioCommands::Create {
            namespace,
            scenario_id,
            title,
            ttl_secs,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "create",
            });
            if let Some(id) = scenario_id {
                body["scenario_id"] = serde_json::json!(id);
            }
            if let Some(t) = title {
                body["title"] = serde_json::json!(t);
            }
            if let Some(ttl) = ttl_secs {
                body["ttl_secs"] = serde_json::json!(ttl);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ScenarioCommands::StageMemory {
            namespace,
            scenario_id,
            content,
            memory_id,
            memory_type,
            metadata,
            retire_id,
            principal,
        } => {
            let meta_val: serde_json::Value = serde_json::from_str(metadata)
                .map_err(|e| anyhow::anyhow!("Invalid metadata JSON: {e}"))?;
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "stage_memory",
                "scenario_id": scenario_id,
                "content": content,
                "memory_type": memory_type,
                "metadata": meta_val,
            });
            if let Some(id) = memory_id {
                body["memory_id"] = serde_json::json!(id);
            }
            if let Some(rid) = retire_id {
                body["retire_id"] = serde_json::json!(rid);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ScenarioCommands::StageTwin {
            namespace,
            scenario_id,
            twin_id,
            variables,
            merge,
            schema_id,
            principal,
        } => {
            let vars_val: serde_json::Value = serde_json::from_str(variables)
                .map_err(|e| anyhow::anyhow!("Invalid variables JSON: {e}"))?;
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "stage_twin",
                "scenario_id": scenario_id,
                "twin_id": twin_id,
                "variables": vars_val,
                "merge": merge,
            });
            if let Some(sid) = schema_id {
                body["schema_id"] = serde_json::json!(sid);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ScenarioCommands::ApplyAction {
            namespace,
            scenario_id,
            slug,
            params,
            principal,
        } => {
            let params_val: serde_json::Value = serde_json::from_str(params)
                .map_err(|e| anyhow::anyhow!("Invalid params JSON: {e}"))?;
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "apply_action",
                "scenario_id": scenario_id,
                "slug": slug,
                "params": params_val,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ScenarioCommands::Eval {
            namespace,
            scenario_id,
            query,
            format,
        } => Ok(serde_json::json!({
            "namespace": namespace,
            "action": "evaluate_sparql",
            "scenario_id": scenario_id,
            "query": query,
            "format": format,
        })),
        ScenarioCommands::Diff {
            namespace,
            scenario_id,
            compare_to,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "diff",
                "scenario_id": scenario_id,
            });
            if let Some(other) = compare_to {
                body["compare_to_scenario_id"] = serde_json::json!(other);
            }
            Ok(body)
        }
        ScenarioCommands::Commit {
            namespace,
            scenario_id,
            enforce_shacl,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "action": "commit",
                "scenario_id": scenario_id,
                "enforce_shacl_on_commit": enforce_shacl,
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        ScenarioCommands::Drop {
            namespace,
            scenario_id,
        } => Ok(serde_json::json!({
            "namespace": namespace,
            "action": "drop",
            "scenario_id": scenario_id,
        })),
        ScenarioCommands::List { namespace } => Ok(serde_json::json!({
            "namespace": namespace,
            "action": "list",
        })),
    }
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
enum TaxonomyCommands {
    /// Set or atomically supersede the singleton Project Thesis for a namespace
    #[command(name = "set-thesis")]
    SetThesis {
        /// Target namespace
        namespace: String,
        /// Optional JSON payload string or @file path
        #[arg(long)]
        payload: Option<String>,
        /// Primary domain slug
        #[arg(long)]
        primary_domain: Option<String>,
        /// Project mission statement
        #[arg(long)]
        mission: Option<String>,
        /// Strategic and architectural direction
        #[arg(long)]
        direction: Option<String>,
        /// Comma-separated core pillar slugs
        #[arg(long, value_delimiter = ',')]
        core_pillars: Vec<String>,
        /// Comma-separated adjacent domain slugs
        #[arg(long, value_delimiter = ',')]
        adjacent_domains: Vec<String>,
        /// Comma-separated evaluation axis slugs
        #[arg(long, value_delimiter = ',')]
        evaluation_axes: Vec<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Upsert or atomically supersede a SKOS subject-matter concept
    #[command(name = "upsert-concept")]
    UpsertConcept {
        /// Target namespace
        namespace: String,
        /// Optional JSON payload string or @file path
        #[arg(long)]
        payload: Option<String>,
        /// Canonical kebab-case concept slug
        #[arg(long)]
        slug: Option<String>,
        /// Preferred SKOS label
        #[arg(long)]
        pref_label: Option<String>,
        /// Subject-matter definition
        #[arg(long)]
        definition: Option<String>,
        /// Domain/pillar slug
        #[arg(long)]
        domain: Option<String>,
        /// Comma-separated broader parent concept slugs
        #[arg(long, value_delimiter = ',')]
        broader: Vec<String>,
        /// Comma-separated related concept slugs
        #[arg(long, value_delimiter = ',')]
        related_concepts: Vec<String>,
        /// Comma-separated contrasting concept slugs
        #[arg(long, value_delimiter = ',')]
        contrasts_with: Vec<String>,
        /// Realization status (unimplemented|partial|production|superseded)
        #[arg(long)]
        realization_status: Option<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Render the hierarchical SKOS taxonomy tree
    Tree {
        /// Target namespace
        namespace: String,
        /// Optional domain filter
        #[arg(long)]
        domain: Option<String>,
        /// Optional root concept slug
        #[arg(long)]
        root_slug: Option<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Generate an Ontology-Augmented Subject-Matter Briefing
    Briefing {
        /// Target namespace
        namespace: String,
        /// Topic or natural-language query
        #[arg(long)]
        query: Option<String>,
        /// Optional domain filter
        #[arg(long)]
        domain: Option<String>,
        /// Multi-hop expansion radius (1..=4, default 2)
        #[arg(long, default_value_t = 2)]
        max_hops: usize,
        /// Max seed concepts (default 8)
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Generate a Multi-Perspective Comparative Matrix across 2 or more concepts
    Compare {
        /// Target namespace
        namespace: String,
        /// Comma-separated concept slugs to compare (2 or more)
        #[arg(long, value_delimiter = ',', required = true)]
        slugs: Vec<String>,
        /// Optional comma-separated evaluation axis slugs
        #[arg(long, value_delimiter = ',')]
        axes: Vec<String>,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Compute Epistemic Coverage & Ontology Anti-Pattern Gaps
    Gaps {
        /// Target namespace
        namespace: String,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
    /// Run SME Opportunity & Frontier Synthesis Advisor
    Advise {
        /// Target namespace
        namespace: String,
        /// Optional query or topic focus
        #[arg(long)]
        query: Option<String>,
        /// Optional domain filter
        #[arg(long)]
        domain: Option<String>,
        /// Max ranked recommendations to return (default 5, max 20)
        #[arg(long, default_value_t = 5)]
        top_k: usize,
        /// Optional requested principal
        #[arg(long)]
        principal: Option<String>,
    },
}

fn parse_inline_or_file_json(raw: &str) -> anyhow::Result<serde_json::Value> {
    let trimmed = raw.trim();
    let content = if let Some(path) = trimmed.strip_prefix('@') {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read JSON file '{path}': {e}"))?
    } else {
        trimmed.to_string()
    };
    serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("invalid JSON payload: {e}"))
}

fn taxonomy_command_to_json(taxonomy: &TaxonomyCommands) -> anyhow::Result<serde_json::Value> {
    match taxonomy {
        TaxonomyCommands::SetThesis {
            namespace,
            payload,
            primary_domain,
            mission,
            direction,
            core_pillars,
            adjacent_domains,
            evaluation_axes,
            principal,
        } => {
            let mut body = if let Some(raw) = payload {
                parse_inline_or_file_json(raw)?
            } else {
                serde_json::json!({})
            };
            body["namespace"] = serde_json::json!(namespace);
            body["operation"] = serde_json::json!("set_thesis");
            if let Some(v) = primary_domain {
                body["primary_domain"] = serde_json::json!(v);
            }
            if let Some(v) = mission {
                body["mission"] = serde_json::json!(v);
            }
            if let Some(v) = direction {
                body["direction"] = serde_json::json!(v);
            }
            if !core_pillars.is_empty() {
                body["core_pillars"] = serde_json::json!(core_pillars);
            }
            if !adjacent_domains.is_empty() {
                body["adjacent_domains"] = serde_json::json!(adjacent_domains);
            }
            if !evaluation_axes.is_empty() {
                body["evaluation_axes"] = serde_json::json!(evaluation_axes);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        TaxonomyCommands::UpsertConcept {
            namespace,
            payload,
            slug,
            pref_label,
            definition,
            domain,
            broader,
            related_concepts,
            contrasts_with,
            realization_status,
            principal,
        } => {
            let mut body = if let Some(raw) = payload {
                parse_inline_or_file_json(raw)?
            } else {
                serde_json::json!({})
            };
            body["namespace"] = serde_json::json!(namespace);
            body["operation"] = serde_json::json!("upsert_concept");
            if let Some(v) = slug {
                body["slug"] = serde_json::json!(v);
            }
            if let Some(v) = pref_label {
                body["pref_label"] = serde_json::json!(v);
            }
            if let Some(v) = definition {
                body["definition"] = serde_json::json!(v);
            }
            if let Some(v) = domain {
                body["domain"] = serde_json::json!(v);
            }
            if !broader.is_empty() {
                body["broader"] = serde_json::json!(broader);
            }
            if !related_concepts.is_empty() {
                body["related_concepts"] = serde_json::json!(related_concepts);
            }
            if !contrasts_with.is_empty() {
                body["contrasts_with"] = serde_json::json!(contrasts_with);
            }
            if let Some(v) = realization_status {
                body["realization_status"] = serde_json::json!(v);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        TaxonomyCommands::Tree {
            namespace,
            domain,
            root_slug,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "operation": "tree",
            });
            if let Some(d) = domain {
                body["domain"] = serde_json::json!(d);
            }
            if let Some(r) = root_slug {
                body["root_slug"] = serde_json::json!(r);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        TaxonomyCommands::Briefing {
            namespace,
            query,
            domain,
            max_hops,
            limit,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "operation": "briefing",
                "max_hops": max_hops,
                "limit": limit,
            });
            if let Some(q) = query {
                body["query"] = serde_json::json!(q);
            }
            if let Some(d) = domain {
                body["domain"] = serde_json::json!(d);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        TaxonomyCommands::Compare {
            namespace,
            slugs,
            axes,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "operation": "compare",
                "slugs": slugs,
            });
            if !axes.is_empty() {
                body["axes"] = serde_json::json!(axes);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        TaxonomyCommands::Gaps {
            namespace,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "operation": "gaps",
            });
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
        TaxonomyCommands::Advise {
            namespace,
            query,
            domain,
            top_k,
            principal,
        } => {
            let mut body = serde_json::json!({
                "namespace": namespace,
                "operation": "advise",
                "top_k": top_k,
            });
            if let Some(q) = query {
                body["query"] = serde_json::json!(q);
            }
            if let Some(d) = domain {
                body["domain"] = serde_json::json!(d);
            }
            if let Some(p) = principal {
                body["principal"] = serde_json::json!(p);
            }
            Ok(body)
        }
    }
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
    match Config::load() {
        // Config::load() is read-only: it never materializes a config file,
        // which a congested-proxy probe has no business doing.
        Ok(c) => Some(c.db_path.to_string_lossy().to_string()),
        Err(e) => {
            tracing::warn!("config_db_path failed to load config: {e}");
            None
        }
    }
}

/// Surface a pending first-run embedding-model choice, if materialization
/// recorded one. Called at interactive dispatch only — the caller has
/// already excluded the daemon and shutdown commands — and the seam
/// re-checks that stdin is a terminal before anything is presented. Runs
/// before the command executes, so a pick is persisted before any store
/// creation on its path. A failure is reported, never fatal to the
/// command.
fn surface_pending_model_choice() {
    use crate::model_choice::ChoiceResponse;
    use std::io::{IsTerminal, Write};
    let Some(db_path) = config_db_path() else {
        return;
    };
    let interactive = std::io::stdin().is_terminal();
    let mut responder = |offer: &str| -> ChoiceResponse {
        println!("{offer}");
        print!("Choose a model by number, or enter 0 to keep the current default: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return ChoiceResponse::Invalid;
        }
        // 0 is the documented decline input; a number picks that entry
        // (an out-of-range number is invalid, handled by the seam);
        // anything else — text, blank — is invalid, not a decline.
        match line.trim().parse::<usize>() {
            Ok(0) => ChoiceResponse::Decline,
            Ok(n) => ChoiceResponse::Pick(n - 1),
            Err(_) => ChoiceResponse::Invalid,
        }
    };
    match crate::model_choice::surface_pending_choice(
        &crate::embed::config_dir(),
        std::path::Path::new(&db_path),
        interactive,
        &mut responder,
    ) {
        Ok(crate::model_choice::PendingChoiceOutcome::Chosen(name)) => {
            println!("Embedding model set to {name} for this install.");
        }
        Ok(crate::model_choice::PendingChoiceOutcome::InvalidInput) => {
            println!(
                "That was not a valid choice, so nothing was changed. The embedding-model choice will be offered again at the next command."
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("Could not apply the pending embedding-model choice: {e}"),
    }
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
        if let Some(parent) = config.db_path.parent() {
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

/// Bind the fallback store's serving descriptor
/// to the embedder an arm just built, through the concrete store
/// behind the trait object — the same downcast the RepairHygiene arm
/// uses. The fallback group constructs its store before any arm builds
/// an embedder, so the construction-time builder cannot bind it; this
/// post-build binding is what keys the store's identity checks on the
/// model that will actually produce the arm's vectors.
fn bind_fallback_serving_descriptor(
    vector_store: &Arc<dyn VectorStore>,
    embedder: &Arc<dyn crate::traits::Embedder>,
) {
    use crate::store::ladybug::LadybugStore;
    if let Some(store) =
        crate::traits::VectorStore::as_any(vector_store.as_ref()).downcast_ref::<LadybugStore>()
    {
        store.set_serving_descriptor(Some(embedder.embedding_descriptor()));
    }
}

/// Prepare the fallback store for a write arm (Add, Edit, Ingest): bind the
/// serving descriptor, then initialize the store under it.
///
/// `init` is the step that runs the store-state maintenance
/// (`maintain_store_state`): on a store recorded pristine under a
/// different model, it adopts the serving descriptor when serving
/// matches the configuration — which is exactly the descriptor a
/// fallback arm binds, because it built its embedder from the
/// configuration. Without this step the write arms dispatched
/// straight to `upsert`, whose first-write guard refuses a pristine
/// store whose recorded descriptor differs from serving with
/// restart guidance this path could never satisfy: re-running the
/// command bound and upserted again, still with no init, so the
/// demanded adoption never occurred (issue #75). The daemon write
/// half has always ordered its work this way
/// (`embed_dedup_and_upsert` inits before it embeds or upserts);
/// the fallback write arms now follow the same pattern. Read arms
/// deliberately do not use this helper: they gain no init side
/// effects they do not have today.
async fn prepare_fallback_write_store(
    vector_store: &Arc<dyn VectorStore>,
    embedder: &Arc<dyn crate::traits::Embedder>,
    namespace: &str,
) -> anyhow::Result<()> {
    bind_fallback_serving_descriptor(vector_store, embedder);
    vector_store.init(namespace).await
}

/// Whether some process holds the daemon lock right now.
fn daemon_holds_lock(db_path: &std::path::Path) -> bool {
    // Read-only status probes must not create the lock file: open_daemon_lock
    // opens with .create(true), so an absent-but-unheld lock would be
    // materialized just by asking about it.
    if !daemon_lock_path(db_path).exists() {
        return false;
    }
    match open_daemon_lock(db_path) {
        Ok(file) => matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(e) => {
            tracing::warn!(
                "daemon_holds_lock probe failed for {}: {e}",
                db_path.display()
            );
            false
        }
    }
}

/// Take the store lock for an uncoordinated copy and keep it (#241).
///
/// The `--no-open` gate used to be a point-in-time probe: it took the lock,
/// learned whether a daemon held it, and dropped the handle before the
/// first file copy. A daemon that started in between was never seen, so the
/// database, its WAL, and the checkpoint shadow could be captured at
/// different instants while every per-file check still passed.
///
/// The returned guard holds the lock until it is dropped, so a daemon that
/// starts during the copy window cannot open the store and cannot change
/// what is being copied. The lock file is created when it does not exist
/// yet: "nobody holds the lock" is decided by taking it, not by looking for
/// it.
///
/// A lock that cannot be taken is not evidence that the store is free, so
/// the failure is closed: the caller refuses instead of copying.
fn hold_store_lock_for_copy(db_path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let lock_path = daemon_lock_path(db_path);
    let file = open_daemon_lock(db_path).map_err(|e| {
        anyhow::anyhow!(
            "Refusing the uncoordinated file copy: the store lock could not be taken (could not open {}: {}), so it cannot be shown that no daemon holds the store. Take a coordinated backup with `neurostrata-mcp backup <dir>` (without --no-open), or run `neurostrata-mcp shutdown` first.",
            lock_path.display(),
            e
        )
    })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(anyhow::anyhow!("{}", NO_OPEN_LOCK_HELD_MESSAGE)),
        Err(std::fs::TryLockError::Error(e)) => Err(anyhow::anyhow!(
            "Refusing the uncoordinated file copy: the store lock could not be taken ({}), so it cannot be shown that no daemon holds the store. Take a coordinated backup with `neurostrata-mcp backup <dir>` (without --no-open), or run `neurostrata-mcp shutdown` first.",
            e
        )),
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
fn task_gate_request(
    namespace: &str,
    strict: bool,
    scope: &crate::task::gate::GateScope,
) -> serde_json::Value {
    serde_json::json!({
        "namespace": namespace,
        "strict": strict,
        "scope": scope,
    })
}

async fn direct_report(
    config: &Config,
    namespace: &str,
    scope: Option<&crate::task::gate::GateScope>,
) -> Result<crate::task::gate::GateReport, String> {
    let dimensions = embed::configured_dimensions().map_err(|e| {
        format!(
            "the configured embedding dimensions could not be read: {}",
            e
        )
    })?;
    let store: Arc<dyn VectorStore> = Arc::new(
        LadybugStore::new(config.db_path.to_string_lossy().to_string(), dimensions)
            .map_err(|e| format!("the database could not be opened: {}", e))?,
    );
    store
        .init(namespace)
        .await
        .map_err(|e| format!("the schema could not be opened: {}", e))?;
    let namespace = crate::server::resolve_namespace(&store, namespace).await;
    crate::task::gate::run_scoped(&store, &namespace, scope)
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

/// Refusal for `backup --no-open` while the daemon lock is held (#209). The
/// copy is uncoordinated: the mmapped database, its WAL, and the checkpoint
/// shadow would be read at different instants while the daemon may be
/// mid-checkpoint, producing a snapshot that verifies but cannot be restored.
const NO_OPEN_LOCK_HELD_MESSAGE: &str = "Refusing --no-open while a process holds the store lock: the store may be mid-checkpoint, so the database, its write-ahead log, and the checkpoint shadow could be copied at different instants. Take a coordinated backup instead with `neurostrata-mcp backup <dir>` (without --no-open), or run `neurostrata-mcp shutdown` and take the no-open copy once it has exited.";

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
        if let Err(e) = std::fs::copy(&target, &backup) {
            eprintln!(
                "Warning: failed to back up existing hook to {}: {e}",
                backup.display()
            );
        }
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
    if !hooks_dir.is_dir() {
        std::fs::create_dir_all(&hooks_dir)?;
    }
    let target = hooks_dir.join("pre-commit");
    // A pre-existing hook we did not scaffold is the user's; back it up
    // rather than silently destroying it by overwriting.
    if target.exists() {
        let existing = std::fs::read_to_string(&target)?;
        if !crate::task::is_ours_hook(&existing) {
            let backup = hooks_dir.join("pre-commit.bak");
            std::fs::copy(&target, &backup)?;
            eprintln!(
                "Backed up an existing (foreign) pre-commit hook to {}",
                backup.display()
            );
        }
    }
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
        if let Err(e) = file.set_len(0) {
            tracing::warn!("final checkpoint report truncate failed: {e}");
        }
        if let Err(e) = (&*file).write_all(report.as_bytes()) {
            tracing::warn!("final checkpoint report write failed: {e}");
        }
        if let Err(e) = file.sync_all() {
            tracing::warn!("final checkpoint report sync failed: {e}");
        }
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
                if !res.status().is_success() {
                    anyhow::bail!("daemon /cli/read returned HTTP {}", res.status());
                }
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
                if !res.status().is_success() {
                    anyhow::bail!("daemon /cli/read returned HTTP {}", res.status());
                }
                let v: serde_json::Value = res.json().await?;
                Ok(serde_json::from_value(v["rows"].clone())?)
            }
        }
    }
}

fn print_learn_control(req: crate::learning::control::LearningControlRequest) {
    match crate::learning::control::apply_control(&req) {
        Ok(res) => match serde_json::to_string_pretty(&res) {
            Ok(out) => println!("{}", out),
            Err(e) => eprintln!("learn control result serialize failed: {}", e),
        },
        Err(e) => {
            eprintln!("learn control failed: {}", e);
            std::process::exit(1);
        }
    }
}

/// Build the dedup dataset, train a candidate, evaluate it fail-closed, and
/// promote it when the real eval report carries numeric metrics.
async fn run_learn_train(version: &str) -> anyhow::Result<()> {
    let root = crate::learning::control::default_learning_dir();
    let paths = crate::judgment::laya_onnx::LearningPaths::new(root.clone());
    let mgr = crate::judgment::laya_onnx::LearningRuntimeManager::new(paths.clone());
    let rt_state = mgr.ensure_ready().await;
    let runtime = std::sync::Arc::new(mgr.runtime()?);
    let model_cache = runtime.model_cache.clone();
    let trainer = crate::learning::trainer::Trainer::new(runtime);

    let mut job_state = trainer.transition(
        &crate::learning::trainer::TrainingJobState::WaitingForData,
        crate::learning::trainer::TrainingJobState::Training,
    )?;

    let policy = crate::learning::dataset::DistancePolicy::load_from(&paths.root_dir());
    policy.save_to(&paths.root_dir())?;
    let dataset_report = crate::learning::dataset::build_dedup_dataset(
        &paths.root_dir(),
        &paths.root_dir().join("dataset"),
    )
    .await?;

    trainer.train_candidate(&version.to_string()).await?;
    job_state = trainer.transition(
        &job_state,
        crate::learning::trainer::TrainingJobState::ProxyShadow,
    )?;

    // The builtin train path writes under runtime model_cache, not LearningPaths.
    let candidate_dir = model_cache.join("candidates").join(version);
    let eval_path = candidate_dir.join("eval_report.json");
    let eval_raw = std::fs::read_to_string(&eval_path).map_err(|_| {
        anyhow::anyhow!(
            "no eval_report.json for candidate {} at {}; training did not produce a measurable artifact; evaluation refused (fail-closed)",
            version,
            eval_path.display()
        )
    })?;
    let eval_report: crate::learning::trainer::EvaluationReport = serde_json::from_str(&eval_raw)
        .map_err(|e| {
        anyhow::anyhow!("eval_report.json for {} is not parseable: {}", version, e)
    })?;
    let score = trainer.evaluate_candidate(&eval_report)?;
    job_state = trainer.transition(
        &job_state,
        crate::learning::trainer::TrainingJobState::GoldShadow,
    )?;

    let bundle = crate::learning::model_bundle::ModelBundle::load(&candidate_dir, "laya")?;

    // Promote in the active manifest and persist it.
    let manifest_path = paths.root_dir().join("active_manifest.json");
    let mut manifest: crate::learning::trainer::ActiveManifest = if manifest_path.exists() {
        match std::fs::read_to_string(&manifest_path)
            .map_err(|e| e.to_string())
            .and_then(|raw| {
                serde_json::from_str::<crate::learning::trainer::ActiveManifest>(&raw)
                    .map_err(|e| e.to_string())
            }) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "active manifest at {} unreadable or unparseable, using defaults: {e}",
                    manifest_path.display()
                );
                crate::learning::trainer::ActiveManifest::default()
            }
        }
    } else {
        crate::learning::trainer::ActiveManifest::default()
    };
    trainer.promote(&mut manifest, &version.to_string(), score)?;
    job_state = trainer.transition(
        &job_state,
        crate::learning::trainer::TrainingJobState::Promoted,
    )?;

    std::fs::create_dir_all(paths.root_dir())?;
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    // Mirror promoted versions into status.json.
    let mut status = crate::learning::control::load_status(&paths.root_dir());
    status.active_version = manifest.active_version.clone();
    status.previous_good_version = manifest.previous_good_version.clone();
    status.candidate_version = None;
    status.training_state = job_state;
    status.runtime_state = match rt_state {
        crate::judgment::laya_onnx::LayaReadiness::Ready => {
            crate::judgment::laya_onnx::RuntimeState::Ready
        }
        crate::judgment::laya_onnx::LayaReadiness::Degraded { .. } => {
            crate::judgment::laya_onnx::RuntimeState::Unavailable
        }
    };
    status.latest_evaluation = Some(eval_report);
    std::fs::write(
        paths.root_dir().join("status.json"),
        serde_json::to_string_pretty(&status)?,
    )?;

    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "version": version,
            "rt_state": format!("{:?}", rt_state),
            "score": score,
            "active_version": manifest.active_version,
            "previous_good_version": manifest.previous_good_version,
            "checkpoint_path": bundle.checkpoint_path(),
            "manifest_path": bundle.manifest_path(),
            "dataset_report": dataset_report,
            "train_jsonl": paths.train_jsonl(),
            "proxy_eval_jsonl": paths.proxy_eval_jsonl(),
            "base_model_dir": paths.base_model_dir(),
            "candidate_dir": paths.candidate_dir(&version.to_string()),
        })
    );
    Ok(())
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
                let a_len = match read.list(a).await {
                    Ok(m) => m.len(),
                    Err(e) => {
                        tracing::warn!("doctor list failed for '{a}': {e}");
                        0
                    }
                };
                let b_len = match read.list(b).await {
                    Ok(m) => m.len(),
                    Err(e) => {
                        tracing::warn!("doctor list failed for '{b}': {e}");
                        0
                    }
                };
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
        // F11: surface enrichment_state and ontology_warnings from registry row.
        for memory in &memories {
            if crate::registry::is_registry_row(&memory.payload.metadata) {
                if let Some(es) = memory.payload.metadata.get("enrichment_state") {
                    println!("  [{}] enrichment_state: {}", ns, es);
                }
                if let Some(ow) = memory.payload.metadata.get("ontology_warnings") {
                    println!("  [{}] ontology_warnings: {}", ns, ow);
                }
            }
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

    #[test]
    fn copy_snapshot_into_place_is_failure_atomic_and_cleans_up_on_error() {
        // Issue #87: a companion copy that fails midway must not leave the
        // store file at the target — the restore is staged, committed only
        // as a whole set, and the staged files are removed on error.
        let root = std::env::temp_dir().join(format!("mcp-restore-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = root.join("snapshot");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("ladybug.store"), b"store-bytes").unwrap();
        // A directory where the WAL companion should be: fs::copy of it
        // fails, after the store file copy already succeeded.
        std::fs::create_dir_all(snapshot.join("ladybug.store.wal")).unwrap();

        let target = root.join("restored");
        let err = copy_snapshot_into_place(&snapshot, &target)
            .expect_err("copying a directory as the WAL companion fails");
        assert!(
            !target.exists(),
            "no partial store is left at the target after a failed companion copy: {err}"
        );
        let mut target_wal = target.as_os_str().to_os_string();
        target_wal.push(".wal");
        assert!(
            !std::path::Path::new(&target_wal).exists(),
            "no staged WAL survives a failed companion copy"
        );
        assert!(
            !root
                .join(format!("restored.restore-tmp-{}", std::process::id()))
                .exists(),
            "the staged store copy is removed on failure"
        );
        assert!(
            !root
                .join(format!("restored.wal.restore-tmp-{}", std::process::id()))
                .exists(),
            "the staged WAL copy is removed on failure"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_snapshot_into_place_refuses_sidecar_directory_and_preserves_target_wal() {
        // Issue #95: a pre-existing target_wal file and a target_sidecar
        // directory must not leave a half-committed target. The sidecar
        // directory blocks the commit, so the call fails before target is
        // touched, the pre-existing WAL is preserved intact, and the target
        // file is absent.
        let root =
            std::env::temp_dir().join(format!("mcp-restore-sidecar-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = root.join("snapshot");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("ladybug.store"), b"store-bytes").unwrap();

        let target = root.join("restored");
        let target_wal = root.join("restored.wal");
        std::fs::write(&target_wal, b"preexisting-wal").unwrap();
        let target_sidecar = crate::model_choice::store_state_path(&target);
        std::fs::create_dir_all(&target_sidecar).unwrap();

        let err = copy_snapshot_into_place(&snapshot, &target)
            .expect_err("a directory at the sidecar path blocks the commit");
        assert!(
            !target.exists(),
            "the target store file must be absent when the sidecar path is a directory: {err}"
        );
        assert_eq!(
            std::fs::read(&target_wal).unwrap(),
            b"preexisting-wal",
            "the pre-existing target WAL must survive the refusal intact"
        );
        assert!(
            !root
                .join(format!("restored.restore-tmp-{}", std::process::id()))
                .exists(),
            "the staged store copy is removed on refusal"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_mistyped_subcommand_is_an_error_not_an_execution() {
        let parsed = Cli::try_parse_from(["neurostrata-mcp", "lsit"]);
        assert!(
            parsed.is_err(),
            "a typo must never reach std::process::Command"
        );
    }

    #[test]
    fn bench_subcommand_parses() {
        let cli = Cli::try_parse_from([
            "neurostrata-mcp",
            "bench",
            "--suite",
            "all",
            "--iterations",
            "2",
            "--max-regression-pct",
            "10.0",
            "--record-metrics",
            "ns",
        ])
        .expect("bench parses");
        assert!(matches!(cli.command, Some(Commands::Bench { .. })));
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
                ..
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
    #[cfg(unix)]
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
        t4_git_ok(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
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

    /// A plain `cargo test` builds the test harness, not the CLI bin; CI builds
    /// the bin first. Build it on demand here, then let the caller skip with a
    /// clear message if the toolchain cannot produce it.
    #[cfg(unix)]
    fn t4_ensure_cli_binary() -> Option<std::path::PathBuf> {
        let bin = t4_cli_binary();
        if bin.is_file() {
            return Some(bin);
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = std::process::Command::new(cargo)
            .args(["build", "--bin", "neurostrata-mcp"])
            .status();
        match built {
            Ok(status) if status.success() && bin.is_file() => Some(bin),
            _ => None,
        }
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
        let Some(bin) = t4_ensure_cli_binary() else {
            eprintln!(
                "skipping: the CLI binary is not built at {} and `cargo build --bin neurostrata-mcp` failed",
                t4_cli_binary().display()
            );
            return;
        };
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
            String::from_utf8_lossy(&skipped.stdout)
                .contains("Task gate: skipped in 't4cli' (NEUROSTRATA_SKIP_GATE=1)."),
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

    #[test]
    fn the_action_commands_parse() {
        let def = Cli::try_parse_from([
            "neurostrata-mcp",
            "action",
            "define",
            "MyProj",
            r#"{"slug":"s","title":"t","description":"d","required_role":"editor","parameters":[],"submission_criteria":[],"effects":[]}"#,
        ])
        .expect("action define parses");
        assert!(matches!(
            def.command,
            Some(Commands::Action {
                action: ActionCommands::Define { .. }
            })
        ));

        let val = Cli::try_parse_from([
            "neurostrata-mcp",
            "action",
            "validate",
            "MyProj",
            "recalibrate-loop",
            "--params",
            r#"{"twin_id":"loop-a"}"#,
        ])
        .expect("action validate parses");
        assert!(matches!(
            val.command,
            Some(Commands::Action {
                action: ActionCommands::Validate { .. }
            })
        ));

        let app = Cli::try_parse_from([
            "neurostrata-mcp",
            "action",
            "apply",
            "MyProj",
            "recalibrate-loop",
            "--params",
            r#"{"twin_id":"loop-a"}"#,
            "--principal",
            "alice",
        ])
        .expect("action apply parses");
        assert!(matches!(
            app.command,
            Some(Commands::Action {
                action: ActionCommands::Apply { .. }
            })
        ));

        let rev = Cli::try_parse_from([
            "neurostrata-mcp",
            "action",
            "revert",
            "MyProj",
            "rec-123",
            "--force",
        ])
        .expect("action revert parses");
        match rev.command {
            Some(Commands::Action {
                action:
                    ActionCommands::Revert {
                        namespace,
                        receipt_id,
                        force,
                        ..
                    },
            }) => {
                assert_eq!(namespace, "MyProj");
                assert_eq!(receipt_id, "rec-123");
                assert!(force);
            }
            other => panic!("expected Action::Revert, got {:?}", other),
        }

        let log = Cli::try_parse_from([
            "neurostrata-mcp",
            "action",
            "log",
            "MyProj",
            "--limit",
            "25",
        ])
        .expect("action log parses");
        match log.command {
            Some(Commands::Action {
                action:
                    ActionCommands::Log {
                        namespace, limit, ..
                    },
            }) => {
                assert_eq!(namespace, "MyProj");
                assert_eq!(limit, 25);
            }
            other => panic!("expected Action::Log, got {:?}", other),
        }

        let tax_adv = Cli::try_parse_from([
            "neurostrata-mcp",
            "taxonomy",
            "advise",
            "NeuroStrata",
            "--query",
            "top 5 improvements",
            "--top-k",
            "5",
        ])
        .expect("taxonomy advise parses");
        match tax_adv.command {
            Some(Commands::Taxonomy {
                taxonomy:
                    TaxonomyCommands::Advise {
                        namespace,
                        query,
                        top_k,
                        ..
                    },
            }) => {
                assert_eq!(namespace, "NeuroStrata");
                assert_eq!(query.as_deref(), Some("top 5 improvements"));
                assert_eq!(top_k, 5);
            }
            other => panic!("expected Taxonomy::Advise, got {:?}", other),
        }
    }

    #[test]
    fn the_scenario_commands_parse() {
        let create = Cli::try_parse_from([
            "neurostrata-mcp",
            "scenario",
            "create",
            "MyProj",
            "--scenario-id",
            "scen-1",
            "--title",
            "Cooling loop sandbox",
        ])
        .expect("scenario create parses");
        assert!(matches!(
            create.command,
            Some(Commands::Scenario {
                scenario: ScenarioCommands::Create { .. }
            })
        ));

        let eval = Cli::try_parse_from([
            "neurostrata-mcp",
            "scenario",
            "eval",
            "MyProj",
            "scen-1",
            "ASK { ?s a ns:TwinState }",
        ])
        .expect("scenario eval parses");
        assert!(matches!(
            eval.command,
            Some(Commands::Scenario {
                scenario: ScenarioCommands::Eval { .. }
            })
        ));

        let diff = Cli::try_parse_from([
            "neurostrata-mcp",
            "scenario",
            "diff",
            "MyProj",
            "scen-1",
            "--compare-to",
            "scen-2",
        ])
        .expect("scenario diff parses");
        assert!(matches!(
            diff.command,
            Some(Commands::Scenario {
                scenario: ScenarioCommands::Diff { .. }
            })
        ));

        let ov = Cli::try_parse_from([
            "neurostrata-mcp",
            "object-view",
            "MyProj",
            "loop-a",
            "--principal",
            "bob",
        ])
        .expect("object-view parses");
        assert!(matches!(ov.command, Some(Commands::ObjectView { .. })));

        let loc = Cli::try_parse_from([
            "neurostrata-mcp",
            "locate",
            "MyProj",
            "check_caller_path",
            "--max-locations",
            "5",
        ])
        .expect("locate parses");
        assert!(matches!(loc.command, Some(Commands::Locate { .. })));

        let dt = Cli::try_parse_from([
            "neurostrata-mcp",
            "diff-tour",
            "MyProj",
            "--max-blast-radius-callers",
            "10",
        ])
        .expect("diff-tour parses");
        assert!(matches!(dt.command, Some(Commands::DiffTour { .. })));

        let ins = Cli::try_parse_from(["neurostrata-mcp", "insights", "MyProj", "snapshot"])
            .expect("insights parses");
        assert!(matches!(ins.command, Some(Commands::Insights { .. })));
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

    #[test]
    fn strict_gate_request_carries_the_callers_git_scope() {
        let scope = crate::task::gate::GateScope {
            branch: Some("release/v1.17.3".to_string()),
            worktree: Some("/repo/release".to_string()),
        };
        let body = task_gate_request("NeuroStrata", true, &scope);
        assert_eq!(
            body,
            serde_json::json!({
                "namespace": "NeuroStrata",
                "strict": true,
                "scope": {
                    "branch": "release/v1.17.3",
                    "worktree": "/repo/release",
                },
            })
        );
    }

    /// An embedder serving ModelB at the fixture width: its descriptor
    /// is the shape `FastEmbedder::embedding_descriptor` produces for
    /// the ModelB entry, matching the descriptor the store synthesizes
    /// for the same configured entry.
    struct FallbackModelBEmbedder;

    #[async_trait::async_trait]
    impl crate::traits::Embedder for FallbackModelBEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        fn dimensions(&self) -> usize {
            4
        }
        fn embedder_id(&self) -> &str {
            "ModelB"
        }
        fn embedding_descriptor(&self) -> crate::traits::EmbeddingDescriptor {
            crate::traits::EmbeddingDescriptor {
                provider: "fastembed".to_string(),
                model: "ModelB".to_string(),
                revision: "onnx".to_string(),
                dims: 4,
                normalization: "unknown".to_string(),
                preprocessing: "unknown".to_string(),
                distance_metric: "cosine".to_string(),
            }
        }
    }

    /// Regression test for issue #75 (Codex finding T6 on PR #66): the
    /// daemon-stopped fallback `add` on a store recorded pristine
    /// under the default model, after the user configured a different
    /// model. The fallback process serves the configured model, so
    /// the pristine record must be adopted in this process and the
    /// add must proceed — re-running `add` must not loop on the
    /// guard's restart guidance forever.
    #[tokio::test]
    async fn fallback_add_on_pristine_store_adopts_configured_model_and_writes() {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let root =
            std::env::temp_dir().join(format!("ns-fallback-add-adopt-{}", uuid::Uuid::new_v4()));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let write_config = |model: &str| {
            std::fs::write(
                config_dir.join("embedders.json"),
                format!("[{{\"model_name\":\"{model}\",\"dimensions\":4}}]"),
            )
            .unwrap();
        };
        let db_path = root.join("ladybug.db");
        let sidecar = |db: &std::path::Path| -> serde_json::Value {
            let content =
                std::fs::read_to_string(crate::model_choice::store_state_path(db)).unwrap();
            serde_json::from_str(&content).unwrap()
        };

        // The store as the daemon left it: recorded pristine under
        // the default model (ModelA here).
        write_config("ModelA");
        let seed = LadybugStore::for_testing(&db_path, 4)
            .expect("open temp database")
            .with_embed_config_dir(&config_dir)
            .with_serving_descriptor(Some(crate::traits::EmbeddingDescriptor {
                provider: "fastembed".to_string(),
                model: "ModelA".to_string(),
                revision: "onnx".to_string(),
                dims: 4,
                normalization: "unknown".to_string(),
                preprocessing: "unknown".to_string(),
                distance_metric: "cosine".to_string(),
            }));
        seed.init("probe").await.expect("seed init");
        assert_eq!(sidecar(&db_path)["has_data"], serde_json::json!(false));
        assert_eq!(
            sidecar(&db_path)["descriptor"]["model"],
            serde_json::json!("ModelA")
        );
        drop(seed);

        // The user configured ModelB; the daemon is stopped, so the
        // fallback arm runs: it constructs its store unbound, builds
        // the configured embedder, and binds its descriptor.
        write_config("ModelB");
        let vector_store: Arc<dyn VectorStore> = Arc::new(
            LadybugStore::for_testing(&db_path, 4)
                .expect("reopen temp database")
                .with_embed_config_dir(&config_dir),
        );
        let embedder: Arc<dyn crate::traits::Embedder> = Arc::new(FallbackModelBEmbedder);
        prepare_fallback_write_store(&vector_store, &embedder, "probe")
            .await
            .expect("the fallback write preparation inits the store under the serving model");
        let vector = embedder
            .embed("a memory added while the daemon is stopped")
            .await
            .expect("embed");
        let payload = crate::traits::MemoryPayload {
            content: "a memory added while the daemon is stopped".to_string(),
            memory_type: "fact".to_string(),
            location: String::new(),
            user_id: "system".to_string(),
            agent_name: Some("NeuroStrata".to_string()),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        vector_store
            .upsert("probe", "fallback-m1", vector, payload)
            .await
            .expect("the fallback add must adopt the pristine record and proceed");
        assert_eq!(
            sidecar(&db_path)["descriptor"]["model"],
            serde_json::json!("ModelB"),
            "the pristine record is adopted to the configured model"
        );
        assert_eq!(sidecar(&db_path)["has_data"], serde_json::json!(true));
        let found = vector_store
            .search("probe", vec![0.1, 0.2, 0.3, 0.4], 5, false, false)
            .await
            .expect("search under the adopted model");
        assert!(!found.is_empty(), "the added row is searchable");
        drop(vector_store);
        let _ = std::fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    /// The sibling fallback Edit arm shares the Add arm's shape —
    /// bind a serving descriptor, then write through `edit_memory`'s
    /// `upsert` with no init — so it shares the defect on any store
    /// whose recorded state is still pristine when its upsert runs
    /// (here: a row-bearing store whose sidecar still reads pristine,
    /// the stale-label state the store-state tests construct by hand).
    /// Its upsert is the same guard's subject, so the arm prepares
    /// its store through the same helper and the edit proceeds after
    /// the pristine record is adopted.
    #[tokio::test]
    async fn fallback_edit_on_pristine_labelled_store_adopts_and_edits() {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let root =
            std::env::temp_dir().join(format!("ns-fallback-edit-adopt-{}", uuid::Uuid::new_v4()));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let write_config = |model: &str| {
            std::fs::write(
                config_dir.join("embedders.json"),
                format!("[{{\"model_name\":\"{model}\",\"dimensions\":4}}]"),
            )
            .unwrap();
        };
        let db_path = root.join("ladybug.db");
        let descriptor = |model: &str| crate::traits::EmbeddingDescriptor {
            provider: "fastembed".to_string(),
            model: model.to_string(),
            revision: "onnx".to_string(),
            dims: 4,
            normalization: "unknown".to_string(),
            preprocessing: "unknown".to_string(),
            distance_metric: "cosine".to_string(),
        };
        let payload = |content: &str| crate::traits::MemoryPayload {
            content: content.to_string(),
            memory_type: "fact".to_string(),
            location: String::new(),
            user_id: "system".to_string(),
            agent_name: Some("NeuroStrata".to_string()),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };

        write_config("ModelA");
        let seed = LadybugStore::for_testing(&db_path, 4)
            .expect("open temp database")
            .with_embed_config_dir(&config_dir)
            .with_serving_descriptor(Some(descriptor("ModelA")));
        seed.init("probe").await.expect("seed init");
        seed.upsert("probe", "m1", vec![0.1, 0.2, 0.3, 0.4], payload("original"))
            .await
            .expect("seed upsert");
        drop(seed);
        // The stale label: the sidecar reads pristine under ModelA
        // although the table holds the seeded row.
        crate::model_choice::write_store_state_identity(
            &db_path,
            &crate::model_choice::StoreState::new(false, descriptor("ModelA")),
        )
        .expect("rewrite the sidecar to the pristine label");

        write_config("ModelB");
        let vector_store: Arc<dyn VectorStore> = Arc::new(
            LadybugStore::for_testing(&db_path, 4)
                .expect("reopen temp database")
                .with_embed_config_dir(&config_dir),
        );
        let embedder: Arc<dyn crate::traits::Embedder> = Arc::new(FallbackModelBEmbedder);
        prepare_fallback_write_store(&vector_store, &embedder, "probe")
            .await
            .expect("the fallback write preparation inits the store under the serving model");
        let outcome = crate::server::edit_memory(
            &*vector_store,
            &*embedder,
            "probe",
            "m1",
            "probe",
            "edited",
            "",
        )
        .await
        .expect("the fallback edit must adopt the pristine record and proceed");
        assert!(
            matches!(outcome, crate::server::EditOutcome::Edited),
            "the seeded row is edited: {outcome:?}"
        );
        let (_, edited) = vector_store
            .get("probe", "m1")
            .await
            .expect("get the edited row")
            .expect("the edited row exists");
        assert_eq!(edited.content, "edited");
        let content = std::fs::read_to_string(crate::model_choice::store_state_path(&db_path))
            .expect("sidecar exists");
        let sidecar: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            sidecar["descriptor"]["model"],
            serde_json::json!("ModelB"),
            "the pristine record is adopted to the configured model"
        );
        drop(vector_store);
        let _ = std::fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    /// Regression test for the Ingest arm of the issue #75 class
    /// (class census follow-up to Task 4): the daemon-stopped fallback
    /// `ingest` on a store recorded pristine under the default model,
    /// after the user configured a different model. Ingest writes via
    /// `ingest_directory` -> `vector_store.upsert`, the same guarded
    /// entry point as Add/Edit, so it needs the same bind+init
    /// preparation before any guarded write. This test exercises the
    /// arm's call-sequence contract at the helper seam (the arms are
    /// not invocable through `run()`): prepare via
    /// `prepare_fallback_write_store`, then a guarded upsert exactly
    /// as ingest performs it.
    #[tokio::test]
    async fn fallback_ingest_on_pristine_store_adopts_configured_model_and_writes() {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let root =
            std::env::temp_dir().join(format!("ns-fallback-ingest-adopt-{}", uuid::Uuid::new_v4()));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let write_config = |model: &str| {
            std::fs::write(
                config_dir.join("embedders.json"),
                format!("[{{\"model_name\":\"{model}\",\"dimensions\":4}}]"),
            )
            .unwrap();
        };
        let db_path = root.join("ladybug.db");
        let sidecar = |db: &std::path::Path| -> serde_json::Value {
            let content =
                std::fs::read_to_string(crate::model_choice::store_state_path(db)).unwrap();
            serde_json::from_str(&content).unwrap()
        };

        write_config("ModelA");
        let seed = LadybugStore::for_testing(&db_path, 4)
            .expect("open temp database")
            .with_embed_config_dir(&config_dir)
            .with_serving_descriptor(Some(crate::traits::EmbeddingDescriptor {
                provider: "fastembed".to_string(),
                model: "ModelA".to_string(),
                revision: "onnx".to_string(),
                dims: 4,
                normalization: "unknown".to_string(),
                preprocessing: "unknown".to_string(),
                distance_metric: "cosine".to_string(),
            }));
        seed.init("probe").await.expect("seed init");
        assert_eq!(sidecar(&db_path)["has_data"], serde_json::json!(false));
        assert_eq!(
            sidecar(&db_path)["descriptor"]["model"],
            serde_json::json!("ModelA")
        );
        drop(seed);

        write_config("ModelB");
        let vector_store: Arc<dyn VectorStore> = Arc::new(
            LadybugStore::for_testing(&db_path, 4)
                .expect("reopen temp database")
                .with_embed_config_dir(&config_dir),
        );
        let embedder: Arc<dyn crate::traits::Embedder> = Arc::new(FallbackModelBEmbedder);
        prepare_fallback_write_store(&vector_store, &embedder, "probe")
            .await
            .expect("the fallback write preparation inits the store under the serving model");
        let vector = embedder
            .embed("a node ingested while the daemon is stopped")
            .await
            .expect("embed");
        let payload = crate::traits::MemoryPayload {
            content: "a node ingested while the daemon is stopped".to_string(),
            memory_type: "fact".to_string(),
            location: String::new(),
            user_id: "system".to_string(),
            agent_name: Some("NeuroStrata".to_string()),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        vector_store
            .upsert("probe", "fallback-ingest-m1", vector, payload)
            .await
            .expect("the fallback ingest must adopt the pristine record and proceed");
        assert_eq!(
            sidecar(&db_path)["descriptor"]["model"],
            serde_json::json!("ModelB"),
            "the pristine record is adopted to the configured model"
        );
        assert_eq!(sidecar(&db_path)["has_data"], serde_json::json!(true));
        let found = vector_store
            .search("probe", vec![0.1, 0.2, 0.3, 0.4], 5, false, false)
            .await
            .expect("search under the adopted model");
        assert!(!found.is_empty(), "the ingested row is searchable");
        drop(vector_store);
        let _ = std::fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    /// Regression test for the issue #96 P8-1 / #85 arm class: the
    /// daemon-down fallback `SetMetadata` and `Archive` arms mutate the
    /// store through `set_metadata` / `archive_memory`, both of which
    /// write via `upsert` — the same guarded entry point Add, Edit, and
    /// Ingest already prepare for. On a store whose recorded state is
    /// stale (here: a row-bearing store whose sidecar still reads
    /// pristine under the pre-switch model), the guard refuses the
    /// write with restart guidance this path could never satisfy. The
    /// arms now build their embedder and call `prepare_fallback_write_store`
    /// before any mutation, so this test exercises the arm's
    /// call-sequence contract at the helper seam (the arms are not
    /// invocable through `run()`): prepare via the helper, then the
    /// guarded metadata write and the guarded archive exactly as the
    /// arms perform them.
    #[tokio::test]
    async fn fallback_write_arms_prepare_store_before_mutating() {
        let previous = std::env::var("NEUROSTRATA_MODEL").ok();
        std::env::remove_var("NEUROSTRATA_MODEL");
        let root =
            std::env::temp_dir().join(format!("ns-fallback-write-arms-{}", uuid::Uuid::new_v4()));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let write_config = |model: &str| {
            std::fs::write(
                config_dir.join("embedders.json"),
                format!("[{{\"model_name\":\"{model}\",\"dimensions\":4}}]"),
            )
            .unwrap();
        };
        let db_path = root.join("ladybug.db");
        let descriptor = |model: &str| crate::traits::EmbeddingDescriptor {
            provider: "fastembed".to_string(),
            model: model.to_string(),
            revision: "onnx".to_string(),
            dims: 4,
            normalization: "unknown".to_string(),
            preprocessing: "unknown".to_string(),
            distance_metric: "cosine".to_string(),
        };
        let payload = |content: &str| crate::traits::MemoryPayload {
            content: content.to_string(),
            memory_type: "fact".to_string(),
            location: String::new(),
            user_id: "system".to_string(),
            agent_name: Some("NeuroStrata".to_string()),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };

        write_config("ModelA");
        let seed = LadybugStore::for_testing(&db_path, 4)
            .expect("open temp database")
            .with_embed_config_dir(&config_dir)
            .with_serving_descriptor(Some(descriptor("ModelA")));
        seed.init("probe").await.expect("seed init");
        seed.upsert("probe", "m1", vec![0.1, 0.2, 0.3, 0.4], payload("original"))
            .await
            .expect("seed upsert");
        drop(seed);
        // The stale label: the sidecar reads pristine under ModelA
        // although the table holds the seeded row.
        crate::model_choice::write_store_state_identity(
            &db_path,
            &crate::model_choice::StoreState::new(false, descriptor("ModelA")),
        )
        .expect("rewrite the sidecar to the pristine label");

        write_config("ModelB");
        let vector_store: Arc<dyn VectorStore> = Arc::new(
            LadybugStore::for_testing(&db_path, 4)
                .expect("reopen temp database")
                .with_embed_config_dir(&config_dir),
        );
        let embedder: Arc<dyn crate::traits::Embedder> = Arc::new(FallbackModelBEmbedder);
        prepare_fallback_write_store(&vector_store, &embedder, "probe")
            .await
            .expect("the fallback write preparation inits the store under the serving model");
        let outcome = crate::server::set_metadata(
            &*vector_store,
            "probe",
            "m1",
            serde_json::json!({ "k": "v" }),
            "admin",
        )
        .await
        .expect("the fallback SetMetadata arm must adopt the pristine record and proceed");
        assert!(
            matches!(outcome, crate::server::MetadataOutcome::Updated),
            "the metadata write lands: {outcome:?}"
        );
        let (_, row) = vector_store
            .get("probe", "m1")
            .await
            .expect("read the updated row")
            .expect("the row exists");
        assert_eq!(
            row.metadata.get("k").and_then(serde_json::Value::as_str),
            Some("v"),
            "the fallback SetMetadata write survives the metadata round-trip"
        );
        let bus = Arc::new(crate::events::ThalamicBus::new(crate::daemon::BUS_CAPACITY));
        let archive = crate::handlers::archive_memory::archive_memory(
            serde_json::json!({ "namespace": "probe", "id": "m1" }),
            vector_store.clone(),
            bus,
        )
        .await;
        assert!(
            archive.is_archived(),
            "the fallback Archive arm must retire the row the same metadata write left behind: {archive:?}"
        );
        drop(vector_store);
        let _ = std::fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("NEUROSTRATA_MODEL", value),
            None => std::env::remove_var("NEUROSTRATA_MODEL"),
        }
    }

    fn restore_copy_fixture(
        tag: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("ns-restore-copy-{}-{}", tag, uuid::Uuid::new_v4()));
        let snapshot_dir = root.join("snapshot");
        let target = root.join("target").join("ladybug.store");
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(snapshot_dir.join("ladybug.store"), b"snapshot-db").unwrap();
        (root, snapshot_dir, target)
    }

    fn restore_target_wal_path(target: &std::path::Path) -> std::path::PathBuf {
        let mut wal = target.as_os_str().to_os_string();
        wal.push(".wal");
        std::path::PathBuf::from(wal)
    }

    #[test]
    fn restore_copy_drops_orphan_target_wal_and_sidecar_when_snapshot_has_none() {
        let (root, snapshot_dir, target) = restore_copy_fixture("orphans");
        // The target DB file is absent (the Restore arm's guard passes),
        // but the old store's WAL and sidecar were left behind as orphans.
        let target_wal = restore_target_wal_path(&target);
        let target_sidecar = crate::model_choice::store_state_path(&target);
        std::fs::write(&target_wal, b"orphan-wal").unwrap();
        std::fs::write(&target_sidecar, b"orphan-sidecar").unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(std::fs::read(&target).unwrap(), b"snapshot-db");
        assert!(
            !target_wal.exists(),
            "the orphan target WAL must not survive a restore whose snapshot has none"
        );
        assert!(
            !target_sidecar.exists(),
            "the orphan target sidecar must not survive a restore whose snapshot has none"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_overwrites_orphan_target_wal_and_sidecar_from_snapshot() {
        let (root, snapshot_dir, target) = restore_copy_fixture("overwrite");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"snapshot-wal").unwrap();
        std::fs::write(
            snapshot_dir.join("ladybug.store.store-state.json"),
            b"snapshot-sidecar",
        )
        .unwrap();
        let target_wal = restore_target_wal_path(&target);
        let target_sidecar = crate::model_choice::store_state_path(&target);
        std::fs::write(&target_wal, b"orphan-wal").unwrap();
        std::fs::write(&target_sidecar, b"orphan-sidecar").unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(std::fs::read(&target).unwrap(), b"snapshot-db");
        assert_eq!(std::fs::read(&target_wal).unwrap(), b"snapshot-wal");
        assert_eq!(std::fs::read(&target_sidecar).unwrap(), b"snapshot-sidecar");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_without_snapshot_wal_or_sidecar_and_no_orphans_succeeds() {
        let (root, snapshot_dir, target) = restore_copy_fixture("plain");

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(std::fs::read(&target).unwrap(), b"snapshot-db");
        assert!(!restore_target_wal_path(&target).exists());
        assert!(!crate::model_choice::store_state_path(&target).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn restore_verify_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        // Built the way export builds a snapshot: a ladybug.store file,
        // a manifest.json carrying the snapshot format/version, and a
        // checksums.json written by write_checksums over the actual
        // bytes (which also writes the manifest.json.zst copy). No WAL
        // and no sidecar are written, so their checksums are absent —
        // verify_snapshot treats an absent, unmentioned WAL (and
        // sidecar) as not part of the snapshot.
        let root = std::env::temp_dir().join(format!(
            "ns-restore-verify-{}-{}",
            tag,
            uuid::Uuid::new_v4()
        ));
        let snapshot_dir = root.join("snapshot");
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        std::fs::write(snapshot_dir.join("ladybug.store"), b"snapshot-db").unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"provenance\":\"coordinated\",\"wal_kind\":\"none\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        crate::storage::snapshot::write_checksums(&snapshot_dir).unwrap();
        (root, snapshot_dir)
    }

    #[test]
    fn restore_verify_accepts_valid_snapshot() {
        let (root, snapshot_dir) = restore_verify_fixture("valid");

        ensure_snapshot_verified(&snapshot_dir).expect("a valid snapshot verifies");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_verify_rejects_tampered_store_bytes() {
        let (root, snapshot_dir) = restore_verify_fixture("tampered");
        std::fs::write(snapshot_dir.join("ladybug.store"), b"tampered-db").unwrap();

        let result = ensure_snapshot_verified(&snapshot_dir);

        assert!(
            result.is_err(),
            "store bytes that no longer match checksums.json must fail verification"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_verify_rejects_missing_manifest() {
        let (root, snapshot_dir) = restore_verify_fixture("no-manifest");
        std::fs::remove_file(snapshot_dir.join("manifest.json")).unwrap();

        let result = ensure_snapshot_verified(&snapshot_dir);

        assert!(
            result.is_err(),
            "a snapshot with no manifest.json must fail verification"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_verify_accepts_an_uncoordinated_snapshot_whose_integrity_holds() {
        // #209: a `--no-open` snapshot is exactly what an operator restores
        // after a crash, so the restore path accepts integrity even when the
        // snapshot is not coordinated. It must not, however, claim full
        // validity: the pair was captured without the store lock.
        let (root, snapshot_dir) = restore_verify_fixture("uncoordinated");
        let manifest = std::fs::read_to_string(snapshot_dir.join("manifest.json")).unwrap();
        let manifest = manifest
            .replacen("\"coordinated\":true", "\"coordinated\":false", 1)
            .replacen(
                "\"provenance\":\"coordinated\"",
                "\"provenance\":\"unverified\"",
                1,
            );
        std::fs::write(snapshot_dir.join("manifest.json"), manifest).unwrap();
        crate::storage::snapshot::write_checksums(&snapshot_dir).unwrap();

        let report = crate::storage::verify_snapshot(&snapshot_dir).expect("integrity holds");
        assert!(report.integrity_ok);
        assert!(
            !report.valid,
            "an uncoordinated snapshot is not fully valid (#209)"
        );
        ensure_snapshot_verified(&snapshot_dir)
            .expect("an uncoordinated but intact snapshot stays restorable");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_store_lock_is_held_across_the_uncoordinated_copy_window() {
        // #241: the `--no-open` gate used to be a point-in-time probe: it took
        // the store lock, learned whether a daemon held it, and dropped the
        // handle before the first file copy. A daemon that started in between
        // was never seen, so the database, its WAL, and the checkpoint shadow
        // could be captured at different instants while every per-file check
        // still passed. The copy has to hold the lock for its whole window: a
        // competing lock must not be acquirable while it runs.
        let root = std::env::temp_dir().join(format!("ns-lock-window-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ladybug.store");
        std::fs::write(&db, b"db-bytes").unwrap();
        std::fs::write(root.join("ladybug.store.wal"), b"wal-bytes").unwrap();
        std::fs::write(root.join("ladybug.store.shadow"), b"shadow-bytes").unwrap();
        let out = root.join("snapshot");

        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = observed.clone();
        let lock_path = daemon_lock_path(&db);
        export_uncoordinated_copy_observed(&db, &out, &|member| {
            seen.lock().unwrap().push(member.to_string());
            // A daemon starting now must not be able to take the store lock.
            let contender = std::thread::spawn({
                let lock_path = lock_path.clone();
                move || {
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(&lock_path)
                        .expect("the lock file is reachable while the copy runs");
                    file.try_lock()
                }
            });
            let outcome = contender.join().expect("the contender thread finished");
            assert!(
                matches!(outcome, Err(std::fs::TryLockError::WouldBlock)),
                "a competing lock must not be acquirable during the copy window (member {member})"
            );
        })
        .expect("the uncoordinated copy holds the store lock");

        let members = observed.lock().unwrap().clone();
        assert!(
            members.iter().any(|m| m == "ladybug.store"),
            "the store was copied: {members:?}"
        );
        assert!(
            members.iter().any(|m| m == "ladybug.store.wal"),
            "the WAL was copied: {members:?}"
        );
        assert!(
            members.iter().any(|m| m == "ladybug.store.shadow"),
            "the checkpoint shadow was copied: {members:?}"
        );
        assert!(out.join("ladybug.store").exists(), "the snapshot carries the store");
        assert!(
            out.join("ladybug.store.wal").exists(),
            "the snapshot carries the WAL"
        );
        assert!(
            out.join("ladybug.store.shadow").exists(),
            "the snapshot carries the checkpoint shadow"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_open_backup_refuses_while_the_daemon_lock_is_held() {
        // #209: `--no-open` used to skip both coordination gates and copy the
        // mmapped database and its WAL at different instants. While a process
        // holds the store lock the store may be mid-checkpoint, so the
        // uncoordinated file copy is refused; a quiescent store still exports.
        let root = std::env::temp_dir().join(format!("ns-no-open-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ladybug.store");

        let lock = open_daemon_lock(&db).expect("create the daemon lock");
        lock.lock().expect("hold the daemon lock");
        assert!(
            daemon_holds_lock(&db),
            "precondition: the lock probe sees the holder"
        );

        let err = hold_store_lock_for_copy(&db)
            .expect_err("--no-open must be refused while the store lock is held");
        assert!(
            err.to_string().contains("--no-open"),
            "the refusal names the flag: {err}"
        );
        assert!(
            err.to_string().contains("without --no-open"),
            "the refusal names the coordinated route: {err}"
        );
        drop(lock);

        let quiescent = root.join("quiescent.store");
        hold_store_lock_for_copy(&quiescent)
            .expect("a quiescent store still backs up without opening");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_open_backup_fails_closed_when_the_lock_cannot_be_taken() {
        // #209 (V5): a lock that cannot be taken is not "the lock is free".
        // The refusal must surface the failure rather than let the copy run
        // against a store whose owner cannot be established.
        let root = std::env::temp_dir().join(format!("ns-no-open-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ladybug.store");
        // A directory where the lock file belongs: the lock cannot be taken.
        std::fs::create_dir_all(daemon_lock_path(&db)).unwrap();

        let err = hold_store_lock_for_copy(&db)
            .expect_err("a lock that cannot be taken must refuse --no-open");
        let message = err.to_string();
        assert!(
            message.contains("lock"),
            "the refusal names the lock it could not take: {message}"
        );
        assert!(
            message.contains("--no-open"),
            "the refusal names the flag: {message}"
        );
        assert!(
            message.contains("without --no-open"),
            "the refusal names the coordinated route: {message}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_fallback_refuses_while_the_store_lock_is_held_and_copies_nothing() {
        // #209 (J7): the automatic fallback after a refused strict open copies
        // the same files as `--no-open`, so it must take the same lock gate —
        // and it must take it before the first file copy, not after.
        let root = std::env::temp_dir().join(format!("ns-fallback-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ladybug.store");
        std::fs::write(&db, b"db-bytes").unwrap();
        let out = root.join("snapshot");

        let lock = open_daemon_lock(&db).expect("create the daemon lock");
        lock.lock().expect("hold the daemon lock");

        let err = export_uncoordinated_fallback(&db, &out)
            .expect_err("the fallback refuses while the store lock is held");
        assert!(
            err.to_string().contains("--no-open"),
            "the refusal names the flag: {err}"
        );
        assert!(
            !out.exists(),
            "no snapshot directory is created when the fallback refuses"
        );
        drop(lock);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_fallback_refuses_when_the_lock_cannot_be_taken() {
        // An untakeable lock means "cannot show the store is free", so the
        // automatic fallback refuses and places no snapshot (#209, J7).
        let root = std::env::temp_dir().join(format!("ns-fallback-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ladybug.store");
        std::fs::write(&db, b"db-bytes").unwrap();
        std::fs::create_dir_all(daemon_lock_path(&db)).unwrap();
        let out = root.join("snapshot");

        let err = export_uncoordinated_fallback(&db, &out)
            .expect_err("the fallback refuses a lock it cannot take");
        assert!(
            err.to_string().contains("lock"),
            "the refusal names the lock it could not take: {err}"
        );
        assert!(
            !out.exists(),
            "no snapshot directory is created when the fallback refuses"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_no_open_copy_refuses_while_the_lock_is_held_and_copies_nothing() {
        // The gate has to run before the first `export_database_files` call: a
        // check that fires after the copy is not a gate (#209).
        let root = std::env::temp_dir().join(format!("ns-no-open-arm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ladybug.store");
        std::fs::write(&db, b"db-bytes").unwrap();
        let out = root.join("snapshot");

        let lock = open_daemon_lock(&db).expect("create the daemon lock");
        lock.lock().expect("hold the daemon lock");

        let err = export_uncoordinated_copy(&db, &out)
            .expect_err("the no-open copy refuses while the store lock is held");
        assert!(
            err.to_string().contains("--no-open"),
            "the refusal names the flag: {err}"
        );
        assert!(
            !out.exists(),
            "no snapshot directory is created when the copy refuses"
        );
        drop(lock);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_places_both_wal_sides_and_drops_stale_ones() {
        // #209 (V5): a `wal_kind: both` snapshot restores both byte streams
        // under their own names. The restore set owns both companion names, so
        // whatever the target held before is replaced or dropped as a unit.
        let (root, snapshot_dir, target) = restore_copy_fixture("both-wal");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"active-bytes").unwrap();
        std::fs::write(
            snapshot_dir.join("ladybug.store.wal.checkpoint"),
            b"frozen-bytes",
        )
        .unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"both\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        // Stale one-sided state at the target: an old active WAL, an old
        // frozen WAL, and a shadow the snapshot does not carry. All three are
        // superseded by the restore set.
        std::fs::write(restore_companion_path(&target, ".wal"), b"stale-active").unwrap();
        std::fs::write(
            restore_companion_path(&target, ".wal.checkpoint"),
            b"stale-frozen",
        )
        .unwrap();
        std::fs::write(restore_companion_path(&target, ".shadow"), b"stale-shadow").unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(std::fs::read(&target).unwrap(), b"snapshot-db");
        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".wal")).unwrap(),
            b"active-bytes",
            "the active side lands under <db>.wal"
        );
        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".wal.checkpoint")).unwrap(),
            b"frozen-bytes",
            "the frozen side lands under <db>.wal.checkpoint"
        );
        assert!(
            !restore_companion_path(&target, ".shadow").exists(),
            "a stale shadow the snapshot does not carry is dropped"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_refuses_a_both_wal_snapshot_missing_a_side() {
        // Fail closed: a manifest that promises both sides but carries one must
        // not restore as a homogeneous pair.
        let (root, snapshot_dir, target) = restore_copy_fixture("both-missing");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"active-bytes").unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"both\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();

        let err = copy_snapshot_into_place(&snapshot_dir, &target)
            .expect_err("a both-kind snapshot with one side is refused");
        assert!(
            err.to_string().contains("frozen"),
            "the refusal names the missing side: {err}"
        );
        assert!(
            !target.exists(),
            "a refused restore places no store at the target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_reads_a_version_1_legacy_snapshot_as_active() {
        // Pre-#209 snapshots are version 1 with no `wal_kind`; their WAL was
        // the active `.wal`, so it keeps restoring under that name.
        let (root, snapshot_dir, target) = restore_copy_fixture("v1-legacy");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"legacy-wal").unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":1,\"created_at\":\"2026-01-01T00:00:00Z\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT
            ),
        )
        .unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".wal")).unwrap(),
            b"legacy-wal"
        );
        assert!(!restore_companion_path(&target, ".wal.checkpoint").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_refuses_a_version_1_manifest_claiming_both() {
        // #209 (J7): `both` needs format version 2. A version 1 manifest that
        // claims it is refused instead of trusted, because a v1 reader would
        // drop the frozen side.
        let (root, snapshot_dir, target) = restore_copy_fixture("v1-both");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"active-bytes").unwrap();
        std::fs::write(
            snapshot_dir.join("ladybug.store.wal.checkpoint"),
            b"frozen-bytes",
        )
        .unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":1,\"created_at\":\"2026-01-01T00:00:00Z\",\"wal_kind\":\"both\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT
            ),
        )
        .unwrap();

        let err = copy_snapshot_into_place(&snapshot_dir, &target)
            .expect_err("a version 1 manifest claiming both is refused");
        assert!(
            err.to_string().contains("version 1"),
            "the refusal names the version: {err}"
        );
        assert!(
            !target.exists(),
            "a refused restore places no store at the target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_refuses_a_version_2_manifest_without_wal_kind() {
        let (root, snapshot_dir, target) = restore_copy_fixture("v2-no-kind");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"wal-bytes").unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT
            ),
        )
        .unwrap();

        let err = copy_snapshot_into_place(&snapshot_dir, &target)
            .expect_err("a version 2 manifest without wal_kind is refused");
        assert!(
            err.to_string().contains("wal_kind"),
            "the refusal names the missing field: {err}"
        );
        assert!(
            !target.exists(),
            "a refused restore places no store at the target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_refuses_a_none_wal_snapshot_carrying_a_wal_member() {
        // #209 (Codex P2): the manifest says there is no WAL, so a WAL member
        // is refused before any file is staged rather than silently dropped.
        let (root, snapshot_dir, target) = restore_copy_fixture("none-with-wal");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"unexpected-wal").unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();

        let err = copy_snapshot_into_place(&snapshot_dir, &target)
            .expect_err("a none-wal snapshot carrying a WAL is refused");
        let message = err.to_string();
        assert!(
            message.contains("none") && message.contains("ladybug.store.wal"),
            "the refusal names the inconsistency: {message}"
        );
        assert!(
            !target.exists(),
            "a refused restore places no store at the target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_restores_a_clean_none_wal_snapshot() {
        let (root, snapshot_dir, target) = restore_copy_fixture("none-clean");
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"none\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(std::fs::read(&target).unwrap(), b"snapshot-db");
        assert!(
            !restore_companion_path(&target, ".wal").exists(),
            "a none-wal snapshot places no active WAL"
        );
        assert!(
            !restore_companion_path(&target, ".wal.checkpoint").exists(),
            "a none-wal snapshot places no frozen WAL"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_refuses_a_one_sided_snapshot_carrying_the_frozen_member() {
        // #209 (J7): `active` plus a leftover frozen member is not a pair this
        // reader owns; restoring it would place bytes the manifest never
        // promised.
        let (root, snapshot_dir, target) = restore_copy_fixture("active-with-frozen");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"active-bytes").unwrap();
        std::fs::write(
            snapshot_dir.join("ladybug.store.wal.checkpoint"),
            b"leftover-frozen",
        )
        .unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"active\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();

        let err = copy_snapshot_into_place(&snapshot_dir, &target)
            .expect_err("a one-sided snapshot carrying the frozen member is refused");
        assert!(
            err.to_string().contains("ladybug.store.wal.checkpoint"),
            "the refusal names the unexpected member: {err}"
        );
        assert!(
            !target.exists(),
            "a refused restore places no store at the target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_refuses_a_both_wal_snapshot_missing_the_active_side() {
        let (root, snapshot_dir, target) = restore_copy_fixture("both-missing-active");
        std::fs::write(
            snapshot_dir.join("ladybug.store.wal.checkpoint"),
            b"frozen-bytes",
        )
        .unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":true,\"wal_kind\":\"both\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();

        let err = copy_snapshot_into_place(&snapshot_dir, &target)
            .expect_err("a both-kind snapshot missing the active side is refused");
        assert!(
            err.to_string().contains("active"),
            "the refusal names the missing side: {err}"
        );
        assert!(
            !target.exists(),
            "a refused restore places no store at the target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn restore_companion_path(target: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        let mut path = target.as_os_str().to_os_string();
        path.push(suffix);
        std::path::PathBuf::from(path)
    }

    #[test]
    fn restore_copy_preserves_a_frozen_wal_and_the_checkpoint_shadow() {
        // #209: a snapshot of a crashed checkpoint restores its WAL under the
        // frozen name, together with the shadow and pinned marker the engine
        // needs to replay the checkpointed prefix.
        let (root, snapshot_dir, target) = restore_copy_fixture("frozen");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"frozen-wal").unwrap();
        std::fs::write(snapshot_dir.join("ladybug.store.shadow"), b"shadow-pages").unwrap();
        std::fs::write(
            snapshot_dir.join("ladybug.store.pinned.tmp"),
            b"pinned-marker",
        )
        .unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"frozen\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(std::fs::read(&target).unwrap(), b"snapshot-db");
        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".wal.checkpoint")).unwrap(),
            b"frozen-wal",
            "a frozen checkpoint WAL is restored under its frozen name (#209)"
        );
        assert!(
            !restore_companion_path(&target, ".wal").exists(),
            "a frozen WAL must never be restored as the active WAL"
        );
        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".shadow")).unwrap(),
            b"shadow-pages",
            "the checkpoint shadow is restored beside the store"
        );
        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".pinned.tmp")).unwrap(),
            b"pinned-marker",
            "the pinned-checkpoint marker is restored beside the store"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_keeps_an_active_wal_active() {
        // Legacy snapshots carry no `wal_kind`; their WAL was copied from the
        // active `<db>.wal`, so it restores under that name as before.
        let (root, snapshot_dir, target) = restore_copy_fixture("active");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"active-wal").unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".wal")).unwrap(),
            b"active-wal"
        );
        assert!(!restore_companion_path(&target, ".wal.checkpoint").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_copy_drops_a_stale_active_wal_when_the_snapshot_wal_is_frozen() {
        // A leftover active `.wal` beside the target would pair with the
        // restored frozen `.wal.checkpoint` and make the engine refuse the
        // store, so the restore set owns both names (#209).
        let (root, snapshot_dir, target) = restore_copy_fixture("stale-active");
        std::fs::write(snapshot_dir.join("ladybug.store.wal"), b"frozen-wal").unwrap();
        std::fs::write(
            snapshot_dir.join("manifest.json"),
            format!(
                "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\",\"coordinated\":false,\"provenance\":\"unverified\",\"wal_kind\":\"frozen\"}}",
                crate::storage::snapshot::SNAPSHOT_FORMAT,
                crate::storage::snapshot::SNAPSHOT_VERSION
            ),
        )
        .unwrap();
        std::fs::write(
            restore_companion_path(&target, ".wal"),
            b"orphan-active-wal",
        )
        .unwrap();

        copy_snapshot_into_place(&snapshot_dir, &target).expect("restore copy");

        assert!(
            !restore_companion_path(&target, ".wal").exists(),
            "a stale active WAL must not survive a frozen-WAL restore"
        );
        assert_eq!(
            std::fs::read(restore_companion_path(&target, ".wal.checkpoint")).unwrap(),
            b"frozen-wal"
        );
        let _ = std::fs::remove_dir_all(&root);
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
            if let Err(e) = crash::install_alt_stack() {
                tracing::warn!("install_alt_stack failed: {e}");
            }
        })
        .build()?)
}

/// The uncoordinated copy, with an observer that is told which member was
/// just copied.
///
/// The observer runs while the copy is still in flight, so a caller can ask
/// what the store looks like *during* the copy window rather than only
/// before or after it.
///
/// The store lock is held for the whole window (#241): the guard is dropped
/// only after the export returns, so a daemon that starts mid-copy cannot
/// change the database, its WAL, or the checkpoint shadow under the
/// snapshot's feet.
fn export_uncoordinated_copy_observed(
    db_path: &std::path::Path,
    dir: &std::path::Path,
    during_copy: &dyn Fn(&str),
) -> anyhow::Result<()> {
    let _lock = hold_store_lock_for_copy(db_path)?;
    LadybugStore::export_database_files_observed(db_path, dir, during_copy)
}

/// The uncoordinated copy: the store lock is taken and held for the whole
/// window (#241).
fn export_uncoordinated_copy(
    db_path: &std::path::Path,
    dir: &std::path::Path,
) -> anyhow::Result<()> {
    export_uncoordinated_copy_observed(db_path, dir, &|_| {})
}

/// The automatic no-open fallback used when a strict open refuses the store
/// (#196). It copies exactly the same files as `--no-open`, so it takes the
/// same lock gate before the first copy (#209) and holds the store lock for
/// the whole copy window (#241).
fn export_uncoordinated_fallback(
    db_path: &std::path::Path,
    dir: &std::path::Path,
) -> anyhow::Result<()> {
    export_uncoordinated_copy(db_path, dir)
}

/// Verify a snapshot directory before a restore copies anything from it.
///
/// Export writes `manifest.json` and `checksums.json` precisely so an
/// import can check them; restoring without this call would import a
/// tampered or corrupt snapshot — including its store-state sidecar, a
/// checksummed member — as recorded identity with no integrity check.
/// Err carries the verification error, or the report's error text when
/// verification completed but judged the snapshot invalid.
fn ensure_snapshot_verified(dir: &std::path::Path) -> anyhow::Result<()> {
    match crate::storage::verify_snapshot(dir) {
        Ok(report) if report.integrity_ok => {
            // #209: an uncoordinated (`--no-open`) snapshot is precisely what
            // an operator restores after a crash, so integrity is the bar for
            // restoring. It is not a full `valid` verdict, and the restore
            // says so rather than silently promoting the snapshot.
            if !report.coordinated {
                eprintln!(
                    "WARN: snapshot in {} is uncoordinated ({}): its bytes match their checksums, but it was taken without the store lock, so the database, WAL, and shadow came from separate instants.",
                    dir.display(),
                    report.provenance
                );
            }
            Ok(())
        }
        Ok(report) => Err(anyhow::anyhow!(
            "{}",
            report
                .error
                .unwrap_or_else(|| "snapshot is not valid".to_string())
        )),
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}

/// Copy a snapshot's files into place for a restore: the store file, its
/// WAL under the role the snapshot recorded, the checkpoint shadow and its
/// pinned marker, and the store-state sidecar. Extracted from the Restore arm
/// so the copy logic is testable without driving the whole CLI.
///
/// The copies are staged at temporary sibling paths (`<target>.restore-tmp-<pid>`)
/// and renamed into place only after the whole set copied: a companion
/// copy that fails must not leave the store file (or a half-written
/// companion) at the target, and a failed restore must not trample a
/// pre-existing store's companions (issue #87).
///
/// Companion names belong to the restore set as a group (#209): a snapshot
/// whose WAL was frozen restores `<db>.wal.checkpoint`, so a stale
/// `<db>.wal` at the target is removed with the set rather than left to pair
/// with it; a mixed pair (`wal_kind: both`) restores both streams. Snapshots
/// written before #209 carry no `wal_kind`; their WAL was copied from the
/// active `<db>.wal`, so they restore under that name.
fn copy_snapshot_into_place(
    snapshot_dir: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<()> {
    let pid = std::process::id();
    let appended = |base: &std::path::Path, suffix: &str| {
        let mut os = base.as_os_str().to_os_string();
        os.push(suffix);
        std::path::PathBuf::from(os)
    };

    // Which name(s) the snapshot's WAL belongs under (#209). Version 1 has no
    // role and its WAL was the active `.wal`; version 2 requires the role and
    // is the only one that can express a mixed pair. Reading the rules here as
    // well as in `verify_snapshot` keeps a direct caller from restoring a
    // snapshot the verifier would refuse.
    let (version, wal_kind) = snapshot_version_and_wal_kind(snapshot_dir)?;
    if !crate::storage::snapshot::snapshot_version_supported(version) {
        return Err(anyhow::anyhow!(
            "unsupported snapshot format version {}; this build reads version {} and legacy version {}",
            version,
            crate::storage::snapshot::SNAPSHOT_VERSION,
            crate::storage::snapshot::SNAPSHOT_VERSION_LEGACY
        ));
    }
    let legacy = version == crate::storage::snapshot::SNAPSHOT_VERSION_LEGACY;
    if legacy && wal_kind.as_deref() == Some(crate::storage::snapshot::WAL_KIND_BOTH) {
        return Err(anyhow::anyhow!(
            "version {} snapshot claims wal_kind \"both\", which only format version {} can express; a v1 reader would verify it and drop the frozen side",
            crate::storage::snapshot::SNAPSHOT_VERSION_LEGACY,
            crate::storage::snapshot::SNAPSHOT_VERSION
        ));
    }
    if !legacy && wal_kind.is_none() {
        return Err(anyhow::anyhow!(
            "version {} snapshot records no wal_kind; version {} manifests must record active, frozen, both or none",
            version,
            crate::storage::snapshot::SNAPSHOT_VERSION
        ));
    }
    let declares_both = wal_kind.as_deref() == Some(crate::storage::snapshot::WAL_KIND_BOTH);
    let wal_suffix = match wal_kind.as_deref() {
        Some(crate::storage::snapshot::WAL_KIND_FROZEN) => ".wal.checkpoint",
        Some(crate::storage::snapshot::WAL_KIND_NONE) => "",
        _ => ".wal",
    };
    let snapshot_wal = snapshot_dir.join("ladybug.store.wal");
    let snapshot_checkpoint_wal = snapshot_dir.join("ladybug.store.wal.checkpoint");
    // #209 (Codex P2): `none` promises no WAL member. Refuse it before staging
    // instead of restoring the store and silently leaving the WAL out.
    if wal_kind.as_deref() == Some(crate::storage::snapshot::WAL_KIND_NONE) && snapshot_wal.exists()
    {
        return Err(anyhow::anyhow!(
            "the snapshot records wal_kind \"none\" but carries ladybug.store.wal; refusing to restore a WAL the manifest says does not exist",
        ));
    }
    if declares_both && !snapshot_wal.exists() {
        return Err(anyhow::anyhow!(
            "the snapshot records wal_kind \"both\" but carries no active ladybug.store.wal"
        ));
    }
    if declares_both && !snapshot_checkpoint_wal.exists() {
        return Err(anyhow::anyhow!(
            "the snapshot records wal_kind \"both\" but carries no frozen ladybug.store.wal.checkpoint"
        ));
    }
    if !declares_both && snapshot_checkpoint_wal.exists() {
        return Err(anyhow::anyhow!(
            "the snapshot carries ladybug.store.wal.checkpoint but its wal_kind is {:?}; a mixed pair is recorded as \"both\"",
            wal_kind
        ));
    }

    // (snapshot member, target path) for every file the restore may place.
    let mut members: Vec<(std::path::PathBuf, std::path::PathBuf)> =
        vec![(snapshot_dir.join("ladybug.store"), target.to_path_buf())];
    if !wal_suffix.is_empty() {
        members.push((snapshot_wal.clone(), appended(target, wal_suffix)));
    }
    if declares_both {
        members.push((snapshot_checkpoint_wal, appended(target, ".wal.checkpoint")));
    }
    members.push((
        snapshot_dir.join("ladybug.store.store-state.json"),
        crate::model_choice::store_state_path(target),
    ));
    members.push((
        snapshot_dir.join("ladybug.store.shadow"),
        appended(target, ".shadow"),
    ));
    members.push((
        snapshot_dir.join("ladybug.store.pinned.tmp"),
        appended(target, ".pinned.tmp"),
    ));

    // Every companion name the target could hold, whether or not this
    // snapshot produces it: stale siblings of the wrong role, or from an
    // older store, must not survive the commit.
    let companion_suffixes = [
        ".wal",
        ".wal.checkpoint",
        ".store-state.json",
        ".shadow",
        ".pinned.tmp",
    ];

    // Step A0: refuse a directory at any companion path BEFORE touching
    // anything. remove_file/rename cannot succeed there, so the commit would
    // fail partway and leave the target half-committed.
    for suffix in companion_suffixes {
        let path = appended(target, suffix);
        if path.is_dir() {
            return Err(anyhow::anyhow!(
                "the restore target's {} path is a directory: {}",
                suffix,
                path.display()
            ));
        }
    }

    // Step A: stage every member the snapshot carries beside the target.
    let mut staged: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    for (index, (from, to)) in members.iter().enumerate() {
        if index > 0 && !from.exists() {
            continue;
        }
        let stage = appended(to, &format!(".restore-tmp-{pid}"));
        if let Err(e) = std::fs::copy(from, &stage) {
            for (_, staged_path) in &staged {
                let _ = std::fs::remove_file(staged_path);
            }
            let _ = std::fs::remove_file(&stage);
            return Err(e.into());
        }
        staged.push((to.clone(), stage));
    }

    // Step A2: stash any pre-existing companion aside so the commit can
    // replace it or drop it, and a refused commit can put it back.
    let mut stashed: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    for suffix in companion_suffixes {
        let path = appended(target, suffix);
        if !path.exists() {
            continue;
        }
        let prev = appended(target, &format!("{}.prev-tmp-{pid}", suffix));
        if let Err(e) = std::fs::rename(&path, &prev) {
            for (_, staged_path) in &staged {
                let _ = std::fs::remove_file(staged_path);
            }
            for (prev_path, dest) in &stashed {
                let _ = std::fs::rename(prev_path, dest);
            }
            return Err(anyhow::anyhow!(
                "could not move the existing {} out of the way: {}",
                suffix,
                e
            ));
        }
        stashed.push((prev, path));
    }

    // Step B: commit the staged set with full rollback on error.
    let rollback = |committed: usize| {
        // Remove any target files created by a partial commit.
        for (dest, _) in staged.iter().take(committed) {
            let _ = std::fs::remove_file(dest);
        }
        // Restore stashed companions, if any.
        for (prev, dest) in &stashed {
            let _ = std::fs::rename(prev, dest);
        }
        for (_, stage) in &staged {
            let _ = std::fs::remove_file(stage);
        }
    };

    for (index, (dest, stage)) in staged.iter().enumerate() {
        if let Err(e) = std::fs::rename(stage, dest) {
            rollback(index);
            return Err(anyhow::anyhow!(
                "could not move the staged {} into place: {}",
                dest.display(),
                e
            ));
        }
    }

    // Step C: the commit succeeded; the stashed companions are now
    // superseded by the new set, so discard them.
    for (prev, _) in &stashed {
        let _ = std::fs::remove_file(prev);
    }
    Ok(())
}

/// The format version and WAL role a snapshot recorded in its manifest
/// (#209).
///
/// Version 1 (legacy, and a directory with no manifest at all — which
/// `ensure_snapshot_verified` refuses on the real restore path anyway) has no
/// `wal_kind`; its WAL member is the active `<db>.wal`. Version 2 requires the
/// role, because only v2 can express a mixed pair.
fn snapshot_version_and_wal_kind(
    snapshot_dir: &std::path::Path,
) -> anyhow::Result<(u32, Option<String>)> {
    let manifest_path = snapshot_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Ok((crate::storage::snapshot::SNAPSHOT_VERSION_LEGACY, None));
    }
    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        anyhow::anyhow!(
            "could not read the snapshot manifest {:?} to learn its WAL role: {}",
            manifest_path,
            e
        )
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&raw)?;
    let version = manifest
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Ok((
        version,
        manifest
            .get("wal_kind")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    ))
}

fn main() -> anyhow::Result<()> {
    let runtime = runtime()?;
    runtime.block_on(run())
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
            std::process::Command::new(exe)
                .arg("daemon")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;

            eprintln!(
                "Waiting for daemon to become ready (this may take a moment while models load)..."
            );

            // Wait for daemon to become ready. Bounded per attempt as well as
            // overall: an unbound port on this machine hangs rather than
            // refusing -- the behaviour DaemonProbe exists to describe -- so an
            // untimed send here could spend the whole "20 seconds max" inside
            // one call.
            let client = daemon_http_client();
            for _ in 0..200 {
                // 20 seconds max
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
            // 20s, and the proxy is told where the daemon came from so it waits
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
        // Deferred embedding-model choice: materialization
        // only ever records a pending choice; it is surfaced here, at
        // interactive dispatch, before the command runs — never on the
        // daemon/serving path, and not on the way to shutting down.
        if !matches!(command, Commands::Daemon { .. } | Commands::Shutdown) {
            surface_pending_model_choice();
        }
        match command {
            Commands::Daemon { profile } => {
                if profile {
                    procmem::enable_profile();
                }
                println!("NeuroStrata MCP Server initializing in DAEMON-ONLY mode...");
                let config = Config::from_default_path()?;
                // Pre-flight: refuse a data-bearing store under a
                // commensurably different configured model before the
                // daemon lock is taken, the embedder is built, or the
                // engine opens — a refused start leaves the store
                // byte-untouched. The store-level check on the true
                // serving descriptor remains the authoritative gate.
                crate::model_choice::check_daemon_open_identity(
                    &config.db_path,
                    &crate::embed::config_dir(),
                )?;
                // Taken before the database opens. `shutdown` waits for this lock
                // to be released rather than for the port to close.
                if DAEMON_LOCK.set(take_daemon_lock(&config.db_path)?).is_err() {
                    tracing::debug!("DAEMON_LOCK was already initialized");
                }
                let embedder = build_embedder()?;
                let vector_store: Arc<dyn VectorStore> = Arc::new(
                    LadybugStore::new(
                        config.db_path.to_string_lossy().to_string(),
                        embedder.dimensions(),
                    )?
                    .with_serving_descriptor(Some(embedder.embedding_descriptor())),
                );
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
                crate::judgment::laya_onnx::LayaOnnxProvider::fetch_model(&model_id, &target_dir)
                    .await?;
                println!(
                    "Laya model '{}' fetched and verified to '{}'",
                    model_id, target_dir
                );
            }
            Commands::Learn { action } => match action {
                LearnCommands::Status => {
                    let status = crate::learning::control::load_status(
                        &crate::learning::control::default_learning_dir(),
                    );
                    println!("{}", serde_json::to_string_pretty(&status)?);
                }
                LearnCommands::Pause => {
                    print_learn_control(crate::learning::control::LearningControlRequest {
                        action: crate::learning::control::LearningControlAction::Pause,
                        purge_observations: false,
                    });
                }
                LearnCommands::Resume => {
                    print_learn_control(crate::learning::control::LearningControlRequest {
                        action: crate::learning::control::LearningControlAction::Resume,
                        purge_observations: false,
                    });
                }
                LearnCommands::Rollback => {
                    print_learn_control(crate::learning::control::LearningControlRequest {
                        action: crate::learning::control::LearningControlAction::Rollback,
                        purge_observations: false,
                    });
                }
                LearnCommands::Reset {
                    purge_observations,
                    confirm_purge,
                } => {
                    if purge_observations {
                        let config = Config::load().unwrap();
                        let daemon_state = crate::daemon::daemon_status(&config.db_path);
                        if matches!(daemon_state, crate::daemon::DaemonState::Running { .. }) {
                            print_learn_control(crate::learning::control::LearningControlRequest {
                                action: crate::learning::control::LearningControlAction::Reset,
                                purge_observations: true,
                            });
                        } else {
                            let result =
                                crate::learning::control::purge_observations_offline(confirm_purge);
                            match result {
                                Ok(res) => {
                                    println!("{}", serde_json::to_string_pretty(&res).unwrap())
                                }
                                Err(e) => {
                                    eprintln!("{}", e);
                                    std::process::exit(1);
                                }
                            }
                        }
                    } else {
                        print_learn_control(crate::learning::control::LearningControlRequest {
                            action: crate::learning::control::LearningControlAction::Reset,
                            purge_observations,
                        });
                    }
                }
                LearnCommands::Train { version } => {
                    run_learn_train(&version).await?;
                }
            },
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
                    crate::synaptic_mesh::handle_synaptic_mesh_command(&action, &ctx, &mut svc);
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
            Commands::Kge {
                action:
                    KgeCommands::Train {
                        namespace,
                        all,
                        epochs,
                        dim,
                        kge_weight,
                    },
            } => {
                if !all && namespace.is_none() {
                    anyhow::bail!("--namespace <ns> or --all is required");
                }
                if all && namespace.is_some() {
                    anyhow::bail!("--namespace and --all are mutually exclusive");
                }
                if !(0.0..=0.3).contains(&kge_weight) {
                    anyhow::bail!("--kge-weight must be in [0, 0.3]");
                }
                // Run TransE KGE training over the namespace graph (spec §5.4).
                crate::kge::cli_train(
                    namespace.as_deref(),
                    all,
                    Some(epochs),
                    Some(dim),
                    Some(kge_weight),
                )?;
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
                    match client
                        .post("http://127.0.0.1:34343/shutdown")
                        .timeout(std::time::Duration::from_secs(10))
                        .send()
                        .await
                    {
                        Ok(res) => {
                            if !res.status().is_success() {
                                let status = res.status();
                                let body = res.text().await.unwrap_or_default();
                                eprintln!("Shutdown refused by daemon (HTTP {}): {}", status, body);
                                std::process::exit(1);
                            }
                        }
                        Err(e) => {
                            if e.is_connect() && !daemon_holds_lock(&db_path) {
                                println!("Nothing is listening on 127.0.0.1:34343 now -- either a daemon stopped as this ran, or there was never one to stop.");
                                return Ok(());
                            }
                            eprintln!("The stop request has not been acknowledged yet: {}. Waiting for the daemon to go anyway.", e);
                        }
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
                        let report = match std::fs::read_to_string(daemon_lock_path(&db_path)) {
                            Ok(report) => report,
                            Err(e) => {
                                tracing::debug!(
                                    "the daemon's final checkpoint at {} could not be read: {}",
                                    daemon_lock_path(&db_path).display(),
                                    e
                                );
                                String::new()
                            }
                        };
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
            Commands::Bench {
                suite,
                iterations,
                output,
                baseline,
                max_regression_pct,
                fail_on_regression,
                record_metrics,
            } => {
                let cfg = crate::bench::BenchConfig {
                    suite,
                    iterations,
                    output,
                    baseline,
                    max_regression_pct,
                    fail_on_regression,
                    record_metrics,
                };
                let receipt = crate::bench::run_benchmarks(&cfg).await?;
                println!("{}", serde_json::to_string_pretty(&receipt)?);
                if fail_on_regression {
                    if let Some(cmp) = &receipt.comparison {
                        if !cmp.passed {
                            std::process::exit(1);
                        }
                    }
                }
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
                        println!("lock: {}", if held { "held" } else { "free" });
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
            Commands::Backup { dir, no_open } => {
                // The database is single-writer. When a daemon holds it and
                // `--no-open` was not requested, ask the daemon to do the work
                // rather than fighting it for the lock.
                if daemon_running && !no_open {
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
                if !no_open && daemon_busy(probe, daemon_holds_lock(&config.db_path)) {
                    eprintln!("{}", DAEMON_BUSY_MESSAGE);
                    std::process::exit(1);
                }
                if no_open {
                    // #209: an uncoordinated copy is only honest when nobody
                    // else owns the store. While the daemon lock is held the
                    // database, its WAL, and the checkpoint shadow can be
                    // read at different instants, which is exactly the
                    // verify-but-cannot-restore pair this refuses. A
                    // coordinated backup goes through the daemon above.
                    //
                    // #241: the lock is not probed and released — it is held
                    // for the whole copy, so a daemon that starts mid-copy
                    // cannot change the files being copied. The guard is
                    // dropped only after the export returns.
                    let _lock = match hold_store_lock_for_copy(&config.db_path) {
                        Ok(lock) => lock,
                        Err(e) => {
                            eprintln!("{}", e);
                            std::process::exit(1);
                        }
                    };
                    LadybugStore::export_database_files(
                        &config.db_path,
                        std::path::Path::new(&dir),
                    )?;
                    println!("Backed up to {} (no-open file copy)", dir);
                    return Ok(());
                }
                // The width opens the store; exporting never embeds anything.
                // If strict WAL verification refuses to open the store (#196),
                // fall back to a no-open file-copy snapshot so the operator can
                // still capture a verified backup before recovery.
                match LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embed::configured_dimensions()?,
                ) {
                    Ok(store) => {
                        let vector_store: Arc<dyn VectorStore> = Arc::new(store);
                        vector_store.export_database(&dir).await?;
                        println!("Backed up to {}", dir);
                    }
                    Err(e) if e.to_string().contains("Refusing to open:") => {
                        eprintln!(
                            "WARN: store open refused by strict WAL verification ({}); falling back to --no-open file-copy backup so the unrecovered store and WAL are captured intact.",
                            e
                        );
                        export_uncoordinated_fallback(&config.db_path, std::path::Path::new(&dir))?;
                        println!("Backed up to {} (no-open file copy)", dir);
                    }
                    Err(e) => return Err(e),
                }
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
                    eprintln!(
                        "{:?} does not look like a snapshot: no ladybug.store inside it.",
                        dir
                    );
                    std::process::exit(1);
                }
                if let Err(e) = ensure_snapshot_verified(std::path::Path::new(&dir)) {
                    eprintln!("Snapshot verification failed: {}", e);
                    std::process::exit(1);
                }
                copy_snapshot_into_place(std::path::Path::new(&dir), &target)?;

                // Opened only after the copy, to verify the restored store
                // reads back and to report what it holds.
                let vector_store: Arc<dyn VectorStore> = match LadybugStore::new(
                    target.to_string_lossy().to_string(),
                    embed::configured_dimensions()?,
                ) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        // The snapshot verified, but the store it placed
                        // will not open — leaving it at the target would
                        // park an unopenable store where a live one was
                        // expected. Roll the copy back entirely so a
                        // failed restore leaves the target absent (issue
                        // #87); the snapshot itself is untouched and the
                        // restore can be retried.
                        let _ = std::fs::remove_file(&target);
                        // Every companion the restore set may have placed,
                        // in either WAL role plus the checkpoint state (#209):
                        // a failed restore leaves the target absent rather
                        // than parking an unopenable partial set.
                        for suffix in [".wal", ".wal.checkpoint", ".shadow", ".pinned.tmp"] {
                            let mut companion = target.as_os_str().to_os_string();
                            companion.push(suffix);
                            let _ = std::fs::remove_file(std::path::PathBuf::from(companion));
                        }
                        let _ =
                            std::fs::remove_file(crate::model_choice::store_state_path(&target));
                        return Err(e);
                    }
                };

                let namespaces = match vector_store.list_namespaces().await {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("restore list_namespaces check failed: {e}");
                        Vec::new()
                    }
                };
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
                    if skip_gate_value_skips(std::env::var("NEUROSTRATA_SKIP_GATE").ok().as_deref())
                    {
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

                    let cwd = std::env::current_dir().unwrap_or_default();
                    let scope = crate::task::gate::GateScope::capture(&cwd);
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
                                .json(&task_gate_request(&namespace, strict, &scope))
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
                                    let line = crate::task::hook_hygiene(&cwd);
                                    if !line.is_empty() {
                                        println!("{}", line);
                                    }
                                    direct_report(&config, &namespace, Some(&scope))
                                        .await
                                        .map(|report| report.gate_json())
                                }
                            }
                        }
                    };

                    // BB-2: a malformed reply must never read as a clean gate.
                    // The verdict is either a violations array, or the report
                    // simply says the verdict is unavailable -- which flows
                    // through the Err arm's strict-aware could-not-check exit.
                    let outcome = outcome.and_then(|body| {
                        body.get("violations")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .ok_or_else(|| {
                                format!(
                                    "the daemon's gate reply was malformed (missing violations array): {}",
                                    body
                                )
                            })
                    });

                    match outcome {
                        Ok(mut violations) => {
                            if strict && cwd.join("docs").join("manifest.json").is_file() {
                                let doc_report = crate::doc_gate::evaluate_doc_gate(&cwd, None);
                                if !doc_report.passed {
                                    for v in &doc_report.violations {
                                        violations.push(serde_json::json!({
                                            "kind": format!("doc_gate:{}", v.kind),
                                            "id": v.rule_id,
                                            "detail": format!("{}: {}", v.path, v.detail),
                                        }));
                                    }
                                }
                            }
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
                            Ok(resp) => {
                                tracing::debug!(
                                    "daemon /mcp task_validate answered HTTP {}; falling back to the direct report",
                                    resp.status()
                                );
                                None
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "the /mcp task_validate request to the daemon failed: {}; falling back to the direct report",
                                    e
                                );
                                None
                            }
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
                            Ok(config) => match direct_report(&config, &namespace, None).await {
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
                            anyhow::bail!(
                                "daemon /mcp task import returned HTTP {}: {}",
                                status,
                                body
                            );
                        }
                        serde_json::from_str(&body).map_err(|e| {
                            anyhow::anyhow!("invalid JSON from daemon /mcp task import: {e}")
                        })?
                    } else {
                        let config = Config::from_default_path()?;
                        if daemon_busy(probe, daemon_holds_lock(&config.db_path)) {
                            eprintln!("{}", DAEMON_BUSY_MESSAGE);
                            std::process::exit(1);
                        }
                        let embedder = build_embedder()?;
                        let vector_store: Arc<dyn VectorStore> = Arc::new(
                            LadybugStore::new(
                                config.db_path.to_string_lossy().to_string(),
                                embed::configured_dimensions()?,
                            )?
                            .with_serving_descriptor(Some(embedder.embedding_descriptor())),
                        );
                        let res = match crate::task::import_beads(
                            vector_store.clone(),
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
                        };
                        if vector_store.is_dirty() {
                            if let Err(e) = vector_store.checkpoint().await {
                                tracing::warn!("task import checkpoint failed: {e}");
                            }
                        }
                        res
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
                            println!("Backup timer installed: {}", report.backup_installed);
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
            Commands::DocGate { root, json } => {
                let target_root = root
                    .or_else(git_toplevel)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let report = crate::doc_gate::evaluate_doc_gate(&target_root, None);
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).unwrap_or_default()
                    );
                } else if report.passed {
                    println!(
                        "PASS doc-gate (connector={}, tiers={}, rules={})",
                        report.connector_id, report.checked_tiers, report.checked_rules
                    );
                } else {
                    eprintln!(
                        "FAIL doc-gate (connector={}, violations={}):",
                        report.connector_id,
                        report.violations.len()
                    );
                    for v in &report.violations {
                        eprintln!("  - [{} / {}] {}: {}", v.rule_id, v.kind, v.path, v.detail);
                    }
                }
                if !report.passed {
                    std::process::exit(1);
                }
                return Ok(());
            }
            Commands::Workflow { action } => {
                match action {
                    WorkflowCommands::Status { root, json: _ } => {
                        let target_root = root
                            .or_else(git_toplevel)
                            .or_else(|| std::env::current_dir().ok())
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        let connector =
                            crate::workflow::resolve_workflow_connector(&target_root);
                        let scope = connector.capture_scope(&target_root);
                        let (revision_id, dirty_state_hash) =
                            connector.capture_state_digest(&target_root);
                        let changed_paths = connector.changed_paths(&target_root);
                        let payload = serde_json::json!({
                            "connector_id": connector.connector_id(),
                            "scope": scope,
                            "state_digest": {
                                "revision_id": revision_id,
                                "dirty_state_hash": dirty_state_hash,
                            },
                            "changed_paths": changed_paths,
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&payload).unwrap_or_default()
                        );
                    }
                    WorkflowCommands::Transitions { json: _ } => {
                        let sample = crate::workflow::evaluate_workflow_transition(
                            crate::workflow::WorkflowStage::Unpreflighted,
                            crate::workflow::WorkflowEvent::LoadContext,
                        );
                        let payload = serde_json::json!({
                            "initial_stage": sample.from,
                            "after_load_context": sample.to,
                            "edges": crate::workflow::WORKFLOW_TABLE,
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&payload).unwrap_or_default()
                        );
                    }
                    WorkflowCommands::Install { root, json: _ } => {
                        let target_root = root
                            .or_else(git_toplevel)
                            .or_else(|| std::env::current_dir().ok())
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        let connector =
                            crate::workflow::resolve_workflow_connector(&target_root);
                        match connector.install_triggers(&target_root) {
                            Ok(report) => {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&report).unwrap_or_default()
                                );
                            }
                            Err(e) => {
                                eprintln!("ERROR: {e}");
                                std::process::exit(1);
                            }
                        }
                    }
                }
                return Ok(());
            }
            Commands::DataConnector { .. } => { /* handled after store open */ }
            Commands::Hooks { action } => match action {
                HooksCommands::Install { force } => {
                    let root = match git_toplevel().or_else(|| std::env::current_dir().ok()) {
                        Some(r) => r,
                        None => {
                            eprintln!("ERROR: cannot determine git repository root or current working directory.");
                            std::process::exit(1);
                        }
                    };
                    if !root.join(".git").exists() {
                        eprintln!(
                            "ERROR: {:?} is not a git repository; there is nowhere to install the pre-push hook.",
                            root
                        );
                        std::process::exit(1);
                    }
                    match install_pre_push_hook(&root, force) {
                        Ok(target) => {
                            let _ = install_pre_commit_hook(&root);
                            let repo_ns = root
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("default");
                            let _ = crate::task::harness_plugins::auto_install_detected_plugins(
                                &root, repo_ns, None,
                            );
                            println!(
                                "Installed the NeuroStrata task gate at {} (plus pre-commit ledger and detected harness plugins).",
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
                    let root = match git_toplevel().or_else(|| std::env::current_dir().ok()) {
                        Some(r) => r,
                        None => {
                            eprintln!("ERROR: cannot determine git repository root or current working directory.");
                            std::process::exit(1);
                        }
                    };
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

                // AW-C4: `--dry-run` is read-only — it never opens the store —
                // so it runs even while the daemon holds the lock.
                if let Commands::Ingest {
                    dry_run: true,
                    dir,
                    namespace,
                    schema_path,
                    dry_run_mode,
                } = &other
                {
                    let dir_str = cli_ingest_root(dir);
                    let dir_path_owned = std::path::PathBuf::from(&dir_str);
                    let schema_str = if let Some(path) = schema_path {
                        std::fs::read_to_string(path).unwrap_or_else(|e| {
                            eprintln!("Failed to read schema from {}: {}", path, e);
                            std::process::exit(1);
                        })
                    } else {
                        include_str!("schema.json").to_string()
                    };
                    let schema = match crate::parser::schema::ParserSchema::load(&schema_str) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to load schema: {}", e);
                            return Err(e.into());
                        }
                    };
                    let report = crate::parser::ingest::dry_run_directory(
                        &dir_path_owned,
                        &schema,
                        &dry_run_mode,
                    )
                    .await?;
                    println!("Namespace: {}", namespace);
                    println!("{}", report);
                    return Ok(());
                }

                // `storage status` and `storage verify-snapshot` are path-only
                // filesystem inspections (spec 2026-10-28-tdb2-storage-hygiene-design.md)
                // and must run without opening `LadybugStore` so they remain
                // available when strict WAL verification refuses an open or
                // when a daemon holds the lock (#196).
                if let Commands::Storage { action } = other {
                    match action {
                        StorageCommands::Status => {
                            let out = crate::server::handle_storage_status_from_config();
                            println!("{}", out);
                        }
                        StorageCommands::VerifySnapshot { dir } => {
                            match crate::storage::verify_snapshot(std::path::Path::new(&dir)) {
                                Ok(report) if report.valid => {
                                    println!("Verified snapshot in {}: valid", dir);
                                }
                                Ok(report) if report.integrity_ok => {
                                    // #209: the bytes match their checksums,
                                    // but the snapshot was taken without the
                                    // store lock. That is restorable and it is
                                    // not a full `valid` verdict.
                                    println!(
                                        "Verified snapshot in {}: integrity ok, uncoordinated ({}) -- the bytes match their checksums, but the database, WAL, and checkpoint state were captured without the store lock, so this is not a coordinated snapshot",
                                        dir, report.provenance
                                    );
                                }
                                Ok(report) => {
                                    eprintln!("Snapshot verification failed: {:?}", report.error);
                                    std::process::exit(1);
                                }
                                Err(e) => {
                                    eprintln!("Snapshot verification failed: {}", e);
                                    std::process::exit(1);
                                }
                            }
                        }
                    }
                    return Ok(());
                }

                if daemon_running {
                    // Read-only graph export is not a store mutation: it rides
                    // the daemon like backup does, instead of demanding a
                    // shutdown (the same trap as guinea-pig BUG-5).
                    if let Commands::ExportGraph {
                        exclude_superseded,
                        include_archived,
                        clear,
                        source_id: cli_source_id,
                        ..
                    } = &other
                    {
                        let target_path = graph_target
                            .as_ref()
                            .expect("export graph target resolved before daemon dispatch");
                        let anchor_root = std::env::var_os("NEUROSTRATA_ANCHOR_ROOT")
                            .map(std::path::PathBuf::from);
                        // AW-A2: `--source-id <id>` on the CLI scopes the
                        // export just like the env var it falls back to.
                        let source_id = cli_source_id
                            .clone()
                            .or_else(|| std::env::var("NEUROSTRATA_SOURCE_ID").ok());
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
                                .append_pair("all", "true")
                                .append_pair("clear", &clear.to_string());
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
                        Commands::Delete { namespace, id } => {
                            let res = daemon_operator_client()
                                .post(format!("http://{DAEMON_ADDR}/delete"))
                                .json(&serde_json::json!({ "namespace": namespace, "id": id }))
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("delete request failed: {e}"))?;
                            let status = res.status();
                            let body = res.text().await.unwrap_or_default();
                            if status.is_success() {
                                println!("Memory deleted successfully.");
                                return Ok(());
                            }
                            eprintln!("Delete failed: {}", body);
                            std::process::exit(1);
                        }
                        Commands::Archive { namespace, id } => {
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/memory/archive"))
                                .json(&serde_json::json!({ "namespace": namespace, "id": id }))
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("archive request failed: {e}"))?;
                            let status = res.status();
                            let body = res.text().await.unwrap_or_default();
                            if status.is_success() {
                                println!("{}", body);
                                return Ok(());
                            }
                            eprintln!("{}", body);
                            std::process::exit(1);
                        }
                        Commands::Edit {
                            namespace,
                            id,
                            new_namespace,
                            content,
                            location,
                        } => {
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/edit"))
                                .json(&serde_json::json!({
                                    "old_namespace": namespace,
                                    "id": id,
                                    "new_namespace": new_namespace,
                                    "content": content,
                                    "location": location,
                                }))
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("edit request failed: {e}"))?;
                            let status = res.status();
                            let body = res.text().await.unwrap_or_default();
                            if status.is_success() {
                                println!("Successfully edited memory {}", id);
                                return Ok(());
                            }
                            if status == reqwest::StatusCode::NOT_FOUND {
                                eprintln!("No memory with id {} in namespace {}.", id, namespace);
                                std::process::exit(1);
                            }
                            if status == reqwest::StatusCode::CONFLICT {
                                eprintln!("{} was written by directory ingestion, so the next ingest would overwrite an edit. Change the file and ingest again instead.", id);
                                std::process::exit(1);
                            }
                            eprintln!("Edit failed: {}", body);
                            std::process::exit(1);
                        }
                        Commands::SetMetadata {
                            namespace,
                            id,
                            metadata,
                        } => {
                            let new_metadata: serde_json::Value = serde_json::from_str(metadata)
                                .map_err(|e| anyhow::anyhow!("Invalid JSON metadata: {}", e))?;
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/set-metadata"))
                                .json(&serde_json::json!({
                                    "namespace": namespace,
                                    "id": id,
                                    "metadata": new_metadata,
                                }))
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("set-metadata request failed: {e}"))?;
                            let status = res.status();
                            let body = res.text().await.unwrap_or_default();
                            if status.is_success() {
                                println!("Successfully updated metadata for memory {}", id);
                                return Ok(());
                            }
                            if status == reqwest::StatusCode::NOT_FOUND {
                                eprintln!("No memory with id {} in namespace {}.", id, namespace);
                                std::process::exit(1);
                            }
                            if status == reqwest::StatusCode::CONFLICT {
                                eprintln!(
                                    "{} was written by directory ingestion and cannot be updated.",
                                    id
                                );
                                std::process::exit(1);
                            }
                            eprintln!("Set-metadata failed: {}", body);
                            std::process::exit(1);
                        }
                        Commands::Action { action } => {
                            let req_body = action_command_to_json(action)?;
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/action"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("action request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
                        }
                        Commands::Scenario { scenario } => {
                            let req_body = scenario_command_to_json(scenario)?;
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/scenario"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("scenario request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
                        }
                        Commands::ObjectView {
                            namespace,
                            id,
                            scenario_id,
                            include_history_limit,
                            evaluate_action_preconditions,
                            max_bytes,
                            principal,
                        } => {
                            let mut req_body = serde_json::json!({
                                "namespace": namespace,
                                "id": id,
                                "include_history_limit": include_history_limit,
                                "evaluate_action_preconditions": evaluate_action_preconditions,
                                "max_bytes": max_bytes,
                            });
                            if let Some(s) = scenario_id {
                                req_body["scenario_id"] = serde_json::json!(s);
                            }
                            if let Some(p) = principal {
                                req_body["principal"] = serde_json::json!(p);
                            }
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/object-view"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("object-view request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
                        }
                        Commands::Locate {
                            namespace,
                            query,
                            path_prefix,
                            symbols_only,
                            max_locations,
                            max_bytes,
                            project_root,
                            principal,
                        } => {
                            let mut req_body = serde_json::json!({
                                "namespace": namespace,
                                "query": query,
                                "symbols_only": symbols_only,
                                "max_locations": max_locations,
                                "max_bytes": max_bytes,
                            });
                            if let Some(p) = path_prefix {
                                req_body["path_prefix"] = serde_json::json!(p);
                            }
                            if let Some(r) = project_root {
                                req_body["project_root"] = serde_json::json!(r);
                            }
                            if let Some(p) = principal {
                                req_body["principal"] = serde_json::json!(p);
                            }
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/locate"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("locate request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
                        }
                        Commands::DiffTour {
                            namespace,
                            diff_file,
                            diff_text,
                            project_root,
                            max_blast_radius_callers,
                            principal,
                        } => {
                            let resolved_diff = if let Some(t) = diff_text {
                                Some(t.clone())
                            } else if let Some(f) = diff_file {
                                if f == "-" {
                                    use std::io::Read;
                                    let mut buf = String::new();
                                    let _ = std::io::stdin().read_to_string(&mut buf);
                                    Some(buf)
                                } else {
                                    std::fs::read_to_string(f).ok()
                                }
                            } else {
                                None
                            };
                            let mut req_body = serde_json::json!({
                                "namespace": namespace,
                                "max_blast_radius_callers": max_blast_radius_callers,
                            });
                            if let Some(d) = resolved_diff {
                                req_body["diff_text"] = serde_json::json!(d);
                            }
                            if let Some(r) = project_root {
                                req_body["project_root"] = serde_json::json!(r);
                            }
                            if let Some(p) = principal {
                                req_body["principal"] = serde_json::json!(p);
                            }
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/diff-tour"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("diff-tour request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
                        }
                        Commands::Insights {
                            namespace,
                            action,
                            name,
                            title,
                            query_kind,
                            query,
                            path_prefix,
                            from_ts,
                            to_ts,
                            backfill_points,
                            principal,
                        } => {
                            let mut req_body = serde_json::json!({
                                "namespace": namespace,
                                "action": action,
                            });
                            if let Some(v) = name {
                                req_body["name"] = serde_json::json!(v);
                            }
                            if let Some(v) = title {
                                req_body["title"] = serde_json::json!(v);
                            }
                            if let Some(v) = query_kind {
                                req_body["query_kind"] = serde_json::json!(v);
                            }
                            if let Some(v) = query {
                                req_body["query"] = serde_json::json!(v);
                            }
                            if let Some(v) = path_prefix {
                                req_body["path_prefix"] = serde_json::json!(v);
                            }
                            if let Some(v) = from_ts {
                                req_body["from_ts"] = serde_json::json!(v);
                            }
                            if let Some(v) = to_ts {
                                req_body["to_ts"] = serde_json::json!(v);
                            }
                            if let Some(v) = backfill_points {
                                req_body["backfill_points"] = serde_json::json!(v);
                            }
                            if let Some(v) = principal {
                                req_body["principal"] = serde_json::json!(v);
                            }
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/insights"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("insights request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
                        }
                        Commands::Taxonomy { taxonomy } => {
                            let req_body = taxonomy_command_to_json(taxonomy)?;
                            let res = daemon_http_client()
                                .post(format!("http://{DAEMON_ADDR}/taxonomy"))
                                .json(&req_body)
                                .send()
                                .await
                                .map_err(|e| anyhow::anyhow!("taxonomy request failed: {e}"))?;
                            let body = res.text().await.unwrap_or_default();
                            println!("{}", body);
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                return Ok(());
                            }
                            std::process::exit(1);
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
                let concrete_store: Arc<LadybugStore> = match LadybugStore::new(
                    config.db_path.to_string_lossy().to_string(),
                    embed::configured_dimensions()?,
                ) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        if matches!(other, Commands::Doctor { .. }) {
                            let status = crate::storage::compute_storage_status(
                                &config.db_path,
                                &config.storage_hygiene,
                            );
                            eprintln!(
                                "Doctor: store at {:?} could not be opened: {}",
                                config.db_path, e
                            );
                            eprintln!(
                                "Storage diagnostics (no-open): store_bytes={}, wal_bytes={}, wal_age_secs={:?}, wal_recovery_artifact_count={}, wal_recovery_artifact_bytes={}",
                                status.store_bytes,
                                status.wal_bytes,
                                status.wal_age_secs,
                                status.wal_recovery_artifact_count,
                                status.wal_recovery_artifact_bytes,
                            );
                        }
                        return Err(e);
                    }
                };
                let vector_store: Arc<dyn VectorStore> = concrete_store.clone();

                if let Commands::DataConnector { action } = &other {
                    let workspace_root =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let (namespace, args_json, principal) = match action {
                        DataConnectorCommands::List => (
                            "NeuroStrata".to_string(),
                            serde_json::json!({ "operation": "list_connectors" }),
                            None,
                        ),
                        DataConnectorCommands::Describe { connector_id, pool_id, principal } => (
                            "NeuroStrata".to_string(),
                            serde_json::json!({
                                "operation": "describe_pool",
                                "connector_id": connector_id,
                                "pool_id": pool_id,
                            }),
                            principal.clone(),
                        ),
                        DataConnectorCommands::RegisterPointer { namespace, payload, connector_id, pointer_id, pool_id, pointer_uri, entity_type, captured_at_secs, max_staleness_secs, principal } => {
                            let payload_value: serde_json::Value = match payload {
                                Some(raw) => {
                                    let text = if let Some(file_path) = raw.strip_prefix('@') {
                                        std::fs::read_to_string(file_path)
                                            .map_err(|e| anyhow::anyhow!("read payload file {}: {}", file_path, e))?
                                    } else {
                                        raw.clone()
                                    };
                                    serde_json::from_str(&text)
                                        .context("data-connector register-pointer payload must be valid JSON")?
                                }
                                None => serde_json::json!({}),
                            };
                            let pointer_id_value = pointer_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                            let record = serde_json::json!({
                                "pointer_id": pointer_id_value,
                                "connector_id": connector_id.clone().unwrap_or_else(|| "memory_pool".to_string()),
                                "pool_id": pool_id.clone().unwrap_or_else(|| "default".to_string()),
                                "pointer_uri": pointer_uri.clone().unwrap_or_else(|| "records".to_string()),
                                "entity_type": entity_type.clone().unwrap_or_else(|| "json_record".to_string()),
                                "expected_source_sha256": payload_value.get("expected_source_sha256").cloned().unwrap_or(serde_json::Value::Null),
                                "captured_at_secs": captured_at_secs.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
                                "max_staleness_secs": max_staleness_secs.unwrap_or(0),
                            });
                            (namespace.clone(), serde_json::json!({ "operation": "register_pointer", "record": record }), principal.clone())
                        }
                        DataConnectorCommands::Resolve { namespace, pointer_id, offset, limit, max_bytes, now_secs, principal } => (
                            namespace.clone(),
                            serde_json::json!({
                                "operation": "resolve_pointer",
                                "req": {
                                    "pointer_id": pointer_id,
                                    "offset": offset,
                                    "limit": limit,
                                    "max_bytes": max_bytes,
                                },
                                "now_secs": now_secs.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
                            }),
                            principal.clone(),
                        ),
                        DataConnectorCommands::ApplyEffect { namespace, pointer_id, operation, payload, action_receipt_id, idempotency_key, expected_source_sha256, now_secs, principal } => {
                            let payload_value: serde_json::Value = match payload {
                                Some(raw) => {
                                    let text = if let Some(file_path) = raw.strip_prefix('@') {
                                        std::fs::read_to_string(file_path)
                                            .map_err(|e| anyhow::anyhow!("read payload file {}: {}", file_path, e))?
                                    } else {
                                        raw.clone()
                                    };
                                    serde_json::from_str(&text)
                                        .context("data-connector apply-effect payload must be valid JSON")?
                                }
                                None => serde_json::json!({}),
                            };
                            (
                                namespace.clone(),
                                serde_json::json!({
                                    "operation": "apply_effect",
                                    "req": {
                                        "pointer_id": pointer_id,
                                        "operation": operation,
                                        "payload": payload_value,
                                        "action_receipt_id": action_receipt_id,
                                        "expected_source_sha256": expected_source_sha256,
                                        "idempotency_key": idempotency_key,
                                    },
                                    "now_secs": now_secs.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
                                }),
                                principal.clone(),
                            )
                        }
                        DataConnectorCommands::Mediate { namespace, from_stage, to_stage, gate_passed, pointer_id, offset, limit, max_bytes, now_secs, principal } => (
                            namespace.clone(),
                            serde_json::json!({
                                "operation": "mediate",
                                "from_stage": from_stage,
                                "to_stage": to_stage,
                                "gate_passed": gate_passed,
                                "slice_req": {
                                    "pointer_id": pointer_id,
                                    "offset": offset,
                                    "limit": limit,
                                    "max_bytes": max_bytes,
                                },
                                "now_secs": now_secs.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
                            }),
                            principal.clone(),
                        ),
                    };
                    let bus = Arc::new(crate::events::ThalamicBus::new(daemon::BUS_CAPACITY));
                    let kernel = crate::kernel::AgentKernel::new(concrete_store.clone(), bus, workspace_root);
                    match kernel
                        .execute_connector_command(&namespace, &args_json, principal.as_deref())
                        .await
                    {
                        Ok(value) => {
                            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
                        }
                        Err(e) => {
                            eprintln!("data-connector error: {}", e);
                            std::process::exit(1);
                        }
                    }
                    return Ok(());
                }

                let read = ReadSource::Direct(vector_store.clone());

                // CLI fallback: hook hygiene against the CWD git root (daemon down).
                let cwd = std::env::current_dir().unwrap_or_default();
                let line = crate::task::hook_hygiene(&cwd);
                if !line.is_empty() {
                    println!("{}", line);
                }

                match other {
                    Commands::Storage { .. } => unreachable!("handled before store open"),
                    Commands::Chat { action } => match action {
                        ChatCommands::Ask {
                            namespace,
                            question,
                            top_k,
                        } => {
                            let embedder = build_embedder()?;
                            bind_fallback_serving_descriptor(&vector_store, &embedder);
                            let args = serde_json::json!({
                                "namespace": namespace,
                                "question": question,
                                "top_k": top_k,
                            });
                            let out = crate::server::handle_chat_ask(
                                &args,
                                embedder.clone(),
                                vector_store.clone(),
                            )
                            .await;
                            println!("{}", out);
                        }
                    },
                    Commands::Metrics { action } => match action {
                        MetricsCommands::List { namespace } => {
                            let args = serde_json::json!({ "namespace": namespace });
                            let out =
                                crate::server::handle_metrics_list(&args, vector_store.clone())
                                    .await;
                            println!("{}", out);
                        }
                        MetricsCommands::Query {
                            namespace,
                            kind,
                            name,
                            limit,
                        } => {
                            let args = serde_json::json!({
                                "namespace": namespace,
                                "kind": kind,
                                "name": name,
                                "limit": limit,
                            });
                            let out =
                                crate::server::handle_metrics_query(&args, vector_store.clone())
                                    .await;
                            println!("{}", out);
                        }
                    },
                    Commands::Graphql { query, namespace } => {
                        let args = serde_json::json!({
                            "query": query,
                            "namespace": namespace,
                        });
                        let out =
                            crate::server::handle_graphql_query(&args, vector_store.clone()).await;
                        println!("{}", out);
                    }
                    Commands::Action { action } => {
                        let embedder = build_embedder()?;
                        bind_fallback_serving_descriptor(&vector_store, &embedder);
                        let args = action_command_to_json(&action)?;
                        let out = crate::server::handle_action(
                            &args,
                            embedder.clone(),
                            vector_store.clone(),
                        )
                        .await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::Scenario { scenario } => {
                        let embedder = build_embedder()?;
                        bind_fallback_serving_descriptor(&vector_store, &embedder);
                        let args = scenario_command_to_json(&scenario)?;
                        let out = crate::server::handle_scenario(
                            &args,
                            embedder.clone(),
                            vector_store.clone(),
                        )
                        .await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::ObjectView {
                        namespace,
                        id,
                        scenario_id,
                        include_history_limit,
                        evaluate_action_preconditions,
                        max_bytes,
                        principal,
                    } => {
                        let mut args = serde_json::json!({
                            "namespace": namespace,
                            "id": id,
                            "include_history_limit": include_history_limit,
                            "evaluate_action_preconditions": evaluate_action_preconditions,
                            "max_bytes": max_bytes,
                        });
                        if let Some(s) = scenario_id {
                            args["scenario_id"] = serde_json::json!(s);
                        }
                        if let Some(p) = principal {
                            args["principal"] = serde_json::json!(p);
                        }
                        let out =
                            crate::server::handle_object_view(&args, vector_store.clone()).await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::Locate {
                        namespace,
                        query,
                        path_prefix,
                        symbols_only,
                        max_locations,
                        max_bytes,
                        project_root,
                        principal,
                    } => {
                        let mut args = serde_json::json!({
                            "namespace": namespace,
                            "query": query,
                            "symbols_only": symbols_only,
                            "max_locations": max_locations,
                            "max_bytes": max_bytes,
                        });
                        if let Some(p) = path_prefix {
                            args["path_prefix"] = serde_json::json!(p);
                        }
                        if let Some(r) = project_root {
                            args["project_root"] = serde_json::json!(r);
                        }
                        if let Some(p) = principal {
                            args["principal"] = serde_json::json!(p);
                        }
                        let embedder = build_embedder().ok();
                        let out = crate::server::handle_locate_with_auth(
                            &args,
                            Some("admin"),
                            embedder.as_ref(),
                            vector_store.clone(),
                        )
                        .await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::DiffTour {
                        namespace,
                        diff_file,
                        diff_text,
                        project_root,
                        max_blast_radius_callers,
                        principal,
                    } => {
                        let resolved_diff = if let Some(t) = diff_text {
                            Some(t)
                        } else if let Some(f) = diff_file {
                            if f == "-" {
                                use std::io::Read;
                                let mut buf = String::new();
                                let _ = std::io::stdin().read_to_string(&mut buf);
                                Some(buf)
                            } else {
                                std::fs::read_to_string(&f).ok()
                            }
                        } else {
                            None
                        };
                        let mut args = serde_json::json!({
                            "namespace": namespace,
                            "max_blast_radius_callers": max_blast_radius_callers,
                        });
                        if let Some(d) = resolved_diff {
                            args["diff_text"] = serde_json::json!(d);
                        }
                        if let Some(r) = project_root {
                            args["project_root"] = serde_json::json!(r);
                        }
                        if let Some(p) = principal {
                            args["principal"] = serde_json::json!(p);
                        }
                        let out =
                            crate::server::handle_diff_tour(&args, vector_store.clone()).await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::Insights {
                        namespace,
                        action,
                        name,
                        title,
                        query_kind,
                        query,
                        path_prefix,
                        from_ts,
                        to_ts,
                        backfill_points,
                        principal,
                    } => {
                        let mut args = serde_json::json!({
                            "namespace": namespace,
                            "action": action,
                        });
                        if let Some(v) = name {
                            args["name"] = serde_json::json!(v);
                        }
                        if let Some(v) = title {
                            args["title"] = serde_json::json!(v);
                        }
                        if let Some(v) = query_kind {
                            args["query_kind"] = serde_json::json!(v);
                        }
                        if let Some(v) = query {
                            args["query"] = serde_json::json!(v);
                        }
                        if let Some(v) = path_prefix {
                            args["path_prefix"] = serde_json::json!(v);
                        }
                        if let Some(v) = from_ts {
                            args["from_ts"] = serde_json::json!(v);
                        }
                        if let Some(v) = to_ts {
                            args["to_ts"] = serde_json::json!(v);
                        }
                        if let Some(v) = backfill_points {
                            args["backfill_points"] = serde_json::json!(v);
                        }
                        if let Some(v) = principal {
                            args["principal"] = serde_json::json!(v);
                        }
                        let out =
                            crate::server::handle_insights(&args, vector_store.clone()).await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::Taxonomy { taxonomy } => {
                        let embedder = build_embedder()?;
                        bind_fallback_serving_descriptor(&vector_store, &embedder);
                        let args = taxonomy_command_to_json(&taxonomy)?;
                        let out = crate::server::handle_taxonomy(
                            &args,
                            embedder.clone(),
                            vector_store.clone(),
                        )
                        .await;
                        println!("{}", out);
                        let parsed: serde_json::Value =
                            serde_json::from_str(&out).unwrap_or_default();
                        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                            std::process::exit(1);
                        }
                    }
                    Commands::Doctor { namespace } => {
                        run_doctor(&read, namespace.as_deref()).await?;
                    }
                    Commands::Laya { .. } => {
                        unreachable!("handled before store open")
                    }
                    Commands::Namespaces => {
                        run_namespaces(&read).await?;
                    }
                    Commands::List { namespace } => {
                        run_list(&read, &namespace).await?;
                    }
                    // Reached only through the fallback; the dedicated Status
                    // arm exits before the store opens.
                    Commands::Status => unreachable!("handled before store open"),
                    Commands::IngestScip {
                        scip_path,
                        namespace,
                    } => {
                        let report = crate::parser::scip::ingest_scip_index(
                            vector_store.clone(),
                            &namespace,
                            std::path::Path::new(&scip_path),
                        )
                        .await?;
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    }
                    Commands::Ingest {
                        dir,
                        namespace,
                        schema_path,
                        dry_run: _,
                        dry_run_mode: _,
                    } => {
                        let embedder = build_embedder()?;
                        prepare_fallback_write_store(&vector_store, &embedder, &namespace)
                            .await?;
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

                        let schema = match crate::parser::schema::ParserSchema::load(&schema_str) {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!("Failed to load schema: {}", e);
                                return Err(e.into());
                            }
                        };
                        // Opt-in pre-ingest hygiene: drop the ingester-owned
                        // rows (and their lexical docs) so the rebuild starts
                        // from a clean tree, then surface the generation
                        // bookkeeping query alongside it.
                        if std::env::var("NEUROSTRATA_CLEAR_BEFORE_INGEST").is_ok() {
                            vector_store.clear_ingested(&namespace).await?;
                            if let Err(e) = vector_store.generation_nodes(&namespace, 0).await {
                                tracing::debug!("generation_nodes(0) probe note: {e}");
                            }
                        }
                        println!(
                            "Ingesting AST from {:?} into namespace '{}'",
                            dir_path, namespace
                        );
                        crate::parser::ingest::ingest_directory(
                            dir_path,
                            &schema,
                            embedder,
                            vector_store.clone(),
                            &namespace,
                            // The CLI has nowhere to report progress to: it
                            // is already printing each file as it goes.
                            None,
                            &[],
                        )
                        .await?;
                        println!("Ingestion complete.");
                    }
                    Commands::ExportGraph {
                        exclude_superseded,
                        include_archived,
                        clear,
                        source_id: cli_source_id,
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
                        if clear {
                            if let Err(e) = crate::events::clear_export_freshness(&*vector_store).await
                            {
                                eprintln!("Warning: failed to clear export freshness: {e}");
                            }
                        }
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
                        // AW-A2: `--source-id <id>` on the CLI scopes the
                        // export just like the env var it falls back to.
                        let source_id = cli_source_id
                            .clone()
                            .or_else(|| std::env::var("NEUROSTRATA_SOURCE_ID").ok());
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
                        if let Err(e) = crate::events::clear_export_freshness(&*vector_store).await {
                            eprintln!("Warning: failed to clear export freshness: {e}");
                        }
                    }
                    Commands::NormalizeMetadata { namespace } => {
                        let embedder = build_embedder()?;
                        prepare_fallback_write_store(&vector_store, &embedder, &namespace)
                            .await?;
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
                                skipped += 1;
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
                    Commands::RepairHygiene { namespace, all } => {
                        use crate::store::ladybug::LadybugStore;
                        let direct =
                            crate::traits::VectorStore::as_any(vector_store.as_ref())
                                .downcast_ref::<LadybugStore>()
                                .ok_or_else(|| {
                                    anyhow::anyhow!("repair-hygiene requires LadybugStore backend")
                                })?;
                        if all {
                            let namespaces = vector_store.list_namespaces().await?;
                            let mut total_shape = 0usize;
                            let mut total_dangling = 0usize;
                            for ns in &namespaces {
                                let (fixed_shape, fixed_dangling) =
                                    direct.repair_namespace_hygiene(ns).await?;
                                total_shape += fixed_shape;
                                total_dangling += fixed_dangling;
                            }
                            println!(
                                "Namespace hygiene repaired across {} namespaces: {} shape fixes, {} dangling fixes.",
                                namespaces.len(),
                                total_shape,
                                total_dangling
                            );
                        } else {
                            let ns = namespace.ok_or_else(|| anyhow::anyhow!("Provide <NAMESPACE> or --all"))?;
                            let (fixed_shape, fixed_dangling) =
                                direct.repair_namespace_hygiene(&ns).await?;
                            println!("Namespace hygiene repaired: {} shape fixes, {} dangling fixes.", fixed_shape, fixed_dangling);
                        }
                    }
                    Commands::Delete { namespace, id } => {
                        vector_store.delete(&namespace, &id).await?;
                        println!("Memory deleted successfully.");
                    }
                    Commands::Archive { namespace, id } => {
                        let embedder = build_embedder()?;
                        prepare_fallback_write_store(&vector_store, &embedder, &namespace)
                            .await?;
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
                        let embedder = build_embedder()?;
                        prepare_fallback_write_store(&vector_store, &embedder, &namespace)
                            .await?;
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
                        let embedder = build_embedder()?;
                        prepare_fallback_write_store(&vector_store, &embedder, &namespace)
                            .await?;
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
                        let embedder = build_embedder()?;
                        prepare_fallback_write_store(&vector_store, &embedder, &namespace)
                            .await?;
                        let new_metadata: serde_json::Value = serde_json::from_str(&metadata)
                            .map_err(|e| anyhow::anyhow!("Invalid JSON metadata: {}", e))?;
                        match crate::server::set_metadata(
                            &*vector_store,
                            &namespace,
                            &id,
                            new_metadata,
                            "admin",
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
                    Commands::Daemon { .. }
                    | Commands::Shutdown
                    | Commands::Run { .. }
                    | Commands::SynapticMesh { .. }
                    | Commands::Backup { .. }
                    | Commands::Restore { .. }
                    | Commands::Task { .. }
                    | Commands::Setup { .. }
                    | Commands::ProjectContext { .. }
                    | Commands::BusMetrics
                    | Commands::Kge { .. }
                    | Commands::DocGate { .. }
                    | Commands::Workflow { .. }
                    | Commands::DataConnector { .. }
                    | Commands::Hooks { .. }
                    | Commands::Bench { .. }
                    // THE OUTER match already handles Learn (src/main.rs ~2822);
                    // this inner catch-all can never see it.
                    | Commands::Learn { .. } => unreachable!(),
                }
                if vector_store.is_dirty() {
                    if let Err(e) = vector_store.checkpoint().await {
                        tracing::warn!("final CLI checkpoint failed: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}
