//! The adapter that makes [`MeshStorage`] satisfy the [`StorageApi`] seam.
//!
//! Shape mismatch this file exists to absorb (checker-confirmed, slice 2a/2b
//! review): `MeshStorage` is built from associated functions returning
//! `anyhow::Result` while [`StorageApi`] is object-based, `&mut self`,
//! `String`-typed, and `String`-idiomed for mesh ids. Every `Err` here is
//! formatted with `format!("{:#}", e)` — anyhow's `to_string` drops the
//! source chain and would turn "manifest could not be read" into a message
//! without the OS error behind it.
//!
//! The ambient-resolver rule from the honest-schema review lives here too:
//! `project_root` and `node_id` arrive as arguments at the MCP call site and
//! as computed defaults at the CLI call site; nothing in the dispatch path
//! sniffs the environment by surprise.

use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use super::cli::{handle_synaptic_mesh_command, MeshContext, SynapticMeshCommands};
use super::conflict;
use super::import::load_cursor;
use super::outbox::{outbox_dir_for, OUTBOX_FILE_NAME};
use super::storage::{MeshStorage, MANIFEST_FILE_NAME};
use super::storage_api::StorageApi;

/// The conventional mesh root: `~/.NeuroStrata/synaptic-mesh`
/// (spec §"Storage layout"). Computed at call sites — deliberately NOT a
/// method on `MeshContext`: an ambient-reading constructor on the context
/// struct invited handlers to reach for it (slice-2b checker landmine).
pub fn conventional_base_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".NeuroStrata")
        .join("synaptic-mesh")
}

/// Stable per-node identity: a UUID created on first run and persisted at
/// `<home>/.NeuroStrata/mesh-node-id` (spec §"Trust model": node_id ↔ git
/// author). Parameterized by home so tests never touch the real HOME.
pub fn resolve_node_id_at(home: &Path) -> String {
    let path = home.join(".NeuroStrata").join("mesh-node-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let fresh = Uuid::new_v4().to_string();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("{}\n", fresh));
    fresh
}

/// The call-site wrapper: resolves the local home once, then delegates.
pub fn resolve_node_id() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    resolve_node_id_at(&home)
}

/// Does this checkout have a commit yet? A fresh `init` has no HEAD until
/// the first record lands, and `git push HEAD` on an unborn branch dies with
/// a refspec stack trace that answers nothing. Cheap probe, empty-state
/// courtesy only.
fn head_exists(root: &Path) -> bool {
    std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "rev-parse",
            "--verify",
            "-q",
            "HEAD",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Does the configured remote have refs yet? Three answers, not two:
/// `Some(false)` = reachable but EMPTY (bare repo nobody pushed to — a
/// normal "team inbox not yet filled" state); `Some(true)` = has refs;
/// `None` = could not ask (unreachable/auth) — and an unreachable remote
/// must fall through to the real fetch so the failure SURFACES, per the
/// spec's fail-soft rule ("an unreachable remote reports the failure").
fn remote_refs_present(root: &Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "ls-remote",
            "--refs",
            "origin",
            "HEAD",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.is_empty())
}

/// The mesh session for one project. `current` caches the opened repo across
/// calls within one process; a fresh process re-resolves through the project
/// pointer, which is what makes the MCP server (stateless per call) and a
/// one-shot CLI invocation behave the same.
pub struct MeshService {
    project_root: PathBuf,
    current: Option<MeshStorage>,
}

