//! MeshStorage — the daemon-owned mesh checkout, one standalone git repo per
//! mesh at `~/.NeuroStrata/synaptic-mesh/<mesh_id>/` (see spec §"Storage
//! layout", Approach D). The project tree holds nothing but the tiny pointer
//! file `<project>/.NeuroStrata/synaptic-mesh-ref`.
//!
//! The mesh repo is a peer of the project repo, never nested in it: git's
//! content-addressable tree *is* the snapshot, `git log` *is* the event log,
//! and per-record files keep concurrent writers conflict-free.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use super::manifest::{MeshManifest, MeshPolicy, MESH_MANIFEST_SCHEMA_VERSION};

/// The manifest's name inside every mesh repo.
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
/// Directory under the project root that holds the mesh pointer.
pub const POINTER_DIR_NAME: &str = ".NeuroStrata";
/// The pointer file itself: `<mesh_id>\t<mesh_dir>\t<remote or ->`.
pub const POINTER_FILE_NAME: &str = "synaptic-mesh-ref";

/// A handle to one mesh checkout: the directory, plus the manifest as read at
/// open time.
#[derive(Debug)]
pub struct MeshStorage {
    root: PathBuf,
    manifest: MeshManifest,
}

impl MeshStorage {
    /// Create a NEW mesh: `<base_root>/<new uuid v4>/`, `git init`, write
    /// `manifest.json` (schema `synaptic-mesh/v1`, epoch 1, default policy).
    /// `base_root` is normally `~/.NeuroStrata/synaptic-mesh`; tests pass a
    /// scratch dir. `remote` only records the URL and configures `origin` —
    /// no network happens here.
    pub fn init(
        base_root: &Path,
        node_id: &str,
        mesh_name: &str,
        namespaces: &[String],
        remote: Option<&str>,
    ) -> Result<MeshStorage> {
        let mesh_id = Uuid::new_v4();
        let root = base_root.join(mesh_id.to_string());
        std::fs::create_dir_all(&root).with_context(|| {
            format!(
                "mesh checkout dir could not be created at {}",
                root.display()
            )
        })?;

        run_ok(&root, &["init"])?;
        if let Some(url) = remote {
            run_ok(&root, &["remote", "add", "origin", url])?;
        }

        let manifest = MeshManifest {
            schema_version: MESH_MANIFEST_SCHEMA_VERSION.to_string(),
            mesh_id,
            mesh_name: mesh_name.to_string(),
            created_at: chrono::Utc::now(),
            created_by: node_id.to_string(),
            epoch: 1,
            namespaces: namespaces.to_vec(),
            policy: MeshPolicy::default(),
            remote: remote.map(str::to_string),
            signers: Vec::new(),
        };
        write_manifest(&root, &manifest)?;

        // AK-H1: the ignore rule goes in before the first `git add -A` so the
        // private signing key can never enter history through the first
        // commit.
        let mesh = MeshStorage { root, manifest };
        mesh.ensure_signing_key_ignored()?;
        mesh.ensure_bundles_ignored()?;

        Ok(mesh)
    }