impl MeshService {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            current: None,
        }
    }

    /// The project-side pointer path (same layout `MeshStorage` writes).
    fn pointer(&self) -> PathBuf {
        self.project_root
            .join(".NeuroStrata")
            .join("synaptic-mesh-ref")
    }

    /// Resolve the joined mesh, opening it through the pointer when this
    /// process has not opened it yet. ABSENT pointer answers with the
    /// canonical no-mesh prefix (so `cli::render` collapses it); a CORRUPT
    /// pointer propagates its damage untouched — silence about a broken
    /// pointer is how the no-mesh path hides real failures (slice-2b
    /// checker finding).
    fn joined(&mut self) -> Result<&mut MeshStorage, String> {
        if self.current.is_none() {
            // Pointer access goes through the trait seam, not the concrete
            // static: the surface must have production users on both sides
            // of the dyn boundary or the seam is decorative.
            let project = self.project_root.clone();
            let pointer = StorageApi::read_pointer(self, &project)?;
            let Some((mesh_id, dir, _remote)) = pointer else {
                return Err(format!(
                    "no mesh joined — no pointer at {}",
                    self.pointer().display()
                ));
            };
            let storage = MeshStorage::open(&dir).map_err(|e| format!("{:#}", e))?;
            if storage.manifest().mesh_id.to_string() != mesh_id {
                return Err(format!(
                    "no mesh joined — pointer names mesh {} but {} holds {}",
                    mesh_id,
                    dir.display(),
                    storage.manifest().mesh_id
                ));
            }
            self.current = Some(storage);
        }
        Ok(self.current.as_mut().expect("just resolved"))
    }

    /// The one status report, shared by the trait method (which cannot take
    /// `&mut self` to populate the cache) and by nothing else.
    ///
    /// Slice 7 (spec §"Recovery"/§"Status surfaces"): the report expands from
    /// one line to the full surface — heads read from the import-cursor file
    /// and the mesh repo's git log, the outbox WAL re-read for pending counts,
    /// and health derived from the recovery probes. Where a surface still
    /// needs a subsystem that has not landed, the line says so instead of
    /// inventing a number.
    fn status_report(&self) -> Result<String, String> {
        let pointer = StorageApi::read_pointer(self, &self.project_root)?;
        let Some((mesh_id, dir, _remote)) = pointer else {
            return Err(format!(
                "no mesh joined — no pointer at {}",
                self.pointer().display()
            ));
        };
        let storage = MeshStorage::open(&dir).map_err(|e| format!("{:#}", e))?;
        let m = storage.manifest();
        if m.mesh_id.to_string() != mesh_id {
            return Err(format!(
                "no mesh joined — pointer names mesh {} but {} holds {}",
                mesh_id,
                dir.display(),
                m.mesh_id
            ));
        }
        let root = storage.root().to_path_buf();

        // Outbox pending state straight from the WAL (slice-7 rule: no
        // worker.rs APIs here — the file IS the truth, and outbox.rs only
        // names its location).
        let wal = wal_summary(&root);

        let mut lines = Vec::new();
        lines.push(format!(
            "mesh {} \"{}\" epoch {} schema {} remote {} root {}",
            m.mesh_id,
            m.mesh_name,
            m.epoch,
            m.schema_version,
            m.remote.as_deref().unwrap_or("<none configured>"),
            root.display()
        ));

        // Joined namespaces with their sync state = per-namespace pending
        // count from the WAL. Namespaces the WAL knows but the manifest no
        // longer lists are still reported — dropped-but-queued entries must
        // not vanish from the report just because the manifest moved on.
        let mut ns_parts: Vec<String> = m
            .namespaces
            .iter()
            .map(|ns| {
                format!(
                    "{} (pending {})",
                    ns,
                    wal.per_ns.get(ns).copied().unwrap_or(0)
                )
            })
            .collect();
        for (ns, count) in &wal.per_ns {
            if !m.namespaces.contains(ns) {
                ns_parts.push(format!("{ns} (removed from manifest, pending {count})"));
            }
        }
        lines.push(format!("namespaces: {}", ns_parts.join(", ")));

        match (wal.pending, wal.oldest_ts) {
            (0, _) => lines.push("outbox: 0 pending".to_string()),
            (n, oldest) => lines.push(format!(
                "outbox: {n} pending, oldest {}",
                age_of(
                    oldest
                        .map(|ts| (now_secs() - ts).max(0) as u64)
                        .unwrap_or(0)
                )
            )),
        }
        if wal.corrupt > 0 {
            lines.push(format!(
                "outbox: {} unreadable WAL line(s) — diagnostics preserved, never deleted",
                wal.corrupt
            ));
        }

        // Last accepted head: the import cursor file (daemon state, sibling
        // of the checkout), plus its age.
        let short = |h: &str| h.get(..7).unwrap_or(h).to_string();
        match accepted_head_state(&root, &m.mesh_id.to_string()) {
            Some((head, age)) => lines.push(format!(
                "last accepted head: {} (age {})",
                short(&head),
                age
            )),
            None => lines.push("last accepted head: none".to_string()),
        }

        // Last published head: the checkout's own git log, compared against
        // the remote-tracking ref when a remote is configured.
        lines.push(published_head_line(&root, m.remote.as_deref()));

        // The conflict overlay's truth (slice 6, wired at integration): an
        // empty status_lines means nothing is registered — render an
        // explicit zero, because a silently absent line can be misread as
        // "no conflicts" by whoever never learned to look for it.
        let conflict_lines = conflict::status_lines(&root);
        if conflict_lines.is_empty() {
            lines.push("conflicts: 0 unresolved".to_string());
        } else {
            lines.extend(conflict_lines);
        }

        // Local store counts (shared vs private, spec §"Status surfaces")
        // need the VectorStore; the CLI dispatcher here has no database (the
        // daemon may even hold its lock), so the report says so rather than
        // lies. The disclosure sentence stays on every render (spec:457).
        lines.push(
            "local store counts (shared/private): store-side seam — the CLI serves mesh state only; the daemon can report store counts"
                .to_string(),
        );
        lines.push(format!("health: {}", health_line(&root)));
        lines.push(
            "DISCLOSURE: the mesh is a permanent record shared with the team — tombstones suppress content from search, but full erasure requires a coordinated history rewrite, and an uncooperative peer's clone retains the content."
                .to_string(),
        );
        Ok(lines.join("\n"))
    }
}

// --- slice 7 shared probes (status + recovery use the same truth) ----------

/// Non-interactive git probe answering `None` when git could not be asked at
/// all (the caller decides what omission means — usually a quiet skip).
fn git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()
}

/// The checkout's dirty porcelain paths, mirroring import.rs's probe. The
/// untracked top-level `manifest.json` written once at `init` is EXEMPT:
/// every fresh mesh carries it until the mesh-worker's first commit, and
/// showing "paused" on pristine init state would make every brand new mesh
/// read broken. An EDITED manifest shows as `M`, not `??`, and counts.
pub fn dirty_paths(mesh_root: &Path) -> Vec<String> {
    let Some(out) = git(mesh_root, &["status", "--porcelain"]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| {
            !(l.starts_with("?? ") && l.trim_start_matches("?? ").trim() == MANIFEST_FILE_NAME)
        })
        .map(|l| l.trim().to_string())
        .collect()
}

/// Detached HEAD: `git rev-parse --abbrev-ref HEAD` answers `HEAD` (unborn
/// branches answer the branch name or fail — neither is detached).
pub fn detached_head(mesh_root: &Path) -> bool {
    git(mesh_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "HEAD")
        .unwrap_or(false)
}

/// Interrupted merge: git leaves `MERGE_HEAD` open until abort or commit.
pub fn merge_in_progress(mesh_root: &Path) -> bool {
    mesh_root.join(".git").join("MERGE_HEAD").exists()
}

/// The health verdict: `healthy` or one `paused: <reason>` — dirty first
/// (it blocks replay AND publish), then detached, then interrupted merge.
fn health_line(mesh_root: &Path) -> String {
    let dirty = dirty_paths(mesh_root);
    if !dirty.is_empty() {
        return format!("paused: dirty-checkout ({})", dirty.join("; "));
    }
    if detached_head(mesh_root) {
        return "paused: detached HEAD — run `mesh recover` for the reattach steps".to_string();
    }
    if merge_in_progress(mesh_root) {
        return "paused: merge in progress — `mesh recover` names the finish/abort choice"
            .to_string();
    }
    "healthy".to_string()
}

/// `(head, age)` from the import cursor file, when it is intact and belongs
/// to this mesh (import.rs owns the validation rules; the report only reads).
fn accepted_head_state(mesh_root: &Path, mesh_id: &str) -> Option<(String, String)> {
    let head = load_cursor(mesh_root, mesh_id)?;
    let file = super::import::cursor_file(mesh_root);
    let age = std::fs::metadata(&file)
        .and_then(|meta| meta.modified())
        .ok()
        .map(|t| age_of(t.elapsed().ok().map(|d| d.as_secs()).unwrap_or(0)))
        .unwrap_or_else(|| "unknown".to_string());
    Some((head, age))
}

/// The published-head line for the checkout: local HEAD (or `none` while the
/// branch is unborn) contrasted against the remote-tracking ref when a
/// remote is configured. Read-only on purpose: no fetch here, so "in sync"
/// means "what we last fetched/pushed equals HEAD", and a never-fetched
/// mesh says so.
fn published_head_line(mesh_root: &Path, remote: Option<&str>) -> String {
    let local = git(mesh_root, &["log", "-1", "--format=%H %ct", "HEAD"])
        .filter(|o| o.status.success())
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let mut parts = text.split_whitespace();
            let head = parts.next()?.to_string();
            let ts = parts.next()?.parse::<i64>().ok()?;
            Some((head, ts))
        });
    let Some((head, ts)) = local else {
        return "last published head: none (unborn branch — nothing committed yet)".to_string();
    };
    let age = age_of((now_secs() - ts).max(0) as u64);
    let short = head.get(..7).unwrap_or(&head).to_string();
    if remote.is_none() {
        return format!("last published head: {short} (age {age}) — no remote configured");
    }
    let tracking = git(
        mesh_root,
        &[
            "for-each-ref",
            "refs/remotes/origin",
            "--count=1",
            "--sort=-committerdate",
            "--format=%(objectname)",
        ],
    )
    .filter(|o| o.status.success())
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    .filter(|s| !s.is_empty());
    match tracking {
        Some(t) if t == head => {
            format!("last published head: {short} (age {age}) — remote-tracking in sync")
        }
        Some(t) => format!(
            "last published head: {short} (age {age}) — remote-tracking {} differs (fetch to reconcile, never force-push)",
            t.get(..7).unwrap_or(&t)
        ),
        None => format!(
            "last published head: {short} (age {age}) — remote-tracking unknown (never fetched)"
        ),
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Compact human age — statuses scroll, so a string beats a bare number.
fn age_of(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// What one read of the outbox WAL says. The WAL's pending set is its whole
/// parsed content (outbox.rs compacts committed entries away), so counting
/// lines IS counting pending entries; a fragment (no trailing newline) and
/// corrupt lines are counted separately and never deleted.
struct WalSummary {
    pending: usize,
    per_ns: std::collections::BTreeMap<String, usize>,
    oldest_ts: Option<i64>,
    corrupt: usize,
}

fn wal_summary(mesh_dir: &Path) -> WalSummary {
    let mut summary = WalSummary {
        pending: 0,
        per_ns: Default::default(),
        oldest_ts: None,
        corrupt: 0,
    };
    let bytes = std::fs::read(outbox_dir_for(mesh_dir).join(OUTBOX_FILE_NAME));
    let Ok(bytes) = bytes else {
        return summary; // no WAL yet = nothing pending
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut segments: Vec<&str> = text.split('\n').collect();
    // Mirror outbox.rs: the tail without a newline is a crash fragment,
    // dropped rather than read as a complete entry.
    segments.pop();
    for line in segments {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(entry) => {
                summary.pending += 1;
                if let Some(ns) = entry["namespace"].as_str() {
                    *summary.per_ns.entry(ns.to_string()).or_insert(0) += 1;
                }
                if let Some(ts) = entry["ts"].as_i64() {
                    summary.oldest_ts = Some(summary.oldest_ts.map_or(ts, |old| old.min(ts)));
                }
            }
            Err(_) => summary.corrupt += 1,
        }
    }
    summary
}

// --- slice 7: hook piggyback + recovery surfaces ---------------------------

/// `neurostrata-mcp mesh sync-enqueue` — pull-then-push for the project
/// pre-push hook (spec §"Hooks"). Always renders ONE line and never an
/// `Err`: the exit-0 invariant is enforced by the caller, but this function
/// owns the shape a hook consumes. "No mesh joined" is a *quiet* nothing-
/// to-do for hook purposes (spec §"Hooks": the mesh command itself never
/// depends on a project-side hook), not an error.
pub fn sync_enqueue_line(store: &mut dyn StorageApi) -> String {
    let pull = match StorageApi::pull(store) {
        Err(e) if e.starts_with("no mesh joined") => {
            return "mesh sync-enqueue: no mesh joined — nothing to do".to_string()
        }
        Err(e) => return soft(e),
        Ok(line) => line,
    };
    match StorageApi::push(store) {
        Ok(line) => format!("mesh sync-enqueued: {pull}; {line}"),
        Err(e) => soft(e),
    }
}

/// The hook-safe failure rendering: a rejected/unreachable remote queues
/// locally and the hook still exits 0 (spec §"Hooks": never block the
/// source push). Git stderr can be multiline; a hook line cannot be — it is
/// flattened so the guarantee "one line" holds even on failure.
fn soft(err: String) -> String {
    let flat = err.replace(['\n', '\r'], "; ");
    format!("mesh sync-enqueue failed (non-blocking; work stays queued locally): {flat}")
}

/// `neurostrata-mcp mesh recover` — actionable recovery steps for the mesh
/// checkout, per spec §"Recovery". ALWAYS print-only: it never fixes
/// anything itself, and every hint is phrased as a manual action ("never
/// automatic `reset --hard`" is a spec mandate, so the wording says never).
pub fn recover_report(project_root: &Path) -> String {
    let pointer = match MeshStorage::read_pointer(project_root) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "mesh recover: the project pointer is unreadable ({e:#}). Fix or delete it manually, then re-join (`mesh join <dir>`)."
            )
        }
    };
    let Some((mesh_id, dir, _remote)) = pointer else {
        return "no mesh joined — run `neurostrata-mcp synaptic-mesh init <name>` first"
            .to_string();
    };
    let storage = match MeshStorage::open(&dir) {
        Ok(s) => s,
        Err(e) => {
            return format!(
                "mesh recover: the manifest at {} is unreadable ({e:#}). Corrupt-objects recovery (spec §Recovery): back up the WAL at {} and any unpublished commits, then reclone — NEVER `git reset --hard`, never auto-fix.",
                dir.display(),
                outbox_dir_for(&dir).join(OUTBOX_FILE_NAME).display()
            )
        }
    };
    let root = storage.root().to_path_buf();
    let mut out = vec![format!(
        "mesh recover for mesh {} at {} — facts plus the manual fix each state needs (nothing is changed for you):",
        mesh_id,
        root.display()
    )];

    let dirty = dirty_paths(&root);
    if !dirty.is_empty() {
        out.push(format!(
            "dirty working tree → pause sync, local store untouched. Fix: commit or stash INSIDE the mesh checkout ({}) — never `git reset --hard`. Dirty paths: {}",
            root.display(),
            dirty.join("; ")
        ));
    }
    if detached_head(&root) {
        let branches = git(
            &root,
            &["for-each-ref", "refs/heads", "--format=%(refname:short)"],
        )
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(5)
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        let hint = if branches.is_empty() {
            "reclone as the last resort (back up the WAL first)".to_string()
        } else {
            format!(
                "reattach manually: `git -C {} checkout <branch>` (local branches: {})",
                root.display(),
                branches.join(", ")
            )
        };
        out.push(format!(
            "detached HEAD → pause sync. Fix: {hint} — never `git reset --hard`; nothing here reattaches it for you."
        ));
    }
    if merge_in_progress(&root) {
        out.push(format!(
            "interrupted merge → pause sync. Fix: finish it by committing, or `git -C {} merge --abort` and pull again.",
            root.display()
        ));
    }
    if out.len() == 1 {
        out.push(
            "healthy: nothing to recover. If sync still fails, `mesh status` names the health reason (auth failures queue locally by design)."
                .to_string(),
        );
    }
    out.join("\n")
}