    /// Open an EXISTING mesh checkout. Refuses any manifest whose
    /// `schema_version` is not `synaptic-mesh/v1`, naming the offending
    /// version, so an old daemon cannot silently misread a newer mesh.
    pub fn open(mesh_dir: &Path) -> Result<MeshStorage> {
        let path = mesh_dir.join(MANIFEST_FILE_NAME);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("mesh manifest could not be read at {}", path.display()))?;
        let manifest: MeshManifest = serde_json::from_str(&text).with_context(|| {
            format!(
                "mesh manifest at {} is not valid {}",
                path.display(),
                MESH_MANIFEST_SCHEMA_VERSION
            )
        })?;
        if manifest.schema_version != MESH_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported mesh manifest schema_version {:?} at {} (expected {})",
                manifest.schema_version,
                path.display(),
                MESH_MANIFEST_SCHEMA_VERSION
            );
        }

        Ok(MeshStorage {
            root: mesh_dir.to_path_buf(),
            manifest,
        })
    }

    /// The project-side pointer: `<project>/.NeuroStrata/synaptic-mesh-ref`,
    /// one line, `<mesh_id>\t<mesh_dir>\t<remote or ->`. Created by `join`,
    /// deleted by `leave`.
    pub fn write_pointer(
        project: &Path,
        mesh_id: Uuid,
        mesh_dir: &Path,
        remote: Option<&str>,
    ) -> Result<()> {
        let path = Self::pointer_path(project);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("pointer dir could not be created at {}", parent.display())
            })?;
        }
        let line = format!(
            "{}\t{}\t{}\n",
            mesh_id,
            mesh_dir.display(),
            remote.unwrap_or("-")
        );
        std::fs::write(&path, line)
            .with_context(|| format!("mesh pointer could not be written to {}", path.display()))?;
        Ok(())
    }

    /// Read the project pointer. `Ok(None)` when the project has no mesh.
    pub fn read_pointer(project: &Path) -> Result<Option<(Uuid, PathBuf, Option<String>)>> {
        let path = Self::pointer_path(project);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("mesh pointer could not be read at {}", path.display())
                })
            }
        };

        let fields: Vec<&str> = text.trim_end().split('\t').collect();
        if fields.len() != 3 {
            bail!(
                "mesh pointer at {} is malformed: expected `<mesh_id>\\t<mesh_dir>\\t<remote or ->`, got {:?}",
                path.display(),
                text
            );
        }
        let mesh_id = Uuid::parse_str(fields[0]).with_context(|| {
            format!(
                "mesh pointer at {} names an invalid mesh id {:?}",
                path.display(),
                fields[0]
            )
        })?;
        let mesh_dir = PathBuf::from(fields[1]);
        // The wire format writes `-` for "no remote" (a bare tab-separated
        // empty field would be ambiguous with a typo).
        let remote = match fields[2] {
            "-" | "" => None,
            url => Some(url.to_string()),
        };
        Ok(Some((mesh_id, mesh_dir, remote)))
    }

    /// `leave()`: drop the project's link to the mesh. Idempotent — a project
    /// with no pointer is already left.
    pub fn remove_pointer(project: &Path) -> Result<()> {
        let path = Self::pointer_path(project);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| {
                format!("mesh pointer could not be removed at {}", path.display())
            }),
        }
    }

    pub fn manifest(&self) -> &MeshManifest {
        &self.manifest
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stage + commit everything in the mesh repo as `node_id`. Returns
    /// `Ok(false)` on a no-op (nothing changed) — an empty commit is a bug
    /// in the caller, never a fallback.
    pub fn commit_all(&self, message: &str, node_id: &str) -> Result<bool> {
        self.ensure_signing_key_ignored()?;
        self.ensure_bundles_ignored()?;
        run_ok(&self.root, &["add", "-A"])?;
        let staged = run_ok(&self.root, &["status", "--porcelain"])?;
        if String::from_utf8_lossy(&staged.stdout).trim().is_empty() {
            return Ok(false);
        }

        let user_name = format!("user.name={}", node_id);
        let user_email = format!("user.email={}@synaptic-mesh.local", node_id);
        let args = [
            "-c",
            user_name.as_str(),
            "-c",
            user_email.as_str(),
            "commit",
            "-m",
            message,
        ];
        run_ok(&self.root, &args)?;
        Ok(true)
    }

    /// `git fetch` + fast-forward merge from the configured remote. Err when
    /// no remote is set, or when history has genuinely diverged (operator
    /// resolves; never force-overwrite).
    pub fn pull(&self) -> Result<()> {
        self.require_remote()?;
        // `origin HEAD` = the remote's default branch, written as the single
        // line of FETCH_HEAD (a bare `git fetch origin` lists every branch and
        // the first line is not necessarily the one to merge).
        run_ok(&self.root, &["fetch", "origin", "HEAD"])?;
        // NOT bare `FETCH_HEAD`: git special-cases that literal name and skips
        // FETCH_HEAD lines marked `not-for-merge` (which is what a fetch that
        // also updated a remote-tracking ref writes), silently reporting
        // "Already up to date" without merging. `FETCH_HEAD^{commit}` peels to
        // the commit and bypasses the special case. --ff-only: divergent
        // history is an operator decision, never a silent merge commit.
        run_ok(&self.root, &["merge", "--ff-only", "FETCH_HEAD^{commit}"])?;
        Ok(())
    }

    /// `git push` the current checkout to the configured remote. Err when no
    /// remote is set.
    pub fn push(&self) -> Result<()> {
        self.require_remote()?;
        run_ok(&self.root, &["push", "origin", "HEAD"])?;
        Ok(())
    }

    fn require_remote(&self) -> Result<()> {
        match &self.manifest.remote {
            Some(url) if !url.trim().is_empty() => Ok(()),
            _ => bail!(
                "mesh {} has no remote configured; join a mesh with a remote before pull/push",
                self.manifest.mesh_id
            ),
        }
    }

    /// Keep the local Ed25519 signing key out of the published history.
    ///
    /// The default key path is `<mesh_root>/.neurostrata-mesh/signing.key`
    /// (see `worker.rs`) and [`commit_all`] stages with `git add -A`, so
    /// without an explicit ignore rule `git add -A` would publish the
    /// private key into the team-shared history on the very next commit.
    /// Both patterns are enforced because a mesh may hand its key a
    /// different physical name through `NEUROSTRATA_MESH_SIGNING_KEY`.
    /// Idempotent: existing lines are never duplicated or rewritten.
    fn ensure_signing_key_ignored(&self) -> Result<()> {
        self.ensure_ignored_lines(&[".neurostrata-mesh/signing.key", "*.key"])
    }

    /// Keep drain bundles out of the published history.
    ///
    /// A drain's opt-in compressed bundle lands in `<mesh_root>/bundles/`
    /// (see `worker.rs`) and is a local-only artifact: it is a lagging
    /// duplicate replay source, never the drain's publication channel.
    /// [`commit_all`] stages with `git add -A`, so without an explicit
    /// ignore rule a written bundle would be staged by a later drain —
    /// and, until then, it would trip the import dirty gate as an
    /// untracked path. A tracked bundle would also be re-scanned on
    /// every import pass. The rule is artifact-scoped
    /// (`/bundles/*.bundle.zst`): `write_compressed_bundle` writes
    /// exactly `<name>.bundle.zst` directly into `bundles/` — the
    /// drain passes `drain-<millis>` as the name — via a plain
    /// `std::fs::write`, with no staging file and no rename, so that
    /// suffix is the whole artifact set in-flight and at rest (a
    /// partial write still carries it). Only those files are ignored.
    /// Namespaces are root-level directories
    /// (`<root>/<namespace>/{memories,procedures}/<id>.json`; see
    /// `record_path` in `worker.rs`), so a namespace literally named
    /// `bundles` keeps its `memories/`/`procedures/` records stageable
    /// and publishing normally — the previous directory-level rule
    /// (`/bundles`) silently swallowed them: the drain marked them
    /// committed while `commit_all`'s `git add -A` never staged them. The
    /// trade: a stray non-artifact file placed directly in `bundles/`
    /// — or at the `bundles` path itself, as a regular file or a
    /// symlink — is no longer ignore-covered. It shows in
    /// `git status --porcelain` and pauses the import dirty gate, but
    /// only until the next committing drain: [`commit_all`] stages
    /// with an unconditional `git add -A` and no drain-side gate
    /// check, so that drain sweeps the stray into tracked,
    /// peer-published mesh history (a file or symlink at `bundles`
    /// is committed as `bundles`), after which porcelain is clean
    /// again. Under the superseded directory-level rule the whole
    /// `bundles` path was ignore-covered, so a stray stayed hidden
    /// indefinitely. Idempotent: existing lines are never duplicated
    /// or rewritten.
    fn ensure_bundles_ignored(&self) -> Result<()> {
        self.ensure_ignored_lines(&["/bundles/*.bundle.zst"])
    }

    /// Append any of `wanted` missing from the checkout's `.gitignore`.
    ///
    /// Shared body of the ignore-rule wrappers above: reads the file,
    /// appends only the missing lines under a header naming the artifact
    /// class being kept local, and never rewrites existing lines.
    /// Idempotent.
    fn ensure_ignored_lines(&self, wanted: &[&str]) -> Result<()> {
        let path = self.root.join(".gitignore");
        let existing = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("failed to read {}: {e}", path.display());
                }
                String::new()
            }
        };
        let have: Vec<&str> = existing.lines().map(|l| l.trim()).collect();
        let missing: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|w| !have.contains(w))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut text = existing;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        // The header names the artifact class this call is keeping local;
        // both rule sets share the "never publish" rationale.
        if wanted.contains(&"/bundles/*.bundle.zst") {
            text.push_str(
                "# Drain bundles are local-only artifacts — never publish to the mesh history.\n",
            );
        } else {
            text.push_str(
                "# Private Ed25519 signing material — never publish to the mesh history.\n",
            );
        }
        for entry in &missing {
            text.push_str(entry);
            text.push('\n');
        }
        std::fs::write(&path, text)
            .with_context(|| format!(".gitignore could not be written to {}", path.display()))?;
        Ok(())
    }

    /// The project-side pointer path: `<project>/.NeuroStrata/synaptic-mesh-ref`.
    pub(crate) fn pointer_path(project: &Path) -> PathBuf {
        project.join(POINTER_DIR_NAME).join(POINTER_FILE_NAME)
    }
}

fn write_manifest(root: &Path, manifest: &MeshManifest) -> Result<()> {
    let path = root.join(MANIFEST_FILE_NAME);
    // Pretty JSON, one field per line, matching the spec's record format and
    // keeping `git diff` on the manifest reviewable.
    let json =
        serde_json::to_string_pretty(manifest).context("manifest could not be serialized")?;
    std::fs::write(&path, format!("{}\n", json))
        .with_context(|| format!("manifest could not be written to {}", path.display()))?;
    Ok(())
}

/// Default bound, in seconds, for one mesh git subprocess.
pub(crate) const GIT_COMMAND_TIMEOUT_SECS: u64 = 30;

/// How often the bounded wait polls the child for exit.
const GIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// After the child exits (normally, or killed on timeout), how long **each**
/// drained stream is given to reach EOF. A grandchild that inherited the pipes
/// (a remote helper, an ssh transport) can outlive the direct child; the bound
/// must not depend on it.
const GIT_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// The per-call bound for mesh git subprocesses. `NEUROSTRATA_GIT_TIMEOUT_SECS`
/// is read per call so an operator can widen or tighten it without a restart;
/// unset, zero, or unparsable values fall back to [`GIT_COMMAND_TIMEOUT_SECS`]
/// rather than disabling the bound.
pub(crate) fn git_timeout() -> Duration {
    let secs = std::env::var("NEUROSTRATA_GIT_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs >= 1)
        .unwrap_or(GIT_COMMAND_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Run one git subcommand non-interactively (`GIT_TERMINAL_PROMPT=0`: a
/// credential prompt would wedge the daemon) inside the mesh checkout, bounded
/// by [`git_timeout`].
///
/// stdout and stderr are drained on dedicated threads while the child runs, so
/// a chatty git cannot deadlock on a full OS pipe buffer. The child is polled
/// until it exits or the deadline passes; on expiry it is killed, reaped with
/// `wait` (no zombie survives this call), and the error names the subcommand
/// and the bound. Either way the drain is bounded: a grandchild that inherited
/// the pipes cannot hold the caller past [`GIT_DRAIN_GRACE`] per stream, and
/// whatever the drain threads delivered is returned in full.
pub(crate) fn run(root: &Path, args: &[&str]) -> Result<Output> {
    let timeout = git_timeout();
    let display = args.join(" ");
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The child leads its own process group, so a timeout kill reaches git's
    // helpers/descendants (transports, pagers, hooks) too — killing only the
    // direct child leaves them running and holding the inherited pipes.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("git {display} could not be executed"))?;

    let mut child_stdout = child.stdout.take().expect("stdout is piped");
    let mut child_stderr = child.stderr.take().expect("stderr is piped");
    // Nonblocking pipes + a cancel flag bound the reader threads' lifetime:
    // a blocking read_to_end cannot be interrupted when a descendant holds
    // the pipe open, so without this the drain grace expiring would leave
    // both readers (and their descriptors) behind forever.
    #[cfg(unix)]
    {
        set_nonblocking(&child_stdout);
        set_nonblocking(&child_stderr);
    }
    let reader_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    // Builder::spawn, not thread::spawn: a failed reader spawn must not
    // panic past the child — the already-spawned git is killed and reaped
    // (bounded) before the error returns, never left running unowned.
    let stdout_sink = std::sync::Arc::clone(&stdout_buf);
    let stdout_cancel = std::sync::Arc::clone(&reader_cancel);
    if let Err(error) = std::thread::Builder::new().spawn(move || {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if stdout_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match child_stdout.read(&mut chunk) {
                Ok(0) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(GIT_POLL_INTERVAL)
                }
                Err(_) => break,
                Ok(read) => stdout_sink
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend_from_slice(&chunk[..read]),
            }
        }
        let _ = stdout_tx.send(());
    }) {
        kill_child(&mut child);
        reap_within(child, GIT_DRAIN_GRACE);
        return Err(error)
            .with_context(|| format!("git {display} could not start its stdout reader"));
    }
    let stderr_sink = std::sync::Arc::clone(&stderr_buf);
    let stderr_cancel = std::sync::Arc::clone(&reader_cancel);
    if let Err(error) = std::thread::Builder::new().spawn(move || {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if stderr_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match child_stderr.read(&mut chunk) {
                Ok(0) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(GIT_POLL_INTERVAL)
                }
                Err(_) => break,
                Ok(read) => stderr_sink
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend_from_slice(&chunk[..read]),
            }
        }
        let _ = stderr_tx.send(());
    }) {
        reader_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        kill_child(&mut child);
        reap_within(child, GIT_DRAIN_GRACE);
        return Err(error)
            .with_context(|| format!("git {display} could not start its stderr reader"));
    }

    // Elapsed-based deadline, never `Instant::now() + timeout`: a huge
    // configured bound (e.g. NEUROSTRATA_GIT_TIMEOUT_SECS=u64::MAX) overflows
    // the representable Instant range and the addition panics, orphaning the
    // spawned git. Comparing elapsed time cannot overflow.
    let started = Instant::now();
    let status: ExitStatus = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(GIT_POLL_INTERVAL),
            Ok(None) => {
                kill_child(&mut child);
                reap_within(child, GIT_DRAIN_GRACE);
                // Detach rather than block forever on a pipe a grandchild may
                // still hold: the drain threads exit on their own once the
                // last writer closes.
                drain_within(
                    &stdout_rx,
                    &stderr_rx,
                    &stdout_buf,
                    &stderr_buf,
                    &reader_cancel,
                );
                bail!(
                    "git {display} timed out after {}s: process killed (mesh git timeout; override with NEUROSTRATA_GIT_TIMEOUT_SECS)",
                    timeout.as_secs()
                );
            }
            Err(error) => {
                kill_child(&mut child);
                reap_within(child, GIT_DRAIN_GRACE);
                drain_within(
                    &stdout_rx,
                    &stderr_rx,
                    &stdout_buf,
                    &stderr_buf,
                    &reader_cancel,
                );
                return Err(error).with_context(|| format!("git {display} could not be waited on"));
            }
        }
    };

    // The child exited cleanly, but EOF is not guaranteed: a grandchild may
    // still hold the inherited pipes. Drain bounded (never a bare recv()) so a
    // normal exit cannot hang, while a closed pipe still yields the full
    // captured bytes.
    let (stdout, stderr) = drain_within(
        &stdout_rx,
        &stderr_rx,
        &stdout_buf,
        &stderr_buf,
        &reader_cancel,
    );
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Put a pipe read-end into nonblocking mode so its reader thread can poll
/// the cancel flag between reads instead of parking in `read` forever.
#[cfg(unix)]
fn set_nonblocking<Fd: std::os::fd::AsRawFd>(fd: &Fd) {
    // SAFETY: fcntl on a descriptor this call owns; failure leaves the
    // descriptor blocking, which degrades to the pre-fix drain behaviour.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// Terminate the child and, on unix, its whole process group: git's
/// helpers inherit the pipes and can outlive a direct-child-only kill,
/// continuing work after the call has already timed out. The group was
/// created for this child at spawn (`process_group(0)`), so the group kill
/// cannot reach unrelated processes.
fn kill_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = child.id() as libc::pid_t;
        if pgid > 0 {
            // SAFETY: killpg on the child's own group; a dead group is a
            // no-op error we deliberately ignore before the direct kill.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

/// Reap a killed child, but only within `bound`: a child stuck in
/// uninterruptible kernel I/O can ignore the kill for an unbounded time, and
/// a bare `wait()` would hold the caller past the timeout the kill was meant
/// to enforce. Returns false when the child is still unreaped at the bound;
/// the caller proceeds anyway (the kill is already delivered, so the child
/// is terminated as soon as the kernel lets it die).
///
/// On the false path the child is NOT dropped: `Child` drop does not reap,
/// so a process that dies after the bound would linger as a zombie until
/// daemon exit, accumulating across occurrences. Instead ownership moves to
/// a detached reaper thread that blocks on `wait()` off the caller's path —
/// the caller still returns within `bound`, and the zombie is collected
/// whenever the process finally exits. If the reaper thread itself cannot
/// be spawned, the child drops unreaped, degrading to the pre-reaper
/// behaviour rather than blocking or panicking past the bound.
fn reap_within(mut child: std::process::Child, bound: Duration) -> bool {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if started.elapsed() < bound => std::thread::sleep(GIT_POLL_INTERVAL),
            _ => {
                let _ = std::thread::Builder::new().spawn(move || {
                    let _ = child.wait();
                });
                return false;
            }
        }
    }
}