impl StorageApi for MeshService {
    fn init_mesh(
        &mut self,
        base_root: &Path,
        node_id: &str,
        mesh_name: &str,
        namespaces: &[String],
        remote: Option<&str>,
    ) -> Result<String, String> {
        let storage = MeshStorage::init(base_root, node_id, mesh_name, namespaces, remote)
            .map_err(|e| format!("{:#}", e))?;
        let mesh_id = storage.manifest().mesh_id.to_string();
        let root = storage.root().to_path_buf();
        let project = self.project_root.clone();
        StorageApi::write_pointer(self, &project, &mesh_id, &root, remote)?;
        self.current = Some(storage);
        Ok(mesh_id)
    }

    fn open_mesh(&mut self, mesh_dir: &Path) -> Result<(), String> {
        let storage = MeshStorage::open(mesh_dir).map_err(|e| format!("{:#}", e))?;
        let (mesh_id, remote) = {
            let m = storage.manifest();
            (m.mesh_id.to_string(), m.remote.clone())
        };
        let project = self.project_root.clone();
        StorageApi::write_pointer(self, &project, &mesh_id, mesh_dir, remote.as_deref())?;
        self.current = Some(storage);
        Ok(())
    }

    fn write_pointer(
        &mut self,
        project: &Path,
        mesh_id: &str,
        mesh_dir: &Path,
        remote: Option<&str>,
    ) -> Result<(), String> {
        let parsed = Uuid::parse_str(mesh_id)
            .map_err(|e| format!("mesh id is not a uuid ({mesh_id}): {e}"))?;
        MeshStorage::write_pointer(project, parsed, mesh_dir, remote)
            .map_err(|e| format!("{:#}", e))
    }

    fn read_pointer(
        &self,
        project: &Path,
    ) -> Result<Option<(String, PathBuf, Option<String>)>, String> {
        MeshStorage::read_pointer(project)
            .map(|found| found.map(|(id, dir, remote)| (id.to_string(), dir, remote)))
            .map_err(|e| format!("{:#}", e))
    }

    fn remove_pointer(&mut self, project: &Path) -> Result<(), String> {
        MeshStorage::remove_pointer(project).map_err(|e| format!("{:#}", e))?;
        if project == self.project_root {
            self.current = None;
        }
        Ok(())
    }

    fn status(&self) -> Result<String, String> {
        self.status_report()
    }

    fn push(&mut self) -> Result<String, String> {
        let storage = self.joined()?;
        // Ordering matters: a mesh with no remote is a CONFIG error and must
        // surface as one even before its first commit. Only a configured
        // mesh gets the "nothing to publish yet" courtesy (a fresh init has
        // no commits until the mesh-worker lands a record — slice 3).
        let remote = storage.manifest().remote.clone();
        if remote.is_none() {
            return storage
                .push()
                .map(|()| "pushed".to_string())
                .map_err(|e| format!("{:#}", e));
        }
        if !head_exists(storage.root()) {
            return Ok(format!(
                "nothing to publish yet — mesh {} has no local commits",
                storage.manifest().mesh_id
            ));
        }
        storage.push().map_err(|e| format!("{:#}", e))?;
        Ok(format!(
            "pushed mesh {} to {}",
            storage.manifest().mesh_id,
            remote.unwrap_or_default()
        ))
    }

    fn pull(&mut self) -> Result<String, String> {
        let storage = self.joined()?;
        let mesh_id = storage.manifest().mesh_id;
        if storage.manifest().remote.is_some()
            && matches!(remote_refs_present(storage.root()), Some(false))
        {
            return Ok(format!(
                "pulled mesh {}: nothing on the remote yet — records land with the mesh-worker (slice 3)",
                mesh_id
            ));
        }
        storage.pull().map_err(|e| format!("{:#}", e))?;
        Ok(format!(
            "pulled mesh {} (fast-forward only; a divergence reports and preserves local work)",
            mesh_id
        ))
    }
}

/// Dispatch one MCP `tools/call` for the synaptic-mesh surface, production
/// base root (the conventional `~/.NeuroStrata/synaptic-mesh`).
pub fn handle_mesh_tool_call(name: &str, arguments: &Value) -> String {
    handle_mesh_tool_call_with_base(name, arguments, &conventional_base_root())
}

/// Resolve the project root a mesh tool call acts on (Task 2 / J6): the
/// caller's `project_root` is canonicalized, and the canonical path is the
/// ONLY value the service may use — never the verbatim string, whose `..`
/// spellings and symlink aliases would each name a different anchor for
/// the same project. A root that cannot be canonicalized does not exist on
/// this machine and is refused as foreign. No `project_root` at all
/// resolves the way the daemon resolves its own root (environment, then
/// working directory), so the fallback and the daemon can never disagree
/// about which project a rootless call means.
pub fn resolve_mesh_project_root(arguments: &Value) -> Result<PathBuf, String> {
    match arguments.get("project_root").and_then(|v| v.as_str()) {
        Some(raw) => std::fs::canonicalize(raw).map_err(|e| {
            format!(
                "error: project_root {raw} cannot be resolved ({e}); the mesh \
                 tools act on the canonical project root, so pass a project \
                 root that exists on this machine"
            )
        }),
        None => {
            let resolved = crate::daemon::resolve_project_root();
            Ok(std::fs::canonicalize(&resolved).unwrap_or(resolved))
        }
    }
}

/// Test-seam variant: same dispatch, explicit base root. Tests pass a scratch
/// dir so a mesh never lands under the developer's real home.
pub fn handle_mesh_tool_call_with_base(name: &str, arguments: &Value, base_root: &Path) -> String {
    let project_root = match resolve_mesh_project_root(arguments) {
        Ok(root) => root,
        Err(e) => return e,
    };
    let node_id = arguments
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(resolve_node_id);
    let ctx = MeshContext {
        project_root: project_root.clone(),
        base_root: base_root.to_path_buf(),
        node_id,
    };
    let mut svc = MeshService::new(project_root);
    let command = match name {
        "neurostrata_synaptic_mesh_init" => match arguments.get("name").and_then(|v| v.as_str()) {
            Some(mesh_name) => SynapticMeshCommands::Init {
                name: mesh_name.to_string(),
                namespace: arguments
                    .get("namespace")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                remote: arguments
                    .get("remote")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            },
            None => return "error: neurostrata_synaptic_mesh_init requires `name`".to_string(),
        },
        "neurostrata_synaptic_mesh_join" => match arguments.get("dir").and_then(|v| v.as_str()) {
            Some(dir) => SynapticMeshCommands::Join {
                dir: PathBuf::from(dir),
            },
            None => return "error: neurostrata_synaptic_mesh_join requires `dir`".to_string(),
        },
        "neurostrata_synaptic_mesh_push" => SynapticMeshCommands::Push,
        "neurostrata_synaptic_mesh_pull" => SynapticMeshCommands::Pull,
        "neurostrata_synaptic_mesh_status" => SynapticMeshCommands::Status,
        "neurostrata_synaptic_mesh_leave" => SynapticMeshCommands::Leave,
        other => return format!("error: unknown synaptic-mesh tool: {other}"),
    };
    handle_synaptic_mesh_command(&command, &ctx, &mut svc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ns-mesh-svc-{}-{}", label, Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn service_init_writes_pointer_and_status_reports_it() {
        let base = scratch("base");
        let project = scratch("proj");
        let mut svc = MeshService::new(project.clone());
        let id = svc
            .init_mesh(
                &base,
                "node-aaa",
                "platform-team",
                &["proj-ns".to_string()],
                None,
            )
            .expect("init");
        assert!(Uuid::parse_str(&id).is_ok(), "id is uuid, got {id}");
        assert!(project
            .join(".NeuroStrata")
            .join("synaptic-mesh-ref")
            .exists());
        let report = svc.status().expect("status after init");
        assert!(report.contains("platform-team"), "report: {report}");
        assert!(report.contains(&id), "report: {report}");
    }

    #[test]
    fn fresh_service_finds_mesh_through_pointer() {
        let base = scratch("base2");
        let project = scratch("proj2");
        let mut first = MeshService::new(project.clone());
        let id = first
            .init_mesh(&base, "node-b", "shared", &["ns".into()], None)
            .unwrap();
        drop(first);
        // New-process simulation: a fresh service with no cached handle.
        let mut second = MeshService::new(project);
        let report = second.status().expect("lazy resolve via pointer");
        assert!(report.contains(&id));
        // push without a remote must error honestly, not panic
        let err = second.push().expect_err("no remote configured");
        assert!(err.contains("no remote"), "err: {err}");
    }

    #[test]
    fn no_mesh_paths_collapse_to_the_canonical_reply() {
        let project = scratch("lonely");
        let mut svc = MeshService::new(project);
        let ctx = MeshContext {
            project_root: scratch("ctx-proj"),
            base_root: scratch("ctx-base"),
            node_id: "n".into(),
        };
        for cmd in [
            SynapticMeshCommands::Push,
            SynapticMeshCommands::Pull,
            SynapticMeshCommands::Status,
        ] {
            let reply = handle_synaptic_mesh_command(&cmd, &ctx, &mut svc);
            assert!(
                reply.starts_with("no mesh joined"),
                "{cmd:?} replied {reply:?} — render() must collapse the no-mesh prefix"
            );
        }
    }

    #[test]
    fn join_adopts_and_writes_the_pointer_in_one_call() {
        let base = scratch("jbase");
        let project_a = scratch("jproj-a");
        let project_b = scratch("jproj-b");
        let mut a = MeshService::new(project_a);
        let id = a
            .init_mesh(&base, "node-j", "adopted", &["ns".into()], None)
            .unwrap();
        let mesh_dir = base.join(&id);
        let mut b = MeshService::new(project_b.clone());
        b.open_mesh(&mesh_dir).expect("join");
        let read = b
            .read_pointer(&project_b)
            .unwrap()
            .expect("pointer written");
        assert_eq!(read.0, id);
        assert_eq!(read.1, mesh_dir);
        assert_eq!(read.2, None);
    }

    #[test]
    fn malformed_pointer_surfaces_the_source_chain() {
        let project = scratch("garbage");
        std::fs::create_dir_all(project.join(".NeuroStrata")).unwrap();
        std::fs::write(
            project.join(".NeuroStrata").join("synaptic-mesh-ref"),
            "junk-line",
        )
        .unwrap();
        let svc = MeshService::new(project);
        let err = svc
            .status()
            .expect_err("garbage pointer must not read as joined");
        assert!(
            err.contains("malformed"),
            "expected storage's malformed detail in the chain, got: {err}"
        );
        assert!(
            !err.starts_with("no mesh joined"),
            "a corrupt pointer is NOT 'no mesh joined' — it must render as an error, got: {err}"
        );
    }

    #[test]
    fn node_id_is_persistent_and_stable() {
        let home = scratch("home");
        let first = resolve_node_id_at(&home);
        let second = resolve_node_id_at(&home);
        assert_eq!(first, second, "stable across calls");
        assert!(Uuid::parse_str(&first).is_ok(), "it is a uuid");
        assert!(home.join(".NeuroStrata").join("mesh-node-id").exists());
    }

    #[test]
    fn tool_call_dispatch_round_trip_explicit_project_root() {
        let base = scratch("tbase");
        let project = scratch("tproj");
        let out = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_init",
            &json!({"name": "tool-mesh", "namespace": ["t"], "project_root": project, "node_id": "nt"}),
            &base,
        );
        assert!(out.contains("mesh created"), "init reply: {out}");
        let status = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_status",
            &json!({"project_root": project}),
            &base,
        );
        assert!(status.contains("tool-mesh"), "status reply: {status}");
        assert!(
            status.contains("permanent record"),
            "status must carry the spec:457 disclosure: {status}"
        );
        let leave = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_leave",
            &json!({"project_root": project}),
            &base,
        );
        assert!(leave.contains("left"), "leave reply: {leave}");
        assert!(!project
            .join(".NeuroStrata")
            .join("synaptic-mesh-ref")
            .exists());
    }

    #[test]
    fn tool_call_unknown_name_errors_loudly() {
        let out = handle_mesh_tool_call("neurostrata_synaptic_mesh_bogus", &json!({}));
        assert!(out.starts_with("error:"), "reply: {out}");
        assert!(out.contains("unknown"));
    }

    /// Task 2 (J6): the service acts on the CANONICAL project root, never
    /// the caller's verbatim spelling — a path spelled through a
    /// subdirectory and back (`sub/../`) names the same project and gets
    /// the same answer as the canonical spelling.
    #[test]
    fn mesh_project_root_spelling_resolves_to_the_canonical_project() {
        let base = scratch("canon-base");
        let project = scratch("canon-proj");
        let init = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_init",
            &json!({"name": "canon-mesh", "namespace": ["c"], "project_root": project, "node_id": "cn"}),
            &base,
        );
        assert!(init.contains("mesh created"), "init reply: {init}");
        let sub = project.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let spelled = format!("{}/../", sub.display());
        let status = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_status",
            &json!({"project_root": spelled}),
            &base,
        );
        assert!(
            status.contains("canon-mesh"),
            "the canonical project answers, whatever the spelling: {status}"
        );
    }

    #[test]
    fn tool_call_push_pull_answer_the_fresh_mesh_honestly() {
        // init + remote configured, but no record committed yet (the
        // mesh-worker lands records in slice 3). git dies with refspec
        // stack traces on this state; the adapter must answer with the
        // empty-state truth instead.
        let remote_dir = scratch("remote");
        let bare = remote_dir.join("mesh.git");
        let out = std::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&bare)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git init --bare");
        assert!(out.status.success());
        let base = scratch("dbase");
        let project = scratch("dproj");
        let init = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_init",
            &json!({"name":"div", "namespace":["d"], "remote": bare.display().to_string(), "project_root": project, "node_id":"dn"}),
            &base,
        );
        assert!(init.contains("mesh created"), "init reply: {init}");
        let push = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_push",
            &json!({"project_root": project}),
            &base,
        );
        assert!(
            push.contains("nothing to publish yet"),
            "fresh mesh push must answer the empty state, got: {push}"
        );
        let pull = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_pull",
            &json!({"project_root": project}),
            &base,
        );
        assert!(
            pull.contains("nothing on the remote yet"),
            "empty-remote pull must answer the empty state, got: {pull}"
        );
    }

    // --- slice 7: full status surface --------------------------------------

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit_manifest(svc: &mut MeshService, author: &str) {
        let storage = svc.joined().expect("mesh joined");
        assert!(storage.commit_all("bootstrap", author).unwrap());
    }

    /// A mesh this node owns through a project pointer; returns the service
    /// and the checkout. `status_report` re-resolves through the pointer on
    /// every call (no cache), so post-init file writes show up in the SAME
    /// service's report.
    fn init_joined(base: &Path, project: &Path, namespaces: &[&str]) -> (MeshService, PathBuf) {
        let ns: Vec<String> = namespaces.iter().map(|s| s.to_string()).collect();
        let mut svc = MeshService::new(project.to_path_buf());
        let id = svc
            .init_mesh(base, "node-7", "status-team", &ns, None)
            .expect("init");
        (svc, base.join(&id))
    }

    #[test]
    fn status_on_a_fresh_mesh_reports_the_full_surface() {
        let base = scratch("st-fresh");
        let project = scratch("st-fresh-proj");
        let (svc, dir) = init_joined(&base, &project, &["alpha"]);
        let report = svc.status().expect("status on fresh mesh");

        assert!(report.contains("status-team"), "name: {report}");
        assert!(report.contains("epoch 1"), "epoch: {report}");
        assert!(
            report.contains("schema synaptic-mesh/v1"),
            "schema line: {report}"
        );
        assert!(report.contains("alpha (pending 0)"), "per-ns: {report}");
        assert!(report.contains("outbox: 0 pending"), "outbox: {report}");
        assert!(report.contains("last accepted head: none"), "{report}");
        assert!(
            report.contains("remote <none configured>"),
            "honest remote line: {report}"
        );
        assert!(
            report.contains("unborn branch — nothing committed yet"),
            "fresh checkout has no published head: {report}"
        );
        assert!(
            report.contains("conflicts: 0 unresolved"),
            "clean mesh states zero explicitly — silence is not an answer: {report}"
        );
        assert!(report.contains("health: healthy"), "fresh mesh: {report}");
        assert!(
            report.contains("DISCLOSURE: the mesh is a permanent record"),
            "spec:457 sentence stays: {report}"
        );
        assert!(
            std::fs::metadata(&dir).expect("mesh dir stat").is_dir(),
            "status is read-only sanity: mesh dir untouched"
        );
    }

    /// End-to-end pin for the wired seam: a conflict registered in the
    /// overlay must surface through `status()` — the count AND the per-entry
    /// detail. This is what a teammate reads when a namespace's integration
    /// is paused, so the line cannot lie in either direction: the fresh-mesh
    /// test pins the explicit zero, this test pins the non-zero.
    #[test]
    fn status_surfaces_registered_conflicts_through_the_seam() {
        let base = scratch("st-conf");
        let project = scratch("st-conf-proj");
        let (svc, dir) = init_joined(&base, &project, &["alpha"]);
        let id = Uuid::new_v4();
        let record: crate::synaptic_mesh::record::MeshRecord = serde_json::from_value(json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": id.to_string(),
            "namespace": "alpha",
            "memory_type": "rule",
            "lifecycle": {
                "state": "active",
                "created_at": "2026-10-11T00:00:00Z",
                "created_by": "node-peer-1",
                "superseded_by": null,
                "archived_at": null,
                "tombstoned_at": null
            },
            "content": {"text": "incoming replacement"},
            "metadata": {"eligibility": "shareable", "trust": "untrusted"},
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": "untrusted"
        }))
        .expect("record fixture");
        conflict::register_conflict(
            &dir,
            "alpha",
            &id.to_string(),
            "u-seam-1",
            "two different replacements",
            conflict::ConflictKind::Unresolved,
            &record,
        )
        .expect("register");
        let report = svc.status().expect("status with a conflict");
        assert!(
            report.contains("conflicts: 1 unresolved"),
            "seam must surface the count: {report}"
        );
        assert!(
            report.contains("two different replacements"),
            "seam must surface the reason: {report}"
        );
        assert!(
            !report.contains("conflicts: 0 unresolved"),
            "a registered conflict is never reported as zero: {report}"
        );
    }

    /// e2e pin for the PULL TRIGGER side of the conflict loop (slice 7):
    /// the seam test above pins the import side; this one drives the pull
    /// trigger path with the same registered conflict and pins that the
    /// round trip through pull leaves the overlay's truth intact — status
    /// still counts the conflict, the import cursor is never touched, and
    /// nothing panics on the way.
    #[test]
    fn pull_trigger_with_a_live_conflict_keeps_cursor_and_conflict_report() {
        let remote_dir = scratch("pt-remote");
        let bare = remote_dir.join("trigger-mesh.git");
        let out = std::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&bare)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git init --bare");
        assert!(out.status.success());
        let base = scratch("pt-base");
        let project = scratch("pt-proj");
        let init = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_init",
            &json!({
                "name": "trigger-team",
                "namespace": ["alpha"],
                "remote": bare.display().to_string(),
                "project_root": project,
                "node_id": "pt-node"
            }),
            &base,
        );
        assert!(init.contains("mesh created"), "init reply: {init}");
        let svc = MeshService::new(project.clone());
        let (mesh_id, dir, _) = svc
            .read_pointer(&project)
            .expect("pointer readable")
            .expect("pointer written");

        // The accepted-head cursor has daemon state BEFORE the trigger.
        let seeded = "0123456789abcdef0123456789abcdef01234567".to_string();
        crate::synaptic_mesh::import::write_cursor(&dir, &mesh_id, &seeded).expect("seed cursor");
        assert_eq!(
            load_cursor(&dir, &mesh_id),
            Some(seeded.clone()),
            "cursor seeded"
        );

        // The conflict fixture is the seam fixture, verbatim pattern.
        let id = Uuid::new_v4();
        let record: crate::synaptic_mesh::record::MeshRecord = serde_json::from_value(json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": id.to_string(),
            "namespace": "alpha",
            "memory_type": "rule",
            "lifecycle": {
                "state": "active",
                "created_at": "2026-10-11T00:00:00Z",
                "created_by": "node-peer-1",
                "superseded_by": null,
                "archived_at": null,
                "tombstoned_at": null
            },
            "content": {"text": "incoming replacement"},
            "metadata": {"eligibility": "shareable", "trust": "untrusted"},
            "relationships": [],
            "update_id": Uuid::new_v4().to_string(),
            "origin_mesh_epoch": 1,
            "trust": "untrusted"
        }))
        .expect("record fixture");
        conflict::register_conflict(
            &dir,
            "alpha",
            &id.to_string(),
            "u-trigger-1",
            "two different replacements",
            conflict::ConflictKind::Unresolved,
            &record,
        )
        .expect("register");

        // The trigger: the full tool-call path into `svc.pull` with the
        // conflicted mesh's pointer. The remote has no refs yet, so the
        // pull answers the empty state — but the trigger still runs.
        let pull = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_pull",
            &json!({"project_root": project}),
            &base,
        );
        assert!(
            pull.contains("nothing on the remote yet"),
            "empty-remote pull reply: {pull}"
        );

        // Cursor unmoved.
        assert_eq!(
            load_cursor(&dir, &mesh_id),
            Some(seeded.clone()),
            "the pull trigger never touches the accepted-head cursor"
        );

        // Status through the SAME tool surface still counts the conflict.
        let status = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_status",
            &json!({"project_root": project}),
            &base,
        );
        assert!(
            status.contains("conflicts: 1 unresolved"),
            "conflict survives the pull trigger: {status}"
        );
        assert!(
            status.contains("0123456"),
            "status still reads the seeded cursor head: {status}"
        );
    }

    #[test]
    fn status_counts_wal_pending_and_their_oldest_age_per_namespace() {
        let base = scratch("st-wal");
        let project = scratch("st-wal-proj");
        let (svc, dir) = init_joined(&base, &project, &["alpha", "beta"]);
        let wal = outbox_dir_for(&dir).join(OUTBOX_FILE_NAME);
        std::fs::create_dir_all(outbox_dir_for(&dir)).unwrap();
        let old_ts = chrono::Utc::now().timestamp() - 121;
        let new_ts = chrono::Utc::now().timestamp() - 10;
        // Written the way outbox.rs appends (every line newline-terminated):
        // the last line's \n matters — wal_summary drops the would-be tail
        // fragment otherwise, and the beta entry would vanish from the count.
        std::fs::write(
            &wal,
            format!(
                r#"{{"stable_op_id":"{}","op":"created","id":"m1","namespace":"alpha","memory_type":"rule","content":"c","metadata":{{}},"ts":{old_ts}}}
{{"stable_op_id":"{}","op":"created","id":"m2","namespace":"beta","memory_type":"fact","content":"c","metadata":{{}},"ts":{new_ts}}}
"#,
                Uuid::new_v4(),
                Uuid::new_v4()
            ),
        )
        .unwrap();

        let report = svc.status().expect("status");
        assert!(report.contains("outbox: 2 pending"), "{report}");
        assert!(report.contains("alpha (pending 1)"), "{report}");
        assert!(report.contains("beta (pending 1)"), "{report}");
        assert!(report.contains("oldest 2m"), "121s renders as 2m: {report}");
    }

    #[test]
    fn a_wal_fragment_without_newline_is_not_pending() {
        let base = scratch("st-frag");
        let project = scratch("st-frag-proj");
        let (svc, dir) = init_joined(&base, &project, &["alpha"]);
        let wal = outbox_dir_for(&dir).join(OUTBOX_FILE_NAME);
        std::fs::create_dir_all(outbox_dir_for(&dir)).unwrap();
        std::fs::write(
            &wal,
            format!(
                // Entry terminated with \n, exactly as outbox.rs appends it.
                r#"{{"stable_op_id":"{}","op":"created","id":"m1","namespace":"alpha","memory_type":"rule","content":"c","metadata":{{}},"ts":1700000000}}
"#
                , Uuid::new_v4()
            ),
        )
        .unwrap();
        // Crash mid-append: a second entry with no trailing newline.
        use std::io::Write;
        let mut tail = serde_json::json!({ "stable_op_id": Uuid::new_v4() }).to_string();
        tail.push('x');
        std::fs::OpenOptions::new()
            .append(true)
            .open(&wal)
            .unwrap()
            .write_all(tail.as_bytes())
            .unwrap();

        let report = svc.status().unwrap();
        assert!(
            report.contains("outbox: 1 pending"),
            "fragment dropped, one durable entry: {report}"
        );
    }

    #[test]
    fn status_pauses_on_a_dirty_checkout_and_names_the_paths() {
        let base = scratch("st-dirty");
        let project = scratch("st-dirty-proj");
        let (svc, dir) = init_joined(&base, &project, &["alpha"]);
        std::fs::write(dir.join("junk-record.json"), "{}").unwrap();

        let report = svc.status().unwrap();
        assert!(
            report.contains("health: paused: dirty-checkout"),
            "dirty pauses sync: {report}"
        );
        assert!(
            report.contains("junk-record.json"),
            "the dirty path is named: {report}"
        );
        assert!(
            !report.contains("health: healthy"),
            "no double health verdicts: {report}"
        );
    }

    #[test]
    fn the_fresh_init_manifest_alone_does_not_read_as_dirty() {
        // The init-time untracked manifest.json is init's own artifact;
        // pausing every brand-new mesh on it would be a false alarm. An
        // EDITED manifest (`M`) still counts — covered by dirty_pages above
        // via junk files and here by the healthy verdict.
        let base = scratch("st-pristine");
        let project = scratch("st-pristine-proj");
        let (svc, _dir) = init_joined(&base, &project, &["alpha"]);
        let report = svc.status().unwrap();
        assert!(report.contains("health: healthy"), "{report}");
    }

    #[test]
    fn status_pauses_on_a_detached_head() {
        let base = scratch("st-det");
        let project = scratch("st-det-proj");
        let (mut svc, dir) = init_joined(&base, &project, &["alpha"]);
        commit_manifest(&mut svc, "node-7");
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        git_ok(&dir, &["checkout", "-q", &sha]);

        let report = svc.status().unwrap();
        assert!(report.contains("health: paused: detached HEAD"), "{report}");
    }

    // --- slice 7: sync-enqueue (hook target) --------------------------------

    #[test]
    fn sync_enqueue_pulls_then_pushes_and_reports_one_line() {
        let base = scratch("sync-ok-base");
        let project = scratch("sync-ok-proj");
        let bare = base.join("team.git");
        git_ok(&base, &["init", "--bare", "-q", &bare.to_string_lossy()]);

        let mut svc = MeshService::new(project.clone());
        svc.init_mesh(
            &base,
            "node-7",
            "enqueue-team",
            &["alpha".to_string()],
            Some(bare.to_str().unwrap()),
        )
        .expect("init with remote");
        commit_manifest(&mut svc, "node-7");

        let reply = sync_enqueue_line(&mut svc);
        assert!(reply.starts_with("mesh sync-enqueued:"), "reply: {reply}");
        assert!(!reply.contains("failed"), "success reads success: {reply}");
        assert_eq!(reply.lines().count(), 1, "one line for the hook");
    }

    #[test]
    fn sync_enqueue_on_an_unreachable_remote_reports_soft_and_singly() {
        // The spec §Hooks invariant under a genuinely failing transport:
        // one line names "non-blocking", the work stays queued locally, and
        // the caller (cli.rs test side) exits 0.
        let base = scratch("sync-dead-base");
        let project = scratch("sync-dead-proj");
        let dead_remote = base.join("does-not-exist.git");

        let mut svc = MeshService::new(project.clone());
        svc.init_mesh(
            &base,
            "node-7",
            "dead-team",
            &["alpha".to_string()],
            Some(dead_remote.to_str().unwrap()),
        )
        .expect("init with a remote to nowhere");
        commit_manifest(&mut svc, "node-7");

        let reply = sync_enqueue_line(&mut svc);
        assert!(
            reply.starts_with("mesh sync-enqueue failed (non-blocking"),
            "soft failure shape: {reply}"
        );
        assert_eq!(
            reply.lines().count(),
            1,
            "Git stderr must be flattened: {reply}"
        );
        assert!(
            reply.contains("queued locally"),
            "the hook learns the work is not lost: {reply}"
        );
    }

    #[test]
    fn sync_enqueue_without_a_mesh_is_quiet_nothing_to_do() {
        let project = scratch("sync-lonely-proj");
        let mut svc = MeshService::new(project);
        let reply = sync_enqueue_line(&mut svc);
        assert!(
            reply.contains("no mesh joined") && reply.contains("nothing to do"),
            "{reply}"
        );
    }

    // --- slice 7: recover ----------------------------------------------------

    #[test]
    fn recover_on_a_detached_checkout_names_the_manual_reattach() {
        let base = scratch("rec-det-base");
        let project = scratch("rec-det-proj");
        let (mut svc, dir) = init_joined(&base, &project, &["alpha"]);
        commit_manifest(&mut svc, "node-7");
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        git_ok(&dir, &["checkout", "-q", &sha]);

        let report = recover_report(&project);
        assert!(report.contains("detached HEAD"), "{report}");
        assert!(
            report.contains("reattach manually"),
            "the fix is manual: {report}"
        );
        assert!(
            report.contains("never `git reset --hard`"),
            "spec §Recovery wording: {report}"
        );
        assert!(
            report.contains("pause sync"),
            "state language mirrors spec: {report}"
        );
    }

    #[test]
    fn recover_on_a_dirty_checkout_names_the_paths_and_the_manual_fix() {
        let base = scratch("rec-dirty-base");
        let project = scratch("rec-dirty-proj");
        let (_svc, dir) = init_joined(&base, &project, &["alpha"]);
        std::fs::write(dir.join("junk-record.json"), "{}").unwrap();

        let report = recover_report(&project);
        assert!(report.contains("dirty working tree"), "{report}");
        assert!(
            report.contains("junk-record.json"),
            "the dirty path is named: {report}"
        );
        assert!(
            report.contains("commit or stash"),
            "the fix is a manual commit/stash: {report}"
        );
        assert!(
            report.contains("never `git reset --hard`"),
            "spec §Recovery: never automatic reset --hard: {report}"
        );
        assert!(
            report.contains("pause sync, local store untouched"),
            "state language mirrors spec: {report}"
        );
        // Plus a read-back to prove nothing got fixed: the junk file survived.
        assert!(
            dir.join("junk-record.json").exists(),
            "recover is print-only — it never removes the dirty file"
        );
    }

    #[test]
    fn recover_on_a_healthy_mesh_says_so_and_fixes_nothing() {
        let base = scratch("rec-ok-base");
        let project = scratch("rec-ok-proj");
        let (_svc, _dir) = init_joined(&base, &project, &["alpha"]);
        let report = recover_report(&project);
        assert!(report.contains("healthy: nothing to recover"), "{report}");
    }

    #[test]
    fn recover_without_a_mesh_answers_the_canonical_prefix() {
        let project = scratch("rec-lonely");
        let report = recover_report(&project);
        assert!(report.starts_with("no mesh joined"), "{report}");
    }

    #[test]
    fn ages_are_human() {
        assert_eq!(age_of(5), "5s");
        assert_eq!(age_of(59), "59s");
        assert_eq!(age_of(60), "1m");
        assert_eq!(age_of(5400), "1h");
        assert_eq!(age_of(90000), "1d");
    }
}