/// Wait at most [`GIT_DRAIN_GRACE`] for **each** drained stream to reach EOF
/// (their threads keep running if they do not) and return whatever arrived.
/// A per-stream budget keeps a grandchild holding stdout from starving the
/// stderr capture, and vice versa.
fn drain_within(
    stdout_rx: &std::sync::mpsc::Receiver<()>,
    stderr_rx: &std::sync::mpsc::Receiver<()>,
    stdout_buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr_buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader_cancel: &std::sync::atomic::AtomicBool,
) -> (Vec<u8>, Vec<u8>) {
    // Wait for EOF only within the grace, but never discard bytes already
    // read: a grandchild can hold the pipe open after the direct child has
    // written the output we need.
    let _ = stdout_rx.recv_timeout(GIT_DRAIN_GRACE);
    let _ = stderr_rx.recv_timeout(GIT_DRAIN_GRACE);
    // The drain budget is spent: cancel any reader still blocked on a pipe a
    // descendant holds open, so the threads exit and release their
    // descriptors instead of accumulating across mesh attempts.
    reader_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let stdout = stdout_buf
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let stderr = stderr_buf
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    (stdout, stderr)
}

pub(crate) fn run_ok(root: &Path, args: &[&str]) -> Result<Output> {
    let out = run(root, args)?;
    if out.status.success() {
        return Ok(out);
    }
    bail!(
        "git {} failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// Test-only scaffolding shared by every module that feeds the mesh git
/// funnel (`storage`, `import`, `mesh_service`): a PATH shim that intercepts
/// `git` for scratch checkouts carrying a marker file, plus an env guard that
/// restores the process environment on drop.
#[cfg(all(test, unix))]
pub(crate) mod git_test_support {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Set/unset one environment variable and restore the previous value when
    /// dropped, so a panicking test cannot leak its value into the process.
    pub(crate) struct EnvRestore {
        key: &'static str,
        prior: Option<OsString>,
    }

    impl EnvRestore {
        pub(crate) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let prior = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prior }
        }

        pub(crate) fn unset(key: &'static str) -> Self {
            let prior = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, prior }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// A shim `git` executable. It falls through to the real git for every
    /// checkout that does not carry a marker file, so parallel tests that
    /// also spawn git keep working while a test holds the PATH override:
    ///
    /// - `.hang-git` makes the shim record its pid and `exec sleep 60` (a
    ///   stuck git), which is how the watchdog tests prove kill + reap;
    /// - `.flood-git` writes 2 MiB to stdout and 1 MiB to stderr, far past
    ///   any OS pipe buffer, which is how the drain test proves no deadlock.
    /// - `.grandchild-git` leaves a background grandchild (`sleep 30`)
    ///   holding both inherited pipes and exits 0 immediately, which is how
    ///   the normal-exit test proves EOF is not a precondition for return.
    /// - `.hangtree-git` hangs like `.hang-git` but first spawns a
    ///   `sleep 60` descendant in the same process group (its pid lands in
    ///   `git-grandchild.pid` next to the pid file), which is how the
    ///   timeout test proves the kill reaches the whole tree.
    pub(crate) struct GitShim {
        dir: PathBuf,
    }

    impl GitShim {
        pub(crate) fn install(tag: &str, pid_file: &Path) -> Self {
            let real = Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|path| path.trim().to_string())
                .filter(|path| !path.is_empty())
                .expect("git must be on PATH to install the test shim");
            let dir = std::env::temp_dir().join(format!(
                "neurostrata-mesh-git-shim-{}-{}-{}",
                std::process::id(),
                tag,
                uuid::Uuid::new_v4()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let script = format!(
                "#!/bin/sh\n\
                 target=.\n\
                 if [ \"$1\" = \"-C\" ]; then target=\"$2\"; fi\n\
                 if [ -f \"$target/.hang-git\" ]; then\n\
                 \tprintf '%s\\n' \"$$\" > '{pid}'\n\
                 \texec sleep 60\n\
                 fi\n\
                 if [ -f \"$target/.flood-git\" ]; then\n\
                 \thead -c 2097152 /dev/zero\n\
                 \thead -c 1048576 /dev/zero >&2\n\
                 \texit 0\n\
                 fi\n\
                 if [ -f \"$target/.grandchild-git\" ]; then\n\
                 \tprintf 'grandchild-payload\\n'\n\
                 \tsleep 30 &\n\
                 \texit 0\n\
                 fi\n\
                 if [ -f \"$target/.hangtree-git\" ]; then\n\
                 \tsleep 60 &\n\
                 \tprintf '%s\\n' \"$!\" > '{gpid}'\n\
                 \tprintf '%s\\n' \"$$\" > '{pid}'\n\
                 \texec sleep 60\n\
                 fi\n\
                 exec '{real}' \"$@\"\n",
                pid = pid_file.display(),
                gpid = pid_file.with_file_name("git-grandchild.pid").display(),
                real = real,
            );
            let path = dir.join("git");
            std::fs::write(&path, script).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            Self { dir }
        }

        /// Prepend the shim dir to PATH. Hold the returned guard for the
        /// duration of the test; drop restores the original PATH.
        pub(crate) fn activate(&self) -> EnvRestore {
            let mut path = OsString::from(self.dir.as_os_str());
            if let Some(existing) = std::env::var_os("PATH") {
                path.push(":");
                path.push(existing);
            }
            EnvRestore::set("PATH", path)
        }
    }

    pub(crate) fn read_pid(pid_file: &Path) -> u32 {
        std::fs::read_to_string(pid_file)
            .expect("the shim must record its pid")
            .trim()
            .parse()
            .expect("shim pid is a u32")
    }

    /// `kill -0` succeeds for a live OR zombie process, so it is exactly the
    /// oracle for "the child was killed and then reaped by `wait`".
    pub(crate) fn assert_reaped(pid: u32) {
        let status = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill -0 must be runnable");
        assert!(
            !status.success(),
            "child pid {pid} is gone only if it was killed AND reaped with wait (kill -0 still sees it)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Scratch dir keyed by pid + tag + uuid: parallel tests never collide,
    /// and a leftover from a crashed run is cleared first.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-mesh-storage-{}-{}-{}",
            std::process::id(),
            tag,
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn init_under(base: &Path) -> MeshStorage {
        MeshStorage::init(
            base,
            "node-abc123",
            "stripe-api-team",
            &["stripe-api".to_string()],
            None,
        )
        .unwrap()
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
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
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn init_creates_dir_repo_and_manifest() {
        let base = scratch("init");
        let mesh = init_under(&base);

        let root = mesh.root();
        assert!(root.is_dir(), "mesh dir must exist at {}", root.display());
        assert!(root.join(".git").is_dir(), "`git init` must create .git");
        assert!(
            root.join(MANIFEST_FILE_NAME).is_file(),
            "manifest.json must be written at init"
        );

        let on_disk: MeshManifest =
            serde_json::from_str(&std::fs::read_to_string(root.join(MANIFEST_FILE_NAME)).unwrap())
                .expect("manifest.json parses as MeshManifest");
        assert_eq!(on_disk.schema_version, MESH_MANIFEST_SCHEMA_VERSION);
        assert_eq!(on_disk.mesh_name, "stripe-api-team");
        assert_eq!(on_disk.namespaces, vec!["stripe-api".to_string()]);
        assert_eq!(on_disk.epoch, 1);
        assert_eq!(on_disk.created_by, "node-abc123");
        assert_eq!(on_disk.policy, MeshPolicy::default());
        assert!(on_disk.remote.is_none());

        // The checkout dir is keyed by the manifest's mesh id (uuid v4).
        let dir_id = root.file_name().unwrap().to_str().unwrap();
        assert_eq!(dir_id, on_disk.mesh_id.to_string());
        let parsed = Uuid::parse_str(dir_id).expect("mesh dir name is a uuid");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
    }

    #[test]
    fn manifest_round_trips_to_disk() {
        let base = scratch("roundtrip");
        let mesh = init_under(&base);
        let original = mesh.manifest().clone();

        let reopened = MeshStorage::open(mesh.root()).expect("open freshly inited mesh");
        assert_eq!(reopened.manifest(), &original);
        assert_eq!(reopened.root(), mesh.root());
    }

    #[test]
    fn open_refuses_unknown_schema_version() {
        let base = scratch("schema-mismatch");
        let mesh = init_under(&base);
        let path = mesh.root().join(MANIFEST_FILE_NAME);

        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["schema_version"] = serde_json::json!("synaptic-mesh/v99");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let err = MeshStorage::open(mesh.root())
            .expect_err("a future schema_version must be refused")
            .to_string();
        assert!(
            err.contains("synaptic-mesh/v99"),
            "error must name the offending version, got: {err}"
        );
    }

    #[test]
    fn pointer_round_trips() {
        let project = scratch("pointer");
        let mesh_dir = PathBuf::from("/opt/neurostrata/synaptic-mesh/abc");

        assert!(
            MeshStorage::read_pointer(&project).unwrap().is_none(),
            "a project with no pointer file is not in a mesh"
        );

        let id = Uuid::new_v4();
        let url = "https://example.com/team/mesh.git";
        MeshStorage::write_pointer(&project, id, &mesh_dir, Some(url)).unwrap();
        assert_eq!(
            MeshStorage::read_pointer(&project).unwrap(),
            Some((id, mesh_dir.clone(), Some(url.to_string())))
        );

        // A mesh with no remote writes `-`, and reads back as None.
        let id2 = Uuid::new_v4();
        MeshStorage::write_pointer(&project, id2, &mesh_dir, None).unwrap();
        assert_eq!(
            MeshStorage::read_pointer(&project).unwrap(),
            Some((id2, mesh_dir.clone(), None))
        );

        MeshStorage::remove_pointer(&project).unwrap();
        assert!(MeshStorage::read_pointer(&project).unwrap().is_none());
        // leave() is idempotent.
        MeshStorage::remove_pointer(&project).unwrap();
    }

    #[test]
    fn commit_all_records_a_commit_and_author() {
        let base = scratch("commit");
        let mesh = init_under(&base);

        // The slice-3 mesh-worker's shape: one file per record.
        let memories = mesh.root().join("stripe-api").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m1.json"), r#"{"id":"m1"}"#).unwrap();

        assert!(
            mesh.commit_all("mesh slice 2a: first record", "node-author-1")
                .unwrap(),
            "a dirty tree must commit"
        );
        assert_eq!(
            git_out(mesh.root(), &["log", "--format=%an"]),
            "node-author-1"
        );
        assert_eq!(git_out(mesh.root(), &["rev-list", "--count", "HEAD"]), "1");

        assert!(
            !mesh.commit_all("nothing changed", "node-author-1").unwrap(),
            "a clean tree is Ok(false), never an empty commit"
        );
    }

    #[test]
    fn push_and_pull_between_two_checkouts() {
        let base = scratch("sync");
        let remote = base.join("remote.git");
        let out = Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let a_dir = base.join("checkout-a");
        let a = MeshStorage::init(
            &a_dir,
            "node-a",
            "stripe-api-team",
            &["stripe-api".to_string()],
            Some(remote.to_str().unwrap()),
        )
        .unwrap();
        assert!(a.commit_all("bootstrap", "node-a").unwrap());
        a.push().unwrap();

        // Point the bare repo's HEAD at the branch A actually pushed, so the
        // clone checks it out (git init's default HEAD may not match).
        let branch = git_out(a.root(), &["branch", "--show-current"]);
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(&remote)
            .args([
                "symbolic-ref",
                "HEAD",
                format!("refs/heads/{}", branch).as_str(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let b_dir = base.join("checkout-b");
        let out = Command::new("git")
            .arg("clone")
            .arg(&remote)
            .arg(&b_dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let b = MeshStorage::open(&b_dir).unwrap();
        assert_eq!(b.manifest().mesh_id, a.manifest().mesh_id);
        assert_eq!(b.manifest(), a.manifest(), "clone must carry the manifest");

        let memories = b.root().join("stripe-api").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m2.json"), r#"{"id":"m2"}"#).unwrap();
        assert!(b.commit_all("node-b record", "node-b").unwrap());
        b.push().unwrap();
        assert_eq!(git_out(b.root(), &["log", "--format=%an", "-1"]), "node-b");

        a.pull().unwrap();
        assert!(
            a.root()
                .join("stripe-api")
                .join("memories")
                .join("m2.json")
                .is_file(),
            "pull must fast-forward the record in"
        );
    }

    #[test]
    fn push_without_remote_errors() {
        let base = scratch("no-remote");
        let mesh = init_under(&base);
        assert!(mesh.manifest().remote.is_none());

        let pull_err = mesh.pull().expect_err("pull with no remote must error");
        assert!(
            pull_err.to_string().contains("no remote"),
            "got: {pull_err}"
        );
        let push_err = mesh.push().expect_err("push with no remote must error");
        assert!(
            push_err.to_string().contains("no remote"),
            "got: {push_err}"
        );
    }

    #[test]
    fn commit_all_never_stages_or_commits_the_signing_key() {
        let base = scratch("signing-key");
        let mesh = init_under(&base);

        // The key material exists before the first commit (a feasible local
        // state: auto-sign has run or the operator pre-created the key).
        let key_dir = mesh.root().join(".neurostrata-mesh");
        std::fs::create_dir_all(&key_dir).unwrap();
        std::fs::write(key_dir.join("signing.key"), "ed25519 private bytes").unwrap();

        let memories = mesh.root().join("stripe-api").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m1.json"), r#"{"id":"m1"}"#).unwrap();
        assert!(mesh
            .commit_all("first record with a local key", "node-key")
            .unwrap());

        let gitignore = std::fs::read_to_string(mesh.root().join(".gitignore")).unwrap();
        assert!(
            gitignore.contains(".neurostrata-mesh/signing.key") && gitignore.contains("*.key"),
            ".gitignore must carry the signing-key patterns, got: {gitignore:?}"
        );
        assert_eq!(
            git_out(mesh.root(), &["ls-files", ".neurostrata-mesh/signing.key"]),
            "",
            "the signing key must never be staged or committed"
        );
        assert_eq!(
            git_out(
                mesh.root(),
                &["check-ignore", "--quiet", ".neurostrata-mesh/signing.key"]
            ),
            "",
            "check-ignore must flag the signing key"
        );

        // Idempotent: a second pass never duplicates or rewrites the rules.
        let before = gitignore.clone();
        mesh.ensure_signing_key_ignored().unwrap();
        assert_eq!(
            std::fs::read_to_string(mesh.root().join(".gitignore")).unwrap(),
            before
        );
    }

    #[test]
    fn init_ignores_drain_bundles_and_never_duplicates_the_rule() {
        let base = scratch("bundles-ignored");
        let mesh = init_under(&base);

        // Whether `git check-ignore` does NOT accept `path`: the raw
        // command, because `git_out` asserts success (exit 0 = ignored).
        let not_ignored = |path: &str| {
            !Command::new("git")
                .args(["check-ignore", "--quiet", path])
                .current_dir(mesh.root())
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap()
                .status
                .success()
        };

        let gitignore = std::fs::read_to_string(mesh.root().join(".gitignore")).unwrap();
        assert!(
            gitignore
                .lines()
                .any(|l| l.trim() == "/bundles/*.bundle.zst"),
            ".gitignore must carry the /bundles/*.bundle.zst pattern, got: {gitignore:?}"
        );
        assert_eq!(
            git_out(
                mesh.root(),
                &["check-ignore", "--quiet", "bundles/drain-1.bundle.zst"]
            ),
            "",
            "check-ignore must flag a drain bundle artifact"
        );
        // Deliberately REVERSED from the directory-level rule: the bare
        // `bundles` path is no longer ignore-covered. Under `/bundles`
        // this assertion demanded the opposite (see the P2 fix commit).
        assert!(
            not_ignored("bundles"),
            "check-ignore must NOT flag the bare bundles path"
        );
        assert!(
            not_ignored("probe/bundles/inner.txt"),
            "check-ignore must NOT flag a nested same-named path"
        );

        // Idempotent: a second pass never duplicates or rewrites the rule.
        mesh.ensure_bundles_ignored().unwrap();
        assert_eq!(
            std::fs::read_to_string(mesh.root().join(".gitignore")).unwrap(),
            gitignore
        );
    }

    /// Discriminating oracle for the artifact-scoped rule: a root-level
    /// namespace literally named `bundles` publishes its records
    /// normally, while a drain bundle artifact beside them stays
    /// local-only. Under the old directory-level rule (`/bundles`) the
    /// `git add -A` inside `commit_all` never staged the records, so
    /// the `ls-files` assertions below fail there (verified against a
    /// scratch repo carrying the old pattern).
    #[test]
    fn a_namespace_named_bundles_still_publishes_its_records() {
        let base = scratch("bundles-namespace");
        let mesh = init_under(&base);

        let memories = mesh.root().join("bundles").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m1.json"), r#"{"id":"m1"}"#).unwrap();
        let procedures = mesh.root().join("bundles").join("procedures");
        std::fs::create_dir_all(&procedures).unwrap();
        std::fs::write(procedures.join("p1.json"), r#"{"id":"p1"}"#).unwrap();
        // The artifact the rule exists for, in the same directory.
        std::fs::write(
            mesh.root().join("bundles").join("drain-1.bundle.zst"),
            b"bundle bytes",
        )
        .unwrap();

        for rel in ["bundles/memories/m1.json", "bundles/procedures/p1.json"] {
            let out = Command::new("git")
                .args(["check-ignore", "--quiet", rel])
                .current_dir(mesh.root())
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap();
            assert!(
                !out.status.success(),
                "check-ignore must NOT flag the bundles-namespace record {rel}"
            );
        }

        assert!(
            mesh.commit_all("bundles namespace records", "node-abc123")
                .unwrap(),
            "commit_all must report a commit for the namespace records"
        );
        let tracked = git_out(mesh.root(), &["ls-files"]);
        for rel in ["bundles/memories/m1.json", "bundles/procedures/p1.json"] {
            assert!(
                tracked.lines().any(|l| l == rel),
                "{rel} must be tracked, ls-files: {tracked}"
            );
        }
        assert!(
            !tracked.lines().any(|l| l.ends_with(".bundle.zst")),
            "no bundle artifact may be tracked, ls-files: {tracked}"
        );
    }

    // --- git subprocess bound (issue #416) -------------------------------

    /// A git that never returns must be killed at the configured bound, with
    /// an error naming the subcommand — and the child must be reaped, never
    /// left as a zombie waiting on a `wait` that never comes.
    #[cfg(unix)]
    #[test]
    fn git_hang_times_out_and_kills() {
        use super::git_test_support::{assert_reaped, read_pid, EnvRestore, GitShim};
        use std::time::{Duration, Instant};

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch("git-hang");
        let pid_file = root.join("git.pid");
        let shim = GitShim::install("hang", &pid_file);
        let _path = shim.activate();
        let _timeout = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", "1");
        std::fs::write(root.join(".hang-git"), b"").unwrap();

        let started = Instant::now();
        let err = run_ok(&root, &["rev-parse", "--verify", "HEAD"])
            .expect_err("a hung git must be killed, not awaited");
        let elapsed = started.elapsed();
        let msg = format!("{err:#}");

        assert!(
            elapsed < Duration::from_secs(10),
            "NEUROSTRATA_GIT_TIMEOUT_SECS=1 must bound the call, took {elapsed:?}"
        );
        assert!(
            msg.contains("rev-parse --verify HEAD"),
            "the error must name the subcommand, got: {msg}"
        );
        assert!(
            msg.contains("timed out after 1s"),
            "the error must name the bound, got: {msg}"
        );
        assert!(
            msg.contains("NEUROSTRATA_GIT_TIMEOUT_SECS"),
            "the error must name the override, got: {msg}"
        );

        assert_reaped(read_pid(&pid_file));
    }

    /// A timed-out git's descendants must die with it: the child leads its
    /// own process group and the timeout kill targets the group, so a helper
    /// holding the inherited pipes cannot outlive the call.
    #[cfg(unix)]
    #[test]
    fn git_timeout_kills_descendants_too() {
        use super::git_test_support::{read_pid, EnvRestore, GitShim};
        use std::time::{Duration, Instant};

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch("git-hangtree");
        let pid_file = root.join("git.pid");
        let shim = GitShim::install("hangtree", &pid_file);
        let _path = shim.activate();
        let _timeout = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", "1");
        std::fs::write(root.join(".hangtree-git"), b"").unwrap();

        run_ok(&root, &["rev-parse", "--verify", "HEAD"])
            .expect_err("a hung git tree must be killed, not awaited");

        let grandchild_pid = read_pid(&root.join("git-grandchild.pid"));
        // Group-kill delivery is asynchronous: poll briefly for the
        // descendant to disappear (kill -0 also sees zombies, and the
        // orphan is reaped by init once dead).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let alive = Command::new("kill")
                .arg("-0")
                .arg(grandchild_pid.to_string())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "descendant pid {grandchild_pid} outlived the timed-out git call"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Large output past any OS pipe buffer must not deadlock: both streams
    /// are drained concurrently while the child runs.
    #[cfg(unix)]
    #[test]
    fn git_large_output_does_not_deadlock() {
        use super::git_test_support::{EnvRestore, GitShim};
        use std::time::{Duration, Instant};

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch("git-flood");
        let pid_file = root.join("git.pid");
        let shim = GitShim::install("flood", &pid_file);
        let _path = shim.activate();
        let _timeout = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", "30");
        std::fs::write(root.join(".flood-git"), b"").unwrap();

        let started = Instant::now();
        let out = run_ok(&root, &["log", "--oneline"])
            .expect("a child flooding both pipes must still complete");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(20),
            "draining 3 MiB must not approach the bound, took {elapsed:?}"
        );
        assert_eq!(
            out.stdout.len(),
            2 * 1024 * 1024,
            "stdout must be captured in full"
        );
        assert_eq!(
            out.stderr.len(),
            1024 * 1024,
            "stderr must be captured in full"
        );
    }

    /// A direct child that exits 0 while a grandchild still holds the
    /// inherited pipes must not hang the caller: the normal-exit path is
    /// bounded exactly like the timeout path, and full output is still
    /// captured whenever the writers do reach EOF.
    #[cfg(unix)]
    #[test]
    fn git_normal_exit_with_grandchild_holding_pipes_returns_bounded() {
        use super::git_test_support::{EnvRestore, GitShim};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch("git-grandchild");
        let pid_file = root.join("git.pid");
        let shim = GitShim::install("grandchild", &pid_file);
        let _path = shim.activate();
        // Generous bound: the direct child exits at once, so the deadline must
        // play no part. Only the drain grace may keep the caller waiting.
        let _timeout = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", "30");
        std::fs::write(root.join(".grandchild-git"), b"").unwrap();

        let (tx, rx) = mpsc::channel();
        let thread_root = root.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = run(&thread_root, &["log", "--oneline"]);
            let _ = tx.send((result, started.elapsed()));
        });

        let (result, elapsed) = rx.recv_timeout(Duration::from_secs(5)).expect(
            "run() must return once the direct child exits 0 even when a grandchild still \
                 holds stdout/stderr; waiting for EOF is the issue #416 hang",
        );
        let out = result.expect("a clean exit must be Ok even with a grandchild holding the pipes");

        assert!(
            out.status.success(),
            "the shim exited 0, got {:?}",
            out.status
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "a normal exit must be bounded by the drain grace, took {elapsed:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "grandchild-payload",
            "output written before the grandchild held the pipe must survive the bounded drain"
        );
    }

    /// When the drain grace expires, the reader threads must be cancelled,
    /// not left blocked on pipes a grandchild still holds: repeated bounded
    /// calls must not accumulate open descriptors.
    #[cfg(target_os = "linux")]
    #[test]
    fn git_drain_cancels_reader_threads_when_grace_expires() {
        use super::git_test_support::{EnvRestore, GitShim};
        use std::time::{Duration, Instant};

        fn open_fds() -> usize {
            std::fs::read_dir("/proc/self/fd").unwrap().count()
        }

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch("git-drain-cancel");
        let pid_file = root.join("git.pid");
        let shim = GitShim::install("grandchild", &pid_file);
        let _path = shim.activate();
        let _timeout = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", "30");
        std::fs::write(root.join(".grandchild-git"), b"").unwrap();

        let baseline = open_fds();
        for _ in 0..3 {
            let out = run(&root, &["log", "--oneline"]).expect("bounded run must succeed");
            assert!(out.status.success());
        }
        // Cancelled readers exit promptly; leaked readers would hold two
        // descriptors per run for the grandchild's whole 30s life.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let leaked = open_fds().saturating_sub(baseline);
            if leaked <= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reader threads leaked {leaked} descriptors after the drain grace expired"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn git_failure_surfaces_stderr() {
        let root = scratch("git-stderr");
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "scratch repo init: {}",
            String::from_utf8_lossy(&init.stderr)
        );

        let err = run_ok(&root, &["rev-parse", "--verify", "no-such-ref-416"])
            .expect_err("a failing git must be an error");
        let msg = format!("{err:#}");

        assert!(
            msg.contains("rev-parse --verify no-such-ref-416"),
            "the error must name the subcommand, got: {msg}"
        );
        assert!(
            msg.contains("fatal:"),
            "git's own stderr must be surfaced, got: {msg}"
        );
        assert!(
            msg.contains("no-such-ref-416"),
            "stderr must identify the missing ref, got: {msg}"
        );
    }

    /// The success path is byte-for-byte what `Command::output()` produced:
    /// same stdout bytes, empty stderr, exit status preserved.
    #[test]
    fn git_success_path_unchanged() {
        let base = scratch("git-success");
        let mesh = init_under(&base);

        let out = run_ok(mesh.root(), &["rev-parse", "--is-inside-work-tree"])
            .expect("a clean probe must succeed");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"true\n", "stdout must be unchanged");
        assert!(out.stderr.is_empty(), "a quiet git writes no stderr");

        // And a real push to a bare remote still lands its refs.
        let remote = scratch("git-success-remote").join("team.git");
        let init = Command::new("git")
            .args(["init", "--bare", "-q", &remote.to_string_lossy()])
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "bare remote init: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let mesh = MeshStorage::init(
            &base,
            "node-abc123",
            "stripe-api-team",
            &["stripe-api".to_string()],
            Some(remote.to_str().unwrap()),
        )
        .unwrap();
        let memories = mesh.root().join("stripe-api").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m1.json"), r#"{"id":"m1"}"#).unwrap();
        assert!(mesh.commit_all("record", "node-abc123").unwrap());
        mesh.push().expect("push through the bounded funnel");

        let refs = Command::new("git")
            .args(["ls-remote", "--refs", &remote.to_string_lossy()])
            .output()
            .unwrap();
        assert!(refs.status.success());
        assert!(
            !refs.stdout.is_empty(),
            "a successful push must land a ref on the bare remote"
        );
    }

    /// The bound's knob: unset, zero, empty, negative, or unparsable must
    /// fall back to the default rather than disable the timeout (a zero
    /// "no limit" reading would recreate the hang this issue closes).
    #[cfg(unix)]
    #[test]
    fn git_timeout_defaults_for_unset_zero_and_unparsable_values() {
        use super::git_test_support::EnvRestore;
        use std::time::Duration;

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(GIT_COMMAND_TIMEOUT_SECS, 30, "documented default");

        {
            let _unset = EnvRestore::unset("NEUROSTRATA_GIT_TIMEOUT_SECS");
            assert_eq!(git_timeout(), Duration::from_secs(30));
        }
        for raw in ["0", "", "  ", "not-a-number", "-5", "1.5"] {
            let _set = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", raw);
            assert_eq!(
                git_timeout(),
                Duration::from_secs(30),
                "NEUROSTRATA_GIT_TIMEOUT_SECS={raw:?} must fall back to the default"
            );
        }
    }

    /// A bound too large for the platform's Instant range must not panic:
    /// the deadline is tracked as elapsed time, so a fast git still succeeds
    /// under an absurd configured timeout instead of crashing the caller and
    /// orphaning the child.
    #[cfg(unix)]
    #[test]
    fn git_huge_timeout_does_not_panic() {
        use super::git_test_support::EnvRestore;

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _timeout = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", u64::MAX.to_string());
        let root = scratch("git-huge-timeout");
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(init.status.success(), "scratch repo init");
        run(&root, &["rev-parse", "--git-dir"])
            .expect("a huge configured timeout must not panic the bounded runner");
    }

    /// A valid value is honoured per call (no caching, no restart).
    #[cfg(unix)]
    #[test]
    fn git_timeout_reads_configured_value() {
        use super::git_test_support::EnvRestore;
        use std::time::Duration;

        let _env = crate::daemon::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (raw, secs) in [("1", 1), ("45", 45), (" 7 ", 7)] {
            let _set = EnvRestore::set("NEUROSTRATA_GIT_TIMEOUT_SECS", raw);
            assert_eq!(
                git_timeout(),
                Duration::from_secs(secs),
                "NEUROSTRATA_GIT_TIMEOUT_SECS={raw:?}"
            );
        }
    }
}
